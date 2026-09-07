//! Opinionated Markdown editing interactions shared by standalone terminal Apps.
//!
//! This layer deliberately stays smaller than a general rich-text toolkit. It
//! adds a closed block-insertion vocabulary, Markdown-aware Enter/Backspace,
//! and native-feeling pointer selection to [`MarkdownTextArea`].

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use tui_textarea::{CursorMove, Input, Key, TextArea};

use crate::{KitTheme, MarkdownTextArea};
#[cfg(feature = "ui-bridge")]
use crate::{
    MarkdownEditorActions, MarkdownMenuTrigger, SemanticMenu, SemanticMenuAnchor, SemanticMenuItem,
    SemanticMenuPresentation, UiEvent, UiEventKind, UiEventValue,
};

const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);

/// One action id shared by the terminal interaction controller and hosted renderers.
#[cfg(feature = "ui-bridge")]
pub const MARKDOWN_INSERT_SELECT_ACTION: &str = "markdown-menu-select";
/// App-owned dismissal action for the nested Markdown insert Menu.
#[cfg(feature = "ui-bridge")]
pub const MARKDOWN_INSERT_DISMISS_ACTION: &str = "markdown-menu-dismiss";

/// The closed set of blocks exposed by Markdown's `/` insert menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkdownBlockKind {
    Heading1,
    Heading2,
    Heading3,
    Heading4,
    Heading5,
    Heading6,
    Paragraph,
    BulletList,
    NumberedList,
    Todo,
    Quote,
    CodeBlock,
    Divider,
}

impl MarkdownBlockKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.item().label
    }

    #[must_use]
    pub const fn sample(self) -> &'static str {
        self.item().sample
    }

    const fn item(self) -> &'static MarkdownInsertItem {
        match self {
            Self::Heading1 => &MARKDOWN_INSERT_ITEMS[0],
            Self::Heading2 => &MARKDOWN_INSERT_ITEMS[1],
            Self::Heading3 => &MARKDOWN_INSERT_ITEMS[2],
            Self::Heading4 => &MARKDOWN_INSERT_ITEMS[3],
            Self::Heading5 => &MARKDOWN_INSERT_ITEMS[4],
            Self::Heading6 => &MARKDOWN_INSERT_ITEMS[5],
            Self::Paragraph => &MARKDOWN_INSERT_ITEMS[6],
            Self::BulletList => &MARKDOWN_INSERT_ITEMS[7],
            Self::NumberedList => &MARKDOWN_INSERT_ITEMS[8],
            Self::Todo => &MARKDOWN_INSERT_ITEMS[9],
            Self::Quote => &MARKDOWN_INSERT_ITEMS[10],
            Self::CodeBlock => &MARKDOWN_INSERT_ITEMS[11],
            Self::Divider => &MARKDOWN_INSERT_ITEMS[12],
        }
    }
}

/// One entry in the Markdown insert vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkdownInsertItem {
    pub kind: MarkdownBlockKind,
    pub shortcut: char,
    pub label: &'static str,
    pub sample: &'static str,
    pub aliases: &'static [&'static str],
    pub primary: bool,
}

