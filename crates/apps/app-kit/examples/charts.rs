//! Standalone-first showcase for App Kit's deliberately closed chart family.
//!
//! Run `cargo run --example charts` in any terminal. When an Unpeel-compatible
//! Host injects the optional UI bridge, the same process publishes each chart
//! as a native Page body for SwiftUI and web renderers.

use std::error::Error;
use std::io;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use unpeel_app_kit::{
    BarChart, BarChartBar, BarChartEmphasis, Gauge, InputField, KitTheme, LineChart, LineChartAxis,
    LineChartPoint, LineChartSeries, ListState, Page, PagePointerDecision, PageTheme, Sparkline,
};

#[cfg(feature = "ui-bridge")]
use unpeel_app_kit::{
    AppMetadata, UiBridge, UiBridgeEvent, UiDeltaOperation, UiEventKind, UiEventOutcome,
    UiEventValue, UiNode,
};

const CHART_COUNT: usize = 4;
const NEXT_ACTION: &str = "next-chart";
#[cfg(feature = "ui-bridge")]
const ROOT_ID: &str = "chart-page";
#[cfg(feature = "ui-bridge")]
const VIEW_ID: &str = "main";

struct ChartsApp {
    selected: usize,
    revision: u64,
    input: InputField,
    list_state: ListState,
}

impl ChartsApp {
    fn new() -> Self {
        Self {
            selected: 0,
            revision: 1,
            input: InputField::new(""),
            list_state: ListState::default(),
        }
    }

    fn page(&self) -> Page {
        chart_page(self.selected)
    }

    #[cfg(any(feature = "ui-bridge", test))]
    fn chart_id(&self) -> &'static str {
        match self.selected {
            0 => "activity-sparkline",
            1 => "revenue-bars",
            2 => "request-lines",
            _ => "deployment-gauge",
        }
    }

    fn advance(&mut self, offset: isize) -> u64 {
        let base = self.revision;
        self.selected = (self.selected as isize + offset).rem_euclid(CHART_COUNT as isize) as usize;
        self.revision += 1;
        base
    }
}

fn chart_page(index: usize) -> Page {
    match index % CHART_COUNT {
        0 => Page::with_sparkline(
            "Sparkline",
            Sparkline::new(
                "activity-sparkline",
                [2.0, 5.0, 3.5, 8.0, 6.0, 10.0, 9.0],
                "Daily activity: 2, 5, 3.5, 8, 6, 10, 9 thousand events",
            )
            .bounds(Some(0.0), Some(12.0))
            .caption("Daily activity")
            .unit("thousand events")
            .activate(NEXT_ACTION),
        ),
        1 => Page::with_bar_chart(
            "Bar Chart",
            BarChart::new(
                "revenue-bars",
                [
                    BarChartBar::new("Jan", 12.0).value_caption("12k"),
                    BarChartBar::new("Feb", 18.0)
                        .value_caption("18k")
                        .emphasis(BarChartEmphasis::Accent),
                    BarChartBar::new("Mar", 7.0)
                        .value_caption("7k")
                        .emphasis(BarChartEmphasis::Danger),
                    BarChartBar::new("Apr", 15.0).value_caption("15k"),
                ],
                "Revenue: January 12k, February 18k, March 7k, April 15k",
            )
            .activate(NEXT_ACTION),
        ),
        2 => Page::with_line_chart(
            "Line Chart",
            LineChart::new(
                "request-lines",
                [
                    LineChartSeries::new(
                        "Actual",
                        [
                            LineChartPoint::new(0.0, 2.0),
                            LineChartPoint::new(1.0, 5.0),
                            LineChartPoint::new(2.0, 4.0),
                            LineChartPoint::new(3.0, 7.0),
                        ],
                    ),
                    LineChartSeries::new(
                        "Forecast",
                        [
                            LineChartPoint::new(0.0, 3.0),
                            LineChartPoint::new(1.0, 4.0),
                            LineChartPoint::new(2.0, 6.0),
                            LineChartPoint::new(3.0, 8.0),
                        ],
                    ),
                ],
                "Actual and forecast requests over four days",
            )
            .x_axis(LineChartAxis::default().bounds(0.0, 3.0).label("Day"))
            .y_axis(LineChartAxis::default().bounds(0.0, 10.0).label("Requests"))
            .activate(NEXT_ACTION),
        ),
        _ => Page::with_gauge(
            "Gauge",
            Gauge::new(
                "deployment-gauge",
                0.64,
                "Deployment",
                "Deployment is 64 percent complete",
            )
            .activate(NEXT_ACTION),
        ),
    }
}

