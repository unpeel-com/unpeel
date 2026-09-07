//! System clipboard via the platform pasteboard, plus OSC 52 so the copy
//! also lands on whichever device is VIEWING the terminal — a remote
//! controller (phone) renders this PTY on its own screen, and pbcopy alone
//! would only reach the host Mac's clipboard.

use std::io::Write;
use std::process::{Command, Stdio};

pub fn read() -> Option<String> {
    let output = Command::new("pbpaste").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    if text.is_empty() { None } else { Some(text) }
}

/// OSC 52 clipboard write through the terminal itself. Every frontend
/// rendering this PTY (local ghostty, the phone's in-memory surface)
/// receives it and can populate its own pasteboard.
fn write_osc52(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = stdout.flush();
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub fn write(text: &str) -> bool {
    write_osc52(text);
    let Ok(mut child) = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        drop(stdin);
        let _ = child.kill();
        return false;
    }
    drop(stdin);
    child.wait().map(|status| status.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"A"), "QQ==");
        assert_eq!(base64(b"AB"), "QUI=");
        assert_eq!(base64(b"ABC"), "QUJD");
        assert_eq!(
            base64("hello world — æøå".as_bytes()),
            "aGVsbG8gd29ybGQg4oCUIMOmw7jDpQ=="
        );
    }
}
