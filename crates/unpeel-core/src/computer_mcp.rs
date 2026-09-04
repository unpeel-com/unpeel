//! Unpeel Computer MCP domain: the `computer` action tool on the unified
//! `unpeel` server (`mcp_host.rs`), giving an agent session control of the
//! Host desktop — window screenshots + accessibility-tree maps, background
//! clicking/typing, app/window control — through the embedded **cua-driver**
//! engine (github.com/trycua/cua, `libs/cua-driver`).
//!
//! Design doc: `unpeel-apple:docs/feature/computer-mcp.md` (engine switched from Peekaboo
//! to cua-driver 2026-07-22, pre-release — the action surface was reshaped to
//! cua-driver's window-first model at the same time; there is no legacy
//! surface to keep compatible).
//!
//! Engine model (differs from the browser engine's one-shot CLI):
//!
//! - On macOS the **native app** owns a long-lived daemon: it spawns
//!   `cua-driver serve --embedded --socket <home>/computer/daemon.sock` as a
//!   direct child (`ComputerEngineManager.swift`), so TCC attributes
//!   Accessibility/Screen Recording to Unpeel.app itself — the documented
//!   embedded contract (`Skills/cua-driver/EMBEDDING.md`). The daemon runs
//!   in `unrestricted` mode via the explicit two-env contract because Unpeel
//!   owns the approval UX (the Off/Ask/Allow gate below).
//! - On Linux, the canonical `unpeel serve` Host owns the same embedded
//!   daemon. It only advertises the adapter when `cua-driver` is installed
//!   and the Host process can see an X11 or Wayland graphical session.
//! - This module makes **one-shot CLI calls** against that socket:
//!   `cua-driver call <tool> '<json>' --socket <path>`. The server builds
//!   the tool-arg JSON itself from a per-action whitelist, so agents can
//!   never smuggle policy-overriding fields (session ids, debug outputs,
//!   screenshot destinations).
//! - Control is **background by design**: element actions dispatch through
//!   the accessibility rung and pixel actions use per-pid event posting — the
//!   user's real cursor never moves (an overlay cursor glides instead), and
//!   focus is not stolen unless the agent explicitly escalates delivery.
//!
//! Each Unpeel session maps to one cua-driver session (`unpeel-<id>`,
//! `capture_scope: auto`): its own overlay cursor, and the window→desktop
//! escalation ladder is per session — desktop-scope perception/input needs an
//! explicit `escalate` action, so agents work window-scoped until they can
//! justify the visible desktop.
//!
//! Screenshots land in `<session>/artifacts/computer/screenshots/` via the
//! engine's `--screenshot-out-file` (stdout stays base64-free) and tools
//! return the file path — never inline image bytes — so the desktop and phone
//! galleries pick them up like browser captures.

use crate::mcp_host::{app_request_with_timeout, self_session_id, strip_ansi};
use crate::session_host;
use crate::state::{current_timestamp_ms, ComputerAccess, ComputerSettings};
use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Engine calls can include a capture + full AX walk; give them the same
/// room as browser calls.
const ENGINE_TIMEOUT_MS: u64 = 60_000;
const ENGINE_OUTPUT_MAX_CHARS: usize = 32_000;
/// Approval prompt round-trip budget; must stay inside the HookServer's 150s
/// per-request ceiling for approval routes.
const APPROVAL_TIMEOUT_SECS: u64 = 130;

/// `unpeel-host __computer_cleanup__ <session-id>`: end the session's
/// cua-driver session (cursor + scope state). Spawned by the native app's
/// kill/cleanup path, mirroring `__browser_cleanup__`.
pub const COMPUTER_CLEANUP_ARG: &str = "__computer_cleanup__";

pub(crate) const COMPUTER_ACTIONS: &[&str] = &[
    "apps",
    "launch",
    "quit",
    "front",
    "windows",
    "see",
    "screenshot",
    "desktop",
    "click",
    "type",
    "set_value",
    "press",
    "hotkey",
    "scroll",
    "drag",
    "move_cursor",
    "escalate",
    "context",
];

pub(crate) fn is_computer_action(action: &str) -> bool {
    COMPUTER_ACTIONS.contains(&action)
}

/// Access state read from `app-state.json` per call (live gate).
struct ComputerSecurity {
    default_access: ComputerAccess,
    approvals: Vec<String>,
    settings: ComputerSettings,
}

fn load_security() -> ComputerSecurity {
    let path = crate::app_paths::unpeel_home().join("app-state.json");
    let value = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    // Absent field → serde default (Ask); explicit unknown value → Off.
    let default_access = match value
        .get("computer_default_access")
        // Minor-13 development builds briefly wrote this incorrect key from
        // the remote Settings panel. Read it as a migration fallback; every
        // current writer uses `computer_default_access`.
        .or_else(|| value.get("computer_access"))
        .and_then(Value::as_str)
    {
        None => ComputerAccess::default(),
        Some(raw) => ComputerAccess::from_state_str(raw),
    };
    let approvals = value
        .get("computer_approvals")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let settings = value
        .get("computer_settings")
        .cloned()
        .and_then(|raw| serde_json::from_value::<ComputerSettings>(raw).ok())
        .unwrap_or_default();
    ComputerSecurity {
        default_access,
        approvals,
        settings,
    }
}

fn caller_session_id() -> Result<String, String> {
    self_session_id().ok_or_else(|| {
        "The calling session is unknown, so Unpeel MCP can't authorize computer access. Run \
this from a hosted Unpeel session."
            .to_string()
    })
}

/// The per-call gate: Off refuses, Allow passes, Ask requires a remembered or
/// freshly-granted user approval for this session. `context` and `help` never
/// come through here.
pub(crate) fn caller_refusal_reason() -> Option<String> {
    let Some(session_id) = self_session_id() else {
        return Some(
            "The calling session is unknown, so Unpeel MCP can't authorize computer access. \
Run this from a hosted Unpeel session."
                .into(),
        );
    };
    if session_host::load_manifest(&session_id).is_none() {
        return Some(
            "The calling session has no Unpeel manifest, so Unpeel MCP can't \
authorize access. Run this from a hosted Unpeel session."
                .into(),
        );
    }
    let security = load_security();
    match security.default_access {
        ComputerAccess::Off => Some(
            "Computer access is off, so the computer tools are unavailable. The user can turn \
it on in Settings ▸ Computer."
                .into(),
        ),
        ComputerAccess::Allow => None,
        ComputerAccess::Ask => {
            if security.approvals.iter().any(|id| id == &session_id) {
                return None;
            }
            match request_computer_approval(&session_id) {
                Ok(true) => None,
                Ok(false) => Some(
                    "The user declined computer access for this session. Do not retry; ask \
the user to approve it in Settings ▸ Computer if they change their mind."
                        .into(),
                ),
                Err(error) => Some(format!(
                    "Computer access needs the user's approval, but the approval prompt \
could not be shown ({error}). Ask the user to open Unpeel and retry, or set Settings ▸ \
Computer to Allow."
                )),
            }
        }
    }
}

