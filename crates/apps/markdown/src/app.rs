use std::io;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::{DefaultTerminal, Frame};
use tui_textarea::{CursorMove, Input, Key};
use unpeel_app_kit::{
    AgentBridge, AppReporter, DropTargetEvent, DropTargetSurface, FooterAction,
    MarkdownCommandHint, MarkdownEditorActions, MarkdownEditorConfig, MarkdownEditorEvent,
    MarkdownEditorSpec, MarkdownMenuTrigger, MarkdownPresentation, MarkdownTextArea,
    MarkdownTextAreaStyle, MenuItem, MenuTheme, PopupMenu, SemanticMenu, SemanticMenuAnchor,
    SemanticMenuItem, SemanticMenuPresentation, ThemeMonitor, UiBridge, UiBridgeEvent, UiComponent,
    UiEvent, UiEventKind, UiEventOutcome, UiEventValue, UiNode, markdown_delta_operations,
};

use crate::block::{self, BlockKind, EnterAction};
use crate::clipboard;
use crate::format::{self, Mark};
use crate::heading;
use crate::highlight;
use crate::mouse;
use crate::slash::{self, ItemId, MenuOrigin};
use crate::theme::Theme;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(700);
const UI_VIEW_ID: &str = "main";
const UI_EDITOR_ID: &str = "markdown-editor";
const UI_MENU_SELECT: &str = "markdown-menu-select";
const UI_MENU_DISMISS: &str = "markdown-menu-dismiss";
const UI_CONTEXT_SEND: &str = "send-reference-to-agent";
const UI_CONTEXT_COPY: &str = "copy-reference";
const UI_CONTEXT_SEND_ID: &str = "context-send-agent";
const UI_CONTEXT_COPY_ID: &str = "context-copy-reference";
const UI_FOOTER_NEW_ID: &str = "new-note";
const UI_FOOTER_NEW_ACTION: &str = "new-note";
const UI_FOOTER_SAVE_ID: &str = "save-note";
const UI_FOOTER_SAVE_ACTION: &str = "save-note";
const UI_FOOTER_AUTOSAVE_ID: &str = "toggle-autosave";
const UI_FOOTER_AUTOSAVE_ACTION: &str = "toggle-autosave";
const UI_BACK_ACTION: &str = "back-to-notes";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edit,
    Menu,
}

struct BlockMenu {
    origin: MenuOrigin,
    selected: usize,
    popup: Option<PopupMenu<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ContextAction {
    SendToAgent(String),
    CopyReference(String),
}

struct ContextMenu {
    popup: PopupMenu<String>,
    reference: String,
}

impl ContextMenu {
    fn selected_action(&self) -> Option<ContextAction> {
        match self.popup.selected_value().map(String::as_str)? {
            UI_CONTEXT_SEND_ID => Some(ContextAction::SendToAgent(self.reference.clone())),
            UI_CONTEXT_COPY_ID => Some(ContextAction::CopyReference(self.reference.clone())),
            _ => None,
        }
    }
}

impl Deref for ContextMenu {
    type Target = PopupMenu<String>;

    fn deref(&self) -> &Self::Target {
        &self.popup
    }
}

impl DerefMut for ContextMenu {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.popup
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Char,
    Word,
    Line,
}

struct DragState {
    kind: DragKind,
    anchor_start: (usize, usize),
    anchor_end: (usize, usize),
}

pub struct App<'a> {
    path: PathBuf,
    textarea: MarkdownTextArea<'a>,
    theme: Theme,
    theme_monitor: ThemeMonitor,
    drop_target: DropTargetSurface,
    mode: Mode,
    menu: Option<BlockMenu>,
    context_menu: Option<ContextMenu>,
    agent: AgentBridge,
    autosave: bool,
    /// Opened from the note list: the title carries a back chevron.
    back_to_list: bool,
    status_area: Rect,
    footer_area: Rect,
    dirty: bool,
    last_edit_at: Option<Instant>,
    exit: bool,
    status: Option<(String, Instant)>,
    drag: Option<DragState>,
    last_click: Option<(Instant, u16, u16, u8)>,
    presentation: MarkdownPresentation,
}

