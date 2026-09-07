mod app;
mod backend;
mod block;
mod clipboard;
mod format;
mod heading;
mod highlight;
mod install;
mod mouse;
mod picker;
mod slash;
mod start;
mod theme;

use std::path::{Path, PathBuf};

use app::App;
use backend::BackendCapture;
use picker::Picker;
use theme::Theme;
use unpeel_app_kit::{AppContext, AppMetadata, AppReporter, KeyboardEnhancementGuard, UiBridge};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    match std::env::args_os().nth(1).as_deref() {
        Some(argument) if argument == "--help" || argument == "-h" => {
            println!("Usage: unpeel-markdown [FILE|FOLDER]");
            return Ok(());
        }
        Some(argument) if argument == "--version" => {
            println!("unpeel-markdown {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    let mut status = AppReporter::detect(install::APP_ID);
    status.idle();
    let explicit_path = std::env::args().nth(1).map(PathBuf::from);
    let app_context = AppContext::detect();
    let theme = Theme::detect();
    let mut ui_revision = 0u64;
    let mut ui_bridge = UiBridge::detect(
        AppMetadata::new(
            install::APP_ID,
            "Unpeel Markdown",
            env!("CARGO_PKG_VERSION"),
        )
        .description(
            "One Markdown component tree interpreted by Ratatui, native, and web renderers",
        ),
    )?;
    ratatui::run(|terminal| {
        let _keyboard = KeyboardEnhancementGuard::enter()?;
        let _capture = BackendCapture::enable(terminal)?;
        let path = match explicit_path.clone() {
            Some(path) => path,
            None => match start::read_workspace(install::APP_ID) {
                Some(path) => path,
                None => {
                    let Some(path) = start::choose_workspace(
                        terminal,
                        theme,
                        app_context.current_root(),
                        &mut ui_bridge,
                        &mut ui_revision,
                    )?
                    else {
                        return Ok(());
                    };
                    start::write_workspace(install::APP_ID, &path)?;
                    path
                }
            },
        };
        if path.is_dir() {
            // Vault mode: searchable note list; quitting the editor returns
            // here. The session title follows navigation: the open note
            // while editing, the vault folder while browsing.
            let vault_title = display_name(&path);
            status.set_title(&vault_title);
            status.set_context(&browsing_context(&path));
            status.flush();
            let mut picker = Picker::open(path.clone(), theme)?;
            while let Some(file) = picker.pick(terminal, &mut ui_bridge, &mut ui_revision)? {
                status.set_title(&display_name(&file));
                status.set_status(&editing_status(&file));
                status.flush();
                App::open_with_autosave(file, theme, start::read_autosave(install::APP_ID))?
                    .with_back_to_list(true)
                    .run(terminal, &mut status, &mut ui_bridge, &mut ui_revision)?;
                status.set_title(&vault_title);
                status.set_status("browsing notes");
                status.set_context(&browsing_context(&path));
                status.flush();
            }
            Ok(())
        } else {
            status.set_title(&display_name(&path));
            status.set_status(&editing_status(&path));
            status.flush();
            App::open_with_autosave(path, theme, start::read_autosave(install::APP_ID))?.run(
                terminal,
                &mut status,
                &mut ui_bridge,
                &mut ui_revision,
            )
        }
    })?;
    Ok(())
}

/// Sidebar status line: which note is open. Editing is user-paced typing, so
/// this App never claims Busy — the status text is the live surface.
fn editing_status(path: &Path) -> String {
    format!("editing {}", display_name(path))
}

/// Agent-facing context while the user browses the vault: the folder, with
/// no open file. Editing context comes from the editor loop itself.
fn browsing_context(vault: &Path) -> serde_json::Value {
    let folder = std::fs::canonicalize(vault).unwrap_or_else(|_| vault.to_path_buf());
    serde_json::json!({
        "file": null,
        "folder": folder.display().to_string(),
    })
}

/// The session-title form of a path: the file or folder name. "." resolves
/// to the real directory name so a bare vault launch titles usefully.
fn display_name(path: &Path) -> String {
    let resolved = if path.as_os_str() == "." {
        std::env::current_dir().unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    resolved
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| resolved.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_resolves_dot_for_explicit_vault_launches() {
        assert!(!display_name(&PathBuf::from(".")).is_empty());
    }
}
