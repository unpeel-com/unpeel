#[cfg(feature = "ui-bridge")]
use std::fmt;
use std::ops::{Deref, DerefMut};

use crossterm::event::MouseEvent;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
#[cfg(feature = "ui-bridge")]
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tui_textarea::{CursorMove, CursorRenderMode, Input, Key, TextArea, WrapMode};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[cfg(feature = "ui-bridge")]
use crate::{
    ActionId, FooterAction, FooterActions, MarkdownEditorActions, MarkdownEditorSpec,
    MarkdownMenuTrigger, MarkdownPresentation, NodeId, SemanticMenu, TextEdit, TextPosition,
    TextSelection, UI_PROTOCOL_MAX_VERSION, UI_PROTOCOL_MIN_VERSION, UI_PROTOCOL_NAME, UiEvent,
    UiEventKind, UiEventValue, UiNode,
};
use crate::{MarkdownCommandHint, TerminalPointerPhase, TerminalPointerState, VerticalScrollbar};

const DEFAULT_LEFT_PADDING: u16 = 1;
const DEFAULT_TAB_LENGTH: u8 = 2;
const DEFAULT_MAX_HISTORIES: usize = 500;
const DEFAULT_PLACEHOLDER: &str = "Type '/' for commands";
const DEFAULT_DROP_EDGE_ROWS: u16 = 2;

/// Colors used by [`MarkdownTextArea`] for its editor chrome and cursor.
///
/// Text syntax colors remain owned by the caller, which can paint the wrapped
/// [`TextArea`] through [`MarkdownTextArea::text_area_mut`] before rendering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkdownTextAreaStyle {
    /// Style for the spec-owned terminal title/status row.
    pub status: Style,
    /// Background style for the data row containing the cursor.
    pub cursor_line: Style,
    /// Style retained for the cursor cell, even though the component uses the
    /// terminal-native cursor rather than a painted buffer cell.
    pub cursor: Style,
    /// Built-in selection style. Markdown Apps can leave this empty and paint
    /// selections as part of syntax highlighting.
    pub selection: Style,
    /// Pointer hover on footer actions and the back chevron; distinct from
    /// `selection`.
    pub hovered: Style,
    /// Style for ordinary line numbers.
    pub gutter: Style,
    /// Style for the line number containing the cursor.
    pub current_gutter: Style,
    /// Style for unused scrollbar track cells.
    pub scrollbar_track: Style,
    /// Style for proportional scrollbar thumb cells.
    pub scrollbar_thumb: Style,
}

/// Cross-renderer component-library name for [`MarkdownTextAreaStyle`].
pub type MarkdownEditorStyle = MarkdownTextAreaStyle;

/// Cross-renderer presentation metadata paired with a [`MarkdownTextArea`].
///
/// The text, cursor, selection, and placeholder continue to come directly
/// from `tui-textarea`. This configuration contains only cross-renderer state
/// that belongs to the surrounding App.
#[cfg(feature = "ui-bridge")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkdownEditorConfig {
    node_id: NodeId,
    presentation: MarkdownPresentation,
    read_only: bool,
    dirty: bool,
    title: Option<String>,
    back: Option<String>,
    command_hint: Option<MarkdownCommandHint>,
    insert_menu: Option<SemanticMenu>,
    context_menu: Option<SemanticMenu>,
    actions: MarkdownEditorActions,
    footer: FooterActions,
}

#[cfg(feature = "ui-bridge")]
impl MarkdownEditorConfig {
    /// Creates editable Markdown component metadata with the standard actions.
    #[must_use]
    pub fn new(node_id: impl Into<NodeId>) -> Self {
        Self {
            node_id: node_id.into(),
            presentation: MarkdownPresentation::Source,
            read_only: false,
            dirty: false,
            title: None,
            back: None,
            command_hint: None,
            insert_menu: None,
            context_menu: None,
            actions: MarkdownEditorActions::editable(),
            footer: FooterActions::default(),
        }
    }

    #[must_use]
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    #[must_use]
    pub const fn presentation(mut self, presentation: MarkdownPresentation) -> Self {
        self.presentation = presentation;
        self
    }

    #[must_use]
    pub const fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Adds App-owned empty-line command discovery to every renderer.
    #[must_use]
    pub fn command_hint(mut self, command_hint: MarkdownCommandHint) -> Self {
        self.command_hint = Some(command_hint);
        self
    }

    #[must_use]
    pub fn insert_menu(mut self, insert_menu: Option<SemanticMenu>) -> Self {
        self.insert_menu = insert_menu;
        self
    }

    #[must_use]
    pub fn context_menu(mut self, context_menu: SemanticMenu) -> Self {
        self.context_menu = Some(context_menu);
        self
    }

    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self.actions = if read_only {
            MarkdownEditorActions::read_only()
        } else {
            MarkdownEditorActions::editable()
        };
        self
    }

    /// Replaces standard action identifiers for an App-specific reducer.
    #[must_use]
    pub fn actions(mut self, actions: MarkdownEditorActions) -> Self {
        self.actions = actions;
        self
    }

    #[must_use]
    pub fn footer_actions(mut self, actions: impl IntoIterator<Item = FooterAction>) -> Self {
        self.footer = FooterActions::new(actions);
        self
    }

    /// Declares the App-owned action behind the title's back chevron.
    #[must_use]
    pub fn back_action(mut self, action: impl Into<String>) -> Self {
        self.back = Some(action.into());
        self
    }

    #[must_use]
    pub fn back(&self) -> Option<&str> {
        self.back.as_deref()
    }

    /// Enables an App-owned slash/palette Menu entry point. It is opt-in so a
    /// plain Markdown editor does not advertise behavior its reducer lacks.
    #[must_use]
    pub fn open_menu_action(mut self, action: impl Into<ActionId>) -> Self {
        self.actions.open_menu = Some(action.into());
        self
    }
}

/// Result of applying one native/web Markdown action to the Ratatui component.
#[cfg(feature = "ui-bridge")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownEditorEvent {
    TextChanged {
        changed: bool,
    },
    SelectionChanged,
    Undo {
        changed: bool,
    },
    Redo {
        changed: bool,
    },
    SaveRequested,
    /// The renderer activated the title's back chevron; the App owns what
    /// "back" means (usually closing the document).
    BackRequested,
    PresentationRequested(MarkdownPresentation),
    MenuRequested(MarkdownMenuTrigger),
}

/// A matching Markdown action that cannot safely be applied.
#[cfg(feature = "ui-bridge")]
#[derive(Debug, PartialEq, Eq)]
pub enum MarkdownEditorEventError {
    UnexpectedProtocol { protocol: String, version: u32 },
    StaleRevision { expected: u64, received: u64 },
    ReadOnly,
    UnsupportedAction(String),
    InvalidEvent(String),
    InvalidPosition(TextPosition),
    PositionTooLarge(TextPosition),
}

