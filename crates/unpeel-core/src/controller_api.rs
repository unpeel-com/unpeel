//! Transport-neutral Controller → Host request routing.
//!
//! HTTP, SSH stdio, and Link Relay are adapters around this contract. They
//! authenticate a principal, translate their framing into `ControllerRequest`,
//! and serialize `ControllerResponse`; Host/session semantics live here. The
//! first migration slice owns bootstrap construction and read-only terminal
//! core session actions. Add routes here as the native and TUI Host adapters
//! converge; keep platform services at the authenticated adapter boundary.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::controller_protocol::HostProtocolDescriptor;
use crate::session_artifacts;
use crate::session_host::{self, HostedSessionState, SessionHostCommand};
use crate::session_input;
use crate::session_ops;
use crate::state::{current_timestamp_ms, initial_session_label, SessionInfo};
use crate::transcripts;

const SESSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const SESSION_CREATE_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_CREATE_INITIAL_COLUMNS: u16 = 120;
const SESSION_CREATE_INITIAL_ROWS: u16 = 32;
const REPLAY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const REPLAY_CACHE_ENTRIES: usize = 512;
const MAX_REQUEST_ID_BYTES: usize = 128;
const MAX_CREATE_ID_BYTES: usize = 256;
const MAX_CREATE_COMMAND_BYTES: usize = 64 * 1024;
const MAX_CREATE_PATH_BYTES: usize = 16 * 1024;
const MAX_CREATE_BRANCH_BYTES: usize = 4 * 1024;
const MAX_CREATE_INITIAL_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ControllerPrincipal {
    /// A durable device credential minted by Host pairing.
    PairedDevice {
        device_id: String,
        name: String,
        /// Stable human identity. Missing on pre-ownership adapters and old
        /// device stores; create then falls back to the Host owner supplied by
        /// the authenticated adapter context.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal_id: Option<String>,
    },
    /// An owner-equivalent local/SSH/server-token transport. `subject` is
    /// diagnostic identity such as an SSH user, never authorization by itself.
    OwnerTransport {
        transport: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: HashMap<String, String>,
    #[serde(default)]
    pub body: Value,
    /// MIME type of the original body, without carrying transport auth
    /// headers into the semantic router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Binary bodies use standard padded base64. JSON routes keep this absent
    /// and use `body`; adapters can therefore carry uploads through the same
    /// envelope without lossy UTF-8 conversion or a second protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    pub principal: ControllerPrincipal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub status: u16,
    pub body: Value,
}

impl ControllerResponse {
    pub fn body_json(&self) -> String {
        self.body.to_string()
    }
}

/// Adapter-owned pieces that enrich the common bootstrap snapshot. The
/// snapshot is already in the shipped v1 mobile DTO dialect; this router owns
/// the protocol/version fields so an adapter cannot become a second capability
/// authority.
#[derive(Debug, Clone)]
pub struct HostBootstrapContext {
    pub snapshot: Value,
    pub host_id: Option<String>,
    pub remote_server_port: Option<u16>,
    pub remote_server_certificate_fingerprint: Option<String>,
    pub pending_approvals: Vec<Value>,
    pub protocol: HostProtocolDescriptor,
}

impl HostBootstrapContext {
    pub fn headless(snapshot: Value) -> Self {
        Self {
            snapshot,
            host_id: None,
            remote_server_port: None,
            remote_server_certificate_fingerprint: None,
            pending_approvals: Vec::new(),
            protocol: HostProtocolDescriptor::headless_v1(),
        }
    }
}

/// Adapter-resolved data needed by transport-neutral Host routes.
///
/// Bootstrap and archive data deliberately remain separate: bootstrap's
/// shipped snapshot is merged into its public response, while the archive map
/// can contain every filed Session and must only cross the wire for an
/// authenticated `/mobile/archive` request. A present project key with an
/// empty list means "known project, nothing archived"; an absent key means an
/// unknown project.
#[derive(Debug, Clone, Default)]
pub struct HostRouteContext {
    pub bootstrap: Option<HostBootstrapContext>,
    pub archived_sessions_by_project: HashMap<String, Vec<Value>>,
}

/// Host-owned project data exposed to the shared create resolver. Controllers
/// may identify one of these entries, but paths and worktree metadata always
/// come back from this catalog rather than being trusted from the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCreateProject {
    pub id: String,
    pub path: String,
    pub is_folder: bool,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
}

/// Host-owned preset data exposed to the shared create resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCreatePreset {
    pub id: String,
    pub command: String,
    pub enabled: bool,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostCreateSubmitMode {
    PasteOnly,
    #[default]
    PasteAndSubmit,
    Raw,
}

/// Fully Host-resolved create input passed across the adapter's effect
/// boundary. No Controller-supplied filesystem path survives into this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHostCreate {
    pub project_id: String,
    pub command: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub worktree_branch: Option<String>,
    /// Host-authenticated attribution. These fields are not present in the
    /// Controller's create DTO and are filled only after authorization.
    pub owner_principal_id: String,
    pub created_by_device_id: Option<String>,
    pub source_preset_id: Option<String>,
    pub initial_text: Option<String>,
    pub initial_text_submit_mode: HostCreateSubmitMode,
}

/// Adapter result for a newly created Session. `session` is the optional
/// optimistic wire summary used by newer Controllers while headless adapters
/// may omit it and let the next bootstrap publish the row.
#[derive(Debug, Clone, PartialEq)]
pub struct HostCreateOutcome {
    pub session_id: String,
    pub session: Option<Value>,
}

pub type HostCreateExecutor =
    Arc<dyn Fn(ResolvedHostCreate) -> Result<HostCreateOutcome, String> + Send + Sync + 'static>;

/// Adapter-supplied create catalog plus a fakeable execution boundary. The
/// TUI executor captures its hook-listener port; native deliberately supplies
/// no context until its richer Swift-owned launch path is migrated intact.
#[derive(Clone)]
pub struct HostCreateContext {
    pub host_owner_principal_id: String,
    pub projects: Vec<HostCreateProject>,
    pub presets: Vec<HostCreatePreset>,
    executor: HostCreateExecutor,
}

impl HostCreateContext {
    pub fn new(
        host_owner_principal_id: String,
        projects: Vec<HostCreateProject>,
        presets: Vec<HostCreatePreset>,
        executor: HostCreateExecutor,
    ) -> Self {
        Self {
            host_owner_principal_id,
            projects,
            presets,
            executor,
        }
    }

    fn execute(&self, request: ResolvedHostCreate) -> Result<HostCreateOutcome, String> {
        (self.executor)(request)
    }
}

/// Session lifecycle verbs exposed by the shipped Controller protocol. Archive
/// and restore remain fields of `/mobile/session-organization`; adding them
/// here would silently create a second wire dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerSessionAction {
    Stop,
    Restart,
    RestartAgent,
    ResumeAgent,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerSessionActionRequest {
    pub session_id: String,
    pub action: ControllerSessionAction,
}

/// Typed adapter failures let the common router preserve the shipped HTTP
/// contract without parsing platform-specific error strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerEffectError {
    UnknownSession,
    SessionNotRunning,
    SessionStillRunning,
    AgentNotRestartable,
    Failed(String),
}

pub type ControllerSessionActionExecutor = Arc<
    dyn Fn(ControllerSessionActionRequest) -> Result<(), ControllerEffectError>
        + Send
        + Sync
        + 'static,
>;

/// Adapter-owned effect boundary for Host mutations whose cleanup still
/// differs by frontend. Headless adapters provide real shared-core effects;
/// native deliberately provides no instance until its richer Swift cleanup
/// has moved behind this boundary intact.
#[derive(Clone)]
pub struct ControllerEffects {
    session_action_executor: ControllerSessionActionExecutor,
}

impl ControllerEffects {
    pub fn new(session_action_executor: ControllerSessionActionExecutor) -> Self {
        Self {
            session_action_executor,
        }
    }

    fn execute_session_action(
        &self,
        request: ControllerSessionActionRequest,
    ) -> Result<(), ControllerEffectError> {
        (self.session_action_executor)(request)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostCreateWireRequest {
    #[serde(rename = "projectID", alias = "projectId")]
    project_id: String,
    #[serde(default, rename = "presetID", alias = "presetId")]
    preset_id: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    worktree_path: Option<String>,
    #[serde(default)]
    worktree_branch: Option<String>,
    #[serde(default)]
    initial_text: Option<String>,
    #[serde(default)]
    initial_text_submit_mode: HostCreateSubmitMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerApiError {
    pub status: u16,
    pub message: String,
}

struct ReplayEntry {
    principal: ControllerPrincipal,
    request_id: String,
    fingerprint: [u8; 32],
    inserted_at: Instant,
    state: Mutex<ReplayState>,
    ready: Condvar,
}

enum ReplayState {
    InFlight,
    Complete(Option<ControllerResponse>),
}

impl ReplayEntry {
    fn new(
        principal: ControllerPrincipal,
        request_id: String,
        fingerprint: [u8; 32],
        inserted_at: Instant,
    ) -> Self {
        Self {
            principal,
            request_id,
            fingerprint,
            inserted_at,
            state: Mutex::new(ReplayState::InFlight),
            ready: Condvar::new(),
        }
    }

    fn wait_for_response(&self) -> Option<ControllerResponse> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            match &*state {
                ReplayState::InFlight => {
                    state = self
                        .ready
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                ReplayState::Complete(response) => return response.clone(),
            }
        }
    }

    fn complete(&self, response: Option<ControllerResponse>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = ReplayState::Complete(response);
        self.ready.notify_all();
    }

    fn is_in_flight(&self) -> bool {
        matches!(
            *self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ReplayState::InFlight
        )
    }

    fn is_expired(&self, now: Instant) -> bool {
        !self.is_in_flight() && now.duration_since(self.inserted_at) > REPLAY_CACHE_TTL
    }
}

/// Ensures a panicking mutation leader cannot strand every matching retry on
/// an in-flight condition variable forever. The leader still unwinds; only
/// followers receive this synthetic terminal response.
struct ReplayLeaderGuard {
    entry: Arc<ReplayEntry>,
    failure: Option<ControllerResponse>,
}

impl ReplayLeaderGuard {
    fn new(entry: Arc<ReplayEntry>, request_id: Option<String>) -> Self {
        Self {
            entry,
            failure: Some(ControllerResponse {
                id: request_id,
                status: 500,
                body: json!({ "error": "request processing aborted" }),
            }),
        }
    }

    fn complete(&mut self, response: Option<ControllerResponse>) {
        self.entry.complete(response);
        self.failure = None;
    }
}

impl Drop for ReplayLeaderGuard {
    fn drop(&mut self) {
        if let Some(response) = self.failure.take() {
            self.entry.complete(Some(response));
        }
    }
}

type ReplayCache = VecDeque<Arc<ReplayEntry>>;

static REPLAY_CACHE: OnceLock<Mutex<ReplayCache>> = OnceLock::new();

impl ControllerApiError {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

fn bounded_create_identifier(value: &str, field: &str) -> Result<String, ControllerApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_CREATE_ID_BYTES || value.contains('\0') {
        return Err(ControllerApiError::new(400, format!("invalid {field}")));
    }
    Ok(value.to_owned())
}

fn validate_optional_create_value(
    value: Option<&str>,
    max_bytes: usize,
    field: &str,
) -> Result<(), ControllerApiError> {
    if let Some(value) = value {
        if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
            return Err(ControllerApiError::new(400, format!("invalid {field}")));
        }
    }
    Ok(())
}

fn validate_host_create_value(
    value: &str,
    max_bytes: usize,
    field: &str,
) -> Result<(), ControllerApiError> {
    if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
        return Err(ControllerApiError::new(
            500,
            format!("invalid Host {field}"),
        ));
    }
    Ok(())
}

fn principal_can_create_session(principal: &ControllerPrincipal) -> bool {
    // Keep this match exhaustive. When scoped Link/Room principals are added,
    // the compiler must force an explicit authorization decision here instead
    // of silently granting them owner powers through a wildcard arm.
    match principal {
        ControllerPrincipal::PairedDevice { .. } | ControllerPrincipal::OwnerTransport { .. } => {
            true
        }
    }
}

