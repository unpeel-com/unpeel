//! Transport-neutral, Controller-side view of a remote Unpeel Host.
//!
//! This module is deliberately a pure client of [`HostConnection`]. It does
//! not read local Unpeel state, install assets, or call local session verbs.
//! Bootstrap is the only unconstrained call; every later operation is bound
//! to the transport generation returned by the accepted bootstrap.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::controller_protocol::{HostProtocolDescriptor, HOST_PROTOCOL_MAJOR};
use crate::host_connection::{
    ConnectionGeneration, HostCall, HostConnection, HostConnectionError, RequestSemantics,
};

pub const MOBILE_PROTOCOL_VERSION: u16 = 1;
pub const REMOTE_OUTPUT_DEFAULT_LIMIT: usize = 200 * 1024;
pub const REMOTE_OUTPUT_MAX_LIMIT: usize = 200 * 1024;
pub const REMOTE_OUTPUT_MAX_WAIT: Duration = Duration::from_secs(25);
pub const REMOTE_TERMINAL_WRITE_MAX_BYTES: usize = 64 * 1024;
pub const REMOTE_DESKTOP_RESIZE_MIN_COLUMNS: u16 = 2;
pub const REMOTE_DESKTOP_RESIZE_MAX_COLUMNS: u16 = 300;
pub const REMOTE_DESKTOP_RESIZE_MIN_ROWS: u16 = 2;
pub const REMOTE_DESKTOP_RESIZE_MAX_ROWS: u16 = 120;
/// Controller-side sanity cap; the Host organization route trims but does not
/// bound titles, so this stays a client guard rather than a wire contract.
pub const REMOTE_SESSION_TITLE_MAX_BYTES: usize = 1024;
/// Mirrors the Host router's `/mobile/session-order` list bound.
pub const REMOTE_SESSION_ORDER_MAX_IDS: usize = 1024;
/// Mirror of the Host create resolver's `MAX_CREATE_COMMAND_BYTES`.
pub const REMOTE_CREATE_COMMAND_MAX_BYTES: usize = 64 * 1024;
/// Mirror of the Host create resolver's `MAX_CREATE_INITIAL_TEXT_BYTES`.
pub const REMOTE_CREATE_INITIAL_TEXT_MAX_BYTES: usize = 256 * 1024;
/// Mirror of the Host create resolver's `MAX_CREATE_PATH_BYTES`.
pub const REMOTE_CREATE_PATH_MAX_BYTES: usize = 16 * 1024;
/// Mirror of the Host create resolver's `MAX_CREATE_BRANCH_BYTES`.
pub const REMOTE_CREATE_BRANCH_MAX_BYTES: usize = 4 * 1024;
/// A pairing invitation carries either one short proxy URL or one sealed
/// pairing envelope. Keep it far below the general Controller body ceiling.
pub const REMOTE_PAIRING_INVITATION_MAX_BYTES: usize = 128 * 1024;

const BOOTSTRAP_CAPABILITY: &str = "host.bootstrap";
const OUTPUT_CAPABILITY: &str = "session.output.read";
const WRITE_CAPABILITY: &str = "session.input.write";
const RESIZE_DESKTOP_CAPABILITY: &str = "session.resize_desktop";
const MARK_READ_CAPABILITY: &str = "session.mark_read";
const APPROVAL_ANSWER_CAPABILITY: &str = "approval.answer";
const TITLE_SET_CAPABILITY: &str = "session.title.set";
const PIN_SET_CAPABILITY: &str = "session.pin.set";
const NOTIFY_WHEN_DONE_CAPABILITY: &str = "session.notify_when_done.set";
const PROJECT_SET_CAPABILITY: &str = "session.project.set";
const ARCHIVE_CAPABILITY: &str = "session.archive";
const RESTORE_CAPABILITY: &str = "session.restore";
const STOP_CAPABILITY: &str = "session.stop";
const REMOVE_CAPABILITY: &str = "session.remove";
const RESTART_CAPABILITY: &str = "session.restart";
const RESTART_AGENT_CAPABILITY: &str = "session.runtime.restart";
const RESUME_AGENT_CAPABILITY: &str = "session.runtime.resume";
const CREATE_CAPABILITY: &str = "session.create";
const ORDER_SET_CAPABILITY: &str = "session.order.set";
const PROJECT_ORGANIZATION_CAPABILITY: &str = "project.organization.set";
const PRESETS_CAPABILITY: &str = "settings.presets.set";
const OPENERS_CAPABILITY: &str = "settings.openers.set";
const WORKSPACE_SETTINGS_CAPABILITY: &str = "settings.workspace.set";
const APPS_INSTALL_CAPABILITY: &str = "apps.install";
const APPS_OPEN_CAPABILITY: &str = "apps.open";
const ARCHIVE_LIST_CAPABILITY: &str = "session.archive.list";
const TRANSCRIPT_MARKDOWN_CAPABILITY: &str = "session.transcript.markdown";
const METRICS_CAPABILITY: &str = "session.metrics.read";
const PAIRING_INVITATION_CAPABILITY: &str = "pairing.invitation";
const UPLOAD_CAPABILITY: &str = "artifact.upload";
pub const REMOTE_CAPABILITY_OUTPUT_READ: &str = OUTPUT_CAPABILITY;
pub const REMOTE_CAPABILITY_INPUT_WRITE: &str = WRITE_CAPABILITY;
pub const REMOTE_CAPABILITY_RESIZE_DESKTOP: &str = RESIZE_DESKTOP_CAPABILITY;
pub const REMOTE_CAPABILITY_MARK_READ: &str = MARK_READ_CAPABILITY;
pub const REMOTE_CAPABILITY_TITLE_SET: &str = TITLE_SET_CAPABILITY;
pub const REMOTE_CAPABILITY_PIN_SET: &str = PIN_SET_CAPABILITY;
pub const REMOTE_CAPABILITY_NOTIFY_WHEN_DONE_SET: &str = NOTIFY_WHEN_DONE_CAPABILITY;
pub const REMOTE_CAPABILITY_ARCHIVE: &str = ARCHIVE_CAPABILITY;
pub const REMOTE_CAPABILITY_RESTORE: &str = RESTORE_CAPABILITY;
pub const REMOTE_CAPABILITY_STOP: &str = STOP_CAPABILITY;
pub const REMOTE_CAPABILITY_REMOVE: &str = REMOVE_CAPABILITY;
pub const REMOTE_CAPABILITY_RESTART: &str = RESTART_CAPABILITY;
pub const REMOTE_CAPABILITY_RESTART_AGENT: &str = RESTART_AGENT_CAPABILITY;
pub const REMOTE_CAPABILITY_RESUME_AGENT: &str = RESUME_AGENT_CAPABILITY;
pub const REMOTE_CAPABILITY_CREATE: &str = CREATE_CAPABILITY;
pub const REMOTE_CAPABILITY_ORDER_SET: &str = ORDER_SET_CAPABILITY;
pub const REMOTE_CAPABILITY_PROJECT_ORGANIZATION_SET: &str = PROJECT_ORGANIZATION_CAPABILITY;
pub const REMOTE_CAPABILITY_PRESETS_SET: &str = PRESETS_CAPABILITY;
pub const REMOTE_CAPABILITY_OPENERS_SET: &str = OPENERS_CAPABILITY;
pub const REMOTE_CAPABILITY_WORKSPACE_SETTINGS_SET: &str = WORKSPACE_SETTINGS_CAPABILITY;
pub const REMOTE_CAPABILITY_APPS_INSTALL: &str = APPS_INSTALL_CAPABILITY;
pub const REMOTE_CAPABILITY_APPS_OPEN: &str = APPS_OPEN_CAPABILITY;
pub const REMOTE_CAPABILITY_ARCHIVE_LIST: &str = ARCHIVE_LIST_CAPABILITY;
pub const REMOTE_CAPABILITY_TRANSCRIPT_MARKDOWN: &str = TRANSCRIPT_MARKDOWN_CAPABILITY;
pub const REMOTE_CAPABILITY_METRICS_READ: &str = METRICS_CAPABILITY;
pub const REMOTE_CAPABILITY_PAIRING_INVITATION: &str = PAIRING_INVITATION_CAPABILITY;
const BOOTSTRAP_PATH: &str = "/mobile/bootstrap";
const OUTPUT_PATH: &str = "/mobile/output";
const WRITE_PATH: &str = "/mobile/write";
const RESIZE_DESKTOP_PATH: &str = "/mobile/resize-desktop";
const MARK_READ_PATH: &str = "/mobile/mark-read";
const APPROVAL_ANSWER_PATH: &str = "/mobile/approvals/answer";
const SESSION_ORGANIZATION_PATH: &str = "/mobile/session-organization";
const SESSION_ACTION_PATH: &str = "/mobile/session-action";
const RESTART_SESSION_PATH: &str = "/mobile/restart-session";
const SESSIONS_CREATE_PATH: &str = "/mobile/sessions";
const SESSION_ORDER_PATH: &str = "/mobile/session-order";
const PROJECT_ORGANIZATION_PATH: &str = "/mobile/project-organization";
const PRESETS_PATH: &str = "/mobile/presets";
const OPENERS_PATH: &str = "/mobile/openers";
const WORKSPACE_SETTINGS_PATH: &str = "/mobile/workspace-settings";
const APPS_INSTALL_PATH: &str = "/mobile/apps/install";
const APPS_OPEN_PATH: &str = "/mobile/apps/open";
const ARCHIVE_LIST_PATH: &str = "/mobile/archive";
const TRANSCRIPT_MARKDOWN_PATH: &str = "/mobile/transcript-markdown";
const METRICS_PATH: &str = "/mobile/metrics";
const PAIRING_INVITATION_PATH: &str = "/mobile/pairing-invitation";
const UPLOAD_PATH: &str = "/mobile/upload";
const MAX_SESSION_ID_BYTES: usize = 128;
/// Attachment uploads carry raw image bytes; the phone's flow compresses
/// before sending, so this bound is generous headroom, not a target.
const REMOTE_UPLOAD_MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROJECT_ID_BYTES: usize = 256;
const MAX_PRESET_ID_BYTES: usize = 256;
const MAX_HOST_ID_BYTES: usize = 256;
const INITIAL_TAIL_ALIGNMENT_ALLOWANCE: usize = 16 * 1024;
const DEFAULT_BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(10);
// A maximum Host long-poll may legitimately spend the full 25 seconds in the
// Host before its correlated response crosses SSH or Link. Keep transport
// response time outside that advertised wait instead of turning ordinary
// relay latency into a generation-destroying timeout.
const DEFAULT_OUTPUT_HEADROOM: Duration = Duration::from_secs(10);
const DEFAULT_EFFECT_TIMEOUT: Duration = Duration::from_secs(10);
const APP_INSTALL_EFFECT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteSessionStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteActivityState {
    Starting,
    Working,
    Blocked,
    Done,
    Idle,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProjectFolderSummary {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "parentFolderID")]
    pub parent_folder_id: Option<String>,
    #[serde(default, rename = "colorID")]
    pub color_id: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProjectSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default, rename = "folderID")]
    pub folder_id: Option<String>,
    #[serde(default, rename = "parentProjectID")]
    pub parent_project_id: Option<String>,
    #[serde(default)]
    pub worktree_branch: Option<String>,
    #[serde(default)]
    pub is_group: Option<bool>,
    #[serde(default, rename = "colorID")]
    pub color_id: Option<String>,
    /// Plain group pinned above its parent's ordinary mixed rows. Optional
    /// for protocol-minor compatibility; absent means unpinned.
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub git_branch: Option<String>,
    pub mcp_blocked: bool,
    #[serde(default)]
    pub sort_order: Option<i64>,
    #[serde(default)]
    pub archived_session_count: Option<u64>,
    /// Host-owned sidebar sort mode. Optional for protocol-minor/backward
    /// compatibility; absent means the manual/custom order.
    #[serde(default)]
    pub date_sorted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePresetSummary {
    pub id: String,
    pub label: String,
    pub command: String,
    #[serde(default, rename = "cliID")]
    pub cli_id: Option<String>,
    pub enabled: bool,
    pub quick_launch: bool,
    pub is_default: bool,
    #[serde(default)]
    pub tint_color_hex: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSessionCapabilities {
    pub restart: bool,
    /// Legacy protocol-minor-5 field retained for decoding older summaries.
    /// Current Hosts do not advertise it.
    #[serde(default)]
    pub restart_agent: bool,
    /// Resume the saved agent only after it has returned to the owned shell.
    /// Absent on Hosts before protocol minor 6 and false by default.
    #[serde(default)]
    pub resume_agent: bool,
    pub notify_when_done: bool,
    #[serde(default)]
    pub archive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSessionSummary {
    pub id: String,
    #[serde(rename = "projectID")]
    pub project_id: String,
    /// Host-observed runtime currently owning the Session's foreground job.
    /// This is deliberately separate from `providerID`, which remains the
    /// legacy launch-command identity for older Controllers.
    #[serde(
        default,
        rename = "activeRuntimeID",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_runtime_id: Option<String>,
    /// Host-resolved installed Unpeel App identity: the Controller cannot
    /// know a third-party App's name/tint from a compiled catalog, so both
    /// arrive as data. Absent on older Hosts and on non-App sessions.
    #[serde(
        default,
        rename = "activeAppID",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_app_id: Option<String>,
    #[serde(
        default,
        rename = "activeAppName",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_app_name: Option<String>,
    #[serde(
        default,
        rename = "activeAppTintHex",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_app_tint_hex: Option<u32>,
    /// Host-owned latch between managed launch submission and either runtime
    /// observation or definitive wrapper completion. Absent on older Hosts.
    #[serde(default)]
    pub runtime_launch_pending: bool,
    #[serde(default, rename = "providerID")]
    pub provider_id: Option<String>,
    pub title: String,
    pub command: String,
    pub created_at_unix_ms: i64,
    /// Opaque Host-authenticated Session owner. Missing on older Hosts.
    #[serde(default, rename = "ownerPrincipalID", alias = "ownerPrincipalId")]
    pub owner_principal_id: Option<String>,
    /// Optional creation-device audit provenance; never the authorization
    /// identity for the Session.
    #[serde(default, rename = "createdByDeviceID", alias = "createdByDeviceId")]
    pub created_by_device_id: Option<String>,
    /// Host-owned preset id used to create this Session.
    #[serde(default, rename = "sourcePresetID", alias = "sourcePresetId")]
    pub source_preset_id: Option<String>,
    #[serde(default)]
    pub updated_at_unix_ms: Option<i64>,
    pub status: RemoteSessionStatus,
    pub activity: RemoteActivityState,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub worktree_branch: Option<String>,
    #[serde(default, rename = "parentSessionID")]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub last_output_preview: Option<String>,
    #[serde(default)]
    pub notify_when_done: bool,
    #[serde(default)]
    pub terminal_background_hex: Option<u32>,
    #[serde(default)]
    pub capabilities: Option<RemoteSessionCapabilities>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub spinner_color_hex: Option<u32>,
    /// Latest App alert when it is this Session's newest activity. Additive;
    /// older Hosts omit it and older Controllers ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_alert_body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_alert_at_unix_ms: Option<i64>,
    /// A phone currently owns this Session's PTY grid (`resize-desktop`):
    /// a desktop Controller letterboxes its surface to it and offers "fit
    /// to desktop". Additive; older Hosts omit it. Must round-trip here or
    /// the native bridge strips it from the Local snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_fit_columns: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_fit_rows: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_fit_since_unix_ms: Option<u64>,
    /// Working directory the Session's PTY was launched in (the manifest
    /// `cwd`). A Controller seeds its terminal pane with it so cmd-clicked
    /// relative paths resolve against where the agent actually runs, before
    /// any OSC 7 report arrives. Additive; older Hosts omit it. Must
    /// round-trip here or the native bridge strips it from the snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePendingApproval {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(rename = "callerSessionID")]
    pub caller_session_id: String,
    #[serde(default, rename = "targetSessionID")]
    pub target_session_id: Option<String>,
    pub requested_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePaneGroupSummary {
    pub id: String,
    #[serde(rename = "representativeSessionID")]
    pub representative_session_id: String,
    #[serde(rename = "sessionIDs")]
    pub session_ids: Vec<String>,
}

/// One official App from the Host's embedded registry. `installed` is a live
/// PATH projection; the remaining fields describe Apps that may be offered
/// before installation without duplicating the catalog in a Controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAppSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tint: Option<String>,
    pub command: String,
    #[serde(default)]
    pub media_types: Vec<String>,
    #[serde(default)]
    pub file_extensions: HashMap<String, String>,
    #[serde(default)]
    pub resource_kinds: Vec<String>,
    #[serde(default)]
    pub default_for: Vec<String>,
    #[serde(default)]
    pub installed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBootstrapSnapshot {
    pub protocol_version: u16,
    #[serde(default)]
    pub host_protocol: Option<HostProtocolDescriptor>,
    #[serde(default, rename = "macID")]
    pub host_id: Option<String>,
    #[serde(default, rename = "macName")]
    pub host_name: Option<String>,
    pub folders: Vec<RemoteProjectFolderSummary>,
    pub projects: Vec<RemoteProjectSummary>,
    pub presets: Vec<RemotePresetSummary>,
    /// Additive (minor 10): current behavior knobs, for Controllers to show
    /// before editing through `settings.workspace.set`. Absent on older
    /// Hosts.
    #[serde(default, rename = "workspaceSettings")]
    pub workspace_settings: Option<RemoteWorkspaceSettings>,
    /// Additive (minor 15): the complete official catalog, including Apps not
    /// installed on this Host yet.
    #[serde(default)]
    pub available_apps: Vec<RemoteAppSummary>,
    /// Additive (minor 15): live installed subset, retained separately for
    /// simple Controllers and compatibility with the original plan.
    #[serde(default)]
    pub installed_apps: Vec<RemoteAppSummary>,
    /// Additive (minor 15): typed selector -> app/editor/system preference.
    #[serde(default)]
    pub openers: HashMap<String, String>,
    /// Additive (minor 15): validated semantic App/pane bindings. Controllers
    /// use the same envelope locally and remotely.
    #[serde(default)]
    pub app_presentations: Option<Value>,
    pub sessions: Vec<RemoteSessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_groups: Option<Vec<RemotePaneGroupSummary>>,
    pub captured_at_unix_ms: i64,
    #[serde(default)]
    pub remote_server_port: Option<u16>,
    #[serde(default)]
    pub remote_server_certificate_fingerprint: Option<String>,
    #[serde(default)]
    pub experimental_worktrees_enabled: Option<bool>,
    #[serde(default)]
    pub pro_entitled: Option<bool>,
    #[serde(default)]
    pub pending_approvals: Option<Vec<RemotePendingApproval>>,
    /// Presentation metadata retained across the Rust Controller backend so
    /// native UI does not lose fields advertised by a headless/SSH Host.
    #[serde(default)]
    pub host_tint_hue: Option<f64>,
    #[serde(default)]
    pub host_device_kind: Option<String>,
    #[serde(default)]
    pub host_device_model: Option<String>,
}

impl RemoteBootstrapSnapshot {
    /// Capability checks are authoritative when the Host advertises its
    /// descriptor. The two v1 read operations in this module retain the
    /// shipped pre-ledger fallback; no 404 probing is used as discovery.
    pub fn supports(&self, capability: &str) -> bool {
        match &self.host_protocol {
            Some(protocol) => protocol.supports(capability),
            None => matches!(capability, BOOTSTRAP_CAPABILITY | OUTPUT_CAPABILITY),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteBootstrap {
    pub snapshot: RemoteBootstrapSnapshot,
    pub raw: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteEffectFailureKind {
    /// Validation, capability, bootstrap, or transport failure proved the
    /// effect did not enter this Host generation.
    NotApplied,
    /// The effect may have landed but no trustworthy success receipt exists.
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEffectFailure {
    operation: &'static str,
    kind: RemoteEffectFailureKind,
    error: RemoteSessionBackendError,
}

impl RemoteEffectFailure {
    pub fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn kind(&self) -> RemoteEffectFailureKind {
        self.kind
    }

    pub fn error(&self) -> &RemoteSessionBackendError {
        &self.error
    }

    pub fn into_error(self) -> RemoteSessionBackendError {
        self.error
    }
}

impl fmt::Display for RemoteEffectFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RemoteEffectFailureKind::NotApplied => {
                write!(formatter, "{} was not sent: {}", self.operation, self.error)
            }
            RemoteEffectFailureKind::OutcomeUnknown => write!(
                formatter,
                "{} may have been applied; refresh Host state before retrying: {}",
                self.operation, self.error
            ),
        }
    }
}

impl std::error::Error for RemoteEffectFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteEffectReceipt {
    request_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteDesktopResize {
    Fit { columns: u16, rows: u16 },
    Clear,
}

impl RemoteEffectReceipt {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteOutputPollOptions {
    pub limit: usize,
    pub wait: Duration,
}

impl Default for RemoteOutputPollOptions {
    fn default() -> Self {
        Self {
            limit: REMOTE_OUTPUT_DEFAULT_LIMIT,
            wait: Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteSessionBackendError {
    Connection(HostConnectionError),
    HostStatus {
        operation: &'static str,
        status: u16,
        message: Option<String>,
    },
    InvalidResponse {
        operation: &'static str,
        message: String,
    },
    UnsupportedMobileProtocol {
        advertised: u16,
        required: u16,
    },
    IncompatibleHostProtocol {
        advertised_major: u16,
        advertised_minor: u16,
        required_major: u16,
    },
    MissingCapability(String),
    HostIdentityChanged {
        expected: String,
        received: Option<String>,
    },
    InvalidSessionId,
    InvalidProjectId,
    InvalidOutputOptions(String),
    InvalidEffectInput {
        operation: &'static str,
        message: String,
    },
    OutputPagePending(String),
    BootstrapChanged,
    StaleOutputPage,
    StateExhausted,
}

impl fmt::Display for RemoteSessionBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(error) => write!(formatter, "{error}"),
            Self::HostStatus {
                operation,
                status,
                message,
            } => match message {
                Some(message) => write!(formatter, "Host {operation} returned {status}: {message}"),
                None => write!(formatter, "Host {operation} returned {status}"),
            },
            Self::InvalidResponse { operation, message } => {
                write!(formatter, "invalid Host {operation} response: {message}")
            }
            Self::UnsupportedMobileProtocol {
                advertised,
                required,
            } => write!(
                formatter,
                "unsupported mobile protocol {advertised} (Controller requires {required})"
            ),
            Self::IncompatibleHostProtocol {
                advertised_major,
                advertised_minor,
                required_major,
            } => write!(
                formatter,
                "incompatible Host protocol {advertised_major}.{advertised_minor} (Controller requires major {required_major})"
            ),
            Self::MissingCapability(capability) => {
                write!(formatter, "Host does not advertise required capability {capability}")
            }
            Self::HostIdentityChanged { expected, received } => match received {
                Some(received) => write!(
                    formatter,
                    "Host identity changed from {expected} to {received}"
                ),
                None => write!(
                    formatter,
                    "Host stopped publishing its pinned identity {expected}"
                ),
            },
            Self::InvalidSessionId => write!(formatter, "invalid remote Session id"),
            Self::InvalidProjectId => write!(formatter, "invalid remote project id"),
            Self::InvalidOutputOptions(message) => {
                write!(formatter, "invalid remote output options: {message}")
            }
            Self::InvalidEffectInput { operation, message } => {
                write!(formatter, "invalid remote {operation}: {message}")
            }
            Self::OutputPagePending(session_id) => {
                write!(formatter, "Session {session_id} already has an uncommitted output page")
            }
            Self::BootstrapChanged => {
                write!(formatter, "the accepted Host bootstrap changed during the operation")
            }
            Self::StaleOutputPage => write!(formatter, "the remote output page is no longer current"),
            Self::StateExhausted => write!(formatter, "remote backend state id space is exhausted"),
        }
    }
}

impl std::error::Error for RemoteSessionBackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connection(error) => Some(error),
            _ => None,
        }
    }
}

impl From<HostConnectionError> for RemoteSessionBackendError {
    fn from(error: HostConnectionError) -> Self {
        Self::Connection(error)
    }
}

#[derive(Clone)]
pub struct RemoteSessionBackend {
    inner: Arc<BackendInner>,
}

struct BackendInner {
    id: uuid::Uuid,
    connection: Arc<dyn HostConnection>,
    bootstrap_timeout: Duration,
    output_headroom: Duration,
    bootstrap_gate: Mutex<()>,
    effect_sequence: EffectSequence,
    state: Mutex<BackendState>,
}

#[derive(Default)]
struct EffectSequence {
    state: Mutex<EffectSequenceState>,
    ready: Condvar,
}

#[derive(Default)]
struct EffectSequenceState {
    next_ticket: u64,
    serving: u64,
}

struct EffectTurn<'a> {
    sequence: &'a EffectSequence,
    ticket: u64,
}

impl EffectSequence {
    fn enter(&self) -> EffectTurn<'_> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ticket = state.next_ticket;
        state.next_ticket = state
            .next_ticket
            .checked_add(1)
            .expect("remote effect ticket counter exhausted");
        while state.serving != ticket {
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        EffectTurn {
            sequence: self,
            ticket,
        }
    }
}

impl Drop for EffectTurn<'_> {
    fn drop(&mut self) {
        let mut state = self
            .sequence
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        debug_assert_eq!(state.serving, self.ticket);
        state.serving = state
            .serving
            .checked_add(1)
            .expect("remote effect ticket counter exhausted");
        self.sequence.ready.notify_all();
    }
}

#[derive(Clone)]
struct AcceptedBootstrap {
    generation: ConnectionGeneration,
    value: RemoteBootstrap,
}

#[derive(Default)]
struct BackendState {
    last_bootstrap: Option<RemoteBootstrap>,
    accepted_generation: Option<ConnectionGeneration>,
    pinned_host_id: Option<String>,
    bootstrap_revision: u64,
    next_page_id: u64,
    cursors: HashMap<String, OutputCursor>,
}

#[derive(Default)]
struct OutputCursor {
    committed: Option<u64>,
    version: u64,
    pending: Option<PendingOutput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOutput {
    Fetching {
        page_id: u64,
        generation: ConnectionGeneration,
    },
    Staged {
        page_id: u64,
        generation: ConnectionGeneration,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputPollCursor {
    /// Continue from this backend's last committed page.
    Continue,
    /// Replace its cursor with the Controller's exact renderer position.
    /// `None` means a fresh bounded-tail replay.
    From(Option<u64>),
}

#[derive(Debug, Clone)]
struct OutputPageToken {
    backend_id: uuid::Uuid,
    session_id: String,
    page_id: u64,
    cursor_version: u64,
    generation: ConnectionGeneration,
    requested_offset: Option<u64>,
    next_offset: u64,
}

#[must_use = "feed this page and commit it, or discard it without advancing the cursor"]
pub struct RemoteOutputPage {
    token: Option<OutputPageToken>,
    backend: Weak<BackendInner>,
    session_id: String,
    requested_offset: Option<u64>,
    offset: u64,
    next_offset: u64,
    bytes: Vec<u8>,
    reset_before_feed: bool,
    truncated: bool,
    captured_at_unix_ms: i64,
}

impl fmt::Debug for RemoteOutputPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteOutputPage")
            .field("session_id", &self.session_id)
            .field("requested_offset", &self.requested_offset)
            .field("offset", &self.offset)
            .field("next_offset", &self.next_offset)
            .field("bytes", &self.bytes.len())
            .field("reset_before_feed", &self.reset_before_feed)
            .field("truncated", &self.truncated)
            .field("captured_at_unix_ms", &self.captured_at_unix_ms)
            .finish()
    }
}

impl RemoteOutputPage {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn requested_offset(&self) -> Option<u64> {
        self.requested_offset
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn next_offset(&self) -> u64 {
        self.next_offset
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn reset_required(&self) -> bool {
        self.reset_before_feed
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn captured_at_unix_ms(&self) -> i64 {
        self.captured_at_unix_ms
    }

    /// Advance the backend cursor only after the renderer accepted every byte.
    pub fn commit(mut self) -> Result<(), RemoteSessionBackendError> {
        let token = self
            .token
            .take()
            .ok_or(RemoteSessionBackendError::StaleOutputPage)?;
        let backend = self
            .backend
            .upgrade()
            .ok_or(RemoteSessionBackendError::StaleOutputPage)?;
        backend.commit_page(&token)
    }

    /// Explicitly abandon the page. Dropping it has the same effect.
    pub fn discard(mut self) {
        if let (Some(token), Some(backend)) = (self.token.take(), self.backend.upgrade()) {
            backend.discard_page(&token);
        }
    }
}

impl Drop for RemoteOutputPage {
    fn drop(&mut self) {
        if let (Some(token), Some(backend)) = (self.token.take(), self.backend.upgrade()) {
            backend.discard_page(&token);
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputChunkWire {
    #[serde(rename = "sessionID")]
    session_id: String,
    offset: u64,
    next_offset: u64,
    data_base64: String,
    #[serde(default)]
    truncated: bool,
    captured_at_unix_ms: i64,
}

#[derive(Debug, Deserialize)]
struct EffectReceiptWire {
    ok: bool,
}

/// A correlated, generation-matched 200 effect reply whose body has not yet
/// been interpreted.
#[derive(Debug)]
struct EffectExchange {
    request_id: u64,
    generation: ConnectionGeneration,
    body: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct TerminalWriteWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    data: &'a str,
}

#[derive(Debug, Serialize)]
struct DesktopResizeFitWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    columns: u16,
    rows: u16,
}

#[derive(Debug, Serialize)]
struct DesktopResizeClearWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    clear: bool,
}

#[derive(Debug, Serialize)]
struct MarkReadWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
}

#[derive(Debug, Serialize)]
struct SessionTitleWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    title: &'a str,
}

#[derive(Debug, Serialize)]
struct SessionPinnedWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    pinned: bool,
}

#[derive(Debug, Serialize)]
struct SessionNotifyWhenDoneWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    #[serde(rename = "notifyWhenDone")]
    notify_when_done: bool,
}

#[derive(Debug, Serialize)]
struct ApprovalAnswerWire<'a> {
    id: &'a str,
    approved: bool,
}

#[derive(Serialize)]
struct SessionProjectWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    #[serde(rename = "projectID")]
    project_id: &'a str,
}

#[derive(Debug, Serialize)]
struct SessionArchivedWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    archived: bool,
}

