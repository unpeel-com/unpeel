use crate::app_paths::unpeel_home;
use crate::hook_assets::write_executable_script;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const GROK_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/grok/assets/hooks/lifecycle.sh"
));

pub(crate) const GROK_DEFAULTS_WRAPPER_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/grok/assets/hooks/defaults-wrapper.sh"
));

pub(crate) const GROK_COMMAND_WRAPPER_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/grok/assets/hooks/command-wrapper.sh"
));

pub fn install_grok_hooks() -> Result<(), String> {
    // Grok-native hooks map argv[1] -> Unpeel lifecycle events and POST to the
    // hook port. SessionStart only latches provider metadata; UserPromptSubmit
    // is the turn-opening busy event. Grok also scans Claude/Cursor hook files;
    // those Unpeel scripts no-op when GROK_SESSION_ID is set so a Claude-shaped
    // session_start cannot latch busy. Hosted Grok also disables
    // [compat.claude]/[compat.cursor] hooks (overlay + GROK_*_HOOKS_ENABLED)
    // so vendor commands that interpolate unset $VARs do not fail as a red
    // session-start error. Real attention comes from Notification/PreToolUse
    // in unpeel.json.
    let script_path = grok_hook_script_path();
    write_executable_script(&script_path, GROK_HOOK_SCRIPT, "Grok hook script")?;
    write_executable_script(
        &grok_defaults_wrapper_path(),
        GROK_DEFAULTS_WRAPPER_SCRIPT,
        "Grok defaults wrapper",
    )?;
    write_executable_script(
        &grok_command_wrapper_path(),
        GROK_COMMAND_WRAPPER_SCRIPT,
        "Grok command wrapper",
    )?;
    ensure_grok_hooks(&script_path)?;
    Ok(())
}

pub(crate) fn grok_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("grok-hook.sh")
}

pub fn grok_appearance_bin_dir() -> PathBuf {
    unpeel_home().join("hooks").join("grok-bin")
}

pub fn app_appearance_path() -> PathBuf {
    unpeel_home().join("app-appearance")
}

pub(crate) fn grok_defaults_wrapper_path() -> PathBuf {
    grok_appearance_bin_dir().join("defaults")
}

pub(crate) fn grok_command_wrapper_path() -> PathBuf {
    grok_appearance_bin_dir().join("grok")
}
pub(crate) fn grok_hooks_path() -> Option<PathBuf> {
    // Grok merges every `*.json` under ~/.grok/hooks/ (global hooks are always
    // trusted). We own `unpeel.json`, so it can be rewritten wholesale.
    dirs::home_dir().map(|home| home.join(".grok").join("hooks").join("unpeel.json"))
}
pub(crate) fn ensure_grok_hooks(script_path: &Path) -> Result<(), String> {
    let Some(hooks_path) = grok_hooks_path() else {
        return Ok(());
    };
    if let Some(parent) = hooks_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create Grok hooks dir {}: {e}", parent.display()))?;
    }

    let json = grok_hooks_json(script_path)?;
    fs::write(&hooks_path, format!("{json}\n")).map_err(|e| {
        format!(
            "Failed to write Grok hooks file {}: {e}",
            hooks_path.display()
        )
    })?;

    Ok(())
}

pub(crate) fn grok_hooks_json(script_path: &Path) -> Result<String, String> {
    let command = script_path.to_string_lossy();
    // Grok-native event names (10-hooks.md): SessionStart means the interactive
    // CLI opened, not that the agent is working, so it only latches provider
    // metadata. UserPromptSubmit drives busy; turn/session end events drive
    // idle. Attention is wired from
    // approval_required notifications and ask_user_question PreToolUse only —
    // not generic Notification/Stop pairs (those stuck sessions yellow) and
    // not Cursor-compat beforeShellExecution PermissionRequest (auto-approved
    // noise under --always-approve).
    let attention_command = format!("{command} Attention");
    let hooks_json = json!({
        "hooks": {
            "SessionStart": [
                { "hooks": [ { "type": "command", "command": format!("{command} HookSeen") } ] }
            ],
            "UserPromptSubmit": [
                { "hooks": [ { "type": "command", "command": format!("{command} UserPromptSubmit") } ] }
            ],
            "Stop": [
                { "hooks": [ { "type": "command", "command": format!("{command} Stop") } ] }
            ],
            "StopFailure": [
                { "hooks": [ { "type": "command", "command": format!("{command} Stop") } ] }
            ],
            "SessionEnd": [
                { "hooks": [ { "type": "command", "command": format!("{command} Stop") } ] }
            ],
            "Notification": [
                {
                    "matcher": "approval_required",
                    "hooks": [
                        { "type": "command", "command": attention_command.clone() }
                    ]
                }
            ],
            "PreToolUse": [
                {
                    "matcher": "ask_user_question|AskUserQuestion",
                    "hooks": [
                        { "type": "command", "command": attention_command }
                    ]
                }
            ]
        }
    });

    serde_json::to_string_pretty(&hooks_json)
        .map_err(|e| format!("Failed to serialize Grok hooks file: {e}"))
}
