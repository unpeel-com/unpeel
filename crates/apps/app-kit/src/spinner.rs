//! Shared braille activity spinner used by busy rows, footers, and prompts.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

/// The ten-frame braille cycle every App Kit component animates with.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A frame counter for the shared braille spinner.
///
/// Apps advance it from their own tick (around every 80 to 120 ms) and hand
/// the frame to whichever component is busy: `ListState::set_spinner_frame`,
/// `FooterActionsWidget::spinner_frame`, or [`Spinner::widget`] directly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Spinner {
    frame: usize,
}

impl Spinner {
    #[must_use]
    pub const fn new() -> Self {
        Self { frame: 0 }
    }

    /// Advances to the next frame and returns it.
    pub const fn tick(&mut self) -> usize {
        self.frame = (self.frame + 1) % SPINNER_FRAMES.len();
        self.frame
    }

    #[must_use]
    pub const fn frame(&self) -> usize {
        self.frame
    }

    pub const fn set_frame(&mut self, frame: usize) {
        self.frame = frame % SPINNER_FRAMES.len();
    }

    /// The glyph for the current frame.
    #[must_use]
    pub const fn glyph(&self) -> &'static str {
        Self::glyph_for(self.frame)
    }

    /// The glyph for any frame counter, wrapping around the cycle.
    #[must_use]
    pub const fn glyph_for(frame: usize) -> &'static str {
        SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
    }

    #[must_use]
    pub const fn widget(&self) -> SpinnerWidget {
        SpinnerWidget {
            frame: self.frame,
            style: Style::new(),
        }
    }
}

/// Renders one spinner glyph in the top-left cell of its area.
#[derive(Clone, Copy, Debug)]
pub struct SpinnerWidget {
    frame: usize,
    style: Style,
}

impl SpinnerWidget {
    #[must_use]
    pub const fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }
}

impl Widget for SpinnerWidget {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        buffer.set_string(area.x, area.y, Spinner::glyph_for(self.frame), self.style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_cycles_through_the_shared_braille_frames() {
        let mut spinner = Spinner::new();
        assert_eq!(spinner.glyph(), "⠋");
        assert_eq!(spinner.tick(), 1);
        assert_eq!(spinner.glyph(), "⠙");
        for _ in 0..9 {
            spinner.tick();
        }
        assert_eq!(spinner.frame(), 0);
        assert_eq!(Spinner::glyph_for(23), "⠸");
        let area = Rect::new(0, 0, 3, 1);
        let mut buffer = Buffer::empty(area);
        spinner.widget().render(area, &mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "⠋");
    }
}
