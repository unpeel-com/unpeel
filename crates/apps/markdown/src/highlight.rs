//! Lightweight markdown source highlighting for the editor buffer.

use ratatui::style::{Modifier, Style};
use tui_textarea::TextArea;

use crate::block::{self, BlockKind};
use crate::theme::Theme;

pub fn refresh(textarea: &mut TextArea<'_>, theme: Theme) {
    textarea.clear_custom_highlight();
    let lines: Vec<String> = textarea.lines().to_vec();
    let selection = textarea.selection_range();
    for (range, style, priority) in collect(&lines, theme) {
        // tui-textarea treats custom_highlight columns as UTF-8 byte offsets and
        // slices the line with them. Cursor/selection columns are character indexes.
        // A later custom span replaces the previous style entirely, so syntax
        // colors must not cover App Kit's foreground/background selection.
        for clipped in exclude_selection(range, selection) {
            textarea.custom_highlight(range_to_bytes(&lines, clipped), style, priority);
        }
    }
}

fn char_col_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

fn range_to_bytes(lines: &[String], range: Range) -> Range {
    let ((start_row, start_col), (end_row, end_col)) = range;
    let start = lines
        .get(start_row)
        .map(|line| char_col_to_byte(line, start_col))
        .unwrap_or(0);
    let end = lines
        .get(end_row)
        .map(|line| char_col_to_byte(line, end_col))
        .unwrap_or(0);
    ((start_row, start), (end_row, end))
}

type Range = ((usize, usize), (usize, usize));

fn exclude_selection(range: Range, selection: Option<Range>) -> Vec<Range> {
    let Some(sel) = selection else {
        return vec![range];
    };
    let (sel_start, sel_end) = if sel.0 <= sel.1 {
        (sel.0, sel.1)
    } else {
        (sel.1, sel.0)
    };
    let ((row, start), (end_row, end)) = range;
    if row != end_row || row < sel_start.0 || row > sel_end.0 || start >= end {
        return vec![range];
    }
    let sel_from = if row == sel_start.0 { sel_start.1 } else { 0 };
    let sel_to = if row == sel_end.0 {
        sel_end.1
    } else {
        usize::MAX
    };
    let mut parts = Vec::new();
    if start < end.min(sel_from) {
        parts.push(((row, start), (row, end.min(sel_from))));
    }
    if start.max(sel_to) < end {
        parts.push(((row, start.max(sel_to)), (row, end)));
    }
    parts
}

fn collect(lines: &[String], theme: Theme) -> Vec<(Range, Style, u8)> {
    let mut out = Vec::new();
    let mut in_fence = false;

    for (row, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            push_span(
                &mut out,
                row,
                0,
                line.chars().count(),
                fence_style(theme),
                20,
            );
            continue;
        }
        if in_fence {
            push_span(
                &mut out,
                row,
                0,
                line.chars().count(),
                code_style(theme),
                20,
            );
            continue;
        }

        let parsed = block::parse(line);
        match parsed.kind {
            BlockKind::Heading(_) => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    heading_style(theme),
                    10,
                );
            }
            BlockKind::Divider => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    divider_style(theme),
                    10,
                );
            }
            BlockKind::Todo => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    paragraph_style(theme),
                    1,
                );
                if let Some((start, end_inclusive)) = block::checkbox_cols(line) {
                    push_span(
                        &mut out,
                        row,
                        start,
                        end_inclusive + 1,
                        checkbox_style(theme),
                        12,
                    );
                }
                if parsed.checked && !parsed.body.is_empty() {
                    push_span(
                        &mut out,
                        row,
                        parsed.prefix_cols,
                        line.chars().count(),
                        done_style(theme),
                        11,
                    );
                }
            }
            _ if !line.is_empty() => {
                push_span(
                    &mut out,
                    row,
                    0,
                    line.chars().count(),
                    paragraph_style(theme),
                    1,
                );
            }
            _ => {}
        }

        highlight_inlines(row, line, &mut out, theme);
    }

    out
}

