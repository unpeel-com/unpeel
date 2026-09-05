//! Controller-side child-process transport for the shared Host contract.
//!
//! Two launch flavors share one gateway protocol and all of the process
//! plumbing: system SSH to a remote Host, and the loopback workspace gateway
//! ([`LocalProcessConnection`]) that spawns `unpeel-host __remote_stdio__`
//! directly against another local workspace home. Product callers always
//! launch `/usr/bin/ssh` with structured arguments; no target or remote
//! command is ever interpolated through a shell. A background reader
//! correlates out-of-order stdio responses, while writes are serialized into
//! complete frames. Process loss fails every call in that generation. Only an
//! unconstrained call may reconnect lazily. Semantic backends must bootstrap
//! that new transport generation before binding later reads or effects to it;
//! the transport never replays a call itself.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::host_connection::{
    ConnectionGeneration, DeliveryState, HostCall, HostConnection, HostConnectionError, HostReply,
    PreparedHostCall, RequestSemantics,
};
use crate::relay_wire::{self, TunnelRequest, TunnelResponse};
use crate::remote_stdio::{self, FRAME_KIND_REQUEST, FRAME_KIND_RESPONSE, REMOTE_STDIO_ARG};

pub const SYSTEM_SSH_PATH: &str = "/usr/bin/ssh";
const STDERR_TAIL_BYTES: usize = 16 * 1024;
const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const INTERACTIVE_PREAMBLE_BYTES: usize = 64 * 1024;
const INTERACTIVE_START_TIMEOUT: Duration = Duration::from_secs(20);
const SSH_INSTALL_TIMEOUT: Duration = Duration::from_secs(180);
const SSH_INSTALL_OUTPUT_BYTES: usize = 64 * 1024;
const SSH_INSTALL_COMMAND: &str = "install_path=\"${TMPDIR:-/tmp}/unpeel-install-$$.sh\"; curl -fsSL https://unpeel.com/install.sh -o \"$install_path\" && sh \"$install_path\"; install_status=$?; rm -f \"$install_path\"; exit \"$install_status\"";

/// How system SSH starts the Host gateway. Ordinary SSH servers should use
/// `Command`; `InteractiveShell` exists for managed shells (for example
/// Upstash Box) that accept a PTY login but reject SSH remote commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshLaunchMode {
    Command,
    InteractiveShell,
}

/// Optional non-interactive password/API-key authentication. The secret is
/// deliberately not `Debug`; it is passed only in the environment of the
/// owned SSH process and read by the supplied askpass helper.
#[derive(Clone)]
pub struct SshAskpass {
    program: PathBuf,
    secret: String,
}

impl SshAskpass {
    pub fn new(
        program: impl AsRef<Path>,
        secret: impl Into<String>,
    ) -> Result<Self, HostConnectionError> {
        let program = program.as_ref();
        if !program.is_absolute() {
            return Err(HostConnectionError::Configuration(
                "SSH askpass executable must be an absolute path".to_string(),
            ));
        }
        let secret = secret.into();
        if secret.is_empty() {
            return Err(HostConnectionError::Configuration(
                "SSH password or API key cannot be empty".to_string(),
            ));
        }
        Ok(Self {
            program: program.to_owned(),
            secret,
        })
    }
}

#[derive(Clone)]
pub struct SshConnectionOptions {
    pub launch_mode: SshLaunchMode,
    pub askpass: Option<SshAskpass>,
}

impl Default for SshConnectionOptions {
    fn default() -> Self {
        Self {
            launch_mode: SshLaunchMode::Command,
            askpass: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshInstallResult {
    pub launch_mode: SshLaunchMode,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    uri: String,
    destination: String,
}

impl SshTarget {
    /// Parse `ssh://alias` or `ssh://user@host`. V1 intentionally leaves
    /// ports, ProxyJump, identities, and other policy in `~/.ssh/config`.
    pub fn parse(uri: &str) -> Result<Self, HostConnectionError> {
        let destination = uri.strip_prefix("ssh://").ok_or_else(|| {
            HostConnectionError::InvalidTarget("expected ssh://alias".to_string())
        })?;
        if destination.is_empty() {
            return Err(HostConnectionError::InvalidTarget(
                "the destination is empty".to_string(),
            ));
        }
        if destination.len() > 255 {
            return Err(HostConnectionError::InvalidTarget(
                "the destination is too long".to_string(),
            ));
        }
        if destination.starts_with('-') {
            return Err(HostConnectionError::InvalidTarget(
                "the destination cannot begin with '-'".to_string(),
            ));
        }
        if destination.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | '?' | '#' | ':' | ';' | '|' | '&' | '$' | '`'
                )
        }) {
            return Err(HostConnectionError::InvalidTarget(
                "use an SSH config alias or user@host without a path, port, or shell characters"
                    .to_string(),
            ));
        }
        let at_count = destination.bytes().filter(|byte| *byte == b'@').count();
        if at_count > 1 || destination.starts_with('@') || destination.ends_with('@') {
            return Err(HostConnectionError::InvalidTarget(
                "invalid user@host destination".to_string(),
            ));
        }
        Ok(Self {
            uri: uri.to_owned(),
            destination: destination.to_owned(),
        })
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn destination(&self) -> &str {
        &self.destination
    }
}

enum PendingFailure {
    Disconnected(String),
    TimedOut(DeliveryState),
}

type PendingResult = Result<TunnelResponse, PendingFailure>;

struct PendingCall {
    sender: Sender<PendingResult>,
    wrote_any: Arc<AtomicBool>,
}

struct ProgressWriter<'a> {
    writer: &'a mut ChildStdin,
    wrote_any: &'a AtomicBool,
}

impl Write for ProgressWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.writer.write(bytes)?;
        if count > 0 {
            self.wrote_any.store(true, Ordering::Release);
        }
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

struct WatchdogCancel(Option<mpsc::SyncSender<()>>);

impl Drop for WatchdogCancel {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}

struct DiagnosticTail {
    bytes: Vec<u8>,
    /// Set when the child's stderr reached EOF, so a reader that saw stdout
    /// close can tell whether the diagnostic tail is final yet.
    closed: bool,
}

