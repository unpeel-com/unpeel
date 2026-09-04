//! Read-only view of the native app's UserDefaults overlay
//! (`com.unpeel.native`): native projects + project order, pins, per-project
//! manual session order, title renames, and the archived set. This is what
//! lets the disk-fallback sidebar match the desktop when the running app
//! build has no `/mcp/sidebar` route (or no app runs at all). Never written.
//!
//! Skipped entirely when `UNPEEL_HOME` is set: isolated workspace and blank
//! instances use a home-derived defaults suite, and test fixtures must not
//! inherit the real app's organization.

use std::collections::{HashMap, HashSet};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::{Arc, RwLock};

use base64::Engine;

const MAX_ADAPTER_PLIST_BYTES: usize = 320 * 1024;
const MAX_ADAPTER_PLIST_B64_BYTES: usize = MAX_ADAPTER_PLIST_BYTES.div_ceil(3) * 4;

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct NativeOverlay {
    /// Workspace App color (`AppTint.rawValue`), used as the hosted App
    /// accent when a Session's project has no folder color.
    pub app_tint: Option<String>,
    /// User-visible name of the implicit/default workspace. Isolated
    /// workspaces resolve through the shared profiles registry instead.
    pub default_workspace_name: Option<String>,
    /// Native-created projects: id → name.
    pub projects: Vec<(String, String)>,
    /// Their paths, id → path. Needed to answer "is this folder already a
    /// project?" — the app's projects live only here, so a check against
    /// `app-state.json` alone happily adds a duplicate.
    pub project_paths: std::collections::HashMap<String, String>,
    /// Child projects: project id → (parent project id, worktree branch —
    /// `None` for plain organizational groups, which are children too).
    pub child_parents: HashMap<String, (String, Option<String>)>,
    pub project_order: Vec<String>,
    /// session id → pinned_at (merged `added` pins; `removedKeys` applied).
    pub pins: HashMap<String, u64>,
    pub pinned_order: HashMap<String, Vec<String>>,
    pub titles: HashMap<String, String>,
    pub archived: HashSet<String>,
    pub archived_at: HashMap<String, u64>,
    pub session_order: HashMap<String, Vec<String>>,
    /// Native preset additions/edits (enabled only): (label, command).
    pub presets: Vec<(String, String)>,
    /// Project folder colors: project id → `ProjectFolderColor.rawValue`
    /// ("sky", "blue", …). Set in the app's context menu; display-only here.
    pub project_colors: HashMap<String, String>,
}

/// One cached native overlay shared with the hook listener. The Host driver
/// owns refresh timing and generation checks; request threads only clone the
/// last accepted value, so `/app-theme` and `/app-context` never block on an
/// app callback or read another workspace's defaults domain.
#[derive(Clone, Default)]
pub struct SharedNativeOverlay(Arc<RwLock<Option<NativeOverlay>>>);

impl SharedNativeOverlay {
    pub fn new(value: Option<NativeOverlay>) -> Self {
        Self(Arc::new(RwLock::new(value)))
    }

    pub fn snapshot(&self) -> Option<NativeOverlay> {
        self.0.read().ok().and_then(|value| value.clone())
    }

    pub fn replace(&self, value: Option<NativeOverlay>) {
        if let Ok(mut current) = self.0.write() {
            *current = value;
        }
    }
}

