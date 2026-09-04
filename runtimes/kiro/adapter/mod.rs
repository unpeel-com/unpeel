use super::{shared, Integration, RuntimeLaunchOptions};
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/kiro/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/kiro/adapter/setup.rs"
    ));
}

/// V3 has true global hooks, so it keeps Kiro's built-in Default agent intact.
/// Older Kiro engines only support hooks inside an agent config; attach the
/// managed compatibility agent unless the user deliberately selected one.
pub(crate) fn startup_command(command: &str) -> String {
    let trimmed = command.trim();
    if !shared::command_head(trimmed).eq_ignore_ascii_case("kiro-cli") {
        return trimmed.to_string();
    }
    if has_flag(trimmed, "--v3")
        || has_flag(trimmed, "--agent")
        || has_flag(trimmed, "--agent-engine")
    {
        return trimmed.to_string();
    }
    format!("{trimmed} --agent unpeel-runtime")
}

fn has_flag(command: &str, flag: &str) -> bool {
    command.split_whitespace().skip(1).any(|token| {
        token.eq_ignore_ascii_case(flag)
            || token
                .to_ascii_lowercase()
                .starts_with(&format!("{}=", flag.to_ascii_lowercase()))
    })
}

fn configure_host_command(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    let sessions = if launch.mcp_enabled { "1" } else { "0" };
    let browser = if launch.browser_mcp_enabled { "1" } else { "0" };
    let session_id = launch.session.id.as_str();
    let app_port = launch
        .hook_port
        .map(|port| port.to_string())
        .unwrap_or_default();
    let unpeel_home = crate::app_paths::unpeel_home()
        .to_string_lossy()
        .to_string();
    let browser_bin = std::env::var("UNPEEL_BROWSER_BIN").unwrap_or_default();
    let executable = crate::session_host::resolve_current_executable()?;
    let executable = executable.to_string_lossy().to_string();

    cmd.env("UNPEEL_KIRO_SESSIONS_MCP_ENABLED", sessions);
    cmd.env("UNPEEL_KIRO_BROWSER_MCP_ENABLED", browser);
    cmd.env("UNPEEL_KIRO_SESSION_ID", session_id);
    cmd.env("UNPEEL_KIRO_APP_PORT", &app_port);
    cmd.env("UNPEEL_KIRO_UNPEEL_HOME", &unpeel_home);
    cmd.env("UNPEEL_KIRO_BROWSER_BIN", &browser_bin);
    cmd.env("UNPEEL_KIRO_MCP_BIN", &executable);
    shell_prelude.push(format!(
        "export UNPEEL_KIRO_SESSIONS_MCP_ENABLED={} UNPEEL_KIRO_BROWSER_MCP_ENABLED={} UNPEEL_KIRO_SESSION_ID={} UNPEEL_KIRO_APP_PORT={} UNPEEL_KIRO_UNPEEL_HOME={} UNPEEL_KIRO_BROWSER_BIN={} UNPEEL_KIRO_MCP_BIN={}",
        shared::shell_quote(sessions),
        shared::shell_quote(browser),
        shared::shell_quote(session_id),
        shared::shell_quote(&app_port),
        shared::shell_quote(&unpeel_home),
        shared::shell_quote(&browser_bin),
        shared::shell_quote(&executable),
    ));
    Ok(())
}

fn prepare_startup_command(command: &str, _options: RuntimeLaunchOptions) -> String {
    startup_command(command)
}

fn has_automatic_mcp_setup(_command: &str) -> bool {
    true
}

/// Kiro configs installed by older Unpeel builds invoke this argv directly.
/// The shared Host asks every compiled adapter for compatibility aliases, so
/// the provider spelling remains here instead of becoming a central case.
fn legacy_mcp_gate_kind(argument: &str) -> Option<&'static str> {
    (argument == "__kiro_mcp__").then_some(crate::mcp_gate::UNIFIED_KIND)
}

/// Those older configs also expand Kiro-specific grant aliases into their MCP
/// subprocess. Match the historical truthy forms while the shared gate keeps
/// enforcing a valid hosted Session identity.
fn legacy_mcp_gate_granted(kind: &str) -> bool {
    legacy_mcp_gate_granted_with(kind, |name| std::env::var(name).ok())
}

