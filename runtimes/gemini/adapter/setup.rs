use crate::app_paths::unpeel_home;
use crate::hook_assets::{read_mergeable_json_object, write_executable_script, write_file_atomic};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const GEMINI_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/gemini/assets/hooks/lifecycle.sh"
));

pub fn install_gemini_hooks() -> Result<(), String> {
    let script_path = gemini_hook_script_path();
    write_executable_script(&script_path, GEMINI_HOOK_SCRIPT, "Gemini hook script")?;
    ensure_gemini_settings_hook(&script_path)?;
    Ok(())
}
pub(crate) fn gemini_hook_script_path() -> PathBuf {
    unpeel_home().join("hooks").join("gemini-hook.sh")
}

pub(crate) fn gemini_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".gemini").join("settings.json"))
}
pub(crate) fn ensure_gemini_settings_hook(script_path: &Path) -> Result<(), String> {
    let Some(settings_path) = gemini_settings_path() else {
        return Ok(());
    };
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create Gemini settings dir {}: {e}",
                parent.display()
            )
        })?;
    }

    let Some(mut settings) = read_mergeable_json_object(&settings_path, "Gemini settings")? else {
        return Ok(());
    };

    let command = script_path.to_string_lossy().to_string();
    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks_obj = hooks.as_object_mut().unwrap();

    let mut changed = false;
    for event_name in ["BeforeAgent", "AfterAgent", "AfterTool", "Notification"] {
        let entries = hooks_obj
            .entry(event_name.to_string())
            .or_insert_with(|| json!([]));
        if !entries.is_array() {
            *entries = json!([]);
        }
        let array = entries.as_array_mut().unwrap();
        let already_installed = array.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(|value| value.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command").and_then(|value| value.as_str())
                            == Some(command.as_str())
                    })
                })
        });
        if !already_installed {
            array.push(json!({
                "hooks": [{
                    "type": "command",
                    "command": command
                }]
            }));
            changed = true;
        }
    }

    if changed {
        let json = serde_json::to_string_pretty(&settings)
            .map_err(|e| format!("Failed to serialize Gemini settings: {e}"))?;
        write_file_atomic(&settings_path, &format!("{json}\n"), "Gemini settings")?;
    }

    Ok(())
}
