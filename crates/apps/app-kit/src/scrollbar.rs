use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget};

/// A capless, proportional vertical scrollbar for row-based Ratatui views.
///
/// Ratatui's `ScrollbarState` expects a subtly different content length when
/// `viewport_content_length` is set: the number of valid scroll positions,
/// not the total number of rows. This component owns that conversion so the
/// thumb reaches the exact first and last track cells in every App.
#[derive(Clone, Debug)]
pub struct VerticalScrollbar<'a> {
    content_rows: usize,
    viewport_rows: usize,
    position: usize,
    track_symbol: Option<&'a str>,
    thumb_symbol: &'a str,
    track_style: Style,
    thumb_style: Style,
}

impl VerticalScrollbar<'static> {
    /// Creates a scrollbar for a row-based viewport.
    ///
    /// `content_rows` is the complete virtual row count, `viewport_rows` is
    /// the number currently visible, and `position` is the requested top row.
    #[must_use]
    pub const fn new(content_rows: usize, viewport_rows: usize, position: usize) -> Self {
        Self {
            content_rows,
            viewport_rows,
            position,
            track_symbol: Some("│"),
            thumb_symbol: "┃",
            track_style: Style::new(),
            thumb_style: Style::new(),
        }
    }
}

impl<'a> VerticalScrollbar<'a> {
    /// Whether the content overflows a non-empty viewport.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.viewport_rows > 0 && self.content_rows > self.viewport_rows
    }

    /// The greatest valid top-row offset.
    #[must_use]
    pub const fn max_position(&self) -> usize {
        self.content_rows.saturating_sub(self.viewport_rows)
    }

    /// The requested position clamped to [`Self::max_position`].
    #[must_use]
    pub const fn position(&self) -> usize {
        if self.position < self.max_position() {
            self.position
        } else {
            self.max_position()
        }
    }

    /// Number of valid top-row positions represented by the thumb.
    #[must_use]
    pub const fn scroll_position_count(&self) -> Option<usize> {
        if self.is_visible() {
            Some(self.max_position().saturating_add(1))
        } else {
            None
        }
    }

    /// Sets the track glyph. `None` leaves unused track cells untouched.
    #[must_use]
    pub const fn track_symbol(mut self, symbol: Option<&'a str>) -> Self {
        self.track_symbol = symbol;
        self
    }

    /// Sets the thumb glyph.
    #[must_use]
    pub const fn thumb_symbol(mut self, symbol: &'a str) -> Self {
        self.thumb_symbol = symbol;
        self
    }

    /// Sets the track style.
    #[must_use]
    pub const fn track_style(mut self, style: Style) -> Self {
        self.track_style = style;
        self
    }

    /// Sets the thumb style.
    #[must_use]
    pub const fn thumb_style(mut self, style: Style) -> Self {
        self.thumb_style = style;
        self
    }
}

impl Widget for VerticalScrollbar<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some(position_count) = self.scroll_position_count() else {
            return;
        };
        if area.is_empty() {
            return;
        }

        let mut state = ScrollbarState::new(position_count)
            .position(self.position())
            .viewport_content_length(self.viewport_rows);
        StatefulWidget::render(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(self.track_symbol)
                .thumb_symbol(self.thumb_symbol)
                .track_style(self.track_style)
                .thumb_style(self.thumb_style),
            area,
            buffer,
            &mut state,
        );
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    #[test]
    fn metrics_are_based_on_valid_scroll_positions() {
        assert!(!VerticalScrollbar::new(10, 10, 0).is_visible());
        assert!(!VerticalScrollbar::new(10, 20, 0).is_visible());
        assert_eq!(
            VerticalScrollbar::new(12, 10, 0).scroll_position_count(),
            Some(3)
        );
        assert_eq!(VerticalScrollbar::new(12, 10, usize::MAX).position(), 2);
    }

    #[test]
    fn thumb_reaches_exact_track_ends_and_is_proportional() {
        let area = Rect::new(0, 0, 1, 10);

        let mut top = Buffer::empty(area);
        VerticalScrollbar::new(20, 10, 0).render(area, &mut top);
        assert_eq!(top[(0, 0)].symbol(), "┃");

        let mut bottom = Buffer::empty(area);
        VerticalScrollbar::new(20, 10, usize::MAX).render(area, &mut bottom);
        assert_eq!(bottom[(0, 9)].symbol(), "┃");
        let thumb_rows = (0..10)
            .filter(|row| bottom[(0, *row)].symbol() == "┃")
            .count();
        assert_eq!(thumb_rows, 5);
    }

    #[test]
    fn styles_and_symbols_are_configurable() {
        let area = Rect::new(0, 0, 1, 4);
        let mut buffer = Buffer::empty(area);
        VerticalScrollbar::new(8, 4, 0)
            .track_symbol(Some("·"))
            .thumb_symbol("█")
            .track_style(Style::new().fg(Color::Blue))
            .thumb_style(Style::new().fg(Color::Red))
            .render(area, &mut buffer);

        assert_eq!(buffer[(0, 0)].symbol(), "█");
        assert_eq!(buffer[(0, 0)].fg, Color::Red);
        assert_eq!(buffer[(0, 3)].symbol(), "·");
        assert_eq!(buffer[(0, 3)].fg, Color::Blue);
    }

    #[test]
    fn fitting_content_renders_nothing() {
        let area = Rect::new(0, 0, 1, 4);
        let mut buffer = Buffer::empty(area);
        VerticalScrollbar::new(4, 4, 0).render(area, &mut buffer);
        assert!((0..4).all(|row| buffer[(0, row)].symbol() == " "));
    }
}
