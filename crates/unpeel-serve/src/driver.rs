//! UI-free Host driver used by `unpeel serve`.
//!
//! Session state remains owned by the individual `unpeel-host` processes and
//! their journals. This process only rebuilds the same in-memory model the TUI
//! used to publish, owns the Controller transports while no native app does,
//! and can be restarted without affecting a running Session.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::activity::ActivityEngine;
use crate::approvals::ApprovalHub;
use crate::computer::ComputerAdapter;
use crate::hook_listener::{HookEventMessage, HookListener};
use crate::local_gateway::{LocalControlRequest, LocalGatewayServer};
use crate::mobile::{MobileResizes, MobileServer, SharedSnapshot};
use crate::pairing::PairingWindow;
use crate::platform_adapter::PlatformAdapterHub;
use crate::relay::RelayUplink;
use crate::sessions::{scan_sidebar, ScanCache, SessionRow, SidebarItem, SidebarModel, Status};

const LOOP_INTERVAL: Duration = Duration::from_millis(100);
const RESCAN_INTERVAL: Duration = Duration::from_secs(1);
const NATIVE_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LINK_RETRY_DELAY: Duration = Duration::from_secs(60);
const LINK_REJECTED_RETRY_DELAY: Duration = Duration::from_secs(15 * 60);
const STATUS_VERSION: u64 = 1;

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_shutdown(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServeEvent {
    Started {
        pid: u32,
        home: PathBuf,
        hook_port: u16,
    },
    LocalReady {
        socket: PathBuf,
    },
    DirectStarted {
        port: u16,
    },
    DirectStopped {
        reason: String,
    },
    LinkStarted,
    LinkStopped {
        reason: String,
    },
    /// Worker-owned `__remote__` terminal streamer lifecycle.
    Streamer(crate::remote_streamer::StreamerEvent),
    /// Worker-managed shared `__pty_core__` lifecycle (adopt/spawn/respawn;
    /// never stop).
    PtyCore(crate::pty_core_supervisor::CoreEvent),
    NativeAuthority {
        present: bool,
    },
    Warning(String),
    Stopped,
}

impl fmt::Display for ServeEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Started {
                pid,
                home,
                hook_port,
            } => write!(
                formatter,
                "Unpeel Host serving {} (pid {pid}, hook port {hook_port})",
                home.display()
            ),
            Self::DirectStarted { port } => {
                write!(
                    formatter,
                    "Direct Controller endpoint listening on port {port}"
                )
            }
            Self::LocalReady { socket } => write!(
                formatter,
                "Local Controller endpoint ready at {}",
                socket.display()
            ),
            Self::DirectStopped { reason } => {
                write!(formatter, "Direct Controller endpoint stopped: {reason}")
            }
            Self::LinkStarted => formatter.write_str("Unpeel Link uplink started"),
            Self::LinkStopped { reason } => write!(formatter, "Unpeel Link stopped: {reason}"),
            Self::Streamer(event) => {
                use crate::remote_streamer::StreamerEvent;
                match event {
                    StreamerEvent::Started { pid, restart: false } => {
                        write!(formatter, "terminal streamer started (pid {pid})")
                    }
                    StreamerEvent::Started { pid, restart: true } => {
                        write!(formatter, "terminal streamer respawned (pid {pid})")
                    }
                    StreamerEvent::Exited {
                        status,
                        rapid_failures,
                        gave_up: true,
                    } => write!(
                        formatter,
                        "terminal streamer exited ({status}) {rapid_failures} times in quick succession; giving up until pairing changes"
                    ),
                    StreamerEvent::Exited {
                        status,
                        rapid_failures,
                        gave_up: false,
                    } => write!(
                        formatter,
                        "terminal streamer exited ({status}, rapid failures {rapid_failures}); respawning in {}s",
                        crate::remote_streamer::RESTART_DELAY.as_secs()
                    ),
                    StreamerEvent::ReapedStale { pid } => write!(
                        formatter,
                        "terminal streamer pid {pid} verified stale via pid_started_at; sent SIGTERM"
                    ),
                    StreamerEvent::SpawnFailed { error } => {
                        write!(formatter, "terminal streamer could not start: {error}")
                    }
                }
            }
            Self::PtyCore(event) => {
                use crate::pty_core_supervisor::CoreEvent;
                match event {
                    CoreEvent::Adopted { pid, sessions } => write!(
                        formatter,
                        "PTY core adopted (pid {pid}, {sessions} sessions)"
                    ),
                    CoreEvent::Started { pid, restart: false } => {
                        write!(formatter, "PTY core started (pid {pid})")
                    }
                    CoreEvent::Started { pid, restart: true } => {
                        write!(formatter, "PTY core respawned (pid {pid})")
                    }
                    CoreEvent::Ready { pid, sessions } => write!(
                        formatter,
                        "PTY core live (pid {pid}, {sessions} sessions)"
                    ),
                    CoreEvent::Exited {
                        status,
                        rapid_failures,
                        gave_up: true,
                    } => write!(
                        formatter,
                        "PTY core exited ({status}) {rapid_failures} times in quick succession; giving up until the worker restarts"
                    ),
                    CoreEvent::Exited {
                        status,
                        rapid_failures,
                        gave_up: false,
                    } => write!(
                        formatter,
                        "PTY core exited ({status}, rapid failures {rapid_failures}); respawning in {}s",
                        crate::pty_core_supervisor::RESTART_DELAY.as_secs()
                    ),
                    CoreEvent::Lost { pid } => write!(
                        formatter,
                        "adopted PTY core pid {pid} is gone; respawning in {}s",
                        crate::pty_core_supervisor::RESTART_DELAY.as_secs()
                    ),
                    CoreEvent::SpawnFailed { error } => {
                        write!(formatter, "PTY core could not start: {error}")
                    }
                    CoreEvent::LeftRunning { pid, sessions } => write!(
                        formatter,
                        "worker stopping; PTY core pid {pid} left running ({} sessions) — terminals do not depend on the worker",
                        sessions.map_or("unknown".to_string(), |count| count.to_string())
                    ),
                    CoreEvent::TakeoverStarted { old_pid, new_pid } => write!(
                        formatter,
                        "PTY core pid {old_pid} runs an older build; takeover core pid {new_pid} is moving every session over"
                    ),
                    CoreEvent::TakenOver {
                        old_pid,
                        new_pid,
                        sessions,
                    } => write!(
                        formatter,
                        "PTY core taken over in place (pid {old_pid} -> {new_pid}, {sessions} sessions, no terminal restarted)"
                    ),
                    CoreEvent::Warning(message) => write!(formatter, "PTY core: {message}"),
                }
            }
            Self::NativeAuthority { present: true } => {
                formatter.write_str("compatibility Host detected; Controller serving handed off")
            }
            Self::NativeAuthority { present: false } => {
                formatter.write_str("canonical Host owns Controller serving")
            }
            Self::Warning(message) => write!(formatter, "warning: {message}"),
            Self::Stopped => formatter.write_str("Unpeel Host stopped"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeAuthority {
    Absent,
    Present,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ServeStatus<'a> {
    version: u64,
    pid: u32,
    started_at_unix_ms: u64,
    workspace_home: &'a Path,
    hook_port: u16,
    local_socket: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    direct_port: Option<u16>,
    link_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_streamer: Option<crate::remote_streamer::StreamerStatus>,
    /// Shared PTY core managed by this worker (additive; absent while the
    /// `UNPEEL_PTY_CORE` gate is off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pty_core: Option<crate::pty_core_supervisor::CoreStatus>,
    /// Host-owned Browser MCP engine install (additive, 2026-09-03):
    /// `{state: ready|installing|failed, version, path, error}`. Never a
    /// startup failure — the worker installs in a background thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    browser_engine: Option<unpeel_core::browser_engine::Status>,
    /// Host-owned Computer Use engine install (additive, 0.5.0):
    /// `{state: ready|installing|failed|missing|disabled, version, path,
    /// error}`. Installed on demand once Computer Use is turned on, never at
    /// bare start; `UNPEEL_COMPUTER_ENGINE_INSTALL=0` reports `disabled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    computer_engine: Option<unpeel_core::computer_engine::Status>,
    /// The Computer Use adapter's Controller-facing truth (additive, 0.5.0):
    /// the same `{computerUseAvailable, computerUseReady,
    /// computerUseUnavailableReason?}` the worker publishes in bootstrap, so
    /// a headless Host can be diagnosed from `serve.json` alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    computer_use: Option<serde_json::Value>,
    native_app_owns_controllers: bool,
    platform_capabilities: Vec<String>,
    /// Identity of the serving binary (additive, 0.4.0): a Controller
    /// bundled with a different `unpeel-host` compares these before
    /// attaching so an in-place app update never drives a stale worker.
    #[serde(skip_serializing_if = "Option::is_none")]
    executable: Option<PathBuf>,
    host_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_id: Option<String>,
}

struct ServeLease {
    _file: std::fs::File,
    status_path: PathBuf,
    pid: u32,
}

impl ServeLease {
    fn acquire() -> Result<Self, String> {
        let home = unpeel_core::app_paths::unpeel_home();
        std::fs::create_dir_all(&home)
            .map_err(|error| format!("could not create {}: {error}", home.display()))?;
        let lock_path = home.join("serve.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(&lock_path)
            .map_err(|error| format!("could not open {}: {error}", lock_path.display()))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err(format!(
                    "an Unpeel Host is already serving this workspace ({})",
                    home.display()
                ));
            }
            return Err(format!("could not lock {}: {error}", lock_path.display()));
        }
        Ok(Self {
            _file: file,
            status_path: home.join("serve.json"),
            pid: std::process::id(),
        })
    }

    fn publish(&self, status: &ServeStatus<'_>) -> Result<(), String> {
        let body = serde_json::to_vec_pretty(status).map_err(|error| error.to_string())?;
        let temporary = self.status_path.with_file_name(format!(
            ".serve.{}.{}.tmp",
            self.pid,
            uuid::Uuid::new_v4()
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&body)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.status_path)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!(
                "could not publish {}: {error}",
                self.status_path.display()
            ));
        }
        Ok(())
    }
}

