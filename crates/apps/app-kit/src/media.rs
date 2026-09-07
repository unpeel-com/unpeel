//! Static image semantics and optional Ratatui rendering.
//!
//! The serializable types stay lightweight so the hosted bridge can describe
//! Media without pulling image decoders into every App. Actual terminal image
//! decoding and graphics-protocol rendering is behind the `media` feature.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

use crate::TerminalPointerState;

/// Renderer capability for the v1 static Media component.
pub const MEDIA_COMPONENT_CAPABILITY: &str = "media";
/// Largest decoded payload allowed in an inline Media reference.
pub const MAX_INLINE_MEDIA_BYTES: usize = 256 * 1024;

const MAX_INLINE_BASE64_CHARACTERS: usize = MAX_INLINE_MEDIA_BYTES.div_ceil(3) * 4;
const MAX_SAFE_WIRE_INTEGER: u64 = 9_007_199_254_740_991;

/// Reference to image bytes. Large assets remain outside snapshot JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MediaSource {
    /// Trusted local renderer path on the App's shared filesystem.
    Path { path: String },
    /// Small self-contained image, bounded by [`MAX_INLINE_MEDIA_BYTES`].
    Inline { media_type: String, base64: String },
    /// Content-addressed image fetched through an existing Host route.
    Blob {
        sha256: String,
        media_type: String,
        byte_length: u64,
    },
}

impl MediaSource {
    #[must_use]
    pub fn path(path: impl Into<String>) -> Self {
        Self::Path { path: path.into() }
    }

    #[must_use]
    pub fn inline(media_type: impl Into<String>, base64: impl Into<String>) -> Self {
        Self::Inline {
            media_type: media_type.into(),
            base64: base64.into(),
        }
    }

    #[must_use]
    pub fn blob(
        sha256: impl Into<String>,
        media_type: impl Into<String>,
        byte_length: u64,
    ) -> Self {
        Self::Blob {
            sha256: sha256.into(),
            media_type: media_type.into(),
            byte_length,
        }
    }

