use std::io;
use std::path::Path;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::Frame;
use unpeel_app_kit::{
    Content, ContentFont, ContentLine, ContentState, FooterAction, InputField, KitTheme, ListState,
    Page, PageTheme, UiNode,
};

/// Semantic action that opens the preview modal for the targeted file.
pub const PREVIEW_ACTION: &str = "preview";
/// Semantic action that closes the preview modal.
pub const PREVIEW_CLOSE_ACTION: &str = "preview-close";
/// Node id of the modal Page while a preview is open.
pub const PREVIEW_NODE_ID: &str = "file-preview";
/// Content id inside the modal Page.
pub const PREVIEW_CONTENT_ID: &str = "file-preview-content";
/// Marker line id appended when the file is truncated to the line cap.
pub const PREVIEW_TRUNCATED_LINE_ID: &str = "preview-truncated";

/// Files larger than this are not read; the modal shows a notice instead.
pub const PREVIEW_MAX_BYTES: u64 = 512 * 1024;
/// At most this many file lines reach the modal; the rest are truncated.
pub const PREVIEW_MAX_LINES: usize = 5_000;
/// Display width cap per line so one minified line cannot break validation.
pub const PREVIEW_MAX_COLS: usize = 2_000;
/// Only this prefix is probed for NUL bytes when detecting binary files.
const BINARY_PROBE_LEN: usize = 8 * 1024;
/// Horizontal scroll step in columns.
const HORIZONTAL_STEP: i32 = 4;
/// Page scroll height before the first frame sets the real viewport.
const FALLBACK_PAGE_ROWS: u16 = 10;

/// Read-only modal document plus its renderer-local scroll state.
pub struct PreviewModal {
    title: String,
    page: Page,
    input: InputField,
    list_state: ListState,
    content_state: ContentState,
    line_count: usize,
    viewport_rows: u16,
}

impl PreviewModal {
    /// Loads `path` for in-situ preview. Fails only for non-regular files
    /// (directories, devices) and metadata errors; binary, oversized, or
    /// unreadable files become notice documents instead of errors.
    pub fn open(path: &Path) -> io::Result<Self> {
        let (summary, content) = load_content(path)?;
        let raw = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let mut title = sanitize_line(raw.as_ref());
        if title.trim().is_empty() {
            title = "preview".to_string();
        }
        let line_count = content.lines.len();
        let page = Page::with_content(summary, content)
            .back_action(PREVIEW_CLOSE_ACTION)
            .footer_actions([FooterAction::new(
                "preview-close",
                "close",
                PREVIEW_CLOSE_ACTION,
            )
            .accelerator("escape")]);
        Ok(Self {
            title,
            page,
            input: InputField::new(""),
            list_state: ListState::default(),
            content_state: ContentState::new(),
            line_count,
            viewport_rows: 0,
        })
    }

    /// Semantic projection of the open modal: a read-only Content Page with
    /// a back action and a footer close action. No selection is published,
    /// so no select action is declared either.
    pub fn semantic_node(&self) -> UiNode {
        UiNode::page(PREVIEW_NODE_ID, self.page.clone())
    }

    pub fn scroll_vertical(&mut self, delta: i32) {
        self.content_state
            .scroll_vertical(delta, self.line_count);
    }

    pub fn scroll_horizontal(&mut self, delta: i32) {
        self.content_state.scroll_horizontal(delta);
    }

    /// Scrolls by the last rendered viewport height, falling back to one
    /// screenful guess before the first frame.
    pub fn scroll_page(&mut self, direction: i32) {
        let rows = if self.viewport_rows == 0 {
            FALLBACK_PAGE_ROWS
        } else {
            self.viewport_rows
        } as i32;
        self.content_state
            .scroll_vertical(direction.saturating_mul(rows), self.line_count);
    }

    pub fn go_home(&mut self) {
        self.content_state.set_offsets(0, 0);
    }

    pub fn go_end(&mut self) {
        let maximum = self.content_state.max_vertical_offset(self.line_count);
        let horizontal = self.content_state.horizontal_offset();
        self.content_state.set_offsets(maximum, horizontal);
    }

    /// Renders the centered modal with its own background so the tree
    /// behind never bleeds through.
    pub fn render(&mut self, frame: &mut Frame<'_>, theme: KitTheme) {
        let rect = preview_rect(frame.area());
        if rect.is_empty() {
            return;
        }
        frame.render_widget(Clear, rect);
        let max_title = rect.width.saturating_sub(4) as usize;
        let title: String = self.title.chars().take(max_title.max(1)).collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme.accent))
            .title(title);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if inner.is_empty() {
            return;
        }
        let widget = self
            .page
            .widget_with_content_state(
                &mut self.input,
                &mut self.list_state,
                &mut self.content_state,
            )
            .theme(PageTheme::for_theme(theme));
        frame.render_widget(widget, inner);
        self.viewport_rows = self.content_state.viewport_rows();
    }
}

