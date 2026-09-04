//! Cross-frontend change notification.
//!
//! Shared state lives on disk (`app-state.json`, `session-order.json`,
//! `project-order.json`, `pane-layouts.json`, the per-session markers). Every frontend already
//! discovers the others through `~/.unpeel/app-ports` — the registry the
//! provider hook scripts broadcast to, precisely because several Unpeel
//! instances can run at once. This reuses that bus for one more message:
//! **"shared state changed, re-read it."**
//!
//! Why a ping rather than watching files:
//!
//! * It is **immediate**. The desktop coalesces FSEvents at 0.5s and falls
//!   back to a 5s safety rescan; the terminal polls at 1s. A drag in one
//!   frontend appearing a second or five later in the other reads as broken.
//! * It **says what changed**, so a listener can do the cheap rebuild
//!   instead of a full disk rescan.
//! * It needs **no daemon** — the architecture's central constraint. Both
//!   frontends already run an HTTP listener on a registered port.
//!
//! It is strictly an optimisation: every listener still watches files and
//! still polls, so a missed ping costs latency, never correctness. Senders
//! never block on it and never fail because of it.

use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

/// What changed. Listeners may use this to scope their refresh; treating
/// every kind as "rescan everything" is always correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Sidebar ordering: `session-order.json` or `project-order.json`.
    Order,
    /// A session's organization markers: archived / title / read.
    SessionMarkers,
    /// `app-state.json`: projects, presets, pins, policies.
    AppState,
    /// A session started, stopped, or went away.
    Lifecycle,
    /// Controller pane layouts: `pane-layouts.json`.
    PaneLayouts,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Change::Order => "order",
            Change::SessionMarkers => "session-markers",
            Change::AppState => "app-state",
            Change::Lifecycle => "lifecycle",
            Change::PaneLayouts => "pane-layouts",
        }
    }
}

/// The route every frontend serves to receive these.
pub const ROUTE: &str = "/state-changed";

/// Path-taking, so tests never touch the process-global `UNPEEL_HOME`:
/// cargo runs them in parallel threads and swapping it races every other
/// test that resolves a path at that moment.
fn registered_ports_at(path: &std::path::Path) -> Vec<u16> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut ports: Vec<u16> = Vec::new();
    for line in raw.lines() {
        if let Ok(port) = line.trim().parse::<u16>() {
            if port != 0 && !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
    ports
}

/// Tell every other instance that shared state moved.
///
/// `own_port` is this process's own listener, skipped so a frontend does not
/// rescan in response to itself. Fire-and-forget: connect timeouts are short,
/// failures are ignored, and the whole thing runs on a detached thread so a
/// slow or dead peer can never stall a UI action.
pub fn announce(change: Change, own_port: Option<u16>) {
    announce_from(
        &crate::app_paths::unpeel_home().join("app-ports"),
        change,
        own_port,
    )
}

/// In-flight announcement threads. Long-lived frontends never wait on
/// these (a slow peer must not stall a drag), but a one-shot CLI verb exits
/// the moment it returns — killing detached threads before they send — so
/// the CLI calls `flush()` on its way out.
static PENDING: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

/// Wait for every queued announcement to finish sending. One-shot processes
/// (the CLI) call this before exiting; long-lived UIs never need to.
pub fn flush() {
    let handles: Vec<_> = match PENDING.lock() {
        Ok(mut pending) => pending.drain(..).collect(),
        Err(_) => return,
    };
    for handle in handles {
        let _ = handle.join();
    }
}

/// `announce` against an explicit registry — see `registered_ports_at`.
///
/// Sends on a background thread so a slow or wedged peer can never stall
/// the caller — announcements happen on UI action paths (a drag, a rename).
/// The thread is tracked so `flush()` can wait for it in short-lived
/// processes.
pub fn announce_from(registry: &std::path::Path, change: Change, own_port: Option<u16>) {
    let ports: Vec<u16> = registered_ports_at(registry)
        .into_iter()
        .filter(|port| Some(*port) != own_port)
        .collect();
    if ports.is_empty() {
        return;
    }
    let handle = std::thread::spawn(move || {
        let body = format!("{{\"change\":\"{}\"}}", change.as_str());
        for port in ports {
            let address = format!("127.0.0.1:{port}");
            let Ok(target) = address.parse() else {
                continue;
            };
            let Ok(mut stream) = TcpStream::connect_timeout(&target, Duration::from_millis(250))
            else {
                // A port in the registry whose owner has gone is normal.
                continue;
            };
            let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
            let request = format!(
                "POST {ROUTE} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(request.as_bytes());
            let _ = stream.flush();
        }
    });
    if let Ok(mut pending) = PENDING.lock() {
        // Reap finished threads so the list stays small in a long session.
        pending.retain(|h| !h.is_finished());
        pending.push(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn announce_posts_to_every_registered_port_but_our_own() {
        let dir = std::env::temp_dir().join(format!("unpeel-bus-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Two listeners stand in for two other frontends.
        let (tx, rx) = mpsc::channel();
        let mut ports = Vec::new();
        for _ in 0..2 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            ports.push(listener.local_addr().unwrap().port());
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buffer = [0u8; 512];
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    let _ = tx.send(String::from_utf8_lossy(&buffer[..read]).into_owned());
                }
            });
        }
        // ...and a third port we claim as our own, which must be skipped.
        let own = TcpListener::bind("127.0.0.1:0").unwrap();
        let own_port = own.local_addr().unwrap().port();
        let own_tx = tx.clone();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = own.accept() {
                let mut buffer = [0u8; 64];
                let _ = stream.read(&mut buffer);
                let _ = own_tx.send("OWN PORT WAS CALLED".into());
            }
        });

        std::fs::write(
            dir.join("app-ports"),
            format!("{}\n{}\n{}\n", ports[0], ports[1], own_port),
        )
        .unwrap();

        announce_from(&dir.join("app-ports"), Change::Order, Some(own_port));
        flush();
        for _ in 0..2 {
            let request = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("both peers should be told");
            assert!(request.starts_with("POST /state-changed"), "{request}");
            assert!(request.contains("\"change\":\"order\""), "{request}");
        }
        // Nothing else should arrive — least of all on our own port.
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn announce_is_silent_with_no_registry() {
        let dir = std::env::temp_dir().join(format!("unpeel-bus-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Must not panic or block when nobody is listening.
        announce_from(&dir.join("app-ports"), Change::AppState, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
