//! Shared focus navigation and viewport state for App Kit row collections.

use std::ops::{Deref, DerefMut};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use ratatui::layout::{Position, Rect};
use serde::{Deserialize, Serialize};

use crate::{TerminalPointerPhase, TerminalPointerState};

/// What PageUp/PageDown change for a list.
///
/// Catalogs normally move selection. Dashboards such as Usage can retain a
/// selected item while paging only the viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListPageBehavior {
    #[default]
    Selection,
    Scroll,
}

/// How single-row movement behaves at the first and last item.
///
/// Flat Lists clamp. Explorer retains its established wrap behavior while
/// using the exact same focus/viewport engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowBoundaryBehavior {
    #[default]
    Clamp,
    Wrap,
}

/// Primary semantic role of the focused row.
///
/// The navigation layer needs only this closed behavior hint; component ids,
/// actions, persistence, and routing remain App-owned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowPrimaryRole {
    #[default]
    Static,
    Toggle,
    Checkmark,
    Disclosure,
    Command,
    Destructive,
}

impl RowPrimaryRole {
    #[must_use]
    pub const fn is_interactive(self) -> bool {
        !matches!(self, Self::Static)
    }
}

/// Closed keyboard vocabulary shared by every flat App Kit list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListNavigationAction {
    Down,
    Up,
    First,
    Last,
    PageDown,
    PageUp,
    Activate,
    Back,
}

/// Observable result after applying one navigation action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListNavigationOutcome {
    None,
    SelectionChanged(usize),
    Scrolled(usize),
    Activate(usize),
    Back,
    /// Focus moved to the Page's back row above the first item
    /// (see `Page::navigate`).
    FocusedBack,
}

/// Role-aware result of interpreting one key for a focused row collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKeyDecision {
    Navigate(ListNavigationAction),
    InvokePrimary,
}

/// Role-aware result of clicking one row in a terminal collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPointerDecision {
    Select(usize),
    InvokePrimary(usize),
}

/// App Kit's standard list keymap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListKeymap {
    space_pages_down: bool,
    character_aliases: bool,
}

impl ListKeymap {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            space_pages_down: false,
            character_aliases: true,
        }
    }

    /// Enables Space as an alias for PageDown for Apps that already expose it.
    #[must_use]
    pub const fn space_pages_down(mut self, enabled: bool) -> Self {
        self.space_pages_down = enabled;
        self
    }

    #[must_use]
    pub const fn pages_down_with_space(&self) -> bool {
        self.space_pages_down
    }

    /// Enables or disables `j`/`k`/`g`/`G`/`q` navigation aliases.
    ///
    /// Explorer disables them because printable characters focus its filter.
    #[must_use]
    pub const fn character_aliases(mut self, enabled: bool) -> Self {
        self.character_aliases = enabled;
        self
    }

    #[must_use]
    pub const fn uses_character_aliases(&self) -> bool {
        self.character_aliases
    }

    /// Maps a crossterm press/repeat event into the shared list vocabulary.
    #[must_use]
    pub fn action_for_key(&self, key: &KeyEvent) -> Option<ListNavigationAction> {
        if matches!(key.kind, KeyEventKind::Release) {
            return None;
        }
        let command_modifier = key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        );
        match key.code {
            KeyCode::Down => Some(ListNavigationAction::Down),
            KeyCode::Up => Some(ListNavigationAction::Up),
            KeyCode::Home => Some(ListNavigationAction::First),
            KeyCode::End => Some(ListNavigationAction::Last),
            KeyCode::PageDown => Some(ListNavigationAction::PageDown),
            KeyCode::PageUp => Some(ListNavigationAction::PageUp),
            KeyCode::Enter => Some(ListNavigationAction::Activate),
            KeyCode::Esc => Some(ListNavigationAction::Back),
            KeyCode::Char(' ') if self.space_pages_down && !command_modifier => {
                Some(ListNavigationAction::PageDown)
            }
            KeyCode::Char('j') if self.character_aliases && !command_modifier => {
                Some(ListNavigationAction::Down)
            }
            KeyCode::Char('k') if self.character_aliases && !command_modifier => {
                Some(ListNavigationAction::Up)
            }
            KeyCode::Char('g') if self.character_aliases && !command_modifier => {
                Some(ListNavigationAction::First)
            }
            KeyCode::Char('G') if self.character_aliases && !command_modifier => {
                Some(ListNavigationAction::Last)
            }
            KeyCode::Char('q') if self.character_aliases && !command_modifier => {
                Some(ListNavigationAction::Back)
            }
            _ => None,
        }
    }

    /// Applies the one App Kit keyboard decision table for a focused row.
    ///
    /// Enter invokes the row's primary role. Space invokes only a Toggle;
    /// otherwise it pages down. Escape/back aliases remain navigation so the
    /// Page/App can route them through its authoritative back action.
    #[must_use]
    pub fn decision_for_key(
        &self,
        key: &KeyEvent,
        primary_role: RowPrimaryRole,
    ) -> Option<RowKeyDecision> {
        if matches!(key.kind, KeyEventKind::Release) {
            return None;
        }
        let command_modifier = key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        );
        if command_modifier {
            return None;
        }
        match key.code {
            KeyCode::Enter => primary_role
                .is_interactive()
                .then_some(RowKeyDecision::InvokePrimary),
            KeyCode::Char(' ') if primary_role == RowPrimaryRole::Toggle => {
                Some(RowKeyDecision::InvokePrimary)
            }
            KeyCode::Char(' ') => Some(RowKeyDecision::Navigate(ListNavigationAction::PageDown)),
            _ => self.action_for_key(key).map(RowKeyDecision::Navigate),
        }
    }
}

