use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use unpeel_app_kit::TerminalPointerState;

use crate::git::{ChangedFile, Commit, DiffDocument, RemoteAction, RemoteState, Repository};

#[derive(Clone, Debug)]
pub enum Screen {
    Files,
    History,
    CommitFiles,
    Diff(DiffDocument),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Changes,
    History,
}

#[derive(Clone, Copy, Debug, Default)]
struct ListPosition {
    selected: usize,
    scroll: usize,
}

const HISTORY_PAGE_SIZE: usize = 100;

#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

pub struct App {
    pub repository: Repository,
    pub files: Vec<ChangedFile>,
    pub branch: Option<String>,
    pub remote: RemoteState,
    pub remote_busy: Option<RemoteAction>,
    remote_job: Option<Receiver<io::Result<()>>>,
    pub tab: Tab,
    pub history: Vec<Commit>,
    pub has_more_history: bool,
    pub commit: Option<Commit>,
    pub commit_files: Vec<ChangedFile>,
    history_limit: usize,
    changes_position: ListPosition,
    history_position: ListPosition,
    commit_position: ListPosition,
    pub selected: usize,
    pub screen: Screen,
    /// Parsed only when the patch changes, shared by every renderer.
    pub syntax: Option<crate::highlight::DocumentSyntax>,
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
    pub pointer: TerminalPointerState,
}

impl App {
    pub fn new(repository: Repository) -> io::Result<Self> {
        let files = repository.changed_files()?;
        let branch = repository.branch();
        let remote = repository.remote_state()?;
        Ok(Self {
            branch,
            remote,
            remote_busy: None,
            remote_job: None,
            tab: Tab::Changes,
            history: Vec::new(),
            has_more_history: false,
            commit: None,
            commit_files: Vec::new(),
            history_limit: HISTORY_PAGE_SIZE,
            changes_position: ListPosition::default(),
            history_position: ListPosition::default(),
            commit_position: ListPosition::default(),
            repository,
            files,
            selected: 0,
            screen: Screen::Files,
            syntax: None,
            list_scroll: 0,
            detail_scroll: 0,
            horizontal_scroll: 0,
            reveal_selected: true,
            viewport_rows: 0,
            max_scroll: 0,
            max_horizontal_scroll: 0,
            notice: None,
            selection: None,
            pointer: TerminalPointerState::new(),
        })
    }

    pub fn start_remote_action(&mut self, action: RemoteAction) -> io::Result<()> {
        if self.remote_busy.is_some() {
            return Err(io::Error::other("A Git operation is already running"));
        }
        if !self.remote.allows(action) {
            return Err(io::Error::other(
                "This Git operation is unavailable for the current branch",
            ));
        }
        let repository = self.repository.clone();
        let expected = self.remote.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("git-remote".into())
            .spawn(move || {
                let _ = sender.send(repository.remote_action(action, &expected));
            })?;
        self.remote_job = Some(receiver);
        self.remote_busy = Some(action);
        self.notice = None;
        Ok(())
    }

