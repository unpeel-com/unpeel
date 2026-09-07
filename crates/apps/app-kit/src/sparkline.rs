//! Closed, read-only Sparkline component shared by terminal and semantic renderers.

use crossterm::event::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Sparkline as RatatuiSparkline, Widget};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::components::{ComponentValidationError, validate_identifier, validate_text};
use crate::{KitTheme, TerminalPointerState};

/// Renderer capability for the read-only Sparkline component.
pub const SPARKLINE_COMPONENT_CAPABILITY: &str = "sparkline";

const MAX_SPARKLINE_POINTS: usize = 100_000;
const MAX_SPARKLINE_LABEL_BYTES: usize = 4 * 1024;
const MAX_SPARKLINE_ACCESSIBILITY_BYTES: usize = 16 * 1024;
const TERMINAL_SCALE: f64 = 1_000.0;

/// One finite point in a [`Sparkline`] series.
///
/// The transparent wrapper keeps the wire format numeric while giving the
/// containing component lawful equality semantics (including during decoding
/// before validation).
#[derive(Clone, Copy, Debug)]
pub struct SparklinePoint(f64);

/// Shared finite-number wire wrapper used by the deliberately closed chart
/// components. SparklinePoint remains as the source-compatible name for the
/// first member of the family.
pub type ChartValue = SparklinePoint;

impl SparklinePoint {
    #[must_use]
    pub fn new(value: f64) -> Self {
        // Canonicalize signed zero so semantically identical snapshots compare
        // equal and produce identical deltas.
        Self(if value == 0.0 { 0.0 } else { value })
    }

    #[must_use]
    pub const fn value(self) -> f64 {
        self.0
    }
}

impl From<f64> for SparklinePoint {
    fn from(value: f64) -> Self {
        Self::new(value)
    }
}

impl PartialEq for SparklinePoint {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for SparklinePoint {}

impl Serialize for SparklinePoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for SparklinePoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        f64::deserialize(deserializer).map(Self::new)
    }
}

/// A deliberately small, data-first history graph.
///
/// This is not a generic chart grammar. The numeric series remains available
/// to accessibility tools and agent participants, while each presentation
/// layer maps the same bounds to its native compact Sparkline primitive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sparkline {
    pub id: String,
    pub series: Vec<SparklinePoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<SparklinePoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<SparklinePoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub accessibility_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<String>,
}

impl Sparkline {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        series: impl IntoIterator<Item = f64>,
        accessibility_text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            series: series.into_iter().map(SparklinePoint::new).collect(),
            min: None,
            max: None,
            caption: None,
            unit: None,
            accessibility_text: accessibility_text.into(),
            activate: None,
        }
    }

    /// Sets either or both authoritative display bounds. A missing side is
    /// derived from the series using the same zero-baseline rule everywhere.
    #[must_use]
    pub fn bounds(mut self, min: Option<f64>, max: Option<f64>) -> Self {
        self.min = min.map(SparklinePoint::new);
        self.max = max.map(SparklinePoint::new);
        self
    }

    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    #[must_use]
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Declares the component's one optional idempotent activation action.
    #[must_use]
    pub fn activate(mut self, action: impl Into<String>) -> Self {
        self.activate = Some(action.into());
        self
    }

    /// Resolves a terminal click through the same optional action carried by
    /// the semantic component.
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
    pub fn values(&self) -> impl ExactSizeIterator<Item = f64> + '_ {
        self.series.iter().copied().map(SparklinePoint::value)
    }

    /// Resolves the authoritative cross-renderer domain.
    ///
    /// Inferred domains include zero, preserving Ratatui Sparkline's baseline.
    /// An all-zero domain expands to `0...1` so every renderer handles the
    /// degenerate series identically.
    #[must_use]
    pub fn resolved_bounds(&self) -> Option<(f64, f64)> {
        let mut values = self.values();
        let first = values.next()?;
        let (series_min, series_max) = values.fold((first, first), |(min, max), value| {
            (min.min(value), max.max(value))
        });
        let min = self.min.map_or(series_min.min(0.0), SparklinePoint::value);
        let mut max = self.max.map_or(series_max.max(0.0), SparklinePoint::value);
        if min == max {
            max = min + 1.0;
        }
        Some((min, max))
    }

    /// Normalized `0...1` values used by Swift Charts and the web SVG. Keeping
    /// this rule in the spec prevents presenter-specific chart math.
    #[must_use]
    pub fn normalized_values(&self) -> Vec<f64> {
        let Some((min, max)) = self.resolved_bounds() else {
            return Vec::new();
        };
        let range = max - min;
        self.values()
            .map(|value| ((value - min) / range).clamp(0.0, 1.0))
            .collect()
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        if self.series.is_empty() || self.series.len() > MAX_SPARKLINE_POINTS {
            return Err(ComponentValidationError::new(
                format!("{path}.series"),
                format!("must contain 1..={MAX_SPARKLINE_POINTS} points"),
            ));
        }
        for (index, point) in self.series.iter().enumerate() {
            if !point.value().is_finite() {
                return Err(ComponentValidationError::new(
                    format!("{path}.series[{index}]"),
                    "must be finite",
                ));
            }
        }
        for (name, bound) in [("min", self.min), ("max", self.max)] {
            if bound.is_some_and(|value| !value.value().is_finite()) {
                return Err(ComponentValidationError::new(
                    format!("{path}.{name}"),
                    "must be finite",
                ));
            }
        }
        if let (Some(min), Some(max)) = (self.min, self.max)
            && min.value() >= max.value()
        {
            return Err(ComponentValidationError::new(
                format!("{path}.bounds"),
                "min must be less than max",
            ));
        }
        let series_min = self.values().fold(f64::INFINITY, f64::min);
        let series_max = self.values().fold(f64::NEG_INFINITY, f64::max);
        if self.min.is_some_and(|min| series_min < min.value())
            || self.max.is_some_and(|max| series_max > max.value())
        {
            return Err(ComponentValidationError::new(
                format!("{path}.bounds"),
                "explicit bounds must contain every series point",
            ));
        }
        for (name, value) in [("caption", &self.caption), ("unit", &self.unit)] {
            if let Some(value) = value {
                validate_text(value, MAX_SPARKLINE_LABEL_BYTES, &format!("{path}.{name}"))?;
                if value.contains('\n') {
                    return Err(ComponentValidationError::new(
                        format!("{path}.{name}"),
                        "must be a single line",
                    ));
                }
            }
        }
        validate_text(
            &self.accessibility_text,
            MAX_SPARKLINE_ACCESSIBILITY_BYTES,
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

    pub(crate) fn replace_data_from(&mut self, replacement: Self) {
        self.series = replacement.series;
        self.min = replacement.min;
        self.max = replacement.max;
        self.caption = replacement.caption;
        self.unit = replacement.unit;
        self.accessibility_text = replacement.accessibility_text;
    }

    /// Standalone Ratatui interpretation. It keeps the newest points visible
    /// when the allocated row is narrower than the series.
    #[must_use]
    pub fn widget(&self) -> SparklineWidget<'_> {
        SparklineWidget {
            sparkline: self,
            style: Style::new().fg(KitTheme::dark().accent),
            pointer: TerminalPointerState::new(),
        }
    }
}

