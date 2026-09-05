//! Disk-backed Host adapter for transport-neutral Controller requests.
//!
//! The native app and the TUI can enrich the shared router with in-process UI
//! state. An on-demand SSH gateway has neither frontend, so this adapter builds
//! the authoritative subset from `~/.unpeel`: app state, session manifests,
//! markers, output logs, and control sockets. Platform-only capabilities such
//! as pairing and approval prompts are deliberately not advertised here.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};

use crate::controller_api::{
    ControllerEffects, ControllerPrincipal, ControllerRequest, ControllerResponse,
    HostBootstrapContext, HostCreateContext, HostCreatePreset, HostCreateProject, HostRouteContext,
};
use crate::controller_protocol::HostProtocolDescriptor;
use crate::relay_wire::TunnelRequest;
use crate::session_host::{self, HostedSessionManifest, HostedSessionState, SessionHostCommand};

const OUTPUT_WAIT_MAX_MS: u64 = 25_000;
const OUTPUT_WAIT_POLL_MS: u64 = 20;
/// Base64 and the JSON envelope must still fit the common Relay/SSH response
/// budget. Controllers can keep paging with `nextOffset`.
// Output bytes are base64 inside the route JSON, then that JSON is base64
// again inside the transport envelope. Keep enough room for both expansions
// plus envelope metadata under the shared 512 KiB plaintext ceiling.
const OUTPUT_MAX_BYTES: usize = 256 * 1024;
const MAX_SESSION_ID_BYTES: usize = 128;

pub type ProjectColorWriter<'a> = &'a dyn Fn(&str, Option<&str>) -> Result<(), String>;

#[derive(Clone)]
pub struct ControllerHostRuntime {
    principal: ControllerPrincipal,
    hook_port: Option<u16>,
}

impl ControllerHostRuntime {
    pub fn owner_transport(
        transport: impl Into<String>,
        subject: Option<String>,
        hook_port: Option<u16>,
    ) -> Self {
        Self {
            principal: ControllerPrincipal::OwnerTransport {
                transport: transport.into(),
                subject,
                principal_id: crate::relay_uplink::ensure_host_id()
                    .ok()
                    .map(|host_id| crate::state::host_owner_principal_id(&host_id)),
            },
            hook_port,
        }
    }

    /// Translate the common Relay/SSH wire request into a Host-authenticated
    /// semantic request. The wire cannot choose its own principal.
    pub fn handle_tunnel(
        &self,
        namespace: &str,
        request: TunnelRequest,
        cancelled: &AtomicBool,
    ) -> ControllerResponse {
        self.handle_tunnel_with_project_color_writer(namespace, request, cancelled, None)
    }

    /// Workspace-worker variant of the disk Host router. The optional writer
    /// injects only the platform persistence effect; project resolution,
    /// validation, compound-effect ordering, and response dialect remain in
    /// the shared Rust Host. SSH and adapter-free headless callers keep using
    /// `handle_tunnel`, which honestly reports folder colors as unsupported.
    pub fn handle_tunnel_with_project_color_writer(
        &self,
        namespace: &str,
        request: TunnelRequest,
        cancelled: &AtomicBool,
        project_color_writer: Option<ProjectColorWriter<'_>>,
    ) -> ControllerResponse {
        if !request.path.starts_with("/mobile/") || request.path == "/mobile/pair" {
            return response(request.id, 404, json!({ "error": "not found" }));
        }
        if !matches!(request.method.as_str(), "GET" | "POST") {
            return response(request.id, 405, json!({ "error": "method not allowed" }));
        }

        let (body, body_base64) = if request.body.is_empty() {
            (Value::Null, None)
        } else {
            match serde_json::from_slice(&request.body) {
                Ok(value) => (value, None),
                Err(_) => (
                    Value::Null,
                    Some(base64::engine::general_purpose::STANDARD.encode(&request.body)),
                ),
            }
        };
        let request_id = format!("{namespace}:{}", request.id);
        let semantic = ControllerRequest {
            id: Some(request_id),
            method: request.method,
            path: request.path,
            query: request.query.into_iter().collect(),
            body,
            content_type: request.content_type,
            body_base64,
            principal: self.principal.clone(),
        };
        self.handle(&semantic, cancelled, project_color_writer)
    }

