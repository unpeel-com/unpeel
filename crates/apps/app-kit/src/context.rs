//! Typed context detection for standalone and Unpeel-hosted Apps.
//!
//! Hosted details come from the Host rather than from parsing Unpeel's state
//! files. That keeps workspace naming, project/worktree resolution, and
//! principal fallback in the authority that owns them.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::host::HostedSession;

const CONTEXT_VERSION: u32 = 1;
const IO_TIMEOUT: Duration = Duration::from_millis(300);
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Where this App process is running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AppMode {
    #[default]
    Standalone,
    Hosted,
}

/// The current isolated Unpeel workspace instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceContext {
    /// Stable workspace registry id. The implicit workspace uses `default`;
    /// an unregistered development home may not have an id.
    pub id: Option<String>,
    /// Human-readable workspace name supplied by the Host.
    pub name: String,
}

/// The logical project that owns the Session.
///
/// For a worktree Session this is the base project; use
/// [`AppContext::current_root`] for the checkout the process should read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectContext {
    /// Stable project id within the current workspace.
    pub id: String,
    /// Human-readable project name supplied by the Host.
    pub name: String,
    /// Absolute path to the logical base project checkout.
    pub path: PathBuf,
}

/// The active worktree checkout, when the Session is not at its base project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeContext {
    /// Absolute path to the active worktree checkout.
    pub path: PathBuf,
    /// Git branch when the Host can resolve one.
    pub branch: Option<String>,
}

/// The current Session owner as an opaque, Host-scoped principal.
///
/// Apps must not infer an email, account provider, or display name from this
/// value. Those claims require a separate consented identity API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnpeelUser {
    /// Opaque, Host-scoped principal id suitable only for attribution keys.
    pub id: String,
}

/// Environment-neutral context for a Ratatui App.
///
/// `detect()` is infallible and standalone-safe. A valid
/// `UNPEEL_SESSION_ID` always produces `Hosted` mode even if an older or
/// temporarily unavailable Host cannot answer the typed context query; use
/// [`Self::host_available`] to distinguish that state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppContext {
    mode: AppMode,
    session_id: Option<String>,
    host_available: bool,
    workspace: Option<WorkspaceContext>,
    project: Option<ProjectContext>,
    worktree: Option<WorktreeContext>,
    user: Option<UnpeelUser>,
}

impl AppContext {
    /// Detect the current process context without making standalone Apps
    /// depend on Unpeel.
    #[must_use]
    pub fn detect() -> Self {
        let Some(session_id) = std::env::var("UNPEEL_SESSION_ID")
            .ok()
            .filter(|value| valid_id(value))
        else {
            return Self::standalone();
        };

        let mut context = Self {
            mode: AppMode::Hosted,
            session_id: Some(session_id),
            host_available: false,
            workspace: None,
            project: None,
            worktree: None,
            user: None,
        };
        context.refresh();
        context
    }

    fn standalone() -> Self {
        Self {
            mode: AppMode::Standalone,
            session_id: None,
            host_available: false,
            workspace: None,
            project: None,
            worktree: None,
            user: None,
        }
    }

    /// Re-read live Host-owned context. Returns true when any exposed value
    /// or Host availability changed. A transient failure retains the last
    /// good details while reporting `host_available() == false`.
    pub fn refresh(&mut self) -> bool {
        if self.mode != AppMode::Hosted {
            return false;
        }
        let before = self.clone();
        let Some(host) = HostedSession::detect() else {
            self.host_available = false;
            return *self != before;
        };
        let Some(response) = query_hosted_context(&host) else {
            self.host_available = false;
            return *self != before;
        };
        let Some(values) = validate_response(response, host.session_id()) else {
            self.host_available = false;
            return *self != before;
        };
        self.host_available = true;
        self.workspace = values.workspace;
        self.project = values.project;
        self.worktree = values.worktree;
        self.user = values.user;
        *self != before
    }

    #[must_use]
    /// Whether the process is standalone or belongs to a hosted App Session.
    pub const fn mode(&self) -> AppMode {
        self.mode
    }

    #[must_use]
    /// True for a valid hosted App Session, even during a transient Host outage.
    pub const fn is_hosted(&self) -> bool {
        matches!(self.mode, AppMode::Hosted)
    }

    #[must_use]
    /// Whether the most recent typed context query reached and validated the Host.
    pub const fn host_available(&self) -> bool {
        self.host_available
    }

    #[must_use]
    /// Hosted Session id, or `None` in a normal terminal.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    #[must_use]
    /// Current isolated workspace, when supplied by the Host.
    pub fn current_workspace(&self) -> Option<&WorkspaceContext> {
        self.workspace.as_ref()
    }

    #[must_use]
    /// Logical base project, even when the Session runs in a worktree.
    pub fn current_project(&self) -> Option<&ProjectContext> {
        self.project.as_ref()
    }

    #[must_use]
    /// Active worktree checkout, when it differs from the base project.
    pub fn current_worktree(&self) -> Option<&WorktreeContext> {
        self.worktree.as_ref()
    }

    #[must_use]
    /// Opaque current Session owner, when supplied by the Host.
    pub fn current_user(&self) -> Option<&UnpeelUser> {
        self.user.as_ref()
    }

    /// Filesystem root for this Session: the active worktree when present,
    /// otherwise the base project path.
    #[must_use]
    pub fn current_root(&self) -> Option<&Path> {
        self.worktree
            .as_ref()
            .map(|worktree| worktree.path.as_path())
            .or_else(|| self.project.as_ref().map(|project| project.path.as_path()))
    }
}

