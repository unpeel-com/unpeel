use crate::hook_assets::read_mergeable_json_object;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

/// fx loads MCP servers only from this global file (`profile_paths.zig` +
/// `builtins/mcp.zig` resolve `$HOME/.fx/mcp.json`; there is no per-launch
/// flag, environment override, or project-level MCP source).
pub(crate) fn fx_mcp_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".fx").join("mcp.json"))
}

/// The managed `unpeel` entry points at the provider-neutral environment
/// gate. Deliberately no `environment` block: fx replaces the child's whole
/// environment when one is declared and inherits the parent's otherwise
/// (`mcp_runtime.zig` builds an env map only from configured entries), so
/// omitting it is what carries `UNPEEL_SESSION_ID` and the per-launch grant
/// variables into each session's gate process. Outside a granted hosted
/// Session the gate serves a valid endpoint with no tools.
pub(crate) fn fx_mcp_server_value(executable: &str) -> Value {
    json!({
        "type": "local",
        "command": [
            executable,
            crate::mcp_gate::MCP_GATE_ARG,
            crate::mcp_gate::UNIFIED_KIND,
        ],
        "enabled": true,
    })
}

pub fn install_fx_runtime_support() -> Result<(), String> {
    let Some(path) = fx_mcp_config_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create fx config dir: {error}"))?;
    }
    let Some(mut config) = read_mergeable_json_object(&path, "fx mcp.json")? else {
        // Never replace malformed user configuration.
        return Ok(());
    };
    let root = config.as_object_mut().unwrap();
    let servers = root.entry("mcp").or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Ok(());
    }
    let executable = crate::session_host::resolve_current_executable()?;
    let desired = fx_mcp_server_value(&executable.to_string_lossy());
    let servers = servers.as_object_mut().unwrap();
    if servers.get("unpeel") == Some(&desired) {
        return Ok(());
    }
    servers.insert("unpeel".into(), desired);
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("Failed to serialize fx mcp.json: {error}"))?;
    crate::hook_assets::write_file_atomic(&path, &format!("{serialized}\n"), "fx mcp.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_entry_declares_no_environment_so_identity_is_inherited() {
        let entry = fx_mcp_server_value("/bin/unpeel-host");
        assert_eq!(entry["type"], "local");
        assert_eq!(
            entry["command"],
            json!(["/bin/unpeel-host", "__mcp_gate__", "unified"])
        );
        assert_eq!(entry["enabled"], true);
        assert!(entry.get("environment").is_none());
        assert!(entry.get("env").is_none());
    }
}
