//! Ratatui master/detail rendering.
//!
//! The default surface follows the shared Unpeel TUI list language: compact,
//! borderless, full-width selected rows with a two-cell content inset. Enter
//! opens one provider's detailed meters and history without changing that
//! selected-list vocabulary.

use crate::config::{AlertOption, Alerts};
use crate::sources::{Level, Metric, PercentDisplay, Provider, ProviderKind, Snapshot};
use crate::theme as ui;
use crate::timeparse::{compact_duration, now_epoch_secs};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Sparkline, Widget};
use ratatui::Frame;
use unpeel_tui_kit::{KitTheme, VerticalScrollbar, SELECTABLE_LEFT_PADDING};

const METRIC_GAP: u16 = 1;
const DETAIL_TOP_GAP: u16 = 1;

fn accent(palette: &ui::Palette, kind: ProviderKind) -> Color {
    match kind {
        ProviderKind::Codex => palette.codex_accent,
        ProviderKind::Claude => palette.claude_accent,
    }
}

fn level_color(palette: &ui::Palette, level: Level) -> Color {
    match level {
        Level::Ok => palette.meter_blue,
        Level::Warn => palette.warning,
        Level::Alert => palette.attention,
    }
}

pub struct View {
    pub selected: usize,
    pub detail_open: bool,
    pub scanning: bool,
    pub hosted: bool,
    pub alerts: Alerts,
    pub alert_dialog: Option<usize>,
    /// Requested top row in the active list or detail viewport.
    pub scroll_offset: u16,
    /// Bring the selected provider into view after keyboard navigation,
    /// refreshes and resize. Mouse/page scrolling disables this
    /// until selection changes again.
    pub reveal_selected: bool,
}

#[derive(Debug, Default)]
pub struct RenderResult {
    pub hits: Vec<Hit>,
    pub scroll_offset: u16,
    pub max_scroll: u16,
    pub viewport_height: u16,
    pub scrollbar_area: Option<Rect>,
    pub back_button: Option<Hit>,
    pub alert_button: Option<Hit>,
    pub alert_option_hits: Vec<Hit>,
    pub alert_dialog_area: Option<Rect>,
}

/// One selectable row's clickable screen rectangle, inclusive on every edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub left: u16,
    pub right: u16,
    pub top: u16,
    pub bottom: u16,
}

impl Hit {
    pub fn contains(&self, column: u16, row: u16) -> bool {
        (self.left..=self.right).contains(&column) && (self.top..=self.bottom).contains(&row)
    }

    fn from_rect(index: usize, area: Rect) -> Option<Self> {
        (!area.is_empty()).then_some(Self {
            index,
            left: area.x,
            right: area.right().saturating_sub(1),
            top: area.y,
            bottom: area.bottom().saturating_sub(1),
        })
    }
}

pub fn draw(
    frame: &mut Frame,
    snapshot: Option<&Snapshot>,
    view: &View,
    palette: &ui::Palette,
) -> RenderResult {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(frame, header, snapshot, view, palette);
    let mut result = match snapshot {
        None => {
            render_empty_state(frame, body, view.scanning, palette);
            RenderResult::default()
        }
        Some(snapshot) if snapshot.providers.is_empty() => {
            render_empty_state(frame, body, false, palette);
            RenderResult::default()
        }
        Some(snapshot) if view.detail_open => {
            let selected = view
                .selected
                .min(snapshot.providers.len().saturating_sub(1));
            render_provider_detail(frame, body, &snapshot.providers[selected], view, palette)
        }
        Some(snapshot) => render_provider_list(frame, body, &snapshot.providers, view, palette),
    };
    result.alert_button = render_footer(frame, footer, view.hosted, view.detail_open, palette);
    if let Some(selected) = view.alert_dialog.filter(|_| view.hosted) {
        let (area, hits) = render_alert_dialog(frame, selected, view.alerts, palette);
        result.alert_dialog_area = Some(area);
        result.alert_option_hits = hits;
    }
    result
}

fn render_header(
    frame: &mut Frame,
    area: Rect,
    snapshot: Option<&Snapshot>,
    view: &View,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }

    frame.render_widget(
        Block::new()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(palette.track)),
        area,
    );

    let content = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let summary = header_summary(snapshot, view);
    let summary_width = Line::from(summary.as_str())
        .width()
        .min(area.width.saturating_sub(6) as usize) as u16;
    let [title_area, summary_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(summary_width)]).areas(content);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " USAGE",
            Style::default()
                .fg(palette.header)
                .add_modifier(Modifier::BOLD),
        ))),
        title_area,
    );
    frame.render_widget(
        Paragraph::new(summary)
            .style(Style::default().fg(palette.muted))
            .alignment(Alignment::Right),
        summary_area,
    );
}

fn header_summary(snapshot: Option<&Snapshot>, view: &View) -> String {
    let mut parts = Vec::new();
    if let Some(total) = snapshot.and_then(Snapshot::day_total_usd) {
        if total > 0.0 {
            parts.push(format!("24h {} est", crate::sources::compact_usd(total)));
        }
    }
    if view.hosted {
        let enabled = view.alerts.enabled_count();
        parts.push(if enabled == 0 {
            "alerts off".into()
        } else {
            format!("alerts {enabled}/{}", AlertOption::ALL.len())
        });
    }
    if view.scanning {
        parts.push(ui::spinner_frame().into());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" · "))
    }
}

fn render_empty_state(frame: &mut Frame, area: Rect, scanning: bool, palette: &ui::Palette) {
    if area.is_empty() {
        return;
    }
    let message = if scanning {
        format!("{} scanning local usage…", ui::spinner_frame())
    } else {
        "no local usage data".into()
    };
    let row = Rect::new(
        area.x,
        area.y.saturating_add(area.height.saturating_sub(1) / 2),
        area.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(message)
            .style(Style::default().fg(palette.muted))
            .alignment(Alignment::Center),
        row,
    );
}

