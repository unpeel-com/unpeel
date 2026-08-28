use std::io;
use std::path::{Path, PathBuf};

use crate::git::{ChangedFile, DiffDocument, Repository};

#[derive(Clone, Debug)]
pub enum Screen {
    Files,
    Diff(DiffDocument),
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

pub struct App {
    pub repository: Repository,
    pub files: Vec<ChangedFile>,
    pub selected: usize,
    pub screen: Screen,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub horizontal_scroll: usize,
    pub reveal_selected: bool,
    pub viewport_rows: usize,
    pub max_scroll: usize,
    pub max_horizontal_scroll: usize,
    pub notice: Option<Notice>,
    /// Anchor and head diff-line indexes of the detail selection. The head
    /// may precede the anchor while dragging upward.
    pub selection: Option<(usize, usize)>,
}

impl App {
    pub fn new(repository: Repository) -> io::Result<Self> {
        let files = repository.changed_files()?;
        Ok(Self {
            repository,
            files,
            selected: 0,
            screen: Screen::Files,
            list_scroll: 0,
            detail_scroll: 0,
            horizontal_scroll: 0,
            reveal_selected: true,
            viewport_rows: 0,
            max_scroll: 0,
            max_horizontal_scroll: 0,
            notice: None,
            selection: None,
        })
    }

    #[must_use]
    pub fn is_detail(&self) -> bool {
        matches!(self.screen, Screen::Diff(_))
    }

    #[must_use]
    pub fn selected_file(&self) -> Option<&ChangedFile> {
        self.files.get(self.selected)
    }

    #[must_use]
    pub fn selected_absolute_path(&self) -> Option<PathBuf> {
        self.selected_file()
            .map(|file| self.repository.root().join(file.path()))
    }

    pub fn select(&mut self, index: usize) {
        if self.files.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = index.min(self.files.len() - 1);
        self.reveal_selected = true;
        self.notice = None;
    }

