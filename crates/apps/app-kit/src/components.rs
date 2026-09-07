//! Closed, standalone-first components for data and document Apps.
//!
//! The same owned values render through Ratatui and, when `ui-bridge` is
//! enabled, serialize into the semantic component channel. Containment stays
//! deliberately slot-based: a [`List`] owns only [`ListItem`] values, and a
//! row slot accepts only the controls enumerated by [`ListItemSlot`].

#![cfg_attr(not(feature = "ui-bridge"), allow(dead_code))]

use std::collections::HashSet;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::{
    BarChart, Content, ContentState, ContentTheme, Gauge, InputField, InputFieldTheme, KitTheme,
    LineChart, ListNavigationAction, ListNavigationOutcome, ListPageBehavior, ListState,
    RowPointerDecision, RowPrimaryRole, SELECTABLE_LEFT_PADDING, SelectableRow, SemanticMenu,
    Sparkline, TerminalPointerPhase, TerminalPointerState, VerticalScrollbar,
};

/// Renderer capability for the v1 Page container.
pub const PAGE_COMPONENT_CAPABILITY: &str = "page";
/// Renderer capability for the v1 List container.
pub const LIST_COMPONENT_CAPABILITY: &str = "list";
/// Renderer capability for the v1 ListItem row.
pub const LIST_ITEM_COMPONENT_CAPABILITY: &str = "listItem";
/// Renderer capability for ListItem subtitle/trailing-value metadata.
pub const LIST_ITEM_METADATA_CAPABILITY: &str = "listItemMetadata";
/// Renderer capability for an activatable ListItem row.
pub const LIST_ITEM_ACTIVATE_CAPABILITY: &str = "listItemActivate";
/// Renderer capability for status/badge/busy ListItem presentation.
pub const LIST_ITEM_PRESENTATION_CAPABILITY: &str = "listItemPresentation";
/// Renderer capability for semantic styled runs inside ListItem text fields.
pub const LIST_ITEM_STYLED_TEXT_CAPABILITY: &str = "listItemStyledText";
/// Renderer capability for primary row roles and their native affordances.
pub const LIST_ITEM_ROLE_CAPABILITY: &str = "listItemRole";
/// Renderer capability for authoritative selection and shared list navigation.
pub const LIST_SELECTION_CAPABILITY: &str = "listSelection";
/// Renderer capability for static leading status symbols.
pub const STATUS_SYMBOL_COMPONENT_CAPABILITY: &str = "statusSymbol";
/// Renderer capability for compact ListItem badges.
pub const BADGE_COMPONENT_CAPABILITY: &str = "badge";
/// Renderer capability for the v1 Toggle control.
pub const TOGGLE_COMPONENT_CAPABILITY: &str = "toggle";
/// Renderer capability for the v1 Input control.
pub const INPUT_COMPONENT_CAPABILITY: &str = "input";
/// Renderer capability for the v1 Button control.
pub const BUTTON_COMPONENT_CAPABILITY: &str = "button";
/// Renderer capability for Page-level back navigation.
pub const PAGE_BACK_CAPABILITY: &str = "pageBack";
/// Renderer capability for an ordered screen-level action footer.
pub const FOOTER_ACTIONS_CAPABILITY: &str = "footerActions";

const MAX_ITEMS: usize = 100_000;
pub(crate) const MAX_SHORT_TEXT_BYTES: usize = 4 * 1024;
const MAX_LABEL_BYTES: usize = 16 * 1024;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_LIST_ITEM_TEXT_RUNS: usize = 256;

/// Visual intent for a semantic [`Button`]. The renderer chooses its native
/// platform treatment; these are not arbitrary style tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ButtonRole {
    #[default]
    Default,
    Primary,
    Destructive,
}

/// One semantic action control accepted by named component slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Button {
    pub id: String,
    pub label: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "is_default_button_role")]
    pub role: ButtonRole,
}

impl Button {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            action: action.into(),
            role: ButtonRole::Default,
        }
    }

    #[must_use]
    pub const fn role(mut self, role: ButtonRole) -> Self {
        self.role = role;
        self
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        validate_text(&self.label, MAX_SHORT_TEXT_BYTES, &format!("{path}.label"))?;
        validate_identifier(&self.action, &format!("{path}.action"))
    }
}

const fn is_default_button_role(role: &ButtonRole) -> bool {
    matches!(role, ButtonRole::Default)
}

/// Visual intent for a screen-level footer action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FooterActionRole {
    #[default]
    Default,
    Danger,
}

const fn is_default_footer_action_role(role: &FooterActionRole) -> bool {
    matches!(role, FooterActionRole::Default)
}

/// One App-owned action in an ordered screen footer.
///
/// `accelerator` uses App Kit's closed one-key grammar: one printable ASCII
/// character, `ctrl+<alphanumeric>`, or `escape`, `enter`, and `space`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FooterAction {
    pub id: String,
    pub label: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_footer_action_role")]
    pub role: FooterActionRole,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// The action is in progress; the terminal animates a braille spinner
    /// beside the label. Native and web renderers may show their own
    /// activity indicator.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
}

impl FooterAction {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            action: action.into(),
            accelerator: None,
            role: FooterActionRole::Default,
            disabled: false,
            busy: false,
        }
    }

    /// Marks the action as in progress; see [`Self::busy`].
    #[must_use]
    pub const fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    #[must_use]
    pub fn accelerator(mut self, accelerator: impl Into<String>) -> Self {
        self.accelerator = Some(accelerator.into());
        self
    }

    #[must_use]
    pub const fn role(mut self, role: FooterActionRole) -> Self {
        self.role = role;
        self
    }

    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    #[must_use]
    pub fn accelerator_label(&self) -> Option<String> {
        self.accelerator.as_deref().map(|accelerator| {
            accelerator.strip_prefix("ctrl+").map_or_else(
                || match accelerator {
                    "escape" => "Esc".to_owned(),
                    "enter" => "Enter".to_owned(),
                    "space" => "Space".to_owned(),
                    _ => accelerator.to_owned(),
                },
                |key| format!("^{}", key.to_ascii_uppercase()),
            )
        })
    }

    /// Matches one terminal key without introducing an App-side keymap.
    #[must_use]
    pub fn matches_key(&self, key: &KeyEvent) -> bool {
        if self.disabled || key.kind != KeyEventKind::Press {
            return false;
        }
        let Some(accelerator) = self.accelerator.as_deref() else {
            return false;
        };
        let command_modifiers = KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER
            | KeyModifiers::META;
        if let Some(character) = accelerator.strip_prefix("ctrl+") {
            return key.modifiers.contains(KeyModifiers::CONTROL)
                && !key
                    .modifiers
                    .intersects(command_modifiers - KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char(candidate) if character.chars().next().is_some_and(|expected| candidate.eq_ignore_ascii_case(&expected)));
        }
        if key.modifiers.intersects(command_modifiers) {
            return false;
        }
        match accelerator {
            "escape" => key.code == KeyCode::Esc,
            "enter" => key.code == KeyCode::Enter,
            "space" => key.code == KeyCode::Char(' '),
            character => {
                matches!(key.code, KeyCode::Char(candidate) if character.starts_with(candidate))
            }
        }
    }

    /// Builds the exact action emitted by every hosted renderer.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action(&self) -> crate::UiAction {
        crate::UiAction::activate(self.id.clone(), self.action.clone())
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        validate_text(&self.label, MAX_SHORT_TEXT_BYTES, &format!("{path}.label"))?;
        validate_single_line(&self.label, &format!("{path}.label"))?;
        validate_identifier(&self.action, &format!("{path}.action"))?;
        if let Some(accelerator) = &self.accelerator
            && !valid_footer_accelerator(accelerator)
        {
            return Err(ComponentValidationError::new(
                format!("{path}.accelerator"),
                "must be one printable ASCII character, ctrl+<alphanumeric>, escape, enter, or space",
            ));
        }
        Ok(())
    }
}

fn valid_footer_accelerator(accelerator: &str) -> bool {
    matches!(accelerator, "escape" | "enter" | "space")
        || accelerator
            .strip_prefix("ctrl+")
            .is_some_and(|key| key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric())
        || (accelerator.len() == 1 && accelerator.as_bytes()[0].is_ascii_graphic())
}

/// Ordered Page-level controls shared by terminal, Swift, and web.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FooterActions {
    #[serde(default)]
    pub actions: Vec<FooterAction>,
}

impl FooterActions {
    #[must_use]
    pub fn new(actions: impl IntoIterator<Item = FooterAction>) -> Self {
        Self {
            actions: actions.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    #[must_use]
    pub fn action_for_key(&self, key: &KeyEvent) -> Option<&FooterAction> {
        self.actions.iter().find(|action| action.matches_key(key))
    }

    /// Resolves a terminal click through the same ordered action vocabulary
    /// used by accelerators, Swift toolbar buttons, and web footer buttons.
    #[must_use]
    pub fn action_for_mouse(&self, event: &MouseEvent, area: Rect) -> Option<&FooterAction> {
        TerminalPointerState::click_position(event)
            .and_then(|position| self.action_at(position, area))
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action_for_key(&self, key: &KeyEvent) -> Option<crate::UiAction> {
        self.action_for_key(key).map(FooterAction::ui_action)
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action_for_mouse(&self, event: &MouseEvent, area: Rect) -> Option<crate::UiAction> {
        self.action_for_mouse(event, area)
            .map(FooterAction::ui_action)
    }

    /// Resolves a pointer against the exact compact terminal hint geometry.
    #[must_use]
    pub fn action_at(
        &self,
        position: ratatui::layout::Position,
        area: Rect,
    ) -> Option<&FooterAction> {
        if !area.contains(position) {
            return None;
        }
        let mut x = area.x.saturating_add(2);
        for action in &self.actions {
            let key_width = action
                .accelerator_label()
                .as_deref()
                .map_or(0, UnicodeWidthStr::width);
            let separator = usize::from(key_width > 0);
            let width = key_width
                .saturating_add(separator)
                .saturating_add(UnicodeWidthStr::width(action.label.as_str()));
            let end = x.saturating_add(u16::try_from(width).unwrap_or(u16::MAX));
            if position.x >= x && position.x < end && !action.disabled {
                return Some(action);
            }
            x = end.saturating_add(2);
        }
        None
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        if self.actions.len() > MAX_ITEMS {
            return Err(ComponentValidationError::new(
                path,
                format!("footer action count exceeds {MAX_ITEMS}"),
            ));
        }
        let mut ids = HashSet::new();
        let mut accelerators = HashSet::new();
        for (index, action) in self.actions.iter().enumerate() {
            action.validate(&format!("{path}.actions[{index}]"))?;
            if !ids.insert(action.id.as_str()) {
                return Err(ComponentValidationError::new(
                    format!("{path}.actions[{index}].id"),
                    format!("duplicate FooterAction id {:?}", action.id),
                ));
            }
            if let Some(accelerator) = &action.accelerator
                && !accelerators.insert(accelerator.as_str())
            {
                return Err(ComponentValidationError::new(
                    format!("{path}.actions[{index}].accelerator"),
                    format!("duplicate footer accelerator {accelerator:?}"),
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub const fn widget(&self) -> FooterActionsWidget<'_> {
        FooterActionsWidget {
            footer: self,
            style: Style::new(),
            key_style: Style::new().add_modifier(Modifier::BOLD),
            label_style: Style::new(),
            danger_style: Style::new(),
            disabled_style: Style::new().add_modifier(Modifier::DIM),
            hover_style: Style::new().add_modifier(Modifier::UNDERLINED),
            pressed_style: Style::new()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
            pointer: TerminalPointerState::new(),
            spinner_frame: 0,
        }
    }
}

/// Compact key-hint bar used by every Ratatui screen-root interpreter.
pub struct FooterActionsWidget<'a> {
    footer: &'a FooterActions,
    style: Style,
    key_style: Style,
    label_style: Style,
    danger_style: Style,
    disabled_style: Style,
    hover_style: Style,
    pressed_style: Style,
    pointer: TerminalPointerState,
    spinner_frame: usize,
}

impl FooterActionsWidget<'_> {
    /// Frame for the braille spinner drawn beside busy actions.
    #[must_use]
    pub const fn spinner_frame(mut self, frame: usize) -> Self {
        self.spinner_frame = frame;
        self
    }

    #[must_use]
    pub const fn styles(
        mut self,
        style: Style,
        key_style: Style,
        label_style: Style,
        danger_style: Style,
        disabled_style: Style,
    ) -> Self {
        self.style = style;
        self.key_style = key_style;
        self.label_style = label_style;
        self.danger_style = danger_style;
        self.disabled_style = disabled_style;
        self
    }

    /// Supplies renderer-local hover/press state. Action identity remains in
    /// the immutable [`FooterActions`] value.
    #[must_use]
    pub const fn pointer(mut self, pointer: TerminalPointerState) -> Self {
        self.pointer = pointer;
        self
    }

    #[must_use]
    pub const fn interaction_styles(mut self, hover: Style, pressed: Style) -> Self {
        self.hover_style = hover;
        self.pressed_style = pressed;
        self
    }
}

impl Widget for FooterActionsWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        buffer.set_style(area, self.style);
        let mut spans = vec![Span::raw("  ")];
        let mut x = area.x.saturating_add(2);
        for (index, action) in self.footer.actions.iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
                x = x.saturating_add(2);
            }
            let mut action_style = if action.disabled {
                self.disabled_style
            } else if action.role == FooterActionRole::Danger {
                self.danger_style
            } else {
                self.label_style
            };
            let accelerator = action.accelerator_label();
            let key_width = accelerator.as_deref().map_or(0, UnicodeWidthStr::width);
            let spinner_width = usize::from(action.busy) * 2;
            let width = key_width
                .saturating_add(usize::from(key_width > 0))
                .saturating_add(spinner_width)
                .saturating_add(UnicodeWidthStr::width(action.label.as_str()));
            let hit = Rect::new(
                x,
                area.y,
                u16::try_from(width)
                    .unwrap_or(u16::MAX)
                    .min(area.right().saturating_sub(x)),
                1,
            );
            let phase = if action.disabled {
                TerminalPointerPhase::Idle
            } else {
                self.pointer.phase(hit)
            };
            action_style = match phase {
                TerminalPointerPhase::Idle => action_style,
                TerminalPointerPhase::Hovered => action_style.patch(self.hover_style),
                TerminalPointerPhase::Pressed => action_style.patch(self.pressed_style),
            };
            if let Some(accelerator) = accelerator {
                let key_style = if action.disabled {
                    self.disabled_style
                } else {
                    match phase {
                        TerminalPointerPhase::Idle => self.key_style,
                        TerminalPointerPhase::Hovered => self.key_style.patch(self.hover_style),
                        TerminalPointerPhase::Pressed => self.key_style.patch(self.pressed_style),
                    }
                };
                spans.push(Span::styled(accelerator, key_style));
                spans.push(Span::styled(" ", action_style));
            }
            if action.busy {
                spans.push(Span::styled(
                    format!("{} ", crate::Spinner::glyph_for(self.spinner_frame)),
                    if action.disabled {
                        self.key_style
                    } else {
                        action_style
                    },
                ));
            }
            spans.push(Span::styled(action.label.clone(), action_style));
            x = x.saturating_add(u16::try_from(width).unwrap_or(u16::MAX));
        }
        Line::from(spans).render(area, buffer);
    }
}

/// Boolean control embeddable in an explicitly declared row slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Toggle {
    pub id: String,
    pub label: String,
    pub value: bool,
    pub set_value: String,
    #[serde(default, skip_serializing_if = "is_default_toggle_role")]
    pub role: ToggleRole,
}

/// What a Toggle means for its row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToggleRole {
    /// Marks the row done: the terminal strikes the label and `done` must
    /// mirror the value. Native renderers show a checkbox.
    #[default]
    Completion,
    /// A preference switch. The label stays as is and `done` is untouched.
    /// Native renderers show a switch.
    Setting,
}

const fn is_default_toggle_role(role: &ToggleRole) -> bool {
    matches!(role, ToggleRole::Completion)
}

impl Toggle {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        value: bool,
        set_value: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            set_value: set_value.into(),
            role: ToggleRole::Completion,
        }
    }

    /// A preference switch that never strikes through its row.
    #[must_use]
    pub fn setting(
        id: impl Into<String>,
        label: impl Into<String>,
        value: bool,
        set_value: impl Into<String>,
    ) -> Self {
        Self::new(id, label, value, set_value).role(ToggleRole::Setting)
    }

    #[must_use]
    pub const fn role(mut self, role: ToggleRole) -> Self {
        self.role = role;
        self
    }

    /// Compact terminal marker used inside a row.
    #[must_use]
    pub const fn marker(&self) -> &'static str {
        match (self.role, self.value) {
            (ToggleRole::Completion, true) => "[x]",
            (ToggleRole::Completion, false) => "[ ]",
            (ToggleRole::Setting, true) => "(●)",
            (ToggleRole::Setting, false) => "( )",
        }
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        validate_text(&self.label, MAX_SHORT_TEXT_BYTES, &format!("{path}.label"))?;
        validate_identifier(&self.set_value, &format!("{path}.setValue"))
    }
}

/// Selection-mode checkmark carried by one ListItem accessory.
///
/// Unlike a Toggle, Space remains PageDown; Enter/click applies this idempotent
/// boolean action through the App-owned reducer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkmark {
    pub id: String,
    pub label: String,
    pub value: bool,
    pub set_value: String,
}

impl Checkmark {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        value: bool,
        set_value: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            set_value: set_value.into(),
        }
    }

    #[must_use]
    pub const fn marker(&self) -> &'static str {
        if self.value { "✓" } else { " " }
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        validate_text(&self.label, MAX_SHORT_TEXT_BYTES, &format!("{path}.label"))?;
        validate_identifier(&self.set_value, &format!("{path}.setValue"))
    }
}

/// Semantic foreground treatment shared by terminal, native, and web rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListItemTone {
    #[default]
    Default,
    Muted,
    Accent,
    Info,
    Success,
    Warning,
    Danger,
}

/// Semantic label weight; arbitrary font/style values stay out of the wire vocabulary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListItemEmphasis {
    #[default]
    Regular,
    Strong,
}

/// One semantic span inside a ListItem label, detail, or trailing value.
///
/// Omitted tone/weight inherit the containing field. Explicit semantic tones
/// remain visible while a row is selected, so danger/warning state is not
/// erased by terminal focus styling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItemTextRun {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<ListItemTone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<ListItemEmphasis>,
}

impl ListItemTextRun {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: None,
            emphasis: None,
        }
    }

    #[must_use]
    pub const fn tone(mut self, tone: ListItemTone) -> Self {
        self.tone = Some(tone);
        self
    }

    #[must_use]
    pub const fn emphasis(mut self, emphasis: ListItemEmphasis) -> Self {
        self.emphasis = Some(emphasis);
        self
    }
}

fn list_item_runs_text(runs: &[ListItemTextRun]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

fn validate_list_item_text_runs(
    runs: &[ListItemTextRun],
    fallback: &str,
    path: &str,
) -> Result<(), ComponentValidationError> {
    if runs.is_empty() {
        return Ok(());
    }
    if runs.len() > MAX_LIST_ITEM_TEXT_RUNS {
        return Err(ComponentValidationError::new(
            path,
            format!("must contain at most {MAX_LIST_ITEM_TEXT_RUNS} styled runs"),
        ));
    }
    for (index, run) in runs.iter().enumerate() {
        let run_path = format!("{path}[{index}].text");
        validate_text(&run.text, MAX_LABEL_BYTES, &run_path)?;
        validate_single_line(&run.text, &run_path)?;
        if run.text.is_empty() {
            return Err(ComponentValidationError::new(
                run_path,
                "styled run text must not be empty",
            ));
        }
    }
    if list_item_runs_text(runs) != fallback {
        return Err(ComponentValidationError::new(
            path,
            "styled runs must concatenate exactly to the plain text fallback",
        ));
    }
    Ok(())
}

fn validate_optional_list_item_text_runs(
    runs: &[ListItemTextRun],
    fallback: Option<&str>,
    path: &str,
) -> Result<(), ComponentValidationError> {
    if runs.is_empty() {
        return Ok(());
    }
    let Some(fallback) = fallback else {
        return Err(ComponentValidationError::new(
            path,
            "styled runs require their plain text fallback",
        ));
    };
    validate_list_item_text_runs(runs, fallback, path)
}

/// Visual severity for a plain command/button row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListItemActionRole {
    #[default]
    Default,
    Destructive,
}

