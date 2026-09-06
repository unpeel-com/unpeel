//! Graphical-session diagnostics for the Host service. Never starts a desktop or engine.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopSession {
    /// `X11 :0` / `Wayland wayland-0` (`macOS` on macOS).
    pub display: String,
    pub wayland: bool,
    /// The session bus address to hand the daemon (`unix:path=…`); `None`
    /// only on macOS, where the app owns the daemon and the question does
    /// not arise.
    pub session_bus: Option<String>,
}

/// Resolve the desktop session from this process's environment. `Err`
/// names the missing piece (no display, or no session bus) so the Host's
/// unavailable reason and the CLI's exit-4 line say what to fix.
pub fn desktop_session() -> Result<DesktopSession, String> {
    let env = |name: &str| std::env::var(name).ok();
    desktop_session_from(
        env("WAYLAND_DISPLAY").as_deref(),
        env("XDG_RUNTIME_DIR").as_deref(),
        env("DISPLAY").as_deref(),
        env("DBUS_SESSION_BUS_ADDRESS").as_deref(),
        Some(unsafe { libc::getuid() }),
    )
}

/// Pure form of `desktop_session` for the Host, the CLI, and tests.
pub fn desktop_session_from(
    wayland_display: Option<&str>,
    runtime_dir: Option<&str>,
    display: Option<&str>,
    bus_env: Option<&str>,
    uid: Option<u32>,
) -> Result<DesktopSession, String> {
    if std::env::consts::OS == "macos" {
        return Ok(DesktopSession {
            display: "macOS".into(),
            wayland: false,
            session_bus: None,
        });
    }
    let (display, wayland) = match graphical_session_from(wayland_display, runtime_dir, display) {
        Some(label) => {
            let wayland = label.starts_with("Wayland ");
            (label, wayland)
        }
        None => {
            return Err(
                "no graphical session is visible to this process: start it from the desktop \
session with DISPLAY (X11) or WAYLAND_DISPLAY and XDG_RUNTIME_DIR (Wayland) set"
                    .into(),
            )
        }
    };
    let session_bus = session_bus_address(bus_env, runtime_dir, uid).ok_or_else(|| {
        format!(
            "{display} is visible but no session D-Bus is: the accessibility (AT-SPI) bus the \
desktop tools use lives on it. Set DBUS_SESSION_BUS_ADDRESS, or start from a \
session whose user manager owns $XDG_RUNTIME_DIR/bus (`systemctl --user`)"
        )
    })?;
    Ok(DesktopSession {
        display,
        wayland,
        session_bus: Some(session_bus),
    })
}

/// Session-bus discovery:
/// `DBUS_SESSION_BUS_ADDRESS` → `$XDG_RUNTIME_DIR/bus` → `/run/user/<uid>/bus`
/// (the last two must exist as sockets).
pub fn session_bus_address(
    bus_env: Option<&str>,
    runtime_dir: Option<&str>,
    uid: Option<u32>,
) -> Option<String> {
    if let Some(value) = bus_env.map(str::trim).filter(|v| !v.is_empty()) {
        return Some(value.to_string());
    }
    let mut candidates = Vec::new();
    if let Some(dir) = runtime_dir.map(str::trim).filter(|d| !d.is_empty()) {
        candidates.push(PathBuf::from(dir).join("bus"));
    }
    if let Some(uid) = uid {
        candidates.push(PathBuf::from(format!("/run/user/{uid}/bus")));
    }
    candidates
        .into_iter()
        .find(|path| is_socket(path))
        .map(|path| format!("unix:path={}", path.display()))
}

fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path)
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false)
}

/// The desktop session's display label alone (`X11 :0`), or `None`; kept
/// for callers that only need to know whether a display exists.
pub fn graphical_session() -> Option<String> {
    graphical_session_from(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    )
}

pub fn graphical_session_from(
    wayland_display: Option<&str>,
    runtime_dir: Option<&str>,
    display: Option<&str>,
) -> Option<String> {
    if std::env::consts::OS == "macos" {
        return Some("macOS".into());
    }
    if let Some(value) = wayland_display.map(str::trim).filter(|v| !v.is_empty()) {
        let has_runtime_dir = Path::new(value).is_absolute()
            || runtime_dir.map(str::trim).is_some_and(|d| !d.is_empty());
        if has_runtime_dir {
            return Some(format!("Wayland {value}"));
        }
    }
    display
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!("X11 {v}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graphical_session_needs_a_display_or_a_resolvable_wayland_socket() {
        if std::env::consts::OS == "macos" {
            assert!(graphical_session_from(None, None, None).is_some());
            return;
        }
        assert_eq!(graphical_session_from(None, None, None), None);
        assert_eq!(graphical_session_from(None, None, Some(" ")), None);
        assert_eq!(
            graphical_session_from(None, None, Some(":0")).as_deref(),
            Some("X11 :0")
        );
        assert_eq!(
            graphical_session_from(Some("wayland-0"), None, Some(":0")).as_deref(),
            Some("X11 :0"),
            "a relative Wayland socket without a runtime dir is unusable"
        );
        assert_eq!(
            graphical_session_from(Some("wayland-0"), Some("/run/user/1000"), None).as_deref(),
            Some("Wayland wayland-0")
        );
        assert_eq!(
            graphical_session_from(Some("/tmp/wl"), None, None).as_deref(),
            Some("Wayland /tmp/wl")
        );
    }

    #[test]
    fn session_bus_discovery_prefers_env_then_runtime_dir_then_run_user() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        assert_eq!(
            session_bus_address(Some(" unix:path=/x "), None, None).as_deref(),
            Some("unix:path=/x")
        );
        // A runtime dir whose bus is a plain file (not a socket) is skipped.
        let runtime = home.join("rt");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("bus"), b"not a socket").unwrap();
        assert_eq!(
            session_bus_address(Some(""), runtime.to_str(), Some(4_000_000_000)),
            None
        );
        let _ = std::fs::remove_file(runtime.join("bus"));
        let listener = std::os::unix::net::UnixListener::bind(runtime.join("bus")).unwrap();
        assert_eq!(
            session_bus_address(None, runtime.to_str(), None).as_deref(),
            Some(format!("unix:path={}", runtime.join("bus").display()).as_str())
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn desktop_session_names_the_missing_piece() {
        if std::env::consts::OS == "macos" {
            let session = desktop_session_from(None, None, None, None, None).unwrap();
            assert!(session.session_bus.is_none() && !session.wayland);
            return;
        }
        let err = desktop_session_from(None, None, None, None, None).unwrap_err();
        assert!(err.contains("no graphical session"), "{err}");
        let err =
            desktop_session_from(None, None, Some(":0"), None, Some(4_000_000_000)).unwrap_err();
        assert!(err.contains("no session D-Bus"), "{err}");
        let session =
            desktop_session_from(None, None, Some(":0"), Some("unix:path=/b"), None).unwrap();
        assert_eq!(session.display, "X11 :0");
        assert!(!session.wayland);
        assert_eq!(session.session_bus.as_deref(), Some("unix:path=/b"));
        let session = desktop_session_from(
            Some("wayland-0"),
            Some("/run/user/1"),
            None,
            Some("unix:path=/b"),
            None,
        )
        .unwrap();
        assert!(session.wayland);
        assert_eq!(session.session_bus.as_deref(), Some("unix:path=/b"));
    }
}
