//! Host-owned semantic App instances and their Session-facing presentations.
//!
//! This module deliberately stops at the Host/Controller boundary:
//!
//! - the Host records that an App instance exists in a project and that a
//!   Session asked to present one of its views as a panel;
//! - a Controller decides whether that panel is visible, where its trailing
//!   edge is, how wide it is, and which window renders it.
//!
//! In particular, nothing here reads or writes `pane-layouts.json`. A reveal
//! is a monotonic revision on the semantic binding. Each Controller may store
//! a local dismissal receipt for that revision; a later intentional `ensure`
//! advances the revision and therefore asks every observing Controller to
//! reveal the panel again.
//!
//! State is an additive, versioned top-level value in `app-state.json`. The
//! ordinary `app_state::edit` choke point supplies flocking, atomic writes,
//! unknown-top-level-field preservation, and state-bus notification.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::path::Path;

pub const APP_PRESENTATIONS_STATE_KEY: &str = "app_presentations";
pub const APP_PRESENTATIONS_STATE_VERSION: u16 = 1;
pub const DEFAULT_APP_VIEW_ID: &str = "main";

const APP_ID_MAX_BYTES: usize = 128;
const VIEW_ID_MAX_BYTES: usize = 128;
const SESSION_ID_MAX_BYTES: usize = 256;
const PROJECT_ID_MAX_BYTES: usize = 256;
const RESOURCE_KIND_MAX_BYTES: usize = 64;
const RESOURCE_ID_MAX_BYTES: usize = 4 * 1024;
const REQUEST_ID_MAX_BYTES: usize = 256;
const RECENT_REQUEST_ID_LIMIT: usize = 16;

/// A stable Host resource represented by an App instance. For the first
/// Design slice this is normally a folder; Room-backed Apps can use `room`.
/// The value is identity metadata, not a path the presentation registry opens.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AppResourceRef {
    pub kind: String,
    pub id: String,
}

/// Semantic placement requested from Controllers. Geometry is intentionally
/// absent: desktop, terminal, and phone Controllers project `panel`
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppPresentationTarget {
    Panel,
}

/// One Host-owned App process/resource identity. Horizon A instances have a
/// companion hosted Session. Keeping the instance separate from its
/// presentations lets two agent Sessions refer to the same project App
/// without spawning duplicate processes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstance {
    pub id: String,
    pub app_id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<AppResourceRef>,
    pub companion_session_id: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

/// A semantic association between the calling Session and an App view.
/// `reveal_revision` is an intent edge, not a visibility claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPresentation {
    pub id: String,
    pub caller_session_id: String,
    pub instance_id: String,
    pub view_id: String,
    pub target: AppPresentationTarget,
    pub reveal_revision: u64,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

/// Internal Host input. The MCP adapter must derive `caller_session_id` and
/// `project_id` from the calling manifest; they are not agent arguments.
/// Likewise, only a direct-user Host path may mint the instance and companion
/// Session ids; the MCP adapter can attach to an existing instance only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureAppPresentation {
    pub caller_session_id: String,
    pub project_id: String,
    pub app_id: String,
    pub view_id: String,
    pub resource: Option<AppResourceRef>,
    pub target: AppPresentationTarget,
    /// Whether this ensure should advance the Controller reveal intent. The
    /// MCP adapter defaults `apps.open` to true; false is useful when a Host
    /// only needs to establish the semantic attachment.
    pub reveal: bool,
    /// Optional effect id supplied by the adapter. Replaying the same id
    /// returns the existing result without advancing the reveal revision.
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureAppPresentationResult {
    pub instance: AppInstance,
    pub presentation: AppPresentation,
    pub created_instance: bool,
    pub created_presentation: bool,
    /// True when this call created a presentation or advanced its reveal
    /// revision. False for a replay of a remembered `request_id`.
    pub reveal_requested: bool,
    pub deduplicated_request: bool,
}

/// Agent-facing receipt for `apps.open`. Backing and caller Session ids are
/// intentionally absent. Keep the full ensure result inside the Host for
/// companion lifecycle work, and serialize this projection at the MCP edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppPresentationReceipt {
    pub app_id: String,
    pub instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<AppResourceRef>,
    pub presentation_id: String,
    pub view_id: String,
    pub target: AppPresentationTarget,
    pub reveal_revision: u64,
    pub created_instance: bool,
    pub created_presentation: bool,
    pub reveal_requested: bool,
    pub deduplicated_request: bool,
}

impl EnsureAppPresentationResult {
    pub fn agent_receipt(&self) -> AppPresentationReceipt {
        AppPresentationReceipt {
            app_id: self.instance.app_id.clone(),
            instance_id: self.instance.id.clone(),
            resource: self.instance.resource.clone(),
            presentation_id: self.presentation.id.clone(),
            view_id: self.presentation.view_id.clone(),
            target: self.presentation.target,
            reveal_revision: self.presentation.reveal_revision,
            created_instance: self.created_instance,
            created_presentation: self.created_presentation,
            reveal_requested: self.reveal_requested,
            deduplicated_request: self.deduplicated_request,
        }
    }
}

/// Agent-safe App instance context. The backing companion Session id is
/// intentionally absent; Controllers can read Host state, while an agent only
/// needs the semantic App/resource identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextAppInstance {
    pub id: String,
    pub app_id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<AppResourceRef>,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

impl From<&AppInstance> for ContextAppInstance {
    fn from(instance: &AppInstance) -> Self {
        Self {
            id: instance.id.clone(),
            app_id: instance.app_id.clone(),
            project_id: instance.project_id.clone(),
            resource: instance.resource.clone(),
            created_at_unix_ms: instance.created_at_unix_ms,
            updated_at_unix_ms: instance.updated_at_unix_ms,
        }
    }
}

/// Agent-safe attached binding. The caller Session id is implicit in the
/// request and the companion Session id stays Host/Controller-private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachedAppPresentation {
    pub presentation_id: String,
    pub instance: ContextAppInstance,
    pub view_id: String,
    pub target: AppPresentationTarget,
    pub reveal_revision: u64,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectAppInstance {
    pub instance: ContextAppInstance,
    pub attached_to_caller: bool,
}

/// Live semantic neighborhood for one calling Session. It deliberately says
/// nothing about a Controller's current window, focus, direction, or width.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AppPresentationContext {
    pub attached: Vec<AttachedAppPresentation>,
    pub project: Vec<ProjectAppInstance>,
}

