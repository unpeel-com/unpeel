//! Borderless changed-file list and unified-diff detail surface.

use std::io::{self, Stdout};
use std::time::Duration;

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
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use ratatui::{Frame, Terminal};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use unpeel_tui_kit::{ColorScheme, KitTheme, SELECTABLE_LEFT_PADDING, VerticalScrollbar};

use crate::app::{App, Screen};
use crate::git::{ChangedFile, DiffDocument};
use crate::unpeel::ContextReporter;

const HEADER_ROWS: u16 = 2;
const FOOTER_ROWS: u16 = 1;
const DETAIL_GAP_ROWS: u16 = 1;
const DETAIL_META_ROWS: u16 = 1;

pub fn run(mut app: App) -> io::Result<()> {
    let theme = KitTheme::detected();
    let mut terminal = TerminalGuard::enter()?;
    let mut reporter = ContextReporter::detect();
    let mut rendered = RenderResult::default();
    let mut needs_draw = true;

    loop {
        if needs_draw {
            reporter.publish(&app);
            rendered = terminal.draw(&app, theme)?;
            app.apply_render_metrics(
                rendered.scroll_offset,
                rendered.max_scroll,
                rendered.viewport_rows,
                rendered.max_horizontal_scroll,
            );
            needs_draw = false;
        }

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let Some(action) = action_for_key(key, app.is_detail()) else {
                    continue;
                };
                if handle_action(&mut app, action) {
                    break;
                }
                needs_draw = true;
            }
            Event::Mouse(mouse) => {
                let position = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if app.is_detail() {
                            if rendered
                                .back_button
                                .is_some_and(|hit| hit.contains(position))
                            {
                                app.back();
                                needs_draw = true;
                            }
                        } else if let Some(hit) =
                            rendered.hits.iter().find(|hit| hit.contains(position))
                        {
                            app.select(hit.index);
                            needs_draw = true;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        app.scroll_vertical(-3);
                        needs_draw = true;
                    }
                    MouseEventKind::ScrollDown => {
                        app.scroll_vertical(3);
                        needs_draw = true;
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
}

fn action_for_key(key: KeyEvent, detail: bool) -> Option<InputAction> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    if control && key.code == KeyCode::Char('c') {
        return Some(InputAction::Quit);
    }
    match key.code {
        KeyCode::Char('q') if !control => Some(InputAction::Quit),
        KeyCode::Enter if detail => Some(InputAction::Back),
        KeyCode::Enter => Some(InputAction::Activate),
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
    }
    false
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

    fn draw(&mut self, app: &App, theme: KitTheme) -> io::Result<RenderResult> {
        let mut result = RenderResult::default();
        self.terminal.draw(|frame| {
            result = render_frame(frame, app, theme);
        })?;
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
    back_button: Option<RectHit>,
    scroll_offset: usize,
    max_scroll: usize,
    viewport_rows: usize,
    max_horizontal_scroll: usize,
}

fn render_frame(frame: &mut Frame<'_>, app: &App, theme: KitTheme) -> RenderResult {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(HEADER_ROWS),
        Constraint::Min(0),
        Constraint::Length(FOOTER_ROWS),
    ])
    .areas(frame.area());

    render_header(frame, header, app, theme);
    let result = match &app.screen {
        Screen::Files => render_file_list(
            frame,
            body,
            &app.files,
            app.selected,
            app.list_scroll,
            app.reveal_selected,
            theme,
        ),
        Screen::Diff(document) => render_diff_detail(
            frame,
            body,
            document,
            app.detail_scroll,
            app.horizontal_scroll,
            theme,
        ),
    };
    render_footer(frame, footer, app, theme);
    result
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App, theme: KitTheme) {
    let summary = match &app.screen {
        Screen::Files => format!("{} changed ", app.files.len()),
        Screen::Diff(document) => {
            format!("+{} −{} ", document.additions, document.deletions)
        }
    };
    render_header_surface(frame, area, &summary, theme);
}

