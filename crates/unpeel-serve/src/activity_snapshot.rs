//! Compatibility projection of the canonical Host activity model.
//!
//! `activity-state.json` predates the Host protocol and is still consumed by
//! Sessions MCP and older/background frontends. The workspace worker is its
//! writer whenever it owns lifecycle authority; clients only read it.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::activity::{ActivityEngine, HookState};
use crate::sessions::{SessionRow, SidebarModel, Status};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityStateSignature(Vec<ActivityStateEntrySignature>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActivityStateEntrySignature {
    id: String,
    activity_status: &'static str,
    raw_status: &'static str,
    unread: bool,
    completed: bool,
}

#[derive(Serialize)]
struct ActivityStateFile<'a> {
    version: u64,
    updated_at: u64,
    sessions: std::collections::BTreeMap<&'a str, ActivityStateEntry>,
}

#[derive(Serialize)]
struct ActivityStateEntry {
    activity_status: &'static str,
    raw_status: &'static str,
    unread: bool,
    completed: bool,
    updated_at: u64,
}

pub(crate) fn publish(
    path: &Path,
    model: &SidebarModel,
    unread_ids: &HashSet<String>,
    engine: &ActivityEngine,
    updated_at: u64,
    previous: &mut Option<ActivityStateSignature>,
) -> io::Result<bool> {
    let signature = signature(model, unread_ids, engine);
    if previous.as_ref() == Some(&signature) && path.is_file() {
        return Ok(false);
    }

    let sessions = signature
        .0
        .iter()
        .map(|entry| {
            (
                entry.id.as_str(),
                ActivityStateEntry {
                    activity_status: entry.activity_status,
                    raw_status: entry.raw_status,
                    unread: entry.unread,
                    completed: entry.completed,
                    updated_at,
                },
            )
        })
        .collect();
    let payload = ActivityStateFile {
        version: 1,
        updated_at,
        sessions,
    };
    let bytes = serde_json::to_vec_pretty(&payload).map_err(io::Error::other)?;
    atomic_write(path, &bytes)?;
    *previous = Some(signature);
    Ok(true)
}

/// Carry unread claims across the compatibility-frontend → canonical-Host
/// handoff. Read receipts still win in `derive_unread`, so a stale snapshot
/// cannot resurrect already-observed work.
pub(crate) fn load_unread(path: &Path) -> HashSet<String> {
    let Some(value) = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    else {
        return HashSet::new();
    };
    value
        .get("sessions")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|sessions| sessions.iter())
        .filter(|(_, value)| {
            value
                .get("unread")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .map(|(id, _)| id.clone())
        .collect()
}

fn signature(
    model: &SidebarModel,
    unread_ids: &HashSet<String>,
    engine: &ActivityEngine,
) -> ActivityStateSignature {
    let mut entries = model
        .rows
        .iter()
        .map(|row| {
            let unread = unread_ids.contains(&row.id);
            let (activity_status, raw_status) = status_words(row, unread);
            ActivityStateEntrySignature {
                id: row.id.clone(),
                activity_status,
                raw_status,
                unread,
                completed: engine.hook_owned_state(&row.id) == Some(HookState::Idle),
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    ActivityStateSignature(entries)
}

fn status_words(row: &SessionRow, unread: bool) -> (&'static str, &'static str) {
    match (row.status, unread) {
        (Status::Starting, _) => ("starting", "starting"),
        (Status::Busy, _) => ("working", "busy"),
        (Status::Attention, _) => ("blocked", "attention"),
        (Status::Idle, true) => ("done", "idle"),
        (Status::Idle, false) => ("idle", "idle"),
        (Status::Exited, true) => ("done", "exited"),
        (Status::Exited, false) => ("exited", "exited"),
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = PathBuf::from(parent).join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, status: Status) -> SessionRow {
        SessionRow {
            id: id.into(),
            project_id: "project".into(),
            label: id.into(),
            command: "claude".into(),
            active_runtime_id: None,
            active_app: None,
            resume_available: false,
            archive_available: false,
            resume_agent_available: false,
            running: status != Status::Exited,
            status,
            created_at: 1,
            pinned: false,
            archived: false,
            unread: false,
            latest_alert_body: None,
            cwd: "/tmp".into(),
            activity_at: 1,
            group_id: "project".into(),
            detected_local_urls: Vec::new(),
        }
    }

    #[test]
    fn canonical_snapshot_is_stable_and_preserves_unread_handoff() {
        let directory = std::env::temp_dir().join(format!(
            "unpeel-activity-state-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("activity-state.json");
        let model = SidebarModel {
            rows: vec![row("busy", Status::Busy), row("done", Status::Idle)],
            ..SidebarModel::default()
        };
        let unread = HashSet::from(["done".to_string()]);
        let mut engine = ActivityEngine::default();
        engine.apply_hook_event("done", "Stop", None, std::time::SystemTime::now());
        let mut previous = None;

        assert!(publish(&path, &model, &unread, &engine, 10, &mut previous).unwrap());
        assert!(!publish(&path, &model, &unread, &engine, 20, &mut previous).unwrap());
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["sessions"]["busy"]["activity_status"], "working");
        assert_eq!(value["sessions"]["done"]["activity_status"], "done");
        assert_eq!(value["sessions"]["done"]["completed"], true);
        assert_eq!(load_unread(&path), unread);

        fs::remove_dir_all(directory).unwrap();
    }
}
