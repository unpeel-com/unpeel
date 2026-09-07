//! Shared Unpeel App-to-agent handoff primitives.
//!
//! The Host's unified MCP remains the authority for peer discovery and
//! same-group write policy. Apps only ask it for the best nearby agent and
//! paste a reference into that agent's input without submitting it.

use std::collections::HashSet;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};

const MAX_LITERAL_KEYS_PER_CALL: usize = 40;

/// Error returned when no eligible agent exists or the Host MCP is
/// unavailable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentError(String);

impl AgentError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AgentError {}

/// Whether this process is running inside a hosted Unpeel session.
#[must_use]
pub fn is_hosted() -> bool {
    std::env::var("UNPEEL_SESSION_ID").is_ok_and(|session_id| !session_id.trim().is_empty())
}

/// A control-safe, absolute path token suitable for an agent input.
///
/// JSON string escaping keeps newlines and other unusual filename bytes from
/// becoming terminal control input. Non-UTF-8 bytes are represented lossily;
/// Unpeel's supported macOS filesystems normally provide UTF-8 names.
#[must_use]
pub fn path_reference(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let quoted = serde_json::to_string(absolute.to_string_lossy().as_ref())
        .unwrap_or_else(|_| "\"\"".to_owned());
    format!("[path: {quoted}]")
}

/// OSC 52 clipboard-copy sequence for an App's standalone fallback.
#[must_use]
pub fn clipboard_sequence(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

/// Paste text into the best agent near this App's pane.
///
/// A direct pane neighbor wins — even one outside this sidebar group, in
/// which case the Host asks the user to approve the cross-group write —
/// then a recognized agent runtime in the group, then (for Apps pinned in
/// the sticky project sidebar) a recognized agent elsewhere in the same
/// root project, then an older command-derived provider match. Settled
/// agents are preferred. The text is deliberately not submitted, leaving
/// the user in control of the final prompt.
pub fn send_to_agent(text: &str) -> Result<String, AgentError> {
    if !is_hosted() {
        return Err(AgentError::new("not inside an Unpeel session"));
    }
    let mut client = McpClient::spawn()?;
    let (target_id, label) = resolve_agent(&mut client)?;
    client.call_tool(
        "sessions",
        &json!({
            "action": "send_text",
            "session_id": target_id,
            "text": format!("{text} "),
            "submit": false,
        }),
    )?;
    Ok(label)
}

/// Type an exact, single-line file reference into the best nearby agent.
///
/// References use the Sessions MCP keystroke path instead of its message
/// path. That keeps a cross-group handoff literal — `path:line-range` only —
/// without the provenance envelope that belongs on conversational messages.
/// The reference is not submitted.
pub fn send_reference_to_agent(reference: &str) -> Result<String, AgentError> {
    if !is_hosted() {
        return Err(AgentError::new("not inside an Unpeel session"));
    }
    let batches = literal_key_batches(reference)?;
    let mut client = McpClient::spawn()?;
    let (target_id, label) = resolve_agent(&mut client)?;
    for keys in batches {
        client.call_tool(
            "sessions",
            &json!({
                "action": "send_keys",
                "session_id": target_id,
                "keys": keys,
                "delay_ms": 0,
            }),
        )?;
    }
    Ok(label)
}

fn literal_key_batches(reference: &str) -> Result<Vec<Vec<String>>, AgentError> {
    if reference.is_empty() {
        return Err(AgentError::new("reference is empty"));
    }
    if reference.chars().any(char::is_control) {
        return Err(AgentError::new(
            "reference contains unsupported control characters",
        ));
    }
    let keys = reference
        .chars()
        .map(|character| character.to_string())
        .collect::<Vec<_>>();
    Ok(keys
        .chunks(MAX_LITERAL_KEYS_PER_CALL)
        .map(<[String]>::to_vec)
        .collect())
}

/// Cached, non-blocking peer discovery for context menus plus synchronous
/// send helpers for the selected action.
///
/// `refresh` never blocks the UI thread and coalesces concurrent probes.
/// `send_text` re-resolves at click time so a stale menu label cannot send to
/// a departed session.
#[derive(Clone, Default)]
pub struct AgentBridge {
    label: Arc<Mutex<Option<String>>>,
    project_context: Arc<Mutex<Option<AgentProjectContext>>>,
    probing: Arc<AtomicBool>,
    /// One live MCP server per bridge, reused across refreshes: a probe on a
    /// warm client costs a few milliseconds. Dropped and respawned on any
    /// error.
    client: Arc<Mutex<Option<McpClient>>>,
    /// Last seen modification stamp of the Controller's durable pane layout
    /// (`pane-layouts.json`), which the Mac app rewrites on every selection
    /// and layout change — the cheap trigger for an immediate re-probe.
    layout_stamp: Arc<Mutex<Option<std::time::SystemTime>>>,
}

/// Project/worktree identity of the agent this App would hand off to.
/// `cwd` is Host-authoritative Session launch context, not a pane-title guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProjectContext {
    pub session_id: String,
    pub label: String,
    pub project_id: String,
    pub cwd: PathBuf,
}

