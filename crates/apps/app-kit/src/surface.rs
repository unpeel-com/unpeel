//! Reference-only Surface semantics and optional terminal embedding.
//!
//! App Kit owns only the component box and opaque Host route. Retained scenes,
//! resources, input packets, GPU rendering, and Kitty presentation remain in
//! `unpeel-surface` and its USRF channel. In particular, this module has no
//! frame-streaming representation.

use std::collections::HashSet;
use std::fmt;

use crossterm::event::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::components::{
    BUTTON_COMPONENT_CAPABILITY, Button, ComponentValidationError,
    validate_identifier as validate_component_identifier, validate_text,
};
use crate::{TerminalPointerPhase, TerminalPointerState};

/// Renderer capability for the v1 reference-only Surface component.
pub const SURFACE_COMPONENT_CAPABILITY: &str = "surface";
/// Renderer capability for the closed Surface + semantic-controls container.
pub const CANVAS_PAGE_COMPONENT_CAPABILITY: &str = "canvasPage";

const MAX_CANVAS_CONTROLS: usize = 32;
const MAX_CANVAS_TITLE_BYTES: usize = 4 * 1024;

/// Opaque route resolved and authorized by the existing workspace Host.
///
/// Neither value is a socket path, URL, credential, or USRF header field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceReference {
    pub session_id: String,
    pub stream_id: String,
}

impl SurfaceReference {
    #[must_use]
    pub fn new(session_id: impl Into<String>, stream_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            stream_id: stream_id.into(),
        }
    }
}

/// Preferred terminal footprint. Either axis may derive from the live scene
/// viewport aspect; omitting the complete object lets the pane supply the box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceCellSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<u16>,
}

impl SurfaceCellSize {
    #[must_use]
    pub const fn new(w: u16, h: u16) -> Self {
        Self {
            w: Some(w),
            h: Some(h),
        }
    }

    #[must_use]
    pub const fn width(w: u16) -> Self {
        Self {
            w: Some(w),
            h: None,
        }
    }

    #[must_use]
    pub const fn height(h: u16) -> Self {
        Self {
            w: None,
            h: Some(h),
        }
    }
}

/// Preferred native point/Web CSS-pixel footprint. Either axis may derive
/// from the live scene viewport aspect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfacePointSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<u32>,
}

impl SurfacePointSize {
    #[must_use]
    pub const fn new(w: u32, h: u32) -> Self {
        Self {
            w: Some(w),
            h: Some(h),
        }
    }

    #[must_use]
    pub const fn width(w: u32) -> Self {
        Self {
            w: Some(w),
            h: None,
        }
    }

    #[must_use]
    pub const fn height(h: u32) -> Self {
        Self {
            w: None,
            h: Some(h),
        }
    }
}

/// Current logical scene viewport supplied by the Surface presenter, never by
/// the semantic snapshot. It is used only to derive a missing layout axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceViewportSize {
    pub w: u32,
    pub h: u32,
}

impl SurfaceViewportSize {
    #[must_use]
    pub const fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }
}

/// Compositing background behind the locally rendered Surface scene.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SurfaceBackground {
    #[default]
    Transparent,
    Solid {
        color: String,
    },
}

/// Maximum input class that a presenter may forward over USRF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceInputPolicy {
    #[default]
    None,
    Pointer,
    PointerAndKeyboard,
}

/// Closed App Kit leaf that reserves a box for an out-of-band Surface stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceSpec {
    pub reference: SurfaceReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<SurfaceCellSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<SurfacePointSize>,
    #[serde(default)]
    pub background: SurfaceBackground,
    #[serde(default)]
    pub input_policy: SurfaceInputPolicy,
}

impl SurfaceSpec {
    #[must_use]
    pub fn new(reference: SurfaceReference) -> Self {
        Self {
            reference,
            cells: None,
            points: None,
            background: SurfaceBackground::Transparent,
            input_policy: SurfaceInputPolicy::None,
        }
    }

    #[must_use]
    pub const fn cells(mut self, cells: SurfaceCellSize) -> Self {
        self.cells = Some(cells);
        self
    }

    #[must_use]
    pub const fn points(mut self, points: SurfacePointSize) -> Self {
        self.points = Some(points);
        self
    }

    #[must_use]
    pub fn background(mut self, background: SurfaceBackground) -> Self {
        self.background = background;
        self
    }

    #[must_use]
    pub const fn input_policy(mut self, input_policy: SurfaceInputPolicy) -> Self {
        self.input_policy = input_policy;
        self
    }

