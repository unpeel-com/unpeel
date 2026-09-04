//! MCP approval hub: when no app runs, the MCP host's port discovery finds
//! the TUI's hook listener and POSTs its blocking approval requests here
//! (`/mcp/approve-write|browser|computer|app-open`). Requests queue in this hub; the
//! TUI renders the front of the queue as a y/n prompt and paired phones see
//! it as `pendingApprovals` in bootstrap (answered via
//! `/mobile/approvals/answer`) — first answer wins. Approvals persist into
//! the shared `app-state.json` exactly where the app keeps them, so grants
//! survive and both frontends honor them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct PendingApproval {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub caller_session_id: String,
    pub target_session_id: Option<String>,
    pub requested_at: u64,
    responder: Sender<bool>,
}

#[derive(Default)]
pub struct ApprovalHub {
    pending: Mutex<Vec<PendingApproval>>,
    generation: AtomicU64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl ApprovalHub {
    /// Queue a request and block until answered or `timeout` (denied). The
    /// caller is an HTTP handler thread, so blocking here is the contract —
    /// the MCP host is itself blocking on our response.
    pub fn request(
        self: &Arc<Self>,
        kind: &str,
        title: String,
        body: String,
        caller_session_id: String,
        target_session_id: Option<String>,
        timeout: Duration,
    ) -> bool {
        let (tx, rx): (Sender<bool>, Receiver<bool>) = std::sync::mpsc::channel();
        let id = uuid::Uuid::new_v4().to_string();
        if let Ok(mut guard) = self.pending.lock() {
            guard.push(PendingApproval {
                id: id.clone(),
                kind: kind.to_string(),
                title,
                body,
                caller_session_id,
                target_session_id,
                requested_at: now_ms(),
                responder: tx,
            });
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        let approved = rx.recv_timeout(timeout).unwrap_or(false);
        // Drop the entry if it's still queued (timeout path).
        if let Ok(mut guard) = self.pending.lock() {
            let before = guard.len();
            guard.retain(|p| p.id != id);
            if guard.len() != before {
                self.generation.fetch_add(1, Ordering::AcqRel);
            }
        }
        approved
    }

    /// Answer by id (from the TUI keys or the phone). Returns false when the
    /// id is unknown (already answered or timed out).
    pub fn answer(&self, id: &str, approved: bool) -> bool {
        let Ok(mut guard) = self.pending.lock() else {
            return false;
        };
        let Some(index) = guard.iter().position(|p| p.id == id) else {
            return false;
        };
        let entry = guard.remove(index);
        self.generation.fetch_add(1, Ordering::AcqRel);
        entry.responder.send(approved).is_ok()
    }

    /// The front of the queue for the TUI prompt.
    pub fn front(&self) -> Option<(String, String)> {
        let guard = self.pending.lock().ok()?;
        guard.first().map(|p| (p.id.clone(), p.title.clone()))
    }

    /// `pendingApprovals` for the mobile bootstrap (Swift wire dialect).
    pub fn list_json(&self) -> Vec<serde_json::Value> {
        let Ok(guard) = self.pending.lock() else {
            return Vec::new();
        };
        guard
            .iter()
            .map(|p| {
                let mut value = serde_json::json!({
                    "id": p.id,
                    "kind": p.kind,
                    "title": p.title,
                    "body": p.body,
                    "callerSessionID": p.caller_session_id,
                    "requestedAtUnixMs": p.requested_at,
                });
                if let Some(target) = &p.target_session_id {
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("targetSessionID".into(), target.clone().into());
                    }
                }
                value
            })
            .collect()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// Persist a grant into the shared app-state.json exactly where the app
/// keeps it, so the approval outlives this TUI run and the app honors it.
pub fn persist_grant(kind: &str, caller: &str, target: Option<&str>) {
    let _ = unpeel_core::app_state::edit(|root| {
        match kind {
            "write" => {
                let Some(target) = target else { return Ok(()) };
                let map = root
                    .entry("mcp_write_approvals")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(list) = map
                    .as_object_mut()
                    .map(|m| {
                        m.entry(caller.to_string())
                            .or_insert_with(|| serde_json::json!([]))
                    })
                    .and_then(|v| v.as_array_mut())
                {
                    if !list.iter().any(|v| v.as_str() == Some(target)) {
                        list.push(target.into());
                    }
                }
            }
            "browser" | "computer" => {
                let key = if kind == "browser" {
                    "browser_approvals"
                } else {
                    "computer_approvals"
                };
                let list = root.entry(key).or_insert_with(|| serde_json::json!([]));
                if let Some(array) = list.as_array_mut() {
                    if !array.iter().any(|v| v.as_str() == Some(caller)) {
                        array.push(caller.into());
                    }
                }
            }
            "app-open" => {
                let Some(app_id) = target else { return Ok(()) };
                let map = root
                    .entry("mcp_app_open_approvals")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(list) = map
                    .as_object_mut()
                    .map(|m| {
                        m.entry(caller.to_string())
                            .or_insert_with(|| serde_json::json!([]))
                    })
                    .and_then(|v| v.as_array_mut())
                {
                    if !list.iter().any(|v| v.as_str() == Some(app_id)) {
                        list.push(app_id.into());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    });
}

/// Fast-path check against previously persisted grants.
pub fn already_granted(kind: &str, caller: &str, target: Option<&str>) -> bool {
    let Some(state) = std::fs::read(unpeel_core::app_paths::app_state_path())
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
    else {
        return false;
    };
    match kind {
        "write" => target.is_some_and(|t| {
            state
                .get("mcp_write_approvals")
                .and_then(|m| m.get(caller))
                .and_then(|l| l.as_array())
                .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(t)))
        }),
        "browser" | "computer" => {
            let key = if kind == "browser" {
                "browser_approvals"
            } else {
                "computer_approvals"
            };
            state
                .get(key)
                .and_then(|l| l.as_array())
                .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(caller)))
        }
        "app-open" => target.is_some_and(|app_id| {
            state
                .get("mcp_app_open_approvals")
                .and_then(|m| m.get(caller))
                .and_then(|l| l.as_array())
                .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(app_id)))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn generation_advances_for_enqueue_and_answer_snapshots() {
        let hub = Arc::new(ApprovalHub::default());
        let request_hub = Arc::clone(&hub);
        let waiter = thread::spawn(move || {
            request_hub.request(
                "browser",
                "Allow browser access?".into(),
                "A session requested browser access.".into(),
                "caller-session".into(),
                None,
                Duration::from_secs(2),
            )
        });

        let mut queued = None;
        for _ in 0..100 {
            queued = hub.list_json().into_iter().next();
            if queued.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let queued = queued.expect("approval should enter the published snapshot");
        let queued_generation = hub.generation();
        assert!(queued_generation > 0);
        let id = queued["id"].as_str().expect("approval id");

        assert!(hub.answer(id, true));
        assert!(waiter.join().expect("request thread should finish"));
        assert!(hub.list_json().is_empty());
        assert!(hub.generation() > queued_generation);
    }
}
