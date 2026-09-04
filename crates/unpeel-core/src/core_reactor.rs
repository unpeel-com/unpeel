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
//! Two invariants keep one Session from touching another:
//!
//! - **The reactor thread never blocks on a Session.** Every PTY write it
//!   performs is a non-blocking write into that Session's bounded input
//!   queue (`SessionIo::drain_pending_input`); one-shot `Write` commands
//!   submit into the same queue (`Control::Input`) instead of writing the
//!   PTY themselves, so no transient thread ever holds a lock the reactor
//!   needs across a blocking write. A wedged terminal fills its own queue
//!   and is told so; its siblings' sockets keep answering.
//! - **Slots are identities only together with a generation.** Every
//!   cross-thread message about a Session carries a [`SlotRef`] (slot +
//!   monotonic generation) and is dropped when the generation no longer
//!   matches, and a freed slot is not reused until the teardown thread that
//!   owns its last messages has reported done (`Control::SessionEnded`).
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

        /// Drop a process watch (a Session ended before its child did).
        pub fn unwatch_process(&self, pid: u32) {
            let change = libc::kevent {
                ident: pid as libc::uintptr_t,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_DELETE,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let _ = unsafe {
                libc::kevent(
                    self.kq,
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
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

        pub fn unwatch_process(&self, _pid: u32) {}

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
    pub(crate) fn new() -> std::io::Result<Self> {
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

    /// Watch a process for exit (a hosted child, owned or handed over);
    /// `Ok(None)` when the platform has no such watch.
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

    /// Forget a process watch before its event fires (the Session ended
    /// first); a late event for the token is then ignored by `owners`.
    pub(crate) fn unwatch_process(&mut self, pid: u32, token: u64) {
        self.poller.unwatch_process(pid);
        self.owners.remove(&token);
    }
}

/// The identity of a hosted Session inside the reactor: its slot plus the
/// generation that occupied it. Every message from another thread names
/// one of these, and the reactor drops any whose generation is not the
/// slot's current one, so a late timer/journal/teardown message for a
/// Session that already left can never reach the slot's next occupant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlotRef {
    pub slot: usize,
    pub generation: u64,
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
    Wake(SlotRef),
    /// Bytes for a Session's PTY from a one-shot `Write` command. The
    /// reactor checks `write_id` against the Session's ledger, queues the
    /// bytes into its bounded input queue, records the id, and answers
    /// `Ok(true)` (queued), `Ok(false)` (duplicate id, nothing queued), or
    /// `Err` (queue full or Session gone). The submitting thread never
    /// touches the PTY itself.
    Input {
        slot: SlotRef,
        data: Vec<u8>,
        write_id: Option<String>,
        ack: mpsc::Sender<Result<bool, String>>,
    },
    /// The journal writer drained this Session below the low mark.
    JournalDrained(SlotRef),
    /// The journal writer failed for this Session; end it.
    JournalFailed(SlotRef),
    /// A teardown finished: its slot may be reused now, and freed heap is
    /// released on the next idle tick.
    SessionEnded(SlotRef),
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
    /// Install a Session's jobs; replaces whatever an older generation left
    /// in the slot.
    Add {
        slot: SlotRef,
        jobs: Vec<HostTimerJob>,
    },
    /// Retire a Session's jobs. Always acked; a stale generation retires
    /// nothing (the slot already belongs to a newer Session).
    Remove {
        slot: SlotRef,
        ack: mpsc::Sender<()>,
    },
}

/// How long a one-shot `Write` waits for the reactor to queue its bytes.
/// The reactor never blocks on a Session, so this only trips while it is
/// busy with a handoff; the bytes still land when it gets to them.
const INPUT_SUBMIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct ReactorHandle {
    control_tx: mpsc::Sender<Control>,
    waker: Arc<Waker>,
}

impl ReactorHandle {
    pub(crate) fn session_ended(&self, slot: SlotRef) {
        self.send(Control::SessionEnded(slot));
    }

    /// Queue `data` for a Session's PTY from a transient command thread.
    /// See `Control::Input` for the reply.
    pub(crate) fn submit_input(
        &self,
        slot: SlotRef,
        data: Vec<u8>,
        write_id: Option<String>,
    ) -> Result<bool, String> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.send(Control::Input {
            slot,
            data,
            write_id,
            ack: ack_tx,
        });
        match ack_rx.recv_timeout(INPUT_SUBMIT_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err("Write error: the session host did not accept input in time".into())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("Write error: session is no longer hosted".into())
            }
        }
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

    pub(crate) fn wake_session(&self, slot: SlotRef) {
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
            let mut reactor = Reactor::new(registry, waker, control_rx, timer_for_loop);
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

/// A child-exit watch: which Session it belongs to and where its status
/// goes (shared with a `HandedOverChild`, informational for an owned one).
struct ProcessWatch {
    slot: SlotRef,
    pid: u32,
    exit_slot: Arc<Mutex<Option<portable_pty::ExitStatus>>>,
}

struct Reactor {
    registry: Registry,
    waker: Arc<Waker>,
    control_rx: mpsc::Receiver<Control>,
    timer_tx: mpsc::Sender<TimerMsg>,
    sessions: Vec<Option<Box<SessionIo>>>,
    /// The generation currently (or last) occupying each slot; bumped on
    /// every registration so a `SlotRef` from an earlier occupant fails
    /// the check in `with_session_ref`.
    generations: Vec<u64>,
    /// Slots whose last occupant's teardown has reported done. A slot
    /// leaves `sessions` at `end_session` but only arrives here on
    /// `Control::SessionEnded`, so no late message from that teardown can
    /// meet a new occupant.
    free_slots: Vec<usize>,
    /// Set by a finished teardown; the idle tick runs the allocator release
    /// once, after every racing teardown has had its say.
    release_pending: bool,
    /// Child-exit watches by process token.
    process_watches: HashMap<u64, ProcessWatch>,
}

impl Reactor {
    fn new(
        registry: Registry,
        waker: Arc<Waker>,
        control_rx: mpsc::Receiver<Control>,
        timer_tx: mpsc::Sender<TimerMsg>,
    ) -> Self {
        Self {
            registry,
            waker,
            control_rx,
            timer_tx,
            sessions: Vec::new(),
            generations: Vec::new(),
            free_slots: Vec::new(),
            release_pending: false,
            process_watches: HashMap::new(),
        }
    }

    fn run(&mut self) {
        let mut last_tick = Instant::now();
        loop {
            self.run_once(REACTOR_IDLE_TICK, &mut last_tick);
        }
    }

    /// One pass: control messages, one poll, one bounded read per ready
    /// PTY, one batched frame per touched client, and the idle tick when
    /// it is due.
    fn run_once(&mut self, wait: Duration, last_tick: &mut Instant) {
        self.drain_control();
        let events = match self.registry.poller.wait(Some(wait)) {
            Ok(events) => events,
            Err(error) => {
                log::error!("reactor wait failed: {error}");
                thread::sleep(Duration::from_millis(50));
                return;
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
            *last_tick = Instant::now();
            self.idle_tick();
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
                self.registry.owners.remove(&event.token);
                let Some(watch) = self.process_watches.remove(&event.token) else {
                    return;
                };
                super::session_io::record_child_exit(&watch.exit_slot, event.process_exit);
                // The child is gone. Its PTY usually reaches EOF on its own,
                // but not when a grandchild still holds the slave: end the
                // Session the way a Kill does (drain what is buffered, then
                // stop) so the teardown reaps the child instead of leaving
                // a zombie under the core for as long as the slave lives.
                let _ = slot;
                let ended = self.with_session_ref(watch.slot, |session, registry| {
                    session.clear_child_watch();
                    session.shared.running.store(false, Ordering::Relaxed);
                    match session.drain_after_stop(registry) {
                        ReadOutcome::Ended => Some(Ok(())),
                        ReadOutcome::Continue => None,
                    }
                });
                if let Some(outcome) = ended {
                    self.end_session(watch.slot.slot, outcome);
                }
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

    /// `with_session` for a message from another thread: runs only when the
    /// slot is still occupied by the generation the message names.
    fn with_session_ref(
        &mut self,
        slot: SlotRef,
        f: impl FnOnce(&mut SessionIo, &mut Registry) -> Option<Result<(), String>>,
    ) -> Option<Result<(), String>> {
        if !self.slot_is_current(slot) {
            return None;
        }
        self.with_session(slot.slot, f)
    }

    fn slot_is_current(&self, slot: SlotRef) -> bool {
        self.generations.get(slot.slot) == Some(&slot.generation)
            && self.sessions.get(slot.slot).is_some_and(|s| s.is_some())
    }

    fn drain_control(&mut self) {
        while let Ok(control) = self.control_rx.try_recv() {
            match control {
                Control::Register { session, ack } => {
                    let result = self.register(*session).map(|_| ());
                    let _ = ack.send(result);
                }
                Control::Wake(slot) => {
                    let mut needs_flush = false;
                    let ended = self.with_session_ref(slot, |session, registry| {
                        needs_flush = true;
                        if !session.shared.running.load(Ordering::Relaxed) {
                            match session.drain_after_stop(registry) {
                                ReadOutcome::Ended => return Some(Ok(())),
                                ReadOutcome::Continue => {}
                            }
                        } else if let Err(error) = session.drain_pending_input(registry) {
                            log::warn!("{}: {error}", session.session_id());
                        }
                        None
                    });
                    if let Some(outcome) = ended {
                        self.end_session(slot.slot, outcome);
                    } else if needs_flush {
                        self.with_session_ref(slot, |session, registry| {
                            session.flush_stream_clients(registry);
                            None
                        });
                    }
                }
                Control::Input {
                    slot,
                    data,
                    write_id,
                    ack,
                } => {
                    let mut result = Err("Write error: session is no longer hosted".to_string());
                    self.with_session_ref(slot, |session, registry| {
                        result = session.submit_input(registry, data, write_id.as_deref());
                        None
                    });
                    let _ = ack.send(result);
                }
                Control::JournalDrained(slot) => {
                    self.with_session_ref(slot, |session, registry| {
                        session.journal_drained(registry);
                        None
                    });
                }
                Control::JournalFailed(slot) => {
                    if self.slot_is_current(slot) {
                        self.end_session(slot.slot, Err("Failed to write output log".into()));
                    }
                }
                Control::SessionEnded(slot) => {
                    self.release_pending = true;
                    self.release_slot(slot);
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

    /// A teardown reported done: the slot it used may host again. Only the
    /// generation that owned the teardown frees it; a stale report (or one
    /// for a slot that already has a new occupant) frees nothing.
    fn release_slot(&mut self, slot: SlotRef) {
        if self.generations.get(slot.slot) != Some(&slot.generation) {
            return;
        }
        if self.sessions.get(slot.slot).is_some_and(|s| s.is_some()) {
            return;
        }
        if !self.free_slots.contains(&slot.slot) {
            self.free_slots.push(slot.slot);
        }
    }

    /// Host a Session. Returns its identity once its fds are registered,
    /// its timer jobs installed, and (where the platform can) its child
    /// watched for exit.
    fn register(&mut self, mut session: SessionIo) -> Result<SlotRef, String> {
        let slot = match self.free_slots.pop() {
            Some(slot) => slot,
            None => {
                self.sessions.push(None);
                self.generations.push(0);
                self.sessions.len() - 1
            }
        };
        let generation = self.generations[slot] + 1;
        self.generations[slot] = generation;
        let slot_ref = SlotRef { slot, generation };
        let jobs = session.take_timer_jobs();
        if let Err(error) = session.register(&mut self.registry, slot_ref) {
            self.free_slots.push(slot);
            return Err(error);
        }
        let _ = self.timer_tx.send(TimerMsg::Add {
            slot: slot_ref,
            jobs,
        });
        if let Some((pid, exit_slot)) = session.child_watch() {
            match self.registry.watch_process(pid, slot) {
                Ok(Some(token)) => {
                    session.set_child_watch_token(token);
                    self.process_watches.insert(
                        token,
                        ProcessWatch {
                            slot: slot_ref,
                            pid,
                            exit_slot,
                        },
                    );
                }
                Ok(None) => {}
                Err(error) => log::warn!("{}: {error}", session.session_id()),
            }
        }
        self.sessions[slot] = Some(Box::new(session));
        Ok(slot_ref)
    }

    fn end_session(&mut self, slot: usize, outcome: Result<(), String>) {
        let Some(slot_ref) = self.sessions.get_mut(slot) else {
            return;
        };
        let Some(session) = slot_ref.take() else {
            return;
        };
        let identity = SlotRef {
            slot,
            generation: self.generations[slot],
        };
        if let Some(token) = session.child_watch_token() {
            if let Some(watch) = self.process_watches.remove(&token) {
                self.registry.unwatch_process(watch.pid, token);
            }
        }
        let registry = &mut self.registry;
        let teardown: Option<SessionTeardown> =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.finish(registry)))
                .ok();
        let Some(teardown) = teardown else {
            crate::hook_assets::append_trace_log_line(&format!(
                "pty-core session slot {slot} panicked while finishing"
            ));
            // No teardown thread will report for this slot.
            self.free_slots.push(slot);
            return;
        };
        let timer_tx = self.timer_tx.clone();
        // The epilogue waits on the writer and timer acks, reaps the child,
        // and takes the manifest lock; keep that off the reactor so no
        // other Session notices a neighbour leaving. The slot stays out of
        // `free_slots` until that thread reports `SessionEnded`.
        if thread::Builder::new()
            .name("session-exit".into())
            .spawn(move || teardown.run(&timer_tx, outcome))
            .is_err()
        {
            log::error!("failed to spawn session teardown thread");
            self.free_slots.push(identity.slot);
        }
    }

    fn adopt(
        &mut self,
        mut session: SessionIo,
        child_pid: Option<u32>,
        exit_slot: Arc<Mutex<Option<portable_pty::ExitStatus>>>,
    ) -> Result<(), String> {
        if let Some(pid) = child_pid {
            session.set_child_watch(pid, exit_slot);
        }
        self.register(session).map(|_| ())
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
                slot: session.slot_ref(),
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
                    // Forgetting is synchronous (no teardown thread will
                    // report), so the slot is free right away.
                    self.free_slots.push(slot);
                    if let Some(token) = session.child_watch_token() {
                        if let Some(watch) = self.process_watches.remove(&token) {
                            self.registry.unwatch_process(watch.pid, token);
                        }
                    }
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
                let _ = self.timer_tx.send(TimerMsg::Add {
                    slot: session.slot_ref(),
                    jobs,
                });
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
                } else if let Err(error) = session.drain_pending_input(registry) {
                    // Input deferred behind an agent restart (see
                    // `drain_pending_input`) is retried here at the latest.
                    log::warn!("{}: {error}", session.session_id());
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
                let slot = session.pressure.slot_ref();
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
                    failed.push(session.pressure.slot_ref());
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
    // Keyed by slot, tagged with the generation that installed the jobs:
    // a `Remove` from an older generation (a teardown that lost the race
    // with the slot's next occupant) retires nothing and is still acked.
    let mut jobs: HashMap<usize, (u64, Vec<HostTimerJob>)> = HashMap::new();
    let running = AtomicBool::new(true);
    loop {
        let now = Instant::now();
        let mut next_wake = now + HOST_TIMER_MAX_SLEEP;
        for (_, (_, session_jobs)) in jobs.iter_mut() {
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
                    let entry = jobs
                        .entry(slot.slot)
                        .or_insert((slot.generation, Vec::new()));
                    if entry.0 != slot.generation {
                        // A newer occupant: whatever the previous one left
                        // is retired here, whether or not its Remove came.
                        *entry = (slot.generation, Vec::new());
                    }
                    entry.1.extend(new);
                }
                TimerMsg::Remove { slot, ack } => {
                    if jobs
                        .get(&slot.slot)
                        .is_some_and(|(generation, _)| *generation == slot.generation)
                    {
                        jobs.remove(&slot.slot);
                    }
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

    // ───────────── isolation, slot identity, reaping (real PTYs) ─────────────

    use super::super::session_io::test_support::{
        child_is_reaped, pty_is_raw, short_dir, spawn_session, wait_until, TestSession,
    };
    use super::super::session_io::{reap_hosted_child, PENDING_PTY_INPUT_MAX_BYTES};
    use super::super::{process_start_time_ms, recorded_pid_identity, HostTimerJob, PidIdentity};
    use std::sync::atomic::AtomicUsize;

    /// The three core threads as `start_services` wires them, with the
    /// reactor itself kept on the test thread and driven by hand.
    struct TestCore {
        reactor: Reactor,
        handle: ReactorHandle,
        journal_tx: mpsc::Sender<JournalMsg>,
        timer_tx: mpsc::Sender<TimerMsg>,
    }

    fn test_core() -> TestCore {
        let (journal_tx, journal_rx) = mpsc::channel::<JournalMsg>();
        let (timer_tx, timer_rx) = mpsc::channel::<TimerMsg>();
        let (control_tx, control_rx) = mpsc::channel::<Control>();
        let waker = Arc::new(Waker::new().unwrap());
        let handle = ReactorHandle {
            control_tx,
            waker: Arc::clone(&waker),
        };
        let registry = Registry::new().unwrap();
        registry
            .poller
            .set(waker.read_fd, WAKER_TOKEN, true, false)
            .unwrap();
        let writer_handle = handle.clone();
        thread::spawn(move || run_journal_writer(journal_rx, writer_handle));
        thread::spawn(move || run_timer(timer_rx));
        let reactor = Reactor::new(registry, waker, control_rx, timer_tx.clone());
        TestCore {
            reactor,
            handle,
            journal_tx,
            timer_tx,
        }
    }

    fn ticking_job(ticks: &Arc<AtomicUsize>) -> HostTimerJob {
        let ticks = Arc::clone(ticks);
        HostTimerJob::new(Duration::ZERO, Duration::from_millis(5), move || {
            ticks.fetch_add(1, Ordering::Relaxed);
            true
        })
    }

    fn ticks_advance(ticks: &Arc<AtomicUsize>) -> bool {
        let before = ticks.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(80));
        ticks.load(Ordering::Relaxed) > before
    }

    /// Process control messages until `done`, or fail after `timeout`.
    fn pump(
        reactor: &mut Reactor,
        what: &str,
        timeout: Duration,
        mut done: impl FnMut(&Reactor) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            reactor.drain_control();
            if done(reactor) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn spawn_sleeper(
        core: &TestCore,
        dir: &std::path::Path,
        id: &str,
        ticks: &Arc<AtomicUsize>,
    ) -> TestSession {
        spawn_session(
            dir,
            id,
            &["/bin/sleep", "300"],
            core.handle.clone(),
            core.journal_tx.clone(),
            vec![ticking_job(ticks)],
        )
    }

    #[test]
    fn timer_retires_only_the_generation_that_installed_the_jobs() {
        let (timer_tx, timer_rx) = mpsc::channel::<TimerMsg>();
        thread::spawn(move || run_timer(timer_rx));
        let ticks = Arc::new(AtomicUsize::new(0));
        let previous = SlotRef {
            slot: 0,
            generation: 1,
        };
        let current = SlotRef {
            slot: 0,
            generation: 2,
        };
        timer_tx
            .send(TimerMsg::Add {
                slot: current,
                jobs: vec![ticking_job(&ticks)],
            })
            .unwrap();
        wait_until("the job to run", Duration::from_secs(2), || {
            ticks.load(Ordering::Relaxed) > 0
        });

        // A late Remove from the slot's previous occupant is acked (its
        // teardown must not wait) and retires nothing.
        let (ack_tx, ack_rx) = mpsc::channel();
        timer_tx
            .send(TimerMsg::Remove {
                slot: previous,
                ack: ack_tx,
            })
            .unwrap();
        ack_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("a stale Remove is acked");
        assert!(
            ticks_advance(&ticks),
            "the current occupant's jobs survive the previous occupant's Remove"
        );

        // The occupant's own Remove retires them.
        let (ack_tx, ack_rx) = mpsc::channel();
        timer_tx
            .send(TimerMsg::Remove {
                slot: current,
                ack: ack_tx,
            })
            .unwrap();
        ack_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!ticks_advance(&ticks), "retired jobs stop running");

        // An Add for a newer generation replaces whatever an older one left
        // in the slot, whether or not its Remove ever came.
        timer_tx
            .send(TimerMsg::Add {
                slot: current,
                jobs: vec![ticking_job(&ticks)],
            })
            .unwrap();
        wait_until("generation 2 to run again", Duration::from_secs(2), || {
            ticks_advance(&ticks)
        });
        let newer = SlotRef {
            slot: 0,
            generation: 3,
        };
        let other = Arc::new(AtomicUsize::new(0));
        timer_tx
            .send(TimerMsg::Add {
                slot: newer,
                jobs: vec![ticking_job(&other)],
            })
            .unwrap();
        wait_until("generation 3 to run", Duration::from_secs(2), || {
            other.load(Ordering::Relaxed) > 0
        });
        assert!(
            !ticks_advance(&ticks),
            "generation 2's jobs were replaced by generation 3's"
        );
    }

    /// Register N, end them, watch that their slots are not handed out
    /// until each teardown reports done, register N more into exactly
    /// those slots, then replay every late message the old occupants'
    /// threads could still send: the new occupants keep their jobs and
    /// stay hosted, and the old children were reaped.
    #[test]
    fn freed_slot_waits_for_the_teardown_and_stale_messages_never_reach_the_next_occupant() {
        let dir = short_dir("slots");
        let mut core = test_core();
        const N: usize = 3;

        let old_ticks = Arc::new(AtomicUsize::new(0));
        let mut old: Vec<(SlotRef, TestSession)> = Vec::new();
        for index in 0..N {
            let mut session = spawn_sleeper(&core, &dir, &format!("old{index}"), &old_ticks);
            let slot = core.reactor.register(session.session_take()).unwrap();
            assert_eq!(
                slot,
                SlotRef {
                    slot: index,
                    generation: 1
                }
            );
            old.push((slot, session));
        }
        wait_until(
            "the old occupants' jobs to run",
            Duration::from_secs(2),
            || old_ticks.load(Ordering::Relaxed) > 0,
        );

        for (slot, _) in &old {
            core.reactor.end_session(slot.slot, Ok(()));
        }
        assert!(
            core.reactor.free_slots.is_empty(),
            "a slot is not free while its teardown thread may still send for it"
        );
        // A Session registered meanwhile gets a slot of its own, never a
        // retiring one.
        let meanwhile_ticks = Arc::new(AtomicUsize::new(0));
        let mut meanwhile = spawn_sleeper(&core, &dir, "meanwhile", &meanwhile_ticks);
        let meanwhile_slot = core.reactor.register(meanwhile.session_take()).unwrap();
        assert_eq!(
            meanwhile_slot,
            SlotRef {
                slot: N,
                generation: 1
            }
        );

        for (_, session) in &old {
            assert_eq!(
                session
                    .exited
                    .recv_timeout(Duration::from_secs(20))
                    .unwrap(),
                Ok(())
            );
        }
        pump(
            &mut core.reactor,
            "the teardowns to report",
            Duration::from_secs(10),
            |r| r.free_slots.len() == N,
        );
        for (_, session) in &old {
            assert!(
                child_is_reaped(session.child_pid),
                "the old occupant's child {} was reaped by its teardown",
                session.child_pid
            );
        }
        assert!(
            !ticks_advance(&old_ticks),
            "the old occupants' jobs are retired"
        );

        let new_ticks = Arc::new(AtomicUsize::new(0));
        let mut new: Vec<(SlotRef, TestSession)> = Vec::new();
        for index in 0..N {
            let mut session = spawn_sleeper(&core, &dir, &format!("new{index}"), &new_ticks);
            let slot = core.reactor.register(session.session_take()).unwrap();
            assert!(slot.slot < N, "the freed slots are reused: {slot:?}");
            assert_eq!(slot.generation, 2);
            new.push((slot, session));
        }
        wait_until(
            "the new occupants' jobs to run",
            Duration::from_secs(2),
            || new_ticks.load(Ordering::Relaxed) > 0,
        );

        // Everything an old occupant's threads could still say, late.
        for (slot, _) in &old {
            let stale = *slot;
            core.handle.send(Control::JournalFailed(stale));
            core.handle.send(Control::JournalDrained(stale));
            core.handle.send(Control::Wake(stale));
            core.handle.send(Control::SessionEnded(stale));
            let (ack_tx, ack_rx) = mpsc::channel();
            core.timer_tx
                .send(TimerMsg::Remove {
                    slot: stale,
                    ack: ack_tx,
                })
                .unwrap();
            ack_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        core.reactor.drain_control();

        for (slot, session) in &new {
            assert!(
                core.reactor.sessions[slot.slot].is_some(),
                "{slot:?} is still hosted after the previous occupant's late messages"
            );
            assert_eq!(core.reactor.generations[slot.slot], 2);
            assert_eq!(
                recorded_pid_identity(session.child_pid, session.child_started_at),
                PidIdentity::Matches,
                "the new occupant's child is alive"
            );
        }
        assert!(
            !core.reactor.free_slots.iter().any(|s| *s < N),
            "a stale SessionEnded frees nothing"
        );
        assert!(
            ticks_advance(&new_ticks),
            "the new occupants' timer jobs survive a stale Remove"
        );

        // Cleanup: end everything and wait for the reaps.
        for (slot, _) in &new {
            core.reactor.end_session(slot.slot, Ok(()));
        }
        core.reactor.end_session(meanwhile_slot.slot, Ok(()));
        for session in new
            .iter()
            .map(|(_, s)| s)
            .chain(std::iter::once(&meanwhile))
        {
            session
                .exited
                .recv_timeout(Duration::from_secs(20))
                .unwrap()
                .unwrap();
            wait_until("the child to be reaped", Duration::from_secs(10), || {
                child_is_reaped(session.child_pid)
            });
        }
        pump(
            &mut core.reactor,
            "every teardown to report",
            Duration::from_secs(10),
            |r| r.free_slots.len() == N + 1,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reap_hosted_child_terminates_and_waits_on_a_live_child() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        let spawn = |command: &[&str]| {
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .unwrap();
            let mut cmd = CommandBuilder::new(command[0]);
            cmd.args(&command[1..]);
            let child = pair.slave.spawn_command(cmd).unwrap();
            drop(pair.slave);
            (pair.master, child)
        };

        // Alive: asked to leave, waited on, gone from the process table.
        let (_master, mut child) = spawn(&["/bin/sleep", "300"]);
        let pid = child.process_id().unwrap();
        let started = process_start_time_ms(pid);
        let began = Instant::now();
        let status = reap_hosted_child(child.as_mut(), started).expect("a status");
        assert!(!status.success(), "a signalled child is not a success");
        assert!(began.elapsed() < Duration::from_secs(3));
        assert!(child_is_reaped(pid));
        assert_eq!(
            recorded_pid_identity(pid, started),
            PidIdentity::NotOurs,
            "the pid is gone"
        );

        // Already exited: waited on right away with its real code.
        let (_master, mut child) = spawn(&["/bin/sh", "-c", "exit 7"]);
        let pid = child.process_id().unwrap();
        let started = process_start_time_ms(pid);
        thread::sleep(Duration::from_millis(500));
        let status = reap_hosted_child(child.as_mut(), started).expect("a status");
        assert_eq!(status.exit_code(), 7);
        assert!(child_is_reaped(pid));
    }

    /// A terminal whose child never reads its input fills its own queue,
    /// is told so at once, and is never allowed to block the thread that
    /// serves every other Session.
    #[test]
    fn input_queue_is_bounded_per_session_and_never_blocks() {
        let dir = short_dir("queue");
        let core = test_core();
        let mut registry = Registry::new().unwrap();
        let mut raw = spawn_session(
            &dir,
            "raw",
            &["/bin/sh", "-c", "stty raw -echo; exec /bin/sleep 300"],
            core.handle.clone(),
            core.journal_tx.clone(),
            Vec::new(),
        );
        let master_fd = raw.session_mut().pty_fd_for_test();
        wait_until(
            "the slave to enter raw mode",
            Duration::from_secs(5),
            || pty_is_raw(master_fd),
        );
        let slot = SlotRef {
            slot: 0,
            generation: 1,
        };
        raw.session_mut().register(&mut registry, slot).unwrap();

        assert_eq!(
            raw.session_mut()
                .submit_input(&mut registry, b"once".to_vec(), Some("w-1")),
            Ok(true)
        );
        assert_eq!(
            raw.session_mut()
                .submit_input(&mut registry, b"once".to_vec(), Some("w-1")),
            Ok(false),
            "a retried write id is a duplicate"
        );
        assert_eq!(
            raw.session_mut().write_id_snapshot(),
            vec!["w-1".to_string()]
        );

        let began = Instant::now();
        let chunk = vec![b'x'; 64 * 1024];
        let mut refused = None;
        for _ in 0..64 {
            match raw
                .session_mut()
                .submit_input(&mut registry, chunk.clone(), None)
            {
                Ok(true) => {}
                Ok(false) => unreachable!("no write id"),
                Err(error) => {
                    refused = Some(error);
                    break;
                }
            }
        }
        let refused = refused.expect("a terminal that never reads fills its queue");
        assert!(refused.contains("not accepting input"), "{refused}");
        assert!(raw.session_mut().pending_input_len() <= PENDING_PTY_INPUT_MAX_BYTES);
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "the refusal was immediate, not a blocked write ({:?})",
            began.elapsed()
        );
        let late = raw
            .session_mut()
            .submit_input(&mut registry, chunk.clone(), Some("w-2"));
        assert!(late.is_err(), "{late:?}");
        assert_eq!(
            raw.session_mut().write_id_snapshot(),
            vec!["w-1".to_string()],
            "a refused write id is not recorded, so it stays retryable"
        );
        // The bound is on bytes, not a latch: a keystroke still fits.
        assert_eq!(
            raw.session_mut()
                .submit_input(&mut registry, b"k".to_vec(), Some("w-3")),
            Ok(true)
        );

        let _ = raw.shared.runtime.lock().unwrap().child.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The child exits but a grandchild keeps the slave open, so the PTY
    /// never reaches EOF. The reactor's process watch ends the Session
    /// anyway and the teardown reaps the child.
    #[cfg(target_os = "macos")]
    #[test]
    fn child_exit_is_observed_without_pty_eof_and_the_child_is_reaped() {
        let dir = short_dir("watch");
        let mut core = test_core();
        let mut session = spawn_session(
            &dir,
            "bg",
            // The grandchild ignores the HUP the master's close sends at
            // teardown, so its survival proves the Session ended on the
            // child's exit and not on PTY EOF.
            &[
                "/bin/sh",
                "-c",
                "trap '' HUP; sleep 60 & echo grandchild=$!; sleep 0.3; exit 3",
            ],
            core.handle.clone(),
            core.journal_tx.clone(),
            Vec::new(),
        );
        let slot = core.reactor.register(session.session_take()).unwrap();
        assert!(
            core.reactor
                .process_watches
                .values()
                .any(|watch| watch.slot == slot && watch.pid == session.child_pid),
            "an owned child is watched for exit at registration"
        );

        let mut last_tick = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(10);
        while core.reactor.sessions[slot.slot].is_some() {
            assert!(
                Instant::now() < deadline,
                "the Session did not end when its child exited"
            );
            core.reactor
                .run_once(Duration::from_millis(50), &mut last_tick);
        }
        assert_eq!(
            session
                .exited
                .recv_timeout(Duration::from_secs(20))
                .unwrap(),
            Ok(())
        );
        pump(
            &mut core.reactor,
            "the slot to be released",
            Duration::from_secs(10),
            |r| r.free_slots.contains(&slot.slot),
        );
        assert!(child_is_reaped(session.child_pid), "no zombie child");

        // The grandchild still holds the slave: EOF is not what ended it.
        let journal = std::fs::read(&session.output_path).unwrap();
        let text = String::from_utf8_lossy(&journal);
        let grandchild: u32 = text
            .split("grandchild=")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|digits| digits.parse().ok())
            .expect("the shell printed its background child's pid");
        assert!(
            unsafe { libc::kill(grandchild as libc::pid_t, 0) } == 0,
            "the grandchild is still running"
        );
        let _ = unsafe { libc::kill(grandchild as libc::pid_t, libc::SIGKILL) };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
