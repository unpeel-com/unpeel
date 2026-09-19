use crate::hook_assets::{read_mergeable_json_object, write_file_atomic};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// Unpeel's OMP lifecycle reporter. OMP has no shell-hook configuration: its
/// extension event bus is the provider's own lifecycle mechanism, so the
/// reporter is an extension module the installer drops into the agent
/// directory, where OMP's native discovery loads it.
pub(crate) const OMP_LIFECYCLE_EXTENSION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/omp/assets/extensions/unpeel-lifecycle.ts"
));

pub(crate) const OMP_EXTENSION_FILE_NAME: &str = "unpeel-lifecycle.ts";

/// OMP resolves its agent directory from `PI_CODING_AGENT_DIR` when a profile
/// or a user shell alias sets it, and from `~/.omp/agent` otherwise. Extension
/// auto-discovery reads `<agent dir>/extensions`, MCP config `<agent dir>/mcp.json`.
pub(crate) fn omp_agent_dir() -> Option<PathBuf> {
    let configured = std::env::var_os("PI_CODING_AGENT_DIR").filter(|value| !value.is_empty());
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        return Some(if path.is_absolute() {
            path
        } else {
            std::env::current_dir().ok()?.join(path)
        });
    }
    Some(dirs::home_dir()?.join(".omp").join("agent"))
}

pub(crate) fn omp_extension_path() -> Option<PathBuf> {
    Some(
        omp_agent_dir()?
            .join("extensions")
            .join(OMP_EXTENSION_FILE_NAME),
    )
}

pub(crate) fn omp_mcp_config_path() -> Option<PathBuf> {
    Some(omp_agent_dir()?.join("mcp.json"))
}

/// The managed `unpeel` stdio entry. No `env` block: the shim resolves the
/// calling Session from the inherited environment, or from process ancestry
/// when the launcher strips it, and serves no tools outside a hosted Session.
pub(crate) fn omp_mcp_server_value(shim: &str) -> Value {
    json!({
        "type": "stdio",
        "command": shim,
        "args": [],
    })
}

/// An entry is Unpeel-owned when it starts the shim, or (older builds) the
/// Host binary through the MCP gate.
pub(crate) fn omp_mcp_entry_is_managed(value: &Value) -> bool {
    let shim = value
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(crate::integrations::install::is_mcp_shim_command);
    shim || value
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|args| {
            args.first().and_then(Value::as_str) == Some(crate::mcp_gate::MCP_GATE_ARG)
        })
}

/// Install the OMP integration: the lifecycle extension in the agent
/// directory and the Unpeel MCP shim as a persistent `mcp.json` entry.
pub fn install() -> Result<(), String> {
    if let Some(path) = omp_extension_path() {
        install_lifecycle_extension_at(&path)?;
    }
    let Some(path) = omp_mcp_config_path() else {
        return Ok(());
    };
    let shim = crate::integrations::install::write_mcp_shim()?;
    write_omp_mcp_config_at(&path, &shim.to_string_lossy())
}

pub(crate) fn install_lifecycle_extension_at(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create OMP extensions dir {}: {error}",
                parent.display()
            )
        })?;
    }
    write_file_atomic(path, OMP_LIFECYCLE_EXTENSION, "OMP lifecycle extension")
}

/// Merge one `unpeel` entry into OMP's own MCP config. OMP starts every
/// `mcpServers` entry of this file, so the merge preserves unrelated servers
/// and never replaces a user's own `unpeel` server: a conflicting name falls
/// back to `omp-unpeel`.
pub(crate) fn upsert_omp_managed_mcp(
    servers: &mut serde_json::Map<String, Value>,
    entry: Value,
) {
    let name = match servers.get("unpeel") {
        None => "unpeel",
        Some(existing) if omp_mcp_entry_is_managed(existing) => "unpeel",
        Some(_) => "omp-unpeel",
    };
    if servers.get(name).is_none_or(omp_mcp_entry_is_managed) {
        servers.insert(name.to_string(), entry);
    }
}

pub(crate) fn write_omp_mcp_config_at(path: &Path, shim: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create OMP agent dir {}: {error}", parent.display()))?;
    }
    let _lock = crate::app_state::lock_exclusive(path)?;
    let Some(mut config) = read_mergeable_json_object(path, "OMP mcp.json")? else {
        // Never replace malformed user configuration.
        return Ok(());
    };
    let Some(root) = config.as_object_mut() else {
        return Ok(());
    };
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Ok(());
    };
    upsert_omp_managed_mcp(servers, omp_mcp_server_value(shim));
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("Failed to serialize OMP mcp.json: {error}"))?;
    write_file_atomic(path, &format!("{serialized}\n"), "OMP mcp.json")
}