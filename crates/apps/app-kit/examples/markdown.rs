//! Standalone Markdown editor and hosted semantic-component demo.

use std::error::Error;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use serde::{Deserialize, Serialize};
use tui_textarea::{Input as TextInput, Key as TextKey};
use unpeel_app_kit::{
    AppMetadata, MarkdownCommandHint, MarkdownEditor, MarkdownEditorActions, MarkdownEditorConfig,
    MarkdownEditorEvent, MarkdownEditorInteraction, MarkdownEditorStyle, MarkdownPresentation,
    UiBridge, UiBridgeEvent, UiEventOutcome, UiNode, markdown_delta_operations,
};

const STATE_FORMAT: &str = "unpeel.app-kit.example.markdown";
const STATE_FORMAT_VERSION: u32 = 1;
const DEFAULT_STATE_FILE: &str = ".unpeel-markdown.json";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const VIEW_ID: &str = "main";
const EDITOR_ID: &str = "markdown-editor";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkdownState {
    format: String,
    format_version: u32,
    revision: u64,
    text: String,
    presentation: MarkdownPresentation,
}

impl Default for MarkdownState {
    fn default() -> Self {
        Self {
            format: STATE_FORMAT.to_owned(),
            format_version: STATE_FORMAT_VERSION,
            revision: 1,
            text: concat!(
                "# App Kit Markdown\n\n",
                "This document is edited by the **same process** in Ratatui and SwiftUI.\n\n",
                "- Switch the kitchen sink between terminal, native, and split views.\n",
                "- Disconnect and reconnect to exercise revision resume.\n",
                "- Let a scoped agent append a line through the semantic channel.\n",
            )
            .to_owned(),
            presentation: MarkdownPresentation::Source,
        }
    }
}

struct MarkdownApp {
    state: MarkdownState,
    revision: u64,
    state_path: PathBuf,
    editor: MarkdownEditor<'static>,
    interaction: MarkdownEditorInteraction,
    status: String,
}

impl MarkdownApp {
    fn load(state_path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let state = match fs::read(&state_path) {
            Ok(bytes) => {
                let state: MarkdownState = serde_json::from_slice(&bytes)?;
                validate_saved_state(&state)?;
                state
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => MarkdownState::default(),
            Err(error) => return Err(error.into()),
        };
        let mut editor = MarkdownEditor::new(
            document_lines(&state.text),
            MarkdownEditorStyle {
                status: Style::new().fg(Color::Gray),
                cursor_line: Style::new().bg(Color::Rgb(27, 31, 38)),
                cursor: Style::new().fg(Color::White),
                selection: Style::new().bg(Color::Rgb(47, 78, 120)),
                hovered: Style::new().bg(Color::Rgb(40, 44, 52)),
                gutter: Style::new().fg(Color::DarkGray),
                current_gutter: Style::new().fg(Color::Cyan),
                scrollbar_track: Style::new().fg(Color::Rgb(40, 44, 52)),
                scrollbar_thumb: Style::new().fg(Color::Rgb(100, 108, 122)),
            },
        );
        editor
            .text_area_mut()
            .set_placeholder_text("Write Markdown… Type / on an empty line for blocks.");
        let revision = state.revision;
        Ok(Self {
            state,
            revision,
            state_path,
            editor,
            interaction: MarkdownEditorInteraction::new(),
            status: String::new(),
        })
    }

    fn config(&self) -> MarkdownEditorConfig {
        self.config_with_title("Kitchen Sink.md")
    }

    fn config_with_title(&self, title: impl Into<String>) -> MarkdownEditorConfig {
        MarkdownEditorConfig::new(EDITOR_ID)
            .title(title)
            .presentation(self.state.presentation)
            .open_menu_action(MarkdownEditorActions::OPEN_MENU)
            .command_hint(MarkdownCommandHint::new("Type '/' for commands"))
            .insert_menu(self.interaction.semantic_insert_menu(&self.editor))
    }

    fn node(&self) -> UiNode {
        self.editor.ui_node(&self.config())
    }

    fn personalized_node(&self, participant_name: &str) -> UiNode {
        self.editor
            .ui_node(&self.config_with_title(format!("Kitchen Sink.md · {participant_name}")))
    }

    fn commit_projection_change(&mut self) -> Result<(u64, u64), Box<dyn Error>> {
        let (base, revision) = self.advance_projection_revision()?;
        self.state.revision = revision;
        self.state.text = markdown_document(self.editor.lines());
        save_state(&self.state_path, &self.state)?;
        self.status = format!("Saved revision {}", self.revision);
        Ok((base, revision))
    }

    fn advance_projection_revision(&mut self) -> Result<(u64, u64), Box<dyn Error>> {
        let base = self.revision;
        self.revision = base
            .checked_add(1)
            .filter(|revision| *revision <= MAX_SAFE_INTEGER)
            .ok_or("revision space is exhausted")?;
        Ok((base, self.revision))
    }

    fn save_without_revision(&mut self) -> Result<(), Box<dyn Error>> {
        self.state.revision = self.revision;
        self.state.text = markdown_document(self.editor.lines());
        save_state(&self.state_path, &self.state)?;
        self.status = format!("Saved {}", self.state_path.display());
        Ok(())
    }
}

fn document_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split('\n').map(ToOwned::to_owned).collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn markdown_document(lines: &[String]) -> String {
    lines.join("\n")
}

fn validate_saved_state(state: &MarkdownState) -> Result<(), Box<dyn Error>> {
    if state.format != STATE_FORMAT
        || state.format_version != STATE_FORMAT_VERSION
        || state.revision == 0
        || state.revision > MAX_SAFE_INTEGER
    {
        return Err("unsupported Markdown save format; migrate or remove the state file".into());
    }
    Ok(())
}

fn save_state(path: &Path, state: &MarkdownState) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("markdown.json");
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), state)?;
    temporary.write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn state_path() -> io::Result<PathBuf> {
    match std::env::var_os("UNPEEL_MARKDOWN_PATH") {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Ok(std::env::current_dir()?.join(DEFAULT_STATE_FILE)),
    }
}