/// Blocking approval round-trip to the app (alert on the desktop). Approval
/// persistence (into `computer_approvals`) happens app-side on Allow.
fn request_computer_approval(session_id: &str) -> Result<bool, String> {
    let response = app_request_with_timeout(
        "/mcp/approve-computer",
        &json!({ "session_id": session_id }),
        Duration::from_secs(APPROVAL_TIMEOUT_SECS),
    )?;
    Ok(response.get("approved").and_then(Value::as_bool) == Some(true))
}

// ── Engine plumbing ──────────────────────────────────────────────────────

/// Engine resolution, shared with the worker and `unpeel computer install`
/// (`computer_engine::resolve`): `UNPEEL_CUA_DRIVER_BIN` → the verified
/// managed copy under `~/.unpeel/computer/bin` → next to `unpeel-host` (the
/// development app bundle) → PATH. A stale managed copy is skipped, not used.
pub fn resolve_engine_binary() -> Result<PathBuf, String> {
    crate::computer_engine::resolve(&crate::app_paths::unpeel_home())
}

/// cua-driver sends content-free product telemetry by default; Unpeel's
/// data boundary says nothing about a Host's activity leaves the user's
/// machines, so every engine process Unpeel starts opts out. The daemon
/// honors the same variable (see `unpeel-serve::computer` and the app's
/// `ComputerEngineManager`).
pub const TELEMETRY_OPT_OUT: (&str, &str) = ("CUA_DRIVER_RS_TELEMETRY_ENABLED", "0");

/// The app-owned embedded daemon's socket. Per `UNPEEL_HOME`, so workspace
/// instances get their own daemon (one screen, but per-home policy files).
pub fn daemon_socket_path() -> PathBuf {
    crate::app_paths::unpeel_home()
        .join("computer")
        .join("daemon.sock")
}

fn daemon_down_error() -> String {
    if cfg!(target_os = "linux") {
        "The computer-use engine is not running on this Host. Start `unpeel serve` from \
the graphical X11/Wayland session and check Settings ▸ Computer (Computer use must be \
enabled and access must not be Off). Retry after the Host reports the adapter as ready."
            .to_string()
    } else {
        "The computer-use engine is not running. Unpeel starts it while computer access is \
enabled — ask the user to open Unpeel and check Settings ▸ Computer (access must not be \
Off). Retry after the user confirms."
            .to_string()
    }
}

/// Canonical Computer access read shared by launch policy and the live MCP
/// gate. The misspelled minor-13 key is fallback-only (see `load_security`).
pub fn access_from_app_state(state: &Value) -> ComputerAccess {
    match state
        .get("computer_default_access")
        .or_else(|| state.get("computer_access"))
    {
        None => ComputerAccess::default(),
        Some(value) => value
            .as_str()
            .map(ComputerAccess::from_state_str)
            .unwrap_or(ComputerAccess::Off),
    }
}

