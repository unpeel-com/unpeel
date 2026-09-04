//! The event-driven core: one reactor thread (kqueue on BSD/macOS, epoll on
//! Linux) owns every hosted Session's PTY master read, control-socket
//! accept, and attach-stream client; one timer thread runs every Session's
//! periodic jobs; one writer thread batches every Session's journal writes.
//! A hosted Session therefore owns zero dedicated threads. One-shot control
//! commands run on transient threads (see `session_io`).
//!
//! The poller is level-triggered and deliberately tiny (raw `libc`, no async
//! runtime): `add`/`modify`/`remove` an fd with read/write interest and
//! `wait` for events. Fairness comes from the loop shape — one bounded read
//! per ready PTY per pass — not from edge-triggered draining.
//!
//! Child module of `session_host`, like `session_io`.

use super::session_io::{
    HandoffExport, JournalBackpressure, ReadOutcome, SessionIo, SessionTeardown, TokenKind,
    JOURNAL_BACKLOG_LOW_BYTES,
};
use super::{HostTimerJob, RetainedOutputWriter, HOST_TIMER_MAX_SLEEP};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const WAKER_TOKEN: u64 = u64::MAX;
/// Idle tick: stalled-client sweeps and the safety re-check of stopped
/// Sessions happen at least this often.
const REACTOR_IDLE_TICK: Duration = Duration::from_millis(1000);
const JOURNAL_FLUSH_INTERVAL: Duration =
    Duration::from_millis(super::SESSION_OUTPUT_BATCH_FLUSH_MS);
const JOURNAL_BATCH_MAX_BYTES: usize = super::SESSION_OUTPUT_BATCH_MAX_BYTES;

// ───────────────────────────── poller ─────────────────────────────

