//! unpeel-attach — tmux-style attach client for Unpeel hosted sessions.
//!
//! Runs inside a real PTY owned by the rendering terminal (Ghostty surface)
//! and adapts it to a detached Unpeel session host:
//!
//! - switches its own PTY into raw mode for the duration of the attach
//!   (restored on exit) so keystrokes reach the workload byte-by-byte
//!   instead of being line-buffered and caret-echoed by the local tty,
//! - replays a boundary-aligned tail of `output.bin` to stdout,
//! - follows `output.bin` for live output via kqueue vnode events (Linux:
//!   epoll + inotify; no
//!   fixed-interval polling: a retained-but-hidden attach costs ~0 idle CPU,
//!   and new output is forwarded the moment the host appends it),
//! - relays stdin to the host control socket as `write` commands, dropping
//!   terminal focus reports (`ESC[I`/`ESC[O`) UNLESS the workload enabled
//!   focus reporting (DEC 1004 in the manifest's terminal_modes) — apps
//!   like terminal-browser throttle to a background frame rate until a
//!   focus-in arrives (always forward with `--forward-focus-events`),
//! - forwards terminal size changes as `resize` commands,
//! - exits when the host dies or stdin closes.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use unpeel_attach::{
    connect_input_stream, connect_output_stream, host_is_alive, load_manifest,
    mode_restore_preamble, output_retained_from, read_output_stream_frame, read_replay_tail,
    request_snapshot, send_command, snapshot_attach_enabled, split_valid_utf8, terminal_size,
    write_input_stream_frame, AttachCommand, FocusEventFilter, ManifestState, MuteFilter,
    OutputStreamRead, RawModeGuard, DEFAULT_MUTE_INPUT_MS, DEFAULT_REPLAY_BYTES,
};

/// Retry cadence while output.bin does not exist yet (host hasn't produced
/// output); once the file is open, kqueue wakes us instead of a timer.
const OUTPUT_FILE_RETRY_MS: u64 = 50;
// A host under load can take well over 2s to write its first manifest (a
// worker-spawned host adds a hop; a busy Mac adds more). Waiting is cheap and
// the pane already shows a starting state, so be patient before declaring
// the session missing.
const MANIFEST_STARTUP_TIMEOUT_MS: u64 = 20_000;
const MANIFEST_STARTUP_RETRY_MS: u64 = 25;
const SOCKET_STARTUP_RESIZE_TIMEOUT_MS: u64 = 10_000;
const SOCKET_STARTUP_RESIZE_RETRY_MS: u64 = 25;
const MANIFEST_CHECK_INTERVAL_MS: u64 = 1_000;
/// Poll cadence and stability window for settling the surface's grid before we
/// forward it as the startup resize (see `settled_terminal_size`).
const STARTUP_SIZE_SETTLE_POLL_MS: u64 = 15;
const STARTUP_SIZE_SETTLE_STABLE_MS: u64 = 60;
const STARTUP_SIZE_SETTLE_TIMEOUT_MS: u64 = 300;
/// AppKit can report two or three intermediate grids while the phone
/// letterbox is laid out. Forward only the final stable live size so one
/// keyboard show/hide produces one Host SIGWINCH and one TUI repaint.
const LIVE_SIZE_SETTLE_MS: u64 = 60;
const OUTPUT_READ_BUFFER_BYTES: usize = 64 * 1024;
const SESSION_ENDED_BANNER: &[u8] = b"\r\n[session ended]\r\n";
// CAN first aborts an unterminated OSC/DCS/SOS control string. Without it,
// RIS and the CSI clears that follow can themselves be swallowed by the
// parser state whose introducing bytes were just evicted.
const TERMINAL_REPLAY_RESET: &[u8] = b"\x18\x1bc\x1b[3J\x1b[2J\x1b[H";
const OUTPUT_STREAM_RUNNING: u8 = 0;
const OUTPUT_STREAM_EXITED: u8 = 1;
const OUTPUT_STREAM_FAILED: u8 = 2;

struct Args {
    session_id: String,
    sessions_dir: PathBuf,
    replay_bytes: u64,
    mute_input_ms: u64,
    forward_focus_events: bool,
}

fn default_sessions_dir() -> PathBuf {
    // Honor UNPEEL_HOME like the app and unpeel-host (app_paths::unpeel_home),
    // so a dev/blank instance's surfaces attach to its isolated state dir.
    if let Some(home) = std::env::var_os("UNPEEL_HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join("app-sessions");
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".unpeel").join("app-sessions")
}

