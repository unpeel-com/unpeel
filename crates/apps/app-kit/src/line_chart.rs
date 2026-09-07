//! Closed, data-first multi-series LineChart component.

use std::collections::HashSet;

use crossterm::event::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols;
use ratatui::text::Line;
use ratatui::widgets::{Axis, Chart as RatatuiChart, Dataset, GraphType, Widget};
use serde::{Deserialize, Serialize};

use crate::components::{ComponentValidationError, validate_identifier, validate_text};
use crate::{ChartValue, ColorScheme, KitTheme, TerminalPointerState};

/// Renderer capability for the LineChart component.
pub const LINE_CHART_COMPONENT_CAPABILITY: &str = "lineChart";

const MAX_SERIES: usize = 16;
const MAX_POINTS: usize = 100_000;
const MAX_LABEL_BYTES: usize = 4 * 1024;
const MAX_ACCESSIBILITY_BYTES: usize = 16 * 1024;

/// One finite Cartesian point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineChartPoint {
    pub x: ChartValue,
    pub y: ChartValue,
}

impl LineChartPoint {
    #[must_use]
    pub fn new(x: f64, y: f64) -> Self {
        Self {
            x: ChartValue::new(x),
            y: ChartValue::new(y),
        }
    }
}

/// One named line series. Names are the only v1 legend vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineChartSeries {
    pub name: String,
    pub points: Vec<LineChartPoint>,
}

impl LineChartSeries {
    #[must_use]
    pub fn new(name: impl Into<String>, points: impl IntoIterator<Item = LineChartPoint>) -> Self {
        Self {
            name: name.into(),
            points: points.into_iter().collect(),
        }
    }
}

/// Explicit finite bounds for one axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineChartBounds {
    pub min: ChartValue,
    pub max: ChartValue,
}

impl LineChartBounds {
    #[must_use]
    pub fn new(min: f64, max: f64) -> Self {
        Self {
            min: ChartValue::new(min),
            max: ChartValue::new(max),
        }
    }

    #[must_use]
    pub const fn values(self) -> (f64, f64) {
        (self.min.value(), self.max.value())
    }
}

/// Optional bounds and title for one axis.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineChartAxis {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<LineChartBounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl LineChartAxis {
    #[must_use]
    pub fn bounds(mut self, min: f64, max: f64) -> Self {
        self.bounds = Some(LineChartBounds::new(min, max));
        self
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.bounds.is_none() && self.label.is_none()
    }
}

/// A deliberately minimal line chart with named series and two axes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineChart {
    pub id: String,
    pub series: Vec<LineChartSeries>,
    #[serde(default, skip_serializing_if = "LineChartAxis::is_empty")]
    pub x_axis: LineChartAxis,
    #[serde(default, skip_serializing_if = "LineChartAxis::is_empty")]
    pub y_axis: LineChartAxis,
    pub accessibility_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<String>,
}