/// Per-item geometry and focusability fed to [`RowNavigationState::prepare_with_rows`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowMetrics {
    pub height: u16,
    pub selectable: bool,
}

impl RowMetrics {
    #[must_use]
    pub const fn new(height: u16, selectable: bool) -> Self {
        Self { height, selectable }
    }
}

/// Selection, scroll offset, and last-rendered geometry for a row collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowNavigationState {
    selected: Option<usize>,
    offset: usize,
    viewport_rows: usize,
    scroll_padding: usize,
    page_overlap: usize,
    page_behavior: ListPageBehavior,
    boundary_behavior: RowBoundaryBehavior,
    reveal_selected: bool,
    rows_area: Rect,
    pointer: TerminalPointerState,
    /// Per-item terminal heights. Empty means every item is one row.
    heights: Vec<u16>,
    /// Per-item focusability. Empty means every item is selectable.
    selectable: Vec<bool>,
    /// Cumulative start row per item plus the total (`heights.len() + 1`).
    starts: Vec<usize>,
}

impl Default for RowNavigationState {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RowNavigationState {
    #[must_use]
    pub const fn new(selected: Option<usize>) -> Self {
        Self {
            selected,
            offset: 0,
            viewport_rows: 0,
            scroll_padding: 0,
            page_overlap: 1,
            page_behavior: ListPageBehavior::Selection,
            boundary_behavior: RowBoundaryBehavior::Clamp,
            reveal_selected: true,
            rows_area: Rect::new(0, 0, 0, 0),
            pointer: TerminalPointerState::new(),
            heights: Vec::new(),
            selectable: Vec::new(),
            starts: Vec::new(),
        }
    }

    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    #[must_use]
    pub const fn viewport_rows(&self) -> usize {
        self.viewport_rows
    }

    #[must_use]
    pub const fn rows_area(&self) -> Rect {
        self.rows_area
    }

    /// Whether every item occupies exactly one terminal row.
    #[must_use]
    pub const fn has_uniform_rows(&self) -> bool {
        self.heights.is_empty()
    }

    /// Whether keyboard focus and clicks may land on `index`. Dividers and
    /// other passive rows report `false` and are skipped by navigation.
    #[must_use]
    pub fn is_selectable(&self, index: usize) -> bool {
        self.selectable.get(index).copied().unwrap_or(true)
    }

