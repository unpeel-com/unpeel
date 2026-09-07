//! Full-width bordered text input that grows with its contents.
//!
//! The plain configuration is a rounded bordered field with a placeholder for
//! forms, search boxes, or commit messages. Optional pieces turn it into a
//! chat-style prompt bar: a prompt glyph, border titles, a busy status row
//! above the box, and a key-hint footer below it.

#[cfg(feature = "ui-bridge")]
use std::fmt;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Widget};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

use crate::input::{
    char_len, char_to_byte, display_width, next_word_boundary, ordered, previous_word_boundary,
    take_width, word_bounds,
};
use crate::{ColorScheme, DoubleClickTracker, InputFieldTheme, KitTheme, SPINNER_FRAMES};
#[cfg(feature = "ui-bridge")]
use crate::{
    NodeId, UI_PROTOCOL_MAX_VERSION, UI_PROTOCOL_MIN_VERSION, UI_PROTOCOL_NAME, UiEvent,
    UiEventKind, UiEventValue, UiNode,
};

/// Renderer capability advertised for the closed `textBox` root component.
pub const TEXT_BOX_COMPONENT_CAPABILITY: &str = "textBox";
const DEFAULT_MAX_TEXT_BYTES: usize = 1024 * 1024;

const DEFAULT_MIN_ROWS: u16 = 3;
const DEFAULT_MAX_ROWS: u16 = 10;
const HINT_SEPARATOR: &str = " │ ";

/// Colors used by [`TextBox`]. Text, placeholder, prompt glyph, and
/// selection colors come from the embedded [`InputFieldTheme`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextBoxTheme {
    pub input: InputFieldTheme,
    pub border: Style,
    pub border_focused: Style,
    pub title: Style,
    pub cursor: Style,
    pub spinner: Style,
    pub status: Style,
    pub status_meta: Style,
    pub hint_key: Style,
    pub hint_label: Style,
    pub hint_separator: Style,
}

impl TextBoxTheme {
    #[must_use]
    pub const fn dark() -> Self {
        Self::for_palette(KitTheme::dark(), InputFieldTheme::dark())
    }

    #[must_use]
    pub const fn light() -> Self {
        Self::for_palette(KitTheme::light(), InputFieldTheme::light())
    }

    #[must_use]
    pub const fn for_color_scheme(scheme: ColorScheme) -> Self {
        Self::for_palette(
            KitTheme::for_scheme(scheme),
            InputFieldTheme::for_color_scheme(scheme),
        )
    }

    #[must_use]
    pub fn detected() -> Self {
        Self::for_color_scheme(ColorScheme::detect())
    }

    const fn for_palette(palette: KitTheme, mut input: InputFieldTheme) -> Self {
        input.left_padding = 1;
        Self {
            input,
            border: Style::new().fg(palette.subtle),
            border_focused: Style::new().fg(palette.muted),
            title: Style::new().fg(palette.subtle),
            cursor: Style::new().add_modifier(Modifier::REVERSED),
            spinner: Style::new().fg(palette.accent),
            status: Style::new().fg(palette.text),
            status_meta: Style::new().fg(palette.muted),
            hint_key: Style::new().fg(palette.text).add_modifier(Modifier::BOLD),
            hint_label: Style::new().fg(palette.subtle),
            hint_separator: Style::new().fg(palette.subtle),
        }
    }
}

impl Default for TextBoxTheme {
    fn default() -> Self {
        Self::dark()
    }
}

/// One `Key:label` entry in the footer hint row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyHint {
    pub key: String,
    pub label: String,
}

impl KeyHint {
    #[must_use]
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: sanitize_line(key.into()),
            label: sanitize_line(label.into()),
        }
    }
}

impl<K: Into<String>, L: Into<String>> From<(K, L)> for KeyHint {
    fn from((key, label): (K, L)) -> Self {
        Self::new(key, label)
    }
}

/// Where a border title is embedded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TitlePosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// What Enter does. Shift+Enter and Alt+Enter always insert a newline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmitMode {
    /// Enter submits the text; Shift+Enter or Alt+Enter inserts a newline.
    #[default]
    Enter,
    /// Enter always inserts a newline; the host decides when to call
    /// [`TextBox::submit`] (for example on Ctrl+Enter).
    Never,
}

/// Status shown above the box while the App waits on something.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BusyStatus {
    /// Left text such as `Waiting for response…`.
    pub label: String,
    /// Elapsed time appended to the label as `8.5s`.
    pub elapsed: Duration,
    /// Right-aligned metadata such as `51s ↓342k [stop]`.
    pub right_meta: String,
}

impl BusyStatus {
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: sanitize_line(label.into()),
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn with_elapsed(mut self, elapsed: Duration) -> Self {
        self.elapsed = elapsed;
        self
    }

    #[must_use]
    pub fn with_right_meta(mut self, meta: impl Into<String>) -> Self {
        self.right_meta = sanitize_line(meta.into());
        self
    }
}

/// Backend-neutral editing actions understood by [`TextBox::handle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextBoxAction {
    Insert(char),
    InsertText(String),
    /// Inserts a line break (Shift+Enter / Alt+Enter).
    Newline,
    Backspace,
    Delete,
    Clear,
    SelectAll,
    Left {
        extend: bool,
        word: bool,
    },
    Right {
        extend: bool,
        word: bool,
    },
    Up {
        extend: bool,
    },
    Down {
        extend: bool,
    },
    /// Start of the current visual row.
    Home {
        extend: bool,
    },
    /// End of the current visual row.
    End {
        extend: bool,
    },
    DocumentStart {
        extend: bool,
    },
    DocumentEnd {
        extend: bool,
    },
    /// Submits the current text (Enter). See [`TextBox::submit`].
    Submit,
}

impl TextBoxAction {
    /// Default crossterm key mapping. Returns `None` for keys the box does
    /// not understand so Apps can layer their own shortcuts on top.
    #[must_use]
    pub fn from_key(key: &KeyEvent, submit_mode: SubmitMode) -> Option<Self> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let word = control || alt;
        Some(match key.code {
            KeyCode::Enter if shift || alt || submit_mode == SubmitMode::Never => Self::Newline,
            KeyCode::Enter => Self::Submit,
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Delete => Self::Delete,
            KeyCode::Left => Self::Left {
                extend: shift,
                word,
            },
            KeyCode::Right => Self::Right {
                extend: shift,
                word,
            },
            KeyCode::Up => Self::Up { extend: shift },
            KeyCode::Down => Self::Down { extend: shift },
            KeyCode::Home if control => Self::DocumentStart { extend: shift },
            KeyCode::End if control => Self::DocumentEnd { extend: shift },
            KeyCode::Home => Self::Home { extend: shift },
            KeyCode::End => Self::End { extend: shift },
            KeyCode::Char('a') if control => Self::SelectAll,
            KeyCode::Char('e') if control => Self::End { extend: false },
            KeyCode::Char('j') if control => Self::Newline,
            KeyCode::Char('u') if control => Self::Clear,
            KeyCode::Char(character) if !control && !alt => Self::Insert(character),
            _ => return None,
        })
    }
}

/// Result of [`TextBox::handle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextBoxOutcome {
    Unchanged,
    Changed,
    /// Enter was pressed with non-blank text; the box has been cleared.
    Submitted(String),
}

