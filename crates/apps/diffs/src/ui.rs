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
    ListNavigationAction, ListRowLayout, ListState, MenuTheme, Page, PageTab, PageTheme,
    PageToolbar, PageToolbarState, PopupMenu, SemanticMenu, SemanticMenuAnchor, SemanticMenuItem,
    SemanticMenuPresentation, StatusSymbol, ThemeMonitor, UiAction, UiBridge, UiBridgeEvent,
    UiComponent, UiEventKind, UiEventOutcome, UiEventValue, UiNode, clipboard_sequence,
    page_delta_operations,
};
#[cfg(test)]
use unpeel_app_kit::{SELECTABLE_LEFT_PADDING, VerticalScrollbar};

use crate::app::{App, Screen, Tab};
use crate::git::{ChangedFile, DiffDocument, RemoteAction};
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
        AppMetadata::new(crate::install::APP_ID, "Git", env!("CARGO_PKG_VERSION"))
            .description("Git changes and commit history"),
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
    let mut toolbar_state = PageToolbarState::default();
    let mut needs_draw = true;
    let mut last_sync = Instant::now();
    let mut needs_publish = false;
    let mut pending_event = None;
    let mut can_send = agent.label().is_some();

    loop {
        let operation_completed = app.poll_remote_action();
        needs_publish |= operation_completed;
        needs_draw |= operation_completed || app.remote_busy.is_some();
        if drain_bridge(
            &mut app,
            &agent,
            &mut bridge,
            &mut ui_revision,
            &mut published,
        )? {
            needs_draw = true;
            needs_publish = true;
        }
        let next_can_send = agent.label().is_some();
        needs_publish |= next_can_send != can_send;
        can_send = next_can_send;
        if needs_publish {
            publish_semantic_projection(
                &app,
                can_send,
                &mut bridge,
                &mut ui_revision,
                &mut published,
            )?;
            needs_publish = false;
        }
        if needs_draw {
            let title = session_title_for(&app.repository, app.branch.as_deref());
            if title != session_title {
                reporter.set_title(&title);
                session_title = title;
            }
            reporter.set_context(&serde_json::json!({
                "root": app.root(),
                "view": match app.screen { Screen::Files => "files", Screen::History => "history", Screen::CommitFiles => "commit", Screen::Diff(_) => "diff" },
                "tab": if app.tab == Tab::History { "history" } else { "changes" },
                "commit": app.commit.as_ref().map(|commit| &commit.id),
                "changed_files": app.files.len(),
                "selected_path": app.selected_absolute_path(),
                "selected_status": app.selected_file().map(|file| file.state_label()),
                "selected_diff_lines": app
                    .selection_range()
                    .map(|(start, end)| [start + 1, end + 1]),
            }));
            if bridge.should_render_terminal() {
                rendered = terminal.draw(
                    &published,
                    &app,
                    &mut drags,
                    menu.as_mut(),
                    &mut toolbar_state,
                    theme,
                )?;
                app.apply_render_metrics(
                    rendered.scroll_offset,
                    rendered.max_scroll,
                    rendered.viewport_rows,
                    rendered.max_horizontal_scroll,
                );
            }
            needs_draw = false;
        }

        if pending_event.is_none() && !event::poll(Duration::from_millis(250))? {
            drags.heartbeat()?;
            if theme_monitor.refresh() {
                theme = theme_monitor.theme();
                needs_draw = true;
            }
            if follow_agent_context
                && app.remote_busy.is_none()
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
                        Ok(true) => {
                            needs_draw = true;
                            needs_publish = true;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            app.fail(error);
                            needs_draw = true;
                            needs_publish = true;
                        }
                    }
                }
                agent.refresh();
            }
            // Quietly follow the working tree while the user is not
            // mid-interaction; transient Git errors are retried next tick.
            if menu.is_none()
                && !toolbar_state.is_open()
                && !selecting
                && last_sync.elapsed() >= AUTO_SYNC_INTERVAL
            {
                last_sync = Instant::now();
                if app.sync().unwrap_or(false) {
                    needs_draw = true;
                    needs_publish = true;
                }
            }
            continue;
        }
        let event = match pending_event.take() {
            Some(event) => event,
            None => event::read()?,
        };
        if menu.is_none() && !toolbar_state.is_open() && wheel_delta(&event).is_some() {
            // Offsets are renderer-local: use the already-published Page, and
            // drain a burst before drawing. Clamp every tick in order so excess
            // motion at an edge cannot cancel a subsequent direction reversal.
            needs_publish |= app.notice.is_some();
            let batch = scroll_batch(&mut app, event, || {
                if event::poll(Duration::ZERO)? {
                    event::read().map(Some)
                } else {
                    Ok(None)
                }
            })?;
            needs_draw |= batch.changed;
            pending_event = batch.pending;
            continue;
        }
        // Commands/selection can change the semantic tree. Pointer motion,
        // release, and resize only affect the terminal's local presentation.
        needs_publish |= !matches!(event, Event::Mouse(mouse)
            if matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Up(_)))
            && !matches!(event, Event::Resize(_, _));
        if !matches!(event, Event::Key(key) if is_force_quit(key))
            && menu.is_none()
            && let UiComponent::Page(page) = &published.element
            && let Some(action) = toolbar_state.handle(
                &event,
                page.toolbar.as_ref(),
                rendered.title_area,
                MenuTheme::for_color_scheme(theme.scheme),
            )
        {
            if let Some((id, action)) = action
                && let Err(error) =
                    apply_semantic_action(&mut app, &agent, &UiAction::activate(id, action))
            {
                app.fail(error);
            }
            needs_draw = true;
            continue;
        }
        match event {
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
                if key.code == KeyCode::Esc && app.can_go_back() && !app.is_detail() {
                    app.back();
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
                        } else if let Some(tab) = rendered.tabs_area.and_then(|area| {
                            let UiComponent::Page(page) = &published.element else {
                                return None;
                            };
                            page.tab_at(position, area)
                        }) {
                            selecting = false;
                            let action = UiAction::activate(tab.id.as_str(), tab.action.as_str());
                            if let Err(error) = apply_semantic_action(&mut app, &agent, &action) {
                                app.fail(error);
                            }
                            needs_draw = true;
                        } else if rendered
                            .back_button
                            .is_some_and(|hit| hit.contains(position))
                        {
                            selecting = false;
                            app.back();
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

/// Terminal wheel reports already represent lines (including precise trackpad
/// movement converted by the terminal). Do not multiply their distance again.
fn wheel_delta(event: &Event) -> Option<isize> {
    match event {
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => Some(-1),
            MouseEventKind::ScrollDown => Some(1),
            _ => None,
        },
        _ => None,
    }
}

struct ScrollBatch {
    changed: bool,
    pending: Option<Event>,
}

fn scroll_batch(
    app: &mut App,
    first: Event,
    mut next: impl FnMut() -> io::Result<Option<Event>>,
) -> io::Result<ScrollBatch> {
    let mut result = ScrollBatch {
        changed: false,
        pending: None,
    };
    let mut event = first;
    // Bounded work keeps remote actions and redraws responsive during a long
    // gesture. Never discard a following click, key, resize, or direction change.
    for index in 0..256 {
        let Some(delta) = wheel_delta(&event) else {
            result.pending = Some(event);
            break;
        };
        if let Event::Mouse(mouse) = &event {
            result.changed |= app.pointer.track(mouse);
        }
        result.changed |= app.scroll_vertical(delta);
        if index == 255 {
            break;
        }
        let Some(queued) = next()? else {
            break;
        };
        event = queued;
    }
    Ok(result)
}

fn semantic_node(app: &App, can_send: bool) -> UiNode {
    UiNode::page(SEMANTIC_ROOT_ID, semantic_page(app, can_send))
}

fn semantic_page(app: &App, can_send: bool) -> Page {
    let page = match &app.screen {
        Screen::Files | Screen::CommitFiles => {
            let historical = app.tab == Tab::History;
            let files = app.active_files();
            let mut list = List::new(
                FILE_LIST_ID,
                files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| file_list_item(file, index, historical))
                    .collect(),
            )
            .empty_message(if historical {
                "No files changed in this commit"
            } else {
                "Working tree clean"
            });
            if !files.is_empty() {
                list = list.selected(
                    file_node_id(app.selected.min(files.len() - 1)),
                    SELECT_FILE_ACTION,
                );
            }
            if !historical {
                list = list.context_menu(semantic_file_menu(can_send));
            }
            let title = app
                .commit
                .as_ref()
                .map(|commit| {
                    format!(
                        "{} · {} · {} · {}",
                        commit.short_id, commit.subject, commit.author, commit.date
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "{} · {} changed",
                        app.branch.as_deref().unwrap_or("HEAD"),
                        files.len()
                    )
                });
            let page = Page::new(semantic_page_title(app, &title), list);
            if historical {
                page.back_action(CLOSE_DIFF_ACTION)
            } else {
                page
            }
        }
        Screen::History => {
            let mut list = List::new(
                "git-history",
                app.history
                    .iter()
                    .map(|commit| {
                        ListItem::new(
                            format!("commit-{}", commit.id),
                            control_safe(&commit.subject),
                        )
                        .detail(control_safe(&format!(
                            "{} · {}",
                            commit.author, commit.date
                        )))
                        .value(&commit.short_id)
                        .activate_action("open-commit")
                    })
                    .collect(),
            )
            .row_layout(ListRowLayout::Stacked)
            .empty_message("No commits yet");
            if let Some(commit) = app.history.get(app.selected) {
                list = list.selected(format!("commit-{}", commit.id), "select-commit");
            }
            Page::new(
                semantic_page_title(
                    app,
                    &format!(
                        "{} · {} commits{}",
                        app.branch.as_deref().unwrap_or("HEAD"),
                        app.history.len(),
                        if app.has_more_history { "+" } else { "" }
                    ),
                ),
                list,
            )
        }
        Screen::Diff(document) => {
            let lines = document
                .lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let mut result = semantic_diff_line(index, line);
                    if let Some(runs) = app
                        .syntax
                        .as_ref()
                        .and_then(|lines| lines.get(index))
                        .and_then(Option::as_ref)
                    {
                        // Keep the +/- marker in the change color; code uses
                        // the shared run tones and the row keeps its tint.
                        result.runs[0].text = line[..1].to_owned();
                        let mut column = 1;
                        result.runs.extend(runs.iter().cloned().map(|mut run| {
                            run.text = semantic_diff_text_at(&run.text, &mut column);
                            run
                        }));
                        result.tone = match line.as_bytes()[0] {
                            b'+' => ContentLineTone::Added,
                            b'-' => ContentLineTone::Removed,
                            _ => ContentLineTone::Default,
                        };
                    }
                    result
                })
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
                "{}{} · +{} −{}",
                app.commit
                    .as_ref()
                    .map(|commit| format!("{} · ", commit.short_id))
                    .unwrap_or_default(),
                document.file.path().display(),
                document.additions,
                document.deletions
            );
            Page::with_content(semantic_page_title(app, &title), content)
                .back_action(CLOSE_DIFF_ACTION)
        }
    };
    let mut actions =
        vec![FooterAction::new("refresh-diffs", "refresh", REFRESH_ACTION).accelerator("r")];
    if matches!(app.screen, Screen::History) && app.has_more_history {
        actions.push(
            FooterAction::new("older-commits", "load older", "load-history").accelerator("n"),
        );
    }
    page.toolbar(git_toolbar(app))
        .tabs([
            PageTab::new("changes-tab", "Changes", "show-changes")
                .selected(app.tab == Tab::Changes),
            PageTab::new("history-tab", "History", "show-history")
                .selected(app.tab == Tab::History),
        ])
        .footer_actions(actions)
}