#[cfg(feature = "ui-bridge")]
impl fmt::Display for MarkdownEditorEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedProtocol { protocol, version } => write!(
                formatter,
                "unexpected UI protocol {protocol}/{version}; expected {UI_PROTOCOL_NAME}/{UI_PROTOCOL_MIN_VERSION}..={UI_PROTOCOL_MAX_VERSION}"
            ),
            Self::StaleRevision { expected, received } => write!(
                formatter,
                "stale Markdown event revision {received}; current revision is {expected}"
            ),
            Self::ReadOnly => formatter.write_str("Markdown editor is read-only"),
            Self::UnsupportedAction(action) => {
                write!(formatter, "unsupported Markdown action {action:?}")
            }
            Self::InvalidEvent(message) => write!(formatter, "invalid Markdown event: {message}"),
            Self::InvalidPosition(position) => write!(
                formatter,
                "invalid Markdown position {}:{} (UTF-16)",
                position.line, position.utf16_column
            ),
            Self::PositionTooLarge(position) => write!(
                formatter,
                "Markdown position {}:{} exceeds the Ratatui editor cursor range",
                position.line, position.utf16_column
            ),
        }
    }
}

#[cfg(feature = "ui-bridge")]
impl std::error::Error for MarkdownEditorEventError {}

/// A reusable Markdown editor built on `tui-textarea-2`.
///
/// The component owns the visual concerns that otherwise tend to drift
/// between Apps: soft wrapping, a continuation-aware line-number gutter,
/// overflow reservation, proportional scrolling, native cursor placement,
/// wrapped mouse hit-testing, and drag auto-scroll. Editing commands and
/// Markdown syntax highlighting stay with the consuming App.
pub struct MarkdownTextArea<'a> {
    text_area: TextArea<'a>,
    style: MarkdownTextAreaStyle,
    area: Rect,
    scroll_top: (u16, u16),
    left_padding: u16,
    pointer: TerminalPointerState,
}

/// Opinionated App Kit Markdown component.
///
/// The current terminal implementation is [`MarkdownTextArea`], so existing
/// Apps retain their API while new component-oriented code can use this name.
/// Its `render` method is always Ratatui-backed. With the `ui-bridge` feature,
/// the same editor can also project state to Swift and web wrappers.
pub type MarkdownEditor<'a> = MarkdownTextArea<'a>;

/// Terminal rectangles produced by the complete MarkdownEditor interpreter.
#[cfg(feature = "ui-bridge")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkdownEditorTerminalLayout {
    pub body: Rect,
    /// Title row at the top (with the back chevron when `back` is set).
    pub status: Rect,
    pub footer: Rect,
}

impl MarkdownEditorTerminalLayout {
    /// Whether a click at `position` lands on the title row (the padding row
    /// beneath it does not count).
    #[must_use]
    pub fn back_hit(&self, position: ratatui::layout::Position) -> bool {
        !self.status.is_empty()
            && Rect {
                height: 1,
                ..self.status
            }
            .contains(position)
    }
}

