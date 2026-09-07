//! Closed, data-first categorical BarChart component.

use crossterm::event::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Bar as RatatuiBar, BarChart as RatatuiBarChart, BarGroup, Widget};
use serde::{Deserialize, Serialize};

use crate::components::{ComponentValidationError, validate_identifier, validate_text};
use crate::{ChartValue, KitTheme, TerminalPointerState};

/// Renderer capability for the BarChart component.
pub const BAR_CHART_COMPONENT_CAPABILITY: &str = "barChart";

const MAX_BARS: usize = 1_000;
const MAX_LABEL_BYTES: usize = 4 * 1024;
const MAX_ACCESSIBILITY_BYTES: usize = 16 * 1024;
const TERMINAL_SCALE: f64 = 1_000_000.0;

/// The only per-bar emphasis choices in v1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BarChartEmphasis {
    #[default]
    Default,
    Accent,
    Danger,
}

/// One labeled, non-negative numeric bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BarChartBar {
    pub label: String,
    pub value: ChartValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_caption: Option<String>,
    #[serde(default)]
    pub emphasis: BarChartEmphasis,
}

impl BarChartBar {
    #[must_use]
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value: ChartValue::new(value),
            value_caption: None,
            emphasis: BarChartEmphasis::Default,
        }
    }

    #[must_use]
    pub fn value_caption(mut self, caption: impl Into<String>) -> Self {
        self.value_caption = Some(caption.into());
        self
    }

    #[must_use]
    pub const fn emphasis(mut self, emphasis: BarChartEmphasis) -> Self {
        self.emphasis = emphasis;
        self
    }

    fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_chart_label(&self.label, &format!("{path}.label"), false)?;
        let value = self.value.value();
        if !value.is_finite() || value < 0.0 {
            return Err(ComponentValidationError::new(
                format!("{path}.value"),
                "must be a finite non-negative number",
            ));
        }
        if let Some(caption) = &self.value_caption {
            validate_chart_label(caption, &format!("{path}.valueCaption"), true)?;
        }
        Ok(())
    }
}

/// A deliberately small categorical bar chart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BarChart {
    pub id: String,
    pub bars: Vec<BarChartBar>,
    pub accessibility_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<String>,
}

impl BarChart {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        bars: impl IntoIterator<Item = BarChartBar>,
        accessibility_text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            bars: bars.into_iter().collect(),
            accessibility_text: accessibility_text.into(),
            activate: None,
        }
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

    /// Shared zero-based normalization consumed by native and web fixtures.
    #[must_use]
    pub fn normalized_values(&self) -> Vec<f64> {
        let maximum = self
            .bars
            .iter()
            .map(|bar| bar.value.value())
            .fold(0.0_f64, f64::max)
            .max(1.0);
        self.bars
            .iter()
            .map(|bar| (bar.value.value() / maximum).clamp(0.0, 1.0))
            .collect()
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        if self.bars.is_empty() || self.bars.len() > MAX_BARS {
            return Err(ComponentValidationError::new(
                format!("{path}.bars"),
                format!("must contain 1..={MAX_BARS} bars"),
            ));
        }
        for (index, bar) in self.bars.iter().enumerate() {
            bar.validate(&format!("{path}.bars[{index}]"))?;
        }
        validate_accessibility(&self.accessibility_text, path)?;
        if let Some(activate) = &self.activate {
            validate_identifier(activate, &format!("{path}.activate"))?;
        }
        Ok(())
    }

    pub(crate) fn replace_data_from(&mut self, replacement: Self) {
        self.bars = replacement.bars;
        self.accessibility_text = replacement.accessibility_text;
    }

    #[must_use]
    pub fn widget(&self) -> BarChartWidget<'_> {
        let theme = KitTheme::dark();
        BarChartWidget {
            chart: self,
            default_style: Style::new().fg(theme.muted),
            accent_style: Style::new().fg(theme.accent),
            danger_style: Style::new().fg(theme.danger),
            value_style: Style::new().fg(theme.text),
            pointer: TerminalPointerState::new(),
        }
    }
}

/// Ratatui interpretation of BarChart using Ratatui's native BarChart widget.
pub struct BarChartWidget<'a> {
    chart: &'a BarChart,
    default_style: Style,
    accent_style: Style,
    danger_style: Style,
    value_style: Style,
    pointer: TerminalPointerState,
}

