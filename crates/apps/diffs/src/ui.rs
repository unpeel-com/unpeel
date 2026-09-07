//! Borderless changed-file list and unified-diff detail surface.

use std::io::{self, Stdout, Write as _};
use std::ops::{Deref, DerefMut};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::buffer::Buffer;
#[cfg(test)]
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
#[cfg(test)]
use ratatui::style::{Color, Modifier};
#[cfg(test)]
use ratatui::text::{Line, Span};
#[cfg(test)]
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;
#[cfg(test)]
use unpeel_app_kit::UiDeltaOperation;
use unpeel_app_kit::{
    AgentBridge, AppContext, AppMetadata, AppReporter, ColorScheme, Content, ContentEmphasis,
    ContentFont, ContentLine, ContentLineTone, ContentRun, ContentSelection, ContentState,
    ContentTheme, ContentTone, DragSurface, EditorBridge, FooterAction, InputField,
    KeyboardEnhancementGuard, KitTheme, List, ListItem, ListItemSlot, ListItemTone, ListKeymap,
    ListNavigationAction, ListState, MenuTheme, Page, PageTheme, PopupMenu, SemanticMenu,
    SemanticMenuAnchor, SemanticMenuItem, SemanticMenuPresentation, StatusSymbol, ThemeMonitor,
    UiAction, UiBridge, UiBridgeEvent, UiComponent, UiEventKind, UiEventOutcome, UiEventValue,
    UiNode, clipboard_sequence, page_delta_operations,
};
#[cfg(test)]
use unpeel_app_kit::{SELECTABLE_LEFT_PADDING, VerticalScrollbar};

use crate::app::{App, Screen};
use crate::git::{ChangedFile, DiffDocument};
#[cfg(test)]
use crate::highlight::{DocumentColors, Highlighter};

#[cfg(test)]
const DETAIL_GAP_ROWS: u16 = 1;
#[cfg(test)]
const DETAIL_META_ROWS: u16 = 1;
const AUTO_SYNC_INTERVAL: Duration = Duration::from_millis(1000);
const UI_VIEW_ID: &str = "main";
const SEMANTIC_ROOT_ID: &str = "diffs-page";
const FILE_LIST_ID: &str = "diff-files";
const SELECT_FILE_ACTION: &str = "select-file";
const OPEN_FILE_ACTION: &str = "open-file";
const CLOSE_DIFF_ACTION: &str = "close-diff";
const REFRESH_ACTION: &str = "refresh-diffs";
const SELECT_DIFF_LINES_ACTION: &str = "select-diff-lines";
const OPEN_IN_EDITOR_ACTION: &str = "open-in-editor";
const SEND_TO_AGENT_ACTION: &str = "send-to-agent";
const COPY_ACTION: &str = "copy";

/// Safety cadence for asking the Host who is beside this App. The real
/// triggers are file stamps (`AgentBridge::context_changed`: the
/// Controller's pane layout and the followed neighbor's manifest), so this
/// only has to catch what those miss, and can be slow.
const AGENT_CONTEXT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

pub fn run(
    mut app: App,
    follow_agent_context: bool,
    mut app_context: AppContext,
) -> io::Result<()> {
    let mut theme_monitor = ThemeMonitor::detected();
    let mut theme = theme_monitor.theme();
    let mut terminal = TerminalGuard::enter()?;
    let mut drags = DragSurface::detect();
    let _keyboard = KeyboardEnhancementGuard::enter()?;
    let mut reporter = AppReporter::detect(crate::install::APP_ID);
    let mut bridge = UiBridge::detect(
        AppMetadata::new(
            crate::install::APP_ID,
            "Unpeel Diffs",
            env!("CARGO_PKG_VERSION"),
        )
        .description("One Diffs component tree interpreted by Ratatui, native, and web renderers"),
    )
    .map_err(ui_bridge_error)?;
    let agent = AgentBridge::new();
    agent.refresh();
    let mut ui_revision = 1u64;
    let mut published = semantic_node(&app, agent.label().is_some());
    bridge
        .publish(UI_VIEW_ID, ui_revision, published.clone())
        .map_err(ui_bridge_error)?;
    let mut last_agent_context_refresh = Instant::now();
    // The session title names the working tree and its branch (the Host
    // folds it into the sidebar row until the user renames it). Recomputed
    // when the tree changes or the periodic sync runs, written on change.
    let mut session_title = String::new();
    let mut rendered = RenderResult::default();
    let mut menu: Option<ContextMenu> = None;
    let mut selecting = false;
    let mut needs_draw = true;
    let mut last_sync = Instant::now();

    loop {
        if drain_bridge(
            &mut app,
            &agent,
            &mut bridge,
            &mut ui_revision,
            &mut published,
        )? {
            needs_draw = true;
        }
        publish_semantic_projection(
            &app,
            agent.label().is_some(),
            &mut bridge,
            &mut ui_revision,
            &mut published,
        )?;
        if needs_draw {
            let title = session_title_for(&app.repository);
            if title != session_title {
                reporter.set_title(&title);
                session_title = title;
            }
            reporter.set_context(&serde_json::json!({
                "root": app.root(),
                "view": if app.is_detail() { "diff" } else { "files" },
                "changed_files": app.files.len(),
                "selected_path": app.selected_absolute_path(),
                "selected_status": app.selected_file().map(|file| file.state_label()),
                "selected_diff_lines": app
                    .selection_range()
                    .map(|(start, end)| [start + 1, end + 1]),
            }));
            if bridge.should_render_terminal() {
                rendered = terminal.draw(&published, &app, &mut drags, menu.as_mut(), theme)?;
                app.apply_render_metrics(
                    rendered.scroll_offset,
                    rendered.max_scroll,
                    rendered.viewport_rows,
                    rendered.max_horizontal_scroll,
                );
            }
            needs_draw = false;
        }

        if !event::poll(Duration::from_millis(250))? {
            drags.heartbeat()?;
            if theme_monitor.refresh() {
                theme = theme_monitor.theme();
                needs_draw = true;
            }
            if follow_agent_context
                && (agent.context_changed()
                    || last_agent_context_refresh.elapsed() >= AGENT_CONTEXT_REFRESH_INTERVAL)
            {
                last_agent_context_refresh = Instant::now();
                let app_context_changed = app_context.refresh();
                let next_root = agent
                    .project_context()
                    .filter(|context| context.cwd.is_dir())
                    .map(|context| context.cwd)
                    .or_else(|| {
                        app_context_changed
                            .then(|| app_context.current_root().map(PathBuf::from))
                            .flatten()
                    });
                if let Some(root) = next_root.filter(|root| root.is_dir()) {
                    match app.follow_path(&root) {
                        Ok(true) => needs_draw = true,
                        Ok(false) => {}
                        Err(error) => app.fail(error),
                    }
                }
                agent.refresh();
            }
            // Quietly follow the working tree while the user is not
            // mid-interaction; transient Git errors are retried next tick.
            if menu.is_none() && !selecting && last_sync.elapsed() >= AUTO_SYNC_INTERVAL {
                last_sync = Instant::now();
                if app.sync().unwrap_or(false) {
                    needs_draw = true;
                }
            }
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if is_force_quit(key) {
                    break;
                }
                if menu.is_some() {
                    match key.code {
                        KeyCode::Esc => {
                            menu = None;
                            needs_draw = true;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            menu.as_mut().expect("menu is open").move_selection(-1);
                            needs_draw = true;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            menu.as_mut().expect("menu is open").move_selection(1);
                            needs_draw = true;
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            let open_menu = menu.take().expect("menu is open");
                            activate_menu(open_menu, &mut app, &agent);
                            needs_draw = true;
                        }
                        _ => {}
                    }
                    continue;
                }
                if let Some(action) = published.footer_action_for_key(&key).cloned() {
                    apply_footer_action(&mut app, &agent, &action);
                    needs_draw = true;
                    continue;
                }
                let Some(action) =
                    action_for_key(key, app.is_detail(), app.selection_range().is_some())
                else {
                    continue;
                };
                if action == InputAction::SendToAgent {
                    send_selection(&mut app, &agent);
                    needs_draw = true;
                    continue;
                }
                if handle_action(&mut app, action) {
                    break;
                }
                needs_draw = true;
            }
            Event::Mouse(mouse) => {
                needs_draw |= app.pointer.track(&mouse);
                let position = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(mut open_menu) = menu.take() {
                            if open_menu.action_index_for_mouse(&mouse).is_some() {
                                activate_menu(open_menu, &mut app, &agent);
                            }
                            needs_draw = true;
                        } else if let Some(action) = rendered
                            .footer_area
                            .and_then(|area| published.footer()?.action_at(position, area))
                            .cloned()
                        {
                            selecting = false;
                            apply_footer_action(&mut app, &agent, &action);
                            needs_draw = true;
                        } else if app.is_detail() {
                            if rendered
                                .back_button
                                .is_some_and(|hit| hit.contains(position))
                            {
                                app.back();
                            } else if let Some(index) = diff_line_at(&rendered, position) {
                                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                                    app.extend_selection(index);
                                } else {
                                    app.begin_selection(index);
                                }
                                selecting = true;
                            } else {
                                app.clear_selection();
                            }
                            needs_draw = true;
                        } else if let Some(hit) = rendered
                            .hits
                            .iter()
                            .copied()
                            .find(|hit| hit.contains(position))
                        {
                            let action = match &published.element {
                                UiComponent::Page(page) => page
                                    .list()
                                    .items
                                    .get(hit.index)
                                    .and_then(unpeel_app_kit::ListItem::primary_ui_action),
                                _ => None,
                            };
                            if let Some(action) = action {
                                if let Err(message) =
                                    apply_semantic_action(&mut app, &agent, &action)
                                {
                                    app.fail(io::Error::other(message));
                                }
                            } else {
                                app.select(hit.index);
                            }
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Right) => {
                        selecting = false;
                        if app.is_detail() {
                            if let Some(index) = diff_line_at(&rendered, position) {
                                let covered = app
                                    .selection_range()
                                    .is_some_and(|(start, end)| index >= start && index <= end);
                                if !covered {
                                    app.begin_selection(index);
                                }
                                agent.refresh();
                                let absolute = app.selected_absolute_path();
                                menu = published_context_menu(&published)
                                    .map(|spec| diff_menu(absolute, spec, position, theme.scheme));
                                needs_draw = true;
                            } else if menu.take().is_some() {
                                needs_draw = true;
                            }
                        } else if let Some(hit) = rendered
                            .hits
                            .iter()
                            .copied()
                            .find(|hit| hit.contains(position))
                        {
                            app.select(hit.index);
                            if let (Some(file), Some(absolute)) =
                                (app.selected_file(), app.selected_absolute_path())
                            {
                                let relative = control_safe(file.path().to_string_lossy().as_ref());
                                agent.refresh();
                                menu = published_context_menu(&published).map(|spec| {
                                    list_menu(relative, absolute, spec, position, theme.scheme)
                                });
                            }
                            needs_draw = true;
                        } else if menu.take().is_some() {
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if selecting && let Some(index) = diff_line_near(&rendered, position) {
                            app.extend_selection(index);
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        selecting = false;
                    }
                    MouseEventKind::ScrollUp => {
                        if let Some(open_menu) = menu.as_mut() {
                            open_menu.move_selection(-1);
                        } else {
                            app.scroll_vertical(-3);
                        }
                        needs_draw = true;
                    }
                    MouseEventKind::ScrollDown => {
                        if let Some(open_menu) = menu.as_mut() {
                            open_menu.move_selection(1);
                        } else {
                            app.scroll_vertical(3);
                        }
                        needs_draw = true;
                    }
                    MouseEventKind::Moved => {
                        if menu
                            .as_mut()
                            .is_some_and(|open_menu| open_menu.hover_at(position))
                        {
                            needs_draw = true;
                        }
                    }
                    _ => {}
                }
            }
            Event::Resize(_, _) => {
                app.reveal_selected = true;
                needs_draw = true;
            }
            _ => {}
        }
    }
    Ok(())
}