impl AgentBridge {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Last known peer label. `None` means the menu should offer a copy
    /// fallback rather than promise that an agent is available.
    #[must_use]
    pub fn label(&self) -> Option<String> {
        self.label.lock().ok()?.clone()
    }

    /// Last asynchronously discovered main/neighboring agent context.
    #[must_use]
    pub fn project_context(&self) -> Option<AgentProjectContext> {
        self.project_context.lock().ok()?.clone()
    }

    /// True when the Controller's durable pane layout changed since the last
    /// call — the moment a sidebar-pinned App's neighbor can change. One
    /// `stat` per call, so Apps can ask on every UI tick and refresh right
    /// then instead of on a fixed cadence.
    pub fn layout_changed(&self) -> bool {
        let Some(path) = layout_file_path() else {
            return false;
        };
        let stamp = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok();
        let mut last = self
            .layout_stamp
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *last == stamp {
            return false;
        }
        let first = last.is_none();
        *last = stamp;
        // The very first observation just records the baseline.
        !first
    }

    /// Refresh the cached label off the UI thread, if hosted by Unpeel.
    pub fn refresh(&self) {
        if !is_hosted() || self.probing.swap(true, Ordering::SeqCst) {
            return;
        }
        let label = Arc::clone(&self.label);
        let project_context = Arc::clone(&self.project_context);
        let probing = Arc::clone(&self.probing);
        let shared_client = Arc::clone(&self.client);
        std::thread::spawn(move || {
            let found = {
                let mut slot = shared_client
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = match slot.as_mut() {
                    Some(client) => resolve_agent_project_context(client),
                    None => McpClient::spawn().and_then(|mut client| {
                        let result = resolve_agent_project_context(&mut client);
                        *slot = Some(client);
                        result
                    }),
                };
                if result.is_err() {
                    // A failed or exited server is not retried in place: the
                    // next refresh spawns a fresh one.
                    *slot = None;
                }
                result.ok()
            };
            if let Ok(mut slot) = label.lock() {
                *slot = found.as_ref().map(|context| context.label.clone());
            }
            if let Ok(mut slot) = project_context.lock() {
                *slot = found;
            }
            probing.store(false, Ordering::SeqCst);
        });
    }

    /// Paste text into the current target and update the cached label.
    pub fn send_text(&self, text: &str) -> Result<String, AgentError> {
        let label = send_to_agent(text)?;
        if let Ok(mut slot) = self.label.lock() {
            *slot = Some(label.clone());
        }
        Ok(label)
    }

    /// Type an exact one-line `path:line-range` reference without a sender
    /// envelope and without submitting it.
    pub fn send_reference(&self, reference: &str) -> Result<String, AgentError> {
        let label = send_reference_to_agent(reference)?;
        if let Ok(mut slot) = self.label.lock() {
            *slot = Some(label.clone());
        }
        Ok(label)
    }

    /// Paste a safe absolute path reference into the current target.
    pub fn send_path(&self, path: impl AsRef<Path>) -> Result<String, AgentError> {
        self.send_text(&path_reference(path))
    }
}

fn resolve_agent_project_context(
    client: &mut McpClient,
) -> Result<AgentProjectContext, AgentError> {
    let group = client
        .call_tool(
            "sessions",
            &json!({ "action": "list_group", "include_exited": false }),
        )
        .ok();
    // Following is about WHERE the neighbor works, not whether it is an
    // agent: a plain terminal opened inside a worktree beside this App is
    // the folder to show. Sending text still targets agents only.
    if let Some(current) = client
        .call_tool("sessions", &json!({ "action": "current" }))
        .ok()
        && let Some(context) = adjacent_follow_context(&current, group.as_ref())
    {
        return Ok(context);
    }
    let (session_id, label) = resolve_agent(client)?;
    let agents = client
        .call_tool("agents", &json!({ "action": "list" }))
        .ok();
    project_context_for_target(&session_id, &label, agents.as_ref(), group.as_ref())
        .ok_or_else(|| AgentError::new("agent project context unavailable"))
}

