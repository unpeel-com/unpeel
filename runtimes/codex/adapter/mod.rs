use super::{shared, Integration, RuntimeLaunchOptions};
use crate::session_host::SessionHostLaunch;
use crate::setup as command_setup;
use portable_pty::CommandBuilder;
use std::path::PathBuf;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/codex/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/codex/adapter/setup.rs"
    ));
}

pub(crate) fn startup_command(command: &str) -> String {
    let trimmed = command.trim();
    if !super::shared::command_head(trimmed).eq_ignore_ascii_case("codex") {
        return trimmed.to_string();
    }

    let head_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let rest = &trimmed[head_end..];
    let wrapper = setup::wrapper_bin_dir().join("codex");
    format!(
        "{}{}",
        shared::shell_quote(&wrapper.to_string_lossy()),
        rest
    )
}

fn real_codex_search_dirs(wrapper_dir: &PathBuf) -> Vec<PathBuf> {
    filter_wrapper_dir(command_setup::search_dirs(), wrapper_dir)
}

fn filter_wrapper_dir(dirs: Vec<PathBuf>, wrapper_dir: &PathBuf) -> Vec<PathBuf> {
    let canonical_wrapper = wrapper_dir.canonicalize().ok();
    dirs.into_iter()
        .filter(|dir| {
            if dir == wrapper_dir {
                return false;
            }
            canonical_wrapper
                .as_ref()
                .zip(dir.canonicalize().ok().as_ref())
                .is_none_or(|(wrapper, candidate)| wrapper != candidate)
        })
        .collect()
}

fn joined_path(dirs: &[PathBuf]) -> String {
    std::env::join_paths(dirs)
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|_| {
            dirs.iter()
                .map(|dir| dir.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(":")
        })
}

fn configure_host_command(
    launch: &SessionHostLaunch,
    cmd: &mut CommandBuilder,
    shell_prelude: &mut Vec<String>,
) -> Result<(), String> {

    // Use the resolved shell PATH (includes nvm, rbenv, pyenv, etc.) instead
    // of the bare process PATH which is incomplete in GUI apps on macOS. The
    // shell may already contain Unpeel's wrapper directory from an earlier
    // session, so remove it before resolving the real binary or exporting the
    // fallback path. Otherwise the wrapper recursively execs itself.
    let wrapper_dir = setup::wrapper_bin_dir();
    let search_dirs = real_codex_search_dirs(&wrapper_dir);
    let original_path = joined_path(&search_dirs);
    cmd.env("UNPEEL_ORIGINAL_PATH", &original_path);

    if let Some(real_bin) = command_setup::find_command_path("codex", &search_dirs) {
        cmd.env("UNPEEL_REAL_CODEX_BIN", &real_bin);
        shell_prelude.push(format!(
            "export UNPEEL_REAL_CODEX_BIN={}",
            shared::shell_quote(&real_bin)
        ));
    }

    if launch.wait_for_attach {
        let session_dir = crate::session_host::session_dir(&launch.session.id);
        let session_dir = session_dir.to_string_lossy().to_string();
        cmd.env("UNPEEL_WAIT_FOR_ATTACH", "1");
        cmd.env("UNPEEL_SESSION_DIR", &session_dir);
        shell_prelude.push(format!(
            "export UNPEEL_WAIT_FOR_ATTACH=1 UNPEEL_SESSION_DIR={}",
            shared::shell_quote(&session_dir)
        ));
    }

    // The wrapper registers the unified Unpeel MCP server with this binary
    // (`unpeel-host __mcp__`) via `-c mcp_servers.unpeel.*` overrides when any
    // domain is enabled; the server advertises only the domains recorded in
    // this session's manifest. Without the env var the wrapper skips the
    // registration entirely.
    if launch.mcp_enabled || launch.browser_mcp_enabled || launch.computer_mcp_enabled {
        if let Ok(mcp_bin) = crate::session_host::resolve_current_executable() {
            let mcp_bin = mcp_bin.to_string_lossy().to_string();
            cmd.env("UNPEEL_MCP_BIN", &mcp_bin);
            shell_prelude.push(format!(
                "export UNPEEL_MCP_BIN={}",
                shared::shell_quote(&mcp_bin)
            ));
        }
    }

    let wrapped_path = if original_path.is_empty() {
        wrapper_dir.to_string_lossy().to_string()
    } else {
        let mut dirs = vec![wrapper_dir];
        dirs.extend(std::env::split_paths(&original_path));
        std::env::join_paths(dirs)
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|_| format!("{}:{}", setup::wrapper_bin_dir().display(), original_path))
    };
    cmd.env("PATH", &wrapped_path);
    shell_prelude.push(format!(
        "export PATH={}",
        shared::shell_quote(&wrapped_path)
    ));
    shell_prelude.push(format!(
        "export UNPEEL_ORIGINAL_PATH={}",
        shared::shell_quote(&original_path)
    ));
    Ok(())
}

fn prepare_startup_command(command: &str, _options: RuntimeLaunchOptions) -> String {
    startup_command(command)
}

fn has_automatic_mcp_setup(_command: &str) -> bool {
    true
}

pub(crate) const INTEGRATION: Integration = Integration::new(
    Some(setup::install_codex_wrapper),
    Some(configure_host_command),
)
.with_startup_command(prepare_startup_command)
.with_automatic_mcp_setup(has_automatic_mcp_setup)
    .with_resume_adapter(resume::ADAPTER);

#[cfg(test)]
mod tests {
    use super::filter_wrapper_dir;
    use super::joined_path;
    use std::path::PathBuf;

    #[test]
    fn joined_path_preserves_search_order() {
        let dirs = vec![PathBuf::from("/first/bin"), PathBuf::from("/second/bin")];
        assert_eq!(joined_path(&dirs), "/first/bin:/second/bin");
    }

    #[test]
    fn wrapper_directory_is_removed_from_real_codex_search_path() {
        let wrapper = PathBuf::from("/Users/example/.unpeel/hooks/bin");
        let real = PathBuf::from("/opt/homebrew/bin");
        let filtered = filter_wrapper_dir(vec![wrapper.clone(), real.clone()], &wrapper);
        assert_eq!(filtered, vec![real]);
    }
}