    fn validate(&self, path: &str) -> Result<(), MediaSpecError> {
        match self {
            Self::Path { path: value } => {
                if value.is_empty() || value.len() > 4096 || value.contains('\0') {
                    return Err(MediaSpecError::new(
                        format!("{path}.path"),
                        "path must contain 1..=4096 non-NUL bytes",
                    ));
                }
            }
            Self::Inline { media_type, base64 } => {
                validate_media_type(media_type, &format!("{path}.mediaType"))?;
                if base64.len() > MAX_INLINE_BASE64_CHARACTERS {
                    return Err(MediaSpecError::new(
                        format!("{path}.base64"),
                        format!(
                            "inline image exceeds the {MAX_INLINE_MEDIA_BYTES}-byte decoded limit"
                        ),
                    ));
                }
                let decoded = BASE64_STANDARD.decode(base64).map_err(|_| {
                    MediaSpecError::new(format!("{path}.base64"), "invalid base64 image data")
                })?;
                if decoded.is_empty() || decoded.len() > MAX_INLINE_MEDIA_BYTES {
                    return Err(MediaSpecError::new(
                        format!("{path}.base64"),
                        format!(
                            "inline image must contain 1..={MAX_INLINE_MEDIA_BYTES} decoded bytes"
                        ),
                    ));
                }
            }
            Self::Blob {
                sha256,
                media_type,
                byte_length,
            } => {
                if sha256.len() != 64
                    || !sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(MediaSpecError::new(
                        format!("{path}.sha256"),
                        "sha256 must be 64 lowercase hexadecimal characters",
                    ));
                }
                validate_media_type(media_type, &format!("{path}.mediaType"))?;
                if *byte_length == 0 || *byte_length > MAX_SAFE_WIRE_INTEGER {
                    return Err(MediaSpecError::new(
                        format!("{path}.byteLength"),
                        "byteLength must be a positive cross-renderer safe integer",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Intrinsic raster dimensions in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPixelSize {
    pub w: u32,
    pub h: u32,
}

impl MediaPixelSize {
    #[must_use]
    pub const fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }
}

/// Requested terminal footprint. One axis may be derived from intrinsic aspect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaCellSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<u16>,
}

impl MediaCellSize {
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

/// Requested native/web footprint. One axis may be derived from intrinsic aspect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPointSize {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<u32>,
}

impl MediaPointSize {
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

/// Cross-platform image fitting behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaFit {
    #[default]
    Contain,
    Cover,
    Fill,
}

/// Complete static Media state shared by Ratatui, Swift, and web renderers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSpec {
    pub source: MediaSource,
    pub intrinsic: MediaPixelSize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<MediaCellSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<MediaPointSize>,
    #[serde(default)]
    pub fit: MediaFit,
    pub alt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<String>,
}

impl MediaSpec {
    #[must_use]
    pub fn new(source: MediaSource, intrinsic: MediaPixelSize, alt: impl Into<String>) -> Self {
        Self {
            source,
            intrinsic,
            cells: None,
            points: None,
            fit: MediaFit::Contain,
            alt: alt.into(),
            activate: None,
        }
    }

    #[must_use]
    pub const fn cells(mut self, cells: MediaCellSize) -> Self {
        self.cells = Some(cells);
        self
    }

    #[must_use]
    pub const fn points(mut self, points: MediaPointSize) -> Self {
        self.points = Some(points);
        self
    }

    #[must_use]
    pub const fn fit(mut self, fit: MediaFit) -> Self {
        self.fit = fit;
        self
    }

    #[must_use]
    pub fn activate(mut self, action: impl Into<String>) -> Self {
        self.activate = Some(action.into());
        self
    }

    pub fn validate(&self) -> Result<(), MediaSpecError> {
        self.source.validate("media.source")?;
        if self.intrinsic.w == 0 || self.intrinsic.h == 0 {
            return Err(MediaSpecError::new(
                "media.intrinsic",
                "intrinsic pixel dimensions must both be positive",
            ));
        }
        if let Some(cells) = self.cells {
            validate_optional_size(
                cells.w.map(u64::from),
                cells.h.map(u64::from),
                "media.cells",
            )?;
        }
        if let Some(points) = self.points {
            validate_optional_size(
                points.w.map(u64::from),
                points.h.map(u64::from),
                "media.points",
            )?;
        }
        if self.alt.len() > 16 * 1024 {
            return Err(MediaSpecError::new(
                "media.alt",
                "alt text must contain at most 16384 bytes",
            ));
        }
        if let Some(action) = &self.activate {
            validate_identifier(action, "media.activate")?;
        }
        Ok(())
    }

    /// Resolves the native/web point size, using one pixel per point by default.
    #[must_use]
    pub fn resolved_points(&self) -> (u32, u32) {
        let Some(points) = self.points else {
            return (self.intrinsic.w, self.intrinsic.h);
        };
        resolve_size(points.w, points.h, self.intrinsic.w, self.intrinsic.h)
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_node(&self, id: impl Into<crate::NodeId>) -> crate::UiNode {
        crate::UiNode::media(id, self.clone())
    }

    /// Builds the component's sole optional activation intent.
    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn activation_action(&self, id: impl Into<crate::NodeId>) -> Option<crate::UiAction> {
        self.activate
            .clone()
            .map(|action| crate::UiAction::activate(id, action))
    }

    /// Resolves a terminal image click through the declared optional action.
    #[must_use]
    pub fn action_for_mouse(&self, event: &MouseEvent, area: Rect) -> Option<&str> {
        let position = TerminalPointerState::click_position(event)?;
        area.contains(position)
            .then_some(self.activate.as_deref())
            .flatten()
    }

    #[cfg(feature = "ui-bridge")]
    #[must_use]
    pub fn ui_action_for_mouse(
        &self,
        id: impl Into<crate::NodeId>,
        event: &MouseEvent,
        area: Rect,
    ) -> Option<crate::UiAction> {
        self.action_for_mouse(event, area)
            .map(|action| crate::UiAction::activate(id, action.to_owned()))
    }
}

/// Semantic Media validation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSpecError {
    pub path: String,
    pub message: String,
}

impl MediaSpecError {
    #[must_use]
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for MediaSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for MediaSpecError {}

fn resolve_size(
    width: Option<u32>,
    height: Option<u32>,
    intrinsic_width: u32,
    intrinsic_height: u32,
) -> (u32, u32) {
    match (width, height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, ratio_ceil(width, intrinsic_height, intrinsic_width)),
        (None, Some(height)) => (
            ratio_ceil(height, intrinsic_width, intrinsic_height),
            height,
        ),
        (None, None) => (intrinsic_width, intrinsic_height),
    }
}

fn ratio_ceil(value: u32, numerator: u32, denominator: u32) -> u32 {
    let scaled = u64::from(value) * u64::from(numerator);
    scaled
        .div_ceil(u64::from(denominator.max(1)))
        .min(u64::from(u32::MAX)) as u32
}

fn validate_optional_size(
    width: Option<u64>,
    height: Option<u64>,
    path: &str,
) -> Result<(), MediaSpecError> {
    if width.is_none() && height.is_none() {
        return Err(MediaSpecError::new(
            path,
            "at least one dimension must be present",
        ));
    }
    if width == Some(0) || height == Some(0) {
        return Err(MediaSpecError::new(path, "dimensions must be positive"));
    }
    Ok(())
}

fn validate_media_type(value: &str, path: &str) -> Result<(), MediaSpecError> {
    if value.len() > 127
        || !value.starts_with("image/")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-/".contains(&byte))
    {
        return Err(MediaSpecError::new(
            path,
            "mediaType must be a portable image MIME type",
        ));
    }
    Ok(())
}

fn validate_identifier(value: &str, path: &str) -> Result<(), MediaSpecError> {
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
    {
        return Err(MediaSpecError::new(
            path,
            "action must be a portable identifier",
        ));
    }
    Ok(())
}

#[cfg(feature = "media")]
mod terminal {
    use std::{fmt, fs, io};

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
    use image::{DynamicImage, imageops::FilterType};
    use ratatui::{
        buffer::Buffer,
        layout::{Rect, Size},
        widgets::Widget,
    };
    use ratatui_image::{
        Image as RatatuiImage, Resize, errors::Errors as RatatuiImageError, picker::Picker,
        protocol::Protocol,
    };
    use sha2::{Digest, Sha256};

    use super::{MediaFit, MediaSource, MediaSpec, MediaSpecError, resolve_size};
    use crate::TerminalPointerState;

    /// Loaded terminal Media using Kitty, iTerm2, Sixel, or half-block output.
    pub struct Media {
        spec: MediaSpec,
        protocol: Protocol,
        cell_size: Size,
    }

    impl Media {
        /// Loads a local path or bounded inline source.
        ///
        /// Blob sources require the App/Host to fetch bytes through its existing
        /// authorized route and pass them to [`Self::from_resolved_bytes`].
        pub fn load(spec: MediaSpec, picker: &Picker) -> Result<Self, MediaError> {
            spec.validate()?;
            let bytes = match &spec.source {
                MediaSource::Path { path } => fs::read(path)?,
                MediaSource::Inline { base64, .. } => BASE64_STANDARD
                    .decode(base64)
                    .map_err(|_| MediaError::InvalidInlineBase64)?,
                MediaSource::Blob { .. } => return Err(MediaError::BlobResolutionRequired),
            };
            Self::build_from_bytes(spec, &bytes, picker, false)
        }

        /// Builds Media from bytes resolved outside the component.
        ///
        /// Blob byte length and SHA-256 are verified before decoding.
        pub fn from_resolved_bytes(
            spec: MediaSpec,
            bytes: &[u8],
            picker: &Picker,
        ) -> Result<Self, MediaError> {
            spec.validate()?;
            Self::build_from_bytes(spec, bytes, picker, true)
        }

        /// Builds Media from an already-decoded first/static frame.
        ///
        /// Blob references are rejected here because their length and digest
        /// can only be verified from bytes; use [`Self::from_resolved_bytes`].
        pub fn from_image(
            spec: MediaSpec,
            image: DynamicImage,
            picker: &Picker,
        ) -> Result<Self, MediaError> {
            spec.validate()?;
            if matches!(&spec.source, MediaSource::Blob { .. }) {
                return Err(MediaError::BlobResolutionRequired);
            }
            Self::build(spec, image, picker)
        }

        #[must_use]
        pub const fn spec(&self) -> &MediaSpec {
            &self.spec
        }

        #[must_use]
        pub const fn cell_size(&self) -> Size {
            self.cell_size
        }

        #[must_use]
        pub fn activate_action(&self) -> Option<&str> {
            self.spec.activate.as_deref()
        }

        #[cfg(feature = "ui-bridge")]
        #[must_use]
        pub fn ui_node(&self, id: impl Into<crate::NodeId>) -> crate::UiNode {
            self.spec.ui_node(id)
        }

        /// Builds the component's sole optional activation intent.
        #[cfg(feature = "ui-bridge")]
        #[must_use]
        pub fn activation_action(&self, id: impl Into<crate::NodeId>) -> Option<crate::UiAction> {
            self.spec.activation_action(id)
        }

        /// Ratatui view with optional renderer-local hover/press treatment.
        #[must_use]
        pub const fn widget(&self) -> MediaWidget<'_> {
            MediaWidget {
                media: self,
                pointer: TerminalPointerState::new(),
            }
        }

        fn build_from_bytes(
            spec: MediaSpec,
            bytes: &[u8],
            picker: &Picker,
            verify_blob: bool,
        ) -> Result<Self, MediaError> {
            if verify_blob {
                verify_blob_bytes(&spec.source, bytes)?;
            }
            let image = image::load_from_memory(bytes)?;
            Self::build(spec, image, picker)
        }

        fn build(
            spec: MediaSpec,
            image: DynamicImage,
            picker: &Picker,
        ) -> Result<Self, MediaError> {
            let cell_size = resolved_cell_size(&spec, picker);
            let font_size = picker.font_size();
            let pixel_width = u32::from(cell_size.width) * u32::from(font_size.width);
            let pixel_height = u32::from(cell_size.height) * u32::from(font_size.height);
            let protocol = match spec.fit {
                MediaFit::Contain => picker.new_protocol(
                    image,
                    cell_size,
                    Resize::Scale(Some(FilterType::Triangle)),
                )?,
                MediaFit::Cover => {
                    let image =
                        image.resize_to_fill(pixel_width, pixel_height, FilterType::Triangle);
                    picker.new_protocol(image, cell_size, Resize::Fit(None))?
                }
                MediaFit::Fill => {
                    let image = image.resize_exact(pixel_width, pixel_height, FilterType::Triangle);
                    picker.new_protocol(image, cell_size, Resize::Fit(None))?
                }
            };
            Ok(Self {
                spec,
                protocol,
                cell_size,
            })
        }
    }

    /// Pointer-aware terminal interpretation of a loaded static image.
    pub struct MediaWidget<'a> {
        media: &'a Media,
        pointer: TerminalPointerState,
    }

    impl MediaWidget<'_> {
        #[must_use]
        pub const fn pointer(mut self, pointer: TerminalPointerState) -> Self {
            self.pointer = pointer;
            self
        }
    }