impl DiagnosticTail {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            closed: false,
        }
    }

    fn mark_closed(&mut self) {
        self.closed = true;
    }

    fn is_closed(&self) -> bool {
        self.closed
    }

    fn append(&mut self, bytes: &[u8]) {
        if bytes.len() >= STDERR_TAIL_BYTES {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&bytes[bytes.len() - STDERR_TAIL_BYTES..]);
            return;
        }
        let overflow = self
            .bytes
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(STDERR_TAIL_BYTES);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
        self.bytes.extend_from_slice(bytes);
    }

    fn message(&self) -> Option<String> {
        let message = String::from_utf8_lossy(&self.bytes).trim().to_owned();
        (!message.is_empty()).then_some(message)
    }
}

struct ProcessGeneration {
    id: u64,
    /// "SSH" or "workspace gateway": only for user-facing diagnostics.
    label: &'static str,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: Mutex<HashMap<u64, PendingCall>>,
    dead: AtomicBool,
    diagnostics: Arc<Mutex<DiagnosticTail>>,
}

impl ProcessGeneration {
    fn is_alive(&self) -> bool {
        !self.dead.load(Ordering::Acquire)
    }

    /// stdout EOF usually races the stderr drain: the child writes its reason,
    /// exits, and both pipes close within microseconds of each other, in either
    /// order (observed on Linux CI). Give the stderr reader a bounded moment to
    /// reach EOF so `diagnostic_message` reports the child's own reason instead
    /// of the generic "closed its response stream".
    fn await_stderr_settled(&self, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            let closed = self
                .diagnostics
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_closed();
            if closed || Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn diagnostic_message(&self, fallback: &str) -> String {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .message()
            .unwrap_or_else(|| fallback.to_owned())
    }

    fn complete(&self, response: TunnelResponse) -> bool {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&response.id);
        if let Some(pending) = pending {
            let _ = pending.sender.send(Ok(response));
            true
        } else {
            false
        }
    }

    fn fail(&self, message: impl Into<String>) {
        if self.dead.swap(true, Ordering::AcqRel) {
            return;
        }
        let message = self.diagnostic_message(&message.into());
        let pending: Vec<PendingCall> = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, pending)| pending)
            .collect();
        for pending in pending {
            let _ = pending
                .sender
                .send(Err(PendingFailure::Disconnected(message.clone())));
        }
        self.terminate();
    }

    fn timeout_request(&self, request_id: u64) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&request_id);
        let Some(pending) = pending else {
            return;
        };
        let delivery = if pending.wrote_any.load(Ordering::Acquire) {
            DeliveryState::OutcomeUnknown
        } else {
            DeliveryState::NotSent
        };
        let _ = pending.sender.send(Err(PendingFailure::TimedOut(delivery)));
        self.fail(format!("request {request_id} timed out"));
    }

    fn terminate(&self) {
        // Kill before taking the writer lock. If a malformed peer stops
        // reading while a large frame fills the pipe, waiting for stdin first
        // would deadlock the response reader that is trying to invalidate the
        // generation. Killing the owned child unblocks that write.
        let child = self
            .child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut child) = child {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        drop(
            self.stdin
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take(),
        );
    }

    fn round_trip(
        self: &Arc<Self>,
        request: &TunnelRequest,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<TunnelResponse, AttemptFailure> {
        if !self.is_alive() {
            return Err(AttemptFailure::disconnected(
                DeliveryState::NotSent,
                self.diagnostic_message(&format!("{} process is not running", self.label)),
            ));
        }

        let (sender, receiver): (Sender<PendingResult>, Receiver<PendingResult>) = mpsc::channel();
        let wrote_any = Arc::new(AtomicBool::new(false));
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !self.is_alive() {
                return Err(AttemptFailure::disconnected(
                    DeliveryState::NotSent,
                    self.diagnostic_message(&format!(
                        "{} process stopped before request dispatch",
                        self.label
                    )),
                ));
            }
            if pending.len() >= MAX_IN_FLIGHT_REQUESTS {
                return Err(AttemptFailure::TooManyInFlight);
            }
            if pending.contains_key(&request.id) {
                return Err(AttemptFailure::DuplicateRequestId);
            }
            pending.insert(
                request.id,
                PendingCall {
                    sender,
                    wrote_any: Arc::clone(&wrote_any),
                },
            );
        }

        let (cancel_watchdog, watchdog_cancelled) = mpsc::sync_channel(1);
        let weak = Arc::downgrade(self);
        let request_id = request.id;
        if let Err(error) = std::thread::Builder::new()
            .name(format!("unpeel-ssh-timeout-{request_id}"))
            .spawn(move || {
                if matches!(
                    watchdog_cancelled.recv_timeout(timeout),
                    Err(RecvTimeoutError::Timeout)
                ) {
                    if let Some(generation) = weak.upgrade() {
                        generation.timeout_request(request_id);
                    }
                }
            })
        {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&request.id);
            return Err(AttemptFailure::disconnected(
                DeliveryState::NotSent,
                format!("start {} request watchdog: {error}", self.label),
            ));
        }
        let _watchdog = WatchdogCancel(Some(cancel_watchdog));

        let write_result = {
            let mut stdin = self
                .stdin
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !self.is_alive() || stdin.is_none() {
                Err((
                    DeliveryState::NotSent,
                    format!("{} process stopped before request write", self.label),
                ))
            } else {
                let mut writer = ProgressWriter {
                    writer: stdin.as_mut().expect("checked gateway stdin"),
                    wrote_any: &wrote_any,
                };
                remote_stdio::write_frame(&mut writer, FRAME_KIND_REQUEST, payload).map_err(
                    |message| {
                        let delivery = if wrote_any.load(Ordering::Acquire) {
                            DeliveryState::OutcomeUnknown
                        } else {
                            DeliveryState::NotSent
                        };
                        (delivery, message)
                    },
                )
            }
        };
        if let Err((delivery, message)) = write_result {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&request.id);
            self.fail(message.clone());
            if let Ok(Err(failure)) = receiver.try_recv() {
                return match failure {
                    PendingFailure::Disconnected(message) => Err(AttemptFailure::disconnected(
                        if wrote_any.load(Ordering::Acquire) {
                            DeliveryState::OutcomeUnknown
                        } else {
                            DeliveryState::NotSent
                        },
                        message,
                    )),
                    PendingFailure::TimedOut(delivery) => Err(AttemptFailure::TimedOut(delivery)),
                };
            }
            return Err(AttemptFailure::disconnected(delivery, message));
        }

        match receiver.recv() {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(PendingFailure::Disconnected(message))) => Err(AttemptFailure::disconnected(
                if wrote_any.load(Ordering::Acquire) {
                    DeliveryState::OutcomeUnknown
                } else {
                    DeliveryState::NotSent
                },
                message,
            )),
            Ok(Err(PendingFailure::TimedOut(delivery))) => Err(AttemptFailure::TimedOut(delivery)),
            Err(_) => {
                let message =
                    self.diagnostic_message(&format!("{} response channel closed", self.label));
                self.fail(message.clone());
                Err(AttemptFailure::disconnected(
                    DeliveryState::OutcomeUnknown,
                    message,
                ))
            }
        }
    }
}

