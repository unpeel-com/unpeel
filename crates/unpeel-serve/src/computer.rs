//! Computer Use capability adapter for the canonical Host.
//!
//! On Linux, `unpeel serve` owns the long-lived Cua Driver child. On macOS,
//! the native app must remain the direct parent so the daemon inherits its
//! TCC responsibility chain; the worker obtains that app-owned adapter's
//! status through the authenticated, connection-scoped platform channel.
//! The unified MCP remains a socket client and Unpeel's existing
//! Off/Ask/Allow policy stays authoritative. A Controller sees two separate
//! facts in bootstrap: prerequisites are `available`, and the daemon is
//! currently `ready`.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

use serde_json::{json, Value};
use unpeel_core::computer_engine::{self, DesktopSession};
use unpeel_core::state::ComputerAccess;

use crate::platform_adapter::{PlatformAdapterHub, PlatformAdapterResponse};

const RECONCILE_INTERVAL: Duration = Duration::from_millis(250);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const PLATFORM_STATUS_INTERVAL: Duration = Duration::from_secs(2);
const PLATFORM_STATUS_OPERATION: &str = "computer.status";
const MAX_PLATFORM_REASON_BYTES: usize = 2_048;
/// How often the adapter re-resolves a missing engine (a present, verified
/// engine is cached by path and re-checked only for existence).
const ENGINE_CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputerAdapterStatus {
    pub available: bool,
    pub ready: bool,
    pub reason: Option<String>,
}

impl ComputerAdapterStatus {
    pub fn wire(&self) -> Value {
        let mut value = json!({
            "computerUseAvailable": self.available,
            "computerUseReady": self.ready,
        });
        if let Some(reason) = self.reason.as_deref() {
            value["computerUseUnavailableReason"] = reason.into();
        }
        value
    }

    fn from_platform_response(response: PlatformAdapterResponse) -> Result<Self, String> {
        if response.status != 200 {
            return Err(format!(
                "native Computer Use adapter returned HTTP {}",
                response.status
            ));
        }
        let object = response
            .body
            .as_object()
            .ok_or_else(|| "native Computer Use status is not an object".to_string())?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "available" | "ready" | "reason"))
        {
            return Err("native Computer Use status has unknown fields".into());
        }
        let available = object
            .get("available")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                "native Computer Use status has no boolean available field".to_string()
            })?;
        let ready = object
            .get("ready")
            .and_then(Value::as_bool)
            .ok_or_else(|| "native Computer Use status has no boolean ready field".to_string())?;
        if ready && !available {
            return Err("native Computer Use status is ready but unavailable".into());
        }
        let reason = match object.get("reason") {
            None => None,
            Some(Value::String(reason))
                if !reason.is_empty()
                    && reason.len() <= MAX_PLATFORM_REASON_BYTES
                    && !reason.chars().any(char::is_control) =>
            {
                Some(reason.clone())
            }
            Some(_) => return Err("native Computer Use status has an invalid reason".into()),
        };
        if ready && reason.is_some() {
            return Err("ready native Computer Use status cannot include a reason".into());
        }
        Ok(Self {
            available,
            ready,
            reason,
        })
    }
}

type PlatformStatusResult = (u64, Result<ComputerAdapterStatus, String>);
pub(crate) type SharedComputerStatus = Arc<Mutex<ComputerAdapterStatus>>;

/// The Host-owned engine install (`computer_engine`), driven on demand: the
/// worker downloads the pinned cua-driver only once Computer Use is turned
/// on (or `unpeel computer install` runs by hand), never at bare start —
/// unlike the browser engine, most Hosts never enable this domain, and the
/// archive is 30–40 MiB. Published additively as `serve.json.computerEngine`.
struct EngineInstall {
    rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    status: computer_engine::Status,
    /// A resolved engine (managed, override, bundled, or PATH), re-checked
    /// for existence each tick and dropped when it disappears.
    path: Option<PathBuf>,
    next_check: Instant,
    /// The last probe failure (the engine exists but cannot start); cleared
    /// once a probe succeeds.
    probe_failed: Option<String>,
}