    fn handle(
        &self,
        request: &ControllerRequest,
        cancelled: &AtomicBool,
        project_color_writer: Option<ProjectColorWriter<'_>>,
    ) -> ControllerResponse {
        let needs_catalog = matches!(
            (request.method.as_str(), request.path.as_str()),
            ("GET", "/mobile/bootstrap")
                | ("GET", "/mobile/archive")
                | ("POST", "/mobile/sessions")
                | ("POST", "/mobile/project-organization")
                | ("POST", "/mobile/presets")
        );
        let catalog = if needs_catalog {
            match DiskCatalog::capture() {
                Ok(catalog) => Some(catalog),
                Err(message) => {
                    return ControllerResponse {
                        id: request.id.clone(),
                        status: 500,
                        body: json!({ "error": message }),
                    };
                }
            }
        } else {
            None
        };
        let route_context = catalog.as_ref().map(DiskCatalog::route_context);
        let create_context = catalog
            .as_ref()
            .map(|catalog| catalog.create_context(self.hook_port));
        let effects = ControllerEffects::new(Arc::new({
            let hook_port = self.hook_port;
            move |request| {
                crate::controller_api::execute_headless_session_action(request, hook_port)
            }
        }));

        if let Some(response) = crate::controller_api::route_with_effects(
            request,
            route_context.as_ref(),
            create_context.as_ref(),
            Some(&effects),
        ) {
            return response;
        }

        let (status, body) = match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/mobile/output") => output(request, cancelled),
            ("POST", "/mobile/session-organization") => organization(request),
            ("POST", "/mobile/project-organization") => {
                let projects = catalog
                    .as_ref()
                    .and_then(|catalog| catalog.bootstrap.get("projects"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                project_organization_response(&request.body, &projects, project_color_writer)
            }
            ("POST", "/mobile/presets") => {
                let presets = catalog
                    .as_ref()
                    .and_then(|catalog| catalog.bootstrap.get("presets"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                preset_patch_response(&request.body, &presets)
            }
            ("POST", "/mobile/workspace-settings") => workspace_settings_response(&request.body),
            ("POST", "/mobile/resize-desktop") => resize_desktop(request),
            // Approval queues live inside the native app or TUI. This
            // disk-only adapter must not invent an empty queue or accept a
            // no-op answer.
            ("POST", "/mobile/approvals/answer") => (
                501,
                json!({ "error": "approval answers require a frontend Host adapter" }),
            ),
            _ => (404, json!({ "error": "not found" })),
        };
        ControllerResponse {
            id: request.id.clone(),
            status,
            body,
        }
    }
}

fn response(id: u64, status: u16, body: Value) -> ControllerResponse {
    ControllerResponse {
        id: Some(id.to_string()),
        status,
        body,
    }
}

#[derive(Clone)]
struct ProjectRecord {
    id: String,
    name: String,
    path: String,
    parent_id: Option<String>,
    sort_order: u64,
    is_folder: bool,
    worktree_branch: Option<String>,
    pinned: bool,
}

struct DiskCatalog {
    host_id: String,
    bootstrap: Value,
    archives: HashMap<String, Vec<Value>>,
    projects: Vec<HostCreateProject>,
    presets: Vec<HostCreatePreset>,
}

impl DiskCatalog {
    fn capture() -> Result<Self, String> {
        let host_id = crate::relay_uplink::ensure_host_id()?;
        let state = crate::app_state::load()?;
        let menu_attention_detection = state
            .get("menu_attention_detection")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let inactive_preview_limit = wire_sidebar_inactive_window(&state);
        let date_sorted_projects: HashSet<String> = state
            .get("session_sort_modes")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|modes| modes.iter())
            .filter_map(|(id, mode)| (mode.as_str() == Some("date")).then_some(id.clone()))
            .collect();
        let mut projects = project_records(&state);
        projects.sort_by(|left, right| {
            left.sort_order
                .cmp(&right.sort_order)
                .then_with(|| left.id.cmp(&right.id))
        });
        // A drag persisted by ANY frontend lands in the shared
        // project-order.json and outranks the file's sort_order — the same
        // precedence every sidebar gives it. The bootstrap must advertise
        // the order the Host displays, or Controllers scramble it.
        let shared_order = crate::session_ops::project_order();
        if !shared_order.is_empty() {
            projects.sort_by_key(|record| {
                shared_order
                    .iter()
                    .position(|id| *id == record.id)
                    .unwrap_or(usize::MAX)
            });
        }
        let manifests = session_host::list_manifests();
        let pinned = pinned_session_ids(&state);
        let activity = activity_state();
        let known_ids: HashSet<String> =
            projects.iter().map(|project| project.id.clone()).collect();
        let folder_ids: HashSet<String> = projects
            .iter()
            .filter(|project| project.is_folder && project.parent_id.is_none())
            .map(|project| project.id.clone())
            .collect();

        let folders: Vec<Value> = projects
            .iter()
            .filter(|project| folder_ids.contains(&project.id))
            .map(|project| json!({ "id": project.id, "name": project.name }))
            .collect();
        let mut wire_projects = Vec::new();
        let mut create_projects = Vec::new();
        for (display_rank, project) in projects
            .iter()
            .filter(|project| !folder_ids.contains(&project.id))
            .enumerate()
        {
            let archived_count = manifests
                .iter()
                .filter(|manifest| {
                    effective_project_id(manifest, &known_ids) == project.id
                        && crate::session_ops::archived_marker(&manifest.session.id).is_some()
                })
                .count();
            // sortOrder is the DISPLAY rank (array order and field agree),
            // never the raw file value a shared-order drag may contradict.
            let mut value = json!({
                "id": project.id,
                "name": project.name,
                "path": project.path,
                "sortOrder": display_rank,
                "mcpBlocked": false,
                "archivedSessionCount": archived_count,
                "pinned": project.pinned,
            });
            if let Some(object) = value.as_object_mut() {
                if let Some(parent) = project.parent_id.as_deref() {
                    if folder_ids.contains(parent) {
                        object.insert("folderID".into(), parent.into());
                    } else {
                        object.insert("parentProjectID".into(), parent.into());
                    }
                }
                if let Some(branch) = project.worktree_branch.as_deref() {
                    object.insert("worktreeBranch".into(), branch.into());
                }
                if let Some(branch) = git_head_branch(&project.path) {
                    object.insert("gitBranch".into(), branch.into());
                }
                if project.is_folder && project.parent_id.is_some() {
                    object.insert("isGroup".into(), true.into());
                }
                if date_sorted_projects.contains(&project.id) {
                    object.insert("dateSorted".into(), true.into());
                }
            }
            wire_projects.push(value);
            create_projects.push(HostCreateProject {
                id: project.id.clone(),
                path: project.path.clone(),
                is_folder: project.is_folder && project.parent_id.is_some(),
                worktree_path: project
                    .worktree_branch
                    .as_ref()
                    .map(|_| project.path.clone()),
                worktree_branch: project.worktree_branch.clone(),
            });
        }

        let (wire_presets, create_presets) = presets(&state);
        let activity_log =
            crate::activity_log::ActivityLogStore::load_default().unwrap_or_default();
        let mut sessions = Vec::new();
        let mut archives: HashMap<String, Vec<Value>> = wire_projects
            .iter()
            .filter_map(|project| project.get("id")?.as_str().map(str::to_owned))
            .map(|id| (id, Vec::new()))
            .collect();
        let mut manifests = manifests;
        let host_owner_principal_id = crate::state::host_owner_principal_id(&host_id);
        manifests.sort_by(|left, right| {
            right
                .session
                .created_at
                .cmp(&left.session.created_at)
                .then_with(|| left.session.id.cmp(&right.session.id))
        });
        for manifest in &manifests {
            let project_id = effective_project_id(manifest, &known_ids);
            let archived = crate::session_ops::archived_marker(&manifest.session.id).is_some();
            let resumable = crate::session_ops::can_archive_manifest(manifest);
            let summary = session_summary_with_menu_attention(
                manifest,
                &project_id,
                pinned.contains(&manifest.session.id),
                archived,
                resumable,
                &host_owner_principal_id,
                activity.get(&manifest.session.id),
                activity_log
                    .entries()
                    .iter()
                    .rev()
                    .find(|entry| entry.session_id == manifest.session.id)
                    .filter(|entry| entry.kind == crate::activity_log::ActivityLogKind::Alert),
                menu_attention_detection,
            );
            if archived {
                archives
                    .entry(project_id.clone())
                    .or_default()
                    .push(summary.clone());
            }
            // The bootstrap includes the same inactive preview as the Host
            // sidebar. Stopped and archived rows share its limit; archive
            // status still overrides pin/manual placement and stays last.
            sessions.push(summary);
        }
        crate::session_ops::attach_mixed_session_order_fields(&mut wire_projects);
        let session_orders: HashMap<String, Vec<String>> = wire_projects
            .iter()
            .filter_map(|project| project.get("id")?.as_str())
            .map(|id| (id.to_owned(), crate::session_ops::session_order(id)))
            .collect();
        sort_wire_sessions(&mut sessions, &wire_projects, &session_orders);
        retain_wire_sidebar_inactive_window(&mut sessions, inactive_preview_limit);
        for archived in archives.values_mut() {
            sort_wire_sessions(archived, &wire_projects, &session_orders);
        }
        let workspace_settings = wire_workspace_settings(&state);
        let experimental_worktrees_enabled = workspace_settings
            .get("experimentalSettings")
            .and_then(|settings| settings.get("worktrees"))
            .and_then(Value::as_bool);
        let host_tint_hue = workspace_settings
            .get("appearanceSettings")
            .and_then(|settings| settings.get("appTint"))
            .and_then(Value::as_str)
            .and_then(|tint| match tint {
                "peel" => Some(17.0),
                "amber" => Some(45.0),
                "green" => Some(140.0),
                "teal" => Some(187.0),
                "blue" => Some(212.0),
                "indigo" => Some(243.0),
                "violet" => Some(285.0),
                _ => None,
            });

        Ok(Self {
            host_id,
            bootstrap: json!({
                "macName": hostname_short(),
                "folders": folders,
                "projects": wire_projects,
                "presets": wire_presets,
                "sessions": sessions,
                // Additive: the workspace's behavior knobs, so Controllers
                // can SHOW current values before editing them through
                // `settings.workspace.set`.
                "workspaceSettings": workspace_settings,
                "experimentalWorktreesEnabled": experimental_worktrees_enabled,
                "hostTintHue": host_tint_hue,
                "hostDeviceKind": if cfg!(target_os = "linux") { "linux" } else { "unknown" },
            }),
            archives,
            projects: create_projects,
            presets: create_presets,
        })
    }

    fn route_context(&self) -> HostRouteContext {
        let mut bootstrap = HostBootstrapContext::headless(self.bootstrap.clone());
        bootstrap.host_id = Some(self.host_id.clone());
        // These require a frontend-owned in-memory service and are not
        // available merely because the disk-backed gateway is running.
        bootstrap.protocol = disk_protocol();
        HostRouteContext {
            bootstrap: Some(bootstrap),
            archived_sessions_by_project: self.archives.clone(),
        }
    }

    fn create_context(&self, hook_port: Option<u16>) -> HostCreateContext {
        HostCreateContext::new(
            crate::state::host_owner_principal_id(&self.host_id),
            self.projects.clone(),
            self.presets.clone(),
            Arc::new(move |request| {
                crate::controller_api::execute_headless_session_create(request, hook_port)
            }),
        )
    }
}

/// Shared Host semantics for `POST /mobile/project-organization`
/// (`project.organization.set`) over a wire-project catalog: rename a group,
/// set a main project's folder color (only when the adapter can persist one —
/// `color_writer` is `None` on a bare disk gateway), flip a group's session
/// sort, and `sortOrder` — move the project to that index among its
/// same-parent siblings in the advertised display order. Persistence goes
/// through the shared choke points (`app_state::edit`,
/// `session_ops::set_project_sibling_order` — flock + state-bus announce).
/// Every field is type-checked and every unsupported field rejected before
/// anything applies, so a compound patch can never half-apply behind a
/// 400/404/501. Used by the TUI's `/mobile` server and the disk gateway so a
/// Controller sees one behavior whichever transport carried the patch.
#[allow(clippy::type_complexity)]
pub fn project_organization_response(
    body: &Value,
    wire_projects: &[Value],
    color_writer: Option<&dyn Fn(&str, Option<&str>) -> Result<(), String>>,
) -> (u16, Value) {
    let error = |message: &str| json!({ "error": message });
    let Some(project_id) = body
        .get("projectID")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (400, error("invalid project id"));
    };
    let display_name = match body.get("displayName") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
        Some(_) => return (400, error("displayName must be a string")),
    };
    let color_id = match body.get("colorID") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return (400, error("colorID must be a string")),
    };
    let date_sorted = match body.get("dateSorted") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return (400, error("dateSorted must be a boolean")),
    };
    let pinned = match body.get("pinned") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return (400, error("pinned must be a boolean")),
    };
    let sort_order = match body.get("sortOrder") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(index) if index >= 0 => Some(index as usize),
            _ => return (400, error("sortOrder must be a non-negative integer")),
        },
    };
    let folder_move_requested = match body.get("folderID") {
        None | Some(Value::Null) => false,
        Some(Value::String(_)) => true,
        Some(_) => return (400, error("folderID must be a string")),
    };

    let Some(target) = wire_projects
        .iter()
        .find(|project| project.get("id").and_then(Value::as_str) == Some(project_id))
    else {
        return (404, error("unknown project"));
    };
    // Unsupported operations are rejected after the project resolves and
    // before anything applies (native resource ordering).
    if folder_move_requested {
        return (
            501,
            error("moving a project between folders is not supported by this Host"),
        );
    }
    let parent_id = target.get("parentProjectID").and_then(Value::as_str);
    let is_group = target
        .get("isGroup")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if display_name.is_some() && !is_group {
        return (400, error("Only groups can be renamed remotely"));
    }
    if pinned.is_some() && !is_group {
        return (400, error("Only groups can be pinned remotely"));
    }
    if let Some(color) = color_id.as_deref() {
        // Folder color is a MAIN-project verb — groups and worktrees stay
        // neutral (same rule as the desktop and TUI menus).
        if parent_id.is_some() {
            return (400, error("Only main projects can be colored"));
        }
        const FOLDER_COLORS: [&str; 8] = [
            "sky", "blue", "violet", "rose", "amber", "moss", "teal", "graphite",
        ];
        if !color.is_empty() && !FOLDER_COLORS.contains(&color) {
            return (400, error(&format!("Unknown folder color: {color}")));
        }
        if color_writer.is_none() {
            return (501, error("folder colors are not supported by this Host"));
        }
    }
    if display_name.is_none()
        && color_id.is_none()
        && date_sorted.is_none()
        && sort_order.is_none()
        && pinned.is_none()
    {
        // Match the native DTO: an empty patch, explicit nulls, and a name
        // that trims to empty are successful no-ops.
        return (200, json!({ "ok": true }));
    }

    // Apply. Once any field lands, a later failure is effect-unknown;
    // Controllers must refresh Host state before deciding whether to retry.
    // The group/non-group split was already answered from the wire catalog
    // above, so a failure here is broken shared state or IO, not validation.
    if let Some(name) = display_name {
        if let Err(e) = crate::session_ops::rename_group_project(project_id, &name) {
            return (
                500,
                error(&format!("organization rename preflight failed: {e}")),
            );
        }
    }
    if let (Some(color), Some(write_color)) = (color_id.as_deref(), color_writer) {
        let color = (!color.is_empty()).then_some(color);
        if let Err(e) = write_color(project_id, color) {
            return (
                500,
                error(&format!(
                    "organization update effect unknown; refresh Host state: {e}"
                )),
            );
        }
    }
    if let Some(date_sorted) = date_sorted {
        if let Err(e) = crate::session_ops::set_session_date_sorted(project_id, date_sorted) {
            return (
                500,
                error(&format!(
                    "organization update effect unknown; refresh Host state: {e}"
                )),
            );
        }
    }
    if let Some(pinned) = pinned {
        if let Err(e) = crate::session_ops::set_group_pinned(project_id, pinned) {
            return (
                500,
                error(&format!(
                    "organization update effect unknown; refresh Host state: {e}"
                )),
            );
        }
    }
    if let Some(index) = sort_order {
        let ids_where = |filter: &dyn Fn(&Value) -> bool| -> Vec<String> {
            wire_projects
                .iter()
                .filter(|project| filter(project))
                .filter_map(|project| project.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        };
        let sibling_ids = ids_where(&|project| {
            project.get("parentProjectID").and_then(Value::as_str) == parent_id
        });
        let mut ordered = sibling_ids.clone();
        if let Some(from) = ordered.iter().position(|id| id == project_id) {
            let id = ordered.remove(from);
            let to = index.min(ordered.len());
            ordered.insert(to, id);
        }
        if ordered != sibling_ids {
            let all_ids = ids_where(&|_| true);
            if let Err(e) = crate::session_ops::set_project_sibling_order(&ordered, &all_ids) {
                return (
                    500,
                    error(&format!(
                        "organization update effect unknown; refresh Host state: {e}"
                    )),
                );
            }
        }
    }
    (200, json!({ "ok": true }))
}

/// Canonical option lists for the workspace behavior knobs. The TUI's
/// `AUTO_STOP_ARCHIVE_MINUTE_OPTIONS` and unpeel-serve's
/// `SIDEBAR_STOPPED_LIMIT_OPTIONS` mirror these (they cannot depend on this
/// crate's callers) — keep all three in sync.
const WORKSPACE_AUTO_STOP_MINUTE_OPTIONS: [i64; 7] = [0, 30, 60, 120, 240, 480, 1440];
const WORKSPACE_SIDEBAR_LIMIT_OPTIONS: [i64; 6] = [0, 3, 5, 10, 15, 25];

/// The workspace's Host-owned settings as a camelCase wire object, read from
/// the raw app-state document with the same fallbacks each consumer applies
/// (absent auto-stop = on at the default cutoff; absent access = its default).
pub fn wire_workspace_settings(state: &Value) -> Value {
    let minutes = state
        .get("auto_stop_archive_minutes")
        .and_then(Value::as_i64)
        .filter(|value| WORKSPACE_AUTO_STOP_MINUTE_OPTIONS.contains(value))
        .unwrap_or(120);
    let limit = state
        .get("sidebar_stopped_limit")
        .and_then(Value::as_i64)
        .filter(|value| WORKSPACE_SIDEBAR_LIMIT_OPTIONS.contains(value))
        .unwrap_or(5);
    let access = |key: &str, default: &str| -> String {
        state
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| default.to_owned())
    };
    json!({
        "autoStopArchiveMinutes": minutes,
        "sidebarStoppedLimit": limit,
        "browserDefaultAccess": access("browser_default_access", "on"),
        "mcpNonchildWriteAccess": access("mcp_nonchild_write_access", "ask"),
        "computerAccess": state
            .get("computer_default_access")
            // Migration fallback for minor-13 development builds.
            .or_else(|| state.get("computer_access"))
            .and_then(Value::as_str)
            .unwrap_or("ask"),
        "mcpWorktreeAccess": state
            .get("mcp_worktree_access")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "mcpAutoAddBrowserScreenshots": state
            .get("mcp_auto_add_browser_screenshots")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        "transcriptSettings": wire_transcript_settings(state.get("transcript_settings")),
        "appearanceSettings": wire_appearance_settings(state),
        "notificationSettings": {
            "menuAttentionDetection": state
                .get("menu_attention_detection")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        "experimentalSettings": wire_experimental_settings(state.get("experimental_features")),
    })
}

fn wire_appearance_settings(state: &Value) -> Value {
    let stored = state.get("appearance_settings");
    let enum_value = |source: Option<&Value>, key: &str, allowed: &[&str], fallback: &str| {
        source
            .and_then(|object| object.get(key))
            .and_then(Value::as_str)
            .filter(|value| allowed.contains(value))
            .map(str::to_owned)
            .unwrap_or_else(|| fallback.to_owned())
    };
    let unit_value = |key: &str, fallback: f64| {
        stored
            .and_then(|object| object.get(key))
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            .unwrap_or(fallback)
    };
    json!({
        "theme": enum_value(
            Some(state), "theme", &["system", "light", "dark"], "system"
        ),
        "appTint": enum_value(
            stored,
            "app_tint",
            &["none", "peel", "amber", "green", "teal", "blue", "indigo", "violet"],
            "none",
        ),
        "backgroundOpacity": unit_value("background_opacity", 0.9),
        "surfaceOpacity": unit_value("surface_opacity", 1.0),
        "backgroundTone": unit_value("background_tone", 0.22),
        "surfaceTone": unit_value("surface_tone", 0.12),
        "sessionTitleMode": enum_value(
            Some(state),
            "session_title_mode",
            &["first_prompt", "agent", "off"],
            "agent",
        ),
    })
}