fn as_string_array(value: &plist::Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_string().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn blob_json(value: &plist::Value) -> Option<serde_json::Value> {
    serde_json::from_slice(value.as_data()?).ok()
}

pub fn load() -> Option<NativeOverlay> {
    // The overlay is the macOS app's UserDefaults domain: absent by
    // definition on a Linux host (and in isolated workspaces), where the
    // shared on-disk contract is the whole truth.
    #[cfg(not(target_os = "macos"))]
    return None;
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os("UNPEEL_HOME").is_some_and(|v| !v.is_empty()) {
            return None;
        }
        let output = Command::new("defaults")
            .args(["export", "com.unpeel.native", "-"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let root: plist::Value = plist::from_bytes(&output.stdout).ok()?;
        from_plist(&root)
    }
}

/// Decode the allowlisted defaults plist returned by the native adapter.
/// Both the base64 and decoded forms are bounded before plist parsing; the
/// callback cannot smuggle unrelated defaults because Swift constructs the
/// plist from an exact key allowlist.
pub fn from_adapter_response(value: &serde_json::Value) -> Result<NativeOverlay, String> {
    let object = value
        .as_object()
        .filter(|object| object.len() == 1)
        .ok_or("invalid native overlay response")?;
    let encoded = object
        .get("defaultsPlistBase64")
        .and_then(serde_json::Value::as_str)
        .filter(|encoded| encoded.len() <= MAX_ADAPTER_PLIST_B64_BYTES)
        .ok_or("invalid native overlay response")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "invalid native overlay encoding")?;
    if bytes.len() > MAX_ADAPTER_PLIST_BYTES {
        return Err("native overlay is too large".into());
    }
    let plist: plist::Value =
        plist::from_bytes(&bytes).map_err(|_| "invalid native overlay plist")?;
    from_plist(&plist).ok_or_else(|| "invalid native overlay plist".into())
}

/// Whether this Host can persist folder colors at all: they live in the
/// desktop app's UserDefaults domain, which does not exist off macOS or in
/// isolated workspaces (`UNPEEL_HOME`).
pub fn project_folder_color_supported() -> bool {
    cfg!(target_os = "macos")
        && std::env::var_os("UNPEEL_HOME").is_none_or(|value| value.is_empty())
}

/// Write a folder color into the desktop app's UserDefaults — the same store
/// the app's color picker and the TUI's own context menu write, so every
/// frontend reads one truth. Used by the headless Controller route
/// (`project.organization.set`); the interactive path in `main.rs` keeps its
/// own copy because it also updates the in-memory overlay and status line.
/// Colors do not exist off macOS or in isolated workspaces (`UNPEEL_HOME`),
/// where the overlay itself is skipped — report unsupported instead of
/// writing another instance's defaults domain.
pub fn write_project_folder_color(project_id: &str, color: Option<&str>) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (project_id, color);
        Err("folder colors need the desktop app".into())
    }
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os("UNPEEL_HOME").is_some_and(|v| !v.is_empty()) {
            return Err("folder colors are not supported by this Host".into());
        }
        const DOMAIN: &str = "com.unpeel.native";
        const KEY: &str = "unpeel.native.projectFolderColors";
        let run = |args: &[&str]| {
            Command::new("defaults")
                .args(args)
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        };
        let ok = match color {
            Some(color) => run(&["write", DOMAIN, KEY, "-dict-add", project_id, color]),
            None => {
                // No per-key delete in `defaults`: rewrite the dict whole.
                let mut colors = load().map(|o| o.project_colors).unwrap_or_default();
                colors.remove(project_id);
                if colors.is_empty() {
                    // Deleting an already-absent key fails; that's still done.
                    run(&["delete", DOMAIN, KEY]);
                    true
                } else {
                    let mut args: Vec<String> =
                        vec!["write".into(), DOMAIN.into(), KEY.into(), "-dict".into()];
                    for (key, value) in &colors {
                        args.push(key.clone());
                        args.push(value.clone());
                    }
                    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    run(&refs)
                }
            }
        };
        if ok {
            Ok(())
        } else {
            Err("could not save the folder color".into())
        }
    }
}