impl App<'_> {
    #[cfg(test)]
    pub fn open(path: PathBuf, theme: Theme) -> io::Result<Self> {
        Self::open_with_autosave(path, theme, true)
    }

    /// Shows a back chevron in the title that returns to the note list.
    #[must_use]
    pub fn with_back_to_list(mut self, enabled: bool) -> Self {
        self.back_to_list = enabled;
        self
    }

    pub fn open_with_autosave(path: PathBuf, theme: Theme, autosave: bool) -> io::Result<Self> {
        let contents = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let mut textarea = MarkdownTextArea::new(contents.lines(), markdown_text_area_style(theme));
        textarea.text_area_mut().set_placeholder_text("");
        highlight::refresh(&mut textarea, theme);
        let agent = AgentBridge::new();
        agent.refresh();

        let is_new = !path.exists();
        let mut app = Self {
            path,
            textarea,
            theme,
            theme_monitor: ThemeMonitor::from_theme(theme.kit),
            drop_target: DropTargetSurface::detect(),
            mode: Mode::Edit,
            menu: None,
            context_menu: None,
            agent,
            autosave,
            back_to_list: false,
            status_area: Rect::default(),
            footer_area: Rect::default(),
            dirty: false,
            last_edit_at: None,
            exit: false,
            status: None,
            drag: None,
            last_click: None,
            presentation: MarkdownPresentation::Source,
        };
        if is_new {
            app.flash(if autosave {
                "new file — auto-save will create it"
            } else {
                "new file — press Ctrl+S to create it"
            });
        }
        Ok(app)
    }

    pub fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        status: &mut AppReporter,
        bridge: &mut UiBridge,
        revision_counter: &mut u64,
    ) -> io::Result<()> {
        let mut revision = revision_counter
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
        let mut published = self.ui_node();
        bridge
            .publish(UI_VIEW_ID, revision, published.clone())
            .map_err(ui_bridge_error)?;
        while !self.exit {
            self.drain_bridge(bridge, &mut revision, &mut published)?;
            self.maybe_autosave();
            self.publish_projection(bridge, &mut revision, &mut published)?;
            if bridge.should_render_terminal() {
                self.drop_target.begin_frame();
                terminal.draw(|frame| self.draw_node(frame, &published))?;
                self.drop_target.commit()?;
            }
            self.publish_context(status);
            self.handle_events()?;
            self.publish_projection(bridge, &mut revision, &mut published)?;
        }
        if self.autosave && self.dirty {
            self.write_document(false);
            self.publish_projection(bridge, &mut revision, &mut published)?;
        }
        *revision_counter = revision;
        status.flush();
        Ok(())
    }

    fn editor_config(&self) -> MarkdownEditorConfig {
        let mut config = MarkdownEditorConfig::new(UI_EDITOR_ID);
        if self.back_to_list {
            config = config.back_action(UI_BACK_ACTION);
        }
        config
            .title(self.semantic_title())
            .dirty(self.dirty)
            .presentation(self.presentation)
            .open_menu_action(MarkdownEditorActions::OPEN_MENU)
            .command_hint(MarkdownCommandHint::new("Type '/' for commands"))
            .insert_menu(self.semantic_insert_menu())
            .context_menu(self.semantic_context_menu())
            .footer_actions([
                FooterAction::new(UI_FOOTER_NEW_ID, "new", UI_FOOTER_NEW_ACTION)
                    .accelerator("ctrl+n"),
                FooterAction::new(UI_FOOTER_SAVE_ID, "save", UI_FOOTER_SAVE_ACTION)
                    .accelerator("ctrl+s"),
                FooterAction::new(
                    UI_FOOTER_AUTOSAVE_ID,
                    if self.autosave {
                        "auto-save on"
                    } else {
                        "auto-save off"
                    },
                    UI_FOOTER_AUTOSAVE_ACTION,
                ),
            ])
    }

    fn semantic_title(&self) -> String {
        let (row, col) = self.textarea.cursor();
        let save_state = if self.dirty { "Unsaved" } else { "Saved" };
        let message = self
            .status
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(3))
            .map(|(text, _)| text.as_str());
        match message {
            Some(message) => format!(
                "{} · {save_state} · {}:{} · {message}",
                file_name(&self.path),
                row + 1,
                col + 1
            ),
            None => format!(
                "{} · {save_state} · {}:{}",
                file_name(&self.path),
                row + 1,
                col + 1
            ),
        }
    }

    fn ui_node(&self) -> UiNode {
        self.textarea.ui_node(&self.editor_config())
    }

    fn semantic_insert_menu(&self) -> Option<SemanticMenu> {
        let state = self.menu.as_ref()?;
        let visible = self.visible_menu_items();
        let selected = visible.get(state.selected).copied();
        let mut menu = SemanticMenu::new(
            match state.origin {
                MenuOrigin::Slash => "Insert block",
                MenuOrigin::Palette => "Block commands",
            },
            visible.iter().map(|item| {
                SemanticMenuItem::new(menu_item_wire_id(item.id), item.name, UI_MENU_SELECT)
                    .hint(format!("{}  {}", item.shortcut, item.sample))
            }),
        )
        .anchor(SemanticMenuAnchor::Caret)
        .presentation(SemanticMenuPresentation::Popup)
        .dismiss_action(UI_MENU_DISMISS);
        if let Some(selected) = selected {
            menu = menu.selected_id(menu_item_wire_id(selected.id));
        }
        Some(menu)
    }

    fn semantic_context_menu(&self) -> SemanticMenu {
        semantic_context_menu(self.agent.label().is_some())
    }

    fn current_reference(&self) -> String {
        let rows = heading::selected_rows(
            self.textarea.cursor(),
            normalized_selection(self.textarea.selection_range()),
            self.textarea.lines().len(),
        );
        line_reference(&self.path, rows)
    }

    fn handle_menu_ui_event(&mut self, revision: u64, event: &UiEvent) -> Result<bool, String> {
        if event.base_revision != revision {
            return Err(format!(
                "Markdown changed from revision {} to {revision}; retry the menu action",
                event.base_revision
            ));
        }
        let node = event.action.node_id.as_str();
        let action = event.action.action.as_str();
        if event.action.kind == UiEventKind::Activate && event.action.value == UiEventValue::None {
            match (node, action) {
                (UI_FOOTER_NEW_ID, UI_FOOTER_NEW_ACTION) => {
                    self.new_file();
                    return Ok(true);
                }
                (UI_FOOTER_SAVE_ID, UI_FOOTER_SAVE_ACTION) => {
                    self.save();
                    return Ok(true);
                }
                (UI_FOOTER_AUTOSAVE_ID, UI_FOOTER_AUTOSAVE_ACTION) => {
                    self.toggle_autosave();
                    return Ok(true);
                }
                _ => {}
            }
        }
        if action == UI_MENU_SELECT {
            if event.action.kind != UiEventKind::Activate
                || event.action.value != UiEventValue::None
            {
                return Err("Menu selection requires an activate event".to_string());
            }
            let Some(index) = self
                .visible_menu_items()
                .iter()
                .position(|item| menu_item_wire_id(item.id) == node)
            else {
                return Err("Menu item is no longer visible".to_string());
            };
            self.apply_menu_index(index);
            return Ok(true);
        }
        if node == UI_EDITOR_ID && action == UI_MENU_DISMISS {
            if event.action.kind != UiEventKind::Cancel {
                return Err("Menu dismissal requires a cancel event".to_string());
            }
            let revert_slash = self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.origin == MenuOrigin::Slash);
            self.close_menu(revert_slash);
            return Ok(true);
        }
        let context_action = match (node, action) {
            (UI_CONTEXT_SEND_ID, UI_CONTEXT_SEND) => {
                Some(ContextAction::SendToAgent(self.current_reference()))
            }
            (UI_CONTEXT_COPY_ID, UI_CONTEXT_COPY) => {
                Some(ContextAction::CopyReference(self.current_reference()))
            }
            _ => None,
        };
        if let Some(context_action) = context_action {
            if event.action.kind != UiEventKind::Activate
                || event.action.value != UiEventValue::None
            {
                return Err("Context Menu selection requires an activate event".to_string());
            }
            self.activate_context_action(context_action);
            return Ok(true);
        }
        Ok(false)
    }

    fn publish_projection(
        &self,
        bridge: &mut UiBridge,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<()> {
        let next = self.ui_node();
        let operations = markdown_delta_operations(published, &next);
        if operations.is_empty() {
            return Ok(());
        }
        let next_revision = revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
        bridge
            .publish_delta(UI_VIEW_ID, *revision, next_revision, operations)
            .map_err(ui_bridge_error)?;
        *revision = next_revision;
        *published = next;
        Ok(())
    }

    fn drain_bridge(
        &mut self,
        bridge: &mut UiBridge,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<()> {
        while let Some(message) = bridge.poll().map_err(ui_bridge_error)? {
            match message {
                UiBridgeEvent::Action { event, .. } => {
                    let menu_result = self.handle_menu_ui_event(*revision, &event);
                    let result = match menu_result {
                        Ok(true) => None,
                        Ok(false) => Some(self.textarea.handle_ui_event(
                            *revision,
                            &self.editor_config(),
                            &event,
                        )),
                        Err(error) => {
                            self.publish_projection(bridge, revision, published)?;
                            bridge
                                .acknowledge(&event, UiEventOutcome::Rejected(error), *revision)
                                .map_err(ui_bridge_error)?;
                            continue;
                        }
                    };
                    let outcome = match result {
                        None => UiEventOutcome::Applied,
                        Some(Ok(Some(MarkdownEditorEvent::TextChanged { changed: true })))
                        | Some(Ok(Some(MarkdownEditorEvent::Undo { changed: true })))
                        | Some(Ok(Some(MarkdownEditorEvent::Redo { changed: true }))) => {
                            self.after_edit();
                            self.sync_external_slash_menu();
                            UiEventOutcome::Applied
                        }
                        Some(Ok(Some(MarkdownEditorEvent::PresentationRequested(
                            presentation,
                        )))) => {
                            self.presentation = presentation;
                            UiEventOutcome::Applied
                        }
                        Some(Ok(Some(MarkdownEditorEvent::MenuRequested(trigger)))) => {
                            self.handle_semantic_menu_trigger(trigger)
                        }
                        Some(Ok(Some(MarkdownEditorEvent::SaveRequested))) => {
                            self.save();
                            UiEventOutcome::Applied
                        }
                        Some(Ok(Some(MarkdownEditorEvent::BackRequested))) => {
                            self.exit = true;
                            UiEventOutcome::Applied
                        }
                        Some(Ok(Some(MarkdownEditorEvent::SelectionChanged)))
                        | Some(Ok(Some(MarkdownEditorEvent::TextChanged { changed: false })))
                        | Some(Ok(Some(MarkdownEditorEvent::Undo { changed: false })))
                        | Some(Ok(Some(MarkdownEditorEvent::Redo { changed: false }))) => {
                            UiEventOutcome::Applied
                        }
                        Some(Ok(None)) => UiEventOutcome::Rejected(
                            "Action targets a different Markdown component".to_string(),
                        ),
                        Some(Err(error)) => UiEventOutcome::Rejected(error.to_string()),
                    };
                    self.publish_projection(bridge, revision, published)?;
                    bridge
                        .acknowledge(&event, outcome, *revision)
                        .map_err(ui_bridge_error)?;
                }
                UiBridgeEvent::Attached { .. }
                | UiBridgeEvent::Detached { .. }
                | UiBridgeEvent::Lifecycle { .. } => {}
            }
        }
        Ok(())
    }

    /// Live context for agents: which note is open, where the cursor is,
    /// what is selected, and whether the buffer has unsaved edits. Debounced
    /// and deduplicated by the reporter, so per-iteration calls are cheap.
    fn publish_context(&self, status: &mut AppReporter) {
        let file = std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone());
        let folder = file.parent().map(|parent| parent.display().to_string());
        let (row, _) = self.textarea.cursor();
        let selection_lines = self.textarea.selection_range().map(|(start, end)| {
            let (low, high) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            [low.0 + 1, high.0 + 1]
        });
        status.set_context(&serde_json::json!({
            "file": file.display().to_string(),
            "folder": folder,
            "cursor_line": row + 1,
            "selection_lines": selection_lines,
            "dirty": self.dirty,
            "autosave": self.autosave,
        }));
    }

    fn draw_node(&mut self, frame: &mut Frame, node: &UiNode) {
        let UiComponent::MarkdownEditor(spec) = &node.element else {
            return;
        };
        highlight::refresh(&mut self.textarea, self.theme);
        let show_cursor = self.context_menu.is_none()
            && (self.mode == Mode::Edit
                || self
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.origin == MenuOrigin::Slash));
        let layout = self
            .textarea
            .render_component(frame, frame.area(), show_cursor, spec);
        self.drop_target.register(layout.body);
        self.footer_area = layout.footer;
        self.status_area = layout.status;

        if self.mode == Mode::Menu {
            self.draw_menu(frame, layout.body, spec);
        }
        if let Some(menu) = self.context_menu.as_mut() {
            menu.render(frame);
        }
    }

    #[cfg(test)]
    fn draw(&mut self, frame: &mut Frame) {
        let node = self.ui_node();
        self.draw_node(frame, &node);
    }

    fn draw_menu(&mut self, frame: &mut Frame, area: Rect, spec: &MarkdownEditorSpec) {
        let Some(semantic_menu) = spec.insert_menu.as_ref() else {
            return;
        };
        let anchor = self
            .textarea
            .rendered_cursor_position()
            .unwrap_or(Position::new(
                area.x.saturating_add(2),
                area.y.saturating_add(1),
            ));
        let mut popup =
            semantic_menu.popup(anchor, MenuTheme::for_color_scheme(self.theme.kit.scheme));
        popup.render(frame);
        if let Some(menu) = self.menu.as_mut() {
            menu.selected = popup.selected_index().unwrap_or(0);
            menu.popup = Some(popup);
        }
    }

    fn handle_events(&mut self) -> io::Result<()> {
        self.handle_drop_target_event()?;
        if !event::poll(Duration::from_millis(120))? {
            self.drop_target.heartbeat()?;
            if self.theme_monitor.refresh() {
                self.theme = Theme::from_kit(self.theme_monitor.theme());
                self.textarea
                    .set_component_style(markdown_text_area_style(self.theme));
            }
            return Ok(());
        }
        // Drain the whole queue before the next draw. Touch terminals send
        // scroll flicks as bursts of wheel events; redrawing per event turns
        // one flick into dozens of full diff frames — an output storm that
        // remote viewers have to replay.
        loop {
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    self.handle_key_event(key);
                }
                Event::Paste(text) if self.context_menu.is_none() => self.paste_text(&text),
                Event::Mouse(mouse) => self.handle_mouse(mouse),
                Event::Resize(_, _) => {}
                _ => {}
            }
            if self.exit || !event::poll(Duration::ZERO)? {
                return Ok(());
            }
        }
    }

    fn handle_drop_target_event(&mut self) -> io::Result<()> {
        let Some(event) = self.drop_target.poll()? else {
            return Ok(());
        };
        match event {
            DropTargetEvent::Hover { position } => {
                self.prepare_for_drop();
                self.textarea.position_drop_cursor(position);
            }
            DropTargetEvent::Leave => {}
            DropTargetEvent::Drop {
                position,
                text,
                references,
            } => {
                self.prepare_for_drop();
                self.textarea.position_drop_cursor(position);
                let text = if text.is_empty() {
                    references.join(" ")
                } else {
                    text
                };
                self.paste_text(&text);
            }
        }
        Ok(())
    }

    fn prepare_for_drop(&mut self) {
        self.mode = Mode::Edit;
        self.menu = None;
        self.context_menu = None;
        self.drag = None;
        self.last_click = None;
    }

    fn handle_key_event(&mut self, key: KeyEvent) {
        if is_control_c(&key) {
            self.exit = true;
            return;
        }
        if self.context_menu.is_some() {
            self.handle_context_menu_key(key);
            return;
        }
        let footer_action = self.ui_node().footer_action_for_key(&key).cloned();
        if let Some(action) = footer_action {
            self.activate_footer_action(&action);
            return;
        }
        if !self.handle_key_command(&key) {
            self.handle_input(input_from_key(key));
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        self.textarea.track_mouse(&mouse);
        if let Some(menu) = self.context_menu.as_mut() {
            menu.track_mouse(&mouse);
        }
        if let Some(popup) = self.menu.as_mut().and_then(|menu| menu.popup.as_mut()) {
            popup.track_mouse(&mouse);
        }
        let point = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => self.open_context_menu(point),
            MouseEventKind::Down(MouseButton::Left) if self.context_menu.is_some() => {
                self.click_context_menu(point)
            }
            MouseEventKind::Down(MouseButton::Left)
                if self.back_to_list && self.status_area.contains(point) =>
            {
                self.exit = true;
            }
            MouseEventKind::Down(MouseButton::Left) if self.footer_area.contains(point) => {
                let action = self
                    .ui_node()
                    .footer()
                    .and_then(|footer| footer.action_at(point, self.footer_area))
                    .cloned();
                if let Some(action) = action {
                    self.activate_footer_action(&action);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.on_mouse_down(point, mouse.modifiers),
            MouseEventKind::Moved if self.context_menu.is_some() => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.hover_at(point);
                }
            }
            MouseEventKind::Moved => {
                self.hover_menu(point);
                if self.drag.is_some() {
                    self.on_mouse_drag(point);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_some() => {
                self.on_mouse_drag(point);
            }
            MouseEventKind::Up(MouseButton::Left) => self.on_mouse_up(),
            MouseEventKind::ScrollDown if self.context_menu.is_some() => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.move_selection(1);
                }
            }
            MouseEventKind::ScrollUp if self.context_menu.is_some() => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.move_selection(-1);
                }
            }
            MouseEventKind::ScrollDown if self.menu_contains(point) => self.move_menu(1),
            MouseEventKind::ScrollUp if self.menu_contains(point) => self.move_menu(-1),
            MouseEventKind::ScrollDown if self.textarea.contains(point) => {
                self.textarea
                    .scroll_lines_with_selection(1, mouse.modifiers.contains(KeyModifiers::SHIFT));
            }
            MouseEventKind::ScrollUp if self.textarea.contains(point) => {
                self.textarea
                    .scroll_lines_with_selection(-1, mouse.modifiers.contains(KeyModifiers::SHIFT));
            }
            _ => {}
        }
    }

    fn handle_context_menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.context_menu = None,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.move_selection(1);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') if key.kind == KeyEventKind::Press => {
                self.activate_context_menu();
            }
            _ => {}
        }
    }

    fn open_context_menu(&mut self, point: Position) {
        if !self.textarea.contains(point) {
            self.context_menu = None;
            return;
        }
        if self.mode == Mode::Menu {
            self.close_menu(false);
        }
        let clicked = self.textarea.hit_test(point);
        let selection = normalized_selection(self.textarea.selection_range());
        if !selection.is_some_and(|range| selection_contains(range, clicked)) {
            self.textarea.cancel_selection();
            self.jump(clicked.0, clicked.1);
        }
        let rows = heading::selected_rows(
            self.textarea.cursor(),
            normalized_selection(self.textarea.selection_range()),
            self.textarea.lines().len(),
        );
        let reference = line_reference(&self.path, rows);
        self.agent.refresh();
        self.context_menu = Some(editor_context_menu(
            reference,
            self.agent.label().is_some(),
            point,
            self.theme,
        ));
        self.drag = None;
        self.last_click = None;
    }

    fn click_context_menu(&mut self, point: Position) {
        let activate = self.context_menu.as_mut().is_some_and(|menu| {
            let enabled = menu.item_at(point).is_some_and(MenuItem::is_enabled);
            if enabled {
                menu.select_at(point);
            }
            enabled
        });
        if activate {
            self.activate_context_menu();
        } else {
            self.context_menu = None;
        }
    }

    fn activate_context_menu(&mut self) {
        let Some(menu) = self.context_menu.take() else {
            return;
        };
        let Some(action) = menu.selected_action() else {
            return;
        };
        self.activate_context_action(action);
    }

    fn activate_context_action(&mut self, action: ContextAction) {
        match action {
            ContextAction::SendToAgent(reference) => match self.agent.send_reference(&reference) {
                Ok(label) => self.flash(format!("Sent reference to {label}")),
                Err(error) if clipboard::write(&reference) => {
                    self.flash(format!("{error}; reference copied instead"))
                }
                Err(error) => self.flash(format!("{error}; copy failed")),
            },
            ContextAction::CopyReference(reference) => {
                if clipboard::write(&reference) {
                    self.flash("reference copied");
                } else {
                    self.flash("copy failed");
                }
            }
        }
    }

    fn on_mouse_down(&mut self, point: Position, modifiers: KeyModifiers) {
        if self.mode == Mode::Menu {
            if let Some(index) = self.menu_item_at(point) {
                self.apply_menu_index(index);
                return;
            }
            if self.menu_contains(point) {
                return;
            }
            self.close_menu(false);
        }
        if !self.textarea.contains(point) {
            return;
        }
        let (row, col) = self.textarea.hit_test(point);
        if self.click_checkbox(row, col) {
            return;
        }
        let count = self.register_click(point.x, point.y);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        match (count, shift) {
            (1, true) => {
                if !self.textarea.is_selecting() {
                    self.textarea.start_selection();
                }
                self.jump(row, col);
                self.drag = Some(DragState {
                    kind: DragKind::Char,
                    anchor_start: self.selection_anchor(),
                    anchor_end: self.selection_anchor(),
                });
            }
            (2, _) => {
                let (start, end) = self.select_word_at(row, col);
                self.drag = Some(DragState {
                    kind: DragKind::Word,
                    anchor_start: (row, start),
                    anchor_end: (row, end),
                });
            }
            (3, _) => {
                let end = self.select_line_at(row);
                self.drag = Some(DragState {
                    kind: DragKind::Line,
                    anchor_start: (row, 0),
                    anchor_end: (row, end),
                });
            }
            _ => {
                self.textarea.cancel_selection();
                self.jump(row, col);
                self.textarea.start_selection();
                self.drag = Some(DragState {
                    kind: DragKind::Char,
                    anchor_start: (row, col),
                    anchor_end: (row, col),
                });
            }
        }
    }

    fn on_mouse_drag(&mut self, point: Position) {
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        let kind = drag.kind;
        let anchor_start = drag.anchor_start;
        let anchor_end = drag.anchor_end;
        self.textarea.auto_scroll(point);
        let (row, col) = self.textarea.hit_test(point);
        match kind {
            DragKind::Char => self.jump(row, col),
            DragKind::Word => {
                let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
                let (word_start, word_end) = mouse::word_bounds(&line, col);
                let start = if mouse::pos_le((row, col), anchor_start) {
                    (row, word_start)
                } else {
                    anchor_start
                };
                let end = if mouse::pos_le(anchor_end, (row, col)) {
                    (row, word_end)
                } else {
                    anchor_end
                };
                self.set_selection(start, end);
            }
            DragKind::Line => {
                let start_row = anchor_start.0.min(row);
                let end_row = anchor_start.0.max(row);
                let end_col = self.line_len(end_row);
                self.set_selection((start_row, 0), (end_row, end_col));
            }
        }
    }

    fn on_mouse_up(&mut self) {
        if let Some(range) = self.textarea.selection_range()
            && range.0 == range.1
        {
            self.textarea.cancel_selection();
        }
        self.drag = None;
    }

    fn register_click(&mut self, x: u16, y: u16) -> u8 {
        let now = Instant::now();
        let count = match self.last_click {
            Some((at, lx, ly, n))
                if at.elapsed() < Duration::from_millis(400)
                    && x.abs_diff(lx) <= 1
                    && y.abs_diff(ly) <= 1 =>
            {
                n.saturating_add(1).min(3)
            }
            _ => 1,
        };
        self.last_click = Some((now, x, y, count));
        count
    }

    fn jump(&mut self, row: usize, col: usize) {
        self.textarea.move_cursor(CursorMove::Jump(
            row.min(usize::from(u16::MAX)) as u16,
            col.min(usize::from(u16::MAX)) as u16,
        ));
    }

    fn set_selection(&mut self, start: (usize, usize), end: (usize, usize)) {
        self.textarea.cancel_selection();
        self.jump(start.0, start.1);
        self.textarea.start_selection();
        self.jump(end.0, end.1);
    }

    fn select_word_at(&mut self, row: usize, col: usize) -> (usize, usize) {
        let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
        let (start, end) = mouse::word_bounds(&line, col);
        self.set_selection((row, start), (row, end));
        (start, end)
    }

    fn select_line_at(&mut self, row: usize) -> usize {
        let end = self.line_len(row);
        self.set_selection((row, 0), (row, end));
        end
    }

    fn line_len(&self, row: usize) -> usize {
        self.textarea
            .lines()
            .get(row)
            .map(|line| line.chars().count())
            .unwrap_or(0)
    }

    fn selection_anchor(&self) -> (usize, usize) {
        self.textarea
            .selection_range()
            .map(|(start, _)| start)
            .unwrap_or_else(|| self.textarea.cursor())
    }

    fn handle_input(&mut self, input: Input) {
        if self.handle_command(&input) {
            return;
        }
        if self.mode == Mode::Menu {
            self.handle_menu_input(input);
            return;
        }
        match input {
            Input { key: Key::Esc, .. } => self.exit = true,
            Input {
                key: Key::Enter, ..
            } => self.handle_enter(),
            Input {
                key: Key::Backspace,
                ..
            } => self.handle_backspace(),
            input @ Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
                ..
            } => self.handle_down(input),
            Input {
                key: Key::Tab,
                shift: true,
                ..
            } if !self.in_fence() => {
                block::indent_lines(&mut self.textarea, -2);
                self.after_edit();
            }
            Input { key: Key::Tab, .. } if !self.in_fence() => {
                block::indent_lines(&mut self.textarea, 2);
                self.after_edit();
            }
            Input {
                key: Key::Char('/' | '7'),
                ctrl: false,
                ..
            } if is_slash_key(&input) && self.can_open_slash() => {
                self.textarea.insert_char('/');
                self.after_edit();
                self.open_menu(MenuOrigin::Slash);
            }
            Input {
                key: Key::Char('/' | '7'),
                ctrl: false,
                ..
            } if is_slash_key(&input) => {
                self.textarea.insert_char('/');
                self.after_edit();
                self.apply_markdown_shortcut();
            }
            Input {
                key: Key::Char('\\'),
                ctrl: false,
                ..
            } if self.can_open_slash() => {
                self.open_menu(MenuOrigin::Palette);
            }
            Input {
                key: Key::Char(ch),
                ctrl: false,
                ..
            } if !ch.is_control() => {
                self.textarea.insert_char(ch);
                self.after_edit();
                self.apply_markdown_shortcut();
            }
            other => {
                if self.textarea.input(other) {
                    self.after_edit();
                    self.apply_markdown_shortcut();
                }
            }
        }
    }

    fn handle_key_command(&mut self, key: &KeyEvent) -> bool {
        if !is_command_modifier(key.modifiers) {
            return false;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter => {
                self.toggle_or_make_todo();
                true
            }
            KeyCode::Char(ch) => self.dispatch_command(ch, shift),
            _ => false,
        }
    }

    fn handle_command(&mut self, input: &Input) -> bool {
        if input.ctrl && matches!(input.key, Key::Enter) {
            self.toggle_or_make_todo();
            return true;
        }
        if !input.ctrl {
            return false;
        }
        let Key::Char(ch) = input.key else {
            return false;
        };
        self.dispatch_command(ch, input.shift)
    }

    fn dispatch_command(&mut self, ch: char, shift: bool) -> bool {
        match ch.to_ascii_lowercase() {
            'q' | 'w' => self.exit = true,
            's' if shift => self.toggle_mark(Mark::Strike),
            'b' => self.toggle_mark(Mark::Bold),
            'i' => self.toggle_mark(Mark::Italic),
            'e' => self.toggle_mark(Mark::Code),
            'd' => self.duplicate_line(),
            'a' => self.textarea.select_all(),
            'c' => self.copy_selection(),
            'x' if shift => self.toggle_mark(Mark::Strike),
            'x' => self.cut_selection(),
            'v' => self.paste_clipboard(),
            'y' => self.redo_edit(),
            'z' if shift => self.redo_edit(),
            'z' => self.undo_edit(),
            'f' => self.flash("find: select text, then search in the file"),
            _ => return false,
        }
        true
    }

    fn activate_footer_action(&mut self, action: &FooterAction) {
        match (action.id.as_str(), action.action.as_str()) {
            (UI_FOOTER_NEW_ID, UI_FOOTER_NEW_ACTION) => self.new_file(),
            (UI_FOOTER_SAVE_ID, UI_FOOTER_SAVE_ACTION) => self.save(),
            (UI_FOOTER_AUTOSAVE_ID, UI_FOOTER_AUTOSAVE_ACTION) => self.toggle_autosave(),
            _ => self.flash("unknown footer action"),
        }
    }

    fn new_file(&mut self) {
        if self.dirty {
            self.flash("save first (⌘S), then ⌘N");
            return;
        }
        self.path = PathBuf::from("untitled.md");
        self.textarea = MarkdownTextArea::new([""], markdown_text_area_style(self.theme));
        highlight::refresh(&mut self.textarea, self.theme);
        self.dirty = false;
        self.last_edit_at = None;
        self.flash(if self.autosave {
            "new file — auto-save on"
        } else {
            "new file — auto-save off"
        });
    }

    fn toggle_autosave(&mut self) {
        self.autosave = !self.autosave;
        self.mode = Mode::Edit;
        self.menu = None;
        self.context_menu = None;
        if self.autosave && self.dirty {
            self.last_edit_at = Some(Instant::now());
        }
        let state = if self.autosave { "on" } else { "off" };
        match crate::start::write_autosave(crate::install::APP_ID, self.autosave) {
            Ok(()) => self.flash(format!("auto-save {state}")),
            Err(error) => self.flash(format!(
                "auto-save {state}; preference save failed: {error}"
            )),
        }
    }

    fn duplicate_line(&mut self) {
        let (row, col) = self.textarea.cursor();
        let Some(line) = self.textarea.lines().get(row).cloned() else {
            return;
        };
        self.textarea.move_cursor(CursorMove::End);
        self.textarea.insert_newline();
        self.textarea.insert_str(&line);
        self.jump(row + 1, col);
        self.after_edit();
    }

    fn copy_selection(&mut self) {
        if !self.textarea.is_selecting() {
            return;
        }
        self.textarea.copy();
        let text = self.textarea.yank_text();
        if !text.is_empty() {
            let _ = clipboard::write(&text);
        }
    }

    fn cut_selection(&mut self) {
        if !self.textarea.is_selecting() {
            return;
        }
        if self.textarea.cut() {
            let text = self.textarea.yank_text();
            if !text.is_empty() {
                let _ = clipboard::write(&text);
            }
            self.after_edit();
            self.sync_slash_menu();
        }
    }

    fn paste_clipboard(&mut self) {
        if let Some(text) = clipboard::read() {
            self.paste_text(&text);
            return;
        }
        if self.textarea.paste() {
            self.after_edit();
            self.sync_slash_menu();
        }
    }

    fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.origin == MenuOrigin::Palette)
        {
            self.close_menu(false);
        }
        if self.textarea.insert_str(text) {
            self.after_edit();
            self.sync_slash_menu();
        }
    }

    fn undo_edit(&mut self) {
        if self.textarea.undo() {
            self.after_edit();
            self.sync_slash_menu();
            self.flash("undo");
        } else {
            self.flash("nothing to undo");
        }
    }

    fn redo_edit(&mut self) {
        if self.textarea.redo() {
            self.after_edit();
            self.sync_slash_menu();
            self.flash("redo");
        } else {
            self.flash("nothing to redo");
        }
    }

    fn handle_menu_input(&mut self, input: Input) {
        let origin = self.menu.as_ref().map(|menu| menu.origin);
        let selected = self.menu.as_ref().map(|menu| menu.selected).unwrap_or(0);
        match input {
            Input { key: Key::Esc, .. } => {
                self.close_menu(origin == Some(MenuOrigin::Slash));
            }
            Input { key: Key::Up, .. } => self.move_menu(-1),
            Input { key: Key::Down, .. } => self.move_menu(1),
            Input { key: Key::Home, .. } => self.set_menu_selected(0),
            Input { key: Key::End, .. } => self.set_menu_selected(usize::MAX),
            Input {
                key: Key::Enter | Key::Tab,
                ..
            } => self.apply_menu_index(selected),
            Input {
                key: Key::Char(ch @ '1'..='6'),
                ..
            } => self.apply_kind(BlockKind::Heading(ch.to_digit(10).unwrap_or(1) as u8)),
            Input {
                key: Key::Char('0'),
                ..
            } => self.apply_kind(BlockKind::Paragraph),
            Input {
                key: Key::Char('a'),
                ctrl: false,
                ..
            } if origin == Some(MenuOrigin::Palette) => self.toggle_autosave(),
            other if origin == Some(MenuOrigin::Slash) => {
                if self.textarea.input(other) {
                    self.after_edit();
                }
                self.sync_slash_menu();
            }
            _ => {}
        }
    }

    fn can_open_slash(&self) -> bool {
        slash::can_open_slash(
            self.textarea.lines(),
            self.textarea.cursor().0,
            self.textarea.is_selecting(),
        )
    }

    /// Applies a renderer's declared text trigger against authoritative Rust
    /// state. Renderers never decide blank-line/fence eligibility themselves.
    fn handle_semantic_menu_trigger(&mut self, trigger: MarkdownMenuTrigger) -> UiEventOutcome {
        if self.can_open_slash() {
            if trigger == MarkdownMenuTrigger::Slash {
                self.textarea.insert_char('/');
                self.after_edit();
                self.open_menu(MenuOrigin::Slash);
            } else {
                self.open_menu(MenuOrigin::Palette);
            }
            return UiEventOutcome::Applied;
        }

        self.textarea.insert_char(match trigger {
            MarkdownMenuTrigger::Slash => '/',
            MarkdownMenuTrigger::Palette => '\\',
        });
        self.after_edit();
        self.apply_markdown_shortcut();
        UiEventOutcome::Applied
    }

    fn in_fence(&self) -> bool {
        slash::in_code_fence(self.textarea.lines(), self.textarea.cursor().0)
    }

    fn open_menu(&mut self, origin: MenuOrigin) {
        self.mode = Mode::Menu;
        self.context_menu = None;
        self.menu = Some(BlockMenu {
            origin,
            selected: 0,
            popup: None,
        });
        self.drag = None;
    }

    fn close_menu(&mut self, revert_slash: bool) {
        if revert_slash
            && self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.origin == MenuOrigin::Slash)
        {
            heading::clear_slash_command(&mut self.textarea);
            self.after_edit();
        }
        self.mode = Mode::Edit;
        self.menu = None;
    }

    fn current_slash_query(&self) -> Option<String> {
        let (row, col) = self.textarea.cursor();
        let line = self.textarea.lines().get(row)?;
        slash::slash_query(line, col).map(str::to_string)
    }

    fn visible_menu_items(&self) -> Vec<&'static slash::MenuItem> {
        let Some(menu) = self.menu.as_ref() else {
            return Vec::new();
        };
        let query = match menu.origin {
            MenuOrigin::Slash => self.current_slash_query().unwrap_or_default(),
            MenuOrigin::Palette => String::new(),
        };
        slash::visible_items(menu.origin, &query)
    }

    fn move_menu(&mut self, delta: i32) {
        let count = self.visible_menu_items().len();
        if count == 0 {
            return;
        }
        if let Some(menu) = self.menu.as_mut() {
            let current = menu.selected.min(count - 1) as i32;
            menu.selected = (current + delta).rem_euclid(count as i32) as usize;
        }
    }

    fn set_menu_selected(&mut self, index: usize) {
        let count = self.visible_menu_items().len();
        if let Some(menu) = self.menu.as_mut() {
            menu.selected = if count == 0 { 0 } else { index.min(count - 1) };
        }
    }

    fn hover_menu(&mut self, point: Position) {
        if let Some(index) = self.menu_item_at(point)
            && let Some(menu) = self.menu.as_mut()
        {
            menu.selected = index;
        }
    }

    fn menu_item_at(&self, point: Position) -> Option<usize> {
        self.menu.as_ref()?.popup.as_ref()?.item_index_at(point)
    }

    fn menu_contains(&self, point: Position) -> bool {
        self.menu
            .as_ref()
            .and_then(|menu| menu.popup.as_ref())
            .is_some_and(|popup| popup.area().contains(point))
    }

    fn sync_slash_menu(&mut self) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        if menu.origin != MenuOrigin::Slash {
            return;
        }
        if self.current_slash_query().is_none() {
            self.mode = Mode::Edit;
            self.menu = None;
            return;
        }
        let count = self.visible_menu_items().len();
        if let Some(menu) = self.menu.as_mut() {
            menu.selected = if count == 0 {
                0
            } else {
                menu.selected.min(count - 1)
            };
        }
    }

    fn sync_external_slash_menu(&mut self) {
        if self.menu.is_none()
            && !self.textarea.is_selecting()
            && !self.in_fence()
            && self.current_slash_query().is_some()
        {
            self.open_menu(MenuOrigin::Slash);
        }
        self.sync_slash_menu();
    }

    fn apply_menu_index(&mut self, index: usize) {
        let Some(item) = self.visible_menu_items().get(index).copied() else {
            return;
        };
        match item.id {
            ItemId::Block(kind) => self.apply_kind(kind),
            ItemId::LiteralBackslash => {
                self.close_menu(false);
                self.textarea.insert_char('\\');
                self.after_edit();
            }
            ItemId::ToggleAutosave => self.toggle_autosave(),
        }
    }

    fn apply_kind(&mut self, kind: BlockKind) {
        let slash = self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.origin == MenuOrigin::Slash);
        if slash {
            block::apply_slash(&mut self.textarea, kind);
        } else {
            block::apply_to_textarea(&mut self.textarea, kind);
        }
        self.mode = Mode::Edit;
        self.menu = None;
        self.after_edit();
        self.flash(format!("applied {}", kind.label()));
    }

    fn handle_enter(&mut self) {
        if self.textarea.is_selecting() || self.in_fence() {
            let _ = self.textarea.input(Input {
                key: Key::Enter,
                ..Input::default()
            });
            self.after_edit();
            return;
        }
        let (row, _) = self.textarea.cursor();
        let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
        match block::enter_action(&line) {
            EnterAction::Continue => {
                let prefix = block::continue_prefix(&line);
                self.textarea.insert_newline();
                self.textarea.insert_str(prefix);
                self.after_edit();
            }
            EnterAction::ExitList => {
                let indent = heading::split_indent(&line).0.to_string();
                heading::replace_current_line(&mut self.textarea, &indent);
                self.after_edit();
            }
            EnterAction::Default => {
                let _ = self.textarea.input(Input {
                    key: Key::Enter,
                    ..Input::default()
                });
                self.after_edit();
            }
        }
    }

    fn handle_down(&mut self, input: Input) {
        let (row, _) = self.textarea.cursor();
        let append_trailing_line = !self.textarea.is_selecting()
            && row + 1 == self.textarea.lines().len()
            && self
                .textarea
                .lines()
                .get(row)
                .is_some_and(|line| !line.is_empty());
        if append_trailing_line {
            self.textarea.move_cursor(CursorMove::End);
            self.textarea.insert_newline();
            self.after_edit();
        } else if self.textarea.input(input) {
            self.after_edit();
            self.apply_markdown_shortcut();
        }
    }

    fn handle_backspace(&mut self) {
        if self.textarea.is_selecting() {
            let _ = self.textarea.input(Input {
                key: Key::Backspace,
                ..Input::default()
            });
            self.after_edit();
            return;
        }
        let (row, col) = self.textarea.cursor();
        let line = self.textarea.lines().get(row).cloned().unwrap_or_default();
        if let Some(new_line) = block::backspace_to_paragraph(&line, col) {
            heading::replace_current_line(&mut self.textarea, &new_line);
            let indent_cols = heading::split_indent(&new_line).0.chars().count();
            self.jump(row, indent_cols);
            self.after_edit();
            return;
        }
        if self.textarea.input(Input {
            key: Key::Backspace,
            ..Input::default()
        }) {
            self.after_edit();
        }
    }

    fn apply_markdown_shortcut(&mut self) {
        let (row, _) = self.textarea.cursor();
        let Some(line) = self.textarea.lines().get(row).cloned() else {
            return;
        };
        let Some(new_line) = block::markdown_shortcut(&line) else {
            return;
        };
        heading::replace_current_line(&mut self.textarea, &new_line);
        self.after_edit();
    }

    fn toggle_mark(&mut self, mark: Mark) {
        if format::toggle(&mut self.textarea, mark) {
            self.after_edit();
            self.flash(mark.label());
        }
    }

    fn toggle_or_make_todo(&mut self) {
        let (row, _) = self.textarea.cursor();
        if slash::in_code_fence(self.textarea.lines(), row) {
            return;
        }
        let Some(line) = self.textarea.lines().get(row).cloned() else {
            return;
        };
        if let Some(new_line) = block::toggle_todo(&line) {
            heading::replace_current_line(&mut self.textarea, &new_line);
            self.after_edit();
            return;
        }
        block::apply_to_textarea(&mut self.textarea, BlockKind::Todo);
        self.after_edit();
        self.flash("to-do");
    }

    fn click_checkbox(&mut self, row: usize, col: usize) -> bool {
        let Some(line) = self.textarea.lines().get(row) else {
            return false;
        };
        let Some((start, end)) = block::checkbox_cols(line) else {
            return false;
        };
        if col < start || col > end {
            return false;
        }
        let Some(new_line) = block::toggle_todo(line) else {
            return false;
        };
        self.jump(row, col);
        heading::replace_current_line(&mut self.textarea, &new_line);
        self.after_edit();
        true
    }

    fn after_edit(&mut self) {
        self.dirty = true;
        self.last_edit_at = Some(Instant::now());
        highlight::refresh(&mut self.textarea, self.theme);
    }

    fn maybe_autosave(&mut self) {
        if self.autosave
            && self.dirty
            && self
                .last_edit_at
                .is_some_and(|edited| edited.elapsed() >= AUTOSAVE_DELAY)
        {
            self.write_document(false);
        }
    }

    fn save(&mut self) {
        self.write_document(true);
    }

    fn write_document(&mut self, announce: bool) -> bool {
        let mut text = self.textarea.lines().join("\n");
        if !text.ends_with('\n') {
            text.push('\n');
        }
        match std::fs::write(&self.path, text) {
            Ok(()) => {
                self.dirty = false;
                self.last_edit_at = None;
                if announce {
                    self.flash(format!("saved {}", self.path.display()));
                }
                true
            }
            Err(error) => {
                self.last_edit_at = Some(Instant::now());
                self.flash(format!("save failed: {error}"));
                false
            }
        }
    }

    fn flash(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }
}

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error.to_string())
}