#[derive(Debug, Serialize)]
struct SessionActionWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    action: &'a str,
}

#[derive(Debug, Serialize)]
struct RestartSessionWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
}

#[derive(Debug, Serialize)]
struct SessionOrderWire<'a> {
    #[serde(rename = "projectID")]
    project_id: &'a str,
    #[serde(rename = "orderedSessionIDs")]
    ordered_session_ids: &'a [String],
}

/// One project's organization patch (`project.organization.set`). Absent
/// fields are left unchanged by the Host. `sort_order` moves the project to
/// that index among its same-parent siblings in the Host's current display
/// order; the Host persists it through the same path a local drag commits.
/// Legacy-folder moves (`folderID`) are deliberately not represented: no
/// Host implements them and the route rejects them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteProjectOrganizationPatch {
    pub sort_order: Option<i64>,
    pub display_name: Option<String>,
    pub color_id: Option<String>,
    pub date_sorted: Option<bool>,
    pub pinned: Option<bool>,
}

impl RemoteProjectOrganizationPatch {
    pub fn sort_order(index: i64) -> Self {
        Self {
            sort_order: Some(index),
            ..Self::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.sort_order.is_none()
            && self.display_name.is_none()
            && self.color_id.is_none()
            && self.date_sorted.is_none()
            && self.pinned.is_none()
    }
}

/// Typed patch for `settings.workspace.set`: the workspace's Host-owned
/// settings. All fields are optional; the Host validates each against its
/// whitelist before anything applies.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteWorkspaceSettingsPatch {
    pub auto_stop_archive_minutes: Option<i64>,
    pub transcript_settings: Option<RemoteTranscriptSettingsUpdate>,
    pub appearance_settings: Option<RemoteAppearanceSettingsUpdate>,
    pub notification_settings: Option<RemoteNotificationSettingsUpdate>,
    pub experimental_settings: Option<RemoteExperimentalSettingsUpdate>,
    pub sidebar_stopped_limit: Option<i64>,
    pub browser_default_access: Option<String>,
    pub mcp_nonchild_write_access: Option<String>,
    pub computer_access: Option<String>,
    pub mcp_worktree_access: Option<bool>,
    pub mcp_auto_add_browser_screenshots: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenerWire<'a> {
    selector: &'a str,
    opener: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AppInstallWire<'a> {
    #[serde(rename = "appID")]
    app_id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AppOpenResourceWire<'a> {
    kind: &'a str,
    id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppOpenWire<'a> {
    #[serde(rename = "callerSessionID")]
    caller_session_id: &'a str,
    #[serde(rename = "appID")]
    app_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<&'a str>,
    resource: AppOpenResourceWire<'a>,
    #[serde(rename = "requestID")]
    request_id: &'a str,
}

impl RemoteWorkspaceSettingsPatch {
    fn is_empty(&self) -> bool {
        self.transcript_settings.is_none()
            && self.appearance_settings.is_none()
            && self.notification_settings.is_none()
            && self.experimental_settings.is_none()
            && self.auto_stop_archive_minutes.is_none()
            && self.sidebar_stopped_limit.is_none()
            && self.browser_default_access.is_none()
            && self.mcp_nonchild_write_access.is_none()
            && self.computer_access.is_none()
            && self.mcp_worktree_access.is_none()
            && self.mcp_auto_add_browser_screenshots.is_none()
    }
}

/// Nested transcript rendering patch inside `settings.workspace.set`; all
/// fields optional, merged into the stored object by the Host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTranscriptSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_user: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_assistant: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_file_changes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_plan_updates: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_session_info: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<i64>,
}

/// The Host-advertised transcript rendering values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTranscriptSettings {
    pub include_user: bool,
    pub include_assistant: bool,
    pub include_reasoning: bool,
    pub include_tools: bool,
    pub include_file_changes: bool,
    pub include_plan_updates: bool,
    pub include_session_info: bool,
    pub max_entries: i64,
}

/// Appearance patch inside `settings.workspace.set`. Controllers render
/// these values while scoped to the Host; session-title mode is applied by
/// the Host itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAppearanceSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_tint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_tone: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_tone: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAppearanceSettings {
    pub theme: String,
    pub app_tint: String,
    pub background_opacity: f64,
    pub surface_opacity: f64,
    pub background_tone: f64,
    pub surface_tone: f64,
    pub session_title_mode: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteNotificationSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub menu_attention_detection: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteNotificationSettings {
    pub menu_attention_detection: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteExperimentalSettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions_mcp: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_mcp: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteExperimentalSettings {
    pub worktrees: bool,
    pub sessions_mcp: bool,
    pub browser_mcp: bool,
    pub computer_use: bool,
    /// Runtime adapter advertisement (protocol minor 14). Missing means an
    /// older Host; Controllers must not infer support from `hostDeviceKind`.
    #[serde(default)]
    pub computer_use_available: Option<bool>,
    #[serde(default)]
    pub computer_use_ready: Option<bool>,
    #[serde(default)]
    pub computer_use_unavailable_reason: Option<String>,
    pub workspaces: bool,
}

/// The Host-advertised current values (`bootstrap.workspaceSettings`,
/// additive — absent on pre-minor-10 Hosts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkspaceSettings {
    #[serde(default)]
    pub transcript_settings: Option<RemoteTranscriptSettings>,
    #[serde(default)]
    pub appearance_settings: Option<RemoteAppearanceSettings>,
    #[serde(default)]
    pub notification_settings: Option<RemoteNotificationSettings>,
    #[serde(default)]
    pub experimental_settings: Option<RemoteExperimentalSettings>,
    pub auto_stop_archive_minutes: i64,
    pub sidebar_stopped_limit: i64,
    pub browser_default_access: String,
    pub mcp_nonchild_write_access: String,
    pub computer_access: String,
    pub mcp_worktree_access: bool,
    pub mcp_auto_add_browser_screenshots: bool,
}

#[derive(Debug, Serialize)]
struct WorkspaceSettingsWire<'a> {
    #[serde(rename = "transcriptSettings", skip_serializing_if = "Option::is_none")]
    transcript_settings: Option<&'a RemoteTranscriptSettingsUpdate>,
    #[serde(rename = "appearanceSettings", skip_serializing_if = "Option::is_none")]
    appearance_settings: Option<&'a RemoteAppearanceSettingsUpdate>,
    #[serde(
        rename = "notificationSettings",
        skip_serializing_if = "Option::is_none"
    )]
    notification_settings: Option<&'a RemoteNotificationSettingsUpdate>,
    #[serde(
        rename = "experimentalSettings",
        skip_serializing_if = "Option::is_none"
    )]
    experimental_settings: Option<&'a RemoteExperimentalSettingsUpdate>,
    #[serde(
        rename = "autoStopArchiveMinutes",
        skip_serializing_if = "Option::is_none"
    )]
    auto_stop_archive_minutes: Option<i64>,
    #[serde(
        rename = "sidebarStoppedLimit",
        skip_serializing_if = "Option::is_none"
    )]
    sidebar_stopped_limit: Option<i64>,
    #[serde(
        rename = "browserDefaultAccess",
        skip_serializing_if = "Option::is_none"
    )]
    browser_default_access: Option<&'a str>,
    #[serde(
        rename = "mcpNonchildWriteAccess",
        skip_serializing_if = "Option::is_none"
    )]
    mcp_nonchild_write_access: Option<&'a str>,
    #[serde(rename = "computerAccess", skip_serializing_if = "Option::is_none")]
    computer_access: Option<&'a str>,
    #[serde(rename = "mcpWorktreeAccess", skip_serializing_if = "Option::is_none")]
    mcp_worktree_access: Option<bool>,
    #[serde(
        rename = "mcpAutoAddBrowserScreenshots",
        skip_serializing_if = "Option::is_none"
    )]
    mcp_auto_add_browser_screenshots: Option<bool>,
}

/// One-preset patch for `settings.presets.set`: `preset_id: None` creates
/// (`command` required — the Host mints the id), otherwise edit
/// `command`/`label`, star (`quick_launch`), move to `sort_order` in the
/// Host's display order, or `removed` (not combinable with other fields).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemotePresetPatch {
    pub preset_id: Option<String>,
    pub command: Option<String>,
    pub label: Option<String>,
    pub quick_launch: Option<bool>,
    pub sort_order: Option<i64>,
    pub removed: bool,
}

impl RemotePresetPatch {
    fn edits_nothing(&self) -> bool {
        self.command.is_none()
            && self.label.is_none()
            && self.quick_launch.is_none()
            && self.sort_order.is_none()
    }
}

#[derive(Debug, Serialize)]
struct PresetPatchWire<'a> {
    #[serde(rename = "presetID", skip_serializing_if = "Option::is_none")]
    preset_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    #[serde(rename = "quickLaunch", skip_serializing_if = "Option::is_none")]
    quick_launch: Option<bool>,
    #[serde(rename = "sortOrder", skip_serializing_if = "Option::is_none")]
    sort_order: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ProjectOrganizationWire<'a> {
    #[serde(rename = "projectID")]
    project_id: &'a str,
    #[serde(rename = "sortOrder", skip_serializing_if = "Option::is_none")]
    sort_order: Option<i64>,
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    display_name: Option<&'a str>,
    #[serde(rename = "colorID", skip_serializing_if = "Option::is_none")]
    color_id: Option<&'a str>,
    #[serde(rename = "dateSorted", skip_serializing_if = "Option::is_none")]
    date_sorted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pinned: Option<bool>,
}

/// Initial-text submit behavior for a Controller-created Session; matches the
/// shipped `RemoteTextSubmitMode` wire enum (camelCase values).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteTextSubmitMode {
    PasteOnly,
    #[default]
    PasteAndSubmit,
    Raw,
}

/// Parameters for `POST /mobile/sessions`. The Host resolves the project and
/// preset against its own catalog; Controller-supplied worktree fields are
/// compatibility assertions only and never introduce a path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteSessionCreateRequest {
    pub project_id: String,
    pub preset_id: Option<String>,
    pub command: Option<String>,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    pub initial_text: Option<String>,
    pub initial_text_submit_mode: RemoteTextSubmitMode,
}

impl RemoteSessionCreateRequest {
    pub fn from_preset(project_id: impl Into<String>, preset_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            preset_id: Some(preset_id.into()),
            ..Self::default()
        }
    }

    pub fn from_command(project_id: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            command: Some(command.into()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionWire<'a> {
    #[serde(rename = "projectID")]
    project_id: &'a str,
    #[serde(rename = "presetID", skip_serializing_if = "Option::is_none")]
    preset_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_branch: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_text_submit_mode: Option<RemoteTextSubmitMode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponseWire {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(default)]
    captured_at_unix_ms: Option<i64>,
    #[serde(default)]
    session: Option<RemoteSessionSummary>,
}

/// Receipt for a Controller-initiated Session create. `session` is the
/// optional optimistic summary newer Hosts return; headless Hosts may omit it
/// and let the next bootstrap publish the row.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteCreatedSession {
    pub receipt: RemoteEffectReceipt,
    pub session_id: String,
    pub captured_at_unix_ms: Option<i64>,
    pub session: Option<RemoteSessionSummary>,
}

#[derive(Debug, Deserialize)]
struct UploadResponseWire {
    path: String,
}

/// Receipt for an attachment upload: the HOST-side path the Controller may
/// paste into the terminal as an attachable reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUploadedAttachment {
    pub receipt: RemoteEffectReceipt,
    pub path: String,
}

#[derive(Debug, Deserialize)]
struct ArchivedSessionsWire {
    #[serde(rename = "projectID")]
    project_id: String,
    sessions: Vec<RemoteSessionSummary>,
}

#[derive(Debug, Deserialize)]
struct TranscriptMarkdownWire {
    #[serde(rename = "sessionID")]
    session_id: String,
    markdown: String,
}

/// One Session's rendered conversation transcript from the Host's shared
/// provider transcript reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTranscriptMarkdown {
    pub session_id: String,
    pub markdown: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetricsWire {
    #[serde(rename = "sessionID")]
    session_id: String,
    columns: u16,
    rows: u16,
    /// Absent on Hosts that predate the offset in the gateway metrics body;
    /// the grid alone is what the phone's fit math needs.
    #[serde(default)]
    output_offset: Option<u64>,
    captured_at_unix_ms: i64,
}

/// One live Session's current terminal grid from the Host's viewport
/// snapshot — what a Controller needs to fit/letterbox a remote terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSessionMetrics {
    pub session_id: String,
    pub columns: u16,
    pub rows: u16,
    pub output_offset: Option<u64>,
    pub captured_at_unix_ms: i64,
}

impl RemoteSessionBackend {
    pub fn new(connection: Arc<dyn HostConnection>) -> Self {
        Self::with_timeouts(
            connection,
            DEFAULT_BOOTSTRAP_TIMEOUT,
            DEFAULT_OUTPUT_HEADROOM,
        )
    }