fn resolve_host_create(
    body: &Value,
    context: &HostCreateContext,
) -> Result<ResolvedHostCreate, ControllerApiError> {
    let request: HostCreateWireRequest = serde_json::from_value(body.clone())
        .map_err(|_| ControllerApiError::new(400, "request failed"))?;
    let project_id = bounded_create_identifier(&request.project_id, "project id")?;
    let preset_id = request
        .preset_id
        .as_deref()
        .map(|value| bounded_create_identifier(value, "preset id"))
        .transpose()?;

    if let Some(command) = request.command.as_deref() {
        if command.len() > MAX_CREATE_COMMAND_BYTES || command.contains('\0') {
            return Err(ControllerApiError::new(400, "invalid command"));
        }
    }
    validate_optional_create_value(
        request.worktree_path.as_deref(),
        MAX_CREATE_PATH_BYTES,
        "worktree path",
    )?;
    validate_optional_create_value(
        request.worktree_branch.as_deref(),
        MAX_CREATE_BRANCH_BYTES,
        "worktree branch",
    )?;
    if request
        .initial_text
        .as_ref()
        .is_some_and(|value| value.len() > MAX_CREATE_INITIAL_TEXT_BYTES)
    {
        return Err(ControllerApiError::new(400, "initial text too large"));
    }

    let project = context
        .projects
        .iter()
        .find(|candidate| candidate.id == project_id)
        .ok_or_else(|| ControllerApiError::new(400, format!("unknown project id: {project_id}")))?;
    // Plain sidebar groups are folder records that carry their parent
    // project's path (native `promptCreateGroup`, TUI `add_group_to_app_state`).
    // Launching "inside" a group is ordinary: the Session keeps the group as
    // its project id and runs in the parent's directory, exactly like the
    // local spawn path always did. Only a folder with no path is unlaunchable.
    if project.is_folder && project.path.trim().is_empty() {
        return Err(ControllerApiError::new(400, "project is a folder"));
    }
    validate_host_create_value(&project.path, MAX_CREATE_PATH_BYTES, "project path")?;
    if let Some(path) = project.worktree_path.as_deref() {
        validate_host_create_value(path, MAX_CREATE_PATH_BYTES, "worktree path")?;
    }
    if let Some(branch) = project.worktree_branch.as_deref() {
        validate_host_create_value(branch, MAX_CREATE_BRANCH_BYTES, "worktree branch")?;
    }

    // Request worktree fields are compatibility assertions only. They can
    // confirm the Host-selected project target but can never introduce a path
    // or branch that is absent from the Host catalog.
    if request
        .worktree_path
        .as_deref()
        .is_some_and(|requested| project.worktree_path.as_deref() != Some(requested))
        || request
            .worktree_branch
            .as_deref()
            .is_some_and(|requested| project.worktree_branch.as_deref() != Some(requested))
    {
        return Err(ControllerApiError::new(
            400,
            "worktree does not belong to project",
        ));
    }

    let command = if let Some(preset_id) = preset_id.as_deref() {
        // Prefer a project-scoped row if a legacy catalog happens to contain
        // the same id globally too. Either way, preset selection wins over an
        // explicit command in the wire request, matching shipped clients.
        let preset = context
            .presets
            .iter()
            .find(|preset| {
                preset.id == preset_id
                    && preset.enabled
                    && preset.project_id.as_deref() == Some(project_id.as_str())
            })
            .or_else(|| {
                context.presets.iter().find(|preset| {
                    preset.id == preset_id && preset.enabled && preset.project_id.is_none()
                })
            })
            .ok_or_else(|| {
                ControllerApiError::new(400, format!("unknown preset id: {preset_id}"))
            })?;
        if preset.command.len() > MAX_CREATE_COMMAND_BYTES || preset.command.contains('\0') {
            return Err(ControllerApiError::new(500, "invalid Host preset command"));
        }
        preset.command.clone()
    } else if let Some(command) = request.command {
        command.trim().to_owned()
    } else {
        return Err(ControllerApiError::new(400, "missing presetID or command"));
    };

    Ok(ResolvedHostCreate {
        project_id,
        command,
        cwd: project
            .worktree_path
            .clone()
            .unwrap_or_else(|| project.path.clone()),
        worktree_path: project.worktree_path.clone(),
        worktree_branch: project.worktree_branch.clone(),
        // Filled from the authenticated principal by `create_session` after
        // the untrusted request has been fully resolved against Host state.
        owner_principal_id: String::new(),
        created_by_device_id: None,
        source_preset_id: preset_id,
        initial_text: request.initial_text,
        initial_text_submit_mode: request.initial_text_submit_mode,
    })
}

fn create_session(
    body: &Value,
    principal: &ControllerPrincipal,
    context: &HostCreateContext,
) -> Result<Value, ControllerApiError> {
    if !principal_can_create_session(principal) {
        return Err(ControllerApiError::new(403, "owner access required"));
    }
    let mut resolved = resolve_host_create(body, context)?;
    let (owner_principal_id, created_by_device_id) = match principal {
        ControllerPrincipal::PairedDevice {
            device_id,
            principal_id,
            ..
        } => (
            principal_id
                .as_deref()
                .filter(|value| valid_attribution_id(value))
                .unwrap_or(&context.host_owner_principal_id)
                .to_owned(),
            valid_attribution_id(device_id).then(|| device_id.clone()),
        ),
        ControllerPrincipal::OwnerTransport { principal_id, .. } => (
            principal_id
                .as_deref()
                .filter(|value| valid_attribution_id(value))
                .unwrap_or(&context.host_owner_principal_id)
                .to_owned(),
            None,
        ),
    };
    resolved.owner_principal_id = owner_principal_id;
    resolved.created_by_device_id = created_by_device_id;
    let outcome = context.execute(resolved).map_err(|message| {
        log::warn!("controller session create failed: {message}");
        ControllerApiError::new(500, "failed to create session")
    })?;
    if !valid_session_id(&outcome.session_id) || outcome.session_id.len() > MAX_CREATE_ID_BYTES {
        return Err(ControllerApiError::new(
            500,
            "create executor returned an invalid session id",
        ));
    }
    let mut body = json!({
        "sessionID": outcome.session_id,
        "capturedAtUnixMs": current_timestamp_ms(),
    });
    if let Some(session) = outcome.session {
        body.as_object_mut()
            .expect("create response is an object")
            .insert("session".into(), session);
    }
    Ok(body)
}

/// Principal and device ids come from an authenticated adapter, but still
/// cross a persistence boundary into manifests. Keep them opaque while
/// refusing unbounded, whitespace-padded, or control-character values.
fn valid_attribution_id(value: &str) -> bool {
    crate::state::valid_session_attribution_id(value)
}

/// Standard TUI/headless create effect. Adapters wrap this in a
/// [`HostCreateExecutor`] so they can capture their hook-listener port while
/// router tests substitute a side-effect-free callback.
pub fn execute_headless_session_create(
    request: ResolvedHostCreate,
    hook_port: Option<u16>,
) -> Result<HostCreateOutcome, String> {
    let ResolvedHostCreate {
        project_id,
        command,
        cwd,
        worktree_path,
        worktree_branch,
        owner_principal_id,
        created_by_device_id,
        source_preset_id,
        initial_text,
        initial_text_submit_mode,
    } = request;
    // Allocate the recovery handle before invoking the launcher. Once the
    // launcher accepts this id, the Controller gets a successful receipt;
    // readiness and optional initial input are best-effort follow-up work,
    // matching the shipped native Host instead of turning a live Session into
    // an opaque create failure.
    let session_id = uuid::Uuid::new_v4().to_string().to_lowercase();
    let session = SessionInfo {
        id: session_id.clone(),
        project_id,
        label: initial_session_label(&command, &cwd),
        custom_title: false,
        command,
        created_at: current_timestamp_ms(),
        owner_principal_id: Some(owner_principal_id),
        created_by_device_id,
        source_preset_id,
        tag_id: None,
        worktree_path,
        worktree_branch,
        parent_session_id: None,
        spawned_by: None,
        role: None,
        task: None,
    };
    // The create receipt is already correlated and authoritative. Return a
    // complete starting-row summary with it so Controllers can render/select
    // the new Session immediately instead of waiting for the detached Host's
    // first manifest to reach a later bootstrap.
    let optimistic_session = json!({
        "id": session.id,
        "projectID": session.project_id,
        "title": session.label,
        "command": session.command,
        "createdAtUnixMs": session.created_at,
        "updatedAtUnixMs": session.created_at,
        "ownerPrincipalID": session.owner_principal_id,
        "createdByDeviceID": session.created_by_device_id,
        "sourcePresetID": session.source_preset_id,
        "status": "running",
        "activity": "starting",
        "unread": false,
        "pinned": false,
        "worktreePath": session.worktree_path,
        "worktreeBranch": session.worktree_branch,
        "notifyWhenDone": false,
        "archived": false,
    });
    session_ops::spawn_session(
        session,
        &cwd,
        hook_port,
        SESSION_CREATE_INITIAL_COLUMNS,
        SESSION_CREATE_INITIAL_ROWS,
    )?;
    if let Some(initial_text) = initial_text.filter(|text| !text.is_empty()) {
        let mode = match initial_text_submit_mode {
            HostCreateSubmitMode::PasteOnly => session_ops::InitialTextSubmitMode::PasteOnly,
            HostCreateSubmitMode::PasteAndSubmit => {
                session_ops::InitialTextSubmitMode::PasteAndSubmit
            }
            HostCreateSubmitMode::Raw => session_ops::InitialTextSubmitMode::Raw,
        };
        let delivery_session_id = session_id.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("unpeel-create-initial-text".into())
            .spawn(move || {
                if let Err(error) = session_host::wait_until_ready(
                    &delivery_session_id,
                    SESSION_CREATE_READY_TIMEOUT,
                ) {
                    log::warn!(
                        "session {delivery_session_id} initial text skipped; Host not ready: {error}"
                    );
                    return;
                }
                if let Err(error) =
                    session_ops::deliver_initial_text(&delivery_session_id, &initial_text, mode)
                {
                    log::warn!(
                        "session {delivery_session_id} initial text delivery failed: {error}"
                    );
                }
            })
        {
            log::warn!("session {session_id} initial text worker failed to start: {error}");
        }
    }
    Ok(HostCreateOutcome {
        session_id,
        session: Some(optimistic_session),
    })
}

/// Standard TUI/headless lifecycle effect. The manifest check supplies the
/// resource/status distinctions that `session_ops` intentionally treats as
/// idempotent (notably stopping an already-dead socket).
fn classify_restart_agent_failure(message: String) -> ControllerEffectError {
    // The Host revalidates the live foreground immediately before signaling
    // it. A summary can therefore pass the Controller preflight and become
    // stale before this command executes. Keep those expected concurrency and
    // eligibility rejections as 409s without treating transport, signal, PTY
    // write, or runtime-support installation failures as user conflicts.
    if message.starts_with("session ") && message.ends_with(" is not running") {
        return ControllerEffectError::SessionNotRunning;
    }
    if message.starts_with("Agent restart generation changed")
        || message == "Agent restart requires a nonblank, known resumable launch command"
        || message == "An agent restart is already in progress"
        || message == "Session host no longer has an owned shell process"
        || message == "Session host process identity could not be verified"
        || message == "Session host has no verifiable process start time"
        || message == "Session terminal has no foreground process group"
        || message == "Terminal foreground is outside the owned session"
        || message.starts_with("Refusing to restart ")
        || message.starts_with("Refusing to resume ")
        || message == "Owned shell changed while resuming the agent"
        || message == "Owned shell lost the terminal before agent relaunch"
        || message == "An agent resume launch is pending"
        || message == "Session leader is no longer the owned shell executable"
        || message == "Session leader is no longer the owned interactive login shell"
        || message == "Session process membership could not be verified"
    {
        return ControllerEffectError::AgentNotRestartable;
    }
    ControllerEffectError::Failed(message)
}