impl EngineInstall {
    fn new(now: Instant) -> Self {
        Self {
            rx: None,
            probe_failed: None,
            status: if computer_engine::install_enabled() {
                computer_engine::Status::missing(None)
            } else {
                computer_engine::Status::disabled()
            },
            path: None,
            next_check: now,
        }
    }
}

/// The Controller-facing sentence for an engine that is not usable yet.
fn engine_unavailable_reason(status: &computer_engine::Status) -> String {
    match status.state.as_str() {
        "installing" => format!(
            "Installing Cua Driver {} on this Host; Computer Use becomes available when it \
finishes.",
            status.version
        ),
        "failed" => format!(
            "Cua Driver {} is not usable: {}. `unpeel computer install --check` on this Host \
reports the same; {} overrides the engine.",
            status.version,
            status.error.as_deref().unwrap_or("unknown error"),
            computer_engine::ENV_OVERRIDE
        ),
        "disabled" => format!(
            "Cua Driver is not installed and automatic install is off ({}=0). Run `unpeel \
computer install` on this Host or set {}.",
            computer_engine::ENV_INSTALL,
            computer_engine::ENV_OVERRIDE
        ),
        _ => format!(
            "Cua Driver {} is not installed. Turn on Computer use to install it on this Host, \
or run `unpeel computer install`.",
            status.version
        ),
    }
}