    pub fn validate(&self) -> Result<(), SurfaceSpecError> {
        validate_identifier(&self.reference.session_id, "surface.reference.sessionId")?;
        validate_identifier(&self.reference.stream_id, "surface.reference.streamId")?;
        if let Some(cells) = self.cells {
            validate_optional_size(
                cells.w.map(u64::from),
                cells.h.map(u64::from),
                "surface.cells",
            )?;
        }
        if let Some(points) = self.points {
            validate_optional_size(
                points.w.map(u64::from),
                points.h.map(u64::from),
                "surface.points",
            )?;
        }
        if let SurfaceBackground::Solid { color } = &self.background
            && !valid_srgba(color)
        {
            return Err(SurfaceSpecError::new(
                "surface.background.color",
                "solid color must be #RRGGBB or #RRGGBBAA sRGBA",
            ));
        }
        Ok(())
    }

    /// Resolves an explicitly requested terminal size from current scene
    /// aspect. `None` means the containing terminal pane owns the dimensions.
    #[must_use]
    pub fn resolved_cells(&self, viewport: SurfaceViewportSize) -> Option<(u16, u16)> {
        let cells = self.cells?;
        Some(resolve_u16_size(cells.w, cells.h, viewport))
    }

    /// Resolves an explicitly requested native/Web size from current scene
    /// aspect. `None` means the containing presentation owns the dimensions.
    #[must_use]
    pub fn resolved_points(&self, viewport: SurfaceViewportSize) -> Option<(u32, u32)> {
        let points = self.points?;
        Some(resolve_u32_size(points.w, points.h, viewport))
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_node(&self, id: impl Into<crate::NodeId>) -> crate::UiNode {
        crate::UiNode::surface(id, self.clone())
    }
}

/// Named Surface slot used by [`CanvasPage`]. The slot is structurally closed:
/// it accepts a Surface reference and metadata, never an arbitrary `UiNode`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasSurface {
    pub id: String,
    #[serde(flatten)]
    pub surface: SurfaceSpec,
}

impl CanvasSurface {
    #[must_use]
    pub fn new(id: impl Into<String>, surface: SurfaceSpec) -> Self {
        Self {
            id: id.into(),
            surface,
        }
    }
}

/// Controls accepted by a [`CanvasPage`]'s fixed top overlay. This remains an
/// enumerated slot vocabulary; arbitrary children and layout nodes are not
/// accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CanvasControl {
    Button(Button),
}

impl CanvasControl {
    #[must_use]
    pub const fn button(button: Button) -> Self {
        Self::Button(button)
    }

    #[must_use]
    pub const fn id(&self) -> &str {
        match self {
            Self::Button(button) => button.id.as_str(),
        }
    }

    #[must_use]
    pub const fn as_button(&self) -> &Button {
        match self {
            Self::Button(button) => button,
        }
    }
}

/// Opinionated canvas screen: exactly one out-of-band Surface slot with a
/// bounded row of semantic Button controls overlaid at the top. Scene input
/// stays on USRF; every control action stays on `unpeel.ui`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasPage {
    pub title: String,
    pub surface: CanvasSurface,
    #[serde(default)]
    pub controls: Vec<CanvasControl>,
}

