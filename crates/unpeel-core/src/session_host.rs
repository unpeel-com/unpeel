use crate::app_paths::app_sessions_root as unpeel_sessions_root;
use crate::integrations::{self, shared};
use crate::menu_prompt::viewport_has_menu_prompt;
use crate::runtime_observer::ActiveRuntimeObservation;
use crate::state::{current_timestamp_ms, SessionInfo};
use crate::terminal_viewport::{TerminalViewportSnapshot, TerminalViewportState};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc, Arc, Mutex, OnceLock,
};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

#[cfg(unix)]
#[path = "core_reactor.rs"]
pub(crate) mod core_reactor;
#[cfg(unix)]
#[path = "fd_pass.rs"]
pub(crate) mod fd_pass;
#[cfg(unix)]
#[path = "session_io.rs"]
pub(crate) mod session_io;

pub const SESSION_HOST_ARG: &str = "__session_host__";
pub const COMPACT_OUTPUT_JOURNALS_ARG: &str = "__compact_output_journals__";
/// First hosted-PTY control protocol that can restart a managed agent in
/// place while preserving the Session and terminal identity.
pub const SESSION_HOST_RESTART_AGENT_PROTOCOL_VERSION: u32 = 2;
/// First hosted-PTY control protocol with the shell-only Resume Agent verb.
/// Unlike the legacy restart operation, this capability never signals a
/// foreground runtime; it can submit the saved resume recipe only after a
/// fresh proof that the owned shell controls the terminal.
pub const SESSION_HOST_RESUME_AGENT_PROTOCOL_VERSION: u32 = 3;
/// First hosted-PTY protocol whose terminal journal keeps lifetime logical
/// offsets while reclaiming old physical blocks. Bumping the current protocol
/// makes surviving pre-retention hosts eligible for the normal Reload Terminal
/// recommendation instead of letting their output logs grow forever.
pub const SESSION_HOST_BOUNDED_JOURNAL_PROTOCOL_VERSION: u32 = 4;
const SESSION_HOST_PROTOCOL_VERSION: u32 = SESSION_HOST_BOUNDED_JOURNAL_PROTOCOL_VERSION;
const DEFAULT_OUTPUT_READ_BYTES: usize = 128 * 1024;
const DEFAULT_OUTPUT_TAIL_BYTES: usize = 256 * 1024;
/// Terminal output is a recovery/replay journal, not permanent transcript
/// storage. Keep ample history for attach (2 MiB), viewport reconstruction
/// (up to 4 MiB), and remote resume (2 MiB), while bounding repaint-heavy
/// always-on TUIs to a predictable amount of physical disk per Session.
pub const SESSION_OUTPUT_JOURNAL_RETAIN_BYTES: u64 = 64 * 1024 * 1024;
/// Advance retention in coarse steps so a hot TUI does not rewrite metadata
/// or punch filesystem blocks for every output batch. Physical use therefore
/// floats between 64 and 72 MiB, plus filesystem metadata.
const SESSION_OUTPUT_JOURNAL_ADVANCE_BYTES: u64 = 8 * 1024 * 1024;
/// A syntactically incomplete control string cannot be allowed to pin the
/// retained floor forever. If the last true VT-safe boundary is farther back
/// than this, retention starts at a UTF-8 boundary and readers intentionally
/// treat the truncated suffix as a fresh Ground-state terminal stream.
const SESSION_OUTPUT_JOURNAL_CONTROL_SLACK_BYTES: u64 = 64 * 1024;
pub const OUTPUT_RETENTION_FILE: &str = "output-retention.json";
const SESSION_HEARTBEAT_INTERVAL_MS: u64 = 60_000;
const SESSION_HEARTBEAT_STALE_MS: u64 = 180_000;
/// How often the host re-scans a session's visible screen for an agent-drawn
/// select-menu prompt. Fast enough that the attention badge feels immediate,
/// cheap enough to run per live session (a substring scan of the viewport).
const SESSION_MENU_SCAN_INTERVAL_MS: u64 = 500;
/// Foreground jobs change on user commands, so live runtime identity needs a
/// tighter cadence than the heartbeat while remaining cheap per hosted PTY.
const SESSION_RUNTIME_SCAN_INTERVAL_MS: u64 = 300;
/// Keep a recognized agent through short-lived foreground tool subprocesses
/// and transient process-enumeration misses. Returning to the owned shell is
/// definitive and clears immediately; other misses need this confirmation.
const SESSION_RUNTIME_CLEAR_MISSES: u8 = 6;
/// Local-URL liveness probes run every this many menu-scan ticks (~5s):
/// probing issues real HTTP GETs against detected dev servers, so it must
/// stay far below the scan cadence while still removing dead servers from
/// the UI within seconds of them stopping.
const URL_PROBE_TICKS: u32 = 10;
const SESSION_PING_TIMEOUT_MS: u64 = 250;
const SESSION_SNAPSHOT_TIMEOUT_MS: u64 = 2_000;
const MANIFEST_HEALTH_CACHE_TTL_MS: u64 = 500;
const MANIFEST_HEALTH_CACHE_PRUNE_LEN: usize = 256;
const MANIFEST_HEALTH_CACHE_STALE_MS: u64 = 60_000;
const SESSION_OUTPUT_BATCH_FLUSH_MS: u64 = 32;
const SESSION_OUTPUT_BATCH_MAX_BYTES: usize = 128 * 1024;
#[allow(dead_code)]
const SESSION_OUTPUT_CHANNEL_CAPACITY: usize = 64;
const SESSION_OUTPUT_READ_BUFFER_BYTES: usize = 64 * 1024;
/// Bound how long the output thread can stay inside `poll` before observing a
/// control-socket Kill. The read itself remains blocking once poll says the
/// owned PTY has data, so steady-state sessions do not spin.
#[allow(dead_code)]
const SESSION_OUTPUT_READ_POLL_MS: i32 = 100;
#[allow(dead_code)]
const SESSION_OUTPUT_STREAM_BATCH_FLUSH_MS: u64 = 1;
const SESSION_OUTPUT_STREAM_BATCH_MAX_BYTES: usize = 64 * 1024;
/// Recent output chunks the broadcaster keeps in memory so a subscriber that
/// joins at the journal's committed tail can bridge the (≤ 32 ms) flush lag
/// without a hole. Anything older comes from `output.bin` through the
/// journal fallback, so this only needs to cover a flush interval of output,
/// not a screenful: 128 KiB is many times a PTY's worst 32 ms burst, and the
/// old 1 MiB was ~1 MiB of resident cost per busy Session in the core.
const SESSION_OUTPUT_STREAM_RECENT_BYTES: usize = 128 * 1024;
const SESSION_OUTPUT_STREAM_READ_TIMEOUT_MS: u64 = 250;
/// Per-subscriber cap on bytes buffered in the broadcast channel but not yet
/// written to the client socket. A subscriber that stops draining (hung UI,
/// SIGSTOPed attach client, dead peer that never sent RST) is dropped once it
/// exceeds this, so a chatty session can't grow host memory without bound.
const SESSION_OUTPUT_STREAM_SUBSCRIBER_MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;
/// Hard write timeout on an accepted output-stream connection. If a client
/// stops reading for this long, `write_all` in the forwarder fails, the
/// subscriber is removed, and its channel/backlog is freed. Belt-and-suspenders
/// with the per-subscriber byte cap above.
const SESSION_OUTPUT_STREAM_WRITE_TIMEOUT_MS: u64 = 60_000;
const SESSION_OUTPUT_STREAM_FRAME_HEADER_BYTES: usize = 13;
/// Reader-side sanity bound on a single output-stream frame. Hosts never
/// send frames beyond the 64KB batch cap; anything wildly larger means the
/// peer is not speaking the frame protocol at all.
const SESSION_OUTPUT_STREAM_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const SESSION_INPUT_STREAM_FRAME_HEADER_BYTES: usize = 4;
const SESSION_INPUT_STREAM_MAX_FRAME_BYTES: usize = 256 * 1024;
const SESSION_INPUT_STREAM_ACK: u8 = 0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionExecutionScope {
    #[default]
    Local,
    RemoteController,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHostLaunch {
    pub session: SessionInfo,
    pub cwd: String,
    pub dark_mode: Option<bool>,
    /// Optional `#RRGGBB` accent resolved by the launching frontend. Native
    /// uses the Session project's folder color, then its workspace App color.
    /// Headless/TUI launches may forward the same Host-level environment
    /// contract. Missing preserves each standalone App's own palette.
    #[serde(default)]
    pub app_accent: Option<String>,
    pub hook_port: Option<u16>,
    /// A Controller in remote scope must never create local session artifacts
    /// or install local provider hooks. Missing on legacy launch files means
    /// local. The native spawn choke point sets this explicitly as a second
    /// line of defense before `run_host` performs either side effect.
    #[serde(default)]
    pub execution_scope: SessionExecutionScope,
    // Initial PTY size so full-screen TUIs draw at the real terminal size on
    // first paint instead of 80x24 followed by a disruptive resize redraw.
    #[serde(default)]
    pub initial_cols: Option<u16>,
    #[serde(default)]
    pub initial_rows: Option<u16>,
    /// Foreground UI launches can ask provider wrappers to wait briefly until
    /// the first attach client is relaying input. This lets startup terminal
    /// probes run against the real rendering terminal instead of being emitted
    /// before any client exists.
    #[serde(default)]
    pub wait_for_attach: bool,
    /// True when the provider CLI gets Unpeel's Sessions MCP registered
    /// (`--mcp-config` / `UNPEEL_MCP_BIN`). Defaults to true because Read is the
    /// default session role; a blocked project forces this false.
    #[serde(default = "default_mcp_enabled")]
    pub mcp_enabled: bool,
    /// True only for sessions granted Browser Access: the provider CLI gets
    /// Unpeel's Browser MCP registered (second `--mcp-config` /
    /// `UNPEEL_BROWSER_MCP_BIN`). Defaults to false — browser automation can
    /// reach logged-in sites, so access is opt-in per session.
    #[serde(default)]
    pub browser_mcp_enabled: bool,
    /// True only for sessions launched with Computer access available
    /// (Settings ▸ Computer not Off, macOS ≥ 15): the unified MCP server
    /// advertises the `computer` domain to this session. Defaults to false —
    /// computer use drives the user's real screen and input, so the gate is
    /// deliberate; the app-wide Ask/Allow policy still applies per call.
    #[serde(default)]
    pub computer_mcp_enabled: bool,
}

fn default_mcp_enabled() -> bool {
    true
}

/// Host-owned, live runtime state. This is deliberately separate from the
/// session's launch command: a blank terminal may currently be running Claude,
/// but restarting that session must still relaunch the original blank shell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostedSessionRuntime {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_observation: Option<ActiveRuntimeObservation>,
}

/// Manifest projection of an installed App's runtime identity
/// (`crate::app_runtime`). Display fields are the Host's resolved catalog
/// copy: a catalog update lands on the next observation write, and an
/// uninstalled binary simply stops resolving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedAppIdentity {
    pub id: String,
    pub name: String,
    /// `#RRGGBB`, validated when the central catalog is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spinner_tint: Option<String>,
}

