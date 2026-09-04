//! Controller viewer presence owned by the canonical workspace Host.
//!
//! Authenticated Direct and Link output reads touch an in-memory lease here.
//! The worker publishes those leases beside the existing `__remote__` WSS
//! presence file so native clients can render one merged viewer surface while
//! remaining pure consumers. Notification policy reads the same leases; a
//! foreground phone suppresses only its own APNs target, never every device.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use unpeel_core::controller_api::ControllerPrincipal;

const MOBILE_VIEWER_TTL: Duration = Duration::from_secs(15);
const REMOTE_VIEWER_TTL: Duration = Duration::from_secs(20);
const PUBLISH_GRANULARITY: u64 = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct MobileViewer {
    device_id: String,
    name: String,
    last_seen_ms: u64,
}

#[derive(Default)]
struct PresenceState {
    /// Session id -> paired device id -> current lease.
    mobile: HashMap<String, HashMap<String, MobileViewer>>,
    last_published_bucket: HashMap<(String, String), u64>,
}

/// Shared by the Direct listener, Relay dispatcher, and Host activity loop.
pub struct PresenceHub {
    state: Mutex<PresenceState>,
    mobile_path: PathBuf,
    remote_path: PathBuf,
}

impl PresenceHub {
    pub fn new(home: &Path) -> Self {
        let remote_dir = home.join("remote");
        Self {
            state: Mutex::new(PresenceState::default()),
            mobile_path: remote_dir.join("mobile-presence.json"),
            remote_path: remote_dir.join("presence.json"),
        }
    }

    /// Record a successful authenticated terminal output read. Owner/local
    /// transports deliberately do not become remote viewers.
    pub fn touch_output(
        &self,
        session_id: &str,
        principal: &ControllerPrincipal,
        now_ms: u64,
    ) -> bool {
        let ControllerPrincipal::PairedDevice {
            device_id, name, ..
        } = principal
        else {
            return false;
        };
        if session_id.is_empty() || device_id.is_empty() {
            return false;
        }
        let mut publish = false;
        if let Ok(mut state) = self.state.lock() {
            prune_mobile(&mut state, now_ms);
            let key = (session_id.to_owned(), device_id.to_owned());
            let bucket = now_ms / PUBLISH_GRANULARITY;
            publish = state.last_published_bucket.get(&key).copied() != Some(bucket);
            state.last_published_bucket.insert(key, bucket);
            state
                .mobile
                .entry(session_id.to_owned())
                .or_default()
                .insert(
                    device_id.to_owned(),
                    MobileViewer {
                        device_id: device_id.to_owned(),
                        name: if name.trim().is_empty() {
                            "Phone".into()
                        } else {
                            name.to_owned()
                        },
                        last_seen_ms: now_ms,
                    },
                );
            if publish {
                let snapshot = mobile_wire(&state, now_ms);
                drop(state);
                let _ = publish_private_json(&self.mobile_path, &snapshot);
            }
        }
        publish
    }

    /// Remove expired leases and refresh the published file if that changed
    /// its visible contents. The Host tick calls this even when no new output
    /// arrives, so a disconnected viewer disappears without app-side logic.
    pub fn prune(&self, now_ms: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let changed = prune_mobile(&mut state, now_ms);
        if changed {
            let snapshot = mobile_wire(&state, now_ms);
            drop(state);
            let _ = publish_private_json(&self.mobile_path, &snapshot);
        }
        changed
    }

    /// Exact paired-device ids currently rendering this Session through
    /// Direct, Link, or the WSS terminal data plane.
    pub fn viewing_device_ids(&self, session_id: &str, now_ms: u64) -> HashSet<String> {
        let mut ids = HashSet::new();
        if let Ok(mut state) = self.state.lock() {
            prune_mobile(&mut state, now_ms);
            if let Some(viewers) = state.mobile.get(session_id) {
                ids.extend(viewers.keys().cloned());
            }
        }
        ids.extend(remote_viewing_device_ids(
            &self.remote_path,
            session_id,
            now_ms,
        ));
        ids
    }

    /// Any live viewer (including an identity-less legacy WSS client) counts
    /// as observing for the local unread/banner policy.
    pub fn any_viewer(&self, session_id: &str, now_ms: u64) -> bool {
        if !self.viewing_device_ids(session_id, now_ms).is_empty() {
            return true;
        }
        remote_has_any_viewer(&self.remote_path, session_id, now_ms)
    }

    #[cfg(test)]
    fn mobile_path(&self) -> &Path {
        &self.mobile_path
    }
}

fn prune_mobile(state: &mut PresenceState, now_ms: u64) -> bool {
    let cutoff = now_ms.saturating_sub(MOBILE_VIEWER_TTL.as_millis() as u64);
    let before = state.mobile.values().map(HashMap::len).sum::<usize>();
    state.mobile.retain(|session_id, viewers| {
        viewers.retain(|device_id, viewer| {
            let live = viewer.last_seen_ms >= cutoff;
            if !live {
                state
                    .last_published_bucket
                    .remove(&(session_id.clone(), device_id.clone()));
            }
            live
        });
        !viewers.is_empty()
    });
    before != state.mobile.values().map(HashMap::len).sum::<usize>()
}