pub(crate) struct PollEvent {
    pub token: u64,
    pub readable: bool,
    pub writable: bool,
    pub hup: bool,
    /// `Some(wait status)` for a process-exit watch event.
    pub process_exit: Option<i64>,
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod sys {
    use super::PollEvent;
    use std::os::unix::io::RawFd;
    use std::time::Duration;

    pub struct Poller {
        kq: RawFd,
        events: Vec<libc::kevent>,
    }

    // `kevent.udata` is a raw pointer we only ever use as an integer token;
    // the event buffer is plain scratch memory owned by the reactor thread.
    unsafe impl Send for Poller {}

    impl Poller {
        pub fn new() -> std::io::Result<Self> {
            let kq = unsafe { libc::kqueue() };
            if kq < 0 {
                return Err(std::io::Error::last_os_error());
            }
            unsafe {
                libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            Ok(Self {
                kq,
                events: vec![unsafe { std::mem::zeroed() }; 256],
            })
        }

        fn change(&self, fd: RawFd, filter: i16, flags: u16, token: u64) -> std::io::Result<()> {
            let change = libc::kevent {
                ident: fd as libc::uintptr_t,
                filter,
                flags,
                fflags: 0,
                data: 0,
                udata: token as *mut libc::c_void,
            };
            let rc = unsafe {
                libc::kevent(
                    self.kq,
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if rc < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn set(&self, fd: RawFd, token: u64, read: bool, write: bool) -> std::io::Result<()> {
            // Level-triggered: EV_ADD without EV_CLEAR. Deleting a filter that
            // was never added answers ENOENT; that is the desired state.
            let read_flags = if read { libc::EV_ADD } else { libc::EV_DELETE };
            let write_flags = if write { libc::EV_ADD } else { libc::EV_DELETE };
            let read_result = self.change(fd, libc::EVFILT_READ, read_flags, token);
            let write_result = self.change(fd, libc::EVFILT_WRITE, write_flags, token);
            if read {
                read_result?;
            }
            if write {
                write_result?;
            }
            Ok(())
        }

        pub fn remove(&self, fd: RawFd) {
            let _ = self.change(fd, libc::EVFILT_READ, libc::EV_DELETE, 0);
            let _ = self.change(fd, libc::EVFILT_WRITE, libc::EV_DELETE, 0);
        }

        /// Watch a process that is not our child for exit (handed-over
        /// shells). The event's `data` carries the wait status.
        pub fn watch_process(&self, pid: u32, token: u64) -> std::io::Result<()> {
            let change = libc::kevent {
                ident: pid as libc::uintptr_t,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT | libc::NOTE_EXITSTATUS,
                data: 0,
                udata: token as *mut libc::c_void,
            };
            let rc = unsafe {
                libc::kevent(
                    self.kq,
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if rc < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn wait(&mut self, timeout: Option<Duration>) -> std::io::Result<Vec<PollEvent>> {
            let ts = timeout.map(|t| libc::timespec {
                tv_sec: t.as_secs() as libc::time_t,
                tv_nsec: t.subsec_nanos() as libc::c_long,
            });
            let n = unsafe {
                libc::kevent(
                    self.kq,
                    std::ptr::null(),
                    0,
                    self.events.as_mut_ptr(),
                    self.events.len() as libc::c_int,
                    ts.as_ref()
                        .map(|t| t as *const libc::timespec)
                        .unwrap_or(std::ptr::null()),
                )
            };
            if n < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    return Ok(Vec::new());
                }
                return Err(error);
            }
            let mut out = Vec::with_capacity(n as usize);
            for event in &self.events[..n as usize] {
                let token = event.udata as u64;
                let hup = event.flags & libc::EV_EOF != 0 || event.flags & libc::EV_ERROR != 0;
                out.push(PollEvent {
                    token,
                    readable: event.filter == libc::EVFILT_READ,
                    writable: event.filter == libc::EVFILT_WRITE,
                    hup,
                    process_exit: (event.filter == libc::EVFILT_PROC).then_some(event.data as i64),
                });
            }
            Ok(out)
        }
    }

    impl Drop for Poller {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.kq);
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod sys {
    use super::PollEvent;
    use std::os::unix::io::RawFd;
    use std::time::Duration;

    pub struct Poller {
        ep: RawFd,
        events: Vec<libc::epoll_event>,
    }

    impl Poller {
        pub fn new() -> std::io::Result<Self> {
            let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
            if ep < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self {
                ep,
                events: vec![libc::epoll_event { events: 0, u64: 0 }; 256],
            })
        }

        pub fn set(&self, fd: RawFd, token: u64, read: bool, write: bool) -> std::io::Result<()> {
            let mut events = 0u32;
            if read {
                events |= libc::EPOLLIN as u32;
            }
            if write {
                events |= libc::EPOLLOUT as u32;
            }
            if events == 0 {
                self.remove(fd);
                return Ok(());
            }
            let mut event = libc::epoll_event { events, u64: token };
            let rc = unsafe { libc::epoll_ctl(self.ep, libc::EPOLL_CTL_MOD, fd, &mut event) };
            if rc == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(error);
            }
            let rc = unsafe { libc::epoll_ctl(self.ep, libc::EPOLL_CTL_ADD, fd, &mut event) };
            if rc < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn remove(&self, fd: RawFd) {
            let mut event = libc::epoll_event { events: 0, u64: 0 };
            let _ = unsafe { libc::epoll_ctl(self.ep, libc::EPOLL_CTL_DEL, fd, &mut event) };
        }

        /// No process watch on epoll (pidfd is a later increment): a
        /// handed-over shell's exit is observed through its PTY EOF and its
        /// exit code recorded as unknown.
        pub fn watch_process(&self, _pid: u32, _token: u64) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "process watch unavailable",
            ))
        }

        pub fn wait(&mut self, timeout: Option<Duration>) -> std::io::Result<Vec<PollEvent>> {
            let timeout_ms = timeout
                .map(|t| t.as_millis().min(i32::MAX as u128) as libc::c_int)
                .unwrap_or(-1);
            let n = unsafe {
                libc::epoll_wait(
                    self.ep,
                    self.events.as_mut_ptr(),
                    self.events.len() as libc::c_int,
                    timeout_ms,
                )
            };
            if n < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    return Ok(Vec::new());
                }
                return Err(error);
            }
            let mut out = Vec::with_capacity(n as usize);
            for event in &self.events[..n as usize] {
                let flags = event.events;
                let hup = flags & (libc::EPOLLHUP as u32 | libc::EPOLLERR as u32) != 0;
                out.push(PollEvent {
                    token: event.u64,
                    // A HUP'd pty master still needs a read attempt to drain
                    // and to observe EIO/EOF; report it as readable.
                    readable: flags & libc::EPOLLIN as u32 != 0 || hup,
                    writable: flags & libc::EPOLLOUT as u32 != 0,
                    hup,
                    process_exit: None,
                });
            }
            Ok(out)
        }
    }

    impl Drop for Poller {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.ep);
            }
        }
    }
}

// ───────────────────────────── registry ─────────────────────────────

/// Token allocator over the poller: maps each registered fd to the Session
/// slot and fd kind that owns it.
pub(crate) struct Registry {
    poller: sys::Poller,
    next_token: u64,
    owners: HashMap<u64, (usize, TokenKind)>,
    /// Shared PTY read buffer (see `SessionIo::pty_readable`).
    pub(crate) scratch: Vec<u8>,
}

impl Registry {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            poller: sys::Poller::new()?,
            next_token: 1,
            owners: HashMap::new(),
            scratch: vec![0u8; super::SESSION_OUTPUT_READ_BUFFER_BYTES],
        })
    }

    pub(crate) fn add(
        &mut self,
        fd: RawFd,
        slot: usize,
        kind: TokenKind,
        read: bool,
        write: bool,
    ) -> Result<u64, String> {
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.poller
            .set(fd, token, read, write)
            .map_err(|e| format!("Failed to register fd {fd}: {e}"))?;
        self.owners.insert(token, (slot, kind));
        Ok(token)
    }

    pub(crate) fn modify(
        &mut self,
        fd: RawFd,
        token: u64,
        read: bool,
        write: bool,
    ) -> Result<(), String> {
        self.poller
            .set(fd, token, read, write)
            .map_err(|e| format!("Failed to update fd {fd}: {e}"))
    }

    pub(crate) fn remove(&mut self, fd: RawFd, token: u64) {
        self.poller.remove(fd);
        self.owners.remove(&token);
    }

    /// Watch a non-child process for exit; `Ok(None)` when the platform
    /// has no such watch.
    pub(crate) fn watch_process(&mut self, pid: u32, slot: usize) -> Result<Option<u64>, String> {
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        match self.poller.watch_process(pid, token) {
            Ok(()) => {
                self.owners.insert(token, (slot, TokenKind::Process));
                Ok(Some(token))
            }
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Ok(None),
            Err(error) => Err(format!("Failed to watch process {pid}: {error}")),
        }
    }
}

