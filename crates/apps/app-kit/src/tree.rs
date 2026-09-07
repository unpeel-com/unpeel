//! Closed semantic Tree/Explorer vocabulary and its standalone Ratatui view.
//!
//! Filesystem ownership stays with the App. `TreeItem::id` is an opaque App
//! key; host paths and drag payloads never belong in this model.

#![cfg_attr(not(feature = "ui-bridge"), allow(dead_code))]

use std::collections::HashSet;
use std::fmt;

use crossterm::event::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::{
    BUTTON_COMPONENT_CAPABILITY, Button, ButtonRole, FOOTER_ACTIONS_CAPABILITY, FooterAction,
    FooterActions, InputField, InputFieldTheme, KitTheme, ListPageBehavior, RowBoundaryBehavior,
    RowNavigationState, SELECTABLE_LEFT_PADDING, SelectableRow, SemanticMenu, TerminalPointerPhase,
    TerminalPointerState, VerticalScrollbar,
};

pub const TREE_COMPONENT_CAPABILITY: &str = "tree";
pub const TREE_HIERARCHY_CAPABILITY: &str = "treeHierarchy";
pub const TREE_FILTER_CAPABILITY: &str = "treeFilter";
pub const TREE_PARENT_CAPABILITY: &str = "treeParent";

const MAX_TREE_NODES: usize = 100_000;
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_TEXT_BYTES: usize = 16 * 1024;
const MAX_FILTER_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TreePresentation {
    #[default]
    DrillDown,
    Outline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeItemKind {
    Parent,
    Directory,
    File,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeChildState {
    #[default]
    Loaded,
    Unloaded,
    Loading,
}

/// Backend-neutral result of clicking a rendered terminal Tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreePointerOutcome {
    Select(String),
    Activate(String),
    SetExpanded { item_id: String, expanded: bool },
    PrimaryAction,
    FooterAction(usize),
}

/// One bounded semantic entry. Only directories may own child entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeItem {
    pub id: String,
    pub label: String,
    pub kind: TreeItemKind,
    /// Muted secondary text after the label (a file's first heading, a size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub symlink: bool,
    #[serde(default)]
    pub child_state: TreeChildState,
    #[serde(default)]
    pub expanded: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TreeItem>,
}

impl TreeItem {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: TreeItemKind) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            detail: None,
            hidden: false,
            symlink: false,
            child_state: TreeChildState::Loaded,
            expanded: false,
            children: Vec::new(),
        }
    }

    /// Muted secondary text shown after the label.
    #[must_use]
    pub fn detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail.filter(|detail| !detail.is_empty());
        self
    }

    #[must_use]
    pub fn parent(id: impl Into<String>) -> Self {
        Self::new(id, "..", TreeItemKind::Parent)
    }

    #[must_use]
    pub fn directory(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(id, label, TreeItemKind::Directory)
    }

    #[must_use]
    pub fn file(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(id, label, TreeItemKind::File)
    }

    #[must_use]
    pub const fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    #[must_use]
    pub const fn symlink(mut self, symlink: bool) -> Self {
        self.symlink = symlink;
        self
    }

    #[must_use]
    pub const fn child_state(mut self, child_state: TreeChildState) -> Self {
        self.child_state = child_state;
        self
    }

    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = TreeItem>) -> Self {
        self.children = children.into_iter().collect();
        self.child_state = TreeChildState::Loaded;
        self
    }

    #[must_use]
    pub const fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = expanded;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeFilter {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    pub set_value: String,
}

impl TreeFilter {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
        set_value: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: value.into(),
            placeholder: String::new(),
            set_value: set_value.into(),
        }
    }

    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }
}

/// Actions owned by the App's reducer/router.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeActions {
    pub select: String,
    pub open: String,
    pub parent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_expanded: Option<String>,
}

impl TreeActions {
    pub const SELECT: &'static str = "tree-select";
    pub const OPEN: &'static str = "tree-open";
    pub const PARENT: &'static str = "tree-parent";
    pub const SET_EXPANDED: &'static str = "tree-set-expanded";

    #[must_use]
    pub fn drill_down() -> Self {
        Self {
            select: Self::SELECT.to_owned(),
            open: Self::OPEN.to_owned(),
            parent: Self::PARENT.to_owned(),
            set_expanded: None,
        }
    }

    #[must_use]
    pub fn outline() -> Self {
        Self {
            set_expanded: Some(Self::SET_EXPANDED.to_owned()),
            ..Self::drill_down()
        }
    }
}

impl Default for TreeActions {
    fn default() -> Self {
        Self::drill_down()
    }
}

/// One semantic file/document navigator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tree {
    pub label: String,
    pub location: String,
    #[serde(default)]
    pub presentation: TreePresentation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<TreeFilter>,
    #[serde(default)]
    pub items: Vec<TreeItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_message: Option<String>,
    /// One named, constrained toolbar action. This keeps document creation
    /// available in native renderers without turning Tree into a generic
    /// child container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_action: Option<Button>,
    /// Bounded actions for the selected/pointed entry. Renderers include that
    /// entry id with the chosen action; paths never enter the component tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_menu: Option<SemanticMenu>,
    #[serde(default)]
    pub actions: TreeActions,
    #[serde(default, skip_serializing_if = "FooterActions::is_empty")]
    pub footer: FooterActions,
}