impl TextBoxOutcome {
    #[must_use]
    pub const fn changed(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

/// One embedded border title on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBoxTitle {
    pub text: String,
    pub position: TitlePosition,
}

/// Busy status on the wire; elapsed time is whole milliseconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextBoxBusy {
    pub label: String,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub right_meta: String,
}

impl From<&BusyStatus> for TextBoxBusy {
    fn from(status: &BusyStatus) -> Self {
        Self {
            label: status.label.clone(),
            elapsed_ms: u64::try_from(status.elapsed.as_millis()).unwrap_or(u64::MAX),
            right_meta: status.right_meta.clone(),
        }
    }
}

impl From<&TextBoxBusy> for BusyStatus {
    fn from(busy: &TextBoxBusy) -> Self {
        Self {
            label: busy.label.clone(),
            elapsed: Duration::from_millis(busy.elapsed_ms),
            right_meta: busy.right_meta.clone(),
        }
    }
}

/// Action identifiers a semantic renderer may send back for a text box.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextBoxActions {
    /// `change` event carrying the full replacement text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_text: Option<String>,
    /// `submit` event carrying the submitted text; the App owns what happens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit: Option<String>,
}

impl TextBoxActions {
    pub const SET_TEXT: &'static str = "set-text";
    pub const SUBMIT: &'static str = "submit";

    /// Both actions with their conventional identifiers.
    #[must_use]
    pub fn editable() -> Self {
        Self {
            set_text: Some(Self::SET_TEXT.into()),
            submit: Some(Self::SUBMIT.into()),
        }
    }
}

/// Owned text box state interpreted by Ratatui, Swift, or web.
///
/// This is the wire form of [`TextBox`]: text, chrome, and actions without
/// renderer-local cursor, selection, or scroll state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextBoxSpec {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<TextBoxTitle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<KeyHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<TextBoxBusy>,
    #[serde(default)]
    pub submit_mode: SubmitMode,
    #[serde(default = "default_min_rows")]
    pub min_rows: u16,
    #[serde(default = "default_max_rows")]
    pub max_rows: u16,
    #[serde(default)]
    pub actions: TextBoxActions,
}

const fn default_min_rows() -> u16 {
    DEFAULT_MIN_ROWS
}

const fn default_max_rows() -> u16 {
    DEFAULT_MAX_ROWS
}

impl Default for TextBoxSpec {
    fn default() -> Self {
        Self {
            text: String::new(),
            placeholder: String::new(),
            prompt: String::new(),
            titles: Vec::new(),
            hints: Vec::new(),
            busy: None,
            submit_mode: SubmitMode::default(),
            min_rows: DEFAULT_MIN_ROWS,
            max_rows: DEFAULT_MAX_ROWS,
            actions: TextBoxActions::default(),
        }
    }
}

impl TextBoxSpec {
    #[must_use]
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            placeholder: placeholder.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }

    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    #[must_use]
    pub fn with_title(mut self, text: impl Into<String>, position: TitlePosition) -> Self {
        self.titles.retain(|title| title.position != position);
        self.titles.push(TextBoxTitle {
            text: text.into(),
            position,
        });
        self
    }

    #[must_use]
    pub fn with_footer_hints<H: Into<KeyHint>>(
        mut self,
        hints: impl IntoIterator<Item = H>,
    ) -> Self {
        self.hints = hints.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_busy(mut self, busy: Option<&BusyStatus>) -> Self {
        self.busy = busy.map(TextBoxBusy::from);
        self
    }

    #[must_use]
    pub const fn with_submit_mode(mut self, mode: SubmitMode) -> Self {
        self.submit_mode = mode;
        self
    }

    #[must_use]
    pub const fn with_rows(mut self, min_rows: u16, max_rows: u16) -> Self {
        self.min_rows = if min_rows == 0 { 1 } else { min_rows };
        self.max_rows = if max_rows < self.min_rows {
            self.min_rows
        } else {
            max_rows
        };
        self
    }

    #[must_use]
    pub fn with_actions(mut self, actions: TextBoxActions) -> Self {
        self.actions = actions;
        self
    }

    /// Validates the closed wire shape at `path` (for example `root`).
    pub fn validate(&self, path: &str) -> Result<(), crate::ComponentValidationError> {
        use crate::components::{validate_identifier, validate_text};
        validate_text(&self.text, DEFAULT_MAX_TEXT_BYTES, &format!("{path}.text"))?;
        validate_short(&self.placeholder, &format!("{path}.placeholder"))?;
        validate_short(&self.prompt, &format!("{path}.prompt"))?;
        for (index, title) in self.titles.iter().enumerate() {
            validate_short(&title.text, &format!("{path}.titles[{index}].text"))?;
            if self
                .titles
                .iter()
                .filter(|other| other.position == title.position)
                .count()
                > 1
            {
                return Err(crate::ComponentValidationError::new(
                    format!("{path}.titles[{index}].position"),
                    "each title position may appear at most once",
                ));
            }
        }
        for (index, hint) in self.hints.iter().enumerate() {
            validate_short(&hint.key, &format!("{path}.hints[{index}].key"))?;
            validate_short(&hint.label, &format!("{path}.hints[{index}].label"))?;
        }
        if let Some(busy) = &self.busy {
            validate_short(&busy.label, &format!("{path}.busy.label"))?;
            validate_short(&busy.right_meta, &format!("{path}.busy.rightMeta"))?;
        }
        if self.min_rows == 0 || self.max_rows < self.min_rows {
            return Err(crate::ComponentValidationError::new(
                format!("{path}.minRows"),
                "minRows must be at least 1 and at most maxRows",
            ));
        }
        if let Some(action) = &self.actions.set_text {
            validate_identifier(action, &format!("{path}.actions.setText"))?;
        }
        if let Some(action) = &self.actions.submit {
            validate_identifier(action, &format!("{path}.actions.submit"))?;
        }
        Ok(())
    }
}

fn validate_short(value: &str, path: &str) -> Result<(), crate::ComponentValidationError> {
    crate::components::validate_text(value, crate::components::MAX_SHORT_TEXT_BYTES, path)?;
    if value.contains('\n') {
        return Err(crate::ComponentValidationError::new(
            path,
            "must be a single line",
        ));
    }
    Ok(())
}

/// Cross-renderer identity and actions for a hosted [`TextBox`].
#[cfg(feature = "ui-bridge")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextBoxConfig {
    pub node_id: NodeId,
    pub actions: TextBoxActions,
}

#[cfg(feature = "ui-bridge")]
impl TextBoxConfig {
    #[must_use]
    pub fn new(node_id: impl Into<NodeId>) -> Self {
        Self {
            node_id: node_id.into(),
            actions: TextBoxActions::editable(),
        }
    }

    #[must_use]
    pub fn with_actions(mut self, actions: TextBoxActions) -> Self {
        self.actions = actions;
        self
    }
}

/// A text box action applied from a semantic renderer.
#[cfg(feature = "ui-bridge")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextBoxUiEvent {
    /// The renderer replaced the text; the terminal box now matches.
    TextChanged { changed: bool },
    /// The renderer submitted this text; the terminal box has been cleared.
    Submitted(String),
}