/// Canonical insert-menu ordering used by the terminal, Swift, and web views.
pub const MARKDOWN_INSERT_ITEMS: &[MarkdownInsertItem] = &[
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Heading1,
        shortcut: '1',
        label: "Heading 1",
        sample: "#",
        aliases: &["h1", "1", "#", "heading 1", "heading1"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Heading2,
        shortcut: '2',
        label: "Heading 2",
        sample: "##",
        aliases: &["h2", "2", "##", "heading 2", "heading2"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Heading3,
        shortcut: '3',
        label: "Heading 3",
        sample: "###",
        aliases: &["h3", "3", "###", "heading 3", "heading3"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Heading4,
        shortcut: '4',
        label: "Heading 4",
        sample: "####",
        aliases: &["h4", "4", "####", "heading 4", "heading4"],
        primary: false,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Heading5,
        shortcut: '5',
        label: "Heading 5",
        sample: "#####",
        aliases: &["h5", "5", "#####", "heading 5", "heading5"],
        primary: false,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Heading6,
        shortcut: '6',
        label: "Heading 6",
        sample: "######",
        aliases: &["h6", "6", "######", "heading 6", "heading6"],
        primary: false,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Paragraph,
        shortcut: '0',
        label: "Text",
        sample: "paragraph",
        aliases: &["p", "0", "text", "body", "paragraph"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::BulletList,
        shortcut: 'b',
        label: "Bulleted list",
        sample: "-",
        aliases: &["bullet", "bulleted", "ul", "list", "-"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::NumberedList,
        shortcut: 'n',
        label: "Numbered list",
        sample: "1.",
        aliases: &["numbered", "ol", "number", "1"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Todo,
        shortcut: 't',
        label: "To-do",
        sample: "[]",
        aliases: &["todo", "to-do", "task", "check", "checkbox"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Quote,
        shortcut: 'q',
        label: "Quote",
        sample: ">",
        aliases: &["quote", "blockquote", ">"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::CodeBlock,
        shortcut: 'c',
        label: "Code",
        sample: "```",
        aliases: &["code", "fence", "pre"],
        primary: true,
    },
    MarkdownInsertItem {
        kind: MarkdownBlockKind::Divider,
        shortcut: '-',
        label: "Divider",
        sample: "---",
        aliases: &["divider", "hr", "line", "---"],
        primary: true,
    },
];

#[cfg(feature = "ui-bridge")]
const fn markdown_insert_item_id(kind: MarkdownBlockKind) -> &'static str {
    match kind {
        MarkdownBlockKind::Heading1 => "block-heading-1",
        MarkdownBlockKind::Heading2 => "block-heading-2",
        MarkdownBlockKind::Heading3 => "block-heading-3",
        MarkdownBlockKind::Heading4 => "block-heading-4",
        MarkdownBlockKind::Heading5 => "block-heading-5",
        MarkdownBlockKind::Heading6 => "block-heading-6",
        MarkdownBlockKind::Paragraph => "block-paragraph",
        MarkdownBlockKind::BulletList => "block-bullet-list",
        MarkdownBlockKind::NumberedList => "block-numbered-list",
        MarkdownBlockKind::Todo => "block-todo",
        MarkdownBlockKind::Quote => "block-quote",
        MarkdownBlockKind::CodeBlock => "block-code",
        MarkdownBlockKind::Divider => "block-divider",
    }
}

/// Result of routing one keyboard or pointer interaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkdownInteractionOutcome {
    #[default]
    Ignored,
    StateChanged,
    TextChanged,
}

/// A hosted Menu action could not be applied to the authoritative Markdown state.
#[cfg(feature = "ui-bridge")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownInteractionEventError {
    StaleRevision { expected: u64, received: u64 },
    InvalidAction(String),
}

#[cfg(feature = "ui-bridge")]
impl std::fmt::Display for MarkdownInteractionEventError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleRevision { expected, received } => write!(
                formatter,
                "Markdown changed from revision {received} to {expected}; retry the Menu action"
            ),
            Self::InvalidAction(message) => formatter.write_str(message),
        }
    }
}

#[cfg(feature = "ui-bridge")]
impl std::error::Error for MarkdownInteractionEventError {}

impl MarkdownInteractionOutcome {
    #[must_use]
    pub const fn is_handled(self) -> bool {
        !matches!(self, Self::Ignored)
    }

    #[must_use]
    pub const fn text_changed(self) -> bool {
        matches!(self, Self::TextChanged)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionDragKind {
    Character,
    Word,
    Line,
}

#[derive(Clone, Copy, Debug)]
struct SelectionDrag {
    kind: SelectionDragKind,
    anchor_start: (usize, usize),
    anchor_end: (usize, usize),
}

#[derive(Debug, Default)]
struct MarkdownInsertPopup {
    area: Rect,
    item_areas: Vec<Rect>,
    item_kinds: Vec<MarkdownBlockKind>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum MarkdownInsertOrigin {
    #[default]
    Slash,
    #[cfg(feature = "ui-bridge")]
    Palette,
}

impl MarkdownInsertPopup {
    const fn area(&self) -> Rect {
        self.area
    }

    fn item_at(&self, position: Position) -> Option<(usize, MarkdownBlockKind)> {
        self.item_areas
            .iter()
            .position(|area| area.contains(position))
            .and_then(|index| {
                self.item_kinds
                    .get(index)
                    .copied()
                    .map(|kind| (index, kind))
            })
    }
}

/// Stateful interaction controller for an opinionated [`MarkdownTextArea`].
///
/// Apps remain responsible for their event loop and persistence. Route keys
/// and pointer events here, render the editor normally, then call
/// [`Self::render_overlay`] so the `/` menu is drawn above it.
#[derive(Debug)]
pub struct MarkdownEditorInteraction {
    menu_open: bool,
    menu_origin: MarkdownInsertOrigin,
    menu_selected: usize,
    popup: Option<MarkdownInsertPopup>,
    drag: Option<SelectionDrag>,
    last_click: Option<(Instant, Position, u8)>,
}

impl Default for MarkdownEditorInteraction {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownEditorInteraction {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            menu_open: false,
            menu_origin: MarkdownInsertOrigin::Slash,
            menu_selected: 0,
            popup: None,
            drag: None,
            last_click: None,
        }
    }

    #[must_use]
    pub const fn is_insert_menu_open(&self) -> bool {
        self.menu_open
    }

    /// Projects the terminal interaction controller's current Menu verbatim
    /// into the closed component vocabulary consumed by Swift and web.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn semantic_insert_menu(&self, editor: &MarkdownTextArea<'_>) -> Option<SemanticMenu> {
        if !self.menu_open {
            return None;
        }
        let visible = self.visible_items(editor);
        let items = visible.iter().map(|item| {
            SemanticMenuItem::new(
                markdown_insert_item_id(item.kind),
                item.label,
                MARKDOWN_INSERT_SELECT_ACTION,
            )
            .hint(format!("{}  {}", item.shortcut, item.sample))
        });
        let mut menu = SemanticMenu::new("Insert block", items)
            .anchor(SemanticMenuAnchor::Caret)
            .presentation(SemanticMenuPresentation::Popup)
            .dismiss_action(MARKDOWN_INSERT_DISMISS_ACTION);
        if let Some(selected) = visible.get(self.menu_selected.min(visible.len().saturating_sub(1)))
        {
            menu = menu.selected_id(markdown_insert_item_id(selected.kind));
        }
        Some(menu)
    }

    /// Routes only the nested Menu actions owned by this interaction model.
    /// All other editor actions return `Ok(None)` for `MarkdownTextArea` to handle.
    #[cfg(feature = "ui-bridge")]
    pub fn handle_semantic_ui_event(
        &mut self,
        expected_revision: u64,
        editor_id: &str,
        editor: &mut MarkdownTextArea<'_>,
        event: &UiEvent,
    ) -> Result<Option<MarkdownInteractionOutcome>, MarkdownInteractionEventError> {
        let is_open = event.action.node_id.as_str() == editor_id
            && event.action.action.as_str() == MarkdownEditorActions::OPEN_MENU;
        let is_dismiss = event.action.node_id.as_str() == editor_id
            && event.action.action.as_str() == MARKDOWN_INSERT_DISMISS_ACTION;
        let is_selection = event.action.action.as_str() == MARKDOWN_INSERT_SELECT_ACTION;
        if !is_open && !is_dismiss && !is_selection {
            return Ok(None);
        }
        if event.base_revision != expected_revision {
            return Err(MarkdownInteractionEventError::StaleRevision {
                expected: expected_revision,
                received: event.base_revision,
            });
        }

        if is_open {
            let UiEventValue::Text(trigger) = &event.action.value else {
                return Err(MarkdownInteractionEventError::InvalidAction(
                    "Markdown Menu trigger requires a text value".to_owned(),
                ));
            };
            if event.action.kind != UiEventKind::Command {
                return Err(MarkdownInteractionEventError::InvalidAction(
                    "Markdown Menu trigger requires a command event".to_owned(),
                ));
            }
            let trigger = match trigger.as_str() {
                "slash" => MarkdownMenuTrigger::Slash,
                "palette" => MarkdownMenuTrigger::Palette,
                other => {
                    return Err(MarkdownInteractionEventError::InvalidAction(format!(
                        "unknown Markdown Menu trigger {other}"
                    )));
                }
            };
            return Ok(Some(self.request_semantic_menu(editor, trigger)));
        }

        if is_dismiss {
            if event.action.kind != UiEventKind::Cancel || event.action.value != UiEventValue::None
            {
                return Err(MarkdownInteractionEventError::InvalidAction(
                    "Markdown Menu dismissal requires a cancel event".to_owned(),
                ));
            }
            let before = document(editor);
            if self.menu_open && self.menu_origin == MarkdownInsertOrigin::Slash {
                clear_slash_command(editor.text_area_mut());
            }
            self.close_menu();
            return Ok(Some(outcome(true, before != document(editor))));
        }

        if event.action.kind != UiEventKind::Activate || event.action.value != UiEventValue::None {
            return Err(MarkdownInteractionEventError::InvalidAction(
                "Markdown Menu selection requires an activate event".to_owned(),
            ));
        }
        if !self.menu_open {
            return Err(MarkdownInteractionEventError::InvalidAction(
                "Markdown Menu is no longer open".to_owned(),
            ));
        }
        let Some(kind) = self
            .visible_items(editor)
            .into_iter()
            .find(|item| markdown_insert_item_id(item.kind) == event.action.node_id.as_str())
            .map(|item| item.kind)
        else {
            return Err(MarkdownInteractionEventError::InvalidAction(
                "Markdown Menu item is no longer visible".to_owned(),
            ));
        };
        let before = document(editor);
        apply_slash(editor.text_area_mut(), kind);
        self.close_menu();
        Ok(Some(outcome(true, before != document(editor))))
    }

    #[cfg(feature = "ui-bridge")]
    fn request_semantic_menu(
        &mut self,
        editor: &mut MarkdownTextArea<'_>,
        trigger: MarkdownMenuTrigger,
    ) -> MarkdownInteractionOutcome {
        let before = document(editor);
        if can_open_slash(editor.text_area()) {
            if trigger == MarkdownMenuTrigger::Slash {
                editor.insert_char('/');
                self.menu_origin = MarkdownInsertOrigin::Slash;
            } else {
                self.menu_origin = MarkdownInsertOrigin::Palette;
            }
            self.menu_open = true;
            self.menu_selected = 0;
            self.popup = None;
        } else {
            editor.insert_char(match trigger {
                MarkdownMenuTrigger::Slash => '/',
                MarkdownMenuTrigger::Palette => '\\',
            });
            apply_markdown_shortcut(editor.text_area_mut());
        }
        outcome(true, before != document(editor))
    }

    /// Routes one `tui-textarea` input through Markdown-aware editing rules.
    pub fn handle_input(
        &mut self,
        editor: &mut MarkdownTextArea<'_>,
        input: Input,
    ) -> MarkdownInteractionOutcome {
        let before = document(editor);
        let handled = if self.menu_open {
            self.handle_menu_input(editor, input)
        } else {
            self.handle_editor_input(editor, input)
        };
        outcome(handled, before != document(editor))
    }

    /// Inserts a paste and keeps an open slash menu synchronized.
    pub fn handle_paste(
        &mut self,
        editor: &mut MarkdownTextArea<'_>,
        text: &str,
    ) -> MarkdownInteractionOutcome {
        let before = document(editor);
        editor.text_area_mut().insert_str(text);
        if self.menu_open {
            self.sync_menu(editor);
        }
        outcome(true, before != document(editor))
    }

    /// Handles a primary-button press. `extend_selection` implements Shift-click.
    pub fn pointer_down(
        &mut self,
        editor: &mut MarkdownTextArea<'_>,
        position: Position,
        extend_selection: bool,
    ) -> MarkdownInteractionOutcome {
        let before = document(editor);
        if self.menu_open {
            let clicked = self
                .popup
                .as_ref()
                .and_then(|popup| popup.item_at(position));
            if let Some((index, kind)) = clicked {
                self.menu_selected = index;
                apply_slash(editor.text_area_mut(), kind);
                self.close_menu();
                return outcome(true, before != document(editor));
            }
            if self
                .popup
                .as_ref()
                .is_some_and(|popup| popup.area().contains(position))
            {
                return MarkdownInteractionOutcome::StateChanged;
            }
            self.close_menu();
        }
        if !editor.contains(position) {
            return outcome(false, before != document(editor));
        }

        let (row, column) = editor.hit_test(position);
        let clicks = self.register_click(position);
        match (clicks, extend_selection) {
            (1, true) => {
                if !editor.is_selecting() {
                    editor.start_selection();
                }
                jump(editor.text_area_mut(), row, column);
                let anchor = selection_anchor(editor.text_area_mut());
                self.drag = Some(SelectionDrag {
                    kind: SelectionDragKind::Character,
                    anchor_start: anchor,
                    anchor_end: anchor,
                });
            }
            (2, _) => {
                let (start, end) = select_word(editor.text_area_mut(), row, column);
                self.drag = Some(SelectionDrag {
                    kind: SelectionDragKind::Word,
                    anchor_start: (row, start),
                    anchor_end: (row, end),
                });
            }
            (3, _) => {
                let end = select_line(editor.text_area_mut(), row);
                self.drag = Some(SelectionDrag {
                    kind: SelectionDragKind::Line,
                    anchor_start: (row, 0),
                    anchor_end: (row, end),
                });
            }
            _ => {
                editor.cancel_selection();
                jump(editor.text_area_mut(), row, column);
                editor.start_selection();
                self.drag = Some(SelectionDrag {
                    kind: SelectionDragKind::Character,
                    anchor_start: (row, column),
                    anchor_end: (row, column),
                });
            }
        }
        MarkdownInteractionOutcome::StateChanged
    }

    /// Updates hover selection in the insert menu and an active text drag.
    pub fn pointer_move(
        &mut self,
        editor: &mut MarkdownTextArea<'_>,
        position: Position,
    ) -> MarkdownInteractionOutcome {
        let hovered = self
            .popup
            .as_ref()
            .and_then(|popup| popup.item_at(position))
            .map(|(index, _)| index);
        let mut changed = hovered.is_some_and(|index| {
            let changed = self.menu_selected != index;
            self.menu_selected = index;
            changed
        });
        if self.drag.is_some() {
            self.pointer_drag(editor, position);
            changed = true;
        }
        if changed {
            MarkdownInteractionOutcome::StateChanged
        } else {
            MarkdownInteractionOutcome::Ignored
        }
    }

    /// Extends the current character, word, or line selection.
    pub fn pointer_drag(
        &mut self,
        editor: &mut MarkdownTextArea<'_>,
        position: Position,
    ) -> MarkdownInteractionOutcome {
        let Some(drag) = self.drag else {
            return MarkdownInteractionOutcome::Ignored;
        };
        editor.auto_scroll(position);
        let (row, column) = editor.hit_test(position);
        match drag.kind {
            SelectionDragKind::Character => jump(editor.text_area_mut(), row, column),
            SelectionDragKind::Word => {
                let line = editor.lines().get(row).map(String::as_str).unwrap_or("");
                let (word_start, word_end) = word_bounds(line, column);
                let start = if position_le((row, column), drag.anchor_start) {
                    (row, word_start)
                } else {
                    drag.anchor_start
                };
                let end = if position_le(drag.anchor_end, (row, column)) {
                    (row, word_end)
                } else {
                    drag.anchor_end
                };
                set_selection(editor.text_area_mut(), start, end);
            }
            SelectionDragKind::Line => {
                let start_row = drag.anchor_start.0.min(row);
                let end_row = drag.anchor_start.0.max(row);
                let end_column = line_len(editor.text_area_mut(), end_row);
                set_selection(
                    editor.text_area_mut(),
                    (start_row, 0),
                    (end_row, end_column),
                );
            }
        }
        MarkdownInteractionOutcome::StateChanged
    }

    /// Completes a primary-button selection drag.
    pub fn pointer_up(&mut self, editor: &mut MarkdownTextArea<'_>) -> MarkdownInteractionOutcome {
        let had_drag = self.drag.take().is_some();
        if editor
            .selection_range()
            .is_some_and(|(start, end)| start == end)
        {
            editor.cancel_selection();
        }
        if had_drag {
            MarkdownInteractionOutcome::StateChanged
        } else {
            MarkdownInteractionOutcome::Ignored
        }
    }

    /// Scrolls the editor or moves through the menu under the pointer.
    pub fn pointer_scroll(
        &mut self,
        editor: &mut MarkdownTextArea<'_>,
        position: Position,
        rows: i16,
        extend_selection: bool,
    ) -> MarkdownInteractionOutcome {
        if self.menu_open
            && self
                .popup
                .as_ref()
                .is_some_and(|popup| popup.area().contains(position))
        {
            self.move_menu(if rows < 0 { -1 } else { 1 }, editor);
            return MarkdownInteractionOutcome::StateChanged;
        }
        if editor.contains(position) && editor.scroll_lines_with_selection(rows, extend_selection) {
            MarkdownInteractionOutcome::StateChanged
        } else {
            MarkdownInteractionOutcome::Ignored
        }
    }

    /// Draws the slash-command popover after the editor has established its cursor position.
    pub fn render_overlay(&mut self, editor: &MarkdownTextArea<'_>, frame: &mut Frame<'_>) {
        if !self.menu_open {
            self.popup = None;
            return;
        }
        let Some(anchor) = editor.rendered_cursor_position() else {
            return;
        };
        let visible = self.visible_items(editor);
        self.menu_selected = self.menu_selected.min(visible.len().saturating_sub(1));
        self.popup = Some(render_insert_popup(
            frame,
            anchor,
            &visible,
            self.menu_selected,
            KitTheme::detected(),
        ));
    }

    fn handle_editor_input(&mut self, editor: &mut MarkdownTextArea<'_>, input: Input) -> bool {
        match input {
            Input {
                key: Key::Enter, ..
            } => {
                handle_enter(editor.text_area_mut());
                true
            }
            Input {
                key: Key::Backspace,
                ..
            } => {
                handle_backspace(editor.text_area_mut());
                true
            }
            Input {
                key: Key::Char('/'),
                ctrl: false,
                alt: false,
                ..
            } if can_open_slash(editor.text_area_mut()) => {
                editor.insert_char('/');
                self.menu_open = true;
                self.menu_origin = MarkdownInsertOrigin::Slash;
                self.menu_selected = 0;
                self.popup = None;
                true
            }
            Input {
                key: Key::Char(character),
                ctrl: false,
                ..
            } if !character.is_control() => {
                editor.insert_char(character);
                apply_markdown_shortcut(editor.text_area_mut());
                true
            }
            other => editor.input(other),
        }
    }

    fn handle_menu_input(&mut self, editor: &mut MarkdownTextArea<'_>, input: Input) -> bool {
        match input {
            Input { key: Key::Esc, .. } => {
                if self.menu_origin == MarkdownInsertOrigin::Slash {
                    clear_slash_command(editor.text_area_mut());
                }
                self.close_menu();
                true
            }
            Input { key: Key::Up, .. } => {
                self.move_menu(-1, editor);
                true
            }
            Input { key: Key::Down, .. } => {
                self.move_menu(1, editor);
                true
            }
            Input { key: Key::Home, .. } => {
                self.menu_selected = 0;
                true
            }
            Input { key: Key::End, .. } => {
                self.menu_selected = self.visible_items(editor).len().saturating_sub(1);
                true
            }
            Input {
                key: Key::Enter | Key::Tab,
                ..
            } => {
                if let Some(kind) = self.selected_kind(editor) {
                    apply_slash(editor.text_area_mut(), kind);
                }
                self.close_menu();
                true
            }
            Input {
                key: Key::Char(character @ '1'..='6'),
                ctrl: false,
                alt: false,
                ..
            } => {
                let kind = match character {
                    '1' => MarkdownBlockKind::Heading1,
                    '2' => MarkdownBlockKind::Heading2,
                    '3' => MarkdownBlockKind::Heading3,
                    '4' => MarkdownBlockKind::Heading4,
                    '5' => MarkdownBlockKind::Heading5,
                    _ => MarkdownBlockKind::Heading6,
                };
                apply_slash(editor.text_area_mut(), kind);
                self.close_menu();
                true
            }
            other => {
                let handled = editor.input(other);
                self.sync_menu(editor);
                handled
            }
        }
    }

    fn visible_items(&self, editor: &MarkdownTextArea<'_>) -> Vec<&'static MarkdownInsertItem> {
        let query = match self.menu_origin {
            MarkdownInsertOrigin::Slash => slash_query(editor.text_area()).unwrap_or_default(),
            #[cfg(feature = "ui-bridge")]
            MarkdownInsertOrigin::Palette => String::new(),
        };
        visible_markdown_insert_items(&query)
    }

    fn selected_kind(&self, editor: &MarkdownTextArea<'_>) -> Option<MarkdownBlockKind> {
        self.visible_items(editor)
            .get(self.menu_selected)
            .map(|item| item.kind)
    }

    fn move_menu(&mut self, delta: isize, editor: &MarkdownTextArea<'_>) {
        let count = self.visible_items(editor).len();
        if count == 0 {
            self.menu_selected = 0;
            return;
        }
        let current = self.menu_selected.min(count - 1);
        self.menu_selected = if delta < 0 {
            current.checked_sub(1).unwrap_or(count - 1)
        } else {
            (current + 1) % count
        };
    }

    fn sync_menu(&mut self, editor: &MarkdownTextArea<'_>) {
        if self.menu_origin == MarkdownInsertOrigin::Slash
            && slash_query(editor.text_area()).is_none()
        {
            self.close_menu();
            return;
        }
        self.menu_selected = self
            .menu_selected
            .min(self.visible_items(editor).len().saturating_sub(1));
        self.popup = None;
    }

    fn close_menu(&mut self) {
        self.menu_open = false;
        self.menu_origin = MarkdownInsertOrigin::Slash;
        self.menu_selected = 0;
        self.popup = None;
    }

    fn register_click(&mut self, position: Position) -> u8 {
        let now = Instant::now();
        let count = match self.last_click {
            Some((at, previous, count))
                if now.duration_since(at) <= MULTI_CLICK_INTERVAL
                    && position.x.abs_diff(previous.x) <= 1
                    && position.y.abs_diff(previous.y) <= 1 =>
            {
                count.saturating_add(1).min(3)
            }
            _ => 1,
        };
        self.last_click = Some((now, position, count));
        count
    }
}

const MARKDOWN_INSERT_MENU_WIDTH: u16 = 38;

fn render_insert_popup(
    frame: &mut Frame<'_>,
    anchor: Position,
    items: &[&MarkdownInsertItem],
    selected: usize,
    theme: KitTheme,
) -> MarkdownInsertPopup {
    let bounds = frame.area();
    if bounds.is_empty() {
        return MarkdownInsertPopup::default();
    }
    let rows = u16::try_from(items.len().max(1)).unwrap_or(u16::MAX);
    let height = rows.saturating_add(2).min(bounds.height);
    let width = MARKDOWN_INSERT_MENU_WIDTH.min(bounds.width);
    let preferred_x = anchor.x.saturating_sub(1).max(bounds.x);
    let x = preferred_x.min(bounds.right().saturating_sub(width));
    let below = anchor.y.saturating_add(1);
    let y = if below.saturating_add(height) <= bounds.bottom() {
        below
    } else {
        anchor
            .y
            .saturating_sub(height)
            .max(bounds.y)
            .min(bounds.bottom().saturating_sub(height))
    };
    let area = Rect::new(x, y, width, height);
    let inner = area.inner(Margin::new(1, 1));
    let selected = selected.min(items.len().saturating_sub(1));
    let mut item_areas = Vec::with_capacity(items.len());
    let mut item_kinds = Vec::with_capacity(items.len());
    let mut lines = Vec::with_capacity(items.len().max(1));
    let background = Style::new().fg(theme.text).bg(theme.surface);

    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            " no matching block",
            Style::new().fg(theme.muted).bg(theme.surface),
        )));
    } else {
        let row_width = usize::from(inner.width);
        for (index, item) in items.iter().take(usize::from(inner.height)).enumerate() {
            item_areas.push(Rect::new(
                inner.x,
                inner
                    .y
                    .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                inner.width,
                1,
            ));
            item_kinds.push(item.kind);
            let content = format!(" {}  {:<14}  {}", item.shortcut, item.label, item.sample);
            let padding = row_width.saturating_sub(content.chars().count());
            let text = format!("{content}{}", " ".repeat(padding));
            let style = if index == selected {
                theme.selected_row.add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme.muted).bg(theme.surface)
            };
            lines.push(Line::from(Span::styled(text, style)));
        }
    }

    let block = Block::bordered()
        .title(" insert ")
        .title_style(
            Style::new()
                .fg(theme.text)
                .bg(theme.surface)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::new().fg(theme.subtle).bg(theme.surface))
        .style(background);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).style(background).block(block), area);
    MarkdownInsertPopup {
        area,
        item_areas,
        item_kinds,
    }
}

