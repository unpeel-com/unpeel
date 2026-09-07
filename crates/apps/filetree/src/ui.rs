use std::io::{self, Stdout, Write};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::{Frame, Terminal};
use unpeel_app_kit::{
    OpenOutcome, open_resource,
    AgentBridge, AppContext, AppMetadata, AppReporter, DoubleClickTracker, DragSurface,
    EditorBridge, Explorer, ExplorerEvent, ExplorerInput, ExplorerTheme, FooterAction,
    KeyboardEnhancementGuard, KitTheme, MenuTheme, PopupMenu, SemanticMenu, SemanticMenuAnchor,
    SemanticMenuItem, SemanticMenuPresentation, ThemeMonitor, TreeState, TreeTheme, UiAction,
    UiBridge, UiBridgeEvent, UiComponent, UiEventKind, UiEventOutcome, UiEventValue, UiNode,
    clipboard_sequence, tree_delta_operations,
};

const UI_VIEW_ID: &str = "main";
const UI_TREE_ID: &str = "file-tree";
const OPEN_ACTION: &str = "open";
const OPEN_IN_EDITOR_ACTION: &str = "open-in-editor";
const SEND_TO_AGENT_ACTION: &str = "send-to-agent";
const COPY_PATH_ACTION: &str = "copy-path";
const REFRESH_TREE_ACTION: &str = "refresh-tree";
const TOGGLE_HIDDEN_ACTION: &str = "toggle-hidden";

