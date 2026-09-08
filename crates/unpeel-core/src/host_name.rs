//! The machine name a Host shows Controllers when nothing more specific
//! (a workspace name) applies.
//!
//! `gethostname` is a DNS label, and on a Mac it carries whatever domain the
//! router handed out (`mac.lan`), so it is never shown as-is. macOS has a
//! user-facing computer name (System Settings ▸ General ▸ About), the same
//! value the native app reads through `Host.current().localizedName`; other
//! platforms fall back to the first hostname label.

use std::sync::OnceLock;

/// The user-facing machine name: the macOS computer name when available,
/// otherwise the hostname's first label. Resolved once per process.
pub fn machine_display_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| computer_name().unwrap_or_else(short_hostname))
        .clone()
}

/// The hostname's first label with any `.local` suffix removed first, so
/// `studio.local` and `studio.lan` both read `studio`. Empty or failing
/// lookups read `Mac` on macOS and `Host` elsewhere.
pub fn short_hostname() -> String {
    let mut buffer = [0u8; 256];
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if rc == 0 {
        let raw = buffer.split(|byte| *byte == 0).next().unwrap_or_default();
        let name = String::from_utf8_lossy(raw);
        let label = name
            .trim_end_matches(".local")
            .split('.')
            .next()
            .unwrap_or_default()
            .trim();
        if !label.is_empty() {
            return label.to_owned();
        }
    }
    if cfg!(target_os = "macos") {
        "Mac".into()
    } else {
        "Host".into()
    }
}

#[cfg(target_os = "macos")]
fn computer_name() -> Option<String> {
    let output = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)).then_some(name)
}

#[cfg(not(target_os = "macos"))]
fn computer_name() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hostname_is_one_label() {
        let name = short_hostname();
        assert!(!name.is_empty());
        assert!(!name.contains('.'), "{name}");
    }

    #[test]
    fn machine_display_name_is_stable_and_never_a_dns_name() {
        let first = machine_display_name();
        assert_eq!(first, machine_display_name());
        assert!(!first.is_empty());
        assert!(
            !first.ends_with(".lan") && !first.ends_with(".local"),
            "{first}"
        );
    }
}
