use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::Line;
use ratatui::widgets::Widget;

use crate::DragSurface;

/// A one-line Ratatui path component whose visible label initiates a native
/// file or directory drag in Unpeel.
///
/// The hit region follows the rendered content width and honors left, center,
/// and right alignment. Styling belongs on the supplied [`Line`].
pub struct DraggablePath<'content, 'surface> {
    surface: &'surface mut DragSurface,
    path: PathBuf,
    label: Line<'content>,
}

impl<'content, 'surface> DraggablePath<'content, 'surface> {
    #[must_use]
    pub fn new(
        surface: &'surface mut DragSurface,
        path: impl AsRef<Path>,
        label: impl Into<Line<'content>>,
    ) -> Self {
        Self {
            surface,
            path: path.as_ref().to_path_buf(),
            label: label.into(),
        }
    }
}

impl Widget for DraggablePath<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let content_width = u16::try_from(self.label.width())
            .unwrap_or(u16::MAX)
            .min(area.width);
        let x = match self.label.alignment.unwrap_or(Alignment::Left) {
            Alignment::Left => area.x,
            Alignment::Center => area
                .x
                .saturating_add(area.width.saturating_sub(content_width) / 2),
            Alignment::Right => area
                .x
                .saturating_add(area.width.saturating_sub(content_width)),
        };
        let hit_area = Rect::new(x, area.y, content_width, area.height.min(1));
        self.surface.register(hit_area, &self.path);
        self.label.render(area, buffer);
    }
}

/// Wraps any Ratatui widget and makes its complete rectangle a path drag
/// source. Use [`DraggablePath`] when only a single label should be draggable.
pub struct DragSource<'surface, W> {
    surface: &'surface mut DragSurface,
    path: PathBuf,
    widget: W,
}

impl<'surface, W> DragSource<'surface, W> {
    #[must_use]
    pub fn new(surface: &'surface mut DragSurface, path: impl AsRef<Path>, widget: W) -> Self {
        Self {
            surface,
            path: path.as_ref().to_path_buf(),
            widget,
        }
    }
}

impl<W: Widget> Widget for DragSource<'_, W> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.surface.register(area, &self.path);
        self.widget.render(area, buffer);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Stylize};

    use super::*;

    #[test]
    fn path_component_registers_only_its_aligned_visible_label() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 2));
        let mut surface = DragSurface::disabled();
        surface.begin_frame();

        DraggablePath::new(
            &mut surface,
            "/tmp/report.csv",
            Line::from("report.csv").yellow().right_aligned(),
        )
        .render(Rect::new(2, 1, 20, 1), &mut buffer);

        assert_eq!(surface.regions().len(), 1);
        assert_eq!(surface.regions()[0].area, Rect::new(12, 1, 10, 1));
        assert_eq!(surface.regions()[0].path, Path::new("/tmp/report.csv"));
        assert_eq!(buffer[(12, 1)].fg, Color::Yellow);
    }

    #[test]
    fn generic_source_registers_the_full_widget_rectangle() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 4));
        let mut surface = DragSurface::disabled();
        surface.begin_frame();

        DragSource::new(&mut surface, "/tmp/folder", Line::from("folder"))
            .render(Rect::new(2, 1, 12, 2), &mut buffer);

        assert_eq!(surface.regions()[0].area, Rect::new(2, 1, 12, 2));
    }
}