pub fn execute_headless_session_action(
    request: ControllerSessionActionRequest,
    hook_port: Option<u16>,
) -> Result<(), ControllerEffectError> {
    let Some(manifest) = session_host::refresh_manifest_health(&request.session_id) else {
        return Err(ControllerEffectError::UnknownSession);
    };
    match request.action {
        ControllerSessionAction::Stop
        | ControllerSessionAction::RestartAgent
        | ControllerSessionAction::ResumeAgent
            if manifest.state != HostedSessionState::Running =>
        {
            return Err(ControllerEffectError::SessionNotRunning);
        }
        ControllerSessionAction::Restart if manifest.state != HostedSessionState::Exited => {
            return Err(ControllerEffectError::SessionStillRunning);
        }
        _ => {}
    }
    if matches!(
        request.action,
        ControllerSessionAction::RestartAgent | ControllerSessionAction::ResumeAgent
    ) {
        // A current Controller never routes either spelling to a surviving
        // protocol-v2 child: that old handler could stop an active runtime.
        // On v3 both the legacy decode and the new action are shell-only.
        let minimum_host_protocol = session_host::SESSION_HOST_RESUME_AGENT_PROTOCOL_VERSION;
        if manifest.runtime_launch_pending
            || manifest.host_protocol_version.unwrap_or(0) < minimum_host_protocol
            || !crate::resume::can_resume_agent(
                &manifest.session.command,
                session_host::active_runtime_id(&manifest),
            )
        {
            return Err(ControllerEffectError::AgentNotRestartable);
        }
    }

    let result = match request.action {
        ControllerSessionAction::Stop => session_ops::stop_session(&request.session_id),
        ControllerSessionAction::Restart => session_ops::resume_session(
            &request.session_id,
            hook_port,
            SESSION_CREATE_INITIAL_COLUMNS,
            SESSION_CREATE_INITIAL_ROWS,
        )
        .map(|_| ()),
        ControllerSessionAction::RestartAgent => session_ops::restart_agent(&request.session_id),
        ControllerSessionAction::ResumeAgent => session_ops::resume_agent(&request.session_id),
        ControllerSessionAction::Remove => session_ops::remove_session(&request.session_id),
    };
    result.map_err(|message| match request.action {
        ControllerSessionAction::RestartAgent | ControllerSessionAction::ResumeAgent => {
            classify_restart_agent_failure(message)
        }
        ControllerSessionAction::Restart
            if message.starts_with("session ") && message.ends_with(" is still running") =>
        {
            ControllerEffectError::SessionStillRunning
        }
        _ => ControllerEffectError::Failed(message),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetrics {
    pub session_id: String,
    pub columns: u16,
    pub rows: u16,
    pub output_offset: u64,
    pub captured_at_unix_ms: u64,
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && !session_id.contains('/')
        && !session_id.contains('\\')
        && !session_id.contains("..")
        && !session_id.contains('\0')
}

fn body_session_id(body: &Value) -> Result<String, ControllerApiError> {
    let Some(session_id) = body.get("sessionID").and_then(Value::as_str) else {
        return Err(ControllerApiError::new(400, "invalid session id"));
    };
    let session_id = session_id.trim();
    if !valid_session_id(session_id) {
        return Err(ControllerApiError::new(400, "invalid session id"));
    }
    Ok(session_id.to_owned())
}

fn query_session_id(query: &HashMap<String, String>) -> Result<String, ControllerApiError> {
    let Some(session_id) = query.get("session_id").or_else(|| query.get("sessionID")) else {
        return Err(ControllerApiError::new(400, "invalid session id"));
    };
    let session_id = session_id.trim();
    if !valid_session_id(session_id) {
        return Err(ControllerApiError::new(400, "invalid session id"));
    }
    Ok(session_id.to_owned())
}

fn artifact_segment(value: Option<&String>) -> Result<String, ControllerApiError> {
    let Some(segment) = value.map(|value| value.trim()).filter(|value| {
        !value.is_empty()
            && !value.contains('/')
            && !value.contains('\\')
            && !value.contains("..")
            && !value.contains('\0')
    }) else {
        return Err(ControllerApiError::new(400, "invalid artifact path"));
    };
    Ok(segment.to_owned())
}

fn required_upload_u64(
    query: &HashMap<String, String>,
    key: &str,
) -> Result<u64, ControllerApiError> {
    query
        .get(key)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| ControllerApiError::new(400, format!("invalid {key}")))
}

fn upload_principal_key(principal: &ControllerPrincipal) -> String {
    // The durable upload receipt must bind to the authenticated principal, but
    // staging metadata does not need a device name or SSH subject in clear
    // text. Hash a canonical identity that deliberately excludes the mutable
    // paired-device display name.
    let identity = match principal {
        ControllerPrincipal::PairedDevice { device_id, .. } => {
            json!({ "kind": "paired_device", "deviceID": device_id })
        }
        ControllerPrincipal::OwnerTransport {
            transport, subject, ..
        } => json!({
            "kind": "owner_transport",
            "transport": transport,
            "subject": subject,
        }),
    };
    format!("{:x}", Sha256::digest(identity.to_string().as_bytes()))
}

fn replay_protected(request: &ControllerRequest) -> bool {
    matches!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/mobile/sessions")
            | ("POST", "/mobile/restart-session")
            | ("POST", "/mobile/session-action")
            | ("POST", "/mobile/write")
            | ("POST", "/mobile/resize")
            | ("POST", "/mobile/request-screenshot")
            | ("POST", "/mobile/mark-read")
            | ("POST", "/mobile/artifact-delete")
    )
}

fn lifecycle_effect_route(request: &ControllerRequest) -> bool {
    matches!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/mobile/restart-session") | ("POST", "/mobile/session-action")
    )
}

fn same_replay_principal(left: &ControllerPrincipal, right: &ControllerPrincipal) -> bool {
    match (left, right) {
        (
            ControllerPrincipal::PairedDevice {
                device_id: left, ..
            },
            ControllerPrincipal::PairedDevice {
                device_id: right, ..
            },
        ) => left == right,
        (
            ControllerPrincipal::OwnerTransport {
                transport: left_transport,
                subject: left_subject,
                ..
            },
            ControllerPrincipal::OwnerTransport {
                transport: right_transport,
                subject: right_subject,
                ..
            },
        ) => left_transport == right_transport && left_subject == right_subject,
        _ => false,
    }
}

fn request_fingerprint(request: &ControllerRequest) -> [u8; 32] {
    let mut principal = request.principal.clone();
    if let ControllerPrincipal::PairedDevice { name, .. } = &mut principal {
        // Device name is presentation metadata and may be renamed between a
        // Link send and retry; the durable device id is the replay identity.
        name.clear();
    }
    // HashMap iteration is intentionally randomized. Stable request ids must
    // survive a fresh transport decode, so canonicalize query ordering before
    // hashing instead of serializing ControllerRequest directly.
    let query: BTreeMap<&str, &str> = request
        .query
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let encoded = serde_json::to_vec(&(
        &request.id,
        &request.method,
        &request.path,
        query,
        &request.body,
        &request.content_type,
        &request.body_base64,
        principal,
    ))
    .unwrap_or_default();
    Sha256::digest(encoded).into()
}

fn write_session(body: &Value) -> Result<Value, ControllerApiError> {
    let session_id = body_session_id(body)?;
    let Some(data) = body.get("data").and_then(Value::as_str) else {
        return Err(ControllerApiError::new(400, "request failed"));
    };
    validate_terminal_effect_target(&session_id)?;
    let write_id = body
        .get("wid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .map(str::to_owned);
    terminal_effect_dispatch_result(session_host::send_command_with_timeout(
        &session_id,
        &SessionHostCommand::Write {
            data: data.to_owned(),
            write_id,
        },
        SESSION_COMMAND_TIMEOUT,
    ))
}

fn resize_session(body: &Value) -> Result<Value, ControllerApiError> {
    let session_id = body_session_id(body)?;
    let Some(columns) = body.get("columns").and_then(Value::as_i64) else {
        return Err(ControllerApiError::new(400, "request failed"));
    };
    let Some(rows) = body.get("rows").and_then(Value::as_i64) else {
        return Err(ControllerApiError::new(400, "request failed"));
    };
    let columns = columns.clamp(2, 300) as u16;
    let rows = rows.clamp(2, 120) as u16;
    validate_terminal_effect_target(&session_id)?;
    terminal_effect_dispatch_result(session_host::send_command_with_timeout(
        &session_id,
        &SessionHostCommand::Resize {
            cols: columns,
            rows,
        },
        SESSION_COMMAND_TIMEOUT,
    ))
}

/// A 404/409 is emitted only before dispatch, from durable Host state, so a
/// Controller may treat it as proven not-applied. Once the control command is
/// attempted, any socket/write/read/reply failure is ambiguous: the PTY may
/// already have consumed the bytes before its acknowledgement was lost.
fn validate_terminal_effect_target(session_id: &str) -> Result<(), ControllerApiError> {
    let Some(manifest) = session_host::load_manifest(session_id) else {
        return Err(ControllerApiError::new(404, "unknown session"));
    };
    if manifest.state != HostedSessionState::Running {
        return Err(ControllerApiError::new(409, "session has exited"));
    }
    Ok(())
}

fn terminal_effect_dispatch_result(
    result: Result<(), String>,
) -> Result<Value, ControllerApiError> {
    result.map_err(|_| ControllerApiError::new(500, "session host acknowledgement unavailable"))?;
    Ok(json!({ "ok": true }))
}

fn run_session_action(
    body: &Value,
    forced_action: Option<ControllerSessionAction>,
    effects: &ControllerEffects,
) -> Result<Value, ControllerApiError> {
    // Match the shipped Codable enum boundary: an unknown action is a DTO
    // failure (400) and never reaches resource resolution or an effect.
    let action = match forced_action {
        Some(action) => action,
        None => match body.get("action").and_then(Value::as_str) {
            Some("stop") => ControllerSessionAction::Stop,
            Some("restart") => ControllerSessionAction::Restart,
            Some("restart_agent") => ControllerSessionAction::RestartAgent,
            Some("resume_agent") => ControllerSessionAction::ResumeAgent,
            Some("remove") => ControllerSessionAction::Remove,
            _ => return Err(ControllerApiError::new(400, "request failed")),
        },
    };
    let session_id = body_session_id(body)?;
    let request = ControllerSessionActionRequest {
        session_id: session_id.clone(),
        action,
    };
    effects
        .execute_session_action(request)
        .map_err(|error| match error {
            ControllerEffectError::UnknownSession => {
                ControllerApiError::new(404, format!("Unknown session id: {session_id}"))
            }
            ControllerEffectError::SessionNotRunning => {
                ControllerApiError::new(409, format!("Session is not running: {session_id}"))
            }
            ControllerEffectError::SessionStillRunning => {
                ControllerApiError::new(409, format!("Session is still running: {session_id}"))
            }
            ControllerEffectError::AgentNotRestartable => {
                let verb = if action == ControllerSessionAction::ResumeAgent {
                    "resume"
                } else {
                    "restart"
                };
                ControllerApiError::new(409, format!("No managed agent to {verb}: {session_id}"))
            }
            ControllerEffectError::Failed(message) => {
                log::warn!(
                    "controller {} effect failed for {session_id}: {message}",
                    match action {
                        ControllerSessionAction::Stop => "stop",
                        ControllerSessionAction::Restart => "restart",
                        ControllerSessionAction::RestartAgent => "restart agent",
                        ControllerSessionAction::ResumeAgent => "resume agent",
                        ControllerSessionAction::Remove => "remove",
                    }
                );
                let verb = match action {
                    ControllerSessionAction::Stop => "stop",
                    ControllerSessionAction::Restart => "restart",
                    ControllerSessionAction::RestartAgent => "restart agent",
                    ControllerSessionAction::ResumeAgent => "resume agent",
                    ControllerSessionAction::Remove => "remove",
                };
                ControllerApiError::new(500, format!("Could not {verb} session: {session_id}"))
            }
        })?;
    Ok(json!({ "ok": true }))
}

pub fn read_session_metrics(session_id: &str) -> Result<SessionMetrics, ControllerApiError> {
    if !valid_session_id(session_id) {
        return Err(ControllerApiError::new(400, "invalid session id"));
    }
    let Some(manifest) = session_host::load_manifest(session_id) else {
        return Err(ControllerApiError::new(404, "unknown session"));
    };
    if manifest.state != HostedSessionState::Running {
        return Err(ControllerApiError::new(409, "session has exited"));
    }
    let snapshot = session_host::request_current_viewport_snapshot(session_id, 0, Some(1))
        .map_err(|message| ControllerApiError::new(502, message))?;
    if snapshot.cols <= 2 && snapshot.rows <= 2 {
        return Err(ControllerApiError::new(
            502,
            "session host predates viewport metrics; restart the session",
        ));
    }
    Ok(SessionMetrics {
        session_id: session_id.to_owned(),
        columns: snapshot.cols,
        rows: snapshot.rows,
        output_offset: snapshot.output_offset,
        captured_at_unix_ms: current_timestamp_ms(),
    })
}

fn bootstrap_body(context: &HostBootstrapContext) -> Value {
    let mut envelope = json!({
        "protocolVersion": 1,
        "hostProtocol": context.protocol,
        "capturedAtUnixMs": current_timestamp_ms(),
        "serverVersion": env!("CARGO_PKG_VERSION"),
    });
    if let (Some(object), Some(snapshot)) = (envelope.as_object_mut(), context.snapshot.as_object())
    {
        for (key, value) in snapshot {
            object.insert(key.clone(), value.clone());
        }
        // Router-owned metadata wins over anything cached in the UI snapshot.
        object.insert("protocolVersion".into(), 1.into());
        object.insert("capturedAtUnixMs".into(), current_timestamp_ms().into());
        // Additive: the Host's crate version. A Controller without the
        // capability list falls back to `serverVersion >= 0.5.3` to decide
        // that the direct `/mobile` endpoint is TLS.
        object.insert("serverVersion".into(), env!("CARGO_PKG_VERSION").into());
        object.insert(
            "hostProtocol".into(),
            serde_json::to_value(&context.protocol).unwrap_or(Value::Null),
        );
        if let Some(host_id) = &context.host_id {
            // Shipped protocol-v1 compatibility name. Internally this is a
            // Host id and must not make Controllers branch on Host kind.
            object.insert("macID".into(), host_id.clone().into());
        }
        if let Some(port) = context.remote_server_port {
            object.insert("remoteServerPort".into(), port.into());
        }
        if let Some(fingerprint) = &context.remote_server_certificate_fingerprint {
            object.insert(
                "remoteServerCertificateFingerprint".into(),
                fingerprint.clone().into(),
            );
        }
        if !context.pending_approvals.is_empty() {
            object.insert(
                "pendingApprovals".into(),
                context.pending_approvals.clone().into(),
            );
        }
        // A headless Linux host advertises its hardware kind so Controllers show
        // the right icon (native Mac hosts inject these Swift-side instead).
        // Presentation only; a Controller treats a missing value as unknown.
        #[cfg(target_os = "linux")]
        if !object.contains_key("hostDeviceKind") {
            object.insert("hostDeviceKind".into(), "linux".into());
            if let Some(model) = linux_uname_model() {
                object.insert("hostDeviceModel".into(), model.into());
            }
        }
        // Additive, data-only isolation + environment advertisement
        // (unpeel-apple:docs/plans/computer-use-release.md, Lane D / decisions D2, D5).
        // A Controller treats a missing value as unknown; 0.5.0 makes no
        // policy decision on either. Router-owned so both Host kinds publish
        // the same shape.
        if !object.contains_key("hostIsolationTier") {
            object.insert("hostIsolationTier".into(), host_isolation_tier().into());
        }
        if !object.contains_key("hostEnvironment") {
            if let Some(environment) = host_environment() {
                object.insert("hostEnvironment".into(), environment);
            }
        }
    }
    envelope
}

/// The Host's isolation tier as plain bootstrap data: `vm`, `container`, or
/// `host`. macOS always reports `host`. On Linux it asks
/// `systemd-detect-virt` (containers first, then VMs), so an LXC/Docker guest
/// reads `container` and a KVM/QEMU guest `vm`; bare metal, or a box without
/// systemd, reads `host`. Never a policy input in 0.5.0 — the sandboxed-
/// project grant policy in `shared-workspaces.md` will consume it later.
pub fn host_isolation_tier() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if detect_virt_matches("--container") {
            return "container";
        }
        if detect_virt_matches("--vm") {
            return "vm";
        }
        "host"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "host"
    }
}

