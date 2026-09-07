//! Shared single-line text input for filters and lightweight forms.

use std::cmp::Ordering;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthChar;

use crate::{ColorScheme, DoubleClickTracker, KitTheme, SELECTABLE_LEFT_PADDING};

/// Borderless colors and spacing used by [`InputField`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputFieldTheme {
    pub style: Style,
    pub text: Style,
    pub focused: Style,
    pub placeholder: Style,
    pub prompt: Style,
    pub selection: Style,
    pub left_padding: u16,
}

impl InputFieldTheme {
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
            style: Style::new(),
            text: Style::new().fg(palette.text),
            focused: Style::new().fg(palette.text).add_modifier(Modifier::BOLD),
            placeholder: Style::new().fg(palette.subtle),
            prompt: Style::new().fg(palette.muted),
            selection: palette.selected_row,
            left_padding: SELECTABLE_LEFT_PADDING,
        }
    }
}

impl Default for InputFieldTheme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Backend-neutral editing actions understood by [`InputField::handle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputFieldAction {
    Insert(char),
    InsertText(String),
    Backspace,
    Delete,
    Clear,
    SelectAll,
    Left { extend: bool, word: bool },
    Right { extend: bool, word: bool },
    Home { extend: bool },
    End { extend: bool },
}

/// A reusable borderless single-line input with terminal-native cursor data.
///
/// Text and selection positions are Unicode character indexes. The component
/// owns horizontal scrolling, keyboard editing, selection replacement, mouse
/// drag selection, and target-aware double-click word selection. Apps render
/// it through [`Self::widget`] and may apply the returned
/// [`Self::cursor_position`] to their Ratatui frame.
#[derive(Debug)]
pub struct InputField {
    text: String,
    cursor: usize,
    selection_anchor: Option<usize>,
    focused: bool,
    prompt: String,
    placeholder: String,
    theme: InputFieldTheme,
    area: Rect,
    content_area: Rect,
    cursor_position: Option<Position>,
    scroll: usize,
    drag_anchor: Option<usize>,
    clicks: DoubleClickTracker<(usize, usize)>,
}

impl InputField {
    #[must_use]
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            selection_anchor: None,
            focused: false,
            prompt: String::new(),
            placeholder: sanitize(placeholder.into()),
            theme: InputFieldTheme::default(),
            area: Rect::default(),
            content_area: Rect::default(),
            cursor_position: None,
            scroll: 0,
            drag_anchor: None,
            clicks: DoubleClickTracker::new(),
        }
    }

    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = sanitize(prompt.into());
        self
    }

    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) -> bool {
        let placeholder = sanitize(placeholder.into());
        let changed = placeholder != self.placeholder;
        self.placeholder = placeholder;
        changed
    }

    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn set_prompt(&mut self, prompt: impl Into<String>) -> bool {
        let prompt = sanitize(prompt.into());
        let changed = prompt != self.prompt;
        self.prompt = prompt;
        changed
    }

    #[must_use]
    pub const fn with_theme(mut self, theme: InputFieldTheme) -> Self {
        self.theme = theme;
        self
    }

    pub const fn set_theme(&mut self, theme: InputFieldTheme) {
        self.theme = theme;
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub const fn focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) -> bool {
        let changed = self.focused != focused;
        self.focused = focused;
        if !focused {
            self.drag_anchor = None;
            self.clicks.reset();
        }
        changed
    }

    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    #[must_use]
    pub const fn content_area(&self) -> Rect {
        self.content_area
    }

    /// Native cursor location derived from the most recent render.
    #[must_use]
    pub const fn cursor_position(&self) -> Option<Position> {
        self.cursor_position
    }

    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.drag_anchor.is_some()
    }

    #[must_use]
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        (anchor != self.cursor).then(|| ordered(anchor, self.cursor))
    }

    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        let (start, end) = self.selection_range()?;
        Some(&self.text[char_to_byte(&self.text, start)..char_to_byte(&self.text, end)])
    }

    /// Replaces the contents, puts the cursor at the end, and clears selection.
    pub fn set_text(&mut self, text: impl Into<String>) -> bool {
        let text = sanitize(text.into());
        let changed = text != self.text;
        self.clicks.reset();
        self.text = text;
        self.cursor = char_len(&self.text);
        self.selection_anchor = None;
        self.scroll = 0;
        self.drag_anchor = None;
        changed
    }

    /// Applies one backend-neutral editing action.
    pub fn handle(&mut self, action: InputFieldAction) -> bool {
        match action {
            InputFieldAction::Insert(character) => self.insert_text(character.to_string()),
            InputFieldAction::InsertText(text) => self.insert_text(text),
            InputFieldAction::Backspace => self.backspace(),
            InputFieldAction::Delete => self.delete(),
            InputFieldAction::Clear => self.clear(),
            InputFieldAction::SelectAll => self.select_all(),
            InputFieldAction::Left { extend, word } => self.move_left(extend, word),
            InputFieldAction::Right { extend, word } => self.move_right(extend, word),
            InputFieldAction::Home { extend } => self.move_to(0, extend),
            InputFieldAction::End { extend } => self.move_to(char_len(&self.text), extend),
        }
    }

    pub fn insert_text(&mut self, text: impl Into<String>) -> bool {
        let insertion = sanitize(text.into());
        self.clicks.reset();
        if insertion.is_empty() {
            return false;
        }
        self.delete_selection();
        let byte = char_to_byte(&self.text, self.cursor);
        self.text.insert_str(byte, &insertion);
        self.cursor += char_len(&insertion);
        self.selection_anchor = None;
        true
    }

    pub fn backspace(&mut self) -> bool {
        self.clicks.reset();
        if self.delete_selection() {
            return true;
        }
        let Some(previous) = self.cursor.checked_sub(1) else {
            return false;
        };
        let start = char_to_byte(&self.text, previous);
        let end = char_to_byte(&self.text, self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor = previous;
        true
    }

    pub fn delete(&mut self) -> bool {
        self.clicks.reset();
        if self.delete_selection() {
            return true;
        }
        let length = char_len(&self.text);
        if self.cursor >= length {
            return false;
        }
        let start = char_to_byte(&self.text, self.cursor);
        let end = char_to_byte(&self.text, self.cursor + 1);
        self.text.replace_range(start..end, "");
        true
    }

    pub fn clear(&mut self) -> bool {
        self.clicks.reset();
        if self.text.is_empty() {
            return false;
        }
        self.text.clear();
        self.cursor = 0;
        self.selection_anchor = None;
        self.scroll = 0;
        self.drag_anchor = None;
        true
    }

    pub fn select_all(&mut self) -> bool {
        self.clicks.reset();
        let end = char_len(&self.text);
        let changed = self.selection_range() != (end > 0).then_some((0, end));
        self.selection_anchor = (end > 0).then_some(0);
        self.cursor = end;
        changed
    }

    pub fn move_left(&mut self, extend: bool, word: bool) -> bool {
        if !extend && let Some((start, _)) = self.selection_range() {
            return self.move_to(start, false);
        }
        let target = if word {
            previous_word_boundary(&self.text, self.cursor)
        } else {
            self.cursor.saturating_sub(1)
        };
        self.move_to(target, extend)
    }

    pub fn move_right(&mut self, extend: bool, word: bool) -> bool {
        if !extend && let Some((_, end)) = self.selection_range() {
            return self.move_to(end, false);
        }
        let target = if word {
            next_word_boundary(&self.text, self.cursor)
        } else {
            self.cursor.saturating_add(1).min(char_len(&self.text))
        };
        self.move_to(target, extend)
    }

    pub fn move_to(&mut self, target: usize, extend: bool) -> bool {
        self.clicks.reset();
        let target = target.min(char_len(&self.text));
        let previous_cursor = self.cursor;
        let previous_anchor = self.selection_anchor;
        if extend {
            self.selection_anchor.get_or_insert(self.cursor);
        } else {
            self.selection_anchor = None;
        }
        self.cursor = target;
        previous_cursor != self.cursor || previous_anchor != self.selection_anchor
    }

    /// Focuses and positions the cursor. A second click in the same word
    /// selects that word; Shift-click extends the current selection.
    pub fn mouse_down(&mut self, position: Position, extend: bool) -> bool {
        if !self.area.contains(position) {
            self.clicks.reset();
            return false;
        }
        let was_focused = self.focused;
        self.focused = true;
        let index = self.hit_index(position);
        let word = word_bounds(&self.text, index);
        let double = word.0 < word.1 && self.clicks.click(word);
        let previous = (self.cursor, self.selection_anchor, self.drag_anchor);
        if double {
            self.selection_anchor = Some(word.0);
            self.cursor = word.1;
            self.drag_anchor = Some(word.0);
        } else if extend {
            let anchor = self.selection_anchor.unwrap_or(self.cursor);
            self.selection_anchor = Some(anchor);
            self.cursor = index;
            self.drag_anchor = Some(anchor);
        } else {
            self.cursor = index;
            self.selection_anchor = None;
            self.drag_anchor = Some(index);
        }
        !was_focused || previous != (self.cursor, self.selection_anchor, self.drag_anchor)
    }

    /// Extends an active mouse selection, clamping beyond either edge.
    pub fn mouse_drag(&mut self, position: Position) -> bool {
        let Some(anchor) = self.drag_anchor else {
            return false;
        };
        self.clicks.reset();
        let next = if position.x <= self.content_area.x {
            0
        } else if position.x >= self.content_area.right() {
            char_len(&self.text)
        } else {
            self.hit_index(position)
        };
        let previous = (self.cursor, self.selection_anchor);
        self.selection_anchor = Some(anchor);
        self.cursor = next;
        previous != (self.cursor, self.selection_anchor)
    }

    pub fn mouse_up(&mut self) -> bool {
        self.drag_anchor.take().is_some()
    }

    pub(crate) fn clear_render_state(&mut self) {
        self.area = Rect::default();
        self.content_area = Rect::default();
        self.cursor_position = None;
    }

    #[must_use]
    pub fn widget(&mut self) -> InputFieldWidget<'_> {
        InputFieldWidget { input: self }
    }

    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        let start_byte = char_to_byte(&self.text, start);
        let end_byte = char_to_byte(&self.text, end);
        self.text.replace_range(start_byte..end_byte, "");
        self.cursor = start;
        self.selection_anchor = None;
        self.drag_anchor = None;
        true
    }

    fn hit_index(&self, position: Position) -> usize {
        let length = char_len(&self.text);
        if self.content_area.is_empty() || position.x <= self.content_area.x {
            return self.scroll.min(length);
        }
        if position.x >= self.content_area.right() {
            return length;
        }
        let target = usize::from(position.x.saturating_sub(self.content_area.x));
        let mut width = 0usize;
        for (index, character) in self.text.chars().enumerate().skip(self.scroll) {
            let character_width = character.width().unwrap_or(0).max(1);
            if target < width.saturating_add(character_width) {
                return if target.saturating_sub(width) * 2 < character_width {
                    index
                } else {
                    index + 1
                };
            }
            width = width.saturating_add(character_width);
            if width >= usize::from(self.content_area.width) {
                break;
            }
        }
        length
    }

    fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        self.area = Rect {
            height: area.height.min(1),
            ..area
        };
        buffer.set_style(self.area, self.theme.style);
        let padding = self.theme.left_padding.min(self.area.width);
        let prompt_width = display_width(&self.prompt)
            .min(usize::from(self.area.width.saturating_sub(padding)))
            as u16;
        self.content_area = Rect {
            x: self
                .area
                .x
                .saturating_add(padding)
                .saturating_add(prompt_width),
            y: self.area.y,
            width: self
                .area
                .width
                .saturating_sub(padding)
                .saturating_sub(prompt_width),
            height: self.area.height,
        };
        self.ensure_cursor_visible();

        let active = if self.focused {
            self.theme.focused
        } else {
            self.theme.text
        };
        let mut spans = vec![
            Span::styled(" ".repeat(usize::from(padding)), self.theme.style),
            Span::styled(
                self.prompt.clone(),
                self.theme.style.patch(self.theme.prompt),
            ),
        ];
        if self.text.is_empty() {
            spans.push(Span::styled(
                take_width(&self.placeholder, usize::from(self.content_area.width)),
                self.theme.style.patch(self.theme.placeholder),
            ));
        } else {
            let selected = self.selection_range();
            let mut used = 0usize;
            for (index, character) in self.text.chars().enumerate().skip(self.scroll) {
                let width = character.width().unwrap_or(0).max(1);
                if used.saturating_add(width) > usize::from(self.content_area.width) {
                    break;
                }
                let mut style = self.theme.style.patch(active);
                if selected.is_some_and(|(start, end)| index >= start && index < end) {
                    style = style.patch(self.theme.selection);
                }
                spans.push(Span::styled(character.to_string(), style));
                used = used.saturating_add(width);
            }
        }
        Line::from(spans).render(self.area, buffer);

        self.cursor_position = if self.focused && !self.content_area.is_empty() {
            let cursor_width = display_width_between(&self.text, self.scroll, self.cursor);
            Some(Position::new(
                self.content_area
                    .x
                    .saturating_add(cursor_width.min(usize::from(u16::MAX)) as u16)
                    .min(self.content_area.right().saturating_sub(1)),
                self.content_area.y,
            ))
        } else {
            None
        };
    }

    fn ensure_cursor_visible(&mut self) {
        let length = char_len(&self.text);
        self.cursor = self.cursor.min(length);
        self.scroll = self.scroll.min(length);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        let width = usize::from(self.content_area.width);
        if width == 0 {
            self.scroll = self.cursor;
            return;
        }
        while self.scroll < self.cursor
            && display_width_between(&self.text, self.scroll, self.cursor) >= width
        {
            self.scroll += 1;
        }
    }
}

/// Stateful Ratatui view returned by [`InputField::widget`].
pub struct InputFieldWidget<'a> {
    input: &'a mut InputField,
}

impl Widget for InputFieldWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.input.render(area, buffer);
    }
}

pub(crate) fn sanitize(text: String) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

pub(crate) fn char_len(text: &str) -> usize {
    text.chars().count()
}

pub(crate) fn char_to_byte(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

pub(crate) fn ordered(left: usize, right: usize) -> (usize, usize) {
    match left.cmp(&right) {
        Ordering::Greater => (right, left),
        Ordering::Less | Ordering::Equal => (left, right),
    }
}

pub(crate) fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).max(1))
        .sum()
}

fn display_width_between(text: &str, start: usize, end: usize) -> usize {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|character| character.width().unwrap_or(0).max(1))
        .sum()
}

pub(crate) fn take_width(text: &str, available: usize) -> String {
    let mut used = 0usize;
    text.chars()
        .take_while(|character| {
            let width = character.width().unwrap_or(0).max(1);
            let fits = used.saturating_add(width) <= available;
            if fits {
                used = used.saturating_add(width);
            }
            fits
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Word,
    Space,
    Punctuation,
}

fn word_class(character: char) -> WordClass {
    if character.is_alphanumeric() || matches!(character, '_' | '-') {
        WordClass::Word
    } else if character.is_whitespace() {
        WordClass::Space
    } else {
        WordClass::Punctuation
    }
}

pub(crate) fn word_bounds(text: &str, index: usize) -> (usize, usize) {
    let characters = text.chars().collect::<Vec<_>>();
    if index >= characters.len() {
        return (characters.len(), characters.len());
    }
    let class = word_class(characters[index]);
    let mut start = index;
    while start > 0 && word_class(characters[start - 1]) == class {
        start -= 1;
    }
    let mut end = index + 1;
    while end < characters.len() && word_class(characters[end]) == class {
        end += 1;
    }
    (start, end)
}

pub(crate) fn previous_word_boundary(text: &str, cursor: usize) -> usize {
    let characters = text.chars().collect::<Vec<_>>();
    let mut index = cursor.min(characters.len());
    while index > 0 && word_class(characters[index - 1]) == WordClass::Space {
        index -= 1;
    }
    if index == 0 {
        return 0;
    }
    let class = word_class(characters[index - 1]);
    while index > 0 && word_class(characters[index - 1]) == class {
        index -= 1;
    }
    index
}

pub(crate) fn next_word_boundary(text: &str, cursor: usize) -> usize {
    let characters = text.chars().collect::<Vec<_>>();
    let mut index = cursor.min(characters.len());
    if index < characters.len() {
        let class = word_class(characters[index]);
        while index < characters.len() && word_class(characters[index]) == class {
            index += 1;
        }
    }
    while index < characters.len() && word_class(characters[index]) == WordClass::Space {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    #[test]
    fn keyboard_editing_replaces_selection_and_is_unicode_safe() {
        let mut input = InputField::new("Filter");
        input.set_text("alpha 界 beta");
        input.handle(InputFieldAction::Home { extend: false });
        input.handle(InputFieldAction::Right {
            extend: false,
            word: true,
        });
        input.handle(InputFieldAction::Right {
            extend: true,
            word: true,
        });
        assert_eq!(input.selected_text(), Some("界 "));
        input.handle(InputFieldAction::InsertText("-".into()));
        assert_eq!(input.text(), "alpha -beta");
        input.handle(InputFieldAction::Backspace);
        assert_eq!(input.text(), "alpha beta");
        input.select_all();
        input.handle(InputFieldAction::Delete);
        assert_eq!(input.text(), "");
    }

    #[test]
    fn render_scrolls_to_cursor_and_uses_gray_selection() {
        let theme = InputFieldTheme::dark();
        let mut input = InputField::new("Filter files")
            .with_prompt("/ ")
            .with_theme(theme);
        input.set_text("a very long filename");
        input.set_focused(true);
        input.handle(InputFieldAction::Home { extend: false });
        for _ in 0..4 {
            input.handle(InputFieldAction::Right {
                extend: true,
                word: false,
            });
        }
        let area = Rect::new(0, 0, 12, 1);
        let mut buffer = Buffer::empty(area);
        input.widget().render(area, &mut buffer);

        assert_eq!(input.cursor_position(), Some(Position::new(8, 0)));
        let selected = theme.selection.bg.expect("selection background");
        assert!((4..8).all(|column| buffer[(column, 0)].bg == selected));
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    }

    #[test]
    fn mouse_drag_selects_and_double_click_selects_a_word() {
        let mut input = InputField::new("Filter").with_prompt("/ ");
        input.set_text("alpha beta");
        let area = Rect::new(0, 0, 30, 1);
        let mut buffer = Buffer::empty(area);
        input.widget().render(area, &mut buffer);

        let alpha = Position::new(5, 0);
        input.mouse_down(alpha, false);
        input.mouse_up();
        input.mouse_down(alpha, false);
        assert_eq!(input.selected_text(), Some("alpha"));
        input.mouse_up();

        input.mouse_down(Position::new(4, 0), false);
        input.mouse_drag(Position::new(12, 0));
        assert_eq!(input.selected_text(), Some("alpha be"));
        input.mouse_up();
    }
}