/// A matching text box action that cannot safely be applied.
#[cfg(feature = "ui-bridge")]
#[derive(Debug, PartialEq, Eq)]
pub enum TextBoxEventError {
    UnexpectedProtocol { protocol: String, version: u32 },
    StaleRevision { expected: u64, received: u64 },
    UnsupportedAction(String),
    InvalidEvent(String),
}

#[cfg(feature = "ui-bridge")]
impl fmt::Display for TextBoxEventError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedProtocol { protocol, version } => write!(
                formatter,
                "unexpected UI protocol {protocol}/{version}; expected {UI_PROTOCOL_NAME}/{UI_PROTOCOL_MIN_VERSION}..={UI_PROTOCOL_MAX_VERSION}"
            ),
            Self::StaleRevision { expected, received } => write!(
                formatter,
                "stale text box event revision {received}; current revision is {expected}"
            ),
            Self::UnsupportedAction(action) => {
                write!(formatter, "unsupported text box action {action:?}")
            }
            Self::InvalidEvent(message) => write!(formatter, "invalid text box event: {message}"),
        }
    }
}

#[cfg(feature = "ui-bridge")]
impl std::error::Error for TextBoxEventError {}

/// One wrapped visual row: a half-open character range. `hard` rows end at a
/// newline or at the end of the text; soft rows end at a wrap point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    start: usize,
    end: usize,
    hard: bool,
}

/// A rounded, bordered, full-width text input that grows with its contents.
///
/// Text and selection positions are Unicode character indexes into the
/// unwrapped text. The component owns wrapping, vertical growth between the
/// minimum and maximum row counts, scrolling, keyboard editing, selection,
/// and mouse selection. Apps ask [`Self::height_for_width`] for the space to
/// reserve, render [`Self::widget`], and may apply [`Self::cursor_position`]
/// to the frame when they prefer the native cursor over the drawn block.
///
/// `TextBox::new("Search…")` is a plain field. `with_rows(1, 1)` makes a
/// bordered single-line input. Chat prompts add [`Self::with_prompt`],
/// [`Self::with_title`], [`Self::set_busy`], and [`Self::with_footer_hints`].
#[derive(Debug)]
pub struct TextBox {
    text: String,
    cursor: usize,
    selection_anchor: Option<usize>,
    preferred_column: Option<usize>,
    focused: bool,
    prompt: String,
    placeholder: String,
    titles: Vec<(String, TitlePosition)>,
    hints: Vec<KeyHint>,
    submit_mode: SubmitMode,
    busy: Option<BusyStatus>,
    spinner_frame: usize,
    min_rows: u16,
    max_rows: u16,
    theme: TextBoxTheme,
    area: Rect,
    box_area: Rect,
    content_area: Rect,
    content_width: u16,
    cursor_position: Option<Position>,
    scroll_row: usize,
    drag_anchor: Option<usize>,
    clicks: DoubleClickTracker<(usize, usize)>,
}

