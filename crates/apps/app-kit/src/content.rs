//! Closed read-only content vocabulary for document detail screens.
//!
//! `Content` deliberately stops short of an editor: the App owns immutable
//! styled lines, renderers own scrolling and text selection, and the only
//! optional interaction is an idempotent line-range selection plus a bounded
//! semantic context menu.

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use serde::{Deserialize, Serialize};

use crate::{ColorScheme, ComponentValidationError, KitTheme, SemanticMenu, VerticalScrollbar};

/// Renderer capability for the read-only Content body slot.
pub const CONTENT_COMPONENT_CAPABILITY: &str = "content";
/// Renderer capability for keyed line selection and context actions.
pub const CONTENT_SELECTION_CAPABILITY: &str = "contentSelection";
/// Maximum number of logical lines in one Content projection.
pub const MAX_CONTENT_LINES: usize = 100_000;
const MAX_CONTENT_BYTES: usize = 12 * 1024 * 1024;
const MAX_CONTENT_LINE_BYTES: usize = 256 * 1024;

/// Platform-appropriate typeface intent; this is not a free-form font API.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentFont {
    #[default]
    Body,
    Monospace,
}

/// Closed foreground vocabulary for styled runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentTone {
    #[default]
    Default,
    Muted,
    Accent,
    Info,
    Success,
    Warning,
    Danger,
}

/// Deliberate emphasis choices shared by terminal, native, and web.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentEmphasis {
    #[default]
    Regular,
    Strong,
    Italic,
}

/// Whole-line role used for diff backgrounds and document section hierarchy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentLineTone {
    #[default]
    Default,
    Muted,
    Header,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentRun {
    pub text: String,
    #[serde(default, skip_serializing_if = "is_default_content_tone")]
    pub tone: ContentTone,
    #[serde(default, skip_serializing_if = "is_default_content_emphasis")]
    pub emphasis: ContentEmphasis,
}

impl ContentRun {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: ContentTone::Default,
            emphasis: ContentEmphasis::Regular,
        }
    }

    #[must_use]
    pub const fn tone(mut self, tone: ContentTone) -> Self {
        self.tone = tone;
        self
    }

    #[must_use]
    pub const fn emphasis(mut self, emphasis: ContentEmphasis) -> Self {
        self.emphasis = emphasis;
        self
    }
}

const fn is_default_content_tone(tone: &ContentTone) -> bool {
    matches!(tone, ContentTone::Default)
}

const fn is_default_content_emphasis(emphasis: &ContentEmphasis) -> bool {
    matches!(emphasis, ContentEmphasis::Regular)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentLine {
    pub id: String,
    pub runs: Vec<ContentRun>,
    #[serde(default, skip_serializing_if = "is_default_content_line_tone")]
    pub tone: ContentLineTone,
}

impl ContentLine {
    #[must_use]
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            runs: vec![ContentRun::new(text)],
            tone: ContentLineTone::Default,
        }
    }

    #[must_use]
    pub fn styled(id: impl Into<String>, runs: Vec<ContentRun>) -> Self {
        Self {
            id: id.into(),
            runs,
            tone: ContentLineTone::Default,
        }
    }

    #[must_use]
    pub const fn tone(mut self, tone: ContentLineTone) -> Self {
        self.tone = tone;
        self
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

const fn is_default_content_line_tone(tone: &ContentLineTone) -> bool {
    matches!(tone, ContentLineTone::Default)
}

/// Inclusive keyed range. Keying by line id keeps selection stable when lines
/// are inserted before it and avoids cross-language integer-width ambiguity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSelection {
    pub anchor_id: String,
    pub head_id: String,
}

impl ContentSelection {
    #[must_use]
    pub fn new(anchor_id: impl Into<String>, head_id: impl Into<String>) -> Self {
        Self {
            anchor_id: anchor_id.into(),
            head_id: head_id.into(),
        }
    }

    #[must_use]
    pub fn line(id: impl Into<String>) -> Self {
        let id = id.into();
        Self::new(id.clone(), id)
    }
}

/// Scrollable, selectable, read-only styled document content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    pub id: String,
    pub label: String,
    pub lines: Vec<ContentLine>,
    #[serde(default)]
    pub wrap: bool,
    #[serde(default, skip_serializing_if = "is_default_content_font")]
    pub font: ContentFont,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub empty_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ContentSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_menu: Option<SemanticMenu>,
}

