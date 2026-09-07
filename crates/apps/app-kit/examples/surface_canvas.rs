//! Standalone-first Surface canvas with semantic App Kit buttons overlaid.
//!
//! The planet scene and pointer/key input stay on Surface's USRF channel. The
//! closed CanvasPage toolbar is ordinary App Kit state and its Button actions
//! use `unpeel.ui` when hosted. With no Host, the exact same controls are a
//! fully interactive Ratatui overlay.

use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use unpeel_app_kit::surface_runtime::{
    EVENT_ACTION, EVENT_KEY_DOWN, EVENT_KEY_HOME, EVENT_KEY_UP, EVENT_POINTER_DOWN,
    EVENT_POINTER_DRAG, EVENT_POINTER_MOVE, EVENT_POINTER_UP, EVENT_SCROLL_DOWN, EVENT_SCROLL_UP,
};
#[cfg(feature = "ui-bridge")]
use unpeel_app_kit::{
    AppMetadata, UiBridge, UiBridgeEvent, UiEvent, UiEventKind, UiEventOutcome, UiEventValue,
};
use unpeel_app_kit::{
    Button, ButtonRole, CanvasPage, Surface, SurfaceBackground, SurfaceInputPolicy,
    SurfaceReference, SurfaceSpec, SurfaceView, TerminalPointerState,
};

#[cfg(feature = "ui-bridge")]
const VIEW_ID: &str = "main";
#[cfg(feature = "ui-bridge")]
const ROOT_ID: &str = "planet-canvas-page";
const SURFACE_ID: &str = "planet-canvas";
const STREAM_ID: &str = "canvas-planets";
const OVERVIEW_ID: &str = "canvas-overview";
const PREVIOUS_ID: &str = "canvas-previous";
const NEXT_ID: &str = "canvas-next";
const SELECT_ID: &str = "canvas-select";
const OVERVIEW_ACTION: &str = "show-overview";
const PREVIOUS_ACTION: &str = "previous-planet";
const NEXT_ACTION: &str = "next-planet";
const SELECT_ACTION: &str = "select-planet";
const DEFAULT_GUEST_RELATIVE_PATH: &str =
    "../unpeel-surface/target/wasm32-unknown-unknown/release/surface_planets_example.wasm";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CanvasIntent {
    Overview,
    Previous,
    Next,
    Select,
}

impl CanvasIntent {
    const fn surface_event(self) -> i32 {
        match self {
            Self::Overview => EVENT_KEY_HOME,
            Self::Previous => EVENT_KEY_UP,
            Self::Next => EVENT_KEY_DOWN,
            Self::Select => EVENT_ACTION,
        }
    }
}

fn surface_spec() -> SurfaceSpec {
    let session_id = std::env::var("UNPEEL_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "standalone-canvas".to_owned());
    SurfaceSpec::new(SurfaceReference::new(session_id, STREAM_ID))
        .background(SurfaceBackground::Solid {
            color: "#050912ff".to_owned(),
        })
        .input_policy(SurfaceInputPolicy::PointerAndKeyboard)
}

fn canvas_page() -> CanvasPage {
    CanvasPage::new("Planet Canvas", SURFACE_ID, surface_spec())
        .button(Button::new(OVERVIEW_ID, "Overview", OVERVIEW_ACTION))
        .button(Button::new(PREVIOUS_ID, "Previous", PREVIOUS_ACTION))
        .button(Button::new(NEXT_ID, "Next", NEXT_ACTION))
        .button(Button::new(SELECT_ID, "Select", SELECT_ACTION).role(ButtonRole::Primary))
}

fn guest_path() -> Result<PathBuf, Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    if let Some(argument) = arguments.next() {
        if argument == "--guest" {
            let path = arguments.next().ok_or("--guest needs a WASM path")?;
            return existing_guest(PathBuf::from(path));
        }
        return Err(format!("unknown argument {argument:?}; expected --guest PATH").into());
    }
    if let Some(path) = std::env::var_os("UNPEEL_SURFACE_PLANETS_WASM")
        && !path.is_empty()
    {
        return existing_guest(PathBuf::from(path));
    }
    existing_guest(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_GUEST_RELATIVE_PATH))
}

