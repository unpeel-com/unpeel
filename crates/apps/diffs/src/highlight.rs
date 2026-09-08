//! Language-aware diff tokens expressed in App Kit's shared foreground tones.
//! Old and new sides have independent parser state; row backgrounds stay
//! entirely under ContentLineTone/selection control.

use std::sync::OnceLock;
use syntect::easy::ScopeRegionIterator;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use unpeel_app_kit::{ContentRun, ContentTone};

use crate::git::DiffDocument;

const MAX_HIGHLIGHT_LINES: usize = 4000;
const MAX_HIGHLIGHT_BYTES: usize = 1024 * 1024;
const MAX_LINE_BYTES: usize = 16 * 1024;

/// Code runs exclude the one-character diff prefix. None denotes metadata.
pub type DocumentSyntax = Vec<Option<Vec<ContentRun>>>;

pub fn document_runs(document: &DiffDocument) -> Option<DocumentSyntax> {
    static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
    HIGHLIGHTER
        .get_or_init(Highlighter::new)
        .document_runs(document)
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    rules: Vec<(Scope, ContentTone)>,
}

impl Highlighter {
    pub fn new() -> Self {
        let rules = [
            ("comment", ContentTone::Muted),
            ("string", ContentTone::Success),
            ("constant", ContentTone::Warning),
            ("keyword", ContentTone::Accent),
            ("storage", ContentTone::Accent),
            ("entity.name", ContentTone::Info),
            ("support", ContentTone::Info),
            ("variable.function", ContentTone::Info),
            ("entity.other.attribute-name", ContentTone::Warning),
            ("invalid", ContentTone::Danger),
        ]
        .into_iter()
        .map(|(scope, tone)| (Scope::new(scope).unwrap(), tone))
        .collect();
        Self {
            syntaxes: two_face::syntax::extra_newlines(),
            rules,
        }
    }

    pub fn document_runs(&self, document: &DiffDocument) -> Option<DocumentSyntax> {
        if document.lines.len() > MAX_HIGHLIGHT_LINES
            || document
                .lines
                .iter()
                .any(|line| line.len() > MAX_LINE_BYTES)
            || document.lines.iter().map(String::len).sum::<usize>() > MAX_HIGHLIGHT_BYTES
        {
            return None;
        }
        let syntax = self.syntax_for(document)?;
        let mut old = LineParser::new(syntax);
        let mut new = LineParser::new(syntax);
        let mut in_hunk = false;
        let mut result = Vec::with_capacity(document.lines.len());
        for line in &document.lines {
            if line.starts_with("@@") {
                // Gaps between hunks omit source: do not leak a stale string
                // or comment context from the preceding hunk across the gap.
                old = LineParser::new(syntax);
                new = LineParser::new(syntax);
                in_hunk = true;
                result.push(None);
                continue;
            }
            if line.starts_with("diff ") {
                in_hunk = false;
            }
            let runs = if in_hunk {
                match line.as_bytes().first() {
                    Some(b'-') => old.runs(&line[1..], self),
                    Some(b'+') => new.runs(&line[1..], self),
                    Some(b' ') => {
                        // Context belongs to both versions; only one set of
                        // foregrounds is needed for the visible context row.
                        old.runs(&line[1..], self);
                        new.runs(&line[1..], self)
                    }
                    _ => None,
                }
            } else {
                None
            };
            result.push(runs);
        }
        Some(result)
    }

    fn tone(&self, stack: &ScopeStack) -> ContentTone {
        stack
            .scopes
            .iter()
            .rev()
            .find_map(|scope| {
                self.rules
                    .iter()
                    .find_map(|(prefix, tone)| prefix.is_prefix_of(*scope).then_some(*tone))
            })
            .unwrap_or_default()
    }

    fn syntax_for(&self, document: &DiffDocument) -> Option<&SyntaxReference> {
        let path = document.file.path();
        let extension = path.extension().and_then(|value| value.to_str());
        let file_name = path.file_name().and_then(|value| value.to_str());
        extension
            .and_then(|extension| self.syntaxes.find_syntax_by_extension(extension))
            .or_else(|| file_name.and_then(|name| self.syntaxes.find_syntax_by_extension(name)))
    }
}

struct LineParser {
    parse: ParseState,
    scopes: ScopeStack,
}

impl LineParser {
    fn new(syntax: &SyntaxReference) -> Self {
        Self {
            parse: ParseState::new(syntax),
            scopes: ScopeStack::new(),
        }
    }

