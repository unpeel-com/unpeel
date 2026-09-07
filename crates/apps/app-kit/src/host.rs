//! Shared, optional integration for a Ratatui App hosted by Unpeel.
//!
//! This is a convenience implementation of Unpeel's public file + loopback
//! HTTP contract, not a requirement on standalone Apps. Every operation is a
//! silent no-op outside a hosted Session.

use std::io::Write as _;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const IO_TIMEOUT: Duration = Duration::from_millis(250);
const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) struct HostedSession {
    pub(crate) session_id: String,
    pub(crate) session_dir: PathBuf,
    pub(crate) app_port: Option<u16>,
    pub(crate) port_registry: PathBuf,
}

impl HostedSession {
    pub(crate) fn detect() -> Option<Self> {
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
        Some(Self {
            session_id,
            session_dir,
            app_port: std::env::var("UNPEEL_APP_PORT")
                .ok()
                .and_then(|port| port.parse().ok()),
            port_registry,
        })
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self.app_port.into_iter().collect();
        if let Ok(raw) = std::fs::read_to_string(&self.port_registry) {
            for line in raw.lines() {
                if let Ok(port) = line.trim().parse::<u16>()
                    && !ports.contains(&port)
                {
                    ports.push(port);
                }
            }
        }
        ports
    }
}

/// Shared App→Unpeel reporter for sidebar activity/status, agent-readable live
/// context, automatic titles, and informational alerts.
///
/// Construct one per process with the reverse-DNS App id and keep it alive for
/// the event loop. Rapid status/context changes are deduplicated and debounced;
/// [`Self::flush`] (also called on drop) lands the last pending value.
#[derive(Debug)]
pub struct AppReporter {
    app_id: String,
    host: Option<HostedSession>,
    status_last: Option<(Instant, String)>,
    status_pending: Option<String>,
    context_last: Option<(Instant, String)>,
    context_pending: Option<String>,
}