fn semantic_node(app: &App, can_send: bool) -> UiNode {
    UiNode::page(SEMANTIC_ROOT_ID, semantic_page(app, can_send))
}

fn semantic_page(app: &App, can_send: bool) -> Page {
    match &app.screen {
        Screen::Files => {
            let mut list = List::new(
                FILE_LIST_ID,
                app.files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| file_list_item(file, index))
                    .collect(),
            )
            .empty_message("working tree clean");
            if !app.files.is_empty() {
                list = list.selected(
                    file_node_id(app.selected.min(app.files.len() - 1)),
                    SELECT_FILE_ACTION,
                );
            }
            list = list.context_menu(semantic_file_menu(can_send));
            Page::new(semantic_page_title(app, "Changes"), list).footer_actions([
                FooterAction::new("refresh-diffs", "refresh", REFRESH_ACTION).accelerator("r"),
            ])
        }
        Screen::Diff(document) => {
            let lines = document
                .lines
                .iter()
                .enumerate()
                .map(|(index, line)| semantic_diff_line(index, line))
                .collect::<Vec<_>>();
            let mut content = Content::new(
                "diff-content",
                format!("Patch for {}", document.file.list_name()),
                lines,
            )
            .wrap(false)
            .font(ContentFont::Monospace)
            .empty_message("No textual diff")
            .select_action(SELECT_DIFF_LINES_ACTION)
            .context_menu(semantic_diff_menu(
                can_send,
                app.selected_absolute_path().is_some(),
            ));
            if let Some((anchor, head)) = app.selection {
                content.selection = Some(ContentSelection::new(
                    diff_line_id(anchor),
                    diff_line_id(head),
                ));
            }
            let title = format!(
                "{} · +{} −{}",
                document.file.list_name(),
                document.additions,
                document.deletions
            );
            Page::with_content(semantic_page_title(app, &title), content)
                .back_action(CLOSE_DIFF_ACTION)
                .footer_actions([
                    FooterAction::new("refresh-diffs", "refresh", REFRESH_ACTION).accelerator("r"),
                ])
        }
    }
}

fn semantic_page_title(app: &App, base: &str) -> String {
    app.notice.as_ref().map_or_else(
        || base.to_owned(),
        |notice| {
            format!(
                "{base} · {}{}",
                if notice.error { "Error: " } else { "" },
                notice.text
            )
        },
    )
}

fn semantic_diff_text(line: &str) -> String {
    let expanded = expand_tabs(line);
    let mut label = String::with_capacity(expanded.len());
    for character in expanded.chars().filter(|character| !character.is_control()) {
        label.push(character);
    }
    label
}

fn semantic_diff_line(index: usize, line: &str) -> ContentLine {
    let text = semantic_diff_text(line);
    let bytes = line.as_bytes();
    let (line_tone, run_tone, emphasis) = match bytes.first() {
        Some(b'+') if !line.starts_with("+++") => (
            ContentLineTone::Added,
            ContentTone::Success,
            ContentEmphasis::Regular,
        ),
        Some(b'-') if !line.starts_with("---") => (
            ContentLineTone::Removed,
            ContentTone::Danger,
            ContentEmphasis::Regular,
        ),
        Some(b'@') => (
            ContentLineTone::Header,
            ContentTone::Info,
            ContentEmphasis::Strong,
        ),
        _ if line.starts_with("diff ") || line.starts_with("index ") => (
            ContentLineTone::Muted,
            ContentTone::Muted,
            ContentEmphasis::Regular,
        ),
        _ => (
            ContentLineTone::Default,
            ContentTone::Default,
            ContentEmphasis::Regular,
        ),
    };
    ContentLine::styled(
        diff_line_id(index),
        vec![ContentRun::new(text).tone(run_tone).emphasis(emphasis)],
    )
    .tone(line_tone)
}

fn diff_line_id(index: usize) -> String {
    format!("diff-line-{index}")
}

fn diff_line_index(node_id: &str) -> Option<usize> {
    node_id.strip_prefix("diff-line-")?.parse().ok()
}

fn semantic_file_menu(can_send: bool) -> SemanticMenu {
    let mut items = vec![SemanticMenuItem::new(
        "open-in-editor",
        "Open in editor",
        OPEN_IN_EDITOR_ACTION,
    )];
    if can_send {
        items.push(SemanticMenuItem::new(
            "send-path",
            "Send to agent",
            SEND_TO_AGENT_ACTION,
        ));
    }
    items.push(SemanticMenuItem::new("copy-path", "Copy path", COPY_ACTION));
    SemanticMenu::new("File actions", items)
        .presentation(SemanticMenuPresentation::Context)
        .anchor(SemanticMenuAnchor::Pointer)
}

fn semantic_diff_menu(can_send: bool, can_open: bool) -> SemanticMenu {
    let mut open = SemanticMenuItem::new("open-in-editor", "Open in editor", OPEN_IN_EDITOR_ACTION);
    if !can_open {
        open = open.disabled(true);
    }
    let mut items = vec![open];
    if can_send {
        items.push(SemanticMenuItem::new(
            "send-lines",
            "Send to agent",
            SEND_TO_AGENT_ACTION,
        ));
    }
    items.extend([
        SemanticMenuItem::new("copy-lines", "Copy lines", COPY_ACTION),
        SemanticMenuItem::new("refresh-diff", "Refresh", REFRESH_ACTION),
    ]);
    SemanticMenu::new("Diff actions", items)
        .presentation(SemanticMenuPresentation::Context)
        .anchor(SemanticMenuAnchor::Pointer)
}

fn published_context_menu(node: &UiNode) -> Option<&SemanticMenu> {
    let UiComponent::Page(page) = &node.element else {
        return None;
    };
    match &page.body {
        unpeel_app_kit::PageBodySlot::List(list) => list.context_menu.as_ref(),
        unpeel_app_kit::PageBodySlot::Content(content) => content.context_menu.as_ref(),
        _ => None,
    }
}

fn publish_semantic_projection(
    app: &App,
    can_send: bool,
    bridge: &mut UiBridge,
    revision: &mut u64,
    published: &mut UiNode,
) -> io::Result<()> {
    let next = semantic_node(app, can_send);
    if next == *published {
        return Ok(());
    }
    let next_revision = revision
        .checked_add(1)
        .ok_or_else(|| io::Error::other("Diffs UI revision space is exhausted"))?;
    let operations = page_delta_operations(published, &next);
    bridge
        .publish_delta(UI_VIEW_ID, *revision, next_revision, operations)
        .map_err(ui_bridge_error)?;
    *revision = next_revision;
    *published = next;
    Ok(())
}

fn drain_bridge(
    app: &mut App,
    agent: &AgentBridge,
    bridge: &mut UiBridge,
    revision: &mut u64,
    published: &mut UiNode,
) -> io::Result<bool> {
    let mut changed = false;
    while let Some(message) = bridge.poll().map_err(ui_bridge_error)? {
        match message {
            UiBridgeEvent::Action { event, .. } => {
                let result = if event.base_revision != *revision {
                    Err(format!(
                        "Diffs changed from revision {} to {}; retry the action",
                        event.base_revision, revision
                    ))
                } else {
                    apply_semantic_action(app, agent, &event.action)
                };
                let outcome = match result {
                    Ok(()) => {
                        changed = true;
                        publish_semantic_projection(
                            app,
                            agent.label().is_some(),
                            bridge,
                            revision,
                            published,
                        )?;
                        UiEventOutcome::Applied
                    }
                    Err(message) => UiEventOutcome::Rejected(message),
                };
                bridge
                    .acknowledge(&event, outcome, *revision)
                    .map_err(ui_bridge_error)?;
            }
            UiBridgeEvent::Attached { .. }
            | UiBridgeEvent::Detached { .. }
            | UiBridgeEvent::Lifecycle { .. } => changed = true,
        }
    }
    Ok(changed)
}

