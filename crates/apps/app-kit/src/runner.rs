//! Minimal App authoring: one struct with a `page()` builder and a
//! `reduce()` function. The runner owns the terminal, the event loop, the
//! optional hosted bridge, revisions, and deltas.
//!
//! See `docs/writing-an-app.md` for the short guide.

use std::io;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};

use crate::{
    AppMetadata, InputField, InputFieldAction, KitTheme, ListItem, ListKeymap,
    ListNavigationOutcome, ListState, Page, PageBodySlot, PagePointerDecision, PageTheme,
    RowKeyDecision, RowPointerDecision, RowPrimaryRole,
};

const TICK: Duration = Duration::from_millis(80);
#[cfg(feature = "ui-bridge")]
const VIEW_ID: &str = "main";

/// An App is a model plus one `Page` projection and one reducer.
///
/// The runner turns terminal keys, mouse events, and hosted renderer events
/// into the closed [`AppAction`] vocabulary, calls [`App::reduce`], and
/// republishes the `Page` when the reducer reports a change. Apps never see
/// the terminal, the bridge, revisions, or deltas.
pub trait App {
    /// The complete screen for the current model. Called after every change,
    /// so keep it a pure function of the model with stable ids.
    fn page(&self) -> Page;

    /// Applies one action to the model.
    fn reduce(&mut self, action: AppAction) -> Reduce;

    /// Called roughly every 80 ms while the App is idle, for spinners and
    /// background work. Return `true` when the page should be rebuilt.
    fn tick(&mut self) -> bool {
        false
    }

    /// Terminal-only extras such as the spinner frame for busy rows.
    fn spinner_frame(&self) -> usize {
        0
    }
}

/// What the reducer did with an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduce {
    /// The model changed; the runner rebuilds and republishes the page.
    Changed,
    /// Nothing changed; nothing is published.
    Ignored,
    /// Leave the App.
    Quit,
}

/// A renderer value carried by the escape-hatch [`AppAction::Change`].
#[derive(Clone, Debug, PartialEq)]
pub enum AppValue {
    None,
    Bool(bool),
    Integer(i64),
    Number(f64),
    Text(String),
    TextList(Vec<String>),
}

/// The closed set of things a user can do to a `Page`, derived by the runner
/// from terminal input and from hosted renderer events alike.
#[derive(Clone, Debug, PartialEq)]
pub enum AppAction {
    /// Enter (or the native submit button) in the header `Input`. The runner
    /// clears the terminal input when the reducer reports a change.
    Submit { input: String, text: String },
    /// A row's activate action: Enter, click, or a Disclosure chevron.
    Activate { item: String, action: String },
    /// The row's declared delete action (`d` or Delete in the terminal, the
    /// trash button natively).
    Delete { item: String },
    /// The list's selection moved and the list declares a select action.
    Select { item: String },
    /// A Toggle or Checkmark control inside a row flipped.
    Toggle {
        item: String,
        control: String,
        on: bool,
    },
    /// A footer action, by its declared action id.
    Command { action: String },
    /// The Page's back action: Escape, the `‹` chevron, or Backspace.
    Back,
    /// Escape on a Page without a back action. Returning [`Reduce::Ignored`]
    /// quits the App.
    Cancel,
    /// Everything else, in raw form: the node id, action id, and value.
    Change {
        node: String,
        action: String,
        value: AppValue,
    },
}

/// Whether the runner keeps going after an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// Runs an App in the terminal until it quits.
///
/// Raw mode, the alternate screen, and mouse capture are restored on exit or
/// panic. With the `ui-bridge` feature the same process also serves hosted
/// SwiftUI and web renderers when an Unpeel Host injects an endpoint.
pub fn run_app(app: impl App, metadata: AppMetadata) -> io::Result<()> {
    let mut session = Session::new(app, metadata)?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _restore = TerminalRestore;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;
    let result = session.run_loop(&mut terminal);
    terminal.show_cursor()?;
    result
}

struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

/// The runner's state for one App: current page, terminal focus, and the
/// hosted projection. Exposed so tests can drive it without a terminal.
pub struct Session<A: App> {
    app: A,
    page: Page,
    list_state: ListState,
    input: InputField,
    input_focused: bool,
    theme: PageTheme,
    area: Rect,
    #[cfg(feature = "ui-bridge")]
    bridge: Option<crate::UiBridge>,
    #[cfg(feature = "ui-bridge")]
    revision: u64,
}