impl From<crate::app_runtime::AppRuntimeIdentity> for ObservedAppIdentity {
    fn from(identity: crate::app_runtime::AppRuntimeIdentity) -> Self {
        Self {
            id: identity.app_id.to_string(),
            name: identity.name,
            tint: identity.tint,
            spinner_tint: identity.spinner_tint,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedSessionManifest {
    pub session: SessionInfo,
    pub cwd: String,
    pub state: HostedSessionState,
    pub pid: Option<u32>,
    /// Kernel-reported start time (ms since epoch) of the process `pid`
    /// refers to, captured when the pid was recorded. Kill/reap paths compare
    /// it against the live process's start time before signaling anything —
    /// under agent load the pid counter wraps in well under an hour, so a
    /// stale manifest's pid routinely points at an unrelated live process.
    /// Legacy manifests omit it; identity is then unprovable and no
    /// force-kill may be escalated from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started_at: Option<u64>,
    /// The Host process that owns this Session's PTY loop: a per-process
    /// `__session_host__` or the shared `__pty_core__`. Additive liveness
    /// evidence for the launch window only: a Running manifest whose child
    /// `pid` is still `None` (the PTY has not spawned yet) is alive while
    /// this process provably exists. Never a kill target — the core hosts
    /// many Sessions, so signaling it would take every one of them down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_pid_started_at: Option<u64>,
    pub exit_code: Option<i32>,
    /// Build identity for the session-host binary that created this manifest.
    /// Native clients use it to recommend restarting sessions that survived an
    /// app/host update and are still running old host code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_build_id: Option<String>,
    /// Stable protocol/capability version for restart recommendations. This is
    /// the intentional compatibility lever; do not use build timestamps for UI
    /// restart decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_protocol_version: Option<u32>,
    /// True once any client has written input to the hosted PTY. New sessions
    /// start false so restart can avoid provider resume flags for conversations
    /// that were never actually started. Legacy manifests predate this field
    /// and default true to preserve existing restart behavior.
    #[serde(default = "default_has_been_written_to")]
    pub has_been_written_to: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_transcript_path: Option<String>,
    /// Runtime-owned storage created for this managed Session. The Host only
    /// accepts paths beneath its own Unpeel home and rejects symlink hops
    /// before creating them. Kept provider-neutral so removal never needs to
    /// parse a particular CLI's flags in a frontend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_storage_path: Option<String>,
    /// Provider-verified text markers for a failed precise resume. These are
    /// derived by the runtime adapter from the exact command the Host launched;
    /// clients merely scan the bounded output generation for every marker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume_failure_markers: Vec<String>,
    /// The process currently occupying this PTY when it is a recognized agent
    /// runtime. Observation enriches presentation and activity; it never
    /// rewrites `session.command` or grants launch/resume capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<HostedSessionRuntime>,
    /// Installed Unpeel App identity for this session, Host-resolved so
    /// clients render App branding (name, tint, `kind: app`) as data without
    /// a compiled catalog entry. Stamped at spawn when the launch command is
    /// an installed App's binary, then maintained by foreground observation
    /// (an App id in `runtime.current_observation` resolves here; a built-in
    /// runtime or a proven return to the shell clears it). Identity and
    /// presentation only: it grants no Busy authority — an App's
    /// busy/idle/attention comes from its own lifecycle reporting through the
    /// hook port — and never enables launch/resume verbs. Legacy manifests
    /// predate the field and default to none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_app: Option<ObservedAppIdentity>,
    /// Monotonic generation of the stable runtime launch inside this hosted
    /// PTY. Direct provider sessions begin at one; blank terminals remain at
    /// zero. An in-place agent restart advances this only after the relaunch
    /// bytes were accepted by the PTY, allowing clients to discard hook/activity
    /// state that belonged to the preceding process without replacing the
    /// Session or terminal identity.
    #[serde(default)]
    pub runtime_launch_generation: u64,
    /// True after Resume Agent has submitted a new managed launch but before
    /// the Host has observed that runtime or its completion wrapper has
    /// definitively returned to the shell. This Host-owned latch prevents a
    /// second Controller from injecting another launch during the short
    /// post-submit shell/observer window. Legacy manifests default false.
    #[serde(default)]
    pub runtime_launch_pending: bool,
    /// Wall-clock timestamp for the current stable runtime launch generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_launched_at: Option<u64>,
    /// Absolute output-stream offset at which this runtime generation began.
    /// In-place restarts retain output.bin, so resume-failure readers must
    /// scan from this boundary instead of the file head (which belongs to an
    /// older process generation).
    #[serde(default)]
    pub runtime_launch_output_offset: u64,
    /// Launch-time Sessions MCP domain grant. This is separate from
    /// `mcp_client_registered`: a blank terminal does not receive automatic
    /// provider configuration, but a CLI the user configured manually still
    /// needs the Session's bounded MCP grant. `None` is the old-manifest form
    /// and falls back to the historical registration bit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_enabled: Option<bool>,
    /// Launch-time Browser MCP domain grant; see `mcp_enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_mcp_enabled: Option<bool>,
    /// Launch-time Computer MCP domain grant; see `mcp_enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_mcp_enabled: Option<bool>,
    /// Whether Unpeel automatically registered the unified MCP client with the
    /// provider CLI for this runtime launch. This is evidence about provider
    /// setup, not the domain authorization bit: blank terminals keep this false
    /// while a manually configured CLI uses `mcp_enabled` above. Legacy
    /// manifests predate this field and default false.
    #[serde(default)]
    pub mcp_client_registered: bool,
    /// Whether automatic provider setup included the unified MCP client while
    /// the Browser domain was enabled. See `mcp_client_registered`; the domain
    /// itself is authorized by `browser_mcp_enabled` and may also be reached by
    /// a manually configured client. Legacy manifests default false.
    #[serde(default)]
    pub browser_client_registered: bool,
    /// Whether automatic provider setup included the unified MCP client while
    /// the Computer domain was enabled. Domain authorization is stored
    /// separately in `computer_mcp_enabled`.
    #[serde(default)]
    pub computer_client_registered: bool,
    /// True while the session's visible screen looks like an agent-drawn select
    /// menu waiting for a keyboard choice (Claude/Codex numbered prompts). These
    /// menus fire no lifecycle hook, so the host scans the rendered viewport
    /// (see `crate::menu_prompt`) and edge-writes this flag; native clients turn
    /// it into the sidebar attention badge. Legacy manifests predate the field
    /// and default false.
    #[serde(default)]
    pub menu_prompt_active: bool,
    /// Wall-clock ms when the parsed screen TEXT last changed, written by the
    /// same viewport scan (coalesced to at most one manifest write per ~2s).
    /// This is the "really doing something" signal: idle repaint loops that
    /// redraw identical content (grok's idle animation) advance output.bin's
    /// size and mtime forever but never advance this stamp. Clients prefer it
    /// over output.bin for busy heuristics and activity recency, falling back
    /// to output.bin on manifests from older hosts (where it is None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_changed_at: Option<u64>,
    /// Local service URLs this session's processes currently serve, in the
    /// browsable sense: printed on the session's screen with a loopback
    /// authority and an explicit port, and answering the host's HTTP probe
    /// like a web page (see `crate::local_urls`). Edge-written by the same
    /// viewport scan as `menu_prompt_active`; entries disappear when the
    /// server stops answering. Legacy manifests predate the field and
    /// default empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detected_local_urls: Vec<String>,
    /// Input-shaping terminal modes currently active in the parsed VT (alt
    /// screen, mouse tracking, bracketed paste, …), edge-written by the same
    /// viewport scan. Attach clients re-assert these after their replay
    /// reset: the sequences that established them usually precede the
    /// replayed output tail, so without this a reattached full-screen mouse
    /// app renders but no longer receives wheel/click reports. Omitted while
    /// everything matches reset defaults; legacy manifests predate the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_modes: Option<crate::terminal_viewport::TerminalModeState>,
    #[serde(default)]
    pub heartbeat_at: u64,
    /// Defaulted like every other field here: a manifest that is missing
    /// one must still load. Without this, a single absent key makes the
    /// whole session invisible to every frontend rather than merely
    /// undated — the worst possible failure for a user's existing work.
    #[serde(default)]
    pub updated_at: u64,
}

impl HostedSessionManifest {
    /// Effective launch-time domain grants. Older manifests predate the
    /// explicit grant fields and therefore retain their shipped behavior by
    /// falling back to the corresponding registration evidence.
    pub fn sessions_mcp_enabled(&self) -> bool {
        self.mcp_enabled.unwrap_or(self.mcp_client_registered)
    }

    pub fn browser_mcp_enabled(&self) -> bool {
        self.browser_mcp_enabled
            .unwrap_or(self.browser_client_registered)
    }

    pub fn computer_mcp_enabled(&self) -> bool {
        self.computer_mcp_enabled
            .unwrap_or(self.computer_client_registered)
    }
}

fn default_has_been_written_to() -> bool {
    true
}

/// Returns the live observed runtime ID without coupling callers to the
/// manifest's nested diagnostic shape.
pub fn active_runtime_id(manifest: &HostedSessionManifest) -> Option<&str> {
    if manifest.state != HostedSessionState::Running {
        return None;
    }
    manifest
        .runtime
        .as_ref()?
        .current_observation
        .as_ref()
        .map(|observation| observation.runtime_id.as_str())
        .filter(|runtime_id| !runtime_id.is_empty())
}

/// Minimum spacing between `screen_changed_at` manifest writes: a session
/// streaming real content changes its screen every scan tick, and stamping
/// each one would rewrite the manifest at the scan rate.
const SCREEN_STAMP_COALESCE_MS: u64 = 2_000;

/// Detects material screen changes for `screen_changed_at`: a change is a
/// different parsed-text hash, so an idle TUI repainting identical content
/// (cursor churn, synchronized-update frames) observes nothing. Returns the
/// stamp to persist when a write is due — immediately for a change after
/// quiet, at most every `SCREEN_STAMP_COALESCE_MS` while changes keep
/// coming, with the trailing stamp caught on a later tick.
#[derive(Default)]
struct ScreenChangeTracker {
    last_hash: Option<u64>,
    changed_at: Option<u64>,
    written_at: Option<u64>,
    last_write_ms: u64,
}

impl ScreenChangeTracker {
    fn observe(&mut self, screen: &str, now_ms: u64) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        screen.hash(&mut hasher);
        let hash = hasher.finish();
        if self.last_hash != Some(hash) {
            self.last_hash = Some(hash);
            self.changed_at = Some(now_ms);
        }
        let pending = self.changed_at.filter(|c| Some(*c) != self.written_at)?;
        if self.written_at.is_some()
            && now_ms.saturating_sub(self.last_write_ms) < SCREEN_STAMP_COALESCE_MS
        {
            return None;
        }
        self.written_at = Some(pending);
        self.last_write_ms = now_ms;
        Some(pending)
    }
}

/// Edge-trigger and debounce live runtime observations before they reach the
/// manifest. The tracker carries a known runtime through temporary unknown
/// child jobs, but a verified return to the hosted shell clears it at once.
#[derive(Default)]
struct RuntimeObservationTracker {
    current: Option<ActiveRuntimeObservation>,
    consecutive_misses: u8,
}

impl RuntimeObservationTracker {
    /// `Some(next)` means persist an edge; the inner `None` clears the nested
    /// runtime record. `None` means the manifest already represents this state.
    fn observe(
        &mut self,
        observation: Option<ActiveRuntimeObservation>,
        returned_to_owned_shell: bool,
    ) -> Option<Option<ActiveRuntimeObservation>> {
        match observation {
            Some(observation) => {
                self.consecutive_misses = 0;
                if self.current.as_ref() == Some(&observation) {
                    return None;
                }
                self.current = Some(observation.clone());
                Some(Some(observation))
            }
            None if self.current.is_none() => {
                self.consecutive_misses = 0;
                None
            }
            None => {
                self.consecutive_misses = self.consecutive_misses.saturating_add(1);
                if !returned_to_owned_shell
                    && self.consecutive_misses < SESSION_RUNTIME_CLEAR_MISSES
                {
                    return None;
                }
                self.current = None;
                self.consecutive_misses = 0;
                Some(None)
            }
        }
    }
}

/// Identity of the running host executable: mtime + size of the binary,
/// the same stamp every hosted manifest records as `host_build_id`. The
/// serve worker and machine service publish it so a Controller can detect
/// an app/service version skew after an in-place update.
/// Identity of the binary THIS process is running, captured once per process.
/// Status records are rewritten for as long as the process lives; reading the
/// executable on every write would make them track a rebuilt file on disk
/// instead of the image actually running, hiding exactly the app/service
/// skew the stale-service restart exists to catch.
pub fn current_host_build_id() -> Option<String> {
    static BUILD_ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILD_ID.get_or_init(read_current_host_build_id).clone()
}

/// Build identity of an arbitrary Host binary on disk, in the same
/// mtime:length shape `current_host_build_id` reports for the running one.
/// The serve supervisor compares the adopted core's id against the binary it
/// would launch to decide whether a core-to-core takeover is due.
/// Kernel start time of `pid` for record fields (`pid_started_at`); public
/// for supervisors that write records for processes they observe.
pub fn recorded_process_start_time_ms(pid: u32) -> Option<u64> {
    process_start_time_ms(pid)
}

pub fn host_build_id_for(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let modified = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(format!(
        "{}.{:09}:{}",
        modified.as_secs(),
        modified.subsec_nanos(),
        metadata.len()
    ))
}

fn read_current_host_build_id() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let metadata = fs::metadata(exe).ok()?;
    let modified = metadata.modified().ok()?;
    let modified = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(format!(
        "{}.{:09}:{}",
        modified.as_secs(),
        modified.subsec_nanos(),
        metadata.len()
    ))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostedSessionState {
    Running,
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionHostCommand {
    Write {
        data: String,
        /// Optional client-generated idempotency key for one logical input
        /// send. A remote controller may deliver the same keystroke over two
        /// transports when the first delivery is ambiguous (a WebSocket send
        /// that times out after the bytes were already on the wire, then an
        /// HTTP retry). Both carry the same `write_id`; the host applies the
        /// first and drops the duplicate so input is not doubled. Absent for
        /// local writers (attach, MCP), which never race two transports.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        write_id: Option<String>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    StreamOutput {
        offset: u64,
        /// True for clients backed by a REAL answering terminal (the native
        /// attach client's Ghostty surface). While one is connected the host
        /// stops answering terminal probes itself (DA1 excluded — that stays
        /// host-answered always, see OutputQueryScanner).
        #[serde(default)]
        answers_queries: bool,
    },
    StreamInput,
    ViewportSnapshot {
        cols: u16,
        rows: u16,
        #[serde(default)]
        scroll_offset_rows: u32,
        #[serde(default)]
        viewport_rows: Option<u16>,
    },
    Ping,
    /// Snapshot attach (additive, 2026-09-02): reply with the Host's resident
    /// VT state as VT bytes plus the exact journal offset it was rendered
    /// at, so a client can apply the snapshot and stream from that offset
    /// instead of replaying a raw journal tail. The reply is one JSON line
    /// (`{"ok":true,"snapshot":{"journal_offset","cols","rows","bytes_len"}}`)
    /// followed by exactly `bytes_len` raw bytes. Hosts that predate this
    /// command reject the unknown variant and close without a reply, which
    /// is the client's fallback signal.
    Snapshot,
    /// Resume the stable, known agent launch inside this same hosted PTY.
    /// The Host derives the command itself from its manifest/markers, verifies
    /// the live foreground owner, and never promotes a passively observed
    /// runtime in a blank terminal.
    RestartAgent {
        /// Compare-and-swap guard against two concurrent callers each
        /// injecting a resume line. A successful restart advances the
        /// manifest generation, making every other request prepared from the
        /// preceding generation stale.
        expected_generation: u64,
    },
    /// Resume the saved runtime recipe only when the owned shell freshly owns
    /// the PTY foreground. Kept separate on the wire so Controllers can
    /// capability-gate the safe operation while old `restart_agent` requests
    /// continue to decode.
    ResumeAgent {
        expected_generation: u64,
    },
    Kill,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHostResponse {
    pub ok: bool,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<TerminalViewportSnapshot>,
}

/// Header line of a [`SessionHostCommand::Snapshot`] reply; the VT bytes
/// follow the newline verbatim. `journal_offset` is the lifetime output
/// offset the snapshot corresponds to (the first byte a subscriber must
/// stream from).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotVtHeader {
    pub journal_offset: u64,
    pub cols: u16,
    pub rows: u16,
    pub bytes_len: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotVtReply {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    snapshot: Option<SnapshotVtHeader>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOutputChunk {
    pub data: Vec<u8>,
    pub next_offset: u64,
    pub exited: bool,
    pub exists: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct OutputRetentionState {
    version: u32,
    retained_from: u64,
}

impl OutputRetentionState {
    const VERSION: u32 = 1;

    fn at(retained_from: u64) -> Self {
        Self {
            version: Self::VERSION,
            retained_from,
        }
    }
}

fn mark_input_written(session_id: &str, has_been_written_to: &Arc<AtomicBool>) {
    if !has_been_written_to.swap(true, Ordering::Relaxed) {
        let _ = update_manifest_session(session_id, |manifest| {
            manifest.has_been_written_to = true;
        });
    }
}

fn maybe_auto_title_from_input(
    session_id: &str,
    data: &[u8],
    title_buffer: &Arc<Mutex<String>>,
    title_done: &Arc<AtomicBool>,
) {
    if title_done.load(Ordering::Relaxed) {
        return;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let candidate = extract_submitted_prompt(&mut title_buffer.lock().unwrap(), text);
    if let Some(candidate) = candidate {
        if apply_manifest_auto_title(session_id, &candidate) {
            title_done.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
fn read_input_stream_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, String> {
    let mut header = [0u8; SESSION_INPUT_STREAM_FRAME_HEADER_BYTES];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(format!("Failed to read input stream frame: {error}")),
    }

    let len = u32::from_be_bytes(header) as usize;
    if len > SESSION_INPUT_STREAM_MAX_FRAME_BYTES {
        return Err(format!("Input stream frame too large: {len} bytes"));
    }

    let mut data = vec![0u8; len];
    if len > 0 {
        reader
            .read_exact(&mut data)
            .map_err(|e| format!("Failed to read input stream payload: {e}"))?;
    }
    Ok(Some(data))
}

pub(crate) struct HostRuntime {
    master: Box<dyn MasterPty + Send>,
    /// Non-blocking master fd behind a writer that restores blocking
    /// `write_all` for transient command threads (see `session_io`).
    writer: session_io::PtyWriter,
    child: Box<dyn Child + Send + Sync>,
    /// Last grid successfully applied to the kernel PTY. Multiple renderers
    /// can report the same logical resize (the phone's direct resize plus the
    /// Mac letterbox surface); suppress duplicates here so one user action
    /// produces one SIGWINCH and one TUI redraw.
    pty_cols: u16,
    pty_rows: u16,
    /// Executable originally selected as this PTY's persistent login shell.
    /// The session-leader PID and start time survive `exec`, so Resume Agent
    /// must also re-verify this executable identity before writing input.
    shell_executable: PathBuf,
    /// Exact last runtime identity observed by this Host. Unlike a fresh
    /// catalog lookup, this survives an agent being stopped/backgrounded and
    /// then execing or renaming itself. Resume Agent uses its PID/start/PGID
    /// until a complete session scan proves the old job is gone.
    last_runtime_observation: Option<ActiveRuntimeObservation>,
    recent_write_ids: RecentWriteIds,
}

#[cfg(unix)]
fn signal_owned_foreground_process_group(runtime: &HostRuntime, signal: i32) {
    let (Some(child_pid), Some(foreground_pgid)) = (
        runtime.child.process_id(),
        runtime.master.process_group_leader(),
    ) else {
        return;
    };
    let Ok(child_sid) = i32::try_from(child_pid) else {
        return;
    };
    if child_sid <= 1 || foreground_pgid <= 1 {
        return;
    }

    // portable-pty calls setsid() in the owned child before exec. Interactive
    // shells put foreground commands (for example `cat`) in a different
    // process group, so kill(-child_pid) misses the process that still owns
    // the slave PTY. Only signal the foreground group when the kernel proves
    // that it belongs to this exact child-owned session.
    if unsafe { libc::getsid(foreground_pgid) } == child_sid {
        let _ = unsafe { libc::kill(-foreground_pgid, signal) };
    }
}

#[cfg(unix)]
fn terminate_hosted_runtime(runtime: &mut HostRuntime) {
    signal_owned_foreground_process_group(runtime, libc::SIGTERM);
    // Keep the owned session leader unreaped through foreground escalation.
    // Even if TERM has already made it a zombie, the kernel cannot reuse its
    // PID while this Child remains unwaited; the second PTY lookup therefore
    // still verifies against the same session identity rather than a recycled
    // process. Give the foreground a short graceful window, then re-read and
    // re-verify the PTY's current foreground group before forcing it down.
    thread::sleep(Duration::from_millis(50));
    signal_owned_foreground_process_group(runtime, libc::SIGKILL);

    if let Err(error) = runtime.child.kill() {
        // A concurrently-exited child is already the desired outcome. Keep
        // Kill best-effort and protocol-compatible; the output loop below is
        // independently interrupted and will publish the exited manifest.
        if !matches!(runtime.child.try_wait(), Ok(Some(_))) {
            log::warn!("Failed to terminate hosted child: {error}");
        }
    }
}

#[cfg(unix)]
enum RestartAgentForeground {
    OwnedShell,
    Runtime,
}

/// A marker moved out of its canonical name while an in-place relaunch is
/// committed. Moving, rather than deleting, lets a failed PTY write restore
/// the exact old bytes without racing a newly-created marker at that name.
#[cfg(unix)]
struct StagedRestartMarker {
    original: PathBuf,
    staged: PathBuf,
    resolved: bool,
}

#[cfg(unix)]
impl StagedRestartMarker {
    fn commit(mut self) {
        let _ = fs::remove_file(&self.staged);
        self.resolved = true;
    }

    fn rollback(mut self) {
        self.rollback_unresolved();
        self.resolved = true;
    }

    fn rollback_unresolved(&self) {
        if !self.original.exists() {
            let _ = fs::rename(&self.staged, &self.original);
        } else {
            // A new-generation writer already published a replacement. Never
            // overwrite it with the seed/marker from the process we stopped.
            let _ = fs::remove_file(&self.staged);
        }
    }
}

#[cfg(unix)]
impl Drop for StagedRestartMarker {
    fn drop(&mut self) {
        if !self.resolved {
            self.rollback_unresolved();
        }
    }
}

#[cfg(unix)]
fn stage_restart_marker(
    path: PathBuf,
    generation: u64,
) -> Result<Option<StagedRestartMarker>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("Invalid restart marker path {}", path.display()))?;
    let staged = path.with_file_name(format!(
        ".{file_name}.restart-{}-{generation}",
        std::process::id()
    ));
    let _ = fs::remove_file(&staged);
    fs::rename(&path, &staged)
        .map_err(|e| format!("Failed to stage {} for agent restart: {e}", path.display()))?;
    Ok(Some(StagedRestartMarker {
        original: path,
        staged,
        resolved: false,
    }))
}

#[cfg(unix)]
fn classify_restart_agent_foreground(
    runtime: &HostRuntime,
    manifest: &HostedSessionManifest,
    expected_runtime_id: &str,
) -> Result<RestartAgentForeground, String> {
    let child_pid = runtime
        .child
        .process_id()
        .ok_or("Session host no longer has an owned shell process")?;
    if manifest.pid != Some(child_pid) || manifest_pid_identity(manifest) != PidIdentity::Matches {
        return Err("Session host process identity could not be verified".into());
    }
    let child_sid = i32::try_from(child_pid).map_err(|_| "Invalid session leader pid")?;
    let foreground_pgid = runtime
        .master
        .process_group_leader()
        .ok_or("Session terminal has no foreground process group")?;
    if foreground_pgid <= 1 || unsafe { libc::getsid(foreground_pgid) } != child_sid {
        return Err("Terminal foreground is outside the owned session".into());
    }
    let started_at = manifest
        .pid_started_at
        .ok_or("Session host has no verifiable process start time")?;
    let manifest_observation = manifest
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.current_observation.as_ref());
    let prior_runtime_observation = runtime
        .last_runtime_observation
        .as_ref()
        .or(manifest_observation);
    let process_inspection = crate::runtime_observer::inspect_owned_session_processes(
        child_pid,
        started_at,
        &runtime.shell_executable,
        expected_runtime_id,
        prior_runtime_observation,
    )
    .ok_or("Session process membership could not be verified")?;
    if !process_inspection.shell_executable_matches {
        return Err("Session leader is no longer the owned shell executable".into());
    }
    // A fresh catalog miss cannot erase a prior observed process identity:
    // that process may have execed/renamed to an unrecognized binary or left
    // descendants in its old job group. Only exact PID/start + PGID absence
    // proves it disappeared.
    if let Some(prior) = prior_runtime_observation {
        match process_inspection.prior_runtime_present {
            Some(true) if prior.runtime_id.eq_ignore_ascii_case(expected_runtime_id) => {
                return Ok(RestartAgentForeground::Runtime);
            }
            Some(true) => {
                return Err(format!(
                    "Refusing to resume {expected_runtime_id}: terminal foreground is {} or its observed job is still running",
                    prior.runtime_id
                ));
            }
            Some(false) => {}
            None => {
                return Err("Prior runtime process identity could not be verified as gone".into());
            }
        }
    }
    // Any catalog-recognized runtime anywhere in the owned kernel session is
    // an immediate blocker, including a different stopped/background agent.
    // The leader may still be the startup shell's `-c` invocation while the
    // initial runtime is active, so check jobs before the final shell argv.
    if let Some(observation) = process_inspection.recognized_runtime_observation.as_ref() {
        if observation
            .runtime_id
            .eq_ignore_ascii_case(expected_runtime_id)
        {
            return Ok(RestartAgentForeground::Runtime);
        }
        return Err(format!(
            "Refusing to resume {expected_runtime_id}: terminal foreground is {} or its observed job is still running",
            observation.runtime_id
        ));
    }
    if !process_inspection.shell_invocation_matches {
        return Err("Session leader is no longer the owned interactive login shell".into());
    }
    let observation =
        crate::runtime_observer::observe_foreground_runtime(child_pid, started_at, foreground_pgid);

    if let Some(observation) = observation {
        if !observation
            .runtime_id
            .eq_ignore_ascii_case(expected_runtime_id)
        {
            return Err(format!(
                "Refusing to restart {expected_runtime_id}: terminal foreground is {}",
                observation.runtime_id
            ));
        }
        // A group containing the session leader cannot be stopped without
        // also killing the shell/terminal identity we are required to keep.
        if foreground_pgid == child_sid {
            return Err(format!(
                "Refusing to restart {expected_runtime_id}: its process group is not isolated from the owned shell"
            ));
        }
        return Ok(RestartAgentForeground::Runtime);
    }

    if foreground_pgid == child_sid {
        return Ok(RestartAgentForeground::OwnedShell);
    }
    Err(format!(
        "Refusing to restart {expected_runtime_id}: an unrecognized foreground job owns the terminal"
    ))
}

#[cfg(unix)]
fn wait_for_owned_shell(
    runtime: &HostRuntime,
    manifest: &HostedSessionManifest,
    expected_runtime_id: &str,
    timeout: Duration,
) -> Result<bool, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if owned_shell_is_foreground(runtime, manifest, expected_runtime_id)? {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(false)
}

#[cfg(unix)]
fn owned_shell_is_foreground(
    runtime: &HostRuntime,
    manifest: &HostedSessionManifest,
    expected_runtime_id: &str,
) -> Result<bool, String> {
    Ok(matches!(
        classify_restart_agent_foreground(runtime, manifest, expected_runtime_id)?,
        RestartAgentForeground::OwnedShell
    ))
}

#[cfg(unix)]
fn require_fresh_owned_shell(
    runtime: &HostRuntime,
    manifest: &HostedSessionManifest,
    expected_runtime_id: &str,
) -> Result<u32, String> {
    let child_pid = runtime
        .child
        .process_id()
        .ok_or("Session host no longer has an owned shell process")?;
    match classify_restart_agent_foreground(runtime, manifest, expected_runtime_id)? {
        RestartAgentForeground::OwnedShell => Ok(child_pid),
        RestartAgentForeground::Runtime => Err(format!(
            "Refusing to resume {expected_runtime_id}: the agent is still running"
        )),
    }
}

#[cfg(unix)]
fn runtime_launch_completion_path(session_id: &str) -> PathBuf {
    session_dir(session_id).join(".runtime-launch-complete")
}

#[cfg(unix)]
fn remove_runtime_launch_completion_marker(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let _ = fs::remove_dir(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// A launch may trust the completion marker only if no older marker survived.
/// Cleanup after an observed completion remains best-effort, but pre-launch
/// preparation fails closed: otherwise an undeletable stale directory could
/// clear the new pending latch before the provider process is ever observed.
#[cfg(unix)]
fn prepare_runtime_launch_completion_marker(path: &Path) -> Result<(), String> {
    remove_runtime_launch_completion_marker(path);
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to verify runtime launch completion marker {}: {error}",
            path.display()
        )),
        Ok(_) => Err(format!(
            "Failed to clear stale runtime launch completion marker {}",
            path.display()
        )),
    }
}

/// Append a Host-owned completion marker without changing the provider's
/// exit status in the user's live shell. Runtime observation normally clears
/// the pending latch first; this marker covers a launch that returns/fails too
/// quickly to appear in the 300ms process scan.
#[cfg(unix)]
fn runtime_launch_completion_command(
    shell_family: ShellFamily,
    runtime_command: &str,
    completion_path: &Path,
) -> String {
    let path = completion_path.to_string_lossy();
    match shell_family {
        ShellFamily::Posix => format!(
            "{runtime_command}; __unpeel_resume_status=$?; /bin/mkdir {}; \
             (exit \"$__unpeel_resume_status\")",
            shared::shell_quote(&path)
        ),
        ShellFamily::Fish => format!(
            "{runtime_command}; set -l __unpeel_resume_status $status; /bin/mkdir {}; \
             /bin/sh -c \"exit $__unpeel_resume_status\"",
            fish_single_quote(&path)
        ),
        ShellFamily::Other => runtime_command.to_string(),
    }
}

#[cfg(unix)]
fn clear_runtime_launch_pending(
    session_id: &str,
    generation_before_submit: u64,
    pending_runtime_generation: &AtomicU64,
    pending_generation: u64,
) {
    let _ = update_manifest_session(session_id, |manifest| {
        if manifest.runtime_launch_generation == generation_before_submit {
            manifest.runtime_launch_pending = false;
        }
    });
    let _ = pending_runtime_generation.compare_exchange(
        pending_generation,
        0,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

#[cfg(unix)]
fn resume_agent_in_place(
    session_id: &str,
    runtime: &Arc<Mutex<HostRuntime>>,
    broadcaster: &Arc<Mutex<OutputBroadcaster>>,
    runtime_generation: &AtomicU64,
    pending_runtime_generation: &AtomicU64,
    expected_generation: u64,
) -> Result<(), String> {
    let manifest =
        load_manifest(session_id).ok_or_else(|| format!("no manifest for {session_id}"))?;
    if manifest.state != HostedSessionState::Running {
        return Err(format!("session {session_id} is not running"));
    }
    if manifest.runtime_launch_generation != expected_generation {
        return Err(format!(
            "Agent restart generation changed (expected {expected_generation}, current {})",
            manifest.runtime_launch_generation
        ));
    }
    if manifest.runtime_launch_pending || pending_runtime_generation.load(Ordering::Acquire) != 0 {
        return Err("An agent resume launch is pending".into());
    }
    let stable_command = manifest.session.command.trim();
    // The Host is the final capability authority. Presentation and Controller
    // gates are advisory; a direct/local caller must not turn a stopped-only
    // Resume recipe into a same-PTY Resume Agent operation.
    if !crate::resume::can_resume_agent(stable_command, None) {
        return Err("Agent restart requires a nonblank, known resumable launch command".into());
    }
    let expected_runtime_id = integrations::runtime_for_command(stable_command)
        .map(|runtime| runtime.legacy_slug.as_str())
        .ok_or("Agent restart requires a known runtime command alias")?;
    // Prove eligibility before runtime-support installers or launch
    // preparation can write provider config. Derive quoting from the actual
    // shell this Host spawned, not the user's current SHELL/default resolver:
    // that preference can change while this long-lived PTY survives.
    let live_shell = {
        let guard = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        require_fresh_owned_shell(&guard, &manifest, expected_runtime_id)?;
        guard.shell_executable.to_string_lossy().into_owned()
    };
    let live_shell_family = shell_family(&live_shell);
    if live_shell_family == ShellFamily::Other {
        // Provider startup adapters are POSIX programs, but there is no one
        // quoting syntax that can safely transport an arbitrary program
        // through every possible interactive shell. Refuse before support
        // installation or launch preparation; no relaunch command can be
        // safely submitted through an unknown shell family.
        return Err(format!(
            "Refusing to resume {expected_runtime_id}: this Session's login shell is not supported for in-place relaunch"
        ));
    }

    // Derive everything that can fail before touching the current process.
    let relaunch_command = crate::session_ops::relaunch_command(
        session_id,
        crate::session_ops::RelaunchMode::Restart { force_fresh: false },
    )?;
    if integrations::has_runtime_support_installer(expected_runtime_id) {
        integrations::install_runtime_support(expected_runtime_id)?;
    }
    let mcp_enabled = manifest.sessions_mcp_enabled();
    let browser_mcp_enabled = manifest.browser_mcp_enabled();
    let computer_mcp_enabled = manifest.computer_mcp_enabled();
    integrations::prepare_runtime_launch(
        expected_runtime_id,
        mcp_enabled,
        browser_mcp_enabled,
        computer_mcp_enabled,
    )?;
    let automatic_mcp_registration = integrations::automatic_mcp_registration(
        expected_runtime_id,
        &relaunch_command,
        mcp_enabled,
        browser_mcp_enabled,
        computer_mcp_enabled,
    );
    let startup_command = integrations::startup_command(
        expected_runtime_id,
        &relaunch_command,
        mcp_enabled,
        browser_mcp_enabled,
        computer_mcp_enabled,
    );

    let guard = runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    require_fresh_owned_shell(&guard, &manifest, expected_runtime_id)?;

    // Do not invert the output reader's broadcaster → runtime lock order.
    // The restart serializer still excludes user input while we briefly drop
    // the runtime guard to snapshot the generation's exact stream boundary.
    drop(guard);
    let launch_output_offset = broadcaster
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .next_offset;
    let mut guard = runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !wait_for_owned_shell(
        &guard,
        &manifest,
        expected_runtime_id,
        Duration::from_millis(250),
    )? {
        return Err("Owned shell lost the terminal before agent relaunch".into());
    }

    let next_generation = manifest.runtime_launch_generation.saturating_add(1).max(1);
    let scoped_runtime_command =
        runtime_generation_scoped_command(live_shell_family, &startup_command, next_generation);
    let completion_path = runtime_launch_completion_path(session_id);
    prepare_runtime_launch_completion_marker(&completion_path)?;
    let runtime_command = runtime_launch_completion_command(
        live_shell_family,
        &scoped_runtime_command,
        &completion_path,
    );
    let hook_seed = stage_restart_marker(
        session_dir(session_id).join("last-hook-event.json"),
        next_generation,
    )?;
    // Capture before submission: a provider can emit its first hook/output
    // synchronously, and those events belong to this new generation.
    let launched_at = current_timestamp_ms();
    // A shell can own the foreground while still holding a half-typed line.
    // Put Ctrl-U in the same final write as the command: every failure before
    // this point is then a true no-input no-op, while Ctrl-U edits the shell's
    // line buffer without generating SIGINT.
    let mut payload = Vec::with_capacity(runtime_command.len().saturating_add(2));
    payload.push(b'\x15');
    payload.extend_from_slice(runtime_command.as_bytes());
    payload.push(b'\r');
    // Marker staging can touch disk. Re-read the PTY foreground immediately
    // before the irreversible submission so an autonomous foreground handoff
    // cannot turn Resume Agent into input for a live or unknown job.
    let ownership_error = match owned_shell_is_foreground(&guard, &manifest, expected_runtime_id) {
        Ok(true) => None,
        Ok(false) => Some("Owned shell lost the terminal before agent relaunch".to_string()),
        Err(error) => Some(error),
    };
    if let Some(error) = ownership_error {
        if let Some(marker) = hook_seed {
            marker.rollback();
        }
        return Err(error);
    }

    // Publish the Host-owned latch before the irreversible PTY write. A
    // second raw socket Controller waits on the transaction lock and then
    // observes either this manifest bit or the in-memory generation. If the
    // disk write fails, no terminal input has occurred.
    let pending_manifest = update_manifest_session(session_id, |current| {
        if current.state == HostedSessionState::Running
            && current.runtime_launch_generation == expected_generation
            && !current.runtime_launch_pending
        {
            current.runtime_launch_pending = true;
        }
    })?
    .ok_or_else(|| format!("no manifest for {session_id}"))?;
    if pending_manifest.runtime_launch_generation != expected_generation
        || !pending_manifest.runtime_launch_pending
    {
        if let Some(marker) = hook_seed {
            marker.rollback();
        }
        return Err("Agent restart generation changed before relaunch submission".into());
    }
    if pending_runtime_generation
        .compare_exchange(0, next_generation, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        clear_runtime_launch_pending(
            session_id,
            expected_generation,
            pending_runtime_generation,
            next_generation,
        );
        if let Some(marker) = hook_seed {
            marker.rollback();
        }
        return Err("An agent resume launch is pending".into());
    }

    // The manifest publication above can touch disk. Re-prove every process
    // invariant immediately before submitting Ctrl-U + command as one write.
    let ownership_error = match owned_shell_is_foreground(&guard, &manifest, expected_runtime_id) {
        Ok(true) => None,
        Ok(false) => Some("Owned shell lost the terminal before agent relaunch".to_string()),
        Err(error) => Some(error),
    };
    if let Some(error) = ownership_error {
        clear_runtime_launch_pending(
            session_id,
            expected_generation,
            pending_runtime_generation,
            next_generation,
        );
        if let Some(marker) = hook_seed {
            marker.rollback();
        }
        return Err(error);
    }
    if let Err(error) = guard
        .writer
        .write_all(&payload)
        .and_then(|_| guard.writer.flush())
    {
        clear_runtime_launch_pending(
            session_id,
            expected_generation,
            pending_runtime_generation,
            next_generation,
        );
        remove_runtime_launch_completion_marker(&completion_path);
        if let Some(marker) = hook_seed {
            marker.rollback();
        }
        return Err(format!("Failed to submit agent relaunch command: {error}"));
    }
    drop(guard);

    let manifest_update = update_manifest_session(session_id, |manifest| {
        manifest.session.command = relaunch_command.clone();
        manifest.resume_failure_markers =
            crate::resume::resume_failure_markers(&relaunch_command).unwrap_or_default();
        manifest.runtime = None;
        manifest.runtime_launch_generation = next_generation;
        manifest.runtime_launch_pending = true;
        manifest.runtime_launched_at = Some(launched_at);
        manifest.runtime_launch_output_offset = launch_output_offset;
        manifest.mcp_client_registered = automatic_mcp_registration.sessions;
        manifest.browser_client_registered = automatic_mcp_registration.browser;
        manifest.computer_client_registered = automatic_mcp_registration.computer;
        manifest.menu_prompt_active = false;
    });
    // Wake/reset the observer only after the clearing manifest write. If this
    // edge came first, a fast new runtime could be observed and then erased by
    // our commit while the tracker believed it was already persisted. Also
    // reset it after a failed commit: the PTY submission is already
    // irreversible, and a fresh observation is safer than retaining the old
    // process identity in memory.
    runtime_generation.fetch_add(1, Ordering::AcqRel);
    // Once the relaunch bytes were accepted, old-generation markers must
    // never be restored, even if the subsequent manifest commit failed. Drop
    // the staged tombstones on every post-submit path so a rare I/O failure
    // does not leave private state accumulating beside the canonical files.
    if let Some(marker) = hook_seed {
        marker.commit();
    }
    match manifest_update {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err("Agent relaunched, but its session manifest disappeared".into()),
        Err(error) => Err(format!(
            "Agent relaunched, but its session manifest could not be updated: {error}"
        )),
    }
}

#[cfg(unix)]
struct OutputStreamSubscriber {
    id: u64,
    /// Client is backed by an answering terminal (see StreamOutput).
    answers_queries: bool,
    tx: mpsc::Sender<SessionOutputChunk>,
    /// Bytes enqueued to `tx` but not yet drained by the forwarder. Shared with
    /// the forwarder, which subtracts each chunk's length as it receives it.
    buffered: Arc<AtomicUsize>,
}

#[cfg(unix)]
struct RecentOutputChunk {
    start_offset: u64,
    data: Vec<u8>,
}

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct OutputBroadcaster {
    next_subscriber_id: u64,
    next_offset: u64,
    exited: bool,
    recent_bytes: usize,
    recent: VecDeque<RecentOutputChunk>,
    subscribers: Vec<OutputStreamSubscriber>,
}

#[cfg(unix)]
impl OutputBroadcaster {
    fn at_offset(next_offset: u64) -> Self {
        Self {
            next_offset,
            ..Self::default()
        }
    }

    fn subscribe(
        &mut self,
        offset: u64,
        tx: mpsc::Sender<SessionOutputChunk>,
        answers_queries: bool,
    ) -> Option<(u64, Arc<AtomicUsize>)> {
        // The compact stream frame carries only `next_offset`, so it cannot
        // describe a rebase. Silently starting at the oldest recent chunk
        // would create an undetectable gap whenever the disk writer lags the
        // broadcaster beyond this ring. Refuse the stream and let callers use
        // their journal fallback, whose page contract does carry a rebase.
        let oldest_available = self
            .recent
            .front()
            .map(|chunk| chunk.start_offset)
            .unwrap_or(self.next_offset);
        if offset < oldest_available || offset > self.next_offset {
            return None;
        }
        self.next_subscriber_id = self.next_subscriber_id.saturating_add(1);
        let subscriber_id = self.next_subscriber_id;
        let subscriber = OutputStreamSubscriber {
            id: subscriber_id,
            answers_queries,
            tx,
            buffered: Arc::new(AtomicUsize::new(0)),
        };
        let buffered = subscriber.buffered.clone();

        for chunk in &self.recent {
            let chunk_end = chunk.start_offset + chunk.data.len() as u64;
            if chunk_end <= offset {
                continue;
            }

            let skip = offset.saturating_sub(chunk.start_offset) as usize;
            let data = chunk.data.get(skip..).unwrap_or_default().to_vec();
            if data.is_empty() {
                continue;
            }

            let _ = enqueue_output_chunk(
                &subscriber,
                SessionOutputChunk {
                    data,
                    next_offset: chunk_end,
                    exited: false,
                    exists: true,
                },
            );
        }

        if self.exited {
            let _ = enqueue_output_chunk(
                &subscriber,
                SessionOutputChunk {
                    data: Vec::new(),
                    next_offset: self.next_offset,
                    exited: true,
                    exists: true,
                },
            );
            return Some((subscriber_id, buffered));
        }

        self.subscribers.push(subscriber);
        Some((subscriber_id, buffered))
    }

    /// Whether any connected client is backed by a real answering terminal.
    /// While none is, the host answers terminal probes itself (CPR, kitty
    /// keyboard, OSC color queries) so probe-dependent TUIs — muse exits ~4s
    /// after launch without answers — survive headless/phone-only sessions.
    fn has_answering_subscriber(&self) -> bool {
        self.subscribers.iter().any(|s| s.answers_queries)
    }

    fn subscriber_answers_queries(&self, subscriber_id: u64) -> bool {
        self.subscribers
            .iter()
            .any(|s| s.id == subscriber_id && s.answers_queries)
    }

    fn remove_subscriber(&mut self, subscriber_id: u64) {
        self.subscribers
            .retain(|subscriber| subscriber.id != subscriber_id);
        if self.subscribers.is_empty() {
            self.subscribers = Vec::new();
        }
    }

    fn has_subscribers(&self) -> bool {
        !self.subscribers.is_empty()
    }

    /// Drop the recent ring and its capacity (idle diet). Offsets stay
    /// monotonic: a later subscriber older than `next_offset` is refused
    /// and uses the journal, exactly like one behind a full ring.
    fn release_recent(&mut self) {
        if self.recent.is_empty() && self.recent.capacity() == 0 {
            return;
        }
        self.recent = VecDeque::new();
        self.recent_bytes = 0;
    }

    fn broadcast_chunk(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let start_offset = self.next_offset;
        let next_offset = start_offset + data.len() as u64;
        self.next_offset = next_offset;
        self.recent_bytes += data.len();
        self.recent.push_back(RecentOutputChunk {
            start_offset,
            data: data.to_vec(),
        });

        while self.recent_bytes > SESSION_OUTPUT_STREAM_RECENT_BYTES {
            let Some(oldest) = self.recent.pop_front() else {
                break;
            };
            self.recent_bytes = self.recent_bytes.saturating_sub(oldest.data.len());
        }

        let chunk = SessionOutputChunk {
            data: data.to_vec(),
            next_offset,
            exited: false,
            exists: true,
        };
        self.subscribers
            .retain(|subscriber| enqueue_output_chunk(subscriber, chunk.clone()));
    }

    fn mark_exited(&mut self) {
        self.exited = true;
        let chunk = SessionOutputChunk {
            data: Vec::new(),
            next_offset: self.next_offset,
            exited: true,
            exists: true,
        };
        self.subscribers
            .retain(|subscriber| enqueue_output_chunk(subscriber, chunk.clone()));
        self.subscribers.clear();
    }
}

/// Enqueue a chunk to a subscriber, accounting for its buffered bytes. Returns
/// `false` (so callers drop the subscriber) if the receiver is gone or the
/// subscriber has fallen further behind than
/// `SESSION_OUTPUT_STREAM_SUBSCRIBER_MAX_BUFFERED_BYTES`, which keeps a stalled
/// client from growing host memory without bound.
#[cfg(unix)]
fn enqueue_output_chunk(subscriber: &OutputStreamSubscriber, chunk: SessionOutputChunk) -> bool {
    if subscriber.buffered.load(Ordering::Relaxed)
        > SESSION_OUTPUT_STREAM_SUBSCRIBER_MAX_BUFFERED_BYTES
    {
        return false;
    }
    let len = chunk.data.len();
    subscriber.buffered.fetch_add(len, Ordering::Relaxed);
    if subscriber.tx.send(chunk).is_err() {
        subscriber.buffered.fetch_sub(len, Ordering::Relaxed);
        return false;
    }
    true
}

pub fn session_output_frame_bytes(chunk: &SessionOutputChunk) -> Vec<u8> {
    let len = chunk.data.len() as u32;
    let mut frame = Vec::with_capacity(SESSION_OUTPUT_STREAM_FRAME_HEADER_BYTES + chunk.data.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&chunk.next_offset.to_be_bytes());
    let mut flags = 0u8;
    if chunk.exited {
        flags |= 1;
    }
    if chunk.exists {
        flags |= 2;
    }
    frame.push(flags);
    frame.extend_from_slice(&chunk.data);
    frame
}

#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
fn write_output_stream_frame<W: Write>(
    writer: &mut W,
    chunk: &SessionOutputChunk,
) -> std::io::Result<()> {
    let frame = session_output_frame_bytes(chunk);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
pub enum OutputStreamRead {
    Chunk(SessionOutputChunk),
    TimedOut,
    Closed,
}

#[cfg(unix)]
pub fn read_output_stream_frame<R: Read>(reader: &mut R) -> Result<OutputStreamRead, String> {
    let mut header = [0u8; SESSION_OUTPUT_STREAM_FRAME_HEADER_BYTES];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(OutputStreamRead::TimedOut);
        }
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(OutputStreamRead::Closed);
        }
        Err(error) => return Err(format!("Failed to read output stream frame: {error}")),
    }

    let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    // Real hosts batch frames to SESSION_OUTPUT_STREAM_BATCH_MAX_BYTES; a
    // length far beyond that means the peer is not speaking this protocol
    // (a JSON error line parsed as a header allocates gigabytes) — fail
    // instead of allocating.
    if len > SESSION_OUTPUT_STREAM_MAX_FRAME_BYTES {
        return Err(format!("Output stream frame too large: {len} bytes"));
    }
    let next_offset = u64::from_be_bytes(header[4..12].try_into().unwrap());
    let flags = header[12];
    let mut data = vec![0u8; len];
    if len > 0 {
        reader
            .read_exact(&mut data)
            .map_err(|e| format!("Failed to read output stream payload: {e}"))?;
    }

    Ok(OutputStreamRead::Chunk(SessionOutputChunk {
        data,
        next_offset,
        exited: flags & 1 != 0,
        exists: flags & 2 != 0,
    }))
}

#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
fn flush_batched_output_stream_chunk<W: Write>(
    writer: &mut W,
    pending: &mut Option<SessionOutputChunk>,
) -> Result<(), String> {
    let Some(chunk) = pending.take() else {
        return Ok(());
    };

    write_output_stream_frame(writer, &chunk)
        .map_err(|e| format!("Failed to write output stream frame: {e}"))
}

#[cfg(unix)]
fn can_merge_output_stream_chunk(
    pending: &SessionOutputChunk,
    next: &SessionOutputChunk,
    max_batch_bytes: usize,
) -> bool {
    if pending.exited || next.exited || pending.exists != next.exists {
        return false;
    }

    let next_start = next.next_offset.saturating_sub(next.data.len() as u64);
    pending.next_offset == next_start
        && pending.data.len().saturating_add(next.data.len()) <= max_batch_bytes
}

#[cfg(unix)]
#[cfg_attr(not(test), allow(dead_code))]
fn run_batched_output_stream_forwarder<W: Write>(
    writer: &mut W,
    rx: mpsc::Receiver<SessionOutputChunk>,
    flush_interval: Duration,
    max_batch_bytes: usize,
    buffered: &AtomicUsize,
) -> Result<(), String> {
    let mut pending: Option<SessionOutputChunk> = None;

    loop {
        let recv_result = if pending.is_none() {
            rx.recv()
                .map(Some)
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            match rx.recv_timeout(flush_interval) {
                Ok(chunk) => Ok(Some(chunk)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    flush_batched_output_stream_chunk(writer, &mut pending)?;
                    return Ok(());
                }
            }
        };

        match recv_result {
            Ok(Some(chunk)) => {
                // Chunk has left the channel; release its reserved budget so the
                // broadcaster stops counting it against this subscriber.
                buffered.fetch_sub(
                    chunk.data.len().min(buffered.load(Ordering::Relaxed)),
                    Ordering::Relaxed,
                );
                if chunk.exited {
                    flush_batched_output_stream_chunk(writer, &mut pending)?;
                    write_output_stream_frame(writer, &chunk)
                        .map_err(|e| format!("Failed to write output stream frame: {e}"))?;
                    return Ok(());
                }

                if let Some(current) = pending.as_mut() {
                    if can_merge_output_stream_chunk(current, &chunk, max_batch_bytes) {
                        current.data.extend_from_slice(&chunk.data);
                        current.next_offset = chunk.next_offset;
                        if current.data.len() >= max_batch_bytes {
                            flush_batched_output_stream_chunk(writer, &mut pending)?;
                        }
                        continue;
                    }

                    flush_batched_output_stream_chunk(writer, &mut pending)?;
                }

                if chunk.data.len() >= max_batch_bytes {
                    write_output_stream_frame(writer, &chunk)
                        .map_err(|e| format!("Failed to write output stream frame: {e}"))?;
                } else {
                    pending = Some(chunk);
                }
            }
            Ok(None) => {
                flush_batched_output_stream_chunk(writer, &mut pending)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                flush_batched_output_stream_chunk(writer, &mut pending)?;
            }
        }
    }
}

fn flush_output_batch<W: Write>(writer: &mut W, pending: &mut Vec<u8>) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }
    writer
        .write_all(pending)
        .map_err(|e| format!("Failed to write output log: {e}"))?;
    pending.clear();
    Ok(())
}

/// File writer that preserves lifetime logical offsets while keeping only a
/// bounded suffix physically allocated. Readers use `output-retention.json`
/// to rebase a cursor that fell behind the retained window; the sparse prefix
/// keeps every current offset, long-poll comparison, and live stream cursor
/// monotonic across arbitrarily long-lived sessions.
pub(crate) struct RetainedOutputWriter {
    file: File,
    retention_path: PathBuf,
    retained_from: u64,
    retain_bytes: u64,
    advance_bytes: u64,
    next_retention_check_at: u64,
    retention_disabled: bool,
}

impl RetainedOutputWriter {
    fn new(
        mut file: File,
        output_path: PathBuf,
        initial_offset: u64,
        retain_bytes: u64,
        advance_bytes: u64,
    ) -> Result<Self, String> {
        let retention_path = output_path
            .parent()
            .ok_or("Invalid output journal path")?
            .join(OUTPUT_RETENTION_FILE);
        // A replacement Host discards the preceding terminal scrollback but
        // must never reuse its logical cursors: a disconnected Controller's
        // old offset could otherwise alias unrelated new bytes after regrowth.
        // Start after the preceding high-water mark as a sparse intentional
        // gap and publish that floor before any bytes become visible.
        save_output_retention_path(&retention_path, initial_offset)?;
        file.set_len(0)
            .and_then(|_| file.set_len(initial_offset))
            .and_then(|_| {
                file.seek(std::io::SeekFrom::Start(initial_offset))
                    .map(|_| ())
            })
            .map_err(|error| format!("Failed to initialize output journal offset: {error}"))?;
        Ok(Self {
            file,
            retention_path,
            retained_from: initial_offset,
            retain_bytes: retain_bytes.max(1),
            advance_bytes: advance_bytes.max(1),
            next_retention_check_at: initial_offset
                .saturating_add(retain_bytes.max(1))
                .saturating_add(advance_bytes.max(1)),
            retention_disabled: false,
        })
    }

    /// Take over an existing journal from a previous Host of the SAME
    /// Session (core-to-core handoff): keep every byte, the published
    /// retention floor, and the logical cursor. Unlike `new`, this never
    /// truncates or re-sparsifies the file — that path is for replacement
    /// Hosts whose scrollback is discarded on purpose.
    pub(crate) fn reopen(
        mut file: File,
        output_path: PathBuf,
        next_offset: u64,
        retain_bytes: u64,
        advance_bytes: u64,
    ) -> Result<Self, String> {
        let retention_path = output_path
            .parent()
            .ok_or("Invalid output journal path")?
            .join(OUTPUT_RETENTION_FILE);
        let retained_from = read_output_retention_path(&retention_path).min(next_offset);
        let length = file
            .metadata()
            .map_err(|error| format!("Failed to inspect output journal: {error}"))?
            .len();
        if length != next_offset {
            return Err(format!(
                "Output journal length {length} does not match the handed-over offset {next_offset}"
            ));
        }
        file.seek(std::io::SeekFrom::Start(next_offset))
            .map_err(|error| format!("Failed to seek output journal: {error}"))?;
        Ok(Self {
            file,
            retention_path,
            retained_from,
            retain_bytes: retain_bytes.max(1),
            advance_bytes: advance_bytes.max(1),
            next_retention_check_at: next_offset
                .saturating_add(retain_bytes.max(1))
                .saturating_add(advance_bytes.max(1)),
            retention_disabled: false,
        })
    }

    fn maybe_advance_retention(&mut self) -> Result<(), String> {
        if self.retention_disabled {
            return Ok(());
        }
        let metadata = self
            .file
            .metadata()
            .map_err(|error| format!("Failed to inspect output journal: {error}"))?;
        let logical_end = metadata.len();
        if logical_end < self.next_retention_check_at {
            return Ok(());
        }
        // A malformed/unclosed OSC/DCS can leave no safe boundary across the
        // whole window. Do not rescan that growing window for every 128 KiB
        // batch; wait for another coarse advance before trying again.
        self.next_retention_check_at = logical_end.saturating_add(self.advance_bytes);

        #[cfg(unix)]
        let block_size = {
            use std::os::unix::fs::MetadataExt;
            metadata.blksize().max(1)
        };
        #[cfg(not(unix))]
        let block_size = 4096u64;

        let desired = logical_end.saturating_sub(self.retain_bytes);
        let next_retained_from =
            safe_output_retention_boundary(&mut self.file, self.retained_from, desired)?;
        self.file
            .seek(std::io::SeekFrom::Start(logical_end))
            .map_err(|error| format!("Failed to restore output journal cursor: {error}"))?;
        if next_retained_from <= self.retained_from {
            return Ok(());
        }

        // Filesystems punch only whole blocks. The logical readable floor is
        // an independently proven VT/UTF-8 boundary; any hidden partial block
        // between the punched prefix and that floor costs <1 block.
        let punch_start = self.retained_from - (self.retained_from % block_size);
        let punch_end = next_retained_from - (next_retained_from % block_size);
        if punch_end <= punch_start {
            return Ok(());
        }

        // Publish and durably sync the readable floor BEFORE deallocation.
        // A crash can therefore discard a few extra readable bytes, but can
        // never make a reader feed sparse NULs into its terminal.
        save_output_retention_path(&self.retention_path, next_retained_from)?;
        if let Err(error) = punch_output_hole(&self.file, punch_start, punch_end - punch_start) {
            // Never truncate/regrow a live inode as a fallback: lock-free
            // attach/mobile readers could commit the transient EOF or sparse
            // zeros. Mainline APFS and Linux host filesystems support punching;
            // an exotic unsupported filesystem stays lossless and always-on,
            // emits one diagnostic, and leaves retention disabled for this Host.
            let _ = save_output_retention_path(&self.retention_path, self.retained_from);
            self.retention_disabled = true;
            log::warn!(
                "output journal retention disabled for {}: {error}",
                self.retention_path.display()
            );
            return Ok(());
        }
        self.retained_from = next_retained_from;
        self.next_retention_check_at = next_retained_from
            .saturating_add(self.retain_bytes)
            .saturating_add(self.advance_bytes);
        Ok(())
    }
}

impl Write for RetainedOutputWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(bytes)?;
        if written > 0 {
            if let Err(error) = self.maybe_advance_retention() {
                // Retention is maintenance, not part of the PTY durability
                // contract. The scan temporarily seeks this shared file, so
                // first restore its append cursor; then fail open and keep the
                // Host journaling rather than disconnecting an always-on
                // session because a marker could not be saved or inspected.
                self.file.seek(std::io::SeekFrom::End(0)).map_err(|seek_error| {
                    std::io::Error::other(format!(
                        "Output journal retention failed ({error}) and its append cursor could not be restored: {seek_error}"
                    ))
                })?;
                self.retention_disabled = true;
                log::warn!(
                    "output journal retention disabled for {}: {error}",
                    self.retention_path.display()
                );
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(target_os = "macos")]
fn punch_output_hole(file: &File, offset: u64, length: u64) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let offset = i64::try_from(offset).map_err(|_| "output journal offset overflow")?;
    let length = i64::try_from(length).map_err(|_| "output journal length overflow")?;
    let mut request = libc::fpunchhole_t {
        fp_flags: 0,
        reserved: 0,
        fp_offset: offset,
        fp_length: length,
    };
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &mut request) };
    if result == -1 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn punch_output_hole(file: &File, offset: u64, length: u64) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let offset = i64::try_from(offset).map_err(|_| "output journal offset overflow")?;
    let length = i64::try_from(length).map_err(|_| "output journal length overflow")?;
    let result = unsafe {
        libc::fallocate(
            file.as_raw_fd(),
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
            offset,
            length,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn punch_output_hole(_file: &File, _offset: u64, _length: u64) -> Result<(), String> {
    Err("hole punching is unavailable on this platform".into())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputJournalCompaction {
    pub scanned: usize,
    pub compacted: usize,
    pub logical_bytes_evicted: u64,
}

fn compact_output_journal_path(
    path: &Path,
    retention_path: &Path,
    retain_bytes: u64,
) -> Result<Option<u64>, String> {
    let link_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect output journal: {error}"))?;
    if !link_metadata.file_type().is_file() {
        return Ok(None);
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("Failed to open output journal: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Failed to stat output journal: {error}"))?;
    let logical_end = metadata.len();
    let retained_from = read_output_retention_path(retention_path).min(logical_end);
    if logical_end.saturating_sub(retained_from) <= retain_bytes {
        return Ok(None);
    }

    let desired = logical_end.saturating_sub(retain_bytes);
    let next_retained_from = safe_output_retention_boundary(&mut file, retained_from, desired)?;
    if next_retained_from <= retained_from {
        return Ok(None);
    }
    #[cfg(unix)]
    let block_size = {
        use std::os::unix::fs::MetadataExt;
        metadata.blksize().max(1)
    };
    #[cfg(not(unix))]
    let block_size = 4096u64;
    let punch_start = retained_from - (retained_from % block_size);
    let punch_end = next_retained_from - (next_retained_from % block_size);
    if punch_end <= punch_start {
        return Ok(None);
    }

    save_output_retention_path(retention_path, next_retained_from)?;
    if let Err(error) = punch_output_hole(&file, punch_start, punch_end - punch_start) {
        let _ = save_output_retention_path(retention_path, retained_from);
        return Err(format!("Failed to punch output journal blocks: {error}"));
    }
    Ok(Some(next_retained_from - retained_from))
}

/// One-shot migration/maintenance pass for durable stopped Sessions. Running
/// pre-v4 Hosts are deliberately untouched: their bundled readers do not know
/// the retention floor and must use the normal Reload Terminal upgrade path.
/// Exited logs have no writer, so publishing a safe floor and punching their
/// prefix is race-free and immediately reclaims legacy multi-gigabyte files.
pub fn compact_exited_output_journals() -> Result<OutputJournalCompaction, String> {
    let mut result = OutputJournalCompaction::default();
    let entries = match fs::read_dir(app_sessions_root()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(format!("Failed to list Session journals: {error}")),
    };

    for entry in entries.flatten() {
        let session_id = entry.file_name().to_string_lossy().to_string();
        let Ok(entry_type) = entry.file_type() else {
            continue;
        };
        if !entry_type.is_dir() {
            continue;
        }
        // Removal, archive, restore, and Host replacement all use this same
        // cross-process lease. Revalidate everything only after acquiring it:
        // otherwise a concurrent Remove could delete the directory between
        // our scan and marker publication, and create_dir_all would resurrect
        // a ghost Session around the retention sidecar.
        let Ok(_lifecycle_lock) = crate::session_ops::lock_session_lifecycle(&session_id) else {
            continue;
        };
        let Some(manifest) = load_manifest(&session_id) else {
            continue;
        };
        if manifest.state != HostedSessionState::Exited {
            continue;
        }
        let session_path = app_sessions_root().join(&session_id);
        let Ok(session_metadata) = fs::symlink_metadata(&session_path) else {
            continue;
        };
        if !session_metadata.file_type().is_dir() {
            continue;
        }
        let path = session_path.join("output.bin");
        let Ok(link_metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !link_metadata.file_type().is_file() {
            continue;
        }
        result.scanned += 1;
        let retention_path = session_path.join(OUTPUT_RETENTION_FILE);
        match compact_output_journal_path(
            &path,
            &retention_path,
            SESSION_OUTPUT_JOURNAL_RETAIN_BYTES,
        ) {
            Ok(Some(evicted)) => {
                result.compacted += 1;
                result.logical_bytes_evicted = result.logical_bytes_evicted.saturating_add(evicted);
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!(
                    "could not compact exited output journal {}: {error}",
                    path.display()
                );
            }
        }
    }
    Ok(result)
}

#[cfg_attr(not(test), allow(dead_code))]
fn run_batched_output_writer<W: Write>(
    mut writer: W,
    rx: mpsc::Receiver<Vec<u8>>,
    flush_interval: Duration,
    max_batch_bytes: usize,
) -> Result<(), String> {
    let mut pending = Vec::with_capacity(max_batch_bytes.max(1));

    loop {
        let recv_result = if pending.is_empty() {
            rx.recv()
                .map(Some)
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        } else {
            match rx.recv_timeout(flush_interval) {
                Ok(chunk) => Ok(Some(chunk)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    flush_output_batch(&mut writer, &mut pending)?;
                    return Ok(());
                }
            }
        };

        match recv_result {
            Ok(Some(chunk)) => {
                pending.extend_from_slice(&chunk);
                if pending.len() >= max_batch_bytes {
                    flush_output_batch(&mut writer, &mut pending)?;
                }
            }
            Ok(None) => {
                flush_output_batch(&mut writer, &mut pending)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                flush_output_batch(&mut writer, &mut pending)?;
            }
        }
    }
}

#[derive(Clone)]
struct CachedManifestHealth {
    checked_at: u64,
    manifest: Option<HostedSessionManifest>,
}

const LEAKED_LAUNCHER_ENV_KEYS: &[&str] = &[
    // Parent agent/dev shells often force a specific color tier. Unpeel owns
    // the PTY and advertises its real xterm-256color/truecolor capabilities.
    "FORCE_COLOR",
    "CLICOLOR_FORCE",
    "npm_command",
    "npm_execpath",
    "npm_lifecycle_event",
    "npm_lifecycle_script",
    "npm_node_execpath",
];

const HERDR_ENV_PREFIX: &str = "HERDR_";

fn env_keys_with_prefix<I>(vars: I, prefix: &str) -> Vec<OsString>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    vars.into_iter()
        .filter_map(|(key, _)| env_key_has_prefix(&key, prefix).then_some(key))
        .collect()
}

fn env_key_has_prefix(key: &OsStr, prefix: &str) -> bool {
    key.as_encoded_bytes().starts_with(prefix.as_bytes())
}

fn strip_env_prefix_from_process_command<I>(cmd: &mut std::process::Command, vars: I, prefix: &str)
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    for key in env_keys_with_prefix(vars, prefix) {
        cmd.env_remove(key);
    }
}

fn strip_env_prefix_from_pty_command<I>(cmd: &mut CommandBuilder, vars: I, prefix: &str)
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    for key in env_keys_with_prefix(vars, prefix) {
        cmd.env_remove(key);
    }
}

pub(crate) fn strip_leaked_launcher_env(cmd: &mut CommandBuilder) {
    // Dev shells launched from `bunx tauri dev` inherit npm/bun lifecycle vars from the
    // app process. Some CLIs treat those vars as authoritative and rebuild their own
    // child-process invocation incorrectly, so strip them before opening user shells.
    for key in LEAKED_LAUNCHER_ENV_KEYS {
        cmd.env_remove(key);
    }
}

pub(crate) fn strip_runtime_inherited_env(cmd: &mut CommandBuilder) {
    // A Host may itself be launched inside any supported provider. Strip all
    // descriptor-declared provider identity before opening the new shell so a
    // nested runtime cannot inherit the parent conversation/session.
    for runtime in crate::runtime_catalog::builtin_runtime_catalog().descriptors() {
        for key in &runtime.environment.strip_inherited {
            cmd.env_remove(key);
        }
    }
}

pub(crate) fn strip_leaked_herdr_process_env(cmd: &mut std::process::Command) {
    strip_env_prefix_from_process_command(cmd, std::env::vars_os(), HERDR_ENV_PREFIX);
}

pub(crate) fn strip_leaked_herdr_pty_env(cmd: &mut CommandBuilder) {
    strip_env_prefix_from_pty_command(cmd, std::env::vars_os(), HERDR_ENV_PREFIX);
}

/// How the login shell relates to the POSIX startup script the host composes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShellFamily {
    /// Bourne-compatible: runs the startup script directly.
    Posix,
    /// fish: valid login shell for env setup, but cannot parse the script.
    Fish,
    /// Unknown non-Bourne shell (nu, elvish, …): launch through /bin/sh.
    Other,
}

fn shell_family(shell_path: &str) -> ShellFamily {
    match shared::shell_name(shell_path).as_str() {
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" | "mksh" => ShellFamily::Posix,
        "fish" => ShellFamily::Fish,
        _ => ShellFamily::Other,
    }
}

/// Quote for fish: inside fish single quotes, `\` and `'` are escaped with a
/// backslash (unlike POSIX single quotes, which cannot contain `'` at all).
fn fish_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The startup script is POSIX sh (`set +e`, `export VAR=…`, `{ …; }`, `$?`),
/// which fish cannot parse — handing it to `fish -c` dies with a parse error
/// before anything launches. Keep fish as the login shell so config.fish env
/// (PATH for nvm/brew-installed CLIs) still applies, then exec /bin/sh for the
/// script itself; the script's trailing fallback still execs interactive fish.
fn fish_bridge_script(shell_script: &str) -> String {
    format!("exec /bin/sh -c {}", fish_single_quote(shell_script))
}

fn shell_history_append_snippet(shell_path: &str, command: &str) -> Option<String> {
    let shell_name = shared::shell_name(shell_path);
    let quoted_command = shared::shell_quote(command);

    match shell_name.as_str() {
        "zsh" => Some(format!("print -sr -- {quoted_command}; fc -AI")),
        "bash" => Some(format!("history -s -- {quoted_command}; history -a")),
        _ => None,
    }
}

fn fallback_shell_exec_snippet(shell_path: &str) -> String {
    format!("exec {} -l -i", shared::shell_quote(shell_path))
}

/// Shell snippet that blocks (up to ~2s) until the attach client has written
/// `.attach-ready`. The attach client writes it only *after* syncing the
/// surface's real grid to the host PTY (its startup `resize`), so holding the
/// provider CLI here means its first paint matches the window instead of the
/// launch-time `initial_cols`/`initial_rows`. This matters because already-
/// emitted output (e.g. Claude's welcome banner) never reflows on a later
/// resize: if the CLI prints before the surface size is known, it stays at the
/// guessed/stale launch grid until the user nudges the window. Codex did this
/// in its wrapper; doing it generically extends the same guarantee to every
/// provider. POSIX-only constructs so it runs under both zsh and bash. Skipped
/// for background/MCP launches (no attach client → `wait_for_attach == false`).
fn attach_ready_wait_snippet(session_id: &str) -> String {
    let ready = attach_ready_path(session_id);
    let quoted = shared::shell_quote(&ready.to_string_lossy());
    format!(
        "__unpeel_attach_ready={quoted}; __unpeel_attach_i=0; \
         while [ \"$__unpeel_attach_i\" -lt 100 ] && [ ! -e \"$__unpeel_attach_ready\" ]; do \
         sleep 0.02; __unpeel_attach_i=$((__unpeel_attach_i+1)); done"
    )
}

fn build_startup_shell_script(
    shell_path: &str,
    shell_prelude: Vec<String>,
    startup_command: &str,
    history_snippet: Option<String>,
) -> String {
    let mut shell_segments = Vec::new();

    // User shell startup files can enable errexit/ERR_EXIT. The hosted PTY
    // should survive a provider returning non-zero and land in a shell.
    shell_segments.push("set +e".to_string());
    shell_segments.extend(shell_prelude);
    // Run the command without echoing it first — the full launch command (with
    // injected MCP configs etc.) flashing before the TUI starts reads as noise.
    // It stays recoverable via shell history and the session manifest.
    //
    // `trap : INT` around the command: the runtime runs in its own process
    // group (job control is on for `-i`), so Ctrl-C reaches only it — but
    // zsh then treats a foreground job killed by SIGINT as its own interrupt
    // and abandons the rest of this `-c` list, never reaching the fallback
    // `exec` below: the wrapper exits 130 and the Session dies with the
    // runtime (bash continues on its own). A no-op *handler* makes zsh carry
    // on like bash; `trap ''` would be wrong because children inherit an
    // ignored SIGINT and the runtime could no longer be interrupted at all.
    // Reset afterwards so the fallback shell starts with default handling.
    shell_segments.push("trap : INT".to_string());
    shell_segments.push(format!("{{ {startup_command}; }}"));
    shell_segments.push("__unpeel_startup_status=$?".to_string());
    shell_segments.push("trap - INT".to_string());
    shell_segments.push("set +e".to_string());

    if let Some(history_snippet) = history_snippet {
        shell_segments.push(format!("{{ {history_snippet}; }}"));
        shell_segments.push("set +e".to_string());
    }

    shell_segments.push(fallback_shell_exec_snippet(shell_path));
    shell_segments.join("; ")
}

/// Scope the Host-owned runtime generation to one provider invocation. The
/// persistent interactive shell must never retain this value: after a managed
/// agent exits, a different CLI typed by the user is only passively observed
/// and must not be able to masquerade as the managed runtime generation.
///
/// Initial launches always execute inside the POSIX startup script (fish is
/// bridged through `/bin/sh`). In-place restarts execute in the user's now-live
/// interactive shell. Provider startup adapters are nevertheless POSIX shell
/// programs (Kimi probes with `if ...; then`, Cline wraps cleanup in `{ ...; }`,
/// and future adapters may do the same), so a non-POSIX live shell must pass
/// the complete recipe to `/bin/sh` as one quoted argument. The generation is
/// attached with `env` to that child only and therefore cannot leak into the
/// persistent fish shell after the provider exits. Other live-shell syntaxes
/// are rejected before the current agent is touched; they cannot safely share
/// either POSIX or fish quoting rules.
#[cfg(unix)]
fn runtime_generation_scoped_command(
    shell_family: ShellFamily,
    command: &str,
    generation: u64,
) -> String {
    match shell_family {
        ShellFamily::Posix => format!(
            "export UNPEEL_RUNTIME_GENERATION={generation}; {{ {command}; }}; \
             __unpeel_runtime_status=$?; unset UNPEEL_RUNTIME_GENERATION; \
             (exit \"$__unpeel_runtime_status\")"
        ),
        ShellFamily::Fish => generation_scoped_posix_child(command, generation, fish_single_quote),
        ShellFamily::Other => {
            generation_scoped_posix_child(command, generation, shared::shell_quote)
        }
    }
}

#[cfg(unix)]
fn generation_scoped_posix_child(
    command: &str,
    generation: u64,
    quote_for_live_shell: fn(&str) -> String,
) -> String {
    // Disable inherited errexit before entering the adapter recipe. The
    // group's status is still `/bin/sh -c`'s status, so a provider failure is
    // faithfully returned through env and the live shell while that shell
    // itself remains alive.
    let child_script = format!("set +e; {{ {command}; }}");
    format!(
        "/usr/bin/env UNPEEL_RUNTIME_GENERATION={generation} /bin/sh -c {}",
        quote_for_live_shell(&child_script)
    )
}

pub fn app_sessions_root() -> PathBuf {
    unpeel_sessions_root()
}

pub fn session_dir(session_id: &str) -> PathBuf {
    app_sessions_root().join(session_id)
}

pub fn manifest_path(session_id: &str) -> PathBuf {
    session_dir(session_id).join("manifest.json")
}

pub fn launch_path(session_id: &str) -> PathBuf {
    session_dir(session_id).join("launch.json")
}

pub fn output_path(session_id: &str) -> PathBuf {
    session_dir(session_id).join("output.bin")
}

pub fn output_retention_path(session_id: &str) -> PathBuf {
    session_dir(session_id).join(OUTPUT_RETENTION_FILE)
}

/// Earliest lifetime output offset whose bytes remain readable. Missing or
/// legacy metadata means the old fully-retained `output.bin` contract.
pub fn output_retained_from(session_id: &str) -> u64 {
    read_output_retention_path(&output_retention_path(session_id))
}

fn read_output_retention_path(path: &Path) -> u64 {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<OutputRetentionState>(&bytes).ok())
        .filter(|state| state.version == OutputRetentionState::VERSION)
        .map(|state| state.retained_from)
        .unwrap_or(0)
}

fn save_output_retention_path(path: &Path, retained_from: u64) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("Invalid output-retention parent path")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create output-retention directory: {error}"))?;
    let body = serde_json::to_vec(&OutputRetentionState::at(retained_from))
        .map_err(|error| format!("Failed to serialize output retention: {error}"))?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                format!(
                    "Failed to open output-retention temporary file {}: {error}",
                    temporary.display()
                )
            })?;
        file.write_all(&body)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("Failed to persist output retention: {error}"))?;
        drop(file);
        fs::rename(&temporary, path)
            .map_err(|error| format!("Failed to publish output retention: {error}"))?;
        // Make the marker-before-hole ordering durable: after a crash readers
        // may skip a little extra retained history, but can never consume a
        // punched sparse prefix as NUL terminal input.
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("Failed to sync output-retention directory: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

const REPLAY_TAIL_ALIGNMENT_LOOKBACK_BYTES: u64 = 16 * 1024;

fn is_utf8_continuation_byte(byte: u8) -> bool {
    (byte & 0b1100_0000) == 0b1000_0000
}

#[derive(Clone, Copy)]
enum ReplayScanState {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    Dcs,
    DcsEscape,
    SosPmApc,
    SosPmApcEscape,
}

fn align_replay_tail_start(
    file: &mut File,
    desired_start: u64,
    retained_from: u64,
) -> Result<u64, String> {
    if desired_start <= retained_from {
        return Ok(retained_from);
    }

    let scan_start = desired_start
        .saturating_sub(REPLAY_TAIL_ALIGNMENT_LOOKBACK_BYTES)
        .max(retained_from);
    let scan_len = desired_start.saturating_sub(scan_start) as usize;
    if scan_len == 0 {
        return Ok(desired_start);
    }

    use std::io::Seek;
    use std::io::SeekFrom;
    file.seek(SeekFrom::Start(scan_start))
        .map_err(|e| format!("Failed to seek output log for replay alignment: {e}"))?;

    let mut window = vec![0u8; scan_len];
    file.read_exact(&mut window)
        .map_err(|e| format!("Failed to read output log for replay alignment: {e}"))?;

    let mut initial_index = 0usize;
    while initial_index < window.len() && is_utf8_continuation_byte(window[initial_index]) {
        initial_index += 1;
    }

    let mut last_boundary = scan_start + initial_index as u64;
    let mut state = ReplayScanState::Ground;

    for (index, byte) in window.iter().enumerate().skip(initial_index) {
        let absolute = scan_start + index as u64;
        if matches!(*byte, 0x18 | 0x1a) {
            last_boundary = absolute + 1;
            state = ReplayScanState::Ground;
            continue;
        }
        if *byte == 0x1b
            && matches!(
                state,
                ReplayScanState::Escape
                    | ReplayScanState::EscapeIntermediate
                    | ReplayScanState::Csi
            )
        {
            last_boundary = absolute;
            state = ReplayScanState::Escape;
            continue;
        }
        match state {
            ReplayScanState::Ground => match *byte {
                0x1b => {
                    last_boundary = absolute;
                    state = ReplayScanState::Escape;
                }
                b'\n' | b'\r' => {
                    last_boundary = absolute + 1;
                }
                _ => {}
            },
            ReplayScanState::Escape => match *byte {
                b'[' => state = ReplayScanState::Csi,
                b']' => state = ReplayScanState::Osc,
                b'P' => state = ReplayScanState::Dcs,
                b'X' | b'^' | b'_' => state = ReplayScanState::SosPmApc,
                0x20..=0x2f => state = ReplayScanState::EscapeIntermediate,
                _ => {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
            },
            ReplayScanState::EscapeIntermediate => {
                if (0x30..=0x7e).contains(byte) {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
            }
            ReplayScanState::Csi => {
                if (0x40..=0x7e).contains(byte) {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
            }
            ReplayScanState::Osc => match *byte {
                0x07 => {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                }
                0x1b => state = ReplayScanState::OscEscape,
                _ => {}
            },
            ReplayScanState::OscEscape => {
                if *byte == b'\\' {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                } else {
                    state = ReplayScanState::Osc;
                }
            }
            ReplayScanState::Dcs => {
                if *byte == 0x1b {
                    state = ReplayScanState::DcsEscape;
                }
            }
            ReplayScanState::DcsEscape => {
                if *byte == b'\\' {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                } else {
                    state = ReplayScanState::Dcs;
                }
            }
            ReplayScanState::SosPmApc => {
                if *byte == 0x1b {
                    state = ReplayScanState::SosPmApcEscape;
                }
            }
            ReplayScanState::SosPmApcEscape => {
                if *byte == b'\\' {
                    last_boundary = absolute + 1;
                    state = ReplayScanState::Ground;
                } else {
                    state = ReplayScanState::SosPmApc;
                }
            }
        }
    }

    Ok(last_boundary.min(desired_start))
}

/// Find the latest replay-safe logical boundary at or before `desired_start`,
/// scanning from a boundary already proven safe. Unlike ordinary tail
/// alignment this deliberately scans the whole retention advance (normally
/// ~8 MiB): the new persisted floor must never begin inside CSI/OSC/DCS or a
/// UTF-8 rune after the preceding blocks are punched.
fn safe_output_retention_boundary(
    file: &mut File,
    safe_start: u64,
    desired_start: u64,
) -> Result<u64, String> {
    use std::io::SeekFrom;

    if desired_start <= safe_start {
        return Ok(safe_start);
    }
    file.seek(SeekFrom::Start(safe_start))
        .map_err(|error| format!("Failed to seek output retention scan: {error}"))?;

    let mut state = ReplayScanState::Ground;
    let mut last_boundary = safe_start;
    let mut absolute = safe_start;
    let mut remaining = desired_start - safe_start;
    let mut buffer = vec![0u8; SESSION_OUTPUT_READ_BUFFER_BYTES];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..want])
            .map_err(|error| format!("Failed to scan output retention boundary: {error}"))?;
        for byte in &buffer[..want] {
            if matches!(*byte, 0x18 | 0x1a) {
                last_boundary = absolute + 1;
                state = ReplayScanState::Ground;
                absolute += 1;
                continue;
            }
            if *byte == 0x1b
                && matches!(
                    state,
                    ReplayScanState::Escape
                        | ReplayScanState::EscapeIntermediate
                        | ReplayScanState::Csi
                )
            {
                last_boundary = absolute;
                state = ReplayScanState::Escape;
                absolute += 1;
                continue;
            }
            match state {
                ReplayScanState::Ground => {
                    // Every non-continuation byte starts a UTF-8 code point
                    // (or ASCII control) and is therefore a safe byte cursor
                    // while the VT parser is in Ground.
                    if !is_utf8_continuation_byte(*byte) {
                        last_boundary = absolute;
                    }
                    match *byte {
                        0x1b => state = ReplayScanState::Escape,
                        b'\n' | b'\r' => last_boundary = absolute + 1,
                        _ => {}
                    }
                }
                ReplayScanState::Escape => match *byte {
                    b'[' => state = ReplayScanState::Csi,
                    b']' => state = ReplayScanState::Osc,
                    b'P' => state = ReplayScanState::Dcs,
                    b'X' | b'^' | b'_' => state = ReplayScanState::SosPmApc,
                    0x20..=0x2f => state = ReplayScanState::EscapeIntermediate,
                    _ => {
                        last_boundary = absolute + 1;
                        state = ReplayScanState::Ground;
                    }
                },
                ReplayScanState::EscapeIntermediate => {
                    if (0x30..=0x7e).contains(byte) {
                        last_boundary = absolute + 1;
                        state = ReplayScanState::Ground;
                    }
                }
                ReplayScanState::Csi => {
                    if (0x40..=0x7e).contains(byte) {
                        last_boundary = absolute + 1;
                        state = ReplayScanState::Ground;
                    }
                }
                ReplayScanState::Osc => match *byte {
                    0x07 => {
                        last_boundary = absolute + 1;
                        state = ReplayScanState::Ground;
                    }
                    0x1b => state = ReplayScanState::OscEscape,
                    _ => {}
                },
                ReplayScanState::OscEscape => {
                    if *byte == b'\\' {
                        last_boundary = absolute + 1;
                        state = ReplayScanState::Ground;
                    } else {
                        state = ReplayScanState::Osc;
                    }
                }
                ReplayScanState::Dcs => {
                    if *byte == 0x1b {
                        state = ReplayScanState::DcsEscape;
                    }
                }
                ReplayScanState::DcsEscape => {
                    if *byte == b'\\' {
                        last_boundary = absolute + 1;
                        state = ReplayScanState::Ground;
                    } else {
                        state = ReplayScanState::Dcs;
                    }
                }
                ReplayScanState::SosPmApc => {
                    if *byte == 0x1b {
                        state = ReplayScanState::SosPmApcEscape;
                    }
                }
                ReplayScanState::SosPmApcEscape => {
                    if *byte == b'\\' {
                        last_boundary = absolute + 1;
                        state = ReplayScanState::Ground;
                    } else {
                        state = ReplayScanState::SosPmApc;
                    }
                }
            }
            absolute += 1;
        }
        remaining -= want as u64;
    }
    let last_boundary = last_boundary.min(desired_start);
    if desired_start.saturating_sub(last_boundary) <= SESSION_OUTPUT_JOURNAL_CONTROL_SLACK_BYTES {
        return Ok(last_boundary);
    }

    // A hostile or broken TUI can emit an OSC/DCS/SOS string that never
    // terminates. There is then no semantically perfect replay point, but an
    // always-on journal must still have a hard physical bound. Cut at the
    // nearest UTF-8 boundary and define it as a fresh Ground-state epoch;
    // stale renderers are cleared when their cursor is rebased. This loses
    // only already-evicted scrollback and never starts inside a valid rune.
    let scan_start = desired_start.saturating_sub(3).max(safe_start);
    file.seek(SeekFrom::Start(scan_start))
        .map_err(|error| format!("Failed to seek forced output retention boundary: {error}"))?;
    let mut boundary_bytes = vec![0u8; (desired_start - scan_start + 1) as usize];
    file.read_exact(&mut boundary_bytes)
        .map_err(|error| format!("Failed to read forced output retention boundary: {error}"))?;
    let mut relative = boundary_bytes.len().saturating_sub(1);
    while relative > 0 && is_utf8_continuation_byte(boundary_bytes[relative]) {
        relative -= 1;
    }
    if is_utf8_continuation_byte(boundary_bytes[relative]) {
        // Four consecutive continuation bytes cannot be part of one valid
        // UTF-8 scalar. The byte stream is already malformed, so the exact
        // desired cursor is the least-surprising bounded reset point.
        Ok(desired_start)
    } else {
        Ok(scan_start + relative as u64)
    }
}

/// Primary Device Attributes response the host answers with. Byte-identical to
/// what the app's Ghostty surface would say (ghostty `stream_handler.zig`:
/// 62 = VT220-level conformance, 22 = color text, 52 = clipboard access — the
/// embedded surface runs ghostty's default `clipboard-write = allow`), so a
/// program probing before any surface attaches negotiates the same features it
/// would against the live renderer.
const PRIMARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[?62;22;52c";

/// Standard xterm 256-color palette entry, for host-side OSC 4 answers while
/// no surface is attached (probes only — a real surface reports its theme).
fn xterm_palette_rgb(index: u16) -> (u8, u8, u8) {
    const BASE16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xCD, 0x00, 0x00),
        (0x00, 0xCD, 0x00),
        (0xCD, 0xCD, 0x00),
        (0x00, 0x00, 0xEE),
        (0xCD, 0x00, 0xCD),
        (0x00, 0xCD, 0xCD),
        (0xE5, 0xE5, 0xE5),
        (0x7F, 0x7F, 0x7F),
        (0xFF, 0x00, 0x00),
        (0x00, 0xFF, 0x00),
        (0xFF, 0xFF, 0x00),
        (0x5C, 0x5C, 0xFF),
        (0xFF, 0x00, 0xFF),
        (0x00, 0xFF, 0xFF),
        (0xFF, 0xFF, 0xFF),
    ];
    match index {
        0..=15 => BASE16[index as usize],
        16..=231 => {
            let value = index - 16;
            let component = |v: u16| -> u8 {
                if v == 0 {
                    0
                } else {
                    (v * 40 + 55) as u8
                }
            };
            (
                component(value / 36),
                component((value / 6) % 6),
                component(value % 6),
            )
        }
        _ => {
            let gray = (8 + (index.saturating_sub(232)) * 10).min(238) as u8;
            (gray, gray, gray)
        }
    }
}

