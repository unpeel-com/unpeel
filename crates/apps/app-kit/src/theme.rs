use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use ratatui::style::{Color, Style};
use serde::Deserialize;

/// Standard content inset for full-width selectable rows.
pub const SELECTABLE_LEFT_PADDING: u16 = 2;

/// Host-provided accent for the current App Session. Unpeel sets this to a
/// canonical `#RRGGBB` value from the Session's project folder color, falling
/// back to the workspace App color.
pub const APP_ACCENT_ENV: &str = "UNPEEL_APP_ACCENT";
const LIVE_THEME_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Terminal color scheme used to choose a kit palette.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorScheme {
    #[default]
    Dark,
    Light,
}

impl ColorScheme {
    /// Detects a scheme from `UNPEEL_TUI_THEME=dark|light`, then the common
    /// `COLORFGBG` terminal hint. Detection deliberately falls back to dark;
    /// Apps with their own appearance setting should pass it explicitly.
    #[must_use]
    pub fn detect() -> Self {
        std::env::var("UNPEEL_TUI_THEME")
            .ok()
            .as_deref()
            .and_then(parse_name)
            .or_else(|| {
                std::env::var("COLORFGBG")
                    .ok()
                    .as_deref()
                    .and_then(parse_colorfgbg)
            })
            .unwrap_or_default()
    }
}

/// Small shared palette for Ratatui Apps that do not own a product theme.
///
/// Backgrounds remain transparent except for selected rows. This keeps Apps
/// native to the surrounding terminal while ensuring selection contrast in
/// both dark and light terminals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KitTheme {
    pub scheme: ColorScheme,
    pub text: Color,
    pub muted: Color,
    pub subtle: Color,
    pub accent: Color,
    pub surface: Color,
    pub danger: Color,
    pub selected_row: Style,
    /// Pointer hover on a selectable row: lighter than the selection so the
    /// two never read as the same state.
    pub hovered_row: Style,
    pub scrollbar_track: Style,
    pub scrollbar_thumb: Style,
}

impl KitTheme {
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            scheme: ColorScheme::Dark,
            text: Color::White,
            muted: Color::Gray,
            subtle: Color::DarkGray,
            accent: Color::LightBlue,
            surface: Color::Rgb(39, 39, 42),
            danger: Color::LightRed,
            selected_row: Style::new().fg(Color::White).bg(Color::Rgb(63, 63, 70)),
            hovered_row: Style::new().fg(Color::White).bg(Color::Rgb(46, 46, 52)),
            scrollbar_track: Style::new().fg(Color::Rgb(65, 65, 65)),
            scrollbar_thumb: Style::new().fg(Color::Rgb(145, 145, 145)),
        }
    }

    #[must_use]
    pub const fn light() -> Self {
        Self {
            scheme: ColorScheme::Light,
            text: Color::Black,
            muted: Color::DarkGray,
            subtle: Color::Gray,
            accent: Color::Blue,
            surface: Color::Rgb(238, 238, 240),
            danger: Color::Red,
            selected_row: Style::new().fg(Color::Black).bg(Color::Rgb(216, 216, 220)),
            hovered_row: Style::new().fg(Color::Black).bg(Color::Rgb(236, 236, 240)),
            scrollbar_track: Style::new().fg(Color::Rgb(200, 200, 200)),
            scrollbar_thumb: Style::new().fg(Color::Rgb(100, 100, 100)),
        }
    }

    #[must_use]
    pub const fn for_scheme(scheme: ColorScheme) -> Self {
        match scheme {
            ColorScheme::Dark => Self::dark(),
            ColorScheme::Light => Self::light(),
        }
    }

    #[must_use]
    pub fn detected() -> Self {
        let scheme = ColorScheme::detect();
        let mut theme = Self::for_scheme(scheme);
        if let Some(accent) = hosted_accent_for_scheme(scheme) {
            theme.accent = accent;
        }
        theme
    }
}

impl Default for KitTheme {
    fn default() -> Self {
        Self::dark()
    }
}

fn parse_name(value: &str) -> Option<ColorScheme> {
    match value.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(ColorScheme::Dark),
        "light" => Some(ColorScheme::Light),
        _ => None,
    }
}