fn editor_context_menu(
    reference: String,
    can_send: bool,
    anchor: Position,
    theme: Theme,
) -> ContextMenu {
    let semantic = semantic_context_menu(can_send);
    ContextMenu {
        popup: semantic.popup(anchor, MenuTheme::for_color_scheme(theme.kit.scheme)),
        reference,
    }
}

fn semantic_context_menu(can_send: bool) -> SemanticMenu {
    let mut items = Vec::with_capacity(2);
    if can_send {
        items.push(SemanticMenuItem::new(
            UI_CONTEXT_SEND_ID,
            "Send to agent",
            UI_CONTEXT_SEND,
        ));
    }
    items.push(SemanticMenuItem::new(
        UI_CONTEXT_COPY_ID,
        "Copy reference",
        UI_CONTEXT_COPY,
    ));
    SemanticMenu::new("Selection actions", items)
        .presentation(SemanticMenuPresentation::Context)
        .anchor(SemanticMenuAnchor::Pointer)
}

fn menu_item_wire_id(item: ItemId) -> &'static str {
    match item {
        ItemId::Block(BlockKind::Heading(1)) => "block-heading-1",
        ItemId::Block(BlockKind::Heading(2)) => "block-heading-2",
        ItemId::Block(BlockKind::Heading(3)) => "block-heading-3",
        ItemId::Block(BlockKind::Heading(4)) => "block-heading-4",
        ItemId::Block(BlockKind::Heading(5)) => "block-heading-5",
        ItemId::Block(BlockKind::Heading(6)) => "block-heading-6",
        ItemId::Block(BlockKind::Heading(_)) => "block-heading",
        ItemId::Block(BlockKind::Paragraph) => "block-paragraph",
        ItemId::Block(BlockKind::Bullet) => "block-bullet-list",
        ItemId::Block(BlockKind::Numbered) => "block-numbered-list",
        ItemId::Block(BlockKind::Todo) => "block-todo",
        ItemId::Block(BlockKind::Quote) => "block-quote",
        ItemId::Block(BlockKind::Code) => "block-code",
        ItemId::Block(BlockKind::Divider) => "block-divider",
        ItemId::LiteralBackslash => "insert-backslash",
        ItemId::ToggleAutosave => "toggle-autosave",
    }
}

