use crate::app_paths::unpeel_home;
use crate::hook_assets::{
    notify_hook_script_path, read_mergeable_json_object, write_executable_script,
    write_file_atomic, NOTIFY_HOOK_SCRIPT,
};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const CODEX_WRAPPER_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/codex/assets/hooks/command-wrapper.sh"
));
pub(crate) const CODEX_NOTIFY_NORMALIZER_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/codex/assets/hooks/notify-normalizer.sh"
));

pub fn install_codex_wrapper() -> Result<(), String> {
    let transport_path = notify_hook_script_path();
    write_executable_script(
        &transport_path,
        NOTIFY_HOOK_SCRIPT,
        "shared notify transport",
    )?;
    let notify_path = codex_notify_hook_script_path();
    let normalizer = CODEX_NOTIFY_NORMALIZER_SCRIPT.replace(
        "{{NOTIFY_PATH}}",
        transport_path.to_string_lossy().as_ref(),
    );
    write_executable_script(&notify_path, &normalizer, "Codex notify normalizer")?;

    let wrapper_path = codex_wrapper_path();
    let wrapper =
        CODEX_WRAPPER_SCRIPT.replace("{{NOTIFY_PATH}}", notify_path.to_string_lossy().as_ref());
    write_executable_script(&wrapper_path, &wrapper, "Codex wrapper script")?;

    ensure_codex_hooks_json(&notify_path)?;
    ensure_codex_hooks_feature_enabled()?;
    Ok(())
}
pub(crate) fn codex_notify_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("codex-notify-hook.sh")
}

pub(crate) fn codex_wrapper_path() -> PathBuf {
    wrapper_bin_dir().join("codex")
}

pub fn wrapper_bin_dir() -> PathBuf {
    unpeel_home().join("hooks").join("bin")
}
pub(crate) fn codex_hooks_json_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join("hooks.json"))
}

pub(crate) fn codex_config_toml_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codex").join("config.toml"))
}
pub(crate) const CODEX_HOOKS_FEATURE_FLAG: &str = "hooks";
pub(crate) const CODEX_DEPRECATED_HOOKS_FEATURE_FLAG: &str = "codex_hooks";
pub(crate) const CODEX_MULTI_AGENT_FEATURE_FLAG: &str = "multi_agent";
pub(crate) const CODEX_DEPRECATED_MULTI_AGENT_FEATURE_FLAG: &str = "collab";

pub(crate) fn codex_feature_value<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    root.get("features").and_then(|features| features.get(key))
}

pub(crate) fn codex_feature_bool(raw: &str, key: &str) -> Result<Option<bool>, String> {
    if raw.trim().is_empty() {
        return Ok(None);
    }

    let root = raw
        .parse::<toml::Value>()
        .map_err(|e| format!("Failed to parse Codex config.toml: {e}"))?;
    Ok(codex_feature_value(&root, key).and_then(|value| value.as_bool()))
}

pub(crate) fn codex_feature_is_present(raw: &str, key: &str) -> Result<bool, String> {
    if raw.trim().is_empty() {
        return Ok(false);
    }

    let root = raw
        .parse::<toml::Value>()
        .map_err(|e| format!("Failed to parse Codex config.toml: {e}"))?;
    Ok(codex_feature_value(&root, key).is_some())
}

pub(crate) fn codex_hooks_feature_is_enabled(raw: &str) -> Result<bool, String> {
    Ok(codex_feature_bool(raw, CODEX_HOOKS_FEATURE_FLAG)?.unwrap_or(false))
}

