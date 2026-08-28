//! Borderless changed-file list and unified-diff detail surface.

use std::io::{self, Stdout, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Frame, Terminal};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use unpeel_app_kit::{
    AgentBridge, ColorScheme, DoubleClickTracker, DragSurface, EditorBridge,
    KeyboardEnhancementGuard, KitTheme, MenuItem, MenuTheme, PopupMenu, SELECTABLE_LEFT_PADDING,
    VerticalScrollbar, clipboard_sequence, display_path_from_root,
};

use crate::app::{App, Screen};
use crate::git::{ChangedFile, DiffDocument};
use crate::unpeel::ContextReporter;

const FOOTER_ROWS: u16 = 1;
const DETAIL_GAP_ROWS: u16 = 1;
const DETAIL_META_ROWS: u16 = 1;
const AUTO_SYNC_INTERVAL: Duration = Duration::from_millis(1000);

pub fn run(mut app: App) -> io::Result<()> {
    let theme = KitTheme::detected();
    let mut terminal = TerminalGuard::enter()?;
    let mut drags = DragSurface::detect();
    let _keyboard = KeyboardEnhancementGuard::enter()?;
    let mut reporter = ContextReporter::detect();
    let agent = AgentBridge::new();
    agent.refresh();
    let mut rendered = RenderResult::default();
    let mut clicks = DoubleClickTracker::new();
    let mut menu: Option<ContextMenu> = None;
    let mut selecting = false;
    let mut needs_draw = true;
    let mut last_sync = Instant::now();

    loop {
        if needs_draw {
            reporter.publish(&app);
            rendered = terminal.draw(&app, &mut drags, menu.as_mut(), theme)?;
            app.apply_render_metrics(
                rendered.scroll_offset,
                rendered.max_scroll,
                rendered.viewport_rows,
                rendered.max_horizontal_scroll,
            );
            needs_draw = false;
        }

        if !event::poll(Duration::from_millis(250))? {
            drags.heartbeat()?;
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
                clicks.reset();
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
                let position = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(mut open_menu) = menu.take() {
                            clicks.reset();
                            if open_menu
                                .item_at(position)
                                .is_some_and(MenuItem::is_enabled)
                            {
                                open_menu.select_at(position);
                                activate_menu(open_menu, &mut app, &agent);
                            }
                            needs_draw = true;
                        } else if app.is_detail() {
                            clicks.reset();
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
                        } else if let Some((index, activate)) =
                            list_click_at(&rendered, position, &mut clicks)
                        {
                            app.select(index);
                            if activate && let Err(error) = app.open_selected() {
                                app.fail(error);
                            }
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Right) => {
                        clicks.reset();
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
                                menu = Some(diff_menu(
                                    app.selected_absolute_path(),
                                    agent.label().is_some(),
                                    position,
                                    theme.scheme,
                                ));
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
                                menu = Some(list_menu(
                                    relative,
                                    absolute,
                                    agent.label().is_some(),
                                    position,
                                    theme.scheme,
                                ));
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
                        clicks.reset();
                        if let Some(open_menu) = menu.as_mut() {
                            open_menu.move_selection(-1);
                        } else {
                            app.scroll_vertical(-3);
                        }
                        needs_draw = true;
                    }
                    MouseEventKind::ScrollDown => {
                        clicks.reset();
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
                clicks.reset();
                app.reveal_selected = true;
                needs_draw = true;
            }
            _ => {}
        }
    }
    Ok(())
}