impl<'a> MarkdownTextArea<'a> {
    /// Creates a Markdown editor from an iterator of logical lines.
    pub fn new<I, S>(lines: I, style: MarkdownTextAreaStyle) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::from_text_area(TextArea::from(lines), style)
    }

    /// Wraps an existing `tui-textarea-2` instance and applies the component's
    /// Markdown editor defaults.
    #[must_use]
    pub fn from_text_area(mut text_area: TextArea<'a>, style: MarkdownTextAreaStyle) -> Self {
        configure_text_area(&mut text_area, style);
        Self {
            text_area,
            style,
            area: Rect::default(),
            scroll_top: (0, 0),
            left_padding: DEFAULT_LEFT_PADDING,
            pointer: TerminalPointerState::default(),
        }
    }

    /// Changes the left-side breathing room reserved outside the gutter.
    #[must_use]
    pub const fn left_padding(mut self, columns: u16) -> Self {
        self.left_padding = columns;
        self
    }

    /// Replaces component colors without changing text, history, or scroll.
    pub fn set_component_style(&mut self, style: MarkdownTextAreaStyle) {
        self.style = style;
        apply_text_area_style(&mut self.text_area, style);
    }

    /// Returns the underlying editor for syntax painting or advanced setup.
    #[must_use]
    pub const fn text_area(&self) -> &TextArea<'a> {
        &self.text_area
    }

    /// Returns the underlying editor for syntax painting or advanced setup.
    #[must_use]
    pub fn text_area_mut(&mut self) -> &mut TextArea<'a> {
        &mut self.text_area
    }

    /// Replaces all logical lines and resets the cursor and scroll position.
    ///
    /// This mirrors [`TextArea::set_lines`] while also resetting the external
    /// visual-row state owned by this component.
    pub fn set_lines(&mut self, lines: Vec<String>, cursor: (usize, usize)) {
        self.text_area.set_lines(lines, cursor);
        self.scroll_top = (0, 0);
    }

    /// Builds the owned component rendered by Swift or web clients.
    ///
    /// This is a second rendering path over the same `tui-textarea` state.
    /// Calling it never changes terminal behavior or requires an Unpeel Host.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_node(&self, config: &MarkdownEditorConfig) -> UiNode {
        let mut editor = MarkdownEditorSpec::new(
            markdown_document(self.text_area.lines()),
            semantic_selection(&self.text_area),
        )
        .presentation(config.presentation)
        .dirty(config.dirty)
        .read_only(config.read_only)
        .placeholder(self.text_area.placeholder_text());
        editor.command_hint.clone_from(&config.command_hint);
        editor.insert_menu.clone_from(&config.insert_menu);
        editor.context_menu.clone_from(&config.context_menu);
        editor.actions = config.actions.clone();
        editor.title.clone_from(&config.title);
        editor.back.clone_from(&config.back);
        editor.footer.clone_from(&config.footer);
        UiNode::markdown_editor(config.node_id.clone(), editor)
    }

    /// Applies one matching native/web action to the same state Ratatui uses.
    ///
    /// `Ok(None)` means the event targets another component. Commands whose
    /// state belongs to the App, such as save and presentation changes, are
    /// returned for the App reducer rather than applied inside the text area.
    #[cfg(feature = "ui-bridge")]
    pub fn handle_ui_event(
        &mut self,
        revision: u64,
        config: &MarkdownEditorConfig,
        event: &UiEvent,
    ) -> Result<Option<MarkdownEditorEvent>, MarkdownEditorEventError> {
        if event.action.node_id != config.node_id {
            return Ok(None);
        }
        if event.protocol != UI_PROTOCOL_NAME
            || !(UI_PROTOCOL_MIN_VERSION..=UI_PROTOCOL_MAX_VERSION)
                .contains(&event.protocol_version)
        {
            return Err(MarkdownEditorEventError::UnexpectedProtocol {
                protocol: event.protocol.clone(),
                version: event.protocol_version,
            });
        }
        if event.base_revision != revision {
            return Err(MarkdownEditorEventError::StaleRevision {
                expected: revision,
                received: event.base_revision,
            });
        }

        let action = event.action.action.as_str();
        if config.back.as_deref() == Some(action) {
            require_kind(event, UiEventKind::Cancel)?;
            return Ok(Some(MarkdownEditorEvent::BackRequested));
        }
        if action_matches(config.actions.replace_range.as_ref(), action) {
            if config.read_only {
                return Err(MarkdownEditorEventError::ReadOnly);
            }
            require_kind(event, UiEventKind::Change)?;
            let UiEventValue::TextEdit(edit) = &event.action.value else {
                return Err(MarkdownEditorEventError::InvalidEvent(
                    "replace-range requires a textEdit value".to_owned(),
                ));
            };
            let changed = apply_text_edit(&mut self.text_area, edit)?;
            self.scroll_top = (0, 0);
            return Ok(Some(MarkdownEditorEvent::TextChanged { changed }));
        }
        if action_matches(config.actions.set_selection.as_ref(), action) {
            require_kind(event, UiEventKind::Select)?;
            let UiEventValue::TextSelection(selection) = &event.action.value else {
                return Err(MarkdownEditorEventError::InvalidEvent(
                    "set-selection requires a textSelection value".to_owned(),
                ));
            };
            apply_text_selection(&mut self.text_area, *selection)?;
            return Ok(Some(MarkdownEditorEvent::SelectionChanged));
        }
        if action_matches(config.actions.undo.as_ref(), action) {
            if config.read_only {
                return Err(MarkdownEditorEventError::ReadOnly);
            }
            require_command(event)?;
            return Ok(Some(MarkdownEditorEvent::Undo {
                changed: self.text_area.undo(),
            }));
        }
        if action_matches(config.actions.redo.as_ref(), action) {
            if config.read_only {
                return Err(MarkdownEditorEventError::ReadOnly);
            }
            require_command(event)?;
            return Ok(Some(MarkdownEditorEvent::Redo {
                changed: self.text_area.redo(),
            }));
        }
        if action_matches(config.actions.save.as_ref(), action) {
            require_command(event)?;
            return Ok(Some(MarkdownEditorEvent::SaveRequested));
        }
        if action_matches(config.actions.set_presentation.as_ref(), action) {
            require_kind(event, UiEventKind::Change)?;
            let UiEventValue::Text(presentation) = &event.action.value else {
                return Err(MarkdownEditorEventError::InvalidEvent(
                    "set-presentation requires a text value".to_owned(),
                ));
            };
            let presentation = presentation.parse().map_err(|_| {
                MarkdownEditorEventError::InvalidEvent(format!(
                    "unknown Markdown presentation {presentation:?}"
                ))
            })?;
            return Ok(Some(MarkdownEditorEvent::PresentationRequested(
                presentation,
            )));
        }
        if action_matches(config.actions.open_menu.as_ref(), action) {
            if config.read_only {
                return Err(MarkdownEditorEventError::ReadOnly);
            }
            require_kind(event, UiEventKind::Command)?;
            let UiEventValue::Text(trigger) = &event.action.value else {
                return Err(MarkdownEditorEventError::InvalidEvent(
                    "open-menu requires a text value".to_owned(),
                ));
            };
            let trigger = trigger.parse().map_err(|_| {
                MarkdownEditorEventError::InvalidEvent(format!(
                    "unknown Markdown menu trigger {trigger:?}"
                ))
            })?;
            return Ok(Some(MarkdownEditorEvent::MenuRequested(trigger)));
        }

        Err(MarkdownEditorEventError::UnsupportedAction(
            action.to_owned(),
        ))
    }

    /// The most recently rendered editor body, including the gutter but not
    /// the outside padding or overflow scrollbar.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    /// Whether a terminal position is inside the most recently rendered body.
    #[must_use]
    pub const fn contains(&self, position: Position) -> bool {
        self.area.contains(position)
    }

    #[must_use]
    pub const fn pointer(&self) -> TerminalPointerState {
        self.pointer
    }

    /// Feeds hover/press ephemera used by the component-owned footer and
    /// leaves document selection mechanics to the existing editor handlers.
    pub fn track_mouse(&mut self, event: &MouseEvent) -> bool {
        self.pointer.track(event)
    }

    /// The current visual-row and horizontal scroll offsets.
    #[must_use]
    pub const fn scroll_top(&self) -> (u16, u16) {
        self.scroll_top
    }

    /// The greatest valid visual-row offset for the current render area.
    #[must_use]
    pub fn max_scroll(&self) -> u16 {
        if self.area.height == 0 {
            return 0;
        }
        clamp_u16(
            self.layout_rows()
                .len()
                .saturating_sub(usize::from(self.area.height)),
        )
    }

    /// Scrolls by visual rows and clamps at the first and last complete page.
    /// Returns whether the scroll position changed.
    pub fn scroll_lines(&mut self, rows: i16) -> bool {
        let (current, target) = self.scroll_target(rows);
        if target == current {
            return false;
        }

        scroll_text_area(&mut self.text_area, i32::from(target) - i32::from(current));
        self.scroll_top.0 = target;
        true
    }

    /// Scrolls by visual rows using `tui-textarea-2`'s mouse-selection rules.
    ///
    /// This is the wheel-event counterpart to [`Self::scroll_lines`]. Setting
    /// `extend_selection` starts or extends a selection like Shift+wheel;
    /// leaving it false clears an existing selection as the underlying editor
    /// normally does for an unmodified wheel event.
    pub fn scroll_lines_with_selection(&mut self, rows: i16, extend_selection: bool) -> bool {
        let (current, target) = self.scroll_target(rows);
        if target == current {
            return false;
        }

        let step = if target > current { 1i32 } else { -1i32 };
        let key = if step > 0 {
            Key::MouseScrollDown
        } else {
            Key::MouseScrollUp
        };
        for _ in 0..i32::from(target).abs_diff(i32::from(current)) {
            self.text_area.input(Input {
                key,
                ctrl: false,
                alt: false,
                shift: extend_selection,
            });
        }
        self.scroll_top.0 = target;
        true
    }

    /// Scrolls one visual row when a selection drag moves above or below the
    /// editor body. Returns whether the scroll position changed.
    pub fn auto_scroll(&mut self, position: Position) -> bool {
        if position.y < self.area.y {
            self.scroll_lines(-1)
        } else if position.y >= self.area.bottom() {
            self.scroll_lines(1)
        } else {
            false
        }
    }

    /// Moves the insertion cursor beneath a native file/folder drag.
    ///
    /// Hovering within two rows of the top or bottom edge scrolls one visual
    /// row per update. This is intentionally separate from text-selection
    /// dragging: a semantic drop cancels any selection and previews the exact
    /// insertion point that will receive the dropped reference.
    pub fn position_drop_cursor(&mut self, position: Position) -> bool {
        let previous_cursor = self.text_area.cursor();
        let previous_scroll = self.scroll_top;
        let was_selecting = self.text_area.is_selecting();
        let edge_rows = DEFAULT_DROP_EDGE_ROWS.min(self.area.height.saturating_div(2));
        if edge_rows > 0 {
            if position.y < self.area.y.saturating_add(edge_rows) {
                self.scroll_lines(-1);
            } else if position.y >= self.area.bottom().saturating_sub(edge_rows) {
                self.scroll_lines(1);
            }
        }
        let (row, column) = self.hit_test(position);
        self.text_area.cancel_selection();
        self.text_area
            .move_cursor(CursorMove::Jump(clamp_u16(row), clamp_u16(column)));
        was_selecting
            || previous_cursor != self.text_area.cursor()
            || previous_scroll != self.scroll_top
    }

    /// Maps terminal coordinates to a logical `(line, character-column)`.
    ///
    /// Positions above or below the body deliberately clamp through the
    /// visible visual rows, which lets selection drags extend beyond an edge.
    #[must_use]
    pub fn hit_test(&self, position: Position) -> (usize, usize) {
        let lines = self.text_area.lines();
        let gutter = gutter_width(lines.len());
        let width = wrap_width(self.area.width, gutter);
        hit_test(
            HitContext {
                lines,
                inner: self.area,
                scroll_top: self.scroll_top,
                gutter,
                width,
                tab_len: self.text_area.tab_length(),
            },
            position.x,
            position.y,
        )
    }

    /// Renders the editor, line-number gutter, and overflow scrollbar.
    ///
    /// `show_cursor` controls only placement of the terminal-native cursor;
    /// the editor's hidden cell cursor remains disabled in all modes.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.render_internal(frame, area, show_cursor, None, false);
    }

    /// Renders the standalone editor with the same App-owned command-hint
    /// contract used by hosted Swift and web interpreters.
    pub fn render_with_command_hint(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        show_cursor: bool,
        command_hint: Option<&MarkdownCommandHint>,
        insert_menu_open: bool,
    ) {
        self.render_internal(frame, area, show_cursor, command_hint, insert_menu_open);
    }

    /// Renders directly from the same metadata used to build the semantic
    /// projection. This keeps the terminal hint and nested Menu state on one
    /// Rust-owned configuration path.
    #[cfg(feature = "ui-bridge")]
    pub fn render_with_config(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        show_cursor: bool,
        config: &MarkdownEditorConfig,
    ) {
        let footer_height = u16::from(!config.footer.is_empty() && area.height > 0);
        let body = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(footer_height),
        );
        self.render_internal(
            frame,
            body,
            show_cursor,
            (config.presentation != MarkdownPresentation::Preview)
                .then_some(config.command_hint.as_ref())
                .flatten(),
            config.insert_menu.is_some(),
        );
        if footer_height > 0 {
            let footer = Rect::new(
                area.x,
                area.bottom().saturating_sub(footer_height),
                area.width,
                footer_height,
            );
            frame.render_widget(
                config
                    .footer
                    .widget()
                    .styles(
                        self.style.status,
                        self.style.status.add_modifier(Modifier::BOLD),
                        self.style.status,
                        Style::new().fg(Color::Red),
                        self.style.status.add_modifier(Modifier::DIM),
                    )
                    .pointer(self.pointer)
                    .interaction_styles(
                        self.style.hovered,
                        self.style.selection.add_modifier(Modifier::BOLD),
                    ),
                footer,
            );
        }
    }

    /// Renders the complete terminal component from the exact published spec.
    ///
    /// The document editor keeps renderer-local scroll/cursor mechanics, but
    /// title, hint visibility, and semantic Menu presence come only from the
    /// Rust `MarkdownEditorSpec` consumed by every renderer.
    #[cfg(feature = "ui-bridge")]
    pub fn render_component(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        show_cursor: bool,
        spec: &MarkdownEditorSpec,
    ) -> MarkdownEditorTerminalLayout {
        let footer_height = u16::from(!spec.footer.is_empty() && area.height > 0);
        // Title on top with one padding row beneath it, actions at the
        // bottom: the shared screen shape.
        let available = area.height.saturating_sub(footer_height);
        let status_height = if spec.title.is_none() || available <= 1 {
            0
        } else if available > 2 {
            2
        } else {
            1
        };
        let status = Rect::new(area.x, area.y, area.width, status_height);
        let body = Rect::new(
            area.x,
            area.y.saturating_add(status_height),
            area.width,
            area.height
                .saturating_sub(status_height.saturating_add(footer_height)),
        );
        let footer = Rect::new(
            area.x,
            area.bottom().saturating_sub(footer_height),
            area.width,
            footer_height,
        );
        self.render_internal(
            frame,
            body,
            show_cursor,
            (spec.presentation != MarkdownPresentation::Preview)
                .then_some(spec.command_hint.as_ref())
                .flatten(),
            spec.insert_menu.is_some(),
        );
        if let Some(title) = &spec.title {
            let chevron = if spec.back.is_some() { "‹  " } else { "" };
            frame.render_widget(
                Paragraph::new(format!("  {chevron}{title}")).style(self.style.status),
                status,
            );
            if spec.back.is_some() {
                // Only the chevron takes the hover/press fill, like Page.
                let fill = match self.pointer.phase(status) {
                    TerminalPointerPhase::Idle => None,
                    TerminalPointerPhase::Hovered => Some(self.style.hovered),
                    TerminalPointerPhase::Pressed => {
                        Some(self.style.selection.add_modifier(Modifier::BOLD))
                    }
                };
                if let Some(fill) = fill {
                    let chevron_area =
                        Rect::new(status.x.saturating_add(1), status.y, 3.min(status.width), 1);
                    frame.buffer_mut().set_style(chevron_area, fill);
                }
            }
        }
        if footer_height > 0 {
            frame.render_widget(
                spec.footer
                    .widget()
                    .styles(
                        self.style.status,
                        self.style.status.add_modifier(Modifier::BOLD),
                        self.style.status,
                        Style::new().fg(Color::Red),
                        self.style.status.add_modifier(Modifier::DIM),
                    )
                    .pointer(self.pointer)
                    .interaction_styles(
                        self.style.hovered,
                        self.style.selection.add_modifier(Modifier::BOLD),
                    ),
                footer,
            );
        }
        MarkdownEditorTerminalLayout {
            body,
            status,
            footer,
        }
    }

    fn render_internal(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        show_cursor: bool,
        command_hint: Option<&MarkdownCommandHint>,
        insert_menu_open: bool,
    ) {
        let left_padding = self.left_padding.min(area.width);
        let content = Rect {
            x: area.x.saturating_add(left_padding),
            width: area.width.saturating_sub(left_padding),
            ..area
        };
        let gutter = gutter_width(self.text_area.lines().len());
        let viewport = usize::from(content.height);
        let preview_rows = visual_rows(
            self.text_area.lines(),
            wrap_width(content.width, gutter),
            self.text_area.tab_length(),
        );
        let overflow = preview_rows.len() > viewport && viewport > 0;
        self.area = if overflow {
            Rect {
                width: content.width.saturating_sub(1),
                ..content
            }
        } else {
            content
        };
        self.clamp_scroll();

        let gutter_area = Rect {
            x: self.area.x,
            y: self.area.y,
            width: gutter.min(self.area.width),
            height: self.area.height,
        };
        let text_area = Rect {
            x: self.area.x.saturating_add(gutter_area.width),
            y: self.area.y,
            width: self.area.width.saturating_sub(gutter_area.width),
            height: self.area.height,
        };

        frame.render_widget(&self.text_area, text_area);
        self.sync_scroll();
        self.render_gutter(frame, gutter_area);
        self.render_command_hint(frame, command_hint, insert_menu_open);
        if overflow {
            let scrollbar_area = Rect {
                x: area.right().saturating_sub(1),
                width: area.width.min(1),
                ..area
            };
            let rows = self.layout_rows();
            frame.render_widget(
                VerticalScrollbar::new(rows.len(), viewport, usize::from(self.scroll_top.0))
                    .track_style(self.style.scrollbar_track)
                    .thumb_style(self.style.scrollbar_thumb),
                scrollbar_area,
            );
        }

        if show_cursor && let Some(position) = self.text_area.rendered_cursor_position() {
            frame.set_cursor_position(position);
        }
    }

    fn render_command_hint(
        &self,
        frame: &mut Frame,
        command_hint: Option<&MarkdownCommandHint>,
        insert_menu_open: bool,
    ) {
        let Some(hint) = command_hint else { return };
        let document = markdown_document(self.text_area.lines());
        if !hint.is_visible(
            &document,
            self.text_area.cursor().0,
            !self.text_area.is_selecting(),
            insert_menu_open,
            self.text_area.placeholder_text(),
        ) {
            return;
        }
        let Some(position) = self.text_area.rendered_cursor_position() else {
            return;
        };
        let width = u16::try_from(hint.text.width())
            .unwrap_or(u16::MAX)
            .min(self.area.right().saturating_sub(position.x));
        if width < 4 {
            return;
        }
        frame.render_widget(
            Paragraph::new(hint.text.as_str()).style(self.style.gutter),
            Rect::new(position.x, position.y, width, 1),
        );
    }

    fn layout_rows(&self) -> Vec<VisualRow> {
        let gutter = gutter_width(self.text_area.lines().len());
        visual_rows(
            self.text_area.lines(),
            wrap_width(self.area.width, gutter),
            self.text_area.tab_length(),
        )
    }

    fn scroll_target(&self, rows: i16) -> (u16, u16) {
        let current = self.scroll_top.0;
        let target = if rows >= 0 {
            current.saturating_add(rows as u16)
        } else {
            current.saturating_sub(rows.unsigned_abs())
        }
        .min(self.max_scroll());
        (current, target)
    }

    fn render_gutter(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let rows = self.layout_rows();
        let top = usize::from(self.scroll_top.0);
        let digits = usize::from(line_number_digits(self.text_area.lines().len()));
        let current = self.text_area.cursor().0;
        let numbered: Vec<Line> = (0..usize::from(area.height))
            .map(|index| {
                let Some(row) = rows.get(top + index) else {
                    return Line::from("");
                };
                if row.start_col != 0 {
                    return Line::from("");
                }
                let label = format!("{:>digits$}  ", row.line + 1);
                let style = if row.line == current {
                    self.style.current_gutter
                } else {
                    self.style.gutter
                };
                Line::from(Span::styled(label, style))
            })
            .collect();
        frame.render_widget(Paragraph::new(numbered), area);
    }

    fn sync_scroll(&mut self) {
        let Some(rendered) = self.text_area.rendered_cursor_position() else {
            self.clamp_scroll();
            return;
        };
        let gutter = gutter_width(self.text_area.lines().len());
        let width = wrap_width(self.area.width, gutter);
        if let Some(scroll) = infer_scroll(
            self.text_area.lines(),
            self.area,
            width,
            self.text_area.tab_length(),
            self.text_area.cursor(),
            rendered,
        ) {
            self.scroll_top = scroll;
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        if self.scroll_top.0 <= max {
            return;
        }
        let extra = self.scroll_top.0 - max;
        scroll_text_area(&mut self.text_area, -i32::from(extra));
        self.scroll_top.0 = max;
    }
}

