//! Markdown selection helpers that remain App-specific.
//!
//! Wrapped terminal hit-testing and scroll state live in
//! `unpeel_app_kit::MarkdownTextArea`.

pub fn word_bounds(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let index = col.min(chars.len() - 1);
    let class = char_class(chars[index]);
    let start = (0..=index)
        .rev()
        .take_while(|&candidate| char_class(chars[candidate]) == class)
        .last()
        .unwrap_or(index);
    let end = (index..chars.len())
        .take_while(|&candidate| char_class(chars[candidate]) == class)
        .last()
        .map(|candidate| candidate + 1)
        .unwrap_or(index + 1);
    (start, end)
}

pub const fn pos_le(left: (usize, usize), right: (usize, usize)) -> bool {
    left.0 < right.0 || (left.0 == right.0 && left.1 <= right.1)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn char_class(character: char) -> CharClass {
    if character.is_whitespace() {
        CharClass::Space
    } else if character.is_alphanumeric() || character == '_' {
        CharClass::Word
    } else {
        CharClass::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_bounds_select_identifier() {
        assert_eq!(word_bounds("foo bar", 1), (0, 3));
        assert_eq!(word_bounds("foo bar", 4), (4, 7));
        assert_eq!(word_bounds("a,b", 1), (1, 2));
    }
}