fn render_footer(
    frame: &mut Frame,
    area: Rect,
    hosted: bool,
    detail_open: bool,
    palette: &ui::Palette,
) -> Option<Hit> {
    if area.is_empty() {
        return None;
    }
    let hints: &[(&str, &str)] = if detail_open && area.width >= 68 {
        if hosted {
            &[
                ("esc/enter", "back"),
                ("j/k", "scroll"),
                ("r", "refresh"),
                ("a", "alerts"),
                ("t", "theme"),
                ("q", "quit"),
            ]
        } else {
            &[
                ("esc/enter", "back"),
                ("j/k", "scroll"),
                ("r", "refresh"),
                ("t", "theme"),
                ("q", "quit"),
            ]
        }
    } else if detail_open {
        &[("esc", "back"), ("j/k", "scroll"), ("q", "quit")]
    } else if area.width >= 68 {
        if hosted {
            &[
                ("j/k", "select"),
                ("enter", "details"),
                ("r", "refresh"),
                ("a", "alerts"),
                ("t", "theme"),
                ("q", "quit"),
            ]
        } else {
            &[
                ("j/k", "select"),
                ("enter", "details"),
                ("r", "refresh"),
                ("t", "theme"),
                ("q", "quit"),
            ]
        }
    } else {
        &[("j/k", "select"), ("enter", "details"), ("q", "quit")]
    };
    frame.render_widget(Paragraph::new(ui::hint_line(palette, hints)), area);

    let mut x = area.x.saturating_add(1);
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(3);
        }
        let width =
            u16::try_from(key.chars().count() + 1 + label.chars().count()).unwrap_or(u16::MAX);
        if *label == "alerts" {
            let visible = area.right().saturating_sub(x).min(width);
            return Hit::from_rect(0, Rect::new(x, area.y, visible, 1));
        }
        x = x.saturating_add(width);
    }
    None
}