fn parse_args() -> Result<Args, String> {
    let mut session_id: Option<String> = None;
    let mut sessions_dir = default_sessions_dir();
    let mut replay_bytes = DEFAULT_REPLAY_BYTES;
    let mut mute_input_ms = DEFAULT_MUTE_INPUT_MS;
    let mut forward_focus_events = false;

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--sessions-dir" => {
                let value = iter.next().ok_or("--sessions-dir requires a value")?;
                sessions_dir = PathBuf::from(value);
            }
            "--replay-bytes" => {
                let value = iter.next().ok_or("--replay-bytes requires a value")?;
                replay_bytes = value
                    .parse()
                    .map_err(|_| format!("Invalid --replay-bytes value: {value}"))?;
            }
            "--mute-input-ms" => {
                let value = iter.next().ok_or("--mute-input-ms requires a value")?;
                mute_input_ms = value
                    .parse()
                    .map_err(|_| format!("Invalid --mute-input-ms value: {value}"))?;
            }
            "--forward-focus-events" => {
                forward_focus_events = true;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: unpeel-attach [--sessions-dir <dir>] [--replay-bytes <n>] \
                     [--mute-input-ms <n>] [--forward-focus-events] <session-id>"
                );
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown flag: {other}"));
            }
            other => {
                if session_id.is_some() {
                    return Err("Multiple session ids given".into());
                }
                session_id = Some(other.to_string());
            }
        }
    }

    Ok(Args {
        session_id: session_id.ok_or("Usage: unpeel-attach <session-id>")?,
        sessions_dir,
        replay_bytes,
        mute_input_ms,
        forward_focus_events,
    })
}

fn main() {
    let code = match parse_args().and_then(run) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("unpeel-attach: {error}");
            1
        }
    };
    std::process::exit(code);
}

/// Mirror of `unpeel_core::session_host::socket_path`: macOS caps
/// `sockaddr_un.sun_path` at 104 bytes, so a deep home (another local
/// workspace: `~/.unpeel/profiles/<name>/app-sessions/<uuid>/session.sock`)
/// makes the host bind its control socket at a deterministic short path
/// instead. Attach must compute the same one, or a workspace pane shows the
/// replayed tail and then never connects.
const MAX_UNIX_SOCKET_PATH_LEN: usize = 100;

