//! Unpeel Browser MCP: `unpeel-host __browser_mcp__` speaks MCP (JSON-RPC 2.0
//! over stdio) and gives an agent session a real browser through the bundled
//! `agent-browser` engine.
//!
//! The engine has no MCP mode of its own (it is a CLI-first tool), so this
//! server owns the tool schema and translates each call into one engine CLI
//! invocation. Because the server constructs the engine argv itself, the agent
//! can never pass policy-overriding flags; access is re-checked against
//! `app-state.json` on every call, the same live-gate pattern as
//! `mcp_host.rs`.
//!
//! Engine mode is the native CDP daemon (`AGENT_BROWSER_NATIVE=1`). By default
//! it drives system Chrome/Chromium with zero runtime dependencies (no Node,
//! no Playwright, no Chromium download). A Host provisioner may instead write
//! an owner-only `~/.unpeel/browser/remote-cdp.json`; the same agent-browser
//! daemon then attaches to that provider-owned browser over authenticated WSS
//! CDP or a bare loopback port. Each Unpeel session still gets an isolated
//! engine daemon/socket (`unpeel-<session-id>`) under
//! `~/.unpeel/browser/sockets`.

use crate::mcp_host::{self_session_id, strip_ansi};
use crate::session_host;
use crate::state::{current_timestamp_ms, BrowserAccess};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::AsRawFd;

pub const BROWSER_MCP_ARG: &str = "__browser_mcp__";
pub const BROWSER_CLEANUP_ARG: &str = "__browser_cleanup__";

const PROTOCOL_VERSION_FALLBACK: &str = "2025-06-18";
const SERVER_NAME: &str = "unpeel-browser";
/// Engine calls launch Chrome on first use; give them room. `wait` calls get
/// the engine's own 25s default timeout well inside this.
const ENGINE_TIMEOUT_MS: u64 = 60_000;
/// Cleanup should never hang a session-close path.
const CLEANUP_TIMEOUT_MS: u64 = 10_000;
const ENGINE_OUTPUT_MAX_CHARS: usize = 32_000;
const WAIT_MS_MAX: u64 = 30_000;
const REMOTE_CDP_CONFIG_SCHEMA: u64 = 1;
const REMOTE_CDP_CONFIG_MAX_BYTES: u64 = 16 * 1024;
const REMOTE_CDP_ENGINE_ENV: &str = "AGENT_BROWSER_CDP";
const PROJECT_BROWSER_RECORD_SCHEMA: u64 = 1;
const PROJECT_BROWSER_IDLE_TIMEOUT_MS: u64 = 60 * 60 * 1000;
const PROJECT_MEMBER_START_GRACE_MS: u64 = 2 * 60 * 1000;

pub fn run_stdio() -> Result<(), String> {
    trace(&format!(
        "start self={} pid={}",
        self_session_id().unwrap_or_else(|| "-".into()),
        std::process::id()
    ));

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("Failed to read MCP stdin: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            trace("dropped unparseable message");
            continue;
        };
        if let Some(response) = handle_message(&message) {
            let body = serde_json::to_string(&response)
                .map_err(|e| format!("Failed to encode MCP response: {e}"))?;
            let mut out = stdout.lock();
            out.write_all(body.as_bytes())
                .and_then(|_| out.write_all(b"\n"))
                .and_then(|_| out.flush())
                .map_err(|e| format!("Failed to write MCP response: {e}"))?;
        }
    }
    trace("stdin closed, exiting");
    Ok(())
}

/// `unpeel-host __browser_cleanup__ <session-id>`: close the session's engine
/// daemon (and its browser) and remove its socket/pid files. Called by the
/// native app when a session is closed or pruned, because the engine daemon
/// deliberately outlives both the MCP server and the provider CLI.
/// Best-effort: a missing engine binary or dead daemon is not an error.
pub fn run_cleanup(args: &[String]) -> Result<(), String> {
    let session_id = args
        .first()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or("Usage: unpeel-host __browser_cleanup__ <session-id>")?;

    match resolve_engine_binary() {
        Ok(binary) => {
            let result = close_session_browser(&binary, session_id);
            trace(&format!(
                "cleanup session={session_id} close={}",
                match &result {
                    Ok(_) => "ok".to_string(),
                    Err(error) => format!("err {}", compact_one_line(error)),
                }
            ));
        }
        Err(_) => remove_engine_sidecars(&engine_session_key(session_id)),
    }
    Ok(())
}

fn handle_message(message: &Value) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str)?;
    let id = match message.get("id") {
        Some(id) if !id.is_null() => id.clone(),
        _ => return None,
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    let outcome = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => tools_call(&params),
        _ => Err(json!({
            "code": -32601,
            "message": format!("Method not found: {method}"),
        })),
    };

    Some(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

fn initialize_result(params: &Value) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION_FALLBACK);
    let options = load_options();
    let isolation = match load_remote_cdp_binding() {
        Ok(Some(binding)) => format!(
            "This session has a pinned tab in the {} remote browser. Other sessions cannot \
silently take over this tab.",
            binding.provider
        ),
        Ok(None)
            if options.settings.allowed_domains.trim().is_empty()
                && self_session_id()
                    .as_deref()
                    .and_then(|id| options.project_scope(id))
                    .is_some() =>
        {
            "This session has its own pinned tab in the project browser window. Sessions in the \
same project tree share that window, profile, cookies, and logins; other projects do not."
                .to_string()
        }
        _ => "This session has its own isolated local browser, profile, cookies, and window."
            .to_string(),
    };
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": format!("Operate a real browser for this Unpeel session. {isolation} \
    Core loop: \
    browser_open a URL, browser_snapshot to get element refs like @e1, act by ref \
    (browser_click/browser_fill), then re-snapshot after navigation or DOM changes — refs go \
    stale. Prefer refs from snapshots over CSS selectors, and selectors over coordinates. Use \
    browser_wait after actions that trigger loads. browser_screenshot saves into this session's \
    artifact folder and returns the file path. Check browser_console when a page misbehaves. \
    Do not paste cookies, tokens, passwords, or downloaded private files into the conversation \
    unless the user explicitly asks. Call browser_context first if you need the current \
    configuration or the browser tools seem unavailable."),
    })
}

pub(crate) fn tools_call(params: &Value) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).ok_or(json!({
        "code": -32602,
        "message": "tools/call requires a string 'name'",
    }))?;
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    // Re-checked per call so turning Browser Access off applies immediately,
    // even to already-connected sessions. browser_context stays available so
    // an agent can discover *why* the tools are refusing.
    let outcome = if name == "browser_context" {
        tool_browser_context()
    } else if let Some(reason) = caller_refusal_reason() {
        Err(reason)
    } else {
        run_tool(name, &arguments)
    };

    match outcome {
        Ok(text) => {
            trace(&format!("tool={name} ok"));
            Ok(json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false,
            }))
        }
        Err(error) => {
            trace(&format!("tool={name} error={}", compact_one_line(&error)));
            Ok(json!({
                "content": [{ "type": "text", "text": error }],
                "isError": true,
            }))
        }
    }
}

pub(crate) fn run_tool(name: &str, arguments: &Value) -> Result<String, String> {
    match name {
        "browser_open" => tool_open(arguments),
        "browser_snapshot" => tool_snapshot(arguments),
        "browser_click" => tool_click(arguments),
        "browser_fill" => tool_fill(arguments),
        "browser_type" => tool_type(arguments),
        "browser_press" => tool_press(arguments),
        "browser_get" => tool_get(arguments),
        "browser_screenshot" => tool_screenshot(arguments),
        "browser_wait" => tool_wait(arguments),
        "browser_scroll" => tool_scroll(arguments),
        "browser_console" => tool_console(arguments),
        "browser_close" => tool_close(arguments),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

/// The Browser Access state read from `app-state.json`: per-session overrides
/// plus the app-wide default. Read leniently per call — a malformed override
/// value parses to Off (never silently grants an explicit setting), an absent
/// default means the shipped default (On), and turning the default Off in
/// Settings is the master disable.
struct BrowserSecurity {
    grants: HashMap<String, BrowserAccess>,
    default_grant: BrowserAccess,
    /// Session ids the user approved under `Ask` (`browser_approvals`).
    approvals: Vec<String>,
}

fn load_security() -> BrowserSecurity {
    load_security_at(&crate::app_paths::app_state_path())
}

fn load_security_at(path: &Path) -> BrowserSecurity {
    let value = match std::fs::read(path) {
        Ok(raw) => match serde_json::from_slice::<Value>(&raw) {
            Ok(Value::Object(object)) => Value::Object(object),
            Ok(_) | Err(_) => return denied_browser_security(),
        },
        // A genuinely missing file is a fresh install and keeps the shipped
        // default. Existing empty/whitespace files fail parsing above and are
        // treated as truncated state instead.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(_) => return denied_browser_security(),
    };
    let grants = value
        .get("browser_access")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(id, raw)| {
                    // An explicitly present but malformed override is Off,
                    // never "missing" (which would inherit the app default).
                    let access = raw
                        .as_str()
                        .map(BrowserAccess::from_state_str)
                        .unwrap_or(BrowserAccess::Off);
                    (id.clone(), access)
                })
                .collect()
        })
        .unwrap_or_default();
    let default_grant = match value.get("browser_default_access") {
        // A genuinely absent field is the shipped default (On).
        None => BrowserAccess::default(),
        // Unknown strings already fail closed in `from_state_str`.
        Some(Value::String(raw)) => BrowserAccess::from_state_str(raw),
        // A present value with the wrong JSON type is malformed, not absent.
        Some(_) => BrowserAccess::Off,
    };
    let approvals = value
        .get("browser_approvals")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    BrowserSecurity {
        grants,
        default_grant,
        approvals,
    }
}

fn denied_browser_security() -> BrowserSecurity {
    BrowserSecurity {
        grants: HashMap::new(),
        default_grant: BrowserAccess::Off,
        approvals: Vec::new(),
    }
}

impl BrowserSecurity {
    fn effective_access(&self, session_id: &str) -> BrowserAccess {
        self.grants
            .get(session_id)
            .copied()
            .unwrap_or(self.default_grant)
    }
}