fn centered_dialog(area: Rect) -> Rect {
    let width = area.width.min(62);
    let height = area.height.min(11);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn render_alert_dialog(
    frame: &mut Frame,
    selected: usize,
    alerts: Alerts,
    palette: &ui::Palette,
) -> (Rect, Vec<Hit>) {
    let area = centered_dialog(frame.area());
    if area.is_empty() {
        return (area, Vec::new());
    }
    frame.render_widget(Clear, area);
    let block = Block::new()
        .title(" Alerts ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette.focus));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return (area, Vec::new());
    }

    frame.render_widget(
        Paragraph::new("Unpeel notifications for this session")
            .style(Style::default().fg(palette.muted)),
        row_at(inner, inner.y),
    );

    let mut hits = Vec::new();
    for (index, option) in AlertOption::ALL.into_iter().enumerate() {
        let title_y = inner.y.saturating_add(2 + index as u16 * 2);
        if title_y >= inner.bottom() {
            break;
        }
        let active = selected.min(AlertOption::ALL.len() - 1) == index;
        let marker = if active { "▎" } else { " " };
        let checkbox = if alerts.enabled(option) { "[x]" } else { "[ ]" };
        let style = Style::default()
            .fg(if active {
                palette.header
            } else {
                palette.primary
            })
            .add_modifier(if active {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        frame.render_widget(
            Paragraph::new(format!("{marker} {checkbox} {}", option.label())).style(style),
            row_at(inner, title_y),
        );
        let description_y = title_y.saturating_add(1);
        if description_y < inner.bottom() {
            frame.render_widget(
                Paragraph::new(format!("      {}", option.description()))
                    .style(Style::default().fg(palette.muted)),
                row_at(inner, description_y),
            );
        }
        let height = 1 + u16::from(description_y < inner.bottom());
        if let Some(hit) = Hit::from_rect(index, Rect::new(inner.x, title_y, inner.width, height)) {
            hits.push(hit);
        }
    }

    if inner.height >= 1 {
        frame.render_widget(
            Paragraph::new("↑/↓ select · space toggle · esc close")
                .style(Style::default().fg(palette.muted))
                .alignment(Alignment::Center),
            row_at(inner, inner.bottom().saturating_sub(1)),
        );
    }
    (area, hits)
}

fn render_provider_list(
    frame: &mut Frame,
    area: Rect,
    providers: &[Provider],
    view: &View,
    palette: &ui::Palette,
) -> RenderResult {
    if area.is_empty() {
        return RenderResult::default();
    }

    let selected = view.selected.min(providers.len().saturating_sub(1));
    let total_height = u16::try_from(providers.len()).unwrap_or(u16::MAX);
    let show_scrollbar = total_height > area.height && area.width > 1;
    let rows_area = if show_scrollbar {
        Rect::new(area.x, area.y, area.width - 1, area.height)
    } else {
        area
    };
    if rows_area.is_empty() {
        return RenderResult {
            viewport_height: area.height,
            ..RenderResult::default()
        };
    }

    let max_scroll = total_height.saturating_sub(rows_area.height);
    let requested = view.scroll_offset.min(max_scroll);
    let scroll_offset = if view.reveal_selected {
        reveal_selected_row(selected, rows_area.height, requested, max_scroll)
    } else {
        requested
    };

    let mut hits = Vec::new();
    for row in 0..rows_area.height {
        let index = usize::from(scroll_offset.saturating_add(row));
        let Some(provider) = providers.get(index) else {
            break;
        };
        let row_area = Rect::new(rows_area.x, rows_area.y + row, rows_area.width, 1);
        render_provider_list_row(
            frame.buffer_mut(),
            row_area,
            provider,
            index == selected,
            palette,
        );
        if let Some(hit) = Hit::from_rect(index, row_area) {
            hits.push(hit);
        }
    }

    let scrollbar_area =
        show_scrollbar.then(|| Rect::new(area.right().saturating_sub(1), area.y, 1, area.height));
    if let Some(scrollbar_area) = scrollbar_area {
        render_scrollbar(
            frame,
            scrollbar_area,
            total_height,
            rows_area.height,
            scroll_offset,
            palette,
        );
    }

    RenderResult {
        hits,
        scroll_offset,
        max_scroll,
        viewport_height: rows_area.height,
        scrollbar_area,
        ..RenderResult::default()
    }
}

fn reveal_selected_row(
    selected: usize,
    viewport_height: u16,
    current: u16,
    max_scroll: u16,
) -> u16 {
    if viewport_height == 0 {
        return 0;
    }
    let selected = u16::try_from(selected).unwrap_or(u16::MAX);
    if selected < current {
        selected.min(max_scroll)
    } else if selected >= current.saturating_add(viewport_height) {
        selected
            .saturating_add(1)
            .saturating_sub(viewport_height)
            .min(max_scroll)
    } else {
        current.min(max_scroll)
    }
}

fn render_provider_list_row(
    buffer: &mut Buffer,
    area: Rect,
    provider: &Provider,
    selected: bool,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }

    let row_style = if selected {
        selected_row_style(palette)
    } else {
        Style::default()
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

    let (summary, level) = provider_basic_data(provider);
    let summary_width = if content.width >= 18 {
        u16::try_from(Line::from(summary.as_str()).width())
            .unwrap_or(u16::MAX)
            .min(content.width.saturating_sub(10))
    } else {
        0
    };
    let [title_area, summary_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(summary_width.saturating_add(u16::from(summary_width > 0))),
    ])
    .areas(content);

    let name_style = if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(if provider.alert.is_some() {
                palette.attention
            } else {
                palette.primary
            })
            .add_modifier(Modifier::BOLD)
    };
    let mut title = vec![Span::styled(
        display_provider_name(&provider.name),
        name_style,
    )];
    if !provider.badge.is_empty() {
        title.push(Span::raw(" "));
        title.push(Span::styled(
            display_badge(&provider.badge),
            if selected {
                Style::default().add_modifier(Modifier::DIM)
            } else {
                Style::default().fg(palette.muted)
            },
        ));
    }
    Paragraph::new(Line::from(title))
        .style(row_style)
        .render(title_area, buffer);

    if summary_width > 0 {
        let summary_style = if selected {
            Style::default().add_modifier(Modifier::DIM)
        } else if provider.alert.is_some() || level == Level::Alert {
            Style::default().fg(palette.attention)
        } else if level == Level::Warn {
            Style::default().fg(palette.warning)
        } else {
            Style::default().fg(palette.muted)
        };
        Paragraph::new(summary)
            .style(row_style.patch(summary_style))
            .alignment(Alignment::Right)
            .render(summary_area, buffer);
    }
}

fn provider_basic_data(provider: &Provider) -> (String, Level) {
    if !provider.present {
        return ("Not installed".into(), Level::Ok);
    }
    let Some(metric) = provider.metrics.first() else {
        return ("No recent activity".into(), Level::Ok);
    };
    let value = match (metric.percent, metric.percent_display) {
        (Some(used), PercentDisplay::Remaining) => {
            format!("{:.0}% left", (100.0 - used).clamp(0.0, 100.0))
        }
        (Some(used), PercentDisplay::Used) => format!("{used:.0}% used"),
        (None, _) => split_metric_value(&metric.value).0,
    };
    let label = display_metric_label(&metric.label);
    let summary = if value.trim().is_empty() {
        label
    } else {
        format!("{label} {value}")
    };
    (summary, metric.level)
}

fn metric_height(metric: &Metric) -> u16 {
    if metric.percent.is_some() {
        3
    } else if !metric.spark.is_empty() {
        if chart_is_inline(metric) {
            1
        } else {
            2
        }
    } else {
        1
    }
}

fn chart_is_inline(metric: &Metric) -> bool {
    metric.label.eq_ignore_ascii_case("Usage Trend")
}

fn render_provider_detail(
    frame: &mut Frame,
    area: Rect,
    provider: &Provider,
    view: &View,
    palette: &ui::Palette,
) -> RenderResult {
    if area.is_empty() {
        return RenderResult::default();
    }

    let back_area = Rect::new(area.x, area.y, area.width, 1);
    let back_style = selected_row_style(palette);
    frame.buffer_mut().set_style(back_area, back_style);
    let padding = SELECTABLE_LEFT_PADDING.min(back_area.width);
    frame.render_widget(
        Paragraph::new("← Back").style(back_style.add_modifier(Modifier::BOLD)),
        Rect::new(
            back_area.x.saturating_add(padding),
            back_area.y,
            back_area.width.saturating_sub(padding),
            1,
        ),
    );

    let viewport_y = area.y.saturating_add(1).saturating_add(DETAIL_TOP_GAP);
    let viewport_height = area.bottom().saturating_sub(viewport_y);
    let horizontal_padding = if area.width >= 5 {
        SELECTABLE_LEFT_PADDING
    } else if area.width >= 3 {
        1
    } else {
        0
    };
    let content_area = Rect::new(
        area.x.saturating_add(horizontal_padding),
        viewport_y,
        area.width
            .saturating_sub(horizontal_padding.saturating_mul(2)),
        viewport_height,
    );
    let total_height = detail_content_height(provider);
    let max_scroll = total_height.saturating_sub(content_area.height);
    let scroll_offset = view.scroll_offset.min(max_scroll);

    if !content_area.is_empty() {
        let virtual_area = Rect::new(0, 0, content_area.width, total_height.max(1));
        let mut virtual_buffer = Buffer::empty(virtual_area);
        render_detail_content(&mut virtual_buffer, virtual_area, provider, palette);
        let destination = frame.buffer_mut();
        for row in 0..content_area.height {
            let source_y = scroll_offset.saturating_add(row);
            if source_y >= total_height {
                break;
            }
            for column in 0..content_area.width {
                destination[(content_area.x + column, content_area.y + row)] =
                    virtual_buffer[(column, source_y)].clone();
            }
        }
    }

    let show_scrollbar = total_height > content_area.height && area.width > 1;
    let scrollbar_area = show_scrollbar.then(|| {
        Rect::new(
            area.right().saturating_sub(1),
            viewport_y,
            1,
            viewport_height,
        )
    });
    if let Some(scrollbar_area) = scrollbar_area {
        render_scrollbar(
            frame,
            scrollbar_area,
            total_height,
            content_area.height,
            scroll_offset,
            palette,
        );
    }

    RenderResult {
        scroll_offset,
        max_scroll,
        viewport_height: content_area.height,
        scrollbar_area,
        back_button: Hit::from_rect(view.selected, back_area),
        ..RenderResult::default()
    }
}

fn detail_content_height(provider: &Provider) -> u16 {
    let mut height = 1u16;
    height = height.saturating_add(u16::from(provider.alert.is_some()));
    height = height.saturating_add(METRIC_GAP);
    if !provider.present || provider.metrics.is_empty() {
        return height.saturating_add(1);
    }
    let metric_rows = provider
        .metrics
        .iter()
        .map(metric_height)
        .fold(0u16, u16::saturating_add)
        .saturating_add(METRIC_GAP.saturating_mul(
            u16::try_from(provider.metrics.len().saturating_sub(1)).unwrap_or(u16::MAX),
        ));
    height = height.saturating_add(metric_rows);
    let detail_rows = u16::try_from(provider.detail.len())
        .unwrap_or(u16::MAX)
        .saturating_add(u16::from(provider.as_of.is_some()));
    height.saturating_add(if detail_rows > 0 {
        METRIC_GAP.saturating_add(detail_rows)
    } else {
        0
    })
}

fn render_detail_content(
    buffer: &mut Buffer,
    area: Rect,
    provider: &Provider,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }

    render_detail_header(buffer, row_at(area, area.y), provider, palette);
    let mut y = area.y.saturating_add(1);
    if let Some(alert) = &provider.alert {
        Paragraph::new(format!("⚠ {alert}"))
            .style(Style::default().fg(palette.attention))
            .render(row_at(area, y), buffer);
        y = y.saturating_add(1);
    }
    y = y.saturating_add(METRIC_GAP);
    if !provider.present {
        render_muted_row(buffer, row_at(area, y), "not installed", palette);
        return;
    }
    if provider.metrics.is_empty() {
        render_muted_row(buffer, row_at(area, y), "no recent activity", palette);
        return;
    }
    for (index, metric) in provider.metrics.iter().enumerate() {
        if y >= area.bottom() {
            return;
        }
        let height = metric_height(metric).min(area.bottom().saturating_sub(y));
        render_metric(
            buffer,
            Rect::new(area.x, y, area.width, height),
            metric,
            palette,
        );
        y = y.saturating_add(metric_height(metric));
        if index + 1 < provider.metrics.len() {
            y = y.saturating_add(METRIC_GAP);
        }
    }
    let has_details = !provider.detail.is_empty() || provider.as_of.is_some();
    if has_details {
        y = y.saturating_add(METRIC_GAP);
    }
    for (key, value) in &provider.detail {
        if y >= area.bottom() {
            return;
        }
        render_key_value(buffer, row_at(area, y), key, value, palette);
        y = y.saturating_add(1);
    }
    if let Some(as_of) = provider.as_of {
        if y < area.bottom() {
            let age = compact_duration(now_epoch_secs() - as_of);
            render_key_value(
                buffer,
                row_at(area, y),
                "updated",
                &format!("{age} ago"),
                palette,
            );
        }
    }
}