fn wire_experimental_settings(stored: Option<&Value>) -> Value {
    let read = |key: &str, fallback: bool| {
        stored
            .and_then(|object| object.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(fallback)
    };
    json!({
        "worktrees": read("worktrees", true),
        "sessionsMcp": read("sessions_mcp", true),
        "browserMcp": read("browser_mcp", true),
        "computerUse": read("computer_use", false),
        "workspaces": read("workspaces", true),
    })
}

/// The stored `transcript_settings` object as camelCase wire values, with
/// the shipped defaults for absent keys (TranscriptSettings in state.rs and
/// the native Models.swift twin).
fn wire_transcript_settings(stored: Option<&Value>) -> Value {
    let read_bool = |key: &str, default: bool| {
        stored
            .and_then(|object| object.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(default)
    };
    json!({
        "includeUser": read_bool("include_user", true),
        "includeAssistant": read_bool("include_assistant", true),
        "includeReasoning": read_bool("include_reasoning", false),
        "includeTools": read_bool("include_tools", false),
        "includeFileChanges": read_bool("include_file_changes", true),
        "includePlanUpdates": read_bool("include_plan_updates", true),
        "includeSessionInfo": read_bool("include_session_info", true),
        "maxEntries": stored
            .and_then(|object| object.get("max_entries"))
            .and_then(Value::as_i64)
            .filter(|entries| *entries >= 0)
            .unwrap_or(20),
    })
}

fn workspace_nested_object<'a>(
    body: &'a Value,
    key: &str,
) -> Result<Option<&'a serde_json::Map<String, Value>>, String> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        Some(_) => Err(format!("{key} must be an object")),
    }
}

fn workspace_nested_string(
    object: Option<&serde_json::Map<String, Value>>,
    object_name: &str,
    key: &str,
    allowed: &[&str],
) -> Result<Option<String>, String> {
    match object.and_then(|object| object.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if allowed.contains(&value.as_str()) => Ok(Some(value.clone())),
        Some(_) => Err(format!(
            "{object_name}.{key} must be one of {}",
            allowed.join(", ")
        )),
    }
}

fn workspace_nested_bool(
    object: Option<&serde_json::Map<String, Value>>,
    object_name: &str,
    key: &str,
) -> Result<Option<bool>, String> {
    match object.and_then(|object| object.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{object_name}.{key} must be a boolean")),
    }
}

fn workspace_nested_unit_number(
    object: Option<&serde_json::Map<String, Value>>,
    object_name: &str,
    key: &str,
) -> Result<Option<f64>, String> {
    match object.and_then(|object| object.get(key)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_f64() {
            Some(value) if value.is_finite() && (0.0..=1.0).contains(&value) => Ok(Some(value)),
            _ => Err(format!("{object_name}.{key} must be between 0 and 1")),
        },
        Some(_) => Err(format!("{object_name}.{key} must be between 0 and 1")),
    }
}

/// Shared Host semantics for `POST /mobile/workspace-settings`
/// (`settings.workspace.set`): a typed patch over the workspace's app-state
/// presentation, notification/experimental behavior, transcript options,
/// cleanup, and MCP access policies. Every present field is validated
/// against its whitelist BEFORE anything applies (a compound patch never
/// half-applies behind a 400), then all land in one locked
/// `app_state::edit` whose save announces to every frontend of the edited
/// workspace. Used by the TUI `/mobile` server, the SSH disk gateway
/// (headless/Upstash-class Hosts), and mirrored by the native provider so a
/// Controller sees one behavior whichever transport carried the patch.
pub fn workspace_settings_response(body: &Value) -> (u16, Value) {
    let error = |message: &str| json!({ "error": message });

    let minutes = match body.get("autoStopArchiveMinutes") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(minutes) if WORKSPACE_AUTO_STOP_MINUTE_OPTIONS.contains(&minutes) => Some(minutes),
            _ => {
                return (
                    400,
                    error("autoStopArchiveMinutes must be one of 0, 30, 60, 120, 240, 480, 1440"),
                )
            }
        },
    };
    let limit = match body.get("sidebarStoppedLimit") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(limit) if WORKSPACE_SIDEBAR_LIMIT_OPTIONS.contains(&limit) => Some(limit),
            _ => {
                return (
                    400,
                    error("sidebarStoppedLimit must be one of 0, 3, 5, 10, 15, 25"),
                )
            }
        },
    };
    let string_field = |key: &str, allowed: &[&str]| -> Result<Option<String>, (u16, Value)> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) if allowed.contains(&value.as_str()) => {
                Ok(Some(value.clone()))
            }
            Some(_) => Err((
                400,
                error(&format!("{key} must be one of {}", allowed.join(", "))),
            )),
        }
    };
    let browser = match string_field("browserDefaultAccess", &["on", "ask", "off"]) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mcp_write = match string_field("mcpNonchildWriteAccess", &["ask", "allow", "deny"]) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let computer = match string_field("computerAccess", &["ask", "allow", "off"]) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let bool_field = |key: &str| -> Result<Option<bool>, (u16, Value)> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err((400, error(&format!("{key} must be a boolean")))),
        }
    };
    let worktrees = match bool_field("mcpWorktreeAccess") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let screenshots = match bool_field("mcpAutoAddBrowserScreenshots") {
        Ok(value) => value,
        Err(response) => return response,
    };
    // transcriptSettings: an optional nested patch — every present subfield
    // type-checked here; merged into the stored object on apply.
    let transcript_patch: Option<Vec<(String, Value)>> = match body.get("transcriptSettings") {
        None | Some(Value::Null) => None,
        Some(Value::Object(object)) => {
            let mut updates = Vec::new();
            const BOOL_KEYS: [(&str, &str); 7] = [
                ("includeUser", "include_user"),
                ("includeAssistant", "include_assistant"),
                ("includeReasoning", "include_reasoning"),
                ("includeTools", "include_tools"),
                ("includeFileChanges", "include_file_changes"),
                ("includePlanUpdates", "include_plan_updates"),
                ("includeSessionInfo", "include_session_info"),
            ];
            for (wire_key, stored_key) in BOOL_KEYS {
                match object.get(wire_key) {
                    None | Some(Value::Null) => {}
                    Some(Value::Bool(value)) => {
                        updates.push((stored_key.to_owned(), (*value).into()))
                    }
                    Some(_) => {
                        return (
                            400,
                            error(&format!("transcriptSettings.{wire_key} must be a boolean")),
                        )
                    }
                }
            }
            match object.get("maxEntries") {
                None | Some(Value::Null) => {}
                Some(value) => match value.as_i64() {
                    Some(entries) if entries >= 0 => {
                        updates.push(("max_entries".to_owned(), entries.into()))
                    }
                    _ => {
                        return (
                            400,
                            error("transcriptSettings.maxEntries must be a non-negative integer"),
                        )
                    }
                },
            }
            (!updates.is_empty()).then_some(updates)
        }
        Some(_) => return (400, error("transcriptSettings must be an object")),
    };

    let appearance_object = match workspace_nested_object(body, "appearanceSettings") {
        Ok(value) => value,
        Err(message) => return (400, error(&message)),
    };
    let nested_string = |key: &str, allowed: &[&str]| {
        workspace_nested_string(appearance_object, "appearanceSettings", key, allowed)
    };
    let appearance_theme = match nested_string("theme", &["system", "light", "dark"]) {
        Ok(value) => value,
        Err(message) => return (400, error(&message)),
    };
    let appearance_tint = match nested_string(
        "appTint",
        &[
            "none", "peel", "amber", "green", "teal", "blue", "indigo", "violet",
        ],
    ) {
        Ok(value) => value,
        Err(message) => return (400, error(&message)),
    };
    let appearance_title_mode =
        match nested_string("sessionTitleMode", &["first_prompt", "agent", "off"]) {
            Ok(value) => value,
            Err(message) => return (400, error(&message)),
        };
    let mut appearance_patch = Vec::new();
    for (wire_key, stored_key) in [
        ("backgroundOpacity", "background_opacity"),
        ("surfaceOpacity", "surface_opacity"),
        ("backgroundTone", "background_tone"),
        ("surfaceTone", "surface_tone"),
    ] {
        match workspace_nested_unit_number(appearance_object, "appearanceSettings", wire_key) {
            Ok(Some(value)) => appearance_patch.push((stored_key.to_owned(), json!(value))),
            Ok(None) => {}
            Err(message) => return (400, error(&message)),
        }
    }
    if let Some(value) = &appearance_tint {
        appearance_patch.push(("app_tint".to_owned(), value.clone().into()));
    }

    let notification_object = match workspace_nested_object(body, "notificationSettings") {
        Ok(value) => value,
        Err(message) => return (400, error(&message)),
    };
    let menu_attention = match workspace_nested_bool(
        notification_object,
        "notificationSettings",
        "menuAttentionDetection",
    ) {
        Ok(value) => value,
        Err(message) => return (400, error(&message)),
    };

    let experimental_object = match workspace_nested_object(body, "experimentalSettings") {
        Ok(value) => value,
        Err(message) => return (400, error(&message)),
    };
    let mut experimental_patch = Vec::new();
    for (wire_key, stored_key) in [
        ("worktrees", "worktrees"),
        ("sessionsMcp", "sessions_mcp"),
        ("browserMcp", "browser_mcp"),
        ("computerUse", "computer_use"),
        ("workspaces", "workspaces"),
    ] {
        match workspace_nested_bool(experimental_object, "experimentalSettings", wire_key) {
            Ok(Some(value)) => experimental_patch.push((stored_key.to_owned(), value)),
            Ok(None) => {}
            Err(message) => return (400, error(&message)),
        }
    }

    if minutes.is_none()
        && limit.is_none()
        && browser.is_none()
        && mcp_write.is_none()
        && computer.is_none()
        && worktrees.is_none()
        && screenshots.is_none()
        && transcript_patch.is_none()
        && appearance_theme.is_none()
        && appearance_title_mode.is_none()
        && appearance_patch.is_empty()
        && menu_attention.is_none()
        && experimental_patch.is_empty()
    {
        return (200, json!({ "ok": true }));
    }

    let outcome = crate::app_state::edit(|object| {
        if let Some(minutes) = minutes {
            object.insert("auto_stop_archive_minutes".into(), minutes.into());
        }
        if let Some(limit) = limit {
            object.insert("sidebar_stopped_limit".into(), limit.into());
        }
        if let Some(browser) = &browser {
            object.insert("browser_default_access".into(), browser.clone().into());
        }
        if let Some(value) = &mcp_write {
            object.insert("mcp_nonchild_write_access".into(), value.clone().into());
        }
        if let Some(value) = &computer {
            object.insert("computer_default_access".into(), value.clone().into());
            // Do not leave the short-lived minor-13 spelling able to shadow
            // later compatibility readers.
            object.remove("computer_access");
        }
        if let Some(value) = worktrees {
            object.insert("mcp_worktree_access".into(), value.into());
        }
        if let Some(value) = screenshots {
            object.insert("mcp_auto_add_browser_screenshots".into(), value.into());
        }
        if let Some(updates) = &transcript_patch {
            let mut stored = object
                .get("transcript_settings")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (key, value) in updates {
                stored.insert(key.clone(), value.clone());
            }
            object.insert("transcript_settings".into(), Value::Object(stored));
        }
        if let Some(value) = &appearance_theme {
            object.insert("theme".into(), value.clone().into());
        }
        if let Some(value) = &appearance_title_mode {
            object.insert("session_title_mode".into(), value.clone().into());
        }
        if !appearance_patch.is_empty() {
            let mut stored = object
                .get("appearance_settings")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (key, value) in &appearance_patch {
                stored.insert(key.clone(), value.clone());
            }
            object.insert("appearance_settings".into(), Value::Object(stored));
        }
        if let Some(value) = menu_attention {
            object.insert("menu_attention_detection".into(), value.into());
        }
        if !experimental_patch.is_empty() {
            let mut stored = object
                .get("experimental_features")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (key, value) in &experimental_patch {
                stored.insert(key.clone(), (*value).into());
            }
            object.insert("experimental_features".into(), Value::Object(stored));
        }
        Ok(())
    });
    if let Err(e) = outcome {
        return (
            500,
            error(&format!(
                "workspace settings effect unknown; refresh Host state: {e}"
            )),
        );
    }
    (200, json!({ "ok": true }))
}

/// One preset mutation, resolved by `preset_patch_response` and applied
/// against the shared `app-state.json` presets array.
enum PresetApply {
    Create {
        id: String,
        label: String,
        command: String,
        quick_launch: bool,
        sort_order: Option<usize>,
    },
    Update {
        id: String,
        command: Option<String>,
        label: Option<String>,
        quick_launch: Option<bool>,
        sort_order: Option<usize>,
    },
    Remove {
        id: String,
    },
}