/// Parse an exported `com.unpeel.native` defaults plist. Split from `load()`
/// so the dialect (blob JSON key spellings, pin tombstones) is testable
/// without a real defaults domain.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[allow(clippy::field_reassign_with_default)]
pub(crate) fn from_plist(root: &plist::Value) -> Option<NativeOverlay> {
    let dict = root.as_dictionary()?;
    let mut overlay = NativeOverlay::default();

    overlay.app_tint = dict
        .get("unpeel.native.appTint")
        .and_then(plist::Value::as_string)
        .filter(|raw| workspace_tint_hex(raw).is_some())
        .map(str::to_owned);
    overlay.default_workspace_name = dict
        .get("unpeel.native.defaultWorkspaceName")
        .and_then(plist::Value::as_string)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);

    if let Some(projects) = dict.get("unpeel.native.projects").and_then(blob_json) {
        if let Some(list) = projects.as_array() {
            for p in list {
                if let (Some(id), Some(name)) = (
                    p.get("id").and_then(|v| v.as_str()),
                    p.get("name").and_then(|v| v.as_str()),
                ) {
                    overlay.projects.push((id.to_string(), name.to_string()));
                    if let Some(path) = p.get("path").and_then(|v| v.as_str()) {
                        overlay
                            .project_paths
                            .insert(id.to_string(), path.to_string());
                    }
                    // The native app encodes Project with camelCase keys
                    // (`parentProjectID`/`worktreeBranch`); snake_case is the
                    // app-state.json dialect, kept as a fallback. A parent
                    // with no branch is a plain group — still a child.
                    if let Some(parent) = p
                        .get("parentProjectID")
                        .or_else(|| p.get("parent_project_id"))
                        .and_then(|v| v.as_str())
                    {
                        let branch = p
                            .get("worktreeBranch")
                            .or_else(|| p.get("worktree_branch"))
                            .and_then(|v| v.as_str())
                            .map(str::to_owned);
                        overlay
                            .child_parents
                            .insert(id.to_string(), (parent.to_string(), branch));
                    }
                }
            }
        }
    }
    if let Some(order) = dict.get("unpeel.native.projectOrder") {
        overlay.project_order = as_string_array(order);
    }
    if let Some(pins) = dict.get("unpeel.sidebar.pins").and_then(blob_json) {
        let removed: HashSet<&str> = pins
            .get("removedKeys")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if let Some(added) = pins.get("added").and_then(|v| v.as_array()) {
            for pin in added {
                let key = pin.get("key").and_then(|v| v.as_str()).unwrap_or_default();
                if removed.contains(key) {
                    continue;
                }
                if let Some(id) = pin.get("session_id").and_then(|v| v.as_str()) {
                    let at = pin.get("pinned_at").and_then(|v| v.as_u64()).unwrap_or(0);
                    overlay.pins.insert(id.to_string(), at);
                }
            }
        }
    }
    if let Some(titles) = dict
        .get("unpeel.native.sessionTitles")
        .and_then(|v| v.as_dictionary())
    {
        for (id, title) in titles {
            if let Some(t) = title.as_string() {
                overlay.titles.insert(id.clone(), t.to_string());
            }
        }
    }
    if let Some(colors) = dict
        .get("unpeel.native.projectFolderColors")
        .and_then(|v| v.as_dictionary())
    {
        for (id, color) in colors {
            if let Some(c) = color.as_string() {
                overlay.project_colors.insert(id.clone(), c.to_string());
            }
        }
    }
    if let Some(archived) = dict.get("unpeel.native.archivedSessions") {
        overlay.archived = as_string_array(archived).into_iter().collect();
    }
    if let Some(at) = dict
        .get("unpeel.native.archivedAt")
        .and_then(|v| v.as_dictionary())
    {
        for (id, stamp) in at {
            if let Some(ms) = stamp.as_unsigned_integer() {
                overlay.archived_at.insert(id.clone(), ms);
            }
        }
    }
    if let Some(presets) = dict.get("unpeel.native.presets").and_then(blob_json) {
        if let Some(added) = presets.get("added").and_then(|v| v.as_array()) {
            for p in added {
                if p.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
                    continue;
                }
                if let (Some(label), Some(command)) = (
                    p.get("label").and_then(|v| v.as_str()),
                    p.get("command").and_then(|v| v.as_str()),
                ) {
                    overlay
                        .presets
                        .push((label.to_string(), command.to_string()));
                }
            }
        }
    }
    for (key, value) in dict {
        if let Some(project_id) = key.strip_prefix("unpeel.native.sessionOrder.") {
            overlay
                .session_order
                .insert(project_id.to_string(), as_string_array(value));
        } else if let Some(project_id) = key.strip_prefix("unpeel.native.pinnedOrder.") {
            overlay
                .pinned_order
                .insert(project_id.to_string(), as_string_array(value));
        }
    }
    Some(overlay)
}

/// Resolve the live accent served to an App Session. A child project inherits
/// the nearest colored parent, then falls back to the workspace App tint.
pub fn hosted_app_accent(
    overlay: &NativeOverlay,
    project_id: &str,
    is_dark: bool,
) -> Option<String> {
    let mut current = Some(project_id);
    let mut visited = HashSet::new();
    while let Some(project_id) = current.filter(|id| visited.insert((*id).to_owned())) {
        if let Some(raw) = overlay.project_colors.get(project_id) {
            if let Some(hex) = project_folder_hex(raw, is_dark) {
                return Some(hex.to_owned());
            }
        }
        current = overlay
            .child_parents
            .get(project_id)
            .map(|(parent, _)| parent.as_str());
    }
    overlay
        .app_tint
        .as_deref()
        .and_then(workspace_tint_hex)
        .map(str::to_owned)
}

fn project_folder_hex(raw: &str, is_dark: bool) -> Option<&'static str> {
    Some(match (raw, is_dark) {
        ("sky", false) => "#2095C9",
        ("sky", true) => "#7DD3FC",
        ("blue", false) => "#4F73E6",
        ("blue", true) => "#7EA6FF",
        ("violet", false) => "#7B5BDA",
        ("violet", true) => "#B79CFF",
        ("rose", false) => "#D75F8F",
        ("rose", true) => "#F79AC0",
        ("amber", false) => "#B87511",
        ("amber", true) => "#F8C86A",
        ("moss", false) => "#5F9A3D",
        ("moss", true) => "#9DD67A",
        ("teal", false) => "#159B91",
        ("teal", true) => "#64DCCB",
        ("graphite", false) => "#687083",
        ("graphite", true) => "#B8BCC8",
        _ => return None,
    })
}

