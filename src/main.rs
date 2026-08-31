//! unpeel-usage — local AI usage & credits at a glance.
//!
//! Standalone-first: a complete terminal dashboard in any shell, reading
//! only the files your AI tools already write (`~/.codex`, `~/.claude`,
//! `~/.grok`, `~/.local/share/muse`).
//! Inside Unpeel it registers as an App: branded sidebar row, live status
//! line, and opt-in informational alerts that reach Recent, desktop, and phone
//! without changing the session lifecycle. Claude and Grok can reuse their
//! CLI logins for live limits; history remains local.

mod claude;
mod codex;
mod config;
mod grok;
mod install;
mod muse;
mod sources;
mod theme;
mod timeparse;
mod ui;

use crate::theme::{nav, Nav};
use config::{AlertOption, Alerts, Config};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use sources::{Level, Metric, Snapshot};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use unpeel_app_kit::{
    AppContext, AppMetadata, AppReporter, KeyboardEnhancementGuard, ThemeMonitor, UiBridge,
    UiBridgeEvent, UiDeltaOperation, UiEventKind, UiEventOutcome, UiEventValue, UiNode,
};

const UI_VIEW_ID: &str = "main";

fn main() {
    let config = Config::load();
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("report") => {
            report(&config);
        }
        Some("--version") | Some("-V") => {
            println!("unpeel-usage {}", env!("CARGO_PKG_VERSION"));
        }
        Some(other) => {
            eprintln!("unknown argument '{other}'. Usage: unpeel-usage [report|--version]");
            std::process::exit(2);
        }
        None => {
            if let Err(error) = run_tui(config) {
                eprintln!("unpeel-usage: {error}");
                std::process::exit(1);
            }
        }
    }
}

/// One-shot plain-text snapshot for scripts, status bars, and smoke tests.
fn report(config: &Config) {
    let context = AppContext::detect();
    let project = current_project_root(&context);
    let snapshot = Snapshot::scan(config, project.as_deref());
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
}