impl TextBox {
    #[must_use]
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            selection_anchor: None,
            preferred_column: None,
            focused: false,
            prompt: String::new(),
            placeholder: sanitize_line(placeholder.into()),
            titles: Vec::new(),
            hints: Vec::new(),
            submit_mode: SubmitMode::default(),
            busy: None,
            spinner_frame: 0,
            min_rows: DEFAULT_MIN_ROWS,
            max_rows: DEFAULT_MAX_ROWS,
            theme: TextBoxTheme::default(),
            area: Rect::default(),
            box_area: Rect::default(),
            content_area: Rect::default(),
            content_width: 0,
            cursor_position: None,
            scroll_row: 0,
            drag_anchor: None,
            clicks: DoubleClickTracker::new(),
        }
    }

    /// Optional prompt glyph drawn before the first row, e.g. `❯ `.
    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.set_prompt(prompt);
        self
    }

    /// Adds a dim title embedded in the border line at `position`.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>, position: TitlePosition) -> Self {
        self.add_title(title, position);
        self
    }

    /// Chat-prompt convenience: a dim title in the bottom-right border.
    #[must_use]
    pub fn with_status_title(self, title: impl Into<String>) -> Self {
        self.with_title(title, TitlePosition::BottomRight)
    }

    #[must_use]
    pub const fn with_submit_mode(mut self, mode: SubmitMode) -> Self {
        self.submit_mode = mode;
        self
    }

    /// Footer `Key:label` hints separated by dim bars.
    #[must_use]
    pub fn with_footer_hints<H: Into<KeyHint>>(
        mut self,
        hints: impl IntoIterator<Item = H>,
    ) -> Self {
        self.set_footer_hints(hints);
        self
    }

    #[must_use]
    pub const fn with_theme(mut self, theme: TextBoxTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Minimum and maximum text rows (excluding the border). Below the
    /// maximum the box grows with its contents; beyond it the text scrolls.
    #[must_use]
    pub const fn with_rows(mut self, min_rows: u16, max_rows: u16) -> Self {
        let min_rows = if min_rows == 0 { 1 } else { min_rows };
        self.min_rows = min_rows;
        self.max_rows = if max_rows < min_rows {
            min_rows
        } else {
            max_rows
        };
        self
    }

    pub fn set_prompt(&mut self, prompt: impl Into<String>) -> bool {
        let prompt = sanitize_line(prompt.into());
        let changed = prompt != self.prompt;
        self.prompt = prompt;
        changed
    }

    /// Adds or replaces the title at `position`. An empty title removes it.
    pub fn add_title(&mut self, title: impl Into<String>, position: TitlePosition) -> bool {
        let title = sanitize_line(title.into());
        let previous = self.titles.clone();
        self.titles.retain(|(_, existing)| *existing != position);
        if !title.is_empty() {
            self.titles.push((title, position));
        }
        previous != self.titles
    }

    pub fn clear_titles(&mut self) -> bool {
        let changed = !self.titles.is_empty();
        self.titles.clear();
        changed
    }

    pub const fn set_submit_mode(&mut self, mode: SubmitMode) {
        self.submit_mode = mode;
    }

    #[must_use]
    pub const fn submit_mode(&self) -> SubmitMode {
        self.submit_mode
    }

    /// Maps a crossterm key through [`TextBoxAction::from_key`] using this
    /// box's [`SubmitMode`].
    #[must_use]
    pub fn action_for_key(&self, key: &KeyEvent) -> Option<TextBoxAction> {
        TextBoxAction::from_key(key, self.submit_mode)
    }

    pub fn set_footer_hints<H: Into<KeyHint>>(
        &mut self,
        hints: impl IntoIterator<Item = H>,
    ) -> bool {
        let hints = hints.into_iter().map(Into::into).collect::<Vec<_>>();
        let changed = hints != self.hints;
        self.hints = hints;
        changed
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) -> bool {
        let placeholder = sanitize_line(placeholder.into());
        let changed = placeholder != self.placeholder;
        self.placeholder = placeholder;
        changed
    }

    pub const fn set_theme(&mut self, theme: TextBoxTheme) {
        self.theme = theme;
    }

    /// Shows or hides the status row. The spinner restarts when busy begins.
    pub fn set_busy(&mut self, busy: Option<BusyStatus>) -> bool {
        let changed = busy != self.busy;
        if self.busy.is_none() && busy.is_some() {
            self.spinner_frame = 0;
        }
        self.busy = busy;
        changed
    }

    /// Advances the spinner. Returns `true` when a redraw is needed.
    pub fn tick(&mut self) -> bool {
        if self.busy.is_none() {
            return false;
        }
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
        true
    }

    #[must_use]
    pub const fn busy(&self) -> Option<&BusyStatus> {
        self.busy.as_ref()
    }

    #[must_use]
    pub fn busy_mut(&mut self) -> Option<&mut BusyStatus> {
        self.busy.as_mut()
    }

    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    #[must_use]
    pub fn titles(&self) -> &[(String, TitlePosition)] {
        &self.titles
    }

    #[must_use]
    pub fn footer_hints(&self) -> &[KeyHint] {
        &self.hints
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

    /// Whole area from the most recent render, including status and footer.
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.area
    }

    /// Bordered box area from the most recent render.
    #[must_use]
    pub const fn box_area(&self) -> Rect {
        self.box_area
    }

    /// Text area inside the border, after padding and the prompt glyph.
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

    /// Number of text rows the box currently shows (before the border).
    #[must_use]
    pub fn visible_rows(&self, width: u16) -> u16 {
        let content_width = self.content_width_for(width);
        let rows = self.layout(content_width).len();
        let rows = u16::try_from(rows).unwrap_or(u16::MAX);
        rows.clamp(self.min_rows, self.max_rows)
    }

    /// Total height (status row, bordered box, footer) needed at `width`.
    #[must_use]
    pub fn height_for_width(&self, width: u16) -> u16 {
        self.visible_rows(width)
            .saturating_add(2)
            .saturating_add(u16::from(self.busy.is_some()))
            .saturating_add(u16::from(!self.hints.is_empty()))
    }

    /// Replaces the contents, puts the cursor at the end, and clears selection.
    pub fn set_text(&mut self, text: impl Into<String>) -> bool {
        let text = sanitize(text.into());
        let changed = text != self.text;
        self.clicks.reset();
        self.text = text;
        self.cursor = char_len(&self.text);
        self.selection_anchor = None;
        self.preferred_column = None;
        self.scroll_row = 0;
        self.drag_anchor = None;
        changed
    }

    /// Applies one backend-neutral editing action.
    pub fn handle(&mut self, action: TextBoxAction) -> TextBoxOutcome {
        let changed = match action {
            TextBoxAction::Insert(character) => self.insert_text(character.to_string()),
            TextBoxAction::InsertText(text) => self.insert_text(text),
            TextBoxAction::Newline => self.insert_text("\n"),
            TextBoxAction::Backspace => self.backspace(),
            TextBoxAction::Delete => self.delete(),
            TextBoxAction::Clear => self.clear(),
            TextBoxAction::SelectAll => self.select_all(),
            TextBoxAction::Left { extend, word } => self.move_left(extend, word),
            TextBoxAction::Right { extend, word } => self.move_right(extend, word),
            TextBoxAction::Up { extend } => self.move_vertical(-1, extend),
            TextBoxAction::Down { extend } => self.move_vertical(1, extend),
            TextBoxAction::Home { extend } => {
                let (row, _) = self.cursor_row_col();
                let rows = self.layout(self.content_width);
                let target = rows.get(row).map_or(0, |row| row.start);
                self.move_to(target, extend)
            }
            TextBoxAction::End { extend } => {
                let (row, _) = self.cursor_row_col();
                let rows = self.layout(self.content_width);
                let target = rows.get(row).map_or(char_len(&self.text), |row| row.end);
                self.move_to(target, extend)
            }
            TextBoxAction::DocumentStart { extend } => self.move_to(0, extend),
            TextBoxAction::DocumentEnd { extend } => self.move_to(char_len(&self.text), extend),
            TextBoxAction::Submit => {
                return match self.submit() {
                    Some(text) => TextBoxOutcome::Submitted(text),
                    None => TextBoxOutcome::Unchanged,
                };
            }
        };
        if changed {
            TextBoxOutcome::Changed
        } else {
            TextBoxOutcome::Unchanged
        }
    }

    /// Takes the text when it is not blank and resets the box.
    pub fn submit(&mut self) -> Option<String> {
        if self.text.trim().is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.text);
        self.clear();
        Some(text)
    }

    pub fn insert_text(&mut self, text: impl Into<String>) -> bool {
        let insertion = sanitize(text.into());
        self.clicks.reset();
        self.preferred_column = None;
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
        self.preferred_column = None;
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
        self.preferred_column = None;
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
        self.preferred_column = None;
        let had_text = !self.text.is_empty();
        self.text.clear();
        self.cursor = 0;
        self.selection_anchor = None;
        self.scroll_row = 0;
        self.drag_anchor = None;
        had_text
    }

    pub fn select_all(&mut self) -> bool {
        self.clicks.reset();
        self.preferred_column = None;
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

    /// Moves to `target`, optionally extending the selection. Resets the
    /// sticky column used by vertical movement.
    pub fn move_to(&mut self, target: usize, extend: bool) -> bool {
        self.preferred_column = None;
        self.move_to_keeping_column(target, extend)
    }

    fn move_to_keeping_column(&mut self, target: usize, extend: bool) -> bool {
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

    fn move_vertical(&mut self, delta: isize, extend: bool) -> bool {
        let rows = self.layout(self.content_width);
        let (row, column) = row_col_of(&rows, &self.text, self.cursor);
        let column = self.preferred_column.unwrap_or(column);
        let Some(target_row) = row.checked_add_signed(delta) else {
            self.preferred_column = None;
            return self.move_to(0, extend);
        };
        if target_row >= rows.len() {
            self.preferred_column = None;
            return self.move_to(char_len(&self.text), extend);
        }
        let target = index_at_column(&rows[target_row], &self.text, column);
        let changed = self.move_to_keeping_column(target, extend);
        self.preferred_column = Some(column);
        changed
    }

    /// Scrolls the text rows without moving the cursor (mouse wheel).
    pub fn scroll_rows(&mut self, delta: isize) -> bool {
        let rows = self.layout(self.content_width).len();
        let visible = usize::from(self.content_area.height.max(1));
        let max_scroll = rows.saturating_sub(visible);
        let next = self.scroll_row.saturating_add_signed(delta).min(max_scroll);
        let changed = next != self.scroll_row;
        self.scroll_row = next;
        changed
    }

    /// Focuses and positions the cursor. A second click in the same word
    /// selects that word; Shift-click extends the current selection.
    pub fn mouse_down(&mut self, position: Position, extend: bool) -> bool {
        if !self.box_area.contains(position) {
            self.clicks.reset();
            return false;
        }
        let was_focused = self.focused;
        self.focused = true;
        self.preferred_column = None;
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
        let next = if position.y < self.content_area.y {
            0
        } else if position.y >= self.content_area.bottom() {
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

    #[must_use]
    pub fn widget(&mut self) -> TextBoxWidget<'_> {
        TextBoxWidget { prompt: self }
    }

    /// Wire projection of the current text and chrome.
    #[must_use]
    pub fn spec(&self) -> TextBoxSpec {
        TextBoxSpec {
            text: self.text.clone(),
            placeholder: self.placeholder.clone(),
            prompt: self.prompt.clone(),
            titles: self
                .titles
                .iter()
                .map(|(text, position)| TextBoxTitle {
                    text: text.clone(),
                    position: *position,
                })
                .collect(),
            hints: self.hints.clone(),
            busy: self.busy.as_ref().map(TextBoxBusy::from),
            submit_mode: self.submit_mode,
            min_rows: self.min_rows,
            max_rows: self.max_rows,
            actions: TextBoxActions::default(),
        }
    }

    /// Builds a terminal box from a wire spec (cursor at the end of the text).
    #[must_use]
    pub fn from_spec(spec: &TextBoxSpec) -> Self {
        let mut text_box = Self::new(spec.placeholder.clone())
            .with_prompt(spec.prompt.clone())
            .with_footer_hints(spec.hints.clone())
            .with_submit_mode(spec.submit_mode)
            .with_rows(spec.min_rows, spec.max_rows);
        for title in &spec.titles {
            text_box.add_title(title.text.clone(), title.position);
        }
        text_box.set_busy(spec.busy.as_ref().map(BusyStatus::from));
        text_box.set_text(spec.text.clone());
        text_box
    }

    /// Publishes this box as the closed `textBox` root component.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_node(&self, config: &TextBoxConfig) -> UiNode {
        let spec = self.spec().with_actions(config.actions.clone());
        UiNode::text_box(config.node_id.clone(), spec)
    }

    /// Applies one renderer event addressed to `config.node_id`.
    ///
    /// Returns `Ok(None)` for events aimed at other nodes. `set-text` replaces
    /// the text; `submit` clears the box and hands the text back to the App.
    #[cfg(feature = "ui-bridge")]
    pub fn handle_ui_event(
        &mut self,
        revision: u64,
        config: &TextBoxConfig,
        event: &UiEvent,
    ) -> Result<Option<TextBoxUiEvent>, TextBoxEventError> {
        if event.action.node_id != config.node_id {
            return Ok(None);
        }
        if event.protocol != UI_PROTOCOL_NAME
            || !(UI_PROTOCOL_MIN_VERSION..=UI_PROTOCOL_MAX_VERSION)
                .contains(&event.protocol_version)
        {
            return Err(TextBoxEventError::UnexpectedProtocol {
                protocol: event.protocol.clone(),
                version: event.protocol_version,
            });
        }
        if event.base_revision != revision {
            return Err(TextBoxEventError::StaleRevision {
                expected: revision,
                received: event.base_revision,
            });
        }
        let action = event.action.action.as_str();
        let matches = |configured: Option<&String>| configured.is_some_and(|id| id == action);
        if matches(config.actions.set_text.as_ref()) {
            let text = require_text(event, UiEventKind::Change)?;
            return Ok(Some(TextBoxUiEvent::TextChanged {
                changed: self.set_text(text),
            }));
        }
        if matches(config.actions.submit.as_ref()) {
            let text = require_text(event, UiEventKind::Submit)?;
            self.clear();
            return Ok(Some(TextBoxUiEvent::Submitted(text.to_owned())));
        }
        Err(TextBoxEventError::UnsupportedAction(action.to_owned()))
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

    fn cursor_row_col(&self) -> (usize, usize) {
        let rows = self.layout(self.content_width);
        row_col_of(&rows, &self.text, self.cursor)
    }

    fn content_width_for(&self, width: u16) -> u16 {
        let inner = width.saturating_sub(2);
        let padding = self.theme.input.left_padding.min(inner);
        let prompt = u16::try_from(display_width(&self.prompt)).unwrap_or(u16::MAX);
        inner
            .saturating_sub(padding)
            .saturating_sub(prompt)
            .saturating_sub(1)
    }

    /// Greedy word wrap into visual rows. `width == 0` means unbounded.
    fn layout(&self, width: u16) -> Vec<Row> {
        wrap_rows(&self.text, usize::from(width))
    }

    fn hit_index(&self, position: Position) -> usize {
        let rows = self.layout(self.content_width);
        if rows.is_empty() || self.content_area.is_empty() {
            return 0;
        }
        let row_offset = usize::from(position.y.saturating_sub(self.content_area.y));
        let row_index = self.scroll_row.saturating_add(row_offset);
        if position.y < self.content_area.y {
            return rows.first().map_or(0, |row| row.start);
        }
        let Some(row) = rows.get(row_index) else {
            return char_len(&self.text);
        };
        if position.x <= self.content_area.x {
            return row.start;
        }
        let column = usize::from(position.x.saturating_sub(self.content_area.x));
        index_at_column(row, &self.text, column)
    }

    fn ensure_cursor_visible(&mut self, rows: &[Row]) {
        let (row, _) = row_col_of(rows, &self.text, self.cursor);
        let visible = usize::from(self.content_area.height.max(1));
        let max_scroll = rows.len().saturating_sub(visible);
        self.scroll_row = self.scroll_row.min(max_scroll);
        if row < self.scroll_row {
            self.scroll_row = row;
        } else if row >= self.scroll_row + visible {
            self.scroll_row = row + 1 - visible;
        }
    }

    fn render(&mut self, area: Rect, buffer: &mut Buffer) {
        self.area = area;
        self.cursor = self.cursor.min(char_len(&self.text));
        self.cursor_position = None;
        if area.is_empty() {
            self.box_area = Rect::default();
            self.content_area = Rect::default();
            return;
        }
        buffer.set_style(area, self.theme.input.style);

        let mut remaining = area;
        if self.busy.is_some() && remaining.height > 0 {
            let status_area = Rect {
                height: 1,
                ..remaining
            };
            self.render_status(status_area, buffer);
            remaining.y += 1;
            remaining.height -= 1;
        }
        let footer_rows = u16::from(!self.hints.is_empty() && remaining.height > 0);
        let footer_area = Rect {
            y: remaining.bottom().saturating_sub(footer_rows),
            height: footer_rows,
            ..remaining
        };
        remaining.height -= footer_rows;

        let wanted_rows = self.visible_rows(area.width);
        self.box_area = Rect {
            height: wanted_rows.saturating_add(2).min(remaining.height),
            ..remaining
        };
        self.render_box(buffer);
        if footer_rows > 0 {
            self.render_footer(footer_area, buffer);
        }
    }

    fn render_status(&self, area: Rect, buffer: &mut Buffer) {
        let Some(busy) = &self.busy else {
            return;
        };
        let elapsed = format!(" {:.1}s", busy.elapsed.as_secs_f64());
        let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
        let base = self.theme.input.style;
        let mut spans = vec![
            Span::styled(" ", base),
            Span::styled(spinner, base.patch(self.theme.spinner)),
            Span::styled(" ", base),
            Span::styled(busy.label.clone(), base.patch(self.theme.status)),
            Span::styled(elapsed.clone(), base.patch(self.theme.status_meta)),
        ];
        // Leading pad, spinner, space, label, elapsed.
        let left_width = 2 + display_width(spinner) + display_width(&busy.label) + elapsed.len();
        let meta_width = display_width(&busy.right_meta);
        // Leave one trailing column after the metadata.
        let available = usize::from(area.width).saturating_sub(1);
        if !busy.right_meta.is_empty() && left_width + 1 + meta_width <= available {
            let gap = available - left_width - meta_width;
            spans.push(Span::styled(" ".repeat(gap), base));
            spans.push(Span::styled(
                busy.right_meta.clone(),
                base.patch(self.theme.status_meta),
            ));
        }
        Line::from(spans).render(area, buffer);
    }

    fn render_footer(&self, area: Rect, buffer: &mut Buffer) {
        let base = self.theme.input.style;
        let mut spans = vec![Span::styled(" ", base)];
        for (index, hint) in self.hints.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(
                    HINT_SEPARATOR,
                    base.patch(self.theme.hint_separator),
                ));
            }
            spans.push(Span::styled(
                hint.key.clone(),
                base.patch(self.theme.hint_key),
            ));
            spans.push(Span::styled(":", base.patch(self.theme.hint_label)));
            spans.push(Span::styled(
                hint.label.clone(),
                base.patch(self.theme.hint_label),
            ));
        }
        Line::from(spans).render(area, buffer);
    }

    fn render_box(&mut self, buffer: &mut Buffer) {
        let area = self.box_area;
        if area.is_empty() {
            self.content_area = Rect::default();
            return;
        }
        let border = if self.focused {
            self.theme.border_focused
        } else {
            self.theme.border
        };
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(self.theme.input.style.patch(border));
        let border_style = self.theme.input.style.patch(border);
        let title_style = self.theme.input.style.patch(self.theme.title);
        for (title, position) in &self.titles {
            // Keep one border segment between the corner and the title.
            let dash = Span::styled("─", border_style);
            let text = Span::styled(format!(" {title} "), title_style);
            block = match position {
                TitlePosition::TopLeft => {
                    block.title_top(Line::from(vec![dash, text]).left_aligned())
                }
                TitlePosition::TopRight => {
                    block.title_top(Line::from(vec![text, dash]).right_aligned())
                }
                TitlePosition::BottomLeft => {
                    block.title_bottom(Line::from(vec![dash, text]).left_aligned())
                }
                TitlePosition::BottomRight => {
                    block.title_bottom(Line::from(vec![text, dash]).right_aligned())
                }
            };
        }
        let inner = block.inner(area);
        block.render(area, buffer);

        let padding = self.theme.input.left_padding.min(inner.width);
        let prompt_width = u16::try_from(display_width(&self.prompt))
            .unwrap_or(u16::MAX)
            .min(inner.width.saturating_sub(padding));
        self.content_area = Rect {
            x: inner.x.saturating_add(padding).saturating_add(prompt_width),
            y: inner.y,
            width: inner
                .width
                .saturating_sub(padding)
                .saturating_sub(prompt_width)
                .saturating_sub(1),
            height: inner.height,
        };
        self.content_width = self.content_area.width;
        let rows = self.layout(self.content_width);
        self.ensure_cursor_visible(&rows);
        if inner.is_empty() {
            return;
        }

        let base = self.theme.input.style;
        let active = if self.focused {
            self.theme.input.focused
        } else {
            self.theme.input.text
        };
        let selected = self.selection_range();
        let prompt_x = inner.x.saturating_add(padding);
        for (offset, y) in (inner.y..inner.bottom()).enumerate() {
            let row_index = self.scroll_row + offset;
            if row_index == 0 {
                buffer.set_string(
                    prompt_x,
                    y,
                    &self.prompt,
                    base.patch(self.theme.input.prompt),
                );
            }
            let Some(row) = rows.get(row_index) else {
                continue;
            };
            if self.text.is_empty() {
                buffer.set_string(
                    self.content_area.x,
                    y,
                    take_width(&self.placeholder, usize::from(self.content_area.width)),
                    base.patch(self.theme.input.placeholder),
                );
                continue;
            }
            let mut x = self.content_area.x;
            for (index, character) in self
                .text
                .chars()
                .enumerate()
                .skip(row.start)
                .take(row.end - row.start)
            {
                let width = u16::try_from(character.width().unwrap_or(0).max(1)).unwrap_or(1);
                if x.saturating_add(width) > self.content_area.right() {
                    break;
                }
                let mut style = base.patch(active);
                if selected.is_some_and(|(start, end)| index >= start && index < end) {
                    style = style.patch(self.theme.input.selection);
                }
                buffer.set_string(x, y, character.to_string(), style);
                x = x.saturating_add(width);
            }
        }

        if self.focused && !self.content_area.is_empty() {
            let (row, column) = row_col_of(&rows, &self.text, self.cursor);
            if row >= self.scroll_row && row - self.scroll_row < usize::from(inner.height) {
                let x = self
                    .content_area
                    .x
                    .saturating_add(u16::try_from(column).unwrap_or(u16::MAX))
                    .min(self.content_area.right().saturating_sub(1));
                let y = inner.y + u16::try_from(row - self.scroll_row).unwrap_or(0);
                let position = Position::new(x, y);
                self.cursor_position = Some(position);
                buffer.set_style(Rect::new(x, y, 1, 1), self.theme.cursor);
            }
        }
    }
}