/// Centered rectangle covering 80% of the viewport in both axes.
pub fn preview_rect(area: Rect) -> Rect {
    if area.is_empty() {
        return area;
    }
    let width = ((area.width as u32 * 80) / 100).clamp(1, area.width as u32) as u16;
    let height = ((area.height as u32 * 80) / 100).clamp(1, area.height as u32) as u16;
    let x = area
        .x
        .saturating_add(area.width.saturating_sub(width) / 2);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width, height)
}

/// Builds the modal document for `path` and its one-line summary.
/// Directories and other non-regular files are rejected; everything else
/// becomes either text lines or a notice document, never an error.
pub fn load_content(path: &Path) -> io::Result<(String, Content)> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Preview works on files",
        ));
    }
    let size = metadata.len();
    let raw = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let name = sanitize_line(raw.as_ref());
    let notice = |summary: String, lines: Vec<ContentLine>| {
        let content = Content::new(PREVIEW_CONTENT_ID, name.clone(), lines)
            .wrap(false)
            .font(ContentFont::Monospace);
        (summary, content)
    };
    if size > PREVIEW_MAX_BYTES {
        return Ok(notice(
            format!("{name} - too large to preview - {}", format_size(size)),
            vec![
                ContentLine::new(
                    "preview-line-1",
                    format!(
                        "This file is too large to preview ({}).",
                        format_size(size)
                    ),
                ),
                ContentLine::new(
                    "preview-line-2",
                    format!(
                        "Preview shows text files up to {}.",
                        format_size(PREVIEW_MAX_BYTES)
                    ),
                ),
                ContentLine::new("preview-line-3", "Open in editor to view it."),
            ],
        ));
    }
    let bytes = match read_bounded(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            let reason = sanitize_line(&error.to_string());
            return Ok(notice(
                format!("{name} - unreadable"),
                vec![
                    ContentLine::new("preview-line-1", format!("Could not read file: {reason}.")),
                    ContentLine::new("preview-line-2", "Open in editor to view it."),
                ],
            ));
        }
    };
    let probe = bytes.len().min(BINARY_PROBE_LEN);
    if bytes[..probe].contains(&0) {
        return Ok(notice(
            format!("{name} - binary file - {}", format_size(size)),
            vec![
                ContentLine::new(
                    "preview-line-1",
                    format!("This file looks binary ({}).", format_size(size)),
                ),
                ContentLine::new("preview-line-2", "Preview shows text files only."),
                ContentLine::new("preview-line-3", "Open in editor to view it."),
            ],
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let total = text.lines().count();
    if total == 0 {
        let content = Content::new(PREVIEW_CONTENT_ID, name.clone(), Vec::new())
            .wrap(false)
            .font(ContentFont::Monospace)
            .empty_message("Empty file");
        return Ok((format!("{name} - empty file"), content));
    }
    let mut lines: Vec<ContentLine> = text
        .lines()
        .take(PREVIEW_MAX_LINES)
        .enumerate()
        .map(|(index, line)| {
            ContentLine::new(format!("preview-line-{}", index + 1), fit_line(line))
        })
        .collect();
    let summary = if total > PREVIEW_MAX_LINES {
        lines.push(ContentLine::new(
            PREVIEW_TRUNCATED_LINE_ID,
            format!("... showing first {PREVIEW_MAX_LINES} of {total} lines"),
        ));
        format!(
            "{name} - first {PREVIEW_MAX_LINES} of {total} lines - {}",
            format_size(size)
        )
    } else {
        format!("{name} - {total} lines - {}", format_size(size))
    };
    let content = Content::new(PREVIEW_CONTENT_ID, name, lines)
        .wrap(false)
        .font(ContentFont::Monospace);
    Ok((summary, content))
}

/// Reads at most the preview cap plus one byte, so a file that grows
/// between the metadata check and the read still cannot hang the UI.
fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(PREVIEW_MAX_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Expands tabs to 4-column stops and replaces remaining control
/// characters with spaces, matching the diffs renderer parity rule.
pub fn sanitize_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut column = 0usize;
    for character in text.chars() {
        if character == '\t' {
            let spaces = 4 - (column % 4);
            for _ in 0..spaces {
                out.push(' ');
            }
            column += spaces;
        } else if character.is_control() {
            out.push(' ');
            column += 1;
        } else {
            out.push(character);
            column += 1;
        }
    }
    out
}

/// Fits one sanitized line into the per-line validation cap.
fn fit_line(text: &str) -> String {
    let clean = sanitize_line(text);
    if clean.chars().count() > PREVIEW_MAX_COLS {
        let mut out: String = clean.chars().take(PREVIEW_MAX_COLS).collect();
        out.push('…');
        out
    } else {
        clean
    }
}

/// Human-readable byte size for notice lines, e.g. `512 B`, `1.0 KiB`.
pub fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{:.1} MiB", bytes as f64 / (KIB * KIB))
    }
}