/// Compact leading state such as `M`, `A`, `D`, or an issue number/state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusSymbol {
    pub symbol: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "is_default_list_item_tone")]
    pub tone: ListItemTone,
    #[serde(default, skip_serializing_if = "is_default_list_item_emphasis")]
    pub emphasis: ListItemEmphasis,
    #[serde(default)]
    pub preserve_tone_when_selected: bool,
}

impl StatusSymbol {
    #[must_use]
    pub fn new(symbol: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            label: label.into(),
            tone: ListItemTone::Default,
            emphasis: ListItemEmphasis::Regular,
            preserve_tone_when_selected: false,
        }
    }

    #[must_use]
    pub const fn tone(mut self, tone: ListItemTone) -> Self {
        self.tone = tone;
        self
    }

    #[must_use]
    pub const fn emphasis(mut self, emphasis: ListItemEmphasis) -> Self {
        self.emphasis = emphasis;
        self
    }

    #[must_use]
    pub const fn preserve_tone_when_selected(mut self, preserve: bool) -> Self {
        self.preserve_tone_when_selected = preserve;
        self
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_text(
            &self.symbol,
            MAX_SHORT_TEXT_BYTES,
            &format!("{path}.symbol"),
        )?;
        if self.symbol.is_empty() || self.symbol.contains(['\n', '\r']) {
            return Err(ComponentValidationError::new(
                format!("{path}.symbol"),
                "status symbol must be a non-empty single line",
            ));
        }
        validate_text(&self.label, MAX_SHORT_TEXT_BYTES, &format!("{path}.label"))
    }
}

/// Compact inline metadata shown beside a row label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Badge {
    pub text: String,
    #[serde(default, skip_serializing_if = "is_default_list_item_tone")]
    pub tone: ListItemTone,
}

impl Badge {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: ListItemTone::Muted,
        }
    }

    #[must_use]
    pub const fn tone(mut self, tone: ListItemTone) -> Self {
        self.tone = tone;
        self
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_text(&self.text, MAX_SHORT_TEXT_BYTES, &format!("{path}.text"))?;
        if self.text.contains(['\n', '\r']) {
            return Err(ComponentValidationError::new(
                format!("{path}.text"),
                "badge text must be a single line",
            ));
        }
        Ok(())
    }
}

const fn is_default_list_item_tone(tone: &ListItemTone) -> bool {
    matches!(tone, ListItemTone::Default)
}

const fn is_default_list_item_emphasis(emphasis: &ListItemEmphasis) -> bool {
    matches!(emphasis, ListItemEmphasis::Regular)
}

const fn default_value_tone() -> ListItemTone {
    ListItemTone::Muted
}

const fn is_default_value_tone(tone: &ListItemTone) -> bool {
    matches!(tone, ListItemTone::Muted)
}

/// Controls currently allowed in a ListItem's named slots.
///
/// This enum grows deliberately only when a control has defined terminal,
/// native, web, and agent semantics. Arbitrary child nodes are never accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ListItemSlot {
    Toggle(Toggle),
    Status(StatusSymbol),
    Badge(Badge),
    Sparkline(Sparkline),
    Gauge(Gauge),
    Disclosure,
    Checkmark(Checkmark),
}

impl ListItemSlot {
    #[must_use]
    pub const fn toggle(toggle: Toggle) -> Self {
        Self::Toggle(toggle)
    }

    #[must_use]
    pub const fn status(status: StatusSymbol) -> Self {
        Self::Status(status)
    }

    #[must_use]
    pub const fn badge(badge: Badge) -> Self {
        Self::Badge(badge)
    }

    /// Read-only history data. Sparkline is deliberately trailing-only so a
    /// row remains semantically closed rather than becoming a chart container.
    #[must_use]
    pub const fn sparkline(sparkline: Sparkline) -> Self {
        Self::Sparkline(sparkline)
    }

    /// A bounded ratio with App-owned copy. Gauge is deliberately
    /// trailing-only so a row remains a semantic metric rather than an
    /// arbitrary chart container.
    #[must_use]
    pub const fn gauge(gauge: Gauge) -> Self {
        Self::Gauge(gauge)
    }

    /// UITableViewCell-style navigation affordance. The row's `activate`
    /// action remains the authoritative App/router transition.
    #[must_use]
    pub const fn disclosure() -> Self {
        Self::Disclosure
    }

    #[must_use]
    pub const fn checkmark(checkmark: Checkmark) -> Self {
        Self::Checkmark(checkmark)
    }

    #[must_use]
    pub const fn id(&self) -> Option<&str> {
        match self {
            Self::Toggle(toggle) => Some(toggle.id.as_str()),
            Self::Checkmark(checkmark) => Some(checkmark.id.as_str()),
            Self::Sparkline(sparkline) => Some(sparkline.id.as_str()),
            Self::Gauge(gauge) => Some(gauge.id.as_str()),
            Self::Status(_) | Self::Badge(_) | Self::Disclosure => None,
        }
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        match self {
            Self::Toggle(toggle) => toggle.validate(path),
            Self::Status(status) => status.validate(path),
            Self::Badge(badge) => badge.validate(path),
            Self::Sparkline(sparkline) => sparkline.validate(path),
            Self::Gauge(gauge) => gauge.validate(path),
            Self::Disclosure => Ok(()),
            Self::Checkmark(checkmark) => checkmark.validate(path),
        }
    }

    fn toggle_mut(&mut self, id: &str) -> Option<&mut Toggle> {
        match self {
            Self::Toggle(toggle) if toggle.id == id => Some(toggle),
            Self::Toggle(_)
            | Self::Status(_)
            | Self::Badge(_)
            | Self::Sparkline(_)
            | Self::Gauge(_)
            | Self::Disclosure
            | Self::Checkmark(_) => None,
        }
    }

    fn as_toggle(&self) -> Option<&Toggle> {
        match self {
            Self::Toggle(toggle) => Some(toggle),
            Self::Status(_)
            | Self::Badge(_)
            | Self::Sparkline(_)
            | Self::Gauge(_)
            | Self::Disclosure
            | Self::Checkmark(_) => None,
        }
    }

    fn checkmark_mut(&mut self, id: &str) -> Option<&mut Checkmark> {
        match self {
            Self::Checkmark(checkmark) if checkmark.id == id => Some(checkmark),
            Self::Toggle(_)
            | Self::Status(_)
            | Self::Badge(_)
            | Self::Sparkline(_)
            | Self::Gauge(_)
            | Self::Disclosure
            | Self::Checkmark(_) => None,
        }
    }

    fn as_checkmark(&self) -> Option<&Checkmark> {
        match self {
            Self::Checkmark(checkmark) => Some(checkmark),
            Self::Toggle(_)
            | Self::Status(_)
            | Self::Badge(_)
            | Self::Sparkline(_)
            | Self::Gauge(_)
            | Self::Disclosure => None,
        }
    }

    fn as_sparkline(&self) -> Option<&Sparkline> {
        match self {
            Self::Sparkline(sparkline) => Some(sparkline),
            Self::Toggle(_)
            | Self::Status(_)
            | Self::Badge(_)
            | Self::Gauge(_)
            | Self::Disclosure
            | Self::Checkmark(_) => None,
        }
    }

    fn as_gauge(&self) -> Option<&Gauge> {
        match self {
            Self::Gauge(gauge) => Some(gauge),
            Self::Toggle(_)
            | Self::Status(_)
            | Self::Badge(_)
            | Self::Sparkline(_)
            | Self::Disclosure
            | Self::Checkmark(_) => None,
        }
    }
}

/// How a terminal List lays out each row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ListRowLayout {
    /// One terminal row per item: detail follows the label and the value is
    /// right-aligned (the historical behavior).
    #[default]
    Inline,
    /// Two text rows per item: label and slots on the first, detail and value
    /// left-aligned in the value tone beneath it.
    Stacked,
    /// Stacked only while the row width is below `stack_below_width`.
    #[serde(rename_all = "camelCase")]
    Auto { stack_below_width: u16 },
}

impl ListRowLayout {
    /// Resolves the layout for a concrete row width.
    #[must_use]
    pub const fn stacks_at(self, row_width: u16) -> bool {
        match self {
            Self::Inline => false,
            Self::Stacked => true,
            Self::Auto { stack_below_width } => row_width < stack_below_width,
        }
    }
}

/// A full-width band rendered on its own row above or below a ListItem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ListItemBand {
    /// A progress meter spanning the content width with its label and
    /// caption.
    Gauge(Gauge),
    /// Read-only history spanning the content width.
    Sparkline(Sparkline),
    /// One line of toned text.
    #[serde(rename_all = "camelCase")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "is_default_list_item_tone")]
        tone: ListItemTone,
    },
    /// A thin rule spanning the content width.
    Divider,
}

impl ListItemBand {
    #[must_use]
    pub const fn gauge(gauge: Gauge) -> Self {
        Self::Gauge(gauge)
    }

    #[must_use]
    pub const fn sparkline(sparkline: Sparkline) -> Self {
        Self::Sparkline(sparkline)
    }

    #[must_use]
    pub fn text(text: impl Into<String>, tone: ListItemTone) -> Self {
        Self::Text {
            text: text.into(),
            tone,
        }
    }

    #[must_use]
    pub const fn divider() -> Self {
        Self::Divider
    }

    #[must_use]
    pub const fn id(&self) -> Option<&str> {
        match self {
            Self::Gauge(gauge) => Some(gauge.id.as_str()),
            Self::Sparkline(sparkline) => Some(sparkline.id.as_str()),
            Self::Text { .. } | Self::Divider => None,
        }
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        match self {
            Self::Gauge(gauge) => {
                gauge.validate(path)?;
                if gauge.activate.is_some() {
                    return Err(ComponentValidationError::new(
                        format!("{path}.activate"),
                        "band charts are read-only; use the row activate action",
                    ));
                }
                Ok(())
            }
            Self::Sparkline(sparkline) => {
                sparkline.validate(path)?;
                if sparkline.activate.is_some() {
                    return Err(ComponentValidationError::new(
                        format!("{path}.activate"),
                        "band charts are read-only; use the row activate action",
                    ));
                }
                Ok(())
            }
            Self::Text { text, .. } => {
                validate_text(text, MAX_LABEL_BYTES, &format!("{path}.text"))?;
                validate_single_line(text, &format!("{path}.text"))
            }
            Self::Divider => Ok(()),
        }
    }
}

/// Which side of the row a [`ListItemMedia`] column occupies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListItemMediaSide {
    #[default]
    Leading,
    Trailing,
}

const MAX_LIST_ITEM_MEDIA_WIDTH: u16 = 12;

/// A media column spanning every row of a ListItem.
///
/// The terminal paints a tone-colored block with an optional glyph or
/// initials; native and web renderers show `spec` when present. Real
/// terminal thumbnails are a follow-up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItemMedia {
    #[serde(default, skip_serializing_if = "is_default_media_side")]
    pub side: ListItemMediaSide,
    /// Terminal columns reserved for the media block (1 through 12).
    pub width: u16,
    /// Short glyph or initials centered in the terminal placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_list_item_tone")]
    pub tone: ListItemTone,
    /// Optional real image for renderers that can draw one.
    #[cfg(any(feature = "media", feature = "ui-bridge"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<crate::MediaSpec>,
}

const fn is_default_media_side(side: &ListItemMediaSide) -> bool {
    matches!(side, ListItemMediaSide::Leading)
}

impl ListItemMedia {
    #[must_use]
    pub fn new(side: ListItemMediaSide, width: u16) -> Self {
        Self {
            side,
            width: width.clamp(1, MAX_LIST_ITEM_MEDIA_WIDTH),
            glyph: None,
            tone: ListItemTone::Default,
            #[cfg(any(feature = "media", feature = "ui-bridge"))]
            spec: None,
        }
    }

    #[must_use]
    pub fn leading(width: u16) -> Self {
        Self::new(ListItemMediaSide::Leading, width)
    }

    #[must_use]
    pub fn trailing(width: u16) -> Self {
        Self::new(ListItemMediaSide::Trailing, width)
    }

    #[must_use]
    pub fn glyph(mut self, glyph: impl Into<String>) -> Self {
        self.glyph = Some(glyph.into());
        self
    }

    #[must_use]
    pub const fn tone(mut self, tone: ListItemTone) -> Self {
        self.tone = tone;
        self
    }

    #[cfg(any(feature = "media", feature = "ui-bridge"))]
    #[must_use]
    pub fn spec(mut self, spec: crate::MediaSpec) -> Self {
        self.spec = Some(spec);
        self
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        if self.width == 0 || self.width > MAX_LIST_ITEM_MEDIA_WIDTH {
            return Err(ComponentValidationError::new(
                format!("{path}.width"),
                format!("must be between 1 and {MAX_LIST_ITEM_MEDIA_WIDTH} columns"),
            ));
        }
        if let Some(glyph) = &self.glyph {
            validate_text(glyph, MAX_SHORT_TEXT_BYTES, &format!("{path}.glyph"))?;
            validate_single_line(glyph, &format!("{path}.glyph"))?;
        }
        #[cfg(any(feature = "media", feature = "ui-bridge"))]
        if let Some(spec) = &self.spec {
            spec.validate().map_err(|error| {
                ComponentValidationError::new(
                    format!("{path}.spec.{}", error.path.trim_start_matches("media.")),
                    error.message,
                )
            })?;
        }
        Ok(())
    }
}

/// One keyed, semantically meaningful row in a [`List`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub label_runs: Vec<ListItemTextRun>,
    #[serde(default, skip_serializing_if = "is_default_list_item_tone")]
    pub label_tone: ListItemTone,
    #[serde(default, skip_serializing_if = "is_default_list_item_emphasis")]
    pub emphasis: ListItemEmphasis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_runs: Vec<ListItemTextRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_runs: Vec<ListItemTextRun>,
    #[serde(
        default = "default_value_tone",
        skip_serializing_if = "is_default_value_tone"
    )]
    pub value_tone: ListItemTone,
    /// Minimum complete row width at which the trailing value is retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_min_width: Option<u16>,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub busy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading: Option<ListItemSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing: Option<ListItemSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessory: Option<ListItemSlot>,
    /// Full-width band rendered on its own row above the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<ListItemBand>,
    /// Full-width band rendered on its own row below the label/detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<ListItemBand>,
    /// Media column spanning every row of the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<ListItemMedia>,
    /// A passive separator row: a thin muted rule with the label as an
    /// optional caption. Never selectable; keyboard navigation skips it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub divider: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_list_item_action_role")]
    pub action_role: ListItemActionRole,
}

