//! Crossterm backend modes used by this app.

use std::io;

use ratatui::DefaultTerminal;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;

// Keep printable input on the terminal's ordinary text path so Option/Alt
// combinations follow the active keyboard layout. Crossterm cannot yet read
// the Kitty protocol's associated-text field, so REPORT_ALL_KEYS_AS_ESCAPE_CODES
// would turn e.g. Norwegian macOS Option+8/9 back into physical `8`/`9` keys.
const KEYBOARD_FLAGS: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        .union(KeyboardEnhancementFlags::REPORT_EVENT_TYPES);

/// Enables mouse capture on the Ratatui Crossterm backend and restores it on drop.
pub struct BackendCapture;

impl BackendCapture {
    pub fn enable(terminal: &mut DefaultTerminal) -> io::Result<Self> {
        execute!(
            terminal.backend_mut(),
            EnableMouseCapture,
            EnableBracketedPaste,
            SetCursorStyle::BlinkingBar
        )?;
        let _ = execute!(
            terminal.backend_mut(),
            PushKeyboardEnhancementFlags(KEYBOARD_FLAGS)
        );
        Ok(Self)
    }
}

impl Drop for BackendCapture {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableBracketedPaste,
            SetCursorStyle::DefaultUserShape
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_flags_preserve_layout_produced_text() {
        assert!(KEYBOARD_FLAGS.contains(KeyboardEnhancementFlags::REPORT_EVENT_TYPES));
        assert!(!KEYBOARD_FLAGS.contains(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS));
        assert!(
            !KEYBOARD_FLAGS.contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES)
        );
    }
}