fn render_header_surface(frame: &mut Frame<'_>, area: Rect, summary: &str, theme: KitTheme) {
    if area.is_empty() {
        return;
    }
    frame.render_widget(
        Block::new()
            .borders(Borders::BOTTOM)
            .border_style(Style::new().fg(theme.scrollbar_track.fg.unwrap_or(theme.subtle))),
        area,
    );

    let content = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let summary_width = u16::try_from(Line::from(summary).width())
        .unwrap_or(u16::MAX)
        .min(content.width.saturating_sub(8));
    let [title_area, summary_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(summary_width)]).areas(content);
    let padding = SELECTABLE_LEFT_PADDING.min(title_area.width);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "DIFFS",
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Rect::new(
            title_area.x.saturating_add(padding),
            title_area.y,
            title_area.width.saturating_sub(padding),
            title_area.height,
        ),
    );
    frame.render_widget(
        Paragraph::new(summary)
            .style(Style::new().fg(theme.muted))
            .alignment(Alignment::Right),
        summary_area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &App, theme: KitTheme) {
    if area.is_empty() {
        return;
    }
    let (text, color) = app.notice.as_ref().map_or_else(
        || {
            if app.is_detail() {
                (
                    "esc/enter back · ↑↓ scroll · ←→ pan · r refresh · q quit".to_owned(),
                    theme.muted,
                )
            } else {
                (
                    "↑↓ select · enter diff · r refresh · q quit".to_owned(),
                    theme.muted,
                )
            }
        },
        |notice| {
            (
                notice.text.clone(),
                if notice.error {
                    theme.danger
                } else {
                    theme.muted
                },
            )
        },
    );
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
    files: &[ChangedFile],
    selected: usize,
    requested_scroll: usize,
    reveal_selected: bool,
    theme: KitTheme,
) -> RenderResult {
    if area.is_empty() {
        return RenderResult::default();
    }
    if files.is_empty() {
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

    let total_rows = files.len();
    let show_scrollbar = total_rows > usize::from(area.height) && area.width > 1;
    let rows_area = if show_scrollbar {
        Rect::new(area.x, area.y, area.width - 1, area.height)
    } else {
        area
    };
    let viewport_rows = usize::from(rows_area.height);
    let max_scroll = total_rows.saturating_sub(viewport_rows);
    let selected = selected.min(files.len() - 1);
    let requested_scroll = requested_scroll.min(max_scroll);
    let scroll_offset = if reveal_selected {
        reveal_selected_row(selected, viewport_rows, requested_scroll, max_scroll)
    } else {
        requested_scroll
    };

    let mut hits = Vec::new();
    for row in 0..rows_area.height {
        let index = scroll_offset.saturating_add(usize::from(row));
        let Some(file) = files.get(index) else {
            break;
        };
        let row_area = Rect::new(rows_area.x, rows_area.y + row, rows_area.width, 1);
        render_file_row(frame.buffer_mut(), row_area, file, index == selected, theme);
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
        Constraint::Length(if show_state { state_width } else { 0 }),
    ])
    .areas(content);
    let status_style = Style::new().fg(status_color(file.status_symbol(), theme.scheme));
    frame_line(
        buffer,
        label_area,
        Line::from(vec![
            Span::styled(file.status_symbol().to_string(), status_style),
            Span::raw("  "),
            Span::styled(file.display_path(), row_style),
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
    requested_scroll: usize,
    horizontal_scroll: usize,
    theme: KitTheme,
) -> RenderResult {
    if area.is_empty() {
        return RenderResult::default();
    }

    let back_area = Rect::new(area.x, area.y, area.width, 1);
    frame.buffer_mut().set_style(back_area, theme.selected_row);
    let back_padding = SELECTABLE_LEFT_PADDING.min(back_area.width);
    frame.render_widget(
        Paragraph::new("← Back").style(theme.selected_row.add_modifier(Modifier::BOLD)),
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
                let expanded = expand_tabs(line);
                let visible = visible_cells(&expanded, horizontal_scroll, content_area.width);
                frame.render_widget(
                    Paragraph::new(visible).style(diff_line_style(line, theme)),
                    Rect::new(content_area.x, content_area.y + row, content_area.width, 1),
                );
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
        Constraint::Length(if show_summary { summary_width } else { 0 }),
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

fn diff_line_style(line: &str, theme: KitTheme) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        Style::new().fg(theme.muted).add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::new().fg(status_color('A', theme.scheme))
    } else if line.starts_with('-') {
        Style::new().fg(status_color('D', theme.scheme))
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

    #[test]
    fn escape_is_back_in_detail_and_does_not_exit_the_list() {
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(action_for_key(escape, true), Some(InputAction::Back));
        assert_eq!(action_for_key(escape, false), None);
    }

    #[test]
    fn title_and_file_labels_use_the_shared_two_cell_inset() {
        let theme = KitTheme::dark();
        let files = vec![ChangedFile::fixture("src/ui.rs", ' ', 'M')];
        let mut terminal = Terminal::new(TestBackend::new(48, 8)).unwrap();
        let mut list_result = RenderResult::default();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let [header, body] =
                    Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
                render_header_surface(frame, header, "1 changed ", theme);
                list_result = render_file_list(frame, body, &files, 0, 0, true, theme);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer_line(buffer, 0).starts_with("  DIFFS"));
        assert!(buffer_line(buffer, 2).starts_with("  M  src/ui.rs"));
        assert_eq!(buffer[(47, 2)].bg, theme.selected_row.bg.unwrap());
        assert_eq!(list_result.hits[0].area.width, 48);
    }

    #[test]
    fn detail_has_a_full_width_back_row_and_colored_patch_lines() {
        let theme = KitTheme::light();
        let document = DiffDocument {
            file: ChangedFile::fixture("src/ui.rs", ' ', 'M'),
            lines: vec![
                "diff --git a/src/ui.rs b/src/ui.rs".into(),
                "@@ -1 +1 @@".into(),
                "-old".into(),
                "+new".into(),
            ],
            additions: 1,
            deletions: 1,
        };
        let mut terminal = Terminal::new(TestBackend::new(44, 12)).unwrap();
        let mut result = RenderResult::default();
        terminal
            .draw(|frame| {
                result = render_diff_detail(frame, frame.area(), &document, 0, 0, theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert!(buffer_line(buffer, 0).starts_with("  ← Back"));
        assert_eq!(buffer[(43, 0)].bg, theme.selected_row.bg.unwrap());
        assert_eq!(buffer[(2, 6)].fg, Color::Red);
        assert_eq!(buffer[(2, 7)].fg, Color::Green);
        assert!(result.back_button.is_some());
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
        terminal
            .draw(|frame| {
                let _ = render_file_list(frame, frame.area(), &files, 0, 0, true, theme);
                let _ = render_diff_detail(frame, frame.area(), &document, 0, 0, theme);
            })
            .unwrap();
    }
}