impl<A: App> Session<A> {
    /// Creates a session and, with `ui-bridge`, binds the Host endpoint.
    pub fn new(app: A, metadata: AppMetadata) -> io::Result<Self> {
        #[cfg(feature = "ui-bridge")]
        let bridge = crate::UiBridge::detect(metadata)
            .map_err(|error| io::Error::other(error.to_string()))?;
        #[cfg(not(feature = "ui-bridge"))]
        let _ = metadata;
        #[allow(unused_mut)]
        let mut session = Self::with_theme(app, KitTheme::detected());
        #[cfg(feature = "ui-bridge")]
        {
            session.bridge = Some(bridge);
            session.publish_snapshot()?;
        }
        Ok(session)
    }

    /// A session without any hosted endpoint, for tests and embedding.
    #[must_use]
    pub fn with_theme(app: A, theme: KitTheme) -> Self {
        let page = app.page();
        let mut input = InputField::new("");
        let input_focused = page.input_spec().is_some();
        input.set_focused(input_focused);
        let selected = list_of(&page).and_then(|list| {
            list.selected_id
                .as_deref()
                .and_then(|id| list.items.iter().position(|item| item.id == id))
                .or_else(|| (!list.items.is_empty()).then_some(0))
        });
        Self {
            app,
            page,
            list_state: ListState::new(selected),
            input,
            input_focused,
            theme: PageTheme::for_theme(theme),
            area: Rect::default(),
            #[cfg(feature = "ui-bridge")]
            bridge: None,
            #[cfg(feature = "ui-bridge")]
            revision: 1,
        }
    }

    #[must_use]
    pub fn app(&self) -> &A {
        &self.app
    }

    #[must_use]
    pub fn page(&self) -> &Page {
        &self.page
    }

    #[must_use]
    pub const fn list_state(&self) -> &ListState {
        &self.list_state
    }

    #[must_use]
    pub const fn input_focused(&self) -> bool {
        self.input_focused
    }

