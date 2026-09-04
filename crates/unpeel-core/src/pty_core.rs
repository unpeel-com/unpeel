//! The shared PTY core: `unpeel-host __pty_core__` hosts N Sessions in ONE
//! process by running `session_host::run_host` once per Session on its own
//! thread. Everything a Session publishes — `manifest.json`, `session.sock`,
//! `output.bin`, the attach protocol, hook env — is byte-for-byte what a
//! per-process `__session_host__` publishes; only the process boundary moves.
//!
//! Contract (see `docs/agents/pty-core.md`):
//!
//! - one instance per `UNPEEL_HOME`, held by an flock on `pty-core.lock`; a
//!   second instance exits 0 immediately;
//! - `pty-core.json` records `{pid, pid_started_at, socket, host_build_id,
//!   protocol}` after bind and is removed on clean exit;
//! - `pty-core.sock` (mode 0600) speaks one newline-delimited JSON request
//!   per connection: `ping`, `launch`, `shutdown`;
//! - `launch` replies only after the Session's preliminary manifest is on
//!   disk, so an `unpeel-attach` started in parallel still finds it inside
//!   its short manifest wait;
//! - `shutdown` succeeds only with zero hosted Sessions. Nothing may ever
//!   stop a core that hosts live Sessions.
//!
//! Spawn routing lives in `session_host::spawn_host_process_from_launch_file`:
//! it tries the core through [`try_launch_via_core`] and falls back to the
//! per-process spawn on any failure. `UNPEEL_PTY_CORE=0` disables the core
//! path everywhere.

use crate::app_paths::unpeel_home;
use crate::hook_assets::append_trace_log_line;
use crate::session_host::{self, current_host_build_id, process_start_time_ms, SessionHostLaunch};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const PTY_CORE_ARG: &str = "__pty_core__";
pub const PTY_CORE_PROTOCOL: u32 = 1;
const LOCK_FILE: &str = "pty-core.lock";
const RECORD_FILE: &str = "pty-core.json";
const SOCKET_FILE: &str = "pty-core.sock";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const LAUNCH_REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the core waits for a Session thread to publish its preliminary
/// manifest before answering the launch request as failed.
const MANIFEST_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

static IN_CORE_PROCESS: AtomicBool = AtomicBool::new(false);

/// True inside a `__pty_core__` process. `run_host` consults this to publish
/// a `pid: None` preliminary manifest instead of its own-pid placeholder.
pub fn is_core_process() -> bool {
    IN_CORE_PROCESS.load(Ordering::Relaxed)
}

pub fn lock_path() -> PathBuf {
    unpeel_home().join(LOCK_FILE)
}

pub fn record_path() -> PathBuf {
    unpeel_home().join(RECORD_FILE)
}

pub fn socket_path() -> PathBuf {
    unpeel_home().join(SOCKET_FILE)
}

/// Whether spawn routing may try the core at all (`UNPEEL_PTY_CORE=0` forces
/// per-process hosting everywhere).
pub fn routing_enabled() -> bool {
    std::env::var("UNPEEL_PTY_CORE").map_or(true, |value| value.trim() != "0")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PtyCoreRecord {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started_at: Option<u64>,
    pub socket: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_build_id: Option<String>,
    pub protocol: u32,
}

pub fn load_record() -> Option<PtyCoreRecord> {
    let raw = fs::read(record_path()).ok()?;
    serde_json::from_slice(&raw).ok()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Ping,
    Launch {
        launch_file: String,
    },
    Shutdown,
    /// A new core asks for every Session plus the core's own lock and
    /// listener fds; the rest of the connection is framed (`fd_pass`).
    Handoff {
        #[serde(default)]
        build_id: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sessions: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Reply {
    fn error(message: impl Into<String>) -> Self {
        Reply {
            ok: false,
            pid: None,
            sessions: None,
            host_build_id: None,
            session_id: None,
            error: Some(message.into()),
        }
    }
}

// ───────────────────────────── client side ─────────────────────────────

#[derive(Debug)]
pub enum CoreLaunchError {
    /// No core socket for this home, or routing is disabled: not an error
    /// worth tracing, the per-process spawn is simply the path today.
    Unavailable,
    /// A core exists but the launch did not go through; the caller falls
    /// back and logs this.
    Failed(String),
}

impl std::fmt::Display for CoreLaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreLaunchError::Unavailable => write!(f, "pty core unavailable"),
            CoreLaunchError::Failed(message) => write!(f, "{message}"),
        }
    }
}