    pub fn with_timeouts(
        connection: Arc<dyn HostConnection>,
        bootstrap_timeout: Duration,
        output_headroom: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(BackendInner {
                id: uuid::Uuid::new_v4(),
                connection,
                bootstrap_timeout,
                output_headroom,
                bootstrap_gate: Mutex::new(()),
                effect_sequence: EffectSequence::default(),
                state: Mutex::new(BackendState::default()),
            }),
        }
    }

    /// Refresh and accept the Host snapshot. Concurrent callers share the
    /// refresh that completed while they waited for the bootstrap gate.
    pub fn bootstrap(&self) -> Result<RemoteBootstrap, RemoteSessionBackendError> {
        self.inner.refresh_bootstrap()
    }

    /// Return the most recently validated snapshot, including while the
    /// transport generation is stale and [`Self::needs_bootstrap`] is true.
    pub fn last_bootstrap(&self) -> Option<RemoteBootstrap> {
        self.inner.lock_state().last_bootstrap.clone()
    }

    /// A stale snapshot may remain available for display while this is true;
    /// no bound call can run until the next accepted bootstrap.
    pub fn needs_bootstrap(&self) -> bool {
        self.inner.lock_state().accepted_generation.is_none()
    }

    pub fn committed_output_offset(&self, session_id: &str) -> Option<u64> {
        self.inner
            .lock_state()
            .cursors
            .get(session_id)
            .and_then(|cursor| cursor.committed)
    }

    /// Force the next poll to request a fresh bounded tail for this Session.
    pub fn reset_output_cursor(&self, session_id: &str) -> Result<(), RemoteSessionBackendError> {
        validate_session_id(session_id)?;
        let mut state = self.inner.lock_state();
        let cursor = state.cursors.entry(session_id.to_owned()).or_default();
        cursor.version = cursor
            .version
            .checked_add(1)
            .ok_or(RemoteSessionBackendError::StateExhausted)?;
        cursor.committed = None;
        cursor.pending = None;
        Ok(())
    }

    pub fn poll_output(
        &self,
        session_id: &str,
        options: RemoteOutputPollOptions,
    ) -> Result<RemoteOutputPage, RemoteSessionBackendError> {
        validate_session_id(session_id)?;
        validate_output_options(options)?;
        self.inner
            .poll_output(session_id, options, OutputPollCursor::Continue)
    }

    /// Poll from the Controller's exact renderer cursor, replacing any
    /// previously committed or pending page for this Session. This is the
    /// recovery path after a response was committed locally but lost before
    /// the Controller received it. `None` explicitly requests a fresh bounded
    /// tail; it is distinct from [`Self::poll_output`], which continues the
    /// backend's current committed cursor.
    ///
    /// Repositioning and reserving the replacement page happen under the same
    /// state lock. An older renderer can therefore finish its transport read,
    /// but its versioned page can no longer stage or commit across this call.
    pub fn poll_output_from(
        &self,
        session_id: &str,
        requested_offset: Option<u64>,
        options: RemoteOutputPollOptions,
    ) -> Result<RemoteOutputPage, RemoteSessionBackendError> {
        validate_session_id(session_id)?;
        validate_output_options(options)?;
        self.inner.poll_output(
            session_id,
            options,
            OutputPollCursor::From(requested_offset),
        )
    }

    /// Dispatch UTF-8 terminal bytes at most once on the generation accepted
    /// by bootstrap. The backend never reconnects or replays this effect.
    pub fn write_terminal(
        &self,
        session_id: &str,
        data: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        let effect_turn = self.inner.begin_effect();
        effect_preflight("terminal write", || {
            validate_session_id(session_id)?;
            if data.len() > REMOTE_TERMINAL_WRITE_MAX_BYTES {
                return Err(invalid_effect_input(
                    "terminal write",
                    format!(
                        "data is {} bytes (maximum {REMOTE_TERMINAL_WRITE_MAX_BYTES})",
                        data.len()
                    ),
                ));
            }
            Ok(())
        })?;
        let body = encode_effect_body("terminal write", &TerminalWriteWire { session_id, data })?;
        self.inner.perform_effect(
            &effect_turn,
            "terminal write",
            WRITE_CAPABILITY,
            WRITE_PATH,
            body,
        )
    }

    /// Fit or clear the Host desktop/TUI presentation for this Controller.
    /// Fit dimensions follow the shipped v1 clamping contract.
    pub fn resize_desktop(
        &self,
        session_id: &str,
        resize: RemoteDesktopResize,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        let effect_turn = self.inner.begin_effect();
        effect_preflight("desktop resize", || validate_session_id(session_id))?;
        let body = match resize {
            RemoteDesktopResize::Fit { columns, rows } => encode_effect_body(
                "desktop resize",
                &DesktopResizeFitWire {
                    session_id,
                    columns: columns.clamp(
                        REMOTE_DESKTOP_RESIZE_MIN_COLUMNS,
                        REMOTE_DESKTOP_RESIZE_MAX_COLUMNS,
                    ),
                    rows: rows.clamp(
                        REMOTE_DESKTOP_RESIZE_MIN_ROWS,
                        REMOTE_DESKTOP_RESIZE_MAX_ROWS,
                    ),
                },
            )?,
            RemoteDesktopResize::Clear => encode_effect_body(
                "desktop resize",
                &DesktopResizeClearWire {
                    session_id,
                    clear: true,
                },
            )?,
        };
        self.inner.perform_effect(
            &effect_turn,
            "desktop resize",
            RESIZE_DESKTOP_CAPABILITY,
            RESIZE_DESKTOP_PATH,
            body,
        )
    }

    /// Clear the Host's unread marker for a Session. It still travels as an
    /// effect and is never silently replayed even though the Host verb is
    /// idempotent/best-effort.
    pub fn mark_session_read(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        let effect_turn = self.inner.begin_effect();
        effect_preflight("mark Session read", || validate_session_id(session_id))?;
        let body = encode_effect_body("mark Session read", &MarkReadWire { session_id })?;
        self.inner.perform_effect(
            &effect_turn,
            "mark Session read",
            MARK_READ_CAPABILITY,
            MARK_READ_PATH,
            body,
        )
    }

    /// Rename a Session (`session.title.set`). The Host trims the title and
    /// treats a whitespace-only rename as a no-op, so that case is rejected
    /// here instead of silently succeeding.
    pub fn set_session_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session title";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            validate_session_id(session_id)?;
            if title.trim().is_empty() {
                return Err(invalid_effect_input(OPERATION, "title is empty"));
            }
            if title.len() > REMOTE_SESSION_TITLE_MAX_BYTES {
                return Err(invalid_effect_input(
                    OPERATION,
                    format!(
                        "title is {} bytes (maximum {REMOTE_SESSION_TITLE_MAX_BYTES})",
                        title.len()
                    ),
                ));
            }
            Ok(())
        })?;
        let body = encode_effect_body(OPERATION, &SessionTitleWire { session_id, title })?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            TITLE_SET_CAPABILITY,
            SESSION_ORGANIZATION_PATH,
            body,
        )
    }

    /// Pin or unpin a Session in the Host sidebar (`session.pin.set`).
    pub fn set_session_pinned(
        &self,
        session_id: &str,
        pinned: bool,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session pin";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(OPERATION, &SessionPinnedWire { session_id, pinned })?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            PIN_SET_CAPABILITY,
            SESSION_ORGANIZATION_PATH,
            body,
        )
    }

    /// Opt a Session in or out of completion delivery through the Host's
    /// currently registered platform adapter (`session.notify_when_done.set`).
    pub fn set_session_notify_when_done(
        &self,
        session_id: &str,
        notify_when_done: bool,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "notify when done";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(
            OPERATION,
            &SessionNotifyWhenDoneWire {
                session_id,
                notify_when_done,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            NOTIFY_WHEN_DONE_CAPABILITY,
            SESSION_ORGANIZATION_PATH,
            body,
        )
    }

    /// Answer one Host-owned MCP approval. First answer wins; a 409 remains
    /// an applied semantic response and is surfaced to the Controller rather
    /// than replayed across a replacement connection.
    pub fn answer_approval(
        &self,
        approval_id: &str,
        approved: bool,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "approval answer";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            let id = approval_id.trim();
            if id.is_empty() || id.len() > MAX_SESSION_ID_BYTES || id.contains(['\r', '\n', '\0']) {
                return Err(invalid_effect_input(OPERATION, "invalid approval id"));
            }
            Ok(())
        })?;
        let body = encode_effect_body(
            OPERATION,
            &ApprovalAnswerWire {
                id: approval_id,
                approved,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            APPROVAL_ANSWER_CAPABILITY,
            APPROVAL_ANSWER_PATH,
            body,
        )
    }

    /// File a Session under another project/group (`session.project.set`) —
    /// the Host writes its shared project-override marker; the manifest's own
    /// project clears it. Display-only, never a manifest edit.
    pub fn set_session_project(
        &self,
        session_id: &str,
        project_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session project";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            validate_session_id(session_id)?;
            validate_project_id(project_id)
                .map_err(|error| invalid_effect_input(OPERATION, error.to_string()))
        })?;
        let body = encode_effect_body(
            OPERATION,
            &SessionProjectWire {
                session_id,
                project_id,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            PROJECT_SET_CAPABILITY,
            SESSION_ORGANIZATION_PATH,
            body,
        )
    }

    /// File a Session away non-destructively (`session.archive`): the Host
    /// stops the hosted PTY but keeps the whole session dir.
    pub fn archive_session(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session archive";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(
            OPERATION,
            &SessionArchivedWire {
                session_id,
                archived: true,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            ARCHIVE_CAPABILITY,
            SESSION_ORGANIZATION_PATH,
            body,
        )
    }

    /// Restore an archived Session to the sidebar (`session.restore`).
    pub fn restore_session(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session restore";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(
            OPERATION,
            &SessionArchivedWire {
                session_id,
                archived: false,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            RESTORE_CAPABILITY,
            SESSION_ORGANIZATION_PATH,
            body,
        )
    }

    /// Stop a running Session's hosted PTY, keeping the row restartable
    /// (`session.stop`).
    pub fn stop_session(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session stop";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(
            OPERATION,
            &SessionActionWire {
                session_id,
                action: "stop",
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            STOP_CAPABILITY,
            SESSION_ACTION_PATH,
            body,
        )
    }

    /// Remove a Session row and its on-disk artifacts (`session.remove`) —
    /// the destructive verb.
    pub fn remove_session(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session remove";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(
            OPERATION,
            &SessionActionWire {
                session_id,
                action: "remove",
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            REMOVE_CAPABILITY,
            SESSION_ACTION_PATH,
            body,
        )
    }

    /// Restart a Session with the Host's resume behavior (`session.restart`).
    pub fn restart_session(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session restart";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(OPERATION, &RestartSessionWire { session_id })?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            RESTART_CAPABILITY,
            RESTART_SESSION_PATH,
            body,
        )
    }

    /// Restart the managed runtime inside its existing hosted PTY
    /// (`session.runtime.restart`). The Session id, terminal, and scrollback
    /// stay intact; Hosts fail closed for blank/passively observed runtimes.
    pub fn restart_agent(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session agent restart";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(
            OPERATION,
            &SessionActionWire {
                session_id,
                action: "restart_agent",
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            RESTART_AGENT_CAPABILITY,
            SESSION_ACTION_PATH,
            body,
        )
    }

    /// Resume an ended managed runtime inside its existing hosted PTY
    /// (`session.runtime.resume`). The Host rejects active runtimes and
    /// unrecognized foreground jobs without signaling them.
    pub fn resume_agent(
        &self,
        session_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session agent resume";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_session_id(session_id))?;
        let body = encode_effect_body(
            OPERATION,
            &SessionActionWire {
                session_id,
                action: "resume_agent",
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            RESUME_AGENT_CAPABILITY,
            SESSION_ACTION_PATH,
            body,
        )
    }

    /// Replace one project's hand-ordered sidebar Session ranks
    /// (`session.order.set`). The list is the combined pinned + regular order
    /// exactly as a desktop drag commits it.
    pub fn set_session_order(
        &self,
        project_id: &str,
        ordered_session_ids: &[String],
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "session order";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            validate_project_id(project_id)?;
            if ordered_session_ids.len() > REMOTE_SESSION_ORDER_MAX_IDS {
                return Err(invalid_effect_input(
                    OPERATION,
                    format!(
                        "{} session ids (maximum {REMOTE_SESSION_ORDER_MAX_IDS})",
                        ordered_session_ids.len()
                    ),
                ));
            }
            for session_id in ordered_session_ids {
                validate_session_id(session_id)?;
            }
            Ok(())
        })?;
        let body = encode_effect_body(
            OPERATION,
            &SessionOrderWire {
                project_id,
                ordered_session_ids,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            ORDER_SET_CAPABILITY,
            SESSION_ORDER_PATH,
            body,
        )
    }

    /// Organize one sidebar project/group (`project.organization.set`) —
    /// one-project patch, exactly the shipped route shape. The reorder case
    /// (`sort_order`) is the Controller half of a project drag: the Host
    /// moves the project to that sibling index in ITS current display order
    /// and persists through the same choke point a local drag uses.
    pub fn set_project_organization(
        &self,
        project_id: &str,
        patch: &RemoteProjectOrganizationPatch,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "project organization";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            validate_project_id(project_id)?;
            if patch.is_empty() {
                return Err(invalid_effect_input(OPERATION, "patch is empty"));
            }
            if patch.sort_order.is_some_and(|index| index < 0) {
                return Err(invalid_effect_input(OPERATION, "sortOrder is negative"));
            }
            if let Some(display_name) = patch.display_name.as_deref() {
                if display_name.trim().is_empty() {
                    return Err(invalid_effect_input(OPERATION, "displayName is empty"));
                }
                if display_name.len() > REMOTE_SESSION_TITLE_MAX_BYTES {
                    return Err(invalid_effect_input(
                        OPERATION,
                        format!(
                            "displayName is {} bytes (maximum {REMOTE_SESSION_TITLE_MAX_BYTES})",
                            display_name.len()
                        ),
                    ));
                }
            }
            Ok(())
        })?;
        let body = encode_effect_body(
            OPERATION,
            &ProjectOrganizationWire {
                project_id,
                sort_order: patch.sort_order,
                display_name: patch.display_name.as_deref(),
                color_id: patch.color_id.as_deref(),
                date_sorted: patch.date_sorted,
                pinned: patch.pinned,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            PROJECT_ORGANIZATION_CAPABILITY,
            PROJECT_ORGANIZATION_PATH,
            body,
        )
    }

    /// Edit the Host's flat preset list (`settings.presets.set`) — one-preset
    /// patch: create, edit command/label, star, move to a display index, or
    /// remove. The Host applies it through its own preset choke point
    /// (`app_state::edit` — flock + state-bus announce), so both frontends of
    /// the edited workspace pick the change up live; a create's minted id
    /// arrives on the next bootstrap/snapshot refresh. Generation-bound and
    /// never auto-replayed, like every effect.
    pub fn set_preset(
        &self,
        patch: &RemotePresetPatch,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "preset edit";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            if let Some(preset_id) = patch.preset_id.as_deref() {
                if preset_id.trim().is_empty()
                    || preset_id.len() > MAX_PRESET_ID_BYTES
                    || preset_id.contains('\0')
                {
                    return Err(invalid_effect_input(OPERATION, "invalid preset id"));
                }
            } else if patch.removed {
                return Err(invalid_effect_input(
                    OPERATION,
                    "removed requires a preset id",
                ));
            } else if patch.command.is_none() {
                return Err(invalid_effect_input(
                    OPERATION,
                    "creating a preset requires a command",
                ));
            }
            if patch.removed && !patch.edits_nothing() {
                return Err(invalid_effect_input(
                    OPERATION,
                    "removed cannot be combined with other fields",
                ));
            }
            if !patch.removed && patch.preset_id.is_some() && patch.edits_nothing() {
                return Err(invalid_effect_input(OPERATION, "patch is empty"));
            }
            if let Some(command) = patch.command.as_deref() {
                if command.trim().is_empty() {
                    return Err(invalid_effect_input(OPERATION, "command is empty"));
                }
                if command.len() > REMOTE_SESSION_TITLE_MAX_BYTES {
                    return Err(invalid_effect_input(
                        OPERATION,
                        format!(
                            "command is {} bytes (maximum {REMOTE_SESSION_TITLE_MAX_BYTES})",
                            command.len()
                        ),
                    ));
                }
            }
            if let Some(label) = patch.label.as_deref() {
                if label.len() > REMOTE_SESSION_TITLE_MAX_BYTES {
                    return Err(invalid_effect_input(
                        OPERATION,
                        format!(
                            "label is {} bytes (maximum {REMOTE_SESSION_TITLE_MAX_BYTES})",
                            label.len()
                        ),
                    ));
                }
            }
            if patch.sort_order.is_some_and(|index| index < 0) {
                return Err(invalid_effect_input(OPERATION, "sortOrder is negative"));
            }
            Ok(())
        })?;
        let body = encode_effect_body(
            OPERATION,
            &PresetPatchWire {
                preset_id: patch.preset_id.as_deref(),
                command: patch.command.as_deref(),
                label: patch.label.as_deref(),
                quick_launch: patch.quick_launch,
                sort_order: patch.sort_order,
                removed: patch.removed.then_some(true),
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            PRESETS_CAPABILITY,
            PRESETS_PATH,
            body,
        )
    }

    /// Edit the workspace's behavior knobs (`settings.workspace.set`) — the
    /// auto-stop-archive cutoff, the sidebar inactive window, and the MCP
    /// access policies. One typed patch, one generation-bound effect; the
    /// Host validates every field against its whitelist before anything
    /// applies, and its app-state save announces to every frontend of the
    /// edited workspace.
    pub fn set_workspace_settings(
        &self,
        patch: &RemoteWorkspaceSettingsPatch,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "workspace settings";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            if patch.is_empty() {
                return Err(invalid_effect_input(OPERATION, "patch is empty"));
            }
            let enum_ok =
                |value: &Option<String>, allowed: &[&str], name: &str| match value.as_deref() {
                    None => Ok(()),
                    Some(value) if allowed.contains(&value) => Ok(()),
                    Some(_) => Err(invalid_effect_input(OPERATION, format!("invalid {name}"))),
                };
            enum_ok(
                &patch.browser_default_access,
                &["on", "ask", "off"],
                "browserDefaultAccess",
            )?;
            enum_ok(
                &patch.mcp_nonchild_write_access,
                &["ask", "allow", "deny"],
                "mcpNonchildWriteAccess",
            )?;
            enum_ok(
                &patch.computer_access,
                &["ask", "allow", "off"],
                "computerAccess",
            )?;
            if let Some(appearance) = &patch.appearance_settings {
                enum_ok(
                    &appearance.theme,
                    &["system", "light", "dark"],
                    "appearanceSettings.theme",
                )?;
                enum_ok(
                    &appearance.app_tint,
                    &[
                        "none", "peel", "amber", "green", "teal", "blue", "indigo", "violet",
                    ],
                    "appearanceSettings.appTint",
                )?;
                enum_ok(
                    &appearance.session_title_mode,
                    &["first_prompt", "agent", "off"],
                    "appearanceSettings.sessionTitleMode",
                )?;
                for (name, value) in [
                    ("backgroundOpacity", appearance.background_opacity),
                    ("surfaceOpacity", appearance.surface_opacity),
                    ("backgroundTone", appearance.background_tone),
                    ("surfaceTone", appearance.surface_tone),
                ] {
                    if value
                        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
                    {
                        return Err(invalid_effect_input(
                            OPERATION,
                            format!("invalid appearanceSettings.{name}"),
                        ));
                    }
                }
            }
            if patch
                .auto_stop_archive_minutes
                .is_some_and(|minutes| minutes < 0)
            {
                return Err(invalid_effect_input(OPERATION, "negative cutoff"));
            }
            if patch.sidebar_stopped_limit.is_some_and(|limit| limit < 0) {
                return Err(invalid_effect_input(OPERATION, "negative sidebar window"));
            }
            if patch
                .transcript_settings
                .as_ref()
                .and_then(|settings| settings.max_entries)
                .is_some_and(|entries| entries < 0)
            {
                return Err(invalid_effect_input(OPERATION, "negative maxEntries"));
            }
            Ok(())
        })?;
        let body = encode_effect_body(
            OPERATION,
            &WorkspaceSettingsWire {
                transcript_settings: patch.transcript_settings.as_ref(),
                appearance_settings: patch.appearance_settings.as_ref(),
                notification_settings: patch.notification_settings.as_ref(),
                experimental_settings: patch.experimental_settings.as_ref(),
                auto_stop_archive_minutes: patch.auto_stop_archive_minutes,
                sidebar_stopped_limit: patch.sidebar_stopped_limit,
                browser_default_access: patch.browser_default_access.as_deref(),
                mcp_nonchild_write_access: patch.mcp_nonchild_write_access.as_deref(),
                computer_access: patch.computer_access.as_deref(),
                mcp_worktree_access: patch.mcp_worktree_access,
                mcp_auto_add_browser_screenshots: patch.mcp_auto_add_browser_screenshots,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            WORKSPACE_SETTINGS_CAPABILITY,
            WORKSPACE_SETTINGS_PATH,
            body,
        )
    }

    /// Set one Host-owned typed-resource opener preference.
    pub fn set_opener(
        &self,
        selector: &str,
        opener: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "resource opener";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            if selector.is_empty()
                || selector.len() > 228
                || (!selector.starts_with("file:") && !selector.starts_with("resource:"))
                || !selector.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(byte, b':' | b'/' | b'.' | b'+' | b'_' | b'-')
                })
            {
                return Err(invalid_effect_input(OPERATION, "invalid opener selector"));
            }
            if !matches!(opener, "editor" | "system") && !opener.starts_with("app:unpeel.app.") {
                return Err(invalid_effect_input(OPERATION, "invalid opener"));
            }
            Ok(())
        })?;
        let body = encode_effect_body(OPERATION, &OpenerWire { selector, opener })?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            OPENERS_CAPABILITY,
            OPENERS_PATH,
            body,
        )
    }

    /// Install an official App on the Host. The Host resolves the id through
    /// its own embedded allowlist and chooses the platform artifact.
    pub fn install_app(&self, app_id: &str) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "App install";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            if app_id.is_empty()
                || app_id.len() > 128
                || !app_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                return Err(invalid_effect_input(OPERATION, "invalid App id"));
            }
            Ok(())
        })?;
        let body = encode_effect_body(OPERATION, &AppInstallWire { app_id })?;
        self.inner.perform_effect_with_timeout(
            &effect_turn,
            OPERATION,
            APPS_INSTALL_CAPABILITY,
            APPS_INSTALL_PATH,
            body,
            APP_INSTALL_EFFECT_TIMEOUT,
        )
    }

    /// Open an installed App for one typed resource. This user-initiated
    /// Controller entry may create/restart the semantic companion; MCP can
    /// only attach to one that already exists. The resulting pane arrives
    /// through bootstrap's `appPresentations` projection.
    pub fn open_app(
        &self,
        caller_session_id: &str,
        app_id: &str,
        resource_kind: &str,
        media_type: Option<&str>,
        resource_id: &str,
        request_id: &str,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        const OPERATION: &str = "App open";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            for (name, value) in [
                ("caller Session id", caller_session_id),
                ("App id", app_id),
                ("request id", request_id),
            ] {
                if value.is_empty()
                    || value.len() > 256
                    || value.contains('\0')
                    || !value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
                {
                    return Err(invalid_effect_input(OPERATION, format!("invalid {name}")));
                }
            }
            if !crate::app_resources::valid_resource_kind(resource_kind) {
                return Err(invalid_effect_input(OPERATION, "invalid resource kind"));
            }
            match media_type {
                Some(media_type)
                    if media_type.is_empty()
                        || media_type.len() > 128
                        || !media_type.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'/' | b'.' | b'+' | b'-')
                        }) =>
                {
                    return Err(invalid_effect_input(OPERATION, "invalid media type"));
                }
                None if resource_kind == "file" => {
                    return Err(invalid_effect_input(
                        OPERATION,
                        "file resources require a media type",
                    ));
                }
                Some(_) if resource_kind != "file" => {
                    return Err(invalid_effect_input(
                        OPERATION,
                        "media type is valid only for file resources",
                    ));
                }
                _ => {}
            }
            if resource_id.is_empty() || resource_id.len() > 4 * 1024 || resource_id.contains('\0')
            {
                return Err(invalid_effect_input(OPERATION, "invalid resource id"));
            }
            if matches!(resource_kind, "file" | "folder" | "git.working-tree")
                && !std::path::Path::new(resource_id).is_absolute()
            {
                return Err(invalid_effect_input(
                    OPERATION,
                    "path-backed resources require an absolute Host path",
                ));
            }
            Ok(())
        })?;
        let body = encode_effect_body(
            OPERATION,
            &AppOpenWire {
                caller_session_id,
                app_id,
                media_type,
                resource: AppOpenResourceWire {
                    kind: resource_kind,
                    id: resource_id,
                },
                request_id,
            },
        )?;
        self.inner.perform_effect(
            &effect_turn,
            OPERATION,
            APPS_OPEN_CAPABILITY,
            APPS_OPEN_PATH,
            body,
        )
    }

    /// Create a Session on the Host (`session.create`). Session creation is
    /// user-initiated from Controller UI; this method is that path. Exactly
    /// like every other effect it is dispatched at most once on the accepted
    /// generation and never replayed.
    pub fn create_session(
        &self,
        request: &RemoteSessionCreateRequest,
    ) -> Result<RemoteCreatedSession, RemoteEffectFailure> {
        const OPERATION: &str = "session create";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || validate_create_request(request))?;
        let body = encode_effect_body(
            OPERATION,
            &CreateSessionWire {
                project_id: &request.project_id,
                preset_id: request.preset_id.as_deref(),
                command: request.command.as_deref(),
                worktree_path: request.worktree_path.as_deref(),
                worktree_branch: request.worktree_branch.as_deref(),
                initial_text: request.initial_text.as_deref(),
                initial_text_submit_mode: request
                    .initial_text
                    .is_some()
                    .then_some(request.initial_text_submit_mode),
            },
        )?;
        let exchange = self.inner.perform_effect_exchange(
            &effect_turn,
            OPERATION,
            CREATE_CAPABILITY,
            SESSIONS_CREATE_PATH,
            &[],
            "application/json",
            body,
            DEFAULT_EFFECT_TIMEOUT,
        )?;
        let response: CreateSessionResponseWire =
            serde_json::from_slice(&exchange.body).map_err(|error| {
                self.inner
                    .effect_receipt_unknown(OPERATION, exchange.generation, error.to_string())
            })?;
        if validate_session_id(&response.session_id).is_err() {
            return Err(self.inner.effect_receipt_unknown(
                OPERATION,
                exchange.generation,
                "created Session id is invalid".to_owned(),
            ));
        }
        Ok(RemoteCreatedSession {
            receipt: RemoteEffectReceipt {
                request_id: exchange.request_id,
            },
            session_id: response.session_id,
            captured_at_unix_ms: response.captured_at_unix_ms,
            session: response.session,
        })
    }

    /// Create or complete a controller-assisted phone pairing invitation.
    /// The body stays opaque here because the Host owns the one-time token
    /// and sealed pairing envelope. Both actions mutate Host authorization,
    /// so they share the normal generation-bound, at-most-once effect path.
    pub fn pairing_invitation(&self, body: &[u8]) -> Result<Vec<u8>, RemoteEffectFailure> {
        const OPERATION: &str = "pairing invitation";
        let effect_turn = self.inner.begin_effect();
        effect_preflight(OPERATION, || {
            if body.is_empty() || body.len() > REMOTE_PAIRING_INVITATION_MAX_BYTES {
                return Err(invalid_effect_input(
                    OPERATION,
                    format!(
                        "body is {} bytes (maximum {REMOTE_PAIRING_INVITATION_MAX_BYTES})",
                        body.len()
                    ),
                ));
            }
            let value: Value = serde_json::from_slice(body).map_err(|error| {
                invalid_effect_input(OPERATION, format!("body is not JSON: {error}"))
            })?;
            let action = value.get("action").and_then(Value::as_str);
            if !matches!(action, Some("create" | "complete")) {
                return Err(invalid_effect_input(
                    OPERATION,
                    "action must be create or complete",
                ));
            }
            Ok(())
        })?;
        let exchange = self.inner.perform_effect_exchange(
            &effect_turn,
            OPERATION,
            PAIRING_INVITATION_CAPABILITY,
            PAIRING_INVITATION_PATH,
            &[],
            "application/json",
            body.to_vec(),
            DEFAULT_EFFECT_TIMEOUT,
        )?;
        Ok(exchange.body)
    }

    /// Upload raw image bytes to the Host (`artifact.upload`, the same
    /// operation the phone's attach flow uses). The Host saves them under the
    /// session's artifacts (or its shared dropped-images dir without a
    /// session) and returns the HOST-side path the Controller pastes into the
    /// terminal. Ordinary at-most-once effect semantics; an ambiguous outcome
    /// is never replayed automatically (a duplicate would only orphan a file,
    /// but the paste must reference exactly one).
    pub fn upload_attachment(
        &self,
        session_id: Option<&str>,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<RemoteUploadedAttachment, RemoteEffectFailure> {
        const OPERATION: &str = "attachment upload";
        let effect_turn = self.inner.begin_effect();
        // The Host derives the stored extension from the content type; only
        // the two image types the shipped route understands are offered.
        let wire_content_type: Option<&'static str> = match content_type {
            "image/png" => Some("image/png"),
            "image/jpeg" => Some("image/jpeg"),
            _ => None,
        };
        effect_preflight(OPERATION, || {
            if wire_content_type.is_none() {
                return Err(invalid_effect_input(
                    OPERATION,
                    format!("unsupported content type: {content_type}"),
                ));
            }
            if let Some(session_id) = session_id {
                validate_session_id(session_id)?;
            }
            if bytes.is_empty() {
                return Err(invalid_effect_input(OPERATION, "upload is empty"));
            }
            if bytes.len() > REMOTE_UPLOAD_MAX_BYTES {
                return Err(invalid_effect_input(
                    OPERATION,
                    format!(
                        "upload is {} bytes (maximum {REMOTE_UPLOAD_MAX_BYTES})",
                        bytes.len()
                    ),
                ));
            }
            Ok(())
        })?;
        let mut query: Vec<(&'static str, String)> = Vec::new();
        if let Some(session_id) = session_id {
            query.push(("session_id", session_id.to_owned()));
        }
        let exchange = self.inner.perform_effect_exchange(
            &effect_turn,
            OPERATION,
            UPLOAD_CAPABILITY,
            UPLOAD_PATH,
            &query,
            wire_content_type.expect("validated in preflight"),
            bytes,
            DEFAULT_EFFECT_TIMEOUT,
        )?;
        let response: UploadResponseWire =
            serde_json::from_slice(&exchange.body).map_err(|error| {
                self.inner
                    .effect_receipt_unknown(OPERATION, exchange.generation, error.to_string())
            })?;
        if response.path.is_empty() || !response.path.starts_with('/') {
            return Err(self.inner.effect_receipt_unknown(
                OPERATION,
                exchange.generation,
                "upload receipt did not name an absolute Host path".to_owned(),
            ));
        }
        Ok(RemoteUploadedAttachment {
            receipt: RemoteEffectReceipt {
                request_id: exchange.request_id,
            },
            path: response.path,
        })
    }

    /// List one project's archived Sessions (`session.archive.list`) — a
    /// capability-gated, generation-bound read, not an effect.
    pub fn list_archived_sessions(
        &self,
        project_id: &str,
    ) -> Result<Vec<RemoteSessionSummary>, RemoteSessionBackendError> {
        const OPERATION: &str = "archived Sessions";
        validate_project_id(project_id)?;
        let body = self.inner.perform_read(
            OPERATION,
            ARCHIVE_LIST_CAPABILITY,
            ARCHIVE_LIST_PATH,
            &[("project_id", project_id.to_owned())],
        )?;
        let wire: ArchivedSessionsWire = serde_json::from_slice(&body).map_err(|error| {
            RemoteSessionBackendError::InvalidResponse {
                operation: OPERATION,
                message: error.to_string(),
            }
        })?;
        if wire.project_id != project_id {
            return Err(RemoteSessionBackendError::InvalidResponse {
                operation: OPERATION,
                message: "response project id does not match request".to_owned(),
            });
        }
        Ok(wire.sessions)
    }

    /// Read a Session's rendered conversation transcript
    /// (`session.transcript.markdown`) — a capability-gated,
    /// generation-bound read, not an effect. `entries` limits the transcript
    /// to the most recent N entries; `None` uses the Host's setting.
    pub fn read_transcript_markdown(
        &self,
        session_id: &str,
        entries: Option<u32>,
    ) -> Result<RemoteTranscriptMarkdown, RemoteSessionBackendError> {
        const OPERATION: &str = "transcript";
        validate_session_id(session_id)?;
        let mut query = vec![("session_id", session_id.to_owned())];
        if let Some(entries) = entries {
            query.push(("entries", entries.to_string()));
        }
        let body = self.inner.perform_read(
            OPERATION,
            TRANSCRIPT_MARKDOWN_CAPABILITY,
            TRANSCRIPT_MARKDOWN_PATH,
            &query,
        )?;
        let wire: TranscriptMarkdownWire = serde_json::from_slice(&body).map_err(|error| {
            RemoteSessionBackendError::InvalidResponse {
                operation: OPERATION,
                message: error.to_string(),
            }
        })?;
        if wire.session_id != session_id {
            return Err(RemoteSessionBackendError::InvalidResponse {
                operation: OPERATION,
                message: "response Session id does not match request".to_owned(),
            });
        }
        Ok(RemoteTranscriptMarkdown {
            session_id: wire.session_id,
            markdown: wire.markdown,
        })
    }

    /// Read one live Session's current terminal grid
    /// (`session.metrics.read`) — a capability-gated, generation-bound read,
    /// not an effect. The Host answers 409 for an exited Session.
    pub fn read_session_metrics(
        &self,
        session_id: &str,
    ) -> Result<RemoteSessionMetrics, RemoteSessionBackendError> {
        const OPERATION: &str = "session metrics";
        validate_session_id(session_id)?;
        let body = self.inner.perform_read(
            OPERATION,
            METRICS_CAPABILITY,
            METRICS_PATH,
            &[("session_id", session_id.to_owned())],
        )?;
        let wire: SessionMetricsWire = serde_json::from_slice(&body).map_err(|error| {
            RemoteSessionBackendError::InvalidResponse {
                operation: OPERATION,
                message: error.to_string(),
            }
        })?;
        if wire.session_id != session_id {
            return Err(RemoteSessionBackendError::InvalidResponse {
                operation: OPERATION,
                message: "response Session id does not match request".to_owned(),
            });
        }
        Ok(RemoteSessionMetrics {
            session_id: wire.session_id,
            columns: wire.columns,
            rows: wire.rows,
            output_offset: wire.output_offset,
            captured_at_unix_ms: wire.captured_at_unix_ms,
        })
    }

    pub fn disconnect(&self) {
        self.inner.connection.disconnect();
        self.inner.invalidate_all();
    }
}

