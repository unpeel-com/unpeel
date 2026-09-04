//! Per-session I/O state machine for the event-driven core.
//!
//! After `run_host` has finished a Session's setup (launch prep, hook
//! install, manifests, provider setup, PTY spawn) it hands a [`SessionIo`] to
//! the shared reactor (`core_reactor`). From then on the Session owns no
//! thread of its own: the reactor drives its PTY reads, its control-socket
//! accepts, and its long-lived attach clients (`StreamOutput` /
//! `StreamInput`) from one loop; periodic jobs run on the core-wide timer
//! thread and journal writes on the core-wide writer thread. One-shot
//! control commands (Write, Resize, Ping, Kill, Resume…) keep the released
//! blocking handler and run on a transient thread per request, exactly as
//! before, so the protocol on `session.sock` is byte-identical.
//!
//! This is a child module of `session_host` so it can reach the private
//! scanners, broadcaster, and manifest helpers without widening their
//! visibility.

use super::core_reactor::{JournalMsg, ReactorHandle, Registry, TimerMsg};
use super::*;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};

/// Bytes of journal output a Session may have in flight to the core-wide
/// writer before its PTY reads pause. Mirrors the old per-session
/// `sync_channel(64)` of ≤ 64 KiB chunks: bounded memory, never unbounded
/// growth behind a slow disk. Reads resume below the low mark.
pub(crate) const JOURNAL_BACKLOG_HIGH_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const JOURNAL_BACKLOG_LOW_BYTES: usize = 1024 * 1024;
/// Largest JSON handshake line a control-socket client may send before the
/// core stops buffering and drops it.
const CLIENT_HANDSHAKE_MAX_BYTES: usize = 64 * 1024;
/// Input frames wait here while the PTY's input queue is full (a stopped or
/// non-reading foreground). Beyond this the input client is dropped rather
/// than letting one wedged terminal grow the core without bound.
const PENDING_PTY_INPUT_MAX_BYTES: usize = 1024 * 1024;

/// Everything a control-socket command handler needs, previously threaded
/// through `handle_client` as a dozen `Arc` parameters.
pub(crate) struct SessionShared {
    pub session_id: String,
    pub runtime: Arc<Mutex<HostRuntime>>,
    pub viewport: Arc<Mutex<TerminalViewportState>>,
    pub running: Arc<AtomicBool>,
    pub broadcaster: Arc<Mutex<OutputBroadcaster>>,
    pub title_buffer: Arc<Mutex<String>>,
    pub title_done: Arc<AtomicBool>,
    pub has_been_written_to: Arc<AtomicBool>,
    pub agent_restart_lock: Arc<Mutex<()>>,
    pub runtime_generation: Arc<AtomicU64>,
    pub pending_runtime_generation: Arc<AtomicU64>,
    /// Set by the reactor at registration; lets a handler running on a
    /// transient thread (Kill) poke the reactor about this Session.
    pub reactor: ReactorHandle,
    pub slot: AtomicUsize,
}

impl SessionShared {
    /// Ask the reactor to revisit this Session (after `running` flipped).
    pub(crate) fn wake(&self) {
        self.reactor.wake_session(self.slot.load(Ordering::Acquire));
    }
}

/// The PTY master writer. Every write path (client Write, StreamInput,
/// query answers, resume-in-place) goes through `HostRuntime.writer`; the
/// master fd is non-blocking so the reactor can never stall on it, and this
/// wrapper restores blocking `write_all` semantics for the transient-thread
/// paths by waiting for `POLLOUT` between attempts.
pub(crate) struct PtyWriter {
    inner: Box<dyn Write + Send>,
    fd: RawFd,
}

impl PtyWriter {
    pub(crate) fn new(inner: Box<dyn Write + Send>, fd: RawFd) -> Self {
        Self { inner, fd }
    }

