//! Shared first-run seeding: what a brand-new Unpeel install should show
//! before the user configures anything. The desktop does this in Swift at
//! startup (PATH scan + usage ordering); this is the frontend-agnostic
//! version, so the TUI, the CLI, and a headless Linux host all arrive at
//! the same defaults from the same signals.
//!
//! Nothing here overwrites user choices: seeding only fills empty lists,
//! and suggestions are returned for a caller to accept, never applied.

use std::collections::HashMap;
use std::path::Path;

use crate::integrations::{builtin_presets, command_head};
use crate::setup::{find_command_path, search_dirs};

/// A preset we would seed: the builtin definition for an installed CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPreset {
    pub id: String,
    pub label: String,
    pub command: String,
    pub quick_launch: bool,
}

/// A project we would suggest adding, derived from where the user's
/// existing sessions actually ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedProject {
    pub name: String,
    pub path: String,
    /// How many sessions have run here — the ranking signal.
    pub session_count: usize,
    /// Whether the directory is a git repo (a strong "this is a project" hint).
    pub is_repo: bool,
}

/// Builtin presets for the CLIs actually installed on this machine, in the
/// order the user has used them most (by existing sessions), then by the
/// builtin order for tools they've never run.
pub fn installed_presets() -> Vec<SeedPreset> {
    let dirs = search_dirs();
    let usage = command_usage();
    let mut presets: Vec<(usize, usize, SeedPreset)> = builtin_presets()
        .iter()
        .enumerate()
        .filter_map(|(builtin_rank, definition)| {
            let head = command_head(definition.command);
            if head.is_empty() || find_command_path(head, &dirs).is_none() {
                return None;
            }
            // Most-used first; ties keep the builtin order.
            let uses = usage.get(head).copied().unwrap_or(0);
            Some((
                usize::MAX - uses,
                builtin_rank,
                SeedPreset {
                    id: definition.id.to_string(),
                    label: definition.label.to_string(),
                    command: definition.command.to_string(),
                    quick_launch: definition.quick_launch,
                },
            ))
        })
        .collect();
    presets.sort_by_key(|(usage_rank, builtin_rank, _)| (*usage_rank, *builtin_rank));
    presets.into_iter().map(|(_, _, preset)| preset).collect()
}

/// How many existing sessions ran each CLI — the same "what do you actually
/// use" signal the desktop orders presets by.
fn command_usage() -> HashMap<String, usize> {
    let mut usage = HashMap::new();
    for manifest in session_manifests() {
        let Some(command) = manifest
            .get("session")
            .and_then(|s| s.get("command"))
            .and_then(|c| c.as_str())
        else {
            continue;
        };
        let head = command_head(command);
        if !head.is_empty() {
            *usage.entry(head.to_string()).or_insert(0) += 1;
        }
    }
    usage
}

fn session_manifests() -> Vec<serde_json::Value> {
    let root = crate::app_paths::app_sessions_root();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let raw = std::fs::read(entry.path().join("manifest.json")).ok()?;
            serde_json::from_slice(&raw).ok()
        })
        .collect()
}

/// Projects worth offering on first run, ranked by how many sessions ran
/// there. Only existing directories; git repos outrank plain folders, and
/// obvious non-projects (home, /tmp, /) are skipped.
pub fn suggested_projects(limit: usize) -> Vec<SuggestedProject> {
    let home = dirs::home_dir();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for manifest in session_manifests() {
        let Some(cwd) = manifest.get("cwd").and_then(|c| c.as_str()) else {
            continue;
        };
        let cwd = cwd.trim_end_matches('/');
        if cwd.is_empty() {
            continue;
        }
        let path = Path::new(cwd);
        if !path.is_dir() {
            continue;
        }
        // A session's cwd is only a project hint when it's somewhere
        // specific — not the home dir, a temp dir, or the filesystem root.
        if home.as_deref() == Some(path)
            || cwd == "/"
            || cwd.starts_with("/tmp")
            || cwd.starts_with("/private/tmp")
            || cwd.starts_with("/var/folders")
        {
            continue;
        }
        *counts.entry(cwd.to_string()).or_insert(0) += 1;
    }
    let mut suggestions: Vec<SuggestedProject> = counts
        .into_iter()
        .map(|(path, session_count)| {
            let is_repo = Path::new(&path).join(".git").exists();
            let name = Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            SuggestedProject {
                name,
                path,
                session_count,
                is_repo,
            }
        })
        .collect();
    // Repos first, then most-used, then alphabetical for a stable list.
    suggestions.sort_by(|a, b| {
        b.is_repo
            .cmp(&a.is_repo)
            .then(b.session_count.cmp(&a.session_count))
            .then(a.name.cmp(&b.name))
    });
    suggestions.truncate(limit);
    suggestions
}

