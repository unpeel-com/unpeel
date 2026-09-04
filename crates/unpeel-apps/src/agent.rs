//! Agent-pane integration for Unpeel apps: detect an agent session in the
//! app's own sidebar group and paste a reference into its input.
//!
//! Any app on Surface (the designer, a diff viewer, a markdown editor) can
//! offer "Send to agent" with three pieces:
//!
//! - [`AdjacentAgent`] — an off-thread prober so menus can *honestly* label
//!   the action ("Send to agent" vs a copy fallback) before any click.
//! - [`send_to_adjacent_agent`] — resolve the best agent pane in the group
//!   and paste text into its input without submitting, so the user wraps
//!   their instruction around it.
//! - [`clipboard_sequence`] — the OSC 52 fallback for when no agent pane is
//!   next door (or the app runs outside a hosted Unpeel session).
//!
//! Everything rides `unpeel-host __mcp__`, the same unified MCP server
//! agents use, so Unpeel enforces the user's write policy for the explicit
//! caller→target pair. The token an app sends is whatever reference format
//! that app's skill documents.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

/// OSC 52 clipboard-copy sequence — the fallback path an app prints to its
/// own terminal when no agent pane is available.
pub fn clipboard_sequence(text: &str) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07")
}

/// Paste `token` into the input of an agent session in this session's own
/// sidebar group — the pane next to the app. The Sessions MCP write policy
/// applies, so the first send to that target may wait for user approval.
/// Returns the receiving session's label.
///
/// Requires running inside a hosted Unpeel session (`UNPEEL_SESSION_ID`)
/// with `unpeel-host` on PATH; callers fall back to [`clipboard_sequence`]
/// on Err.
pub fn send_to_adjacent_agent(token: &str) -> Result<String, String> {
    if std::env::var("UNPEEL_SESSION_ID").is_err() {
        return Err("not inside an Unpeel session".into());
    }
    let mut client = McpClient::spawn()?;
    let (target_id, label) = resolve_adjacent_agent(&mut client)?;
    // submit:false — the token lands in the agent's input so the user can
    // add their instruction around it before pressing Enter.
    client.call_tool(
        "sessions",
        &serde_json::json!({
            "action": "send_text",
            "session_id": target_id,
            "text": format!("{token} "),
            "submit": false,
        }),
    )?;
    Ok(label)
}

/// The `(session_id, label)` of the agent peer "Send to agent" would paste
/// into right now.
fn resolve_adjacent_agent(client: &mut McpClient) -> Result<(String, String), String> {
    let group = client.call_tool(
        "sessions",
        &serde_json::json!({ "action": "list_group", "include_exited": false }),
    )?;
    let sessions = group
        .get("sessions")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    // Preferred path: the agents domain reports which sessions contain a
    // recognized agent runtime *right now* — including one the user typed
    // into a plain shell — so the token cannot land in an agentless prompt.
    // Older unpeel-host builds have no agents tool; the picker falls back
    // to the command-derived provider heuristic.
    let agents = client
        .call_tool("agents", &serde_json::json!({ "action": "list" }))
        .ok()
        .and_then(|listed| {
            listed
                .get("agents")
                .and_then(|value| value.as_array())
                .cloned()
        });
    pick_adjacent_agent(&sessions, agents.as_deref())
        .ok_or_else(|| "no agent session in this group".to_string())
}

/// Pure selection over the two tool payloads, for testability: prefer a
/// settled (idle/done) agent in the group over blocked or busy ones; never
/// pick this session itself or a plain shell.
fn pick_adjacent_agent(
    sessions: &[serde_json::Value],
    agents: Option<&[serde_json::Value]>,
) -> Option<(String, String)> {
    let str_of = |value: &serde_json::Value, key: &str| -> String {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let settled_rank = |status: &str| match status {
        "idle" | "done" => 0,
        "blocked" => 1,
        _ => 2,
    };
    let group_ids: std::collections::HashSet<String> = sessions
        .iter()
        .map(|session| str_of(session, "id"))
        .collect();
    if let Some(agents) = agents {
        let mut agents: Vec<&serde_json::Value> = agents
            .iter()
            .filter(|agent| {
                agent.get("self").and_then(|v| v.as_bool()) != Some(true)
                    && agent
                        .get("agent_ref")
                        .map(|reference| group_ids.contains(&str_of(reference, "session_id")))
                        .unwrap_or(false)
            })
            .collect();
        agents.sort_by_key(|agent| settled_rank(&str_of(agent, "activity_status")));
        if let Some(agent) = agents.first() {
            let reference = agent.get("agent_ref").expect("retained agents have a ref");
            let label = str_of(agent, "label");
            return Some((
                str_of(reference, "session_id"),
                if label.is_empty() {
                    str_of(agent, "runtime_id")
                } else {
                    label
                },
            ));
        }
    }
    // Fallback: sessions *launched as* an agent CLI. `provider` is derived
    // from the launch command and never empty — "shell" is a plain shell,
    // which must not receive the token.
    let mut candidates: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|session| {
            let provider = str_of(session, "provider");
            str_of(session, "state") == "running" && !provider.is_empty() && provider != "shell"
        })
        .collect();
    candidates.sort_by_key(|session| settled_rank(&str_of(session, "activity_status")));
    let target = candidates.first()?;
    let label = str_of(target, "label");
    Some((
        str_of(target, "id"),
        if label.is_empty() {
            str_of(target, "provider")
        } else {
            label
        },
    ))
}