/// Longest byte run the query scanner will hold as a possible in-flight DA1
/// query before giving up and passing it through. A real DA1 query is at most
/// a handful of bytes (`ESC [ 0 c`); this only bounds pathological streams.
const QUERY_SCAN_MAX_SEQUENCE_BYTES: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryScanState {
    Ground,
    Escape,
    Csi,
    Osc,
    /// Inside an OSC, after ESC (deciding ST terminator vs abort).
    OscEscape,
}

/// A terminal query the host answered (and excised from the stream).
#[derive(Clone, PartialEq, Eq, Debug)]
enum HostAnsweredQuery {
    /// `CSI c` / `CSI 0 c` / `ESC Z` — always host-answered (see below).
    Da1,
    /// `CSI 6 n` cursor position report — probe-gated.
    CursorPosition,
    /// `CSI ? u` kitty keyboard flags query — probe-gated.
    KittyFlags,
    /// `OSC 10/11 ; ?` foreground/background color query — probe-gated.
    OscColor { code: u8 },
    /// `OSC 4 ; <index> ; ?` palette query — probe-gated.
    OscPalette { index: u16 },
}

/// Scans the hosted child's terminal-bound output for Primary Device
/// Attributes queries — `CSI c` / `CSI 0 c` (and `ESC Z`, the obsolete DECID
/// form, which ghostty also treats as DA1) — so the host can answer them.
///
/// The shell starts inside the hosted PTY before any Ghostty surface attaches,
/// and the attach client deliberately mutes surface responses to *replayed*
/// queries (answering history produces stray input). So a startup probe like
/// fish's — which blocks up to seconds waiting for the DA1 reply that
/// terminates its query batch — can never be answered by a surface. The host
/// is the one party that always exists for the PTY's whole lifetime, so it
/// answers DA1 itself and excises the query from the recorded/broadcast
/// stream: a live surface never sees it (no double answer) and a replayed
/// tail can never re-trigger one (no stray late answer).
///
/// Everything that is not a DA1 query passes through byte-for-byte; incomplete
/// escape sequences at a chunk boundary are carried until the next chunk
/// (terminal parsers buffer incomplete sequences the same way).
struct OutputQueryScanner {
    state: QueryScanState,
    /// Bytes of the in-flight candidate sequence, starting with ESC.
    seq: Vec<u8>,
}

impl OutputQueryScanner {
    fn new() -> Self {
        Self {
            state: QueryScanState::Ground,
            seq: Vec::new(),
        }
    }

    /// Scan a chunk. Returns the bytes to pass through and the queries the
    /// caller must answer (each was excised from the stream).
    ///
    /// DA1 is always intercepted (fish blocks on it — see the type comment).
    /// The other probe kinds (CPR, kitty flags, OSC color queries) are
    /// intercepted only while `intercept_probes` is true — i.e. while no
    /// answering terminal is attached. An attached Ghostty surface answers
    /// them with real values (and e.g. Claude genuinely negotiates the kitty
    /// protocol), so the host must stay out of the way then; with nothing
    /// attached, unanswered probes kill probe-dependent TUIs (muse exits ~4s
    /// after launch), so the host steps in.
    fn scan(&mut self, input: &[u8], intercept_probes: bool) -> (Vec<u8>, Vec<HostAnsweredQuery>) {
        let mut out = Vec::with_capacity(input.len());
        let mut queries = Vec::new();

        for &byte in input {
            match self.state {
                QueryScanState::Ground => {
                    if byte == 0x1b {
                        self.seq.push(byte);
                        self.state = QueryScanState::Escape;
                    } else {
                        out.push(byte);
                    }
                }
                QueryScanState::Escape => match byte {
                    // DECID: obsolete DA1 request, answered like CSI c.
                    b'Z' => {
                        self.seq.clear();
                        self.state = QueryScanState::Ground;
                        queries.push(HostAnsweredQuery::Da1);
                    }
                    b'[' => {
                        self.seq.push(byte);
                        self.state = QueryScanState::Csi;
                    }
                    b']' if intercept_probes => {
                        self.seq.push(byte);
                        self.state = QueryScanState::Osc;
                    }
                    // ESC ESC: emit the first, keep scanning from the second.
                    0x1b => {
                        out.push(0x1b);
                    }
                    _ => {
                        self.seq.push(byte);
                        self.flush_into(&mut out);
                    }
                },
                QueryScanState::Csi => match byte {
                    // Parameter and intermediate bytes.
                    0x20..=0x3f => {
                        self.seq.push(byte);
                        if self.seq.len() > QUERY_SCAN_MAX_SEQUENCE_BYTES {
                            self.flush_into(&mut out);
                        }
                    }
                    // Final byte terminates the CSI sequence.
                    0x40..=0x7e => {
                        self.seq.push(byte);
                        if Self::is_da1_query(&self.seq) {
                            self.seq.clear();
                            self.state = QueryScanState::Ground;
                            queries.push(HostAnsweredQuery::Da1);
                        } else if let Some(query) = intercept_probes
                            .then(|| Self::probe_query(&self.seq))
                            .flatten()
                        {
                            self.seq.clear();
                            self.state = QueryScanState::Ground;
                            queries.push(query);
                        } else {
                            self.flush_into(&mut out);
                        }
                    }
                    // ESC aborts the CSI and starts a new sequence.
                    0x1b => {
                        out.extend_from_slice(&self.seq);
                        self.seq.clear();
                        self.seq.push(0x1b);
                        self.state = QueryScanState::Escape;
                    }
                    // Embedded C0 control or other unexpected byte: stop
                    // treating this run as a query candidate.
                    _ => {
                        self.seq.push(byte);
                        self.flush_into(&mut out);
                    }
                },
                QueryScanState::Osc => match byte {
                    // BEL terminates the OSC.
                    0x07 => {
                        self.seq.push(byte);
                        if let Some(query) = Self::osc_color_query(&self.seq) {
                            self.seq.clear();
                            self.state = QueryScanState::Ground;
                            queries.push(query);
                        } else {
                            self.flush_into(&mut out);
                        }
                    }
                    // ESC: either the ST terminator (ESC \) or an abort.
                    0x1b => {
                        self.seq.push(byte);
                        self.state = QueryScanState::OscEscape;
                    }
                    _ => {
                        self.seq.push(byte);
                        if self.seq.len() > QUERY_SCAN_MAX_SEQUENCE_BYTES {
                            self.flush_into(&mut out);
                        }
                    }
                },
                QueryScanState::OscEscape => {
                    if byte == 0x5c {
                        self.seq.push(byte);
                        if let Some(query) = Self::osc_color_query(&self.seq) {
                            self.seq.clear();
                            self.state = QueryScanState::Ground;
                            queries.push(query);
                        } else {
                            self.flush_into(&mut out);
                        }
                    } else {
                        // Not an ST: abort the OSC candidate, restart on ESC.
                        let held = std::mem::take(&mut self.seq);
                        // Emit everything up to (not including) the trailing ESC.
                        out.extend_from_slice(&held[..held.len() - 1]);
                        self.seq.push(0x1b);
                        self.state = QueryScanState::Escape;
                        // Re-process the current byte in Escape state.
                        match byte {
                            b'Z' => {
                                self.seq.clear();
                                self.state = QueryScanState::Ground;
                                queries.push(HostAnsweredQuery::Da1);
                            }
                            b'[' => {
                                self.seq.push(byte);
                                self.state = QueryScanState::Csi;
                            }
                            b']' if intercept_probes => {
                                self.seq.push(byte);
                                self.state = QueryScanState::Osc;
                            }
                            0x1b => {
                                out.push(0x1b);
                            }
                            _ => {
                                self.seq.push(byte);
                                self.flush_into(&mut out);
                            }
                        }
                    }
                }
            }
        }

        (out, queries)
    }

    /// Drain any held partial sequence (used when the child exits so the last
    /// bytes still reach the log).
    fn take_pending(&mut self) -> Vec<u8> {
        self.state = QueryScanState::Ground;
        std::mem::take(&mut self.seq)
    }