impl Tree {
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        location: impl Into<String>,
        items: impl IntoIterator<Item = TreeItem>,
    ) -> Self {
        Self {
            label: label.into(),
            location: location.into(),
            presentation: TreePresentation::DrillDown,
            filter: None,
            items: items.into_iter().collect(),
            selected_id: None,
            empty_message: None,
            primary_action: None,
            context_menu: None,
            actions: TreeActions::drill_down(),
            footer: FooterActions::default(),
        }
    }

    #[must_use]
    pub fn presentation(mut self, presentation: TreePresentation) -> Self {
        self.presentation = presentation;
        self.actions = match presentation {
            TreePresentation::DrillDown => TreeActions::drill_down(),
            TreePresentation::Outline => TreeActions::outline(),
        };
        self
    }

    #[must_use]
    pub fn outline(mut self) -> Self {
        self.presentation = TreePresentation::Outline;
        self.actions = TreeActions::outline();
        self
    }

    #[must_use]
    pub fn filter(mut self, filter: TreeFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    #[must_use]
    pub fn selected_id(mut self, selected_id: impl Into<String>) -> Self {
        self.selected_id = Some(selected_id.into());
        self
    }

    #[must_use]
    pub fn empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = Some(message.into());
        self
    }

    #[must_use]
    pub fn primary_action(mut self, action: Button) -> Self {
        self.primary_action = Some(action);
        self
    }

    #[must_use]
    pub fn context_menu(mut self, menu: SemanticMenu) -> Self {
        self.context_menu = Some(menu);
        self
    }

    #[must_use]
    pub fn actions(mut self, actions: TreeActions) -> Self {
        self.actions = actions;
        self
    }

    #[must_use]
    pub fn footer_actions(mut self, actions: impl IntoIterator<Item = FooterAction>) -> Self {
        self.footer = FooterActions::new(actions);
        self
    }

    #[must_use]
    pub fn required_capabilities(&self) -> Vec<&'static str> {
        let mut capabilities = vec![TREE_COMPONENT_CAPABILITY];
        if self.presentation == TreePresentation::Outline
            || self.items.iter().any(|item| !item.children.is_empty())
        {
            capabilities.push(TREE_HIERARCHY_CAPABILITY);
        }
        if self.filter.is_some() {
            capabilities.push(TREE_FILTER_CAPABILITY);
        }
        if self
            .items
            .iter()
            .any(|item| item.kind == TreeItemKind::Parent)
        {
            capabilities.push(TREE_PARENT_CAPABILITY);
        }
        if self.primary_action.is_some() {
            capabilities.push(BUTTON_COMPONENT_CAPABILITY);
        }
        if self.context_menu.is_some() {
            capabilities.extend([
                crate::MENU_COMPONENT_CAPABILITY,
                crate::MENU_ANCHOR_CAPABILITY,
            ]);
        }
        if !self.footer.is_empty() {
            capabilities.push(FOOTER_ACTIONS_CAPABILITY);
        }
        capabilities
    }

    pub fn validate(&self) -> Result<(), TreeValidationError> {
        validate_text(&self.label, MAX_TREE_TEXT_BYTES, "tree.label")?;
        validate_text(&self.location, MAX_TREE_TEXT_BYTES, "tree.location")?;
        if let Some(message) = &self.empty_message {
            validate_text(message, MAX_TREE_TEXT_BYTES, "tree.emptyMessage")?;
        }
        if let Some(action) = &self.primary_action {
            action
                .validate("tree.primaryAction")
                .map_err(|error| TreeValidationError::new(error.path, error.message))?;
        }
        if let Some(menu) = &self.context_menu {
            menu.validate().map_err(|error| {
                TreeValidationError::new(
                    format!(
                        "tree.contextMenu.{}",
                        error.path.trim_start_matches("menu.")
                    ),
                    error.message,
                )
            })?;
        }
        self.footer
            .validate("tree.footer")
            .map_err(|error| TreeValidationError::new(error.path, error.message))?;
        validate_identifier(&self.actions.select, "tree.actions.select")?;
        validate_identifier(&self.actions.open, "tree.actions.open")?;
        validate_identifier(&self.actions.parent, "tree.actions.parent")?;
        if let Some(action) = &self.actions.set_expanded {
            validate_identifier(action, "tree.actions.setExpanded")?;
        }
        if self.presentation == TreePresentation::Outline && self.actions.set_expanded.is_none() {
            return Err(TreeValidationError::new(
                "tree.actions.setExpanded",
                "outline Trees require an idempotent expansion action",
            ));
        }
        if let Some(filter) = &self.filter {
            validate_identifier(&filter.id, "tree.filter.id")?;
            validate_text(&filter.label, MAX_TREE_TEXT_BYTES, "tree.filter.label")?;
            validate_text(&filter.value, MAX_FILTER_BYTES, "tree.filter.value")?;
            validate_text(
                &filter.placeholder,
                MAX_TREE_TEXT_BYTES,
                "tree.filter.placeholder",
            )?;
            validate_identifier(&filter.set_value, "tree.filter.setValue")?;
        }

        let mut ids = HashSet::new();
        let mut count = 0;
        let mut parent_count = 0;
        validate_items(
            &self.items,
            0,
            "tree.items",
            &mut ids,
            &mut count,
            &mut parent_count,
        )?;
        for (index, action) in self.footer.actions.iter().enumerate() {
            if !ids.insert(action.id.clone()) {
                return Err(TreeValidationError::new(
                    format!("tree.footer.actions[{index}].id"),
                    format!("duplicate Tree id {:?}", action.id),
                ));
            }
        }
        if parent_count > 1 {
            return Err(TreeValidationError::new(
                "tree.items",
                "a Tree accepts at most one synthetic parent item",
            ));
        }
        if let Some(selected) = &self.selected_id
            && !ids.contains(selected)
        {
            return Err(TreeValidationError::new(
                "tree.selectedId",
                format!("selected item {selected:?} is not present"),
            ));
        }
        Ok(())
    }

    pub(crate) fn set_selection(
        &mut self,
        selected_id: Option<String>,
    ) -> Result<(), TreeValidationError> {
        self.selected_id = selected_id;
        self.validate()
    }

    pub(crate) fn set_filter_value(
        &mut self,
        filter_id: &str,
        value: String,
    ) -> Result<(), TreeValidationError> {
        let Some(filter) = self.filter.as_mut().filter(|filter| filter.id == filter_id) else {
            return Err(TreeValidationError::new(
                "delta.filterId",
                format!("filter {filter_id:?} is not present"),
            ));
        };
        filter.value = value;
        self.validate()
    }

    pub(crate) fn splice_children(
        &mut self,
        parent_id: Option<&str>,
        index: usize,
        delete_count: usize,
        items: Vec<TreeItem>,
    ) -> Result<(), TreeValidationError> {
        let target = if let Some(parent_id) = parent_id {
            let Some(parent) = find_item_mut(&mut self.items, parent_id) else {
                return Err(TreeValidationError::new(
                    "delta.parentId",
                    format!("parent {parent_id:?} is not present"),
                ));
            };
            if parent.kind != TreeItemKind::Directory {
                return Err(TreeValidationError::new(
                    "delta.parentId",
                    "only a directory may own Tree children",
                ));
            }
            &mut parent.children
        } else {
            &mut self.items
        };
        if index > target.len() || delete_count > target.len().saturating_sub(index) {
            return Err(TreeValidationError::new(
                "delta.index",
                "Tree child splice is outside the current child collection",
            ));
        }
        target.splice(index..index + delete_count, items);
        self.validate()
    }

    pub(crate) fn set_child_state(
        &mut self,
        item_id: &str,
        child_state: TreeChildState,
    ) -> Result<(), TreeValidationError> {
        let Some(item) = find_item_mut(&mut self.items, item_id) else {
            return Err(TreeValidationError::new(
                "delta.itemId",
                format!("item {item_id:?} is not present"),
            ));
        };
        item.child_state = child_state;
        if child_state != TreeChildState::Loaded {
            item.children.clear();
        }
        self.validate()
    }

    pub(crate) fn set_expanded(
        &mut self,
        item_id: &str,
        expanded: bool,
    ) -> Result<(), TreeValidationError> {
        let Some(item) = find_item_mut(&mut self.items, item_id) else {
            return Err(TreeValidationError::new(
                "delta.itemId",
                format!("item {item_id:?} is not present"),
            ));
        };
        item.expanded = expanded;
        self.validate()
    }

    #[must_use]
    pub fn widget<'a>(&'a self, state: &'a mut TreeState) -> TreeWidget<'a> {
        TreeWidget {
            tree: self,
            state,
            filter_input: None,
            theme: TreeTheme::default(),
            filter_theme: None,
        }
    }

    /// Uses the exact Tree interpreter while preserving a renderer-local text
    /// cursor, selection, and IME surface for the optional filter slot.
    #[must_use]
    pub fn widget_with_filter<'a>(
        &'a self,
        state: &'a mut TreeState,
        filter_input: &'a mut InputField,
    ) -> TreeWidget<'a> {
        TreeWidget {
            tree: self,
            state,
            filter_input: Some(filter_input),
            theme: TreeTheme::default(),
            filter_theme: None,
        }
    }

    #[must_use]
    fn item(&self, id: &str) -> Option<&TreeItem> {
        find_item(&self.items, id)
    }

    /// Converts a terminal Tree decision into the exact hosted action emitted
    /// by the Swift and web interpreters.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action_for_pointer_outcome(
        &self,
        node_id: impl Into<crate::NodeId>,
        outcome: &TreePointerOutcome,
    ) -> Option<crate::UiAction> {
        let node_id = node_id.into();
        match outcome {
            TreePointerOutcome::Select(item_id) => Some(crate::UiAction::new(
                node_id,
                self.actions.select.clone(),
                crate::UiEventKind::Select,
                crate::UiEventValue::Text(item_id.clone()),
            )),
            TreePointerOutcome::Activate(item_id) => {
                let item = self.item(item_id)?;
                if item.kind == TreeItemKind::Parent {
                    Some(crate::UiAction::new(
                        node_id,
                        self.actions.parent.clone(),
                        crate::UiEventKind::Cancel,
                        crate::UiEventValue::None,
                    ))
                } else {
                    Some(crate::UiAction::new(
                        node_id,
                        self.actions.open.clone(),
                        crate::UiEventKind::Activate,
                        crate::UiEventValue::Text(item_id.clone()),
                    ))
                }
            }
            TreePointerOutcome::SetExpanded { item_id, expanded } => Some(crate::UiAction::new(
                node_id,
                self.actions.set_expanded.clone()?,
                crate::UiEventKind::Change,
                crate::UiEventValue::TextList(vec![item_id.clone(), expanded.to_string()]),
            )),
            TreePointerOutcome::PrimaryAction => self
                .primary_action
                .as_ref()
                .map(|action| crate::UiAction::activate(action.id.clone(), action.action.clone())),
            TreePointerOutcome::FooterAction(index) => {
                self.footer.actions.get(*index).map(FooterAction::ui_action)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeValidationError {
    pub path: String,
    pub message: String,
}

impl TreeValidationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for TreeValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for TreeValidationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeTheme {
    pub style: Style,
    pub location: Style,
    pub filter: Style,
    pub item: Style,
    pub directory: Style,
    pub parent: Style,
    pub symlink: Style,
    pub selected: Style,
    /// Pointer hover on an unselected row; distinct from `selected`.
    pub hovered: Style,
    pub empty: Style,
    pub scrollbar_track: Style,
    pub scrollbar_thumb: Style,
    pub left_padding: u16,
}

impl TreeTheme {
    #[must_use]
    pub const fn for_theme(theme: KitTheme) -> Self {
        Self {
            style: Style::new(),
            location: Style::new().fg(theme.text).bold(),
            filter: Style::new().fg(theme.muted),
            item: Style::new().fg(theme.text),
            directory: Style::new().fg(theme.accent),
            parent: Style::new().fg(theme.accent),
            symlink: Style::new().fg(Color::Cyan),
            selected: theme.selected_row,
            hovered: theme.hovered_row,
            empty: Style::new().fg(theme.subtle),
            scrollbar_track: theme.scrollbar_track,
            scrollbar_thumb: theme.scrollbar_thumb,
            left_padding: SELECTABLE_LEFT_PADDING,
        }
    }

    /// Input styling for the Tree's named filter slot, derived from the same
    /// palette and horizontal rhythm as its rows.
    #[must_use]
    pub const fn input_theme(self) -> InputFieldTheme {
        InputFieldTheme {
            style: self.style,
            text: self.filter,
            focused: self.item.bold(),
            placeholder: self.empty,
            prompt: self.filter,
            selection: self.selected,
            left_padding: self.left_padding,
        }
    }
}

impl Default for TreeTheme {
    fn default() -> Self {
        Self::for_theme(KitTheme::dark())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeState {
    navigation: RowNavigationState,
    rows_area: Rect,
    primary_action_area: Rect,
    footer_area: Rect,
    visible_ids: Vec<String>,
    disclosure_areas: Vec<(String, Rect)>,
}

impl Default for TreeState {
    fn default() -> Self {
        let mut navigation = RowNavigationState::default();
        navigation.set_boundary_behavior(RowBoundaryBehavior::Wrap);
        navigation.set_navigation(1, 0, ListPageBehavior::Selection);
        Self {
            navigation,
            rows_area: Rect::default(),
            primary_action_area: Rect::default(),
            footer_area: Rect::default(),
            visible_ids: Vec::new(),
            disclosure_areas: Vec::new(),
        }
    }
}

impl TreeState {
    #[must_use]
    pub fn selected_id(&self) -> Option<&str> {
        self.navigation
            .selected()
            .and_then(|index| self.visible_ids.get(index))
            .map(String::as_str)
    }

    #[must_use]
    pub fn item_id_at(&self, position: ratatui::layout::Position) -> Option<&str> {
        self.navigation
            .item_at(position, self.visible_ids.len())
            .and_then(|index| self.visible_ids.get(index))
            .map(String::as_str)
    }

    #[must_use]
    pub const fn primary_action_at(&self, position: ratatui::layout::Position) -> bool {
        self.primary_action_area.contains(position)
    }

    #[must_use]
    pub const fn primary_action_area(&self) -> Rect {
        self.primary_action_area
    }

    #[must_use]
    pub fn footer_action_at<'a>(
        &self,
        tree: &'a Tree,
        position: ratatui::layout::Position,
    ) -> Option<&'a FooterAction> {
        tree.footer.action_at(position, self.footer_area)
    }

    #[must_use]
    pub const fn footer_area(&self) -> Rect {
        self.footer_area
    }

    #[must_use]
    pub const fn rows_area(&self) -> Rect {
        self.rows_area
    }

    #[must_use]
    pub const fn pointer(&self) -> TerminalPointerState {
        self.navigation.pointer()
    }

    pub const fn set_pointer(&mut self, pointer: TerminalPointerState) {
        self.navigation.set_pointer(pointer);
    }

    pub fn track_mouse(&mut self, event: &MouseEvent) -> bool {
        self.navigation.track_mouse(event)
    }

    /// Resolves all Tree terminal hit regions from the most recent render.
    /// Single click selects, double click invokes the same action as Enter,
    /// and an outline disclosure changes only the expanded state.
    pub fn pointer_decision(
        &mut self,
        tree: &Tree,
        event: &MouseEvent,
        clicks: &mut crate::DoubleClickTracker<String>,
    ) -> Option<TreePointerOutcome> {
        self.track_mouse(event);
        let position = TerminalPointerState::click_position(event)?;
        if self.primary_action_at(position) && tree.primary_action.is_some() {
            clicks.reset();
            return Some(TreePointerOutcome::PrimaryAction);
        }
        if let Some(action) = self.footer_action_at(tree, position) {
            clicks.reset();
            return tree
                .footer
                .actions
                .iter()
                .position(|candidate| candidate.id == action.id)
                .map(TreePointerOutcome::FooterAction);
        }
        if let Some((item_id, _)) = self
            .disclosure_areas
            .iter()
            .find(|(_, area)| area.contains(position))
            && let Some(item) = tree.item(item_id)
        {
            clicks.reset();
            return Some(TreePointerOutcome::SetExpanded {
                item_id: item_id.clone(),
                expanded: !item.expanded,
            });
        }
        let Some(index) = self.navigation.item_at(position, self.visible_ids.len()) else {
            clicks.reset();
            return None;
        };
        let item_id = self.visible_ids[index].clone();
        if clicks.click(item_id.clone()) {
            self.navigation.select(Some(index), self.visible_ids.len());
            Some(TreePointerOutcome::Activate(item_id))
        } else if self.navigation.select(Some(index), self.visible_ids.len()) {
            Some(TreePointerOutcome::Select(item_id))
        } else {
            None
        }
    }

    #[cfg(feature = "ui-bridge")]
    pub fn ui_action_for_mouse(
        &mut self,
        tree: &Tree,
        node_id: impl Into<crate::NodeId>,
        event: &MouseEvent,
        clicks: &mut crate::DoubleClickTracker<String>,
    ) -> Option<crate::UiAction> {
        let outcome = self.pointer_decision(tree, event, clicks)?;
        tree.ui_action_for_pointer_outcome(node_id, &outcome)
    }

    #[must_use]
    pub const fn offset(&self) -> usize {
        self.navigation.offset()
    }

    #[must_use]
    pub const fn viewport_rows(&self) -> usize {
        self.rows_area.height as usize
    }
}

pub struct TreeWidget<'a> {
    tree: &'a Tree,
    state: &'a mut TreeState,
    filter_input: Option<&'a mut InputField>,
    theme: TreeTheme,
    filter_theme: Option<InputFieldTheme>,
}

impl TreeWidget<'_> {
    #[must_use]
    pub const fn theme(mut self, theme: TreeTheme) -> Self {
        self.theme = theme;
        self
    }

    /// Overrides the Tree-derived terminal style for its filter Input.
    #[must_use]
    pub const fn filter_theme(mut self, theme: InputFieldTheme) -> Self {
        self.filter_theme = Some(theme);
        self
    }
}

impl Widget for TreeWidget<'_> {
    fn render(mut self, area: Rect, buffer: &mut Buffer) {
        buffer.set_style(area, self.theme.style);
        if area.is_empty() {
            self.state.rows_area = Rect::default();
            self.state.primary_action_area = Rect::default();
            self.state.footer_area = Rect::default();
            self.state.visible_ids.clear();
            self.state.disclosure_areas.clear();
            return;
        }
        let filter_height = u16::from(self.tree.filter.is_some() && area.height > 0);
        if let Some(filter) = &self.tree.filter {
            let row = Rect::new(area.x, area.y, area.width, filter_height);
            if let Some(input) = self.filter_input.as_mut() {
                if input.text() != filter.value {
                    input.set_text(filter.value.clone());
                }
                input.set_placeholder(filter.placeholder.clone());
                input.set_prompt(format!("{}: ", filter.label));
                let mut theme = self
                    .filter_theme
                    .unwrap_or_else(|| self.theme.input_theme());
                if input.focused() {
                    // Focused filter rows take the shared selection background.
                    theme.style = self.theme.style.patch(self.theme.selected);
                }
                input.set_theme(theme);
                input.widget().render(row, buffer);
            } else {
                let value = if filter.value.is_empty() {
                    filter.placeholder.as_str()
                } else {
                    filter.value.as_str()
                };
                Line::styled(
                    format!(
                        "{}{}: {}",
                        " ".repeat(usize::from(self.theme.left_padding)),
                        filter.label,
                        value
                    ),
                    self.theme.filter,
                )
                .render(row, buffer);
            }
        }
        // The location row is the screen title; keep one padding row under
        // it whenever there is room.
        let location_height = match area.height.saturating_sub(filter_height) {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        let location_area = Rect::new(
            area.x,
            area.y.saturating_add(filter_height),
            area.width,
            location_height.min(1),
        );
        Line::styled(
            format!(
                "{}{}",
                " ".repeat(usize::from(self.theme.left_padding)),
                self.tree.location
            ),
            self.theme.location,
        )
        .render(location_area, buffer);

        let mut visible = Vec::new();
        flatten_visible(&self.tree.items, 0, self.tree.presentation, &mut visible);
        self.state.visible_ids = visible.iter().map(|(item, _)| item.id.clone()).collect();
        self.state.disclosure_areas.clear();
        let rows_y = area
            .y
            .saturating_add(filter_height)
            .saturating_add(location_height);
        let action_height = u16::from(self.tree.primary_action.is_some());
        let footer_height = u16::from(!self.tree.footer.is_empty());
        let rows_height = area
            .height
            .saturating_sub(filter_height + location_height + action_height + footer_height);
        let overflow = visible.len() > usize::from(rows_height);
        self.state.rows_area = Rect::new(
            area.x,
            rows_y,
            area.width.saturating_sub(u16::from(overflow)),
            rows_height,
        );
        self.state
            .navigation
            .prepare(self.state.rows_area, visible.len());
        let selected = self
            .tree
            .selected_id
            .as_deref()
            .and_then(|id| visible.iter().position(|(item, _)| item.id == id));
        self.state.navigation.select(selected, visible.len());

        self.state.primary_action_area = if let Some(action) = &self.tree.primary_action {
            let action_area = Rect::new(
                area.x,
                area.bottom().saturating_sub(footer_height + 1),
                area.width,
                action_height,
            );
            let mut style = match action.role {
                ButtonRole::Default => self.theme.directory,
                ButtonRole::Primary => self.theme.directory.bold(),
                ButtonRole::Destructive => Style::new().fg(Color::Red).bold(),
            };
            style = match self.state.pointer().phase(action_area) {
                TerminalPointerPhase::Idle => style,
                TerminalPointerPhase::Hovered => style.patch(
                    self.theme
                        .selected
                        .add_modifier(ratatui::style::Modifier::DIM),
                ),
                TerminalPointerPhase::Pressed => style.patch(
                    self.theme
                        .selected
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            };
            Line::styled(
                format!(
                    "{}[ {} ]",
                    " ".repeat(usize::from(self.theme.left_padding)),
                    action.label
                ),
                style,
            )
            .render(action_area, buffer);
            action_area
        } else {
            Rect::default()
        };
        self.state.footer_area = if footer_height > 0 {
            let footer_area = Rect::new(
                area.x,
                area.bottom().saturating_sub(footer_height),
                area.width,
                footer_height,
            );
            self.tree
                .footer
                .widget()
                .styles(
                    self.theme.style,
                    self.theme.directory.bold(),
                    self.theme.filter,
                    Style::new().fg(Color::Red),
                    self.theme.empty,
                )
                .pointer(self.state.pointer())
                .interaction_styles(
                    self.theme.hovered,
                    self.theme
                        .selected
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )
                .render(footer_area, buffer);
            footer_area
        } else {
            Rect::default()
        };

        if visible.is_empty() {
            if let Some(message) = &self.tree.empty_message {
                Line::styled(
                    format!(
                        "{}{}",
                        " ".repeat(usize::from(self.theme.left_padding)),
                        message
                    ),
                    self.theme.empty,
                )
                .render(self.state.rows_area, buffer);
            }
            return;
        }

        for (slot, index) in (self.state.navigation.offset()..visible.len())
            .take(usize::from(rows_height))
            .enumerate()
        {
            let (item, depth) = visible[index];
            let row = Rect::new(
                self.state.rows_area.x,
                self.state.rows_area.y.saturating_add(slot as u16),
                self.state.rows_area.width,
                1,
            );
            let selected = self.state.navigation.selected() == Some(index);
            let pointer_phase = self.state.navigation.pointer_phase_at(index);
            let highlighted = selected || pointer_phase != TerminalPointerPhase::Idle;
            let mut inactive = match item.kind {
                TreeItemKind::Parent => self.theme.parent,
                TreeItemKind::Directory => self.theme.directory,
                TreeItemKind::File => self.theme.item,
            };
            if item.symlink {
                inactive = self.theme.symlink;
            }
            let active = inactive.patch(match pointer_phase {
                TerminalPointerPhase::Idle | TerminalPointerPhase::Hovered if selected => {
                    self.theme.selected
                }
                TerminalPointerPhase::Idle => self.theme.selected,
                TerminalPointerPhase::Hovered => self.theme.hovered,
                TerminalPointerPhase::Pressed => self
                    .theme
                    .selected
                    .add_modifier(ratatui::style::Modifier::BOLD),
            });
            let content = SelectableRow::new(highlighted, active)
                .inactive_style(inactive)
                .left_padding(self.theme.left_padding)
                .right_padding(0)
                .paint(row, buffer);
            let indent = if self.tree.presentation == TreePresentation::Outline {
                "  ".repeat(depth)
            } else {
                String::new()
            };
            if self.tree.presentation == TreePresentation::Outline
                && item.kind == TreeItemKind::Directory
            {
                let indent_width = u16::try_from(depth.saturating_mul(2)).unwrap_or(u16::MAX);
                self.state.disclosure_areas.push((
                    item.id.clone(),
                    Rect::new(
                        content.x.saturating_add(indent_width),
                        content.y,
                        2.min(content.width.saturating_sub(indent_width)),
                        1,
                    ),
                ));
            }
            let marker = match item.kind {
                TreeItemKind::Parent => "../".to_owned(),
                TreeItemKind::Directory => {
                    let disclosure = if self.tree.presentation == TreePresentation::Outline {
                        if item.expanded { "▾ " } else { "▸ " }
                    } else {
                        ""
                    };
                    format!("{disclosure}{}/", item.label.trim_end_matches('/'))
                }
                TreeItemKind::File => item.label.clone(),
            };
            let label_style = if highlighted { active } else { inactive };
            let label = format!("{indent}{marker}");
            let mut spans = vec![Span::styled(label.clone(), label_style)];
            if let Some(detail) = &item.detail {
                let remaining = usize::from(content.width)
                    .saturating_sub(UnicodeWidthStr::width(label.as_str()))
                    .saturating_sub(2);
                if remaining > 0 {
                    let detail_style = if highlighted {
                        active.add_modifier(ratatui::style::Modifier::DIM)
                    } else {
                        self.theme.style.patch(self.theme.filter)
                    };
                    let clipped = detail
                        .chars()
                        .scan(0usize, |used, character| {
                            *used +=
                                UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]) as &str);
                            (*used <= remaining).then_some(character)
                        })
                        .collect::<String>();
                    spans.push(Span::styled(format!("  {clipped}"), detail_style));
                }
            }
            Line::from(spans).render(content, buffer);
        }
        if overflow {
            VerticalScrollbar::new(
                visible.len(),
                usize::from(rows_height),
                self.state.navigation.offset(),
            )
            .track_style(self.theme.scrollbar_track)
            .thumb_style(self.theme.scrollbar_thumb)
            .render(
                Rect::new(area.right().saturating_sub(1), rows_y, 1, rows_height),
                buffer,
            );
        }
    }
}

