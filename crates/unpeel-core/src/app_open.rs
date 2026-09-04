//! Host-owned App resolution and presentation shared by Controllers and MCP.
//!
//! Callers decide how the App was selected and whether user approval is
//! required. This module resolves the installed App and derives the caller's
//! effective project/cwd for both paths. Direct-user paths may create or
//! restart the companion Session; MCP may only attach/reveal an existing one.

use crate::app_presentations::{
    AppPresentationTarget, AppResourceRef, EnsureAppPresentation, EnsureAppPresentationResult,
    DEFAULT_APP_VIEW_ID,
};
use crate::session_host::{self, HostedSessionManifest, HostedSessionState};
use crate::state::{current_timestamp_ms, SessionInfo};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAppRequest {
    pub caller_session_id: String,
    pub app_id: String,
    pub resource: Option<AppResourceRef>,
    pub media_type: Option<String>,
    pub view_id: String,
    pub reveal: bool,
    pub request_id: Option<String>,
}

impl OpenAppRequest {
    pub fn panel(
        caller_session_id: impl Into<String>,
        app_id: impl Into<String>,
        resource: Option<AppResourceRef>,
        media_type: Option<String>,
    ) -> Self {
        Self {
            caller_session_id: caller_session_id.into(),
            app_id: app_id.into(),
            resource,
            media_type,
            view_id: DEFAULT_APP_VIEW_ID.into(),
            reveal: true,
            request_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAppResult {
    pub app_id: String,
    pub app_name: String,
    pub presentation: EnsureAppPresentationResult,
    pub process_state: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenStandaloneAppRequest {
    pub app_id: String,
    pub resource: Option<AppResourceRef>,
    pub media_type: Option<String>,
    pub cwd: String,
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenStandaloneAppResult {
    pub app_id: String,
    pub app_name: String,
    pub session_id: String,
}

fn known_project_ids() -> HashSet<String> {
    crate::app_state::load()
        .ok()
        .and_then(|value| value.get("projects").cloned())
        .and_then(|projects| projects.as_array().cloned())
        .map(|projects| {
            projects
                .into_iter()
                .filter_map(|project| project.get("id")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn effective_project_id(manifest: &HostedSessionManifest) -> String {
    let known_projects = known_project_ids();
    crate::session_ops::project_override_marker(&manifest.session.id)
        .filter(|project_id| known_projects.contains(project_id))
        .unwrap_or_else(|| manifest.session.project_id.clone())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn launch_command(executable: &str, resource: Option<&AppResourceRef>) -> Result<String, String> {
    let executable = executable.trim();
    if executable.is_empty() || executable.contains('\0') {
        return Err("App has no valid launch executable".into());
    }
    let command = shell_quote(executable);
    match resource {
        Some(resource) => {
            if resource.id.contains('\0') {
                return Err("App resource contains a NUL byte".into());
            }
            Ok(format!("{command} {}", shell_quote(&resource.id)))
        }
        _ => Ok(command.to_string()),
    }
}

fn resolve_launch(
    app_id: &str,
    resource: Option<&AppResourceRef>,
    media_type: Option<&str>,
) -> Result<(crate::apps_mcp::InstalledApp, String), String> {
    let app = crate::apps_mcp::installed_apps()
        .into_iter()
        .find(|app| app.id.eq_ignore_ascii_case(app_id))
        .ok_or_else(|| format!("App '{app_id}' is not installed on this Host."))?;
    let resource_kind = resource
        .map(|resource| resource.kind.as_str())
        .unwrap_or("folder");
    if !crate::apps_mcp::installed_app_handles(&app, resource_kind, media_type) {
        return Err(format!(
            "App '{}' does not declare support for {}.",
            app.id,
            crate::apps_mcp::resource_selector(resource_kind, media_type)
        ));
    }
    if resource.is_some_and(|resource| {
        matches!(
            resource.kind.as_str(),
            "file" | "folder" | "git.working-tree"
        ) && !std::path::Path::new(&resource.id).is_absolute()
    }) {
        return Err(format!(
            "{resource_kind} resources require an absolute Host path."
        ));
    }
    let command_name = app
        .command
        .as_deref()
        .ok_or_else(|| format!("App '{}' has no launch command.", app.id))?;
    let executable = app.dir.join(command_name);
    let executable = executable
        .to_str()
        .ok_or_else(|| format!("App '{}' executable path is not UTF-8.", app.id))?;
    let command = launch_command(executable, resource)?;
    Ok((app, command))
}

fn companion_manifest_state(session_id: &str) -> Option<HostedSessionState> {
    session_host::refresh_manifest_health(session_id).map(|manifest| manifest.state)
}

fn ensure_companion_running(
    result: &mut EnsureAppPresentationResult,
    ensure: &EnsureAppPresentation,
    caller: &HostedSessionManifest,
    app_name: &str,
    command: &str,
    hook_port: Option<u16>,
) -> Result<&'static str, String> {
    if companion_manifest_state(&result.instance.companion_session_id)
        == Some(HostedSessionState::Exited)
    {
        crate::session_ops::remove_session(&result.instance.companion_session_id)?;
        *result = crate::app_presentations::ensure_app_presentation(ensure)?;
    }

    let companion_id = result.instance.companion_session_id.clone();
    let _lifecycle_lock = crate::session_ops::lock_session_lifecycle(&companion_id)?;
    if companion_manifest_state(&companion_id) == Some(HostedSessionState::Running) {
        return Ok("running");
    }

    let session = SessionInfo {
        id: companion_id.clone(),
        project_id: ensure.project_id.clone(),
        label: app_name.to_string(),
        custom_title: true,
        command: command.to_string(),
        created_at: current_timestamp_ms(),
        owner_principal_id: caller.session.owner_principal_id.clone(),
        created_by_device_id: None,
        source_preset_id: None,
        tag_id: None,
        worktree_path: caller.session.worktree_path.clone(),
        worktree_branch: caller.session.worktree_branch.clone(),
        parent_session_id: None,
        spawned_by: Some(caller.session.id.clone()),
        role: Some("app-panel".into()),
        task: Some(format!("Companion panel for {app_name}")),
    };
    crate::session_ops::spawn_session(session, &caller.cwd, hook_port, 80, 32)
        .map_err(|error| format!("Failed to start App '{app_name}': {error}"))?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if companion_manifest_state(&companion_id) == Some(HostedSessionState::Running) {
            return Ok("running");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok("starting")
}

pub fn open_app(request: &OpenAppRequest, hook_port: Option<u16>) -> Result<OpenAppResult, String> {
    let caller = session_host::load_manifest(&request.caller_session_id)
        .ok_or_else(|| format!("Unknown caller session id '{}'.", request.caller_session_id))?;
    if caller.state != HostedSessionState::Running {
        return Err(format!(
            "Caller Session '{}' is not running.",
            request.caller_session_id
        ));
    }
    let (app, command) = resolve_launch(
        &request.app_id,
        request.resource.as_ref(),
        request.media_type.as_deref(),
    )?;
    let ensure = EnsureAppPresentation {
        caller_session_id: caller.session.id.clone(),
        project_id: effective_project_id(&caller),
        app_id: app.id.clone(),
        view_id: request.view_id.clone(),
        resource: request.resource.clone(),
        target: AppPresentationTarget::Panel,
        reveal: request.reveal,
        request_id: request.request_id.clone(),
    };
    let mut presentation = crate::app_presentations::ensure_app_presentation(&ensure)?;
    let process_state = ensure_companion_running(
        &mut presentation,
        &ensure,
        &caller,
        &app.name,
        &command,
        hook_port,
    )?;
    Ok(OpenAppResult {
        app_id: app.id,
        app_name: app.name,
        presentation,
        process_state,
    })
}

/// Agent-owned entry: attach/reveal only a companion Session that a user
/// already created through a Controller or the CLI. Unlike `open_app`, this
/// path deliberately has no spawn/remove/restart branch.
pub fn open_existing_app(request: &OpenAppRequest) -> Result<OpenAppResult, String> {
    let caller = session_host::load_manifest(&request.caller_session_id)
        .ok_or_else(|| format!("Unknown caller session id '{}'.", request.caller_session_id))?;
    if caller.state != HostedSessionState::Running {
        return Err(format!(
            "Caller Session '{}' is not running.",
            request.caller_session_id
        ));
    }
    let (app, _) = resolve_launch(
        &request.app_id,
        request.resource.as_ref(),
        request.media_type.as_deref(),
    )?;
    let ensure = EnsureAppPresentation {
        caller_session_id: caller.session.id.clone(),
        project_id: effective_project_id(&caller),
        app_id: app.id.clone(),
        view_id: request.view_id.clone(),
        resource: request.resource.clone(),
        target: AppPresentationTarget::Panel,
        reveal: request.reveal,
        request_id: request.request_id.clone(),
    };
    let presentation = crate::app_presentations::ensure_existing_app_presentation(&ensure)?;
    if companion_manifest_state(&presentation.instance.companion_session_id)
        != Some(HostedSessionState::Running)
    {
        return Err(format!(
            "The existing {} companion is not running. Agents cannot create or restart App Sessions; ask the user to open it first.",
            app.name
        ));
    }
    Ok(OpenAppResult {
        app_id: app.id,
        app_name: app.name,
        presentation,
        process_state: "running",
    })
}

/// User-owned CLI entry when there is no caller pane: resolve the exact same
/// App launch as `open_app`, but make the App itself a top-level hosted
/// Session. MCP never calls this because agent-created Sessions are forbidden.
pub fn open_standalone_app(
    request: &OpenStandaloneAppRequest,
    hook_port: Option<u16>,
) -> Result<OpenStandaloneAppResult, String> {
    if request.cwd.is_empty()
        || request.cwd.contains('\0')
        || !std::path::Path::new(&request.cwd).is_absolute()
    {
        return Err("App Session cwd must be an absolute Host path.".into());
    }
    let (app, command) = resolve_launch(
        &request.app_id,
        request.resource.as_ref(),
        request.media_type.as_deref(),
    )?;
    let session = SessionInfo {
        id: String::new(),
        project_id: request.project_id.clone(),
        label: app.name.clone(),
        custom_title: true,
        command,
        created_at: current_timestamp_ms(),
        owner_principal_id: None,
        created_by_device_id: None,
        source_preset_id: None,
        tag_id: None,
        worktree_path: None,
        worktree_branch: None,
        parent_session_id: None,
        spawned_by: Some("cli".into()),
        role: Some("app".into()),
        task: Some(format!("Open {}", app.name)),
    };
    let session_id = crate::session_ops::spawn_session(session, &request.cwd, hook_port, 120, 32)
        .map_err(|error| format!("Failed to start App '{}': {error}", app.name))?;
    session_host::wait_until_ready(&session_id, Duration::from_secs(10))
        .map_err(|error| format!("App Session {session_id} did not become ready: {error}"))?;
    Ok(OpenStandaloneAppResult {
        app_id: app.id,
        app_name: app.name,
        session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_resources_become_one_shell_safe_argument() {
        let resource = AppResourceRef {
            kind: "file".into(),
            id: "/tmp/a file's notes.md".into(),
        };
        assert_eq!(
            launch_command("/opt/unpeel apps/unpeel-markdown", Some(&resource)).unwrap(),
            "'/opt/unpeel apps/unpeel-markdown' '/tmp/a file'\"'\"'s notes.md'"
        );
    }

    #[test]
    fn typed_resources_are_one_shell_safe_argument() {
        let resource = AppResourceRef {
            kind: "folder".into(),
            id: "/tmp/project".into(),
        };
        assert_eq!(
            launch_command("/opt/unpeel-design", Some(&resource)).unwrap(),
            "'/opt/unpeel-design' '/tmp/project'"
        );
    }
}