/// Ask the running core (if any) to host the Session described by
/// `launch_file`. `Ok` carries the Session id once its preliminary manifest
/// is on disk. The launch file is consumed by the core exactly as a
/// per-process Host consumes it.
pub fn try_launch_via_core(launch_file: &Path) -> Result<String, CoreLaunchError> {
    if !routing_enabled() {
        return Err(CoreLaunchError::Unavailable);
    }
    let socket = socket_path();
    if !socket.exists() {
        return Err(CoreLaunchError::Unavailable);
    }
    let launch_file = if launch_file.is_absolute() {
        launch_file.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| CoreLaunchError::Failed(format!("current dir: {e}")))?
            .join(launch_file)
    };
    // Remember the Session id up front: the core deletes the launch file the
    // moment it commits to hosting the Session, so if the reply is lost or
    // late the file's absence proves the core owns the launch and a
    // per-process fallback would double-host it.
    let session_id = fs::read(&launch_file)
        .ok()
        .and_then(|raw| serde_json::from_slice::<SessionHostLaunch>(&raw).ok())
        .map(|launch| launch.session.id);
    let request = Request::Launch {
        launch_file: launch_file.to_string_lossy().to_string(),
    };
    let reply = match request_at(&socket, &request, LAUNCH_REPLY_TIMEOUT) {
        Ok(reply) => reply,
        Err(error) => {
            if let (false, Some(session_id)) = (launch_file.exists(), session_id) {
                append_trace_log_line(&format!(
                    "pty-core launch reply lost for {session_id} ({error}); the core consumed the launch, not falling back"
                ));
                return Ok(session_id);
            }
            return Err(CoreLaunchError::Failed(error));
        }
    };
    match (reply.ok, reply.session_id) {
        (true, Some(session_id)) => Ok(session_id),
        (true, None) => Err(CoreLaunchError::Failed(
            "core accepted the launch without a session id".into(),
        )),
        (false, _) => Err(CoreLaunchError::Failed(
            reply
                .error
                .unwrap_or_else(|| "core refused the launch".into()),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreStatus {
    pub pid: u32,
    pub sessions: usize,
    pub host_build_id: Option<String>,
}

/// `ping` the core for this home.
pub fn ping(timeout: Duration) -> Result<CoreStatus, String> {
    ping_at(&socket_path(), timeout)
}

pub fn ping_at(socket: &Path, timeout: Duration) -> Result<CoreStatus, String> {
    let reply = request_at(socket, &Request::Ping, timeout)?;
    if !reply.ok {
        return Err(reply.error.unwrap_or_else(|| "ping refused".into()));
    }
    Ok(CoreStatus {
        pid: reply.pid.ok_or("ping reply without pid")?,
        sessions: reply.sessions.unwrap_or(0),
        host_build_id: reply.host_build_id,
    })
}

/// Ask the core to exit. `Ok(())` means it accepted (it hosts no Sessions);
/// `Err` carries `busy` with the live count otherwise.
pub fn request_shutdown(timeout: Duration) -> Result<(), String> {
    shutdown_at(&socket_path(), timeout)
}

pub fn shutdown_at(socket: &Path, timeout: Duration) -> Result<(), String> {
    let reply = request_at(socket, &Request::Shutdown, timeout)?;
    if reply.ok {
        Ok(())
    } else {
        Err(match (reply.error, reply.sessions) {
            (Some(error), Some(count)) => format!("{error} ({count} sessions)"),
            (Some(error), None) => error,
            (None, _) => "shutdown refused".into(),
        })
    }
}

/// Shut down every PTY core whose `pty-core.sock` lives under `root`
/// (any depth: a fixture root may hold a machine home, per-workspace homes,
/// and `profiles/<name>` homes). Returns the socket paths that were found.
///
/// Test-fixture teardown is the only caller: cores are detached on purpose
/// and outlive the worker that started them, so a fixture that started a
/// worker must ask its cores to exit before removing the home, or every
/// `cargo test` run leaks one core per fixture home. A core refuses
/// `shutdown` while it hosts Sessions; the fixture owns those Sessions, so
/// this keeps asking until they are gone or `timeout` passes, then waits for
/// the socket the core unlinks on exit. Best effort: an already-gone core is
/// not an error and nothing is ever signalled by pid.
pub fn shutdown_cores_under(root: &Path, timeout: Duration) -> Vec<PathBuf> {
    fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 6 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                // Session dirs never hold a core socket; skip the hot path.
                if path.file_name().is_some_and(|name| name == "app-sessions") {
                    continue;
                }
                collect(&path, depth + 1, out);
            } else if path.file_name().is_some_and(|name| name == SOCKET_FILE) {
                out.push(path);
            }
        }
    }

    let mut sockets = Vec::new();
    collect(root, 0, &mut sockets);
    let deadline = std::time::Instant::now() + timeout;
    for socket in &sockets {
        loop {
            match shutdown_at(socket, Duration::from_secs(2)) {
                Ok(()) => break,
                // A busy core still hosts fixture Sessions that are exiting;
                // ask again. Anything else means it is already gone.
                Err(error) if error.contains("busy") => {}
                Err(_) => break,
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        while socket.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    sockets
}

#[cfg(unix)]
fn request_at(socket: &Path, request: &Request, timeout: Duration) -> Result<Reply, String> {
    use std::os::unix::net::UnixStream;

    let stream = connect_with_timeout(socket, CONNECT_TIMEOUT)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("core read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(CONNECT_TIMEOUT))
        .map_err(|e| format!("core write timeout: {e}"))?;
    let mut body = serde_json::to_string(request).map_err(|e| format!("encode: {e}"))?;
    body.push('\n');
    let mut writer = &stream;
    writer
        .write_all(body.as_bytes())
        .map_err(|e| format!("core write: {e}"))?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| format!("core read: {e}"))?;
    if line.trim().is_empty() {
        return Err("core closed the connection without a reply".into());
    }
    let _ = UnixStream::shutdown(&stream, std::net::Shutdown::Both);
    serde_json::from_str(line.trim()).map_err(|e| format!("core reply: {e}"))
}

#[cfg(not(unix))]
fn request_at(_socket: &Path, _request: &Request, _timeout: Duration) -> Result<Reply, String> {
    Err("pty core requires unix sockets".into())
}

/// `UnixStream::connect` has no timeout of its own; a connect to a live
/// listener either succeeds at once or fails with ECONNREFUSED/ENOENT. A
/// listener whose backlog is full makes connect block, so bound the attempt
/// by retrying a non-blocking connect until the deadline instead.
#[cfg(unix)]
fn connect_with_timeout(
    socket: &Path,
    timeout: Duration,
) -> Result<std::os::unix::net::UnixStream, String> {
    use std::os::unix::net::UnixStream;

    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == Some(libc::EAGAIN) =>
            {
                if Instant::now() >= deadline {
                    return Err("core connect timed out".into());
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(format!("core connect: {error}")),
        }
    }
}

// ───────────────────────────── core process ─────────────────────────────

/// Called from the Session's teardown when it ends (or from the runner
/// itself when its setup failed after taking ownership).
pub(crate) type OnExit = Box<dyn FnOnce(Result<(), String>) + Send>;

/// What the core does with one parsed launch: set the Session up and hand
/// it to the shared reactor, returning once it is registered. `on_exit`
/// fires when the Session ends. Production runs the real `start_host`;
/// unit tests inject a fake so they never touch the global `UNPEEL_HOME`
/// or spawn PTYs.
pub(crate) type SessionRunner =
    Arc<dyn Fn(SessionHostLaunch, OnExit) -> Result<(), String> + Send + Sync>;

struct CoreState {
    live: Mutex<HashSet<String>>,
    shutting_down: AtomicBool,
    runner: SessionRunner,
    home: PathBuf,
    /// Where `<id>/manifest.json` lands; the launch reply waits on it.
    sessions_root: PathBuf,
    /// Launches whose setup thread has not registered with the reactor
    /// yet; a handoff waits for zero (a half-built Session cannot move).
    launching: AtomicUsize,
    /// A handoff is in progress: launches and shutdown answer `handing_off`.
    handing_off: AtomicBool,
    /// Every Session moved to a new core; the accept loop exits without
    /// removing the socket or the record, which the new core now owns.
    handed_off: AtomicBool,
    /// Our lock and listener fds, passed along with the Sessions.
    lock_fd: std::os::unix::io::RawFd,
    listener_fd: std::os::unix::io::RawFd,
}

impl CoreState {
    fn live_count(&self) -> usize {
        self.live.lock().map(|live| live.len()).unwrap_or(0)
    }
}

/// Entry point for `unpeel-host __pty_core__`. Returns `Ok` when this
/// process was not needed (another core holds the lock) or after a clean
/// `shutdown`.
pub fn run_from_args(args: &[String]) -> Result<(), String> {
    let home = crate::app_paths::ensure_unpeel_home()
        .map_err(|e| format!("Failed to prepare UNPEEL_HOME: {e}"))?;
    IN_CORE_PROCESS.store(true, Ordering::Relaxed);
    let sessions_root = crate::app_paths::app_sessions_root();
    let runner: SessionRunner = Arc::new(session_host::start_host);
    if args.iter().any(|arg| arg == TAKEOVER_ARG) {
        #[cfg(unix)]
        {
            return run_takeover(home, sessions_root, runner);
        }
        #[cfg(not(unix))]
        {
            return Err("pty core requires unix sockets".into());
        }
    }
    run_core_at(home, sessions_root, runner)
}

/// `unpeel-host __pty_core__ --takeover`: replace the running core without
/// restarting any terminal (see `docs/agents/pty-core.md`, "Handoff").
pub const TAKEOVER_ARG: &str = "--takeover";

/// Host Sessions for `home` until a `shutdown` request succeeds. Everything
/// the core owns lives directly under `home` so an isolated test home gets
/// its own lock, record, and socket.
pub(crate) fn run_core_at(
    home: PathBuf,
    sessions_root: PathBuf,
    runner: SessionRunner,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        run_core_unix(home, sessions_root, runner)
    }
    #[cfg(not(unix))]
    {
        let _ = (home, sessions_root, runner);
        Err("pty core requires unix sockets".into())
    }
}

#[cfg(unix)]
fn run_core_unix(
    home: PathBuf,
    sessions_root: PathBuf,
    runner: SessionRunner,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixListener;

    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(home.join(LOCK_FILE))
        .map_err(|e| format!("Failed to open pty core lock: {e}"))?;
    if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        // Another core already serves this home. Exiting 0 keeps every
        // launcher's "start a core, then launch" sequence idempotent.
        return Ok(());
    }

    let socket = home.join(SOCKET_FILE);
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .map_err(|e| format!("Failed to bind {}: {e}", socket.display()))?;
    let _ = fs::set_permissions(&socket, fs::Permissions::from_mode(0o600));
    write_record(&home)?;
    append_trace_log_line(&format!(
        "pty-core started pid={} home={}",
        std::process::id(),
        home.display()
    ));
    let state = Arc::new(CoreState {
        live: Mutex::new(HashSet::new()),
        shutting_down: AtomicBool::new(false),
        runner,
        home: home.clone(),
        sessions_root,
        launching: AtomicUsize::new(0),
        handing_off: AtomicBool::new(false),
        handed_off: AtomicBool::new(false),
        lock_fd: lock_file.as_raw_fd(),
        listener_fd: listener.as_raw_fd(),
    });
    serve_core(home, lock_file, listener, state)
}

/// Publish `pty-core.json` for this process (after bind, or after a
/// takeover committed).
#[cfg(unix)]
fn write_record(home: &Path) -> Result<(), String> {
    let pid = std::process::id();
    let record = PtyCoreRecord {
        pid,
        pid_started_at: process_start_time_ms(pid),
        socket: home.join(SOCKET_FILE).to_string_lossy().to_string(),
        host_build_id: current_host_build_id(),
        protocol: PTY_CORE_PROTOCOL,
    };
    let record_path = home.join(RECORD_FILE);
    let tmp = record_path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(&record).map_err(|e| format!("encode record: {e}"))?,
    )
    .and_then(|_| fs::rename(&tmp, &record_path))
    .map_err(|e| format!("Failed to write {}: {e}", record_path.display()))
}