fn apply_semantic_action(
    app: &mut App,
    agent: &AgentBridge,
    action: &unpeel_app_kit::UiAction,
) -> Result<(), String> {
    match (
        action.node_id.as_str(),
        action.action.as_str(),
        action.kind,
        &action.value,
    ) {
        (FILE_LIST_ID, SELECT_FILE_ACTION, UiEventKind::Change, UiEventValue::Text(item_id))
            if !app.is_detail() =>
        {
            let index = file_index_from_node_id(item_id)
                .ok_or_else(|| "Selected file has an invalid target".to_owned())?;
            if index >= app.files.len() {
                return Err("Selected file no longer exists".to_owned());
            }
            app.select(index);
            Ok(())
        }
        (node_id, OPEN_FILE_ACTION, UiEventKind::Activate, UiEventValue::None)
            if !app.is_detail() =>
        {
            let index = file_index_from_node_id(node_id)
                .ok_or_else(|| "File action has an invalid target".to_owned())?;
            if index >= app.files.len() {
                return Err("File no longer exists".to_owned());
            }
            app.select(index);
            app.open_selected().map_err(|error| error.to_string())
        }
        (SEMANTIC_ROOT_ID, CLOSE_DIFF_ACTION, UiEventKind::Cancel, UiEventValue::None)
            if app.is_detail() =>
        {
            app.back();
            Ok(())
        }
        ("refresh-diffs", REFRESH_ACTION, UiEventKind::Activate, UiEventValue::None)
        | ("refresh-diff", REFRESH_ACTION, UiEventKind::Activate, UiEventValue::Text(_)) => {
            app.refresh().map_err(|error| error.to_string())
        }
        (
            "diff-content",
            SELECT_DIFF_LINES_ACTION,
            UiEventKind::Select,
            UiEventValue::TextList(ids),
        ) if app.is_detail() && ids.len() == 2 => {
            let anchor = diff_line_index(&ids[0])
                .ok_or_else(|| "Diff selection anchor is invalid".to_owned())?;
            let head = diff_line_index(&ids[1])
                .ok_or_else(|| "Diff selection head is invalid".to_owned())?;
            app.begin_selection(anchor);
            app.extend_selection(head);
            Ok(())
        }
        (_, OPEN_IN_EDITOR_ACTION, UiEventKind::Activate, UiEventValue::Text(target)) => {
            select_semantic_target(app, target)?;
            let path = app
                .selected_absolute_path()
                .ok_or_else(|| "File no longer exists".to_owned())?;
            EditorBridge::open(&path).map_err(|error| error.to_string())?;
            app.notify("Opened in editor");
            Ok(())
        }
        (_, SEND_TO_AGENT_ACTION, UiEventKind::Activate, UiEventValue::Text(target)) => {
            select_semantic_target(app, target)?;
            if app.is_detail() {
                ensure_semantic_diff_selection(app, target)?;
                send_selection(app, agent);
            } else {
                let file = app
                    .selected_file()
                    .ok_or_else(|| "File no longer exists".to_owned())?;
                let path = control_safe(file.path().to_string_lossy().as_ref());
                match agent.send_text(&path) {
                    Ok(label) => app.notify(format!("Sent path to {label}")),
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(())
        }
        (_, COPY_ACTION, UiEventKind::Activate, UiEventValue::Text(target)) => {
            select_semantic_target(app, target)?;
            let text = if app.is_detail() {
                ensure_semantic_diff_selection(app, target)?;
                app.selected_diff_lines()
                    .map(|lines| lines.join("\n"))
                    .ok_or_else(|| "No diff lines selected".to_owned())?
            } else {
                app.selected_absolute_path()
                    .ok_or_else(|| "File no longer exists".to_owned())?
                    .to_string_lossy()
                    .into_owned()
            };
            copy_text(&text).map_err(|error| error.to_string())?;
            app.notify(if app.is_detail() {
                "Diff lines copied"
            } else {
                "Path copied"
            });
            Ok(())
        }
        _ => Err("Action is not declared by the current Diffs Page".to_owned()),
    }
}

fn apply_footer_action(app: &mut App, agent: &AgentBridge, action: &FooterAction) {
    let event = UiAction::new(
        action.id.clone(),
        action.action.clone(),
        UiEventKind::Activate,
        UiEventValue::None,
    );
    if let Err(error) = apply_semantic_action(app, agent, &event) {
        app.fail(error);
    }
}

fn select_semantic_target(app: &mut App, target: &str) -> Result<(), String> {
    if let Some(index) = file_index_from_node_id(target) {
        if index >= app.files.len() {
            return Err("File no longer exists".to_owned());
        }
        app.select(index);
    }
    Ok(())
}

fn ensure_semantic_diff_selection(app: &mut App, target: &str) -> Result<(), String> {
    let Some(index) = diff_line_index(target) else {
        return Ok(());
    };
    let covered = app
        .selection_range()
        .is_some_and(|(start, end)| index >= start && index <= end);
    if !covered {
        app.begin_selection(index);
    }
    Ok(())
}

fn file_index_from_node_id(node_id: &str) -> Option<usize> {
    node_id.strip_prefix("file-")?.parse().ok()
}

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error.to_string())
}

fn diff_line_at(rendered: &RenderResult, position: Position) -> Option<usize> {
    rendered
        .diff_hits
        .iter()
        .find(|hit| hit.contains(position))
        .map(|hit| hit.index)
}