fn existing_guest(path: PathBuf) -> Result<PathBuf, Box<dyn Error>> {
    if path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "planet guest not found at {}\n\
         build it with:\n  cargo build --release --manifest-path \
         ../unpeel-surface/Cargo.toml -p surface-planets-example \
         --target wasm32-unknown-unknown\n\
         or pass --guest PATH / set UNPEEL_SURFACE_PLANETS_WASM",
        path.display()
    )
    .into())
}

fn intent_for_button(id: &str, action: &str) -> Option<CanvasIntent> {
    match (id, action) {
        (OVERVIEW_ID, OVERVIEW_ACTION) => Some(CanvasIntent::Overview),
        (PREVIOUS_ID, PREVIOUS_ACTION) => Some(CanvasIntent::Previous),
        (NEXT_ID, NEXT_ACTION) => Some(CanvasIntent::Next),
        (SELECT_ID, SELECT_ACTION) => Some(CanvasIntent::Select),
        _ => None,
    }
}

#[cfg(feature = "ui-bridge")]
fn semantic_intent(event: &UiEvent) -> Result<CanvasIntent, String> {
    if event.action.kind != UiEventKind::Activate || event.action.value != UiEventValue::None {
        return Err("Canvas toolbar Buttons require an activate action with no value".to_owned());
    }
    intent_for_button(event.action.node_id.as_str(), event.action.action.as_str())
        .ok_or_else(|| "Action is not declared by this CanvasPage".to_owned())
}

#[cfg(feature = "ui-bridge")]
fn drain_bridge(
    surface: &mut Surface,
    bridge: &mut UiBridge,
    revision: u64,
) -> Result<(), Box<dyn Error>> {
    while let Some(message) = bridge.poll()? {
        match message {
            UiBridgeEvent::Action { event, .. } => {
                let outcome = match semantic_intent(&event) {
                    Ok(intent) if event.base_revision == revision => {
                        surface.event(intent.surface_event(), 0, 0)?;
                        UiEventOutcome::Applied
                    }
                    Ok(_) => UiEventOutcome::Rejected(
                        "Canvas toolbar revision changed; retry the action".to_owned(),
                    ),
                    Err(message) => UiEventOutcome::Rejected(message),
                };
                bridge.acknowledge(&event, outcome, revision)?;
            }
            UiBridgeEvent::Attached { .. }
            | UiBridgeEvent::Detached { .. }
            | UiBridgeEvent::Lifecycle { .. } => {}
        }
    }
    Ok(())
}