    impl Widget for MediaWidget<'_> {
        fn render(self, area: Rect, buffer: &mut Buffer) {
            let target = Rect {
                width: area.width.min(self.media.cell_size.width),
                height: area.height.min(self.media.cell_size.height),
                ..area
            };
            RatatuiImage::new(&self.media.protocol)
                .allow_clipping(true)
                .render(target, buffer);
            buffer.set_style(
                target,
                self.pointer
                    .interaction_style(target, self.media.spec.activate.is_some()),
            );
        }
    }

    impl Widget for &Media {
        fn render(self, area: Rect, buffer: &mut Buffer) {
            self.widget().render(area, buffer);
        }
    }

    fn resolved_cell_size(spec: &MediaSpec, picker: &Picker) -> Size {
        let font = picker.font_size();
        let natural_width = spec.intrinsic.w.div_ceil(u32::from(font.width));
        let natural_height = spec.intrinsic.h.div_ceil(u32::from(font.height));
        let Some(cells) = spec.cells else {
            return Size::new(
                saturating_u16(natural_width),
                saturating_u16(natural_height),
            );
        };
        let width = cells.w.map(u32::from);
        let height = cells.h.map(u32::from);
        let (width, height) = match (width, height) {
            (Some(width), Some(height)) => (width, height),
            (Some(width), None) => {
                let pixel_width = width * u32::from(font.width);
                let pixel_height =
                    super::ratio_ceil(pixel_width, spec.intrinsic.h, spec.intrinsic.w);
                (width, pixel_height.div_ceil(u32::from(font.height)))
            }
            (None, Some(height)) => {
                let pixel_height = height * u32::from(font.height);
                let pixel_width =
                    super::ratio_ceil(pixel_height, spec.intrinsic.w, spec.intrinsic.h);
                (pixel_width.div_ceil(u32::from(font.width)), height)
            }
            (None, None) => resolve_size(None, None, natural_width, natural_height),
        };
        Size::new(saturating_u16(width), saturating_u16(height))
    }

    fn saturating_u16(value: u32) -> u16 {
        value.clamp(1, u32::from(u16::MAX)) as u16
    }

    fn verify_blob_bytes(source: &MediaSource, bytes: &[u8]) -> Result<(), MediaError> {
        let MediaSource::Blob {
            sha256,
            byte_length,
            ..
        } = source
        else {
            return Ok(());
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != *byte_length {
            return Err(MediaError::BlobLengthMismatch {
                expected: *byte_length,
                received: bytes.len(),
            });
        }
        let actual = format!("{:x}", Sha256::digest(bytes));
        if &actual != sha256 {
            return Err(MediaError::BlobDigestMismatch);
        }
        Ok(())
    }

    /// Loading or graphics-protocol preparation failure.
    #[derive(Debug)]
    pub enum MediaError {
        InvalidSpec(MediaSpecError),
        Io(io::Error),
        Decode(image::ImageError),
        TerminalProtocol(RatatuiImageError),
        InvalidInlineBase64,
        BlobResolutionRequired,
        BlobLengthMismatch { expected: u64, received: usize },
        BlobDigestMismatch,
    }

    impl fmt::Display for MediaError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::InvalidSpec(error) => {
                    write!(formatter, "invalid Media specification: {error}")
                }
                Self::Io(error) => write!(formatter, "could not read Media source: {error}"),
                Self::Decode(error) => write!(formatter, "could not decode Media image: {error}"),
                Self::TerminalProtocol(error) => {
                    write!(formatter, "could not prepare terminal Media: {error}")
                }
                Self::InvalidInlineBase64 => formatter.write_str("invalid inline Media base64"),
                Self::BlobResolutionRequired => formatter.write_str(
                    "blob Media must be resolved through the existing authorized Host route",
                ),
                Self::BlobLengthMismatch { expected, received } => write!(
                    formatter,
                    "resolved Media blob length mismatch: expected {expected}, received {received}"
                ),
                Self::BlobDigestMismatch => {
                    formatter.write_str("resolved Media blob SHA-256 mismatch")
                }
            }
        }
    }

    impl std::error::Error for MediaError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                Self::InvalidSpec(error) => Some(error),
                Self::Io(error) => Some(error),
                Self::Decode(error) => Some(error),
                Self::TerminalProtocol(error) => Some(error),
                Self::InvalidInlineBase64
                | Self::BlobResolutionRequired
                | Self::BlobLengthMismatch { .. }
                | Self::BlobDigestMismatch => None,
            }
        }
    }

    impl From<MediaSpecError> for MediaError {
        fn from(value: MediaSpecError) -> Self {
            Self::InvalidSpec(value)
        }
    }

    impl From<io::Error> for MediaError {
        fn from(value: io::Error) -> Self {
            Self::Io(value)
        }
    }

    impl From<image::ImageError> for MediaError {
        fn from(value: image::ImageError) -> Self {
            Self::Decode(value)
        }
    }

    impl From<RatatuiImageError> for MediaError {
        fn from(value: RatatuiImageError) -> Self {
            Self::TerminalProtocol(value)
        }
    }
}