pub fn run(
    mut explorer: Explorer,
    follow_agent_context: bool,
    mut app_context: AppContext,
) -> io::Result<()> {
    let mut theme_monitor = ThemeMonitor::detected();
    let mut theme = theme_monitor.theme();
    explorer.set_theme(explorer_theme(theme));
    explorer.set_show_path(false);
    let mut drags = DragSurface::detect();
    let mut terminal = TerminalGuard::enter()?;
    let _keyboard = KeyboardEnhancementGuard::enter()?;
    let mut reporter = AppReporter::detect(crate::install::APP_ID);
    let agent = AgentBridge::new();
    agent.refresh();
    let mut last_agent_context_refresh = Instant::now();
    let mut menu = None;
    let mut clicks = DoubleClickTracker::new();
    let mut status = None;
    let mut needs_draw = true;
    let mut tree_state = TreeState::default();
    let mut bridge = UiBridge::detect(
        AppMetadata::new(
            crate::install::APP_ID,
            "Unpeel File Tree",
            env!("CARGO_PKG_VERSION"),
        )
        .description("One Explorer Tree interpreted by Ratatui, native, and web renderers"),
    )
    .map_err(ui_bridge_error)?;
    let mut ui_revision = 1u64;
    let mut published = semantic_node(&mut explorer, agent.label().is_some(), status.as_ref());
    bridge
        .publish(UI_VIEW_ID, ui_revision, published.clone())
        .map_err(ui_bridge_error)?;
    // The session title follows the folder being browsed (the Host folds it
    // into the sidebar row until the user renames it). Written only when it
    // changes: the reporter persists a marker file per call.
    let mut session_title = String::new();

    loop {
        drain_bridge(
            &mut explorer,
            &agent,
            &mut bridge,
            &mut ui_revision,
            &mut published,
            &mut status,
            &mut needs_draw,
        )?;
        publish_projection(
            &mut explorer,
            agent.label().is_some(),
            status.as_ref(),
            &mut bridge,
            &mut ui_revision,
            &mut published,
        )?;
        if needs_draw {
            let title = folder_title(explorer.cwd(), explorer.navigation_root());
            if title != session_title {
                reporter.set_title(&title);
                session_title = title;
            }
            let selected = explorer.selected();
            reporter.set_context(&serde_json::json!({
                "cwd": explorer.cwd(),
                "selected_path": selected.map(|entry| entry.path()),
                "selected_kind": selected.map(|entry| if entry.is_directory() {
                    "directory"
                } else {
                    "file"
                }),
            }));
            if bridge.should_render_terminal() {
                terminal.draw(
                    &published,
                    &mut explorer,
                    &mut drags,
                    &mut tree_state,
                    menu.as_mut(),
                    theme,
                )?;
            }
            needs_draw = false;
        }
        if !event::poll(Duration::from_millis(250))? {
            drags.heartbeat()?;
            if theme_monitor.refresh() {
                theme = theme_monitor.theme();
                explorer.set_theme(explorer_theme(theme));
                needs_draw = true;
            }
            if follow_agent_context
                && last_agent_context_refresh.elapsed() >= Duration::from_secs(1)
            {
                last_agent_context_refresh = Instant::now();
                let app_context_changed = app_context.refresh();
                let next_root = agent
                    .project_context()
                    .filter(|context| context.cwd.is_dir())
                    .map(|context| context.cwd)
                    .or_else(|| {
                        app_context_changed
                            .then(|| app_context.current_root().map(PathBuf::from))
                            .flatten()
                    });
                if let Some(root) = next_root
                    && root.is_dir()
                    && explorer.set_navigation_root(&root).unwrap_or(false)
                {
                    status = None;
                    needs_draw = true;
                }
                agent.refresh();
            }
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                clicks.reset();
                if is_force_quit(key) {
                    break;
                }
                if menu.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            menu = None;
                            needs_draw = true;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            menu.as_mut().expect("menu is open").move_selection(-1);
                            needs_draw = true;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            menu.as_mut().expect("menu is open").move_selection(1);
                            needs_draw = true;
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            let open_menu = menu.take().expect("menu is open");
                            status = Some(activate_menu(open_menu, &agent));
                            needs_draw = true;
                        }
                        _ => {}
                    }
                    continue;
                }
                if let Some(action) = published.footer_action_for_key(&key).cloned() {
                    status = apply_footer_action(&mut explorer, &action);
                    needs_draw = true;
                    continue;
                }
                let Some(input) = explorer.input_for_key(&key) else {
                    continue;
                };
                status = match explorer.handle(input) {
                    Ok(event) => status_for_event(event, &explorer),
                    Err(error) => Some(Status::error(error.to_string())),
                };
                needs_draw = true;
            }
            Event::Mouse(mouse) => {
                needs_draw |= tree_state.track_mouse(&mouse);
                let position = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Right) => {
                        clicks.reset();
                        let target = tree_state.item_id_at(position).map(str::to_owned);
                        if let Some((target, path)) = target.and_then(|target| {
                            explorer
                                .path_for_semantic_item(&target)
                                .map(|path| (target, path.to_path_buf()))
                        }) {
                            explorer.set_filter_focused(false);
                            let _ = explorer.select_semantic_item(&target);
                            agent.refresh();
                            menu = match &published.element {
                                UiComponent::Tree(tree) => tree.context_menu.as_ref(),
                                _ => None,
                            }
                            .map(|spec| context_menu(path, spec, position, theme.scheme));
                            needs_draw = true;
                        } else if menu.take().is_some() {
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(mut open_menu) = menu.take() {
                            clicks.reset();
                            if open_menu.action_index_for_mouse(&mouse).is_some() {
                                status = Some(activate_menu(open_menu, &agent));
                            }
                            needs_draw = true;
                        } else if let Some(action) = match &published.element {
                            UiComponent::Tree(tree) => {
                                tree_state.footer_action_at(tree, position).cloned()
                            }
                            _ => None,
                        } {
                            clicks.reset();
                            status = apply_footer_action(&mut explorer, &action);
                            needs_draw = true;
                        } else if explorer.filter_area().contains(position) {
                            clicks.reset();
                            explorer.filter_mouse_down(
                                position,
                                mouse.modifiers.contains(KeyModifiers::SHIFT),
                            );
                            status = None;
                            needs_draw = true;
                        } else if let Some(activate) =
                            explorer_click_at(&mut explorer, &tree_state, position, &mut clicks)
                        {
                            if activate {
                                status = handle_explorer(&mut explorer, ExplorerInput::Open);
                            }
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if explorer.filter_dragging() {
                            explorer.filter_mouse_drag(position);
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if explorer.filter_mouse_up() {
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        clicks.reset();
                        if let Some(open_menu) = menu.as_mut() {
                            open_menu.move_selection(-1);
                            needs_draw = true;
                        } else {
                            status = handle_explorer(&mut explorer, ExplorerInput::Up);
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        clicks.reset();
                        if let Some(open_menu) = menu.as_mut() {
                            open_menu.move_selection(1);
                            needs_draw = true;
                        } else {
                            status = handle_explorer(&mut explorer, ExplorerInput::Down);
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Moved => {
                        if menu
                            .as_mut()
                            .is_some_and(|open_menu| open_menu.hover_at(position))
                        {
                            needs_draw = true;
                        }
                    }
                    _ => {}
                }
            }
            Event::Paste(text) if menu.is_none() => {
                clicks.reset();
                status = status_for_event(explorer.insert_filter_text(text), &explorer);
                needs_draw = true;
            }
            Event::Resize(_, _) => {
                clicks.reset();
                needs_draw = true;
            }
            _ => {}
        }
    }
    Ok(())
}

fn publish_projection(
    explorer: &mut Explorer,
    can_send: bool,
    status: Option<&Status>,
    bridge: &mut UiBridge,
    revision: &mut u64,
    published: &mut UiNode,
) -> io::Result<()> {
    let next = semantic_node(explorer, can_send, status);
    if next == *published {
        return Ok(());
    }
    let next_revision = revision
        .checked_add(1)
        .ok_or_else(|| io::Error::other("File Tree UI revision space is exhausted"))?;
    bridge
        .publish_delta(
            UI_VIEW_ID,
            *revision,
            next_revision,
            tree_delta_operations(published, &next),
        )
        .map_err(ui_bridge_error)?;
    *revision = next_revision;
    *published = next;
    Ok(())
}

fn semantic_node(explorer: &mut Explorer, can_send: bool, status: Option<&Status>) -> UiNode {
    let mut tree = explorer
        .semantic_tree("Files")
        .context_menu(semantic_context_menu(can_send))
        .footer_actions([
            FooterAction::new("refresh-files", "refresh", REFRESH_TREE_ACTION)
                .accelerator("ctrl+r"),
            FooterAction::new(
                "toggle-hidden-files",
                if explorer.show_hidden() {
                    "hide hidden"
                } else {
                    "show hidden"
                },
                TOGGLE_HIDDEN_ACTION,
            )
            .accelerator("ctrl+h"),
        ]);
    if let Some(status) = status {
        tree.location = format!(
            "{} · {}{}",
            tree.location,
            if status.error { "Error: " } else { "" },
            status.message
        );
    }
    UiNode::tree(UI_TREE_ID, tree)
}

fn semantic_context_menu(can_send: bool) -> SemanticMenu {
    let mut items = vec![
        // "Open" follows the workspace's opener policy (an App beside this
        // pane, the editor, or the system); "Open in editor" is the explicit
        // override.
        SemanticMenuItem::new("open", "Open", OPEN_ACTION),
        SemanticMenuItem::new("open-in-editor", "Open in editor", OPEN_IN_EDITOR_ACTION),
    ];
    if can_send {
        items.push(SemanticMenuItem::new(
            "send-to-agent",
            "Send to agent",
            SEND_TO_AGENT_ACTION,
        ));
    }
    items.push(SemanticMenuItem::new(
        "copy-path",
        "Copy path",
        COPY_PATH_ACTION,
    ));
    SemanticMenu::new("File actions", items)
        .presentation(SemanticMenuPresentation::Context)
        .anchor(SemanticMenuAnchor::Pointer)
}

fn drain_bridge(
    explorer: &mut Explorer,
    agent: &AgentBridge,
    bridge: &mut UiBridge,
    revision: &mut u64,
    published: &mut UiNode,
    status: &mut Option<Status>,
    needs_draw: &mut bool,
) -> io::Result<()> {
    while let Some(message) = bridge.poll().map_err(ui_bridge_error)? {
        let event = match message {
            UiBridgeEvent::Action { event, .. } => event,
            UiBridgeEvent::Attached { .. }
            | UiBridgeEvent::Detached { .. }
            | UiBridgeEvent::Lifecycle { .. } => {
                // A detached native/web renderer can make the PTY visible
                // again, so repaint even when the Explorer model is unchanged.
                *needs_draw = true;
                continue;
            }
        };
        let footer_action = matches!(
            event.action.action.as_str(),
            REFRESH_TREE_ACTION | TOGGLE_HIDDEN_ACTION
        );
        let semantic_menu_action = matches!(
            event.action.action.as_str(),
            OPEN_IN_EDITOR_ACTION | SEND_TO_AGENT_ACTION | COPY_PATH_ACTION
        );
        let outcome = if footer_action {
            if event.base_revision != *revision {
                UiEventOutcome::Rejected(format!(
                    "File Tree changed from revision {} to {}; retry the action",
                    event.base_revision, revision
                ))
            } else {
                match apply_footer_ui_action(explorer, &event.action) {
                    Ok(event) => {
                        *status = status_for_event(event, explorer);
                        *needs_draw = true;
                        UiEventOutcome::Applied
                    }
                    Err(message) => UiEventOutcome::Rejected(message),
                }
            }
        } else if semantic_menu_action {
            if event.base_revision != *revision {
                UiEventOutcome::Rejected(format!(
                    "File Tree changed from revision {} to {}; retry the action",
                    event.base_revision, revision
                ))
            } else {
                match semantic_context_action(explorer, agent.label().is_some(), &event.action) {
                    Ok(action) => {
                        *status = Some(activate_context_action(action, agent));
                        *needs_draw = true;
                        UiEventOutcome::Applied
                    }
                    Err(message) => UiEventOutcome::Rejected(message),
                }
            }
        } else {
            match explorer.handle_ui_event(*revision, UI_TREE_ID, &event) {
                Ok(Some(explorer_event)) => {
                    *status = status_for_event(explorer_event, explorer);
                    *needs_draw = true;
                    UiEventOutcome::Applied
                }
                Ok(None) => UiEventOutcome::Rejected(
                    "Action targets a different File Tree component".to_string(),
                ),
                Err(message) => UiEventOutcome::Rejected(message),
            }
        };
        publish_projection(
            explorer,
            agent.label().is_some(),
            status.as_ref(),
            bridge,
            revision,
            published,
        )?;
        bridge
            .acknowledge(&event, outcome, *revision)
            .map_err(ui_bridge_error)?;
    }
    Ok(())
}

fn apply_footer_action(explorer: &mut Explorer, action: &FooterAction) -> Option<Status> {
    let action = UiAction::new(
        action.id.clone(),
        action.action.clone(),
        UiEventKind::Activate,
        UiEventValue::None,
    );
    match apply_footer_ui_action(explorer, &action) {
        Ok(event) => status_for_event(event, explorer),
        Err(message) => Some(Status::error(message)),
    }
}

fn apply_footer_ui_action(
    explorer: &mut Explorer,
    action: &UiAction,
) -> Result<ExplorerEvent, String> {
    let input = match (
        action.node_id.as_str(),
        action.action.as_str(),
        action.kind,
        &action.value,
    ) {
        ("refresh-files", REFRESH_TREE_ACTION, UiEventKind::Activate, UiEventValue::None) => {
            ExplorerInput::Refresh
        }
        (
            "toggle-hidden-files",
            TOGGLE_HIDDEN_ACTION,
            UiEventKind::Activate,
            UiEventValue::None,
        ) => ExplorerInput::ToggleHidden,
        _ => return Err("Action is not declared by the current File Tree footer".to_owned()),
    };
    explorer.handle(input).map_err(|error| error.to_string())
}

fn semantic_context_action(
    explorer: &mut Explorer,
    can_send: bool,
    action: &unpeel_app_kit::UiAction,
) -> Result<ContextAction, String> {
    if action.kind != UiEventKind::Activate {
        return Err("File Tree menu actions must activate".to_owned());
    }
    let UiEventValue::Text(target) = &action.value else {
        return Err("File Tree menu actions require an opaque Tree target".to_owned());
    };
    let build: fn(PathBuf) -> ContextAction =
        match (action.node_id.as_str(), action.action.as_str()) {
            ("open", OPEN_ACTION) => ContextAction::Open,
            ("open-in-editor", OPEN_IN_EDITOR_ACTION) => ContextAction::OpenInEditor,
            ("send-to-agent", SEND_TO_AGENT_ACTION) if can_send => ContextAction::SendToAgent,
            ("copy-path", COPY_PATH_ACTION) => ContextAction::CopyPath,
            _ => return Err("Action is not declared by the current File Tree menu".to_owned()),
        };
    explorer.select_semantic_item(target)?;
    let path = explorer
        .selected()
        .map(|entry| entry.path().to_path_buf())
        .ok_or_else(|| "File Tree target is no longer present".to_owned())?;
    Ok(build(path))
}

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error)
}

