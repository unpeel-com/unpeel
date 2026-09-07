use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::{ColorScheme, KitTheme, TerminalPointerPhase, TerminalPointerState, VerticalScrollbar};

/// Semantic color treatment for one popup-menu item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MenuItemTone {
    #[default]
    Normal,
    Muted,
    Danger,
}

/// One selectable value in a [`PopupMenu`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem<T> {
    label: String,
    value: T,
    enabled: bool,
    tone: MenuItemTone,
}

impl<T> MenuItem<T> {
    pub fn new(label: impl Into<String>, value: T) -> Self {
        Self {
            label: label.into(),
            value,
            enabled: true,
            tone: MenuItemTone::Normal,
        }
    }

    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    #[must_use]
    pub const fn tone(mut self, tone: MenuItemTone) -> Self {
        self.tone = tone;
        self
    }

    #[must_use]
    pub const fn danger(self) -> Self {
        self.tone(MenuItemTone::Danger)
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn item_tone(&self) -> MenuItemTone {
        self.tone
    }
}

/// Borderless gray popup styling shared by context menus and dropdowns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuTheme {
    pub background: Style,
    pub item: Style,
    pub selected: Style,
    pub muted: Style,
    pub disabled: Style,
    pub danger: Style,
    pub scrollbar_track: Style,
    pub scrollbar_thumb: Style,
    pub outer_padding: u16,
    pub left_padding: u16,
    pub right_padding: u16,
    pub minimum_width: u16,
}

impl MenuTheme {
    #[must_use]
    pub const fn dark() -> Self {
        Self::for_palette(KitTheme::dark())
    }

    #[must_use]
    pub const fn light() -> Self {
        Self::for_palette(KitTheme::light())
    }

    #[must_use]
    pub const fn for_color_scheme(scheme: ColorScheme) -> Self {
        Self::for_palette(KitTheme::for_scheme(scheme))
    }

    #[must_use]
    pub fn detected() -> Self {
        Self::for_color_scheme(ColorScheme::detect())
    }

    const fn for_palette(palette: KitTheme) -> Self {
        Self {
            background: Style::new().fg(palette.text).bg(palette.surface),
            item: Style::new().fg(palette.text).bg(palette.surface),
            selected: palette.selected_row,
            muted: Style::new().fg(palette.muted).bg(palette.surface),
            disabled: Style::new().fg(palette.subtle).bg(palette.surface),
            danger: Style::new().fg(palette.danger).bg(palette.surface),
            scrollbar_track: Style::new().fg(palette.subtle).bg(palette.surface),
            scrollbar_thumb: Style::new().fg(palette.muted).bg(palette.surface),
            outer_padding: 0,
            left_padding: 0,
            right_padding: 0,
            minimum_width: 0,
        }
    }
}

impl Default for MenuTheme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Reusable flat popup for context menus and dropdowns.
///
/// The popup paints its own gray surface with no default cell padding and uses
/// a full-row gray hover/keyboard selection. It has no Ratatui `Block` or stock
/// border. Mouse hit-testing is derived from the most recent render.
#[derive(Debug)]
pub struct PopupMenu<T> {
    items: Vec<MenuItem<T>>,
    selected: Option<usize>,
    anchor: Position,
    area: Rect,
    items_area: Rect,
    scroll: usize,
    theme: MenuTheme,
    pointer: TerminalPointerState,
}

impl<T> PopupMenu<T> {
    pub fn new(anchor: Position, items: impl IntoIterator<Item = MenuItem<T>>) -> Self {
        let items = items.into_iter().collect::<Vec<_>>();
        let selected = items.iter().position(MenuItem::is_enabled);
        Self {
            items,
            selected,
            anchor,
            area: Rect::default(),
            items_area: Rect::default(),
            scroll: 0,
            theme: MenuTheme::default(),
            pointer: TerminalPointerState::default(),
        }
    }