fn render_detail_header(
    buffer: &mut Buffer,
    area: Rect,
    provider: &Provider,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }
    let mut title = vec![Span::styled(
        display_provider_name(&provider.name),
        Style::default()
            .fg(if provider.alert.is_some() {
                palette.attention
            } else {
                palette.primary
            })
            .add_modifier(Modifier::BOLD),
    )];
    if !provider.badge.is_empty() {
        title.push(Span::raw(" "));
        title.push(Span::styled(
            display_badge(&provider.badge),
            Style::default().fg(palette.muted),
        ));
    }
    let [title_area, status_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(u16::from(area.width >= 2).saturating_mul(2)),
    ])
    .areas(area);
    Paragraph::new(Line::from(title)).render(title_area, buffer);
    let status_color = if provider.alert.is_some() {
        palette.attention
    } else if provider.present && !provider.metrics.is_empty() {
        accent(palette, provider.kind)
    } else {
        palette.track
    };
    Paragraph::new(Span::styled("●", Style::default().fg(status_color)))
        .alignment(Alignment::Right)
        .render(status_area, buffer);
}

fn selected_row_style(palette: &ui::Palette) -> Style {
    match palette.mode {
        ui::ThemeMode::Dark => KitTheme::dark().selected_row,
        ui::ThemeMode::Light => KitTheme::light().selected_row,
        // Detection-free mode deliberately follows the terminal defaults.
        ui::ThemeMode::Adaptive => Style::default().add_modifier(Modifier::REVERSED),
    }
}

fn scrollbar_styles(palette: &ui::Palette) -> (Style, Style) {
    match palette.mode {
        ui::ThemeMode::Dark => {
            let kit = KitTheme::dark();
            (kit.scrollbar_track, kit.scrollbar_thumb)
        }
        ui::ThemeMode::Light => {
            let kit = KitTheme::light();
            (kit.scrollbar_track, kit.scrollbar_thumb)
        }
        ui::ThemeMode::Adaptive => (
            Style::default().fg(palette.track),
            Style::default().fg(palette.focus),
        ),
    }
}

fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    content_rows: u16,
    viewport_rows: u16,
    position: u16,
    palette: &ui::Palette,
) {
    let (track, thumb) = scrollbar_styles(palette);
    frame.render_widget(
        VerticalScrollbar::new(
            usize::from(content_rows),
            usize::from(viewport_rows),
            usize::from(position),
        )
        .track_style(track)
        .thumb_style(thumb),
        area,
    );
}

fn display_provider_name(name: &str) -> String {
    name.strip_prefix("Claude Code")
        .map(|suffix| format!("Claude{suffix}"))
        .unwrap_or_else(|| name.to_string())
}

fn display_badge(badge: &str) -> String {
    if badge.contains('@') {
        badge.to_string()
    } else {
        title_case(badge)
    }
}

fn row_at(area: Rect, y: u16) -> Rect {
    Rect::new(area.x, y, area.width, u16::from(y < area.bottom()))
}

