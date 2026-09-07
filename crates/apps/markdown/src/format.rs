//! Inline markdown format toggles.

use tui_textarea::{CursorMove, TextArea};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Bold,
    Italic,
    Code,
    Strike,
}

impl Mark {
    pub fn wrappers(self) -> (&'static str, &'static str) {
        match self {
            Self::Bold => ("**", "**"),
            Self::Italic => ("*", "*"),
            Self::Code => ("`", "`"),
            Self::Strike => ("~~", "~~"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::Code => "code",
            Self::Strike => "strikethrough",
        }
    }
}

pub fn toggle(textarea: &mut TextArea<'_>, mark: Mark) -> bool {
    let (left, right) = mark.wrappers();
    if let Some(selected) = selected_text(textarea) {
        let next = if let Some(inner) = unwrap(&selected, left, right) {
            inner
        } else {
            format!("{left}{selected}{right}")
        };
        let yank = textarea.yank_text();
        if !textarea.cut() {
            return false;
        }
        textarea.insert_str(&next);
        textarea.set_yank_text(yank);
        true
    } else {
        let (row, col) = textarea.cursor();
        textarea.insert_str(format!("{left}{right}"));
        textarea.move_cursor(CursorMove::Jump(
            row.min(u16::MAX as usize) as u16,
            (col + left.chars().count()).min(u16::MAX as usize) as u16,
        ));
        true
    }
}

fn unwrap(text: &str, left: &str, right: &str) -> Option<String> {
    let inner = text.strip_prefix(left)?.strip_suffix(right)?;
    Some(inner.to_string())
}

fn selected_text(textarea: &TextArea<'_>) -> Option<String> {
    let ((start_row, start_col), (end_row, end_col)) = textarea.selection_range()?;
    if (start_row, start_col) == (end_row, end_col) {
        return None;
    }
    let lines = textarea.lines();
    if start_row == end_row {
        return Some(slice_cols(
            lines.get(start_row).map(String::as_str).unwrap_or(""),
            start_col,
            end_col,
        ));
    }
    let mut out = String::new();
    out.push_str(&slice_cols(
        lines.get(start_row).map(String::as_str).unwrap_or(""),
        start_col,
        usize::MAX,
    ));
    for line in lines.iter().take(end_row).skip(start_row + 1) {
        out.push('\n');
        out.push_str(line);
    }
    out.push('\n');
    out.push_str(&slice_cols(
        lines.get(end_row).map(String::as_str).unwrap_or(""),
        0,
        end_col,
    ));
    Some(out)
}

fn slice_cols(line: &str, start: usize, end: usize) -> String {
    line.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}
