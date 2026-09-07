//! Notion-style block types on top of the markdown source buffer.

use tui_textarea::{CursorMove, TextArea};

use crate::heading::{self, replace_current_line, split_indent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    Bullet,
    Numbered,
    Todo,
    Quote,
    Code,
    Divider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnterAction {
    Continue,
    ExitList,
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedBlock<'a> {
    pub indent: &'a str,
    pub kind: BlockKind,
    pub body: &'a str,
    pub prefix_cols: usize,
    pub checked: bool,
    pub number: u32,
}

impl BlockKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Paragraph => "text",
            Self::Heading(1) => "heading 1",
            Self::Heading(2) => "heading 2",
            Self::Heading(3) => "heading 3",
            Self::Heading(4) => "heading 4",
            Self::Heading(5) => "heading 5",
            Self::Heading(6) => "heading 6",
            Self::Heading(_) => "heading",
            Self::Bullet => "bulleted list",
            Self::Numbered => "numbered list",
            Self::Todo => "to-do",
            Self::Quote => "quote",
            Self::Code => "code",
            Self::Divider => "divider",
        }
    }

    pub fn is_list(self) -> bool {
        matches!(self, Self::Bullet | Self::Numbered | Self::Todo)
    }
}

pub fn parse(line: &str) -> ParsedBlock<'_> {
    let (indent, rest) = split_indent(line);
    let indent_cols = indent.chars().count();

    if is_divider(rest) {
        return ParsedBlock {
            indent,
            kind: BlockKind::Divider,
            body: "",
            prefix_cols: line.chars().count(),
            checked: false,
            number: 0,
        };
    }

    if rest.starts_with("```") {
        return ParsedBlock {
            indent,
            kind: BlockKind::Code,
            body: rest,
            prefix_cols: indent_cols,
            checked: false,
            number: 0,
        };
    }

    if let Some(level) = heading::heading_level(line) {
        let hashes = level as usize;
        let after = &rest[hashes..];
        let space = usize::from(after.starts_with([' ', '\t']));
        return ParsedBlock {
            indent,
            kind: BlockKind::Heading(level),
            body: after.trim_start(),
            prefix_cols: indent_cols + hashes + space,
            checked: false,
            number: 0,
        };
    }

    if let Some((checked, body)) = strip_todo(rest) {
        return ParsedBlock {
            indent,
            kind: BlockKind::Todo,
            body,
            prefix_cols: indent_cols + 6,
            checked,
            number: 0,
        };
    }

    if let Some(body) = strip_bullet(rest) {
        return ParsedBlock {
            indent,
            kind: BlockKind::Bullet,
            body,
            prefix_cols: indent_cols + 2,
            checked: false,
            number: 0,
        };
    }

    if let Some((number, body, marker_cols)) = strip_numbered(rest) {
        return ParsedBlock {
            indent,
            kind: BlockKind::Numbered,
            body,
            prefix_cols: indent_cols + marker_cols,
            checked: false,
            number,
        };
    }

    if let Some(body) = strip_quote(rest) {
        let marker = if rest.starts_with("> ") { 2 } else { 1 };
        return ParsedBlock {
            indent,
            kind: BlockKind::Quote,
            body,
            prefix_cols: indent_cols + marker,
            checked: false,
            number: 0,
        };
    }

    ParsedBlock {
        indent,
        kind: BlockKind::Paragraph,
        body: rest,
        prefix_cols: indent_cols,
        checked: false,
        number: 0,
    }
}

pub fn apply_block(line: &str, kind: BlockKind) -> String {
    let parsed = parse(line);
    let indent = parsed.indent;
    let body = parsed.body;
    match kind {
        BlockKind::Paragraph => format!("{indent}{body}"),
        BlockKind::Heading(level) => heading::apply_heading(&format!("{indent}{body}"), level),
        BlockKind::Bullet => with_body(indent, "- ", body),
        BlockKind::Numbered => with_body(indent, "1. ", body),
        BlockKind::Todo => {
            let mark = if parsed.kind == BlockKind::Todo && parsed.checked {
                "x"
            } else {
                " "
            };
            with_body(indent, &format!("- [{mark}] "), body)
        }
        BlockKind::Quote => with_body(indent, "> ", body),
        BlockKind::Divider => format!("{indent}---"),
        BlockKind::Code => format!("{indent}```"),
    }
}