/// The Host owns project/worktree identity for hosted Apps. A normal shell
/// has no Host context, so its working directory remains the CLI fallback.
fn current_project_root(context: &AppContext) -> Option<PathBuf> {
    context
        .current_root()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LimitState {
    Normal,
    Close,
    Reached,
}

#[derive(Debug)]
struct LimitReading {
    key: String,
    title: String,
    detail: String,
    state: LimitState,
}

fn metric_limit_state(metric: &Metric) -> LimitState {
    let reached = metric.annotation.as_deref() == Some("Limit reached")
        || metric.percent.is_some_and(|percent| percent >= 99.5);
    if reached {
        return LimitState::Reached;
    }
    let close = metric.percent.is_some_and(|percent| percent >= 80.0)
        || (metric.level != Level::Ok && (metric.annotation.is_some() || metric.percent.is_none()));
    if close {
        LimitState::Close
    } else {
        LimitState::Normal
    }
}

fn limit_readings(snapshot: &Snapshot) -> Vec<LimitReading> {
    snapshot
        .providers
        .iter()
        .flat_map(|provider| {
            provider.metrics.iter().map(|metric| {
                let title = format!("{} {}", provider.name, metric.label);
                LimitReading {
                    key: format!("{:?}\0{}\0{}", provider.kind, provider.name, metric.label),
                    title,
                    detail: metric
                        .annotation
                        .clone()
                        .filter(|detail| !detail.trim().is_empty())
                        .unwrap_or_else(|| metric.value.clone()),
                    state: metric_limit_state(metric),
                }
            })
        })
        .collect()
}

#[derive(Debug, Default)]
struct AlertTracker {
    previous: HashMap<String, LimitState>,
}

#[derive(Debug, Default)]
struct AlertUpdate {
    notification: Option<String>,
}

impl AlertTracker {
    /// Observe every metric on every refresh, even while notifications are
    /// disabled. This lets "Available again" describe a real constrained →
    /// normal edge after the user opts into it.
    fn update(&mut self, snapshot: &Snapshot, options: Alerts) -> AlertUpdate {
        let readings = limit_readings(snapshot);
        let mut next = HashMap::with_capacity(readings.len());
        let mut events: Vec<(u8, String)> = Vec::new();

        for reading in readings {
            let previous = self
                .previous
                .get(&reading.key)
                .copied()
                .unwrap_or(LimitState::Normal);
            match (previous, reading.state) {
                (before, LimitState::Reached) if before < LimitState::Reached => {
                    if options.limit_reached {
                        events.push((3, format!("{} limit reached", reading.title)));
                    } else if before == LimitState::Normal && options.close_to_limit {
                        events.push((2, format!("{} is at or near its limit", reading.title)));
                    }
                }
                (LimitState::Normal, LimitState::Close) if options.close_to_limit => {
                    let suffix = (!reading.detail.trim().is_empty())
                        .then(|| format!(" ({})", reading.detail));
                    events.push((
                        2,
                        format!(
                            "{} is close to its limit{}",
                            reading.title,
                            suffix.unwrap_or_default()
                        ),
                    ));
                }
                (before, LimitState::Normal)
                    if before != LimitState::Normal && options.available_again =>
                {
                    events.push((1, format!("{} is available again", reading.title)));
                }
                _ => {}
            }
            next.insert(reading.key, reading.state);
        }

        self.previous = next;
        let event_count = events.len();
        events.sort_by(|left, right| right.0.cmp(&left.0));
        let notification = events.into_iter().next().map(|(_, mut message)| {
            if event_count > 1 {
                message.push_str(&format!(" · {} more changes", event_count - 1));
            }
            message
        });
        AlertUpdate { notification }
    }
}

struct App {
    config: Config,
    palette: theme::Palette,
    status: AppReporter,
    snapshot: Option<Snapshot>,
    selected: usize,
    detail_open: bool,
    scroll_offset: u16,
    max_scroll: u16,
    viewport_height: u16,
    reveal_selected: bool,
    scanning: bool,
    hosted: bool,
    alert_dialog: Option<usize>,
    alert_tracker: AlertTracker,
    quit: bool,
}

enum ScanEvent {
    Started,
    Finished(Snapshot),
}

impl App {
    fn view(&self) -> ui::View {
        ui::View {
            selected: self.selected,
            detail_open: self.detail_open,
            scanning: self.scanning,
            hosted: self.hosted,
            alerts: self.config.alerts,
            alert_dialog: self.alert_dialog,
            scroll_offset: self.scroll_offset,
            reveal_selected: self.reveal_selected,
        }
    }

    fn semantic_node(&self) -> UiNode {
        UiNode::page(
            ui::SEMANTIC_ROOT_ID,
            ui::semantic_page(self.snapshot.as_ref(), &self.view()),
        )
    }

    fn publish_semantic_projection(
        &self,
        bridge: &mut UiBridge,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<()> {
        let next = self.semantic_node();
        if next == *published {
            return Ok(());
        }
        let next_revision = revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Usage UI revision space is exhausted"))?;
        bridge
            .publish_delta(
                UI_VIEW_ID,
                *revision,
                next_revision,
                vec![UiDeltaOperation::ReplaceRoot { root: next.clone() }],
            )
            .map_err(ui_bridge_error)?;
        *revision = next_revision;
        *published = next;
        Ok(())
    }

    fn drain_bridge(
        &mut self,
        bridge: &mut UiBridge,
        trigger: &mpsc::Sender<()>,
        revision: &mut u64,
        published: &mut UiNode,
    ) -> io::Result<()> {
        while let Some(message) = bridge.poll().map_err(ui_bridge_error)? {
            match message {
                UiBridgeEvent::Action { event, .. } => {
                    let result = if event.base_revision != *revision {
                        Err(format!(
                            "Usage changed from revision {} to {}; retry the action",
                            event.base_revision, revision
                        ))
                    } else {
                        self.apply_semantic_action(
                            event.action.node_id.as_str(),
                            event.action.action.as_str(),
                            event.action.kind,
                            &event.action.value,
                            trigger,
                        )
                    };
                    let outcome = match result {
                        Ok(()) => {
                            self.publish_semantic_projection(bridge, revision, published)?;
                            UiEventOutcome::Applied
                        }
                        Err(message) => UiEventOutcome::Rejected(message),
                    };
                    bridge
                        .acknowledge(&event, outcome, *revision)
                        .map_err(ui_bridge_error)?;
                }
                UiBridgeEvent::Attached { .. }
                | UiBridgeEvent::Detached { .. }
                | UiBridgeEvent::Lifecycle { .. } => {}
            }
        }
        Ok(())
    }

    fn apply_semantic_action(
        &mut self,
        node_id: &str,
        action: &str,
        kind: UiEventKind,
        value: &UiEventValue,
        trigger: &mpsc::Sender<()>,
    ) -> Result<(), String> {
        if *value != UiEventValue::None {
            return Err("Usage navigation actions do not accept a value".to_string());
        }
        let page = ui::semantic_page(self.snapshot.as_ref(), &self.view());
        if !semantic_action_is_declared(&page, node_id, action, kind) {
            return Err("Action is not declared by the current Usage Page".to_string());
        }
        match (action, kind) {
            (ui::OPEN_PROVIDER_ACTION, UiEventKind::Activate) => {
                let index = ui::provider_index_from_node_id(node_id)
                    .ok_or_else(|| "Provider action has an invalid target".to_string())?;
                if index >= self.provider_count() {
                    return Err("Provider no longer exists".to_string());
                }
                self.select(index);
                self.open_detail();
                Ok(())
            }
            (ui::CLOSE_PROVIDER_ACTION, UiEventKind::Cancel) if node_id == ui::SEMANTIC_ROOT_ID => {
                self.close_detail();
                Ok(())
            }
            (ui::REFRESH_ACTION, UiEventKind::Activate) => {
                trigger
                    .send(())
                    .map_err(|_| "Usage scanner is no longer available".to_string())?;
                self.scanning = true;
                Ok(())
            }
            _ => Err("Action is not declared by the Usage Page".to_string()),
        }
    }

    /// Fold a fresh scan in: update the sidebar status line and emit a
    /// first-class informational alert when an opted-in edge fires.
    /// Every call is a silent no-op outside Unpeel.
    fn apply(&mut self, snapshot: Snapshot) {
        self.scanning = false;
        self.status.set_status(&snapshot.status_line());
        if self.hosted {
            let update = self.alert_tracker.update(&snapshot, self.config.alerts);
            if let Some(notification) = update.notification {
                self.status.alert("Usage alert", &notification);
            }
        }
        if snapshot.providers.is_empty() {
            self.selected = 0;
            self.detail_open = false;
            self.scroll_offset = 0;
        } else if self.selected >= snapshot.providers.len() {
            self.selected = snapshot.providers.len() - 1;
            self.detail_open = false;
            self.reveal_selected = true;
        }
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
            self.detail_open = false;
        }
        self.reveal_selected = true;
    }

    fn open_detail(&mut self) {
        if self.provider_count() == 0 {
            return;
        }
        self.detail_open = true;
        self.scroll_offset = 0;
        self.reveal_selected = false;
    }

    fn close_detail(&mut self) {
        self.detail_open = false;
        self.scroll_offset = 0;
        self.reveal_selected = true;
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

    fn scroll_down(&mut self, rows: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(rows).min(self.max_scroll);
        self.reveal_selected = false;
    }

    fn scroll_up(&mut self, rows: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(rows);
        self.reveal_selected = false;
    }

    fn scroll_to_bar_row(&mut self, row: u16, area: ratatui::layout::Rect) {
        let track = area.height.saturating_sub(1);
        let relative = row.saturating_sub(area.y).min(track);
        self.scroll_offset = if track == 0 {
            0
        } else {
            let numerator = u32::from(relative) * u32::from(self.max_scroll);
            let rounded = (numerator + u32::from(track) / 2) / u32::from(track);
            u16::try_from(rounded).unwrap_or(self.max_scroll)
        };
        self.reveal_selected = false;
    }

    fn open_alert_dialog(&mut self) {
        if self.hosted {
            self.alert_dialog = Some(0);
        }
    }

    fn move_alert_selection(&mut self, delta: isize) {
        let Some(selected) = self.alert_dialog else {
            return;
        };
        let last = AlertOption::ALL.len().saturating_sub(1);
        self.alert_dialog = Some(if delta < 0 {
            selected.saturating_sub(delta.unsigned_abs())
        } else {
            selected.saturating_add(delta as usize).min(last)
        });
    }

    fn toggle_alert_option(&mut self, index: usize) {
        let Some(option) = AlertOption::ALL.get(index).copied() else {
            return;
        };
        self.config.alerts.toggle(option);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.status.idle();
        self.status.flush();
    }
}

/// Restores the terminal even when drawing or input returns an error, or a
/// panic unwinds through the event loop.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}

fn run_tui(config: Config) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let _terminal_guard = TerminalGuard;
    let _keyboard = KeyboardEnhancementGuard::enter()?;
    // OSC 11 replies arrive on stdin; resolve after raw mode starts but before
    // crossterm's event reader has a chance to consume the response.
    let mut theme_monitor = ThemeMonitor::detected();
    let palette = theme::resolve_with_hosted_accent(config.theme, theme_monitor.hosted_accent());
    execute!(std::io::stdout(), EnableMouseCapture)?;

    let status = AppReporter::detect(install::APP_ID);
    let hosted = status.is_hosted();
    status.idle();
    let mut app = App {
        config,
        palette,
        status,
        snapshot: None,
        selected: 0,
        detail_open: false,
        scroll_offset: 0,
        max_scroll: 0,
        viewport_height: 0,
        reveal_selected: true,
        scanning: true,
        hosted,
        alert_dialog: None,
        alert_tracker: AlertTracker::default(),
        quit: false,
    };
    let mut bridge = UiBridge::detect(
        AppMetadata::new(install::APP_ID, "Unpeel Usage", env!("CARGO_PKG_VERSION")).description(
            "Standalone Ratatui usage dashboard with an optional native/web projection",
        ),
    )
    .map_err(ui_bridge_error)?;
    let mut ui_revision = 1u64;
    let mut published = app.semantic_node();
    bridge
        .publish(UI_VIEW_ID, ui_revision, published.clone())
        .map_err(ui_bridge_error)?;

    // Scans run off the UI thread so a large transcript sweep never blocks
    // a frame; the trigger channel doubles as the refresh timer.
    let (scan_tx, scan_rx) = mpsc::channel::<ScanEvent>();
    let (trigger_tx, trigger_rx) = mpsc::channel::<()>();
    let scan_config = app.config.clone();
    let refresh = Duration::from_secs(app.config.refresh_secs.max(5));
    std::thread::spawn(move || {
        let mut app_context = AppContext::detect();
        loop {
            if scan_tx.send(ScanEvent::Started).is_err() {
                return;
            }
            app_context.refresh();
            let project = current_project_root(&app_context);
            let snapshot = Snapshot::scan(&scan_config, project.as_deref());
            if scan_tx.send(ScanEvent::Finished(snapshot)).is_err() {
                return;
            }
            match trigger_rx.recv_timeout(refresh) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    while !app.quit {
        while let Ok(event) = scan_rx.try_recv() {
            match event {
                ScanEvent::Started => app.scanning = true,
                ScanEvent::Finished(snapshot) => app.apply(snapshot),
            }
        }
        app.drain_bridge(&mut bridge, &trigger_tx, &mut ui_revision, &mut published)?;
        app.publish_semantic_projection(&mut bridge, &mut ui_revision, &mut published)?;
        let view = app.view();
        let mut rendered = ui::RenderResult::default();
        let rendered_terminal = bridge.should_render_terminal();
        if rendered_terminal {
            terminal.draw(|frame| {
                rendered = ui::draw(frame, app.snapshot.as_ref(), &view, &app.palette);
            })?;
        }
        let hits = rendered.hits;
        if rendered_terminal {
            app.scroll_offset = rendered.scroll_offset;
            app.max_scroll = rendered.max_scroll;
            app.viewport_height = rendered.viewport_height;
            app.reveal_selected = false;
        }
        let scrollbar_area = rendered.scrollbar_area;
        let back_button = rendered.back_button;
        let alert_option_hits = rendered.alert_option_hits;
        let alert_dialog_area = rendered.alert_dialog_area;
        if !event::poll(Duration::from_millis(100))? {
            if theme_monitor.refresh() {
                app.palette
                    .apply_hosted_accent(theme_monitor.hosted_accent());
            }
            continue;
        }
        let read = event::read()?;
        match read {
            Event::Key(key) => {
                if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    continue;
                }
                if app.alert_dialog.is_some() {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        app.quit = true;
                    } else {
                        match key.code {
                            KeyCode::Char('j') | KeyCode::Down => app.move_alert_selection(1),
                            KeyCode::Char('k') | KeyCode::Up => app.move_alert_selection(-1),
                            KeyCode::Home | KeyCode::Char('g') => app.alert_dialog = Some(0),
                            KeyCode::End | KeyCode::Char('G') => {
                                app.alert_dialog = Some(AlertOption::ALL.len() - 1)
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                if let Some(selected) = app.alert_dialog {
                                    app.toggle_alert_option(selected);
                                }
                            }
                            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('a') => {
                                app.alert_dialog = None
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                match nav(&key) {
                    Some(Nav::Quit) => app.quit = true,
                    Some(Nav::Down) if app.detail_open => app.scroll_down(1),
                    Some(Nav::Down) => app.select_next(),
                    Some(Nav::Up) if app.detail_open => app.scroll_up(1),
                    Some(Nav::Up) => app.select_prev(),
                    Some(Nav::Top) if app.detail_open => app.scroll_up(u16::MAX),
                    Some(Nav::Top) => app.select(0),
                    Some(Nav::Bottom) if app.detail_open => app.scroll_down(u16::MAX),
                    Some(Nav::Bottom) => app.select(app.provider_count().saturating_sub(1)),
                    Some(Nav::Select) => {
                        if app.detail_open {
                            app.close_detail();
                        } else {
                            app.open_detail();
                        }
                    }
                    Some(Nav::Back) => {
                        if app.detail_open {
                            app.close_detail();
                        }
                    }
                    None => match key.code {
                        KeyCode::PageDown => {
                            app.scroll_down(app.viewport_height.saturating_sub(1).max(1))
                        }
                        KeyCode::PageUp => {
                            app.scroll_up(app.viewport_height.saturating_sub(1).max(1))
                        }
                        KeyCode::Char('r') => {
                            if trigger_tx.send(()).is_ok() {
                                app.scanning = true;
                            }
                        }
                        KeyCode::Char('a') => {
                            app.open_alert_dialog();
                        }
                        KeyCode::Char('t') => {
                            app.palette = app
                                .palette
                                .toggled()
                                .with_hosted_accent(theme_monitor.hosted_accent());
                        }
                        _ => {}
                    },
                }
            }
            Event::Mouse(mouse) if app.alert_dialog.is_some() => {
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    if let Some(hit) = alert_option_hits
                        .iter()
                        .find(|hit| hit.contains(mouse.column, mouse.row))
                    {
                        app.alert_dialog = Some(hit.index);
                        app.toggle_alert_option(hit.index);
                    } else if alert_dialog_area.is_some_and(|area| {
                        mouse.column < area.x
                            || mouse.column >= area.right()
                            || mouse.row < area.y
                            || mouse.row >= area.bottom()
                    }) {
                        app.alert_dialog = None;
                    }
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown => app.scroll_down(3),
                MouseEventKind::ScrollUp => app.scroll_up(3),
                // Click selects a row; a second click opens its detail view.
                MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left) => {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                        && back_button
                            .as_ref()
                            .is_some_and(|hit| hit.contains(mouse.column, mouse.row))
                    {
                        app.close_detail();
                    } else if let Some(area) = scrollbar_area.filter(|area| {
                        mouse.column >= area.x
                            && mouse.column < area.right()
                            && mouse.row >= area.y
                            && mouse.row < area.bottom()
                    }) {
                        app.scroll_to_bar_row(mouse.row, area);
                    } else if let Some(hit) = hits
                        .iter()
                        .find(|hit| hit.contains(mouse.column, mouse.row))
                    {
                        if hit.index == app.selected {
                            app.open_detail();
                        } else {
                            app.select(hit.index);
                        }
                    }
                }
                _ => {}
            },
            Event::Resize(_, _) => app.reveal_selected = true,
            _ => {}
        }
        app.publish_semantic_projection(&mut bridge, &mut ui_revision, &mut published)?;
    }
    Ok(())
}

fn ui_bridge_error(error: unpeel_app_kit::UiBridgeError) -> io::Error {
    io::Error::other(error.to_string())
}

fn semantic_action_is_declared(
    page: &unpeel_app_kit::Page,
    node_id: &str,
    action: &str,
    kind: UiEventKind,
) -> bool {
    (node_id == ui::SEMANTIC_ROOT_ID
        && kind == UiEventKind::Cancel
        && page.back.as_deref() == Some(action))
        || (kind == UiEventKind::Activate
            && page
                .list()
                .items
                .iter()
                .any(|item| item.id == node_id && item.activate.as_deref() == Some(action)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{Provider, ProviderKind};
    use unpeel_app_kit::{List, ListItem, Page};

    fn limit_snapshot(level: Level, used: f64, annotation: Option<&str>) -> Snapshot {
        let mut metric =
            Metric::used_percent("Weekly", used, format!("{used:.0}% · resets 2h"), level);
        metric.annotation = annotation.map(str::to_string);
        Snapshot {
            providers: vec![Provider {
                kind: ProviderKind::Claude,
                name: "Claude Code".into(),
                badge: String::new(),
                present: true,
                metrics: vec![metric],
                detail: Vec::new(),
                as_of: None,
                alert: None,
                status_fragment: None,
                day_usd: None,
                monthly_tokens: Vec::new(),
                project_usage: Vec::new(),
            }],
        }
    }

    #[test]
    fn close_and_reached_notifications_are_independent_edges() {
        let options = Alerts {
            close_to_limit: true,
            limit_reached: true,
            ..Alerts::default()
        };
        let mut tracker = AlertTracker::default();

        let close = tracker.update(&limit_snapshot(Level::Warn, 82.0, None), options);
        assert!(close
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("close to its limit")));

        let unchanged = tracker.update(&limit_snapshot(Level::Warn, 84.0, None), options);
        assert!(unchanged.notification.is_none());

        let reached = tracker.update(
            &limit_snapshot(Level::Alert, 100.0, Some("Limit reached")),
            options,
        );
        assert!(reached
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("limit reached")));
    }

    #[test]
    fn available_again_requires_a_real_recovery_transition() {
        let mut tracker = AlertTracker::default();
        let constrained = tracker.update(
            &limit_snapshot(Level::Alert, 96.0, Some("Running out")),
            Alerts::default(),
        );
        assert!(constrained.notification.is_none());

        let options = Alerts {
            available_again: true,
            ..Alerts::default()
        };
        let recovered = tracker.update(&limit_snapshot(Level::Ok, 3.0, None), options);
        assert!(recovered
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("available again")));

        let still_available = tracker.update(&limit_snapshot(Level::Ok, 4.0, None), options);
        assert!(still_available.notification.is_none());
    }

    #[test]
    fn paced_runout_counts_as_close_even_below_eighty_percent() {
        let metric = {
            let mut metric =
                Metric::used_percent("Weekly", 53.0, "53% · resets 3d".into(), Level::Alert);
            metric.annotation = Some("Limit in 56m".into());
            metric
        };
        assert_eq!(metric_limit_state(&metric), LimitState::Close);
    }

    #[test]
    fn semantic_actions_must_be_declared_on_the_current_page_and_node() {
        let page = Page::new(
            "Usage",
            List::new(
                "providers",
                vec![ListItem::new("provider-0", "Codex").activate_action(ui::OPEN_PROVIDER_ACTION)],
            ),
        );
        assert!(semantic_action_is_declared(
            &page,
            "provider-0",
            ui::OPEN_PROVIDER_ACTION,
            UiEventKind::Activate,
        ));
        assert!(!semantic_action_is_declared(
            &page,
            "forged-node",
            ui::OPEN_PROVIDER_ACTION,
            UiEventKind::Activate,
        ));
        assert!(!semantic_action_is_declared(
            &page,
            "refresh-usage",
            ui::REFRESH_ACTION,
            UiEventKind::Activate,
        ));
    }
}
