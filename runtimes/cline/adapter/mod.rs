use super::{shared, Integration, RuntimeLaunchOptions};
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;
use serde_json::{json, Map, Value};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/cline/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/cline/adapter/setup.rs"
    ));
}

fn configure_host_command(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    configure_session_hub(launch, cmd, shell_prelude)?;

    if !launch.mcp_enabled && !launch.browser_mcp_enabled && !launch.computer_mcp_enabled {
        return Ok(());
    }

    let config_path = write_session_mcp_config(launch)?;
    let config_value = config_path.to_string_lossy().to_string();
    cmd.env("CLINE_MCP_SETTINGS_PATH", &config_value);
    shell_prelude.push(format!(
        "export CLINE_MCP_SETTINGS_PATH={}",
        shared::shell_quote(&config_value)
    ));
    Ok(())
}

fn configure_session_hub(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    // Cline 3 runs agent sessions inside a detached hub daemon. That daemon
    // inherits the environment of whichever CLI starts it, so using Cline's
    // default shared hub would make later Unpeel sessions inherit the first
    // session's hook identity and MCP settings path. Give every hosted
    // terminal its own discovery record and endpoint while leaving Cline's
    // canonical session/settings storage shared.
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("Failed to reserve a Cline hub port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("Failed to resolve the Cline hub port: {error}"))?
        .port()
        .to_string();
    drop(listener);

    let discovery = crate::app_paths::app_sessions_root()
        .join(&launch.session.id)
        .join("cline-hub-discovery.json")
        .to_string_lossy()
        .to_string();
    cmd.env("CLINE_HUB_DISCOVERY_PATH", &discovery);
    cmd.env("CLINE_HUB_PORT", &port);
    shell_prelude.push(format!(
        "export CLINE_HUB_DISCOVERY_PATH={} CLINE_HUB_PORT={}",
        shared::shell_quote(&discovery),
        shared::shell_quote(&port),
    ));
    // HUP/TERM exits run the EXIT cleanup; a normal Cline exit is also cleaned
    // by startup_command below. This scopes hub lifetime to the hosted terminal.
    shell_prelude.push(
        "trap 'cline hub stop >/dev/null 2>&1 || true' EXIT; \
         trap 'exit 129' HUP; trap 'exit 143' TERM"
            .to_string(),
    );
    Ok(())
}

pub(crate) fn startup_command(command: &str) -> String {
    let command = command.trim();
    format!(
        "{{ {command}; __unpeel_cline_status=$?; \
         cline hub stop >/dev/null 2>&1 || true; \
         (exit \"$__unpeel_cline_status\"); }}"
    )
}

fn write_session_mcp_config(launch: &SessionHostLaunch) -> Result<PathBuf, String> {
    let session_dir = crate::app_paths::app_sessions_root().join(&launch.session.id);
    fs::create_dir_all(&session_dir).map_err(|error| {
        format!(
            "Failed to create Cline session config dir {}: {error}",
            session_dir.display()
        )
    })?;
    let target = session_dir.join("cline-mcp-settings.json");
    let mut config = read_user_mcp_config()?;
    let servers = config
        .as_object_mut()
        .expect("read_user_mcp_config always returns an object")
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        return Err("Cline MCP config has a non-object mcpServers field.".to_string());
    }
    let servers = servers.as_object_mut().unwrap();
    let executable = crate::session_host::resolve_current_executable()?;
    let executable = executable.to_string_lossy().to_string();

    // One unified server carries every granted domain; it advertises only
    // the domains recorded in this session's manifest.
    if launch.mcp_enabled || launch.browser_mcp_enabled || launch.computer_mcp_enabled {
        servers.insert(
            "unpeel".to_string(),
            stdio_server(&executable, crate::mcp_host::MCP_HOST_ARG),
        );
    }

    let serialized = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("Failed to serialize Cline MCP config: {error}"))?;
    write_atomic(&target, &format!("{serialized}\n"))?;
    Ok(target)
}

