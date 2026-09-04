//! Worker-owned supervision of the `unpeel-host __remote__` TLS/WSS terminal
//! streamer.
//!
//! The workspace worker owns the phone-facing data plane in the client-only
//! path, so it must also own the streamer's lifetime. This mirrors the policy
//! the compatibility Swift `RemoteControlManager` had: detect exit, respawn
//! after a short delay, count rapid failures, give up after a ceiling, and try
//! again when the paired-device set changes (a fresh pairing is the user
//! actively trying). Before every spawn a `remote.json` pid is reaped only when
//! `pid_started_at` proves it is the recorded streamer; never `pkill -f`.

use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use unpeel_core::app_paths;
use unpeel_core::session_host::{recorded_pid_identity, PidIdentity};

/// Delay before respawning an exited streamer.
pub(crate) const RESTART_DELAY: Duration = Duration::from_secs(2);
/// An exit sooner than this after launch counts as a rapid failure.
pub(crate) const RAPID_EXIT_WINDOW: Duration = Duration::from_secs(10);
/// Consecutive rapid failures after which supervision stops until the
/// paired-device set changes or the mobile server restarts.
pub(crate) const MAX_RAPID_FAILURES: u32 = 5;
/// How long a verified stale instance gets to leave after SIGTERM before the
/// replacement is spawned regardless.
const STALE_EXIT_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamerState {
    /// The supervised child is running.
    Live,
    /// The child exited; a respawn is scheduled.
    Restarting,
    /// Crash-loop ceiling reached; waiting for a pairing change.
    GaveUp,
    /// No `unpeel-host` binary could be resolved or spawned.
    Unavailable,
}