fn control_socket_path(session_dir: &Path, session_id: &str) -> PathBuf {
    let preferred = session_dir.join("session.sock");
    if preferred.as_os_str().len() <= MAX_UNIX_SOCKET_PATH_LEN {
        return preferred;
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from("/tmp")
        .join(format!("unpeel-{uid}"))
        .join(format!("{session_id}.sock"))
}

fn run(args: Args) -> Result<i32, String> {
    let session_dir = args.sessions_dir.join(&args.session_id);
    let output_path = session_dir.join("output.bin");
    let socket_path = control_socket_path(&session_dir, &args.session_id);

    // Put our controlling terminal (the surface PTY we were spawned on) into
    // raw mode for the duration of the attach. A fresh PTY starts in
    // canonical mode with ECHO: without this, escape-key input (arrows, ...)
    // is caret-echoed locally as literal `^[[A` and held back until Enter
    // instead of reaching the workload per keystroke. The guard restores the
    // original settings on every exit path of this function. `None` (not a
    // tty, e.g. piped stdin in tests) means there is nothing to switch.
    let _raw_mode = RawModeGuard::enable(libc::STDIN_FILENO);

    let manifest = wait_for_startup_manifest(
        &session_dir,
        Duration::from_millis(MANIFEST_STARTUP_TIMEOUT_MS),
    )
    .ok_or_else(|| format!("No session manifest at {}", session_dir.display()))?;

    // Match the host PTY to our surface before replaying so full-screen TUIs
    // repaint at the right size instead of the previous client's size. The
    // session host writes a preliminary manifest before provider setup and
    // before binding session.sock, so this must retry instead of dropping the
    // resize when attach wins the startup race.
    let startup_resize = (manifest.state == ManifestState::Running)
        .then(|| {
            send_startup_resize_when_ready(
                &session_dir,
                &socket_path,
                Duration::from_millis(SOCKET_STARTUP_RESIZE_TIMEOUT_MS),
            )
        })
        .flatten();

    // Snapshot attach: the Host renders its resident VT (cells, styles,
    // scrollback, modes, cursor) at an exact journal offset; we apply it to a
    // reset terminal and stream from that offset. Exact for incremental
    // repaint TUIs, and independent of how much journal tail is retained.
    // Any Host that cannot answer (older build, exited, transport error)
    // gets today's raw tail replay unchanged.
    let attach_started = Instant::now();
    let snapshot = (manifest.state == ManifestState::Running && snapshot_attach_enabled())
        .then(|| request_snapshot(&socket_path).ok().flatten())
        .flatten();
    let (replay, mut offset, snapshot_applied) = match snapshot {
        Some(snapshot) => (snapshot.bytes, snapshot.journal_offset, true),
        None => {
            let (replay, offset) = read_replay_tail(&output_path, args.replay_bytes)?;
            (replay, offset, false)
        }
    };
    // Wipe screen + scrollback before replay: the terminal that spawned us
    // may have printed its own preamble (macOS login(1) prints "Last login"
    // and "You have mail"), which is not part of the session. The reset also
    // wipes input-shaping modes the workload negotiated before the replayed
    // tail begins (alt screen, mouse tracking, bracketed paste), so re-assert
    // the host's current view of them before replaying. A snapshot carries
    // every non-default mode itself, so it needs no manifest preamble.
    {
        let preamble = if snapshot_applied {
            Vec::new()
        } else {
            mode_restore_preamble(&session_dir)
        };
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(TERMINAL_REPLAY_RESET)
            .and_then(|_| stdout.write_all(&preamble))
            .and_then(|_| stdout.write_all(&replay))
            .and_then(|_| stdout.flush())
            .map_err(|e| format!("Failed to write replay to stdout: {e}"))?;
    }
    record_attach_timing(attach_started, snapshot_applied, replay.len());

    if manifest.state == ManifestState::Exited {
        return Ok(0);
    }

    let shutdown = Arc::new(AtomicBool::new(false));

    // Self-pipes wake the kqueue wait: SIGWINCH writes to one (registered
    // with signal_hook so the write happens inside the signal handler), the
    // stdin pump writes to another on EOF, and the output stream pump writes
    // to one when it exits. Read ends live in kqueue so the live loop sleeps
    // indefinitely yet reacts to everything within a syscall.
    let winch_pipe = SelfPipe::new()?;
    signal_hook::low_level::pipe::register_raw(signal_hook::consts::SIGWINCH, winch_pipe.write)
        .map_err(|e| format!("Failed to install SIGWINCH handler: {e}"))?;
    let stdin_eof_pipe = SelfPipe::new()?;
    let output_stream_pipe = SelfPipe::new()?;

    // Sync our PTY size to the host once up front. We only forward resizes on
    // SIGWINCH, but if the rendering surface is already at its final size when
    // we attach (e.g. the surface mounts after layout has settled), no
    // SIGWINCH ever fires — the host would keep its launch-time initial_cols/
    // initial_rows and the workload renders at the wrong grid until the user
    // nudges the window. If startup retry above could not send it, make one
    // final best-effort attempt before entering the live loop.
    let mut last_forwarded_size = startup_resize;
    if last_forwarded_size.is_none() {
        if let Some((cols, rows)) = terminal_size() {
            if send_command(&socket_path, &AttachCommand::Resize { cols, rows }).is_ok() {
                last_forwarded_size = Some((cols, rows));
            }
        }
    }

    let replay_mute_until = Instant::now() + Duration::from_millis(args.mute_input_ms);
    // Focus reports (ESC[I/ESC[O) are stripped only while the workload has
    // NOT enabled focus reporting (DEC 1004). The host publishes the live
    // mode set in the manifest; refreshed each manifest-check tick below.
    let focus_reporting_active = Arc::new(AtomicBool::new(workload_focus_reporting(&session_dir)));
    spawn_stdin_pump(
        socket_path.clone(),
        replay_mute_until,
        !args.forward_focus_events,
        Arc::clone(&focus_reporting_active),
        Arc::clone(&shutdown),
        stdin_eof_pipe.write,
    );
    schedule_attach_ready(session_dir.clone(), replay_mute_until);

    let output_stream_status = Arc::new(AtomicU8::new(OUTPUT_STREAM_RUNNING));
    let output_stream_offset = Arc::new(AtomicU64::new(offset));
    let mut use_output_stream = spawn_output_stream_pump(
        socket_path.clone(),
        offset,
        Arc::clone(&shutdown),
        Arc::clone(&output_stream_status),
        Arc::clone(&output_stream_offset),
        output_stream_pipe.write,
    );

    // Live loop: follow output.bin (kqueue-driven), forward resizes, watch
    // host liveness.
    let kq = Kqueue::new()?;
    kq.watch_read(winch_pipe.read)?;
    kq.watch_read(stdin_eof_pipe.read)?;
    if use_output_stream {
        kq.watch_read(output_stream_pipe.read)?;
    }

    let mut output_file: Option<File> = if use_output_stream {
        None
    } else {
        File::open(&output_path).ok()
    };
    if let Some(file) = output_file.as_ref() {
        kq.watch_vnode_writes(file.as_raw_fd())?;
    }
    let mut buf = vec![0u8; OUTPUT_READ_BUFFER_BYTES];
    let mut last_manifest_check = Instant::now();
    let mut pending_resize_deadline: Option<Instant> = None;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(0);
        }

        if drain_pipe(winch_pipe.read) {
            // Do not sample the first AppKit/Ghostty frame. Every additional
            // SIGWINCH moves this deadline, so the one command below carries
            // only the settled letterbox grid.
            pending_resize_deadline =
                Some(Instant::now() + Duration::from_millis(LIVE_SIZE_SETTLE_MS));
        }

        if pending_resize_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            pending_resize_deadline = None;
            if let Some((cols, rows)) = terminal_size() {
                let size = (cols, rows);
                if last_forwarded_size != Some(size)
                    && send_command(&socket_path, &AttachCommand::Resize { cols, rows }).is_ok()
                {
                    last_forwarded_size = Some(size);
                }
            }
        }

        if use_output_stream && drain_pipe(output_stream_pipe.read) {
            match output_stream_status.load(Ordering::Relaxed) {
                OUTPUT_STREAM_EXITED => return Ok(0),
                OUTPUT_STREAM_FAILED => {
                    use_output_stream = false;
                    offset = output_stream_offset.load(Ordering::Relaxed);
                    output_file = File::open(&output_path).ok();
                    if let Some(file) = output_file.as_ref() {
                        kq.watch_vnode_writes(file.as_raw_fd())?;
                    }
                }
                _ => {}
            }
        }

        if !use_output_stream && output_file.is_none() {
            output_file = File::open(&output_path).ok();
            if let Some(file) = output_file.as_ref() {
                kq.watch_vnode_writes(file.as_raw_fd())?;
            }
        }

        if !use_output_stream {
            // Drain everything that's there; vnode events are edge-triggered
            // (EV_CLEAR), so anything appended after this drain re-queues an
            // event and the next kevent wait returns immediately.
            if let Some(file) = output_file.as_mut() {
                let mut stdout = std::io::stdout().lock();
                while pump_new_output(
                    file,
                    &output_path,
                    args.replay_bytes,
                    &mut offset,
                    &mut buf,
                    &mut stdout,
                )? > 0
                {}
            }
        }

        if last_manifest_check.elapsed() >= Duration::from_millis(MANIFEST_CHECK_INTERVAL_MS) {
            last_manifest_check = Instant::now();
            focus_reporting_active.store(workload_focus_reporting(&session_dir), Ordering::Relaxed);
            if !host_is_alive(&session_dir) {
                // Final drain: the host may have flushed last bytes between
                // our read and the manifest flipping to exited.
                let mut stdout = std::io::stdout().lock();
                if !use_output_stream {
                    if let Some(file) = output_file.as_mut() {
                        while pump_new_output(
                            file,
                            &output_path,
                            args.replay_bytes,
                            &mut offset,
                            &mut buf,
                            &mut stdout,
                        )? > 0
                        {}
                    }
                }
                let _ = stdout.write_all(SESSION_ENDED_BANNER);
                let _ = stdout.flush();
                return Ok(0);
            }
        }

        // Block until output is appended, a signal/EOF pipe fires, or the
        // manifest-check interval elapses. While output.bin doesn't exist
        // yet there is no vnode to watch, so retry on a short timer.
        let mut timeout_ms = if use_output_stream || output_file.is_some() {
            MANIFEST_CHECK_INTERVAL_MS
        } else {
            OUTPUT_FILE_RETRY_MS
        };
        if let Some(deadline) = pending_resize_deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let resize_wait_ms = u64::try_from(remaining.as_millis())
                .unwrap_or(u64::MAX)
                .max(1);
            timeout_ms = timeout_ms.min(resize_wait_ms);
        }
        kq.wait(timeout_ms)?;
    }
}