impl Drop for ProcessGeneration {
    fn drop(&mut self) {
        self.dead.store(true, Ordering::Release);
        self.terminate();
    }
}

enum AttemptFailure {
    TooManyInFlight,
    DuplicateRequestId,
    Disconnected {
        delivery: DeliveryState,
        message: String,
    },
    TimedOut(DeliveryState),
}

impl AttemptFailure {
    fn disconnected(delivery: DeliveryState, message: String) -> Self {
        Self::Disconnected { delivery, message }
    }

    fn invalidates_generation(&self) -> bool {
        matches!(self, Self::Disconnected { .. } | Self::TimedOut(_))
    }

    fn into_public(self, request_id: u64, semantics: RequestSemantics) -> HostConnectionError {
        match self {
            Self::TooManyInFlight => HostConnectionError::TooManyInFlight {
                request_id,
                limit: MAX_IN_FLIGHT_REQUESTS,
            },
            Self::DuplicateRequestId => HostConnectionError::DuplicateRequestId(request_id),
            Self::Disconnected { delivery, message } => HostConnectionError::Disconnected {
                request_id,
                semantics,
                delivery,
                message,
            },
            Self::TimedOut(delivery) => HostConnectionError::TimedOut {
                request_id,
                semantics,
                delivery,
            },
        }
    }
}

struct ConnectionState {
    generation: Option<Arc<ProcessGeneration>>,
}

/// How the gateway child is launched. Everything above the spawned process —
/// framing, correlation, generations, timeouts, shutdown — is identical for
/// both flavors; the argv/env is the only difference.
enum ProcessLaunch {
    Ssh {
        target: SshTarget,
        ssh_program: PathBuf,
        options: SshConnectionOptions,
    },
    /// Loopback workspace gateway: `<unpeel-host> __remote_stdio__` spawned
    /// directly with `UNPEEL_HOME=<workspace home>`. The Controller supplies
    /// both absolute paths; this layer never guesses install locations.
    LocalGateway {
        host_program: PathBuf,
        unpeel_home: PathBuf,
        require_host_service: bool,
    },
}

/// The workspace-gateway flavor of the shared child-process transport
/// (the private "workspaces-unification" design record phase 2). Construct it with
/// [`SshHostConnection::local_gateway`].
pub type LocalProcessConnection = SshHostConnection;

pub struct SshHostConnection {
    connection_id: uuid::Uuid,
    launch: ProcessLaunch,
    closed: AtomicBool,
    state: Mutex<ConnectionState>,
    next_generation: AtomicU64,
    next_request_id: AtomicU64,
}

impl SshHostConnection {
    pub fn new(target: SshTarget) -> Self {
        Self::with_options_and_ssh_program(target, SshConnectionOptions::default(), SYSTEM_SSH_PATH)
            .expect("the fixed system SSH path is absolute")
    }

    /// Loopback gateway to another LOCAL workspace: spawn the caller-supplied
    /// `unpeel-host` binary in `__remote_stdio__` mode with the workspace's
    /// `UNPEEL_HOME`. Inherited `UNPEEL_*`/`HERDR_*` env is stripped (same
    /// containment as hosted-child spawns) so the gateway serves exactly the
    /// selected home, never this process's own state dir.
    pub fn local_gateway(
        host_program: impl AsRef<Path>,
        unpeel_home: impl AsRef<Path>,
    ) -> Result<Self, HostConnectionError> {
        Self::local_gateway_with_service_requirement(host_program, unpeel_home, false)
    }

    /// Local Controller transport that must reach the persistent workspace
    /// worker owned by `unpeel serve`. Unlike [`Self::local_gateway`], this
    /// never falls back to constructing a second semantic Host inside the
    /// compatibility child when `host.sock` is unavailable.
    pub fn local_host_service(
        host_program: impl AsRef<Path>,
        unpeel_home: impl AsRef<Path>,
    ) -> Result<Self, HostConnectionError> {
        Self::local_gateway_with_service_requirement(host_program, unpeel_home, true)
    }

    fn local_gateway_with_service_requirement(
        host_program: impl AsRef<Path>,
        unpeel_home: impl AsRef<Path>,
        require_host_service: bool,
    ) -> Result<Self, HostConnectionError> {
        let host_program = host_program.as_ref();
        if !host_program.is_absolute() {
            return Err(HostConnectionError::Configuration(
                "workspace gateway executable must be an absolute path".to_string(),
            ));
        }
        let unpeel_home = unpeel_home.as_ref();
        if !unpeel_home.is_absolute() {
            return Err(HostConnectionError::Configuration(
                "workspace home must be an absolute path".to_string(),
            ));
        }
        Ok(Self::with_launch(ProcessLaunch::LocalGateway {
            host_program: host_program.to_owned(),
            unpeel_home: unpeel_home.to_owned(),
            require_host_service,
        }))
    }

    pub fn with_options(
        target: SshTarget,
        options: SshConnectionOptions,
    ) -> Result<Self, HostConnectionError> {
        Self::with_options_and_ssh_program(target, options, SYSTEM_SSH_PATH)
    }

