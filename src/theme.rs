//! Shared look & feel: light and dark semantic palettes, terminal theme
//! detection, and the small key/hint idioms. Plain Ratatui — no SDK.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;

/// User preference from `config.toml`. Auto queries the terminal at startup.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    Auto,
    Light,
    Dark,
}

impl ThemePreference {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Adaptive,
    Light,
    Dark,
}

/// Every rendered color has a semantic role so the whole interface changes
/// together instead of leaving dark-only islands in a light terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub mode: ThemeMode,
    pub primary: Color,
    pub muted: Color,
    pub header: Color,
    pub focus: Color,
    pub attention: Color,
    pub warning: Color,
    pub claude_accent: Color,
    pub codex_accent: Color,
    pub track: Color,
    pub surface_edge: Color,
    pub meter_blue: Color,
}

impl Palette {
    /// Detection-free palette: default terminal foreground/background plus
    /// terminal-defined ANSI accents. This follows the host theme by design.
    pub const ADAPTIVE: Self = Self {
        mode: ThemeMode::Adaptive,
        primary: Color::Reset,
        muted: Color::DarkGray,
        header: Color::Reset,
        focus: Color::Magenta,
        attention: Color::Red,
        warning: Color::Yellow,
        claude_accent: Color::Red,
        codex_accent: Color::Cyan,
        track: Color::DarkGray,
        surface_edge: Color::DarkGray,
        meter_blue: Color::Blue,
    };

    pub const DARK: Self = Self {
        mode: ThemeMode::Dark,
        primary: Color::Rgb(235, 238, 245),
        muted: Color::Rgb(150, 156, 170),
        header: Color::Rgb(196, 202, 216),
        focus: Color::Rgb(156, 147, 184),
        attention: Color::Rgb(239, 105, 105),
        warning: Color::Rgb(245, 185, 66),
        claude_accent: Color::Rgb(217, 119, 87),
        codex_accent: Color::Rgb(94, 190, 160),
        track: Color::Rgb(58, 62, 74),
        surface_edge: Color::Rgb(53, 56, 66),
        meter_blue: Color::Rgb(64, 132, 255),
    };

    pub const LIGHT: Self = Self {
        mode: ThemeMode::Light,
        primary: Color::Rgb(29, 32, 40),
        muted: Color::Rgb(100, 107, 122),
        header: Color::Rgb(75, 82, 98),
        focus: Color::Rgb(104, 88, 153),
        attention: Color::Rgb(190, 55, 54),
        warning: Color::Rgb(168, 105, 0),
        claude_accent: Color::Rgb(183, 76, 45),
        codex_accent: Color::Rgb(35, 126, 97),
        track: Color::Rgb(207, 212, 221),
        surface_edge: Color::Rgb(201, 206, 216),
        meter_blue: Color::Rgb(43, 103, 224),
    };

    const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Adaptive => Self::ADAPTIVE,
            ThemeMode::Light => Self::LIGHT,
            ThemeMode::Dark => Self::DARK,
        }
    }

    /// Cycle adaptive → light → dark for a session-level manual correction.
    pub const fn toggled(self) -> Self {
        match self.mode {
            ThemeMode::Adaptive => Self::LIGHT,
            ThemeMode::Light => Self::DARK,
            ThemeMode::Dark => Self::ADAPTIVE,
        }
    }
}

/// Resolve the configured palette. `UNPEEL_USAGE_THEME=auto|light|dark` is a
/// convenient per-launch override and takes precedence over `config.toml`.
pub fn resolve(configured: ThemePreference) -> Palette {
    let preference = std::env::var("UNPEEL_USAGE_THEME")
        .ok()
        .and_then(|value| ThemePreference::parse(&value))
        .unwrap_or(configured);
    let mode = match preference {
        ThemePreference::Light => ThemeMode::Light,
        ThemePreference::Dark => ThemeMode::Dark,
        ThemePreference::Auto => detect_terminal_mode().unwrap_or(ThemeMode::Adaptive),
    };
    Palette::for_mode(mode)
}

fn detect_terminal_mode() -> Option<ThemeMode> {
    if let Some(mode) = osc11_background_mode() {
        return Some(mode);
    }
    // Unpeel stamps COLORFGBG when a shell starts, so it becomes stale when
    // the app appearance changes around that running shell. Its terminal
    // defaults are reliable even when OSC 11 is unavailable.
    if std::env::var("TERM_PROGRAM").is_ok_and(|program| program.eq_ignore_ascii_case("unpeel")) {
        return None;
    }
    colorfgbg_mode()
}

