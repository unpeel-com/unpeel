use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::{
    ColorScheme, DragSurface, InputField, InputFieldAction, InputFieldTheme, KitTheme, ListKeymap,
    ListNavigationAction, ListNavigationOutcome, ListPageBehavior, RowBoundaryBehavior,
    RowKeyDecision, RowNavigationState, RowPrimaryRole, SELECTABLE_LEFT_PADDING, SelectableRow,
    TerminalPointerPhase, TerminalPointerState, VerticalScrollbar,
};

/// One item in the current directory shown by [`Explorer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerEntry {
    path: PathBuf,
    name: String,
    directory: bool,
    symlink: bool,
    parent: bool,
    detail: Option<String>,
}

/// App-owned secondary text for a file row, for example a document's first
/// heading. Called once per file on every refresh, so keep it cheap and
/// bounded (read a prefix, never the whole file).
#[derive(Clone)]
pub struct ExplorerEntryDetail(Arc<EntryDetailFn>);

type EntryDetailFn = dyn Fn(&Path) -> Option<String> + Send + Sync;

impl ExplorerEntryDetail {
    #[must_use]
    pub fn new(provider: impl Fn(&Path) -> Option<String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(provider))
    }

    fn resolve(&self, path: &Path) -> Option<String> {
        let detail = (self.0)(path)?;
        let detail = sanitize_label(detail.trim());
        (!detail.is_empty()).then_some(detail)
    }
}

impl fmt::Debug for ExplorerEntryDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExplorerEntryDetail(..)")
    }
}

impl ExplorerEntry {
    /// Muted secondary text shown after the name, when a detail provider is
    /// installed.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Absolute Host-local path represented by this row.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sanitized filename without the directory suffix.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this entry can be entered as a directory.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.directory
    }

    /// Whether this path itself is a symbolic link.
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        self.symlink
    }

    /// Whether this is the synthetic `../` entry.
    #[must_use]
    pub const fn is_parent(&self) -> bool {
        self.parent
    }

    /// Display label, including `/` for navigable directories.
    #[must_use]
    pub fn display_name(&self) -> String {
        if self.parent {
            "../".to_owned()
        } else if self.directory {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// Backend-neutral actions understood by [`Explorer::handle`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ExplorerInput {
    Up,
    Down,
    First,
    Last,
    PageUp,
    PageDown,
    Parent,
    Open,
    ToggleHidden,
    Refresh,
    FocusFilter,
    BlurFilter,
    ClearFilter,
    FilterCharacter(char),
    FilterBackspace,
    FilterDelete,
    FilterSelectAll,
    FilterLeft {
        extend: bool,
        word: bool,
    },
    FilterRight {
        extend: bool,
        word: bool,
    },
    FilterHome {
        extend: bool,
    },
    FilterEnd {
        extend: bool,
    },
    #[default]
    None,
}

/// Observable result of handling an [`ExplorerInput`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ExplorerEvent {
    #[default]
    None,
    SelectionChanged,
    DirectoryChanged(PathBuf),
    FileActivated(PathBuf),
    Refreshed,
    FilterChanged,
    FilterFocusChanged,
}

/// Borderless visual styling for [`Explorer`].
///
/// There is deliberately no `Block` or border field. Apps can place the
/// component inside their own layout without inheriting IDE-like chrome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplorerTheme {
    pub style: Style,
    pub filter: Style,
    pub filter_focused: Style,
    pub filter_placeholder: Style,
    pub path: Style,
    pub item: Style,
    pub directory: Style,
    pub symlink: Style,
    pub parent: Style,
    pub selected: Style,
    /// Pointer hover on an unselected row; distinct from `selected`.
    pub hovered: Style,
    pub empty: Style,
    pub scrollbar_track: Style,
    pub scrollbar_thumb: Style,
    /// Optional marker whose width is reserved on every row to avoid jitter.
    pub selected_symbol: Option<String>,
    /// Blank columns before the header and item labels.
    pub left_padding: u16,
    /// Rows kept visible above and below the selection when possible.
    pub scroll_padding: usize,
}

impl Default for ExplorerTheme {
    fn default() -> Self {
        Self::dark()
    }
}

impl ExplorerTheme {
    /// Defaults tuned for a dark terminal while leaving its base background
    /// untouched.
    #[must_use]
    pub const fn dark() -> Self {
        Self::for_palette(KitTheme::dark())
    }

    /// Defaults tuned for a light terminal while leaving its base background
    /// untouched.
    #[must_use]
    pub const fn light() -> Self {
        Self::for_palette(KitTheme::light())
    }

    #[must_use]
    pub const fn for_color_scheme(scheme: ColorScheme) -> Self {
        Self::for_palette(KitTheme::for_scheme(scheme))
    }

    /// Builds Explorer styling from a complete kit palette, preserving a
    /// Host-provided project/workspace accent from [`KitTheme::detected`].
    #[must_use]
    pub const fn for_theme(theme: KitTheme) -> Self {
        Self::for_palette(theme)
    }

    #[must_use]
    pub fn detected() -> Self {
        Self::for_theme(KitTheme::detected())
    }

    const fn for_palette(palette: KitTheme) -> Self {
        Self {
            style: Style::new(),
            filter: Style::new().fg(palette.muted),
            filter_focused: Style::new().fg(palette.text).add_modifier(Modifier::BOLD),
            filter_placeholder: Style::new().fg(palette.subtle),
            path: Style::new().add_modifier(Modifier::BOLD),
            item: Style::new().fg(palette.text),
            directory: Style::new().fg(palette.accent),
            symlink: Style::new().fg(Color::Cyan),
            parent: Style::new().fg(palette.accent),
            selected: palette.selected_row,
            hovered: palette.hovered_row,
            empty: Style::new().fg(palette.subtle),
            scrollbar_track: palette.scrollbar_track,
            scrollbar_thumb: palette.scrollbar_thumb,
            selected_symbol: None,
            left_padding: SELECTABLE_LEFT_PADDING,
            scroll_padding: 1,
        }
    }
}

/// A flat, borderless filesystem explorer for Ratatui Apps.
///
/// The component owns the current directory, selection, hidden-file policy,
/// paging, viewport scroll, rendering, and terminal-cell drag registration.
/// Input conversion and what to do when a file is activated remain App-owned.
#[derive(Debug)]
pub struct Explorer {
    cwd: PathBuf,
    navigation_root: Option<PathBuf>,
    all_entries: Vec<ExplorerEntry>,
    entries: Vec<ExplorerEntry>,
    navigation: RowNavigationState,
    show_hidden: bool,
    file_extensions: Option<Vec<String>>,
    prune_unmatched_directories: bool,
    show_filter: bool,
    filter: InputField,
    show_path: bool,
    theme: ExplorerTheme,
    area: Rect,
    list_area: Rect,
    semantic_ids: HashMap<PathBuf, String>,
    next_semantic_id: u64,
    detail_provider: Option<ExplorerEntryDetail>,
}