    /// Nearest selectable index at or after `from`, then at or before it.
    fn snap_selectable(&self, from: usize, item_count: usize) -> Option<usize> {
        if item_count == 0 {
            return None;
        }
        let from = from.min(item_count - 1);
        (from..item_count)
            .find(|index| self.is_selectable(*index))
            .or_else(|| (0..from).rev().find(|index| self.is_selectable(*index)))
    }

    fn next_selectable(&self, from: usize, item_count: usize) -> Option<usize> {
        (from.saturating_add(1)..item_count).find(|index| self.is_selectable(*index))
    }

    fn previous_selectable(&self, from: usize) -> Option<usize> {
        (0..from).rev().find(|index| self.is_selectable(*index))
    }

    /// Terminal rows used by one item after the last render (1 when unknown).
    #[must_use]
    pub fn item_height(&self, index: usize) -> usize {
        self.heights
            .get(index)
            .map_or(1, |height| usize::from(*height))
    }

    /// First terminal row of `index` measured from the top of the content.
    #[must_use]
    pub fn item_start_row(&self, index: usize) -> usize {
        if self.heights.is_empty() {
            return index;
        }
        self.starts[index.min(self.heights.len())]
    }

    /// Complete virtual row count for a scrollbar.
    #[must_use]
    pub fn content_rows(&self, item_count: usize) -> usize {
        if self.heights.is_empty() {
            item_count
        } else {
            self.starts.last().copied().unwrap_or(0)
        }
    }

    /// Content row at the top of the viewport (the first row of `offset`).
    #[must_use]
    pub fn offset_row(&self) -> usize {
        self.item_start_row(self.offset)
    }

    /// Items that fit completely in the viewport from the current offset.
    #[must_use]
    pub fn visible_item_count(&self, item_count: usize) -> usize {
        if self.heights.is_empty() {
            return self
                .viewport_rows
                .min(item_count.saturating_sub(self.offset));
        }
        let mut used = 0usize;
        let mut count = 0usize;
        for index in self.offset..item_count.min(self.heights.len()) {
            used += self.item_height(index);
            if used > self.viewport_rows {
                break;
            }
            count += 1;
        }
        count
    }

    /// Index of the item that owns content row `row` (clamped to the end).
    fn item_at_row(&self, row: usize) -> usize {
        if self.heights.is_empty() {
            return row;
        }
        self.starts[..self.heights.len()]
            .partition_point(|start| *start <= row)
            .saturating_sub(1)
    }

    /// First item whose start row is at or after `row`.
    fn item_starting_at_or_after(&self, row: usize) -> usize {
        if self.heights.is_empty() {
            return row;
        }
        self.starts[..self.heights.len()].partition_point(|start| *start < row)
    }

    #[must_use]
    pub const fn pointer(&self) -> TerminalPointerState {
        self.pointer
    }

    pub const fn set_pointer(&mut self, pointer: TerminalPointerState) {
        self.pointer = pointer;
    }

    /// Feeds renderer-local pointer state shared by List, Tree, and Explorer.
    pub fn track_mouse(&mut self, event: &MouseEvent) -> bool {
        self.pointer.track(event)
    }

    #[must_use]
    pub fn pointer_phase_at(&self, index: usize) -> TerminalPointerPhase {
        if !self.is_selectable(index) {
            return TerminalPointerPhase::Idle;
        }
        self.row_area(index)
            .map_or(TerminalPointerPhase::Idle, |area| self.pointer.phase(area))
    }

    #[must_use]
    pub fn hovered_item(&self, item_count: usize) -> Option<usize> {
        self.pointer
            .position()
            .and_then(|position| self.item_at(position, item_count))
            .filter(|index| self.is_selectable(*index))
    }

    pub const fn set_navigation(
        &mut self,
        scroll_padding: usize,
        page_overlap: usize,
        page_behavior: ListPageBehavior,
    ) {
        self.scroll_padding = scroll_padding;
        self.page_overlap = page_overlap;
        self.page_behavior = page_behavior;
    }

    pub const fn set_boundary_behavior(&mut self, behavior: RowBoundaryBehavior) {
        self.boundary_behavior = behavior;
    }