    /// Current hosted revision (1 before any change).
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        loop {
            #[cfg(feature = "ui-bridge")]
            if self.pump_bridge()? == Flow::Quit {
                return Ok(());
            }
            if self.should_render_terminal() {
                terminal.draw(|frame| self.render(frame))?;
            }
            if event::poll(TICK)? {
                let flow = match event::read()? {
                    Event::Key(key) => self.handle_key(key)?,
                    Event::Mouse(mouse) => self.handle_mouse(mouse)?,
                    Event::Paste(text) => self.handle_paste(text),
                    Event::Resize(_, _) => {
                        terminal.autoresize()?;
                        Flow::Continue
                    }
                    _ => Flow::Continue,
                };
                if flow == Flow::Quit {
                    return Ok(());
                }
            } else {
                self.tick()?;
            }
        }
    }

    fn should_render_terminal(&self) -> bool {
        #[cfg(feature = "ui-bridge")]
        {
            self.bridge
                .as_ref()
                .is_none_or(crate::UiBridge::should_render_terminal)
        }
        #[cfg(not(feature = "ui-bridge"))]
        {
            true
        }
    }

    /// Advances the App's idle work and rebuilds the page if asked.
    pub fn tick(&mut self) -> io::Result<()> {
        if self.app.tick() {
            self.rebuild()?;
        }
        Ok(())
    }

    /// Draws the page; the terminal input cursor follows the header field.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        self.area = frame.area();
        self.list_state.set_spinner_frame(self.app.spinner_frame());
        let theme = self.theme;
        frame.render_widget(
            self.page
                .widget(&mut self.input, &mut self.list_state)
                .theme(theme),
            self.area,
        );
        if self.input_focused
            && let Some(position) = self.input.cursor_position()
        {
            frame.set_cursor_position(position);
        }
    }

    /// Converts one key into actions and applies them.
    pub fn handle_key(&mut self, key: KeyEvent) -> io::Result<Flow> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Ok(Flow::Continue);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('c') {
            return Ok(Flow::Quit);
        }
        if let Some(action) = self.page.footer.action_for_key(&key)
            && !action.disabled
        {
            let action = action.action.clone();
            return self.apply(AppAction::Command { action });
        }
        let Some(action) = self.key_action(key) else {
            return Ok(Flow::Continue);
        };
        self.apply(action)
    }

    /// Maps a key to an action without applying it (terminal focus and list
    /// navigation side effects still happen).
    pub fn key_action(&mut self, key: KeyEvent) -> Option<AppAction> {
        let has_input = self.page.input_spec().is_some();
        let has_list = list_of(&self.page).is_some_and(|list| !list.items.is_empty());
        if has_input && self.input_focused {
            return self.input_key_action(key, has_list);
        }
        let modifiers = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Tab | KeyCode::Char('i') if has_input => {
                self.focus_input(true);
                None
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Left => {
                Some(self.back_or_cancel())
            }
            KeyCode::Delete | KeyCode::Char('d') => self
                .selected_item()
                .filter(|item| item.delete.is_some())
                .map(|item| AppAction::Delete {
                    item: item.id.clone(),
                }),
            // Shared list keys (arrows, j/k, Space, Enter, paging) win over
            // typing; other printable characters start editing the input.
            _ if self.list_keymap_recognizes(&key) => self.list_key_action(key),
            KeyCode::Char(character) if has_input && !modifiers => {
                self.focus_input(true);
                self.input.handle(InputFieldAction::Insert(character));
                None
            }
            _ => None,
        }
    }

    fn input_key_action(&mut self, key: KeyEvent, has_list: bool) -> Option<AppAction> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let word = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter => {
                let input = self.page.input_spec()?;
                input.submit.as_ref()?;
                Some(AppAction::Submit {
                    input: input.id.clone(),
                    text: self.input.text().to_owned(),
                })
            }
            KeyCode::Esc => Some(self.back_or_cancel()),
            KeyCode::Tab | KeyCode::Up | KeyCode::Down if has_list => {
                self.focus_input(false);
                None
            }
            KeyCode::Backspace => {
                self.input.handle(InputFieldAction::Backspace);
                self.input_changed()
            }
            KeyCode::Delete => {
                self.input.handle(InputFieldAction::Delete);
                self.input_changed()
            }
            KeyCode::Left => {
                self.input.handle(InputFieldAction::Left {
                    extend: shift,
                    word,
                });
                None
            }
            KeyCode::Right => {
                self.input.handle(InputFieldAction::Right {
                    extend: shift,
                    word,
                });
                None
            }
            KeyCode::Home => {
                self.input.handle(InputFieldAction::Home { extend: shift });
                None
            }
            KeyCode::End => {
                self.input.handle(InputFieldAction::End { extend: shift });
                None
            }
            KeyCode::Char('a') if control => {
                self.input.handle(InputFieldAction::SelectAll);
                None
            }
            KeyCode::Char('u') if control => {
                self.input.handle(InputFieldAction::Clear);
                self.input_changed()
            }
            KeyCode::Char(character) if !word => {
                self.input.handle(InputFieldAction::Insert(character));
                self.input_changed()
            }
            _ => None,
        }
    }

    /// A header `Input` with a set-value action reports every edit.
    fn input_changed(&self) -> Option<AppAction> {
        let input = self.page.input_spec()?;
        let action = input.set_value.clone()?;
        Some(AppAction::Change {
            node: input.id.clone(),
            action,
            value: AppValue::Text(self.input.text().to_owned()),
        })
    }

    fn list_keymap_recognizes(&self, key: &KeyEvent) -> bool {
        let role = self
            .selected_item()
            .map_or(RowPrimaryRole::Static, ListItem::primary_role);
        ListKeymap::new().decision_for_key(key, role).is_some()
    }

    fn list_key_action(&mut self, key: KeyEvent) -> Option<AppAction> {
        let role = self
            .selected_item()
            .map_or(RowPrimaryRole::Static, ListItem::primary_role);
        let keymap = ListKeymap::new()
            .space_pages_down(list_of(&self.page).is_some_and(|list| list.space_pages_down));
        match keymap.decision_for_key(&key, role)? {
            RowKeyDecision::InvokePrimary => self.selected_item().and_then(primary_action),
            RowKeyDecision::Navigate(action) => {
                let count = list_of(&self.page).map_or(0, |list| list.items.len());
                match self.page.navigate(&mut self.list_state, action) {
                    ListNavigationOutcome::Back => Some(self.back_or_cancel()),
                    ListNavigationOutcome::SelectionChanged(index) => self.selection_action(index),
                    ListNavigationOutcome::Activate(index) => list_of(&self.page)
                        .and_then(|list| list.items.get(index))
                        .and_then(primary_action),
                    ListNavigationOutcome::None
                    | ListNavigationOutcome::Scrolled(_)
                    | ListNavigationOutcome::FocusedBack => {
                        let _ = count;
                        None
                    }
                }
            }
        }
    }

    fn back_or_cancel(&self) -> AppAction {
        if self.page.back.is_some() {
            AppAction::Back
        } else {
            AppAction::Cancel
        }
    }

    fn selection_action(&self, index: usize) -> Option<AppAction> {
        let list = list_of(&self.page)?;
        list.select.as_ref()?;
        let item = list.items.get(index)?;
        Some(AppAction::Select {
            item: item.id.clone(),
        })
    }

    fn selected_item(&self) -> Option<&ListItem> {
        let list = list_of(&self.page)?;
        list.items.get(self.list_state.selected()?)
    }

    fn focus_input(&mut self, focused: bool) {
        self.input_focused = focused;
        self.input.set_focused(focused);
    }

    /// Converts one mouse event into an action and applies it.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> io::Result<Flow> {
        let Some(action) = self.mouse_action(mouse) else {
            return Ok(Flow::Continue);
        };
        self.apply(action)
    }

    /// Maps a mouse event to an action without applying it.
    pub fn mouse_action(&mut self, mouse: MouseEvent) -> Option<AppAction> {
        let position = Position::new(mouse.column, mouse.row);
        let area = self.area;
        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left)
                if self.input.area().contains(position) =>
            {
                self.focus_input(true);
                self.input
                    .mouse_down(position, mouse.modifiers.contains(KeyModifiers::SHIFT));
                self.list_state.track_mouse(&mouse);
                return None;
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left)
                if self.input.is_dragging() =>
            {
                self.input.mouse_drag(position);
                return None;
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) if self.input.is_dragging() => {
                self.input.mouse_up();
                return None;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let count = list_of(&self.page).map_or(0, |list| list.items.len());
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                };
                self.list_state.scroll_by(delta, count);
                return None;
            }
            _ => {}
        }
        let decision = self
            .page
            .pointer_decision(&mut self.list_state, &mouse, area)?;
        let action = match decision {
            PagePointerDecision::Footer(action) => {
                if action.disabled {
                    return None;
                }
                AppAction::Command {
                    action: action.action.clone(),
                }
            }
            PagePointerDecision::Back(_) => AppAction::Back,
            PagePointerDecision::List(RowPointerDecision::Select(index)) => {
                self.focus_input(false);
                self.selection_action(index)?
            }
            PagePointerDecision::List(RowPointerDecision::InvokePrimary(index)) => {
                self.focus_input(false);
                let item = list_of(&self.page)?.items.get(index)?;
                primary_action(item)?
            }
            PagePointerDecision::Activate { node_id, action } => AppAction::Activate {
                item: node_id.to_owned(),
                action: action.to_owned(),
            },
        };
        Some(action)
    }

    /// Bracketed paste goes into the focused header input.
    pub fn handle_paste(&mut self, text: String) -> Flow {
        if self.input_focused && self.page.input_spec().is_some() {
            self.input.handle(InputFieldAction::InsertText(text));
        }
        Flow::Continue
    }

    /// Applies one action: reduces, then rebuilds and republishes on change.
    pub fn apply(&mut self, action: AppAction) -> io::Result<Flow> {
        let submitted = matches!(action, AppAction::Submit { .. });
        let cancel = matches!(action, AppAction::Cancel);
        match self.app.reduce(action) {
            Reduce::Changed => {
                if submitted {
                    self.input.clear();
                }
                self.rebuild()?;
                Ok(Flow::Continue)
            }
            Reduce::Ignored if cancel => Ok(Flow::Quit),
            Reduce::Ignored => Ok(Flow::Continue),
            Reduce::Quit => Ok(Flow::Quit),
        }
    }

    /// Rebuilds the page from the model and publishes the difference.
    fn rebuild(&mut self) -> io::Result<()> {
        let next = self.app.page();
        if next == self.page {
            return Ok(());
        }
        #[cfg(feature = "ui-bridge")]
        self.publish_delta(&next)?;
        self.page = next;
        if let Some(list) = list_of(&self.page) {
            let count = list.items.len();
            let selected = list
                .selected_id
                .as_deref()
                .and_then(|id| list.items.iter().position(|item| item.id == id))
                .or(self.list_state.selected());
            self.list_state.select(selected, count);
        }
        if self.page.input_spec().is_none() {
            self.focus_input(false);
        }
        Ok(())
    }
}