impl Explorer {
    /// Opens a directory, or opens a file's parent and selects that file.
    /// Parent navigation is unbounded; Apps that represent a project should
    /// use [`Self::scoped`] instead.
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_path(path.as_ref(), false)
    }

    /// Opens an Explorer whose initial directory is a hard navigation root.
    ///
    /// The root has no synthetic `../` entry, parent actions become no-ops at
    /// that level, and canonicalized directory targets outside it are denied.
    /// Passing a file scopes the Explorer to that file's parent directory.
    pub fn scoped(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_path(path.as_ref(), true)
    }

    fn from_path(path: &Path, scoped: bool) -> io::Result<Self> {
        let requested = fs::canonicalize(path)?;
        let metadata = fs::metadata(&requested)?;
        let (cwd, preferred) = if metadata.is_dir() {
            (requested, None)
        } else {
            let parent = requested.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "file has no parent directory")
            })?;
            (parent.to_path_buf(), Some(requested))
        };
        let navigation_root = scoped.then(|| cwd.clone());
        let all_entries = read_entries(&cwd, false, navigation_root.is_none(), None, false)?;
        let entries = all_entries.clone();
        let selected = preferred
            .as_deref()
            .and_then(|path| entries.iter().position(|entry| entry.path == path))
            .unwrap_or(0)
            .min(entries.len().saturating_sub(1));
        let theme = ExplorerTheme::default();
        let filter = InputField::new("Filter files")
            .with_prompt("/ ")
            .with_theme(input_theme(&theme));
        let mut navigation = RowNavigationState::new((!entries.is_empty()).then_some(selected));
        navigation.set_boundary_behavior(RowBoundaryBehavior::Wrap);
        navigation.set_navigation(theme.scroll_padding, 0, ListPageBehavior::Selection);
        navigation.prepare(Rect::new(0, 0, 0, 12), entries.len());
        Ok(Self {
            cwd,
            navigation_root,
            all_entries,
            entries,
            navigation,
            show_hidden: false,
            file_extensions: None,
            prune_unmatched_directories: false,
            show_filter: true,
            filter,
            show_path: true,
            theme,
            detail_provider: None,
            area: Rect::default(),
            list_area: Rect::default(),
            semantic_ids: HashMap::new(),
            next_semantic_id: 1,
        })
    }

    /// Applies a borderless visual theme.
    #[must_use]
    pub fn with_theme(mut self, theme: ExplorerTheme) -> Self {
        self.filter.set_theme(input_theme(&theme));
        self.navigation
            .set_navigation(theme.scroll_padding, 0, ListPageBehavior::Selection);
        self.theme = theme;
        self
    }

    /// Replaces the visual theme without changing navigation state.
    pub fn set_theme(&mut self, theme: ExplorerTheme) {
        self.filter.set_theme(input_theme(&theme));
        self.navigation
            .set_navigation(theme.scroll_padding, 0, ListPageBehavior::Selection);
        self.theme = theme;
    }

    #[must_use]
    pub const fn theme(&self) -> &ExplorerTheme {
        &self.theme
    }

    /// Shows or hides the always-borderless filter row.
    pub const fn set_show_filter(&mut self, show: bool) {
        self.show_filter = show;
    }

    #[must_use]
    pub const fn show_filter(&self) -> bool {
        self.show_filter
    }

    /// Current case-insensitive filename filter.
    #[must_use]
    pub fn filter(&self) -> &str {
        self.filter.text()
    }

    #[must_use]
    pub const fn filter_focused(&self) -> bool {
        self.filter.focused()
    }

    /// Replaces the hint shown by an empty filter row.
    pub fn set_filter_placeholder(&mut self, placeholder: impl Into<String>) -> bool {
        self.filter.set_placeholder(placeholder)
    }

    /// Replaces the filter, dropping terminal control characters.
    pub fn set_filter(&mut self, filter: impl Into<String>) -> bool {
        let next = filter
            .into()
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        if next == self.filter.text() {
            return false;
        }
        let preferred = self.selected().map(|entry| entry.path.clone());
        self.filter.set_text(next);
        self.rebuild_filtered(preferred.as_deref());
        true
    }

    /// Clears the filename filter while preserving the selected path.
    pub fn clear_filter(&mut self) -> bool {
        self.set_filter(String::new())
    }

    /// Focuses or blurs the filter row.
    pub fn set_filter_focused(&mut self, focused: bool) -> bool {
        self.filter.set_focused(focused)
    }

    /// Shows or hides the draggable current-directory header.
    pub const fn set_show_path(&mut self, show: bool) {
        self.show_path = show;
    }

    #[must_use]
    pub const fn show_path(&self) -> bool {
        self.show_path
    }

    /// Absolute current directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Hard navigation boundary, when this Explorer is scoped.
    #[must_use]
    pub fn navigation_root(&self) -> Option<&Path> {
        self.navigation_root.as_deref()
    }

    /// Entries in display order after filtering, including `../` when a
    /// parent exists. The parent remains available for navigation even when
    /// its label does not match the filter.
    #[must_use]
    pub fn entries(&self) -> &[ExplorerEntry] {
        &self.entries
    }

    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.navigation.selected().unwrap_or(0)
    }

    #[must_use]
    pub fn selected(&self) -> Option<&ExplorerEntry> {
        self.entries.get(self.selected_index())
    }

    /// Projects the current App-owned directory as the closed semantic Tree.
    ///
    /// Entry ids are process-local opaque keys. Absolute filesystem paths stay
    /// exclusively in this Explorer for TUI drag/open behavior.
    #[must_use]
    pub fn semantic_tree(&mut self, label: impl Into<String>) -> crate::Tree {
        let entries = self
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.path.clone(),
                    entry.name.clone(),
                    entry.directory,
                    entry.symlink,
                    entry.parent,
                    entry.detail.clone(),
                )
            })
            .collect::<Vec<_>>();
        let selected_path = self.selected().map(|entry| entry.path.clone());
        let mut selected_id = None;
        let mut items = Vec::with_capacity(entries.len());
        for (path, name, directory, symlink, parent, detail) in entries {
            let id = self.semantic_id_for_path(&path);
            if selected_path.as_ref() == Some(&path) {
                selected_id = Some(id.clone());
            }
            let item = if parent {
                crate::TreeItem::parent(id)
            } else if directory {
                crate::TreeItem::directory(id, name)
                    .child_state(crate::TreeChildState::Unloaded)
                    .symlink(symlink)
                    .hidden(
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with('.')),
                    )
            } else {
                crate::TreeItem::file(id, name)
                    .symlink(symlink)
                    .hidden(
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with('.')),
                    )
                    .detail(detail)
            };
            items.push(item);
        }
        let root = self.navigation_root.as_deref().unwrap_or(&self.cwd);
        let mut location = crate::display_path_from_root(&self.cwd, root);
        if location == "." {
            // The root itself reads as its folder name, not a lone dot.
            location = root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or(location);
        }
        let mut tree =
            crate::Tree::new(label, location, items).empty_message(if self.filter().is_empty() {
                "empty folder"
            } else {
                "no matches"
            });
        if self.show_filter {
            tree = tree.filter(
                crate::TreeFilter::new("explorer-filter", "Filter", self.filter(), "tree-filter")
                    .placeholder(self.filter.placeholder()),
            );
        }
        if let Some(selected_id) = selected_id {
            tree = tree.selected_id(selected_id);
        }
        tree
    }

    /// Selects an entry named by the opaque id emitted by [`Self::semantic_tree`].
    ///
    /// Apps use this for closed Tree extensions such as a semantic context
    /// menu. The opaque id is resolved inside Explorer, so Host-local paths
    /// never have to appear in the semantic protocol.
    #[must_use]
    #[cfg(feature = "ui-bridge")]
    pub fn path_for_semantic_item(&self, id: &str) -> Option<&Path> {
        self.semantic_index(id)
            .and_then(|index| self.entries.get(index))
            .map(ExplorerEntry::path)
    }

    /// Selects an entry named by the opaque id emitted by [`Self::semantic_tree`].
    ///
    /// The mapping stays App-local: the component tree carries only the
    /// opaque id while terminal drag and pointer plumbing may resolve its
    /// filesystem path after the shared Tree interpreter produces geometry.
    #[cfg(feature = "ui-bridge")]
    pub fn select_semantic_item(&mut self, id: &str) -> Result<ExplorerEvent, String> {
        let index = self
            .semantic_index(id)
            .ok_or_else(|| format!("Tree item {id:?} is not present"))?;
        self.set_selected_index(index);
        Ok(ExplorerEvent::SelectionChanged)
    }

    /// Applies one authenticated semantic Tree action to the same state used
    /// by Ratatui. The caller publishes/acknowledges the resulting revision.
    #[cfg(feature = "ui-bridge")]
    pub fn handle_ui_event(
        &mut self,
        revision: u64,
        tree_node_id: &str,
        event: &crate::UiEvent,
    ) -> Result<Option<ExplorerEvent>, String> {
        if event.base_revision != revision {
            return Err(format!(
                "Tree action base revision {} does not match {revision}",
                event.base_revision
            ));
        }
        let action = event.action.action.as_str();
        if action == crate::TreeActions::SELECT || action == crate::TreeActions::OPEN {
            if event.action.node_id.as_str() != tree_node_id {
                return Ok(None);
            }
            let crate::UiEventValue::Text(item_id) = &event.action.value else {
                return Err("Tree select/open requires an opaque text item id".to_owned());
            };
            self.select_semantic_item(item_id)?;
            if action == crate::TreeActions::SELECT {
                if event.action.kind != crate::UiEventKind::Select {
                    return Err("Tree selection requires a select event".to_owned());
                }
                return Ok(Some(ExplorerEvent::SelectionChanged));
            }
            if event.action.kind != crate::UiEventKind::Activate {
                return Err("Tree open requires an activate event".to_owned());
            }
            return self
                .handle(ExplorerInput::Open)
                .map(Some)
                .map_err(|error| error.to_string());
        }
        if action == crate::TreeActions::PARENT {
            if event.action.node_id.as_str() != tree_node_id
                || !matches!(
                    event.action.kind,
                    crate::UiEventKind::Cancel | crate::UiEventKind::Activate
                )
            {
                return Err("Tree parent action has the wrong target or kind".to_owned());
            }
            return self
                .handle(ExplorerInput::Parent)
                .map(Some)
                .map_err(|error| error.to_string());
        }
        if action == "tree-filter" {
            if event.action.node_id.as_str() != "explorer-filter"
                || event.action.kind != crate::UiEventKind::Change
            {
                return Err("Tree filter action has the wrong target or kind".to_owned());
            }
            let crate::UiEventValue::Text(value) = &event.action.value else {
                return Err("Tree filter requires a text value".to_owned());
            };
            return Ok(self
                .set_filter(value.clone())
                .then_some(ExplorerEvent::FilterChanged));
        }
        Ok(None)
    }

    #[must_use]
    pub const fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Optional case-insensitive file extensions admitted by this Explorer.
    /// Directories are always retained for navigation.
    #[must_use]
    pub fn file_extensions(&self) -> Option<&[String]> {
        self.file_extensions.as_deref()
    }

    /// Restricts visible files to the supplied extensions while preserving
    /// directories. Leading dots are optional (`"md"` and `".md"` match).
    pub fn set_file_extensions<I, S>(&mut self, extensions: I) -> io::Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let extensions = normalize_extensions(extensions);
        if self.file_extensions.as_ref() == Some(&extensions) {
            return Ok(());
        }
        self.replace_file_extensions(Some(extensions))
    }

    /// Builder form of [`Self::set_file_extensions`].
    pub fn with_file_extensions<I, S>(mut self, extensions: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.set_file_extensions(extensions)?;
        Ok(self)
    }

    /// Removes a configured extension restriction.
    pub fn clear_file_extensions(&mut self) -> io::Result<()> {
        if self.file_extensions.is_none() {
            return Ok(());
        }
        self.replace_file_extensions(None)
    }

    /// Installs App-owned secondary text for file rows and refreshes.
    pub fn set_entry_detail(&mut self, provider: Option<ExplorerEntryDetail>) -> io::Result<()> {
        self.detail_provider = provider;
        self.refresh()
    }

    /// Builder form of [`Self::set_entry_detail`].
    pub fn with_entry_detail(mut self, provider: ExplorerEntryDetail) -> io::Result<Self> {
        self.set_entry_detail(Some(provider))?;
        Ok(self)
    }

    /// Whether directories without a matching descendant file are hidden.
    ///
    /// This policy is active only while [`Self::file_extensions`] is set.
    /// Recursive discovery follows ordinary directories but never directory
    /// symlinks, so a scoped Explorer cannot scan through a link outside its
    /// navigation root.
    #[must_use]
    pub const fn prune_unmatched_directories(&self) -> bool {
        self.prune_unmatched_directories
    }

    /// Hides directories unless they contain a visible file admitted by the
    /// current extension policy somewhere below them.
    pub fn set_prune_unmatched_directories(&mut self, prune: bool) -> io::Result<()> {
        if prune == self.prune_unmatched_directories {
            return Ok(());
        }
        let preferred = self.selected().map(|entry| entry.path.clone());
        let all_entries = read_entries(
            &self.cwd,
            self.show_hidden,
            self.parent_allowed_at(&self.cwd),
            self.file_extensions.as_deref(),
            prune,
        )?;
        self.prune_unmatched_directories = prune;
        self.all_entries = all_entries;
        self.rebuild_filtered(preferred.as_deref());
        Ok(())
    }

    /// Builder form of [`Self::set_prune_unmatched_directories`].
    pub fn with_prune_unmatched_directories(mut self, prune: bool) -> io::Result<Self> {
        self.set_prune_unmatched_directories(prune)?;
        Ok(self)
    }

    #[must_use]
    pub const fn scroll_offset(&self) -> usize {
        self.navigation.offset()
    }

    /// Number of matching files and folders, excluding the synthetic parent.
    #[must_use]
    pub fn match_count(&self) -> usize {
        self.entries.iter().filter(|entry| !entry.parent).count()
    }

    /// Number of unfiltered files and folders, excluding the synthetic parent.
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.all_entries
            .iter()
            .filter(|entry| !entry.parent)
            .count()
    }

    /// Most recently rendered outer area.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    /// Most recently rendered filter row.
    #[must_use]
    pub const fn filter_area(&self) -> Rect {
        self.filter.area()
    }

    /// Native cursor location for the focused filter after the latest render.
    #[must_use]
    pub const fn filter_cursor_position(&self) -> Option<Position> {
        self.filter.cursor_position()
    }

    /// Borrows the renderer-local filter editor for the shared semantic Tree
    /// interpreter. The Explorer remains the owner of editing/focus state;
    /// `Tree::widget_with_filter` only paints the filter slot declared by the
    /// exact published Tree.
    pub fn filter_input_mut(&mut self) -> &mut InputField {
        &mut self.filter
    }

    /// Whether a mouse selection drag started in the filter is active.
    #[must_use]
    pub const fn filter_dragging(&self) -> bool {
        self.filter.is_dragging()
    }

    /// Starts filter cursor placement or selection from a terminal cell.
    pub fn filter_mouse_down(&mut self, position: Position, extend: bool) -> bool {
        self.filter.mouse_down(position, extend)
    }

    /// Extends an active filter selection drag.
    pub fn filter_mouse_drag(&mut self, position: Position) -> bool {
        self.filter.mouse_drag(position)
    }

    /// Ends an active filter selection drag.
    pub fn filter_mouse_up(&mut self) -> bool {
        self.filter.mouse_up()
    }

    /// Inserts pasted or composed text at the filter cursor.
    pub fn insert_filter_text(&mut self, text: impl Into<String>) -> ExplorerEvent {
        self.focus_and_edit_filter(InputFieldAction::InsertText(text.into()))
    }

    /// Most recently rendered item viewport, excluding filter, path header,
    /// and scrollbar.
    #[must_use]
    pub const fn list_area(&self) -> Rect {
        self.list_area
    }

    /// Returns the item under a terminal cell from the most recent render.
    #[must_use]
    pub fn entry_at(&self, position: Position) -> Option<&ExplorerEntry> {
        self.entry_index_at(position)
            .and_then(|index| self.entries.get(index))
    }

    /// Selects the item under a terminal cell from the most recent render.
    pub fn select_at(&mut self, position: Position) -> bool {
        let Some(index) = self.entry_index_at(position) else {
            return false;
        };
        self.set_selected_index(index)
    }

    #[must_use]
    pub const fn pointer(&self) -> TerminalPointerState {
        self.navigation.pointer()
    }

    pub fn track_mouse(&mut self, event: &MouseEvent) -> bool {
        self.navigation.track_mouse(event)
    }

    /// Complete standalone pointer interpretation for Explorer. It preserves
    /// the established single-click selection/double-click activation model,
    /// and routes wheel/filter gestures through the same state as keyboard.
    pub fn handle_mouse(
        &mut self,
        event: &MouseEvent,
        clicks: &mut crate::DoubleClickTracker<PathBuf>,
    ) -> io::Result<ExplorerEvent> {
        self.track_mouse(event);
        let position = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if self.filter_area().contains(position) => {
                clicks.reset();
                Ok(
                    if self
                        .filter_mouse_down(position, event.modifiers.contains(KeyModifiers::SHIFT))
                    {
                        ExplorerEvent::FilterFocusChanged
                    } else {
                        ExplorerEvent::None
                    },
                )
            }
            MouseEventKind::Drag(MouseButton::Left) if self.filter_dragging() => {
                Ok(if self.filter_mouse_drag(position) {
                    ExplorerEvent::FilterChanged
                } else {
                    ExplorerEvent::None
                })
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.filter_mouse_up();
                Ok(ExplorerEvent::None)
            }
            MouseEventKind::ScrollUp if self.list_area.contains(position) => {
                clicks.reset();
                self.handle(ExplorerInput::Up)
            }
            MouseEventKind::ScrollDown if self.list_area.contains(position) => {
                clicks.reset();
                self.handle(ExplorerInput::Down)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(index) = self.entry_index_at(position) else {
                    clicks.reset();
                    return Ok(ExplorerEvent::None);
                };
                let path = self.entries[index].path.clone();
                let activate = clicks.click(path);
                let changed = self.set_selected_index(index);
                self.filter.set_focused(false);
                if activate {
                    self.open_selected()
                } else if changed {
                    Ok(ExplorerEvent::SelectionChanged)
                } else {
                    Ok(ExplorerEvent::None)
                }
            }
            _ => Ok(ExplorerEvent::None),
        }
    }

    /// Selects an entry by index, clamped to the available entries.
    pub fn set_selected_index(&mut self, index: usize) -> bool {
        if self.entries.is_empty() {
            self.navigation.select(None, 0);
            return false;
        }
        let next = index.min(self.entries.len() - 1);
        self.navigation.select(Some(next), self.entries.len())
    }

    /// Selects the entry matching an absolute path.
    pub fn select_path(&mut self, path: impl AsRef<Path>) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.path == path.as_ref())
        else {
            return false;
        };
        self.set_selected_index(index)
    }

    /// Changes directory and resets selection to its first entry.
    pub fn set_cwd(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
        let cwd = self.canonical_scoped_directory(path.as_ref())?;
        let all_entries = read_entries(
            &cwd,
            self.show_hidden,
            self.parent_allowed_at(&cwd),
            self.file_extensions.as_deref(),
            self.prune_unmatched_directories,
        )?;
        self.cwd = cwd;
        self.all_entries = all_entries;
        self.filter.clear();
        self.filter.set_focused(false);
        self.rebuild_filtered(None);
        Ok(())
    }

    /// Replaces a scoped Explorer's navigation root and current directory.
    ///
    /// This is the reusable project/worktree switch primitive: filtering
    /// policy, hidden-file policy, theme, and component configuration remain
    /// intact while navigation is atomically rebound to another canonical
    /// directory. The text filter and selection reset because their paths
    /// belong to the previous tree.
    pub fn set_navigation_root(&mut self, path: impl AsRef<Path>) -> io::Result<bool> {
        let cwd = canonical_directory(path.as_ref())?;
        if self.navigation_root.as_deref() == Some(cwd.as_path()) {
            return Ok(false);
        }
        let all_entries = read_entries(
            &cwd,
            self.show_hidden,
            false,
            self.file_extensions.as_deref(),
            self.prune_unmatched_directories,
        )?;
        self.navigation_root = Some(cwd.clone());
        self.cwd = cwd;
        self.all_entries = all_entries;
        self.filter.clear();
        self.filter.set_focused(false);
        self.rebuild_filtered(None);
        Ok(true)
    }

    /// Refreshes the current directory while preserving the selected path.
    pub fn refresh(&mut self) -> io::Result<()> {
        let preferred = self.selected().map(|entry| entry.path.clone());
        let previous = self.selected_index();
        let mut all_entries = read_entries(
            &self.cwd,
            self.show_hidden,
            self.parent_allowed_at(&self.cwd),
            self.file_extensions.as_deref(),
            self.prune_unmatched_directories,
        )?;
        if let Some(provider) = &self.detail_provider {
            for entry in all_entries.iter_mut().filter(|entry| !entry.directory) {
                entry.detail = provider.resolve(&entry.path);
            }
        }
        self.all_entries = all_entries;
        self.rebuild_filtered(preferred.as_deref());
        if preferred.is_none() {
            self.navigation.select(
                (!self.entries.is_empty())
                    .then_some(previous.min(self.entries.len().saturating_sub(1))),
                self.entries.len(),
            );
        }
        Ok(())
    }

    /// Changes hidden-file visibility while preserving selection when possible.
    pub fn set_show_hidden(&mut self, show: bool) -> io::Result<()> {
        if show == self.show_hidden {
            return Ok(());
        }
        let preferred = self.selected().map(|entry| entry.path.clone());
        let previous = self.selected_index();
        let all_entries = read_entries(
            &self.cwd,
            show,
            self.parent_allowed_at(&self.cwd),
            self.file_extensions.as_deref(),
            self.prune_unmatched_directories,
        )?;
        self.all_entries = all_entries;
        self.show_hidden = show;
        self.rebuild_filtered(preferred.as_deref());
        if preferred.is_none() {
            self.navigation.select(
                (!self.entries.is_empty())
                    .then_some(previous.min(self.entries.len().saturating_sub(1))),
                self.entries.len(),
            );
        }
        Ok(())
    }

    /// Converts a terminal key with the shared row decision table while
    /// preserving Explorer's filter focus and printable-character contract.
    /// App-specific commands (quit, create, menus) remain outside the helper.
    #[must_use]
    pub fn input_for_key(&self, key: &KeyEvent) -> Option<ExplorerInput> {
        if matches!(key.kind, KeyEventKind::Release) {
            return None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alternate = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let command = key
            .modifiers
            .intersects(KeyModifiers::SUPER | KeyModifiers::META);
        let non_text_modifier = key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        );
        if self.filter_focused() {
            return match key.code {
                KeyCode::Esc => Some(ExplorerInput::Parent),
                KeyCode::Tab | KeyCode::Down => Some(ExplorerInput::BlurFilter),
                KeyCode::Up => Some(ExplorerInput::Up),
                KeyCode::Left if command => Some(ExplorerInput::FilterHome { extend: shift }),
                KeyCode::Right if command => Some(ExplorerInput::FilterEnd { extend: shift }),
                KeyCode::Left => Some(ExplorerInput::FilterLeft {
                    extend: shift,
                    word: control || alternate,
                }),
                KeyCode::Right => Some(ExplorerInput::FilterRight {
                    extend: shift,
                    word: control || alternate,
                }),
                KeyCode::Home => Some(ExplorerInput::FilterHome { extend: shift }),
                KeyCode::End => Some(ExplorerInput::FilterEnd { extend: shift }),
                KeyCode::PageUp => Some(ExplorerInput::PageUp),
                KeyCode::PageDown => Some(ExplorerInput::PageDown),
                KeyCode::Enter => Some(ExplorerInput::Open),
                KeyCode::Backspace => Some(ExplorerInput::FilterBackspace),
                KeyCode::Char('h') if control => Some(ExplorerInput::FilterBackspace),
                KeyCode::Delete => Some(ExplorerInput::FilterDelete),
                KeyCode::Char('a') if control || command => Some(ExplorerInput::FilterSelectAll),
                KeyCode::Char('u') if control => Some(ExplorerInput::ClearFilter),
                KeyCode::Char(character) if !non_text_modifier => {
                    Some(ExplorerInput::FilterCharacter(character))
                }
                _ => None,
            };
        }
        match key.code {
            KeyCode::Char('h') if control => return Some(ExplorerInput::ToggleHidden),
            KeyCode::Char('f') if control => return Some(ExplorerInput::FocusFilter),
            KeyCode::Char('r') if control => return Some(ExplorerInput::Refresh),
            KeyCode::Tab | KeyCode::Char('/') => return Some(ExplorerInput::FocusFilter),
            KeyCode::Up if self.selected_index() == 0 => return Some(ExplorerInput::FocusFilter),
            KeyCode::Left | KeyCode::Backspace => return Some(ExplorerInput::Parent),
            KeyCode::Right => return Some(ExplorerInput::Open),
            _ => {}
        }
        let primary = self.selected().map_or(RowPrimaryRole::Static, |entry| {
            if entry.is_directory() || entry.is_parent() {
                RowPrimaryRole::Disclosure
            } else {
                RowPrimaryRole::Command
            }
        });
        match ListKeymap::new()
            .character_aliases(false)
            .decision_for_key(key, primary)
        {
            Some(RowKeyDecision::InvokePrimary) => Some(ExplorerInput::Open),
            Some(RowKeyDecision::Navigate(action)) => match action {
                ListNavigationAction::Down => Some(ExplorerInput::Down),
                ListNavigationAction::Up => Some(ExplorerInput::Up),
                ListNavigationAction::First => Some(ExplorerInput::First),
                ListNavigationAction::Last => Some(ExplorerInput::Last),
                ListNavigationAction::PageDown => Some(ExplorerInput::PageDown),
                ListNavigationAction::PageUp => Some(ExplorerInput::PageUp),
                ListNavigationAction::Activate => Some(ExplorerInput::Open),
                ListNavigationAction::Back => Some(ExplorerInput::Parent),
            },
            None => match key.code {
                KeyCode::Char(character) if !non_text_modifier => {
                    Some(ExplorerInput::FilterCharacter(character))
                }
                _ => None,
            },
        }
    }

    /// Updates navigation state from a backend-neutral action.
    pub fn handle(&mut self, input: ExplorerInput) -> io::Result<ExplorerEvent> {
        match input {
            ExplorerInput::Up => Ok(self.navigate_rows(ListNavigationAction::Up)),
            ExplorerInput::Down => Ok(self.navigate_rows(ListNavigationAction::Down)),
            ExplorerInput::First => Ok(self.navigate_rows(ListNavigationAction::First)),
            ExplorerInput::Last => Ok(self.navigate_rows(ListNavigationAction::Last)),
            ExplorerInput::PageUp => Ok(self.navigate_rows(ListNavigationAction::PageUp)),
            ExplorerInput::PageDown => Ok(self.navigate_rows(ListNavigationAction::PageDown)),
            ExplorerInput::Parent => self.open_parent(),
            ExplorerInput::Open => self.open_selected(),
            ExplorerInput::ToggleHidden => {
                self.set_show_hidden(!self.show_hidden)?;
                Ok(ExplorerEvent::Refreshed)
            }
            ExplorerInput::Refresh => {
                self.refresh()?;
                Ok(ExplorerEvent::Refreshed)
            }
            ExplorerInput::FocusFilter => {
                if self.set_filter_focused(true) {
                    Ok(ExplorerEvent::FilterFocusChanged)
                } else {
                    Ok(ExplorerEvent::None)
                }
            }
            ExplorerInput::BlurFilter => {
                if self.set_filter_focused(false) {
                    Ok(ExplorerEvent::FilterFocusChanged)
                } else {
                    Ok(ExplorerEvent::None)
                }
            }
            ExplorerInput::ClearFilter => {
                if self.clear_filter() {
                    Ok(ExplorerEvent::FilterChanged)
                } else {
                    Ok(ExplorerEvent::None)
                }
            }
            ExplorerInput::FilterCharacter(character) => {
                if character.is_control() {
                    return Ok(ExplorerEvent::None);
                }
                Ok(self.focus_and_edit_filter(InputFieldAction::Insert(character)))
            }
            ExplorerInput::FilterBackspace => Ok(self.edit_filter(InputFieldAction::Backspace)),
            ExplorerInput::FilterDelete => Ok(self.edit_filter(InputFieldAction::Delete)),
            ExplorerInput::FilterSelectAll => {
                self.filter.handle(InputFieldAction::SelectAll);
                Ok(ExplorerEvent::None)
            }
            ExplorerInput::FilterLeft { extend, word } => {
                self.filter.handle(InputFieldAction::Left { extend, word });
                Ok(ExplorerEvent::None)
            }
            ExplorerInput::FilterRight { extend, word } => {
                self.filter.handle(InputFieldAction::Right { extend, word });
                Ok(ExplorerEvent::None)
            }
            ExplorerInput::FilterHome { extend } => {
                self.filter.handle(InputFieldAction::Home { extend });
                Ok(ExplorerEvent::None)
            }
            ExplorerInput::FilterEnd { extend } => {
                self.filter.handle(InputFieldAction::End { extend });
                Ok(ExplorerEvent::None)
            }
            ExplorerInput::None => Ok(ExplorerEvent::None),
        }
    }

    /// Returns a Ratatui widget that also registers every rendered path with
    /// the supplied drag surface.
    pub fn widget<'explorer, 'drag>(
        &'explorer mut self,
        drags: &'drag mut DragSurface,
    ) -> ExplorerWidget<'explorer, 'drag> {
        ExplorerWidget {
            explorer: self,
            drags,
        }
    }

    fn semantic_id_for_path(&mut self, path: &Path) -> String {
        if let Some(id) = self.semantic_ids.get(path) {
            return id.clone();
        }
        let id = format!("entry-{}", self.next_semantic_id);
        self.next_semantic_id = self.next_semantic_id.saturating_add(1).max(1);
        self.semantic_ids.insert(path.to_path_buf(), id.clone());
        id
    }

    #[cfg(feature = "ui-bridge")]
    fn semantic_index(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|entry| {
            self.semantic_ids
                .get(&entry.path)
                .is_some_and(|candidate| candidate == id)
        })
    }

    fn entry_index_at(&self, position: Position) -> Option<usize> {
        self.navigation.item_at(position, self.entries.len())
    }

    fn rebuild_filtered(&mut self, preferred: Option<&Path>) {
        let needle = self.filter.text().to_lowercase();
        self.entries = self
            .all_entries
            .iter()
            .filter(|entry| {
                entry.parent || needle.is_empty() || entry.name.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        let first_match = self
            .entries
            .iter()
            .position(|entry| !entry.parent)
            .unwrap_or(0);
        let selected = preferred
            .and_then(|path| {
                self.entries.iter().position(|entry| {
                    entry.path == path && (self.filter.text().is_empty() || !entry.parent)
                })
            })
            .unwrap_or(first_match)
            .min(self.entries.len().saturating_sub(1));
        self.navigation.set_offset(0, self.entries.len());
        self.navigation.select(
            (!self.entries.is_empty()).then_some(selected),
            self.entries.len(),
        );
        self.navigation.request_reveal();
    }

    fn edit_filter(&mut self, action: InputFieldAction) -> ExplorerEvent {
        let before = self.filter.text().to_owned();
        self.filter.handle(action);
        if self.filter.text() == before {
            return ExplorerEvent::None;
        }
        let preferred = self.selected().map(|entry| entry.path.clone());
        self.rebuild_filtered(preferred.as_deref());
        ExplorerEvent::FilterChanged
    }

    fn focus_and_edit_filter(&mut self, action: InputFieldAction) -> ExplorerEvent {
        let focus_changed = self.filter.set_focused(true);
        let event = self.edit_filter(action);
        if event == ExplorerEvent::None && focus_changed {
            ExplorerEvent::FilterFocusChanged
        } else {
            event
        }
    }

    fn replace_file_extensions(&mut self, extensions: Option<Vec<String>>) -> io::Result<()> {
        let preferred = self.selected().map(|entry| entry.path.clone());
        let all_entries = read_entries(
            &self.cwd,
            self.show_hidden,
            self.parent_allowed_at(&self.cwd),
            extensions.as_deref(),
            self.prune_unmatched_directories,
        )?;
        self.file_extensions = extensions;
        self.all_entries = all_entries;
        self.rebuild_filtered(preferred.as_deref());
        Ok(())
    }

    fn navigate_rows(&mut self, action: ListNavigationAction) -> ExplorerEvent {
        match self.navigation.navigate(action, self.entries.len()) {
            ListNavigationOutcome::SelectionChanged(_) => ExplorerEvent::SelectionChanged,
            ListNavigationOutcome::None
            | ListNavigationOutcome::Scrolled(_)
            | ListNavigationOutcome::Activate(_)
            | ListNavigationOutcome::Back
            | ListNavigationOutcome::FocusedBack => ExplorerEvent::None,
        }
    }

    fn open_parent(&mut self) -> io::Result<ExplorerEvent> {
        if !self.parent_allowed_at(&self.cwd) {
            return Ok(ExplorerEvent::None);
        }
        let Some(parent) = self.cwd.parent().map(Path::to_path_buf) else {
            return Ok(ExplorerEvent::None);
        };
        self.change_directory(parent)
    }

    fn open_selected(&mut self) -> io::Result<ExplorerEvent> {
        let Some(entry) = self.selected().cloned() else {
            return Ok(ExplorerEvent::None);
        };
        self.filter.set_focused(false);
        if entry.directory {
            self.change_directory(entry.path)
        } else {
            Ok(ExplorerEvent::FileActivated(entry.path))
        }
    }

    fn change_directory(&mut self, path: PathBuf) -> io::Result<ExplorerEvent> {
        let previous = self.cwd.clone();
        let cwd = self.canonical_scoped_directory(&path)?;
        let all_entries = read_entries(
            &cwd,
            self.show_hidden,
            self.parent_allowed_at(&cwd),
            self.file_extensions.as_deref(),
            self.prune_unmatched_directories,
        )?;
        let selected = all_entries
            .iter()
            .position(|entry| entry.path == previous)
            .unwrap_or(0)
            .min(all_entries.len().saturating_sub(1));
        self.cwd = cwd;
        self.all_entries = all_entries;
        self.filter.clear();
        self.filter.set_focused(false);
        self.entries = self.all_entries.clone();
        self.navigation.set_offset(0, self.entries.len());
        self.navigation.select(
            (!self.entries.is_empty()).then_some(selected),
            self.entries.len(),
        );
        self.navigation.request_reveal();
        Ok(ExplorerEvent::DirectoryChanged(self.cwd.clone()))
    }

    fn parent_allowed_at(&self, cwd: &Path) -> bool {
        cwd.parent().is_some() && self.navigation_root.as_ref().is_none_or(|root| cwd != root)
    }

    fn canonical_scoped_directory(&self, path: &Path) -> io::Result<PathBuf> {
        let directory = canonical_directory(path)?;
        if let Some(root) = self.navigation_root.as_ref()
            && !directory.starts_with(root)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is outside Explorer root {}",
                    directory.display(),
                    root.display()
                ),
            ));
        }
        Ok(directory)
    }

    fn render(&mut self, area: Rect, buffer: &mut Buffer, drags: &mut DragSurface) {
        self.area = area;
        buffer.set_style(area, self.theme.style);
        if area.is_empty() {
            self.filter.clear_render_state();
            self.list_area = Rect::default();
            self.navigation.prepare(Rect::default(), self.entries.len());
            return;
        }

        let filter_height = u16::from(self.show_filter && area.height > 0);
        if filter_height == 1 {
            let filter_area = Rect::new(area.x, area.y, area.width, 1);
            // The filter is a row like any other: keyboard focus paints the
            // shared selection background behind it while the cursor types.
            let mut theme = input_theme(&self.theme);
            if self.filter.focused() {
                theme.style = self.theme.style.patch(self.theme.selected);
            }
            self.filter.set_theme(theme);
            self.filter.widget().render(filter_area, buffer);
        } else {
            self.filter.clear_render_state();
        }

        // The path row is the screen title; keep one padding row beneath it.
        let path_height = if !self.show_path {
            0
        } else {
            match area.height.saturating_sub(filter_height) {
                0 => 0,
                1 => 1,
                _ => 2,
            }
        };
        if path_height >= 1 {
            let header = Rect::new(area.x, area.y.saturating_add(filter_height), area.width, 1);
            let label_width = area.width.saturating_sub(self.theme.left_padding);
            let label = format!(
                "{}{}",
                " ".repeat(usize::from(self.theme.left_padding.min(area.width))),
                truncate_start(
                    &self.navigation_root.as_deref().map_or_else(
                        || self.cwd.display().to_string(),
                        |root| crate::display_path_from_root(&self.cwd, root),
                    ),
                    label_width,
                ),
            );
            let style = self.theme.style.patch(self.theme.path);
            buffer.set_style(header, style);
            Line::styled(label, style).render(header, buffer);
            drags.register(header, &self.cwd);
        }

        let header_height = filter_height.saturating_add(path_height);
        let list_y = area.y.saturating_add(header_height);
        let list_height = area.height.saturating_sub(header_height);
        let overflow = list_height > 0 && self.entries.len() > usize::from(list_height);
        let list_width = area.width.saturating_sub(u16::from(overflow));
        self.list_area = Rect::new(area.x, list_y, list_width, list_height);
        self.navigation
            .set_navigation(self.theme.scroll_padding, 0, ListPageBehavior::Selection);
        self.navigation.prepare(self.list_area, self.entries.len());

        let visible = usize::from(list_height);
        if self.match_count() == 0 && visible > self.entries.len() {
            let label = format!(
                "{}{}",
                " ".repeat(usize::from(
                    self.theme.left_padding.min(self.list_area.width)
                )),
                if self.filter.text().is_empty() {
                    "empty folder"
                } else {
                    "no matches"
                },
            );
            let message_area = Rect::new(
                self.list_area.x,
                self.list_area.y.saturating_add(self.entries.len() as u16),
                self.list_area.width,
                1,
            );
            let style = self.theme.style.patch(self.theme.empty);
            Line::styled(label, style).render(message_area, buffer);
        }

        for (slot, index) in (self.navigation.offset()..self.entries.len())
            .take(visible)
            .enumerate()
        {
            let row = Rect::new(
                self.list_area.x,
                self.list_area.y.saturating_add(slot as u16),
                self.list_area.width,
                1,
            );
            let entry = &self.entries[index];
            let selected = self.navigation.selected() == Some(index);
            let pointer_phase = self.navigation.pointer_phase_at(index);
            let highlighted = selected || pointer_phase != TerminalPointerPhase::Idle;
            let inactive_style = self.entry_style(entry);
            let active_style = inactive_style.patch(match pointer_phase {
                TerminalPointerPhase::Idle | TerminalPointerPhase::Hovered if selected => {
                    self.theme.selected
                }
                TerminalPointerPhase::Idle => self.theme.selected,
                TerminalPointerPhase::Hovered => self.theme.hovered,
                TerminalPointerPhase::Pressed => self.theme.selected.add_modifier(Modifier::BOLD),
            });
            let content = SelectableRow::new(highlighted, active_style)
                .inactive_style(inactive_style)
                .left_padding(self.theme.left_padding)
                .right_padding(0)
                .paint(row, buffer);
            let label = self.entry_label(entry, selected, content.width);
            let label_style = if highlighted {
                active_style
            } else {
                inactive_style
            };
            let mut spans = vec![Span::styled(label.clone(), label_style)];
            if let Some(detail) = entry.detail() {
                let remaining = usize::from(content.width)
                    .saturating_sub(display_width(&label))
                    .saturating_sub(2);
                if remaining > 0 {
                    let detail_style = if highlighted {
                        active_style.add_modifier(Modifier::DIM)
                    } else {
                        self.theme.style.patch(self.theme.filter)
                    };
                    spans.push(Span::styled(
                        format!("  {}", truncate_end(detail, remaining)),
                        detail_style,
                    ));
                }
            }
            Line::from(spans).render(content, buffer);
            drags.register(row, &entry.path);
        }

        if overflow {
            let scrollbar = Rect::new(
                area.right().saturating_sub(1),
                list_y,
                area.width.min(1),
                list_height,
            );
            VerticalScrollbar::new(
                self.entries.len(),
                usize::from(list_height),
                self.navigation.offset(),
            )
            .track_style(self.theme.scrollbar_track)
            .thumb_style(self.theme.scrollbar_thumb)
            .render(scrollbar, buffer);
        }
    }

    fn entry_style(&self, entry: &ExplorerEntry) -> Style {
        let kind = if entry.parent {
            self.theme.parent
        } else if entry.symlink {
            self.theme.symlink
        } else if entry.directory {
            self.theme.directory
        } else {
            self.theme.item
        };
        self.theme.style.patch(kind)
    }

    fn entry_label(&self, entry: &ExplorerEntry, selected: bool, width: u16) -> String {
        if width == 0 {
            return String::new();
        }
        let symbol = self.theme.selected_symbol.as_deref().unwrap_or("");
        let marker = if selected {
            symbol.to_owned()
        } else {
            " ".repeat(display_width(symbol))
        };
        let prefix = marker;
        let remaining = usize::from(width).saturating_sub(display_width(&prefix));
        format!("{prefix}{}", truncate_end(&entry.display_name(), remaining))
    }
}