/// Stateful Ratatui view returned by [`TextBox::widget`].
pub struct TextBoxWidget<'a> {
    prompt: &'a mut TextBox,
}

impl Widget for TextBoxWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.prompt.render(area, buffer);
    }
}

#[cfg(feature = "ui-bridge")]
fn require_text(event: &UiEvent, expected: UiEventKind) -> Result<&str, TextBoxEventError> {
    if event.action.kind != expected {
        return Err(TextBoxEventError::InvalidEvent(format!(
            "action {} requires {expected:?}, received {:?}",
            event.action.action, event.action.kind
        )));
    }
    let UiEventValue::Text(text) = &event.action.value else {
        return Err(TextBoxEventError::InvalidEvent(format!(
            "action {} requires a text value",
            event.action.action
        )));
    };
    if text.len() > DEFAULT_MAX_TEXT_BYTES {
        return Err(TextBoxEventError::InvalidEvent(format!(
            "text exceeds {DEFAULT_MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(text)
}

/// Keeps newlines, normalises CRLF, tabs become spaces, drops other controls.
fn sanitize(text: String) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter_map(|character| match character {
            '\n' => Some('\n'),
            '\r' => Some('\n'),
            '\t' => Some(' '),
            other if other.is_control() => None,
            other => Some(other),
        })
        .collect()
}

fn sanitize_line(text: String) -> String {
    crate::input::sanitize(text)
}

fn wrap_rows(text: &str, width: usize) -> Vec<Row> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut line_start = 0usize;
    loop {
        let line_end = characters[line_start..]
            .iter()
            .position(|character| *character == '\n')
            .map_or(characters.len(), |offset| line_start + offset);
        wrap_line(&characters, line_start, line_end, width, &mut rows);
        if line_end >= characters.len() {
            break;
        }
        line_start = line_end + 1;
    }
    rows
}

fn wrap_line(characters: &[char], start: usize, end: usize, width: usize, rows: &mut Vec<Row>) {
    if width == 0 || start == end {
        rows.push(Row {
            start,
            end,
            hard: true,
        });
        return;
    }
    let mut row_start = start;
    while row_start < end {
        let mut used = 0usize;
        let mut index = row_start;
        let mut last_space: Option<usize> = None;
        while index < end {
            let character_width = characters[index].width().unwrap_or(0).max(1);
            if used + character_width > width {
                // A space that would overflow hangs past the edge so the row
                // breaks cleanly after it.
                if characters[index] == ' ' {
                    last_space = Some(index);
                    index += 1;
                }
                break;
            }
            used += character_width;
            if characters[index] == ' ' {
                last_space = Some(index);
            }
            index += 1;
        }
        if index >= end {
            rows.push(Row {
                start: row_start,
                end,
                hard: true,
            });
            return;
        }
        // The row is full. Prefer breaking after the last space so words stay
        // whole; fall back to a hard character break.
        let break_at = match last_space {
            Some(space) if space + 1 > row_start => space + 1,
            _ => index.max(row_start + 1),
        };
        rows.push(Row {
            start: row_start,
            end: break_at,
            hard: false,
        });
        row_start = break_at;
    }
    rows.push(Row {
        start: end,
        end,
        hard: true,
    });
}

fn row_col_of(rows: &[Row], text: &str, index: usize) -> (usize, usize) {
    let row_index = rows
        .iter()
        .position(|row| index < row.end || (index == row.end && row.hard))
        .unwrap_or(rows.len().saturating_sub(1));
    let Some(row) = rows.get(row_index) else {
        return (0, 0);
    };
    let column = text
        .chars()
        .skip(row.start)
        .take(index.saturating_sub(row.start))
        .map(|character| character.width().unwrap_or(0).max(1))
        .sum();
    (row_index, column)
}

fn index_at_column(row: &Row, text: &str, column: usize) -> usize {
    let mut used = 0usize;
    for (index, character) in text
        .chars()
        .enumerate()
        .skip(row.start)
        .take(row.end - row.start)
    {
        let width = character.width().unwrap_or(0).max(1);
        if column < used + width {
            return if (column - used) * 2 < width {
                index
            } else {
                index + 1
            };
        }
        used += width;
    }
    // Soft-wrapped rows end at a consumed space; keep the cursor on this row.
    if row.hard {
        row.end
    } else {
        row.end.saturating_sub(1).max(row.start)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    fn draw(prompt: &mut TextBox, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| frame.render_widget(prompt.widget(), frame.area()))
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (buffer.area.x..buffer.area.right())
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn wraps_words_and_places_cursor_on_the_wrapped_row() {
        let mut prompt = TextBox::new("Ask anything").with_prompt("❯ ");
        prompt.set_focused(true);
        prompt.set_text("alpha beta gamma delta");
        // Width 16: border 2, padding 1, prompt 2, right pad 1 -> 10 columns.
        let buffer = draw(&mut prompt, 16, 5);
        assert_eq!(row_text(&buffer, 1), "│ ❯ alpha beta │");
        assert_eq!(row_text(&buffer, 2), "│   gamma      │");
        assert_eq!(row_text(&buffer, 3), "│   delta      │");
        assert_eq!(prompt.cursor_position(), Some(Position::new(9, 3)));
        assert_eq!(buffer[(9, 3)].modifier, Modifier::REVERSED);
    }

    #[test]
    fn grows_from_min_rows_to_max_rows_then_scrolls() {
        let mut prompt = TextBox::new("Ask").with_rows(3, 4);
        assert_eq!(prompt.height_for_width(20), 5);
        prompt.set_text("one\ntwo");
        assert_eq!(prompt.height_for_width(20), 5);
        prompt.set_text("one\ntwo\nthree\nfour");
        assert_eq!(prompt.height_for_width(20), 6);
        prompt.set_text("one\ntwo\nthree\nfour\nfive\nsix");
        assert_eq!(prompt.height_for_width(20), 6);

        prompt.set_focused(true);
        let buffer = draw(&mut prompt, 20, 6);
        assert_eq!(row_text(&buffer, 1).trim_matches('│').trim(), "three");
        assert_eq!(row_text(&buffer, 4).trim_matches('│').trim(), "six");
        assert_eq!(prompt.cursor_position(), Some(Position::new(5, 4)));

        prompt.handle(TextBoxAction::DocumentStart { extend: false });
        let buffer = draw(&mut prompt, 20, 6);
        assert_eq!(row_text(&buffer, 1), "│ one              │");
        assert_eq!(prompt.cursor_position(), Some(Position::new(2, 1)));
    }

    #[test]
    fn status_title_is_embedded_in_the_bottom_border() {
        let mut prompt = TextBox::new("Ask")
            .with_prompt("❯ ")
            .with_status_title("Grok 4.6 (xhigh) · always-approve")
            .with_footer_hints([("Shift+Tab", "mode"), ("Esc", "cancel")]);
        let buffer = draw(&mut prompt, 50, 6);
        assert_eq!(row_text(&buffer, 0), format!("╭{}╮", "─".repeat(48)));
        let bottom = row_text(&buffer, 4);
        assert!(bottom.starts_with("╰─"), "{bottom}");
        assert!(
            bottom.ends_with(" Grok 4.6 (xhigh) · always-approve ─╯"),
            "{bottom}"
        );
        let title_start = 50 - 2 - " Grok 4.6 (xhigh) · always-approve ".chars().count() as u16;
        assert_eq!(
            buffer[(title_start, 4)].fg,
            TextBoxTheme::dark().title.fg.unwrap_or(Color::Reset)
        );
        assert_eq!(
            row_text(&buffer, 5).trim_end(),
            " Shift+Tab:mode │ Esc:cancel"
        );
        assert!(buffer[(1, 5)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(11, 5)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn busy_status_row_shows_spinner_elapsed_and_right_meta() {
        let mut prompt = TextBox::new("Ask");
        prompt.set_busy(Some(
            BusyStatus::new("Waiting for response…")
                .with_elapsed(Duration::from_millis(8_500))
                .with_right_meta("51s ↓342k [stop]"),
        ));
        assert_eq!(prompt.height_for_width(60), 6);
        let buffer = draw(&mut prompt, 60, 6);
        let status = row_text(&buffer, 0);
        assert!(
            status.starts_with(" ⠋ Waiting for response… 8.5s"),
            "{status}"
        );
        assert!(status.ends_with("51s ↓342k [stop] "), "{status}");
        assert!(prompt.tick());
        let buffer = draw(&mut prompt, 60, 6);
        assert!(row_text(&buffer, 0).starts_with(" ⠙ "));
        assert!(row_text(&buffer, 1).starts_with('╭'));
        prompt.set_busy(None);
        assert!(!prompt.tick());
        assert_eq!(prompt.height_for_width(60), 5);
    }

    #[test]
    fn editing_actions_handle_newlines_words_and_submission() {
        let mut prompt = TextBox::new("Ask");
        prompt.set_focused(true);
        prompt.handle(TextBoxAction::InsertText("hello world".into()));
        prompt.handle(TextBoxAction::Newline);
        prompt.handle(TextBoxAction::InsertText("second".into()));
        assert_eq!(prompt.text(), "hello world\nsecond");
        draw(&mut prompt, 40, 5);

        prompt.handle(TextBoxAction::Up { extend: false });
        assert_eq!(prompt.cursor(), 6);
        prompt.handle(TextBoxAction::Home { extend: false });
        assert_eq!(prompt.cursor(), 0);
        prompt.handle(TextBoxAction::Right {
            extend: true,
            word: true,
        });
        assert_eq!(prompt.selected_text(), Some("hello "));
        prompt.handle(TextBoxAction::End { extend: false });
        assert_eq!(prompt.cursor(), 11);
        prompt.handle(TextBoxAction::Down { extend: true });
        assert_eq!(prompt.selected_text(), Some("\nsecond"));

        assert_eq!(
            prompt.handle(TextBoxAction::Submit),
            TextBoxOutcome::Submitted("hello world\nsecond".into())
        );
        assert_eq!(prompt.text(), "");
        prompt.handle(TextBoxAction::InsertText("   ".into()));
        assert_eq!(
            prompt.handle(TextBoxAction::Submit),
            TextBoxOutcome::Unchanged
        );
    }

    #[test]
    fn mouse_selects_across_wrapped_rows() {
        let mut prompt = TextBox::new("Ask").with_prompt("❯ ");
        prompt.set_text("alpha beta gamma delta");
        draw(&mut prompt, 16, 5);
        assert!(prompt.mouse_down(Position::new(4, 1), false));
        prompt.mouse_drag(Position::new(6, 2));
        assert_eq!(prompt.selected_text(), Some("alpha beta ga"));
        prompt.mouse_up();
        prompt.mouse_down(Position::new(5, 3), false);
        prompt.mouse_up();
        prompt.mouse_down(Position::new(5, 3), false);
        assert_eq!(prompt.selected_text(), Some("delta"));
    }

    #[test]
    fn key_mapping_distinguishes_submit_from_newline() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            TextBoxAction::from_key(&enter, SubmitMode::Enter),
            Some(TextBoxAction::Submit)
        );
        assert_eq!(
            TextBoxAction::from_key(&enter, SubmitMode::Never),
            Some(TextBoxAction::Newline)
        );
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(
            TextBoxAction::from_key(&shift_enter, SubmitMode::Enter),
            Some(TextBoxAction::Newline)
        );
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(
            TextBoxAction::from_key(&alt_enter, SubmitMode::Enter),
            Some(TextBoxAction::Newline)
        );
    }

    #[test]
    fn spec_round_trips_through_json_and_validates() {
        let mut prompt = TextBox::new("Ask")
            .with_prompt("❯ ")
            .with_status_title("model")
            .with_footer_hints([("Esc", "cancel")])
            .with_rows(2, 5);
        prompt.set_text("hello");
        prompt.set_busy(Some(
            BusyStatus::new("Waiting").with_elapsed(Duration::from_millis(1_250)),
        ));
        let spec = prompt.spec().with_actions(TextBoxActions::editable());
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["titles"][0]["position"], "bottomRight");
        assert_eq!(json["busy"]["elapsedMs"], 1250);
        assert_eq!(json["submitMode"], "enter");
        let decoded: TextBoxSpec = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, spec);
        assert!(decoded.validate("root").is_ok());
        let rebuilt = TextBox::from_spec(&decoded);
        assert_eq!(rebuilt.text(), "hello");
        assert_eq!(
            rebuilt.busy().map(|busy| busy.elapsed),
            Some(Duration::from_millis(1_250))
        );

        let mut invalid = spec.clone();
        invalid.min_rows = 0;
        assert_eq!(invalid.validate("root").unwrap_err().path, "root.minRows");
        let mut duplicate = spec.clone();
        duplicate.titles.push(TextBoxTitle {
            text: "again".into(),
            position: TitlePosition::BottomRight,
        });
        assert_eq!(
            duplicate.validate("root").unwrap_err().path,
            "root.titles[0].position"
        );
        let defaults: TextBoxSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.min_rows, DEFAULT_MIN_ROWS);
        assert_eq!(defaults.max_rows, DEFAULT_MAX_ROWS);
    }

    #[cfg(feature = "ui-bridge")]
    #[test]
    fn ui_events_replace_text_and_submit() {
        use crate::{UiAction, UiComponent};

        let mut prompt = TextBox::new("Ask");
        let config = TextBoxConfig::new("prompt");
        let node = prompt.ui_node(&config);
        assert_eq!(node.id.as_str(), "prompt");
        let UiComponent::TextBox(spec) = &node.element else {
            panic!("expected textBox root");
        };
        assert_eq!(spec.actions, TextBoxActions::editable());
        assert!(node.validate().is_ok());

        let event = |action: &str, kind: UiEventKind, value: UiEventValue, revision: u64| {
            UiEvent::new(
                "app",
                "person",
                "client",
                "renderer",
                "main",
                "event-1",
                revision,
                UiAction::new("prompt", action, kind, value),
            )
        };
        let set = event(
            "set-text",
            UiEventKind::Change,
            UiEventValue::Text("draft".into()),
            3,
        );
        assert_eq!(
            prompt.handle_ui_event(3, &config, &set).unwrap(),
            Some(TextBoxUiEvent::TextChanged { changed: true })
        );
        assert_eq!(prompt.text(), "draft");
        assert_eq!(
            prompt.handle_ui_event(4, &config, &set).unwrap_err(),
            TextBoxEventError::StaleRevision {
                expected: 4,
                received: 3
            }
        );
        let wrong_kind = event(
            "set-text",
            UiEventKind::Submit,
            UiEventValue::Text("x".into()),
            3,
        );
        assert!(matches!(
            prompt.handle_ui_event(3, &config, &wrong_kind),
            Err(TextBoxEventError::InvalidEvent(_))
        ));
        let submit = event(
            "submit",
            UiEventKind::Submit,
            UiEventValue::Text("draft".into()),
            3,
        );
        assert_eq!(
            prompt.handle_ui_event(3, &config, &submit).unwrap(),
            Some(TextBoxUiEvent::Submitted("draft".into()))
        );
        assert_eq!(prompt.text(), "");
        let other = UiEvent::new(
            "app",
            "person",
            "client",
            "renderer",
            "main",
            "event-2",
            3,
            UiAction::new(
                "elsewhere",
                "submit",
                UiEventKind::Submit,
                UiEventValue::None,
            ),
        );
        assert_eq!(prompt.handle_ui_event(3, &config, &other).unwrap(), None);
    }

    #[test]
    fn plain_single_row_field_and_title_positions() {
        let mut field = TextBox::new("Search files")
            .with_rows(1, 1)
            .with_title("Search", TitlePosition::TopLeft);
        assert_eq!(field.height_for_width(24), 3);
        let buffer = draw(&mut field, 24, 3);
        assert_eq!(row_text(&buffer, 0), "╭─ Search ─────────────╮");
        assert_eq!(row_text(&buffer, 1), "│ Search files         │");
        assert_eq!(row_text(&buffer, 2), format!("╰{}╯", "─".repeat(22)));

        field.set_text("main.rs");
        field.set_focused(true);
        let buffer = draw(&mut field, 24, 3);
        assert_eq!(row_text(&buffer, 1), "│ main.rs              │");
        assert_eq!(field.cursor_position(), Some(Position::new(9, 1)));

        field.add_title("12 hits", TitlePosition::BottomLeft);
        field.add_title("", TitlePosition::TopLeft);
        let buffer = draw(&mut field, 24, 3);
        assert_eq!(row_text(&buffer, 0), format!("╭{}╮", "─".repeat(22)));
        assert_eq!(row_text(&buffer, 2), "╰─ 12 hits ────────────╯");
    }
}