    /// One non-blocking attempt; the reactor queues the remainder.
    pub(crate) fn try_write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.inner.write(data)
    }

    /// Detach the underlying writer without running its destructor.
    /// portable-pty's master writer sends `\n` + VEOF to the PTY when it is
    /// dropped (so a hosted shell sees EOF at teardown); after a handoff the
    /// PTY belongs to the new core and must NOT receive that EOF from us.
    /// The fd leaks in this process, which exits right after the handoff.
    pub(crate) fn disarm(&mut self) {
        let inner = std::mem::replace(&mut self.inner, Box::new(std::io::sink()));
        std::mem::forget(inner);
    }

    fn wait_writable(&self, timeout_ms: i32) {
        let mut descriptor = libc::pollfd {
            fd: self.fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let _ = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    }
}

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            match self.inner.write(buf) {
                Err(error) if error.kind() == ErrorKind::WouldBlock => self.wait_writable(250),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(crate) fn set_nonblocking(fd: RawFd, nonblocking: bool) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// What the teardown needs once the PTY has ended: the final manifest write
/// merges the exit edge exactly as the old in-thread epilogue did.
pub(crate) struct SessionExitPlan {
    pub pid: Option<u32>,
    pub pid_started_at: Option<u64>,
    pub host_build_id: Option<String>,
    pub session_socket_path: PathBuf,
    pub on_exit: Box<dyn FnOnce(Result<(), String>) + Send>,
}

/// Shared with the journal writer thread: bytes handed over but not yet
/// written, and whether the reactor paused this Session's PTY reads.
#[derive(Default)]
pub(crate) struct JournalBackpressure {
    pub backlog: AtomicUsize,
    pub paused: AtomicBool,
    /// Reactor slot, set at registration so the writer can address the
    /// Session in `Control` messages.
    pub slot: AtomicUsize,
}

static NEXT_JOURNAL_ID: AtomicU64 = AtomicU64::new(1);

/// Journal streams are keyed independently of reactor slots because the
/// writer is opened before the Session is registered.
pub(crate) fn next_journal_id() -> u64 {
    NEXT_JOURNAL_ID.fetch_add(1, Ordering::Relaxed)
}

pub(crate) struct JournalHandle {
    pub id: u64,
    pub tx: mpsc::Sender<JournalMsg>,
    pub pressure: Arc<JournalBackpressure>,
    /// The writer thread reported a write failure; the Session ends like the
    /// old reader loop did when its writer channel closed.
    pub failed: Arc<AtomicBool>,
}

enum ClientState {
    Handshake {
        buf: Vec<u8>,
    },
    StreamOut {
        rx: mpsc::Receiver<SessionOutputChunk>,
        buffered: Arc<AtomicUsize>,
        subscriber_id: u64,
        pending: Option<SessionOutputChunk>,
        /// The exited frame is queued; close once it has been written.
        closing: bool,
    },
    StreamIn {
        buf: Vec<u8>,
    },
}

struct Client {
    stream: UnixStream,
    token: u64,
    state: ClientState,
    outbuf: Vec<u8>,
    out_pos: usize,
    write_interest: bool,
    last_progress: Instant,
}

pub(crate) enum ReadOutcome {
    Continue,
    /// PTY reached EOF/error or the Session was stopped and drained.
    Ended,
}

pub(crate) struct SessionIo {
    pub shared: Arc<SessionShared>,
    pty_fd: RawFd,
    pty_reader: Box<dyn Read + Send>,
    pty_token: u64,
    pty_read_interest: bool,
    pty_write_interest: bool,
    listener: Option<UnixListener>,
    listener_token: u64,
    clients: HashMap<u64, Client>,
    query_scanner: OutputQueryScanner,
    title_scanner: OscTitleScanner,
    agent_title_settled: bool,
    dark_mode: bool,
    journal: JournalHandle,
    pending_input: VecDeque<u8>,
    timer_jobs: Option<Vec<HostTimerJob>>,
    exit: Option<SessionExitPlan>,
    ended: bool,
    /// What rebuilding this Session's timer jobs needs (resume after a
    /// failed handoff, and the metadata a handoff carries).
    job_seed: JobSeed,
    /// Clients received from a previous core, attached at `register`.
    adopted_clients: Vec<(UnixStream, ClientHandoff)>,
    /// Set while the old core has paused this Session for a handoff.
    handing_off: bool,
    /// Last PTY output; idle buffers are released a tick after this.
    last_output_at: Instant,
}

/// A Session with no PTY output for this long releases its recent ring,
/// input queue, and drained client outboxes (all re-grown on demand).
const IDLE_RELEASE_AFTER: Duration = Duration::from_secs(1);

/// The per-Session inputs of `build_session_timer_jobs` that are not
/// already in `SessionShared`.
#[derive(Clone)]
pub(crate) struct JobSeed {
    pub command: String,
    pub shell: String,
    pub pid: Option<u32>,
    pub pid_started_at: Option<u64>,
}

impl SessionIo {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        shared: Arc<SessionShared>,
        pty_fd: RawFd,
        pty_reader: Box<dyn Read + Send>,
        listener: UnixListener,
        dark_mode: bool,
        journal: JournalHandle,
        timer_jobs: Vec<HostTimerJob>,
        exit: SessionExitPlan,
        job_seed: JobSeed,
    ) -> Self {
        Self {
            shared,
            pty_fd,
            pty_reader,
            pty_token: 0,
            pty_read_interest: false,
            pty_write_interest: false,
            listener: Some(listener),
            listener_token: 0,
            clients: HashMap::new(),
            query_scanner: OutputQueryScanner::new(),
            title_scanner: OscTitleScanner::new(),
            agent_title_settled: false,
            dark_mode,
            journal,
            pending_input: VecDeque::new(),
            timer_jobs: Some(timer_jobs),
            exit: Some(exit),
            ended: false,
            job_seed,
            adopted_clients: Vec::new(),
            handing_off: false,
            last_output_at: Instant::now(),
        }
    }

    /// Idle-tick diet: a quiet Session keeps only its state, no buffers. The
    /// broadcaster's recent ring exists for late subscribers (an attach that
    /// lags the in-memory stream by the journal's flush interval); after a
    /// quiet second it goes, and a subscriber it would have served falls
    /// back to the journal exactly as one behind the ring does today.
    pub(crate) fn release_idle_buffers(&mut self) {
        if self.ended || self.last_output_at.elapsed() < IDLE_RELEASE_AFTER {
            return;
        }
        if let Ok(mut broadcaster) = self.shared.broadcaster.lock() {
            if !broadcaster.has_subscribers() {
                broadcaster.release_recent();
            }
        }
        if self.pending_input.is_empty() && self.pending_input.capacity() > 0 {
            self.pending_input = VecDeque::new();
        }
        for client in self.clients.values_mut() {
            if client.outbuf.is_empty() && client.outbuf.capacity() > 0 {
                client.outbuf = Vec::new();
            }
        }
    }

    /// Clients moved over from a previous core; attached when this Session
    /// registers with the reactor.
    pub(crate) fn set_adopted_clients(&mut self, clients: Vec<(UnixStream, ClientHandoff)>) {
        self.adopted_clients = clients;
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.shared.session_id
    }

    pub(crate) fn journal_tx(&self) -> &mpsc::Sender<JournalMsg> {
        &self.journal.tx
    }

    pub(crate) fn journal_id(&self) -> u64 {
        self.journal.id
    }

    pub(crate) fn take_timer_jobs(&mut self) -> Vec<HostTimerJob> {
        self.timer_jobs.take().unwrap_or_default()
    }

    // ───────────────────────── registration ─────────────────────────

    /// Register the PTY master and the listener with the reactor's poller.
    pub(crate) fn register(&mut self, registry: &mut Registry, slot: usize) -> Result<(), String> {
        self.shared.slot.store(slot, Ordering::Release);
        self.journal.pressure.slot.store(slot, Ordering::Release);
        self.pty_token = registry.add(self.pty_fd, slot, TokenKind::Pty, true, false)?;
        self.pty_read_interest = true;
        if let Some(listener) = &self.listener {
            self.listener_token =
                registry.add(listener.as_raw_fd(), slot, TokenKind::Listener, true, false)?;
        }
        let adopted = std::mem::take(&mut self.adopted_clients);
        for (stream, handoff) in adopted {
            self.adopt_client(registry, slot, stream, handoff);
        }
        Ok(())
    }

    /// Re-create a client the previous core handed over: same socket, same
    /// stream position, its unsent bytes queued first.
    fn adopt_client(
        &mut self,
        registry: &mut Registry,
        slot: usize,
        stream: UnixStream,
        handoff: ClientHandoff,
    ) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let state = match handoff.kind {
            ClientKind::StreamOut => {
                let (tx, rx) = mpsc::channel();
                let Some((subscriber_id, buffered)) = self
                    .shared
                    .broadcaster
                    .lock()
                    .unwrap()
                    .subscribe(handoff.offset, tx, handoff.answers_queries)
                else {
                    // Cannot happen right after a takeover (the broadcaster
                    // starts exactly at the handed-over offset); drop safely.
                    return;
                };
                ClientState::StreamOut {
                    rx,
                    buffered,
                    subscriber_id,
                    pending: None,
                    closing: false,
                }
            }
            ClientKind::StreamIn => ClientState::StreamIn { buf: handoff.inbuf },
            ClientKind::Handshake => ClientState::Handshake { buf: handoff.inbuf },
        };
        let want_write = !handoff.outbuf.is_empty();
        let Ok(token) = registry.add(
            stream.as_raw_fd(),
            slot,
            TokenKind::Client,
            true,
            want_write,
        ) else {
            return;
        };
        self.clients.insert(
            token,
            Client {
                stream,
                token,
                state,
                outbuf: handoff.outbuf,
                out_pos: 0,
                write_interest: want_write,
                last_progress: Instant::now(),
            },
        );
    }

    fn set_pty_interest(&mut self, registry: &mut Registry, read: bool, write: bool) {
        if self.pty_read_interest == read && self.pty_write_interest == write {
            return;
        }
        self.pty_read_interest = read;
        self.pty_write_interest = write;
        let _ = registry.modify(self.pty_fd, self.pty_token, read, write);
    }

    // ───────────────────────── PTY output ─────────────────────────

    /// One level-triggered read: at most one `SESSION_OUTPUT_READ_BUFFER_BYTES`
    /// chunk per readiness so a flooding Session cannot starve its siblings.
    pub(crate) fn pty_readable(&mut self, registry: &mut Registry) -> ReadOutcome {
        if self.ended {
            return ReadOutcome::Ended;
        }
        if self.journal.failed.load(Ordering::Acquire) {
            return ReadOutcome::Ended;
        }
        // One read buffer for the whole reactor (it never reads two PTYs at
        // once), so an idle Session holds no 64 KiB of its own.
        let mut scratch = std::mem::take(&mut registry.scratch);
        let read = self.pty_reader.read(&mut scratch);
        let outcome = match read {
            Ok(0) => ReadOutcome::Ended,
            Ok(n) => {
                self.process_pty_bytes(&scratch[..n]);
                self.apply_journal_backpressure(registry);
                ReadOutcome::Continue
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if !self.shared.running.load(Ordering::Relaxed) {
                    // Kill already terminated the child; a retained slave may
                    // never reach EOF. Everything buffered has been drained
                    // (this read found nothing), so leave like the old loop.
                    return ReadOutcome::Ended;
                }
                ReadOutcome::Continue
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => ReadOutcome::Continue,
            Err(error) => {
                crate::hook_assets::append_trace_log_line(&format!(
                    "session {} pty read error: {error} (running={})",
                    self.shared.session_id,
                    self.shared.running.load(Ordering::Relaxed)
                ));
                ReadOutcome::Ended
            }
        };
        registry.scratch = scratch;
        outcome
    }

    /// After Kill: drain whatever termination output is still buffered, then
    /// end. Called on an explicit wake once `running` is false.
    pub(crate) fn drain_after_stop(&mut self, registry: &mut Registry) -> ReadOutcome {
        for _ in 0..64 {
            match self.pty_readable(registry) {
                ReadOutcome::Ended => return ReadOutcome::Ended,
                ReadOutcome::Continue => {
                    if !self.shared.running.load(Ordering::Relaxed) {
                        continue;
                    }
                    return ReadOutcome::Continue;
                }
            }
        }
        // Still producing: let the ordinary readiness path keep draining.
        ReadOutcome::Continue
    }

    fn process_pty_bytes(&mut self, bytes: &[u8]) {
        let shared = Arc::clone(&self.shared);
        // Answer terminal queries at the host and excise them from the
        // stream: DA1 always, and the wider probe set whenever no answering
        // surface is connected. See `OutputQueryScanner`.
        let intercept_probes = !shared
            .broadcaster
            .lock()
            .unwrap()
            .has_answering_subscriber();
        let (chunk, host_queries) = self.query_scanner.scan(bytes, intercept_probes);
        if !host_queries.is_empty() {
            let cursor = shared.viewport.lock().unwrap().cursor_position();
            let mut guard = shared.runtime.lock().unwrap();
            for query in &host_queries {
                let _ = match query {
                    HostAnsweredQuery::Da1 => {
                        guard.writer.write_all(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE)
                    }
                    HostAnsweredQuery::CursorPosition => guard
                        .writer
                        .write_all(format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).as_bytes()),
                    HostAnsweredQuery::KittyFlags => guard.writer.write_all(b"\x1b[?0u"),
                    HostAnsweredQuery::OscColor { code } => {
                        let rgb = match (code, self.dark_mode) {
                            (10, true) => "f3f3/f5f5/fbfb",
                            (10, false) => "1111/1212/1717",
                            (_, true) => "1a1a/1a1a/1f1f",
                            (_, false) => "ffff/ffff/ffff",
                        };
                        guard
                            .writer
                            .write_all(format!("\x1b]{code};rgb:{rgb}\x07").as_bytes())
                    }
                    HostAnsweredQuery::OscPalette { index } => {
                        let (r, g, b) = xterm_palette_rgb(*index);
                        guard.writer.write_all(
                            format!(
                                "\x1b]4;{index};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x07"
                            )
                            .as_bytes(),
                        )
                    }
                };
            }
            let _ = guard.writer.flush();
        }
        if chunk.is_empty() {
            return;
        }
        self.publish_chunk(chunk);
    }

    /// Viewport → live stream → journal, in that order, exactly as the old
    /// reader loop: the visible terminal stays responsive even when the disk
    /// writer is briefly behind.
    fn publish_chunk(&mut self, chunk: Vec<u8>) {
        self.last_output_at = Instant::now();
        let shared = Arc::clone(&self.shared);
        shared.viewport.lock().unwrap().feed(&chunk);
        let agent_title = self.title_scanner.scan(&chunk);
        shared.broadcaster.lock().unwrap().broadcast_chunk(&chunk);
        self.journal
            .pressure
            .backlog
            .fetch_add(chunk.len(), Ordering::AcqRel);
        let id = self.journal.id;
        if self
            .journal
            .tx
            .send(JournalMsg::Chunk { id, data: chunk })
            .is_err()
        {
            self.journal.failed.store(true, Ordering::Release);
        }
        if let Some(title) = agent_title {
            if !self.agent_title_settled {
                self.agent_title_settled = apply_agent_terminal_title(&shared.session_id, &title);
            }
        }
    }

    fn apply_journal_backpressure(&mut self, registry: &mut Registry) {
        let backlog = self.journal.pressure.backlog.load(Ordering::Acquire);
        if backlog > JOURNAL_BACKLOG_HIGH_BYTES && self.pty_read_interest {
            self.journal.pressure.paused.store(true, Ordering::Release);
            let write = self.pty_write_interest;
            self.set_pty_interest(registry, false, write);
        }
    }

    /// The writer drained below the low mark: resume PTY reads.
    pub(crate) fn journal_drained(&mut self, registry: &mut Registry) {
        if !self.pty_read_interest && !self.ended {
            let write = self.pty_write_interest;
            self.set_pty_interest(registry, true, write);
        }
    }

    // ───────────────────────── PTY input ─────────────────────────

    /// Queue bytes for the PTY, writing what fits right now. Order between
    /// queued bytes and later frames is preserved by always appending.
    fn write_pty_input(&mut self, registry: &mut Registry, data: &[u8]) -> Result<(), String> {
        self.pending_input.extend(data);
        self.drain_pending_input(registry)
    }

    pub(crate) fn drain_pending_input(&mut self, registry: &mut Registry) -> Result<(), String> {
        if self.pending_input.is_empty() {
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let mut wrote_any = false;
        let blocked = {
            let _restart_guard = shared
                .agent_restart_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut guard = shared.runtime.lock().unwrap();
            let mut blocked = false;
            while !self.pending_input.is_empty() {
                let (head, _) = self.pending_input.as_slices();
                match guard.writer.try_write(head) {
                    Ok(0) => {
                        blocked = true;
                        break;
                    }
                    Ok(n) => {
                        self.pending_input.drain(..n);
                        wrote_any = true;
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        blocked = true;
                        break;
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) => return Err(format!("Write error: {error}")),
                }
            }
            blocked
        };
        if wrote_any {
            mark_input_written(&shared.session_id, &shared.has_been_written_to);
        }
        if blocked && self.pending_input.len() > PENDING_PTY_INPUT_MAX_BYTES {
            self.pending_input.clear();
            return Err("PTY input queue stalled; dropping buffered input".into());
        }
        let read = self.pty_read_interest;
        self.set_pty_interest(registry, read, blocked);
        Ok(())
    }

    // ───────────────────────── clients ─────────────────────────

    pub(crate) fn accept_clients(&mut self, registry: &mut Registry, slot: usize) {
        let Some(listener) = self.listener.as_ref() else {
            return;
        };
        for _ in 0..16 {
            match listener.accept() {
                Ok((stream, _)) => {
                    if !self.shared.running.load(Ordering::Relaxed) {
                        // Mirrors the old accept loop: no client after stop.
                        drop(stream);
                        continue;
                    }
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    let Ok(token) =
                        registry.add(stream.as_raw_fd(), slot, TokenKind::Client, true, false)
                    else {
                        continue;
                    };
                    self.clients.insert(
                        token,
                        Client {
                            stream,
                            token,
                            state: ClientState::Handshake { buf: Vec::new() },
                            outbuf: Vec::new(),
                            out_pos: 0,
                            write_interest: false,
                            last_progress: Instant::now(),
                        },
                    );
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }

    pub(crate) fn client_readable(&mut self, registry: &mut Registry, token: u64) {
        let Some(mut client) = self.clients.remove(&token) else {
            return;
        };
        let keep = match &mut client.state {
            ClientState::Handshake { .. } => self.client_handshake(registry, &mut client),
            ClientState::StreamIn { .. } => self.client_input(registry, &mut client),
            ClientState::StreamOut { .. } => {
                // The attach client never sends after the handshake; readable
                // here means EOF (detach) or garbage. Either way, drop it.
                let mut probe = [0u8; 64];
                Some(match client.stream.read(&mut probe) {
                    Ok(0) => false,
                    Ok(_) => true,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => true,
                    Err(error) if error.kind() == ErrorKind::Interrupted => true,
                    Err(_) => false,
                })
            }
        };
        match keep {
            Some(true) => {
                self.clients.insert(token, client);
            }
            Some(false) | None => self.close_client(registry, client),
        }
    }

    /// Returns `Some(true)` to keep the client, `Some(false)` to close it,
    /// `None` when it has been handed over to a transient thread.
    fn client_handshake(&mut self, registry: &mut Registry, client: &mut Client) -> Option<bool> {
        let ClientState::Handshake { buf } = &mut client.state else {
            return Some(true);
        };
        let mut chunk = [0u8; 4096];
        loop {
            match client.stream.read(&mut chunk) {
                Ok(0) => return Some(false),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.contains(&b'\n') {
                        break;
                    }
                    if buf.len() > CLIENT_HANDSHAKE_MAX_BYTES {
                        return Some(false);
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Some(true),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Some(false),
            }
        }
        let newline = buf.iter().position(|b| *b == b'\n').unwrap_or(buf.len());
        let line = String::from_utf8_lossy(&buf[..newline]).to_string();
        let rest = buf.split_off((newline + 1).min(buf.len()));
        let Ok(command) = serde_json::from_str::<SessionHostCommand>(line.trim()) else {
            return Some(false);
        };
        match command {
            SessionHostCommand::StreamOutput {
                offset,
                answers_queries,
            } => {
                let (tx, rx) = mpsc::channel();
                let Some((subscriber_id, buffered)) = self
                    .shared
                    .broadcaster
                    .lock()
                    .unwrap()
                    .subscribe(offset, tx, answers_queries)
                else {
                    // Closing without a frame is the only gap-safe answer the
                    // v1 framing supports; the client falls back to the
                    // journal and retries from a retained cursor.
                    return Some(false);
                };
                client.state = ClientState::StreamOut {
                    rx,
                    buffered,
                    subscriber_id,
                    pending: None,
                    closing: false,
                };
                client.last_progress = Instant::now();
                Some(self.pump_stream_client(registry, client))
            }
            SessionHostCommand::StreamInput => {
                client.outbuf.push(SESSION_INPUT_STREAM_ACK);
                client.state = ClientState::StreamIn { buf: rest };
                if !self.flush_client_outbuf(registry, client) {
                    return Some(false);
                }
                self.client_input(registry, client)
            }
            other => {
                // One-shot command: hand the socket to the released blocking
                // handler on its own short-lived thread. Deregister first so
                // the reactor never sees this fd again.
                registry.remove(client.stream.as_raw_fd(), client.token);
                let stream = match client.stream.try_clone() {
                    Ok(stream) => stream,
                    Err(_) => return Some(false),
                };
                let shared = Arc::clone(&self.shared);
                let _ = thread::Builder::new()
                    .name("session-cmd".into())
                    .spawn(move || {
                        let _ = dispatch_client_command(stream, other, &shared);
                    });
                None
            }
        }
    }

    fn client_input(&mut self, registry: &mut Registry, client: &mut Client) -> Option<bool> {
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let eof;
        {
            let ClientState::StreamIn { buf } = &mut client.state else {
                return Some(true);
            };
            let mut chunk = [0u8; 8192];
            let mut reached_eof = false;
            loop {
                match client.stream.read(&mut chunk) {
                    Ok(0) => {
                        reached_eof = true;
                        break;
                    }
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > SESSION_INPUT_STREAM_MAX_FRAME_BYTES * 2 {
                            break;
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => {
                        reached_eof = true;
                        break;
                    }
                }
            }
            eof = reached_eof;
            // Parse complete frames: 4-byte big-endian length + payload.
            loop {
                if buf.len() < SESSION_INPUT_STREAM_FRAME_HEADER_BYTES {
                    break;
                }
                let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                if len > SESSION_INPUT_STREAM_MAX_FRAME_BYTES {
                    return Some(false);
                }
                if buf.len() < SESSION_INPUT_STREAM_FRAME_HEADER_BYTES + len {
                    break;
                }
                let payload = buf[SESSION_INPUT_STREAM_FRAME_HEADER_BYTES
                    ..SESSION_INPUT_STREAM_FRAME_HEADER_BYTES + len]
                    .to_vec();
                buf.drain(..SESSION_INPUT_STREAM_FRAME_HEADER_BYTES + len);
                if !payload.is_empty() {
                    frames.push(payload);
                }
            }
        }
        let shared = Arc::clone(&self.shared);
        for data in frames {
            if let Err(error) = self.write_pty_input(registry, &data) {
                log::warn!(
                    "input stream write failed for {}: {error}",
                    shared.session_id
                );
                return Some(false);
            }
            maybe_auto_title_from_input(
                &shared.session_id,
                &data,
                &shared.title_buffer,
                &shared.title_done,
            );
        }
        Some(!eof)
    }

    pub(crate) fn client_writable(&mut self, registry: &mut Registry, token: u64) {
        let Some(mut client) = self.clients.remove(&token) else {
            return;
        };
        let keep = match &client.state {
            ClientState::StreamOut { .. } => self.pump_stream_client(registry, &mut client),
            _ => self.flush_client_outbuf(registry, &mut client),
        };
        if keep {
            self.clients.insert(token, client);
        } else {
            self.close_client(registry, client);
        }
    }

    /// New output was broadcast (or the Session exited): push it to every
    /// streaming client that can take it right now.
    pub(crate) fn flush_stream_clients(&mut self, registry: &mut Registry) {
        let tokens: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, client)| matches!(client.state, ClientState::StreamOut { .. }))
            .map(|(token, _)| *token)
            .collect();
        for token in tokens {
            let Some(mut client) = self.clients.remove(&token) else {
                continue;
            };
            if self.pump_stream_client(registry, &mut client) {
                self.clients.insert(token, client);
            } else {
                self.close_client(registry, client);
            }
        }
    }

    /// Pull chunks from the subscriber channel into batched frames and write
    /// as much as the socket accepts. Same merge rules and batch cap as the
    /// old forwarder thread; batching now happens per reactor pass.
    fn pump_stream_client(&mut self, registry: &mut Registry, client: &mut Client) -> bool {
        loop {
            if !self.flush_client_outbuf(registry, client) {
                return false;
            }
            if client.out_pos < client.outbuf.len() {
                // Socket is full; wait for writable.
                return true;
            }
            let ClientState::StreamOut {
                rx,
                buffered,
                pending,
                closing,
                ..
            } = &mut client.state
            else {
                return true;
            };
            if *closing {
                return false;
            }
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(chunk) => {
                        buffered.fetch_sub(
                            chunk.data.len().min(buffered.load(Ordering::Relaxed)),
                            Ordering::Relaxed,
                        );
                        if chunk.exited {
                            if let Some(current) = pending.take() {
                                client.outbuf.extend(session_output_frame_bytes(&current));
                            }
                            client.outbuf.extend(session_output_frame_bytes(&chunk));
                            *closing = true;
                            break;
                        }
                        if let Some(current) = pending.as_mut() {
                            if can_merge_output_stream_chunk(
                                current,
                                &chunk,
                                SESSION_OUTPUT_STREAM_BATCH_MAX_BYTES,
                            ) {
                                current.data.extend_from_slice(&chunk.data);
                                current.next_offset = chunk.next_offset;
                                if current.data.len() >= SESSION_OUTPUT_STREAM_BATCH_MAX_BYTES {
                                    let full = pending.take().unwrap();
                                    client.outbuf.extend(session_output_frame_bytes(&full));
                                }
                                continue;
                            }
                            let previous = pending.take().unwrap();
                            client.outbuf.extend(session_output_frame_bytes(&previous));
                        }
                        if chunk.data.len() >= SESSION_OUTPUT_STREAM_BATCH_MAX_BYTES {
                            client.outbuf.extend(session_output_frame_bytes(&chunk));
                        } else {
                            *pending = Some(chunk);
                        }
                        if client.outbuf.len() >= SESSION_OUTPUT_STREAM_BATCH_MAX_BYTES * 4 {
                            // Let the socket absorb what we have before
                            // pulling more; keeps per-client memory bounded.
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // Dropped by the broadcaster (backlog cap) or the
                        // Session exited without a frame for us.
                        disconnected = true;
                        break;
                    }
                }
            }
            if let Some(current) = pending.take() {
                client.outbuf.extend(session_output_frame_bytes(&current));
            }
            let closing_now = *closing;
            if client.out_pos == client.outbuf.len() {
                if disconnected || closing_now {
                    return false;
                }
                return true;
            }
            if disconnected {
                // Best effort: write what we have, then go.
                let _ = self.flush_client_outbuf(registry, client);
                return false;
            }
            // Loop to write the freshly framed bytes.
        }
    }

    /// Write `outbuf` as far as the socket allows. Returns `false` when the
    /// client is gone or has stalled past the write timeout.
    fn flush_client_outbuf(&mut self, registry: &mut Registry, client: &mut Client) -> bool {
        while client.out_pos < client.outbuf.len() {
            match client.stream.write(&client.outbuf[client.out_pos..]) {
                Ok(0) => return false,
                Ok(n) => {
                    client.out_pos += n;
                    client.last_progress = Instant::now();
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        if client.out_pos >= client.outbuf.len() {
            client.outbuf.clear();
            client.out_pos = 0;
        } else if client.out_pos > 0 && client.out_pos >= client.outbuf.len() / 2 {
            client.outbuf.drain(..client.out_pos);
            client.out_pos = 0;
        }
        let want_write = !client.outbuf.is_empty();
        if want_write
            && client.last_progress.elapsed()
                > Duration::from_millis(SESSION_OUTPUT_STREAM_WRITE_TIMEOUT_MS)
        {
            // Same bound the old forwarder applied via set_write_timeout: a
            // client that stops reading is dropped, never pinned.
            return false;
        }
        if want_write != client.write_interest {
            client.write_interest = want_write;
            let _ = registry.modify(client.stream.as_raw_fd(), client.token, true, want_write);
        }
        true
    }

    /// Stalled-client sweep, run from the reactor's idle tick.
    pub(crate) fn sweep_stalled_clients(&mut self, registry: &mut Registry) {
        let stalled: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, client)| {
                !client.outbuf.is_empty()
                    && client.last_progress.elapsed()
                        > Duration::from_millis(SESSION_OUTPUT_STREAM_WRITE_TIMEOUT_MS)
            })
            .map(|(token, _)| *token)
            .collect();
        for token in stalled {
            if let Some(client) = self.clients.remove(&token) {
                self.close_client(registry, client);
            }
        }
    }

    fn close_client(&mut self, registry: &mut Registry, client: Client) {
        registry.remove(client.stream.as_raw_fd(), client.token);
        if let ClientState::StreamOut { subscriber_id, .. } = &client.state {
            self.shared
                .broadcaster
                .lock()
                .unwrap()
                .remove_subscriber(*subscriber_id);
        }
        drop(client);
    }

    // ───────────────────────── end of life ─────────────────────────

    /// Detach everything from the poller and return the pieces the teardown
    /// thread needs. Mirrors the old epilogue up to (not including) the
    /// writer join: pending scanner bytes are published, the broadcaster is
    /// marked exited, and streaming clients get one last flush.
    pub(crate) fn finish(mut self, registry: &mut Registry) -> SessionTeardown {
        self.ended = true;
        let pending = self.query_scanner.take_pending();
        if !pending.is_empty() {
            self.publish_chunk(pending);
        }
        self.shared.broadcaster.lock().unwrap().mark_exited();
        self.flush_stream_clients(registry);
        registry.remove(self.pty_fd, self.pty_token);
        if let Some(listener) = self.listener.take() {
            registry.remove(listener.as_raw_fd(), self.listener_token);
            drop(listener);
        }
        let clients: Vec<Client> = self.clients.drain().map(|(_, client)| client).collect();
        for client in clients {
            self.close_client(registry, client);
        }
        let slot = self.shared.slot.load(Ordering::Acquire);
        SessionTeardown {
            shared: Arc::clone(&self.shared),
            slot,
            journal_tx: self.journal.tx.clone(),
            journal_id: self.journal.id,
            exit: self.exit.take(),
            // Dropping the reader/master dups here; the runtime keeps the
            // master for try_wait/kill until the teardown thread is done.
            _pty_reader: self.pty_reader,
        }
    }
}

/// Reactor token classification for a Session's fds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Pty,
    Listener,
    Client,
    /// A process-exit watch on a handed-over child.
    Process,
}

pub(crate) struct SessionTeardown {
    pub shared: Arc<SessionShared>,
    pub slot: usize,
    pub journal_tx: mpsc::Sender<JournalMsg>,
    pub journal_id: u64,
    pub exit: Option<SessionExitPlan>,
    _pty_reader: Box<dyn Read + Send>,
}

impl SessionTeardown {
    /// The old epilogue after the reader loop: journal flushed and closed
    /// BEFORE the exited manifest, timer jobs retired BEFORE it too, then the
    /// exit edge merged under the manifest lock and the sockets removed.
    pub(crate) fn run(self, timer_tx: &mpsc::Sender<TimerMsg>, outcome: Result<(), String>) {
        let SessionTeardown {
            shared,
            slot,
            journal_tx,
            journal_id,
            exit,
            _pty_reader,
        } = self;
        let mut result = outcome;

        let (ack_tx, ack_rx) = mpsc::channel();
        if journal_tx
            .send(JournalMsg::Close {
                id: journal_id,
                ack: ack_tx,
            })
            .is_ok()
        {
            match ack_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => result = result.and(Err(error)),
                Err(_) => result = result.and(Err("Output writer did not flush".into())),
            }
        }

        shared.running.store(false, Ordering::Relaxed);
        let (timer_ack_tx, timer_ack_rx) = mpsc::channel();
        if timer_tx
            .send(TimerMsg::Remove {
                slot,
                ack: timer_ack_tx,
            })
            .is_ok()
        {
            let _ = timer_ack_rx.recv_timeout(Duration::from_secs(30));
        }

        let Some(exit) = exit else {
            return;
        };
        let exit_code = shared
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.exit_code() as i32);
        let input_was_written = shared.has_been_written_to.load(Ordering::Relaxed);
        let host_build_id = exit.host_build_id.clone();
        let _ = update_manifest_session(&shared.session_id, |manifest| {
            manifest.state = HostedSessionState::Exited;
            manifest.pid = exit.pid;
            manifest.pid_started_at = exit.pid_started_at;
            manifest.exit_code = exit_code;
            manifest.host_build_id = host_build_id.clone();
            manifest.host_protocol_version = Some(SESSION_HOST_PROTOCOL_VERSION);
            manifest.has_been_written_to |= input_was_written;
            manifest.runtime = None;
            manifest.runtime_launch_pending = false;
            manifest.menu_prompt_active = false;
            manifest.screen_changed_at = None;
            manifest.detected_local_urls.clear();
            manifest.terminal_modes = None;
            manifest.heartbeat_at = current_timestamp_ms();
        });
        let _ = fs::remove_file(socket_path(&shared.session_id));
        let _ = fs::remove_file(&exit.session_socket_path);
        // Release the PTY master and child handle now that the exit edge is
        // published; the runtime Arc may still be held by a late one-shot
        // command thread, which then finds a closed child.
        (exit.on_exit)(result);
        let reactor = shared.reactor.clone();
        drop(shared);
        drop(_pty_reader);
        // Everything this Session owned is freed now; let the reactor hand
        // the pages back once its next idle tick finds no teardown racing.
        reactor.session_ended();
    }
}