impl ListItem {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            label_runs: Vec::new(),
            label_tone: ListItemTone::Default,
            emphasis: ListItemEmphasis::Regular,
            detail: None,
            detail_runs: Vec::new(),
            value: None,
            value_runs: Vec::new(),
            value_tone: ListItemTone::Muted,
            value_min_width: None,
            done: false,
            busy: false,
            leading: None,
            trailing: None,
            accessory: None,
            top: None,
            bottom: None,
            media: None,
            divider: false,
            delete: None,
            activate: None,
            action_role: ListItemActionRole::Default,
        }
    }

    /// A standalone separator row. It is one row tall, draws a thin muted
    /// rule across the content width, is never selected, and keyboard
    /// navigation jumps over it.
    #[must_use]
    pub fn divider(id: impl Into<String>) -> Self {
        let mut item = Self::new(id, "");
        item.divider = true;
        item
    }

    /// A separator row with a short muted caption embedded in the rule.
    #[must_use]
    pub fn divider_labeled(id: impl Into<String>, label: impl Into<String>) -> Self {
        let mut item = Self::divider(id);
        item.label = label.into();
        item
    }

    #[must_use]
    pub const fn is_divider(&self) -> bool {
        self.divider
    }

    /// Secondary row text, rendered beneath the label natively and inline in
    /// compact terminal lists.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self.detail_runs.clear();
        self
    }

    /// Trailing read-only value such as a quota, date, or status.
    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self.value_runs.clear();
        self
    }

    /// Replaces the label with one closed sequence of semantic styled runs.
    /// The plain `label` fallback is derived from these runs and validated to
    /// remain byte-for-byte identical on the wire.
    #[must_use]
    pub fn label_runs(mut self, runs: impl IntoIterator<Item = ListItemTextRun>) -> Self {
        self.label_runs = runs.into_iter().collect();
        self.label = list_item_runs_text(&self.label_runs);
        self
    }

    /// Replaces the detail with semantic styled runs and derives its plain
    /// fallback for accessibility, agents, and older persisted snapshots.
    #[must_use]
    pub fn detail_runs(mut self, runs: impl IntoIterator<Item = ListItemTextRun>) -> Self {
        self.detail_runs = runs.into_iter().collect();
        self.detail = Some(list_item_runs_text(&self.detail_runs));
        self
    }

    /// Replaces the trailing value with semantic styled runs and derives its
    /// plain fallback. A trailing Gauge may coexist with this copy; the value
    /// is the visible caption and the Gauge contributes the compact meter.
    #[must_use]
    pub fn value_runs(mut self, runs: impl IntoIterator<Item = ListItemTextRun>) -> Self {
        self.value_runs = runs.into_iter().collect();
        self.value = Some(list_item_runs_text(&self.value_runs));
        self
    }

    #[must_use]
    pub const fn label_tone(mut self, tone: ListItemTone) -> Self {
        self.label_tone = tone;
        self
    }

    #[must_use]
    pub const fn emphasis(mut self, emphasis: ListItemEmphasis) -> Self {
        self.emphasis = emphasis;
        self
    }

    #[must_use]
    pub const fn value_tone(mut self, tone: ListItemTone) -> Self {
        self.value_tone = tone;
        self
    }

    /// Drops the trailing value below this total terminal-row width.
    #[must_use]
    pub const fn value_min_width(mut self, columns: u16) -> Self {
        self.value_min_width = Some(columns);
        self
    }

    #[must_use]
    pub const fn done(mut self, done: bool) -> Self {
        self.done = done;
        self
    }

    #[must_use]
    pub const fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    #[must_use]
    pub fn leading(mut self, slot: ListItemSlot) -> Self {
        self.leading = Some(slot);
        self
    }

    #[must_use]
    pub fn trailing(mut self, slot: ListItemSlot) -> Self {
        self.trailing = Some(slot);
        self
    }

    #[must_use]
    pub fn accessory(mut self, slot: ListItemSlot) -> Self {
        self.accessory = Some(slot);
        self
    }

    /// Full-width band on its own row above the label.
    #[must_use]
    pub fn top(mut self, band: ListItemBand) -> Self {
        self.top = Some(band);
        self
    }

    /// Full-width band on its own row below the label and detail. The usual
    /// case is a progress Gauge spanning the row.
    #[must_use]
    pub fn bottom(mut self, band: ListItemBand) -> Self {
        self.bottom = Some(band);
        self
    }

    /// Media column beside the text spanning every row of the item.
    #[must_use]
    pub fn media(mut self, media: ListItemMedia) -> Self {
        self.media = Some(media);
        self
    }

    /// Terminal rows this item needs in the given layout.
    #[must_use]
    pub fn row_height(&self, stacked: bool) -> u16 {
        if self.divider {
            return 1;
        }
        1 + u16::from(stacked && (self.detail.is_some() || self.value.is_some()))
            + u16::from(self.top.is_some())
            + u16::from(self.bottom.is_some())
    }

    fn bands(&self) -> impl Iterator<Item = &ListItemBand> {
        [self.top.as_ref(), self.bottom.as_ref()]
            .into_iter()
            .flatten()
    }

    #[must_use]
    pub fn delete_action(mut self, action: impl Into<String>) -> Self {
        self.delete = Some(action.into());
        self
    }

    /// Declares the idempotent action used to open or select this row.
    #[must_use]
    pub fn activate_action(mut self, action: impl Into<String>) -> Self {
        self.activate = Some(action.into());
        self
    }

    /// Declares a plain command/button row with no accessory affordance.
    #[must_use]
    pub fn command_action(self, action: impl Into<String>) -> Self {
        self.activate_action(action)
    }

    /// Declares a command row whose native and web treatment is destructive.
    #[must_use]
    pub fn destructive_action(mut self, action: impl Into<String>) -> Self {
        self.activate = Some(action.into());
        self.action_role = ListItemActionRole::Destructive;
        self.label_tone = ListItemTone::Danger;
        self
    }

    /// Declares navigation to another App-owned Page. The accessory is only
    /// an affordance; the reducer remains the router and publishes the Page.
    #[must_use]
    pub fn disclosure_action(mut self, action: impl Into<String>) -> Self {
        self.activate = Some(action.into());
        self.accessory = Some(ListItemSlot::Disclosure);
        self
    }

    /// Declares a selection-mode row with one idempotent checkmark action.
    #[must_use]
    pub fn checkmark(mut self, checkmark: Checkmark) -> Self {
        self.accessory = Some(ListItemSlot::Checkmark(checkmark));
        self
    }

    /// Behavior hint consumed by the shared Enter/Space decision table.
    #[must_use]
    pub fn primary_role(&self) -> RowPrimaryRole {
        if self.divider {
            RowPrimaryRole::Static
        } else if self
            .slots()
            .any(|slot| matches!(slot, ListItemSlot::Toggle(_)))
        {
            RowPrimaryRole::Toggle
        } else if self
            .slots()
            .any(|slot| matches!(slot, ListItemSlot::Checkmark(_)))
        {
            RowPrimaryRole::Checkmark
        } else if self
            .slots()
            .any(|slot| matches!(slot, ListItemSlot::Disclosure))
        {
            RowPrimaryRole::Disclosure
        } else if self
            .slots()
            .filter_map(ListItemSlot::as_sparkline)
            .any(|sparkline| sparkline.activate.is_some())
            || self
                .slots()
                .filter_map(ListItemSlot::as_gauge)
                .any(|gauge| gauge.activate.is_some())
        {
            RowPrimaryRole::Command
        } else if self.activate.is_some() {
            match self.action_role {
                ListItemActionRole::Default => RowPrimaryRole::Command,
                ListItemActionRole::Destructive => RowPrimaryRole::Destructive,
            }
        } else {
            RowPrimaryRole::Static
        }
    }

    #[must_use]
    pub fn primary_toggle(&self) -> Option<&Toggle> {
        self.slots().find_map(ListItemSlot::as_toggle)
    }

    #[must_use]
    pub fn primary_checkmark(&self) -> Option<&Checkmark> {
        self.slots().find_map(ListItemSlot::as_checkmark)
    }

    #[must_use]
    pub fn primary_gauge(&self) -> Option<&Gauge> {
        self.slots().find_map(ListItemSlot::as_gauge)
    }

    #[must_use]
    pub fn primary_sparkline(&self) -> Option<&Sparkline> {
        self.slots().find_map(ListItemSlot::as_sparkline)
    }

    /// Builds the authoritative primary action shared by Enter, Space for a
    /// Toggle, and a terminal row click.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn primary_ui_action(&self) -> Option<crate::UiAction> {
        if let Some(toggle) = self.primary_toggle() {
            return Some(crate::UiAction::new(
                toggle.id.clone(),
                toggle.set_value.clone(),
                crate::UiEventKind::Change,
                crate::UiEventValue::Bool(!toggle.value),
            ));
        }
        if let Some(checkmark) = self.primary_checkmark() {
            return Some(crate::UiAction::new(
                checkmark.id.clone(),
                checkmark.set_value.clone(),
                crate::UiEventKind::Change,
                crate::UiEventValue::Bool(!checkmark.value),
            ));
        }
        if let Some(action) = &self.activate {
            return Some(crate::UiAction::activate(self.id.clone(), action.clone()));
        }
        if let Some(sparkline) = self
            .primary_sparkline()
            .filter(|sparkline| sparkline.activate.is_some())
        {
            return sparkline
                .activate
                .clone()
                .map(|action| crate::UiAction::activate(sparkline.id.clone(), action));
        }
        self.primary_gauge()
            .filter(|gauge| gauge.activate.is_some())
            .and_then(|gauge| {
                gauge
                    .activate
                    .clone()
                    .map(|action| crate::UiAction::activate(gauge.id.clone(), action))
            })
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        validate_text(&self.label, MAX_LABEL_BYTES, &format!("{path}.label"))?;
        if self.divider {
            if self.label.contains('\n') {
                return Err(ComponentValidationError::new(
                    format!("{path}.label"),
                    "must be a single line",
                ));
            }
            let passive = self.detail.is_none()
                && self.value.is_none()
                && self.leading.is_none()
                && self.trailing.is_none()
                && self.accessory.is_none()
                && self.top.is_none()
                && self.bottom.is_none()
                && self.media.is_none()
                && self.delete.is_none()
                && self.activate.is_none()
                && !self.busy
                && !self.done;
            if !passive {
                return Err(ComponentValidationError::new(
                    format!("{path}.divider"),
                    "a divider row carries only an optional caption label",
                ));
            }
            return Ok(());
        }
        validate_single_line(&self.label, &format!("{path}.label"))?;
        validate_list_item_text_runs(&self.label_runs, &self.label, &format!("{path}.labelRuns"))?;
        if let Some(detail) = &self.detail {
            validate_text(detail, MAX_LABEL_BYTES, &format!("{path}.detail"))?;
            validate_single_line(detail, &format!("{path}.detail"))?;
        }
        validate_optional_list_item_text_runs(
            &self.detail_runs,
            self.detail.as_deref(),
            &format!("{path}.detailRuns"),
        )?;
        if let Some(value) = &self.value {
            validate_text(value, MAX_SHORT_TEXT_BYTES, &format!("{path}.value"))?;
            validate_single_line(value, &format!("{path}.value"))?;
        }
        validate_optional_list_item_text_runs(
            &self.value_runs,
            self.value.as_deref(),
            &format!("{path}.valueRuns"),
        )?;
        for (name, slot) in [
            ("leading", self.leading.as_ref()),
            ("trailing", self.trailing.as_ref()),
            ("accessory", self.accessory.as_ref()),
        ] {
            if let Some(slot) = slot {
                slot.validate(&format!("{path}.{name}"))?;
            }
        }
        for (name, band) in [("top", self.top.as_ref()), ("bottom", self.bottom.as_ref())] {
            if let Some(band) = band {
                band.validate(&format!("{path}.{name}"))?;
            }
        }
        if let Some(media) = &self.media {
            media.validate(&format!("{path}.media"))?;
        }
        let toggles = self
            .slots()
            .filter_map(ListItemSlot::as_toggle)
            .collect::<Vec<_>>();
        if toggles.len() > 1 {
            return Err(ComponentValidationError::new(
                format!("{path}.slots"),
                "ListItem v1 accepts at most one completion Toggle",
            ));
        }
        let checkmarks = self
            .slots()
            .filter_map(ListItemSlot::as_checkmark)
            .collect::<Vec<_>>();
        let disclosures = self
            .slots()
            .filter(|slot| matches!(slot, ListItemSlot::Disclosure))
            .count();
        if checkmarks.len() > 1 || disclosures > 1 {
            return Err(ComponentValidationError::new(
                format!("{path}.slots"),
                "ListItem accepts at most one checkmark or disclosure accessory",
            ));
        }
        if !checkmarks.is_empty() && !matches!(self.accessory, Some(ListItemSlot::Checkmark(_))) {
            return Err(ComponentValidationError::new(
                format!("{path}.accessory"),
                "Checkmark is accepted only in the accessory slot",
            ));
        }
        if disclosures > 0 && !matches!(self.accessory, Some(ListItemSlot::Disclosure)) {
            return Err(ComponentValidationError::new(
                format!("{path}.accessory"),
                "Disclosure is accepted only in the accessory slot",
            ));
        }
        let sparklines = self
            .slots()
            .filter_map(ListItemSlot::as_sparkline)
            .collect::<Vec<_>>();
        if sparklines.len() > 1
            || (!sparklines.is_empty()
                && !matches!(self.trailing, Some(ListItemSlot::Sparkline(_))))
        {
            return Err(ComponentValidationError::new(
                format!("{path}.trailing"),
                "Sparkline is accepted only once in the trailing slot",
            ));
        }
        let gauges = self
            .slots()
            .filter_map(ListItemSlot::as_gauge)
            .collect::<Vec<_>>();
        if gauges.len() > 1
            || (!gauges.is_empty() && !matches!(self.trailing, Some(ListItemSlot::Gauge(_))))
        {
            return Err(ComponentValidationError::new(
                format!("{path}.trailing"),
                "Gauge is accepted only once in the trailing slot",
            ));
        }
        let independent_roles = usize::from(!toggles.is_empty())
            + usize::from(!checkmarks.is_empty())
            + usize::from(disclosures > 0)
            + usize::from(
                sparklines
                    .iter()
                    .any(|sparkline| sparkline.activate.is_some()),
            )
            + usize::from(gauges.iter().any(|gauge| gauge.activate.is_some()))
            + usize::from(self.activate.is_some() && disclosures == 0);
        if independent_roles > 1 {
            return Err(ComponentValidationError::new(
                format!("{path}.role"),
                "ListItem primary role is ambiguous",
            ));
        }
        if disclosures > 0 && self.activate.is_none() {
            return Err(ComponentValidationError::new(
                format!("{path}.activate"),
                "Disclosure requires an App-owned activate action",
            ));
        }
        if self.action_role == ListItemActionRole::Destructive
            && (self.activate.is_none() || disclosures > 0)
        {
            return Err(ComponentValidationError::new(
                format!("{path}.actionRole"),
                "destructive is accepted only for a plain command row",
            ));
        }
        if let Some(toggle) = toggles.first()
            && toggle.role == ToggleRole::Completion
            && toggle.value != self.done
        {
            return Err(ComponentValidationError::new(
                format!("{path}.done"),
                "done must match the completion Toggle value",
            ));
        }
        if let Some(action) = &self.delete {
            validate_identifier(action, &format!("{path}.delete"))?;
        }
        if let Some(action) = &self.activate {
            validate_identifier(action, &format!("{path}.activate"))?;
        }
        Ok(())
    }

    fn slots(&self) -> impl Iterator<Item = &ListItemSlot> {
        [
            self.leading.as_ref(),
            self.trailing.as_ref(),
            self.accessory.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    fn slots_mut(&mut self) -> impl Iterator<Item = &mut ListItemSlot> {
        [
            self.leading.as_mut(),
            self.trailing.as_mut(),
            self.accessory.as_mut(),
        ]
        .into_iter()
        .flatten()
    }

    fn set_toggle_value(&mut self, id: &str, value: bool) -> bool {
        let mut found = None;
        for slot in self.slots_mut() {
            if let Some(toggle) = slot.toggle_mut(id) {
                toggle.value = value;
                found = Some(toggle.role);
                break;
            }
        }
        if found == Some(ToggleRole::Completion) {
            self.done = value;
        }
        found.is_some()
    }

    fn set_checkmark_value(&mut self, id: &str, value: bool) -> bool {
        for slot in self.slots_mut() {
            if let Some(checkmark) = slot.checkmark_mut(id) {
                checkmark.value = value;
                return true;
            }
        }
        false
    }
}

const fn is_default_list_item_action_role(role: &ListItemActionRole) -> bool {
    matches!(role, ListItemActionRole::Default)
}

/// A keyed collection that contains only [`ListItem`] rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub id: String,
    pub items: Vec<ListItem>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub empty_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub scroll_padding: u16,
    #[serde(
        default = "default_page_overlap",
        skip_serializing_if = "is_default_page_overlap"
    )]
    pub page_overlap: u16,
    #[serde(default, skip_serializing_if = "is_default_page_behavior")]
    pub page_behavior: ListPageBehavior,
    #[serde(default)]
    pub space_pages_down: bool,
    /// Terminal row layout; native and web renderers stack by their own rules.
    #[serde(default, skip_serializing_if = "is_default_row_layout")]
    pub row_layout: ListRowLayout,
    /// One bounded action vocabulary presented for the focused/pointed row.
    /// Renderers return the target row id with the selected menu action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_menu: Option<SemanticMenu>,
}

impl List {
    #[must_use]
    pub fn new(id: impl Into<String>, items: Vec<ListItem>) -> Self {
        Self {
            id: id.into(),
            items,
            empty_message: String::new(),
            selected_id: None,
            select: None,
            scroll_padding: 0,
            page_overlap: default_page_overlap(),
            page_behavior: ListPageBehavior::Selection,
            space_pages_down: false,
            row_layout: ListRowLayout::Inline,
            context_menu: None,
        }
    }

    #[must_use]
    pub const fn row_layout(mut self, layout: ListRowLayout) -> Self {
        self.row_layout = layout;
        self
    }

    /// Terminal rows each item needs at `row_width` under this list's layout.
    #[must_use]
    pub fn row_heights(&self, row_width: u16) -> Vec<u16> {
        let stacked = self.row_layout.stacks_at(row_width);
        self.items
            .iter()
            .map(|item| item.row_height(stacked))
            .collect()
    }

    #[must_use]
    pub fn empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = message.into();
        self
    }

    #[must_use]
    pub fn selected(mut self, item_id: impl Into<String>, action: impl Into<String>) -> Self {
        self.selected_id = Some(item_id.into());
        self.select = Some(action.into());
        self
    }

    #[must_use]
    pub const fn scroll_padding(mut self, rows: u16) -> Self {
        self.scroll_padding = rows;
        self
    }

    #[must_use]
    pub const fn page_overlap(mut self, rows: u16) -> Self {
        self.page_overlap = rows;
        self
    }

    #[must_use]
    pub const fn page_behavior(mut self, behavior: ListPageBehavior) -> Self {
        self.page_behavior = behavior;
        self
    }

    #[must_use]
    pub const fn space_pages_down(mut self, enabled: bool) -> Self {
        self.space_pages_down = enabled;
        self
    }

    #[must_use]
    pub fn context_menu(mut self, menu: SemanticMenu) -> Self {
        self.context_menu = Some(menu);
        self
    }

    /// Renders the same single-line row language used by the sibling Apps.
    #[must_use]
    pub fn widget<'a>(&'a self, state: &'a mut ListState) -> ListWidget<'a> {
        ListWidget {
            list: self,
            state,
            theme: PageTheme::default(),
        }
    }

    /// Interprets one terminal pointer event using the same row-role table as
    /// Enter/Space. The first click selects static rows and immediately invokes
    /// the primary role of interactive rows.
    pub fn pointer_decision(
        &self,
        state: &mut ListState,
        event: &MouseEvent,
    ) -> Option<RowPointerDecision> {
        state.track_mouse(event);
        let position = TerminalPointerState::click_position(event)?;
        let index = state.item_at(position, self.items.len())?;
        if self.items[index].is_divider() {
            return None;
        }
        let changed = state.select(Some(index), self.items.len());
        if self.items[index].primary_role().is_interactive() {
            Some(RowPointerDecision::InvokePrimary(index))
        } else {
            changed.then_some(RowPointerDecision::Select(index))
        }
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn selection_ui_action(&self, index: usize) -> Option<crate::UiAction> {
        let item = self.items.get(index)?;
        let action = self.select.clone()?;
        Some(crate::UiAction::new(
            self.id.clone(),
            action,
            crate::UiEventKind::Change,
            crate::UiEventValue::Text(item.id.clone()),
        ))
    }

    #[cfg(feature = "ui-bridge")]
    pub fn ui_action_for_mouse(
        &self,
        state: &mut ListState,
        event: &MouseEvent,
    ) -> Option<crate::UiAction> {
        match self.pointer_decision(state, event)? {
            RowPointerDecision::Select(index) => self.selection_ui_action(index),
            RowPointerDecision::InvokePrimary(index) => self.items.get(index)?.primary_ui_action(),
        }
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        if self.items.len() > MAX_ITEMS {
            return Err(ComponentValidationError::new(
                format!("{path}.items"),
                format!("List may contain at most {MAX_ITEMS} items"),
            ));
        }
        validate_text(
            &self.empty_message,
            MAX_SHORT_TEXT_BYTES,
            &format!("{path}.emptyMessage"),
        )?;
        if let Some(selected_id) = &self.selected_id
            && !self.items.iter().any(|item| &item.id == selected_id)
        {
            return Err(ComponentValidationError::new(
                format!("{path}.selectedId"),
                "selectedId must identify a ListItem in this List",
            ));
        }
        if let Some(select) = &self.select {
            validate_identifier(select, &format!("{path}.select"))?;
        }
        if let Some(menu) = &self.context_menu {
            menu.validate().map_err(|error| {
                ComponentValidationError::new(
                    format!(
                        "{path}.contextMenu.{}",
                        error.path.trim_start_matches("menu.")
                    ),
                    error.message,
                )
            })?;
        }
        for (index, item) in self.items.iter().enumerate() {
            item.validate(&format!("{path}.items[{index}]"))?;
        }
        Ok(())
    }

    pub(crate) fn insert(
        &mut self,
        index: usize,
        item: ListItem,
    ) -> Result<(), ComponentValidationError> {
        if index > self.items.len() {
            return Err(ComponentValidationError::new(
                "delta.index",
                "List insertion index is outside the collection",
            ));
        }
        self.items.insert(index, item);
        Ok(())
    }

    pub(crate) fn remove(&mut self, item_id: &str) -> Result<(), ComponentValidationError> {
        let Some(index) = self.items.iter().position(|item| item.id == item_id) else {
            return Err(ComponentValidationError::new(
                "delta.itemId",
                format!("ListItem {item_id:?} is not present"),
            ));
        };
        self.items.remove(index);
        Ok(())
    }
}

const fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}

const fn default_page_overlap() -> u16 {
    1
}

const fn is_default_page_overlap(value: &u16) -> bool {
    *value == default_page_overlap()
}

const fn is_default_row_layout(value: &ListRowLayout) -> bool {
    matches!(value, ListRowLayout::Inline)
}

const fn is_default_page_behavior(value: &ListPageBehavior) -> bool {
    matches!(value, ListPageBehavior::Selection)
}

/// Single-line input placed in a Page's named header slot.
///
/// `setValue` is optional because submit-only inputs may keep an uncommitted
/// draft locally and send it with the submit event. This avoids a network
/// round trip for every keystroke while the Rust App remains authoritative for
/// committed state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit: Option<String>,
}

impl Input {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: String::new(),
            placeholder: String::new(),
            set_value: None,
            submit: None,
        }
    }

    #[must_use]
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    #[must_use]
    pub fn set_value_action(mut self, action: impl Into<String>) -> Self {
        self.set_value = Some(action.into());
        self
    }

    #[must_use]
    pub fn submit_action(mut self, action: impl Into<String>) -> Self {
        self.submit = Some(action.into());
        self
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        validate_text(&self.label, MAX_SHORT_TEXT_BYTES, &format!("{path}.label"))?;
        validate_text(&self.value, MAX_INPUT_BYTES, &format!("{path}.value"))?;
        validate_text(
            &self.placeholder,
            MAX_SHORT_TEXT_BYTES,
            &format!("{path}.placeholder"),
        )?;
        if let Some(action) = &self.set_value {
            validate_identifier(action, &format!("{path}.setValue"))?;
        }
        if let Some(action) = &self.submit {
            validate_identifier(action, &format!("{path}.submit"))?;
        }
        Ok(())
    }
}

/// Controls currently accepted by a Page's header region.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PageHeaderSlot {
    Input(Input),
}

impl PageHeaderSlot {
    #[must_use]
    pub const fn input(input: Input) -> Self {
        Self::Input(input)
    }

    #[must_use]
    pub const fn as_input(&self) -> &Input {
        match self {
            Self::Input(input) => input,
        }
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        match self {
            Self::Input(input) => input.validate(path),
        }
    }
}

/// Containers currently accepted by a Page's body region.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PageBodySlot {
    List(List),
    Content(Content),
    Sparkline(Sparkline),
    BarChart(BarChart),
    LineChart(LineChart),
    Gauge(Gauge),
}

impl PageBodySlot {
    #[must_use]
    pub const fn list(list: List) -> Self {
        Self::List(list)
    }

    #[must_use]
    pub const fn content(content: Content) -> Self {
        Self::Content(content)
    }

    #[must_use]
    pub const fn sparkline(sparkline: Sparkline) -> Self {
        Self::Sparkline(sparkline)
    }

    #[must_use]
    pub const fn bar_chart(chart: BarChart) -> Self {
        Self::BarChart(chart)
    }

    #[must_use]
    pub const fn line_chart(chart: LineChart) -> Self {
        Self::LineChart(chart)
    }

    #[must_use]
    pub const fn gauge(gauge: Gauge) -> Self {
        Self::Gauge(gauge)
    }

    #[must_use]
    pub const fn as_list(&self) -> &List {
        match self {
            Self::List(list) => list,
            Self::Content(_)
            | Self::Sparkline(_)
            | Self::BarChart(_)
            | Self::LineChart(_)
            | Self::Gauge(_) => panic!("Page body is not List"),
        }
    }

    fn as_list_mut(&mut self) -> Option<&mut List> {
        match self {
            Self::List(list) => Some(list),
            Self::Content(_)
            | Self::Sparkline(_)
            | Self::BarChart(_)
            | Self::LineChart(_)
            | Self::Gauge(_) => None,
        }
    }

    #[must_use]
    pub const fn as_content(&self) -> Option<&Content> {
        match self {
            Self::Content(content) => Some(content),
            Self::List(_)
            | Self::Sparkline(_)
            | Self::BarChart(_)
            | Self::LineChart(_)
            | Self::Gauge(_) => None,
        }
    }