impl CanvasPage {
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        surface_id: impl Into<String>,
        surface: SurfaceSpec,
    ) -> Self {
        Self {
            title: title.into(),
            surface: CanvasSurface::new(surface_id, surface),
            controls: Vec::new(),
        }
    }

    #[must_use]
    pub fn control(mut self, control: CanvasControl) -> Self {
        self.controls.push(control);
        self
    }

    #[must_use]
    pub fn button(mut self, button: Button) -> Self {
        self.controls.push(CanvasControl::Button(button));
        self
    }

    #[must_use]
    pub fn required_capabilities(&self) -> Vec<&'static str> {
        let mut capabilities = vec![
            CANVAS_PAGE_COMPONENT_CAPABILITY,
            SURFACE_COMPONENT_CAPABILITY,
        ];
        if !self.controls.is_empty() {
            capabilities.push(BUTTON_COMPONENT_CAPABILITY);
        }
        capabilities
    }

    pub fn validate(&self) -> Result<(), ComponentValidationError> {
        validate_text(&self.title, MAX_CANVAS_TITLE_BYTES, "canvasPage.title")?;
        validate_component_identifier(&self.surface.id, "canvasPage.surface.id")?;
        self.surface.surface.validate().map_err(|error| {
            ComponentValidationError::new(
                error.path.replacen("surface", "canvasPage.surface", 1),
                error.message,
            )
        })?;
        if self.controls.len() > MAX_CANVAS_CONTROLS {
            return Err(ComponentValidationError::new(
                "canvasPage.controls",
                format!("must contain at most {MAX_CANVAS_CONTROLS} controls"),
            ));
        }
        let mut ids = HashSet::from([self.surface.id.clone()]);
        for (index, control) in self.controls.iter().enumerate() {
            let path = format!("canvasPage.controls[{index}]");
            match control {
                CanvasControl::Button(button) => button.validate(&path)?,
            }
            if !ids.insert(control.id().to_owned()) {
                return Err(ComponentValidationError::new(
                    format!("{path}.id"),
                    format!("duplicate component id {:?}", control.id()),
                ));
            }
        }
        Ok(())
    }

    /// Fixed terminal rectangles for the named Surface and top-controls slots.
    #[must_use]
    pub fn layout(&self, area: Rect) -> CanvasPageLayout {
        let toolbar = if area.width >= 4 && area.height >= 3 {
            Rect::new(
                area.x.saturating_add(1),
                area.y.saturating_add(1),
                area.width.saturating_sub(2),
                3.min(area.height.saturating_sub(1)),
            )
        } else {
            area
        };
        let inner = Rect::new(
            toolbar.x.saturating_add(1),
            toolbar.y.saturating_add(1),
            toolbar.width.saturating_sub(2),
            toolbar.height.saturating_sub(2).min(1),
        );
        let requested = self
            .controls
            .iter()
            .map(|control| {
                u16::try_from(control.as_button().label.width())
                    .unwrap_or(u16::MAX)
                    .saturating_add(4)
                    .max(5)
            })
            .collect::<Vec<_>>();
        let requested_total = requested
            .iter()
            .copied()
            .fold(0u16, u16::saturating_add)
            .saturating_add(u16::try_from(requested.len().saturating_sub(1)).unwrap_or(u16::MAX));
        let controls_x = inner
            .right()
            .saturating_sub(requested_total.min(inner.width));
        let mut cursor = controls_x;
        let mut controls = Vec::with_capacity(requested.len());
        for width in requested {
            let remaining = inner.right().saturating_sub(cursor);
            let width = width.min(remaining);
            controls.push(Rect::new(cursor, inner.y, width, inner.height));
            cursor = cursor.saturating_add(width).saturating_add(1);
        }
        let title = Rect::new(
            inner.x,
            inner.y,
            controls_x.saturating_sub(inner.x).saturating_sub(1),
            inner.height,
        );
        CanvasPageLayout {
            surface: area,
            toolbar,
            title,
            controls,
        }
    }

    #[must_use]
    pub const fn widget(&self, selected_control: Option<usize>) -> CanvasPageWidget<'_> {
        CanvasPageWidget {
            page: self,
            selected_control,
            theme: CanvasPageTheme::DEFAULT,
            pointer: TerminalPointerState::new(),
        }
    }

    #[must_use]
    pub fn control_at(&self, area: Rect, position: Position) -> Option<(usize, &Button)> {
        self.layout(area)
            .controls
            .iter()
            .position(|control| control.contains(position))
            .and_then(|index| {
                self.controls
                    .get(index)
                    .map(|control| (index, control.as_button()))
            })
    }

    #[must_use]
    pub fn action_for_mouse(&self, area: Rect, event: &MouseEvent) -> Option<(usize, &Button)> {
        let position = TerminalPointerState::click_position(event)?;
        self.control_at(area, position)
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action_for_mouse(&self, area: Rect, event: &MouseEvent) -> Option<crate::UiAction> {
        let (_, button) = self.action_for_mouse(area, event)?;
        Some(crate::UiAction::activate(
            button.id.clone(),
            button.action.clone(),
        ))
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_node(&self, id: impl Into<crate::NodeId>) -> crate::UiNode {
        crate::UiNode::canvas_page(id, self.clone())
    }
}

/// Terminal hit boxes for the closed CanvasPage slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CanvasPageLayout {
    pub surface: Rect,
    pub toolbar: Rect,
    pub title: Rect,
    pub controls: Vec<Rect>,
}

/// Opinionated Ratatui palette for the translucent-looking opaque toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPageTheme {
    pub toolbar: Style,
    pub title: Style,
    pub button: Style,
    pub primary: Style,
    pub destructive: Style,
    pub selected: Style,
}