fn render_muted_row(buffer: &mut Buffer, area: Rect, text: &str, palette: &ui::Palette) {
    Paragraph::new(text.to_string())
        .style(Style::default().fg(palette.muted))
        .render(area, buffer);
}

fn render_metric(buffer: &mut Buffer, area: Rect, metric: &Metric, palette: &ui::Palette) {
    if area.is_empty() {
        return;
    }

    if metric.percent.is_some() {
        render_bounded_metric(buffer, area, metric, palette);
    } else if !metric.spark.is_empty() {
        render_chart_metric(buffer, area, metric, palette);
    } else {
        render_unbounded_metric(buffer, area, metric, palette);
    }
}

fn render_bounded_metric(buffer: &mut Buffer, area: Rect, metric: &Metric, palette: &ui::Palette) {
    let annotation_color = if metric.annotation.is_some() && metric.level == Level::Alert {
        palette.attention
    } else {
        palette.muted
    };
    let (raw_value, context) = split_metric_value(&metric.value);
    let supporting_value = metric
        .annotation
        .as_deref()
        .or_else(|| (!raw_value.trim_end().ends_with('%')).then_some(raw_value.as_str()));
    render_split_row(
        buffer,
        row_at(area, area.y),
        &display_metric_label(&metric.label),
        supporting_value.unwrap_or_default(),
        Style::default()
            .fg(palette.primary)
            .add_modifier(Modifier::BOLD),
        Style::default().fg(annotation_color),
    );

    let Some(used_percent) = metric.percent else {
        return;
    };
    let ratio = match metric.percent_display {
        PercentDisplay::Remaining => 1.0 - (used_percent / 100.0).clamp(0.0, 1.0),
        PercentDisplay::Used => (used_percent / 100.0).clamp(0.0, 1.0),
    };
    if area.height >= 2 {
        UsageMeter {
            ratio,
            marker: metric.marker,
            level: metric.level,
            palette: *palette,
        }
        .render(row_at(area, area.y + 1), buffer);
    }
    if area.height >= 3 {
        let headline = match metric.percent_display {
            PercentDisplay::Remaining => {
                let remaining = (100.0 - used_percent).clamp(0.0, 100.0);
                format!("{remaining:.0}% left")
            }
            PercentDisplay::Used => format!("{used_percent:.0}% used"),
        };
        render_split_row(
            buffer,
            row_at(area, area.y + 2),
            &headline,
            context.as_deref().unwrap_or_default(),
            Style::default().fg(palette.primary),
            Style::default().fg(palette.muted),
        );
    }
}

fn render_chart_metric(buffer: &mut Buffer, area: Rect, metric: &Metric, palette: &ui::Palette) {
    let data: Vec<u64> = metric
        .spark
        .iter()
        .map(|value| (value.max(0.0) * 1_000.0).round() as u64)
        .collect();
    if chart_is_inline(metric) {
        render_label_value_row(buffer, row_at(area, area.y), metric, palette);
        let chart_width = u16::try_from(data.len())
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(18));
        if chart_width > 0 {
            let chart_area = Rect::new(
                area.right().saturating_sub(chart_width),
                area.y,
                chart_width,
                1,
            );
            let start = data.len().saturating_sub(chart_width as usize);
            Sparkline::default()
                .data(&data[start..])
                .style(Style::default().fg(level_color(palette, metric.level)))
                .render(chart_area, buffer);
        }
    } else {
        render_label_value_row(buffer, row_at(area, area.y), metric, palette);
        if area.height >= 2 {
            let start = data.len().saturating_sub(area.width as usize);
            Sparkline::default()
                .data(&data[start..])
                .style(Style::default().fg(level_color(palette, metric.level)))
                .render(row_at(area, area.y + 1), buffer);
        }
    }
}

fn render_unbounded_metric(
    buffer: &mut Buffer,
    area: Rect,
    metric: &Metric,
    palette: &ui::Palette,
) {
    render_label_value_row(buffer, row_at(area, area.y), metric, palette);
}

fn render_label_value_row(buffer: &mut Buffer, area: Rect, metric: &Metric, palette: &ui::Palette) {
    let value_color = if metric.level == Level::Ok {
        palette.muted
    } else {
        level_color(palette, metric.level)
    };
    render_split_row(
        buffer,
        area,
        &display_metric_label(&metric.label),
        &metric.value,
        Style::default()
            .fg(palette.primary)
            .add_modifier(Modifier::BOLD),
        Style::default().fg(value_color),
    );
}

fn render_split_row(
    buffer: &mut Buffer,
    area: Rect,
    left: &str,
    right: &str,
    left_style: Style,
    right_style: Style,
) {
    if area.is_empty() {
        return;
    }
    if right.is_empty() {
        Paragraph::new(left.to_string())
            .style(left_style)
            .render(area, buffer);
        return;
    }
    let right_natural = u16::try_from(Line::from(right).width()).unwrap_or(u16::MAX);
    let right_width = right_natural.min(area.width.saturating_sub(8).max(area.width / 2));
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(right_width.saturating_add(u16::from(area.width > right_width))),
    ])
    .areas(area);
    Paragraph::new(left.to_string())
        .style(left_style)
        .render(left_area, buffer);
    Paragraph::new(right.to_string())
        .style(right_style)
        .alignment(Alignment::Right)
        .render(right_area, buffer);
}

fn split_metric_value(value: &str) -> (String, Option<String>) {
    let (primary, context) = value
        .split_once(" · ")
        .map_or((value, None), |(primary, context)| (primary, Some(context)));
    let context = context.map(|context| {
        context
            .strip_prefix("resets ")
            .map(|reset| format!("Resets in {reset}"))
            .unwrap_or_else(|| title_case(context))
    });
    (primary.to_string(), context)
}

fn display_metric_label(label: &str) -> String {
    match label.to_ascii_lowercase().as_str() {
        "week" | "weekly" => "Weekly".into(),
        "5h" | "5h block" => "Session".into(),
        "24h" => "Usage trend".into(),
        "burn" => "Burn rate".into(),
        _ => title_case(label),
    }
}

fn title_case(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().chain(chars).collect()
}

