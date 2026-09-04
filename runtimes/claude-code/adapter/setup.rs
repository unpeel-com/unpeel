use crate::app_paths::unpeel_home;
use crate::hook_assets::{read_mergeable_json_object, write_executable_script, write_file_atomic};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const CLAUDE_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/claude-code/assets/hooks/lifecycle.sh"
));

pub(crate) const HOOK_EVENTS: &[&str] = &[
    // SessionStart latches provider metadata only (the script forwards it as
    // HookSeen): it fires at launch and on in-tool /resume, /clear, /compact
    // with the new session_id, so precise resume tracks the conversation the
    // user actually switched to.
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "PermissionRequest",
];
pub fn install_claude_hooks() -> Result<(), String> {
    let script_path = claude_hook_script_path();
    write_executable_script(&script_path, CLAUDE_HOOK_SCRIPT, "Claude hook script")?;
    ensure_claude_settings_hook(&script_path)?;
    write_claude_unpeel_mcp_config()?;
    // Legacy per-domain configs: still referenced by launch commands of
    // sessions started before the unified server; rewritten so their exe
    // paths stay current too. New launches only use the unified config.
    write_claude_mcp_config()?;
    write_claude_browser_mcp_config()?;
    Ok(())
}

/// Unified MCP server config passed to Claude via a single additive
/// `--mcp-config`. One server (`unpeel`) carries every enabled domain; the
/// server reads the calling session's manifest to decide which domains to
/// advertise. Rewritten on every launch so the executable path stays current.
pub fn claude_unpeel_mcp_config_path() -> PathBuf {
    unpeel_home().join("mcp").join("claude-unpeel-mcp.json")
}

pub(crate) fn write_claude_unpeel_mcp_config() -> Result<(), String> {
    let exe = crate::session_host::resolve_current_executable()?;
    // The file name keeps the pre-rename `unpeel-mcp` spelling: launch
    // commands recorded by existing sessions reference this exact path, so
    // only the server key inside changes to `unpeel`.
    let config = json!({
        "mcpServers": {
            "unpeel": {
                "type": "stdio",
                "command": exe.to_string_lossy(),
                "args": [crate::mcp_host::MCP_HOST_ARG],
            }
        }
    });
    let path = claude_unpeel_mcp_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create MCP config dir {}: {e}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize Claude unified MCP config: {e}"))?;
    fs::write(&path, format!("{serialized}\n")).map_err(|e| {
        format!(
            "Failed to write Claude unified MCP config {}: {e}",
            path.display()
        )
    })
}

/// MCP server config passed to Claude via `--mcp-config`. Rewritten on every
/// launch so the executable path stays current across app updates and dev
/// builds. The spawned server inherits the session env, so no per-session
/// values are baked into the file.
pub fn claude_mcp_config_path() -> PathBuf {
    unpeel_home().join("mcp").join("claude-mcp.json")
}

pub(crate) fn write_claude_mcp_config() -> Result<(), String> {
    let exe = crate::session_host::resolve_current_executable()?;
    let config = json!({
        "mcpServers": {
            "unpeel-sessions": {
                "type": "stdio",
                "command": exe.to_string_lossy(),
                "args": [crate::mcp_host::MCP_HOST_ARG],
            }
        }
    });
    let path = claude_mcp_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create MCP config dir {}: {e}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize Claude MCP config: {e}"))?;
    fs::write(&path, format!("{serialized}\n"))
        .map_err(|e| format!("Failed to write Claude MCP config {}: {e}", path.display()))
}

/// Browser MCP server config passed to Claude as a second (additive)
/// `--mcp-config`, only for sessions whose Browser Access grant wants it.
/// Rewritten on every launch like the Sessions config so the executable path
/// stays current.
pub fn claude_browser_mcp_config_path() -> PathBuf {
    unpeel_home().join("mcp").join("claude-browser-mcp.json")
}

