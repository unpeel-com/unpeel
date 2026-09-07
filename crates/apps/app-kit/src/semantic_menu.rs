//! Closed semantic Menu vocabulary and its adapter to the existing Ratatui
//! [`crate::PopupMenu`] painter.
//!
//! A Menu is a bounded action list, not a generic popup container. Anchors are
//! presentation hints resolved in each renderer's own coordinate space.

use std::collections::HashSet;
use std::fmt;

use ratatui::layout::Position;
use serde::{Deserialize, Serialize};

use crate::{MenuItem, MenuTheme, PopupMenu};

pub const MENU_COMPONENT_CAPABILITY: &str = "menu";
pub const MENU_ANCHOR_CAPABILITY: &str = "menuAnchor";

const MAX_MENU_ITEMS: usize = 256;
const MAX_MENU_TEXT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SemanticMenuItemRole {
    #[default]
    Default,
    Danger,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SemanticMenuAnchor {
    #[default]
    Control,
    Caret,
    Pointer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SemanticMenuPresentation {
    #[default]
    Popup,
    Context,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticMenuItem {
    pub id: String,
    pub label: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "is_default_role")]
    pub role: SemanticMenuItemRole,
}

impl SemanticMenuItem {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            action: action.into(),
            hint: None,
            disabled: false,
            role: SemanticMenuItemRole::Default,
        }
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    #[must_use]
    pub const fn role(mut self, role: SemanticMenuItemRole) -> Self {
        self.role = role;
        self
    }

    #[must_use]
    pub const fn danger(self) -> Self {
        self.role(SemanticMenuItemRole::Danger)
    }
}