/// Renderable view returned by [`Explorer::widget`].
pub struct ExplorerWidget<'explorer, 'drag> {
    explorer: &'explorer mut Explorer,
    drags: &'drag mut DragSurface,
}

impl Widget for ExplorerWidget<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.explorer.render(area, buffer, self.drags);
    }
}

fn input_theme(theme: &ExplorerTheme) -> InputFieldTheme {
    InputFieldTheme {
        style: theme.style,
        text: theme.filter,
        focused: theme.filter_focused,
        placeholder: theme.filter_placeholder,
        prompt: theme.filter,
        selection: theme.selected,
        left_padding: theme.left_padding,
    }
}

fn canonical_directory(path: &Path) -> io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    if fs::metadata(&path)?.is_dir() {
        Ok(path)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        ))
    }
}

fn read_entries(
    cwd: &Path,
    show_hidden: bool,
    include_parent: bool,
    file_extensions: Option<&[String]>,
    prune_unmatched_directories: bool,
) -> io::Result<Vec<ExplorerEntry>> {
    let mut entries = Vec::new();
    if include_parent && let Some(parent) = cwd.parent() {
        entries.push(ExplorerEntry {
            path: parent.to_path_buf(),
            name: "..".to_owned(),
            directory: true,
            symlink: false,
            parent: true,
            detail: None,
        });
    }

    let mut children = fs::read_dir(cwd)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let raw_name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && raw_name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let symlink = fs::symlink_metadata(&path)
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_symlink());
            let directory = fs::metadata(&path)
                .ok()
                .is_some_and(|metadata| metadata.is_dir());
            if let Some(extensions) = file_extensions {
                if directory {
                    if prune_unmatched_directories
                        && (symlink
                            || !directory_contains_matching_file(&path, extensions, show_hidden))
                    {
                        return None;
                    }
                } else if !path_matches_extensions(&path, extensions) {
                    return None;
                }
            }
            Some(ExplorerEntry {
                path,
                name: sanitize_label(&raw_name),
                directory,
                symlink,
                parent: false,
                detail: None,
            })
        })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        right
            .directory
            .cmp(&left.directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.path.cmp(&right.path))
    });
    entries.extend(children);
    Ok(entries)
}