fn spawn_output_stream_pump(
    socket_path: PathBuf,
    offset: u64,
    shutdown: Arc<AtomicBool>,
    status: Arc<AtomicU8>,
    current_offset: Arc<AtomicU64>,
    wake_fd: RawFd,
) -> bool {
    let Ok(mut stream) = connect_output_stream(&socket_path, offset) else {
        return false;
    };

    thread::spawn(move || {
        let mut expected_offset = offset;
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }

            match read_output_stream_frame(&mut stream) {
                Ok(OutputStreamRead::Chunk(chunk)) => {
                    let chunk_start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
                    if chunk_start != expected_offset {
                        // The stream frame has no reset flag. Never render across
                        // an inferred gap; disk fallback can rebase explicitly.
                        status.store(OUTPUT_STREAM_FAILED, Ordering::Relaxed);
                        wake_output_stream_waiter(wake_fd);
                        return;
                    }
                    if !chunk.data.is_empty() {
                        let mut stdout = std::io::stdout().lock();
                        if stdout
                            .write_all(&chunk.data)
                            .and_then(|_| stdout.flush())
                            .is_err()
                        {
                            status.store(OUTPUT_STREAM_FAILED, Ordering::Relaxed);
                            wake_output_stream_waiter(wake_fd);
                            return;
                        }
                    }
                    expected_offset = chunk.next_offset;
                    current_offset.store(expected_offset, Ordering::Relaxed);

                    if chunk.exited {
                        let mut stdout = std::io::stdout().lock();
                        let _ = stdout.write_all(SESSION_ENDED_BANNER);
                        let _ = stdout.flush();
                        status.store(OUTPUT_STREAM_EXITED, Ordering::Relaxed);
                        wake_output_stream_waiter(wake_fd);
                        return;
                    }
                }
                Ok(OutputStreamRead::TimedOut) => continue,
                Ok(OutputStreamRead::Closed) | Err(_) => {
                    status.store(OUTPUT_STREAM_FAILED, Ordering::Relaxed);
                    wake_output_stream_waiter(wake_fd);
                    return;
                }
            }
        }
    });

    true
}

