//! Ratatui dashboard rendering.
//!
//! The screen is composed entirely from Ratatui layout primitives and
//! widgets. Provider cards are real `Block`s, percentages use a purpose-built
//! Ratatui `UsageMeter`, hourly activity is a `Sparkline`, and overflowing
//! provider lists get a `Scrollbar`. The only terminal-specific code lives in
//! the event loop.

use crate::config::{AlertOption, Alerts};
use crate::sources::{Level, Metric, PercentDisplay, Provider, ProviderKind, Snapshot};
use crate::theme as ui;
use crate::timeparse::{compact_duration, now_epoch_secs};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Sparkline, Widget};
use ratatui::Frame;
use unpeel_tui_kit::VerticalScrollbar;

const SECTION_GAP: u16 = 1;
const HEADER_HEIGHT: u16 = 1;
const HEADER_TO_CARD_GAP: u16 = 1;
const METRIC_GAP: u16 = 1;
const CELL_PADDING: u16 = 1;

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
    pub expanded: bool,
    pub scanning: bool,
    pub hosted: bool,
    pub alerts: Alerts,
    pub alert_dialog: Option<usize>,
    /// Requested top row in the virtual provider canvas.
    pub scroll_offset: u16,
    /// Bring the selected provider into view after keyboard navigation,
    /// refreshes, expansion, and resize. Mouse/page scrolling disables this
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
    pub alert_button: Option<Hit>,
    pub alert_option_hits: Vec<Hit>,
    pub alert_dialog_area: Option<Rect>,
}