    fn as_content_mut(&mut self) -> Option<&mut Content> {
        match self {
            Self::Content(content) => Some(content),
            Self::List(_)
            | Self::Sparkline(_)
            | Self::BarChart(_)
            | Self::LineChart(_)
            | Self::Gauge(_) => None,
        }
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        match self {
            Self::List(list) => list.validate(path),
            Self::Content(content) => content.validate(path),
            Self::Sparkline(sparkline) => sparkline.validate(path),
            Self::BarChart(chart) => chart.validate(path),
            Self::LineChart(chart) => chart.validate(path),
            Self::Gauge(gauge) => gauge.validate(path),
        }
    }
}

/// Top-level data/document container with named, constrained regions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<PageHeaderSlot>,
    pub body: PageBodySlot,
    #[serde(default, skip_serializing_if = "FooterActions::is_empty")]
    pub footer: FooterActions,
}

impl Page {
    #[must_use]
    pub fn new(title: impl Into<String>, list: List) -> Self {
        Self {
            title: title.into(),
            back: None,
            header: None,
            body: PageBodySlot::List(list),
            footer: FooterActions::default(),
        }
    }

    /// Creates a document/detail Page whose body is deliberately read-only.
    #[must_use]
    pub fn with_content(title: impl Into<String>, content: Content) -> Self {
        Self {
            title: title.into(),
            back: None,
            header: None,
            body: PageBodySlot::Content(content),
            footer: FooterActions::default(),
        }
    }

    #[must_use]
    pub fn with_sparkline(title: impl Into<String>, sparkline: Sparkline) -> Self {
        Self {
            title: title.into(),
            back: None,
            header: None,
            body: PageBodySlot::Sparkline(sparkline),
            footer: FooterActions::default(),
        }
    }

    #[must_use]
    pub fn with_bar_chart(title: impl Into<String>, chart: BarChart) -> Self {
        Self {
            title: title.into(),
            back: None,
            header: None,
            body: PageBodySlot::BarChart(chart),
            footer: FooterActions::default(),
        }
    }

    #[must_use]
    pub fn with_line_chart(title: impl Into<String>, chart: LineChart) -> Self {
        Self {
            title: title.into(),
            back: None,
            header: None,
            body: PageBodySlot::LineChart(chart),
            footer: FooterActions::default(),
        }
    }

    #[must_use]
    pub fn with_gauge(title: impl Into<String>, gauge: Gauge) -> Self {
        Self {
            title: title.into(),
            back: None,
            header: None,
            body: PageBodySlot::Gauge(gauge),
            footer: FooterActions::default(),
        }
    }

    #[must_use]
    pub fn input(mut self, input: Input) -> Self {
        self.header = Some(PageHeaderSlot::Input(input));
        self
    }

    /// Declares a Page-level action that returns to the previous semantic view.
    #[must_use]
    pub fn back_action(mut self, action: impl Into<String>) -> Self {
        self.back = Some(action.into());
        self
    }

    /// Installs the ordered screen-level action slot.
    #[must_use]
    pub fn footer_actions(mut self, actions: impl IntoIterator<Item = FooterAction>) -> Self {
        self.footer = FooterActions::new(actions);
        self
    }

    #[must_use]
    pub const fn list(&self) -> &List {
        self.body.as_list()
    }

    #[must_use]
    pub const fn content(&self) -> Option<&Content> {
        self.body.as_content()
    }

    #[must_use]
    pub const fn chart_id(&self) -> Option<&str> {
        match &self.body {
            PageBodySlot::Sparkline(chart) => Some(chart.id.as_str()),
            PageBodySlot::BarChart(chart) => Some(chart.id.as_str()),
            PageBodySlot::LineChart(chart) => Some(chart.id.as_str()),
            PageBodySlot::Gauge(chart) => Some(chart.id.as_str()),
            PageBodySlot::List(_) | PageBodySlot::Content(_) => None,
        }
    }

    #[must_use]
    pub fn input_spec(&self) -> Option<&Input> {
        self.header.as_ref().map(PageHeaderSlot::as_input)
    }

    /// Capabilities a renderer needs for this exact closed tree.
    #[must_use]
    pub fn required_capabilities(&self) -> Vec<&'static str> {
        let mut capabilities = vec![PAGE_COMPONENT_CAPABILITY];
        if !self.footer.is_empty() {
            capabilities.push(FOOTER_ACTIONS_CAPABILITY);
        }
        let chart_capability = match &self.body {
            PageBodySlot::Sparkline(_) => Some(crate::SPARKLINE_COMPONENT_CAPABILITY),
            PageBodySlot::BarChart(_) => Some(crate::BAR_CHART_COMPONENT_CAPABILITY),
            PageBodySlot::LineChart(_) => Some(crate::LINE_CHART_COMPONENT_CAPABILITY),
            PageBodySlot::Gauge(_) => Some(crate::GAUGE_COMPONENT_CAPABILITY),
            PageBodySlot::List(_) | PageBodySlot::Content(_) => None,
        };
        if let Some(capability) = chart_capability {
            capabilities.push(capability);
            if self.header.is_some() {
                capabilities.push(INPUT_COMPONENT_CAPABILITY);
            }
            if self.back.is_some() {
                capabilities.push(PAGE_BACK_CAPABILITY);
            }
            return capabilities;
        }
        let PageBodySlot::List(list) = &self.body else {
            capabilities.push(crate::CONTENT_COMPONENT_CAPABILITY);
            if self.header.is_some() {
                capabilities.push(INPUT_COMPONENT_CAPABILITY);
            }
            if self.back.is_some() {
                capabilities.push(PAGE_BACK_CAPABILITY);
            }
            if let PageBodySlot::Content(content) = &self.body {
                if content.selection.is_some() || content.select.is_some() {
                    capabilities.push(crate::CONTENT_SELECTION_CAPABILITY);
                }
                if content.context_menu.is_some() {
                    capabilities.extend([
                        crate::MENU_COMPONENT_CAPABILITY,
                        crate::MENU_ANCHOR_CAPABILITY,
                    ]);
                }
            }
            return capabilities;
        };
        capabilities.extend([LIST_COMPONENT_CAPABILITY, LIST_ITEM_COMPONENT_CAPABILITY]);
        if self.header.is_some() {
            capabilities.push(INPUT_COMPONENT_CAPABILITY);
        }
        if self.back.is_some() {
            capabilities.push(PAGE_BACK_CAPABILITY);
        }
        if list
            .items
            .iter()
            .any(|item| item.detail.is_some() || item.value.is_some())
        {
            capabilities.push(LIST_ITEM_METADATA_CAPABILITY);
        }
        if list.items.iter().any(|item| item.activate.is_some()) {
            capabilities.push(LIST_ITEM_ACTIVATE_CAPABILITY);
        }
        if list
            .items
            .iter()
            .any(|item| item.primary_role().is_interactive())
        {
            capabilities.push(LIST_ITEM_ROLE_CAPABILITY);
        }
        if list.items.iter().any(|item| {
            item.slots()
                .any(|slot| matches!(slot, ListItemSlot::Toggle(_)))
        }) {
            capabilities.push(TOGGLE_COMPONENT_CAPABILITY);
        }
        if list.items.iter().any(|item| {
            item.busy
                || item.label_tone != ListItemTone::Default
                || item.value_tone != ListItemTone::Muted
                || item.emphasis != ListItemEmphasis::Regular
                || item.value_min_width.is_some()
                || item
                    .slots()
                    .any(|slot| matches!(slot, ListItemSlot::Status(_) | ListItemSlot::Badge(_)))
        }) {
            capabilities.push(LIST_ITEM_PRESENTATION_CAPABILITY);
        }
        if list.items.iter().any(|item| {
            !item.label_runs.is_empty()
                || !item.detail_runs.is_empty()
                || !item.value_runs.is_empty()
        }) {
            capabilities.push(LIST_ITEM_STYLED_TEXT_CAPABILITY);
        }
        if list.items.iter().any(|item| {
            item.slots()
                .any(|slot| matches!(slot, ListItemSlot::Status(_)))
        }) {
            capabilities.push(STATUS_SYMBOL_COMPONENT_CAPABILITY);
        }
        if list.items.iter().any(|item| {
            item.slots()
                .any(|slot| matches!(slot, ListItemSlot::Badge(_)))
        }) {
            capabilities.push(BADGE_COMPONENT_CAPABILITY);
        }
        if list.items.iter().any(|item| {
            item.slots()
                .any(|slot| matches!(slot, ListItemSlot::Sparkline(_)))
        }) {
            capabilities.push(crate::SPARKLINE_COMPONENT_CAPABILITY);
        }
        if list.items.iter().any(|item| {
            item.slots()
                .any(|slot| matches!(slot, ListItemSlot::Gauge(_)))
        }) {
            capabilities.push(crate::GAUGE_COMPONENT_CAPABILITY);
        }
        if list.selected_id.is_some()
            || list.select.is_some()
            || list.scroll_padding != 0
            || list.page_overlap != default_page_overlap()
            || list.page_behavior != ListPageBehavior::Selection
            || list.space_pages_down
        {
            capabilities.push(LIST_SELECTION_CAPABILITY);
        }
        if list.context_menu.is_some() {
            capabilities.extend([
                crate::MENU_COMPONENT_CAPABILITY,
                crate::MENU_ANCHOR_CAPABILITY,
            ]);
        }
        capabilities
    }

    /// Validates the closed tree, including globally unique nested ids.
    pub fn validate(&self) -> Result<(), ComponentValidationError> {
        validate_text(&self.title, MAX_SHORT_TEXT_BYTES, "page.title")?;
        if let Some(back) = &self.back {
            validate_identifier(back, "page.back")?;
        }
        if let Some(header) = &self.header {
            header.validate("page.header")?;
        }
        self.body.validate("page.body")?;
        self.footer.validate("page.footer")?;

        let mut ids = HashSet::new();
        if let Some(input) = self.input_spec() {
            register_unique(&mut ids, &input.id, "page.header.id")?;
        }
        match &self.body {
            PageBodySlot::List(list) => {
                register_unique(&mut ids, &list.id, "page.body.id")?;
                for (index, item) in list.items.iter().enumerate() {
                    register_unique(&mut ids, &item.id, &format!("page.body.items[{index}].id"))?;
                    for slot in item.slots() {
                        if let Some(id) = slot.id() {
                            register_unique(
                                &mut ids,
                                id,
                                &format!("page.body.items[{index}].slot.id"),
                            )?;
                        }
                    }
                    for band in item.bands() {
                        if let Some(id) = band.id() {
                            register_unique(
                                &mut ids,
                                id,
                                &format!("page.body.items[{index}].band.id"),
                            )?;
                        }
                    }
                }
            }
            PageBodySlot::Content(content) => {
                register_unique(&mut ids, &content.id, "page.body.id")?;
                for (index, line) in content.lines.iter().enumerate() {
                    register_unique(&mut ids, &line.id, &format!("page.body.lines[{index}].id"))?;
                }
            }
            PageBodySlot::Sparkline(chart) => {
                register_unique(&mut ids, &chart.id, "page.body.id")?;
            }
            PageBodySlot::BarChart(chart) => {
                register_unique(&mut ids, &chart.id, "page.body.id")?;
            }
            PageBodySlot::LineChart(chart) => {
                register_unique(&mut ids, &chart.id, "page.body.id")?;
            }
            PageBodySlot::Gauge(chart) => {
                register_unique(&mut ids, &chart.id, "page.body.id")?;
            }
        }
        for (index, action) in self.footer.actions.iter().enumerate() {
            register_unique(
                &mut ids,
                &action.id,
                &format!("page.footer.actions[{index}].id"),
            )?;
        }
        Ok(())
    }

    /// Uses App Kit's single-line List renderer and InputField named slots.
    #[must_use]
    /// Applies one shared key action with the back row as a focusable stop.
    ///
    /// With a back action, Up from the first row (or from an unselected
    /// list) moves focus to the title row, which paints like a selected row;
    /// Enter or Escape there returns [`ListNavigationOutcome::Back`] and Down
    /// returns to the first row. Pages without a back action delegate to
    /// [`RowNavigationState::navigate`] unchanged.
    pub fn navigate(
        &self,
        state: &mut ListState,
        action: ListNavigationAction,
    ) -> ListNavigationOutcome {
        let item_count = self.list_len();
        if self.back.is_none() {
            state.set_back_focused(false);
            return state.navigate(action, item_count);
        }
        if state.back_focused() {
            return match action {
                ListNavigationAction::Activate | ListNavigationAction::Back => {
                    ListNavigationOutcome::Back
                }
                ListNavigationAction::Up
                | ListNavigationAction::First
                | ListNavigationAction::PageUp => ListNavigationOutcome::None,
                ListNavigationAction::Down
                | ListNavigationAction::Last
                | ListNavigationAction::PageDown => {
                    state.set_back_focused(false);
                    let target = if action == ListNavigationAction::Down {
                        ListNavigationAction::First
                    } else {
                        action
                    };
                    state.select(None, item_count);
                    state.navigate(target, item_count)
                }
            };
        }
        let at_top = state.offset() == 0
            && state
                .selected()
                .is_none_or(|selected| (0..selected).all(|index| !state.is_selectable(index)));
        if action == ListNavigationAction::Up && at_top {
            state.set_back_focused(true);
            state.select(None, item_count);
            return ListNavigationOutcome::FocusedBack;
        }
        state.navigate(action, item_count)
    }

    fn list_len(&self) -> usize {
        match &self.body {
            PageBodySlot::List(list) => list.items.len(),
            _ => 0,
        }
    }

    pub fn widget<'a>(
        &'a self,
        input: &'a mut InputField,
        list_state: &'a mut ListState,
    ) -> PageWidget<'a> {
        PageWidget {
            page: self,
            input,
            list_state,
            content_state: PageContentState::Owned(ContentState::new()),
            theme: PageTheme::default(),
            input_theme: None,
            content_theme: ContentTheme::default(),
        }
    }

    /// Uses the exact Page interpreter while preserving renderer-local Content
    /// scroll state between frames.
    ///
    /// Apps with a Content body must use this instead of recreating the Page
    /// header and document painter around their own scroll implementation.
    #[must_use]
    pub fn widget_with_content_state<'a>(
        &'a self,
        input: &'a mut InputField,
        list_state: &'a mut ListState,
        content_state: &'a mut ContentState,
    ) -> PageWidget<'a> {
        PageWidget {
            page: self,
            input,
            list_state,
            content_state: PageContentState::Borrowed(content_state),
            theme: PageTheme::default(),
            input_theme: None,
            content_theme: ContentTheme::default(),
        }
    }

    /// Resolves the terminal rectangles used by the Page's named slots.
    ///
    /// Apps use this for target-aware mouse input without duplicating the
    /// component's layout math. The returned List rectangle maps one terminal
    /// row to one ListItem in v1.
    #[must_use]
    pub fn layout(&self, area: Rect) -> PageLayout {
        let footer_height = u16::from(!self.footer.is_empty() && area.height > 0);
        let content_area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(footer_height),
        );
        let footer = (footer_height > 0).then(|| {
            Rect::new(
                area.x,
                area.bottom().saturating_sub(footer_height),
                area.width,
                footer_height,
            )
        });
        let constraints = if self.input_spec().is_some() {
            vec![
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ]
        } else {
            vec![Constraint::Length(2), Constraint::Min(0)]
        };
        let slots = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(content_area);
        if self.input_spec().is_some() {
            PageLayout {
                title: slots[0],
                input: Some(slots[1]),
                list: slots[3],
                footer,
            }
        } else {
            PageLayout {
                title: slots[0],
                input: None,
                list: slots[1],
                footer,
            }
        }
    }

    /// Resolves one terminal click from the exact Page layout and closed
    /// component vocabulary. This remains available in pure-TUI builds.
    pub fn pointer_decision<'a>(
        &'a self,
        list_state: &mut ListState,
        event: &MouseEvent,
        area: Rect,
    ) -> Option<PagePointerDecision<'a>> {
        list_state.track_mouse(event);
        let position = TerminalPointerState::click_position(event)?;
        let layout = self.layout(area);
        if let Some(footer) = layout.footer
            && let Some(action) = self.footer.action_for_mouse(event, footer)
        {
            return Some(PagePointerDecision::Footer(action));
        }
        if layout.title.contains(position)
            && let Some(back) = &self.back
        {
            return Some(PagePointerDecision::Back(back));
        }
        if !layout.list.contains(position) {
            return None;
        }
        match &self.body {
            PageBodySlot::List(list) => list
                .pointer_decision(list_state, event)
                .map(PagePointerDecision::List),
            PageBodySlot::Sparkline(chart) => {
                chart.action_for_mouse(event, layout.list).map(|action| {
                    PagePointerDecision::Activate {
                        node_id: chart.id.as_str(),
                        action,
                    }
                })
            }
            PageBodySlot::BarChart(chart) => {
                chart.action_for_mouse(event, layout.list).map(|action| {
                    PagePointerDecision::Activate {
                        node_id: chart.id.as_str(),
                        action,
                    }
                })
            }
            PageBodySlot::LineChart(chart) => {
                chart.action_for_mouse(event, layout.list).map(|action| {
                    PagePointerDecision::Activate {
                        node_id: chart.id.as_str(),
                        action,
                    }
                })
            }
            PageBodySlot::Gauge(chart) => {
                chart.action_for_mouse(event, layout.list).map(|action| {
                    PagePointerDecision::Activate {
                        node_id: chart.id.as_str(),
                        action,
                    }
                })
            }
            PageBodySlot::Content(_) => None,
        }
    }

    /// Converts the standalone terminal decision to the identical typed
    /// action emitted by Swift and web.
    #[cfg(feature = "ui-bridge")]
    pub fn ui_action_for_mouse(
        &self,
        node_id: impl Into<crate::NodeId>,
        list_state: &mut ListState,
        event: &MouseEvent,
        area: Rect,
    ) -> Option<crate::UiAction> {
        match self.pointer_decision(list_state, event, area)? {
            PagePointerDecision::Footer(action) => Some(action.ui_action()),
            PagePointerDecision::Back(action) => Some(crate::UiAction::new(
                node_id,
                action.to_owned(),
                crate::UiEventKind::Cancel,
                crate::UiEventValue::None,
            )),
            PagePointerDecision::List(RowPointerDecision::Select(index)) => {
                self.list().selection_ui_action(index)
            }
            PagePointerDecision::List(RowPointerDecision::InvokePrimary(index)) => {
                self.list().items.get(index)?.primary_ui_action()
            }
            PagePointerDecision::Activate { node_id, action } => {
                Some(crate::UiAction::activate(node_id, action))
            }
        }
    }

    pub(crate) fn set_input_value(
        &mut self,
        input_id: &str,
        value: String,
    ) -> Result<(), ComponentValidationError> {
        let Some(PageHeaderSlot::Input(input)) = &mut self.header else {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                "Page has no Input header",
            ));
        };
        if input.id != input_id {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                format!("Input {input_id:?} is not present"),
            ));
        }
        input.value = value;
        Ok(())
    }

    pub(crate) fn set_toggle_value(
        &mut self,
        toggle_id: &str,
        value: bool,
    ) -> Result<(), ComponentValidationError> {
        let Some(list) = self.body.as_list_mut() else {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                "Page body is not a List",
            ));
        };
        for item in &mut list.items {
            if item.set_toggle_value(toggle_id, value) {
                return Ok(());
            }
        }
        Err(ComponentValidationError::new(
            "delta.nodeId",
            format!("Toggle {toggle_id:?} is not present"),
        ))
    }

    pub(crate) fn set_checkmark_value(
        &mut self,
        checkmark_id: &str,
        value: bool,
    ) -> Result<(), ComponentValidationError> {
        let Some(list) = self.body.as_list_mut() else {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                "Page body is not a List",
            ));
        };
        for item in &mut list.items {
            if item.set_checkmark_value(checkmark_id, value) {
                return Ok(());
            }
        }
        Err(ComponentValidationError::new(
            "delta.nodeId",
            format!("Checkmark {checkmark_id:?} is not present"),
        ))
    }

    pub(crate) fn set_sparkline_data(
        &mut self,
        replacement: Sparkline,
    ) -> Result<(), ComponentValidationError> {
        if let PageBodySlot::Sparkline(sparkline) = &mut self.body
            && sparkline.id == replacement.id
        {
            sparkline.replace_data_from(replacement);
            return Ok(());
        }
        let Some(list) = self.body.as_list_mut() else {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                "Page body is not a List",
            ));
        };
        for item in &mut list.items {
            for slot in item.slots_mut() {
                if let ListItemSlot::Sparkline(sparkline) = slot
                    && sparkline.id == replacement.id
                {
                    sparkline.replace_data_from(replacement);
                    return Ok(());
                }
            }
        }
        Err(ComponentValidationError::new(
            "delta.nodeId",
            format!("Sparkline {:?} is not present", replacement.id),
        ))
    }

    pub(crate) fn set_bar_chart_data(
        &mut self,
        replacement: BarChart,
    ) -> Result<(), ComponentValidationError> {
        let PageBodySlot::BarChart(chart) = &mut self.body else {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                "Page body is not BarChart",
            ));
        };
        if chart.id != replacement.id {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                format!("BarChart {:?} is not present", replacement.id),
            ));
        }
        chart.replace_data_from(replacement);
        Ok(())
    }

    pub(crate) fn set_line_chart_data(
        &mut self,
        replacement: LineChart,
    ) -> Result<(), ComponentValidationError> {
        let PageBodySlot::LineChart(chart) = &mut self.body else {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                "Page body is not LineChart",
            ));
        };
        if chart.id != replacement.id {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                format!("LineChart {:?} is not present", replacement.id),
            ));
        }
        chart.replace_data_from(replacement);
        Ok(())
    }

    pub(crate) fn set_gauge_data(
        &mut self,
        replacement: Gauge,
    ) -> Result<(), ComponentValidationError> {
        if let PageBodySlot::Gauge(gauge) = &mut self.body
            && gauge.id == replacement.id
        {
            gauge.replace_data_from(replacement);
            return Ok(());
        }
        let Some(list) = self.body.as_list_mut() else {
            return Err(ComponentValidationError::new(
                "delta.nodeId",
                "Page body is neither Gauge nor List",
            ));
        };
        for item in &mut list.items {
            for slot in item.slots_mut() {
                if let ListItemSlot::Gauge(gauge) = slot
                    && gauge.id == replacement.id
                {
                    gauge.replace_data_from(replacement);
                    return Ok(());
                }
            }
        }
        Err(ComponentValidationError::new(
            "delta.nodeId",
            format!("Gauge {:?} is not present", replacement.id),
        ))
    }

    pub(crate) fn insert_list_item(
        &mut self,
        list_id: &str,
        index: usize,
        item: ListItem,
    ) -> Result<(), ComponentValidationError> {
        let Some(list) = self.body.as_list_mut() else {
            return Err(ComponentValidationError::new(
                "delta.listId",
                "Page body is not a List",
            ));
        };
        if list.id != list_id {
            return Err(ComponentValidationError::new(
                "delta.listId",
                format!("List {list_id:?} is not present"),
            ));
        }
        list.insert(index, item)
    }

    pub(crate) fn set_list_selection(
        &mut self,
        list_id: &str,
        selected_id: Option<String>,
    ) -> Result<(), ComponentValidationError> {
        let Some(list) = self.body.as_list_mut() else {
            return Err(ComponentValidationError::new(
                "delta.listId",
                "Page body is not a List",
            ));
        };
        if list.id != list_id {
            return Err(ComponentValidationError::new(
                "delta.listId",
                format!("List {list_id:?} is not present"),
            ));
        }
        if let Some(selected_id) = &selected_id
            && !list.items.iter().any(|item| &item.id == selected_id)
        {
            return Err(ComponentValidationError::new(
                "delta.selectedId",
                format!("ListItem {selected_id:?} is not present"),
            ));
        }
        list.selected_id = selected_id;
        Ok(())
    }

    pub(crate) fn remove_list_item(
        &mut self,
        list_id: &str,
        item_id: &str,
    ) -> Result<(), ComponentValidationError> {
        let Some(list) = self.body.as_list_mut() else {
            return Err(ComponentValidationError::new(
                "delta.listId",
                "Page body is not a List",
            ));
        };
        if list.id != list_id {
            return Err(ComponentValidationError::new(
                "delta.listId",
                format!("List {list_id:?} is not present"),
            ));
        }
        list.remove(item_id)
    }

    pub(crate) fn set_content_selection(
        &mut self,
        content_id: &str,
        selection: Option<crate::ContentSelection>,
    ) -> Result<(), ComponentValidationError> {
        let Some(content) = self.body.as_content_mut() else {
            return Err(ComponentValidationError::new(
                "delta.contentId",
                "Page body is not Content",
            ));
        };
        if content.id != content_id {
            return Err(ComponentValidationError::new(
                "delta.contentId",
                format!("Content {content_id:?} is not present"),
            ));
        }
        content.set_selection(selection)
    }

    pub(crate) fn splice_content_lines(
        &mut self,
        content_id: &str,
        index: usize,
        delete_count: usize,
        lines: Vec<crate::ContentLine>,
    ) -> Result<(), ComponentValidationError> {
        let Some(content) = self.body.as_content_mut() else {
            return Err(ComponentValidationError::new(
                "delta.contentId",
                "Page body is not Content",
            ));
        };
        if content.id != content_id {
            return Err(ComponentValidationError::new(
                "delta.contentId",
                format!("Content {content_id:?} is not present"),
            ));
        }
        content.splice_lines(index, delete_count, lines)
    }
}