    pub fn move_selection(&mut self, delta: isize) {
        let next = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize)
        };
        self.select(next);
    }

    pub fn page_selection(&mut self, delta: isize) {
        let page = self.viewport_rows.saturating_sub(1).max(1);
        self.move_selection(delta.saturating_mul(page as isize));
    }

    pub fn open_selected(&mut self) -> io::Result<()> {
        let Some(file) = self.selected_file().cloned() else {
            return Ok(());
        };
        let document = self.repository.diff(&file)?;
        self.screen = Screen::Diff(document);
        self.detail_scroll = 0;
        self.horizontal_scroll = 0;
        self.max_scroll = 0;
        self.max_horizontal_scroll = 0;
        self.notice = None;
        self.selection = None;
        Ok(())
    }

    pub fn back(&mut self) {
        self.screen = Screen::Files;
        self.detail_scroll = 0;
        self.horizontal_scroll = 0;
        self.reveal_selected = true;
        self.notice = None;
        self.selection = None;
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        let selected_path = self.selected_file().map(|file| file.path().to_path_buf());
        let detail_path = match &self.screen {
            Screen::Files => None,
            Screen::Diff(document) => Some(document.file.path().to_path_buf()),
        };
        self.files = self.repository.changed_files()?;
        self.selected = selected_path
            .as_deref()
            .and_then(|path| self.files.iter().position(|file| file.path() == path))
            .unwrap_or(0)
            .min(self.files.len().saturating_sub(1));

        if let Some(path) = detail_path {
            if let Some(file) = self.files.iter().find(|file| file.path() == path) {
                self.screen = Screen::Diff(self.repository.diff(file)?);
            } else {
                self.back();
            }
        }
        self.reveal_selected = true;
        self.selection = None;
        self.notice = Some(Notice {
            text: format!("Refreshed · {} changed", self.files.len()),
            error: false,
        });
        Ok(())
    }

    /// Quietly reconcile with the working tree: refresh the file list and
    /// the open diff without posting a notice or moving the viewport.
    /// Returns whether anything visible changed.
    pub fn sync(&mut self) -> io::Result<bool> {
        let files = self.repository.changed_files()?;
        let mut changed = false;
        if files != self.files {
            let selected_path = self.selected_file().map(|file| file.path().to_path_buf());
            self.files = files;
            self.selected = selected_path
                .as_deref()
                .and_then(|path| self.files.iter().position(|file| file.path() == path))
                .unwrap_or(0)
                .min(self.files.len().saturating_sub(1));
            changed = true;
        }

        let detail_path = match &self.screen {
            Screen::Files => None,
            Screen::Diff(document) => Some(document.file.path().to_path_buf()),
        };
        if let Some(path) = detail_path {
            match self.files.iter().find(|file| file.path() == path).cloned() {
                Some(file) => {
                    let next = self.repository.diff(&file)?;
                    let unchanged = matches!(&self.screen, Screen::Diff(open) if *open == next);
                    if !unchanged {
                        self.detail_scroll =
                            self.detail_scroll.min(next.lines.len().saturating_sub(1));
                        // Line indexes are stale against the new document.
                        self.selection = None;
                        self.screen = Screen::Diff(next);
                        changed = true;
                    }
                }
                None => {
                    self.back();
                    changed = true;
                }
            }
        }
        Ok(changed)
    }

    pub fn fail(&mut self, error: impl std::fmt::Display) {
        self.notice = Some(Notice {
            text: error.to_string(),
            error: true,
        });
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: false,
        });
    }

    /// Start a new one-line selection at a diff line in the detail view.
    pub fn begin_selection(&mut self, line: usize) {
        let Some(line) = self.clamped_diff_line(line) else {
            return;
        };
        self.selection = Some((line, line));
        self.notice = None;
    }

    /// Move the selection head, keeping the existing anchor.
    pub fn extend_selection(&mut self, line: usize) {
        let Some(line) = self.clamped_diff_line(line) else {
            return;
        };
        let anchor = self.selection.map_or(line, |(anchor, _)| anchor);
        self.selection = Some((anchor, line));
        self.notice = None;
    }

    pub fn clear_selection(&mut self) -> bool {
        self.selection.take().is_some()
    }

    /// Ordered inclusive diff-line range of the current selection.
    #[must_use]
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let (anchor, head) = self.selection?;
        Some((anchor.min(head), anchor.max(head)))
    }

    #[must_use]
    pub fn selected_diff_lines(&self) -> Option<&[String]> {
        let Screen::Diff(document) = &self.screen else {
            return None;
        };
        let (start, end) = self.selection_range()?;
        document.lines.get(start..=end)
    }

    fn clamped_diff_line(&self, line: usize) -> Option<usize> {
        let Screen::Diff(document) = &self.screen else {
            return None;
        };
        if document.lines.is_empty() {
            return None;
        }
        Some(line.min(document.lines.len() - 1))
    }

    pub fn scroll_vertical(&mut self, delta: isize) {
        let current = if self.is_detail() {
            self.detail_scroll
        } else {
            self.list_scroll
        };
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize)
        }
        .min(self.max_scroll);
        if self.is_detail() {
            self.detail_scroll = next;
        } else {
            self.list_scroll = next;
            self.reveal_selected = false;
        }
        self.notice = None;
    }

    pub fn scroll_horizontal(&mut self, delta: isize) {
        self.horizontal_scroll = if delta.is_negative() {
            self.horizontal_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.horizontal_scroll.saturating_add(delta as usize)
        }
        .min(self.max_horizontal_scroll);
        self.notice = None;
    }

    pub fn scroll_to_start(&mut self) {
        if self.is_detail() {
            self.detail_scroll = 0;
        } else {
            self.select(0);
        }
    }

    pub fn scroll_to_end(&mut self) {
        if self.is_detail() {
            self.detail_scroll = self.max_scroll;
        } else {
            self.select(self.files.len().saturating_sub(1));
        }
    }

    pub fn apply_render_metrics(
        &mut self,
        scroll: usize,
        max_scroll: usize,
        viewport_rows: usize,
        max_horizontal_scroll: usize,
    ) {
        if self.is_detail() {
            self.detail_scroll = scroll;
        } else {
            self.list_scroll = scroll;
            self.reveal_selected = false;
        }
        self.max_scroll = max_scroll;
        self.viewport_rows = viewport_rows;
        self.max_horizontal_scroll = max_horizontal_scroll;
        self.horizontal_scroll = self.horizontal_scroll.min(max_horizontal_scroll);
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.repository.root()
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn diff_app() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        for arguments in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Unpeel Tests"],
            vec!["config", "user.email", "tests@unpeel.local"],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .args(&arguments)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        std::fs::write(directory.path().join("new.txt"), "one\ntwo\nthree\n").unwrap();
        let repository = Repository::discover(directory.path()).unwrap();
        let mut app = App::new(repository).unwrap();
        app.open_selected().unwrap();
        (directory, app)
    }

    #[test]
    fn selections_order_their_range_and_clamp_to_the_document() {
        let (_directory, mut app) = diff_app();
        let last = match &app.screen {
            Screen::Diff(document) => document.lines.len() - 1,
            Screen::Files => unreachable!(),
        };

        app.begin_selection(3);
        app.extend_selection(1);
        assert_eq!(app.selection_range(), Some((1, 3)));
        assert_eq!(app.selected_diff_lines().unwrap().len(), 3);

        app.extend_selection(usize::MAX);
        assert_eq!(app.selection_range(), Some((3, last)));

        assert!(app.clear_selection());
        assert!(!app.clear_selection());
        assert_eq!(app.selected_diff_lines(), None);
    }

    #[test]
    fn leaving_the_detail_view_drops_the_selection() {
        let (_directory, mut app) = diff_app();
        app.begin_selection(0);
        assert!(app.selection.is_some());
        app.back();
        assert_eq!(app.selection, None);
    }

    #[test]
    fn sync_follows_the_working_tree_without_a_notice() {
        let (directory, mut app) = diff_app();
        assert!(!app.sync().unwrap());
        app.begin_selection(1);
        app.notice = None;

        std::fs::write(directory.path().join("new.txt"), "one\nCHANGED\nthree\n").unwrap();
        assert!(app.sync().unwrap());
        assert_eq!(app.selection, None);
        assert_eq!(app.notice.as_ref().map(|notice| notice.error), None);
        match &app.screen {
            Screen::Diff(document) => {
                assert!(document.lines.iter().any(|line| line == "+CHANGED"));
            }
            Screen::Files => panic!("diff should stay open"),
        }

        std::fs::remove_file(directory.path().join("new.txt")).unwrap();
        assert!(app.sync().unwrap());
        assert!(!app.is_detail());
        assert!(app.files.is_empty());
    }
}
