//! Best-effort local App registration. The viewer remains fully usable
//! without Unpeel and does not add diff chrome to the core product.

use std::path::PathBuf;

pub const APP_ID: &str = "unpeel.app.diffs";

const APP_TOML: &str = r##"# Installed by unpeel-diffs; this is a standalone App.
manifest_version = 1
id = "unpeel.app.diffs"
name = "Diffs"
version = "@APP_VERSION@"
command = "@LAUNCH_COMMAND@"
description = "Standalone borderless Git working-tree viewer with a changed-file list and diff detail"

[detection]
command_aliases = ["unpeel-diffs"]
process_aliases = ["unpeel-diffs"]

[display]
tint = "#64748B"

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
    const NAME: &str = "unpeel-diffs";
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
    fn manifest_describes_a_standalone_terminal_app() {
        assert!(APP_TOML.contains("id = \"unpeel.app.diffs\""));
        assert!(APP_TOML.contains("version = \"@APP_VERSION@\""));
        assert!(APP_TOML.contains("standalone App"));
        assert!(APP_TOML.contains("command_aliases = [\"unpeel-diffs\"]"));
    }

    #[test]
    fn launch_commands_are_toml_escaped() {
        assert_eq!(
            toml_escaped(r#"C:\\Apps\"Diffs\""#),
            r#"C:\\\\Apps\\\"Diffs\\\""#
        );
    }
}