/// Borderless terminal presentation for [`Page`] and its current slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTheme {
    pub style: Style,
    pub title: Style,
    pub item: Style,
    pub detail: Style,
    pub value: Style,
    pub accent: Style,
    pub info: Style,
    pub success: Style,
    pub warning: Style,
    pub danger: Style,
    pub done: Style,
    pub toggle: Style,
    pub badge: Style,
    pub busy: Style,
    pub selected_busy: Style,
    pub delete: Style,
    pub empty: Style,
    pub selected: Style,
    /// Pointer hover on an unselected row; distinct from `selected`.
    pub hovered: Style,
    pub selected_item: Style,
    pub selected_detail: Style,
    pub selected_value: Style,
    pub selected_badge: Style,
    pub navigation: Style,
    /// Thin rule used by `ListItemBand::Divider`; muted in light and dark.
    pub divider: Style,
    pub scrollbar_track: Style,
    pub scrollbar_thumb: Style,
    pub left_padding: u16,
    /// Optional inactive styling for the compatibility padding cells before
    /// the semantic row content.
    pub left_padding_style: Style,
    /// Legacy terminal rows may opt out; new List rows retain one cell.
    pub right_padding: u16,
    /// Whether a trailing value's separating cell inherits the value style.
    /// Existing rows that historically styled only glyph cells may opt out.
    pub style_value_gap: bool,
    /// Compatibility for rows whose colored status span historically owned
    /// its following spacer cells. New rows keep those cells neutral.
    pub style_status_spacing: bool,
}

/// Terminal hit-test geometry for a rendered [`Page`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageLayout {
    pub title: Rect,
    pub input: Option<Rect>,
    pub list: Rect,
    pub footer: Option<Rect>,
}

/// Renderer-neutral meaning of one terminal click inside a Page. Apps that
/// compile without `ui-bridge` can reduce this directly; hosted Apps convert
/// it to the same typed `UiAction` used by Swift and web.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagePointerDecision<'a> {
    Footer(&'a FooterAction),
    Back(&'a str),
    List(RowPointerDecision),
    Activate { node_id: &'a str, action: &'a str },
}

impl PageTheme {
    #[must_use]
    pub const fn for_theme(theme: KitTheme) -> Self {
        Self {
            style: Style::new(),
            title: Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
            item: Style::new().fg(theme.text),
            detail: Style::new().fg(theme.muted),
            value: Style::new().fg(theme.muted),
            accent: Style::new().fg(theme.accent),
            info: Style::new().fg(match theme.scheme {
                crate::ColorScheme::Dark => ratatui::style::Color::LightBlue,
                crate::ColorScheme::Light => ratatui::style::Color::Blue,
            }),
            success: Style::new().fg(match theme.scheme {
                crate::ColorScheme::Dark => ratatui::style::Color::LightGreen,
                crate::ColorScheme::Light => ratatui::style::Color::Green,
            }),
            warning: Style::new().fg(match theme.scheme {
                crate::ColorScheme::Dark => ratatui::style::Color::LightYellow,
                crate::ColorScheme::Light => ratatui::style::Color::Yellow,
            }),
            danger: Style::new().fg(theme.danger),
            done: Style::new().fg(theme.muted),
            toggle: Style::new().fg(theme.accent),
            badge: Style::new().fg(theme.muted),
            busy: Style::new().fg(theme.accent),
            selected_busy: Style::new(),
            delete: Style::new().fg(theme.subtle),
            empty: Style::new().fg(theme.subtle),
            selected: theme.selected_row,
            hovered: theme.hovered_row,
            selected_item: Style::new(),
            selected_detail: Style::new().add_modifier(Modifier::DIM),
            selected_value: Style::new(),
            selected_badge: Style::new().add_modifier(Modifier::DIM),
            navigation: Style::new().fg(theme.subtle),
            divider: Style::new().fg(theme.subtle).add_modifier(Modifier::DIM),
            scrollbar_track: theme.scrollbar_track,
            scrollbar_thumb: theme.scrollbar_thumb,
            left_padding: SELECTABLE_LEFT_PADDING,
            left_padding_style: Style::new(),
            right_padding: 1,
            style_value_gap: true,
            style_status_spacing: false,
        }
    }

    #[must_use]
    pub fn detected() -> Self {
        Self::for_theme(KitTheme::detected())
    }

    /// Input styling derived from the same terminal design tokens as this
    /// Page. A Page owns the visual treatment of its named Input slot; the
    /// renderer-local editing state must not retain an unrelated palette.
    #[must_use]
    pub const fn input_theme(self) -> InputFieldTheme {
        InputFieldTheme {
            style: self.style,
            text: self.item,
            focused: self.item.add_modifier(Modifier::BOLD),
            placeholder: self.empty,
            prompt: self.detail,
            selection: self.selected,
            left_padding: self.left_padding,
        }
    }

    fn inset_body(self, area: Rect) -> Rect {
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

    fn tone(self, tone: ListItemTone) -> Style {
        match tone {
            ListItemTone::Default => self.item,
            ListItemTone::Muted => self.value,
            ListItemTone::Accent => self.accent,
            ListItemTone::Info => self.info,
            ListItemTone::Success => self.success,
            ListItemTone::Warning => self.warning,
            ListItemTone::Danger => self.danger,
        }
    }
}

impl Default for PageTheme {
    fn default() -> Self {
        Self::for_theme(KitTheme::dark())
    }
}

/// Standalone single-line List renderer built from App Kit row primitives.
pub struct ListWidget<'a> {
    list: &'a List,
    state: &'a mut ListState,
    theme: PageTheme,
}

impl ListWidget<'_> {
    #[must_use]
    pub const fn theme(mut self, theme: PageTheme) -> Self {
        self.theme = theme;
        self
    }
}

impl Widget for ListWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            self.state.prepare(area, self.list.items.len());
            return;
        }
        let item_count = self.list.items.len();
        self.state.set_navigation(
            usize::from(self.list.scroll_padding),
            usize::from(self.list.page_overlap),
            self.list.page_behavior,
        );
        if let Some(selected_id) = &self.list.selected_id {
            let selected = self
                .list
                .items
                .iter()
                .position(|item| &item.id == selected_id);
            self.state.select(selected, item_count);
        }
        let has_dividers = self.list.items.iter().any(ListItem::is_divider);
        let uniform = !has_dividers
            && self.list.row_layout == ListRowLayout::Inline
            && self
                .list
                .items
                .iter()
                .all(|item| item.top.is_none() && item.bottom.is_none());
        let heights = if uniform {
            Vec::new()
        } else {
            // Heights depend on the width; the scrollbar column changes it by
            // one cell, which only matters for Auto thresholds at the edge.
            self.list.row_heights(area.width)
        };
        let content_rows = if uniform {
            item_count
        } else {
            heights.iter().map(|height| usize::from(*height)).sum()
        };
        let overflow = content_rows > usize::from(area.height) && area.width > 1;
        let rows_area = Rect {
            width: area.width.saturating_sub(u16::from(overflow)),
            ..area
        };
        let stacked = self.list.row_layout.stacks_at(rows_area.width);
        if uniform {
            self.state.prepare(rows_area, item_count);
        } else if has_dividers {
            let rows = self
                .list
                .items
                .iter()
                .zip(&heights)
                .map(|(item, height)| crate::RowMetrics::new(*height, !item.is_divider()))
                .collect::<Vec<_>>();
            self.state.prepare_with_rows(rows_area, &rows);
        } else {
            self.state.prepare_with_heights(rows_area, &heights);
        }

        if item_count == 0 {
            let content = SelectableRow::new(false, self.theme.selected)
                .inactive_style(self.theme.style)
                .right_padding(self.theme.right_padding)
                .paint(
                    Rect::new(rows_area.x, rows_area.y, rows_area.width, 1),
                    buffer,
                );
            buffer.set_style(
                Rect::new(
                    rows_area.x,
                    rows_area.y,
                    self.theme.left_padding.min(rows_area.width),
                    1,
                ),
                self.theme.empty,
            );
            Paragraph::new(self.list.empty_message.as_str())
                .style(self.theme.empty)
                .render(content, buffer);
        } else {
            let mut y = rows_area.y;
            let offset = self.state.offset();
            for (index, item) in self.list.items.iter().enumerate().skip(offset) {
                if y >= rows_area.bottom() {
                    break;
                }
                let height = if uniform {
                    1
                } else {
                    heights.get(index).copied().unwrap_or(1).max(1)
                }
                .min(rows_area.bottom() - y);
                render_list_item(
                    item,
                    Rect::new(rows_area.x, y, rows_area.width, height),
                    stacked,
                    self.state.selected() == Some(index),
                    self.state.pointer_phase_at(index),
                    self.state.spinner_frame(),
                    self.theme,
                    buffer,
                );
                y = y.saturating_add(height);
            }
        }
        if overflow {
            VerticalScrollbar::new(
                content_rows,
                usize::from(rows_area.height),
                self.state.offset_row(),
            )
            .track_style(self.theme.scrollbar_track)
            .thumb_style(self.theme.scrollbar_thumb)
            .render(
                Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
                buffer,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_list_item(
    item: &ListItem,
    area: Rect,
    stacked: bool,
    selected: bool,
    pointer_phase: TerminalPointerPhase,
    spinner_frame: usize,
    theme: PageTheme,
    buffer: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }
    let highlighted = !item.divider && (selected || pointer_phase != TerminalPointerPhase::Idle);
    let active_style = match pointer_phase {
        TerminalPointerPhase::Idle | TerminalPointerPhase::Hovered if selected => theme.selected,
        TerminalPointerPhase::Idle => theme.selected,
        TerminalPointerPhase::Hovered => theme.hovered,
        TerminalPointerPhase::Pressed => theme.selected.add_modifier(Modifier::BOLD),
    };
    let content = SelectableRow::new(highlighted, active_style)
        .inactive_style(theme.style)
        .right_padding(theme.right_padding)
        .paint(area, buffer);
    if !highlighted && theme.left_padding_style != Style::new() {
        buffer.set_style(
            Rect::new(
                area.x,
                area.y,
                theme.left_padding.min(area.width),
                area.height,
            ),
            theme.left_padding_style,
        );
    }
    if content.is_empty() {
        return;
    }
    if item.divider {
        render_list_divider(item, content, theme, buffer);
        return;
    }
    let content = render_list_item_media(item, content, highlighted, theme, buffer);
    if content.is_empty() {
        return;
    }

    // Split the multi-row content into top band, text rows, and bottom band.
    let mut rows = content;
    if let Some(band) = &item.top {
        let band_area = Rect { height: 1, ..rows };
        render_list_item_band(item, band, band_area, highlighted, theme, buffer);
        rows.y = rows.y.saturating_add(1);
        rows.height = rows.height.saturating_sub(1);
    }
    let text_rows = 1 + u16::from(stacked && (item.detail.is_some() || item.value.is_some()));
    let bottom_band = item.bottom.as_ref().filter(|_| rows.height > text_rows);
    if let Some(band) = bottom_band {
        let band_area = Rect::new(rows.x, rows.bottom() - 1, rows.width, 1);
        render_list_item_band(item, band, band_area, highlighted, theme, buffer);
        rows.height -= 1;
    }
    if rows.is_empty() {
        return;
    }
    let content = Rect { height: 1, ..rows };
    let stacked_row =
        (stacked && rows.height > 1).then(|| Rect::new(rows.x, rows.y + 1, rows.width, 1));
    let value_on_second_row = stacked && stacked_row.is_some();

    let mut left = Vec::new();
    if item.busy {
        let style = if highlighted {
            theme.selected_busy
        } else {
            theme.busy
        };
        left.push(Span::styled(
            format!("{} ", crate::Spinner::glyph_for(spinner_frame)),
            style,
        ));
    }
    if let Some(slot) = &item.leading {
        append_leading_slot(&mut left, slot, highlighted, theme);
    }
    let mut label_style = if item.done {
        if highlighted {
            theme.selected_detail
        } else {
            theme.done
        }
        .add_modifier(Modifier::CROSSED_OUT)
    } else if highlighted {
        theme.selected_item
    } else if item.action_role == ListItemActionRole::Destructive {
        theme.danger
    } else {
        theme.tone(item.label_tone)
    };
    if item.emphasis == ListItemEmphasis::Strong {
        label_style = label_style.add_modifier(Modifier::BOLD);
    }
    append_list_item_text(
        &mut left,
        &item.label,
        &item.label_runs,
        label_style,
        theme.selected_item,
        highlighted,
        theme,
        item.done,
    );
    if let Some(ListItemSlot::Badge(badge)) = &item.accessory {
        left.push(Span::raw(" "));
        left.push(Span::styled(
            badge.text.clone(),
            if highlighted {
                theme.selected_badge
            } else {
                theme.tone(badge.tone)
            },
        ));
    }
    if let Some(detail) = item.detail.as_ref().filter(|_| !value_on_second_row) {
        let detail_style = if highlighted {
            theme.selected_detail
        } else {
            theme.detail
        };
        left.push(Span::styled("  ", detail_style));
        append_list_item_text(
            &mut left,
            detail,
            &item.detail_runs,
            detail_style,
            theme.selected_detail,
            highlighted,
            theme,
            false,
        );
    }

    let mut suffix = Vec::new();
    // A full-width band is the terminal's interpretation of the same metric;
    // the compact trailing chart stays on the wire for native renderers.
    let band_has_sparkline = matches!(item.bottom, Some(ListItemBand::Sparkline(_)))
        || matches!(item.top, Some(ListItemBand::Sparkline(_)));
    let band_has_gauge = matches!(item.bottom, Some(ListItemBand::Gauge(_)))
        || matches!(item.top, Some(ListItemBand::Gauge(_)));
    let sparkline = item
        .trailing
        .as_ref()
        .and_then(ListItemSlot::as_sparkline)
        .filter(|_| !band_has_sparkline);
    let gauge = item
        .trailing
        .as_ref()
        .and_then(ListItemSlot::as_gauge)
        .filter(|_| !band_has_gauge);
    if let Some(slot) = &item.trailing
        && !matches!(slot, ListItemSlot::Sparkline(_) | ListItemSlot::Gauge(_))
    {
        append_trailing_slot(&mut suffix, slot, highlighted, theme);
    }
    if let Some(slot) = &item.accessory
        && !matches!(slot, ListItemSlot::Badge(_))
    {
        append_trailing_slot(&mut suffix, slot, highlighted, theme);
    }
    if item.delete.is_some() {
        suffix.push(Span::styled(
            "[d]",
            if highlighted {
                theme.selected_detail
            } else {
                theme.delete
            },
        ));
    }

    let suffix_width = Line::from(suffix.clone()).width();
    let gauge_caption = gauge
        .filter(|_| item.value.is_none())
        .map(Gauge::value_label);
    let value = item
        .value
        .as_deref()
        .filter(|_| !value_on_second_row)
        .or(gauge_caption.as_deref())
        .filter(|value| {
            let value_width = UnicodeWidthStr::width(*value);
            let default_min = value_width
                .saturating_add(suffix_width)
                .saturating_add(usize::from(SELECTABLE_LEFT_PADDING))
                .saturating_add(9)
                .saturating_add(if gauge.is_some() { 19 } else { 0 });
            usize::from(area.width)
                >= usize::from(
                    item.value_min_width
                        .unwrap_or_else(|| u16::try_from(default_min).unwrap_or(u16::MAX)),
                )
        });
    let value_style = value.map(|_| {
        if highlighted {
            theme.selected_value
        } else {
            theme.tone(item.value_tone)
        }
    });
    let mut right = Vec::new();
    if let Some(value) = value {
        append_list_item_text(
            &mut right,
            value,
            if item.value.is_some() {
                &item.value_runs
            } else {
                &[]
            },
            value_style.unwrap_or_default(),
            theme.selected_value,
            highlighted,
            theme,
            false,
        );
    }
    if !suffix.is_empty() {
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.extend(suffix);
    }
    let text_right_width = Line::from(right.clone())
        .width()
        .min(usize::from(content.width));
    let text_right_columns = u16::try_from(text_right_width).unwrap_or(content.width);
    let chart_columns = sparkline.map_or_else(
        || {
            gauge.map_or(0, |_| {
                18.min(
                    content
                        .width
                        .saturating_sub(18)
                        .saturating_sub(text_right_columns)
                        .saturating_sub(u16::from(text_right_columns > 0)),
                )
            })
        },
        |sparkline| {
            u16::try_from(sparkline.series.len())
                .unwrap_or(u16::MAX)
                .min(
                    content
                        .width
                        .saturating_sub(18)
                        .saturating_sub(text_right_columns)
                        .saturating_sub(u16::from(text_right_columns > 0)),
                )
        },
    );
    let chart_gap = u16::from(text_right_columns > 0 && chart_columns > 0);
    let right_columns = text_right_columns
        .saturating_add(chart_gap)
        .saturating_add(chart_columns)
        .min(content.width);
    let gap = u16::from(right_columns > 0 && right_columns < content.width);
    let [label_area, value_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(right_columns.saturating_add(gap)),
    ])
    .areas(content);
    Paragraph::new(Line::from(left)).render(label_area, buffer);
    if text_right_columns > 0 {
        let text_area = if chart_columns == 0 {
            value_area
        } else {
            Rect::new(
                value_area.x.saturating_add(gap),
                value_area.y,
                text_right_columns.min(value_area.width.saturating_sub(gap)),
                value_area.height,
            )
        };
        let mut paragraph = Paragraph::new(Line::from(right)).alignment(Alignment::Right);
        if theme.style_value_gap
            && let Some(style) = value_style
        {
            paragraph = paragraph.style(style);
        }
        paragraph.render(text_area, buffer);
    }
    if chart_columns > 0
        && let Some(sparkline) = sparkline
    {
        let chart_area = Rect::new(
            value_area
                .x
                .saturating_add(gap)
                .saturating_add(text_right_columns)
                .saturating_add(chart_gap),
            value_area.y,
            chart_columns,
            value_area.height,
        );
        sparkline
            .widget()
            .style(if highlighted {
                theme.selected_value
            } else {
                theme.tone(item.value_tone)
            })
            .render(chart_area, buffer);
    } else if chart_columns > 0
        && let Some(gauge) = gauge
    {
        let chart_area = Rect::new(
            value_area
                .x
                .saturating_add(gap)
                .saturating_add(text_right_columns)
                .saturating_add(chart_gap),
            value_area.y,
            chart_columns,
            value_area.height,
        );
        gauge
            .widget()
            .without_label()
            .styles(
                if highlighted {
                    theme.selected_value
                } else {
                    theme.tone(item.value_tone)
                },
                if highlighted {
                    theme.selected_detail
                } else {
                    theme.navigation
                },
            )
            .render(chart_area, buffer);
    }

    if let Some(second) = stacked_row {
        let mut spans = Vec::new();
        if let Some(detail) = &item.detail {
            let detail_style = if highlighted {
                theme.selected_detail
            } else {
                theme.detail
            };
            append_list_item_text(
                &mut spans,
                detail,
                &item.detail_runs,
                detail_style,
                theme.selected_detail,
                highlighted,
                theme,
                false,
            );
        }
        if let Some(value) = &item.value {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            append_list_item_text(
                &mut spans,
                value,
                &item.value_runs,
                if highlighted {
                    theme.selected_value
                } else {
                    theme.tone(item.value_tone)
                },
                theme.selected_value,
                highlighted,
                theme,
                false,
            );
        }
        Paragraph::new(Line::from(spans)).render(second, buffer);
    }
}