/// One card's clickable screen rectangle, inclusive on every edge.
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
        Some(snapshot) => render_providers(frame, body, &snapshot.providers, view, palette),
    };
    result.alert_button = render_footer(frame, footer, view.hosted, palette);
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
    palette: &ui::Palette,
) -> Option<Hit> {
    if area.is_empty() {
        return None;
    }
    let hints: &[(&str, &str)] = if area.width >= 68 {
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
    } else if hosted && area.width >= 36 {
        &[("j/k", "select"), ("a", "alerts"), ("q", "quit")]
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

fn render_providers(
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
    let heights: Vec<u16> = providers
        .iter()
        .enumerate()
        .map(|(index, provider)| section_height(provider, view.expanded && index == selected))
        .collect();
    let total_height =
        heights
            .iter()
            .copied()
            .fold(0u16, u16::saturating_add)
            .saturating_add(SECTION_GAP.saturating_mul(
                u16::try_from(providers.len().saturating_sub(1)).unwrap_or(u16::MAX),
            ));
    let scrollable = total_height > area.height;
    let horizontal_inset = u16::from(area.width >= 4);
    let content_area = Rect::new(
        area.x.saturating_add(horizontal_inset),
        area.y,
        area.width
            .saturating_sub(horizontal_inset.saturating_mul(2)),
        area.height,
    );
    let show_scrollbar = scrollable && content_area.width > 1;
    let sections_area = if show_scrollbar {
        Rect::new(
            content_area.x,
            content_area.y,
            content_area.width - 1,
            content_area.height,
        )
    } else {
        content_area
    };
    if sections_area.is_empty() {
        return RenderResult {
            viewport_height: area.height,
            ..RenderResult::default()
        };
    }

    let starts = section_starts(&heights);
    let max_scroll = total_height.saturating_sub(sections_area.height);
    let requested = view.scroll_offset.min(max_scroll);
    let scroll_offset = if view.reveal_selected {
        reveal_selected_offset(
            &starts,
            &heights,
            selected,
            sections_area.height,
            requested,
            max_scroll,
        )
    } else {
        requested
    };

    // Render every provider into a virtual Ratatui canvas, then copy the
    // requested row window into the frame. Partial cards stay continuous at
    // both viewport edges and the scrollbar can represent real content rows.
    let virtual_area = Rect::new(0, 0, sections_area.width, total_height.max(1));
    let mut virtual_buffer = Buffer::empty(virtual_area);
    for (index, provider) in providers.iter().enumerate() {
        render_provider_section(
            &mut virtual_buffer,
            Rect::new(0, starts[index], sections_area.width, heights[index]),
            provider,
            index == selected,
            view.expanded && index == selected,
            palette,
        );
    }
    {
        let destination = frame.buffer_mut();
        for row in 0..sections_area.height {
            let source_y = scroll_offset.saturating_add(row);
            if source_y >= total_height {
                break;
            }
            for column in 0..sections_area.width {
                destination[(sections_area.x + column, sections_area.y + row)] =
                    virtual_buffer[(column, source_y)].clone();
            }
        }
    }

    let mut hits = Vec::new();
    let viewport_end = scroll_offset.saturating_add(sections_area.height);
    for (index, start) in starts.iter().copied().enumerate() {
        let end = start.saturating_add(heights[index]);
        let visible_start = start.max(scroll_offset);
        let visible_end = end.min(viewport_end);
        if visible_start < visible_end {
            let section_area = Rect::new(
                sections_area.x,
                sections_area
                    .y
                    .saturating_add(visible_start.saturating_sub(scroll_offset)),
                sections_area.width,
                visible_end.saturating_sub(visible_start),
            );
            if let Some(hit) = Hit::from_rect(index, section_area) {
                hits.push(hit);
            }
        }
    }

    let scrollbar_area = show_scrollbar.then(|| {
        Rect::new(
            content_area.right().saturating_sub(1),
            content_area.y,
            1,
            content_area.height,
        )
    });
    if let Some(scrollbar_area) = scrollbar_area {
        frame.render_widget(
            VerticalScrollbar::new(
                usize::from(total_height),
                usize::from(sections_area.height),
                usize::from(scroll_offset),
            )
            .track_style(Style::default().fg(palette.track))
            .thumb_style(Style::default().fg(palette.focus)),
            scrollbar_area,
        );
    }

    RenderResult {
        hits,
        scroll_offset,
        max_scroll,
        viewport_height: sections_area.height,
        scrollbar_area,
        ..RenderResult::default()
    }
}

fn section_starts(heights: &[u16]) -> Vec<u16> {
    let mut starts = Vec::with_capacity(heights.len());
    let mut cursor = 0u16;
    for height in heights {
        starts.push(cursor);
        cursor = cursor.saturating_add(*height).saturating_add(SECTION_GAP);
    }
    starts
}

fn reveal_selected_offset(
    starts: &[u16],
    heights: &[u16],
    selected: usize,
    viewport_height: u16,
    current: u16,
    max_scroll: u16,
) -> u16 {
    let Some((&start, &height)) = starts.get(selected).zip(heights.get(selected)) else {
        return current.min(max_scroll);
    };
    if viewport_height == 0 {
        return 0;
    }
    let end = start.saturating_add(height);
    let viewport_end = current.saturating_add(viewport_height);
    if height > viewport_height {
        if start < current || start >= viewport_end {
            start.min(max_scroll)
        } else {
            current.min(max_scroll)
        }
    } else if start < current {
        start.min(max_scroll)
    } else if end > viewport_end {
        end.saturating_sub(viewport_height).min(max_scroll)
    } else {
        current.min(max_scroll)
    }
}

fn section_height(provider: &Provider, expanded: bool) -> u16 {
    HEADER_HEIGHT
        .saturating_add(HEADER_TO_CARD_GAP)
        .saturating_add(card_height(provider, expanded))
}

fn card_height(provider: &Provider, expanded: bool) -> u16 {
    let metric_rows = if !provider.present || provider.metrics.is_empty() {
        1
    } else {
        provider
            .metrics
            .iter()
            .map(metric_height)
            .fold(0u16, u16::saturating_add)
            .saturating_add(METRIC_GAP.saturating_mul(
                u16::try_from(provider.metrics.len().saturating_sub(1)).unwrap_or(u16::MAX),
            ))
    };
    let detail_count = if expanded {
        u16::try_from(provider.detail.len())
            .unwrap_or(u16::MAX)
            .saturating_add(u16::from(provider.as_of.is_some()))
    } else {
        0
    };
    let details = detail_count.saturating_add(u16::from(detail_count > 0));
    metric_rows
        .saturating_add(details)
        .saturating_add(2)
        .saturating_add(CELL_PADDING.saturating_mul(2))
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

fn render_provider_section(
    buffer: &mut Buffer,
    area: Rect,
    provider: &Provider,
    selected: bool,
    expanded: bool,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }

    let header = Rect::new(area.x, area.y, area.width, area.height.min(HEADER_HEIGHT));
    render_provider_header(buffer, header, provider, selected, palette);

    let card_y = area
        .y
        .saturating_add(HEADER_HEIGHT)
        .saturating_add(HEADER_TO_CARD_GAP);
    if card_y >= area.bottom() {
        return;
    }
    let card = Rect::new(area.x, card_y, area.width, area.bottom() - card_y);
    render_provider_card(buffer, card, provider, selected, expanded, palette);
}

fn render_provider_header(
    buffer: &mut Buffer,
    area: Rect,
    provider: &Provider,
    selected: bool,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }

    let accent = accent(palette, provider.kind);
    let name_color = if provider.alert.is_some() {
        palette.attention
    } else {
        palette.primary
    };
    let name_style = Style::default().fg(name_color).add_modifier(if selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    });
    let marker = if selected { "▎ " } else { "  " };
    let mut title = vec![
        Span::styled(marker, Style::default().fg(palette.focus)),
        Span::styled(display_provider_name(&provider.name), name_style),
    ];
    if !provider.badge.is_empty() {
        title.push(Span::raw(" "));
        title.push(Span::styled(
            display_badge(&provider.badge),
            Style::default().fg(palette.muted),
        ));
    }
    let symbol = if provider.alert.is_some() {
        "⚠"
    } else {
        "●"
    };
    let symbol_color = if provider.alert.is_some() {
        palette.attention
    } else if provider.present && !provider.metrics.is_empty() {
        accent
    } else {
        palette.track
    };
    let [title_area, symbol_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(u16::from(area.width >= 2).saturating_mul(2)),
    ])
    .areas(area);
    Paragraph::new(Line::from(title)).render(title_area, buffer);
    Paragraph::new(Span::styled(symbol, Style::default().fg(symbol_color)))
        .alignment(Alignment::Right)
        .render(symbol_area, buffer);
}