impl<'a> Deref for MarkdownTextArea<'a> {
    type Target = TextArea<'a>;

    fn deref(&self) -> &Self::Target {
        &self.text_area
    }
}

impl DerefMut for MarkdownTextArea<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.text_area
    }
}

fn markdown_document(lines: &[String]) -> String {
    lines.join("\n")
}

#[cfg(feature = "ui-bridge")]
fn semantic_selection(text_area: &TextArea<'_>) -> TextSelection {
    let head = cursor_to_text_position(text_area.lines(), text_area.cursor());
    let Some((start, end)) = text_area.selection_range() else {
        return TextSelection::caret(head);
    };
    let start = cursor_to_text_position(text_area.lines(), start);
    let end = cursor_to_text_position(text_area.lines(), end);
    let anchor = if head == start { end } else { start };
    TextSelection { anchor, head }
}

#[cfg(feature = "ui-bridge")]
fn cursor_to_text_position(lines: &[String], cursor: (usize, usize)) -> TextPosition {
    let (line_index, character_column) = cursor;
    let line = lines.get(line_index).map(String::as_str).unwrap_or("");
    let utf16_column = line
        .chars()
        .take(character_column)
        .map(char::len_utf16)
        .sum::<usize>();
    TextPosition::new(
        u32::try_from(line_index).unwrap_or(u32::MAX),
        u32::try_from(utf16_column).unwrap_or(u32::MAX),
    )
}

