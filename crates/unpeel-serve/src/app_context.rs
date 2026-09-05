//! Host-owned execution context for the public App Kit.
//!
//! Apps must not parse manifests, app-state, native defaults, or the workspace
//! registry themselves. This module resolves those sources into one small,
//! versioned response shared by native and headless Hosts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use unpeel_core::session_host::HostedSessionManifest;
use unpeel_core::state::AppState;

use crate::overlay::NativeOverlay;

const CONTEXT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct WorkspaceContext {
    id: Option<String>,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ProjectContext {
    id: String,
    name: String,
    path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct WorktreeContext {
    path: String,
    branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct UserContext {
    id: String,
}

#[derive(Debug, Serialize)]
struct AppContextResponse {
    version: u32,
    session_id: String,
    workspace: Option<WorkspaceContext>,
    project: Option<ProjectContext>,
    worktree: Option<WorktreeContext>,
    user: Option<UserContext>,
}

#[derive(Clone, Debug)]
struct ProjectRecord {
    id: String,
    name: String,
    path: String,
    parent_id: Option<String>,
    worktree_branch: Option<String>,
}

#[derive(Deserialize)]
struct WorkspaceRegistry {
    #[serde(default, rename = "profiles")]
    workspaces: Vec<WorkspaceRecord>,
}

#[derive(Deserialize)]
struct WorkspaceRecord {
    id: String,
    name: String,
    home: String,
}

pub(crate) fn response_with_overlay(
    manifest: &HostedSessionManifest,
    overlay: Option<&NativeOverlay>,
) -> String {
    let app_state = crate::sessions::load_app_state();
    let records = project_records(app_state.as_ref(), overlay);
    let (project, worktree) = resolve_project(
        &manifest.session.project_id,
        manifest.session.worktree_path.as_deref(),
        manifest.session.worktree_branch.as_deref(),
        &manifest.cwd,
        &records,
    );
    let user_id = manifest.session.owner_principal_id.clone().or_else(|| {
        unpeel_core::relay_uplink::ensure_host_id()
            .ok()
            .map(|host_id| unpeel_core::state::host_owner_principal_id(&host_id))
    });
    let user = user_id
        .filter(|id| unpeel_core::state::valid_session_attribution_id(id))
        .map(|id| UserContext { id });
    let response = AppContextResponse {
        version: CONTEXT_VERSION,
        session_id: manifest.session.id.clone(),
        workspace: Some(current_workspace(overlay)),
        project,
        worktree,
        user,
    };
    serde_json::to_string(&response).unwrap_or_else(|_| {
        format!(
            r#"{{"version":{CONTEXT_VERSION},"session_id":"","workspace":null,"project":null,"worktree":null,"user":null}}"#
        )
    })
}

fn project_records(
    app_state: Option<&AppState>,
    overlay: Option<&NativeOverlay>,
) -> HashMap<String, ProjectRecord> {
    let mut records = HashMap::new();
    if let Some(app_state) = app_state {
        for project in &app_state.projects {
            records.insert(
                project.id.clone(),
                ProjectRecord {
                    id: project.id.clone(),
                    name: project.name.clone(),
                    path: project.path.clone(),
                    parent_id: project.parent_project_id.clone(),
                    worktree_branch: project.worktree_branch.clone(),
                },
            );
        }
    }
    if let Some(overlay) = overlay {
        for (id, name) in &overlay.projects {
            let path = overlay.project_paths.get(id).cloned().unwrap_or_default();
            let child = overlay.child_parents.get(id);
            let record = records.entry(id.clone()).or_insert_with(|| ProjectRecord {
                id: id.clone(),
                name: name.clone(),
                path: path.clone(),
                parent_id: child.map(|(parent, _)| parent.clone()),
                worktree_branch: child.and_then(|(_, branch)| branch.clone()),
            });
            record.name.clone_from(name);
            if !path.is_empty() {
                record.path = path;
            }
            if let Some((parent, branch)) = child {
                record.parent_id = Some(parent.clone());
                record.worktree_branch.clone_from(branch);
            }
        }
    }
    records
}

fn resolve_project(
    project_id: &str,
    session_worktree_path: Option<&str>,
    session_worktree_branch: Option<&str>,
    cwd: &str,
    records: &HashMap<String, ProjectRecord>,
) -> (Option<ProjectContext>, Option<WorktreeContext>) {
    let selected = records.get(project_id);
    let selected_is_worktree = selected
        .is_some_and(|project| project.parent_id.is_some() && project.worktree_branch.is_some());
    let base = if selected_is_worktree {
        selected
            .and_then(|project| project.parent_id.as_deref())
            .and_then(|parent| records.get(parent))
            .or(selected)
    } else {
        selected
    };
    let project = base.and_then(|project| {
        let path = absolute_wire_path(&project.path)?;
        valid_wire_text(&project.id, 256)
            .then_some(())
            .and_then(|()| valid_wire_text(&project.name, 1024).then_some(()))?;
        Some(ProjectContext {
            id: project.id.clone(),
            name: project.name.clone(),
            path,
        })
    });

    let selected_worktree_branch = selected_is_worktree
        .then(|| selected.and_then(|project| project.worktree_branch.clone()))
        .flatten();
    let selected_worktree_path = selected_is_worktree
        .then(|| selected.map(|project| project.path.clone()))
        .flatten();
    let branch = session_worktree_branch
        .map(str::to_owned)
        .or(selected_worktree_branch);
    let path = session_worktree_path
        .map(str::to_owned)
        .or(selected_worktree_path)
        .or_else(|| branch.is_some().then(|| cwd.to_owned()));
    let worktree = path.and_then(|path| {
        let path = absolute_wire_path(&path)?;
        if branch
            .as_deref()
            .is_some_and(|branch| !valid_wire_text(branch, 1024))
        {
            return None;
        }
        Some(WorktreeContext { path, branch })
    });
    (project, worktree)
}

fn current_workspace(overlay: Option<&NativeOverlay>) -> WorkspaceContext {
    let explicit_home = std::env::var_os("UNPEEL_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let real_unpeel = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".unpeel");
    workspace_at(explicit_home.as_deref(), &real_unpeel, overlay)
}

/// The name Controllers should show for THIS Host when it serves an isolated
/// workspace (`UNPEEL_HOME` other than the real `~/.unpeel`): the registered
/// workspace name — exactly what the desktop's workspace picker shows — so a
/// phone paired to two workspaces on one Mac can tell them apart instead of
/// seeing the hostname twice. `None` for the default workspace, which keeps
/// naming itself after the Mac.
pub(crate) fn isolated_workspace_name() -> Option<String> {
    let explicit_home = std::env::var_os("UNPEEL_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())?;
    let real_unpeel = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".unpeel");
    isolated_workspace_name_at(&explicit_home, &real_unpeel)
}

fn isolated_workspace_name_at(explicit_home: &Path, real_unpeel: &Path) -> Option<String> {
    if normalized_path(explicit_home) == normalized_path(real_unpeel) {
        return None;
    }
    Some(workspace_at(Some(explicit_home), real_unpeel, None).name)
}

fn workspace_at(
    explicit_home: Option<&Path>,
    real_unpeel: &Path,
    overlay: Option<&NativeOverlay>,
) -> WorkspaceContext {
    let Some(explicit_home) = explicit_home else {
        return WorkspaceContext {
            id: Some("default".to_string()),
            name: overlay
                .and_then(|overlay| overlay.default_workspace_name.clone())
                .filter(|name| valid_wire_text(name, 1024))
                .unwrap_or_else(|| "Personal".to_string()),
        };
    };
    let target = normalized_path(explicit_home);
    let registry = std::fs::read(real_unpeel.join("profiles.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<WorkspaceRegistry>(&raw).ok());
    if let Some(record) = registry.and_then(|registry| {
        registry
            .workspaces
            .into_iter()
            .find(|record| normalized_path(Path::new(&record.home)) == target)
    }) {
        if valid_wire_text(&record.name, 1024) {
            return WorkspaceContext {
                id: valid_wire_text(&record.id, 256).then_some(record.id),
                name: record.name,
            };
        }
    }
    let name = explicit_home
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_wire_text(name, 1024))
        .unwrap_or("Workspace")
        .to_string();
    WorkspaceContext { id: None, name }
}

fn normalized_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn absolute_wire_path(value: &str) -> Option<String> {
    (Path::new(value).is_absolute() && valid_wire_text(value, 16_384)).then(|| value.to_string())
}

fn valid_wire_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_children_resolve_to_the_base_project_and_checkout() {
        let records = HashMap::from([
            (
                "base".to_string(),
                ProjectRecord {
                    id: "base".into(),
                    name: "Unpeel".into(),
                    path: "/repo".into(),
                    parent_id: None,
                    worktree_branch: None,
                },
            ),
            (
                "child".to_string(),
                ProjectRecord {
                    id: "child".into(),
                    name: "Feature".into(),
                    path: "/repo-feature".into(),
                    parent_id: Some("base".into()),
                    worktree_branch: Some("feature/context".into()),
                },
            ),
        ]);
        let (project, worktree) = resolve_project("child", None, None, "/repo-feature", &records);
        assert_eq!(project.unwrap().path, "/repo");
        assert_eq!(
            worktree.unwrap(),
            WorktreeContext {
                path: "/repo-feature".into(),
                branch: Some("feature/context".into()),
            }
        );
    }

    #[test]
    fn workspace_resolution_uses_registry_and_default_overlay_names() {
        let root =
            std::env::temp_dir().join(format!("unpeel-app-context-{}", uuid::Uuid::new_v4()));
        let scoped = root.join("profiles/work");
        std::fs::create_dir_all(&scoped).unwrap();
        std::fs::write(
            root.join("profiles.json"),
            serde_json::json!({
                "version": 1,
                "profiles": [{"id":"workspace-1","name":"Work","home":scoped}]
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            workspace_at(Some(&scoped), &root, None),
            WorkspaceContext {
                id: Some("workspace-1".into()),
                name: "Work".into(),
            }
        );
        // The Controller-facing Host name follows the same registry: a
        // registered workspace is named like the desktop picker names it,
        // an unregistered home falls back to its folder, and the default
        // workspace keeps the hostname (None here).
        assert_eq!(
            isolated_workspace_name_at(&scoped, &root).as_deref(),
            Some("Work")
        );
        let unregistered = root.join("profiles/scratch");
        std::fs::create_dir_all(&unregistered).unwrap();
        assert_eq!(
            isolated_workspace_name_at(&unregistered, &root).as_deref(),
            Some("scratch")
        );
        assert_eq!(isolated_workspace_name_at(&root, &root), None);

        let overlay = NativeOverlay {
            default_workspace_name: Some("Personal renamed".into()),
            ..NativeOverlay::default()
        };
        assert_eq!(
            workspace_at(None, &root, Some(&overlay)),
            WorkspaceContext {
                id: Some("default".into()),
                name: "Personal renamed".into(),
            }
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
