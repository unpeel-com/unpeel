//! Drawing. The layout is deliberately narrow-first: every card degrades
//! gracefully down to sidebar-panel widths (~40 columns) — bars shrink
//! before text, and text truncates before wrapping.

use crate::sources::{Level, Metric, Provider, Snapshot};
use crate::timeparse::{compact_duration, now_epoch_secs};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use crate::theme as ui;

/// Dim bar track, one shade under MUTED so filled cells pop.
const TRACK: Color = Color::Rgb(58, 62, 74);

fn level_color(level: Level) -> Color {
    match level {
        Level::Ok => ui::FOCUS,
        Level::Warn => Color::Yellow,
        Level::Alert => ui::ATTENTION,
    }
}

pub struct View {
    pub selected: usize,
    pub expanded: bool,
    pub scanning: bool,
    pub alerts_enabled: bool,
}

pub fn draw(frame: &mut Frame, snapshot: Option<&Snapshot>, view: &View) {
    let area = frame.area();
    let mut lines: Vec<Line> = Vec::new();

    // Header: title left, state right.
    let mut header = vec![Span::styled(
        " Usage",
        Style::default().fg(ui::HEADER).add_modifier(Modifier::BOLD),
    )];
    let mut right = String::new();
    if !view.alerts_enabled {
        right.push_str("alerts off ");
    }
    if view.scanning {
        right.push_str(ui::spinner_frame());
        right.push(' ');
    }
    let used: usize = 6 + right.chars().count();
    let pad = (area.width as usize).saturating_sub(used);
    header.push(Span::raw(" ".repeat(pad)));
    header.push(Span::styled(right, Style::default().fg(ui::MUTED)));
    lines.push(Line::from(header));
    lines.push(rule(area.width));

    match snapshot {
        None => {
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{} scanning local usage…", ui::spinner_frame()),
                    Style::default().fg(ui::MUTED),
                ),
            ]));
        }
        Some(snapshot) => {
            for (index, provider) in snapshot.providers.iter().enumerate() {
                let selected = index == view.selected;
                push_card(
                    &mut lines,
                    provider,
                    selected,
                    selected && view.expanded,
                    area.width,
                );
            }
        }
    }

    // Footer pinned to the last row.
    let body_height = area.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(area.x, area.y, area.width, body_height),
    );
    // Sidebar-panel widths get the compact hint set.
    let hints = if area.width >= 58 {
        ui::hint_line(&[
            ("j/k", "select"),
            ("enter", "details"),
            ("r", "refresh"),
            ("a", "alerts"),
            ("q", "quit"),
        ])
    } else {
        ui::hint_line(&[("j/k", "select"), ("enter", "details"), ("q", "quit")])
    };
    frame.render_widget(
        Paragraph::new(hints),
        Rect::new(area.x, area.y + body_height, area.width, 1),
    );
}

fn rule(width: u16) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width as usize),
        Style::default().fg(TRACK),
    ))
}

fn push_card(
    lines: &mut Vec<Line<'static>>,
    provider: &Provider,
    selected: bool,
    expanded: bool,
    width: u16,
) {
    let width = width as usize;
    lines.push(Line::default());

    // Card title: selection bar, name, right-aligned badge.
    let marker = if selected { "▎" } else { " " };
    let name_style = if provider.alert.is_some() {
        Style::default().fg(ui::ATTENTION).add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(ui::HEADER).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ui::HEADER)
    };
    let name = if provider.alert.is_some() {
        format!("{} ⚠", provider.name)
    } else {
        provider.name.to_string()
    };
    let mut title = vec![
        Span::styled(marker.to_string(), Style::default().fg(ui::FOCUS)),
        Span::styled(name.clone(), name_style),
    ];
    if !provider.badge.is_empty() {
        let used = 1 + name.chars().count();
        let badge = &provider.badge;
        let pad = width.saturating_sub(used + badge.chars().count() + 1);
        title.push(Span::raw(" ".repeat(pad)));
        title.push(Span::styled(badge.clone(), Style::default().fg(ui::MUTED)));
    }
    lines.push(Line::from(title));

    if !provider.present {
        lines.push(muted_row(width, "not installed"));
        return;
    }
    if provider.metrics.is_empty() {
        lines.push(muted_row(width, "no recent activity"));
        return;
    }
    for metric in &provider.metrics {
        lines.push(metric_row(metric, width));
    }
    if expanded {
        for (key, value) in &provider.detail {
            lines.push(kv_row(key, value, width));
        }
        if let Some(as_of) = provider.as_of {
            let age = compact_duration(now_epoch_secs() - as_of);
            lines.push(kv_row("updated", &format!("{age} ago"), width));
        }
    }
}

fn muted_row(width: usize, text: &str) -> Line<'static> {
    let _ = width;
    Line::from(vec![
        Span::raw("   "),
        Span::styled(text.to_string(), Style::default().fg(ui::MUTED)),
    ])
}

/// `   label  ▐███░░░░▌  value` — or without the bar when there is no
/// percentage or no room for a meaningful one.
fn metric_row(metric: &Metric, width: usize) -> Line<'static> {
    const LABEL_WIDTH: usize = 9;
    let color = level_color(metric.level);
    let label = format!("   {:<LABEL_WIDTH$}", truncate(&metric.label, LABEL_WIDTH));
    let value = &metric.value;
    let fixed = label.chars().count() + 2 + value.chars().count();
    let bar_room = width.saturating_sub(fixed + 1);

    let mut spans = vec![Span::styled(label, Style::default().fg(ui::MUTED))];
    match metric.percent {
        Some(percent) if bar_room >= 6 => {
            let bar_width = bar_room.min(24);
            let filled = ((percent / 100.0) * bar_width as f64).round() as usize;
            // Any nonzero usage shows at least one cell — an empty-looking
            // bar reads as "no data", not "3%".
            let filled = filled.clamp(usize::from(percent > 0.0), bar_width);
            spans.push(Span::styled(
                "█".repeat(filled),
                Style::default().fg(color),
            ));
            spans.push(Span::styled(
                "░".repeat(bar_width - filled),
                Style::default().fg(TRACK),
            ));
            let pad = width.saturating_sub(fixed + bar_width);
            spans.push(Span::raw(" ".repeat(pad.max(1))));
        }
        _ => {
            let pad = width.saturating_sub(fixed);
            spans.push(Span::raw(" ".repeat(pad.max(1))));
        }
    }
    let value_style = if metric.level == Level::Ok {
        Style::default().fg(ui::HEADER)
    } else {
        Style::default().fg(color)
    };
    spans.push(Span::styled(value.clone(), value_style));
    Line::from(spans)
}

fn kv_row(key: &str, value: &str, width: usize) -> Line<'static> {
    let key_text = format!("     {key}");
    let used = key_text.chars().count() + value.chars().count();
    let pad = width.saturating_sub(used + 1);
    Line::from(vec![
        Span::styled(key_text, Style::default().fg(ui::MUTED)),
        Span::raw(" ".repeat(pad.max(1))),
        Span::styled(value.to_string(), Style::default().fg(ui::HEADER)),
    ])
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