/// Hand freed heap back to the OS. A long-lived core that hosted many busy
/// Sessions otherwise keeps their freed pages dirty in malloc's free lists
/// (the retention Superlogical criticised in Zellij), so its footprint never
/// shrinks after `unpeel rm`. Cheap enough to run per Session exit.
pub(crate) fn release_freed_memory() {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn malloc_default_zone() -> *mut libc::c_void;
            fn malloc_zone_pressure_relief(
                zone: *mut libc::c_void,
                goal: libc::size_t,
            ) -> libc::size_t;
        }
        unsafe {
            malloc_zone_pressure_relief(malloc_default_zone(), 0);
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

// ───────────────────── one-shot command dispatch ─────────────────────

/// The released blocking control-socket handler: read one JSON command line
/// and act on it. Kept for completeness (a stream that was never touched by
/// the reactor); the reactor itself reads the line and calls
/// `dispatch_client_command`.
#[allow(dead_code)]
pub(crate) fn handle_client(stream: UnixStream, shared: &SessionShared) -> Result<(), String> {
    configure_session_client(&stream)?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| format!("Failed to clone stream: {e}"))?,
    );
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read session command: {e}"))?;
    let command: SessionHostCommand =
        serde_json::from_str(line.trim()).map_err(|e| format!("Invalid session command: {e}"))?;
    dispatch_client_command(stream, command, shared)
}

