//! Shared `app-state.json` helpers behind the one-shot verbs: project
//! registration, the flat preset list, and the resume-availability message.
//! These used to live inside the interactive terminal UI; they are pure
//! disk-contract code and every write goes through `app_state::edit`, which
//! announces on the state bus so the app and every Host see it at once.

use unpeel_serve::overlay;
use unpeel_serve::sessions::SessionRow;

/// Result of registering a folder as a project.
pub enum AddProject {
    Added,
    Existing { name: String },
}

pub fn add_project_to_app_state(name: &str, path: &str) -> Result<AddProject, String> {
    // The app's own projects live in ITS UserDefaults, not in app-state.json,
    // so checking the file alone cheerfully adds a second "unpeel" pointing
    // at the same folder — which then shows up in the desktop as an empty
    // duplicate of a project full of sessions.
    if let Some(overlay) = overlay::load() {
        if let Some((id, existing)) = overlay
            .project_paths
            .iter()
            .find(|(_, existing)| same_folder(existing, path))
        {
            let label = overlay
                .projects
                .iter()
                .find(|(pid, _)| pid == id)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| existing.clone());
            return Ok(AddProject::Existing { name: label });
        }
    }
    unpeel_core::app_state::edit(|state| {
        let projects = state
            .entry("projects".to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or("app-state.json has no projects array")?;
        if let Some(existing) = projects.iter().find(|p| {
            p.get("path")
                .and_then(|v| v.as_str())
                .is_some_and(|existing| same_folder(existing, path))
        }) {
            return Ok(AddProject::Existing {
                name: existing
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(name)
                    .to_string(),
            });
        }
        projects.push(serde_json::json!({
            "id": format!("tui-{}", uuid::Uuid::new_v4()),
            "name": name,
            "path": path,
        }));
        Ok(AddProject::Added)
    })
}

fn same_folder(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        let trimmed = p.trim_end_matches('/');
        std::fs::canonicalize(trimmed)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| trimmed.to_string())
    };
    !a.is_empty() && norm(a) == norm(b)
}

fn stored_presets_mut(
    state: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<&mut Vec<serde_json::Value>, String> {
    state
        .entry("presets")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| "app-state.json presets is not an array".to_string())
}

fn preset_selector_index(list: &[serde_json::Value], selector: &str) -> Result<usize, String> {
    let id_matches: Vec<usize> = list
        .iter()
        .enumerate()
        .filter_map(|(index, preset)| {
            (preset.get("id").and_then(|value| value.as_str()) == Some(selector)).then_some(index)
        })
        .collect();
    if id_matches.len() == 1 {
        return Ok(id_matches[0]);
    }
    if id_matches.len() > 1 {
        return Err(format!("multiple presets have id {selector:?}"));
    }

    let label_matches: Vec<usize> = list
        .iter()
        .enumerate()
        .filter_map(|(index, preset)| {
            (preset.get("label").and_then(|value| value.as_str()) == Some(selector))
                .then_some(index)
        })
        .collect();
    match label_matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(format!("no preset labelled or identified by {selector:?}")),
        _ => Err(format!(
            "multiple presets are labelled {selector:?}; use an exact preset id"
        )),
    }
}

fn set_preset_cli_flag(selector: &str, field: &str, value: bool) -> Result<String, String> {
    unpeel_core::app_state::edit(|state| {
        let presets = stored_presets_mut(state)?;
        let index = preset_selector_index(presets, selector)?;
        let preset = presets[index]
            .as_object_mut()
            .ok_or_else(|| "preset row is not an object".to_string())?;
        let label = preset
            .get("label")
            .and_then(|value| value.as_str())
            .unwrap_or(selector)
            .to_owned();
        preset.insert(field.into(), value.into());
        Ok(label)
    })
}

pub(crate) fn resume_unavailable_message(session: &SessionRow) -> &'static str {
    if !session.running {
        return "this session cannot be resumed";
    }
    if !unpeel_core::resume::can_resume(&session.command) {
        return "this live terminal has no managed agent to resume";
    }
    if session.active_runtime_id.is_some() {
        return "the managed agent is still active";
    }
    "Resume Agent is unavailable for this live Host"
}

const PRESETS_EDIT_USAGE: &str = "usage: unpeel presets edit <label|id> [--label L] [--command C]";