fn workspace_tint_hex(raw: &str) -> Option<&'static str> {
    Some(match raw {
        "peel" => "#D97757",
        "amber" => "#E3A63B",
        "green" => "#3FBF63",
        "teal" => "#4EC3C9",
        "blue" => "#4FA8FF",
        "indigo" => "#7A7EF2",
        "violet" => "#B166E8",
        "none" => return None,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_parents_read_the_native_camel_case_dialect() {
        // `NativeProjectRecord` in UnpeelStore.swift has no CodingKeys, so
        // the desktop writes camelCase — snake_case here would silently
        // orphan every desktop-created worktree into the top-level list.
        let projects = serde_json::json!([
            {"id": "p1", "name": "unpeel", "path": "/tmp/unpeel"},
            {"id": "w1", "name": "Example", "path": "/tmp/wt",
             "parentProjectID": "p1", "worktreeBranch": "worktree/example"},
            {"id": "w2", "name": "legacy", "path": "/tmp/wt2",
             "parent_project_id": "p1", "worktree_branch": "legacy"},
            // A group: parent, no branch. Must still be read as a child.
            {"id": "g1", "name": "Backlog", "path": "/tmp/unpeel",
             "parentProjectID": "p1", "isFolder": true},
        ]);
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "unpeel.native.projects".into(),
            plist::Value::Data(serde_json::to_vec(&projects).unwrap()),
        );
        let overlay = from_plist(&plist::Value::Dictionary(dict)).expect("parses");

        assert_eq!(overlay.projects.len(), 4);
        assert_eq!(
            overlay.child_parents.get("w1"),
            Some(&("p1".to_string(), Some("worktree/example".to_string())))
        );
        assert_eq!(
            overlay.child_parents.get("w2"),
            Some(&("p1".to_string(), Some("legacy".to_string())))
        );
        assert_eq!(
            overlay.child_parents.get("g1"),
            Some(&("p1".to_string(), None))
        );
        assert!(!overlay.child_parents.contains_key("p1"));
    }

    #[test]
    fn project_folder_colors_read_the_plain_dictionary() {
        // Colors are the one overlay key stored as a plain plist dict
        // (`AppDefaults.dictionary`), not a JSON blob.
        let mut colors = plist::Dictionary::new();
        colors.insert("p1".into(), plist::Value::String("sky".into()));
        colors.insert("p2".into(), plist::Value::String("graphite".into()));
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "unpeel.native.projectFolderColors".into(),
            plist::Value::Dictionary(colors),
        );
        let overlay = from_plist(&plist::Value::Dictionary(dict)).expect("parses");

        assert_eq!(overlay.project_colors.get("p1"), Some(&"sky".to_string()));
        assert_eq!(
            overlay.project_colors.get("p2"),
            Some(&"graphite".to_string())
        );
    }

    #[test]
    fn platform_snapshot_decodes_the_same_bounded_plist_dialect() {
        let mut titles = plist::Dictionary::new();
        titles.insert("s1".into(), plist::Value::String("Native title".into()));
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "unpeel.native.appTint".into(),
            plist::Value::String("teal".into()),
        );
        dict.insert(
            "unpeel.native.sessionTitles".into(),
            plist::Value::Dictionary(titles),
        );
        let mut bytes = Vec::new();
        plist::to_writer_binary(&mut bytes, &plist::Value::Dictionary(dict)).unwrap();
        let response = serde_json::json!({
            "defaultsPlistBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        let overlay = from_adapter_response(&response).unwrap();
        assert_eq!(overlay.app_tint.as_deref(), Some("teal"));
        assert_eq!(
            overlay.titles.get("s1").map(String::as_str),
            Some("Native title")
        );

        let with_unknown = serde_json::json!({
            "defaultsPlistBase64": "AA==",
            "unexpected": true,
        });
        assert!(from_adapter_response(&with_unknown).is_err());
        let too_large = serde_json::json!({
            "defaultsPlistBase64": "A".repeat(MAX_ADAPTER_PLIST_B64_BYTES + 1),
        });
        assert!(from_adapter_response(&too_large).is_err());
    }

    #[test]
    fn hosted_accent_prefers_project_parent_then_workspace() {
        let mut overlay = NativeOverlay {
            app_tint: Some("violet".into()),
            ..NativeOverlay::default()
        };
        overlay.child_parents.insert(
            "worktree".into(),
            ("project".into(), Some("feature".into())),
        );
        overlay
            .project_colors
            .insert("project".into(), "teal".into());

        assert_eq!(
            hosted_app_accent(&overlay, "worktree", true).as_deref(),
            Some("#64DCCB")
        );
        assert_eq!(
            hosted_app_accent(&overlay, "worktree", false).as_deref(),
            Some("#159B91")
        );
        overlay.project_colors.clear();
        assert_eq!(
            hosted_app_accent(&overlay, "worktree", true).as_deref(),
            Some("#B166E8")
        );
    }
}