fn normalized_selection(
    selection: Option<((usize, usize), (usize, usize))>,
) -> Option<((usize, usize), (usize, usize))> {
    selection.map(|(start, end)| {
        if start <= end {
            (start, end)
        } else {
            (end, start)
        }
    })
}

fn selection_contains(selection: ((usize, usize), (usize, usize)), point: (usize, usize)) -> bool {
    selection.0 < selection.1 && selection.0 <= point && point < selection.1
}

fn line_reference(path: &Path, rows: (usize, usize)) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|directory| directory.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    });
    let working_directory = std::env::current_dir()
        .ok()
        .and_then(|directory| std::fs::canonicalize(directory).ok());
    let shown = working_directory
        .as_deref()
        .and_then(|directory| absolute.strip_prefix(directory).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(absolute.as_path())
        .to_string_lossy()
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let first = rows.0.saturating_add(1);
    let last = rows.1.saturating_add(1);
    if first == last {
        format!("{shown}:{first}")
    } else {
        format!("{shown}:{first}-{last}")
    }
}

fn is_slash_key(input: &Input) -> bool {
    match input.key {
        Key::Char('/') => true,
        Key::Char('7') if input.shift => true,
        _ => false,
    }
}

fn is_command_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(
        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::HYPER,
    )
}

fn is_control_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(ch) if ch.eq_ignore_ascii_case(&'c'))
}

