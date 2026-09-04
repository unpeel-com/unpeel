//! Live hook-event ingestion, the TUI's half of the multi-instance hook
//! broadcast: provider hook scripts POST every lifecycle event to every port
//! in `~/.unpeel/app-ports`, so the TUI registers its own port there and runs
//! the same minimal HTTP contract as the native `HookServer.swift` — 200
//! `{"ok":true}` for sessions whose manifest exists in this home, 404
//! otherwise (foreign instances must not swallow events).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, SystemTime};

use std::sync::Arc;

use unpeel_core::app_paths;

use crate::approvals::{already_granted, persist_grant, ApprovalHub};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(125);

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const PORT_REGISTRY_CAP: usize = 16;
const MAX_ALERT_TITLE_UNITS: usize = 120;
const MAX_ALERT_BODY_UNITS: usize = 512;

/// One App-owned informational alert. It is carried beside provider hook
/// events on the listener channel but never enters the lifecycle engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppAlertMessage {
    pub title: Option<String>,
    pub body: String,
}

pub struct HookEventMessage {
    pub session_id: String,
    pub event_name: String,
    pub tool_name: Option<String>,
    /// Host-owned managed-runtime generation carried by current Unpeel hook
    /// assets. `None` is the compatibility shape from older installed hooks.
    pub runtime_generation: Option<u64>,
    /// Captured as soon as the complete HTTP request reached this listener.
    /// Main-loop scheduling must not make an old runtime's Stop look newer
    /// than an in-place replacement launch recorded in the manifest.
    pub received_at: SystemTime,
    pub app_alert: Option<AppAlertMessage>,
}

impl HookEventMessage {
    /// The cross-frontend "shared state changed" ping (`/state-changed`),
    /// carried on the hook channel rather than a second one: it means the
    /// same thing to the run loop — refresh now, don't wait for the poll.
    pub fn state_change(kind: &str) -> Self {
        Self {
            session_id: String::new(),
            event_name: format!("__state__:{kind}"),
            tool_name: None,
            runtime_generation: None,
            received_at: SystemTime::now(),
            app_alert: None,
        }
    }

    pub fn is_state_change(&self) -> bool {
        self.event_name.starts_with("__state__:")
    }

    pub fn is_app_alert(&self) -> bool {
        self.app_alert.is_some()
    }
}

/// Human-readable name for an approval dialog: the Session's label when its
/// manifest is readable, else the short id. The raw ids stay in the body.
fn session_display_name(session_id: &str) -> String {
    let label = unpeel_core::session_host::load_manifest(session_id)
        .map(|manifest| manifest.session.label.trim().to_owned())
        .unwrap_or_default();
    if label.is_empty() {
        session_id[..8.min(session_id.len())].to_owned()
    } else {
        format!("“{label}”")
    }
}

fn normalized_alert_text(value: &str, max_utf16_units: usize) -> Result<Option<String>, ()> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Ok(None);
    }
    if normalized.encode_utf16().count() > max_utf16_units {
        return Err(());
    }
    Ok(Some(normalized))
}

fn app_alert_from_json(json: &serde_json::Value) -> Result<AppAlertMessage, ()> {
    if json.get("kind").and_then(serde_json::Value::as_str) != Some("alert") {
        return Err(());
    }
    let title = match json.get("title") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => normalized_alert_text(value.as_str().ok_or(())?, MAX_ALERT_TITLE_UNITS)?,
    };
    let body = normalized_alert_text(
        json.get("body")
            .and_then(serde_json::Value::as_str)
            .ok_or(())?,
        MAX_ALERT_BODY_UNITS,
    )?
    .ok_or(())?;
    Ok(AppAlertMessage { title, body })
}

fn runtime_generation_from_json(json: &serde_json::Value) -> Option<u64> {
    let value = json
        .get("unpeel_runtime_generation")
        .or_else(|| json.get("unpeelRuntimeGeneration"))?;
    value.as_u64()
}

pub struct HookListener {
    pub port: u16,
    pub events: Receiver<HookEventMessage>,
}

fn registry_path() -> std::path::PathBuf {
    app_paths::unpeel_home().join("app-ports")
}

fn read_registry_at(path: &std::path::Path) -> Vec<u16> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse::<u16>().ok())
        .collect()
}