fn explorer_click_at(
    explorer: &mut Explorer,
    tree_state: &TreeState,
    position: Position,
    clicks: &mut DoubleClickTracker<PathBuf>,
) -> Option<bool> {
    let Some(target) = tree_state.item_id_at(position).map(str::to_owned) else {
        clicks.reset();
        return None;
    };
    let Some(path) = explorer
        .path_for_semantic_item(&target)
        .map(std::path::Path::to_path_buf)
    else {
        clicks.reset();
        return None;
    };
    explorer.set_filter_focused(false);
    let activate = clicks.click(path);
    let _ = explorer.select_semantic_item(&target);
    Some(activate)
}

fn explorer_theme(theme: KitTheme) -> ExplorerTheme {
    ExplorerTheme::for_theme(theme)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ContextAction {
    Open(PathBuf),
    OpenInEditor(PathBuf),
    SendToAgent(PathBuf),
    CopyPath(PathBuf),
}

#[derive(Debug)]
struct ContextMenu {
    popup: PopupMenu<String>,
    path: PathBuf,
}

impl Deref for ContextMenu {
    type Target = PopupMenu<String>;

    fn deref(&self) -> &Self::Target {
        &self.popup
    }
}

impl DerefMut for ContextMenu {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.popup
    }
}

fn context_menu(
    path: PathBuf,
    spec: &SemanticMenu,
    anchor: Position,
    scheme: unpeel_app_kit::ColorScheme,
) -> ContextMenu {
    ContextMenu {
        popup: spec.popup(anchor, MenuTheme::for_color_scheme(scheme)),
        path,
    }
}