/// Shared Host semantics for `POST /mobile/presets` (`settings.presets.set`):
/// one-preset patch over the flat preset list in `app-state.json` — create
/// (no `presetID`, `command` required; responds with the minted id), edit
/// `command`/`label`, star (`quickLaunch`), `sortOrder` — move the preset to
/// that index in the advertised display order — and `removed` (delete, not
/// combinable with other fields). Persistence goes through
/// `app_state::edit` (flock + state-bus announce), so both frontends of the
/// edited Host pick the change up live. Every field is type-checked and the
/// preset resolved against the wire catalog before anything applies, matching
/// `project.organization.set`'s validation ordering. Disabled legacy rows and
/// project-scoped rows a Controller never saw keep their file positions
/// across reorders.
pub fn preset_patch_response(body: &Value, wire_presets: &[Value]) -> (u16, Value) {
    let error = |message: &str| json!({ "error": message });
    let preset_id = match body.get("presetID") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                return (400, error("invalid preset id"));
            }
            Some(value.to_owned())
        }
        Some(_) => return (400, error("presetID must be a string")),
    };
    let command = match body.get("command") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                return (400, error("command must be a non-empty string"));
            }
            Some(value.to_owned())
        }
        Some(_) => return (400, error("command must be a string")),
    };
    let label = match body.get("label") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            // Match the organization DTO: a label that trims to empty is a
            // no-op, not an error.
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
        Some(_) => return (400, error("label must be a string")),
    };
    let quick_launch = match body.get("quickLaunch") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return (400, error("quickLaunch must be a boolean")),
    };
    let sort_order = match body.get("sortOrder") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(index) if index >= 0 => Some(index as usize),
            _ => return (400, error("sortOrder must be a non-negative integer")),
        },
    };
    let removed = match body.get("removed") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return (400, error("removed must be a boolean")),
    };

    let visible_ids: Vec<String> = wire_presets
        .iter()
        .filter_map(|preset| preset.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let effect_unknown = |e: &str| {
        (
            500,
            error(&format!(
                "preset update effect unknown; refresh Host state: {e}"
            )),
        )
    };

    let Some(preset_id) = preset_id else {
        if removed {
            return (400, error("removed requires presetID"));
        }
        let Some(command) = command else {
            return (400, error("creating a preset requires a command"));
        };
        let id = uuid::Uuid::new_v4().to_string().to_lowercase();
        let apply = PresetApply::Create {
            id: id.clone(),
            label: label.unwrap_or_else(|| command.clone()),
            command,
            quick_launch: quick_launch.unwrap_or(false),
            sort_order,
        };
        if let Err(e) = apply_preset_patch(&apply) {
            return effect_unknown(&e);
        }
        return (200, json!({ "ok": true, "presetID": id }));
    };

    if !visible_ids.iter().any(|id| id == &preset_id) {
        return (404, error("unknown preset"));
    }
    if removed {
        if command.is_some() || label.is_some() || quick_launch.is_some() || sort_order.is_some() {
            return (400, error("removed cannot be combined with other fields"));
        }
        if let Err(e) = apply_preset_patch(&PresetApply::Remove { id: preset_id }) {
            return effect_unknown(&e);
        }
        return (200, json!({ "ok": true }));
    }
    // A move to the slot the preset already occupies in the advertised order
    // is a successful no-op — no shared write, same rule as sibling reorder.
    let sort_order = sort_order.filter(|index| {
        let mut ordered = visible_ids.clone();
        if let Some(from) = ordered.iter().position(|id| id == &preset_id) {
            let id = ordered.remove(from);
            ordered.insert((*index).min(ordered.len()), id);
        }
        ordered != visible_ids
    });
    if command.is_none() && label.is_none() && quick_launch.is_none() && sort_order.is_none() {
        return (200, json!({ "ok": true }));
    }
    let apply = PresetApply::Update {
        id: preset_id,
        command,
        label,
        quick_launch,
        sort_order,
    };
    if let Err(e) = apply_preset_patch(&apply) {
        return effect_unknown(&e);
    }
    (200, json!({ "ok": true }))
}

fn apply_preset_patch(patch: &PresetApply) -> Result<(), String> {
    crate::app_state::edit(|object| apply_preset_patch_to(object, patch))
}

/// The pure mutation against the raw app-state object map, split out so
/// tests exercise the semantics without touching shared files.
fn apply_preset_patch_to(
    object: &mut serde_json::Map<String, Value>,
    patch: &PresetApply,
) -> Result<(), String> {
    let presets = object
        .entry("presets")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(list) = presets.as_array_mut() else {
        return Err("presets is not an array".into());
    };
    let row_id = |row: &Value| row.get("id").and_then(Value::as_str).map(str::to_owned);
    match patch {
        PresetApply::Create {
            id,
            label,
            command,
            quick_launch,
            sort_order,
        } => {
            list.push(json!({
                "id": id,
                "label": label,
                "command": command,
                "project_id": null,
                "enabled": true,
                "quick_launch": quick_launch,
            }));
            if let Some(index) = sort_order {
                reorder_visible_preset(list, id, *index)?;
            }
            Ok(())
        }
        // Idempotent: a row already gone since bootstrap is a successful
        // delete, not an error.
        PresetApply::Remove { id } => {
            list.retain(|row| row_id(row).as_deref() != Some(id));
            Ok(())
        }
        PresetApply::Update {
            id,
            command,
            label,
            quick_launch,
            sort_order,
        } => {
            {
                let Some(row) = list
                    .iter_mut()
                    .find(|row| row_id(row).as_deref() == Some(id))
                else {
                    return Err("preset vanished since bootstrap".into());
                };
                let row = row
                    .as_object_mut()
                    .ok_or_else(|| "preset row is not an object".to_string())?;
                if let Some(command) = command {
                    row.insert("command".into(), command.clone().into());
                }
                if let Some(label) = label {
                    row.insert("label".into(), label.clone().into());
                }
                if let Some(star) = quick_launch {
                    row.insert("quick_launch".into(), (*star).into());
                }
            }
            if let Some(index) = sort_order {
                reorder_visible_preset(list, id, *index)?;
            }
            Ok(())
        }
    }
}

/// Move one enabled preset to `index` among the enabled rows, leaving
/// disabled legacy rows fixed in their file positions — the preset twin of
/// `session_ops::set_project_sibling_order`'s slot assignment.
fn reorder_visible_preset(list: &mut [Value], id: &str, index: usize) -> Result<(), String> {
    let enabled = |row: &Value| row.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let slots: Vec<usize> = list
        .iter()
        .enumerate()
        .filter(|(_, row)| enabled(row))
        .map(|(slot, _)| slot)
        .collect();
    let mut visible: Vec<Value> = slots.iter().map(|&slot| list[slot].clone()).collect();
    let Some(from) = visible
        .iter()
        .position(|row| row.get("id").and_then(Value::as_str) == Some(id))
    else {
        return Err("preset vanished since bootstrap".into());
    };
    let row = visible.remove(from);
    visible.insert(index.min(visible.len()), row);
    for (slot, row) in slots.into_iter().zip(visible) {
        list[slot] = row;
    }
    Ok(())
}

fn disk_protocol() -> HostProtocolDescriptor {
    let mut protocol = HostProtocolDescriptor::headless_v1();
    protocol.capabilities.retain(|capability| {
        !matches!(
            capability.as_str(),
            "approval.answer"
                | "approval.list"
                | "pairing.create"
                | "pairing.invitation"
                | "session.output.subscribe"
        )
    });
    protocol
}

/// Current HEAD branch of a checkout. Follows a worktree `.git` file to
/// the real gitdir, matching the native Host's `GitHeadReader`.
fn git_head_branch(repo_path: &str) -> Option<String> {
    let git_entry = std::path::Path::new(repo_path).join(".git");
    let meta = std::fs::metadata(&git_entry).ok()?;
    let head_path = if meta.is_dir() {
        git_entry.join("HEAD")
    } else {
        let contents = std::fs::read_to_string(&git_entry).ok()?;
        let gitdir = contents.lines().find_map(|line| {
            line.strip_prefix("gitdir:")
                .map(|rest| rest.trim().to_owned())
        })?;
        let resolved = if std::path::Path::new(&gitdir).is_absolute() {
            std::path::PathBuf::from(gitdir)
        } else {
            std::path::Path::new(repo_path).join(gitdir)
        };
        resolved.join("HEAD")
    };
    let head = std::fs::read_to_string(head_path).ok()?;
    let head = head.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else if !head.is_empty() {
        Some(head.chars().take(7).collect())
    } else {
        None
    }
}

fn project_records(state: &Value) -> Vec<ProjectRecord> {
    let mut records: Vec<ProjectRecord> = state
        .get("projects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let parent_id = string_field(value, &["parent_project_id", "parentProjectID"])
                .filter(|parent| !parent.trim().is_empty())
                .map(str::to_owned);
            let is_folder = bool_field(value, &["is_folder", "isFolder"]).unwrap_or(false);
            let worktree_branch =
                string_field(value, &["worktree_branch", "worktreeBranch"]).map(str::to_owned);
            let has_pin_marker = value.get("pinned_at").and_then(Value::as_u64).is_some()
                || bool_field(value, &["pinned"]).unwrap_or(false);
            Some(ProjectRecord {
                id: string_field(value, &["id"])?.to_owned(),
                name: string_field(value, &["name"])
                    .unwrap_or("Project")
                    .to_owned(),
                path: string_field(value, &["path"])
                    .unwrap_or_default()
                    .to_owned(),
                parent_id: parent_id.clone(),
                sort_order: integer_field(value, &["sort_order", "sortOrder"]).unwrap_or(0),
                is_folder,
                worktree_branch: worktree_branch.clone(),
                pinned: is_folder
                    && parent_id.is_some()
                    && worktree_branch.is_none()
                    && has_pin_marker,
            })
        })
        .collect();

    // Bootstrap project order is Host display order. Stable-partition pinned
    // plain groups within each parent's existing sibling slots; unrelated
    // roots and descendants never cross one another.
    let parents: HashSet<String> = records
        .iter()
        .filter_map(|record| record.parent_id.clone())
        .collect();
    for parent in parents {
        let positions: Vec<usize> = records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                (record.parent_id.as_deref() == Some(parent.as_str())).then_some(index)
            })
            .collect();
        let mut siblings: Vec<ProjectRecord> = positions
            .iter()
            .map(|index| records[*index].clone())
            .collect();
        siblings.sort_by_key(|record| !record.pinned);
        for (index, sibling) in positions.into_iter().zip(siblings) {
            records[index] = sibling;
        }
    }
    records
}

fn presets(state: &Value) -> (Vec<Value>, Vec<HostCreatePreset>) {
    let mut wire = Vec::new();
    let mut create = Vec::new();
    for value in state
        .get("presets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = string_field(value, &["id"]) else {
            continue;
        };
        let Some(command) = string_field(value, &["command"]) else {
            continue;
        };
        let enabled = bool_field(value, &["enabled"]).unwrap_or(true);
        if !enabled {
            continue;
        }
        let label = string_field(value, &["label"]).unwrap_or(command);
        let project_id = string_field(value, &["project_id", "projectID"]).map(str::to_owned);
        wire.push(json!({
            "id": id,
            "label": label,
            "command": command,
            "enabled": true,
            "quickLaunch": bool_field(value, &["quick_launch", "quickLaunch"])
                .unwrap_or(false),
            "isDefault": false,
        }));
        create.push(HostCreatePreset {
            id: id.to_owned(),
            command: command.to_owned(),
            enabled: true,
            project_id,
        });
    }
    (wire, create)
}

fn pinned_session_ids(state: &Value) -> HashSet<String> {
    state
        .get("pinned_sessions")
        .or_else(|| state.get("pinnedSessions"))
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|projects| projects.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|entry| match entry {
            Value::String(id) => Some(id.clone()),
            Value::Object(object) => object
                .get("session_id")
                .or_else(|| object.get("sessionID"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    object
                        .get("key")
                        .and_then(Value::as_str)
                        .and_then(|key| key.strip_prefix("session:"))
                        .map(str::to_owned)
                }),
            _ => None,
        })
        .collect()
}

fn activity_state() -> HashMap<String, Value> {
    std::fs::read(crate::app_paths::activity_state_path())
        .ok()
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .and_then(|value| value.get("sessions").and_then(Value::as_object).cloned())
        .map(|object| object.into_iter().collect())
        .unwrap_or_default()
}