fn wake_output_stream_waiter(fd: RawFd) {
    let wake = [1u8];
    unsafe { libc::write(fd, wake.as_ptr().cast(), 1) };
}

fn wait_for_startup_manifest(
    session_dir: &Path,
    timeout: Duration,
) -> Option<unpeel_attach::Manifest> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(manifest) = load_manifest(session_dir) {
            return Some(manifest);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(MANIFEST_STARTUP_RETRY_MS));
    }
}

/// Wait for the surface's reported grid to hold steady before trusting it.
///
/// libghostty spawns this attach process's PTY at its *default* grid (the
/// `ghostty_surface_config_s` has no size field) and only resizes it to the real
/// surface size a beat later, on its post-`createSurface` `setSize`. Reading
/// `terminal_size()` inside that window yields the stale default; forwarding that
/// as the startup resize shrinks the host PTY below its correct launch grid right
/// before the provider CLI's first paint — and already-emitted banners never
/// reflow, so the session renders narrow until the user nudges the window.
///
/// Poll until the size is unchanged for `STARTUP_SIZE_SETTLE_STABLE_MS`, bounded
/// by `deadline`: a genuinely static surface returns after one stable window, and
/// a surface still mid-layout resolves to its latest value by the deadline.
fn settled_terminal_size(deadline: Instant) -> Option<(u16, u16)> {
    let mut size = terminal_size()?;
    let mut stable_since = Instant::now();
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(STARTUP_SIZE_SETTLE_POLL_MS));
        match terminal_size() {
            Some(current) if current == size => {
                if stable_since.elapsed() >= Duration::from_millis(STARTUP_SIZE_SETTLE_STABLE_MS) {
                    return Some(size);
                }
            }
            Some(current) => {
                size = current;
                stable_since = Instant::now();
            }
            // Lost the tty mid-settle: forward the last size we saw.
            None => return Some(size),
        }
    }
    Some(size)
}

fn send_startup_resize_when_ready(
    session_dir: &Path,
    socket_path: &Path,
    timeout: Duration,
) -> Option<(u16, u16)> {
    let deadline = Instant::now() + timeout;
    let settle_deadline =
        (Instant::now() + Duration::from_millis(STARTUP_SIZE_SETTLE_TIMEOUT_MS)).min(deadline);
    let (cols, rows) = settled_terminal_size(settle_deadline)?;
    let command = AttachCommand::Resize { cols, rows };

    loop {
        if send_command(socket_path, &command).is_ok() {
            return Some((cols, rows));
        }
        if Instant::now() >= deadline || !host_is_alive(session_dir) {
            return None;
        }
        thread::sleep(Duration::from_millis(SOCKET_STARTUP_RESIZE_RETRY_MS));
    }
}

