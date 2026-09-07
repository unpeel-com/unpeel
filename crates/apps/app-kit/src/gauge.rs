//! Closed, data-first ratio Gauge component.

use crossterm::event::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Gauge as RatatuiGauge, LineGauge, Widget};
use serde::{Deserialize, Serialize};

use crate::components::{ComponentValidationError, validate_identifier, validate_text};
use crate::{ChartValue, KitTheme, TerminalPointerState};

/// Renderer capability for the Gauge component.
pub const GAUGE_COMPONENT_CAPABILITY: &str = "gauge";

const MAX_LABEL_BYTES: usize = 4 * 1024;
const MAX_ACCESSIBILITY_BYTES: usize = 16 * 1024;

/// A ratio from zero through one with one label and optional activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gauge {
    pub id: String,
    pub ratio: ChartValue,
    pub label: String,
    /// Optional App-authored value copy. When absent, every renderer uses the
    /// shared rounded percentage derived from `ratio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub accessibility_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<String>,
}

impl Gauge {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        ratio: f64,
        label: impl Into<String>,
        accessibility_text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            ratio: ChartValue::new(ratio),
            label: label.into(),
            caption: None,
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

    /// Supplies the canonical visible value copy for this ratio. This is used
    /// for domain wording such as "77% left · Resets in 5d 14h", which a
    /// renderer must never derive or invert independently.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    #[must_use]
    pub fn value_label(&self) -> String {
        self.caption
            .clone()
            .unwrap_or_else(|| format!("{}%", (self.ratio.value() * 100.0).round() as u64))
    }

    #[must_use]
    pub fn percentage_label(&self) -> String {
        format!("{}  {}", self.label, self.value_label())
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        validate_identifier(&self.id, &format!("{path}.id"))?;
        if !self.ratio.value().is_finite() || !(0.0..=1.0).contains(&self.ratio.value()) {
            return Err(ComponentValidationError::new(
                format!("{path}.ratio"),
                "must be finite and between 0 and 1 inclusive",
            ));
        }
        validate_single_line(&self.label, &format!("{path}.label"))?;
        if let Some(caption) = &self.caption {
            validate_single_line(caption, &format!("{path}.caption"))?;
        }
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

    pub(crate) fn replace_data_from(&mut self, replacement: Self) {
        self.ratio = replacement.ratio;
        self.label = replacement.label;
        self.caption = replacement.caption;
        self.accessibility_text = replacement.accessibility_text;
    }

    #[must_use]
    pub fn widget(&self) -> GaugeWidget<'_> {
        let theme = KitTheme::dark();
        GaugeWidget {
            gauge: self,
            filled_style: Style::new().fg(theme.accent),
            unfilled_style: Style::new().fg(theme.subtle),
            compact: false,
            show_label: true,
            pointer: TerminalPointerState::new(),
        }
    }
}

/// Ratatui Gauge for multi-line areas and LineGauge for one-row areas.
pub struct GaugeWidget<'a> {
    gauge: &'a Gauge,
    filled_style: Style,
    unfilled_style: Style,
    compact: bool,
    show_label: bool,
    pointer: TerminalPointerState,
}

impl GaugeWidget<'_> {
    /// Applies App Kit's terminal palette to the filled and remaining tracks.
    #[must_use]
    pub const fn theme(mut self, theme: KitTheme) -> Self {
        self.filled_style = Style::new().fg(theme.accent);
        self.unfilled_style = Style::new().fg(theme.subtle);
        self
    }

    #[must_use]
    pub const fn styles(mut self, filled: Style, unfilled: Style) -> Self {
        self.filled_style = filled;
        self.unfilled_style = unfilled;
        self
    }

    /// Uses only the App-owned value caption inside a constrained List row;
    /// the containing ListItem already presents the Gauge's semantic label.
    #[must_use]
    pub const fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Draws only the filled/remaining track. A containing ListItem uses this
    /// after laying out the same Gauge's App-owned caption beside the track.
    #[must_use]
    pub const fn without_label(mut self) -> Self {
        self.show_label = false;
        self
    }

    #[must_use]
    pub const fn pointer(mut self, pointer: TerminalPointerState) -> Self {
        self.pointer = pointer;
        self
    }
}

impl Widget for GaugeWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let ratio = self.gauge.ratio.value();
        let interaction = self
            .pointer
            .interaction_style(area, self.gauge.activate.is_some());
        let filled_style = self.filled_style.patch(interaction);
        let unfilled_style = self.unfilled_style.patch(interaction);
        let label = if !self.show_label {
            String::new()
        } else if self.compact {
            self.gauge.value_label()
        } else {
            self.gauge.percentage_label()
        };
        if area.height == 1 && label.is_empty() {
            // LineGauge always leaves one cell after its label, even an empty
            // one; a bare track should start at the first column.
            let width = usize::from(area.width);
            let filled = ((width as f64) * ratio).floor() as usize;
            for (offset, x) in (area.x..area.right()).enumerate() {
                let style = if offset < filled {
                    filled_style
                } else {
                    unfilled_style
                };
                buffer.set_string(x, area.y, "─", style);
            }
        } else if area.height == 1 {
            LineGauge::default()
                .ratio(ratio)
                .label(label)
                .filled_style(filled_style)
                .unfilled_style(unfilled_style)
                .render(area, buffer);
        } else {
            RatatuiGauge::default()
                .ratio(ratio)
                .label(label)
                .gauge_style(filled_style)
                .style(unfilled_style)
                .use_unicode(true)
                .render(area, buffer);
        }
    }
}

fn validate_single_line(value: &str, path: &str) -> Result<(), ComponentValidationError> {
    validate_text(value, MAX_LABEL_BYTES, path)?;
    if value.contains('\n') || value.trim().is_empty() {
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
    fn ratio_and_label_are_owned_by_the_spec() {
        let gauge = Gauge::new("build", 0.625, "Build", "Build is 62.5 percent complete");
        assert!(gauge.validate("gauge").is_ok());
        assert_eq!(gauge.percentage_label(), "Build  63%");
        assert!(
            Gauge::new("bad", 1.1, "Bad", "Invalid")
                .validate("gauge")
                .is_err()
        );
    }

    #[test]
    fn app_authored_caption_wins_over_renderer_percentage_copy() {
        let gauge = Gauge::new("quota", 0.77, "7-day limit", "77 percent remains")
            .caption("77% left · Resets in 5d 14h");
        assert_eq!(gauge.value_label(), "77% left · Resets in 5d 14h");
        assert_eq!(
            gauge.percentage_label(),
            "7-day limit  77% left · Resets in 5d 14h"
        );
        assert!(gauge.validate("gauge").is_ok());
    }

    #[test]
    fn uses_line_gauge_for_one_row_and_gauge_for_larger_areas() {
        let gauge = Gauge::new("build", 0.5, "Build", "Half complete");
        for height in [1, 3] {
            let mut terminal = Terminal::new(TestBackend::new(24, height)).unwrap();
            terminal
                .draw(|frame| frame.render_widget(gauge.widget(), frame.area()))
                .unwrap();
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|cell| { !cell.symbol().trim().is_empty() })
            );
        }
    }
}