impl BackendInner {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, BackendState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_bootstrap_gate(&self) -> std::sync::MutexGuard<'_, ()> {
        self.bootstrap_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn begin_effect(&self) -> EffectTurn<'_> {
        self.effect_sequence.enter()
    }

    fn refresh_bootstrap(&self) -> Result<RemoteBootstrap, RemoteSessionBackendError> {
        let observed_revision = self.lock_state().bootstrap_revision;
        self.refresh_bootstrap_observed(observed_revision)
    }

    fn refresh_bootstrap_observed(
        &self,
        observed_revision: u64,
    ) -> Result<RemoteBootstrap, RemoteSessionBackendError> {
        let _gate = self.lock_bootstrap_gate();
        {
            let state = self.lock_state();
            if state.bootstrap_revision != observed_revision && state.accepted_generation.is_some()
            {
                if let Some(bootstrap) = &state.last_bootstrap {
                    return Ok(bootstrap.clone());
                }
            }
        }
        // Invalidation/disconnect may run while transport I/O is in flight.
        // A reply may only publish if the callable-generation epoch is still
        // the one under which this bootstrap was dispatched.
        let dispatch_revision = self.lock_state().bootstrap_revision;

        let call = match self.connection.prepare(HostCall::new(
            "GET",
            BOOTSTRAP_PATH,
            RequestSemantics::ReadOnly,
        )) {
            Ok(call) => call,
            Err(error) => {
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_all();
                }
                return Err(error.into());
            }
        };
        let reply = match self.connection.request(call, self.bootstrap_timeout) {
            Ok(reply) => reply,
            Err(error) => {
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_all();
                }
                return Err(error.into());
            }
        };
        if reply.status != 200 {
            self.invalidate_all();
            return Err(host_status("bootstrap", reply.status, &reply.body));
        }
        let raw: Value = match serde_json::from_slice(&reply.body) {
            Ok(raw) => raw,
            Err(error) => {
                self.invalidate_all();
                return Err(RemoteSessionBackendError::InvalidResponse {
                    operation: "bootstrap",
                    message: error.to_string(),
                });
            }
        };
        let snapshot: RemoteBootstrapSnapshot = match serde_json::from_value(raw.clone()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.invalidate_all();
                return Err(RemoteSessionBackendError::InvalidResponse {
                    operation: "bootstrap",
                    message: error.to_string(),
                });
            }
        };
        if let Err(error) = validate_bootstrap(&snapshot) {
            self.invalidate_all();
            return Err(error);
        }

        let value = RemoteBootstrap { snapshot, raw };
        let mut state = self.lock_state();
        if state.bootstrap_revision != dispatch_revision {
            return Err(RemoteSessionBackendError::BootstrapChanged);
        }
        if let Some(expected) = state.pinned_host_id.clone() {
            if value.snapshot.host_id.as_ref() != Some(&expected) {
                let received = value.snapshot.host_id.clone();
                state.accepted_generation = None;
                advance_bootstrap_revision(&mut state)?;
                clear_fetching_pages(&mut state, None);
                return Err(RemoteSessionBackendError::HostIdentityChanged { expected, received });
            }
        } else if let Some(host_id) = &value.snapshot.host_id {
            state.pinned_host_id = Some(host_id.clone());
        }

        if state.accepted_generation != Some(reply.generation) {
            clear_fetching_pages(&mut state, None);
        }
        state.last_bootstrap = Some(value.clone());
        state.accepted_generation = Some(reply.generation);
        advance_bootstrap_revision(&mut state)?;
        Ok(value)
    }

    fn ensure_bootstrap(&self) -> Result<AcceptedBootstrap, RemoteSessionBackendError> {
        {
            let state = self.lock_state();
            if let (Some(generation), Some(value)) =
                (state.accepted_generation, state.last_bootstrap.clone())
            {
                return Ok(AcceptedBootstrap { generation, value });
            }
        }
        self.refresh_bootstrap()?;
        let state = self.lock_state();
        match (state.accepted_generation, state.last_bootstrap.clone()) {
            (Some(generation), Some(value)) => Ok(AcceptedBootstrap { generation, value }),
            _ => Err(RemoteSessionBackendError::BootstrapChanged),
        }
    }

    fn poll_output(
        self: &Arc<Self>,
        session_id: &str,
        options: RemoteOutputPollOptions,
        requested_cursor: OutputPollCursor,
    ) -> Result<RemoteOutputPage, RemoteSessionBackendError> {
        let accepted = self.ensure_bootstrap()?;
        if !accepted.value.snapshot.supports(OUTPUT_CAPABILITY) {
            return Err(RemoteSessionBackendError::MissingCapability(
                OUTPUT_CAPABILITY.to_owned(),
            ));
        }
        let timeout = options
            .wait
            .checked_add(self.output_headroom)
            .ok_or_else(|| {
                RemoteSessionBackendError::InvalidOutputOptions(
                    "wait plus response headroom overflows".to_owned(),
                )
            })?;
        let token = {
            let mut state = self.lock_state();
            if state.accepted_generation != Some(accepted.generation) {
                return Err(RemoteSessionBackendError::BootstrapChanged);
            }
            let next_page_id = state
                .next_page_id
                .checked_add(1)
                .ok_or(RemoteSessionBackendError::StateExhausted)?;
            let page_id = state.next_page_id;
            state.next_page_id = next_page_id;
            let cursor = state.cursors.entry(session_id.to_owned()).or_default();
            if let OutputPollCursor::From(requested_offset) = requested_cursor {
                cursor.version = cursor
                    .version
                    .checked_add(1)
                    .ok_or(RemoteSessionBackendError::StateExhausted)?;
                cursor.committed = requested_offset;
                cursor.pending = None;
            }
            if cursor.pending.is_some() {
                return Err(RemoteSessionBackendError::OutputPagePending(
                    session_id.to_owned(),
                ));
            }
            let token = OutputPageToken {
                backend_id: self.id,
                session_id: session_id.to_owned(),
                page_id,
                cursor_version: cursor.version,
                generation: accepted.generation,
                requested_offset: cursor.committed,
                next_offset: 0,
            };
            cursor.pending = Some(PendingOutput::Fetching {
                page_id,
                generation: accepted.generation,
            });
            token
        };

        let mut call = HostCall::new("GET", OUTPUT_PATH, RequestSemantics::ReadOnly)
            .with_query("session_id", session_id)
            .with_query("limit", options.limit.to_string());
        if let Some(offset) = token.requested_offset {
            call = call.with_query("offset", offset.to_string());
        }
        if !options.wait.is_zero() {
            call = call.with_query("wait_ms", options.wait.as_millis().to_string());
        }
        let call = match self
            .connection
            .prepare_in_generation(accepted.generation, call)
        {
            Ok(call) => call,
            Err(error) => {
                self.clear_page_reservation(&token);
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_generation(accepted.generation);
                }
                return Err(error.into());
            }
        };
        let reply = match self.connection.request(call, timeout) {
            Ok(reply) => reply,
            Err(error) => {
                self.clear_page_reservation(&token);
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_generation(accepted.generation);
                }
                return Err(error.into());
            }
        };
        if reply.generation != accepted.generation {
            self.clear_page_reservation(&token);
            self.invalidate_generation(accepted.generation);
            return Err(RemoteSessionBackendError::BootstrapChanged);
        }
        if reply.status != 200 {
            self.clear_page_reservation(&token);
            return Err(host_status("output", reply.status, &reply.body));
        }
        let wire = match decode_output_chunk(
            &reply.body,
            session_id,
            token.requested_offset,
            options.limit,
        ) {
            Ok(wire) => wire,
            Err(error) => {
                self.clear_page_reservation(&token);
                return Err(error);
            }
        };

        let mut token = token;
        token.next_offset = wire.next_offset;
        {
            let mut state = self.lock_state();
            if state.accepted_generation != Some(accepted.generation) {
                clear_matching_page(&mut state, &token);
                return Err(RemoteSessionBackendError::BootstrapChanged);
            }
            let Some(cursor) = state.cursors.get_mut(session_id) else {
                return Err(RemoteSessionBackendError::BootstrapChanged);
            };
            if cursor.version != token.cursor_version
                || cursor.committed != token.requested_offset
                || cursor.pending
                    != Some(PendingOutput::Fetching {
                        page_id: token.page_id,
                        generation: token.generation,
                    })
            {
                return Err(RemoteSessionBackendError::BootstrapChanged);
            }
            cursor.pending = Some(PendingOutput::Staged {
                page_id: token.page_id,
                generation: token.generation,
            });
        }

        Ok(RemoteOutputPage {
            token: Some(token.clone()),
            backend: Arc::downgrade(self),
            session_id: session_id.to_owned(),
            requested_offset: token.requested_offset,
            offset: wire.offset,
            next_offset: wire.next_offset,
            bytes: wire.bytes,
            reset_before_feed: token
                .requested_offset
                .map(|offset| offset != wire.offset)
                .unwrap_or(true)
                || wire.truncated,
            truncated: wire.truncated,
            captured_at_unix_ms: wire.captured_at_unix_ms,
        })
    }

    fn perform_effect(
        &self,
        effect_turn: &EffectTurn<'_>,
        operation: &'static str,
        capability: &'static str,
        path: &'static str,
        body: Vec<u8>,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        self.perform_effect_with_timeout(
            effect_turn,
            operation,
            capability,
            path,
            body,
            DEFAULT_EFFECT_TIMEOUT,
        )
    }

    fn perform_effect_with_timeout(
        &self,
        effect_turn: &EffectTurn<'_>,
        operation: &'static str,
        capability: &'static str,
        path: &'static str,
        body: Vec<u8>,
        timeout: Duration,
    ) -> Result<RemoteEffectReceipt, RemoteEffectFailure> {
        let exchange = self.perform_effect_exchange(
            effect_turn,
            operation,
            capability,
            path,
            &[],
            "application/json",
            body,
            timeout,
        )?;
        let receipt: EffectReceiptWire =
            serde_json::from_slice(&exchange.body).map_err(|error| {
                self.effect_receipt_unknown(operation, exchange.generation, error.to_string())
            })?;
        if !receipt.ok {
            return Err(self.effect_receipt_unknown(
                operation,
                exchange.generation,
                "success receipt did not contain ok=true".to_owned(),
            ));
        }
        Ok(RemoteEffectReceipt {
            request_id: exchange.request_id,
        })
    }

    /// The whole shared effect pipeline — ordering, generation binding,
    /// capability recheck, correlation, and status semantics — up to but not
    /// including success-body parsing, for effects whose receipt carries more
    /// than `ok:true`.
    #[allow(clippy::too_many_arguments)]
    fn perform_effect_exchange(
        &self,
        _effect_turn: &EffectTurn<'_>,
        operation: &'static str,
        capability: &'static str,
        path: &'static str,
        query: &[(&'static str, String)],
        content_type: &'static str,
        body: Vec<u8>,
        timeout: Duration,
    ) -> Result<EffectExchange, RemoteEffectFailure> {
        let accepted = self.ensure_bootstrap().map_err(|error| {
            effect_failure(operation, RemoteEffectFailureKind::NotApplied, error)
        })?;
        // Bootstrap refresh and effects share this gate. Recheck callable
        // state after acquiring it so an identity/generation/capability change
        // cannot race a call prepared from an older accepted snapshot. A
        // same-generation catalog refresh is harmless: use its latest
        // capability ledger instead of rejecting a keystroke merely because
        // Session status changed while this effect waited for the gate.
        let _bootstrap_gate = self.lock_bootstrap_gate();
        let latest = {
            let state = self.lock_state();
            match (&state.last_bootstrap, state.accepted_generation) {
                (Some(value), Some(generation)) if generation == accepted.generation => {
                    value.clone()
                }
                _ => {
                    return Err(effect_failure(
                        operation,
                        RemoteEffectFailureKind::NotApplied,
                        RemoteSessionBackendError::BootstrapChanged,
                    ));
                }
            }
        };
        if !latest.snapshot.supports(capability) {
            return Err(effect_failure(
                operation,
                RemoteEffectFailureKind::NotApplied,
                RemoteSessionBackendError::MissingCapability(capability.to_owned()),
            ));
        }
        let mut call = HostCall::new("POST", path, RequestSemantics::Effect);
        for (key, value) in query {
            call = call.with_query(*key, value.clone());
        }
        let call = call.with_body(content_type, body);
        let call = match self
            .connection
            .prepare_in_generation(accepted.generation, call)
        {
            Ok(call) => call,
            Err(error) => {
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_generation(accepted.generation);
                }
                return Err(effect_failure(
                    operation,
                    RemoteEffectFailureKind::NotApplied,
                    error.into(),
                ));
            }
        };
        let request_id = call.request_id();
        let reply = match self.connection.request(call, timeout) {
            Ok(reply) => reply,
            Err(error) => {
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_generation(accepted.generation);
                }
                let kind = if error.effect_outcome_is_unknown() {
                    RemoteEffectFailureKind::OutcomeUnknown
                } else {
                    RemoteEffectFailureKind::NotApplied
                };
                return Err(effect_failure(operation, kind, error.into()));
            }
        };
        if reply.request_id != request_id {
            self.invalidate_generation(accepted.generation);
            return Err(effect_failure(
                operation,
                RemoteEffectFailureKind::OutcomeUnknown,
                RemoteSessionBackendError::InvalidResponse {
                    operation,
                    message: format!(
                        "reply request id {} did not match {request_id}",
                        reply.request_id
                    ),
                },
            ));
        }
        if reply.generation != accepted.generation {
            self.invalidate_generation(accepted.generation);
            return Err(effect_failure(
                operation,
                RemoteEffectFailureKind::OutcomeUnknown,
                RemoteSessionBackendError::BootstrapChanged,
            ));
        }
        if reply.status != 200 {
            // The response is correlated to this exact request and generation,
            // so semantic rejection is materially different from losing the
            // receipt. The shared router emits Host 4xx only before dispatch
            // (validation/resource state), and the common transports reserve
            // 503 for bounded pre-dispatch saturation. Any failure after the
            // session control command is attempted is a 5xx because the PTY
            // may have applied it before its acknowledgement was lost. Keep
            // the healthy generation callable only for the proven cases; the
            // backend itself still never replays an effect.
            let kind = correlated_effect_failure_kind(reply.status);
            if kind == RemoteEffectFailureKind::OutcomeUnknown {
                self.invalidate_generation(accepted.generation);
            }
            return Err(effect_failure(
                operation,
                kind,
                host_status(operation, reply.status, &reply.body),
            ));
        }
        Ok(EffectExchange {
            request_id,
            generation: accepted.generation,
            body: reply.body,
        })
    }

    /// A correlated 200 arrived but its success body could not be trusted:
    /// the effect may have landed, so the generation is torn down and the
    /// failure reads as outcome-unknown.
    fn effect_receipt_unknown(
        &self,
        operation: &'static str,
        generation: ConnectionGeneration,
        message: String,
    ) -> RemoteEffectFailure {
        self.invalidate_generation(generation);
        effect_failure(
            operation,
            RemoteEffectFailureKind::OutcomeUnknown,
            RemoteSessionBackendError::InvalidResponse { operation, message },
        )
    }

    /// A capability-gated, generation-bound GET beyond the two shipped v1
    /// reads. Mirrors `poll_output`'s transport handling without cursor
    /// state: a non-200 keeps the healthy generation; transport loss or a
    /// cross-generation reply invalidates it.
    fn perform_read(
        &self,
        operation: &'static str,
        capability: &'static str,
        path: &'static str,
        query: &[(&'static str, String)],
    ) -> Result<Vec<u8>, RemoteSessionBackendError> {
        let accepted = self.ensure_bootstrap()?;
        if !accepted.value.snapshot.supports(capability) {
            return Err(RemoteSessionBackendError::MissingCapability(
                capability.to_owned(),
            ));
        }
        let mut call = HostCall::new("GET", path, RequestSemantics::ReadOnly);
        for (key, value) in query {
            call = call.with_query(*key, value.clone());
        }
        let call = match self
            .connection
            .prepare_in_generation(accepted.generation, call)
        {
            Ok(call) => call,
            Err(error) => {
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_generation(accepted.generation);
                }
                return Err(error.into());
            }
        };
        let reply = match self.connection.request(call, self.bootstrap_timeout) {
            Ok(reply) => reply,
            Err(error) => {
                if connection_error_invalidates_generation(&error) {
                    self.invalidate_generation(accepted.generation);
                }
                return Err(error.into());
            }
        };
        if reply.generation != accepted.generation {
            self.invalidate_generation(accepted.generation);
            return Err(RemoteSessionBackendError::BootstrapChanged);
        }
        if reply.status != 200 {
            return Err(host_status(operation, reply.status, &reply.body));
        }
        Ok(reply.body)
    }

    fn commit_page(&self, token: &OutputPageToken) -> Result<(), RemoteSessionBackendError> {
        let mut state = self.lock_state();
        if token.backend_id != self.id {
            return Err(RemoteSessionBackendError::StaleOutputPage);
        }
        let cursor = state
            .cursors
            .get_mut(&token.session_id)
            .ok_or(RemoteSessionBackendError::StaleOutputPage)?;
        if cursor.version != token.cursor_version
            || cursor.committed != token.requested_offset
            || cursor.pending
                != Some(PendingOutput::Staged {
                    page_id: token.page_id,
                    generation: token.generation,
                })
        {
            return Err(RemoteSessionBackendError::StaleOutputPage);
        }
        cursor.committed = Some(token.next_offset);
        cursor.pending = None;
        Ok(())
    }

    fn discard_page(&self, token: &OutputPageToken) {
        if token.backend_id != self.id {
            return;
        }
        let mut state = self.lock_state();
        clear_matching_page(&mut state, token);
    }

    fn clear_page_reservation(&self, token: &OutputPageToken) {
        let mut state = self.lock_state();
        clear_matching_page(&mut state, token);
    }

    fn invalidate_generation(&self, generation: ConnectionGeneration) {
        let mut state = self.lock_state();
        if state.accepted_generation != Some(generation) {
            return;
        }
        state.accepted_generation = None;
        let _ = advance_bootstrap_revision(&mut state);
        clear_fetching_pages(&mut state, Some(generation));
    }

    fn invalidate_all(&self) {
        let mut state = self.lock_state();
        state.accepted_generation = None;
        let _ = advance_bootstrap_revision(&mut state);
        clear_fetching_pages(&mut state, None);
    }
}

