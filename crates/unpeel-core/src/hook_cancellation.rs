//! Host-observed cancellation intent. This never signals a process or disables
//! provider hooks: it fences activity from an interrupted turn until a later
//! submitted prompt establishes a new lifecycle. Runtime packages opt in.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::Path;

const MARKER: &str = "hook-cancellation.json";
pub const ESCAPE_SETTLE_MS: u64 = 150;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Cancellation {
    pub runtime_generation: u64,
    pub cancelled_at: u64,
    pub submitted_at: Option<u64>,
}

pub fn read_in(dir: &Path) -> Option<Cancellation> {
    let mut bytes = Vec::new();
    fs::File::open(dir.join(MARKER))
        .ok()?
        .take(4096)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The caller announces successful writes on the existing state bus. Never
/// create a Session directory: input finishing during teardown cannot revive it.
pub(crate) fn record_in(
    dir: &Path,
    generation: u64,
    cancelled_at: Option<u64>,
    submitted_at: Option<u64>,
) -> Result<bool, String> {
    if !dir.is_dir() || (cancelled_at.is_none() && !dir.join(MARKER).is_file()) {
        return Ok(false);
    }
    let path = dir.join(MARKER);
    let _lock = crate::app_state::lock_exclusive(&path)?;
    let previous = read_in(dir);
    if previous
        .as_ref()
        .is_some_and(|value| value.runtime_generation > generation)
    {
        return Ok(false);
    }
    let previous = previous.filter(|value| value.runtime_generation == generation);
    let mut marker = match (previous, cancelled_at) {
        (Some(previous), Some(at)) if at <= previous.cancelled_at => previous,
        (_, Some(at)) => Cancellation {
            runtime_generation: generation,
            cancelled_at: at,
            submitted_at: None,
        },
        (Some(previous), None) => previous,
        (None, None) => return Ok(false),
    };
    if let Some(at) = submitted_at.filter(|at| *at > marker.cancelled_at) {
        marker.submitted_at = Some(marker.submitted_at.unwrap_or(at).min(at));
    }
    let bytes = serde_json::to_string(&marker).map_err(|error| error.to_string())?;
    crate::hook_assets::write_file_atomic(&path, &bytes, "hook cancellation")?;
    Ok(true)
}

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
enum InputState {
    #[default]
    Plain,
    Escape(u64, bool),
    Csi(Vec<u8>, bool),
    Ss3,
    String,
    StringEscape,
    Paste(usize),
}

/// Scans only bytes successfully delivered by user-input write paths. Device
/// query replies and launch commands bypass it. State survives input frames,
/// partial PTY writes, and core handoff. No key is consumed or delayed.
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub(crate) struct InputTracker {
    state: InputState,
    generation: u64,
    cancelled_at: Option<u64>,
    submitted_at: Option<u64>,
}

impl InputTracker {
    fn note_cancel(&mut self, at: u64) {
        self.cancelled_at = Some(at);
        self.submitted_at = self.submitted_at.filter(|submitted| *submitted > at);
    }

    pub(crate) fn feed(&mut self, bytes: &[u8], generation: u64, now: u64, menu_active: bool) {
        if self.generation != generation {
            *self = Self {
                generation,
                ..Self::default()
            };
        }
        for &byte in bytes {
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                InputState::Plain => match byte {
                    0x1b => InputState::Escape(now, !menu_active),
                    b'\r' | b'\n' => {
                        self.submitted_at.get_or_insert(now);
                        InputState::Plain
                    }
                    _ => InputState::Plain,
                },
                InputState::Escape(at, allowed) => match byte {
                    b'[' => InputState::Csi(Vec::new(), allowed),
                    b'O' => InputState::Ss3,
                    b']' | b'P' | b'_' | b'^' | b'X' => InputState::String,
                    0x1b => {
                        if allowed {
                            self.note_cancel(at);
                        }
                        InputState::Escape(now, !menu_active)
                    }
                    // Alt/Meta-prefixed keys are not standalone Escape.
                    _ => InputState::Plain,
                },
                InputState::Csi(mut parameters, allowed) => {
                    if (0x40..=0x7e).contains(&byte) {
                        if byte == b'~' && parameters == b"200" {
                            InputState::Paste(0)
                        } else {
                            // Kitty's unmodified Escape/Enter press or repeat;
                            // releases and modified keys have no authority.
                            if byte == b'u' && allowed {
                                if matches!(
                                    parameters.as_slice(),
                                    b"27" | b"27;1" | b"27;1:1" | b"27;1:2"
                                ) {
                                    self.note_cancel(now);
                                } else if matches!(
                                    parameters.as_slice(),
                                    b"13" | b"13;1" | b"13;1:1" | b"13;1:2"
                                ) {
                                    self.submitted_at.get_or_insert(now);
                                }
                            }
                            InputState::Plain
                        }
                    } else {
                        // Bounded even for an unterminated/malformed sequence.
                        if parameters.len() < 64 {
                            parameters.push(byte);
                        }
                        InputState::Csi(parameters, allowed)
                    }
                }
                InputState::Ss3 => InputState::Plain,
                InputState::String => match byte {
                    7 => InputState::Plain,
                    0x1b => InputState::StringEscape,
                    _ => InputState::String,
                },
                InputState::StringEscape => {
                    if byte == b'\\' {
                        InputState::Plain
                    } else {
                        InputState::String
                    }
                }
                InputState::Paste(matched) => {
                    const END: &[u8] = b"\x1b[201~";
                    let matched = if byte == END[matched] {
                        matched + 1
                    } else {
                        usize::from(byte == END[0])
                    };
                    if matched == END.len() {
                        InputState::Plain
                    } else {
                        InputState::Paste(matched)
                    }
                }
            };
        }
    }

    pub(crate) fn take_pending(&mut self, now: u64) -> (u64, Option<u64>, Option<u64>) {
        if let InputState::Escape(at, allowed) = self.state {
            if now.saturating_sub(at) >= ESCAPE_SETTLE_MS {
                if allowed {
                    self.note_cancel(at);
                }
                self.state = InputState::Plain;
            }
        }
        (
            self.generation,
            self.cancelled_at.take(),
            self.submitted_at.take(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_escape_is_not_cancellation_even_after_menu_disappears() {
        let mut input = InputTracker::default();
        input.feed(b"\x1b", 1, 1000, true);
        assert_eq!(input.take_pending(1300), (1, None, None));
        input.feed(b"\x1b[", 1, 1500, true);
        input.feed(b"27u", 1, 1600, false);
        assert_eq!(input.take_pending(1800), (1, None, None));
    }

    #[test]
    fn marker_preserves_newer_generations_and_never_revives_removed_session() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!record_in(dir.path(), 1, None, Some(1000)).unwrap());
        assert!(record_in(dir.path(), 1, Some(2000), None).unwrap());
        assert!(record_in(dir.path(), 1, None, Some(3000)).unwrap());
        let marker = read_in(dir.path()).unwrap();
        assert_eq!(marker.cancelled_at, 2000);
        assert_eq!(marker.submitted_at, Some(3000));
        record_in(dir.path(), 1, None, Some(3080)).unwrap();
        assert_eq!(
            read_in(dir.path()).unwrap().submitted_at,
            Some(3000),
            "the paste recipe's second Enter must not invalidate a hook from the first"
        );
        record_in(dir.path(), 2, Some(4000), None).unwrap();
        assert!(!record_in(dir.path(), 1, Some(5000), None).unwrap());
        let marker = read_in(dir.path()).unwrap();
        assert_eq!(marker.runtime_generation, 2);
        assert_eq!(marker.submitted_at, None);
        let removed = dir.path().join("removed");
        assert!(!record_in(&removed, 2, Some(6000), None).unwrap());
        assert!(!removed.exists());
    }

    #[test]
    fn double_enter_keeps_first_submission_and_next_cancel_resets_it() {
        let mut input = InputTracker::default();
        input.feed(b"\x1b[27u", 1, 1000, false);
        input.feed(b"\r", 1, 2000, false);
        input.feed(b"\r", 1, 2080, false);
        assert_eq!(input.take_pending(2100), (1, Some(1000), Some(2000)));
        input.feed(b"\r", 1, 3000, false);
        input.feed(b"\x1b[27u", 1, 3100, false);
        assert_eq!(input.take_pending(3400), (1, Some(3100), None));
    }

    #[test]
    fn escape_waits_for_fragmented_sequences_and_ignores_paste() {
        let mut input = InputTracker::default();
        input.feed(b"\x1b", 1, 1000, false);
        assert_eq!(input.take_pending(1100), (1, None, None));
        input.feed(b"[A", 1, 1100, false);
        assert_eq!(input.take_pending(1400), (1, None, None));
        for byte in b"\x1b[200~hello\r\x1b\x1b[27u\x1b[201~\x1bb\x1bOP\x1b]title\x1b\\" {
            input.feed(&[*byte], 1, 1500, false);
        }
        assert_eq!(input.take_pending(1800), (1, None, None));
        input.feed(b"\x1b", 1, 2000, false);
        assert_eq!(input.take_pending(2150), (1, Some(2000), None));
        input.feed(b"new prompt\r", 1, 2200, false);
        assert_eq!(input.take_pending(2300), (1, None, Some(2200)));
    }

    #[test]
    fn kitty_press_cancels_but_release_and_modifiers_do_not() {
        let mut input = InputTracker::default();
        input.feed(b"\x1b[27;1:3u\x1b[27;2u", 3, 1000, false);
        assert_eq!(input.take_pending(1200), (3, None, None));
        input.feed(b"\x1b[27u", 3, 1300, false);
        assert_eq!(input.take_pending(1400), (3, Some(1300), None));
        input.feed(b"\x1b[13;1u", 3, 1500, false);
        assert_eq!(input.take_pending(1600), (3, None, Some(1500)));
    }

    #[test]
    fn generation_edge_drops_pending_escape_and_handoff_preserves_paste() {
        let mut input = InputTracker::default();
        input.feed(b"\x1b", 1, 1000, false);
        input.feed(b"hi", 2, 1050, false);
        assert_eq!(input.take_pending(1500), (2, None, None));
        input.feed(b"\x1b[200~", 2, 1600, false);
        let mut restored: InputTracker =
            serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
        restored.feed(b"\x1b\r\x1b[201~", 2, 1700, false);
        assert_eq!(restored.take_pending(2000), (2, None, None));
    }
}