#[derive(Deserialize)]
struct HostedContextResponse {
    version: u32,
    session_id: String,
    workspace: Option<WorkspaceWire>,
    project: Option<ProjectWire>,
    worktree: Option<WorktreeWire>,
    user: Option<UserWire>,
}

#[derive(Deserialize)]
struct WorkspaceWire {
    id: Option<String>,
    name: String,
}

#[derive(Deserialize)]
struct ProjectWire {
    id: String,
    name: String,
    path: String,
}

#[derive(Deserialize)]
struct WorktreeWire {
    path: String,
    branch: Option<String>,
}

#[derive(Deserialize)]
struct UserWire {
    id: String,
}

struct ValidatedContext {
    workspace: Option<WorkspaceContext>,
    project: Option<ProjectContext>,
    worktree: Option<WorktreeContext>,
    user: Option<UnpeelUser>,
}

fn validate_response(
    response: HostedContextResponse,
    session_id: &str,
) -> Option<ValidatedContext> {
    if response.version != CONTEXT_VERSION || response.session_id != session_id {
        return None;
    }
    let workspace = match response.workspace {
        Some(workspace) => {
            if workspace.id.as_deref().is_some_and(|id| !valid_id(id))
                || !valid_text(&workspace.name, 1024)
            {
                return None;
            }
            Some(WorkspaceContext {
                id: workspace.id,
                name: workspace.name,
            })
        }
        None => None,
    };
    let project = match response.project {
        Some(project) => {
            if !valid_id(&project.id)
                || !valid_text(&project.name, 1024)
                || !valid_absolute_path(&project.path)
            {
                return None;
            }
            Some(ProjectContext {
                id: project.id,
                name: project.name,
                path: PathBuf::from(project.path),
            })
        }
        None => None,
    };
    let worktree = match response.worktree {
        Some(worktree) => {
            if !valid_absolute_path(&worktree.path)
                || worktree
                    .branch
                    .as_deref()
                    .is_some_and(|branch| !valid_text(branch, 1024))
            {
                return None;
            }
            Some(WorktreeContext {
                path: PathBuf::from(worktree.path),
                branch: worktree.branch,
            })
        }
        None => None,
    };
    let user = match response.user {
        Some(user) => {
            if !valid_id(&user.id) {
                return None;
            }
            Some(UnpeelUser { id: user.id })
        }
        None => None,
    };
    Some(ValidatedContext {
        workspace,
        project,
        worktree,
        user,
    })
}

fn valid_id(value: &str) -> bool {
    valid_text(value, 256)
}

fn valid_text(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_absolute_path(value: &str) -> bool {
    valid_text(value, 16_384) && Path::new(value).is_absolute()
}

fn query_hosted_context(host: &HostedSession) -> Option<HostedContextResponse> {
    for port in host.ports() {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let Ok(mut stream) = TcpStream::connect_timeout(&address, IO_TIMEOUT) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let body = b"{}";
        if write!(
            stream,
            "POST /app-context/{} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            host.session_id(),
            body.len()
        )
        .is_err()
            || stream.write_all(body).is_err()
        {
            continue;
        }
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = String::new();
        if stream
            .take(MAX_RESPONSE_BYTES)
            .read_to_string(&mut response)
            .is_err()
        {
            continue;
        }
        if response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            != Some("200")
        {
            continue;
        }
        let Some(body) = response.split_once("\r\n\r\n").map(|(_, body)| body) else {
            continue;
        };
        if let Ok(context) = serde_json::from_str(body) {
            return Some(context);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn response(json: &str) -> HostedContextResponse {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn validates_typed_host_context_and_prefers_worktree_root() {
        let values = validate_response(
            response(
                r#"{
                    "version":1,
                    "session_id":"session-1",
                    "workspace":{"id":"work","name":"Work"},
                    "project":{"id":"project-1","name":"Unpeel","path":"/repo"},
                    "worktree":{"path":"/repo-feature","branch":"feature/context"},
                    "user":{"id":"host-owner:abc"}
                }"#,
            ),
            "session-1",
        )
        .unwrap();
        let context = AppContext {
            mode: AppMode::Hosted,
            session_id: Some("session-1".into()),
            host_available: true,
            workspace: values.workspace,
            project: values.project,
            worktree: values.worktree,
            user: values.user,
        };
        assert_eq!(context.current_workspace().unwrap().name, "Work");
        assert_eq!(
            context.current_project().unwrap().path,
            PathBuf::from("/repo")
        );
        assert_eq!(context.current_root(), Some(Path::new("/repo-feature")));
        assert_eq!(context.current_user().unwrap().id, "host-owner:abc");
    }

    #[test]
    fn rejects_cross_session_unknown_version_and_relative_paths() {
        for json in [
            r#"{"version":2,"session_id":"session-1","workspace":null,"project":null,"worktree":null,"user":null}"#,
            r#"{"version":1,"session_id":"other","workspace":null,"project":null,"worktree":null,"user":null}"#,
            r#"{"version":1,"session_id":"session-1","workspace":null,"project":{"id":"p","name":"P","path":"relative"},"worktree":null,"user":null}"#,
        ] {
            assert!(validate_response(response(json), "session-1").is_none());
        }
    }

    #[test]
    fn hosted_query_uses_the_session_scoped_route() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            assert!(request.starts_with("POST /app-context/session-1 HTTP/1.1\r\n"));
            assert!(request.ends_with("{}"));
            let body = r#"{"version":1,"session_id":"session-1","workspace":null,"project":null,"worktree":null,"user":null}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let host = HostedSession {
            session_id: "session-1".into(),
            session_dir: directory.path().into(),
            app_port: Some(port),
            port_registry: directory.path().join("no-ports"),
        };
        assert_eq!(query_hosted_context(&host).unwrap().version, 1);
        server.join().unwrap();
    }
}