    #[must_use]
    pub const fn boundary_behavior(&self) -> RowBoundaryBehavior {
        self.boundary_behavior
    }

    pub fn select(&mut self, selected: Option<usize>, item_count: usize) -> bool {
        let next = if item_count == 0 {
            None
        } else {
            selected.and_then(|index| self.snap_selectable(index, item_count))
        };
        let changed = next != self.selected;
        self.selected = next;
        if changed {
            self.reveal_selected = true;
        }
        changed
    }

    pub const fn request_reveal(&mut self) {
        self.reveal_selected = true;
    }

    pub fn set_offset(&mut self, offset: usize, item_count: usize) {
        self.offset = offset.min(self.max_offset(item_count));
        self.reveal_selected = false;
    }

    pub fn scroll_by(&mut self, delta: isize, item_count: usize) -> bool {
        let next = self
            .offset
            .saturating_add_signed(delta)
            .min(self.max_offset(item_count));
        let changed = next != self.offset;
        self.offset = next;
        self.reveal_selected = false;
        changed
    }

    #[must_use]
    pub fn max_offset(&self, item_count: usize) -> usize {
        if self.heights.is_empty() {
            return item_count.saturating_sub(self.viewport_rows);
        }
        let item_count = item_count.min(self.heights.len());
        let total = self.starts[item_count];
        let first_row = total.saturating_sub(self.viewport_rows);
        self.starts[..item_count].partition_point(|start| *start < first_row)
    }

    /// Applies one shared key action using the last rendered viewport size.
    pub fn navigate(
        &mut self,
        action: ListNavigationAction,
        item_count: usize,
    ) -> ListNavigationOutcome {
        if action == ListNavigationAction::Back {
            return ListNavigationOutcome::Back;
        }
        if item_count == 0 {
            self.selected = None;
            self.offset = 0;
            return ListNavigationOutcome::None;
        }
        let Some(current) = self.snap_selectable(self.selected.unwrap_or(0), item_count) else {
            self.selected = None;
            return ListNavigationOutcome::None;
        };
        if action == ListNavigationAction::Activate {
            self.selected = Some(current);
            return ListNavigationOutcome::Activate(current);
        }
        let page = if self.heights.is_empty() {
            self.viewport_rows
        } else {
            self.visible_item_count(item_count)
        }
        .saturating_sub(self.page_overlap)
        .max(1);
        if self.page_behavior == ListPageBehavior::Scroll
            && matches!(
                action,
                ListNavigationAction::PageDown | ListNavigationAction::PageUp
            )
        {
            let delta = if action == ListNavigationAction::PageDown {
                isize::try_from(page).unwrap_or(isize::MAX)
            } else {
                -isize::try_from(page).unwrap_or(isize::MAX)
            };
            return if self.scroll_by(delta, item_count) {
                ListNavigationOutcome::Scrolled(self.offset)
            } else {
                ListNavigationOutcome::None
            };
        }
        let wrap = self.boundary_behavior == RowBoundaryBehavior::Wrap;
        let first = self.snap_selectable(0, item_count).unwrap_or(current);
        let last = self.previous_selectable(item_count).unwrap_or(current);
        let next = match action {
            ListNavigationAction::Down => self
                .next_selectable(current, item_count)
                .unwrap_or(if wrap { first } else { current }),
            ListNavigationAction::Up => {
                self.previous_selectable(current)
                    .unwrap_or(if wrap { last } else { current })
            }
            ListNavigationAction::First => first,
            ListNavigationAction::Last => last,
            ListNavigationAction::PageDown => {
                let target = current.saturating_add(page).min(item_count - 1);
                (target..item_count)
                    .find(|index| self.is_selectable(*index))
                    .or_else(|| self.previous_selectable(target + 1))
                    .unwrap_or(current)
            }
            ListNavigationAction::PageUp => {
                let target = current.saturating_sub(page);
                (0..=target)
                    .rev()
                    .find(|index| self.is_selectable(*index))
                    .or_else(|| self.next_selectable(target.saturating_sub(1), item_count))
                    .unwrap_or(current)
            }
            ListNavigationAction::Activate | ListNavigationAction::Back => current,
        }
        .min(item_count - 1);
        if self.select(Some(next), item_count) {
            self.reveal(item_count);
            ListNavigationOutcome::SelectionChanged(next)
        } else {
            self.reveal(item_count);
            ListNavigationOutcome::None
        }
    }

