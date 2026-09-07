//! App Kit Surface embed using unpeel-surface's existing planet guest.
//!
//! Build the guest in a sibling checkout first:
//! `cargo build --release --manifest-path ../unpeel-surface/Cargo.toml
//!   -p surface-planets-example --target wasm32-unknown-unknown`
//!
//! Then run this ordinary TUI:
//! `cargo run --example surface_planets --no-default-features --features surface-embed`

use std::error::Error;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Alignment;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use unpeel_app_kit::surface_runtime::{
    EVENT_ACTION, EVENT_KEY_DOWN, EVENT_KEY_END, EVENT_KEY_HOME, EVENT_KEY_UP,
};
#[cfg(feature = "ui-bridge")]
use unpeel_app_kit::{AppMetadata, UiBridge, UiBridgeEvent, UiEventOutcome};
use unpeel_app_kit::{
    Surface, SurfaceBackground, SurfaceInputPolicy, SurfaceReference, SurfaceSpec, SurfaceView,
};

#[cfg(feature = "ui-bridge")]
const VIEW_ID: &str = "main";
#[cfg(feature = "ui-bridge")]
const SURFACE_ID: &str = "planet-surface";
const STREAM_ID: &str = "planets";
const DEFAULT_GUEST_RELATIVE_PATH: &str =
    "../unpeel-surface/target/wasm32-unknown-unknown/release/surface_planets_example.wasm";

fn surface_spec() -> SurfaceSpec {
    let session_id = std::env::var("UNPEEL_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "standalone-planets".to_owned());
    SurfaceSpec::new(SurfaceReference::new(session_id, STREAM_ID))
        .background(SurfaceBackground::Solid {
            color: "#050912ff".to_owned(),
        })
        .input_policy(SurfaceInputPolicy::PointerAndKeyboard)
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

#[cfg(feature = "ui-bridge")]
fn drain_bridge(bridge: &mut UiBridge, revision: u64) -> Result<(), Box<dyn Error>> {
    while let Some(message) = bridge.poll()? {
        match message {
            UiBridgeEvent::Action { event, .. } => {
                bridge.acknowledge(
                    &event,
                    UiEventOutcome::Rejected(
                        "Surface interaction travels over its authorized USRF stream".to_owned(),
                    ),
                    revision,
                )?;
            }
            UiBridgeEvent::Attached { .. }
            | UiBridgeEvent::Detached { .. }
            | UiBridgeEvent::Lifecycle { .. } => {}
        }
    }
    Ok(())
}

fn send_key(surface: &mut Surface, key: KeyCode) -> Result<bool, Box<dyn Error>> {
    let event = match key {
        KeyCode::Up | KeyCode::Left => EVENT_KEY_UP,
        KeyCode::Down | KeyCode::Right => EVENT_KEY_DOWN,
        KeyCode::Home => EVENT_KEY_HOME,
        KeyCode::End => EVENT_KEY_END,
        KeyCode::Enter | KeyCode::Char(' ') => EVENT_ACTION,
        _ => return Ok(false),
    };
    surface.event(event, 0, 0)?;
    Ok(true)
}

fn run() -> Result<(), Box<dyn Error>> {
    let guest = guest_path()?;
    let spec = surface_spec();
    spec.validate()?;

    #[cfg(feature = "ui-bridge")]
    let mut bridge = UiBridge::detect(
        AppMetadata::new(
            "dev.unpeel.app-kit.surface-planets",
            "Surface Planets",
            env!("CARGO_PKG_VERSION"),
        )
        .description("Reference-only App Kit embed of the unpeel-surface planet guest"),
    )?;
    #[cfg(feature = "ui-bridge")]
    let revision = 1;
    #[cfg(feature = "ui-bridge")]
    bridge.publish(VIEW_ID, revision, spec.ui_node(SURFACE_ID))?;

    // SurfaceLayer consumes this independent route itself. Its presence means
    // the guest must keep producing retained scenes even when the local PTY is
    // hidden in favor of a connected presenter.
    let has_remote_presenter = std::env::var_os("UNPEEL_SURFACE_SOCKET").is_some()
        || std::env::var_os("UNPEEL_SURFACE_REMOTE_ADDR").is_some();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let initial = terminal.size()?;
    let mut surface = match Surface::load(spec, &guest, initial.width, initial.height) {
        Ok(surface) => surface,
        Err(error) => {
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
            return Err(error);
        }
    };
    let started = Instant::now();

    let result = (|| -> Result<(), Box<dyn Error>> {
        loop {
            #[cfg(feature = "ui-bridge")]
            drain_bridge(&mut bridge, revision)?;
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
                        frame.render_widget(SurfaceView, frame.area());
                        let help_area = ratatui::layout::Rect {
                            x: frame.area().x,
                            y: frame.area().bottom().saturating_sub(1),
                            width: frame.area().width,
                            height: 1,
                        };
                        frame.render_widget(
                            Paragraph::new(
                                "Surface Planets · arrows/Space navigate · Home overview · Esc quit",
                            )
                            .alignment(Alignment::Center)
                            .style(Style::new().fg(Color::White).bg(Color::Rgb(5, 9, 18))),
                            help_area,
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
                    let _ = send_key(&mut surface, key.code)?;
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
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn main() {
    if let Err(error) = run() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        eprintln!("surface_planets: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "ui-bridge")]
    #[test]
    fn projection_is_reference_only() {
        let value = serde_json::to_value(surface_spec().ui_node(SURFACE_ID)).unwrap();
        assert_eq!(value["type"], "surface");
        assert_eq!(value["reference"]["streamId"], STREAM_ID);
        assert!(value.get("scene").is_none());
        assert!(value.get("frame").is_none());
        assert!(value.get("guestPath").is_none());
    }

    #[test]
    fn missing_guest_error_explains_the_build_step() {
        let error =
            existing_guest(std::path::Path::new("/definitely/missing/planets.wasm").to_owned())
                .unwrap_err()
                .to_string();
        assert!(error.contains("surface-planets-example"));
        assert!(error.contains("--guest"));
    }
}