fn effective_project_id(manifest: &HostedSessionManifest, known: &HashSet<String>) -> String {
    crate::session_ops::project_override_marker(&manifest.session.id)
        .filter(|project_id| known.contains(project_id))
        .unwrap_or_else(|| manifest.session.project_id.clone())
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn session_summary(
    manifest: &HostedSessionManifest,
    project_id: &str,
    pinned: bool,
    archived: bool,
    resumable: bool,
    host_owner_principal_id: &str,
    activity: Option<&Value>,
    latest_alert: Option<&crate::activity_log::ActivityLogEntry>,
) -> Value {
    session_summary_with_menu_attention(
        manifest,
        project_id,
        pinned,
        archived,
        resumable,
        host_owner_principal_id,
        activity,
        latest_alert,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn session_summary_with_menu_attention(
    manifest: &HostedSessionManifest,
    project_id: &str,
    pinned: bool,
    archived: bool,
    resumable: bool,
    host_owner_principal_id: &str,
    activity: Option<&Value>,
    latest_alert: Option<&crate::activity_log::ActivityLogEntry>,
    menu_attention_detection: bool,
) -> Value {
    let running = manifest.state == HostedSessionState::Running;
    let updated_at = crate::session_ops::latest_lifecycle_ms(
        &manifest.session.id,
        &manifest.session.command,
        manifest.session.created_at,
        (!running).then_some(manifest.updated_at),
    )
    .max(crate::session_ops::archive_stamp(&manifest.session.id).unwrap_or(0))
    .max(latest_alert.map(|entry| entry.at).unwrap_or(0));
    let claimed_unread = activity
        .and_then(|value| value.get("unread"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let head = crate::integrations::command_head(&manifest.session.command);
    let unread = claimed_unread
        && match crate::session_ops::read_marker(&manifest.session.id) {
            Some(read_at) => {
                let settled_at = crate::session_ops::last_activity_ms(
                    &manifest.session.id,
                    &manifest.session.command,
                );
                let alert_at = latest_alert.map(|entry| entry.at);
                match (settled_at, alert_at) {
                    (Some(settled), Some(alert)) => settled.max(alert) > read_at,
                    (Some(settled), None) => settled > read_at,
                    (None, Some(alert)) => alert > read_at,
                    (None, None) => false,
                }
            }
            None => true,
        };
    let persisted_activity = activity
        .and_then(|value| value.get("activity_status"))
        .or_else(|| activity.and_then(|value| value.get("activityStatus")))
        .and_then(Value::as_str);
    let activity_name = if !running {
        if unread {
            "done"
        } else {
            "idle"
        }
    } else if menu_attention_detection && manifest.menu_prompt_active {
        "blocked"
    } else {
        match persisted_activity {
            Some("starting") => "starting",
            Some("working") => "working",
            Some("blocked") => "blocked",
            _ if unread => "done",
            _ => "idle",
        }
    };
    let active_runtime_id = running
        .then(|| session_host::active_runtime_id(manifest))
        .flatten();
    let resume_agent = running
        && resumable
        && !manifest.runtime_launch_pending
        && manifest.host_protocol_version.unwrap_or(0)
            >= session_host::SESSION_HOST_RESUME_AGENT_PROTOCOL_VERSION
        && crate::resume::can_resume_agent(&manifest.session.command, active_runtime_id);
    let mut value = json!({
        "id": manifest.session.id,
        "projectID": project_id,
        "title": crate::session_ops::title_marker(&manifest.session.id)
            .unwrap_or_else(|| manifest.session.label.clone()),
        "command": manifest.session.command,
        "createdAtUnixMs": manifest.session.created_at,
        "ownerPrincipalID": manifest.session.owner_principal_id
            .as_deref()
            .unwrap_or(host_owner_principal_id),
        "updatedAtUnixMs": updated_at,
        "status": if running { "running" } else { "exited" },
        "activity": activity_name,
        "unread": unread,
        "pinned": pinned,
        "notifyWhenDone": false,
        "runtimeLaunchPending": manifest.runtime_launch_pending,
        "capabilities": {
            // `restart` is the legacy terminal-replacing Resume operation.
            // A live Session offers shell-only Resume Agent after its managed
            // runtime has exited; active/passively observed jobs offer neither.
            // A stopped blank terminal keeps terminal-replacing Resume even
            // though there is no conversation evidence.
            "restart": !running
                && (resumable || manifest.session.command.trim().is_empty()),
            "resumeAgent": resume_agent,
            // Decode-compatible tombstones for older Controllers.
            "fork": false,
            "appendSystemContext": false,
            "notifyWhenDone": false,
            // Evidence-based OFFER (matching the local sidebar and the
            // auto-stop sweep); the archive route itself stays compatible
            // for explicit requests.
            "archive": resumable,
        },
        "archived": archived,
    });
    // A retained final observation is useful in Host diagnostics, but it is
    // never advertised as the currently active runtime after the PTY exits.
    if let Some(runtime_id) = active_runtime_id {
        value["activeRuntimeID"] = runtime_id.into();
    }
    // Installed-App identity travels resolved: a Controller has no compiled
    // catalog entry for a third-party App, so name and tint arrive as data.
    // Additive; older Controllers ignore the keys. Never advertised after
    // the PTY exits, mirroring `activeRuntimeID`.
    if running {
        if let Some(app) = manifest.active_app.as_ref() {
            value["activeAppID"] = app.id.clone().into();
            value["activeAppName"] = app.name.clone().into();
            if let Some(hex) = app
                .tint
                .as_deref()
                .or(app.spinner_tint.as_deref())
                .and_then(|tint| u32::from_str_radix(tint.strip_prefix('#')?, 16).ok())
            {
                value["activeAppTintHex"] = hex.into();
            }
        }
        // A phone currently owns this Session's PTY grid (`resize-desktop`).
        // Additive: a desktop Controller letterboxes its surface to the same
        // grid and offers "fit to desktop"; older Controllers ignore it.
        if let Some(fit) = crate::session_ops::phone_fit_marker(&manifest.session.id) {
            value["phoneFitColumns"] = fit.columns.into();
            value["phoneFitRows"] = fit.rows.into();
            value["phoneFitSinceUnixMs"] = fit.since_unix_ms.into();
        }
    }
    if let Some(alert) = latest_alert {
        if let Some(body) = alert.message.as_deref() {
            value["latestAlertBody"] = body.into();
            value["latestAlertAtUnixMs"] = alert.at.into();
        }
    }
    // Launch working directory, so a Controller can seed its pane's cwd for
    // cmd-clicked relative paths without a project lookup. Additive; older
    // Controllers ignore it. Same field shape as the serve Host's summary.
    if !manifest.cwd.is_empty() {
        value["cwd"] = manifest.cwd.clone().into();
    }
    if let Some(provider) = provider_id(head) {
        value["providerID"] = provider.into();
    }
    if let Some(device_id) = &manifest.session.created_by_device_id {
        value["createdByDeviceID"] = device_id.clone().into();
    }
    if let Some(preset_id) = &manifest.session.source_preset_id {
        value["sourcePresetID"] = preset_id.clone().into();
    }
    value
}

/// Put each Host project's wire sessions in the order its sidebar advertises.
/// Non-archived pins remain their explicit first section. Live rows come next
/// (Recent lifecycle order in date mode, manual order in custom mode), then
/// naturally stopped rows, with archives always last and newest-filed first.
/// The inactive preview later caps both stopped sections together. Grouping
/// the flat wire array by project is harmless to clients and makes its
/// filtered order unambiguous.
fn sort_wire_sessions(
    sessions: &mut [Value],
    projects: &[Value],
    session_orders: &HashMap<String, Vec<String>>,
) {
    let project_rank: HashMap<&str, usize> = projects
        .iter()
        .enumerate()
        .filter_map(|(rank, project)| Some((project.get("id")?.as_str()?, rank)))
        .collect();
    let date_sorted: HashSet<&str> = projects
        .iter()
        .filter(|project| {
            project
                .get("dateSorted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|project| project.get("id")?.as_str())
        .collect();
    fn string<'a>(value: &'a Value, field: &str) -> &'a str {
        value.get(field).and_then(Value::as_str).unwrap_or_default()
    }
    fn number(value: &Value, field: &str) -> u64 {
        value.get(field).and_then(Value::as_u64).unwrap_or(0)
    }
    sessions.sort_by(|left, right| {
        let left_project = string(left, "projectID");
        let right_project = string(right, "projectID");
        project_rank
            .get(left_project)
            .copied()
            .unwrap_or(usize::MAX)
            .cmp(
                &project_rank
                    .get(right_project)
                    .copied()
                    .unwrap_or(usize::MAX),
            )
            .then_with(|| left_project.cmp(right_project))
            .then_with(|| {
                let archived = |value: &Value| {
                    value
                        .get("archived")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                };
                let left_archived = archived(left);
                let right_archived = archived(right);
                if left_archived != right_archived {
                    return left_archived.cmp(&right_archived);
                }
                if left_archived {
                    return number(right, "updatedAtUnixMs")
                        .cmp(&number(left, "updatedAtUnixMs"))
                        .then_with(|| string(left, "id").cmp(string(right, "id")));
                }

                let left_pinned = left.get("pinned").and_then(Value::as_bool).unwrap_or(false);
                let right_pinned = right
                    .get("pinned")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let pinned_order = right_pinned.cmp(&left_pinned);
                if pinned_order != std::cmp::Ordering::Equal {
                    return pinned_order;
                }
                let order = session_orders.get(left_project);
                let manual_key = |value: &Value| match order
                    .and_then(|ids| ids.iter().position(|id| id == string(value, "id")))
                {
                    Some(rank) => (1, rank, std::cmp::Reverse(number(value, "createdAtUnixMs"))),
                    None => (0, 0, std::cmp::Reverse(number(value, "createdAtUnixMs"))),
                };
                if left_pinned {
                    return manual_key(left)
                        .cmp(&manual_key(right))
                        .then_with(|| string(left, "id").cmp(string(right, "id")));
                }

                let running = |value: &Value| string(value, "status") == "running";
                let left_running = running(left);
                let right_running = running(right);
                if left_running != right_running {
                    return right_running.cmp(&left_running);
                }
                if !left_running {
                    return number(right, "updatedAtUnixMs")
                        .cmp(&number(left, "updatedAtUnixMs"))
                        .then_with(|| string(left, "id").cmp(string(right, "id")));
                }
                if !date_sorted.contains(left_project) {
                    return manual_key(left)
                        .cmp(&manual_key(right))
                        .then_with(|| string(left, "id").cmp(string(right, "id")));
                }
                let working =
                    |value: &Value| matches!(string(value, "activity"), "starting" | "working");
                working(right)
                    .cmp(&working(left))
                    .then_with(|| {
                        number(right, "updatedAtUnixMs").cmp(&number(left, "updatedAtUnixMs"))
                    })
                    .then_with(|| string(left, "id").cmp(string(right, "id")))
            })
    });
}

/// Resolve the inactive-preview setting from its compatibility key. Missing
/// or junk values use the five-row default; explicit zero hides every
/// unpinned stopped or archived row.
fn wire_sidebar_inactive_window(state: &Value) -> usize {
    const OPTIONS: [u64; 6] = [0, 3, 5, 10, 15, 25];
    match state.get("sidebar_stopped_limit").and_then(Value::as_u64) {
        Some(limit) if OPTIONS.contains(&limit) => limit as usize,
        _ => 5,
    }
}

/// Keep every live row and non-archived pin, then at most the configured
/// number of stopped and archived previews combined per project. Unread rows
/// stay visible past that window. `sort_wire_sessions` has already put natural
/// stops before the contiguous newest-filed archive section.
fn retain_wire_sidebar_inactive_window(sessions: &mut Vec<Value>, inactive_window: usize) {
    let mut inactive_by_project: HashMap<String, usize> = HashMap::new();
    sessions.retain(|session| {
        let archived = session
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let pinned = session
            .get("pinned")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let running = session.get("status").and_then(Value::as_str) == Some("running");
        let unread = session
            .get("unread")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if (running || pinned) && !archived {
            return true;
        }
        let project = session
            .get("projectID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let count = inactive_by_project.entry(project).or_default();
        let keep = *count < inactive_window || unread;
        *count += 1;
        keep
    });
}

fn provider_id(head: &str) -> Option<&'static str> {
    crate::runtime_catalog::builtin_runtime_catalog()
        .by_command_alias_for_current_platform(head)
        .map(|runtime| runtime.legacy_slug.as_str())
}

fn string_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
}

fn integer_field(value: &Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_u64))
}

fn bool_field(value: &Value, names: &[&str]) -> Option<bool> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_bool))
}

fn hostname_short() -> String {
    let mut buffer = [0u8; 256];
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if rc != 0 {
        return "Host".into();
    }
    let name = buffer.split(|byte| *byte == 0).next().unwrap_or_default();
    String::from_utf8_lossy(name)
        .trim_end_matches(".local")
        .to_owned()
}

fn query_session_id(request: &ControllerRequest) -> Option<&str> {
    request
        .query
        .get("session_id")
        .or_else(|| request.query.get("sessionID"))
        .map(String::as_str)
        .filter(|value| safe_session_id(value))
}