/// Filters the canonical insert vocabulary using labels and stable aliases.
#[must_use]
pub fn visible_markdown_insert_items(query: &str) -> Vec<&'static MarkdownInsertItem> {
    let query = query.trim();
    MARKDOWN_INSERT_ITEMS
        .iter()
        .filter(|item| {
            if query.is_empty() {
                item.primary
            } else {
                item_matches(item, query)
            }
        })
        .collect()
}

fn item_matches(item: &MarkdownInsertItem, query: &str) -> bool {
    let query = query.to_ascii_lowercase();
    item.label.to_ascii_lowercase().contains(&query)
        || item.aliases.iter().any(|alias| {
            *alias == query
                || (!query.starts_with('#') && alias.to_ascii_lowercase().starts_with(&query))
        })
}

fn outcome(handled: bool, text_changed: bool) -> MarkdownInteractionOutcome {
    if text_changed {
        MarkdownInteractionOutcome::TextChanged
    } else if handled {
        MarkdownInteractionOutcome::StateChanged
    } else {
        MarkdownInteractionOutcome::Ignored
    }
}

fn document(editor: &MarkdownTextArea<'_>) -> String {
    editor.lines().join("\n")
}

fn can_open_slash(editor: &TextArea<'_>) -> bool {
    let row = editor.cursor().0;
    !editor.is_selecting()
        && editor
            .lines()
            .get(row)
            .is_some_and(|line| line.trim().is_empty())
        && !in_code_fence(editor.lines(), row)
}