/// Validated Host binding consumed by trusted Controllers and by the MCP
/// adapter's narrow caller-relative pane projection. Unlike the general
/// agent-safe semantic context, this includes the identities needed to
/// classify a companion Session. The MCP edge emits those details only when
/// that companion is a direct neighbor in the durable local Controller tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerAppPresentation {
    pub presentation_id: String,
    pub caller_session_id: String,
    pub companion_session_id: String,
    pub instance_id: String,
    pub app_id: String,
    pub view_id: String,
    pub target: AppPresentationTarget,
    pub reveal_revision: u64,
}

/// Controller-local receipt written when the user dismisses a rendered App
/// panel. This value does not belong in Host state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppPresentationDismissal {
    pub presentation_id: String,
    pub dismissed_reveal_revision: u64,
}

impl AppPresentationDismissal {
    fn for_presentation(presentation: &AppPresentation) -> Self {
        Self {
            presentation_id: presentation.id.clone(),
            dismissed_reveal_revision: presentation.reveal_revision,
        }
    }

    /// Whether this receipt still suppresses the Host's latest reveal intent.
    pub fn suppresses(&self, presentation: &AppPresentation) -> bool {
        self.presentation_id == presentation.id
            && self.dismissed_reveal_revision >= presentation.reveal_revision
    }
}

/// Record a Controller-local dismissal without ever moving its receipt
/// backwards. Repeating a dismiss for the same revision is an exact no-op;
/// observing a stale presentation after a newer dismiss cannot accidentally
/// re-enable an older reveal intent.
pub fn record_app_presentation_dismissal(
    previous: Option<&AppPresentationDismissal>,
    presentation: &AppPresentation,
) -> AppPresentationDismissal {
    match previous {
        Some(previous) if previous.suppresses(presentation) => previous.clone(),
        _ => AppPresentationDismissal::for_presentation(presentation),
    }
}