fn text_input(key: KeyEvent) -> TextInput {
    let mapped = match key.code {
        KeyCode::Char(character) => TextKey::Char(character),
        KeyCode::F(number) => TextKey::F(number),
        KeyCode::Backspace => TextKey::Backspace,
        KeyCode::Enter => TextKey::Enter,
        KeyCode::Left => TextKey::Left,
        KeyCode::Right => TextKey::Right,
        KeyCode::Up => TextKey::Up,
        KeyCode::Down => TextKey::Down,
        KeyCode::Tab | KeyCode::BackTab => TextKey::Tab,
        KeyCode::Delete => TextKey::Delete,
        KeyCode::Home => TextKey::Home,
        KeyCode::End => TextKey::End,
        KeyCode::PageUp => TextKey::PageUp,
        KeyCode::PageDown => TextKey::PageDown,
        KeyCode::Esc => TextKey::Esc,
        _ => TextKey::Null,
    };
    TextInput {
        key: mapped,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

fn publish_projection_change(
    app: &mut MarkdownApp,
    bridge: &mut UiBridge,
    previous: UiNode,
    persist_model: bool,
) -> Result<bool, Box<dyn Error>> {
    let next = app.node();
    let operations = markdown_delta_operations(&previous, &next);
    if operations.is_empty() {
        return Ok(false);
    }
    let (base_revision, revision) = if persist_model {
        app.commit_projection_change()?
    } else {
        app.advance_projection_revision()?
    };
    bridge.publish_delta(VIEW_ID, base_revision, revision, operations)?;
    Ok(true)
}

fn drain_bridge(app: &mut MarkdownApp, bridge: &mut UiBridge) -> Result<(), Box<dyn Error>> {
    while let Some(message) = bridge.poll()? {
        match message {
            UiBridgeEvent::Attached {
                participant,
                client_id,
                ..
            } if std::env::var_os("UNPEEL_KITCHEN_SINK").is_some() => {
                let name = participant
                    .display_name
                    .as_deref()
                    .unwrap_or(participant.id.as_str());
                bridge.publish_to(
                    client_id,
                    VIEW_ID,
                    app.revision,
                    app.personalized_node(name),
                )?;
            }
            UiBridgeEvent::Action { event, .. } => {
                let previous = app.node();
                let config = app.config();
                let menu_result = app.interaction.handle_semantic_ui_event(
                    app.revision,
                    EDITOR_ID,
                    &mut app.editor,
                    &event,
                );
                let outcome = match menu_result {
                    Err(error) => UiEventOutcome::Rejected(error.to_string()),
                    Ok(Some(interaction)) => {
                        publish_projection_change(
                            app,
                            bridge,
                            previous,
                            interaction.text_changed(),
                        )?;
                        UiEventOutcome::Applied
                    }
                    Ok(None) => match app.editor.handle_ui_event(app.revision, &config, &event) {
                        Ok(Some(MarkdownEditorEvent::TextChanged { changed: true }))
                        | Ok(Some(MarkdownEditorEvent::Undo { changed: true }))
                        | Ok(Some(MarkdownEditorEvent::Redo { changed: true })) => {
                            publish_projection_change(app, bridge, previous, true)?;
                            UiEventOutcome::Applied
                        }
                        Ok(Some(MarkdownEditorEvent::PresentationRequested(presentation))) => {
                            app.state.presentation = presentation;
                            publish_projection_change(app, bridge, previous, true)?;
                            UiEventOutcome::Applied
                        }
                        Ok(Some(MarkdownEditorEvent::BackRequested)) => UiEventOutcome::Rejected(
                            "this example has no screen to go back to".to_owned(),
                        ),
                        Ok(Some(MarkdownEditorEvent::SaveRequested)) => {
                            app.save_without_revision()?;
                            UiEventOutcome::Applied
                        }
                        Ok(Some(MarkdownEditorEvent::MenuRequested(_))) => UiEventOutcome::Rejected(
                            "Markdown Menu trigger was not routed through its interaction model"
                                .to_owned(),
                        ),
                        Ok(Some(MarkdownEditorEvent::SelectionChanged)) => {
                            publish_projection_change(app, bridge, previous, false)?;
                            UiEventOutcome::Applied
                        }
                        Ok(Some(MarkdownEditorEvent::TextChanged { changed: false }))
                        | Ok(Some(MarkdownEditorEvent::Undo { changed: false }))
                        | Ok(Some(MarkdownEditorEvent::Redo { changed: false })) => {
                            UiEventOutcome::Applied
                        }
                        Ok(None) => UiEventOutcome::Rejected(
                            "Action targets a different component".to_owned(),
                        ),
                        Err(error) => UiEventOutcome::Rejected(error.to_string()),
                    },
                };
                bridge.acknowledge(&event, outcome, app.revision)?;
            }
            UiBridgeEvent::Attached { .. }
            | UiBridgeEvent::Detached { .. }
            | UiBridgeEvent::Lifecycle { .. } => {}
        }
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut app = MarkdownApp::load(state_path()?)?;
    app.save_without_revision()?;
    let mut bridge = UiBridge::detect(
        AppMetadata::new(
            "dev.unpeel.app-kit.markdown",
            "Markdown",
            env!("CARGO_PKG_VERSION"),
        )
        .description("Standalone Ratatui and hosted SwiftUI Markdown editor"),
    )?;
    bridge.publish(VIEW_ID, app.revision, app.node())?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = (|| -> Result<(), Box<dyn Error>> {
        loop {
            drain_bridge(&mut app, &mut bridge)?;
            if bridge.should_render_terminal() {
                terminal.draw(|frame| {
                    let areas = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(1),
                            Constraint::Min(1),
                            Constraint::Length(1),
                        ])
                        .split(frame.area());
                    frame.render_widget(
                        Paragraph::new(" App Kit Markdown").style(Style::new().fg(Color::Cyan)),
                        areas[0],
                    );
                    app.editor.render(frame, areas[1], true);
                    app.interaction.render_overlay(&app.editor, frame);
                    let controls =
                        "/ insert · drag/double/triple-click select · Ctrl-S save · Esc quit";
                    let help = if app.status.is_empty() {
                        controls.to_owned()
                    } else {
                        format!("{} · {controls}", app.status)
                    };
                    frame.render_widget(
                        Paragraph::new(help).style(Style::new().fg(Color::DarkGray)),
                        areas[2],
                    );
                })?;
            }

            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            match event::read()? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    if key.code == KeyCode::Esc && !app.interaction.is_insert_menu_open() {
                        break;
                    }
                    if key.code == KeyCode::Char('s')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.save_without_revision()?;
                        continue;
                    }
                    let previous = app.node();
                    let outcome = app
                        .interaction
                        .handle_input(&mut app.editor, text_input(key));
                    if outcome.is_handled() {
                        publish_projection_change(
                            &mut app,
                            &mut bridge,
                            previous,
                            outcome.text_changed(),
                        )?;
                    }
                }
                Event::Paste(text) => {
                    let previous = app.node();
                    let outcome = app.interaction.handle_paste(&mut app.editor, &text);
                    if outcome.is_handled() {
                        publish_projection_change(
                            &mut app,
                            &mut bridge,
                            previous,
                            outcome.text_changed(),
                        )?;
                    }
                }
                Event::Mouse(mouse) => {
                    let previous = app.node();
                    app.editor.track_mouse(&mouse);
                    let position = Position::new(mouse.column, mouse.row);
                    let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
                    let outcome = match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            app.interaction
                                .pointer_down(&mut app.editor, position, shift)
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            app.interaction.pointer_drag(&mut app.editor, position)
                        }
                        MouseEventKind::Moved => {
                            app.interaction.pointer_move(&mut app.editor, position)
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            app.interaction.pointer_up(&mut app.editor)
                        }
                        MouseEventKind::ScrollDown => {
                            app.interaction
                                .pointer_scroll(&mut app.editor, position, 2, shift)
                        }
                        MouseEventKind::ScrollUp => {
                            app.interaction
                                .pointer_scroll(&mut app.editor, position, -2, shift)
                        }
                        _ => Default::default(),
                    };
                    if outcome.is_handled() {
                        publish_projection_change(
                            &mut app,
                            &mut bridge,
                            previous,
                            outcome.text_changed(),
                        )?;
                    }
                }
                Event::Resize(_, _) => terminal.autoresize()?,
                _ => {}
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn main() {
    if let Err(error) = run() {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        eprintln!("markdown: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tui_textarea::CursorMove;

    #[test]
    fn durable_document_restores_after_a_committed_edit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("markdown.json");
        let mut app = MarkdownApp::load(path.clone()).unwrap();
        app.editor.text_area_mut().insert_str("persisted ");
        app.commit_projection_change().unwrap();

        let restored = MarkdownApp::load(path).unwrap();
        assert!(markdown_document(restored.editor.lines()).starts_with("persisted "));
        assert_eq!(restored.state.revision, 2);
    }

    #[test]
    fn selection_only_projection_advances_runtime_without_rewriting_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("markdown.json");
        let mut app = MarkdownApp::load(path.clone()).unwrap();
        app.save_without_revision().unwrap();
        let saved = fs::read(&path).unwrap();
        let previous = app.node();

        app.editor
            .text_area_mut()
            .move_cursor(CursorMove::Jump(0, 2));
        app.editor.text_area_mut().start_selection();
        app.editor
            .text_area_mut()
            .move_cursor(CursorMove::Jump(0, 8));
        let operations = markdown_delta_operations(&previous, &app.node());
        assert!(operations.iter().any(|operation| matches!(
            operation,
            unpeel_app_kit::UiDeltaOperation::MarkdownSetSelection { .. }
        )));

        assert_eq!(app.advance_projection_revision().unwrap(), (1, 2));
        assert_eq!(app.revision, 2);
        assert_eq!(app.state.revision, 1);
        assert_eq!(fs::read(path).unwrap(), saved);
    }

    #[test]
    fn hosted_slash_round_trip_publishes_and_applies_the_same_menu_as_terminal() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = MarkdownApp::load(directory.path().join("markdown.json")).unwrap();
        app.editor.text_area_mut().move_cursor(CursorMove::Bottom);
        app.editor.text_area_mut().move_cursor(CursorMove::End);
        app.editor.text_area_mut().insert_newline();
        let previous = app.node();
        let unpeel_app_kit::UiComponent::MarkdownEditor(editor) = &previous.element else {
            panic!("Markdown example must publish MarkdownEditor");
        };
        assert_eq!(
            editor
                .actions
                .open_menu
                .as_ref()
                .map(|action| action.as_str()),
            Some(MarkdownEditorActions::OPEN_MENU)
        );
        assert_eq!(
            editor.command_hint.as_ref().map(|hint| hint.text.as_str()),
            Some("Type '/' for commands")
        );

        let slash = unpeel_app_kit::UiEvent::new(
            "app-test",
            "person-test",
            "client-test",
            "renderer-test",
            VIEW_ID,
            "event-slash",
            app.revision,
            unpeel_app_kit::UiAction::new(
                EDITOR_ID,
                MarkdownEditorActions::OPEN_MENU,
                unpeel_app_kit::UiEventKind::Command,
                unpeel_app_kit::UiEventValue::Text("slash".to_owned()),
            ),
        );
        assert!(
            app.interaction
                .handle_semantic_ui_event(app.revision, EDITOR_ID, &mut app.editor, &slash)
                .unwrap()
                .is_some_and(|outcome| outcome.text_changed())
        );
        let with_menu = app.node();
        let operations = markdown_delta_operations(&previous, &with_menu);
        assert!(operations.iter().any(|operation| matches!(
            operation,
            unpeel_app_kit::UiDeltaOperation::MarkdownSetMenus {
                insert_menu: Some(_),
                ..
            }
        )));
        let unpeel_app_kit::UiComponent::MarkdownEditor(editor) = &with_menu.element else {
            panic!("slash must preserve MarkdownEditor");
        };
        let item = editor.insert_menu.as_ref().unwrap().items[0].clone();
        app.advance_projection_revision().unwrap();

        let selection = unpeel_app_kit::UiEvent::new(
            "app-test",
            "person-test",
            "client-test",
            "renderer-test",
            VIEW_ID,
            "event-selection",
            app.revision,
            unpeel_app_kit::UiAction::activate(item.id, item.action),
        );
        assert!(
            app.interaction
                .handle_semantic_ui_event(app.revision, EDITOR_ID, &mut app.editor, &selection,)
                .unwrap()
                .is_some_and(|outcome| outcome.text_changed())
        );
        let selected = app.node();
        let unpeel_app_kit::UiComponent::MarkdownEditor(editor) = &selected.element else {
            panic!("selection must preserve MarkdownEditor");
        };
        assert!(editor.insert_menu.is_none());
        assert!(markdown_document(app.editor.lines()).ends_with("# "));
    }
}