/// Ratatui renderer for [`Sparkline`].
pub struct SparklineWidget<'a> {
    sparkline: &'a Sparkline,
    style: Style,
    pointer: TerminalPointerState,
}

impl SparklineWidget<'_> {
    /// Applies App Kit's terminal palette while preserving the chart data.
    #[must_use]
    pub const fn theme(mut self, theme: KitTheme) -> Self {
        self.style = Style::new().fg(theme.accent);
        self
    }

    #[must_use]
    pub const fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub const fn pointer(mut self, pointer: TerminalPointerState) -> Self {
        self.pointer = pointer;
        self
    }
}

impl Widget for SparklineWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() || self.sparkline.series.is_empty() {
            return;
        }
        let width = usize::from(area.width);
        let start = self.sparkline.series.len().saturating_sub(width);
        let visible = &self.sparkline.series[start..];
        // Ratatui's compact-cell interpretation is a newest-points viewport.
        // Unspecified bounds are inferred from that viewport, preserving the
        // established responsive widget behavior. Explicit bounds remain
        // authoritative across every viewport and renderer.
        let visible_min = visible
            .iter()
            .map(|point| point.value())
            .fold(f64::INFINITY, f64::min);
        let visible_max = visible
            .iter()
            .map(|point| point.value())
            .fold(f64::NEG_INFINITY, f64::max);
        let min = self
            .sparkline
            .min
            .map_or(visible_min.min(0.0), SparklinePoint::value);
        let mut max = self
            .sparkline
            .max
            .map_or(visible_max.max(0.0), SparklinePoint::value);
        if min == max {
            max = min + 1.0;
        }
        let data = visible
            .iter()
            .map(|point| ((point.value() - min) * TERMINAL_SCALE).round().max(0.0) as u64)
            .collect::<Vec<_>>();
        let terminal_max = ((max - min) * TERMINAL_SCALE).round().max(1.0) as u64;
        let style = self.style.patch(
            self.pointer
                .interaction_style(area, self.sparkline.activate.is_some()),
        );
        RatatuiSparkline::default()
            .data(data)
            .max(terminal_max)
            .style(style)
            .render(area, buffer);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Style};

    use super::*;

    #[test]
    fn defaults_to_a_zero_baseline_and_normalizes_once() {
        let sparkline = Sparkline::new("usage", [2.0, 4.0, 3.0], "Usage history");
        assert_eq!(sparkline.resolved_bounds(), Some((0.0, 4.0)));
        assert_eq!(sparkline.normalized_values(), vec![0.5, 1.0, 0.75]);
    }

    #[test]
    fn validation_rejects_invalid_bounds_and_empty_accessibility() {
        let invalid = Sparkline::new("usage", [2.0, 4.0], "").bounds(Some(3.0), Some(5.0));
        assert!(invalid.validate("sparkline").is_err());
    }

    #[test]
    fn terminal_widget_keeps_the_newest_points() {
        let backend = TestBackend::new(2, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let sparkline = Sparkline::new("usage", [0.0, 1.0, 2.0, 4.0], "Usage history");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    sparkline.widget().style(Style::new().fg(Color::Blue)),
                    frame.area(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "▄");
        assert_eq!(buffer[(1, 0)].symbol(), "█");
        assert_eq!(buffer[(0, 0)].fg, Color::Blue);
    }
}