fn with_body(indent: &str, marker: &str, body: &str) -> String {
    if body.is_empty() {
        format!("{indent}{marker}")
    } else {
        format!("{indent}{marker}{body}")
    }
}

/// Turn a `/query` line into the chosen block, keeping indent.
pub fn apply_slash_line(line: &str, kind: BlockKind) -> String {
    let indent = split_indent(line).0;
    match kind {
        BlockKind::Heading(level) => heading::apply_heading(indent, level),
        BlockKind::Code => format!("{indent}```\n\n{indent}```"),
        other => apply_block(indent, other),
    }
}

pub fn apply_slash(textarea: &mut TextArea<'_>, kind: BlockKind) {
    let (row, _) = textarea.cursor();
    let Some(line) = textarea.lines().get(row) else {
        return;
    };
    let indent = split_indent(line).0.to_string();
    match kind {
        BlockKind::Heading(level) => heading::apply_slash_heading(textarea, level),
        BlockKind::Code => {
            replace_current_line(textarea, &format!("{indent}```"));
            textarea.insert_newline();
            textarea.insert_newline();
            textarea.insert_str(format!("{indent}```"));
            textarea.move_cursor(CursorMove::Up);
        }
        other => replace_current_line(textarea, &apply_slash_line(line, other)),
    }
}

pub fn apply_to_textarea(textarea: &mut TextArea<'_>, kind: BlockKind) {
    match kind {
        BlockKind::Heading(level) => heading::apply_to_textarea(textarea, level),
        BlockKind::Code => {
            let (row, _) = textarea.cursor();
            let indent = textarea
                .lines()
                .get(row)
                .map(|line| split_indent(line).0.to_string())
                .unwrap_or_default();
            replace_current_line(textarea, &format!("{indent}```"));
            textarea.insert_newline();
            textarea.insert_newline();
            textarea.insert_str(format!("{indent}```"));
            textarea.move_cursor(CursorMove::Up);
        }
        BlockKind::Divider => {
            let (row, _) = textarea.cursor();
            let indent = textarea
                .lines()
                .get(row)
                .map(|line| split_indent(line).0.to_string())
                .unwrap_or_default();
            replace_current_line(textarea, &format!("{indent}---"));
        }
        kind => {
            let line_count = textarea.lines().len();
            if line_count == 0 {
                return;
            }
            let cursor = textarea.cursor();
            let (start, end) =
                heading::selected_rows(cursor, textarea.selection_range(), line_count);
            let rewritten: Vec<String> = textarea
                .lines()
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    if !(start..=end).contains(&i) {
                        return line.clone();
                    }
                    if kind == BlockKind::Numbered {
                        let n = i - start + 1;
                        let parsed = parse(line);
                        with_body(parsed.indent, &format!("{n}. "), parsed.body)
                    } else {
                        apply_block(line, kind)
                    }
                })
                .collect();
            textarea.cancel_selection();
            textarea.move_cursor(CursorMove::Jump(clamp_u16(start), 0));
            textarea.start_selection();
            textarea.move_cursor(CursorMove::Jump(clamp_u16(end), 0));
            textarea.move_cursor(CursorMove::End);
            textarea.cut();
            textarea.insert_str(rewritten[start..=end].join("\n"));
            textarea.move_cursor(CursorMove::Jump(
                clamp_u16(cursor.0),
                clamp_u16(rewritten[cursor.0].chars().count()),
            ));
        }
    }
}