    pub fn poll_remote_action(&mut self) -> bool {
        let Some(receiver) = &self.remote_job else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                Err(io::Error::other("Git operation stopped unexpectedly"))
            }
        };
        let action = self.remote_busy.take().expect("running operation");
        self.remote_job = None;
        let sync = self.sync();
        match result.and(sync.map(|_| ())) {
            Ok(()) => self.notify(format!("{} complete", action.name())),
            Err(error) => self.fail(error),
        }
        true
    }

    /// Follow a neighboring/main agent into another checkout (including a
    /// worktree), or back to the main checkout. Explicit-path launches leave
    /// this unused and remain pinned to their requested repository.
    pub fn follow_path(&mut self, path: impl AsRef<Path>) -> io::Result<bool> {
        if self.remote_busy.is_some() {
            return Ok(false);
        }
        let repository = Repository::discover(path)?;
        if repository.root() == self.repository.root() {
            return Ok(false);
        }
        let mut next = Self::new(repository)?;
        next.switch_tab(self.tab)?;
        *self = next;
        Ok(true)
    }

    #[must_use]
    pub fn is_detail(&self) -> bool {
        matches!(self.screen, Screen::Diff(_))
    }

    pub fn can_go_back(&self) -> bool {
        matches!(self.screen, Screen::Diff(_) | Screen::CommitFiles)
    }

    pub fn active_files(&self) -> &[ChangedFile] {
        if self.tab == Tab::History {
            &self.commit_files
        } else {
            &self.files
        }
    }

    pub fn list_len(&self) -> usize {
        if matches!(self.screen, Screen::History) {
            self.history.len()
        } else {
            self.active_files().len()
        }
    }

    #[must_use]
    pub fn selected_file(&self) -> Option<&ChangedFile> {
        match &self.screen {
            Screen::History => None,
            Screen::Diff(document) => Some(&document.file),
            _ => self.active_files().get(self.selected),
        }
    }

    #[must_use]
    pub fn selected_absolute_path(&self) -> Option<PathBuf> {
        // Historical paths and line numbers need not exist in this checkout.
        if self.tab == Tab::History {
            return None;
        }
        self.selected_file()
            .map(|file| self.repository.root().join(file.path()))
    }

    fn remember_position(&mut self) {
        let position = ListPosition {
            selected: self.selected,
            scroll: self.list_scroll,
        };
        match self.screen {
            Screen::Files => self.changes_position = position,
            Screen::History => self.history_position = position,
            Screen::CommitFiles => self.commit_position = position,
            Screen::Diff(_) => {}
        }
    }

    fn restore_position(&mut self, position: ListPosition) {
        self.selected = position.selected.min(self.list_len().saturating_sub(1));
        self.list_scroll = position.scroll;
        self.reset_detail();
        self.reveal_selected = true;
    }

    fn reset_detail(&mut self) {
        self.detail_scroll = 0;
        self.horizontal_scroll = 0;
        self.max_scroll = 0;
        self.max_horizontal_scroll = 0;
        self.selection = None;
        self.notice = None;
    }

    pub fn switch_tab(&mut self, tab: Tab) -> io::Result<()> {
        if tab == self.tab {
            return Ok(());
        }
        if tab == Tab::History {
            self.reload_history()?;
        }
        self.remember_position();
        self.tab = tab;
        self.commit = None;
        self.commit_files.clear();
        self.screen = if tab == Tab::History {
            Screen::History
        } else {
            Screen::Files
        };
        let position = if tab == Tab::History {
            self.history_position
        } else {
            self.changes_position
        };
        self.restore_position(position);
        Ok(())
    }

    fn reload_history(&mut self) -> io::Result<bool> {
        let mut history = self.repository.history(self.history_limit + 1)?;
        let has_more = history.len() > self.history_limit;
        history.truncate(self.history_limit);
        let changed = history != self.history || has_more != self.has_more_history;
        let position = if matches!(self.screen, Screen::History) {
            self.selected
        } else {
            self.history_position.selected
        };
        let selected_id = self.history.get(position).map(|commit| &commit.id);
        let selected = selected_id
            .and_then(|id| history.iter().position(|commit| &commit.id == id))
            .unwrap_or(0);
        self.history = history;
        self.has_more_history = has_more;
        self.history_position.selected = selected;
        if matches!(self.screen, Screen::History) {
            self.selected = selected;
        }
        Ok(changed)
    }

    pub fn load_more_history(&mut self) -> io::Result<()> {
        let previous_limit = self.history_limit;
        self.history_limit = self.history_limit.saturating_add(HISTORY_PAGE_SIZE);
        if let Err(error) = self.reload_history() {
            self.history_limit = previous_limit;
            return Err(error);
        }
        Ok(())
    }

    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.list_len().saturating_sub(1));
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
        if matches!(self.screen, Screen::History) {
            let Some(commit) = self.history.get(self.selected).cloned() else {
                return Ok(());
            };
            let files = self.repository.commit_files(&commit)?;
            self.remember_position();
            self.commit = Some(commit);
            self.commit_files = files;
            self.screen = Screen::CommitFiles;
            self.restore_position(ListPosition::default());
            return Ok(());
        }
        let Some(file) = self.selected_file().cloned() else {
            return Ok(());
        };
        let document = match &self.commit {
            Some(commit) => self.repository.commit_diff(commit, &file)?,
            None => self.repository.diff(&file)?,
        };
        self.remember_position();
        self.syntax = crate::highlight::document_runs(&document);
        self.screen = Screen::Diff(document);
        self.reset_detail();
        Ok(())
    }

    pub fn back(&mut self) {
        if self.is_detail() && self.tab == Tab::History {
            self.screen = Screen::CommitFiles;
            self.restore_position(self.commit_position);
        } else if self.tab == Tab::History {
            self.commit = None;
            self.commit_files.clear();
            self.screen = Screen::History;
            self.restore_position(self.history_position);
        } else {
            self.screen = Screen::Files;
            self.restore_position(self.changes_position);
        }
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        self.sync()?;
        self.reveal_selected = true;
        self.selection = None;
        self.notify(if self.tab == Tab::History {
            format!("Refreshed · {} commits", self.history.len())
        } else {
            format!("Refreshed · {} changed", self.files.len())
        });
        Ok(())
    }

    /// Quietly reconcile with the working tree: refresh the file list and
    /// the open diff without posting a notice or moving the viewport.
    /// Returns whether anything visible changed.
    pub fn sync(&mut self) -> io::Result<bool> {
        let files = self.repository.changed_files()?;
        let branch = self.repository.branch();
        let remote = self.repository.remote_state()?;
        let mut changed = branch != self.branch || remote != self.remote;
        self.remote = remote;
        self.branch = branch;
        if files != self.files {
            let position = if self.tab == Tab::Changes {
                self.selected
            } else {
                self.changes_position.selected
            };
            let selected_path = self.files.get(position).map(|file| file.path());
            let selected = selected_path
                .and_then(|path| files.iter().position(|file| file.path() == path))
                .unwrap_or(0);
            self.files = files;
            self.changes_position.selected = selected;
            if self.tab == Tab::Changes {
                self.selected = selected;
            }
            changed = true;
        }
        if self.tab == Tab::History {
            return Ok(self.reload_history()? || changed);
        }

        let detail_path = match &self.screen {
            Screen::Files | Screen::History | Screen::CommitFiles => None,
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
                        self.syntax = crate::highlight::document_runs(&next);
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
            text: error
                .to_string()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(500)
                .collect(),
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

    pub fn scroll_vertical(&mut self, delta: isize) -> bool {
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
        let notice_changed = self.notice.take().is_some();
        next != current || notice_changed
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
            self.select(self.list_len().saturating_sub(1));
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
    fn remote_worker_blocks_duplicate_actions_and_reports_completion() {
        let (directory, mut app) = diff_app();
        let remote = tempfile::tempdir().unwrap();
        for (root, arguments) in [
            (remote.path(), vec!["init", "--bare"]),
            (
                directory.path(),
                vec!["remote", "add", "origin", remote.path().to_str().unwrap()],
            ),
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(arguments)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        app.sync().unwrap();
        app.start_remote_action(RemoteAction::Fetch).unwrap();
        assert_eq!(app.remote_busy, Some(RemoteAction::Fetch));
        assert!(app.start_remote_action(RemoteAction::Fetch).is_err());
        assert!(!app.follow_path(remote.path()).unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !app.poll_remote_action() {
            assert!(
                std::time::Instant::now() < deadline,
                "Git worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.remote_busy.is_none());
        let notice = app.notice.as_ref().unwrap();
        assert!(!notice.error, "{}", notice.text);
        assert_eq!(notice.text, "Fetch complete");
    }

    #[test]
    fn selections_order_their_range_and_clamp_to_the_document() {
        let (_directory, mut app) = diff_app();
        let last = match &app.screen {
            Screen::Diff(document) => document.lines.len() - 1,
            _ => unreachable!(),
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
            _ => panic!("diff should stay open"),
        }

        std::fs::remove_file(directory.path().join("new.txt")).unwrap();
        assert!(app.sync().unwrap());
        assert!(!app.is_detail());
        assert!(app.files.is_empty());
    }

    fn commit(root: &Path, subject: &str) {
        for arguments in [
            vec!["add", "."],
            vec!["commit", "--allow-empty", "-m", subject],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args(arguments)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
    }

    #[test]
    fn history_drills_into_commits_and_back_without_losing_changes_selection() {
        let (directory, mut app) = diff_app();
        app.back();
        commit(directory.path(), "First");
        std::fs::write(directory.path().join("new.txt"), "second\n").unwrap();
        commit(directory.path(), "Second");
        std::fs::write(directory.path().join("a.txt"), "dirty\n").unwrap();
        std::fs::write(directory.path().join("z.txt"), "dirty\n").unwrap();
        app.sync().unwrap();
        app.select(1);
        app.switch_tab(Tab::History).unwrap();
        assert_eq!(app.history.len(), 2);
        app.select(1);
        app.open_selected().unwrap();
        assert!(matches!(app.screen, Screen::CommitFiles));
        assert_eq!(app.commit.as_ref().unwrap().subject, "First");
        assert!(app.selected_absolute_path().is_none());
        app.open_selected().unwrap();
        let Screen::Diff(document) = &app.screen else {
            panic!("expected patch")
        };
        assert!(document.lines.iter().any(|line| line == "+one"));
        let original = document.clone();
        std::fs::write(directory.path().join("new.txt"), "third\n").unwrap();
        commit(directory.path(), "Third");
        app.sync().unwrap();
        let Screen::Diff(document) = &app.screen else {
            panic!("expected pinned patch")
        };
        assert_eq!(document, &original);
        app.back();
        assert!(matches!(app.screen, Screen::CommitFiles));
        app.back();
        assert!(matches!(app.screen, Screen::History));
        assert_eq!(app.history[app.selected].subject, "First");
        app.switch_tab(Tab::Changes).unwrap();
        assert!(matches!(app.screen, Screen::Files));
        assert!(app.files.is_empty());
        assert!(app.commit.is_none());
    }

    #[test]
    fn history_pagination_preserves_commit_selection() {
        let (directory, mut app) = diff_app();
        app.back();
        for subject in ["First", "Second", "Third"] {
            commit(directory.path(), subject);
        }
        app.history_limit = 2;
        app.switch_tab(Tab::History).unwrap();
        assert_eq!(app.history.len(), 2);
        assert!(app.has_more_history);
        app.select(1);
        let selected = app.history[1].id.clone();
        app.load_more_history().unwrap();
        assert_eq!(app.history.len(), 3);
        assert!(!app.has_more_history);
        assert_eq!(app.history[app.selected].id, selected);
        assert!(app.notice.is_none());
    }
}