/// `systemd-detect-virt <flag>` exits 0 and prints a technology name when it
/// detects that class, or exits non-zero with `none`. Absent binary → false.
#[cfg(target_os = "linux")]
fn detect_virt_matches(flag: &str) -> bool {
    std::process::Command::new("systemd-detect-virt")
        .arg(flag)
        .output()
        .ok()
        .and_then(|output| {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (output.status.success() && !name.is_empty() && name != "none").then_some(())
        })
        .is_some()
}

/// The hosting environment as plain data — currently only `{kind:"box", id}`
/// when this Host detects it runs inside a Box (decision D5). The Controller,
/// holding the user's own Box credentials, is what mints a desktop URL from
/// it; no Box secret ever lives on the Host.
///
/// **Unverified on a real box (2026-09-03):** how a Box identifies itself
/// from inside is not confirmed, so this checks a documented probe list —
/// the first that yields a non-empty id wins:
///   1. env `UNPEEL_HOST_ENVIRONMENT_BOX_ID` (explicit override / test seam)
///   2. env `BOXD_MACHINE_ID`, then `BOX_ID`, then `BOXD_ID`
///   3. the first line of `/etc/boxd/machine-id` or `~/.boxd/machine-id`
///
/// Update this list once a box is available; keep it data-only.
pub fn host_environment() -> Option<Value> {
    let id = box_identity()?;
    Some(json!({ "kind": "box", "id": id }))
}

fn box_identity() -> Option<String> {
    for key in [
        "UNPEEL_HOST_ENVIRONMENT_BOX_ID",
        "BOXD_MACHINE_ID",
        "BOX_ID",
        "BOXD_ID",
    ] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    let mut candidates = vec![std::path::PathBuf::from("/etc/boxd/machine-id")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(std::path::Path::new(&home).join(".boxd/machine-id"));
    }
    for path in candidates {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Some(first) = contents.lines().next() {
                let trimmed = first.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Best-effort `uname -s -m` (e.g. "Linux aarch64") as the Linux host model
/// hint. Returns None if `uname(2)` fails.
#[cfg(target_os = "linux")]
fn linux_uname_model() -> Option<String> {
    // SAFETY: `uname` fills a zeroed `utsname`; we only read the C strings back.
    unsafe {
        let mut info: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut info) != 0 {
            return None;
        }
        let sysname = std::ffi::CStr::from_ptr(info.sysname.as_ptr())
            .to_string_lossy()
            .into_owned();
        let machine = std::ffi::CStr::from_ptr(info.machine.as_ptr())
            .to_string_lossy()
            .into_owned();
        let model = format!("{sysname} {machine}").trim().to_string();
        if model.is_empty() {
            None
        } else {
            Some(model)
        }
    }
}

/// Route a semantic Host request. `None` means the shared router does not own
/// the route yet and the compatibility adapter must handle it. Returning a
/// concrete 4xx/5xx response means the route is owned and failed normally.
pub fn route(
    request: &ControllerRequest,
    bootstrap: Option<&HostBootstrapContext>,
) -> Option<ControllerResponse> {
    route_with_parts(request, bootstrap, None, None, None)
}

/// Route with adapter-resolved data for every shared Host operation. Existing
/// bootstrap-only adapters can keep calling [`route`] while they migrate.
pub fn route_with_context(
    request: &ControllerRequest,
    context: Option<&HostRouteContext>,
) -> Option<ControllerResponse> {
    route_with_parts(
        request,
        context.and_then(|value| value.bootstrap.as_ref()),
        context.map(|value| &value.archived_sessions_by_project),
        None,
        None,
    )
}

/// Route with the optional headless create adapter. Native callers continue
/// using [`route_with_context`], which intentionally leaves
/// `POST /mobile/sessions` unhandled for the Swift compatibility path.
pub fn route_with_create_context(
    request: &ControllerRequest,
    context: Option<&HostRouteContext>,
    create_context: Option<&HostCreateContext>,
) -> Option<ControllerResponse> {
    route_with_parts(
        request,
        context.and_then(|value| value.bootstrap.as_ref()),
        context.map(|value| &value.archived_sessions_by_project),
        create_context,
        None,
    )
}

/// Route with every currently migrated adapter boundary. The native bridge
/// intentionally keeps calling [`route_with_context`] so lifecycle requests
/// fall through to Swift; the headless Host supplies both create and
/// lifecycle effects here.
pub fn route_with_effects(
    request: &ControllerRequest,
    context: Option<&HostRouteContext>,
    create_context: Option<&HostCreateContext>,
    effects: Option<&ControllerEffects>,
) -> Option<ControllerResponse> {
    route_with_parts(
        request,
        context.and_then(|value| value.bootstrap.as_ref()),
        context.map(|value| &value.archived_sessions_by_project),
        create_context,
        effects,
    )
}

fn route_with_parts(
    request: &ControllerRequest,
    bootstrap: Option<&HostBootstrapContext>,
    archived_sessions_by_project: Option<&HashMap<String, Vec<Value>>>,
    create_context: Option<&HostCreateContext>,
    effects: Option<&ControllerEffects>,
) -> Option<ControllerResponse> {
    // A compatibility adapter owns lifecycle when no effects are supplied.
    // Bypass even replay-id validation/cache reservation in that case so the
    // router cannot turn a Swift-owned request into a 400/409 or serialize two
    // fall-throughs around a cached `None`.
    if !replay_protected(request) || (lifecycle_effect_route(request) && effects.is_none()) {
        return route_uncached(
            request,
            bootstrap,
            archived_sessions_by_project,
            create_context,
            effects,
        );
    }
    let Some(request_id) = request.id.as_deref() else {
        return route_uncached(
            request,
            bootstrap,
            archived_sessions_by_project,
            create_context,
            effects,
        );
    };
    if request_id.is_empty() || request_id.len() > MAX_REQUEST_ID_BYTES {
        return Some(ControllerResponse {
            id: request.id.clone(),
            status: 400,
            body: json!({ "error": "invalid request id" }),
        });
    }

    // Reserve the request id under the short global lock, then execute outside
    // it. Matching retries wait on only this entry; unrelated mutations remain
    // independent even when a create is waiting for its Host to become ready.
    let cache = REPLAY_CACHE.get_or_init(|| Mutex::new(VecDeque::new()));
    let now = Instant::now();
    let fingerprint = request_fingerprint(request);
    let (entry, is_leader) = {
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|entry| !entry.is_expired(now));
        if let Some(previous) = cache.iter().find(|entry| {
            same_replay_principal(&entry.principal, &request.principal)
                && entry.request_id == request_id
        }) {
            if previous.fingerprint != fingerprint {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 409,
                    body: json!({ "error": "request id reused with different request" }),
                });
            }
            (Arc::clone(previous), false)
        } else {
            // Completed entries are oldest-first. In-flight entries are never
            // evicted; a burst may exceed the cap temporarily until leaders
            // complete, at which point the cache is trimmed again below.
            while cache.len() >= REPLAY_CACHE_ENTRIES {
                let Some(index) = cache.iter().position(|entry| !entry.is_in_flight()) else {
                    break;
                };
                cache.remove(index);
            }
            let entry = Arc::new(ReplayEntry::new(
                request.principal.clone(),
                request_id.to_owned(),
                fingerprint,
                now,
            ));
            cache.push_back(Arc::clone(&entry));
            (entry, true)
        }
    };

    if !is_leader {
        return entry.wait_for_response();
    }

    let mut completion = ReplayLeaderGuard::new(Arc::clone(&entry), request.id.clone());
    let response = route_uncached(
        request,
        bootstrap,
        archived_sessions_by_project,
        create_context,
        effects,
    );
    completion.complete(response.clone());

    // An unhandled route belongs to a compatibility adapter, so wake any
    // followers with `None` but do not keep that outcome as a durable replay.
    // A completed slow entry may also have crossed the TTL while in flight.
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if response.is_none() {
        cache.retain(|candidate| !Arc::ptr_eq(candidate, &entry));
    }
    let now = Instant::now();
    cache.retain(|candidate| !candidate.is_expired(now));
    while cache.len() > REPLAY_CACHE_ENTRIES {
        let Some(index) = cache.iter().position(|candidate| !candidate.is_in_flight()) else {
            break;
        };
        cache.remove(index);
    }
    response
}