pub(crate) fn caller_refusal_reason() -> Option<String> {
    let Some(session_id) = self_session_id() else {
        return Some(
            "The calling session is unknown, so Unpeel MCP can't authorize browser access. \
Run this from a hosted Unpeel session."
                .into(),
        );
    };
    if session_host::load_manifest(&session_id).is_none() {
        return Some(
            "The calling session has no Unpeel manifest, so Unpeel MCP can't authorize browser \
access. Run this from a hosted Unpeel session."
                .into(),
        );
    }
    let security = load_security();
    match security.effective_access(&session_id) {
        BrowserAccess::On => None,
        BrowserAccess::Off => Some(
            "This session's Browser Access is off, so the browser tools are unavailable. The \
user can turn it on in Settings ▸ Browser."
                .into(),
        ),
        BrowserAccess::Ask => {
            if security.approvals.iter().any(|id| id == &session_id) {
                return None;
            }
            match request_browser_approval(&session_id) {
                Ok(true) => None,
                Ok(false) => Some(
                    "The user declined browser access for this session. Do not retry; ask \
the user to approve it in Settings ▸ Browser if they change their mind."
                        .into(),
                ),
                Err(error) => Some(format!(
                    "Browser access needs the user's approval, but the approval prompt could \
not be shown ({error}). Ask the user to open Unpeel and retry, or set Settings ▸ Browser to \
Allow."
                )),
            }
        }
    }
}

/// Blocking approval round-trip to the app (alert on the desktop), same
/// contract as computer approvals; persistence into `browser_approvals`
/// happens app-side on Allow. 130s client timeout inside the HookServer's
/// 150s ceiling for approval routes.
fn request_browser_approval(session_id: &str) -> Result<bool, String> {
    let response = crate::mcp_host::app_request_with_timeout(
        "/mcp/approve-browser",
        &serde_json::json!({ "session_id": session_id }),
        Duration::from_secs(130),
    )?;
    Ok(response.get("approved").and_then(Value::as_bool) == Some(true))
}

fn caller_session_id() -> Result<String, String> {
    self_session_id().ok_or_else(|| "The calling session is unknown.".to_string())
}

/// App-wide engine options plus the pieces of app state they depend on
/// (`theme` for the color scheme, `projects` for the per-project profile
/// root). Read leniently per call so Settings changes apply to the next
/// engine invocation without any restart.
struct BrowserOptions {
    settings: crate::state::BrowserSettings,
    auto_add_screenshots_to_gallery: bool,
    /// "dark" / "light" → passed to the engine; anything else (system,
    /// absent) leaves the engine to its own default.
    theme: Option<String>,
    projects: Vec<crate::state::Project>,
}

fn load_options() -> BrowserOptions {
    let path = crate::app_paths::unpeel_home().join("app-state.json");
    let value = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    let settings = value
        .get("browser_settings")
        .cloned()
        .and_then(|raw| serde_json::from_value::<crate::state::BrowserSettings>(raw).ok())
        .unwrap_or_default();
    let auto_add_screenshots_to_gallery = value
        .get("mcp_auto_add_browser_screenshots")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let theme = value
        .get("theme")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .filter(|theme| theme == "dark" || theme == "light");
    let projects = value
        .get("projects")
        .cloned()
        .and_then(|raw| serde_json::from_value::<Vec<crate::state::Project>>(raw).ok())
        .unwrap_or_default();
    BrowserOptions {
        settings,
        auto_add_screenshots_to_gallery,
        theme,
        projects,
    }
}

impl BrowserOptions {
    /// Browser scope for the caller's project tree. Worktree projects resolve
    /// to their top-level parent, so every Session in that tree shares one
    /// window/profile while unrelated projects remain isolated.
    fn project_scope(&self, session_id: &str) -> Option<ProjectBrowserScope> {
        if self.settings.profile_mode.trim() != "project" {
            return None;
        }
        let manifest = session_host::load_manifest(session_id)?;
        let root = crate::state::project_tree_root(&self.projects, &manifest.session.project_id);
        // Project ids are UUID-like, but sanitize defensively — this becomes
        // a directory name.
        let safe: String = root
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let digest = format!("{:x}", Sha256::digest(root.as_bytes()));
        let key = digest.chars().take(16).collect::<String>();
        let browser_root = crate::app_paths::unpeel_home().join("browser");
        Some(ProjectBrowserScope {
            root_id: root,
            safe_root: safe.clone(),
            key: key.clone(),
            profile_dir: browser_root.join("profiles").join(&safe),
            state_dir: browser_root.join("projects").join(&key),
            owner_session_key: format!("unpeel-project-{key}"),
            state_name: format!("unpeel-proj-{safe}"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectBrowserScope {
    root_id: String,
    safe_root: String,
    key: String,
    profile_dir: PathBuf,
    state_dir: PathBuf,
    owner_session_key: String,
    state_name: String,
}

impl ProjectBrowserScope {
    fn members_dir(&self) -> PathBuf {
        self.state_dir.join("members")
    }

    fn record_path(&self) -> PathBuf {
        self.state_dir.join("browser.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.state_dir.join("lock")
    }

    fn downloads_dir(&self) -> PathBuf {
        self.state_dir.join("downloads")
    }

    fn member_path(&self, session_id: &str) -> PathBuf {
        let digest = format!("{:x}", Sha256::digest(session_id.as_bytes()));
        self.members_dir().join(format!("{}.json", &digest[..16]))
    }
}

// ---------------------------------------------------------------------------
// Engine plumbing
// ---------------------------------------------------------------------------

/// Locate the `agent-browser` engine binary — the shared order in
/// `browser_engine::resolve`: `UNPEEL_AGENT_BROWSER_BIN` (or the older
/// `UNPEEL_BROWSER_BIN`) → the Host-installed, hash-verified
/// `~/.unpeel/browser/bin/agent-browser` → next to `unpeel-host` (the app
/// bundle, a compatibility candidate until the repo split) → PATH. A missing
/// engine names the `unpeel browser install` fix.
fn resolve_engine_binary() -> Result<PathBuf, String> {
    crate::browser_engine::resolve(&crate::app_paths::unpeel_home())
}

fn engine_session_key(session_id: &str) -> String {
    format!("unpeel-{session_id}")
}

fn engine_socket_dir() -> PathBuf {
    crate::app_paths::unpeel_home()
        .join("browser")
        .join("sockets")
}

/// Optional Host-owned external browser binding. Provisioners write this
/// owner-only file with either an authenticated CDP WebSocket URL or a bare
/// loopback port for a browser that the Host platform already exposes inside
/// its own container (Upstash Browser currently uses port 9222). Keeping a
/// credentialed URL out of app-state avoids copying it through normal settings
/// DTOs and Controller bootstrap payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteCdpBinding {
    endpoint: String,
    provider: String,
}

fn remote_cdp_config_path() -> PathBuf {
    crate::app_paths::unpeel_home()
        .join("browser")
        .join("remote-cdp.json")
}

fn load_remote_cdp_binding() -> Result<Option<RemoteCdpBinding>, String> {
    load_remote_cdp_binding_at(&remote_cdp_config_path())
}

fn load_remote_cdp_binding_at(path: &Path) -> Result<Option<RemoteCdpBinding>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not inspect remote browser configuration at {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Remote browser configuration at {} must be a regular file, not a symlink.",
            path.display()
        ));
    }
    if metadata.len() > REMOTE_CDP_CONFIG_MAX_BYTES {
        return Err(format!(
            "Remote browser configuration at {} is too large.",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "Remote browser configuration at {} must be owner-only (chmod 600).",
                path.display()
            ));
        }
    }

    let raw = std::fs::read(path).map_err(|error| {
        format!(
            "Could not read remote browser configuration at {}: {error}",
            path.display()
        )
    })?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| {
        format!(
            "Remote browser configuration at {} is not valid JSON.",
            path.display()
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        format!(
            "Remote browser configuration at {} must be a JSON object.",
            path.display()
        )
    })?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "schema" | "endpoint" | "provider"))
    {
        return Err(format!(
            "Remote browser configuration at {} contains an unknown field.",
            path.display()
        ));
    }
    if object.get("schema").and_then(Value::as_u64) != Some(REMOTE_CDP_CONFIG_SCHEMA) {
        return Err(format!(
            "Remote browser configuration at {} has an unsupported schema.",
            path.display()
        ));
    }
    let endpoint = object
        .get("endpoint")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .ok_or_else(|| {
            format!(
                "Remote browser configuration at {} is missing endpoint.",
                path.display()
            )
        })?;
    validate_remote_cdp_endpoint(endpoint).map_err(|reason| {
        format!(
            "Remote browser configuration at {} has an invalid endpoint: {reason}",
            path.display()
        )
    })?;
    let provider = object
        .get("provider")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or("remote");
    if provider.len() > 64
        || !provider
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(format!(
            "Remote browser configuration at {} has an invalid provider label.",
            path.display()
        ));
    }

    Ok(Some(RemoteCdpBinding {
        endpoint: endpoint.to_string(),
        provider: provider.to_string(),
    }))
}

