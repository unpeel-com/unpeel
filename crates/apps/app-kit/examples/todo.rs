//! Canonical App Kit example: a persisted todo list in one struct.
//!
//! `cargo run --example todo` in any terminal. With the default `ui-bridge`
//! feature the same binary also serves SwiftUI and web renderers when an
//! Unpeel Host injects an endpoint. See `docs/writing-an-app.md`.

use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};
use unpeel_app_kit::{
    App, AppAction, AppMetadata, Input, List, ListItem, ListItemSlot, Page, Reduce, Toggle, run_app,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Todo {
    id: u64,
    label: String,
    done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TodoState {
    next_id: u64,
    todos: Vec<Todo>,
}

struct TodoApp {
    state: TodoState,
    path: PathBuf,
    selected: Option<u64>,
}

impl TodoApp {
    fn load(path: PathBuf) -> io::Result<Self> {
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => TodoState {
                next_id: 4,
                todos: [
                    "Run the standalone TUI",
                    "Attach SwiftUI or web",
                    "Invite an agent with edit grant",
                ]
                .iter()
                .enumerate()
                .map(|(i, label)| Todo {
                    id: i as u64 + 1,
                    label: (*label).to_owned(),
                    done: i == 0,
                })
                .collect(),
            },
            Err(error) => return Err(error),
        };
        let selected = state.todos.first().map(|todo| todo.id);
        Ok(Self {
            state,
            path,
            selected,
        })
    }

    fn save(&self) -> io::Result<()> {
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&self.state)?)?;
        fs::rename(temporary, &self.path)
    }
}

fn todo_id(item: &str) -> Option<u64> {
    item.strip_prefix("todo-")?.parse().ok()
}

impl App for TodoApp {
    fn page(&self) -> Page {
        let rows = self.state.todos.iter().map(|todo| {
            ListItem::new(format!("todo-{}", todo.id), todo.label.clone())
                .done(todo.done)
                .trailing(ListItemSlot::toggle(Toggle::new(
                    format!("todo-{}-toggle", todo.id),
                    "Completed",
                    todo.done,
                    "set-done",
                )))
                .delete_action("delete-todo")
        });
        let mut list = List::new("todos", rows.collect()).empty_message("No todos yet");
        if let Some(id) = self.selected {
            list = list.selected(format!("todo-{id}"), "select-todo");
        }
        Page::new("Todos", list).input(
            Input::new("new-todo", "New todo")
                .placeholder("What needs doing?")
                .submit_action("add-todo"),
        )
    }

    fn reduce(&mut self, action: AppAction) -> Reduce {
        match action {
            AppAction::Submit { text, .. } if !text.trim().is_empty() => {
                let id = self.state.next_id;
                self.state.next_id += 1;
                self.state.todos.push(Todo {
                    id,
                    label: text.trim().to_owned(),
                    done: false,
                });
            }
            AppAction::Toggle { item, on, .. } => {
                let Some(todo) = todo_id(&item)
                    .and_then(|id| self.state.todos.iter_mut().find(|todo| todo.id == id))
                else {
                    return Reduce::Ignored;
                };
                todo.done = on;
            }
            AppAction::Delete { item } => {
                let id = todo_id(&item);
                self.state.todos.retain(|todo| Some(todo.id) != id);
                if self.selected == id {
                    self.selected = self.state.todos.first().map(|todo| todo.id);
                }
            }
            AppAction::Select { item } => {
                self.selected = todo_id(&item);
                return Reduce::Changed;
            }
            AppAction::Cancel => return Reduce::Quit,
            _ => return Reduce::Ignored,
        }
        let _ = self.save();
        Reduce::Changed
    }
}

fn main() -> io::Result<()> {
    let path = std::env::var_os("UNPEEL_TODO_PATH")
        .map_or_else(|| PathBuf::from(".unpeel-todo.json"), PathBuf::from);
    let metadata = AppMetadata::new("dev.unpeel.app-kit.todo", "Todo", env!("CARGO_PKG_VERSION"))
        .description("Canonical standalone and hosted App Kit example");
    run_app(TodoApp::load(path)?, metadata)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use unpeel_app_kit::{KitTheme, Session};

    use super::*;

    fn session(dir: &tempfile::TempDir) -> Session<TodoApp> {
        let app = TodoApp::load(dir.path().join("todo.json")).unwrap();
        let mut session = Session::with_theme(app, KitTheme::dark());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| session.render(frame)).unwrap();
        session
    }

    #[test]
    fn durable_reducer_restores_the_committed_model() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = TodoApp::load(dir.path().join("todo.json")).unwrap();
        app.reduce(AppAction::Toggle {
            item: "todo-2".into(),
            control: String::new(),
            on: true,
        });
        app.reduce(AppAction::Submit {
            input: "new-todo".into(),
            text: "Persist me".into(),
        });
        let restored = TodoApp::load(dir.path().join("todo.json")).unwrap();
        assert!(restored.state.todos[1].done);
        assert_eq!(restored.state.todos.last().unwrap().label, "Persist me");
    }

    #[test]
    fn terminal_row_click_targets_the_visible_todo_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session(&dir);
        let rows = session.list_state().rows_area();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rows.x + 10,
            row: rows.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        session.handle_mouse(click).unwrap();
        assert!(session.app().state.todos[1].done);
        assert!(!session.input_focused());
    }

    #[test]
    fn terminal_todo_inherits_the_shared_focus_and_role_key_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session(&dir);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        session.handle_key(key(KeyCode::Tab)).unwrap();
        session.handle_key(key(KeyCode::Enter)).unwrap();
        assert!(
            !session.app().state.todos[0].done,
            "Enter flips the first toggle"
        );
        session.handle_key(key(KeyCode::Down)).unwrap();
        assert_eq!(session.app().selected, Some(2));
        session.handle_key(key(KeyCode::Char(' '))).unwrap();
        assert!(
            session.app().state.todos[1].done,
            "Space flips the selected toggle"
        );
    }

    #[cfg(feature = "ui-bridge")]
    #[test]
    fn shared_page_fixture_is_generated_from_the_canonical_todo_model() {
        let dir = tempfile::tempdir().unwrap();
        let app = TodoApp::load(dir.path().join("todo.json")).unwrap();
        let fixture = include_str!("../protocol/unpeel-ui-v1.ndjson")
            .lines()
            .nth(13)
            .unwrap();
        let unpeel_app_kit::UiMessage::Snapshot(snapshot) = serde_json::from_str(fixture).unwrap()
        else {
            panic!("Todo fixture must be a snapshot");
        };
        assert_eq!(
            snapshot.root,
            unpeel_app_kit::UiNode::page("todo-page", app.page())
        );
    }
}
