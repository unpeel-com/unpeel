//! Terminal-only visual contract for selectable App Kit rows.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::{KitTheme, SELECTABLE_LEFT_PADDING};

/// Paints a full-width selected/hovered row and returns its content rectangle.
///
/// Apps with richer rows than [`crate::ListItem`] can share the same visual
/// language without copying color or inset constants. Pass `selected ||
/// hovered` as `active`; inactive rows remain transparent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectableRow {
    active: bool,
    active_style: Style,
    inactive_style: Style,
    left_padding: u16,
    right_padding: u16,
}

impl SelectableRow {
    /// Uses an explicit active style, useful for Apps with adaptive palettes.
    #[must_use]
    pub const fn new(active: bool, active_style: Style) -> Self {
        Self {
            active,
            active_style,
            inactive_style: Style::new(),
            left_padding: SELECTABLE_LEFT_PADDING,
            right_padding: 1,
        }
    }

    /// Uses App Kit's gray row treatment for the selected color scheme.
    #[must_use]
    pub const fn for_theme(active: bool, theme: KitTheme) -> Self {
        Self::new(active, theme.selected_row)
    }

    #[must_use]
    pub const fn inactive_style(mut self, style: Style) -> Self {
        self.inactive_style = style;
        self
    }

    /// Overrides the standard two-cell inset for a compatibility surface.
    /// New List rows should keep the default; Explorer uses its existing
    /// theme value while sharing this exact painter.
    #[must_use]
    pub const fn left_padding(mut self, columns: u16) -> Self {
        self.left_padding = columns;
        self
    }

    #[must_use]
    pub const fn right_padding(mut self, columns: u16) -> Self {
        self.right_padding = columns;
        self
    }

    /// Paints the complete row before returning the two-cell-inset content.
    pub fn paint(self, area: Rect, buffer: &mut Buffer) -> Rect {
        if area.is_empty() {
            return area;
        }
        buffer.set_style(
            area,
            if self.active {
                self.active_style
            } else {
                self.inactive_style
            },
        );
        let left = self.left_padding.min(area.width);
        let remaining = area.width.saturating_sub(left);
        let right = self.right_padding.min(remaining);
        Rect::new(
            area.x.saturating_add(left),
            area.y,
            remaining.saturating_sub(right),
            area.height,
        )
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    #[test]
    fn active_row_is_full_width_with_an_exact_two_cell_leading_inset() {
        let area = Rect::new(3, 2, 12, 1);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        let content = SelectableRow::for_theme(true, KitTheme::dark()).paint(area, &mut buffer);

        assert_eq!(content, Rect::new(5, 2, 9, 1));
        for x in area.x..area.right() {
            assert_eq!(buffer[(x, area.y)].bg, Color::Rgb(63, 63, 70));
        }
    }
}
