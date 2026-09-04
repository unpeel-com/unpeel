use crate::app_paths::unpeel_home;
use crate::hook_assets::{append_trace_log_line, write_executable_script, write_file_atomic};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const MUSE_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../runtimes/muse-code/assets/hooks/lifecycle.sh"
));

pub fn install_muse_hooks() -> Result<(), String> {
    let plugin_dir = muse_plugin_dir();
    let hooks_dir = plugin_dir.join("hooks");
    let manifest_dir = plugin_dir.join(".muse-plugin");
    fs::create_dir_all(&hooks_dir).map_err(|e| {
        format!(
            "Failed to create Muse hooks dir {}: {e}",
            hooks_dir.display()
        )
    })?;
    fs::create_dir_all(&manifest_dir).map_err(|e| {
        format!(
            "Failed to create Muse plugin manifest dir {}: {e}",
            manifest_dir.display()
        )
    })?;

    for (_, file) in MUSE_HOOK_EVENT_FILES {
        write_executable_script(&hooks_dir.join(file), MUSE_HOOK_SCRIPT, "Muse hook script")?;
    }

    let manifest = muse_plugin_manifest_json()?;
    write_file_atomic(
        &manifest_dir.join("plugin.json"),
        &manifest,
        "Muse plugin manifest",
    )?;

    ensure_muse_plugin_registered(&plugin_dir, &manifest)
}

pub(crate) const MUSE_PLUGIN_ID: &str = "unpeel";

pub(crate) const MUSE_HOOK_EVENT_FILES: &[(&str, &str)] = &[
    ("SessionStart", "session-start.sh"),
    ("UserPromptSubmit", "user-prompt-submit.sh"),
    ("Stop", "stop.sh"),
    ("PermissionRequest", "permission-request.sh"),
];

pub(crate) fn muse_plugin_manifest_json() -> Result<String, String> {
    let hooks: Vec<Value> = MUSE_HOOK_EVENT_FILES
        .iter()
        .map(|(event, file)| {
            json!({
                "id": format!("unpeel-{}", file.trim_end_matches(".sh")),
                "event": event,
                "command": ["sh", format!("hooks/{file}")],
                "timeoutMs": 5000,
            })
        })
        .collect();
    let manifest = json!({
        "schemaVersion": 1,
        "name": MUSE_PLUGIN_ID,
        "displayName": "Unpeel",
        "version": "0.1.0",
        "description": "Forwards Muse Code lifecycle events to the Unpeel app.",
        "compat": { "source": "native", "manifestDir": ".muse-plugin" },
        "capabilities": {
            "skills": [],
            "commands": [],
            "hooks": hooks,
            "mcpServers": [],
            "reminders": []
        }
    });
    serde_json::to_string_pretty(&manifest)
        .map(|serialized| format!("{serialized}\n"))
        .map_err(|e| format!("Failed to serialize Muse plugin manifest: {e}"))
}

pub(crate) fn ensure_muse_plugin_registered(
    plugin_dir: &Path,
    manifest: &str,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(manifest.as_bytes());
    hasher.update(MUSE_HOOK_SCRIPT.as_bytes());
    let digest = format!("{:x}", hasher.finalize());

    // Skip the muse CLI round-trip when the exact staged content was already
    // installed AND muse's own lockfile still lists the plugin (the user may
    // have run `muse plugins remove`, which must trigger a re-install).
    let marker = muse_plugin_marker_path();
    let lockfile_has_plugin = muse_data_dir()
        .map(|dir| dir.join("plugins").join("installed.json"))
        .filter(|path| path.is_file())
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|raw| raw.contains(&format!("\"{MUSE_PLUGIN_ID}\"")))
        .unwrap_or(false);
    if lockfile_has_plugin
        && fs::read_to_string(&marker)
            .map(|recorded| recorded.trim() == digest)
            .unwrap_or(false)
    {
        return Ok(());
    }

    let Some(muse_bin) = crate::setup::find_command_path("muse", &crate::setup::search_dirs())
    else {
        // No muse on this machine yet; the staged package registers on the
        // first launch after it appears.
        return Ok(());
    };

    let dir = plugin_dir.to_string_lossy().to_string();
    let install_args = ["plugins", "install", dir.as_str(), "--json"];
    if let Err(first) = run_muse_plugins_command(&muse_bin, &install_args) {
        // Muse refuses to re-install an id it already has (including "from a
        // different source", which happens when another UNPEEL_HOME instance
        // registered its staged copy — the content is byte-identical, only
        // the recorded source path differs). Re-point it by removing and
        // installing fresh.
        if !first.contains("already installed") {
            return Err(first);
        }
        let _ =
            run_muse_plugins_command(&muse_bin, &["plugins", "remove", MUSE_PLUGIN_ID, "--json"]);
        run_muse_plugins_command(&muse_bin, &install_args)?;
    }
    run_muse_plugins_command(&muse_bin, &["plugins", "approve", MUSE_PLUGIN_ID, "--json"])?;
    let _ = fs::write(&marker, &digest);
    append_trace_log_line(&format!("muse-plugin installed digest={}", &digest[..12]));
    Ok(())
}

pub(crate) fn run_muse_plugins_command(muse_bin: &str, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(muse_bin)
        .args(args)
        .env("MUSE_EXPERIMENTAL_PLUGINS", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run muse {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "muse {} failed: {} {}",
            args.join(" "),
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(())
}
pub fn muse_plugin_dir() -> PathBuf {
    unpeel_home().join("hooks").join("muse-plugin")
}

pub(crate) fn muse_plugin_marker_path() -> PathBuf {
    unpeel_home().join("hooks").join("muse-plugin.installed")
}

/// Muse Code's data dir (`$XDG_DATA_HOME/muse`, default
/// `~/.local/share/muse`): the plugin lockfile lives at
/// `<data>/plugins/installed.json`.
pub(crate) fn muse_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir).join("muse"));
    }
    dirs::home_dir().map(|home| home.join(".local").join("share").join("muse"))
}