fn body_session_id(request: &ControllerRequest) -> Option<&str> {
    request
        .body
        .get("sessionID")
        .or_else(|| request.body.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| safe_session_id(value))
}

fn safe_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_ID_BYTES
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains("..")
}

fn output(request: &ControllerRequest, cancelled: &AtomicBool) -> (u16, Value) {
    let Some(session_id) = query_session_id(request) else {
        return (400, json!({ "error": "invalid session id" }));
    };
    let offset = request
        .query
        .get("offset")
        .and_then(|value| value.parse::<u64>().ok());
    let limit = request
        .query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(OUTPUT_MAX_BYTES)
        .clamp(1, OUTPUT_MAX_BYTES);
    let wait_ms = request
        .query
        .get("wait_ms")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(OUTPUT_WAIT_MAX_MS);
    if let Some(offset) = offset {
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        while wait_ms > 0
            && !cancelled.load(Ordering::Relaxed)
            && Instant::now() < deadline
            && std::fs::metadata(session_host::output_path(session_id))
                .map(|metadata| metadata.len())
                .unwrap_or(0)
                == offset
        {
            std::thread::sleep(Duration::from_millis(OUTPUT_WAIT_POLL_MS));
        }
    }
    let chunk = match session_host::read_output_chunk(session_id, offset, Some(limit), Some(limit))
    {
        Ok(chunk) => chunk,
        Err(message) => return (500, json!({ "error": message })),
    };
    let start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
    let mut body = json!({
        "sessionID": session_id,
        "offset": start,
        "nextOffset": chunk.next_offset,
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(chunk.data),
        "truncated": offset.map_or(start > 0, |requested| requested != start),
        "capturedAtUnixMs": crate::state::current_timestamp_ms(),
    });
    // A fresh tail is a replay baseline the client resets before feeding:
    // carry the DEC-mode restore preamble (mouse tracking, alt screen, …)
    // so a phone's long-poll fallback and the relay bootstrap learn the
    // modes whose set sequences scrolled out of the retained tail. Not
    // journal bytes; offsets are untouched.
    if offset.is_none() {
        if let Some(preamble) = crate::remote_server::replay_mode_preamble_base64(
            session_host::load_manifest(session_id)
                .and_then(|manifest| manifest.terminal_modes)
                .as_ref(),
            start,
        ) {
            body["modePreambleBase64"] = json!(preamble);
        }
    }
    (200, body)
}

/// A Controller may organize a Session it just created (create receipt →
/// `session.project.set`) before the new host has written its first
/// manifest. When the session directory already exists the spawn is in
/// progress, so wait briefly for the manifest instead of answering 404.
const SPAWNING_MANIFEST_WAIT: Duration = Duration::from_secs(5);

fn wait_for_spawning_manifest(session_id: &str, timeout: Duration) -> bool {
    let session_dir = crate::app_paths::app_sessions_root().join(session_id);
    wait_for_manifest_in(&session_dir, timeout, || {
        session_host::load_manifest(session_id).is_some()
    })
}

fn wait_for_manifest_in(
    session_dir: &std::path::Path,
    timeout: Duration,
    mut manifest_present: impl FnMut() -> bool,
) -> bool {
    if manifest_present() {
        return true;
    }
    if !session_dir.is_dir() {
        return false;
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        if manifest_present() {
            return true;
        }
    }
    false
}

/// `session.project.set` target guard shared by every Host kind (native
/// compatibility routes, `unpeel serve`, and the SSH disk gateway) so a
/// phone or stale Controller can never do what the desktop drag refuses.
///
/// A Session's shell runs in exactly one checkout. Its HOME is the nearest
/// git-worktree project at or above its manifest project, or the root
/// project when it is not inside a worktree. The override marker is display
/// only, so the only legal targets are that home and plain groups directly
/// under it. `projects` accepts both the app-state spelling
/// (`parent_project_id` / `worktree_branch` / `is_folder`) and the wire
/// spelling (`parentProjectID` / `worktreeBranch` / `isGroup`). A manifest
/// project the catalog does not know (legacy/orphan rows) keeps the old
/// rule: any known target.
pub fn validate_session_project_target(
    projects: &[Value],
    manifest_project_id: &str,
    target: &str,
) -> Result<(), String> {
    fn field<'a>(row: &'a Value, keys: &[&str]) -> Option<&'a Value> {
        keys.iter().find_map(|key| row.get(*key))
    }
    fn parent(row: &Value) -> Option<&str> {
        field(row, &["parent_project_id", "parentProjectID"])
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    }
    fn is_worktree(row: &Value) -> bool {
        parent(row).is_some()
            && field(row, &["worktree_branch", "worktreeBranch"])
                .and_then(Value::as_str)
                .is_some_and(|branch| !branch.is_empty())
    }
    fn is_plain_group(row: &Value) -> bool {
        !is_worktree(row)
            && parent(row).is_some()
            && field(row, &["is_folder", "isFolder", "isGroup"])
                .and_then(Value::as_bool)
                .unwrap_or(false)
    }

    let by_id: HashMap<&str, &Value> = projects
        .iter()
        .filter_map(|row| Some((row.get("id")?.as_str()?, row)))
        .collect();
    let Some(target_row) = by_id.get(target) else {
        return Err("unknown target project".into());
    };
    let Some(mut home_row) = by_id.get(manifest_project_id).copied() else {
        return Ok(());
    };
    let mut home = manifest_project_id;
    let mut hops = 0;
    while !is_worktree(home_row) && hops < 16 {
        let Some(parent_id) = parent(home_row) else {
            break;
        };
        let Some(parent_row) = by_id.get(parent_id) else {
            break;
        };
        home = parent_id;
        home_row = parent_row;
        hops += 1;
    }
    if target == home || (is_plain_group(target_row) && parent(target_row) == Some(home)) {
        return Ok(());
    }
    Err(if is_worktree(home_row) {
        "session runs in a git worktree; it can only be filed inside that worktree".into()
    } else {
        "target is outside the session's project".into()
    })
}

fn organization(request: &ControllerRequest) -> (u16, Value) {
    let Some(session_id) = body_session_id(request) else {
        return (400, json!({ "error": "invalid session id" }));
    };
    if !wait_for_spawning_manifest(session_id, SPAWNING_MANIFEST_WAIT) {
        return (404, json!({ "error": "unknown session" }));
    }
    let title = match request.body.get("title") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim();
            (!value.is_empty()).then_some(value)
        }
        Some(_) => return (400, json!({ "error": "title must be a string" })),
    };
    let pinned = match request.body.get("pinned") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return (400, json!({ "error": "pinned must be a boolean" })),
    };
    let archived = match request.body.get("archived") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return (400, json!({ "error": "archived must be a boolean" })),
    };
    // `session.project.set`: file the Session under another project/group via
    // the shared project-override marker (display only — never a manifest
    // edit). The manifest's own project clears the override.
    let project_id = match request.body.get("projectID") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                return (400, json!({ "error": "projectID must not be empty" }));
            }
            Some(value.to_owned())
        }
        Some(_) => return (400, json!({ "error": "projectID must be a string" })),
    };
    if request
        .body
        .get("notifyWhenDone")
        .is_some_and(|value| !value.is_null())
    {
        if !request.body["notifyWhenDone"].is_boolean() {
            return (400, json!({ "error": "notifyWhenDone must be a boolean" }));
        }
        return (
            501,
            json!({ "error": "notifyWhenDone is not supported by this Host" }),
        );
    }
    if let Some(target) = project_id.as_deref() {
        let manifest_project = session_host::load_manifest(session_id)
            .map(|manifest| manifest.session.project_id)
            .unwrap_or_default();
        if target == manifest_project {
            if let Err(message) = crate::session_ops::clear_project_override(session_id) {
                return (500, json!({ "error": message }));
            }
        } else {
            // The target must be a project this Host's shared state knows —
            // a stale override would orphan the row — and it must stay
            // inside the Session's own checkout.
            let projects = crate::app_state::load()
                .ok()
                .and_then(|state| state.get("projects").and_then(Value::as_array).cloned())
                .unwrap_or_default();
            if let Err(message) =
                validate_session_project_target(&projects, &manifest_project, target)
            {
                return (400, json!({ "error": message }));
            }
            if let Err(message) = crate::session_ops::set_project_override(session_id, target) {
                return (500, json!({ "error": message }));
            }
        }
    }
    if let Some(value) = pinned {
        if let Err(message) = crate::session_ops::set_pinned(session_id, value) {
            return (500, json!({ "error": message }));
        }
    }
    if let Some(value) = title {
        if let Err(message) = crate::session_ops::set_title(session_id, value) {
            return (500, json!({ "error": message }));
        }
    }
    let result = match archived {
        Some(true) => crate::session_ops::archive_session(session_id),
        Some(false) => crate::session_ops::restore_session(session_id),
        None => Ok(()),
    };
    match result {
        Ok(()) => (200, json!({ "ok": true })),
        Err(message) => (500, json!({ "error": message })),
    }
}

