//! Slash-command block menu (`/` on an empty line).

use crate::block::BlockKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuOrigin {
    /// Typed `/` on an empty line. Query lives in the buffer after `/`.
    Slash,
    /// Typed `\`. Applies to the current line or selection.
    Palette,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemId {
    Block(BlockKind),
    LiteralBackslash,
    ToggleAutosave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub id: ItemId,
    pub shortcut: char,
    pub name: &'static str,
    pub sample: &'static str,
    pub aliases: &'static [&'static str],
    pub primary: bool,
}

const ITEMS: &[MenuItem] = &[
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(1)),
        shortcut: '1',
        name: "Heading 1",
        sample: "#",
        aliases: &["h1", "1", "#", "heading 1", "heading1"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(2)),
        shortcut: '2',
        name: "Heading 2",
        sample: "##",
        aliases: &["h2", "2", "##", "heading 2", "heading2"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(3)),
        shortcut: '3',
        name: "Heading 3",
        sample: "###",
        aliases: &["h3", "3", "###", "heading 3", "heading3"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(4)),
        shortcut: '4',
        name: "Heading 4",
        sample: "####",
        aliases: &["h4", "4", "####", "heading 4", "heading4"],
        primary: false,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(5)),
        shortcut: '5',
        name: "Heading 5",
        sample: "#####",
        aliases: &["h5", "5", "#####", "heading 5", "heading5"],
        primary: false,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Heading(6)),
        shortcut: '6',
        name: "Heading 6",
        sample: "######",
        aliases: &["h6", "6", "######", "heading 6", "heading6"],
        primary: false,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Paragraph),
        shortcut: '0',
        name: "Text",
        sample: "paragraph",
        aliases: &["p", "0", "text", "body", "paragraph"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Bullet),
        shortcut: 'b',
        name: "Bulleted list",
        sample: "-",
        aliases: &["bullet", "bulleted", "ul", "list", "-"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Numbered),
        shortcut: 'n',
        name: "Numbered list",
        sample: "1.",
        aliases: &["numbered", "ol", "number", "1"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Todo),
        shortcut: 't',
        name: "To-do",
        sample: "[]",
        aliases: &["todo", "to-do", "task", "check", "checkbox"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Quote),
        shortcut: 'q',
        name: "Quote",
        sample: ">",
        aliases: &["quote", "blockquote", ">"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Code),
        shortcut: 'c',
        name: "Code",
        sample: "```",
        aliases: &["code", "fence", "pre"],
        primary: true,
    },
    MenuItem {
        id: ItemId::Block(BlockKind::Divider),
        shortcut: '-',
        name: "Divider",
        sample: "---",
        aliases: &["divider", "hr", "line", "---"],
        primary: true,
    },
];

const BACKSLASH: MenuItem = MenuItem {
    id: ItemId::LiteralBackslash,
    shortcut: '\\',
    name: "Backslash",
    sample: "insert \\",
    aliases: &["\\", "backslash"],
    primary: false,
};

const AUTOSAVE: MenuItem = MenuItem {
    id: ItemId::ToggleAutosave,
    shortcut: 'a',
    name: "Toggle auto-save",
    sample: "on / off",
    aliases: &["autosave", "auto-save", "save", "setting"],
    primary: false,
};

pub fn items_for(origin: MenuOrigin) -> Vec<&'static MenuItem> {
    match origin {
        MenuOrigin::Slash => ITEMS.iter().collect(),
        MenuOrigin::Palette => ITEMS.iter().chain([&BACKSLASH, &AUTOSAVE]).collect(),
    }
}

pub fn visible_items(origin: MenuOrigin, query: &str) -> Vec<&'static MenuItem> {
    let query = query.trim();
    items_for(origin)
        .into_iter()
        .filter(|item| {
            if query.is_empty() {
                item.primary || origin == MenuOrigin::Palette
            } else {
                item.matches(query)
            }
        })
        .collect()
}