#[derive(Debug, Clone, Copy)]
struct UsageMeter {
    ratio: f64,
    marker: Option<f64>,
    level: Level,
    palette: ui::Palette,
}

impl Widget for UsageMeter {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let ratio = self.ratio.clamp(0.0, 1.0);
        let raw_fill = (ratio * f64::from(area.width)).round() as u16;
        let filled = if ratio > 0.0 { raw_fill.max(1) } else { 0 }.min(area.width);
        for offset in 0..area.width {
            let (symbol, color) = if offset < filled {
                ("━", level_color(&self.palette, self.level))
            } else {
                ("─", self.palette.track)
            };
            buffer[(area.x + offset, area.y)]
                .set_symbol(symbol)
                .set_fg(color);
        }
        if let Some(marker) = self.marker.filter(|marker| marker.is_finite()) {
            let offset = ((marker.clamp(0.0, 1.0) * f64::from(area.width)).round() as u16)
                .min(area.width.saturating_sub(1));
            buffer[(area.x + offset, area.y)]
                .set_symbol("│")
                .set_fg(self.palette.muted);
        }
    }
}

fn render_key_value(
    buffer: &mut Buffer,
    area: Rect,
    key: &str,
    value: &str,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }
    let key_natural = Line::from(key).width() as u16;
    let key_width = key_natural.min(area.width / 2).max(1);
    let [key_area, value_area] = Layout::horizontal([
        Constraint::Length(key_width.saturating_add(1)),
        Constraint::Min(0),
    ])
    .areas(area);
    Paragraph::new(key.to_string())
        .style(Style::default().fg(palette.muted))
        .render(key_area, buffer);
    Paragraph::new(value.to_string())
        .style(Style::default().fg(palette.header))
        .alignment(Alignment::Right)
        .render(value_area, buffer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn sample() -> Snapshot {
        let mut codex_week = Metric::new("week", "3% · resets 6d 18h".into(), Level::Ok);
        codex_week.percent = Some(3.0);
        let mut block = Metric::new("5h block", "$3.24 est · resets 1h 20m".into(), Level::Ok);
        block.percent = Some(32.0);
        let mut day = Metric::new("24h", "$8.91 est · 1.2M tok".into(), Level::Ok);
        day.spark = (0..24).map(|hour| (hour % 5) as f64).collect();
        let claude = |name: &str, badge: &str| Provider {
            kind: ProviderKind::Claude,
            name: name.into(),
            badge: badge.into(),
            present: true,
            metrics: vec![
                block.clone(),
                Metric::new("burn", "$1.20/hr est".into(), Level::Ok),
                day.clone(),
            ],
            detail: vec![("sonnet-4-6".into(), "$5.12 · 800k tok".into())],
            as_of: Some(now_epoch_secs() - 120),
            alert: None,
            status_fragment: None,
            day_usd: Some(8.91),
        };
        Snapshot {
            providers: vec![
                Provider {
                    kind: ProviderKind::Codex,
                    name: "Codex".into(),
                    badge: "pro".into(),
                    present: true,
                    metrics: vec![codex_week, Metric::new("credits", "$436".into(), Level::Ok)],
                    detail: Vec::new(),
                    as_of: Some(now_epoch_secs() - 60),
                    alert: None,
                    status_fragment: None,
                    day_usd: None,
                },
                claude("Claude Code", "tommy@uxthemes.com"),
                claude("Claude Code · work", "work@uxthemes.com"),
            ],
        }
    }

    fn render_state(
        width: u16,
        height: u16,
        selected: usize,
        detail_open: bool,
        scroll_offset: u16,
        reveal_selected: bool,
    ) -> (String, RenderResult) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let snapshot = sample();
        let view = View {
            selected,
            detail_open,
            scanning: false,
            hosted: false,
            alerts: Alerts::default(),
            alert_dialog: None,
            scroll_offset,
            reveal_selected,
        };
        let mut rendered = RenderResult::default();
        terminal
            .draw(|frame| {
                rendered = draw(frame, Some(&snapshot), &view, &ui::Palette::DARK);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (screen, rendered)
    }

    fn render_with(
        width: u16,
        height: u16,
        selected: usize,
        detail_open: bool,
    ) -> (String, Vec<Hit>) {
        let (screen, rendered) = render_state(width, height, selected, detail_open, 0, true);
        (screen, rendered.hits)
    }

    fn render(width: u16, height: u16, detail_open: bool) -> (String, Vec<Hit>) {
        render_with(width, height, 1, detail_open)
    }

    #[test]
    fn default_view_is_a_compact_explorer_style_list() {
        let (screen, hits) = render(72, 12, false);
        assert!(screen.contains("USAGE"), "uppercase brand\n{screen}");
        assert!(
            screen.contains("24h $17.82 est"),
            "header 24h total\n{screen}"
        );
        assert!(screen.contains("Codex Pro"), "provider and badge\n{screen}");
        assert!(
            screen.contains("tommy@uxthemes.com"),
            "account badge\n{screen}"
        );
        assert!(screen.contains("Claude · work"), "second account\n{screen}");
        assert!(
            screen.contains("Weekly 97% left"),
            "basic Codex data\n{screen}"
        );
        assert!(
            screen.contains("Session 68% left"),
            "basic Claude data\n{screen}"
        );
        assert_eq!(hits.len(), 3);
        assert!(
            !screen.contains('╭') && !screen.contains('╯'),
            "borderless\n{screen}"
        );
        assert!(
            !screen.contains("Resets in"),
            "details stay collapsed\n{screen}"
        );
    }

    #[test]
    fn claude_detail_keeps_the_full_usage_hierarchy() {
        let mut session =
            Metric::used_percent("Session", 39.0, "39% · resets 2h 24m".into(), Level::Ok);
        session.marker = None;
        let mut weekly =
            Metric::used_percent("Weekly", 53.0, "53% · resets 3d 4h".into(), Level::Warn);
        weekly.annotation = Some("~2% spare".into());
        weekly.marker = Some(0.55);
        let mut fable =
            Metric::used_percent("Fable", 99.0, "99% · resets 3d 4h".into(), Level::Alert);
        fable.annotation = Some("Limit in 56m".into());
        fable.marker = Some(0.55);
        let mut trend = Metric::new("Usage Trend", String::new(), Level::Ok);
        trend.spark = (0..30)
            .map(|day| if day % 9 == 0 { 12.0 } else { (day % 4) as f64 })
            .collect();
        let snapshot = Snapshot {
            providers: vec![Provider {
                kind: ProviderKind::Claude,
                name: "Claude Code".into(),
                badge: "Team 5x".into(),
                present: true,
                metrics: vec![
                    session,
                    weekly,
                    fable,
                    Metric::new("Extra Usage", "$165.21 spent".into(), Level::Ok),
                    trend,
                    Metric::new("Today", "$220.46 est · 174.3M tokens".into(), Level::Ok),
                    Metric::new("Yesterday", "$13.52 est · 16.6M tokens".into(), Level::Ok),
                    Metric::new("Last 30 Days", "$5.7K est · 4.3B tokens".into(), Level::Ok),
                ],
                detail: Vec::new(),
                as_of: Some(now_epoch_secs()),
                alert: None,
                status_fragment: None,
                day_usd: Some(220.46),
            }],
        };
        let width = 84;
        let height = 32;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    Some(&snapshot),
                    &View {
                        selected: 0,
                        detail_open: true,
                        scanning: false,
                        hosted: false,
                        alerts: Alerts::default(),
                        alert_dialog: None,
                        scroll_offset: 0,
                        reveal_selected: true,
                    },
                    &ui::Palette::LIGHT,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        for expected in [
            "← Back",
            "Claude Team 5x",
            "39% used",
            "~2% spare",
            "99% used",
            "Limit in 56m",
            "$165.21 spent",
            "Usage Trend",
            "$220.46 est · 174.3M tokens",
            "$13.52 est · 16.6M tokens",
            "$5.7K est · 4.3B tokens",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}\n{screen}");
        }
        assert!(
            !screen.contains('╭') && !screen.contains('╯'),
            "borderless\n{screen}"
        );
    }

    #[test]
    fn usage_meter_is_full_width_and_uses_status_colors() {
        let mut terminal = Terminal::new(TestBackend::new(10, 1)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    UsageMeter {
                        ratio: 0.6,
                        marker: None,
                        level: Level::Ok,
                        palette: ui::Palette::DARK,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let symbols = (0..10).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert_eq!(symbols, "━━━━━━────");
        assert_eq!(buffer[(0, 0)].fg, ui::Palette::DARK.meter_blue);
        assert_eq!(buffer[(9, 0)].fg, ui::Palette::DARK.track);
    }

    #[test]
    fn selected_rows_use_the_shared_light_and_dark_kit_backgrounds() {
        for palette in [ui::Palette::LIGHT, ui::Palette::DARK] {
            let width = 40;
            let height = 8;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let snapshot = sample();
            let view = View {
                selected: 0,
                detail_open: false,
                scanning: false,
                hosted: false,
                alerts: Alerts::default(),
                alert_dialog: None,
                scroll_offset: 0,
                reveal_selected: true,
            };
            terminal
                .draw(|frame| {
                    draw(frame, Some(&snapshot), &view, &palette);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let expected = selected_row_style(&palette);
            let expected_background = expected.bg.expect("kit selection background");
            assert!(
                (0..width).all(|x| buffer[(x, 2)].bg == expected_background),
                "selection should paint the complete row"
            );
            assert_eq!(buffer[(0, 3)].bg, Color::Reset, "unselected row");
            assert_eq!(buffer[(0, 2)].symbol(), " ");
            assert_eq!(buffer[(1, 2)].symbol(), " ");
            assert_eq!(buffer[(2, 2)].symbol(), "C", "two-cell label inset");
        }
    }

    #[test]
    fn adaptive_selection_uses_terminal_native_reverse_video() {
        assert!(selected_row_style(&ui::Palette::ADAPTIVE)
            .add_modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn provider_rows_have_no_drag_handle_or_selection_marker() {
        let (screen, _) = render(72, 12, false);
        assert!(!screen.contains('⠿'), "drag handle\n{screen}");
        assert!(
            screen.contains("  Codex Pro"),
            "two-cell row inset\n{screen}"
        );
        assert!(
            !screen.contains('▎'),
            "selection uses a row background\n{screen}"
        );
    }

    #[test]
    fn reset_copy_is_split_for_the_openusage_style_row() {
        assert_eq!(
            split_metric_value("3% · resets 6d 18h"),
            ("3%".into(), Some("Resets in 6d 18h".into()))
        );
    }

    #[test]
    fn narrow_layout_never_wraps() {
        for width in [20u16, 38, 44, 52] {
            let (screen, _) = render(width, 24, true);
            for line in screen.lines() {
                assert_eq!(
                    line.chars().count(),
                    width as usize,
                    "unexpected line width at {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn hit_regions_cover_each_visible_row_edge_to_edge() {
        let (screen, hits) = render(72, 12, false);
        assert_eq!(hits.len(), 3);
        let rows: Vec<&str> = screen.lines().collect();
        for (hit, title) in hits.iter().zip(["Codex", "Claude", "Claude · work"]) {
            assert!(
                rows[hit.top as usize].contains(title),
                "hit top row should hold {title:?}: {:?}",
                rows[hit.top as usize]
            );
            assert_eq!(hit.bottom, hit.top);
            assert_eq!(hit.left, 0);
            assert_eq!(hit.right, 71);
            assert!(hit.contains(hit.left, hit.top));
        }
        for pair in hits.windows(2) {
            assert_eq!(pair[1].top, pair[0].bottom + 1);
        }
        assert!(hits.last().unwrap().bottom < 11);
    }

    #[test]
    fn short_viewport_scrolls_selected_row_into_view() {
        let (screen, hits) = render_with(72, 5, 2, false);
        assert_eq!(hits.first().map(|hit| hit.index), Some(1));
        assert_eq!(hits.last().map(|hit| hit.index), Some(2));
        assert!(screen.contains("Claude · work"), "selected row\n{screen}");
        assert!(screen.contains('┃'), "scrollbar thumb\n{screen}");
        for hit in &hits {
            assert!(
                hit.bottom < 4,
                "hit reaches footer: {}..{}",
                hit.top,
                hit.bottom
            );
        }
    }

    #[test]
    fn scrollbar_reaches_the_exact_top_and_bottom_rows() {
        let (top_screen, top) = render_state(72, 5, 0, false, 0, false);
        let area = top.scrollbar_area.expect("top scrollbar");
        let top_rows: Vec<&str> = top_screen.lines().collect();
        assert_eq!(
            top_rows[area.y as usize].chars().nth(area.x as usize),
            Some('┃'),
            "thumb should start at the first track row\n{top_screen}"
        );

        let (bottom_screen, bottom) = render_state(72, 5, 2, false, u16::MAX, false);
        let area = bottom.scrollbar_area.expect("bottom scrollbar");
        let bottom_rows: Vec<&str> = bottom_screen.lines().collect();
        assert_eq!(bottom.scroll_offset, bottom.max_scroll);
        assert_eq!(
            bottom_rows[area.bottom().saturating_sub(1) as usize]
                .chars()
                .nth(area.x as usize),
            Some('┃'),
            "thumb should end at the final track row\n{bottom_screen}"
        );
    }

    #[test]
    fn row_scrolling_keeps_the_last_item_flush_with_the_viewport() {
        let (screen, rendered) = render_state(72, 5, 2, false, u16::MAX, false);
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rendered.scroll_offset, rendered.max_scroll);
        assert_eq!(rendered.hits.last().map(|hit| hit.index), Some(2));
        assert!(
            rows[3].contains("Claude · work"),
            "last row should touch the bottom of the list viewport\n{screen}"
        );
        assert!(rows[2].contains("Claude"), "preceding row\n{screen}");
    }

    #[test]
    fn scrollbar_thumb_is_proportional_to_visible_content() {
        let (screen, rendered) = render_state(72, 10, 1, true, 0, false);
        let area = rendered.scrollbar_area.expect("scrollbar");
        let rows: Vec<&str> = screen.lines().collect();
        let thumb_rows = (area.y..area.bottom())
            .filter(|row| rows[*row as usize].chars().nth(area.x as usize) == Some('┃'))
            .count();
        assert!(
            thumb_rows > 1 && thumb_rows < area.height as usize,
            "thumb={thumb_rows}\n{screen}"
        );
    }

    #[test]
    fn clicks_must_be_inside_both_row_axes() {
        let (_, hits) = render(72, 24, false);
        let first = hits[0];
        assert!(first.contains(first.left, first.top));
        assert!(!first.contains(first.right.saturating_add(1), first.top));
        assert!(!first.contains(first.left, first.bottom.saturating_add(1)));
    }

    #[test]
    fn list_rows_use_the_shared_two_cell_left_inset() {
        let (screen, hits) = render(72, 12, false);
        let rows: Vec<&str> = screen.lines().collect();
        let codex = hits.iter().find(|hit| hit.index == 0).unwrap();
        let row = rows[codex.top as usize];
        assert_eq!(&row[..2], "  ", "two-cell inset\n{screen}");
        assert!(row[2..].starts_with("Codex Pro"), "content row\n{screen}");
        assert_eq!(codex.bottom, codex.top, "one terminal row per item");
    }

    #[test]
    fn selection_reveal_uses_the_smallest_required_scroll() {
        assert_eq!(reveal_selected_row(2, 17, 0, 0), 0);
        assert_eq!(reveal_selected_row(2, 2, 0, 5), 1);
        assert_eq!(reveal_selected_row(5, 2, 0, 4), 4);
        assert_eq!(reveal_selected_row(1, 2, 4, 4), 1);
    }

    #[test]
    fn detail_has_a_pinned_full_width_back_row() {
        let (screen, rendered) = render_state(72, 10, 1, true, u16::MAX, false);
        let back = rendered.back_button.expect("back hit");
        assert_eq!(back.left, 0);
        assert_eq!(back.right, 71);
        assert_eq!(back.top, 2);
        assert_eq!(back.bottom, 2);
        assert!(screen.lines().nth(2).unwrap().contains("  ← Back"));
        assert!(rendered.hits.is_empty(), "detail has no provider-row hits");
    }

    #[test]
    fn hosted_alerts_open_as_a_clickable_ratatui_dialog() {
        let width = 72;
        let height = 24;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let snapshot = sample();
        let alerts = Alerts {
            available_again: true,
            ..Alerts::default()
        };
        let mut rendered = RenderResult::default();
        terminal
            .draw(|frame| {
                rendered = draw(
                    frame,
                    Some(&snapshot),
                    &View {
                        selected: 0,
                        detail_open: false,
                        scanning: false,
                        hosted: true,
                        alerts,
                        alert_dialog: Some(2),
                        scroll_offset: 0,
                        reveal_selected: true,
                    },
                    &ui::Palette::DARK,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        for expected in [
            "Alerts",
            "Close to a limit",
            "Limit reached",
            "Available again",
            "[x] Available again",
            "space toggle",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}\n{screen}");
        }
        assert!(rendered.alert_button.is_some(), "footer alert hit");
        assert_eq!(rendered.alert_option_hits.len(), 3);
        let selected = rendered
            .alert_option_hits
            .iter()
            .find(|hit| hit.index == 2)
            .expect("available-again hit");
        assert!(selected.contains(selected.left, selected.bottom));
    }

    #[test]
    fn alert_controls_are_absent_outside_unpeel() {
        let (screen, rendered) = render_state(72, 24, 0, false, 0, true);
        assert!(rendered.alert_button.is_none());
        assert!(rendered.alert_option_hits.is_empty());
        assert!(!screen.contains("alerts off"));
        assert!(!screen.contains("a alerts"));
    }

    #[test]
    fn tiny_terminals_render_without_panicking() {
        for width in 1..=12 {
            for height in 1..=4 {
                let _ = render_with(width, height, 0, true);
            }
        }
    }
}