    fn runs(&mut self, text: &str, highlighter: &Highlighter) -> Option<Vec<ContentRun>> {
        let source = format!("{text}\n");
        let operations = self.parse.parse_line(&source, &highlighter.syntaxes).ok()?;
        let mut runs: Vec<ContentRun> = Vec::new();
        for (text, operation) in ScopeRegionIterator::new(&operations, &source) {
            self.scopes.apply(operation).ok()?;
            let text = text.trim_end_matches('\n');
            if text.is_empty() {
                continue;
            }
            let tone = highlighter.tone(&self.scopes);
            if let Some(previous) = runs.last_mut().filter(|run| run.tone == tone) {
                previous.text.push_str(text);
            } else {
                runs.push(ContentRun::new(text).tone(tone));
            }
        }
        Some(runs)
    }
}

// Adapter for the older standalone renderer's regression fixtures. The
// shipping renderer consumes the semantic ContentRun values directly.
#[cfg(test)]
use ratatui::style::Color;
#[cfg(test)]
use unpeel_app_kit::{ColorScheme, KitTheme};
#[cfg(test)]
pub type DocumentColors = Vec<Option<Vec<(Color, String)>>>;
#[cfg(test)]
impl Highlighter {
    pub fn document_colors(
        &self,
        document: &DiffDocument,
        scheme: ColorScheme,
    ) -> Option<DocumentColors> {
        let theme = match scheme {
            ColorScheme::Dark => KitTheme::dark(),
            ColorScheme::Light => KitTheme::light(),
        };
        self.document_runs(document).map(|lines| {
            lines
                .into_iter()
                .map(|runs| {
                    runs.map(|runs| {
                        runs.into_iter()
                            .map(|run| {
                                let color = match run.tone {
                                    ContentTone::Default => theme.text,
                                    ContentTone::Muted => theme.muted,
                                    ContentTone::Accent => theme.accent,
                                    ContentTone::Info => Color::LightBlue,
                                    ContentTone::Success => Color::LightGreen,
                                    ContentTone::Warning => Color::LightYellow,
                                    ContentTone::Danger => theme.danger,
                                };
                                (color, run.text)
                            })
                            .collect()
                    })
                })
                .collect()
        })
    }
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
    fn swift_typescript_and_rust_preserve_text_and_distinguish_tokens() {
        let highlighter = Highlighter::new();
        for (path, code) in [
            ("Model.swift", "let title: String = \"Hello\" // comment"),
            ("model.ts", "const title: string = \"Hello\"; // comment"),
            ("model.rs", "let title: &str = \"Hello\"; // comment"),
        ] {
            let mut document = rust_document();
            document.file = ChangedFile::fixture(path, ' ', 'M');
            document.lines = vec!["@@ -1 +1 @@".into(), format!("+{code}")];
            let syntax = highlighter.document_runs(&document).expect(path);
            let runs = syntax[1].as_ref().unwrap();
            assert_eq!(
                runs.iter().map(|run| run.text.as_str()).collect::<String>(),
                code
            );
            for tone in [
                ContentTone::Accent,
                ContentTone::Success,
                ContentTone::Muted,
            ] {
                assert!(
                    runs.iter().any(|run| run.tone == tone),
                    "{path}: missing {tone:?} in {runs:?}"
                );
            }
        }
    }

    #[test]
    fn old_and_new_sides_keep_independent_multiline_context_and_reset_at_hunks() {
        let highlighter = Highlighter::new();
        let mut document = rust_document();
        document.lines = [
            "@@ -1,2 +1,2 @@",
            "-/* old comment",
            "+let title = 2;",
            "-end */",
            "+// new comment",
            "@@ -50 +50 @@",
            "+let next = 3;",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let syntax = highlighter.document_runs(&document).unwrap();
        assert!(
            syntax[1]
                .as_ref()
                .unwrap()
                .iter()
                .all(|run| run.tone == ContentTone::Muted)
        );
        assert!(
            syntax[2]
                .as_ref()
                .unwrap()
                .iter()
                .any(|run| run.tone == ContentTone::Accent)
        );
        assert!(
            syntax[3]
                .as_ref()
                .unwrap()
                .iter()
                .all(|run| run.tone == ContentTone::Muted)
        );
        assert!(
            syntax[6]
                .as_ref()
                .unwrap()
                .iter()
                .any(|run| run.tone == ContentTone::Accent)
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