fn slash_query(editor: &TextArea<'_>) -> Option<String> {
    let (row, column) = editor.cursor();
    let line = editor.lines().get(row)?;
    let indent_bytes = line.len().saturating_sub(line.trim_start().len());
    let indent_columns = line[..indent_bytes].chars().count();
    if column < indent_columns {
        return None;
    }
    line[indent_bytes..]
        .strip_prefix('/')
        .map(ToOwned::to_owned)
}

fn in_code_fence(lines: &[String], row: usize) -> bool {
    let mut in_fence = false;
    for (index, line) in lines.iter().enumerate() {
        if index > row {
            break;
        }
        if line.trim_start().starts_with("```") {
            if index == row {
                return true;
            }
            in_fence = !in_fence;
        }
    }
    in_fence
}

fn clear_slash_command(editor: &mut TextArea<'_>) {
    let (row, _) = editor.cursor();
    let indent = editor
        .lines()
        .get(row)
        .map(|line| split_indent(line).0.to_owned())
        .unwrap_or_default();
    replace_current_line(editor, &indent);
}

fn apply_slash(editor: &mut TextArea<'_>, kind: MarkdownBlockKind) {
    let (row, _) = editor.cursor();
    let indent = editor
        .lines()
        .get(row)
        .map(|line| split_indent(line).0.to_owned())
        .unwrap_or_default();
    let replacement = match kind {
        MarkdownBlockKind::Heading1 => format!("{indent}# "),
        MarkdownBlockKind::Heading2 => format!("{indent}## "),
        MarkdownBlockKind::Heading3 => format!("{indent}### "),
        MarkdownBlockKind::Heading4 => format!("{indent}#### "),
        MarkdownBlockKind::Heading5 => format!("{indent}##### "),
        MarkdownBlockKind::Heading6 => format!("{indent}###### "),
        MarkdownBlockKind::Paragraph => indent,
        MarkdownBlockKind::BulletList => format!("{indent}- "),
        MarkdownBlockKind::NumberedList => format!("{indent}1. "),
        MarkdownBlockKind::Todo => format!("{indent}- [ ] "),
        MarkdownBlockKind::Quote => format!("{indent}> "),
        MarkdownBlockKind::CodeBlock => format!("{indent}```\n\n{indent}```"),
        MarkdownBlockKind::Divider => format!("{indent}---"),
    };
    replace_current_line(editor, &replacement);
    if kind == MarkdownBlockKind::CodeBlock {
        editor.move_cursor(CursorMove::Up);
    }
}

