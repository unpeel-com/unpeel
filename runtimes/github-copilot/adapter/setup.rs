use crate::app_paths::unpeel_home;
use crate::hook_assets::{
    ensure_project_exclude_entry, write_executable_script, write_project_file_no_symlinks,
};
use serde_json::json;
use std::path::{Path, PathBuf};

pub(crate) const COPILOT_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/github-copilot/assets/hooks/lifecycle.sh"
));

pub fn install_copilot_hook() -> Result<(), String> {
    let script_path = copilot_hook_script_path();
    write_executable_script(&script_path, COPILOT_HOOK_SCRIPT, "Copilot hook script")?;
    Ok(())
}
pub fn prepare_copilot_project_hooks(cwd: &str) -> Result<(), String> {
    let hook_script_path = copilot_hook_script_path();
    let hook_json = json!({
        "version": 1,
        "hooks": {
            "sessionStart": [{ "type": "command", "bash": format!("{} sessionStart", hook_script_path.display()), "timeoutSec": 5 }],
            "sessionEnd": [{ "type": "command", "bash": format!("{} sessionEnd", hook_script_path.display()), "timeoutSec": 5 }],
            "userPromptSubmitted": [{ "type": "command", "bash": format!("{} userPromptSubmitted", hook_script_path.display()), "timeoutSec": 5 }],
            "postToolUse": [{ "type": "command", "bash": format!("{} postToolUse", hook_script_path.display()), "timeoutSec": 5 }]
        }
    });
    let serialized = serde_json::to_string_pretty(&hook_json)
        .map_err(|e| format!("Failed to serialize Copilot hook config: {e}"))?;
    write_project_file_no_symlinks(
        Path::new(cwd),
        Path::new(".github/hooks/unpeel-notify.json"),
        format!("{serialized}\n").as_bytes(),
    )?;

    ensure_project_exclude_entry(cwd, ".github/hooks/unpeel-notify.json");

    Ok(())
}
pub(crate) fn copilot_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("copilot-hook.sh")
}