impl Drop for ServeLease {
    fn drop(&mut self) {
        let belongs_to_us = std::fs::read(&self.status_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .and_then(|value| value.get("pid").and_then(serde_json::Value::as_u64))
            == Some(u64::from(self.pid));
        if belongs_to_us {
            let _ = std::fs::remove_file(&self.status_path);
        }
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// True only while another process holds the per-workspace serve lease.
/// Stale status/lock files never count as a live Host.
pub fn is_running() -> bool {
    is_running_at(&unpeel_core::app_paths::unpeel_home())
}

/// Path-addressed counterpart used by the machine Host-service supervisor.
/// It cannot change `UNPEEL_HOME` inside the supervisor process just to
/// inspect another workspace.
pub fn is_running_at(home: &Path) -> bool {
    let path = home.join("serve.lock");
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
    else {
        return false;
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
    }
}

struct NativeProbe {
    latest: Arc<Mutex<NativeAuthority>>,
}

impl NativeProbe {
    fn start(own_port: u16) -> Self {
        // Startup stays conservative: an unresolved first probe is treated as
        // a present compatibility Host so serving is never grabbed from a
        // possibly-live app; the first clean Absent probe flips it.
        let initial = match probe_native_authority(Some(own_port)) {
            ProbeObservation::Absent => NativeAuthority::Absent,
            ProbeObservation::Present | ProbeObservation::Unresolved => NativeAuthority::Present,
        };
        let latest = Arc::new(Mutex::new(initial));
        let worker_value = Arc::clone(&latest);
        std::thread::Builder::new()
            .name("unpeel-serve-native-probe".into())
            .spawn(move || {
                let mut published = initial;
                let mut consecutive_present = match initial {
                    NativeAuthority::Present => NATIVE_PRESENT_CONFIRMATIONS,
                    NativeAuthority::Absent => 0,
                };
                loop {
                    let observed = probe_native_authority(Some(own_port));
                    published =
                        resolve_probe_observation(published, &mut consecutive_present, observed);
                    if let Ok(mut guard) = worker_value.lock() {
                        *guard = published;
                    }
                    std::thread::sleep(NATIVE_PROBE_INTERVAL);
                }
            })
            .expect("spawn native ownership probe");
        Self { latest }
    }

    fn current(&self) -> NativeAuthority {
        self.latest
            .lock()
            .map(|guard| *guard)
            .unwrap_or(NativeAuthority::Present)
    }
}

/// One probe pass over the registered frontend ports. `Unresolved` means a
/// listener accepted the connection but stalled or answered garbage — that is
/// evidence of nothing (a busy hook server under load looks exactly like
/// this), so it must never flip ownership on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeObservation {
    Absent,
    Present,
    Unresolved,
}

/// Handing Controller serving off tears down Direct, Link, and every live
/// phone connection, so a compatibility Host must be confirmed by this many
/// consecutive probes before the worker lets go. Reclaiming (Absent) stays
/// immediate.
const NATIVE_PRESENT_CONFIRMATIONS: u32 = 3;

fn resolve_probe_observation(
    published: NativeAuthority,
    consecutive_present: &mut u32,
    observed: ProbeObservation,
) -> NativeAuthority {
    match observed {
        ProbeObservation::Absent => {
            *consecutive_present = 0;
            NativeAuthority::Absent
        }
        ProbeObservation::Present => {
            *consecutive_present = consecutive_present.saturating_add(1);
            if *consecutive_present >= NATIVE_PRESENT_CONFIRMATIONS {
                NativeAuthority::Present
            } else {
                published
            }
        }
        ProbeObservation::Unresolved => published,
    }
}

struct ProbeResponse {
    status: u16,
    frontend: Option<String>,
    controller_owner: Option<String>,
}

enum ProbeResult {
    Unreachable,
    Unresolved,
    Response(ProbeResponse),
}

fn candidate_frontend_ports() -> Vec<u16> {
    let mut ports =
        std::fs::read_to_string(unpeel_core::app_paths::unpeel_home().join("app-ports"))
            .unwrap_or_default()
            .lines()
            .rev()
            .filter_map(|line| line.trim().parse::<u16>().ok())
            .collect::<Vec<_>>();
    ports.dedup();
    ports
}

fn request_probe(port: u16, path: &str, token: &str) -> ProbeResult {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return ProbeResult::Unreachable;
    };
    if stream.set_read_timeout(Some(PROBE_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(PROBE_TIMEOUT)).is_err()
    {
        return ProbeResult::Unresolved;
    }
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nx-unpeel-auth: {token}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return ProbeResult::Unresolved;
    }
    let mut response = Vec::new();
    if stream.read_to_end(&mut response).is_err() {
        return ProbeResult::Unresolved;
    }
    ProbeResult::Response(parse_probe_response(&response))
}

fn parse_probe_response(response: &[u8]) -> ProbeResponse {
    let response = String::from_utf8_lossy(response);
    let head = response
        .split_once("\r\n\r\n")
        .map_or(response.as_ref(), |v| v.0);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let header = |wanted: &str| {
        head.lines().skip(1).find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(wanted)
                .then(|| value.trim().to_ascii_lowercase())
        })
    };
    let frontend = header("x-unpeel-frontend");
    let controller_owner = header("x-unpeel-controller-owner");
    ProbeResponse {
        status,
        frontend,
        controller_owner,
    }
}

fn response_is_non_owning_frontend(response: &ProbeResponse) -> bool {
    response.controller_owner.as_deref() == Some("serve")
        || matches!(response.frontend.as_deref(), Some("tui" | "serve"))
}

fn probe_native_authority(own_port: Option<u16>) -> ProbeObservation {
    let token = std::fs::read_to_string(
        unpeel_core::app_paths::unpeel_home()
            .join("mcp")
            .join("auth-token"),
    )
    .unwrap_or_default();
    for port in candidate_frontend_ports() {
        if Some(port) == own_port {
            continue;
        }
        let response = match request_probe(port, "/mcp/sidebar", token.trim()) {
            ProbeResult::Unreachable => continue,
            ProbeResult::Unresolved => return ProbeObservation::Unresolved,
            ProbeResult::Response(response) => response,
        };
        if response_is_non_owning_frontend(&response) {
            continue;
        }
        if response.status == 404 {
            match request_probe(port, "/mcp/list-presets", token.trim()) {
                ProbeResult::Response(ProbeResponse {
                    status: 404,
                    frontend: None,
                    controller_owner: None,
                }) => continue,
                ProbeResult::Response(response) if response_is_non_owning_frontend(&response) => {
                    continue
                }
                ProbeResult::Unreachable | ProbeResult::Unresolved => {
                    return ProbeObservation::Unresolved
                }
                ProbeResult::Response(_) => return ProbeObservation::Present,
            }
        }
        return ProbeObservation::Present;
    }
    ProbeObservation::Absent
}

enum LinkRefreshResult {
    StoredKey {
        key: String,
        result: Result<
            unpeel_core::license::PendingRelayEntitlement,
            unpeel_core::license::RelayEntitlementError,
        >,
    },
    NativeKeychain {
        generation: u64,
        mac_id: String,
        result: Result<
            crate::platform_adapter::PlatformAdapterResponse,
            crate::platform_adapter::PlatformAdapterError,
        >,
    },
}