    /// Alternate executable injection for transport conformance tests. The
    /// shipped App/TUI path always uses [`SshHostConnection::new`].
    #[doc(hidden)]
    pub fn with_ssh_program(
        target: SshTarget,
        ssh_program: impl AsRef<Path>,
    ) -> Result<Self, HostConnectionError> {
        Self::with_options_and_ssh_program(target, SshConnectionOptions::default(), ssh_program)
    }

    #[doc(hidden)]
    pub fn with_options_and_ssh_program(
        target: SshTarget,
        options: SshConnectionOptions,
        ssh_program: impl AsRef<Path>,
    ) -> Result<Self, HostConnectionError> {
        let ssh_program = ssh_program.as_ref();
        if !ssh_program.is_absolute() {
            return Err(HostConnectionError::Configuration(
                "SSH executable must be an absolute path".to_string(),
            ));
        }
        Ok(Self::with_launch(ProcessLaunch::Ssh {
            target,
            ssh_program: ssh_program.to_owned(),
            options,
        }))
    }

    fn with_launch(launch: ProcessLaunch) -> Self {
        Self {
            connection_id: uuid::Uuid::new_v4(),
            launch,
            closed: AtomicBool::new(false),
            state: Mutex::new(ConnectionState { generation: None }),
            next_generation: AtomicU64::new(1),
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Only the SSH flavor has a destination; the workspace gateway is
    /// addressed by its home path instead.
    pub fn target(&self) -> Option<&SshTarget> {
        match &self.launch {
            ProcessLaunch::Ssh { target, .. } => Some(target),
            ProcessLaunch::LocalGateway { .. } => None,
        }
    }

    fn transport_label(&self) -> &'static str {
        match &self.launch {
            ProcessLaunch::Ssh { .. } => "SSH",
            ProcessLaunch::LocalGateway { .. } => "workspace gateway",
        }
    }

    fn allocate_request_id(&self) -> Result<u64, HostConnectionError> {
        let mut current = self.next_request_id.load(Ordering::Relaxed);
        loop {
            if current == u64::MAX {
                return Err(HostConnectionError::RequestIdExhausted);
            }
            match self.next_request_id.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(current),
                Err(actual) => current = actual,
            }
        }
    }