pub fn indent_lines(textarea: &mut TextArea<'_>, spaces: i32) {
    let line_count = textarea.lines().len();
    if line_count == 0 {
        return;
    }
    let cursor = textarea.cursor();
    let (start, end) = heading::selected_rows(cursor, textarea.selection_range(), line_count);
    let rewritten: Vec<String> = textarea
        .lines()
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if !(start..=end).contains(&i) {
                return line.clone();
            }
            if spaces >= 0 {
                format!("{}{line}", " ".repeat(spaces as usize))
            } else {
                let remove = (-spaces) as usize;
                let trim = line.chars().take(remove).take_while(|c| *c == ' ').count();
                line.chars().skip(trim).collect()
            }
        })
        .collect();
    textarea.cancel_selection();
    textarea.move_cursor(CursorMove::Jump(clamp_u16(start), 0));
    textarea.start_selection();
    textarea.move_cursor(CursorMove::Jump(clamp_u16(end), 0));
    textarea.move_cursor(CursorMove::End);
    textarea.cut();
    textarea.insert_str(rewritten[start..=end].join("\n"));
    textarea.move_cursor(CursorMove::Jump(
        clamp_u16(cursor.0),
        clamp_u16(
            rewritten
                .get(cursor.0)
                .map(|l| l.chars().count())
                .unwrap_or(0),
        ),
    ));
}

fn clamp_u16(value: usize) -> u16 {
    value.min(u16::MAX as usize) as u16
}

pub fn enter_action(line: &str) -> EnterAction {
    let parsed = parse(line);
    if parsed.body.is_empty() && (parsed.kind.is_list() || parsed.kind == BlockKind::Quote) {
        return EnterAction::ExitList;
    }
    if parsed.kind.is_list() || parsed.kind == BlockKind::Quote {
        EnterAction::Continue
    } else {
        EnterAction::Default
    }
}

pub fn continue_prefix(line: &str) -> String {
    let parsed = parse(line);
    match parsed.kind {
        BlockKind::Bullet => format!("{}- ", parsed.indent),
        BlockKind::Todo => format!("{}- [ ] ", parsed.indent),
        BlockKind::Numbered => format!("{}{}. ", parsed.indent, parsed.number.saturating_add(1)),
        BlockKind::Quote => format!("{}> ", parsed.indent),
        _ => parsed.indent.to_string(),
    }
}

/// If backspace should convert the block to a paragraph, return the replacement line.
pub fn backspace_to_paragraph(line: &str, col: usize) -> Option<String> {
    let parsed = parse(line);
    if matches!(
        parsed.kind,
        BlockKind::Paragraph | BlockKind::Code | BlockKind::Divider
    ) {
        return None;
    }
    if col == 0 || col > parsed.prefix_cols {
        return None;
    }
    Some(format!("{}{}", parsed.indent, parsed.body))
}

pub fn toggle_todo(line: &str) -> Option<String> {
    let parsed = parse(line);
    if parsed.kind != BlockKind::Todo {
        return None;
    }
    let mark = if parsed.checked { " " } else { "x" };
    Some(with_body(
        parsed.indent,
        &format!("- [{mark}] "),
        parsed.body,
    ))
}

/// Inclusive columns covered by `[` through `]` in a to-do marker, if any.
pub fn checkbox_cols(line: &str) -> Option<(usize, usize)> {
    let parsed = parse(line);
    if parsed.kind != BlockKind::Todo {
        return None;
    }
    let open = parsed.indent.chars().count() + 2; // '[' in "- ["
    Some((open, open + 2))
}

pub fn markdown_shortcut(line: &str) -> Option<String> {
    let (indent, rest) = split_indent(line);
    match rest {
        "[] " | "[ ] " => Some(format!("{indent}- [ ] ")),
        "[x] " | "[X] " => Some(format!("{indent}- [x] ")),
        _ => None,
    }
}

fn strip_todo(rest: &str) -> Option<(bool, &str)> {
    for (prefix, checked) in [
        ("- [ ] ", false),
        ("- [x] ", true),
        ("- [X] ", true),
        ("* [ ] ", false),
        ("* [x] ", true),
        ("* [X] ", true),
    ] {
        if let Some(body) = rest.strip_prefix(prefix) {
            return Some((checked, body));
        }
    }
    None
}

fn strip_bullet(rest: &str) -> Option<&str> {
    rest.strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
}