fn handle_enter(editor: &mut TextArea<'_>) {
    let (row, _) = editor.cursor();
    let line = editor.lines().get(row).cloned().unwrap_or_default();
    let parsed = parse_block(&line);
    if parsed.body.is_empty()
        && matches!(
            parsed.kind,
            ParsedKind::Bullet | ParsedKind::Numbered(_) | ParsedKind::Todo | ParsedKind::Quote
        )
    {
        replace_current_line(editor, parsed.indent);
        return;
    }
    let prefix = match parsed.kind {
        ParsedKind::Bullet => Some(format!("{}- ", parsed.indent)),
        ParsedKind::Numbered(number) => {
            Some(format!("{}{}. ", parsed.indent, number.saturating_add(1)))
        }
        ParsedKind::Todo => Some(format!("{}- [ ] ", parsed.indent)),
        ParsedKind::Quote => Some(format!("{}> ", parsed.indent)),
        _ => None,
    };
    editor.insert_newline();
    if let Some(prefix) = prefix {
        editor.insert_str(prefix);
    }
}

fn handle_backspace(editor: &mut TextArea<'_>) {
    if editor.is_selecting() {
        editor.input(Input {
            key: Key::Backspace,
            ..Input::default()
        });
        return;
    }
    let (row, column) = editor.cursor();
    let line = editor.lines().get(row).cloned().unwrap_or_default();
    let parsed = parse_block(&line);
    if !matches!(
        parsed.kind,
        ParsedKind::Paragraph | ParsedKind::Code | ParsedKind::Divider
    ) && column > 0
        && column <= parsed.prefix_columns
    {
        let replacement = format!("{}{}", parsed.indent, parsed.body);
        let indent_columns = parsed.indent.chars().count();
        replace_current_line(editor, &replacement);
        jump(editor, row, indent_columns);
        return;
    }
    editor.input(Input {
        key: Key::Backspace,
        ..Input::default()
    });
}