/// A menu's view of the agent pane next door, so its entry can honestly
/// read "Send to agent" or a copy fallback before the click. `label()`
/// answers instantly from the last probe; `refresh()` runs at most one
/// probe at a time off the UI thread (a right-click must never wait on a
/// spawned MCP client). At most one refresh interval stale — the send
/// re-resolves for real anyway.
#[derive(Default)]
pub struct AdjacentAgent {
    label: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    probing: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl AdjacentAgent {
    pub fn new() -> Self {
        Self::default()
    }

    /// Last known peer label; `None` means the menu should offer a copy.
    pub fn label(&self) -> Option<String> {
        self.label.lock().ok()?.clone()
    }

    pub fn refresh(&self) {
        if std::env::var("UNPEEL_SESSION_ID").is_err() {
            return;
        }
        if self.probing.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let label = std::sync::Arc::clone(&self.label);
        let probing = std::sync::Arc::clone(&self.probing);
        std::thread::spawn(move || {
            let found = McpClient::spawn()
                .and_then(|mut client| resolve_adjacent_agent(&mut client))
                .ok()
                .map(|(_, label)| label);
            if let Ok(mut slot) = label.lock() {
                *slot = found;
            }
            probing.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

/// A minimal JSON-RPC client over a spawned `unpeel-host __mcp__` child —
/// the same unified MCP server agents use, so group/write policy is
/// enforced by Unpeel, not reimplemented per app. Public so apps can reach
/// other Unpeel tools (sessions, agents, artifacts) with the same client.
pub struct McpClient {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    pub fn spawn() -> Result<Self, String> {
        let mut child = Command::new("unpeel-host")
            .arg("__mcp__")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("unpeel-host not available: {error}"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let mut client = Self {
            child,
            reader: BufReader::new(stdout),
            next_id: 1,
        };
        client.request(
            "initialize",
            serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {} }),
        )?;
        Ok(client)
    }

    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| "mcp stdin closed".to_string())?;
        writeln!(stdin, "{body}").map_err(|error| format!("mcp write failed: {error}"))?;
        stdin
            .flush()
            .map_err(|error| format!("mcp flush failed: {error}"))?;
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|error| format!("mcp read failed: {error}"))?;
            if read == 0 {
                return Err("mcp server exited".into());
            }
            let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            if message.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!("mcp error: {error}"));
            }
            return Ok(message.get("result").cloned().unwrap_or_default());
        }
    }

    /// tools/call, decoding the text content (Unpeel tools return JSON text)
    /// and turning tool-level errors into Err.
    pub fn call_tool(
        &mut self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let result = self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        )?;
        let text = result
            .pointer("/content/0/text")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            return Err(text);
        }
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Dropping stdin ends the server's stdin loop; reap the child.
        self.child.stdin.take();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_a_settled_group_agent_and_skips_self() {
        let sessions = vec![json!({ "id": "a" }), json!({ "id": "b" })];
        let agents = vec![
            json!({ "self": true, "activity_status": "idle", "label": "me",
                    "agent_ref": { "session_id": "a" } }),
            json!({ "activity_status": "working", "label": "busy claude",
                    "agent_ref": { "session_id": "b" } }),
            json!({ "activity_status": "idle", "label": "ready codex",
                    "agent_ref": { "session_id": "b" } }),
            json!({ "activity_status": "idle", "label": "other group",
                    "agent_ref": { "session_id": "z" } }),
        ];
        assert_eq!(
            pick_adjacent_agent(&sessions, Some(&agents)),
            Some(("b".to_string(), "ready codex".to_string()))
        );
    }

    #[test]
    fn falls_back_to_the_provider_heuristic_and_rejects_shells() {
        let sessions = vec![
            json!({ "id": "s", "state": "running", "provider": "shell",
                    "activity_status": "idle", "label": "zsh" }),
            json!({ "id": "c", "state": "running", "provider": "claude",
                    "activity_status": "busy", "label": "working claude" }),
            json!({ "id": "x", "state": "running", "provider": "codex",
                    "activity_status": "idle", "label": "" }),
        ];
        assert_eq!(
            pick_adjacent_agent(&sessions, None),
            Some(("x".to_string(), "codex".to_string()))
        );
        // Only a shell in the group: nothing to send to.
        assert_eq!(pick_adjacent_agent(&sessions[..1], None), None);
    }
}