#[cfg(feature = "ui-bridge")]
fn text_position_to_cursor(
    lines: &[String],
    position: TextPosition,
) -> Result<(u16, u16), MarkdownEditorEventError> {
    let line_index = usize::try_from(position.line)
        .map_err(|_| MarkdownEditorEventError::PositionTooLarge(position))?;
    let Some(line) = lines.get(line_index) else {
        return Err(MarkdownEditorEventError::InvalidPosition(position));
    };
    let target = usize::try_from(position.utf16_column)
        .map_err(|_| MarkdownEditorEventError::PositionTooLarge(position))?;
    let mut utf16_column = 0usize;
    let mut character_column = 0usize;
    if target != 0 {
        for character in line.chars() {
            utf16_column += character.len_utf16();
            character_column += 1;
            if utf16_column == target {
                break;
            }
            if utf16_column > target {
                return Err(MarkdownEditorEventError::InvalidPosition(position));
            }
        }
        if utf16_column != target {
            return Err(MarkdownEditorEventError::InvalidPosition(position));
        }
    }

    Ok((
        u16::try_from(line_index)
            .map_err(|_| MarkdownEditorEventError::PositionTooLarge(position))?,
        u16::try_from(character_column)
            .map_err(|_| MarkdownEditorEventError::PositionTooLarge(position))?,
    ))
}

#[cfg(feature = "ui-bridge")]
fn apply_text_edit(
    text_area: &mut TextArea<'_>,
    edit: &TextEdit,
) -> Result<bool, MarkdownEditorEventError> {
    if edit.range.start > edit.range.end {
        return Err(MarkdownEditorEventError::InvalidEvent(
            "replace-range range is reversed".to_owned(),
        ));
    }
    let start = text_position_to_cursor(text_area.lines(), edit.range.start)?;
    let end = text_position_to_cursor(text_area.lines(), edit.range.end)?;
    text_area.cancel_selection();
    text_area.move_cursor(CursorMove::Jump(start.0, start.1));
    if start != end {
        text_area.start_selection();
        text_area.move_cursor(CursorMove::Jump(end.0, end.1));
    }
    Ok(text_area.insert_str(&edit.text))
}

