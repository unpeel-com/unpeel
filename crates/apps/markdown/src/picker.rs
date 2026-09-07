//! Vault picker built from App Kit's scoped Explorer. Folders remain
//! navigable, only Markdown files are admitted, Enter opens the selected
//! item, and Ctrl-N creates a note in the current folder.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::{DefaultTerminal, Frame};
use unpeel_app_kit::{
    DoubleClickTracker, DragSurface, Explorer, ExplorerEntryDetail, ExplorerEvent, ExplorerInput,
    ExplorerTheme, FooterAction, Input, InputField, InputFieldTheme, List, ListState, Page,
    PageTheme, ThemeMonitor, TreeState, TreeTheme, UiBridge, UiBridgeEvent, UiComponent, UiEvent,
    UiEventKind, UiEventOutcome, UiEventValue, UiNode, tree_delta_operations,
};

use crate::theme::Theme;

const UI_VIEW_ID: &str = "main";
const UI_TREE_ID: &str = "notes-tree";
const UI_NEW_NOTE_ID: &str = "new-note";
const UI_NEW_NOTE_ACTION: &str = "new-note";
const UI_NEW_NOTE_INPUT_ID: &str = "new-note-name";
const UI_NEW_NOTE_SET_VALUE: &str = "set-new-note-name";
const UI_NEW_NOTE_SUBMIT: &str = "create-new-note";
const UI_NEW_NOTE_CANCEL: &str = "cancel-new-note";
const UI_PREVIOUS_ID: &str = "previous-note";
const UI_PREVIOUS_ACTION: &str = "previous-note";
const UI_REFRESH_ID: &str = "refresh-notes";
const UI_REFRESH_ACTION: &str = "refresh-notes";

pub struct Picker {
    root: PathBuf,
    explorer: Explorer,
    drags: DragSurface,
    clicks: DoubleClickTracker<PathBuf>,
    status: Option<String>,
    create: Option<CreateState>,
    tree_state: TreeState,
    create_input: InputField,
    theme: Theme,
    theme_monitor: ThemeMonitor,
}

impl Picker {
    pub fn open(root: PathBuf, theme: Theme) -> io::Result<Self> {
        let mut explorer = Explorer::scoped(root)?
            .with_file_extensions(["md"])?
            .with_entry_detail(ExplorerEntryDetail::new(first_heading))?
            .with_theme(ExplorerTheme::for_theme(theme.kit));
        explorer.set_show_path(false);
        explorer.set_filter_placeholder("Filter notes");
        let root = explorer
            .navigation_root()
            .expect("scoped Explorer always has a root")
            .to_path_buf();
        Ok(Self {
            root,
            explorer,
            drags: DragSurface::detect(),
            clicks: DoubleClickTracker::new(),
            status: None,
            create: None,
            tree_state: TreeState::default(),
            create_input: InputField::new("Note name")
                .with_theme(InputFieldTheme::for_color_scheme(theme.kit.scheme)),
            theme,
            theme_monitor: ThemeMonitor::from_theme(theme.kit),
        })
    }