fn route_uncached(
    request: &ControllerRequest,
    bootstrap: Option<&HostBootstrapContext>,
    archived_sessions_by_project: Option<&HashMap<String, Vec<Value>>>,
    create_context: Option<&HostCreateContext>,
    effects: Option<&ControllerEffects>,
) -> Option<ControllerResponse> {
    let body = match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/mobile/bootstrap") => match bootstrap {
            Some(context) => bootstrap_body(context),
            None => json!({ "error": "bootstrap context unavailable" }),
        },
        ("GET", "/mobile/metrics") => {
            let session_id = match query_session_id(&request.query) {
                Ok(session_id) => session_id,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            match read_session_metrics(&session_id) {
                Ok(metrics) => json!({
                    "sessionID": metrics.session_id,
                    "columns": metrics.columns.clamp(2, 300),
                    "rows": metrics.rows.clamp(2, 120),
                    "outputOffset": metrics.output_offset,
                    "capturedAtUnixMs": metrics.captured_at_unix_ms,
                    "desktopViewing": false,
                }),
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            }
        }
        ("GET", "/mobile/transcript-markdown") => {
            let Some(session_id) = request
                .query
                .get("session_id")
                .or_else(|| request.query.get("sessionID"))
                .map(|value| value.trim())
                .filter(|value| valid_session_id(value))
            else {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 400,
                    body: json!({ "error": "invalid session id" }),
                });
            };
            // Match the shipped native bridge: invalid numbers use the app
            // setting, negative values mean the whole conversation, and the
            // returned Markdown is trimmed before crossing the wire.
            let entries = request
                .query
                .get("entries")
                .and_then(|value| value.parse::<i64>().ok())
                .map(|value| value.max(0) as usize);
            match transcripts::read_session_transcript_markdown(session_id, entries, false) {
                Ok(markdown) => json!({
                    "sessionID": session_id,
                    "markdown": markdown.trim(),
                }),
                Err(message) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        // The native compatibility adapter has always
                        // surfaced renderer/provider failures as 502.
                        status: 502,
                        body: json!({ "error": message }),
                    });
                }
            }
        }
        ("GET", "/mobile/archive") => {
            let Some(project_id) = request
                .query
                .get("project_id")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            else {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 400,
                    body: json!({ "error": "project_id required" }),
                });
            };
            let Some(projects) = archived_sessions_by_project else {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 500,
                    body: json!({ "error": "archive context unavailable" }),
                });
            };
            let Some(sessions) = projects.get(project_id) else {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 404,
                    body: json!({ "error": "unknown project" }),
                });
            };
            json!({
                "projectID": project_id,
                "sessions": sessions,
            })
        }
        ("POST", "/mobile/sessions") => {
            let context = create_context?;
            match create_session(&request.body, &request.principal, context) {
                Ok(body) => body,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            }
        }
        ("POST", "/mobile/restart-session") => {
            let effects = effects?;
            match run_session_action(
                &request.body,
                Some(ControllerSessionAction::Restart),
                effects,
            ) {
                Ok(body) => body,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            }
        }
        ("POST", "/mobile/session-action") => {
            let effects = effects?;
            match run_session_action(&request.body, None, effects) {
                Ok(body) => body,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            }
        }
        ("POST", "/mobile/write") => match write_session(&request.body) {
            Ok(body) => body,
            Err(error) => {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: error.status,
                    body: json!({ "error": error.message }),
                });
            }
        },
        ("POST", "/mobile/resize") => match resize_session(&request.body) {
            Ok(body) => body,
            Err(error) => {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: error.status,
                    body: json!({ "error": error.message }),
                });
            }
        },
        ("POST", "/mobile/request-screenshot") => {
            let session_id = match body_session_id(&request.body) {
                Ok(session_id) => session_id,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            if let Err(message) =
                session_input::request_screenshot_with_timeout(&session_id, SESSION_COMMAND_TIMEOUT)
            {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 404,
                    body: json!({ "error": message }),
                });
            }
            json!({
                "accepted": true,
                "requestedAtUnixMs": current_timestamp_ms(),
            })
        }
        ("POST", "/mobile/mark-read") => {
            let session_id = match body_session_id(&request.body) {
                Ok(session_id) => session_id,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            // Shipped v1 treats this receipt as idempotent/best-effort: a
            // Session removed between bootstrap and tap still returns 200.
            // Existing dirs get the shared marker plus state-bus announce.
            let _ = session_ops::mark_read(&session_id);
            json!({ "ok": true })
        }
        ("POST", "/mobile/session-order") => {
            // Replace one project's hand-ordered sidebar ranks. The list is
            // the combined pinned + regular order exactly as a desktop drag
            // commits it; sessions absent from it keep newest-first on top.
            let Some(project_id) = request
                .body
                .get("projectID")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| valid_session_id(value))
            else {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 400,
                    body: json!({ "error": "invalid project id" }),
                });
            };
            let Some(entries) = request
                .body
                .get("orderedSessionIDs")
                .and_then(Value::as_array)
            else {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 400,
                    body: json!({ "error": "orderedSessionIDs must be an array" }),
                });
            };
            if entries.len() > 1024 {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 400,
                    body: json!({ "error": "too many session ids" }),
                });
            }
            let mut ordered: Vec<String> = Vec::with_capacity(entries.len());
            for entry in entries {
                let Some(id) = entry
                    .as_str()
                    .map(str::trim)
                    .filter(|value| valid_session_id(value))
                else {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: 400,
                        body: json!({ "error": "invalid session id" }),
                    });
                };
                if !ordered.iter().any(|existing| existing == id) {
                    ordered.push(id.to_owned());
                }
            }
            if let Err(message) = session_ops::set_session_order(project_id, &ordered) {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 500,
                    body: json!({ "error": message }),
                });
            }
            json!({ "ok": true })
        }
        ("GET", "/mobile/artifacts") => {
            let session_id = match query_session_id(&request.query) {
                Ok(session_id) => session_id,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            json!({
                "sessionID": session_id,
                "artifacts": session_artifacts::list(&session_id),
                "capturedAtUnixMs": current_timestamp_ms(),
            })
        }
        ("GET", "/mobile/artifact") => {
            let session_id = match query_session_id(&request.query) {
                Ok(session_id) => session_id,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            let kind = match artifact_segment(request.query.get("kind")) {
                Ok(kind) => kind,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            let name = match artifact_segment(request.query.get("name")) {
                Ok(name) => name,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            let offset = request
                .query
                .get("offset")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let limit = request
                .query
                .get("limit")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(session_artifacts::ARTIFACT_READ_MAX_CHUNK_BYTES);
            match session_artifacts::read_chunk(&session_id, &kind, &name, offset, limit) {
                Ok(chunk) => json!({
                    "sessionID": session_id,
                    "kind": chunk.kind,
                    "name": chunk.name,
                    "contentType": chunk.content_type,
                    "offset": chunk.offset,
                    "nextOffset": chunk.next_offset,
                    "totalSize": chunk.total_size,
                    "dataBase64": base64::engine::general_purpose::STANDARD.encode(chunk.bytes),
                    "capturedAtUnixMs": current_timestamp_ms(),
                }),
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            }
        }
        ("POST", "/mobile/upload-chunk") => {
            let session_id = match query_session_id(&request.query) {
                Ok(session_id) => session_id,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            let upload_id = request
                .query
                .get("upload_id")
                .map(|value| value.trim())
                .unwrap_or_default();
            let offset = match required_upload_u64(&request.query, "offset") {
                Ok(value) => value,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            let total_size = match required_upload_u64(&request.query, "total_size") {
                Ok(value) => value,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            let sha256 = request
                .query
                .get("sha256")
                .map(|value| value.trim())
                .unwrap_or_default();
            let content_type = request
                .content_type
                .as_deref()
                .unwrap_or_default()
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let bytes = match request.body_base64.as_deref() {
                Some(encoded) => match base64::engine::general_purpose::STANDARD.decode(encoded) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return Some(ControllerResponse {
                            id: request.id.clone(),
                            status: 400,
                            body: json!({
                                "error": "invalid upload body",
                                "code": "invalid_body",
                                "uploadID": upload_id,
                            }),
                        });
                    }
                },
                None => Vec::new(),
            };
            let principal = upload_principal_key(&request.principal);
            match session_artifacts::upload_resumable_artifact_chunk(
                session_artifacts::ResumableArtifactUploadRequest {
                    session_id: &session_id,
                    upload_id,
                    offset,
                    total_size,
                    sha256,
                    content_type: &content_type,
                    principal: &principal,
                    bytes: &bytes,
                },
            ) {
                Ok(progress) => {
                    let mut body = json!({
                        "sessionID": session_id,
                        "uploadID": progress.upload_id,
                        "offset": offset,
                        "nextOffset": progress.next_offset,
                        "totalSize": total_size,
                        "complete": progress.complete,
                    });
                    if progress.complete {
                        let object = body.as_object_mut().expect("upload response object");
                        if let Some(path) = progress.path {
                            object
                                .insert("path".into(), path.to_string_lossy().into_owned().into());
                        }
                        object.insert("kind".into(), "uploads".into());
                        if let Some(name) = progress.name {
                            object.insert("name".into(), name.into());
                        }
                        if let Some(content_type) = progress.content_type {
                            object.insert("contentType".into(), content_type.into());
                        }
                        if let Some(sha256) = progress.sha256 {
                            object.insert("sha256".into(), sha256.into());
                        }
                    }
                    body
                }
                Err(error) => {
                    let mut body = json!({
                        "error": error.message,
                        "code": error.code,
                        "uploadID": upload_id,
                    });
                    if let Some(next_offset) = error.next_offset {
                        body.as_object_mut()
                            .expect("upload error response object")
                            .insert("nextOffset".into(), next_offset.into());
                    }
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body,
                    });
                }
            }
        }
        ("POST", "/mobile/artifact-delete") => {
            let session_id = match query_session_id(&request.query) {
                Ok(session_id) => session_id,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            let kind = match artifact_segment(request.query.get("kind")) {
                Ok(kind) => kind,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            if session_artifacts::kind_dir(&session_id, &kind).is_none() {
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 404,
                    body: json!({ "error": "unknown artifact kind" }),
                });
            }
            let name = match artifact_segment(request.query.get("name")) {
                Ok(name) => name,
                Err(error) => {
                    return Some(ControllerResponse {
                        id: request.id.clone(),
                        status: error.status,
                        body: json!({ "error": error.message }),
                    });
                }
            };
            if let Err(message) = session_artifacts::delete(&session_id, &kind, &name) {
                log::warn!(
                    "controller artifact delete failed for {session_id}/{kind}/{name}: {message}"
                );
                return Some(ControllerResponse {
                    id: request.id.clone(),
                    status: 500,
                    body: json!({ "error": "artifact delete failed" }),
                });
            }
            // Preserve the shipped native `[String:String]` response shape.
            json!({ "ok": "true" })
        }
        _ => return None,
    };
    Some(ControllerResponse {
        id: request.id.clone(),
        status: if request.path == "/mobile/bootstrap" && bootstrap.is_none() {
            500
        } else {
            200
        },
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Barrier};
    use std::thread;

    fn request(method: &str, path: &str) -> ControllerRequest {
        ControllerRequest {
            id: Some(format!("test-{}", uuid::Uuid::new_v4())),
            method: method.into(),
            path: path.into(),
            query: HashMap::new(),
            body: Value::Null,
            content_type: None,
            body_base64: None,
            principal: ControllerPrincipal::OwnerTransport {
                transport: "test".into(),
                subject: None,
                principal_id: None,
            },
        }
    }

    fn create_catalog(executor: HostCreateExecutor) -> HostCreateContext {
        HostCreateContext::new(
            "host-owner:test".into(),
            vec![
                HostCreateProject {
                    id: "project-1".into(),
                    path: "/host/project".into(),
                    is_folder: false,
                    worktree_path: None,
                    worktree_branch: None,
                },
                HostCreateProject {
                    id: "worktree-1".into(),
                    path: "/host/project".into(),
                    is_folder: false,
                    worktree_path: Some("/host/worktrees/feature".into()),
                    worktree_branch: Some("feature/remote".into()),
                },
                HostCreateProject {
                    id: "folder-1".into(),
                    path: String::new(),
                    is_folder: true,
                    worktree_path: None,
                    worktree_branch: None,
                },
                HostCreateProject {
                    id: "group-1".into(),
                    path: "/host/project".into(),
                    is_folder: true,
                    worktree_path: None,
                    worktree_branch: None,
                },
            ],
            vec![
                HostCreatePreset {
                    id: "preset-1".into(),
                    command: "global-command".into(),
                    enabled: true,
                    project_id: None,
                },
                HostCreatePreset {
                    id: "preset-1".into(),
                    command: "project-command --safe".into(),
                    enabled: true,
                    project_id: Some("project-1".into()),
                },
                HostCreatePreset {
                    id: "disabled".into(),
                    command: "disabled-command".into(),
                    enabled: false,
                    project_id: None,
                },
            ],
            executor,
        )
    }

    fn valid_create_request() -> ControllerRequest {
        let mut request = request("POST", "/mobile/sessions");
        request.body = json!({
            "projectID": "project-1",
            "command": "codex",
        });
        request
    }

    fn controller_effects(
        executor: impl Fn(ControllerSessionActionRequest) -> Result<(), ControllerEffectError>
            + Send
            + Sync
            + 'static,
    ) -> ControllerEffects {
        ControllerEffects::new(Arc::new(executor))
    }

    #[test]
    fn session_order_validates_before_touching_shared_state() {
        // Validation-only coverage: the success path writes the real
        // session-order.json, so it lives in the isolated PTY suite
        // (tests/cases/mobile.py) instead of a unit test.
        let mut missing_project = request("POST", "/mobile/session-order");
        missing_project.body = json!({ "orderedSessionIDs": ["session-1"] });
        let response = route(&missing_project, None).unwrap();
        assert_eq!(response.status, 400);

        let mut malformed_ids = request("POST", "/mobile/session-order");
        malformed_ids.body = json!({ "projectID": "project-1", "orderedSessionIDs": "session-1" });
        let response = route(&malformed_ids, None).unwrap();
        assert_eq!(response.status, 400);

        let mut traversal_id = request("POST", "/mobile/session-order");
        traversal_id.body = json!({
            "projectID": "project-1",
            "orderedSessionIDs": ["../escape"],
        });
        let response = route(&traversal_id, None).unwrap();
        assert_eq!(response.status, 400);
    }

    #[test]
    fn create_without_adapter_context_remains_unhandled() {
        let request = valid_create_request();
        assert!(route(&request, None).is_none());
        assert!(route_with_context(&request, None).is_none());
    }

    #[test]
    fn lifecycle_routes_require_effects_and_validate_before_execution() {
        let mut restart = request("POST", "/mobile/restart-session");
        restart.body = json!({ "sessionID": "known" });
        assert!(route_with_context(&restart, None).is_none());
        assert!(route_with_create_context(&restart, None, None).is_none());
        restart.id = Some("x".repeat(MAX_REQUEST_ID_BYTES + 1));
        assert!(
            route_with_context(&restart, None).is_none(),
            "native fallback bypasses lifecycle replay validation without effects"
        );

        let executions = Arc::new(AtomicUsize::new(0));
        let execution_count = Arc::clone(&executions);
        let effects = controller_effects(move |_| {
            execution_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        let mut missing = request("POST", "/mobile/restart-session");
        missing.body = json!({});
        let response = route_with_effects(&missing, None, None, Some(&effects)).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], "invalid session id");

        let mut unknown_action = request("POST", "/mobile/session-action");
        unknown_action.body = json!({
            "sessionID": "known",
            "action": "archive",
        });
        let response = route_with_effects(&unknown_action, None, None, Some(&effects)).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], "request failed");
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn lifecycle_effects_preserve_shipped_actions_and_statuses() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&captured);
        let effects = controller_effects(move |request| {
            captured_requests
                .lock()
                .expect("capture lock")
                .push(request.clone());
            match request.session_id.as_str() {
                "missing" => Err(ControllerEffectError::UnknownSession),
                "exited" => Err(ControllerEffectError::SessionNotRunning),
                "broken" => Err(ControllerEffectError::Failed("adapter detail".into())),
                _ => Ok(()),
            }
        });

        let mut restart = request("POST", "/mobile/restart-session");
        restart.body = json!({ "sessionID": "known" });
        let response = route_with_effects(&restart, None, None, Some(&effects)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, json!({ "ok": true }));

        let mut remove = request("POST", "/mobile/session-action");
        remove.body = json!({ "sessionID": "known", "action": "remove" });
        assert_eq!(
            route_with_effects(&remove, None, None, Some(&effects))
                .unwrap()
                .status,
            200
        );

        let mut restart_agent = request("POST", "/mobile/session-action");
        restart_agent.body = json!({ "sessionID": "known", "action": "restart_agent" });
        assert_eq!(
            route_with_effects(&restart_agent, None, None, Some(&effects))
                .unwrap()
                .status,
            200
        );

        let mut resume_agent = request("POST", "/mobile/session-action");
        resume_agent.body = json!({ "sessionID": "known", "action": "resume_agent" });
        assert_eq!(
            route_with_effects(&resume_agent, None, None, Some(&effects))
                .unwrap()
                .status,
            200
        );

        let mut missing = request("POST", "/mobile/session-action");
        missing.body = json!({ "sessionID": "missing", "action": "restart" });
        assert_eq!(
            route_with_effects(&missing, None, None, Some(&effects))
                .unwrap()
                .status,
            404
        );

        let mut exited = request("POST", "/mobile/session-action");
        exited.body = json!({ "sessionID": "exited", "action": "restart_agent" });
        assert_eq!(
            route_with_effects(&exited, None, None, Some(&effects))
                .unwrap()
                .status,
            409
        );

        let mut failed = request("POST", "/mobile/restart-session");
        failed.body = json!({ "sessionID": "broken" });
        let response = route_with_effects(&failed, None, None, Some(&effects)).unwrap();
        assert_eq!(response.status, 500);
        assert_eq!(response.body["error"], "Could not restart session: broken");

        assert_eq!(
            captured.lock().expect("capture lock").as_slice(),
            &[
                ControllerSessionActionRequest {
                    session_id: "known".into(),
                    action: ControllerSessionAction::Restart,
                },
                ControllerSessionActionRequest {
                    session_id: "known".into(),
                    action: ControllerSessionAction::Remove,
                },
                ControllerSessionActionRequest {
                    session_id: "known".into(),
                    action: ControllerSessionAction::RestartAgent,
                },
                ControllerSessionActionRequest {
                    session_id: "known".into(),
                    action: ControllerSessionAction::ResumeAgent,
                },
                ControllerSessionActionRequest {
                    session_id: "missing".into(),
                    action: ControllerSessionAction::Restart,
                },
                ControllerSessionActionRequest {
                    session_id: "exited".into(),
                    action: ControllerSessionAction::RestartAgent,
                },
                ControllerSessionActionRequest {
                    session_id: "broken".into(),
                    action: ControllerSessionAction::Restart,
                },
            ]
        );
    }

    #[test]
    fn restart_agent_race_rejections_remain_conflicts_without_hiding_io_failures() {
        assert_eq!(
            classify_restart_agent_failure(
                "Refusing to restart claude: terminal foreground is codex".into()
            ),
            ControllerEffectError::AgentNotRestartable
        );
        assert_eq!(
            classify_restart_agent_failure(
                "Agent restart generation changed (expected 3, current 4)".into()
            ),
            ControllerEffectError::AgentNotRestartable
        );
        assert_eq!(
            classify_restart_agent_failure(
                "Refusing to resume claude: the agent is still running".into()
            ),
            ControllerEffectError::AgentNotRestartable
        );
        assert_eq!(
            classify_restart_agent_failure("session live-session is not running".into()),
            ControllerEffectError::SessionNotRunning
        );
        assert_eq!(
            classify_restart_agent_failure(
                "Failed to write restarted agent command to the session PTY: broken pipe".into()
            ),
            ControllerEffectError::Failed(
                "Failed to write restarted agent command to the session PTY: broken pipe".into()
            )
        );
    }

    #[test]
    fn lifecycle_replay_is_single_flight_and_rejects_id_reuse() {
        let executions = Arc::new(AtomicUsize::new(0));
        let execution_count = Arc::clone(&executions);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let executor_gate = Arc::clone(&gate);
        let (entered_tx, entered_rx) = mpsc::channel();
        let effects = controller_effects(move |_| {
            execution_count.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).expect("lifecycle entry receiver");
            let (lock, ready) = &*executor_gate;
            let mut released = lock.lock().expect("lifecycle gate");
            while !*released {
                released = ready.wait(released).expect("lifecycle gate wait");
            }
            Ok(())
        });
        let mut request = request("POST", "/mobile/session-action");
        request.id = Some(format!("lifecycle-replay-{}", uuid::Uuid::new_v4()));
        request.body = json!({ "sessionID": "known", "action": "restart" });

        let leader_request = request.clone();
        let leader_effects = effects.clone();
        let leader = thread::spawn(move || {
            route_with_effects(&leader_request, None, None, Some(&leader_effects))
                .expect("leader response")
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("leader should enter lifecycle effect");

        let follower_request = request.clone();
        let follower_effects = effects.clone();
        let (response_tx, response_rx) = mpsc::channel();
        let follower = thread::spawn(move || {
            response_tx
                .send(route_with_effects(
                    &follower_request,
                    None,
                    None,
                    Some(&follower_effects),
                ))
                .expect("follower response receiver");
        });
        thread::yield_now();

        let (lock, ready) = &*gate;
        *lock.lock().expect("lifecycle gate release") = true;
        ready.notify_all();
        let leader_response = leader.join().expect("leader thread");
        let follower_response = response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("follower should receive replay")
            .expect("follower response");
        follower.join().expect("follower thread");

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(follower_response, leader_response);
        assert_eq!(leader_response.body, json!({ "ok": true }));
        assert_eq!(
            route_with_effects(&request, None, None, Some(&effects)).unwrap(),
            leader_response
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        let mut mismatch = request;
        mismatch.body["action"] = "remove".into();
        let response = route_with_effects(&mismatch, None, None, Some(&effects)).unwrap();
        assert_eq!(response.status, 409);
        assert_eq!(
            response.body["error"],
            "request id reused with different request"
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn create_resolves_host_catalog_preset_and_optional_summary() {
        let captured = Arc::new(Mutex::new(Vec::<ResolvedHostCreate>::new()));
        let executor_captured = Arc::clone(&captured);
        let context = create_catalog(Arc::new(move |request| {
            executor_captured
                .lock()
                .expect("capture lock")
                .push(request);
            Ok(HostCreateOutcome {
                session_id: "created-session".into(),
                session: Some(json!({
                    "id": "created-session",
                    "projectID": "project-1",
                    "status": "starting",
                })),
            })
        }));
        let mut request = valid_create_request();
        request.principal = ControllerPrincipal::PairedDevice {
            device_id: "phone-1".into(),
            name: "Phone".into(),
            principal_id: Some("principal-alice".into()),
        };
        request.body = json!({
            "projectID": " project-1 ",
            "presetID": "preset-1",
            "command": "controller-command-must-not-win",
            // Attribution is authenticated adapter context, never a
            // Controller-selected create field.
            "ownerPrincipalID": "principal-attacker",
            "createdByDeviceID": "attacker-device",
            "initialText": "héllo\n\u{1b}[A",
        });

        let response = route_with_create_context(&request, None, Some(&context)).unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body["sessionID"], "created-session");
        assert!(response.body["capturedAtUnixMs"].as_u64().unwrap() > 0);
        assert_eq!(response.body["session"]["status"], "starting");
        assert_eq!(
            captured.lock().expect("capture lock").as_slice(),
            &[ResolvedHostCreate {
                project_id: "project-1".into(),
                command: "project-command --safe".into(),
                cwd: "/host/project".into(),
                worktree_path: None,
                worktree_branch: None,
                owner_principal_id: "principal-alice".into(),
                created_by_device_id: Some("phone-1".into()),
                source_preset_id: Some("preset-1".into()),
                initial_text: Some("héllo\n\u{1b}[A".into()),
                initial_text_submit_mode: HostCreateSubmitMode::PasteAndSubmit,
            }]
        );
    }

    #[test]
    fn create_legacy_or_invalid_principal_falls_back_to_host_owner() {
        let captured = Arc::new(Mutex::new(Vec::<ResolvedHostCreate>::new()));
        let executor_captured = Arc::clone(&captured);
        let context = create_catalog(Arc::new(move |request| {
            executor_captured
                .lock()
                .expect("capture lock")
                .push(request);
            Ok(HostCreateOutcome {
                session_id: "created-session".into(),
                session: None,
            })
        }));
        let mut request = valid_create_request();
        request.principal = ControllerPrincipal::PairedDevice {
            device_id: "phone-legacy".into(),
            name: "Phone".into(),
            principal_id: None,
        };

        let response = route_with_create_context(&request, None, Some(&context)).unwrap();

        assert_eq!(response.status, 200);
        let captured = captured.lock().expect("capture lock");
        assert_eq!(captured[0].owner_principal_id, "host-owner:test");
        assert_eq!(
            captured[0].created_by_device_id.as_deref(),
            Some("phone-legacy")
        );
    }

    #[test]
    fn create_uses_only_catalogued_worktree_and_rejects_folders() {
        let captured = Arc::new(Mutex::new(Vec::<ResolvedHostCreate>::new()));
        let executor_captured = Arc::clone(&captured);
        let context = create_catalog(Arc::new(move |request| {
            executor_captured
                .lock()
                .expect("capture lock")
                .push(request);
            Ok(HostCreateOutcome {
                session_id: "worktree-session".into(),
                session: None,
            })
        }));
        let mut request = valid_create_request();
        request.body = json!({
            "projectID": "worktree-1",
            "command": "  codex --full-auto\n",
            "worktreePath": "/host/worktrees/feature",
            "worktreeBranch": "feature/remote",
            "initialText": "raw bytes\r",
            "initialTextSubmitMode": "raw",
        });
        let response = route_with_create_context(&request, None, Some(&context)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            captured.lock().expect("capture lock").as_slice(),
            &[ResolvedHostCreate {
                project_id: "worktree-1".into(),
                command: "codex --full-auto".into(),
                cwd: "/host/worktrees/feature".into(),
                worktree_path: Some("/host/worktrees/feature".into()),
                worktree_branch: Some("feature/remote".into()),
                owner_principal_id: "host-owner:test".into(),
                created_by_device_id: None,
                source_preset_id: None,
                initial_text: Some("raw bytes\r".into()),
                initial_text_submit_mode: HostCreateSubmitMode::Raw,
            }]
        );

        let mut mismatch = valid_create_request();
        mismatch.body = json!({
            "projectID": "worktree-1",
            "command": "codex",
            "worktreePath": "/attacker/chosen/path",
        });
        let response = route_with_create_context(&mismatch, None, Some(&context)).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(
            response.body["error"],
            "worktree does not belong to project"
        );

        let mut folder = valid_create_request();
        folder.body = json!({ "projectID": "folder-1", "command": "codex" });
        let response = route_with_create_context(&folder, None, Some(&context)).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], "project is a folder");
        assert_eq!(captured.lock().expect("capture lock").len(), 1);

        // A sidebar group carries its parent's path: creating inside it keeps
        // the group as the Session's project and runs in the parent directory.
        let mut group = valid_create_request();
        group.body = json!({ "projectID": "group-1", "command": "codex" });
        let response = route_with_create_context(&group, None, Some(&context)).unwrap();
        assert_eq!(response.status, 200, "{:?}", response.body);
        let executed = captured.lock().expect("capture lock");
        assert_eq!(executed.len(), 2);
        assert_eq!(executed[1].project_id, "group-1");
        assert_eq!(executed[1].cwd, "/host/project");
        assert_eq!(executed[1].worktree_path, None);
    }

    #[test]
    fn create_rejects_invalid_or_unbounded_requests_before_execution() {
        let executions = Arc::new(AtomicUsize::new(0));
        let executor_count = Arc::clone(&executions);
        let context = create_catalog(Arc::new(move |_| {
            executor_count.fetch_add(1, Ordering::SeqCst);
            Ok(HostCreateOutcome {
                session_id: "unexpected".into(),
                session: None,
            })
        }));
        let bodies = [
            json!({}),
            json!({ "projectID": "project-1" }),
            json!({ "projectID": "project-1", "presetID": "disabled" }),
            json!({
                "projectID": "project-1",
                "command": "x".repeat(MAX_CREATE_COMMAND_BYTES + 1),
            }),
            json!({
                "projectID": "project-1",
                "command": "codex",
                "initialText": "x".repeat(MAX_CREATE_INITIAL_TEXT_BYTES + 1),
            }),
            json!({
                "projectID": "project-1",
                "command": "codex",
                "initialTextSubmitMode": "futureMode",
            }),
        ];

        for body in bodies {
            let mut request = valid_create_request();
            request.body = body;
            let response = route_with_create_context(&request, None, Some(&context)).unwrap();
            assert_eq!(response.status, 400, "{}", request.body);
        }
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn create_replay_executes_once_and_rejects_request_id_reuse() {
        let executions = Arc::new(AtomicUsize::new(0));
        let executor_count = Arc::clone(&executions);
        let context = create_catalog(Arc::new(move |_| {
            executor_count.fetch_add(1, Ordering::SeqCst);
            Ok(HostCreateOutcome {
                session_id: "replayed-session".into(),
                session: None,
            })
        }));
        let mut first = valid_create_request();
        first.id = Some(format!("create-replay-{}", uuid::Uuid::new_v4()));

        let first_response =
            route_with_create_context(&first, None, Some(&context)).expect("create response");
        let replay =
            route_with_create_context(&first, None, Some(&context)).expect("replay response");
        assert_eq!(replay, first_response);
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        let mut mismatch = first;
        mismatch.body["command"] = "different".into();
        let response = route_with_create_context(&mismatch, None, Some(&context)).unwrap();
        assert_eq!(response.status, 409);
        assert_eq!(
            response.body["error"],
            "request id reused with different request"
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn slow_create_does_not_block_an_unrelated_replay_mutation() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let executor_gate = Arc::clone(&gate);
        let (entered_tx, entered_rx) = mpsc::channel();
        let slow_context = create_catalog(Arc::new(move |_| {
            entered_tx.send(()).expect("slow create entry receiver");
            let (lock, ready) = &*executor_gate;
            let mut released = lock.lock().expect("slow create gate");
            while !*released {
                released = ready.wait(released).expect("slow create gate wait");
            }
            Ok(HostCreateOutcome {
                session_id: "slow-session".into(),
                session: None,
            })
        }));
        let mut slow_request = valid_create_request();
        slow_request.id = Some(format!("slow-create-{}", uuid::Uuid::new_v4()));
        let slow_thread = thread::spawn(move || {
            route_with_create_context(&slow_request, None, Some(&slow_context))
                .expect("slow create response")
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("slow create should enter its executor");

        let fast_context = create_catalog(Arc::new(move |_| {
            Ok(HostCreateOutcome {
                session_id: "fast-session".into(),
                session: None,
            })
        }));
        let mut fast_request = valid_create_request();
        fast_request.id = Some(format!("fast-create-{}", uuid::Uuid::new_v4()));
        let (fast_tx, fast_rx) = mpsc::channel();
        let fast_thread = thread::spawn(move || {
            let response = route_with_create_context(&fast_request, None, Some(&fast_context));
            fast_tx.send(response).expect("fast response receiver");
        });
        let fast_response = fast_rx.recv_timeout(Duration::from_millis(500));

        let (lock, ready) = &*gate;
        *lock.lock().expect("slow create gate release") = true;
        ready.notify_all();
        let slow_response = slow_thread.join().expect("slow create thread");
        fast_thread.join().expect("fast create thread");

        assert_eq!(slow_response.status, 200);
        let fast_response = fast_response
            .expect("unrelated replay mutation must not wait for slow create")
            .expect("fast create response");
        assert_eq!(fast_response.status, 200);
        assert_eq!(fast_response.body["sessionID"], "fast-session");
    }

    #[test]
    fn concurrent_same_id_create_has_one_leader_and_one_result() {
        let executions = Arc::new(AtomicUsize::new(0));
        let executor_count = Arc::clone(&executions);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let executor_gate = Arc::clone(&gate);
        let (entered_tx, entered_rx) = mpsc::channel();
        let context = create_catalog(Arc::new(move |_| {
            executor_count.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).expect("create entry receiver");
            let (lock, ready) = &*executor_gate;
            let mut released = lock.lock().expect("create gate");
            while !*released {
                released = ready.wait(released).expect("create gate wait");
            }
            Ok(HostCreateOutcome {
                session_id: "single-flight-session".into(),
                session: None,
            })
        }));
        let mut request = valid_create_request();
        request.id = Some(format!("concurrent-create-{}", uuid::Uuid::new_v4()));

        let leader_context = context.clone();
        let leader_request = request.clone();
        let leader = thread::spawn(move || {
            route_with_create_context(&leader_request, None, Some(&leader_context))
                .expect("leader response")
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("leader should enter create executor");

        let follower_started = Arc::new(Barrier::new(2));
        let follower_barrier = Arc::clone(&follower_started);
        let follower = thread::spawn(move || {
            follower_barrier.wait();
            route_with_create_context(&request, None, Some(&context)).expect("follower response")
        });
        follower_started.wait();
        thread::yield_now();

        let (lock, ready) = &*gate;
        *lock.lock().expect("create gate release") = true;
        ready.notify_all();
        let leader_response = leader.join().expect("leader thread");
        let follower_response = follower.join().expect("follower thread");

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(follower_response, leader_response);
        assert_eq!(leader_response.body["sessionID"], "single-flight-session");
    }

    #[test]
    fn in_flight_request_id_mismatch_is_rejected_without_waiting() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let executor_gate = Arc::clone(&gate);
        let (entered_tx, entered_rx) = mpsc::channel();
        let context = create_catalog(Arc::new(move |_| {
            entered_tx.send(()).expect("create entry receiver");
            let (lock, ready) = &*executor_gate;
            let mut released = lock.lock().expect("create gate");
            while !*released {
                released = ready.wait(released).expect("create gate wait");
            }
            Ok(HostCreateOutcome {
                session_id: "mismatch-leader".into(),
                session: None,
            })
        }));
        let mut request = valid_create_request();
        request.id = Some(format!("in-flight-mismatch-{}", uuid::Uuid::new_v4()));
        let leader_context = context.clone();
        let leader_request = request.clone();
        let leader = thread::spawn(move || {
            route_with_create_context(&leader_request, None, Some(&leader_context))
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("leader should enter create executor");

        request.body["command"] = "different".into();
        let (mismatch_tx, mismatch_rx) = mpsc::channel();
        let mismatch = thread::spawn(move || {
            mismatch_tx
                .send(route_with_create_context(&request, None, Some(&context)))
                .expect("mismatch response receiver");
        });
        let mismatch_response = mismatch_rx.recv_timeout(Duration::from_millis(500));

        let (lock, ready) = &*gate;
        *lock.lock().expect("create gate release") = true;
        ready.notify_all();
        leader.join().expect("leader thread");
        mismatch.join().expect("mismatch thread");

        let mismatch_response = mismatch_response
            .expect("fingerprint mismatch must not wait for the leader")
            .expect("mismatch response");
        assert_eq!(mismatch_response.status, 409);
        assert_eq!(
            mismatch_response.body["error"],
            "request id reused with different request"
        );
    }

    #[test]
    fn unhandled_replay_route_does_not_leave_an_in_flight_entry() {
        let mut request = valid_create_request();
        request.id = Some(format!("unhandled-replay-{}", uuid::Uuid::new_v4()));
        assert!(route(&request, None).is_none());

        let (response_tx, response_rx) = mpsc::channel();
        let replay = thread::spawn(move || {
            response_tx
                .send(route(&request, None))
                .expect("unhandled response receiver");
        });
        let response = response_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("unhandled replay must not wait forever");
        replay.join().expect("unhandled replay thread");
        assert!(response.is_none());
    }

    #[test]
    fn panicking_replay_leader_wakes_matching_followers() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let executor_gate = Arc::clone(&gate);
        let (entered_tx, entered_rx) = mpsc::channel();
        let context = create_catalog(Arc::new(move |_| {
            entered_tx.send(()).expect("panic entry receiver");
            let (lock, ready) = &*executor_gate;
            let mut released = lock.lock().expect("panic gate");
            while !*released {
                released = ready.wait(released).expect("panic gate wait");
            }
            panic!("intentional replay leader panic");
        }));
        let mut request = valid_create_request();
        request.id = Some(format!("panic-replay-{}", uuid::Uuid::new_v4()));

        let leader_context = context.clone();
        let leader_request = request.clone();
        let leader = thread::spawn(move || {
            route_with_create_context(&leader_request, None, Some(&leader_context))
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("leader should enter executor");

        let release_gate = Arc::clone(&gate);
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let (lock, ready) = &*release_gate;
            *lock.lock().expect("panic gate release") = true;
            ready.notify_all();
        });
        let follower = route_with_create_context(&request, None, Some(&context))
            .expect("follower receives terminal response");

        releaser.join().expect("releaser thread");
        assert!(leader.join().is_err(), "leader should still unwind");
        assert_eq!(follower.status, 500);
        assert_eq!(follower.body["error"], "request processing aborted");
    }

    #[test]
    fn envelope_round_trips_without_losing_principal_or_request_id() {
        let mut original = request("POST", "/mobile/upload");
        original.content_type = Some("image/png".into());
        original.body_base64 = Some("iVBORw==".into());
        let encoded = serde_json::to_vec(&original).unwrap();
        let decoded: ControllerRequest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, original);
        let value: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["principal"]["kind"], "owner_transport");
        assert_eq!(value["bodyBase64"], "iVBORw==");
    }

    #[test]
    fn bootstrap_publishes_isolation_tier_and_optional_environment() {
        let context = HostBootstrapContext::headless(json!({ "sessions": [] }));
        let body = bootstrap_body(&context);
        // Always present, one of the three values; this build's own tier.
        let tier = body.get("hostIsolationTier").and_then(Value::as_str);
        assert!(
            matches!(tier, Some("vm" | "container" | "host")),
            "tier: {tier:?}"
        );
        assert_eq!(tier, Some(host_isolation_tier()));
        // Environment is optional and only appears as a Box descriptor.
        if let Some(environment) = body.get("hostEnvironment") {
            assert_eq!(environment.get("kind").and_then(Value::as_str), Some("box"));
        }
    }

    #[test]
    fn box_identity_reads_the_documented_probe_order() {
        // The explicit override wins and yields a Box environment.
        std::env::set_var("UNPEEL_HOST_ENVIRONMENT_BOX_ID", "bx_probe_test");
        let environment = host_environment().expect("box environment");
        assert_eq!(environment.get("kind").and_then(Value::as_str), Some("box"));
        assert_eq!(
            environment.get("id").and_then(Value::as_str),
            Some("bx_probe_test")
        );
        std::env::remove_var("UNPEEL_HOST_ENVIRONMENT_BOX_ID");
    }

    #[test]
    fn macos_reports_the_host_tier_and_no_box() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(host_isolation_tier(), "host");
        }
    }

    #[test]
    fn bootstrap_metadata_is_router_owned() {
        let mut context = HostBootstrapContext::headless(json!({
            "sessions": [],
            "protocolVersion": 99,
            "capturedAtUnixMs": 1,
            "hostProtocol": { "majorVersion": 99 },
            "macID": "stale",
        }));
        context.host_id = Some("host-1".into());
        context.remote_server_port = Some(7123);
        context.remote_server_certificate_fingerprint = Some("AA:BB".into());
        context.pending_approvals = vec![json!({ "id": "approval-1" })];

        let response = route(&request("GET", "/mobile/bootstrap"), Some(&context)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body["macID"], "host-1");
        assert_eq!(response.body["protocolVersion"], 1);
        assert!(response.body["capturedAtUnixMs"].as_u64().unwrap() > 1);
        assert_eq!(response.body["remoteServerPort"], 7123);
        assert_eq!(response.body["hostProtocol"]["majorVersion"], 1);
        assert_eq!(response.body["pendingApprovals"][0]["id"], "approval-1");
    }

    #[test]
    fn route_context_keeps_archives_out_of_bootstrap() {
        let mut archives = HashMap::new();
        archives.insert(
            "project-1".into(),
            vec![json!({ "id": "archived-session", "title": "Secret history" })],
        );
        let context = HostRouteContext {
            bootstrap: Some(HostBootstrapContext::headless(json!({ "sessions": [] }))),
            archived_sessions_by_project: archives,
        };

        let response =
            route_with_context(&request("GET", "/mobile/bootstrap"), Some(&context)).unwrap();
        assert_eq!(response.status, 200);
        assert!(response.body.get("archivedSessionsByProject").is_none());
        assert!(!response.body.to_string().contains("Secret history"));
    }

    #[test]
    fn archive_list_distinguishes_missing_unknown_and_known_empty_projects() {
        let missing = route_with_context(&request("GET", "/mobile/archive"), None).unwrap();
        assert_eq!(missing.status, 400);
        assert_eq!(missing.body["error"], "project_id required");

        let mut archive_request = request("GET", "/mobile/archive");
        archive_request
            .query
            .insert("project_id".into(), "unknown".into());
        let unavailable = route_with_context(&archive_request, None).unwrap();
        assert_eq!(unavailable.status, 500);
        assert_eq!(unavailable.body["error"], "archive context unavailable");

        let unknown_context = HostRouteContext::default();
        let unknown = route_with_context(&archive_request, Some(&unknown_context)).unwrap();
        assert_eq!(unknown.status, 404);
        assert_eq!(unknown.body["error"], "unknown project");

        archive_request
            .query
            .insert("project_id".into(), "project-1".into());
        let context = HostRouteContext {
            bootstrap: None,
            archived_sessions_by_project: HashMap::from([("project-1".into(), Vec::new())]),
        };
        let known = route_with_context(&archive_request, Some(&context)).unwrap();
        assert_eq!(known.status, 200);
        assert_eq!(
            known.body,
            json!({ "projectID": "project-1", "sessions": [] })
        );
    }

    #[test]
    fn archive_list_preserves_adapter_resolved_session_json() {
        let session = json!({
            "id": "session-1",
            "projectID": "project-1",
            "title": "Archived",
            "command": "claude",
            "createdAtUnixMs": 42,
            "status": "exited",
            "activity": "idle",
            "unread": false,
            "pinned": false,
            "archived": true,
        });
        let context = HostRouteContext {
            bootstrap: None,
            archived_sessions_by_project: HashMap::from([(
                "project-1".into(),
                vec![session.clone()],
            )]),
        };
        let mut archive_request = request("GET", "/mobile/archive");
        archive_request
            .query
            .insert("project_id".into(), " project-1 ".into());

        let response = route_with_context(&archive_request, Some(&context)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body["projectID"], "project-1");
        assert_eq!(response.body["sessions"], json!([session]));
    }

    #[test]
    fn metrics_missing_session_is_a_owned_bad_request() {
        let response = route(&request("GET", "/mobile/metrics"), None).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], "invalid session id");
    }

    #[test]
    fn transcript_missing_session_is_an_owned_bad_request() {
        let response = route(&request("GET", "/mobile/transcript-markdown"), None).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], "invalid session id");
    }

    #[test]
    fn write_and_resize_validate_their_owned_envelopes() {
        let write = route(&request("POST", "/mobile/write"), None).unwrap();
        assert_eq!(write.status, 400);
        assert_eq!(write.body["error"], "invalid session id");

        for session_id in ["   ", "../escape", "folder/session", r"folder\session"] {
            let mut invalid = request("POST", "/mobile/write");
            invalid.body = json!({ "sessionID": session_id, "data": "hello" });
            let response = route(&invalid, None).unwrap();
            assert_eq!(response.status, 400, "{session_id:?}");
            assert_eq!(response.body["error"], "invalid session id");
        }

        let mut wrong_data = request("POST", "/mobile/write");
        wrong_data.body = json!({ "sessionID": "session-1", "data": 42 });
        let wrong_data = route(&wrong_data, None).unwrap();
        assert_eq!(wrong_data.status, 400);
        assert_eq!(wrong_data.body["error"], "request failed");

        let mut resize_request = request("POST", "/mobile/resize");
        resize_request.body = json!({ "sessionID": "missing-session" });
        let resize = route(&resize_request, None).unwrap();
        assert_eq!(resize.status, 400);
        assert_eq!(resize.body["error"], "request failed");
    }

    #[test]
    fn terminal_failure_after_dispatch_is_never_reported_as_not_applied() {
        let error = terminal_effect_dispatch_result(Err(
            "session Host closed after consuming the command".into(),
        ))
        .unwrap_err();
        assert_eq!(error.status, 500);
        assert_eq!(error.message, "session host acknowledgement unavailable");
    }

    #[test]
    fn screenshot_request_validates_its_owned_envelope() {
        let response = route(&request("POST", "/mobile/request-screenshot"), None).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], "invalid session id");
    }

    #[test]
    fn mark_read_rejects_a_missing_session_id() {
        let response = route(&request("POST", "/mobile/mark-read"), None).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(response.body["error"], "invalid session id");
    }

    #[test]
    fn artifact_routes_validate_paths_without_touching_the_filesystem() {
        let list = route(&request("GET", "/mobile/artifacts"), None).unwrap();
        assert_eq!(list.status, 400);
        assert_eq!(list.body["error"], "invalid session id");

        let mut read = request("GET", "/mobile/artifact");
        read.query.insert("session_id".into(), "session-1".into());
        read.query.insert("kind".into(), "screenshots".into());
        let read = route(&read, None).unwrap();
        assert_eq!(read.status, 400);
        assert_eq!(read.body["error"], "invalid artifact path");

        let mut unknown_kind = request("GET", "/mobile/artifact");
        unknown_kind
            .query
            .insert("session_id".into(), "session-1".into());
        unknown_kind.query.insert("kind".into(), "unknown".into());
        unknown_kind.query.insert("name".into(), "x".into());
        let unknown_kind = route(&unknown_kind, None).unwrap();
        assert_eq!(unknown_kind.status, 404);
        assert_eq!(unknown_kind.body["error"], "unknown artifact kind");

        let mut delete = request("POST", "/mobile/artifact-delete");
        delete.query.insert("session_id".into(), "session-1".into());
        delete.query.insert("kind".into(), "screenshots".into());
        delete
            .query
            .insert("name".into(), "../manifest.json".into());
        let delete = route(&delete, None).unwrap();
        assert_eq!(delete.status, 400);
        assert_eq!(delete.body["error"], "invalid artifact path");

        let upload = route(&request("POST", "/mobile/upload-chunk"), None).unwrap();
        assert_eq!(upload.status, 400);
        assert_eq!(upload.body["error"], "invalid session id");
    }

    #[test]
    fn stable_request_ids_cache_mutation_outcomes_and_reject_mismatches() {
        let mut first = request("POST", "/mobile/write");
        first.id = Some(format!("replay-test-{}", uuid::Uuid::new_v4()));
        first.body = json!({
            "sessionID": format!("missing-{}", uuid::Uuid::new_v4()),
            "data": "first",
        });
        let first_response = route(&first, None).unwrap();
        assert_eq!(first_response.status, 404);

        let replay = route(&first, None).unwrap();
        assert_eq!(replay, first_response);

        let mut mismatch = first.clone();
        mismatch.body["data"] = "different".into();
        let mismatch = route(&mismatch, None).unwrap();
        assert_eq!(mismatch.status, 409);
        assert_eq!(
            mismatch.body["error"],
            "request id reused with different request"
        );
    }

    #[test]
    fn request_fingerprint_canonicalizes_query_order() {
        let mut left = request("POST", "/mobile/artifact-delete");
        left.id = Some(format!("query-order-{}", uuid::Uuid::new_v4()));
        left.query.insert("session_id".into(), "session-1".into());
        left.query.insert("kind".into(), "uploads".into());
        left.query.insert("name".into(), "report.txt".into());

        let mut right = request("POST", "/mobile/artifact-delete");
        right.id = left.id.clone();
        right.query.insert("name".into(), "report.txt".into());
        right.query.insert("kind".into(), "uploads".into());
        right.query.insert("session_id".into(), "session-1".into());

        assert_eq!(request_fingerprint(&left), request_fingerprint(&right));
    }

    #[test]
    fn unknown_routes_remain_with_the_compatibility_adapter() {
        assert!(route(&request("GET", "/mobile/not-yet-migrated"), None).is_none());
    }
}
