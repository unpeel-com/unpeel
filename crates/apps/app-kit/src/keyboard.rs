//! Scoped terminal keyboard enhancements shared by Unpeel Apps.

use std::io::{self, Write};

// Kitty keyboard protocol: push flags=1 (disambiguate escape codes), then pop
// one stack level when the App leaves its terminal surface.
const PUSH_UNAMBIGUOUS_ESCAPE: &[u8] = b"\x1b[>1u";
const POP_KEYBOARD_FLAGS: &[u8] = b"\x1b[<1u";

/// Keeps Escape distinguishable from the prefix of another terminal sequence.
///
/// Terminals that support the progressive keyboard protocol encode a physical
/// Escape as a complete key event while this guard is alive. Unsupported
/// terminals ignore the CSI mode request and retain their ordinary behavior.
#[must_use = "dropping the guard restores the previous keyboard protocol"]
#[derive(Debug)]
pub struct KeyboardEnhancementGuard {
    active: bool,
}

impl KeyboardEnhancementGuard {
    /// Pushes the unambiguous-Escape keyboard flag for the current terminal.
    pub fn enter() -> io::Result<Self> {
        write_sequence(PUSH_UNAMBIGUOUS_ESCAPE)?;
        Ok(Self { active: true })
    }

    /// Restores the previous keyboard mode immediately instead of on drop.
    pub fn restore(mut self) -> io::Result<()> {
        self.active = false;
        write_sequence(POP_KEYBOARD_FLAGS)
    }
}

impl Drop for KeyboardEnhancementGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = write_sequence(POP_KEYBOARD_FLAGS);
        }
    }
}

fn write_sequence(sequence: &[u8]) -> io::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    output.write_all(sequence)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_sequences_push_disambiguation_and_pop_one_level() {
        assert_eq!(PUSH_UNAMBIGUOUS_ESCAPE, b"\x1b[>1u");
        assert_eq!(POP_KEYBOARD_FLAGS, b"\x1b[<1u");
    }
}