    #[must_use]
    pub const fn with_theme(mut self, theme: MenuTheme) -> Self {
        self.theme = theme;
        self
    }

    pub const fn set_theme(&mut self, theme: MenuTheme) {
        self.theme = theme;
    }

    #[must_use]
    pub const fn theme(&self) -> MenuTheme {
        self.theme
    }

    #[must_use]
    pub fn items(&self) -> &[MenuItem<T>] {
        &self.items
    }

    #[must_use]
    pub const fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    #[must_use]
    pub fn selected_item(&self) -> Option<&MenuItem<T>> {
        self.selected.and_then(|index| self.items.get(index))
    }

    #[must_use]
    pub fn selected_value(&self) -> Option<&T> {
        self.selected_item().map(MenuItem::value)
    }

    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    #[must_use]
    pub const fn items_area(&self) -> Rect {
        self.items_area
    }

    pub const fn set_anchor(&mut self, anchor: Position) {
        self.anchor = anchor;
    }

    #[must_use]
    pub const fn pointer(&self) -> TerminalPointerState {
        self.pointer
    }

    /// Tracks hover/press and makes an enabled row under the pointer the same
    /// active row that Up/Down and Enter use.
    pub fn track_mouse(&mut self, event: &MouseEvent) -> bool {
        let mut changed = self.pointer.track(event);
        if matches!(
            event.kind,
            MouseEventKind::Moved
                | MouseEventKind::Drag(crossterm::event::MouseButton::Left)
                | MouseEventKind::Down(crossterm::event::MouseButton::Left)
        ) {
            changed |= self.select_at(Position::new(event.column, event.row));
        }
        changed
    }

    /// Returns the enabled menu row activated by a left click. Keyboard Enter
    /// reads the same selected index, so both input paths are identical.
    pub fn action_index_for_mouse(&mut self, event: &MouseEvent) -> Option<usize> {
        self.track_mouse(event);
        let position = TerminalPointerState::click_position(event)?;
        let index = self.item_index_at(position)?;
        self.items
            .get(index)
            .is_some_and(MenuItem::is_enabled)
            .then_some(index)
    }