// ───────────────────────────── waker ─────────────────────────────

struct Waker {
    read_fd: RawFd,
    write_fd: RawFd,
}

impl Waker {
    fn new() -> std::io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in fds {
            super::session_io::set_nonblocking(fd, true)?;
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        Ok(Self {
            read_fd: fds[0],
            write_fd: fds[1],
        })
    }

    fn wake(&self) {
        let byte = [1u8];
        let _ = unsafe { libc::write(self.write_fd, byte.as_ptr() as *const libc::c_void, 1) };
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        loop {
            let n = unsafe {
                libc::read(
                    self.read_fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
        }
    }
}

impl Drop for Waker {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

// ───────────────────────────── messages ─────────────────────────────

pub(crate) enum Control {
    Register {
        session: Box<SessionIo>,
        ack: mpsc::Sender<Result<(), String>>,
    },
    /// Revisit a Session: `running` flipped, or a broadcaster write happened
    /// off the reactor thread.
    Wake(usize),
    /// The journal writer drained this Session below the low mark.
    JournalDrained(usize),
    /// The journal writer failed for this Session; end it.
    JournalFailed(usize),
    /// A teardown finished; release freed heap on the next idle tick.
    SessionEnded,
    /// Hand every hosted Session (and the core's own lock/listener fds) to
    /// the new core on the other end of `stream`; reply on `done`.
    Handoff {
        stream: UnixStream,
        core_fds: Vec<RawFd>,
        done: mpsc::Sender<Result<Vec<String>, String>>,
    },
    /// Host a Session rebuilt from a handoff; watch its child if possible.
    Adopt {
        session: Box<SessionIo>,
        child_pid: Option<u32>,
        exit_slot: Arc<Mutex<Option<portable_pty::ExitStatus>>>,
        ack: mpsc::Sender<Result<(), String>>,
    },
}

pub(crate) enum JournalMsg {
    Open {
        id: u64,
        writer: RetainedOutputWriter,
        pressure: Arc<JournalBackpressure>,
    },
    Chunk {
        id: u64,
        data: Vec<u8>,
    },
    Close {
        id: u64,
        ack: mpsc::Sender<Result<(), String>>,
    },
    /// Write everything pending for this Session now (handoff quiesce).
    Flush {
        id: u64,
        ack: mpsc::Sender<Result<(), String>>,
    },
}

pub(crate) enum TimerMsg {
    Add {
        slot: usize,
        jobs: Vec<HostTimerJob>,
    },
    Remove {
        slot: usize,
        ack: mpsc::Sender<()>,
    },
}

#[derive(Clone)]
pub(crate) struct ReactorHandle {
    control_tx: mpsc::Sender<Control>,
    waker: Arc<Waker>,
}

impl ReactorHandle {
    pub(crate) fn session_ended(&self) {
        self.send(Control::SessionEnded);
    }

    /// Run a handoff of every hosted Session to the core on `stream`.
    /// Returns the number of Sessions moved once the new core committed,
    /// or the error after which every Session resumed here.
    pub(crate) fn handoff(
        &self,
        stream: UnixStream,
        core_fds: Vec<RawFd>,
    ) -> Result<Vec<String>, String> {
        let (done_tx, done_rx) = mpsc::channel();
        self.send(Control::Handoff {
            stream,
            core_fds,
            done: done_tx,
        });
        done_rx
            .recv()
            .unwrap_or_else(|_| Err("reactor dropped the handoff".into()))
    }

    pub(crate) fn wake_session(&self, slot: usize) {
        let _ = self.control_tx.send(Control::Wake(slot));
        self.waker.wake();
    }

    fn send(&self, control: Control) {
        let _ = self.control_tx.send(control);
        self.waker.wake();
    }
}

/// The three core-wide threads, started on first use by either hosting
/// mode (per-process `__session_host__` runs them with N = 1).
pub(crate) struct CoreServices {
    pub reactor: ReactorHandle,
    pub journal_tx: mpsc::Sender<JournalMsg>,
}

/// Host a Session rebuilt from a handoff (new core side).
pub(crate) fn adopt_session(rebuilt: super::session_io::RebuiltSession) -> Result<(), String> {
    let services = services()?;
    let (ack_tx, ack_rx) = mpsc::channel();
    services.reactor.send(Control::Adopt {
        session: Box::new(rebuilt.session),
        child_pid: rebuilt.child_pid,
        exit_slot: rebuilt.exit_slot,
        ack: ack_tx,
    });
    ack_rx.recv().map_err(|_| "reactor is gone".to_string())?
}

static SERVICES: OnceLock<Result<CoreServices, String>> = OnceLock::new();

pub(crate) fn services() -> Result<&'static CoreServices, String> {
    SERVICES
        .get_or_init(start_services)
        .as_ref()
        .map_err(|error| error.clone())
}

fn start_services() -> Result<CoreServices, String> {
    let (journal_tx, journal_rx) = mpsc::channel::<JournalMsg>();
    let (timer_tx, timer_rx) = mpsc::channel::<TimerMsg>();
    let (control_tx, control_rx) = mpsc::channel::<Control>();
    let waker = Arc::new(Waker::new().map_err(|e| format!("Failed to create reactor waker: {e}"))?);
    let reactor = ReactorHandle {
        control_tx,
        waker: Arc::clone(&waker),
    };

    let registry = Registry::new().map_err(|e| format!("Failed to create poller: {e}"))?;
    registry
        .poller
        .set(waker.read_fd, WAKER_TOKEN, true, false)
        .map_err(|e| format!("Failed to register reactor waker: {e}"))?;

    let reactor_for_writer = reactor.clone();
    thread::Builder::new()
        .name("core-journal".into())
        .spawn(move || run_journal_writer(journal_rx, reactor_for_writer))
        .map_err(|e| format!("Failed to spawn journal writer: {e}"))?;

    thread::Builder::new()
        .name("core-timer".into())
        .spawn(move || run_timer(timer_rx))
        .map_err(|e| format!("Failed to spawn timer thread: {e}"))?;

    let timer_for_loop = timer_tx.clone();
    thread::Builder::new()
        .name("core-reactor".into())
        .spawn(move || {
            let mut reactor = Reactor {
                registry,
                waker,
                control_rx,
                timer_tx: timer_for_loop,
                sessions: Vec::new(),
                free_slots: Vec::new(),
                release_pending: false,
                process_watches: HashMap::new(),
            };
            reactor.run();
        })
        .map_err(|e| format!("Failed to spawn reactor: {e}"))?;

    drop(timer_tx);
    Ok(CoreServices {
        reactor,
        journal_tx,
    })
}

/// Hand a fully set-up Session to the reactor. Returns once its fds are
/// registered (or with the registration error).
pub(crate) fn host_session(session: SessionIo) -> Result<(), String> {
    let services = services()?;
    let (ack_tx, ack_rx) = mpsc::channel();
    services.reactor.send(Control::Register {
        session: Box::new(session),
        ack: ack_tx,
    });
    ack_rx.recv().map_err(|_| "reactor is gone".to_string())?
}

// ───────────────────────────── reactor ─────────────────────────────

struct Reactor {
    registry: Registry,
    waker: Arc<Waker>,
    control_rx: mpsc::Receiver<Control>,
    timer_tx: mpsc::Sender<TimerMsg>,
    sessions: Vec<Option<Box<SessionIo>>>,
    free_slots: Vec<usize>,
    /// Set by a finished teardown; the idle tick runs the allocator release
    /// once, after every racing teardown has had its say.
    release_pending: bool,
    /// Exit slots of handed-over children being watched, by process token.
    process_watches: HashMap<u64, Arc<Mutex<Option<portable_pty::ExitStatus>>>>,
}

impl Reactor {
    fn run(&mut self) {
        let mut last_tick = Instant::now();
        loop {
            self.drain_control();
            let events = match self.registry.poller.wait(Some(REACTOR_IDLE_TICK)) {
                Ok(events) => events,
                Err(error) => {
                    log::error!("reactor wait failed: {error}");
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            let mut touched: Vec<usize> = Vec::new();
            for event in events {
                if event.token == WAKER_TOKEN {
                    self.waker.drain();
                    continue;
                }
                let Some(&(slot, kind)) = self.registry.owners.get(&event.token) else {
                    continue;
                };
                self.dispatch(slot, kind, event, &mut touched);
            }
            // Push freshly broadcast output to streaming clients in the same
            // pass it was read: one batched frame per pass per client.
            touched.sort_unstable();
            touched.dedup();
            for slot in touched {
                self.with_session(slot, |session, registry| {
                    session.flush_stream_clients(registry);
                    None
                });
            }
            if last_tick.elapsed() >= REACTOR_IDLE_TICK {
                last_tick = Instant::now();
                self.idle_tick();
            }
        }
    }

    fn dispatch(
        &mut self,
        slot: usize,
        kind: TokenKind,
        event: PollEvent,
        touched: &mut Vec<usize>,
    ) {
        match kind {
            TokenKind::Process => {
                if let Some(exit_slot) = self.process_watches.remove(&event.token) {
                    super::session_io::record_child_exit(&exit_slot, event.process_exit);
                }
                self.registry.owners.remove(&event.token);
                // The shell is gone; its PTY reaches EOF on its own, and a
                // retained slave is caught by the stop path like a Kill.
                let _ = slot;
            }
            TokenKind::Pty => {
                if event.writable {
                    self.with_session(slot, |session, registry| {
                        if let Err(error) = session.drain_pending_input(registry) {
                            log::warn!("{}: {error}", session.session_id());
                        }
                        None
                    });
                }
                if event.readable || event.hup {
                    touched.push(slot);
                    let ended = self.with_session(slot, |session, registry| {
                        match session.pty_readable(registry) {
                            ReadOutcome::Continue => None,
                            ReadOutcome::Ended => Some(Ok(())),
                        }
                    });
                    if let Some(outcome) = ended {
                        self.end_session(slot, outcome);
                    }
                }
            }
            TokenKind::Listener => {
                self.with_session(slot, |session, registry| {
                    session.accept_clients(registry, slot);
                    None
                });
            }
            TokenKind::Client => {
                touched.push(slot);
                self.with_session(slot, |session, registry| {
                    if event.readable || event.hup {
                        session.client_readable(registry, event.token);
                    }
                    if event.writable {
                        session.client_writable(registry, event.token);
                    }
                    None
                });
            }
        }
    }

    /// Run `f` on one Session with panic isolation: a panic inside a
    /// Session's callback ends that Session (exited manifest, `Err` to its
    /// owner) and the reactor keeps serving every other one.
    fn with_session(
        &mut self,
        slot: usize,
        f: impl FnOnce(&mut SessionIo, &mut Registry) -> Option<Result<(), String>>,
    ) -> Option<Result<(), String>> {
        let Some(Some(session)) = self.sessions.get_mut(slot) else {
            return None;
        };
        let registry = &mut self.registry;
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(session, registry)));
        match outcome {
            Ok(result) => result,
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic".into());
                let id = session.session_id().to_string();
                crate::hook_assets::append_trace_log_line(&format!(
                    "pty-core session {id} panicked in the reactor: {message}"
                ));
                self.end_session(slot, Err(format!("session I/O panicked: {message}")));
                None
            }
        }
    }

    fn drain_control(&mut self) {
        while let Ok(control) = self.control_rx.try_recv() {
            match control {
                Control::Register { session, ack } => {
                    let result = self.register(*session);
                    let _ = ack.send(result);
                }
                Control::Wake(slot) => {
                    let mut needs_flush = false;
                    let ended = self.with_session(slot, |session, registry| {
                        needs_flush = true;
                        if !session.shared.running.load(Ordering::Relaxed) {
                            match session.drain_after_stop(registry) {
                                ReadOutcome::Ended => return Some(Ok(())),
                                ReadOutcome::Continue => {}
                            }
                        }
                        None
                    });
                    if let Some(outcome) = ended {
                        self.end_session(slot, outcome);
                    } else if needs_flush {
                        self.with_session(slot, |session, registry| {
                            session.flush_stream_clients(registry);
                            None
                        });
                    }
                }
                Control::JournalDrained(slot) => {
                    self.with_session(slot, |session, registry| {
                        session.journal_drained(registry);
                        None
                    });
                }
                Control::JournalFailed(slot) => {
                    self.end_session(slot, Err("Failed to write output log".into()));
                }
                Control::SessionEnded => {
                    self.release_pending = true;
                }
                Control::Handoff {
                    stream,
                    core_fds,
                    done,
                } => {
                    let result = self.handoff(stream, core_fds);
                    let _ = done.send(result);
                }
                Control::Adopt {
                    session,
                    child_pid,
                    exit_slot,
                    ack,
                } => {
                    let result = self.adopt(*session, child_pid, exit_slot);
                    let _ = ack.send(result);
                }
            }
        }
    }

    fn register(&mut self, mut session: SessionIo) -> Result<(), String> {
        let slot = match self.free_slots.pop() {
            Some(slot) => slot,
            None => {
                self.sessions.push(None);
                self.sessions.len() - 1
            }
        };
        let jobs = session.take_timer_jobs();
        if let Err(error) = session.register(&mut self.registry, slot) {
            self.free_slots.push(slot);
            return Err(error);
        }
        let _ = self.timer_tx.send(TimerMsg::Add { slot, jobs });
        self.sessions[slot] = Some(Box::new(session));
        Ok(())
    }

    fn end_session(&mut self, slot: usize, outcome: Result<(), String>) {
        let Some(slot_ref) = self.sessions.get_mut(slot) else {
            return;
        };
        let Some(session) = slot_ref.take() else {
            return;
        };
        self.free_slots.push(slot);
        let registry = &mut self.registry;
        let teardown: Option<SessionTeardown> =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.finish(registry)))
                .ok();
        let Some(teardown) = teardown else {
            crate::hook_assets::append_trace_log_line(&format!(
                "pty-core session slot {slot} panicked while finishing"
            ));
            return;
        };
        let timer_tx = self.timer_tx.clone();
        // The epilogue waits on the writer and timer acks and takes the
        // manifest lock; keep that off the reactor so no other Session
        // notices a neighbour leaving.
        if thread::Builder::new()
            .name("session-exit".into())
            .spawn(move || teardown.run(&timer_tx, outcome))
            .is_err()
        {
            log::error!("failed to spawn session teardown thread");
        }
    }

    fn adopt(
        &mut self,
        session: SessionIo,
        child_pid: Option<u32>,
        exit_slot: Arc<Mutex<Option<portable_pty::ExitStatus>>>,
    ) -> Result<(), String> {
        let slot = match self.free_slots.last() {
            Some(slot) => *slot,
            None => self.sessions.len(),
        };
        self.register(session)?;
        if let Some(pid) = child_pid {
            if let Ok(Some(token)) = self.registry.watch_process(pid, slot) {
                self.process_watches.insert(token, exit_slot);
            }
        }
        Ok(())
    }

    /// Move every hosted Session to the new core on `stream`. Runs on the
    /// reactor thread: no other Session is served until the new core has
    /// committed or the handoff is abandoned and everything resumed.
    fn handoff(
        &mut self,
        mut stream: UnixStream,
        core_fds: Vec<RawFd>,
    ) -> Result<Vec<String>, String> {
        use super::fd_pass::{recv_message, send_message};
        use std::io::Write as _;

        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let slots: Vec<usize> = (0..self.sessions.len())
            .filter(|slot| self.sessions[*slot].is_some())
            .collect();

        // Quiesce: journals flushed, timer jobs retired. Both ack from
        // their threads; a wedged one aborts the handoff cleanly.
        let mut paused: Vec<usize> = Vec::new();
        let mut failure: Option<String> = None;
        for &slot in &slots {
            let Some(session) = self.sessions[slot].as_mut() else {
                continue;
            };
            let (ack_tx, ack_rx) = mpsc::channel();
            let _ = session.journal_tx().send(JournalMsg::Flush {
                id: session.journal_id(),
                ack: ack_tx,
            });
            match ack_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failure = Some(format!(
                        "journal flush for {}: {error}",
                        session.session_id()
                    ));
                    break;
                }
                Err(_) => {
                    failure = Some(format!(
                        "journal flush for {} timed out",
                        session.session_id()
                    ));
                    break;
                }
            }
            let (timer_ack_tx, timer_ack_rx) = mpsc::channel();
            let _ = self.timer_tx.send(TimerMsg::Remove {
                slot,
                ack: timer_ack_tx,
            });
            if timer_ack_rx.recv_timeout(Duration::from_secs(10)).is_err() {
                failure = Some(format!(
                    "timer retire for {} timed out",
                    session.session_id()
                ));
                break;
            }
            paused.push(slot);
        }

        let mut exports: Vec<(usize, HandoffExport)> = Vec::new();
        if failure.is_none() {
            for &slot in &paused {
                let registry = &mut self.registry;
                let export = match self.sessions[slot].as_mut() {
                    Some(session) => session.export_handoff(registry),
                    None => continue,
                };
                match export {
                    Ok(export) => exports.push((slot, export)),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
        }

        let mut committed = false;
        if failure.is_none() {
            let header = serde_json::json!({
                "ok": true,
                "sessions": exports.len(),
                "protocol": 1,
            })
            .to_string();
            let mut send = send_message(&mut stream, header.as_bytes(), &core_fds)
                .map_err(|e| format!("handoff header: {e}"));
            if send.is_ok() {
                for (_, export) in &exports {
                    let meta = match serde_json::to_vec(&export.meta) {
                        Ok(meta) => meta,
                        Err(error) => {
                            send = Err(format!("encode session handoff: {error}"));
                            break;
                        }
                    };
                    if let Err(error) = send_message(&mut stream, &meta, &export.fds) {
                        send = Err(format!("send session handoff: {error}"));
                        break;
                    }
                    if let Err(error) = stream.write_all(&export.snapshot) {
                        send = Err(format!("send snapshot: {error}"));
                        break;
                    }
                }
            }
            match send {
                Err(error) => failure = Some(error),
                Ok(()) => match recv_message(&mut stream) {
                    Ok((reply, _)) => {
                        let ok = serde_json::from_slice::<serde_json::Value>(&reply)
                            .ok()
                            .and_then(|value| value.get("ok").and_then(|v| v.as_bool()))
                            .unwrap_or(false);
                        if ok {
                            committed = true;
                        } else {
                            failure = Some(format!(
                                "new core refused the handoff: {}",
                                String::from_utf8_lossy(&reply)
                            ));
                        }
                    }
                    Err(error) => failure = Some(format!("waiting for commit: {error}")),
                },
            }
        }

        if committed {
            let mut moved = Vec::with_capacity(exports.len());
            for (slot, export) in exports {
                moved.push(export.meta.id);
                if let Some(session) = self.sessions[slot].take() {
                    self.free_slots.push(slot);
                    session.forget_after_handoff(&mut self.registry);
                }
            }
            return Ok(moved);
        }

        // Abandon: tell the peer (best effort) and resume every paused
        // Session exactly where it stopped.
        let error = failure.unwrap_or_else(|| "handoff aborted".into());
        let refusal = serde_json::json!({ "ok": false, "error": error }).to_string();
        let _ = send_message(&mut stream, refusal.as_bytes(), &[]);
        for slot in paused {
            let registry = &mut self.registry;
            if let Some(session) = self.sessions[slot].as_mut() {
                let jobs = session.resume_after_handoff(registry);
                let _ = self.timer_tx.send(TimerMsg::Add { slot, jobs });
            }
        }
        Err(error)
    }

    fn idle_tick(&mut self) {
        if self.release_pending {
            self.release_pending = false;
            super::session_io::release_freed_memory();
        }
        let slots: Vec<usize> = (0..self.sessions.len())
            .filter(|slot| self.sessions[*slot].is_some())
            .collect();
        for slot in slots {
            let ended = self.with_session(slot, |session, registry| {
                session.sweep_stalled_clients(registry);
                session.release_idle_buffers();
                if !session.shared.running.load(Ordering::Relaxed) {
                    // Safety net: a Kill whose wake was lost still ends.
                    if let ReadOutcome::Ended = session.drain_after_stop(registry) {
                        return Some(Ok(()));
                    }
                }
                None
            });
            if let Some(outcome) = ended {
                self.end_session(slot, outcome);
            }
        }
    }
}