impl CanvasPageTheme {
    pub const DEFAULT: Self = Self {
        toolbar: Style::new()
            .fg(Color::Rgb(244, 246, 251))
            .bg(Color::Rgb(23, 27, 36)),
        title: Style::new()
            .fg(Color::Rgb(244, 246, 251))
            .bg(Color::Rgb(23, 27, 36))
            .add_modifier(Modifier::BOLD),
        button: Style::new()
            .fg(Color::Rgb(244, 246, 251))
            .bg(Color::Rgb(49, 56, 70)),
        primary: Style::new()
            .fg(Color::Rgb(5, 18, 16))
            .bg(Color::Rgb(53, 194, 180))
            .add_modifier(Modifier::BOLD),
        destructive: Style::new()
            .fg(Color::Rgb(255, 141, 146))
            .bg(Color::Rgb(49, 56, 70)),
        selected: Style::new()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
    };
}

impl Default for CanvasPageTheme {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Ratatui overlay for CanvasPage's fixed top toolbar. The underlying Surface
/// is rendered separately by unpeel-surface and remains visible in every cell
/// outside this opaque semantic control region.
#[derive(Clone, Copy, Debug)]
pub struct CanvasPageWidget<'a> {
    page: &'a CanvasPage,
    selected_control: Option<usize>,
    theme: CanvasPageTheme,
    pointer: TerminalPointerState,
}

impl CanvasPageWidget<'_> {
    #[must_use]
    pub const fn theme(mut self, theme: CanvasPageTheme) -> Self {
        self.theme = theme;
        self
    }

    #[must_use]
    pub const fn pointer(mut self, pointer: TerminalPointerState) -> Self {
        self.pointer = pointer;
        self
    }
}

impl Widget for CanvasPageWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let layout = self.page.layout(area);
        if layout.toolbar.is_empty() {
            return;
        }
        Clear.render(layout.toolbar, buffer);
        Block::new()
            .borders(Borders::ALL)
            .style(self.theme.toolbar)
            .render(layout.toolbar, buffer);
        Paragraph::new(self.page.title.as_str())
            .style(self.theme.title)
            .render(layout.title, buffer);
        for (index, (control, area)) in self
            .page
            .controls
            .iter()
            .zip(layout.controls.iter().copied())
            .enumerate()
        {
            if area.is_empty() {
                continue;
            }
            let button = control.as_button();
            let phase = self.pointer.phase(area);
            let style = if phase != TerminalPointerPhase::Idle {
                match phase {
                    TerminalPointerPhase::Idle => unreachable!(),
                    TerminalPointerPhase::Hovered => {
                        self.theme.selected.add_modifier(Modifier::DIM)
                    }
                    TerminalPointerPhase::Pressed => {
                        self.theme.selected.add_modifier(Modifier::BOLD)
                    }
                }
            } else if self.selected_control == Some(index) {
                self.theme.selected
            } else {
                match button.role {
                    crate::ButtonRole::Default => self.theme.button,
                    crate::ButtonRole::Primary => self.theme.primary,
                    crate::ButtonRole::Destructive => self.theme.destructive,
                }
            };
            Paragraph::new(button.label.as_str())
                .alignment(Alignment::Center)
                .style(style)
                .render(area, buffer);
        }
    }
}

/// Semantic Surface validation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceSpecError {
    pub path: String,
    pub message: String,
}

impl SurfaceSpecError {
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for SurfaceSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for SurfaceSpecError {}

fn validate_identifier(value: &str, path: &str) -> Result<(), SurfaceSpecError> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
    {
        return Err(SurfaceSpecError::new(
            path,
            "value must be a portable 1..=256-byte identifier",
        ));
    }
    Ok(())
}

fn validate_optional_size(
    width: Option<u64>,
    height: Option<u64>,
    path: &str,
) -> Result<(), SurfaceSpecError> {
    if width.is_none() && height.is_none() {
        return Err(SurfaceSpecError::new(
            path,
            "at least one dimension must be present",
        ));
    }
    if width == Some(0) || height == Some(0) {
        return Err(SurfaceSpecError::new(path, "dimensions must be positive"));
    }
    Ok(())
}