pub(crate) fn enable_codex_hooks_feature_in_toml(raw: &str) -> Result<String, String> {
    let hooks_enabled = codex_hooks_feature_is_enabled(raw)?;
    let legacy_hooks_present = codex_feature_is_present(raw, CODEX_DEPRECATED_HOOKS_FEATURE_FLAG)?;
    let legacy_multi_agent_present =
        codex_feature_is_present(raw, CODEX_DEPRECATED_MULTI_AGENT_FEATURE_FLAG)?;
    let legacy_multi_agent_enabled =
        codex_feature_bool(raw, CODEX_DEPRECATED_MULTI_AGENT_FEATURE_FLAG)?.unwrap_or(false);
    let multi_agent_present = codex_feature_is_present(raw, CODEX_MULTI_AGENT_FEATURE_FLAG)?;

    if hooks_enabled && !legacy_hooks_present && !legacy_multi_agent_present {
        return Ok(raw.to_string());
    }

    let mut lines = raw.lines().map(ToString::to_string).collect::<Vec<_>>();
    let mut features_start = None;
    let mut features_end = lines.len();

    for (index, line) in lines.iter().enumerate() {
        let uncommented = line.split('#').next().unwrap_or("").trim();
        if uncommented == "[features]" {
            features_start = Some(index);
            continue;
        }
        if features_start.is_some()
            && uncommented.starts_with('[')
            && uncommented.ends_with(']')
            && uncommented != "[features]"
        {
            features_end = index;
            break;
        }
    }

    if let Some(start) = features_start {
        let mut feature_lines = Vec::new();
        let mut hooks_seen = false;
        let mut multi_agent_seen = false;

        for line in lines.iter().take(features_end).skip(start + 1) {
            let uncommented = line.split('#').next().unwrap_or("").trim();
            let key = uncommented.split('=').next().unwrap_or("").trim();
            match key {
                CODEX_HOOKS_FEATURE_FLAG => {
                    hooks_seen = true;
                    feature_lines.push("hooks = true".to_string());
                }
                CODEX_DEPRECATED_HOOKS_FEATURE_FLAG => {}
                CODEX_MULTI_AGENT_FEATURE_FLAG => {
                    multi_agent_seen = true;
                    feature_lines.push(line.to_string());
                }
                CODEX_DEPRECATED_MULTI_AGENT_FEATURE_FLAG => {}
                _ => feature_lines.push(line.to_string()),
            }
        }

        let mut updated = Vec::with_capacity(lines.len() + 2);
        updated.extend_from_slice(&lines[..=start]);
        if !hooks_seen {
            updated.push("hooks = true".to_string());
        }
        if legacy_multi_agent_enabled && !multi_agent_seen && !multi_agent_present {
            updated.push("multi_agent = true".to_string());
        }
        updated.extend(feature_lines);
        updated.extend_from_slice(&lines[features_end..]);
        return finalize_codex_config_toml(updated);
    }

    if !lines.is_empty() && lines.last().is_some_and(|line| !line.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.push("[features]".to_string());
    lines.push("hooks = true".to_string());
    finalize_codex_config_toml(lines)
}

pub(crate) fn finalize_codex_config_toml(lines: Vec<String>) -> Result<String, String> {
    let mut updated = lines.join("\n");
    updated.push('\n');
    if !codex_hooks_feature_is_enabled(&updated)? {
        return Err("Failed to enable Codex hooks feature".to_string());
    }
    if codex_feature_is_present(&updated, CODEX_DEPRECATED_HOOKS_FEATURE_FLAG)?
        || codex_feature_is_present(&updated, CODEX_DEPRECATED_MULTI_AGENT_FEATURE_FLAG)?
    {
        return Err("Failed to remove deprecated Codex feature flags".to_string());
    }
    Ok(updated)
}

pub(crate) fn ensure_codex_hooks_feature_enabled() -> Result<(), String> {
    let Some(config_path) = codex_config_toml_path() else {
        return Ok(());
    };
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create Codex config dir {}: {e}",
                parent.display()
            )
        })?;
    }

    let raw = fs::read_to_string(&config_path).unwrap_or_default();
    let updated = enable_codex_hooks_feature_in_toml(&raw)?;
    if updated != raw {
        fs::write(&config_path, updated)
            .map_err(|e| format!("Failed to update Codex config.toml: {e}"))?;
    }
    Ok(())
}

pub(crate) const CODEX_MANAGED_HOOK_SUFFIX: &str = "; fi # unpeel-managed";

pub(crate) fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn build_codex_hook_command(script_path: &Path) -> String {
    let quoted_path = shell_single_quote(&script_path.to_string_lossy());
    format!("if [ -x {quoted_path} ]; then {quoted_path}{CODEX_MANAGED_HOOK_SUFFIX}")
}

