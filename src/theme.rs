//! Shared look & feel: the same color vocabulary Unpeel's own sidebar uses,
//! plus the small key/hint idioms. Plain ratatui — no SDK.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Secondary text: muted but legible.
pub const MUTED: Color = Color::Rgb(150, 156, 170);
/// Headers and structure, one shade under body text.
pub const HEADER: Color = Color::Rgb(196, 202, 216);
/// Focus/selection accent (purple-gray, terminal-palette independent).
pub const FOCUS: Color = Color::Rgb(156, 147, 184);
/// Needs-user accent.
pub const ATTENTION: Color = Color::LightRed;
/// Per-provider brand accents: Claude's coral, Codex's teal.
pub const CLAUDE_ACCENT: Color = Color::Rgb(217, 119, 87);
pub const CODEX_ACCENT: Color = Color::Rgb(94, 190, 160);

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Braille spinner keyed to wall time (100ms per frame), so every widget
/// animates in sync.
pub fn spinner_frame() -> &'static str {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    SPINNER_FRAMES[(millis / 100) as usize % SPINNER_FRAMES.len()]
}

/// Footer hint idiom: `j/k select · enter details · q quit`.
pub fn hint_line(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        }
        spans.push(Span::styled(
            key.to_string(),
            Style::default().fg(HEADER),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(MUTED),
        ));
    }
    Line::from(spans)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Top,
    Bottom,
    Select,
    Back,
    Quit,
}

/// The standard list-navigation vocabulary (j/k, g/G, Enter, Esc, q/Ctrl-C).
pub fn nav(key: &KeyEvent) -> Option<Nav> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return matches!(key.code, KeyCode::Char('c')).then_some(Nav::Quit);
    }
    match key.code {
        KeyCode::Char('q') => Some(Nav::Quit),
        KeyCode::Char('j') | KeyCode::Down => Some(Nav::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Nav::Up),
        KeyCode::Char('g') | KeyCode::Home => Some(Nav::Top),
        KeyCode::Char('G') | KeyCode::End => Some(Nav::Bottom),
        KeyCode::Enter => Some(Nav::Select),
        KeyCode::Esc => Some(Nav::Back),
        _ => None,
    }
}