fn is_force_quit(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn activate_menu(menu: ContextMenu, agent: &AgentBridge) -> Status {
    let ContextMenu { popup, path } = menu;
    let Some(item_id) = popup.selected_value().map(String::as_str) else {
        return Status::error("No menu action selected");
    };
    let action = match item_id {
        "open" => ContextAction::Open(path),
        "open-in-editor" => ContextAction::OpenInEditor(path),
        "send-to-agent" => ContextAction::SendToAgent(path),
        "copy-path" => ContextAction::CopyPath(path),
        _ => return Status::error("Unknown menu action"),
    };
    activate_context_action(action, agent)
}

fn activate_context_action(action: ContextAction, agent: &AgentBridge) -> Status {
    match action {
        ContextAction::Open(path) => open_status(&path),
        ContextAction::OpenInEditor(path) => match EditorBridge::open(&path) {
            Ok(()) => Status::message("Opened in editor"),
            Err(error) => Status::error(format!("Open failed: {error}")),
        },
        ContextAction::SendToAgent(path) => match agent.send_path(&path) {
            Ok(label) => Status::message(format!("Sent path to {label}")),
            Err(error) => match copy_path(&path) {
                Ok(()) => Status::error(format!("{error}; path copied instead")),
                Err(copy_error) => Status::error(format!("{error}; copy failed: {copy_error}")),
            },
        },
        ContextAction::CopyPath(path) => match copy_path(&path) {
            Ok(()) => Status::message("Path copied"),
            Err(error) => Status::error(format!("Copy failed: {error}")),
        },
    }
}

fn copy_path(path: &std::path::Path) -> io::Result<()> {
    let sequence = clipboard_sequence(path.to_string_lossy().as_ref());
    let mut stdout = io::stdout();
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()
}

#[derive(Debug)]
struct Status {
    message: String,
    error: bool,
}

impl Status {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: false,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            error: true,
        }
    }
}

