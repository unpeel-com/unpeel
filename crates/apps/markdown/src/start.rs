//! Persistent bare-launch state shared in shape with `unpeel-design`.
//!
//! A command-line path is always explicit and bypasses this state. A bare
//! launch remembers only the user-chosen notes folder; notes themselves stay
//! ordinary Markdown files and are rescanned every time the picker opens.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use unpeel_app_kit::{
    Input, InputField, InputFieldTheme, List, ListState, Page, PageTheme, UiBridge, UiBridgeEvent,
    UiComponent, UiEventKind, UiEventOutcome, UiEventValue, UiNode, page_delta_operations,
};

use crate::theme::Theme;

const STATE_VERSION: u64 = 1;
const UI_VIEW_ID: &str = "main";
const UI_ROOT_ID: &str = "workspace-page";
const UI_INPUT_ID: &str = "workspace-folder";
const UI_SET_VALUE: &str = "set-workspace-folder";
const UI_SUBMIT: &str = "choose-workspace-folder";
const UI_CANCEL: &str = "cancel-workspace-folder";

fn config_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("UNPEEL_APP_CONFIG_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Some(path.join("unpeel-apps"));
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config").join("unpeel-apps"))
}

fn state_path(app_id: &str) -> Option<PathBuf> {
    Some(config_root()?.join(app_id).join("start.json"))
}

fn read_state_at(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read(path).ok()?;
    if raw.len() > 16 * 1024 {
        return None;
    }
    let state: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if state.get("version")?.as_u64()? != STATE_VERSION {
        return None;
    }
    Some(state)
}

fn read_workspace_at(path: &Path) -> Option<PathBuf> {
    let state = read_state_at(path)?;
    let workspace = PathBuf::from(state.get("workspace")?.as_str()?);
    workspace.is_dir().then_some(workspace)
}

pub fn read_workspace(app_id: &str) -> Option<PathBuf> {
    read_workspace_at(&state_path(app_id)?)
}

fn read_autosave_at(path: &Path) -> bool {
    read_state_at(path)
        .and_then(|state| state.get("autosave").and_then(serde_json::Value::as_bool))
        .unwrap_or(true)
}

pub fn read_autosave(app_id: &str) -> bool {
    state_path(app_id).is_none_or(|path| read_autosave_at(&path))
}

fn write_state_at(path: &Path, workspace: Option<&Path>, autosave: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("start state has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "version": STATE_VERSION,
        "workspace": workspace.map(|path| path.to_string_lossy().into_owned()),
        "autosave": autosave,
    }))?;
    let temporary = parent.join(format!(".start.json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, body)?;
    std::fs::rename(temporary, path)
}

fn write_workspace_at(path: &Path, workspace: &Path) -> io::Result<()> {
    write_state_at(path, Some(workspace), read_autosave_at(path))
}

pub fn write_workspace(app_id: &str, workspace: &Path) -> io::Result<()> {
    let Some(path) = state_path(app_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no home directory available for start state",
        ));
    };
    write_workspace_at(&path, workspace)
}

fn write_autosave_at(path: &Path, enabled: bool) -> io::Result<()> {
    let workspace = read_workspace_at(path);
    write_state_at(path, workspace.as_deref(), enabled)
}

pub fn write_autosave(app_id: &str, enabled: bool) -> io::Result<()> {
    let Some(path) = state_path(app_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no home directory available for start state",
        ));
    };
    write_autosave_at(&path, enabled)
}

pub fn resolve_folder_input(input: &str) -> io::Result<PathBuf> {
    let input = input.trim();
    if input.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "enter a folder path",
        ));
    }
    let expanded = if input == "~" {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?
    } else if let Some(rest) = input.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?
            .join(rest)
    } else {
        PathBuf::from(input)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()?.join(expanded)
    };
    std::fs::create_dir_all(&absolute)?;
    let canonical = absolute.canonicalize()?;
    if !canonical.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a folder", canonical.display()),
        ));
    }
    Ok(canonical)
}