    /// Runs the picker until the user chooses a file (`Some`) or quits (`None`).
    pub fn pick(
        &mut self,
        terminal: &mut DefaultTerminal,
        bridge: &mut UiBridge,
        revision_counter: &mut u64,
    ) -> io::Result<Option<PathBuf>> {
        self.rescan()?;
        let mut revision = revision_counter
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
        let mut published = self.ui_node();
        bridge
            .publish(UI_VIEW_ID, revision, published.clone())
            .map_err(ui_bridge_error)?;
        loop {
            if let Some(result) = self.drain_bridge(bridge, &mut revision, &mut published)? {
                *revision_counter = revision;
                return Ok(result);
            }
            self.publish_projection(bridge, &mut revision, &mut published)?;
            if bridge.should_render_terminal() {
                self.drags.begin_frame();
                terminal.draw(|frame| self.draw_node(frame, &published))?;
                self.drags.commit()?;
            }
            while !event::poll(Duration::from_millis(250))? {
                self.drags.heartbeat()?;
                if let Some(result) = self.drain_bridge(bridge, &mut revision, &mut published)? {
                    *revision_counter = revision;
                    return Ok(result);
                }
                if self.theme_monitor.refresh() {
                    self.theme = Theme::from_kit(self.theme_monitor.theme());
                    self.explorer
                        .set_theme(ExplorerTheme::for_theme(self.theme.kit));
                    self.create_input
                        .set_theme(InputFieldTheme::for_color_scheme(self.theme.kit.scheme));
                }
            }
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    let choice = if self.create.is_some() {
                        self.on_create_key(key)
                    } else {
                        self.on_key(key)
                    };
                    if let Some(choice) = choice
                        && let Some(result) = self.apply_choice(choice)?
                    {
                        self.publish_projection(bridge, &mut revision, &mut published)?;
                        *revision_counter = revision;
                        return Ok(result);
                    }
                }
                Event::Mouse(mouse) if self.create.is_none() => {
                    if let Some(choice) = self.on_mouse(mouse)
                        && let Some(result) = self.apply_choice(choice)?
                    {
                        self.publish_projection(bridge, &mut revision, &mut published)?;
                        *revision_counter = revision;
                        return Ok(result);
                    }
                }
                Event::Paste(text) => {
                    self.clicks.reset();
                    if let Some(create) = self.create.as_mut() {
                        create.insert(&text);
                    } else {
                        self.explorer.insert_filter_text(text);
                        self.status = None;
                    }
                }
                _ => {}
            }
            self.publish_projection(bridge, &mut revision, &mut published)?;
        }
    }

    fn rescan(&mut self) -> io::Result<()> {
        self.explorer.refresh()?;
        self.explorer.set_filter_focused(false);
        Ok(())
    }

    fn clear_drag_map(&mut self) -> io::Result<()> {
        self.drags.begin_frame();
        self.drags.commit()
    }

    fn apply_choice(&mut self, choice: Choice) -> io::Result<Option<Option<PathBuf>>> {
        match choice {
            Choice::Open(path) => Ok(Some(Some(path))),
            Choice::Create(folder) => {
                self.clear_drag_map()?;
                self.create = Some(CreateState::new(folder));
                self.create_input.set_focused(true);
                Ok(None)
            }
            Choice::Update => Ok(None),
            Choice::Quit => Ok(Some(None)),
        }
    }

    fn on_create_key(&mut self, key: KeyEvent) -> Option<Choice> {
        let create = self.create.as_mut()?;
        match key.code {
            KeyCode::Esc => {
                self.create = None;
                None
            }
            KeyCode::Enter => {
                let folder = create.folder.clone();
                let name = create.name.clone();
                match create_note(&folder, &name) {
                    Ok(path) => Some(Choice::Open(path)),
                    Err(message) => {
                        create.error = Some(message);
                        None
                    }
                }
            }
            KeyCode::Backspace => {
                create.name.pop();
                create.error = None;
                None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                create.name.clear();
                create.error = None;
                None
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                    && create.name.chars().count() < 120 =>
            {
                create.name.push(character);
                create.error = None;
                None
            }
            _ => None,
        }
    }

    fn ui_node(&mut self) -> UiNode {
        if let Some(create) = &self.create {
            let message = create.error.clone().unwrap_or_else(|| {
                note_stem(&create.name)
                    .map(|stem| format!("Creates {stem}.md"))
                    .unwrap_or_else(|_| "Creates a Markdown file in this folder".to_string())
            });
            let page = Page::new(
                "New note",
                List::new("new-note-details", Vec::new()).empty_message(message),
            )
            .input(
                Input::new(UI_NEW_NOTE_INPUT_ID, "Name")
                    .value(create.name.clone())
                    .placeholder("Note name")
                    .set_value_action(UI_NEW_NOTE_SET_VALUE)
                    .submit_action(UI_NEW_NOTE_SUBMIT),
            )
            .back_action(UI_NEW_NOTE_CANCEL);
            return UiNode::page(UI_TREE_ID, page);
        }
        let mut tree = self.explorer.semantic_tree("Notes").footer_actions([
            FooterAction::new(UI_NEW_NOTE_ID, "new", UI_NEW_NOTE_ACTION).accelerator("ctrl+n"),
            FooterAction::new(UI_PREVIOUS_ID, "previous", UI_PREVIOUS_ACTION).accelerator("ctrl+p"),
            FooterAction::new(UI_REFRESH_ID, "refresh", UI_REFRESH_ACTION).accelerator("ctrl+r"),
        ]);
        if let Some(status) = &self.status {
            tree.location = format!("{} · {status}", tree.location);
        }
        UiNode::tree(UI_TREE_ID, tree)
    }

    fn publish_projection(
        &mut self,
        bridge: &mut UiBridge,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<()> {
        let next = self.ui_node();
        if next == *published {
            return Ok(());
        }
        let operations = tree_delta_operations(published, &next);
        let next_revision = revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
        bridge
            .publish_delta(UI_VIEW_ID, *revision, next_revision, operations)
            .map_err(ui_bridge_error)?;
        *revision = next_revision;
        *published = next;
        Ok(())
    }

    fn drain_bridge(
        &mut self,
        bridge: &mut UiBridge,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<Option<Option<PathBuf>>> {
        while let Some(message) = bridge.poll().map_err(ui_bridge_error)? {
            let UiBridgeEvent::Action { event, .. } = message else {
                continue;
            };
            let decision = self.handle_ui_event(*revision, &event);
            let (outcome, result) = match decision {
                Ok(Some(choice)) => match self.apply_choice(choice) {
                    Ok(result) => (UiEventOutcome::Applied, result),
                    Err(error) => (UiEventOutcome::Rejected(error.to_string()), None),
                },
                Ok(None) => (
                    UiEventOutcome::Rejected(
                        "Action targets a different picker component".to_string(),
                    ),
                    None,
                ),
                Err(message) => (UiEventOutcome::Rejected(message), None),
            };
            self.publish_projection(bridge, revision, published)?;
            bridge
                .acknowledge(&event, outcome, *revision)
                .map_err(ui_bridge_error)?;
            if result.is_some() {
                return Ok(result);
            }
        }
        Ok(None)
    }

    fn handle_ui_event(
        &mut self,
        revision: u64,
        event: &UiEvent,
    ) -> Result<Option<Choice>, String> {
        if event.base_revision != revision {
            return Err(format!(
                "Picker changed from revision {} to {revision}; retry the action",
                event.base_revision
            ));
        }
        if let Some(create) = self.create.as_mut() {
            match (
                event.action.node_id.as_str(),
                event.action.action.as_str(),
                event.action.kind,
                &event.action.value,
            ) {
                (
                    UI_NEW_NOTE_INPUT_ID,
                    UI_NEW_NOTE_SET_VALUE,
                    UiEventKind::Change,
                    UiEventValue::Text(value),
                ) => {
                    create.set(value);
                    Ok(Some(Choice::Update))
                }
                (
                    UI_NEW_NOTE_INPUT_ID,
                    UI_NEW_NOTE_SUBMIT,
                    UiEventKind::Submit,
                    UiEventValue::Text(value),
                ) => {
                    create.set(value);
                    match create_note(&create.folder, &create.name) {
                        Ok(path) => Ok(Some(Choice::Open(path))),
                        Err(message) => {
                            create.error = Some(message.clone());
                            Err(message)
                        }
                    }
                }
                (UI_TREE_ID, UI_NEW_NOTE_CANCEL, UiEventKind::Cancel, UiEventValue::None) => {
                    self.create = None;
                    Ok(Some(Choice::Update))
                }
                _ => Ok(None),
            }
        } else if event.action.kind == UiEventKind::Activate
            && event.action.value == UiEventValue::None
        {
            match (event.action.node_id.as_str(), event.action.action.as_str()) {
                (UI_NEW_NOTE_ID, UI_NEW_NOTE_ACTION) => {
                    Ok(Some(Choice::Create(self.explorer.cwd().to_path_buf())))
                }
                (UI_PREVIOUS_ID, UI_PREVIOUS_ACTION) => {
                    let choice = self.apply_explorer(ExplorerInput::Up);
                    Ok(Some(choice.unwrap_or(Choice::Update)))
                }
                (UI_REFRESH_ID, UI_REFRESH_ACTION) => {
                    let choice = self.apply_explorer(ExplorerInput::Refresh);
                    Ok(Some(choice.unwrap_or(Choice::Update)))
                }
                _ => self
                    .explorer
                    .handle_ui_event(revision, UI_TREE_ID, event)
                    .map(|event| match event {
                        Some(ExplorerEvent::FileActivated(path)) => Some(Choice::Open(path)),
                        Some(_) => Some(Choice::Update),
                        None => None,
                    }),
            }
        } else {
            self.explorer
                .handle_ui_event(revision, UI_TREE_ID, event)
                .map(|event| match event {
                    Some(ExplorerEvent::FileActivated(path)) => Some(Choice::Open(path)),
                    Some(_) => Some(Choice::Update),
                    None => None,
                })
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Choice> {
        self.clicks.reset();
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('c') && control {
            return Some(Choice::Quit);
        }
        let footer_action = self.ui_node().footer_action_for_key(&key).cloned();
        if let Some(action) = footer_action {
            return match (action.id.as_str(), action.action.as_str()) {
                (UI_NEW_NOTE_ID, UI_NEW_NOTE_ACTION) => {
                    Some(Choice::Create(self.explorer.cwd().to_path_buf()))
                }
                (UI_PREVIOUS_ID, UI_PREVIOUS_ACTION) => self
                    .apply_explorer(ExplorerInput::Up)
                    .or(Some(Choice::Update)),
                (UI_REFRESH_ID, UI_REFRESH_ACTION) => self
                    .apply_explorer(ExplorerInput::Refresh)
                    .or(Some(Choice::Update)),
                _ => None,
            };
        }
        if key.code == KeyCode::Esc {
            if self.explorer.cwd() == self.root {
                return Some(Choice::Quit);
            }
            let input = self.explorer.input_for_key(&key)?;
            return self.apply_explorer(input);
        }
        let input = self.explorer.input_for_key(&key)?;
        self.apply_explorer(input)
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> Option<Choice> {
        self.tree_state.track_mouse(&mouse);
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.clicks.reset();
                self.apply_explorer(ExplorerInput::Up)
            }
            MouseEventKind::ScrollDown => {
                self.clicks.reset();
                self.apply_explorer(ExplorerInput::Down)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let footer_action = match &self.ui_node().element {
                    UiComponent::Tree(tree) => {
                        self.tree_state.footer_action_at(tree, position).cloned()
                    }
                    _ => None,
                };
                if let Some(action) = footer_action {
                    self.clicks.reset();
                    return match (action.id.as_str(), action.action.as_str()) {
                        (UI_NEW_NOTE_ID, UI_NEW_NOTE_ACTION) => {
                            Some(Choice::Create(self.explorer.cwd().to_path_buf()))
                        }
                        (UI_PREVIOUS_ID, UI_PREVIOUS_ACTION) => self
                            .apply_explorer(ExplorerInput::Up)
                            .or(Some(Choice::Update)),
                        (UI_REFRESH_ID, UI_REFRESH_ACTION) => self
                            .apply_explorer(ExplorerInput::Refresh)
                            .or(Some(Choice::Update)),
                        _ => None,
                    };
                } else if self.explorer.filter_area().contains(position) {
                    self.clicks.reset();
                    self.explorer
                        .filter_mouse_down(position, mouse.modifiers.contains(KeyModifiers::SHIFT));
                    self.status = None;
                } else if let Some((target, path)) = self
                    .tree_state
                    .item_id_at(position)
                    .map(str::to_owned)
                    .and_then(|target| {
                        self.explorer
                            .path_for_semantic_item(&target)
                            .map(|path| (target, path.to_path_buf()))
                    })
                {
                    self.explorer.set_filter_focused(false);
                    let _ = self.explorer.select_semantic_item(&target);
                    if self.clicks.click(path) {
                        return self.apply_explorer(ExplorerInput::Open);
                    }
                } else {
                    self.clicks.reset();
                }
                None
            }
            MouseEventKind::Drag(MouseButton::Left) if self.explorer.filter_dragging() => {
                self.explorer.filter_mouse_drag(position);
                None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.explorer.filter_mouse_up();
                None
            }
            _ => None,
        }
    }

    fn apply_explorer(&mut self, input: ExplorerInput) -> Option<Choice> {
        match self.explorer.handle(input) {
            Ok(ExplorerEvent::FileActivated(path)) => Some(Choice::Open(path)),
            Ok(ExplorerEvent::DirectoryChanged(_)) => {
                self.explorer.set_filter_focused(false);
                self.status = None;
                None
            }
            Ok(ExplorerEvent::FilterChanged | ExplorerEvent::Refreshed) => {
                self.status = None;
                None
            }
            Ok(_) => None,
            Err(error) => {
                self.status = Some(error.to_string());
                None
            }
        }
    }

    fn draw_node(&mut self, frame: &mut Frame, node: &UiNode) {
        match &node.element {
            UiComponent::Tree(tree) => {
                frame.render_widget(
                    tree.widget_with_filter(&mut self.tree_state, self.explorer.filter_input_mut())
                        .theme(TreeTheme::for_theme(self.theme.kit)),
                    frame.area(),
                );
                let rows = self.tree_state.rows_area();
                for row in 0..rows.height {
                    let position = Position::new(rows.x, rows.y.saturating_add(row));
                    if let Some(path) = self
                        .tree_state
                        .item_id_at(position)
                        .and_then(|id| self.explorer.path_for_semantic_item(id))
                    {
                        self.drags
                            .register(Rect::new(rows.x, position.y, rows.width, 1), path);
                    }
                }
                if let Some(position) = self.explorer.filter_cursor_position() {
                    frame.set_cursor_position(position);
                }
            }
            UiComponent::Page(page) => {
                let mut state = ListState::new(None);
                frame.render_widget(
                    page.widget(&mut self.create_input, &mut state)
                        .theme(PageTheme::for_theme(self.theme.kit)),
                    frame.area(),
                );
                if let Some(position) = self.create_input.cursor_position() {
                    frame.set_cursor_position(position);
                }
            }
            _ => {}
        }
    }

    #[cfg(test)]
    fn draw(&mut self, frame: &mut Frame) {
        let node = self.ui_node();
        self.draw_node(frame, &node);
    }
}

enum Choice {
    Open(PathBuf),
    Create(PathBuf),
    Update,
    Quit,
}

struct CreateState {
    folder: PathBuf,
    name: String,
    error: Option<String>,
}

impl CreateState {
    fn new(folder: PathBuf) -> Self {
        Self {
            folder,
            name: String::new(),
            error: None,
        }
    }

    fn set(&mut self, value: &str) {
        self.name = value
            .chars()
            .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
            .take(120)
            .collect();
        self.error = None;
    }

    fn insert(&mut self, value: &str) {
        let remaining = 120usize.saturating_sub(self.name.chars().count());
        self.name.extend(
            value
                .chars()
                .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
                .take(remaining),
        );
        self.error = None;
    }
}

/// The first `#` heading in a note, read from a bounded prefix so large
/// vaults stay cheap to list. Shown muted beside the file name.
fn first_heading(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut prefix = vec![0u8; 8 * 1024];
    let read = file.read(&mut prefix).ok()?;
    prefix.truncate(read);
    let text = String::from_utf8_lossy(&prefix);
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("# "))
        .map(|heading| heading.trim().to_owned())
        .filter(|heading| !heading.is_empty())
}

fn note_stem(name: &str) -> Result<String, String> {
    let name = name.trim().trim_end_matches(".md").trim();
    if name.is_empty() {
        return Err("enter a note name".to_string());
    }
    let mut stem = String::new();
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            stem.push(character);
        } else if (character.is_whitespace() || matches!(character, '-' | '_'))
            && !stem.is_empty()
            && !stem.ends_with('-')
        {
            stem.push('-');
        }
    }
    let stem = stem.trim_end_matches('-').to_string();
    if stem.is_empty() {
        Err("use at least one letter or number".to_string())
    } else {
        Ok(stem)
    }
}