    fn flush_into(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.seq);
        self.seq.clear();
        self.state = QueryScanState::Ground;
    }

    /// `ESC [ <digits/semicolons> c` with no private markers or intermediates
    /// is a Primary Device Attributes request (`CSI c`, `CSI 0 c`). `CSI > c`
    /// (DA2) and `CSI = c` (DA3) are deliberately not matched — the host only
    /// answers what it excises.
    fn is_da1_query(seq: &[u8]) -> bool {
        let Some((&final_byte, params)) = seq[2..].split_last() else {
            return false;
        };
        final_byte == b'c'
            && params
                .iter()
                .all(|byte| byte.is_ascii_digit() || *byte == b';')
    }

    /// Probe-gated CSI queries: CPR (`CSI 6 n`) and the kitty keyboard flags
    /// query (`CSI ? u`). `seq` starts with `ESC [`.
    fn probe_query(seq: &[u8]) -> Option<HostAnsweredQuery> {
        match &seq[2..] {
            b"6n" => Some(HostAnsweredQuery::CursorPosition),
            b"?u" => Some(HostAnsweredQuery::KittyFlags),
            _ => None,
        }
    }

    /// Probe-gated OSC color queries: `OSC 10 ; ?`, `OSC 11 ; ?` (fg/bg) and
    /// `OSC 4 ; <index> ; ?` (palette), BEL- or ST-terminated. `seq` starts
    /// with `ESC ]` and ends with the terminator.
    fn osc_color_query(seq: &[u8]) -> Option<HostAnsweredQuery> {
        let body = &seq[2..];
        let body = body
            .strip_suffix(&[0x07])
            .or_else(|| body.strip_suffix(&[0x1b, b'\\']))?;
        match body {
            b"10;?" => Some(HostAnsweredQuery::OscColor { code: 10 }),
            b"11;?" => Some(HostAnsweredQuery::OscColor { code: 11 }),
            _ => {
                let rest = body.strip_prefix(b"4;")?;
                let index_bytes = rest.strip_suffix(b";?")?;
                if index_bytes.is_empty() || index_bytes.len() > 3 {
                    return None;
                }
                let index: u16 = std::str::from_utf8(index_bytes).ok()?.parse().ok()?;
                (index < 256).then_some(HostAnsweredQuery::OscPalette { index })
            }
        }
    }
}

/// Longest `OSC 0/2` title payload the title scanner keeps. Real terminal
/// titles are short; anything longer is discarded outright (never truncated)
/// so a binary-garbage OSC can't become a session label.
const OSC_TITLE_MAX_PAYLOAD_BYTES: usize = 512;

/// Bytes a non-title or overflowed OSC string may run before the scanner
/// abandons it and returns to ground. Purely a hostile-stream bound: an
/// unterminated OSC must not swallow the rest of the session's output.
const OSC_TITLE_SKIP_BUDGET_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TitleScanState {
    Ground,
    Escape,
    /// After `ESC ]`, collecting the numeric code up to `;`.
    Code,
    /// Inside an `OSC 0/2` payload.
    Payload,
    /// Inside `Payload`, after ESC (deciding ST terminator vs abort).
    PayloadEscape,
    /// Inside any other (or overflowed) OSC string, consuming to its
    /// terminator without buffering.
    Skip,
    /// Inside `Skip`, after ESC.
    SkipEscape,
}

/// Passive scanner for `OSC 0` / `OSC 2` terminal-title updates in the hosted
/// child's output. Unlike [`OutputQueryScanner`] it never modifies the stream
/// — every byte has already been passed through, recorded, and broadcast; this
/// only observes completed title payloads so the Host can adopt an agent's own
/// semantic titles as the session label (see `apply_agent_terminal_title`).
struct OscTitleScanner {
    state: TitleScanState,
    code: u32,
    payload: Vec<u8>,
    skipped: usize,
    /// Last title emitted, so steady-state repaints that re-assert the same
    /// title cost nothing downstream.
    last_title: Option<String>,
}

impl OscTitleScanner {
    fn new() -> Self {
        Self {
            state: TitleScanState::Ground,
            code: 0,
            payload: Vec::new(),
            skipped: 0,
            last_title: None,
        }
    }

    /// Observe a chunk; returns the last *new* completed title in it, if any.
    fn scan(&mut self, input: &[u8]) -> Option<String> {
        let mut title = None;
        for &byte in input {
            match self.state {
                TitleScanState::Ground => {
                    if byte == 0x1b {
                        self.state = TitleScanState::Escape;
                    }
                }
                TitleScanState::Escape => {
                    self.state = match byte {
                        b']' => {
                            self.code = 0;
                            TitleScanState::Code
                        }
                        0x1b => TitleScanState::Escape,
                        _ => TitleScanState::Ground,
                    };
                }
                TitleScanState::Code => match byte {
                    b'0'..=b'9' => {
                        self.code = self.code.saturating_mul(10) + u32::from(byte - b'0');
                    }
                    b';' if self.code == 0 || self.code == 2 => {
                        self.payload.clear();
                        self.state = TitleScanState::Payload;
                    }
                    b';' => {
                        self.skipped = 0;
                        self.state = TitleScanState::Skip;
                    }
                    // BEL/ST here is an empty non-title OSC; anything else
                    // (including a bare ESC restarting a sequence) aborts.
                    0x1b => self.state = TitleScanState::Escape,
                    _ => self.state = TitleScanState::Ground,
                },
                TitleScanState::Payload => match byte {
                    0x07 => {
                        if let Some(text) = self.take_title() {
                            title = Some(text);
                        }
                        self.state = TitleScanState::Ground;
                    }
                    0x1b => self.state = TitleScanState::PayloadEscape,
                    _ => {
                        if self.payload.len() < OSC_TITLE_MAX_PAYLOAD_BYTES {
                            self.payload.push(byte);
                        } else {
                            // Oversized: not a human title. Drain to the
                            // terminator without keeping it.
                            self.payload.clear();
                            self.skipped = 0;
                            self.state = TitleScanState::Skip;
                        }
                    }
                },
                TitleScanState::PayloadEscape => {
                    if byte == 0x5c {
                        if let Some(text) = self.take_title() {
                            title = Some(text);
                        }
                        self.state = TitleScanState::Ground;
                    } else {
                        // Aborted OSC; the ESC starts a new sequence.
                        self.payload.clear();
                        self.state = TitleScanState::Escape;
                        if byte == b']' {
                            self.code = 0;
                            self.state = TitleScanState::Code;
                        } else if byte != 0x1b {
                            self.state = TitleScanState::Ground;
                        }
                    }
                }
                TitleScanState::Skip => {
                    self.skipped += 1;
                    match byte {
                        0x07 => self.state = TitleScanState::Ground,
                        0x1b => self.state = TitleScanState::SkipEscape,
                        _ if self.skipped > OSC_TITLE_SKIP_BUDGET_BYTES => {
                            self.state = TitleScanState::Ground;
                        }
                        _ => {}
                    }
                }
                TitleScanState::SkipEscape => {
                    self.state = match byte {
                        0x5c => TitleScanState::Ground,
                        0x1b => TitleScanState::SkipEscape,
                        b']' => {
                            self.code = 0;
                            TitleScanState::Code
                        }
                        _ => TitleScanState::Ground,
                    };
                }
            }
        }
        title
    }

    fn take_title(&mut self) -> Option<String> {
        let payload = std::mem::take(&mut self.payload);
        let text = String::from_utf8(payload).ok()?;
        let text: String = text
            .chars()
            .filter(|ch| !ch.is_control())
            .collect::<String>()
            .trim()
            .to_string();
        // Claude prefixes its semantic OSC title with this activity marker.
        // It is terminal chrome rather than part of the model-written task
        // summary, so normalize it before repaint deduplication and capping.
        let text = text
            .strip_prefix('⠂')
            .filter(|rest| rest.chars().next().is_some_and(char::is_whitespace))
            .map(str::trim_start)
            .unwrap_or(&text)
            .to_string();
        if text.is_empty() || self.last_title.as_deref() == Some(text.as_str()) {
            return None;
        }
        self.last_title = Some(text.clone());
        Some(if text.chars().count() > APP_TITLE_CAP_CHARS {
            let cut: String = text.chars().take(APP_TITLE_CAP_CHARS - 1).collect();
            format!("{cut}…")
        } else {
            text
        })
    }
}

/// macOS caps `sockaddr_un.sun_path` at 104 bytes (Linux 108); the bind fails
/// once the path plus its NUL no longer fits. A workspace home
/// (`~/.unpeel/profiles/<name>/app-sessions/<uuid>/session.sock`) is ~15 bytes
/// longer than the default home and tips over that limit, which killed every
/// session hosted in a non-default workspace. Stay conservative across both
/// platforms.
#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_LEN: usize = 100;

#[cfg(unix)]
pub fn socket_path(session_id: &str) -> PathBuf {
    let preferred = session_dir(session_id).join("session.sock");
    if preferred.as_os_str().len() <= MAX_UNIX_SOCKET_PATH_LEN {
        return preferred;
    }
    // Deterministic short fallback keyed by the globally-unique session id, so
    // every consumer (host, attach, gateway, MCP) computes the same path
    // regardless of how deep its UNPEEL_HOME is. The host creates the parent
    // 0700 before bind; only the ephemeral socket moves — manifest/output stay
    // in the session dir.
    let uid = unsafe { libc::getuid() };
    PathBuf::from("/tmp")
        .join(format!("unpeel-{uid}"))
        .join(format!("{session_id}.sock"))
}

#[cfg(unix)]
fn attach_ready_path(session_id: &str) -> PathBuf {
    session_dir(session_id).join(".attach-ready")
}

fn ensure_session_dir(session_id: &str) -> Result<(), String> {
    fs::create_dir_all(session_dir(session_id))
        .map_err(|e| format!("Failed to create session dir: {e}"))
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid parent path")?;
    fs::create_dir_all(parent).map_err(|e| format!("Failed to create parent dir: {e}"))?;
    let json =
        serde_json::to_vec_pretty(value).map_err(|e| format!("Failed to serialize json: {e}"))?;
    // Atomic replace, never truncate-in-place: manifests are read lock-free
    // by every frontend (sidebar scans, the live stream's health check, the
    // hook server), and a reader that catches a half-written file parses
    // nothing and concludes the session is gone — which killed live preview
    // streams mid-output ("torn manifest decode mid-heartbeat"). The tmp
    // name carries the pid so concurrent writers (host heartbeat + an
    // observer-side health refresh) never scribble over each other's tmp.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, json).map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("Failed to replace {}: {e}", path.display()))
}

fn manifest_health_cache() -> &'static Mutex<HashMap<String, CachedManifestHealth>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedManifestHealth>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Number of recent input idempotency keys the host remembers per process.
/// One host process serves one session, so this is a per-session window. A
/// remote controller only ever retries the single most-recent send it could
/// not confirm, so a small window is ample; the cap bounds memory against a
/// misbehaving or malicious client flooding unique keys.
const WRITE_ID_HISTORY: usize = 256;
/// Every transport adapter applies this limit, and the session host enforces
/// it again at the authority boundary so a raw socket caller cannot turn the
/// bounded entry count into unbounded retained memory.
const WRITE_ID_MAX_BYTES: usize = 128;

fn validate_write_id(write_id: Option<&str>) -> Result<Option<&str>, &'static str> {
    match write_id {
        None | Some("") => Ok(None),
        Some(write_id) if write_id.len() > WRITE_ID_MAX_BYTES => Err("write_id exceeds 128 bytes"),
        Some(write_id) => Ok(Some(write_id)),
    }
}

#[derive(Default)]
struct RecentWriteIds {
    order: VecDeque<String>,
    seen: HashSet<String>,
}

impl RecentWriteIds {
    fn contains(&self, write_id: &str) -> bool {
        self.seen.contains(write_id)
    }

    /// Record only after the PTY write succeeds. Keeping this history inside
    /// `HostRuntime` lets the caller serialize check → write → record under
    /// the same runtime lock; a failed first delivery therefore remains
    /// retryable, and two racing transports cannot both apply the bytes.
    fn record_applied(&mut self, write_id: &str) {
        if !self.seen.insert(write_id.to_string()) {
            return;
        }
        self.order.push_back(write_id.to_string());
        while self.order.len() > WRITE_ID_HISTORY {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
    }
}

fn cache_manifest_health(session_id: &str, manifest: Option<HostedSessionManifest>) {
    let now = current_timestamp_ms();
    let mut cache = manifest_health_cache().lock().unwrap();
    // Entries for dead sessions are never explicitly evicted, so drop stale
    // ones once the cache grows past the prune threshold to keep it bounded.
    if cache.len() >= MANIFEST_HEALTH_CACHE_PRUNE_LEN {
        cache.retain(|_, cached| {
            now.saturating_sub(cached.checked_at) < MANIFEST_HEALTH_CACHE_STALE_MS
        });
    }
    cache.insert(
        session_id.to_string(),
        CachedManifestHealth {
            checked_at: now,
            manifest,
        },
    );
}

fn clear_cached_manifest_health(session_id: &str) {
    manifest_health_cache().lock().unwrap().remove(session_id);
}

fn refresh_manifest_health_cached(
    session_id: &str,
    max_age_ms: u64,
) -> Option<HostedSessionManifest> {
    let now = current_timestamp_ms();
    if let Some(cached) = manifest_health_cache()
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
    {
        if now.saturating_sub(cached.checked_at) <= max_age_ms {
            return cached.manifest;
        }
    }

    let manifest = refresh_manifest_health(session_id);
    cache_manifest_health(session_id, manifest.clone());
    manifest
}

fn refresh_manifest_health_from_loaded_manifest_cached(
    manifest: HostedSessionManifest,
    max_age_ms: u64,
) -> HostedSessionManifest {
    let session_id = manifest.session.id.clone();
    let now = current_timestamp_ms();
    if let Some(cached) = manifest_health_cache()
        .lock()
        .unwrap()
        .get(&session_id)
        .cloned()
    {
        if now.saturating_sub(cached.checked_at) <= max_age_ms {
            if let Some(cached_manifest) = cached.manifest {
                return cached_manifest;
            }
        }
    }

    let refreshed = refresh_manifest_health_from_manifest(manifest);
    cache_manifest_health(&session_id, Some(refreshed.clone()));
    refreshed
}

fn manifest_lock_target(session_id: &str) -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};

    let home = crate::app_paths::ensure_unpeel_home().map_err(|error| error.to_string())?;
    let lock_dir = home.join("session-manifest-locks");
    fs::create_dir_all(&lock_dir).map_err(|error| error.to_string())?;
    Ok(lock_dir.join(format!("{:x}", Sha256::digest(session_id.as_bytes()))))
}

fn save_manifest_unlocked(manifest: &HostedSessionManifest) -> Result<(), String> {
    write_json_file(&manifest_path(&manifest.session.id), manifest)?;
    cache_manifest_health(&manifest.session.id, Some(manifest.clone()));
    Ok(())
}

pub fn save_manifest(manifest: &HostedSessionManifest) -> Result<(), String> {
    let _manifest_lock =
        crate::app_state::lock_exclusive(&manifest_lock_target(&manifest.session.id)?)?;
    save_manifest_unlocked(manifest)
}

pub fn load_manifest(session_id: &str) -> Option<HostedSessionManifest> {
    let raw = fs::read(manifest_path(session_id)).ok()?;
    serde_json::from_slice(&raw).ok()
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(not(unix))]
fn process_exists(_pid: u32) -> bool {
    true
}

/// Tolerance when comparing a manifest's recorded pid start time against the
/// kernel-reported one. The manifest value is captured within milliseconds of
/// spawn; 10s absorbs clock rounding and a loaded machine while staying far
/// below any realistic pid-reuse interval.
const PID_START_TOLERANCE_MS: u64 = 10_000;

/// Whether a manifest's recorded pid still refers to the session's own child
/// process, or the pid has been recycled by the OS since the child died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidIdentity {
    /// Positively verified: the live process is the recorded child.
    Matches,
    /// Positively refuted: the pid exists but belongs to an unrelated
    /// process (recycled). Treat the session as already dead; never signal.
    NotOurs,
    /// Cannot prove either way (legacy manifest without `pid_started_at`
    /// whose child has exec'd away the identifying argv, or the kernel query
    /// failed). Safe default: never force-kill, never declare dead.
    Unknown,
}

/// Kernel-reported start time of `pid` in ms since the epoch — the half of a
/// process identity that a bare pid lacks. Record it next to every pid you
/// persist (`pid_started_at`) so `recorded_pid_identity` can verify the
/// process later. `None` when the process is gone or the kernel query failed.
#[cfg(target_os = "macos")]
pub fn process_start_time_ms(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let rc = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if rc != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000)
}

#[cfg(target_os = "linux")]
pub fn process_start_time_ms(pid: u32) -> Option<u64> {
    // /proc/<pid>/stat: field 22 is starttime in clock ticks since boot; the
    // comm field can contain spaces, so parse from after the closing paren.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit(')').next()?;
    let start_ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks_per_sec <= 0 {
        return None;
    }
    let btime: u64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    Some(btime * 1000 + start_ticks * 1000 / ticks_per_sec as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn process_start_time_ms(_pid: u32) -> Option<u64> {
    None
}

/// Best-effort argv probe for manifests that predate `pid_started_at`: the
/// hosted child is spawned as `zsh -l -i -c "<script>"` whose script embeds
/// the session id several times, so a positive hit is definitive. A miss is
/// NOT — once the agent exits, the wrapper execs a plain interactive shell
/// whose argv no longer mentions the session.
fn process_argv_mentions(pid: u32, needle: &str) -> Option<bool> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let command = String::from_utf8_lossy(&output.stdout);
    if command.trim().is_empty() {
        return None;
    }
    Some(command.contains(needle))
}

/// Public because **every frontend must agree on whether a session is
/// alive**. The desktop already refuses to call a session live when its pid
/// was recycled (`manifestPidIdentity` in UnpeelStore.swift); a frontend that
/// checks only `kill(pid, 0)` will show stopped sessions as running, because
/// the pid counter wraps in under an hour under agent load.
pub fn manifest_pid_identity(manifest: &HostedSessionManifest) -> PidIdentity {
    let Some(pid) = manifest.pid else {
        return PidIdentity::Unknown;
    };
    if let (Some(recorded), Some(actual)) = (manifest.pid_started_at, process_start_time_ms(pid)) {
        return if actual.abs_diff(recorded) <= PID_START_TOLERANCE_MS {
            PidIdentity::Matches
        } else {
            PidIdentity::NotOurs
        };
    }
    match process_argv_mentions(pid, &manifest.session.id) {
        Some(true) => PidIdentity::Matches,
        _ => PidIdentity::Unknown,
    }
}

/// Start-time identity check for any recorded helper process (for example
/// the `__remote__` streamer named in `remote.json`). `Matches` only when the
/// live process provably started when the record says; a record without a
/// start time, or a kernel query failure, is `Unknown` and must never be
/// signaled. A dead pid is reported as `NotOurs` so callers treat it as gone.
pub fn recorded_pid_identity(pid: u32, recorded_started_at_ms: Option<u64>) -> PidIdentity {
    if !process_exists(pid) {
        return PidIdentity::NotOurs;
    }
    match (recorded_started_at_ms, process_start_time_ms(pid)) {
        (Some(recorded), Some(actual)) => {
            if actual.abs_diff(recorded) <= PID_START_TOLERANCE_MS {
                PidIdentity::Matches
            } else {
                PidIdentity::NotOurs
            }
        }
        _ => PidIdentity::Unknown,
    }
}

/// Whether a Running manifest that has not yet recorded its child pid is
/// still inside a live Host's launch window. The shared PTY core publishes
/// `pid: None` for the preliminary manifest (see `run_host`), so readers must
/// consult the additive `host_pid` before treating the record as dead. The
/// identity check is exact: a recycled host pid, or one without a recorded
/// start time, proves nothing.
pub fn manifest_launching_host_is_alive(manifest: &HostedSessionManifest) -> bool {
    if manifest.state != HostedSessionState::Running || manifest.pid.is_some() {
        return false;
    }
    let Some(host_pid) = manifest.host_pid else {
        return false;
    };
    recorded_pid_identity(host_pid, manifest.host_pid_started_at) == PidIdentity::Matches
}

/// Liveness as every frontend should compute it: a Running record whose
/// child pid is alive and provably ours, or a launching record whose Host
/// is alive. Never use the result as a kill target.
pub fn manifest_is_live(manifest: &HostedSessionManifest) -> bool {
    if manifest.state != HostedSessionState::Running {
        return false;
    }
    match manifest.pid {
        Some(pid) => process_exists(pid) && manifest_pid_identity(manifest) != PidIdentity::NotOurs,
        None => manifest_launching_host_is_alive(manifest),
    }
}

fn manifest_host_is_healthy(manifest: &HostedSessionManifest) -> bool {
    if manifest.state != HostedSessionState::Running {
        return true;
    }

    let Some(pid) = manifest.pid else {
        // A core-hosted Session between its preliminary manifest and PTY
        // spawn: healthy while the core is alive, exactly as a per-process
        // Host's own-pid placeholder counted as healthy before.
        return manifest_launching_host_is_alive(manifest);
    };
    if !process_exists(pid) {
        return false;
    }
    // A recycled pid must not keep a dead session looking alive: if the live
    // process provably started at a different time than the recorded child,
    // the child is gone. (Start-time check only — this runs on every health
    // refresh, so no ps-based argv fallback here.)
    if let (Some(recorded), Some(actual)) = (manifest.pid_started_at, process_start_time_ms(pid)) {
        if actual.abs_diff(recorded) > PID_START_TOLERANCE_MS {
            return false;
        }
    }

    #[cfg(unix)]
    if !socket_path(&manifest.session.id).exists() {
        return false;
    }

    true
}

/// Whether it is safe to convert a stale Running record into Exited. Missing
/// control-plane evidence is not process-death evidence: a Host can lose or
/// delay its socket while the exact recorded child remains alive. Replacement
/// Resume deletes the old Session directory, so it may proceed only after the
/// child is absent or the PID is positively proven recycled.
fn manifest_child_is_definitively_gone(manifest: &HostedSessionManifest) -> bool {
    let Some(pid) = manifest.pid else {
        // Current Hosts publish a placeholder PID before spawn and replace it
        // with the child PID afterward. A legacy/incomplete Running record
        // without either identity is unknowable, not proof of process death.
        return false;
    };
    if !process_exists(pid) {
        return true;
    }
    manifest_pid_identity(manifest) == PidIdentity::NotOurs
}

fn manifest_last_heartbeat_at(manifest: &HostedSessionManifest) -> u64 {
    if manifest.heartbeat_at > 0 {
        manifest.heartbeat_at
    } else {
        manifest.updated_at
    }
}

fn manifest_heartbeat_is_stale(manifest: &HostedSessionManifest, now_ms: u64) -> bool {
    now_ms.saturating_sub(manifest_last_heartbeat_at(manifest)) > SESSION_HEARTBEAT_STALE_MS
}

fn refresh_manifest_health_from_manifest(manifest: HostedSessionManifest) -> HostedSessionManifest {
    let session_id = manifest.session.id.clone();
    if manifest.state == HostedSessionState::Running
        && manifest_heartbeat_is_stale(&manifest, current_timestamp_ms())
        && ping_session_host(&session_id, Duration::from_millis(SESSION_PING_TIMEOUT_MS))
    {
        return update_manifest_session(&session_id, |current| {
            if current.state == HostedSessionState::Running && manifest_host_is_healthy(current) {
                current.heartbeat_at = current_timestamp_ms();
            }
        })
        .ok()
        .flatten()
        .unwrap_or(manifest);
    }

    if manifest_host_is_healthy(&manifest) {
        return manifest;
    }

    if manifest.state != HostedSessionState::Exited {
        return update_manifest_session(&session_id, |current| {
            // A live Host can recover between the caller's lock-free read and
            // this locked mutation. Revalidate the latest document rather
            // than filing an exited state from stale evidence.
            if current.state != HostedSessionState::Exited
                && !manifest_host_is_healthy(current)
                && manifest_child_is_definitively_gone(current)
            {
                current.state = HostedSessionState::Exited;
                current.exit_code = current.exit_code.or(Some(-1));
                current.runtime_launch_pending = false;
            }
        })
        .ok()
        .flatten()
        .unwrap_or(manifest);
    }

    manifest
}

pub fn refresh_manifest_health(session_id: &str) -> Option<HostedSessionManifest> {
    let manifest = load_manifest(session_id)?;
    Some(refresh_manifest_health_from_manifest(manifest))
}

pub fn list_manifests() -> Vec<HostedSessionManifest> {
    let root = app_sessions_root();
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    let mut manifests = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("manifest.json");
        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<HostedSessionManifest>(&raw) else {
            continue;
        };
        let refreshed = refresh_manifest_health_from_loaded_manifest_cached(
            manifest,
            MANIFEST_HEALTH_CACHE_TTL_MS,
        );
        manifests.push(refreshed);
    }
    manifests
}

pub fn list_manifests_for_project(project_id: &str) -> Vec<HostedSessionManifest> {
    let root = app_sessions_root();
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    let mut manifests = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("manifest.json");
        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<HostedSessionManifest>(&raw) else {
            continue;
        };
        if manifest.session.project_id != project_id {
            continue;
        }
        let refreshed = refresh_manifest_health_from_loaded_manifest_cached(
            manifest,
            MANIFEST_HEALTH_CACHE_TTL_MS,
        );
        manifests.push(refreshed);
    }
    manifests
}

pub fn update_manifest_session<F>(
    session_id: &str,
    update: F,
) -> Result<Option<HostedSessionManifest>, String>
where
    F: FnOnce(&mut HostedSessionManifest),
{
    // One hosted session is one process, but heartbeat, viewport, runtime
    // observation, and control handling update its manifest from different
    // threads. Serialize their read-modify-write cycles so an edge-only field
    // (notably `runtime.currentObservation`) cannot be overwritten by a thread
    // that read the preceding manifest just before the edge landed.
    static UPDATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = UPDATE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Native and Rust processes both patch this document. Atomic rename keeps
    // readers from seeing torn JSON, but it cannot prevent two independent
    // read-modify-write cycles from dropping one another's fields. Coordinate
    // them on a stable path outside the removable Session directory. This is
    // deliberately not the lifecycle lock: callers hold that while waiting
    // for this Host and taking it here would deadlock.
    let _manifest_lock = crate::app_state::lock_exclusive(&manifest_lock_target(session_id)?)?;
    let Some(mut manifest) = load_manifest(session_id) else {
        return Ok(None);
    };
    update(&mut manifest);
    manifest.updated_at = current_timestamp_ms();
    save_manifest_unlocked(&manifest)?;
    Ok(Some(manifest))
}

/// Whether a token references an image file: a path or URL ending in an
/// image extension. Dragging a screenshot into a terminal pastes exactly
/// such a token, which makes a useless session title.
fn is_image_ref(token: &str) -> bool {
    let token = token.trim();
    let looks_like_ref = token.starts_with('/')
        || token.starts_with("~/")
        || token.starts_with("./")
        || token.starts_with("file://")
        || token.starts_with("http://")
        || token.starts_with("https://");
    if !looks_like_ref {
        return false;
    }
    // URLs may carry a query/fragment after the extension.
    let path = token.split(['?', '#']).next().unwrap_or(token);
    let Some((_, ext)) = path.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "tiff"
            | "tif"
            | "heic"
            | "heif"
            | "svg"
            | "ico"
            | "icns"
            | "avif"
    )
}

/// Strip one leading image reference — either a quoted path (drag-and-drop
/// pastes `'/var/folders/…/Screenshot ….png'`, spaces and all) or a bare
/// whitespace-free path/URL token. Returns the remainder, or `None` when
/// the input does not start with an image reference.
fn strip_leading_image_ref(input: &str) -> Option<&str> {
    let first = input.chars().next()?;
    if first == '\'' || first == '"' {
        let body_and_rest = &input[first.len_utf8()..];
        let end = body_and_rest.find(first)?;
        if is_image_ref(&body_and_rest[..end]) {
            return Some(&body_and_rest[end + first.len_utf8()..]);
        }
        return None;
    }
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    if is_image_ref(&input[..end]) {
        return Some(&input[end..]);
    }
    None
}

/// Trim, strip leading prompt glyphs and pasted image paths/URLs, and
/// collapse whitespace from a typed line so it can be used as a session
/// title. Returns `None` when nothing usable remains (e.g. the prompt was
/// only an image), so titling falls through to the next prompt. Titles are
/// capped at 96 bytes.
pub fn normalize_prompt_title(input: &str) -> Option<String> {
    let mut trimmed = input
        .trim()
        .trim_start_matches(['>', '$', '#', '%', ':', '|', '·'])
        .trim_start();
    while let Some(rest) = strip_leading_image_ref(trimmed) {
        trimmed = rest.trim_start();
    }
    if trimmed.is_empty() {
        return None;
    }
    if is_slash_command_line(trimmed) {
        return None;
    }

    let mut collapsed = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                collapsed.push(' ');
                last_was_space = true;
            }
            continue;
        }
        last_was_space = false;
        collapsed.push(ch);
        if collapsed.len() >= 96 {
            break;
        }
    }

    let collapsed = collapsed.trim();
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed.to_string())
    }
}

/// True when a submitted line is a CLI slash command (`/resume`, `/model
/// opus`, `/plugin:skill args`) rather than a prompt. Every agent CLI has
/// these and none of them make a useful title, so titling falls through to
/// the next real prompt — notably a fresh session whose first line is
/// `/resume` no longer stays titled "/resume" forever. The discriminator is
/// a single-segment leading token: absolute paths (`/tmp/build.log is huge`)
/// contain a second `/` or a dot and still title.
fn is_slash_command_line(line: &str) -> bool {
    let Some(first) = line.split_whitespace().next() else {
        return false;
    };
    let Some(name) = first.strip_prefix('/') else {
        return false;
    };
    name.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':'))
}

/// Scan a chunk of raw terminal input for the line the user submits with
/// Enter. `buffer` holds the partially-typed line and persists across
/// chunks; escape sequences (CSI/OSC/DCS/APC/SOS/PM) are skipped, backspace edits the
/// buffer, Ctrl+U clears it. Returns the first submitted line of the chunk,
/// normalized via `normalize_prompt_title`.
/// Cap on the partially-typed line buffer. A title is normalized to <100 bytes,
/// so this is generous headroom for in-line editing while stopping a giant paste
/// with no trailing newline from ballooning the buffer before the first prompt
/// latches. Once reached, further characters are ignored until the next newline
/// clears the buffer; the leading text (which becomes the title) is preserved.
const AUTO_TITLE_BUFFER_MAX_CHARS: usize = 4096;

pub fn extract_submitted_prompt(buffer: &mut String, data: &str) -> Option<String> {
    #[derive(Clone, Copy)]
    enum ParseState {
        Normal,
        Escape,
        Csi,
        Osc,
        OscEscape,
        Dcs,
        DcsEscape,
    }

    let mut state = ParseState::Normal;
    let mut candidate: Option<String> = None;

    for ch in data.chars() {
        match state {
            ParseState::Normal => match ch {
                '\u{1b}' => state = ParseState::Escape,
                '\r' | '\n' => {
                    if candidate.is_none() {
                        candidate = normalize_prompt_title(buffer);
                    }
                    buffer.clear();
                }
                '\u{8}' | '\u{7f}' => {
                    buffer.pop();
                }
                '\u{15}' => {
                    buffer.clear();
                }
                '\t' if buffer.len() < AUTO_TITLE_BUFFER_MAX_CHARS => buffer.push(' '),
                '\t' => {}
                ch if ch.is_control() => {}
                _ if buffer.len() < AUTO_TITLE_BUFFER_MAX_CHARS => buffer.push(ch),
                _ => {}
            },
            ParseState::Escape => match ch {
                '[' => state = ParseState::Csi,
                ']' => state = ParseState::Osc,
                // DCS, APC, SOS, PM are all ST-terminated strings. APC matters
                // here: terminals answer kitty-graphics probes on stdin with
                // `ESC _ G i=31337;OK ESC \`, which must not leak into titles.
                'P' | '_' | 'X' | '^' => state = ParseState::Dcs,
                _ => state = ParseState::Normal,
            },
            ParseState::Csi => {
                if ('@'..='~').contains(&ch) {
                    state = ParseState::Normal;
                }
            }
            ParseState::Osc => match ch {
                '\u{7}' => state = ParseState::Normal,
                '\u{1b}' => state = ParseState::OscEscape,
                _ => {}
            },
            ParseState::OscEscape => {
                state = ParseState::Normal;
            }
            ParseState::Dcs => match ch {
                '\u{7}' => state = ParseState::Normal,
                '\u{1b}' => state = ParseState::DcsEscape,
                _ => {}
            },
            ParseState::DcsEscape => {
                state = ParseState::Normal;
            }
        }
    }

    candidate
}

/// Auto-title a hosted session's manifest from a submitted prompt — the
/// host-side mirror of `apply_prompt_title_if_needed` in pty_manager.rs.
/// This covers clients that write straight to the control socket (the
/// native attach client, MCP `send_text`); the Tauri frontend applies the
/// same rule app-side, so both converge on the same label.
///
/// Returns `true` once titling is permanently settled for this session
/// (renamed by the user, or already auto-titled), so callers can stop
/// tracking input.
pub fn apply_manifest_auto_title(session_id: &str, candidate: &str) -> bool {
    // Shared knob: Off disables every automatic title source. Not "settled" —
    // flipping the mode back mid-session resumes titling on the next prompt.
    if session_title_mode() == crate::state::SessionTitleMode::Off {
        return false;
    }
    let Some(next_label) = normalize_prompt_title(candidate) else {
        return false;
    };
    let Some(manifest) = load_manifest(session_id) else {
        return false;
    };

    let session = &manifest.session;
    let is_blank_terminal = session.command.is_empty();
    if session.custom_title {
        return true;
    }
    // "Label differs from command" used to mean "already auto-titled", but the
    // label holds the *display* command while spawn decorates the real command
    // with appended flags (minted `--session-id`, pi `--session-dir`, restart
    // resume flags). A label that is still a prefix of the command is the
    // untitled initial state; only a non-prefix label means titling settled.
    if !is_blank_terminal && !session.command.starts_with(session.label.as_str()) {
        return true;
    }
    if session.label == next_label {
        return false;
    }

    // For blank terminals, mark as custom_title so we only auto-title once.
    let mark_custom = is_blank_terminal;
    let _ = update_manifest_session(session_id, |manifest| {
        manifest.session.label = next_label.clone();
        manifest.session.custom_title = mark_custom;
    });
    true
}

// Unit tests must not depend on the developer's real `app-state.json`
// (`app_state::load` resolves the process-global home); they pin the mode
// here instead. Defaults to the production default.
#[cfg(test)]
thread_local! {
    static SESSION_TITLE_MODE_FOR_TEST: std::cell::Cell<crate::state::SessionTitleMode> =
        const { std::cell::Cell::new(crate::state::SessionTitleMode::Agent) };
}

/// The shared session-title knob, read fresh from `app-state.json` so a
/// Settings change applies to running hosts without a restart. Reads are
/// edge-triggered (a submitted prompt, a *new* OSC title), never per-chunk.
/// Unknown future values fall back to the default, mirroring the
/// `SessionTitleMode` serde contract.
fn session_title_mode() -> crate::state::SessionTitleMode {
    #[cfg(test)]
    return SESSION_TITLE_MODE_FOR_TEST.with(|cell| cell.get());
    #[cfg(not(test))]
    {
        use crate::state::SessionTitleMode;
        match crate::app_state::load()
            .ok()
            .as_ref()
            .and_then(|state| state.get("session_title_mode"))
            .and_then(|value| value.as_str())
        {
            Some("first_prompt") => SessionTitleMode::FirstPrompt,
            Some("agent") => SessionTitleMode::Agent,
            Some("off") => SessionTitleMode::Off,
            _ => SessionTitleMode::Agent,
        }
    }
}

/// Whether the observed foreground runtime declares meaningful `OSC 0/2`
/// terminal titles (`semantic_terminal_title` in its runtime package).
/// Observations publish the compatibility `legacy_slug`.
fn runtime_reports_semantic_titles(runtime_id: &str) -> bool {
    crate::runtime_catalog::builtin_runtime_catalog()
        .by_legacy_slug(runtime_id)
        .is_some_and(|runtime| {
            runtime
                .capabilities
                .contains(&crate::runtime_catalog::RuntimeCapability::SemanticTerminalTitle)
        })
}

