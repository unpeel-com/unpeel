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
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use ratatui::Frame;
use std::path::Path;
#[cfg(test)]
use unpeel_app_kit::SelectableRow;
use unpeel_app_kit::{
    Badge, KitTheme, List, ListItem, ListItemEmphasis, ListItemSlot, ListItemTone,
    ListPageBehavior, ListState, Page, PageTheme, Sparkline, Toggle, VerticalScrollbar,
    SELECTABLE_LEFT_PADDING,
};

const METRIC_GAP: u16 = 1;
const DETAIL_TOP_GAP: u16 = 1;

pub const SEMANTIC_ROOT_ID: &str = "usage-page";
pub const OPEN_PROVIDER_ACTION: &str = "open-provider";
pub const SELECT_PROVIDER_ACTION: &str = "select-provider";
pub const CLOSE_PROVIDER_ACTION: &str = "close-provider";
pub const REFRESH_ACTION: &str = "refresh-usage";
pub const OPEN_ALERTS_ACTION: &str = "open-alerts";
pub const CLOSE_ALERTS_ACTION: &str = "close-alerts";
pub const SELECT_ALERT_ACTION: &str = "select-alert";
pub const SET_ALERT_ACTION: &str = "set-alert";

pub fn provider_node_id(index: usize) -> String {
    format!("provider-{index}")
}

pub fn provider_index_from_node_id(node_id: &str) -> Option<usize> {
    node_id.strip_prefix("provider-")?.parse().ok()
}

/// Closed App Kit projection of the same master/detail model as the Ratatui
/// dashboard. Bounded quota meters keep their compact list metadata, while
/// numeric history is a semantic Sparkline interpreted by every renderer.
pub fn semantic_page(snapshot: Option<&Snapshot>, view: &View) -> Page {
    if view.hosted && view.alert_dialog.is_some() {
        return semantic_alerts(view);
    }
    let Some(snapshot) = snapshot else {
        return Page::new(
            "Usage",
            List::new("usage-providers", Vec::new()).empty_message("Scanning local usage…"),
        );
    };
    if view.detail_open && !snapshot.providers.is_empty() {
        let index = view.selected.min(snapshot.providers.len() - 1);
        return semantic_provider_detail(&snapshot.providers[index], index, view.scanning);
    }

    let mut items = snapshot
        .providers
        .iter()
        .enumerate()
        .map(|(index, provider)| provider_list_item(provider, index, 96))
        .collect::<Vec<_>>();
    items.push(
        ListItem::new(
            "refresh-usage",
            if view.scanning {
                "Refreshing…"
            } else {
                "Refresh"
            },
        )
        .detail("Rescan local provider history and live limits")
        .activate_action(REFRESH_ACTION),
    );
    if view.hosted {
        items.push(
            ListItem::new("open-alerts", "Alerts")
                .detail("Choose which limit changes notify this session")
                .disclosure_action(OPEN_ALERTS_ACTION),
        );
    }
    let mut list = List::new("usage-providers", items)
        .empty_message("No local usage data")
        .page_behavior(ListPageBehavior::Scroll);
    if snapshot.providers.get(view.selected).is_some() {
        list = list.selected(provider_node_id(view.selected), SELECT_PROVIDER_ACTION);
    }
    Page::new("Usage", list)
}

fn provider_list_item(provider: &Provider, index: usize, row_width: u16) -> ListItem {
    let list_name = display_provider_list_name(&provider.name);
    let list_badge = display_provider_list_badge(&provider.badge);
    let title_width = Line::from(match &list_badge {
        Some(badge) => format!("{list_name} {badge}"),
        None => list_name.clone(),
    })
    .width();
    let content_width = row_width
        .saturating_sub(SELECTABLE_LEFT_PADDING)
        .saturating_sub(1);
    let reserved_title = u16::try_from(title_width)
        .unwrap_or(u16::MAX)
        .min(content_width / 2);
    let summary_budget = content_width
        .saturating_sub(reserved_title)
        .saturating_sub(1);
    let (summary, level) = provider_basic_data(provider, summary_budget);
    let mut item = ListItem::new(provider_node_id(index), list_name)
        .emphasis(ListItemEmphasis::Strong)
        .label_tone(if provider.alert.is_some() {
            ListItemTone::Danger
        } else {
            ListItemTone::Default
        })
        .activate_action(OPEN_PROVIDER_ACTION);
    if let Some(badge) = list_badge {
        item = item.accessory(ListItemSlot::badge(Badge::new(badge)));
    }
    if !summary.is_empty() {
        item = item
            .value(summary)
            .value_tone(if provider.alert.is_some() || level == Level::Alert {
                ListItemTone::Danger
            } else if level == Level::Warn {
                ListItemTone::Warning
            } else {
                ListItemTone::Muted
            })
            .value_min_width(0);
    }
    item
}