/// The action a row's primary role produces.
fn primary_action(item: &ListItem) -> Option<AppAction> {
    if let Some(toggle) = item.primary_toggle() {
        return Some(AppAction::Toggle {
            item: item.id.clone(),
            control: toggle.id.clone(),
            on: !toggle.value,
        });
    }
    if let Some(checkmark) = item.primary_checkmark() {
        return Some(AppAction::Toggle {
            item: item.id.clone(),
            control: checkmark.id.clone(),
            on: !checkmark.value,
        });
    }
    if let Some(action) = &item.activate {
        return Some(AppAction::Activate {
            item: item.id.clone(),
            action: action.clone(),
        });
    }
    if let Some(sparkline) = item.primary_sparkline()
        && let Some(action) = &sparkline.activate
    {
        return Some(AppAction::Activate {
            item: sparkline.id.clone(),
            action: action.clone(),
        });
    }
    let gauge = item.primary_gauge()?;
    let action = gauge.activate.as_ref()?;
    Some(AppAction::Activate {
        item: gauge.id.clone(),
        action: action.clone(),
    })
}

fn list_of(page: &Page) -> Option<&crate::List> {
    match &page.body {
        PageBodySlot::List(list) => Some(list),
        _ => None,
    }
}

#[cfg(feature = "ui-bridge")]
impl<A: App> Session<A> {
    const ROOT_ID: &'static str = "app-page";

