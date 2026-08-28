//! Syntect-based syntax colors for diff content lines.
//!
//! Only the code after the `+`/`-`/space prefix is highlighted; headers,
//! hunk markers, and unknown file types keep the viewer's plain styling.
//! Colors are foreground-only so the diff row tints and selection highlight
//! stay in charge of backgrounds.

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use unpeel_app_kit::ColorScheme;

use crate::git::DiffDocument;

/// Documents beyond this size render un-highlighted rather than stalling the
/// UI thread on a single parse pass.
const MAX_HIGHLIGHT_LINES: usize = 4000;

/// One foreground-colored fragment of a line's code (prefix excluded).
pub type ColoredSpan = (Color, String);

/// Per-document colors: one entry per diff line, `None` for rows that keep
/// the viewer's plain styling (headers, meta, `\ No newline` markers).
pub type DocumentColors = Vec<Option<Vec<ColoredSpan>>>;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

impl Highlighter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            themes: ThemeSet::load_defaults(),
        }
    }

    /// Colors for every line of the document, or `None` when the file type
    /// is unknown or the document is too large to highlight responsively.
    #[must_use]
    pub fn document_colors(
        &self,
        document: &DiffDocument,
        scheme: ColorScheme,
    ) -> Option<DocumentColors> {
        if document.lines.len() > MAX_HIGHLIGHT_LINES {
            return None;
        }
        let syntax = self.syntax_for(document)?;
        let mut lines = HighlightLines::new(syntax, self.theme(scheme));
        Some(
            document
                .lines
                .iter()
                .map(|line| {
                    if !is_content_line(line) {
                        return None;
                    }
                    let code = format!("{}\n", &line[1..]);
                    let regions = lines.highlight_line(&code, &self.syntaxes).ok()?;
                    Some(
                        regions
                            .into_iter()
                            .map(|(style, text)| {
                                let fg = style.foreground;
                                (
                                    Color::Rgb(fg.r, fg.g, fg.b),
                                    text.trim_end_matches('\n').to_owned(),
                                )
                            })
                            .filter(|(_, text)| !text.is_empty())
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    fn theme(&self, scheme: ColorScheme) -> &Theme {
        let name = match scheme {
            ColorScheme::Dark => "base16-eighties.dark",
            ColorScheme::Light => "InspiredGitHub",
        };
        &self.themes.themes[name]
    }

    fn syntax_for(&self, document: &DiffDocument) -> Option<&SyntaxReference> {
        let path = document.file.path();
        let extension = path.extension().and_then(|extension| extension.to_str());
        let file_name = path.file_name().and_then(|name| name.to_str());
        extension
            .and_then(|extension| self.syntaxes.find_syntax_by_extension(extension))
            .or_else(|| file_name.and_then(|name| self.syntaxes.find_syntax_by_extension(name)))
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// A patch body line whose text after the one-character prefix is real file
/// content. Headers ("+++", "---", "@@", "diff --git", "index ") and the
/// `\ No newline` marker are not.
fn is_content_line(line: &str) -> bool {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
        return false;
    }
    matches!(line.as_bytes().first(), Some(b'+' | b'-' | b' '))
}

#[cfg(test)]
mod tests {
    use crate::git::ChangedFile;

    use super::*;

    fn rust_document() -> DiffDocument {
        DiffDocument {
            file: ChangedFile::fixture("src/ui.rs", ' ', 'M'),
            lines: vec![
                "diff --git a/src/ui.rs b/src/ui.rs".into(),
                "@@ -1 +1 @@".into(),
                "-let old = 1;".into(),
                "+let renewed = 2;".into(),
            ],
            additions: 1,
            deletions: 1,
        }
    }

    #[test]
    fn rust_content_lines_get_colored_spans_and_headers_stay_plain() {
        let highlighter = Highlighter::new();
        let colors = highlighter
            .document_colors(&rust_document(), ColorScheme::Dark)
            .unwrap();

        assert_eq!(colors.len(), 4);
        assert!(colors[0].is_none(), "diff header stays plain");
        assert!(colors[1].is_none(), "hunk header stays plain");
        let added = colors[3].as_ref().unwrap();
        let text = added
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>();
        assert_eq!(text, "let renewed = 2;");
        assert!(
            added.len() > 1,
            "the keyword should color differently from the identifier"
        );
    }

    #[test]
    fn unknown_file_types_and_huge_documents_skip_highlighting() {
        let highlighter = Highlighter::new();
        let mut document = rust_document();
        document.file = ChangedFile::fixture("notes.unknown-ext", ' ', 'M');
        assert!(
            highlighter
                .document_colors(&document, ColorScheme::Dark)
                .is_none()
        );

        let mut huge = rust_document();
        huge.lines = vec![String::from("+x"); MAX_HIGHLIGHT_LINES + 1];
        assert!(
            highlighter
                .document_colors(&huge, ColorScheme::Dark)
                .is_none()
        );
    }
}