impl Content {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>, lines: Vec<ContentLine>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            lines,
            wrap: true,
            font: ContentFont::Body,
            empty_message: String::new(),
            selection: None,
            select: None,
            context_menu: None,
        }
    }

    #[must_use]
    pub const fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    #[must_use]
    pub const fn font(mut self, font: ContentFont) -> Self {
        self.font = font;
        self
    }

    #[must_use]
    pub fn empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = message.into();
        self
    }

    #[must_use]
    pub fn selected(mut self, selection: ContentSelection, action: impl Into<String>) -> Self {
        self.selection = Some(selection);
        self.select = Some(action.into());
        self
    }

    #[must_use]
    pub fn select_action(mut self, action: impl Into<String>) -> Self {
        self.select = Some(action.into());
        self
    }

    #[must_use]
    pub fn context_menu(mut self, menu: SemanticMenu) -> Self {
        self.context_menu = Some(menu);
        self
    }

    #[must_use]
    pub fn widget<'a>(&'a self, state: &'a mut ContentState) -> ContentWidget<'a> {
        ContentWidget {
            content: self,
            state,
            theme: ContentTheme::default(),
        }
    }

    pub(crate) fn validate(&self, path: &str) -> Result<(), ComponentValidationError> {
        crate::components::validate_identifier(&self.id, &format!("{path}.id"))?;
        crate::components::validate_text(&self.label, 16 * 1024, &format!("{path}.label"))?;
        crate::components::validate_text(
            &self.empty_message,
            16 * 1024,
            &format!("{path}.emptyMessage"),
        )?;
        if self.lines.len() > MAX_CONTENT_LINES {
            return Err(ComponentValidationError::new(
                format!("{path}.lines"),
                format!("Content accepts at most {MAX_CONTENT_LINES} lines"),
            ));
        }
        let mut ids = HashSet::new();
        let mut total_bytes = 0usize;
        for (line_index, line) in self.lines.iter().enumerate() {
            let line_path = format!("{path}.lines[{line_index}]");
            crate::components::validate_identifier(&line.id, &format!("{line_path}.id"))?;
            if !ids.insert(line.id.as_str()) {
                return Err(ComponentValidationError::new(
                    format!("{line_path}.id"),
                    "Content line ids must be unique",
                ));
            }
            let mut line_bytes = 0usize;
            for (run_index, run) in line.runs.iter().enumerate() {
                crate::components::validate_text(
                    &run.text,
                    MAX_CONTENT_LINE_BYTES,
                    &format!("{line_path}.runs[{run_index}].text"),
                )?;
                if run.text.contains('\n') {
                    return Err(ComponentValidationError::new(
                        format!("{line_path}.runs[{run_index}].text"),
                        "Content runs must stay within one logical line",
                    ));
                }
                line_bytes = line_bytes.saturating_add(run.text.len());
            }
            if line_bytes > MAX_CONTENT_LINE_BYTES {
                return Err(ComponentValidationError::new(
                    format!("{line_path}.runs"),
                    format!("one Content line accepts at most {MAX_CONTENT_LINE_BYTES} bytes"),
                ));
            }
            total_bytes = total_bytes.saturating_add(line_bytes);
        }
        if total_bytes > MAX_CONTENT_BYTES {
            return Err(ComponentValidationError::new(
                format!("{path}.lines"),
                format!("Content accepts at most {MAX_CONTENT_BYTES} text bytes"),
            ));
        }
        if let Some(selection) = &self.selection {
            for (field, id) in [
                ("anchorId", &selection.anchor_id),
                ("headId", &selection.head_id),
            ] {
                if !ids.contains(id.as_str()) {
                    return Err(ComponentValidationError::new(
                        format!("{path}.selection.{field}"),
                        "selection must identify a Content line",
                    ));
                }
            }
        }
        if let Some(select) = &self.select {
            crate::components::validate_identifier(select, &format!("{path}.select"))?;
        }
        if self.selection.is_some() && self.select.is_none() {
            return Err(ComponentValidationError::new(
                format!("{path}.selection"),
                "a published selection requires a select action",
            ));
        }
        if let Some(menu) = &self.context_menu {
            menu.validate().map_err(|error| {
                ComponentValidationError::new(
                    format!(
                        "{path}.contextMenu.{}",
                        error.path.trim_start_matches("menu.")
                    ),
                    error.message,
                )
            })?;
        }
        Ok(())
    }

    pub(crate) fn set_selection(
        &mut self,
        selection: Option<ContentSelection>,
    ) -> Result<(), ComponentValidationError> {
        self.selection = selection;
        self.validate("page.body")
    }

    pub(crate) fn splice_lines(
        &mut self,
        index: usize,
        delete_count: usize,
        lines: Vec<ContentLine>,
    ) -> Result<(), ComponentValidationError> {
        if index > self.lines.len() || delete_count > self.lines.len().saturating_sub(index) {
            return Err(ComponentValidationError::new(
                "delta.index",
                "Content line splice is outside the collection",
            ));
        }
        self.lines.splice(index..index + delete_count, lines);
        Ok(())
    }
}