/// Whether workspace policy asks the Host adapter to run. Read-only and
/// fail-closed for malformed explicit values.
pub fn requested_from_app_state(state: &Value) -> bool {
    let experimental_enabled = state
        .get("experimental_features")
        .and_then(|features| features.get("computer_use"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    experimental_enabled && access_from_app_state(state) != ComputerAccess::Off
}

/// New sessions only advertise the `computer` MCP domain after both policy
/// and the Host-owned daemon are present. A stale socket can still fail on
/// first call, but it can never cause the domain to be enabled while the
/// experiment or access policy is off.
pub fn enabled_for_launch_from_app_state(state: &Value) -> bool {
    requested_from_app_state(state) && daemon_is_ready()
}

/// Bounded, side-effect-free readiness probe used at the new-session launch
/// choke point. Socket existence alone is insufficient after a Host crash.
pub fn daemon_is_ready() -> bool {
    let Ok(binary) = resolve_engine_binary() else {
        return false;
    };
    let socket = daemon_socket_path();
    if !socket.exists() {
        return false;
    }
    let Ok(mut child) = Command::new(binary)
        .arg("status")
        .arg("--socket")
        .arg(socket)
        .env("CUA_DRIVER_EMBEDDED", "1")
        .env(TELEMETRY_OPT_OUT.0, TELEMETRY_OPT_OUT.1)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn screenshots_dir(session_id: &str) -> PathBuf {
    session_host::session_dir(session_id)
        .join("artifacts")
        .join("computer")
        .join("screenshots")
}

fn new_screenshot_path(session_id: &str, prefix: &str) -> PathBuf {
    screenshots_dir(session_id).join(format!("{prefix}-{}.png", current_timestamp_ms()))
}

/// One prepared engine invocation: a cua-driver tool name, the arg JSON the
/// server built (whitelisted, never raw agent input), and optionally where
/// screenshot bytes should land instead of stdout.
#[derive(Debug)]
struct EngineCall {
    tool: &'static str,
    args: Value,
    screenshot_out: Option<PathBuf>,
}

impl EngineCall {
    fn new(tool: &'static str, args: Value) -> Self {
        EngineCall {
            tool,
            args,
            screenshot_out: None,
        }
    }
}

/// Run one engine invocation for an agent action: attach the caller's
/// cua-driver session, exec, and rewrite daemon-down / missing-TCC failures
/// into guidance the agent can relay.
fn exec_engine(call: EngineCall) -> Result<String, String> {
    let session = cua_session_name()?;
    ensure_cua_session(&session)?;
    let mut args = call.args;
    if let Value::Object(ref mut map) = args {
        map.insert("session".into(), Value::String(session.clone()));
    }
    let result = exec_engine_unchecked(call.tool, &args, call.screenshot_out.as_deref());
    match result {
        Err(error) if is_session_scope_error(&error) => {
            // The daemon restarted or idle-reclaimed our session: re-declare
            // it once and retry, instead of surfacing a transient error.
            CUA_SESSION_STARTED.store(false, Ordering::SeqCst);
            ensure_cua_session(&session)?;
            exec_engine_unchecked(call.tool, &args, call.screenshot_out.as_deref())
                .map_err(augment_engine_error)
        }
        Err(error) => Err(augment_engine_error(error)),
        ok => ok,
    }
}

fn is_session_scope_error(error: &str) -> bool {
    error.contains("session_required")
        || error.contains("unknown session")
        || error.contains("capture_scope is immutable")
}

/// Each Unpeel session is one cua-driver session: stable id, own overlay
/// cursor, own scope ladder.
fn cua_session_name() -> Result<String, String> {
    Ok(format!("unpeel-{}", caller_session_id()?))
}

/// Declared once per server process (cheap re-declare after daemon restarts).
static CUA_SESSION_STARTED: AtomicBool = AtomicBool::new(false);

fn ensure_cua_session(session: &str) -> Result<(), String> {
    if CUA_SESSION_STARTED.load(Ordering::SeqCst) {
        return Ok(());
    }
    let args = json!({ "session": session, "capture_scope": "auto" });
    match exec_engine_unchecked("start_session", &args, None) {
        Ok(_) => {
            CUA_SESSION_STARTED.store(true, Ordering::SeqCst);
            Ok(())
        }
        // An earlier declare (same scope) surviving in the daemon is fine.
        Err(error) if error.contains("already uses capture_scope='auto'") => {
            CUA_SESSION_STARTED.store(true, Ordering::SeqCst);
            Ok(())
        }
        Err(error) => Err(augment_engine_error(error)),
    }
}

/// Spawn `cua-driver call <tool> '<json>' --socket <path>` and return its
/// cleaned combined output. `CUA_DRIVER_EMBEDDED=1` keeps every code path in
/// embedded behavior (no LaunchServices daemon auto-launch, ever).
fn exec_engine_unchecked(
    tool: &str,
    args: &Value,
    screenshot_out: Option<&std::path::Path>,
) -> Result<String, String> {
    let binary = resolve_engine_binary()?;
    let socket = daemon_socket_path();
    if !socket.exists() {
        return Err(daemon_down_error());
    }
    let mut command = Command::new(&binary);
    command
        .arg("call")
        .arg(tool)
        .arg(args.to_string())
        .arg("--socket")
        .arg(&socket)
        .env("CUA_DRIVER_EMBEDDED", "1")
        .env(TELEMETRY_OPT_OUT.0, TELEMETRY_OPT_OUT.1)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = screenshot_out {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        command.arg("--screenshot-out-file").arg(path);
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to launch the computer-use engine: {e}"))?;

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

    let deadline = Instant::now() + Duration::from_millis(ENGINE_TIMEOUT_MS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "The computer-use engine did not respond within {}s (tool: {tool}).",
                        ENGINE_TIMEOUT_MS / 1000,
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Failed to wait for the computer-use engine: {e}")),
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
    let text = truncate_output(&strip_inline_screenshot(combined.trim()));

    if status.success() {
        Ok(if text.is_empty() { "ok".into() } else { text })
    } else if text.contains("daemon is not running") || text.contains("daemon request on") {
        Err(daemon_down_error())
    } else if text.is_empty() {
        Err(format!(
            "Computer-use engine command failed (exit {}).",
            status.code().unwrap_or(-1)
        ))
    } else {
        Err(text)
    }
}

/// Belt-and-braces: even though `--screenshot-out-file` keeps stdout
/// base64-free, never let an inline `screenshot_png_b64` blob reach the
/// agent's context if one slips through (e.g. a call made without an out
/// file against a tool that captures anyway).
fn strip_inline_screenshot(output: &str) -> String {
    if !output.contains("screenshot_png_b64") {
        return output.to_string();
    }
    let mut stream = serde_json::Deserializer::from_str(output.trim_start()).into_iter::<Value>();
    if let Some(Ok(mut value)) = stream.next() {
        if let Value::Object(ref mut map) = value {
            map.remove("screenshot_png_b64");
            map.remove("screenshot_mime_type");
            return serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
        }
    }
    output.to_string()
}

fn truncate_output(text: &str) -> String {
    if text.chars().count() <= ENGINE_OUTPUT_MAX_CHARS {
        return text.to_string();
    }
    let truncated: String = text.chars().take(ENGINE_OUTPUT_MAX_CHARS).collect();
    format!("{truncated}\n… (output truncated — pass 'query' on see to filter the element tree)")
}

// ── platform permission/runtime help ────────────────────────────────────

/// `check_permissions` through the embedded daemon → (accessibility,
/// screen_recording), or None when the probe/parse fails.
fn probe_permissions() -> Option<(bool, bool)> {
    let output = exec_engine_unchecked("check_permissions", &json!({}), None).ok()?;
    let value = serde_json::Deserializer::from_str(output.trim_start())
        .into_iter::<Value>()
        .next()?
        .ok()?;
    Some((
        value.get("accessibility").and_then(Value::as_bool)?,
        value.get("screen_recording").and_then(Value::as_bool)?,
    ))
}

fn missing_permission_names(probe: (bool, bool)) -> Vec<String> {
    let mut missing = Vec::new();
    if !probe.0 {
        missing.push("Accessibility".to_string());
    }
    if !probe.1 {
        missing.push("Screen Recording".to_string());
    }
    missing
}

/// When an engine action fails and required TCC grants are missing, that is
/// almost certainly why. Replace the raw failure with guidance the agent can
/// relay, and ask the desktop app to show the user a grant prompt.
fn augment_engine_error(error: String) -> String {
    if error == daemon_down_error() {
        return error;
    }
    if cfg!(target_os = "linux") {
        return format!(
            "{error}\n\nOn Linux, run `cua-driver doctor --json` in the Host's graphical \
session and verify DISPLAY or WAYLAND_DISPLAY, the session D-Bus address, and AT-SPI. \
Then restart `unpeel serve` and retry."
        );
    }
    let missing = match probe_permissions() {
        Some(probe) => missing_permission_names(probe),
        None => Vec::new(),
    };
    if missing.is_empty() {
        return error;
    }
    let nudged = notify_app_permissions_needed(&missing);
    format!(
        "{error}\n\nLikely cause: Unpeel is missing required macOS permissions for computer \
use: {list}. Every computer action will fail until the user grants them in System Settings ▸ \
Privacy & Security (Unpeel ▸ Settings ▸ Computer shows live status with grant buttons).{shown} \
Tell the user what is needed; after they grant, retry — no app restart is required.",
        list = missing.join(", "),
        shown = if nudged {
            " Unpeel has shown the user a grant prompt on the desktop."
        } else {
            ""
        }
    )
}

/// Fire-and-forget desktop nudge: the app shows a one-time alert with
/// grant buttons (`/mcp/computer-permissions-needed`, deduplicated
/// app-side). Returns whether the app acknowledged it.
fn notify_app_permissions_needed(missing: &[String]) -> bool {
    app_request_with_timeout(
        "/mcp/computer-permissions-needed",
        &json!({ "session_id": self_session_id(), "missing": missing }),
        Duration::from_secs(5),
    )
    .is_ok()
}

// ── App allowlist (guardrail, not a sandbox) ─────────────────────────────

fn check_app_allowed(settings: &ComputerSettings, app: &str) -> Result<(), String> {
    let raw = settings.allowed_apps.trim();
    if raw.is_empty() {
        return Ok(());
    }
    let wanted = app.trim().to_ascii_lowercase();
    let allowed = raw
        .split([',', ' '])
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .any(|entry| entry == wanted);
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "The app '{app}' is not in the computer-use app allowlist (Settings ▸ Computer). \
Allowed: {raw}."
        ))
    }
}

/// Best-effort pid guardrail: when an allowlist is configured, resolve the
/// pid through `list_apps` and check its name/bundle id. Fails closed when
/// the pid can't be resolved — an allowlist that can't identify the target
/// must not wave it through.
fn check_pid_allowed(settings: &ComputerSettings, pid: i64) -> Result<(), String> {
    if settings.allowed_apps.trim().is_empty() {
        return Ok(());
    }
    let output = exec_engine_unchecked("list_apps", &json!({}), None)
        .map_err(|e| format!("App allowlist check could not list running apps: {e}"))?;
    let value = serde_json::Deserializer::from_str(output.trim_start())
        .into_iter::<Value>()
        .next()
        .and_then(Result::ok)
        .ok_or("App allowlist check could not parse the running-app list.")?;
    let apps = value
        .get("apps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let entry = apps
        .iter()
        .find(|app| app.get("pid").and_then(Value::as_i64) == Some(pid));
    let Some(entry) = entry else {
        return Err(format!(
            "pid {pid} is not a running app the allowlist can identify; target apps via \
'launch' or 'apps' first."
        ));
    };
    let name = entry.get("name").and_then(Value::as_str).unwrap_or("");
    let bundle = entry.get("bundle_id").and_then(Value::as_str).unwrap_or("");
    if check_app_allowed(settings, name).is_ok() || check_app_allowed(settings, bundle).is_ok() {
        Ok(())
    } else {
        Err(format!(
            "The app '{name}' (pid {pid}) is not in the computer-use app allowlist \
(Settings ▸ Computer). Allowed: {}.",
            settings.allowed_apps.trim()
        ))
    }
}

// ── Arg plumbing ─────────────────────────────────────────────────────────

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Missing required parameter: {key}"))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_int(args: &Value, key: &str) -> Result<i64, String> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("Missing required parameter: {key} (integer)"))
}

fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Copy the whitelisted keys that are present from agent args into the
/// engine-arg object — the policy boundary: everything else the agent sent
/// is dropped on the floor.
fn copy_keys(source: &Value, keys: &[&str], target: &mut Map<String, Value>) {
    for key in keys {
        if let Some(value) = source.get(*key) {
            if !value.is_null() {
                target.insert((*key).to_string(), value.clone());
            }
        }
    }
}

/// Modifier keys: accept an array of strings or a comma/plus-separated
/// string ("cmd,shift" / "cmd+shift").
fn normalize_key_list(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        Value::String(raw) => Some(
            raw.split([',', '+'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

/// The shared pointer/keyboard target fields (`pid`, `window_id`,
/// `element_index`, `element_token`, `x`, `y`, `scope`, `delivery_mode`)
/// plus per-action extras.
fn input_args(args: &Value, extras: &[&str]) -> Map<String, Value> {
    let mut map = Map::new();
    copy_keys(
        args,
        &[
            "pid",
            "window_id",
            "element_index",
            "element_token",
            "x",
            "y",
            "scope",
            "delivery_mode",
        ],
        &mut map,
    );
    copy_keys(args, extras, &mut map);
    if let Some(modifiers) = args.get("modifiers").and_then(normalize_key_list) {
        if !modifiers.is_empty() {
            // click/drag call it `modifier`; press_key calls it `modifiers`.
            // Insert both — each tool's schema-validated side picks its own
            // and additionalProperties never reaches the daemon (the CLI
            // sanitizes reserved args; unknown keys are tool-side errors),
            // so the dispatcher below removes the wrong one per tool.
            map.insert("modifier".into(), json!(modifiers));
        }
    }
    map
}

// ── Action dispatch ──────────────────────────────────────────────────────

/// Dispatch one computer action (gate already passed for everything except
/// `context`, which is always callable).
pub(crate) fn run_action(action: &str, args: &Value) -> Result<String, String> {
    let settings = load_security().settings;
    let call = build_action(action, args, &settings)?;
    // Best-effort pid allowlist guardrail on every pid-targeting action.
    if let Some(pid) = call.args.get("pid").and_then(Value::as_i64) {
        check_pid_allowed(&settings, pid)?;
    }
    let artifact = call.screenshot_out.clone();
    let output = exec_engine(call)?;
    Ok(match artifact {
        Some(path) if path.is_file() => {
            format!("Saved screenshot to {}\n{output}", path.display())
        }
        _ => output,
    })
}

/// Build the engine invocation for one action. Pure (no engine exec), so the
/// arg mapping is unit-testable; allowlist checks that need the engine run
/// in `run_action`.
fn build_action(
    action: &str,
    args: &Value,
    settings: &ComputerSettings,
) -> Result<EngineCall, String> {
    match action {
        "apps" => Ok(EngineCall::new("list_apps", json!({}))),
        "launch" => build_launch(args, settings),
        "quit" => {
            let pid = required_int(args, "pid")?;
            Ok(EngineCall::new("kill_app", json!({ "pid": pid })))
        }
        "front" => {
            let pid = required_int(args, "pid")?;
            let mut map = Map::new();
            map.insert("pid".into(), json!(pid));
            copy_keys(args, &["window_id"], &mut map);
            Ok(EngineCall::new("bring_to_front", Value::Object(map)))
        }
        "windows" => {
            let pid = required_int(args, "pid")?;
            Ok(EngineCall::new("list_windows", json!({ "pid": pid })))
        }
        "see" => build_see(args),
        "screenshot" => build_screenshot(args),
        "desktop" => build_desktop(),
        "click" => build_click(args),
        "type" => {
            let text = required_str(args, "text")?;
            let mut map = input_args(args, &["delay_ms"]);
            map.remove("modifier");
            map.insert("text".into(), json!(text));
            Ok(EngineCall::new("type_text", Value::Object(map)))
        }
        "set_value" => {
            let value = optional_str(args, "value")
                .or_else(|| optional_str(args, "text"))
                .ok_or("set_value needs 'value' (the control's new value).")?;
            let pid = required_int(args, "pid")?;
            let mut map = input_args(args, &[]);
            map.remove("modifier");
            map.insert("pid".into(), json!(pid));
            map.insert("value".into(), json!(value));
            Ok(EngineCall::new("set_value", Value::Object(map)))
        }
        "press" => {
            let key = required_str(args, "key")?;
            let mut map = input_args(args, &[]);
            if let Some(modifiers) = map.remove("modifier") {
                map.insert("modifiers".into(), modifiers);
            }
            map.insert("key".into(), json!(key));
            Ok(EngineCall::new("press_key", Value::Object(map)))
        }
        "hotkey" => {
            let keys = args
                .get("keys")
                .and_then(normalize_key_list)
                .filter(|keys| !keys.is_empty())
                .ok_or(
                    "hotkey needs 'keys' (e.g. [\"cmd\",\"shift\",\"t\"] or \"cmd,shift,t\").",
                )?;
            let mut map = input_args(args, &[]);
            map.remove("modifier");
            map.remove("element_index");
            map.remove("element_token");
            map.insert("keys".into(), json!(keys));
            Ok(EngineCall::new("hotkey", Value::Object(map)))
        }
        "scroll" => {
            let direction = required_str(args, "direction")?;
            if !matches!(direction, "up" | "down" | "left" | "right") {
                return Err("scroll direction must be up, down, left, or right.".into());
            }
            let mut map = input_args(args, &["by", "amount"]);
            map.remove("modifier");
            map.insert("direction".into(), json!(direction));
            Ok(EngineCall::new("scroll", Value::Object(map)))
        }
        "drag" => {
            let mut map = input_args(
                args,
                &[
                    "from_x",
                    "from_y",
                    "to_x",
                    "to_y",
                    "duration_ms",
                    "steps",
                    "button",
                ],
            );
            map.remove("element_index");
            map.remove("element_token");
            map.remove("x");
            map.remove("y");
            for key in ["from_x", "from_y", "to_x", "to_y"] {
                if !map.contains_key(key) {
                    return Err(
                        "drag needs from_x, from_y, to_x, to_y (window-local screenshot pixels)."
                            .into(),
                    );
                }
            }
            Ok(EngineCall::new("drag", Value::Object(map)))
        }
        "move_cursor" => {
            let mut map = Map::new();
            copy_keys(args, &["x", "y"], &mut map);
            if !map.contains_key("x") || !map.contains_key("y") {
                return Err("move_cursor needs 'x' and 'y'.".into());
            }
            Ok(EngineCall::new("move_cursor", Value::Object(map)))
        }
        "escalate" => {
            let reason = required_str(args, "reason")?;
            let mut map = Map::new();
            map.insert("reason".into(), json!(reason));
            copy_keys(args, &["detail"], &mut map);
            Ok(EngineCall::new("escalate_session", Value::Object(map)))
        }
        _ => Err(format!("Unknown computer action: {action}")),
    }
}

fn build_launch(args: &Value, settings: &ComputerSettings) -> Result<EngineCall, String> {
    let bundle_id = optional_str(args, "bundle_id");
    let name = optional_str(args, "app").or_else(|| optional_str(args, "name"));
    let mut map = Map::new();
    match (bundle_id, name) {
        (Some(bundle), _) => {
            check_app_allowed(settings, bundle)?;
            map.insert("bundle_id".into(), json!(bundle));
        }
        (None, Some(name)) => {
            check_app_allowed(settings, name)?;
            map.insert("name".into(), json!(name));
        }
        (None, None) => return Err("launch needs 'app' (application name) or 'bundle_id'.".into()),
    }
    if let Some(urls) = args.get("urls").and_then(Value::as_array) {
        map.insert("urls".into(), json!(urls));
    }
    if bool_arg(args, "new_instance", false) {
        map.insert("creates_new_application_instance".into(), json!(true));
    }
    Ok(EngineCall::new("launch_app", Value::Object(map)))
}

fn build_see(args: &Value) -> Result<EngineCall, String> {
    let pid = required_int(args, "pid")?;
    let window_id = required_int(args, "window_id")?;
    let with_screenshot = bool_arg(args, "screenshot", true);
    let mut map = Map::new();
    map.insert("pid".into(), json!(pid));
    map.insert("window_id".into(), json!(window_id));
    map.insert("include_screenshot".into(), json!(with_screenshot));
    copy_keys(args, &["query"], &mut map);
    let mut call = EngineCall::new("get_window_state", Value::Object(map));
    if with_screenshot {
        call.screenshot_out = Some(new_screenshot_path(&caller_session_id()?, "see"));
    }
    Ok(call)
}

/// Pure capture of one window — `get_window_state` with a minimal element
/// budget, so agents keep a cheap "let me see it" verb and every capture
/// still lands in the session gallery.
fn build_screenshot(args: &Value) -> Result<EngineCall, String> {
    let pid = required_int(args, "pid")?;
    let window_id = required_int(args, "window_id")?;
    let mut call = EngineCall::new(
        "get_window_state",
        json!({
            "pid": pid,
            "window_id": window_id,
            "include_screenshot": true,
            "max_elements": 1,
        }),
    );
    call.screenshot_out = Some(new_screenshot_path(&caller_session_id()?, "screenshot"));
    Ok(call)
}

fn build_desktop() -> Result<EngineCall, String> {
    let mut call = EngineCall::new("get_desktop_state", json!({}));
    call.screenshot_out = Some(new_screenshot_path(&caller_session_id()?, "desktop"));
    Ok(call)
}

fn build_click(args: &Value) -> Result<EngineCall, String> {
    let has_element =
        args.get("element_index").is_some() || optional_str(args, "element_token").is_some();
    let has_coords = args.get("x").is_some() && args.get("y").is_some();
    if !has_element && !has_coords {
        return Err(
            "click needs 'element_index' (from the latest see, with pid + window_id) or \
'x'/'y' (screenshot pixels)."
                .into(),
        );
    }
    let tool: &'static str = if bool_arg(args, "right", false) {
        "right_click"
    } else if bool_arg(args, "double", false) {
        "double_click"
    } else {
        "click"
    };
    let map = input_args(args, &["action", "button", "count"]);
    Ok(EngineCall::new(tool, Value::Object(map)))
}

// ── context ──────────────────────────────────────────────────────────────

/// Always-callable context report: explains access state, engine health, and
/// platform prerequisites so an agent can discover why tools refuse.
pub(crate) fn tool_computer_context() -> Result<String, String> {
    let security = load_security();
    let session_id = self_session_id();
    let mut lines = vec!["Unpeel Computer MCP context".to_string(), String::new()];

    let access = security.default_access.as_state_str();
    lines.push(format!("- Access mode (app-wide): {access}"));
    if let Some(id) = &session_id {
        if security.default_access == ComputerAccess::Ask {
            let approved = security.approvals.iter().any(|entry| entry == id);
            lines.push(format!(
                "- This session's approval: {}",
                if approved {
                    "granted (remembered)"
                } else {
                    "not yet granted — the first action will ask the user"
                }
            ));
        }
        lines.push(format!(
            "- Screenshot artifacts: {}",
            screenshots_dir(id).display()
        ));
        lines.push(format!(
            "- cua-driver session: unpeel-{id} (capture_scope auto: \
window-first; the 'escalate' action unlocks desktop scope)"
        ));
    } else {
        lines.push("- Calling session: unknown (no UNPEEL_SESSION_ID)".into());
    }
    let allowed = security.settings.allowed_apps.trim();
    lines.push(format!(
        "- App allowlist: {}",
        if allowed.is_empty() {
            "all apps"
        } else {
            allowed
        }
    ));

    match resolve_engine_binary() {
        Ok(path) => {
            lines.push(format!("- Engine: {}", path.display()));
            if !daemon_socket_path().exists() {
                lines.push(format!(
                    "- Engine daemon: NOT RUNNING — {}",
                    daemon_down_error()
                ));
            } else if cfg!(target_os = "linux") {
                let display = std::env::var("WAYLAND_DISPLAY")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!("Wayland ({value})"))
                    .or_else(|| {
                        std::env::var("DISPLAY")
                            .ok()
                            .filter(|value| !value.trim().is_empty())
                            .map(|value| format!("X11 ({value})"))
                    })
                    .unwrap_or_else(|| "none visible to this process".into());
                lines.push(format!("- Linux graphical session: {display}"));
                lines.push(
                    "- Linux diagnostics: run `cua-driver doctor --json` on the Host; \
X11/Wayland plus the session D-Bus/AT-SPI services must be reachable."
                        .into(),
                );
            } else {
                match probe_permissions() {
                    Some(probe) => {
                        lines.push(format!(
                            "- macOS permissions: Accessibility {}, Screen Recording {}",
                            if probe.0 { "granted" } else { "NOT granted" },
                            if probe.1 { "granted" } else { "NOT granted" },
                        ));
                        let missing = missing_permission_names(probe);
                        if !missing.is_empty() {
                            lines.push(format!(
                                "- Required permissions are MISSING ({}): every computer \
action will fail until the user grants them in System Settings ▸ Privacy & Security (Unpeel ▸ \
Settings ▸ Computer shows live status with grant buttons). Tell the user; after they grant, \
retry — no app restart is required.",
                                missing.join(", ")
                            ));
                        }
                    }
                    None => lines.push(
                        "- macOS permissions: probe failed (daemon unreachable or \
unexpected output)"
                            .into(),
                    ),
                }
            }
        }
        Err(error) => lines.push(format!("- Engine: NOT AVAILABLE — {error}")),
    }

    lines.push(String::new());
    lines.push(
        "The screen is the user's real desktop: what you see may be sensitive, and other \
sessions or the user may be using it too. Control is background — element and pixel actions \
do not move the user's cursor or steal focus (an overlay cursor is shown instead), and \
desktop-scope input needs an explicit 'escalate'. Screenshots save into this session's \
artifacts and tools return the file path."
            .into(),
    );
    Ok(lines.join("\n"))
}

// ── Cleanup ──────────────────────────────────────────────────────────────

/// `unpeel-host __computer_cleanup__ <session-id>`: end the session's
/// cua-driver session so its overlay cursor and scope state are reclaimed
/// immediately (the daemon's idle TTL would get there eventually). Tolerates
/// a stopped daemon — cleanup must never fail a session removal.
pub fn run_cleanup(args: &[String]) -> Result<(), String> {
    let session_id = args
        .first()
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or("Usage: unpeel-host __computer_cleanup__ <session-id>")?;
    let session = format!("unpeel-{session_id}");
    match exec_engine_unchecked("end_session", &json!({ "session": session }), None) {
        Ok(_) => Ok(()),
        // Down daemon / unknown session are both fine outcomes for cleanup.
        Err(_) => Ok(()),
    }
}

// ── Help docs ────────────────────────────────────────────────────────────

/// Per-action docs for `{"action":"help"}`, in the same shape as the legacy
/// tool definitions so `render_action_help` can render them. This is the
/// single source of full parameter docs — the advertised schema stays terse.
pub(crate) fn action_docs() -> Vec<Value> {
    vec![
        json!({
            "name": "apps",
            "description": "List running applications (name, bundle id, pid, frontmost). \
        App-level discovery; for window questions use 'windows'.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] },
        }),
        json!({
            "name": "launch",
            "description": "Launch (or resolve, if already running — idempotent) an app \
        WITHOUT foregrounding it, and return its pid plus a windows array. The loop starter: \
        launch → see → act. Pass new_instance:true when another session may be driving the same app.",
            "inputSchema": { "type": "object", "properties": {
                "app": { "type": "string", "description": "Application name (e.g. \"Notes\")" },
                "bundle_id": { "type": "string", "description": "Bundle id (e.g. \"com.apple.finder\") — wins over app" },
                "urls": { "type": "array", "items": {"type": "string"}, "description": "Documents/URLs to open with the app" },
                "new_instance": { "type": "boolean", "description": "Force a separate app instance (own window) for this run" },
            }, "required": [] },
        }),
        json!({
            "name": "quit",
            "description": "Quit an application by pid.",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Process id from launch/apps" },
            }, "required": ["pid"] },
        }),
        json!({
            "name": "front",
            "description": "Bring an app's window to the foreground. Explicit escalation — \
        normal control is background and never steals focus; use only when the user asked to see \
        the app or background delivery was dropped.",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Specific window (default: main)" },
            }, "required": ["pid"] },
        }),
        json!({
            "name": "windows",
            "description": "List an app's windows: window_id, title, bounds, on-screen and \
        Space state. launch already returns this; call for long-lived pids.",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id" },
            }, "required": ["pid"] },
        }),
        json!({
            "name": "see",
            "description": "Snapshot one window: the accessibility tree (every actionable \
        element tagged [N] = element_index) PLUS a screenshot saved to this session's artifacts. \
        The mandatory bracket around every action — see before (fresh indices) and after (verify \
        the effect; an unchanged tree means the action probably silently failed). Indices go stale \
        on every new see. Works on background/minimized windows.",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Window from launch/windows" },
                "query": { "type": "string", "description": "Case-insensitive filter for the element tree" },
                "screenshot": { "type": "boolean", "description": "Capture pixels too (default true; false = tree only, cheaper re-index)" },
            }, "required": ["pid", "window_id"] },
        }),
        json!({
            "name": "screenshot",
            "description": "Capture one window into this session's artifacts and return the \
        file path — no element tree. Use see when you need element ids.",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Window from launch/windows" },
            }, "required": ["pid", "window_id"] },
        }),
        json!({
            "name": "desktop",
            "description": "Full-display screenshot (desktop scope) saved to artifacts. \
        Requires desktop scope: call 'escalate' first — window-scoped work never needs this.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] },
        }),
        json!({
            "name": "click",
            "description": "Click an element (element_index from the latest see — the \
        verifiable, backgroundable default) or a pixel (x/y read straight off that see's \
        screenshot; window-local, top-left origin). Never steals focus or moves the real cursor. \
        Escalate rungs only on a real signal: effect:\"suspected_noop\" or a degraded/empty tree → \
        pixel click; escalation \"foreground\" → re-call with delivery_mode:\"foreground\".",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Window (required with element_index)" },
                "element_index": { "type": "integer", "description": "[N] from the latest see" },
                "x": { "type": "number", "description": "Screenshot-pixel X (with y; alternative to element_index)" },
                "y": { "type": "number", "description": "Screenshot-pixel Y" },
                "double": { "type": "boolean", "description": "Double-click / open" },
                "right": { "type": "boolean", "description": "Right-click / context menu" },
                "button": { "type": "string", "description": "left | right | middle (default left)" },
                "count": { "type": "integer", "description": "Click count (pixel path)" },
                "action": { "type": "string", "description": "AX action override: press, show_menu, pick, confirm, cancel, open" },
                "modifiers": { "type": "array", "items": {"type": "string"}, "description": "Held modifiers: cmd, shift, option, ctrl" },
                "scope": { "type": "string", "description": "\"desktop\" for screen-absolute x/y (no pid; needs escalate)" },
                "delivery_mode": { "type": "string", "description": "background (default) | foreground — foreground briefly fronts the window then restores; a reaction to a dropped delivery, never a prediction" },
            }, "required": [] },
        }),
        json!({
            "name": "type",
            "description": "Insert text. Element form (element_index) targets the field \
        directly through accessibility; pixel form (x/y) clicks there first to establish renderer \
        focus, then types — the fix for Chromium/Electron fields the AX path can't reach. If the \
        response says effect:\"unverifiable\" with escalation \"px\", switch to the pixel form and \
        verify against the screenshot.",
            "inputSchema": { "type": "object", "properties": {
                "text": { "type": "string", "description": "Text to insert" },
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Window (with element_index)" },
                "element_index": { "type": "integer", "description": "Field from the latest see" },
                "x": { "type": "number", "description": "Pixel X of the field (focus-click first)" },
                "y": { "type": "number", "description": "Pixel Y" },
                "delay_ms": { "type": "integer", "description": "Per-keystroke delay" },
                "delivery_mode": { "type": "string", "description": "background (default) | foreground" },
            }, "required": ["text"] },
        }),
        json!({
            "name": "set_value",
            "description": "Set a NON-TEXT control's whole value through accessibility: \
        dropdowns, checkboxes, sliders, steppers. Also the keyboard-commit workaround on minimized \
        windows. For text use 'type'.",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Window of the element" },
                "element_index": { "type": "integer", "description": "Control from the latest see" },
                "value": { "type": "string", "description": "New value" },
            }, "required": ["pid", "value"] },
        }),
        json!({
            "name": "press",
            "description": "Send a named key (return, tab, escape, arrows, delete, f1–f12…). \
        Element form focuses the element first; pixel form clicks to focus; pid-only sends to the \
        app's current focus. Keyboard commits silently no-op on minimized windows — use set_value \
        or click a commit button instead.",
            "inputSchema": { "type": "object", "properties": {
                "key": { "type": "string", "description": "Key name" },
                "pid": { "type": "integer", "description": "Target process id" },
                "modifiers": { "type": "array", "items": {"type": "string"}, "description": "cmd, shift, option, ctrl" },
                "window_id": { "type": "integer", "description": "Window (with element_index)" },
                "element_index": { "type": "integer", "description": "Element to focus first" },
                "x": { "type": "number", "description": "Pixel X to focus-click first" },
                "y": { "type": "number", "description": "Pixel Y" },
                "delivery_mode": { "type": "string", "description": "background (default) | foreground" },
            }, "required": ["key"] },
        }),
        json!({
            "name": "hotkey",
            "description": "Send a modifier combo (e.g. [\"cmd\",\"c\"]) to a pid without \
        focus change; with x/y it focus-clicks that field first (e.g. cmd+v to paste into it).",
            "inputSchema": { "type": "object", "properties": {
                "keys": { "type": "string", "description": "Combo: [\"cmd\",\"shift\",\"t\"] or \"cmd,shift,t\"" },
                "pid": { "type": "integer", "description": "Target process id" },
                "x": { "type": "number", "description": "Pixel X to focus-click first" },
                "y": { "type": "number", "description": "Pixel Y" },
                "window_id": { "type": "integer", "description": "Anchor window for x/y" },
                "delivery_mode": { "type": "string", "description": "background (default) | foreground" },
            }, "required": ["keys"] },
        }),
        json!({
            "name": "scroll",
            "description": "Scroll a window, an element from see, or a pixel point.",
            "inputSchema": { "type": "object", "properties": {
                "direction": { "type": "string", "description": "up | down | left | right" },
                "amount": { "type": "integer", "description": "How far (default engine-chosen)" },
                "by": { "type": "string", "description": "line | page" },
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Target window" },
                "element_index": { "type": "integer", "description": "Scroll at this element's center (nested overflow regions)" },
                "x": { "type": "number", "description": "Pixel X to scroll at (with y)" },
                "y": { "type": "number", "description": "Pixel Y" },
            }, "required": ["direction"] },
        }),
        json!({
            "name": "drag",
            "description": "Drag from one point to another in window-local screenshot \
        pixels (mouseDown → interpolated moves → mouseUp).",
            "inputSchema": { "type": "object", "properties": {
                "pid": { "type": "integer", "description": "Target process id" },
                "window_id": { "type": "integer", "description": "Window the pixels were measured against" },
                "from_x": { "type": "number", "description": "Start X" },
                "from_y": { "type": "number", "description": "Start Y" },
                "to_x": { "type": "number", "description": "End X" },
                "to_y": { "type": "number", "description": "End Y" },
                "duration_ms": { "type": "integer", "description": "Gesture duration (default 500, max 10000)" },
                "steps": { "type": "integer", "description": "Interpolated move events (default 20)" },
                "modifiers": { "type": "array", "items": {"type": "string"}, "description": "Held modifiers" },
                "button": { "type": "string", "description": "left | right | middle" },
            }, "required": ["from_x", "from_y", "to_x", "to_y"] },
        }),
        json!({
            "name": "move_cursor",
            "description": "Glide this session's overlay cursor to a point (visual only — \
        the user's real pointer never moves). Seeds the cursor so later element actions animate \
        a full glide; useful for demos the user is watching.",
            "inputSchema": { "type": "object", "properties": {
                "x": { "type": "number", "description": "Target X" },
                "y": { "type": "number", "description": "Target Y" },
            }, "required": ["x", "y"] },
        }),
        json!({
            "name": "escalate",
            "description": "One-way escalation of this session from window scope to \
        desktop scope (full-display perception, screen-absolute + foreground input). Only after \
        the window ladder is exhausted and verified: element action → pixel action → \
        delivery_mode foreground, each re-checked with see.",
            "inputSchema": { "type": "object", "properties": {
                "reason": { "type": "string", "description": "Advertised reason, e.g. \"foreground_ineffective\"" },
                "detail": { "type": "string", "description": "Bounded non-sensitive summary of what failed" },
            }, "required": ["reason"] },
        }),
        json!({
            "name": "context",
            "description": "Current Computer MCP configuration: access state, this session's \
        approval, engine + daemon availability, macOS permission status, app allowlist, artifact \
        folder. Call this first if computer tools seem unavailable.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] },
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_and_docs_agree() {
        let documented: Vec<String> = action_docs()
            .iter()
            .map(|doc| doc["name"].as_str().unwrap().to_string())
            .collect();
        for action in COMPUTER_ACTIONS {
            assert!(
                documented.iter().any(|name| name == action),
                "action {action} has no help docs"
            );
        }
        for name in &documented {
            assert!(
                is_computer_action(name),
                "documented action {name} is not dispatchable"
            );
        }
    }

    fn build(action: &str, args: Value) -> Result<EngineCall, String> {
        build_action(action, &args, &ComputerSettings::default())
    }

    #[test]
    fn click_requires_a_target_form() {
        let err = build("click", json!({ "pid": 4 })).expect_err("click without target");
        assert!(err.contains("element_index"));
        assert!(err.contains("'x'"));
    }

    #[test]
    fn click_maps_right_and_double_to_dedicated_tools() {
        let call = build(
            "click",
            json!({ "pid": 4, "window_id": 9, "element_index": 3 }),
        )
        .expect("plain click");
        assert_eq!(call.tool, "click");
        assert_eq!(call.args["element_index"], 3);

        let call = build(
            "click",
            json!({ "pid": 4, "x": 10, "y": 20, "right": true }),
        )
        .expect("right click");
        assert_eq!(call.tool, "right_click");

        let call = build(
            "click",
            json!({ "pid": 4, "x": 10, "y": 20, "double": true }),
        )
        .expect("double click");
        assert_eq!(call.tool, "double_click");
    }

    #[test]
    fn whitelist_drops_unknown_and_policy_fields() {
        let call = build(
            "click",
            json!({
                "pid": 4, "window_id": 9, "element_index": 3,
                "session": "evil", "debug_image_out": "/tmp/x", "screenshot_out_file": "/tmp/y",
            }),
        )
        .expect("click");
        assert!(call.args.get("session").is_none());
        assert!(call.args.get("debug_image_out").is_none());
        assert!(call.args.get("screenshot_out_file").is_none());
    }

    #[test]
    fn hotkey_accepts_string_or_array_keys() {
        let call = build("hotkey", json!({ "pid": 4, "keys": "cmd,shift,t" })).expect("string");
        assert_eq!(call.args["keys"], json!(["cmd", "shift", "t"]));
        let call = build("hotkey", json!({ "pid": 4, "keys": ["cmd", "c"] })).expect("array");
        assert_eq!(call.args["keys"], json!(["cmd", "c"]));
        assert!(build("hotkey", json!({ "pid": 4 })).is_err());
    }

    #[test]
    fn press_maps_modifiers_field_name() {
        let call = build(
            "press",
            json!({ "pid": 4, "key": "return", "modifiers": "cmd+shift" }),
        )
        .expect("press");
        assert_eq!(call.tool, "press_key");
        assert_eq!(call.args["modifiers"], json!(["cmd", "shift"]));
        assert!(call.args.get("modifier").is_none());
    }

    #[test]
    fn launch_maps_app_name_and_bundle_id() {
        let call = build("launch", json!({ "app": "Notes" })).expect("by name");
        assert_eq!(call.tool, "launch_app");
        assert_eq!(call.args["name"], "Notes");

        let call = build(
            "launch",
            json!({ "bundle_id": "com.apple.finder", "new_instance": true }),
        )
        .expect("by bundle");
        assert_eq!(call.args["bundle_id"], "com.apple.finder");
        assert_eq!(call.args["creates_new_application_instance"], true);
        assert!(build("launch", json!({})).is_err());
    }

    #[test]
    fn launch_enforces_the_allowlist() {
        let settings = ComputerSettings {
            allowed_apps: "Safari, TextEdit".into(),
        };
        assert!(build_action("launch", &json!({ "app": "textedit" }), &settings).is_ok());
        let err = build_action("launch", &json!({ "app": "Mail" }), &settings)
            .expect_err("Mail is not allowed");
        assert!(err.contains("allowlist"));
    }

    #[test]
    fn scroll_validates_direction() {
        let err = build("scroll", json!({ "direction": "sideways" })).expect_err("bad direction");
        assert!(err.contains("up, down, left, or right"));
    }

    #[test]
    fn drag_requires_the_coordinate_quad() {
        let err = build("drag", json!({ "pid": 4, "from_x": 1, "from_y": 2 }))
            .expect_err("incomplete drag");
        assert!(err.contains("to_x"));
        let call = build(
            "drag",
            json!({ "pid": 4, "from_x": 1, "from_y": 2, "to_x": 3, "to_y": 4 }),
        )
        .expect("full drag");
        assert_eq!(call.tool, "drag");
    }

    #[test]
    fn set_value_accepts_text_alias() {
        let call = build(
            "set_value",
            json!({ "pid": 4, "window_id": 9, "element_index": 2, "text": "on" }),
        )
        .expect("set_value");
        assert_eq!(call.args["value"], "on");
    }

    #[test]
    fn escalate_requires_a_reason() {
        assert!(build("escalate", json!({})).is_err());
        let call = build(
            "escalate",
            json!({ "reason": "foreground_ineffective", "detail": "AX+px+fg all no-oped" }),
        )
        .expect("escalate");
        assert_eq!(call.tool, "escalate_session");
    }

    #[test]
    fn allowlist_blocks_unlisted_apps() {
        let settings = ComputerSettings {
            allowed_apps: "Safari, TextEdit".into(),
        };
        assert!(check_app_allowed(&settings, "safari").is_ok());
        assert!(check_app_allowed(&settings, "TextEdit").is_ok());
        let err = check_app_allowed(&settings, "Mail").expect_err("Mail is not allowed");
        assert!(err.contains("allowlist"));

        let open = ComputerSettings::default();
        assert!(check_app_allowed(&open, "Anything").is_ok());
    }

    #[test]
    fn truncate_output_caps_long_text() {
        let long = "x".repeat(ENGINE_OUTPUT_MAX_CHARS + 100);
        let capped = truncate_output(&long);
        assert!(capped.contains("output truncated"));
    }

    #[test]
    fn strips_inline_screenshot_payloads() {
        let output = r#"{
  "tree_markdown": "[1] button",
  "screenshot_png_b64": "aGVsbG8=",
  "screenshot_mime_type": "image/png"
}"#;
        let cleaned = strip_inline_screenshot(output);
        assert!(!cleaned.contains("screenshot_png_b64"));
        assert!(cleaned.contains("tree_markdown"));
        // Non-JSON output passes through untouched.
        assert_eq!(strip_inline_screenshot("plain text"), "plain text");
    }

    #[test]
    fn parses_check_permissions_shape() {
        // Real `cua-driver call check_permissions` structuredContent shape.
        let value: Value = serde_json::from_str(
            r#"{
  "accessibility": true,
  "screen_recording": false,
  "screen_recording_capturable": null,
  "source": { "attribution": "host", "embedded": true }
}"#,
        )
        .unwrap();
        let probe = (
            value["accessibility"].as_bool().unwrap(),
            value["screen_recording"].as_bool().unwrap(),
        );
        assert_eq!(
            missing_permission_names(probe),
            vec!["Screen Recording".to_string()]
        );
        assert!(missing_permission_names((true, true)).is_empty());
    }

    #[test]
    fn host_launch_policy_uses_canonical_access_and_migrates_minor_13_key() {
        let enabled = |access_key: &str, access: Value| {
            requested_from_app_state(&json!({
                "experimental_features": { "computer_use": true },
                (access_key): access,
            }))
        };
        assert!(enabled("computer_default_access", json!("ask")));
        assert!(enabled("computer_default_access", json!("allow")));
        assert!(!enabled("computer_default_access", json!("off")));
        assert!(enabled("computer_access", json!("allow")));
        assert!(!requested_from_app_state(&json!({
            "computer_default_access": "allow",
            "experimental_features": { "computer_use": false },
        })));
        assert!(!requested_from_app_state(&json!({
            "computer_default_access": "future-mode",
            "experimental_features": { "computer_use": true },
        })));
    }
}