    fn allocate_generation_id(&self) -> Result<u64, String> {
        let mut current = self.next_generation.load(Ordering::Relaxed);
        loop {
            if current == u64::MAX {
                return Err("SSH generation id space is exhausted".to_string());
            }
            match self.next_generation.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(current),
                Err(actual) => current = actual,
            }
        }
    }

    fn current_live_generation(&self) -> Option<Arc<ProcessGeneration>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
            .as_ref()
            .filter(|generation| generation.is_alive())
            .map(Arc::clone)
    }

    fn generation_token(&self, generation: &ProcessGeneration) -> ConnectionGeneration {
        ConnectionGeneration {
            connection_id: self.connection_id,
            sequence: generation.id,
        }
    }

    pub fn disconnect(&self) {
        self.closed.store(true, Ordering::Release);
        let generation = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
            .take();
        if let Some(generation) = generation {
            generation.fail("Controller disconnected");
        }
    }

    fn command(&self) -> Command {
        self.command_with_purpose(false)
    }

    fn install_command(&self) -> Command {
        self.command_with_purpose(true)
    }

    fn command_with_purpose(&self, install: bool) -> Command {
        let (target, ssh_program, options) = match &self.launch {
            ProcessLaunch::Ssh {
                target,
                ssh_program,
                options,
            } => (target, ssh_program, options),
            ProcessLaunch::LocalGateway {
                host_program,
                unpeel_home,
                require_host_service,
            } => {
                let mut command = Command::new(host_program);
                command
                    .arg(REMOTE_STDIO_ARG)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                // Same containment as hosted-child spawns (session_host.rs):
                // this Controller may itself be a workspace instance or run
                // inside a Herdr pane; the gateway must resolve ONLY the
                // selected workspace home and report to no outer supervisor.
                strip_env_prefix_from_command(&mut command, std::env::vars_os(), "UNPEEL_");
                strip_env_prefix_from_command(&mut command, std::env::vars_os(), "HERDR_");
                command
                    .env("UNPEEL_HOME", unpeel_home)
                    // `__remote_stdio__` is shared with SSH, whose managed
                    // PTYs need an idle reaper. This direct child has reliable
                    // EOF ownership and may remain quiet in the workspace
                    // pool, so it must not inherit the SSH watchdog.
                    .env("UNPEEL_LOCAL_GATEWAY", "1");
                if *require_host_service {
                    command.env("UNPEEL_LOCAL_HOST_REQUIRED", "1");
                }
                return command;
            }
        };
        let mut command = Command::new(ssh_program);
        command
            .arg(match options.launch_mode {
                SshLaunchMode::Command => "-T",
                SshLaunchMode::InteractiveShell => "-tt",
            })
            // Disable credential/display forwarding regardless of the user's
            // ssh_config. ClearAllForwardings only clears Local/Remote/Dynamic
            // port forwards; agent, X11, and GSSAPI credential delegation must
            // be turned off explicitly so a compromised Host cannot reach the
            // Controller's SSH agent, X display, or Kerberos credentials.
            .arg("-a")
            .arg("-x")
            .arg("-o")
            .arg(if options.askpass.is_some() {
                "BatchMode=no"
            } else {
                "BatchMode=yes"
            })
            .arg("-o")
            // The native app has no controlling terminal for OpenSSH's
            // first-use confirmation. Trust a previously unseen key once,
            // but retain OpenSSH's fail-closed behavior for changed keys.
            .arg("StrictHostKeyChecking=accept-new")
            .arg("-o")
            .arg("NumberOfPasswordPrompts=1")
            .arg("-o")
            .arg("ClearAllForwardings=yes")
            .arg("-o")
            .arg("ForwardAgent=no")
            .arg("-o")
            .arg("ForwardX11=no")
            .arg("-o")
            .arg("ForwardX11Trusted=no")
            .arg("-o")
            .arg("GSSAPIDelegateCredentials=no")
            .arg("-o")
            .arg("ControlMaster=no")
            .arg("-o")
            .arg("ControlPath=none")
            .arg("-o")
            .arg("PermitLocalCommand=no")
            .arg("-o")
            .arg("EscapeChar=none")
            .arg("-o")
            .arg("RemoteCommand=none")
            .arg("-o")
            .arg("StdinNull=no")
            .arg("--")
            .arg(target.destination())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if options.launch_mode == SshLaunchMode::Command {
            if install {
                // This is a fixed product-owned command. No target, secret,
                // or other user input is interpolated into the remote shell.
                command.arg(SSH_INSTALL_COMMAND);
            } else {
                // Prefer the canonical persistent `unpeel serve` runtime on
                // the Host. The gateway falls back to its disk adapter when
                // no live local socket exists, preserving older installs.
                command
                    .arg("env")
                    .arg("UNPEEL_LOCAL_GATEWAY=1")
                    .arg("unpeel-host")
                    .arg(REMOTE_STDIO_ARG);
            }
        }
        if let Some(askpass) = options.askpass.as_ref() {
            command
                .env("SSH_ASKPASS", &askpass.program)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env("DISPLAY", "unpeel-ssh")
                .env("UNPEEL_SSH_ASKPASS_SECRET", &askpass.secret);
        }
        command
    }

    fn generation(&self) -> Result<Arc<ProcessGeneration>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.closed.load(Ordering::Acquire) {
            return Err("Host connection is closed".to_string());
        }
        if let Some(generation) = state.generation.as_ref() {
            if generation.is_alive() {
                return Ok(Arc::clone(generation));
            }
        }
        state.generation = None;
        let generation_id = self.allocate_generation_id()?;
        let generation = self.spawn_generation(generation_id)?;
        state.generation = Some(Arc::clone(&generation));
        Ok(generation)
    }

    fn spawn_generation(&self, generation_id: u64) -> Result<Arc<ProcessGeneration>, String> {
        let label = self.transport_label();
        let mut child = self.command().spawn().map_err(|error| error.to_string())?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            format!("{label} stdin was unavailable")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            format!("{label} stdout was unavailable")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            format!("{label} stderr was unavailable")
        })?;
        let interactive = matches!(
            &self.launch,
            ProcessLaunch::Ssh { options, .. }
                if options.launch_mode == SshLaunchMode::InteractiveShell
        );
        let stdout = if interactive {
            let marker = format!("UNPEEL_GATEWAY_READY_{}", uuid::Uuid::new_v4().simple());
            let command = format!(
                "stty -echo; printf '\\n{marker}\\n'; stty raw -echo; exec env \
UNPEEL_LOCAL_GATEWAY=1 unpeel-host {REMOTE_STDIO_ARG}\n"
            );
            if let Err(error) = stdin
                .write_all(command.as_bytes())
                .and_then(|_| stdin.flush())
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("start interactive SSH Host gateway: {error}"));
            }
            wait_for_interactive_gateway(stdout, &marker, &mut child)?
        } else {
            PrefixedReader::new(stdout, Vec::new())
        };
        let generation = Arc::new(ProcessGeneration {
            id: generation_id,
            label,
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            pending: Mutex::new(HashMap::new()),
            dead: AtomicBool::new(false),
            diagnostics: Arc::new(Mutex::new(DiagnosticTail::new())),
        });

        let weak = Arc::downgrade(&generation);
        std::thread::Builder::new()
            .name(format!("unpeel-ssh-read-{generation_id}"))
            .spawn(move || read_responses(stdout, weak))
            .map_err(|error| {
                generation.fail(format!("start {label} response reader: {error}"));
                error.to_string()
            })?;

        let diagnostics = Arc::clone(&generation.diagnostics);
        if let Err(error) = std::thread::Builder::new()
            .name(format!("unpeel-ssh-stderr-{generation_id}"))
            .spawn(move || drain_stderr(stderr, diagnostics))
        {
            generation.fail(format!("start {label} diagnostics reader: {error}"));
            return Err(error.to_string());
        }

        Ok(generation)
    }

    fn invalidate(&self, generation: &Arc<ProcessGeneration>, message: &str) {
        generation.fail(message.to_owned());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .generation
            .as_ref()
            .is_some_and(|current| current.id == generation.id)
        {
            state.generation = None;
        }
    }
}

impl HostConnection for SshHostConnection {
    fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::Closed);
        }
        Ok(PreparedHostCall {
            connection_id: self.connection_id,
            request_id: self.allocate_request_id()?,
            required_generation: None,
            call,
        })
    }

    fn prepare_in_generation(
        &self,
        generation: ConnectionGeneration,
        call: HostCall,
    ) -> Result<PreparedHostCall, HostConnectionError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::Closed);
        }
        if generation.connection_id != self.connection_id {
            return Err(HostConnectionError::WrongGeneration(generation));
        }
        let request_id = self.allocate_request_id()?;
        let current = self.current_live_generation();
        let generation_is_current = current
            .as_ref()
            .map(|current| current.id == generation.sequence)
            .unwrap_or(false);
        if !generation_is_current {
            return Err(HostConnectionError::GenerationChanged {
                request_id,
                expected: generation,
            });
        }
        Ok(PreparedHostCall {
            connection_id: self.connection_id,
            request_id,
            required_generation: Some(generation),
            call,
        })
    }

    fn request(
        &self,
        call: PreparedHostCall,
        timeout: Duration,
    ) -> Result<HostReply, HostConnectionError> {
        let request_id = call.request_id;
        let semantics = call.call.semantics;
        let required_generation = call.required_generation;
        if call.connection_id != self.connection_id {
            return Err(HostConnectionError::WrongConnection(request_id));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::ClosedRequest(request_id));
        }
        let request = call.into_tunnel();
        let payload = relay_wire::encode_tunnel_request(&request);
        if !relay_wire::plaintext_frame_fits(payload.len()) {
            return Err(HostConnectionError::RequestTooLarge {
                request_id,
                encoded_bytes: payload.len(),
                max_bytes: relay_wire::MAX_PLAINTEXT_BYTES,
            });
        }

        let generation = match required_generation {
            Some(expected) => {
                let current = self.current_live_generation();
                match current.as_ref() {
                    Some(current) if current.id == expected.sequence => Arc::clone(current),
                    _ => {
                        return Err(HostConnectionError::GenerationChanged {
                            request_id,
                            expected,
                        });
                    }
                }
            }
            None => match self.generation() {
                Ok(generation) => generation,
                Err(_) if self.closed.load(Ordering::Acquire) => {
                    return Err(HostConnectionError::ClosedRequest(request_id));
                }
                Err(message) => {
                    return Err(HostConnectionError::Launch {
                        request_id,
                        message,
                    });
                }
            },
        };
        match generation.round_trip(&request, &payload, timeout) {
            Ok(response) => Ok(HostReply {
                request_id: response.id,
                generation: self.generation_token(&generation),
                status: response.status,
                body: response.body,
            }),
            Err(failure) => {
                if failure.invalidates_generation() {
                    self.invalidate(
                        &generation,
                        &format!("{} request failed", self.transport_label()),
                    );
                }
                Err(failure.into_public(request_id, semantics))
            }
        }
    }

    fn disconnect(&self) {
        SshHostConnection::disconnect(self);
    }
}