impl LineChart {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        series: impl IntoIterator<Item = LineChartSeries>,
        accessibility_text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            series: series.into_iter().collect(),
            x_axis: LineChartAxis::default(),
            y_axis: LineChartAxis::default(),
            accessibility_text: accessibility_text.into(),
            activate: None,
        }
    }

    #[must_use]
    pub fn x_axis(mut self, axis: LineChartAxis) -> Self {
        self.x_axis = axis;
        self
    }

    #[must_use]
    pub fn y_axis(mut self, axis: LineChartAxis) -> Self {
        self.y_axis = axis;
        self
    }

    #[must_use]
    pub fn activate(mut self, action: impl Into<String>) -> Self {
        self.activate = Some(action.into());
        self
    }

    #[must_use]
    pub fn action_for_mouse(&self, event: &MouseEvent, area: Rect) -> Option<&str> {
        let position = TerminalPointerState::click_position(event)?;
        area.contains(position)
            .then_some(self.activate.as_deref())
            .flatten()
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action_for_mouse(&self, event: &MouseEvent, area: Rect) -> Option<crate::UiAction> {
        self.action_for_mouse(event, area)
            .map(|action| crate::UiAction::activate(self.id.clone(), action.to_owned()))
    }

    #[must_use]
    pub fn resolved_x_bounds(&self) -> (f64, f64) {
        self.resolved_bounds(true)
    }

    #[must_use]
    pub fn resolved_y_bounds(&self) -> (f64, f64) {
        self.resolved_bounds(false)
    }

    fn resolved_bounds(&self, x_axis: bool) -> (f64, f64) {
        let axis = if x_axis { &self.x_axis } else { &self.y_axis };
        if let Some(bounds) = axis.bounds {
            return bounds.values();
        }
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        for point in self.series.iter().flat_map(|series| &series.points) {
            let value = if x_axis {
                point.x.value()
            } else {
                point.y.value()
            };
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
        if minimum == maximum {
            maximum = minimum + 1.0;
        }
        (minimum, maximum)
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        if self.series.is_empty() || self.series.len() > MAX_SERIES {
            return Err(ComponentValidationError::new(
                format!("{path}.series"),
                format!("must contain 1..={MAX_SERIES} series"),
            ));
        }
        let mut names = HashSet::new();
        let mut point_count = 0_usize;
        for (series_index, series) in self.series.iter().enumerate() {
            validate_single_line(
                &series.name,
                &format!("{path}.series[{series_index}].name"),
                false,
            )?;
            if !names.insert(&series.name) {
                return Err(ComponentValidationError::new(
                    format!("{path}.series[{series_index}].name"),
                    "series names must be unique",
                ));
            }
            if series.points.is_empty() {
                return Err(ComponentValidationError::new(
                    format!("{path}.series[{series_index}].points"),
                    "must not be empty",
                ));
            }
            point_count = point_count.saturating_add(series.points.len());
            for (point_index, point) in series.points.iter().enumerate() {
                if !point.x.value().is_finite() || !point.y.value().is_finite() {
                    return Err(ComponentValidationError::new(
                        format!("{path}.series[{series_index}].points[{point_index}]"),
                        "x and y must be finite",
                    ));
                }
            }
        }
        if point_count > MAX_POINTS {
            return Err(ComponentValidationError::new(
                format!("{path}.series"),
                format!("must contain at most {MAX_POINTS} total points"),
            ));
        }
        self.validate_axis(&self.x_axis, true, &format!("{path}.xAxis"))?;
        self.validate_axis(&self.y_axis, false, &format!("{path}.yAxis"))?;
        validate_text(
            &self.accessibility_text,
            MAX_ACCESSIBILITY_BYTES,
            &format!("{path}.accessibilityText"),
        )?;
        if self.accessibility_text.trim().is_empty() {
            return Err(ComponentValidationError::new(
                format!("{path}.accessibilityText"),
                "must not be empty",
            ));
        }
        if let Some(activate) = &self.activate {
            validate_identifier(activate, &format!("{path}.activate"))?;
        }
        Ok(())
    }

    fn validate_axis(
        &self,
        axis: &LineChartAxis,
        x_axis: bool,
        path: &str,
    ) -> Result<(), ComponentValidationError> {
        if let Some(label) = &axis.label {
            validate_single_line(label, &format!("{path}.label"), true)?;
        }
        let Some(bounds) = axis.bounds else {
            return Ok(());
        };
        let (minimum, maximum) = bounds.values();
        if !minimum.is_finite() || !maximum.is_finite() || minimum >= maximum {
            return Err(ComponentValidationError::new(
                format!("{path}.bounds"),
                "min and max must be finite with min less than max",
            ));
        }
        let contains_every_point =
            self.series
                .iter()
                .flat_map(|series| &series.points)
                .all(|point| {
                    let value = if x_axis {
                        point.x.value()
                    } else {
                        point.y.value()
                    };
                    (minimum..=maximum).contains(&value)
                });
        if !contains_every_point {
            return Err(ComponentValidationError::new(
                format!("{path}.bounds"),
                "must contain every series point",
            ));
        }
        Ok(())
    }

    pub(crate) fn replace_data_from(&mut self, replacement: Self) {
        self.series = replacement.series;
        self.x_axis = replacement.x_axis;
        self.y_axis = replacement.y_axis;
        self.accessibility_text = replacement.accessibility_text;
    }

    #[must_use]
    pub fn widget(&self) -> LineChartWidget<'_> {
        let theme = KitTheme::dark();
        LineChartWidget {
            chart: self,
            axis_style: Style::new().fg(theme.subtle),
            series_styles: terminal_series_styles(theme),
            pointer: TerminalPointerState::new(),
        }
    }
}

/// Ratatui interpretation of LineChart using Chart and Dataset.
pub struct LineChartWidget<'a> {
    chart: &'a LineChart,
    axis_style: Style,
    series_styles: [Style; 6],
    pointer: TerminalPointerState,
}