/// The accept loop shared by a freshly bound core and a core that took
/// over: one request thread per connection until `shutdown` (0 Sessions)
/// or until every Session has been handed to a successor.
#[cfg(unix)]
fn serve_core(
    home: PathBuf,
    lock_file: fs::File,
    listener: std::os::unix::net::UnixListener,
    state: Arc<CoreState>,
) -> Result<(), String> {
    let pid = std::process::id();
    // The preliminary manifest resolves App branding through the installed
    // App index, whose first build probes the login shell's PATH (seconds on
    // a slow profile). Per-process Hosts pay that on every launch; the core
    // pays it once, and warming it here keeps even the first launch reply
    // well inside the client's wait.
    thread::Builder::new()
        .name("pty-core-warm".into())
        .spawn(|| {
            let _ = crate::app_runtime::app_for_launch_command("sh");
        })
        .map_err(|e| format!("Failed to spawn warm-up thread: {e}"))?;
    let socket = home.join(SOCKET_FILE);
    let record_path = home.join(RECORD_FILE);

    for connection in listener.incoming() {
        let stream = match connection {
            Ok(stream) => stream,
            Err(error) => {
                append_trace_log_line(&format!("pty-core accept error: {error}"));
                continue;
            }
        };
        let request_state = Arc::clone(&state);
        thread::Builder::new()
            .name("pty-core-request".into())
            .spawn(move || serve_connection(stream, request_state))
            .map_err(|e| format!("Failed to spawn request thread: {e}"))?;
        if state.shutting_down.load(Ordering::SeqCst) || state.handed_off.load(Ordering::SeqCst) {
            break;
        }
    }

    if state.handed_off.load(Ordering::SeqCst) {
        // The successor owns the socket, the record, the lock, and every
        // Session. Let in-flight teardowns of Sessions that were ending
        // during the handoff finish, then leave everything in place.
        let deadline = Instant::now() + Duration::from_secs(30);
        while state.live_count() > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        append_trace_log_line(&format!("pty-core handed off pid={pid}"));
        drop(listener);
        drop(lock_file);
        return Ok(());
    }

    let _ = fs::remove_file(&socket);
    let _ = fs::remove_file(&record_path);
    append_trace_log_line(&format!("pty-core exited pid={pid}"));
    // The lock file stays; releasing the flock happens when `lock_file`
    // drops with this frame.
    drop(lock_file);
    Ok(())
}

