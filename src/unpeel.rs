//! The entire Unpeel integration, self-contained. An Unpeel App is a plain
//! terminal program; when hosted, Unpeel exports `UNPEEL_SESSION_ID` and
//! friends, and the App talks back over three tiny surfaces:
//!
//! - **Activity**: POST the canonical hook events (`UserPromptSubmit` busy,
//!   `Stop` idle, `PermissionRequest` attention) to `/hook/<session_id>` on
//!   every registered port, mirrored into the durable
//!   `last-hook-event.json` seed so the latch survives frontend restarts.
//! - **Status text**: the `status.json` marker in the session dir — atomic
//!   whole-file overwrite, debounced, announced on the state bus.
//! - **Alerts**: POST bounded informational copy to `/notify/<session_id>`.
//!   Unpeel records it in Recent and delivers desktop/phone notifications
//!   without changing Busy/Idle/Attention.
//!
//! Outside Unpeel every call is a silent no-op. No SDK required; this file
//! is the whole contract and is freely copyable into any App.

use std::io::Write as _;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IO_TIMEOUT: Duration = Duration::from_millis(250);
const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct Host {
    pub session_id: String,
    pub session_dir: PathBuf,
    pub app_port: Option<u16>,
    pub port_registry: PathBuf,
}

impl Host {
    pub fn detect() -> Option<Host> {
        let session_id = std::env::var("UNPEEL_SESSION_ID").ok()?;
        if session_id.trim().is_empty() {
            return None;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let unpeel_home = std::env::var_os("UNPEEL_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join(".unpeel")))?;
        let session_dir = std::env::var_os("UNPEEL_SESSION_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| unpeel_home.join("app-sessions").join(&session_id));
        let port_registry = std::env::var_os("UNPEEL_APP_PORT_REGISTRY_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| unpeel_home.join("app-ports"));
        Some(Host {
            session_id,
            session_dir,
            app_port: std::env::var("UNPEEL_APP_PORT")
                .ok()
                .and_then(|port| port.parse().ok()),
            port_registry,
        })
    }

    /// Every port to notify: the launching instance first, then the shared
    /// registry (multiple Unpeel instances can run at once), deduplicated.
    fn ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self.app_port.into_iter().collect();
        if let Ok(raw) = std::fs::read_to_string(&self.port_registry) {
            for line in raw.lines() {
                if let Ok(port) = line.trim().parse::<u16>() {
                    if !ports.contains(&port) {
                        ports.push(port);
                    }
                }
            }
        }
        ports
    }
}

/// Sidebar presence: activity plus one short status line. Construct once and
/// keep it for the process lifetime; drop or `flush` before exit so a
/// trailing debounced status lands.
pub struct StatusReporter {
    host: Option<Host>,
    last_written: Option<(Instant, String)>,
    pending: Option<String>,
}

impl StatusReporter {
    pub fn detect() -> Self {
        StatusReporter {
            host: Host::detect(),
            last_written: None,
            pending: None,
        }
    }

    pub fn is_hosted(&self) -> bool {
        self.host.is_some()
    }

    /// The session is working — sidebar spinner. Usage scans are quick and
    /// local, so this app never claims Busy; kept because it is part of the
    /// three-event contract any App can use.
    #[allow(dead_code)]
    pub fn busy(&self) {
        self.post_hook_event("UserPromptSubmit");
    }

    /// The session settled — spinner clears, unread integrates.
    pub fn idle(&self) {
        self.post_hook_event("Stop");
    }

    /// The session needs the user — attention accent, and Unpeel's ordinary
    /// needs-input notification path (desktop banner, phone push).
    #[allow(dead_code)]
    pub fn attention(&self) {
        self.post_hook_event("PermissionRequest");
    }

    /// Raise an informational App alert. Unpeel owns persistence, unread,
    /// Recent surfaces, and desktop/phone delivery; this does not imply that
    /// the Session needs input and does not touch the lifecycle seed.
    pub fn alert(&self, title: &str, body: &str) {
        let Some(host) = &self.host else { return };
        let title = bounded_single_line(title, 120);
        let body_text = bounded_single_line(body, 512);
        if body_text.is_empty() {
            return;
        }
        let body = format!(
            r#"{{"kind":"alert","title":{},"body":{}}}"#,
            json_string(&title),
            json_string(&body_text)
        );
        post_json(host, &format!("/notify/{}", host.session_id), &body);
    }

    /// Short, single-line sidebar status ("Codex 3% · Claude $2.10 · Grok 14%").
    /// Rapid-fire and identical writes coalesce; the latest text wins.
    pub fn set_status(&mut self, text: &str) {
        if self.host.is_none() {
            return;
        }
        let text = text.trim().replace(['\n', '\r'], " ");
        if let Some((_, last)) = &self.last_written {
            if *last == text && self.pending.is_none() {
                return;
            }
        }
        if let Some((at, _)) = &self.last_written {
            if at.elapsed() < DEBOUNCE {
                self.pending = Some(text);
                return;
            }
        }
        self.write_status(&text);
    }

    pub fn flush(&mut self) {
        if let Some(text) = self.pending.take() {
            self.write_status(&text);
        }
    }

    fn write_status(&mut self, text: &str) {
        let Some(host) = &self.host else { return };
        // Never create the session dir — the session may already be gone.
        if !host.session_dir.is_dir() {
            return;
        }
        let body = format!(
            r#"{{"text":{},"updated_at":{}}}"#,
            json_string(text),
            now_ms()
        );
        let tmp = host.session_dir.join(".status.json.tmp");
        let ok = std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, host.session_dir.join("status.json")))
            .is_ok();
        if ok {
            // The ping is an optimisation; frontends still poll.
            post_json(host, "/state-changed", r#"{"change":"session-markers"}"#);
        }
        self.pending = None;
        self.last_written = Some((Instant::now(), text.to_string()));
    }

    fn post_hook_event(&self, event: &str) {
        let Some(host) = &self.host else { return };
        let body = format!(r#"{{"hook_event_name":"{event}"}}"#);
        // Durable seed first, so the latch survives even when no instance is
        // listening right now — the same file the provider hook scripts keep.
        if host.session_dir.is_dir() {
            let tmp = host
                .session_dir
                .join(format!(".last-hook-event.json.{}", std::process::id()));
            if std::fs::write(&tmp, &body).is_ok() {
                let _ = std::fs::rename(&tmp, host.session_dir.join("last-hook-event.json"));
            }
        }
        post_json(host, &format!("/hook/{}", host.session_id), &body);
    }
}

impl Drop for StatusReporter {
    fn drop(&mut self) {
        self.flush();
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

fn bounded_single_line(text: &str, maximum_utf16_units: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut units = 0;
    normalized
        .chars()
        .take_while(|character| {
            let next = units + character.len_utf16();
            if next > maximum_utf16_units {
                false
            } else {
                units = next;
                true
            }
        })
        .collect()
}

/// Fire-and-forget local POST to every registered instance. Failures are
/// ignored: a port whose owner has gone is normal.
fn post_json(host: &Host, path: &str, body: &str) {
    for port in host.ports() {
        let address = format!("127.0.0.1:{port}");
        let Ok(target) = address.parse() else {
            continue;
        };
        let Ok(mut stream) = TcpStream::connect_timeout(&target, IO_TIMEOUT) else {
            continue;
        };
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(request.as_bytes());
        let _ = stream.flush();
    }
}