/// Draws a divider row: a thin muted rule with an optional caption.
fn render_list_divider(item: &ListItem, content: Rect, theme: PageTheme, buffer: &mut Buffer) {
    let area = Rect {
        height: 1,
        ..content
    };
    let rule = "─".repeat(usize::from(area.width));
    buffer.set_string(area.x, area.y, &rule, theme.divider);
    if item.label.is_empty() {
        return;
    }
    let caption = format!(" {} ", item.label);
    let caption_width = u16::try_from(UnicodeWidthStr::width(caption.as_str())).unwrap_or(u16::MAX);
    if caption_width + 2 > area.width {
        return;
    }
    buffer.set_string(area.x + 2, area.y, &caption, theme.detail);
}

/// Reserves and paints the media column, returning the remaining content.
fn render_list_item_media(
    item: &ListItem,
    content: Rect,
    highlighted: bool,
    theme: PageTheme,
    buffer: &mut Buffer,
) -> Rect {
    let Some(media) = &item.media else {
        return content;
    };
    let width = media.width.clamp(1, MAX_LIST_ITEM_MEDIA_WIDTH);
    if content.width <= width + 1 {
        return content;
    }
    let (block, remaining) = match media.side {
        ListItemMediaSide::Leading => (
            Rect::new(content.x, content.y, width, content.height),
            Rect::new(
                content.x + width + 1,
                content.y,
                content.width - width - 1,
                content.height,
            ),
        ),
        ListItemMediaSide::Trailing => (
            Rect::new(content.right() - width, content.y, width, content.height),
            Rect::new(
                content.x,
                content.y,
                content.width - width - 1,
                content.height,
            ),
        ),
    };
    let tone = theme.tone(media.tone);
    let fill = tone.fg.or(theme.navigation.fg);
    let mut style = Style::new();
    if let Some(color) = fill {
        style = style.bg(color);
    } else {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if highlighted {
        style = style.add_modifier(Modifier::BOLD);
    }
    buffer.set_style(block, style);
    for y in block.y..block.bottom() {
        buffer.set_string(block.x, y, " ".repeat(usize::from(block.width)), style);
    }
    if let Some(glyph) = &media.glyph {
        let glyph_width = u16::try_from(UnicodeWidthStr::width(glyph.as_str())).unwrap_or(u16::MAX);
        if glyph_width <= block.width {
            let x = block.x + (block.width - glyph_width) / 2;
            let y = block.y + block.height.saturating_sub(1) / 2;
            let glyph_style = style.fg(theme.selected_item.fg.unwrap_or(Color::White));
            buffer.set_string(x, y, glyph, glyph_style);
        }
    }
    remaining
}

fn render_list_item_band(
    item: &ListItem,
    band: &ListItemBand,
    area: Rect,
    highlighted: bool,
    theme: PageTheme,
    buffer: &mut Buffer,
) {
    if area.is_empty() {
        return;
    }
    // Charts take the row's value tone like the compact trailing slot; the
    // default muted tone falls back to the accent so a meter stays visible.
    let chart_style = if highlighted {
        theme.selected_value
    } else if item.value_tone == ListItemTone::Muted {
        theme.accent
    } else {
        theme.tone(item.value_tone)
    };
    let track_style = if highlighted {
        theme.selected_detail
    } else {
        theme.navigation
    };
    match band {
        ListItemBand::Gauge(gauge) => {
            let widget = gauge.widget().styles(chart_style, track_style);
            // The row's value already shows the caption; keep the band a
            // pure meter then. Otherwise the caption lives inside the band.
            if item.value.is_some() {
                widget.without_label().render(area, buffer);
            } else {
                widget.compact().render(area, buffer);
            }
        }
        ListItemBand::Sparkline(sparkline) => {
            sparkline.widget().style(chart_style).render(area, buffer);
        }
        ListItemBand::Text { text, tone } => Paragraph::new(text.as_str())
            .style(if highlighted {
                theme.selected_detail
            } else {
                theme.tone(*tone)
            })
            .render(area, buffer),
        ListItemBand::Divider => Paragraph::new("─".repeat(usize::from(area.width)))
            .style(if highlighted {
                theme.selected_detail
            } else {
                theme.divider
            })
            .render(area, buffer),
    }
}

#[allow(clippy::too_many_arguments)]
fn append_list_item_text(
    spans: &mut Vec<Span<'static>>,
    fallback: &str,
    runs: &[ListItemTextRun],
    fallback_style: Style,
    selected_default: Style,
    selected: bool,
    theme: PageTheme,
    crossed_out: bool,
) {
    if runs.is_empty() {
        spans.push(Span::styled(fallback.to_owned(), fallback_style));
        return;
    }
    for run in runs {
        let mut style = match run.tone {
            None => fallback_style,
            Some(ListItemTone::Default) if selected => selected_default,
            Some(ListItemTone::Muted) if selected => theme.selected_detail,
            Some(tone) => theme.tone(tone),
        };
        if let Some(emphasis) = run.emphasis {
            style = match emphasis {
                ListItemEmphasis::Regular => style.remove_modifier(Modifier::BOLD),
                ListItemEmphasis::Strong => style.add_modifier(Modifier::BOLD),
            };
        }
        if crossed_out {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        spans.push(Span::styled(run.text.clone(), style));
    }
}

fn append_leading_slot(
    spans: &mut Vec<Span<'static>>,
    slot: &ListItemSlot,
    selected: bool,
    theme: PageTheme,
) {
    match slot {
        ListItemSlot::Toggle(toggle) => spans.push(Span::styled(
            format!("{} ", toggle.marker()),
            if selected {
                theme.selected_item
            } else {
                theme.toggle
            },
        )),
        ListItemSlot::Status(status) => {
            let mut style = if selected && !status.preserve_tone_when_selected {
                theme.selected_item
            } else {
                theme.tone(status.tone)
            };
            if status.emphasis == ListItemEmphasis::Strong {
                style = style.add_modifier(Modifier::BOLD);
            }
            if theme.style_status_spacing {
                spans.push(Span::styled(format!("{}  ", status.symbol), style));
            } else {
                spans.push(Span::styled(status.symbol.clone(), style));
                spans.push(Span::raw("  "));
            }
        }
        ListItemSlot::Badge(badge) => spans.push(Span::styled(
            format!("{} ", badge.text),
            if selected {
                theme.selected_badge
            } else {
                theme.tone(badge.tone)
            },
        )),
        ListItemSlot::Sparkline(_) | ListItemSlot::Gauge(_) => {}
        ListItemSlot::Disclosure => spans.push(Span::styled(
            "› ",
            if selected {
                theme.selected_detail
            } else {
                theme.value
            },
        )),
        ListItemSlot::Checkmark(checkmark) => spans.push(Span::styled(
            format!("{} ", checkmark.marker()),
            if selected {
                theme.selected_item
            } else {
                theme.accent
            },
        )),
    }
}

fn append_trailing_slot(
    spans: &mut Vec<Span<'static>>,
    slot: &ListItemSlot,
    selected: bool,
    theme: PageTheme,
) {
    match slot {
        ListItemSlot::Toggle(toggle) => spans.push(Span::styled(
            toggle.marker(),
            if selected {
                theme.selected_item
            } else {
                theme.toggle
            },
        )),
        ListItemSlot::Status(status) => spans.push(Span::styled(
            status.symbol.clone(),
            if selected && !status.preserve_tone_when_selected {
                theme.selected_item
            } else {
                theme.tone(status.tone)
            },
        )),
        ListItemSlot::Badge(badge) => spans.push(Span::styled(
            badge.text.clone(),
            if selected {
                theme.selected_badge
            } else {
                theme.tone(badge.tone)
            },
        )),
        ListItemSlot::Sparkline(_) | ListItemSlot::Gauge(_) => {}
        ListItemSlot::Disclosure => spans.push(Span::styled(
            "›",
            if selected {
                theme.selected_detail
            } else {
                theme.value
            },
        )),
        ListItemSlot::Checkmark(checkmark) => spans.push(Span::styled(
            checkmark.marker(),
            if selected {
                theme.selected_item
            } else {
                theme.accent
            },
        )),
    }
}

/// Renderable Page view returned by [`Page::widget`].
pub struct PageWidget<'a> {
    page: &'a Page,
    input: &'a mut InputField,
    list_state: &'a mut ListState,
    content_state: PageContentState<'a>,
    theme: PageTheme,
    input_theme: Option<InputFieldTheme>,
    content_theme: ContentTheme,
}

enum PageContentState<'a> {
    Owned(ContentState),
    Borrowed(&'a mut ContentState),
}

impl PageContentState<'_> {
    fn as_mut(&mut self) -> &mut ContentState {
        match self {
            Self::Owned(state) => state,
            Self::Borrowed(state) => state,
        }
    }
}

impl PageWidget<'_> {
    #[must_use]
    pub const fn theme(mut self, theme: PageTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Overrides the Page-derived terminal styling for its Input slot.
    #[must_use]
    pub const fn input_theme(mut self, theme: InputFieldTheme) -> Self {
        self.input_theme = Some(theme);
        self
    }

    #[must_use]
    pub const fn content_theme(mut self, theme: ContentTheme) -> Self {
        self.content_theme = theme;
        self
    }
}

impl Widget for PageWidget<'_> {
    fn render(mut self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        buffer.set_style(area, self.theme.style);
        let layout = self.page.layout(area);
        // Only the chevron takes the gray treatment: keyboard focus, hover,
        // and press paint the cells around "‹" while the title stays plain.
        let back_phase = if self.page.back.is_some() {
            self.list_state.pointer().phase(layout.title)
        } else {
            TerminalPointerPhase::Idle
        };
        let back_focused = self.page.back.is_some() && self.list_state.back_focused();
        let back_active = back_focused || back_phase != TerminalPointerPhase::Idle;
        Paragraph::new(format!(
            "{}{}{}",
            " ".repeat(usize::from(self.theme.left_padding)),
            if self.page.back.is_some() {
                "‹  "
            } else {
                ""
            },
            self.page.title
        ))
        .style(self.theme.title)
        .render(layout.title, buffer);
        if back_active {
            let active_style = match back_phase {
                TerminalPointerPhase::Idle => self.theme.selected,
                TerminalPointerPhase::Hovered if back_focused => self.theme.selected,
                TerminalPointerPhase::Hovered => self.theme.hovered,
                TerminalPointerPhase::Pressed => self.theme.selected.add_modifier(Modifier::BOLD),
            };
            let chevron_area = Rect::new(
                layout
                    .title
                    .x
                    .saturating_add(self.theme.left_padding.saturating_sub(1)),
                layout.title.y,
                3.min(layout.title.width),
                1,
            );
            buffer.set_style(chevron_area, active_style.patch(self.theme.selected_item));
        }

        if let Some(input) = self.page.input_spec() {
            if self.input.text() != input.value {
                self.input.set_text(input.value.clone());
            }
            self.input.set_placeholder(input.placeholder.clone());
            self.input.set_prompt(format!("{}: ", input.label));
            self.input
                .set_theme(self.input_theme.unwrap_or_else(|| self.theme.input_theme()));
            self.input
                .widget()
                .render(layout.input.expect("Page input layout"), buffer);
        }
        match &self.page.body {
            PageBodySlot::List(list) => list
                .widget(self.list_state)
                .theme(self.theme)
                .render(layout.list, buffer),
            PageBodySlot::Content(content) => {
                content
                    .widget(self.content_state.as_mut())
                    .theme(self.content_theme)
                    .render(layout.list, buffer);
            }
            PageBodySlot::Sparkline(sparkline) => {
                let body = self.theme.inset_body(layout.list);
                let metadata = [sparkline.caption.as_deref(), sparkline.unit.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                let chart_slot = if metadata.is_empty() {
                    body
                } else {
                    let label = Rect::new(body.x, body.y, body.width, 1);
                    Paragraph::new(metadata)
                        .style(self.theme.value)
                        .render(label, buffer);
                    Rect::new(
                        body.x,
                        body.y.saturating_add(1),
                        body.width,
                        body.height.saturating_sub(1),
                    )
                };
                let height = chart_slot.height.clamp(1, 3);
                let area = Rect::new(
                    chart_slot.x,
                    chart_slot.y + chart_slot.height.saturating_sub(height) / 2,
                    chart_slot.width,
                    height,
                );
                sparkline
                    .widget()
                    .style(self.theme.accent)
                    .pointer(self.list_state.pointer())
                    .render(area, buffer);
            }
            PageBodySlot::BarChart(chart) => chart
                .widget()
                .styles(
                    self.theme.value,
                    self.theme.accent,
                    self.theme.danger,
                    self.theme.item,
                )
                .pointer(self.list_state.pointer())
                .render(self.theme.inset_body(layout.list), buffer),
            PageBodySlot::LineChart(chart) => chart
                .widget()
                .styles(
                    self.theme.navigation,
                    [
                        self.theme.accent,
                        self.theme.info,
                        self.theme.success,
                        self.theme.warning,
                        self.theme.danger,
                        self.theme.item,
                    ],
                )
                .pointer(self.list_state.pointer())
                .render(self.theme.inset_body(layout.list), buffer),
            PageBodySlot::Gauge(gauge) => {
                let body = self.theme.inset_body(layout.list);
                let height = body.height.clamp(1, 3);
                let area = Rect::new(
                    body.x,
                    body.y + body.height.saturating_sub(height) / 2,
                    body.width,
                    height,
                );
                gauge
                    .widget()
                    .styles(self.theme.accent, self.theme.value)
                    .pointer(self.list_state.pointer())
                    .render(area, buffer);
            }
        }
        if let Some(footer) = layout.footer {
            self.page
                .footer
                .widget()
                .styles(
                    self.theme.style,
                    self.theme.accent.add_modifier(Modifier::BOLD),
                    self.theme.detail,
                    self.theme.danger,
                    self.theme.navigation.add_modifier(Modifier::DIM),
                )
                .pointer(self.list_state.pointer())
                .spinner_frame(self.list_state.spinner_frame())
                .interaction_styles(
                    self.theme.hovered,
                    self.theme.selected.add_modifier(Modifier::BOLD),
                )
                .render(footer, buffer);
        }
    }
}

/// Validation failure for the closed component tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentValidationError {
    pub path: String,
    pub message: String,
}