const fn is_default_content_font(font: &ContentFont) -> bool {
    matches!(font, ContentFont::Body)
}

/// Renderer-local scroll state; it is intentionally not App state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentState {
    vertical_offset: u16,
    horizontal_offset: u16,
    viewport_rows: u16,
}

impl ContentState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            vertical_offset: 0,
            horizontal_offset: 0,
            viewport_rows: 0,
        }
    }

    pub fn scroll_vertical(&mut self, delta: i32, line_count: usize) {
        let maximum = line_count.saturating_sub(usize::from(self.viewport_rows));
        self.vertical_offset = usize::from(self.vertical_offset)
            .saturating_add_signed(delta as isize)
            .min(maximum)
            .try_into()
            .unwrap_or(u16::MAX);
    }

    pub fn scroll_horizontal(&mut self, delta: i32) {
        self.horizontal_offset = usize::from(self.horizontal_offset)
            .saturating_add_signed(delta as isize)
            .try_into()
            .unwrap_or(u16::MAX);
    }

    /// Synchronizes an App's existing renderer-local viewport while migrating
    /// onto the shared Content interpreter. The next render clamps it against
    /// the current document and viewport.
    pub const fn set_offsets(&mut self, vertical: u16, horizontal: u16) {
        self.vertical_offset = vertical;
        self.horizontal_offset = horizontal;
    }

    #[must_use]
    pub const fn vertical_offset(&self) -> u16 {
        self.vertical_offset
    }

    #[must_use]
    pub const fn horizontal_offset(&self) -> u16 {
        self.horizontal_offset
    }

    #[must_use]
    pub const fn viewport_rows(&self) -> u16 {
        self.viewport_rows
    }

    #[must_use]
    pub fn max_vertical_offset(&self, line_count: usize) -> u16 {
        line_count
            .saturating_sub(usize::from(self.viewport_rows))
            .try_into()
            .unwrap_or(u16::MAX)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentTheme {
    pub default: Style,
    pub muted: Style,
    pub accent: Style,
    pub info: Style,
    pub success: Style,
    pub warning: Style,
    pub danger: Style,
    pub header: Style,
    pub added_line: Style,
    pub removed_line: Style,
    pub selected_line: Style,
    pub scrollbar_track: Style,
    pub scrollbar_thumb: Style,
}

impl ContentTheme {
    #[must_use]
    pub const fn for_theme(theme: KitTheme) -> Self {
        let (info, success, warning, added, removed) = match theme.scheme {
            ColorScheme::Dark => (
                Color::LightBlue,
                Color::LightGreen,
                Color::LightYellow,
                Color::Rgb(20, 54, 37),
                Color::Rgb(70, 28, 34),
            ),
            ColorScheme::Light => (
                Color::Blue,
                Color::Green,
                Color::Yellow,
                Color::Rgb(222, 247, 230),
                Color::Rgb(255, 226, 229),
            ),
        };
        Self {
            default: Style::new().fg(theme.text),
            muted: Style::new().fg(theme.muted),
            accent: Style::new().fg(theme.accent),
            info: Style::new().fg(info),
            success: Style::new().fg(success),
            warning: Style::new().fg(warning),
            danger: Style::new().fg(theme.danger),
            header: Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
            added_line: Style::new().bg(added),
            removed_line: Style::new().bg(removed),
            selected_line: theme.selected_row,
            scrollbar_track: theme.scrollbar_track,
            scrollbar_thumb: theme.scrollbar_thumb,
        }
    }

    fn run(self, run: &ContentRun) -> Style {
        let style = match run.tone {
            ContentTone::Default => self.default,
            ContentTone::Muted => self.muted,
            ContentTone::Accent => self.accent,
            ContentTone::Info => self.info,
            ContentTone::Success => self.success,
            ContentTone::Warning => self.warning,
            ContentTone::Danger => self.danger,
        };
        match run.emphasis {
            ContentEmphasis::Regular => style,
            ContentEmphasis::Strong => style.add_modifier(Modifier::BOLD),
            ContentEmphasis::Italic => style.add_modifier(Modifier::ITALIC),
        }
    }
}

impl Default for ContentTheme {
    fn default() -> Self {
        Self::for_theme(KitTheme::dark())
    }
}

pub struct ContentWidget<'a> {
    content: &'a Content,
    state: &'a mut ContentState,
    theme: ContentTheme,
}