impl Drop for SshHostConnection {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Install the released Unpeel CLI on an SSH destination using the same
/// system-SSH policy and optional askpass credential as Host connections.
/// The remote script is fixed by Unpeel; callers cannot supply shell text.
pub fn install_unpeel_over_ssh(
    target: SshTarget,
    options: SshConnectionOptions,
) -> Result<SshInstallResult, String> {
    install_unpeel_over_ssh_with_program(target, options, SYSTEM_SSH_PATH)
}

#[doc(hidden)]
pub fn install_unpeel_over_ssh_with_program(
    target: SshTarget,
    options: SshConnectionOptions,
    ssh_program: impl AsRef<Path>,
) -> Result<SshInstallResult, String> {
    let launch_mode = options.launch_mode;
    let connection = SshHostConnection::with_options_and_ssh_program(target, options, ssh_program)
        .map_err(|error| error.to_string())?;
    let mut child = connection
        .install_command()
        .spawn()
        .map_err(|error| format!("start SSH installer: {error}"))?;

    if launch_mode == SshLaunchMode::InteractiveShell {
        let script = format!("stty -echo 2>/dev/null; {SSH_INSTALL_COMMAND}\n");
        let write_result = child
            .stdin
            .as_mut()
            .ok_or_else(|| "SSH installer stdin was unavailable".to_string())?
            .write_all(script.as_bytes());
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("start interactive SSH installer: {error}"));
        }
    }
    drop(child.stdin.take());

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "SSH installer stdout was unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "SSH installer stderr was unavailable".to_string())?;
    let stdout_reader = std::thread::Builder::new()
        .name("unpeel-ssh-install-stdout".to_string())
        .spawn(move || read_bounded_tail(stdout, SSH_INSTALL_OUTPUT_BYTES))
        .map_err(|error| format!("read SSH installer output: {error}"))?;
    let stderr_reader = std::thread::Builder::new()
        .name("unpeel-ssh-install-stderr".to_string())
        .spawn(move || read_bounded_tail(stderr, SSH_INSTALL_OUTPUT_BYTES))
        .map_err(|error| format!("read SSH installer diagnostics: {error}"))?;

    let deadline = Instant::now() + SSH_INSTALL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("Installing Unpeel over SSH timed out after 3 minutes".to_string());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!("wait for SSH installer: {error}"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "SSH installer output reader stopped unexpectedly".to_string())?
        .map_err(|error| format!("read SSH installer output: {error}"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "SSH installer diagnostics reader stopped unexpectedly".to_string())?
        .map_err(|error| format!("read SSH installer diagnostics: {error}"))?;
    let output = combined_install_output(&stdout, &stderr);
    if !status.success() {
        let detail = if output.is_empty() {
            format!("SSH exited with {status}")
        } else {
            output
        };
        return Err(format!("Could not install Unpeel: {detail}"));
    }
    Ok(SshInstallResult {
        launch_mode,
        output,
    })
}

fn read_bounded_tail(mut reader: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(retained);
        }
        retained.extend_from_slice(&buffer[..count]);
        if retained.len() > limit {
            retained.drain(..retained.len() - limit);
        }
    }
}

fn combined_install_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout,
        (true, false) => stderr,
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

struct PrefixedReader<R> {
    inner: R,
    prefix: std::io::Cursor<Vec<u8>>,
}

impl<R> PrefixedReader<R> {
    fn new(inner: R, prefix: Vec<u8>) -> Self {
        Self {
            inner,
            prefix: std::io::Cursor::new(prefix),
        }
    }
}

impl<R: Read> Read for PrefixedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.prefix.read(buffer)?;
        if count > 0 {
            return Ok(count);
        }
        self.inner.read(buffer)
    }
}

fn wait_for_interactive_gateway(
    stdout: ChildStdout,
    marker: &str,
    child: &mut Child,
) -> Result<PrefixedReader<ChildStdout>, String> {
    let marker = marker.as_bytes().to_vec();
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("unpeel-ssh-interactive-start".to_string())
        .spawn(move || {
            let mut stdout = stdout;
            let mut preamble = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(Err(
                            "interactive SSH shell closed before Unpeel started".to_string(),
                        ));
                        return;
                    }
                    Ok(count) => {
                        preamble.extend_from_slice(&buffer[..count]);
                        if let Some(end) = isolated_marker_end(&preamble, &marker) {
                            let remaining = preamble.split_off(end);
                            let _ = sender.send(Ok(PrefixedReader::new(stdout, remaining)));
                            return;
                        }
                        if preamble.len() > INTERACTIVE_PREAMBLE_BYTES {
                            let _ = sender.send(Err(
                                "interactive SSH shell produced too much output before Unpeel started"
                                    .to_string(),
                            ));
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(format!(
                            "read interactive SSH Host gateway startup: {error}"
                        )));
                        return;
                    }
                }
            }
        })
        .map_err(|error| format!("start interactive SSH gateway reader: {error}"))?;

    match receiver.recv_timeout(INTERACTIVE_START_TIMEOUT) {
        Ok(Ok(reader)) => Ok(reader),
        Ok(Err(message)) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(message)
        }
        Err(RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("interactive SSH Host did not start Unpeel within 20 seconds".to_string())
        }
        Err(RecvTimeoutError::Disconnected) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("interactive SSH gateway reader stopped unexpectedly".to_string())
        }
    }
}

