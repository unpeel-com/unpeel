use crate::app_paths::unpeel_home;
use crate::hook_assets::{read_mergeable_json_object, write_executable_script, write_file_atomic};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const KIRO_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/kiro/assets/hooks/lifecycle.sh"
));

pub fn install_kiro_hooks() -> Result<(), String> {
    let script_path = kiro_hook_script_path();
    write_executable_script(&script_path, KIRO_HOOK_SCRIPT, "Kiro hook script")?;
    write_kiro_v3_hooks(&script_path)?;
    write_kiro_v2_agent(&script_path)?;
    write_kiro_mcp_config()?;
    Ok(())
}
pub(crate) fn kiro_home_dir() -> Option<PathBuf> {
    std::env::var_os("KIRO_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".kiro")))
}

pub(crate) fn kiro_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("kiro-hook.sh")
}

pub(crate) fn kiro_v3_hooks_path() -> Option<PathBuf> {
    Some(kiro_home_dir()?.join("hooks").join("unpeel.json"))
}

pub(crate) fn kiro_v2_agent_path() -> Option<PathBuf> {
    Some(kiro_home_dir()?.join("agents").join("unpeel-runtime.json"))
}

pub(crate) fn kiro_mcp_path() -> Option<PathBuf> {
    Some(kiro_home_dir()?.join("settings").join("mcp.json"))
}
pub(crate) fn kiro_hook_command(script_path: &Path, event: &str) -> String {
    format!(
        "{} {event}",
        crate::integrations::shared::shell_quote(&script_path.to_string_lossy())
    )
}

pub(crate) fn write_kiro_v3_hooks(script_path: &Path) -> Result<(), String> {
    let Some(path) = kiro_v3_hooks_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create Kiro hooks dir: {error}"))?;
    }
    let definitions = [
        ("session-start", "SessionStart", None),
        ("prompt", "UserPromptSubmit", None),
        ("pre-tool", "PreToolUse", Some(".*")),
        ("post-tool", "PostToolUse", Some(".*")),
        ("stop", "Stop", None),
    ];
    let hooks = definitions
        .into_iter()
        .map(|(name, trigger, matcher)| {
            let mut hook = json!({
                "name": format!("unpeel-{name}"),
                "trigger": trigger,
                "action": {
                    "type": "command",
                    "command": kiro_hook_command(script_path, trigger),
                },
                "timeout": 5,
            });
            if let Some(matcher) = matcher {
                hook["matcher"] = Value::String(matcher.to_string());
            }
            hook
        })
        .collect::<Vec<_>>();
    let config = json!({ "version": "v1", "hooks": hooks });
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("Failed to serialize Kiro v3 hooks: {error}"))?;
    write_file_atomic(&path, &format!("{serialized}\n"), "Kiro v3 hooks")
}

pub(crate) fn write_kiro_v2_agent(script_path: &Path) -> Result<(), String> {
    let Some(path) = kiro_v2_agent_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create Kiro agents dir: {error}"))?;
    }
    let entry = |event: &str| json!({ "command": kiro_hook_command(script_path, event) });
    let matched_entry = |event: &str| {
        json!({
            "matcher": "*",
            "command": kiro_hook_command(script_path, event),
        })
    };
    let config = json!({
        "name": "unpeel-runtime",
        "description": "Unpeel lifecycle integration for Kiro CLI v1/v2.",
        "prompt": Value::Null,
        "mcpServers": {},
        "tools": ["*"],
        "allowedTools": [],
        "resources": [],
        "includeMcpJson": true,
        "hooks": {
            "agentSpawn": [entry("agentSpawn")],
            "userPromptSubmit": [entry("userPromptSubmit")],
            "preToolUse": [matched_entry("preToolUse")],
            "postToolUse": [matched_entry("postToolUse")],
            "stop": [entry("stop")],
        }
    });
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("Failed to serialize Kiro v2 agent: {error}"))?;
    write_file_atomic(&path, &format!("{serialized}\n"), "Kiro v2 agent")
}

pub(crate) fn write_kiro_mcp_config() -> Result<(), String> {
    let Some(path) = kiro_mcp_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create Kiro MCP dir: {error}"))?;
    }
    let Some(mut config) = read_mergeable_json_object(&path, "Kiro mcp.json")? else {
        return Ok(());
    };
    let root = config.as_object_mut().unwrap();
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    // Never replace a non-object user value with an Unpeel-only map.
    if !servers.is_object() {
        return Ok(());
    }
    let exe = crate::session_host::resolve_current_executable()?;
    let servers = servers.as_object_mut().unwrap();
    servers.insert("unpeel".into(), kiro_mcp_server_value(&exe));
    // Prune the pre-rename `unpeel-mcp` entry only when its argv matches an
    // Unpeel-owned Kiro server. Keep the legacy argv recognizable across the
    // migration to the provider-neutral MCP gate.
    if servers
        .get("unpeel-mcp")
        .is_some_and(is_owned_kiro_mcp_entry)
    {
        servers.remove("unpeel-mcp");
    }
    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("Failed to serialize Kiro mcp.json: {error}"))?;
    write_file_atomic(&path, &format!("{serialized}\n"), "Kiro mcp.json")
}

pub(crate) fn kiro_mcp_server_value(exe: &Path) -> Value {
    json!({
        "command": exe.to_string_lossy(),
        "args": [crate::mcp_gate::MCP_GATE_ARG, crate::mcp_gate::UNIFIED_KIND],
        // Kiro v3 intentionally gives MCP subprocesses only the variables
        // declared in this block. These aliases are always set on an
        // Unpeel launch, so concurrent Kiro sessions each resolve their own
        // identity, home, and capability grants without rewriting the shared
        // settings file.
        "env": {
            "UNPEEL_SESSIONS_MCP_ENABLED": "${UNPEEL_KIRO_SESSIONS_MCP_ENABLED}",
            "UNPEEL_BROWSER_MCP_ENABLED": "${UNPEEL_KIRO_BROWSER_MCP_ENABLED}",
            "UNPEEL_SESSION_ID": "${UNPEEL_KIRO_SESSION_ID}",
            "UNPEEL_APP_PORT": "${UNPEEL_KIRO_APP_PORT}",
            "UNPEEL_HOME": "${UNPEEL_KIRO_UNPEEL_HOME}",
            "UNPEEL_BROWSER_BIN": "${UNPEEL_KIRO_BROWSER_BIN}",
        },
    })
}

fn is_owned_kiro_mcp_entry(entry: &Value) -> bool {
    let Some(args) = entry.get("args").and_then(Value::as_array) else {
        return false;
    };
    let argv = args.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    matches!(argv.as_slice(), ["__kiro_mcp__"])
        || matches!(
            argv.as_slice(),
            [gate, kind]
                if *gate == crate::mcp_gate::MCP_GATE_ARG
                    && *kind == crate::mcp_gate::UNIFIED_KIND
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_recognizes_only_unpeel_owned_kiro_server_argv() {
        assert!(is_owned_kiro_mcp_entry(
            &json!({ "args": ["__kiro_mcp__"] })
        ));
        assert!(is_owned_kiro_mcp_entry(&json!({
            "args": [crate::mcp_gate::MCP_GATE_ARG, crate::mcp_gate::UNIFIED_KIND]
        })));
        assert!(!is_owned_kiro_mcp_entry(&json!({
            "args": [crate::mcp_gate::MCP_GATE_ARG, crate::mcp_gate::SESSIONS_KIND]
        })));
        assert!(!is_owned_kiro_mcp_entry(
            &json!({ "args": ["custom-server"] })
        ));
    }
}