fn stdio_server(executable: &str, arg: &str) -> Value {
    json!({
        "transport": {
            "type": "stdio",
            "command": executable,
            "args": [arg],
        }
    })
}

fn read_user_mcp_config() -> Result<Value, String> {
    let path = cline_user_mcp_config_path();
    if !path.is_file() {
        return Ok(json!({}));
    }
    let raw = fs::read_to_string(&path).map_err(|error| {
        format!(
            "Failed to read Cline MCP config {}: {error}",
            path.display()
        )
    })?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "Cline MCP config {} is invalid JSON: {error}",
            path.display()
        )
    })?;
    if !value.is_object() {
        return Err(format!(
            "Cline MCP config {} must contain a JSON object.",
            path.display()
        ));
    }
    Ok(value)
}

fn cline_user_mcp_config_path() -> PathBuf {
    if let Some(path) =
        std::env::var_os("CLINE_MCP_SETTINGS_PATH").filter(|value| !value.is_empty())
    {
        return resolve_path(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("CLINE_DATA_DIR").filter(|value| !value.is_empty()) {
        return resolve_path(PathBuf::from(path))
            .join("settings")
            .join("cline_mcp_settings.json");
    }
    setup::cline_home_dir()
        .join("data")
        .join("settings")
        .join("cline_mcp_settings.json")
}

fn resolve_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let temporary = path.with_file_name(format!(
        ".{}.unpeel-tmp.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cline-mcp-settings"),
        std::process::id()
    ));
    fs::write(&temporary, contents).map_err(|error| {
        format!(
            "Failed to write Cline MCP config {}: {error}",
            temporary.display()
        )
    })?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "Failed to install Cline MCP config {}: {error}",
            path.display()
        ));
    }
    Ok(())
}

fn prepare_startup_command(command: &str, _options: RuntimeLaunchOptions) -> String {
    startup_command(command)
}

fn has_automatic_mcp_setup(_command: &str) -> bool {
    true
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_cline_hooks),
    Some(configure_host_command),
)
.with_startup_command(prepare_startup_command)
.with_automatic_mcp_setup(has_automatic_mcp_setup)
.with_resume_adapter(resume::ADAPTER);

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(sessions: bool, browser: bool) -> SessionHostLaunch {
        serde_json::from_value(serde_json::json!({
            "session": {
                "id": "cline-session",
                "project_id": "test-project",
                "label": "test",
                "command": "cline"
            },
            "cwd": "/tmp",
            "dark_mode": null,
            "hook_port": 4321,
            "mcp_enabled": sessions,
            "browser_mcp_enabled": browser
        }))
        .expect("launch fixture")
    }

    #[test]
    fn no_grants_still_isolate_the_cline_hub() {
        let mut command = CommandBuilder::new("true");
        let mut prelude = Vec::new();
        configure_host_command(&launch(false, false), &mut command, &mut prelude)
            .expect("configure Cline");
        let shell = prelude.join("\n");
        assert!(shell.contains("CLINE_HUB_DISCOVERY_PATH="));
        assert!(shell.contains("CLINE_HUB_PORT="));
        assert!(!shell.contains("CLINE_MCP_SETTINGS_PATH="));
    }

    #[test]
    fn stdio_server_uses_current_cline_transport_shape() {
        assert_eq!(
            stdio_server("/tmp/unpeel-host", "__mcp__"),
            json!({
                "transport": {
                    "type": "stdio",
                    "command": "/tmp/unpeel-host",
                    "args": ["__mcp__"]
                }
            })
        );
    }

    #[test]
    fn startup_stops_the_session_scoped_hub_without_hiding_exit_status() {
        let command = startup_command("cline --plan");
        assert!(command.contains("cline --plan"));
        assert!(command.contains("cline hub stop"));
        assert!(command.contains("__unpeel_cline_status"));
    }
}
