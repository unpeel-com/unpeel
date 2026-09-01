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
#[cfg(test)]
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
#[cfg(test)]
use ratatui::layout::{Alignment, Constraint, Layout};
#[cfg(test)]
use ratatui::style::Color;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
#[cfg(test)]
use ratatui::text::Span;
#[cfg(test)]
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Frame;
use std::path::Path;
#[cfg(test)]
use unpeel_app_kit::SelectableRow;
#[cfg(test)]
use unpeel_app_kit::VerticalScrollbar;
use unpeel_app_kit::{
    Badge, FooterAction, Gauge, InputField, KitTheme, List, ListItem, ListItemEmphasis,
    ListItemSlot, ListItemTone, ListPageBehavior, ListState, Page, PageTheme, Sparkline,
    TerminalPointerState, Toggle, UiComponent, UiNode, SELECTABLE_LEFT_PADDING,
};

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

/// Authoritative master/detail component tree interpreted by Ratatui, SwiftUI,
/// and web as peers. Bounded quotas are semantic Gauge slots with App-owned
/// direction/copy, while numeric history is a Sparkline over the same values.
pub fn semantic_page(snapshot: Option<&Snapshot>, view: &View) -> Page {
    if view.hosted && view.alert_dialog.is_some() {
        return semantic_alerts(view);
    }
    let Some(snapshot) = snapshot else {
        return Page::new(
            "Usage",
            List::new("usage-providers", Vec::new()).empty_message("Scanning local usage…"),
        )
        .footer_actions(usage_footer_actions(view.scanning, view.hosted));
    };
    if view.detail_open && !snapshot.providers.is_empty() {
        let index = view.selected.min(snapshot.providers.len() - 1);
        return semantic_provider_detail(
            &snapshot.providers[index],
            index,
            view.scanning,
            view.hosted,
        );
    }

    let items = snapshot
        .providers
        .iter()
        .enumerate()
        .map(|(index, provider)| provider_list_item(provider, index, 96))
        .collect::<Vec<_>>();
    let mut list = List::new("usage-providers", items)
        .empty_message("No local usage data")
        .page_behavior(ListPageBehavior::Scroll);
    if snapshot.providers.get(view.selected).is_some() {
        list = list.selected(provider_node_id(view.selected), SELECT_PROVIDER_ACTION);
    }
    Page::new("Usage", list).footer_actions(usage_footer_actions(view.scanning, view.hosted))
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

fn semantic_provider_detail(
    provider: &Provider,
    index: usize,
    scanning: bool,
    hosted: bool,
) -> Page {
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
        let presentation = metric_presentation(metric);
        let mut item = ListItem::new(
            format!("{prefix}-metric-{metric_index}"),
            display_metric_label(&metric.label),
        );
        if let Some(ratio) = presentation.ratio {
            item = item
                .trailing(ListItemSlot::gauge(metric_gauge(
                    format!("{prefix}-metric-{metric_index}-gauge"),
                    metric,
                    ratio,
                    &presentation.caption,
                )))
                .value_tone(metric_tone(metric.level));
        } else if !metric.spark.is_empty() {
            item = item
                .trailing(ListItemSlot::sparkline(metric_sparkline(
                    format!("{prefix}-metric-{metric_index}-sparkline"),
                    metric,
                )))
                .value_tone(metric_tone(metric.level));
            if !presentation.caption.is_empty() {
                item = item.value(presentation.caption);
            }
        } else if !presentation.caption.is_empty() {
            item = item.value(presentation.caption);
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
    let title = if provider.badge.is_empty() {
        display_provider_name(&provider.name)
    } else {
        format!(
            "{} · {}",
            display_provider_name(&provider.name),
            display_badge(&provider.badge)
        )
    };
    Page::new(title, List::new("usage-detail", items))
        .back_action(CLOSE_PROVIDER_ACTION)
        .footer_actions(usage_footer_actions(scanning, hosted))
}

fn usage_footer_actions(scanning: bool, hosted: bool) -> Vec<FooterAction> {
    let mut actions = Vec::with_capacity(usize::from(hosted) + 1);
    if hosted {
        actions
            .push(FooterAction::new("open-alerts", "alert", OPEN_ALERTS_ACTION).accelerator("a"));
    }
    actions.push(
        FooterAction::new(
            "refresh-usage",
            if scanning { "refreshing…" } else { "refresh" },
            REFRESH_ACTION,
        )
        .accelerator("r")
        .disabled(scanning),
    );
    actions
}

/// Canonical bounded-metric presentation shared by every renderer.
///
/// Provider APIs report a used percentage. `PercentDisplay` is App-owned
/// product intent, so the used/remaining transform happens here before the
/// component tree is published. Ratatui, Swift, and web receive the same
/// caption and must not infer the opposite direction.
#[derive(Clone, Debug, PartialEq)]
struct MetricPresentation {
    caption: String,
    ratio: Option<f64>,
}

fn metric_presentation(metric: &Metric) -> MetricPresentation {
    let Some(used_percent) = metric.percent else {
        return MetricPresentation {
            caption: metric.value.clone(),
            ratio: None,
        };
    };
    let used_ratio = (used_percent / 100.0).clamp(0.0, 1.0);
    let (ratio, headline) = match metric.percent_display {
        PercentDisplay::Remaining => {
            let remaining = 1.0 - used_ratio;
            (remaining, format!("{:.0}% left", remaining * 100.0))
        }
        PercentDisplay::Used => (used_ratio, format!("{used_percent:.0}% used")),
    };
    let (_, context) = split_metric_value(&metric.value);
    MetricPresentation {
        caption: context.map_or(headline.clone(), |context| {
            format!("{headline} · {context}")
        }),
        ratio: Some(ratio),
    }
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

fn metric_gauge(id: impl Into<String>, metric: &Metric, ratio: f64, caption: &str) -> Gauge {
    let label = display_metric_label(&metric.label);
    Gauge::new(id, ratio, label.clone(), format!("{label}: {caption}")).caption(caption)
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
    pub footer_area: Option<Rect>,
}

/// One selectable row's clickable screen rectangle, inclusive on every edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub node_id: String,
    pub left: u16,
    pub right: u16,
    pub top: u16,
    pub bottom: u16,
}

impl Hit {
    pub fn contains(&self, column: u16, row: u16) -> bool {
        (self.left..=self.right).contains(&column) && (self.top..=self.bottom).contains(&row)
    }

    fn from_rect(index: usize, node_id: impl Into<String>, area: Rect) -> Option<Self> {
        (!area.is_empty()).then_some(Self {
            index,
            node_id: node_id.into(),
            left: area.x,
            right: area.right().saturating_sub(1),
            top: area.y,
            bottom: area.bottom().saturating_sub(1),
        })
    }
}

#[cfg(test)]
pub fn draw_node(
    frame: &mut Frame,
    node: &UiNode,
    view: &View,
    palette: &ui::Palette,
) -> RenderResult {
    draw_node_with_pointer(frame, node, view, palette, TerminalPointerState::new())
}

pub fn draw_node_with_pointer(
    frame: &mut Frame,
    node: &UiNode,
    view: &View,
    palette: &ui::Palette,
    pointer: TerminalPointerState,
) -> RenderResult {
    let UiComponent::Page(page) = &node.element else {
        return RenderResult::default();
    };
    let list = page.list();
    let selected = list
        .selected_id
        .as_deref()
        .and_then(|selected| list.items.iter().position(|item| item.id == selected));
    let mut state = ListState::new(selected);
    state.set_pointer(pointer);
    state.set_offset(usize::from(view.scroll_offset), list.items.len());
    if view.reveal_selected {
        state.request_reveal();
    }
    let mut input = InputField::new("");
    frame.render_widget(
        page.widget(&mut input, &mut state)
            .theme(provider_list_theme(palette)),
        frame.area(),
    );

    let rows_area = state.rows_area();
    let hits: Vec<Hit> = (0..usize::from(rows_area.height))
        .filter_map(|row| {
            let index = state.offset().saturating_add(row);
            let item = list.items.get(index)?;
            Hit::from_rect(
                index,
                item.id.clone(),
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
    let scrollbar_area = (list.items.len() > usize::from(rows_area.height)
        && frame.area().width > 1)
        .then(|| Rect::new(rows_area.right(), rows_area.y, 1, rows_area.height));
    let back_button = page.back.as_ref().and_then(|_| {
        Hit::from_rect(
            usize::MAX,
            node.id.as_str().to_owned(),
            page.layout(frame.area()).title,
        )
    });
    let footer_area = page.layout(frame.area()).footer;
    let alert_option_hits = if view.alert_dialog.is_some() {
        hits.clone()
    } else {
        Vec::new()
    };
    RenderResult {
        hits,
        scroll_offset: u16::try_from(state.offset()).unwrap_or(u16::MAX),
        max_scroll: u16::try_from(state.max_offset(list.items.len())).unwrap_or(u16::MAX),
        viewport_height: rows_area.height,
        scrollbar_area,
        back_button,
        alert_option_hits,
        footer_area,
    }
}

#[cfg(test)]
fn draw(
    frame: &mut Frame,
    snapshot: Option<&Snapshot>,
    view: &View,
    palette: &ui::Palette,
) -> RenderResult {
    let node = UiNode::page(SEMANTIC_ROOT_ID, semantic_page(snapshot, view));
    draw_node(frame, &node, view, palette)
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

// Direct List parity harness retained only for the frozen pre-migration
// buffer test. Runtime screens render the published Page through `draw_node`.
#[cfg(test)]
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
            providers.get(index)?;
            Hit::from_rect(
                index,
                provider_node_id(index),
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
        if let Some(hit) = Hit::from_rect(index, provider_node_id(index), row_area) {
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

#[cfg(test)]
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
        let quota = detail
            .list()
            .items
            .iter()
            .find_map(|item| match item.trailing.as_ref() {
                Some(ListItemSlot::Gauge(gauge)) => Some(gauge),
                _ => None,
            })
            .expect("bounded quota must enter the semantic tree as a Gauge");
        assert_eq!(quota.value_label(), "97% left · Resets in 6d 18h");
        assert!(detail
            .footer
            .actions
            .iter()
            .any(|item| item.action == REFRESH_ACTION));
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
        let height = 8;
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
        let screen = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (screen, buffer)
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
            "‹  Total usage",
            "This month",
            "unpeel",
            "1,000,000",
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
        assert!(screen.contains("Current project · Unpeel"), "{screen}");
        assert!(screen.contains("August 2026 · current"), "{screen}");
        assert!(screen.contains("42,000"), "{screen}");
        assert!(
            !screen.contains("Project               This month"),
            "{screen}"
        );
    }

    #[test]
    fn footer_shows_actions_and_disables_refresh_while_scanning() {
        let (idle, _) = render_footer_state(true, false);
        assert!(idle.contains("a alert"), "idle component tree\n{idle}");
        assert!(idle.contains("r refresh"), "idle component tree\n{idle}");

        let (scanning, _) = render_footer_state(true, true);
        assert!(
            scanning.contains("refreshing…"),
            "scan component tree\n{scanning}"
        );
        assert!(
            !scanning.contains("r refresh  "),
            "scan component tree\n{scanning}"
        );
    }

    #[test]
    fn default_view_is_a_compact_explorer_style_list() {
        let (screen, hits) = render(72, 12, false);
        assert!(screen
            .lines()
            .next()
            .is_some_and(|line| line.contains("Usage")));
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
        let snapshot = sample();
        let page = semantic_page(
            Some(&snapshot),
            &View {
                selected: 0,
                detail_open: false,
                scanning: false,
                hosted: false,
                alerts: Alerts::default(),
                alert_dialog: None,
                scroll_offset: 0,
                reveal_selected: true,
            },
        );
        assert!(page.list().items[1]
            .value
            .as_deref()
            .is_some_and(|value| value.contains("Fable 7-day 11% used")));
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
            "‹  Claude · Team 5x",
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
    fn remaining_quota_is_transformed_once_before_any_renderer_sees_it() {
        let mut metric =
            Metric::used_percent("7-day limit", 23.0, "23% · resets 5d 14h".into(), Level::Ok);
        metric.percent_display = PercentDisplay::Remaining;
        let presentation = metric_presentation(&metric);
        assert_eq!(presentation.caption, "77% left · Resets in 5d 14h");
        assert!(presentation
            .ratio
            .is_some_and(|ratio| (ratio - 0.77).abs() < f64::EPSILON * 4.0));

        let mut snapshot = sample();
        snapshot.providers[0].metrics = vec![metric];
        let view = View {
            selected: 0,
            detail_open: true,
            scanning: false,
            hosted: false,
            alerts: Alerts::default(),
            alert_dialog: None,
            scroll_offset: 0,
            reveal_selected: true,
        };
        let node = UiNode::page(SEMANTIC_ROOT_ID, semantic_page(Some(&snapshot), &view));
        let UiComponent::Page(page) = &node.element else {
            unreachable!()
        };
        let Some(ListItemSlot::Gauge(gauge)) = page.list().items[0].trailing.as_ref() else {
            panic!("bounded quota must publish a Gauge");
        };
        assert_eq!(gauge.ratio.value(), 0.77);
        assert_eq!(gauge.value_label(), "77% left · Resets in 5d 14h");

        let mut terminal = Terminal::new(TestBackend::new(72, 7)).unwrap();
        terminal
            .draw(|frame| {
                draw_node(frame, &node, &view, &ui::Palette::DARK);
            })
            .unwrap();
        let screen = (0..7)
            .map(|y| {
                (0..72)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("77% left · Resets in 5d 14h"), "{screen}");
        let meter_row = (0..72)
            .map(|x| &terminal.backend().buffer()[(x, 2)])
            .collect::<Vec<_>>();
        assert!(
            meter_row.iter().any(|cell| {
                cell.symbol() == "─" && cell.fg == ui::Palette::DARK.meter_blue
            })
                && meter_row.iter().any(|cell| {
                    cell.symbol() == "─" && cell.fg == ui::Palette::DARK.muted
                }),
            "terminal must interpret the shared Gauge as distinct filled and remaining tracks\n{screen}"
        );
        assert!(
            !screen.contains("23% left"),
            "renderer inverted the canonical value\n{screen}"
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
        assert!(hits.last().unwrap().bottom < 12);
    }

    #[test]
    fn short_viewport_scrolls_selected_row_into_view() {
        let (screen, hits) = render_with(72, 4, 2, false);
        assert_eq!(hits.first().map(|hit| hit.index), Some(2));
        assert_eq!(hits.last().map(|hit| hit.index), Some(2));
        assert!(screen.contains("Claude · work"), "selected row\n{screen}");
        assert!(screen.contains('┃'), "scrollbar thumb\n{screen}");
        for hit in &hits {
            assert!(
                hit.bottom < 4,
                "hit leaves the viewport: {}..{}",
                hit.top,
                hit.bottom
            );
        }
    }

    #[test]
    fn scrollbar_reaches_the_exact_top_and_bottom_rows() {
        let (top_screen, top, _) = render_state(72, 4, 0, false, 0, false);
        let area = top.scrollbar_area.expect("top scrollbar");
        let top_rows: Vec<&str> = top_screen.lines().collect();
        assert_eq!(
            top_rows[area.y as usize].chars().nth(area.x as usize),
            Some('┃'),
            "thumb should start at the first track row\n{top_screen}"
        );

        let (bottom_screen, bottom, _) = render_state(72, 4, 2, false, u16::MAX, false);
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
        let (screen, rendered, _) = render_state(72, 4, 2, false, u16::MAX, false);
        let rows: Vec<&str> = screen.lines().collect();
        assert_eq!(rendered.scroll_offset, rendered.max_scroll);
        assert_eq!(rendered.hits.last().map(|hit| hit.index), Some(2));
        assert!(
            rows[2].contains("Claude · work"),
            "last row should touch the bottom of the list viewport\n{screen}"
        );
        assert!(rows[3].contains("r refresh"), "semantic footer\n{screen}");
    }

    #[test]
    fn scrollbar_thumb_is_proportional_to_visible_content() {
        let (screen, rendered, _) = render_state(72, 8, 1, true, 0, false);
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
        let first = &hits[0];
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
        assert_eq!(back.bottom, 1);
        assert!(screen.lines().next().unwrap().contains("  ‹  "));
        assert!(
            (0..72).all(|x| buffer[(x, 0)].bg == Color::Reset),
            "Back should not paint a row background"
        );
        assert!(
            rendered
                .hits
                .iter()
                .all(|hit| hit.node_id.starts_with("provider-1-")),
            "detail hits must come from the exact published detail items"
        );
    }

    #[test]
    fn hosted_alerts_render_the_exact_semantic_page() {
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
            "[x]",
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
        assert!(!screen.contains("Alerts"));
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