/// New-core side of a handoff: connect to the running core, receive every
/// Session with its fds, rebuild them on this process's reactor, publish
/// the record, commit, then serve on the inherited listener.
#[cfg(unix)]
fn run_takeover(
    home: PathBuf,
    sessions_root: PathBuf,
    runner: SessionRunner,
) -> Result<(), String> {
    use crate::session_host::fd_pass::{recv_message, send_message};
    use std::io::Read;
    use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::net::UnixListener;

    let socket = home.join(SOCKET_FILE);
    let mut stream = connect_with_timeout(&socket, CONNECT_TIMEOUT)
        .map_err(|e| format!("no core to take over at {}: {e}", socket.display()))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let request = serde_json::to_string(&Request::Handoff {
        build_id: current_host_build_id(),
    })
    .map_err(|e| format!("encode: {e}"))?;
    {
        let mut writer = &stream;
        writer
            .write_all(format!("{request}\n").as_bytes())
            .map_err(|e| format!("handoff request: {e}"))?;
    }
    let (header, core_fds) =
        recv_message(&mut stream).map_err(|e| format!("handoff header: {e}"))?;
    let header: serde_json::Value =
        serde_json::from_slice(&header).map_err(|e| format!("handoff header: {e}"))?;
    if header.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(format!(
            "core refused the handoff: {}",
            header
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ));
    }
    let count = header.get("sessions").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    if core_fds.len() != 2 {
        return Err(format!(
            "handoff header carried {} fds, expected lock + listener",
            core_fds.len()
        ));
    }
    let mut core_fds = core_fds;
    let listener_fd = core_fds.pop().unwrap();
    let lock_fd = core_fds.pop().unwrap();
    let lock_file = unsafe { fs::File::from_raw_fd(lock_fd.into_raw_fd()) };
    let listener = UnixListener::from(listener_fd);
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("Failed to configure inherited listener: {e}"))?;

    let state = Arc::new(CoreState {
        live: Mutex::new(HashSet::new()),
        shutting_down: AtomicBool::new(false),
        runner,
        home: home.clone(),
        sessions_root,
        launching: AtomicUsize::new(0),
        handing_off: AtomicBool::new(false),
        handed_off: AtomicBool::new(false),
        lock_fd: lock_file.as_raw_fd(),
        listener_fd: listener.as_raw_fd(),
    });

    let services = crate::session_host::core_reactor::services()?;
    let mut adopted = 0usize;
    let outcome = (|| -> Result<(), String> {
        for _ in 0..count {
            let (meta, fds) =
                recv_message(&mut stream).map_err(|e| format!("session handoff: {e}"))?;
            let meta: crate::session_host::session_io::SessionHandoff =
                serde_json::from_slice(&meta).map_err(|e| format!("session handoff: {e}"))?;
            let mut snapshot = vec![0u8; meta.snapshot_len as usize];
            {
                let mut reader = &stream;
                reader
                    .read_exact(&mut snapshot)
                    .map_err(|e| format!("snapshot for {}: {e}", meta.id))?;
            }
            let session_id = meta.id.clone();
            if let Ok(mut live) = state.live.lock() {
                live.insert(session_id.clone());
            }
            let exit_state = Arc::clone(&state);
            let exit_session_id = session_id.clone();
            let on_exit: OnExit = Box::new(move |result: Result<(), String>| {
                if let Err(error) = result {
                    append_trace_log_line(&format!(
                        "pty-core session {exit_session_id} failed: {error}"
                    ));
                    session_host::mark_manifest_exited(&exit_session_id);
                }
                if let Ok(mut live) = exit_state.live.lock() {
                    live.remove(&exit_session_id);
                }
            });
            let rebuilt = crate::session_host::session_io::rebuild_from_handoff(
                meta, snapshot, fds, services, on_exit,
            )?;
            crate::session_host::core_reactor::adopt_session(rebuilt)?;
            adopted += 1;
        }
        write_record(&home)?;
        Ok(())
    })();

    match outcome {
        Ok(()) => {
            let commit = serde_json::json!({ "ok": true, "adopted": adopted }).to_string();
            send_message(&mut stream, commit.as_bytes(), &[])
                .map_err(|e| format!("commit: {e}"))?;
            append_trace_log_line(&format!(
                "pty-core takeover pid={} adopted {adopted} sessions",
                std::process::id()
            ));
            serve_core(home, lock_file, listener, state)
        }
        Err(error) => {
            // Never a half-handoff: the old core keeps everything. Our
            // partially rebuilt Sessions are dropped without teardown
            // (forgetting the fds; the old core's copies stay live).
            let refusal = serde_json::json!({ "ok": false, "error": error }).to_string();
            let _ = send_message(&mut stream, refusal.as_bytes(), &[]);
            std::process::exit(2);
        }
    }
}