pub(crate) fn write_claude_browser_mcp_config() -> Result<(), String> {
    let exe = crate::session_host::resolve_current_executable()?;
    let config = json!({
        "mcpServers": {
            "unpeel-browser": {
                "type": "stdio",
                "command": exe.to_string_lossy(),
                "args": [crate::browser_mcp::BROWSER_MCP_ARG],
            }
        }
    });
    let path = claude_browser_mcp_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create MCP config dir {}: {e}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize Claude Browser MCP config: {e}"))?;
    fs::write(&path, format!("{serialized}\n")).map_err(|e| {
        format!(
            "Failed to write Claude Browser MCP config {}: {e}",
            path.display()
        )
    })
}
pub(crate) fn claude_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("claude-hooks.sh")
}
pub(crate) fn claude_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("settings.json"))
}
pub(crate) fn build_hook_entry(event: &str, command: &str) -> Value {
    let mut entry = json!({
        "hooks": [{
            "type": "command",
            "command": command,
            "async": true,
            "timeout": 5
        }]
    });
    if event == "PermissionRequest" {
        entry["matcher"] = Value::String("*".into());
    }
    entry
}

/// Unpeel-managed `claude-hooks.sh` copies left behind by tests or deleted
/// workspaces. Grok also runs Claude settings hooks, so a stale `/tmp/...`
/// copy that still posts `session_start` as busy will spin every Grok
/// session even after the live script is fixed.
pub(crate) fn is_stale_unpeel_claude_hook(command: &str, current: &str) -> bool {
    let path = command.split_whitespace().next().unwrap_or(command);
    if path == current {
        return false;
    }
    if !path.ends_with("claude-hooks.sh") {
        return false;
    }
    path.starts_with("/tmp/") || path.starts_with("/var/folders/") || !Path::new(path).is_file()
}

pub(crate) fn prune_stale_unpeel_claude_hooks(array: &mut Vec<Value>, current: &str) -> bool {
    let mut changed = false;
    array.retain_mut(|entry| {
        let Some(hooks) = entry
            .get_mut("hooks")
            .and_then(|value| value.as_array_mut())
        else {
            return true;
        };
        let before = hooks.len();
        hooks.retain(|hook| {
            hook.get("command")
                .and_then(|value| value.as_str())
                .is_none_or(|command| !is_stale_unpeel_claude_hook(command, current))
        });
        if hooks.len() != before {
            changed = true;
        }
        !hooks.is_empty()
    });
    changed
}

pub(crate) fn ensure_claude_settings_hook(script_path: &Path) -> Result<(), String> {
    let Some(settings_path) = claude_settings_path() else {
        return Ok(());
    };
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create Claude settings dir {}: {e}",
                parent.display()
            )
        })?;
    }

    let Some(mut settings) = read_mergeable_json_object(&settings_path, "Claude settings")? else {
        // Existing settings.json is not a valid JSON object; skip rather than
        // clobber the user's real settings with an Unpeel-only file.
        return Ok(());
    };

    let command = script_path.to_string_lossy().to_string();
    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks_obj = hooks.as_object_mut().unwrap();

    let mut changed = false;
    for event in HOOK_EVENTS {
        let entries = hooks_obj
            .entry((*event).to_string())
            .or_insert_with(|| json!([]));
        if !entries.is_array() {
            *entries = json!([]);
        }
        let array = entries.as_array_mut().unwrap();
        if prune_stale_unpeel_claude_hooks(array, &command) {
            changed = true;
        }
        let already_installed = array.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(|value| value.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command").and_then(|value| value.as_str())
                            == Some(command.as_str())
                    })
                })
        });
        if !already_installed {
            array.push(build_hook_entry(event, &command));
            changed = true;
        }
    }

    if changed {
        let json = serde_json::to_string_pretty(&settings)
            .map_err(|e| format!("Failed to serialize Claude settings: {e}"))?;
        write_file_atomic(&settings_path, &format!("{json}\n"), "Claude settings")?;
    }

    Ok(())
}