fn user_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home.filter(|home| !home.as_os_str().is_empty()) {
        if path == home {
            return "~".to_string();
        }
        if let Ok(relative) = path.strip_prefix(home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

fn default_folder_input(project_root: Option<&Path>) -> String {
    let folder = project_root
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .map(|path| path.join("docs"))
        .unwrap_or_else(|| PathBuf::from("docs"));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    user_path(&folder, home.as_deref())
}

/// First-run folder chooser. The project's `docs` folder is prefilled so it
/// can be accepted with Enter, while still allowing a different path.
pub fn choose_workspace(
    terminal: &mut DefaultTerminal,
    theme: Theme,
    project_root: Option<&Path>,
    bridge: &mut UiBridge,
    revision_counter: &mut u64,
) -> io::Result<Option<PathBuf>> {
    let mut value = default_folder_input(project_root);
    let mut error: Option<String> = None;
    let hint = "Press Enter to use this folder, or edit the path";
    let mut revision = revision_counter
        .checked_add(1)
        .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
    let mut published = workspace_node(&value, error.as_deref(), hint);
    let mut input = InputField::new("~/Documents/Notes")
        .with_theme(InputFieldTheme::for_color_scheme(theme.kit.scheme));
    input.set_focused(true);
    bridge
        .publish(UI_VIEW_ID, revision, published.clone())
        .map_err(ui_bridge_error)?;
    loop {
        while let Some(message) = bridge.poll().map_err(ui_bridge_error)? {
            let UiBridgeEvent::Action { event, .. } = message else {
                continue;
            };
            let mut result = None;
            let outcome = if event.base_revision != revision {
                UiEventOutcome::Rejected(format!(
                    "Folder chooser changed from revision {} to {revision}; retry the action",
                    event.base_revision
                ))
            } else {
                match (
                    event.action.node_id.as_str(),
                    event.action.action.as_str(),
                    event.action.kind,
                    &event.action.value,
                ) {
                    (UI_INPUT_ID, UI_SET_VALUE, UiEventKind::Change, UiEventValue::Text(next)) => {
                        value = sanitized_folder_input(next);
                        error = None;
                        UiEventOutcome::Applied
                    }
                    (UI_INPUT_ID, UI_SUBMIT, UiEventKind::Submit, UiEventValue::Text(next)) => {
                        value = sanitized_folder_input(next);
                        match resolve_folder_input(&value) {
                            Ok(path) => {
                                result = Some(Some(path));
                                UiEventOutcome::Applied
                            }
                            Err(failure) => {
                                error = Some(failure.to_string());
                                UiEventOutcome::Rejected(failure.to_string())
                            }
                        }
                    }
                    (UI_ROOT_ID, UI_CANCEL, UiEventKind::Cancel, UiEventValue::None) => {
                        result = Some(None);
                        UiEventOutcome::Applied
                    }
                    _ => UiEventOutcome::Rejected(
                        "Action targets a different folder chooser component".to_string(),
                    ),
                }
            };
            publish_workspace_projection(
                bridge,
                &mut revision,
                &mut published,
                &value,
                error.as_deref(),
                hint,
            )?;
            bridge
                .acknowledge(&event, outcome, revision)
                .map_err(ui_bridge_error)?;
            if let Some(result) = result {
                *revision_counter = revision;
                return Ok(result);
            }
        }
        publish_workspace_projection(
            bridge,
            &mut revision,
            &mut published,
            &value,
            error.as_deref(),
            hint,
        )?;
        if bridge.should_render_terminal() {
            terminal.draw(|frame| {
                let UiComponent::Page(page) = &published.element else {
                    return;
                };
                let mut state = ListState::new(None);
                frame.render_widget(
                    page.widget(&mut input, &mut state)
                        .theme(PageTheme::for_theme(theme.kit)),
                    frame.area(),
                );
                if let Some(position) = input.cursor_position() {
                    frame.set_cursor_position(position);
                }
            })?;
        }

        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Esc => {
                        *revision_counter = revision;
                        return Ok(None);
                    }
                    KeyCode::Enter => match resolve_folder_input(&value) {
                        Ok(path) => {
                            *revision_counter = revision;
                            return Ok(Some(path));
                        }
                        Err(failure) => error = Some(failure.to_string()),
                    },
                    KeyCode::Backspace => {
                        value.pop();
                        error = None;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear();
                        error = None;
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                            && value.chars().count() < 512 =>
                    {
                        value.push(character);
                        error = None;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                let pasted: String = text
                    .chars()
                    .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
                    .take(512usize.saturating_sub(value.chars().count()))
                    .collect();
                value.push_str(&pasted);
                error = None;
            }
            _ => {}
        }
    }
}

fn workspace_node(value: &str, error: Option<&str>, hint: &str) -> UiNode {
    let message = error.unwrap_or(hint);
    UiNode::page(
        UI_ROOT_ID,
        Page::new(
            "Choose your notes folder",
            List::new("workspace-details", Vec::new()).empty_message(message),
        )
        .input(
            Input::new(UI_INPUT_ID, "Folder")
                .value(value)
                .placeholder("~/Documents/Notes")
                .set_value_action(UI_SET_VALUE)
                .submit_action(UI_SUBMIT),
        )
        .back_action(UI_CANCEL),
    )
}

fn publish_workspace_projection(
    bridge: &mut UiBridge,
    revision: &mut u64,
    published: &mut UiNode,
    value: &str,
    error: Option<&str>,
    hint: &str,
) -> io::Result<()> {
    let next = workspace_node(value, error, hint);
    if next == *published {
        return Ok(());
    }
    let next_revision = revision
        .checked_add(1)
        .ok_or_else(|| io::Error::other("Markdown UI revision space is exhausted"))?;
    bridge
        .publish_delta(
            UI_VIEW_ID,
            *revision,
            next_revision,
            page_delta_operations(published, &next),
        )
        .map_err(ui_bridge_error)?;
    *revision = next_revision;
    *published = next;
    Ok(())
}

fn sanitized_folder_input(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(character, '\n' | '\r' | '\0'))
        .take(512)
        .collect()
}

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip_keeps_only_an_existing_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("notes");
        std::fs::create_dir(&workspace).unwrap();
        let state = temp.path().join("state/start.json");
        write_workspace_at(&state, &workspace).unwrap();
        assert_eq!(read_workspace_at(&state), Some(workspace.clone()));
        std::fs::remove_dir(workspace).unwrap();
        assert_eq!(read_workspace_at(&state), None);
    }

    #[test]
    fn autosave_defaults_on_and_persists_without_losing_the_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("notes");
        std::fs::create_dir(&workspace).unwrap();
        let state = temp.path().join("state/start.json");

        assert!(read_autosave_at(&state));
        write_workspace_at(&state, &workspace).unwrap();
        write_autosave_at(&state, false).unwrap();
        assert!(!read_autosave_at(&state));
        assert_eq!(read_workspace_at(&state), Some(workspace.clone()));

        write_workspace_at(&state, &workspace).unwrap();
        assert!(!read_autosave_at(&state));
    }

    #[test]
    fn corrupt_and_future_state_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("start.json");
        std::fs::write(&state, b"not json").unwrap();
        assert_eq!(read_workspace_at(&state), None);
        std::fs::write(
            &state,
            format!(
                r#"{{"version":99,"workspace":{}}}"#,
                serde_json::to_string(&temp.path()).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(read_workspace_at(&state), None);
    }

    #[test]
    fn paths_inside_home_use_a_tilde_prefix() {
        let home = Path::new("/Users/alice");
        assert_eq!(
            user_path(Path::new("/Users/alice/Dev/project/docs"), Some(home)),
            "~/Dev/project/docs"
        );
        assert_eq!(user_path(home, Some(home)), "~");
        assert_eq!(
            user_path(Path::new("/opt/project/docs"), Some(home)),
            "/opt/project/docs"
        );
    }

    #[test]
    fn hosted_project_root_drives_the_first_run_suggestion() {
        assert_eq!(
            default_folder_input(Some(Path::new("/opt/current-worktree"))),
            "/opt/current-worktree/docs"
        );
    }

    #[test]
    fn first_run_folder_chooser_is_a_semantic_page_with_native_input_actions() {
        let node = workspace_node("~/Notes", Some("enter a folder path"), "hint");
        let unpeel_app_kit::UiComponent::Page(page) = node.element else {
            panic!("first-run chooser must publish Page");
        };
        page.validate().unwrap();
        assert_eq!(page.back.as_deref(), Some(UI_CANCEL));
        let input = page.input_spec().unwrap();
        assert_eq!(input.id, UI_INPUT_ID);
        assert_eq!(input.value, "~/Notes");
        assert_eq!(input.set_value.as_deref(), Some(UI_SET_VALUE));
        assert_eq!(input.submit.as_deref(), Some(UI_SUBMIT));
        assert_eq!(page.list().empty_message, "enter a folder path");
    }
}
