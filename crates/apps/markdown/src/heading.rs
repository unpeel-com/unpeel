//! ATX heading rewrite helpers (`#` … `######`) for the current line or selection.

use tui_textarea::{CursorMove, TextArea};

/// Apply an ATX heading of `level` (1–6) or strip heading markers when `level` is 0.
pub fn apply_heading(line: &str, level: u8) -> String {
    let (indent, content) = split_indent(line);
    let body = strip_atx(content);
    if level == 0 {
        format!("{indent}{body}")
    } else {
        let marks = "#".repeat(level.min(6) as usize);
        if body.is_empty() {
            format!("{indent}{marks} ")
        } else {
            format!("{indent}{marks} {body}")
        }
    }
}

/// CommonMark ATX level when the line is a heading, otherwise `None`.
pub fn heading_level(line: &str) -> Option<u8> {
    let content = split_indent(line).1;
    atx_hash_count(content)
}

/// Inclusive row range covered by the cursor or an active selection.
///
/// Selection ranges from `tui-textarea` are half-open. A range that ends at
/// column 0 of a later line does not include that line.
pub fn selected_rows(
    cursor: (usize, usize),
    selection: Option<((usize, usize), (usize, usize))>,
    line_count: usize,
) -> (usize, usize) {
    let last = line_count.saturating_sub(1);
    let Some(((start_row, _), (end_row, end_col))) = selection else {
        return (cursor.0.min(last), cursor.0.min(last));
    };

    let mut start = start_row;
    let mut end = if end_row > start_row && end_col == 0 {
        end_row - 1
    } else {
        end_row
    };
    if end < start {
        std::mem::swap(&mut start, &mut end);
    }
    (start.min(last), end.min(last))
}

/// Replace the current line without touching the yank buffer.
pub fn replace_current_line(textarea: &mut TextArea<'_>, new_line: &str) {
    let (row, _) = textarea.cursor();
    let Some(old) = textarea.lines().get(row).cloned() else {
        return;
    };
    textarea.cancel_selection();
    textarea.move_cursor(CursorMove::Jump(clamp_u16(row), 0));
    if !old.is_empty() {
        let _ = textarea.delete_line_by_end();
    }
    if !new_line.is_empty() {
        textarea.insert_str(new_line);
    }
    textarea.move_cursor(CursorMove::End);
}

/// Turn a `/query` line into an ATX heading, keeping leading indent.
pub fn apply_slash_heading(textarea: &mut TextArea<'_>, level: u8) {
    let (row, _) = textarea.cursor();
    let Some(line) = textarea.lines().get(row) else {
        return;
    };
    let indent = split_indent(line).0.to_string();
    replace_current_line(textarea, &apply_heading(&indent, level));
}

/// Remove the `/query` token and leave the line's indent.
pub fn clear_slash_command(textarea: &mut TextArea<'_>) {
    let (row, _) = textarea.cursor();
    let Some(line) = textarea.lines().get(row) else {
        return;
    };
    let indent = split_indent(line).0.to_string();
    replace_current_line(textarea, &indent);
}

/// Rewrite every selected (or current) line to heading `level` and restore the cursor.
pub fn apply_to_textarea(textarea: &mut TextArea<'_>, level: u8) {
    let line_count = textarea.lines().len();
    if line_count == 0 {
        return;
    }

    let cursor = textarea.cursor();
    let (start, end) = selected_rows(cursor, textarea.selection_range(), line_count);
    let rewritten: Vec<String> = textarea
        .lines()
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if (start..=end).contains(&i) {
                apply_heading(line, level)
            } else {
                line.clone()
            }
        })
        .collect();

    let new_col = adjust_col(&textarea.lines()[cursor.0], &rewritten[cursor.0], cursor.1);

    textarea.cancel_selection();
    textarea.move_cursor(CursorMove::Jump(clamp_u16(start), 0));
    textarea.start_selection();
    textarea.move_cursor(CursorMove::Jump(clamp_u16(end), 0));
    textarea.move_cursor(CursorMove::End);
    textarea.cut();
    textarea.insert_str(rewritten[start..=end].join("\n"));
    textarea.move_cursor(CursorMove::Jump(clamp_u16(cursor.0), clamp_u16(new_col)));
}

pub(crate) fn split_indent(line: &str) -> (&str, &str) {
    let i = line.len() - line.trim_start().len();
    (&line[..i], &line[i..])
}

fn atx_hash_count(content: &str) -> Option<u8> {
    let hashes = content.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &content[hashes..];
    if rest.is_empty() || rest.starts_with([' ', '\t']) {
        Some(hashes as u8)
    } else {
        None
    }
}

fn strip_atx(content: &str) -> &str {
    match atx_hash_count(content) {
        Some(level) => content[level as usize..].trim_start(),
        None => content,
    }
}

fn prefix_len(line: &str) -> usize {
    let (indent, content) = split_indent(line);
    let indent_cols = indent.chars().count();
    match atx_hash_count(content) {
        Some(level) => {
            let after = &content[level as usize..];
            let space = usize::from(after.starts_with([' ', '\t']));
            indent_cols + level as usize + space
        }
        None => indent_cols,
    }
}

fn adjust_col(old_line: &str, new_line: &str, col: usize) -> usize {
    let old_prefix = prefix_len(old_line);
    let new_prefix = prefix_len(new_line);
    let new_len = new_line.chars().count();
    if col <= old_prefix {
        new_prefix.min(new_len)
    } else {
        (new_prefix + (col - old_prefix)).min(new_len)
    }
}

fn clamp_u16(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_heading_adds_and_replaces_markers() {
        assert_eq!(apply_heading("Hello", 1), "# Hello");
        assert_eq!(apply_heading("# Hello", 2), "## Hello");
        assert_eq!(apply_heading("## Hello", 3), "### Hello");
        assert_eq!(apply_heading("### Hello", 0), "Hello");
        assert_eq!(apply_heading("  list item", 1), "  # list item");
        assert_eq!(apply_heading("#not-a-heading", 1), "# #not-a-heading");
        assert_eq!(apply_heading("", 2), "## ");
    }

    #[test]
    fn heading_level_requires_space() {
        assert_eq!(heading_level("# Title"), Some(1));
        assert_eq!(heading_level("###  Title"), Some(3));
        assert_eq!(heading_level("#"), Some(1));
        assert_eq!(heading_level("#nope"), None);
        assert_eq!(heading_level("####### too many"), None);
    }

    #[test]
    fn selected_rows_include_half_open_selection() {
        assert_eq!(selected_rows((2, 4), None, 10), (2, 2));
        assert_eq!(selected_rows((0, 0), Some(((0, 0), (2, 3))), 10), (0, 2));
        assert_eq!(selected_rows((0, 0), Some(((0, 0), (2, 0))), 10), (0, 1));
    }
}