    fn node(page: &Page) -> crate::UiNode {
        crate::UiNode::page(Self::ROOT_ID, page.clone())
    }

    fn publish_snapshot(&mut self) -> io::Result<()> {
        let node = Self::node(&self.page);
        if let Some(bridge) = self.bridge.as_mut() {
            bridge
                .publish(VIEW_ID, self.revision, node)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        Ok(())
    }

    /// Publishes `next` as a delta from the current page, falling back to a
    /// full snapshot when the diff cannot be expressed as operations.
    fn publish_delta(&mut self, next: &Page) -> io::Result<()> {
        let operations = Self::delta_operations(&self.page, next);
        let base = self.revision;
        self.revision = self.revision.saturating_add(1);
        let Some(bridge) = self.bridge.as_mut() else {
            return Ok(());
        };
        let delta = bridge.publish_delta(VIEW_ID, base, self.revision, operations);
        if delta.is_err() {
            bridge
                .publish(VIEW_ID, self.revision, Self::node(next))
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        Ok(())
    }

    /// The compact operations between two pages (a root replacement when
    /// the pages are not the same component).
    #[must_use]
    pub fn delta_operations(previous: &Page, next: &Page) -> Vec<crate::UiDeltaOperation> {
        crate::page_delta_operations(&Self::node(previous), &Self::node(next))
    }

    /// Drains hosted renderer events, applying each as an [`AppAction`].
    fn pump_bridge(&mut self) -> io::Result<Flow> {
        loop {
            let Some(bridge) = self.bridge.as_mut() else {
                return Ok(Flow::Continue);
            };
            let Some(event) = bridge
                .poll()
                .map_err(|error| io::Error::other(error.to_string()))?
            else {
                return Ok(Flow::Continue);
            };
            let crate::UiBridgeEvent::Action { event, .. } = event else {
                continue;
            };
            let (outcome, flow) = if event.base_revision != self.revision {
                (
                    crate::UiEventOutcome::Rejected(format!(
                        "page changed from revision {} to {}; retry the action",
                        event.base_revision, self.revision
                    )),
                    Flow::Continue,
                )
            } else {
                let action = self.action_from_ui(&event.action);
                if let AppAction::Change { node, value, .. } = &action
                    && let Some(input) = self.page.input_spec()
                    && input.id == *node
                    && let AppValue::Text(text) = value
                {
                    self.input.set_text(text.clone());
                }
                let flow = self.apply(action)?;
                (crate::UiEventOutcome::Applied, flow)
            };
            if let Some(bridge) = self.bridge.as_mut() {
                bridge
                    .acknowledge(&event, outcome, self.revision)
                    .map_err(|error| io::Error::other(error.to_string()))?;
            }
            if flow == Flow::Quit {
                return Ok(Flow::Quit);
            }
        }
    }

    /// Translates a renderer's typed action into the App vocabulary using
    /// the published page. Unknown shapes arrive as [`AppAction::Change`].
    #[must_use]
    pub fn action_from_ui(&self, action: &crate::UiAction) -> AppAction {
        use crate::{UiEventKind, UiEventValue};
        let node = action.node_id.as_str();
        let id = action.action.as_str();
        let page = &self.page;
        if let Some(input) = page.input_spec()
            && input.id == node
            && input.submit.as_deref() == Some(id)
            && let UiEventValue::Text(text) = &action.value
        {
            return AppAction::Submit {
                input: input.id.clone(),
                text: text.clone(),
            };
        }
        if page.back.as_deref() == Some(id) && action.kind == UiEventKind::Cancel {
            return AppAction::Back;
        }
        if page
            .footer
            .actions
            .iter()
            .any(|footer| footer.id == node && footer.action == id)
        {
            return AppAction::Command {
                action: id.to_owned(),
            };
        }
        if let Some(list) = list_of(page) {
            if list.id == node
                && list.select.as_deref() == Some(id)
                && let UiEventValue::Text(item) = &action.value
            {
                return AppAction::Select { item: item.clone() };
            }
            for item in &list.items {
                if item.id == node {
                    if item.activate.as_deref() == Some(id) {
                        return AppAction::Activate {
                            item: item.id.clone(),
                            action: id.to_owned(),
                        };
                    }
                    if item.delete.as_deref() == Some(id) {
                        return AppAction::Delete {
                            item: item.id.clone(),
                        };
                    }
                }
                if let Some(toggle) = item.primary_toggle()
                    && toggle.id == node
                    && toggle.set_value == id
                    && let UiEventValue::Bool(on) = action.value
                {
                    return AppAction::Toggle {
                        item: item.id.clone(),
                        control: toggle.id.clone(),
                        on,
                    };
                }
                if let Some(checkmark) = item.primary_checkmark()
                    && checkmark.id == node
                    && checkmark.set_value == id
                    && let UiEventValue::Bool(on) = action.value
                {
                    return AppAction::Toggle {
                        item: item.id.clone(),
                        control: checkmark.id.clone(),
                        on,
                    };
                }
            }
        }
        AppAction::Change {
            node: node.to_owned(),
            action: id.to_owned(),
            value: match &action.value {
                UiEventValue::None => AppValue::None,
                UiEventValue::Bool(value) => AppValue::Bool(*value),
                UiEventValue::Index(value) => {
                    AppValue::Integer(i64::try_from(*value).unwrap_or(i64::MAX))
                }
                UiEventValue::Integer(value) => AppValue::Integer(*value),
                UiEventValue::Number(value) => AppValue::Number(*value),
                UiEventValue::Text(value) => AppValue::Text(value.clone()),
                UiEventValue::TextList(values) => AppValue::TextList(values.clone()),
                UiEventValue::TextEdit(edit) => AppValue::Text(edit.text.clone()),
                UiEventValue::TextSelection(_) => AppValue::None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::MouseButton;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::{FooterAction, Input, List, ListItemSlot, Toggle};

    #[derive(Default)]
    struct Todos {
        items: Vec<(u64, String, bool)>,
        selected: Option<u64>,
        log: Vec<AppAction>,
        next: u64,
    }

    impl Todos {
        fn seeded() -> Self {
            Self {
                items: vec![(1, "one".into(), false), (2, "two".into(), true)],
                selected: Some(1),
                log: Vec::new(),
                next: 3,
            }
        }
    }

    impl App for Todos {
        fn page(&self) -> Page {
            let items = self
                .items
                .iter()
                .map(|(id, label, done)| {
                    ListItem::new(format!("todo-{id}"), label.clone())
                        .done(*done)
                        .trailing(ListItemSlot::toggle(Toggle::new(
                            format!("todo-{id}-toggle"),
                            "Done",
                            *done,
                            "set-done",
                        )))
                        .delete_action("delete")
                })
                .collect();
            let mut list = List::new("todos", items);
            if let Some(id) = self.selected {
                list = list.selected(format!("todo-{id}"), "select");
            }
            Page::new("Todos", list)
                .input(Input::new("new", "New").submit_action("add"))
                .footer_actions([
                    FooterAction::new("refresh", "refresh", "refresh-all").accelerator("r")
                ])
        }

        fn reduce(&mut self, action: AppAction) -> Reduce {
            self.log.push(action.clone());
            match action {
                AppAction::Submit { text, .. } => {
                    self.items.push((self.next, text, false));
                    self.next += 1;
                    Reduce::Changed
                }
                AppAction::Toggle { item, on, .. } => {
                    let id: u64 = item.trim_start_matches("todo-").parse().unwrap();
                    self.items.iter_mut().find(|entry| entry.0 == id).unwrap().2 = on;
                    Reduce::Changed
                }
                AppAction::Delete { item } => {
                    let id: u64 = item.trim_start_matches("todo-").parse().unwrap();
                    self.items.retain(|entry| entry.0 != id);
                    Reduce::Changed
                }
                AppAction::Select { item } => {
                    self.selected = item.trim_start_matches("todo-").parse().ok();
                    Reduce::Changed
                }
                AppAction::Command { .. } => Reduce::Ignored,
                AppAction::Cancel => Reduce::Quit,
                _ => Reduce::Ignored,
            }
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn draw(session: &mut Session<Todos>) {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|frame| session.render(frame)).unwrap();
    }

    #[test]
    fn keys_become_typed_actions() {
        let mut session = Session::with_theme(Todos::seeded(), KitTheme::dark());
        draw(&mut session);
        assert!(session.input_focused(), "a Page with an Input starts there");

        session.handle_key(key(KeyCode::Char('x'))).unwrap();
        assert_eq!(
            session.handle_key(key(KeyCode::Enter)).unwrap(),
            Flow::Continue
        );
        assert_eq!(
            session.app().log.last(),
            Some(&AppAction::Submit {
                input: "new".into(),
                text: "x".into()
            })
        );
        assert_eq!(
            session.input.text(),
            "",
            "submit clears the input on change"
        );
        assert_eq!(session.app().items.len(), 3);

        session.handle_key(key(KeyCode::Tab)).unwrap();
        assert!(!session.input_focused());
        session.handle_key(key(KeyCode::Down)).unwrap();
        assert_eq!(
            session.app().log.last(),
            Some(&AppAction::Select {
                item: "todo-2".into()
            })
        );
        session.handle_key(key(KeyCode::Char(' '))).unwrap();
        assert_eq!(
            session.app().log.last(),
            Some(&AppAction::Toggle {
                item: "todo-2".into(),
                control: "todo-2-toggle".into(),
                on: false
            })
        );
        session.handle_key(key(KeyCode::Char('d'))).unwrap();
        assert_eq!(
            session.app().log.last(),
            Some(&AppAction::Delete {
                item: "todo-2".into()
            })
        );
        session.handle_key(key(KeyCode::Char('r'))).unwrap();
        assert_eq!(
            session.app().log.last(),
            Some(&AppAction::Command {
                action: "refresh-all".into()
            })
        );
        assert_eq!(session.handle_key(key(KeyCode::Esc)).unwrap(), Flow::Quit);
        assert_eq!(session.app().log.last(), Some(&AppAction::Cancel));
    }

    #[test]
    fn mouse_clicks_become_typed_actions() {
        let mut session = Session::with_theme(Todos::seeded(), KitTheme::dark());
        draw(&mut session);
        let rows = session.list_state().rows_area();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rows.x + 4,
            row: rows.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        session.handle_mouse(click).unwrap();
        assert_eq!(
            session.app().log.last(),
            Some(&AppAction::Toggle {
                item: "todo-2".into(),
                control: "todo-2-toggle".into(),
                on: false
            })
        );
        assert!(!session.input_focused(), "clicking a row leaves the input");
    }

    #[test]
    fn ignored_reductions_publish_nothing() {
        let mut session = Session::with_theme(Todos::seeded(), KitTheme::dark());
        draw(&mut session);
        let before = session.page().clone();
        session.handle_key(key(KeyCode::Tab)).unwrap();
        session.handle_key(key(KeyCode::Char('r'))).unwrap();
        assert_eq!(session.page(), &before);
        #[cfg(feature = "ui-bridge")]
        assert_eq!(session.revision(), 1);
    }

    #[cfg(feature = "ui-bridge")]
    #[test]
    fn page_diffs_are_compact_deltas_and_renderer_actions_round_trip() {
        use crate::{UiAction, UiDeltaOperation, UiEventKind, UiEventValue};

        let mut session = Session::with_theme(Todos::seeded(), KitTheme::dark());
        draw(&mut session);
        let before = session.page().clone();

        // Insert.
        let inserted = session.action_from_ui(&UiAction::new(
            "new",
            "add",
            UiEventKind::Submit,
            UiEventValue::Text("three".into()),
        ));
        assert!(matches!(inserted, AppAction::Submit { .. }));
        session.apply(inserted).unwrap();
        let ops = Session::<Todos>::delta_operations(&before, session.page());
        assert!(
            ops.iter()
                .any(|op| matches!(op, UiDeltaOperation::ListInsertItem { .. })),
            "{ops:?}"
        );
        assert_eq!(session.revision(), 2);

        // Update.
        let before = session.page().clone();
        let toggled = session.action_from_ui(&UiAction::new(
            "todo-1-toggle",
            "set-done",
            UiEventKind::Change,
            UiEventValue::Bool(true),
        ));
        assert_eq!(
            toggled,
            AppAction::Toggle {
                item: "todo-1".into(),
                control: "todo-1-toggle".into(),
                on: true
            }
        );
        session.apply(toggled).unwrap();
        let ops = Session::<Todos>::delta_operations(&before, session.page());
        assert!(
            ops.iter()
                .any(|op| matches!(op, UiDeltaOperation::ToggleSetValue { .. })),
            "{ops:?}"
        );

        // Select.
        let before = session.page().clone();
        let selected = session.action_from_ui(&UiAction::new(
            "todos",
            "select",
            UiEventKind::Change,
            UiEventValue::Text("todo-2".into()),
        ));
        assert_eq!(
            selected,
            AppAction::Select {
                item: "todo-2".into()
            }
        );
        session.apply(selected).unwrap();
        let ops = Session::<Todos>::delta_operations(&before, session.page());
        assert!(
            ops.iter()
                .any(|op| matches!(op, UiDeltaOperation::ListSetSelection { .. })),
            "{ops:?}"
        );

        // Remove.
        let before = session.page().clone();
        let removed = session.action_from_ui(&UiAction::new(
            "todo-2",
            "delete",
            UiEventKind::Change,
            UiEventValue::None,
        ));
        assert_eq!(
            removed,
            AppAction::Delete {
                item: "todo-2".into()
            }
        );
        session.apply(removed).unwrap();
        let ops = Session::<Todos>::delta_operations(&before, session.page());
        assert!(
            ops.iter()
                .any(|op| matches!(op, UiDeltaOperation::ListRemoveItem { .. })),
            "{ops:?}"
        );

        // Footer and unknown shapes.
        assert_eq!(
            session.action_from_ui(&UiAction::activate("refresh", "refresh-all")),
            AppAction::Command {
                action: "refresh-all".into()
            }
        );
        assert!(matches!(
            session.action_from_ui(&UiAction::new(
                "mystery",
                "poke",
                UiEventKind::Command,
                UiEventValue::Integer(3)
            )),
            AppAction::Change {
                value: AppValue::Integer(3),
                ..
            }
        ));
    }
}