fn semantic_provider_detail(provider: &Provider, index: usize, scanning: bool) -> Page {
    let prefix = provider_node_id(index);
    let mut items = Vec::new();
    if let Some(alert) = &provider.alert {
        items.push(ListItem::new(format!("{prefix}-alert"), "Alert").value(alert.clone()));
    }
    if !provider.present {
        items.push(ListItem::new(format!("{prefix}-status"), "Status").value("Not installed"));
    } else if provider.metrics.is_empty()
        && provider.monthly_tokens.is_empty()
        && provider.project_usage.is_empty()
    {
        items.push(ListItem::new(format!("{prefix}-status"), "Status").value("No recent activity"));
    }
    for (metric_index, metric) in provider.metrics.iter().enumerate() {
        let mut item = ListItem::new(
            format!("{prefix}-metric-{metric_index}"),
            display_metric_label(&metric.label),
        );
        if !metric.value.is_empty() {
            item = item.value(metric.value.clone());
        }
        if !metric.spark.is_empty() {
            item = item
                .trailing(ListItemSlot::sparkline(metric_sparkline(
                    format!("{prefix}-metric-{metric_index}-sparkline"),
                    metric,
                )))
                .value_tone(metric_tone(metric.level));
        }
        if let Some(annotation) = &metric.annotation {
            item = item.detail(annotation.clone());
        }
        items.push(item);
    }
    if provider.kind == ProviderKind::Total {
        for (project_index, (path, tokens)) in
            current_project_rows(provider).into_iter().enumerate()
        {
            items.push(
                ListItem::new(
                    format!("{prefix}-project-{project_index}"),
                    project_label(path),
                )
                .detail("Current month")
                .value(format_exact_tokens(tokens)),
            );
        }
    }
    for (month_index, usage) in provider.monthly_tokens.iter().enumerate() {
        items.push(
            ListItem::new(
                format!("{prefix}-month-{month_index}"),
                month_label(usage.month, usage.year, month_index == 0),
            )
            .value(if usage.tokens == 0 {
                "—".to_string()
            } else {
                format_exact_tokens(usage.tokens)
            }),
        );
    }
    for (detail_index, (key, value)) in provider.detail.iter().enumerate() {
        items.push(
            ListItem::new(format!("{prefix}-detail-{detail_index}"), title_case(key))
                .value(value.clone()),
        );
    }
    if let Some(as_of) = provider.as_of {
        items.push(
            ListItem::new(format!("{prefix}-updated"), "Updated").value(format!(
                "{} ago",
                compact_duration(now_epoch_secs() - as_of)
            )),
        );
    }
    items.push(
        ListItem::new(
            format!("{prefix}-refresh"),
            if scanning { "Refreshing…" } else { "Refresh" },
        )
        .activate_action(REFRESH_ACTION),
    );
    items.push(
        ListItem::new(format!("{prefix}-alerts"), "Alerts")
            .detail("Choose session notifications")
            .disclosure_action(OPEN_ALERTS_ACTION),
    );
    let title = if provider.badge.is_empty() {
        display_provider_name(&provider.name)
    } else {
        format!(
            "{} · {}",
            display_provider_name(&provider.name),
            display_badge(&provider.badge)
        )
    };
    Page::new(title, List::new("usage-detail", items)).back_action(CLOSE_PROVIDER_ACTION)
}

fn metric_tone(level: Level) -> ListItemTone {
    match level {
        Level::Ok => ListItemTone::Info,
        Level::Warn => ListItemTone::Warning,
        Level::Alert => ListItemTone::Danger,
    }
}

fn metric_sparkline(id: impl Into<String>, metric: &Metric) -> Sparkline {
    let label = display_metric_label(&metric.label);
    let minimum = metric.spark.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = metric
        .spark
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    Sparkline::new(
        id,
        metric.spark.iter().copied(),
        format!(
            "{label} history: {} points, minimum {minimum}, maximum {maximum}",
            metric.spark.len()
        ),
    )
}

fn semantic_alerts(view: &View) -> Page {
    let selected = view
        .alert_dialog
        .unwrap_or_default()
        .min(AlertOption::ALL.len() - 1);
    let items = AlertOption::ALL
        .into_iter()
        .enumerate()
        .map(|(index, option)| {
            let enabled = view.alerts.enabled(option);
            ListItem::new(alert_node_id(index), option.label())
                .detail(option.description())
                .done(enabled)
                .trailing(ListItemSlot::toggle(Toggle::new(
                    alert_toggle_id(index),
                    option.label(),
                    enabled,
                    SET_ALERT_ACTION,
                )))
        })
        .collect();
    Page::new(
        "Alerts",
        List::new("usage-alerts", items).selected(alert_node_id(selected), SELECT_ALERT_ACTION),
    )
    .back_action(CLOSE_ALERTS_ACTION)
}

pub fn alert_node_id(index: usize) -> String {
    format!("alert-{index}")
}

pub fn alert_toggle_id(index: usize) -> String {
    format!("alert-toggle-{index}")
}

pub fn alert_index_from_node_id(node_id: &str) -> Option<usize> {
    node_id
        .strip_prefix("alert-toggle-")
        .or_else(|| node_id.strip_prefix("alert-"))?
        .parse()
        .ok()
}