    /// Updates geometry and resolves selection/scroll bounds before rendering.
    /// Every item is treated as one terminal row.
    pub fn prepare(&mut self, rows_area: Rect, item_count: usize) {
        self.heights.clear();
        self.starts.clear();
        self.selectable.clear();
        self.prepare_common(rows_area, item_count);
    }

    /// Like [`Self::prepare`] with an explicit terminal height per item.
    /// Offsets stay item indexes; viewport, reveal, paging, scrollbar
    /// geometry, and hit-testing all count rows through these heights.
    pub fn prepare_with_heights(&mut self, rows_area: Rect, heights: &[u16]) {
        self.selectable.clear();
        self.prepare_with_heights_and_selectable(rows_area, heights);
    }

    /// Like [`Self::prepare_with_heights`] with per-item focusability. Rows
    /// flagged `false` (dividers, headers) are skipped by keyboard navigation
    /// and never selected by [`Self::select`].
    pub fn prepare_with_rows(&mut self, rows_area: Rect, rows: &[RowMetrics]) {
        self.selectable.clear();
        self.selectable
            .extend(rows.iter().map(|row| row.selectable));
        let heights = rows.iter().map(|row| row.height).collect::<Vec<_>>();
        self.prepare_with_heights_and_selectable(rows_area, &heights);
    }

    fn prepare_with_heights_and_selectable(&mut self, rows_area: Rect, heights: &[u16]) {
        self.heights.clear();
        self.heights
            .extend(heights.iter().map(|height| (*height).max(1)));
        self.starts.clear();
        self.starts.reserve(self.heights.len() + 1);
        let mut total = 0usize;
        self.starts.push(0);
        for height in &self.heights {
            total += usize::from(*height);
            self.starts.push(total);
        }
        self.prepare_common(rows_area, self.heights.len());
    }

    fn prepare_common(&mut self, rows_area: Rect, item_count: usize) {
        self.rows_area = rows_area;
        self.viewport_rows = usize::from(rows_area.height);
        if item_count == 0 {
            self.selected = None;
            self.offset = 0;
            self.reveal_selected = false;
            return;
        }
        if let Some(selected) = self.selected {
            self.selected = self.snap_selectable(selected, item_count);
        }
        self.offset = self.offset.min(self.max_offset(item_count));
        if self.reveal_selected {
            self.reveal(item_count);
        }
    }

    pub fn reveal(&mut self, item_count: usize) {
        let Some(selected) = self.selected else {
            self.offset = self.offset.min(self.max_offset(item_count));
            self.reveal_selected = false;
            return;
        };
        if self.viewport_rows == 0 {
            self.offset = 0;
            return;
        }
        let padding = self
            .scroll_padding
            .min(self.viewport_rows.saturating_sub(1) / 2);
        let selected_start = self.item_start_row(selected);
        let selected_end = selected_start.saturating_add(self.item_height(selected));
        let offset_row = self.offset_row();
        let first_row = offset_row.saturating_add(padding);
        let last_row_exclusive =
            offset_row.saturating_add(self.viewport_rows.saturating_sub(padding));
        if selected_start < first_row {
            self.offset = self.item_at_row(selected_start.saturating_sub(padding));
        } else if selected_end > last_row_exclusive {
            let needed_row = selected_end
                .saturating_add(padding)
                .saturating_sub(self.viewport_rows);
            self.offset = self.item_starting_at_or_after(needed_row).min(selected);
        }
        self.offset = self.offset.min(self.max_offset(item_count));
        self.reveal_selected = false;
    }

