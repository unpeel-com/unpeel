//! Closed Markdown presentation values shared by the standalone editor and
//! the optional semantic protocol.

use serde::{Deserialize, Serialize};

/// The closed visibility rules understood by every Markdown renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MarkdownCommandHintVisibility {
    /// Show beside a collapsed caret on an empty logical line, except inside
    /// a fenced code block or while the App-owned insert Menu is open.
    CursorOnEmptyLineOutsideCodeFence,
}

/// App-owned ghost text associated with Markdown command discovery.
///
/// The text and its visibility rule are semantic component state. A renderer
/// may choose native typography, but it must not invent another rule or hint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownCommandHint {
    pub text: String,
    pub visibility: MarkdownCommandHintVisibility,
}

impl MarkdownCommandHint {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            visibility: MarkdownCommandHintVisibility::CursorOnEmptyLineOutsideCodeFence,
        }
    }

    /// Resolves the declared visibility rule from authoritative editor state.
    ///
    /// `document_placeholder` takes precedence for a completely empty
    /// document; it is a separate specification field with separate meaning.
    #[must_use]
    pub fn is_visible(
        &self,
        document: &str,
        cursor_line: usize,
        selection_collapsed: bool,
        insert_menu_open: bool,
        document_placeholder: &str,
    ) -> bool {
        match self.visibility {
            MarkdownCommandHintVisibility::CursorOnEmptyLineOutsideCodeFence => {
                selection_collapsed
                    && !insert_menu_open
                    && (!document.is_empty() || document_placeholder.is_empty())
                    && document
                        .split('\n')
                        .nth(cursor_line)
                        .is_some_and(str::is_empty)
                    && !in_code_fence(document, cursor_line)
            }
        }
    }
}

fn in_code_fence(document: &str, cursor_line: usize) -> bool {
    let mut in_fence = false;
    for (index, line) in document.split('\n').enumerate() {
        if index > cursor_line {
            break;
        }
        if line.trim_start().starts_with("```") {
            if index == cursor_line {
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
    fn command_hint_rule_is_closed_and_document_placeholder_takes_precedence() {
        let hint = MarkdownCommandHint::new("Type '/' for commands");
        assert!(hint.is_visible("title\n\nbody", 1, true, false, "Write Markdown…"));
        assert!(!hint.is_visible("title\ntext", 1, true, false, ""));
        assert!(!hint.is_visible("```\n\n```", 1, true, false, ""));
        assert!(!hint.is_visible("title\n\nbody", 1, false, false, ""));
        assert!(!hint.is_visible("title\n\nbody", 1, true, true, ""));
        assert!(!hint.is_visible("", 0, true, false, "Write Markdown…"));
        assert!(hint.is_visible("", 0, true, false, ""));
    }
}