#[cfg(feature = "ui-bridge")]
fn apply_text_selection(
    text_area: &mut TextArea<'_>,
    selection: TextSelection,
) -> Result<(), MarkdownEditorEventError> {
    let anchor = text_position_to_cursor(text_area.lines(), selection.anchor)?;
    let head = text_position_to_cursor(text_area.lines(), selection.head)?;
    text_area.cancel_selection();
    text_area.move_cursor(CursorMove::Jump(anchor.0, anchor.1));
    if anchor != head {
        text_area.start_selection();
    }
    text_area.move_cursor(CursorMove::Jump(head.0, head.1));
    Ok(())
}

#[cfg(feature = "ui-bridge")]
fn action_matches(configured: Option<&ActionId>, received: &str) -> bool {
    configured.is_some_and(|action| action.as_str() == received)
}

#[cfg(feature = "ui-bridge")]
fn require_kind(event: &UiEvent, expected: UiEventKind) -> Result<(), MarkdownEditorEventError> {
    if event.action.kind == expected {
        Ok(())
    } else {
        Err(MarkdownEditorEventError::InvalidEvent(format!(
            "action {} requires {expected:?}, received {:?}",
            event.action.action, event.action.kind
        )))
    }
}

#[cfg(feature = "ui-bridge")]
fn require_command(event: &UiEvent) -> Result<(), MarkdownEditorEventError> {
    require_kind(event, UiEventKind::Command)?;
    if event.action.value == UiEventValue::None {
        Ok(())
    } else {
        Err(MarkdownEditorEventError::InvalidEvent(format!(
            "command {} must not carry a value",
            event.action.action
        )))
    }
}

fn configure_text_area(text_area: &mut TextArea<'_>, style: MarkdownTextAreaStyle) {
    text_area.set_cursor_render_mode(CursorRenderMode::Hidden);
    text_area.set_wrap_mode(WrapMode::WordOrGlyph);
    text_area.set_tab_length(DEFAULT_TAB_LENGTH);
    text_area.remove_line_number();
    apply_text_area_style(text_area, style);
    text_area.set_placeholder_text(DEFAULT_PLACEHOLDER);
    text_area.set_max_histories(DEFAULT_MAX_HISTORIES);
}

fn apply_text_area_style(text_area: &mut TextArea<'_>, style: MarkdownTextAreaStyle) {
    text_area.set_cursor_line_style(style.cursor_line);
    text_area.set_cursor_style(style.cursor);
    text_area.set_selection_style(style.selection);
}

fn scroll_text_area(text_area: &mut TextArea<'_>, mut rows: i32) {
    while rows != 0 {
        let step = rows.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        text_area.scroll((step, 0));
        rows -= i32::from(step);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VisualRow {
    line: usize,
    start_col: usize,
    end_col: usize,
    last_in_line: bool,
}

const fn line_number_digits(line_count: usize) -> u16 {
    if line_count == 0 {
        1
    } else {
        line_count.ilog10() as u16 + 1
    }
}

const fn gutter_width(line_count: usize) -> u16 {
    line_number_digits(line_count) + 3
}

fn wrap_width(inner_width: u16, gutter: u16) -> usize {
    usize::from(inner_width.saturating_sub(gutter)).max(1)
}

fn visual_rows(lines: &[String], width: usize, tab_len: u8) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        let ranges = wrap_ranges(line, width, tab_len);
        let last = ranges.len().saturating_sub(1);
        for (index, (start_col, end_col)) in ranges.into_iter().enumerate() {
            rows.push(VisualRow {
                line: line_idx,
                start_col,
                end_col,
                last_in_line: index == last,
            });
        }
    }
    if rows.is_empty() {
        rows.push(VisualRow {
            line: 0,
            start_col: 0,
            end_col: 0,
            last_in_line: true,
        });
    }
    rows
}

#[derive(Clone, Copy)]
struct HitContext<'a> {
    lines: &'a [String],
    inner: Rect,
    scroll_top: (u16, u16),
    gutter: u16,
    width: usize,
    tab_len: u8,
}

fn hit_test(context: HitContext<'_>, column: u16, row: u16) -> (usize, usize) {
    let rows = visual_rows(context.lines, context.width, context.tab_len);
    let visual_y =
        (u32::from(context.scroll_top.0) + u32::from(row.saturating_sub(context.inner.y))) as usize;
    let visual_row = rows[visual_y.min(rows.len().saturating_sub(1))];

    if column < context.inner.x.saturating_add(context.gutter) {
        return (visual_row.line, visual_row.start_col);
    }

    let local_x = u32::from(column.saturating_sub(context.inner.x.saturating_add(context.gutter)))
        + u32::from(context.scroll_top.1);
    let fragment = slice_cols(
        context
            .lines
            .get(visual_row.line)
            .map(String::as_str)
            .unwrap_or(""),
        visual_row.start_col,
        visual_row.end_col,
    );
    let mut col = visual_row.start_col + col_at_width(&fragment, local_x as usize, context.tab_len);
    if !visual_row.last_in_line {
        col = col.min(visual_row.end_col.saturating_sub(1));
    } else {
        col = col.min(visual_row.end_col);
    }
    (visual_row.line, col)
}

fn infer_scroll(
    lines: &[String],
    inner: Rect,
    width: usize,
    tab_len: u8,
    cursor: (usize, usize),
    rendered: Position,
) -> Option<(u16, u16)> {
    if !inner.contains(rendered) {
        return None;
    }
    let (visual_row, _) = data_to_visual(lines, width, tab_len, cursor)?;
    let top_row = visual_row.saturating_sub(usize::from(rendered.y.saturating_sub(inner.y)));
    Some((clamp_u16(top_row), 0))
}

fn data_to_visual(
    lines: &[String],
    width: usize,
    tab_len: u8,
    cursor: (usize, usize),
) -> Option<(usize, usize)> {
    let rows = visual_rows(lines, width, tab_len);
    let (line, col) = cursor;
    for (index, row) in rows.iter().enumerate() {
        if row.line != line {
            continue;
        }
        let contains = if row.last_in_line {
            col >= row.start_col && col <= row.end_col
        } else {
            col >= row.start_col && col < row.end_col
        };
        if contains {
            let fragment = slice_cols(&lines[line], row.start_col, col);
            return Some((index, display_width(&fragment, tab_len)));
        }
    }
    rows.last().map(|row| {
        let fragment = slice_cols(
            lines.get(row.line).map(String::as_str).unwrap_or(""),
            row.start_col,
            row.end_col,
        );
        (rows.len() - 1, display_width(&fragment, tab_len))
    })
}

