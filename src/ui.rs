//! Drawing. The layout is deliberately narrow-first: every card degrades
//! gracefully down to sidebar-panel widths (~40 columns) — bars shrink
//! before text, and text truncates before wrapping.
//!
//! Each provider carries its brand accent (Claude coral, Codex teal): the
//! card glyph, its healthy bars, and its sparkline all render in it, so a
//! glance separates providers before any text is read.

use crate::sources::{Level, Metric, Provider, ProviderKind, Snapshot};
use crate::timeparse::{compact_duration, now_epoch_secs};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use crate::theme as ui;

/// Dim bar track, one shade under MUTED so filled cells pop.
const TRACK: Color = Color::Rgb(58, 62, 74);

const SPARK_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn accent(kind: ProviderKind) -> Color {
    match kind {
        ProviderKind::Codex => ui::CODEX_ACCENT,
        ProviderKind::Claude => ui::CLAUDE_ACCENT,
    }
}

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

/// One card's clickable screen rows (inclusive), for mouse hit-testing.
pub struct Hit {
    pub index: usize,
    pub top: u16,
    pub bottom: u16,
}

impl Hit {
    pub fn contains(&self, row: u16) -> bool {
        (self.top..=self.bottom).contains(&row)
    }
}

pub fn draw(frame: &mut Frame, snapshot: Option<&Snapshot>, view: &View) -> Vec<Hit> {
    let area = frame.area();
    let mut lines: Vec<Line> = Vec::new();
    let mut hits: Vec<Hit> = Vec::new();

    // Header: title left; 24h total, alert state, and spinner right.
    let mut header = vec![Span::styled(
        " USAGE",
        Style::default().fg(ui::HEADER).add_modifier(Modifier::BOLD),
    )];
    let mut right_parts: Vec<String> = Vec::new();
    if let Some(total) = snapshot.and_then(Snapshot::day_total_usd) {
        if total > 0.0 {
            right_parts.push(format!("24h {} est", crate::sources::compact_usd(total)));
        }
    }
    if !view.alerts_enabled {
        right_parts.push("alerts off".into());
    }
    if view.scanning {
        right_parts.push(ui::spinner_frame().to_string());
    }
    let right = right_parts.join(" · ");
    let used: usize = 6 + right.chars().count() + 1;
    let pad = (area.width as usize).saturating_sub(used);
    header.push(Span::raw(" ".repeat(pad)));
    header.push(Span::styled(right, Style::default().fg(ui::MUTED)));
    header.push(Span::raw(" "));
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
                // The card's rows start past its leading blank line.
                let top = lines.len() as u16 + 1;
                push_card(
                    &mut lines,
                    provider,
                    selected,
                    selected && view.expanded,
                    area.width,
                );
                hits.push(Hit {
                    index,
                    top: area.y + top,
                    bottom: area.y + lines.len() as u16 - 1,
                });
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

    // Clip hit regions to the rendered body: rows under the footer are not
    // clickable, and fully hidden cards drop out entirely.
    let last_row = area.y + body_height.saturating_sub(1);
    hits.retain(|hit| hit.top <= last_row);
    for hit in &mut hits {
        hit.bottom = hit.bottom.min(last_row);
    }
    hits
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
    let accent = accent(provider.kind);
    lines.push(Line::default());

    // Card title: selection bar, accent glyph, name, right-aligned badge.
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
        provider.name.clone()
    };
    let glyph_style = if provider.present && !provider.metrics.is_empty() {
        Style::default().fg(accent)
    } else {
        Style::default().fg(TRACK)
    };
    let mut title = vec![
        Span::styled(marker.to_string(), Style::default().fg(ui::FOCUS)),
        Span::styled("● ".to_string(), glyph_style),
        Span::styled(name.clone(), name_style),
    ];
    let used = 3 + name.chars().count();
    if !provider.badge.is_empty() {
        let badge = truncate(&provider.badge, width.saturating_sub(used + 3));
        let pad = width.saturating_sub(used + badge.chars().count() + 1);
        title.push(Span::raw(" ".repeat(pad)));
        title.push(Span::styled(badge, Style::default().fg(ui::MUTED)));
    }
    lines.push(Line::from(title));

    if !provider.present {
        lines.push(muted_row("not installed"));
        return;
    }
    if provider.metrics.is_empty() {
        lines.push(muted_row("no recent activity"));
        return;
    }
    for metric in &provider.metrics {
        lines.push(metric_row(metric, width, accent));
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

fn muted_row(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("     "),
        Span::styled(text.to_string(), Style::default().fg(ui::MUTED)),
    ])
}