enum OverlayRefreshResult {
    Finished {
        generation: u64,
        result: Result<crate::overlay::NativeOverlay, String>,
    },
}

/// Canonical per-workspace Host engine.
///
/// `unpeel serve` owns one of these in a foreground process. Native and other
/// launchers should drive this same runtime with platform capability adapters
/// instead of implementing a parallel serving loop.
pub struct HostRuntime {
    lease: ServeLease,
    home: PathBuf,
    started_at_unix_ms: u64,
    hook_port: u16,
    hook_events: mpsc::Receiver<HookEventMessage>,
    local_gateway: LocalGatewayServer,
    local_controls: mpsc::Receiver<LocalControlRequest>,
    platform_adapters: Arc<PlatformAdapterHub>,
    platform_adapter_generation: u64,
    native_probe: NativeProbe,
    native_authority: NativeAuthority,
    engine: ActivityEngine,
    scan_cache: ScanCache,
    model: SidebarModel,
    overlay: crate::overlay::SharedNativeOverlay,
    overlay_refresh_rx: Option<mpsc::Receiver<OverlayRefreshResult>>,
    overlay_refresh_pending: bool,
    activity_log: unpeel_core::activity_log::ActivityLogStore,
    unread_ids: HashSet<String>,
    /// Host-owned auto-stop-and-archive sweep (`auto_archive.rs`).
    auto_archive: crate::auto_archive::Sweeper,
    last_activity_state_signature: Option<crate::activity_snapshot::ActivityStateSignature>,
    snapshot: SharedSnapshot,
    approvals: Arc<ApprovalHub>,
    approval_sync_generation: (u64, u64),
    pairing: Arc<PairingWindow>,
    presence: Arc<crate::presence::PresenceHub>,
    pairing_requested: bool,
    pairing_completion_recorded: bool,
    computer: ComputerAdapter,
    resizes: MobileResizes,
    mark_read_tx: mpsc::Sender<String>,
    mark_read_rx: mpsc::Receiver<String>,
    mobile_server: Option<MobileServer>,
    relay_uplink: Option<RelayUplink>,
    /// Paired-device set the streamer supervisor last saw; a change lifts a
    /// crash-loop hold.
    streamer_device_signature: String,
    /// Last published streamer status, so `serve.json` republishes on change.
    streamer_status: Option<crate::remote_streamer::StreamerStatus>,
    /// Worker-managed shared PTY core (gate off → inert).
    pty_core: crate::pty_core_supervisor::PtyCoreSupervisor,
    pty_core_status: Option<crate::pty_core_supervisor::CoreStatus>,
    /// Background engine install started at boot; `None` once resolved.
    browser_engine_rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    browser_engine_status: unpeel_core::browser_engine::Status,
    link_refresh_rx: Option<mpsc::Receiver<LinkRefreshResult>>,
    link_refresh_retry_at: Instant,
    last_scan: Instant,
    status_dirty: bool,
}

impl HostRuntime {
    pub fn start() -> Result<(Self, Vec<ServeEvent>), String> {
        let lease = ServeLease::acquire()?;
        let home = unpeel_core::app_paths::unpeel_home();
        let started_at_unix_ms = now_ms();
        unpeel_core::relay_uplink::ensure_host_id()?;
        seed_blank_home();

        let platform_adapters = Arc::new(PlatformAdapterHub::default());
        let overlay = crate::overlay::SharedNativeOverlay::new(crate::overlay::load());
        let approvals = Arc::new(ApprovalHub::default());
        let pairing = Arc::new(PairingWindow::default());
        let presence = Arc::new(crate::presence::PresenceHub::new(&home));
        let HookListener { port, events } = crate::hook_listener::start_with_platform(
            Arc::clone(&approvals),
            overlay.clone(),
            Arc::clone(&platform_adapters),
        )?;
        unpeel_core::session_ops::set_own_listener_port(port);
        let mut computer = ComputerAdapter::new(&home, Arc::clone(&platform_adapters));
        let (local_control_tx, local_controls) = mpsc::channel();
        let native_probe = NativeProbe::start(port);
        let native_authority = native_probe.current();
        let overlay_snapshot = overlay.snapshot();
        let mut engine = ActivityEngine::default();
        let mut scan_cache = ScanCache::default();
        let model = scan_sidebar(
            &mut engine,
            overlay_snapshot.as_ref(),
            &HashSet::new(),
            &mut scan_cache,
        );
        let activity_log =
            unpeel_core::activity_log::ActivityLogStore::load_default().unwrap_or_default();
        let persisted_unread =
            crate::activity_snapshot::load_unread(&unpeel_core::app_paths::activity_state_path());
        let unread_ids = derive_unread(&model, &activity_log, &persisted_unread);
        computer.reconcile();
        let mut initial_snapshot = crate::sessions::mobile_snapshot(
            &model,
            overlay_snapshot.as_ref(),
            &unread_ids,
            Some(&activity_log),
        );
        computer.decorate_workspace_settings(&mut initial_snapshot.bootstrap);
        let snapshot = Arc::new(Mutex::new(initial_snapshot));
        let (mark_read_tx, mark_read_rx) = mpsc::channel();
        let local_gateway = LocalGatewayServer::start(
            &home,
            port,
            local_control_tx,
            Arc::clone(&platform_adapters),
            computer.shared_status(),
            Arc::clone(&approvals),
            Arc::clone(&pairing),
            Arc::clone(&snapshot),
        )?;
        let local_socket = local_gateway.path().to_path_buf();
        let (pty_core, pty_core_events) =
            crate::pty_core_supervisor::PtyCoreSupervisor::from_env(home.clone());
        let mut driver = Self {
            lease,
            home: home.clone(),
            started_at_unix_ms,
            hook_port: port,
            hook_events: events,
            local_gateway,
            local_controls,
            platform_adapters,
            platform_adapter_generation: 0,
            native_probe,
            native_authority,
            engine,
            scan_cache,
            model,
            overlay,
            overlay_refresh_rx: None,
            overlay_refresh_pending: false,
            activity_log,
            unread_ids,
            last_activity_state_signature: None,
            snapshot,
            approvals,
            approval_sync_generation: (u64::MAX, u64::MAX),
            pairing,
            presence,
            pairing_requested: false,
            pairing_completion_recorded: false,
            computer,
            resizes: Arc::new(Mutex::new(HashMap::new())),
            mark_read_tx,
            mark_read_rx,
            mobile_server: None,
            relay_uplink: None,
            streamer_device_signature: crate::mobile::paired_device_signature(),
            streamer_status: None,
            pty_core,
            pty_core_status: None,
            auto_archive: crate::auto_archive::Sweeper::default(),
            browser_engine_rx: if browser_engine_install_enabled() {
                Some(spawn_browser_engine_install(&home))
            } else {
                None
            },
            browser_engine_status: if browser_engine_install_enabled() {
                unpeel_core::browser_engine::Status::installing()
            } else {
                unpeel_core::browser_engine::Status::disabled()
            },
            link_refresh_rx: None,
            link_refresh_retry_at: Instant::now(),
            last_scan: Instant::now() - RESCAN_INTERVAL,
            status_dirty: true,
        };
        let mut emitted = vec![
            ServeEvent::Started {
                pid: std::process::id(),
                home,
                hook_port: port,
            },
            ServeEvent::LocalReady {
                socket: local_socket,
            },
        ];
        emitted.extend(pty_core_events.into_iter().map(ServeEvent::PtyCore));
        emitted.extend(driver.tick());
        Ok((driver, emitted))
    }

    pub fn direct_port(&self) -> Option<u16> {
        self.mobile_server.as_ref().map(|server| server.port)
    }

