use crate::hook_assets::{
    ensure_project_exclude_entry, notify_hook_script_path, write_executable_script,
    write_project_file_no_symlinks, NOTIFY_HOOK_SCRIPT,
};
use std::path::Path;

pub(crate) const AMP_PLUGIN_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/amp/assets/hooks/plugin.js"
));

pub fn install_amp_plugin() -> Result<(), String> {
    let notify_path = notify_hook_script_path();
    write_executable_script(&notify_path, NOTIFY_HOOK_SCRIPT, "notify hook script")?;
    Ok(())
}
pub fn prepare_amp_project_plugin(cwd: &str) -> Result<(), String> {
    let plugin = AMP_PLUGIN_SCRIPT.replace(
        "{{NOTIFY_PATH}}",
        notify_hook_script_path().to_string_lossy().as_ref(),
    );
    write_project_file_no_symlinks(
        Path::new(cwd),
        Path::new(".amp/plugins/unpeel-notify.js"),
        plugin.as_bytes(),
    )?;
    ensure_project_exclude_entry(cwd, ".amp/plugins/unpeel-notify.js");
    Ok(())
}
