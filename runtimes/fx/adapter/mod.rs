use super::{shared, Integration};
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/fx/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/fx/adapter/setup.rs"
    ));
}

/// fx only reads persistent MCP configuration (`~/.fx/mcp.json`), where the
/// managed entry points at the environment gate. Carry the launch's actual
/// grants into the PTY process and its login-shell prelude; fx's stdio MCP
/// children inherit that environment, so each session's gate resolves its own
/// identity and grants without rewriting the shared config file.
fn configure_host_command(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    let sessions = if launch.mcp_enabled { "1" } else { "0" };
    let browser = if launch.browser_mcp_enabled { "1" } else { "0" };
    let computer = if launch.computer_mcp_enabled {
        "1"
    } else {
        "0"
    };
    cmd.env(crate::mcp_gate::SESSIONS_ENABLED_ENV, sessions);
    cmd.env(crate::mcp_gate::BROWSER_ENABLED_ENV, browser);
    cmd.env(crate::mcp_gate::COMPUTER_ENABLED_ENV, computer);
    shell_prelude.push(format!(
        "export {}={} {}={} {}={}",
        crate::mcp_gate::SESSIONS_ENABLED_ENV,
        shared::shell_quote(sessions),
        crate::mcp_gate::BROWSER_ENABLED_ENV,
        shared::shell_quote(browser),
        crate::mcp_gate::COMPUTER_ENABLED_ENV,
        shared::shell_quote(computer),
    ));
    Ok(())
}

fn has_automatic_mcp_setup(_command: &str) -> bool {
    true
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_fx_runtime_support),
    Some(configure_host_command),
)
.with_automatic_mcp_setup(has_automatic_mcp_setup)
.with_resume_adapter(resume::ADAPTER);

#[cfg(test)]
mod tests {
    use super::configure_host_command;
    use crate::session_host::SessionHostLaunch;
    use portable_pty::CommandBuilder;

    #[test]
    fn exports_generic_gate_grants_for_inherited_mcp_children() {
        let launch: SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "fx-session",
                "project_id": "test-project",
                "label": "test",
                "command": "fx"
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
        configure_host_command(&launch, &mut cmd, &mut prelude).expect("configure fx");
        let exports = prelude.join("\n");
        assert!(exports.contains("UNPEEL_SESSIONS_MCP_ENABLED='1'"));
        assert!(exports.contains("UNPEEL_BROWSER_MCP_ENABLED='0'"));
        assert!(exports.contains("UNPEEL_COMPUTER_MCP_ENABLED='0'"));
    }
}
