use super::{shared, Integration, RuntimeLaunchOptions};
use crate::session_host::SessionHostLaunch;
use portable_pty::CommandBuilder;
use std::path::PathBuf;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/kimi/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/kimi/adapter/setup.rs"
    ));
}

fn kimi_bin_dir() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("KIMI_CODE_HOME") {
        return Some(PathBuf::from(root).join("bin"));
    }
    dirs::home_dir().map(|home| home.join(".kimi-code").join("bin"))
}

fn configure_host_command(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    // Kiro's global zsh integration can intercept a `zsh -c` launch from the
    // first line of ~/.zshrc and execute it before later PATH exports run.
    // Kimi Code installs into ~/.kimi-code/bin, so make that location available
    // in both the process environment and the captured startup command.
    if let Some(bin_dir) = kimi_bin_dir() {
        let current_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path_dirs = vec![bin_dir.clone()];
        path_dirs.extend(std::env::split_paths(&current_path));
        if let Ok(path) = std::env::join_paths(path_dirs) {
            cmd.env("PATH", path);
        }
        shell_prelude.push(format!(
            "export PATH={}:\"$PATH\"",
            shared::shell_quote(&bin_dir.to_string_lossy())
        ));
    }

    // Kimi Code 0.27 only reads persistent MCP configuration. The managed
    // entries installed in ~/.kimi-code/mcp.json point at an environment gate,
    // so carry the launch's actual grants into both the PTY process and its
    // login-shell prelude. Legacy Kimi ignores these variables and continues to
    // receive its per-launch config files below.
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

/// Legacy Kimi accepts repeatable `--mcp-config-file` arguments. Kimi Code
/// 0.27 removed those flags and loads the environment-gated entries Unpeel
/// installs in `~/.kimi-code/mcp.json` instead. Probe the executable's own help
/// at launch so one preset remains compatible with both generations.
pub(crate) fn startup_command(command: &str, unified_mcp_enabled: bool) -> String {
    let trimmed = command.trim();
    if !shared::command_head(trimmed).eq_ignore_ascii_case("kimi") {
        return trimmed.to_string();
    }

    if !unified_mcp_enabled {
        return trimmed.to_string();
    }

    let legacy = legacy_startup_command(trimmed, unified_mcp_enabled);
    format!(
        "if kimi --help 2>&1 | grep -q -- '--mcp-config-file'; then {legacy}; else {trimmed}; fi"
    )
}

fn legacy_startup_command(command: &str, unified_mcp_enabled: bool) -> String {
    let mut result = command.to_string();
    // Kimi only auto-loads ~/.kimi/mcp.json when *no* explicit MCP config was
    // supplied. Adding Unpeel's file would otherwise silently hide every MCP
    // server the user configured through `kimi mcp add`.
    if !command_has_mcp_config(command) {
        if let Some(global) = setup::kimi_global_mcp_config_path().filter(|path| path.is_file()) {
            result = format!(
                "{result} --mcp-config-file {}",
                shared::shell_quote(&global.to_string_lossy())
            );
        }
    }
    if unified_mcp_enabled {
        let path = setup::kimi_unpeel_mcp_config_path();
        let quoted = shared::shell_quote(&path.to_string_lossy());
        if !result.contains(&quoted) && !result.contains(path.to_string_lossy().as_ref()) {
            result = format!("{result} --mcp-config-file {quoted}");
        }
    }
    result
}

fn command_has_mcp_config(command: &str) -> bool {
    command.split_whitespace().skip(1).any(|token| {
        let token = token.to_ascii_lowercase();
        token == "--mcp-config-file"
            || token.starts_with("--mcp-config-file=")
            || token == "--mcp-config"
            || token.starts_with("--mcp-config=")
    })
}

fn prepare_startup_command(command: &str, options: RuntimeLaunchOptions) -> String {
    startup_command(command, options.any_mcp())
}

fn has_automatic_mcp_setup(_command: &str) -> bool {
    true
}

/// Compatibility for Kimi MCP entries installed before grants moved to the
/// provider-neutral environment names. Keep these aliases runtime-local so
/// the shared gate does not learn provider variables.
fn legacy_mcp_gate_granted(kind: &str) -> bool {
    legacy_mcp_gate_granted_with(kind, |name| std::env::var(name).ok())
}

fn legacy_mcp_gate_granted_with(kind: &str, read_env: impl FnOnce(&str) -> Option<String>) -> bool {
    let name = match kind {
        crate::mcp_gate::SESSIONS_KIND => "UNPEEL_KIMI_SESSIONS_MCP_ENABLED",
        crate::mcp_gate::BROWSER_KIND => "UNPEEL_KIMI_BROWSER_MCP_ENABLED",
        crate::mcp_gate::COMPUTER_KIND => "UNPEEL_KIMI_COMPUTER_MCP_ENABLED",
        _ => return false,
    };
    read_env(name).as_deref() == Some("1")
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_kimi_hooks),
    Some(configure_host_command),
)
.with_startup_command(prepare_startup_command)
.with_automatic_mcp_setup(has_automatic_mcp_setup)
.with_resume_adapter(resume::ADAPTER)
.with_legacy_mcp_gate_grant(legacy_mcp_gate_granted);