fn handle_mouse(
    surface: &mut Surface,
    page: &CanvasPage,
    area: Rect,
    selected: &mut usize,
    pointer: &mut TerminalPointerState,
    mouse: MouseEvent,
) -> Result<(), Box<dyn Error>> {
    pointer.track(&mouse);
    if let Some((index, button)) = page.action_for_mouse(area, &mouse) {
        *selected = index;
        let intent =
            intent_for_button(&button.id, &button.action).expect("validated Canvas control");
        surface.event(intent.surface_event(), 0, 0)?;
        return Ok(());
    }
    if page
        .layout(area)
        .toolbar
        .contains(Position::new(mouse.column, mouse.row))
    {
        return Ok(());
    }
    let kind = match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => Some(EVENT_POINTER_DOWN),
        MouseEventKind::Drag(MouseButton::Left) => Some(EVENT_POINTER_DRAG),
        MouseEventKind::Up(MouseButton::Left) => Some(EVENT_POINTER_UP),
        MouseEventKind::Moved => Some(EVENT_POINTER_MOVE),
        MouseEventKind::ScrollUp => Some(EVENT_SCROLL_UP),
        MouseEventKind::ScrollDown => Some(EVENT_SCROLL_DOWN),
        _ => None,
    };
    if let Some(kind) = kind {
        let (x, y) = surface.cell_center(mouse.column, mouse.row);
        surface.event(kind, x, y)?;
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let guest = guest_path()?;
    let page = canvas_page();
    page.validate()?;

    #[cfg(feature = "ui-bridge")]
    let mut bridge = UiBridge::detect(
        AppMetadata::new(
            "dev.unpeel.app-kit.surface-canvas",
            "Surface Canvas",
            env!("CARGO_PKG_VERSION"),
        )
        .description("Surface scene with a closed semantic Button overlay"),
    )?;
    #[cfg(feature = "ui-bridge")]
    let revision = 1;
    #[cfg(feature = "ui-bridge")]
    bridge.publish(VIEW_ID, revision, page.ui_node(ROOT_ID))?;

    let has_remote_presenter = std::env::var_os("UNPEEL_SURFACE_SOCKET").is_some()
        || std::env::var_os("UNPEEL_SURFACE_REMOTE_ADDR").is_some();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let initial = terminal.size()?;
    let mut surface = match Surface::load(
        page.surface.surface.clone(),
        &guest,
        initial.width,
        initial.height,
    ) {
        Ok(surface) => surface,
        Err(error) => {
            disable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                DisableMouseCapture,
                LeaveAlternateScreen
            )?;
            return Err(error);
        }
    };
    let started = Instant::now();
    let mut selected = 0usize;
    let mut pointer = TerminalPointerState::new();
    let mut area = Rect::new(0, 0, initial.width, initial.height);

    let result = (|| -> Result<(), Box<dyn Error>> {
        loop {
            #[cfg(feature = "ui-bridge")]
            drain_bridge(&mut surface, &mut bridge, revision)?;
            #[cfg(feature = "ui-bridge")]
            let terminal_visible = bridge.should_render_terminal();
            #[cfg(not(feature = "ui-bridge"))]
            let terminal_visible = true;

            if terminal_visible || has_remote_presenter {
                let elapsed = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
                surface.render(elapsed, (0, 0))?;
                if terminal_visible {
                    surface.present()?;
                    terminal.draw(|frame| {
                        area = frame.area();
                        frame.render_widget(SurfaceView, area);
                        frame.render_widget(page.widget(Some(selected)).pointer(pointer), area);
                        let help =
                            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
                        frame.render_widget(
                            Paragraph::new(
                                "Tab/←/→ focus · Enter activate · click controls · Esc quit",
                            )
                            .alignment(Alignment::Center)
                            .style(Style::new().fg(Color::White).bg(Color::Rgb(5, 9, 18))),
                            help,
                        );
                    })?;
                }
            }

            if !event::poll(Duration::from_millis(16))? {
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
                    match key.code {
                        KeyCode::Tab | KeyCode::Right => {
                            selected = (selected + 1) % page.controls.len();
                        }
                        KeyCode::BackTab | KeyCode::Left => {
                            selected = selected.checked_sub(1).unwrap_or(page.controls.len() - 1);
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            let button = page.controls[selected].as_button();
                            let intent = intent_for_button(&button.id, &button.action)
                                .expect("validated Canvas control");
                            surface.event(intent.surface_event(), 0, 0)?;
                        }
                        KeyCode::Char(character @ '1'..='4') => {
                            selected = usize::from(character as u8 - b'1');
                            let button = page.controls[selected].as_button();
                            let intent = intent_for_button(&button.id, &button.action)
                                .expect("validated Canvas control");
                            surface.event(intent.surface_event(), 0, 0)?;
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    handle_mouse(
                        &mut surface,
                        &page,
                        area,
                        &mut selected,
                        &mut pointer,
                        mouse,
                    )?;
                }
                Event::Resize(columns, rows) => {
                    terminal.autoresize()?;
                    surface.resize(columns, rows)?;
                }
                _ => {}
            }
        }
        Ok(())
    })();

    let _ = surface.clear();
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
        eprintln!("surface_canvas: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_is_closed_and_reference_only() {
        let page = canvas_page();
        page.validate().unwrap();
        assert_eq!(
            page.required_capabilities(),
            vec!["canvasPage", "surface", "button"]
        );
        let value = serde_json::to_value(&page).unwrap();
        assert_eq!(value["controls"][0]["type"], "button");
        assert!(value["surface"].get("scene").is_none());
        assert!(value["surface"].get("frame").is_none());
        assert!(value["surface"].get("guestPath").is_none());
    }

    #[test]
    fn every_declared_button_maps_to_one_surface_intent() {
        for control in canvas_page().controls {
            let button = control.as_button();
            assert!(intent_for_button(&button.id, &button.action).is_some());
        }
    }
}