#[cfg(unix)]
fn serve_connection(stream: std::os::unix::net::UnixStream, state: Arc<CoreState>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let reply = match serde_json::from_str::<Request>(line.trim()) {
        Ok(Request::Ping) => Reply {
            ok: true,
            pid: Some(std::process::id()),
            sessions: Some(state.live_count()),
            host_build_id: current_host_build_id(),
            session_id: None,
            error: None,
        },
        Ok(Request::Handoff { build_id }) => {
            handle_handoff(stream, &state, build_id);
            return;
        }
        Ok(Request::Launch { .. }) if state.handing_off.load(Ordering::SeqCst) => {
            Reply::error("handing_off")
        }
        Ok(Request::Shutdown) if state.handing_off.load(Ordering::SeqCst) => {
            Reply::error("handing_off")
        }
        Ok(Request::Launch { launch_file }) => match launch_session(&state, &launch_file) {
            Ok(session_id) => Reply {
                ok: true,
                pid: None,
                sessions: None,
                host_build_id: None,
                session_id: Some(session_id),
                error: None,
            },
            Err(error) => Reply::error(error),
        },
        Ok(Request::Shutdown) => {
            let mut reply = Reply::error("busy");
            // Hold the live set while deciding so a concurrent launch
            // cannot slip in between the count and the shutdown decision.
            if let Ok(live) = state.live.lock() {
                if live.is_empty() {
                    state.shutting_down.store(true, Ordering::SeqCst);
                    reply = Reply {
                        ok: true,
                        pid: Some(std::process::id()),
                        sessions: Some(0),
                        host_build_id: None,
                        session_id: None,
                        error: None,
                    };
                } else {
                    reply.sessions = Some(live.len());
                }
            }
            reply
        }
        Err(error) => Reply::error(format!("invalid request: {error}")),
    };
    let mut body = serde_json::to_string(&reply).unwrap_or_else(|_| "{\"ok\":false}".into());
    body.push('\n');
    let mut writer = &stream;
    let _ = writer.write_all(body.as_bytes());
    let _ = writer.flush();
    if state.shutting_down.load(Ordering::SeqCst) && reply.ok {
        // Wake the accept loop so it observes the flag and exits.
        let _ = std::os::unix::net::UnixStream::connect(state.home.join(SOCKET_FILE));
    }
}