fn advance_bootstrap_revision(state: &mut BackendState) -> Result<(), RemoteSessionBackendError> {
    state.bootstrap_revision = state
        .bootstrap_revision
        .checked_add(1)
        .ok_or(RemoteSessionBackendError::StateExhausted)?;
    Ok(())
}

#[derive(Debug)]
struct DecodedOutputChunk {
    offset: u64,
    next_offset: u64,
    bytes: Vec<u8>,
    truncated: bool,
    captured_at_unix_ms: i64,
}

fn decode_output_chunk(
    body: &[u8],
    session_id: &str,
    requested_offset: Option<u64>,
    requested_limit: usize,
) -> Result<DecodedOutputChunk, RemoteSessionBackendError> {
    let wire: OutputChunkWire = serde_json::from_slice(body).map_err(|error| {
        RemoteSessionBackendError::InvalidResponse {
            operation: "output",
            message: error.to_string(),
        }
    })?;
    if wire.session_id != session_id {
        return Err(invalid_output("response Session id does not match request"));
    }
    if wire.captured_at_unix_ms < 0 {
        return Err(invalid_output("capturedAtUnixMs must not be negative"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&wire.data_base64)
        .map_err(|error| invalid_output(&format!("invalid base64: {error}")))?;
    if base64::engine::general_purpose::STANDARD.encode(&bytes) != wire.data_base64 {
        return Err(invalid_output(
            "base64 is not canonical padded standard form",
        ));
    }
    let rebased =
        requested_offset.is_some_and(|requested| requested != wire.offset) || wire.truncated;
    let allowed_bytes = if requested_offset.is_none() || rebased {
        requested_limit
            .checked_add(INITIAL_TAIL_ALIGNMENT_ALLOWANCE)
            .ok_or(RemoteSessionBackendError::StateExhausted)?
    } else {
        requested_limit
    };
    if bytes.len() > allowed_bytes {
        return Err(invalid_output("response exceeds requested byte limit"));
    }
    let expected_next = wire
        .offset
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| invalid_output("offset plus payload length overflows"))?;
    if wire.next_offset != expected_next {
        return Err(invalid_output(
            "nextOffset does not equal offset plus decoded bytes",
        ));
    }
    Ok(DecodedOutputChunk {
        offset: wire.offset,
        next_offset: wire.next_offset,
        bytes,
        truncated: wire.truncated,
        captured_at_unix_ms: wire.captured_at_unix_ms,
    })
}

fn clear_matching_page(state: &mut BackendState, token: &OutputPageToken) {
    let Some(cursor) = state.cursors.get_mut(&token.session_id) else {
        return;
    };
    let matching_id = match cursor.pending {
        Some(PendingOutput::Fetching {
            page_id,
            generation,
        })
        | Some(PendingOutput::Staged {
            page_id,
            generation,
        }) => page_id == token.page_id && generation == token.generation,
        None => false,
    };
    if cursor.version == token.cursor_version && matching_id {
        cursor.pending = None;
    }
}

fn clear_fetching_pages(state: &mut BackendState, generation: Option<ConnectionGeneration>) {
    for cursor in state.cursors.values_mut() {
        let clear = match cursor.pending {
            Some(PendingOutput::Fetching {
                generation: pending_generation,
                ..
            }) => generation
                .map(|generation| generation == pending_generation)
                .unwrap_or(true),
            _ => false,
        };
        if clear {
            cursor.pending = None;
        }
    }
}

fn validate_bootstrap(snapshot: &RemoteBootstrapSnapshot) -> Result<(), RemoteSessionBackendError> {
    if snapshot.protocol_version != MOBILE_PROTOCOL_VERSION {
        return Err(RemoteSessionBackendError::UnsupportedMobileProtocol {
            advertised: snapshot.protocol_version,
            required: MOBILE_PROTOCOL_VERSION,
        });
    }
    if snapshot.captured_at_unix_ms < 0 {
        return Err(RemoteSessionBackendError::InvalidResponse {
            operation: "bootstrap",
            message: "capturedAtUnixMs must not be negative".to_owned(),
        });
    }
    if let Some(host_id) = &snapshot.host_id {
        if host_id.is_empty()
            || host_id.len() > MAX_HOST_ID_BYTES
            || host_id.chars().any(char::is_control)
        {
            return Err(RemoteSessionBackendError::InvalidResponse {
                operation: "bootstrap",
                message: "macID is invalid".to_owned(),
            });
        }
    }
    if let Some(protocol) = &snapshot.host_protocol {
        if !protocol.is_compatible_with(HOST_PROTOCOL_MAJOR) {
            return Err(RemoteSessionBackendError::IncompatibleHostProtocol {
                advertised_major: protocol.major_version,
                advertised_minor: protocol.minor_version,
                required_major: HOST_PROTOCOL_MAJOR,
            });
        }
        if !protocol.supports(BOOTSTRAP_CAPABILITY) {
            return Err(RemoteSessionBackendError::MissingCapability(
                BOOTSTRAP_CAPABILITY.to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_session_id(session_id: &str) -> Result<(), RemoteSessionBackendError> {
    if session_id.is_empty()
        || session_id.len() > MAX_SESSION_ID_BYTES
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
        || session_id.contains('\0')
    {
        return Err(RemoteSessionBackendError::InvalidSessionId);
    }
    Ok(())
}

fn validate_project_id(project_id: &str) -> Result<(), RemoteSessionBackendError> {
    if project_id.is_empty()
        || project_id.len() > MAX_PROJECT_ID_BYTES
        || project_id.contains('/')
        || project_id.contains('\\')
        || project_id.contains("..")
        || project_id.contains('\0')
    {
        return Err(RemoteSessionBackendError::InvalidProjectId);
    }
    Ok(())
}

fn validate_create_request(
    request: &RemoteSessionCreateRequest,
) -> Result<(), RemoteSessionBackendError> {
    const OPERATION: &str = "session create";
    validate_project_id(&request.project_id)?;
    if let Some(preset_id) = request.preset_id.as_deref() {
        if preset_id.trim().is_empty()
            || preset_id.len() > MAX_PRESET_ID_BYTES
            || preset_id.contains('\0')
        {
            return Err(invalid_effect_input(OPERATION, "invalid preset id"));
        }
    }
    if request.preset_id.is_none() {
        let Some(command) = request.command.as_deref() else {
            return Err(invalid_effect_input(
                OPERATION,
                "missing preset id or command",
            ));
        };
        // An exactly empty command is the protocol representation of the
        // built-in blank Terminal. Reject accidental whitespace-only input,
        // but allow Controllers to ask the Host for its login shell.
        if !command.is_empty() && command.trim().is_empty() {
            return Err(invalid_effect_input(OPERATION, "command is empty"));
        }
    }
    if let Some(command) = request.command.as_deref() {
        if command.len() > REMOTE_CREATE_COMMAND_MAX_BYTES || command.contains('\0') {
            return Err(invalid_effect_input(OPERATION, "invalid command"));
        }
    }
    for (value, max_bytes, field) in [
        (
            request.worktree_path.as_deref(),
            REMOTE_CREATE_PATH_MAX_BYTES,
            "worktree path",
        ),
        (
            request.worktree_branch.as_deref(),
            REMOTE_CREATE_BRANCH_MAX_BYTES,
            "worktree branch",
        ),
    ] {
        if let Some(value) = value {
            if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
                return Err(invalid_effect_input(OPERATION, format!("invalid {field}")));
            }
        }
    }
    if request
        .initial_text
        .as_deref()
        .is_some_and(|text| text.len() > REMOTE_CREATE_INITIAL_TEXT_MAX_BYTES)
    {
        return Err(invalid_effect_input(OPERATION, "initial text too large"));
    }
    Ok(())
}

fn validate_output_options(
    options: RemoteOutputPollOptions,
) -> Result<(), RemoteSessionBackendError> {
    if options.limit == 0 || options.limit > REMOTE_OUTPUT_MAX_LIMIT {
        return Err(RemoteSessionBackendError::InvalidOutputOptions(format!(
            "limit must be between 1 and {REMOTE_OUTPUT_MAX_LIMIT}"
        )));
    }
    if options.wait > REMOTE_OUTPUT_MAX_WAIT {
        return Err(RemoteSessionBackendError::InvalidOutputOptions(format!(
            "wait must not exceed {}ms",
            REMOTE_OUTPUT_MAX_WAIT.as_millis()
        )));
    }
    Ok(())
}

fn invalid_effect_input(
    operation: &'static str,
    message: impl Into<String>,
) -> RemoteSessionBackendError {
    RemoteSessionBackendError::InvalidEffectInput {
        operation,
        message: message.into(),
    }
}

fn effect_failure(
    operation: &'static str,
    kind: RemoteEffectFailureKind,
    error: RemoteSessionBackendError,
) -> RemoteEffectFailure {
    RemoteEffectFailure {
        operation,
        kind,
        error,
    }
}

fn correlated_effect_failure_kind(status: u16) -> RemoteEffectFailureKind {
    if (400..=499).contains(&status) || status == 503 {
        RemoteEffectFailureKind::NotApplied
    } else {
        RemoteEffectFailureKind::OutcomeUnknown
    }
}

fn effect_preflight<T>(
    operation: &'static str,
    check: impl FnOnce() -> Result<T, RemoteSessionBackendError>,
) -> Result<T, RemoteEffectFailure> {
    check().map_err(|error| effect_failure(operation, RemoteEffectFailureKind::NotApplied, error))
}

fn encode_effect_body(
    operation: &'static str,
    value: &impl Serialize,
) -> Result<Vec<u8>, RemoteEffectFailure> {
    serde_json::to_vec(value).map_err(|error| {
        effect_failure(
            operation,
            RemoteEffectFailureKind::NotApplied,
            invalid_effect_input(operation, format!("could not encode request: {error}")),
        )
    })
}

fn connection_error_invalidates_generation(error: &HostConnectionError) -> bool {
    matches!(
        error,
        HostConnectionError::Closed
            | HostConnectionError::ClosedRequest(_)
            | HostConnectionError::Launch { .. }
            | HostConnectionError::RequestIdExhausted
            | HostConnectionError::WrongConnection(_)
            | HostConnectionError::WrongGeneration(_)
            | HostConnectionError::GenerationChanged { .. }
            | HostConnectionError::Disconnected { .. }
            | HostConnectionError::TimedOut { .. }
    )
}

fn host_status(operation: &'static str, status: u16, body: &[u8]) -> RemoteSessionBackendError {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| (!body.is_empty()).then(|| String::from_utf8_lossy(body).into_owned()))
        .map(|message| safe_diagnostic(&message, 240));
    RemoteSessionBackendError::HostStatus {
        operation,
        status,
        message,
    }
}

fn invalid_output(message: &str) -> RemoteSessionBackendError {
    RemoteSessionBackendError::InvalidResponse {
        operation: "output",
        message: message.to_owned(),
    }
}

fn safe_diagnostic(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut characters = value.chars();
    for _ in 0..max_chars {
        let Some(character) = characters.next() else {
            return output;
        };
        output.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    if characters.next().is_some() {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Barrier};
    use std::thread;

    use serde_json::json;

    use crate::host_connection::{DeliveryState, HostReply, PreparedHostCall, RequestSemantics};

    #[derive(Debug, Clone)]
    struct ExpectedCall {
        generation: Option<ConnectionGeneration>,
        method: &'static str,
        path: &'static str,
        query: Vec<(String, String)>,
        content_type: Option<&'static str>,
        body: Option<Value>,
        semantics: RequestSemantics,
    }

    #[derive(Debug, Clone)]
    enum ScriptOutcome {
        Reply {
            generation: ConnectionGeneration,
            status: u16,
            body: Vec<u8>,
        },
        MismatchedReplyId {
            generation: ConnectionGeneration,
            status: u16,
            body: Vec<u8>,
        },
        PrepareGenerationChanged,
        Disconnect(DeliveryState),
        Timeout(DeliveryState),
        Launch,
        RequestTooLarge,
        TooManyInFlight,
    }

    #[derive(Debug, Clone)]
    struct ScriptStep {
        expected: ExpectedCall,
        outcome: ScriptOutcome,
    }

    struct ScriptedConnection {
        id: uuid::Uuid,
        next_request_id: AtomicU64,
        closed: AtomicBool,
        steps: Mutex<VecDeque<ScriptStep>>,
        calls: Mutex<Vec<ExpectedCall>>,
        timeouts: Mutex<Vec<Duration>>,
    }

    impl ScriptedConnection {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                id: uuid::Uuid::new_v4(),
                next_request_id: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                steps: Mutex::new(VecDeque::new()),
                calls: Mutex::new(Vec::new()),
                timeouts: Mutex::new(Vec::new()),
            })
        }

        fn generation(&self, sequence: u64) -> ConnectionGeneration {
            ConnectionGeneration {
                connection_id: self.id,
                sequence,
            }
        }

        fn push(&self, step: ScriptStep) {
            self.steps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push_back(step);
        }

        fn remaining(&self) -> usize {
            self.steps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
        }

        fn calls(&self) -> Vec<ExpectedCall> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn timeouts(&self) -> Vec<Duration> {
            self.timeouts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn prepare_call(
            &self,
            generation: Option<ConnectionGeneration>,
            call: HostCall,
        ) -> Result<PreparedHostCall, HostConnectionError> {
            if self.closed.load(Ordering::Acquire) {
                return Err(HostConnectionError::Closed);
            }
            let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
            let step = self
                .steps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .front()
                .cloned()
                .expect("unexpected Host call");
            assert_eq!(step.expected.generation, generation);
            assert_eq!(step.expected.method, call.method);
            assert_eq!(step.expected.path, call.path);
            assert_eq!(step.expected.query, call.query);
            assert_eq!(step.expected.content_type, call.content_type.as_deref());
            match &step.expected.body {
                Some(expected) => assert_eq!(
                    expected,
                    &serde_json::from_slice::<Value>(&call.body).expect("effect body is JSON")
                ),
                None => assert!(call.body.is_empty()),
            }
            assert_eq!(step.expected.semantics, call.semantics);
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(step.expected.clone());
            if matches!(step.outcome, ScriptOutcome::PrepareGenerationChanged) {
                self.steps
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pop_front();
                return Err(HostConnectionError::GenerationChanged {
                    request_id,
                    expected: generation.expect("generation-bound failure"),
                });
            }
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id,
                required_generation: generation,
                call,
            })
        }
    }

    impl HostConnection for ScriptedConnection {
        fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
            self.prepare_call(None, call)
        }

        fn prepare_in_generation(
            &self,
            generation: ConnectionGeneration,
            call: HostCall,
        ) -> Result<PreparedHostCall, HostConnectionError> {
            self.prepare_call(Some(generation), call)
        }

        fn request(
            &self,
            call: PreparedHostCall,
            timeout: Duration,
        ) -> Result<HostReply, HostConnectionError> {
            self.timeouts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(timeout);
            let request_id = call.request_id;
            let semantics = call.call.semantics;
            let step = self
                .steps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .expect("request without a scripted step");
            match step.outcome {
                ScriptOutcome::Reply {
                    generation,
                    status,
                    body,
                } => Ok(HostReply {
                    request_id,
                    generation,
                    status,
                    body,
                }),
                ScriptOutcome::MismatchedReplyId {
                    generation,
                    status,
                    body,
                } => Ok(HostReply {
                    request_id: request_id + 1_000,
                    generation,
                    status,
                    body,
                }),
                ScriptOutcome::Disconnect(delivery) => Err(HostConnectionError::Disconnected {
                    request_id,
                    semantics,
                    delivery,
                    message: "scripted disconnect".to_owned(),
                }),
                ScriptOutcome::Timeout(delivery) => Err(HostConnectionError::TimedOut {
                    request_id,
                    semantics,
                    delivery,
                }),
                ScriptOutcome::Launch => Err(HostConnectionError::Launch {
                    request_id,
                    message: "scripted launch failure".to_owned(),
                }),
                ScriptOutcome::RequestTooLarge => Err(HostConnectionError::RequestTooLarge {
                    request_id,
                    encoded_bytes: 2,
                    max_bytes: 1,
                }),
                ScriptOutcome::TooManyInFlight => Err(HostConnectionError::TooManyInFlight {
                    request_id,
                    limit: 1,
                }),
                ScriptOutcome::PrepareGenerationChanged => {
                    panic!("prepare failure reached request")
                }
            }
        }

        fn disconnect(&self) {
            self.closed.store(true, Ordering::Release);
        }
    }

    fn expected_bootstrap() -> ExpectedCall {
        ExpectedCall {
            generation: None,
            method: "GET",
            path: BOOTSTRAP_PATH,
            query: Vec::new(),
            content_type: None,
            body: None,
            semantics: RequestSemantics::ReadOnly,
        }
    }

    fn expected_output(
        generation: ConnectionGeneration,
        session_id: &str,
        offset: Option<u64>,
    ) -> ExpectedCall {
        let mut query = vec![
            ("session_id".to_owned(), session_id.to_owned()),
            ("limit".to_owned(), REMOTE_OUTPUT_DEFAULT_LIMIT.to_string()),
        ];
        if let Some(offset) = offset {
            query.push(("offset".to_owned(), offset.to_string()));
        }
        ExpectedCall {
            generation: Some(generation),
            method: "GET",
            path: OUTPUT_PATH,
            query,
            content_type: None,
            body: None,
            semantics: RequestSemantics::ReadOnly,
        }
    }

    fn expected_effect(
        generation: ConnectionGeneration,
        path: &'static str,
        body: Value,
    ) -> ExpectedCall {
        ExpectedCall {
            generation: Some(generation),
            method: "POST",
            path,
            query: Vec::new(),
            content_type: Some("application/json"),
            body: Some(body),
            semantics: RequestSemantics::Effect,
        }
    }

    fn wait_for_effect_tickets(backend: &RemoteSessionBackend, expected: u64) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let issued = backend
                .inner
                .effect_sequence
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .next_ticket;
            if issued >= expected {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {expected} effect tickets; saw {issued}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn bootstrap_json(host_id: Option<&str>, major: u16, capabilities: Option<&[&str]>) -> Value {
        let mut value = json!({
            "protocolVersion": MOBILE_PROTOCOL_VERSION,
            "macName": "Studio",
            "folders": [],
            "projects": [],
            "presets": [],
            "sessions": [{
                "id": "s1",
                "projectID": "p1",
                "activeRuntimeID": "claude",
                "providerID": "claude",
                "title": "Research",
                "command": "claude",
                "createdAtUnixMs": 1,
                "ownerPrincipalID": "account:alice",
                "createdByDeviceID": "phone-1",
                "sourcePresetID": "claude-plan",
                "status": "running",
                "activity": "working"
            }],
            "capturedAtUnixMs": 10,
            "futureField": { "ignored": true }
        });
        if let Some(host_id) = host_id {
            value["macID"] = host_id.into();
        }
        if let Some(capabilities) = capabilities {
            value["hostProtocol"] = json!({
                "majorVersion": major,
                "minorVersion": 99,
                "capabilities": capabilities,
                "futureDescriptorField": true
            });
        }
        value
    }

    fn output_json(session_id: &str, offset: u64, bytes: &[u8], truncated: bool) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "sessionID": session_id,
            "offset": offset,
            "nextOffset": offset + bytes.len() as u64,
            "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "truncated": truncated,
            "capturedAtUnixMs": 20
        }))
        .unwrap()
    }

    fn reply_step(
        expected: ExpectedCall,
        generation: ConnectionGeneration,
        status: u16,
        body: Vec<u8>,
    ) -> ScriptStep {
        ScriptStep {
            expected,
            outcome: ScriptOutcome::Reply {
                generation,
                status,
                body,
            },
        }
    }

    fn add_bootstrap(
        connection: &ScriptedConnection,
        generation: ConnectionGeneration,
        body: Value,
    ) {
        connection.push(reply_step(
            expected_bootstrap(),
            generation,
            200,
            serde_json::to_vec(&body).unwrap(),
        ));
    }

    #[test]
    fn session_summary_round_trips_phone_fit_grid() {
        let session: RemoteSessionSummary = serde_json::from_value(json!({
            "id": "s1", "projectID": "p1", "title": "t", "command": "zsh",
            "createdAtUnixMs": 1, "status": "running", "activity": "idle",
            "phoneFitColumns": 44, "phoneFitRows": 37, "phoneFitSinceUnixMs": 99u64
        }))
        .unwrap();
        assert_eq!(session.phone_fit_columns, Some(44));
        assert_eq!(session.phone_fit_rows, Some(37));
        assert_eq!(session.phone_fit_since_unix_ms, Some(99));
        let wire = serde_json::to_value(&session).unwrap();
        assert_eq!(wire["phoneFitColumns"], 44);
        assert_eq!(wire["phoneFitRows"], 37);
        let bare: RemoteSessionSummary = serde_json::from_value(json!({
            "id": "s1", "projectID": "p1", "title": "t", "command": "zsh",
            "createdAtUnixMs": 1, "status": "running", "activity": "idle"
        }))
        .unwrap();
        assert!(serde_json::to_value(&bare)
            .unwrap()
            .get("phoneFitColumns")
            .is_none());
    }

    #[test]
    fn session_summary_round_trips_cwd() {
        let session: RemoteSessionSummary = serde_json::from_value(json!({
            "id": "s1", "projectID": "p1", "title": "t", "command": "zsh",
            "createdAtUnixMs": 1, "status": "running", "activity": "idle",
            "cwd": "/Users/me/Dev/flatsome"
        }))
        .unwrap();
        assert_eq!(session.cwd.as_deref(), Some("/Users/me/Dev/flatsome"));
        let wire = serde_json::to_value(&session).unwrap();
        assert_eq!(wire["cwd"], "/Users/me/Dev/flatsome");
        let bare: RemoteSessionSummary = serde_json::from_value(json!({
            "id": "s1", "projectID": "p1", "title": "t", "command": "zsh",
            "createdAtUnixMs": 1, "status": "running", "activity": "idle"
        }))
        .unwrap();
        assert_eq!(bare.cwd, None);
        assert!(serde_json::to_value(&bare).unwrap().get("cwd").is_none());
    }

    #[test]
    fn bootstrap_is_typed_versioned_and_pins_host_identity() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY, "future"]),
            ),
        );
        let backend = RemoteSessionBackend::new(connection.clone());
        let bootstrap = backend.bootstrap().unwrap();
        assert!(!backend.needs_bootstrap());
        assert_eq!(bootstrap.snapshot.host_id.as_deref(), Some("host-1"));
        assert_eq!(
            bootstrap.snapshot.sessions[0].status,
            RemoteSessionStatus::Running
        );
        assert_eq!(
            bootstrap.snapshot.sessions[0].activity,
            RemoteActivityState::Working
        );
        assert_eq!(
            bootstrap.snapshot.sessions[0].active_runtime_id.as_deref(),
            Some("claude")
        );
        assert_eq!(
            bootstrap.snapshot.sessions[0].provider_id.as_deref(),
            Some("claude")
        );
        assert!(bootstrap.raw.get("futureField").is_some());

        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-2"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        assert!(matches!(
            backend.bootstrap().unwrap_err(),
            RemoteSessionBackendError::HostIdentityChanged { .. }
        ));
        assert!(backend.needs_bootstrap());
        assert_eq!(
            backend
                .last_bootstrap()
                .unwrap()
                .snapshot
                .host_id
                .as_deref(),
            Some("host-1")
        );
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn concurrent_bootstrap_callers_share_one_host_refresh() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        let backend = RemoteSessionBackend::new(connection.clone());
        let observed_revision = backend.inner.lock_state().bootstrap_revision;
        let start = Arc::new(Barrier::new(3));

        let first_backend = backend.clone();
        let first_start = start.clone();
        let first = thread::spawn(move || {
            first_start.wait();
            first_backend
                .inner
                .refresh_bootstrap_observed(observed_revision)
        });
        let second_backend = backend.clone();
        let second_start = start.clone();
        let second = thread::spawn(move || {
            second_start.wait();
            second_backend
                .inner
                .refresh_bootstrap_observed(observed_revision)
        });
        start.wait();

        let first = first.join().unwrap().unwrap();
        let second = second.join().unwrap().unwrap();
        assert_eq!(first, second);
        assert_eq!(connection.calls().len(), 1);
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn bootstrap_rejects_partial_or_incompatible_snapshots_without_publishing() {
        let cases = [
            (
                bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR + 1,
                    Some(&[BOOTSTRAP_CAPABILITY]),
                ),
                "incompatible Host protocol",
            ),
            (
                bootstrap_json(Some("host-1"), HOST_PROTOCOL_MAJOR, Some(&[])),
                "required capability",
            ),
            (
                {
                    let mut value = bootstrap_json(
                        Some("host-1"),
                        HOST_PROTOCOL_MAJOR,
                        Some(&[BOOTSTRAP_CAPABILITY]),
                    );
                    value.as_object_mut().unwrap().remove("projects");
                    value
                },
                "missing field",
            ),
        ];
        for (body, expected) in cases {
            let connection = ScriptedConnection::new();
            let generation = connection.generation(1);
            add_bootstrap(&connection, generation, body);
            let backend = RemoteSessionBackend::new(connection);
            let error = backend.bootstrap().unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
            assert!(backend.needs_bootstrap());
            assert!(backend.last_bootstrap().is_none());
        }
    }

    #[test]
    fn bootstrap_preserves_optional_project_date_sort_mode() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        let mut body = bootstrap_json(
            Some("host-1"),
            HOST_PROTOCOL_MAJOR,
            Some(&[BOOTSTRAP_CAPABILITY]),
        );
        body["projects"] = json!([{
            "id": "p1",
            "name": "Research",
            "path": "/host/research",
            "mcpBlocked": false,
            "dateSorted": true
        }]);
        add_bootstrap(&connection, generation, body);

        let snapshot = RemoteSessionBackend::new(connection)
            .bootstrap()
            .expect("bootstrap decodes");
        assert_eq!(snapshot.snapshot.projects[0].date_sorted, Some(true));
    }

    #[test]
    fn bootstrap_preserves_session_owner_device_and_preset_attribution() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY]),
            ),
        );

        let snapshot = RemoteSessionBackend::new(connection)
            .bootstrap()
            .expect("bootstrap decodes");
        let session = &snapshot.snapshot.sessions[0];
        assert_eq!(session.owner_principal_id.as_deref(), Some("account:alice"));
        assert_eq!(session.created_by_device_id.as_deref(), Some("phone-1"));
        assert_eq!(session.source_preset_id.as_deref(), Some("claude-plan"));
    }

    #[test]
    fn legacy_v1_bootstrap_keeps_only_the_explicit_read_fallback() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(Some("host-1"), HOST_PROTOCOL_MAJOR, None),
        );
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            200,
            output_json("s1", 0, b"ok", false),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        let bootstrap = backend.bootstrap().unwrap();
        assert!(bootstrap.snapshot.host_protocol.is_none());
        assert!(bootstrap.snapshot.supports(OUTPUT_CAPABILITY));
        assert!(!bootstrap.snapshot.supports("session.create"));
        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(2));
        let effect = backend.write_terminal("s1", "x").unwrap_err();
        assert_eq!(effect.kind(), RemoteEffectFailureKind::NotApplied);
        assert!(matches!(
            effect.error(),
            RemoteSessionBackendError::MissingCapability(capability)
                if capability == WRITE_CAPABILITY
        ));
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn missing_output_capability_is_rejected_before_a_bound_call() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY]),
            ),
        );
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        assert!(matches!(
            backend
                .poll_output("s1", RemoteOutputPollOptions::default())
                .unwrap_err(),
            RemoteSessionBackendError::MissingCapability(capability)
                if capability == OUTPUT_CAPABILITY
        ));
        assert_eq!(connection.calls().len(), 1);
        assert_eq!(connection.remaining(), 0);
        assert!(!backend.needs_bootstrap());
    }

    #[test]
    fn effects_use_exact_generation_bound_v1_contracts() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[
                    BOOTSTRAP_CAPABILITY,
                    OUTPUT_CAPABILITY,
                    WRITE_CAPABILITY,
                    RESIZE_DESKTOP_CAPABILITY,
                    MARK_READ_CAPABILITY,
                ]),
            ),
        );
        connection.push(reply_step(
            expected_effect(
                generation,
                WRITE_PATH,
                json!({ "sessionID": "s1", "data": "\u{1b}[A\n\0Hei 👋" }),
            ),
            generation,
            200,
            br#"{"ok":true,"future":"ignored"}"#.to_vec(),
        ));
        connection.push(reply_step(
            expected_effect(
                generation,
                RESIZE_DESKTOP_PATH,
                json!({ "sessionID": "s1", "columns": 2, "rows": 120 }),
            ),
            generation,
            200,
            br#"{"ok":true}"#.to_vec(),
        ));
        connection.push(reply_step(
            expected_effect(
                generation,
                RESIZE_DESKTOP_PATH,
                json!({ "sessionID": "s1", "columns": 300, "rows": 2 }),
            ),
            generation,
            200,
            br#"{"ok":true}"#.to_vec(),
        ));
        connection.push(reply_step(
            expected_effect(
                generation,
                RESIZE_DESKTOP_PATH,
                json!({ "sessionID": "s1", "clear": true }),
            ),
            generation,
            200,
            br#"{"ok":true}"#.to_vec(),
        ));
        connection.push(reply_step(
            expected_effect(generation, MARK_READ_PATH, json!({ "sessionID": "s1" })),
            generation,
            200,
            br#"{"ok":true}"#.to_vec(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        assert_eq!(
            backend
                .write_terminal("s1", "\u{1b}[A\n\0Hei 👋")
                .unwrap()
                .request_id(),
            2
        );
        assert_eq!(
            backend
                .resize_desktop(
                    "s1",
                    RemoteDesktopResize::Fit {
                        columns: 0,
                        rows: u16::MAX,
                    },
                )
                .unwrap()
                .request_id(),
            3
        );
        assert_eq!(
            backend
                .resize_desktop(
                    "s1",
                    RemoteDesktopResize::Fit {
                        columns: u16::MAX,
                        rows: 0,
                    },
                )
                .unwrap()
                .request_id(),
            4
        );
        assert_eq!(
            backend
                .resize_desktop("s1", RemoteDesktopResize::Clear)
                .unwrap()
                .request_id(),
            5
        );
        assert_eq!(backend.mark_session_read("s1").unwrap().request_id(), 6);
        assert_eq!(connection.remaining(), 0);
        assert!(connection
            .calls()
            .iter()
            .skip(1)
            .all(|call| call.semantics == RequestSemantics::Effect));
        assert_eq!(
            connection.timeouts(),
            vec![
                DEFAULT_BOOTSTRAP_TIMEOUT,
                DEFAULT_EFFECT_TIMEOUT,
                DEFAULT_EFFECT_TIMEOUT,
                DEFAULT_EFFECT_TIMEOUT,
                DEFAULT_EFFECT_TIMEOUT,
                DEFAULT_EFFECT_TIMEOUT,
            ]
        );
    }

    #[test]
    fn app_effects_use_shared_host_contracts_and_install_timeout() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[
                    BOOTSTRAP_CAPABILITY,
                    OPENERS_CAPABILITY,
                    APPS_INSTALL_CAPABILITY,
                    APPS_OPEN_CAPABILITY,
                ]),
            ),
        );
        for (path, body) in [
            (
                OPENERS_PATH,
                json!({
                    "selector": "file:text/markdown",
                    "opener": "app:unpeel.app.markdown",
                }),
            ),
            (APPS_INSTALL_PATH, json!({ "appID": "unpeel.app.markdown" })),
            (
                APPS_OPEN_PATH,
                json!({
                    "callerSessionID": "s1",
                    "appID": "unpeel.app.markdown",
                    "mediaType": "text/markdown",
                    "resource": {
                        "kind": "file",
                        "id": "/tmp/hello world.md",
                    },
                    "requestID": "open-1",
                }),
            ),
        ] {
            connection.push(reply_step(
                expected_effect(generation, path, body),
                generation,
                200,
                br#"{"ok":true}"#.to_vec(),
            ));
        }
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        assert_eq!(
            backend
                .set_opener("file:text/markdown", "app:unpeel.app.markdown")
                .unwrap()
                .request_id(),
            2
        );
        assert_eq!(
            backend
                .install_app("unpeel.app.markdown")
                .unwrap()
                .request_id(),
            3
        );
        assert_eq!(
            backend
                .open_app(
                    "s1",
                    "unpeel.app.markdown",
                    "file",
                    Some("text/markdown"),
                    "/tmp/hello world.md",
                    "open-1",
                )
                .unwrap()
                .request_id(),
            4
        );
        assert_eq!(connection.remaining(), 0);
        assert_eq!(
            connection.timeouts(),
            vec![
                DEFAULT_BOOTSTRAP_TIMEOUT,
                DEFAULT_EFFECT_TIMEOUT,
                APP_INSTALL_EFFECT_TIMEOUT,
                DEFAULT_EFFECT_TIMEOUT,
            ]
        );
    }

    #[test]
    fn effect_validation_and_capabilities_fail_before_bound_dispatch() {
        let connection = ScriptedConnection::new();
        let backend = RemoteSessionBackend::new(connection.clone());
        let oversized = "x".repeat(REMOTE_TERMINAL_WRITE_MAX_BYTES + 1);
        for failure in [
            backend.write_terminal("../escape", "x").unwrap_err(),
            backend.write_terminal("s1", &oversized).unwrap_err(),
            backend
                .resize_desktop("", RemoteDesktopResize::Clear)
                .unwrap_err(),
            backend.mark_session_read("bad/id").unwrap_err(),
        ] {
            assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        }
        assert!(connection.calls().is_empty());

        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        backend.bootstrap().unwrap();
        let failure = backend.write_terminal("s1", "x").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        assert!(matches!(
            failure.error(),
            RemoteSessionBackendError::MissingCapability(capability)
                if capability == WRITE_CAPABILITY
        ));
        assert_eq!(connection.calls().len(), 1);
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn ambiguous_effect_is_not_replayed_and_next_read_rebootstraps() {
        let connection = ScriptedConnection::new();
        let first_generation = connection.generation(1);
        let second_generation = connection.generation(2);
        add_bootstrap(
            &connection,
            first_generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY, WRITE_CAPABILITY]),
            ),
        );
        connection.push(ScriptStep {
            expected: expected_effect(
                first_generation,
                WRITE_PATH,
                json!({ "sessionID": "s1", "data": "once" }),
            ),
            outcome: ScriptOutcome::Disconnect(DeliveryState::OutcomeUnknown),
        });
        add_bootstrap(
            &connection,
            second_generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY, WRITE_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(second_generation, "s1", None),
            second_generation,
            200,
            output_json("s1", 0, b"after", false),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let failure = backend.write_terminal("s1", "once").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
        assert!(failure.to_string().contains("may have been applied"));
        assert!(backend.needs_bootstrap());
        assert!(backend.last_bootstrap().is_some());
        assert_eq!(
            connection
                .calls()
                .iter()
                .filter(|call| call.path == WRITE_PATH)
                .count(),
            1
        );

        let page = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        assert_eq!(page.bytes(), b"after");
        page.commit().unwrap();
        assert_eq!(
            connection
                .calls()
                .iter()
                .filter(|call| call.path == WRITE_PATH)
                .count(),
            1,
            "the effect was replayed during reconnect"
        );
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn effect_timeout_preserves_transport_delivery_certainty_without_replay() {
        for (delivery, expected_kind) in [
            (DeliveryState::NotSent, RemoteEffectFailureKind::NotApplied),
            (
                DeliveryState::OutcomeUnknown,
                RemoteEffectFailureKind::OutcomeUnknown,
            ),
        ] {
            let connection = ScriptedConnection::new();
            let generation = connection.generation(1);
            add_bootstrap(
                &connection,
                generation,
                bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
                ),
            );
            connection.push(ScriptStep {
                expected: expected_effect(
                    generation,
                    WRITE_PATH,
                    json!({ "sessionID": "s1", "data": "once" }),
                ),
                outcome: ScriptOutcome::Timeout(delivery),
            });
            let backend = RemoteSessionBackend::new(connection.clone());
            backend.bootstrap().unwrap();

            let failure = backend.write_terminal("s1", "once").unwrap_err();
            assert_eq!(failure.kind(), expected_kind);
            assert!(matches!(
                failure.error(),
                RemoteSessionBackendError::Connection(HostConnectionError::TimedOut {
                    delivery: received,
                    ..
                }) if *received == delivery
            ));
            // SSH/direct tear down a generation when a deadline expires so a
            // late reply cannot be mistaken for a later call. The backend
            // mirrors that transport fact even when no bytes were sent.
            assert!(backend.needs_bootstrap());
            assert_eq!(
                connection
                    .calls()
                    .iter()
                    .filter(|call| call.path == WRITE_PATH)
                    .count(),
                1,
                "timed-out effect was replayed"
            );
            assert_eq!(connection.remaining(), 0);
        }
    }

    #[test]
    fn generation_changed_effect_is_not_applied_or_retried() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
            ),
        );
        connection.push(ScriptStep {
            expected: expected_effect(
                generation,
                WRITE_PATH,
                json!({ "sessionID": "s1", "data": "not sent" }),
            ),
            outcome: ScriptOutcome::PrepareGenerationChanged,
        });
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let failure = backend.write_terminal("s1", "not sent").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        assert!(matches!(
            failure.error(),
            RemoteSessionBackendError::Connection(HostConnectionError::GenerationChanged { .. })
        ));
        assert!(backend.needs_bootstrap());
        assert_eq!(connection.calls().len(), 2);
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn local_transport_rejections_do_not_invalidate_a_healthy_generation() {
        for outcome in [
            ScriptOutcome::RequestTooLarge,
            ScriptOutcome::TooManyInFlight,
        ] {
            let connection = ScriptedConnection::new();
            let generation = connection.generation(1);
            add_bootstrap(
                &connection,
                generation,
                bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
                ),
            );
            connection.push(ScriptStep {
                expected: expected_effect(
                    generation,
                    WRITE_PATH,
                    json!({ "sessionID": "s1", "data": "x" }),
                ),
                outcome,
            });
            let backend = RemoteSessionBackend::new(connection.clone());
            backend.bootstrap().unwrap();

            let failure = backend.write_terminal("s1", "x").unwrap_err();
            assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
            assert!(!backend.needs_bootstrap());
            assert_eq!(connection.remaining(), 0);
        }
    }

    #[test]
    fn mismatched_effect_reply_generation_is_outcome_unknown() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        let wrong_generation = connection.generation(2);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_effect(
                generation,
                WRITE_PATH,
                json!({ "sessionID": "s1", "data": "maybe" }),
            ),
            wrong_generation,
            200,
            br#"{"ok":true}"#.to_vec(),
        ));
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();

        let failure = backend.write_terminal("s1", "maybe").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
        assert!(backend.needs_bootstrap());
    }

    #[test]
    fn mismatched_effect_reply_request_id_is_outcome_unknown() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
            ),
        );
        connection.push(ScriptStep {
            expected: expected_effect(
                generation,
                WRITE_PATH,
                json!({ "sessionID": "s1", "data": "maybe" }),
            ),
            outcome: ScriptOutcome::MismatchedReplyId {
                generation,
                status: 200,
                body: br#"{"ok":true}"#.to_vec(),
            },
        });
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();

        let failure = backend.write_terminal("s1", "maybe").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
        assert!(matches!(
            failure.error(),
            RemoteSessionBackendError::InvalidResponse { message, .. }
                if message.contains("reply request id")
        ));
        assert!(backend.needs_bootstrap());
    }

    #[test]
    fn correlated_semantic_rejections_are_not_applied_and_keep_generation() {
        let cases: &[(u16, &[u8])] = &[
            (400, br#"{"error":"request failed"}"#),
            (404, br#"{"error":"unknown session"}"#),
            (409, br#"{"error":"session has exited"}"#),
            (429, br#"{"error":"too many requests"}"#),
            // SSH stdio and the Link dispatcher emit 503 only when their
            // bounded queue refused the request before application.
            (503, br#"{"error":"Host request queue is full"}"#),
        ];

        for (status, body) in cases {
            let connection = ScriptedConnection::new();
            let generation = connection.generation(1);
            add_bootstrap(
                &connection,
                generation,
                bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
                ),
            );
            connection.push(reply_step(
                expected_effect(
                    generation,
                    WRITE_PATH,
                    json!({ "sessionID": "s1", "data": "once" }),
                ),
                generation,
                *status,
                body.to_vec(),
            ));
            connection.push(reply_step(
                expected_effect(
                    generation,
                    WRITE_PATH,
                    json!({ "sessionID": "s1", "data": "later" }),
                ),
                generation,
                200,
                br#"{"ok":true}"#.to_vec(),
            ));
            let backend = RemoteSessionBackend::new(connection.clone());
            backend.bootstrap().unwrap();

            let failure = backend.write_terminal("s1", "once").unwrap_err();
            assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
            assert!(matches!(
                failure.error(),
                RemoteSessionBackendError::HostStatus {
                    status: received,
                    ..
                } if received == status
            ));
            assert!(!backend.needs_bootstrap());
            assert_eq!(
                connection
                    .calls()
                    .iter()
                    .filter(|call| call.path == WRITE_PATH)
                    .count(),
                1,
                "the backend retried status {status} internally"
            );

            // A caller may continue after an explicitly non-applied result;
            // the correlated rejection did not poison a healthy transport.
            backend.write_terminal("s1", "later").unwrap();
            assert!(!backend.needs_bootstrap());
            assert_eq!(connection.remaining(), 0);
        }
    }

    #[test]
    fn uncertain_host_failures_remain_unknown_and_invalidate_generation() {
        for status in [500, 502, 504] {
            let connection = ScriptedConnection::new();
            let generation = connection.generation(1);
            add_bootstrap(
                &connection,
                generation,
                bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
                ),
            );
            connection.push(reply_step(
                expected_effect(
                    generation,
                    WRITE_PATH,
                    json!({ "sessionID": "s1", "data": "once" }),
                ),
                generation,
                status,
                br#"{"error":"application failed"}"#.to_vec(),
            ));
            let backend = RemoteSessionBackend::new(connection.clone());
            backend.bootstrap().unwrap();

            let failure = backend.write_terminal("s1", "once").unwrap_err();
            assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
            assert!(matches!(
                failure.error(),
                RemoteSessionBackendError::HostStatus {
                    status: received,
                    ..
                } if *received == status
            ));
            assert!(backend.needs_bootstrap());
            assert_eq!(
                connection
                    .calls()
                    .iter()
                    .filter(|call| call.path == WRITE_PATH)
                    .count(),
                1,
                "uncertain effect must never be replayed for status {status}"
            );
            assert_eq!(connection.remaining(), 0);
        }
    }

    #[test]
    fn malformed_or_negative_success_receipts_are_outcome_unknown() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, MARK_READ_CAPABILITY]),
            ),
        );
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        connection.push(reply_step(
            expected_effect(generation, MARK_READ_PATH, json!({ "sessionID": "s1" })),
            generation,
            200,
            br#"{"ok":false}"#.to_vec(),
        ));
        let failure = backend.mark_session_read("s1").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
        assert!(backend.needs_bootstrap());

        for body in [b"{".as_slice(), b"{}".as_slice()] {
            add_bootstrap(
                &connection,
                generation,
                bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, MARK_READ_CAPABILITY]),
                ),
            );
            connection.push(reply_step(
                expected_effect(generation, MARK_READ_PATH, json!({ "sessionID": "s1" })),
                generation,
                200,
                body.to_vec(),
            ));
            backend.bootstrap().unwrap();
            let failure = backend.mark_session_read("s1").unwrap_err();
            assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
            assert!(backend.needs_bootstrap());
        }
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn output_cursor_advances_only_after_commit_and_drop_discards() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        for bytes in [b"hello".as_slice(), b"hello".as_slice()] {
            connection.push(reply_step(
                expected_output(generation, "s1", None),
                generation,
                200,
                output_json("s1", 0, bytes, false),
            ));
        }
        connection.push(reply_step(
            expected_output(generation, "s1", Some(5)),
            generation,
            200,
            output_json("s1", 5, &[0xff], false),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let page = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        assert_eq!(page.requested_offset(), None);
        assert_eq!(page.bytes(), b"hello");
        assert!(page.reset_required());
        assert_eq!(backend.committed_output_offset("s1"), None);
        assert!(matches!(
            backend
                .poll_output("s1", RemoteOutputPollOptions::default())
                .unwrap_err(),
            RemoteSessionBackendError::OutputPagePending(_)
        ));
        drop(page);
        assert_eq!(backend.committed_output_offset("s1"), None);

        let replay = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        replay.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(5));

        let next = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        assert_eq!(next.requested_offset(), Some(5));
        assert_eq!(next.bytes(), &[0xff]);
        assert!(!next.reset_required());
        next.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(6));
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn explicit_output_cursor_recovers_a_lost_delivery_and_supersedes_older_page() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            200,
            output_json("s1", 0, b"abcde", false),
        ));
        connection.push(reply_step(
            expected_output(generation, "s1", Some(5)),
            generation,
            200,
            output_json("s1", 5, b"lost", false),
        ));
        connection.push(reply_step(
            expected_output(generation, "s1", Some(2)),
            generation,
            200,
            output_json("s1", 2, b"cdef", false),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(5));

        // Model a response page owned by an older renderer. The replacement
        // phone request carries the last offset it actually rendered (2), so
        // it must be allowed to rewind without first resolving this page.
        let older_page = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        let replay = backend
            .poll_output_from("s1", Some(2), RemoteOutputPollOptions::default())
            .unwrap();
        assert_eq!(replay.requested_offset(), Some(2));
        assert_eq!((replay.offset(), replay.next_offset()), (2, 6));
        assert!(!replay.reset_required());
        replay.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(6));

        assert!(matches!(
            older_page.commit().unwrap_err(),
            RemoteSessionBackendError::StaleOutputPage
        ));
        assert_eq!(backend.committed_output_offset("s1"), Some(6));
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn maximum_output_long_poll_keeps_relay_response_headroom_outside_host_wait() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        let mut expected = expected_output(generation, "s1", None);
        expected.query.push((
            "wait_ms".to_owned(),
            REMOTE_OUTPUT_MAX_WAIT.as_millis().to_string(),
        ));
        connection.push(reply_step(
            expected,
            generation,
            200,
            output_json("s1", 0, b"", false),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        backend
            .poll_output(
                "s1",
                RemoteOutputPollOptions {
                    limit: REMOTE_OUTPUT_DEFAULT_LIMIT,
                    wait: REMOTE_OUTPUT_MAX_WAIT,
                },
            )
            .unwrap()
            .discard();

        assert_eq!(
            connection.timeouts(),
            vec![
                DEFAULT_BOOTSTRAP_TIMEOUT,
                REMOTE_OUTPUT_MAX_WAIT + DEFAULT_OUTPUT_HEADROOM,
            ]
        );
        assert!(
            REMOTE_OUTPUT_MAX_WAIT + DEFAULT_OUTPUT_HEADROOM > REMOTE_OUTPUT_MAX_WAIT,
            "transport deadline must not expire with the Host long-poll"
        );
        assert!(!backend.needs_bootstrap());
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn rebased_output_requests_a_renderer_reset_and_commits_the_host_offset() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            200,
            output_json("s1", 0, b"hello", false),
        ));
        connection.push(reply_step(
            expected_output(generation, "s1", Some(5)),
            generation,
            200,
            output_json("s1", 1, b"xy", false),
        ));
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        let rebased = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        assert_eq!(rebased.requested_offset(), Some(5));
        assert_eq!((rebased.offset(), rebased.next_offset()), (1, 3));
        assert!(rebased.reset_required());
        rebased.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(3));
    }

    #[test]
    fn malformed_output_releases_the_reservation_without_moving_the_cursor() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "wrong",
                "offset": 0,
                "nextOffset": 1,
                "dataBase64": "eA==",
                "truncated": false,
                "capturedAtUnixMs": 1
            }))
            .unwrap(),
        ));
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            200,
            output_json("s1", 0, b"x", false),
        ));
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        assert!(matches!(
            backend
                .poll_output("s1", RemoteOutputPollOptions::default())
                .unwrap_err(),
            RemoteSessionBackendError::InvalidResponse { .. }
        ));
        assert_eq!(backend.committed_output_offset("s1"), None);
        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(1));
    }

    #[test]
    fn non_success_output_is_not_retried_and_does_not_move_the_cursor() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            503,
            br#"{"error":"busy\u001b[31m\nretry later"}"#.to_vec(),
        ));
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            200,
            output_json("s1", 0, b"x", false),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let error = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap_err();
        assert!(matches!(
            &error,
            RemoteSessionBackendError::HostStatus {
                operation: "output",
                status: 503,
                ..
            }
        ));
        let rendered = error.to_string();
        assert!(rendered.contains("busy [31m retry later"), "{rendered}");
        assert!(!rendered.chars().any(char::is_control));
        assert_eq!(backend.committed_output_offset("s1"), None);
        assert!(!backend.needs_bootstrap());
        assert_eq!(connection.calls().len(), 2, "503 was retried internally");

        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(1));
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn generation_loss_is_not_retried_and_next_poll_bootstraps_then_resumes() {
        let connection = ScriptedConnection::new();
        let first_generation = connection.generation(1);
        let second_generation = connection.generation(2);
        add_bootstrap(
            &connection,
            first_generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(first_generation, "s1", None),
            first_generation,
            200,
            output_json("s1", 0, b"hello", false),
        ));
        connection.push(ScriptStep {
            expected: expected_output(first_generation, "s1", Some(5)),
            outcome: ScriptOutcome::PrepareGenerationChanged,
        });
        add_bootstrap(
            &connection,
            second_generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(second_generation, "s1", Some(5)),
            second_generation,
            200,
            output_json("s1", 5, b" world", false),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();
        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        let calls_before_loss = connection.calls().len();
        let error = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap_err();
        assert!(matches!(
            error,
            RemoteSessionBackendError::Connection(HostConnectionError::GenerationChanged { .. })
        ));
        assert_eq!(connection.calls().len(), calls_before_loss + 1);
        assert!(backend.needs_bootstrap());
        assert!(backend.last_bootstrap().is_some());
        assert_eq!(backend.committed_output_offset("s1"), Some(5));

        let resumed = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        assert_eq!(resumed.requested_offset(), Some(5));
        assert_eq!(resumed.bytes(), b" world");
        resumed.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(11));
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn disconnected_read_invalidates_only_the_callable_generation() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(ScriptStep {
            expected: expected_output(generation, "s1", None),
            outcome: ScriptOutcome::Disconnect(DeliveryState::OutcomeUnknown),
        });
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();
        assert!(matches!(
            backend
                .poll_output("s1", RemoteOutputPollOptions::default())
                .unwrap_err(),
            RemoteSessionBackendError::Connection(HostConnectionError::Disconnected { .. })
        ));
        assert!(backend.needs_bootstrap());
        assert!(backend.last_bootstrap().is_some());
        assert_eq!(backend.committed_output_offset("s1"), None);
        assert_eq!(connection.remaining(), 0, "read was retried automatically");
    }

    #[test]
    fn failed_reconnect_launch_clears_the_callable_generation() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(ScriptStep {
            expected: expected_bootstrap(),
            outcome: ScriptOutcome::Launch,
        });
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        assert!(matches!(
            backend.bootstrap().unwrap_err(),
            RemoteSessionBackendError::Connection(HostConnectionError::Launch { .. })
        ));
        assert!(backend.needs_bootstrap());
        assert!(backend.last_bootstrap().is_some());
    }

    #[test]
    fn mismatched_output_reply_generation_invalidates_the_accepted_generation() {
        let connection = ScriptedConnection::new();
        let first_generation = connection.generation(1);
        let wrong_generation = connection.generation(2);
        add_bootstrap(
            &connection,
            first_generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(first_generation, "s1", None),
            wrong_generation,
            200,
            output_json("s1", 0, b"wrong generation", false),
        ));
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        assert!(matches!(
            backend
                .poll_output("s1", RemoteOutputPollOptions::default())
                .unwrap_err(),
            RemoteSessionBackendError::BootstrapChanged
        ));
        assert!(backend.needs_bootstrap());
        assert!(backend.last_bootstrap().is_some());
        assert_eq!(backend.committed_output_offset("s1"), None);
    }

    #[test]
    fn staged_page_can_commit_after_a_new_generation_is_bootstrapped() {
        let connection = ScriptedConnection::new();
        let first_generation = connection.generation(1);
        let second_generation = connection.generation(2);
        add_bootstrap(
            &connection,
            first_generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(first_generation, "s1", None),
            first_generation,
            200,
            output_json("s1", 0, b"hello", false),
        ));
        add_bootstrap(
            &connection,
            second_generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(second_generation, "s1", Some(5)),
            second_generation,
            200,
            output_json("s1", 5, b"!", false),
        ));
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        let staged = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        backend.bootstrap().unwrap();
        staged.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(5));
        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(6));
    }

    #[test]
    fn independent_sessions_keep_independent_cursors() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_output(generation, "s1", None),
            generation,
            200,
            output_json("s1", 0, b"one", false),
        ));
        connection.push(reply_step(
            expected_output(generation, "s2", None),
            generation,
            200,
            output_json("s2", 10, b"two", true),
        ));
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        let first = backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap();
        let second = backend
            .poll_output("s2", RemoteOutputPollOptions::default())
            .unwrap();
        first.commit().unwrap();
        second.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(3));
        assert_eq!(backend.committed_output_offset("s2"), Some(13));
    }

    struct RaceConnection {
        id: uuid::Uuid,
        next_request_id: AtomicU64,
        bootstrap_count: AtomicU64,
        output_started: Mutex<Option<mpsc::Sender<()>>>,
        output_release: Mutex<mpsc::Receiver<()>>,
    }

    impl HostConnection for RaceConnection {
        fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
            assert_eq!(call.path, BOOTSTRAP_PATH);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: None,
                call,
            })
        }

        fn prepare_in_generation(
            &self,
            generation: ConnectionGeneration,
            call: HostCall,
        ) -> Result<PreparedHostCall, HostConnectionError> {
            assert!(matches!(call.path.as_str(), OUTPUT_PATH | WRITE_PATH));
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: Some(generation),
                call,
            })
        }

        fn request(
            &self,
            call: PreparedHostCall,
            _timeout: Duration,
        ) -> Result<HostReply, HostConnectionError> {
            if call.call.path == BOOTSTRAP_PATH {
                let sequence = self.bootstrap_count.fetch_add(1, Ordering::Relaxed) + 1;
                let generation = ConnectionGeneration {
                    connection_id: self.id,
                    sequence,
                };
                return Ok(HostReply {
                    request_id: call.request_id,
                    generation,
                    status: 200,
                    body: serde_json::to_vec(&bootstrap_json(
                        Some("host-1"),
                        HOST_PROTOCOL_MAJOR,
                        Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY, WRITE_CAPABILITY]),
                    ))
                    .unwrap(),
                });
            }
            if call.call.path == WRITE_PATH {
                return Ok(HostReply {
                    request_id: call.request_id,
                    generation: call.required_generation.unwrap(),
                    status: 200,
                    body: br#"{"ok":true}"#.to_vec(),
                });
            }
            self.output_started
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .unwrap()
                .send(())
                .unwrap();
            self.output_release
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv()
                .unwrap();
            Ok(HostReply {
                request_id: call.request_id,
                generation: call.required_generation.unwrap(),
                status: 200,
                body: output_json("s1", 0, b"old", false),
            })
        }

        fn disconnect(&self) {}
    }

    struct LateBootstrapConnection {
        id: uuid::Uuid,
        next_request_id: AtomicU64,
        bootstrap_count: AtomicU64,
        second_started: Mutex<Option<mpsc::Sender<()>>>,
        second_release: Mutex<mpsc::Receiver<()>>,
    }

    struct OrderedEffectConnection {
        id: uuid::Uuid,
        next_request_id: AtomicU64,
        effect_count: AtomicU64,
        first_started: Mutex<Option<mpsc::Sender<()>>>,
        first_release: Mutex<mpsc::Receiver<()>>,
        applied: Mutex<Vec<String>>,
    }

    impl HostConnection for OrderedEffectConnection {
        fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
            assert_eq!(call.path, BOOTSTRAP_PATH);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: None,
                call,
            })
        }

        fn prepare_in_generation(
            &self,
            generation: ConnectionGeneration,
            call: HostCall,
        ) -> Result<PreparedHostCall, HostConnectionError> {
            assert_eq!(call.path, WRITE_PATH);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: Some(generation),
                call,
            })
        }

        fn request(
            &self,
            call: PreparedHostCall,
            _timeout: Duration,
        ) -> Result<HostReply, HostConnectionError> {
            let generation = ConnectionGeneration {
                connection_id: self.id,
                sequence: 1,
            };
            if call.call.path == BOOTSTRAP_PATH {
                return Ok(HostReply {
                    request_id: call.request_id,
                    generation,
                    status: 200,
                    body: serde_json::to_vec(&bootstrap_json(
                        Some("host-1"),
                        HOST_PROTOCOL_MAJOR,
                        Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
                    ))
                    .unwrap(),
                });
            }
            let body: Value = serde_json::from_slice(&call.call.body).unwrap();
            let data = body["data"].as_str().unwrap().to_owned();
            if self.effect_count.fetch_add(1, Ordering::AcqRel) == 0 {
                self.first_started
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.first_release
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv()
                    .unwrap();
            }
            self.applied
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(data);
            Ok(HostReply {
                request_id: call.request_id,
                generation,
                status: 200,
                body: br#"{"ok":true}"#.to_vec(),
            })
        }

        fn disconnect(&self) {}
    }

    struct IdentityChangeConnection {
        id: uuid::Uuid,
        next_request_id: AtomicU64,
        bootstrap_count: AtomicU64,
        bound_prepares: AtomicU64,
        second_started: Mutex<Option<mpsc::Sender<()>>>,
        second_release: Mutex<mpsc::Receiver<()>>,
    }

    struct SameGenerationRefreshConnection {
        id: uuid::Uuid,
        next_request_id: AtomicU64,
        bootstrap_count: AtomicU64,
        bound_prepares: AtomicU64,
        second_has_write: bool,
        second_started: Mutex<Option<mpsc::Sender<()>>>,
        second_release: Mutex<mpsc::Receiver<()>>,
    }

    impl HostConnection for SameGenerationRefreshConnection {
        fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
            assert_eq!(call.path, BOOTSTRAP_PATH);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: None,
                call,
            })
        }

        fn prepare_in_generation(
            &self,
            generation: ConnectionGeneration,
            call: HostCall,
        ) -> Result<PreparedHostCall, HostConnectionError> {
            assert_eq!(call.path, WRITE_PATH);
            self.bound_prepares.fetch_add(1, Ordering::AcqRel);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: Some(generation),
                call,
            })
        }

        fn request(
            &self,
            call: PreparedHostCall,
            _timeout: Duration,
        ) -> Result<HostReply, HostConnectionError> {
            let generation = ConnectionGeneration {
                connection_id: self.id,
                sequence: 1,
            };
            if call.call.path == BOOTSTRAP_PATH {
                let count = self.bootstrap_count.fetch_add(1, Ordering::AcqRel);
                if count == 1 {
                    self.second_started
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                        .unwrap()
                        .send(())
                        .unwrap();
                    self.second_release
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .recv()
                        .unwrap();
                }
                let body = if count == 0 || self.second_has_write {
                    bootstrap_json(
                        Some("host-1"),
                        HOST_PROTOCOL_MAJOR,
                        Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
                    )
                } else {
                    bootstrap_json(
                        Some("host-1"),
                        HOST_PROTOCOL_MAJOR,
                        Some(&[BOOTSTRAP_CAPABILITY]),
                    )
                };
                return Ok(HostReply {
                    request_id: call.request_id,
                    generation,
                    status: 200,
                    body: serde_json::to_vec(&body).unwrap(),
                });
            }
            assert_eq!(call.call.path, WRITE_PATH);
            Ok(HostReply {
                request_id: call.request_id,
                generation,
                status: 200,
                body: br#"{"ok":true}"#.to_vec(),
            })
        }

        fn disconnect(&self) {}
    }

    impl HostConnection for IdentityChangeConnection {
        fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
            assert_eq!(call.path, BOOTSTRAP_PATH);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: None,
                call,
            })
        }

        fn prepare_in_generation(
            &self,
            generation: ConnectionGeneration,
            call: HostCall,
        ) -> Result<PreparedHostCall, HostConnectionError> {
            self.bound_prepares.fetch_add(1, Ordering::AcqRel);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: Some(generation),
                call,
            })
        }

        fn request(
            &self,
            call: PreparedHostCall,
            _timeout: Duration,
        ) -> Result<HostReply, HostConnectionError> {
            assert_eq!(call.call.path, BOOTSTRAP_PATH);
            let count = self.bootstrap_count.fetch_add(1, Ordering::AcqRel);
            if count == 1 {
                self.second_started
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.second_release
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv()
                    .unwrap();
            }
            Ok(HostReply {
                request_id: call.request_id,
                generation: ConnectionGeneration {
                    connection_id: self.id,
                    sequence: 1,
                },
                status: 200,
                body: serde_json::to_vec(&bootstrap_json(
                    Some(if count == 0 { "host-1" } else { "host-2" }),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, WRITE_CAPABILITY]),
                ))
                .unwrap(),
            })
        }

        fn disconnect(&self) {}
    }

    impl HostConnection for LateBootstrapConnection {
        fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
            assert_eq!(call.path, BOOTSTRAP_PATH);
            Ok(PreparedHostCall {
                connection_id: self.id,
                request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
                required_generation: None,
                call,
            })
        }

        fn prepare_in_generation(
            &self,
            _generation: ConnectionGeneration,
            _call: HostCall,
        ) -> Result<PreparedHostCall, HostConnectionError> {
            panic!("late-bootstrap test never prepares a bound call")
        }

        fn request(
            &self,
            call: PreparedHostCall,
            _timeout: Duration,
        ) -> Result<HostReply, HostConnectionError> {
            let count = self.bootstrap_count.fetch_add(1, Ordering::Relaxed);
            if count == 1 {
                self.second_started
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.second_release
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv()
                    .unwrap();
            }
            Ok(HostReply {
                request_id: call.request_id,
                generation: ConnectionGeneration {
                    connection_id: self.id,
                    sequence: 1,
                },
                status: 200,
                body: serde_json::to_vec(&bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
                ))
                .unwrap(),
            })
        }

        fn disconnect(&self) {}
    }

    #[test]
    fn output_long_poll_does_not_block_effect_dispatch() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let connection = Arc::new(RaceConnection {
            id: uuid::Uuid::new_v4(),
            next_request_id: AtomicU64::new(1),
            bootstrap_count: AtomicU64::new(0),
            output_started: Mutex::new(Some(started_tx)),
            output_release: Mutex::new(release_rx),
        });
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        let polling_backend = backend.clone();
        let polling = thread::spawn(move || {
            polling_backend.poll_output("s1", RemoteOutputPollOptions::default())
        });
        started_rx.recv().unwrap();

        let receipt = backend.write_terminal("s1", "while waiting").unwrap();
        assert_eq!(receipt.request_id(), 3);
        assert!(!polling.is_finished());

        release_tx.send(()).unwrap();
        let page = polling.join().unwrap().unwrap();
        assert_eq!(page.bytes(), b"old");
        page.discard();
    }

    #[test]
    fn concurrent_terminal_writes_are_applied_in_fifo_ticket_order() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let connection = Arc::new(OrderedEffectConnection {
            id: uuid::Uuid::new_v4(),
            next_request_id: AtomicU64::new(1),
            effect_count: AtomicU64::new(0),
            first_started: Mutex::new(Some(started_tx)),
            first_release: Mutex::new(release_rx),
            applied: Mutex::new(Vec::new()),
        });
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let first_backend = backend.clone();
        let first = thread::spawn(move || first_backend.write_terminal("s1", "first"));
        started_rx.recv().unwrap();

        let second_backend = backend.clone();
        let second = thread::spawn(move || second_backend.write_terminal("s1", "second"));
        wait_for_effect_tickets(&backend, 2);

        let third_backend = backend.clone();
        let third = thread::spawn(move || third_backend.write_terminal("s1", "third"));
        wait_for_effect_tickets(&backend, 3);
        assert!(
            !second.is_finished(),
            "a later terminal write overtook the first"
        );
        assert!(
            !third.is_finished(),
            "the third terminal write escaped FIFO"
        );

        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        third.join().unwrap().unwrap();
        assert_eq!(
            *connection
                .applied
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn same_generation_refresh_does_not_reject_a_waiting_effect() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let connection = Arc::new(SameGenerationRefreshConnection {
            id: uuid::Uuid::new_v4(),
            next_request_id: AtomicU64::new(1),
            bootstrap_count: AtomicU64::new(0),
            bound_prepares: AtomicU64::new(0),
            second_has_write: true,
            second_started: Mutex::new(Some(started_tx)),
            second_release: Mutex::new(release_rx),
        });
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let refreshing_backend = backend.clone();
        let refreshing = thread::spawn(move || refreshing_backend.bootstrap());
        started_rx.recv().unwrap();
        let effect_backend = backend.clone();
        let effect = thread::spawn(move || effect_backend.write_terminal("s1", "keystroke"));
        thread::sleep(Duration::from_millis(50));
        assert!(!effect.is_finished());

        release_tx.send(()).unwrap();
        refreshing.join().unwrap().unwrap();
        effect.join().unwrap().unwrap();
        assert_eq!(connection.bound_prepares.load(Ordering::Acquire), 1);
        assert!(!backend.needs_bootstrap());
    }

    #[test]
    fn same_generation_capability_removal_rejects_a_waiting_effect() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let connection = Arc::new(SameGenerationRefreshConnection {
            id: uuid::Uuid::new_v4(),
            next_request_id: AtomicU64::new(1),
            bootstrap_count: AtomicU64::new(0),
            bound_prepares: AtomicU64::new(0),
            second_has_write: false,
            second_started: Mutex::new(Some(started_tx)),
            second_release: Mutex::new(release_rx),
        });
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let refreshing_backend = backend.clone();
        let refreshing = thread::spawn(move || refreshing_backend.bootstrap());
        started_rx.recv().unwrap();
        let effect_backend = backend.clone();
        let effect = thread::spawn(move || effect_backend.write_terminal("s1", "blocked"));
        thread::sleep(Duration::from_millis(50));
        assert!(!effect.is_finished());

        release_tx.send(()).unwrap();
        refreshing.join().unwrap().unwrap();
        let failure = effect.join().unwrap().unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        assert!(matches!(
            failure.error(),
            RemoteSessionBackendError::MissingCapability(capability)
                if capability == WRITE_CAPABILITY
        ));
        assert_eq!(connection.bound_prepares.load(Ordering::Acquire), 0);
        assert!(!backend.needs_bootstrap());
    }

    #[test]
    fn identity_change_finishes_before_a_waiting_effect_can_prepare() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let connection = Arc::new(IdentityChangeConnection {
            id: uuid::Uuid::new_v4(),
            next_request_id: AtomicU64::new(1),
            bootstrap_count: AtomicU64::new(0),
            bound_prepares: AtomicU64::new(0),
            second_started: Mutex::new(Some(started_tx)),
            second_release: Mutex::new(release_rx),
        });
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let refreshing_backend = backend.clone();
        let refreshing = thread::spawn(move || refreshing_backend.bootstrap());
        started_rx.recv().unwrap();
        let effect_backend = backend.clone();
        let effect = thread::spawn(move || effect_backend.write_terminal("s1", "blocked"));
        thread::sleep(Duration::from_millis(50));
        assert!(!effect.is_finished());

        release_tx.send(()).unwrap();
        assert!(matches!(
            refreshing.join().unwrap().unwrap_err(),
            RemoteSessionBackendError::HostIdentityChanged { .. }
        ));
        let failure = effect.join().unwrap().unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        assert!(matches!(
            failure.error(),
            RemoteSessionBackendError::BootstrapChanged
        ));
        assert_eq!(connection.bound_prepares.load(Ordering::Acquire), 0);
        assert!(backend.needs_bootstrap());
    }

    #[test]
    fn response_from_a_superseded_generation_is_never_staged() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let connection = Arc::new(RaceConnection {
            id: uuid::Uuid::new_v4(),
            next_request_id: AtomicU64::new(1),
            bootstrap_count: AtomicU64::new(0),
            output_started: Mutex::new(Some(started_tx)),
            output_release: Mutex::new(release_rx),
        });
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        let polling_backend = backend.clone();
        let polling = thread::spawn(move || {
            polling_backend.poll_output("s1", RemoteOutputPollOptions::default())
        });
        started_rx.recv().unwrap();
        backend.bootstrap().unwrap();
        release_tx.send(()).unwrap();
        assert!(matches!(
            polling.join().unwrap().unwrap_err(),
            RemoteSessionBackendError::BootstrapChanged
        ));
        assert_eq!(backend.committed_output_offset("s1"), None);
        assert!(!backend.needs_bootstrap());
    }

    #[test]
    fn late_bootstrap_reply_cannot_resurrect_a_disconnected_generation() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let connection = Arc::new(LateBootstrapConnection {
            id: uuid::Uuid::new_v4(),
            next_request_id: AtomicU64::new(1),
            bootstrap_count: AtomicU64::new(0),
            second_started: Mutex::new(Some(started_tx)),
            second_release: Mutex::new(release_rx),
        });
        let backend = RemoteSessionBackend::new(connection);
        backend.bootstrap().unwrap();
        let refreshing_backend = backend.clone();
        let refreshing = thread::spawn(move || refreshing_backend.bootstrap());
        started_rx.recv().unwrap();
        backend.disconnect();
        release_tx.send(()).unwrap();
        assert!(matches!(
            refreshing.join().unwrap().unwrap_err(),
            RemoteSessionBackendError::BootstrapChanged
        ));
        assert!(backend.needs_bootstrap());
        assert!(backend.last_bootstrap().is_some());
    }

    #[test]
    fn strict_output_decode_handles_bounds_and_offset_arithmetic() {
        let aligned_tail = vec![b'x'; 16 + INITIAL_TAIL_ALIGNMENT_ALLOWANCE];
        assert!(
            decode_output_chunk(&output_json("s1", 0, &aligned_tail, false), "s1", None, 16)
                .is_ok()
        );
        let oversized_tail = vec![b'x'; 17 + INITIAL_TAIL_ALIGNMENT_ALLOWANCE];
        assert!(decode_output_chunk(
            &output_json("s1", 0, &oversized_tail, false),
            "s1",
            None,
            16
        )
        .is_err());
        assert!(
            decode_output_chunk(&output_json("s1", 0, &[b'x'; 17], false), "s1", Some(0), 16)
                .is_err()
        );
        let rebased_tail = vec![b'x'; 16 + INITIAL_TAIL_ALIGNMENT_ALLOWANCE];
        assert!(decode_output_chunk(
            &output_json("s1", 100, &rebased_tail, true),
            "s1",
            Some(1),
            16
        )
        .is_ok());
        let bad = serde_json::to_vec(&json!({
            "sessionID": "s1",
            "offset": u64::MAX,
            "nextOffset": u64::MAX,
            "dataBase64": "eA==",
            "truncated": false,
            "capturedAtUnixMs": 1
        }))
        .unwrap();
        assert!(decode_output_chunk(&bad, "s1", Some(0), 16).is_err());
    }

    #[test]
    fn session_capabilities_decode_legacy_restart_and_additive_resume() {
        let legacy: RemoteSessionCapabilities = serde_json::from_value(json!({
            "restart": false,
            "restartAgent": true,
            "fork": false,
            "appendSystemContext": false,
            "notifyWhenDone": false
        }))
        .unwrap();
        assert!(legacy.restart_agent);
        assert!(!legacy.resume_agent);

        let current: RemoteSessionCapabilities = serde_json::from_value(json!({
            "restart": false,
            "resumeAgent": true,
            "fork": false,
            "appendSystemContext": false,
            "notifyWhenDone": false
        }))
        .unwrap();
        assert!(!current.restart_agent);
        assert!(current.resume_agent);
    }

    fn expected_read(
        generation: ConnectionGeneration,
        path: &'static str,
        query: Vec<(String, String)>,
    ) -> ExpectedCall {
        ExpectedCall {
            generation: Some(generation),
            method: "GET",
            path,
            query,
            content_type: None,
            body: None,
            semantics: RequestSemantics::ReadOnly,
        }
    }

    const LIFECYCLE_CAPABILITIES: &[&str] = &[
        BOOTSTRAP_CAPABILITY,
        TITLE_SET_CAPABILITY,
        PIN_SET_CAPABILITY,
        NOTIFY_WHEN_DONE_CAPABILITY,
        ARCHIVE_CAPABILITY,
        RESTORE_CAPABILITY,
        STOP_CAPABILITY,
        REMOVE_CAPABILITY,
        RESTART_CAPABILITY,
        RESTART_AGENT_CAPABILITY,
        RESUME_AGENT_CAPABILITY,
        ORDER_SET_CAPABILITY,
        PROJECT_ORGANIZATION_CAPABILITY,
    ];

    #[test]
    fn lifecycle_and_organization_effects_use_v1_wire_contracts() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(LIFECYCLE_CAPABILITIES),
            ),
        );
        let ok = || br#"{"ok":true}"#.to_vec();
        for (path, body) in [
            (
                SESSION_ORGANIZATION_PATH,
                json!({ "sessionID": "s1", "title": "Research pass" }),
            ),
            (
                SESSION_ORGANIZATION_PATH,
                json!({ "sessionID": "s1", "pinned": true }),
            ),
            (
                SESSION_ORGANIZATION_PATH,
                json!({ "sessionID": "s1", "notifyWhenDone": true }),
            ),
            (
                SESSION_ORGANIZATION_PATH,
                json!({ "sessionID": "s1", "archived": true }),
            ),
            (
                SESSION_ORGANIZATION_PATH,
                json!({ "sessionID": "s1", "archived": false }),
            ),
            (
                SESSION_ACTION_PATH,
                json!({ "sessionID": "s1", "action": "stop" }),
            ),
            (
                SESSION_ACTION_PATH,
                json!({ "sessionID": "s1", "action": "remove" }),
            ),
            (RESTART_SESSION_PATH, json!({ "sessionID": "s1" })),
            (
                SESSION_ACTION_PATH,
                json!({ "sessionID": "s1", "action": "restart_agent" }),
            ),
            (
                SESSION_ACTION_PATH,
                json!({ "sessionID": "s1", "action": "resume_agent" }),
            ),
            (
                SESSION_ORDER_PATH,
                json!({ "projectID": "p1", "orderedSessionIDs": ["s1", "s2"] }),
            ),
            (
                PROJECT_ORGANIZATION_PATH,
                json!({ "projectID": "p1", "sortOrder": 2 }),
            ),
            (
                PROJECT_ORGANIZATION_PATH,
                json!({
                    "projectID": "p1",
                    "displayName": "Backlog",
                    "colorID": "sky",
                    "dateSorted": true,
                    "pinned": true
                }),
            ),
        ] {
            connection.push(reply_step(
                expected_effect(generation, path, body),
                generation,
                200,
                ok(),
            ));
        }
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let order = ["s1".to_owned(), "s2".to_owned()];
        assert_eq!(
            backend
                .set_session_title("s1", "Research pass")
                .unwrap()
                .request_id(),
            2
        );
        assert_eq!(
            backend.set_session_pinned("s1", true).unwrap().request_id(),
            3
        );
        assert_eq!(
            backend
                .set_session_notify_when_done("s1", true)
                .unwrap()
                .request_id(),
            4
        );
        assert_eq!(backend.archive_session("s1").unwrap().request_id(), 5);
        assert_eq!(backend.restore_session("s1").unwrap().request_id(), 6);
        assert_eq!(backend.stop_session("s1").unwrap().request_id(), 7);
        assert_eq!(backend.remove_session("s1").unwrap().request_id(), 8);
        assert_eq!(backend.restart_session("s1").unwrap().request_id(), 9);
        assert_eq!(backend.restart_agent("s1").unwrap().request_id(), 10);
        assert_eq!(backend.resume_agent("s1").unwrap().request_id(), 11);
        assert_eq!(
            backend
                .set_session_order("p1", &order)
                .unwrap()
                .request_id(),
            12
        );
        assert_eq!(
            backend
                .set_project_organization("p1", &RemoteProjectOrganizationPatch::sort_order(2))
                .unwrap()
                .request_id(),
            13
        );
        assert_eq!(
            backend
                .set_project_organization(
                    "p1",
                    &RemoteProjectOrganizationPatch {
                        display_name: Some("Backlog".to_owned()),
                        color_id: Some("sky".to_owned()),
                        date_sorted: Some(true),
                        pinned: Some(true),
                        sort_order: None,
                    }
                )
                .unwrap()
                .request_id(),
            14
        );
        assert_eq!(connection.remaining(), 0);
        assert!(connection
            .calls()
            .iter()
            .skip(1)
            .all(|call| call.semantics == RequestSemantics::Effect));
    }

    #[test]
    fn lifecycle_verbs_require_their_advertised_capability() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, OUTPUT_CAPABILITY]),
            ),
        );
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let order = ["s1".to_owned()];
        let create = RemoteSessionCreateRequest::from_preset("p1", "preset-1");
        let cases: Vec<(RemoteEffectFailure, &str)> = vec![
            (
                backend.set_session_title("s1", "Renamed").unwrap_err(),
                TITLE_SET_CAPABILITY,
            ),
            (
                backend.set_session_pinned("s1", false).unwrap_err(),
                PIN_SET_CAPABILITY,
            ),
            (
                backend
                    .set_session_notify_when_done("s1", true)
                    .unwrap_err(),
                NOTIFY_WHEN_DONE_CAPABILITY,
            ),
            (
                backend.archive_session("s1").unwrap_err(),
                ARCHIVE_CAPABILITY,
            ),
            (
                backend.restore_session("s1").unwrap_err(),
                RESTORE_CAPABILITY,
            ),
            (backend.stop_session("s1").unwrap_err(), STOP_CAPABILITY),
            (backend.remove_session("s1").unwrap_err(), REMOVE_CAPABILITY),
            (
                backend.restart_session("s1").unwrap_err(),
                RESTART_CAPABILITY,
            ),
            (
                backend.restart_agent("s1").unwrap_err(),
                RESTART_AGENT_CAPABILITY,
            ),
            (
                backend.resume_agent("s1").unwrap_err(),
                RESUME_AGENT_CAPABILITY,
            ),
            (
                backend.set_session_order("p1", &order).unwrap_err(),
                ORDER_SET_CAPABILITY,
            ),
            (
                backend
                    .set_project_organization("p1", &RemoteProjectOrganizationPatch::sort_order(0))
                    .unwrap_err(),
                PROJECT_ORGANIZATION_CAPABILITY,
            ),
            (
                backend.create_session(&create).unwrap_err(),
                CREATE_CAPABILITY,
            ),
        ];
        for (failure, expected) in cases {
            assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
            assert!(matches!(
                failure.error(),
                RemoteSessionBackendError::MissingCapability(capability)
                    if capability == expected
            ));
        }
        assert!(matches!(
            backend.list_archived_sessions("p1").unwrap_err(),
            RemoteSessionBackendError::MissingCapability(capability)
                if capability == ARCHIVE_LIST_CAPABILITY
        ));
        assert!(matches!(
            backend.read_transcript_markdown("s1", None).unwrap_err(),
            RemoteSessionBackendError::MissingCapability(capability)
                if capability == TRANSCRIPT_MARKDOWN_CAPABILITY
        ));
        assert!(matches!(
            backend.read_session_metrics("s1").unwrap_err(),
            RemoteSessionBackendError::MissingCapability(capability)
                if capability == METRICS_CAPABILITY
        ));
        assert_eq!(connection.calls().len(), 1, "a gated verb reached the Host");
        assert!(!backend.needs_bootstrap());
    }

    #[test]
    fn approval_answer_is_generation_bound_effect_with_bounded_id() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, APPROVAL_ANSWER_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_effect(
                generation,
                APPROVAL_ANSWER_PATH,
                json!({ "id": "approval-1", "approved": true }),
            ),
            generation,
            200,
            br#"{"ok":true}"#.to_vec(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();
        assert_eq!(
            backend
                .answer_approval("approval-1", true)
                .unwrap()
                .request_id(),
            2
        );
        assert!(backend.answer_approval("", false).is_err());
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn lifecycle_validation_fails_before_any_bound_dispatch() {
        let connection = ScriptedConnection::new();
        let backend = RemoteSessionBackend::new(connection.clone());
        let oversized_title = "x".repeat(REMOTE_SESSION_TITLE_MAX_BYTES + 1);
        let too_many_ids = vec!["s1".to_owned(); REMOTE_SESSION_ORDER_MAX_IDS + 1];
        let traversal_ids = ["../escape".to_owned()];
        for failure in [
            backend.set_session_title("s1", "   \n  ").unwrap_err(),
            backend
                .set_session_title("s1", &oversized_title)
                .unwrap_err(),
            backend.set_session_title("../escape", "ok").unwrap_err(),
            backend.set_session_pinned("bad/id", true).unwrap_err(),
            backend.archive_session("").unwrap_err(),
            backend.restore_session("").unwrap_err(),
            backend.stop_session("bad\\id").unwrap_err(),
            backend.remove_session("..").unwrap_err(),
            backend.restart_session("\0").unwrap_err(),
            backend.restart_agent("\0").unwrap_err(),
            backend.resume_agent("\0").unwrap_err(),
            backend.set_session_order("", &[]).unwrap_err(),
            backend.set_session_order("p1", &too_many_ids).unwrap_err(),
            backend.set_session_order("p1", &traversal_ids).unwrap_err(),
            backend
                .set_project_organization(
                    "../escape",
                    &RemoteProjectOrganizationPatch::sort_order(0),
                )
                .unwrap_err(),
            backend
                .set_project_organization("p1", &RemoteProjectOrganizationPatch::default())
                .unwrap_err(),
            backend
                .set_project_organization("p1", &RemoteProjectOrganizationPatch::sort_order(-1))
                .unwrap_err(),
            backend
                .set_project_organization(
                    "p1",
                    &RemoteProjectOrganizationPatch {
                        display_name: Some("   ".to_owned()),
                        ..RemoteProjectOrganizationPatch::default()
                    },
                )
                .unwrap_err(),
            backend
                .create_session(&RemoteSessionCreateRequest::default())
                .unwrap_err(),
            backend
                .create_session(&RemoteSessionCreateRequest {
                    project_id: "p1".to_owned(),
                    ..RemoteSessionCreateRequest::default()
                })
                .unwrap_err(),
            backend
                .create_session(&RemoteSessionCreateRequest::from_command("p1", "  "))
                .unwrap_err(),
            backend
                .create_session(&RemoteSessionCreateRequest::from_command(
                    "p1",
                    "x".repeat(REMOTE_CREATE_COMMAND_MAX_BYTES + 1),
                ))
                .unwrap_err(),
            backend
                .create_session(&RemoteSessionCreateRequest {
                    initial_text: Some("x".repeat(REMOTE_CREATE_INITIAL_TEXT_MAX_BYTES + 1)),
                    ..RemoteSessionCreateRequest::from_preset("p1", "preset-1")
                })
                .unwrap_err(),
        ] {
            assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        }
        assert!(matches!(
            backend.list_archived_sessions("../escape").unwrap_err(),
            RemoteSessionBackendError::InvalidProjectId
        ));
        assert!(matches!(
            backend
                .read_transcript_markdown("bad/id", None)
                .unwrap_err(),
            RemoteSessionBackendError::InvalidSessionId
        ));
        assert!(matches!(
            backend.read_session_metrics("bad/id").unwrap_err(),
            RemoteSessionBackendError::InvalidSessionId
        ));
        assert!(connection.calls().is_empty());
    }

    #[test]
    fn session_create_returns_the_created_session_receipt() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, CREATE_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_effect(
                generation,
                SESSIONS_CREATE_PATH,
                json!({ "projectID": "p1", "presetID": "preset-1" }),
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "new-1",
                "capturedAtUnixMs": 42,
            }))
            .unwrap(),
        ));
        connection.push(reply_step(
            expected_effect(
                generation,
                SESSIONS_CREATE_PATH,
                json!({ "projectID": "p1", "command": "" }),
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "new-3",
                "capturedAtUnixMs": 44,
            }))
            .unwrap(),
        ));
        connection.push(reply_step(
            expected_effect(
                generation,
                SESSIONS_CREATE_PATH,
                json!({
                    "projectID": "p1",
                    "command": "claude",
                    "initialText": "Summarize the repo",
                    "initialTextSubmitMode": "pasteAndSubmit",
                }),
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "new-2",
                "capturedAtUnixMs": 43,
                "session": {
                    "id": "new-2",
                    "projectID": "p1",
                    "title": "claude",
                    "command": "claude",
                    "createdAtUnixMs": 43,
                    "status": "running",
                    "activity": "starting",
                },
            }))
            .unwrap(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let created = backend
            .create_session(&RemoteSessionCreateRequest::from_preset("p1", "preset-1"))
            .unwrap();
        assert_eq!(created.session_id, "new-1");
        assert_eq!(created.captured_at_unix_ms, Some(42));
        assert!(created.session.is_none());
        assert_eq!(created.receipt.request_id(), 2);

        let blank = backend
            .create_session(&RemoteSessionCreateRequest::from_command("p1", ""))
            .unwrap();
        assert_eq!(blank.session_id, "new-3");

        let with_summary = backend
            .create_session(&RemoteSessionCreateRequest {
                initial_text: Some("Summarize the repo".to_owned()),
                ..RemoteSessionCreateRequest::from_command("p1", "claude")
            })
            .unwrap();
        assert_eq!(with_summary.session_id, "new-2");
        let summary = with_summary.session.expect("optimistic summary");
        assert_eq!(summary.id, "new-2");
        assert_eq!(summary.activity, RemoteActivityState::Starting);
        assert_eq!(connection.remaining(), 0);
        assert!(!backend.needs_bootstrap());
    }

    #[test]
    fn pairing_invitation_uses_the_generation_bound_effect_contract() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, PAIRING_INVITATION_CAPABILITY]),
            ),
        );
        let request = json!({
            "action": "create",
            "endpoint": "http://192.168.1.2:41000/mobile/pairing-proxy/INVITE-1"
        });
        let response = json!({
            "protocolVersion": 1,
            "macID": "host-1",
            "macName": "Studio",
            "endpoint": request["endpoint"],
            "token": "TOKEN",
            "expiresAtUnixMs": 1234
        });
        connection.push(reply_step(
            expected_effect(generation, PAIRING_INVITATION_PATH, request.clone()),
            generation,
            200,
            serde_json::to_vec(&response).unwrap(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let body = backend
            .pairing_invitation(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), response);
        assert_eq!(connection.remaining(), 0);
        assert!(!backend.needs_bootstrap());
    }

    #[test]
    fn session_create_with_untrusted_receipt_is_outcome_unknown() {
        for body in [
            br#"{"ok":true}"#.to_vec(),
            serde_json::to_vec(&json!({ "sessionID": "../escape" })).unwrap(),
        ] {
            let connection = ScriptedConnection::new();
            let generation = connection.generation(1);
            add_bootstrap(
                &connection,
                generation,
                bootstrap_json(
                    Some("host-1"),
                    HOST_PROTOCOL_MAJOR,
                    Some(&[BOOTSTRAP_CAPABILITY, CREATE_CAPABILITY]),
                ),
            );
            connection.push(reply_step(
                expected_effect(
                    generation,
                    SESSIONS_CREATE_PATH,
                    json!({ "projectID": "p1", "presetID": "preset-1" }),
                ),
                generation,
                200,
                body,
            ));
            let backend = RemoteSessionBackend::new(connection.clone());
            backend.bootstrap().unwrap();

            let failure = backend
                .create_session(&RemoteSessionCreateRequest::from_preset("p1", "preset-1"))
                .unwrap_err();
            assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
            assert!(
                backend.needs_bootstrap(),
                "an untrusted create receipt must tear down the generation"
            );
            assert_eq!(connection.remaining(), 0);
        }
    }

    #[test]
    fn archived_sessions_read_is_generation_bound_and_typed() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, ARCHIVE_LIST_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_read(
                generation,
                ARCHIVE_LIST_PATH,
                vec![("project_id".to_owned(), "p1".to_owned())],
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "projectID": "p1",
                "sessions": [{
                    "id": "old-1",
                    "projectID": "p1",
                    "title": "Filed",
                    "command": "claude",
                    "createdAtUnixMs": 1,
                    "status": "exited",
                    "activity": "idle",
                    "archived": true,
                }],
            }))
            .unwrap(),
        ));
        connection.push(reply_step(
            expected_read(
                generation,
                ARCHIVE_LIST_PATH,
                vec![("project_id".to_owned(), "p1".to_owned())],
            ),
            generation,
            200,
            serde_json::to_vec(&json!({ "projectID": "other", "sessions": [] })).unwrap(),
        ));
        connection.push(reply_step(
            expected_read(
                generation,
                ARCHIVE_LIST_PATH,
                vec![("project_id".to_owned(), "p-unknown".to_owned())],
            ),
            generation,
            404,
            br#"{"error":"unknown project"}"#.to_vec(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let archived = backend.list_archived_sessions("p1").unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id, "old-1");
        assert!(archived[0].archived);
        assert_eq!(archived[0].status, RemoteSessionStatus::Exited);

        assert!(matches!(
            backend.list_archived_sessions("p1").unwrap_err(),
            RemoteSessionBackendError::InvalidResponse {
                operation: "archived Sessions",
                ..
            }
        ));
        assert!(matches!(
            backend.list_archived_sessions("p-unknown").unwrap_err(),
            RemoteSessionBackendError::HostStatus { status: 404, .. }
        ));
        assert!(
            !backend.needs_bootstrap(),
            "a correlated read rejection must keep the generation callable"
        );
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn transcript_markdown_read_uses_the_v1_query_contract() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, TRANSCRIPT_MARKDOWN_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_read(
                generation,
                TRANSCRIPT_MARKDOWN_PATH,
                vec![
                    ("session_id".to_owned(), "s1".to_owned()),
                    ("entries".to_owned(), "3".to_owned()),
                ],
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "s1",
                "markdown": "## Turn\n\nHei",
            }))
            .unwrap(),
        ));
        connection.push(reply_step(
            expected_read(
                generation,
                TRANSCRIPT_MARKDOWN_PATH,
                vec![("session_id".to_owned(), "s1".to_owned())],
            ),
            generation,
            502,
            br#"{"error":"no transcript source"}"#.to_vec(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let transcript = backend.read_transcript_markdown("s1", Some(3)).unwrap();
        assert_eq!(transcript.session_id, "s1");
        assert_eq!(transcript.markdown, "## Turn\n\nHei");

        assert!(matches!(
            backend.read_transcript_markdown("s1", None).unwrap_err(),
            RemoteSessionBackendError::HostStatus { status: 502, .. }
        ));
        assert!(!backend.needs_bootstrap());
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn session_metrics_read_uses_the_v1_query_contract() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[BOOTSTRAP_CAPABILITY, METRICS_CAPABILITY]),
            ),
        );
        connection.push(reply_step(
            expected_read(
                generation,
                METRICS_PATH,
                vec![("session_id".to_owned(), "s1".to_owned())],
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "s1",
                "columns": 121,
                "rows": 33,
                "outputOffset": 8192,
                "capturedAtUnixMs": 1234,
                "desktopViewing": false,
            }))
            .unwrap(),
        ));
        // A Host that predates `outputOffset` in the gateway metrics body
        // still satisfies the read — the grid is what fit math needs.
        connection.push(reply_step(
            expected_read(
                generation,
                METRICS_PATH,
                vec![("session_id".to_owned(), "s1".to_owned())],
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "s1",
                "columns": 80,
                "rows": 24,
                "capturedAtUnixMs": 1235,
            }))
            .unwrap(),
        ));
        connection.push(reply_step(
            expected_read(
                generation,
                METRICS_PATH,
                vec![("session_id".to_owned(), "s1".to_owned())],
            ),
            generation,
            200,
            serde_json::to_vec(&json!({
                "sessionID": "other",
                "columns": 80,
                "rows": 24,
                "capturedAtUnixMs": 1236,
            }))
            .unwrap(),
        ));
        connection.push(reply_step(
            expected_read(
                generation,
                METRICS_PATH,
                vec![("session_id".to_owned(), "s1".to_owned())],
            ),
            generation,
            409,
            br#"{"error":"session has exited"}"#.to_vec(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let metrics = backend.read_session_metrics("s1").unwrap();
        assert_eq!(
            metrics,
            RemoteSessionMetrics {
                session_id: "s1".to_owned(),
                columns: 121,
                rows: 33,
                output_offset: Some(8192),
                captured_at_unix_ms: 1234,
            }
        );

        let compat = backend.read_session_metrics("s1").unwrap();
        assert_eq!(compat.output_offset, None);
        assert_eq!((compat.columns, compat.rows), (80, 24));

        assert!(matches!(
            backend.read_session_metrics("s1").unwrap_err(),
            RemoteSessionBackendError::InvalidResponse {
                operation: "session metrics",
                ..
            }
        ));
        assert!(matches!(
            backend.read_session_metrics("s1").unwrap_err(),
            RemoteSessionBackendError::HostStatus { status: 409, .. }
        ));
        assert!(
            !backend.needs_bootstrap(),
            "a correlated read rejection must keep the generation callable"
        );
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn lifecycle_semantic_rejection_is_not_applied_and_keeps_generation() {
        let connection = ScriptedConnection::new();
        let generation = connection.generation(1);
        add_bootstrap(
            &connection,
            generation,
            bootstrap_json(
                Some("host-1"),
                HOST_PROTOCOL_MAJOR,
                Some(&[
                    BOOTSTRAP_CAPABILITY,
                    STOP_CAPABILITY,
                    RESTART_CAPABILITY,
                    RESTART_AGENT_CAPABILITY,
                ]),
            ),
        );
        connection.push(reply_step(
            expected_effect(
                generation,
                SESSION_ACTION_PATH,
                json!({ "sessionID": "s1", "action": "restart_agent" }),
            ),
            generation,
            409,
            br#"{"error":"No managed agent to restart: s1"}"#.to_vec(),
        ));
        connection.push(reply_step(
            expected_effect(
                generation,
                SESSION_ACTION_PATH,
                json!({ "sessionID": "s1", "action": "stop" }),
            ),
            generation,
            409,
            br#"{"error":"Session is not running: s1"}"#.to_vec(),
        ));
        connection.push(reply_step(
            expected_effect(
                generation,
                RESTART_SESSION_PATH,
                json!({ "sessionID": "s1" }),
            ),
            generation,
            200,
            br#"{"ok":true}"#.to_vec(),
        ));
        let backend = RemoteSessionBackend::new(connection.clone());
        backend.bootstrap().unwrap();

        let failure = backend.restart_agent("s1").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        assert!(matches!(
            failure.error(),
            RemoteSessionBackendError::HostStatus { status: 409, .. }
        ));
        assert!(!backend.needs_bootstrap());

        let failure = backend.stop_session("s1").unwrap_err();
        assert_eq!(failure.kind(), RemoteEffectFailureKind::NotApplied);
        assert!(matches!(
            failure.error(),
            RemoteSessionBackendError::HostStatus { status: 409, .. }
        ));
        assert!(!backend.needs_bootstrap());

        backend.restart_session("s1").unwrap();
        assert_eq!(connection.remaining(), 0);
    }

    #[test]
    fn output_validation_happens_before_any_connection_call() {
        let connection = ScriptedConnection::new();
        let backend = RemoteSessionBackend::new(connection.clone());
        assert!(matches!(
            backend
                .poll_output("../escape", RemoteOutputPollOptions::default())
                .unwrap_err(),
            RemoteSessionBackendError::InvalidSessionId
        ));
        assert!(matches!(
            backend
                .poll_output(
                    "s1",
                    RemoteOutputPollOptions {
                        limit: REMOTE_OUTPUT_MAX_LIMIT + 1,
                        wait: Duration::ZERO,
                    },
                )
                .unwrap_err(),
            RemoteSessionBackendError::InvalidOutputOptions(_)
        ));
        assert!(connection.calls().is_empty());
    }
}
