//! Client-side session lifecycle: spawn / stop / remove / restart-with-resume
//! implemented against the shared on-disk contract, so any frontend (the TUI,
//! future clients) can run sessions without the native app. The native app
//! keeps its own richer paths (overlays, archive bookkeeping, UI carry-over);
//! these ops are the app-independent core:
//!
//! - **stop** talks only to the session's control socket (`kill` command) —
//!   the socket IS the session's identity, so the pid-wrap hazard that makes
//!   raw `kill(-pid, …)` dangerous never arises here.
//! - **restart** mirrors the app's semantics: rewrite the command via
//!   `crate::resume`, stop the old host, delete its dir (with the re-delete
//!   loop that beats the host's racing final manifest write), spawn a fresh
//!   host under a NEW session id carrying the old `created_at`/label.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app_paths;
use crate::session_host::{
    load_manifest, socket_path, write_launch_file, HostedSessionManifest, HostedSessionState,
    SessionHostLaunch,
};
use crate::state::{BrowserAccess, SessionInfo};

const SOCKET_IO_TIMEOUT: Duration = Duration::from_millis(1_000);
// The session host escalates TERM→KILL within ~50ms of the kill command, so
// this wait is really for the exited-manifest PUBLISH, which can lag well
// past 3s on a loaded Host. Must stay under the remote Controller's 10s
// effect timeout (remote archive/stop ride this synchronously).
const STOP_WAIT: Duration = Duration::from_secs(8);
const DIR_DELETE_RETRIES: u32 = 10;
const DIR_DELETE_RETRY_DELAY: Duration = Duration::from_millis(300);

/// A stable, path-safe lock target for one logical Session. Lifecycle locks
/// live outside `app-sessions`: removing a Session directory must not unlink
/// the inode that another process is waiting to lock. Hashing also keeps an
/// untrusted id from becoming a path component.
fn lifecycle_lock_target_at(lock_dir: &std::path::Path, session_id: &str) -> PathBuf {
    use sha2::{Digest, Sha256};

    lock_dir.join(format!("{:x}", Sha256::digest(session_id.as_bytes())))
}

pub(crate) fn lock_session_lifecycle(
    session_id: &str,
) -> Result<crate::app_state::FileLock, String> {
    let home = app_paths::ensure_unpeel_home().map_err(|e| e.to_string())?;
    let lock_dir = home.join("session-lifecycle-locks");
    std::fs::create_dir_all(&lock_dir).map_err(|e| e.to_string())?;
    crate::app_state::lock_exclusive(&lifecycle_lock_target_at(&lock_dir, session_id))
}

fn session_dir(session_id: &str) -> PathBuf {
    app_paths::app_sessions_root().join(session_id)
}