fn isolated_marker_end(bytes: &[u8], marker: &[u8]) -> Option<usize> {
    let mut start = 0;
    while let Some(relative) = bytes[start..]
        .windows(marker.len())
        .position(|window| window == marker)
    {
        let index = start + relative;
        let before_is_line = index > 0 && bytes[index - 1] == b'\n';
        let after = index + marker.len();
        let after_is_line = bytes.get(after) == Some(&b'\n')
            || (bytes.get(after) == Some(&b'\r') && bytes.get(after + 1) == Some(&b'\n'));
        if before_is_line && after_is_line {
            return Some(after + usize::from(bytes.get(after) == Some(&b'\r')) + 1);
        }
        start = index + 1;
    }
    None
}

fn read_responses<R: Read>(mut stdout: R, generation: Weak<ProcessGeneration>) {
    loop {
        let frame = match remote_stdio::read_frame(&mut stdout) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                if let Some(generation) = generation.upgrade() {
                    generation.await_stderr_settled(Duration::from_secs(1));
                    generation.fail(format!(
                        "{} process closed its response stream",
                        generation.label
                    ));
                }
                return;
            }
            Err(message) => {
                if let Some(generation) = generation.upgrade() {
                    generation.fail(message);
                }
                return;
            }
        };
        if frame.kind != FRAME_KIND_RESPONSE {
            if let Some(generation) = generation.upgrade() {
                generation.fail(format!(
                    "unexpected {} stdio response frame kind {}",
                    generation.label, frame.kind
                ));
            }
            return;
        }
        let response = match relay_wire::parse_tunnel_response(&frame.payload) {
            Ok(response) => response,
            Err(message) => {
                if let Some(generation) = generation.upgrade() {
                    generation.fail(format!(
                        "invalid {} Host response: {message}",
                        generation.label
                    ));
                }
                return;
            }
        };
        let Some(generation) = generation.upgrade() else {
            return;
        };
        let response_id = response.id;
        if !generation.complete(response) {
            generation.fail(format!(
                "{} Host returned unknown or duplicate response id {response_id}",
                generation.label
            ));
            return;
        }
    }
}

/// Mirror of session_host's hosted-child env containment for the workspace
/// gateway spawn: remove every inherited variable with `prefix` so the child
/// resolves only what this transport sets explicitly.
fn strip_env_prefix_from_command<I>(command: &mut Command, vars: I, prefix: &str)
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    for (key, _) in vars {
        if key.as_encoded_bytes().starts_with(prefix.as_bytes()) {
            command.env_remove(key);
        }
    }
}