/// Open a file the way the workspace policy says and describe the outcome.
fn open_status(path: &std::path::Path) -> Status {
    match open_resource(path) {
        Ok(OpenOutcome::App(name)) => Status::message(format!("Opened in {name}")),
        Ok(OpenOutcome::Editor) => Status::message("Opened in editor"),
        Ok(OpenOutcome::System) => Status::message("Opened with the system"),
        Err(error) => Status::error(format!("Open failed: {error}")),
    }
}

fn status_for_event(event: ExplorerEvent, explorer: &Explorer) -> Option<Status> {
    match event {
        // Activation opens the file (see `handle_explorer`); the event
        // itself carries no footer message.
        ExplorerEvent::FileActivated(_) => None,
        ExplorerEvent::Refreshed => Some(Status::message(if explorer.show_hidden() {
            "Hidden files shown"
        } else {
            "Folder refreshed"
        })),
        ExplorerEvent::FilterChanged => Some(Status::message(if explorer.filter().is_empty() {
            format!("Showing all {} items", explorer.total_count())
        } else {
            format!(
                "{} of {} items match",
                explorer.match_count(),
                explorer.total_count()
            )
        })),
        ExplorerEvent::None
        | ExplorerEvent::SelectionChanged
        | ExplorerEvent::DirectoryChanged(_)
        | ExplorerEvent::FilterFocusChanged => None,
    }
}