fn create_note(root: &Path, name: &str) -> Result<PathBuf, String> {
    let title = name.trim().trim_end_matches(".md").trim();
    let path = root.join(format!("{}.md", note_stem(name)?));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                "a note with that name already exists".to_string()
            } else {
                error.to_string()
            }
        })?;
    if let Err(error) = writeln!(file, "# {title}\n") {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error.to_string());
    }
    Ok(path)
}

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_notes_are_named_safely_and_never_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let path = create_note(root.path(), "Project Brief").expect("create note");
        assert_eq!(path, root.path().join("project-brief.md"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Project Brief\n\n"
        );
        assert!(create_note(root.path(), "Project Brief.md").is_err());
        assert_eq!(note_stem("../../Secrets"), Ok("secrets".to_string()));
        assert!(note_stem("---").is_err());
    }

    #[test]
    fn empty_vault_still_exposes_the_new_note_command() {
        let root = tempfile::tempdir().unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        assert!(picker.explorer.entries().is_empty());
        assert!(matches!(
            picker.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Choice::Create(folder)) if folder == picker.root
        ));
    }

    #[test]
    fn picker_status_is_part_of_the_semantic_tree_location() {
        let root = tempfile::tempdir().unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        picker.status = Some("could not refresh".to_string());
        let node = picker.ui_node();
        let unpeel_app_kit::UiComponent::Tree(tree) = node.element else {
            panic!("picker must publish Tree");
        };
        assert_eq!(tree.label, "Notes");
        let root_name = picker
            .root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(tree.location, format!("{root_name} · could not refresh"));
    }

    #[test]
    fn new_note_command_targets_the_current_nested_folder() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let nested = std::fs::canonicalize(nested).unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        picker.explorer.select_path(&nested);
        picker.apply_explorer(ExplorerInput::Open);

        assert!(matches!(
            picker.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Choice::Create(folder)) if folder == nested
        ));
    }

    #[test]
    fn notes_show_their_first_heading_as_muted_detail() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("demo.md"),
            "---\ntitle: x\n---\n\n# Demo heading\n",
        )
        .unwrap();
        std::fs::write(root.path().join("plain.md"), "no heading here\n").unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        let details = picker
            .explorer
            .entries()
            .iter()
            .map(|entry| (entry.display_name(), entry.detail().map(str::to_owned)))
            .collect::<Vec<_>>();
        assert_eq!(
            details,
            [
                ("demo.md".to_owned(), Some("Demo heading".to_owned())),
                ("plain.md".to_owned(), None),
            ]
        );
        let UiComponent::Tree(tree) = picker.ui_node().element else {
            panic!("picker publishes a Tree");
        };
        assert_eq!(tree.items[0].detail.as_deref(), Some("Demo heading"));
    }

    #[test]
    fn picker_is_scoped_and_only_lists_folders_and_markdown_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("notes")).unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        std::fs::write(root.path().join("README.MD"), "# Readme\n").unwrap();
        std::fs::write(root.path().join("ignore.txt"), "ignore\n").unwrap();

        let picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();
        let labels = picker
            .explorer
            .entries()
            .iter()
            .map(|entry| entry.display_name())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["notes/", "demo.md", "README.MD"]);
        assert_eq!(
            picker.explorer.navigation_root(),
            Some(picker.root.as_path())
        );
        assert!(
            picker
                .explorer
                .entries()
                .iter()
                .all(|entry| !entry.is_parent())
        );
    }

    #[test]
    fn typing_uses_the_shared_filter_and_escape_is_back() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("README.md"), "# Readme\n").unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();

        assert!(!picker.explorer.filter_focused());
        for ch in "demo".chars() {
            assert!(
                picker
                    .on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                    .is_none()
            );
        }
        assert_eq!(picker.explorer.filter(), "demo");
        assert_eq!(picker.explorer.entries().len(), 1);
        assert_eq!(picker.explorer.entries()[0].display_name(), "demo.md");
        assert!(picker.explorer.filter_focused());

        picker.explorer.clear_filter();
        let nested = std::fs::canonicalize(root.path().join("nested")).unwrap();
        picker.explorer.select_path(&nested);
        picker.apply_explorer(ExplorerInput::Open);
        assert_eq!(picker.explorer.cwd(), nested);
        assert!(
            picker
                .on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .is_none()
        );
        assert_eq!(picker.explorer.cwd(), picker.root);
        assert!(matches!(
            picker.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Choice::Quit)
        ));
    }

    #[test]
    fn arrow_focus_can_move_between_the_first_row_and_filter() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        let mut picker = Picker::open(root.path().to_path_buf(), Theme::dark()).unwrap();

        assert_eq!(picker.explorer.selected_index(), 0);
        assert!(!picker.explorer.filter_focused());
        assert!(
            picker
                .on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
                .is_none()
        );
        assert!(picker.explorer.filter_focused());
        assert!(
            picker
                .on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
                .is_none()
        );
        assert!(!picker.explorer.filter_focused());
    }

    #[test]
    fn startup_picker_renders_the_exact_published_tree() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("demo.md"), "# Demo\n").unwrap();
        let theme = Theme::dark();
        let mut picker = Picker::open(root.path().to_path_buf(), theme).unwrap();
        let width = 40;
        let mut terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();

        let buffer = terminal.backend().buffer();
        let row_text = |row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        };
        assert!(row_text(0).starts_with("  Filter: Filter notes"));
        assert!(row_text(1).starts_with("  ."));
        assert!(row_text(2).trim().is_empty(), "padding row under the title");
        assert!(row_text(3).starts_with("  demo.md"));
        assert_eq!(picker.tree_state.rows_area(), Rect::new(0, 3, width, 2));
        let selected_background = theme
            .kit
            .selected_row
            .bg
            .expect("App Kit selection has a background");
        assert!((0..width).all(|column| buffer[(column, 3)].bg == selected_background));
        assert_ne!(buffer[(0, 0)].symbol(), "┌");
        assert_ne!(buffer[(width - 1, 4)].symbol(), "┘");
        let action = row_text(5);
        assert!(action.contains("^N new"));
        assert!(action.contains("^P previous"));
        assert!(action.contains("^R refresh"));

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        assert!(picker.on_mouse(click).is_none(), "one click only selects");
        assert!(
            matches!(picker.on_mouse(click), Some(Choice::Open(_))),
            "double click opens"
        );

        let create_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: picker.tree_state.footer_area().x + 2,
            row: picker.tree_state.footer_area().y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(
            picker.on_mouse(create_click),
            Some(Choice::Create(folder)) if folder == picker.root
        ));
    }
}