    /// Maps a point in the last rendered row rectangle to a model index.
    #[must_use]
    pub fn item_at(&self, position: Position, item_count: usize) -> Option<usize> {
        if !self.rows_area.contains(position) {
            return None;
        }
        let row = usize::from(position.y.saturating_sub(self.rows_area.y));
        if self.heights.is_empty() {
            let index = self.offset.saturating_add(row);
            return (index < item_count).then_some(index);
        }
        let content_row = self.offset_row().saturating_add(row);
        if content_row >= self.content_rows(item_count) {
            return None;
        }
        let index = self.item_at_row(content_row);
        (index < item_count).then_some(index)
    }

    /// Terminal rectangle of one item after the last render, clipped to the
    /// viewport. `None` when the item is scrolled out of view.
    #[must_use]
    pub fn item_area(&self, index: usize) -> Option<Rect> {
        if index < self.offset {
            return None;
        }
        let row = self.item_start_row(index).saturating_sub(self.offset_row());
        let row = u16::try_from(row).ok()?;
        if row >= self.rows_area.height {
            return None;
        }
        let height = u16::try_from(self.item_height(index))
            .unwrap_or(u16::MAX)
            .min(self.rows_area.height - row);
        Some(Rect::new(
            self.rows_area.x,
            self.rows_area.y.saturating_add(row),
            self.rows_area.width,
            height,
        ))
    }

    fn row_area(&self, index: usize) -> Option<Rect> {
        self.item_area(index)
    }
}

/// List-specific render state layered over behavior-agnostic row focus.
///
/// Navigation methods are available through `Deref`; the spinner frame stays
/// here so Explorer/Tree do not inherit presentation state from the focus
/// engine they share with List.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListState {
    navigation: RowNavigationState,
    spinner_frame: usize,
    back_focused: bool,
}

impl ListState {
    #[must_use]
    pub const fn new(selected: Option<usize>) -> Self {
        Self {
            navigation: RowNavigationState::new(selected),
            spinner_frame: 0,
            back_focused: false,
        }
    }

    /// Whether the Page's back row, rather than a list row, holds focus.
    #[must_use]
    pub const fn back_focused(&self) -> bool {
        self.back_focused
    }

    pub const fn set_back_focused(&mut self, focused: bool) -> bool {
        let changed = self.back_focused != focused;
        self.back_focused = focused;
        changed
    }

    #[must_use]
    pub const fn navigation(&self) -> &RowNavigationState {
        &self.navigation
    }

    pub const fn navigation_mut(&mut self) -> &mut RowNavigationState {
        &mut self.navigation
    }

    #[must_use]
    pub const fn spinner_frame(&self) -> usize {
        self.spinner_frame
    }

    pub const fn set_spinner_frame(&mut self, frame: usize) {
        self.spinner_frame = frame;
    }
}

impl Deref for ListState {
    type Target = RowNavigationState;

    fn deref(&self) -> &Self::Target {
        &self.navigation
    }
}