fn render_provider_card(
    buffer: &mut Buffer,
    area: Rect,
    provider: &Provider,
    selected: bool,
    expanded: bool,
    palette: &ui::Palette,
) {
    if area.is_empty() {
        return;
    }

    let border_color = if provider.alert.is_some() {
        palette.attention
    } else if selected {
        palette.focus
    } else {
        palette.surface_edge
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::uniform(CELL_PADDING));

    let inner = block.inner(area);
    block.render(area, buffer);
    if inner.is_empty() {
        return;
    }

    let mut y = inner.y;
    if !provider.present {
        render_muted_row(buffer, row_at(inner, y), "not installed", palette);
        return;
    }
    if provider.metrics.is_empty() {
        render_muted_row(buffer, row_at(inner, y), "no recent activity", palette);
        return;
    }
    for (index, metric) in provider.metrics.iter().enumerate() {
        if y >= inner.bottom() {
            return;
        }
        let height = metric_height(metric).min(inner.bottom().saturating_sub(y));
        render_metric(
            buffer,
            Rect::new(inner.x, y, inner.width, height),
            metric,
            palette,
        );
        y = y.saturating_add(metric_height(metric));
        if index + 1 < provider.metrics.len() {
            y = y.saturating_add(METRIC_GAP);
        }
    }
    if expanded {
        let has_details = !provider.detail.is_empty() || provider.as_of.is_some();
        if has_details {
            y = y.saturating_add(METRIC_GAP);
        }
        for (key, value) in &provider.detail {
            if y >= inner.bottom() {
                return;
            }
            render_key_value(buffer, row_at(inner, y), key, value, palette);
            y = y.saturating_add(1);
        }
        if let Some(as_of) = provider.as_of {
            if y < inner.bottom() {
                let age = compact_duration(now_epoch_secs() - as_of);
                render_key_value(
                    buffer,
                    row_at(inner, y),
                    "updated",
                    &format!("{age} ago"),
                    palette,
                );
            }
        }
    }
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
        expanded: bool,
        scroll_offset: u16,
        reveal_selected: bool,
    ) -> (String, RenderResult) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let snapshot = sample();
        let view = View {
            selected,
            expanded,
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

    fn render_with(width: u16, height: u16, selected: usize, expanded: bool) -> (String, Vec<Hit>) {
        let (screen, rendered) = render_state(width, height, selected, expanded, 0, true);
        (screen, rendered.hits)
    }

    fn render(width: u16, height: u16, expanded: bool) -> (String, Vec<Hit>) {
        render_with(width, height, 1, expanded)
    }

    #[test]
    fn wide_layout_matches_openusage_information_hierarchy() {
        let (screen, _) = render(72, 48, true);
        assert!(screen.contains("USAGE"), "uppercase brand\n{screen}");
        assert!(
            screen.contains("24h $17.82 est"),
            "header 24h total\n{screen}"
        );
        assert!(
            screen.contains("tommy@uxthemes.com"),
            "account badge\n{screen}"
        );
        assert!(screen.contains("Claude · work"), "second account\n{screen}");
        assert!(screen.contains("Weekly"), "quota title\n{screen}");
        assert!(screen.contains("97% left"), "remaining quota\n{screen}");
        assert!(
            screen.contains("Resets in 6d 18h"),
            "reset context\n{screen}"
        );
        assert!(screen.contains("Session"), "session title\n{screen}");
        assert!(screen.contains("68% left"), "budget remaining\n{screen}");
        assert!(screen.contains("$1.20/hr est"), "burn rate\n{screen}");
        assert!(
            ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
                .iter()
                .any(|glyph| screen.contains(*glyph)),
            "sparkline or gauge glyphs\n{screen}"
        );
        assert!(screen.contains("sonnet-4-6"), "expanded detail\n{screen}");
        assert!(
            screen.contains('╭') && screen.contains('╯'),
            "card blocks\n{screen}"
        );
    }

    #[test]
    fn claude_card_matches_the_openusage_expanded_hierarchy() {
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
                        expanded: false,
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
    fn cards_inherit_the_terminal_background_in_every_palette() {
        for palette in [ui::Palette::ADAPTIVE, ui::Palette::LIGHT, ui::Palette::DARK] {
            let width = 72;
            let height = 48;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let snapshot = sample();
            let view = View {
                selected: 0,
                expanded: false,
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
            let weekly = (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .find(|&(x, y)| buffer[(x, y)].symbol() == "W")
                .expect("Weekly label");
            let card_corner = (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .find(|&(x, y)| buffer[(x, y)].symbol() == "╭")
                .expect("selected card corner");

            assert_eq!(buffer[weekly].fg, palette.primary);
            assert_eq!(buffer[weekly].bg, Color::Reset);
            assert_eq!(buffer[card_corner].fg, palette.focus);
            assert_eq!(buffer[card_corner].bg, Color::Reset);
            assert_eq!(buffer[(0, 1)].fg, palette.track);
        }
    }

    #[test]
    fn provider_headers_have_no_drag_handle() {
        let (screen, _) = render(72, 48, false);
        assert!(!screen.contains('⠿'), "drag handle\n{screen}");
        assert!(screen.contains("Codex Pro"), "provider header\n{screen}");
        assert!(screen.contains("▎ Claude"), "selection marker\n{screen}");
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
    fn hit_regions_cover_each_visible_card() {
        let (screen, hits) = render(72, 48, false);
        assert_eq!(hits.len(), 3);
        let rows: Vec<&str> = screen.lines().collect();
        for (hit, title) in hits.iter().zip(["Codex", "Claude", "Claude · work"]) {
            assert!(
                rows[hit.top as usize].contains(title),
                "hit top row should hold {title:?}: {:?}",
                rows[hit.top as usize]
            );
            assert!(hit.bottom >= hit.top);
            assert!(hit.right >= hit.left);
            assert!(hit.contains(hit.left, hit.top));
        }
        for pair in hits.windows(2) {
            assert!(pair[1].top > pair[0].bottom);
        }
        assert!(hits.last().unwrap().bottom < 47);
    }

    #[test]
    fn short_viewport_scrolls_selected_card_into_view() {
        let (screen, hits) = render_with(72, 8, 2, false);
        assert_eq!(hits.first().map(|hit| hit.index), Some(2));
        assert!(screen.contains("Claude · work"), "selected card\n{screen}");
        assert!(screen.contains('┃'), "scrollbar thumb\n{screen}");
        for hit in &hits {
            assert!(
                hit.bottom < 7,
                "hit reaches footer: {}..{}",
                hit.top,
                hit.bottom
            );
        }
    }

    #[test]
    fn scrollbar_reaches_the_exact_top_and_bottom_rows() {
        let (top_screen, top) = render_state(72, 8, 0, false, 0, false);
        let area = top.scrollbar_area.expect("top scrollbar");
        let top_rows: Vec<&str> = top_screen.lines().collect();
        assert_eq!(
            top_rows[area.y as usize].chars().nth(area.x as usize),
            Some('┃'),
            "thumb should start at the first track row\n{top_screen}"
        );

        let (bottom_screen, bottom) = render_state(72, 8, 2, false, u16::MAX, false);
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
    fn row_scrolling_keeps_the_last_card_flush_with_the_viewport() {
        let (screen, rendered) = render_state(72, 8, 2, false, u16::MAX, false);
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rendered.scroll_offset, rendered.max_scroll);
        assert_eq!(rendered.hits.last().map(|hit| hit.index), Some(2));
        assert!(
            rows[6].contains('╰') && rows[6].contains('╯'),
            "card bottom\n{screen}"
        );
        assert!(
            (2..=6).all(|row| rows[row].trim().len() > 1),
            "viewport should not end in blank rows\n{screen}"
        );
    }

    #[test]
    fn scrollbar_thumb_is_proportional_to_visible_content() {
        let (screen, rendered) = render_state(72, 24, 0, false, 0, false);
        let area = rendered.scrollbar_area.expect("scrollbar");
        let rows: Vec<&str> = screen.lines().collect();
        let thumb_rows = (area.y..area.bottom())
            .filter(|row| rows[*row as usize].chars().nth(area.x as usize) == Some('┃'))
            .count();
        // Body is 21 of 41 virtual rows, so the thumb should occupy roughly
        // half of the 21-row track rather than a one-cell provider marker.
        assert!(
            (10..=12).contains(&thumb_rows),
            "thumb={thumb_rows}\n{screen}"
        );
    }

    #[test]
    fn clicks_must_be_inside_both_card_axes() {
        let (_, hits) = render(72, 24, false);
        let first = hits[0];
        assert!(first.contains(first.left, first.top));
        assert!(!first.contains(first.right.saturating_add(1), first.top));
        assert!(!first.contains(first.left, first.bottom.saturating_add(1)));
    }

    #[test]
    fn cards_have_one_cell_padding_on_every_side() {
        let (screen, hits) = render(72, 48, false);
        let rows: Vec<&str> = screen.lines().collect();
        let codex = hits.iter().find(|hit| hit.index == 0).unwrap();

        let card_top = codex.top as usize + HEADER_HEIGHT as usize + HEADER_TO_CARD_GAP as usize;
        let top_padding = rows[card_top + 1];
        let first_metric = rows[card_top + 2];
        let bottom_padding = rows[codex.bottom as usize - 1];
        let border = first_metric.find('│').expect("card border");
        assert!(!top_padding.contains("Weekly"), "top padding\n{screen}");
        assert_eq!(
            first_metric.chars().nth(border + 1),
            Some(' '),
            "left padding\n{screen}"
        );
        assert!(first_metric.contains("Weekly"), "content row\n{screen}");
        assert!(
            !bottom_padding.contains("Credits"),
            "bottom padding\n{screen}"
        );
    }

    #[test]
    fn selection_reveal_uses_the_smallest_required_scroll() {
        let heights = [4, 5, 5];
        let starts = section_starts(&heights);
        assert_eq!(reveal_selected_offset(&starts, &heights, 2, 17, 0, 0), 0);
        assert_eq!(reveal_selected_offset(&starts, &heights, 2, 11, 0, 5), 5);
        assert_eq!(reveal_selected_offset(&starts, &heights, 2, 5, 0, 11), 11);
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
                        expanded: false,
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