impl ComponentValidationError {
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ComponentValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ComponentValidationError {}

fn register_unique(
    ids: &mut HashSet<String>,
    id: &str,
    path: &str,
) -> Result<(), ComponentValidationError> {
    if !ids.insert(id.to_owned()) {
        return Err(ComponentValidationError::new(
            path,
            format!("duplicate component id {id:?}"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_identifier(value: &str, path: &str) -> Result<(), ComponentValidationError> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
    {
        return Err(ComponentValidationError::new(
            path,
            "must be a portable identifier",
        ));
    }
    Ok(())
}

pub(crate) fn validate_text(
    value: &str,
    maximum: usize,
    path: &str,
) -> Result<(), ComponentValidationError> {
    if value.len() > maximum
        || value
            .chars()
            .any(|character| matches!(character, '\0' | '\r'))
    {
        return Err(ComponentValidationError::new(
            path,
            format!("must contain at most {maximum} bytes and no NUL/CR characters"),
        ));
    }
    Ok(())
}

fn validate_single_line(value: &str, path: &str) -> Result<(), ComponentValidationError> {
    if value.contains('\n') {
        return Err(ComponentValidationError::new(path, "must be a single line"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn todo_page() -> Page {
        Page::new(
            "Todos",
            List::new(
                "todos",
                vec![
                    ListItem::new("todo-1", "Run the standalone TUI")
                        .done(true)
                        .trailing(ListItemSlot::toggle(Toggle::new(
                            "todo-1-toggle",
                            "Completed",
                            true,
                            "set-done",
                        )))
                        .delete_action("delete-todo"),
                ],
            )
            .empty_message("No todos yet"),
        )
        .input(
            Input::new("new-todo", "New todo")
                .placeholder("What needs doing?")
                .submit_action("add-todo"),
        )
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (buffer.area.x..buffer.area.right())
            .filter(|x| !matches!(buffer[(*x, y)].symbol(), "┃" | "│"))
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn draw_list(list: &List, state: &mut ListState, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(list.widget(state), frame.area()))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn stacked_layout_moves_detail_and_value_beneath_the_label() {
        let items = vec![
            ListItem::new("codex", "Codex")
                .accessory(ListItemSlot::badge(Badge::new("Pro")))
                .detail("5h window")
                .value("42% left · resets in 2h")
                .value_min_width(0),
            ListItem::new("claude", "Claude").value("ok"),
        ];
        let inline = List::new("providers", items.clone());
        let stacked = List::new("providers", items.clone()).row_layout(ListRowLayout::Stacked);
        let auto = List::new("providers", items).row_layout(ListRowLayout::Auto {
            stack_below_width: 40,
        });

        let mut state = ListState::new(Some(0));
        let buffer = draw_list(&inline, &mut state, 60, 4);
        assert_eq!(
            row_text(&buffer, 0),
            "  Codex Pro  5h window              42% left · resets in 2h"
        );
        assert_eq!(
            row_text(&buffer, 1),
            format!("  Claude{}ok", " ".repeat(49))
        );
        assert!(state.has_uniform_rows());

        let mut state = ListState::new(Some(0));
        let buffer = draw_list(&stacked, &mut state, 30, 5);
        assert_eq!(row_text(&buffer, 0), "  Codex Pro");
        assert_eq!(row_text(&buffer, 1), "  5h window  42% left · reset");
        assert_eq!(row_text(&buffer, 2), "  Claude");
        assert_eq!(row_text(&buffer, 3), "  ok");
        assert_eq!(state.item_height(0), 2);
        assert_eq!(state.item_area(1), Some(Rect::new(0, 2, 30, 2)));
        let selected = KitTheme::dark().selected_row.bg.unwrap();
        assert_eq!(buffer[(5, 1)].bg, selected, "selection spans both rows");
        assert_ne!(buffer[(5, 2)].bg, selected);
        let muted = KitTheme::dark().selected_row.fg.unwrap();
        assert_eq!(buffer[(2, 1)].fg, muted);

        let mut state = ListState::new(Some(0));
        let wide = draw_list(&auto, &mut state, 60, 4);
        assert_eq!(row_text(&wide, 1), format!("  Claude{}ok", " ".repeat(49)));
        let narrow = draw_list(&auto, &mut state, 30, 5);
        assert_eq!(row_text(&narrow, 1), "  5h window  42% left · reset");
    }

    #[test]
    fn bands_render_on_their_own_rows_inside_the_content_inset() {
        let list = List::new(
            "bands",
            vec![
                ListItem::new("quota", "Weekly quota")
                    .value("61%")
                    .bottom(ListItemBand::gauge(
                        Gauge::new("quota-gauge", 0.61, "7-day", "61 percent left")
                            .caption("61% left"),
                    )),
                ListItem::new("note", "Release")
                    .top(ListItemBand::text(
                        "Shipped yesterday",
                        ListItemTone::Success,
                    ))
                    .bottom(ListItemBand::divider()),
            ],
        );
        let mut state = ListState::new(None);
        let buffer = draw_list(&list, &mut state, 30, 6);
        assert_eq!(row_text(&buffer, 0), "  Weekly quota            61%");
        let gauge_row = row_text(&buffer, 1);
        assert!(gauge_row.starts_with("  "), "{gauge_row}");
        assert!(
            !gauge_row.contains("61% left"),
            "the value row already shows the caption: {gauge_row}"
        );
        assert!(gauge_row.contains('─'), "{gauge_row}");
        let first_bar = (0..30)
            .find(|x| buffer[(*x, 1)].symbol() == "─")
            .expect("meter cell");
        assert_eq!(
            buffer[(first_bar, 1)].fg,
            PageTheme::for_theme(KitTheme::dark()).accent.fg.unwrap(),
            "default muted value tone falls back to the accent meter"
        );
        assert_eq!(buffer[(1, 1)].symbol(), " ", "band respects the left inset");
        assert_eq!(
            buffer[(29, 1)].symbol(),
            " ",
            "band respects the right inset"
        );
        assert_eq!(row_text(&buffer, 2), "  Shipped yesterday");
        assert_eq!(row_text(&buffer, 3), "  Release");
        assert_eq!(row_text(&buffer, 4), format!("  {}", "─".repeat(27)));
        let divider = PageTheme::for_theme(KitTheme::dark()).divider;
        assert_eq!(buffer[(2, 4)].fg, divider.fg.unwrap());
        assert!(buffer[(2, 4)].modifier.contains(Modifier::DIM));
        assert_eq!(
            PageTheme::for_theme(KitTheme::light()).divider.fg,
            Some(KitTheme::light().subtle)
        );
        assert_eq!(state.item_area(0), Some(Rect::new(0, 0, 30, 2)));
        assert_eq!(state.item_area(1), Some(Rect::new(0, 2, 30, 3)));
        assert_eq!(state.content_rows(2), 5);

        // A trailing Gauge slot stays on the wire for native renderers but the
        // terminal draws only the full-width band.
        let both = List::new(
            "both",
            vec![
                ListItem::new("quota", "Weekly quota")
                    .value("61%")
                    .trailing(ListItemSlot::gauge(Gauge::new("slot", 0.61, "7-day", "61")))
                    .bottom(ListItemBand::gauge(Gauge::new("band", 0.61, "7-day", "61"))),
            ],
        );
        let mut state = ListState::new(None);
        let buffer = draw_list(&both, &mut state, 40, 3);
        assert_eq!(
            row_text(&buffer, 0),
            format!("  Weekly quota{}61%", " ".repeat(22))
        );
    }

    #[test]
    fn media_column_spans_every_row_and_reserves_its_width() {
        let list = List::new(
            "media",
            vec![
                ListItem::new("lead", "Leading")
                    .detail("beside the block")
                    .media(
                        ListItemMedia::leading(4)
                            .glyph("AB")
                            .tone(ListItemTone::Info),
                    ),
                ListItem::new("trail", "Trailing")
                    .detail("block on the right")
                    .media(ListItemMedia::trailing(3)),
            ],
        )
        .row_layout(ListRowLayout::Stacked);
        let mut state = ListState::new(None);
        let buffer = draw_list(&list, &mut state, 32, 4);
        assert_eq!(row_text(&buffer, 0), "   AB  Leading");
        assert_eq!(row_text(&buffer, 1), "       beside the block");
        let info = KitTheme::dark().accent;
        let _ = info;
        for y in 0..2 {
            for x in 2..6 {
                assert!(
                    buffer[(x, y)].bg != Color::Reset,
                    "media block at ({x}, {y})"
                );
            }
            assert_eq!(buffer[(6, y)].bg, Color::Reset, "gap after the block");
        }
        assert_eq!(row_text(&buffer, 2), "  Trailing");
        assert_eq!(row_text(&buffer, 3), "  block on the right");
        for y in 2..4 {
            for x in 28..31 {
                assert!(
                    buffer[(x, y)].bg != Color::Reset,
                    "trailing block at ({x}, {y})"
                );
            }
            assert_eq!(buffer[(27, y)].bg, Color::Reset);
        }
    }

    #[test]
    fn mixed_row_heights_scroll_reveal_and_hit_test_per_item() {
        let list = List::new(
            "mixed",
            vec![
                ListItem::new("a", "Alpha"),
                ListItem::new("b", "Beta").detail("two rows"),
                ListItem::new("c", "Gamma")
                    .bottom(ListItemBand::text("band", ListItemTone::Muted))
                    .detail("three rows"),
                ListItem::new("d", "Delta"),
                ListItem::new("e", "Epsilon").detail("two rows"),
            ],
        )
        .row_layout(ListRowLayout::Stacked)
        .scroll_padding(1);
        let mut state = ListState::new(Some(0));
        let buffer = draw_list(&list, &mut state, 20, 5);
        assert_eq!(row_text(&buffer, 0), "  Alpha");
        assert_eq!(row_text(&buffer, 1), "  Beta");
        assert_eq!(row_text(&buffer, 2), "  two rows");
        assert_eq!(row_text(&buffer, 3), "  Gamma");
        assert_eq!(row_text(&buffer, 4), "  three rows");
        assert_eq!(state.content_rows(5), 9);
        assert_eq!(
            state.rows_area().width,
            19,
            "overflow reserves the scrollbar"
        );
        assert_eq!(state.visible_item_count(5), 2);
        assert_eq!(
            state.item_at(ratatui::layout::Position::new(3, 2), 5),
            Some(1)
        );
        assert_eq!(
            state.item_at(ratatui::layout::Position::new(3, 4), 5),
            Some(2)
        );
        assert_eq!(state.item_area(2), Some(Rect::new(0, 3, 19, 2)), "clipped");

        state.navigate(ListNavigationAction::Down, 5);
        state.navigate(ListNavigationAction::Down, 5);
        let buffer = draw_list(&list, &mut state, 20, 5);
        assert_eq!(state.selected(), Some(2));
        assert_eq!(
            state.offset(),
            2,
            "Gamma's three rows plus one padding row need the viewport from Gamma"
        );
        assert_eq!(row_text(&buffer, 0), "  Gamma");
        assert_eq!(row_text(&buffer, 2), "  band");
        assert_eq!(row_text(&buffer, 3), "  Delta");
        assert_eq!(state.offset_row(), 3);

        state.navigate(ListNavigationAction::Last, 5);
        let buffer = draw_list(&list, &mut state, 20, 5);
        // The offset stays item-granular: the last valid offset keeps Epsilon
        // fully visible instead of clipping it, leaving blank rows below.
        assert_eq!(state.offset(), 3);
        assert_eq!(state.offset(), state.max_offset(5));
        assert_eq!(row_text(&buffer, 0), "  Delta");
        assert_eq!(row_text(&buffer, 2), "  two rows");
        assert_eq!(row_text(&buffer, 3), "");
        assert_eq!(
            state.item_at(ratatui::layout::Position::new(3, 2), 5),
            Some(4)
        );
        assert_eq!(state.item_at(ratatui::layout::Position::new(3, 4), 5), None);
        assert_eq!(state.item_area(4).map(|area| area.height), Some(2));
        assert_eq!(state.item_area(0), None);

        state.navigate(ListNavigationAction::PageUp, 5);
        assert!(state.selected().unwrap() < 4);
        state.navigate(ListNavigationAction::First, 5);
        let buffer = draw_list(&list, &mut state, 20, 5);
        assert_eq!(state.offset(), 0);
        assert_eq!(row_text(&buffer, 0), "  Alpha");
    }

    #[test]
    fn divider_rows_render_muted_rules_and_are_skipped_by_focus_and_clicks() {
        let list = List::new(
            "grouped",
            vec![
                ListItem::divider_labeled("sep-top", "Providers"),
                ListItem::new("a", "Alpha").activate_action("open"),
                ListItem::divider("sep-mid"),
                ListItem::new("b", "Beta").activate_action("open"),
            ],
        );
        Page::new("Grouped", list.clone()).validate().unwrap();
        let mut state = ListState::new(Some(0));
        let buffer = draw_list(&list, &mut state, 24, 4);
        assert_eq!(row_text(&buffer, 0), "  ── Providers ────────");
        assert_eq!(row_text(&buffer, 1), "  Alpha");
        assert_eq!(row_text(&buffer, 2), format!("  {}", "─".repeat(21)));
        assert_eq!(state.selected(), Some(1), "initial focus skips the divider");
        let selected = KitTheme::dark().selected_row.bg.unwrap();
        assert_eq!(buffer[(3, 1)].bg, selected);
        assert_ne!(buffer[(3, 0)].bg, selected);
        let divider = PageTheme::for_theme(KitTheme::dark()).divider;
        assert_eq!(buffer[(2, 2)].fg, divider.fg.unwrap());
        assert_eq!(
            buffer[(4, 0)].fg,
            PageTheme::for_theme(KitTheme::dark()).detail.fg.unwrap()
        );

        state.navigate(ListNavigationAction::Down, 4);
        assert_eq!(state.selected(), Some(3), "Down jumps over the divider");
        state.navigate(ListNavigationAction::Up, 4);
        assert_eq!(state.selected(), Some(1));

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(list.pointer_decision(&mut state, &click), None);
        let release = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            ..click
        };
        state.track_mouse(&release);
        assert_eq!(state.selected(), Some(1));

        let json = serde_json::to_value(ListItem::divider("d")).unwrap();
        assert_eq!(json["divider"], true);
        assert_eq!(json["label"], "");
        assert!(
            serde_json::to_value(ListItem::new("x", "X"))
                .unwrap()
                .get("divider")
                .is_none()
        );
        assert!(
            ListItem::divider("bad")
                .value("no")
                .validate("item")
                .is_err()
        );
    }

    #[test]
    fn back_row_is_a_focusable_gray_stop_above_the_first_item() {
        let page = Page::new(
            "Detail",
            List::new(
                "rows",
                vec![
                    ListItem::divider("sep"),
                    ListItem::new("a", "Alpha"),
                    ListItem::new("b", "Beta"),
                ],
            ),
        )
        .back_action("close");
        let mut state = ListState::new(Some(1));
        let mut input = InputField::new("");
        let mut draw = |state: &mut ListState| {
            let mut terminal = Terminal::new(TestBackend::new(30, 6)).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(page.widget(&mut input, state), frame.area());
                })
                .unwrap();
            terminal.backend().buffer().clone()
        };
        let selected = KitTheme::dark().selected_row.bg.unwrap();
        let buffer = draw(&mut state);
        assert_ne!(buffer[(0, 0)].bg, selected);

        assert_eq!(
            page.navigate(&mut state, ListNavigationAction::Up),
            ListNavigationOutcome::FocusedBack,
            "Up from the first selectable row focuses back"
        );
        assert!(state.back_focused());
        assert_eq!(state.selected(), None);
        let buffer = draw(&mut state);
        assert!(
            (1..4).all(|x| buffer[(x, 0)].bg == selected),
            "only the chevron cells take the selection background"
        );
        assert_ne!(buffer[(0, 0)].bg, selected);
        assert_ne!(buffer[(6, 0)].bg, selected, "title text stays plain");
        assert_eq!(row_text(&buffer, 0), "  ‹  Detail");
        assert_ne!(buffer[(0, 2)].bg, selected, "list rows are unselected");

        assert_eq!(
            page.navigate(&mut state, ListNavigationAction::Up),
            ListNavigationOutcome::None
        );
        assert_eq!(
            page.navigate(&mut state, ListNavigationAction::Activate),
            ListNavigationOutcome::Back
        );
        assert_eq!(
            page.navigate(&mut state, ListNavigationAction::Down),
            ListNavigationOutcome::SelectionChanged(1),
            "Down returns to the first selectable row"
        );
        assert!(!state.back_focused());
        let buffer = draw(&mut state);
        assert_ne!(buffer[(0, 0)].bg, selected);

        // Hovering the back row paints the same gray row, never an underline.
        state.track_mouse(&MouseEvent {
            kind: MouseEventKind::Moved,
            column: 4,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        let buffer = draw(&mut state);
        let hovered = KitTheme::dark().hovered_row.bg.unwrap();
        assert_ne!(hovered, selected, "hover and selection are distinct");
        assert_eq!(buffer[(2, 0)].bg, hovered, "hover paints the chevron");
        assert_ne!(buffer[(8, 0)].bg, hovered);
        assert!(
            (0..30).all(|x| !buffer[(x, 0)].modifier.contains(Modifier::UNDERLINED)),
            "no underline on hover"
        );

        let no_back = Page::new("Plain", List::new("rows", vec![ListItem::new("a", "A")]));
        let mut plain = ListState::new(Some(0));
        assert_eq!(
            no_back.navigate(&mut plain, ListNavigationAction::Up),
            ListNavigationOutcome::None
        );
        assert!(!plain.back_focused());
    }

    #[test]
    fn busy_footer_actions_animate_a_braille_spinner_beside_the_label() {
        let page = Page::new("Usage", List::new("rows", vec![ListItem::new("a", "A")]))
            .footer_actions(vec![
                FooterAction::new("refresh", "refreshing…", "refresh")
                    .accelerator("r")
                    .busy(true)
                    .disabled(true),
            ]);
        let mut input = InputField::new("");
        let mut state = ListState::new(Some(0));
        state.set_spinner_frame(3);
        let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(page.widget(&mut input, &mut state), frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_eq!(row_text(&buffer, 3), "  r ⠸ refreshing…");
        let json =
            serde_json::to_value(FooterAction::new("r", "Refresh", "refresh").busy(true)).unwrap();
        assert_eq!(json["busy"], true);
        assert!(
            serde_json::to_value(FooterAction::new("r", "Refresh", "refresh"))
                .unwrap()
                .get("busy")
                .is_none()
        );
    }

    #[test]
    fn setting_toggles_are_switches_that_never_strike_the_row() {
        let list = List::new(
            "alerts",
            vec![
                ListItem::new("near", "Close to a limit")
                    .detail("At 80% used")
                    .trailing(ListItemSlot::toggle(Toggle::setting(
                        "near-toggle",
                        "Close to a limit",
                        true,
                        "set-alert",
                    ))),
                ListItem::new("todo", "Ship it")
                    .done(true)
                    .trailing(ListItemSlot::toggle(Toggle::new(
                        "todo-toggle",
                        "Done",
                        true,
                        "set-done",
                    ))),
            ],
        );
        Page::new("Alerts", list.clone()).validate().unwrap();
        let mut state = ListState::new(None);
        let buffer = draw_list(&list, &mut state, 40, 3);
        assert_eq!(
            row_text(&buffer, 0),
            "  Close to a limit  At 80% used     (●)"
        );
        assert!(
            !buffer[(2, 0)].modifier.contains(Modifier::CROSSED_OUT),
            "setting toggles keep the label intact"
        );
        assert!(buffer[(2, 1)].modifier.contains(Modifier::CROSSED_OUT));
        let json = serde_json::to_value(&list.items[0].trailing).unwrap();
        assert_eq!(json["role"], "setting");
        assert!(
            serde_json::to_value(&list.items[1].trailing)
                .unwrap()
                .get("role")
                .is_none()
        );
        let mut item = list.items[0].clone();
        assert!(item.set_toggle_value("near-toggle", false));
        assert!(!item.done, "setting toggles never mark the row done");
        assert!(
            ListItem::new("bad", "Bad")
                .done(true)
                .trailing(ListItemSlot::toggle(Toggle::setting(
                    "t", "T", false, "set"
                )))
                .validate("item")
                .is_ok(),
            "done is independent of a setting toggle"
        );
    }

    #[test]
    fn rich_list_item_fields_validate_and_round_trip_serde() {
        let item = ListItem::new("row", "Row")
            .top(ListItemBand::text("above", ListItemTone::Muted))
            .bottom(ListItemBand::gauge(Gauge::new("g", 0.5, "G", "half")))
            .media(ListItemMedia::trailing(3).glyph("R"));
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["top"]["type"], "text");
        assert_eq!(json["bottom"]["type"], "gauge");
        assert_eq!(json["media"]["side"], "trailing");
        assert_eq!(json["media"]["width"], 3);
        let plain = serde_json::to_value(ListItem::new("plain", "Plain")).unwrap();
        assert!(plain.get("top").is_none() && plain.get("media").is_none());
        let decoded: ListItem = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, item);
        let list = List::new("l", vec![item]).row_layout(ListRowLayout::Auto {
            stack_below_width: 50,
        });
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(json["rowLayout"]["type"], "auto");
        assert_eq!(json["rowLayout"]["stackBelowWidth"], 50);
        assert!(
            serde_json::to_value(List::new("l", vec![]))
                .unwrap()
                .get("rowLayout")
                .is_none()
        );
        Page::new("Rich", list).validate().unwrap();

        let clickable = ListItem::new("bad", "Bad").bottom(ListItemBand::gauge(
            Gauge::new("g", 0.5, "G", "half").activate("open"),
        ));
        assert_eq!(
            clickable.validate("item").unwrap_err().path,
            "item.bottom.activate"
        );
        let mut wide = ListItemMedia::leading(3);
        wide.width = 40;
        assert!(
            ListItem::new("m", "M")
                .media(wide)
                .validate("item")
                .is_err()
        );
        let duplicate = Page::new(
            "Dup",
            List::new(
                "l",
                vec![
                    ListItem::new("a", "A")
                        .bottom(ListItemBand::gauge(Gauge::new("g", 0.5, "G", "h"))),
                    ListItem::new("b", "B")
                        .top(ListItemBand::gauge(Gauge::new("g", 0.5, "G", "h"))),
                ],
            ),
        );
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn list_composition_is_closed_and_ids_are_unique() {
        let page = todo_page();
        page.validate().unwrap();
        assert_eq!(
            page.required_capabilities(),
            vec![
                "page",
                "list",
                "listItem",
                "input",
                "listItemRole",
                "toggle"
            ]
        );

        let mut duplicate = page;
        duplicate.body.as_list_mut().unwrap().items[0]
            .trailing
            .as_mut()
            .unwrap()
            .toggle_mut("todo-1-toggle")
            .unwrap()
            .id = "new-todo".to_owned();
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn terminal_page_uses_shared_list_rows_and_input_field() {
        let page = todo_page();
        let mut input = InputField::new("");
        input.set_focused(true);
        let mut state = ListState::new(Some(0));
        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(page.widget(&mut input, &mut state), frame.area());
            })
            .unwrap();
        let rendered = terminal.backend().buffer();
        let text = rendered
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Todos"));
        assert!(text.contains("[x]"));
        assert!(text.contains("Run the standalone TUI"));
        let selected_y = 4;
        assert_eq!(
            rendered[(0, selected_y)].bg,
            ratatui::style::Color::Rgb(63, 63, 70)
        );
        assert_eq!(
            rendered[(49, selected_y)].bg,
            ratatui::style::Color::Rgb(63, 63, 70)
        );
        assert_eq!(rendered[(0, selected_y)].symbol(), " ");
        assert_eq!(rendered[(1, selected_y)].symbol(), " ");
        assert_eq!(rendered[(2, selected_y)].symbol(), "R");
    }

    #[test]
    fn footer_actions_share_terminal_paint_keys_and_hit_geometry() {
        let page = Page::new("Usage", List::new("metrics", Vec::new())).footer_actions([
            FooterAction::new("open-alerts", "alert", "open-alerts").accelerator("a"),
            FooterAction::new("refresh", "refresh", "refresh-usage").accelerator("r"),
        ]);
        page.validate().unwrap();
        let mut input = InputField::new("");
        let mut state = ListState::default();
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(page.widget(&mut input, &mut state), frame.area()))
            .unwrap();

        let rendered = terminal.backend().buffer();
        let footer = page.layout(rendered.area).footer.unwrap();
        let row = (0..rendered.area.width)
            .map(|x| rendered[(x, footer.y)].symbol())
            .collect::<String>();
        assert!(row.starts_with("  a alert  r refresh"));
        assert_eq!(
            page.footer
                .action_for_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
                .map(|action| action.action.as_str()),
            Some("refresh-usage")
        );
        assert_eq!(
            page.footer
                .action_at(ratatui::layout::Position::new(14, footer.y), footer)
                .map(|action| action.id.as_str()),
            Some("refresh")
        );
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 14,
            row: footer.y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            page.footer
                .action_for_mouse(&click, footer)
                .map(|action| action.action.as_str()),
            Some("refresh-usage")
        );
        #[cfg(feature = "ui-bridge")]
        assert_eq!(
            page.footer.ui_action_for_mouse(&click, footer),
            page.footer
                .ui_action_for_key(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE,))
        );

        state.track_mouse(&click);
        terminal
            .draw(|frame| frame.render_widget(page.widget(&mut input, &mut state), frame.area()))
            .unwrap();
        assert!(
            terminal.backend().buffer()[(14, footer.y)]
                .modifier
                .contains(Modifier::BOLD),
            "a pressed footer hint receives the terminal press affordance"
        );
    }

    #[test]
    #[cfg(feature = "ui-bridge")]
    fn terminal_list_click_and_keyboard_primary_emit_the_same_action() {
        let list = List::new(
            "todos",
            vec![
                ListItem::new("todo-1", "Ship it").trailing(ListItemSlot::toggle(Toggle::new(
                    "todo-1-toggle",
                    "Completed",
                    false,
                    "set-done",
                ))),
            ],
        );
        let mut state = ListState::new(Some(0));
        let area = Rect::new(0, 0, 24, 1);
        let mut buffer = Buffer::empty(area);
        list.widget(&mut state).render(area, &mut buffer);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        let pointer = list.ui_action_for_mouse(&mut state, &click).unwrap();
        let keyboard = list.items[0].primary_ui_action().unwrap();
        assert_eq!(pointer, keyboard);
        assert_eq!(pointer.node_id.as_str(), "todo-1-toggle");
        assert_eq!(pointer.value, crate::UiEventValue::Bool(true));

        let mut pressed = Buffer::empty(area);
        list.widget(&mut state).render(area, &mut pressed);
        assert!(pressed[(8, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn page_pointer_decision_remains_available_without_the_bridge() {
        let page = Page::with_gauge(
            "Gauge",
            Gauge::new("gauge", 0.5, "Half", "Half full").activate("open-gauge"),
        );
        let area = Rect::new(0, 0, 24, 8);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            page.pointer_decision(&mut ListState::default(), &click, area),
            Some(PagePointerDecision::Activate {
                node_id: "gauge",
                action: "open-gauge",
            })
        );
    }

    #[test]
    #[cfg(feature = "ui-bridge")]
    fn terminal_chart_clicks_emit_their_declared_actions() {
        let area = Rect::new(2, 3, 20, 5);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            Sparkline::new("spark", [1.0, 2.0], "Trend")
                .activate("open-spark")
                .ui_action_for_mouse(&click, area),
            Some(crate::UiAction::activate("spark", "open-spark"))
        );
        assert_eq!(
            BarChart::new("bars", [crate::BarChartBar::new("A", 1.0)], "One bar",)
                .activate("open-bars")
                .ui_action_for_mouse(&click, area),
            Some(crate::UiAction::activate("bars", "open-bars"))
        );
        assert_eq!(
            LineChart::new(
                "line",
                [crate::LineChartSeries::new(
                    "Series",
                    [crate::LineChartPoint::new(0.0, 1.0)],
                )],
                "One line",
            )
            .activate("open-line")
            .ui_action_for_mouse(&click, area),
            Some(crate::UiAction::activate("line", "open-line"))
        );
        assert_eq!(
            Gauge::new("gauge", 0.5, "Half", "Half full")
                .activate("open-gauge")
                .ui_action_for_mouse(&click, area),
            Some(crate::UiAction::activate("gauge", "open-gauge"))
        );
    }

    #[test]
    fn terminal_list_preserves_full_row_selection_insets_and_overflow_scrollbar() {
        let list = List::new(
            "files",
            (0..5)
                .map(|index| ListItem::new(format!("file-{index}"), format!("file-{index}.rs")))
                .collect(),
        );
        let mut state = ListState::new(Some(1));
        let backend = TestBackend::new(24, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(list.widget(&mut state), frame.area()))
            .unwrap();

        let rendered = terminal.backend().buffer();
        let selected = KitTheme::dark().selected_row.bg.unwrap();
        assert_eq!(rendered[(0, 1)].bg, selected);
        assert_eq!(rendered[(21, 1)].bg, selected);
        assert_eq!(rendered[(22, 1)].bg, selected);
        assert_ne!(rendered[(23, 1)].bg, selected);
        assert_eq!(rendered[(0, 1)].symbol(), " ");
        assert_eq!(rendered[(1, 1)].symbol(), " ");
        assert_eq!(rendered[(2, 1)].symbol(), "f");
        assert_eq!(rendered[(22, 1)].symbol(), " ");
        assert_ne!(rendered[(23, 0)].symbol(), " ");
        assert_eq!(state.rows_area(), Rect::new(0, 0, 23, 3));
        assert_eq!(
            state.item_at(ratatui::layout::Position::new(22, 1), 5),
            Some(1)
        );
        assert_eq!(
            state.item_at(ratatui::layout::Position::new(23, 1), 5),
            None
        );
    }

    #[test]
    fn terminal_list_right_aligns_values_and_drops_them_when_narrow() {
        let list = List::new(
            "changes",
            vec![
                ListItem::new("src", "src/lib.rs")
                    .leading(ListItemSlot::status(
                        StatusSymbol::new("M", "Modified")
                            .tone(ListItemTone::Warning)
                            .emphasis(ListItemEmphasis::Strong),
                    ))
                    .value("modified")
                    .value_min_width(24),
            ],
        );
        let mut state = ListState::new(Some(0));
        let mut wide = Buffer::empty(Rect::new(0, 0, 30, 1));
        list.widget(&mut state)
            .render(Rect::new(0, 0, 30, 1), &mut wide);
        let wide_text = (0..30).map(|x| wide[(x, 0)].symbol()).collect::<String>();
        assert!(wide_text.starts_with("  M  src/lib.rs"));
        assert_eq!(&wide_text[21..29], "modified");
        assert_eq!(&wide_text[29..], " ");

        let mut narrow_state = ListState::new(Some(0));
        let mut narrow = Buffer::empty(Rect::new(0, 0, 20, 1));
        list.widget(&mut narrow_state)
            .render(Rect::new(0, 0, 20, 1), &mut narrow);
        let narrow_text = (0..20).map(|x| narrow[(x, 0)].symbol()).collect::<String>();
        assert!(narrow_text.contains("src/lib.rs"));
        assert!(!narrow_text.contains("modified"));
    }

    #[test]
    fn terminal_list_renders_badges_and_busy_rows_from_closed_slots() {
        let list = List::new(
            "issues",
            vec![
                ListItem::new("loading", "Loading issues")
                    .busy(true)
                    .accessory(ListItemSlot::badge(
                        Badge::new("open").tone(ListItemTone::Success),
                    )),
            ],
        );
        let mut state = ListState::new(Some(0));
        state.set_spinner_frame(0);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 32, 1));
        list.widget(&mut state)
            .render(Rect::new(0, 0, 32, 1), &mut buffer);
        let text = (0..32).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert!(text.starts_with("  ⠋ Loading issues open"));
    }

    #[test]
    fn page_layout_exposes_the_same_named_slots_used_for_rendering() {
        let page = todo_page();
        let layout = page.layout(Rect::new(3, 5, 50, 10));
        assert_eq!(layout.title, Rect::new(3, 5, 50, 2));
        assert_eq!(layout.input, Some(Rect::new(3, 7, 50, 1)));
        assert_eq!(layout.list, Rect::new(3, 9, 50, 6));
    }

    #[test]
    fn page_input_renders_the_value_owned_by_the_component_spec() {
        let page = Page::new("Search", List::new("results", Vec::new())).input(
            Input::new("query", "Query")
                .value("one source of truth")
                .set_value_action("set-query"),
        );
        let mut input = InputField::new("stale renderer value");
        let mut state = ListState::new(None);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 48, 8));
        page.widget(&mut input, &mut state)
            .render(Rect::new(0, 0, 48, 8), &mut buffer);
        assert_eq!(input.text(), "one source of truth");
        let row = (0..48).map(|x| buffer[(x, 2)].symbol()).collect::<String>();
        assert!(row.contains("Query: one source of truth"));
    }

    #[test]
    fn page_input_inherits_the_pages_terminal_palette_and_spacing() {
        let page = Page::new("Search", List::new("results", Vec::new())).input(
            Input::new("query", "Query")
                .placeholder("Find a result")
                .set_value_action("set-query"),
        );
        let mut stale_theme = InputFieldTheme::dark();
        stale_theme.prompt = Style::new().fg(ratatui::style::Color::Red);
        stale_theme.placeholder = Style::new().fg(ratatui::style::Color::Red);
        stale_theme.left_padding = 0;
        let mut input = InputField::new("").with_theme(stale_theme);
        let mut state = ListState::new(None);
        let mut theme = PageTheme::for_theme(KitTheme::light());
        theme.detail = Style::new().fg(ratatui::style::Color::Green);
        theme.empty = Style::new().fg(ratatui::style::Color::Yellow);
        theme.left_padding = 2;
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 8));

        page.widget(&mut input, &mut state)
            .theme(theme)
            .render(buffer.area, &mut buffer);

        assert_eq!(buffer[(0, 2)].symbol(), " ");
        assert_eq!(buffer[(1, 2)].symbol(), " ");
        assert_eq!(buffer[(2, 2)].symbol(), "Q");
        assert_eq!(buffer[(2, 2)].fg, ratatui::style::Color::Green);
        assert_eq!(buffer[(9, 2)].symbol(), "F");
        assert_eq!(buffer[(9, 2)].fg, ratatui::style::Color::Yellow);
    }