impl DerefMut for ListState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.navigation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn keymap_is_shared_and_space_is_explicit() {
        let standard = ListKeymap::new();
        assert_eq!(
            standard.action_for_key(&key(KeyCode::Char('j'))),
            Some(ListNavigationAction::Down)
        );
        assert_eq!(
            standard.decision_for_key(&key(KeyCode::Enter), RowPrimaryRole::Toggle),
            Some(RowKeyDecision::InvokePrimary)
        );
        assert_eq!(
            standard.decision_for_key(&key(KeyCode::Char(' ')), RowPrimaryRole::Toggle),
            Some(RowKeyDecision::InvokePrimary)
        );
        assert_eq!(
            standard.decision_for_key(&key(KeyCode::Char(' ')), RowPrimaryRole::Disclosure),
            Some(RowKeyDecision::Navigate(ListNavigationAction::PageDown))
        );
        assert_eq!(
            standard.decision_for_key(&key(KeyCode::Enter), RowPrimaryRole::Static),
            None
        );
        assert_eq!(
            standard.decision_for_key(
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
                RowPrimaryRole::Command,
            ),
            None
        );
        assert_eq!(
            standard
                .character_aliases(false)
                .action_for_key(&key(KeyCode::Char('j'))),
            None
        );
        assert_eq!(
            standard.action_for_key(&key(KeyCode::Char('q'))),
            Some(ListNavigationAction::Back)
        );
        assert_eq!(standard.action_for_key(&key(KeyCode::Char(' '))), None);
        assert_eq!(
            standard
                .space_pages_down(true)
                .action_for_key(&key(KeyCode::Char(' '))),
            Some(ListNavigationAction::PageDown)
        );
    }

    #[test]
    fn selection_clamps_and_reveals_with_padding() {
        let mut state = ListState::new(Some(0));
        state.set_navigation(1, 1, ListPageBehavior::Selection);
        state.prepare(Rect::new(0, 0, 20, 4), 10);
        assert_eq!(
            state.navigate(ListNavigationAction::Up, 10),
            ListNavigationOutcome::None
        );
        assert_eq!(state.selected(), Some(0));
        for _ in 0..9 {
            state.navigate(ListNavigationAction::Down, 10);
        }
        assert_eq!(state.selected(), Some(9));
        assert_eq!(state.offset(), 6);
        assert_eq!(
            state.navigate(ListNavigationAction::Down, 10),
            ListNavigationOutcome::None
        );
        assert_eq!(state.selected(), Some(9));
    }

    #[test]
    fn page_actions_can_move_selection_or_only_scroll() {
        let mut selection = ListState::new(Some(1));
        selection.prepare(Rect::new(0, 0, 20, 5), 20);
        assert_eq!(
            selection.navigate(ListNavigationAction::PageDown, 20),
            ListNavigationOutcome::SelectionChanged(5)
        );

        let mut scroll = ListState::new(Some(1));
        scroll.set_navigation(0, 1, ListPageBehavior::Scroll);
        scroll.prepare(Rect::new(0, 0, 20, 5), 20);
        assert_eq!(
            scroll.navigate(ListNavigationAction::PageDown, 20),
            ListNavigationOutcome::Scrolled(4)
        );
        assert_eq!(scroll.selected(), Some(1));
    }

    #[test]
    fn variable_heights_count_rows_for_offsets_paging_and_hit_testing() {
        let mut state = ListState::new(Some(0));
        state.set_navigation(0, 1, ListPageBehavior::Selection);
        // Heights: 1, 2, 3, 1, 2 (9 rows) in a 5-row viewport.
        state.prepare_with_heights(Rect::new(0, 0, 20, 5), &[1, 2, 3, 1, 2]);
        assert!(!state.has_uniform_rows());
        assert_eq!(state.content_rows(5), 9);
        assert_eq!(state.item_start_row(2), 3);
        assert_eq!(state.item_height(2), 3);
        assert_eq!(state.max_offset(5), 3);
        assert_eq!(state.visible_item_count(5), 2);
        assert_eq!(state.item_at(Position::new(0, 4), 5), Some(2));
        assert_eq!(state.item_area(2), Some(Rect::new(0, 3, 20, 2)));
        assert_eq!(state.item_area(3), None);

        assert_eq!(
            state.navigate(ListNavigationAction::PageDown, 5),
            ListNavigationOutcome::SelectionChanged(1),
            "a page is the fully visible items minus the overlap"
        );
        assert_eq!(state.offset(), 0);
        state.navigate(ListNavigationAction::Down, 5);
        assert_eq!(state.selected(), Some(2));
        assert_eq!(
            state.offset(),
            1,
            "Beta and Gamma fill the five rows exactly"
        );
        state.navigate(ListNavigationAction::Down, 5);
        assert_eq!(state.offset(), 2);
        assert_eq!(state.offset_row(), 3);
        state.navigate(ListNavigationAction::Last, 5);
        assert_eq!(state.offset(), 3);
        assert_eq!(state.item_at(Position::new(0, 2), 5), Some(4));
        assert_eq!(state.item_at(Position::new(0, 3), 5), None);
        state.navigate(ListNavigationAction::Up, 5);
        state.navigate(ListNavigationAction::Up, 5);
        assert_eq!(state.selected(), Some(2));
        assert_eq!(state.offset(), 2);

        let mut scroll = ListState::new(Some(0));
        scroll.set_navigation(0, 0, ListPageBehavior::Scroll);
        scroll.prepare_with_heights(Rect::new(0, 0, 20, 5), &[1, 2, 3, 1, 2]);
        assert_eq!(
            scroll.navigate(ListNavigationAction::PageDown, 5),
            ListNavigationOutcome::Scrolled(2)
        );
        assert_eq!(scroll.selected(), Some(0));

        // Returning to uniform rows restores the row-per-item behavior.
        scroll.prepare(Rect::new(0, 0, 20, 5), 5);
        assert!(scroll.has_uniform_rows());
        assert_eq!(scroll.max_offset(5), 0);
    }

    #[test]
    fn unselectable_rows_are_skipped_by_navigation_and_selection() {
        let mut state = ListState::new(Some(0));
        state.set_navigation(0, 1, ListPageBehavior::Selection);
        // 0: divider, 1: item, 2: divider, 3: item, 4: item, 5: divider
        let rows = [false, true, false, true, true, false]
            .map(|selectable| RowMetrics::new(1, selectable));
        state.prepare_with_rows(Rect::new(0, 0, 20, 4), &rows);
        assert_eq!(state.selected(), Some(1), "initial selection snaps forward");
        assert!(!state.is_selectable(0));
        assert_eq!(
            state.navigate(ListNavigationAction::Down, 6),
            ListNavigationOutcome::SelectionChanged(3)
        );
        assert_eq!(
            state.navigate(ListNavigationAction::Down, 6),
            ListNavigationOutcome::SelectionChanged(4)
        );
        assert_eq!(
            state.navigate(ListNavigationAction::Down, 6),
            ListNavigationOutcome::None,
            "trailing divider is never the last stop"
        );
        assert_eq!(
            state.navigate(ListNavigationAction::Up, 6),
            ListNavigationOutcome::SelectionChanged(3)
        );
        assert_eq!(
            state.navigate(ListNavigationAction::First, 6),
            ListNavigationOutcome::SelectionChanged(1)
        );
        assert_eq!(
            state.navigate(ListNavigationAction::Last, 6),
            ListNavigationOutcome::SelectionChanged(4)
        );
        assert_eq!(
            state.navigate(ListNavigationAction::PageUp, 6),
            ListNavigationOutcome::SelectionChanged(1),
            "page targets snap to the nearest selectable row"
        );
        assert!(
            state.select(Some(2), 6),
            "selecting a divider snaps forward"
        );
        assert_eq!(state.selected(), Some(3));
        assert!(!state.select(Some(2), 6));
        assert_eq!(state.hovered_item(6), None);

        let mut wrap = RowNavigationState::new(Some(4));
        wrap.set_boundary_behavior(RowBoundaryBehavior::Wrap);
        wrap.prepare_with_rows(Rect::new(0, 0, 20, 4), &rows);
        assert_eq!(
            wrap.navigate(ListNavigationAction::Down, 6),
            ListNavigationOutcome::SelectionChanged(1)
        );
    }

    #[test]
    fn explorer_boundary_policy_wraps_steps_but_clamps_pages() {
        let mut state = RowNavigationState::new(Some(0));
        state.set_boundary_behavior(RowBoundaryBehavior::Wrap);
        state.set_navigation(0, 0, ListPageBehavior::Selection);
        state.prepare(Rect::new(0, 0, 20, 3), 10);
        assert_eq!(
            state.navigate(ListNavigationAction::Up, 10),
            ListNavigationOutcome::SelectionChanged(9)
        );
        assert_eq!(
            state.navigate(ListNavigationAction::Down, 10),
            ListNavigationOutcome::SelectionChanged(0)
        );
        assert_eq!(
            state.navigate(ListNavigationAction::PageUp, 10),
            ListNavigationOutcome::None
        );
        state.select(Some(9), 10);
        assert_eq!(
            state.navigate(ListNavigationAction::PageDown, 10),
            ListNavigationOutcome::None
        );
    }
}