pub fn presets_cli(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let mut shown = 0usize;
            let state = unpeel_core::app_state::load()?;
            let overlay_superseded = state
                .get("native_preset_overlay_migrated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Some(presets) = state.get("presets").and_then(|v| v.as_array()) {
                for p in presets {
                    let enabled = p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                    let starred = p
                        .get("quick_launch")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    println!(
                        "{}{} {:24} {}",
                        if enabled { " " } else { "x" },
                        if starred { "*" } else { " " },
                        p.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                        p.get("command").and_then(|v| v.as_str()).unwrap_or(""),
                    );
                    shown += 1;
                }
            }
            // Presets still held in the app's UserDefaults overlay: read-only
            // until the app runs once and folds them into app-state.json.
            if !overlay_superseded {
                if let Some(overlay) = overlay::load() {
                    for (label, command) in &overlay.presets {
                        println!("- {label:24} {command}   (in the app — open it once to migrate)");
                        shown += 1;
                    }
                }
            }
            if shown == 0 {
                println!("no presets -- add one: unpeel presets add <label> <command>");
            }
            Ok(())
        }
        Some("add") => {
            let (Some(label), Some(command)) = (args.get(1), args.get(2)) else {
                return Err("usage: unpeel presets add <label> <command>".into());
            };
            let preset = serde_json::json!({
                "id": format!("tui-{}", uuid::Uuid::new_v4()),
                "label": label,
                "command": command,
                "project_id": null,
                "enabled": true,
                "quick_launch": false,
            });
            unpeel_core::app_state::edit(|state| {
                stored_presets_mut(state)?.push(preset);
                Ok(())
            })?;
            println!("added: {label} -- {command}");
            Ok(())
        }
        Some("remove") => {
            let Some(needle) = args.get(1) else {
                return Err("usage: unpeel presets remove <label>".into());
            };
            unpeel_core::app_state::edit(|state| {
                let presets = stored_presets_mut(state)?;
                let before = presets.len();
                presets.retain(|preset| {
                    preset.get("label").and_then(|value| value.as_str()) != Some(needle.as_str())
                });
                if presets.len() == before {
                    return Err(format!(
                        "no preset labelled {needle:?} in app-state.json (a preset still held \
                         in the app's overlay migrates on the app's next launch)"
                    ));
                }
                Ok(())
            })?;
            println!("removed: {needle}");
            Ok(())
        }
        Some(command @ ("star" | "unstar" | "enable" | "disable")) => {
            let Some(selector) = args.get(1) else {
                return Err(format!("usage: unpeel presets {command} <label|id>"));
            };
            if args.len() != 2 {
                return Err(format!("usage: unpeel presets {command} <label|id>"));
            }
            let (field, value) = match command {
                "star" => ("quick_launch", true),
                "unstar" => ("quick_launch", false),
                "enable" => ("enabled", true),
                "disable" => ("enabled", false),
                _ => unreachable!(),
            };
            let label = set_preset_cli_flag(selector, field, value)?;
            println!("{command}: {label}");
            Ok(())
        }
        Some("reorder") => {
            let (Some(selector), Some(position)) = (args.get(1), args.get(2)) else {
                return Err("usage: unpeel presets reorder <label|id> <position>".into());
            };
            if args.len() != 3 {
                return Err("usage: unpeel presets reorder <label|id> <position>".into());
            }
            let position = position
                .parse::<usize>()
                .ok()
                .filter(|position| *position > 0)
                .ok_or("preset position must be a positive, 1-based integer")?;
            let label = unpeel_core::app_state::edit(|state| {
                let presets = stored_presets_mut(state)?;
                if position > presets.len() {
                    return Err(format!(
                        "preset position {position} is out of range 1..={}",
                        presets.len()
                    ));
                }
                let from = preset_selector_index(presets, selector)?;
                let label = presets[from]
                    .get("label")
                    .and_then(|value| value.as_str())
                    .unwrap_or(selector)
                    .to_owned();
                let preset = presets.remove(from);
                presets.insert(position - 1, preset);
                Ok(label)
            })?;
            println!("reordered: {label} -> {position}");
            Ok(())
        }
        Some("edit") => {
            let Some(selector) = args.get(1) else {
                return Err(PRESETS_EDIT_USAGE.into());
            };
            let mut new_label: Option<String> = None;
            let mut new_command: Option<String> = None;
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                match flag.as_str() {
                    "--label" => new_label = rest.next().cloned(),
                    "--command" => new_command = rest.next().cloned(),
                    _ => return Err(PRESETS_EDIT_USAGE.into()),
                }
            }
            if new_label.is_none() && new_command.is_none() {
                return Err(PRESETS_EDIT_USAGE.into());
            }
            if new_label.as_deref().is_some_and(str::is_empty)
                || new_command.as_deref().is_some_and(str::is_empty)
            {
                return Err("preset label and command must not be empty".into());
            }
            // Edit in place: the id, star, enabled flag, and position all
            // survive, which remove+add would lose.
            let label = unpeel_core::app_state::edit(|state| {
                let presets = stored_presets_mut(state)?;
                let index = preset_selector_index(presets, selector)?;
                let preset = presets[index]
                    .as_object_mut()
                    .ok_or_else(|| "preset row is not an object".to_string())?;
                if let Some(label) = &new_label {
                    preset.insert("label".into(), label.clone().into());
                }
                if let Some(command) = &new_command {
                    preset.insert("command".into(), command.clone().into());
                }
                Ok(preset
                    .get("label")
                    .and_then(|value| value.as_str())
                    .unwrap_or(selector)
                    .to_owned())
            })?;
            println!("edited: {label}");
            Ok(())
        }
        Some(other) => Err(format!("unknown presets subcommand: {other}")),
    }
}
