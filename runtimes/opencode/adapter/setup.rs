use crate::app_paths::unpeel_home;
use crate::hook_assets::{notify_hook_script_path, write_executable_script, NOTIFY_HOOK_SCRIPT};
use std::fs;
use std::path::PathBuf;

pub(crate) const OPENCODE_PLUGIN_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/opencode/assets/hooks/plugin.js"
));

pub fn install_opencode_plugin() -> Result<(), String> {
    let notify_path = notify_hook_script_path();
    write_executable_script(&notify_path, NOTIFY_HOOK_SCRIPT, "notify hook script")?;

    let plugin_dir = opencode_plugin_dir();
    fs::create_dir_all(&plugin_dir).map_err(|e| {
        format!(
            "Failed to create OpenCode plugin dir {}: {e}",
            plugin_dir.display()
        )
    })?;
    let plugin =
        OPENCODE_PLUGIN_SCRIPT.replace("{{NOTIFY_PATH}}", notify_path.to_string_lossy().as_ref());
    fs::write(opencode_plugin_path(), plugin)
        .map_err(|e| format!("Failed to write OpenCode plugin: {e}"))?;
    Ok(())
}
pub fn opencode_config_dir() -> PathBuf {
    unpeel_home().join("hooks").join("opencode")
}
pub(crate) fn opencode_plugin_dir() -> PathBuf {
    opencode_config_dir().join("plugin")
}

pub(crate) fn opencode_plugin_path() -> PathBuf {
    opencode_plugin_dir().join("unpeel-notify.js")
}