fn wrap_ranges(line: &str, width: usize, tab_len: u8) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let chunks: Vec<(usize, usize)> = UnicodeSegmentation::split_word_bound_indices(line)
        .map(|(start, word)| (start, start + word.len()))
        .collect();

    let mut byte_ranges = Vec::new();
    if chunks.is_empty() {
        byte_ranges.push((0, 0));
    } else {
        let mut index = 0usize;
        let mut segment_start = chunks[0].0;
        let mut segment_end = segment_start;
        let mut segment_width = 0usize;

        while index < chunks.len() {
            let (start, end) = chunks[index];
            if segment_end == segment_start {
                segment_start = start;
            }
            let chunk = &line[start..end];
            let chunk_width = display_width_from(chunk, segment_width, tab_len);
            if segment_width + chunk_width <= width {
                segment_end = end;
                segment_width += chunk_width;
                index += 1;
                continue;
            }
            if segment_end > segment_start {
                byte_ranges.push((segment_start, segment_end));
                segment_start = segment_end;
                segment_width = 0;
                continue;
            }
            split_bytes_by_grapheme(line, start, end, width, tab_len, &mut byte_ranges);
            index += 1;
            segment_start = end;
            segment_end = end;
            segment_width = 0;
        }
        if segment_end > segment_start {
            byte_ranges.push((segment_start, segment_end));
        }
    }

    if byte_ranges.is_empty() {
        byte_ranges.push((0, 0));
    }

    byte_ranges
        .into_iter()
        .map(|(start, end)| (byte_to_col(line, start), byte_to_col(line, end)))
        .collect()
}

fn split_bytes_by_grapheme(
    line: &str,
    start: usize,
    end: usize,
    width: usize,
    tab_len: u8,
    out: &mut Vec<(usize, usize)>,
) {
    let mut segment_start = start;
    while segment_start < end {
        let mut segment_end = segment_start;
        let mut segment_width = 0usize;
        for (offset, grapheme) in
            UnicodeSegmentation::grapheme_indices(&line[segment_start..end], true)
        {
            let grapheme_start = segment_start + offset;
            let grapheme_end = grapheme_start + grapheme.len();
            let next_width = display_width_to(grapheme, segment_width, tab_len);
            let grapheme_width = next_width.saturating_sub(segment_width);
            if segment_end != segment_start && segment_width + grapheme_width > width {
                break;
            }
            segment_end = grapheme_end;
            segment_width = next_width;
            if segment_width > width {
                break;
            }
        }
        if segment_end == segment_start {
            if let Some(character) = line[segment_start..end].chars().next() {
                segment_end = segment_start + character.len_utf8();
            } else {
                break;
            }
        }
        out.push((segment_start, segment_end));
        segment_start = segment_end;
    }
}

fn slice_cols(line: &str, start_col: usize, end_col: usize) -> String {
    line.chars()
        .skip(start_col)
        .take(end_col.saturating_sub(start_col))
        .collect()
}

fn byte_to_col(line: &str, byte: usize) -> usize {
    line.get(..byte.min(line.len()))
        .map(|prefix| prefix.chars().count())
        .unwrap_or_else(|| line.chars().count())
}

fn col_at_width(text: &str, target: usize, tab_len: u8) -> usize {
    let mut column = 0usize;
    let mut chars = 0usize;
    for character in text.chars() {
        if column >= target {
            break;
        }
        let width = char_display_width(character, column, tab_len);
        if column + width > target {
            break;
        }
        column += width;
        chars += 1;
    }
    chars
}

fn display_width(text: &str, tab_len: u8) -> usize {
    display_width_to(text, 0, tab_len)
}

fn display_width_from(text: &str, start_width: usize, tab_len: u8) -> usize {
    display_width_to(text, start_width, tab_len).saturating_sub(start_width)
}

fn display_width_to(text: &str, mut width: usize, tab_len: u8) -> usize {
    for character in text.chars() {
        width += char_display_width(character, width, tab_len);
    }
    width
}

fn char_display_width(character: char, column: usize, tab_len: u8) -> usize {
    if character == '\t' {
        let tab = usize::from(tab_len.max(1));
        (tab - (column % tab)).max(1)
    } else {
        character.width().unwrap_or(0)
    }
}