impl BarChartWidget<'_> {
    /// Applies one coherent App Kit terminal palette to every chart role.
    #[must_use]
    pub const fn theme(mut self, theme: KitTheme) -> Self {
        self.default_style = Style::new().fg(theme.muted);
        self.accent_style = Style::new().fg(theme.accent);
        self.danger_style = Style::new().fg(theme.danger);
        self.value_style = Style::new().fg(theme.text);
        self
    }

    #[must_use]
    pub const fn styles(
        mut self,
        default_style: Style,
        accent_style: Style,
        danger_style: Style,
        value_style: Style,
    ) -> Self {
        self.default_style = default_style;
        self.accent_style = accent_style;
        self.danger_style = danger_style;
        self.value_style = value_style;
        self
    }

    #[must_use]
    pub const fn pointer(mut self, pointer: TerminalPointerState) -> Self {
        self.pointer = pointer;
        self
    }
}

impl Widget for BarChartWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() || self.chart.bars.is_empty() {
            return;
        }
        let normalized = self.chart.normalized_values();
        let interaction = self
            .pointer
            .interaction_style(area, self.chart.activate.is_some());
        let bars = self
            .chart
            .bars
            .iter()
            .zip(normalized)
            .map(|(bar, normalized)| {
                let style = match bar.emphasis {
                    BarChartEmphasis::Default => self.default_style,
                    BarChartEmphasis::Accent => self.accent_style,
                    BarChartEmphasis::Danger => self.danger_style,
                };
                RatatuiBar::with_label(
                    bar.label.clone(),
                    (normalized * TERMINAL_SCALE).round() as u64,
                )
                .text_value(bar.value_caption.clone().unwrap_or_default())
                .style(style.patch(interaction))
                .value_style(self.value_style.patch(interaction))
            })
            .collect::<Vec<_>>();
        let count = u16::try_from(bars.len()).unwrap_or(u16::MAX).max(1);
        let bar_width = area
            .width
            .saturating_sub(count.saturating_sub(1))
            .checked_div(count)
            .unwrap_or(1)
            .clamp(1, 8);
        RatatuiBarChart::default()
            .data(BarGroup::default().bars(&bars))
            .bar_width(bar_width)
            .bar_gap(1)
            .render(area, buffer);
    }
}

fn validate_chart_label(
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

fn validate_accessibility(value: &str, path: &str) -> Result<(), ComponentValidationError> {
    validate_text(
        value,
        MAX_ACCESSIBILITY_BYTES,
        &format!("{path}.accessibilityText"),
    )?;
    if value.trim().is_empty() {
        return Err(ComponentValidationError::new(
            format!("{path}.accessibilityText"),
            "must not be empty",
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
    fn normalizes_numeric_bars_and_validates_closed_emphasis() {
        let chart = BarChart::new(
            "revenue",
            [
                BarChartBar::new("Jan", 2.0),
                BarChartBar::new("Feb", 4.0).emphasis(BarChartEmphasis::Accent),
                BarChartBar::new("Mar", 1.0).emphasis(BarChartEmphasis::Danger),
            ],
            "Revenue by month",
        );
        assert_eq!(chart.normalized_values(), vec![0.5, 1.0, 0.25]);
        assert!(chart.validate("barChart").is_ok());
        assert!(
            BarChart::new("invalid", [BarChartBar::new("Bad", -1.0)], "Invalid",)
                .validate("barChart")
                .is_err()
        );
    }

    #[test]
    fn renders_through_ratatui_bar_chart() {
        let chart = BarChart::new(
            "bars",
            [
                BarChartBar::new("A", 1.0),
                BarChartBar::new("B", 2.0).value_caption("2k"),
            ],
            "A one, B two",
        );
        let mut terminal = Terminal::new(TestBackend::new(12, 5)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(chart.widget(), frame.area()))
            .unwrap();
        assert!(terminal.backend().buffer().content().iter().any(|cell| {
            matches!(cell.symbol(), "█" | "▇" | "▆" | "▅" | "▄" | "▃" | "▂" | "▁")
        }));
    }
}