/// Old-core side of a handoff. The reactor writes every framed reply on
/// `stream`; this thread only gates the request and records the outcome.
#[cfg(unix)]
fn handle_handoff(
    stream: std::os::unix::net::UnixStream,
    state: &Arc<CoreState>,
    build_id: Option<String>,
) {
    use crate::session_host::fd_pass::send_message;
    let mut stream = stream;
    let refuse = |stream: &mut std::os::unix::net::UnixStream, error: &str| {
        let body = serde_json::json!({ "ok": false, "error": error }).to_string();
        let _ = send_message(stream, body.as_bytes(), &[]);
    };
    if state
        .handing_off
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        refuse(&mut stream, "handing_off");
        return;
    }
    if state.shutting_down.load(Ordering::SeqCst) {
        state.handing_off.store(false, Ordering::SeqCst);
        refuse(&mut stream, "shutting_down");
        return;
    }
    // A launch whose setup thread is still running cannot move: wait a
    // moment for it to register, then refuse rather than hand over half.
    let deadline = Instant::now() + Duration::from_secs(15);
    while state.launching.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if state.launching.load(Ordering::SeqCst) > 0 {
        state.handing_off.store(false, Ordering::SeqCst);
        refuse(&mut stream, "busy");
        return;
    }
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(None);
    let services = match crate::session_host::core_reactor::services() {
        Ok(services) => services,
        Err(error) => {
            state.handing_off.store(false, Ordering::SeqCst);
            refuse(&mut stream, &error);
            return;
        }
    };
    append_trace_log_line(&format!(
        "pty-core handoff requested by build {:?} (ours {:?})",
        build_id,
        current_host_build_id()
    ));
    match services
        .reactor
        .handoff(stream, vec![state.lock_fd, state.listener_fd])
    {
        Ok(moved) => {
            if let Ok(mut live) = state.live.lock() {
                for id in &moved {
                    live.remove(id);
                }
            }
            state.handed_off.store(true, Ordering::SeqCst);
            append_trace_log_line(&format!(
                "pty-core handed {} sessions to the new core",
                moved.len()
            ));
            // Wake the accept loop so it observes the flag and exits.
            let _ = std::os::unix::net::UnixStream::connect(state.home.join(SOCKET_FILE));
        }
        Err(error) => {
            append_trace_log_line(&format!("pty-core handoff failed, resumed: {error}"));
            state.handing_off.store(false, Ordering::SeqCst);
        }
    }
}