/// Adopt an agent-written terminal title (`OSC 0/2`) as the session label.
/// Only applies while the shared mode is `agent` AND the observed foreground
/// runtime declares `semantic_terminal_title` — a shell's cwd spam, ssh's
/// hostnames, or vim's filenames never retitle the row. Like the App-title
/// marker it keeps following updates; a user's custom title permanently wins
/// (returns `true` so the caller can stop the disk work, not the scan).
fn apply_agent_terminal_title(session_id: &str, title: &str) -> bool {
    if session_title_mode() != crate::state::SessionTitleMode::Agent {
        return false;
    }
    let Some(manifest) = load_manifest(session_id) else {
        return false;
    };
    if manifest.session.custom_title {
        return true;
    }
    let semantic = active_runtime_id(&manifest).is_some_and(runtime_reports_semantic_titles);
    if !semantic || manifest.session.label == title {
        return false;
    }
    let title = title.to_string();
    let _ = update_manifest_session(session_id, |manifest| {
        // Re-check under the manifest lock; a rename may have landed since.
        if !manifest.session.custom_title {
            manifest.session.label = title.clone();
        }
    });
    false
}

/// App-reported session title: the `app-title.json` marker an App writes
/// beside `status.json` to say what it is currently showing ("hero.md",
/// a picked note, a design folder). The Host folds it into the manifest
/// label like an auto-title — declared by the App, never guessed from the
/// screen — and unlike the one-shot prompt auto-title it keeps following
/// the marker, so returning to a picker and opening another document
/// retitles the session. A user's custom title always wins and permanently
/// stops applications; an empty or removed marker leaves the last label.
const APP_TITLE_MARKER: &str = "app-title.json";
const APP_TITLE_CAP_BYTES: u64 = 4 * 1024;
const APP_TITLE_CAP_CHARS: usize = 80;

fn read_app_title_marker(dir: &Path) -> Option<String> {
    let path = dir.join(APP_TITLE_MARKER);
    if fs::metadata(&path).ok()?.len() > APP_TITLE_CAP_BYTES {
        return None;
    }
    let raw = fs::read(&path).ok()?;
    let text = serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()?
        .get("text")?
        .as_str()?
        .trim()
        .replace(['\n', '\r'], " ");
    if text.is_empty() {
        return None;
    }
    Some(if text.chars().count() > APP_TITLE_CAP_CHARS {
        let cut: String = text.chars().take(APP_TITLE_CAP_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        text
    })
}

/// App-reported live context: the `app-context.json` marker an App writes
/// beside `app-title.json` to publish what it is currently showing or has
/// selected — a file plus line span, a heading, a view — in a structured,
/// app-defined shape (`{"app": id, "context": {...}, "updated_at": ms}`).
/// Unlike the title it is never folded into Host state: MCP pane-context
/// queries read it fresh per call and surface it verbatim on the Session's
/// neighbor entry, so selection-frequency updates cost no manifest churn or
/// state-bus pings. It is app-authored data, never instructions; the App's
/// skill documents its schema, and it is only exposed while the pane is
/// currently branded as an App, so a marker left behind by an exited App
/// never speaks for the shell that remains.
const APP_CONTEXT_MARKER: &str = "app-context.json";
const APP_CONTEXT_CAP_BYTES: u64 = 16 * 1024;

pub fn read_app_context_marker(session_id: &str) -> Option<serde_json::Value> {
    let path = session_dir(session_id).join(APP_CONTEXT_MARKER);
    if fs::metadata(&path).ok()?.len() > APP_CONTEXT_CAP_BYTES {
        return None;
    }
    let raw = fs::read(&path).ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&raw).ok()?;
    value.is_object().then_some(value)
}

/// Apply an App-reported title to the manifest label. Returns `true` when
/// titling is permanently settled for this session (user custom title), so
/// the caller can stop watching the marker.
pub fn apply_app_title(session_id: &str, title: &str) -> bool {
    let Some(manifest) = load_manifest(session_id) else {
        return false;
    };
    if manifest.session.custom_title {
        return true;
    }
    if manifest.session.label == title {
        return false;
    }
    let title = title.to_string();
    let _ = update_manifest_session(session_id, |manifest| {
        // Re-check under the manifest lock; a rename may have landed since.
        if !manifest.session.custom_title {
            manifest.session.label = title.clone();
        }
    });
    false
}

pub fn cleanup_session_artifacts(session_id: &str) -> Result<(), String> {
    let dir = session_dir(session_id);
    clear_cached_manifest_health(session_id);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("Failed to remove session dir: {e}"))?;
    }
    Ok(())
}

/// How long a session must be idle (no heartbeat/update) before it is
/// considered stale and eligible for reaping — 24 hours.
const REAP_STALE_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// Kill stale session host processes and remove orphaned session directories.
///
/// A running session is considered stale when:
///   - its process is already dead, OR
///   - its last heartbeat/update is older than `REAP_STALE_AGE_MS` AND
///     it fails a socket ping (i.e. truly unreachable, not just idle).
///
/// Intended to run once on app startup in a background thread.
pub fn reap_dead_sessions(saved_session_ids: &HashSet<String>) {
    let now = current_timestamp_ms();

    // Phase 1 – kill stale host processes and mark them exited.
    let manifests = list_manifests();
    for manifest in &manifests {
        if manifest.state != HostedSessionState::Running {
            continue;
        }
        let Some(pid) = manifest.pid else { continue };

        if !process_exists(pid) {
            // Process already gone – just mark exited on disk.
            mark_manifest_exited(&manifest.session.id);
            continue;
        }

        let identity = manifest_pid_identity(manifest);
        if identity == PidIdentity::NotOurs {
            // The pid exists but was recycled onto an unrelated process —
            // the session's child is long dead. Mark exited; signaling the
            // pid would kill an innocent process group.
            mark_manifest_exited(&manifest.session.id);
            continue;
        }

        // Only consider killing sessions that haven't been updated recently.
        let age = now.saturating_sub(manifest_last_heartbeat_at(manifest));
        if age < REAP_STALE_AGE_MS {
            continue;
        }

        // Old session – confirm it's truly dead by pinging its socket.
        if ping_session_host(&manifest.session.id, Duration::from_millis(500)) {
            continue;
        }

        // Stale and unreachable → terminate, but only when the pid is
        // positively identified as our own child. An unprovable identity
        // (legacy manifest) is marked exited without signaling: worst case
        // an orphaned shell lingers, which beats group-killing a stranger.
        if identity != PidIdentity::Matches {
            mark_manifest_exited(&manifest.session.id);
            continue;
        }
        #[cfg(unix)]
        if pid > 1 {
            unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
        }
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            if !process_exists(pid) {
                break;
            }
        }
        #[cfg(unix)]
        if process_exists(pid) && pid > 1 {
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
        mark_manifest_exited(&manifest.session.id);
    }

    // Phase 2 – remove directories for sessions that are dead and not saved.
    let root = app_sessions_root();
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir_name = entry.file_name().to_string_lossy().to_string();

        if saved_session_ids.contains(&dir_name) {
            continue;
        }

        let manifest_path = entry.path().join("manifest.json");
        let manifest = fs::read(&manifest_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<HostedSessionManifest>(&raw).ok());

        let should_remove = match &manifest {
            None => true,
            Some(m) if m.state == HostedSessionState::Exited => true,
            Some(m) if m.state == HostedSessionState::Running => match m.pid {
                Some(pid) => !process_exists(pid),
                // A launching core-hosted Session has no child pid yet; its
                // directory belongs to a live Host until that Host is gone.
                None => !manifest_launching_host_is_alive(m),
            },
            _ => false,
        };

        if should_remove {
            clear_cached_manifest_health(&dir_name);
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

pub(crate) fn mark_manifest_exited(session_id: &str) {
    let _ = update_manifest_session(session_id, |manifest| {
        manifest.state = HostedSessionState::Exited;
        manifest.pid = None;
        manifest.runtime_launch_pending = false;
    });
}

/// Why a leftover per-process session host was reaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapReason {
    /// The session's manifest is already `exited`.
    Exited,
    /// The session has been archived (an `archived.json` marker is present).
    Archived,
}

impl ReapReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ReapReason::Exited => "exited",
            ReapReason::Archived => "archived",
        }
    }
}

/// A per-process `__session_host__` that was still running after its session
/// was filed, and the reaper terminated it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedHost {
    pub session_id: String,
    pub host_pid: u32,
    pub reason: ReapReason,
}

/// Terminate per-process session hosts that are STILL ALIVE even though their
/// session is filed — manifest `exited`, or an `archived.json` marker present.
///
/// This is the leak behind the load-average blowup: a per-process host that
/// never exited keeps running its observer / login-shell PATH-probe loop
/// forever, one interactive shell per timer tick. `reap_dead_sessions` only
/// normalizes *Running* manifests and garbage-collects dead *directories*; it
/// never kills a live host whose session already finished. This does.
///
/// Kill discipline (mirrors `reap_dead_sessions`, never a name match):
///   - only the additive `host_pid` is ever signaled — the `__session_host__`
///     process, which `setsid`s at spawn, so `kill(-host_pid)` hits exactly
///     that host's own process group (host + its PTY child), never the
///     worker's;
///   - only when `recorded_pid_identity(host_pid, host_pid_started_at)` is
///     `Matches` (a recycled pid is `NotOurs`, an unverifiable one `Unknown` —
///     both left untouched);
///   - never the shared PTY core (`host_pid == core pid`, or an argv that
///     mentions `__pty_core__`);
///   - and only when the live argv actually contains `__session_host__`.
///
/// Returns what it reaped, newest logic first, for the trace log and the
/// `unpeel hosts prune` verb. Safe to run repeatedly.
pub fn reap_orphan_session_hosts() -> Vec<ReapedHost> {
    let core_pid = crate::pty_core::load_record().map(|record| record.pid);
    let mut reaped = Vec::new();

    for manifest in list_manifests() {
        let session_id = manifest.session.id.clone();

        // Filed = the session is done and no host should still be running for
        // it: an `exited` manifest, or an `archived.json` marker beside it
        // (session_ops::ARCHIVE_MARKER; referenced by literal to avoid a
        // session_ops -> session_host -> session_ops dependency cycle).
        let archived = session_dir(&session_id).join("archived.json").exists();
        let reason = match manifest.state {
            HostedSessionState::Exited => ReapReason::Exited,
            HostedSessionState::Running if archived => ReapReason::Archived,
            _ => continue,
        };

        // Only modern per-process hosts recorded a host pid. Without one there
        // is nothing safe to signal (the child shell pid is not the host).
        let Some(host_pid) = manifest.host_pid else {
            continue;
        };
        if host_pid <= 1 {
            continue;
        }
        // Never signal the shared PTY core: it hosts many Sessions.
        if Some(host_pid) == core_pid {
            continue;
        }
        // Provably the same process we recorded, or leave it alone.
        if recorded_pid_identity(host_pid, manifest.host_pid_started_at) != PidIdentity::Matches {
            continue;
        }
        // Defense in depth against a pid recycled within tolerance onto the
        // core or something unrelated: it must actually be a session host and
        // must not be the core.
        if process_argv_mentions(host_pid, SESSION_HOST_ARG) != Some(true) {
            continue;
        }
        if process_argv_mentions(host_pid, crate::pty_core::PTY_CORE_ARG) == Some(true) {
            continue;
        }

        // Signal both the process group (production hosts `setsid`, so
        // `-host_pid` is their own group — never the worker's) and the pid
        // itself (a host spawned without that pre_exec is not a group leader;
        // a positive kill of a pid we just proved is ours is still safe). The
        // group send is a harmless ESRCH when there is no such group. Mirrors
        // the HostGuard reap in the process tests.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(host_pid as i32), libc::SIGTERM);
            libc::kill(host_pid as i32, libc::SIGTERM);
        }
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            if !process_exists(host_pid) {
                break;
            }
        }
        #[cfg(unix)]
        if process_exists(host_pid) {
            unsafe {
                libc::kill(-(host_pid as i32), libc::SIGKILL);
                libc::kill(host_pid as i32, libc::SIGKILL);
            }
        }

        mark_manifest_exited(&session_id);
        reaped.push(ReapedHost {
            session_id,
            host_pid,
            reason,
        });
    }

    reaped
}

pub fn write_launch_file(launch: &SessionHostLaunch) -> Result<PathBuf, String> {
    ensure_session_dir(&launch.session.id)?;
    let path = launch_path(&launch.session.id);
    write_json_file(&path, launch)?;
    Ok(path)
}

pub fn wait_until_ready(session_id: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(manifest) = load_manifest(session_id) {
            if manifest.state == HostedSessionState::Running {
                #[cfg(unix)]
                if socket_path(session_id).exists() {
                    return Ok(());
                }
                #[cfg(not(unix))]
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err("Timed out waiting for session host".into())
}

pub fn read_output_chunk(
    session_id: &str,
    offset: Option<u64>,
    max_bytes: Option<usize>,
    tail_bytes: Option<usize>,
) -> Result<SessionOutputChunk, String> {
    read_output_chunk_with_retries(session_id, offset, max_bytes, tail_bytes, 3)
}

fn read_output_chunk_with_retries(
    session_id: &str,
    offset: Option<u64>,
    max_bytes: Option<usize>,
    tail_bytes: Option<usize>,
    retries: u8,
) -> Result<SessionOutputChunk, String> {
    let path = output_path(session_id);
    let exists = path.exists();

    if !exists {
        let manifest = refresh_manifest_health_cached(session_id, MANIFEST_HEALTH_CACHE_TTL_MS);
        let exited = manifest
            .as_ref()
            .map(|value| value.state == HostedSessionState::Exited)
            .unwrap_or(false);
        return Ok(SessionOutputChunk {
            data: Vec::new(),
            next_offset: offset.unwrap_or(0),
            exited,
            exists: manifest.is_some(),
        });
    }

    let mut file = File::open(&path).map_err(|e| format!("Failed to open output log: {e}"))?;
    let file_len = file
        .metadata()
        .map_err(|e| format!("Failed to stat output log: {e}"))?
        .len();
    let retained_from = output_retained_from(session_id).min(file_len);

    let read_limit = max_bytes.unwrap_or(DEFAULT_OUTPUT_READ_BYTES) as u64;
    let mut effective_read_limit = read_limit;
    let explicit_offset_is_retained = offset
        .map(|requested| (retained_from..=file_len).contains(&requested))
        .unwrap_or(false);
    let replay_tail = offset.is_none() || !explicit_offset_is_retained;
    let mut start = if explicit_offset_is_retained {
        offset.unwrap_or(retained_from)
    } else {
        retained_from
    };
    if replay_tail {
        let tail = tail_bytes.unwrap_or(DEFAULT_OUTPUT_TAIL_BYTES) as u64;
        if file_len > tail {
            start = (file_len - tail).max(retained_from);
        }
        let desired_start = start;
        start = align_replay_tail_start(&mut file, desired_start, retained_from)?;
        effective_read_limit = read_limit.saturating_add(desired_start.saturating_sub(start));
    }
    if start > file_len {
        start = file_len;
    }

    use std::io::Seek;
    use std::io::SeekFrom;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Failed to seek output log: {e}"))?;

    let to_read = std::cmp::min(effective_read_limit, file_len.saturating_sub(start)) as usize;
    let mut data = vec![0u8; to_read];
    if to_read > 0 {
        file.read_exact(&mut data)
            .map_err(|e| format!("Failed to read output log: {e}"))?;
    }

    let manifest = refresh_manifest_health_cached(session_id, MANIFEST_HEALTH_CACHE_TTL_MS);
    let exited = manifest
        .as_ref()
        .map(|value| value.state == HostedSessionState::Exited)
        .unwrap_or(false);

    // The retention writer publishes its new floor before punching. If it
    // advanced while this lock-free read was in flight, discard the sample:
    // some bytes may have raced deallocation and read back as sparse NULs.
    let newest_file_len = file
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(file_len);
    let newest_retained_from = output_retained_from(session_id).min(newest_file_len);
    if start < newest_retained_from {
        if retries > 0 {
            return read_output_chunk_with_retries(
                session_id,
                offset,
                max_bytes,
                tail_bytes,
                retries - 1,
            );
        }
        data.clear();
        return Ok(SessionOutputChunk {
            data,
            next_offset: newest_retained_from,
            exited,
            exists: true,
        });
    }

    Ok(SessionOutputChunk {
        next_offset: start + data.len() as u64,
        data,
        exited,
        exists: true,
    })
}

pub fn spawn_host_process(launch: &SessionHostLaunch) -> Result<(), String> {
    let launch_file = write_launch_file(launch)?;
    spawn_host_process_from_launch_file(&launch_file)
}

pub fn spawn_host_process_from_launch_file(launch_file: impl AsRef<Path>) -> Result<(), String> {
    let launch_file = launch_file.as_ref();
    // Prefer the shared PTY core when one is running for this home. Any
    // failure (no core, refused, timed out, error reply) falls back to the
    // per-process Host below so a wedged core never blocks a launch.
    match crate::pty_core::try_launch_via_core(launch_file) {
        Ok(_session_id) => return Ok(()),
        Err(crate::pty_core::CoreLaunchError::Unavailable) => {}
        Err(error) => {
            crate::hook_assets::append_trace_log_line(&format!(
                "pty-core launch fallback for {}: {error}",
                launch_file.display()
            ));
        }
    }
    let exe = resolve_current_executable()?;
    let mut command = std::process::Command::new(exe);
    command.arg(SESSION_HOST_ARG).arg(launch_file);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    // A detached session host is not an occupant of the outer Herdr pane.
    // Keeping that pane identity would let provider integrations race the
    // Unpeel TUI's aggregate status authority.
    strip_leaked_herdr_process_env(&mut command);

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
        .map_err(|e| format!("Failed to spawn session host: {e}"))?;
    Ok(())
}

pub(crate) fn resolve_current_executable() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to locate current executable: {e}"))?;
    if exe.is_absolute() && exe.exists() {
        return Ok(exe);
    }

    let current_dir =
        std::env::current_dir().map_err(|e| format!("Failed to read current dir: {e}"))?;
    let candidates = [
        current_dir.join(&exe),
        current_dir.join("crates").join(&exe),
        current_dir
            .join("crates")
            .join("target")
            .join("release")
            .join(&exe),
        current_dir
            .join("crates")
            .join("target")
            .join("debug")
            .join(&exe),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Ok(exe)
}

#[cfg(unix)]
fn send_command_for_response(
    session_id: &str,
    command: &SessionHostCommand,
    timeout: Option<Duration>,
) -> Result<SessionHostResponse, String> {
    use std::os::unix::net::UnixStream;

    let socket = socket_path(session_id);
    let mut stream = UnixStream::connect(&socket)
        .map_err(|e| format!("Failed to connect to session host: {e}"))?;
    if let Some(timeout) = timeout {
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| format!("Failed to set host read timeout: {e}"))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| format!("Failed to set host write timeout: {e}"))?;
    }
    let body =
        serde_json::to_string(command).map_err(|e| format!("Failed to serialize command: {e}"))?;
    stream
        .write_all(body.as_bytes())
        .map_err(|e| format!("Failed to write command: {e}"))?;
    stream
        .write_all(b"\n")
        .map_err(|e| format!("Failed to finalize command: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| format!("Failed to read host response: {e}"))?;
    if response.trim().is_empty() {
        return Err("Session host closed the control connection before replying.".into());
    }
    let reply: SessionHostResponse =
        serde_json::from_str(response.trim()).map_err(|e| format!("Invalid host response: {e}"))?;
    Ok(reply)
}

#[cfg(unix)]
fn send_command_with_optional_timeout(
    session_id: &str,
    command: &SessionHostCommand,
    timeout: Option<Duration>,
) -> Result<(), String> {
    let reply = send_command_for_response(session_id, command, timeout)?;
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| "Session host rejected command".into()));
    }
    Ok(())
}

#[cfg(unix)]
pub fn send_command(session_id: &str, command: &SessionHostCommand) -> Result<(), String> {
    send_command_with_optional_timeout(session_id, command, None)
}

/// One-shot control command with a bounded socket read/write wait. Remote
/// request adapters must use this instead of allowing an unresponsive session
/// Host to pin an HTTP/Relay/FFI worker indefinitely.
#[cfg(unix)]
pub fn send_command_with_timeout(
    session_id: &str,
    command: &SessionHostCommand,
    timeout: Duration,
) -> Result<(), String> {
    send_command_with_optional_timeout(session_id, command, Some(timeout))
}

#[cfg(unix)]
pub fn request_terminal_viewport_snapshot(
    session_id: &str,
    cols: u16,
    rows: u16,
    scroll_offset_rows: u32,
    viewport_rows: Option<u16>,
) -> Result<TerminalViewportSnapshot, String> {
    let reply = send_command_for_response(
        session_id,
        &SessionHostCommand::ViewportSnapshot {
            cols,
            rows,
            scroll_offset_rows,
            viewport_rows,
        },
        Some(Duration::from_millis(SESSION_PING_TIMEOUT_MS)),
    )?;
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| "Session host rejected viewport snapshot request".into()));
    }
    reply
        .viewport
        .ok_or_else(|| "Session host returned no viewport snapshot".into())
}

/// Snapshot the viewport at whatever size the host currently tracks (cols=0 /
/// rows=0 on the wire means "keep current size"). Used by the MCP host, which
/// has no terminal size of its own and must not disturb the real viewport.
#[cfg(unix)]
pub fn request_current_viewport_snapshot(
    session_id: &str,
    scroll_offset_rows: u32,
    viewport_rows: Option<u16>,
) -> Result<TerminalViewportSnapshot, String> {
    let reply = send_command_for_response(
        session_id,
        &SessionHostCommand::ViewportSnapshot {
            cols: 0,
            rows: 0,
            scroll_offset_rows,
            viewport_rows,
        },
        Some(Duration::from_millis(SESSION_SNAPSHOT_TIMEOUT_MS)),
    )?;
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| "Session host rejected viewport snapshot request".into()));
    }
    reply
        .viewport
        .ok_or_else(|| "Session host returned no viewport snapshot".into())
}

/// Ask the Host for a VT state snapshot of its resident terminal. Returns
/// the journal offset the snapshot was rendered at together with the
/// snapshot. An `Err` also covers Hosts that predate the command (they close
/// the connection without a reply line).
#[cfg(unix)]
pub fn request_terminal_snapshot_vt(
    session_id: &str,
) -> Result<(u64, crate::terminal_viewport::SnapshotVt), String> {
    use std::os::unix::net::UnixStream;

    let socket = socket_path(session_id);
    let mut stream = UnixStream::connect(&socket)
        .map_err(|e| format!("Failed to connect to session host: {e}"))?;
    let timeout = Some(Duration::from_millis(SESSION_SNAPSHOT_TIMEOUT_MS));
    stream
        .set_read_timeout(timeout)
        .map_err(|e| format!("Failed to set host read timeout: {e}"))?;
    stream
        .set_write_timeout(timeout)
        .map_err(|e| format!("Failed to set host write timeout: {e}"))?;
    let body = serde_json::to_string(&SessionHostCommand::Snapshot)
        .map_err(|e| format!("Failed to serialize command: {e}"))?;
    stream
        .write_all(body.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to write command: {e}"))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read host response: {e}"))?;
    if line.trim().is_empty() {
        return Err("Session host does not support snapshot attach.".into());
    }
    let reply: SnapshotVtReply =
        serde_json::from_str(line.trim()).map_err(|e| format!("Invalid host response: {e}"))?;
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| "Session host rejected snapshot request".into()));
    }
    let header = reply
        .snapshot
        .ok_or_else(|| "Session host returned no snapshot".to_string())?;
    let len = usize::try_from(header.bytes_len).map_err(|_| "Snapshot too large".to_string())?;
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| format!("Failed to read snapshot bytes: {e}"))?;
    Ok((
        header.journal_offset,
        crate::terminal_viewport::SnapshotVt {
            cols: header.cols,
            rows: header.rows,
            bytes,
        },
    ))
}

#[cfg(not(unix))]
pub fn request_terminal_snapshot_vt(
    _session_id: &str,
) -> Result<(u64, crate::terminal_viewport::SnapshotVt), String> {
    Err("Persistent session host is currently only supported on Unix.".into())
}

#[cfg(unix)]
pub fn connect_output_stream(
    session_id: &str,
    offset: u64,
    timeout: Option<Duration>,
) -> Result<std::os::unix::net::UnixStream, String> {
    use std::os::unix::net::UnixStream;

    let socket = socket_path(session_id);
    let mut stream = UnixStream::connect(&socket)
        .map_err(|e| format!("Failed to connect to session host: {e}"))?;
    let timeout =
        timeout.unwrap_or_else(|| Duration::from_millis(SESSION_OUTPUT_STREAM_READ_TIMEOUT_MS));
    {
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| format!("Failed to set host read timeout: {e}"))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| format!("Failed to set host write timeout: {e}"))?;
    }
    // Internal streamers (remote server, MCP reads) are passive viewers —
    // they never answer terminal queries, so the host keeps probe-answering.
    let body = serde_json::to_string(&SessionHostCommand::StreamOutput {
        offset,
        answers_queries: false,
    })
    .map_err(|e| format!("Failed to serialize stream command: {e}"))?;
    stream
        .write_all(body.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|e| format!("Failed to start output stream: {e}"))?;
    Ok(stream)
}

#[cfg(not(unix))]
pub fn send_command(_session_id: &str, _command: &SessionHostCommand) -> Result<(), String> {
    Err("Persistent session host is currently only supported on Unix.".into())
}

#[cfg(not(unix))]
pub fn send_command_with_timeout(
    _session_id: &str,
    _command: &SessionHostCommand,
    _timeout: Duration,
) -> Result<(), String> {
    Err("Persistent session host is currently only supported on Unix.".into())
}

#[cfg(not(unix))]
pub fn request_terminal_viewport_snapshot(
    _session_id: &str,
    _cols: u16,
    _rows: u16,
    _scroll_offset_rows: u32,
    _viewport_rows: Option<u16>,
) -> Result<TerminalViewportSnapshot, String> {
    Err("Persistent session host is currently only supported on Unix.".into())
}

#[cfg(not(unix))]
pub fn request_current_viewport_snapshot(
    _session_id: &str,
    _scroll_offset_rows: u32,
    _viewport_rows: Option<u16>,
) -> Result<TerminalViewportSnapshot, String> {
    Err("Persistent session host is currently only supported on Unix.".into())
}

#[cfg(not(unix))]
pub fn connect_output_stream(
    _session_id: &str,
    _offset: u64,
    _timeout: Option<Duration>,
) -> Result<(), String> {
    Err("Persistent session host is currently only supported on Unix.".into())
}

#[cfg(unix)]
fn ping_session_host(session_id: &str, timeout: Duration) -> bool {
    send_command_with_optional_timeout(session_id, &SessionHostCommand::Ping, Some(timeout)).is_ok()
}

#[cfg(not(unix))]
fn ping_session_host(_session_id: &str, _timeout: Duration) -> bool {
    false
}

pub fn run_from_args(args: &[String]) -> Result<(), String> {
    let launch_path = args
        .first()
        .ok_or("Missing launch file path for session host".to_string())?;
    let raw = fs::read(launch_path).map_err(|e| format!("Failed to read launch file: {e}"))?;
    let launch: SessionHostLaunch =
        serde_json::from_slice(&raw).map_err(|e| format!("Invalid launch file: {e}"))?;
    let _ = fs::remove_file(launch_path);

    run_host(launch)
}