fn accent(palette: &ui::Palette, kind: ProviderKind) -> Color {
    match kind {
        ProviderKind::Codex => palette.codex_accent,
        ProviderKind::Claude => palette.claude_accent,
        ProviderKind::Grok => palette.grok_accent,
        ProviderKind::Muse => palette.muse_accent,
        ProviderKind::CurrentProject | ProviderKind::Total => palette.focus,
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
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

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
    render_footer(frame, footer, view, palette);
    if let Some(selected) = view.alert_dialog.filter(|_| view.hosted) {
        let (area, hits) = render_alert_dialog(frame, selected, view.alerts, palette);
        result.alert_dialog_area = Some(area);
        result.alert_option_hits = hits;
    }
    result
}

fn render_empty_state(frame: &mut Frame, area: Rect, scanning: bool, palette: &ui::Palette) {
    if area.is_empty() {
        return;
    }
    let message = if scanning {
        "scanning local usage…"
    } else {
        "no local usage data"
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

fn render_footer(frame: &mut Frame, area: Rect, view: &View, palette: &ui::Palette) {
    if area.is_empty() {
        return;
    }
    let mut spans = vec![Span::raw("  ")];
    if view.hosted {
        spans.push(Span::styled("a", Style::default().fg(palette.primary)));
        spans.push(Span::styled(" alert  ", Style::default().fg(palette.muted)));
    }
    spans.push(Span::styled("r", Style::default().fg(palette.primary)));
    if view.scanning {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            ui::spinner_frame(),
            Style::default().fg(palette.focus),
        ));
        spans.push(Span::styled(
            " refreshing…",
            Style::default().fg(palette.muted),
        ));
    } else {
        spans.push(Span::styled(" refresh", Style::default().fg(palette.muted)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
    let show_scrollbar = providers.len() > usize::from(area.height) && area.width > 1;
    let row_width = area.width.saturating_sub(u16::from(show_scrollbar));
    let selected = view.selected.min(providers.len().saturating_sub(1));
    let items = providers
        .iter()
        .enumerate()
        .map(|(index, provider)| provider_list_item(provider, index, row_width))
        .collect::<Vec<_>>();
    let mut list = List::new("usage-providers", items).page_behavior(ListPageBehavior::Scroll);
    if !providers.is_empty() {
        list = list.selected(provider_node_id(selected), SELECT_PROVIDER_ACTION);
    }
    let mut state = ListState::new((!providers.is_empty()).then_some(selected));
    state.set_offset(usize::from(view.scroll_offset), providers.len());
    if view.reveal_selected {
        state.request_reveal();
    }
    frame.render_widget(
        list.widget(&mut state).theme(provider_list_theme(palette)),
        area,
    );

    let rows_area = state.rows_area();
    let hits = (0..usize::from(rows_area.height))
        .filter_map(|row| {
            let index = state.offset().saturating_add(row);
            if index >= providers.len() {
                return None;
            }
            Hit::from_rect(
                index,
                Rect::new(
                    rows_area.x,
                    rows_area
                        .y
                        .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                    rows_area.width,
                    1,
                ),
            )
        })
        .collect();
    let scrollbar_area =
        show_scrollbar.then(|| Rect::new(area.right().saturating_sub(1), area.y, 1, area.height));
    RenderResult {
        hits,
        scroll_offset: u16::try_from(state.offset()).unwrap_or(u16::MAX),
        max_scroll: u16::try_from(state.max_offset(providers.len())).unwrap_or(u16::MAX),
        viewport_height: rows_area.height,
        scrollbar_area,
        ..RenderResult::default()
    }
}

fn provider_list_theme(palette: &ui::Palette) -> PageTheme {
    let (scrollbar_track, scrollbar_thumb) = scrollbar_styles(palette);
    PageTheme {
        style: Style::default(),
        title: Style::default()
            .fg(palette.header)
            .add_modifier(Modifier::BOLD),
        item: Style::default().fg(palette.primary),
        detail: Style::default().fg(palette.muted),
        value: Style::default().fg(palette.muted),
        accent: Style::default().fg(palette.focus),
        info: Style::default().fg(palette.meter_blue),
        success: Style::default().fg(palette.primary),
        warning: Style::default().fg(palette.warning),
        danger: Style::default().fg(palette.attention),
        done: Style::default().fg(palette.muted),
        toggle: Style::default().fg(palette.focus),
        badge: Style::default().fg(palette.muted),
        busy: Style::default().fg(palette.focus),
        selected_busy: Style::default(),
        delete: Style::default().fg(palette.muted),
        empty: Style::default().fg(palette.muted),
        selected: selected_row_style(palette),
        selected_item: Style::default(),
        selected_detail: Style::default().add_modifier(Modifier::DIM),
        selected_value: Style::default().add_modifier(Modifier::DIM),
        selected_badge: Style::default().add_modifier(Modifier::DIM),
        navigation: Style::default().fg(palette.muted),
        scrollbar_track,
        scrollbar_thumb,
        left_padding: SELECTABLE_LEFT_PADDING,
        left_padding_style: Style::default(),
        right_padding: 1,
        style_value_gap: true,
        style_status_spacing: false,
    }
}

// Frozen copy of the pre-migration renderer. Buffer-parity tests below keep
// the shared App Kit List honest as this app's row vocabulary evolves.
#[cfg(test)]
fn render_provider_list_legacy(
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
        render_provider_list_row_legacy(
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

#[cfg(test)]
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

#[cfg(test)]
fn render_provider_list_row_legacy(
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
    let content = SelectableRow::new(selected, row_style).paint(area, buffer);
    if content.is_empty() {
        return;
    }
    let list_name = display_provider_list_name(&provider.name);
    let list_badge = display_provider_list_badge(&provider.badge);
    let title_width = Line::from(match &list_badge {
        Some(badge) => format!("{list_name} {badge}"),
        None => list_name.clone(),
    })
    .width();
    let reserved_title = u16::try_from(title_width)
        .unwrap_or(u16::MAX)
        .min(content.width / 2);
    let summary_budget = content
        .width
        .saturating_sub(reserved_title)
        .saturating_sub(1);
    let (summary, level) = provider_basic_data(provider, summary_budget);
    let summary_width = u16::try_from(Line::from(summary.as_str()).width())
        .unwrap_or(u16::MAX)
        .min(summary_budget);
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
    let mut title = vec![Span::styled(list_name, name_style)];
    if let Some(badge) = list_badge {
        title.push(Span::raw(" "));
        title.push(Span::styled(
            badge,
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

fn provider_basic_data(provider: &Provider, max_width: u16) -> (String, Level) {
    if !provider.present {
        return ("Not installed".into(), Level::Ok);
    }
    let Some(first) = provider.metrics.first() else {
        return ("No recent activity".into(), Level::Ok);
    };
    let bounded: Vec<&Metric> = provider
        .metrics
        .iter()
        .filter(|metric| metric.percent.is_some())
        .collect();
    if !bounded.is_empty() {
        let fable = bounded
            .iter()
            .copied()
            .find(|metric| metric.label.eq_ignore_ascii_case("Fable"));
        let mut focused = vec![bounded[0]];
        if let Some(metric) = fable.or_else(|| bounded.get(1).copied()) {
            if !std::ptr::eq(metric, focused[0]) {
                focused.push(metric);
            }
        }
        let single = [fable.unwrap_or(bounded[0])];
        let candidates = [
            format_quota_usage(&bounded, quota_label_verbose),
            format_quota_usage(&bounded, quota_label_compact),
            format_quota_usage(&focused, quota_label_compact),
            format_quota_usage(&single, quota_label_compact),
        ];
        let summary = candidates
            .into_iter()
            .find(|candidate| Line::from(candidate.as_str()).width() <= usize::from(max_width))
            .unwrap_or_default();
        return (summary, metric_group_level(&bounded));
    }

    // A provider can have a real local history while its first live quota is
    // unavailable (for example Grok before `grok login`). Keep the list useful
    // by falling back to the newest backed calendar period, while the detail
    // view still shows the honest "No data" quota row.
    let first = if metric_has_list_value(first) {
        first
    } else {
        ["Today", "Yesterday", "Last 30 Days"]
            .into_iter()
            .find_map(|label| {
                provider.metrics.iter().find(|metric| {
                    metric.label.eq_ignore_ascii_case(label) && metric_has_list_value(metric)
                })
            })
            .or_else(|| {
                provider
                    .metrics
                    .iter()
                    .find(|metric| metric_has_list_value(metric))
            })
            .unwrap_or(first)
    };
    let value = split_metric_value(&first.value).0;
    let label = display_metric_label(&first.label);
    let summary = if value.trim().is_empty() {
        label
    } else {
        format!("{label} {value}")
    };
    (summary, first.level)
}

fn metric_has_list_value(metric: &Metric) -> bool {
    let value = metric.value.trim();
    !value.is_empty() && !value.eq_ignore_ascii_case("no data")
}

fn format_quota_usage(metrics: &[&Metric], label: fn(&str) -> String) -> String {
    let values = metrics
        .iter()
        .filter_map(|metric| {
            metric
                .percent
                .map(|used| format!("{} {used:.0}%", label(&metric.label)))
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if values.is_empty() {
        String::new()
    } else {
        format!("{values} used")
    }
}

fn quota_label_verbose(label: &str) -> String {
    match label.to_ascii_lowercase().as_str() {
        "session" | "5h" | "5h block" => "5-hour".into(),
        "week" | "weekly" => "7-day".into(),
        "fable" => "Fable 7-day".into(),
        "sonnet" => "Sonnet 7-day".into(),
        _ => display_metric_label(label),
    }
}

fn quota_label_compact(label: &str) -> String {
    match label.to_ascii_lowercase().as_str() {
        "session" | "5h" | "5h block" => "5h".into(),
        "week" | "weekly" => "7d".into(),
        "fable" => "Fable".into(),
        "sonnet" => "Sonnet".into(),
        _ => display_metric_label(label),
    }
}

fn metric_group_level(metrics: &[&Metric]) -> Level {
    if metrics.iter().any(|metric| metric.level == Level::Alert) {
        Level::Alert
    } else if metrics.iter().any(|metric| metric.level == Level::Warn) {
        Level::Warn
    } else {
        Level::Ok
    }
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
    let back_style = Style::default()
        .fg(palette.primary)
        .add_modifier(Modifier::BOLD);
    let padding = SELECTABLE_LEFT_PADDING.min(back_area.width);
    frame.render_widget(
        Paragraph::new("← Back").style(back_style),
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
    if matches!(
        provider.kind,
        ProviderKind::CurrentProject | ProviderKind::Total
    ) {
        let project_rows = if provider.kind == ProviderKind::Total {
            u16::try_from(current_project_rows(provider).len().max(1)).unwrap_or(u16::MAX)
        } else {
            0
        };
        let mut height = 1u16.saturating_add(METRIC_GAP);
        if provider.kind == ProviderKind::Total {
            height = height
                .saturating_add(1)
                .saturating_add(project_rows)
                .saturating_add(METRIC_GAP);
        }
        height = height
            .saturating_add(1)
            .saturating_add(u16::try_from(provider.monthly_tokens.len()).unwrap_or(u16::MAX));
        let metadata_rows = u16::try_from(provider.detail.len())
            .unwrap_or(u16::MAX)
            .saturating_add(u16::from(provider.as_of.is_some()));
        return height.saturating_add(if metadata_rows > 0 {
            METRIC_GAP.saturating_add(metadata_rows)
        } else {
            0
        });
    }
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
    if matches!(
        provider.kind,
        ProviderKind::CurrentProject | ProviderKind::Total
    ) {
        render_usage_summary(buffer, area, provider, palette, y);
        return;
    }
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

fn render_usage_summary(
    buffer: &mut Buffer,
    area: Rect,
    provider: &Provider,
    palette: &ui::Palette,
    mut y: u16,
) {
    y = y.saturating_add(METRIC_GAP);
    if y >= area.bottom() {
        return;
    }
    if provider.kind == ProviderKind::Total {
        render_split_row(
            buffer,
            row_at(area, y),
            "Project",
            "This month",
            Style::default()
                .fg(palette.muted)
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(palette.muted)
                .add_modifier(Modifier::BOLD),
        );
        y = y.saturating_add(1);
        let rows = current_project_rows(provider);
        if rows.is_empty() {
            render_muted_row(buffer, row_at(area, y), "No project data", palette);
            y = y.saturating_add(1);
        } else {
            for (path, tokens) in rows {
                if y >= area.bottom() {
                    return;
                }
                render_split_row(
                    buffer,
                    row_at(area, y),
                    &project_label(path),
                    &format_exact_tokens(tokens),
                    Style::default().fg(palette.primary),
                    Style::default().fg(palette.header),
                );
                y = y.saturating_add(1);
            }
        }
        y = y.saturating_add(METRIC_GAP);
        if y >= area.bottom() {
            return;
        }
    }
    render_split_row(
        buffer,
        row_at(area, y),
        "Month",
        "Tokens",
        Style::default()
            .fg(palette.muted)
            .add_modifier(Modifier::BOLD),
        Style::default()
            .fg(palette.muted)
            .add_modifier(Modifier::BOLD),
    );
    y = y.saturating_add(1);
    for (index, usage) in provider.monthly_tokens.iter().enumerate() {
        if y >= area.bottom() {
            return;
        }
        let current = index == 0;
        let label = month_label(usage.month, usage.year, current);
        let value = if usage.tokens == 0 {
            "—".into()
        } else {
            format_exact_tokens(usage.tokens)
        };
        let label_style = if current {
            Style::default()
                .fg(palette.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.primary)
        };
        let value_style = if current {
            Style::default()
                .fg(palette.header)
                .add_modifier(Modifier::BOLD)
        } else if usage.tokens == 0 {
            Style::default().fg(palette.muted)
        } else {
            Style::default().fg(palette.header)
        };
        render_split_row(
            buffer,
            row_at(area, y),
            &label,
            &value,
            label_style,
            value_style,
        );
        y = y.saturating_add(1);
    }
    let has_metadata = !provider.detail.is_empty() || provider.as_of.is_some();
    if has_metadata {
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

fn current_project_rows(provider: &Provider) -> Vec<(&Path, u64)> {
    let Some(current) = provider.monthly_tokens.first() else {
        return Vec::new();
    };
    let mut rows: Vec<(&Path, u64)> = provider
        .project_usage
        .iter()
        .filter_map(|project| {
            let tokens = project
                .monthly_tokens
                .iter()
                .find(|usage| usage.year == current.year && usage.month == current.month)?
                .tokens;
            (tokens > 0).then_some((project.path.as_path(), tokens))
        })
        .collect();
    rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    rows
}

fn project_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_str().unwrap_or("project"))
        .to_string()
}

fn month_label(month: u8, year: i32, current: bool) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let name = month
        .checked_sub(1)
        .and_then(|index| MONTHS.get(usize::from(index)))
        .copied()
        .unwrap_or("Unknown");
    if current {
        format!("{name} {year} · current")
    } else {
        format!("{name} {year}")
    }
}

fn format_exact_tokens(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
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

fn display_provider_list_name(name: &str) -> String {
    let display = display_provider_name(name);
    display
        .split_once(" · ")
        .filter(|(_, suffix)| suffix.contains('@'))
        .map(|(provider, _)| provider.to_string())
        .unwrap_or(display)
}

fn display_provider_list_badge(badge: &str) -> Option<String> {
    (!badge.is_empty() && !badge.contains('@')).then(|| display_badge(badge))
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
    let sparkline = metric_sparkline("terminal-metric-sparkline", metric);
    if chart_is_inline(metric) {
        render_label_value_row(buffer, row_at(area, area.y), metric, palette);
        let chart_width = u16::try_from(metric.spark.len())
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(18));
        if chart_width > 0 {
            let chart_area = Rect::new(
                area.right().saturating_sub(chart_width),
                area.y,
                chart_width,
                1,
            );
            sparkline
                .widget()
                .style(Style::default().fg(level_color(palette, metric.level)))
                .render(chart_area, buffer);
        }
    } else {
        render_label_value_row(buffer, row_at(area, area.y), metric, palette);
        if area.height >= 2 {
            sparkline
                .widget()
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
        "week" | "weekly" => "7-day limit".into(),
        "session" | "5h" | "5h block" => "5-hour limit".into(),
        "fable" => "Fable 7-day limit".into(),
        "sonnet" => "Sonnet 7-day limit".into(),
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
    use ratatui::widgets::Sparkline as RatatuiSparkline;
    use ratatui::Terminal;

    fn sample() -> Snapshot {
        let mut codex_week = Metric::new("week", "3% · resets 6d 18h".into(), Level::Ok);
        codex_week.percent = Some(3.0);
        let block = Metric::used_percent("Session", 32.0, "32% · resets 1h 20m".into(), Level::Ok);
        let weekly = Metric::used_percent("Weekly", 44.0, "44% · resets 3d 2h".into(), Level::Ok);
        let fable = Metric::used_percent("Fable", 11.0, "11% · resets 3d 2h".into(), Level::Ok);
        let mut day = Metric::new("24h", "$8.91 est · 1.2M tok".into(), Level::Ok);
        day.spark = (0..24).map(|hour| (hour % 5) as f64).collect();
        let claude = |name: &str, badge: &str| Provider {
            kind: ProviderKind::Claude,
            name: name.into(),
            badge: badge.into(),
            present: true,
            metrics: vec![
                block.clone(),
                weekly.clone(),
                fable.clone(),
                Metric::new("burn", "$1.20/hr est".into(), Level::Ok),
                day.clone(),
            ],
            detail: vec![("sonnet-4-6".into(), "$5.12 · 800k tok".into())],
            as_of: Some(now_epoch_secs() - 120),
            alert: None,
            status_fragment: None,
            day_usd: Some(8.91),
            monthly_tokens: Vec::new(),
            project_usage: Vec::new(),
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
                    monthly_tokens: Vec::new(),
                    project_usage: Vec::new(),
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
    ) -> (String, RenderResult, Buffer) {
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
        (screen, rendered, buffer)
    }

    #[test]
    fn semantic_page_preserves_provider_master_detail_and_actions() {
        let snapshot = sample();
        let mut view = View {
            selected: 0,
            detail_open: false,
            scanning: false,
            hosted: false,
            alerts: Alerts::default(),
            alert_dialog: None,
            scroll_offset: 0,
            reveal_selected: true,
        };
        let catalog = semantic_page(Some(&snapshot), &view);
        catalog.validate().unwrap();
        assert_eq!(
            catalog.list().items[0].activate.as_deref(),
            Some(OPEN_PROVIDER_ACTION)
        );
        assert_eq!(catalog.list().selected_id.as_deref(), Some("provider-0"));
        assert_eq!(
            catalog.list().select.as_deref(),
            Some(SELECT_PROVIDER_ACTION)
        );
        assert!(matches!(
            catalog.list().items[0].accessory,
            Some(ListItemSlot::Badge(_))
        ));
        assert!(catalog.list().items[0].value.is_some());
        assert_eq!(provider_index_from_node_id("provider-2"), Some(2));

        view.detail_open = true;
        let detail = semantic_page(Some(&snapshot), &view);
        detail.validate().unwrap();
        assert_eq!(detail.back.as_deref(), Some(CLOSE_PROVIDER_ACTION));
        assert!(detail
            .list()
            .items
            .iter()
            .any(|item| item.label == "7-day limit"));
        assert!(detail
            .list()
            .items
            .iter()
            .any(|item| item.activate.as_deref() == Some(REFRESH_ACTION)));
        view.selected = snapshot
            .providers
            .iter()
            .position(|provider| {
                provider
                    .metrics
                    .iter()
                    .any(|metric| !metric.spark.is_empty())
            })
            .expect("sample must include history");
        let history_detail = semantic_page(Some(&snapshot), &view);
        let history = history_detail
            .list()
            .items
            .iter()
            .find_map(|item| match item.trailing.as_ref() {
                Some(ListItemSlot::Sparkline(sparkline)) => Some(sparkline),
                _ => None,
            })
            .expect("history must enter the semantic tree");
        assert_eq!(history.values().count(), 24);
        assert!(history.accessibility_text.contains("24 points"));
    }

    #[test]
    fn app_kit_sparkline_is_buffer_identical_to_the_frozen_usage_graph() {
        let values: [f64; 7] = [0.0, 1.0, 4.0, 2.0, 3.0, 8.0, 1.0];
        let data = values
            .iter()
            .map(|value| ((*value).max(0.0) * 1_000.0).round() as u64)
            .collect::<Vec<_>>();
        let metric = Metric {
            spark: values.to_vec(),
            ..Metric::new("Usage Trend", String::new(), Level::Ok)
        };
        let sparkline = metric_sparkline("usage-trend-test", &metric);
        let style = Style::default().fg(ui::Palette::DARK.meter_blue);

        for width in [1, 3, 7] {
            let area = Rect::new(0, 0, width, 1);
            let start = data.len().saturating_sub(usize::from(width));
            let mut legacy = Buffer::empty(area);
            RatatuiSparkline::default()
                .data(&data[start..])
                .style(style)
                .render(area, &mut legacy);

            let mut component = Buffer::empty(area);
            sparkline.widget().style(style).render(area, &mut component);
            assert_eq!(component, legacy, "width {width}");
        }
    }

    #[test]
    fn semantic_alerts_page_exposes_native_toggles_and_back_navigation() {
        let view = View {
            selected: 0,
            detail_open: false,
            scanning: false,
            hosted: true,
            alerts: Alerts {
                close_to_limit: true,
                ..Alerts::default()
            },
            alert_dialog: Some(1),
            scroll_offset: 0,
            reveal_selected: true,
        };
        let page = semantic_page(Some(&sample()), &view);
        page.validate().unwrap();
        assert_eq!(page.title, "Alerts");
        assert_eq!(page.back.as_deref(), Some(CLOSE_ALERTS_ACTION));
        assert_eq!(page.list().selected_id.as_deref(), Some("alert-1"));
        let ListItemSlot::Toggle(toggle) = page.list().items[0].trailing.as_ref().unwrap() else {
            panic!("alert row must contain Toggle");
        };
        assert!(toggle.value);
        assert_eq!(toggle.set_value, SET_ALERT_ACTION);
    }

    #[test]
    fn app_kit_list_matches_the_frozen_usage_renderer_buffer_for_buffer() {
        let snapshot = sample();
        let cases = [
            (18, 1, 0, 0, true),
            (24, 2, 2, 0, true),
            (40, 2, 1, 1, false),
            (72, 8, 2, 0, true),
        ];
        for palette in [ui::Palette::ADAPTIVE, ui::Palette::LIGHT, ui::Palette::DARK] {
            for (width, height, selected, scroll_offset, reveal_selected) in cases {
                let view = View {
                    selected,
                    detail_open: false,
                    scanning: false,
                    hosted: false,
                    alerts: Alerts::default(),
                    alert_dialog: None,
                    scroll_offset,
                    reveal_selected,
                };
                let mut current = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut legacy = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut current_result = RenderResult::default();
                let mut legacy_result = RenderResult::default();
                current
                    .draw(|frame| {
                        current_result = render_provider_list(
                            frame,
                            frame.area(),
                            &snapshot.providers,
                            &view,
                            &palette,
                        );
                    })
                    .unwrap();
                legacy
                    .draw(|frame| {
                        legacy_result = render_provider_list_legacy(
                            frame,
                            frame.area(),
                            &snapshot.providers,
                            &view,
                            &palette,
                        );
                    })
                    .unwrap();
                assert_eq!(
                    current.backend().buffer(),
                    legacy.backend().buffer(),
                    "{width}x{height}, selected {selected}, palette {:?}",
                    palette.mode
                );
                assert_eq!(current_result.hits, legacy_result.hits);
                assert_eq!(current_result.scroll_offset, legacy_result.scroll_offset);
                assert_eq!(current_result.max_scroll, legacy_result.max_scroll);
                assert_eq!(
                    current_result.viewport_height,
                    legacy_result.viewport_height
                );
                assert_eq!(current_result.scrollbar_area, legacy_result.scrollbar_area);
            }
        }
    }

    fn render_with(
        width: u16,
        height: u16,
        selected: usize,
        detail_open: bool,
    ) -> (String, Vec<Hit>) {
        let (screen, rendered, _) = render_state(width, height, selected, detail_open, 0, true);
        (screen, rendered.hits)
    }

    fn render(width: u16, height: u16, detail_open: bool) -> (String, Vec<Hit>) {
        render_with(width, height, 1, detail_open)
    }

    fn render_footer_state(hosted: bool, scanning: bool) -> (String, Buffer) {
        let width = 48;
        let height = 6;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let snapshot = sample();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    Some(&snapshot),
                    &View {
                        selected: 0,
                        detail_open: false,
                        scanning,
                        hosted,
                        alerts: Alerts::default(),
                        alert_dialog: None,
                        scroll_offset: 0,
                        reveal_selected: true,
                    },
                    &ui::Palette::DARK,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let row = (0..width)
            .map(|x| buffer[(x, height - 1)].symbol().to_string())
            .collect::<String>();
        (row, buffer)
    }

    #[test]
    fn total_usage_opens_as_an_exact_monthly_token_table() {
        let snapshot = Snapshot {
            providers: vec![Provider {
                kind: ProviderKind::Total,
                name: "Total usage".into(),
                badge: String::new(),
                present: true,
                metrics: vec![Metric::new("This month", "1.2M tokens".into(), Level::Ok)],
                detail: Vec::new(),
                as_of: None,
                alert: None,
                status_fragment: None,
                day_usd: None,
                monthly_tokens: vec![
                    crate::sources::MonthUsage {
                        year: 2026,
                        month: 8,
                        tokens: 1_234_567,
                    },
                    crate::sources::MonthUsage {
                        year: 2026,
                        month: 7,
                        tokens: 0,
                    },
                ],
                project_usage: vec![crate::sources::ProjectUsage {
                    path: "/work/unpeel".into(),
                    monthly_tokens: vec![crate::sources::MonthUsage {
                        year: 2026,
                        month: 8,
                        tokens: 1_000_000,
                    }],
                }],
            }],
        };
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
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
                    &ui::Palette::DARK,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..12)
            .map(|y| {
                (0..60)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "← Back",
            "Total usage",
            "Project",
            "This month",
            "unpeel",
            "1,000,000",
            "Month",
            "Tokens",
            "August 2026 · current",
            "1,234,567",
            "July 2026",
            "—",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}\n{screen}");
        }
    }

    #[test]
    fn current_project_uses_the_same_monthly_detail_without_a_project_table() {
        let snapshot = Snapshot {
            providers: vec![Provider {
                kind: ProviderKind::CurrentProject,
                name: "Current project".into(),
                badge: "unpeel".into(),
                present: true,
                metrics: vec![Metric::new("This month", "42k tokens".into(), Level::Ok)],
                detail: vec![("path".into(), "/work/unpeel".into())],
                as_of: None,
                alert: None,
                status_fragment: None,
                day_usd: None,
                monthly_tokens: vec![crate::sources::MonthUsage {
                    year: 2026,
                    month: 8,
                    tokens: 42_000,
                }],
                project_usage: Vec::new(),
            }],
        };
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
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
                    &ui::Palette::DARK,
                );
            })
            .unwrap();
        let screen = (0..10)
            .map(|y| {
                (0..60)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Current project Unpeel"), "{screen}");
        assert!(screen.contains("August 2026 · current"), "{screen}");
        assert!(screen.contains("42,000"), "{screen}");
        assert!(
            !screen.contains("Project               This month"),
            "{screen}"
        );
    }

    #[test]
    fn footer_shows_actions_and_replaces_refresh_with_a_spinner_while_scanning() {
        let (idle, buffer) = render_footer_state(true, false);
        assert!(
            idle.starts_with("  a alert  r refresh"),
            "idle footer\n{idle}"
        );
        assert_eq!(buffer[(2, 5)].fg, ui::Palette::DARK.primary);
        assert_eq!(buffer[(4, 5)].fg, ui::Palette::DARK.muted);
        assert_eq!(buffer[(11, 5)].fg, ui::Palette::DARK.primary);
        assert_eq!(buffer[(13, 5)].fg, ui::Palette::DARK.muted);

        let (scanning, _) = render_footer_state(true, true);
        assert!(
            scanning.starts_with("  a alert  r "),
            "scan footer\n{scanning}"
        );
        assert!(scanning.contains(" refreshing…"), "scan footer\n{scanning}");
        assert!(!scanning.contains("r refresh"), "scan footer\n{scanning}");
    }

    #[test]
    fn default_view_is_a_compact_explorer_style_list() {
        let (screen, hits) = render(72, 12, false);
        assert!(
            screen
                .lines()
                .next()
                .is_some_and(|line| line.starts_with("  Codex Pro")),
            "content starts with the two-cell-inset provider list\n{screen}"
        );
        assert!(screen.contains("Codex Pro"), "provider and badge\n{screen}");
        assert!(
            !screen.contains("tommy@uxthemes.com") && !screen.contains("work@uxthemes.com"),
            "emails stay out of the main list\n{screen}"
        );
        assert!(screen.contains("Claude · work"), "second account\n{screen}");
        assert!(
            screen.contains("7-day 3% used"),
            "clear Codex quota data\n{screen}"
        );
        assert!(
            screen.contains("5-hour 32% · 7-day 44% · Fable 7-day 11% used"),
            "clear Claude quota data including Fable\n{screen}"
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
    fn list_uses_local_history_when_the_live_quota_has_no_data() {
        let provider = Provider {
            kind: ProviderKind::Grok,
            name: "Grok".into(),
            badge: String::new(),
            present: true,
            metrics: vec![
                Metric::new("Weekly", "No data".into(), Level::Ok),
                Metric::new("Today", "No data".into(), Level::Ok),
                Metric::new("Yesterday", "No data".into(), Level::Ok),
                Metric::new("Last 30 Days", "$84.02 · 398.7M tokens".into(), Level::Ok),
            ],
            detail: Vec::new(),
            as_of: None,
            alert: None,
            status_fragment: None,
            day_usd: None,
            monthly_tokens: Vec::new(),
            project_usage: Vec::new(),
        };

        assert_eq!(
            provider_basic_data(&provider, 40),
            ("Last 30 Days $84.02".into(), Level::Ok)
        );
    }

    #[test]
    fn narrow_list_prioritizes_a_spelled_out_fable_reading() {
        let (screen, _) = render(38, 12, false);
        assert!(screen.contains("Fable 11% used"), "Fable quota\n{screen}");
        assert!(!screen.contains('@'), "email hidden\n{screen}");
    }

    #[test]
    fn account_email_remains_available_in_detail() {
        let (screen, _) = render(72, 30, true);
        assert!(
            screen.contains("tommy@uxthemes.com"),
            "account detail\n{screen}"
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
                monthly_tokens: Vec::new(),
                project_usage: Vec::new(),
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
            "5-hour limit",
            "39% used",
            "7-day limit",
            "~2% spare",
            "Fable 7-day limit",
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
                (0..width).all(|x| buffer[(x, 0)].bg == expected_background),
                "selection should paint the complete row"
            );
            assert_eq!(buffer[(0, 1)].bg, Color::Reset, "unselected row");
            assert_eq!(buffer[(0, 0)].symbol(), " ");
            assert_eq!(buffer[(1, 0)].symbol(), " ");
            assert_eq!(buffer[(2, 0)].symbol(), "C", "two-cell label inset");
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
        let (screen, hits) = render_with(72, 3, 2, false);
        assert_eq!(hits.first().map(|hit| hit.index), Some(1));
        assert_eq!(hits.last().map(|hit| hit.index), Some(2));
        assert!(screen.contains("Claude · work"), "selected row\n{screen}");
        assert!(screen.contains('┃'), "scrollbar thumb\n{screen}");
        for hit in &hits {
            assert!(
                hit.bottom < 2,
                "hit leaves the viewport: {}..{}",
                hit.top,
                hit.bottom
            );
        }
    }

    #[test]
    fn scrollbar_reaches_the_exact_top_and_bottom_rows() {
        let (top_screen, top, _) = render_state(72, 2, 0, false, 0, false);
        let area = top.scrollbar_area.expect("top scrollbar");
        let top_rows: Vec<&str> = top_screen.lines().collect();
        assert_eq!(
            top_rows[area.y as usize].chars().nth(area.x as usize),
            Some('┃'),
            "thumb should start at the first track row\n{top_screen}"
        );

        let (bottom_screen, bottom, _) = render_state(72, 2, 2, false, u16::MAX, false);
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
        let (screen, rendered, _) = render_state(72, 3, 2, false, u16::MAX, false);
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rendered.scroll_offset, rendered.max_scroll);
        assert_eq!(rendered.hits.last().map(|hit| hit.index), Some(2));
        assert!(
            rows[1].contains("Claude · work"),
            "last row should touch the bottom of the list viewport\n{screen}"
        );
        assert!(rows[0].contains("Claude"), "preceding row\n{screen}");
    }

    #[test]
    fn scrollbar_thumb_is_proportional_to_visible_content() {
        let (screen, rendered, _) = render_state(72, 15, 1, true, 0, false);
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
    fn detail_has_a_transparent_back_action_with_a_full_width_hit_target() {
        let (screen, rendered, buffer) = render_state(72, 10, 1, true, u16::MAX, false);
        let back = rendered.back_button.expect("back hit");
        assert_eq!(back.left, 0);
        assert_eq!(back.right, 71);
        assert_eq!(back.top, 0);
        assert_eq!(back.bottom, 0);
        assert!(screen.lines().next().unwrap().contains("  ← Back"));
        assert!(
            (0..72).all(|x| buffer[(x, 0)].bg == Color::Reset),
            "Back should not paint a row background"
        );
        assert!(rendered.hits.is_empty(), "detail has no provider-row hits");
    }

    #[test]
    fn hosted_alerts_open_as_a_ratatui_dialog() {
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
        ] {
            assert!(screen.contains(expected), "missing {expected:?}\n{screen}");
        }
        assert!(!screen.contains("Esc close"), "dialog has no shortcut help");
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
        let (screen, rendered, _) = render_state(72, 24, 0, false, 0, true);
        assert!(rendered.alert_option_hits.is_empty());
        assert!(!screen.contains("alerts off"));
        assert!(!screen.contains("a alerts"));
        assert!(!screen.contains("a alert"));
        assert!(screen.contains("r refresh"));
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
