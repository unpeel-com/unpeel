//! Static Media component demo for Ratatui and optional native renderers.

use std::error::Error;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use serde::{Deserialize, Serialize};
use unpeel_app_kit::{
    AppMetadata, Media, MediaCellSize, MediaFit, MediaPicker, MediaPixelSize, MediaPointSize,
    MediaSource, MediaSpec, TerminalPointerState, UiBridge, UiBridgeEvent, UiDeltaOperation,
    UiEventKind, UiEventOutcome, UiEventValue,
};

const STATE_FORMAT: &str = "unpeel.app-kit.example.media";
const STATE_FORMAT_VERSION: u32 = 1;
const DEFAULT_STATE_FILE: &str = ".unpeel-media.json";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const VIEW_ID: &str = "main";
const MEDIA_ID: &str = "sample-media";
const ACTIVATE_ACTION: &str = "cycle-fit";
const TINY_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaState {
    format: String,
    format_version: u32,
    revision: u64,
    fit: MediaFit,
}

impl Default for MediaState {
    fn default() -> Self {
        Self {
            format: STATE_FORMAT.to_owned(),
            format_version: STATE_FORMAT_VERSION,
            revision: 1,
            fit: MediaFit::Contain,
        }
    }
}

struct MediaApp {
    state: MediaState,
    state_path: PathBuf,
    media: Media,
    picker: MediaPicker,
}

impl MediaApp {
    fn load(state_path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let state = match fs::read(&state_path) {
            Ok(bytes) => {
                let state: MediaState = serde_json::from_slice(&bytes)?;
                validate_saved_state(&state)?;
                state
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => MediaState::default(),
            Err(error) => return Err(error.into()),
        };
        // Half-blocks make this example deterministic in SwiftTerm and in plain
        // terminals. Apps can use from_query_stdio() to select Kitty/iTerm/Sixel.
        let picker = MediaPicker::halfblocks();
        let media = Media::load(media_spec(state.fit, "A tiny App Kit test image"), &picker)?;
        Ok(Self {
            state,
            state_path,
            media,
            picker,
        })
    }

    fn spec(&self) -> MediaSpec {
        media_spec(self.state.fit, "A tiny App Kit test image")
    }

    fn cycle_fit(&mut self) -> Result<(u64, u64), Box<dyn Error>> {
        self.state.fit = match self.state.fit {
            MediaFit::Contain => MediaFit::Cover,
            MediaFit::Cover => MediaFit::Fill,
            MediaFit::Fill => MediaFit::Contain,
        };
        let base = self.state.revision;
        self.state.revision = base
            .checked_add(1)
            .filter(|revision| *revision <= MAX_SAFE_INTEGER)
            .ok_or("revision space is exhausted")?;
        self.media = Media::load(self.spec(), &self.picker)?;
        save_state(&self.state_path, &self.state)?;
        Ok((base, self.state.revision))
    }
}

fn media_spec(fit: MediaFit, alt: &str) -> MediaSpec {
    MediaSpec::new(
        MediaSource::inline("image/png", TINY_PNG),
        MediaPixelSize::new(1, 1),
        alt,
    )
    .cells(MediaCellSize::new(28, 10))
    .points(MediaPointSize::new(420, 220))
    .fit(fit)
    .activate(ACTIVATE_ACTION)
}

fn validate_saved_state(state: &MediaState) -> Result<(), Box<dyn Error>> {
    if state.format != STATE_FORMAT
        || state.format_version != STATE_FORMAT_VERSION
        || state.revision == 0
        || state.revision > MAX_SAFE_INTEGER
    {
        return Err("unsupported Media save format; migrate or remove the state file".into());
    }
    Ok(())
}

fn save_state(path: &Path, state: &MediaState) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("media.json");
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
    match std::env::var_os("UNPEEL_MEDIA_PATH") {
        Some(path) if !path.is_empty() => Ok(PathBuf::from(path)),
        _ => Ok(std::env::current_dir()?.join(DEFAULT_STATE_FILE)),
    }
}