fn apply_markdown_shortcut(editor: &mut TextArea<'_>) {
    let (row, _) = editor.cursor();
    let Some(line) = editor.lines().get(row) else {
        return;
    };
    let (indent, rest) = split_indent(line);
    let replacement = match rest {
        "[] " | "[ ] " => Some(format!("{indent}- [ ] ")),
        "[x] " | "[X] " => Some(format!("{indent}- [x] ")),
        _ => None,
    };
    if let Some(replacement) = replacement {
        replace_current_line(editor, &replacement);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParsedKind {
    Paragraph,
    Heading,
    Bullet,
    Numbered(u32),
    Todo,
    Quote,
    Code,
    Divider,
}

struct ParsedBlock<'a> {
    indent: &'a str,
    kind: ParsedKind,
    body: &'a str,
    prefix_columns: usize,
}

fn parse_block(line: &str) -> ParsedBlock<'_> {
    let (indent, rest) = split_indent(line);
    let indent_columns = indent.chars().count();
    if is_divider(rest) {
        return ParsedBlock {
            indent,
            kind: ParsedKind::Divider,
            body: "",
            prefix_columns: line.chars().count(),
        };
    }
    if rest.starts_with("```") {
        return ParsedBlock {
            indent,
            kind: ParsedKind::Code,
            body: rest,
            prefix_columns: indent_columns,
        };
    }
    let hashes = rest
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if (1..=6).contains(&hashes) && rest.chars().nth(hashes).is_some_and(char::is_whitespace) {
        let body = rest
            .char_indices()
            .nth(hashes)
            .map(|(offset, _)| rest[offset..].trim_start())
            .unwrap_or("");
        return ParsedBlock {
            indent,
            kind: ParsedKind::Heading,
            body,
            prefix_columns: indent_columns + hashes + 1,
        };
    }
    for (marker, kind) in [
        ("- [ ] ", ParsedKind::Todo),
        ("- [x] ", ParsedKind::Todo),
        ("- [X] ", ParsedKind::Todo),
    ] {
        if let Some(body) = rest.strip_prefix(marker) {
            return ParsedBlock {
                indent,
                kind,
                body,
                prefix_columns: indent_columns + marker.chars().count(),
            };
        }
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(body) = rest.strip_prefix(marker) {
            return ParsedBlock {
                indent,
                kind: ParsedKind::Bullet,
                body,
                prefix_columns: indent_columns + marker.chars().count(),
            };
        }
    }
    let digit_count = rest.chars().take_while(char::is_ascii_digit).count();
    if digit_count > 0 {
        let digits = &rest[..digit_count];
        let marker = format!("{digits}. ");
        if let Some(body) = rest.strip_prefix(&marker)
            && let Ok(number) = digits.parse()
        {
            return ParsedBlock {
                indent,
                kind: ParsedKind::Numbered(number),
                body,
                prefix_columns: indent_columns + marker.chars().count(),
            };
        }
    }
    if let Some(body) = rest.strip_prefix("> ") {
        return ParsedBlock {
            indent,
            kind: ParsedKind::Quote,
            body,
            prefix_columns: indent_columns + 2,
        };
    }
    ParsedBlock {
        indent,
        kind: ParsedKind::Paragraph,
        body: rest,
        prefix_columns: indent_columns,
    }
}