fn validate_items(
    items: &[TreeItem],
    depth: usize,
    path: &str,
    ids: &mut HashSet<String>,
    count: &mut usize,
    parent_count: &mut usize,
) -> Result<(), TreeValidationError> {
    if depth > MAX_TREE_DEPTH {
        return Err(TreeValidationError::new(
            path,
            format!("Tree depth exceeds {MAX_TREE_DEPTH}"),
        ));
    }
    for (index, item) in items.iter().enumerate() {
        *count = count.saturating_add(1);
        if *count > MAX_TREE_NODES {
            return Err(TreeValidationError::new(
                path,
                format!("Tree node count exceeds {MAX_TREE_NODES}"),
            ));
        }
        let item_path = format!("{path}[{index}]");
        validate_identifier(&item.id, &format!("{item_path}.id"))?;
        if !ids.insert(item.id.clone()) {
            return Err(TreeValidationError::new(
                format!("{item_path}.id"),
                format!("duplicate Tree item id {:?}", item.id),
            ));
        }
        validate_text(
            &item.label,
            MAX_TREE_TEXT_BYTES,
            &format!("{item_path}.label"),
        )?;
        if item.label.contains(['\n', '\r']) {
            return Err(TreeValidationError::new(
                format!("{item_path}.label"),
                "Tree labels must be single-line",
            ));
        }
        if let Some(detail) = &item.detail {
            validate_text(detail, MAX_TREE_TEXT_BYTES, &format!("{item_path}.detail"))?;
            if detail.contains(['\n', '\r']) {
                return Err(TreeValidationError::new(
                    format!("{item_path}.detail"),
                    "Tree details must be single-line",
                ));
            }
        }
        match item.kind {
            TreeItemKind::Parent => {
                *parent_count += 1;
                if depth != 0 || !item.children.is_empty() || item.expanded {
                    return Err(TreeValidationError::new(
                        item_path,
                        "the synthetic parent must be a root leaf",
                    ));
                }
            }
            TreeItemKind::File => {
                if !item.children.is_empty() || item.expanded {
                    return Err(TreeValidationError::new(
                        item_path,
                        "files cannot own or expand Tree children",
                    ));
                }
            }
            TreeItemKind::Directory => {
                if item.child_state != TreeChildState::Loaded && !item.children.is_empty() {
                    return Err(TreeValidationError::new(
                        format!("{item_path}.children"),
                        "unloaded/loading directories cannot contain snapshot children",
                    ));
                }
                validate_items(
                    &item.children,
                    depth + 1,
                    &format!("{item_path}.children"),
                    ids,
                    count,
                    parent_count,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, path: &str) -> Result<(), TreeValidationError> {
    if value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(TreeValidationError::new(
            path,
            "identifier must contain 1...255 ASCII alphanumeric, '.', '_', '-', or ':' bytes",
        ));
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize, path: &str) -> Result<(), TreeValidationError> {
    if value.len() > maximum {
        return Err(TreeValidationError::new(
            path,
            format!("text exceeds {maximum} UTF-8 bytes"),
        ));
    }
    if value.chars().any(|character| {
        matches!(character, '\u{0000}'..='\u{0008}' | '\u{000B}' | '\u{000C}' | '\u{000E}'..='\u{001F}' | '\u{007F}')
    }) {
        return Err(TreeValidationError::new(path, "text contains a control character"));
    }
    Ok(())
}

fn find_item_mut<'a>(items: &'a mut [TreeItem], id: &str) -> Option<&'a mut TreeItem> {
    for item in items {
        if item.id == id {
            return Some(item);
        }
        if let Some(found) = find_item_mut(&mut item.children, id) {
            return Some(found);
        }
    }
    None
}

fn find_item<'a>(items: &'a [TreeItem], id: &str) -> Option<&'a TreeItem> {
    for item in items {
        if item.id == id {
            return Some(item);
        }
        if let Some(found) = find_item(&item.children, id) {
            return Some(found);
        }
    }
    None
}

