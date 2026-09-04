use super::{shared, Integration, RuntimeLaunchOptions};

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/claude-code/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/claude-code/adapter/setup.rs"
    ));
}

/// Register the unified Unpeel MCP server with a Claude launch. One additive
/// `--mcp-config` carries every enabled domain (sessions, browser, …) — the
/// server itself advertises only the domains this session launched with, read
/// from its manifest. `install_claude_hooks` writes the config file before
/// launch. A user command that already passes `--mcp-config` launches
/// untouched.
pub(crate) fn startup_command(command: &str, unified_mcp_enabled: bool) -> String {
    let trimmed = command.trim();
    if !shared::command_head(trimmed).eq_ignore_ascii_case("claude")
        || trimmed.contains("--mcp-config")
        || !unified_mcp_enabled
    {
        return trimmed.to_string();
    }
    let config_path = setup::claude_unpeel_mcp_config_path();
    format!(
        "{} --mcp-config {}",
        trimmed,
        shared::shell_quote(&config_path.to_string_lossy())
    )
}

fn prepare_startup_command(command: &str, options: RuntimeLaunchOptions) -> String {
    startup_command(command, options.any_mcp())
}

fn has_automatic_mcp_setup(command: &str) -> bool {
    startup_command(command, true) != command.trim()
}

pub(crate) const INTEGRATION: Integration =
    Integration::new(Some(setup::install_claude_hooks), None)
        .with_startup_command(prepare_startup_command)
        .with_automatic_mcp_setup(has_automatic_mcp_setup)
        .with_resume_adapter(resume::ADAPTER);

#[cfg(test)]
mod tests {
    use super::startup_command;

    #[test]
    fn appends_one_unified_config_when_any_domain_is_enabled() {
        assert_eq!(startup_command("claude", false), "claude");

        let result = startup_command("claude", true);
        assert!(result.contains("claude-unpeel-mcp.json"));
        assert_eq!(result.matches("--mcp-config").count(), 1);
    }

    #[test]
    fn user_supplied_mcp_config_launches_untouched() {
        let command = "claude --mcp-config /tmp/custom.json";
        assert_eq!(startup_command(command, true), command);
    }

    #[test]
    fn non_claude_commands_pass_through() {
        assert_eq!(startup_command("codex", true), "codex");
    }
}