/// Execute a one-shot control command on the calling (transient) thread and
/// write its JSON response. `StreamOutput`/`StreamInput` never reach here;
/// the reactor owns those connections.
pub(crate) fn dispatch_client_command(
    mut stream: UnixStream,
    command: SessionHostCommand,
    shared: &SessionShared,
) -> Result<(), String> {
    configure_session_client(&stream)?;
    let session_id = shared.session_id.as_str();
    let runtime = &shared.runtime;
    let viewport = &shared.viewport;
    let broadcaster = &shared.broadcaster;
    let agent_restart_lock = &shared.agent_restart_lock;

    if let SessionHostCommand::Snapshot = command {
        // Snapshot attach: offset capture and render happen under ONE
        // viewport lock. The reactor feeds the viewport before it
        // broadcasts, so a subscriber starting at this offset sees exactly
        // the bytes the snapshot does not already contain. Reply is one
        // JSON header line followed by the raw VT bytes.
        let (journal_offset, snapshot) = viewport.lock().unwrap().snapshot_vt();
        let header = crate::session_host::SnapshotVtHeader {
            journal_offset,
            cols: snapshot.cols,
            rows: snapshot.rows,
            bytes_len: snapshot.bytes.len() as u64,
        };
        let body = serde_json::json!({ "ok": true, "snapshot": header }).to_string();
        stream
            .write_all(body.as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .and_then(|_| stream.write_all(&snapshot.bytes))
            .and_then(|_| stream.flush())
            .map_err(|e| format!("Failed to write snapshot: {e}"))?;
        return Ok(());
    }

    let response = match command {
        SessionHostCommand::Write { data, write_id } => {
            match validate_write_id(write_id.as_deref()) {
                Err(error) => SessionHostResponse {
                    ok: false,
                    error: Some(error.to_string()),
                    viewport: None,
                },
                Ok(write_id) => {
                    // Serialize idempotency check → PTY write → history
                    // commit. A failed write is deliberately not recorded, so
                    // an HTTP retry can still deliver it; a racing retry waits
                    // on this same lock and observes the committed id after
                    // the first successful write.
                    let applied = {
                        let _restart_guard = agent_restart_lock
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let mut guard = runtime.lock().unwrap();
                        if write_id.is_some_and(|id| guard.recent_write_ids.contains(id)) {
                            false
                        } else {
                            guard
                                .writer
                                .write_all(data.as_bytes())
                                .map_err(|e| format!("Write error: {e}"))?;
                            if let Some(write_id) = write_id {
                                guard.recent_write_ids.record_applied(write_id);
                            }
                            true
                        }
                    };
                    if applied {
                        mark_input_written(session_id, &shared.has_been_written_to);
                        // Auto-title from the first submitted prompt for
                        // clients that write straight to the control socket
                        // (native attach, MCP).
                        maybe_auto_title_from_input(
                            session_id,
                            data.as_bytes(),
                            &shared.title_buffer,
                            &shared.title_done,
                        );
                    }
                    SessionHostResponse {
                        ok: true,
                        error: None,
                        viewport: None,
                    }
                }
            }
        }
        SessionHostCommand::Resize { cols, rows } => {
            // The phone endpoint both applies a raw resize (for unmounted
            // sessions) and updates the mounted Mac letterbox. The latter's
            // Ghostty→attach path reports the same grid again. Serialize and
            // deduplicate at the Host authority so that pair produces one
            // kernel PTY resize/SIGWINCH and one viewport reflow.
            let resized = {
                let mut guard = runtime.lock().unwrap();
                if guard.pty_cols == cols && guard.pty_rows == rows {
                    false
                } else {
                    guard
                        .master
                        .resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                        .map_err(|e| format!("Resize error: {e}"))?;
                    guard.pty_cols = cols;
                    guard.pty_rows = rows;
                    true
                }
            };
            // Scope the runtime guard to the cheap PTY resize only. Viewport
            // reflow can re-wrap up to 4MB; keeping it outside preserves
            // keystroke/write responsiveness.
            if resized {
                viewport.lock().unwrap().resize(cols, rows);
            }
            SessionHostResponse {
                ok: true,
                error: None,
                viewport: None,
            }
        }
        SessionHostCommand::Ping => SessionHostResponse {
            ok: true,
            error: None,
            viewport: None,
        },
        SessionHostCommand::RestartAgent {
            expected_generation,
        }
        | SessionHostCommand::ResumeAgent {
            expected_generation,
        } => {
            let result = match agent_restart_lock.try_lock() {
                Ok(_restart_guard) => resume_agent_in_place(
                    session_id,
                    runtime,
                    broadcaster,
                    &shared.runtime_generation,
                    &shared.pending_runtime_generation,
                    expected_generation,
                ),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    let _restart_guard = poisoned.into_inner();
                    resume_agent_in_place(
                        session_id,
                        runtime,
                        broadcaster,
                        &shared.runtime_generation,
                        &shared.pending_runtime_generation,
                        expected_generation,
                    )
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    Err("An agent restart is already in progress".into())
                }
            };
            match result {
                Ok(()) => SessionHostResponse {
                    ok: true,
                    error: None,
                    viewport: None,
                },
                Err(error) => SessionHostResponse {
                    ok: false,
                    error: Some(error),
                    viewport: None,
                },
            }
        }
        SessionHostCommand::ViewportSnapshot {
            cols,
            rows,
            scroll_offset_rows,
            viewport_rows,
        } => {
            let mut guard = viewport.lock().unwrap();
            // cols/rows of 0 mean "snapshot at the current size" (used by
            // callers like the MCP host that have no viewport of their own).
            // Non-zero dimensions are a virtual client snapshot: resize a
            // clone so remote/mobile clients cannot perturb the shared
            // viewport model that desktop attach owns via explicit Resize.
            let snapshot = if cols > 0 && rows > 0 {
                guard.snapshot_resized(cols, rows, scroll_offset_rows, viewport_rows)
            } else {
                guard.snapshot(scroll_offset_rows, viewport_rows)
            };
            SessionHostResponse {
                ok: true,
                error: None,
                viewport: Some(snapshot),
            }
        }
        SessionHostCommand::StreamOutput { .. } | SessionHostCommand::StreamInput => {
            return Err("stream commands are owned by the reactor".into());
        }
        // Answered by the early return above (header line + raw bytes, not a
        // JSON response).
        SessionHostCommand::Snapshot => unreachable!("snapshot is answered before the match"),
        SessionHostCommand::Kill => {
            // A raw socket client can race this final Host-owned stop against
            // RestartAgent without passing through session_ops' lifecycle
            // lock. If restart won, wait for its PTY submission to finish and
            // then terminate the newly launched foreground too. If Kill won,
            // a concurrent restart observes this lock and fails instead of
            // writing a new agent command behind the terminal shutdown.
            let _restart_guard = agent_restart_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            terminate_hosted_runtime(&mut runtime.lock().unwrap());
            // Let the reactor drain anything emitted during graceful
            // termination before using this flag to interrupt a retained
            // slave PTY that never reaches EOF.
            shared.running.store(false, Ordering::Relaxed);
            shared.wake();
            SessionHostResponse {
                ok: true,
                error: None,
                viewport: None,
            }
        }
    };

    let body =
        serde_json::to_string(&response).map_err(|e| format!("Failed to encode response: {e}"))?;
    stream
        .write_all(body.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to write response: {e}"))?;
    Ok(())
}

// ───────────────────── core-to-core handoff ─────────────────────

/// One attached control-socket client as moved between cores.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ClientHandoff {
    pub kind: ClientKind,
    #[serde(default)]
    pub answers_queries: bool,
    /// Stream position for `StreamOut`: the next journal offset this client
    /// expects. Always the Session's handed-over offset (everything before
    /// it is in `outbuf` or already on the wire).
    #[serde(default)]
    pub offset: u64,
    /// Bytes framed but not yet written to the socket.
    #[serde(default)]
    pub outbuf: Vec<u8>,
    /// Bytes read from the socket but not yet consumed (partial input
    /// frame, or a handshake line still being collected).
    #[serde(default)]
    pub inbuf: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClientKind {
    Handshake,
    StreamOut,
    StreamIn,
}

/// Everything the new core needs to host a Session whose fds it received.
/// Additive; the handoff header's `protocol` versions it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SessionHandoff {
    pub id: String,
    pub cwd: String,
    pub command: String,
    pub shell: String,
    pub custom_title: bool,
    pub title_done: bool,
    pub has_been_written_to: bool,
    pub runtime_launch_generation: u64,
    pub pending_runtime_generation: u64,
    pub pty_cols: u16,
    pub pty_rows: u16,
    pub child_pid: Option<u32>,
    pub child_pid_started_at: Option<u64>,
    pub dark_mode: bool,
    /// Broadcaster `next_offset` == journal length == snapshot offset.
    pub journal_next_offset: u64,
    pub retained_from: u64,
    pub agent_title_settled: bool,
    pub snapshot_cols: u16,
    pub snapshot_rows: u16,
    pub snapshot_len: u64,
    pub pending_pty_input: Vec<u8>,
    pub session_socket_path: String,
    pub clients: Vec<ClientHandoff>,
}