/// Whether the workspace's persisted policy asks for Computer Use at all —
/// the on-demand trigger for the engine install. Read-only; a malformed
/// state file means "not requested".
fn computer_use_requested() -> bool {
    unpeel_core::app_state::load()
        .ok()
        .and_then(|state| {
            state
                .get("experimental_features")
                .and_then(|features| features.get("computer_use"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

pub struct ComputerAdapter {
    home: PathBuf,
    child: Option<Child>,
    owns_socket: bool,
    status: ComputerAdapterStatus,
    last_reconcile: Instant,
    started_at: Option<Instant>,
    next_health: Instant,
    next_retry: Instant,
    platform_adapters: Arc<PlatformAdapterHub>,
    platform_generation: u64,
    platform_status_rx: Option<mpsc::Receiver<PlatformStatusResult>>,
    next_platform_status: Instant,
    shared_status: SharedComputerStatus,
    engine: EngineInstall,
    /// The desktop session the daemon is (to be) started in: display plus
    /// session bus, re-resolved every reconcile so a session that appears
    /// or vanishes flips readiness truthfully.
    session: Option<DesktopSession>,
}

impl ComputerAdapter {
    pub fn new(home: &Path, platform_adapters: Arc<PlatformAdapterHub>) -> Self {
        let now = Instant::now();
        let platform_generation = platform_adapters.generation();
        let status = ComputerAdapterStatus {
            available: false,
            ready: false,
            reason: Some(if cfg!(target_os = "linux") {
                "Checking Cua Driver and the graphical session…".into()
            } else {
                "Computer Use on macOS requires the Unpeel app to be running on this Host.".into()
            }),
        };
        Self {
            home: home.to_path_buf(),
            child: None,
            owns_socket: false,
            status: status.clone(),
            last_reconcile: now - RECONCILE_INTERVAL,
            started_at: None,
            next_health: now,
            next_retry: now,
            platform_adapters,
            platform_generation,
            platform_status_rx: None,
            next_platform_status: now,
            shared_status: Arc::new(Mutex::new(status)),
            engine: EngineInstall::new(now),
            session: None,
        }
    }

    pub fn status(&self) -> &ComputerAdapterStatus {
        &self.status
    }

    /// The Host-owned engine install state for `serve.json.computerEngine`.
    pub fn engine_status(&self) -> &computer_engine::Status {
        &self.engine.status
    }

    /// A resolved (or freshly installed) engine is only usable if it runs:
    /// probe it once here. A binary whose hashes verified but whose X11
    /// client libraries are missing (a bare image) reports `failed` with
    /// the libraries and the apt line, and is re-probed on the next resolve
    /// so installing them heals readiness without a restart.
    fn adopt_engine(&mut self, path: PathBuf) {
        match computer_engine::probe(&path) {
            Ok(_) => {
                self.engine.probe_failed = None;
                self.engine.path = Some(path.clone());
                self.engine.status = computer_engine::Status::ready(path);
            }
            Err(failure) => {
                let message = failure.to_string();
                if self.engine.probe_failed.as_deref() != Some(message.as_str()) {
                    crate::tracelog::trace("computer-engine", &message);
                }
                self.engine.probe_failed = Some(message.clone());
                self.engine.path = None;
                self.engine.status = computer_engine::Status::failed(message);
            }
        }
    }

    /// Poll the background install, keep the resolved engine path fresh, and
    /// start an install when policy asks for Computer Use and no usable
    /// engine exists. Returns true when the published engine status changed.
    fn reconcile_engine(&mut self) -> bool {
        let before = self.engine.status.clone();
        if let Some(rx) = self.engine.rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(path)) => {
                    self.adopt_engine(path);
                    self.engine.rx = None;
                }
                Ok(Err(error)) => {
                    self.engine.status = computer_engine::Status::failed(error);
                    self.engine.rx = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.engine.status = computer_engine::Status::failed(
                        "engine install thread exited without a result".into(),
                    );
                    self.engine.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(path) = self.engine.path.as_ref() {
            if path.is_file() {
                return before != self.engine.status;
            }
            self.engine.path = None;
        }
        let now = Instant::now();
        if self.engine.rx.is_some() || now < self.engine.next_check {
            return before != self.engine.status;
        }
        self.engine.next_check = now + ENGINE_CHECK_INTERVAL;
        match computer_engine::resolve(&self.home) {
            Ok(path) => self.adopt_engine(path),
            Err(error) => {
                if !computer_engine::install_enabled() {
                    self.engine.status = computer_engine::Status::disabled();
                } else if self.engine.status.state == "failed" && self.engine.probe_failed.is_none()
                {
                    // Keep an install failure visible; a policy edit or
                    // restart retries. (A probe failure re-resolves so that
                    // installing the libraries heals it without a restart.)
                } else if computer_use_requested() {
                    self.engine.rx = Some(spawn_engine_install(&self.home));
                    self.engine.status = computer_engine::Status::installing();
                } else {
                    self.engine.status = computer_engine::Status::missing(Some(error));
                }
            }
        }
        before != self.engine.status
    }

    pub(crate) fn shared_status(&self) -> SharedComputerStatus {
        Arc::clone(&self.shared_status)
    }

    /// Reconcile policy + prerequisites + child health. Returns true only
    /// when Controller-visible adapter state changed.
    pub fn reconcile(&mut self) -> bool {
        if self.last_reconcile.elapsed() < RECONCILE_INTERVAL {
            return false;
        }
        self.last_reconcile = Instant::now();
        let engine_changed = self.reconcile_engine();
        let before = self.status.clone();
        self.reconcile_now();
        let changed = before != self.status;
        if changed {
            if let Ok(mut shared) = self.shared_status.lock() {
                *shared = self.status.clone();
            }
        }
        changed || engine_changed
    }

    /// Add the dynamic adapter advertisement beside the persisted
    /// experimental toggle. This is capability data, not a guessed platform
    /// branch in the Controller.
    pub fn decorate_workspace_settings(&self, bootstrap: &mut Value) {
        decorate_workspace_settings(&self.status, bootstrap);
    }

    /// Decorate an independently generated local-gateway bootstrap with the
    /// same worker-owned status used by Direct and Link snapshots.
    pub(crate) fn decorate_shared_workspace_settings(
        status: &SharedComputerStatus,
        bootstrap: &mut Value,
    ) {
        if let Ok(status) = status.lock() {
            decorate_workspace_settings(&status, bootstrap);
        }
    }

    fn reconcile_now(&mut self) {
        if !cfg!(target_os = "linux") {
            self.reconcile_platform_status();
            return;
        }

        let binary = match self.engine.path.clone() {
            Some(binary) => binary,
            None => {
                self.stop_owned();
                self.status = ComputerAdapterStatus {
                    available: false,
                    ready: false,
                    reason: Some(engine_unavailable_reason(&self.engine.status)),
                };
                return;
            }
        };
        let display = match computer_engine::desktop_session() {
            Ok(session) => {
                let display = session.display.clone();
                self.session = Some(session);
                display
            }
            Err(reason) => {
                self.stop_workspace_daemon(&binary);
                self.session = None;
                self.status = ComputerAdapterStatus {
                    available: false,
                    ready: false,
                    reason: Some(format!(
                        "`unpeel serve` sees {reason}. Then check `cua-driver doctor --json` \
in that session. (`unpeel serve install --graphical` binds the service to the desktop session.)"
                    )),
                };
                return;
            }
        };

        let state = match unpeel_core::app_state::load() {
            Ok(state) => state,
            Err(error) => {
                self.stop_workspace_daemon(&binary);
                self.status = ComputerAdapterStatus {
                    available: true,
                    ready: false,
                    reason: Some(format!("Could not read Computer Use policy: {error}")),
                };
                return;
            }
        };
        let feature_enabled = state
            .get("experimental_features")
            .and_then(|features| features.get("computer_use"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let access = unpeel_core::computer_mcp::access_from_app_state(&state);
        if !feature_enabled {
            self.stop_workspace_daemon(&binary);
            self.status = ComputerAdapterStatus {
                available: true,
                ready: false,
                reason: None,
            };
            return;
        }
        if access == ComputerAccess::Off {
            self.stop_workspace_daemon(&binary);
            self.status = ComputerAdapterStatus {
                available: true,
                ready: false,
                reason: Some("Computer access is Off in Settings ▸ Computer use.".into()),
            };
            return;
        }

        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    self.child = None;
                    self.owns_socket = false;
                    self.started_at = None;
                    let _ = std::fs::remove_file(self.socket_path());
                    self.status = ComputerAdapterStatus {
                        available: true,
                        ready: false,
                        reason: Some(format!(
                            "Cua Driver exited ({exit}); retrying from the {display} session."
                        )),
                    };
                    self.next_retry = Instant::now() + RETRY_DELAY;
                    return;
                }
                Err(error) => {
                    self.stop_owned();
                    self.status = ComputerAdapterStatus {
                        available: true,
                        ready: false,
                        reason: Some(format!("Could not inspect Cua Driver: {error}")),
                    };
                    self.next_retry = Instant::now() + RETRY_DELAY;
                    return;
                }
                Ok(None) => {}
            }
        }

        let now = Instant::now();
        if self.status.ready && now < self.next_health {
            return;
        }
        // A live socket without our child is an orphan from an abruptly
        // terminated older Host. This serve lease is the sole canonical
        // owner for the workspace: stop the orphan once, then launch a
        // death-tied child rather than silently adopting an unrestricted
        // daemon that would outlive us again.
        if self.child.is_none() && self.socket_path().exists() {
            stop_daemon(binary.as_path(), &self.socket_path());
            let _ = std::fs::remove_file(self.socket_path());
        }
        if self.socket_path().exists() && daemon_healthy(&binary, &self.socket_path()) {
            self.status = ComputerAdapterStatus {
                available: true,
                ready: true,
                reason: None,
            };
            self.next_health = now + HEALTH_INTERVAL;
            return;
        }

        if self.child.is_some() {
            if self
                .started_at
                .is_some_and(|started| started.elapsed() >= STARTUP_TIMEOUT)
            {
                self.stop_owned();
                self.status = ComputerAdapterStatus {
                    available: true,
                    ready: false,
                    reason: Some(format!(
                        "Cua Driver did not become ready in the {display} session. Run \
`cua-driver doctor --json` there and inspect {}.",
                        self.log_path().display()
                    )),
                };
                self.next_retry = now + RETRY_DELAY;
            } else {
                self.status = ComputerAdapterStatus {
                    available: true,
                    ready: false,
                    reason: Some(format!("Starting Cua Driver in the {display} session…")),
                };
            }
            return;
        }

        if now < self.next_retry {
            return;
        }
        match self.start_daemon(&binary) {
            Ok(()) => {
                self.status = ComputerAdapterStatus {
                    available: true,
                    ready: false,
                    reason: Some(format!("Starting Cua Driver in the {display} session…")),
                };
            }
            Err(error) => {
                self.status = ComputerAdapterStatus {
                    available: true,
                    ready: false,
                    reason: Some(format!("Could not start Cua Driver: {error}")),
                };
                self.next_retry = now + RETRY_DELAY;
            }
        }
    }

    fn reconcile_platform_status(&mut self) {
        self.stop_owned();
        let now = Instant::now();
        let generation = self.platform_adapters.generation();
        let supported = self.platform_adapters.supports(PLATFORM_STATUS_OPERATION);
        if generation != self.platform_generation {
            // A result belongs to the exact connection generation that asked
            // for it. Never let a departing app leave stale `ready` state on
            // the replacement registration (or after the app exits).
            self.platform_generation = generation;
            self.platform_status_rx = None;
            self.next_platform_status = now;
            self.status = Self::waiting_for_platform_status(supported);
        }

        if !supported {
            self.platform_status_rx = None;
            self.status = Self::waiting_for_platform_status(false);
            return;
        }

        let mut completed = None;
        let mut disconnected = false;
        if let Some(receiver) = self.platform_status_rx.as_ref() {
            match receiver.try_recv() {
                Ok(result) => completed = Some(result),
                Err(mpsc::TryRecvError::Disconnected) => disconnected = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if completed.is_some() || disconnected {
            self.platform_status_rx = None;
            self.next_platform_status = now + PLATFORM_STATUS_INTERVAL;
        }

        if let Some((result_generation, result)) = completed {
            let current_generation = self.platform_adapters.generation();
            if result_generation == current_generation
                && self.platform_adapters.supports(PLATFORM_STATUS_OPERATION)
            {
                match result {
                    Ok(status) => self.status = status,
                    Err(error) => {
                        crate::tracelog::trace(
                            "computer",
                            &format!("native Computer Use status rejected: {error}"),
                        );
                        self.status = ComputerAdapterStatus {
                            available: false,
                            ready: false,
                            reason: Some(
                                "The Unpeel app did not report a valid Computer Use status.".into(),
                            ),
                        };
                    }
                }
            } else {
                self.platform_generation = current_generation;
                self.next_platform_status = now;
                self.status = Self::waiting_for_platform_status(
                    self.platform_adapters.supports(PLATFORM_STATUS_OPERATION),
                );
            }
        } else if disconnected {
            self.status = ComputerAdapterStatus {
                available: false,
                ready: false,
                reason: Some("The Unpeel app did not report its Computer Use status.".into()),
            };
        }

        if self.platform_status_rx.is_none()
            && now >= self.next_platform_status
            && self.platform_adapters.supports(PLATFORM_STATUS_OPERATION)
        {
            let adapters = Arc::clone(&self.platform_adapters);
            let generation = adapters.generation();
            let (sender, receiver) = mpsc::channel();
            match std::thread::Builder::new()
                .name("unpeel-computer-status".into())
                .spawn(move || {
                    let result = adapters
                        .call(PLATFORM_STATUS_OPERATION, json!({}))
                        .map_err(|error| error.to_string())
                        .and_then(ComputerAdapterStatus::from_platform_response);
                    let _ = sender.send((generation, result));
                }) {
                Ok(_) => self.platform_status_rx = Some(receiver),
                Err(error) => {
                    crate::tracelog::trace(
                        "computer",
                        &format!("could not start native status request: {error}"),
                    );
                    self.next_platform_status = now + PLATFORM_STATUS_INTERVAL;
                    self.status = ComputerAdapterStatus {
                        available: false,
                        ready: false,
                        reason: Some(
                            "The Unpeel app did not report its Computer Use status.".into(),
                        ),
                    };
                }
            }
        }
    }

    fn waiting_for_platform_status(supported: bool) -> ComputerAdapterStatus {
        ComputerAdapterStatus {
            available: false,
            ready: false,
            reason: Some(if supported {
                "Checking the Unpeel app's Computer Use adapter…".into()
            } else {
                "Computer Use on macOS requires the Unpeel app to be running on this Host.".into()
            }),
        }
    }

    fn socket_path(&self) -> PathBuf {
        self.home.join("computer").join("daemon.sock")
    }

    fn log_path(&self) -> PathBuf {
        self.home.join("computer").join("daemon.log")
    }

    fn start_daemon(&mut self, binary: &Path) -> Result<(), String> {
        let computer_dir = self.home.join("computer");
        std::fs::create_dir_all(&computer_dir)
            .map_err(|error| format!("create {}: {error}", computer_dir.display()))?;
        let socket = self.socket_path();
        if socket.exists() && !daemon_healthy(binary, &socket) {
            std::fs::remove_file(&socket)
                .map_err(|error| format!("remove stale {}: {error}", socket.display()))?;
        }
        let mut log_options = OpenOptions::new();
        log_options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        log_options.mode(0o600);
        let log = log_options
            .open(self.log_path())
            .map_err(|error| format!("open daemon log: {error}"))?;
        let stderr = log
            .try_clone()
            .map_err(|error| format!("clone daemon log: {error}"))?;

        let mut command = Command::new(binary);
        command
            .arg("serve")
            .arg("--embedded")
            .arg("--socket")
            .arg(&socket)
            .env("CUA_DRIVER_EMBEDDED", "1")
            // Unpeel owns the visible Off/Ask/Allow gate and approval hub.
            .env("CUA_DRIVER_PERMISSION_MODE", "unrestricted")
            .env("CUA_DRIVER_DANGEROUSLY_BYPASS_APPROVALS", "1")
            // Nothing about this Host's activity leaves the user's machines.
            .env(
                unpeel_core::computer_mcp::TELEMETRY_OPT_OUT.0,
                unpeel_core::computer_mcp::TELEMETRY_OPT_OUT.1,
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        // The session bus the AT-SPI bridge lives on, and the Wayland opt-in
        // when that is the chosen session (resolved by `desktop_session`).
        if let Some(session) = self.session.as_ref() {
            for (name, value) in session.daemon_env() {
                command.env(name, value);
            }
        }

        #[cfg(target_os = "linux")]
        unsafe {
            let parent = libc::getpid();
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Unpeel Host exited before Cua Driver started",
                    ));
                }
                Ok(())
            });
        }

        self.child = Some(
            command
                .spawn()
                .map_err(|error| format!("launch {}: {error}", binary.display()))?,
        );
        self.owns_socket = true;
        self.started_at = Some(Instant::now());
        self.next_health = Instant::now();
        Ok(())
    }

    fn stop_owned(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if self.owns_socket {
            let _ = std::fs::remove_file(self.socket_path());
        }
        self.owns_socket = false;
        self.started_at = None;
    }

    fn stop_workspace_daemon(&mut self, binary: &Path) {
        self.stop_owned();
        if self.socket_path().exists() {
            stop_daemon(binary, &self.socket_path());
            let _ = std::fs::remove_file(self.socket_path());
        }
    }
}

fn decorate_workspace_settings(status: &ComputerAdapterStatus, bootstrap: &mut Value) {
    let Some(experimental) = bootstrap
        .get_mut("workspaceSettings")
        .and_then(|settings| settings.get_mut("experimentalSettings"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let wire = status.wire();
    let Some(wire) = wire.as_object() else { return };
    for (key, value) in wire {
        experimental.insert(key.clone(), value.clone());
    }
    if status.reason.is_none() {
        experimental.remove("computerUseUnavailableReason");
    }
}

impl Drop for ComputerAdapter {
    fn drop(&mut self) {
        self.stop_owned();
    }
}

fn daemon_healthy(binary: &Path, socket: &Path) -> bool {
    bounded_driver_command(binary, "status", socket)
}

fn stop_daemon(binary: &Path, socket: &Path) {
    let _ = bounded_driver_command(binary, "stop", socket);
}

/// Spawn the engine install off the reconcile loop; the result lands on the
/// next tick (flock-serialised with `unpeel computer install`).
fn spawn_engine_install(home: &Path) -> mpsc::Receiver<Result<PathBuf, String>> {
    let (tx, rx) = mpsc::channel();
    let home = home.to_path_buf();
    std::thread::Builder::new()
        .name("computer-engine-install".into())
        .spawn(move || {
            let result = computer_engine::ensure_installed(&home);
            if let Err(error) = &result {
                crate::tracelog::trace("computer-engine", &format!("install failed: {error}"));
            }
            let _ = tx.send(result);
        })
        .expect("spawn computer-engine install thread");
    rx
}

fn bounded_driver_command(binary: &Path, subcommand: &str, socket: &Path) -> bool {
    let Ok(mut child) = Command::new(binary)
        .arg(subcommand)
        .arg("--socket")
        .arg(socket)
        .env("CUA_DRIVER_EMBEDDED", "1")
        .env(
            unpeel_core::computer_mcp::TELEMETRY_OPT_OUT.0,
            unpeel_core::computer_mcp::TELEMETRY_OPT_OUT.1,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn status_wire_omits_absent_reason() {
        assert_eq!(
            ComputerAdapterStatus {
                available: true,
                ready: true,
                reason: None,
            }
            .wire(),
            json!({
                "computerUseAvailable": true,
                "computerUseReady": true,
            })
        );
    }

    #[test]
    fn decorates_only_experimental_adapter_fields() {
        let mut adapter = ComputerAdapter::new(
            Path::new("/tmp/unpeel-computer-adapter-test"),
            Arc::new(PlatformAdapterHub::default()),
        );
        adapter.status = ComputerAdapterStatus {
            available: false,
            ready: false,
            reason: Some("missing display".into()),
        };
        let mut bootstrap = json!({
            "workspaceSettings": {
                "experimentalSettings": { "computerUse": false }
            }
        });
        adapter.decorate_workspace_settings(&mut bootstrap);
        assert_eq!(
            bootstrap["workspaceSettings"]["experimentalSettings"],
            json!({
                "computerUse": false,
                "computerUseAvailable": false,
                "computerUseReady": false,
                "computerUseUnavailableReason": "missing display",
            })
        );
    }

    #[test]
    fn platform_status_response_is_bounded_and_fail_closed() {
        let accepted = ComputerAdapterStatus::from_platform_response(PlatformAdapterResponse {
            status: 200,
            body: json!({ "available": true, "ready": false, "reason": "Starting…" }),
        })
        .unwrap();
        assert_eq!(
            accepted,
            ComputerAdapterStatus {
                available: true,
                ready: false,
                reason: Some("Starting…".into()),
            }
        );

        for body in [
            json!({ "available": false, "ready": true }),
            json!({ "available": true, "ready": true, "reason": "stale" }),
            json!({ "available": true, "ready": false, "unknown": true }),
            json!({ "available": true, "ready": false, "reason": "bad\nreason" }),
        ] {
            assert!(
                ComputerAdapterStatus::from_platform_response(PlatformAdapterResponse {
                    status: 200,
                    body,
                })
                .is_err()
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supervises_and_reaps_an_embedded_linux_driver() {
        let root = tempfile::tempdir().unwrap();
        let driver = root.path().join("fake-cua-driver");
        std::fs::write(
            &driver,
            r#"#!/bin/sh
if [ "$1" = "serve" ]; then
  : > "$4"
  exec sleep 3600
fi
if [ "$1" = "status" ]; then
  test -e "$3"
  exit $?
fi
exit 2
"#,
        )
        .unwrap();
        std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o700)).unwrap();

        let socket;
        {
            let mut adapter =
                ComputerAdapter::new(root.path(), Arc::new(PlatformAdapterHub::default()));
            adapter.start_daemon(&driver).unwrap();
            socket = adapter.socket_path();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                socket.exists(),
                "fake embedded driver never published its socket"
            );
            assert!(daemon_healthy(&driver, &socket));
        }
        assert!(
            !socket.exists(),
            "adapter drop left its owned socket behind"
        );
    }
}
