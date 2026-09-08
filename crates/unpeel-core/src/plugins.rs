//! Workspace-scoped activation and automatic launch commands. Installation
//! stays on the Host; activation never deletes a binary or a saved preset.

use serde_json::{json, Map, Value};

use crate::controller_api::HostCreatePreset;

pub const AGENT_DEFAULT_PREFIX: &str = "__agent_default__:";
pub const APP_PRESET_PREFIX: &str = "__app__:";
pub const CUSTOM_PLUGIN_PREFIX: &str = "preset:";

pub fn active(state: &Value, id: &str) -> bool {
    state
        .get("plugin_activation")
        .and_then(|map| map.get(id))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// All supported agents, resolved against this Host's executable search path.
pub fn agents_wire() -> Value {
    agents_wire_in_dirs(&crate::setup::search_dirs())
}

pub(crate) fn agents_wire_in_dirs(dirs: &[std::path::PathBuf]) -> Value {
    Value::Array(crate::runtime_catalog::builtin_runtime_catalog()
        .current_platform_descriptors()
        .filter(|runtime| runtime.display.kind == crate::runtime_catalog::RuntimeKind::Agent)
        .map(|runtime| {
            let command = runtime.detection.command_aliases.first().cloned().unwrap_or_default();
            let installed = runtime.detection.command_aliases.iter()
                .any(|alias| crate::setup::find_command_path(alias, dirs).is_some());
            let install_command = runtime.install.as_ref().and_then(|install| {
                if installed {
                    install.update_command.as_ref().or(install.command.as_ref())
                } else {
                    install.command.as_ref()
                }
            });
            json!({
                "id": runtime.id, "name": runtime.label, "command": command,
                "installed": installed,
                "installCommand": install_command,
                "websiteURL": runtime.install.as_ref().map(|install| &install.official_url),
            })
        }).collect())
}

/// A one-plugin patch, validated before any shared-state edit takes place.
pub fn validate_patch(body: &Value) -> Result<Option<(String, bool)>, String> {
    let Some(patch) = body
        .get("pluginActivation")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let id = patch
        .get("id")
        .and_then(Value::as_str)
        .ok_or("pluginActivation.id must be a string")?;
    let active = patch
        .get("active")
        .and_then(Value::as_bool)
        .ok_or("pluginActivation.active must be a boolean")?;
    let catalog = crate::runtime_catalog::builtin_runtime_catalog();
    if catalog.by_id(id).is_none()
        && crate::apps_mcp::catalog_app(id).is_none()
        && id
            .strip_prefix(CUSTOM_PLUGIN_PREFIX)
            .is_none_or(|id| id.is_empty())
    {
        return Err("unknown plugin".into());
    }
    Ok(Some((id.to_owned(), active)))
}

/// A reorder contains the visible subset. Hidden/inactive entries keep their
/// slots, including newer plugin identities an older Controller cannot see.
pub fn validate_order(body: &Value) -> Result<Option<Vec<String>>, String> {
    let Some(value) = body.get("pluginOrder").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let values = value.as_array().ok_or("pluginOrder must be an array")?;
    if values.len() > 512 {
        return Err("too many plugins".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::new();
    for value in values {
        let id = value
            .as_str()
            .ok_or("pluginOrder entries must be strings")?;
        validate_patch(&json!({"pluginActivation":{"id":id,"active":true}}))?;
        if !seen.insert(id) {
            return Err("duplicate plugin in pluginOrder".into());
        }
        order.push(id.to_owned());
    }
    Ok(Some(order))
}

pub fn apply_order(object: &mut Map<String, Value>, order: &[String]) {
    let mut stored: Vec<String> = object
        .get("plugin_order")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let mut seen = std::collections::HashSet::new();
    stored.retain(|id| seen.insert(id.clone()));
    for id in order {
        if !stored.contains(id) {
            stored.push(id.clone());
        }
    }
    let mut replacements = order.iter();
    for id in &mut stored {
        if order.contains(id) {
            if let Some(next) = replacements.next() {
                *id = next.clone();
            }
        }
    }
    object.insert("plugin_order".into(), json!(stored));
}

pub fn apply_activation(
    object: &mut Map<String, Value>,
    id: &str,
    active: bool,
) -> Result<(), String> {
    let settings = object
        .entry("plugin_activation")
        .or_insert_with(|| json!({}));
    let settings = settings
        .as_object_mut()
        .ok_or("plugin_activation must be an object")?;
    settings.insert(id.to_owned(), active.into());
    Ok(())
}

fn command_plugin<'a>(command: &str, apps: &'a [crate::apps_mcp::CatalogApp]) -> Option<&'a str> {
    let head = command.split_whitespace().next()?.trim_matches(['\'', '"']);
    let executable = std::path::Path::new(head).file_name()?.to_str()?;
    if let Some(app) = apps.iter().find(|app| app.binary == executable) {
        return Some(&app.id);
    }
    crate::runtime_catalog::builtin_runtime_catalog()
        .by_command_alias_for_current_platform(executable)
        .map(|runtime| runtime.id.as_str())
}

/// Augment saved rows without writing defaults during bootstrap. Both Host
/// implementations use this projection; inactive rows remain editable, while
/// their enabled=false flag excludes them from launchers and preset starts.
pub fn project_presets(
    state: &Value,
    agents: &Value,
    apps: &Value,
    wire: &mut Vec<Value>,
    create: &mut Vec<HostCreatePreset>,
) {
    let app_catalog = crate::apps_mcp::catalog_apps();
    for agent in agents.as_array().into_iter().flatten() {
        let id = agent["id"].as_str().unwrap_or_default();
        if agent["installed"] != true
            || wire.iter().any(|row| {
                row["projectID"].as_str().is_none()
                    && command_plugin(row["command"].as_str().unwrap_or_default(), &app_catalog)
                        == Some(id)
            })
        {
            continue;
        }
        append_default(
            wire,
            create,
            format!("{AGENT_DEFAULT_PREFIX}{id}"),
            agent["name"].clone(),
            agent["command"].clone(),
        );
    }
    for app in apps.as_array().into_iter().flatten() {
        if app["installed"] != true
            || wire.iter().any(|row| {
                row["projectID"].as_str().is_none()
                    && command_plugin(row["command"].as_str().unwrap_or_default(), &app_catalog)
                        == app["id"].as_str()
            })
        {
            continue;
        }
        append_default(
            wire,
            create,
            format!(
                "{APP_PRESET_PREFIX}{}",
                app["id"].as_str().unwrap_or_default()
            ),
            app["name"].clone(),
            app["command"].clone(),
        );
    }
    let mut defaults = std::collections::HashSet::new();
    for row in wire.iter_mut() {
        let id = command_plugin(row["command"].as_str().unwrap_or_default(), &app_catalog)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "{CUSTOM_PLUGIN_PREFIX}{}",
                    row["id"].as_str().unwrap_or_default()
                )
            });
        row["enabled"] = (row["enabled"] != false && active(state, &id)).into();
        row["isDefault"] =
            (row["projectID"].as_str().is_none() && defaults.insert(id.clone())).into();
        row["pluginID"] = id.into();
    }
    let order: Vec<&str> = state
        .get("plugin_order")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !order.is_empty() {
        wire.sort_by_key(|row| {
            order
                .iter()
                .position(|id| Some(*id) == row["pluginID"].as_str())
                .unwrap_or(usize::MAX)
        });
    }
    for preset in create {
        let id = command_plugin(&preset.command, &app_catalog)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{CUSTOM_PLUGIN_PREFIX}{}", preset.id));
        preset.enabled &= active(state, &id);
    }
}

