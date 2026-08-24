//! unpeel-usage — local AI usage & credits at a glance.
//!
//! Standalone-first: a complete terminal dashboard in any shell, reading
//! only the files your AI tools already write (`~/.codex`, `~/.claude`).
//! Inside Unpeel it registers as an App: branded sidebar row, live status
//! line, and low-credit alerts that reach the desktop and phone through
//! the ordinary attention/notification pipeline.

mod claude;
mod codex;
mod config;
mod install;
mod sources;
mod theme;
mod timeparse;
mod ui;
mod unpeel;

use config::Config;
use sources::Snapshot;
use std::sync::mpsc;
use std::time::Duration;
use crate::theme::{nav, Nav};
use crate::unpeel::StatusReporter;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use ratatui::crossterm::execute;

fn main() {
    let config = Config::load();
    install::ensure_installed();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("report") => {
            report(&config);
        }
        Some("--version") | Some("-V") => {
            println!("unpeel-usage {}", env!("CARGO_PKG_VERSION"));
        }
        Some(other) => {
            eprintln!("unknown argument '{other}'. Usage: unpeel-usage [report]");
            std::process::exit(2);
        }
        None => run_tui(config),
    }
}

/// One-shot plain-text snapshot for scripts, status bars, and smoke tests.
fn report(config: &Config) {
    let snapshot = Snapshot::scan(config);
    for provider in &snapshot.providers {
        if !provider.present {
            println!("{}: not installed", provider.name);
            continue;
        }
        if provider.metrics.is_empty() {
            println!("{}: no recent activity", provider.name);
            continue;
        }
        for metric in &provider.metrics {
            println!("{}: {} {}", provider.name, metric.label, metric.value);
        }
    }
    for alert in snapshot.alerts() {
        println!("ALERT: {alert}");
    }
}

struct App {
    config: Config,
    status: StatusReporter,
    snapshot: Option<Snapshot>,
    selected: usize,
    expanded: bool,
    scanning: bool,
    alerts_enabled: bool,
    alerting: bool,
    quit: bool,
}

impl App {
    /// Fold a fresh scan in: update the sidebar status line and drive the
    /// alert edge — attention on crossing a threshold, idle on recovery.
    /// Every call is a silent no-op outside Unpeel.
    fn apply(&mut self, snapshot: Snapshot) {
        self.scanning = false;
        let alerting = self.alerts_enabled && !snapshot.alerts().is_empty();
        self.status
            .set_status(&snapshot.status_line(self.alerts_enabled));
        if alerting && !self.alerting {
            self.status.attention();
        } else if !alerting && self.alerting {
            self.status.idle();
        }
        self.alerting = alerting;
        self.snapshot = Some(snapshot);
    }

    fn provider_count(&self) -> usize {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.providers.len())
            .unwrap_or(0)
    }

    fn select(&mut self, index: usize) {
        if index != self.selected {
            self.selected = index;
            self.expanded = false;
        }
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.provider_count() {
            self.select(self.selected + 1);
        }
    }

    fn select_prev(&mut self) {
        if self.selected > 0 {
            self.select(self.selected - 1);
        }
    }
}

fn run_tui(config: Config) {
    let status = StatusReporter::detect();
    status.idle();
    let mut app = App {
        alerts_enabled: config.alerts.enabled,
        config,
        status,
        snapshot: None,
        selected: 0,
        expanded: false,
        scanning: true,
        alerting: false,
        quit: false,
    };

    // Scans run off the UI thread so a large transcript sweep never blocks
    // a frame; the trigger channel doubles as the refresh timer.
    let (snapshot_tx, snapshot_rx) = mpsc::channel::<Snapshot>();
    let (trigger_tx, trigger_rx) = mpsc::channel::<()>();
    let scan_config = app.config.clone();
    let refresh = Duration::from_secs(app.config.refresh_secs.max(5));
    std::thread::spawn(move || loop {
        if snapshot_tx.send(Snapshot::scan(&scan_config)).is_err() {
            return;
        }
        match trigger_rx.recv_timeout(refresh) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    });

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // Card screen positions from the last frame, for mouse hit-testing.
    let mut hits: Vec<ui::Hit> = Vec::new();
    while !app.quit {
        while let Ok(snapshot) = snapshot_rx.try_recv() {
            app.apply(snapshot);
        }
        let view = ui::View {
            selected: app.selected,
            expanded: app.expanded,
            scanning: app.scanning,
            alerts_enabled: app.alerts_enabled,
        };
        let _ = terminal.draw(|frame| hits = ui::draw(frame, app.snapshot.as_ref(), &view));
        if !matches!(event::poll(Duration::from_millis(100)), Ok(true)) {
            continue;
        }
        let Ok(read) = event::read() else { break };
        match read {
            Event::Key(key) => {
                if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    continue;
                }
                match nav(&key) {
                    Some(Nav::Quit) => app.quit = true,
                    Some(Nav::Down) => app.select_next(),
                    Some(Nav::Up) => app.select_prev(),
                    Some(Nav::Top) => app.select(0),
                    Some(Nav::Bottom) => app.select(app.provider_count().saturating_sub(1)),
                    Some(Nav::Select) => app.expanded = !app.expanded,
                    Some(Nav::Back) => app.expanded = false,
                    None => match key.code {
                        KeyCode::Char('r') => {
                            app.scanning = true;
                            let _ = trigger_tx.send(());
                        }
                        KeyCode::Char('a') => {
                            app.alerts_enabled = !app.alerts_enabled;
                            if let Some(snapshot) = app.snapshot.take() {
                                app.apply(snapshot);
                            }
                        }
                        _ => {}
                    },
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => app.select_next(),
                MouseEventKind::ScrollUp => app.select_prev(),
                // Click selects a card; a second click on it toggles details.
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(hit) = hits.iter().find(|hit| hit.contains(mouse.row)) {
                        if hit.index == app.selected {
                            app.expanded = !app.expanded;
                        } else {
                            app.select(hit.index);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    app.status.idle();
    app.status.flush();
}