fn drain_stderr(mut stderr: ChildStderr, diagnostics: Arc<Mutex<DiagnosticTail>>) {
    let mut buffer = [0u8; 4096];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) | Err(_) => {
                diagnostics
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .mark_closed();
                return;
            }
            Ok(count) => diagnostics
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .append(&buffer[..count]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_is_an_opaque_safe_ssh_destination() {
        let target = SshTarget::parse("ssh://tommy@studio").unwrap();
        assert_eq!(target.destination(), "tommy@studio");
        assert_eq!(target.uri(), "ssh://tommy@studio");

        for invalid in [
            "studio",
            "ssh://",
            "ssh://-oProxyCommand=bad",
            "ssh://studio/path",
            "ssh://studio:2222",
            "ssh://studio;touch-bad",
            "ssh://user@@studio",
        ] {
            assert!(SshTarget::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn system_command_is_structured_and_noninteractive() {
        let connection = SshHostConnection::new(SshTarget::parse("ssh://studio").unwrap());
        let command = connection.command();
        assert_eq!(command.get_program(), Path::new(SYSTEM_SSH_PATH));
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            arguments,
            [
                "-T",
                "-a",
                "-x",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "NumberOfPasswordPrompts=1",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ForwardAgent=no",
                "-o",
                "ForwardX11=no",
                "-o",
                "ForwardX11Trusted=no",
                "-o",
                "GSSAPIDelegateCredentials=no",
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
                "-o",
                "PermitLocalCommand=no",
                "-o",
                "EscapeChar=none",
                "-o",
                "RemoteCommand=none",
                "-o",
                "StdinNull=no",
                "--",
                "studio",
                "env",
                "UNPEEL_LOCAL_GATEWAY=1",
                "unpeel-host",
                REMOTE_STDIO_ARG,
            ]
        );
    }

    #[test]
    fn local_gateway_command_is_direct_scoped_and_env_contained() {
        // Present in this test process; must be stripped from the child.
        std::env::set_var("UNPEEL_TEST_LEAK_PROBE", "leak");
        std::env::set_var("HERDR_TEST_LEAK_PROBE", "leak");
        let connection =
            SshHostConnection::local_gateway("/bundle/unpeel-host", "/homes/writing").unwrap();
        assert!(connection.target().is_none());
        let command = connection.command();
        assert_eq!(command.get_program(), Path::new("/bundle/unpeel-host"));
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(arguments, [REMOTE_STDIO_ARG]);
        let environment: HashMap<String, Option<String>> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            environment.get("UNPEEL_HOME"),
            Some(&Some("/homes/writing".to_string()))
        );
        assert_eq!(
            environment.get("UNPEEL_LOCAL_GATEWAY"),
            Some(&Some("1".to_string()))
        );
        assert!(!environment.contains_key("UNPEEL_LOCAL_HOST_REQUIRED"));
        assert_eq!(environment.get("UNPEEL_TEST_LEAK_PROBE"), Some(&None));
        assert_eq!(environment.get("HERDR_TEST_LEAK_PROBE"), Some(&None));

        let required =
            SshHostConnection::local_host_service("/bundle/unpeel-host", "/homes/writing")
                .unwrap()
                .command();
        let required_environment: HashMap<String, Option<String>> = required
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            required_environment.get("UNPEEL_LOCAL_HOST_REQUIRED"),
            Some(&Some("1".to_string()))
        );
        std::env::remove_var("UNPEEL_TEST_LEAK_PROBE");
        std::env::remove_var("HERDR_TEST_LEAK_PROBE");

        for (program, home) in [
            ("unpeel-host", "/homes/writing"),
            ("/bundle/unpeel-host", "profiles/writing"),
        ] {
            assert!(
                SshHostConnection::local_gateway(program, home).is_err(),
                "accepted relative path {program} {home}"
            );
        }
    }

    #[test]
    fn interactive_command_uses_pty_askpass_and_no_remote_argv() {
        let options = SshConnectionOptions {
            launch_mode: SshLaunchMode::InteractiveShell,
            askpass: Some(SshAskpass::new("/absolute/askpass", "secret").unwrap()),
        };
        let connection =
            SshHostConnection::with_options(SshTarget::parse("ssh://managed").unwrap(), options)
                .unwrap();
        let command = connection.command();
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(arguments.first().map(String::as_str), Some("-tt"));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["-o", "BatchMode=no"]));
        assert_eq!(arguments.last().map(String::as_str), Some("managed"));
        assert!(!arguments
            .iter()
            .any(|argument| argument == REMOTE_STDIO_ARG));
        let environment: HashMap<String, String> = command
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            environment.get("SSH_ASKPASS").map(String::as_str),
            Some("/absolute/askpass")
        );
        assert_eq!(
            environment.get("SSH_ASKPASS_REQUIRE").map(String::as_str),
            Some("force")
        );
    }

    #[test]
    fn installer_command_is_fixed_and_uses_selected_launch_mode() {
        let target = SshTarget::parse("ssh://studio").unwrap();
        let command_connection =
            SshHostConnection::with_options(target.clone(), SshConnectionOptions::default())
                .unwrap();
        let command = command_connection.install_command();
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(arguments.first().map(String::as_str), Some("-T"));
        assert_eq!(
            arguments.last().map(String::as_str),
            Some(SSH_INSTALL_COMMAND)
        );
        assert!(!arguments
            .iter()
            .any(|argument| argument == REMOTE_STDIO_ARG));

        let interactive_connection = SshHostConnection::with_options(
            target,
            SshConnectionOptions {
                launch_mode: SshLaunchMode::InteractiveShell,
                askpass: None,
            },
        )
        .unwrap();
        let interactive = interactive_connection.install_command();
        let arguments: Vec<String> = interactive
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(arguments.first().map(String::as_str), Some("-tt"));
        assert_eq!(arguments.last().map(String::as_str), Some("studio"));
    }

    #[test]
    fn interactive_marker_must_occupy_its_own_line() {
        let marker = b"UNPEEL_READY";
        assert_eq!(
            isolated_marker_end(b"banner\r\nUNPEEL_READY\r\nframe", marker),
            Some(22)
        );
        assert!(isolated_marker_end(b"echo printf UNPEEL_READY suffix\r\n", marker).is_none());
    }

    #[test]
    fn prepared_calls_are_connection_owned_and_monotonic() {
        let first = SshHostConnection::new(SshTarget::parse("ssh://studio").unwrap());
        let second = SshHostConnection::new(SshTarget::parse("ssh://studio").unwrap());
        let call = || HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly);
        let first_call = first.prepare(call()).unwrap();
        let second_call = first.prepare(call()).unwrap();
        assert_eq!(first_call.request_id(), 1);
        assert_eq!(second_call.request_id(), 2);

        let error = second
            .request(first_call, Duration::from_millis(1))
            .unwrap_err();
        assert_eq!(error, HostConnectionError::WrongConnection(1));

        let first_generation = ConnectionGeneration {
            connection_id: first.connection_id,
            sequence: 1,
        };
        assert_eq!(
            second
                .prepare_in_generation(first_generation, call())
                .unwrap_err(),
            HostConnectionError::WrongGeneration(first_generation)
        );
        let error = first
            .prepare_in_generation(first_generation, call())
            .unwrap_err();
        assert!(matches!(
            &error,
            HostConnectionError::GenerationChanged {
                request_id: 3,
                expected,
            } if *expected == first_generation
        ));
        assert_eq!(error.delivery(), Some(DeliveryState::NotSent));
    }

    #[test]
    fn request_ids_never_wrap_and_oversize_never_spawns() {
        let target = SshTarget::parse("ssh://studio").unwrap();
        let connection =
            SshHostConnection::with_ssh_program(target, "/definitely/missing/ssh").unwrap();
        connection
            .next_request_id
            .store(u64::MAX, Ordering::Relaxed);
        let call = HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly);
        assert_eq!(
            connection.prepare(call).unwrap_err(),
            HostConnectionError::RequestIdExhausted
        );

        connection.next_request_id.store(1, Ordering::Relaxed);
        let oversized = HostCall::new("POST", "/mobile/write", RequestSemantics::Effect).with_body(
            "application/octet-stream",
            vec![0; relay_wire::MAX_PLAINTEXT_BYTES],
        );
        let prepared = connection.prepare(oversized).unwrap();
        let error = connection
            .request(prepared, Duration::from_millis(1))
            .unwrap_err();
        assert!(matches!(
            &error,
            HostConnectionError::RequestTooLarge { request_id: 1, .. }
        ));
        assert_eq!(error.delivery(), Some(DeliveryState::NotSent));

        connection
            .next_generation
            .store(u64::MAX, Ordering::Relaxed);
        let prepared = connection
            .prepare(HostCall::new(
                "GET",
                "/mobile/bootstrap",
                RequestSemantics::ReadOnly,
            ))
            .unwrap();
        let error = connection
            .request(prepared, Duration::from_millis(1))
            .unwrap_err();
        assert!(matches!(
            &error,
            HostConnectionError::Launch {
                request_id: 2,
                message,
            } if message.contains("generation id space is exhausted")
        ));
        assert_eq!(error.delivery(), Some(DeliveryState::NotSent));
    }
}
