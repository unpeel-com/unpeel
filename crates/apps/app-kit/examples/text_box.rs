//! `TextBox` demo: a chat-style prompt bar and a plain form-style field.
//!
//! Run with `cargo run --example text_box` (also builds with
//! `--no-default-features`). Tab switches focus between the two boxes.
//! In the prompt, Enter submits and Shift+Enter or Alt+Enter inserts a
//! newline; the fake "response" shows the busy status row for a few seconds.
//! In the form field Enter always inserts a newline and Ctrl+S submits.
//! Esc or Ctrl+C quits.

use std::error::Error;
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use unpeel_app_kit::{
    BusyStatus, KitTheme, SubmitMode, TextBox, TextBoxAction, TextBoxOutcome, TextBoxTheme,
    TitlePosition,
};

const TICK: Duration = Duration::from_millis(80);
const FAKE_RESPONSE_TIME: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Prompt,
    Form,
}

struct App {
    prompt: TextBox,
    form: TextBox,
    focus: Focus,
    transcript: Vec<String>,
    busy_since: Option<Instant>,
    palette: KitTheme,
}

impl App {
    fn new() -> Self {
        let palette = KitTheme::detected();
        let theme = TextBoxTheme::detected();
        let mut prompt = TextBox::new("Ask anything, or type / for commands")
            .with_prompt("❯ ")
            .with_status_title("Grok 4.6 (xhigh) · always-approve")
            .with_footer_hints([
                ("Shift+Tab", "mode"),
                ("Esc", "cancel"),
                ("Ctrl+x", "shortcuts"),
            ])
            .with_rows(3, 8)
            .with_theme(theme);
        prompt.set_focused(true);
        let form = TextBox::new("Summarize the change (Ctrl+S to submit)")
            .with_title("Commit message", TitlePosition::TopLeft)
            .with_submit_mode(SubmitMode::Never)
            .with_rows(2, 6)
            .with_theme(theme);
        Self {
            prompt,
            form,
            focus: Focus::Prompt,
            transcript: vec!["Welcome. Send a message to see the busy state.".to_owned()],
            busy_since: None,
            palette,
        }
    }

    fn focused_box(&mut self) -> &mut TextBox {
        match self.focus {
            Focus::Prompt => &mut self.prompt,
            Focus::Form => &mut self.form,
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        self.prompt.set_focused(focus == Focus::Prompt);
        self.form.set_focused(focus == Focus::Form);
    }

    fn start_response(&mut self, message: String) {
        self.transcript.push(format!("you: {message}"));
        self.busy_since = Some(Instant::now());
        self.prompt.set_busy(Some(
            BusyStatus::new("Waiting for response…").with_right_meta("↓0k [stop]"),
        ));
    }

    fn tick(&mut self) -> bool {
        let mut changed = self.prompt.tick();
        if let Some(since) = self.busy_since {
            let elapsed = since.elapsed();
            if elapsed >= FAKE_RESPONSE_TIME {
                self.busy_since = None;
                self.prompt.set_busy(None);
                self.transcript
                    .push("assistant: Done. That took about four seconds.".to_owned());
                changed = true;
            } else if let Some(busy) = self.prompt.busy_mut() {
                busy.elapsed = elapsed;
                busy.right_meta = format!(
                    "{}s ↓{}k [stop]",
                    elapsed.as_secs(),
                    elapsed.as_millis() / 12
                );
                changed = true;
            }
        }
        changed
    }

    /// Returns `false` when the App should quit.
    fn key(&mut self, key: KeyEvent) -> bool {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return true;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Char('c') if control => return false,
            KeyCode::Tab | KeyCode::BackTab => {
                let next = match self.focus {
                    Focus::Prompt => Focus::Form,
                    Focus::Form => Focus::Prompt,
                };
                self.set_focus(next);
                return true;
            }
            KeyCode::Char('s') if control && self.focus == Focus::Form => {
                if let Some(text) = self.form.submit() {
                    self.transcript
                        .push(format!("commit message ({} chars) saved", text.len()));
                }
                return true;
            }
            _ => {}
        }
        let Some(action) = self.focused_box().action_for_key(&key) else {
            return true;
        };
        if let TextBoxOutcome::Submitted(text) = self.focused_box().handle(action) {
            match self.focus {
                Focus::Prompt => self.start_response(text),
                Focus::Form => self.transcript.push(format!("form: {text}")),
            }
        }
        true
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let position = Position::new(mouse.column, mouse.row);
        let extend = mouse.modifiers.contains(KeyModifiers::SHIFT);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.prompt.box_area().contains(position) {
                    self.set_focus(Focus::Prompt);
                    self.prompt.mouse_down(position, extend);
                } else if self.form.box_area().contains(position) {
                    self.set_focus(Focus::Form);
                    self.form.mouse_down(position, extend);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.focused_box().mouse_drag(position);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.focused_box().mouse_up();
            }
            MouseEventKind::ScrollUp => {
                self.focused_box().scroll_rows(-1);
            }
            MouseEventKind::ScrollDown => {
                self.focused_box().scroll_rows(1);
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        let prompt_height = self.prompt.height_for_width(area.width);
        let form_height = self.form.height_for_width(area.width);
        let [transcript, form, gap, prompt] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(form_height),
                Constraint::Length(1),
                Constraint::Length(prompt_height),
            ])
            .areas(area);
        let _ = gap;

        let lines = self
            .transcript
            .iter()
            .map(|line| {
                Line::from(Span::styled(
                    line.clone(),
                    Style::new().fg(self.palette.text),
                ))
            })
            .collect::<Vec<_>>();
        let skip = lines
            .len()
            .saturating_sub(usize::from(transcript.height.saturating_sub(1)));
        frame.render_widget(
            Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>())
                .wrap(Wrap { trim: false }),
            transcript,
        );
        let help = Line::from(vec![
            Span::styled("Tab", Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(
                " switches boxes · form field uses SubmitMode::Never",
                Style::new().fg(self.palette.muted),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(help),
            Rect {
                y: transcript.bottom().saturating_sub(1),
                height: 1,
                ..transcript
            },
        );

        frame.render_widget(self.form.widget(), form);
        frame.render_widget(self.prompt.widget(), prompt);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = run(&mut terminal);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<(), Box<dyn Error>> {
    let mut app = App::new();
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|frame| app.render(frame))?;
        let timeout = TICK.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if !app.key(key) {
                        return Ok(());
                    }
                }
                Event::Mouse(mouse) => app.mouse(mouse),
                Event::Paste(text) => {
                    app.focused_box().handle(TextBoxAction::InsertText(text));
                }
                _ => {}
            }
        }
        if last_tick.elapsed() >= TICK {
            app.tick();
            last_tick = Instant::now();
        }
    }
}