/// Attach-to-correct-screen latency probe: with `UNPEEL_ATTACH_TIMING_FILE`
/// set, append one line per attach measuring request → last replay byte
/// flushed. Development/measurement only; never on by default.
fn record_attach_timing(started: Instant, snapshot: bool, replay_len: usize) {
    let Some(path) = std::env::var_os("UNPEEL_ATTACH_TIMING_FILE") else {
        return;
    };
    let line = format!(
        "attach_us={} path={} bytes={}\n",
        started.elapsed().as_micros(),
        if snapshot { "snapshot" } else { "tail" },
        replay_len
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn mark_attach_ready(session_dir: &Path) {
    let _ = std::fs::write(session_dir.join(".attach-ready"), b"1\n");
}

/// Publish provider-launch readiness only after replay-generated terminal
/// responses are no longer being filtered. Codex probes the terminal palette
/// immediately at startup; releasing it during the mute window discards the
/// real OSC color response and leaves its composer without background styling.
fn schedule_attach_ready(session_dir: PathBuf, replay_mute_until: Instant) {
    let remaining = replay_mute_until.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        mark_attach_ready(&session_dir);
        return;
    }

    thread::spawn(move || {
        thread::sleep(remaining);
        mark_attach_ready(&session_dir);
    });
}

// ---------------------------------------------------------------------------
// Event-wait plumbing: kqueue on macOS/BSD, epoll + inotify on Linux, behind
// one small `Kqueue` API (new / watch_read / watch_vnode_writes / wait).
// ---------------------------------------------------------------------------

/// Non-blocking, close-on-exec pipe pair used to wake the kqueue wait from a
/// signal handler or another thread. Raw fds; lives for the process lifetime.
struct SelfPipe {
    read: RawFd,
    write: RawFd,
}

impl SelfPipe {
    fn new() -> Result<Self, String> {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!(
                "Failed to create pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        for fd in fds {
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        Ok(Self {
            read: fds[0],
            write: fds[1],
        })
    }
}

/// Read a pipe dry; returns whether anything was pending. Pipe payloads are
/// pure wakeups — the bytes carry no meaning.
fn drain_pipe(fd: RawFd) -> bool {
    let mut buf = [0u8; 64];
    let mut any = false;
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            any = true;
            continue;
        }
        return any;
    }
}

/// Linux: the same three operations over epoll + inotify. epoll cannot
/// watch a regular file for appends, so `watch_vnode_writes` registers an
/// inotify IN_MODIFY watch on the file's path (resolved through
/// /proc/self/fd) and the inotify fd itself sits in the epoll set; the live
/// loop re-checks every source after any wake, exactly as on macOS.
#[cfg(target_os = "linux")]
struct Kqueue {
    epoll: RawFd,
    inotify: RawFd,
    watched: std::cell::RefCell<Vec<(RawFd, libc::c_int)>>,
}

#[cfg(target_os = "linux")]
impl Kqueue {
    fn new() -> Result<Self, String> {
        let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll < 0 {
            return Err(format!(
                "Failed to create epoll: {}",
                std::io::Error::last_os_error()
            ));
        }
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if inotify < 0 {
            return Err(format!(
                "Failed to create inotify: {}",
                std::io::Error::last_os_error()
            ));
        }
        let this = Self {
            epoll,
            inotify,
            watched: std::cell::RefCell::new(Vec::new()),
        };
        this.watch_read(inotify)?;
        Ok(this)
    }

    fn watch_read(&self, fd: RawFd) -> Result<(), String> {
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: fd as u64,
        };
        let rc = unsafe { libc::epoll_ctl(self.epoll, libc::EPOLL_CTL_ADD, fd, &mut event) };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                return Ok(());
            }
            return Err(format!("Failed to register epoll event: {error}"));
        }
        Ok(())
    }

    /// Watch the open file for writes. The file may be re-opened after a
    /// journal rotation, so a previous watch on the same fd number is
    /// replaced rather than duplicated.
    fn watch_vnode_writes(&self, fd: RawFd) -> Result<(), String> {
        let path = std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .map_err(|error| format!("Failed to resolve fd {fd} for inotify: {error}"))?;
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "output path contains a NUL byte".to_string())?;
        let wd = unsafe {
            libc::inotify_add_watch(
                self.inotify,
                c_path.as_ptr(),
                libc::IN_MODIFY | libc::IN_CLOSE_WRITE,
            )
        };
        if wd < 0 {
            return Err(format!(
                "Failed to add inotify watch on {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let mut watched = self.watched.borrow_mut();
        if let Some(previous) = watched.iter().position(|(f, _)| *f == fd) {
            let (_, old_wd) = watched.swap_remove(previous);
            if old_wd != wd {
                unsafe { libc::inotify_rm_watch(self.inotify, old_wd) };
            }
        }
        watched.push((fd, wd));
        Ok(())
    }

    /// Wait for any registered event or the timeout, then drain the inotify
    /// queue so a burst of appends costs one wake, not one per write.
    fn wait(&self, timeout_ms: u64) -> Result<(), String> {
        let mut events: [libc::epoll_event; 4] = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::epoll_wait(
                self.epoll,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                timeout_ms.min(i32::MAX as u64) as libc::c_int,
            )
        };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(());
            }
            return Err(format!("epoll wait failed: {error}"));
        }
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.inotify, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
struct Kqueue {
    fd: RawFd,
}

