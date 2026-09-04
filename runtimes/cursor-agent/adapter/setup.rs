use crate::app_paths::unpeel_home;
use crate::hook_assets::{read_mergeable_json_object, write_executable_script, write_file_atomic};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const CURSOR_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/cursor-agent/assets/hooks/lifecycle.sh"
));

pub fn install_cursor_hooks() -> Result<(), String> {
    let script_path = cursor_hook_script_path();
    write_executable_script(&script_path, CURSOR_HOOK_SCRIPT, "Cursor hook script")?;
    ensure_cursor_hooks(&script_path)?;
    Ok(())
}
pub(crate) fn cursor_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("cursor-hook.sh")
}

pub(crate) fn cursor_hooks_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".cursor").join("hooks.json"))
}

pub(crate) fn cursor_mcp_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".cursor").join("mcp.json"))
}

/// Merge Unpeel MCP server entries into `~/.cursor/mcp.json` for cursor-agent.
/// Called per launch with the session's grant flags so the browser entry
/// is added or removed. Rewritten every launch so executable paths stay current.
/// No per-session values are baked into the file (it is global and shared by
/// concurrent sessions) — and cursor-agent spawns MCP servers with a stripped
/// environment, so `UNPEEL_SESSION_ID` never arrives by inheritance either.
/// Caller identity comes from `mcp_host::self_session_id`'s process-ancestry
/// fallback instead.
pub fn write_cursor_mcp_config(unified_mcp_enabled: bool) -> Result<(), String> {
    let exe = crate::session_host::resolve_current_executable()?;
    let unified = json!({
        "type": "stdio",
        "command": exe.to_string_lossy(),
        "args": [crate::mcp_host::MCP_HOST_ARG],
    });
    // One unified entry per launch; the legacy names (`unpeel-mcp` before the
    // 2026-07-25 rename, the per-domain pair before unification) are pruned
    // so Cursor sessions don't see the same domains twice.
    merge_cursor_mcp_servers_at(
        cursor_mcp_path().as_deref(),
        [
            ("unpeel", unified_mcp_enabled.then_some(unified)),
            ("unpeel-mcp", None),
            ("unpeel-sessions", None),
            ("unpeel-browser", None),
        ],
    )
}

pub(crate) fn merge_cursor_mcp_servers_at(
    mcp_path: Option<&std::path::Path>,
    servers: [(&str, Option<Value>); 4],
) -> Result<(), String> {
    let Some(mcp_path) = mcp_path else {
        return Ok(());
    };
    if let Some(parent) = mcp_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create Cursor MCP dir {}: {e}", parent.display()))?;
    }

    let Some(mut mcp_json) = read_mergeable_json_object(mcp_path, "Cursor mcp.json")? else {
        return Ok(());
    };
    let root = mcp_json.as_object_mut().unwrap();
    let servers_root = root.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers_root.is_object() {
        *servers_root = json!({});
    }
    let servers_obj = servers_root.as_object_mut().unwrap();
    for (name, entry) in servers {
        match entry {
            Some(value) => {
                servers_obj.insert(name.to_string(), value);
            }
            None => {
                servers_obj.remove(name);
            }
        }
    }

    let serialized = serde_json::to_string_pretty(&mcp_json)
        .map_err(|e| format!("Failed to serialize Cursor mcp.json: {e}"))?;
    write_file_atomic(mcp_path, &format!("{serialized}\n"), "Cursor mcp.json")?;
    Ok(())
}
pub(crate) fn ensure_cursor_hooks(script_path: &Path) -> Result<(), String> {
    let Some(hooks_path) = cursor_hooks_path() else {
        return Ok(());
    };
    if let Some(parent) = hooks_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create Cursor hooks dir {}: {e}",
                parent.display()
            )
        })?;
    }

    let Some(mut hooks_json) = read_mergeable_json_object(&hooks_path, "Cursor hooks.json")? else {
        return Ok(());
    };
    let root = hooks_json.as_object_mut().unwrap();
    root.entry("version").or_insert_with(|| json!(1));
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks_obj = hooks.as_object_mut().unwrap();

    let command = script_path.to_string_lossy().to_string();
    let desired = [
        ("beforeSubmitPrompt", format!("{command} Start")),
        ("stop", format!("{command} Stop")),
        (
            "beforeShellExecution",
            format!("{command} PermissionRequest"),
        ),
        ("beforeMCPExecution", format!("{command} PermissionRequest")),
    ];

    let mut changed = false;
    for (event_name, hook_command) in desired {
        let entries = hooks_obj
            .entry(event_name.to_string())
            .or_insert_with(|| json!([]));
        if !entries.is_array() {
            *entries = json!([]);
        }
        let array = entries.as_array_mut().unwrap();
        let already_installed = array.iter().any(|entry| {
            entry.get("command").and_then(|value| value.as_str()) == Some(hook_command.as_str())
        });
        if !already_installed {
            array.push(json!({ "command": hook_command }));
            changed = true;
        }
    }

    if changed {
        let json = serde_json::to_string_pretty(&hooks_json)
            .map_err(|e| format!("Failed to serialize Cursor hooks.json: {e}"))?;
        write_file_atomic(&hooks_path, &format!("{json}\n"), "Cursor hooks.json")?;
    }

    Ok(())
}