/// Ask the active terminal for its current default background. This must run
/// after Ratatui has enabled raw mode so the response reaches stdin
/// immediately. Reading the fd directly also avoids crossterm interpreting
/// the OSC response as a key event.
#[cfg(unix)]
fn osc11_background_mode() -> Option<ThemeMode> {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    use std::time::{Duration, Instant};

    let mut output = std::io::stdout();
    output.write_all(b"\x1b]11;?\x1b\\").ok()?;
    output.flush().ok()?;

    let fd = std::io::stdin().as_raw_fd();
    let deadline = Instant::now() + Duration::from_millis(200);
    let mut response = Vec::new();
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = remaining.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if ready <= 0 {
            return None;
        }
        let mut chunk = [0u8; 64];
        let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if read <= 0 {
            return None;
        }
        response.extend_from_slice(&chunk[..read as usize]);
        if response.contains(&0x07) || response.windows(2).any(|pair| pair == b"\x1b\\") {
            break;
        }
        if response.len() > 256 {
            return None;
        }
    }
    parse_osc11_mode(&response)
}

#[cfg(not(unix))]
fn osc11_background_mode() -> Option<ThemeMode> {
    None
}

fn parse_osc11_mode(response: &[u8]) -> Option<ThemeMode> {
    let text = String::from_utf8_lossy(response);
    let mut channels = text.split("rgb:").nth(1)?.split('/').take(3).map(|part| {
        let hex: String = part.chars().take_while(char::is_ascii_hexdigit).collect();
        let value = u32::from_str_radix(&hex, 16).ok()?;
        let max = 16u32.checked_pow(hex.len() as u32)? - 1;
        Some(value as f32 / max.max(1) as f32)
    });
    let red = channels.next()??;
    let green = channels.next()??;
    let blue = channels.next()??;
    let luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
    Some(if luminance > 0.55 {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    })
}

/// `COLORFGBG` is a useful fallback in terminals that expose their palette as
/// ANSI indices but do not answer OSC 10/11 queries.
fn colorfgbg_mode() -> Option<ThemeMode> {
    let background = std::env::var("COLORFGBG")
        .ok()?
        .split([';', ':'])
        .next_back()?
        .trim()
        .parse::<u8>()
        .ok()?;
    Some(if ansi_color_is_light(background) {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    })
}

fn ansi_color_is_light(index: u8) -> bool {
    let (red, green, blue) = match index {
        0..=15 => {
            const ANSI: [(u8, u8, u8); 16] = [
                (0, 0, 0),
                (128, 0, 0),
                (0, 128, 0),
                (128, 128, 0),
                (0, 0, 128),
                (128, 0, 128),
                (0, 128, 128),
                (192, 192, 192),
                (128, 128, 128),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (0, 0, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ];
            ANSI[index as usize]
        }
        16..=231 => {
            let cube = index - 16;
            let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (
                channel(cube / 36),
                channel((cube % 36) / 6),
                channel(cube % 6),
            )
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    };
    // Integer approximation of relative luminance; 150 separates the common
    // light terminal backgrounds from dark and mid-tone palettes.
    (u16::from(red) * 3 + u16::from(green) * 6 + u16::from(blue)) / 10 >= 150
}

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

/// Minimal two-cell-inset footer hint idiom shared by list Apps.
pub fn hint_line(palette: &Palette, hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(palette.muted)));
        }
        spans.push(Span::styled(
            key.to_string(),
            Style::default().fg(palette.header),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(palette.muted),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_preferences_are_case_insensitive() {
        assert_eq!(ThemePreference::parse("AUTO"), Some(ThemePreference::Auto));
        assert_eq!(
            ThemePreference::parse(" light "),
            Some(ThemePreference::Light)
        );
        assert_eq!(ThemePreference::parse("dark"), Some(ThemePreference::Dark));
        assert_eq!(ThemePreference::parse("sepia"), None);
    }

    #[test]
    fn ansi_background_luminance_handles_light_and_dark_colors() {
        for index in [0, 1, 4, 16, 232] {
            assert!(!ansi_color_is_light(index), "index {index} should be dark");
        }
        for index in [7, 11, 14, 15, 231, 255] {
            assert!(ansi_color_is_light(index), "index {index} should be light");
        }
    }

    #[test]
    fn parses_current_background_from_osc11_responses() {
        assert_eq!(
            parse_osc11_mode(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some(ThemeMode::Light)
        );
        assert_eq!(
            parse_osc11_mode(b"\x1b]11;rgb:2828/2c2c/3434\x07"),
            Some(ThemeMode::Dark)
        );
        assert_eq!(parse_osc11_mode(b"not a color"), None);
    }

    #[test]
    fn palettes_cover_every_semantic_color() {
        assert_eq!(Palette::ADAPTIVE.mode, ThemeMode::Adaptive);
        assert_eq!(Palette::LIGHT.mode, ThemeMode::Light);
        assert_eq!(Palette::DARK.mode, ThemeMode::Dark);
        assert_ne!(Palette::LIGHT.primary, Palette::DARK.primary);
        assert_ne!(Palette::LIGHT.track, Palette::DARK.track);
        assert_eq!(Palette::ADAPTIVE.primary, Color::Reset);
        assert_eq!(Palette::ADAPTIVE.toggled(), Palette::LIGHT);
        assert_eq!(Palette::LIGHT.toggled(), Palette::DARK);
        assert_eq!(Palette::DARK.toggled(), Palette::ADAPTIVE);
    }
}