pub(crate) struct HandoffExport {
    pub meta: SessionHandoff,
    pub snapshot: Vec<u8>,
    /// `[pty master, session.sock listener, clients…]` in `meta.clients`
    /// order. Borrowed: the old core keeps them open until commit.
    pub fds: Vec<RawFd>,
}

impl SessionIo {
    /// Pause this Session for a handoff and describe it. The reactor has
    /// already flushed the journal and retired the timer jobs. Nothing is
    /// closed here: on a failed handoff `resume_after_handoff` continues.
    pub(crate) fn export_handoff(
        &mut self,
        registry: &mut Registry,
    ) -> Result<HandoffExport, String> {
        if self.ended || !self.shared.running.load(Ordering::Relaxed) {
            return Err(format!("session {} is stopping", self.shared.session_id));
        }
        self.handing_off = true;
        let write = self.pty_write_interest;
        self.set_pty_interest(registry, false, write);
        // Everything read from the PTY so far is in the VT, the broadcaster
        // and the journal; make the streaming clients' view current too.
        self.flush_stream_clients(registry);

        let (journal_next_offset, snapshot) = {
            let viewport = self.shared.viewport.lock().unwrap();
            viewport.snapshot_vt()
        };
        let broadcaster_offset = self.shared.broadcaster.lock().unwrap().next_offset;
        if broadcaster_offset != journal_next_offset {
            self.handing_off = false;
            let write = self.pty_write_interest;
            self.set_pty_interest(registry, true, write);
            return Err(format!(
                "session {} viewport offset {journal_next_offset} != stream offset {broadcaster_offset}",
                self.shared.session_id
            ));
        }
        let (pty_cols, pty_rows, shell, child_pid) = {
            let runtime = self.shared.runtime.lock().unwrap();
            (
                runtime.pty_cols,
                runtime.pty_rows,
                runtime.shell_executable.to_string_lossy().to_string(),
                runtime.child.process_id(),
            )
        };
        let mut fds = vec![self.pty_fd];
        let listener_fd = self
            .listener
            .as_ref()
            .map(|listener| listener.as_raw_fd())
            .ok_or("session has no listener")?;
        fds.push(listener_fd);
        let mut clients = Vec::new();
        let mut tokens: Vec<u64> = self.clients.keys().copied().collect();
        tokens.sort_unstable();
        for token in tokens {
            let client = &mut self.clients.get_mut(&token).unwrap();
            let (kind, answers_queries, inbuf) = match &mut client.state {
                ClientState::Handshake { buf } => (ClientKind::Handshake, false, buf.clone()),
                ClientState::StreamIn { buf } => (ClientKind::StreamIn, false, buf.clone()),
                ClientState::StreamOut {
                    closing, pending, ..
                } => {
                    if *closing {
                        continue;
                    }
                    if let Some(current) = pending.take() {
                        client.outbuf.extend(session_output_frame_bytes(&current));
                    }
                    let answers = self
                        .shared
                        .broadcaster
                        .lock()
                        .unwrap()
                        .subscriber_answers_queries(match &client.state {
                            ClientState::StreamOut { subscriber_id, .. } => *subscriber_id,
                            _ => 0,
                        });
                    (ClientKind::StreamOut, answers, Vec::new())
                }
            };
            clients.push(ClientHandoff {
                kind,
                answers_queries,
                offset: journal_next_offset,
                outbuf: client.outbuf[client.out_pos..].to_vec(),
                inbuf,
            });
            fds.push(client.stream.as_raw_fd());
        }
        let meta = SessionHandoff {
            id: self.shared.session_id.clone(),
            cwd: load_manifest(&self.shared.session_id)
                .map(|manifest| manifest.cwd)
                .unwrap_or_default(),
            command: self.job_seed.command.clone(),
            shell,
            custom_title: self.shared.title_done.load(Ordering::Relaxed),
            title_done: self.shared.title_done.load(Ordering::Relaxed),
            has_been_written_to: self.shared.has_been_written_to.load(Ordering::Relaxed),
            runtime_launch_generation: self.shared.runtime_generation.load(Ordering::Acquire),
            pending_runtime_generation: self
                .shared
                .pending_runtime_generation
                .load(Ordering::Acquire),
            pty_cols,
            pty_rows,
            child_pid: child_pid.or(self.job_seed.pid),
            child_pid_started_at: self.job_seed.pid_started_at,
            dark_mode: self.dark_mode,
            journal_next_offset,
            retained_from: output_retained_from(&self.shared.session_id),
            agent_title_settled: self.agent_title_settled,
            snapshot_cols: snapshot.cols,
            snapshot_rows: snapshot.rows,
            snapshot_len: snapshot.bytes.len() as u64,
            pending_pty_input: self.pending_input.iter().copied().collect(),
            session_socket_path: self
                .exit
                .as_ref()
                .map(|exit| exit.session_socket_path.to_string_lossy().to_string())
                .unwrap_or_default(),
            clients,
        };
        Ok(HandoffExport {
            meta,
            snapshot: snapshot.bytes,
            fds,
        })
    }

