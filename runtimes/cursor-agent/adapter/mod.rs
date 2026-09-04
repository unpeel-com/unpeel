use super::{shared, Integration, RuntimeLaunchOptions};

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/cursor-agent/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/cursor-agent/adapter/setup.rs"
    ));
}

/// Register Unpeel MCP servers in `~/.cursor/mcp.json` (via
/// `write_cursor_mcp_config` at spawn) and auto-approve MCP servers on launch
/// whenever any unified MCP domain is enabled for the launch.
pub(crate) fn startup_command(command: &str, unified_enabled: bool) -> String {
    let trimmed = command.trim();
    if !shared::command_head(trimmed).eq_ignore_ascii_case("cursor-agent") {
        return trimmed.to_string();
    }

    let mut result = trimmed.to_string();
    if !command_has_flag(&result, "--force") && !command_has_flag(&result, "--yolo") {
        result = format!("{result} --force");
    }
    if unified_enabled && !command_has_flag(&result, "--approve-mcps") {
        result = format!("{result} --approve-mcps");
    }
    result
}

fn command_has_flag(command: &str, flag: &str) -> bool {
    command.split_whitespace().skip(1).any(|token| {
        let token = token.to_ascii_lowercase();
        let flag = flag.to_ascii_lowercase();
        token == flag || token.starts_with(&format!("{flag}="))
    })
}

fn prepare_startup_command(command: &str, options: RuntimeLaunchOptions) -> String {
    startup_command(command, options.any_mcp())
}

fn has_automatic_mcp_setup(_command: &str) -> bool {
    true
}

fn prepare_runtime_launch(options: RuntimeLaunchOptions) -> Result<(), String> {
    setup::write_cursor_mcp_config(options.any_mcp())
}

pub(crate) const INTEGRATION: Integration =
    Integration::new(Some(setup::install_cursor_hooks), None)
        .with_startup_command(prepare_startup_command)
        .with_automatic_mcp_setup(has_automatic_mcp_setup)
        .with_runtime_launch_preparation(prepare_runtime_launch)
        .with_resume_adapter(resume::ADAPTER);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_command_adds_force_and_approve_mcps() {
        assert_eq!(
            startup_command("cursor-agent", true),
            "cursor-agent --force --approve-mcps"
        );
        assert_eq!(
            startup_command("cursor-agent --force", true),
            "cursor-agent --force --approve-mcps"
        );
        assert_eq!(
            startup_command("cursor-agent --force", false),
            "cursor-agent --force"
        );
    }

    #[test]
    fn startup_command_preserves_existing_resume_flags() {
        assert_eq!(
            startup_command("cursor-agent --resume chat-1", true),
            "cursor-agent --resume chat-1 --force --approve-mcps"
        );
    }
}