fn split_indent(line: &str) -> (&str, &str) {
    let offset = line
        .char_indices()
        .find_map(|(offset, character)| (!matches!(character, ' ' | '\t')).then_some(offset))
        .unwrap_or(line.len());
    line.split_at(offset)
}

fn is_divider(rest: &str) -> bool {
    let trimmed = rest.trim();
    let mut characters = trimmed.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    matches!(first, '-' | '*' | '_')
        && trimmed.chars().count() >= 3
        && characters.all(|character| character == first)
}

fn replace_current_line(editor: &mut TextArea<'_>, replacement: &str) {
    let row = editor.cursor().0;
    editor.cancel_selection();
    jump(editor, row, 0);
    editor.start_selection();
    editor.move_cursor(CursorMove::End);
    editor.cut();
    editor.insert_str(replacement);
}

fn jump(editor: &mut TextArea<'_>, row: usize, column: usize) {
    editor.move_cursor(CursorMove::Jump(clamp_u16(row), clamp_u16(column)));
}

fn set_selection(editor: &mut TextArea<'_>, start: (usize, usize), end: (usize, usize)) {
    editor.cancel_selection();
    jump(editor, start.0, start.1);
    editor.start_selection();
    jump(editor, end.0, end.1);
}

fn select_word(editor: &mut TextArea<'_>, row: usize, column: usize) -> (usize, usize) {
    let line = editor.lines().get(row).map(String::as_str).unwrap_or("");
    let (start, end) = word_bounds(line, column);
    set_selection(editor, (row, start), (row, end));
    (start, end)
}

fn select_line(editor: &mut TextArea<'_>, row: usize) -> usize {
    let end = line_len(editor, row);
    set_selection(editor, (row, 0), (row, end));
    end
}

fn line_len(editor: &TextArea<'_>, row: usize) -> usize {
    editor
        .lines()
        .get(row)
        .map(|line| line.chars().count())
        .unwrap_or(0)
}

fn selection_anchor(editor: &TextArea<'_>) -> (usize, usize) {
    editor
        .selection_range()
        .map(|(start, _)| start)
        .unwrap_or_else(|| editor.cursor())
}

fn word_bounds(line: &str, column: usize) -> (usize, usize) {
    let characters = line.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return (0, 0);
    }
    let index = column.min(characters.len() - 1);
    let class = character_class(characters[index]);
    let start = (0..=index)
        .rev()
        .take_while(|candidate| character_class(characters[*candidate]) == class)
        .last()
        .unwrap_or(index);
    let end = (index..characters.len())
        .take_while(|candidate| character_class(characters[*candidate]) == class)
        .last()
        .map(|candidate| candidate + 1)
        .unwrap_or(index + 1);
    (start, end)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharacterClass {
    Word,
    Space,
    Other,
}

fn character_class(character: char) -> CharacterClass {
    if character.is_whitespace() {
        CharacterClass::Space
    } else if character.is_alphanumeric() || character == '_' {
        CharacterClass::Word
    } else {
        CharacterClass::Other
    }
}

const fn position_le(left: (usize, usize), right: (usize, usize)) -> bool {
    left.0 < right.0 || (left.0 == right.0 && left.1 <= right.1)
}