/// Horizontal scroll step, shared with the terminal key handler.
pub fn horizontal_step() -> i32 {
    HORIZONTAL_STEP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rect_takes_80_percent_centered() {
        assert_eq!(
            preview_rect(Rect::new(0, 0, 100, 40)),
            Rect::new(10, 4, 80, 32)
        );
    }

    #[test]
    fn preview_rect_clamps_to_tiny_areas() {
        let rect = preview_rect(Rect::new(5, 5, 10, 6));
        assert_eq!((rect.width, rect.height), (8, 4));
        assert!(rect.x >= 5 && rect.y >= 5);
        assert!(rect.right() <= 15 && rect.bottom() <= 11);
    }

    #[test]
    fn sanitize_expands_tabs_and_strips_controls() {
        assert_eq!(sanitize_line("a\tb\x01c"), "a   b c");
        assert_eq!(sanitize_line("\t\tx"), "        x");
        assert_eq!(sanitize_line("ok\r\n"), "ok  ");
    }

    #[test]
    fn binary_files_show_a_notice_with_size() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("blob.bin");
        let mut bytes = vec![0u8; 1024];
        bytes[0..5].copy_from_slice(b"hello");
        bytes[100] = 0;
        std::fs::write(&path, &bytes).unwrap();
        let (_, content) = load_content(&path).unwrap();
        let text: String = content
            .lines
            .iter()
            .map(ContentLine::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("binary"), "notice must name binary: {text}");
        assert!(text.contains("1.0 KiB"), "notice must carry size: {text}");
        assert!(
            text.contains("Open in editor"),
            "notice must hint: {text}"
        );
        assert_eq!(content.id, PREVIEW_CONTENT_ID);
        assert!(!content.wrap);
    }

    #[test]
    fn oversize_files_show_a_notice_with_hint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("big.txt");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(PREVIEW_MAX_BYTES + 1).unwrap();
        let (_, content) = load_content(&path).unwrap();
        let text: String = content
            .lines
            .iter()
            .map(ContentLine::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("too large"),
            "notice must name size cap: {text}"
        );
        assert!(
            text.contains("Open in editor"),
            "notice must hint: {text}"
        );
    }

    #[test]
    fn long_files_truncate_with_marker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("long.txt");
        let body = (1..=PREVIEW_MAX_LINES + 10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, body).unwrap();
        let (_, content) = load_content(&path).unwrap();
        assert_eq!(content.lines.len(), PREVIEW_MAX_LINES + 1);
        assert_eq!(
            content.lines[..PREVIEW_MAX_LINES]
                .iter()
                .enumerate()
                .map(|(index, line)| (index, line.id.clone()))
                .collect::<Vec<_>>()[0]
                .1,
            "preview-line-1"
        );
        assert_eq!(
            content.lines[PREVIEW_MAX_LINES].id,
            PREVIEW_TRUNCATED_LINE_ID
        );
    }

    #[test]
    fn long_lines_truncate_to_col_cap() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wide.txt");
        std::fs::write(&path, "x".repeat(PREVIEW_MAX_COLS + 100)).unwrap();
        let (_, content) = load_content(&path).unwrap();
        assert_eq!(content.lines.len(), 1);
        assert!(
            content.lines[0].text().chars().count() <= PREVIEW_MAX_COLS + 1
        );
    }

    #[test]
    fn empty_file_has_empty_message() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();
        let (_, content) = load_content(&path).unwrap();
        assert!(content.lines.is_empty());
        assert!(!content.empty_message.is_empty());
    }

    #[test]
    fn invalid_utf8_is_lossy_not_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("latin1.txt");
        std::fs::write(&path, [0x68, 0x69, 0xFF, 0x0A]).unwrap();
        let (_, content) = load_content(&path).unwrap();
        assert_eq!(content.lines.len(), 1);
        assert!(content.lines[0].text().contains('�'));
    }

    #[test]
    fn preview_semantic_node_is_a_page_with_close_action() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        std::fs::write(&path, "hello").unwrap();
        let modal = PreviewModal::open(&path).unwrap();
        let node = modal.semantic_node();
        assert_eq!(node.id.as_str(), PREVIEW_NODE_ID);
        let unpeel_app_kit::UiComponent::Page(page) = node.element else {
            panic!("preview must publish a Page");
        };
        assert_eq!(page.back.as_deref(), Some(PREVIEW_CLOSE_ACTION));
        assert!(
            page.footer
                .actions
                .iter()
                .any(|action| action.action == PREVIEW_CLOSE_ACTION),
            "semantic renderers need a close affordance"
        );
    }

    #[test]
    fn directories_are_rejected_not_rendered() {
        let directory = tempfile::tempdir().unwrap();
        assert!(PreviewModal::open(directory.path()).is_err());
    }
}