#[cfg(feature = "ui-bridge")]
fn publish_page(
    app: &ChartsApp,
    bridge: &mut UiBridge,
    base_revision: u64,
) -> Result<(), Box<dyn Error>> {
    bridge.publish_delta(
        VIEW_ID,
        base_revision,
        app.revision,
        vec![UiDeltaOperation::ReplaceRoot {
            root: UiNode::page(ROOT_ID, app.page()),
        }],
    )?;
    Ok(())
}

#[cfg(feature = "ui-bridge")]
fn drain_bridge(app: &mut ChartsApp, bridge: &mut UiBridge) -> Result<(), Box<dyn Error>> {
    while let Some(message) = bridge.poll()? {
        match message {
            UiBridgeEvent::Action { event, .. } => {
                let valid = event.base_revision == app.revision
                    && event.action.node_id.as_str() == app.chart_id()
                    && event.action.action.as_str() == NEXT_ACTION
                    && event.action.kind == UiEventKind::Activate
                    && event.action.value == UiEventValue::None;
                let outcome = if valid {
                    let base = app.advance(1);
                    publish_page(app, bridge, base)?;
                    UiEventOutcome::Applied
                } else {
                    UiEventOutcome::Rejected(
                        "Chart action is stale or not declared by the active component".to_owned(),
                    )
                };
                bridge.acknowledge(&event, outcome, app.revision)?;
            }
            UiBridgeEvent::Attached { .. }
            | UiBridgeEvent::Detached { .. }
            | UiBridgeEvent::Lifecycle { .. } => {}
        }
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut app = ChartsApp::new();
    #[cfg(feature = "ui-bridge")]
    let mut bridge = {
        let mut bridge = UiBridge::detect(
            AppMetadata::new(
                "dev.unpeel.app-kit.charts",
                "Charts",
                env!("CARGO_PKG_VERSION"),
            )
            .description("Standalone Ratatui and hosted semantic chart showcase"),
        )?;
        bridge.publish(VIEW_ID, app.revision, UiNode::page(ROOT_ID, app.page()))?;
        bridge
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut page_area = Rect::default();
    let result = (|| -> Result<(), Box<dyn Error>> {
        loop {
            #[cfg(feature = "ui-bridge")]
            drain_bridge(&mut app, &mut bridge)?;

            #[cfg(feature = "ui-bridge")]
            let should_draw = bridge.should_render_terminal();
            #[cfg(not(feature = "ui-bridge"))]
            let should_draw = true;

            if should_draw {
                let page = app.page();
                let theme = PageTheme::for_theme(KitTheme::detected());
                terminal.draw(|frame| {
                    let areas = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(0), Constraint::Length(1)])
                        .split(frame.area());
                    page_area = areas[0];
                    frame.render_widget(
                        page.widget(&mut app.input, &mut app.list_state)
                            .theme(theme),
                        areas[0],
                    );
                    frame.render_widget(
                        Paragraph::new(
                            "←/→ previous/next · Enter/Space/click next · Esc/q quit · 4 semantic charts",
                        )
                        .style(Style::new().fg(KitTheme::detected().subtle)),
                        areas[1],
                    );
                })?;
            }

            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && !key.modifiers.intersects(
                            KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SUPER,
                        ) =>
                {
                    let offset = match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => break,
                        KeyCode::Left | KeyCode::Up => Some(-1),
                        KeyCode::Right | KeyCode::Down | KeyCode::Enter | KeyCode::Char(' ') => {
                            Some(1)
                        }
                        _ => None,
                    };
                    if let Some(offset) = offset {
                        let base = app.advance(offset);
                        #[cfg(not(feature = "ui-bridge"))]
                        let _ = base;
                        #[cfg(feature = "ui-bridge")]
                        publish_page(&app, &mut bridge, base)?;
                    }
                }
                Event::Resize(_, _) => terminal.autoresize()?,
                Event::Mouse(mouse) => {
                    app.list_state.track_mouse(&mouse);
                    let page = app.page();
                    if matches!(
                        page.pointer_decision(&mut app.list_state, &mouse, page_area),
                        Some(PagePointerDecision::Activate { .. })
                    ) {
                        let base = app.advance(1);
                        #[cfg(not(feature = "ui-bridge"))]
                        let _ = base;
                        #[cfg(feature = "ui-bridge")]
                        publish_page(&app, &mut bridge, base)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn main() {
    if let Err(error) = run() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        eprintln!("charts: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chart_page_is_a_valid_closed_component_tree() {
        for index in 0..CHART_COUNT {
            assert!(chart_page(index).validate().is_ok());
        }
    }

    #[test]
    fn selection_wraps_without_changing_chart_identity_rules() {
        let mut app = ChartsApp::new();
        app.advance(-1);
        assert_eq!(app.selected, 3);
        assert_eq!(app.chart_id(), "deployment-gauge");
        assert_eq!(app.revision, 2);
    }
}