fn clamp_u16(value: usize) -> u16 {
    value.min(usize::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    fn render(editor: &mut MarkdownTextArea<'_>, width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| editor.render(frame, frame.area(), true))
            .unwrap();
        terminal
    }

    #[test]
    fn terminal_command_hint_uses_the_shared_empty_line_rule() {
        let mut editor = MarkdownTextArea::new(
            ["title", "", "body"],
            MarkdownTextAreaStyle {
                gutter: Style::new().fg(Color::DarkGray),
                ..MarkdownTextAreaStyle::default()
            },
        );
        editor.text_area_mut().set_placeholder_text("");
        editor.move_cursor(CursorMove::Jump(1, 0));
        let hint = MarkdownCommandHint::new("Type '/' for commands");
        let backend = TestBackend::new(50, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                editor.render_with_command_hint(frame, frame.area(), true, Some(&hint), false);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = (0..buffer.area.width)
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();
        assert!(row.contains("Type '/' for commands"));
        let start = row.find("Type").unwrap() as u16;
        assert_eq!(buffer[(start, 1)].fg, Color::DarkGray);
    }

    #[cfg(feature = "ui-bridge")]
    fn ui_event(revision: u64, action: crate::UiAction) -> UiEvent {
        UiEvent::new(
            "app-1",
            "person-1",
            "client-1",
            "renderer-1",
            "main",
            format!("event-{revision}"),
            revision,
            action,
        )
    }

    #[test]
    fn gutter_counts_digits_and_marks_only_logical_line_starts() {
        let style = MarkdownTextAreaStyle {
            current_gutter: Style::new().fg(Color::Yellow),
            gutter: Style::new().fg(Color::Blue),
            ..MarkdownTextAreaStyle::default()
        };
        let mut editor = MarkdownTextArea::new(["abcdefghij", "second"], style);
        let terminal = render(&mut editor, 10, 5);
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(1, 0)].symbol(), "1");
        assert_eq!(buffer[(1, 0)].fg, Color::Yellow);
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(3, 0)].symbol(), " ");
        assert_eq!(buffer[(4, 0)].symbol(), " ");
        assert_eq!(buffer[(5, 0)].symbol(), "a");
        assert_eq!(buffer[(1, 1)].symbol(), " ");
        assert_eq!(buffer[(1, 2)].symbol(), "2");
        assert_eq!(buffer[(1, 2)].fg, Color::Blue);
    }

    #[test]
    fn overflow_reserves_right_edge_and_uses_shared_scrollbar() {
        let lines = (1..=10).map(|line| format!("row {line}"));
        let mut editor = MarkdownTextArea::new(lines, MarkdownTextAreaStyle::default());
        let terminal = render(&mut editor, 20, 4);
        let buffer = terminal.backend().buffer();

        assert_eq!(editor.area(), Rect::new(1, 0, 18, 4));
        assert_eq!(editor.max_scroll(), 6);
        assert_eq!(buffer[(19, 0)].symbol(), "┃");
        assert_eq!(editor.hit_test(Position::new(1, 0)), (0, 0));
    }

    #[test]
    fn hit_testing_accounts_for_padding_gutter_wrap_and_unicode_width() {
        let mut editor = MarkdownTextArea::new(["ab界defghij"], MarkdownTextAreaStyle::default());
        let _terminal = render(&mut editor, 12, 4);

        assert_eq!(editor.hit_test(Position::new(5, 0)), (0, 0));
        assert_eq!(editor.hit_test(Position::new(7, 0)), (0, 2));
        assert_eq!(editor.hit_test(Position::new(8, 0)), (0, 2));
        assert_eq!(editor.hit_test(Position::new(5, 1)), (0, 3));
    }

    #[test]
    fn scrolling_clamps_at_both_complete_pages() {
        let lines = (1..=20).map(|line| format!("row {line}"));
        let mut editor = MarkdownTextArea::new(lines, MarkdownTextAreaStyle::default());
        let _terminal = render(&mut editor, 30, 5);

        assert!(editor.scroll_lines(i16::MAX));
        assert_eq!(editor.scroll_top().0, 15);
        assert!(!editor.scroll_lines(1));
        assert!(editor.scroll_lines(i16::MIN));
        assert_eq!(editor.scroll_top().0, 0);
        assert!(!editor.scroll_lines(-1));
    }

    #[test]
    fn set_lines_resets_component_scroll() {
        let lines = (1..=10).map(|line| format!("row {line}"));
        let mut editor = MarkdownTextArea::new(lines, MarkdownTextAreaStyle::default());
        let _terminal = render(&mut editor, 20, 3);
        assert!(editor.scroll_lines(2));

        editor.set_lines(vec!["replacement".into()], (0, 0));

        assert_eq!(editor.scroll_top(), (0, 0));
        assert_eq!(editor.lines(), ["replacement"]);
    }

    #[test]
    fn drop_hover_moves_the_cursor_and_auto_scrolls_at_an_edge() {
        let lines = (1..=20).map(|line| format!("row {line}"));
        let mut editor = MarkdownTextArea::new(lines, MarkdownTextAreaStyle::default());
        let _terminal = render(&mut editor, 30, 5);
        editor.start_selection();

        assert!(editor.position_drop_cursor(Position::new(8, 4)));
        assert_eq!(editor.scroll_top().0, 1);
        assert_eq!(editor.cursor(), (5, 2));
        assert!(!editor.is_selecting());

        assert!(editor.position_drop_cursor(Position::new(8, 4)));
        assert_eq!(editor.scroll_top().0, 2);
        assert_eq!(editor.cursor(), (6, 2));
    }

    #[test]
    #[cfg(feature = "ui-bridge")]
    fn semantic_snapshot_uses_the_live_ratatui_document_and_utf16_selection() {
        let mut editor =
            MarkdownTextArea::new(["a🙂b", "second"], MarkdownTextAreaStyle::default());
        editor.move_cursor(CursorMove::Jump(0, 2));
        let config = MarkdownEditorConfig::new("markdown-editor")
            .title("README.md")
            .dirty(true)
            .presentation(MarkdownPresentation::Split);

        let node = editor.ui_node(&config);
        assert_eq!(node.id.as_str(), "markdown-editor");
        let crate::UiComponent::MarkdownEditor(spec) = node.element else {
            panic!("Markdown adapter must produce a Markdown editor");
        };
        assert_eq!(spec.text, "a🙂b\nsecond");
        assert_eq!(
            spec.selection,
            TextSelection::caret(TextPosition::new(0, 3))
        );
        assert_eq!(spec.presentation, MarkdownPresentation::Split);
        assert_eq!(spec.title.as_deref(), Some("README.md"));
        assert!(spec.dirty);
    }

    #[test]
    #[cfg(feature = "ui-bridge")]
    fn semantic_text_edits_apply_to_the_same_ratatui_state() {
        let mut editor =
            MarkdownTextArea::new(["a🙂b", "second"], MarkdownTextAreaStyle::default());
        let config = MarkdownEditorConfig::new("markdown-editor");
        let event = ui_event(
            9,
            crate::UiAction::replace_range(
                "markdown-editor",
                TextEdit::new(
                    crate::TextRange::new(TextPosition::new(0, 1), TextPosition::new(0, 3)),
                    "native",
                ),
            ),
        );

        assert_eq!(
            editor.handle_ui_event(9, &config, &event).unwrap(),
            Some(MarkdownEditorEvent::TextChanged { changed: true })
        );
        assert_eq!(editor.lines(), ["anativeb", "second"]);
        assert_eq!(editor.cursor(), (0, 7));
    }

    #[test]
    #[cfg(feature = "ui-bridge")]
    fn semantic_selection_preserves_anchor_direction_and_rejects_stale_events() {
        let mut editor = MarkdownTextArea::new(["abcdef"], MarkdownTextAreaStyle::default());
        let config = MarkdownEditorConfig::new("markdown-editor");
        let selection = TextSelection {
            anchor: TextPosition::new(0, 5),
            head: TextPosition::new(0, 2),
        };
        let event = ui_event(
            2,
            crate::UiAction::set_selection("markdown-editor", selection),
        );

        assert_eq!(
            editor.handle_ui_event(2, &config, &event).unwrap(),
            Some(MarkdownEditorEvent::SelectionChanged)
        );
        assert_eq!(editor.cursor(), (0, 2));
        assert_eq!(editor.selection_range(), Some(((0, 2), (0, 5))));
        let node = editor.ui_node(&config);
        let crate::UiComponent::MarkdownEditor(spec) = node.element else {
            panic!("Markdown adapter must produce a Markdown editor");
        };
        assert_eq!(spec.selection, selection);

        assert!(matches!(
            editor.handle_ui_event(3, &config, &event),
            Err(MarkdownEditorEventError::StaleRevision {
                expected: 3,
                received: 2
            })
        ));
    }

    #[test]
    #[cfg(feature = "ui-bridge")]
    fn semantic_commands_return_app_owned_intent() {
        let mut editor = MarkdownTextArea::new(["# Hello"], MarkdownTextAreaStyle::default());
        let config = MarkdownEditorConfig::new("markdown-editor")
            .open_menu_action(MarkdownEditorActions::OPEN_MENU);
        let save = ui_event(
            1,
            crate::UiAction::command("markdown-editor", MarkdownEditorActions::SAVE),
        );
        assert_eq!(
            editor.handle_ui_event(1, &config, &save).unwrap(),
            Some(MarkdownEditorEvent::SaveRequested)
        );

        let presentation = ui_event(
            1,
            crate::UiAction::new(
                "markdown-editor",
                MarkdownEditorActions::SET_PRESENTATION,
                UiEventKind::Change,
                UiEventValue::Text("preview".to_owned()),
            ),
        );
        assert_eq!(
            editor.handle_ui_event(1, &config, &presentation).unwrap(),
            Some(MarkdownEditorEvent::PresentationRequested(
                MarkdownPresentation::Preview
            ))
        );

        let palette = ui_event(
            1,
            crate::UiAction::new(
                "markdown-editor",
                MarkdownEditorActions::OPEN_MENU,
                UiEventKind::Command,
                UiEventValue::Text(MarkdownMenuTrigger::Palette.as_str().to_owned()),
            ),
        );
        assert_eq!(
            editor.handle_ui_event(1, &config, &palette).unwrap(),
            Some(MarkdownEditorEvent::MenuRequested(
                MarkdownMenuTrigger::Palette
            ))
        );
    }
}