    /// The handoff did not go through: carry on exactly where we paused.
    pub(crate) fn resume_after_handoff(&mut self, registry: &mut Registry) -> Vec<HostTimerJob> {
        self.handing_off = false;
        if !self.ended {
            let write = self.pty_write_interest;
            self.set_pty_interest(registry, true, write);
        }
        build_session_timer_jobs(SessionJobInputs {
            session_id: self.shared.session_id.clone(),
            command: self.job_seed.command.clone(),
            shell: self.job_seed.shell.clone(),
            pid: self.job_seed.pid,
            pid_started_at: self.job_seed.pid_started_at,
            runtime: Arc::clone(&self.shared.runtime),
            runtime_generation: Arc::clone(&self.shared.runtime_generation),
            pending_runtime_generation: Arc::clone(&self.shared.pending_runtime_generation),
            viewport: Arc::clone(&self.shared.viewport),
        })
    }

    /// The new core owns everything now: forget the fds without running
    /// any teardown (no exited manifest, no socket removal, no on_exit).
    pub(crate) fn forget_after_handoff(mut self, registry: &mut Registry) {
        self.ended = true;
        // See `PtyWriter::disarm`: never let our writer's destructor type
        // EOF into a terminal the new core now owns.
        if let Ok(mut runtime) = self.shared.runtime.lock() {
            runtime.writer.disarm();
        }
        registry.remove(self.pty_fd, self.pty_token);
        if let Some(listener) = self.listener.take() {
            registry.remove(listener.as_raw_fd(), self.listener_token);
        }
        let clients: Vec<Client> = self.clients.drain().map(|(_, client)| client).collect();
        for client in clients {
            registry.remove(client.stream.as_raw_fd(), client.token);
            if let ClientState::StreamOut { subscriber_id, .. } = &client.state {
                self.shared
                    .broadcaster
                    .lock()
                    .unwrap()
                    .remove_subscriber(*subscriber_id);
            }
        }
        // Dropping `self` closes this core's copies of the fds; the new
        // core holds its own. `exit.on_exit` is deliberately never called.
        if let Some(exit) = self.exit.take() {
            drop(exit.on_exit);
        }
    }
}