    pub fn tick(&mut self) -> Vec<ServeEvent> {
        let mut emitted = Vec::new();
        let mut dirty = false;
        self.handle_local_controls(&mut emitted);
        self.poll_browser_engine_install();
        let platform_generation = self.platform_adapters.generation();
        if platform_generation != self.platform_adapter_generation {
            self.platform_adapter_generation = platform_generation;
            self.request_overlay_refresh(platform_generation);
            self.call_platform_maintenance(
                "mobile.e2e-key.reconcile",
                serde_json::json!({ "action": "sync" }),
            );
            dirty = true;
            self.status_dirty = true;
        }
        dirty |= self.reconcile_overlay_refresh();
        self.sync_platform_approvals(platform_generation);
        // Adapter or engine-install changes republish both the snapshot and
        // serve.json (the latter carries `computerEngine`).
        let computer_changed = self.computer.reconcile();
        dirty |= computer_changed;
        self.status_dirty |= computer_changed;
        while let Ok(message) = self.hook_events.try_recv() {
            if message.is_state_change() {
                self.request_overlay_refresh(self.platform_adapter_generation);
                dirty = true;
                continue;
            }
            if let Some(alert) = message.app_alert {
                if !self.native_app_owns_controllers() {
                    self.record_alert(&message.session_id, alert.body, message.received_at);
                }
                dirty = true;
                continue;
            }
            let canonical = crate::activity::normalize_event_name(&message.event_name);
            let runtime = runtime_launch_metadata(&message.session_id);
            let accepted = if let Some(generation) = runtime.0 {
                self.engine.apply_hook_event_for_runtime(
                    &message.session_id,
                    &canonical,
                    message.tool_name.as_deref(),
                    message.received_at,
                    message.runtime_generation,
                    generation,
                    runtime.1,
                )
            } else {
                self.engine.apply_hook_event(
                    &message.session_id,
                    &canonical,
                    message.tool_name.as_deref(),
                    message.received_at,
                );
                true
            };
            dirty |= accepted;
        }
        while let Ok(session_id) = self.mark_read_rx.try_recv() {
            let _ = unpeel_core::session_ops::mark_read(&session_id);
            self.unread_ids.remove(&session_id);
            dirty = true;
        }
        if self.pairing.completed() && !self.pairing_completion_recorded {
            if let Some(port) = self.direct_port() {
                crate::mobile::remember_paired_port(port, false);
            }
            self.invalidate_link_registrations(&mut emitted, "Controller pairing changed");
            self.call_platform_maintenance(
                "mobile.e2e-key.reconcile",
                serde_json::json!({ "action": "sync" }),
            );
            self.pairing_completion_recorded = true;
            dirty = true;
        }

        let observed_native = self.native_probe.current();
        if observed_native != self.native_authority {
            self.native_authority = observed_native;
            emitted.push(ServeEvent::NativeAuthority {
                present: observed_native == NativeAuthority::Present,
            });
            dirty = true;
            self.status_dirty = true;
        }

        if dirty || self.last_scan.elapsed() >= RESCAN_INTERVAL {
            self.rescan();
            self.last_scan = Instant::now();
            self.reconcile_direct(&mut emitted);
            self.sweep_auto_archive(&mut emitted);
        }
        self.reconcile_link(&mut emitted);
        self.supervise_streamer(&mut emitted);
        self.supervise_pty_core(&mut emitted);
        if self.status_dirty {
            if let Err(error) = self.publish_status() {
                emitted.push(ServeEvent::Warning(error));
            }
            self.status_dirty = false;
        }
        emitted
    }

    /// Refresh native-only defaults without ever blocking the Host tick. A
    /// scoped worker has no safe `defaults` domain to shell out to, so the
    /// live adapter is authoritative while registered; on disconnect the
    /// historical default-workspace loader is restored for compatibility.
    fn request_overlay_refresh(&mut self, generation: u64) {
        if !self.platform_adapters.supports("overlay.snapshot") {
            self.overlay_refresh_rx = None;
            self.overlay_refresh_pending = false;
            self.overlay.replace(crate::overlay::load());
            return;
        }
        if self.overlay_refresh_rx.is_some() {
            self.overlay_refresh_pending = true;
            return;
        }
        let adapters = Arc::clone(&self.platform_adapters);
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("unpeel-serve-overlay".into())
            .spawn(move || {
                let result = adapters
                    .call("overlay.snapshot", serde_json::json!({}))
                    .map_err(|error| error.to_string())
                    .and_then(|response| {
                        if response.status != 200 {
                            return Err(format!(
                                "native overlay returned status {}",
                                response.status
                            ));
                        }
                        crate::overlay::from_adapter_response(&response.body)
                    });
                let _ = sender.send(OverlayRefreshResult::Finished { generation, result });
            })
            .ok();
        self.overlay_refresh_rx = Some(receiver);
    }

