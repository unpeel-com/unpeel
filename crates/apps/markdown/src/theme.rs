//! Markdown-specific colors derived from the shared Unpeel App Kit palette.

use ratatui::style::Color;
use unpeel_app_kit::{ColorScheme, KitTheme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub kit: KitTheme,
    pub strong: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub cursor_line: Color,
    pub cursor: Color,
    pub cursor_text: Color,
}

impl Theme {
    pub fn detect() -> Self {
        Self::from_kit(KitTheme::detected())
    }

    #[cfg(test)]
    pub const fn dark() -> Self {
        Self::from_kit(KitTheme::dark())
    }

    #[cfg(test)]
    pub const fn light() -> Self {
        Self::from_kit(KitTheme::light())
    }

    pub(crate) const fn from_kit(kit: KitTheme) -> Self {
        let (cursor_line, cursor_text) = match kit.scheme {
            ColorScheme::Dark => (Color::Rgb(28, 28, 28), Color::Black),
            ColorScheme::Light => (Color::Rgb(244, 244, 245), Color::White),
        };
        Self {
            kit,
            strong: kit.text,
            text: kit.text,
            muted: kit.muted,
            faint: kit.subtle,
            accent: kit.accent,
            cursor_line,
            cursor: kit.muted,
            cursor_text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_palettes_are_distinct() {
        assert_ne!(Theme::light(), Theme::dark());
        assert_eq!(Theme::light().kit, KitTheme::light());
        assert_eq!(Theme::dark().kit, KitTheme::dark());
        assert_eq!(Theme::light().strong, KitTheme::light().text);
        assert_eq!(Theme::dark().strong, KitTheme::dark().text);
    }
}