fn change_label(status: char) -> &'static str {
    match status {
        'A' | '?' => "added",
        'U' => "conflicted",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'T' => "type changed",
        _ => "modified",
    }
}

fn git_toolbar(app: &App) -> PageToolbar {
    let busy = app.remote_busy.is_some();
    let primary = app.remote_busy.unwrap_or_else(|| app.remote.primary());
    let label = if busy {
        format!("{}…", primary.name())
    } else if app.remote.remote.is_none() {
        "No remote".to_owned()
    } else {
        match primary {
            RemoteAction::Pull => format!("Pull ↓{}", app.remote.behind),
            RemoteAction::Push => format!("Push ↑{}", app.remote.ahead),
            RemoteAction::Fetch => "Fetch".to_owned(),
        }
    };
    PageToolbar::new(
        FooterAction::new("git-remote-primary", label, remote_action_id(primary))
            .disabled(busy || !app.remote.allows(primary))
            .busy(busy),
    )
    .menu(SemanticMenu::new(
        "Remote actions",
        [RemoteAction::Fetch, RemoteAction::Pull, RemoteAction::Push].map(|action| {
            let label = match action {
                RemoteAction::Fetch => app
                    .remote
                    .remote
                    .as_ref()
                    .map(|r| format!("Fetch {r}"))
                    .unwrap_or("Fetch — no remote".into()),
                RemoteAction::Pull if app.remote.ahead > 0 && app.remote.behind > 0 => {
                    "Pull — branches diverged".into()
                }
                RemoteAction::Pull if app.remote.upstream.is_none() => "Pull — no upstream".into(),
                RemoteAction::Push if app.remote.upstream.is_none() => "Push — no upstream".into(),
                RemoteAction::Pull => format!("Pull (fast-forward) ↓{}", app.remote.behind),
                RemoteAction::Push => format!("Push ↑{}", app.remote.ahead),
            };
            SemanticMenuItem::new(
                format!("{}-menu", remote_action_id(action)),
                label,
                remote_action_id(action),
            )
            .disabled(busy || !app.remote.allows(action))
        }),
    ))
}