fn valid_srgba(value: &str) -> bool {
    matches!(value.len(), 7 | 9)
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_u16_size(
    width: Option<u16>,
    height: Option<u16>,
    viewport: SurfaceViewportSize,
) -> (u16, u16) {
    match (width, height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (
            width,
            ratio_ceil(u32::from(width), viewport.h, viewport.w).min(u32::from(u16::MAX)) as u16,
        ),
        (None, Some(height)) => (
            ratio_ceil(u32::from(height), viewport.w, viewport.h).min(u32::from(u16::MAX)) as u16,
            height,
        ),
        (None, None) => unreachable!("validated Surface size has an axis"),
    }
}

fn resolve_u32_size(
    width: Option<u32>,
    height: Option<u32>,
    viewport: SurfaceViewportSize,
) -> (u32, u32) {
    match (width, height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, ratio_ceil(width, viewport.h, viewport.w)),
        (None, Some(height)) => (ratio_ceil(height, viewport.w, viewport.h), height),
        (None, None) => unreachable!("validated Surface size has an axis"),
    }
}

fn ratio_ceil(value: u32, numerator: u32, denominator: u32) -> u32 {
    let scaled = u64::from(value) * u64::from(numerator.max(1));
    scaled
        .div_ceil(u64::from(denominator.max(1)))
        .min(u64::from(u32::MAX)) as u32
}

#[cfg(feature = "surface-embed")]
mod terminal {
    use std::error::Error;
    use std::io::{self, Write};
    use std::path::Path;

    pub use unpeel_surface::ratatui::{SurfaceFrame, SurfaceView};

    use super::SurfaceSpec;

    /// Thin App Kit binding around the upstream terminal Surface runtime.
    ///
    /// It deliberately delegates guest execution, retained-scene encoding,
    /// local wgpu rendering, mmap frames, and Kitty lifecycle to
    /// `unpeel_surface::ratatui::SurfaceLayer`.
    pub struct Surface {
        spec: SurfaceSpec,
        layer: unpeel_surface::ratatui::SurfaceLayer,
    }

    impl Surface {
        pub fn load(
            spec: SurfaceSpec,
            guest_path: impl AsRef<Path>,
            columns: u16,
            rows: u16,
        ) -> Result<Self, Box<dyn Error>> {
            spec.validate()?;
            let layer = unpeel_surface::ratatui::SurfaceLayer::load(
                guest_path,
                columns.max(1),
                rows.max(1),
            )?;
            Ok(Self { spec, layer })
        }

        #[must_use]
        pub const fn spec(&self) -> &SurfaceSpec {
            &self.spec
        }

        #[must_use]
        pub fn layer(&self) -> &unpeel_surface::ratatui::SurfaceLayer {
            &self.layer
        }

        #[must_use]
        pub fn layer_mut(&mut self) -> &mut unpeel_surface::ratatui::SurfaceLayer {
            &mut self.layer
        }

        pub fn resize(&mut self, columns: u16, rows: u16) -> Result<(), Box<dyn Error>> {
            self.layer.resize(columns.max(1), rows.max(1))
        }

        pub fn event(&mut self, kind: i32, x: i32, y: i32) -> Result<(), Box<dyn Error>> {
            self.layer.guest_mut().event(kind, x, y)
        }

        pub fn render(
            &mut self,
            time_ms: i32,
            pointer: (i32, i32),
        ) -> Result<SurfaceFrame, Box<dyn Error>> {
            self.layer.render(time_ms, pointer)
        }

        pub fn render_at_cell(
            &mut self,
            time_ms: i32,
            column: u16,
            row: u16,
        ) -> Result<SurfaceFrame, Box<dyn Error>> {
            self.layer.render_at_cell(time_ms, column, row)
        }

        pub fn present(&mut self) -> io::Result<bool> {
            self.layer.present()
        }

        pub fn present_to(&mut self, output: &mut impl Write) -> io::Result<bool> {
            self.layer.present_to(output)
        }

        pub fn clear(&mut self) -> io::Result<()> {
            self.layer.clear()
        }

        #[must_use]
        pub fn logical_size(&self) -> (u32, u32) {
            self.layer.logical_size()
        }

        #[must_use]
        pub fn terminal_size(&self) -> (u16, u16) {
            self.layer.terminal_size()
        }

        #[must_use]
        pub fn cell_center(&self, column: u16, row: u16) -> (i32, i32) {
            self.layer.cell_center(column, row)
        }
    }
}