pub fn should_reveal_app_presentation(
    presentation: &AppPresentation,
    dismissal: Option<&AppPresentationDismissal>,
) -> bool {
    presentation.reveal_revision > 0
        && !dismissal.is_some_and(|dismissal| dismissal.suppresses(presentation))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEnvelope {
    version: u16,
    #[serde(default)]
    instances: Vec<Value>,
    #[serde(default)]
    presentations: Vec<Value>,
    /// Preserve additive fields written by a newer implementation of this
    /// same version when an older process updates one known binding.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Default for StoredEnvelope {
    fn default() -> Self {
        Self {
            version: APP_PRESENTATIONS_STATE_VERSION,
            instances: Vec::new(),
            presentations: Vec::new(),
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAppInstance {
    #[serde(flatten)]
    value: AppInstance,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAppPresentation {
    #[serde(flatten)]
    value: AppPresentation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recent_request_ids: Vec<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn validate_token(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        return Err(format!(
            "{field} must be a non-empty ASCII identifier (letters, digits, '.', '-', '_') no longer than {max_bytes} bytes"
        ));
    }
    Ok(())
}

fn validate_opaque(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "{field} must be non-empty, trimmed, control-free, and no longer than {max_bytes} bytes"
        ));
    }
    Ok(())
}

/// Session ids become filesystem directory names elsewhere in the Host. Keep
/// unsafe path syntax out of this registry even though this module itself
/// never joins one onto a path.
pub fn validate_app_presentation_session_id(field: &str, value: &str) -> Result<(), String> {
    validate_opaque(field, value, SESSION_ID_MAX_BYTES)?;
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(format!("{field} is not a safe Session id"));
    }
    Ok(())
}

fn validate_resource(resource: &AppResourceRef) -> Result<(), String> {
    validate_token("resource.kind", &resource.kind, RESOURCE_KIND_MAX_BYTES)?;
    validate_opaque("resource.id", &resource.id, RESOURCE_ID_MAX_BYTES)
}

fn validate_ensure(request: &EnsureAppPresentation) -> Result<(), String> {
    validate_app_presentation_session_id("caller_session_id", &request.caller_session_id)?;
    validate_opaque("project_id", &request.project_id, PROJECT_ID_MAX_BYTES)?;
    validate_token("app_id", &request.app_id, APP_ID_MAX_BYTES)?;
    validate_token("view_id", &request.view_id, VIEW_ID_MAX_BYTES)?;
    if let Some(resource) = &request.resource {
        validate_resource(resource)?;
    }
    if let Some(request_id) = &request.request_id {
        validate_opaque("request_id", request_id, REQUEST_ID_MAX_BYTES)?;
    }
    Ok(())
}

fn valid_instance(instance: &AppInstance) -> bool {
    validate_token("instance.id", &instance.id, SESSION_ID_MAX_BYTES).is_ok()
        && validate_token("instance.app_id", &instance.app_id, APP_ID_MAX_BYTES).is_ok()
        && validate_opaque(
            "instance.project_id",
            &instance.project_id,
            PROJECT_ID_MAX_BYTES,
        )
        .is_ok()
        && instance
            .resource
            .as_ref()
            .is_none_or(|value| validate_resource(value).is_ok())
        && validate_app_presentation_session_id(
            "instance.companion_session_id",
            &instance.companion_session_id,
        )
        .is_ok()
        && instance.created_at_unix_ms <= instance.updated_at_unix_ms
}

fn valid_presentation(presentation: &AppPresentation) -> bool {
    validate_token("presentation.id", &presentation.id, SESSION_ID_MAX_BYTES).is_ok()
        && validate_app_presentation_session_id(
            "presentation.caller_session_id",
            &presentation.caller_session_id,
        )
        .is_ok()
        && validate_token(
            "presentation.instance_id",
            &presentation.instance_id,
            SESSION_ID_MAX_BYTES,
        )
        .is_ok()
        && validate_token(
            "presentation.view_id",
            &presentation.view_id,
            VIEW_ID_MAX_BYTES,
        )
        .is_ok()
        && presentation.created_at_unix_ms <= presentation.updated_at_unix_ms
}

fn parse_instance(value: &Value) -> Option<StoredAppInstance> {
    let stored: StoredAppInstance = serde_json::from_value(value.clone()).ok()?;
    valid_instance(&stored.value).then_some(stored)
}

fn parse_presentation(value: &Value) -> Option<StoredAppPresentation> {
    let mut stored: StoredAppPresentation = serde_json::from_value(value.clone()).ok()?;
    if !valid_presentation(&stored.value) {
        return None;
    }
    stored.recent_request_ids.retain(|request_id| {
        validate_opaque("recent_request_id", request_id, REQUEST_ID_MAX_BYTES).is_ok()
    });
    if stored.recent_request_ids.len() > RECENT_REQUEST_ID_LIMIT {
        let remove = stored.recent_request_ids.len() - RECENT_REQUEST_ID_LIMIT;
        stored.recent_request_ids.drain(..remove);
    }
    Some(stored)
}

fn envelope_from_root(root: &Map<String, Value>) -> Result<StoredEnvelope, String> {
    let Some(value) = root.get(APP_PRESENTATIONS_STATE_KEY) else {
        return Ok(StoredEnvelope::default());
    };
    let envelope: StoredEnvelope = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid {APP_PRESENTATIONS_STATE_KEY} state: {error}"))?;
    if envelope.version != APP_PRESENTATIONS_STATE_VERSION {
        return Err(format!(
            "unsupported {APP_PRESENTATIONS_STATE_KEY} version {} (this build supports {})",
            envelope.version, APP_PRESENTATIONS_STATE_VERSION
        ));
    }
    Ok(envelope)
}

fn validate_envelope(envelope: &StoredEnvelope) -> Result<(), String> {
    let mut instance_ids = HashSet::<String>::new();
    let mut companion_session_ids = HashSet::<String>::new();
    let mut instance_identities = HashSet::<(String, String, Option<AppResourceRef>)>::new();

    for (index, value) in envelope.instances.iter().enumerate() {
        let stored = parse_instance(value).ok_or_else(|| {
            format!("invalid {APP_PRESENTATIONS_STATE_KEY}.instances[{index}] entry")
        })?;
        if !instance_ids.insert(stored.value.id.clone()) {
            return Err(format!("duplicate App instance id '{}'", stored.value.id));
        }
        if !companion_session_ids.insert(stored.value.companion_session_id.clone()) {
            return Err(format!(
                "companion Session '{}' is referenced by more than one App instance",
                stored.value.companion_session_id
            ));
        }
        let identity = (
            stored.value.app_id.clone(),
            stored.value.project_id.clone(),
            stored.value.resource.clone(),
        );
        if !instance_identities.insert(identity) {
            return Err(format!(
                "duplicate App instance identity for '{}' in project '{}'",
                stored.value.app_id, stored.value.project_id
            ));
        }
    }

    let mut presentation_ids = HashSet::<String>::new();
    let mut binding_identities = HashSet::<(String, String, String, AppPresentationTarget)>::new();
    let mut request_owners = HashMap::<(String, String), String>::new();
    for (index, value) in envelope.presentations.iter().enumerate() {
        let stored = parse_presentation(value).ok_or_else(|| {
            format!("invalid {APP_PRESENTATIONS_STATE_KEY}.presentations[{index}] entry")
        })?;
        if !presentation_ids.insert(stored.value.id.clone()) {
            return Err(format!(
                "duplicate App presentation id '{}'",
                stored.value.id
            ));
        }
        if !instance_ids.contains(&stored.value.instance_id) {
            return Err(format!(
                "App presentation '{}' references missing instance '{}'",
                stored.value.id, stored.value.instance_id
            ));
        }
        let identity = (
            stored.value.caller_session_id.clone(),
            stored.value.instance_id.clone(),
            stored.value.view_id.clone(),
            stored.value.target,
        );
        if !binding_identities.insert(identity) {
            return Err(format!(
                "duplicate App presentation binding for caller '{}'",
                stored.value.caller_session_id
            ));
        }
        for request_id in &stored.recent_request_ids {
            let key = (stored.value.caller_session_id.clone(), request_id.clone());
            if let Some(other_presentation_id) = request_owners.insert(key, stored.value.id.clone())
            {
                if other_presentation_id != stored.value.id {
                    return Err(format!(
                        "request id '{request_id}' is remembered by more than one App presentation for caller '{}'",
                        stored.value.caller_session_id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validated_envelope_from_root(root: &Map<String, Value>) -> Result<StoredEnvelope, String> {
    let envelope = envelope_from_root(root)?;
    validate_envelope(&envelope)?;
    Ok(envelope)
}

fn save_envelope(root: &mut Map<String, Value>, envelope: StoredEnvelope) -> Result<(), String> {
    let value = serde_json::to_value(envelope)
        .map_err(|error| format!("encode {APP_PRESENTATIONS_STATE_KEY}: {error}"))?;
    root.insert(APP_PRESENTATIONS_STATE_KEY.to_string(), value);
    Ok(())
}

fn serialize_entry<T: Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| format!("encode App presentation entry: {error}"))
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string().to_lowercase()
}

fn monotonic_timestamp(created_at_unix_ms: u64, updated_at_unix_ms: u64, now_ms: u64) -> u64 {
    now_ms.max(created_at_unix_ms).max(updated_at_unix_ms)
}

fn remember_request_id(stored: &mut StoredAppPresentation, request_id: &str) {
    stored.recent_request_ids.push(request_id.to_owned());
    if stored.recent_request_ids.len() > RECENT_REQUEST_ID_LIMIT {
        let remove = stored.recent_request_ids.len() - RECENT_REQUEST_ID_LIMIT;
        stored.recent_request_ids.drain(..remove);
    }
}

fn ensure_in_root(
    root: &mut Map<String, Value>,
    request: &EnsureAppPresentation,
    now_ms: u64,
    allow_create_instance: bool,
) -> Result<EnsureAppPresentationResult, String> {
    validate_ensure(request)?;
    let mut envelope = validated_envelope_from_root(root)?;

    // An effect id is caller-scoped, not binding-scoped. Resolve it before
    // creating a new instance so replaying a request cannot accidentally
    // duplicate work if a retried body was changed in transit or by a stale
    // caller. A remembered id with a different semantic body is a conflict.
    if let Some(request_id) = &request.request_id {
        for value in &envelope.presentations {
            let stored = parse_presentation(value)
                .ok_or("validated App presentation could not be decoded")?;
            if stored.value.caller_session_id != request.caller_session_id
                || !stored
                    .recent_request_ids
                    .iter()
                    .any(|seen| seen == request_id)
            {
                continue;
            }
            let instance = envelope
                .instances
                .iter()
                .filter_map(parse_instance)
                .find(|instance| instance.value.id == stored.value.instance_id)
                .ok_or("remembered App presentation references a missing instance")?;
            let same_effect = instance.value.app_id == request.app_id
                && instance.value.project_id == request.project_id
                && instance.value.resource == request.resource
                && stored.value.view_id == request.view_id
                && stored.value.target == request.target;
            if !same_effect {
                return Err(format!(
                    "request id '{request_id}' was already used for a different App presentation"
                ));
            }
            return Ok(EnsureAppPresentationResult {
                instance: instance.value,
                presentation: stored.value,
                created_instance: false,
                created_presentation: false,
                reveal_requested: false,
                deduplicated_request: true,
            });
        }
    }

    let existing_instance = envelope
        .instances
        .iter()
        .enumerate()
        .find_map(|(index, value)| {
            let stored = parse_instance(value)?;
            (stored.value.app_id == request.app_id
                && stored.value.project_id == request.project_id
                && stored.value.resource == request.resource)
                .then_some((index, stored))
        });

    let (instance_index, instance, created_instance) = match existing_instance {
        Some((index, stored)) => (index, stored, false),
        None if !allow_create_instance => {
            return Err(format!(
                "No existing App instance matches '{}' in this project. Agents cannot create App Sessions; ask the user to open it first.",
                request.app_id
            ));
        }
        None => {
            let stored = StoredAppInstance {
                value: AppInstance {
                    id: new_id(),
                    app_id: request.app_id.clone(),
                    project_id: request.project_id.clone(),
                    resource: request.resource.clone(),
                    companion_session_id: new_id(),
                    created_at_unix_ms: now_ms,
                    updated_at_unix_ms: now_ms,
                },
                extra: Map::new(),
            };
            envelope.instances.push(serialize_entry(&stored)?);
            (envelope.instances.len() - 1, stored, true)
        }
    };

    let existing_presentation =
        envelope
            .presentations
            .iter()
            .enumerate()
            .find_map(|(index, value)| {
                let stored = parse_presentation(value)?;
                (stored.value.caller_session_id == request.caller_session_id
                    && stored.value.instance_id == instance.value.id
                    && stored.value.view_id == request.view_id
                    && stored.value.target == request.target)
                    .then_some((index, stored))
            });

    let (
        presentation_index,
        presentation,
        created_presentation,
        reveal_requested,
        deduplicated_request,
    ) = match existing_presentation {
        Some((index, mut stored)) => {
            let deduplicated = request.request_id.as_ref().is_some_and(|request_id| {
                stored
                    .recent_request_ids
                    .iter()
                    .any(|seen| seen == request_id)
            });
            if !deduplicated && request.reveal {
                stored.value.reveal_revision = stored
                    .value
                    .reveal_revision
                    .checked_add(1)
                    .ok_or("App presentation reveal revision overflow")?;
                stored.value.updated_at_unix_ms = monotonic_timestamp(
                    stored.value.created_at_unix_ms,
                    stored.value.updated_at_unix_ms,
                    now_ms,
                );
            }
            if !deduplicated {
                if let Some(request_id) = &request.request_id {
                    remember_request_id(&mut stored, request_id);
                }
            }
            (
                index,
                stored,
                false,
                request.reveal && !deduplicated,
                deduplicated,
            )
        }
        None => {
            let mut stored = StoredAppPresentation {
                value: AppPresentation {
                    id: new_id(),
                    caller_session_id: request.caller_session_id.clone(),
                    instance_id: instance.value.id.clone(),
                    view_id: request.view_id.clone(),
                    target: request.target,
                    reveal_revision: u64::from(request.reveal),
                    created_at_unix_ms: now_ms,
                    updated_at_unix_ms: now_ms,
                },
                recent_request_ids: Vec::new(),
                extra: Map::new(),
            };
            if let Some(request_id) = &request.request_id {
                remember_request_id(&mut stored, request_id);
            }
            envelope.presentations.push(serialize_entry(&stored)?);
            (
                envelope.presentations.len() - 1,
                stored,
                true,
                request.reveal,
                false,
            )
        }
    };

    // Re-serialize parsed entries to persist bounded request-id cleanup while
    // retaining additive fields captured by the flattened maps.
    envelope.instances[instance_index] = serialize_entry(&instance)?;
    envelope.presentations[presentation_index] = serialize_entry(&presentation)?;
    validate_envelope(&envelope)?;
    save_envelope(root, envelope)?;

    Ok(EnsureAppPresentationResult {
        instance: instance.value,
        presentation: presentation.value,
        created_instance,
        created_presentation,
        reveal_requested,
        deduplicated_request,
    })
}

/// Ensure through the process-global Host state. This is the production
/// entry point: its write announces `Change::AppState` through the normal
/// state-bus choke point.
pub fn ensure_app_presentation(
    request: &EnsureAppPresentation,
) -> Result<EnsureAppPresentationResult, String> {
    let now_ms = crate::state::current_timestamp_ms();
    crate::app_state::edit(|root| ensure_in_root(root, request, now_ms, true))
}

/// Attach/reveal an App instance that a user-created Controller or CLI flow
/// already established. This is the agent boundary: MCP may add a semantic
/// binding to an existing instance, but it never mints the companion Session
/// identity that would allow a later launch.
pub fn ensure_existing_app_presentation(
    request: &EnsureAppPresentation,
) -> Result<EnsureAppPresentationResult, String> {
    let now_ms = crate::state::current_timestamp_ms();
    crate::app_state::edit(|root| ensure_in_root(root, request, now_ms, false))
}

/// Read the exact existing App instance an agent open would be allowed to
/// attach to. This performs no state edit and is used before presenting an
/// approval prompt.
pub fn existing_app_instance(
    request: &EnsureAppPresentation,
) -> Result<Option<AppInstance>, String> {
    validate_ensure(request)?;
    let value = crate::app_state::load()?;
    let root = value.as_object().ok_or("app-state.json is not an object")?;
    let envelope = validated_envelope_from_root(root)?;
    Ok(envelope
        .instances
        .iter()
        .filter_map(parse_instance)
        .map(|stored| stored.value)
        .find(|instance| {
            instance.app_id == request.app_id
                && instance.project_id == request.project_id
                && instance.resource == request.resource
        }))
}

#[cfg(test)]
fn ensure_app_presentation_at(
    path: &Path,
    request: &EnsureAppPresentation,
    now_ms: u64,
) -> Result<EnsureAppPresentationResult, String> {
    crate::app_state::edit_at(path, |root| ensure_in_root(root, request, now_ms, true))
}

fn context_from_root(
    root: &Map<String, Value>,
    caller_session_id: &str,
    project_id: &str,
) -> Result<AppPresentationContext, String> {
    validate_app_presentation_session_id("caller_session_id", caller_session_id)?;
    validate_opaque("project_id", project_id, PROJECT_ID_MAX_BYTES)?;
    let envelope = validated_envelope_from_root(root)?;

    let mut instances = HashMap::<String, AppInstance>::new();
    for value in &envelope.instances {
        if let Some(stored) = parse_instance(value) {
            instances
                .entry(stored.value.id.clone())
                .or_insert(stored.value);
        }
    }
    let presentations: Vec<AppPresentation> = envelope
        .presentations
        .iter()
        .filter_map(parse_presentation)
        .map(|stored| stored.value)
        .filter(|presentation| instances.contains_key(&presentation.instance_id))
        .collect();

    let mut attached: Vec<AttachedAppPresentation> = presentations
        .iter()
        .filter(|presentation| presentation.caller_session_id == caller_session_id)
        .filter_map(|presentation| {
            Some(AttachedAppPresentation {
                presentation_id: presentation.id.clone(),
                instance: ContextAppInstance::from(instances.get(&presentation.instance_id)?),
                view_id: presentation.view_id.clone(),
                target: presentation.target,
                reveal_revision: presentation.reveal_revision,
                created_at_unix_ms: presentation.created_at_unix_ms,
                updated_at_unix_ms: presentation.updated_at_unix_ms,
            })
        })
        .collect();
    attached.sort_by(|left, right| {
        left.created_at_unix_ms
            .cmp(&right.created_at_unix_ms)
            .then_with(|| left.presentation_id.cmp(&right.presentation_id))
    });

    let mut project: Vec<ProjectAppInstance> = instances
        .into_values()
        .filter(|instance| instance.project_id == project_id)
        .map(|instance| {
            let attached_to_caller = presentations.iter().any(|presentation| {
                presentation.instance_id == instance.id
                    && presentation.caller_session_id == caller_session_id
            });
            ProjectAppInstance {
                instance: ContextAppInstance::from(&instance),
                attached_to_caller,
            }
        })
        .collect();
    project.sort_by(|left, right| {
        left.instance
            .created_at_unix_ms
            .cmp(&right.instance.created_at_unix_ms)
            .then_with(|| left.instance.id.cmp(&right.instance.id))
    });

    Ok(AppPresentationContext { attached, project })
}

pub fn app_presentation_context(
    caller_session_id: &str,
    project_id: &str,
) -> Result<AppPresentationContext, String> {
    let value = crate::app_state::load()?;
    let root = value.as_object().ok_or("app-state.json is not an object")?;
    context_from_root(root, caller_session_id, project_id)
}

/// Read validated presentation bindings for a Host-local Controller. This is
/// the privileged Host→Controller seam used by the native app and app-less
/// TUI to project semantic panels. MCP may also consume it internally to
/// classify one already-resolved direct neighbor; callers must not serialize
/// the unfiltered binding list.
pub fn controller_app_presentations() -> Result<Vec<ControllerAppPresentation>, String> {
    let value = crate::app_state::load()?;
    let root = value.as_object().ok_or("app-state.json is not an object")?;
    controller_app_presentations_from_root(root)
}

/// Compact, validated bootstrap projection for trusted Controllers. This is
/// deliberately the same semantic envelope the local native client already
/// reads from app-state.json, so local, scoped, SSH, and paired Controllers
/// reconcile one shape and one reveal-revision contract.
pub fn controller_app_presentations_wire() -> Result<Value, String> {
    let bindings = controller_app_presentations()?;
    Ok(controller_app_presentations_wire_from(
        bindings,
        |companion_session_id| {
            crate::session_host::refresh_manifest_health(companion_session_id).is_some_and(
                |manifest| manifest.state == crate::session_host::HostedSessionState::Running,
            )
        },
    ))
}

fn controller_app_presentations_wire_from(
    mut bindings: Vec<ControllerAppPresentation>,
    is_running: impl Fn(&str) -> bool,
) -> Value {
    bindings.retain(|binding| is_running(&binding.companion_session_id));
    let mut instances = bindings
        .iter()
        .map(|binding| {
            (
                binding.instance_id.clone(),
                serde_json::json!({
                    "id": binding.instance_id,
                    "app_id": binding.app_id,
                    "companion_session_id": binding.companion_session_id,
                }),
            )
        })
        .collect::<HashMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    instances.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });
    let presentations = bindings
        .into_iter()
        .map(|binding| {
            serde_json::json!({
                "id": binding.presentation_id,
                "caller_session_id": binding.caller_session_id,
                "instance_id": binding.instance_id,
                "target": binding.target,
                "reveal_revision": binding.reveal_revision,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "version": APP_PRESENTATIONS_STATE_VERSION,
        "instances": instances,
        "presentations": presentations,
    })
}

fn controller_app_presentations_from_root(
    root: &Map<String, Value>,
) -> Result<Vec<ControllerAppPresentation>, String> {
    let envelope = validated_envelope_from_root(root)?;
    let instances: HashMap<String, AppInstance> = envelope
        .instances
        .iter()
        .filter_map(parse_instance)
        .map(|stored| (stored.value.id.clone(), stored.value))
        .collect();
    let mut bindings = envelope
        .presentations
        .iter()
        .filter_map(parse_presentation)
        .filter_map(|stored| {
            let instance = instances.get(&stored.value.instance_id)?;
            Some(ControllerAppPresentation {
                presentation_id: stored.value.id,
                caller_session_id: stored.value.caller_session_id,
                companion_session_id: instance.companion_session_id.clone(),
                instance_id: instance.id.clone(),
                app_id: instance.app_id.clone(),
                view_id: stored.value.view_id,
                target: stored.value.target,
                reveal_revision: stored.value.reveal_revision,
            })
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| left.presentation_id.cmp(&right.presentation_id));
    Ok(bindings)
}

#[cfg(test)]
fn app_presentation_context_at(
    path: &Path,
    caller_session_id: &str,
    project_id: &str,
) -> Result<AppPresentationContext, String> {
    let value = crate::app_state::load_for_edit_at(path)?;
    let root = value.as_object().ok_or("app-state.json is not an object")?;
    context_from_root(root, caller_session_id, project_id)
}

#[cfg(test)]
fn controller_app_presentations_at(path: &Path) -> Result<Vec<ControllerAppPresentation>, String> {
    let value = crate::app_state::load_for_edit_at(path)?;
    let root = value.as_object().ok_or("app-state.json is not an object")?;
    controller_app_presentations_from_root(root)
}

/// Validate the versioned presentation envelope before a lifecycle operation
/// tears down a Session. Unknown future versions fail closed rather than being
/// rewritten by an older Host.
pub(crate) fn validate_app_presentation_state(root: &Map<String, Value>) -> Result<(), String> {
    let _ = validated_envelope_from_root(root)?;
    Ok(())
}

/// Rewrite the two safe Session references owned by this state:
///
/// - caller replacement carries its presentation binding to the resumed
///   Session; caller removal drops only that binding;
/// - companion replacement carries the App instance; companion removal drops
///   the instance and every presentation that points at it.
///
/// Additive fields are retained untouched. A malformed known entry or an
/// unsupported envelope version returns an error so the caller can fail
/// before destructive Session teardown.
pub(crate) fn rewrite_app_presentation_session_references(
    root: &mut Map<String, Value>,
    old_session_id: &str,
    replacement_session_id: Option<&str>,
) -> Result<bool, String> {
    validate_app_presentation_session_id("old_session_id", old_session_id)?;
    if let Some(replacement) = replacement_session_id {
        validate_app_presentation_session_id("replacement_session_id", replacement)?;
    }
    if !root.contains_key(APP_PRESENTATIONS_STATE_KEY) {
        return Ok(false);
    }

    let mut envelope = validated_envelope_from_root(root)?;
    let now_ms = crate::state::current_timestamp_ms();
    let mut changed = false;
    let mut removed_instance_ids = HashSet::<String>::new();

    let mut next_instances = Vec::with_capacity(envelope.instances.len());
    for value in envelope.instances {
        let Some(mut stored) = parse_instance(&value) else {
            next_instances.push(value);
            continue;
        };
        if stored.value.companion_session_id != old_session_id {
            next_instances.push(value);
            continue;
        }
        changed = true;
        match replacement_session_id {
            Some(replacement) => {
                stored.value.companion_session_id = replacement.to_owned();
                stored.value.updated_at_unix_ms = monotonic_timestamp(
                    stored.value.created_at_unix_ms,
                    stored.value.updated_at_unix_ms,
                    now_ms,
                );
                next_instances.push(serialize_entry(&stored)?);
            }
            None => {
                removed_instance_ids.insert(stored.value.id);
            }
        }
    }
    envelope.instances = next_instances;

    let mut next_presentations = Vec::with_capacity(envelope.presentations.len());
    for value in envelope.presentations {
        let Some(mut stored) = parse_presentation(&value) else {
            next_presentations.push(value);
            continue;
        };
        if removed_instance_ids.contains(&stored.value.instance_id) {
            changed = true;
            continue;
        }
        if stored.value.caller_session_id != old_session_id {
            next_presentations.push(value);
            continue;
        }
        changed = true;
        if let Some(replacement) = replacement_session_id {
            stored.value.caller_session_id = replacement.to_owned();
            stored.value.updated_at_unix_ms = monotonic_timestamp(
                stored.value.created_at_unix_ms,
                stored.value.updated_at_unix_ms,
                now_ms,
            );
            next_presentations.push(serialize_entry(&stored)?);
        }
    }
    envelope.presentations = next_presentations;

    if changed {
        validate_envelope(&envelope)?;
        save_envelope(root, envelope)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("app-state.json")
    }

    fn request(caller: &str, project: &str, request_id: Option<&str>) -> EnsureAppPresentation {
        EnsureAppPresentation {
            caller_session_id: caller.to_owned(),
            project_id: project.to_owned(),
            app_id: "unpeel.app.design".into(),
            view_id: "canvas".into(),
            resource: Some(AppResourceRef {
                kind: "folder".into(),
                id: format!("/tmp/{project}/design"),
            }),
            target: AppPresentationTarget::Panel,
            reveal: true,
            request_id: request_id.map(str::to_owned),
        }
    }

    #[test]
    fn ensure_creates_one_instance_and_one_caller_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let result = ensure_app_presentation_at(
            &path,
            &request("caller-1", "project-1", Some("open-1")),
            100,
        )
        .unwrap();

        assert!(result.created_instance);
        assert!(result.created_presentation);
        assert!(result.reveal_requested);
        assert!(!result.deduplicated_request);
        assert_eq!(result.presentation.reveal_revision, 1);
        assert_ne!(result.instance.id, result.instance.companion_session_id);
        validate_app_presentation_session_id("companion", &result.instance.companion_session_id)
            .unwrap();
        let receipt_json = serde_json::to_value(result.agent_receipt()).unwrap();
        assert!(receipt_json.get("companion_session_id").is_none());
        assert!(receipt_json.get("caller_session_id").is_none());
        assert!(receipt_json.get("project_id").is_none());

        let before_context = std::fs::read(&path).unwrap();
        let context = app_presentation_context_at(&path, "caller-1", "project-1").unwrap();
        assert_eq!(
            app_presentation_context_at(&path, "caller-1", "project-1").unwrap(),
            context,
            "context is a stable read"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before_context,
            "context never mutates Host state"
        );
        assert_eq!(context.attached.len(), 1);
        assert_eq!(context.project.len(), 1);
        assert!(context.project[0].attached_to_caller);
        assert_eq!(context.attached[0].instance.id, result.instance.id);
        let agent_json = serde_json::to_value(&context).unwrap();
        let agent_json_text = agent_json.to_string();
        assert!(!agent_json_text.contains("companion_session_id"));
        assert!(!agent_json_text.contains("caller_session_id"));

        let controller = controller_app_presentations_at(&path).unwrap();
        assert_eq!(controller.len(), 1);
        assert_eq!(controller[0].caller_session_id, "caller-1");
        assert_eq!(
            controller[0].companion_session_id,
            result.instance.companion_session_id
        );
        assert_eq!(controller[0].reveal_revision, 1);
    }

    #[test]
    fn controller_wire_projects_only_running_companions() {
        let binding = |suffix: &str| ControllerAppPresentation {
            presentation_id: format!("presentation-{suffix}"),
            caller_session_id: "caller".into(),
            companion_session_id: format!("companion-{suffix}"),
            instance_id: format!("instance-{suffix}"),
            app_id: "unpeel.app.design".into(),
            view_id: "canvas".into(),
            target: AppPresentationTarget::Panel,
            reveal_revision: 1,
        };
        let wire = controller_app_presentations_wire_from(
            vec![binding("running"), binding("exited")],
            |session_id| session_id == "companion-running",
        );
        assert_eq!(wire["instances"].as_array().unwrap().len(), 1);
        assert_eq!(wire["presentations"].as_array().unwrap().len(), 1);
        assert_eq!(wire["instances"][0]["id"], "instance-running");
        assert_eq!(wire["presentations"][0]["id"], "presentation-running");
    }

    #[test]
    fn agent_ensure_reuses_an_instance_and_never_mints_a_companion_session() {
        let mut root = Map::new();
        let first_request = request("user-caller", "project-1", Some("user-open"));

        let error = ensure_in_root(&mut root, &first_request, 100, false).unwrap_err();
        assert!(
            error.contains("Agents cannot create App Sessions"),
            "{error}"
        );
        assert!(
            root.is_empty(),
            "a refused agent open must not mutate state"
        );

        let user_created = ensure_in_root(&mut root, &first_request, 100, true).unwrap();
        assert!(user_created.created_instance);
        let companion_id = user_created.instance.companion_session_id.clone();

        let agent_request = request("agent-caller", "project-1", Some("agent-open"));
        let agent_attached = ensure_in_root(&mut root, &agent_request, 200, false).unwrap();
        assert!(!agent_attached.created_instance);
        assert!(agent_attached.created_presentation);
        assert_eq!(agent_attached.instance.id, user_created.instance.id);
        assert_eq!(agent_attached.instance.companion_session_id, companion_id);
    }

    #[test]
    fn ensure_reuses_instance_and_binding_but_advances_intentional_reveal() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let first = ensure_app_presentation_at(&path, &request("caller-1", "project-1", None), 100)
            .unwrap();
        let second =
            ensure_app_presentation_at(&path, &request("caller-1", "project-1", None), 200)
                .unwrap();

        assert!(!second.created_instance);
        assert!(!second.created_presentation);
        assert!(second.reveal_requested);
        assert_eq!(second.instance.id, first.instance.id);
        assert_eq!(second.presentation.id, first.presentation.id);
        assert_eq!(second.presentation.reveal_revision, 2);
        assert_eq!(second.presentation.updated_at_unix_ms, 200);
    }

    #[test]
    fn repeated_request_id_is_an_exact_effect_deduplication() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let request = request("caller-1", "project-1", Some("request-7"));
        let first = ensure_app_presentation_at(&path, &request, 100).unwrap();
        let replay = ensure_app_presentation_at(&path, &request, 900).unwrap();

        assert!(replay.deduplicated_request);
        assert!(!replay.reveal_requested);
        assert_eq!(replay.instance, first.instance);
        assert_eq!(replay.presentation, first.presentation);
    }

    #[test]
    fn caller_scoped_request_id_reuse_with_a_different_effect_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let original = request("caller-1", "project-1", Some("request-7"));
        ensure_app_presentation_at(&path, &original, 100).unwrap();
        let before = std::fs::read(&path).unwrap();

        let mut changed = original;
        changed.resource.as_mut().unwrap().id = "/tmp/project-1/other".into();
        let error = ensure_app_presentation_at(&path, &changed, 200).unwrap_err();
        assert!(error.contains("different App presentation"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn reveal_false_attaches_without_revealing_until_a_later_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let mut attach = request("caller-1", "project-1", Some("attach-1"));
        attach.reveal = false;
        let attached = ensure_app_presentation_at(&path, &attach, 100).unwrap();
        assert!(attached.created_presentation);
        assert!(!attached.reveal_requested);
        assert_eq!(attached.presentation.reveal_revision, 0);
        assert!(!should_reveal_app_presentation(
            &attached.presentation,
            None
        ));

        attach.request_id = Some("attach-2".into());
        let repeated_attach = ensure_app_presentation_at(&path, &attach, 150).unwrap();
        assert_eq!(repeated_attach.presentation.reveal_revision, 0);
        assert!(!repeated_attach.reveal_requested);

        attach.request_id = Some("open-1".into());
        attach.reveal = true;
        let opened = ensure_app_presentation_at(&path, &attach, 200).unwrap();
        assert_eq!(opened.presentation.reveal_revision, 1);
        assert!(opened.reveal_requested);
        assert!(should_reveal_app_presentation(&opened.presentation, None));
    }

    #[test]
    fn controller_dismissal_holds_until_a_new_reveal_revision() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let first = ensure_app_presentation_at(
            &path,
            &request("caller-1", "project-1", Some("open-1")),
            100,
        )
        .unwrap();
        let dismissal = record_app_presentation_dismissal(None, &first.presentation);
        assert_eq!(
            record_app_presentation_dismissal(Some(&dismissal), &first.presentation),
            dismissal,
            "repeating dismiss for one revision is idempotent"
        );
        assert!(!should_reveal_app_presentation(
            &first.presentation,
            Some(&dismissal)
        ));

        // A transport replay is not a new user/agent intent.
        let replay = ensure_app_presentation_at(
            &path,
            &request("caller-1", "project-1", Some("open-1")),
            150,
        )
        .unwrap();
        assert!(!should_reveal_app_presentation(
            &replay.presentation,
            Some(&dismissal)
        ));

        let reopened = ensure_app_presentation_at(
            &path,
            &request("caller-1", "project-1", Some("open-2")),
            200,
        )
        .unwrap();
        assert!(should_reveal_app_presentation(
            &reopened.presentation,
            Some(&dismissal)
        ));
        let newer_dismissal =
            record_app_presentation_dismissal(Some(&dismissal), &reopened.presentation);
        assert_eq!(newer_dismissal.dismissed_reveal_revision, 2);
        assert_eq!(
            record_app_presentation_dismissal(Some(&newer_dismissal), &first.presentation),
            newer_dismissal,
            "a stale observation cannot move a dismissal receipt backwards"
        );
    }

    #[test]
    fn project_instance_is_shared_while_each_caller_gets_its_own_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let first =
            ensure_app_presentation_at(&path, &request("caller-1", "project-1", Some("one")), 100)
                .unwrap();
        let second =
            ensure_app_presentation_at(&path, &request("caller-2", "project-1", Some("two")), 200)
                .unwrap();

        assert_eq!(first.instance.id, second.instance.id);
        assert_eq!(
            first.instance.companion_session_id,
            second.instance.companion_session_id
        );
        assert_ne!(first.presentation.id, second.presentation.id);

        let context = app_presentation_context_at(&path, "caller-1", "project-1").unwrap();
        assert_eq!(context.attached.len(), 1);
        assert_eq!(context.attached[0].presentation_id, first.presentation.id);
        assert_eq!(context.project.len(), 1);
        assert!(context.project[0].attached_to_caller);

        let second_context = app_presentation_context_at(&path, "caller-2", "project-1").unwrap();
        assert_eq!(second_context.attached.len(), 1);
        assert_eq!(
            second_context.attached[0].presentation_id,
            second.presentation.id
        );
    }

    #[test]
    fn resource_or_project_change_creates_a_distinct_instance() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let first =
            ensure_app_presentation_at(&path, &request("caller-1", "project-1", Some("one")), 100)
                .unwrap();
        let mut other_resource = request("caller-1", "project-1", Some("two"));
        other_resource.resource.as_mut().unwrap().id = "/tmp/project-1/other".into();
        let second = ensure_app_presentation_at(&path, &other_resource, 200).unwrap();
        let third = ensure_app_presentation_at(
            &path,
            &request("caller-1", "project-2", Some("three")),
            300,
        )
        .unwrap();

        assert_ne!(first.instance.id, second.instance.id);
        assert_ne!(first.instance.id, third.instance.id);
    }

    #[test]
    fn unsafe_or_unbounded_references_are_rejected_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let mut unsafe_caller = request("../another-session", "project-1", None);
        assert!(ensure_app_presentation_at(&path, &unsafe_caller, 100).is_err());
        assert!(!path.exists());

        unsafe_caller.caller_session_id = "caller-1".into();
        unsafe_caller.resource.as_mut().unwrap().id = "x".repeat(RESOURCE_ID_MAX_BYTES + 1);
        assert!(ensure_app_presentation_at(&path, &unsafe_caller, 100).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn unknown_envelope_version_fails_closed_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let original = serde_json::json!({
            "projects": [],
            APP_PRESENTATIONS_STATE_KEY: {
                "version": 99,
                "future": {"keep": true}
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();

        let error = ensure_app_presentation_at(&path, &request("caller-1", "project-1", None), 100)
            .unwrap_err();
        assert!(error.contains("unsupported"), "{error}");
        let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(after, original);
    }

    #[test]
    fn malformed_known_session_reference_fails_closed_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let original = serde_json::json!({
            "projects": [],
            APP_PRESENTATIONS_STATE_KEY: {
                "version": APP_PRESENTATIONS_STATE_VERSION,
                "instances": [{
                    "id": "instance-1",
                    "app_id": "unpeel.app.design",
                    "project_id": "project-1",
                    "companion_session_id": "../unsafe",
                    "created_at_unix_ms": 1,
                    "updated_at_unix_ms": 1
                }],
                "presentations": []
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();

        let error = ensure_app_presentation_at(&path, &request("caller-1", "project-1", None), 100)
            .unwrap_err();
        assert!(
            error.contains("invalid app_presentations.instances"),
            "{error}"
        );
        let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(after, original);
    }

    #[test]
    fn additive_envelope_and_entry_fields_survive_an_ensure() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let first =
            ensure_app_presentation_at(&path, &request("caller-1", "project-1", Some("one")), 100)
                .unwrap();
        let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let envelope = state[APP_PRESENTATIONS_STATE_KEY].as_object_mut().unwrap();
        envelope.insert("future_envelope".into(), serde_json::json!({"x": 1}));
        envelope["instances"][0]["future_instance"] = serde_json::json!("kept");
        envelope["presentations"][0]["future_presentation"] = serde_json::json!([1, 2]);
        std::fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

        let second =
            ensure_app_presentation_at(&path, &request("caller-1", "project-1", Some("two")), 200)
                .unwrap();
        assert_eq!(second.instance.id, first.instance.id);
        let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            after[APP_PRESENTATIONS_STATE_KEY]["future_envelope"]["x"],
            1
        );
        assert_eq!(
            after[APP_PRESENTATIONS_STATE_KEY]["instances"][0]["future_instance"],
            "kept"
        );
        assert_eq!(
            after[APP_PRESENTATIONS_STATE_KEY]["presentations"][0]["future_presentation"],
            serde_json::json!([1, 2])
        );
    }

    #[test]
    fn lifecycle_rewrite_carries_callers_and_companions_and_prunes_removals() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let ensured = ensure_app_presentation_at(
            &path,
            &request("caller-old", "project-1", Some("one")),
            100,
        )
        .unwrap();

        crate::app_state::edit_at(&path, |root| {
            rewrite_app_presentation_session_references(root, "caller-old", Some("caller-new"))?;
            rewrite_app_presentation_session_references(
                root,
                &ensured.instance.companion_session_id,
                Some("companion-new"),
            )?;
            Ok(())
        })
        .unwrap();
        let context = app_presentation_context_at(&path, "caller-new", "project-1").unwrap();
        assert_eq!(context.attached.len(), 1);
        let state = crate::app_state::load_for_edit_at(&path).unwrap();
        let envelope = validated_envelope_from_root(state.as_object().unwrap()).unwrap();
        assert!(envelope.instances.iter().any(|value| {
            parse_instance(value)
                .is_some_and(|instance| instance.value.companion_session_id == "companion-new")
        }));

        crate::app_state::edit_at(&path, |root| {
            rewrite_app_presentation_session_references(root, "caller-new", None)?;
            Ok(())
        })
        .unwrap();
        let context = app_presentation_context_at(&path, "caller-new", "project-1").unwrap();
        assert!(context.attached.is_empty());
        assert_eq!(
            context.project.len(),
            1,
            "the running App remains project-visible"
        );

        crate::app_state::edit_at(&path, |root| {
            rewrite_app_presentation_session_references(root, "companion-new", None)?;
            Ok(())
        })
        .unwrap();
        let context = app_presentation_context_at(&path, "caller-new", "project-1").unwrap();
        assert!(context.project.is_empty());
    }
}