    fn reconcile_overlay_refresh(&mut self) -> bool {
        let received = match self.overlay_refresh_rx.as_ref() {
            Some(receiver) => receiver.try_recv(),
            None => return false,
        };
        let OverlayRefreshResult::Finished { generation, result } = match received {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.overlay_refresh_rx = None;
                self.overlay_refresh_pending = false;
                return false;
            }
        };
        self.overlay_refresh_rx = None;
        let accepted = generation == self.platform_adapters.generation()
            && self.platform_adapters.supports("overlay.snapshot");
        let changed = if accepted {
            match result {
                Ok(next) => {
                    let changed = self.overlay.snapshot().as_ref() != Some(&next);
                    self.overlay.replace(Some(next));
                    changed
                }
                Err(error) => {
                    crate::tracelog::trace(
                        "platform-adapter",
                        &format!("overlay.snapshot rejected: {error}"),
                    );
                    false
                }
            }
        } else {
            false
        };
        let pending = std::mem::take(&mut self.overlay_refresh_pending);
        if pending && self.platform_adapters.supports("overlay.snapshot") {
            self.request_overlay_refresh(self.platform_adapters.generation());
        }
        changed
    }

    /// Fire-and-forget mirror/enrichment maintenance. The Host has already
    /// committed the authoritative resource mutation; these callbacks only
    /// keep a native platform store aligned and can safely retry after the
    /// next registration or state change.
    fn call_platform_maintenance(&self, operation: &'static str, request: serde_json::Value) {
        if !self.platform_adapters.supports(operation) {
            return;
        }
        let adapters = Arc::clone(&self.platform_adapters);
        let _ = std::thread::Builder::new()
            .name("unpeel-serve-platform-maintenance".into())
            .spawn(move || {
                let _ = adapters.call(operation, request);
            });
    }

    /// Once per rescan: archive Sessions idle past the workspace cutoff. The
    /// setting is re-read from disk each time so a `unpeel settings set` or
    /// an app edit applies at the next sweep without a restart.
    fn sweep_auto_archive(&mut self, emitted: &mut Vec<ServeEvent>) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0);
        let minutes = crate::auto_archive::minutes_from_disk();
        for event in self
            .auto_archive
            .step(&self.model.rows, &self.unread_ids, minutes, now_ms)
        {
            match event {
                crate::auto_archive::SweepEvent::Archived(session_id) => {
                    crate::tracelog::trace(
                        "host-worker",
                        &format!("auto-archived idle session {session_id} after {minutes} minutes"),
                    );
                    self.status_dirty = true;
                }
                crate::auto_archive::SweepEvent::Failed { session_id, error } => {
                    emitted.push(ServeEvent::Warning(format!(
                        "auto-archive of {session_id} failed: {error}"
                    )));
                }
                crate::auto_archive::SweepEvent::Skipped { session_id, reason } => {
                    crate::tracelog::trace(
                        "host-worker",
                        &format!("auto-archive skips {session_id}: {reason}"),
                    );
                }
            }
        }
    }

    fn rescan(&mut self) {
        let overlay = self.overlay.snapshot();
        let canonical_activity_owner = !self.native_app_owns_controllers();
        let previous = self
            .model
            .rows
            .iter()
            .map(|row| (row.id.clone(), row.status))
            .collect::<HashMap<_, _>>();
        let keep_visible = self.unread_ids.clone();
        self.model = scan_sidebar(
            &mut self.engine,
            overlay.as_ref(),
            &keep_visible,
            &mut self.scan_cache,
        );
        let _ = self.activity_log.refresh();
        let now = now_ms();
        self.presence.prune(now);
        let mut events = Vec::new();
        for row in &self.model.rows {
            let old = previous.get(&row.id).copied();
            let kind = if old.is_none() && row.running {
                Some(unpeel_core::activity_log::ActivityLogKind::Started)
            } else if row.status == Status::Attention && old != Some(Status::Attention) {
                Some(unpeel_core::activity_log::ActivityLogKind::NeedsInput)
            } else if row.status == Status::Idle
                && matches!(
                    old,
                    Some(Status::Starting | Status::Busy | Status::Attention)
                )
            {
                Some(unpeel_core::activity_log::ActivityLogKind::Finished)
            } else if row.status == Status::Exited && old.is_some_and(|old| old != Status::Exited) {
                Some(unpeel_core::activity_log::ActivityLogKind::Exited)
            } else {
                None
            };
            if let Some(kind) = kind {
                events.push((row.clone(), kind));
            }
        }
        if canonical_activity_owner {
            for (row, kind) in events {
                self.append_activity(&row, kind, now, None);
                let observed = match kind {
                    unpeel_core::activity_log::ActivityLogKind::NeedsInput => self
                        .deliver_notification(
                            &row,
                            crate::notifications::NotificationKind::NeedsInput,
                            None,
                            false,
                            now,
                        ),
                    unpeel_core::activity_log::ActivityLogKind::Finished => self
                        .deliver_notification(
                            &row,
                            crate::notifications::NotificationKind::Done,
                            None,
                            true,
                            now,
                        ),
                    _ => false,
                };
                if matches!(
                    kind,
                    unpeel_core::activity_log::ActivityLogKind::NeedsInput
                        | unpeel_core::activity_log::ActivityLogKind::Finished
                ) {
                    if observed {
                        // A Controller rendering the terminal at the edge is
                        // observing it. Advance the shared receipt once so a
                        // later presence expiry cannot resurrect stale unread.
                        let _ = unpeel_core::session_ops::mark_read(&row.id);
                        self.unread_ids.remove(&row.id);
                    } else {
                        self.unread_ids.insert(row.id.clone());
                    }
                }
            }
        }
        let mut unread_claims = self.unread_ids.clone();
        unread_claims.extend(crate::activity_snapshot::load_unread(
            &unpeel_core::app_paths::activity_state_path(),
        ));
        self.unread_ids = derive_unread(&self.model, &self.activity_log, &unread_claims);
        if canonical_activity_owner {
            if let Err(error) = crate::activity_snapshot::publish(
                &unpeel_core::app_paths::activity_state_path(),
                &self.model,
                &self.unread_ids,
                &self.engine,
                now,
                &mut self.last_activity_state_signature,
            ) {
                crate::tracelog::trace(
                    "activity",
                    &format!("activity-state publish failed: {error}"),
                );
            }
        } else {
            // A compatibility frontend may replace the file while it owns
            // lifecycle. Force a complete canonical write on takeover.
            self.last_activity_state_signature = None;
        }
        let mut next = crate::sessions::mobile_snapshot(
            &self.model,
            overlay.as_ref(),
            &self.unread_ids,
            Some(&self.activity_log),
        );
        self.computer
            .decorate_workspace_settings(&mut next.bootstrap);
        if let Ok(mut snapshot) = self.snapshot.lock() {
            *snapshot = next;
        }
    }

    fn reconcile_direct(&mut self, emitted: &mut Vec<ServeEvent>) {
        let paired = crate::mobile::paired_device_count();
        let must_yield = self.native_app_owns_controllers();
        if (paired == 0 && !self.pairing_requested && !self.pairing.is_open()) || must_yield {
            if let Some(uplink) = self.relay_uplink.take() {
                uplink.stop();
                emitted.push(ServeEvent::LinkStopped {
                    reason: if paired == 0 {
                        "no paired Controllers".into()
                    } else {
                        "Mac app owns Controller serving".into()
                    },
                });
            }
            if let Some(server) = self.mobile_server.take() {
                server.stop();
                emitted.push(ServeEvent::DirectStopped {
                    reason: if paired == 0 {
                        "no paired Controllers".into()
                    } else {
                        "Mac app owns Controller serving".into()
                    },
                });
                if let Ok(mut resizes) = self.resizes.lock() {
                    resizes.clear();
                }
                self.status_dirty = true;
            }
            return;
        }

        if self
            .mobile_server
            .as_ref()
            .is_some_and(|server| !server.owns_configured_endpoint())
        {
            // A released native build can temporarily rewrite the endpoint.
            // Only repair it after the native probe has proven no app owns
            // serving; Link remains stopped across the mismatch.
            if let Some(uplink) = self.relay_uplink.take() {
                uplink.stop();
                emitted.push(ServeEvent::LinkStopped {
                    reason: "Direct endpoint ownership changed".into(),
                });
            }
            let restored = self
                .mobile_server
                .as_ref()
                .is_some_and(MobileServer::restore_legacy_configured_endpoint);
            if !restored {
                if let Some(server) = self.mobile_server.take() {
                    server.stop();
                }
                emitted.push(ServeEvent::DirectStopped {
                    reason: "configured endpoint ownership changed".into(),
                });
                self.status_dirty = true;
            }
        }
        if self.mobile_server.is_none() {
            self.mobile_server = crate::mobile::start_with_runtime(
                Arc::clone(&self.snapshot),
                self.mark_read_tx.clone(),
                Some(self.hook_port),
                Arc::clone(&self.resizes),
                Arc::clone(&self.approvals),
                Arc::clone(&self.pairing),
                Arc::clone(&self.platform_adapters),
                Arc::clone(&self.presence),
            );
            if let Some(server) = &self.mobile_server {
                emitted.push(ServeEvent::DirectStarted { port: server.port });
                self.status_dirty = true;
            }
        }
    }

    fn handle_local_controls(&mut self, emitted: &mut Vec<ServeEvent>) {
        while let Ok(request) = self.local_controls.try_recv() {
            match request {
                LocalControlRequest::BeginPairing {
                    advertised_host,
                    advertised_port,
                    reply,
                } => {
                    let result = self.begin_local_pairing(
                        advertised_host.as_deref(),
                        advertised_port,
                        emitted,
                    );
                    let _ = reply.send(result);
                }
                LocalControlRequest::PairingStatus { reply } => {
                    let status = if self.pairing.completed() {
                        "completed"
                    } else if self.pairing.is_open() {
                        "active"
                    } else {
                        "closed"
                    };
                    let _ = reply.send(Ok(serde_json::json!({ "status": status })));
                }
                LocalControlRequest::CancelPairing { reply } => {
                    self.pairing.cancel();
                    self.pairing_requested = false;
                    let _ = reply.send(Ok(serde_json::json!({ "status": "closed" })));
                }
                LocalControlRequest::ListDevices { reply } => {
                    let result = crate::pairing::device_summaries().map(|devices| {
                        let direct_endpoint = self.direct_port().map(|port| {
                            format!(
                                "http://{}:{port}/mobile",
                                crate::mobile::preferred_lan_address()
                            )
                        });
                        serde_json::json!({
                            "devices": devices,
                            "directEndpoint": direct_endpoint,
                        })
                    });
                    let _ = reply.send(result);
                }
                LocalControlRequest::RevokeDevice { device_id, reply } => {
                    let keychain_device_id = device_id.clone();
                    let result = crate::pairing::unpair_device(&device_id).map(|()| {
                        self.invalidate_link_registrations(
                            emitted,
                            "Controller authorization changed",
                        );
                        self.call_platform_maintenance(
                            "mobile.e2e-key.reconcile",
                            serde_json::json!({
                                "action": "remove",
                                "deviceID": keychain_device_id,
                            }),
                        );
                        self.reconcile_direct(emitted);
                        serde_json::json!({ "ok": true })
                    });
                    let _ = reply.send(result);
                }
                LocalControlRequest::SetDeviceRelayAllowed {
                    device_id,
                    allowed,
                    reply,
                } => {
                    let result =
                        crate::pairing::set_device_relay_allowed(&device_id, allowed).map(|()| {
                            self.invalidate_link_registrations(
                                emitted,
                                "Controller Link scope changed",
                            );
                            serde_json::json!({ "ok": true })
                        });
                    let _ = reply.send(result);
                }
            }
        }
    }

    /// Every tick: step the worker-owned `__remote__` supervisor, lift a
    /// crash-loop hold when the paired-device set changes, and republish
    /// `serve.json` whenever the streamer's published state moves.
    fn supervise_streamer(&mut self, emitted: &mut Vec<ServeEvent>) {
        let Some(server) = self.mobile_server.as_ref() else {
            if self.streamer_status.take().is_some() {
                self.status_dirty = true;
            }
            return;
        };
        let mut events = server.supervise_streamer();
        let signature = crate::mobile::paired_device_signature();
        if signature != self.streamer_device_signature {
            self.streamer_device_signature = signature;
            events.extend(server.retry_streamer_after_pairing_change());
        }
        for event in events {
            emitted.push(ServeEvent::Streamer(event));
        }
        let status = server.streamer_status();
        if status != self.streamer_status {
            self.streamer_status = status;
            self.status_dirty = true;
        }
    }

    /// Every tick: step the PTY core supervisor and republish `serve.json`
    /// when its published state moves. The supervisor never stops a core.
    fn supervise_pty_core(&mut self, emitted: &mut Vec<ServeEvent>) {
        for event in self.pty_core.poll() {
            emitted.push(ServeEvent::PtyCore(event));
        }
        let status = self.pty_core.status();
        if status != self.pty_core_status {
            self.pty_core_status = status;
            self.status_dirty = true;
        }
    }

    /// Events to report when the worker is stopping: the PTY core is left
    /// running on purpose (terminals never depend on the worker's lifetime).
    pub fn shutdown_events(&self) -> Vec<ServeEvent> {
        self.pty_core
            .leave_running()
            .into_iter()
            .map(ServeEvent::PtyCore)
            .collect()
    }

    fn invalidate_link_registrations(&mut self, emitted: &mut Vec<ServeEvent>, reason: &str) {
        if let Some(uplink) = self.relay_uplink.take() {
            uplink.stop();
            emitted.push(ServeEvent::LinkStopped {
                reason: reason.to_owned(),
            });
            self.status_dirty = true;
        }
    }

    fn begin_local_pairing(
        &mut self,
        advertised_host: Option<&str>,
        advertised_port: Option<u16>,
        emitted: &mut Vec<ServeEvent>,
    ) -> Result<serde_json::Value, String> {
        if self.native_app_owns_controllers() {
            return Err(
                "the Mac app owns pairing for this workspace; pair from Settings ▸ Remote".into(),
            );
        }
        self.pairing_requested = true;
        self.reconcile_direct(emitted);
        self.pairing_requested = false;
        let port = self
            .direct_port()
            .ok_or("the workspace Host could not open its pairing endpoint")?;
        let pairing_port = advertised_port.unwrap_or(port);
        let host = advertised_host
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(crate::mobile::preferred_lan_address);
        let mac_id = unpeel_core::relay_uplink::ensure_host_id()?;
        let (code, _) = self
            .pairing
            .begin(&host, pairing_port, &mac_id)
            .ok_or("could not open a pairing window")?;
        self.pairing_completion_recorded = false;
        Ok(serde_json::json!({ "code": code, "status": "active" }))
    }

    fn owns_controller_serving(&self) -> bool {
        !self.native_app_owns_controllers()
            && self
                .mobile_server
                .as_ref()
                .is_some_and(MobileServer::owns_configured_endpoint)
    }

    fn native_app_owns_controllers(&self) -> bool {
        self.native_authority == NativeAuthority::Present
            && !self
                .platform_adapters
                .supports("controller.transport.host-owned")
    }

    fn reconcile_link(&mut self, emitted: &mut Vec<ServeEvent>) {
        if let Some(receiver) = &self.link_refresh_rx {
            match receiver.try_recv() {
                Ok(LinkRefreshResult::StoredKey { key, result }) => {
                    self.link_refresh_rx = None;
                    let request_is_current = self.owns_controller_serving()
                        && unpeel_core::license::stored().is_some_and(|(stored, _)| stored == key);
                    match result {
                        Ok(pending) if request_is_current => {
                            match unpeel_core::license::commit_relay_entitlement_for_key(
                                &key, &pending,
                            ) {
                                Ok(()) => self.link_refresh_retry_at = Instant::now(),
                                Err(error) => {
                                    self.link_refresh_retry_at = Instant::now() + LINK_RETRY_DELAY;
                                    emitted.push(ServeEvent::Warning(format!(
                                        "Link authorization could not be saved: {error}"
                                    )));
                                }
                            }
                        }
                        Err(error) if request_is_current && error.is_rejected() => {
                            if let Some(uplink) = self.relay_uplink.take() {
                                uplink.stop();
                            }
                            let _ = unpeel_core::license::reject_relay_entitlement();
                            self.link_refresh_retry_at = Instant::now() + LINK_REJECTED_RETRY_DELAY;
                            emitted.push(ServeEvent::Warning(format!(
                                "Link authorization rejected: {error}"
                            )));
                        }
                        Err(error) if request_is_current => {
                            self.link_refresh_retry_at = Instant::now() + LINK_RETRY_DELAY;
                            emitted.push(ServeEvent::Warning(format!(
                                "Link authorization refresh failed: {error}"
                            )));
                        }
                        _ => self.link_refresh_retry_at = Instant::now(),
                    }
                }
                Ok(LinkRefreshResult::NativeKeychain {
                    generation,
                    mac_id,
                    result,
                }) => {
                    self.link_refresh_rx = None;
                    let request_is_current = self.owns_controller_serving()
                        && generation == self.platform_adapters.generation()
                        && self.platform_adapters.supports("link.entitlement.refresh");
                    if !request_is_current {
                        self.link_refresh_retry_at = Instant::now();
                    } else {
                        match result {
                            Ok(response)
                                if response.status == 200
                                    && response
                                        .body
                                        .get("available")
                                        .and_then(serde_json::Value::as_bool)
                                        .is_some() =>
                            {
                                let available = response.body["available"].as_bool() == Some(true);
                                let cache_committed = matches!(
                                    unpeel_core::relay_uplink::entitlement_cache_state(&mac_id),
                                    unpeel_core::relay_uplink::EntitlementCacheState::Fresh
                                ) && matches!(
                                    unpeel_core::license::allowed_cached_relay_entitlement(),
                                    Ok(Some(_))
                                );
                                self.link_refresh_retry_at = if !available || cache_committed {
                                    if available {
                                        Instant::now()
                                    } else {
                                        Instant::now() + LINK_RETRY_DELAY
                                    }
                                } else {
                                    emitted.push(ServeEvent::Warning(
                                        "native Link authorization was not committed".into(),
                                    ));
                                    Instant::now() + LINK_RETRY_DELAY
                                };
                            }
                            Ok(response) => {
                                self.link_refresh_retry_at = Instant::now() + LINK_RETRY_DELAY;
                                emitted.push(ServeEvent::Warning(format!(
                                    "native Link authorization returned status {}",
                                    response.status
                                )));
                            }
                            Err(error) => {
                                self.link_refresh_retry_at = Instant::now() + LINK_RETRY_DELAY;
                                emitted.push(ServeEvent::Warning(format!(
                                    "native Link authorization refresh failed: {error}"
                                )));
                            }
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.link_refresh_rx = None;
                    self.link_refresh_retry_at = Instant::now() + LINK_RETRY_DELAY;
                }
            }
        }

        if !self.owns_controller_serving() {
            if let Some(uplink) = self.relay_uplink.take() {
                uplink.stop();
                emitted.push(ServeEvent::LinkStopped {
                    reason: "Controller serving is owned elsewhere".into(),
                });
                self.status_dirty = true;
            }
            return;
        }

        if unpeel_core::license::stored_file_exists() && unpeel_core::license::stored().is_none() {
            if let Some(uplink) = self.relay_uplink.take() {
                uplink.stop();
                emitted.push(ServeEvent::LinkStopped {
                    reason: "invalid Link key".into(),
                });
                self.status_dirty = true;
            }
            match unpeel_core::license::reject_invalid_stored_key() {
                Ok(true) => emitted.push(ServeEvent::Warning(
                    "invalid Link key was quarantined".into(),
                )),
                Ok(false) => {}
                Err(error) => emitted.push(ServeEvent::Warning(format!(
                    "invalid Link key could not be quarantined: {error}"
                ))),
            }
            return;
        }

        if self
            .relay_uplink
            .as_ref()
            .is_some_and(RelayUplink::take_authorization_rejected)
        {
            if let Some(uplink) = self.relay_uplink.take() {
                uplink.stop();
            }
            let _ = unpeel_core::license::reject_relay_entitlement();
            self.link_refresh_retry_at = Instant::now();
            emitted.push(ServeEvent::LinkStopped {
                reason: "relay rejected authorization".into(),
            });
            self.status_dirty = true;
        }

        let mac_id = match unpeel_core::relay_uplink::ensure_host_id() {
            Ok(value) => value,
            Err(error) => {
                emitted.push(ServeEvent::Warning(error));
                return;
            }
        };
        let tombstone = unpeel_core::license::link_tombstone_reason();
        let native_keychain_refresh = self.platform_adapters.supports("link.entitlement.refresh")
            && unpeel_core::license::stored().is_none();
        let refresh_allowed = if native_keychain_refresh {
            tombstone.as_ref().is_ok_and(|reason| {
                !matches!(
                    reason,
                    Some(unpeel_core::license::LinkTombstoneReason::UserDisabled)
                )
            })
        } else {
            matches!(
                unpeel_core::license::link_tombstone_allows_refresh(),
                Ok(true)
            )
        };
        let cache_state = unpeel_core::relay_uplink::entitlement_cache_state(&mac_id);
        let needs_refresh = tombstone.as_ref().is_ok_and(|reason| {
            reason.is_some()
                || cache_state != unpeel_core::relay_uplink::EntitlementCacheState::Fresh
        });
        if self.link_refresh_rx.is_none()
            && Instant::now() >= self.link_refresh_retry_at
            && refresh_allowed
            && needs_refresh
        {
            if let Some((key, _)) = unpeel_core::license::stored() {
                let (sender, receiver) = mpsc::channel();
                let refresh_mac_id = mac_id.clone();
                let refresh_key = key.clone();
                std::thread::Builder::new()
                    .name("unpeel-serve-link-refresh".into())
                    .spawn(move || {
                        let result = unpeel_core::license::request_relay_entitlement_for_key(
                            &refresh_mac_id,
                            &refresh_key,
                        );
                        let _ = sender.send(LinkRefreshResult::StoredKey {
                            key: refresh_key,
                            result,
                        });
                    })
                    .expect("spawn Link entitlement refresh");
                self.link_refresh_rx = Some(receiver);
            } else if native_keychain_refresh {
                let generation = self.platform_adapters.generation();
                let adapters = Arc::clone(&self.platform_adapters);
                let (sender, receiver) = mpsc::channel();
                let refresh_mac_id = mac_id.clone();
                std::thread::Builder::new()
                    .name("unpeel-serve-native-link-refresh".into())
                    .spawn(move || {
                        let result = adapters.call(
                            "link.entitlement.refresh",
                            serde_json::json!({ "macID": &refresh_mac_id }),
                        );
                        let _ = sender.send(LinkRefreshResult::NativeKeychain {
                            generation,
                            mac_id: refresh_mac_id,
                            result,
                        });
                    })
                    .expect("spawn native Link entitlement refresh");
                self.link_refresh_rx = Some(receiver);
            }
        }

        let should_run = crate::relay::has_registrations()
            && matches!(
                unpeel_core::license::allowed_cached_relay_entitlement(),
                Ok(Some(_))
            );
        if should_run && self.relay_uplink.is_none() {
            let Some(port) = self.direct_port() else {
                return;
            };
            self.relay_uplink = Some(crate::relay::start_with_runtime(
                Arc::clone(&self.snapshot),
                self.mark_read_tx.clone(),
                Some(self.hook_port),
                Arc::clone(&self.resizes),
                Arc::clone(&self.approvals),
                Arc::clone(&self.pairing),
                port,
                Arc::clone(&self.platform_adapters),
                Arc::clone(&self.presence),
            ));
            emitted.push(ServeEvent::LinkStarted);
            self.status_dirty = true;
        } else if !should_run {
            if let Some(uplink) = self.relay_uplink.take() {
                uplink.stop();
                emitted.push(ServeEvent::LinkStopped {
                    reason: "no valid Link authorization".into(),
                });
                self.status_dirty = true;
            }
        }
    }

    fn record_alert(&mut self, session_id: &str, body: String, at: SystemTime) {
        let Some(row) = self
            .model
            .rows
            .iter()
            .find(|row| row.id == session_id)
            .cloned()
        else {
            return;
        };
        let at = at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.append_activity(
            &row,
            unpeel_core::activity_log::ActivityLogKind::Alert,
            at,
            Some(body.clone()),
        );
        if self.deliver_notification(
            &row,
            crate::notifications::NotificationKind::Alert,
            Some(&body),
            false,
            at,
        ) {
            let _ = unpeel_core::session_ops::mark_read(session_id);
            self.unread_ids.remove(session_id);
        } else {
            self.unread_ids.insert(session_id.to_string());
        }
    }

    fn sync_platform_approvals(&mut self, platform_generation: u64) {
        let approval_generation = self.approvals.generation();
        let generation = (platform_generation, approval_generation);
        if generation == self.approval_sync_generation {
            return;
        }
        self.approval_sync_generation = generation;
        if !self.platform_adapters.supports("approval.present") {
            return;
        }
        match self.platform_adapters.call(
            "approval.present",
            serde_json::json!({ "approvals": self.approvals.list_json() }),
        ) {
            Ok(response) if response.status == 200 => {}
            Ok(response) => crate::tracelog::trace(
                "approval",
                &format!("platform presentation returned status {}", response.status),
            ),
            Err(error) => crate::tracelog::trace(
                "approval",
                &format!("platform presentation failed: {error}"),
            ),
        }
    }

    /// Returns true when any Controller or the local Mac is already observing
    /// this Session. The Host owns the edge and exact-device suppression;
    /// the app callback supplies only platform delivery and window presence.
    fn deliver_notification(
        &self,
        row: &SessionRow,
        kind: crate::notifications::NotificationKind,
        body: Option<&str>,
        requires_notify_when_done: bool,
        now: u64,
    ) -> bool {
        let suppress_device_ids = self.presence.viewing_device_ids(&row.id, now);
        let any_controller = self.presence.any_viewer(&row.id, now);
        let raw_title = row.label.trim();
        let outcome = crate::notifications::deliver(
            &self.platform_adapters,
            crate::notifications::NotificationRequest {
                session_id: &row.id,
                title: if raw_title.is_empty() {
                    "Unpeel session"
                } else {
                    raw_title
                },
                kind,
                body,
                requires_notify_when_done,
                send_desktop: !any_controller,
                suppress_device_ids,
            },
        );
        any_controller || outcome.mac_observed
    }

    fn append_activity(
        &mut self,
        row: &SessionRow,
        kind: unpeel_core::activity_log::ActivityLogKind,
        at: u64,
        message: Option<String>,
    ) {
        let title = row.label.trim();
        let entry = unpeel_core::activity_log::ActivityLogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: row.id.clone(),
            kind,
            at,
            title: if title.is_empty() {
                "Untitled session".into()
            } else {
                title.into()
            },
            command: row.presentation_command().to_string(),
            project_id: if row.group_id.is_empty() {
                row.project_id.clone()
            } else {
                row.group_id.clone()
            },
            project_name: project_name(&self.model, row),
            message,
        };
        let _ = self.activity_log.append(entry);
    }

    /// Fold the background engine install's outcome into `serve.json`.
    fn poll_browser_engine_install(&mut self) {
        let Some(rx) = self.browser_engine_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(path)) => {
                self.browser_engine_status = unpeel_core::browser_engine::Status::ready(path);
            }
            Ok(Err(error)) => {
                self.browser_engine_status = unpeel_core::browser_engine::Status::failed(error);
            }
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.browser_engine_status = unpeel_core::browser_engine::Status::failed(
                    "engine install thread exited without a result".into(),
                );
            }
        }
        self.browser_engine_rx = None;
        self.status_dirty = true;
    }

    fn publish_status(&self) -> Result<(), String> {
        self.lease.publish(&ServeStatus {
            version: STATUS_VERSION,
            pid: std::process::id(),
            started_at_unix_ms: self.started_at_unix_ms,
            workspace_home: &self.home,
            hook_port: self.hook_port,
            local_socket: self.local_gateway.path(),
            direct_port: self.direct_port(),
            link_running: self.relay_uplink.is_some(),
            terminal_streamer: self
                .mobile_server
                .as_ref()
                .and_then(MobileServer::streamer_status),
            pty_core: self.pty_core.status(),
            browser_engine: Some(self.browser_engine_status.clone()),
            computer_engine: Some(self.computer.engine_status().clone()),
            computer_use: Some(self.computer.status().wire()),
            native_app_owns_controllers: self.native_app_owns_controllers(),
            platform_capabilities: self.platform_adapters.capabilities(),
            executable: std::env::current_exe().ok(),
            host_version: env!("CARGO_PKG_VERSION"),
            build_id: unpeel_core::session_host::current_host_build_id(),
        })
    }
}