#[cfg(not(target_os = "linux"))]
impl Kqueue {
    fn new() -> Result<Self, String> {
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(format!(
                "Failed to create kqueue: {}",
                std::io::Error::last_os_error()
            ));
        }
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        Ok(Self { fd })
    }

    fn register(&self, change: libc::kevent) -> Result<(), String> {
        let rc = unsafe {
            libc::kevent(
                self.fd,
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            return Err(format!(
                "Failed to register kqueue event: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn watch_read(&self, fd: RawFd) -> Result<(), String> {
        self.register(libc::kevent {
            ident: fd as usize,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        })
    }

    /// Edge-triggered watch for appends to an open file (NOTE_WRITE fires on
    /// any write, NOTE_EXTEND on growth — output.bin only ever grows).
    fn watch_vnode_writes(&self, fd: RawFd) -> Result<(), String> {
        self.register(libc::kevent {
            ident: fd as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_EXTEND,
            data: 0,
            udata: std::ptr::null_mut(),
        })
    }

    /// Wait for any registered event or the timeout; which one fired doesn't
    /// matter — the caller re-checks all sources each iteration.
    fn wait(&self, timeout_ms: u64) -> Result<(), String> {
        let timeout = libc::timespec {
            tv_sec: (timeout_ms / 1_000) as libc::time_t,
            tv_nsec: ((timeout_ms % 1_000) * 1_000_000) as libc::c_long,
        };
        let mut events: [libc::kevent; 4] = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::kevent(
                self.fd,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                &timeout,
            )
        };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(());
            }
            return Err(format!("kevent wait failed: {error}"));
        }
        Ok(())
    }
}

/// True while the manifest reports the workload enabled focus reporting
/// (DEC private mode 1004) — the gate for forwarding focus reports instead
/// of stripping them.
fn workload_focus_reporting(session_dir: &Path) -> bool {
    load_manifest(session_dir)
        .and_then(|manifest| manifest.terminal_modes)
        .is_some_and(|modes| modes.set.contains(&1004))
}

/// Mode-restore preamble for a mid-stream retention rebase, where only the
/// output path is at hand (`session_dir` is its parent).
fn rebase_mode_preamble(output_path: &Path) -> Vec<u8> {
    output_path
        .parent()
        .map(mode_restore_preamble)
        .unwrap_or_default()
}

/// Read any bytes appended past `offset` and write them raw to stdout.
/// Returns how many bytes were forwarded.
fn pump_new_output(
    file: &mut File,
    output_path: &Path,
    replay_bytes: u64,
    offset: &mut u64,
    buf: &mut [u8],
    stdout: &mut impl Write,
) -> Result<usize, String> {
    let len = file
        .metadata()
        .map_err(|e| format!("Failed to stat output log: {e}"))?
        .len();

    if *offset < output_retained_from(output_path) {
        let (replay, next_offset) = read_replay_tail(output_path, replay_bytes)?;
        stdout
            .write_all(TERMINAL_REPLAY_RESET)
            .and_then(|_| stdout.write_all(&rebase_mode_preamble(output_path)))
            .and_then(|_| stdout.write_all(&replay))
            .and_then(|_| stdout.flush())
            .map_err(|e| format!("Failed to reset retained output replay: {e}"))?;
        *offset = next_offset;
        return Ok(replay.len());
    }

    if len < *offset {
        // In live-stream fallback, the stream offset can briefly be ahead of
        // the persisted log because the host broadcasts output before its
        // batched disk writer flushes. Keep the newer offset so already
        // streamed bytes are not replayed from disk.
        return Ok(0);
    }
    if len == *offset {
        return Ok(0);
    }

    file.seek(SeekFrom::Start(*offset))
        .map_err(|e| format!("Failed to seek output log: {e}"))?;
    let available = (len - *offset).min(buf.len() as u64) as usize;
    let read_start = *offset;
    let n = file
        .read(&mut buf[..available])
        .map_err(|e| format!("Failed to read output log: {e}"))?;
    if n == 0 {
        return Ok(0);
    }

    // Retention publishes its floor before punching. Recheck after copying
    // but before rendering so a concurrent punch can never leak sparse NULs
    // into the terminal.
    if read_start < output_retained_from(output_path) {
        let (replay, next_offset) = read_replay_tail(output_path, replay_bytes)?;
        stdout
            .write_all(TERMINAL_REPLAY_RESET)
            .and_then(|_| stdout.write_all(&rebase_mode_preamble(output_path)))
            .and_then(|_| stdout.write_all(&replay))
            .and_then(|_| stdout.flush())
            .map_err(|e| format!("Failed to reset retained output replay: {e}"))?;
        *offset = next_offset;
        return Ok(replay.len());
    }

    // Raw byte pump: no transcoding, no line buffering — write + flush as-is.
    stdout
        .write_all(&buf[..n])
        .and_then(|_| stdout.flush())
        .map_err(|e| format!("Failed to write output to stdout: {e}"))?;
    *offset += n as u64;
    Ok(n)
}

