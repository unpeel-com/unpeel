//! Best-effort local App registration. The explorer remains fully usable
//! without Unpeel; when an Unpeel home already exists, one manifest lets the
//! host recognize and tint a hand-launched development session.

use std::path::PathBuf;

pub const APP_ID: &str = "unpeel.app.filetree";

const APP_TOML: &str = r##"# Installed by unpeel-filetree; this is a development harness.
manifest_version = 1
id = "unpeel.app.filetree"
name = "Explorer Lab"
version = "@APP_VERSION@"
command = "@LAUNCH_COMMAND@"
description = "Development-only borderless explorer for dragging Host-local file and folder paths into another Unpeel terminal"

[detection]
command_aliases = ["unpeel-filetree"]
process_aliases = ["unpeel-filetree"]

[display]
tint = "#D97706"

[views]
terminal = true
media_types = ["inode/directory"]
"##;

fn unpeel_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("UNPEEL_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".unpeel"))
}

fn launch_command() -> String {
    const NAME: &str = "unpeel-filetree";
    let on_path = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(NAME).is_file()));
    if on_path {
        return NAME.to_string();
    }
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| NAME.to_string())
}

fn toml_escaped(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn ensure_installed() {
    let Some(home) = unpeel_home().filter(|home| home.is_dir()) else {
        return;
    };
    let manifest = APP_TOML
        .replace("@APP_VERSION@", env!("CARGO_PKG_VERSION"))
        .replace("@LAUNCH_COMMAND@", &toml_escaped(&launch_command()));
    let directory = home.join("apps").join(APP_ID);
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = directory.join("app.toml");
    if std::fs::read_to_string(&path).ok().as_deref() == Some(&manifest) {
        return;
    }
    let temporary = directory.join(format!(".app.toml.{}.tmp", std::process::id()));
    if std::fs::write(&temporary, manifest).is_ok() {
        let _ = std::fs::rename(temporary, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_explicitly_a_development_path_drag_harness() {
        assert!(APP_TOML.contains("id = \"unpeel.app.filetree\""));
        assert!(APP_TOML.contains("version = \"@APP_VERSION@\""));
        assert!(APP_TOML.contains("development harness"));
        assert!(APP_TOML.contains("inode/directory"));
        assert!(APP_TOML.contains("command_aliases = [\"unpeel-filetree\"]"));
    }

    #[test]
    fn launch_commands_are_toml_escaped() {
        assert_eq!(
            toml_escaped(r#"C:\\Apps\"Filetree\""#),
            r#"C:\\\\Apps\\\"Filetree\\\""#
        );
    }
}
