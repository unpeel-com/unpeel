use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use unpeel_app_kit::{
    AgentBridge, ColorScheme, DragSurface, Explorer, ExplorerEvent, ExplorerInput, ExplorerTheme,
    KitTheme, MenuItem, MenuTheme, PopupMenu, clipboard_sequence,
};

use crate::unpeel::ContextReporter;

const FOOTER_ROWS: u16 = 1;

pub fn run(mut explorer: Explorer) -> io::Result<()> {
    let theme = KitTheme::detected();
    explorer.set_theme(explorer_theme(theme.scheme));
    let mut drags = DragSurface::detect();
    let mouse_capture = drags.is_available();
    let mut terminal = TerminalGuard::enter(mouse_capture)?;
    let mut reporter = ContextReporter::detect();
    let agent = AgentBridge::new();
    agent.refresh();
    let mut menu = None;
    let mut status = None;
    let mut needs_draw = true;

    loop {
        if needs_draw {
            reporter.publish(explorer.cwd(), explorer.selected());
            terminal.draw(
                &mut explorer,
                &mut drags,
                menu.as_mut(),
                status.as_ref(),
                theme,
            )?;
            needs_draw = false;
        }
        if !event::poll(Duration::from_millis(250))? {
            drags.heartbeat()?;
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
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
                let Some(action) = action_for_key(key, explorer.filter_focused()) else {
                    continue;
                };
                match action {
                    AppAction::Quit => break,
                    AppAction::Explorer(input) => {
                        status = match explorer.handle(input) {
                            Ok(event) => status_for_event(event, &explorer),
                            Err(error) => Some(Status::error(error.to_string())),
                        };
                        needs_draw = true;
                    }
                }
            }
            Event::Mouse(mouse) if mouse_capture => {
                let position = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Right) => {
                        if let Some(path) = explorer
                            .entry_at(position)
                            .map(|entry| entry.path().to_path_buf())
                        {
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
                            if open_menu
                                .item_at(position)
                                .is_some_and(MenuItem::is_enabled)
                            {
                                open_menu.select_at(position);
                                status = Some(activate_menu(open_menu, &agent));
                            }
                            needs_draw = true;
                        } else if explorer.select_at(position) {
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if let Some(open_menu) = menu.as_mut() {
                            open_menu.move_selection(-1);
                            needs_draw = true;
                        } else {
                            status = handle_explorer(&mut explorer, ExplorerInput::Up);
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::ScrollDown => {
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
            Event::Resize(_, _) => needs_draw = true,
            _ => {}
        }
    }
    Ok(())
}

fn explorer_theme(scheme: ColorScheme) -> ExplorerTheme {
    ExplorerTheme::for_color_scheme(scheme)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppAction {
    Quit,
    Explorer(ExplorerInput),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ContextAction {
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
    let mut items = Vec::with_capacity(2);
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

fn action_for_key(key: KeyEvent, filter_focused: bool) -> Option<AppAction> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alternate = key.modifiers.contains(KeyModifiers::ALT);
    if is_force_quit(key) {
        return Some(AppAction::Quit);
    }
    if filter_focused {
        return match key.code {
            KeyCode::Esc => Some(AppAction::Explorer(ExplorerInput::Parent)),
            KeyCode::Tab => Some(AppAction::Explorer(ExplorerInput::BlurFilter)),
            KeyCode::Up => Some(AppAction::Explorer(ExplorerInput::Up)),
            KeyCode::Down => Some(AppAction::Explorer(ExplorerInput::Down)),
            KeyCode::Home => Some(AppAction::Explorer(ExplorerInput::First)),
            KeyCode::End => Some(AppAction::Explorer(ExplorerInput::Last)),
            KeyCode::PageUp => Some(AppAction::Explorer(ExplorerInput::PageUp)),
            KeyCode::PageDown => Some(AppAction::Explorer(ExplorerInput::PageDown)),
            KeyCode::Enter => Some(AppAction::Explorer(ExplorerInput::Open)),
            KeyCode::Backspace | KeyCode::Char('h') if control => {
                Some(AppAction::Explorer(ExplorerInput::FilterBackspace))
            }
            KeyCode::Char('u') if control => Some(AppAction::Explorer(ExplorerInput::ClearFilter)),
            KeyCode::Char(character) if !control && !alternate => Some(AppAction::Explorer(
                ExplorerInput::FilterCharacter(character),
            )),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char('h') if control => Some(AppAction::Explorer(ExplorerInput::ToggleHidden)),
        KeyCode::Char('f') if control => Some(AppAction::Explorer(ExplorerInput::FocusFilter)),
        KeyCode::Char('/') => Some(AppAction::Explorer(ExplorerInput::FocusFilter)),
        KeyCode::Char('q') => Some(AppAction::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(AppAction::Explorer(ExplorerInput::Up)),
        KeyCode::Down | KeyCode::Char('j') => Some(AppAction::Explorer(ExplorerInput::Down)),
        KeyCode::Home | KeyCode::Char('g') => Some(AppAction::Explorer(ExplorerInput::First)),
        KeyCode::End | KeyCode::Char('G') => Some(AppAction::Explorer(ExplorerInput::Last)),
        KeyCode::PageUp => Some(AppAction::Explorer(ExplorerInput::PageUp)),
        KeyCode::PageDown => Some(AppAction::Explorer(ExplorerInput::PageDown)),
        KeyCode::Esc | KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
            Some(AppAction::Explorer(ExplorerInput::Parent))
        }
        KeyCode::Right | KeyCode::Enter | KeyCode::Char('l' | ' ') => {
            Some(AppAction::Explorer(ExplorerInput::Open))
        }
        KeyCode::Char('r') => Some(AppAction::Explorer(ExplorerInput::Refresh)),
        _ => None,
    }
}

fn activate_menu(menu: ContextMenu, agent: &AgentBridge) -> Status {
    let Some(action) = menu.selected_value().cloned() else {
        return Status::error("No menu action selected");
    };
    match action {
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
        ExplorerEvent::FileActivated(path) => Some(Status::message(format!(
            "Drag {} into another pane",
            path.display()
        ))),
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
    mouse_capture: bool,
}

impl TerminalGuard {
    fn enter(mouse_capture: bool) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        if mouse_capture && let Err(error) = execute!(stdout, EnableMouseCapture) {
            let _ = execute!(stdout, LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                if mouse_capture {
                    let _ = execute!(io::stdout(), DisableMouseCapture);
                }
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                let _ = terminal::disable_raw_mode();
                return Err(error);
            }
        };
        if let Err(error) = terminal.hide_cursor() {
            if mouse_capture {
                let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
            }
            let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self {
            terminal,
            mouse_capture,
        })
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
        if self.mouse_capture {
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        }
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
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
    let footer_rows = if area.height >= 2 { FOOTER_ROWS } else { 0 };
    let explorer_area = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(footer_rows),
    );
    frame.render_widget(explorer.widget(drags), explorer_area);

    if footer_rows > 0 {
        let help = if menu.is_some() {
            "Esc close · ↑↓ select · Enter choose"
        } else if explorer.filter_focused() {
            "Esc back · ↑↓ select · Enter open"
        } else {
            "↑↓ select · Enter open · / filter"
        };
        let (message, style) = status.map_or_else(
            || (help.to_owned(), Style::new().fg(theme.muted)),
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
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }

    if let Some(menu) = menu {
        // The native drag receiver sees the frame-level map. Suppress
        // underlying path drags while a context menu covers the Explorer.
        drags.begin_frame();
        menu.render(frame);
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
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), false),
            Some(AppAction::Explorer(ExplorerInput::Open))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), false),
            Some(AppAction::Explorer(ExplorerInput::Parent))
        );
        assert_eq!(
            action_for_key(
                KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
                false
            ),
            Some(AppAction::Explorer(ExplorerInput::ToggleHidden))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), false),
            Some(AppAction::Explorer(ExplorerInput::PageDown))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false),
            Some(AppAction::Explorer(ExplorerInput::Parent))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), true),
            Some(AppAction::Explorer(ExplorerInput::Parent))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), true),
            Some(AppAction::Explorer(ExplorerInput::FilterCharacter('q')))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), false),
            Some(AppAction::Quit)
        );
    }

    #[test]
    fn app_renders_the_shared_borderless_draggable_explorer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("file.txt"), "hello").unwrap();
        let theme = KitTheme::dark();
        let mut explorer = Explorer::new(directory.path())
            .unwrap()
            .with_theme(explorer_theme(theme.scheme));
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        let mut drags = DragSurface::disabled();

        drags.begin_frame();
        terminal
            .draw(|frame| render_frame(frame, &mut explorer, &mut drags, None, None, theme))
            .unwrap();

        assert_eq!(drags.regions().len(), 4);
        assert_eq!(drags.regions()[0].path, explorer.cwd());
        assert_eq!(drags.regions()[0].area.y, 1);
        assert!(drags.regions()[2].path.ends_with("folder"));
        assert_eq!(terminal.backend().buffer()[(49, 0)].bg, Color::Reset);
        assert_eq!(
            terminal.backend().buffer()[(49, 2)].bg,
            theme.selected_row.bg.unwrap()
        );
        assert_eq!(terminal.backend().buffer()[(49, 9)].bg, Color::Reset);
    }

    #[test]
    fn context_menu_uses_full_width_selection_and_suppresses_path_drags() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("file.txt"), "hello").unwrap();
        let theme = KitTheme::light();
        let mut explorer = Explorer::new(directory.path())
            .unwrap()
            .with_theme(explorer_theme(theme.scheme));
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
        assert_eq!(menu.items().len(), 2);
        let items_area = menu.items_area();
        assert_eq!(
            terminal.backend().buffer()[(items_area.right() - 1, items_area.y)].bg,
            theme.selected_row.bg.unwrap()
        );
        assert_eq!(
            terminal.backend().buffer()[(items_area.x, items_area.y)].symbol(),
            " "
        );
        assert_eq!(
            terminal.backend().buffer()[(items_area.x + 1, items_area.y)].symbol(),
            " "
        );
    }

    #[test]
    fn activated_files_are_described_as_drag_sources() {
        let path = PathBuf::from("/tmp/file with spaces.txt");
        let explorer = Explorer::new(std::env::temp_dir()).unwrap();
        let status =
            status_for_event(ExplorerEvent::FileActivated(path.clone()), &explorer).unwrap();
        assert_eq!(
            status.message,
            format!("Drag {} into another pane", path.display())
        );
        assert!(!status.error);
    }
}