/// Row lookup for drag extension: clamps to the first or last visible diff
/// row so dragging past the surface edges keeps growing the selection.
fn diff_line_near(rendered: &RenderResult, position: Position) -> Option<usize> {
    let first = rendered.diff_hits.first()?;
    let last = rendered.diff_hits.last()?;
    if position.y <= first.area.y {
        return Some(first.index);
    }
    if position.y >= last.area.y {
        return Some(last.index);
    }
    rendered
        .diff_hits
        .iter()
        .find(|hit| hit.area.y == position.y)
        .map(|hit| hit.index)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputAction {
    Quit,
    Activate,
    Back,
    Down,
    Up,
    First,
    Last,
    PageDown,
    PageUp,
    PanLeft,
    PanRight,
    SendToAgent,
    ClearSelection,
}

fn is_force_quit(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn action_for_key(key: KeyEvent, detail: bool, has_selection: bool) -> Option<InputAction> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    if is_force_quit(key) {
        return Some(InputAction::Quit);
    }
    if key.code == KeyCode::Esc && !detail {
        return None;
    }
    let contextual = match key.code {
        KeyCode::Char('q') if !control => Some(InputAction::Quit),
        KeyCode::Enter if detail && has_selection => Some(InputAction::SendToAgent),
        KeyCode::Char('s') if detail && has_selection => Some(InputAction::SendToAgent),
        KeyCode::Enter if detail => Some(InputAction::Back),
        KeyCode::Enter => Some(InputAction::Activate),
        KeyCode::Esc if detail && has_selection => Some(InputAction::ClearSelection),
        KeyCode::Esc if detail => Some(InputAction::Back),
        KeyCode::Left | KeyCode::Char('h') if detail => Some(InputAction::PanLeft),
        KeyCode::Right | KeyCode::Char('l') if detail => Some(InputAction::PanRight),
        _ => None,
    };
    contextual.or_else(|| {
        ListKeymap::new()
            .action_for_key(&key)
            .map(list_navigation_input_action)
    })
}

const fn list_navigation_input_action(action: ListNavigationAction) -> InputAction {
    match action {
        ListNavigationAction::Down => InputAction::Down,
        ListNavigationAction::Up => InputAction::Up,
        ListNavigationAction::First => InputAction::First,
        ListNavigationAction::Last => InputAction::Last,
        ListNavigationAction::PageDown => InputAction::PageDown,
        ListNavigationAction::PageUp => InputAction::PageUp,
        ListNavigationAction::Activate => InputAction::Activate,
        ListNavigationAction::Back => InputAction::Back,
    }
}

fn handle_action(app: &mut App, action: InputAction) -> bool {
    match action {
        InputAction::Quit => return true,
        InputAction::Activate => {
            if let Err(error) = app.open_selected() {
                app.fail(error);
            }
        }
        InputAction::Back => app.back(),
        InputAction::Down if app.is_detail() => app.scroll_vertical(1),
        InputAction::Down => app.move_selection(1),
        InputAction::Up if app.is_detail() => app.scroll_vertical(-1),
        InputAction::Up => app.move_selection(-1),
        InputAction::First => app.scroll_to_start(),
        InputAction::Last => app.scroll_to_end(),
        InputAction::PageDown if app.is_detail() => {
            app.scroll_vertical(app.viewport_rows.max(1) as isize);
        }
        InputAction::PageDown => app.page_selection(1),
        InputAction::PageUp if app.is_detail() => {
            app.scroll_vertical(-(app.viewport_rows.max(1) as isize));
        }
        InputAction::PageUp => app.page_selection(-1),
        InputAction::PanLeft => app.scroll_horizontal(-4),
        InputAction::PanRight => app.scroll_horizontal(4),
        InputAction::SendToAgent => {}
        InputAction::ClearSelection => {
            app.clear_selection();
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ContextAction {
    OpenInEditor(PathBuf),
    SendSelection,
    CopySelection,
    /// Bare repo-relative path pasted into the agent input.
    SendPath(String),
    CopyPath(PathBuf),
    Refresh,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ContextTarget {
    Diff { absolute: Option<PathBuf> },
    File { relative: String, absolute: PathBuf },
}

#[derive(Debug)]
struct ContextMenu {
    popup: PopupMenu<String>,
    target: ContextTarget,
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

fn diff_menu(
    absolute: Option<PathBuf>,
    spec: &SemanticMenu,
    anchor: Position,
    scheme: ColorScheme,
) -> ContextMenu {
    ContextMenu {
        popup: spec.popup(anchor, MenuTheme::for_color_scheme(scheme)),
        target: ContextTarget::Diff { absolute },
    }
}

fn list_menu(
    relative: String,
    absolute: PathBuf,
    spec: &SemanticMenu,
    anchor: Position,
    scheme: ColorScheme,
) -> ContextMenu {
    ContextMenu {
        popup: spec.popup(anchor, MenuTheme::for_color_scheme(scheme)),
        target: ContextTarget::File { relative, absolute },
    }
}

fn activate_menu(menu: ContextMenu, app: &mut App, agent: &AgentBridge) {
    let ContextMenu { popup, target } = menu;
    let Some(item_id) = popup.selected_value().map(String::as_str) else {
        return;
    };
    let action = match (item_id, target) {
        (
            "open-in-editor",
            ContextTarget::Diff {
                absolute: Some(path),
            },
        )
        | ("open-in-editor", ContextTarget::File { absolute: path, .. }) => {
            ContextAction::OpenInEditor(path)
        }
        ("send-lines", ContextTarget::Diff { .. }) => ContextAction::SendSelection,
        ("copy-lines", ContextTarget::Diff { .. }) => ContextAction::CopySelection,
        ("refresh-diff", ContextTarget::Diff { .. }) => ContextAction::Refresh,
        ("send-path", ContextTarget::File { relative, .. }) => ContextAction::SendPath(relative),
        ("copy-path", ContextTarget::File { absolute, .. }) => ContextAction::CopyPath(absolute),
        _ => {
            app.fail("Menu action is unavailable");
            return;
        }
    };
    match action {
        ContextAction::OpenInEditor(path) => match EditorBridge::open(&path) {
            Ok(()) => app.notify("Opened in editor"),
            Err(error) => app.fail(format!("Open failed: {error}")),
        },
        ContextAction::SendSelection => send_selection(app, agent),
        ContextAction::CopySelection => {
            match app.selected_diff_lines().map(|lines| lines.join("\n")) {
                Some(text) => match copy_text(&text) {
                    Ok(()) => app.notify("Diff lines copied"),
                    Err(error) => app.fail(format!("Copy failed: {error}")),
                },
                None => app.fail("No diff lines selected"),
            }
        }
        ContextAction::SendPath(path) => match agent.send_text(&path) {
            Ok(label) => app.notify(format!("Sent path to {label}")),
            Err(error) => match copy_text(&path) {
                Ok(()) => app.fail(format!("{error}; path copied instead")),
                Err(copy_error) => app.fail(format!("{error}; copy failed: {copy_error}")),
            },
        },
        ContextAction::CopyPath(path) => match copy_text(path.to_string_lossy().as_ref()) {
            Ok(()) => app.notify("Path copied"),
            Err(error) => app.fail(format!("Copy failed: {error}")),
        },
        ContextAction::Refresh => {
            if let Err(error) = app.refresh() {
                app.fail(error);
            }
        }
    }
}

/// Paste a compact file-and-line reference for the selection into the
/// nearby agent's input, so the user writes their comment in the agent chat.
fn send_selection(app: &mut App, agent: &AgentBridge) {
    let reference = match (&app.screen, app.selection_range()) {
        (Screen::Diff(document), Some(range)) => Some(selection_reference(document, range)),
        _ => None,
    };
    let Some(reference) = reference else {
        app.fail("No diff lines selected");
        return;
    };
    match agent.send_text(&reference) {
        Ok(label) => {
            app.clear_selection();
            app.notify(format!("Sent to {label}"));
        }
        Err(error) => match copy_text(&reference) {
            Ok(()) => app.fail(format!("{error}; reference copied instead")),
            Err(copy_error) => app.fail(format!("{error}; copy failed: {copy_error}")),
        },
    }
}

/// A bare repo-relative `path:line` (or `path:start-end`) token, control-safe
/// so an odd filename byte cannot become terminal input in the agent's pane.
fn selection_reference(document: &DiffDocument, range: (usize, usize)) -> String {
    let path = control_safe(document.file.path().to_string_lossy().as_ref());
    match new_file_line_span(&document.lines, range) {
        Some((first, last)) if first == last => format!("{path}:{first}"),
        Some((first, last)) => format!("{path}:{first}-{last}"),
        None => path,
    }
}

fn control_safe(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// Maps selected diff rows to the file line numbers they touch, using the
/// `+c,d` side of hunk headers. Removed lines anchor at the position the
/// deletion leaves behind. Selections outside any hunk return `None`.
fn new_file_line_span(lines: &[String], (start, end): (usize, usize)) -> Option<(usize, usize)> {
    let mut new_line: Option<usize> = None;
    let mut span: Option<(usize, usize)> = None;
    for (index, line) in lines.iter().enumerate().take(end.saturating_add(1)) {
        if line.starts_with("@@") {
            new_line = hunk_new_start(line);
            continue;
        }
        let Some(current) = new_line else {
            continue;
        };
        if line.starts_with('\\') {
            continue;
        }
        let advances = !line.starts_with('-');
        if index >= start {
            let anchor = current.max(1);
            span = Some(span.map_or((anchor, anchor), |(first, last)| {
                (first.min(anchor), last.max(anchor))
            }));
        }
        if advances {
            new_line = Some(current + 1);
        }
    }
    span
}

fn hunk_new_start(header: &str) -> Option<usize> {
    let plus = header
        .split(' ')
        .find(|part| part.starts_with('+') && part.len() > 1)?;
    plus[1..].split(',').next()?.parse().ok()
}

fn copy_text(text: &str) -> io::Result<()> {
    let sequence = clipboard_sequence(text);
    let mut stdout = io::stdout();
    stdout.write_all(sequence.as_bytes())?;
    stdout.flush()
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
                let _ = terminal::disable_raw_mode();
                return Err(error);
            }
        };
        if let Err(error) = terminal.hide_cursor() {
            let _ = execute!(
                terminal.backend_mut(),
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self { terminal })
    }

    fn draw(
        &mut self,
        node: &UiNode,
        app: &App,
        drags: &mut DragSurface,
        menu: Option<&mut ContextMenu>,
        theme: KitTheme,
    ) -> io::Result<RenderResult> {
        let mut result = RenderResult::default();
        drags.begin_frame();
        self.terminal.draw(|frame| {
            result = render_component_frame(frame, node, app, drags, menu, theme);
        })?;
        drags.commit()?;
        Ok(result)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RectHit {
    area: Rect,
}

impl RectHit {
    fn from_rect(area: Rect) -> Option<Self> {
        (!area.is_empty()).then_some(Self { area })
    }

    fn contains(self, position: Position) -> bool {
        self.area.contains(position)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowHit {
    index: usize,
    area: Rect,
}

impl RowHit {
    fn contains(self, position: Position) -> bool {
        self.area.contains(position)
    }
}

#[derive(Debug, Default)]
struct RenderResult {
    hits: Vec<RowHit>,
    diff_hits: Vec<RowHit>,
    back_button: Option<RectHit>,
    footer_area: Option<Rect>,
    scroll_offset: usize,
    max_scroll: usize,
    viewport_rows: usize,
    max_horizontal_scroll: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct FileListView<'a> {
    root: &'a Path,
    files: &'a [ChangedFile],
    selected: usize,
    requested_scroll: usize,
    reveal_selected: bool,
}

/// Terminal interpretation of the exact Page node published to native and
/// web renderers. App-specific code may register non-visual drag hit regions,
/// but it must not paint a second representation of the screen.
fn render_component_frame(
    frame: &mut Frame<'_>,
    node: &UiNode,
    app: &App,
    drags: &mut DragSurface,
    menu: Option<&mut ContextMenu>,
    theme: KitTheme,
) -> RenderResult {
    let UiComponent::Page(page) = &node.element else {
        return RenderResult::default();
    };
    let layout = page.layout(frame.area());
    let mut input = InputField::new("");
    let mut list_state = match &page.body {
        unpeel_app_kit::PageBodySlot::List(list) => {
            let selected = list
                .selected_id
                .as_deref()
                .and_then(|id| list.items.iter().position(|item| item.id == id));
            let mut state = ListState::new(selected);
            state.set_offset(app.list_scroll, list.items.len());
            if app.reveal_selected {
                state.request_reveal();
            }
            state
        }
        _ => ListState::default(),
    };
    let mut content_state = ContentState::new();
    list_state.set_pointer(app.pointer);
    content_state.set_offsets(
        u16::try_from(app.detail_scroll).unwrap_or(u16::MAX),
        u16::try_from(app.horizontal_scroll).unwrap_or(u16::MAX),
    );
    frame.render_widget(
        page.widget_with_content_state(&mut input, &mut list_state, &mut content_state)
            .theme(file_list_theme(theme))
            .content_theme(ContentTheme::for_theme(theme)),
        frame.area(),
    );

    let mut result = match (&app.screen, &page.body) {
        (Screen::Files, unpeel_app_kit::PageBodySlot::List(list)) => {
            let rows_area = list_state.rows_area();
            let hits = (0..usize::from(rows_area.height))
                .filter_map(|row| {
                    let index = list_state.offset().saturating_add(row);
                    let file = app.files.get(index)?;
                    let row_area = Rect::new(
                        rows_area.x,
                        rows_area
                            .y
                            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                        rows_area.width,
                        1,
                    );
                    drags.register(row_area, app.root().join(file.path()));
                    Some(RowHit {
                        index,
                        area: row_area,
                    })
                })
                .collect();
            RenderResult {
                hits,
                scroll_offset: list_state.offset(),
                max_scroll: list_state.max_offset(list.items.len()),
                viewport_rows: list_state.viewport_rows(),
                ..RenderResult::default()
            }
        }
        (Screen::Diff(_), unpeel_app_kit::PageBodySlot::Content(content)) => {
            let scroll_offset = usize::from(content_state.vertical_offset());
            let viewport_rows = usize::from(content_state.viewport_rows());
            let overflow = content.lines.len() > viewport_rows && layout.list.width > 1;
            let rows_area = Rect::new(
                layout.list.x,
                layout.list.y,
                layout.list.width.saturating_sub(u16::from(overflow)),
                layout.list.height,
            );
            let diff_hits = (0..usize::from(rows_area.height))
                .filter_map(|row| {
                    let index = scroll_offset.saturating_add(row);
                    content.lines.get(index)?;
                    Some(RowHit {
                        index,
                        area: Rect::new(
                            rows_area.x,
                            rows_area
                                .y
                                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                            rows_area.width,
                            1,
                        ),
                    })
                })
                .collect();
            let longest_line = content
                .lines
                .iter()
                .map(|line| UnicodeWidthStr::width(line.text().as_str()))
                .max()
                .unwrap_or(0);
            RenderResult {
                diff_hits,
                back_button: page
                    .back
                    .as_ref()
                    .and_then(|_| RectHit::from_rect(layout.title)),
                scroll_offset,
                max_scroll: usize::from(content_state.max_vertical_offset(content.lines.len())),
                viewport_rows,
                max_horizontal_scroll: longest_line.saturating_sub(usize::from(rows_area.width)),
                ..RenderResult::default()
            }
        }
        _ => RenderResult::default(),
    };
    if let Some(menu) = menu {
        drags.begin_frame();
        menu.render(frame);
    }
    // The component widget owns all pixels; the result only carries terminal
    // geometry back to the App's renderer-local interaction state.
    result.footer_area = layout.footer;
    result
}

#[cfg(test)]
fn render_file_list(
    frame: &mut Frame<'_>,
    area: Rect,
    view: FileListView<'_>,
    theme: KitTheme,
    drags: &mut DragSurface,
) -> RenderResult {
    if area.is_empty() {
        return RenderResult::default();
    }
    if view.files.is_empty() {
        let row = Rect::new(
            area.x,
            area.y.saturating_add(area.height.saturating_sub(1) / 2),
            area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new("working tree clean")
                .style(Style::new().fg(theme.muted))
                .alignment(Alignment::Center),
            row,
        );
        return RenderResult {
            viewport_rows: usize::from(area.height),
            ..RenderResult::default()
        };
    }

    let total_rows = view.files.len();
    let selected = view.selected.min(total_rows - 1);
    let list = List::new(
        FILE_LIST_ID,
        view.files
            .iter()
            .enumerate()
            .map(|(index, file)| file_list_item(file, index))
            .collect(),
    )
    .selected(file_node_id(selected), SELECT_FILE_ACTION);
    let mut state = ListState::new(Some(selected));
    state.set_offset(view.requested_scroll, total_rows);
    if view.reveal_selected {
        state.request_reveal();
    }
    frame.render_widget(list.widget(&mut state).theme(file_list_theme(theme)), area);
    let rows_area = state.rows_area();
    let hits = (0..usize::from(rows_area.height))
        .filter_map(|row| {
            let index = state.offset().saturating_add(row);
            let file = view.files.get(index)?;
            let row_area = Rect::new(
                rows_area.x,
                rows_area
                    .y
                    .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                rows_area.width,
                1,
            );
            drags.register(row_area, view.root.join(file.path()));
            Some(RowHit {
                index,
                area: row_area,
            })
        })
        .collect();
    RenderResult {
        hits,
        scroll_offset: state.offset(),
        max_scroll: state.max_offset(total_rows),
        viewport_rows: state.viewport_rows(),
        ..RenderResult::default()
    }
}

fn file_node_id(index: usize) -> String {
    format!("file-{index}")
}

fn file_list_item(file: &ChangedFile, index: usize) -> ListItem {
    let state = file.state_label();
    let state_width = u16::try_from(UnicodeWidthStr::width(state)).unwrap_or(u16::MAX);
    ListItem::new(file_node_id(index), file.list_name())
        .leading(ListItemSlot::status(
            StatusSymbol::new(file.status_symbol().to_string(), state)
                .tone(status_tone(file.status_symbol()))
                .preserve_tone_when_selected(true),
        ))
        .value(state)
        .value_min_width(state_width.saturating_add(17))
        .activate_action(OPEN_FILE_ACTION)
}

const fn status_tone(status: char) -> ListItemTone {
    match status {
        'A' | '?' => ListItemTone::Success,
        'D' | 'U' => ListItemTone::Danger,
        'R' | 'C' => ListItemTone::Info,
        _ => ListItemTone::Warning,
    }
}

fn file_list_theme(theme: KitTheme) -> PageTheme {
    let mut page = PageTheme::for_theme(theme);
    page.style = Style::new().fg(theme.text);
    page.item = Style::new().fg(theme.text);
    page.value = Style::new().fg(theme.muted);
    page.selected = theme.selected_row;
    page.selected_item = Style::new();
    page.selected_value = Style::new();
    page.scrollbar_track = theme.scrollbar_track;
    page.scrollbar_thumb = theme.scrollbar_thumb;
    page
}

#[cfg(test)]
fn render_file_list_legacy(
    frame: &mut Frame<'_>,
    area: Rect,
    view: FileListView<'_>,
    theme: KitTheme,
    drags: &mut DragSurface,
) -> RenderResult {
    if area.is_empty() {
        return RenderResult::default();
    }
    if view.files.is_empty() {
        let row = Rect::new(
            area.x,
            area.y.saturating_add(area.height.saturating_sub(1) / 2),
            area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new("working tree clean")
                .style(Style::new().fg(theme.muted))
                .alignment(Alignment::Center),
            row,
        );
        return RenderResult {
            viewport_rows: usize::from(area.height),
            ..RenderResult::default()
        };
    }
    let total_rows = view.files.len();
    let show_scrollbar = total_rows > usize::from(area.height) && area.width > 1;
    let rows_area = if show_scrollbar {
        Rect::new(area.x, area.y, area.width - 1, area.height)
    } else {
        area
    };
    let viewport_rows = usize::from(rows_area.height);
    let max_scroll = total_rows.saturating_sub(viewport_rows);
    let selected = view.selected.min(view.files.len() - 1);
    let requested_scroll = view.requested_scroll.min(max_scroll);
    let scroll_offset = if view.reveal_selected {
        reveal_selected_row(selected, viewport_rows, requested_scroll, max_scroll)
    } else {
        requested_scroll
    };
    let mut hits = Vec::new();
    for row in 0..rows_area.height {
        let index = scroll_offset.saturating_add(usize::from(row));
        let Some(file) = view.files.get(index) else {
            break;
        };
        let row_area = Rect::new(rows_area.x, rows_area.y + row, rows_area.width, 1);
        render_file_row_legacy(frame.buffer_mut(), row_area, file, index == selected, theme);
        drags.register(row_area, view.root.join(file.path()));
        hits.push(RowHit {
            index,
            area: row_area,
        });
    }
    if show_scrollbar {
        frame.render_widget(
            VerticalScrollbar::new(total_rows, viewport_rows, scroll_offset)
                .track_style(theme.scrollbar_track)
                .thumb_style(theme.scrollbar_thumb),
            Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
        );
    }
    RenderResult {
        hits,
        scroll_offset,
        max_scroll,
        viewport_rows,
        ..RenderResult::default()
    }
}

#[cfg(test)]
fn render_file_row_legacy(
    buffer: &mut Buffer,
    area: Rect,
    file: &ChangedFile,
    selected: bool,
    theme: KitTheme,
) {
    if area.is_empty() {
        return;
    }
    let row_style = if selected {
        theme.selected_row
    } else {
        Style::new().fg(theme.text)
    };
    buffer.set_style(area, row_style);
    let padding = SELECTABLE_LEFT_PADDING.min(area.width);
    let content = Rect::new(
        area.x.saturating_add(padding),
        area.y,
        area.width.saturating_sub(padding).saturating_sub(1),
        1,
    );
    if content.is_empty() {
        return;
    }

    let state = file.state_label();
    let state_width = u16::try_from(UnicodeWidthStr::width(state)).unwrap_or(u16::MAX);
    let show_state = content.width >= state_width.saturating_add(14);
    let [label_area, state_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(if show_state {
            state_width.saturating_add(1)
        } else {
            0
        }),
    ])
    .areas(content);
    let status_style = Style::new().fg(status_color(file.status_symbol(), theme.scheme));
    frame_line(
        buffer,
        label_area,
        Line::from(vec![
            Span::styled(file.status_symbol().to_string(), status_style),
            Span::raw("  "),
            Span::styled(file.list_name(), row_style),
        ]),
    );
    if show_state {
        let summary_color = if selected {
            theme.selected_row.fg.unwrap_or(theme.text)
        } else {
            theme.muted
        };
        Paragraph::new(state)
            .style(Style::new().fg(summary_color))
            .alignment(Alignment::Right)
            .render(state_area, buffer);
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct DiffDetailView<'a> {
    selection: Option<(usize, usize)>,
    requested_scroll: usize,
    horizontal_scroll: usize,
    colors: Option<&'a DocumentColors>,
}

#[cfg(test)]
fn render_diff_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    document: &DiffDocument,
    view: DiffDetailView<'_>,
    theme: KitTheme,
) -> RenderResult {
    let DiffDetailView {
        selection,
        requested_scroll,
        horizontal_scroll,
        colors,
    } = view;
    if area.is_empty() {
        return RenderResult::default();
    }

    let back_area = Rect::new(area.x, area.y, area.width, 1);
    let back_style = Style::new().fg(theme.text).add_modifier(Modifier::BOLD);
    let back_padding = SELECTABLE_LEFT_PADDING.min(back_area.width);
    frame.render_widget(
        Paragraph::new("← Back").style(back_style),
        Rect::new(
            back_area.x.saturating_add(back_padding),
            back_area.y,
            back_area.width.saturating_sub(back_padding),
            1,
        ),
    );

    let meta_y = area.y.saturating_add(1).saturating_add(DETAIL_GAP_ROWS);
    if meta_y < area.bottom() {
        render_detail_meta(
            frame.buffer_mut(),
            Rect::new(area.x, meta_y, area.width, DETAIL_META_ROWS),
            document,
            theme,
        );
    }

    let diff_y = meta_y
        .saturating_add(DETAIL_META_ROWS)
        .saturating_add(DETAIL_GAP_ROWS);
    let diff_outer = Rect::new(
        area.x,
        diff_y,
        area.width,
        area.bottom().saturating_sub(diff_y),
    );
    let total_rows = document.lines.len().max(1);
    let show_scrollbar = total_rows > usize::from(diff_outer.height) && diff_outer.width > 1;
    let scrollbar_width = u16::from(show_scrollbar);
    let horizontal_padding = if diff_outer.width >= 5 {
        SELECTABLE_LEFT_PADDING
    } else if diff_outer.width >= 3 {
        1
    } else {
        0
    };
    let content_area = Rect::new(
        diff_outer.x.saturating_add(horizontal_padding),
        diff_outer.y,
        diff_outer
            .width
            .saturating_sub(horizontal_padding.saturating_mul(2))
            .saturating_sub(scrollbar_width),
        diff_outer.height,
    );
    let viewport_rows = usize::from(content_area.height);
    let max_scroll = total_rows.saturating_sub(viewport_rows);
    let scroll_offset = requested_scroll.min(max_scroll);
    let longest_line = document
        .lines
        .iter()
        .map(|line| UnicodeWidthStr::width(expand_tabs(line).as_str()))
        .max()
        .unwrap_or(0);
    let max_horizontal_scroll = longest_line.saturating_sub(usize::from(content_area.width));
    let horizontal_scroll = horizontal_scroll.min(max_horizontal_scroll);

    let rows_width = diff_outer.width.saturating_sub(scrollbar_width);
    let mut diff_hits = Vec::new();
    if !content_area.is_empty() {
        if document.lines.is_empty() {
            frame.render_widget(
                Paragraph::new("No textual diff available").style(Style::new().fg(theme.muted)),
                content_area,
            );
        } else {
            for row in 0..content_area.height {
                let index = scroll_offset.saturating_add(usize::from(row));
                let Some(line) = document.lines.get(index) else {
                    break;
                };
                let row_area = Rect::new(diff_outer.x, content_area.y + row, rows_width, 1);
                let selected = selection.is_some_and(|(start, end)| index >= start && index <= end);
                let mut style = diff_line_style(line, theme);
                let row_background = if selected {
                    theme.selected_row.bg
                } else {
                    diff_row_background(line, theme.scheme)
                };
                if let Some(background) = row_background {
                    frame
                        .buffer_mut()
                        .set_style(row_area, Style::new().bg(background));
                    style = style.bg(background);
                }
                let row_rect =
                    Rect::new(content_area.x, content_area.y + row, content_area.width, 1);
                let syntax = colors
                    .and_then(|lines| lines.get(index))
                    .and_then(Option::as_ref);
                if let Some(spans) = syntax {
                    let mut styled = Vec::with_capacity(spans.len() + 1);
                    styled.push((style, line.chars().take(1).collect::<String>()));
                    for (color, text) in spans {
                        let mut span_style = Style::new().fg(*color);
                        if let Some(background) = row_background {
                            span_style = span_style.bg(background);
                        }
                        styled.push((span_style, text.clone()));
                    }
                    frame.render_widget(
                        Paragraph::new(Line::from(visible_spans(
                            &styled,
                            horizontal_scroll,
                            content_area.width,
                        ))),
                        row_rect,
                    );
                } else {
                    let expanded = expand_tabs(line);
                    let visible = visible_cells(&expanded, horizontal_scroll, content_area.width);
                    frame.render_widget(Paragraph::new(visible).style(style), row_rect);
                }
                diff_hits.push(RowHit {
                    index,
                    area: row_area,
                });
            }
        }
    }

    if show_scrollbar {
        frame.render_widget(
            VerticalScrollbar::new(total_rows, viewport_rows, scroll_offset)
                .track_style(theme.scrollbar_track)
                .thumb_style(theme.scrollbar_thumb),
            Rect::new(
                diff_outer.right().saturating_sub(1),
                diff_outer.y,
                1,
                diff_outer.height,
            ),
        );
    }

    RenderResult {
        diff_hits,
        back_button: RectHit::from_rect(back_area),
        scroll_offset,
        max_scroll,
        viewport_rows,
        max_horizontal_scroll,
        ..RenderResult::default()
    }
}

#[cfg(test)]
fn render_detail_meta(buffer: &mut Buffer, area: Rect, document: &DiffDocument, theme: KitTheme) {
    if area.is_empty() {
        return;
    }
    let padding = SELECTABLE_LEFT_PADDING.min(area.width);
    let content = Rect::new(
        area.x.saturating_add(padding),
        area.y,
        area.width.saturating_sub(padding).saturating_sub(1),
        1,
    );
    if content.is_empty() {
        return;
    }
    let summary = format!("+{} −{}", document.additions, document.deletions);
    let summary_width = u16::try_from(UnicodeWidthStr::width(summary.as_str()))
        .unwrap_or(u16::MAX)
        .min(content.width);
    let show_summary = content.width >= summary_width.saturating_add(12);
    let [path_area, summary_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(if show_summary {
            summary_width.saturating_add(1)
        } else {
            0
        }),
    ])
    .areas(content);
    frame_line(
        buffer,
        path_area,
        Line::from(vec![
            Span::styled(
                document.file.status_symbol().to_string(),
                Style::new().fg(status_color(document.file.status_symbol(), theme.scheme)),
            ),
            Span::raw("  "),
            Span::styled(
                document.file.display_path(),
                Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]),
    );
    if show_summary {
        Paragraph::new(summary)
            .style(Style::new().fg(theme.muted))
            .alignment(Alignment::Right)
            .render(summary_area, buffer);
    }
}

#[cfg(test)]
fn frame_line(buffer: &mut Buffer, area: Rect, line: Line<'_>) {
    Paragraph::new(line).render(area, buffer);
}

#[cfg(test)]
fn reveal_selected_row(
    selected: usize,
    viewport_rows: usize,
    current: usize,
    max_scroll: usize,
) -> usize {
    if viewport_rows == 0 {
        return 0;
    }
    if selected < current {
        selected.min(max_scroll)
    } else if selected >= current.saturating_add(viewport_rows) {
        selected
            .saturating_add(1)
            .saturating_sub(viewport_rows)
            .min(max_scroll)
    } else {
        current.min(max_scroll)
    }
}

#[cfg(test)]
fn status_color(status: char, scheme: ColorScheme) -> Color {
    match status {
        'A' | '?' => match scheme {
            ColorScheme::Dark => Color::LightGreen,
            ColorScheme::Light => Color::Green,
        },
        'D' | 'U' => match scheme {
            ColorScheme::Dark => Color::LightRed,
            ColorScheme::Light => Color::Red,
        },
        'R' | 'C' => match scheme {
            ColorScheme::Dark => Color::LightBlue,
            ColorScheme::Light => Color::Blue,
        },
        _ => match scheme {
            ColorScheme::Dark => Color::LightYellow,
            ColorScheme::Light => Color::Yellow,
        },
    }
}

/// Full-row tint behind added and removed patch lines, GitHub-style: the
/// background carries the change kind, so those lines keep the plain text
/// foreground from [`diff_line_style`].
#[cfg(test)]
fn diff_row_background(line: &str, scheme: ColorScheme) -> Option<Color> {
    if line.starts_with("+++") || line.starts_with("---") {
        return None;
    }
    if line.starts_with('+') {
        Some(match scheme {
            ColorScheme::Dark => Color::Rgb(18, 44, 24),
            ColorScheme::Light => Color::Rgb(224, 245, 228),
        })
    } else if line.starts_with('-') {
        Some(match scheme {
            ColorScheme::Dark => Color::Rgb(58, 26, 26),
            ColorScheme::Light => Color::Rgb(255, 233, 231),
        })
    } else {
        None
    }
}

#[cfg(test)]
fn diff_line_style(line: &str, theme: KitTheme) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        Style::new().fg(theme.muted).add_modifier(Modifier::BOLD)
    } else if line.starts_with("@@") {
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else if line.starts_with("diff --git") {
        Style::new().fg(theme.text).add_modifier(Modifier::BOLD)
    } else if line.starts_with("index ")
        || line.starts_with("new file mode ")
        || line.starts_with("deleted file mode ")
        || line.starts_with("similarity index ")
        || line.starts_with("rename from ")
        || line.starts_with("rename to ")
    {
        Style::new().fg(theme.subtle)
    } else {
        Style::new().fg(theme.text)
    }
}

fn expand_tabs(line: &str) -> String {
    let mut expanded = String::with_capacity(line.len());
    let mut column = 0usize;
    for character in line.chars() {
        if character == '\t' {
            let spaces = 4 - (column % 4);
            expanded.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            expanded.push(character);
            column += character.width().unwrap_or(0);
        }
    }
    expanded
}

/// Span-aware sibling of [`visible_cells`]: expands tabs, skips `offset`
/// display columns, and clips to `width`, preserving each fragment's style.
#[cfg(test)]
fn visible_spans(spans: &[(Style, String)], offset: usize, width: u16) -> Vec<Span<'static>> {
    let width = usize::from(width);
    let mut result = Vec::new();
    if width == 0 {
        return result;
    }
    let mut column = 0usize;
    let mut taken = 0usize;
    'spans: for (style, text) in spans {
        let mut visible = String::new();
        for character in text.chars() {
            let expanded = if character == '\t' {
                " ".repeat(4 - (column % 4))
            } else {
                character.to_string()
            };
            for cell in expanded.chars() {
                let cell_width = cell.width().unwrap_or(0);
                let next = column.saturating_add(cell_width);
                if next <= offset || column < offset {
                    column = next;
                    continue;
                }
                if taken.saturating_add(cell_width) > width {
                    if !visible.is_empty() {
                        result.push(Span::styled(visible, *style));
                    }
                    break 'spans;
                }
                visible.push(cell);
                column = next;
                taken = taken.saturating_add(cell_width);
            }
        }
        if !visible.is_empty() {
            result.push(Span::styled(visible, *style));
        }
    }
    result
}

#[cfg(test)]
fn visible_cells(line: &str, offset: usize, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut source_column = 0usize;
    let mut visible_width = 0usize;
    for character in line.chars() {
        let character_width = character.width().unwrap_or(0);
        let next_source = source_column.saturating_add(character_width);
        if next_source <= offset {
            source_column = next_source;
            continue;
        }
        if source_column < offset {
            source_column = next_source;
            continue;
        }
        if visible_width.saturating_add(character_width) > width {
            break;
        }
        result.push(character);
        source_column = next_source;
        visible_width += character_width;
    }
    result
}

/// Sidebar title for a working tree: `<folder> · <branch>` (short commit id
/// when detached), or just the folder when Git has no answer.
fn session_title_for(repository: &crate::git::Repository) -> String {
    let folder = repository
        .root()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| repository.root().display().to_string());
    match repository.branch() {
        Some(branch) => format!("{folder} · {branch}"),
        None => folder,
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use ratatui::backend::TestBackend;

    use super::*;

    fn buffer_line(buffer: &Buffer, row: u16) -> String {
        (buffer.area.x..buffer.area.right())
            .map(|column| buffer[(column, row)].symbol())
            .collect::<String>()
    }

    fn document() -> DiffDocument {
        DiffDocument {
            file: ChangedFile::fixture("src/ui.rs", ' ', 'M'),
            lines: vec![
                "diff --git a/src/ui.rs b/src/ui.rs".into(),
                "@@ -1 +1 @@".into(),
                "-old".into(),
                "+new".into(),
            ],
            additions: 1,
            deletions: 1,
        }
    }

    fn file_app() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        for arguments in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Unpeel Tests"],
            vec!["config", "user.email", "tests@unpeel.local"],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .args(arguments)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        for name in ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"] {
            std::fs::write(directory.path().join(name), format!("// {name}\n")).unwrap();
        }
        let repository = crate::git::Repository::discover(directory.path()).unwrap();
        let app = App::new(repository).unwrap();
        assert_eq!(app.files.len(), 5);
        (directory, app)
    }

    #[test]
    fn escape_is_back_in_detail_and_does_not_exit_the_list() {
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action_for_key(escape, true, false), Some(InputAction::Back));
        assert_eq!(action_for_key(escape, false, false), None);
    }

    #[test]
    fn selection_keys_send_and_clear_instead_of_leaving() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let send = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(
            action_for_key(enter, true, true),
            Some(InputAction::SendToAgent)
        );
        assert_eq!(action_for_key(enter, true, false), Some(InputAction::Back));
        assert_eq!(
            action_for_key(escape, true, true),
            Some(InputAction::ClearSelection)
        );
        assert_eq!(
            action_for_key(send, true, true),
            Some(InputAction::SendToAgent)
        );
        assert_eq!(action_for_key(send, true, false), None);
    }

    #[test]
    fn migrated_diff_key_sequence_uses_the_shared_clamped_list_navigation() {
        let (_directory, mut app) = file_app();
        app.viewport_rows = 3;
        let mut outcomes = Vec::new();
        for code in [
            KeyCode::Up,
            KeyCode::Char('k'),
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::End,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::PageDown,
        ] {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            let action = action_for_key(key, false, false).expect("shared list action");
            assert!(!handle_action(&mut app, action));
            outcomes.push(app.selected);
        }
        assert_eq!(outcomes, vec![0, 0, 1, 2, 4, 4, 0, 2]);
    }

    #[test]
    fn semantic_file_list_carries_status_slots_selection_and_compact_deltas() {
        let (_directory, mut app) = file_app();
        let agent = AgentBridge::new();
        let first = semantic_node(&app, true);
        let unpeel_app_kit::UiComponent::Page(page) = &first.element else {
            unreachable!()
        };
        page.validate().unwrap();
        assert_eq!(page.list().id, FILE_LIST_ID);
        assert_eq!(page.list().selected_id.as_deref(), Some("file-0"));
        assert_eq!(page.list().select.as_deref(), Some(SELECT_FILE_ACTION));
        assert_eq!(page.footer.actions[0].id, "refresh-diffs");
        assert_eq!(page.footer.actions[0].accelerator.as_deref(), Some("r"));
        assert!(matches!(
            page.list().items[0].leading,
            Some(ListItemSlot::Status(_))
        ));

        apply_semantic_action(
            &mut app,
            &agent,
            &unpeel_app_kit::UiAction::new(
                FILE_LIST_ID,
                SELECT_FILE_ACTION,
                UiEventKind::Change,
                UiEventValue::Text("file-3".to_owned()),
            ),
        )
        .unwrap();
        assert_eq!(app.selected, 3);
        let next = semantic_node(&app, true);
        assert!(matches!(
            page_delta_operations(&first, &next).as_slice(),
            [UiDeltaOperation::ListSetSelection { list_id, selected_id }]
                if list_id == FILE_LIST_ID && selected_id.as_deref() == Some("file-3")
        ));
    }

    #[test]
    fn semantic_diff_detail_publishes_the_complete_styled_patch_and_native_actions() {
        let (_directory, mut app) = file_app();
        let mut document = document();
        document
            .lines
            .extend((0..20_050).map(|index| format!(" context line {index}")));
        app.screen = Screen::Diff(document);
        let page = semantic_page(&app, true);
        page.validate().unwrap();
        assert_eq!(page.back.as_deref(), Some(CLOSE_DIFF_ACTION));
        assert_eq!(page.footer.actions[0].id, "refresh-diffs");
        assert_eq!(page.footer.actions[0].accelerator.as_deref(), Some("r"));
        let content = page.content().expect("Content detail body");
        assert_eq!(content.lines.len(), 20_054);
        assert_eq!(content.lines[2].tone, ContentLineTone::Removed);
        assert_eq!(content.lines[3].tone, ContentLineTone::Added);
        assert_eq!(
            content.context_menu.as_ref().unwrap().items.len(),
            4,
            "native and web detail renderers expose the terminal actions"
        );

        app.notify("Diff lines copied");
        assert!(
            semantic_page(&app, true)
                .title
                .ends_with(" · Diff lines copied"),
            "terminal footer notices must remain visible in native and web details"
        );
    }

    #[test]
    fn semantic_diff_selection_uses_a_content_delta() {
        let (_directory, mut app) = file_app();
        app.screen = Screen::Diff(document());
        let first = semantic_node(&app, true);
        app.begin_selection(1);
        app.extend_selection(3);
        let next = semantic_node(&app, true);
        assert!(matches!(
            page_delta_operations(&first, &next).as_slice(),
            [UiDeltaOperation::ContentSetSelection { content_id, selection }]
                if content_id == "diff-content"
                    && selection.as_ref().is_some_and(|selection|
                        selection.anchor_id == "diff-line-1"
                            && selection.head_id == "diff-line-3")
        ));
    }

    #[test]
    fn file_labels_use_the_shared_two_cell_inset_without_an_app_title() {
        let theme = KitTheme::dark();
        let files = vec![ChangedFile::fixture("src/ui.rs", ' ', 'M')];
        let mut terminal = Terminal::new(TestBackend::new(48, 8)).unwrap();
        let mut list_result = RenderResult::default();
        let mut drags = DragSurface::disabled();
        drags.begin_frame();
        terminal
            .draw(|frame| {
                list_result = render_file_list(
                    frame,
                    frame.area(),
                    FileListView {
                        root: Path::new("/repo"),
                        files: &files,
                        selected: 0,
                        requested_scroll: 0,
                        reveal_selected: true,
                    },
                    theme,
                    &mut drags,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer_line(buffer, 0).starts_with("  M  ui.rs"));
        assert!(!buffer_line(buffer, 0).contains("src/ui.rs"));
        assert_eq!(buffer[(47, 0)].bg, theme.selected_row.bg.unwrap());
        assert_eq!(list_result.hits[0].area.width, 48);
        assert_eq!(drags.regions().len(), 1);
        assert_eq!(drags.regions()[0].area, Rect::new(0, 0, 48, 1));
        assert_eq!(drags.regions()[0].path, Path::new("/repo/src/ui.rs"));
    }

    #[test]
    fn app_kit_file_list_matches_the_frozen_renderer_buffer_for_buffer() {
        let files = vec![
            ChangedFile::fixture("src/modified.rs", ' ', 'M'),
            ChangedFile::fixture("src/added.rs", 'A', ' '),
            ChangedFile::fixture("src/untracked.rs", '?', '?'),
            ChangedFile::fixture("src/deleted.rs", 'D', ' '),
        ];
        let cases = [
            (18, 1, 0, 0, true),
            (26, 2, 2, 0, true),
            (40, 2, 1, 1, false),
            (72, 8, 3, 0, true),
        ];
        for theme in [KitTheme::light(), KitTheme::dark()] {
            for (width, height, selected, requested_scroll, reveal_selected) in cases {
                let view = FileListView {
                    root: Path::new("/repo"),
                    files: &files,
                    selected,
                    requested_scroll,
                    reveal_selected,
                };
                let mut current = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut legacy = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut current_result = RenderResult::default();
                let mut legacy_result = RenderResult::default();
                let mut current_drags = DragSurface::disabled();
                let mut legacy_drags = DragSurface::disabled();
                current_drags.begin_frame();
                legacy_drags.begin_frame();
                current
                    .draw(|frame| {
                        current_result =
                            render_file_list(frame, frame.area(), view, theme, &mut current_drags);
                    })
                    .unwrap();
                legacy
                    .draw(|frame| {
                        legacy_result = render_file_list_legacy(
                            frame,
                            frame.area(),
                            view,
                            theme,
                            &mut legacy_drags,
                        );
                    })
                    .unwrap();
                assert_eq!(
                    current.backend().buffer(),
                    legacy.backend().buffer(),
                    "{width}x{height}, selected {selected}, {:?}",
                    theme.scheme
                );
                assert_eq!(current_result.hits, legacy_result.hits);
                assert_eq!(current_result.scroll_offset, legacy_result.scroll_offset);
                assert_eq!(current_result.max_scroll, legacy_result.max_scroll);
                assert_eq!(current_result.viewport_rows, legacy_result.viewport_rows);
                assert_eq!(current_drags.regions(), legacy_drags.regions());
            }
        }
    }

    #[test]
    fn detail_is_transparent_except_for_changed_line_tints() {
        let theme = KitTheme::light();
        let document = document();
        let mut terminal = Terminal::new(TestBackend::new(44, 12)).unwrap();
        let mut result = RenderResult::default();
        terminal
            .draw(|frame| {
                result = render_diff_detail(
                    frame,
                    frame.area(),
                    &document,
                    DiffDetailView::default(),
                    theme,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert!(buffer_line(buffer, 0).starts_with("  ← Back"));
        assert!(
            (0..44).all(|x| buffer[(x, 0)].bg == Color::Reset),
            "Back should not paint a row background"
        );
        assert_eq!(buffer[(2, 6)].fg, theme.text);
        assert_eq!(buffer[(2, 7)].fg, theme.text);
        assert!(
            (0..44).all(|x| [4, 5, 8, 11]
                .iter()
                .all(|y| buffer[(x, *y)].bg == Color::Reset)),
            "unchanged diff rows should stay transparent"
        );
        let removed = diff_row_background("-old", theme.scheme).unwrap();
        let added = diff_row_background("+new", theme.scheme).unwrap();
        assert!(
            (0..44).all(|x| buffer[(x, 6)].bg == removed && buffer[(x, 7)].bg == added),
            "changed rows should carry full-width green/red tints"
        );
        assert_eq!(diff_row_background("+++ b/src/ui.rs", theme.scheme), None);
        assert_eq!(diff_row_background(" context", theme.scheme), None);
        assert_eq!(result.back_button.unwrap().area.width, 44);
    }

    #[test]
    fn selected_diff_lines_highlight_and_report_hit_rows() {
        let theme = KitTheme::dark();
        let document = document();
        let mut terminal = Terminal::new(TestBackend::new(44, 12)).unwrap();
        let mut result = RenderResult::default();
        terminal
            .draw(|frame| {
                result = render_diff_detail(
                    frame,
                    frame.area(),
                    &document,
                    DiffDetailView {
                        selection: Some((2, 3)),
                        ..DiffDetailView::default()
                    },
                    theme,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let highlight = theme.selected_row.bg.unwrap();

        // Diff rows start below back (0), gap (1), meta (2), and gap (3).
        assert_eq!(result.diff_hits.len(), 4);
        assert_eq!(result.diff_hits[0].index, 0);
        assert_eq!(result.diff_hits[0].area, Rect::new(0, 4, 44, 1));
        assert_eq!(buffer[(0, 4)].bg, Color::Reset);
        // Selection overrides the added/removed row tints.
        assert_eq!(buffer[(0, 6)].bg, highlight);
        assert_eq!(buffer[(43, 7)].bg, highlight);
        assert_eq!(buffer[(2, 6)].fg, theme.text);
    }

    #[test]
    fn drag_lookup_clamps_to_the_visible_diff_rows() {
        let rendered = RenderResult {
            diff_hits: vec![
                RowHit {
                    index: 5,
                    area: Rect::new(0, 4, 40, 1),
                },
                RowHit {
                    index: 6,
                    area: Rect::new(0, 5, 40, 1),
                },
            ],
            ..RenderResult::default()
        };
        assert_eq!(diff_line_near(&rendered, Position::new(3, 0)), Some(5));
        assert_eq!(diff_line_near(&rendered, Position::new(39, 5)), Some(6));
        assert_eq!(diff_line_near(&rendered, Position::new(0, 11)), Some(6));
        assert_eq!(diff_line_at(&rendered, Position::new(0, 11)), None);
    }

    #[test]
    fn selection_references_carry_the_relative_path_and_file_line_numbers() {
        let document = document();
        // "-old" and "+new" both anchor at line 1 of the new file.
        assert_eq!(selection_reference(&document, (2, 3)), "src/ui.rs:1");
        // A selection covering only headers falls back to the bare path.
        assert_eq!(selection_reference(&document, (0, 0)), "src/ui.rs");
    }

    #[test]
    fn diff_rows_map_to_new_file_line_spans_per_hunk() {
        let lines: Vec<String> = [
            "diff --git a/x b/x",
            "@@ -10,3 +12,4 @@ fn demo()",
            " context",   // line 12
            "-removed",   // anchors at 13
            "+added",     // line 13
            "+added-two", // line 14
            " context",   // line 15
            "@@ -30,2 +33,2 @@",
            " context", // line 33
            "\\ No newline at end of file",
            " context", // line 34
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        assert_eq!(new_file_line_span(&lines, (2, 2)), Some((12, 12)));
        assert_eq!(new_file_line_span(&lines, (3, 5)), Some((13, 14)));
        assert_eq!(new_file_line_span(&lines, (2, 6)), Some((12, 15)));
        assert_eq!(new_file_line_span(&lines, (8, 10)), Some((33, 34)));
        assert_eq!(new_file_line_span(&lines, (0, 0)), None);
        assert_eq!(hunk_new_start("@@ -10,3 +12,4 @@ fn demo()"), Some(12));
        assert_eq!(hunk_new_start("@@ -1 +1 @@"), Some(1));
    }

    #[test]
    fn terminal_context_menus_are_interpreted_from_the_semantic_menu_spec() {
        let scheme = ColorScheme::Dark;
        let anchor = Position::new(4, 4);
        let detail_path = PathBuf::from("/repo/a.rs");
        let with_agent_spec = semantic_diff_menu(true, true);
        let with_agent = diff_menu(Some(detail_path.clone()), &with_agent_spec, anchor, scheme);
        assert_eq!(with_agent.items().len(), 4);
        assert_eq!(with_agent.items()[0].label(), "Open in editor");
        assert_eq!(with_agent.items()[0].value(), "open-in-editor");
        assert_eq!(with_agent.items()[1].label(), "Send to agent");
        assert_eq!(with_agent.items()[1].value(), "send-lines");
        assert_eq!(with_agent.items()[3].value(), "refresh-diff");

        let without_agent_spec = semantic_diff_menu(false, false);
        let without_agent = diff_menu(None, &without_agent_spec, anchor, scheme);
        assert_eq!(without_agent.items().len(), 3);
        assert!(!without_agent.items()[0].is_enabled());
        assert_eq!(without_agent.items()[1].label(), "Copy lines");

        let list_spec = semantic_file_menu(true);
        let list = list_menu(
            "a.rs".to_owned(),
            PathBuf::from("/repo/a.rs"),
            &list_spec,
            anchor,
            scheme,
        );
        assert_eq!(list.items().len(), 3);
        assert_eq!(list.items()[0].value(), "open-in-editor");
        assert_eq!(list.items()[1].value(), "send-path");
        assert_eq!(list.items()[2].value(), "copy-path");
    }

    #[test]
    fn long_and_tabbed_lines_clip_by_terminal_cells() {
        assert_eq!(expand_tabs("+\tone"), "+   one");
        assert_eq!(visible_cells("abcdef", 2, 3), "cde");
        assert_eq!(visible_cells("a界bc", 1, 3), "界b");
    }

    #[test]
    fn selection_reveal_moves_only_as_far_as_needed() {
        assert_eq!(reveal_selected_row(0, 3, 0, 7), 0);
        assert_eq!(reveal_selected_row(3, 3, 0, 7), 1);
        assert_eq!(reveal_selected_row(8, 3, 1, 7), 6);
        assert_eq!(reveal_selected_row(1, 3, 5, 7), 1);
    }

    #[test]
    fn tiny_surfaces_do_not_panic() {
        let theme = KitTheme::dark();
        let files = vec![ChangedFile::fixture("x", '?', '?')];
        let document = DiffDocument {
            file: files[0].clone(),
            lines: vec!["+x".into()],
            additions: 1,
            deletions: 0,
        };
        let mut terminal = Terminal::new(TestBackend::new(1, 1)).unwrap();
        let mut drags = DragSurface::disabled();
        terminal
            .draw(|frame| {
                let _ = render_file_list(
                    frame,
                    frame.area(),
                    FileListView {
                        root: Path::new("/repo"),
                        files: &files,
                        selected: 0,
                        requested_scroll: 0,
                        reveal_selected: true,
                    },
                    theme,
                    &mut drags,
                );
                let _ = render_diff_detail(
                    frame,
                    frame.area(),
                    &document,
                    DiffDetailView {
                        selection: Some((0, 0)),
                        ..DiffDetailView::default()
                    },
                    theme,
                );
            })
            .unwrap();
    }

    #[test]
    fn syntax_spans_render_over_the_row_tints() {
        let theme = KitTheme::dark();
        let document = document();
        let highlighter = Highlighter::new();
        let colors = highlighter
            .document_colors(&document, theme.scheme)
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(44, 12)).unwrap();
        terminal
            .draw(|frame| {
                let _ = render_diff_detail(
                    frame,
                    frame.area(),
                    &document,
                    DiffDetailView {
                        colors: Some(&colors),
                        ..DiffDetailView::default()
                    },
                    theme,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        // Row 7 is "+new": tinted background with syntect foregrounds.
        let added = diff_row_background("+new", theme.scheme).unwrap();
        assert_eq!(buffer[(2, 7)].bg, added);
        assert_eq!(buffer[(2, 7)].symbol(), "+");
        let code_cell = &buffer[(3, 7)];
        assert_eq!(code_cell.bg, added);
        assert!(
            matches!(code_cell.fg, Color::Rgb(..)),
            "code should use syntect RGB foregrounds, got {:?}",
            code_cell.fg
        );
    }

    #[test]
    fn span_clipping_expands_tabs_and_honors_offset_and_width() {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let plain = Style::new();
        let spans = vec![(plain, "+\tab".to_owned()), (bold, "cdef".to_owned())];

        let full = visible_spans(&spans, 0, 12);
        let text = full
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(text, "+   abcdef");
        assert_eq!(full.last().unwrap().style, bold);

        let clipped = visible_spans(&spans, 2, 3);
        let text = clipped
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(text, "  a");
        assert!(visible_spans(&spans, 0, 0).is_empty());
    }
}