impl AppReporter {
    /// Detects an Unpeel Host; returns an inert reporter when standalone.
    #[must_use]
    pub fn detect(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            host: HostedSession::detect(),
            status_last: None,
            status_pending: None,
            context_last: None,
            context_pending: None,
        }
    }

    /// Whether this process is running inside an Unpeel hosted Session.
    #[must_use]
    pub fn is_hosted(&self) -> bool {
        self.host.is_some()
    }

    /// Current hosted Session id, when available.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.host.as_ref().map(|host| host.session_id.as_str())
    }

    /// Marks the App as working, using Unpeel's ordinary activity engine.
    pub fn busy(&self) {
        self.post_hook_event("UserPromptSubmit");
    }

    /// Marks the App as settled/idle.
    pub fn idle(&self) {
        self.post_hook_event("Stop");
    }

    /// Marks the App as requiring the user's attention.
    pub fn attention(&self) {
        self.post_hook_event("PermissionRequest");
    }

    /// Sets the short single-line status shown with the App Session.
    pub fn set_status(&mut self, text: &str) {
        if self.host.is_none() {
            return;
        }
        let text = single_line(text);
        if self
            .status_last
            .as_ref()
            .is_some_and(|(_, last)| *last == text)
            && self.status_pending.is_none()
        {
            return;
        }
        if self
            .status_last
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < DEBOUNCE)
        {
            self.status_pending = Some(text);
            return;
        }
        self.write_status(&text);
    }

    /// Publishes the App-owned live context surfaced verbatim to neighboring
    /// agents through Unpeel MCP pane-context queries.
    pub fn set_context(&mut self, context: &Value) {
        if self.host.is_none() {
            return;
        }
        let entry = serde_json::json!({
            "app": self.app_id.as_str(),
            "context": context,
        })
        .to_string();
        if self
            .context_last
            .as_ref()
            .is_some_and(|(_, last)| *last == entry)
            && self.context_pending.is_none()
        {
            return;
        }
        if self
            .context_last
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < DEBOUNCE)
        {
            self.context_pending = Some(entry);
            return;
        }
        self.write_context(&entry);
    }

    /// Reports the current App document/resource for Unpeel's automatic title.
    pub fn set_title(&self, text: &str) {
        let Some(host) = &self.host else { return };
        if !host.session_dir.is_dir() {
            return;
        }
        let text = single_line(text);
        if text.is_empty() {
            return;
        }
        let body = serde_json::json!({
            "text": text,
            "updated_at": now_ms(),
        })
        .to_string();
        if atomic_marker(host, "app-title.json", body.as_bytes()) {
            post_json(host, "/state-changed", r#"{"change":"session-markers"}"#);
        }
    }

    /// Emits a bounded informational alert. This enters Unpeel's Recent and
    /// notification surfaces without claiming that the App needs input.
    pub fn alert(&self, title: &str, body: &str) {
        let Some(host) = &self.host else { return };
        let body_text = bounded_single_line(body, 512);
        if body_text.is_empty() {
            return;
        }
        let payload = serde_json::json!({
            "kind": "alert",
            "title": bounded_single_line(title, 120),
            "body": body_text,
        })
        .to_string();
        post_json(host, &format!("/notify/{}", host.session_id), &payload);
    }

    /// Lands the newest debounced status and context immediately.
    pub fn flush(&mut self) {
        if let Some(text) = self.status_pending.take() {
            self.write_status(&text);
        }
        if let Some(entry) = self.context_pending.take() {
            self.write_context(&entry);
        }
    }

    fn write_status(&mut self, text: &str) {
        let Some(host) = &self.host else { return };
        if !host.session_dir.is_dir() {
            return;
        }
        let body = serde_json::json!({
            "text": text,
            "updated_at": now_ms(),
        })
        .to_string();
        if atomic_marker(host, "status.json", body.as_bytes()) {
            post_json(host, "/state-changed", r#"{"change":"session-markers"}"#);
        }
        self.status_pending = None;
        self.status_last = Some((Instant::now(), text.to_string()));
    }

    fn write_context(&mut self, entry: &str) {
        let Some(host) = &self.host else { return };
        if !host.session_dir.is_dir() {
            return;
        }
        let mut body = serde_json::from_str::<Value>(entry).unwrap_or(Value::Null);
        if let Some(object) = body.as_object_mut() {
            object.insert("updated_at".into(), serde_json::json!(now_ms()));
        }
        let _ = atomic_marker(host, "app-context.json", body.to_string().as_bytes());
        self.context_pending = None;
        self.context_last = Some((Instant::now(), entry.to_string()));
    }

    fn post_hook_event(&self, event: &str) {
        let Some(host) = &self.host else { return };
        let body = serde_json::json!({ "hook_event_name": event }).to_string();
        if host.session_dir.is_dir() {
            let _ = atomic_marker(host, "last-hook-event.json", body.as_bytes());
        }
        post_json(host, &format!("/hook/{}", host.session_id), &body);
    }
}

impl Drop for AppReporter {
    fn drop(&mut self) {
        self.flush();
    }
}

fn atomic_marker(host: &HostedSession, filename: &str, body: &[u8]) -> bool {
    let temporary = host
        .session_dir
        .join(format!(".{filename}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, body)
        .and_then(|()| std::fs::rename(&temporary, host.session_dir.join(filename)))
        .is_ok()
}

fn single_line(text: &str) -> String {
    text.trim().replace(['\n', '\r'], " ")
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

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn post_json(host: &HostedSession, path: &str, body: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reporter_for(directory: &std::path::Path) -> AppReporter {
        AppReporter {
            app_id: "unpeel.app.test".into(),
            host: Some(HostedSession {
                session_id: "test-session".into(),
                session_dir: directory.to_path_buf(),
                app_port: None,
                port_registry: directory.join("no-ports"),
            }),
            status_last: None,
            status_pending: None,
            context_last: None,
            context_pending: None,
        }
    }

    #[test]
    fn context_marker_is_typed_deduplicated_and_flushes_the_latest_value() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("app-context.json");
        let mut reporter = reporter_for(directory.path());
        let first = serde_json::json!({ "file": "hero.md", "line": 3 });
        reporter.set_context(&first);
        let written: Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        assert_eq!(written["app"], "unpeel.app.test");
        assert_eq!(written["context"], first);

        std::fs::remove_file(&marker).unwrap();
        reporter.set_context(&first);
        assert!(!marker.exists());
        reporter.set_context(&serde_json::json!({ "file": "hero.md", "line": 4 }));
        reporter.flush();
        let written: Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        assert_eq!(written["context"]["line"], 4);
    }

    #[test]
    fn status_and_title_use_the_shared_markers() {
        let directory = tempfile::tempdir().unwrap();
        let mut reporter = reporter_for(directory.path());
        reporter.set_status("  loading\nitems  ");
        reporter.set_title("  Current note\r\n");
        let status: Value =
            serde_json::from_slice(&std::fs::read(directory.path().join("status.json")).unwrap())
                .unwrap();
        let title: Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("app-title.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(status["text"], "loading items");
        assert_eq!(title["text"], "Current note");
    }
}