/// True when this install has never been configured — no presets and no
/// projects in the shared state. Callers use it to decide whether to seed.
pub fn needs_seeding(state: &serde_json::Value) -> bool {
    let empty = |key: &str| {
        state
            .get(key)
            .and_then(|v| v.as_array())
            .map(|list| list.is_empty())
            .unwrap_or(true)
    };
    empty("presets") && empty("projects")
}

/// Seed the shared `app-state.json` for a fresh install: builtin presets for
/// installed CLIs, plus any projects the caller accepted. Returns what was
/// written. Existing presets/projects are left alone — this only ever fills
/// an empty list, so running it twice is harmless.
pub fn seed_app_state(
    accept_projects: &[SuggestedProject],
) -> Result<(Vec<SeedPreset>, Vec<SuggestedProject>), String> {
    // Guarded load: a missing file is a fresh install, but a file we can't
    // parse is an error — seeding over it would delete the user's projects
    // and presets on the very first run after an update.
    let mut state = crate::app_state::load_for_edit()?;
    let object = state
        .as_object_mut()
        .ok_or("app-state.json is not an object")?;

    let mut seeded_presets = Vec::new();
    let presets_empty = object
        .get("presets")
        .and_then(|v| v.as_array())
        .map(|l| l.is_empty())
        .unwrap_or(true);
    if presets_empty {
        seeded_presets = installed_presets();
        let list: Vec<serde_json::Value> = seeded_presets
            .iter()
            .map(|preset| {
                serde_json::json!({
                    "id": preset.id,
                    "label": preset.label,
                    "command": preset.command,
                    "project_id": serde_json::Value::Null,
                    "enabled": true,
                    "quick_launch": preset.quick_launch,
                })
            })
            .collect();
        object.insert("presets".into(), serde_json::json!(list));
    }

    let mut seeded_projects = Vec::new();
    if !accept_projects.is_empty() {
        let projects = object
            .entry("projects".to_string())
            .or_insert_with(|| serde_json::json!([]));
        if let Some(list) = projects.as_array_mut() {
            for project in accept_projects {
                let exists = list
                    .iter()
                    .any(|p| p.get("path").and_then(|v| v.as_str()) == Some(project.path.as_str()));
                if exists {
                    continue;
                }
                list.push(serde_json::json!({
                    "id": format!("seed-{}", uuid::Uuid::new_v4()),
                    "name": project.name,
                    "path": project.path,
                }));
                seeded_projects.push(project.clone());
            }
        }
    }

    crate::app_state::save(&state)?;
    Ok((seeded_presets, seeded_projects))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_seeding_only_when_both_lists_are_empty() {
        assert!(needs_seeding(&serde_json::json!({})));
        assert!(needs_seeding(
            &serde_json::json!({ "presets": [], "projects": [] })
        ));
        assert!(!needs_seeding(
            &serde_json::json!({ "presets": [{"id": "x"}], "projects": [] })
        ));
        assert!(!needs_seeding(
            &serde_json::json!({ "presets": [], "projects": [{"id": "p"}] })
        ));
    }

    #[test]
    fn installed_presets_only_offers_real_binaries() {
        // Whatever this machine has, every suggestion must resolve on PATH
        // and be a known builtin.
        let dirs = search_dirs();
        for preset in installed_presets() {
            let head = command_head(&preset.command);
            assert!(
                find_command_path(head, &dirs).is_some(),
                "suggested {head} which isn't installed"
            );
            assert!(
                builtin_presets().iter().any(|b| b.id == preset.id),
                "suggested a non-builtin preset"
            );
        }
    }
}