/// The running Session beside this pane, whatever it runs, as a place to
/// follow. Left first (the main content for a sidebar-pinned App), then the
/// other directions.
fn adjacent_follow_context(current: &Value, group: Option<&Value>) -> Option<AgentProjectContext> {
    let neighbors = current.pointer("/pane_context/neighbors")?;
    let entry = ["left", "right", "up", "down"]
        .iter()
        .filter_map(|direction| neighbors.get(*direction))
        .find(|entry| {
            string_field(entry, "state") == "running"
                && Path::new(&string_field(entry, "cwd")).is_absolute()
                && !string_field(entry, "session_id").is_empty()
        })?;
    let session_id = string_field(entry, "session_id");
    let project_id = group
        .and_then(|value| value.get("sessions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|session| string_field(session, "id") == session_id)
        .map(|session| string_field(session, "project_id"))
        .unwrap_or_default();
    Some(AgentProjectContext {
        label: label_or(entry, "label", &string_field(entry, "runtime_id")),
        session_id,
        project_id,
        cwd: PathBuf::from(string_field(entry, "cwd")),
    })
}

fn project_context_for_target(
    session_id: &str,
    label: &str,
    agents: Option<&Value>,
    group: Option<&Value>,
) -> Option<AgentProjectContext> {
    let agent = agents
        .and_then(|value| value.get("agents"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|agent| {
            agent
                .pointer("/agent_ref/session_id")
                .and_then(Value::as_str)
                == Some(session_id)
        });
    let session = group
        .and_then(|value| value.get("sessions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|session| string_field(session, "id") == session_id);
    let source = agent.or(session)?;
    let cwd = PathBuf::from(string_field(source, "cwd"));
    if !cwd.is_absolute() {
        return None;
    }
    Some(AgentProjectContext {
        session_id: session_id.to_owned(),
        label: label.to_owned(),
        project_id: string_field(source, "project_id"),
        cwd,
    })
}

fn resolve_agent(client: &mut McpClient) -> Result<(String, String), AgentError> {
    let group = client.call_tool(
        "sessions",
        &json!({ "action": "list_group", "include_exited": false }),
    )?;
    let sessions = group
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let group_ids = sessions
        .iter()
        .map(|session| string_field(session, "id"))
        .collect::<HashSet<_>>();

    let current = client
        .call_tool("sessions", &json!({ "action": "current" }))
        .ok();
    if let Some(target) = current
        .as_ref()
        .and_then(|current| adjacent_target(current, &group_ids))
    {
        return Ok(target);
    }

    let agents = client
        .call_tool("agents", &json!({ "action": "list" }))
        .ok();
    if let Some(target) = agents
        .as_ref()
        .and_then(|agents| recognized_target(agents, &group_ids))
    {
        return Ok(target);
    }

    // A sticky project-sidebar App lives in the per-project "sidebar-<root>"
    // group, which holds only fellow sidebar panes and is outside the
    // persisted multi-pane layout — so neither the neighbor nor the group
    // paths can see the panes it is displayed beside. Those panes all belong
    // to the same root project, so reach the project's agents instead.
    if let (Some(current), Some(agents)) = (current.as_ref(), agents.as_ref())
        && let Some(target) = sidebar_project_target(agents, current)
    {
        return Ok(target);
    }

    provider_target(&sessions).ok_or_else(|| AgentError::new("no agent pane nearby"))
}

/// Installed Unpeel Apps are recognized runtimes too, stamped with
/// reverse-DNS app ids ("unpeel.app.diffs"); conversational agent slugs
/// ("claude", "codex") never contain a dot.
fn is_conversational_runtime(runtime_id: &str) -> bool {
    !runtime_id.is_empty() && !runtime_id.contains('.')
}

fn sidebar_project_target(agents: &Value, current: &Value) -> Option<(String, String)> {
    let session = current.get("current_session")?;
    if !string_field(session, "group_id").starts_with("sidebar-") {
        return None;
    }
    let project = string_field(session, "project_id");
    if project.is_empty() {
        return None;
    }
    let mut candidates = agents
        .get("agents")
        .and_then(Value::as_array)?
        .iter()
        .filter(|agent| {
            agent.get("self").and_then(Value::as_bool) != Some(true)
                && is_conversational_runtime(&string_field(agent, "runtime_id"))
                && string_field(agent, "project_id") == project
                && string_field(agent, "state") == "running"
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|agent| settled_rank(&string_field(agent, "activity_status")));
    candidates.first().and_then(|agent| {
        let reference = agent.get("agent_ref")?;
        let runtime = string_field(agent, "runtime_id");
        Some((
            string_field(reference, "session_id"),
            label_or(agent, "label", &runtime),
        ))
    })
}

fn adjacent_target(current: &Value, group_ids: &HashSet<String>) -> Option<(String, String)> {
    let neighbors = current.pointer("/pane_context/neighbors")?;
    let mut candidates = ["left", "right", "up", "down"]
        .iter()
        .filter_map(|direction| neighbors.get(*direction))
        .filter(|entry| string_field(entry, "kind") == "agent")
        .collect::<Vec<_>>();
    // A direct pane neighbor outside this sidebar group is still reachable;
    // the Host asks the user for cross-group write approval. Same-group
    // neighbors win because their writes are approval-free.
    candidates.sort_by_key(|entry| {
        (
            !group_ids.contains(&string_field(entry, "session_id")),
            settled_rank(&string_field(entry, "activity_status")),
        )
    });
    candidates.first().map(|entry| {
        let runtime = string_field(entry, "runtime_id");
        (
            string_field(entry, "session_id"),
            label_or(entry, "label", &runtime),
        )
    })
}

fn recognized_target(agents: &Value, group_ids: &HashSet<String>) -> Option<(String, String)> {
    let mut candidates = agents
        .get("agents")
        .and_then(Value::as_array)?
        .iter()
        .filter(|agent| {
            agent.get("self").and_then(Value::as_bool) != Some(true)
                && is_conversational_runtime(&string_field(agent, "runtime_id"))
                && agent.get("agent_ref").is_some_and(|reference| {
                    group_ids.contains(&string_field(reference, "session_id"))
                })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|agent| settled_rank(&string_field(agent, "activity_status")));
    candidates.first().and_then(|agent| {
        let reference = agent.get("agent_ref")?;
        let runtime = string_field(agent, "runtime_id");
        Some((
            string_field(reference, "session_id"),
            label_or(agent, "label", &runtime),
        ))
    })
}

fn provider_target(sessions: &[Value]) -> Option<(String, String)> {
    let mut candidates = sessions
        .iter()
        .filter(|session| {
            let provider = string_field(session, "provider");
            string_field(session, "state") == "running"
                && !provider.is_empty()
                && provider != "shell"
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|session| settled_rank(&string_field(session, "activity_status")));
    candidates.first().map(|session| {
        let provider = string_field(session, "provider");
        (
            string_field(session, "id"),
            label_or(session, "label", &provider),
        )
    })
}

fn settled_rank(status: &str) -> u8 {
    match status {
        "idle" | "done" => 0,
        "blocked" => 1,
        _ => 2,
    }
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn label_or(value: &Value, key: &str, fallback: &str) -> String {
    let label = string_field(value, key);
    if label.is_empty() {
        fallback.to_owned()
    } else {
        label
    }
}

/// The Controller's durable pane layout for the home this Session lives in
/// (`<home>/pane-layouts.json`), derived from the Host's session directory.
fn layout_file_path() -> Option<PathBuf> {
    let session_dir = PathBuf::from(std::env::var_os("UNPEEL_SESSION_DIR")?);
    // <home>/app-sessions/<id> → <home>
    let home = session_dir.parent()?.parent()?;
    Some(home.join("pane-layouts.json"))
}

/// The `unpeel-host` that hosts this App. The Host exports its own binary
/// path to every hosted child (`UNPEEL_HOST_BIN`; the older `UNPEEL_MCP_BIN`
/// on Codex launches) — prefer it over PATH, where a stale CLI install would
/// answer with an older protocol and hide the App's neighbors.
fn host_binary() -> std::ffi::OsString {
    ["UNPEEL_HOST_BIN", "UNPEEL_MCP_BIN"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "unpeel-host".into())
}

struct McpClient {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn spawn() -> Result<Self, AgentError> {
        let mut child = Command::new(host_binary())
            .arg("__mcp__")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| AgentError::new(format!("unpeel-host not available: {error}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::new("mcp stdout unavailable"))?;
        let mut client = Self {
            child,
            reader: BufReader::new(stdout),
            next_id: 1,
        };
        client.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
        )?;
        Ok(client)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, AgentError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| AgentError::new("mcp stdin closed"))?;
        writeln!(stdin, "{body}")
            .map_err(|error| AgentError::new(format!("mcp write failed: {error}")))?;
        stdin
            .flush()
            .map_err(|error| AgentError::new(format!("mcp flush failed: {error}")))?;

        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|error| AgentError::new(format!("mcp read failed: {error}")))?;
            if read == 0 {
                return Err(AgentError::new("mcp server exited"));
            }
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(AgentError::new(format!("mcp error: {error}")));
            }
            return Ok(message.get("result").cloned().unwrap_or_default());
        }
    }

    fn call_tool(&mut self, tool: &str, arguments: &Value) -> Result<Value, AgentError> {
        let result = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )?;
        let text = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(AgentError::new(text));
        }
        Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned())))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.child.stdin.take();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn following_takes_any_running_neighbor_but_prefers_the_left_one() {
        let current = json!({
            "pane_context": {
                "neighbors": {
                    "left": { "kind": "terminal", "state": "running", "cwd": "/tmp/wt/feature", "session_id": "shell", "label": "zsh", "runtime_id": "" },
                    "right": { "kind": "agent", "state": "running", "cwd": "/tmp/main", "session_id": "agent", "label": "Claude", "runtime_id": "claude" },
                    "up": null,
                    "down": null
                }
            }
        });
        let group = json!({ "sessions": [ { "id": "shell", "project_id": "proj-wt" } ] });
        let context = super::adjacent_follow_context(&current, Some(&group)).unwrap();
        assert_eq!(context.session_id, "shell");
        assert_eq!(context.cwd, std::path::PathBuf::from("/tmp/wt/feature"));
        assert_eq!(context.project_id, "proj-wt");

        // An exited or cwd-less neighbor is skipped in favor of the next.
        let current = json!({ "pane_context": { "neighbors": {
            "left": { "kind": "terminal", "state": "exited", "cwd": "/tmp/old", "session_id": "gone" },
            "right": { "kind": "agent", "state": "running", "cwd": "/tmp/main", "session_id": "agent", "label": "Claude" },
            "up": null, "down": null } } });
        assert_eq!(
            super::adjacent_follow_context(&current, None)
                .unwrap()
                .session_id,
            "agent"
        );
        let none = json!({ "pane_context": { "neighbors": { "left": null, "right": null, "up": null, "down": null } } });
        assert!(super::adjacent_follow_context(&none, None).is_none());
    }

    use super::*;

    #[test]
    fn path_tokens_are_absolute_and_control_safe() {
        let token = path_reference(Path::new("folder/odd\nname"));
        assert!(token.starts_with("[path: \"/"));
        assert!(token.contains("odd\\nname"));
        assert!(!token.contains("odd\nname"));
    }

    #[test]
    fn clipboard_sequence_uses_osc_52() {
        assert_eq!(clipboard_sequence("hello"), "\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn literal_reference_keys_preserve_only_the_reference() {
        let reference = format!("notes/{}:12-14", "é".repeat(50));
        let batches = literal_key_batches(&reference).unwrap();
        assert!(
            batches
                .iter()
                .all(|batch| !batch.is_empty() && batch.len() <= MAX_LITERAL_KEYS_PER_CALL)
        );
        assert_eq!(batches.into_iter().flatten().collect::<String>(), reference);
        assert!(literal_key_batches("notes/file.md:1\nextra").is_err());
    }

    #[test]
    fn adjacent_idle_agent_wins_inside_the_group() {
        let current = json!({
            "pane_context": { "neighbors": {
                "left": { "kind": "agent", "session_id": "busy", "label": "Busy", "activity_status": "busy" },
                "right": { "kind": "agent", "session_id": "idle", "runtime_id": "claude", "activity_status": "idle" },
                "down": { "kind": "agent", "session_id": "other-group", "label": "Wrong", "activity_status": "idle" }
            }}
        });
        let group = HashSet::from(["busy".to_owned(), "idle".to_owned()]);
        assert_eq!(
            adjacent_target(&current, &group),
            Some(("idle".to_owned(), "claude".to_owned()))
        );
    }

    #[test]
    fn project_context_uses_the_resolved_agents_host_cwd() {
        let agents = json!({ "agents": [{
            "agent_ref": { "session_id": "agent-1" },
            "project_id": "worktree-project",
            "cwd": "/tmp/project-worktree"
        }] });
        let context =
            project_context_for_target("agent-1", "Claude", Some(&agents), None).expect("context");
        assert_eq!(context.session_id, "agent-1");
        assert_eq!(context.project_id, "worktree-project");
        assert_eq!(context.cwd, PathBuf::from("/tmp/project-worktree"));
    }

    #[test]
    fn adjacent_agents_outside_the_group_remain_reachable() {
        let current = json!({
            "pane_context": { "neighbors": {
                "right": { "kind": "agent", "session_id": "other-group", "label": "Claude", "activity_status": "idle" },
                "down": { "kind": "app", "session_id": "viewer", "label": "Viewer", "activity_status": "idle" }
            }}
        });
        let group = HashSet::from(["me".to_owned()]);
        assert_eq!(
            adjacent_target(&current, &group),
            Some(("other-group".to_owned(), "Claude".to_owned()))
        );
    }

    #[test]
    fn same_group_neighbors_win_over_settled_outsiders() {
        let current = json!({
            "pane_context": { "neighbors": {
                "left": { "kind": "agent", "session_id": "grouped", "label": "Grouped", "activity_status": "busy" },
                "right": { "kind": "agent", "session_id": "outside", "label": "Outside", "activity_status": "idle" }
            }}
        });
        let group = HashSet::from(["grouped".to_owned()]);
        assert_eq!(
            adjacent_target(&current, &group),
            Some(("grouped".to_owned(), "Grouped".to_owned()))
        );
    }

    #[test]
    fn sidebar_apps_reach_the_projects_agents() {
        let current = json!({
            "current_session": { "group_id": "sidebar-native-root", "project_id": "native-root" }
        });
        let agents = json!({ "agents": [
            { "self": true, "runtime_id": "claude", "project_id": "native-root", "state": "running", "activity_status": "idle", "agent_ref": { "session_id": "me" } },
            { "runtime_id": "unpeel.app.usage", "project_id": "native-root", "state": "running", "activity_status": "idle", "label": "Usage", "agent_ref": { "session_id": "usage-app" } },
            { "runtime_id": "claude", "project_id": "other-project", "state": "running", "activity_status": "idle", "agent_ref": { "session_id": "elsewhere" } },
            { "runtime_id": "claude", "project_id": "native-root", "state": "running", "activity_status": "working", "label": "Busy Claude", "agent_ref": { "session_id": "busy" } },
            { "runtime_id": "codex", "project_id": "native-root", "state": "running", "activity_status": "idle", "agent_ref": { "session_id": "settled" } }
        ]});
        assert_eq!(
            sidebar_project_target(&agents, &current),
            Some(("settled".to_owned(), "codex".to_owned()))
        );
    }

    #[test]
    fn ordinary_groups_skip_the_sidebar_project_fallback() {
        let current = json!({
            "current_session": { "group_id": "native-root", "project_id": "native-root" }
        });
        let agents = json!({ "agents": [
            { "runtime_id": "claude", "project_id": "native-root", "state": "running", "activity_status": "idle", "agent_ref": { "session_id": "agent" } }
        ]});
        assert_eq!(sidebar_project_target(&agents, &current), None);
    }

    #[test]
    fn recognized_agents_exclude_installed_apps() {
        let agents = json!({ "agents": [
            { "runtime_id": "unpeel.app.files", "activity_status": "idle", "label": "Files", "agent_ref": { "session_id": "app" } },
            { "runtime_id": "claude", "activity_status": "busy", "agent_ref": { "session_id": "agent" } }
        ]});
        let group = HashSet::from(["app".to_owned(), "agent".to_owned()]);
        assert_eq!(
            recognized_target(&agents, &group),
            Some(("agent".to_owned(), "claude".to_owned()))
        );
    }

    #[test]
    fn provider_fallback_excludes_shells_and_prefers_settled() {
        let sessions = vec![
            json!({ "id": "shell", "provider": "shell", "state": "running", "activity_status": "idle" }),
            json!({ "id": "busy", "provider": "codex", "state": "running", "activity_status": "busy" }),
            json!({ "id": "idle", "provider": "claude", "label": "Claude", "state": "running", "activity_status": "done" }),
        ];
        assert_eq!(
            provider_target(&sessions),
            Some(("idle".to_owned(), "Claude".to_owned()))
        );
    }
}