fn flatten_visible<'a>(
    items: &'a [TreeItem],
    depth: usize,
    presentation: TreePresentation,
    output: &mut Vec<(&'a TreeItem, usize)>,
) {
    for item in items {
        output.push((item, depth));
        if presentation == TreePresentation::Outline && item.expanded {
            flatten_visible(&item.children, depth + 1, presentation, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn fixture() -> Tree {
        Tree::new(
            "Notes",
            "Writing",
            [
                TreeItem::parent("parent"),
                TreeItem::directory("projects", "Projects")
                    .children([TreeItem::file("readme", "README.md")])
                    .expanded(true),
                TreeItem::file("today", "Today.md"),
            ],
        )
        .outline()
        .filter(TreeFilter::new("filter", "Filter notes", "", "tree-filter"))
        .selected_id("today")
    }

    #[test]
    fn validates_closed_hierarchy_and_unique_opaque_ids() {
        fixture().validate().unwrap();
        let invalid = Tree::new(
            "Files",
            ".",
            [TreeItem::file("same", "a").children([TreeItem::file("same", "b")])],
        );
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn terminal_interpretation_uses_shared_selection_and_outline_rows() {
        let tree = fixture();
        let mut state = TreeState::default();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(tree.widget(&mut state), frame.area()))
            .unwrap();
        assert_eq!(state.selected_id(), Some("today"));
        let rendered = terminal.backend().buffer();
        assert!(rendered.content.iter().any(|cell| cell.symbol() == "▾"));
    }

    #[test]
    fn tree_pointer_uses_selection_double_activation_and_disclosure_actions() {
        let tree = fixture();
        let mut state = TreeState::default();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(tree.widget(&mut state), frame.area()))
            .unwrap();
        let mut clicks = crate::DoubleClickTracker::new();
        let parent = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: state.rows_area().x + 3,
            row: state.rows_area().y,
            modifiers: KeyModifiers::NONE,
        };
        let selected = state.pointer_decision(&tree, &parent, &mut clicks).unwrap();
        assert_eq!(selected, TreePointerOutcome::Select("parent".to_owned()));
        #[cfg(feature = "ui-bridge")]
        {
            let selected_action = tree
                .ui_action_for_pointer_outcome("tree", &selected)
                .unwrap();
            assert_eq!(selected_action.kind, crate::UiEventKind::Select);
        }

        let activated = state.pointer_decision(&tree, &parent, &mut clicks).unwrap();
        assert_eq!(activated, TreePointerOutcome::Activate("parent".to_owned()));
        #[cfg(feature = "ui-bridge")]
        {
            let activated_action = tree
                .ui_action_for_pointer_outcome("tree", &activated)
                .unwrap();
            assert_eq!(activated_action.kind, crate::UiEventKind::Cancel);
        }

        let disclosure = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: state.rows_area().x + 2,
            row: state.rows_area().y + 1,
            modifiers: KeyModifiers::NONE,
        };
        let expanded = state
            .pointer_decision(&tree, &disclosure, &mut clicks)
            .unwrap();
        assert_eq!(
            expanded,
            TreePointerOutcome::SetExpanded {
                item_id: "projects".to_owned(),
                expanded: false,
            }
        );
        #[cfg(feature = "ui-bridge")]
        {
            let expanded_action = tree
                .ui_action_for_pointer_outcome("tree", &expanded)
                .unwrap();
            assert_eq!(
                expanded_action.value,
                crate::UiEventValue::TextList(vec!["projects".to_owned(), "false".to_owned()])
            );
        }
    }

    #[test]
    fn terminal_filter_keeps_its_semantic_label_with_or_without_editing_state() {
        let mut tree = fixture();
        tree.filter.as_mut().unwrap().placeholder = "Find a note".to_owned();

        let mut stateless = TreeState::default();
        let mut stateless_buffer = Buffer::empty(Rect::new(0, 0, 40, 8));
        tree.widget(&mut stateless)
            .render(stateless_buffer.area, &mut stateless_buffer);
        let stateless_row = (0..40)
            .map(|x| stateless_buffer[(x, 0)].symbol())
            .collect::<String>();
        assert!(stateless_row.starts_with("  Filter notes: Find a note"));

        let mut input = InputField::new("");
        let mut stateful = TreeState::default();
        let mut theme = TreeTheme::for_theme(KitTheme::light());
        theme.filter = Style::new().fg(Color::Green);
        theme.empty = Style::new().fg(Color::Yellow);
        let mut stateful_buffer = Buffer::empty(Rect::new(0, 0, 40, 8));
        tree.widget_with_filter(&mut stateful, &mut input)
            .theme(theme)
            .render(stateful_buffer.area, &mut stateful_buffer);

        assert_eq!(stateful_buffer[(0, 0)].symbol(), " ");
        assert_eq!(stateful_buffer[(1, 0)].symbol(), " ");
        assert_eq!(stateful_buffer[(2, 0)].symbol(), "F");
        assert_eq!(stateful_buffer[(2, 0)].fg, Color::Green);
        assert_eq!(stateful_buffer[(16, 0)].symbol(), "F");
        assert_eq!(stateful_buffer[(16, 0)].fg, Color::Yellow);
    }
}
