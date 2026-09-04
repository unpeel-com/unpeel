use crate::app_paths::unpeel_home;
use crate::hook_assets::write_executable_script;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const CLINE_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/cline/assets/hooks/lifecycle.sh"
));

pub(crate) const CLINE_HOOK_EVENTS: &[&str] = &[
    "TaskStart",
    "TaskResume",
    "TaskCancel",
    "TaskComplete",
    "TaskError",
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "SessionShutdown",
];
pub fn install_cline_hooks() -> Result<(), String> {
    let script_path = cline_hook_script_path();
    write_executable_script(&script_path, CLINE_HOOK_SCRIPT, "Cline hook script")?;

    let hooks_dir = cline_home_dir().join("hooks");
    fs::create_dir_all(&hooks_dir).map_err(|e| {
        format!(
            "Failed to create Cline hooks dir {}: {e}",
            hooks_dir.display()
        )
    })?;
    let quoted_script = crate::integrations::shared::shell_quote(&script_path.to_string_lossy());
    for event in CLINE_HOOK_EVENTS {
        let contents = format!(
            "#!/bin/bash\n# Managed by Unpeel. Local edits are replaced.\nexec {quoted_script} {event}\n"
        );
        write_cline_event_hook(&hooks_dir, event, &contents)?;
    }
    Ok(())
}

pub(crate) fn write_cline_event_hook(
    hooks_dir: &Path,
    event: &str,
    contents: &str,
) -> Result<(), String> {
    const MANAGED_MARKER: &str = "# Managed by Unpeel.";
    // Cline recognizes every one of these as the same event basename and runs
    // multiple matching files. Prefer `.bash`, but never overwrite a user's
    // hook: reuse our existing slot or take the next unoccupied extension.
    let candidates = [
        hooks_dir.join(format!("{event}.bash")),
        hooks_dir.join(format!("{event}.zsh")),
        hooks_dir.join(format!("{event}.sh")),
        hooks_dir.join(event),
    ];

    let managed = candidates.iter().find(|path| {
        fs::read_to_string(path)
            .map(|value| value.contains(MANAGED_MARKER))
            .unwrap_or(false)
    });
    let target = managed
        .cloned()
        .or_else(|| candidates.iter().find(|path| !path.exists()).cloned())
        .ok_or_else(|| {
            format!(
                "Cline already has user-owned hooks in every supported slot for {event}; \
                 Unpeel left them untouched."
            )
        })?;
    write_executable_script(&target, contents, "Cline lifecycle hook")
}
pub(crate) fn cline_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("cline-hook.sh")
}

pub fn cline_home_dir() -> PathBuf {
    let path = std::env::var_os("CLINE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".cline")
        });
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}