fn remote_action_id(action: RemoteAction) -> &'static str {
    match action {
        RemoteAction::Fetch => "git-fetch",
        RemoteAction::Pull => "git-pull",
        RemoteAction::Push => "git-push",
    }
}

fn semantic_page_title(app: &App, base: &str) -> String {
    app.notice.as_ref().map_or_else(
        || base.to_owned(),
        |notice| {
            if notice.error {
                format!("Error: {} · {base}", notice.text)
            } else {
                format!("{base} · {}", notice.text)
            }
        },
    )
}

fn semantic_diff_text(line: &str) -> String {
    semantic_diff_text_at(line, &mut 0)
}

fn semantic_diff_text_at(line: &str, column: &mut usize) -> String {
    let mut label = String::with_capacity(line.len());
    for character in line.chars() {
        if character == '\t' {
            let spaces = 4 - (*column % 4);
            label.extend(std::iter::repeat_n(' ', spaces));
            *column += spaces;
        } else if !character.is_control() {
            label.push(character);
            *column += character.width().unwrap_or(0);
        }
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
        .ok_or_else(|| io::Error::other("Git UI revision space is exhausted"))?;
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
                        "Git changed from revision {} to {}; retry the action",
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
    if action.kind == UiEventKind::Activate
        && action.value == UiEventValue::None
        && let Some(remote_action) = [RemoteAction::Fetch, RemoteAction::Pull, RemoteAction::Push]
            .into_iter()
            .find(|operation| remote_action_id(*operation) == action.action.as_str())
    {
        if !git_toolbar(app).action(action.node_id.as_str(), action.action.as_str()) {
            return Err("This Git action is unavailable".into());
        }
        return app
            .start_remote_action(remote_action)
            .map_err(|e| e.to_string());
    }
    match (
        action.node_id.as_str(),
        action.action.as_str(),
        action.kind,
        &action.value,
    ) {
        ("changes-tab", "show-changes", UiEventKind::Activate, UiEventValue::None) => {
            app.switch_tab(Tab::Changes).map_err(|e| e.to_string())
        }
        ("history-tab", "show-history", UiEventKind::Activate, UiEventValue::None) => {
            app.switch_tab(Tab::History).map_err(|e| e.to_string())
        }
        ("older-commits", "load-history", UiEventKind::Activate, UiEventValue::None)
            if matches!(app.screen, Screen::History) && app.has_more_history =>
        {
            app.load_more_history().map_err(|e| e.to_string())
        }
        ("git-history", "select-commit", UiEventKind::Change, UiEventValue::Text(id))
            if matches!(app.screen, Screen::History) =>
        {
            let index = commit_index(app, id)?;
            app.select(index);
            Ok(())
        }
        (id, "open-commit", UiEventKind::Activate, UiEventValue::None)
            if matches!(app.screen, Screen::History) =>
        {
            let index = commit_index(app, id)?;
            app.select(index);
            app.open_selected().map_err(|e| e.to_string())
        }
        (FILE_LIST_ID, SELECT_FILE_ACTION, UiEventKind::Change, UiEventValue::Text(item_id))
            if matches!(app.screen, Screen::Files | Screen::CommitFiles) =>
        {
            let index = file_index_from_node_id(item_id)
                .ok_or_else(|| "Selected file has an invalid target".to_owned())?;
            if index >= app.active_files().len() {
                return Err("Selected file no longer exists".to_owned());
            }
            app.select(index);
            Ok(())
        }
        (node_id, OPEN_FILE_ACTION, UiEventKind::Activate, UiEventValue::None)
            if matches!(app.screen, Screen::Files | Screen::CommitFiles) =>
        {
            let index = file_index_from_node_id(node_id)
                .ok_or_else(|| "File action has an invalid target".to_owned())?;
            if index >= app.active_files().len() {
                return Err("File no longer exists".to_owned());
            }
            app.select(index);
            app.open_selected().map_err(|error| error.to_string())
        }
        (SEMANTIC_ROOT_ID, CLOSE_DIFF_ACTION, UiEventKind::Cancel, UiEventValue::None)
            if app.can_go_back() =>
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
        _ => Err("Action is not declared by the current Git Page".to_owned()),
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
        if !matches!(app.screen, Screen::Files | Screen::CommitFiles) {
            return Err("File list is not open".into());
        }
        if index >= app.active_files().len() {
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

fn commit_index(app: &App, node_id: &str) -> Result<usize, String> {
    let id = node_id
        .strip_prefix("commit-")
        .ok_or("Invalid commit target")?;
    app.history
        .iter()
        .position(|commit| commit.id == id)
        .ok_or_else(|| "Commit no longer exists in this history".into())
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
    ChangesTab,
    HistoryTab,
    NextTab,
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
        KeyCode::Char('1') if !control => Some(InputAction::ChangesTab),
        KeyCode::Char('2') if !control => Some(InputAction::HistoryTab),
        KeyCode::Tab | KeyCode::BackTab => Some(InputAction::NextTab),
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
        InputAction::ChangesTab | InputAction::HistoryTab | InputAction::NextTab => {
            let tab = match action {
                InputAction::ChangesTab => Tab::Changes,
                InputAction::HistoryTab => Tab::History,
                _ => {
                    if app.tab == Tab::Changes {
                        Tab::History
                    } else {
                        Tab::Changes
                    }
                }
            };
            if let Err(error) = app.switch_tab(tab) {
                app.fail(error);
            }
        }
        InputAction::Activate => {
            if let Err(error) = app.open_selected() {
                app.fail(error);
            }
        }
        InputAction::Back => app.back(),
        InputAction::Down if app.is_detail() => {
            app.scroll_vertical(1);
        }
        InputAction::Down => app.move_selection(1),
        InputAction::Up if app.is_detail() => {
            app.scroll_vertical(-1);
        }
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
        (Screen::Diff(document), Some(range)) => Some({
            let reference = selection_reference(document, range);
            app.commit
                .as_ref()
                .map(|commit| format!("{}:{reference}", commit.id))
                .unwrap_or(reference)
        }),
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
        toolbar: &mut PageToolbarState,
        theme: KitTheme,
    ) -> io::Result<RenderResult> {
        let mut result = RenderResult::default();
        drags.begin_frame();
        self.terminal.draw(|frame| {
            result = render_component_frame(frame, node, app, drags, menu, theme);
            if let UiComponent::Page(page) = &node.element {
                toolbar.render(frame, page.toolbar.as_ref());
                if toolbar.is_open() {
                    drags.begin_frame();
                }
            }
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
    tabs_area: Option<Rect>,
    title_area: Rect,
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
    list_state.set_spinner_frame(
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            / 100) as usize,
    );
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
        (_, unpeel_app_kit::PageBodySlot::List(list)) => {
            let hits = (list_state.offset()..list.items.len())
                .map_while(|index| list_state.item_area(index).map(|area| (index, area)))
                .map(|(index, row_area)| {
                    if app.tab == Tab::Changes
                        && let Some(file) = app.active_files().get(index)
                    {
                        drags.register(row_area, app.root().join(file.path()));
                    }
                    RowHit {
                        index,
                        area: row_area,
                    }
                })
                .collect();
            RenderResult {
                hits,
                scroll_offset: list_state.offset(),
                max_scroll: list_state.max_offset(list.items.len()),
                viewport_rows: list_state.visible_item_count(list.items.len()),
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
    result.back_button = page
        .back
        .as_ref()
        .and_then(|_| RectHit::from_rect(layout.title));
    result.tabs_area = layout.tabs;
    result.title_area = layout.title;
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
            .map(|(index, file)| file_list_item(file, index, false))
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

fn file_list_item(file: &ChangedFile, index: usize, historical: bool) -> ListItem {
    let status = file.status_symbol();
    let symbol = match status {
        'A' | '?' | 'C' => "⊞",
        'D' => "⊟",
        'R' => "→",
        'U' => "!",
        _ => "⊡",
    };
    let label = if historical {
        change_label(status).to_owned()
    } else {
        format!("{}, {}", change_label(status), file.state_label())
    };
    ListItem::new(file_node_id(index), file.list_name())
        .trailing(ListItemSlot::status(
            StatusSymbol::new(symbol, label).tone(status_tone(status)),
        ))
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

#[cfg(test)]
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
fn session_title_for(repository: &crate::git::Repository, branch: Option<&str>) -> String {
    let folder = repository
        .root()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| repository.root().display().to_string());
    match branch {
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

    fn wheel(kind: MouseEventKind) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column: 10,
            row: 8,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn scroll_bursts_clamp_each_tick_and_reverse_immediately_at_both_edges() {
        let (_directory, mut app) = file_app();
        app.apply_render_metrics(3, 3, 2, 0);
        let down = wheel(MouseEventKind::ScrollDown);
        let up = wheel(MouseEventKind::ScrollUp);
        let mut events = std::iter::repeat_n(down.clone(), 200).chain([up.clone()]);
        let result = scroll_batch(&mut app, down.clone(), || Ok(events.next())).unwrap();
        assert!(result.changed);
        assert!(result.pending.is_none());
        assert_eq!(
            app.list_scroll, 2,
            "one upward tick must move immediately after overscrolling"
        );
        assert!(!app.reveal_selected);

        app.apply_render_metrics(0, 3, 2, 0);
        let mut events = std::iter::repeat_n(up.clone(), 200).chain([down.clone()]);
        scroll_batch(&mut app, up, || Ok(events.next())).unwrap();
        assert_eq!(
            app.list_scroll, 1,
            "one downward tick must move immediately at the top"
        );

        app.screen = Screen::Diff(document());
        app.apply_render_metrics(3, 3, 1, 0);
        let mut events =
            std::iter::repeat_n(down.clone(), 200).chain([wheel(MouseEventKind::ScrollUp)]);
        scroll_batch(&mut app, down, || Ok(events.next())).unwrap();
        assert_eq!(app.detail_scroll, 2);
    }

    #[test]
    fn scroll_batches_preserve_following_input_and_bound_work_without_losing_ticks() {
        let (_directory, mut app) = file_app();
        app.apply_render_metrics(0, 1000, 2, 0);
        let down = wheel(MouseEventKind::ScrollDown);
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let mut queued = std::iter::repeat_n(down.clone(), 300).chain([enter.clone()]);
        let first = scroll_batch(&mut app, down, || Ok(queued.next())).unwrap();
        assert!(first.pending.is_none());
        assert_eq!(app.list_scroll, 256);
        let next = queued.next().unwrap();
        let second = scroll_batch(&mut app, next, || Ok(queued.next())).unwrap();
        assert_eq!(app.list_scroll, 301);
        assert_eq!(second.pending, Some(enter));
        assert!(queued.next().is_none());
    }

    #[test]
    fn live_diff_projection_keeps_syntax_tints_tabs_and_cached_tokens() {
        let (directory, mut app) = file_app();
        let source = "fn greet() {\n\tlet name = \"Hello\"; // comment\n}\n";
        std::fs::write(directory.path().join("a.rs"), source).unwrap();
        app.open_selected().unwrap();
        let cached = app.syntax.as_ref().unwrap().as_ptr();
        let node = semantic_node(&app, false);
        let UiComponent::Page(page) = &node.element else {
            unreachable!()
        };
        let content = page.content().unwrap();
        let line = content
            .lines
            .iter()
            .find(|line| line.text().contains("let name"))
            .unwrap();
        assert_eq!(line.text(), "+   let name = \"Hello\"; // comment");
        assert!(line.runs.iter().any(|run| run.tone == ContentTone::Success));
        assert!(line.runs.iter().any(|run| run.tone == ContentTone::Accent));
        assert!(line.runs.iter().any(|run| run.tone == ContentTone::Muted));
        for theme in [KitTheme::dark(), KitTheme::light()] {
            let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
            let mut drags = DragSurface::disabled();
            terminal
                .draw(|frame| {
                    render_component_frame(frame, &node, &app, &mut drags, None, theme);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let row = (0..20)
                .find(|row| buffer_line(buffer, *row).contains("let name"))
                .unwrap();
            let text = buffer_line(buffer, row);
            let keyword = text.find("let").unwrap() as u16;
            let identifier = text.find("name").unwrap() as u16;
            assert_ne!(buffer[(keyword, row)].fg, buffer[(identifier, row)].fg);
            assert!(
                (0..70).all(|x| buffer[(x, row)].bg
                    == ContentTheme::for_theme(theme).added_line.bg.unwrap())
            );
        }
        app.detail_scroll = 1;
        assert!(!app.sync().unwrap());
        assert_eq!(cached, app.syntax.as_ref().unwrap().as_ptr());
        std::fs::write(directory.path().join("a.rs"), "fn updated() {}\n").unwrap();
        assert!(app.sync().unwrap());
        assert!(
            semantic_page(&app, false)
                .content()
                .unwrap()
                .lines
                .iter()
                .any(|line| line.text().contains("updated"))
        );
    }

    #[test]
    fn scrolling_without_a_model_change_reuses_the_semantic_page() {
        let (_directory, mut app) = file_app();
        app.apply_render_metrics(0, 3, 2, 0);
        let before = semantic_node(&app, false);
        assert!(app.scroll_vertical(1));
        assert_eq!(semantic_node(&app, false), before);
        app.apply_render_metrics(3, 3, 2, 0);
        assert!(
            !app.scroll_vertical(1),
            "scrolling beyond the edge must not request another frame"
        );
        assert!(app.scroll_vertical(-1));
        assert_eq!(app.list_scroll, 2);
    }

    #[test]
    fn tabs_render_and_route_history_through_the_same_component_actions() {
        let (directory, mut app) = file_app();
        for args in [vec!["add", "."], vec!["commit", "-m", "Initial files"]] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(directory.path())
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let agent = AgentBridge::new();
        apply_semantic_action(
            &mut app,
            &agent,
            &UiAction::activate("history-tab", "show-history"),
        )
        .unwrap();
        let node = semantic_node(&app, false);
        node.validate().unwrap();
        let UiComponent::Page(page) = &node.element else {
            unreachable!()
        };
        assert!(page.tabs[1].selected);
        assert!(page.required_capabilities().contains(&"pageTabs"));
        let action = page.list().items[0].primary_ui_action().unwrap();
        for (width, height) in [(1, 1), (16, 4), (32, 10), (80, 20)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut drags = DragSurface::disabled();
            let mut rendered = RenderResult::default();
            terminal
                .draw(|frame| {
                    rendered = render_component_frame(
                        frame,
                        &node,
                        &app,
                        &mut drags,
                        None,
                        KitTheme::dark(),
                    )
                })
                .unwrap();
            assert_eq!(
                rendered.title_area,
                page.layout(Rect::new(0, 0, width, height)).title
            );
            if width >= 32 {
                assert!(buffer_line(terminal.backend().buffer(), 0).contains("Changes"));
                assert!(buffer_line(terminal.backend().buffer(), 0).contains("History"));
                let tabs = rendered.tabs_area.unwrap();
                assert_eq!(
                    page.tab_at(Position::new(width - 1, 0), tabs)
                        .unwrap()
                        .action,
                    "show-history"
                );
                assert_eq!(rendered.hits.len(), 1);
            }
            assert!(drags.regions().is_empty());
        }
        apply_semantic_action(&mut app, &agent, &action).unwrap();
        assert!(matches!(app.screen, Screen::CommitFiles));
        let files = semantic_page(&app, false);
        assert!(
            matches!(&files.list().items[0].trailing, Some(ListItemSlot::Status(status)) if status.label == "added")
        );
        apply_semantic_action(
            &mut app,
            &agent,
            &files.list().items[0].primary_ui_action().unwrap(),
        )
        .unwrap();
        assert!(app.is_detail());
        apply_semantic_action(
            &mut app,
            &agent,
            &UiAction::new(
                SEMANTIC_ROOT_ID,
                CLOSE_DIFF_ACTION,
                UiEventKind::Cancel,
                UiEventValue::None,
            ),
        )
        .unwrap();
        assert!(matches!(app.screen, Screen::CommitFiles));
        apply_semantic_action(
            &mut app,
            &agent,
            &UiAction::new(
                SEMANTIC_ROOT_ID,
                CLOSE_DIFF_ACTION,
                UiEventKind::Cancel,
                UiEventValue::None,
            ),
        )
        .unwrap();
        assert!(matches!(app.screen, Screen::History));
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
            page.list().items[0].trailing,
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
        assert!(buffer_line(buffer, 0).starts_with("  ui.rs"));
        assert!(buffer_line(buffer, 0).ends_with("⊡ "));
        assert!(!buffer_line(buffer, 0).contains("src/ui.rs"));
        assert_eq!(buffer[(47, 0)].bg, theme.selected_row.bg.unwrap());
        assert_eq!(list_result.hits[0].area.width, 48);
        assert_eq!(drags.regions().len(), 1);
        assert_eq!(drags.regions()[0].area, Rect::new(0, 0, 48, 1));
        assert_eq!(drags.regions()[0].path, Path::new("/repo/src/ui.rs"));
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
        assert_eq!(
            code_cell.fg, theme.text,
            "plain identifiers keep the shared text color"
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