#[cfg(test)]
mod tests {
    use super::{
        configure_host_command, legacy_mcp_gate_granted_with, legacy_startup_command,
        startup_command,
    };
    use portable_pty::CommandBuilder;

    #[test]
    fn chooses_legacy_flags_only_when_the_installed_cli_supports_them() {
        assert_eq!(startup_command("kimi --yolo", false), "kimi --yolo");

        let result = startup_command("kimi --yolo", true);
        assert_eq!(result.matches("kimi-unpeel-mcp.json").count(), 1);
        assert!(!result.contains("/kimi-mcp.json"));
        assert!(!result.contains("kimi-browser-mcp.json"));
        assert!(result.contains("grep -q -- '--mcp-config-file'"));
        assert!(result.ends_with("else kimi --yolo; fi"));
    }

    #[test]
    fn does_not_duplicate_unpeel_configs() {
        let command = format!(
            "kimi --mcp-config-file {}",
            super::shared::shell_quote(
                &super::setup::kimi_unpeel_mcp_config_path().to_string_lossy()
            )
        );
        assert_eq!(
            legacy_startup_command(&command, true)
                .matches("kimi-unpeel-mcp.json")
                .count(),
            1
        );
    }

    #[test]
    fn detects_user_mcp_flags() {
        assert!(super::command_has_mcp_config(
            "kimi --mcp-config-file /tmp/user.json"
        ));
        assert!(super::command_has_mcp_config(
            r#"kimi --mcp-config={"mcpServers":{}}"#
        ));
        assert!(!super::command_has_mcp_config("kimi --yolo"));
    }

    #[test]
    fn exports_per_session_grants_for_kimi_code_gate() {
        let launch: super::SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "kimi-session",
                "project_id": "test-project",
                "label": "Kimi",
                "command": "kimi --yolo"
            },
            "cwd": "/tmp",
            "dark_mode": null,
            "hook_port": 4321,
            "mcp_enabled": true,
            "browser_mcp_enabled": false
        }))
        .expect("launch fixture");
        let mut command = CommandBuilder::new("true");
        let mut prelude = Vec::new();
        configure_host_command(&launch, &mut command, &mut prelude).expect("configure");
        let shell = prelude.join("\n");
        assert!(shell.contains(".kimi-code/bin"));
        assert!(shell.contains("UNPEEL_SESSIONS_MCP_ENABLED='1'"));
        assert!(shell.contains("UNPEEL_BROWSER_MCP_ENABLED='0'"));
    }

    #[test]
    fn legacy_kimi_gate_env_names_keep_exact_one_semantics() {
        let granted = |expected: &'static str, value: &'static str| {
            move |name: &str| (name == expected).then(|| value.to_string())
        };
        assert!(legacy_mcp_gate_granted_with(
            crate::mcp_gate::SESSIONS_KIND,
            granted("UNPEEL_KIMI_SESSIONS_MCP_ENABLED", "1")
        ));
        assert!(legacy_mcp_gate_granted_with(
            crate::mcp_gate::BROWSER_KIND,
            granted("UNPEEL_KIMI_BROWSER_MCP_ENABLED", "1")
        ));
        assert!(legacy_mcp_gate_granted_with(
            crate::mcp_gate::COMPUTER_KIND,
            granted("UNPEEL_KIMI_COMPUTER_MCP_ENABLED", "1")
        ));
        assert!(!legacy_mcp_gate_granted_with(
            crate::mcp_gate::SESSIONS_KIND,
            granted("UNPEEL_KIMI_SESSIONS_MCP_ENABLED", "true")
        ));
    }
}