pub(crate) fn parse_managed_codex_hook_command(command: &str) -> Option<PathBuf> {
    let body = command.strip_suffix(CODEX_MANAGED_HOOK_SUFFIX)?;
    let (test, run) = body.split_once(" ]; then ")?;
    let quoted_path = test.strip_prefix("if [ -x ")?;
    if quoted_path != run {
        return None;
    }
    let inner = quoted_path.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(PathBuf::from(inner.replace("'\\''", "'")))
}

// Older builds registered the script path directly, before managed commands
// carried an ownership marker. Keep this deliberately narrow so a missing
// Clarity/Superset/user hook is never mistaken for an Unpeel hook.
pub(crate) fn looks_like_legacy_unpeel_notify_hook(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().and_then(|name| name.to_str()) == Some("notify-hook.sh")
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("hooks")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == ".unpeel" || name.starts_with("unpeel-"))
}

pub(crate) fn managed_codex_hook_path(
    command: &str,
    current_script_path: &Path,
) -> Option<PathBuf> {
    if let Some(path) = parse_managed_codex_hook_command(command) {
        return Some(path);
    }

    let path = PathBuf::from(command);
    if path == current_script_path || looks_like_legacy_unpeel_notify_hook(&path) {
        return Some(path);
    }
    None
}

pub(crate) fn build_codex_hook_entry(event_name: &str, script_path: &Path) -> Value {
    let mut entry = json!({
        "hooks": [{
            "type": "command",
            "command": build_codex_hook_command(script_path)
        }]
    });
    if event_name == "PermissionRequest" {
        entry["matcher"] = Value::String("*".into());
    }
    entry
}

pub(crate) fn reconcile_codex_hooks_json(
    hooks_json: &mut Value,
    notify_script_path: &Path,
) -> bool {
    let hooks = hooks_json
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks_obj = hooks.as_object_mut().unwrap();

    let mut changed = false;
    for event_name in [
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "Stop",
    ] {
        let entries = hooks_obj
            .entry(event_name.to_string())
            .or_insert_with(|| json!([]));
        if !entries.is_array() {
            *entries = json!([]);
        }
        let array = entries.as_array_mut().unwrap();

        // UNPEEL_HOME lets release, dev, and clean-state instances coexist, so
        // retain every live Unpeel hook and prune only scripts that disappeared.
        // Guarded commands make the gap before this startup cleanup harmless.
        array.retain_mut(|entry| {
            let Some(entry_hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let hook_count_before_cleanup = entry_hooks.len();
            entry_hooks.retain(|hook| {
                let Some(command) = hook.get("command").and_then(Value::as_str) else {
                    return true;
                };
                let Some(path) = managed_codex_hook_path(command, notify_script_path) else {
                    return true;
                };
                if path.exists() {
                    true
                } else {
                    changed = true;
                    false
                }
            });
            hook_count_before_cleanup == 0 || !entry_hooks.is_empty()
        });

        let already_installed = array.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|entry_hooks| {
                    entry_hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(Value::as_str)
                            .and_then(|command| {
                                managed_codex_hook_path(command, notify_script_path)
                            })
                            .is_some_and(|path| path == notify_script_path)
                    })
                })
        });
        if !already_installed {
            array.push(build_codex_hook_entry(event_name, notify_script_path));
            changed = true;
        }
    }

    changed
}

/// Writes Unpeel hook definitions into `~/.codex/hooks.json`.
/// Native Codex hooks provide the authoritative start/stop/approval lifecycle.
/// The wrapper watcher remains as a fallback and for richer TUI/session metadata.
pub(crate) fn ensure_codex_hooks_json(notify_script_path: &Path) -> Result<(), String> {
    let Some(hooks_path) = codex_hooks_json_path() else {
        return Ok(());
    };
    if let Some(parent) = hooks_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create Codex hooks dir {}: {e}", parent.display()))?;
    }

    let Some(mut hooks_json) = read_mergeable_json_object(&hooks_path, "Codex hooks.json")? else {
        return Ok(());
    };

    let changed = reconcile_codex_hooks_json(&mut hooks_json, notify_script_path);

    if changed {
        let json = serde_json::to_string_pretty(&hooks_json)
            .map_err(|e| format!("Failed to serialize Codex hooks.json: {e}"))?;
        write_file_atomic(&hooks_path, &format!("{json}\n"), "Codex hooks.json")?;
    }

    Ok(())
}
