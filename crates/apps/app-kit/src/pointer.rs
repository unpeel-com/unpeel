//! Renderer-local pointer state shared by App Kit's Ratatui interpreters.
//!
//! Coordinates, hover, and an in-progress press are terminal presentation
//! ephemera. They never enter the semantic component tree; the component spec
//! remains the sole source of action ids, values, and behavior.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

/// Visual pointer phase for one terminal hit region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalPointerPhase {
    #[default]
    Idle,
    Hovered,
    Pressed,
}

/// Last terminal pointer position plus the lifecycle of a left-button press.
///
/// Apps feed every crossterm mouse event to [`Self::track`]. Component widgets
/// then derive hover/press treatment from the same value used for hit-testing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalPointerState {
    position: Option<Position>,
    left_press_origin: Option<Position>,
    left_pressed: bool,
}

impl TerminalPointerState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            position: None,
            left_press_origin: None,
            left_pressed: false,
        }
    }

    #[must_use]
    pub const fn position(self) -> Option<Position> {
        self.position
    }

    #[must_use]
    pub const fn left_pressed(self) -> bool {
        self.left_pressed
    }

    /// Updates hover position for adapters that receive an already-normalized
    /// terminal cell instead of a complete crossterm event.
    pub fn move_to(&mut self, position: Position) -> bool {
        let changed = self.position != Some(position);
        self.position = Some(position);
        changed
    }

    /// Records one crossterm event. Returns whether visible pointer state
    /// changed and therefore warrants a redraw.
    pub fn track(&mut self, event: &MouseEvent) -> bool {
        let previous = *self;
        let position = Position::new(event.column, event.row);
        self.position = Some(position);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.left_press_origin = Some(position);
                self.left_pressed = true;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.left_pressed = true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.left_press_origin = None;
                self.left_pressed = false;
            }
            _ => {}
        }
        previous != *self
    }

    /// Clears stale hover/press state, for example when a pane loses focus.
    pub fn clear(&mut self) -> bool {
        let changed = *self != Self::new();
        *self = Self::new();
        changed
    }

    #[must_use]
    pub fn phase(self, area: Rect) -> TerminalPointerPhase {
        let Some(position) = self.position.filter(|position| area.contains(*position)) else {
            return TerminalPointerPhase::Idle;
        };
        if self.left_pressed
            && self
                .left_press_origin
                .is_some_and(|origin| area.contains(origin))
            && area.contains(position)
        {
            TerminalPointerPhase::Pressed
        } else {
            TerminalPointerPhase::Hovered
        }
    }

    #[must_use]
    pub fn is_hovering(self, area: Rect) -> bool {
        self.phase(area) != TerminalPointerPhase::Idle
    }

    #[must_use]
    pub fn is_pressing(self, area: Rect) -> bool {
        self.phase(area) == TerminalPointerPhase::Pressed
    }

    /// Shared subtle treatment for an action-bearing terminal leaf.
    #[must_use]
    pub fn interaction_style(self, area: Rect, enabled: bool) -> Style {
        if !enabled {
            return Style::new();
        }
        match self.phase(area) {
            TerminalPointerPhase::Idle => Style::new(),
            TerminalPointerPhase::Hovered => Style::new().add_modifier(Modifier::UNDERLINED),
            TerminalPointerPhase::Pressed => Style::new()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
        }
    }

    /// A click activates on the left-button press, matching the existing App
    /// Kit terminal Apps and allowing a pressed frame before button release.
    #[must_use]
    pub const fn click_position(event: &MouseEvent) -> Option<Position> {
        if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            Some(Position::new(event.column, event.row))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyModifiers, MouseEvent};

    use super::*;

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn tracks_hover_press_drag_release_and_clear() {
        let area = Rect::new(2, 3, 5, 1);
        let mut pointer = TerminalPointerState::default();
        pointer.track(&mouse(MouseEventKind::Moved, 3, 3));
        assert_eq!(pointer.phase(area), TerminalPointerPhase::Hovered);

        pointer.track(&mouse(MouseEventKind::Down(MouseButton::Left), 3, 3));
        assert_eq!(pointer.phase(area), TerminalPointerPhase::Pressed);
        pointer.track(&mouse(MouseEventKind::Drag(MouseButton::Left), 10, 3));
        assert_eq!(pointer.phase(area), TerminalPointerPhase::Idle);
        pointer.track(&mouse(MouseEventKind::Up(MouseButton::Left), 3, 3));
        assert_eq!(pointer.phase(area), TerminalPointerPhase::Hovered);
        assert!(pointer.clear());
        assert_eq!(pointer.phase(area), TerminalPointerPhase::Idle);
    }

    #[test]
    fn only_left_down_is_an_activation_position() {
        assert_eq!(
            TerminalPointerState::click_position(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                4,
                5,
            )),
            Some(Position::new(4, 5))
        );
        assert_eq!(
            TerminalPointerState::click_position(&mouse(MouseEventKind::Moved, 4, 5)),
            None
        );
    }
}