fn publish_root(
    app: &MediaApp,
    bridge: &mut UiBridge,
    base_revision: u64,
) -> Result<(), Box<dyn Error>> {
    bridge.publish_delta(
        VIEW_ID,
        base_revision,
        app.state.revision,
        vec![UiDeltaOperation::ReplaceRoot {
            root: app.spec().ui_node(MEDIA_ID),
        }],
    )?;
    Ok(())
}

fn drain_bridge(app: &mut MediaApp, bridge: &mut UiBridge) -> Result<(), Box<dyn Error>> {
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
                    app.state.revision,
                    media_spec(app.state.fit, &format!("App Kit image for {name}"))
                        .ui_node(MEDIA_ID),
                )?;
            }
            UiBridgeEvent::Action { event, .. } => {
                let valid = event.base_revision == app.state.revision
                    && event.action.node_id.as_str() == MEDIA_ID
                    && event.action.action.as_str() == ACTIVATE_ACTION
                    && event.action.kind == UiEventKind::Activate
                    && event.action.value == UiEventValue::None;
                let outcome = if valid {
                    let (base, _) = app.cycle_fit()?;
                    publish_root(app, bridge, base)?;
                    UiEventOutcome::Applied
                } else {
                    UiEventOutcome::Rejected(
                        "Media action is stale or not declared by this component".to_owned(),
                    )
                };
                bridge.acknowledge(&event, outcome, app.state.revision)?;
            }
            UiBridgeEvent::Attached { .. }
            | UiBridgeEvent::Detached { .. }
            | UiBridgeEvent::Lifecycle { .. } => {}
        }
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut app = MediaApp::load(state_path()?)?;
    save_state(&app.state_path, &app.state)?;
    let mut bridge = UiBridge::detect(
        AppMetadata::new(
            "dev.unpeel.app-kit.media",
            "Media",
            env!("CARGO_PKG_VERSION"),
        )
        .description("Static reference-only Media component demo"),
    )?;
    bridge.publish(VIEW_ID, app.state.revision, app.spec().ui_node(MEDIA_ID))?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut media_area = Rect::default();
    let mut pointer = TerminalPointerState::new();
    let result = (|| -> Result<(), Box<dyn Error>> {
        loop {
            drain_bridge(&mut app, &mut bridge)?;
            if bridge.should_render_terminal() {
                terminal.draw(|frame| {
                    let vertical = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(2),
                            Constraint::Length(app.media.cell_size().height),
                            Constraint::Min(1),
                        ])
                        .split(frame.area());
                    frame.render_widget(
                        Paragraph::new(format!("Media · {:?}", app.state.fit))
                            .alignment(Alignment::Center)
                            .style(Style::new().fg(Color::Cyan)),
                        vertical[0],
                    );
                    let horizontal = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Fill(1),
                            Constraint::Length(app.media.cell_size().width),
                            Constraint::Fill(1),
                        ])
                        .split(vertical[1]);
                    media_area = horizontal[1];
                    frame.render_widget(app.media.widget().pointer(pointer), media_area);
                    frame.render_widget(
                        Paragraph::new("Space/Enter/click cycles fit · Esc quits")
                            .alignment(Alignment::Center)
                            .style(Style::new().fg(Color::DarkGray)),
                        vertical[2],
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
                    if key.code == KeyCode::Esc
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        break;
                    }
                    if matches!(key.code, KeyCode::Char(' ') | KeyCode::Enter) {
                        let (base, _) = app.cycle_fit()?;
                        publish_root(&app, &mut bridge, base)?;
                    }
                }
                Event::Mouse(mouse) => {
                    pointer.track(&mouse);
                    if app.spec().action_for_mouse(&mouse, media_area).is_some() {
                        let (base, _) = app.cycle_fit()?;
                        publish_root(&app, &mut bridge, base)?;
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
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn main() {
    if let Err(error) = run() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        eprintln!("media: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_fit_restores_after_activation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("media.json");
        let mut app = MediaApp::load(path.clone()).unwrap();
        app.cycle_fit().unwrap();

        let restored = MediaApp::load(path).unwrap();
        assert_eq!(restored.state.fit, MediaFit::Cover);
        assert_eq!(restored.state.revision, 2);
    }
}