/// Reads raw bytes from stdin (run() switched our PTY into raw mode before
/// spawning this pump, so keystrokes arrive unbuffered and unechoed) and
/// relays them to the host as `write` commands. Sets `shutdown` on EOF and
/// pokes `eof_wake_fd` so the kqueue wait notices immediately.
///
/// Filtering pipeline, per chunk: stdin bytes → focus-event filter (always
/// on unless `--forward-focus-events`) → replay mute filter (only inside the
/// mute window) → UTF-8 framing → persistent input stream (`write` command
/// fallback for older hosts).
fn spawn_stdin_pump(
    socket_path: PathBuf,
    mute_until: Instant,
    filter_focus_events: bool,
    focus_reporting_active: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    eof_wake_fd: RawFd,
) {
    thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        let mut mute_filter = MuteFilter::new();
        let mut focus_filter = filter_focus_events.then(FocusEventFilter::new);
        let mut input_stream = connect_input_stream(&socket_path).ok();

        loop {
            let (raw, eof): (&[u8], bool) = match stdin.read(&mut buf) {
                Ok(0) => (&[], true),
                Ok(n) => (&buf[..n], false),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => (&[], true),
            };

            // Permanently strip focus in/out reports (ESC[I / ESC[O) the new
            // terminal emits because the replayed tail re-enabled focus
            // reporting; they are not user input and render as literal text
            // in the hosted TUI. On EOF, flush a held-back partial prefix so
            // a trailing real ESC is still delivered.
            // While the workload itself has focus reporting enabled (DEC
            // 1004, from the host's terminal_modes manifest field), the
            // reports ARE meaningful input: apps like terminal-browser use
            // focus-in to leave their background frame-rate throttle —
            // eating the report left them rendering at 4fps forever. The
            // strip only protects workloads that never opted in.
            let forward_focus = focus_reporting_active.load(Ordering::Relaxed);
            let chunk: Vec<u8> = match focus_filter.as_mut() {
                Some(filter) if !forward_focus => {
                    let mut chunk = filter.filter(raw);
                    if eof {
                        chunk.extend_from_slice(&filter.flush());
                    }
                    chunk
                }
                Some(filter) => {
                    // Forwarding: release any held-back prefix from before
                    // the mode flipped, then pass bytes through untouched.
                    let mut chunk = filter.flush();
                    chunk.extend_from_slice(raw);
                    chunk
                }
                None => raw.to_vec(),
            };

            // During the post-replay mute window, strip terminal-query
            // responses (ESC-prefixed sequences) the new terminal emits in
            // reaction to replayed queries; plain keystrokes pass through.
            // Keep filtering past the deadline while mid-sequence so a
            // response spanning the window edge is not half-forwarded.
            if Instant::now() < mute_until || !mute_filter.is_ground() {
                pending.extend_from_slice(&mute_filter.filter(&chunk));
            } else {
                pending.extend_from_slice(&chunk);
            }

            let (valid, held_back) = split_valid_utf8(&pending);
            pending = held_back;
            if !valid.is_empty() {
                let sent = match input_stream.as_mut() {
                    Some(stream) => write_input_stream_frame(stream, valid.as_bytes()).is_ok(),
                    None => false,
                };

                if !sent {
                    input_stream = None;
                    // Fallback for older hosts that don't support the
                    // persistent stream. Errors are ignored here: if the host
                    // died, the main loop's manifest check ends the process.
                    let _ = send_command(
                        Path::new(&socket_path),
                        &AttachCommand::Write { data: valid },
                    );
                }
            }

            if eof {
                break;
            }
        }

        shutdown.store(true, Ordering::Relaxed);
        let wake = [1u8];
        unsafe { libc::write(eof_wake_fd, wake.as_ptr().cast(), 1) };
    });
}

#[cfg(test)]
mod control_socket_path_tests {
    use super::*;

    #[test]
    fn short_session_dir_keeps_socket_beside_manifest() {
        let dir = PathBuf::from("/Users/me/.unpeel/app-sessions/abc");
        assert_eq!(control_socket_path(&dir, "abc"), dir.join("session.sock"));
    }

    #[test]
    fn deep_workspace_home_uses_the_host_fallback_path() {
        let dir = PathBuf::from(
            "/Users/exampleuser/.unpeel/profiles/notebook/app-sessions/75f60237-6d98-42e4-a4cb-a71a3c74c57a",
        );
        let path = control_socket_path(&dir, "75f60237-6d98-42e4-a4cb-a71a3c74c57a");
        assert!(path.starts_with("/tmp"), "{}", path.display());
        assert!(path.ends_with("75f60237-6d98-42e4-a4cb-a71a3c74c57a.sock"));
    }
}