impl ContentWidget<'_> {
    #[must_use]
    pub const fn theme(mut self, theme: ContentTheme) -> Self {
        self.theme = theme;
        self
    }
}

impl Widget for ContentWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.state.viewport_rows = area.height;
        if area.is_empty() {
            return;
        }
        let maximum = self
            .content
            .lines
            .len()
            .saturating_sub(usize::from(area.height));
        self.state.vertical_offset = usize::from(self.state.vertical_offset)
            .min(maximum)
            .try_into()
            .unwrap_or(u16::MAX);
        let overflow = self.content.lines.len() > usize::from(area.height) && area.width > 1;
        let text_area = Rect {
            width: area.width.saturating_sub(u16::from(overflow)),
            ..area
        };
        let selection_range = self.content.selection.as_ref().and_then(|selection| {
            let anchor = self
                .content
                .lines
                .iter()
                .position(|line| line.id == selection.anchor_id)?;
            let head = self
                .content
                .lines
                .iter()
                .position(|line| line.id == selection.head_id)?;
            Some((anchor.min(head), anchor.max(head)))
        });
        let lines = if self.content.lines.is_empty() {
            vec![Line::styled(
                self.content.empty_message.as_str(),
                self.theme.muted,
            )]
        } else {
            self.content
                .lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let mut style = match line.tone {
                        ContentLineTone::Default => Style::new(),
                        ContentLineTone::Muted => self.theme.muted,
                        ContentLineTone::Header => self.theme.header,
                        ContentLineTone::Added => self.theme.added_line,
                        ContentLineTone::Removed => self.theme.removed_line,
                    };
                    if selection_range.is_some_and(|(start, end)| index >= start && index <= end) {
                        style = style.patch(self.theme.selected_line);
                    }
                    Line::from(
                        line.runs
                            .iter()
                            .map(|run| Span::styled(run.text.clone(), self.theme.run(run)))
                            .collect::<Vec<_>>(),
                    )
                    .style(style)
                })
                .collect()
        };
        let mut paragraph = Paragraph::new(Text::from(lines)).scroll((
            self.state.vertical_offset,
            if self.content.wrap {
                0
            } else {
                self.state.horizontal_offset
            },
        ));
        if self.content.wrap {
            paragraph = paragraph.wrap(Wrap { trim: false });
        }
        paragraph.render(text_area, buffer);
        if overflow {
            VerticalScrollbar::new(
                self.content.lines.len(),
                usize::from(area.height),
                usize::from(self.state.vertical_offset),
            )
            .track_style(self.theme.scrollbar_track)
            .thumb_style(self.theme.scrollbar_thumb)
            .render(
                Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
                buffer,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_renders_styled_selection_and_scrollbar() {
        let content = Content::new(
            "patch",
            "Patch",
            vec![
                ContentLine::new("l0", "@@ header").tone(ContentLineTone::Header),
                ContentLine::new("l1", "+added").tone(ContentLineTone::Added),
                ContentLine::new("l2", "-removed").tone(ContentLineTone::Removed),
            ],
        )
        .wrap(false)
        .font(ContentFont::Monospace)
        .selected(ContentSelection::new("l1", "l2"), "select-lines");
        content.validate("page.body").unwrap();
        let mut state = ContentState::new();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 2));
        content.widget(&mut state).render(buffer.area, &mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "@");
        assert_ne!(buffer[(0, 1)].bg, Color::Reset);
        assert_ne!(buffer[(19, 0)].symbol(), " ");
    }
}