// ───────────────────────────── journal writer ─────────────────────────────

struct JournalSession {
    writer: RetainedOutputWriter,
    pressure: Arc<JournalBackpressure>,
    pending: Vec<u8>,
    first_pending_at: Option<Instant>,
    /// When the last chunk arrived; an idle Session's batch buffer is
    /// released after `JOURNAL_IDLE_RELEASE` so a quiet terminal holds no
    /// batch capacity at all.
    last_chunk_at: Instant,
    error: Option<String>,
}

/// A Session whose journal saw no bytes for this long drops its batch
/// buffer capacity (it is re-grown from the next chunk).
const JOURNAL_IDLE_RELEASE: Duration = Duration::from_secs(1);

impl JournalSession {
    /// Idle diet: once a quiet second has passed since the last chunk and
    /// nothing is pending, the batch buffer's capacity goes back to the
    /// allocator. Returns whether anything was released.
    fn release_idle_capacity(&mut self, now: Instant) -> bool {
        if self.pending.is_empty()
            && self.pending.capacity() > 0
            && now.duration_since(self.last_chunk_at) >= JOURNAL_IDLE_RELEASE
        {
            self.pending = Vec::new();
            return true;
        }
        false
    }

    fn flush(&mut self) {
        if self.pending.is_empty() || self.error.is_some() {
            self.pending.clear();
            self.first_pending_at = None;
            return;
        }
        if let Err(error) = self.writer.write_all(&self.pending) {
            self.error = Some(format!("Failed to write output log: {error}"));
        }
        self.pending.clear();
        self.first_pending_at = None;
    }
}