    #[test]
    fn page_charts_share_the_pages_inset_and_semantic_palette() {
        let pages = [
            Page::with_sparkline(
                "Trend",
                Sparkline::new("trend", [1.0, 4.0, 2.0], "Trend values"),
            ),
            Page::with_bar_chart(
                "Bars",
                BarChart::new(
                    "bars",
                    [crate::BarChartBar::new("A", 2.0).emphasis(crate::BarChartEmphasis::Accent)],
                    "A is two",
                ),
            ),
            Page::with_line_chart(
                "Lines",
                LineChart::new(
                    "lines",
                    [crate::LineChartSeries::new(
                        "A",
                        [
                            crate::LineChartPoint::new(0.0, 0.0),
                            crate::LineChartPoint::new(1.0, 1.0),
                        ],
                    )],
                    "A rises",
                ),
            ),
            Page::with_gauge(
                "Gauge",
                Gauge::new("gauge", 0.75, "Ready", "75 percent ready"),
            ),
        ];
        let accent = ratatui::style::Color::Magenta;
        let mut theme = PageTheme::for_theme(KitTheme::dark());
        theme.accent = Style::new().fg(accent);

        for page in pages {
            let mut input = InputField::new("");
            let mut state = ListState::default();
            let mut buffer = Buffer::empty(Rect::new(0, 0, 42, 10));
            page.widget(&mut input, &mut state)
                .theme(theme)
                .render(buffer.area, &mut buffer);

            for y in 2..buffer.area.height {
                assert_eq!(buffer[(0, y)].symbol(), " ", "{} at row {y}", page.title);
                assert_eq!(buffer[(1, y)].symbol(), " ", "{} at row {y}", page.title);
            }
            assert!(
                buffer
                    .content()
                    .iter()
                    .any(|cell| !cell.symbol().trim().is_empty() && cell.fg == accent),
                "{} did not consume the Page accent",
                page.title
            );
        }
    }

    #[test]
    fn list_gauge_keeps_app_caption_and_draws_a_compact_terminal_meter() {
        let gauge = Gauge::new(
            "weekly-gauge",
            0.77,
            "7-day limit",
            "7-day limit: 77 percent left",
        )
        .caption("77% left · Resets in 5d 14h");
        let active = ListItem::new("active", "Quota")
            .trailing(ListItemSlot::gauge(gauge.clone().activate("open-quota")));
        assert_eq!(active.primary_role(), RowPrimaryRole::Command);
        assert!(active.validate("item").is_ok());
        assert!(
            ListItem::new("invalid-leading", "Quota")
                .leading(ListItemSlot::gauge(gauge.clone()))
                .validate("item")
                .is_err()
        );
        assert!(
            ListItem::new("app-caption", "Quota")
                .value("App-owned copy")
                .trailing(ListItemSlot::gauge(gauge.clone()))
                .validate("item")
                .is_ok()
        );
        let page = Page::new(
            "Usage",
            List::new(
                "metrics",
                vec![
                    ListItem::new("weekly", "7-day limit")
                        .value_tone(ListItemTone::Info)
                        .trailing(ListItemSlot::gauge(gauge)),
                ],
            ),
        );
        let mut input = InputField::new("");
        let mut state = ListState::default();
        let mut theme = PageTheme::for_theme(KitTheme::dark());
        theme.info = Style::new().fg(ratatui::style::Color::Magenta);
        theme.navigation = Style::new().fg(ratatui::style::Color::DarkGray);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 72, 7));

        page.widget(&mut input, &mut state)
            .theme(theme)
            .render(buffer.area, &mut buffer);

        let row = (0..72).map(|x| buffer[(x, 2)].symbol()).collect::<String>();
        assert!(row.contains("7-day limit"));
        assert!(row.contains("77% left · Resets in 5d 14h"));
        assert!((0..72).any(|x| {
            buffer[(x, 2)].symbol() == "─" && buffer[(x, 2)].fg == ratatui::style::Color::Magenta
        }));
        assert!((0..72).any(|x| {
            buffer[(x, 2)].symbol() == "─" && buffer[(x, 2)].fg == ratatui::style::Color::DarkGray
        }));
    }

    #[test]
    fn list_item_styled_runs_preserve_semantic_color_and_plain_fallback() {
        let gauge = Gauge::new(
            "fable-gauge",
            1.0,
            "Fable 7-day",
            "Fable 7-day is 100 percent used",
        )
        .caption("100% used");
        let item = ListItem::new("claude", "Claude")
            .label_runs([ListItemTextRun::new("Claude")
                .tone(ListItemTone::Accent)
                .emphasis(ListItemEmphasis::Strong)])
            .detail_runs([
                ListItemTextRun::new("5-hour 32% · ").tone(ListItemTone::Muted),
                ListItemTextRun::new("Fable 7-day 100% used").tone(ListItemTone::Danger),
            ])
            .value_runs([
                ListItemTextRun::new("Fable 7-day ").tone(ListItemTone::Muted),
                ListItemTextRun::new("100% used").tone(ListItemTone::Danger),
            ])
            .value_tone(ListItemTone::Danger)
            .trailing(ListItemSlot::gauge(gauge));
        item.validate("item").unwrap();
        assert_eq!(item.label, "Claude");
        assert_eq!(
            item.detail.as_deref(),
            Some("5-hour 32% · Fable 7-day 100% used")
        );
        assert_eq!(item.value.as_deref(), Some("Fable 7-day 100% used"));

        let page = Page::new("Usage", List::new("providers", vec![item]));
        assert!(
            page.required_capabilities()
                .contains(&LIST_ITEM_STYLED_TEXT_CAPABILITY)
        );
        let mut input = InputField::new("");
        let mut state = ListState::default();
        let mut theme = PageTheme::for_theme(KitTheme::dark());
        theme.danger = Style::new().fg(ratatui::style::Color::Red);
        theme.accent = Style::new().fg(ratatui::style::Color::Cyan);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 110, 7));
        page.widget(&mut input, &mut state)
            .theme(theme)
            .render(buffer.area, &mut buffer);

        let row = (0..110)
            .map(|x| buffer[(x, 2)].symbol())
            .collect::<String>();
        assert!(row.contains("Claude"));
        assert!(row.contains("Fable 7-day 100% used"));
        let red_text = (0..110).any(|x| {
            buffer[(x, 2)].symbol() == "1" && buffer[(x, 2)].fg == ratatui::style::Color::Red
        });
        assert!(
            red_text,
            "danger run must own its terminal foreground: {row}"
        );
        assert!((0..110).any(|x| {
            buffer[(x, 2)].symbol() == "─" && buffer[(x, 2)].fg == ratatui::style::Color::Red
        }));

        let mut invalid = ListItem::new("invalid", "Fallback");
        invalid.label_runs = vec![ListItemTextRun::new("Different")];
        assert!(invalid.validate("item").is_err());
    }

    #[test]
    fn master_detail_fields_require_explicit_renderer_capabilities() {
        let page = Page::new(
            "Usage",
            List::new(
                "providers",
                vec![
                    ListItem::new("codex", "Codex")
                        .detail("Pro")
                        .value("7-day 3% used")
                        .activate_action("open-provider"),
                ],
            ),
        )
        .back_action("close-provider");
        page.validate().unwrap();
        assert_eq!(
            page.required_capabilities(),
            vec![
                "page",
                "list",
                "listItem",
                "pageBack",
                "listItemMetadata",
                "listItemActivate",
                "listItemRole",
            ]
        );
    }

    #[test]
    fn list_item_roles_are_closed_and_render_their_terminal_affordances() {
        let page = Page::new(
            "Roles",
            List::new(
                "roles",
                vec![
                    ListItem::new("settings", "Settings").disclosure_action("open-settings"),
                    ListItem::new("dark", "Dark theme").checkmark(Checkmark::new(
                        "dark-checkmark",
                        "Dark theme selected",
                        true,
                        "set-dark",
                    )),
                    ListItem::new("refresh", "Refresh").command_action("refresh"),
                    ListItem::new("delete", "Delete workspace")
                        .destructive_action("delete-workspace"),
                    ListItem::new("version", "Version 1.0"),
                ],
            ),
        );
        page.validate().unwrap();
        assert_eq!(
            page.list()
                .items
                .iter()
                .map(ListItem::primary_role)
                .collect::<Vec<_>>(),
            vec![
                RowPrimaryRole::Disclosure,
                RowPrimaryRole::Checkmark,
                RowPrimaryRole::Command,
                RowPrimaryRole::Destructive,
                RowPrimaryRole::Static,
            ]
        );

        let mut state = ListState::new(Some(4));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 5));
        page.list()
            .widget(&mut state)
            .render(Rect::new(0, 0, 40, 5), &mut buffer);
        let row = |y| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(0).contains("Settings"));
        assert!(row(0).contains('›'));
        assert!(row(1).contains("Dark theme"));
        assert!(row(1).contains('✓'));
        assert_eq!(buffer[(2, 3)].fg, KitTheme::dark().danger);

        let ambiguous = ListItem::new("ambiguous", "Ambiguous")
            .trailing(ListItemSlot::toggle(Toggle::new(
                "toggle",
                "Toggle",
                false,
                "set-toggle",
            )))
            .activate_action("also-activate");
        assert!(ambiguous.validate("item").is_err());
    }

    #[test]
    fn component_mutations_preserve_done_toggle_invariant() {
        let mut page = todo_page();
        page.set_toggle_value("todo-1-toggle", false).unwrap();
        assert!(!page.list().items[0].done);
        let ListItemSlot::Toggle(toggle) = page.list().items[0].trailing.as_ref().unwrap() else {
            panic!("todo fixture has a Toggle")
        };
        assert!(!toggle.value);
    }
}