fn legacy_mcp_gate_granted_with(kind: &str, read_env: impl FnOnce(&str) -> Option<String>) -> bool {
    let name = match kind {
        crate::mcp_gate::SESSIONS_KIND => "UNPEEL_KIRO_SESSIONS_MCP_ENABLED",
        crate::mcp_gate::BROWSER_KIND => "UNPEEL_KIRO_BROWSER_MCP_ENABLED",
        _ => return false,
    };
    read_env(name).is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_kiro_hooks),
    Some(configure_host_command),
)
.with_startup_command(prepare_startup_command)
.with_automatic_mcp_setup(has_automatic_mcp_setup)
.with_resume_adapter(resume::ADAPTER)
.with_legacy_mcp_gate_kind(legacy_mcp_gate_kind)
.with_legacy_mcp_gate_grant(legacy_mcp_gate_granted);

#[cfg(test)]
mod tests {
    use super::{
        configure_host_command, legacy_mcp_gate_granted_with, legacy_mcp_gate_kind, startup_command,
    };
    use crate::session_host::SessionHostLaunch;
    use portable_pty::CommandBuilder;

    #[test]
    fn preserves_v3_default_agent() {
        assert_eq!(startup_command("kiro-cli --v3"), "kiro-cli --v3");
        assert_eq!(
            startup_command("kiro-cli --agent-engine=v3"),
            "kiro-cli --agent-engine=v3"
        );
    }

    #[test]
    fn adds_compatibility_agent_only_to_unconfigured_legacy_launches() {
        assert_eq!(
            startup_command("kiro-cli"),
            "kiro-cli --agent unpeel-runtime"
        );
        assert_eq!(
            startup_command("kiro-cli --agent mine"),
            "kiro-cli --agent mine"
        );
    }

    #[test]
    fn exports_values_kiro_expands_into_each_mcp_subprocess() {
        let launch: SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "kiro-session",
                "project_id": "test-project",
                "label": "test",
                "command": "kiro-cli --v3"
            },
            "cwd": "/tmp",
            "dark_mode": null,
            "hook_port": 4321,
            "mcp_enabled": true,
            "browser_mcp_enabled": false
        }))
        .expect("launch fixture");
        let mut cmd = CommandBuilder::new("true");
        let mut prelude = Vec::new();
        configure_host_command(&launch, &mut cmd, &mut prelude).expect("configure Kiro");
        let exports = prelude.join("\n");
        assert!(exports.contains("UNPEEL_KIRO_SESSIONS_MCP_ENABLED='1'"));
        assert!(exports.contains("UNPEEL_KIRO_BROWSER_MCP_ENABLED='0'"));
        assert!(exports.contains("UNPEEL_KIRO_SESSION_ID='kiro-session'"));
        assert!(exports.contains("UNPEEL_KIRO_APP_PORT='4321'"));
        assert!(exports.contains("UNPEEL_KIRO_UNPEEL_HOME="));
    }

    #[test]
    fn legacy_mcp_argv_and_grant_aliases_remain_accepted() {
        assert_eq!(
            legacy_mcp_gate_kind("__kiro_mcp__"),
            Some(crate::mcp_gate::UNIFIED_KIND)
        );
        assert_eq!(legacy_mcp_gate_kind("__unknown__"), None);

        for value in ["1", " true ", "YES"] {
            assert!(legacy_mcp_gate_granted_with(
                crate::mcp_gate::SESSIONS_KIND,
                |name| (name == "UNPEEL_KIRO_SESSIONS_MCP_ENABLED").then(|| value.to_string())
            ));
        }
        assert!(legacy_mcp_gate_granted_with(
            crate::mcp_gate::BROWSER_KIND,
            |name| (name == "UNPEEL_KIRO_BROWSER_MCP_ENABLED").then(|| "1".into())
        ));
        assert!(!legacy_mcp_gate_granted_with(
            crate::mcp_gate::BROWSER_KIND,
            |_| Some("0".into())
        ));
    }
}
