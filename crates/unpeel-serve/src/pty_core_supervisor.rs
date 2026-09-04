//! Worker-owned lifecycle of the shared `unpeel-host __pty_core__` process.
//!
//! The PTY core hosts N Sessions in one detached process (setsid, stdio null)
//! so that terminals outlive both the app and this worker, exactly like the
//! per-process `__session_host__` hosts do today. The worker therefore only
//! *starts, adopts, and respawns* the core; it never stops one. The only stop
//! verb is the core's own socket `shutdown`, which the core refuses while it
//! hosts live Sessions, and this supervisor never sends it.
//!
//! Gate: the core is the default since 0.4.4 (2026-09-03). Only
//! `UNPEEL_PTY_CORE=0` keeps the per-process behavior — no core is started
//! and nothing is published; absent, empty, or any other value manages a
//! core, exactly like spawn routing. The variable is
//! inherited unchanged by everything the worker spawns, so
//! `session_host::spawn_host_process_from_launch_file` routing and the escape
//! hatch stay consistent across the whole process tree.
//!
//! On start the supervisor reads `$UNPEEL_HOME/pty-core.json`; when the
//! record names a live process (pid identity via `pid_started_at`, then a
//! `ping` on `pty-core.sock` answering with the same pid) it ADOPTS that core.
//! Otherwise it spawns a fresh one and waits, non-blocking, for `ping`. If the
//! core later exits, it respawns with the same rapid-failure backoff shape as
//! `remote_streamer.rs`. A second core started against a home that already
//! has one exits 0 immediately (flock on `pty-core.lock`), so every exit is
//! first re-checked against the record for adoption before it counts as a
//! failure.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use unpeel_core::session_host::{recorded_pid_identity, PidIdentity};

/// Environment gate shared with `session_host` routing.
pub const ENV_GATE: &str = "UNPEEL_PTY_CORE";
/// `unpeel-host` argv mode of the core (owned by Lane A; contract name).
pub const PTY_CORE_ARG: &str = "__pty_core__";
/// Delay before respawning an exited core.
pub(crate) const RESTART_DELAY: Duration = Duration::from_secs(2);
/// An exit sooner than this after launch counts as a rapid failure.
pub(crate) const RAPID_EXIT_WINDOW: Duration = Duration::from_secs(10);
/// Consecutive rapid failures after which supervision gives up (`failed`).
pub(crate) const MAX_RAPID_FAILURES: u32 = 5;
/// How often a live/adopted core is pinged to refresh `sessions` and detect
/// an exit the worker cannot `wait()` on (an adopted core is not our child).
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
/// Connect + reply budget for one ping.
const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// A freshly spawned core that has not answered `ping` after this long is
/// reported once as a warning; it is never killed.
const START_WARN_AFTER: Duration = Duration::from_secs(15);

/// Whether the worker should manage a PTY core (default on; `0` opts out).
pub fn enabled() -> bool {
    enabled_from(std::env::var_os(ENV_GATE).as_deref())
}

fn enabled_from(value: Option<&std::ffi::OsStr>) -> bool {
    // Mirror `pty_core::routing_enabled`: only an explicit `0` opts out.
    match value {
        None => true,
        Some(value) => value.to_string_lossy().trim() != "0",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreState {
    /// Gate off: nothing managed, nothing published.
    Off,
    /// A core was spawned and has not answered `ping` yet.
    Starting,
    /// This worker's own spawned core answers `ping`.
    Live,
    /// A core started earlier (by a previous worker or by hand) was adopted.
    Adopted,
    /// An adopted core runs an older build; a `--takeover` core was spawned
    /// and is taking every Session over without restarting any of them.
    HandingOff,
    /// Crash-loop ceiling reached or no binary; not retrying automatically.
    Failed,
}

impl CoreState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Live => "live",
            Self::Adopted => "adopted",
            Self::HandingOff => "handing_off",
            Self::Failed => "failed",
        }
    }
}

/// Published into `serve.json` as `ptyCore` (additive).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreStatus {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<u64>,
    pub rapid_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<String>,
    /// While `handing_off`: the old core being replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub takeover_from: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreEvent {
    /// An already-running core was adopted instead of spawning one.
    Adopted {
        pid: u32,
        sessions: u64,
    },
    Started {
        pid: u32,
        restart: bool,
    },
    /// A spawned core answered `ping` for the first time.
    Ready {
        pid: u32,
        sessions: u64,
    },
    Exited {
        status: String,
        rapid_failures: u32,
        gave_up: bool,
    },
    /// An adopted core stopped answering and its pid is gone.
    Lost {
        pid: u32,
    },
    /// The adopted core runs a different build than the binary we would
    /// launch; a takeover core was spawned to replace it in place.
    TakeoverStarted {
        old_pid: u32,
        new_pid: u32,
    },
    /// The takeover core owns every Session and the record; the old core
    /// exits on its own.
    TakenOver {
        old_pid: u32,
        new_pid: u32,
        sessions: u64,
    },
    SpawnFailed {
        error: String,
    },
    /// The worker is going away and deliberately leaves the core running.
    LeftRunning {
        pid: u32,
        sessions: Option<u64>,
    },
    Warning(String),
}