fn path_matches_extensions(path: &Path, extensions: &[String]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|actual| {
            extensions
                .iter()
                .any(|allowed| actual.eq_ignore_ascii_case(allowed))
        })
}

fn directory_contains_matching_file(root: &Path, extensions: &[String], show_hidden: bool) -> bool {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let raw_name = entry.file_name();
            if !show_hidden && raw_name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                if fs::metadata(&path)
                    .ok()
                    .is_some_and(|target| target.is_file())
                    && path_matches_extensions(&path, extensions)
                {
                    return true;
                }
                continue;
            }
            if metadata.is_file() && path_matches_extensions(&path, extensions) {
                return true;
            }
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    false
}

fn normalize_extensions<I, S>(extensions: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut extensions = extensions
        .into_iter()
        .map(|extension| {
            extension
                .as_ref()
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|extension| !extension.is_empty())
        .collect::<Vec<_>>();
    extensions.sort();
    extensions.dedup();
    extensions
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

fn truncate_end(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if used.saturating_add(character_width) > width {
            break;
        }
        output.push(character);
        used = used.saturating_add(character_width);
    }
    output
}

fn truncate_start(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if display_width(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let mut suffix = Vec::new();
    let mut used = 1usize;
    for character in text.chars().rev() {
        let character_width = character.width().unwrap_or(0);
        if used.saturating_add(character_width) > width {
            break;
        }
        suffix.push(character);
        used = used.saturating_add(character_width);
    }
    suffix.reverse();
    format!("…{}", suffix.into_iter().collect::<String>())
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use ratatui::widgets::Widget as _;

    use super::*;

    fn entry_names(explorer: &Explorer) -> Vec<String> {
        explorer
            .entries()
            .iter()
            .map(ExplorerEntry::display_name)
            .collect()
    }

    fn legacy_row_buffer(explorer: &Explorer, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        buffer.set_style(area, explorer.theme.style);
        let list = explorer.list_area;
        let visible = usize::from(list.height);
        for (slot, index) in (explorer.scroll_offset()..explorer.entries.len())
            .take(visible)
            .enumerate()
        {
            let row = Rect::new(list.x, list.y.saturating_add(slot as u16), list.width, 1);
            let entry = &explorer.entries[index];
            let selected = explorer.selected_index() == index;
            let mut style = explorer.entry_style(entry);
            if selected {
                style = style.patch(explorer.theme.selected);
            }
            buffer.set_style(row, style);
            let padding = " ".repeat(usize::from(explorer.theme.left_padding.min(row.width)));
            let symbol = explorer.theme.selected_symbol.as_deref().unwrap_or("");
            let marker = if selected {
                symbol.to_owned()
            } else {
                " ".repeat(display_width(symbol))
            };
            let prefix = format!("{padding}{marker}");
            let remaining = usize::from(row.width).saturating_sub(display_width(&prefix));
            let label = format!("{prefix}{}", truncate_end(&entry.display_name(), remaining));
            Line::styled(label, style).render(row, &mut buffer);
        }
        if list.height > 0 && explorer.entries.len() > visible {
            let scrollbar = Rect::new(
                area.right().saturating_sub(1),
                list.y,
                area.width.min(1),
                list.height,
            );
            VerticalScrollbar::new(explorer.entries.len(), visible, explorer.scroll_offset())
                .track_style(explorer.theme.scrollbar_track)
                .thumb_style(explorer.theme.scrollbar_thumb)
                .render(scrollbar, &mut buffer);
        }
        buffer
    }

    #[test]
    fn flat_listing_hides_dotfiles_and_sorts_directories_first() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("z-dir")).unwrap();
        fs::create_dir(temp.path().join("a-dir")).unwrap();
        fs::write(temp.path().join("a.txt"), "a").unwrap();
        fs::write(temp.path().join(".secret"), "secret").unwrap();

        let mut explorer = Explorer::new(temp.path()).unwrap();
        assert_eq!(entry_names(&explorer), ["../", "a-dir/", "z-dir/", "a.txt"]);

        explorer.set_show_hidden(true).unwrap();
        assert_eq!(
            entry_names(&explorer),
            ["../", "a-dir/", "z-dir/", ".secret", "a.txt"]
        );
    }

    #[cfg(feature = "ui-bridge")]
    #[test]
    fn semantic_context_targets_resolve_without_exposing_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("first.txt"), "first").unwrap();
        fs::write(temp.path().join("second.txt"), "second").unwrap();
        let mut explorer = Explorer::scoped(temp.path()).unwrap();
        let tree = explorer.semantic_tree("Files");
        let second_id = tree.items[1].id.clone();

        assert_eq!(
            explorer.select_semantic_item(&second_id).unwrap(),
            ExplorerEvent::SelectionChanged
        );
        assert_eq!(explorer.selected().unwrap().name(), "second.txt");
        assert!(explorer.select_semantic_item("entry-missing").is_err());
    }

    #[test]
    fn scoped_navigation_stops_at_its_canonical_root() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let child = fs::canonicalize(child).unwrap();

        let mut explorer = Explorer::scoped(&root).unwrap();
        assert_eq!(explorer.navigation_root(), Some(root.as_path()));
        assert_eq!(entry_names(&explorer), ["child/"]);
        assert_eq!(
            explorer.handle(ExplorerInput::Parent).unwrap(),
            ExplorerEvent::None
        );
        assert_eq!(explorer.cwd(), root);

        explorer.select_path(&child);
        assert_eq!(explorer.selected().unwrap().path(), child);
        assert_eq!(
            explorer.handle(ExplorerInput::Open).unwrap(),
            ExplorerEvent::DirectoryChanged(child.clone())
        );
        assert_eq!(entry_names(&explorer), ["../"]);
        assert_eq!(
            explorer.handle(ExplorerInput::Parent).unwrap(),
            ExplorerEvent::DirectoryChanged(root.clone())
        );
        assert_eq!(explorer.cwd(), root);
        assert_eq!(
            explorer.set_cwd(root.parent().unwrap()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn scoped_explorer_can_rebind_to_another_project_root() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        fs::write(first.path().join("main.txt"), "main").unwrap();
        fs::write(second.path().join("worktree.txt"), "worktree").unwrap();
        let mut explorer = Explorer::scoped(first.path()).unwrap();

        assert!(explorer.set_navigation_root(second.path()).unwrap());
        assert_eq!(explorer.cwd(), second.path().canonicalize().unwrap());
        assert_eq!(
            explorer.navigation_root(),
            Some(second.path().canonicalize().unwrap().as_path())
        );
        assert!(
            explorer
                .entries()
                .iter()
                .any(|entry| entry.name() == "worktree.txt")
        );
        assert!(!explorer.set_navigation_root(second.path()).unwrap());
        assert!(explorer.set_navigation_root(first.path()).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn scoped_navigation_denies_symlinks_that_escape_the_root() {
        use std::os::unix::fs::symlink;

        let root_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let escape = root_dir.path().join("escape");
        symlink(outside_dir.path(), &escape).unwrap();

        let mut explorer = Explorer::scoped(root_dir.path()).unwrap();
        explorer.select_path(&escape);
        assert!(explorer.selected().unwrap().path().ends_with("escape"));
        let error = explorer.handle(ExplorerInput::Open).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(explorer.cwd(), fs::canonicalize(root_dir.path()).unwrap());
    }

    #[test]
    fn file_extension_policy_keeps_directories_and_matches_case_insensitively() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("folder")).unwrap();
        fs::write(temp.path().join("note.md"), "note").unwrap();
        fs::write(temp.path().join("A.MD"), "note").unwrap();
        fs::write(temp.path().join("notes.txt"), "text").unwrap();
        fs::write(temp.path().join("LICENSE"), "text").unwrap();

        let mut explorer = Explorer::scoped(temp.path())
            .unwrap()
            .with_file_extensions([".MD", "md"])
            .unwrap();
        assert_eq!(
            explorer.file_extensions(),
            Some(["md".to_owned()].as_slice())
        );
        assert_eq!(entry_names(&explorer), ["folder/", "A.MD", "note.md"]);

        explorer.clear_file_extensions().unwrap();
        assert_eq!(explorer.file_extensions(), None);
        assert_eq!(explorer.total_count(), 5);
    }

    #[test]
    fn extension_policy_can_prune_directories_without_matching_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let notes = temp.path().join("notes");
        let nested = notes.join("nested");
        let empty = temp.path().join("empty");
        let hidden_only = temp.path().join("hidden-file-only");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(&empty).unwrap();
        fs::create_dir(&hidden_only).unwrap();
        fs::write(nested.join("deep.MD"), "note").unwrap();
        fs::write(empty.join("other.txt"), "text").unwrap();
        fs::write(hidden_only.join(".private.md"), "note").unwrap();
        fs::write(temp.path().join("root.md"), "note").unwrap();

        let mut explorer = Explorer::scoped(temp.path()).unwrap();
        explorer.set_prune_unmatched_directories(true).unwrap();
        explorer.set_file_extensions([".md"]).unwrap();

        assert!(explorer.prune_unmatched_directories());
        assert_eq!(entry_names(&explorer), ["notes/", "root.md"]);

        explorer.set_show_hidden(true).unwrap();
        assert_eq!(
            entry_names(&explorer),
            ["hidden-file-only/", "notes/", "root.md"]
        );

        let notes = fs::canonicalize(notes).unwrap();
        explorer.select_path(&notes);
        assert_eq!(explorer.selected().unwrap().path(), notes);
        assert_eq!(
            explorer.handle(ExplorerInput::Open).unwrap(),
            ExplorerEvent::DirectoryChanged(notes.clone())
        );
        assert_eq!(entry_names(&explorer), ["../", "nested/"]);

        assert!(matches!(
            explorer.handle(ExplorerInput::Parent).unwrap(),
            ExplorerEvent::DirectoryChanged(_)
        ));
        explorer.clear_file_extensions().unwrap();
        assert_eq!(
            entry_names(&explorer),
            ["empty/", "hidden-file-only/", "notes/", "root.md"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn matching_directory_scan_never_follows_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("outside.md"), "note").unwrap();
        let linked = root.path().join("linked");
        symlink(outside.path(), &linked).unwrap();

        let mut explorer = Explorer::scoped(root.path()).unwrap();
        explorer.set_prune_unmatched_directories(true).unwrap();
        explorer.set_file_extensions(["md"]).unwrap();

        assert!(!explorer.select_path(linked));
        assert!(explorer.entries().is_empty());
    }

    #[test]
    fn open_and_parent_navigation_return_observable_events() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("note.md"), "hello").unwrap();

        let mut explorer = Explorer::new(temp.path()).unwrap();
        let folder = fs::canonicalize(folder).unwrap();
        assert!(explorer.select_path(&folder));
        assert_eq!(
            explorer.handle(ExplorerInput::Open).unwrap(),
            ExplorerEvent::DirectoryChanged(folder.clone())
        );
        assert_eq!(explorer.cwd(), folder);
        let note = folder.join("note.md");
        assert!(explorer.select_path(&note));
        assert_eq!(
            explorer.handle(ExplorerInput::Open).unwrap(),
            ExplorerEvent::FileActivated(note)
        );

        let previous = fs::canonicalize(temp.path()).unwrap();
        assert_eq!(
            explorer.handle(ExplorerInput::Parent).unwrap(),
            ExplorerEvent::DirectoryChanged(previous.clone())
        );
        assert_eq!(explorer.cwd(), previous);
        assert_eq!(explorer.selected().unwrap().path(), folder);
    }

    #[test]
    fn selection_wraps_and_pages_by_the_rendered_viewport() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..10 {
            fs::write(temp.path().join(format!("{index}.txt")), "x").unwrap();
        }
        let mut explorer = Explorer::new(temp.path()).unwrap();
        let mut drags = DragSurface::disabled();
        // Six rows: path title, padding, and a four-row viewport.
        let area = Rect::new(0, 0, 20, 6);
        let mut buffer = Buffer::empty(area);
        explorer.widget(&mut drags).render(area, &mut buffer);

        assert_eq!(explorer.selected_index(), 0);
        explorer.handle(ExplorerInput::Up).unwrap();
        assert_eq!(explorer.selected_index(), explorer.entries().len() - 1);
        explorer.handle(ExplorerInput::Down).unwrap();
        assert_eq!(explorer.selected_index(), 0);
        explorer.handle(ExplorerInput::PageDown).unwrap();
        assert_eq!(explorer.selected_index(), 3);
    }

    #[test]
    fn explorer_pointer_shares_selection_and_activation_state_with_keyboard() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.md"), "a").unwrap();
        fs::write(temp.path().join("b.md"), "b").unwrap();
        let mut explorer = Explorer::scoped(temp.path()).unwrap();
        let mut drags = DragSurface::disabled();
        let area = Rect::new(0, 0, 24, 6);
        let mut buffer = Buffer::empty(area);
        explorer.widget(&mut drags).render(area, &mut buffer);
        let target = Position::new(explorer.list_area().x + 4, explorer.list_area().y + 1);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.x,
            row: target.y,
            modifiers: KeyModifiers::NONE,
        };
        let mut clicks = crate::DoubleClickTracker::new();

        assert_eq!(
            explorer.handle_mouse(&click, &mut clicks).unwrap(),
            ExplorerEvent::SelectionChanged
        );
        let activated = explorer.handle_mouse(&click, &mut clicks).unwrap();
        assert!(matches!(activated, ExplorerEvent::FileActivated(path) if path.ends_with("b.md")));

        let mut pressed = Buffer::empty(area);
        explorer.widget(&mut drags).render(area, &mut pressed);
        assert!(pressed[target].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn selectable_row_refactor_is_buffer_identical_to_legacy_explorer_rows() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("folder")).unwrap();
        for index in 0..8 {
            fs::write(temp.path().join(format!("file-{index}.md")), "x").unwrap();
        }
        let theme = ExplorerTheme {
            style: Style::new().bg(Color::Black),
            selected: Style::new().fg(Color::White).bg(Color::Red),
            selected_symbol: Some("› ".to_owned()),
            left_padding: 2,
            scroll_padding: 1,
            ..ExplorerTheme::default()
        };
        let mut explorer = Explorer::new(temp.path()).unwrap().with_theme(theme);
        let area = Rect::new(0, 0, 26, 6);
        let mut drags = DragSurface::disabled();
        let mut actual = Buffer::empty(area);
        explorer.widget(&mut drags).render(area, &mut actual);
        explorer.handle(ExplorerInput::PageDown).unwrap();
        explorer.widget(&mut drags).render(area, &mut actual);

        let expected = legacy_row_buffer(&explorer, area);
        for y in explorer.list_area.y..explorer.list_area.bottom() {
            for x in area.x..area.right() {
                assert_eq!(
                    actual[(x, y)],
                    expected[(x, y)],
                    "Explorer cell drifted at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn explorer_keys_use_the_shared_row_decision_table_without_stealing_filter_text() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("folder")).unwrap();
        fs::write(temp.path().join("note.md"), "x").unwrap();
        let mut explorer = Explorer::scoped(temp.path()).unwrap();

        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(ExplorerInput::Open)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(ExplorerInput::PageDown)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(ExplorerInput::Parent)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(ExplorerInput::FilterCharacter('j'))
        );
        explorer.set_filter_focused(true);
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(ExplorerInput::FilterCharacter('k'))
        );
    }

    #[test]
    fn borderless_render_is_full_row_selected_and_registers_every_path() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("folder")).unwrap();
        fs::write(temp.path().join("note.md"), "hello").unwrap();
        let theme = ExplorerTheme {
            selected: Style::new().bg(Color::Red),
            ..ExplorerTheme::default()
        };
        let mut explorer = Explorer::new(temp.path()).unwrap().with_theme(theme);
        let mut drags = DragSurface::disabled();
        // Filter, path title, padding row, then three entry rows.
        let area = Rect::new(0, 0, 24, 6);
        let mut buffer = Buffer::empty(area);
        drags.begin_frame();

        explorer.widget(&mut drags).render(area, &mut buffer);

        assert_ne!(buffer[(0, 0)].symbol(), "┌");
        assert_eq!(buffer[(23, 3)].bg, Color::Red);
        assert_eq!(buffer[(0, 3)].symbol(), " ");
        assert_eq!(buffer[(1, 3)].symbol(), " ");
        assert_eq!(drags.regions().len(), 4);
        assert_eq!(drags.regions()[0].area, Rect::new(0, 1, 24, 1), "path row");
        assert_eq!(
            drags.regions()[0].path,
            fs::canonicalize(temp.path()).unwrap()
        );
        assert!(
            drags
                .regions()
                .iter()
                .all(|region| region.path.is_absolute())
        );
    }

    #[test]
    fn overflow_uses_shared_scrollbar_and_hit_testing_excludes_it() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..8 {
            fs::write(temp.path().join(format!("{index}.txt")), "x").unwrap();
        }
        let mut explorer = Explorer::new(temp.path()).unwrap();
        let mut drags = DragSurface::disabled();
        let area = Rect::new(2, 3, 12, 5);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 10));

        explorer.widget(&mut drags).render(area, &mut buffer);

        // Path title plus its padding row occupy the first two rows.
        assert_eq!(explorer.list_area(), Rect::new(2, 6, 11, 2));
        assert_eq!(buffer[(13, 6)].symbol(), "┃");
        assert!(explorer.entry_at(Position::new(2, 6)).is_some());
        assert!(explorer.entry_at(Position::new(13, 6)).is_none());
        assert!(explorer.select_at(Position::new(2, 7)));
        assert_eq!(explorer.selected_index(), 1);
    }

    #[test]
    fn filter_is_case_insensitive_keeps_parent_and_selects_first_match() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("Source")).unwrap();
        fs::write(temp.path().join("README.md"), "hello").unwrap();
        fs::write(temp.path().join("notes.txt"), "hello").unwrap();

        let mut explorer = Explorer::new(temp.path()).unwrap();
        assert!(!explorer.filter_focused());
        for character in "Me.MD".chars() {
            assert_eq!(
                explorer
                    .handle(ExplorerInput::FilterCharacter(character))
                    .unwrap(),
                ExplorerEvent::FilterChanged
            );
        }

        assert_eq!(entry_names(&explorer), ["../", "README.md"]);
        assert_eq!(explorer.match_count(), 1);
        assert_eq!(explorer.total_count(), 3);
        assert_eq!(explorer.selected().unwrap().display_name(), "README.md");
        assert!(explorer.filter_focused());
    }

    #[test]
    fn typing_or_pasting_focuses_the_filter_automatically() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("alpha.txt"), "hello").unwrap();
        let mut explorer = Explorer::new(temp.path()).unwrap();

        assert_eq!(
            explorer
                .handle(ExplorerInput::FilterCharacter('a'))
                .unwrap(),
            ExplorerEvent::FilterChanged
        );
        assert!(explorer.filter_focused());

        explorer.set_filter_focused(false);
        explorer.clear_filter();
        assert_eq!(
            explorer.insert_filter_text("alpha"),
            ExplorerEvent::FilterChanged
        );
        assert!(explorer.filter_focused());
        assert_eq!(explorer.filter(), "alpha");
    }

    #[test]
    fn filter_backspace_is_unicode_safe_and_clear_restores_entries() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("界.txt"), "hello").unwrap();
        fs::write(temp.path().join("other.txt"), "hello").unwrap();
        let mut explorer = Explorer::new(temp.path()).unwrap();

        explorer.set_filter("界");
        assert_eq!(entry_names(&explorer), ["../", "界.txt"]);
        assert_eq!(
            explorer.handle(ExplorerInput::FilterBackspace).unwrap(),
            ExplorerEvent::FilterChanged
        );
        assert_eq!(explorer.filter(), "");
        assert_eq!(explorer.match_count(), 2);
        assert_eq!(
            explorer.handle(ExplorerInput::FilterBackspace).unwrap(),
            ExplorerEvent::None
        );
        assert_eq!(
            explorer
                .handle(ExplorerInput::FilterCharacter('\n'))
                .unwrap(),
            ExplorerEvent::None
        );
    }

    #[test]
    fn labels_are_control_safe_and_wide_character_aware() {
        assert_eq!(sanitize_label("bad\u{1b}name\n"), "bad�name�");
        assert_eq!(truncate_end("ab界c", 4), "ab界");
        assert_eq!(truncate_end("ab界c", 3), "ab");
        assert_eq!(truncate_start("/long/界/path", 7), "…/path");
    }
}
