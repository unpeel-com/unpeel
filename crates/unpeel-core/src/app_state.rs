//! Guarded access to the shared `~/.unpeel/app-state.json`.
//!
//! This file is a **cross-frontend, cross-version contract**: the desktop app
//! owns keys the Rust side has never heard of (theme, active tabs, pins,
//! whatever ships next), and a user running an older app against a newer
//! `unpeel` — or the reverse — must not lose any of them. Two rules make that
//! safe, and every writer goes through here so they can't be forgotten:
//!
//! 1. **Never clobber a file we failed to understand.** A missing file is a
//!    fresh install and seeds an empty skeleton; a file that exists but won't
//!    read or parse is an error, never an excuse to start over. Getting this
//!    wrong costs the user every project and preset they had.
//! 2. **Never drop a top-level key.** Writes are whole-document, so a typed
//!    round-trip anywhere upstream would silently delete unmodelled keys.
//!    `save` refuses a document that lost one.
//!
//! Edits are untyped (`serde_json::Value`) for the same reason: what we don't
//! model, we pass through untouched.

use std::path::PathBuf;

use serde_json::{Map, Value};

/// An exclusive advisory lock on `<file>.lock`, held for the duration of a
/// read-modify-write. Atomic rename already prevents torn files; this
/// prevents LOST UPDATES — two frontends editing concurrently, the second
/// read happening before the first write, last-writer silently dropping the
/// other's change. It is the strongest argument for a central daemon, and a
/// flock answers it without one. Held via RAII: released on drop, and by
/// the OS if the process dies mid-edit.
pub(crate) struct FileLock(#[allow(dead_code)] std::fs::File);

pub(crate) fn lock_exclusive(target: &std::path::Path) -> Result<FileLock, String> {
    use std::os::fd::AsRawFd;
    let lock_path = target.with_extension("lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open {}: {e}", lock_path.display()))?;
    // Blocking acquire: edits are tiny (read + write of a small JSON file),
    // so waiting beats failing.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "lock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(FileLock(file))
}

fn path() -> PathBuf {
    crate::app_paths::app_state_path()
}

/// The skeleton a genuinely fresh install starts from.
fn empty_state() -> Value {
    serde_json::json!({
        "projects": [],
        "active_project_id": null,
        "presets": [],
        "active_tabs": {},
        "pinned_sessions": {},
    })
}

/// Load for modification. Missing file → skeleton. Present but unreadable or
/// unparseable → `Err`, so callers surface a problem instead of overwriting
/// state they couldn't read.
pub fn load_for_edit() -> Result<Value, String> {
    load_for_edit_at(&path())
}

/// The same rules against an explicit path. Tests use this so they never
/// mutate `UNPEEL_HOME`, which is process-global: swapping it mid-run
/// corrupts whatever other test happens to read a path at that moment.
pub fn load_for_edit_at(path: &std::path::Path) -> Result<Value, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(empty_state()),
        Err(err) => return Err(format!("read {}: {err}", path.display())),
    };
    // An empty file is what a half-finished write leaves behind; treat it as
    // fresh rather than failing forever.
    if raw.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(empty_state());
    }
    let value: Value =
        serde_json::from_slice(&raw).map_err(|err| format!("parse {}: {err}", path.display()))?;
    if !value.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }
    Ok(value)
}

/// Read-only load. Same rules, but callers that only render tolerate a
/// missing file; a broken one still errors.
pub fn load() -> Result<Value, String> {
    load_for_edit()
}

/// Write atomically, refusing any document that dropped a top-level key the
/// on-disk file had. That's the regression that would quietly delete a
/// desktop-owned setting the Rust side doesn't model.
pub fn save(state: &Value) -> Result<(), String> {
    save_at(&path(), state)?;
    // The one choke point every Rust-side app-state write flows through
    // (projects, presets, pins, approval grants): tell the other frontends.
    // Deliberately NOT in `save_at` — unit tests use the `_at` variants
    // against temp paths and must never ping this machine's real registry.
    crate::state_bus::announce(
        crate::state_bus::Change::AppState,
        crate::session_ops::own_listener_port_public(),
    );
    Ok(())
}