fn socket_command(
    session_id: &str,
    command: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let path = socket_path(session_id);
    let mut stream =
        UnixStream::connect(&path).map_err(|e| format!("connect {}: {e}", path.display()))?;
    stream
        .set_read_timeout(Some(SOCKET_IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(SOCKET_IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut line = serde_json::to_string(&command).map_err(|e| e.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).map_err(|e| e.to_string())?;
    serde_json::from_str(response.trim()).map_err(|e| format!("bad response: {e}"))
}

fn manifest(session_id: &str) -> Result<HostedSessionManifest, String> {
    load_manifest(session_id).ok_or_else(|| format!("no manifest for {session_id}"))
}

/// Runtime-managed storage is Host-authored, but revalidate the persisted
/// path before destructive cleanup so a damaged or hand-edited manifest can
/// never point removal outside the active Unpeel home. Older manifests fall
/// back to the runtime adapter's command parser.
fn managed_storage_for_manifest(manifest: &HostedSessionManifest) -> Option<PathBuf> {
    let home = app_paths::unpeel_home();
    let candidate = manifest
        .managed_storage_path
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| crate::resume::managed_storage_path(&manifest.session.command, &home))?;
    let relative = candidate.strip_prefix(&home).ok()?;
    let mut components = relative.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    if candidate.exists() {
        let canonical_home = home.canonicalize().ok()?;
        let canonical_candidate = candidate.canonicalize().ok()?;
        if !canonical_candidate.starts_with(canonical_home) {
            return None;
        }
    }
    Some(candidate)
}

/// Provider-neutral lookup for clients that still own a legacy removal path.
/// New manifests carry the Host-validated path directly; old manifests are
/// interpreted by the runtime adapter from their saved command.
pub fn managed_storage_path_for_session(session_id: &str) -> Option<String> {
    let manifest = load_manifest(session_id)?;
    managed_storage_for_manifest(&manifest).map(|path| path.to_string_lossy().to_string())
}

/// Non-destructive stop: ask the host (via its own socket) to SIGTERM its
/// process group, then wait for the exited manifest. The session dir — and
/// with it the conversation — survives; restart resumes it.
pub fn stop_session(session_id: &str) -> Result<(), String> {
    let _lifecycle_lock = lock_session_lifecycle(session_id)?;
    stop_session_unlocked(session_id)
}

/// Caller holds this Session's lifecycle lock. Kept separate so archive,
/// remove, and restart can compose the same stop without recursively flocking
/// the same inode (which is not a portable re-entrant lock).
fn stop_session_unlocked(session_id: &str) -> Result<(), String> {
    let socket_result = socket_command(session_id, serde_json::json!({"type": "kill"}));
    match &socket_result {
        Ok(response) if response.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => {
            // A host is actually going down: peers should drop the row's
            // "running" state now, not on their next poll.
            crate::state_bus::announce(crate::state_bus::Change::Lifecycle, own_listener_port());
        }
        Ok(response) => {
            return Err(response
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("session host rejected stop")
                .to_owned());
        }
        // A missing socket is only "already stopped" when disk agrees. A
        // running manifest still names a child we may be able to stop safely.
        Err(_) => {}
    }

    if wait_for_exited_manifest(session_id, STOP_WAIT) {
        return Ok(());
    }
    let socket_note = socket_result
        .err()
        .map(|error| format!("; socket request failed: {error}"))
        .unwrap_or_default();
    Err(format!(
        "session {session_id} host did not publish an exited manifest within {}ms{socket_note}",
        STOP_WAIT.as_millis()
    ))
}

fn wait_for_exited_manifest(session_id: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match load_manifest(session_id) {
            Some(m) if m.state == HostedSessionState::Exited => return true,
            None => return true,
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    false
}

/// Archive marker in the session dir — the shared-contract home for
/// "stopped and filed away", writable by any frontend. The native app's
/// UserDefaults overlay predates this; readers should honor both until the
/// app adopts the marker.
pub const ARCHIVE_MARKER: &str = "archived.json";

/// Whether this launch names a runtime with a provider-owned resume recipe.
/// This is only the static half of archive eligibility: callers that have a
/// Session must use [`can_archive_session`] so a CLI that exited before it
/// created a real conversation is removed instead of being filed away.
pub fn can_archive_command(command: &str) -> bool {
    crate::resume::can_resume(command)
}

fn managed_storage_has_resume_data(manifest: &HostedSessionManifest) -> bool {
    let Some(root) = managed_storage_for_manifest(manifest).filter(|path| path.is_dir()) else {
        return false;
    };
    // The Host creates the managed directory before launching the runtime, so
    // directory existence alone is not evidence. A bounded, symlink-free walk
    // looks for a provider-created file without letting a damaged store turn a
    // sidebar capability check into an unbounded filesystem crawl.
    let mut pending = vec![root];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 256 {
                return false;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_file() {
                return true;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    false
}

fn has_real_provider_lifecycle(session_id: &str) -> bool {
    let raw = match std::fs::read(session_dir(session_id).join("last-hook-event.json")) {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let event = serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| {
            value
                .get("hook_event_name")
                .or_else(|| value.get("hookEventName"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    matches!(
        event.as_deref(),
        Some("Start" | "UserPromptSubmit" | "Stop" | "StopFailure" | "PermissionRequest")
    )
}

/// A provider-owned transcript is the strongest proof that a launch crossed
/// from a command we intended to run into a real provider Session. The path
/// may be known before the first lifecycle hook reaches Unpeel, so checking
/// the actual non-empty file avoids a transient false "Remove" affordance
/// without treating a merely pre-minted provider id as resumable.
fn provider_transcript_has_resume_data(path: Option<&str>) -> bool {
    let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
        return false;
    };
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// True only after this particular managed runtime has produced durable
/// resume state. Provider IDs minted before launch are not sufficient: the
/// provider must have written a transcript, emitted a real lifecycle event,
/// or populated managed per-Session storage such as Pi's.
pub fn can_archive_manifest(manifest: &HostedSessionManifest) -> bool {
    if !can_archive_command(&manifest.session.command) {
        return false;
    }
    if managed_storage_has_resume_data(manifest) {
        return true;
    }
    let (marker_id, marker_transcript) = provider_session_marker(&manifest.session.id);
    let provider_id = marker_id.or_else(|| {
        manifest
            .provider_session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    });
    let transcript = marker_transcript
        .as_deref()
        .or(manifest.provider_transcript_path.as_deref());
    provider_id.is_some()
        && (provider_transcript_has_resume_data(transcript)
            || has_real_provider_lifecycle(&manifest.session.id))
}

/// Load and evaluate the Host-owned archive capability for one Session.
pub fn can_archive_session(session_id: &str) -> bool {
    load_manifest(session_id).is_some_and(|manifest| can_archive_manifest(&manifest))
}

/// Non-destructive stop-and-archive: stop the host and stamp the marker.
/// The session dir (and conversation) survives; restart resumes it — and
/// since restart replaces the dir, the marker dies with it ("restarting an
/// archived session is bringing it back").
pub fn archive_session(session_id: &str) -> Result<(), String> {
    let _lifecycle_lock = lock_session_lifecycle(session_id)?;
    // Deliberately ungated: Archive is the non-destructive stop-and-file
    // verb for ANY session. Whether the filed conversation later offers
    // Restore & Resume or plain Restore is decided at restore time by
    // `can_archive_manifest`'s resume evidence — never by refusing to file.
    stop_session_unlocked(session_id)?;
    let dir = session_dir(session_id);
    if !dir.exists() {
        return Err(format!("no session dir for {session_id}"));
    }
    // Stamped: this is the user-verb path (CLI / TUI archive), never a
    // sweep — the row floats to the top of the fixed archive section.
    let marker = serde_json::json!({ "archived_at": now_ms(), "stamped": true });
    let tmp = dir.join(".archived.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(&marker).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(ARCHIVE_MARKER)).map_err(|e| e.to_string())?;
    crate::state_bus::announce(
        crate::state_bus::Change::SessionMarkers,
        own_listener_port(),
    );
    Ok(())
}

/// Bring an archived session back to the sidebar (no restart).
pub fn restore_session(session_id: &str) -> Result<(), String> {
    let _lifecycle_lock = lock_session_lifecycle(session_id)?;
    let _ = std::fs::remove_file(session_dir(session_id).join(ARCHIVE_MARKER));
    crate::state_bus::announce(
        crate::state_bus::Change::SessionMarkers,
        own_listener_port(),
    );
    Ok(())
}

/// Display-group assignment override (`project-override.json` in the session
/// dir): the session renders under this project instead of its
/// manifest `project_id`. This is how "Move to <group>" works without
/// touching the manifest (the host owns manifest writes). Shared contract
/// with the desktop — top-level `project_id` (string) plus `moved_at` (ms
/// epoch); readers tolerate unknown extra keys. Removing the marker moves
/// the session back to its manifest project.
pub const PROJECT_OVERRIDE_MARKER: &str = "project-override.json";

/// File a session into a display group: stamp the override marker.
pub fn set_project_override(session_id: &str, project_id: &str) -> Result<(), String> {
    let dir = session_dir(session_id);
    if !dir.exists() {
        return Err(format!("no session dir for {session_id}"));
    }
    let body = serde_json::json!({ "project_id": project_id, "moved_at": now_ms() });
    let tmp = dir.join(".project-override.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&body).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(PROJECT_OVERRIDE_MARKER)).map_err(|e| e.to_string())?;
    crate::state_bus::announce(
        crate::state_bus::Change::SessionMarkers,
        own_listener_port(),
    );
    Ok(())
}

/// Move the session back to its manifest project: drop the marker.
pub fn clear_project_override(session_id: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(session_dir(session_id).join(PROJECT_OVERRIDE_MARKER));
    crate::state_bus::announce(
        crate::state_bus::Change::SessionMarkers,
        own_listener_port(),
    );
    Ok(())
}

/// The override target, if any. Callers must treat a target that no longer
/// exists as absent (stale marker → fall back to the manifest project).
pub fn project_override_marker(session_id: &str) -> Option<String> {
    let raw = std::fs::read(session_dir(session_id).join(PROJECT_OVERRIDE_MARKER)).ok()?;
    serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()?
        .get("project_id")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

const PROVIDER_MARKER: &str = "provider-session.json";

/// Hook-captured provider conversation metadata: which provider
/// conversation this session IS, so restart can resume it and transcript
/// reads can find it. Written by whichever frontend receives the hook
/// broadcast; the manifest's copy stays as fallback (the host owns
/// manifest writes, so injecting there races its heartbeat rewrites — the
/// marker has a single-writer semantic: latest capture wins).
///
/// Deliberately does NOT announce on the state bus: these writes happen on
/// every hook event, and every frontend already heard the hook itself on
/// the same port broadcast — announcing would double every ping.
/// Returns whether the marker changed — `true` means the session's provider
/// conversation identity moved (an in-tool resume/clear, or first capture),
/// which is the callers' trigger for transcript-based auto-titling.
pub fn set_provider_session(
    session_id: &str,
    provider_session_id: Option<&str>,
    transcript_path: Option<&str>,
) -> Result<bool, String> {
    if provider_session_id.is_none() && transcript_path.is_none() {
        return Ok(false);
    }
    let dir = session_dir(session_id);
    if !dir.exists() {
        return Err(format!("no session dir for {session_id}"));
    }
    // Merge: an id-only event must not erase a previously captured
    // transcript path, and vice versa.
    let (current_id, current_path) = provider_session_marker(session_id);
    let next_id = provider_session_id
        .map(str::to_owned)
        .or(current_id.clone());
    let next_path = transcript_path.map(str::to_owned).or(current_path.clone());
    if next_id == current_id && next_path == current_path {
        return Ok(false); // hooks fire constantly; unchanged must cost nothing
    }
    let mut body = serde_json::Map::new();
    if let Some(id) = &next_id {
        body.insert("provider_session_id".into(), serde_json::json!(id));
    }
    if let Some(path) = &next_path {
        body.insert("provider_transcript_path".into(), serde_json::json!(path));
    }
    body.insert("captured_at".into(), serde_json::json!(now_ms()));
    let tmp = dir.join(".provider-session.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&body).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(PROVIDER_MARKER)).map_err(|e| e.to_string())?;
    Ok(true)
}

/// (provider_session_id, provider_transcript_path) from the marker.
pub fn provider_session_marker(session_id: &str) -> (Option<String>, Option<String>) {
    let raw = match std::fs::read(session_dir(session_id).join(PROVIDER_MARKER)) {
        Ok(raw) => raw,
        Err(_) => return (None, None),
    };
    let value: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => return (None, None),
    };
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    (
        field("provider_session_id"),
        field("provider_transcript_path"),
    )
}

/// The recency stamp: `archived_at` only for USER-initiated archives.
/// Sweep-filed sessions (`stamped: false`) return None here — they are
/// archived, but must neither float to the top of the archive section nor
/// linger in the visible list. A marker without the field predates it and
/// reads as stamped.
pub fn archive_stamp(session_id: &str) -> Option<u64> {
    let raw = std::fs::read(session_dir(session_id).join(ARCHIVE_MARKER)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if value.get("stamped").and_then(|v| v.as_bool()) == Some(false) {
        return None;
    }
    value.get("archived_at")?.as_u64()
}

/// Marker read for sidebar builders: Some(archived_at) when filed.
pub fn archived_marker(session_id: &str) -> Option<u64> {
    let raw = std::fs::read(session_dir(session_id).join(ARCHIVE_MARKER)).ok()?;
    serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()?
        .get("archived_at")?
        .as_u64()
}

/// Shared-contract title override (`title.json` in the session dir): any
/// frontend can rename; readers prefer it over the manifest label. The
/// native app's UserDefaults title overlay predates this marker.
pub fn set_title(session_id: &str, title: &str) -> Result<(), String> {
    let dir = session_dir(session_id);
    if !dir.exists() {
        return Err(format!("no session dir for {session_id}"));
    }
    let tmp = dir.join(".title.json.tmp");
    let body = serde_json::json!({ "title": title, "updated_at": now_ms() });
    std::fs::write(&tmp, serde_json::to_vec(&body).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("title.json")).map_err(|e| e.to_string())?;
    crate::state_bus::announce(
        crate::state_bus::Change::SessionMarkers,
        own_listener_port(),
    );
    Ok(())
}

pub fn title_marker(session_id: &str) -> Option<String> {
    let raw = std::fs::read(session_dir(session_id).join("title.json")).ok()?;
    serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()?
        .get("title")?
        .as_str()
        .map(str::to_owned)
}

/// Marker beside a Session recording that a phone currently owns its PTY
/// grid (`POST /mobile/resize-desktop`). The Host publishes it in the
/// session summary so a desktop Controller can letterbox its surface to the
/// same grid and offer "fit to desktop"; the phone's `clear` verb removes
/// it. A file, like `title.json`, so a worker restart keeps an active fit.
pub const PHONE_FIT_MARKER: &str = "phone-fit.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhoneFitMarker {
    pub columns: u16,
    pub rows: u16,
    pub since_unix_ms: u64,
}

pub fn read_phone_fit_marker_in(dir: &std::path::Path) -> Option<PhoneFitMarker> {
    let raw = std::fs::read(dir.join(PHONE_FIT_MARKER)).ok()?;
    serde_json::from_slice::<PhoneFitMarker>(&raw)
        .ok()
        .filter(|marker| marker.columns >= 2 && marker.rows >= 2)
}

pub fn write_phone_fit_marker_in(
    dir: &std::path::Path,
    marker: &PhoneFitMarker,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(marker).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(".{PHONE_FIT_MARKER}.tmp"));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, dir.join(PHONE_FIT_MARKER))
}

pub fn clear_phone_fit_marker_in(dir: &std::path::Path) -> bool {
    std::fs::remove_file(dir.join(PHONE_FIT_MARKER)).is_ok()
}

pub fn phone_fit_marker(session_id: &str) -> Option<PhoneFitMarker> {
    read_phone_fit_marker_in(&session_dir(session_id))
}

/// Pin or unpin a Session through the shared `app-state.json` contract.
///
/// The mutation is idempotent: retries collapse every stale duplicate before
/// inserting one canonical entry, and an existing `pinned_at` is retained so
/// a lost Controller response cannot reorder the sidebar on retry. A valid
/// project override wins when the Session currently lives in a plain group;
/// stale overrides fall back to the manifest project.
pub fn set_pinned(session_id: &str, pinned: bool) -> Result<(), String> {
    let hosted = manifest(session_id)?;
    let manifest_project_id = hosted.session.project_id;
    let override_project_id = project_override_marker(session_id);

    crate::app_state::edit(|state| {
        let override_is_known = override_project_id.as_ref().is_some_and(|candidate| {
            state
                .get("projects")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|projects| {
                    projects.iter().any(|project| {
                        project.get("id").and_then(serde_json::Value::as_str)
                            == Some(candidate.as_str())
                    })
                })
        });
        let project_id = override_project_id
            .as_ref()
            .filter(|_| override_is_known)
            .cloned()
            .unwrap_or_else(|| manifest_project_id.clone());

        let grouped = state
            .entry("pinned_sessions")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .ok_or("app-state.json pinned_sessions is not an object")?;
        let mut retained_pinned_at = None::<u64>;
        for entries in grouped.values_mut() {
            let entries = entries
                .as_array_mut()
                .ok_or("app-state.json pinned_sessions entry is not a list")?;
            entries.retain(|entry| {
                let matches =
                    entry.get("session_id").and_then(serde_json::Value::as_str) == Some(session_id);
                if matches {
                    if let Some(value) = entry.get("pinned_at").and_then(serde_json::Value::as_u64)
                    {
                        retained_pinned_at =
                            Some(retained_pinned_at.map_or(value, |current| current.max(value)));
                    }
                }
                !matches
            });
        }
        grouped.retain(|_, entries| entries.as_array().is_none_or(|entries| !entries.is_empty()));

        if pinned {
            let entry = serde_json::json!({
                "key": crate::state::pinned_sidebar_session_key(session_id),
                "project_id": project_id,
                "session_id": session_id,
                "pinned_at": retained_pinned_at.unwrap_or_else(now_ms),
            });
            grouped
                .entry(project_id)
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
                .ok_or("app-state.json pinned_sessions entry is not a list")?
                .push(entry);
        }
        Ok(())
    })
}

/// Pin or unpin one plain sidebar group.
///
/// The marker lives on the group's project record so removing it reveals the
/// unchanged manual mixed-row order underneath. Retries retain the original
/// timestamp and therefore never reshuffle other pinned groups.
pub fn set_group_pinned(project_id: &str, pinned: bool) -> Result<(), String> {
    crate::app_state::edit(|state| set_group_pinned_in_state(state, project_id, pinned))
}

fn set_group_pinned_in_state(
    state: &mut serde_json::Map<String, serde_json::Value>,
    project_id: &str,
    pinned: bool,
) -> Result<(), String> {
    let project_id = project_id.trim();
    if project_id.is_empty() {
        return Err("invalid group id".into());
    }
    let projects = state
        .get_mut("projects")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("app-state.json has no projects array")?;
    let project = projects
        .iter_mut()
        .find(|project| project.get("id").and_then(serde_json::Value::as_str) == Some(project_id))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("unknown group")?;
    let is_plain_group = project
        .get("is_folder")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && project
            .get("parent_project_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|parent| !parent.trim().is_empty())
        && project
            .get("worktree_branch")
            .is_none_or(serde_json::Value::is_null);
    if !is_plain_group {
        return Err("only plain groups can be pinned".into());
    }
    if pinned {
        let pinned_at = project
            .get("pinned_at")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(now_ms);
        project.insert("pinned_at".into(), pinned_at.into());
    } else {
        project.remove("pinned_at");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LegacyPinMove {
    session_id: String,
    group_id: String,
    root_id: String,
    pinned_at: u64,
}

/// Convert the retired pin model into ordinary sidebar organization.
///
/// Every top-level project that owns at least one readable legacy pin gets
/// one plain child group named `Pinned`, ordered before its other child
/// folders. Each Session is filed there through the existing shared
/// `project-override.json` marker, then (and only then) its pin record is
/// removed. The operation is intentionally idempotent: an interrupted run
/// leaves either the old pin, the new marker, or both, and the next scan
/// finishes the same conversion without creating another group.
///
/// `session.pin.set` remains a compatible Host operation for older
/// Controllers. A current Host calls this migration whenever pins are
/// observed, so a late write from an older client is folded into the same
/// ordinary group on the next scan.
pub fn migrate_legacy_pins_to_groups() -> Result<usize, String> {
    let mut current = crate::app_state::load()?;
    let entries = legacy_pin_entries(&current);
    if entries.is_empty() {
        return Ok(0);
    }

    // Old pin rows can outlive a removed or moved Session. Do not let those
    // tombstones manufacture an empty Pinned group: there is no Session that
    // could ever receive the group marker. Retiring the dead rows also keeps
    // current frontends from retrying the same impossible migration forever.
    let missing_ids = entries
        .iter()
        .filter(|(_, session_id, _)| !session_dir(session_id).is_dir())
        .map(|(_, session_id, _)| session_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let retired = missing_ids.len();
    if !missing_ids.is_empty() {
        let missing = missing_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        crate::app_state::edit(|state| {
            let Some(pins) = state.get_mut("pinned_sessions") else {
                return Ok(());
            };
            remove_migrated_pin_entries(pins, &missing);
            Ok(())
        })?;
        current = crate::app_state::load()?;
    }

    if legacy_pin_entries(&current).is_empty() {
        prune_unreferenced_legacy_pin_groups()?;
        return Ok(retired);
    }

    // First make every target group durable. Marker writes below may then be
    // retried safely even if the process exits before the pins are cleared.
    let planned = crate::app_state::edit(prepare_legacy_pin_groups)?;
    if planned.is_empty() {
        if retired > 0 {
            prune_unreferenced_legacy_pin_groups()?;
        }
        return Ok(retired);
    }

    let mut moved = Vec::new();
    for item in planned {
        if set_project_override(&item.session_id, &item.group_id).is_ok() {
            moved.push(item);
        }
    }
    if moved.is_empty() {
        if retired > 0 {
            prune_unreferenced_legacy_pin_groups()?;
        }
        return Ok(retired);
    }

    let moved_ids = moved
        .iter()
        .map(|item| item.session_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    crate::app_state::edit(|state| {
        let Some(pins) = state.get_mut("pinned_sessions") else {
            return Ok(());
        };
        remove_migrated_pin_entries(pins, &moved_ids);
        Ok(())
    })?;

    // Preserve the old pinned-row order inside the new group. If the user
    // already had a Pinned group, migrated rows lead its existing manual
    // order and duplicates collapse.
    let mut by_group = std::collections::HashMap::<String, Vec<&LegacyPinMove>>::new();
    for item in &moved {
        by_group
            .entry(item.group_id.clone())
            .or_default()
            .push(item);
    }
    for (group_id, mut items) in by_group {
        items.sort_by(|left, right| {
            left.pinned_at
                .cmp(&right.pinned_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        let mut seen = std::collections::HashSet::new();
        let mut order = items
            .into_iter()
            .map(|item| item.session_id.clone())
            .filter(|id| seen.insert(id.clone()))
            .collect::<Vec<_>>();
        order.extend(
            session_order(&group_id)
                .into_iter()
                .filter(|id| seen.insert(id.clone())),
        );
        set_session_order(&group_id, &order)?;
    }

    put_migrated_pin_groups_first(&moved)?;
    if retired > 0 {
        prune_unreferenced_legacy_pin_groups()?;
    }
    Ok(retired + moved_ids.len())
}

fn legacy_pinned_group_id(root_id: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = format!("{:x}", Sha256::digest(root_id.as_bytes()));
    format!("legacy-pinned-{}", &digest[..16])
}

/// Remove only groups minted by the legacy-pin migration, and only when no
/// Session or child project references them. A user-created empty group named
/// `Pinned` remains a valid drag target and must never be swept up here.
fn prune_unreferenced_legacy_pin_groups() -> Result<usize, String> {
    let referenced = referenced_project_ids();
    let removed = crate::app_state::edit(|state| {
        Ok(remove_unreferenced_legacy_pin_groups(state, &referenced))
    })?;
    if removed.is_empty() {
        return Ok(0);
    }

    let removed_set = removed
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut order = project_order();
    let before = order.len();
    order.retain(|id| !removed_set.contains(id.as_str()));
    if order.len() != before {
        set_project_order(&order)?;
    }
    Ok(removed.len())
}

fn referenced_project_ids() -> std::collections::HashSet<String> {
    let mut referenced = std::collections::HashSet::new();
    let Ok(entries) = std::fs::read_dir(app_paths::app_sessions_root()) else {
        return referenced;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(raw) = std::fs::read(path.join(PROJECT_OVERRIDE_MARKER)) {
            if let Some(project_id) = serde_json::from_slice::<serde_json::Value>(&raw)
                .ok()
                .and_then(|value| value.get("project_id")?.as_str().map(str::to_owned))
            {
                referenced.insert(project_id);
            }
        }
        if let Ok(raw) = std::fs::read(path.join("manifest.json")) {
            if let Some(project_id) = serde_json::from_slice::<serde_json::Value>(&raw)
                .ok()
                .and_then(|value| {
                    value
                        .get("session")?
                        .get("project_id")?
                        .as_str()
                        .map(str::to_owned)
                })
            {
                referenced.insert(project_id);
            }
        }
    }
    referenced
}

fn remove_unreferenced_legacy_pin_groups(
    state: &mut serde_json::Map<String, serde_json::Value>,
    referenced: &std::collections::HashSet<String>,
) -> Vec<String> {
    let Some(projects) = state
        .get_mut("projects")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Vec::new();
    };
    let parent_ids = projects
        .iter()
        .filter_map(|project| {
            project
                .get("parent_project_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect::<std::collections::HashSet<_>>();
    let removable = projects
        .iter()
        .filter_map(|project| {
            let id = project.get("id")?.as_str()?;
            let parent = project.get("parent_project_id")?.as_str()?;
            let is_reserved_id = id == legacy_pinned_group_id(parent);
            let is_plain_pinned_group = project
                .get("is_folder")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                && project.get("worktree_branch").is_none()
                && project
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("Pinned"));
            (is_reserved_id
                && is_plain_pinned_group
                && !referenced.contains(id)
                && !parent_ids.contains(id))
            .then(|| {
                (
                    id.to_owned(),
                    parent.to_owned(),
                    project
                        .get("sort_order")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                )
            })
        })
        .collect::<Vec<_>>();
    if removable.is_empty() {
        return Vec::new();
    }

    let removed_ids = removable
        .iter()
        .map(|(id, _, _)| id.as_str())
        .collect::<std::collections::HashSet<_>>();
    projects.retain(|project| {
        project
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|id| !removed_ids.contains(id))
    });
    for (_, parent, removed_order) in &removable {
        for sibling in projects.iter_mut().filter(|project| {
            project
                .get("parent_project_id")
                .and_then(serde_json::Value::as_str)
                == Some(parent.as_str())
        }) {
            let order = sibling
                .get("sort_order")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if order > *removed_order {
                sibling["sort_order"] = serde_json::json!(order - 1);
            }
        }
    }
    removable.into_iter().map(|(id, _, _)| id).collect()
}

fn legacy_pin_entries(state: &serde_json::Value) -> Vec<(String, String, u64)> {
    let Some(pins) = state.get("pinned_sessions") else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut append = |bucket: Option<&str>, value: &serde_json::Value| {
        let Some(session_id) = value
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let Some(project_id) = value
            .get("project_id")
            .and_then(serde_json::Value::as_str)
            .or(bucket)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        entries.push((
            project_id.to_string(),
            session_id.to_string(),
            value
                .get("pinned_at")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        ));
    };
    match pins {
        serde_json::Value::Object(grouped) => {
            for (project_id, values) in grouped {
                if let Some(values) = values.as_array() {
                    for value in values {
                        append(Some(project_id), value);
                    }
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                append(None, value);
            }
        }
        _ => {}
    }
    entries
}

fn prepare_legacy_pin_groups(
    state: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<LegacyPinMove>, String> {
    let entries = legacy_pin_entries(&serde_json::Value::Object(state.clone()));
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let projects = state
        .get_mut("projects")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("app-state.json has no projects array")?;

    let parent_by_id = projects
        .iter()
        .filter_map(|project| {
            let id = project.get("id")?.as_str()?.to_string();
            let parent = project
                .get("parent_project_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            Some((id, parent))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let root_for = |project_id: &str| -> Option<String> {
        if !parent_by_id.contains_key(project_id) {
            return None;
        }
        let mut current = project_id.to_string();
        let mut seen = std::collections::HashSet::new();
        while seen.insert(current.clone()) {
            match parent_by_id.get(&current).cloned().flatten() {
                Some(parent) => current = parent,
                None => return Some(current),
            }
        }
        None
    };

    let roots = entries
        .iter()
        .filter_map(|(project_id, _, _)| root_for(project_id))
        .collect::<std::collections::HashSet<_>>();
    let mut group_by_root = std::collections::HashMap::new();
    for root_id in roots {
        let existing = projects.iter().find_map(|project| {
            let is_direct_child = project
                .get("parent_project_id")
                .and_then(serde_json::Value::as_str)
                == Some(root_id.as_str());
            let is_plain_group = project
                .get("is_folder")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                && project.get("worktree_branch").is_none();
            let is_pinned = project
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case("Pinned"));
            (is_direct_child && is_plain_group && is_pinned)
                .then(|| project.get("id")?.as_str().map(str::to_string))
                .flatten()
        });
        let group_id = if let Some(existing) = existing {
            existing
        } else {
            let id = legacy_pinned_group_id(&root_id);
            if projects.iter().any(|project| {
                project.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
            }) {
                return Err(format!(
                    "reserved Pinned group id already exists for {root_id}"
                ));
            }
            let root = projects
                .iter()
                .find(|project| {
                    project.get("id").and_then(serde_json::Value::as_str) == Some(root_id.as_str())
                })
                .ok_or_else(|| format!("missing root project {root_id}"))?;
            let path = root
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let workspace_id = root
                .get("workspace_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            for sibling in projects.iter_mut().filter(|project| {
                project
                    .get("parent_project_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(root_id.as_str())
            }) {
                let next = sibling
                    .get("sort_order")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .saturating_add(1);
                sibling["sort_order"] = serde_json::json!(next);
            }
            let mut group = serde_json::json!({
                "id": id,
                "name": "Pinned",
                "path": path,
                "parent_project_id": root_id,
                "sort_order": 0,
                "is_folder": true,
            });
            if let Some(workspace_id) = workspace_id {
                group["workspace_id"] = workspace_id.into();
            }
            projects.push(group);
            id
        };
        group_by_root.insert(root_id, group_id);
    }

    Ok(entries
        .into_iter()
        .filter_map(|(project_id, session_id, pinned_at)| {
            let root_id = root_for(&project_id)?;
            let group_id = group_by_root.get(&root_id)?.clone();
            Some(LegacyPinMove {
                session_id,
                group_id,
                root_id,
                pinned_at,
            })
        })
        .collect())
}

fn remove_migrated_pin_entries(
    pins: &mut serde_json::Value,
    moved_ids: &std::collections::HashSet<&str>,
) {
    let retain = |entry: &serde_json::Value| {
        entry
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|id| !moved_ids.contains(id))
    };
    match pins {
        serde_json::Value::Object(grouped) => {
            for values in grouped.values_mut() {
                if let Some(values) = values.as_array_mut() {
                    values.retain(retain);
                }
            }
            grouped.retain(|_, values| values.as_array().is_none_or(|values| !values.is_empty()));
        }
        serde_json::Value::Array(values) => values.retain(retain),
        _ => {}
    }
}

fn put_migrated_pin_groups_first(moved: &[LegacyPinMove]) -> Result<(), String> {
    let state = crate::app_state::load()?;
    let projects = state
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .ok_or("app-state.json has no projects array")?;
    let all_ids = projects
        .iter()
        .filter_map(|project| project.get("id")?.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    let parent_by_id = projects
        .iter()
        .filter_map(|project| {
            Some((
                project.get("id")?.as_str()?.to_string(),
                project
                    .get("parent_project_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut order = project_order();
    for id in &all_ids {
        if !order.contains(id) {
            order.push(id.clone());
        }
    }
    let groups = moved
        .iter()
        .map(|item| (item.root_id.clone(), item.group_id.clone()))
        .collect::<std::collections::HashSet<_>>();
    for (root_id, group_id) in groups {
        order.retain(|id| id != &group_id);
        let first_sibling = order.iter().position(|id| {
            parent_by_id.get(id).and_then(|parent| parent.as_deref()) == Some(root_id.as_str())
        });
        let insertion = first_sibling
            .or_else(|| {
                order
                    .iter()
                    .position(|id| id == &root_id)
                    .map(|index| index + 1)
            })
            .unwrap_or(order.len());
        order.insert(insertion, group_id);
    }
    set_project_order(&order)
}

/// Shared-contract read receipt (`read.json`): when a frontend last showed
/// this session to the user. Unread is then derived — a session that
/// settled after its read receipt has something new — so the desktop, the
/// TUI, and the phone agree without one of them owning the flag. The
/// native app's in-memory `unreadSessionIDs` predates this marker.
pub fn mark_read(session_id: &str) -> Result<(), String> {
    let dir = session_dir(session_id);
    if !dir.exists() {
        return Err(format!("no session dir for {session_id}"));
    }
    let tmp = dir.join(".read.json.tmp");
    let body = serde_json::json!({ "read_at": now_ms() });
    std::fs::write(&tmp, serde_json::to_vec(&body).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("read.json")).map_err(|e| e.to_string())?;
    crate::state_bus::announce(
        crate::state_bus::Change::SessionMarkers,
        own_listener_port(),
    );
    Ok(())
}

pub fn read_marker(session_id: &str) -> Option<u64> {
    let raw = std::fs::read(session_dir(session_id).join("read.json")).ok()?;
    serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()?
        .get("read_at")?
        .as_u64()
}

/// Last real activity for a session.
///
/// `output.bin` is NOT a usable signal on its own: an idle full-screen
/// agent repaints constantly (spinners, cursor blinks), and a resize on
/// selection makes it redraw too — so its mtime is ~now for almost every
/// running session. Hook-capable tools have a truthful signal in the
/// durable hook seed (a real lifecycle event); only fall back to output for
/// tools that fire no hooks at all.
pub fn last_activity_ms(session_id: &str, command: &str) -> Option<u64> {
    let dir = session_dir(session_id);
    let mtime = |name: &str| -> Option<u64> {
        let modified = std::fs::metadata(dir.join(name)).ok()?.modified().ok()?;
        Some(modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
    };
    if crate::integrations::uses_hook_port(crate::integrations::command_head(command)) {
        // A hook-capable session with no seed yet has simply never had a
        // turn — its creation time is the honest answer.
        return mtime("last-hook-event.json");
    }
    // The host's parsed-screen change stamp beats output.bin's mtime: the
    // text only changes when the screen really shows something new, so it
    // stays put through idle repaint loops. Older hosts don't write it.
    screen_changed_at_ms(session_id).or_else(|| mtime("output.bin"))
}

/// Canonical "latest lifecycle" timestamp used by a sidebar group sorted as
/// Recently updated.
///
/// Creation is the floor. While a session is live, command-aware real
/// activity is the only signal: hook seeds for hook-capable agents, otherwise
/// parsed-screen changes with the legacy output fallback. Once the manifest
/// is definitively exited, its final `updated_at` also counts so a process
/// exit is visible as the latest lifecycle event. Callers must never pass a
/// running manifest's heartbeat-driven `updated_at` here.
pub fn latest_lifecycle_ms(
    session_id: &str,
    command: &str,
    created_at: u64,
    exited_at: Option<u64>,
) -> u64 {
    created_at
        .max(last_activity_ms(session_id, command).unwrap_or(0))
        .max(exited_at.unwrap_or(0))
}

/// The session manifest's `screen_changed_at` — when the host last saw the
/// parsed screen TEXT change. None for manifests from hosts that predate
/// the field (callers fall back to output.bin signals).
pub fn screen_changed_at_ms(session_id: &str) -> Option<u64> {
    let raw = std::fs::read(session_dir(session_id).join("manifest.json")).ok()?;
    serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()?
        .get("screen_changed_at")?
        .as_u64()
}

/// Shared ⌘K/^K recency for callers that do not already hold a manifest:
/// command-aware real activity with `created_at` as the floor. Frontends that
/// have a Session row use its canonical latest-lifecycle stamp instead so an
/// exited manifest's final update is included too. Reading a Session never
/// changes either ordering.
pub fn recents_recency_ms(session_id: &str, command: &str, created_at: u64) -> u64 {
    latest_lifecycle_ms(session_id, command, created_at, None)
}

/// Shared manual sidebar order (`~/.unpeel/session-order.json`):
/// `{ project_id: [session ids] }`. The desktop keeps the same list in its
/// UserDefaults overlay; this file is how a drag in one frontend reaches
/// the others. Ids absent from the list keep their natural (newest-first)
/// position above the hand-ordered block, matching the app.
fn session_order_path() -> PathBuf {
    app_paths::unpeel_home().join("session-order.json")
}

/// `~/.unpeel/project-order.json` — a flat list of project ids in sidebar
/// order. The sibling of `session-order.json`, and shared for the same
/// reason: a drag in one frontend has to show up in the other.
fn project_order_path() -> std::path::PathBuf {
    app_paths::unpeel_home().join("project-order.json")
}

/// This process's own hook-listener port, so `announce` can skip it. Set
/// once by whichever frontend owns the listener.
static OWN_LISTENER_PORT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_own_listener_port(port: u16) {
    OWN_LISTENER_PORT.store(port as u32, std::sync::atomic::Ordering::Relaxed);
}

pub fn own_listener_port_public() -> Option<u16> {
    own_listener_port()
}

fn own_listener_port() -> Option<u16> {
    match OWN_LISTENER_PORT.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        port => Some(port as u16),
    }
}

pub fn project_order() -> Vec<String> {
    std::fs::read(project_order_path())
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.as_array().cloned())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub fn set_project_order(ids: &[String]) -> Result<(), String> {
    let path = project_order_path();
    let _lock = crate::app_state::lock_exclusive(&path)?;
    write_project_order(&path, ids)?;
    crate::state_bus::announce(crate::state_bus::Change::Order, own_listener_port());
    Ok(())
}

/// Replace one parent project's sibling ranks inside the flat shared project
/// order. The merge happens while holding the same lock as the native app,
/// so simultaneous root/folder drags do not overwrite each other.
pub fn set_project_sibling_order(
    sibling_ids: &[String],
    fallback_all_ids: &[String],
) -> Result<(), String> {
    if sibling_ids.is_empty() {
        return Ok(());
    }
    let path = project_order_path();
    let _lock = crate::app_state::lock_exclusive(&path)?;
    let mut shared: Vec<String> = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_else(|| fallback_all_ids.to_vec());
    merge_project_sibling_order(&mut shared, sibling_ids, fallback_all_ids)?;
    write_project_order(&path, &shared)?;
    crate::state_bus::announce(crate::state_bus::Change::Order, own_listener_port());
    Ok(())
}

fn merge_project_sibling_order(
    shared: &mut Vec<String>,
    sibling_ids: &[String],
    fallback_all_ids: &[String],
) -> Result<(), String> {
    for id in fallback_all_ids {
        if !shared.contains(id) {
            shared.push(id.clone());
        }
    }
    let sibling_set: std::collections::HashSet<&str> =
        sibling_ids.iter().map(String::as_str).collect();
    let slots: Vec<usize> = shared
        .iter()
        .enumerate()
        .filter_map(|(index, id)| sibling_set.contains(id.as_str()).then_some(index))
        .collect();
    if slots.len() != sibling_ids.len() {
        return Err("project sibling order does not match the current project list".into());
    }
    for (slot, id) in slots.into_iter().zip(sibling_ids) {
        shared[slot] = id.clone();
    }
    Ok(())
}

fn write_project_order(path: &std::path::Path, ids: &[String]) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(&ids).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn session_order(project_id: &str) -> Vec<String> {
    std::fs::read(session_order_path())
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get(project_id).cloned())
        .and_then(|list| list.as_array().cloned())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// When the shared order for a project contains any child folder id, return
/// that list so Controllers can interleave groups with sessions. Otherwise
/// None — folders stay above sessions, matching the desktop default.
pub fn mixed_session_order_from(
    order: &[String],
    child_ids: &std::collections::HashSet<String>,
) -> Option<Vec<String>> {
    if order.iter().any(|id| child_ids.contains(id)) {
        Some(order.to_vec())
    } else {
        None
    }
}

/// Attach `sessionOrder` to each wire project whose shared rank list mixes
/// in a child group or worktree. Additive: projects without a mixed list
/// keep their existing shape.
pub fn attach_mixed_session_order_fields(projects: &mut [serde_json::Value]) {
    let child_ids: std::collections::HashSet<String> = projects
        .iter()
        .filter_map(|project| {
            project.get("parentProjectID")?.as_str()?;
            project.get("id")?.as_str().map(str::to_owned)
        })
        .collect();
    if child_ids.is_empty() {
        return;
    }
    for project in projects {
        let Some(id) = project.get("id").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(order) = mixed_session_order_from(&session_order(id), &child_ids) else {
            continue;
        };
        if let Some(object) = project.as_object_mut() {
            object.insert("sessionOrder".into(), serde_json::json!(order));
        }
    }
}

pub fn set_session_order(project_id: &str, ids: &[String]) -> Result<(), String> {
    let path = session_order_path();
    let _lock = crate::app_state::lock_exclusive(&path)?;
    let mut value: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let object = value
        .as_object_mut()
        .ok_or("session-order.json is not an object")?;
    if ids.is_empty() {
        object.remove(project_id);
    } else {
        object.insert(project_id.to_string(), serde_json::json!(ids));
    }
    let body = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    crate::state_bus::announce(crate::state_bus::Change::Order, own_listener_port());
    Ok(())
}

/// Rename a shared-state plain group. Refuses project/worktree records so a
/// stale caller can never rename a real project through the group verb.
/// Persists through the app-state choke point (flock + state-bus announce).
pub fn rename_group_project(project_id: &str, name: &str) -> Result<(), String> {
    crate::app_state::edit(|state| {
        let projects = state
            .get_mut("projects")
            .and_then(|value| value.as_array_mut())
            .ok_or("app-state.json has no projects array")?;
        let project = projects
            .iter_mut()
            .find(|project| project.get("id").and_then(|value| value.as_str()) == Some(project_id))
            .ok_or("this group is managed by the desktop app — rename it there")?;
        let is_group = project
            .get("parent_project_id")
            .and_then(|value| value.as_str())
            .is_some()
            && project
                .get("is_folder")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            && project
                .get("worktree_branch")
                .and_then(|value| value.as_str())
                .is_none();
        if !is_group {
            return Err("only plain groups can be renamed here".into());
        }
        project["name"] = serde_json::Value::String(name.to_string());
        Ok(())
    })
}

/// Whether this group's sidebar sessions use the shared Recent order
/// (working first, then latest lifecycle timestamp; no read receipt)
/// instead of the manual drag order. The mode lives in app-state.json
/// (`session_sort_modes`, group id → "date"; absent = custom) so every
/// frontend reads one truth.
pub fn session_date_sorted(project_id: &str) -> bool {
    crate::app_state::load()
        .ok()
        .and_then(|state| {
            state
                .get("session_sort_modes")?
                .get(project_id)?
                .as_str()
                .map(|mode| mode == "date")
        })
        .unwrap_or(false)
}

/// Flip a group between date sort and custom order. Keeps the manual order
/// in session-order.json untouched, so switching back restores the old
/// arrangement. Announces via the app-state choke point.
pub fn set_session_date_sorted(project_id: &str, date_sorted: bool) -> Result<(), String> {
    crate::app_state::edit(|object| {
        let modes = object
            .entry("session_sort_modes")
            .or_insert_with(|| serde_json::json!({}));
        let map = modes
            .as_object_mut()
            .ok_or("session_sort_modes is not an object")?;
        if date_sorted {
            map.insert(project_id.to_string(), serde_json::json!("date"));
        } else {
            map.remove(project_id);
        }
        Ok(())
    })
}

/// Persist a project's folder color in `app-state.json` (`project_colors`):
/// the disk carrier for workspaces without a native UserDefaults overlay.
/// `None` clears the entry. Flock + state-bus announce through
/// `app_state::edit`, like every other shared-state write.
pub fn set_project_folder_color(project_id: &str, color: Option<&str>) -> Result<(), String> {
    crate::app_state::edit(|object| apply_project_folder_color(object, project_id, color))
}

/// `set_project_folder_color` against an explicit file (tests, tooling).
pub fn set_project_folder_color_at(
    path: &std::path::Path,
    project_id: &str,
    color: Option<&str>,
) -> Result<(), String> {
    crate::app_state::edit_at(path, |object| {
        apply_project_folder_color(object, project_id, color)
    })
}

fn apply_project_folder_color(
    object: &mut serde_json::Map<String, serde_json::Value>,
    project_id: &str,
    color: Option<&str>,
) -> Result<(), String> {
    let colors = object
        .entry("project_colors")
        .or_insert_with(|| serde_json::json!({}));
    let map = colors
        .as_object_mut()
        .ok_or("project_colors is not an object")?;
    match color.filter(|value| !value.is_empty()) {
        Some(color) => {
            map.insert(project_id.to_string(), serde_json::json!(color));
        }
        None => {
            map.remove(project_id);
        }
    }
    Ok(())
}

fn rewrite_string_array(
    value: &mut serde_json::Value,
    old_id: &str,
    replacement_id: Option<&str>,
) -> bool {
    let Some(values) = value.as_array_mut() else {
        return false;
    };
    let mut changed = false;
    values.retain_mut(|value| {
        if value.as_str() != Some(old_id) {
            return true;
        }
        changed = true;
        match replacement_id {
            Some(new_id) => {
                *value = serde_json::Value::String(new_id.to_owned());
                true
            }
            None => false,
        }
    });
    changed
}

fn pinned_entry_matches(value: &serde_json::Value, session_id: &str) -> bool {
    if value.as_str() == Some(session_id) {
        return true;
    }
    let Some(entry) = value.as_object() else {
        return false;
    };
    entry.get("session_id").and_then(serde_json::Value::as_str) == Some(session_id)
        || entry.get("key").and_then(serde_json::Value::as_str)
            == Some(crate::state::pinned_sidebar_session_key(session_id).as_str())
}

fn rewrite_pinned_array(
    value: &mut serde_json::Value,
    old_id: &str,
    replacement_id: Option<&str>,
) -> bool {
    let Some(entries) = value.as_array_mut() else {
        return false;
    };
    let old_key = crate::state::pinned_sidebar_session_key(old_id);
    let new_key = replacement_id.map(crate::state::pinned_sidebar_session_key);
    let mut changed = false;
    entries.retain_mut(|entry| {
        if !pinned_entry_matches(entry, old_id) {
            return true;
        }
        changed = true;
        let Some(new_id) = replacement_id else {
            return false;
        };
        if entry.as_str() == Some(old_id) {
            *entry = serde_json::Value::String(new_id.to_owned());
            return true;
        }
        if let Some(object) = entry.as_object_mut() {
            if object.get("session_id").and_then(serde_json::Value::as_str) == Some(old_id) {
                object.insert(
                    "session_id".into(),
                    serde_json::Value::String(new_id.to_owned()),
                );
            }
            if object.get("key").and_then(serde_json::Value::as_str) == Some(old_key.as_str()) {
                object.insert(
                    "key".into(),
                    serde_json::Value::String(new_key.clone().unwrap_or_default()),
                );
            }
        }
        true
    });
    changed
}

/// Rewrite every shared app-state reference to one Session. The mutation is
/// deliberately untyped: fields and nested keys newer clients own survive a
/// restart/removal byte-for-byte unless they are the specific id reference.
fn rewrite_app_state_session_references(
    state: &mut serde_json::Map<String, serde_json::Value>,
    old_id: &str,
    replacement_id: Option<&str>,
) -> Result<bool, String> {
    let mut changed = false;

    if let Some(pinned) = state.get_mut("pinned_sessions") {
        match pinned {
            // Canonical shape: project bucket -> pin entry objects.
            serde_json::Value::Object(grouped) => {
                let mut emptied = Vec::new();
                for (project_id, entries) in grouped.iter_mut() {
                    let changed_here = rewrite_pinned_array(entries, old_id, replacement_id);
                    changed |= changed_here;
                    if replacement_id.is_none()
                        && changed_here
                        && entries.as_array().is_some_and(Vec::is_empty)
                    {
                        emptied.push(project_id.clone());
                    }
                }
                for project_id in emptied {
                    grouped.remove(&project_id);
                }
            }
            // Legacy flat pins remain readable and are rewritten in place.
            serde_json::Value::Array(_) => {
                changed |= rewrite_pinned_array(pinned, old_id, replacement_id);
            }
            _ => {}
        }
    }

    if let Some(grants) = state
        .get_mut("mcp_orchestrators")
        .and_then(serde_json::Value::as_object_mut)
    {
        if let Some(grant) = grants.remove(old_id) {
            if let Some(new_id) = replacement_id {
                grants.insert(new_id.to_owned(), grant);
            }
            changed = true;
        }
    }

    if let Some(approvals) = state
        .get_mut("mcp_write_approvals")
        .and_then(serde_json::Value::as_object_mut)
    {
        if let Some(targets) = approvals.remove(old_id) {
            if let Some(new_id) = replacement_id {
                approvals.insert(new_id.to_owned(), targets);
            }
            changed = true;
        }
        let mut emptied = Vec::new();
        for (caller, targets) in approvals.iter_mut() {
            let changed_here = rewrite_string_array(targets, old_id, replacement_id);
            changed |= changed_here;
            if replacement_id.is_none()
                && changed_here
                && targets.as_array().is_some_and(Vec::is_empty)
            {
                emptied.push(caller.clone());
            }
        }
        for caller in emptied {
            approvals.remove(&caller);
        }
    }

    // App launch approval is scoped to the calling Session and App id. App
    // ids are not Session references, so only the map key follows lifecycle.
    if let Some(approvals) = state
        .get_mut("mcp_app_open_approvals")
        .and_then(serde_json::Value::as_object_mut)
    {
        if let Some(apps) = approvals.remove(old_id) {
            if let Some(new_id) = replacement_id {
                approvals.insert(new_id.to_owned(), apps);
            }
            changed = true;
        }
    }

    for field in ["browser_approvals", "computer_approvals"] {
        if let Some(approvals) = state.get_mut(field) {
            changed |= rewrite_string_array(approvals, old_id, replacement_id);
        }
    }

    changed |= crate::app_presentations::rewrite_app_presentation_session_references(
        state,
        old_id,
        replacement_id,
    )?;

    Ok(changed)
}

fn rewrite_session_order_value(
    value: &mut serde_json::Value,
    old_id: &str,
    replacement_id: Option<&str>,
) -> bool {
    let Some(grouped) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    let mut emptied = Vec::new();
    for (project_id, ids) in grouped.iter_mut() {
        let changed_here = rewrite_string_array(ids, old_id, replacement_id);
        changed |= changed_here;
        if replacement_id.is_none() && changed_here && ids.as_array().is_some_and(Vec::is_empty) {
            emptied.push(project_id.clone());
        }
    }
    for project_id in emptied {
        grouped.remove(&project_id);
    }
    changed
}

/// Explicit-path variant keeps the transformation independently testable and
/// avoids ever swapping process-global `UNPEEL_HOME` in unit tests.
fn rewrite_session_order_references_at(
    path: &std::path::Path,
    old_id: &str,
    replacement_id: Option<&str>,
) -> Result<bool, String> {
    let _lock = crate::app_state::lock_exclusive(path)?;
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let mut value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if !rewrite_session_order_value(&mut value, old_id, replacement_id) {
        return Ok(false);
    }
    let body = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(true)
}

fn validate_session_order_references_at(path: &std::path::Path) -> Result<(), String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if !value.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }
    Ok(())
}

fn validate_shared_session_reference_files() -> Result<serde_json::Value, String> {
    let state =
        crate::app_state::load().map_err(|error| format!("read app-state.json: {error}"))?;
    let root = state
        .as_object()
        .ok_or_else(|| "app-state.json is not an object".to_string())?;
    crate::app_presentations::validate_app_presentation_state(root)?;
    validate_session_order_references_at(&session_order_path())?;
    Ok(state)
}

fn rewrite_shared_session_references(
    old_id: &str,
    replacement_id: Option<&str>,
) -> Result<(), String> {
    let app_state_result = crate::app_state::edit(|state| {
        rewrite_app_state_session_references(state, old_id, replacement_id)
    });
    let order_result =
        rewrite_session_order_references_at(&session_order_path(), old_id, replacement_id);
    if matches!(order_result, Ok(true)) {
        crate::state_bus::announce(crate::state_bus::Change::Order, own_listener_port());
    }

    match (app_state_result, order_result) {
        (Ok(_), Ok(_)) => Ok(()),
        (Err(app_state), Ok(_)) => Err(format!("update app-state.json: {app_state}")),
        (Ok(_), Err(order)) => Err(format!("update session-order.json: {order}")),
        (Err(app_state), Err(order)) => Err(format!(
            "update app-state.json: {app_state}; update session-order.json: {order}"
        )),
    }
}

/// Stop and delete only the physical hosted Session. Public removal adds the
/// reference pruning below; restart deliberately uses this lower layer so it
/// can re-point identity-bearing state to the replacement instead.
fn teardown_session_files(session_id: &str) -> Result<(), String> {
    stop_session_unlocked(session_id)?;
    let dir = session_dir(session_id);
    let _ = std::fs::remove_dir_all(&dir);
    // The dying host may race one final manifest write into the dir; re-delete
    // until it stays gone, but don't stall once it has.
    for _ in 0..DIR_DELETE_RETRIES {
        if !dir.exists() {
            break;
        }
        std::thread::sleep(DIR_DELETE_RETRY_DELAY);
        let _ = std::fs::remove_dir_all(&dir);
    }
    if dir.exists() {
        return Err(format!(
            "session directory still exists after teardown: {}",
            dir.display()
        ));
    }
    Ok(())
}

/// Destructive remove: serialize against restart/remove for this Session,
/// stop and delete its host, then prune every shared reference to its id.
pub fn remove_session(session_id: &str) -> Result<(), String> {
    let _lifecycle_lock = lock_session_lifecycle(session_id)?;
    // Refuse the destructive half when the identity indexes cannot even be
    // read. That keeps a corrupt settings/order file from turning a recoverable
    // Session into an unprunable dangling id.
    validate_shared_session_reference_files()?;
    remove_session_unlocked(session_id)
}

/// Caller holds this Session's lifecycle lock. Failure recovery deliberately
/// bypasses the public preflight: even when an index became unreadable between
/// spawn and identity transfer, the unreported replacement host must still be
/// stopped and removed before cleanup reports the index error.
fn remove_session_unlocked(session_id: &str) -> Result<(), String> {
    let manifest = load_manifest(session_id);
    let managed_storage = manifest.as_ref().and_then(managed_storage_for_manifest);
    let computer_session = manifest
        .as_ref()
        .is_some_and(HostedSessionManifest::computer_mcp_enabled);
    teardown_session_files(session_id)?;
    if computer_session {
        end_computer_engine_session(session_id);
    }
    let storage_result: Result<(), String> =
        managed_storage.map_or(Ok(()), |path| match std::fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "managed runtime storage {} could not be removed: {error}",
                path.display()
            )),
        });
    let prune_result = rewrite_shared_session_references(session_id, None);
    crate::state_bus::announce(crate::state_bus::Change::Lifecycle, own_listener_port());
    match (prune_result, storage_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(prune), Ok(())) => Err(prune),
        (Ok(()), Err(storage)) => Err(format!("session removed, but {storage}")),
        (Err(prune), Err(storage)) => Err(format!("{prune}; additionally, {storage}")),
    }
}

/// Ask the Computer Use engine to forget a removed Session's driver session
/// (`unpeel-host __computer_cleanup__ <id>`), the same call the Mac app makes
/// on Remove. Best effort and detached: a down or absent engine is a fine
/// outcome for cleanup, and Remove never waits on the engine.
fn end_computer_engine_session(session_id: &str) {
    let Ok(binary) = resolve_host_binary() else {
        return;
    };
    let _ = std::process::Command::new(binary)
        .arg(crate::computer_mcp::COMPUTER_CLEANUP_ARG)
        .arg(session_id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Whether a newly launched Session should advertise the Browser MCP domain.
///
/// Keep this as a raw-value read: `app-state.json` is a cross-version contract,
/// and launch policy must not require a typed round-trip that could discard
/// fields owned by another frontend. Missing means the shipped default (On),
/// while malformed or unknown explicit values fail closed through
/// [`BrowserAccess::from_state_str`].
pub(crate) fn browser_mcp_enabled_from_app_state(state: &serde_json::Value) -> bool {
    let experimental_enabled = state
        .get("experimental_features")
        .and_then(|features| features.get("browser_mcp"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let access = match state.get("browser_default_access") {
        None => BrowserAccess::default(),
        Some(value) => value
            .as_str()
            .map(BrowserAccess::from_state_str)
            .unwrap_or(BrowserAccess::Off),
    };
    experimental_enabled && access != BrowserAccess::Off
}

fn mcp_features_enabled_for_launch() -> (bool, bool, bool) {
    match crate::app_state::load() {
        Ok(state) => {
            let sessions = state
                .get("experimental_features")
                .and_then(|features| features.get("sessions_mcp"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            (
                sessions,
                browser_mcp_enabled_from_app_state(&state),
                crate::computer_mcp::enabled_for_launch_from_app_state(&state),
            )
        }
        Err(error) => {
            // A damaged settings file must not take down Session creation,
            // but it also must not silently grant MCP domains.
            log::warn!("Could not read MCP launch policy; disabling it: {error}");
            (false, false, false)
        }
    }
}

/// Spawn a fresh hosted session. Hook assets install host-side at spawn, so
/// callers only provide the launch parameters. Returns the new session id.
pub fn spawn_session(
    mut session: SessionInfo,
    cwd: &str,
    hook_port: Option<u16>,
    initial_cols: u16,
    initial_rows: u16,
) -> Result<String, String> {
    if session.id.is_empty() {
        session.id = uuid::Uuid::new_v4().to_string().to_lowercase();
    }
    // Provider launch preparation belongs to `session_host::run_host`, after
    // the final Session id and Unpeel home are known and before the first
    // manifest is published. Keeping the command original here guarantees
    // every frontend crosses that runtime-owned boundary exactly once.
    let (sessions_mcp_enabled, browser_mcp_enabled, computer_mcp_enabled) =
        mcp_features_enabled_for_launch();
    let launch = SessionHostLaunch {
        session: session.clone(),
        cwd: cwd.to_string(),
        dark_mode: None,
        // A TUI/headless Host can set the same workspace accent contract as
        // the native frontend. Validation happens at the PTY environment
        // boundary; absence keeps hosted Apps on their standalone default.
        app_accent: std::env::var("UNPEEL_APP_ACCENT").ok(),
        hook_port,
        execution_scope: crate::session_host::SessionExecutionScope::Local,
        initial_cols: Some(initial_cols.max(2)),
        initial_rows: Some(initial_rows.max(2)),
        wait_for_attach: false,
        mcp_enabled: sessions_mcp_enabled,
        browser_mcp_enabled,
        computer_mcp_enabled,
    };
    let launch_file = write_launch_file(&launch)?;
    let host = resolve_host_binary()?;
    // Launcher argv mode: `unpeel-host <launch-file>` re-execs itself as
    // `__session_host__` fully detached (setsid) and returns immediately.
    let status = std::process::Command::new(&host)
        .arg(&launch_file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("spawn {}: {e}", host.display()))?;
    if !status.success() {
        return Err(format!("{} exited with {status}", host.display()));
    }
    crate::state_bus::announce(crate::state_bus::Change::Lifecycle, own_listener_port());
    Ok(session.id)
}

/// How a Controller-supplied initial prompt is delivered after a new hosted
/// Session becomes reachable. This deliberately mirrors the shipped mobile
/// DTO rather than guessing from the text itself: `raw` and `pasteOnly` both
/// preserve the bytes exactly, while `pasteAndSubmit` appends one carriage
/// return.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitialTextSubmitMode {
    PasteOnly,
    PasteAndSubmit,
    Raw,
}

pub fn initial_text_payload(text: &str, mode: InitialTextSubmitMode) -> String {
    match mode {
        InitialTextSubmitMode::PasteOnly | InitialTextSubmitMode::Raw => text.to_owned(),
        InitialTextSubmitMode::PasteAndSubmit => {
            let mut payload = String::with_capacity(text.len().saturating_add(1));
            payload.push_str(text);
            payload.push('\r');
            payload
        }
    }
}

/// Deliver optional initial text after the caller has observed a live control
/// socket. Empty text matches the shipped native route and is a no-op (even in
/// submit mode).
pub fn deliver_initial_text(
    session_id: &str,
    text: &str,
    mode: InitialTextSubmitMode,
) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let response = socket_command(
        session_id,
        serde_json::json!({
            "type": "write",
            "data": initial_text_payload(text, mode),
        }),
    )?;
    if response.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(response
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("session host rejected initial text")
        .to_owned())
}

/// Locate the `unpeel-host` binary for clients that are NOT the host
/// themselves (the TUI): env override, then a sibling of the current
/// executable (dev target dir, app bundle), then PATH.
pub fn resolve_host_binary() -> Result<PathBuf, String> {
    if let Some(cmd) = std::env::var_os("UNPEEL_HOST_CMD") {
        let path = PathBuf::from(&cmd);
        if path.exists() {
            return Ok(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().is_some_and(|n| n == "unpeel-host") {
            return Ok(exe);
        }
        let sibling = exe.with_file_name("unpeel-host");
        if sibling.exists() {
            return Ok(sibling);
        }
    }
    Ok(PathBuf::from("unpeel-host"))
}

/// Restart with resume: the app-independent mirror of `restartSession`.
/// Returns the replacement session id.
#[derive(Clone, Copy, Debug)]
pub enum RelaunchMode {
    /// Resume the prior conversation (or start fresh when it never received
    /// input, or when forced).
    Restart { force_fresh: bool },
}

/// The command a relaunch of this session should run, derived entirely from
/// shared on-disk state. The TUI, CLI, and app all call this implementation.
pub fn relaunch_command(session_id: &str, mode: RelaunchMode) -> Result<String, String> {
    let old = manifest(session_id)?;
    // The marker is the strongest signal (single-writer, no host race);
    // the manifest's injected copy is the fallback.
    let provider_id = provider_session_marker(session_id)
        .0
        .or(old.provider_session_id.clone());
    let relaunch = match mode {
        RelaunchMode::Restart { force_fresh } => {
            if force_fresh || !old.has_been_written_to {
                crate::resume::fresh(&old.session.command)
            } else {
                crate::resume::resumed(&old.session.command, provider_id.as_deref())
            }
        }
    };
    Ok(relaunch)
}

/// Legacy spelling retained for local/API compatibility. Current protocol-v3
/// Hosts route it through the same shell-only transaction as `resume_agent`;
/// they never stop an active foreground runtime.
pub fn restart_agent(session_id: &str) -> Result<(), String> {
    agent_in_place_action(session_id, false)
}

/// Resume the saved agent recipe only after the existing hosted PTY has
/// freshly returned to its owned shell. This is the product-facing in-place
/// recovery verb; unlike the historical name above it cannot imply stopping
/// a live runtime.
pub fn resume_agent(session_id: &str) -> Result<(), String> {
    agent_in_place_action(session_id, true)
}

fn agent_in_place_action(session_id: &str, shell_only_wire_verb: bool) -> Result<(), String> {
    // Snapshot before acquiring the cross-process lifecycle lock. Concurrent
    // callers therefore carry the same compare-and-swap generation: the
    // winner advances it and the later caller is rejected instead of queuing
    // a second resume line into the freshly relaunched agent.
    let snapshot = manifest(session_id)?;
    if !can_archive_manifest(&snapshot) {
        return Err(format!(
            "session {session_id} has no resumable provider conversation"
        ));
    }
    // Current callers never send either spelling to a protocol-v2 child: its
    // historical restart handler could signal an active runtime. Protocol v3
    // makes both the compatibility verb and Resume Agent shell-only.
    if snapshot.host_protocol_version.unwrap_or(0)
        < crate::session_host::SESSION_HOST_RESUME_AGENT_PROTOCOL_VERSION
    {
        return Err(format!(
            "session {session_id} host does not support shell-only agent resume"
        ));
    }
    let expected_generation = snapshot.runtime_launch_generation;
    let _lifecycle_lock = lock_session_lifecycle(session_id)?;
    let command = if shell_only_wire_verb {
        crate::session_host::SessionHostCommand::ResumeAgent {
            expected_generation,
        }
    } else {
        crate::session_host::SessionHostCommand::RestartAgent {
            expected_generation,
        }
    };
    crate::session_host::send_command_with_timeout(
        session_id,
        &command,
        // First-party runtime support reconciliation may do a little
        // disk/process work before the Host verifies the foreground. Stay
        // below the Controller's 10s effect deadline while avoiding an
        // ambiguous local timeout on a loaded Host.
        Duration::from_secs(8),
    )?;
    crate::state_bus::announce(crate::state_bus::Change::Lifecycle, own_listener_port());
    crate::state_bus::announce(
        crate::state_bus::Change::SessionMarkers,
        own_listener_port(),
    );
    Ok(())
}

pub fn restart_session(
    session_id: &str,
    hook_port: Option<u16>,
    initial_cols: u16,
    initial_rows: u16,
) -> Result<String, String> {
    let _lifecycle_lock = lock_session_lifecycle(session_id)?;
    restart_session_unlocked(session_id, hook_port, initial_cols, initial_rows)
}

/// Resume a stopped Session by replacing its exited Host. Unlike
/// `restart_session`, this is safe to expose as the ordinary Controller
/// `session.restart`/Resume operation: the state check and replacement share
/// the lifecycle lock, so a stale Controller decision can never tear down a
/// terminal that is still running.
pub fn resume_session(
    session_id: &str,
    hook_port: Option<u16>,
    initial_cols: u16,
    initial_rows: u16,
) -> Result<String, String> {
    let _lifecycle_lock = lock_session_lifecycle(session_id)?;
    // A Host crash can leave a final `running` manifest behind. Normalize it
    // under the same lifecycle lock as the stopped-only decision so a dead or
    // recycled child becomes resumable, while a genuinely healthy Host can
    // never be torn down by a stale Controller snapshot.
    let current = crate::session_host::refresh_manifest_health(session_id)
        .ok_or_else(|| format!("no manifest for {session_id}"))?;
    if current.state != HostedSessionState::Exited {
        return Err(format!("session {session_id} is still running"));
    }
    // Deliberately no resume-evidence gate at the execution layer: UI
    // surfaces advertise Resume from evidence (`can_archive_manifest`), but
    // an explicitly requested replacement falls back to the resume planner's
    // fresh/continue-last recipes — a stale or version-skewed Controller
    // gets a relaunch, never a hard failure.
    restart_session_unlocked(session_id, hook_port, initial_cols, initial_rows)
}

/// Caller holds this Session's lifecycle lock. `restart_session` is the
/// explicit terminal-replacement primitive (including live maintenance
/// reloads); `resume_session` adds the stopped-only product invariant.
fn restart_session_unlocked(
    session_id: &str,
    hook_port: Option<u16>,
    initial_cols: u16,
    initial_rows: u16,
) -> Result<String, String> {
    let old = manifest(session_id)?;
    let relaunch = relaunch_command(session_id, RelaunchMode::Restart { force_fresh: false })?;
    let effective_title = title_marker(session_id);
    // Validate both identity indexes before taking down the old host. Later
    // writes are still independently guarded/atomic, but ordinary corruption
    // now fails while the original Session remains usable.
    let shared_state = validate_shared_session_reference_files()?;
    let group_project_id = project_override_marker(session_id).filter(|project_id| {
        shared_state
            .get("projects")
            .and_then(|value| value.as_array())
            .is_some_and(|projects| {
                projects.iter().any(|project| {
                    project.get("id").and_then(|value| value.as_str()) == Some(project_id.as_str())
                })
            })
    });
    teardown_session_files(session_id)?;

    let mut session = SessionInfo {
        id: uuid::Uuid::new_v4().to_string().to_lowercase(),
        command: relaunch,
        // Keep the old created_at so the row keeps its sidebar position.
        created_at: if old.session.created_at > 0 {
            old.session.created_at
        } else {
            now_ms()
        },
        ..old.session.clone()
    };
    if let Some(group_project_id) = group_project_id {
        session.project_id = group_project_id;
    }
    // `title.json` is the effective override for headless frontends. Bake it
    // into the replacement launch rather than trying to copy a marker after
    // the detached launcher has accepted the request; this also disables the
    // host's prompt auto-title before its first byte of input can race us.
    if let Some(title) = effective_title {
        session.label = title;
        session.custom_title = true;
    }
    // Older manifests may carry a lineage edge; current launches never do.
    session.parent_session_id = None;
    let cwd = old.cwd.clone();
    let replacement_id = session.id.clone();
    if let Err(spawn_error) = spawn_session(session, &cwd, hook_port, initial_cols, initial_rows) {
        let cleanup_errors = cleanup_failed_restart(session_id, &replacement_id);
        let mut message = format!(
            "failed to spawn replacement for {session_id} after removing the original: {spawn_error}"
        );
        if !cleanup_errors.is_empty() {
            message.push_str("; cleanup errors: ");
            message.push_str(&cleanup_errors.join("; "));
        }
        return Err(message);
    }

    if let Err(state_error) = rewrite_shared_session_references(session_id, Some(&replacement_id)) {
        let cleanup_errors = cleanup_failed_restart(session_id, &replacement_id);
        let mut message = format!(
            "replacement {replacement_id} started, but its shared identity could not be transferred: {state_error}"
        );
        if cleanup_errors.is_empty() {
            message.push_str("; the replacement was removed");
        } else {
            message.push_str("; cleanup errors: ");
            message.push_str(&cleanup_errors.join("; "));
        }
        return Err(message);
    }
    Ok(replacement_id)
}

/// Best-effort cleanup for every failure after the original Session has been
/// physically removed. Prune both ids: a state transfer may have completed
/// only its app-state half before an order-file error, while a spawn failure
/// leaves references solely under the old id. In particular, no permission
/// entry for the unreported replacement id is intentionally retained.
fn cleanup_failed_restart(old_id: &str, replacement_id: &str) -> Vec<String> {
    let mut errors = Vec::new();
    match lock_session_lifecycle(replacement_id) {
        Ok(_replacement_lock) => {
            if let Err(error) = remove_session_unlocked(replacement_id) {
                errors.push(format!("remove replacement {replacement_id}: {error}"));
            }
        }
        Err(error) => {
            errors.push(format!(
                "lock replacement {replacement_id} for removal: {error}"
            ));
        }
    }
    if let Err(error) = rewrite_shared_session_references(old_id, None) {
        errors.push(format!("prune original {old_id}: {error}"));
    }
    errors
}

#[cfg(test)]
mod phone_fit_marker_tests {
    use super::{
        clear_phone_fit_marker_in, read_phone_fit_marker_in, write_phone_fit_marker_in,
        PhoneFitMarker, PHONE_FIT_MARKER,
    };

    fn scratch() -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("unpeel-phone-fit-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn marker_round_trips_and_clear_removes_it() {
        let dir = scratch();
        assert!(read_phone_fit_marker_in(&dir).is_none());
        let marker = PhoneFitMarker {
            columns: 60,
            rows: 24,
            since_unix_ms: 1_788_000_000_000,
        };
        write_phone_fit_marker_in(&dir, &marker).unwrap();
        assert!(dir.join(PHONE_FIT_MARKER).exists());
        assert_eq!(read_phone_fit_marker_in(&dir), Some(marker));
        assert!(clear_phone_fit_marker_in(&dir));
        assert!(read_phone_fit_marker_in(&dir).is_none());
        assert!(!clear_phone_fit_marker_in(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_degenerate_marker_is_ignored() {
        let dir = scratch();
        std::fs::write(
            dir.join(PHONE_FIT_MARKER),
            br#"{"columns":1,"rows":0,"since_unix_ms":5}"#,
        )
        .unwrap();
        assert!(read_phone_fit_marker_in(&dir).is_none());
        std::fs::write(dir.join(PHONE_FIT_MARKER), b"not json").unwrap();
        assert!(read_phone_fit_marker_in(&dir).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        browser_mcp_enabled_from_app_state, can_archive_command, initial_text_payload,
        legacy_pinned_group_id, lifecycle_lock_target_at, merge_project_sibling_order,
        mixed_session_order_from, prepare_legacy_pin_groups, provider_transcript_has_resume_data,
        remove_migrated_pin_entries, remove_unreferenced_legacy_pin_groups,
        rewrite_app_state_session_references, rewrite_session_order_references_at,
        rewrite_session_order_value, set_group_pinned_in_state,
        validate_session_order_references_at, InitialTextSubmitMode,
    };

    #[test]
    fn project_folder_color_round_trips_through_app_state() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "unpeel-folder-color-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app-state.json");
        std::fs::write(&path, r#"{"projects":[],"presets":[]}"#).unwrap();

        super::set_project_folder_color_at(&path, "p1", Some("sky")).unwrap();
        super::set_project_folder_color_at(&path, "p2", Some("moss")).unwrap();
        let state: crate::state::AppState =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            state.project_colors.get("p1").map(String::as_str),
            Some("sky")
        );
        assert_eq!(
            state.project_colors.get("p2").map(String::as_str),
            Some("moss")
        );
        // Unmodelled keys survive the edit.
        assert!(state.presets.is_empty());

        // The empty string clears exactly like `None` (the wire's "default").
        super::set_project_folder_color_at(&path, "p1", Some("")).unwrap();
        super::set_project_folder_color_at(&path, "p2", None).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["project_colors"], serde_json::json!({}));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_static_gate_requires_a_runtime_resume_recipe() {
        assert!(!can_archive_command(""));
        assert!(can_archive_command("claude --model opus"));
        assert!(!can_archive_command("my-custom-agent --serve"));
        assert!(!can_archive_command("bash"));
    }

    #[test]
    fn provider_transcript_requires_a_real_nonempty_file() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("conversation.jsonl");
        let path = transcript.to_string_lossy();

        assert!(!provider_transcript_has_resume_data(Some(&path)));
        std::fs::write(&transcript, []).unwrap();
        assert!(!provider_transcript_has_resume_data(Some(&path)));
        std::fs::write(&transcript, b"{}\n").unwrap();
        assert!(provider_transcript_has_resume_data(Some(&path)));
        assert!(!provider_transcript_has_resume_data(Some("   ")));
    }

    #[test]
    fn browser_launch_policy_uses_the_shipped_default_and_enables_non_off_access() {
        assert!(browser_mcp_enabled_from_app_state(&serde_json::json!({})));
        for value in ["on", "allow", "ask"] {
            assert!(
                browser_mcp_enabled_from_app_state(&serde_json::json!({
                    "browser_default_access": value,
                })),
                "{value} should advertise Browser MCP"
            );
        }
    }

    #[test]
    fn browser_launch_policy_fails_closed_for_explicit_invalid_access() {
        for value in [
            serde_json::json!("off"),
            serde_json::json!("future-mode"),
            serde_json::json!(true),
            serde_json::Value::Null,
        ] {
            assert!(
                !browser_mcp_enabled_from_app_state(&serde_json::json!({
                    "browser_default_access": value,
                })),
                "{value} should not advertise Browser MCP"
            );
        }
    }

    #[test]
    fn browser_launch_policy_honors_the_host_experimental_switch() {
        assert!(!browser_mcp_enabled_from_app_state(&serde_json::json!({
            "browser_default_access": "on",
            "experimental_features": { "browser_mcp": false },
        })));
        assert!(browser_mcp_enabled_from_app_state(&serde_json::json!({
            "browser_default_access": "on",
            "experimental_features": { "browser_mcp": true },
        })));
    }

    #[test]
    fn initial_text_modes_preserve_text_and_submit_exactly_once() {
        let text = "héllo\n\u{1b}[A";
        assert_eq!(
            initial_text_payload(text, InitialTextSubmitMode::PasteOnly),
            text
        );
        assert_eq!(initial_text_payload(text, InitialTextSubmitMode::Raw), text);
        assert_eq!(
            initial_text_payload(text, InitialTextSubmitMode::PasteAndSubmit),
            format!("{text}\r")
        );
        assert_eq!(
            initial_text_payload("already\r", InitialTextSubmitMode::PasteAndSubmit),
            "already\r\r",
            "submit mode appends one CR; it never rewrites caller bytes"
        );
    }

    #[test]
    fn sibling_project_merge_preserves_other_parent_ranks() {
        let mut shared = ["root", "a", "other", "x", "b", "y"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let siblings = ["b", "a"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let fallback = ["root", "a", "b", "other", "x", "y", "new"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();

        merge_project_sibling_order(&mut shared, &siblings, &fallback).unwrap();

        assert_eq!(shared, ["root", "b", "other", "x", "a", "y", "new"]);
    }

    #[test]
    fn restart_repoints_identity_state_without_losing_unknown_fields() {
        let mut state = serde_json::json!({
            "pinned_sessions": {
                "visual-group": [{
                    "key": "session:old",
                    "project_id": "manifest-project",
                    "session_id": "old",
                    "pinned_at": 1234,
                    "future_pin_field": {"kept": true}
                }],
                "other-group": [{
                    "key": "session:other",
                    "project_id": "other-group",
                    "session_id": "other",
                    "pinned_at": 99
                }]
            },
            "mcp_orchestrators": {
                "old": {"role": "write", "reach": "global", "future_grant": 7},
                "other": {"role": "read", "reach": "project"}
            },
            "mcp_write_approvals": {
                "old": ["target", "old"],
                "caller": ["old", "other"],
                "untouched": ["other"]
            },
            "mcp_app_open_approvals": {
                "old": ["unpeel.app.design"],
                "other": ["unpeel.app.notes"]
            },
            "browser_approvals": ["other", "old", {"future": "kept"}],
            "computer_approvals": ["old", "other"],
            "browser_access": {"old": "ask"},
            "future_root": {"nested": [1, 2, 3]}
        });

        assert!(rewrite_app_state_session_references(
            state.as_object_mut().unwrap(),
            "old",
            Some("new")
        )
        .unwrap());

        let pin = &state["pinned_sessions"]["visual-group"][0];
        assert_eq!(pin["key"], "session:new");
        assert_eq!(pin["session_id"], "new");
        assert_eq!(pin["project_id"], "manifest-project");
        assert_eq!(pin["pinned_at"], 1234);
        assert_eq!(pin["future_pin_field"]["kept"], true);
        assert_eq!(
            state["pinned_sessions"]["other-group"][0]["session_id"],
            "other"
        );

        assert!(state["mcp_orchestrators"].get("old").is_none());
        assert_eq!(state["mcp_orchestrators"]["new"]["role"], "write");
        assert_eq!(state["mcp_orchestrators"]["new"]["future_grant"], 7);
        assert_eq!(
            state["mcp_write_approvals"]["new"],
            serde_json::json!(["target", "new"])
        );
        assert_eq!(
            state["mcp_write_approvals"]["caller"],
            serde_json::json!(["new", "other"])
        );
        assert_eq!(
            state["mcp_write_approvals"]["untouched"],
            serde_json::json!(["other"])
        );
        assert!(state["mcp_app_open_approvals"].get("old").is_none());
        assert_eq!(
            state["mcp_app_open_approvals"]["new"],
            serde_json::json!(["unpeel.app.design"])
        );
        assert_eq!(
            state["mcp_app_open_approvals"]["other"],
            serde_json::json!(["unpeel.app.notes"])
        );
        assert_eq!(
            state["browser_approvals"],
            serde_json::json!(["other", "new", {"future": "kept"}])
        );
        assert_eq!(
            state["computer_approvals"],
            serde_json::json!(["new", "other"])
        );
        // Bounded migration: unrelated/legacy fields are never typed away.
        assert_eq!(state["browser_access"]["old"], "ask");
        assert_eq!(state["future_root"]["nested"][2], 3);
    }

    #[test]
    fn remove_prunes_every_identity_reference_and_only_those_references() {
        let mut state = serde_json::json!({
            "pinned_sessions": {
                "only-old": [{
                    "key": "session:old", "project_id": "only-old",
                    "session_id": "old", "pinned_at": 1
                }],
                "mixed": [
                    {"key": "session:old", "session_id": "old", "future": true},
                    {"key": "session:other", "session_id": "other", "future": true}
                ]
            },
            "mcp_orchestrators": {"old": {"future": true}, "other": {"future": true}},
            "mcp_write_approvals": {
                "old": ["target"],
                "only_old_target": ["old"],
                "mixed": ["old", "other"]
            },
            "mcp_app_open_approvals": {
                "old": ["unpeel.app.design"],
                "other": ["unpeel.app.notes"]
            },
            "browser_approvals": ["old", "other"],
            "computer_approvals": ["other", "old"],
            "future_root": {"old": "not a session-id field"}
        });

        assert!(
            rewrite_app_state_session_references(state.as_object_mut().unwrap(), "old", None)
                .unwrap()
        );

        assert!(state["pinned_sessions"].get("only-old").is_none());
        assert_eq!(
            state["pinned_sessions"]["mixed"].as_array().unwrap().len(),
            1
        );
        assert_eq!(state["pinned_sessions"]["mixed"][0]["session_id"], "other");
        assert!(state["mcp_orchestrators"].get("old").is_none());
        assert_eq!(state["mcp_orchestrators"]["other"]["future"], true);
        assert!(state["mcp_write_approvals"].get("old").is_none());
        assert!(state["mcp_write_approvals"]
            .get("only_old_target")
            .is_none());
        assert_eq!(
            state["mcp_write_approvals"]["mixed"],
            serde_json::json!(["other"])
        );
        assert!(state["mcp_app_open_approvals"].get("old").is_none());
        assert_eq!(
            state["mcp_app_open_approvals"]["other"],
            serde_json::json!(["unpeel.app.notes"])
        );
        assert_eq!(state["browser_approvals"], serde_json::json!(["other"]));
        assert_eq!(state["computer_approvals"], serde_json::json!(["other"]));
        assert_eq!(state["future_root"]["old"], "not a session-id field");
    }

    #[test]
    fn mixed_session_order_is_none_until_a_child_folder_is_ranked() {
        let children = ["group-research", "worktree-native-b"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            mixed_session_order_from(&["session-a".into(), "session-b".into()], &children),
            None
        );
        assert_eq!(
            mixed_session_order_from(
                &[
                    "session-a".into(),
                    "group-research".into(),
                    "session-b".into()
                ],
                &children
            ),
            Some(vec![
                "session-a".into(),
                "group-research".into(),
                "session-b".into()
            ])
        );
    }

    #[test]
    fn group_pin_round_trip_preserves_unknown_fields_and_timestamp() {
        let mut state = serde_json::json!({
            "projects": [
                {"id": "root", "name": "Root", "path": "/root"},
                {
                    "id": "group", "name": "Research", "path": "/root",
                    "parent_project_id": "root", "is_folder": true,
                    "pinned_at": 42, "future_field": {"kept": true}
                }
            ]
        });

        let object = state.as_object_mut().unwrap();
        set_group_pinned_in_state(object, "group", true).unwrap();
        assert_eq!(state["projects"][1]["pinned_at"], 42);
        assert_eq!(state["projects"][1]["future_field"]["kept"], true);

        set_group_pinned_in_state(state.as_object_mut().unwrap(), "group", false).unwrap();
        assert!(state["projects"][1].get("pinned_at").is_none());
        assert_eq!(state["projects"][1]["future_field"]["kept"], true);
    }

    #[test]
    fn group_pin_rejects_main_projects_and_worktrees() {
        let mut state = serde_json::json!({
            "projects": [
                {"id": "root", "name": "Root", "path": "/root"},
                {
                    "id": "worktree", "name": "Feature", "path": "/root/.worktrees/x",
                    "parent_project_id": "root", "is_folder": true,
                    "worktree_branch": "feature/x"
                }
            ]
        });

        let object = state.as_object_mut().unwrap();
        assert_eq!(
            set_group_pinned_in_state(object, "root", true).unwrap_err(),
            "only plain groups can be pinned"
        );
        assert_eq!(
            set_group_pinned_in_state(object, "worktree", true).unwrap_err(),
            "only plain groups can be pinned"
        );
        assert_eq!(
            set_group_pinned_in_state(object, "missing", true).unwrap_err(),
            "unknown group"
        );
    }

    #[test]
    fn legacy_pins_create_one_idempotent_top_pinned_group_per_root() {
        let mut state = serde_json::json!({
            "projects": [
                {"id": "root", "name": "Root", "path": "/root", "sort_order": 0},
                {"id": "group", "name": "Later", "path": "/root",
                 "parent_project_id": "root", "sort_order": 0, "is_folder": true}
            ],
            "pinned_sessions": {
                "root": [
                    {"session_id": "one", "project_id": "root", "pinned_at": 10}
                ],
                "group": [
                    {"session_id": "two", "project_id": "group", "pinned_at": 20}
                ]
            }
        });

        let first = prepare_legacy_pin_groups(state.as_object_mut().unwrap()).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].group_id, first[1].group_id);
        let pinned_id = first[0].group_id.clone();
        let projects = state["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 3);
        let pinned = projects
            .iter()
            .find(|project| project["id"] == pinned_id)
            .unwrap();
        assert_eq!(pinned["name"], "Pinned");
        assert_eq!(pinned["parent_project_id"], "root");
        assert_eq!(pinned["sort_order"], 0);
        assert_eq!(
            projects
                .iter()
                .find(|project| project["id"] == "group")
                .unwrap()["sort_order"],
            1
        );

        let second = prepare_legacy_pin_groups(state.as_object_mut().unwrap()).unwrap();
        assert_eq!(second.len(), 2);
        assert!(second.iter().all(|item| item.group_id == pinned_id));
        assert_eq!(state["projects"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn legacy_pin_cleanup_clears_only_successfully_migrated_sessions() {
        let mut pins = serde_json::json!({
            "root": [
                {"session_id": "moved", "future": true},
                {"session_id": "retry", "future": true}
            ],
            "other": [{"session_id": "also-moved"}]
        });
        let moved = ["moved", "also-moved"].into_iter().collect();

        remove_migrated_pin_entries(&mut pins, &moved);

        assert_eq!(
            pins["root"],
            serde_json::json!([
                {"session_id": "retry", "future": true}
            ])
        );
        assert!(pins.get("other").is_none());
    }

    #[test]
    fn stale_pin_cleanup_removes_only_empty_migration_owned_group() {
        let root_id = "root";
        let pinned_id = legacy_pinned_group_id(root_id);
        let mut state = serde_json::json!({
            "projects": [
                {"id": root_id, "name": "Root", "path": "/root", "sort_order": 0},
                {"id": pinned_id, "name": "Pinned", "path": "/root",
                 "parent_project_id": root_id, "sort_order": 0, "is_folder": true},
                {"id": "later", "name": "Later", "path": "/root",
                 "parent_project_id": root_id, "sort_order": 1, "is_folder": true},
                {"id": "manual", "name": "Pinned", "path": "/other",
                 "parent_project_id": "other", "sort_order": 0, "is_folder": true}
            ]
        });

        let removed = remove_unreferenced_legacy_pin_groups(
            state.as_object_mut().unwrap(),
            &std::collections::HashSet::new(),
        );

        assert_eq!(removed, vec![pinned_id.clone()]);
        let projects = state["projects"].as_array().unwrap();
        assert!(!projects.iter().any(|project| project["id"] == pinned_id));
        assert!(projects.iter().any(|project| project["id"] == "manual"));
        assert_eq!(
            projects
                .iter()
                .find(|project| project["id"] == "later")
                .unwrap()["sort_order"],
            0
        );
    }

    #[test]
    fn stale_pin_cleanup_keeps_referenced_or_nested_migration_groups() {
        let referenced_root = "referenced-root";
        let nested_root = "nested-root";
        let referenced_id = legacy_pinned_group_id(referenced_root);
        let nested_id = legacy_pinned_group_id(nested_root);
        let mut state = serde_json::json!({
            "projects": [
                {"id": referenced_root, "name": "Referenced", "path": "/referenced"},
                {"id": referenced_id, "name": "Pinned", "path": "/referenced",
                 "parent_project_id": referenced_root, "is_folder": true},
                {"id": nested_root, "name": "Nested", "path": "/nested"},
                {"id": nested_id, "name": "Pinned", "path": "/nested",
                 "parent_project_id": nested_root, "is_folder": true},
                {"id": "child", "name": "Child", "path": "/nested",
                 "parent_project_id": nested_id, "is_folder": true}
            ]
        });
        let referenced = [referenced_id.clone()].into_iter().collect();

        let removed =
            remove_unreferenced_legacy_pin_groups(state.as_object_mut().unwrap(), &referenced);

        assert!(removed.is_empty());
        assert_eq!(state["projects"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn session_order_replacement_keeps_the_exact_bucket_and_rank() {
        let mut order = serde_json::json!({
            "visual-group": ["before", "old", "after"],
            "manifest-project": ["other"],
            "future-metadata": {"old": "not an ordered id"}
        });
        assert!(rewrite_session_order_value(&mut order, "old", Some("new")));
        assert_eq!(
            order["visual-group"],
            serde_json::json!(["before", "new", "after"])
        );
        assert_eq!(order["manifest-project"], serde_json::json!(["other"]));
        assert_eq!(order["future-metadata"]["old"], "not an ordered id");
    }

    #[test]
    fn explicit_order_edit_does_not_touch_global_unpeel_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-order.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "p": ["old", "other"],
                "future": {"keep": true}
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(rewrite_session_order_references_at(&path, "old", None).unwrap());
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["p"], serde_json::json!(["other"]));
        assert_eq!(after["future"]["keep"], true);
    }

    #[test]
    fn session_order_preflight_rejects_valid_non_object_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-order.json");
        std::fs::write(&path, br#"["old", "other"]"#).unwrap();

        let error = validate_session_order_references_at(&path)
            .expect_err("a valid array is not the canonical order map");
        assert!(error.contains("not a JSON object"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), br#"["old", "other"]"#);
    }

    #[test]
    fn lifecycle_lock_targets_are_per_session_and_path_safe() {
        let dir = tempfile::tempdir().unwrap();
        let first = lifecycle_lock_target_at(dir.path(), "session/../one");
        let same = lifecycle_lock_target_at(dir.path(), "session/../one");
        let second = lifecycle_lock_target_at(dir.path(), "session-two");
        assert_eq!(first, same);
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(dir.path()));
        assert!(!first.file_name().unwrap().to_string_lossy().contains('/'));
        assert_eq!(
            first.with_extension("lock").file_name().unwrap(),
            "84c3933eeeef7825a674d522484afcb039087bf69fe534082b6ba70fdef25c82.lock"
        );
    }
}
