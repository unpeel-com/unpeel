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
        Ok(())
    }

    pub fn back(&mut self) {
        self.screen = Screen::Files;
        self.detail_scroll = 0;
        self.horizontal_scroll = 0;
        self.reveal_selected = true;
        self.notice = None;
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
        self.notice = Some(Notice {
            text: format!("Refreshed · {} changed", self.files.len()),
            error: false,
        });
        Ok(())
    }

    pub fn fail(&mut self, error: impl std::fmt::Display) {
        self.notice = Some(Notice {
            text: error.to_string(),
            error: true,
        });
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