/// A PTY master received from another core: the same kernel object,
/// driven through raw ioctls instead of portable-pty's owned handle.
pub(crate) struct RawMasterPty {
    fd: OwnedFd,
}

impl RawMasterPty {
    pub(crate) fn new(fd: OwnedFd) -> Self {
        Self { fd }
    }

    fn dup_file(&self) -> std::io::Result<fs::File> {
        let fd = unsafe { libc::dup(self.fd.as_raw_fd()) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }
}

impl portable_pty::MasterPty for RawMasterPty {
    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        let winsize = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCSWINSZ, &winsize) } != 0 {
            return Err(anyhow::Error::from(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        let mut winsize: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCGWINSZ, &mut winsize) } != 0 {
            return Err(anyhow::Error::from(std::io::Error::last_os_error()));
        }
        Ok(PtySize {
            rows: winsize.ws_row,
            cols: winsize.ws_col,
            pixel_width: winsize.ws_xpixel,
            pixel_height: winsize.ws_ypixel,
        })
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(self.dup_file()?))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(self.dup_file()?))
    }

    fn process_group_leader(&self) -> Option<libc::pid_t> {
        let pgid = unsafe { libc::tcgetpgrp(self.fd.as_raw_fd()) };
        (pgid > 0).then_some(pgid)
    }

    fn as_raw_fd(&self) -> Option<RawFd> {
        Some(self.fd.as_raw_fd())
    }
}

/// The shell of a handed-over Session. It is not this process's child (it
/// was reparented to init when the old core exited), so exit status comes
/// from the reactor's process watch (`EVFILT_PROC` on macOS) or, without
/// one, is unknown; liveness is proven by pid + kernel start time.
#[derive(Clone, Debug)]
pub(crate) struct HandedOverChild {
    pid: u32,
    started_at: Option<u64>,
    exit: Arc<Mutex<Option<portable_pty::ExitStatus>>>,
}