impl LineChartWidget<'_> {
    /// Applies a coherent terminal palette to axes and named series.
    #[must_use]
    pub const fn theme(mut self, theme: KitTheme) -> Self {
        self.axis_style = Style::new().fg(theme.subtle);
        self.series_styles = terminal_series_styles(theme);
        self
    }

    #[must_use]
    pub const fn styles(mut self, axis: Style, series: [Style; 6]) -> Self {
        self.axis_style = axis;
        self.series_styles = series;
        self
    }

    #[must_use]
    pub const fn axis_style(mut self, style: Style) -> Self {
        self.axis_style = style;
        self
    }

    #[must_use]
    pub const fn pointer(mut self, pointer: TerminalPointerState) -> Self {
        self.pointer = pointer;
        self
    }
}

const fn terminal_series_styles(theme: KitTheme) -> [Style; 6] {
    let (info, success, warning) = match theme.scheme {
        ColorScheme::Dark => (Color::LightBlue, Color::LightGreen, Color::LightYellow),
        ColorScheme::Light => (Color::Blue, Color::Green, Color::Yellow),
    };
    [
        Style::new().fg(theme.accent),
        Style::new().fg(info),
        Style::new().fg(success),
        Style::new().fg(warning),
        Style::new().fg(theme.danger),
        Style::new().fg(theme.muted),
    ]
}

impl Widget for LineChartWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() || self.chart.series.is_empty() {
            return;
        }
        let interaction = self
            .pointer
            .interaction_style(area, self.chart.activate.is_some());
        let point_sets = self
            .chart
            .series
            .iter()
            .map(|series| {
                series
                    .points
                    .iter()
                    .map(|point| (point.x.value(), point.y.value()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let datasets = self
            .chart
            .series
            .iter()
            .zip(&point_sets)
            .enumerate()
            .map(|(index, (series, points))| {
                Dataset::default()
                    .name(series.name.clone())
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(self.series_styles[index % self.series_styles.len()].patch(interaction))
                    .data(points)
            })
            .collect::<Vec<_>>();
        let x_bounds = self.chart.resolved_x_bounds();
        let y_bounds = self.chart.resolved_y_bounds();
        let mut x_axis = Axis::default()
            .bounds([x_bounds.0, x_bounds.1])
            .labels(axis_labels(x_bounds))
            .style(self.axis_style.patch(interaction));
        if let Some(label) = &self.chart.x_axis.label {
            x_axis = x_axis.title(label.clone());
        }
        let mut y_axis = Axis::default()
            .bounds([y_bounds.0, y_bounds.1])
            .labels(axis_labels(y_bounds))
            .style(self.axis_style.patch(interaction));
        if let Some(label) = &self.chart.y_axis.label {
            y_axis = y_axis.title(label.clone());
        }
        RatatuiChart::new(datasets)
            .x_axis(x_axis)
            .y_axis(y_axis)
            .render(area, buffer);
    }
}

fn axis_labels(bounds: (f64, f64)) -> [Line<'static>; 2] {
    [
        Line::from(bounds.0.to_string()),
        Line::from(bounds.1.to_string()),
    ]
}

fn validate_single_line(
    value: &str,
    path: &str,
    allow_empty: bool,
) -> Result<(), ComponentValidationError> {
    validate_text(value, MAX_LABEL_BYTES, path)?;
    if value.contains('\n') || (!allow_empty && value.trim().is_empty()) {
        return Err(ComponentValidationError::new(
            path,
            "must be a non-empty single line",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn resolves_axis_bounds_once_and_rejects_clipping_bounds() {
        let chart = LineChart::new(
            "traffic",
            [LineChartSeries::new(
                "Requests",
                [
                    LineChartPoint::new(1.0, 10.0),
                    LineChartPoint::new(2.0, 30.0),
                ],
            )],
            "Requests rose from ten to thirty",
        )
        .x_axis(LineChartAxis::default().label("Day"))
        .y_axis(LineChartAxis::default().bounds(0.0, 40.0).label("Requests"));
        assert_eq!(chart.resolved_x_bounds(), (1.0, 2.0));
        assert_eq!(chart.resolved_y_bounds(), (0.0, 40.0));
        assert!(chart.validate("lineChart").is_ok());
        let invalid = chart
            .clone()
            .y_axis(LineChartAxis::default().bounds(0.0, 20.0));
        assert!(invalid.validate("lineChart").is_err());
    }

    #[test]
    fn renders_named_series_through_ratatui_chart() {
        let chart = LineChart::new(
            "lines",
            [LineChartSeries::new(
                "A",
                [LineChartPoint::new(0.0, 0.0), LineChartPoint::new(1.0, 1.0)],
            )],
            "A rises",
        );
        let mut terminal = Terminal::new(TestBackend::new(30, 8)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(chart.widget(), frame.area()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| !cell.symbol().trim().is_empty());
        assert!(rendered);
    }
}
