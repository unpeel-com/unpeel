use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use unpeel_tui_kit::{DragSurface, Explorer, ExplorerEvent, ExplorerInput, ExplorerTheme};

use crate::unpeel::ContextReporter;

const FOOTER_ROWS: u16 = 2;

const TEXT: Color = Color::Rgb(228, 228, 231);
const MUTED: Color = Color::Rgb(113, 113, 122);
const VIOLET: Color = Color::Rgb(167, 139, 250);
const CYAN: Color = Color::Rgb(103, 232, 249);
const ERROR: Color = Color::Rgb(248, 113, 113);
const SELECTED: Color = Color::Rgb(63, 63, 70);

pub fn run(mut explorer: Explorer) -> io::Result<()> {
    explorer.set_theme(explorer_theme());
    let mut terminal = TerminalGuard::enter()?;
    let mut reporter = ContextReporter::detect();
    let mut drags = DragSurface::detect();
    let mut status = None;
    let mut needs_draw = true;

    loop {
        if needs_draw {
            reporter.publish(explorer.cwd(), explorer.selected());
            terminal.draw(&mut explorer, &mut drags, status.as_ref())?;
            needs_draw = false;
        }
        if !event::poll(Duration::from_millis(250))? {
            drags.heartbeat()?;
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let Some(action) = action_for_key(key) else {
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
            Event::Resize(_, _) => needs_draw = true,
            _ => {}
        }
    }
    Ok(())
}

fn explorer_theme() -> ExplorerTheme {
    ExplorerTheme {
        style: Style::new(),
        path: Style::new().fg(MUTED).add_modifier(Modifier::BOLD),
        item: Style::new().fg(TEXT),
        directory: Style::new().fg(VIOLET),
        symlink: Style::new().fg(CYAN),
        parent: Style::new().fg(VIOLET),
        selected: Style::new().bg(SELECTED).add_modifier(Modifier::BOLD),
        empty: Style::new().fg(MUTED),
        scrollbar_track: Style::new().fg(Color::Rgb(39, 39, 42)),
        scrollbar_thumb: Style::new().fg(MUTED),
        selected_symbol: None,
        left_padding: 2,
        scroll_padding: 1,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppAction {
    Quit,
    Explorer(ExplorerInput),
}

fn action_for_key(key: KeyEvent) -> Option<AppAction> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if control => Some(AppAction::Quit),
        KeyCode::Char('h') if control => Some(AppAction::Explorer(ExplorerInput::ToggleHidden)),
        KeyCode::Char('q') | KeyCode::Esc => Some(AppAction::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(AppAction::Explorer(ExplorerInput::Up)),
        KeyCode::Down | KeyCode::Char('j') => Some(AppAction::Explorer(ExplorerInput::Down)),
        KeyCode::Home | KeyCode::Char('g') => Some(AppAction::Explorer(ExplorerInput::First)),
        KeyCode::End | KeyCode::Char('G') => Some(AppAction::Explorer(ExplorerInput::Last)),
        KeyCode::PageUp => Some(AppAction::Explorer(ExplorerInput::PageUp)),
        KeyCode::PageDown => Some(AppAction::Explorer(ExplorerInput::PageDown)),
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
            Some(AppAction::Explorer(ExplorerInput::Parent))
        }
        KeyCode::Right | KeyCode::Enter | KeyCode::Char('l' | ' ') => {
            Some(AppAction::Explorer(ExplorerInput::Open))
        }
        KeyCode::Char('r') => Some(AppAction::Explorer(ExplorerInput::Refresh)),
        _ => None,
    }
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
        ExplorerEvent::None
        | ExplorerEvent::SelectionChanged
        | ExplorerEvent::DirectoryChanged(_) => None,
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
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                let _ = terminal::disable_raw_mode();
                return Err(error);
            }
        };
        terminal.hide_cursor()?;
        Ok(Self { terminal })
    }

    fn draw(
        &mut self,
        explorer: &mut Explorer,
        drags: &mut DragSurface,
        status: Option<&Status>,
    ) -> io::Result<()> {
        drags.begin_frame();
        self.terminal
            .draw(|frame| render_frame(frame, explorer, drags, status))?;
        drags.commit()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn render_frame(
    frame: &mut Frame<'_>,
    explorer: &mut Explorer,
    drags: &mut DragSurface,
    status: Option<&Status>,
) {
    let area = frame.area();
    let footer_rows = if area.height >= 4 { FOOTER_ROWS } else { 0 };
    let explorer_area = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(footer_rows),
    );
    frame.render_widget(explorer.widget(drags), explorer_area);

    if footer_rows == 0 {
        return;
    }
    let help_area = Rect::new(
        area.x,
        area.bottom().saturating_sub(FOOTER_ROWS),
        area.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(
            "  ↑↓ move  ← parent  →/Enter open  PgUp/PgDn page  ^h hidden  r refresh  q quit",
        )
        .style(Style::new().fg(MUTED)),
        help_area,
    );

    let (message, style) = status.map_or_else(
        || {
            (
                explorer
                    .selected()
                    .map(|entry| entry.path().display().to_string())
                    .unwrap_or_default(),
                Style::new().fg(MUTED),
            )
        },
        |status| {
            (
                status.message.clone(),
                Style::new().fg(if status.error { ERROR } else { MUTED }),
            )
        },
    );
    let status_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
    frame.render_widget(
        Paragraph::new(format!("  {message}")).style(style),
        status_area,
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn keys_map_to_backend_neutral_explorer_actions() {
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Some(AppAction::Explorer(ExplorerInput::Open))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(AppAction::Explorer(ExplorerInput::Parent))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Some(AppAction::Explorer(ExplorerInput::ToggleHidden))
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(AppAction::Explorer(ExplorerInput::PageDown))
        );
    }

    #[test]
    fn app_renders_the_shared_borderless_draggable_explorer() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("folder")).unwrap();
        std::fs::write(directory.path().join("file.txt"), "hello").unwrap();
        let mut explorer = Explorer::new(directory.path())
            .unwrap()
            .with_theme(explorer_theme());
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        let mut drags = DragSurface::disabled();

        drags.begin_frame();
        terminal
            .draw(|frame| render_frame(frame, &mut explorer, &mut drags, None))
            .unwrap();

        assert_eq!(drags.regions().len(), 4);
        assert_eq!(drags.regions()[0].path, explorer.cwd());
        assert_eq!(drags.regions()[0].area.y, 0);
        assert!(drags.regions()[2].path.ends_with("folder"));
        assert_eq!(terminal.backend().buffer()[(49, 0)].bg, Color::Reset);
        assert_eq!(terminal.backend().buffer()[(49, 9)].bg, Color::Reset);
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