fn strip_numbered(rest: &str) -> Option<(u32, &str, usize)> {
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let mut chars = rest.chars();
    let number: u32 = chars
        .by_ref()
        .take(digits)
        .collect::<String>()
        .parse()
        .ok()?;
    if chars.next() != Some('.') {
        return None;
    }
    match chars.next() {
        Some(' ') | None => {
            let marker = digits + 2;
            let body = if rest.len() >= marker {
                &rest[marker..]
            } else {
                ""
            };
            Some((number, body, marker.min(rest.chars().count())))
        }
        _ => None,
    }
}

fn strip_quote(rest: &str) -> Option<&str> {
    rest.strip_prefix("> ").or_else(|| {
        if rest == ">" {
            Some("")
        } else {
            rest.strip_prefix('>')
                .filter(|tail| tail.starts_with('\t'))
                .map(|tail| tail.trim_start())
        }
    })
}

fn is_divider(rest: &str) -> bool {
    let trimmed = rest.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    let all = |needle: char| chars.len() >= 3 && chars.iter().all(|c| *c == needle);
    all('-') || all('*') || all('_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recognizes_block_types() {
        assert_eq!(parse("# Title").kind, BlockKind::Heading(1));
        assert_eq!(parse("- item").kind, BlockKind::Bullet);
        assert_eq!(parse("- [ ] task").kind, BlockKind::Todo);
        assert!(parse("- [x] done").checked);
        assert_eq!(parse("- [x] done").prefix_cols, 6);
        assert_eq!(parse("2. second").kind, BlockKind::Numbered);
        assert_eq!(parse("2. second").number, 2);
        assert_eq!(parse("> quote").kind, BlockKind::Quote);
        assert_eq!(parse("---").kind, BlockKind::Divider);
        assert_eq!(parse("hello").kind, BlockKind::Paragraph);
    }

    #[test]
    fn apply_converts_between_types() {
        assert_eq!(apply_block("Hello", BlockKind::Bullet), "- Hello");
        assert_eq!(apply_block("# Hello", BlockKind::Bullet), "- Hello");
        assert_eq!(apply_block("- Hello", BlockKind::Todo), "- [ ] Hello");
        assert_eq!(apply_block("- [x] Hello", BlockKind::Todo), "- [x] Hello");
        assert_eq!(apply_block("- [ ] Hello", BlockKind::Paragraph), "Hello");
        assert_eq!(apply_block("Hello", BlockKind::Heading(2)), "## Hello");
    }

    #[test]
    fn enter_continues_or_exits_lists() {
        assert_eq!(enter_action("- item"), EnterAction::Continue);
        assert_eq!(enter_action("- "), EnterAction::ExitList);
        assert_eq!(enter_action("- [ ] "), EnterAction::ExitList);
        assert_eq!(enter_action("# Title"), EnterAction::Default);
        assert_eq!(continue_prefix("- item"), "- ");
        assert_eq!(continue_prefix("2. item"), "3. ");
        assert_eq!(continue_prefix("- [x] item"), "- [ ] ");
    }

    #[test]
    fn backspace_at_marker_becomes_paragraph() {
        assert_eq!(backspace_to_paragraph("- item", 2), Some("item".into()));
        assert_eq!(backspace_to_paragraph("# Title", 2), Some("Title".into()));
        assert_eq!(backspace_to_paragraph("- item", 4), None);
        assert_eq!(backspace_to_paragraph("item", 1), None);
    }

    #[test]
    fn todo_toggles_and_shortcuts() {
        assert_eq!(toggle_todo("- [ ] a"), Some("- [x] a".into()));
        assert_eq!(toggle_todo("- [x] a"), Some("- [ ] a".into()));
        assert_eq!(toggle_todo("hello"), None);
        assert_eq!(markdown_shortcut("[] "), Some("- [ ] ".into()));
        assert_eq!(markdown_shortcut("  [x] "), Some("  - [x] ".into()));
        assert_eq!(checkbox_cols("- [ ] task"), Some((2, 4)));
    }

    #[test]
    fn slash_todo_replaces_query() {
        assert_eq!(apply_slash_line("/todo", BlockKind::Todo), "- [ ] ");
        assert_eq!(apply_slash_line("  /h2", BlockKind::Heading(2)), "  ## ");
    }
}