fn list_click_at(
    rendered: &RenderResult,
    position: Position,
    clicks: &mut DoubleClickTracker<usize>,
) -> Option<(usize, bool)> {
    let Some(hit) = rendered
        .hits
        .iter()
        .copied()
        .find(|hit| hit.contains(position))
    else {
        clicks.reset();
        return None;
    };
    Some((hit.index, clicks.click(hit.index)))
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
    Refresh,
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
    match key.code {
        KeyCode::Char('q') if !control => Some(InputAction::Quit),
        KeyCode::Enter if detail && has_selection => Some(InputAction::SendToAgent),
        KeyCode::Char('s') if detail && has_selection => Some(InputAction::SendToAgent),
        KeyCode::Enter if detail => Some(InputAction::Back),
        KeyCode::Enter => Some(InputAction::Activate),
        KeyCode::Esc if detail && has_selection => Some(InputAction::ClearSelection),
        KeyCode::Esc if detail => Some(InputAction::Back),
        KeyCode::Esc => None,
        KeyCode::Down | KeyCode::Char('j') => Some(InputAction::Down),
        KeyCode::Up | KeyCode::Char('k') => Some(InputAction::Up),
        KeyCode::Home | KeyCode::Char('g') => Some(InputAction::First),
        KeyCode::End | KeyCode::Char('G') => Some(InputAction::Last),
        KeyCode::PageDown => Some(InputAction::PageDown),
        KeyCode::PageUp => Some(InputAction::PageUp),
        KeyCode::Left | KeyCode::Char('h') if detail => Some(InputAction::PanLeft),
        KeyCode::Right | KeyCode::Char('l') if detail => Some(InputAction::PanRight),
        KeyCode::Char('r') => Some(InputAction::Refresh),
        _ => None,
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
        InputAction::Refresh => {
            if let Err(error) = app.refresh() {
                app.fail(error);
            }
        }
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
}

type ContextMenu = PopupMenu<ContextAction>;

fn diff_menu(
    absolute: Option<PathBuf>,
    can_send: bool,
    anchor: Position,
    scheme: ColorScheme,
) -> ContextMenu {
    let mut items = Vec::with_capacity(3);
    if let Some(path) = absolute {
        items.push(MenuItem::new(
            "Open in editor",
            ContextAction::OpenInEditor(path),
        ));
    }
    if can_send {
        items.push(MenuItem::new("Send to agent", ContextAction::SendSelection));
    }
    items.push(MenuItem::new("Copy lines", ContextAction::CopySelection));
    PopupMenu::new(anchor, items).with_theme(MenuTheme::for_color_scheme(scheme))
}

fn list_menu(
    relative: String,
    absolute: PathBuf,
    can_send: bool,
    anchor: Position,
    scheme: ColorScheme,
) -> ContextMenu {
    let mut items = Vec::with_capacity(3);
    items.push(MenuItem::new(
        "Open in editor",
        ContextAction::OpenInEditor(absolute.clone()),
    ));
    if can_send {
        items.push(MenuItem::new(
            "Send to agent",
            ContextAction::SendPath(relative),
        ));
    }
    items.push(MenuItem::new(
        "Copy path",
        ContextAction::CopyPath(absolute),
    ));
    PopupMenu::new(anchor, items).with_theme(MenuTheme::for_color_scheme(scheme))
}

fn activate_menu(menu: ContextMenu, app: &mut App, agent: &AgentBridge) {
    let Some(action) = menu.selected_value().cloned() else {
        return;
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
        app: &App,
        drags: &mut DragSurface,
        menu: Option<&mut ContextMenu>,
        theme: KitTheme,
    ) -> io::Result<RenderResult> {
        let mut result = RenderResult::default();
        drags.begin_frame();
        self.terminal.draw(|frame| {
            result = render_frame(frame, app, drags, menu, theme);
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
    scroll_offset: usize,
    max_scroll: usize,
    viewport_rows: usize,
    max_horizontal_scroll: usize,
}

#[derive(Clone, Copy, Debug)]
struct FileListView<'a> {
    root: &'a Path,
    files: &'a [ChangedFile],
    selected: usize,
    requested_scroll: usize,
    reveal_selected: bool,
}

fn render_frame(
    frame: &mut Frame<'_>,
    app: &App,
    drags: &mut DragSurface,
    menu: Option<&mut ContextMenu>,
    theme: KitTheme,
) -> RenderResult {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(FOOTER_ROWS)]).areas(frame.area());

    let result = match &app.screen {
        Screen::Files => render_file_list(
            frame,
            body,
            FileListView {
                root: app.root(),
                files: &app.files,
                selected: app.selected,
                requested_scroll: app.list_scroll,
                reveal_selected: app.reveal_selected,
            },
            theme,
            drags,
        ),
        Screen::Diff(document) => render_diff_detail(
            frame,
            body,
            document,
            app.selection_range(),
            app.detail_scroll,
            app.horizontal_scroll,
            theme,
        ),
    };
    render_footer(frame, footer, app, theme);
    if let Some(menu) = menu {
        // Do not let the native host begin a path drag through an open menu.
        drags.begin_frame();
        menu.render(frame);
    }
    result
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, theme: KitTheme) {
    if area.is_empty() {
        return;
    }
    let (text, color) = if let Some(notice) = &app.notice {
        (
            notice.text.clone(),
            if notice.error {
                theme.danger
            } else {
                theme.muted
            },
        )
    } else if let Some((start, end)) = app.selection_range().filter(|_| app.is_detail()) {
        let count = end - start + 1;
        (
            format!(
                "{count} diff line{} selected · Enter to send to agent · Esc to clear",
                if count == 1 { "" } else { "s" }
            ),
            theme.muted,
        )
    } else {
        let path = app
            .selected_absolute_path()
            .unwrap_or_else(|| app.root().to_path_buf());
        (display_path_from_root(path, app.root()), theme.muted)
    };
    let padding = SELECTABLE_LEFT_PADDING.min(area.width);
    frame.render_widget(
        Paragraph::new(text).style(Style::new().fg(color)),
        Rect::new(
            area.x.saturating_add(padding),
            area.y,
            area.width.saturating_sub(padding),
            area.height,
        ),
    );
}

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
        render_file_row(frame.buffer_mut(), row_area, file, index == selected, theme);
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

fn render_file_row(
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

fn render_diff_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    document: &DiffDocument,
    selection: Option<(usize, usize)>,
    requested_scroll: usize,
    horizontal_scroll: usize,
    theme: KitTheme,
) -> RenderResult {
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
                let expanded = expand_tabs(line);
                let visible = visible_cells(&expanded, horizontal_scroll, content_area.width);
                frame.render_widget(
                    Paragraph::new(visible).style(style),
                    Rect::new(content_area.x, content_area.y + row, content_area.width, 1),
                );
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

fn frame_line(buffer: &mut Buffer, area: Rect, line: Line<'_>) {
    Paragraph::new(line).render(area, buffer);
}

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

#[cfg(test)]
mod tests {
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
    fn double_clicking_the_same_file_row_requests_activation() {
        let rendered = RenderResult {
            hits: vec![RowHit {
                index: 2,
                area: Rect::new(0, 3, 40, 1),
            }],
            ..RenderResult::default()
        };
        let position = Position::new(12, 3);
        let mut clicks = DoubleClickTracker::new();

        assert_eq!(
            list_click_at(&rendered, position, &mut clicks),
            Some((2, false))
        );
        assert_eq!(
            list_click_at(&rendered, position, &mut clicks),
            Some((2, true))
        );
        assert_eq!(
            list_click_at(&rendered, Position::new(12, 4), &mut clicks),
            None
        );
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
    fn detail_is_transparent_except_for_changed_line_tints() {
        let theme = KitTheme::light();
        let document = document();
        let mut terminal = Terminal::new(TestBackend::new(44, 12)).unwrap();
        let mut result = RenderResult::default();
        terminal
            .draw(|frame| {
                result = render_diff_detail(frame, frame.area(), &document, None, 0, 0, theme);
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
                result =
                    render_diff_detail(frame, frame.area(), &document, Some((2, 3)), 0, 0, theme);
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
    fn context_menus_offer_agent_handoff_only_when_a_peer_exists() {
        let scheme = ColorScheme::Dark;
        let anchor = Position::new(4, 4);
        let detail_path = PathBuf::from("/repo/a.rs");
        let with_agent = diff_menu(Some(detail_path.clone()), true, anchor, scheme);
        assert_eq!(with_agent.items().len(), 3);
        assert_eq!(with_agent.items()[0].label(), "Open in editor");
        assert_eq!(
            with_agent.items()[0].value(),
            &ContextAction::OpenInEditor(detail_path)
        );
        assert_eq!(with_agent.items()[1].label(), "Send to agent");
        assert_eq!(with_agent.items()[1].value(), &ContextAction::SendSelection);

        let without_agent = diff_menu(None, false, anchor, scheme);
        assert_eq!(without_agent.items().len(), 1);
        assert_eq!(without_agent.items()[0].label(), "Copy lines");

        let list = list_menu(
            "a.rs".to_owned(),
            PathBuf::from("/repo/a.rs"),
            true,
            anchor,
            scheme,
        );
        assert_eq!(list.items().len(), 3);
        assert_eq!(
            list.items()[0].value(),
            &ContextAction::OpenInEditor(PathBuf::from("/repo/a.rs"))
        );
        assert_eq!(
            list.items()[1].value(),
            &ContextAction::SendPath("a.rs".to_owned())
        );
        assert_eq!(
            list.items()[2].value(),
            &ContextAction::CopyPath(PathBuf::from("/repo/a.rs"))
        );
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
                let _ =
                    render_diff_detail(frame, frame.area(), &document, Some((0, 0)), 0, 0, theme);
            })
            .unwrap();
    }
}