fn write_registry_at(path: &std::path::Path, ports: &[u16]) {
    if ports.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let body = ports
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let tmp = parent.join(format!(
        ".app-ports.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
}

fn update_registry_at(path: &std::path::Path, port: u16, register: bool) {
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let lock_path = parent.join("app-ports.lock");
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)
    else {
        return;
    };
    let _ = std::fs::set_permissions(
        parent.join("app-ports.lock"),
        std::fs::Permissions::from_mode(0o600),
    );
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
        return;
    }
    let mut ports: Vec<u16> = read_registry_at(path)
        .into_iter()
        .filter(|existing| *existing != port)
        .collect();
    if register {
        ports.push(port);
        if ports.len() > PORT_REGISTRY_CAP {
            ports.drain(..ports.len() - PORT_REGISTRY_CAP);
        }
    }
    write_registry_at(path, &ports);
    unsafe {
        libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN);
    }
}

/// Same semantics as `HookServer.registerPort`: dedupe, append last (newest),
/// cap at 16 by dropping oldest.
fn register_port(port: u16) {
    update_registry_at(&registry_path(), port, true);
}

pub fn unregister_port(port: u16) {
    update_registry_at(&registry_path(), port, false);
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-Unpeel-Frontend: tui\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// `/mcp/approve-*` from the MCP host (which found our port in the
/// registry because no app is running). Auth: the shared x-unpeel-auth
/// token; blocking until the user answers in the TUI or from a phone.
fn handle_mcp(
    stream: &mut TcpStream,
    path: &str,
    headers: &std::collections::HashMap<String, String>,
    body: &[u8],
    hub: &Arc<ApprovalHub>,
) {
    if !unpeel_core::mcp_auth::verify_auth(headers.get("x-unpeel-auth").map(String::as_str)) {
        respond(stream, "401 Unauthorized", r#"{"error":"unauthorized"}"#);
        return;
    }
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let field = |key: &str| {
        json.get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let approve = |ok: bool, stream: &mut TcpStream| {
        respond(
            stream,
            "200 OK",
            &serde_json::json!({ "approved": ok }).to_string(),
        );
    };
    match path {
        "/mcp/approve-write" => {
            let (Some(caller), Some(target)) =
                (field("caller_session_id"), field("target_session_id"))
            else {
                respond(
                    stream,
                    "400 Bad Request",
                    r#"{"error":"caller_session_id and target_session_id are required"}"#,
                );
                return;
            };
            if already_granted("write", &caller, Some(&target)) {
                approve(true, stream);
                return;
            }
            let ok = hub.request(
                "write",
                format!(
                    "Allow session {} to write to session {}?",
                    session_display_name(&caller),
                    session_display_name(&target)
                ),
                format!("{caller} → {target}"),
                caller.clone(),
                Some(target.clone()),
                APPROVAL_TIMEOUT,
            );
            if ok {
                persist_grant("write", &caller, Some(&target));
            }
            approve(ok, stream);
        }
        "/mcp/approve-browser" | "/mcp/approve-computer" => {
            let kind = if path.ends_with("browser") {
                "browser"
            } else {
                "computer"
            };
            let Some(session_id) = field("session_id") else {
                respond(
                    stream,
                    "400 Bad Request",
                    r#"{"error":"session_id is required"}"#,
                );
                return;
            };
            if already_granted(kind, &session_id, None) {
                approve(true, stream);
                return;
            }
            let ok = hub.request(
                kind,
                format!(
                    "Allow {kind} access for session {}?",
                    session_display_name(&session_id)
                ),
                session_id.clone(),
                session_id.clone(),
                None,
                APPROVAL_TIMEOUT,
            );
            if ok {
                persist_grant(kind, &session_id, None);
            }
            approve(ok, stream);
        }
        "/mcp/approve-app-open" => {
            let (Some(caller), Some(app_id)) = (field("caller_session_id"), field("app_id")) else {
                respond(
                    stream,
                    "400 Bad Request",
                    r#"{"error":"caller_session_id and app_id are required"}"#,
                );
                return;
            };
            if already_granted("app-open", &caller, Some(&app_id)) {
                approve(true, stream);
                return;
            }
            let app_name = field("app_name").unwrap_or_else(|| app_id.clone());
            let caller_label = session_display_name(&caller);
            let ok = hub.request(
                "app-open",
                format!("Allow session {caller_label} to open {app_name}?"),
                format!("This remembers access to {app_name} for this session."),
                caller.clone(),
                // An App id is not a Session id. Keep the wire projection
                // honest; this handler retains the id while the hub blocks.
                None,
                APPROVAL_TIMEOUT,
            );
            if ok {
                persist_grant("app-open", &caller, Some(&app_id));
            }
            approve(ok, stream);
        }
        "/mcp/computer-permissions-needed" => {
            respond(stream, "200 OK", r#"{"ok":true}"#);
        }
        _ => respond(stream, "404 Not Found", r#"{"error":"not found"}"#),
    }
}

fn normalized_hex_accent(value: &str) -> Option<String> {
    let digits = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", digits.to_ascii_uppercase()))
}

fn handle_app_theme(
    stream: &mut TcpStream,
    session_id: &str,
    body: &[u8],
    overlay: &crate::overlay::SharedNativeOverlay,
) {
    let Some(manifest) = unpeel_core::session_host::load_manifest(session_id) else {
        respond(stream, "404 Not Found", r#"{"error":"unknown session"}"#);
        return;
    };
    let json: serde_json::Value = match serde_json::from_slice(body) {
        Ok(json) => json,
        Err(_) => {
            respond(stream, "400 Bad Request", r#"{"error":"invalid json"}"#);
            return;
        }
    };
    let is_dark = match json.get("scheme").and_then(serde_json::Value::as_str) {
        Some("dark") => true,
        Some("light") => false,
        _ => {
            respond(stream, "400 Bad Request", r#"{"error":"invalid scheme"}"#);
            return;
        }
    };
    let accent = overlay
        .snapshot()
        .as_ref()
        .and_then(|overlay| {
            crate::overlay::hosted_app_accent(overlay, &manifest.session.project_id, is_dark)
        })
        // Headless Hosts have no native UserDefaults overlay, but may opt
        // into the same workspace-level contract through their environment.
        .or_else(|| {
            std::env::var("UNPEEL_APP_ACCENT")
                .ok()
                .as_deref()
                .and_then(normalized_hex_accent)
        });
    respond(
        stream,
        "200 OK",
        &serde_json::json!({ "accent": accent }).to_string(),
    );
}

fn handle_app_context(
    stream: &mut TcpStream,
    session_id: &str,
    overlay: &crate::overlay::SharedNativeOverlay,
) {
    let Some(manifest) = unpeel_core::session_host::load_manifest(session_id) else {
        respond(stream, "404 Not Found", r#"{"error":"unknown session"}"#);
        return;
    };
    let overlay = overlay.snapshot();
    respond(
        stream,
        "200 OK",
        &crate::app_context::response_with_overlay(&manifest, overlay.as_ref()),
    );
}

fn handle_open_in_editor(
    stream: &mut TcpStream,
    session_id: &str,
    body: &[u8],
    platform_adapters: &crate::platform_adapter::PlatformAdapterHub,
) {
    let Some(manifest) = unpeel_core::session_host::load_manifest(session_id) else {
        respond(
            stream,
            "404 Not Found",
            r#"{"error":"unknown app session"}"#,
        );
        return;
    };
    if manifest.state != unpeel_core::session_host::HostedSessionState::Running
        || manifest.active_app.is_none()
    {
        respond(
            stream,
            "404 Not Found",
            r#"{"error":"unknown app session"}"#,
        );
        return;
    }
    let raw = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("path")?.as_str().map(str::to_owned));
    let path = raw
        .filter(|path| !path.is_empty() && path.len() <= 16_384)
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .and_then(|path| std::fs::canonicalize(path).ok());
    let Some(path) = path.and_then(|path| path.to_str().map(str::to_owned)) else {
        respond(stream, "400 Bad Request", r#"{"error":"invalid path"}"#);
        return;
    };
    if !platform_adapters.supports("app.open-in-editor") {
        respond(
            stream,
            "501 Not Implemented",
            r#"{"error":"opening an editor needs the Mac app"}"#,
        );
        return;
    }
    match platform_adapters.call("app.open-in-editor", serde_json::json!({ "path": path })) {
        Ok(response) => {
            let status = match response.status {
                200 => "200 OK",
                400 => "400 Bad Request",
                404 => "404 Not Found",
                501 => "501 Not Implemented",
                _ => "502 Bad Gateway",
            };
            respond(stream, status, &response.body.to_string());
        }
        Err(_) => respond(
            stream,
            "503 Service Unavailable",
            r#"{"error":"native platform adapter unavailable"}"#,
        ),
    }
}

fn handle_connection(
    mut stream: TcpStream,
    events: &Sender<HookEventMessage>,
    hub: &Arc<ApprovalHub>,
    overlay: &crate::overlay::SharedNativeOverlay,
    platform_adapters: &Arc<crate::platform_adapter::PlatformAdapterHub>,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let header_end;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
        if buffer.len() > MAX_BODY_BYTES {
            return;
        }
    }

    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();

    if method != "POST" {
        respond(
            &mut stream,
            "405 Method Not Allowed",
            r#"{"error":"method not allowed"}"#,
        );
        return;
    }
    let is_mcp = path.starts_with("/mcp/");
    // The cross-frontend change ping: another Unpeel wrote shared state and
    // is telling us to re-read it now rather than on our next poll. Carries
    // no session id.
    let is_state = path == unpeel_core::state_bus::ROUTE;
    let is_app_notify = path.starts_with("/notify/");
    let is_app_theme = path.starts_with("/app-theme/");
    let is_app_context = path.starts_with("/app-context/");
    let is_open_editor = path.starts_with("/open-in-editor/");
    let session_id = if is_mcp || is_state {
        String::new()
    } else {
        let route_id = if is_app_notify {
            path.strip_prefix("/notify/")
        } else if is_app_theme {
            path.strip_prefix("/app-theme/")
        } else if is_app_context {
            path.strip_prefix("/app-context/")
        } else if is_open_editor {
            path.strip_prefix("/open-in-editor/")
        } else {
            path.strip_prefix("/hook/")
        };
        match route_id.filter(|id| !id.is_empty()) {
            Some(id) if !id.contains('/') && !id.contains("..") => id.to_string(),
            _ => {
                respond(&mut stream, "404 Not Found", r#"{"error":"not found"}"#);
                return;
            }
        }
    };
    let content_length = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .next()
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        respond(
            &mut stream,
            "400 Bad Request",
            r#"{"error":"body too large"}"#,
        );
        return;
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    }
    // Timestamp receipt before JSON decoding, provider metadata persistence,
    // or Main-loop queueing can delay delivery. Restart-generation cutoffs
    // compare against when the old provider actually reached this socket.
    let received_at = SystemTime::now();

    if is_state {
        let kind = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| value.get("change")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        let _ = events.send(HookEventMessage::state_change(&kind));
        respond(&mut stream, "200 OK", r#"{"ok":true}"#);
        return;
    }

    if is_mcp {
        let mut headers = std::collections::HashMap::new();
        for line in head.lines().skip(1) {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_lowercase(), value.trim().to_string());
            }
        }
        let path = path.to_string();
        handle_mcp(&mut stream, &path, &headers, &body, hub);
        return;
    }
    if is_app_theme {
        handle_app_theme(&mut stream, &session_id, &body, overlay);
        return;
    }
    if is_app_context {
        handle_app_context(&mut stream, &session_id, overlay);
        return;
    }
    if is_open_editor {
        handle_open_in_editor(&mut stream, &session_id, &body, platform_adapters);
        return;
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) else {
        respond(
            &mut stream,
            "400 Bad Request",
            r#"{"error":"invalid json"}"#,
        );
        return;
    };

    if is_app_notify {
        let Some(manifest) = unpeel_core::session_host::load_manifest(&session_id) else {
            respond(
                &mut stream,
                "404 Not Found",
                r#"{"error":"unknown session"}"#,
            );
            return;
        };
        // Alerts are an Unpeel App surface, not a general-purpose local
        // notification socket. A stopped or non-App session cannot use it.
        if manifest.state != unpeel_core::session_host::HostedSessionState::Running
            || manifest.active_app.is_none()
        {
            respond(
                &mut stream,
                "404 Not Found",
                r#"{"error":"unknown app session"}"#,
            );
            return;
        }
        let Ok(app_alert) = app_alert_from_json(&json) else {
            respond(
                &mut stream,
                "400 Bad Request",
                r#"{"error":"invalid alert"}"#,
            );
            return;
        };
        let _ = events.send(HookEventMessage {
            session_id,
            event_name: "__app_alert__".to_string(),
            tool_name: None,
            runtime_generation: None,
            received_at,
            app_alert: Some(app_alert),
        });
        respond(&mut stream, "200 OK", r#"{"ok":true}"#);
        return;
    }

    let event_name = json
        .get("hook_event_name")
        .or_else(|| json.get("hookEventName"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(event_name) = event_name else {
        respond(
            &mut stream,
            "400 Bad Request",
            r#"{"error":"missing hook_event_name"}"#,
        );
        return;
    };

    let runtime_generation = runtime_generation_from_json(&json);

    let Some(manifest) = unpeel_core::session_host::load_manifest(&session_id) else {
        respond(
            &mut stream,
            "404 Not Found",
            r#"{"error":"unknown session"}"#,
        );
        return;
    };
    if runtime_generation.is_some_and(|generation| generation < manifest.runtime_launch_generation)
    {
        // A departed runtime can finish a background hook after Resume Agent
        // has committed its replacement. Acknowledge it so providers do not
        // retry, but never let it overwrite conversation metadata or enter the
        // activity queue for the new generation.
        respond(&mut stream, "200 OK", r#"{"ok":true}"#);
        return;
    }

    // Capture provider conversation metadata into the shared marker — the
    // same key candidates the app's HookServer accepts, so a session's
    // resume id lands on disk whichever frontend received the broadcast.
    let first_string = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            json.get(*key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
    };
    let provider_id = first_string(&[
        "session_id",
        "chatId",
        "chat_id",
        "provider_session_id",
        "providerSessionID",
        "providerSessionId",
        "thread_id",
        "threadID",
        "threadId",
        "conversation_id",
        "conversationID",
        "conversationId",
    ]);
    let transcript = first_string(&[
        "transcript_path",
        "transcriptPath",
        "provider_transcript_path",
        "providerTranscriptPath",
    ]);
    if provider_id.is_some() || transcript.is_some() {
        let changed = unpeel_core::session_ops::set_provider_session(
            &session_id,
            provider_id.as_deref(),
            transcript.as_deref(),
        )
        .unwrap_or(false);
        if changed {
            // The session's conversation identity moved (in-tool /resume or
            // /clear): if it is still untitled, title it from the resumed
            // conversation's transcript. Off-thread — this reads provider
            // storage and must not stall the hook listener.
            let session_id = session_id.clone();
            std::thread::spawn(move || {
                let _ = unpeel_core::transcripts::auto_title_session_from_transcript(&session_id);
            });
        }
    }

    let _ = events.send(HookEventMessage {
        session_id: session_id.to_string(),
        event_name: event_name.to_string(),
        tool_name: json
            .get("tool_name")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        runtime_generation,
        received_at,
        app_alert: None,
    });
    respond(&mut stream, "200 OK", r#"{"ok":true}"#);
}

/// Compatibility frontend entry: the released TUI still owns its historical
/// in-process listener and has no connection-scoped native adapter. New Host
/// work must use `start_with_platform` from the canonical serve driver.
pub fn start(hub: Arc<ApprovalHub>) -> Result<HookListener, String> {
    start_with_platform(
        hub,
        crate::overlay::SharedNativeOverlay::new(crate::overlay::load()),
        Arc::new(crate::platform_adapter::PlatformAdapterHub::default()),
    )
}

pub fn start_with_platform(
    hub: Arc<ApprovalHub>,
    overlay: crate::overlay::SharedNativeOverlay,
    platform_adapters: Arc<crate::platform_adapter::PlatformAdapterHub>,
) -> Result<HookListener, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("hook listener bind: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    register_port(port);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            let hub = Arc::clone(&hub);
            let overlay = overlay.clone();
            let platform_adapters = Arc::clone(&platform_adapters);
            std::thread::spawn(move || {
                handle_connection(stream, &tx, &hub, &overlay, &platform_adapters)
            });
        }
    });

    Ok(HookListener { port, events: rx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_registry_updates_preserve_every_frontend() {
        let dir =
            std::env::temp_dir().join(format!("unpeel-app-ports-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app-ports");
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut workers = Vec::new();
        for port in 41_000..41_008 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                update_registry_at(&path, port, true);
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let mut ports = read_registry_at(&path);
        ports.sort_unstable();
        assert_eq!(ports, (41_000..41_008).collect::<Vec<_>>());
        assert_eq!(
            std::fs::metadata(dir.join("app-ports.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_numeric_runtime_generation_and_legacy_shapes() {
        assert_eq!(
            runtime_generation_from_json(&serde_json::json!({"unpeel_runtime_generation": 7})),
            Some(7)
        );
        assert_eq!(
            runtime_generation_from_json(&serde_json::json!({"unpeelRuntimeGeneration": 8})),
            Some(8)
        );
        assert_eq!(
            runtime_generation_from_json(&serde_json::json!({"unpeel_runtime_generation": "8"})),
            None
        );
        assert_eq!(
            runtime_generation_from_json(&serde_json::json!({"hook_event_name": "Stop"})),
            None
        );
    }

    #[test]
    fn parses_and_bounds_app_alerts() {
        assert_eq!(
            app_alert_from_json(&serde_json::json!({
                "kind": "alert",
                "title": " Usage alert ",
                "body": "Close to\n weekly limit"
            })),
            Ok(AppAlertMessage {
                title: Some("Usage alert".to_string()),
                body: "Close to weekly limit".to_string(),
            })
        );
        assert!(app_alert_from_json(&serde_json::json!({
            "kind": "alert",
            "body": ""
        }))
        .is_err());
        assert!(app_alert_from_json(&serde_json::json!({
            "kind": "alert",
            "body": "x".repeat(MAX_ALERT_BODY_UNITS + 1)
        }))
        .is_err());
        assert!(app_alert_from_json(&serde_json::json!({
            "kind": "needs_input",
            "body": "No"
        }))
        .is_err());
    }
}