fn append_default(
    wire: &mut Vec<Value>,
    create: &mut Vec<HostCreatePreset>,
    id: String,
    label: Value,
    command: Value,
) {
    wire.push(json!({"id": id, "label": label, "command": command,
        "enabled": true, "quickLaunch": false, "isDefault": true}));
    create.push(HostCreatePreset {
        id,
        command: command.as_str().unwrap_or_default().into(),
        enabled: true,
        project_id: None,
    });
}

/// Preserve a projected baseline before adding another command or persisting
/// a drag order. Recheck saved rows under the caller's state-file lock so a
/// concurrent customization cannot be replaced by an old snapshot default.
pub fn preserve_projected_defaults(
    object: &mut Map<String, Value>,
    rows: &[Value],
    command: Option<&str>,
) -> Result<(), String> {
    let apps = crate::apps_mcp::catalog_apps();
    let selected = command.and_then(|command| command_plugin(command, &apps));
    for row in rows {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        let Some(runtime_id) = id
            .strip_prefix(AGENT_DEFAULT_PREFIX)
            .or_else(|| id.strip_prefix(APP_PRESET_PREFIX))
        else {
            continue;
        };
        if command.is_some() && selected != Some(runtime_id) {
            continue;
        }
        let has_command = object
            .get("presets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|saved| {
                saved
                    .get("project_id")
                    .or_else(|| saved.get("projectID"))
                    .and_then(Value::as_str)
                    .is_none()
                    && command_plugin(saved["command"].as_str().unwrap_or_default(), &apps)
                        == Some(runtime_id)
            });
        if !has_command {
            materialize_default(object, id)?;
        }
    }
    Ok(())
}