/// `   label  ▐███░░░░▌  value` — a bar for percentages, a sparkline for
/// hourly activity, or the value alone when there is no room or no shape.
fn metric_row(metric: &Metric, width: usize, accent: Color) -> Line<'static> {
    const LABEL_WIDTH: usize = 9;
    let color = level_color(metric.level);
    let fill = if metric.level == Level::Ok { accent } else { color };
    let label = format!("   {:<LABEL_WIDTH$}", truncate(&metric.label, LABEL_WIDTH));
    let value = &metric.value;
    let fixed = label.chars().count() + 2 + value.chars().count();
    let bar_room = width.saturating_sub(fixed + 1);

    let mut spans = vec![Span::styled(label, Style::default().fg(ui::MUTED))];
    if !metric.spark.is_empty() && bar_room >= 8 {
        // One glyph per trailing hour; narrow cards keep the newest hours.
        let cells = bar_room.min(metric.spark.len());
        let window = &metric.spark[metric.spark.len() - cells..];
        let max = window.iter().cloned().fold(0.0_f64, f64::max);
        for &bucket in window {
            if bucket <= 0.0 || max <= 0.0 {
                spans.push(Span::styled("▁".to_string(), Style::default().fg(TRACK)));
            } else {
                let step = ((bucket / max) * 7.0).round().clamp(1.0, 7.0) as usize;
                spans.push(Span::styled(
                    SPARK_GLYPHS[step].to_string(),
                    Style::default().fg(fill),
                ));
            }
        }
        let pad = width.saturating_sub(fixed + cells);
        spans.push(Span::raw(" ".repeat(pad.max(1))));
    } else if let Some(percent) = metric.percent.filter(|_| bar_room >= 6) {
        let bar_width = bar_room.min(24);
        let filled = ((percent / 100.0) * bar_width as f64).round() as usize;
        // Any nonzero usage shows at least one cell — an empty-looking
        // bar reads as "no data", not "3%".
        let filled = filled.clamp(usize::from(percent > 0.0), bar_width);
        spans.push(Span::styled("█".repeat(filled), Style::default().fg(fill)));
        spans.push(Span::styled(
            "░".repeat(bar_width - filled),
            Style::default().fg(TRACK),
        ));
        let pad = width.saturating_sub(fixed + bar_width);
        spans.push(Span::raw(" ".repeat(pad.max(1))));
    } else {
        let pad = width.saturating_sub(fixed);
        spans.push(Span::raw(" ".repeat(pad.max(1))));
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

    fn render(width: u16, height: u16, expanded: bool) -> (String, Vec<Hit>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let snapshot = sample();
        let view = View {
            selected: 1,
            expanded,
            scanning: false,
            alerts_enabled: true,
        };
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                hits = draw(frame, Some(&snapshot), &view);
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
        (screen, hits)
    }

    #[test]
    fn wide_layout_shows_accounts_totals_and_sparkline() {
        let (screen, _) = render(72, 20, true);
        assert!(screen.contains("USAGE"), "uppercase brand\n{screen}");
        assert!(screen.contains("24h $17.82 est"), "header 24h total\n{screen}");
        assert!(screen.contains("tommy@uxthemes.com"), "account badge\n{screen}");
        assert!(screen.contains("Claude Code · work"), "second account\n{screen}");
        assert!(screen.contains("$1.20/hr est"), "burn rate\n{screen}");
        assert!(screen.contains("▁▃▅▆█"), "sparkline glyphs\n{screen}");
        assert!(screen.contains("sonnet-4-6"), "expanded detail\n{screen}");
    }

    #[test]
    fn narrow_layout_never_wraps() {
        for width in [38u16, 44, 52] {
            let (screen, _) = render(width, 24, true);
            for line in screen.lines() {
                assert!(
                    line.chars().count() <= width as usize,
                    "line overflow at width {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn hit_regions_cover_each_card_title() {
        let (screen, hits) = render(72, 24, false);
        assert_eq!(hits.len(), 3);
        let rows: Vec<&str> = screen.lines().collect();
        for (hit, title) in hits.iter().zip(["Codex", "Claude Code", "Claude Code · work"]) {
            assert!(
                rows[hit.top as usize].contains(title),
                "hit top row should hold {title:?}: {:?}",
                rows[hit.top as usize]
            );
            assert!(hit.bottom >= hit.top);
        }
        // Regions never overlap and never reach the footer hint row.
        for pair in hits.windows(2) {
            assert!(pair[1].top > pair[0].bottom);
        }
        assert!(hits.last().unwrap().bottom < 23);
    }

    #[test]
    fn hit_regions_clip_to_short_terminals() {
        let (_, hits) = render(72, 8, false);
        // 8 rows: header + rule + first card only; nothing may extend under
        // the footer at row 7.
        assert!(!hits.is_empty());
        for hit in &hits {
            assert!(hit.bottom < 7, "hit reaches footer: {}..{}", hit.top, hit.bottom);
        }
    }
}