impl HandedOverChild {
    pub(crate) fn new(pid: u32, started_at: Option<u64>) -> Self {
        Self {
            pid,
            started_at,
            exit: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn exit_slot(&self) -> Arc<Mutex<Option<portable_pty::ExitStatus>>> {
        Arc::clone(&self.exit)
    }

    fn alive(&self) -> bool {
        recorded_pid_identity(self.pid, self.started_at) == PidIdentity::Matches
            || (self.started_at.is_none() && process_exists(self.pid))
    }
}

impl portable_pty::ChildKiller for HandedOverChild {
    /// Signals only a positively identified child (pid + recorded kernel
    /// start time). A handoff without a start time still reports liveness
    /// through the bare probe above, but it must never be a kill target: the
    /// pid may have been recycled onto a stranger since the old core spawned
    /// it. Worst case an orphaned shell lingers, which beats killing an
    /// unrelated process.
    fn kill(&mut self) -> std::io::Result<()> {
        if recorded_pid_identity(self.pid, self.started_at) != PidIdentity::Matches {
            return Ok(());
        }
        if unsafe { libc::kill(self.pid as i32, libc::SIGKILL) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

impl portable_pty::Child for HandedOverChild {
    fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        if let Some(status) = self.exit.lock().unwrap().clone() {
            return Ok(Some(status));
        }
        if self.alive() {
            return Ok(None);
        }
        // Gone without an observed status: report an unknown-but-ended
        // exit so callers that only ask "is it over" get the truth.
        Ok(None)
    }

    fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            if !self.alive() {
                return Ok(portable_pty::ExitStatus::with_exit_code(0));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

/// What `rebuild_from_handoff` hands back besides the Session: the child
/// pid and its exit slot so the reactor can watch the process.
pub(crate) struct RebuiltSession {
    pub session: SessionIo,
    pub child_pid: Option<u32>,
    pub exit_slot: Arc<Mutex<Option<portable_pty::ExitStatus>>>,
}

/// Build a `SessionIo` for a Session received from another core. `fds` are
/// `[pty master, session.sock listener, clients…]` as the export listed
/// them. The journal is reopened in place, the VT is rebuilt from the
/// snapshot at the handed-over offset, and the broadcaster starts there.
pub(crate) fn rebuild_from_handoff(
    meta: SessionHandoff,
    snapshot: Vec<u8>,
    mut fds: Vec<OwnedFd>,
    services: &crate::session_host::core_reactor::CoreServices,
    on_exit: Box<dyn FnOnce(Result<(), String>) + Send>,
) -> Result<RebuiltSession, String> {
    if fds.len() < 2 + meta.clients.len() {
        return Err(format!(
            "session {}: expected {} fds, received {}",
            meta.id,
            2 + meta.clients.len(),
            fds.len()
        ));
    }
    let client_fds: Vec<OwnedFd> = fds.split_off(2);
    let listener_fd = fds.pop().unwrap();
    let master_fd = fds.pop().unwrap();
    let pty_fd = master_fd.as_raw_fd();
    set_nonblocking(pty_fd, true)
        .map_err(|e| format!("Failed to make the handed-over PTY master non-blocking: {e}"))?;
    let master = RawMasterPty::new(master_fd);
    let writer = portable_pty::MasterPty::take_writer(&master)
        .map_err(|e| format!("Failed to dup the PTY writer: {e}"))?;
    let reader = portable_pty::MasterPty::try_clone_reader(&master)
        .map_err(|e| format!("Failed to dup the PTY reader: {e}"))?;
    let listener = UnixListener::from(listener_fd);
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to configure the handed-over listener: {e}"))?;

    let child_pid = meta.child_pid;
    let child = HandedOverChild::new(child_pid.unwrap_or(0), meta.child_pid_started_at);
    let exit_slot = child.exit_slot();
    let runtime = Arc::new(Mutex::new(HostRuntime {
        master: Box::new(master),
        writer: PtyWriter::new(writer, pty_fd),
        child: Box::new(child),
        pty_cols: meta.pty_cols,
        pty_rows: meta.pty_rows,
        shell_executable: PathBuf::from(&meta.shell),
        last_runtime_observation: None,
        recent_write_ids: RecentWriteIds::default(),
    }));

    let mut viewport_state = TerminalViewportState::new(meta.snapshot_cols, meta.snapshot_rows);
    viewport_state.reset_at_output_offset(
        meta.snapshot_cols,
        meta.snapshot_rows,
        meta.journal_next_offset,
        meta.journal_next_offset > 0,
    );
    viewport_state.feed(&snapshot);
    // The snapshot bytes are not journal bytes: pin the offset back.
    viewport_state.set_output_offset(meta.journal_next_offset);
    if meta.pty_cols != meta.snapshot_cols || meta.pty_rows != meta.snapshot_rows {
        viewport_state.resize(meta.pty_cols, meta.pty_rows);
    }
    viewport_state.set_journal_session(meta.id.clone());
    let viewport = Arc::new(Mutex::new(viewport_state));
    let broadcaster = Arc::new(Mutex::new(OutputBroadcaster::at_offset(
        meta.journal_next_offset,
    )));

    let output_path = session_dir(&meta.id).join("output.bin");
    let output_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&output_path)
        .map_err(|e| format!("Failed to reopen output log {}: {e}", output_path.display()))?;
    let retained_output = RetainedOutputWriter::reopen(
        output_file,
        output_path,
        meta.journal_next_offset,
        SESSION_OUTPUT_JOURNAL_RETAIN_BYTES,
        SESSION_OUTPUT_JOURNAL_ADVANCE_BYTES,
    )?;

    let shared = Arc::new(SessionShared {
        session_id: meta.id.clone(),
        runtime: Arc::clone(&runtime),
        viewport: Arc::clone(&viewport),
        running: Arc::new(AtomicBool::new(true)),
        broadcaster,
        title_buffer: Arc::new(Mutex::new(String::new())),
        title_done: Arc::new(AtomicBool::new(meta.title_done)),
        has_been_written_to: Arc::new(AtomicBool::new(meta.has_been_written_to)),
        agent_restart_lock: Arc::new(Mutex::new(())),
        runtime_generation: Arc::new(AtomicU64::new(meta.runtime_launch_generation)),
        pending_runtime_generation: Arc::new(AtomicU64::new(meta.pending_runtime_generation)),
        reactor: services.reactor.clone(),
        slot: AtomicUsize::new(usize::MAX),
    });
    let pressure = Arc::new(JournalBackpressure::default());
    let journal_id = next_journal_id();
    services
        .journal_tx
        .send(JournalMsg::Open {
            id: journal_id,
            writer: retained_output,
            pressure: Arc::clone(&pressure),
        })
        .map_err(|_| "Output writer is gone".to_string())?;
    let journal = JournalHandle {
        id: journal_id,
        tx: services.journal_tx.clone(),
        pressure,
        failed: Arc::new(AtomicBool::new(false)),
    };
    let seed = JobSeed {
        command: meta.command.clone(),
        shell: meta.shell.clone(),
        pid: child_pid,
        pid_started_at: meta.child_pid_started_at,
    };
    let jobs = build_session_timer_jobs(SessionJobInputs {
        session_id: meta.id.clone(),
        command: seed.command.clone(),
        shell: seed.shell.clone(),
        pid: seed.pid,
        pid_started_at: seed.pid_started_at,
        runtime: Arc::clone(&runtime),
        runtime_generation: Arc::clone(&shared.runtime_generation),
        pending_runtime_generation: Arc::clone(&shared.pending_runtime_generation),
        viewport: Arc::clone(&viewport),
    });
    let exit = SessionExitPlan {
        pid: child_pid,
        pid_started_at: meta.child_pid_started_at,
        host_build_id: current_host_build_id(),
        session_socket_path: if meta.session_socket_path.is_empty() {
            socket_path(&meta.id)
        } else {
            PathBuf::from(&meta.session_socket_path)
        },
        on_exit,
    };
    let mut session = SessionIo::new(
        shared,
        pty_fd,
        reader,
        listener,
        meta.dark_mode,
        journal,
        jobs,
        exit,
        seed,
    );
    session.agent_title_settled = meta.agent_title_settled;
    session.pending_input = meta.pending_pty_input.iter().copied().collect();
    let clients: Vec<(UnixStream, ClientHandoff)> = client_fds
        .into_iter()
        .zip(meta.clients)
        .map(|(fd, handoff)| (UnixStream::from(fd), handoff))
        .collect();
    session.set_adopted_clients(clients);

    // The launch-window fields name the Host process; keep them truthful.
    let host_pid = std::process::id();
    let host_pid_started_at = process_start_time_ms(host_pid);
    let _ = update_manifest_session(&meta.id, |manifest| {
        manifest.host_pid = Some(host_pid);
        manifest.host_pid_started_at = host_pid_started_at;
        manifest.host_build_id = current_host_build_id();
        manifest.heartbeat_at = current_timestamp_ms();
    });
    Ok(RebuiltSession {
        session,
        child_pid,
        exit_slot,
    })
}

/// The reactor observed the handed-over child exit (macOS `EVFILT_PROC`).
pub(crate) fn record_child_exit(
    slot: &Arc<Mutex<Option<portable_pty::ExitStatus>>>,
    status: Option<i64>,
) {
    let status = match status {
        Some(raw) if raw >= 0 => {
            let raw = raw as i32;
            if libc::WIFEXITED(raw) {
                portable_pty::ExitStatus::with_exit_code(libc::WEXITSTATUS(raw) as u32)
            } else if libc::WIFSIGNALED(raw) {
                portable_pty::ExitStatus::with_exit_code(128 + libc::WTERMSIG(raw) as u32)
            } else {
                portable_pty::ExitStatus::with_exit_code(raw as u32)
            }
        }
        _ => portable_pty::ExitStatus::with_exit_code(0),
    };
    *slot.lock().unwrap() = Some(status);
}

#[cfg(test)]
mod handed_over_child_tests {
    use super::*;
    use portable_pty::ChildKiller;

    /// A live process that is NOT the handed-over child, standing in for
    /// whatever the OS recycled the recorded pid onto.
    fn unrelated_live_process() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep")
    }

    #[test]
    fn kill_never_signals_a_recycled_or_unproven_pid() {
        let mut stranger = unrelated_live_process();
        let pid = stranger.id();
        let actual_start = process_start_time_ms(pid).expect("start time of a live child");

        // Recorded an hour before the live process started: recycled pid.
        let mut recycled = HandedOverChild::new(pid, Some(actual_start - 3_600_000));
        assert!(recycled.kill().is_ok());
        // Legacy handoff without a start time: liveness is reported, but
        // identity is unproven, so no signal.
        let mut unproven = HandedOverChild::new(pid, None);
        assert!(unproven.kill().is_ok());
        thread::sleep(Duration::from_millis(100));
        assert!(
            stranger.try_wait().unwrap().is_none(),
            "an unrelated process under the recorded pid must survive kill()"
        );

        // The genuine child (matching start time) is signaled.
        let mut ours = HandedOverChild::new(pid, Some(actual_start));
        assert!(ours.kill().is_ok());
        let status = stranger.wait().unwrap();
        assert!(!status.success(), "SIGKILL reached the verified child");
    }
}