#[cfg(feature = "surface-embed")]
pub use terminal::{Surface, SurfaceFrame, SurfaceView};

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas_page() -> CanvasPage {
        CanvasPage::new(
            "Planet Canvas",
            "planet-canvas",
            SurfaceSpec::new(SurfaceReference::new("session-7", "canvas-planets")),
        )
        .button(Button::new("overview", "Overview", "show-overview"))
        .button(Button::new("next", "Next", "next-planet").role(crate::ButtonRole::Primary))
    }

    #[test]
    fn serializes_reference_only_contract() {
        let spec = SurfaceSpec::new(SurfaceReference::new("session-7", "planets"))
            .cells(SurfaceCellSize::width(80))
            .points(SurfacePointSize::height(600))
            .background(SurfaceBackground::Solid {
                color: "#08111fff".to_owned(),
            })
            .input_policy(SurfaceInputPolicy::PointerAndKeyboard);
        spec.validate().unwrap();
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["reference"]["sessionId"], "session-7");
        assert_eq!(json["reference"]["streamId"], "planets");
        assert_eq!(json["background"]["kind"], "solid");
        assert!(json.get("scene").is_none());
        assert!(json.get("pixels").is_none());
        assert!(json.get("socket").is_none());
    }

    #[test]
    fn derives_missing_axis_from_live_viewport() {
        let spec = SurfaceSpec::new(SurfaceReference::new("session-7", "planets"))
            .cells(SurfaceCellSize::width(60))
            .points(SurfacePointSize::height(600));
        let viewport = SurfaceViewportSize::new(960, 600);
        assert_eq!(spec.resolved_cells(viewport), Some((60, 38)));
        assert_eq!(spec.resolved_points(viewport), Some((960, 600)));
    }

    #[test]
    fn rejects_route_and_background_data_outside_closed_contract() {
        let invalid_route = SurfaceSpec::new(SurfaceReference::new("", "planets"));
        assert_eq!(
            invalid_route.validate().unwrap_err().path,
            "surface.reference.sessionId"
        );
        let invalid_color = SurfaceSpec::new(SurfaceReference::new("session", "planets"))
            .background(SurfaceBackground::Solid {
                color: "red".to_owned(),
            });
        assert_eq!(
            invalid_color.validate().unwrap_err().path,
            "surface.background.color"
        );
    }

    #[test]
    fn canvas_page_is_a_closed_surface_and_button_composition() {
        let page = canvas_page();
        page.validate().unwrap();
        assert_eq!(
            page.required_capabilities(),
            vec!["canvasPage", "surface", "button"]
        );
        let json = serde_json::to_value(&page).unwrap();
        assert_eq!(json["surface"]["reference"]["streamId"], "canvas-planets");
        assert_eq!(json["controls"][0]["type"], "button");
        assert!(json["surface"].get("scene").is_none());

        let mut duplicate = page;
        duplicate.controls[1] =
            CanvasControl::button(Button::new("planet-canvas", "Duplicate", "duplicate"));
        assert_eq!(
            duplicate.validate().unwrap_err().path,
            "canvasPage.controls[1].id"
        );
    }

    #[test]
    fn canvas_page_widget_paints_the_fixed_top_control_slot() {
        let page = canvas_page();
        let area = Rect::new(0, 0, 80, 20);
        let layout = page.layout(area);
        assert_eq!(layout.surface, area);
        assert_eq!(layout.toolbar, Rect::new(1, 1, 78, 3));
        assert_eq!(layout.controls.len(), 2);
        assert!(layout.controls[0].x > layout.title.x);

        let mut buffer = Buffer::empty(area);
        page.widget(Some(1)).render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Planet Canvas"));
        assert!(text.contains("Overview"));
        assert!(text.contains("Next"));
        let next = layout.controls[1];
        assert_eq!(buffer[(next.x, next.y)].bg, Color::White);
    }

    #[test]
    fn canvas_button_click_uses_the_declared_semantic_action() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

        let page = canvas_page();
        let area = Rect::new(0, 0, 80, 20);
        let control = page.layout(area).controls[1];
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: control.x,
            row: control.y,
            modifiers: KeyModifiers::NONE,
        };
        let (_, button) = page.action_for_mouse(area, &click).unwrap();
        assert_eq!(
            (button.id.as_str(), button.action.as_str()),
            ("next", "next-planet")
        );
        #[cfg(feature = "ui-bridge")]
        assert_eq!(
            page.ui_action_for_mouse(area, &click),
            Some(crate::UiAction::activate("next", "next-planet"))
        );

        let mut pointer = TerminalPointerState::new();
        pointer.track(&click);
        let mut buffer = Buffer::empty(area);
        page.widget(None).pointer(pointer).render(area, &mut buffer);
        assert!(
            buffer[(control.x, control.y)]
                .modifier
                .contains(Modifier::BOLD)
        );
    }
}