impl StreamerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Restarting => "restarting",
            Self::GaveUp => "gaveUp",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Published into `serve.json` as `terminalStreamer`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamerStatus {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub restarts: u32,
    pub rapid_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamerEvent {
    Started {
        pid: u32,
        restart: bool,
    },
    Exited {
        status: String,
        rapid_failures: u32,
        gave_up: bool,
    },
    /// A `remote.json` pid was proven to be an unsupervised earlier streamer
    /// and asked to exit before this worker spawned its own.
    ReapedStale {
        pid: u32,
    },
    SpawnFailed {
        error: String,
    },
}

pub struct RemoteStreamer {
    child: Option<Child>,
    launched_at: Instant,
    next_spawn_at: Option<Instant>,
    rapid_failures: u32,
    restarts: u32,
    gave_up: bool,
    unavailable: bool,
    last_exit: Option<String>,
    stopped: bool,
}

impl RemoteStreamer {
    /// Create the supervisor and spawn the first streamer immediately.
    pub fn start() -> (Self, Vec<StreamerEvent>) {
        let mut supervisor = Self {
            child: None,
            launched_at: Instant::now(),
            next_spawn_at: None,
            rapid_failures: 0,
            restarts: 0,
            gave_up: false,
            unavailable: false,
            last_exit: None,
            stopped: false,
        };
        let mut events = Vec::new();
        supervisor.spawn(&mut events, false);
        (supervisor, events)
    }

    /// A supervisor that never spawns; for unit-test fixtures of the mobile
    /// server that must not start a real `__remote__` process.
    #[cfg(test)]
    pub(crate) fn stopped_for_tests() -> Self {
        Self {
            child: None,
            launched_at: Instant::now(),
            next_spawn_at: None,
            rapid_failures: 0,
            restarts: 0,
            gave_up: false,
            unavailable: false,
            last_exit: None,
            stopped: true,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    pub fn state(&self) -> StreamerState {
        if self.child.is_some() {
            StreamerState::Live
        } else if self.gave_up {
            StreamerState::GaveUp
        } else if self.unavailable {
            StreamerState::Unavailable
        } else {
            StreamerState::Restarting
        }
    }

    pub fn status(&self) -> StreamerStatus {
        let port = self.pid().and_then(|pid| {
            let (recorded_pid, port, _) = read_remote_record();
            (recorded_pid == Some(pid)).then_some(port).flatten()
        });
        StreamerStatus {
            state: self.state().as_str(),
            pid: self.pid(),
            port,
            restarts: self.restarts,
            rapid_failures: self.rapid_failures,
            last_exit: self.last_exit.clone(),
        }
    }

    /// One supervision step: reap an exited child, schedule or perform a
    /// respawn. Cheap enough for every worker tick.
    pub fn poll(&mut self) -> Vec<StreamerEvent> {
        let mut events = Vec::new();
        if self.stopped {
            return events;
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    let exit = describe_exit(status);
                    self.last_exit = Some(exit.clone());
                    if self.launched_at.elapsed() >= RAPID_EXIT_WINDOW {
                        self.rapid_failures = 0;
                    } else {
                        self.rapid_failures += 1;
                    }
                    if self.rapid_failures >= MAX_RAPID_FAILURES {
                        self.gave_up = true;
                        self.next_spawn_at = None;
                    } else {
                        self.next_spawn_at = Some(Instant::now() + RESTART_DELAY);
                    }
                    events.push(StreamerEvent::Exited {
                        status: exit,
                        rapid_failures: self.rapid_failures,
                        gave_up: self.gave_up,
                    });
                }
                Ok(None) => return events,
                Err(error) => {
                    // The child handle is unusable; treat it as gone so the
                    // next spawn can reap through `remote.json` identity.
                    self.child = None;
                    self.last_exit = Some(format!("wait failed: {error}"));
                    self.next_spawn_at = Some(Instant::now() + RESTART_DELAY);
                }
            }
        }
        if self.gave_up || self.unavailable {
            return events;
        }
        if self
            .next_spawn_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.next_spawn_at = None;
            self.spawn(&mut events, true);
        }
        events
    }

    /// The paired-device set changed: a fresh pairing is the user actively
    /// trying, so a crash-looped or unavailable streamer gets a new chance.
    /// Returns the events of an immediate respawn attempt.
    pub fn retry_after_pairing_change(&mut self) -> Vec<StreamerEvent> {
        let mut events = Vec::new();
        if self.stopped || self.child.is_some() {
            return events;
        }
        self.gave_up = false;
        self.unavailable = false;
        self.rapid_failures = 0;
        self.next_spawn_at = None;
        self.spawn(&mut events, true);
        events
    }

    /// Stop the supervised child and never respawn. Mirrors the previous
    /// unsupervised shutdown exactly: kill, then wait.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.next_spawn_at = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn spawn(&mut self, events: &mut Vec<StreamerEvent>, restart: bool) {
        if let Some(pid) = reap_stale_streamer(self.pid()) {
            events.push(StreamerEvent::ReapedStale { pid });
        }
        let binary = match unpeel_core::session_ops::resolve_host_binary() {
            Ok(binary) => binary,
            Err(error) => {
                self.unavailable = true;
                events.push(StreamerEvent::SpawnFailed { error });
                return;
            }
        };
        match std::process::Command::new(binary)
            .arg("__remote__")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => {
                let pid = child.id();
                self.child = Some(child);
                self.launched_at = Instant::now();
                self.unavailable = false;
                if restart {
                    self.restarts += 1;
                }
                events.push(StreamerEvent::Started { pid, restart });
            }
            Err(error) => {
                // A missing binary is not a crash loop: keep the child slot
                // empty and wait for a pairing change rather than spinning.
                self.unavailable = true;
                events.push(StreamerEvent::SpawnFailed {
                    error: error.to_string(),
                });
            }
        }
    }
}

impl Drop for RemoteStreamer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn remote_record_path() -> PathBuf {
    app_paths::unpeel_home().join("remote.json")
}

/// (pid, port, pid_started_at) from `remote.json`, if readable.
fn read_remote_record() -> (Option<u32>, Option<u16>, Option<u64>) {
    let Ok(raw) = std::fs::read(remote_record_path()) else {
        return (None, None, None);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return (None, None, None);
    };
    let pid = value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok());
    let port = value
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .and_then(|port| u16::try_from(port).ok());
    let started_at = value
        .get("pid_started_at")
        .and_then(serde_json::Value::as_u64);
    (pid, port, started_at)
}

/// If `remote.json` names a live streamer that is not this supervisor's own
/// child and its start time proves it is the recorded process, ask it to exit
/// so the replacement does not leave two streamers alive. Anything unproven
/// is left alone: the new streamer simply rewrites `remote.json`.
fn reap_stale_streamer(own_pid: Option<u32>) -> Option<u32> {
    let (pid, _, started_at) = read_remote_record();
    let pid = pid?;
    if pid == 0 || Some(pid) == own_pid || pid == std::process::id() {
        return None;
    }
    if recorded_pid_identity(pid, started_at) != PidIdentity::Matches {
        return None;
    }
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + STALE_EXIT_GRACE;
    while Instant::now() < deadline {
        if recorded_pid_identity(pid, started_at) != PidIdentity::Matches {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(pid)
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