fn clamp_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use super::*;
    use crate::MarkdownTextAreaStyle;

    fn editor(lines: &[&str]) -> MarkdownTextArea<'static> {
        MarkdownTextArea::new(lines.iter().copied(), MarkdownTextAreaStyle::default())
    }

    #[test]
    fn slash_menu_filters_and_applies_a_todo() {
        let mut editor = editor(&[""]);
        let mut interaction = MarkdownEditorInteraction::new();
        interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Char('/'),
                ..Input::default()
            },
        );
        for character in "todo".chars() {
            interaction.handle_input(
                &mut editor,
                Input {
                    key: Key::Char(character),
                    ..Input::default()
                },
            );
        }
        assert!(interaction.is_insert_menu_open());
        assert_eq!(
            visible_markdown_insert_items("todo")[0].kind,
            MarkdownBlockKind::Todo
        );
        let outcome = interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Enter,
                ..Input::default()
            },
        );
        assert!(outcome.text_changed());
        assert_eq!(editor.lines(), &["- [ ] "]);
    }

    #[test]
    #[cfg(feature = "ui-bridge")]
    fn hosted_slash_and_selection_use_the_same_interaction_state_as_the_tui() {
        let mut editor = editor(&[""]);
        let mut interaction = MarkdownEditorInteraction::new();
        let slash = crate::UiEvent::new(
            "app-test",
            "person-test",
            "client-test",
            "renderer-test",
            "main",
            "event-slash",
            4,
            crate::UiAction::new(
                "markdown-editor",
                MarkdownEditorActions::OPEN_MENU,
                UiEventKind::Command,
                UiEventValue::Text("slash".to_owned()),
            ),
        );
        assert_eq!(
            interaction
                .handle_semantic_ui_event(4, "markdown-editor", &mut editor, &slash)
                .unwrap(),
            Some(MarkdownInteractionOutcome::TextChanged)
        );
        assert_eq!(editor.lines(), &["/"]);
        let menu = interaction.semantic_insert_menu(&editor).unwrap();
        assert_eq!(menu.selected_id.as_deref(), Some("block-heading-1"));
        assert_eq!(menu.items[0].action, MARKDOWN_INSERT_SELECT_ACTION);

        let select = crate::UiEvent::new(
            "app-test",
            "person-test",
            "client-test",
            "renderer-test",
            "main",
            "event-select",
            5,
            crate::UiAction::activate("block-heading-1", MARKDOWN_INSERT_SELECT_ACTION),
        );
        assert_eq!(
            interaction
                .handle_semantic_ui_event(5, "markdown-editor", &mut editor, &select)
                .unwrap(),
            Some(MarkdownInteractionOutcome::TextChanged)
        );
        assert_eq!(editor.lines(), &["# "]);
        assert!(interaction.semantic_insert_menu(&editor).is_none());
    }

    #[test]
    fn slash_menu_uses_the_compact_bordered_dropdown_and_full_row_selection() {
        let mut editor = editor(&[""]);
        let mut interaction = MarkdownEditorInteraction::new();
        interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Char('/'),
                ..Input::default()
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
        terminal
            .draw(|frame| {
                editor.render(frame, frame.area(), true);
                interaction.render_overlay(&editor, frame);
            })
            .unwrap();

        let popup = interaction.popup.as_ref().unwrap();
        let anchor = editor.rendered_cursor_position().unwrap();
        assert_eq!(popup.area.x, anchor.x.saturating_sub(1));
        assert_eq!(
            terminal.backend().buffer()[(popup.area.x, popup.area.y)].symbol(),
            "┌"
        );
        let selected = popup.item_areas[0];
        assert_eq!(
            terminal.backend().buffer()[(selected.right() - 1, selected.y)].bg,
            ratatui::style::Color::Rgb(63, 63, 70)
        );
        let row = (selected.x..selected.right())
            .map(|x| terminal.backend().buffer()[(x, selected.y)].symbol())
            .collect::<String>();
        assert!(row.contains("1  Heading 1"));
        assert!(row.trim_end().ends_with('#'));
    }

    #[test]
    fn backspace_at_a_marker_returns_to_plain_text() {
        let mut editor = editor(&["- [ ] task"]);
        jump(editor.text_area_mut(), 0, 4);
        let mut interaction = MarkdownEditorInteraction::new();
        let outcome = interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Backspace,
                ..Input::default()
            },
        );
        assert!(outcome.text_changed());
        assert_eq!(editor.lines(), &["task"]);
    }

    #[test]
    fn backspace_removes_the_slash_and_closes_an_empty_menu() {
        let mut editor = editor(&[""]);
        let mut interaction = MarkdownEditorInteraction::new();
        interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Char('/'),
                ..Input::default()
            },
        );
        assert!(interaction.is_insert_menu_open());
        let outcome = interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Backspace,
                ..Input::default()
            },
        );
        assert!(outcome.text_changed());
        assert!(!interaction.is_insert_menu_open());
        assert_eq!(editor.lines(), &[""]);
    }

    #[test]
    fn enter_continues_markdown_lists() {
        let mut editor = editor(&["2. item"]);
        jump(editor.text_area_mut(), 0, 7);
        let mut interaction = MarkdownEditorInteraction::new();
        interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Enter,
                ..Input::default()
            },
        );
        assert_eq!(editor.lines(), &["2. item", "3. "]);
    }

    #[test]
    fn pointer_double_click_selects_a_word() {
        let mut editor = editor(&["alpha beta"]);
        let mut interaction = MarkdownEditorInteraction::new();
        let mut terminal = Terminal::new(TestBackend::new(30, 5)).unwrap();
        terminal
            .draw(|frame| editor.render(frame, Rect::new(0, 0, 30, 5), true))
            .unwrap();
        let point = Position::new(editor.area().x + 3, editor.area().y);
        interaction.pointer_down(&mut editor, point, false);
        interaction.pointer_up(&mut editor);
        interaction.pointer_down(&mut editor, point, false);
        assert_eq!(editor.selection_range(), Some(((0, 0), (0, 5))));
    }

    #[test]
    fn menu_does_not_open_inside_a_code_fence() {
        let mut editor = editor(&["```", "", "```"]);
        jump(editor.text_area_mut(), 1, 0);
        let mut interaction = MarkdownEditorInteraction::new();
        interaction.handle_input(
            &mut editor,
            Input {
                key: Key::Char('/'),
                ..Input::default()
            },
        );
        assert!(!interaction.is_insert_menu_open());
        assert_eq!(editor.lines()[1], "/");
    }
}