fn highlight_inlines(row: usize, line: &str, out: &mut Vec<(Range, Style, u8)>, theme: Theme) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '`')
        {
            let end = i + 1 + rel;
            push_span(out, row, i, end + 1, code_style(theme), 30);
            i = end + 1;
            continue;
        }
        if chars[i] == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(rel) = find_closing(&chars, i + 2, &['*', '*'])
        {
            let end = i + 2 + rel + 2;
            push_span(out, row, i, end, bold_style(theme), 15);
            i = end;
            continue;
        }
        if chars[i] == '*'
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == '*')
        {
            let end = i + 1 + rel;
            if end > i + 1 {
                push_span(out, row, i, end + 1, italic_style(theme), 12);
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
}

fn find_closing(chars: &[char], start: usize, needle: &[char]) -> Option<usize> {
    if start + needle.len() > chars.len() {
        return None;
    }
    chars[start..]
        .windows(needle.len())
        .position(|window| window == needle)
}

fn push_span(
    out: &mut Vec<(Range, Style, u8)>,
    row: usize,
    start: usize,
    end: usize,
    style: Style,
    priority: u8,
) {
    if end > start {
        out.push((((row, start), (row, end)), style, priority));
    }
}

fn heading_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD)
}

fn paragraph_style(theme: Theme) -> Style {
    Style::default().fg(theme.text)
}

fn code_style(theme: Theme) -> Style {
    Style::default().fg(theme.muted)
}

fn fence_style(theme: Theme) -> Style {
    Style::default().fg(theme.faint)
}

fn bold_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.strong)
        .add_modifier(Modifier::BOLD)
}

fn italic_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.text)
        .add_modifier(Modifier::ITALIC)
}

fn divider_style(theme: Theme) -> Style {
    Style::default().fg(theme.faint)
}

fn checkbox_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.strong)
        .add_modifier(Modifier::BOLD)
}

fn done_style(theme: Theme) -> Style {
    Style::default()
        .fg(theme.faint)
        .add_modifier(Modifier::CROSSED_OUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_heading_and_inline_code() {
        let theme = Theme::dark();
        let marks = collect(&["# Title".into(), "use `code` here".into()], theme);
        assert!(marks.iter().any(|(range, style, _)| {
            *range == ((0, 0), (0, 7)) && *style == heading_style(theme)
        }));
        assert_eq!(heading_style(theme).fg, Some(theme.kit.accent));
        assert!(
            marks
                .iter()
                .any(|(range, _, _)| *range == ((1, 4), (1, 10)))
        );
    }

    #[test]
    fn checkbox_highlight_includes_the_closing_bracket() {
        for line in ["- [ ] open", "- [x] done"] {
            let theme = Theme::dark();
            let marks = collect(&[line.into()], theme);
            assert!(marks.iter().any(|(range, style, priority)| {
                *range == ((0, 2), (0, 5)) && *style == checkbox_style(theme) && *priority == 12
            }));
        }
    }

    #[test]
    fn checked_todo_highlight_includes_the_first_body_character() {
        let theme = Theme::dark();
        let marks = collect(&["- [x] done".into()], theme);
        assert!(marks.iter().any(|(range, style, priority)| {
            *range == ((0, 6), (0, 10)) && *style == done_style(theme) && *priority == 11
        }));
    }

    #[test]
    fn markdown_highlights_do_not_cover_the_selection() {
        let range = ((0, 0), (0, 10));
        assert_eq!(
            exclude_selection(range, Some(((0, 2), (0, 8)))),
            vec![((0, 0), (0, 2)), ((0, 8), (0, 10))]
        );
        assert!(exclude_selection(range, Some(((0, 0), (0, 10)))).is_empty());
        assert_eq!(exclude_selection(range, None), vec![range]);
    }

    const CMD_LINE: &str =
        "Type `[] ` to start a to-do. Click the box or press ⌘↩ to check it off.";

    #[test]
    fn highlight_ranges_are_char_boundaries_on_multibyte_line() {
        let lines = [CMD_LINE.to_string()];
        let cmd = CMD_LINE.find('⌘').expect("demo line has ⌘");
        assert!(!CMD_LINE.is_char_boundary(cmd + 1));

        for (range, _, _) in collect(&lines, Theme::dark()) {
            let ((sr, sb), (er, eb)) = range_to_bytes(&lines, range);
            assert!(lines[sr].is_char_boundary(sb), "start {sb} in {range:?}");
            assert!(lines[er].is_char_boundary(eb), "end {eb} in {range:?}");
        }
    }

    #[test]
    fn refresh_renders_multibyte_line_and_selection() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;
        use tui_textarea::CursorMove;

        let mut textarea = TextArea::from([CMD_LINE]);
        refresh(&mut textarea, Theme::dark());
        let area = Rect::new(0, 0, 80, 3);
        let mut buf = Buffer::empty(area);
        Widget::render(&textarea, area, &mut buf);

        textarea.move_cursor(CursorMove::Head);
        textarea.start_selection();
        textarea.move_cursor(CursorMove::End);
        refresh(&mut textarea, Theme::dark());
        let mut buf = Buffer::empty(area);
        Widget::render(&textarea, area, &mut buf);
    }
}