/// `save` against an explicit path — see `load_for_edit_at`.
pub fn save_at(path: &std::path::Path, state: &Value) -> Result<(), String> {
    let object = state
        .as_object()
        .ok_or_else(|| "refusing to write a non-object app-state".to_string())?;
    if let Ok(existing) = std::fs::read(path) {
        if let Ok(Value::Object(previous)) = serde_json::from_slice::<Value>(&existing) {
            let dropped: Vec<&String> = previous
                .keys()
                .filter(|key| !object.contains_key(*key))
                .collect();
            if !dropped.is_empty() {
                return Err(format!(
                    "refusing to write app-state.json: would drop {}",
                    dropped
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    let body = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Same-directory temp + rename: a reader either sees the old file or the
    // new one, never a partial write.
    let tmp = path.with_extension("json.unpeel-tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Load → mutate → save, with both guards applied. The edit sees the raw
/// object map, so unmodelled keys pass through untouched.
pub fn edit<T>(
    mutate: impl FnOnce(&mut Map<String, Value>) -> Result<T, String>,
) -> Result<T, String> {
    // Through `save()`, not `edit_at` → `save_at`: `save()` is the choke
    // point that announces the change to other frontends, and delegating
    // to the path-taking variant silently skipped it.
    let _lock = lock_exclusive(&path())?;
    let mut state = load_for_edit()?;
    let object = state
        .as_object_mut()
        .ok_or_else(|| "app-state.json is not an object".to_string())?;
    let outcome = mutate(object)?;
    save(&state)?;
    Ok(outcome)
}

/// `edit` against an explicit path — see `load_for_edit_at`.
pub fn edit_at<T>(
    path: &std::path::Path,
    mutate: impl FnOnce(&mut Map<String, Value>) -> Result<T, String>,
) -> Result<T, String> {
    let _lock = lock_exclusive(path)?;
    let mut state = load_for_edit_at(path)?;
    let object = state
        .as_object_mut()
        .ok_or_else(|| "app-state.json is not an object".to_string())?;
    let outcome = mutate(object)?;
    save_at(path, &state)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private state file. Deliberately does NOT touch `UNPEEL_HOME`:
    /// that variable is process-global, and cargo runs tests in parallel
    /// threads, so swapping it corrupts whatever other test happens to
    /// resolve a path at that moment (it did — one run in five failed
    /// somewhere unrelated). The `_at` variants exist for exactly this.
    fn with_state<T>(body: impl FnOnce(&std::path::Path) -> T) -> T {
        let dir = std::env::temp_dir().join(format!("unpeel-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let outcome = body(&dir.join("app-state.json"));
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    #[test]
    fn missing_file_is_a_fresh_install() {
        with_state(|path| {
            let state = load_for_edit_at(path).expect("missing file seeds a skeleton");
            assert_eq!(state["projects"], serde_json::json!([]));
        });
    }

    #[test]
    fn corrupt_file_errors_instead_of_clobbering() {
        with_state(|path| {
            std::fs::write(path, b"{ this is not json").unwrap();
            let error = load_for_edit_at(path).expect_err("must not pretend it's fresh");
            assert!(error.contains("parse"), "{error}");
            // The user's bytes are still on disk, untouched.
            assert_eq!(std::fs::read(path).unwrap(), b"{ this is not json");
        });
    }

    #[test]
    fn edit_preserves_keys_the_rust_side_does_not_model() {
        with_state(|path| {
            // Everything a shipped desktop might own, including keys added
            // after this code was written.
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "projects": [{"id": "p", "name": "unpeel", "path": "/tmp"}],
                    "presets": [],
                    "theme": "midnight",
                    "active_tabs": {"p": "s1"},
                    "pinned_sessions": {"p": ["s1"]},
                    "some_future_key": {"nested": [1, 2, 3]},
                }))
                .unwrap(),
            )
            .unwrap();

            edit_at(path, |state| {
                state.insert("presets".into(), serde_json::json!([{"id": "x"}]));
                Ok(())
            })
            .expect("edit succeeds");

            let after: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(after["theme"], "midnight");
            assert_eq!(after["some_future_key"]["nested"][2], 3);
            assert_eq!(after["pinned_sessions"]["p"][0], "s1");
            assert_eq!(after["projects"][0]["name"], "unpeel");
            assert_eq!(after["presets"][0]["id"], "x");
        });
    }

    #[test]
    fn save_refuses_to_drop_a_top_level_key() {
        with_state(|path| {
            std::fs::write(
                path,
                serde_json::to_vec(&serde_json::json!({
                    "projects": [], "presets": [], "theme": "midnight",
                }))
                .unwrap(),
            )
            .unwrap();
            // Simulates a typed round-trip that forgot `theme`.
            let lossy = serde_json::json!({ "projects": [], "presets": [] });
            let error = save_at(path, &lossy).expect_err("must refuse");
            assert!(error.contains("theme"), "{error}");
            let after: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(after["theme"], "midnight");
        });
    }

    #[test]
    fn empty_file_is_treated_as_fresh() {
        with_state(|path| {
            std::fs::write(path, b"   \n").unwrap();
            let state = load_for_edit_at(path).expect("blank file is not corruption");
            assert!(state["projects"].as_array().unwrap().is_empty());
        });
    }

    #[test]
    fn concurrent_edits_lose_nothing() {
        // The lost-update race the lock exists for: two writers interleaving
        // read-modify-write. Without the flock this drops entries almost
        // every run; with it, all 40 survive deterministically.
        with_state(|path| {
            let path = path.to_path_buf();
            std::fs::write(&path, b"{\"projects\": []}").unwrap();
            let workers: Vec<_> = (0..4)
                .map(|worker| {
                    let path = path.clone();
                    std::thread::spawn(move || {
                        for round in 0..10 {
                            edit_at(&path, |state| {
                                let list = state
                                    .get_mut("projects")
                                    .and_then(|v| v.as_array_mut())
                                    .unwrap();
                                list.push(serde_json::json!(format!("w{worker}-{round}")));
                                Ok(())
                            })
                            .unwrap();
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap();
            }
            let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                after["projects"].as_array().unwrap().len(),
                40,
                "an edit was lost to a concurrent writer"
            );
        });
    }

    #[test]
    fn writes_are_atomic_and_leave_no_temp_behind() {
        with_state(|path| {
            edit_at(path, |state| {
                state.insert("projects".into(), serde_json::json!([{"id": "p"}]));
                Ok(())
            })
            .unwrap();
            let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains("tmp"))
                .collect();
            assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        });
    }
}