fn input_from_key(key: KeyEvent) -> Input {
    let mut input = Input::from(key);
    if is_command_modifier(key.modifiers) {
        input.ctrl = true;
    }
    input
}

fn markdown_text_area_style(theme: Theme) -> MarkdownTextAreaStyle {
    MarkdownTextAreaStyle {
        status: Style::new().fg(theme.text),
        cursor_line: Style::new().bg(theme.cursor_line),
        cursor: Style::new().bg(theme.cursor).fg(theme.cursor_text),
        selection: theme.kit.selected_row,
        hovered: theme.kit.hovered_row,
        gutter: Style::new().fg(theme.faint),
        current_gutter: Style::new().fg(theme.muted),
        scrollbar_track: theme.kit.scrollbar_track,
        scrollbar_thumb: theme.kit.scrollbar_thumb,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("untitled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unpeel_app_kit::UiComponent;

    fn render_app(theme: Theme, width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo.md");
        let mut app = App::open(path, theme).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn terminal_status_and_actions_are_both_published_editor_slots() {
        let width = 140;
        let buffer = render_app(Theme::dark(), width, 8);
        let status = row_text(&buffer, 0);
        let padding = row_text(&buffer, 1);
        let body = row_text(&buffer, 2);
        let footer = row_text(&buffer, 7);

        assert!(
            status.starts_with("  demo.md · Saved · 1:1"),
            "title sits on top: {status}"
        );
        assert!(!status.contains("Auto-save"), "the footer owns auto-save");
        assert!(padding.trim().is_empty(), "one empty row under the title");
        assert!(!body.contains("demo.md"));
        assert!(footer.starts_with("  ^N new  ^S save  auto-save on"));
    }

    #[test]
    fn semantic_editor_title_carries_the_terminal_footer_state() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo.md");
        let mut app = App::open_with_autosave(path, Theme::dark(), false).unwrap();
        app.flash("saved manually");
        let node = app.ui_node();
        let UiComponent::MarkdownEditor(editor) = node.element else {
            panic!("Markdown App must publish MarkdownEditor");
        };
        let title = editor.title.unwrap();
        assert!(title.contains("demo.md · Saved · 1:1"));
        assert!(!title.contains("Auto-save"));
        assert!(title.contains("saved manually"));
        assert!(
            editor.back.is_none(),
            "single-file mode has no list to go back to"
        );

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo.md");
        let app = App::open_with_autosave(path, Theme::dark(), false)
            .unwrap()
            .with_back_to_list(true);
        let UiComponent::MarkdownEditor(editor) = app.ui_node().element else {
            panic!("Markdown App must publish MarkdownEditor");
        };
        assert_eq!(editor.back.as_deref(), Some(UI_BACK_ACTION));
    }

    #[test]
    fn command_hint_is_one_spec_value_in_terminal_and_semantic_renderers() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("commands.md");
        std::fs::write(&path, "# Commands\n\nBody\n").unwrap();
        let mut app = App::open(path, Theme::dark()).unwrap();
        app.textarea.move_cursor(CursorMove::Jump(1, 0));

        let node = app.ui_node();
        assert!(
            node.required_capabilities()
                .contains(&"markdownCommandHint")
        );
        let UiComponent::MarkdownEditor(editor) = node.element else {
            panic!("Markdown App must publish MarkdownEditor");
        };
        assert_eq!(
            editor.command_hint.as_ref().map(|hint| hint.text.as_str()),
            Some("Type '/' for commands")
        );
        assert!(editor.command_hint_visible());

        let mut terminal = Terminal::new(TestBackend::new(60, 6)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        // Rows 0 and 1 are the title and its padding; the body starts at 2.
        assert!(row_text(terminal.backend().buffer(), 3).contains("Type '/' for commands"));
    }

    #[test]
    fn rust_reducer_alone_decides_whether_slash_opens_the_semantic_menu() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("commands.md");
        std::fs::write(&path, "body\n").unwrap();
        let mut app = App::open(path, Theme::dark()).unwrap();
        app.textarea.move_cursor(CursorMove::End);

        assert!(matches!(
            app.handle_semantic_menu_trigger(MarkdownMenuTrigger::Slash),
            UiEventOutcome::Applied
        ));
        assert_eq!(app.textarea.lines(), ["body/"]);
        assert!(
            app.menu.is_none(),
            "an ineligible slash remains ordinary text"
        );

        let blank_path = temp.path().join("blank.md");
        std::fs::write(&blank_path, "").unwrap();
        let mut blank = App::open(blank_path, Theme::dark()).unwrap();
        assert!(matches!(
            blank.handle_semantic_menu_trigger(MarkdownMenuTrigger::Slash),
            UiEventOutcome::Applied
        ));
        assert_eq!(blank.textarea.lines(), ["/"]);
        assert_eq!(
            blank.menu.as_ref().map(|menu| menu.origin),
            Some(MenuOrigin::Slash)
        );
        let UiComponent::MarkdownEditor(editor) = blank.ui_node().element else {
            panic!("Markdown App must publish MarkdownEditor");
        };
        assert!(editor.insert_menu.is_some());
        assert!(!editor.command_hint_visible());
    }

    #[test]
    fn light_and_dark_renders_use_their_contrast_palettes() {
        for theme in [Theme::light(), Theme::dark()] {
            let buffer = render_app(theme, 120, 24);
            assert_eq!(buffer[(1, 23)].fg, theme.strong, "document title color");
            let heading = (0..buffer.area.height - 1)
                .find_map(|y| {
                    (0..buffer.area.width)
                        .find(|&x| buffer[(x, y)].symbol() == "#")
                        .map(|x| (x, y))
                })
                .expect("visible editor rows contain a heading");
            assert_eq!(buffer[heading].fg, theme.kit.accent, "heading color");
            let body = (0..buffer.area.height - 1)
                .find_map(|y| {
                    (0..buffer.area.width)
                        .find(|&x| buffer[(x, y)].symbol() == "O")
                        .map(|x| (x, y))
                })
                .expect("visible editor rows contain body copy");
            assert_eq!(buffer[body].fg, theme.text, "body color");
        }
    }

    #[test]
    fn text_selection_uses_the_shared_gray_app_kit_style() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        for theme in [Theme::light(), Theme::dark()] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("selection.md");
            let text = "# Selected text";
            std::fs::write(&path, format!("{text}\n")).unwrap();
            let mut app = App::open(path, theme).unwrap();
            app.textarea.start_selection();
            app.textarea.move_cursor(CursorMove::End);

            let mut terminal = Terminal::new(TestBackend::new(60, 5)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            // Rows 0 and 1 are the title and its padding; the document starts at 2.
            let start = (0..buffer.area.width)
                .find(|column| buffer[(*column, 2)].symbol() == "#")
                .expect("selected line is visible");
            let selected_background = theme
                .kit
                .selected_row
                .bg
                .expect("App Kit selection has a background");
            let selected_foreground = theme
                .kit
                .selected_row
                .fg
                .expect("App Kit selection has a foreground");
            for column in start..start + text.len() as u16 {
                assert_eq!(
                    buffer[(column, 2)].bg,
                    selected_background,
                    "selection background at column {column}"
                );
                assert_eq!(
                    buffer[(column, 2)].fg,
                    selected_foreground,
                    "selection foreground at column {column}"
                );
            }
        }
    }

    #[test]
    fn multiline_terminal_selection_projects_as_utf16_for_native_renderers() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selection-sync.md");
        std::fs::write(&path, "alpha\n🙂 beta\n").unwrap();
        let mut app = App::open(path, Theme::dark()).unwrap();
        app.set_selection((0, 1), (1, 2));

        let node = app.ui_node();
        let unpeel_app_kit::UiComponent::MarkdownEditor(editor) = node.element else {
            panic!("Markdown App must publish the MarkdownEditor component");
        };
        assert_eq!(
            editor.selection.anchor,
            unpeel_app_kit::TextPosition::new(0, 1)
        );
        assert_eq!(
            editor.selection.head,
            unpeel_app_kit::TextPosition::new(1, 3),
            "emoji occupies two UTF-16 code units"
        );
    }

    #[test]
    fn slash_palette_and_context_publish_the_tui_menu_reducer_semantically() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("menus.md");
        std::fs::write(&path, "").unwrap();
        let mut app = App::open(path, Theme::dark()).unwrap();
        app.textarea.insert_char('/');
        app.open_menu(MenuOrigin::Slash);
        app.set_menu_selected(2);
        let visible = app.visible_menu_items();
        let expected_ids = visible
            .iter()
            .map(|item| menu_item_wire_id(item.id))
            .collect::<Vec<_>>();

        let node = app.ui_node();
        let UiComponent::MarkdownEditor(editor) = node.element else {
            panic!("Markdown App must publish MarkdownEditor");
        };
        assert_eq!(
            editor
                .actions
                .open_menu
                .as_ref()
                .map(|action| action.as_str()),
            Some(MarkdownEditorActions::OPEN_MENU)
        );
        let menu = editor.insert_menu.expect("slash menu projection");
        assert_eq!(
            menu.items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(menu.selected_id.as_deref(), Some(expected_ids[2]));
        assert!(menu.items[0].hint.as_deref().unwrap().contains('#'));
        assert_eq!(
            editor.context_menu.unwrap().presentation,
            SemanticMenuPresentation::Context
        );

        app.close_menu(true);
        app.open_menu(MenuOrigin::Palette);
        let node = app.ui_node();
        let UiComponent::MarkdownEditor(editor) = node.element else {
            panic!("Markdown App must publish MarkdownEditor");
        };
        let palette = editor.insert_menu.expect("palette projection");
        assert!(palette.item("insert-backslash").is_some());
        assert!(palette.item("toggle-autosave").is_some());
    }

    #[test]
    fn yaml_frontmatter_is_ordinary_editable_markdown() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        let source = "---\ntitle: \"Plain source\"\n---\nBody\n";
        std::fs::write(&path, source).unwrap();
        let mut app = App::open(path.clone(), Theme::dark()).unwrap();

        assert_eq!(
            app.textarea.lines(),
            ["---", "title: \"Plain source\"", "---", "Body"]
        );
        assert!(!app.handle_key_command(&KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE,)));
        app.save();
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }

    #[test]
    fn autosave_writes_a_dirty_buffer_after_the_idle_delay() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("autosave.md");
        std::fs::write(&path, "before\n").unwrap();
        let mut app = App::open_with_autosave(path.clone(), Theme::dark(), true).unwrap();
        app.textarea.move_cursor(CursorMove::End);
        app.textarea.insert_str(" after");
        app.after_edit();
        app.last_edit_at = Some(Instant::now() - AUTOSAVE_DELAY);

        app.maybe_autosave();

        assert!(!app.dirty);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "before after\n");
    }

    #[test]
    fn disabled_autosave_leaves_the_dirty_buffer_on_disk_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("manual.md");
        std::fs::write(&path, "before\n").unwrap();
        let mut app = App::open_with_autosave(path.clone(), Theme::dark(), false).unwrap();
        app.textarea.move_cursor(CursorMove::End);
        app.textarea.insert_str(" after");
        app.after_edit();
        app.last_edit_at = Some(Instant::now() - AUTOSAVE_DELAY);

        app.maybe_autosave();

        assert!(app.dirty);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "before\n");
    }

    #[test]
    fn repeated_backspace_events_delete_one_character_each() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repeat.md");
        std::fs::write(&path, "abc\n").unwrap();
        let mut app = App::open(path, Theme::dark()).unwrap();
        app.textarea.move_cursor(CursorMove::End);
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Backspace, KeyModifiers::NONE, KeyEventKind::Repeat);

        app.handle_key_event(repeat);
        app.handle_key_event(repeat);

        assert_eq!(app.textarea.lines(), ["a"]);
    }

    #[test]
    fn repeated_arrow_events_keep_moving_the_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repeat.md");
        std::fs::write(&path, "abcd\n").unwrap();
        let mut app = App::open(path, Theme::dark()).unwrap();
        app.textarea.move_cursor(CursorMove::End);
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Left, KeyModifiers::NONE, KeyEventKind::Repeat);

        app.handle_key_event(repeat);
        app.handle_key_event(repeat);

        assert_eq!(app.textarea.cursor(), (0, 2));
    }

    #[test]
    fn down_from_the_final_content_line_appends_only_one_empty_line() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("down.md");
        std::fs::write(&path, "last line\n").unwrap();
        let mut app = App::open_with_autosave(path, Theme::dark(), false).unwrap();
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);

        app.handle_key_event(down);
        app.handle_key_event(down);

        assert_eq!(app.textarea.lines(), ["last line", ""]);
        assert_eq!(app.textarea.cursor(), (1, 0));
        assert!(app.dirty);
    }

    #[test]
    fn control_c_exits_even_while_the_context_menu_is_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("exit.md");
        std::fs::write(&path, "text\n").unwrap();
        let mut app = App::open(path, Theme::dark()).unwrap();
        app.context_menu = Some(editor_context_menu(
            "exit.md:1".to_string(),
            false,
            Position::new(0, 0),
            Theme::dark(),
        ));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(app.exit);
    }

    #[test]
    fn editor_context_menu_matches_the_diff_viewer_agent_handoff() {
        let reference = "notes/demo.md:2-4".to_string();
        let anchor = Position::new(4, 4);
        let with_agent = editor_context_menu(reference.clone(), true, anchor, Theme::dark());
        assert_eq!(with_agent.items().len(), 2);
        assert_eq!(with_agent.items()[0].label(), "Send to agent");
        assert_eq!(
            with_agent.selected_action(),
            Some(ContextAction::SendToAgent(reference.clone()))
        );
        assert_eq!(with_agent.items()[1].label(), "Copy reference");

        let without_agent = editor_context_menu(reference.clone(), false, anchor, Theme::dark());
        assert_eq!(without_agent.items().len(), 1);
        assert_eq!(without_agent.items()[0].label(), "Copy reference");
        assert_eq!(
            without_agent.selected_action(),
            Some(ContextAction::CopyReference(reference))
        );
    }

    #[test]
    fn line_references_include_the_clicked_or_selected_markdown_rows() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo.md");
        assert_eq!(line_reference(&path, (0, 0)), "demo.md:1");
        assert_eq!(line_reference(&path, (1, 3)), "demo.md:2-4");
        assert!(selection_contains(((1, 2), (3, 4)), (2, 0)));
        assert!(!selection_contains(((1, 2), (3, 4)), (3, 4)));
        assert_eq!(
            normalized_selection(Some(((3, 4), (1, 2)))),
            Some(((1, 2), (3, 4)))
        );
    }
}