fn launch_session(state: &Arc<CoreState>, launch_file: &str) -> Result<String, String> {
    if state.shutting_down.load(Ordering::SeqCst) {
        return Err("core is shutting down".into());
    }
    if state.handing_off.load(Ordering::SeqCst) {
        return Err("core is handing off".into());
    }
    let launch_path = PathBuf::from(launch_file);
    if !launch_path.is_absolute() {
        return Err("launch_file must be an absolute path".into());
    }
    let raw = fs::read(&launch_path).map_err(|e| format!("Failed to read launch file: {e}"))?;
    let launch: SessionHostLaunch =
        serde_json::from_slice(&raw).map_err(|e| format!("Invalid launch file: {e}"))?;
    let session_id = launch.session.id.clone();
    if session_id.trim().is_empty() {
        return Err("launch file has no session id".into());
    }

    {
        let mut live = state
            .live
            .lock()
            .map_err(|_| "core live set poisoned".to_string())?;
        if state.shutting_down.load(Ordering::SeqCst) {
            return Err("core is shutting down".into());
        }
        if !live.insert(session_id.clone()) {
            return Err(format!(
                "session {session_id} is already hosted by this core"
            ));
        }
    }
    // Consumed exactly like `run_from_args`: from here on this core owns the
    // launch, and a fallback spawn must not double-host it.
    let _ = fs::remove_file(&launch_path);

    let manifest = state.sessions_root.join(&session_id).join("manifest.json");
    let _ = fs::remove_file(&manifest);

    let thread_state = Arc::clone(state);
    let thread_session_id = session_id.clone();
    state.launching.fetch_add(1, Ordering::SeqCst);
    // Setup (hook install, provider prep, PTY spawn) runs on a short-lived
    // thread; once the reactor owns the Session that thread is gone, so a
    // hosted Session keeps no thread of its own.
    let handle = thread::Builder::new()
        .name(format!(
            "session-{}",
            &session_id[..session_id.len().min(8)]
        ))
        .spawn(move || {
            struct LaunchGuard(Arc<CoreState>);
            impl Drop for LaunchGuard {
                fn drop(&mut self) {
                    self.0.launching.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _launch_guard = LaunchGuard(Arc::clone(&thread_state));
            let runner = Arc::clone(&thread_state.runner);
            let exit_state = Arc::clone(&thread_state);
            let exit_session_id = thread_session_id.clone();
            let on_exit: OnExit = Box::new(move |result: Result<(), String>| {
                if let Err(error) = result {
                    append_trace_log_line(&format!(
                        "pty-core session {exit_session_id} failed: {error}"
                    ));
                    session_host::mark_manifest_exited(&exit_session_id);
                }
                if let Ok(mut live) = exit_state.live.lock() {
                    live.remove(&exit_session_id);
                }
            });
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runner(launch, on_exit)));
            let failure = match outcome {
                Ok(Ok(())) => return,
                Ok(Err(error)) => format!("failed: {error}"),
                Err(panic) => {
                    let message = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic".into());
                    format!("panicked: {message}")
                }
            };
            append_trace_log_line(&format!("pty-core session {thread_session_id} {failure}"));
            session_host::mark_manifest_exited(&thread_session_id);
            if let Ok(mut live) = thread_state.live.lock() {
                live.remove(&thread_session_id);
            }
            // Memory release after teardowns runs from the reactor's idle
            // tick (`core_reactor`), which also covers Sessions that exit
            // normally; nothing to do on this short-lived setup thread.
        })
        .map_err(|e| {
            state.launching.fetch_sub(1, Ordering::SeqCst);
            if let Ok(mut live) = state.live.lock() {
                live.remove(&session_id);
            }
            format!("Failed to spawn session thread: {e}")
        })?;

    // Reply only once the preliminary manifest exists — the same moment a
    // per-process Host has it on disk — or the thread already gave up.
    let deadline = Instant::now() + MANIFEST_WAIT_TIMEOUT;
    loop {
        if manifest.exists() {
            return Ok(session_id);
        }
        if handle.is_finished() {
            return Err(format!(
                "session {session_id} ended before publishing its manifest (see hooks/trace.log)"
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "session {session_id} did not publish its manifest within {}s",
                MANIFEST_WAIT_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::session_host::{HostedSessionManifest, HostedSessionState};
    use std::sync::mpsc;

    fn temp_home(tag: &str) -> PathBuf {
        // Unix socket paths are capped near 104 bytes; keep the home short.
        let dir = PathBuf::from(format!("/tmp/upc-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn launch_for(home: &Path, id: &str) -> PathBuf {
        let launch = serde_json::json!({
            "session": {"id": id, "project_id": "p", "label": id, "command": ""},
            "cwd": "/tmp",
            "dark_mode": null,
            "hook_port": null,
        });
        let path = home.join(format!("launch-{id}.json"));
        fs::write(&path, serde_json::to_vec(&launch).unwrap()).unwrap();
        path
    }

    /// A fake session loop: writes the "preliminary manifest" (a marker
    /// file at the path the test passes through `manifest_dir`), then waits
    /// on its release channel, then returns/panics as instructed.
    fn start_core(
        home: &Path,
        manifest_dir: PathBuf,
    ) -> (thread::JoinHandle<Result<(), String>>, mpsc::Sender<String>) {
        let (release_tx, release_rx) = mpsc::channel::<String>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let root = manifest_dir.clone();
        let runner: SessionRunner = Arc::new(move |launch: SessionHostLaunch, on_exit: OnExit| {
            let dir = manifest_dir.join(&launch.session.id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("manifest.json"), b"{}").unwrap();
            // Like the real runner: return once "registered", and let the
            // Session's own life (here a helper thread) report its end.
            let release_rx = Arc::clone(&release_rx);
            thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                    let message = release_rx.lock().unwrap().recv().unwrap();
                    if message == format!("panic:{}", launch.session.id) {
                        panic!("boom in {}", launch.session.id);
                    }
                    if message == format!("fail:{}", launch.session.id) {
                        return Err("simulated failure".to_string());
                    }
                    if message == format!("exit:{}", launch.session.id) {
                        return Ok(());
                    }
                }));
                on_exit(outcome.unwrap_or_else(|_| Err("session panicked".into())));
            });
            Ok(())
        });
        let core_home = home.to_path_buf();
        let handle = thread::spawn(move || run_core_at(core_home, root, runner));
        // Ready = bound socket AND published record: run_core_unix writes
        // pty-core.json after bind, so a caller that raced ahead on the
        // socket alone could observe a live core with no record yet.
        let socket = home.join(SOCKET_FILE);
        let record = home.join(RECORD_FILE);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !(socket.exists() && record.exists()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(socket.exists(), "core never bound its socket");
        assert!(record.exists(), "core never published its record");
        (handle, release_tx)
    }

    fn wait_live(socket: &Path, expected: usize) -> CoreStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = ping_at(socket, Duration::from_secs(2)).unwrap();
            if status.sessions == expected || Instant::now() >= deadline {
                return status;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn ping_launch_shutdown_and_panic_isolation() {
        let home = temp_home("core");
        let socket = home.join(SOCKET_FILE);
        let root = home.join("app-sessions");
        let (core, release) = start_core(&home, root.clone());

        let record = load_record_at(&home);
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.protocol, PTY_CORE_PROTOCOL);
        assert_eq!(record.socket, socket.to_string_lossy());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let idle = ping_at(&socket, Duration::from_secs(2)).unwrap();
        assert_eq!(idle.sessions, 0);
        assert_eq!(idle.pid, std::process::id());

        let a = format!("pty-core-test-a-{}", std::process::id());
        let b = format!("pty-core-test-b-{}", std::process::id());
        let launch_a = launch_for(&home, &a);
        let launch_b = launch_for(&home, &b);

        let stream = connect_with_timeout(&socket, CONNECT_TIMEOUT).unwrap();
        drop(stream);
        let reply = request_at(
            &socket,
            &Request::Launch {
                launch_file: launch_a.to_string_lossy().to_string(),
            },
            LAUNCH_REPLY_TIMEOUT,
        )
        .unwrap();
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.session_id.as_deref(), Some(a.as_str()));
        assert!(!launch_a.exists(), "the core consumes the launch file");
        assert!(root.join(&a).join("manifest.json").exists());

        let reply = request_at(
            &socket,
            &Request::Launch {
                launch_file: launch_b.to_string_lossy().to_string(),
            },
            LAUNCH_REPLY_TIMEOUT,
        )
        .unwrap();
        assert!(reply.ok, "{reply:?}");
        assert_eq!(wait_live(&socket, 2).sessions, 2);

        // Shutdown is refused while Sessions are hosted.
        let busy = shutdown_at(&socket, Duration::from_secs(2)).unwrap_err();
        assert!(busy.contains("busy"), "{busy}");
        assert!(busy.contains("2 sessions"), "{busy}");
        assert!(socket.exists());

        // A panicking session ends only itself: the sibling keeps running
        // and the core keeps answering.
        release.send(format!("panic:{a}")).unwrap();
        assert_eq!(wait_live(&socket, 1).sessions, 1);
        let exited = fs::read(root.join(&a).join("manifest.json")).unwrap();
        // The fake manifest is `{}`, which cannot decode; mark_manifest_exited
        // leaves undecodable records alone, so the file is untouched. The
        // isolation claim is the live count above plus the sibling below.
        assert_eq!(exited, b"{}");
        let still = ping_at(&socket, Duration::from_secs(2)).unwrap();
        assert_eq!(still.sessions, 1);

        // A relaunch of the same id is refused while it is hosted.
        let dup = launch_for(&home, &b);
        let reply = request_at(
            &socket,
            &Request::Launch {
                launch_file: dup.to_string_lossy().to_string(),
            },
            LAUNCH_REPLY_TIMEOUT,
        )
        .unwrap();
        assert!(!reply.ok);
        assert!(reply.error.unwrap().contains("already hosted"));

        // An unreadable launch file is an error reply, not a dead core.
        let reply = request_at(
            &socket,
            &Request::Launch {
                launch_file: home.join("missing.json").to_string_lossy().to_string(),
            },
            LAUNCH_REPLY_TIMEOUT,
        )
        .unwrap();
        assert!(!reply.ok);

        release.send(format!("exit:{b}")).unwrap();
        assert_eq!(wait_live(&socket, 0).sessions, 0);
        shutdown_at(&socket, Duration::from_secs(2)).unwrap();
        core.join().unwrap().unwrap();
        assert!(!socket.exists(), "socket removed on clean exit");
        assert!(
            !home.join(RECORD_FILE).exists(),
            "record removed on clean exit"
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn second_core_for_the_same_home_exits_immediately() {
        let home = temp_home("dup");
        let socket = home.join(SOCKET_FILE);
        let (core, _release) = start_core(&home, home.join("m"));
        let runner: SessionRunner = Arc::new(|_, _| Ok(()));
        let second = run_core_at(home.clone(), home.join("m"), runner);
        assert_eq!(second, Ok(()));
        assert!(socket.exists(), "the first core keeps its socket");
        assert!(
            home.join(RECORD_FILE).exists(),
            "the first core keeps its record"
        );
        shutdown_at(&socket, Duration::from_secs(2)).unwrap();
        core.join().unwrap().unwrap();
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn launching_core_manifest_is_alive_not_killable() {
        let self_pid = std::process::id();
        let started = process_start_time_ms(self_pid).expect("own start time");
        let mut manifest: HostedSessionManifest = serde_json::from_value(serde_json::json!({
            "session": {"id": "launching", "project_id": "p", "label": "l", "command": ""},
            "cwd": "/tmp",
            "state": "running",
            "pid": null,
            "exit_code": null,
            "host_pid": self_pid,
            "host_pid_started_at": started,
            "heartbeat_at": 1,
            "updated_at": 1,
        }))
        .unwrap();
        assert_eq!(manifest.state, HostedSessionState::Running);
        assert!(session_host::manifest_launching_host_is_alive(&manifest));
        assert!(session_host::manifest_is_live(&manifest));
        // Kill paths key on `pid`, which stays None: nothing to signal.
        assert_eq!(manifest.pid, None);

        // A recycled or unrecorded host pid proves nothing.
        manifest.host_pid_started_at = Some(started.saturating_sub(3_600_000));
        assert!(!session_host::manifest_launching_host_is_alive(&manifest));
        manifest.host_pid_started_at = None;
        assert!(!session_host::manifest_launching_host_is_alive(&manifest));

        // Legacy Running record without either identity: still not
        // "definitively gone" and still not alive — unknowable, as before.
        manifest.host_pid = None;
        assert!(!session_host::manifest_is_live(&manifest));
    }

    fn load_record_at(home: &Path) -> PtyCoreRecord {
        serde_json::from_slice(&fs::read(home.join(RECORD_FILE)).unwrap()).unwrap()
    }
}
