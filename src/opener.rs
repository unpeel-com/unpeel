//! Open a resource the way the user's workspace policy says: an App beside
//! this pane, their editor, or the system opener.
//!
//! The policy (Settings ▸ Open resources) lives on the Host. An App never
//! guesses it: it asks `unpeel open <path> --resolve`, then acts — an App
//! through `unpeel open` (which creates or reuses the companion pane beside
//! this Session), the editor through the same bridge the "Open in editor"
//! action uses, the system through the platform opener. Standalone (no
//! Host), everything falls back to the editor bridge.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::agent::{is_hosted, AgentError};
use crate::editor::open_in_editor;

/// How a resource ended up being opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Opened in a companion App pane beside this Session (the App's name).
    App(String),
    /// Handed to the user's editor.
    Editor,
    /// Handed to the platform's default handler.
    System,
}

/// Open `path` according to the workspace's opener policy.
pub fn open_resource(path: impl AsRef<Path>) -> Result<OpenOutcome, AgentError> {
    let path = absolute(path.as_ref());
    if !is_hosted() {
        return open_in_editor(&path)
            .map(|()| OpenOutcome::Editor)
            .map_err(|error| AgentError::new(error.to_string()));
    }
    let resolution = run_unpeel(&["open", &path.to_string_lossy(), "--resolve", "--json"])?;
    let opener = resolution
        .get("opener")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    match opener.as_str() {
        "editor" => open_in_editor(&path)
            .map(|()| OpenOutcome::Editor)
            .map_err(|error| AgentError::new(error.to_string())),
        "system" => open_with_system(&path).map(|()| OpenOutcome::System),
        app if app.starts_with("app:") => {
            let installed = resolution
                .pointer("/app/installed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let name = resolution
                .pointer("/app/name")
                .and_then(Value::as_str)
                .unwrap_or(app.trim_start_matches("app:"))
                .to_owned();
            if !installed {
                return Err(AgentError::new(format!(
                    "{name} is not installed — add it in Settings ▸ Open resources"
                )));
            }
            let receipt = run_unpeel(&["open", &path.to_string_lossy(), "--json"])?;
            let name = receipt
                .pointer("/app/name")
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_owned();
            Ok(OpenOutcome::App(name))
        }
        other => Err(AgentError::new(format!("no opener for this file ({other:?})"))),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// The `unpeel` CLI that belongs to the Host running this App: it ships
/// next to the `unpeel-host` the Host advertises (`UNPEEL_HOST_BIN`), so a
/// stale CLI elsewhere on PATH never answers for a newer Host.
fn unpeel_cli() -> PathBuf {
    std::env::var_os("UNPEEL_HOST_BIN")
        .map(PathBuf::from)
        .and_then(|host| host.parent().map(|dir| dir.join("unpeel")))
        .filter(|cli| cli.is_file())
        .unwrap_or_else(|| PathBuf::from("unpeel"))
}

fn run_unpeel(args: &[&str]) -> Result<Value, AgentError> {
    let output = Command::new(unpeel_cli())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| AgentError::new(format!("unpeel not available: {error}")))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(AgentError::new(if message.is_empty() {
            "unpeel open failed".to_owned()
        } else {
            message
        }));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| AgentError::new(format!("unpeel open reply was not JSON: {error}")))
}

fn open_with_system(path: &Path) -> Result<(), AgentError> {
    let command = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(command)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| AgentError::new(format!("run {command}: {error}")))
}