fn validate_remote_cdp_endpoint(endpoint: &str) -> Result<(), &'static str> {
    if endpoint.len() > 8 * 1024 {
        return Err("the URL is too long");
    }
    if endpoint
        .chars()
        .any(|ch| ch.is_ascii_control() || ch.is_ascii_whitespace())
    {
        return Err("the URL contains whitespace or control characters");
    }
    // agent-browser treats a bare numeric port as a loopback CDP endpoint.
    // This is intentionally narrower than accepting ws:// or http:// URLs:
    // the port form cannot redirect the Host to an untrusted network peer.
    if endpoint.bytes().all(|byte| byte.is_ascii_digit()) {
        return endpoint
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .map(|_| ())
            .ok_or("the loopback port must be between 1 and 65535");
    }

    let Some(authority_and_path) = endpoint.strip_prefix("wss://") else {
        return Err(
            "only authenticated TLS WebSockets (wss://) or a bare loopback port are allowed",
        );
    };
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("the URL must contain a host and no user-info credentials");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectCdpBinding {
    endpoint: String,
    scope: ProjectBrowserScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CdpBinding {
    Remote(RemoteCdpBinding),
    Project(ProjectCdpBinding),
}

impl CdpBinding {
    fn endpoint(&self) -> &str {
        match self {
            CdpBinding::Remote(binding) => &binding.endpoint,
            CdpBinding::Project(binding) => &binding.endpoint,
        }
    }

    fn project_scope(&self) -> Option<&ProjectBrowserScope> {
        match self {
            CdpBinding::Project(binding) => Some(&binding.scope),
            CdpBinding::Remote(_) => None,
        }
    }
}

fn engine_binding_fingerprint(binding: Option<&CdpBinding>) -> String {
    match binding {
        None => "local".to_string(),
        Some(CdpBinding::Remote(binding)) => {
            format!("remote:{:x}", Sha256::digest(binding.endpoint.as_bytes()))
        }
        Some(CdpBinding::Project(binding)) => format!(
            "project:{}:{:x}",
            binding.scope.key,
            Sha256::digest(binding.endpoint.as_bytes())
        ),
    }
}

fn project_key_from_binding_fingerprint(value: &str) -> Option<&str> {
    let suffix = value.strip_prefix("project:")?;
    suffix.split(':').next().filter(|key| !key.is_empty())
}

fn engine_binding_marker_path(session_id: &str) -> PathBuf {
    engine_socket_dir().join(format!("{}.binding", engine_session_key(session_id)))
}

fn write_engine_binding_marker(path: &Path, value: &str) -> Result<(), String> {
    std::fs::write(path, format!("{value}\n")).map_err(|error| {
        format!(
            "Could not record this session's browser connection mode at {}: {error}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
fn redact_remote_cdp_secret(text: String, binding: Option<&CdpBinding>) -> String {
    match binding {
        Some(CdpBinding::Remote(binding)) if binding.endpoint.starts_with("wss://") => {
            text.replace(&binding.endpoint, "<remote-cdp-url>")
        }
        None | Some(_) => text,
    }
}

fn apply_cdp_binding(command: &mut Command, binding: Option<&CdpBinding>) {
    if let Some(binding) = binding {
        command
            .env(REMOTE_CDP_ENGINE_ENV, binding.endpoint())
            // Every Unpeel session attached to a shared Chrome owns one
            // strict tab. The sticky engine setting plus the per-call env
            // survives daemon restarts and prevents cross-session fallback.
            .env("AGENT_BROWSER_PIN_TAB", "1");
    }
}

struct ProjectBrowserLock {
    file: File,
}

impl Drop for ProjectBrowserLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn acquire_project_browser_lock(scope: &ProjectBrowserScope) -> Result<ProjectBrowserLock, String> {
    std::fs::create_dir_all(&scope.state_dir).map_err(|error| {
        format!(
            "Could not create project browser state at {}: {error}",
            scope.state_dir.display()
        )
    })?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(scope.lock_path())
        .map_err(|error| format!("Could not open the project browser lock: {error}"))?;
    #[cfg(unix)]
    {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(format!(
                "Could not lock the project browser: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(ProjectBrowserLock { file })
}

fn validate_project_cdp_endpoint(endpoint: &str) -> Result<(), &'static str> {
    if endpoint.len() > 1024
        || endpoint
            .chars()
            .any(|ch| ch.is_ascii_control() || ch.is_ascii_whitespace())
    {
        return Err("the endpoint is malformed");
    }
    let remainder = endpoint
        .strip_prefix("ws://127.0.0.1:")
        .or_else(|| endpoint.strip_prefix("ws://localhost:"))
        .or_else(|| endpoint.strip_prefix("ws://[::1]:"))
        .ok_or("the endpoint is not loopback WebSocket CDP")?;
    let port = remainder
        .split(['/', '?', '#'])
        .next()
        .and_then(|raw| raw.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or("the endpoint has an invalid loopback port")?;
    let _ = port;
    Ok(())
}

fn project_browser_record(scope: &ProjectBrowserScope, endpoint: &str) -> Value {
    json!({
        "schema": PROJECT_BROWSER_RECORD_SCHEMA,
        "projectRoot": scope.root_id,
        "safeRoot": scope.safe_root,
        "endpoint": endpoint,
        "updatedAt": current_timestamp_ms(),
    })
}

fn write_owner_only_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    std::fs::write(
        path,
        serde_json::to_vec(value).map_err(|error| format!("Could not encode JSON: {error}"))?,
    )
    .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_project_browser_scope(key: &str) -> Option<(ProjectBrowserScope, String)> {
    if key.len() != 16 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let state_dir = crate::app_paths::unpeel_home()
        .join("browser")
        .join("projects")
        .join(key);
    let value: Value =
        serde_json::from_slice(&std::fs::read(state_dir.join("browser.json")).ok()?).ok()?;
    if value.get("schema").and_then(Value::as_u64) != Some(PROJECT_BROWSER_RECORD_SCHEMA) {
        return None;
    }
    let root_id = value.get("projectRoot")?.as_str()?.to_string();
    let safe_root = value.get("safeRoot")?.as_str()?.to_string();
    if root_id.is_empty()
        || safe_root.is_empty()
        || !safe_root
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    let endpoint = value.get("endpoint")?.as_str()?.to_string();
    validate_project_cdp_endpoint(&endpoint).ok()?;
    let browser_root = crate::app_paths::unpeel_home().join("browser");
    Some((
        ProjectBrowserScope {
            root_id,
            safe_root: safe_root.clone(),
            key: key.to_string(),
            profile_dir: browser_root.join("profiles").join(&safe_root),
            state_dir,
            owner_session_key: format!("unpeel-project-{key}"),
            state_name: format!("unpeel-proj-{safe_root}"),
        },
        endpoint,
    ))
}

fn mark_project_browser_member(
    scope: &ProjectBrowserScope,
    session_id: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(scope.members_dir()).map_err(|error| {
        format!(
            "Could not create project browser membership state at {}: {error}",
            scope.members_dir().display()
        )
    })?;
    write_owner_only_json(
        &scope.member_path(session_id),
        &json!({
            "sessionId": session_id,
            "reservedAt": current_timestamp_ms(),
        }),
    )
}

fn project_owner_command(
    binary: &Path,
    scope: &ProjectBrowserScope,
    options: &BrowserOptions,
    args: &[String],
) -> Command {
    let _ = std::fs::create_dir_all(scope.downloads_dir());
    let mut command = Command::new(binary);
    command
        .args(args)
        .env("AGENT_BROWSER_NATIVE", "1")
        .env("AGENT_BROWSER_SESSION", &scope.owner_session_key)
        .env("AGENT_BROWSER_SOCKET_DIR", engine_socket_dir())
        .env("AGENT_BROWSER_PROFILE", &scope.profile_dir)
        .env("AGENT_BROWSER_SESSION_NAME", &scope.state_name)
        .env("AGENT_BROWSER_DOWNLOAD_PATH", scope.downloads_dir())
        .env(
            "AGENT_BROWSER_IDLE_TIMEOUT_MS",
            PROJECT_BROWSER_IDLE_TIMEOUT_MS.to_string(),
        )
        .env(
            "AGENT_BROWSER_MAX_OUTPUT",
            ENGINE_OUTPUT_MAX_CHARS.to_string(),
        )
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(key) = ensure_state_encryption_key() {
        command.env("AGENT_BROWSER_ENCRYPTION_KEY", key);
    }
    if options.settings.headed {
        command.env("AGENT_BROWSER_HEADED", "1");
    }
    let executable = options.settings.executable_path.trim();
    if !executable.is_empty() {
        command.env("AGENT_BROWSER_EXECUTABLE_PATH", executable);
    }
    if let Some(theme) = &options.theme {
        command.env("AGENT_BROWSER_COLOR_SCHEME", theme);
    }
    command
}

fn exec_project_owner(
    binary: &Path,
    scope: &ProjectBrowserScope,
    options: &BrowserOptions,
    args: &[String],
) -> Result<String, String> {
    let command = project_owner_command(binary, scope, options, args);
    run_engine_process(command, args, ENGINE_TIMEOUT_MS, None)
}

fn ensure_project_browser(
    binary: &Path,
    session_id: &str,
    scope: ProjectBrowserScope,
    options: &BrowserOptions,
) -> Result<ProjectCdpBinding, String> {
    std::fs::create_dir_all(&scope.profile_dir).map_err(|error| {
        format!(
            "Could not create the project browser profile at {}: {error}",
            scope.profile_dir.display()
        )
    })?;
    let _lock = acquire_project_browser_lock(&scope)?;
    reap_orphaned_project_chrome(&scope);
    clear_stale_engine_state(&format!("project-{}", scope.key), Some(&scope.profile_dir));
    let output = exec_project_owner(binary, &scope, options, &["get".into(), "cdp-url".into()])?;
    let endpoint = output
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ws://"))
        .ok_or_else(|| "The project browser did not report a CDP endpoint.".to_string())?
        .to_string();
    validate_project_cdp_endpoint(&endpoint).map_err(|reason| {
        format!("The project browser reported an unsafe CDP endpoint: {reason}.")
    })?;
    write_owner_only_json(
        &scope.record_path(),
        &project_browser_record(&scope, &endpoint),
    )?;
    mark_project_browser_member(&scope, session_id)?;
    Ok(ProjectCdpBinding { endpoint, scope })
}

fn resolve_cdp_binding(
    binary: &Path,
    session_id: &str,
    options: &BrowserOptions,
) -> Result<Option<CdpBinding>, String> {
    if let Some(remote) = load_remote_cdp_binding()? {
        if !options.settings.allowed_domains.trim().is_empty() {
            return Err(
                "Site access rules cannot be combined with a provider-owned CDP browser in the \
current browser engine. Clear Settings > Browser > Allowed sites or remove the Host's \
remote browser binding."
                    .to_string(),
            );
        }
        return Ok(Some(CdpBinding::Remote(remote)));
    }
    if options.settings.allowed_domains.trim().is_empty() {
        if let Some(scope) = options.project_scope(session_id) {
            return ensure_project_browser(binary, session_id, scope, options)
                .map(CdpBinding::Project)
                .map(Some);
        }
    }
    Ok(None)
}

/// True when `pid` names a live process (same liveness probe the session host
/// uses). EPERM (alive, different owner) still counts as alive.
fn pid_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Remove engine state left behind by a crashed browser Chrome/daemon, but
/// ONLY when the owning process is dead — a live browser in a sibling session
/// keeps its socket and profile lock. Two kinds of stale state:
///
/// 1. This session's daemon `.pid`/`.sock` when the daemon pid is gone.
/// 2. Chrome's `SingletonLock` (+ `SingletonSocket`/`SingletonCookie`) in the
///    persistent project profile — a stale one makes Chrome exit at launch.
///    The lock is a symlink to `<hostname>-<pid>`; a dead (or unparseable) pid
///    means it's stale.
fn clear_stale_engine_state(session_id: &str, profile_dir: Option<&Path>) {
    let key = engine_session_key(session_id);
    let socket_dir = engine_socket_dir();
    let pid_path = socket_dir.join(format!("{key}.pid"));
    if let Some(pid) = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|raw| raw.trim().parse::<i32>().ok())
    {
        if !pid_is_alive(pid) {
            let _ = std::fs::remove_file(&pid_path);
            let _ = std::fs::remove_file(socket_dir.join(format!("{key}.sock")));
            trace(&format!(
                "cleared stale daemon socket session={session_id} pid={pid}"
            ));
        }
    }

    let Some(dir) = profile_dir else { return };
    let lock = dir.join("SingletonLock");
    let Ok(target) = std::fs::read_link(&lock) else {
        return;
    };
    let stale = target
        .to_str()
        .and_then(|value| value.rsplit('-').next())
        .and_then(|pid| pid.parse::<i32>().ok())
        .map(|pid| !pid_is_alive(pid))
        .unwrap_or(true);
    if stale {
        for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        trace(&format!(
            "cleared stale Chrome singleton lock session={session_id} target={}",
            target.display()
        ));
    }
}

/// Pid recorded in `dir`'s Chrome `SingletonLock` (a symlink whose target is
/// `<hostname>-<pid>`), if the link exists and parses.
fn singleton_lock_pid(dir: &Path) -> Option<i32> {
    let target = std::fs::read_link(dir.join("SingletonLock")).ok()?;
    target
        .to_str()
        .and_then(|value| value.rsplit('-').next())
        .and_then(|pid| pid.parse::<i32>().ok())
}

fn remove_singleton_files(dir: &Path) {
    for name in ["SingletonLock", "SingletonSocket", "SingletonCookie"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// This session's engine daemon pid, only when that process is still alive.
fn live_daemon_pid(session_id: &str) -> Option<i32> {
    live_engine_daemon_pid(&engine_session_key(session_id))
}

fn live_engine_daemon_pid(engine_key: &str) -> Option<i32> {
    std::fs::read_to_string(engine_socket_dir().join(format!("{engine_key}.pid")))
        .ok()
        .and_then(|raw| raw.trim().parse::<i32>().ok())
        .filter(|pid| pid_is_alive(*pid))
}

fn remove_engine_sidecars(engine_key: &str) {
    for suffix in [
        "sock", "pid", "binding", "target", "config", "version", "stream", "engine",
    ] {
        let _ = std::fs::remove_file(engine_socket_dir().join(format!("{engine_key}.{suffix}")));
    }
}

fn binding_from_existing_marker(marker: &str) -> Option<CdpBinding> {
    if let Some(key) = project_key_from_binding_fingerprint(marker) {
        let (scope, endpoint) = read_project_browser_scope(key)?;
        let binding = CdpBinding::Project(ProjectCdpBinding { endpoint, scope });
        return (engine_binding_fingerprint(Some(&binding)) == marker).then_some(binding);
    }
    if marker.starts_with("remote:") {
        let binding = CdpBinding::Remote(load_remote_cdp_binding().ok()??);
        return (engine_binding_fingerprint(Some(&binding)) == marker).then_some(binding);
    }
    None
}

fn session_cleanup_command(
    binary: &Path,
    session_id: &str,
    options: &BrowserOptions,
    binding: Option<&CdpBinding>,
    args: &[String],
) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env("AGENT_BROWSER_NATIVE", "1")
        .env("AGENT_BROWSER_SESSION", engine_session_key(session_id))
        .env("AGENT_BROWSER_SOCKET_DIR", engine_socket_dir())
        .env(
            "AGENT_BROWSER_MAX_OUTPUT",
            ENGINE_OUTPUT_MAX_CHARS.to_string(),
        )
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_cdp_binding(&mut command, binding);
    if let Some(theme) = &options.theme {
        command.env("AGENT_BROWSER_COLOR_SCHEME", theme);
    }
    command
}

fn project_has_live_members(scope: &ProjectBrowserScope) -> bool {
    let now = current_timestamp_ms();
    let Ok(entries) = std::fs::read_dir(scope.members_dir()) else {
        return false;
    };
    let mut live = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let value = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok());
        let session_id = value
            .as_ref()
            .and_then(|value| value.get("sessionId"))
            .and_then(Value::as_str);
        let reserved_at = value
            .as_ref()
            .and_then(|value| value.get("reservedAt"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let recently_reserved =
            reserved_at > 0 && now.saturating_sub(reserved_at) <= PROJECT_MEMBER_START_GRACE_MS;
        let hosted_session_running = session_id.is_some_and(|id| {
            session_host::load_manifest(id)
                .is_some_and(|manifest| manifest.state == session_host::HostedSessionState::Running)
        });
        if session_id.is_some_and(|id| live_daemon_pid(id).is_some())
            || hosted_session_running
            || recently_reserved
        {
            live = true;
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
    live
}

fn release_project_browser_member(
    binary: &Path,
    session_id: &str,
    project_key: &str,
    options: &BrowserOptions,
) {
    let Some((scope, _)) = read_project_browser_scope(project_key) else {
        return;
    };
    let Ok(_lock) = acquire_project_browser_lock(&scope) else {
        return;
    };
    let _ = std::fs::remove_file(scope.member_path(session_id));
    if project_has_live_members(&scope) {
        return;
    }

    if live_engine_daemon_pid(&scope.owner_session_key).is_some() {
        let args = ["close".to_string()];
        let command = project_owner_command(binary, &scope, options, &args);
        let result = run_engine_process(command, &args, CLEANUP_TIMEOUT_MS, None);
        trace(&format!(
            "project-browser close project={} result={}",
            scope.key,
            match result {
                Ok(_) => "ok".to_string(),
                Err(error) => format!("err {}", compact_one_line(&error)),
            }
        ));
    }
    remove_engine_sidecars(&scope.owner_session_key);
    let _ = std::fs::remove_file(scope.record_path());
}

fn close_session_browser(binary: &Path, session_id: &str) -> Result<String, String> {
    let key = engine_session_key(session_id);
    let marker = std::fs::read_to_string(engine_binding_marker_path(session_id))
        .ok()
        .map(|raw| raw.trim().to_string());
    let binding = marker.as_deref().and_then(binding_from_existing_marker);
    let options = load_options();
    let daemon_alive = live_engine_daemon_pid(&key).is_some();
    let pinned_target_exists =
        binding.is_some() && engine_socket_dir().join(format!("{key}.target")).is_file();
    let mut close_result = Ok("ok".to_string());

    if daemon_alive || pinned_target_exists {
        // Attached sessions do not own the shared Chrome process. Close their
        // pinned page explicitly before shutting down only their small daemon;
        // plain `agent-browser close` intentionally leaves an attached tab.
        if binding.is_some() {
            let args = ["tab".to_string(), "close".to_string()];
            let command =
                session_cleanup_command(binary, session_id, &options, binding.as_ref(), &args);
            let tab_result = run_engine_process(
                command,
                &args,
                CLEANUP_TIMEOUT_MS,
                binding.as_ref().and_then(|binding| match binding {
                    CdpBinding::Remote(remote) => Some(remote.endpoint.as_str()),
                    CdpBinding::Project(_) => None,
                }),
            );
            trace(&format!(
                "cleanup session={session_id} tab-close={}",
                match tab_result {
                    Ok(_) => "ok".to_string(),
                    Err(error) => format!("err {}", compact_one_line(&error)),
                }
            ));
        }

        let args = ["close".to_string()];
        let command =
            session_cleanup_command(binary, session_id, &options, binding.as_ref(), &args);
        close_result = run_engine_process(
            command,
            &args,
            CLEANUP_TIMEOUT_MS,
            binding.as_ref().and_then(|binding| match binding {
                CdpBinding::Remote(remote) => Some(remote.endpoint.as_str()),
                CdpBinding::Project(_) => None,
            }),
        );
    }

    remove_engine_sidecars(&key);
    if let Some(project_key) = marker
        .as_deref()
        .and_then(project_key_from_binding_fingerprint)
    {
        release_project_browser_member(binary, session_id, project_key, &options);
    }
    close_result
}

/// Full command line of a live process, used as identity proof before any
/// kill: under agent load pids recycle in under an hour, so a recorded pid is
/// never trusted without its argv still matching (see `pid_started_at` in the
/// session host for the same discipline).
fn pid_command_line(pid: i32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!line.is_empty()).then_some(line)
}

fn parent_pid(pid: i32) -> Option<i32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Recover a project profile when its owner daemon crashed but left Chrome
/// behind. The SingletonLock pid is never trusted alone: the process must
/// still advertise this exact dedicated user-data-dir, and a live recorded or
/// parent agent-browser daemon always wins. This mirrors the session-host rule
/// that a recycled bare pid is never enough authority to signal a process.
fn reap_orphaned_project_chrome(scope: &ProjectBrowserScope) {
    let Some(chrome_pid) = singleton_lock_pid(&scope.profile_dir) else {
        return;
    };
    if !pid_is_alive(chrome_pid) {
        remove_singleton_files(&scope.profile_dir);
        return;
    }
    if live_engine_daemon_pid(&scope.owner_session_key).is_some() {
        return;
    }
    if parent_pid(chrome_pid)
        .and_then(pid_command_line)
        .is_some_and(|command| command.contains("agent-browser"))
    {
        return;
    }
    let profile = scope.profile_dir.to_string_lossy();
    let verified = pid_command_line(chrome_pid).is_some_and(|command| {
        command.contains("--user-data-dir") && command.contains(profile.as_ref())
    });
    if !verified {
        trace(&format!(
            "left ambiguous project browser owner untouched project={} pid={chrome_pid}",
            scope.key
        ));
        return;
    }

    unsafe { libc::kill(chrome_pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_millis(1000);
    while pid_is_alive(chrome_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if pid_is_alive(chrome_pid) {
        unsafe { libc::kill(chrome_pid, libc::SIGKILL) };
        std::thread::sleep(Duration::from_millis(100));
    }
    if !pid_is_alive(chrome_pid) {
        remove_singleton_files(&scope.profile_dir);
        trace(&format!(
            "reaped orphaned project browser project={} pid={chrome_pid}",
            scope.key
        ));
    }
}

fn pid_has_children(pid: i32) -> bool {
    Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Ensure a session daemon is bound to the current local/shared/remote mode.
/// CDP endpoints can rotate while a Host remains alive. agent-browser daemons
/// retain their initial connection, so a changed endpoint must replace a
/// browserless daemon before the next command. A daemon whose actual browser
/// child is still live is never killed implicitly; the user must close it.
/// Returns true when an obsolete daemon was removed.
fn reconcile_engine_binding(
    session_id: &str,
    binding: Option<&CdpBinding>,
) -> Result<bool, String> {
    let marker = engine_binding_marker_path(session_id);
    let wanted = engine_binding_fingerprint(binding);
    let current = std::fs::read_to_string(&marker)
        .ok()
        .map(|raw| raw.trim().to_string());
    if current.as_deref() == Some(wanted.as_str()) {
        return Ok(false);
    }

    // Upgrade compatibility: before binding markers existed every daemon was
    // local. Adopt that known state without disrupting an open browser.
    if current.is_none() && binding.is_none() {
        write_engine_binding_marker(&marker, &wanted)?;
        return Ok(false);
    }

    let current_project = current
        .as_deref()
        .and_then(project_key_from_binding_fingerprint);
    let wanted_project = binding
        .and_then(CdpBinding::project_scope)
        .map(|scope| scope.key.as_str());

    let mut replaced = false;
    if let Some(daemon) = live_daemon_pid(session_id) {
        if current_project.is_some() && current_project != wanted_project {
            return Err(
                "This session's browser sharing mode changed while its project tab is still \
open. Call browser_close once, then retry."
                    .to_string(),
            );
        }
        if pid_has_children(daemon) {
            return Err(
                "The browser connection mode changed while this session's local browser is still \
running. Call browser_close once, then retry."
                    .to_string(),
            );
        }
        if !pid_command_line(daemon).is_some_and(|command| command.contains("agent-browser")) {
            return Err(
                "The browser connection mode changed, but Unpeel could not verify the existing \
browser daemon's identity. Close the session before retrying."
                    .to_string(),
            );
        }
        unsafe { libc::kill(daemon, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_millis(1000);
        while pid_is_alive(daemon) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if pid_is_alive(daemon) {
            unsafe { libc::kill(daemon, libc::SIGKILL) };
            std::thread::sleep(Duration::from_millis(100));
        }
        replaced = true;
    }

    let key = engine_session_key(session_id);
    // `.target` is a CDP target id and is valid only inside the old browser
    // process. Removing all daemon-owned launch sidecars prevents a restarted
    // project browser from reviving a stale target or sticky configuration.
    for suffix in [
        "sock", "pid", "target", "config", "version", "stream", "engine",
    ] {
        let _ = std::fs::remove_file(engine_socket_dir().join(format!("{key}.{suffix}")));
    }
    write_engine_binding_marker(&marker, &wanted)?;
    Ok(replaced)
}

fn browser_artifacts_dir(session_id: &str) -> PathBuf {
    session_host::session_dir(session_id)
        .join("artifacts")
        .join("browser")
}

/// Run one engine CLI invocation for the calling session and return its
/// cleaned combined output.
fn exec_engine(args: &[String], timeout_ms: u64) -> Result<String, String> {
    let session_id = caller_session_id()?;
    let binary = resolve_engine_binary()?;
    exec_engine_with(&binary, &session_id, args, timeout_ms)
}

fn exec_engine_with(
    binary: &PathBuf,
    session_id: &str,
    args: &[String],
    timeout_ms: u64,
) -> Result<String, String> {
    let socket_dir = engine_socket_dir();
    let downloads_dir = browser_artifacts_dir(session_id).join("downloads");
    let _ = std::fs::create_dir_all(&socket_dir);
    let _ = std::fs::create_dir_all(&downloads_dir);

    let options = load_options();
    let cdp_binding = resolve_cdp_binding(binary, session_id, &options)?;
    let previous_project_key = std::fs::read_to_string(engine_binding_marker_path(session_id))
        .ok()
        .and_then(|raw| project_key_from_binding_fingerprint(raw.trim()).map(str::to_string));
    let wanted_project_key = cdp_binding
        .as_ref()
        .and_then(CdpBinding::project_scope)
        .map(|scope| scope.key.clone());

    // Self-heal before launch: a browser Chrome/daemon that died without a
    // clean shutdown leaves a stale daemon socket and/or a Chrome
    // `SingletonLock` in the persistent project profile, and the stale lock
    // makes the next Chrome exit instantly ("Chrome exited before providing
    // DevTools URL"). Clear whichever point at a dead process so a crash can't
    // wedge future browser_opens.
    clear_stale_engine_state(session_id, None);
    let binding_replaced = match reconcile_engine_binding(session_id, cdp_binding.as_ref()) {
        Ok(replaced) => replaced,
        Err(error) => {
            if let Some(scope) = cdp_binding.as_ref().and_then(CdpBinding::project_scope) {
                release_project_browser_member(binary, session_id, &scope.key, &options);
            }
            return Err(error);
        }
    };
    if previous_project_key != wanted_project_key {
        if let Some(project_key) = previous_project_key {
            release_project_browser_member(binary, session_id, &project_key, &options);
        }
    }
    // Rebinding already stopped the obsolete daemon. `close` should not start
    // a fresh connection solely to close it again.
    if binding_replaced && args == ["close"] {
        return Ok("ok".to_string());
    }

    let mut command = Command::new(binary);
    command
        .args(args)
        // The native CDP daemon needs no Node/Playwright/Chromium download and
        // drives the system Chrome; policy features (allowed domains) were
        // verified enforced in this mode.
        .env("AGENT_BROWSER_NATIVE", "1")
        .env("AGENT_BROWSER_SESSION", engine_session_key(session_id))
        .env("AGENT_BROWSER_SOCKET_DIR", &socket_dir)
        // Aligned with our own tool-output truncation so the engine trims at
        // the source instead of us throwing bytes away.
        .env(
            "AGENT_BROWSER_MAX_OUTPUT",
            ENGINE_OUTPUT_MAX_CHARS.to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env("NO_COLOR", "1");
    // agent-browser supports a CDP URL or loopback port through this
    // launch-time environment variable. This keeps any bearer token out of
    // argv and the per-session binding marker; credentialed engine output is
    // redacted below.
    apply_cdp_binding(&mut command, cdp_binding.as_ref());

    // Local launch options do not apply to a provider-owned browser. The
    // provider decides whether it is headed and owns its executable and
    // profile; agent-browser only attaches to the configured CDP endpoint.
    if cdp_binding.is_none() {
        command.env("AGENT_BROWSER_DOWNLOAD_PATH", &downloads_dir);
        // A visible browser matches Unpeel's "watch your agent work" model and
        // is the default; Settings ▸ Browser can switch to background.
        if options.settings.headed {
            command.env("AGENT_BROWSER_HEADED", "1");
        }
    }
    // Site rules — enforced by the engine for navigation, sub-resources, and
    // WebSockets (verified in native mode).
    let allowed = options.settings.allowed_domains.trim();
    if !allowed.is_empty() {
        command.env("AGENT_BROWSER_ALLOWED_DOMAINS", allowed);
    }
    // Custom browser executable (Brave/Edge/Chromium). Empty = auto-detect.
    let executable = options.settings.executable_path.trim();
    if cdp_binding.is_none() && !executable.is_empty() {
        command.env("AGENT_BROWSER_EXECUTABLE_PATH", executable);
    }
    // Follow Unpeel's appearance so pages render in the mode the user works
    // in; "system" leaves the engine default.
    if let Some(theme) = &options.theme {
        command.env("AGENT_BROWSER_COLOR_SCHEME", theme);
    }

    run_engine_process(
        command,
        args,
        timeout_ms,
        cdp_binding.as_ref().and_then(|binding| match binding {
            CdpBinding::Remote(remote) => Some(remote.endpoint.as_str()),
            CdpBinding::Project(_) => None,
        }),
    )
}

fn run_engine_process(
    mut command: Command,
    args: &[String],
    timeout_ms: u64,
    redacted_endpoint: Option<&str>,
) -> Result<String, String> {
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to launch the browser engine: {e}"))?;

    // Drain the pipes on threads so a chatty command can't deadlock against a
    // full pipe buffer while we poll for exit.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_string(&mut buffer);
        }
        buffer
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_string(&mut buffer);
        }
        buffer
    });

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "The browser engine did not respond within {}s (command: {}). The \
browser may still be starting; try again, or browser_close and retry.",
                        timeout_ms / 1000,
                        args.join(" ")
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Failed to wait for the browser engine: {e}")),
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let mut combined = strip_ansi(&stdout);
    let stderr_clean = strip_ansi(&stderr);
    if !stderr_clean.trim().is_empty() {
        if !combined.trim().is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr_clean.trim_end());
    }
    let combined = match redacted_endpoint {
        Some(endpoint) if endpoint.starts_with("wss://") => {
            combined.replace(endpoint, "<remote-cdp-url>")
        }
        _ => combined,
    };
    let text = truncate_output(combined.trim());

    if status.success() {
        Ok(if text.is_empty() {
            "ok".to_string()
        } else {
            text
        })
    } else if text.is_empty() {
        Err(format!(
            "Browser engine command failed (exit {}).",
            status.code().unwrap_or(-1)
        ))
    } else {
        Err(text)
    }
}

/// Per-install key for encrypting the engine's saved login state (64 hex
/// chars = AES-256, the engine's expected format). Created on first use at
/// `~/.unpeel/browser/state-key` (0600), same pattern as the MCP auth token.
/// None only when the key can neither be read nor created — the engine then
/// falls back to plaintext state, which still works, just unencrypted.
fn ensure_state_encryption_key() -> Option<String> {
    let path = crate::app_paths::unpeel_home()
        .join("browser")
        .join("state-key");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(trimmed.to_string());
        }
    }
    let key = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let parent = path.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    std::fs::write(&path, format!("{key}\n")).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Some(key)
}

fn truncate_output(text: &str) -> String {
    if text.chars().count() <= ENGINE_OUTPUT_MAX_CHARS {
        return text.to_string();
    }
    let truncated: String = text.chars().take(ENGINE_OUTPUT_MAX_CHARS).collect();
    format!("{truncated}\n… output truncated")
}

fn compact_one_line(text: &str) -> String {
    let mut line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 200 {
        line = line.chars().take(200).collect::<String>() + "…";
    }
    line
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Missing required string argument '{key}'"))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn tool_open(args: &Value) -> Result<String, String> {
    let url = required_str(args, "url")?;
    exec_engine(&["open".into(), url.into()], ENGINE_TIMEOUT_MS)
}

fn tool_snapshot(args: &Value) -> Result<String, String> {
    let mut argv = vec!["snapshot".to_string()];
    if bool_arg(args, "interactive", true) {
        argv.push("-i".into());
    }
    if bool_arg(args, "compact", true) {
        argv.push("-c".into());
    }
    exec_engine(&argv, ENGINE_TIMEOUT_MS)
}

fn tool_click(args: &Value) -> Result<String, String> {
    let target = required_str(args, "target")?;
    maybe_show_cursor(target);
    exec_engine(&["click".into(), target.into()], ENGINE_TIMEOUT_MS)
}

fn tool_fill(args: &Value) -> Result<String, String> {
    let target = required_str(args, "target")?;
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .ok_or("Missing required string argument 'text'")?;
    maybe_show_cursor(target);
    exec_engine(
        &["fill".into(), target.into(), text.into()],
        ENGINE_TIMEOUT_MS,
    )
}

fn tool_type(args: &Value) -> Result<String, String> {
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or("Missing required string argument 'text'")?;
    match optional_str(args, "target") {
        Some(target) => {
            maybe_show_cursor(target);
            exec_engine(
                &["type".into(), target.into(), text.into()],
                ENGINE_TIMEOUT_MS,
            )
        }
        None => exec_engine(
            &["keyboard".into(), "type".into(), text.into()],
            ENGINE_TIMEOUT_MS,
        ),
    }
}

/// How long the injected cursor's CSS transition takes; the action fires just
/// after it lands so the pointer visibly arrives before the click.
const CURSOR_TRAVEL_MS: u64 = 280;
/// Bound for the two extra engine round-trips (box lookup + overlay eval) —
/// short so a slow page can't stall the real action behind eye candy.
const CURSOR_STEP_TIMEOUT_MS: u64 = 8_000;

/// Animate a visible pointer to the action target ("Show agent cursor").
/// Purely cosmetic and strictly best-effort: any failure (element without a
/// box, page mid-navigation, eval refused) silently skips straight to the
/// real action. Runs only for headed windows — headless would pay latency for
/// pixels nobody sees. The overlay lives in the page as a fixed,
/// pointer-events:none element that navigation naturally clears.
fn maybe_show_cursor(target: &str) {
    let options = load_options();
    if !options.settings.headed || !options.settings.show_cursor {
        return;
    }
    let Ok(session_id) = caller_session_id() else {
        return;
    };
    let Ok(binary) = resolve_engine_binary() else {
        return;
    };
    let Ok(box_output) = exec_engine_with(
        &binary,
        &session_id,
        &["get".into(), "box".into(), target.into(), "--json".into()],
        CURSOR_STEP_TIMEOUT_MS,
    ) else {
        return;
    };
    // `--json` output: {"success":true,"data":{"x":..,"y":..,"width":..,"height":..}}
    let Some(json_start) = box_output.find('{') else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(box_output[json_start..].trim()) else {
        return;
    };
    let data = &parsed["data"];
    let (Some(x), Some(y), Some(w), Some(h)) = (
        data["x"].as_f64(),
        data["y"].as_f64(),
        data["width"].as_f64(),
        data["height"].as_f64(),
    ) else {
        return;
    };
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;

    // Idempotent overlay: first call creates the arrow (spawning near the
    // top-left), later calls glide it; a click ripple fires as it arrives.
    let travel = CURSOR_TRAVEL_MS;
    let js = format!(
        r##"(()=>{{let c=document.getElementById('__unpeel_cursor');if(!c){{c=document.createElement('div');c.id='__unpeel_cursor';c.style.cssText='position:fixed;left:24px;top:24px;z-index:2147483647;pointer-events:none;transition:left {travel}ms cubic-bezier(.25,.7,.35,1),top {travel}ms cubic-bezier(.25,.7,.35,1);filter:drop-shadow(0 1px 2px rgba(0,0,0,.45))';c.innerHTML='<svg width="18" height="18" viewBox="0 0 18 18"><path d="M2 1l13 7.5-5.5 1.3L6.5 16z" fill="#fff" stroke="#111" stroke-width="1.1"/></svg>';document.documentElement.appendChild(c);}}c.getBoundingClientRect();c.style.left='{cx}px';c.style.top='{cy}px';setTimeout(()=>{{let r=document.createElement('div');r.style.cssText='position:fixed;left:{cx}px;top:{cy}px;width:6px;height:6px;margin:-3px;border-radius:50%;border:2px solid rgba(59,130,246,.9);z-index:2147483646;pointer-events:none;opacity:.9;transition:transform .35s ease-out,opacity .35s ease-out';document.documentElement.appendChild(r);requestAnimationFrame(()=>{{r.style.transform='scale(5)';r.style.opacity='0';}});setTimeout(()=>r.remove(),400);}},{travel});}})()"##,
    );
    if exec_engine_with(
        &binary,
        &session_id,
        &["eval".into(), js],
        CURSOR_STEP_TIMEOUT_MS,
    )
    .is_ok()
    {
        // Let the glide land (plus a beat for the ripple) before the action.
        std::thread::sleep(Duration::from_millis(CURSOR_TRAVEL_MS + 80));
    }
}

fn tool_press(args: &Value) -> Result<String, String> {
    let key = required_str(args, "key")?;
    exec_engine(&["press".into(), key.into()], ENGINE_TIMEOUT_MS)
}

const GET_KINDS: &[&str] = &["text", "html", "value", "url", "title", "count"];

fn tool_get(args: &Value) -> Result<String, String> {
    let what = required_str(args, "what")?.to_ascii_lowercase();
    if !GET_KINDS.contains(&what.as_str()) {
        return Err(format!(
            "Unsupported 'what' value '{what}'. Use one of: {}.",
            GET_KINDS.join(", ")
        ));
    }
    let mut argv = vec!["get".to_string(), what.clone()];
    if let Some(target) = optional_str(args, "target") {
        argv.push(target.into());
    } else if matches!(what.as_str(), "text" | "html" | "value" | "count") {
        return Err(format!("'{what}' requires a 'target' (ref or selector)."));
    }
    exec_engine(&argv, ENGINE_TIMEOUT_MS)
}

fn tool_screenshot(args: &Value) -> Result<String, String> {
    let session_id = caller_session_id()?;
    let add_to_gallery = args
        .get("gallery")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| load_options().auto_add_screenshots_to_gallery);
    let dir = browser_artifacts_dir(&session_id).join(if add_to_gallery {
        "screenshots"
    } else {
        "captures"
    });
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create screenshot dir {}: {e}", dir.display()))?;
    let path = dir.join(format!("shot-{}.png", current_timestamp_ms()));
    let path_str = path.to_string_lossy().to_string();

    let mut argv = vec!["screenshot".to_string(), path_str.clone()];
    if bool_arg(args, "full", false) {
        argv.push("--full".into());
    }
    if bool_arg(args, "annotate", false) {
        argv.push("--annotate".into());
    }
    let output = exec_engine(&argv, ENGINE_TIMEOUT_MS)?;
    if path.is_file() {
        Ok(format!("Saved screenshot to {path_str}"))
    } else {
        // The engine reported success but the file is missing — surface both.
        Ok(output)
    }
}

const LOAD_STATES: &[&str] = &["load", "domcontentloaded", "networkidle"];

fn tool_wait(args: &Value) -> Result<String, String> {
    if let Some(selector) = optional_str(args, "selector") {
        return exec_engine(&["wait".into(), selector.into()], ENGINE_TIMEOUT_MS);
    }
    if let Some(state) = optional_str(args, "load") {
        let state = state.to_ascii_lowercase();
        if !LOAD_STATES.contains(&state.as_str()) {
            return Err(format!(
                "Unsupported 'load' value '{state}'. Use one of: {}.",
                LOAD_STATES.join(", ")
            ));
        }
        return exec_engine(&["wait".into(), "--load".into(), state], ENGINE_TIMEOUT_MS);
    }
    if let Some(ms) = args.get("ms").and_then(Value::as_u64) {
        let ms = ms.min(WAIT_MS_MAX);
        return exec_engine(&["wait".into(), ms.to_string()], ENGINE_TIMEOUT_MS);
    }
    Err(
        "browser_wait needs one of: 'selector' (wait for element), 'load' (load state), or 'ms' \
(fixed delay)."
            .into(),
    )
}

const SCROLL_DIRECTIONS: &[&str] = &["up", "down", "left", "right"];

fn tool_scroll(args: &Value) -> Result<String, String> {
    if let Some(target) = optional_str(args, "into_view") {
        return exec_engine(&["scrollintoview".into(), target.into()], ENGINE_TIMEOUT_MS);
    }
    let direction = required_str(args, "direction")?.to_ascii_lowercase();
    if !SCROLL_DIRECTIONS.contains(&direction.as_str()) {
        return Err(format!(
            "Unsupported 'direction' value '{direction}'. Use one of: {}.",
            SCROLL_DIRECTIONS.join(", ")
        ));
    }
    let mut argv = vec!["scroll".to_string(), direction];
    if let Some(pixels) = args.get("pixels").and_then(Value::as_u64) {
        argv.push(pixels.to_string());
    }
    exec_engine(&argv, ENGINE_TIMEOUT_MS)
}

fn tool_console(args: &Value) -> Result<String, String> {
    let mut argv = vec!["console".to_string()];
    if bool_arg(args, "clear", false) {
        argv.push("--clear".into());
    }
    exec_engine(&argv, ENGINE_TIMEOUT_MS)
}

fn tool_close(_args: &Value) -> Result<String, String> {
    let session_id = caller_session_id()?;
    let binary = resolve_engine_binary()?;
    close_session_browser(&binary, &session_id)
}

pub(crate) fn tool_browser_context() -> Result<String, String> {
    let session_id = self_session_id();
    let access = match &session_id {
        Some(id) => load_security().effective_access(id),
        None => BrowserAccess::Off,
    };
    let engine = resolve_engine_binary();
    let mut lines = vec![format!(
        "browser_access: {}",
        match (&session_id, access) {
            (None, _) => "unknown caller (no UNPEEL_SESSION_ID)".to_string(),
            (Some(_), BrowserAccess::On) => "on".to_string(),
            (Some(id), BrowserAccess::Ask) => {
                if load_security().approvals.iter().any(|entry| entry == id) {
                    "ask — approved for this session (remembered)".to_string()
                } else {
                    "ask — the first browser action will ask the user for approval".to_string()
                }
            }
            (Some(_), BrowserAccess::Off) =>
                "off — the user can enable it in Settings ▸ Browser".to_string(),
        }
    )];
    match &engine {
        Ok(path) => lines.push(format!("engine: agent-browser at {}", path.display())),
        Err(error) => lines.push(format!("engine: unavailable — {error}")),
    }
    let options = load_options();
    let remote_cdp = load_remote_cdp_binding();
    let allowed = options.settings.allowed_domains.trim();
    let project_scope = match (&session_id, &remote_cdp) {
        (Some(id), Ok(None)) if allowed.is_empty() => options.project_scope(id),
        _ => None,
    };
    match (&remote_cdp, &project_scope) {
        (Ok(Some(binding)), _) => lines.push(format!(
            "engine_mode: native agent-browser daemon attached to the {} provider-owned browser \
{}",
            binding.provider,
            if binding.endpoint.starts_with("wss://") {
                "over authenticated WSS CDP (endpoint credential redacted)"
            } else {
                "through a Host-loopback CDP port"
            }
        )),
        (Ok(None), Some(_)) => lines.push(format!(
            "engine_mode: one shared project browser window with a pinned tab for this session, \
{}",
            if options.settings.headed {
                "visible"
            } else {
                "background (headless)"
            }
        )),
        (Ok(None), None) => lines.push(format!(
            "engine_mode: isolated native CDP browser driving {}, {}",
            if options.settings.executable_path.trim().is_empty() {
                "system Chrome/Chromium".to_string()
            } else {
                options.settings.executable_path.trim().to_string()
            },
            if options.settings.headed {
                "visible window"
            } else {
                "background (headless)"
            }
        )),
        (Err(error), _) => lines.push(format!(
            "engine_mode: unavailable — invalid remote CDP configuration ({error})"
        )),
    }
    lines.push(
        "video: not available in this engine mode (recording silently produces no file) — \
capture browser_screenshot at each meaningful step instead."
            .into(),
    );
    if !allowed.is_empty() {
        lines.push(format!(
            "site_rules: navigation restricted to {allowed} (engine-enforced)"
        ));
    }
    if let Some(id) = &session_id {
        lines.push(format!("engine_session: {}", engine_session_key(id)));
        lines.push(format!(
            "artifact_dir: {}",
            browser_artifacts_dir(id).display()
        ));
        lines.push(
            match (
                &remote_cdp,
                &project_scope,
                options.auto_add_screenshots_to_gallery,
            ) {
                (Ok(Some(_)), _, true) => {
                    "screenshots save under artifact_dir/screenshots and appear \
in the Session gallery; downloads are owned by the remote browser service."
                        .into()
                }
                (Ok(Some(_)), _, false) => {
                    "screenshots save under artifact_dir/captures and stay out \
of the Session gallery unless screenshot is called with gallery=true or Sessions add_to_gallery \
publishes the file; downloads are owned by the remote browser service."
                        .into()
                }
                (Ok(None), Some(scope), true) => format!(
                    "screenshots save under artifact_dir/screenshots and appear in the Session \
gallery; project downloads save under {}.",
                    scope.downloads_dir().display()
                ),
                (Ok(None), Some(scope), false) => format!(
                    "screenshots save under artifact_dir/captures and stay out of the Session \
gallery unless screenshot is called with gallery=true or Sessions add_to_gallery publishes the \
file; project downloads save under {}.",
                    scope.downloads_dir().display()
                ),
                (_, _, true) => "screenshots save under artifact_dir/screenshots and appear in \
the Session gallery; downloads save under artifact_dir/downloads."
                    .into(),
                (_, _, false) => "screenshots save under artifact_dir/captures and stay out of \
the Session gallery unless screenshot is called with gallery=true or Sessions add_to_gallery \
publishes the file; downloads save under artifact_dir/downloads."
                    .into(),
            },
        );
        match (&remote_cdp, &project_scope) {
            (Ok(Some(binding)), _) => lines.push(format!(
                "browsing data: owned by the {} remote browser service; this Unpeel session \
has its own pinned tab and agent-browser control daemon, while the provider defines browser \
profile and project isolation.",
                binding.provider
            )),
            (Err(_), _) => lines.push(
                "browsing data: unavailable until the remote CDP configuration is repaired.".into(),
            ),
            (Ok(None), Some(scope)) => lines.push(format!(
                "browsing data: one Unpeel-managed window and profile per project tree at {}. \
Every session gets its own pinned tab; cookies and logins are shared across the project, never \
with the user's personal browser or another project.",
                scope.profile_dir.display()
            )),
            (Ok(None), None) if options.settings.profile_mode.trim() == "project" => lines.push(
                "browsing data: configured as kept-per-project, but running fresh-per-session \
right now because site access rules are set (current engine limitation — the two can't \
combine)."
                    .into(),
            ),
            (Ok(None), None) => lines.push(
                "browsing data: fresh per session — cookies and logins vanish when the \
browser closes."
                    .into(),
            ),
        }
    }
    lines.push(
        "The browser never shares the user's own cookies or logins. Safety: never paste \
cookies, tokens, passwords, or private downloaded files into the conversation unless the user \
explicitly asks."
            .into(),
    );
    lines.push(
        "core loop: browser_open → browser_snapshot (refs like @e1) → browser_click/browser_fill \
by ref → re-snapshot after navigation or DOM changes."
            .into(),
    );
    Ok(lines.join("\n"))
}

pub(crate) fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "browser_open",
            "description": "Navigate this session's browser to a URL (launches the browser on \
        first use). Follow with browser_snapshot to see the page and get element refs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to open (https://…, http://localhost:…, or a bare domain)" },
                },
                "required": ["url"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_snapshot",
            "description": "Accessibility snapshot of the current page with stable element refs \
        (@e1, @e2, …) for browser_click/browser_fill. Call again after navigation or DOM \
        changes — refs go stale. Defaults to interactive elements only, compacted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "interactive": { "type": "boolean", "description": "Only interactive elements (default true; false = full tree)" },
                    "compact": { "type": "boolean", "description": "Remove empty structural nodes (default true)" },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_click",
            "description": "Click an element by snapshot ref (@e1) or CSS selector. Prefer refs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Snapshot ref (@e1) or CSS selector" },
                },
                "required": ["target"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_fill",
            "description": "Clear an input and fill it with text, by snapshot ref or CSS selector.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Snapshot ref (@e1) or CSS selector" },
                    "text": { "type": "string", "description": "Text to fill" },
                },
                "required": ["target", "text"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_type",
            "description": "Type text with real keystrokes. With 'target', types into that \
        element (without clearing it); without, types into the focused element — useful for \
        editors and key-driven UIs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Text to type" },
                    "target": { "type": "string", "description": "Optional snapshot ref or CSS selector" },
                },
                "required": ["text"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_press",
            "description": "Press a key or combination, e.g. Enter, Tab, Escape, Control+a.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Key name or combination (Enter, Tab, Control+a)" },
                },
                "required": ["key"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_get",
            "description": "Read page state: 'url' or 'title' for the page, or 'text', 'html', \
        'value', 'count' for an element (requires target).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "what": { "type": "string", "enum": ["text", "html", "value", "url", "title", "count"], "description": "What to read" },
                    "target": { "type": "string", "description": "Snapshot ref or CSS selector (required for element reads)" },
                },
                "required": ["what"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_screenshot",
            "description": "Capture the current page into this session's artifact folder and \
        return the file path. Use annotate for a labeled screenshot that matches snapshot refs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "full": { "type": "boolean", "description": "Full page instead of viewport (default false)" },
                    "annotate": { "type": "boolean", "description": "Numbered labels for interactive elements (default false)" },
                    "gallery": { "type": "boolean", "description": "Add to the Session gallery; omit to use the Sessions use setting" },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_wait",
            "description": "Wait for a selector to appear, a load state, or a fixed delay. Use \
        after actions that trigger navigation or slow loads, before re-snapshotting.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "Wait until this CSS selector exists" },
                    "load": { "type": "string", "enum": ["load", "domcontentloaded", "networkidle"], "description": "Wait for this load state" },
                    "ms": { "type": "integer", "description": "Fixed delay in milliseconds (max 30000)" },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_scroll",
            "description": "Scroll the page in a direction, or scroll an element into view.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "direction": { "type": "string", "enum": ["up", "down", "left", "right"], "description": "Scroll direction" },
                    "pixels": { "type": "integer", "description": "Scroll distance in pixels (engine default if omitted)" },
                    "into_view": { "type": "string", "description": "Instead: scroll this ref/selector into view" },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_console",
            "description": "Read the page's console log (errors and messages) — check this when \
        a page misbehaves after an action.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clear": { "type": "boolean", "description": "Clear the log after reading (default false)" },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_close",
            "description": "Close this session's browser tab and its control daemon. Other \
        sessions' tabs in a shared project browser stay open; the project window closes after its \
        last session tab closes.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "browser_context",
            "description": "Current Browser MCP configuration for this session: access state, \
        engine availability, artifact folder, and usage rules. Call this first if browser tools \
        seem unavailable or you need the artifact path.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
    ]
}

fn trace(message: &str) {
    let path = crate::app_paths::unpeel_home()
        .join("hooks")
        .join("trace.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{} browser-mcp {}", current_timestamp_ms(), message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_state(path: &Path, value: &Value) {
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn browser_security_missing_state_keeps_shipped_default_on() {
        let dir = tempfile::tempdir().unwrap();
        let security = load_security_at(&dir.path().join("missing-app-state.json"));
        assert_eq!(security.effective_access("session"), BrowserAccess::On);
    }

    #[test]
    fn browser_security_absent_default_is_on_and_valid_values_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app-state.json");

        write_state(&path, &json!({}));
        assert_eq!(
            load_security_at(&path).effective_access("session"),
            BrowserAccess::On
        );

        for (raw, expected) in [
            ("on", BrowserAccess::On),
            ("allow", BrowserAccess::On),
            ("ask", BrowserAccess::Ask),
            ("off", BrowserAccess::Off),
        ] {
            write_state(&path, &json!({ "browser_default_access": raw }));
            assert_eq!(
                load_security_at(&path).effective_access("session"),
                expected,
                "persisted value {raw:?}"
            );
        }
    }

    #[test]
    fn browser_security_explicit_invalid_defaults_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app-state.json");

        for value in [json!("future-mode"), json!(42), json!(null), json!([])] {
            write_state(&path, &json!({ "browser_default_access": value }));
            assert_eq!(
                load_security_at(&path).effective_access("session"),
                BrowserAccess::Off,
                "explicit malformed value must not behave like an absent field"
            );
        }
    }

    #[test]
    fn browser_security_malformed_session_override_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app-state.json");
        write_state(
            &path,
            &json!({
                "browser_default_access": "on",
                "browser_access": {
                    "malformed": { "future": true },
                    "unknown": "future-mode",
                    "allowed": "allow",
                    "prompted": "ask",
                    "blocked": "off"
                }
            }),
        );

        let security = load_security_at(&path);
        assert_eq!(security.effective_access("malformed"), BrowserAccess::Off);
        assert_eq!(security.effective_access("unknown"), BrowserAccess::Off);
        assert_eq!(security.effective_access("allowed"), BrowserAccess::On);
        assert_eq!(security.effective_access("prompted"), BrowserAccess::Ask);
        assert_eq!(security.effective_access("blocked"), BrowserAccess::Off);
        assert_eq!(security.effective_access("absent"), BrowserAccess::On);
    }

    #[test]
    fn browser_security_unreadable_corrupt_and_non_object_state_fail_closed() {
        let dir = tempfile::tempdir().unwrap();

        // Reading a directory as a file is a portable read failure and avoids
        // chmod-based tests that behave differently under privileged runners.
        assert_eq!(
            load_security_at(dir.path()).effective_access("session"),
            BrowserAccess::Off
        );

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{ definitely not JSON").unwrap();
        assert_eq!(
            load_security_at(&corrupt).effective_access("session"),
            BrowserAccess::Off
        );

        let truncated = dir.path().join("truncated.json");
        std::fs::write(&truncated, b"   \n").unwrap();
        assert_eq!(
            load_security_at(&truncated).effective_access("session"),
            BrowserAccess::Off
        );

        let non_object = dir.path().join("non-object.json");
        std::fs::write(&non_object, b"[]").unwrap();
        assert_eq!(
            load_security_at(&non_object).effective_access("session"),
            BrowserAccess::Off
        );
    }

    #[test]
    fn tool_definitions_have_valid_schemas() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 13);
        for tool in &tools {
            assert!(tool.get("name").and_then(Value::as_str).is_some());
            assert!(tool.get("description").and_then(Value::as_str).is_some());
            let schema = tool.get("inputSchema").expect("inputSchema");
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
        }
    }

    #[test]
    fn get_requires_target_for_element_reads() {
        let error = tool_get(&json!({ "what": "text" })).unwrap_err();
        assert!(error.contains("target"));
        let error = tool_get(&json!({ "what": "bogus" })).unwrap_err();
        assert!(error.contains("Unsupported"));
    }

    #[test]
    fn wait_requires_exactly_one_mode() {
        let error = tool_wait(&json!({})).unwrap_err();
        assert!(error.contains("selector"));
        let error = tool_wait(&json!({ "load": "bogus" })).unwrap_err();
        assert!(error.contains("Unsupported"));
    }

    #[test]
    fn scroll_validates_direction() {
        let error = tool_scroll(&json!({ "direction": "sideways" })).unwrap_err();
        assert!(error.contains("Unsupported"));
        let error = tool_scroll(&json!({})).unwrap_err();
        assert!(error.contains("direction"));
    }

    #[test]
    fn engine_session_key_is_prefixed() {
        assert_eq!(engine_session_key("abc-123"), "unpeel-abc-123");
    }

    #[test]
    fn remote_cdp_config_accepts_owner_only_authenticated_wss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-cdp.json");
        std::fs::write(
            &path,
            br#"{"schema":1,"endpoint":"wss://browser.example/cdp?token=secret","provider":"upstash"}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let binding = load_remote_cdp_binding_at(&path).unwrap().unwrap();
        assert_eq!(binding.provider, "upstash");
        assert_eq!(binding.endpoint, "wss://browser.example/cdp?token=secret");

        let cdp_binding = CdpBinding::Remote(binding.clone());
        let mut command = Command::new("agent-browser");
        apply_cdp_binding(&mut command, Some(&cdp_binding));
        let configured = command
            .get_envs()
            .find(|(key, _)| *key == REMOTE_CDP_ENGINE_ENV)
            .and_then(|(_, value)| value)
            .and_then(|value| value.to_str());
        assert_eq!(configured, Some(binding.endpoint.as_str()));
        assert_eq!(
            command.get_args().count(),
            0,
            "the secret must not enter argv"
        );
        assert_eq!(
            redact_remote_cdp_secret(
                format!("failed to connect to {}", binding.endpoint),
                Some(&cdp_binding)
            ),
            "failed to connect to <remote-cdp-url>"
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "AGENT_BROWSER_PIN_TAB")
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str()),
            Some("1")
        );
    }

    #[test]
    fn remote_cdp_config_accepts_bare_loopback_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-cdp.json");
        std::fs::write(
            &path,
            br#"{"schema":1,"endpoint":"9222","provider":"upstash"}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let binding = load_remote_cdp_binding_at(&path).unwrap().unwrap();
        assert_eq!(binding.endpoint, "9222");
        let cdp_binding = CdpBinding::Remote(binding);
        assert_eq!(
            redact_remote_cdp_secret("browser on port 9222".into(), Some(&cdp_binding)),
            "browser on port 9222"
        );
    }

    #[test]
    fn remote_cdp_config_fails_closed_for_insecure_or_malformed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-cdp.json");
        let write_owner_only = |value: &Value| {
            std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        };

        write_owner_only(&json!({
            "schema": 1,
            "endpoint": "ws://browser.example/cdp?token=secret",
            "provider": "upstash"
        }));
        assert!(load_remote_cdp_binding_at(&path)
            .unwrap_err()
            .contains("loopback port"));

        write_owner_only(&json!({
            "schema": 1,
            "endpoint": "0",
            "provider": "upstash"
        }));
        assert!(load_remote_cdp_binding_at(&path)
            .unwrap_err()
            .contains("between 1 and 65535"));

        write_owner_only(&json!({
            "schema": 2,
            "endpoint": "wss://browser.example/cdp?token=secret"
        }));
        assert!(load_remote_cdp_binding_at(&path)
            .unwrap_err()
            .contains("unsupported schema"));

        write_owner_only(&json!({
            "schema": 1,
            "endpoint": "wss://browser.example/cdp?token=secret",
            "unexpected": true
        }));
        assert!(load_remote_cdp_binding_at(&path)
            .unwrap_err()
            .contains("unknown field"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_cdp_config_rejects_non_private_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        std::fs::write(
            &target,
            br#"{"schema":1,"endpoint":"wss://browser.example/cdp?token=secret"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_remote_cdp_binding_at(&target)
            .unwrap_err()
            .contains("chmod 600"));

        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("remote-cdp.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(load_remote_cdp_binding_at(&link)
            .unwrap_err()
            .contains("not a symlink"));
    }

    #[test]
    fn engine_binding_fingerprint_never_contains_the_remote_credential() {
        let binding = RemoteCdpBinding {
            endpoint: "wss://browser.example/cdp?token=top-secret".into(),
            provider: "upstash".into(),
        };
        let binding = CdpBinding::Remote(binding);
        let fingerprint = engine_binding_fingerprint(Some(&binding));
        assert!(fingerprint.starts_with("remote:"));
        assert!(!fingerprint.contains("top-secret"));
        assert_eq!(engine_binding_fingerprint(None), "local");
    }

    #[test]
    fn project_binding_fingerprint_names_scope_without_exposing_endpoint() {
        let scope = ProjectBrowserScope {
            root_id: "project-one".into(),
            safe_root: "project-one".into(),
            key: "0123456789abcdef".into(),
            profile_dir: PathBuf::from("/tmp/profile"),
            state_dir: PathBuf::from("/tmp/state"),
            owner_session_key: "unpeel-project-0123456789abcdef".into(),
            state_name: "unpeel-proj-project-one".into(),
        };
        let binding = CdpBinding::Project(ProjectCdpBinding {
            endpoint: "ws://127.0.0.1:9222/devtools/browser/private-id".into(),
            scope,
        });
        let fingerprint = engine_binding_fingerprint(Some(&binding));
        assert!(fingerprint.starts_with("project:0123456789abcdef:"));
        assert!(!fingerprint.contains("9222"));
        assert!(!fingerprint.contains("private-id"));
        assert_eq!(
            project_key_from_binding_fingerprint(&fingerprint),
            Some("0123456789abcdef")
        );
    }

    #[test]
    fn project_cdp_endpoint_accepts_only_loopback_websockets() {
        for endpoint in [
            "ws://127.0.0.1:9222/devtools/browser/id",
            "ws://localhost:49152/devtools/browser/id",
            "ws://[::1]:49152/devtools/browser/id",
        ] {
            assert_eq!(validate_project_cdp_endpoint(endpoint), Ok(()));
        }
        for endpoint in [
            "wss://browser.example/cdp",
            "ws://192.168.1.10:9222/devtools/browser/id",
            "ws://127.0.0.1:0/devtools/browser/id",
            "ws://127.0.0.1:not-a-port/devtools/browser/id",
            "ws://127.0.0.1:9222/with space",
        ] {
            assert!(validate_project_cdp_endpoint(endpoint).is_err());
        }
    }

    #[test]
    fn truncate_output_caps_long_text() {
        let long = "x".repeat(ENGINE_OUTPUT_MAX_CHARS + 10);
        let truncated = truncate_output(&long);
        assert!(truncated.ends_with("… output truncated"));
        assert!(truncate_output("short") == "short");
    }

    #[test]
    fn singleton_lock_pid_parses_chrome_lock_symlink() {
        let dir = std::env::temp_dir().join(format!("unpeel-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(singleton_lock_pid(&dir), None);
        std::os::unix::fs::symlink("mac.lan-12345", dir.join("SingletonLock")).unwrap();
        assert_eq!(singleton_lock_pid(&dir), Some(12345));
        remove_singleton_files(&dir);
        assert_eq!(singleton_lock_pid(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