/// One thread batches every Session's journal writes with the released
/// per-session cadence (flush at `SESSION_OUTPUT_BATCH_MAX_BYTES` or
/// `SESSION_OUTPUT_BATCH_FLUSH_MS` after the first pending byte).
fn run_journal_writer(rx: mpsc::Receiver<JournalMsg>, reactor: ReactorHandle) {
    let mut sessions: HashMap<u64, JournalSession> = HashMap::new();
    loop {
        let now = Instant::now();
        let holds_capacity = sessions
            .values()
            .any(|s| s.pending.capacity() > 0 && s.pending.is_empty());
        let next_deadline = sessions
            .values()
            .filter_map(|s| s.first_pending_at)
            .map(|at| at + JOURNAL_FLUSH_INTERVAL)
            .min()
            .or_else(|| holds_capacity.then(|| now + JOURNAL_IDLE_RELEASE));
        let message = match next_deadline {
            Some(deadline) => match rx.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(message) => Some(message),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            },
        };
        match message {
            Some(JournalMsg::Open {
                id,
                writer,
                pressure,
            }) => {
                sessions.insert(
                    id,
                    JournalSession {
                        writer,
                        pressure,
                        pending: Vec::new(),
                        first_pending_at: None,
                        last_chunk_at: Instant::now(),
                        error: None,
                    },
                );
            }
            Some(JournalMsg::Chunk { id, data }) => {
                let Some(session) = sessions.get_mut(&id) else {
                    continue;
                };
                let slot = session.pressure.slot.load(Ordering::Acquire);
                let previous = session
                    .pressure
                    .backlog
                    .fetch_sub(data.len(), Ordering::AcqRel);
                let remaining = previous.saturating_sub(data.len());
                if session.pressure.paused.load(Ordering::Acquire)
                    && remaining < JOURNAL_BACKLOG_LOW_BYTES
                {
                    session.pressure.paused.store(false, Ordering::Release);
                    reactor.send(Control::JournalDrained(slot));
                }
                session.last_chunk_at = Instant::now();
                if session.error.is_none() {
                    if session.pending.is_empty() {
                        session.first_pending_at = Some(Instant::now());
                    }
                    session.pending.extend_from_slice(&data);
                    if session.pending.len() >= JOURNAL_BATCH_MAX_BYTES {
                        session.flush();
                    }
                    if session.error.is_some() {
                        reactor.send(Control::JournalFailed(slot));
                    }
                }
            }
            Some(JournalMsg::Flush { id, ack }) => {
                let result = match sessions.get_mut(&id) {
                    Some(session) => {
                        session.flush();
                        match session.error.clone() {
                            Some(error) => Err(error),
                            None => Ok(()),
                        }
                    }
                    None => Ok(()),
                };
                let _ = ack.send(result);
            }
            Some(JournalMsg::Close { id, ack }) => {
                let result = match sessions.remove(&id) {
                    Some(mut session) => {
                        session.flush();
                        // Drop the writer: closes the file and its retention
                        // record like the old writer thread's exit did.
                        match session.error.take() {
                            Some(error) => Err(error),
                            None => Ok(()),
                        }
                    }
                    None => Ok(()),
                };
                let _ = ack.send(result);
            }
            None => {}
        }
        let now = Instant::now();
        let mut failed = Vec::new();
        for session in sessions.values_mut() {
            if session
                .first_pending_at
                .is_some_and(|at| now.duration_since(at) >= JOURNAL_FLUSH_INTERVAL)
            {
                session.flush();
                if session.error.is_some() {
                    failed.push(session.pressure.slot.load(Ordering::Acquire));
                }
            }
            session.release_idle_capacity(now);
        }
        for slot in failed {
            reactor.send(Control::JournalFailed(slot));
        }
    }
}