/// Create a runtime-owned storage directory without allowing an adapter path
/// to escape the Host's private state root or traverse a symlink. Runtime
/// adapters are compiled code, but this remains a destructive-cleanup trust
/// boundary because the accepted path is persisted for later removal.
fn ensure_managed_storage_path(unpeel_home: &Path, requested: &Path) -> Result<(), String> {
    let relative = requested.strip_prefix(unpeel_home).map_err(|_| {
        format!(
            "Runtime managed storage must live beneath {}",
            unpeel_home.display()
        )
    })?;
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("Runtime managed storage has an unsafe relative path".into());
    }

    let mut current = unpeel_home.to_path_buf();
    for component in components {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Runtime managed storage crosses a symlink: {}",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Runtime managed storage component is not a directory: {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| {
                    format!(
                        "Failed to create runtime managed storage {}: {error}",
                        current.display()
                    )
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).map_err(
                        |error| {
                            format!(
                                "Failed to protect runtime managed storage {}: {error}",
                                current.display()
                            )
                        },
                    )?;
                }
            }
            Err(error) => {
                return Err(format!(
                    "Failed to inspect runtime managed storage {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

/// One periodic Host job multiplexed onto the per-session timer thread.
/// `run` returns `false` to retire the job (its thread used to `return`).
pub(crate) struct HostTimerJob {
    interval: Duration,
    next_at: Instant,
    run: Box<dyn FnMut() -> bool + Send>,
}

impl HostTimerJob {
    fn new(
        initial_delay: Duration,
        interval: Duration,
        run: impl FnMut() -> bool + Send + 'static,
    ) -> Self {
        Self {
            interval,
            next_at: Instant::now() + initial_delay,
            run: Box::new(run),
        }
    }
}

/// Timer-thread tick granularity while idle. Shutdown joins this thread
/// before the final exit-manifest write, so it must never sleep a full
/// heartbeat interval: a single 60 s sleep once held that write hostage,
/// leaving the client showing a dead terminal instead of the exited state.
const HOST_TIMER_MAX_SLEEP: Duration = Duration::from_millis(250);

/// Inputs for the periodic Host jobs of one Session. Fresh launches and
/// core-to-core takeovers build their jobs from the same struct so a moved
/// Session keeps exactly the released heartbeat, menu scan, and runtime
/// observer behaviour.
pub(crate) struct SessionJobInputs {
    pub session_id: String,
    pub command: String,
    pub shell: String,
    pub pid: Option<u32>,
    pub pid_started_at: Option<u64>,
    pub runtime: Arc<Mutex<HostRuntime>>,
    pub runtime_generation: Arc<AtomicU64>,
    pub pending_runtime_generation: Arc<AtomicU64>,
    pub viewport: Arc<Mutex<TerminalViewportState>>,
}

/// The periodic Host jobs (heartbeat, runtime observer, menu/screen scan)
/// share ONE timer thread (`HostTimerJob`) instead of a thread each: at 50+
/// sessions the idle stacks alone were measurable. Each job keeps its own
/// cadence and state; they simply run back to back on the same thread, so
/// no job may ever block indefinitely (none holds a lock across a sleep).
#[cfg(unix)]
pub(crate) fn build_session_timer_jobs(inputs: SessionJobInputs) -> Vec<HostTimerJob> {
    let SessionJobInputs {
        session_id,
        command,
        shell,
        pid,
        pid_started_at,
        runtime,
        runtime_generation,
        pending_runtime_generation,
        viewport,
    } = inputs;
    let heartbeat_session_id = session_id.clone();
    let heartbeat_job = HostTimerJob::new(
        Duration::from_millis(SESSION_HEARTBEAT_INTERVAL_MS),
        Duration::from_millis(SESSION_HEARTBEAT_INTERVAL_MS),
        move || {
            let _ = update_manifest_session(&heartbeat_session_id, |manifest| {
                manifest.heartbeat_at = current_timestamp_ms();
            });
            true
        },
    );

    // Live runtime observation is PTY-owned and display-only. In
    // particular, a blank shell that later runs `claude` gains Claude's
    // sidebar identity without changing the saved blank launch command or
    // acquiring resume/fork capabilities. Ownership is checked inside the
    // observer against this exact session leader PID + kernel start time.
    let runtime_observer_session_id = session_id.clone();
    let runtime_observer_has_owned_shell = command.trim().is_empty();
    let runtime_observer_shell = PathBuf::from(&shell);
    let runtime_observer_expected_id =
        integrations::runtime_for_command(&command).map(|runtime| runtime.legacy_slug.clone());
    let runtime_observer_completion_path =
        runtime_launch_completion_path(&runtime_observer_session_id);
    let runtime_for_observer = Arc::clone(&runtime);
    let generation_for_runtime_observer = Arc::clone(&runtime_generation);
    let pending_generation_for_runtime_observer = Arc::clone(&pending_runtime_generation);
    let runtime_observer_job = (|| {
        let (Some(session_leader_pid), Some(session_leader_started_at_ms)) = (pid, pid_started_at)
        else {
            return None;
        };
        let mut tracker = RuntimeObservationTracker::default();
        let mut observed_generation = generation_for_runtime_observer.load(Ordering::Acquire);
        let mut previous_foreground_was_shell = false;
        // Self-healing hook installs for hand-started runtimes. Managed
        // launches install their provider's hook assets at spawn; a
        // hook-capable CLI the user types into this PTY may never have
        // been launched through a preset anywhere, leaving it on output
        // heuristics forever. Seeded with the launch runtime (already
        // installed at spawn) so the common managed case never re-runs.
        let mut installed_support_runtime_id = runtime_observer_expected_id.clone();
        // First scan runs immediately, then every scan interval after the
        // previous scan finished (the old thread slept after each pass).
        Some(HostTimerJob::new(
            Duration::ZERO,
            Duration::from_millis(SESSION_RUNTIME_SCAN_INTERVAL_MS),
            move || {
                let generation = generation_for_runtime_observer.load(Ordering::Acquire);
                if generation != observed_generation {
                    observed_generation = generation;
                    tracker = RuntimeObservationTracker::default();
                    runtime_for_observer
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .last_runtime_observation = None;
                }
                let (current_child_pid, foreground_process_group_id) = {
                    let runtime = runtime_for_observer.lock().unwrap();
                    (
                        runtime.child.process_id(),
                        runtime.master.process_group_leader(),
                    )
                };
                // The Child is retained until shutdown, but still fail closed
                // if the PTY runtime no longer names the child we captured.
                if current_child_pid != Some(session_leader_pid) {
                    return false;
                }
                let mut observation = foreground_process_group_id.and_then(|foreground_pgid| {
                    crate::runtime_observer::observe_foreground_runtime(
                        session_leader_pid,
                        session_leader_started_at_ms,
                        foreground_pgid,
                    )
                });
                let foreground_is_session_leader = foreground_process_group_id
                    .and_then(|pgid| u32::try_from(pgid).ok())
                    == Some(session_leader_pid);
                let mut returned_to_owned_shell =
                    runtime_observer_has_owned_shell && foreground_is_session_leader;
                let pending_generation =
                    pending_generation_for_runtime_observer.load(Ordering::Acquire);
                let tracker_retains_runtime = tracker.current.is_some();
                let foreground_just_returned_to_shell =
                    foreground_is_session_leader && !previous_foreground_was_shell;
                previous_foreground_was_shell = foreground_is_session_leader;

                // A stopped/background managed runtime is absent from the
                // foreground observer even though it is still alive in this
                // PTY's kernel session. Retain (or recover) that observation
                // until a full-session scan proves the expected runtime gone
                // and the real interactive login shell has returned.
                // Scanning every OS PID is intentionally gated: once an idle
                // shell is settled with no retained/pending runtime, repeated
                // 300ms scans across many Sessions would be pure overhead.
                if tracker_retains_runtime
                    || pending_generation != 0
                    || foreground_just_returned_to_shell
                {
                    if let Some(expected_runtime_id) = runtime_observer_expected_id.as_deref() {
                        let prior_observation = tracker.current.clone();
                        if let Some(inspection) =
                            crate::runtime_observer::inspect_owned_session_processes(
                                session_leader_pid,
                                session_leader_started_at_ms,
                                &runtime_observer_shell,
                                expected_runtime_id,
                                prior_observation.as_ref(),
                            )
                        {
                            let prior_still_blocks = prior_observation.is_some()
                                && inspection.prior_runtime_present != Some(false);
                            if prior_still_blocks {
                                // Catalog recognition is deliberately weaker
                                // than exact PID/start/PGID retention. Keep the
                                // old evidence if that process renamed/execed.
                                observation = prior_observation;
                            } else if observation.is_none() {
                                observation = inspection.recognized_runtime_observation.clone();
                            }
                            returned_to_owned_shell = foreground_is_session_leader
                                && inspection.shell_executable_matches
                                && inspection.shell_invocation_matches
                                && inspection.recognized_runtime_observation.is_none()
                                && inspection.prior_runtime_present == Some(false);
                        } else {
                            returned_to_owned_shell = false;
                            // An incomplete process scan is not disappearance
                            // proof. Retain the last exact evidence so neither
                            // the manifest nor same-PTY action becomes eligible.
                            if prior_observation.is_some() {
                                observation = prior_observation;
                            }
                        }
                    }
                }

                let observed_expected_runtime = runtime_observer_expected_id
                    .as_deref()
                    .is_some_and(|expected| {
                        observation.as_ref().is_some_and(|observation| {
                            observation.runtime_id.eq_ignore_ascii_case(expected)
                        })
                    });
                let completion_observed =
                    runtime_observer_completion_path.is_dir() && returned_to_owned_shell;
                let clear_pending =
                    pending_generation != 0 && (observed_expected_runtime || completion_observed);
                let observed_runtime_for_manifest = observation.clone();
                let next = tracker.observe(observation, returned_to_owned_shell);
                runtime_for_observer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .last_runtime_observation = tracker.current.clone();
                if next.is_some() || clear_pending {
                    let updated =
                        update_manifest_session(&runtime_observer_session_id, |manifest| {
                            if manifest.runtime_launch_pending
                                && manifest.runtime_launch_generation == pending_generation
                                && clear_pending
                            {
                                manifest.runtime_launch_pending = false;
                            }
                            if let Some(next) = next.as_ref() {
                                // App identity follows the same evidence: an
                                // observed App id resolves to its installed
                                // manifest, a built-in runtime or a proven
                                // return to the shell clears it. No change in
                                // observation leaves the (possibly
                                // spawn-stamped) identity alone.
                                manifest.active_app = next.as_ref().and_then(|observation| {
                                    crate::app_runtime::app_for_runtime_id(&observation.runtime_id)
                                        .map(ObservedAppIdentity::from)
                                });
                                manifest.runtime =
                                    next.clone().map(|observation| HostedSessionRuntime {
                                        current_observation: Some(observation),
                                    });
                            } else if clear_pending && observed_expected_runtime {
                                // If the first manifest write for this
                                // observation raced/failed, the tracker already
                                // considers it current. Persist it alongside the
                                // latch clear instead of losing the evidence.
                                manifest.runtime =
                                    observed_runtime_for_manifest.clone().map(|observation| {
                                        HostedSessionRuntime {
                                            current_observation: Some(observation),
                                        }
                                    });
                            }
                        });
                    if updated
                        .as_ref()
                        .ok()
                        .and_then(Option::as_ref)
                        .is_some_and(|manifest| {
                            manifest.runtime_launch_generation == pending_generation
                                && !manifest.runtime_launch_pending
                        })
                    {
                        let _ = pending_generation_for_runtime_observer.compare_exchange(
                            pending_generation,
                            0,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                        if completion_observed {
                            remove_runtime_launch_completion_marker(
                                &runtime_observer_completion_path,
                            );
                        }
                    }
                }
                // Install/refresh the observed runtime's hook assets on the
                // observation edge. The running process keeps its heuristic
                // (providers read hook config at startup); the NEXT invocation
                // reports through hooks. Installers are idempotent, locked,
                // and rewrite only on content change — the same guarantees
                // the managed spawn path relies on. Recorded even on failure
                // so a broken environment is not retried at scan cadence.
                // `UNPEEL_TEST` guards the operator's real provider configs:
                // PTY test cases observe fake catalog-named binaries under an
                // isolated UNPEEL_HOME, but provider settings paths (for
                // example `~/.claude/settings.json`) are genuinely global.
                if let Some(Some(observation)) = next.as_ref() {
                    let runtime_id = observation.runtime_id.as_str();
                    if installed_support_runtime_id.as_deref() != Some(runtime_id)
                        && integrations::has_runtime_support_installer(runtime_id)
                        && std::env::var("UNPEEL_TEST").as_deref() != Ok("1")
                    {
                        if let Err(error) = integrations::install_runtime_support(runtime_id) {
                            log::warn!(
                            "Failed to install runtime support for observed {runtime_id}: {error}"
                        );
                        }
                        installed_support_runtime_id = Some(runtime_id.to_string());
                    }
                }
                true
            },
        ))
    })();

    // Menu-prompt scan: agent-drawn select menus (Claude/Codex numbered
    // prompts) fire no lifecycle hook, so nothing else can tell the session
    // is waiting for a choice. Poll the live viewport for the menu footer
    // and edge-write `menu_prompt_active` into the manifest so native shows
    // the attention badge. Edge-triggered: steady state does zero writes.
    //
    // The same screen text feeds local-URL detection: printed loopback
    // URLs become lifetime candidates, and every URL_PROBE_TICKS ticks
    // (or immediately when a new one appears) the tracker probes which
    // currently serve a browsable page, edge-writing `detected_local_urls`.
    // It also feeds `screen_changed_at`: a text-hash change stamps the
    // manifest (coalesced), so clients can tell real screen changes from
    // idle repaint loops that only churn output.bin.
    let menu_session_id = session_id.clone();
    let viewport_for_menu = Arc::clone(&viewport);
    let menu_job = {
        let mut last_active = false;
        let mut last_modes: Option<crate::terminal_viewport::TerminalModeState> = None;
        let mut url_tracker = crate::local_urls::LocalUrlTracker::default();
        let mut screen_tracker = ScreenChangeTracker::default();
        let mut ticks_since_probe: u32 = 0;
        // App-title marker watch: (mtime, len) of `app-title.json`, so
        // steady state costs one stat per tick and zero reads/writes.
        let app_title_path = session_dir(&menu_session_id).join(APP_TITLE_MARKER);
        let mut app_title_seen: Option<(std::time::SystemTime, u64)> = None;
        let mut app_title_settled = false;
        HostTimerJob::new(
            Duration::from_millis(SESSION_MENU_SCAN_INTERVAL_MS),
            Duration::from_millis(SESSION_MENU_SCAN_INTERVAL_MS),
            move || {
                if !app_title_settled {
                    let stamp = fs::metadata(&app_title_path)
                        .ok()
                        .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
                    if let Some(stamp) = stamp {
                        if app_title_seen != Some(stamp) {
                            app_title_seen = Some(stamp);
                            if let Some(title) =
                                read_app_title_marker(&session_dir(&menu_session_id))
                            {
                                app_title_settled = apply_app_title(&menu_session_id, &title);
                            }
                        }
                    }
                }
                let (screen, modes) = {
                    let mut viewport = viewport_for_menu.lock().unwrap();
                    (
                        viewport.current_screen_text(),
                        viewport.terminal_mode_state(),
                    )
                };
                if let Some(stamp) = screen_tracker.observe(&screen, current_timestamp_ms()) {
                    let _ = update_manifest_session(&menu_session_id, |manifest| {
                        manifest.screen_changed_at = Some(stamp);
                    });
                }
                // Edge-written like `menu_prompt_active`: steady state costs
                // zero manifest writes; mode flips are rare (workload
                // startup/exit, alt-screen apps opening a pager, …).
                if last_modes.as_ref() != Some(&modes) {
                    last_modes = Some(modes.clone());
                    let _ = update_manifest_session(&menu_session_id, |manifest| {
                        manifest.terminal_modes = (!modes.is_default()).then(|| modes.clone());
                    });
                }
                let active = viewport_has_menu_prompt(&screen);
                if active != last_active {
                    last_active = active;
                    let _ = update_manifest_session(&menu_session_id, |manifest| {
                        manifest.menu_prompt_active = active;
                    });
                }
                let saw_new_url = url_tracker.observe_screen(&screen);
                ticks_since_probe += 1;
                if url_tracker.has_candidates()
                    && (saw_new_url || ticks_since_probe >= URL_PROBE_TICKS)
                {
                    ticks_since_probe = 0;
                    if let Some(live) = url_tracker.probe() {
                        let _ = update_manifest_session(&menu_session_id, |manifest| {
                            manifest.detected_local_urls = live.clone();
                        });
                    }
                }
                true
            },
        )
    };
    let mut jobs: Vec<HostTimerJob> = vec![heartbeat_job, menu_job];
    jobs.extend(runtime_observer_job);
    jobs
}

/// Host one Session to completion on the calling thread (per-process
/// `__session_host__` mode): setup, hand-off to the shared reactor, then
/// block until the Session's teardown reports back.
pub(crate) fn run_host(launch: SessionHostLaunch) -> Result<(), String> {
    let (done_tx, done_rx) = mpsc::channel::<Result<(), String>>();
    start_host(
        launch,
        Box::new(move |result| {
            let _ = done_tx.send(result);
        }),
    )?;
    done_rx
        .recv()
        .unwrap_or_else(|_| Err("Session reactor dropped the session".into()))
}

/// Set a Session up and hand it to the shared reactor; returns as soon as
/// its fds are registered. `on_exit` fires from the teardown thread when
/// the Session ends (the shared PTY core uses this so a hosted Session
/// keeps no thread of its own). Setup errors are returned directly and
/// `on_exit` is then never called.
pub(crate) fn start_host(
    mut launch: SessionHostLaunch,
    on_exit: Box<dyn FnOnce(Result<(), String>) + Send>,
) -> Result<(), String> {
    if launch.execution_scope != SessionExecutionScope::Local {
        return Err(
            "Refusing local session execution while remote Controller scope is selected".into(),
        );
    }

    #[cfg(not(unix))]
    {
        let _ = (launch, on_exit);
        return Err("Persistent session host is currently only supported on Unix.".into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::net::UnixListener;

        // Make the state dir private (0700) before writing any session
        // artifacts, so output logs / manifests / launch files aren't readable
        // by other local users on a multi-user machine.
        let unpeel_home = crate::app_paths::ensure_unpeel_home()
            .map_err(|error| format!("Failed to initialize Unpeel home: {error}"))?;
        // Final ownership authority: every frontend eventually crosses this
        // boundary before a manifest becomes visible. Authenticated remote
        // adapters may already have stamped a future account principal; local
        // and legacy launch files bind to this Host's owner here.
        if launch
            .session
            .owner_principal_id
            .as_deref()
            .is_none_or(|value| !crate::state::valid_session_attribution_id(value))
        {
            let host_id = crate::relay_uplink::ensure_host_id()
                .map_err(|error| format!("Failed to resolve Session owner: {error}"))?;
            launch.session.owner_principal_id =
                Some(crate::state::host_owner_principal_id(&host_id));
        }
        if launch
            .session
            .created_by_device_id
            .as_deref()
            .is_some_and(|value| !crate::state::valid_session_attribution_id(value))
        {
            launch.session.created_by_device_id = None;
        }
        if launch
            .session
            .source_preset_id
            .as_deref()
            .is_some_and(|value| !crate::state::valid_session_attribution_id(value))
        {
            launch.session.source_preset_id = None;
        }
        ensure_session_dir(&launch.session.id)?;
        // THE initial-launch preparation boundary. Every frontend submits its
        // original command; the Host invokes the selected runtime adapter once
        // before any manifest is visible or provider process can start.
        let prepared = crate::resume::prepare_new_launch(
            &launch.session.command,
            &launch.session.id,
            &unpeel_home,
        );
        launch.session.command = prepared.command;
        let managed_storage_path = prepared
            .managed_storage_path
            .map(PathBuf::from)
            .or_else(|| crate::resume::managed_storage_path(&launch.session.command, &unpeel_home));
        if let Some(path) = managed_storage_path.as_deref() {
            ensure_managed_storage_path(&unpeel_home, path)?;
        }
        if let Some(provider_session_id) = prepared.provider_session_id.as_deref() {
            crate::session_ops::set_provider_session(
                &launch.session.id,
                Some(provider_session_id),
                None,
            )?;
        }
        let resume_failure_markers =
            crate::resume::resume_failure_markers(&launch.session.command).unwrap_or_default();
        let output_path = output_path(&launch.session.id);
        let retention_path = output_retention_path(&launch.session.id);
        let prior_journal_exists = output_path.exists() || retention_path.exists();
        let prior_high_water = fs::metadata(&output_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            .max(read_output_retention_path(&retention_path));
        let journal_start_offset = if prior_journal_exists {
            prior_high_water.saturating_add(1)
        } else {
            0
        };
        // Establish the new logical journal generation before publishing the
        // preliminary live manifest. Attach/mobile readers are lock-free; if
        // the manifest appeared first they could replay the preceding Host's
        // terminal or the sparse replacement gap during startup.
        let output_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&output_path)
            .map_err(|e| format!("Failed to open output log {}: {e}", output_path.display()))?;
        let retained_output = RetainedOutputWriter::new(
            output_file,
            output_path.clone(),
            journal_start_offset,
            SESSION_OUTPUT_JOURNAL_RETAIN_BYTES,
            SESSION_OUTPUT_JOURNAL_ADVANCE_BYTES,
        )?;
        let session_socket_path = socket_path(&launch.session.id);
        let _ = fs::remove_file(&session_socket_path);
        let _ = fs::remove_file(attach_ready_path(&launch.session.id));
        let host_build_id = current_host_build_id();
        let launches_stable_runtime = !launch.session.command.trim().is_empty();
        let launches_resume_agent_runtime =
            crate::resume::can_resume_agent(&launch.session.command, None);
        let initial_runtime_launched_at = launches_stable_runtime.then(current_timestamp_ms);
        let initial_runtime_completion_path = runtime_launch_completion_path(&launch.session.id);
        prepare_runtime_launch_completion_marker(&initial_runtime_completion_path)?;

        // Write a preliminary manifest immediately, before provider-specific
        // setup (e.g. codex resolving its real binary + installing its wrapper)
        // and the PTY spawn run. A client (unpeel-attach) spawned in parallel
        // only waits a couple seconds for the manifest to appear; slow
        // providers like codex could miss that window, leaving the surface on
        // the bare login shell ("No session manifest", plus the uncleared
        // "Last login"/"You have mail" banner because attach errors out before
        // its screen-wipe). A per-process Host uses its own pid as the
        // liveness placeholder until the child pid is known; the post-spawn
        // manifest write below corrects it. Inside the shared PTY core that
        // placeholder would be the core's pid, and any kill path trusting it
        // would group-kill EVERY hosted Session — so the core publishes
        // `pid: None` and proves liveness through `host_pid` instead.
        let host_pid = std::process::id();
        let host_pid_started_at = process_start_time_ms(host_pid);
        let placeholder_pid = (!crate::pty_core::is_core_process()).then_some(host_pid);
        save_manifest(&HostedSessionManifest {
            session: launch.session.clone(),
            cwd: launch.cwd.clone(),
            state: HostedSessionState::Running,
            pid: placeholder_pid,
            pid_started_at: placeholder_pid.and_then(process_start_time_ms),
            host_pid: Some(host_pid),
            host_pid_started_at,
            exit_code: None,
            host_build_id: host_build_id.clone(),
            host_protocol_version: Some(SESSION_HOST_PROTOCOL_VERSION),
            has_been_written_to: false,
            provider_session_id: prepared.provider_session_id.clone(),
            provider_transcript_path: None,
            managed_storage_path: managed_storage_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            resume_failure_markers: resume_failure_markers.clone(),
            runtime: None,
            // App-launched sessions carry their identity from the first
            // manifest write so the sidebar never flashes a generic row
            // before the observer's first tick.
            active_app: crate::app_runtime::app_for_launch_command(&launch.session.command)
                .map(ObservedAppIdentity::from),
            runtime_launch_generation: u64::from(launches_stable_runtime),
            runtime_launch_pending: launches_resume_agent_runtime,
            runtime_launched_at: initial_runtime_launched_at,
            runtime_launch_output_offset: journal_start_offset,
            mcp_enabled: Some(launch.mcp_enabled),
            browser_mcp_enabled: Some(launch.browser_mcp_enabled),
            computer_mcp_enabled: Some(launch.computer_mcp_enabled),
            // Provider setup has not completed yet. Never publish launch
            // grants as registration evidence in this preliminary record.
            mcp_client_registered: false,
            browser_client_registered: false,
            computer_client_registered: false,
            menu_prompt_active: false,
            terminal_modes: None,
            screen_changed_at: None,
            detected_local_urls: Vec::new(),
            heartbeat_at: current_timestamp_ms(),
            updated_at: current_timestamp_ms(),
        })?;

        let initial_rows = launch.initial_rows.filter(|rows| *rows >= 2).unwrap_or(24);
        let initial_cols = launch.initial_cols.filter(|cols| *cols >= 2).unwrap_or(80);
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: initial_rows,
                cols: initial_cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {e}"))?;

        let shell = crate::setup::resolved_user_shell();
        let command_head = integrations::command_head(&launch.session.command).to_ascii_lowercase();
        // Runtime package aliases are launch detection inputs, while every
        // integration callback and foreground observation uses the stable
        // compatibility identity. Unknown commands keep their generic head and
        // all optional runtime callbacks below fail open.
        let runtime_id = integrations::runtime_for_command(&launch.session.command)
            .map(|runtime| runtime.legacy_slug.as_str())
            .unwrap_or(command_head.as_str());
        // Install/refresh provider hook assets (hook scripts, codex wrapper,
        // claude-mcp.json) host-side so every frontend gets them — the native
        // app spawns this host directly without going through pty_manager.
        let mut runtime_support_ready = true;
        if integrations::has_runtime_support_installer(runtime_id) {
            if let Err(error) = integrations::install_runtime_support(runtime_id) {
                log::warn!("Failed to install runtime support for {runtime_id}: {error}");
                runtime_support_ready = false;
            }
        }
        if let Err(error) = integrations::prepare_runtime_launch(
            runtime_id,
            launch.mcp_enabled,
            launch.browser_mcp_enabled,
            launch.computer_mcp_enabled,
        ) {
            log::warn!("Failed to prepare runtime launch for {runtime_id}: {error}");
            runtime_support_ready = false;
        }
        let mut shell_prelude: Vec<String> = Vec::new();
        let launch_shell_family = shell_family(&shell);
        let trimmed_command = launch.session.command.trim();
        // The startup script below is POSIX sh. Bourne-family login shells run
        // it directly; fish gets a login-shell bridge into /bin/sh (see
        // fish_bridge_script); any other non-Bourne shell launches straight
        // under /bin/sh so the session at least starts (its rc env is skipped,
        // but the script's fallback still lands in the user's own shell).
        let program = if trimmed_command.is_empty() || launch_shell_family != ShellFamily::Other {
            shell.clone()
        } else {
            "/bin/sh".to_string()
        };
        let mut cmd = CommandBuilder::new(&program);
        cmd.cwd(&launch.cwd);
        strip_leaked_launcher_env(&mut cmd);
        strip_runtime_inherited_env(&mut cmd);
        // Defense in depth for direct __session_host__ entry: providers run
        // behind Unpeel's hosted PTY and must not report against a parent
        // Herdr pane even when the detached-host boundary was bypassed.
        strip_leaked_herdr_pty_env(&mut cmd);
        // A Host can itself be launched from inside another Unpeel Session.
        // Generation provenance belongs only to the managed provider child,
        // never to this persistent shell or a blank terminal.
        cmd.env_remove("UNPEEL_RUNTIME_GENERATION");
        cmd.env_remove("NO_COLOR");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Unpeel");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        if !trimmed_command.is_empty() {
            // The Amazon Q / Kiro CLI dotfile integration (pre blocks at the
            // top of ~/.zprofile and ~/.zshrc) execs its qterm shim when it
            // sees a fresh interactive TTY, replacing the login shell before
            // any later PATH exports run, then re-runs our `-c` startup script
            // in a plain non-interactive zsh — so provider CLIs whose PATH
            // entry lives in .zshrc fail with "command not found". The shim
            // sets this var for its own children to prevent double-launch;
            // setting it makes the integration stand down, so command launches
            // always run under the user's full, unwrapped login shell. Blank
            // terminals are left alone: with no `-c` string to lose, qterm is
            // harmless there and users keep its inline autocomplete.
            cmd.env("PROCESS_LAUNCHED_BY_Q", "1");
        }
        match launch.dark_mode {
            Some(true) | None => cmd.env("COLORFGBG", "15;0"),
            Some(false) => cmd.env("COLORFGBG", "0;15"),
        };
        integrations::configure_host_command(runtime_id, &launch, &mut cmd, &mut shell_prelude)?;
        let mut automatic_mcp_registration = integrations::automatic_mcp_registration(
            runtime_id,
            trimmed_command,
            launch.mcp_enabled,
            launch.browser_mcp_enabled,
            launch.computer_mcp_enabled,
        );
        if !runtime_support_ready {
            automatic_mcp_registration = integrations::AutomaticMcpRegistration::default();
        }
        let mcp_client_registered = automatic_mcp_registration.sessions;
        let browser_client_registered = automatic_mcp_registration.browser;
        let computer_client_registered = automatic_mcp_registration.computer;
        // Hold a foreground provider launch until the attach client has synced
        // the surface's real grid to the PTY, so the CLI's first paint matches
        // the window rather than the launch-time initial grid (already-printed
        // banners never reflow on a later resize). Prepended so nothing runs
        // before the size is settled; the provider exports above are size-inert.
        // Codex already waits in its wrapper; this is a harmless second gate for
        // it and the primary gate for Claude/Gemini/etc. See the snippet doc.
        if launch.wait_for_attach {
            shell_prelude.insert(0, attach_ready_wait_snippet(&launch.session.id));
        }
        if trimmed_command.is_empty() {
            cmd.args(["-l", "-i"]);
        } else {
            let startup_command = integrations::startup_command(
                runtime_id,
                trimmed_command,
                launch.mcp_enabled,
                launch.browser_mcp_enabled,
                launch.computer_mcp_enabled,
            );
            let startup_command =
                runtime_generation_scoped_command(ShellFamily::Posix, &startup_command, 1);
            let startup_command = if launches_resume_agent_runtime {
                runtime_launch_completion_command(
                    ShellFamily::Posix,
                    &startup_command,
                    &initial_runtime_completion_path,
                )
            } else {
                startup_command
            };
            let history_snippet = shell_history_append_snippet(&shell, &launch.session.command);
            let shell_script = build_startup_shell_script(
                &shell,
                shell_prelude,
                &startup_command,
                history_snippet,
            );
            let script_arg = match launch_shell_family {
                ShellFamily::Fish => fish_bridge_script(&shell_script),
                ShellFamily::Posix | ShellFamily::Other => shell_script,
            };
            cmd.args(["-l", "-i", "-c", &script_arg]);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn command: {e}"))?;
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to take writer: {e}"))?;
        let reader_fd = pair.master.as_raw_fd();
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone reader: {e}"))?;

        // The reactor reads this master without ever blocking; the writer
        // wrapper waits for POLLOUT itself on the transient command paths.
        let pty_fd = reader_fd.ok_or("PTY master exposes no file descriptor")?;
        session_io::set_nonblocking(pty_fd, true)
            .map_err(|e| format!("Failed to make the PTY master non-blocking: {e}"))?;
        let writer = session_io::PtyWriter::new(writer, pty_fd);

        let pid = child.process_id();
        let pid_started_at = pid.and_then(process_start_time_ms);
        // Patch the preliminary record under the cross-process manifest lock
        // instead of replacing it from the launch payload. A fast hook/title
        // writer may already have enriched it while provider setup ran.
        update_manifest_session(&launch.session.id, |manifest| {
            manifest.cwd = launch.cwd.clone();
            manifest.state = HostedSessionState::Running;
            manifest.pid = pid;
            manifest.pid_started_at = pid_started_at;
            manifest.exit_code = None;
            manifest.host_build_id = host_build_id.clone();
            manifest.host_protocol_version = Some(SESSION_HOST_PROTOCOL_VERSION);
            manifest.runtime_launch_generation = u64::from(launches_stable_runtime);
            manifest.runtime_launch_pending = launches_resume_agent_runtime;
            manifest.runtime_launched_at = initial_runtime_launched_at;
            manifest.runtime_launch_output_offset = journal_start_offset;
            manifest.mcp_enabled = Some(launch.mcp_enabled);
            manifest.browser_mcp_enabled = Some(launch.browser_mcp_enabled);
            manifest.computer_mcp_enabled = Some(launch.computer_mcp_enabled);
            manifest.mcp_client_registered = mcp_client_registered;
            manifest.browser_client_registered = browser_client_registered;
            manifest.computer_client_registered = computer_client_registered;
            manifest.heartbeat_at = current_timestamp_ms();
        })?
        .ok_or("Session manifest disappeared before the hosted PTY became ready")?;

        let runtime = Arc::new(Mutex::new(HostRuntime {
            master: pair.master,
            writer,
            child,
            pty_cols: initial_cols,
            pty_rows: initial_rows,
            shell_executable: PathBuf::from(&shell),
            last_runtime_observation: None,
            recent_write_ids: RecentWriteIds::default(),
        }));
        // Raw local socket clients can bypass the lifecycle-file lock used by
        // `session_ops`; serialize in-place relaunches here as the final
        // authority. Ordinary PTY writes take this lock too, preventing input
        // from being interleaved between stop, shell recovery, and relaunch.
        let agent_restart_lock = Arc::new(Mutex::new(()));
        let runtime_generation = Arc::new(AtomicU64::new(0));
        let pending_runtime_generation =
            Arc::new(AtomicU64::new(if launches_resume_agent_runtime {
                1
            } else {
                0
            }));
        let mut viewport_state = TerminalViewportState::new(initial_cols, initial_rows);
        // R2-COORD (Lane 3): the resident grid keeps no raw output copy;
        // resized and deep-scroll snapshots replay this session's journal.
        viewport_state.set_journal_session(launch.session.id.clone());
        viewport_state.reset_at_output_offset(
            initial_cols,
            initial_rows,
            journal_start_offset,
            journal_start_offset > 0,
        );
        let viewport = Arc::new(Mutex::new(viewport_state));
        let running = Arc::new(AtomicBool::new(true));
        let broadcaster = Arc::new(Mutex::new(OutputBroadcaster::at_offset(
            journal_start_offset,
        )));
        // The periodic Host jobs (heartbeat, runtime observer, menu/screen
        // scan) share ONE timer thread (`HostTimerJob`, spawned below once
        // every job is built) instead of a thread each: at 50+ sessions the
        // idle stacks alone were measurable. Each job keeps its own cadence
        // and state; they simply run back to back on the same thread, so no
        // job may ever block indefinitely (none holds a lock across a sleep).
        let timer_jobs = build_session_timer_jobs(SessionJobInputs {
            session_id: launch.session.id.clone(),
            command: launch.session.command.clone(),
            shell: shell.clone(),
            pid,
            pid_started_at,
            runtime: Arc::clone(&runtime),
            runtime_generation: Arc::clone(&runtime_generation),
            pending_runtime_generation: Arc::clone(&pending_runtime_generation),
            viewport: Arc::clone(&viewport),
        });

        // The short-fallback socket lives outside the session dir (e.g.
        // /tmp/unpeel-<uid>/), so make sure its parent exists and is private
        // before bind. Idempotent for the normal in-session-dir case.
        if let Some(parent) = session_socket_path.parent() {
            let _ = fs::create_dir_all(parent);
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
        let listener = UnixListener::bind(&session_socket_path).map_err(|e| {
            format!(
                "Failed to bind session socket {}: {e}",
                session_socket_path.display()
            )
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Failed to configure session socket: {e}"))?;

        let session_id = launch.session.id.clone();
        let has_been_written_to = Arc::new(AtomicBool::new(false));
        // Auto-title state shared by all socket clients: the partially-typed
        // prompt line, and a latch flipped once the title is settled.
        let title_buffer = Arc::new(Mutex::new(String::new()));
        let title_done = Arc::new(AtomicBool::new(launch.session.custom_title));

        // From here on the Session owns no thread: the shared reactor reads
        // its PTY and serves its socket, the core-wide timer runs its jobs,
        // the core-wide writer persists its journal, and the teardown thread
        // publishes its exit edge (see `session_io` / `core_reactor`).
        let services = core_reactor::services()?;
        let shared = Arc::new(session_io::SessionShared {
            session_id: session_id.clone(),
            runtime: Arc::clone(&runtime),
            viewport: Arc::clone(&viewport),
            running: Arc::clone(&running),
            broadcaster: Arc::clone(&broadcaster),
            title_buffer,
            title_done,
            has_been_written_to,
            agent_restart_lock: Arc::clone(&agent_restart_lock),
            runtime_generation: Arc::clone(&runtime_generation),
            pending_runtime_generation: Arc::clone(&pending_runtime_generation),
            reactor: services.reactor.clone(),
            slot: AtomicUsize::new(usize::MAX),
        });
        let pressure = Arc::new(session_io::JournalBackpressure::default());
        let journal_id = session_io::next_journal_id();
        services
            .journal_tx
            .send(core_reactor::JournalMsg::Open {
                id: journal_id,
                writer: retained_output,
                pressure: Arc::clone(&pressure),
            })
            .map_err(|_| "Output writer is gone".to_string())?;
        let journal = session_io::JournalHandle {
            id: journal_id,
            tx: services.journal_tx.clone(),
            pressure,
            failed: Arc::new(AtomicBool::new(false)),
        };
        let jobs = timer_jobs;
        let exit = session_io::SessionExitPlan {
            pid,
            pid_started_at,
            host_build_id: host_build_id.clone(),
            session_socket_path: session_socket_path.clone(),
            on_exit,
        };
        let session = session_io::SessionIo::new(
            shared,
            pty_fd,
            reader,
            listener,
            launch.dark_mode.unwrap_or(true),
            journal,
            jobs,
            exit,
            session_io::JobSeed {
                command: launch.session.command.clone(),
                shell: shell.clone(),
                pid,
                pid_started_at,
            },
        );
        if let Err(error) = core_reactor::host_session(session) {
            let (ack_tx, _ack_rx) = mpsc::channel();
            let _ = services.journal_tx.send(core_reactor::JournalMsg::Close {
                id: journal_id,
                ack: ack_tx,
            });
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{
        active_runtime_id, apply_agent_terminal_title, apply_manifest_auto_title,
        attach_ready_path, attach_ready_wait_snippet, build_startup_shell_script,
        cleanup_session_artifacts, compact_output_journal_path, ensure_managed_storage_path,
        env_keys_with_prefix, extract_submitted_prompt, fallback_shell_exec_snippet,
        fish_bridge_script, fish_single_quote, load_manifest, manifest_heartbeat_is_stale,
        manifest_host_is_healthy, manifest_last_heartbeat_at, manifest_pid_identity,
        normalize_prompt_title, output_path, output_retained_from, process_start_time_ms,
        read_input_stream_frame, read_output_chunk, read_output_stream_frame,
        refresh_manifest_health_from_manifest, run_batched_output_stream_forwarder,
        run_batched_output_writer, run_host, runtime_generation_scoped_command,
        safe_output_retention_boundary, save_manifest, shell_family,
        strip_env_prefix_from_process_command, strip_env_prefix_from_pty_command,
        update_manifest_session, write_output_stream_frame, HostAnsweredQuery,
        HostedSessionManifest, HostedSessionRuntime, HostedSessionState, OscTitleScanner,
        OutputBroadcaster, OutputQueryScanner, OutputStreamRead, PidIdentity, RetainedOutputWriter,
        RuntimeObservationTracker, SessionExecutionScope, SessionHostCommand, SessionHostLaunch,
        SessionOutputChunk, ShellFamily, HERDR_ENV_PREFIX, LEAKED_LAUNCHER_ENV_KEYS,
        OSC_TITLE_MAX_PAYLOAD_BYTES, PRIMARY_DEVICE_ATTRIBUTES_RESPONSE,
        SESSION_RUNTIME_CLEAR_MISSES, SESSION_TITLE_MODE_FOR_TEST,
    };
    #[cfg(unix)]
    use super::{
        configure_session_client, prepare_runtime_launch_completion_marker, stage_restart_marker,
        SESSION_INPUT_STREAM_ACK,
    };
    use super::{validate_write_id, RecentWriteIds, WRITE_ID_HISTORY, WRITE_ID_MAX_BYTES};
    use crate::runtime_observer::ActiveRuntimeObservation;
    use crate::state::SessionInfo;
    use portable_pty::CommandBuilder;
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn agent_control_wire_keeps_legacy_restart_and_adds_resume() {
        assert!(matches!(
            serde_json::from_value::<SessionHostCommand>(serde_json::json!({
                "type": "restart_agent",
                "expected_generation": 4
            }))
            .unwrap(),
            SessionHostCommand::RestartAgent {
                expected_generation: 4
            }
        ));
        assert!(matches!(
            serde_json::from_value::<SessionHostCommand>(serde_json::json!({
                "type": "resume_agent",
                "expected_generation": 5
            }))
            .unwrap(),
            SessionHostCommand::ResumeAgent {
                expected_generation: 5
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unresolved_staged_hook_marker_rolls_back_on_early_return() {
        let temp = tempfile::tempdir().expect("staged hook marker temp dir");
        let marker_path = temp.path().join("last-hook-event.json");
        let bytes = br#"{"event":"exact-old-generation"}"#;
        fs::write(&marker_path, bytes).expect("write hook marker");

        let staged = stage_restart_marker(marker_path.clone(), 2)
            .expect("stage hook marker")
            .expect("existing marker stages");
        assert!(!marker_path.exists());
        // Simulate any `?` between marker staging and the irreversible PTY
        // write. Drop must restore the exact bytes without each error path
        // having to remember an explicit rollback call.
        drop(staged);

        assert_eq!(fs::read(&marker_path).expect("restored hook marker"), bytes);
        assert_eq!(
            fs::read_dir(temp.path())
                .expect("marker dir")
                .filter_map(Result::ok)
                .count(),
            1,
            "no staged tombstone remains after rollback"
        );
    }

    #[cfg(unix)]
    #[test]
    fn launch_refuses_an_undeletable_stale_completion_marker() {
        let temp = tempfile::tempdir().expect("completion marker temp dir");
        let marker_path = temp.path().join(".runtime-launch-complete");
        fs::create_dir(&marker_path).expect("create stale marker");
        fs::write(marker_path.join("unexpected-child"), b"stale").expect("make marker nonempty");

        let error = prepare_runtime_launch_completion_marker(&marker_path)
            .expect_err("nonempty stale marker must fail closed");
        assert!(error.contains("Failed to clear stale runtime launch completion marker"));
        assert!(marker_path.is_dir());

        fs::remove_file(marker_path.join("unexpected-child")).expect("empty stale marker");
        prepare_runtime_launch_completion_marker(&marker_path)
            .expect("empty stale marker can be removed and verified absent");
        assert!(fs::symlink_metadata(&marker_path).is_err());
    }

    #[test]
    fn write_id_dedup_catches_retries_and_ages_out() {
        let mut history = RecentWriteIds::default();
        let first = "wid-dedup-test-A";
        assert!(!history.contains(first), "first sighting is new");
        history.record_applied(first);
        assert!(history.contains(first), "an applied id is remembered");

        // A different id is independent.
        assert!(!history.contains("wid-dedup-test-B"));

        // Push enough distinct ids to evict `first`, proving the window is
        // bounded and old keys age out rather than growing without bound.
        for index in 0..(WRITE_ID_HISTORY + 8) {
            history.record_applied(&format!("wid-dedup-test-fill-{index}"));
        }
        assert!(
            !history.contains(first),
            "an id evicted from the bounded window is treated as new again"
        );
    }

    #[test]
    fn write_id_validation_bounds_retained_keys_at_the_host_boundary() {
        assert_eq!(validate_write_id(None), Ok(None));
        assert_eq!(validate_write_id(Some("")), Ok(None));
        let maximum = "a".repeat(WRITE_ID_MAX_BYTES);
        assert_eq!(
            validate_write_id(Some(&maximum)),
            Ok(Some(maximum.as_str()))
        );
        let oversized = "a".repeat(WRITE_ID_MAX_BYTES + 1);
        assert!(validate_write_id(Some(&oversized)).is_err());
        // The Rust boundary is byte-oriented, matching Swift's utf8.count.
        let oversized_unicode = "é".repeat((WRITE_ID_MAX_BYTES / 2) + 1);
        assert!(validate_write_id(Some(&oversized_unicode)).is_err());
    }

    #[test]
    fn screen_change_tracker_ignores_identical_repaints() {
        let mut tracker = super::ScreenChangeTracker::default();
        // First sighting stamps (the session just painted its screen).
        assert_eq!(tracker.observe("prompt box", 1_000), Some(1_000));
        // An idle animation repaints the SAME text forever: no stamps.
        for tick in 1..200u64 {
            assert_eq!(tracker.observe("prompt box", 1_000 + tick * 500), None);
        }
        // Real new content stamps again.
        assert_eq!(
            tracker.observe("prompt box + reply", 200_000),
            Some(200_000)
        );
    }

    #[test]
    fn screen_change_tracker_coalesces_but_keeps_trailing_stamp() {
        let mut tracker = super::ScreenChangeTracker::default();
        assert_eq!(tracker.observe("a", 1_000), Some(1_000));
        // Changes every tick: writes come at most every coalesce window.
        assert_eq!(tracker.observe("b", 1_500), None);
        assert_eq!(tracker.observe("c", 2_000), None);
        assert_eq!(tracker.observe("d", 3_000), Some(3_000));
        // Content settles on "e" at 3.4s; the trailing change must still be
        // persisted once the window reopens, even with no further changes.
        assert_eq!(tracker.observe("e", 3_400), None);
        assert_eq!(tracker.observe("e", 5_500), Some(3_400));
        // Fully settled: no more writes.
        assert_eq!(tracker.observe("e", 60_000), None);
    }

    #[derive(Clone, Default)]
    struct RecordingWriter {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl RecordingWriter {
        fn snapshots(&self) -> Vec<Vec<u8>> {
            self.writes.lock().unwrap().clone()
        }
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.lock().unwrap().push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn launch_with_execution_scope(scope: Option<&str>) -> SessionHostLaunch {
        let mut value = serde_json::json!({
            "session": {
                "id": "scope-test",
                "project_id": "project",
                "label": "scope test",
                "command": "true"
            },
            "cwd": "/tmp",
            "dark_mode": null,
            "hook_port": null
        });
        if let Some(scope) = scope {
            value["execution_scope"] = scope.into();
        }
        serde_json::from_value(value).expect("session launch fixture")
    }

    #[test]
    fn legacy_launch_scope_defaults_to_local() {
        assert_eq!(
            launch_with_execution_scope(None).execution_scope,
            SessionExecutionScope::Local
        );
    }

    #[test]
    fn remote_controller_scope_is_rejected_before_host_side_effects() {
        let launch = launch_with_execution_scope(Some("remote_controller"));
        assert_eq!(
            launch.execution_scope,
            SessionExecutionScope::RemoteController
        );
        let error = run_host(launch).expect_err("remote scope cannot launch locally");
        assert!(
            error.contains("Refusing local session execution"),
            "{error}"
        );
    }

    #[test]
    fn leaked_claude_env_keys_include_child_session_marker() {
        // An inherited CLAUDE_CODE_CHILD_SESSION makes a nested interactive
        // `claude` disable transcript saving and resume — the marker must be
        // stripped for every hosted session.
        let catalog = crate::runtime_catalog::builtin_runtime_catalog();
        let claude = catalog.by_legacy_slug("claude").expect("Claude runtime");
        assert!(claude
            .environment
            .strip_inherited
            .iter()
            .any(|key| key == "CLAUDE_CODE_CHILD_SESSION"));
        assert!(claude
            .environment
            .strip_inherited
            .iter()
            .any(|key| key == "CLAUDECODE"));
    }

    #[test]
    fn leaked_codex_env_keys_include_tui_recording_vars() {
        let catalog = crate::runtime_catalog::builtin_runtime_catalog();
        let codex = catalog.by_legacy_slug("codex").expect("Codex runtime");
        assert!(codex
            .environment
            .strip_inherited
            .iter()
            .any(|key| key == "CODEX_TUI_RECORD_SESSION"));
        assert!(codex
            .environment
            .strip_inherited
            .iter()
            .any(|key| key == "CODEX_TUI_SESSION_LOG_PATH"));
    }

    #[test]
    fn generic_launcher_env_removes_parent_color_tier_overrides() {
        assert!(LEAKED_LAUNCHER_ENV_KEYS.contains(&"FORCE_COLOR"));
        assert!(LEAKED_LAUNCHER_ENV_KEYS.contains(&"CLICOLOR_FORCE"));
    }

    #[test]
    fn herdr_env_prefix_selects_all_and_only_inherited_herdr_keys() {
        let vars = [
            ("HERDR_ENV", "1"),
            ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
            ("HERDR_PLUGIN_FUTURE_SETTING", "enabled"),
            ("HERDR", "not-prefixed"),
            ("herdr_PANE_ID", "wrong-case"),
            ("UNPEEL_HERDR_STATUS", "on"),
            ("UNPEEL_SESSION_ID", "session-1"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));

        assert_eq!(
            env_keys_with_prefix(vars, "HERDR_"),
            vec![
                OsString::from("HERDR_ENV"),
                OsString::from("HERDR_SOCKET_PATH"),
                OsString::from("HERDR_PLUGIN_FUTURE_SETTING"),
            ]
        );
    }

    fn herdr_env_fixture() -> Vec<(OsString, OsString)> {
        [
            ("HERDR_ENV", "1"),
            ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
            ("HERDR_PLUGIN_FUTURE_SETTING", "enabled"),
            ("HERDR", "not-prefixed"),
            ("herdr_PANE_ID", "wrong-case"),
            ("UNPEEL_HERDR_STATUS", "on"),
            ("UNPEEL_SESSION_ID", "session-1"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
    }

    #[test]
    fn herdr_env_is_removed_from_detached_process_command() {
        let vars = herdr_env_fixture();
        let mut cmd = std::process::Command::new("true");
        cmd.env_clear();
        for (key, value) in &vars {
            cmd.env(key, value);
        }

        strip_env_prefix_from_process_command(&mut cmd, vars, HERDR_ENV_PREFIX);

        assert!(cmd.get_envs().all(|(key, value)| {
            !key.as_encoded_bytes().starts_with(b"HERDR_") || value.is_none()
        }));
        for (key, expected) in [
            ("HERDR", "not-prefixed"),
            ("herdr_PANE_ID", "wrong-case"),
            ("UNPEEL_HERDR_STATUS", "on"),
            ("UNPEEL_SESSION_ID", "session-1"),
            ("PATH", "/usr/bin"),
        ] {
            let value = cmd
                .get_envs()
                .find(|(candidate, _)| *candidate == OsStr::new(key))
                .and_then(|(_, value)| value);
            assert_eq!(value, Some(OsStr::new(expected)), "{key}");
        }
    }

    #[test]
    fn herdr_env_is_removed_from_provider_pty_command() {
        let vars = herdr_env_fixture();
        let mut cmd = CommandBuilder::new("true");
        cmd.env_clear();
        for (key, value) in &vars {
            cmd.env(key, value);
        }

        strip_env_prefix_from_pty_command(&mut cmd, vars, HERDR_ENV_PREFIX);

        assert!(cmd
            .iter_full_env_as_str()
            .all(|(key, _)| !key.starts_with(HERDR_ENV_PREFIX)));
        for (key, expected) in [
            ("HERDR", "not-prefixed"),
            ("herdr_PANE_ID", "wrong-case"),
            ("UNPEEL_HERDR_STATUS", "on"),
            ("UNPEEL_SESSION_ID", "session-1"),
            ("PATH", "/usr/bin"),
        ] {
            assert_eq!(cmd.get_env(key), Some(OsStr::new(expected)), "{key}");
        }
    }

    #[test]
    fn fallback_shell_exec_is_login_interactive() {
        assert_eq!(
            fallback_shell_exec_snippet("/bin/zsh"),
            "exec '/bin/zsh' -l -i"
        );
    }

    #[test]
    fn startup_shell_script_survives_nonzero_provider_exit() {
        let script = build_startup_shell_script(
            "/bin/zsh",
            vec!["export UNPEEL_SESSION_ID='session-1'".to_string()],
            "false",
            Some("print -sr -- 'codex'; fc -AI".to_string()),
        );

        assert!(script.starts_with("set +e; "));
        assert!(!script.contains("printf '$"));
        assert!(script.contains(
            "; trap : INT; { false; }; __unpeel_startup_status=$?; trap - INT; set +e; "
        ));
        assert!(script.contains("; { print -sr -- 'codex'; fc -AI; }; set +e; "));
        assert!(script.ends_with("; exec '/bin/zsh' -l -i"));
    }

    /// Ctrl-C into the runtime must land the user in the fallback shell, not
    /// end the Session. Drives the real startup script under `zsh -f -i -c`
    /// on a PTY (the shape the Host uses), interrupts the foreground job, and
    /// expects the segment after the command block to run. Without the INT
    /// handler zsh abandons the list (reproduced on 0.4.3 and main,
    /// 2026-09-03); bash never needed it.
    #[cfg(unix)]
    #[test]
    fn startup_shell_script_survives_ctrl_c_into_the_runtime_under_zsh() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        use std::io::Read;
        use std::time::{Duration, Instant};

        if !std::path::Path::new("/bin/zsh").exists() {
            eprintln!("skipping: no /bin/zsh on this machine");
            return;
        }
        // The "history" segment doubles as the survival marker; the trailing
        // fallback exec goes to `true` so the shell ends by itself.
        let script = build_startup_shell_script(
            "/usr/bin/true",
            vec!["export UNPEEL_TEST_MARK=1".to_string()],
            "sleep 30",
            Some("printf 'SURVIVED status=%s' \"$__unpeel_startup_status\"".to_string()),
        );
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("/bin/zsh");
        cmd.args(["-f", "-i", "-c", &script]);
        let mut child = pair.slave.spawn_command(cmd).expect("spawn zsh");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let mut writer = pair.master.take_writer().expect("writer");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        // Give the shell time to reach `sleep`, then interrupt the foreground.
        std::thread::sleep(Duration::from_millis(700));
        writer.write_all(b"\x03").expect("send ctrl-c");
        writer.flush().ok();
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                out.extend_from_slice(&chunk);
            }
            if out.windows(8).any(|w| w == b"SURVIVED") {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("SURVIVED status=130"),
            "wrapper shell did not survive Ctrl-C: {text:?}"
        );
    }

    #[test]
    fn runtime_generation_is_visible_only_inside_provider_invocation() {
        let scoped = runtime_generation_scoped_command(
            ShellFamily::Posix,
            "printf 'inside=%s;' \"$UNPEEL_RUNTIME_GENERATION\"",
            9,
        );
        let output = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                &format!("{scoped}; printf 'after=%s' \"${{UNPEEL_RUNTIME_GENERATION-unset}}\""),
            ])
            .output()
            .expect("run scoped runtime command");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "inside=9;after=unset"
        );

        let failed = runtime_generation_scoped_command(ShellFamily::Posix, "false", 9);
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", &failed])
            .status()
            .expect("run failed scoped runtime command");
        assert_eq!(status.code(), Some(1), "provider status must propagate");

        let fish = runtime_generation_scoped_command(ShellFamily::Fish, "claude", 10);
        assert!(fish.starts_with("/usr/bin/env UNPEEL_RUNTIME_GENERATION=10 /bin/sh -c "));
        assert!(fish.contains("set +e; { claude; }"));

        // Keep the defensive Other-family quoting branch correct even though
        // Resume Agent rejects unknown live shells before launch preparation.
        let other = runtime_generation_scoped_command(
            ShellFamily::Other,
            r#"printf 'other=%s:%s' "$UNPEEL_RUNTIME_GENERATION" "it's quoted""#,
            11,
        );
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &other])
            .output()
            .expect("run other-shell scoped runtime command");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "other=11:it's quoted"
        );
    }

    #[test]
    fn fish_relaunch_executes_posix_kimi_and_cline_recipes_and_preserves_status() {
        use std::os::unix::fs::PermissionsExt;

        let Some(fish) = crate::setup::find_command_path("fish", &crate::setup::search_dirs())
        else {
            // The pure quoting/status test above remains mandatory on hosts
            // without fish; this process proof runs wherever fish is present.
            return;
        };
        let temp = tempfile::tempdir().expect("runtime wrapper temp dir");
        let write_fake = |name: &str, body: &str| {
            let path = temp.path().join(name);
            std::fs::write(&path, body).expect("write fake provider");
            let mut permissions = std::fs::metadata(&path)
                .expect("fake provider metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&path, permissions).expect("chmod fake provider");
        };
        write_fake(
            "kimi",
            r#"#!/bin/sh
if [ "${1:-}" = "--help" ]; then
  printf '%s\n' '--mcp-config-file'
  exit 0
fi
printf 'kimi-generation=%s' "${UNPEEL_RUNTIME_GENERATION-unset}"
exit "${UNPEEL_FAKE_PROVIDER_STATUS:-0}"
"#,
        );
        write_fake(
            "cline",
            r#"#!/bin/sh
if [ "${1:-}" = "hub" ]; then
  exit 0
fi
printf 'cline-generation=%s' "${UNPEEL_RUNTIME_GENERATION-unset}"
exit "${UNPEEL_FAKE_PROVIDER_STATUS:-0}"
"#,
        );
        let inherited_path = std::env::var("PATH").unwrap_or_default();
        let path = format!("{}:{inherited_path}", temp.path().display());

        for (runtime, startup_command) in [
            (
                "kimi",
                crate::integrations::startup_command("kimi", "kimi", true, false, false),
            ),
            (
                "cline",
                crate::integrations::startup_command("cline", "cline", false, false, false),
            ),
        ] {
            let scoped = runtime_generation_scoped_command(ShellFamily::Fish, &startup_command, 23);
            let fish_script = format!(
                "{scoped}; set -l __unpeel_test_status $status; \
                 set -l __unpeel_test_after leaked; \
                 if set -q UNPEEL_RUNTIME_GENERATION; \
                 else; set __unpeel_test_after unset; end; \
                 printf '|after=%s' $__unpeel_test_after; \
                 command /bin/sh -c \"exit $__unpeel_test_status\""
            );
            let output = std::process::Command::new(&fish)
                .args(["-c", &fish_script])
                .env("PATH", &path)
                // Hosted children explicitly remove this Host-owned marker
                // before starting the interactive shell. Keep the process
                // proof independent of a test runner that happens to carry
                // its own generation marker.
                .env_remove("UNPEEL_RUNTIME_GENERATION")
                .env("UNPEEL_FAKE_PROVIDER_STATUS", "17")
                .output()
                .expect("run fish provider relaunch");
            assert_eq!(
                output.status.code(),
                Some(17),
                "{runtime} status/stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                format!("{runtime}-generation=23|after=unset"),
                "{runtime} stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn shell_family_classifies_login_shells() {
        assert_eq!(shell_family("/bin/zsh"), ShellFamily::Posix);
        assert_eq!(shell_family("/bin/bash"), ShellFamily::Posix);
        assert_eq!(shell_family("/bin/sh"), ShellFamily::Posix);
        assert_eq!(shell_family("/opt/homebrew/bin/fish"), ShellFamily::Fish);
        assert_eq!(shell_family("/usr/local/bin/fish"), ShellFamily::Fish);
        assert_eq!(shell_family("/opt/homebrew/bin/nu"), ShellFamily::Other);
        assert_eq!(shell_family("/bin/tcsh"), ShellFamily::Other);
    }

    #[test]
    fn managed_storage_creation_stays_beneath_unpeel_home() {
        let temp = tempfile::tempdir().expect("managed storage root");
        let root = temp.path().join(".unpeel");
        std::fs::create_dir(&root).expect("create Unpeel root");
        let managed = root.join("runtime-storage").join("session-1");
        ensure_managed_storage_path(&root, &managed).expect("create managed storage");
        assert!(managed.is_dir());

        let escaped = root.join("..").join("outside");
        assert!(ensure_managed_storage_path(&root, &escaped).is_err());
        assert!(!temp.path().join("outside").exists());
    }

    #[cfg(unix)]
    #[test]
    fn managed_storage_creation_rejects_symlink_hops() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("managed storage root");
        let root = temp.path().join(".unpeel");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).expect("create Unpeel root");
        std::fs::create_dir(&outside).expect("create outside dir");
        symlink(&outside, root.join("runtime-storage")).expect("create symlink");

        let requested = root.join("runtime-storage").join("session-1");
        assert!(ensure_managed_storage_path(&root, &requested).is_err());
        assert!(!outside.join("session-1").exists());
    }

    #[test]
    fn fish_single_quote_escapes_quotes_and_backslashes() {
        assert_eq!(fish_single_quote("plain"), "'plain'");
        assert_eq!(fish_single_quote("it's"), r"'it\'s'");
        assert_eq!(fish_single_quote(r"a\b"), r"'a\\b'");
    }

    #[test]
    fn fish_bridge_wraps_posix_script_for_sh() {
        let script = build_startup_shell_script(
            "/opt/homebrew/bin/fish",
            vec!["export UNPEEL_SESSION_ID='session-1'".to_string()],
            "claude",
            None,
        );
        let bridge = fish_bridge_script(&script);
        assert!(bridge.starts_with("exec /bin/sh -c '"));
        // The POSIX script's single quotes must be fish-escaped, not POSIX
        // '"'"'-spliced, so fish hands /bin/sh the script byte-for-byte.
        assert!(bridge.contains(r"export UNPEEL_SESSION_ID=\'session-1\'"));
        assert!(bridge.ends_with(r"exec \'/opt/homebrew/bin/fish\' -l -i'"));
    }

    /// Live regression test for the fish-login-shell launch failure: the raw
    /// POSIX startup script must NOT be handed to `fish -c` (parse error, the
    /// session dies at spawn), and the /bin/sh bridge must run it verbatim.
    /// Skips when fish is not installed.
    #[test]
    fn fish_login_shell_bridge_runs_posix_startup_script() {
        let fish = [
            "/opt/homebrew/bin/fish",
            "/usr/local/bin/fish",
            "/usr/bin/fish",
        ]
        .iter()
        .find(|path| std::path::Path::new(path).is_file());
        let Some(fish) = fish else {
            eprintln!("fish not installed; skipping live fish bridge test");
            return;
        };

        // Fallback exec target is /usr/bin/true so the test never lands in an
        // interactive shell; everything before it is the real composed script.
        let script = build_startup_shell_script(
            "/usr/bin/true",
            vec![
                "export UNPEEL_FISH_TEST='bridged'".to_string(),
                // Same POSIX constructs as attach_ready_wait_snippet.
                "__i=0; while [ \"$__i\" -lt 3 ]; do __i=$((__i+1)); done".to_string(),
            ],
            "printf '%s' \"marker=$UNPEEL_FISH_TEST\"",
            None,
        );

        let raw = std::process::Command::new(fish)
            .args(["-l", "-i", "-c", &script])
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run fish with raw script");
        assert!(
            !raw.status.success(),
            "raw POSIX script unexpectedly parsed under fish; bridge may be droppable"
        );

        let bridged = std::process::Command::new(fish)
            .args(["-l", "-i", "-c", &fish_bridge_script(&script)])
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run fish with bridged script");
        let stdout = String::from_utf8_lossy(&bridged.stdout);
        assert!(
            bridged.status.success() && stdout.contains("marker=bridged"),
            "bridged script failed under fish: status={:?} stdout={stdout:?} stderr={:?}",
            bridged.status,
            String::from_utf8_lossy(&bridged.stderr)
        );
    }

    #[test]
    fn attach_ready_wait_snippet_blocks_on_session_ready_file() {
        let snippet = attach_ready_wait_snippet("session-1");
        let ready = attach_ready_path("session-1");
        // References this session's .attach-ready file, loops with a bounded
        // count, and uses only POSIX test/sleep so it runs under zsh and bash.
        assert!(snippet.contains(".attach-ready"));
        assert!(snippet.contains(&*ready.to_string_lossy()));
        assert!(snippet.contains("-lt 100"));
        assert!(snippet.contains("sleep 0.02"));
        assert!(!snippet.contains("{1..100}"));
    }

    fn manifest_with_times(updated_at: u64, heartbeat_at: u64) -> HostedSessionManifest {
        HostedSessionManifest {
            session: SessionInfo {
                id: "session-1".to_string(),
                project_id: "project-1".to_string(),
                label: "Claude".to_string(),
                custom_title: false,
                command: "claude".to_string(),
                created_at: 1,
                owner_principal_id: None,
                created_by_device_id: None,
                source_preset_id: None,
                tag_id: None,
                worktree_path: None,
                worktree_branch: None,
                parent_session_id: None,
                spawned_by: None,
                role: None,
                task: None,
            },
            cwd: "/tmp/project".to_string(),
            state: HostedSessionState::Running,
            pid: Some(42),
            pid_started_at: None,
            host_pid: None,
            host_pid_started_at: None,
            exit_code: None,
            host_build_id: None,
            host_protocol_version: None,
            has_been_written_to: true,
            provider_session_id: None,
            provider_transcript_path: None,
            managed_storage_path: None,
            resume_failure_markers: Vec::new(),
            runtime: None,
            active_app: None,
            runtime_launch_generation: 1,
            runtime_launch_pending: false,
            runtime_launched_at: Some(1),
            runtime_launch_output_offset: 0,
            mcp_enabled: None,
            browser_mcp_enabled: None,
            computer_mcp_enabled: None,
            mcp_client_registered: false,
            browser_client_registered: false,
            computer_client_registered: false,
            menu_prompt_active: false,
            terminal_modes: None,
            screen_changed_at: None,
            detected_local_urls: Vec::new(),
            heartbeat_at,
            updated_at,
        }
    }

    fn observed_runtime(id: &str, pid: u32) -> ActiveRuntimeObservation {
        ActiveRuntimeObservation {
            runtime_id: id.to_string(),
            pid,
            pid_started_at: Some(1_000 + u64::from(pid)),
            process_group_id: pid,
            process_name: id.to_string(),
            argv: Some(vec![id.to_string()]),
        }
    }

    #[test]
    fn runtime_observation_hysteresis_keeps_tools_but_clears_on_shell() {
        let claude = observed_runtime("claude", 51);
        let mut tracker = RuntimeObservationTracker::default();
        assert_eq!(
            tracker.observe(Some(claude.clone()), false),
            Some(Some(claude.clone()))
        );
        assert_eq!(tracker.observe(Some(claude.clone()), false), None);

        for _ in 1..SESSION_RUNTIME_CLEAR_MISSES {
            assert_eq!(tracker.observe(None, false), None);
        }
        assert_eq!(tracker.current.as_ref(), Some(&claude));
        assert_eq!(tracker.observe(None, false), Some(None));

        assert_eq!(
            tracker
                .observe(Some(claude), false)
                .flatten()
                .unwrap()
                .runtime_id,
            "claude"
        );
        assert_eq!(tracker.observe(None, true), Some(None));
    }

    #[test]
    fn current_runtime_is_nested_and_never_rewrites_the_launch_command() {
        let mut manifest = manifest_with_times(1, 1);
        manifest.session.command.clear();
        manifest.runtime = Some(HostedSessionRuntime {
            current_observation: Some(observed_runtime("claude", 51)),
        });

        assert_eq!(active_runtime_id(&manifest), Some("claude"));
        let value = serde_json::to_value(&manifest).expect("serialize manifest");
        assert_eq!(value["runtime"]["currentObservation"]["id"], "claude");
        assert_eq!(value["session"]["command"], "");

        manifest.state = HostedSessionState::Exited;
        assert_eq!(active_runtime_id(&manifest), None);
    }

    fn unique_session_id(prefix: &str) -> String {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let suffix = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{suffix}")
    }

    fn manifest_for_auto_title(
        session_id: &str,
        command: &str,
        label: &str,
    ) -> HostedSessionManifest {
        HostedSessionManifest {
            session: SessionInfo {
                id: session_id.to_string(),
                project_id: "project-1".to_string(),
                label: label.to_string(),
                custom_title: false,
                command: command.to_string(),
                created_at: 1,
                owner_principal_id: None,
                created_by_device_id: None,
                source_preset_id: None,
                tag_id: None,
                worktree_path: None,
                worktree_branch: None,
                parent_session_id: None,
                spawned_by: None,
                role: None,
                task: None,
            },
            cwd: "/tmp/project".to_string(),
            state: HostedSessionState::Running,
            pid: Some(42),
            pid_started_at: None,
            host_pid: None,
            host_pid_started_at: None,
            exit_code: None,
            host_build_id: None,
            host_protocol_version: None,
            has_been_written_to: false,
            provider_session_id: None,
            provider_transcript_path: None,
            managed_storage_path: None,
            resume_failure_markers: Vec::new(),
            runtime: None,
            active_app: None,
            runtime_launch_generation: u64::from(!command.trim().is_empty()),
            runtime_launch_pending: false,
            runtime_launched_at: (!command.trim().is_empty()).then_some(1),
            runtime_launch_output_offset: 0,
            mcp_enabled: None,
            browser_mcp_enabled: None,
            computer_mcp_enabled: None,
            mcp_client_registered: false,
            browser_client_registered: false,
            computer_client_registered: false,
            menu_prompt_active: false,
            terminal_modes: None,
            screen_changed_at: None,
            detected_local_urls: Vec::new(),
            heartbeat_at: 1,
            updated_at: 1,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn manifest_pid_identity_verifies_start_time() {
        let self_pid = std::process::id();
        let started = process_start_time_ms(self_pid).expect("own process start time");
        let mut manifest = manifest_with_times(1, 1);
        manifest.pid = Some(self_pid);

        manifest.pid_started_at = Some(started);
        assert_eq!(manifest_pid_identity(&manifest), PidIdentity::Matches);

        // A recorded start an hour off means the pid was recycled onto an
        // unrelated process — must be positively refuted, never killed.
        manifest.pid_started_at = Some(started.saturating_sub(3_600_000));
        assert_eq!(manifest_pid_identity(&manifest), PidIdentity::NotOurs);

        // Legacy manifest (no recorded start): this test process's argv does
        // not mention the session id, so identity must stay unprovable.
        manifest.pid_started_at = None;
        assert_eq!(manifest_pid_identity(&manifest), PidIdentity::Unknown);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn manifest_host_is_healthy_rejects_recycled_pid() {
        let self_pid = std::process::id();
        let started = process_start_time_ms(self_pid).expect("own process start time");
        let mut manifest = manifest_with_times(1, 1);
        manifest.pid = Some(self_pid);
        manifest.pid_started_at = Some(started.saturating_sub(3_600_000));
        assert!(!manifest_host_is_healthy(&manifest));
    }

    #[test]
    fn extract_submitted_prompt_returns_typed_line_on_enter() {
        let mut buffer = String::new();
        assert_eq!(extract_submitted_prompt(&mut buffer, "fix the "), None);
        assert_eq!(
            extract_submitted_prompt(&mut buffer, "header bug\r"),
            Some("fix the header bug".to_string())
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn extract_submitted_prompt_skips_escape_sequences_and_handles_backspace() {
        let mut buffer = String::new();
        // Arrow key (CSI), an OSC sequence, typing with one backspace.
        let candidate =
            extract_submitted_prompt(&mut buffer, "\x1b[A\x1b]0;title\x07hellp\u{7f}o\r");
        assert_eq!(candidate, Some("hello".to_string()));
    }

    #[test]
    fn extract_submitted_prompt_skips_apc_graphics_response() {
        // Ghostty answers OpenCode's kitty-graphics probe on stdin with an
        // APC string; it must not become part of the auto-title.
        let mut buffer = String::new();
        let candidate = extract_submitted_prompt(&mut buffer, "\x1b_Gi=31337;OK\x1b\\hi\r");
        assert_eq!(candidate, Some("hi".to_string()));
    }

    #[test]
    fn normalize_prompt_title_strips_leading_image_refs() {
        // Drag-and-drop pastes the screenshot path quoted, spaces included.
        assert_eq!(
            normalize_prompt_title(
                "'/var/folders/cf/T/Screenshot 2026-06-12 at 15.11.52.png' fix the header"
            ),
            Some("fix the header".to_string())
        );
        // Bare path and URL tokens, multiple images.
        assert_eq!(
            normalize_prompt_title(
                "~/Desktop/shot.PNG https://example.com/a.png?raw=1 compare these"
            ),
            Some("compare these".to_string())
        );
        // Image only → no usable title; the next prompt gets to title.
        assert_eq!(normalize_prompt_title("'/tmp/shot.png'"), None);
        assert_eq!(normalize_prompt_title("/tmp/shot.png"), None);
        // Non-image paths and mid-prompt images are left alone.
        assert_eq!(
            normalize_prompt_title("/tmp/build.log is huge"),
            Some("/tmp/build.log is huge".to_string())
        );
        assert_eq!(
            normalize_prompt_title("look at /tmp/shot.png closely"),
            Some("look at /tmp/shot.png closely".to_string())
        );
    }

    #[test]
    fn normalize_prompt_title_skips_slash_commands() {
        // Slash commands (/resume in a fresh session especially) must not
        // become the title; the next real prompt titles the session.
        assert_eq!(normalize_prompt_title("/resume"), None);
        assert_eq!(normalize_prompt_title("  /clear  "), None);
        assert_eq!(normalize_prompt_title("/model opus"), None);
        assert_eq!(
            normalize_prompt_title("/code-review --fix the branch"),
            None
        );
        assert_eq!(normalize_prompt_title("/user:deep_research topic"), None);
        // Prompt glyphs are stripped before the check.
        assert_eq!(normalize_prompt_title("> /resume"), None);
        // Multi-segment or dotted leading tokens are paths, not commands.
        assert_eq!(
            normalize_prompt_title("/usr/bin/env is missing"),
            Some("/usr/bin/env is missing".to_string())
        );
        assert_eq!(
            normalize_prompt_title("/tmp/build.log is huge"),
            Some("/tmp/build.log is huge".to_string())
        );
        // A slash mid-prompt is untouched.
        assert_eq!(
            normalize_prompt_title("what does /resume do"),
            Some("what does /resume do".to_string())
        );
    }

    #[test]
    fn apply_manifest_auto_title_titles_first_prompt_only() {
        let session_id = unique_session_id("auto-title");
        let manifest = manifest_for_auto_title(&session_id, "claude", "claude");
        save_manifest(&manifest).unwrap();

        assert!(apply_manifest_auto_title(&session_id, "fix the bug"));
        let updated = load_manifest(&session_id).unwrap();
        assert_eq!(updated.session.label, "fix the bug");
        assert!(!updated.session.custom_title);

        // A second prompt no longer changes the label.
        assert!(apply_manifest_auto_title(&session_id, "another prompt"));
        let updated = load_manifest(&session_id).unwrap();
        assert_eq!(updated.session.label, "fix the bug");

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn apply_manifest_auto_title_tolerates_launch_decorated_commands() {
        // spawnSession decorates the real command (minted --session-id,
        // pi --session-dir) while the label stays the display command, so
        // label != command must not read as "already titled".
        let session_id = unique_session_id("auto-title-minted");
        let manifest = manifest_for_auto_title(
            &session_id,
            "claude --dangerously-skip-permissions --session-id 'd8b453d8-7271-4317-a562-2de769187aa3'",
            "claude --dangerously-skip-permissions",
        );
        save_manifest(&manifest).unwrap();

        assert!(apply_manifest_auto_title(&session_id, "this is a test"));
        let updated = load_manifest(&session_id).unwrap();
        assert_eq!(updated.session.label, "this is a test");
        assert!(!updated.session.custom_title);

        // Once titled, the label is no longer a prefix of the command, so
        // later prompts leave it alone.
        assert!(apply_manifest_auto_title(&session_id, "another prompt"));
        let updated = load_manifest(&session_id).unwrap();
        assert_eq!(updated.session.label, "this is a test");

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn apply_manifest_auto_title_respects_custom_titles() {
        let session_id = unique_session_id("auto-title-custom");
        let mut manifest = manifest_for_auto_title(&session_id, "claude", "my name");
        manifest.session.custom_title = true;
        save_manifest(&manifest).unwrap();

        assert!(apply_manifest_auto_title(&session_id, "fix the bug"));
        let updated = load_manifest(&session_id).unwrap();
        assert_eq!(updated.session.label, "my name");

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn apply_manifest_auto_title_marks_blank_terminals_custom() {
        let session_id = unique_session_id("auto-title-blank");
        let manifest = manifest_for_auto_title(&session_id, "", "~/Dev/unpeel");
        save_manifest(&manifest).unwrap();

        assert!(apply_manifest_auto_title(&session_id, "ls -la"));
        let updated = load_manifest(&session_id).unwrap();
        assert_eq!(updated.session.label, "ls -la");
        assert!(updated.session.custom_title);

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn apply_manifest_auto_title_respects_off_mode() {
        let session_id = unique_session_id("auto-title-off");
        let manifest = manifest_for_auto_title(&session_id, "claude", "claude");
        save_manifest(&manifest).unwrap();

        SESSION_TITLE_MODE_FOR_TEST.with(|cell| cell.set(crate::state::SessionTitleMode::Off));
        assert!(!apply_manifest_auto_title(&session_id, "fix the bug"));
        assert_eq!(load_manifest(&session_id).unwrap().session.label, "claude");

        // Flipping the mode back resumes titling on the next prompt.
        SESSION_TITLE_MODE_FOR_TEST.with(|cell| cell.set(crate::state::SessionTitleMode::Agent));
        assert!(apply_manifest_auto_title(&session_id, "fix the bug"));
        assert_eq!(
            load_manifest(&session_id).unwrap().session.label,
            "fix the bug"
        );

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn osc_title_scanner_parses_bel_and_st_titles_across_chunks() {
        let mut scanner = OscTitleScanner::new();
        assert_eq!(
            scanner.scan(b"before\x1b]0;Fixing auth bug\x07after"),
            Some("Fixing auth bug".to_string())
        );
        // Steady-state repaints re-asserting the same title cost nothing.
        assert_eq!(scanner.scan(b"\x1b]0;Fixing auth bug\x07"), None);
        // ST terminator, code 2, split across chunk boundaries.
        assert_eq!(scanner.scan(b"\x1b]2;Wri"), None);
        assert_eq!(
            scanner.scan(b"ting tests\x1b\\"),
            Some("Writing tests".to_string())
        );
    }

    #[test]
    fn osc_title_scanner_strips_claude_activity_prefix() {
        let mut scanner = OscTitleScanner::new();
        assert_eq!(
            scanner.scan("\x1b]0;⠂ Fixing auth bug\x07".as_bytes()),
            Some("Fixing auth bug".to_string())
        );
        // Normalize before deduplication so a repaint without the marker is
        // still recognized as the same title.
        assert_eq!(scanner.scan(b"\x1b]0;Fixing auth bug\x07"), None);
        // Do not eat a leading glyph when it is genuinely part of the title.
        assert_eq!(
            scanner.scan("\x1b]0;⠂Braille notes\x07".as_bytes()),
            Some("⠂Braille notes".to_string())
        );
    }

    #[test]
    fn osc_title_scanner_ignores_non_title_oscs_and_oversized_payloads() {
        let mut scanner = OscTitleScanner::new();
        // Hyperlinks (OSC 8), clipboard (OSC 52), icon-only (OSC 1) are not
        // titles, and a later real title still parses.
        assert_eq!(
            scanner.scan(b"\x1b]8;;https://example.com\x1b\\\x1b]52;c;aGk=\x07\x1b]1;icon\x07"),
            None
        );
        let mut oversized = b"\x1b]0;".to_vec();
        oversized.extend(std::iter::repeat_n(b'x', OSC_TITLE_MAX_PAYLOAD_BYTES + 1));
        oversized.extend_from_slice(b"\x07");
        assert_eq!(scanner.scan(&oversized), None);
        assert_eq!(
            scanner.scan(b"\x1b]0;Real title\x07"),
            Some("Real title".to_string())
        );
        // Control characters are stripped, whitespace trimmed.
        assert_eq!(
            scanner.scan(b"\x1b]2; padded\ttitle \x07"),
            Some("paddedtitle".to_string())
        );
    }

    #[test]
    fn apply_agent_terminal_title_gates_on_mode_runtime_and_rename() {
        let session_id = unique_session_id("agent-title");
        let mut manifest = manifest_for_auto_title(&session_id, "claude", "claude");
        manifest.runtime = Some(HostedSessionRuntime {
            current_observation: Some(observed_runtime("claude", 51)),
        });
        save_manifest(&manifest).unwrap();

        // First-prompt mode ignores agent titles.
        SESSION_TITLE_MODE_FOR_TEST
            .with(|cell| cell.set(crate::state::SessionTitleMode::FirstPrompt));
        assert!(!apply_agent_terminal_title(&session_id, "Fixing auth bug"));
        assert_eq!(load_manifest(&session_id).unwrap().session.label, "claude");

        SESSION_TITLE_MODE_FOR_TEST.with(|cell| cell.set(crate::state::SessionTitleMode::Agent));
        assert!(!apply_agent_terminal_title(&session_id, "Fixing auth bug"));
        let updated = load_manifest(&session_id).unwrap();
        assert_eq!(updated.session.label, "Fixing auth bug");
        assert!(!updated.session.custom_title);

        // Unlike the one-shot prompt title, later titles keep following.
        assert!(!apply_agent_terminal_title(&session_id, "Writing tests"));
        assert_eq!(
            load_manifest(&session_id).unwrap().session.label,
            "Writing tests"
        );

        // A user rename permanently wins.
        let _ = update_manifest_session(&session_id, |manifest| {
            manifest.session.label = "my task".into();
            manifest.session.custom_title = true;
        });
        assert!(apply_agent_terminal_title(&session_id, "Another title"));
        assert_eq!(load_manifest(&session_id).unwrap().session.label, "my task");

        SESSION_TITLE_MODE_FOR_TEST.with(|cell| cell.set(crate::state::SessionTitleMode::Agent));
        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn apply_agent_terminal_title_requires_semantic_runtime() {
        let session_id = unique_session_id("agent-title-shell");
        // A shell or non-semantic runtime (pi) in the foreground: cwd spam
        // and static titles must never retitle the row.
        let mut manifest = manifest_for_auto_title(&session_id, "pi", "pi");
        manifest.runtime = Some(HostedSessionRuntime {
            current_observation: Some(observed_runtime("pi", 51)),
        });
        save_manifest(&manifest).unwrap();

        SESSION_TITLE_MODE_FOR_TEST.with(|cell| cell.set(crate::state::SessionTitleMode::Agent));
        assert!(!apply_agent_terminal_title(&session_id, "~/Dev/unpeel"));
        assert_eq!(load_manifest(&session_id).unwrap().session.label, "pi");

        // No observed runtime at all: same refusal.
        let _ = update_manifest_session(&session_id, |manifest| {
            manifest.runtime = None;
        });
        assert!(!apply_agent_terminal_title(&session_id, "~/Dev/unpeel"));
        assert_eq!(load_manifest(&session_id).unwrap().session.label, "pi");

        SESSION_TITLE_MODE_FOR_TEST.with(|cell| cell.set(crate::state::SessionTitleMode::Agent));
        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn manifest_last_heartbeat_falls_back_to_updated_at_for_older_manifests() {
        let manifest = manifest_with_times(1234, 0);
        assert_eq!(manifest_last_heartbeat_at(&manifest), 1234);
    }

    #[test]
    fn manifest_heartbeat_staleness_uses_heartbeat_when_present() {
        let manifest = manifest_with_times(1_000, 5_000);
        assert!(!manifest_heartbeat_is_stale(&manifest, 24_000));
        assert!(manifest_heartbeat_is_stale(&manifest, 186_000));
    }

    #[test]
    fn refresh_manifest_health_keeps_running_manifest_without_pid_fail_closed() {
        let session_id = unique_session_id("health-missing-pid");
        let mut manifest = manifest_with_times(1_000, 1_000);
        manifest.session.id = session_id.clone();
        manifest.pid = None;
        save_manifest(&manifest).unwrap();

        let refreshed = refresh_manifest_health_from_manifest(manifest);
        assert_eq!(refreshed.state, HostedSessionState::Running);
        assert_eq!(refreshed.exit_code, None);

        let persisted = load_manifest(&session_id).unwrap();
        assert_eq!(persisted.state, HostedSessionState::Running);
        assert_eq!(persisted.exit_code, None);

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn batched_output_writer_flushes_when_batch_hits_limit() {
        let writer = RecordingWriter::default();
        let recorded = writer.clone();
        let (tx, rx) = mpsc::channel();

        let worker =
            thread::spawn(move || run_batched_output_writer(writer, rx, Duration::from_secs(1), 5));

        tx.send(b"abc".to_vec()).unwrap();
        tx.send(b"de".to_vec()).unwrap();
        drop(tx);

        worker.join().unwrap().unwrap();

        assert_eq!(recorded.snapshots(), vec![b"abcde".to_vec()]);
    }

    #[test]
    fn batched_output_writer_flushes_pending_bytes_after_timeout() {
        let writer = RecordingWriter::default();
        let recorded = writer.clone();
        let (tx, rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            run_batched_output_writer(writer, rx, Duration::from_millis(10), 1024)
        });

        tx.send(b"ab".to_vec()).unwrap();
        thread::sleep(Duration::from_millis(30));
        tx.send(b"cd".to_vec()).unwrap();
        drop(tx);

        worker.join().unwrap().unwrap();

        assert_eq!(recorded.snapshots(), vec![b"ab".to_vec(), b"cd".to_vec()]);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn retained_writer_bounds_allocated_blocks_and_rebases_stale_reads() {
        use std::os::unix::fs::MetadataExt;

        let session_id = unique_session_id("retained-output");
        let path = output_path(&session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut writer =
            RetainedOutputWriter::new(file, path.clone(), 0, 16 * 1024, 8 * 1024).unwrap();

        let mut expected = Vec::new();
        for index in 0..4_000 {
            expected.extend_from_slice(format!("\x1b[31mline {index:04} 🙂\x1b[0m\r\n").as_bytes());
        }
        writer.write_all(&expected).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let metadata = fs::metadata(&path).unwrap();
        let retained_from = output_retained_from(&session_id);
        assert_eq!(metadata.len(), expected.len() as u64);
        assert!(retained_from > 0);
        let allocation_bound = 16 * 1024 + 8 * 1024 + metadata.blksize() * 2;
        assert!(
            metadata.blocks() * 512 <= allocation_bound,
            "allocated {} bytes, expected at most {allocation_bound}",
            metadata.blocks() * 512
        );

        let retained = fs::read(&path).unwrap();
        assert_eq!(
            &retained[retained_from as usize..],
            &expected[retained_from as usize..]
        );
        let chunk = read_output_chunk(&session_id, Some(0), Some(4096), Some(4096)).unwrap();
        let actual_start = chunk.next_offset - chunk.data.len() as u64;
        assert!(actual_start >= retained_from);
        assert!(!chunk.data.contains(&0), "sparse prefix leaked into replay");
        assert_eq!(
            chunk.data,
            expected[actual_start as usize..chunk.next_offset as usize]
        );

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exited_journal_compaction_is_bounded_exact_and_idempotent() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("output.bin");
        let retention_path = temp.path().join(super::OUTPUT_RETENTION_FILE);
        let mut expected = Vec::new();
        for index in 0..8_000 {
            expected.extend_from_slice(format!("line {index:04} 🙂\r\n").as_bytes());
        }
        fs::write(&path, &expected).unwrap();
        let evicted = compact_output_journal_path(&path, &retention_path, 16 * 1024)
            .unwrap()
            .expect("legacy journal should compact");
        assert!(evicted > 0);

        let metadata = fs::metadata(&path).unwrap();
        let retained_from = super::read_output_retention_path(&retention_path);
        assert_eq!(metadata.len(), expected.len() as u64);
        assert!(retained_from > 0);
        let sparse = fs::read(&path).unwrap();
        assert_eq!(
            &sparse[retained_from as usize..],
            &expected[retained_from as usize..]
        );
        assert!(metadata.blocks() * 512 <= 16 * 1024 + 64 * 1024 + metadata.blksize() * 2);
        assert_eq!(
            compact_output_journal_path(&path, &retention_path, 16 * 1024).unwrap(),
            None,
            "a second maintenance pass must not churn the journal"
        );
    }

    #[test]
    fn retention_floor_handles_utf8_vt_intermediates_cancellation_and_unclosed_strings() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("output.bin");

        let intermediate = b"prefix\r\n\x1b(Bafter";
        fs::write(&path, intermediate).unwrap();
        let escape = intermediate.iter().position(|byte| *byte == 0x1b).unwrap() as u64;
        let mut file = File::open(&path).unwrap();
        assert_eq!(
            safe_output_retention_boundary(&mut file, 0, escape + 2).unwrap(),
            escape,
            "floor must not split ESC ( B after the intermediate"
        );

        let cancelled = b"prefix\r\n\x1b[1;\x1b[31mred";
        fs::write(&path, cancelled).unwrap();
        let escapes = cancelled
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == 0x1b).then_some(index as u64))
            .collect::<Vec<_>>();
        let mut file = File::open(&path).unwrap();
        assert_eq!(
            safe_output_retention_boundary(&mut file, 0, escapes[1] + 3).unwrap(),
            escapes[1],
            "a fresh ESC must cancel the incomplete CSI for alignment"
        );

        let emoji = "prefix\r\n🙂after".as_bytes();
        fs::write(&path, emoji).unwrap();
        let emoji_start = emoji
            .windows("🙂".len())
            .position(|window| window == "🙂".as_bytes())
            .unwrap() as u64;
        let mut file = File::open(&path).unwrap();
        assert_eq!(
            safe_output_retention_boundary(&mut file, 0, emoji_start + 2).unwrap(),
            emoji_start
        );

        let mut unclosed = b"\x1b]never-terminated=".to_vec();
        unclosed.extend(std::iter::repeat_n(
            b'x',
            super::SESSION_OUTPUT_JOURNAL_CONTROL_SLACK_BYTES as usize + 4096,
        ));
        fs::write(&path, &unclosed).unwrap();
        let desired = unclosed.len() as u64 - 1;
        let mut file = File::open(&path).unwrap();
        let forced = safe_output_retention_boundary(&mut file, 0, desired).unwrap();
        assert!(forced >= desired.saturating_sub(3));
        assert!(forced > 0, "an unclosed OSC must not defeat the hard cap");
    }

    #[test]
    fn replacement_journal_starts_after_prior_high_water_without_cursor_alias() {
        let session_id = unique_session_id("output-generation");
        let path = output_path(&session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"old generation").unwrap();
        let prior_end = fs::metadata(&path).unwrap().len();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut writer =
            RetainedOutputWriter::new(file, path.clone(), prior_end + 1, 16 * 1024, 8 * 1024)
                .unwrap();
        writer.write_all(b"new generation").unwrap();
        writer.flush().unwrap();
        drop(writer);

        let floor = output_retained_from(&session_id);
        assert_eq!(floor, prior_end + 1);
        let chunk =
            read_output_chunk(&session_id, Some(prior_end), Some(1024), Some(1024)).unwrap();
        assert_eq!(chunk.next_offset - chunk.data.len() as u64, floor);
        assert_eq!(chunk.data, b"new generation");

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn output_stream_frames_round_trip() {
        let chunk = SessionOutputChunk {
            data: b"hello".to_vec(),
            next_offset: 42,
            exited: false,
            exists: true,
        };
        let mut bytes = Vec::new();
        write_output_stream_frame(&mut bytes, &chunk).unwrap();

        let event = read_output_stream_frame(&mut Cursor::new(bytes)).unwrap();
        match event {
            OutputStreamRead::Chunk(decoded) => {
                assert_eq!(decoded.data, b"hello");
                assert_eq!(decoded.next_offset, 42);
                assert!(!decoded.exited);
                assert!(decoded.exists);
            }
            _ => panic!("expected chunk frame"),
        }
    }

    #[test]
    fn input_stream_frame_decodes_payload() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(5u32).to_be_bytes());
        bytes.extend_from_slice(b"hello");

        let decoded = read_input_stream_frame(&mut Cursor::new(bytes))
            .unwrap()
            .unwrap();
        assert_eq!(decoded, b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn accepted_input_stream_waits_across_ack_to_first_frame_gap() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        // Reproduce BSD/macOS accept behavior on every Unix: the accepted
        // endpoint starts nonblocking even though its handler protocol must
        // wait for complete commands and frames.
        server.set_nonblocking(true).unwrap();

        let server_thread = thread::spawn(move || -> Result<Vec<u8>, String> {
            configure_session_client(&server)?;
            let mut reader = BufReader::new(
                server
                    .try_clone()
                    .map_err(|e| format!("clone test stream: {e}"))?,
            );
            let mut command = String::new();
            reader
                .read_line(&mut command)
                .map_err(|e| format!("read test command: {e}"))?;
            if command != "{\"type\":\"stream_input\"}\n" {
                return Err(format!("unexpected command: {command:?}"));
            }
            server
                .write_all(&[SESSION_INPUT_STREAM_ACK])
                .map_err(|e| format!("write test acknowledgement: {e}"))?;
            read_input_stream_frame(&mut reader)?.ok_or("input stream closed".into())
        });

        client.write_all(b"{\"type\":\"stream_input\"}\n").unwrap();
        let mut ack = [u8::MAX; 1];
        client.read_exact(&mut ack).unwrap();
        assert_eq!(ack, [SESSION_INPUT_STREAM_ACK]);

        // This is the exact window that used to close the nonblocking host
        // socket. A human keystroke can arrive arbitrarily long after ACK.
        thread::sleep(Duration::from_millis(25));
        client.write_all(&(5u32).to_be_bytes()).unwrap();
        client.write_all(b"hello").unwrap();

        assert_eq!(server_thread.join().unwrap().unwrap(), b"hello");
    }

    #[test]
    fn batched_output_stream_forwarder_merges_small_contiguous_chunks() {
        let (tx, rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            let mut writer = Cursor::new(Vec::<u8>::new());
            run_batched_output_stream_forwarder(
                &mut writer,
                rx,
                Duration::from_millis(10),
                16,
                &AtomicUsize::new(0),
            )
            .unwrap();
            writer.into_inner()
        });

        tx.send(SessionOutputChunk {
            data: b"abc".to_vec(),
            next_offset: 3,
            exited: false,
            exists: true,
        })
        .unwrap();
        tx.send(SessionOutputChunk {
            data: b"def".to_vec(),
            next_offset: 6,
            exited: false,
            exists: true,
        })
        .unwrap();
        drop(tx);

        let bytes = worker.join().unwrap();
        let mut reader = Cursor::new(bytes);

        match read_output_stream_frame(&mut reader).unwrap() {
            OutputStreamRead::Chunk(chunk) => {
                assert_eq!(chunk.data, b"abcdef");
                assert_eq!(chunk.next_offset, 6);
                assert!(!chunk.exited);
                assert!(chunk.exists);
            }
            _ => panic!("expected merged chunk frame"),
        }

        assert!(matches!(
            read_output_stream_frame(&mut reader).unwrap(),
            OutputStreamRead::Closed
        ));
    }

    #[test]
    fn batched_output_stream_forwarder_flushes_pending_chunk_after_timeout() {
        let (tx, rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            let mut writer = Cursor::new(Vec::<u8>::new());
            run_batched_output_stream_forwarder(
                &mut writer,
                rx,
                Duration::from_millis(10),
                16,
                &AtomicUsize::new(0),
            )
            .unwrap();
            writer.into_inner()
        });

        tx.send(SessionOutputChunk {
            data: b"ab".to_vec(),
            next_offset: 2,
            exited: false,
            exists: true,
        })
        .unwrap();
        thread::sleep(Duration::from_millis(30));
        tx.send(SessionOutputChunk {
            data: b"cd".to_vec(),
            next_offset: 4,
            exited: false,
            exists: true,
        })
        .unwrap();
        drop(tx);

        let bytes = worker.join().unwrap();
        let mut reader = Cursor::new(bytes);

        match read_output_stream_frame(&mut reader).unwrap() {
            OutputStreamRead::Chunk(chunk) => {
                assert_eq!(chunk.data, b"ab");
                assert_eq!(chunk.next_offset, 2);
            }
            _ => panic!("expected first chunk frame"),
        }

        match read_output_stream_frame(&mut reader).unwrap() {
            OutputStreamRead::Chunk(chunk) => {
                assert_eq!(chunk.data, b"cd");
                assert_eq!(chunk.next_offset, 4);
            }
            _ => panic!("expected second chunk frame"),
        }
    }

    #[test]
    fn batched_output_stream_forwarder_keeps_exit_frame_separate() {
        let (tx, rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            let mut writer = Cursor::new(Vec::<u8>::new());
            run_batched_output_stream_forwarder(
                &mut writer,
                rx,
                Duration::from_millis(10),
                16,
                &AtomicUsize::new(0),
            )
            .unwrap();
            writer.into_inner()
        });

        tx.send(SessionOutputChunk {
            data: b"busy".to_vec(),
            next_offset: 4,
            exited: false,
            exists: true,
        })
        .unwrap();
        tx.send(SessionOutputChunk {
            data: Vec::new(),
            next_offset: 4,
            exited: true,
            exists: true,
        })
        .unwrap();
        drop(tx);

        let bytes = worker.join().unwrap();
        let mut reader = Cursor::new(bytes);

        match read_output_stream_frame(&mut reader).unwrap() {
            OutputStreamRead::Chunk(chunk) => {
                assert_eq!(chunk.data, b"busy");
                assert_eq!(chunk.next_offset, 4);
                assert!(!chunk.exited);
            }
            _ => panic!("expected payload chunk frame"),
        }

        match read_output_stream_frame(&mut reader).unwrap() {
            OutputStreamRead::Chunk(chunk) => {
                assert!(chunk.data.is_empty());
                assert_eq!(chunk.next_offset, 4);
                assert!(chunk.exited);
            }
            _ => panic!("expected exit chunk frame"),
        }
    }

    #[test]
    fn output_broadcaster_replays_recent_backlog_to_new_subscriber() {
        let mut broadcaster = OutputBroadcaster::default();
        broadcaster.broadcast_chunk(b"abc");
        broadcaster.broadcast_chunk(b"def");

        let (tx, rx) = mpsc::channel();
        let (_subscriber_id, _buffered) = broadcaster.subscribe(2, tx, false).unwrap();

        let first = rx.recv().unwrap();
        let second = rx.recv().unwrap();
        assert_eq!(first.data, b"c".to_vec());
        assert_eq!(first.next_offset, 3);
        assert_eq!(second.data, b"def".to_vec());
        assert_eq!(second.next_offset, 6);
    }

    // Leak-regression: a subscriber that never drains its channel must be
    // dropped once its buffered backlog exceeds the cap, so a stalled attach
    // client can't grow host memory without bound.
    #[test]
    fn output_broadcaster_drops_non_draining_subscriber_at_buffer_cap() {
        let mut broadcaster = OutputBroadcaster::default();
        // Hold the receiver but never read it, so nothing decrements `buffered`.
        let (tx, _rx) = mpsc::channel();
        let (_id, buffered) = broadcaster.subscribe(0, tx, false).unwrap();
        assert_eq!(broadcaster.subscribers.len(), 1);

        let chunk = vec![b'x'; super::SESSION_OUTPUT_READ_BUFFER_BYTES];
        let cap = super::SESSION_OUTPUT_STREAM_SUBSCRIBER_MAX_BUFFERED_BYTES;
        // Broadcast well past the cap; the subscriber must be reaped.
        let rounds = (cap / chunk.len()) + 8;
        for _ in 0..rounds {
            broadcaster.broadcast_chunk(&chunk);
        }

        assert_eq!(
            broadcaster.subscribers.len(),
            0,
            "stalled subscriber should have been dropped"
        );
        // Buffered never exceeds the cap by more than one chunk (the enqueue that
        // tips it over still lands, the next one is refused).
        assert!(
            buffered.load(Ordering::Relaxed) <= cap + chunk.len(),
            "buffered {} exceeded cap {} + one chunk",
            buffered.load(Ordering::Relaxed),
            cap
        );
    }

    // Leak-regression: the recent-backlog ring is bounded regardless of how much
    // output flows through it.
    #[test]
    fn output_broadcaster_recent_backlog_stays_within_cap() {
        let mut broadcaster = OutputBroadcaster::default();
        let chunk = vec![b'y'; 64 * 1024];
        for _ in 0..64 {
            broadcaster.broadcast_chunk(&chunk);
        }
        let recent_sum: usize = broadcaster.recent.iter().map(|c| c.data.len()).sum();
        assert!(recent_sum <= super::SESSION_OUTPUT_STREAM_RECENT_BYTES);
        assert_eq!(recent_sum, broadcaster.recent_bytes);
    }

    #[test]
    fn output_broadcaster_releases_its_recent_ring_when_idle_and_unwatched() {
        let mut broadcaster = OutputBroadcaster::default();
        for _ in 0..8 {
            broadcaster.broadcast_chunk(&[b'z'; 4096]);
        }
        assert!(broadcaster.recent_bytes > 0);
        assert!(!broadcaster.has_subscribers());
        let next = broadcaster.next_offset;
        broadcaster.release_recent();
        assert_eq!(broadcaster.recent_bytes, 0);
        assert_eq!(broadcaster.recent.capacity(), 0);
        // Offsets stay monotonic and a late cursor is refused, never aliased.
        assert_eq!(broadcaster.next_offset, next);
        let (tx, _rx) = mpsc::channel();
        assert!(broadcaster.subscribe(next - 1, tx, false).is_none());
        let (tx, _rx) = mpsc::channel();
        assert!(broadcaster.subscribe(next, tx, false).is_some());
        assert!(broadcaster.has_subscribers());
        broadcaster.remove_subscriber(1);
        assert!(!broadcaster.has_subscribers());
        assert_eq!(broadcaster.subscribers.capacity(), 0);
    }

    #[test]
    fn output_broadcaster_refuses_a_cursor_older_than_its_recent_backlog() {
        let mut broadcaster = OutputBroadcaster::default();
        let chunk = vec![b'y'; 64 * 1024];
        for _ in 0..64 {
            broadcaster.broadcast_chunk(&chunk);
        }

        let oldest_available = broadcaster.recent.front().unwrap().start_offset;
        assert!(oldest_available > 0, "test must evict the initial bytes");

        let (stale_tx, _stale_rx) = mpsc::channel();
        assert!(broadcaster
            .subscribe(oldest_available - 1, stale_tx, false)
            .is_none());

        let (current_tx, _current_rx) = mpsc::channel();
        assert!(broadcaster
            .subscribe(broadcaster.next_offset, current_tx, false)
            .is_some());
    }

    // Leak-regression: the forwarder releases each chunk's reserved budget as it
    // drains, so `buffered` returns to zero once the stream is fully consumed —
    // an accounting drift here would slowly wedge the subscriber cap.
    #[test]
    fn output_stream_forwarder_releases_buffered_budget_to_zero() {
        let mut broadcaster = OutputBroadcaster::default();
        let (tx, rx) = mpsc::channel();
        let (id, buffered) = broadcaster.subscribe(0, tx, false).unwrap();

        let forwarder_buffered = buffered.clone();
        let worker = thread::spawn(move || {
            let mut writer = Cursor::new(Vec::<u8>::new());
            run_batched_output_stream_forwarder(
                &mut writer,
                rx,
                Duration::from_millis(5),
                16,
                &forwarder_buffered,
            )
            .unwrap();
        });

        for _ in 0..32 {
            broadcaster.broadcast_chunk(&vec![b'z'; 4096]);
        }
        // Drop the subscriber's sender so the forwarder sees a disconnect and
        // exits after draining everything.
        broadcaster.remove_subscriber(id);
        worker.join().unwrap();

        assert_eq!(
            buffered.load(Ordering::Relaxed),
            0,
            "forwarder should have released all reserved budget"
        );
    }

    // Leak-regression: the partially-typed auto-title buffer never balloons past
    // its cap, even for a huge paste with no trailing newline (F2), and it stops
    // growing once a line is submitted.
    #[test]
    fn app_title_marker_retitles_until_user_rename() {
        let session_id = unique_session_id("app-title");
        let manifest = manifest_for_auto_title(
            &session_id,
            "/opt/bin/unpeel-markdown",
            "/opt/bin/unpeel-markdown",
        );
        fs::create_dir_all(super::session_dir(&session_id)).unwrap();
        save_manifest(&manifest).unwrap();

        // First marker application replaces the command-derived label.
        assert!(!super::apply_app_title(&session_id, "hero.md"));
        assert_eq!(load_manifest(&session_id).unwrap().session.label, "hero.md");

        // Unlike the one-shot prompt auto-title, a later marker change
        // retitles again (picker → another document).
        assert!(!super::apply_app_title(&session_id, "notes.md"));
        assert_eq!(
            load_manifest(&session_id).unwrap().session.label,
            "notes.md"
        );

        // A user rename is permanent: applications stop and report settled.
        let _ = super::update_manifest_session(&session_id, |manifest| {
            manifest.session.label = "My session".into();
            manifest.session.custom_title = true;
        });
        assert!(super::apply_app_title(&session_id, "other.md"));
        assert_eq!(
            load_manifest(&session_id).unwrap().session.label,
            "My session"
        );

        // Marker parsing: bounded, single-line, text field only.
        let dir = super::session_dir(&session_id);
        fs::write(
            dir.join(super::APP_TITLE_MARKER),
            br#"{"text":"a\nb  ","updated_at":1}"#,
        )
        .unwrap();
        assert_eq!(super::read_app_title_marker(&dir).as_deref(), Some("a b"));
        fs::write(dir.join(super::APP_TITLE_MARKER), br#"{"text":"   "}"#).unwrap();
        assert_eq!(super::read_app_title_marker(&dir), None);

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn app_context_marker_reads_bounded_object_only() {
        let session_id = unique_session_id("app-context");
        let dir = super::session_dir(&session_id);
        fs::create_dir_all(&dir).unwrap();

        assert_eq!(super::read_app_context_marker(&session_id), None);

        fs::write(
            dir.join(super::APP_CONTEXT_MARKER),
            br#"{"app":"unpeel.app.design","context":{"file":"hero.html","lines":[12,32]},"updated_at":1}"#,
        )
        .unwrap();
        let value = super::read_app_context_marker(&session_id).unwrap();
        assert_eq!(value["context"]["file"], "hero.html");
        assert_eq!(value["context"]["lines"][1], 32);

        // Non-object payloads and oversized markers are ignored, never
        // surfaced partially.
        fs::write(dir.join(super::APP_CONTEXT_MARKER), br#""just a string""#).unwrap();
        assert_eq!(super::read_app_context_marker(&session_id), None);
        fs::write(
            dir.join(super::APP_CONTEXT_MARKER),
            vec![b' '; (super::APP_CONTEXT_CAP_BYTES + 1) as usize],
        )
        .unwrap();
        assert_eq!(super::read_app_context_marker(&session_id), None);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn auto_title_buffer_is_bounded() {
        let mut buffer = String::new();
        // A giant paste with no newline: buffer must stay capped.
        let huge = "a".repeat(super::AUTO_TITLE_BUFFER_MAX_CHARS * 4);
        let candidate = extract_submitted_prompt(&mut buffer, &huge);
        assert!(candidate.is_none());
        assert!(buffer.len() <= super::AUTO_TITLE_BUFFER_MAX_CHARS);

        // A newline clears the buffer entirely.
        let _ = extract_submitted_prompt(&mut buffer, "\r");
        assert!(buffer.is_empty());
    }

    #[test]
    fn read_output_chunk_aligns_initial_tail_replay_to_escape_boundary() {
        let session_id = unique_session_id("tail-replay-escape");
        let data = b"before\r\nprefix \x1b[31mRED\x1b[0m tail\r\n".to_vec();
        let escape_index = data.iter().position(|byte| *byte == 0x1b).unwrap() as u64;
        let requested_start = escape_index + 2;
        let tail_bytes = (data.len() as u64).saturating_sub(requested_start) as usize;
        let path = output_path(&session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &data).unwrap();

        let chunk =
            read_output_chunk(&session_id, None, Some(tail_bytes), Some(tail_bytes)).unwrap();

        assert_eq!(chunk.data, data[escape_index as usize..]);
        assert_eq!(chunk.next_offset, data.len() as u64);

        cleanup_session_artifacts(&session_id).unwrap();
    }

    #[test]
    fn read_output_chunk_aligns_initial_tail_replay_to_utf8_boundary() {
        let session_id = unique_session_id("tail-replay-utf8");
        let data = "before\r\n🙂 emoji tail\r\n".as_bytes().to_vec();
        let emoji = "🙂".as_bytes();
        let emoji_index = data
            .windows(emoji.len())
            .position(|window| window == emoji)
            .unwrap() as u64;
        let requested_start = emoji_index + 1;
        let tail_bytes = (data.len() as u64).saturating_sub(requested_start) as usize;
        let path = output_path(&session_id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &data).unwrap();

        let chunk =
            read_output_chunk(&session_id, None, Some(tail_bytes), Some(tail_bytes)).unwrap();

        assert_eq!(chunk.data, data["before\r\n".len()..]);
        assert_eq!(
            String::from_utf8(chunk.data.clone()).unwrap(),
            "🙂 emoji tail\r\n"
        );
        assert_eq!(chunk.next_offset, data.len() as u64);

        cleanup_session_artifacts(&session_id).unwrap();
    }

    fn scan_all(scanner: &mut OutputQueryScanner, input: &[u8]) -> (Vec<u8>, usize) {
        let (out, queries) = scanner.scan(input, false);
        (out, queries.len())
    }

    fn scan_probes(
        scanner: &mut OutputQueryScanner,
        input: &[u8],
    ) -> (Vec<u8>, Vec<HostAnsweredQuery>) {
        scanner.scan(input, true)
    }

    #[test]
    fn query_scanner_intercepts_probes_only_when_asked() {
        // The exact startup probes muse 0.1.0 emits (it exits ~4s after
        // launch when they go unanswered): CPR, kitty flags, OSC fg/bg/
        // palette queries.
        let probe = b"\x1b[6n\x1b[?u\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]4;2;?\x07after";
        let mut scanner = OutputQueryScanner::new();
        let (out, queries) = scan_probes(&mut scanner, probe);
        assert_eq!(out, b"after");
        assert_eq!(
            queries,
            vec![
                HostAnsweredQuery::CursorPosition,
                HostAnsweredQuery::KittyFlags,
                HostAnsweredQuery::OscColor { code: 10 },
                HostAnsweredQuery::OscColor { code: 11 },
                HostAnsweredQuery::OscPalette { index: 2 },
            ]
        );

        // With an answering surface attached the same probes pass through
        // untouched (only DA1 stays host-answered).
        let mut passive = OutputQueryScanner::new();
        let (out, queries) = scan_all(&mut passive, probe);
        assert_eq!(out, probe.to_vec());
        assert_eq!(queries, 0);
    }

    #[test]
    fn query_scanner_passes_non_query_osc_through_in_probe_mode() {
        // Title sets and hyperlink OSCs must never be swallowed.
        let input = b"\x1b]0;my title\x07\x1b]8;;https://x\x1b\\text";
        let mut scanner = OutputQueryScanner::new();
        let (out, queries) = scan_probes(&mut scanner, input);
        assert_eq!(out, input.to_vec());
        assert!(queries.is_empty());
    }

    #[test]
    fn query_scanner_carries_probe_split_across_chunks() {
        let mut scanner = OutputQueryScanner::new();
        let (out1, q1) = scan_probes(&mut scanner, b"x\x1b]11;");
        assert_eq!(out1, b"x");
        assert!(q1.is_empty());
        let (out2, q2) = scan_probes(&mut scanner, b"?\x07y");
        assert_eq!(out2, b"y");
        assert_eq!(q2, vec![HostAnsweredQuery::OscColor { code: 11 }]);
    }

    #[test]
    fn query_scanner_answers_and_excises_da1_forms() {
        for query in [&b"\x1b[c"[..], b"\x1b[0c", b"\x1bZ", b"\x1b[0;1c"] {
            let mut scanner = OutputQueryScanner::new();
            let mut input = b"before".to_vec();
            input.extend_from_slice(query);
            input.extend_from_slice(b"after");
            let (out, queries) = scan_all(&mut scanner, &input);
            assert_eq!(out, b"beforeafter", "query {query:?}");
            assert_eq!(queries, 1, "query {query:?}");
        }
    }

    #[test]
    fn query_scanner_passes_fish_startup_probe_untouched_except_da1() {
        // The exact byte stream fish 4.8 emits at startup (captured from a
        // hosted session): kitty-keyboard query, XTVERSION, OSC 11, alt-screen
        // toggles, two XTGETTCAP DCS queries, then the DA1 terminator.
        let probe = b"\x1b[?u\x1b[>0q\x1b]11;?\x1b\\\x1b[?1049h\x1bP+q696e646e\x1b\\\x1bP+q71756572792d6f732d6e616d65\x1b\\\x1b[?1049l\x1b[0c";
        let mut scanner = OutputQueryScanner::new();
        let (out, queries) = scan_all(&mut scanner, probe);
        assert_eq!(queries, 1);
        // Everything except the trailing `ESC [ 0 c` passes through verbatim.
        assert_eq!(out, probe[..probe.len() - 4].to_vec());
    }

    #[test]
    fn query_scanner_handles_query_split_across_chunks() {
        let mut scanner = OutputQueryScanner::new();
        let (out1, q1) = scan_all(&mut scanner, b"hello\x1b[");
        assert_eq!(out1, b"hello");
        assert_eq!(q1, 0);
        let (out2, q2) = scan_all(&mut scanner, b"0c world");
        assert_eq!(out2, b" world");
        assert_eq!(q2, 1);
    }

    #[test]
    fn query_scanner_releases_non_da1_sequences_verbatim() {
        let mut scanner = OutputQueryScanner::new();
        // DA2/DA3 requests, SGR, cursor moves, private modes: all untouched.
        let input = b"\x1b[>c\x1b[=c\x1b[31m\x1b[2J\x1b[?25hplain\x1b[10;20H";
        let (out, queries) = scan_all(&mut scanner, input);
        assert_eq!(out, input.to_vec());
        assert_eq!(queries, 0);
    }

    #[test]
    fn query_scanner_aborted_csi_then_da1() {
        let mut scanner = OutputQueryScanner::new();
        // ESC inside an unfinished CSI aborts it; the new sequence is a DA1.
        let (out, queries) = scan_all(&mut scanner, b"\x1b[12\x1b[c!");
        assert_eq!(out, b"\x1b[12!");
        assert_eq!(queries, 1);
    }

    #[test]
    fn query_scanner_take_pending_flushes_partial_sequence() {
        let mut scanner = OutputQueryScanner::new();
        let (out, queries) = scan_all(&mut scanner, b"tail\x1b[3");
        assert_eq!(out, b"tail");
        assert_eq!(queries, 0);
        assert_eq!(scanner.take_pending(), b"\x1b[3");
        // Scanner is reusable after draining.
        let (out2, q2) = scan_all(&mut scanner, b"\x1b[c");
        assert_eq!(out2, b"");
        assert_eq!(q2, 1);
    }

    #[test]
    fn query_scanner_response_matches_ghostty_primary_da() {
        // Keep the canned answer aligned with ghostty's stream_handler.zig
        // (62 = VT220 conformance, 22 = color, 52 = clipboard access).
        assert_eq!(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE, b"\x1b[?62;22;52c");
    }
}

#[cfg(unix)]
fn configure_session_client(stream: &std::os::unix::net::UnixStream) -> Result<(), String> {
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("Failed to configure session client socket: {e}"))
}