fn resize_desktop(request: &ControllerRequest) -> (u16, Value) {
    let Some(session_id) = body_session_id(request) else {
        return (400, json!({ "error": "invalid session id" }));
    };
    if request
        .body
        .get("clear")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return (200, json!({ "ok": true }));
    }
    let cols = request
        .body
        .get("columns")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .clamp(2, 300) as u16;
    let rows = request
        .body
        .get("rows")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .clamp(2, 120) as u16;
    match session_host::send_command(session_id, &SessionHostCommand::Resize { cols, rows }) {
        Ok(()) => (200, json!({ "ok": true })),
        Err(_) => (404, json!({ "error": "session host unavailable" })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_runtime(state: HostedSessionState, command: &str) -> HostedSessionManifest {
        HostedSessionManifest {
            session: crate::state::SessionInfo {
                id: "__active_runtime_wire_test__".into(),
                project_id: "project-1".into(),
                label: "Shell".into(),
                custom_title: false,
                command: command.into(),
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
            cwd: "/tmp".into(),
            state,
            pid: None,
            pid_started_at: None,
            host_pid: None,
            host_pid_started_at: None,
            exit_code: None,
            host_build_id: None,
            host_protocol_version: Some(session_host::SESSION_HOST_RESUME_AGENT_PROTOCOL_VERSION),
            has_been_written_to: true,
            provider_session_id: None,
            provider_transcript_path: None,
            managed_storage_path: None,
            resume_failure_markers: Vec::new(),
            runtime: Some(crate::session_host::HostedSessionRuntime {
                current_observation: Some(crate::runtime_observer::ActiveRuntimeObservation {
                    runtime_id: "claude".into(),
                    pid: 42,
                    pid_started_at: Some(1),
                    process_group_id: 42,
                    process_name: "claude".into(),
                    argv: Some(vec!["claude".into()]),
                }),
            }),
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
            heartbeat_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn disk_protocol_does_not_claim_in_process_services() {
        let protocol = disk_protocol();
        assert!(!protocol.supports("approval.answer"));
        assert!(!protocol.supports("approval.list"));
        assert!(!protocol.supports("pairing.create"));
        assert!(!protocol.supports("pairing.invitation"));
        assert!(!protocol.supports("session.output.subscribe"));
        assert!(protocol.supports("host.bootstrap"));
        assert!(protocol.supports("session.create"));
        assert!(protocol.supports("session.output.read"));
    }

    #[test]
    fn session_summary_advertises_live_runtime_without_rewriting_launch_provider() {
        let manifest = manifest_with_runtime(HostedSessionState::Running, "codex");
        let summary = session_summary(
            &manifest,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            None,
        );
        assert_eq!(summary["activeRuntimeID"], "claude");
        assert_eq!(summary["providerID"], "codex");
        assert!(summary["capabilities"].get("restartAgent").is_none());
        assert_eq!(summary["capabilities"]["resumeAgent"], false);
        assert_eq!(summary["capabilities"]["restart"], false);

        let managed = manifest_with_runtime(HostedSessionState::Running, "claude");
        let summary = session_summary(
            &managed,
            "project-1",
            false,
            false,
            true,
            "host-owner:test",
            None,
            None,
        );
        assert!(summary["capabilities"].get("restartAgent").is_none());
        assert_eq!(summary["capabilities"]["resumeAgent"], false);
        assert_eq!(summary["capabilities"]["restart"], false);

        let mut returned_to_shell = managed.clone();
        returned_to_shell.runtime = None;
        let summary = session_summary(
            &returned_to_shell,
            "project-1",
            false,
            false,
            true,
            "host-owner:test",
            None,
            None,
        );
        assert_eq!(summary["capabilities"]["resumeAgent"], true);
        assert_eq!(summary["runtimeLaunchPending"], false);

        let mut launch_pending = returned_to_shell.clone();
        launch_pending.runtime_launch_pending = true;
        let summary = session_summary(
            &launch_pending,
            "project-1",
            false,
            false,
            true,
            "host-owner:test",
            None,
            None,
        );
        assert_eq!(summary["runtimeLaunchPending"], true);
        assert_eq!(summary["capabilities"]["resumeAgent"], false);

        let mut old_host = managed.clone();
        old_host.host_protocol_version =
            Some(session_host::SESSION_HOST_RESTART_AGENT_PROTOCOL_VERSION);
        old_host.runtime = None;
        let summary = session_summary(
            &old_host,
            "project-1",
            false,
            false,
            true,
            "host-owner:test",
            None,
            None,
        );
        assert!(summary["capabilities"].get("restartAgent").is_none());
        assert_eq!(summary["capabilities"]["resumeAgent"], false);
        assert_eq!(summary["capabilities"]["restart"], false);

        let blank = manifest_with_runtime(HostedSessionState::Running, "");
        let summary = session_summary(
            &blank,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            None,
        );
        assert_eq!(summary["activeRuntimeID"], "claude");
        assert!(summary.get("providerID").is_none());
        assert!(summary["capabilities"].get("restartAgent").is_none());
        assert_eq!(summary["capabilities"]["resumeAgent"], false);
        assert_eq!(summary["capabilities"]["restart"], false);
    }

    #[test]
    fn session_summary_advertises_running_app_identity_as_data() {
        let mut manifest = manifest_with_runtime(HostedSessionState::Running, "");
        manifest.active_app = Some(session_host::ObservedAppIdentity {
            id: "unpeel.app.design".into(),
            name: "Unpeel Design".into(),
            tint: Some("#8B5CF6".into()),
            spinner_tint: None,
        });
        let summary = session_summary(
            &manifest,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            None,
        );
        assert_eq!(summary["activeAppID"], "unpeel.app.design");
        assert_eq!(summary["activeAppName"], "Unpeel Design");
        assert_eq!(summary["activeAppTintHex"], 0x8B5CF6);
        // The launch cwd travels additively so a Controller pane can resolve
        // cmd-clicked relative paths against it.
        assert_eq!(summary["cwd"], "/tmp");

        let alert = crate::activity_log::ActivityLogEntry {
            id: "alert-1".into(),
            session_id: manifest.session.id.clone(),
            kind: crate::activity_log::ActivityLogKind::Alert,
            at: 500,
            title: "Usage".into(),
            command: "unpeel-usage".into(),
            project_id: "project-1".into(),
            project_name: "Project".into(),
            message: Some("Close to the weekly limit".into()),
        };
        let alerted = session_summary(
            &manifest,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            Some(&alert),
        );
        assert_eq!(alerted["latestAlertBody"], "Close to the weekly limit");
        assert_eq!(alerted["latestAlertAtUnixMs"], 500);
        assert_eq!(alerted["updatedAtUnixMs"], 500);

        // Like activeRuntimeID, App identity is never advertised for an
        // exited PTY.
        let mut exited = manifest.clone();
        exited.state = HostedSessionState::Exited;
        let summary = session_summary(
            &exited,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            None,
        );
        assert!(summary.get("activeAppID").is_none());
        assert!(summary.get("activeAppTintHex").is_none());
    }

    #[test]
    fn session_summary_does_not_advertise_an_exited_runtime_observation() {
        let manifest = manifest_with_runtime(HostedSessionState::Exited, "");
        let summary = session_summary(
            &manifest,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            None,
        );
        assert!(summary.get("activeRuntimeID").is_none());
        assert!(summary["capabilities"].get("restartAgent").is_none());
        assert_eq!(summary["capabilities"]["resumeAgent"], false);
        // An exited BLANK terminal keeps terminal-replacing Resume even
        // though there is no conversation evidence to offer archive for.
        assert_eq!(summary["capabilities"]["restart"], true);
        assert_eq!(summary["capabilities"]["archive"], false);
    }

    #[test]
    fn session_summary_honors_the_host_menu_attention_switch() {
        let mut manifest = manifest_with_runtime(HostedSessionState::Running, "claude");
        manifest.menu_prompt_active = true;

        let enabled = session_summary(
            &manifest,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            None,
        );
        assert_eq!(enabled["activity"], "blocked");

        let disabled = session_summary_with_menu_attention(
            &manifest,
            "project-1",
            false,
            false,
            false,
            "host-owner:test",
            None,
            None,
            false,
        );
        assert_eq!(disabled["activity"], "idle");
    }

    #[test]
    fn wire_date_sort_uses_working_then_lifecycle_while_custom_stays_manual() {
        let projects = vec![
            json!({ "id": "recent", "dateSorted": true }),
            json!({ "id": "custom" }),
        ];
        let session = |id: &str,
                       project: &str,
                       pinned: bool,
                       status: &str,
                       activity: &str,
                       created: u64,
                       updated: u64| {
            json!({
                "id": id,
                "projectID": project,
                "pinned": pinned,
                "status": status,
                "activity": activity,
                "createdAtUnixMs": created,
                "updatedAtUnixMs": updated,
            })
        };
        let mut sessions = vec![
            session(
                "recent-idle-live",
                "recent",
                false,
                "running",
                "idle",
                1,
                20,
            ),
            session(
                "custom-working-old",
                "custom",
                false,
                "running",
                "working",
                1,
                90,
            ),
            session("recent-exited", "recent", false, "exited", "idle", 1, 90),
            session("recent-z", "recent", false, "exited", "idle", 1, 30),
            session("recent-busy", "recent", false, "running", "working", 1, 10),
            session("custom-new", "custom", false, "running", "idle", 100, 100),
            session("recent-a", "recent", false, "exited", "idle", 1, 30),
            session("recent-pinned", "recent", true, "exited", "idle", 1, 999),
        ];

        let orders = HashMap::from([(
            "custom".to_owned(),
            vec!["custom-working-old".to_owned(), "custom-new".to_owned()],
        )]);
        sort_wire_sessions(&mut sessions, &projects, &orders);
        let ids: Vec<&str> = sessions
            .iter()
            .filter_map(|session| session.get("id")?.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "recent-pinned",
                "recent-busy",
                "recent-idle-live",
                "recent-exited",
                "recent-a",
                "recent-z",
                "custom-working-old",
                "custom-new",
            ]
        );
    }

    #[test]
    fn wire_sidebar_caps_stopped_and_archived_rows_together_per_project() {
        let mut sessions = vec![json!({
            "id": "live",
            "projectID": "project",
            "status": "running",
            "pinned": false,
            "updatedAtUnixMs": 1,
        })];
        for updated in 10..17 {
            sessions.push(json!({
                "id": format!("archived-{updated}"),
                "projectID": "project",
                "status": "exited",
                "archived": true,
                "pinned": false,
                "updatedAtUnixMs": updated,
            }));
        }
        sessions.push(json!({
            "id": "naturally-stopped",
            "projectID": "project",
            "status": "exited",
            "archived": false,
            "pinned": false,
            "capabilities": { "archive": false },
            "updatedAtUnixMs": 3,
        }));
        sessions.push(json!({
            "id": "unread-old",
            "projectID": "project",
            "status": "exited",
            "archived": true,
            "unread": true,
            "pinned": false,
            "updatedAtUnixMs": 2,
        }));
        sort_wire_sessions(
            &mut sessions,
            &[json!({ "id": "project", "dateSorted": true })],
            &HashMap::new(),
        );
        retain_wire_sidebar_inactive_window(&mut sessions, 5);

        assert_eq!(
            sessions
                .iter()
                .filter_map(|session| session.get("id")?.as_str())
                .collect::<Vec<_>>(),
            [
                "live",
                "naturally-stopped",
                "archived-16",
                "archived-15",
                "archived-14",
                "archived-13",
                "unread-old",
            ]
        );

        let mut newest_unread = vec![json!({
            "id": "unread-newest",
            "projectID": "project",
            "status": "exited",
            "archived": true,
            "unread": true,
            "pinned": false,
            "updatedAtUnixMs": 100,
        })];
        for updated in 10..15 {
            newest_unread.push(json!({
                "id": format!("read-{updated}"),
                "projectID": "project",
                "status": "exited",
                "archived": true,
                "unread": false,
                "pinned": false,
                "updatedAtUnixMs": updated,
            }));
        }
        sort_wire_sessions(
            &mut newest_unread,
            &[json!({ "id": "project", "dateSorted": true })],
            &HashMap::new(),
        );
        retain_wire_sidebar_inactive_window(&mut newest_unread, 5);
        assert_eq!(
            newest_unread.len(),
            5,
            "unread archives within the window count toward it"
        );
    }

    #[test]
    fn wire_inactive_preview_defaults_to_five_and_honors_explicit_zero() {
        assert_eq!(wire_sidebar_inactive_window(&json!({})), 5);
        assert_eq!(
            wire_sidebar_inactive_window(&json!({ "sidebar_stopped_limit": 7 })),
            5
        );
        assert_eq!(
            wire_sidebar_inactive_window(&json!({ "sidebar_stopped_limit": 0 })),
            0
        );
        assert_eq!(
            wire_sidebar_inactive_window(&json!({ "sidebar_stopped_limit": 10 })),
            10
        );
    }

    /// Validation and no-op paths only — nothing here may touch shared
    /// files, so the fixture stays safe in a parallel test run. The real
    /// write path is proven end to end by the TUI PTY suite (`remote_host`).
    #[test]
    fn project_organization_validates_before_any_shared_write() {
        let projects = vec![
            json!({ "id": "p1", "name": "One", "path": "/tmp/one", "sortOrder": 0 }),
            json!({
                "id": "g1", "name": "Backlog", "path": "/tmp/one",
                "parentProjectID": "p1", "isGroup": true, "sortOrder": 1
            }),
        ];
        let case = |body: Value| project_organization_response(&body, &projects, None).0;

        assert_eq!(case(json!({ "sortOrder": 0 })), 400, "missing project id");
        assert_eq!(
            case(json!({ "projectID": "nope", "sortOrder": 0 })),
            404,
            "unknown project"
        );
        assert_eq!(
            case(json!({ "projectID": "p1" })),
            200,
            "empty patch is a successful no-op"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "sortOrder": "first" })),
            400,
            "malformed sortOrder"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "sortOrder": -1 })),
            400,
            "negative sortOrder"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "sortOrder": 0 })),
            200,
            "single-sibling move to its own slot is a no-op (no write)"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "folderID": "f1" })),
            501,
            "legacy folder moves are rejected, never silently ignored"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "displayName": "New name" })),
            400,
            "only groups rename remotely"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "pinned": true })),
            400,
            "only groups can be pinned remotely"
        );
        assert_eq!(
            case(json!({ "projectID": "g1", "pinned": "yes" })),
            400,
            "pinned must be a boolean"
        );
        assert_eq!(
            case(json!({ "projectID": "g1", "colorID": "sky" })),
            400,
            "colors are a main-project verb"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "colorID": "plaid" })),
            400,
            "unknown color id"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "colorID": "sky" })),
            501,
            "no color writer means colors are honestly unsupported"
        );
        assert_eq!(
            case(json!({ "projectID": "p1", "displayName": "   " })),
            200,
            "whitespace-only rename trims to a no-op"
        );
    }

    /// Validation and no-op paths only — the empty patch returns before the
    /// shared write, so the fixture stays parallel-safe. The wire read half
    /// is pure and covered against a plain document.
    #[test]
    fn workspace_settings_validate_before_any_shared_write() {
        let case = |body: Value| workspace_settings_response(&body).0;
        assert_eq!(case(json!({})), 200, "empty patch is a successful no-op");
        assert_eq!(
            case(json!({ "autoStopArchiveMinutes": 45 })),
            400,
            "cutoff must be a whitelisted option"
        );
        assert_eq!(
            case(json!({ "sidebarStoppedLimit": 7 })),
            400,
            "sidebar window must be a whitelisted option"
        );
        assert_eq!(
            case(json!({ "browserDefaultAccess": "maybe" })),
            400,
            "access enums reject unknown values"
        );
        assert_eq!(
            case(json!({ "mcpWorktreeAccess": "yes" })),
            400,
            "bool knobs reject strings"
        );
        assert_eq!(
            case(json!({ "computerAccess": "allow", "sidebarStoppedLimit": 7 })),
            400,
            "a compound patch with one bad field applies nothing"
        );
        assert_eq!(
            case(json!({ "appearanceSettings": { "theme": "sepia" } })),
            400,
            "appearance enums reject unknown values"
        );
        assert_eq!(
            case(json!({ "appearanceSettings": { "surfaceOpacity": 1.1 } })),
            400,
            "appearance values stay on the unit interval"
        );
        assert_eq!(
            case(json!({
                "experimentalSettings": { "browserMcp": "yes" }
            })),
            400,
            "experimental knobs reject strings"
        );

        let defaults = wire_workspace_settings(&json!({}));
        assert_eq!(
            defaults["appearanceSettings"]["sessionTitleMode"], "agent",
            "absent session-title mode uses the shipped default"
        );

        let wire = wire_workspace_settings(&json!({
            "auto_stop_archive_minutes": 60,
            "browser_default_access": "off",
            "computer_default_access": "allow",
            "mcp_worktree_access": true,
            "theme": "dark",
            "session_title_mode": "agent",
            "appearance_settings": {
                "app_tint": "teal",
                "background_opacity": 0.55,
                "surface_opacity": 0.75,
                "background_tone": 0.31,
                "surface_tone": 0.27
            },
            "menu_attention_detection": false,
            "experimental_features": {
                "sessions_mcp": false,
                "browser_mcp": false
            }
        }));
        assert_eq!(wire["autoStopArchiveMinutes"], 60);
        assert_eq!(wire["sidebarStoppedLimit"], 5, "absent = default window");
        assert_eq!(wire["browserDefaultAccess"], "off");
        assert_eq!(wire["mcpNonchildWriteAccess"], "ask");
        assert_eq!(wire["computerAccess"], "allow");
        assert_eq!(wire["mcpWorktreeAccess"], true);
        assert_eq!(wire["mcpAutoAddBrowserScreenshots"], true);
        assert_eq!(wire["appearanceSettings"]["theme"], "dark");
        assert_eq!(wire["appearanceSettings"]["appTint"], "teal");
        assert_eq!(wire["appearanceSettings"]["backgroundOpacity"], 0.55);
        assert_eq!(wire["appearanceSettings"]["sessionTitleMode"], "agent");
        assert_eq!(
            wire["notificationSettings"]["menuAttentionDetection"],
            false
        );
        assert_eq!(wire["experimentalSettings"]["worktrees"], true);
        assert_eq!(wire["experimentalSettings"]["sessionsMcp"], false);
        assert_eq!(wire["experimentalSettings"]["browserMcp"], false);
        assert_eq!(wire["experimentalSettings"]["computerUse"], false);
        assert_eq!(wire["experimentalSettings"]["workspaces"], true);
    }

    /// Validation and no-op paths only — nothing here may touch shared
    /// files, so the fixture stays safe in a parallel test run. Mutation
    /// semantics are proven against a plain object map below.
    #[test]
    fn preset_patch_validates_before_any_shared_write() {
        let presets = vec![json!({ "id": "p1", "label": "Claude", "command": "claude",
                    "enabled": true, "quickLaunch": true, "isDefault": true })];
        let case = |body: Value| preset_patch_response(&body, &presets).0;

        assert_eq!(case(json!({})), 400, "creating a preset requires a command");
        assert_eq!(
            case(json!({ "removed": true })),
            400,
            "removed requires presetID"
        );
        assert_eq!(
            case(json!({ "presetID": "nope" })),
            404,
            "unknown preset resolves before the no-op check"
        );
        assert_eq!(
            case(json!({ "presetID": "p1" })),
            200,
            "empty patch is a successful no-op"
        );
        assert_eq!(
            case(json!({ "presetID": "p1", "sortOrder": 0 })),
            200,
            "move to its own slot is a no-op (no write)"
        );
        assert_eq!(
            case(json!({ "presetID": "p1", "command": "   " })),
            400,
            "command must be a non-empty string"
        );
        assert_eq!(
            case(json!({ "presetID": "p1", "command": 3 })),
            400,
            "command must be a string"
        );
        assert_eq!(
            case(json!({ "presetID": "p1", "quickLaunch": "yes" })),
            400,
            "quickLaunch must be a boolean"
        );
        assert_eq!(
            case(json!({ "presetID": "p1", "sortOrder": -1 })),
            400,
            "negative sortOrder"
        );
        assert_eq!(
            case(json!({ "presetID": "p1", "removed": true, "command": "x" })),
            400,
            "removed cannot be combined with other fields"
        );
        assert_eq!(
            case(json!({ "presetID": "p1", "label": "   " })),
            200,
            "whitespace-only label trims to a no-op"
        );
    }

    #[test]
    fn preset_patch_apply_mutates_the_flat_list_and_keeps_hidden_rows_fixed() {
        let state = json!({
            "presets": [
                { "id": "a", "label": "A", "command": "claude", "enabled": true, "quick_launch": false },
                { "id": "legacy", "label": "Old", "command": "old", "enabled": false, "quick_launch": false },
                { "id": "b", "label": "B", "command": "codex", "enabled": true, "quick_launch": true },
            ]
        });
        let mut object = state.as_object().unwrap().clone();

        // Update command/label/star on one row.
        apply_preset_patch_to(
            &mut object,
            &PresetApply::Update {
                id: "a".into(),
                command: Some("claude --continue".into()),
                label: Some("Continue".into()),
                quick_launch: Some(true),
                sort_order: None,
            },
        )
        .unwrap();
        assert_eq!(object["presets"][0]["command"], "claude --continue");
        assert_eq!(object["presets"][0]["label"], "Continue");
        assert_eq!(object["presets"][0]["quick_launch"], true);

        // Reorder: move "a" after "b" among the enabled rows; the disabled
        // legacy row keeps its file slot (index 1).
        apply_preset_patch_to(
            &mut object,
            &PresetApply::Update {
                id: "a".into(),
                command: None,
                label: None,
                quick_launch: None,
                sort_order: Some(1),
            },
        )
        .unwrap();
        let ids: Vec<&str> = object["presets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["b", "legacy", "a"]);

        // Create lands at the requested visible index and mints defaults.
        apply_preset_patch_to(
            &mut object,
            &PresetApply::Create {
                id: "c".into(),
                label: "gemini".into(),
                command: "gemini".into(),
                quick_launch: false,
                sort_order: Some(0),
            },
        )
        .unwrap();
        let ids: Vec<&str> = object["presets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["c", "legacy", "b", "a"]);
        assert_eq!(object["presets"][0]["project_id"], Value::Null);
        assert_eq!(object["presets"][0]["enabled"], true);

        // Remove is idempotent.
        apply_preset_patch_to(&mut object, &PresetApply::Remove { id: "b".into() }).unwrap();
        apply_preset_patch_to(&mut object, &PresetApply::Remove { id: "b".into() }).unwrap();
        assert_eq!(object["presets"].as_array().unwrap().len(), 3);

        // Updating a vanished row is effect-unknown, not silent.
        assert!(apply_preset_patch_to(
            &mut object,
            &PresetApply::Update {
                id: "b".into(),
                command: Some("x".into()),
                label: None,
                quick_launch: None,
                sort_order: None,
            },
        )
        .is_err());
    }

    #[test]
    fn disk_project_records_stably_partition_pinned_plain_groups() {
        let records = project_records(&json!({
            "projects": [
                {"id": "root", "name": "Root", "path": "/root", "pinned_at": 1},
                {"id": "ordinary", "name": "Ordinary", "path": "/root",
                 "parent_project_id": "root", "is_folder": true},
                {"id": "worktree", "name": "Worktree", "path": "/root/w",
                 "parent_project_id": "root", "is_folder": true,
                 "worktree_branch": "feature", "pinned_at": 2},
                {"id": "pinned", "name": "Pinned", "path": "/root",
                 "parent_project_id": "root", "is_folder": true, "pinned_at": 3},
                {"id": "other", "name": "Other", "path": "/other"}
            ]
        }));

        assert_eq!(
            records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            ["root", "pinned", "ordinary", "worktree", "other"]
        );
        assert!(
            !records[0].pinned,
            "top-level projects cannot become pinned groups"
        );
        assert!(records[1].pinned);
        assert!(!records[3].pinned, "worktrees cannot become pinned groups");
    }

    #[test]
    fn session_project_target_stays_inside_the_sessions_checkout() {
        // App-state spelling (native compatibility routes + SSH gateway).
        let projects = vec![
            json!({ "id": "root", "name": "Repo", "path": "/repo" }),
            json!({ "id": "group", "name": "Ideas", "path": "/repo",
                    "parent_project_id": "root", "is_folder": true }),
            json!({ "id": "wt", "name": "feature", "path": "/wt/feature",
                    "parent_project_id": "root", "worktree_branch": "feature" }),
            json!({ "id": "wt-group", "name": "Inside", "path": "/wt/feature",
                    "parent_project_id": "wt", "is_folder": true }),
            json!({ "id": "other", "name": "Other", "path": "/other" }),
        ];
        let ok = |manifest: &str, target: &str| {
            validate_session_project_target(&projects, manifest, target).is_ok()
        };
        // A worktree Session: its worktree and the groups inside it only.
        assert!(ok("wt", "wt"));
        assert!(ok("wt", "wt-group"));
        assert!(ok("wt-group", "wt"));
        assert!(!ok("wt", "root"), "parent project is another checkout");
        assert!(
            !ok("wt", "group"),
            "a group under the parent is another checkout"
        );
        assert!(!ok("wt", "other"));
        assert_eq!(
            validate_session_project_target(&projects, "wt", "root").unwrap_err(),
            "session runs in a git worktree; it can only be filed inside that worktree"
        );
        // A root Session: its root and the root's plain groups, never a
        // worktree or anything under one.
        assert!(ok("root", "group"));
        assert!(ok("group", "root"));
        assert!(!ok("root", "wt"));
        assert!(!ok("root", "wt-group"));
        assert!(!ok("root", "other"));
        assert!(!ok("root", "missing"));
        assert_eq!(
            validate_session_project_target(&projects, "root", "missing").unwrap_err(),
            "unknown target project"
        );
        // A manifest project the catalog no longer knows keeps the old
        // rule: any known target, so a legacy row can still be re-homed.
        assert!(ok("gone", "group"));
        assert!(!ok("gone", "missing"));

        // Wire spelling (`unpeel serve` validates against its bootstrap).
        let wire = vec![
            json!({ "id": "root", "name": "Repo", "path": "/repo" }),
            json!({ "id": "group", "name": "Ideas", "path": "/repo",
                    "parentProjectID": "root", "isGroup": true }),
            json!({ "id": "wt", "name": "feature", "path": "/wt/feature",
                    "parentProjectID": "root", "worktreeBranch": "feature" }),
            json!({ "id": "wt-group", "name": "Inside", "path": "/wt/feature",
                    "parentProjectID": "wt", "isGroup": true }),
        ];
        assert!(validate_session_project_target(&wire, "wt", "wt-group").is_ok());
        assert!(validate_session_project_target(&wire, "wt", "group").is_err());
        assert!(validate_session_project_target(&wire, "root", "group").is_ok());
        assert!(validate_session_project_target(&wire, "root", "wt").is_err());
    }

    #[test]
    fn organization_waits_for_a_spawning_session_manifest() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("never-spawned");
        assert!(
            !wait_for_manifest_in(&missing, Duration::from_millis(200), || false),
            "no session directory means the id is genuinely unknown"
        );

        let spawning = root.path().join("spawning");
        std::fs::create_dir_all(&spawning).unwrap();
        let mut polls = 0;
        assert!(
            wait_for_manifest_in(&spawning, Duration::from_secs(5), || {
                polls += 1;
                polls >= 3
            }),
            "a manifest that lands while waiting is accepted"
        );
        assert_eq!(polls, 3);

        assert!(
            !wait_for_manifest_in(&spawning, Duration::from_millis(150), || false),
            "a spawn that never materializes still times out"
        );
    }

    #[test]
    fn git_head_branch_reads_checkout_worktree_and_detached_head() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(
            git_head_branch(repo.to_str().unwrap()).as_deref(),
            Some("main")
        );

        let worktree = root.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let gitdir = repo.join(".git/worktrees/feature");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        assert_eq!(
            git_head_branch(worktree.to_str().unwrap()).as_deref(),
            Some("feature/x")
        );

        std::fs::write(repo.join(".git/HEAD"), "abcdef1234567890\n").unwrap();
        assert_eq!(
            git_head_branch(repo.to_str().unwrap()).as_deref(),
            Some("abcdef1")
        );

        let empty = root.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(git_head_branch(empty.to_str().unwrap()), None);
    }

    #[test]
    fn wire_principal_is_always_replaced_by_the_host() {
        let runtime = ControllerHostRuntime::owner_transport("ssh", Some("uid:501".into()), None);
        let host_id = crate::relay_uplink::ensure_host_id().expect("host id");
        assert_eq!(
            runtime.principal,
            ControllerPrincipal::OwnerTransport {
                transport: "ssh".into(),
                subject: Some("uid:501".into()),
                principal_id: Some(crate::state::host_owner_principal_id(&host_id)),
            }
        );
    }
}
