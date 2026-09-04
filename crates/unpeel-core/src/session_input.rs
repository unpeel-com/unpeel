//! Safe text delivery into hosted agent terminals.
//!
//! This is the single implementation of the bracketed-paste + settle +
//! double-Enter recipe used by Sessions MCP, secure remote input, and typed
//! Host actions such as screenshot requests. UI clients send semantic actions;
//! they never construct terminal escape sequences themselves.

use crate::session_host::{self, SessionHostCommand};
use std::thread;
use std::time::Duration;

/// Delay between pasting text and pressing Enter so agent TUIs finish
/// processing the bracketed paste.
pub const TEXT_SETTLE_DELAY_MS: u64 = 250;
/// Longer settle when pasted text contains file paths, which some TUIs attach
/// asynchronously before the prompt can be submitted.
pub const TEXT_SETTLE_DELAY_PATH_MS: u64 = 900;
/// Some full-screen agent TUIs swallow the first Enter after a paste.
pub const ENTER_FOLLOWUP_DELAY_MS: u64 = 80;

/// Provider-neutral request used by every Host implementation.
pub const SCREENSHOT_REQUEST_PROMPT: &str = "Please capture the current visual result with the Unpeel Browser tool's screenshot action, setting gallery to true so it is saved as a screenshot artifact in this session's gallery. If this task has no visual result, say so instead.";

pub fn sanitize_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

pub fn encode_bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{text}\x1b[201~")
}

pub fn looks_like_it_contains_a_path(text: &str) -> bool {
    text.split_whitespace()
        .any(|word| word.len() > 1 && word.contains('/'))
}

fn write_to_session(session_id: &str, data: &str, timeout: Option<Duration>) -> Result<(), String> {
    let command = SessionHostCommand::Write {
        data: data.to_string(),
        write_id: None,
    };
    match timeout {
        Some(timeout) => session_host::send_command_with_timeout(session_id, &command, timeout),
        None => session_host::send_command(session_id, &command),
    }
}

/// Deliver already-sanitized text through the terminal prompt recipe.
/// Callers are responsible for their own authorization/write gate.
pub fn deliver_sanitized_text(
    session_id: &str,
    sanitized: &str,
    submit: bool,
) -> Result<(), String> {
    deliver_sanitized_text_with_timeout(session_id, sanitized, submit, None)
}

fn deliver_sanitized_text_with_timeout(
    session_id: &str,
    sanitized: &str,
    submit: bool,
    timeout: Option<Duration>,
) -> Result<(), String> {
    if !sanitized.is_empty() {
        write_to_session(session_id, &encode_bracketed_paste(sanitized), timeout)?;
    }
    if submit {
        let settle = if looks_like_it_contains_a_path(sanitized) {
            TEXT_SETTLE_DELAY_PATH_MS
        } else {
            TEXT_SETTLE_DELAY_MS
        };
        thread::sleep(Duration::from_millis(settle));
        write_to_session(session_id, "\r", timeout)?;
        thread::sleep(Duration::from_millis(ENTER_FOLLOWUP_DELAY_MS));
        write_to_session(session_id, "\r", timeout)?;
    }
    Ok(())
}

/// Sanitize and submit one user-visible prompt to a hosted session.
pub fn deliver_prompt(session_id: &str, text: &str) -> Result<(), String> {
    let sanitized = sanitize_paste_text(text);
    if sanitized.trim().is_empty() {
        return Err("prompt is empty after removing control characters".into());
    }
    deliver_sanitized_text(session_id, &sanitized, true)
}

pub fn request_screenshot(session_id: &str) -> Result<(), String> {
    request_screenshot_with_optional_timeout(session_id, None)
}

/// Remote request variant: each control-socket round trip is bounded so a
/// wedged session Host cannot pin an HTTP/Relay/FFI worker indefinitely.
pub fn request_screenshot_with_timeout(session_id: &str, timeout: Duration) -> Result<(), String> {
    request_screenshot_with_optional_timeout(session_id, Some(timeout))
}

fn request_screenshot_with_optional_timeout(
    session_id: &str,
    timeout: Option<Duration>,
) -> Result<(), String> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains("..")
        || session_id.contains('\\')
    {
        return Err("invalid session id".into());
    }
    let sanitized = sanitize_paste_text(SCREENSHOT_REQUEST_PROMPT);
    deliver_sanitized_text_with_timeout(session_id, &sanitized, true, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_provider_neutral_and_names_the_artifact_contract() {
        assert!(SCREENSHOT_REQUEST_PROMPT.contains("Unpeel Browser tool"));
        assert!(SCREENSHOT_REQUEST_PROMPT.contains("screenshot artifact"));
        assert!(!SCREENSHOT_REQUEST_PROMPT.contains("Claude"));
        assert!(!SCREENSHOT_REQUEST_PROMPT.contains("Codex"));
    }

    #[test]
    fn paste_helpers_preserve_text_and_strip_terminal_controls() {
        let sanitized = sanitize_paste_text("hi\r\nthere\x1b[31m end\x07");
        assert_eq!(sanitized, "hi\nthere[31m end");
        assert_eq!(encode_bracketed_paste("hello"), "\x1b[200~hello\x1b[201~");
        assert!(looks_like_it_contains_a_path("look at src/lib/foo.ts"));
        assert!(!looks_like_it_contains_a_path("capture the result"));
    }
}