/// Materialize a generated agent default only when the user edits it. Holding
/// app_state::edit's lock prevents a concurrent edit from producing duplicates.
pub fn materialize_default(object: &mut Map<String, Value>, id: &str) -> Result<(), String> {
    let (label, command) = if let Some(runtime_id) = id.strip_prefix(AGENT_DEFAULT_PREFIX) {
        let runtime = crate::runtime_catalog::builtin_runtime_catalog()
            .by_id(runtime_id)
            .ok_or("unknown agent")?;
        (
            runtime.label.clone(),
            runtime
                .detection
                .command_aliases
                .first()
                .ok_or("agent has no launch command")?
                .clone(),
        )
    } else if let Some(app_id) = id.strip_prefix(APP_PRESET_PREFIX) {
        let app = crate::apps_mcp::catalog_app(app_id).ok_or("unknown app")?;
        (app.name, app.binary)
    } else {
        return Ok(());
    };
    let presets = object.entry("presets").or_insert_with(|| json!([]));
    let presets = presets.as_array_mut().ok_or("presets is not an array")?;
    if !presets.iter().any(|row| row["id"] == id) {
        presets.push(json!({"id": id, "label": label, "command": command,
            "enabled": true, "quick_launch": false, "project_id": null}));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory() -> (Value, Value) {
        (
            json!([
                {"id":"com.anthropic.claude-code","name":"Claude","command":"claude","installed":true},
                {"id":"com.openai.codex","name":"Codex","command":"codex","installed":false}
            ]),
            json!([
                {"id":"unpeel.app.markdown","name":"Markdown","command":"unpeel-markdown","installed":true}
            ]),
        )
    }

    #[test]
    fn plugin_order_merges_visible_rows_without_losing_hidden_or_future_rows() {
        let mut object = json!({"plugin_order":["com.openai.codex","future-plugin","unpeel.app.markdown","com.anthropic.claude-code"], "future":42}).as_object().unwrap().clone();
        apply_order(
            &mut object,
            &[
                "com.anthropic.claude-code".into(),
                "com.openai.codex".into(),
            ],
        );
        assert_eq!(
            object["plugin_order"],
            json!([
                "com.anthropic.claude-code",
                "future-plugin",
                "unpeel.app.markdown",
                "com.openai.codex"
            ])
        );
        assert_eq!(object["future"], 42);
        assert!(
            validate_order(&json!({"pluginOrder":["com.openai.codex","com.openai.codex"]}))
                .is_err()
        );
        assert!(validate_order(&json!({"pluginOrder":"com.openai.codex"})).is_err());
        assert!(validate_order(&json!({"pluginOrder":["unknown-plugin"]})).is_err());
    }

    #[test]
    fn app_variants_keep_the_default_and_plugin_order_groups_commands() {
        let (agents, apps) = inventory();
        let mut state = json!({"presets":[], "plugin_order":["unpeel.app.markdown","com.anthropic.claude-code"]});
        let mut wire = vec![];
        let mut create = vec![];
        project_presets(&state, &agents, &apps, &mut wire, &mut create);
        assert_eq!(wire[0]["pluginID"], "unpeel.app.markdown");
        preserve_projected_defaults(
            state.as_object_mut().unwrap(),
            &wire,
            Some("unpeel-markdown notes.md"),
        )
        .unwrap();
        assert_eq!(state["presets"][0]["command"], "unpeel-markdown");
        materialize_default(
            state.as_object_mut().unwrap(),
            "__app__:unpeel.app.markdown",
        )
        .unwrap();
        assert_eq!(state["presets"].as_array().unwrap().len(), 1);
        let mut wire =
            vec![json!({"id":"custom-app", "command":"unpeel-markdown notes.md", "enabled":true})];
        let mut create = vec![];
        project_presets(&state, &agents, &apps, &mut wire, &mut create);
        assert_eq!(
            wire.iter()
                .filter(|row| row["pluginID"] == "unpeel.app.markdown")
                .count(),
            1
        );
    }

    #[test]
    fn custom_commands_have_stable_activation_ids() {
        let mut wire = vec![json!({"id":"custom", "command":"my-tool --flag", "enabled":true})];
        let mut create = vec![HostCreatePreset {
            id: "custom".into(),
            command: "my-tool --flag".into(),
            enabled: true,
            project_id: None,
        }];
        project_presets(
            &json!({"plugin_activation":{"preset:custom":false}}),
            &json!([]),
            &json!([]),
            &mut wire,
            &mut create,
        );
        assert_eq!(wire[0]["pluginID"], "preset:custom");
        assert_eq!(wire[0]["enabled"], false);
        assert!(!create[0].enabled);
    }

    #[test]
    fn installation_projects_defaults_without_creating_saved_presets() {
        let state = json!({"presets":[], "future":42});
        let (agents, apps) = inventory();
        let (mut wire, mut create) = (vec![], vec![]);
        project_presets(&state, &agents, &apps, &mut wire, &mut create);
        assert_eq!(wire.len(), 2);
        assert_eq!(wire[0]["command"], "claude");
        assert_eq!(wire[1]["command"], "unpeel-markdown");
        assert!(create.iter().all(|preset| preset.enabled));
        assert_eq!(state["presets"], json!([]));
    }

    #[test]
    fn adding_a_variant_keeps_the_projected_default_and_concurrent_customization() {
        let rows = vec![
            json!({"id":format!("{AGENT_DEFAULT_PREFIX}com.anthropic.claude-code"), "command":"claude"}),
        ];
        let mut object = Map::new();
        preserve_projected_defaults(&mut object, &rows, Some("claude --continue")).unwrap();
        assert_eq!(object["presets"][0]["command"], "claude");
        object["presets"][0]["command"] = "claude --plan".into();
        preserve_projected_defaults(&mut object, &rows, None).unwrap();
        assert_eq!(object["presets"].as_array().unwrap().len(), 1);
        assert_eq!(object["presets"][0]["command"], "claude --plan");
    }

    #[test]
    fn project_commands_do_not_replace_an_agents_global_default() {
        let (agents, apps) = inventory();
        let mut wire = vec![json!({"id":"legacy", "command":"claude --project",
            "enabled":true, "projectID":"project"})];
        let mut create = vec![HostCreatePreset {
            id: "legacy".into(),
            command: "claude --project".into(),
            enabled: true,
            project_id: Some("project".into()),
        }];
        project_presets(&json!({}), &agents, &apps, &mut wire, &mut create);
        assert_eq!(wire[0]["isDefault"], false);
        assert!(wire
            .iter()
            .any(|row| row["command"] == "claude" && row["isDefault"] == true));
        assert_eq!(create[0].project_id.as_deref(), Some("project"));
    }

    #[test]
    fn deactivation_preserves_commands_and_blocks_every_preset_for_the_plugin() {
        let (agents, apps) = inventory();
        let mut wire = vec![
            json!({"id":"one","command":"claude --plan","enabled":true,"quickLaunch":true}),
            json!({"id":"two","command":"claude --continue","enabled":true}),
        ];
        let mut create = wire
            .iter()
            .map(|row| HostCreatePreset {
                id: row["id"].as_str().unwrap().into(),
                command: row["command"].as_str().unwrap().into(),
                enabled: true,
                project_id: None,
            })
            .collect();
        let state = json!({"plugin_activation":{"com.anthropic.claude-code":false,"unpeel.app.markdown":false}});
        project_presets(&state, &agents, &apps, &mut wire, &mut create);
        assert_eq!(
            wire.len(),
            3,
            "saved agent commands suppress the generated default"
        );
        assert_eq!(wire[0]["isDefault"], true);
        assert_eq!(wire[1]["isDefault"], false);
        assert_eq!(
            wire[0]["quickLaunch"], true,
            "activation must not erase favorites"
        );
        assert!(wire.iter().all(|row| row["enabled"] == false));
        assert!(create.iter().all(|preset| !preset.enabled));
    }

    #[test]
    fn activation_is_a_single_plugin_merge_and_reactivation_restores_launches() {
        let mut state =
            json!({"presets":[{"future":true}],"plugin_activation":{"other":false},"future":42});
        let object = state.as_object_mut().unwrap();
        apply_activation(object, "com.anthropic.claude-code", false).unwrap();
        apply_activation(object, "com.anthropic.claude-code", true).unwrap();
        assert!(active(&state, "com.anthropic.claude-code"));
        assert!(!active(&state, "other"));
        assert_eq!(state["future"], 42);
        assert_eq!(state["presets"], json!([{"future":true}]));
        let (agents, apps) = inventory();
        let (mut wire, mut create) = (vec![], vec![]);
        project_presets(&state, &agents, &apps, &mut wire, &mut create);
        assert!(create.iter().all(|preset| preset.enabled));
    }

    #[test]
    fn existing_app_command_is_not_duplicated_and_defaults_materialize_only_once() {
        let (agents, apps) = inventory();
        let mut wire = vec![json!({"id":"saved","command":"unpeel-markdown","enabled":true})];
        let mut create = vec![];
        project_presets(&json!({}), &agents, &apps, &mut wire, &mut create);
        assert_eq!(
            wire.iter()
                .filter(|row| row["command"] == "unpeel-markdown")
                .count(),
            1
        );
        let id = format!("{AGENT_DEFAULT_PREFIX}com.anthropic.claude-code");
        let mut state = json!({"presets":[],"future":42});
        materialize_default(state.as_object_mut().unwrap(), &id).unwrap();
        state["presets"][0]["command"] = "claude --plan".into();
        materialize_default(state.as_object_mut().unwrap(), &id).unwrap();
        assert_eq!(state["presets"].as_array().unwrap().len(), 1);
        assert_eq!(state["presets"][0]["command"], "claude --plan");
        assert_eq!(state["future"], 42);
    }

    #[test]
    fn malformed_activation_is_rejected_before_any_write() {
        for patch in [
            json!(false),
            json!({"id":"nope","active":true}),
            json!({"id":"com.anthropic.claude-code","active":"no"}),
        ] {
            assert!(validate_patch(&json!({"pluginActivation":patch})).is_err());
        }
        assert_eq!(
            validate_patch(
                &json!({"pluginActivation":{"id":"unpeel.app.markdown","active":false}})
            )
            .unwrap(),
            Some(("unpeel.app.markdown".into(), false))
        );
    }
}