fn parse_colorfgbg(value: &str) -> Option<ColorScheme> {
    let background = value
        .split([';', ':'])
        .next_back()?
        .trim()
        .parse::<u8>()
        .ok()?;
    Some(if matches!(background, 0..=6 | 8) {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    })
}

/// Returns the accent supplied by an Unpeel Host for this App Session.
///
/// The environment value is deliberately ignored unless a non-empty
/// `UNPEEL_SESSION_ID` is present. This keeps standalone runs visually and
/// behaviorally independent even if a parent shell happens to retain an old
/// `UNPEEL_APP_ACCENT` value.
#[must_use]
pub fn hosted_accent() -> Option<Color> {
    hosted_accent_for_scheme(ColorScheme::detect())
}

/// Returns the live Host accent for an explicit light/dark scheme.
///
/// Current Hosts answer a loopback App-theme query, allowing an already-open
/// TUI to repaint when its project/workspace color changes. The launch-time
/// environment value remains the compatibility fallback for older Hosts.
#[must_use]
pub fn hosted_accent_for_scheme(scheme: ColorScheme) -> Option<Color> {
    let session_id = std::env::var("UNPEEL_SESSION_ID").ok()?;
    if !valid_session_id(&session_id) {
        return None;
    }
    if let Some(port) = std::env::var("UNPEEL_APP_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        && let Some(response) = query_hosted_accent(port, &session_id, scheme)
    {
        return response;
    }
    hosted_accent_from(
        Some(&session_id),
        std::env::var(APP_ACCENT_ENV).ok().as_deref(),
    )
}

fn hosted_accent_from(session_id: Option<&str>, accent: Option<&str>) -> Option<Color> {
    session_id
        .filter(|value| !value.trim().is_empty())
        .and(accent)
        .and_then(parse_rgb_hex)
}

fn parse_rgb_hex(value: &str) -> Option<Color> {
    let value = value.trim();
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let red = u8::from_str_radix(&digits[0..2], 16).ok()?;
    let green = u8::from_str_radix(&digits[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&digits[4..6], 16).ok()?;
    Some(Color::Rgb(red, green, blue))
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[derive(Deserialize)]
struct HostedThemeResponse {
    accent: Option<String>,
}

/// Outer `Option` means a compatible Host answered; the inner value is the
/// current override and may intentionally be `None` after the user clears a
/// color. That distinction prevents a stale launch environment from winning.
fn query_hosted_accent(port: u16, session_id: &str, scheme: ColorScheme) -> Option<Option<Color>> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(300)))
        .ok()?;
    let scheme = match scheme {
        ColorScheme::Dark => "dark",
        ColorScheme::Light => "light",
    };
    let body = serde_json::to_vec(&serde_json::json!({ "scheme": scheme })).ok()?;
    write!(
        stream,
        "POST /app-theme/{session_id} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .ok()?;
    stream.write_all(&body).ok()?;
    stream.shutdown(Shutdown::Write).ok()?;

    let mut response = String::new();
    stream.take(8 * 1024).read_to_string(&mut response).ok()?;
    if response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        != Some("200")
    {
        return None;
    }
    let body = response.split_once("\r\n\r\n")?.1;
    let response: HostedThemeResponse = serde_json::from_str(body).ok()?;
    Some(response.accent.as_deref().and_then(parse_rgb_hex))
}

/// Throttled live-theme detector for App event loops.
///
/// Call [`ThemeMonitor::refresh`] from the loop's ordinary idle/tick branch;
/// when it returns true, redraw using [`ThemeMonitor::theme`]. The monitor
/// queries at most twice per second and is inert outside a hosted Session.
#[derive(Debug)]
pub struct ThemeMonitor {
    theme: KitTheme,
    hosted_accent: Option<Color>,
    last_refresh: Instant,
    refresh_interval: Duration,
}

impl ThemeMonitor {
    #[must_use]
    pub fn detected() -> Self {
        let scheme = ColorScheme::detect();
        let hosted_accent = hosted_accent_for_scheme(scheme);
        let mut theme = KitTheme::for_scheme(scheme);
        if let Some(accent) = hosted_accent {
            theme.accent = accent;
        }
        Self {
            theme,
            hosted_accent,
            last_refresh: Instant::now(),
            refresh_interval: LIVE_THEME_REFRESH_INTERVAL,
        }
    }

    /// Start from an App-owned palette, then adopt live Host changes on the
    /// next refresh. Useful when construction already resolved a theme once.
    #[must_use]
    pub fn from_theme(theme: KitTheme) -> Self {
        Self {
            theme,
            hosted_accent: None,
            last_refresh: Instant::now(),
            refresh_interval: LIVE_THEME_REFRESH_INTERVAL,
        }
    }

    #[must_use]
    pub const fn theme(&self) -> KitTheme {
        self.theme
    }

    #[must_use]
    pub const fn hosted_accent(&self) -> Option<Color> {
        self.hosted_accent
    }

    /// Refresh the Host value when the throttle interval has elapsed.
    /// Returns true when either the scheme or hosted accent changed.
    pub fn refresh(&mut self) -> bool {
        if self.last_refresh.elapsed() < self.refresh_interval {
            return false;
        }
        self.refresh_now()
    }

    /// Refresh immediately. Useful after an explicit appearance/state-change
    /// signal; ordinary event loops should prefer [`Self::refresh`].
    pub fn refresh_now(&mut self) -> bool {
        self.last_refresh = Instant::now();
        let scheme = ColorScheme::detect();
        let hosted_accent = hosted_accent_for_scheme(scheme);
        let mut theme = KitTheme::for_scheme(scheme);
        if let Some(accent) = hosted_accent {
            theme.accent = accent;
        }
        let changed = theme != self.theme || hosted_accent != self.hosted_accent;
        self.theme = theme;
        self.hosted_accent = hosted_accent;
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn common_colorfgbg_values_map_to_expected_scheme() {
        assert_eq!(parse_colorfgbg("15;0"), Some(ColorScheme::Dark));
        assert_eq!(parse_colorfgbg("0;15"), Some(ColorScheme::Light));
        assert_eq!(parse_colorfgbg("0:7"), Some(ColorScheme::Light));
        assert_eq!(parse_colorfgbg("unknown"), None);
    }

    #[test]
    fn selected_rows_have_contrasting_full_palette_defaults() {
        assert_eq!(KitTheme::dark().selected_row.fg, Some(Color::White));
        assert_eq!(KitTheme::light().selected_row.fg, Some(Color::Black));
        assert_ne!(
            KitTheme::dark().selected_row.bg,
            KitTheme::light().selected_row.bg
        );
    }

    #[test]
    fn hosted_accent_requires_a_session_and_valid_rgb_hex() {
        assert_eq!(
            hosted_accent_from(Some("session"), Some("#4EC3C9")),
            Some(Color::Rgb(78, 195, 201))
        );
        assert_eq!(
            hosted_accent_from(Some("session"), Some("d97757")),
            Some(Color::Rgb(217, 119, 87))
        );
        assert_eq!(hosted_accent_from(None, Some("#4EC3C9")), None);
        assert_eq!(hosted_accent_from(Some("  "), Some("#4EC3C9")), None);
        assert_eq!(hosted_accent_from(Some("session"), Some("teal")), None);
        assert_eq!(hosted_accent_from(Some("session"), Some("#12345678")), None);
    }

    #[test]
    fn explorer_palette_can_preserve_the_detected_accent() {
        let mut palette = KitTheme::dark();
        palette.accent = Color::Rgb(177, 102, 232);
        let explorer = crate::ExplorerTheme::for_theme(palette);
        assert_eq!(explorer.directory.fg, Some(palette.accent));
        assert_eq!(explorer.parent.fg, Some(palette.accent));
    }

    #[test]
    fn live_theme_query_distinguishes_a_host_answer() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            assert!(request.starts_with("POST /app-theme/session-1 HTTP/1.1\r\n"));
            assert!(request.ends_with(r#"{"scheme":"dark"}"#));
            let body = r##"{"accent":"#7DD3FC"}"##;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });

        assert_eq!(
            query_hosted_accent(port, "session-1", ColorScheme::Dark),
            Some(Some(Color::Rgb(125, 211, 252)))
        );
        server.join().unwrap();
    }
}