#[cfg(feature = "media")]
pub use ratatui_image::picker::{Picker as MediaPicker, ProtocolType as MediaProtocolType};
#[cfg(feature = "media")]
pub use terminal::{Media, MediaError, MediaWidget};

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    fn inline_spec() -> MediaSpec {
        MediaSpec::new(
            MediaSource::inline("image/png", TINY_PNG),
            MediaPixelSize::new(1, 1),
            "Tiny pixel",
        )
        .cells(MediaCellSize::width(8))
        .points(MediaPointSize::height(40))
        .activate("open-image")
    }

    #[test]
    fn media_spec_validates_reference_sizes_and_idempotent_action() {
        let spec = inline_spec();
        spec.validate().unwrap();
        assert_eq!(spec.resolved_points(), (40, 40));
        assert_eq!(
            serde_json::to_value(&spec).unwrap()["source"]["kind"],
            "inline"
        );
    }

    #[test]
    fn inline_and_blob_references_are_strictly_bounded() {
        let mut spec = inline_spec();
        spec.source = MediaSource::inline(
            "image/png",
            BASE64_STANDARD.encode(vec![0; MAX_INLINE_MEDIA_BYTES + 1]),
        );
        assert!(spec.validate().is_err());

        spec.source = MediaSource::blob("ABC", "image/png", 1);
        assert!(spec.validate().is_err());

        spec.source = MediaSource::inline("image/png", "AB==");
        assert!(spec.validate().is_err());
    }

    #[cfg(feature = "ui-bridge")]
    #[test]
    fn media_declares_at_most_one_semantic_activate_action() {
        let action = inline_spec().activation_action("hero-image").unwrap();
        assert_eq!(action.node_id.as_str(), "hero-image");
        assert_eq!(action.action.as_str(), "open-image");
        assert_eq!(action.kind, crate::UiEventKind::Activate);
        assert_eq!(action.value, crate::UiEventValue::None);
    }

    #[test]
    fn terminal_media_click_uses_the_declared_action() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        let spec = inline_spec();
        assert_eq!(
            spec.action_for_mouse(&click, Rect::new(1, 1, 8, 4)),
            Some("open-image")
        );
        #[cfg(feature = "ui-bridge")]
        assert_eq!(
            spec.ui_action_for_mouse("hero-image", &click, Rect::new(1, 1, 8, 4)),
            spec.activation_action("hero-image")
        );
    }

    #[cfg(feature = "media")]
    #[test]
    fn terminal_media_uses_halfblocks_when_no_graphics_protocol_is_available() {
        use ratatui::{Terminal, backend::TestBackend};
        use ratatui_image::{picker::Picker, picker::ProtocolType};

        let picker = Picker::halfblocks();
        assert_eq!(picker.protocol_type(), ProtocolType::Halfblocks);
        let media = Media::load(inline_spec(), &picker).unwrap();
        assert_eq!(media.cell_size().width, 8);

        let backend = TestBackend::new(10, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(&media, frame.area()))
            .unwrap();
    }

    #[cfg(feature = "media")]
    #[test]
    fn terminal_media_verifies_resolved_blob_length_and_digest() {
        use ratatui_image::picker::Picker;

        let bytes = BASE64_STANDARD.decode(TINY_PNG).unwrap();
        let spec = MediaSpec::new(
            MediaSource::blob(
                "431ced6916a2a21a156e38701afe55bbd7f88969fbbfc56d7fe099d47f265460",
                "image/png",
                68,
            ),
            MediaPixelSize::new(1, 1),
            "Tiny pixel",
        );
        Media::from_resolved_bytes(spec.clone(), &bytes, &Picker::halfblocks()).unwrap();

        let mut wrong = spec.clone();
        wrong.source = MediaSource::blob(
            "0000000000000000000000000000000000000000000000000000000000000000",
            "image/png",
            68,
        );
        assert!(matches!(
            Media::from_resolved_bytes(wrong, &bytes, &Picker::halfblocks()),
            Err(MediaError::BlobDigestMismatch)
        ));

        assert!(matches!(
            Media::from_image(
                spec,
                image::DynamicImage::new_rgba8(1, 1),
                &Picker::halfblocks()
            ),
            Err(MediaError::BlobResolutionRequired)
        ));
    }
}