const fn is_default_role(role: &SemanticMenuItemRole) -> bool {
    matches!(role, SemanticMenuItemRole::Default)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticMenu {
    pub label: String,
    #[serde(default)]
    pub presentation: SemanticMenuPresentation,
    #[serde(default)]
    pub anchor: SemanticMenuAnchor,
    pub items: Vec<SemanticMenuItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismiss: Option<String>,
}

impl SemanticMenu {
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        items: impl IntoIterator<Item = SemanticMenuItem>,
    ) -> Self {
        Self {
            label: label.into(),
            presentation: SemanticMenuPresentation::Popup,
            anchor: SemanticMenuAnchor::Control,
            items: items.into_iter().collect(),
            selected_id: None,
            dismiss: None,
        }
    }

    #[must_use]
    pub const fn presentation(mut self, presentation: SemanticMenuPresentation) -> Self {
        self.presentation = presentation;
        self
    }

    #[must_use]
    pub const fn anchor(mut self, anchor: SemanticMenuAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    #[must_use]
    pub fn selected_id(mut self, selected_id: impl Into<String>) -> Self {
        self.selected_id = Some(selected_id.into());
        self
    }

    #[must_use]
    pub fn dismiss_action(mut self, action: impl Into<String>) -> Self {
        self.dismiss = Some(action.into());
        self
    }

    #[must_use]
    pub const fn required_capabilities(&self) -> [&'static str; 2] {
        [MENU_COMPONENT_CAPABILITY, MENU_ANCHOR_CAPABILITY]
    }

    pub fn validate(&self) -> Result<(), SemanticMenuValidationError> {
        validate_text(&self.label, "menu.label")?;
        if self.items.len() > MAX_MENU_ITEMS {
            return Err(SemanticMenuValidationError::new(
                "menu.items",
                format!("Menu accepts at most {MAX_MENU_ITEMS} items"),
            ));
        }
        if let Some(dismiss) = &self.dismiss {
            validate_identifier(dismiss, "menu.dismiss")?;
        }
        let mut ids = HashSet::new();
        for (index, item) in self.items.iter().enumerate() {
            let path = format!("menu.items[{index}]");
            validate_identifier(&item.id, &format!("{path}.id"))?;
            if !ids.insert(item.id.as_str()) {
                return Err(SemanticMenuValidationError::new(
                    format!("{path}.id"),
                    "Menu item ids must be unique",
                ));
            }
            validate_text(&item.label, &format!("{path}.label"))?;
            if let Some(hint) = &item.hint {
                validate_text(hint, &format!("{path}.hint"))?;
            }
            validate_identifier(&item.action, &format!("{path}.action"))?;
        }
        if let Some(selected_id) = &self.selected_id {
            let Some(selected) = self.items.iter().find(|item| item.id == *selected_id) else {
                return Err(SemanticMenuValidationError::new(
                    "menu.selectedId",
                    "selectedId must identify a Menu item",
                ));
            };
            if selected.disabled {
                return Err(SemanticMenuValidationError::new(
                    "menu.selectedId",
                    "a disabled Menu item cannot be selected",
                ));
            }
        }
        Ok(())
    }

    /// Reuses the existing popup painter exactly; the App supplies its local
    /// terminal-cell anchor while native/web resolve the semantic hint.
    #[must_use]
    pub fn popup(&self, anchor: Position, theme: MenuTheme) -> PopupMenu<String> {
        let mut popup = PopupMenu::new(
            anchor,
            self.items.iter().map(|item| {
                let label = item.hint.as_ref().map_or_else(
                    || item.label.clone(),
                    |hint| format!("{hint}  {}", item.label),
                );
                let row = MenuItem::new(label, item.id.clone());
                let row = if item.disabled { row.disabled() } else { row };
                if item.role == SemanticMenuItemRole::Danger {
                    row.danger()
                } else {
                    row
                }
            }),
        )
        .with_theme(theme);
        if let Some(selected_id) = &self.selected_id
            && let Some(index) = self.items.iter().position(|item| item.id == *selected_id)
        {
            popup.set_selected_index(index);
        }
        popup
    }

    #[must_use]
    pub fn item(&self, id: &str) -> Option<&SemanticMenuItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Builds the exact semantic action used by every Menu interpreter. A
    /// row/tree target remains an opaque App-owned id in the optional value.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action_for_item(&self, id: &str, target: Option<String>) -> Option<crate::UiAction> {
        let item = self.item(id).filter(|item| !item.disabled)?;
        Some(crate::UiAction::new(
            item.id.clone(),
            item.action.clone(),
            crate::UiEventKind::Activate,
            target.map_or(crate::UiEventValue::None, crate::UiEventValue::Text),
        ))
    }

    #[cfg(feature = "ui-bridge")]
    pub fn ui_action_for_popup_mouse(
        &self,
        popup: &mut PopupMenu<String>,
        event: &crossterm::event::MouseEvent,
        target: Option<String>,
    ) -> Option<crate::UiAction> {
        let index = popup.action_index_for_mouse(event)?;
        let id = popup.items().get(index)?.value();
        self.ui_action_for_item(id, target)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticMenuValidationError {
    pub path: String,
    pub message: String,
}

impl SemanticMenuValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for SemanticMenuValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for SemanticMenuValidationError {}

fn validate_identifier(value: &str, path: &str) -> Result<(), SemanticMenuValidationError> {
    if value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(SemanticMenuValidationError::new(
            path,
            "identifier must use 1...255 portable ASCII bytes",
        ));
    }
    Ok(())
}

fn validate_text(value: &str, path: &str) -> Result<(), SemanticMenuValidationError> {
    if value.len() > MAX_MENU_TEXT_BYTES || value.contains(['\n', '\r', '\0']) {
        return Err(SemanticMenuValidationError::new(
            path,
            "Menu text must be one line and at most 16 KiB",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_menu_reuses_popup_selection_and_roles() {
        let menu = SemanticMenu::new(
            "Actions",
            [
                SemanticMenuItem::new("open", "Open", "open"),
                SemanticMenuItem::new("disabled", "Unavailable", "disabled").disabled(true),
                SemanticMenuItem::new("delete", "Delete", "delete").danger(),
            ],
        )
        .selected_id("delete")
        .anchor(SemanticMenuAnchor::Pointer);
        menu.validate().unwrap();
        let popup = menu.popup(Position::new(2, 3), MenuTheme::dark());
        assert_eq!(popup.selected_value().map(String::as_str), Some("delete"));
        assert!(!popup.items()[1].is_enabled());
        assert_eq!(popup.items()[2].item_tone(), crate::MenuItemTone::Danger);
    }
}