// ───────────────────────────── timer ─────────────────────────────

/// Every Session's periodic jobs on one thread. Same scheduling as the old
/// per-session `run_host_timer_jobs`: a job's next run is scheduled from
/// when it finished, jobs run strictly sequentially (so one Session's jobs
/// never interleave with each other), and the idle tick never exceeds
/// `HOST_TIMER_MAX_SLEEP` so a removal ack is never held hostage.
fn run_timer(rx: mpsc::Receiver<TimerMsg>) {
    let mut jobs: HashMap<usize, Vec<HostTimerJob>> = HashMap::new();
    let running = AtomicBool::new(true);
    loop {
        let now = Instant::now();
        let mut next_wake = now + HOST_TIMER_MAX_SLEEP;
        for (_, session_jobs) in jobs.iter_mut() {
            session_jobs.retain_mut(|job| {
                if now >= job.next_at {
                    let keep =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.run)()))
                            .unwrap_or(false);
                    if !keep {
                        return false;
                    }
                    job.next_at = Instant::now() + job.interval;
                }
                true
            });
            for job in session_jobs.iter() {
                next_wake = next_wake.min(job.next_at);
            }
        }
        let sleep = next_wake.saturating_duration_since(Instant::now());
        let message = match rx.recv_timeout(sleep.max(Duration::from_millis(1))) {
            Ok(message) => Some(message),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut pending = message;
        while let Some(message) = pending.take() {
            match message {
                TimerMsg::Add { slot, jobs: new } => {
                    jobs.entry(slot).or_default().extend(new);
                }
                TimerMsg::Remove { slot, ack } => {
                    jobs.remove(&slot);
                    let _ = ack.send(());
                }
            }
            pending = rx.try_recv().ok();
        }
        if !running.load(Ordering::Relaxed) {
            return;
        }
    }
}