fn mobile_wire(state: &PresenceState, now_ms: u64) -> Value {
    let mut sessions = BTreeMap::<String, Vec<Value>>::new();
    for (session_id, viewers) in &state.mobile {
        let mut viewers = viewers.values().cloned().collect::<Vec<_>>();
        viewers.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        sessions.insert(
            session_id.clone(),
            viewers
                .into_iter()
                .map(|viewer| {
                    json!({
                        "ip": Value::Null,
                        "kind": "mobile",
                        "device": format!("{} ({})", viewer.name, viewer.device_id),
                        "last_seen": viewer.last_seen_ms,
                    })
                })
                .collect(),
        );
    }
    json!({ "version": 1, "updated_at": now_ms, "sessions": sessions })
}

fn remote_presence(path: &Path) -> Option<Value> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn remote_entries<'a>(value: &'a Value, session_id: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get("sessions")
        .and_then(Value::as_object)
        .and_then(|sessions| sessions.get(session_id))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn remote_entry_is_live(entry: &Value, now_ms: u64) -> bool {
    let cutoff = now_ms.saturating_sub(REMOTE_VIEWER_TTL.as_millis() as u64);
    entry
        .get("last_seen")
        .and_then(Value::as_u64)
        .is_some_and(|last_seen| last_seen >= cutoff)
}

fn remote_viewing_device_ids(path: &Path, session_id: &str, now_ms: u64) -> HashSet<String> {
    let Some(value) = remote_presence(path) else {
        return HashSet::new();
    };
    remote_entries(&value, session_id)
        .filter(|entry| remote_entry_is_live(entry, now_ms))
        .filter_map(|entry| entry.get("device").and_then(Value::as_str))
        .filter_map(device_id_from_label)
        .map(str::to_owned)
        .collect()
}

fn remote_has_any_viewer(path: &Path, session_id: &str, now_ms: u64) -> bool {
    remote_presence(path).is_some_and(|value| {
        remote_entries(&value, session_id).any(|entry| remote_entry_is_live(entry, now_ms))
    })
}

fn device_id_from_label(label: &str) -> Option<&str> {
    let open = label.rfind(" (")?;
    let id = label.get(open + 2..label.len().checked_sub(1)?)?;
    (label.ends_with(')') && !id.is_empty()).then_some(id)
}

fn publish_private_json(path: &Path, value: &Value) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec(value)?;
    let temporary = path.with_file_name(format!(
        ".mobile-presence.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&body)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str) -> ControllerPrincipal {
        ControllerPrincipal::PairedDevice {
            device_id: id.into(),
            name: name.into(),
            principal_id: None,
        }
    }

    #[test]
    fn mobile_presence_is_exact_per_device_published_and_expires() {
        let home = tempfile::tempdir().unwrap();
        let hub = PresenceHub::new(home.path());
        hub.touch_output("session-a", &device("phone-1", "Tommy's iPhone"), 100_000);
        hub.touch_output("session-a", &device("ipad-1", "iPad"), 100_001);
        assert_eq!(
            hub.viewing_device_ids("session-a", 100_002),
            HashSet::from(["phone-1".into(), "ipad-1".into()])
        );
        let wire: Value =
            serde_json::from_slice(&std::fs::read(hub.mobile_path()).unwrap()).unwrap();
        assert_eq!(wire["sessions"]["session-a"].as_array().unwrap().len(), 2);
        assert_eq!(
            wire["sessions"]["session-a"][1]["device"],
            "Tommy's iPhone (phone-1)"
        );

        assert!(hub.prune(116_000));
        assert!(!hub.any_viewer("session-a", 116_000));
        let wire: Value =
            serde_json::from_slice(&std::fs::read(hub.mobile_path()).unwrap()).unwrap();
        assert!(wire["sessions"].as_object().unwrap().is_empty());
    }

    #[test]
    fn remote_presence_contributes_exact_devices_and_identity_less_viewers() {
        let home = tempfile::tempdir().unwrap();
        let remote = home.path().join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(
            remote.join("presence.json"),
            br#"{"version":1,"sessions":{"s1":[{"device":"Phone (phone-1)","last_seen":100000},{"ip":"10.0.0.2","last_seen":100001}]}}"#,
        )
        .unwrap();
        let hub = PresenceHub::new(home.path());
        assert_eq!(
            hub.viewing_device_ids("s1", 100_002),
            HashSet::from(["phone-1".into()])
        );
        assert!(hub.any_viewer("s1", 100_002));
        assert!(!hub.any_viewer("s1", 121_000));
    }

    #[test]
    fn owner_transport_never_becomes_a_remote_viewer() {
        let home = tempfile::tempdir().unwrap();
        let hub = PresenceHub::new(home.path());
        hub.touch_output(
            "s1",
            &ControllerPrincipal::OwnerTransport {
                transport: "local".into(),
                subject: None,
                principal_id: None,
            },
            100,
        );
        assert!(!hub.any_viewer("s1", 100));
        assert!(!hub.mobile_path().exists());
    }
}