/// `$UNPEEL_HOME/pty-core.json`, written by the core after it binds.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct CoreRecord {
    pub pid: u32,
    #[serde(default)]
    pub pid_started_at: Option<u64>,
    pub socket: PathBuf,
    #[serde(default)]
    pub host_build_id: Option<String>,
    #[serde(default)]
    pub protocol: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct PingReply {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub sessions: Option<u64>,
    #[serde(default)]
    pub host_build_id: Option<String>,
}

pub fn record_path(home: &Path) -> PathBuf {
    home.join("pty-core.json")
}

pub fn socket_path(home: &Path) -> PathBuf {
    home.join("pty-core.sock")
}

pub fn read_record(home: &Path) -> Option<CoreRecord> {
    let raw = std::fs::read(record_path(home)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// One `{"op":"ping"}` round trip on the core socket.
pub fn ping(socket: &Path) -> Result<PingReply, String> {
    let mut stream = UnixStream::connect(socket).map_err(|error| format!("connect: {error}"))?;
    stream
        .set_read_timeout(Some(PING_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(PING_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .write_all(b"{\"op\":\"ping\"}\n")
        .map_err(|error| format!("write: {error}"))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| format!("read: {error}"))?;
    let reply: PingReply =
        serde_json::from_str(line.trim()).map_err(|error| format!("decode: {error}"))?;
    if !reply.ok {
        return Err("core answered ok=false".into());
    }
    Ok(reply)
}

/// Whether the record names a core this worker may adopt: the recorded pid
/// is not provably recycled, and the socket answers `ping` with that pid.
/// Returns the ping reply on success so the caller learns the session count.
pub fn adoptable(record: &CoreRecord) -> Option<PingReply> {
    if record.pid == 0 || record.pid == std::process::id() {
        return None;
    }
    if recorded_pid_identity(record.pid, record.pid_started_at) == PidIdentity::NotOurs {
        return None;
    }
    let reply = ping(&record.socket).ok()?;
    (reply.pid == Some(record.pid)).then_some(reply)
}

/// How the supervisor launches a core; injectable so unit tests never spawn
/// a real `unpeel-host`.
pub type Spawner = Box<dyn FnMut(bool) -> Result<Child, String> + Send>;

/// How long a `--takeover` core may take to publish the record under its
/// own pid before the supervisor gives up on it (the old core, having
/// resumed, keeps serving; the takeover child is never signalled).
pub const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(30);

/// Build id of the binary the production spawner would launch, in the
/// core's own `host_build_id` shape; `None` when it cannot be resolved.
pub fn expected_host_build_id() -> Option<String> {
    let binary = unpeel_core::session_ops::resolve_host_binary().ok()?;
    unpeel_core::session_host::host_build_id_for(&binary)
}

/// Production spawner: `unpeel-host __pty_core__`, detached exactly like a
/// session host (setsid, stdio null, leaked `HERDR_*` env removed).
pub fn detached_core_spawner() -> Spawner {
    Box::new(|takeover| {
        let binary = unpeel_core::session_ops::resolve_host_binary()?;
        let mut command = std::process::Command::new(binary);
        command.arg(PTY_CORE_ARG);
        if takeover {
            command.arg(unpeel_core::pty_core::TAKEOVER_ARG);
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // A detached core is not an occupant of an outer Herdr pane; the
        // core helper in unpeel-core is crate-private, so mirror it here.
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HERDR_") {
                command.env_remove(&key);
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        command
            .spawn()
            .map_err(|error| format!("Failed to spawn PTY core: {error}"))
    })
}

pub struct PtyCoreSupervisor {
    home: PathBuf,
    spawner: Spawner,
    state: CoreState,
    /// Our own spawned child, kept only to reap it; never killed.
    child: Option<Child>,
    /// Pid of the managed core (own child or adopted).
    pid: Option<u32>,
    sessions: Option<u64>,
    launched_at: Instant,
    start_warned: bool,
    last_health: Instant,
    next_spawn_at: Option<Instant>,
    rapid_failures: u32,
    last_exit: Option<String>,
    /// Build id the spawner would produce; `None` disables takeovers.
    expected_build_id: Option<String>,
    /// Re-probe the expected id from disk on every health tick (production);
    /// tests pin it with `set_expected_build_id`.
    probe_expected_build_id: bool,
    /// While handing off: the old core's pid and when the takeover began.
    takeover_from: Option<u32>,
    takeover_started_at: Instant,
    /// One takeover attempt per (old pid, expected build): a takeover that
    /// leaves the ids mismatched must not loop.
    takeover_attempted: Option<(u32, String)>,
    /// Whether an older-build core may be taken over in place. Off by
    /// default since 0.5.2: the 0.5.0 -> 0.5.1 in-place takeover left the new
    /// core unable to host new Sessions (children died at spawn and were
    /// never reaped). Instead the older core keeps serving its Sessions, new
    /// Sessions run one process each, and once the old core is empty it is
    /// asked to exit so a current-build core starts. `UNPEEL_PTY_CORE_TAKEOVER=1`
    /// re-enables the in-place path (tests, and once it is proven).
    allow_takeover: bool,
    /// The older-build core pid we already announced as draining.
    drain_announced: Option<u32>,
}

/// `UNPEEL_PTY_CORE_TAKEOVER=1` opts back into in-place takeovers.
pub fn takeover_enabled() -> bool {
    std::env::var("UNPEEL_PTY_CORE_TAKEOVER").is_ok_and(|value| value.trim() == "1")
}

impl PtyCoreSupervisor {
    /// Gate off: a supervisor that manages nothing and publishes nothing.
    pub fn off(home: PathBuf) -> Self {
        Self {
            home,
            spawner: Box::new(|_| Err("PTY core supervision is off".into())),
            state: CoreState::Off,
            child: None,
            pid: None,
            sessions: None,
            launched_at: Instant::now(),
            start_warned: false,
            last_health: Instant::now(),
            next_spawn_at: None,
            rapid_failures: 0,
            last_exit: None,
            expected_build_id: None,
            probe_expected_build_id: false,
            takeover_from: None,
            takeover_started_at: Instant::now(),
            takeover_attempted: None,
            allow_takeover: takeover_enabled(),
            drain_announced: None,
        }
    }

    /// Pin the build id takeovers are measured against (tests).
    /// Tests pin the in-place takeover policy explicitly.
    pub fn set_allow_takeover(&mut self, allow: bool) {
        self.allow_takeover = allow;
    }

    pub fn set_expected_build_id(&mut self, build_id: Option<String>) {
        self.expected_build_id = build_id;
        self.probe_expected_build_id = false;
    }

    /// Honor the gate: adopt or spawn when `UNPEEL_PTY_CORE` is on.
    pub fn from_env(home: PathBuf) -> (Self, Vec<CoreEvent>) {
        if enabled() {
            let (mut supervisor, events) = Self::start(home, detached_core_spawner());
            supervisor.expected_build_id = expected_host_build_id();
            supervisor.probe_expected_build_id = true;
            let mut events = events;
            supervisor.check_build_skew(&mut events);
            (supervisor, events)
        } else {
            (Self::off(home), Vec::new())
        }
    }

    /// Adopt a live core named by the record, else spawn one.
    pub fn start(home: PathBuf, spawner: Spawner) -> (Self, Vec<CoreEvent>) {
        let mut supervisor = Self::off(home);
        supervisor.spawner = spawner;
        supervisor.state = CoreState::Starting;
        let mut events = Vec::new();
        if !supervisor.try_adopt(&mut events) {
            supervisor.spawn(&mut events, false);
        }
        (supervisor, events)
    }

    pub fn state(&self) -> CoreState {
        self.state
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn status(&self) -> Option<CoreStatus> {
        if self.state == CoreState::Off {
            return None;
        }
        Some(CoreStatus {
            state: self.state.as_str(),
            pid: self.pid,
            sessions: self.sessions,
            rapid_failures: self.rapid_failures,
            last_exit: self.last_exit.clone(),
            takeover_from: self.takeover_from,
        })
    }

    /// While adopted: if the adopted core's build differs from the binary
    /// we would launch, start a `--takeover` core. Never signals anything.
    fn check_build_skew(&mut self, events: &mut Vec<CoreEvent>) {
        if self.state != CoreState::Adopted {
            return;
        }
        if self.probe_expected_build_id {
            self.expected_build_id = expected_host_build_id();
        }
        let (Some(expected), Some(old_pid)) = (self.expected_build_id.clone(), self.pid) else {
            return;
        };
        let Some(record) = read_record(&self.home) else {
            return;
        };
        if record.pid != old_pid {
            return;
        }
        let Some(current) = record.host_build_id.clone() else {
            return;
        };
        if current == expected {
            return;
        }
        if !self.allow_takeover {
            self.drain_older_core(old_pid, &record.socket, events);
            return;
        }
        if self.takeover_attempted.as_ref() == Some(&(old_pid, expected.clone())) {
            return;
        }
        self.takeover_attempted = Some((old_pid, expected));
        match (self.spawner)(true) {
            Ok(child) => {
                let new_pid = child.id();
                self.child = Some(child);
                self.state = CoreState::HandingOff;
                self.takeover_from = Some(old_pid);
                self.takeover_started_at = Instant::now();
                self.last_health = Instant::now();
                events.push(CoreEvent::TakeoverStarted { old_pid, new_pid });
            }
            Err(error) => events.push(CoreEvent::Warning(format!(
                "could not start a takeover core: {error}"
            ))),
        }
    }

    /// An adopted core runs an older build and in-place takeover is off:
    /// leave it serving its Sessions (spawn routing already refuses it, so
    /// new Sessions run one process each) and, once it holds none, ask it to
    /// exit so the normal lost-core path starts a current-build core. Never
    /// signals anything.
    fn drain_older_core(&mut self, old_pid: u32, socket: &Path, events: &mut Vec<CoreEvent>) {
        let sessions = self.sessions.unwrap_or(u64::MAX);
        if sessions == 0 {
            match unpeel_core::pty_core::shutdown_at(socket, HEALTH_INTERVAL) {
                Ok(()) => events.push(CoreEvent::Warning(format!(
                    "PTY core pid {old_pid} runs an older build and holds no Sessions; asked it to exit so a current-build core can start"
                ))),
                Err(error) => events.push(CoreEvent::Warning(format!(
                    "PTY core pid {old_pid} runs an older build and holds no Sessions, but did not accept shutdown: {error}"
                ))),
            }
            self.drain_announced = None;
            return;
        }
        if self.drain_announced != Some(old_pid) {
            self.drain_announced = Some(old_pid);
            events.push(CoreEvent::Warning(format!(
                "PTY core pid {old_pid} runs an older build; it keeps serving its {sessions} Session(s) and new terminals run one process each until it drains (in-place takeover is off)"
            )));
        }
    }

    fn poll_handing_off(&mut self, events: &mut Vec<CoreEvent>) {
        let old_pid = self.takeover_from.unwrap_or(0);
        let new_pid = self.child.as_ref().map(|child| child.id()).unwrap_or(0);
        // A takeover child that exits did not become the core (it exits 2
        // on a refused/failed handoff; the old core resumed everything).
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                let status = describe_exit(status);
                self.child = None;
                self.takeover_from = None;
                self.state = CoreState::Adopted;
                self.last_health = Instant::now();
                events.push(CoreEvent::Warning(format!(
                    "takeover core pid {new_pid} exited ({status}); old core pid {old_pid} keeps serving"
                )));
                return;
            }
        }
        if self.last_health.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_health = Instant::now();
        if let Some(record) = read_record(&self.home) {
            if record.pid == new_pid {
                if let Ok(reply) = ping(&record.socket) {
                    self.state = CoreState::Live;
                    self.pid = Some(new_pid);
                    self.sessions = reply.sessions;
                    self.takeover_from = None;
                    self.rapid_failures = 0;
                    events.push(CoreEvent::TakenOver {
                        old_pid,
                        new_pid,
                        sessions: reply.sessions.unwrap_or(0),
                    });
                    return;
                }
            }
        }
        if self.takeover_started_at.elapsed() >= TAKEOVER_TIMEOUT {
            self.takeover_from = None;
            self.state = CoreState::Adopted;
            events.push(CoreEvent::Warning(format!(
                "takeover core pid {new_pid} did not publish the record within {}s; old core pid {old_pid} keeps serving (neither is signalled)",
                TAKEOVER_TIMEOUT.as_secs()
            )));
        }
    }

    /// Events describing that the worker is leaving the core alone. Called
    /// on worker shutdown; deliberately performs no process action.
    pub fn leave_running(&self) -> Vec<CoreEvent> {
        match (self.state, self.pid) {
            (CoreState::Off, _) | (CoreState::Failed, _) | (_, None) => Vec::new(),
            (_, Some(pid)) => vec![CoreEvent::LeftRunning {
                pid,
                sessions: self.sessions,
            }],
        }
    }

    /// One supervision step; cheap enough for every worker tick.
    pub fn poll(&mut self) -> Vec<CoreEvent> {
        if self.state == CoreState::HandingOff {
            let mut events = Vec::new();
            self.poll_handing_off(&mut events);
            return events;
        }
        let mut events = Vec::new();
        match self.state {
            CoreState::Off | CoreState::Failed => return events,
            CoreState::Starting => self.poll_starting(&mut events),
            CoreState::Live => self.poll_live(&mut events),
            CoreState::Adopted => self.poll_adopted(&mut events),
            CoreState::HandingOff => self.poll_handing_off(&mut events),
        }
        if self.state == CoreState::Starting
            && self.child.is_none()
            && self
                .next_spawn_at
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.next_spawn_at = None;
            if !self.try_adopt(&mut events) {
                self.spawn(&mut events, true);
            }
        }
        events
    }

    fn poll_starting(&mut self, events: &mut Vec<CoreEvent>) {
        if self.reap_own_child(events) {
            return;
        }
        if self.child.is_none() {
            return;
        }
        if self.last_health.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_health = Instant::now();
        if let Some(record) = read_record(&self.home) {
            if Some(record.pid) == self.pid {
                if let Ok(reply) = ping(&record.socket) {
                    self.state = CoreState::Live;
                    self.sessions = reply.sessions;
                    self.rapid_failures = 0;
                    events.push(CoreEvent::Ready {
                        pid: record.pid,
                        sessions: reply.sessions.unwrap_or(0),
                    });
                    return;
                }
            }
        }
        if !self.start_warned && self.launched_at.elapsed() >= START_WARN_AFTER {
            self.start_warned = true;
            events.push(CoreEvent::Warning(format!(
                "PTY core pid {} has not answered ping after {}s; still waiting (never killed)",
                self.pid.unwrap_or(0),
                START_WARN_AFTER.as_secs()
            )));
        }
    }

    fn poll_live(&mut self, events: &mut Vec<CoreEvent>) {
        if self.reap_own_child(events) {
            return;
        }
        self.refresh_health();
    }

    fn poll_adopted(&mut self, events: &mut Vec<CoreEvent>) {
        if self.last_health.elapsed() < HEALTH_INTERVAL {
            return;
        }
        self.last_health = Instant::now();
        let Some(pid) = self.pid else {
            return;
        };
        let record = read_record(&self.home);
        let alive = record
            .as_ref()
            .filter(|record| record.pid == pid)
            .and_then(adoptable);
        match alive {
            Some(reply) => {
                self.sessions = reply.sessions;
                self.check_build_skew(events);
            }
            None => {
                // Only a provably gone process counts as lost; an unanswered
                // ping on a live pid is left alone (the core may be busy).
                let started_at = record.as_ref().and_then(|record| record.pid_started_at);
                if recorded_pid_identity(pid, started_at) == PidIdentity::NotOurs {
                    self.pid = None;
                    self.sessions = None;
                    self.last_exit = Some("adopted core gone".into());
                    self.state = CoreState::Starting;
                    self.next_spawn_at = Some(Instant::now() + RESTART_DELAY);
                    events.push(CoreEvent::Lost { pid });
                }
            }
        }
    }

    fn refresh_health(&mut self) {
        if self.last_health.elapsed() < HEALTH_INTERVAL {
            return;
        }
        self.last_health = Instant::now();
        if let Some(record) = read_record(&self.home) {
            if Some(record.pid) == self.pid {
                if let Ok(reply) = ping(&record.socket) {
                    self.sessions = reply.sessions;
                }
            }
        }
    }

    /// `true` when our own child exited this step (state already updated).
    fn reap_own_child(&mut self, events: &mut Vec<CoreEvent>) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        let status = match child.try_wait() {
            Ok(Some(status)) => describe_exit(status),
            Ok(None) => return false,
            Err(error) => format!("wait failed: {error}"),
        };
        self.child = None;
        self.pid = None;
        self.sessions = None;
        self.last_exit = Some(status.clone());
        let rapid = self.launched_at.elapsed() < RAPID_EXIT_WINDOW;
        // A core that lost the `pty-core.lock` race exits 0 at once because
        // another core already serves this home: adopt that one instead of
        // counting a failure.
        if self.try_adopt(events) {
            return true;
        }
        if rapid {
            self.rapid_failures += 1;
        } else {
            self.rapid_failures = 0;
        }
        let gave_up = self.rapid_failures >= MAX_RAPID_FAILURES;
        if gave_up {
            self.state = CoreState::Failed;
            self.next_spawn_at = None;
        } else {
            self.state = CoreState::Starting;
            self.next_spawn_at = Some(Instant::now() + RESTART_DELAY);
        }
        events.push(CoreEvent::Exited {
            status,
            rapid_failures: self.rapid_failures,
            gave_up,
        });
        true
    }

    fn try_adopt(&mut self, events: &mut Vec<CoreEvent>) -> bool {
        let Some(record) = read_record(&self.home) else {
            return false;
        };
        let Some(reply) = adoptable(&record) else {
            return false;
        };
        self.state = CoreState::Adopted;
        self.pid = Some(record.pid);
        self.sessions = reply.sessions;
        self.rapid_failures = 0;
        self.next_spawn_at = None;
        self.last_health = Instant::now();
        events.push(CoreEvent::Adopted {
            pid: record.pid,
            sessions: reply.sessions.unwrap_or(0),
        });
        true
    }

    fn spawn(&mut self, events: &mut Vec<CoreEvent>, restart: bool) {
        match (self.spawner)(false) {
            Ok(child) => {
                let pid = child.id();
                self.child = Some(child);
                self.pid = Some(pid);
                self.sessions = None;
                self.state = CoreState::Starting;
                self.launched_at = Instant::now();
                self.last_health = Instant::now() - Duration::from_secs(1);
                self.start_warned = false;
                events.push(CoreEvent::Started { pid, restart });
            }
            Err(error) => {
                // No binary is not a crash loop; there is nothing to retry
                // until the worker itself restarts.
                self.state = CoreState::Failed;
                self.next_spawn_at = None;
                events.push(CoreEvent::SpawnFailed { error });
            }
        }
    }
}

impl Drop for PtyCoreSupervisor {
    /// Dropping the supervisor never signals the core: `Child` drop does not
    /// kill, and no `kill`/`wait` is issued here on purpose. Terminals do not
    /// depend on the worker's lifetime.
    fn drop(&mut self) {}
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("code {code}"),
        None => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A tiny listener answering `ping` per the shared contract.
    struct FakeCore {
        pid: u32,
        socket: PathBuf,
        pings: Arc<AtomicUsize>,
    }

    impl FakeCore {
        fn start(home: &Path, pid: u32, sessions: u64) -> Self {
            let socket = socket_path(home);
            let _ = std::fs::remove_file(&socket);
            let listener = UnixListener::bind(&socket).unwrap();
            let pings = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&pings);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    let _ = reader.read_line(&mut line);
                    let mut stream = stream;
                    if line.contains("\"ping\"") {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let _ = writeln!(
                            stream,
                            "{{\"ok\":true,\"pid\":{pid},\"sessions\":{sessions},\"host_build_id\":\"test\"}}"
                        );
                    } else {
                        let _ = writeln!(stream, "{{\"ok\":false,\"error\":\"unsupported\"}}");
                    }
                }
            });
            Self { pid, socket, pings }
        }

        fn write_record(&self, home: &Path, pid_started_at: Option<u64>) {
            let mut record = serde_json::json!({
                "pid": self.pid,
                "socket": self.socket,
                "host_build_id": "test",
                "protocol": 1,
            });
            if let Some(started) = pid_started_at {
                record["pid_started_at"] = serde_json::json!(started);
            }
            std::fs::write(record_path(home), record.to_string()).unwrap();
        }
    }

    #[test]
    fn older_build_core_drains_instead_of_takeover() {
        let home = short_home();
        let mut parked = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let core = FakeCore::start(home.path(), parked.id(), 2);
        core.write_record(home.path(), None);
        let (mut supervisor, events) =
            PtyCoreSupervisor::start(home.path().to_path_buf(), never_spawn());
        assert!(matches!(events.as_slice(), [CoreEvent::Adopted { .. }]));
        supervisor.set_allow_takeover(false);
        supervisor.set_expected_build_id(Some("newer".into()));

        // Holding Sessions: announced once, never taken over, never spawned.
        let mut events = Vec::new();
        supervisor.check_build_skew(&mut events);
        assert!(
            matches!(events.as_slice(), [CoreEvent::Warning(message)] if message.contains("keeps serving its 2 Session")),
            "{events:?}"
        );
        assert_eq!(supervisor.state(), CoreState::Adopted);
        let mut again = Vec::new();
        supervisor.check_build_skew(&mut again);
        assert!(again.is_empty(), "announced once: {again:?}");

        // Empty: asked to exit (the fake refuses; the supervisor only reports).
        supervisor.sessions = Some(0);
        let mut events = Vec::new();
        supervisor.check_build_skew(&mut events);
        assert!(
            matches!(events.as_slice(), [CoreEvent::Warning(message)] if message.contains("holds no Sessions")),
            "{events:?}"
        );
        assert_eq!(supervisor.state(), CoreState::Adopted);
        drop(core);
        let _ = parked.kill();
        let _ = parked.wait();
    }

    fn short_home() -> tempfile::TempDir {
        // Unix socket paths must stay short.
        tempfile::Builder::new()
            .prefix("upc-")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn never_spawn() -> Spawner {
        Box::new(|_| panic!("spawn must not be attempted"))
    }

    fn sleeping_spawner() -> Spawner {
        Box::new(|_| {
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|error| error.to_string())
        })
    }

    fn exiting_spawner(counter: Arc<AtomicUsize>) -> Spawner {
        Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::process::Command::new("true")
                .spawn()
                .map_err(|error| error.to_string())
        })
    }

    #[test]
    fn gate_parses_like_routing() {
        use std::ffi::OsStr;
        assert!(enabled_from(None));
        assert!(!enabled_from(Some(OsStr::new("0"))));
        assert!(!enabled_from(Some(OsStr::new(" 0 "))));
        assert!(enabled_from(Some(OsStr::new(""))));
        assert!(enabled_from(Some(OsStr::new("1"))));
        assert!(enabled_from(Some(OsStr::new("true"))));
    }

    #[test]
    fn off_supervisor_publishes_nothing_and_never_spawns() {
        let home = short_home();
        let mut supervisor = PtyCoreSupervisor::off(home.path().to_path_buf());
        assert_eq!(supervisor.state(), CoreState::Off);
        assert_eq!(supervisor.status(), None);
        assert!(supervisor.poll().is_empty());
        assert!(supervisor.leave_running().is_empty());
    }

    #[test]
    fn adopts_a_live_core_named_by_the_record() {
        let home = short_home();
        // Our own test process is a live pid that is not the worker's
        // "own" pid from the supervisor's perspective only if it differs;
        // use a child `sleep` so the pid is live and not std::process::id().
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let fake = FakeCore::start(home.path(), sleeper.id(), 3);
        fake.write_record(home.path(), None);

        let (mut supervisor, events) =
            PtyCoreSupervisor::start(home.path().to_path_buf(), never_spawn());
        assert_eq!(
            events,
            vec![CoreEvent::Adopted {
                pid: sleeper.id(),
                sessions: 3
            }]
        );
        assert_eq!(supervisor.state(), CoreState::Adopted);
        let status = supervisor.status().unwrap();
        assert_eq!(status.state, "adopted");
        assert_eq!(status.pid, Some(sleeper.id()));
        assert_eq!(status.sessions, Some(3));
        assert!(supervisor.poll().is_empty());
        assert_eq!(
            supervisor.leave_running(),
            vec![CoreEvent::LeftRunning {
                pid: sleeper.id(),
                sessions: Some(3)
            }]
        );
        assert!(fake.pings.load(Ordering::SeqCst) >= 1);
        let _ = sleeper.kill();
        let _ = sleeper.wait();
    }

    #[test]
    fn spawns_when_the_record_is_missing() {
        let home = short_home();
        let (mut supervisor, events) =
            PtyCoreSupervisor::start(home.path().to_path_buf(), sleeping_spawner());
        let pid = supervisor.pid().unwrap();
        assert_eq!(
            events,
            vec![CoreEvent::Started {
                pid,
                restart: false
            }]
        );
        assert_eq!(supervisor.state(), CoreState::Starting);
        assert_eq!(supervisor.status().unwrap().state, "starting");

        // Once the record and socket appear for that pid, it becomes live.
        std::thread::sleep(Duration::from_millis(300));
        let fake = FakeCore::start(home.path(), pid, 0);
        fake.write_record(home.path(), None);
        let events = supervisor.poll();
        assert_eq!(events, vec![CoreEvent::Ready { pid, sessions: 0 }]);
        assert_eq!(supervisor.state(), CoreState::Live);
        assert_eq!(supervisor.status().unwrap().state, "live");
        let child = supervisor.child.take().unwrap();
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn spawns_when_the_record_is_stale() {
        let home = short_home();
        // A record whose pid provably belongs to a dead process.
        let mut dead = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        let _ = dead.wait();
        let fake = FakeCore::start(home.path(), dead_pid, 0);
        fake.write_record(home.path(), None);
        let (supervisor, events) =
            PtyCoreSupervisor::start(home.path().to_path_buf(), sleeping_spawner());
        assert!(matches!(
            events[0],
            CoreEvent::Started { restart: false, .. }
        ));
        assert_eq!(supervisor.state(), CoreState::Starting);
        let mut supervisor = supervisor;
        let mut child = supervisor.child.take().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn rapid_exits_back_off_then_fail_without_signaling() {
        let home = short_home();
        let attempts = Arc::new(AtomicUsize::new(0));
        let (mut supervisor, _) =
            PtyCoreSupervisor::start(home.path().to_path_buf(), exiting_spawner(attempts.clone()));
        let mut exits = 0;
        let deadline = Instant::now() + Duration::from_secs(20);
        while supervisor.state() != CoreState::Failed && Instant::now() < deadline {
            for event in supervisor.poll() {
                if let CoreEvent::Exited { .. } = event {
                    exits += 1;
                }
            }
            // Collapse the real 2 s backoff for the test.
            if let Some(at) = supervisor.next_spawn_at {
                supervisor.next_spawn_at = Some(at - RESTART_DELAY);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(supervisor.state(), CoreState::Failed);
        assert_eq!(exits, MAX_RAPID_FAILURES as i32);
        assert_eq!(attempts.load(Ordering::SeqCst), MAX_RAPID_FAILURES as usize);
        let status = supervisor.status().unwrap();
        assert_eq!(status.state, "failed");
        assert_eq!(status.rapid_failures, MAX_RAPID_FAILURES);
        assert_eq!(status.last_exit.as_deref(), Some("code 0"));
        assert!(supervisor.leave_running().is_empty());
    }

    #[test]
    fn a_lock_losing_exit_adopts_instead_of_failing() {
        let home = short_home();
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let fake = FakeCore::start(home.path(), sleeper.id(), 1);
        let attempts = Arc::new(AtomicUsize::new(0));
        // No record yet → spawn; the "core" exits 0 while a record appears.
        let (mut supervisor, _) =
            PtyCoreSupervisor::start(home.path().to_path_buf(), exiting_spawner(attempts.clone()));
        fake.write_record(home.path(), None);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut events = Vec::new();
        while supervisor.state() == CoreState::Starting && Instant::now() < deadline {
            events.extend(supervisor.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(supervisor.state(), CoreState::Adopted);
        assert!(events
            .iter()
            .any(|event| matches!(event, CoreEvent::Adopted { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, CoreEvent::Exited { .. })));
        assert_eq!(supervisor.status().unwrap().rapid_failures, 0);
        let _ = sleeper.kill();
        let _ = sleeper.wait();
    }

    #[test]
    fn status_serializes_camel_case() {
        let status = CoreStatus {
            state: "adopted",
            pid: Some(77),
            sessions: Some(2),
            rapid_failures: 0,
            takeover_from: None,
            last_exit: None,
        };
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["state"], "adopted");
        assert_eq!(value["pid"], 77);
        assert_eq!(value["sessions"], 2);
        assert_eq!(value["rapidFailures"], 0);
        assert!(value.get("lastExit").is_none());
    }

    #[test]
    fn adopted_core_with_a_different_build_is_taken_over() {
        let home = short_home();
        let parked = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let old = FakeCore::start(home.path(), parked.id(), 3);
        let started = unpeel_core::session_host::recorded_process_start_time_ms(parked.id());
        old.write_record(home.path(), started);

        let takeover_flags = Arc::new(std::sync::Mutex::new(Vec::new()));
        let flags = Arc::clone(&takeover_flags);
        let spawner: Spawner = Box::new(move |takeover| {
            flags.lock().unwrap().push(takeover);
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|error| error.to_string())
        });
        let (mut supervisor, events) = PtyCoreSupervisor::start(home.path().to_path_buf(), spawner);
        assert!(matches!(events.as_slice(), [CoreEvent::Adopted { .. }]));
        assert_eq!(supervisor.state(), CoreState::Adopted);

        supervisor.set_allow_takeover(true);
        // Same build: no takeover.
        supervisor.set_expected_build_id(Some("test".into()));
        let mut events = Vec::new();
        supervisor.check_build_skew(&mut events);
        assert!(events.is_empty());
        assert!(takeover_flags.lock().unwrap().is_empty());

        // Different build: exactly one takeover spawn, flagged as such.
        supervisor.set_expected_build_id(Some("newer".into()));
        supervisor.check_build_skew(&mut events);
        assert!(
            matches!(events.as_slice(), [CoreEvent::TakeoverStarted { .. }]),
            "{events:?}"
        );
        assert_eq!(supervisor.state(), CoreState::HandingOff);
        assert_eq!(*takeover_flags.lock().unwrap(), vec![true]);
        assert_eq!(supervisor.status().unwrap().state, "handing_off");
        assert_eq!(
            supervisor.status().unwrap().takeover_from,
            Some(parked.id())
        );
        supervisor.check_build_skew(&mut events);
        assert_eq!(takeover_flags.lock().unwrap().len(), 1, "no takeover loop");

        // The takeover core publishes the record under its pid and answers
        // ping: the supervisor flips to live without touching the old core.
        let new_pid = supervisor.child.as_ref().unwrap().id();
        let new_core = FakeCore::start(home.path(), new_pid, 3);
        new_core.write_record(home.path(), None);
        std::thread::sleep(Duration::from_millis(300));
        let events = supervisor.poll();
        assert!(
            matches!(
                events.as_slice(),
                [CoreEvent::TakenOver { sessions: 3, .. }]
            ),
            "{events:?}"
        );
        assert_eq!(supervisor.state(), CoreState::Live);
        assert_eq!(supervisor.pid(), Some(new_pid));
        assert!(parked.id() > 0);
        let mut parked = parked;
        let _ = parked.kill();
        if let Some(mut child) = supervisor.child.take() {
            let _ = child.kill();
        }
    }
}