#[allow(dead_code)]
pub(crate) fn wake_reactor() {
    if let Ok(services) = services() {
        services.reactor.waker.wake();
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Control>();
    is_send::<JournalMsg>();
    is_send::<TimerMsg>();
    let _ = Mutex::new(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn reactor_handle_stub() -> ReactorHandle {
        let (control_tx, _control_rx) = mpsc::channel();
        ReactorHandle {
            control_tx,
            waker: Arc::new(Waker::new().unwrap()),
        }
    }

    #[test]
    fn journal_session_releases_batch_capacity_after_a_quiet_second() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.bin");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let writer = RetainedOutputWriter::new(file, path.clone(), 0, 1 << 20, 1 << 20).unwrap();
        let mut session = JournalSession {
            writer,
            pressure: Arc::new(JournalBackpressure::default()),
            pending: Vec::new(),
            first_pending_at: None,
            last_chunk_at: Instant::now(),
            error: None,
        };
        // A burst grows the batch buffer; flushing empties it but keeps
        // the capacity (the next batch reuses it while the burst lasts).
        session.pending.extend_from_slice(&[b'x'; 96 * 1024]);
        session.first_pending_at = Some(Instant::now());
        session.flush();
        assert!(session.pending.is_empty());
        assert!(session.pending.capacity() >= 96 * 1024);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 96 * 1024);
        // Not yet quiet: nothing released.
        assert!(!session.release_idle_capacity(Instant::now()));
        assert!(session.pending.capacity() >= 96 * 1024);
        // A quiet second later the capacity is gone; a new chunk regrows it.
        let later = Instant::now() + JOURNAL_IDLE_RELEASE + Duration::from_millis(1);
        assert!(session.release_idle_capacity(later));
        assert_eq!(session.pending.capacity(), 0);
        session.pending.extend_from_slice(b"more");
        session.last_chunk_at = Instant::now();
        assert!(!session.release_idle_capacity(later));
        session.flush();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 96 * 1024 + 4);
        let _ = reactor_handle_stub();
        let _ = UnixStream::pair();
    }
}