    /// Moves to the next enabled item, wrapping at both ends.
    pub fn move_selection(&mut self, delta: isize) -> bool {
        let enabled = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.enabled.then_some(index))
            .collect::<Vec<_>>();
        if enabled.is_empty() || delta == 0 {
            return false;
        }
        let current = self
            .selected
            .and_then(|selected| enabled.iter().position(|index| *index == selected))
            .unwrap_or(0);
        let next = if delta < 0 {
            current.checked_sub(1).unwrap_or(enabled.len() - 1)
        } else {
            (current + 1) % enabled.len()
        };
        let changed = self.selected != Some(enabled[next]);
        self.selected = Some(enabled[next]);
        changed
    }

    /// Selects an enabled item by index.
    pub fn set_selected_index(&mut self, index: usize) -> bool {
        if !self.items.get(index).is_some_and(MenuItem::is_enabled) {
            return false;
        }
        let changed = self.selected != Some(index);
        self.selected = Some(index);
        changed
    }

    /// Index under a cell from the most recent render, including disabled
    /// rows. Use [`Self::select_at`] when only actionable hits are wanted.
    #[must_use]
    pub fn item_index_at(&self, position: Position) -> Option<usize> {
        if !self.items_area.contains(position) {
            return None;
        }
        let index = self
            .scroll
            .saturating_add(usize::from(position.y.saturating_sub(self.items_area.y)));
        (index < self.items.len()).then_some(index)
    }

    #[must_use]
    pub fn item_at(&self, position: Position) -> Option<&MenuItem<T>> {
        self.item_index_at(position)
            .and_then(|index| self.items.get(index))
    }

    /// Updates the full-row hover selection. Disabled items do not activate.
    pub fn select_at(&mut self, position: Position) -> bool {
        self.pointer.move_to(position);
        self.item_index_at(position)
            .is_some_and(|index| self.set_selected_index(index))
    }

    /// Alias that makes pointer-move call sites read naturally.
    pub fn hover_at(&mut self, position: Position) -> bool {
        self.select_at(position)
    }

    /// Renders inside `frame.area()`, clamping the popup around the anchor.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let bounds = frame.area();
        if bounds.is_empty() {
            self.area = Rect::default();
            self.items_area = Rect::default();
            return;
        }

        let content_width = self
            .items
            .iter()
            .map(|item| UnicodeWidthStr::width(item.label.as_str()))
            .max()
            .unwrap_or(0)
            .saturating_add(usize::from(self.theme.left_padding))
            .saturating_add(usize::from(self.theme.right_padding));
        let natural_height = u16::try_from(self.items.len())
            .unwrap_or(u16::MAX)
            .saturating_add(self.theme.outer_padding.saturating_mul(2));
        let height = natural_height.min(bounds.height);
        let available_rows = height.saturating_sub(self.theme.outer_padding.saturating_mul(2));
        let scrollbar_width = u16::from(self.items.len() > usize::from(available_rows));
        let natural_width = u16::try_from(content_width)
            .unwrap_or(u16::MAX)
            .max(self.theme.minimum_width)
            .saturating_add(self.theme.outer_padding.saturating_mul(2))
            .saturating_add(scrollbar_width);
        let width = natural_width.min(bounds.width);
        let preferred_x = self.anchor.x.saturating_add(1).max(bounds.x);
        let x = preferred_x.min(bounds.right().saturating_sub(width));
        let y = self
            .anchor
            .y
            .max(bounds.y)
            .min(bounds.bottom().saturating_sub(height));
        self.area = Rect::new(x, y, width, height);

        let inset = self.theme.outer_padding;
        let inner = Rect::new(
            self.area.x.saturating_add(inset),
            self.area.y.saturating_add(inset),
            self.area.width.saturating_sub(inset.saturating_mul(2)),
            self.area.height.saturating_sub(inset.saturating_mul(2)),
        );
        let overflow = self.items.len() > usize::from(inner.height);
        self.items_area = Rect::new(
            inner.x,
            inner.y,
            inner.width.saturating_sub(u16::from(overflow)),
            inner.height,
        );
        self.ensure_selected_visible();

        frame.render_widget(Clear, self.area);
        frame.render_widget(Paragraph::new("").style(self.theme.background), self.area);
        for (slot, index) in (self.scroll..self.items.len())
            .take(usize::from(self.items_area.height))
            .enumerate()
        {
            let item = &self.items[index];
            let row = Rect::new(
                self.items_area.x,
                self.items_area.y.saturating_add(slot as u16),
                self.items_area.width,
                1,
            );
            let mut style = if !item.enabled {
                self.theme.disabled
            } else {
                match item.tone {
                    MenuItemTone::Normal => self.theme.item,
                    MenuItemTone::Muted => self.theme.muted,
                    MenuItemTone::Danger => self.theme.danger,
                }
            };
            let phase = if item.enabled {
                self.pointer.phase(row)
            } else {
                TerminalPointerPhase::Idle
            };
            if self.selected == Some(index) || phase != TerminalPointerPhase::Idle {
                style = style.patch(match phase {
                    TerminalPointerPhase::Idle => self.theme.selected,
                    TerminalPointerPhase::Hovered => self
                        .theme
                        .selected
                        .add_modifier(ratatui::style::Modifier::DIM),
                    TerminalPointerPhase::Pressed => self
                        .theme
                        .selected
                        .add_modifier(ratatui::style::Modifier::BOLD),
                });
            }
            frame.render_widget(
                Paragraph::new(format!(
                    "{}{}",
                    " ".repeat(usize::from(
                        self.theme.left_padding.min(self.items_area.width)
                    )),
                    item.label
                ))
                .style(style),
                row,
            );
        }

        if overflow {
            let scrollbar = Rect::new(inner.right().saturating_sub(1), inner.y, 1, inner.height);
            frame.render_widget(
                VerticalScrollbar::new(self.items.len(), usize::from(inner.height), self.scroll)
                    .track_style(self.theme.scrollbar_track)
                    .thumb_style(self.theme.scrollbar_thumb),
                scrollbar,
            );
        }
    }

    fn ensure_selected_visible(&mut self) {
        let viewport = usize::from(self.items_area.height);
        let Some(selected) = self.selected else {
            self.scroll = 0;
            return;
        };
        if viewport == 0 {
            self.scroll = 0;
        } else if selected < self.scroll {
            self.scroll = selected;
        } else if selected >= self.scroll.saturating_add(viewport) {
            self.scroll = selected.saturating_add(1).saturating_sub(viewport);
        }
        self.scroll = self.scroll.min(self.items.len().saturating_sub(viewport));
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    #[test]
    fn dark_menu_is_gray_and_hover_is_lighter_across_the_full_row() {
        let mut menu = PopupMenu::new(
            Position::new(2, 2),
            [
                MenuItem::new("Open", 1),
                MenuItem::new("Delete", 2).danger(),
            ],
        )
        .with_theme(MenuTheme::dark());
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| menu.render(frame)).unwrap();

        let area = menu.area();
        let items = menu.items_area();
        assert_eq!(
            terminal.backend().buffer()[(area.x, area.y)].bg,
            Color::Rgb(63, 63, 70)
        );
        assert_eq!(
            terminal.backend().buffer()[(area.x, area.y + 1)].bg,
            Color::Rgb(39, 39, 42)
        );
        assert_eq!(
            terminal.backend().buffer()[(items.right() - 1, items.y)].bg,
            Color::Rgb(63, 63, 70)
        );
        assert_eq!(
            terminal.backend().buffer()[(items.x, items.y)].symbol(),
            "O"
        );
        assert_eq!(
            terminal.backend().buffer()[(items.x + 1, items.y)].symbol(),
            "p"
        );
        assert_eq!(area, items, "the default menu has no cell padding");

        assert!(menu.hover_at(Position::new(items.x, items.y + 1)));
        terminal.draw(|frame| menu.render(frame)).unwrap();
        assert_eq!(menu.selected_value(), Some(&2));
        assert_eq!(
            terminal.backend().buffer()[(items.right() - 1, items.y + 1)].bg,
            Color::Rgb(63, 63, 70)
        );

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: items.x,
            row: items.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(menu.action_index_for_mouse(&click), Some(1));
        terminal.draw(|frame| menu.render(frame)).unwrap();
        assert!(
            terminal.backend().buffer()[(items.x, items.y + 1)]
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn keyboard_navigation_skips_disabled_items() {
        let mut menu = PopupMenu::new(
            Position::new(0, 0),
            [
                MenuItem::new("First", 1),
                MenuItem::new("Unavailable", 2).disabled(),
                MenuItem::new("Last", 3),
            ],
        );
        assert_eq!(menu.selected_value(), Some(&1));
        assert!(menu.move_selection(1));
        assert_eq!(menu.selected_value(), Some(&3));
        assert!(menu.move_selection(1));
        assert_eq!(menu.selected_value(), Some(&1));
    }

    #[test]
    fn light_menu_uses_darker_selection_and_clamps_to_screen() {
        let mut menu = PopupMenu::new(
            Position::new(u16::MAX, u16::MAX),
            [MenuItem::new("A rather long action", ())],
        )
        .with_theme(MenuTheme::light());
        let mut terminal = Terminal::new(TestBackend::new(18, 4)).unwrap();
        terminal.draw(|frame| menu.render(frame)).unwrap();
        assert_eq!(menu.area().right(), 18);
        assert_eq!(menu.area().bottom(), 4);
        assert_eq!(
            terminal.backend().buffer()[(menu.items_area().x, menu.items_area().y)].bg,
            Color::Rgb(216, 216, 220)
        );
    }
}