fn handle_explorer(explorer: &mut Explorer, input: ExplorerInput) -> Option<Status> {
    match explorer.handle(input) {
        // Enter / double-click on a file: the workspace's opener policy
        // decides where it goes (an App pane beside this one, the editor,
        // or the system), exactly like the "Open" menu action.
        Ok(ExplorerEvent::FileActivated(path)) => Some(open_status(&path)),
        Ok(event) => status_for_event(event, explorer),
        Err(error) => Some(Status::error(error.to_string())),
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        if let Err(error) = execute!(stdout, EnableMouseCapture, EnableBracketedPaste) {
            let _ = execute!(
                stdout,
                DisableBracketedPaste,
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(
                    io::stdout(),
                    DisableBracketedPaste,
                    DisableMouseCapture,
                    LeaveAlternateScreen
                );
                let _ = terminal::disable_raw_mode();
                return Err(error);
            }
        };
        if let Err(error) = terminal.hide_cursor() {
            let _ = execute!(
                terminal.backend_mut(),
                DisableBracketedPaste,
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self { terminal })
    }

    fn draw(
        &mut self,
        node: &UiNode,
        explorer: &mut Explorer,
        drags: &mut DragSurface,
        tree_state: &mut TreeState,
        menu: Option<&mut ContextMenu>,
        theme: KitTheme,
    ) -> io::Result<()> {
        drags.begin_frame();
        self.terminal.draw(|frame| {
            render_component_frame(frame, node, explorer, drags, tree_state, menu, theme);
        })?;
        drags.commit()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

fn render_component_frame(
    frame: &mut Frame<'_>,
    node: &UiNode,
    explorer: &mut Explorer,
    drags: &mut DragSurface,
    tree_state: &mut TreeState,
    menu: Option<&mut ContextMenu>,
    theme: KitTheme,
) {
    let UiComponent::Tree(tree) = &node.element else {
        return;
    };
    let menu_open = menu.is_some();
    frame.render_widget(
        tree.widget_with_filter(tree_state, explorer.filter_input_mut())
            .theme(TreeTheme::for_theme(theme)),
        frame.area(),
    );

    let rows = tree_state.rows_area();
    for row in 0..rows.height {
        let position = Position::new(rows.x, rows.y.saturating_add(row));
        if let Some(path) = tree_state
            .item_id_at(position)
            .and_then(|id| explorer.path_for_semantic_item(id))
        {
            drags.register(Rect::new(rows.x, position.y, rows.width, 1), path);
        }
    }

    if let Some(menu) = menu {
        // The native drag receiver sees the frame-level map. Suppress
        // underlying path drags while a context menu covers the Tree.
        drags.begin_frame();
        menu.render(frame);
    }

    if !menu_open && let Some(position) = explorer.filter_cursor_position() {
        frame.set_cursor_position(position);
    }
}


/// Sidebar title for a browsed folder. At the project root it is the
/// project's own name; below it, the path from that root with a leading
/// slash (`/docs/agents`), so a deep folder reads as "where in the project"
/// rather than a bare basename. Outside any root (or with no root) it falls
/// back to the folder's name, or the whole path for a filesystem root.
fn folder_title(cwd: &std::path::Path, root: Option<&std::path::Path>) -> String {
    if let Some(root) = root
        && let Ok(relative) = cwd.strip_prefix(root)
        && !relative.as_os_str().is_empty()
    {
        let inside = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        return format!("/{inside}");
    }
    cwd.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| cwd.display().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn folder_title_is_the_project_name_at_root_and_a_rooted_path_below_it() {
        use std::path::Path;
        let root = Path::new("/Users/me/Dev/unpeel");
        assert_eq!(super::folder_title(root, Some(root)), "unpeel");
        assert_eq!(
            super::folder_title(Path::new("/Users/me/Dev/unpeel/docs/agents"), Some(root)),
            "/docs/agents"
        );
        // Outside the root, or without one, the folder name stands alone.
        assert_eq!(super::folder_title(Path::new("/tmp/notes"), Some(root)), "notes");
        assert_eq!(super::folder_title(Path::new("/tmp/notes"), None), "notes");
        assert_eq!(super::folder_title(Path::new("/"), None), "/");
    }

    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    #[test]
    fn keys_map_to_backend_neutral_explorer_actions() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("note.md"), "hello").unwrap();
        let mut explorer = Explorer::scoped(directory.path()).unwrap();
        explorer.set_selected_index(1);
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Some(ExplorerInput::Open)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(ExplorerInput::Parent)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Some(ExplorerInput::ToggleHidden)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(ExplorerInput::PageDown)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(ExplorerInput::Parent)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(ExplorerInput::FilterCharacter('q'))
        );
        explorer.set_filter_focused(true);
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(ExplorerInput::Parent)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(ExplorerInput::FilterCharacter('q'))
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(ExplorerInput::FilterBackspace)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(
                KeyCode::Left,
                KeyModifiers::ALT | KeyModifiers::SHIFT
            )),
            Some(ExplorerInput::FilterLeft {
                extend: true,
                word: true,
            })
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER)),
            Some(ExplorerInput::FilterSelectAll)
        );
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT)),
            Some(ExplorerInput::FilterHome { extend: true })
        );
        explorer.set_filter_focused(false);
        explorer.set_selected_index(0);
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(ExplorerInput::FocusFilter)
        );
        explorer.set_selected_index(1);
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(ExplorerInput::Up)
        );
        explorer.set_filter_focused(true);
        assert_eq!(
            explorer.input_for_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Some(ExplorerInput::BlurFilter)
        );
    }

    #[test]
    fn app_renders_the_exact_published_tree_and_registers_local_drags() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("file.txt"), "hello").unwrap();
        let theme = KitTheme::dark();
        let mut explorer = Explorer::scoped(directory.path())
            .unwrap()
            .with_theme(explorer_theme(theme));
        explorer.set_show_path(false);
        let node = semantic_node(&mut explorer, false, None);
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        let mut drags = DragSurface::disabled();
        let mut state = TreeState::default();

        drags.begin_frame();
        terminal
            .draw(|frame| {
                render_component_frame(
                    frame,
                    &node,
                    &mut explorer,
                    &mut drags,
                    &mut state,
                    None,
                    theme,
                );
            })
            .unwrap();

        assert_eq!(drags.regions().len(), 2);
        assert!(drags.regions()[0].path.ends_with("folder"));
        assert_eq!(terminal.backend().buffer()[(49, 0)].bg, Color::Reset);
        // Filter, location title, padding row, then the selected first row.
        assert_eq!(terminal.backend().buffer()[(49, 2)].bg, Color::Reset);
        assert_eq!(
            terminal.backend().buffer()[(49, 3)].bg,
            theme.selected_row.bg.unwrap()
        );
        assert_eq!(terminal.backend().buffer()[(49, 9)].bg, Color::Reset);
        let location = (0..50)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
            .collect::<String>();
        let root_name = directory
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            location.trim_end(),
            format!("  {root_name}"),
            "the root shows its folder name, not a lone dot"
        );
        assert!(
            !location.contains(directory.path().to_string_lossy().as_ref()),
            "Tree location must not expose the absolute project path\n{location}"
        );
        let filter_row = (0..50)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        let first_item_row = (0..50)
            .map(|x| terminal.backend().buffer()[(x, 2)].symbol())
            .collect::<String>();
        assert!(
            !filter_row.contains(directory.path().to_string_lossy().as_ref())
                && !first_item_row.contains(directory.path().to_string_lossy().as_ref()),
            "folder path must stay out of the published Tree\n{filter_row}\n{first_item_row}"
        );
    }

    #[test]
    fn app_projects_the_same_explorer_as_an_opaque_semantic_tree() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("note.md"), "hello").unwrap();
        let mut explorer = Explorer::scoped(directory.path()).unwrap();

        let node = semantic_node(&mut explorer, false, None);
        let unpeel_app_kit::UiComponent::Tree(tree) = node.element else {
            panic!("File Tree must publish the Tree component");
        };
        assert_eq!(tree.label, "Files");
        assert!(tree.filter.is_some());
        assert_eq!(tree.items.len(), 2);
        assert_eq!(tree.context_menu.as_ref().unwrap().items.len(), 3);
        assert_eq!(
            tree.footer
                .actions
                .iter()
                .map(|action| (action.id.as_str(), action.accelerator.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("refresh-files", Some("ctrl+r")),
                ("toggle-hidden-files", Some("ctrl+h")),
            ]
        );
        assert!(tree.items.iter().all(|item| item.id.starts_with("entry-")));
        assert!(
            !serde_json::to_string(&tree)
                .unwrap()
                .contains(directory.path().to_string_lossy().as_ref()),
            "semantic entry ids and labels must never expose the absolute root"
        );

        let status = Status::error("Could not open entry");
        let node = semantic_node(&mut explorer, false, Some(&status));
        let unpeel_app_kit::UiComponent::Tree(tree) = node.element else {
            panic!("File Tree must publish the Tree component");
        };
        let root_name = std::fs::canonicalize(directory.path())
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            tree.location,
            format!("{root_name} · Error: Could not open entry")
        );
    }

    #[test]
    fn semantic_context_menu_resolves_opaque_targets_inside_the_explorer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        std::fs::write(&path, "hello").unwrap();
        let mut explorer = Explorer::scoped(directory.path()).unwrap();
        let node = semantic_node(&mut explorer, true, None);
        let unpeel_app_kit::UiComponent::Tree(tree) = node.element else {
            panic!("File Tree must publish the Tree component");
        };
        let target = tree.items[0].id.clone();
        let action = unpeel_app_kit::UiAction::new(
            "open-in-editor",
            OPEN_IN_EDITOR_ACTION,
            UiEventKind::Activate,
            UiEventValue::Text(target),
        );

        assert_eq!(
            semantic_context_action(&mut explorer, true, &action).unwrap(),
            ContextAction::OpenInEditor(path.canonicalize().unwrap())
        );
        assert_eq!(explorer.selected().unwrap().name(), "note.md");
    }

    #[test]
    fn context_menu_uses_full_width_selection_and_suppresses_path_drags() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("file.txt"), "hello").unwrap();
        let theme = KitTheme::light();
        let mut explorer = Explorer::scoped(directory.path())
            .unwrap()
            .with_theme(explorer_theme(theme));
        let path = directory.path().join("file.txt");
        let spec = semantic_context_menu(true);
        let mut menu = context_menu(path, &spec, Position::new(4, 4), theme.scheme);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut drags = DragSurface::disabled();
        let node = semantic_node(&mut explorer, true, None);
        let mut state = TreeState::default();

        terminal
            .draw(|frame| {
                render_component_frame(
                    frame,
                    &node,
                    &mut explorer,
                    &mut drags,
                    &mut state,
                    Some(&mut menu),
                    theme,
                );
            })
            .unwrap();

        assert!(drags.regions().is_empty());
        assert_eq!(menu.items().len(), 4);
        let items_area = menu.items_area();
        assert_eq!(
            terminal.backend().buffer()[(items_area.right() - 1, items_area.y)].bg,
            theme.selected_row.bg.unwrap()
        );
        assert_eq!(
            terminal.backend().buffer()[(items_area.x, items_area.y)].symbol(),
            "O"
        );
        assert_eq!(
            terminal.backend().buffer()[(items_area.x + 1, items_area.y)].symbol(),
            "p"
        );
    }

    #[test]
    fn activating_a_file_keeps_the_path_footer_unchanged() {
        let path = PathBuf::from("/tmp/file with spaces.txt");
        let explorer = Explorer::new(std::env::temp_dir()).unwrap();
        assert!(status_for_event(ExplorerEvent::FileActivated(path), &explorer).is_none());
    }

    #[test]
    fn double_clicking_a_folder_row_enters_it() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let folder = std::fs::canonicalize(folder).unwrap();
        let mut explorer = Explorer::scoped(directory.path()).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut drags = DragSurface::disabled();
        let node = semantic_node(&mut explorer, false, None);
        let mut state = TreeState::default();
        terminal
            .draw(|frame| {
                render_component_frame(
                    frame,
                    &node,
                    &mut explorer,
                    &mut drags,
                    &mut state,
                    None,
                    KitTheme::dark(),
                )
            })
            .unwrap();

        let position = (state.rows_area().y..state.rows_area().bottom())
            .map(|y| Position::new(state.rows_area().x, y))
            .find(|position| {
                state
                    .item_id_at(*position)
                    .is_some_and(|id| explorer.path_for_semantic_item(id) == Some(folder.as_path()))
            })
            .expect("folder row");
        let mut clicks = DoubleClickTracker::new();

        assert_eq!(
            explorer_click_at(&mut explorer, &state, position, &mut clicks),
            Some(false)
        );
        assert_eq!(explorer.selected().unwrap().path(), folder);
        assert_eq!(
            explorer_click_at(&mut explorer, &state, position, &mut clicks),
            Some(true)
        );
        assert_eq!(
            explorer.handle(ExplorerInput::Open).unwrap(),
            ExplorerEvent::DirectoryChanged(folder.clone())
        );
        assert_eq!(explorer.cwd(), folder);
    }
}