impl MenuItem {
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim();
        if query.is_empty() {
            return true;
        }
        let query = query.to_ascii_lowercase();
        if self.name.to_ascii_lowercase().contains(&query) {
            return true;
        }
        self.aliases
            .iter()
            .any(|alias| alias_matches(alias, &query))
    }
}

fn alias_matches(alias: &str, query: &str) -> bool {
    if alias == query {
        return true;
    }
    if query.starts_with('#') {
        return false;
    }
    alias.starts_with(query)
}

/// `/` opens the menu when the current line is blank and not inside a fence.
pub fn can_open_slash(lines: &[String], row: usize, selecting: bool) -> bool {
    if selecting {
        return false;
    }
    let Some(line) = lines.get(row) else {
        return false;
    };
    line.trim().is_empty() && !in_code_fence(lines, row)
}

/// Text after `/` on the current line, if that line is still a slash command.
pub fn slash_query(line: &str, col: usize) -> Option<&str> {
    let indent_bytes = line.len() - line.trim_start().len();
    let rest = &line[indent_bytes..];
    let query = rest.strip_prefix('/')?;
    let indent_cols = line[..indent_bytes].chars().count();
    if col < indent_cols {
        return None;
    }
    Some(query)
}

pub fn in_code_fence(lines: &[String], row: usize) -> bool {
    let mut in_fence = false;
    for (i, line) in lines.iter().enumerate() {
        if i > row {
            break;
        }
        if line.trim_start().starts_with("```") {
            if i == row {
                return true;
            }
            in_fence = !in_fence;
        }
    }
    in_fence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_only_opens_on_blank_unfenced_lines() {
        let lines = vec![
            "# Title".into(),
            "".into(),
            "```".into(),
            "".into(),
            "```".into(),
        ];
        assert!(!can_open_slash(&lines, 0, false));
        assert!(can_open_slash(&lines, 1, false));
        assert!(!can_open_slash(&lines, 1, true));
        assert!(!can_open_slash(&lines, 3, false));
    }

    #[test]
    fn query_reads_text_after_slash() {
        assert_eq!(slash_query("/", 1), Some(""));
        assert_eq!(slash_query("/h2", 3), Some("h2"));
        assert_eq!(slash_query("  /h3", 5), Some("h3"));
        assert_eq!(slash_query("  /h3", 0), None);
        assert_eq!(slash_query("not a command", 1), None);
    }

    #[test]
    fn filter_matches_aliases_and_names() {
        let h1 = visible_items(MenuOrigin::Slash, "h1");
        assert_eq!(h1.len(), 1);
        assert_eq!(h1[0].id, ItemId::Block(BlockKind::Heading(1)));

        let h2 = visible_items(MenuOrigin::Slash, "2");
        assert_eq!(h2.len(), 1);
        assert_eq!(h2[0].id, ItemId::Block(BlockKind::Heading(2)));

        let hashes = visible_items(MenuOrigin::Slash, "###");
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].id, ItemId::Block(BlockKind::Heading(3)));

        let todo = visible_items(MenuOrigin::Slash, "todo");
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].id, ItemId::Block(BlockKind::Todo));

        assert!(visible_items(MenuOrigin::Slash, "zzz").is_empty());
        assert!(
            visible_items(MenuOrigin::Slash, "")
                .iter()
                .any(|item| item.id == ItemId::Block(BlockKind::Bullet))
        );
        assert!(
            visible_items(MenuOrigin::Slash, "")
                .iter()
                .all(|item| item.id != ItemId::Block(BlockKind::Heading(4)))
        );
        assert!(
            visible_items(MenuOrigin::Palette, "")
                .iter()
                .any(|item| item.id == ItemId::Block(BlockKind::Paragraph))
        );
        assert!(
            visible_items(MenuOrigin::Palette, "")
                .iter()
                .any(|item| item.id == ItemId::ToggleAutosave)
        );
        assert!(
            visible_items(MenuOrigin::Slash, "")
                .iter()
                .all(|item| item.id != ItemId::ToggleAutosave)
        );
    }
}
