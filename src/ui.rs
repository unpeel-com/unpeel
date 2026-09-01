use std::io::{self, Stdout, Write};
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
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use unpeel_app_kit::{
    AgentBridge, AppContext, AppMetadata, AppReporter, ColorScheme, DoubleClickTracker,
    DragSurface, EditorBridge, Explorer, ExplorerEvent, ExplorerInput, ExplorerTheme,
    KeyboardEnhancementGuard, KitTheme, MenuItem, MenuTheme, PopupMenu, ThemeMonitor, UiBridge,
    UiBridgeEvent, UiEventOutcome, UiNode, clipboard_sequence, display_path_from_root,
    tree_delta_operations,
};

const FOOTER_ROWS: u16 = 1;
const UI_VIEW_ID: &str = "main";
const UI_TREE_ID: &str = "file-tree";

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
    let mut bridge = UiBridge::detect(
        AppMetadata::new(
            crate::install::APP_ID,
            "Unpeel File Tree",
            env!("CARGO_PKG_VERSION"),
        )
        .description("Standalone Explorer with an optional semantic Tree projection"),
    )
    .map_err(ui_bridge_error)?;
    let mut ui_revision = 1u64;
    let mut published = UiNode::tree(UI_TREE_ID, explorer.semantic_tree("Files"));
    bridge
        .publish(UI_VIEW_ID, ui_revision, published.clone())
        .map_err(ui_bridge_error)?;

    loop {
        drain_bridge(
            &mut explorer,
            &mut bridge,
            &mut ui_revision,
            &mut published,
            &mut status,
            &mut needs_draw,
        )?;
        publish_projection(&mut explorer, &mut bridge, &mut ui_revision, &mut published)?;
        if needs_draw {
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
                    &mut explorer,
                    &mut drags,
                    menu.as_mut(),
                    status.as_ref(),
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
                let position = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Right) => {
                        clicks.reset();
                        if let Some(path) = explorer
                            .entry_at(position)
                            .map(|entry| entry.path().to_path_buf())
                        {
                            explorer.set_filter_focused(false);
                            explorer.select_at(position);
                            agent.refresh();
                            menu = Some(context_menu(
                                path,
                                agent.label().is_some(),
                                position,
                                theme.scheme,
                            ));
                            needs_draw = true;
                        } else if menu.take().is_some() {
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(mut open_menu) = menu.take() {
                            clicks.reset();
                            if open_menu
                                .item_at(position)
                                .is_some_and(MenuItem::is_enabled)
                            {
                                open_menu.select_at(position);
                                status = Some(activate_menu(open_menu, &agent));
                            }
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
                            explorer_click_at(&mut explorer, position, &mut clicks)
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
    bridge: &mut UiBridge,
    revision: &mut u64,
    published: &mut UiNode,
) -> io::Result<()> {
    let next = UiNode::tree(UI_TREE_ID, explorer.semantic_tree("Files"));
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

fn drain_bridge(
    explorer: &mut Explorer,
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
        let outcome = match explorer.handle_ui_event(*revision, UI_TREE_ID, &event) {
            Ok(Some(explorer_event)) => {
                *status = status_for_event(explorer_event, explorer);
                *needs_draw = true;
                UiEventOutcome::Applied
            }
            Ok(None) => UiEventOutcome::Rejected(
                "Action targets a different File Tree component".to_string(),
            ),
            Err(message) => UiEventOutcome::Rejected(message),
        };
        publish_projection(explorer, bridge, revision, published)?;
        bridge
            .acknowledge(&event, outcome, *revision)
            .map_err(ui_bridge_error)?;
    }
    Ok(())
}

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error)
}

fn explorer_click_at(
    explorer: &mut Explorer,
    position: Position,
    clicks: &mut DoubleClickTracker<PathBuf>,
) -> Option<bool> {
    let Some(path) = explorer
        .entry_at(position)
        .map(|entry| entry.path().to_path_buf())
    else {
        clicks.reset();
        return None;
    };
    explorer.set_filter_focused(false);
    let activate = clicks.click(path);
    explorer.select_at(position);
    Some(activate)
}

fn explorer_theme(theme: KitTheme) -> ExplorerTheme {
    ExplorerTheme::for_theme(theme)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ContextAction {
    OpenInEditor(PathBuf),
    SendToAgent(PathBuf),
    CopyPath(PathBuf),
}

type ContextMenu = PopupMenu<ContextAction>;

fn context_menu(
    path: PathBuf,
    can_send: bool,
    anchor: Position,
    scheme: ColorScheme,
) -> ContextMenu {
    let mut items = Vec::with_capacity(3);
    items.push(MenuItem::new(
        "Open in editor",
        ContextAction::OpenInEditor(path.clone()),
    ));
    if can_send {
        items.push(MenuItem::new(
            "Send to agent",
            ContextAction::SendToAgent(path.clone()),
        ));
    }
    items.push(MenuItem::new("Copy path", ContextAction::CopyPath(path)));
    PopupMenu::new(anchor, items).with_theme(MenuTheme::for_color_scheme(scheme))
}

fn is_force_quit(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn activate_menu(menu: ContextMenu, agent: &AgentBridge) -> Status {
    let Some(action) = menu.selected_value().cloned() else {
        return Status::error("No menu action selected");
    };
    match action {
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

fn status_for_event(event: ExplorerEvent, explorer: &Explorer) -> Option<Status> {
    match event {
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
        explorer: &mut Explorer,
        drags: &mut DragSurface,
        menu: Option<&mut ContextMenu>,
        status: Option<&Status>,
        theme: KitTheme,
    ) -> io::Result<()> {
        drags.begin_frame();
        self.terminal
            .draw(|frame| render_frame(frame, explorer, drags, menu, status, theme))?;
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

fn render_frame(
    frame: &mut Frame<'_>,
    explorer: &mut Explorer,
    drags: &mut DragSurface,
    menu: Option<&mut ContextMenu>,
    status: Option<&Status>,
    theme: KitTheme,
) {
    let area = frame.area();
    let menu_open = menu.is_some();
    let footer_rows = if area.height >= 2 { FOOTER_ROWS } else { 0 };
    let explorer_area = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(footer_rows),
    );
    frame.render_widget(explorer.widget(drags), explorer_area);

    if footer_rows > 0 {
        let footer_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let (message, style) = status.map_or_else(
            || {
                drags.register(footer_area, explorer.cwd());
                let root = explorer.navigation_root().unwrap_or_else(|| explorer.cwd());
                (
                    display_path_from_root(explorer.cwd(), root),
                    Style::new().fg(theme.muted),
                )
            },
            |status| {
                (
                    status.message.clone(),
                    Style::new().fg(if status.error {
                        theme.danger
                    } else {
                        theme.muted
                    }),
                )
            },
        );
        frame.render_widget(
            Paragraph::new(format!("  {message}")).style(style),
            footer_area,
        );
    }

    if let Some(menu) = menu {
        // The native drag receiver sees the frame-level map. Suppress
        // underlying path drags while a context menu covers the Explorer.
        drags.begin_frame();
        menu.render(frame);
    }

    if !menu_open && let Some(position) = explorer.filter_cursor_position() {
        frame.set_cursor_position(position);
    }
}

#[cfg(test)]
mod tests {
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
    fn app_renders_the_shared_borderless_draggable_explorer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("file.txt"), "hello").unwrap();
        let theme = KitTheme::dark();
        let mut explorer = Explorer::scoped(directory.path())
            .unwrap()
            .with_theme(explorer_theme(theme));
        explorer.set_show_path(false);
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        let mut drags = DragSurface::disabled();

        drags.begin_frame();
        terminal
            .draw(|frame| render_frame(frame, &mut explorer, &mut drags, None, None, theme))
            .unwrap();

        assert_eq!(drags.regions().len(), 3);
        let cwd_drag = drags
            .regions()
            .iter()
            .find(|region| region.path == explorer.cwd())
            .expect("current-folder footer drag");
        assert_eq!(cwd_drag.area.y, 11);
        assert!(drags.regions()[0].path.ends_with("folder"));
        assert_eq!(terminal.backend().buffer()[(49, 0)].bg, Color::Reset);
        assert_eq!(
            terminal.backend().buffer()[(49, 1)].bg,
            theme.selected_row.bg.unwrap()
        );
        assert_eq!(terminal.backend().buffer()[(49, 9)].bg, Color::Reset);
        let footer = (0..50)
            .map(|x| terminal.backend().buffer()[(x, 11)].symbol())
            .collect::<String>();
        assert!(
            footer.contains("  ."),
            "project-root-relative folder path\n{footer}"
        );
        assert!(
            !footer.contains(directory.path().to_string_lossy().as_ref()),
            "footer must not expose the absolute project path\n{footer}"
        );
        assert!(!footer.contains("Enter open"), "no shortcut help\n{footer}");
        assert_eq!(terminal.backend().buffer()[(2, 11)].fg, theme.muted);
        let filter_row = (0..50)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();
        let first_item_row = (0..50)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
            .collect::<String>();
        assert!(
            !filter_row.contains(directory.path().to_string_lossy().as_ref())
                && !first_item_row.contains(directory.path().to_string_lossy().as_ref()),
            "folder path should only appear in the footer\n{filter_row}\n{first_item_row}"
        );
    }

    #[test]
    fn app_projects_the_same_explorer_as_an_opaque_semantic_tree() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("note.md"), "hello").unwrap();
        let mut explorer = Explorer::scoped(directory.path()).unwrap();

        let node = UiNode::tree(UI_TREE_ID, explorer.semantic_tree("Files"));
        let unpeel_app_kit::UiComponent::Tree(tree) = node.element else {
            panic!("File Tree must publish the Tree component");
        };
        assert_eq!(tree.label, "Files");
        assert!(tree.filter.is_some());
        assert_eq!(tree.items.len(), 2);
        assert!(tree.items.iter().all(|item| item.id.starts_with("entry-")));
        assert!(
            !serde_json::to_string(&tree)
                .unwrap()
                .contains(directory.path().to_string_lossy().as_ref()),
            "semantic entry ids and labels must never expose the absolute root"
        );
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
        let mut menu = context_menu(path, true, Position::new(4, 4), theme.scheme);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut drags = DragSurface::disabled();

        terminal
            .draw(|frame| {
                render_frame(
                    frame,
                    &mut explorer,
                    &mut drags,
                    Some(&mut menu),
                    None,
                    theme,
                );
            })
            .unwrap();

        assert!(drags.regions().is_empty());
        assert_eq!(menu.items().len(), 3);
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
        terminal
            .draw(|frame| {
                render_frame(
                    frame,
                    &mut explorer,
                    &mut drags,
                    None,
                    None,
                    KitTheme::dark(),
                )
            })
            .unwrap();

        let index = explorer
            .entries()
            .iter()
            .position(|entry| entry.path() == folder)
            .unwrap();
        let position = Position::new(
            explorer.list_area().x,
            explorer.list_area().y + (index - explorer.scroll_offset()) as u16,
        );
        let mut clicks = DoubleClickTracker::new();

        assert_eq!(
            explorer_click_at(&mut explorer, position, &mut clicks),
            Some(false)
        );
        assert_eq!(explorer.selected().unwrap().path(), folder);
        assert_eq!(
            explorer_click_at(&mut explorer, position, &mut clicks),
            Some(true)
        );
        assert_eq!(
            explorer.handle(ExplorerInput::Open).unwrap(),
            ExplorerEvent::DirectoryChanged(folder.clone())
        );
        assert_eq!(explorer.cwd(), folder);
    }
}
