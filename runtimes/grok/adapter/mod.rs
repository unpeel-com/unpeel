use super::{shared, Integration, RuntimeLaunchOptions};
use crate::session_host::SessionHostLaunch;
use crate::setup as command_setup;
use portable_pty::CommandBuilder;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/grok/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/grok/adapter/setup.rs"
    ));
}

pub(crate) fn startup_command(command: &str) -> String {
    let trimmed = command.trim();
    if !super::shared::command_head(trimmed).eq_ignore_ascii_case("grok") {
        return trimmed.to_string();
    }

    let head_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let rest = &trimmed[head_end..];
    let wrapper = setup::grok_appearance_bin_dir().join("grok");
    format!(
        "{}{}",
        shared::shell_quote(&wrapper.to_string_lossy()),
        rest
    )
}

fn configure_host_command(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {
    let bin_dir = setup::grok_appearance_bin_dir();
    let bin_dir_str = bin_dir.to_string_lossy().to_string();
    let existing_path = std::env::var("PATH").unwrap_or_default();
    let path = if existing_path.is_empty() {
        bin_dir_str.clone()
    } else {
        format!("{bin_dir_str}:{existing_path}")
    };
    let appearance_file = setup::app_appearance_path().to_string_lossy().to_string();
    let launch_appearance = match launch.dark_mode {
        Some(false) => "light",
        Some(true) | None => "dark",
    };

    cmd.env("PATH", &path);
    cmd.env("UNPEEL_APP_APPEARANCE_FILE", &appearance_file);
    cmd.env("UNPEEL_GROK_APP_APPEARANCE", launch_appearance);
    // Belt-and-suspenders with the grok wrapper overlay: Grok still scans
    // ~/.claude/settings.json unless these cells are off, and those hooks
    // fail-open as a red "required env var(s) not set" line when they
    // interpolate an unset $VAR.
    cmd.env("GROK_CLAUDE_HOOKS_ENABLED", "false");
    cmd.env("GROK_CURSOR_HOOKS_ENABLED", "false");
    if let Some(real_bin) = command_setup::find_command_path("grok", &command_setup::search_dirs())
    {
        cmd.env("UNPEEL_REAL_GROK_BIN", &real_bin);
        shell_prelude.push(format!(
            "export UNPEEL_REAL_GROK_BIN={}",
            shared::shell_quote(&real_bin)
        ));
    }
    shell_prelude.push(format!(
        "export PATH={} UNPEEL_APP_APPEARANCE_FILE={} UNPEEL_GROK_APP_APPEARANCE={} GROK_CLAUDE_HOOKS_ENABLED=false GROK_CURSOR_HOOKS_ENABLED=false",
        shared::shell_quote(&path),
        shared::shell_quote(&appearance_file),
        shared::shell_quote(launch_appearance),
    ));
    Ok(())
}

fn prepare_startup_command(command: &str, _options: RuntimeLaunchOptions) -> String {
    startup_command(command)
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_grok_hooks),
    Some(configure_host_command),
)
.with_startup_command(prepare_startup_command)
.with_resume_adapter(resume::ADAPTER);

#[cfg(test)]
mod tests {
    use super::configure_host_command;
    use portable_pty::CommandBuilder;

    #[test]
    fn disables_claude_and_cursor_compat_hooks_in_hosted_env() {
        let launch: super::SessionHostLaunch = serde_json::from_value(serde_json::json!({
            "session": {
                "id": "grok-session",
                "project_id": "test-project",
                "label": "Grok",
                "command": "grok --always-approve"
            },
            "cwd": "/tmp",
            "dark_mode": true,
            "hook_port": 4321
        }))
        .expect("launch fixture");
        let mut command = CommandBuilder::new("true");
        let mut prelude = Vec::new();
        configure_host_command(&launch, &mut command, &mut prelude).expect("configure");
        let prelude = prelude.join("\n");
        assert!(
            prelude.contains("GROK_CLAUDE_HOOKS_ENABLED=false"),
            "{prelude}"
        );
        assert!(
            prelude.contains("GROK_CURSOR_HOOKS_ENABLED=false"),
            "{prelude}"
        );
    }
}