/// Install (or confirm) the pinned Browser MCP engine without blocking the
/// worker's start: the result lands in `serve.json.browserEngine` on a later
/// tick, and a failure is a `browser-engine` trace line plus that status,
/// never a startup error. The install itself is flock-serialised, so a
/// concurrent `unpeel browser install` or a sibling workspace worker simply
/// waits and re-verifies.
/// `UNPEEL_BROWSER_ENGINE_INSTALL=0` (or `false`/`off`/`no`) keeps the
/// worker from installing the Browser MCP engine at start: no thread, no
/// network, `serve.json.browserEngine.state = "disabled"`. Benchmarks set it
/// so the start-up footprint never includes a download; an operator who
/// manages the engine by hand (or `unpeel browser install`) can too.
fn browser_engine_install_enabled() -> bool {
    !matches!(
        std::env::var("UNPEEL_BROWSER_ENGINE_INSTALL")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

fn spawn_browser_engine_install(home: &Path) -> mpsc::Receiver<Result<PathBuf, String>> {
    let (tx, rx) = mpsc::channel();
    let home = home.to_path_buf();
    std::thread::Builder::new()
        .name("browser-engine-install".into())
        .spawn(move || {
            let result = unpeel_core::browser_engine::ensure_installed(&home);
            if let Err(error) = &result {
                crate::tracelog::trace("browser-engine", &format!("install failed: {error}"));
            }
            let _ = tx.send(result);
        })
        .expect("spawn browser-engine install thread");
    rx
}

impl Drop for HostRuntime {
    fn drop(&mut self) {
        if let Some(uplink) = self.relay_uplink.take() {
            uplink.stop();
        }
        if let Some(server) = self.mobile_server.take() {
            server.stop();
        }
        crate::hook_listener::unregister_port(self.hook_port);
    }
}

/// Run the Host in the foreground until SIGINT or SIGTERM. A service manager
/// can supervise this command directly; shutdown releases every transport
/// lease before returning.
pub fn run(mut report: impl FnMut(ServeEvent)) -> Result<(), String> {
    SHUTDOWN_REQUESTED.store(false, Ordering::Release);
    unsafe {
        libc::signal(libc::SIGINT, request_shutdown as libc::sighandler_t);
        libc::signal(libc::SIGTERM, request_shutdown as libc::sighandler_t);
    }
    let (mut driver, events) = HostRuntime::start()?;
    for event in events {
        report(event);
    }
    while !SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        for event in driver.tick() {
            report(event);
        }
        std::thread::sleep(LOOP_INTERVAL);
    }
    for event in driver.shutdown_events() {
        report(event);
    }
    drop(driver);
    report(ServeEvent::Stopped);
    Ok(())
}

/// A blank home gets the same builtin presets the Mac app seeds on its
/// first launch (installed agent CLIs found on PATH), so a headless box has
/// something to launch before any Controller connects. Presets only — the
/// app's suggested-project picker is interactive and stays app-side. Fills
/// an empty list once and announces on the state bus; never touches an
/// existing or unparseable file.
fn seed_blank_home() {
    let state: serde_json::Value = match std::fs::read(unpeel_core::app_paths::app_state_path()) {
        Ok(raw) => match serde_json::from_slice(&raw) {
            Ok(state) => state,
            // Unparseable: seeding over it would delete the user's presets
            // and projects on the first run after an update.
            Err(_) => return,
        },
        Err(_) => serde_json::Value::Null,
    };
    if !unpeel_core::first_run::needs_seeding(&state) {
        return;
    }
    match unpeel_core::first_run::seed_app_state(&[]) {
        Ok((presets, _)) => crate::tracelog::trace(
            "host-worker",
            &format!(
                "seeded {} builtin preset(s) into a blank home",
                presets.len()
            ),
        ),
        Err(error) => crate::tracelog::trace(
            "host-worker",
            &format!("first-run seeding skipped: {error}"),
        ),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn runtime_launch_metadata(session_id: &str) -> (Option<u64>, Option<u64>) {
    let Some(manifest) = unpeel_core::session_host::load_manifest(session_id) else {
        return (None, None);
    };
    (
        Some(manifest.runtime_launch_generation),
        manifest.runtime_launched_at,
    )
}

fn latest_activity_at(
    row: &SessionRow,
    activity_log: &unpeel_core::activity_log::ActivityLogStore,
) -> Option<u64> {
    let lifecycle = unpeel_core::session_ops::last_activity_ms(&row.id, &row.command);
    let alert = activity_log
        .entries()
        .iter()
        .rev()
        .find(|entry| {
            entry.session_id == row.id
                && entry.kind == unpeel_core::activity_log::ActivityLogKind::Alert
        })
        .map(|entry| entry.at);
    match (lifecycle, alert) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn derive_unread(
    model: &SidebarModel,
    activity_log: &unpeel_core::activity_log::ActivityLogStore,
    local_claims: &HashSet<String>,
) -> HashSet<String> {
    model
        .rows
        .iter()
        .filter(|row| {
            let claimed = row.unread || local_claims.contains(&row.id);
            if !claimed {
                return false;
            }
            match unpeel_core::session_ops::read_marker(&row.id) {
                Some(read_at) => latest_activity_at(row, activity_log)
                    .is_some_and(|activity_at| activity_at > read_at),
                None => true,
            }
        })
        .map(|row| row.id.clone())
        .collect()
}

fn project_name(model: &SidebarModel, row: &SessionRow) -> String {
    let mut header = String::new();
    for item in &model.items {
        match item {
            SidebarItem::Header(name) => header = name.clone(),
            SidebarItem::WorktreeHeader {
                project_id,
                name,
                is_group,
                ..
            } if *project_id == row.group_id => {
                return if *is_group && !header.is_empty() {
                    format!("{header} › {name}")
                } else {
                    name.clone()
                };
            }
            SidebarItem::Session(index) if model.rows[*index].id == row.id => break,
            _ => {}
        }
    }
    if header.is_empty() {
        row.group_id.clone()
    } else {
        header
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_probe_distinguishes_client_only_app_from_compatibility_host() {
        let client = parse_probe_response(
            b"HTTP/1.1 200 OK\r\nX-Unpeel-Frontend: native\r\n\
              x-unpeel-controller-owner: Serve\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert_eq!(client.status, 200);
        assert_eq!(client.frontend.as_deref(), Some("native"));
        assert_eq!(client.controller_owner.as_deref(), Some("serve"));
        assert!(response_is_non_owning_frontend(&client));

        let compatibility = parse_probe_response(
            b"HTTP/1.1 200 OK\r\nX-Unpeel-Frontend: native\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert!(!response_is_non_owning_frontend(&compatibility));

        let tui = parse_probe_response(
            b"HTTP/1.1 200 OK\r\nX-Unpeel-Frontend: tui\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert!(response_is_non_owning_frontend(&tui));
    }

    #[test]
    fn native_handoff_requires_consecutive_present_confirmations() {
        let mut streak = 0;
        let mut published = NativeAuthority::Absent;

        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Present);
        assert_eq!(published, NativeAuthority::Absent);
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Present);
        assert_eq!(published, NativeAuthority::Absent);
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Present);
        assert_eq!(published, NativeAuthority::Present);

        // Reclaim is immediate and resets the confirmation streak.
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Absent);
        assert_eq!(published, NativeAuthority::Absent);
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Present);
        assert_eq!(published, NativeAuthority::Absent);
    }

    #[test]
    fn unresolved_probe_never_flips_ownership() {
        let mut streak = 0;
        let mut published = NativeAuthority::Absent;

        // A stalled hook server (probe timeout) keeps the current owner.
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Unresolved);
        assert_eq!(published, NativeAuthority::Absent);

        // Unresolved gaps do not break a genuine Present streak.
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Present);
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Unresolved);
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Present);
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Present);
        assert_eq!(published, NativeAuthority::Present);

        // And once Present, unresolved probes keep it Present.
        published = resolve_probe_observation(published, &mut streak, ProbeObservation::Unresolved);
        assert_eq!(published, NativeAuthority::Present);
    }

    #[test]
    fn serve_status_uses_stable_controller_fields() {
        let status = ServeStatus {
            version: STATUS_VERSION,
            pid: 42,
            started_at_unix_ms: 123,
            workspace_home: Path::new("/tmp/workspace"),
            hook_port: 41000,
            local_socket: Path::new("/tmp/workspace/host.sock"),
            direct_port: Some(42000),
            link_running: true,
            terminal_streamer: Some(crate::remote_streamer::StreamerStatus {
                state: "live",
                pid: Some(4242),
                port: Some(43000),
                restarts: 1,
                rapid_failures: 0,
                last_exit: Some("signal 9".into()),
            }),
            browser_engine: None,
            computer_engine: None,
            computer_use: None,
            pty_core: Some(crate::pty_core_supervisor::CoreStatus {
                state: "adopted",
                pid: Some(5150),
                sessions: Some(3),
                rapid_failures: 0,
                takeover_from: None,
                last_exit: None,
            }),
            native_app_owns_controllers: false,
            platform_capabilities: Vec::new(),
            executable: Some(PathBuf::from(
                "/Applications/Unpeel.app/Contents/MacOS/unpeel-host",
            )),
            host_version: "0.4.0",
            build_id: Some("1788338230.000000001:4242".into()),
        };
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["hostVersion"], "0.4.0");
        assert_eq!(value["buildId"], "1788338230.000000001:4242");
        assert_eq!(
            value["executable"],
            "/Applications/Unpeel.app/Contents/MacOS/unpeel-host"
        );
        assert_eq!(value["terminalStreamer"]["state"], "live");
        assert_eq!(value["terminalStreamer"]["pid"], 4242);
        assert_eq!(value["terminalStreamer"]["port"], 43000);
        assert_eq!(value["terminalStreamer"]["rapidFailures"], 0);
        assert_eq!(value["terminalStreamer"]["lastExit"], "signal 9");
        assert_eq!(value["ptyCore"]["state"], "adopted");
        assert_eq!(value["ptyCore"]["pid"], 5150);
        assert_eq!(value["ptyCore"]["sessions"], 3);
        assert_eq!(value["ptyCore"]["rapidFailures"], 0);
        assert!(value["ptyCore"].get("lastExit").is_none());
        assert_eq!(value["hookPort"], 41000);
        assert_eq!(value["localSocket"], "/tmp/workspace/host.sock");
        assert_eq!(value["directPort"], 42000);
        assert_eq!(value["linkRunning"], true);
    }
}
