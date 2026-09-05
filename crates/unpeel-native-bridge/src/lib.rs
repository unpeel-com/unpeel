//! Panic-contained C ABI for the native Swift Host.
//!
//! Only UTF-8 JSON and owned byte buffers cross this boundary. Rust types,
//! allocators, and panics never do. The request/response JSON is exactly the
//! transport-neutral `controller_api` envelope also used by direct, SSH, and
//! Link adapters.

use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use unpeel_core::controller_api::{
    self, ControllerRequest, HostBootstrapContext, HostRouteContext,
};
use unpeel_core::controller_protocol::HostProtocolDescriptor;
use unpeel_core::direct_connection::{DirectHostConnection, DirectHostEndpoint};
use unpeel_core::host_connection::{DeliveryState, HostConnectionError};
use unpeel_core::relay_connection::{
    RelayHostConnection, RelayRequestExecutor, RelayTransportError, RelayTransportReply,
};
use unpeel_core::remote_session_backend::{
    RemoteBootstrapSnapshot, RemoteCreatedSession, RemoteDesktopResize, RemoteEffectFailure,
    RemoteEffectFailureKind, RemoteOutputPage, RemoteOutputPollOptions, RemotePresetPatch,
    RemoteProjectOrganizationPatch, RemoteSessionBackend, RemoteSessionBackendError,
    RemoteSessionCreateRequest, RemoteSessionMetrics, RemoteSessionSummary, RemoteTextSubmitMode,
    RemoteTranscriptMarkdown, RemoteWorkspaceSettingsPatch,
};
use unpeel_core::ssh_connection::{
    install_unpeel_over_ssh, LocalProcessConnection, SshAskpass, SshConnectionOptions,
    SshHostConnection, SshLaunchMode, SshTarget,
};

pub const ABI_VERSION: u32 = 1;
pub const RESULT_OK: i32 = 1;
pub const RESULT_HANDLED: i32 = 1;
pub const RESULT_UNHANDLED: i32 = 0;
pub const ERROR_INVALID_INPUT: i32 = -1;
pub const ERROR_PANIC: i32 = -2;
pub const ERROR_SERIALIZATION: i32 = -3;
pub const ERROR_INVALID_HANDLE: i32 = -4;
pub const ERROR_REMOTE: i32 = -5;

type RemoteHandle = u64;
type RemoteOutputPageHandle = u64;
type PlatformAdapterHandle = u64;

#[derive(Debug)]
struct NativeRemoteError {
    result: i32,
    code: &'static str,
    message: String,
}

impl NativeRemoteError {
    fn invalid_input(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            result: ERROR_INVALID_INPUT,
            code,
            message: message.into(),
        }
    }

    fn invalid_handle(handle: RemoteHandle) -> Self {
        Self {
            result: ERROR_INVALID_HANDLE,
            code: "invalid_remote_handle",
            message: format!(
                "Remote Host handle {handle} is closed or unknown; open the Host again before retrying"
            ),
        }
    }

    fn invalid_output_page_handle(handle: RemoteOutputPageHandle) -> Self {
        Self {
            result: ERROR_INVALID_HANDLE,
            code: "invalid_remote_output_page_handle",
            message: format!(
                "Remote output page handle {handle} is resolved, discarded, or unknown; poll the Session again"
            ),
        }
    }

    fn wrong_output_page_parent(
        page_handle: RemoteOutputPageHandle,
        expected: RemoteHandle,
        received: RemoteHandle,
    ) -> Self {
        Self {
            result: ERROR_INVALID_HANDLE,
            code: "wrong_remote_output_page_parent",
            message: format!(
                "Remote output page {page_handle} belongs to Host handle {expected}, not {received}"
            ),
        }
    }

    fn serialization(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            result: ERROR_SERIALIZATION,
            code,
            message: message.into(),
        }
    }

    fn remote(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            result: ERROR_REMOTE,
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct NativeRemoteEffectError {
    result: i32,
    kind: &'static str,
    code: &'static str,
    operation: &'static str,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeRemoteOutputPageMetadata {
    #[serde(rename = "sessionID")]
    session_id: String,
    requested_offset: Option<u64>,
    offset: u64,
    next_offset: u64,
    reset_before_feed: bool,
    truncated: bool,
    captured_at_unix_ms: i64,
    byte_count: usize,
}

trait RegisteredRemoteOutputPage: Send {
    fn metadata(&self) -> NativeRemoteOutputPageMetadata;
    fn bytes(&self) -> &[u8];
    fn commit(self: Box<Self>) -> Result<(), NativeRemoteError>;
    fn discard(self: Box<Self>);
}

struct RegisteredCoreOutputPage {
    page: RemoteOutputPage,
}

impl RegisteredRemoteOutputPage for RegisteredCoreOutputPage {
    fn metadata(&self) -> NativeRemoteOutputPageMetadata {
        NativeRemoteOutputPageMetadata {
            session_id: self.page.session_id().to_owned(),
            requested_offset: self.page.requested_offset(),
            offset: self.page.offset(),
            next_offset: self.page.next_offset(),
            reset_before_feed: self.page.reset_required(),
            truncated: self.page.truncated(),
            captured_at_unix_ms: self.page.captured_at_unix_ms(),
            byte_count: self.page.bytes().len(),
        }
    }

    fn bytes(&self) -> &[u8] {
        self.page.bytes()
    }

    fn commit(self: Box<Self>) -> Result<(), NativeRemoteError> {
        let Self { page } = *self;
        page.commit()
            .map_err(|error| native_remote_backend_error("output commit", error))
    }

    fn discard(self: Box<Self>) {
        let Self { page } = *self;
        page.discard();
    }
}

/// Registry entries stay behind an integer handle so Swift never owns a Rust
/// pointer or allocator-specific object. The production implementation below
/// is exactly the shared `RemoteSessionBackend`; this narrow trait only lets
/// the C-boundary tests exercise ownership and panic containment without
/// launching a real SSH daemon.
trait RegisteredRemoteBackend: Send + Sync {
    fn bootstrap_snapshot(&self) -> Result<RemoteBootstrapSnapshot, NativeRemoteError>;
    fn poll_output(
        &self,
        session_id: &str,
        options: RemoteOutputPollOptions,
    ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError>;
    fn poll_output_from(
        &self,
        session_id: &str,
        requested_offset: Option<u64>,
        options: RemoteOutputPollOptions,
    ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError>;
    fn reset_output_cursor(&self, session_id: &str) -> Result<(), NativeRemoteError>;
    fn write_terminal(&self, session_id: &str, data: &str) -> Result<u64, NativeRemoteEffectError>;
    fn resize_desktop(
        &self,
        session_id: &str,
        resize: RemoteDesktopResize,
    ) -> Result<u64, NativeRemoteEffectError>;
    fn mark_session_read(&self, session_id: &str) -> Result<u64, NativeRemoteEffectError>;
    fn set_session_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<u64, NativeRemoteEffectError>;
    fn set_session_pinned(
        &self,
        session_id: &str,
        pinned: bool,
    ) -> Result<u64, NativeRemoteEffectError>;
    fn set_session_notify_when_done(
        &self,
        _session_id: &str,
        _enabled: bool,
    ) -> Result<u64, NativeRemoteEffectError> {
        Err(NativeRemoteEffectError {
            result: ERROR_REMOTE,
            kind: "notApplied",
            code: "notify_when_done_unavailable",
            operation: "notify when done",
            message: "Notify when done is unavailable on this backend".into(),
        })
    }
    fn answer_approval(
        &self,
        _approval_id: &str,
        _approved: bool,
    ) -> Result<u64, NativeRemoteEffectError> {
        Err(NativeRemoteEffectError {
            result: ERROR_REMOTE,
            kind: "notApplied",
            code: "approval_answer_unavailable",
            operation: "approval answer",
            message: "Approval answering is unavailable on this backend".into(),
        })
    }
    fn set_session_project(
        &self,
        _session_id: &str,
        _project_id: &str,
    ) -> Result<u64, NativeRemoteEffectError> {
        Err(NativeRemoteEffectError {
            result: ERROR_REMOTE,
            kind: "notApplied",
            code: "session_project_unavailable",
            operation: "session project",
            message: "Session project moves are unavailable on this backend".into(),
        })
    }
    fn session_verb(
        &self,
        verb: RemoteSessionVerb,
        session_id: &str,
    ) -> Result<u64, NativeRemoteEffectError>;
    fn set_session_order(
        &self,
        project_id: &str,
        ordered_session_ids: &[String],
    ) -> Result<u64, NativeRemoteEffectError>;
    fn set_project_organization(
        &self,
        project_id: &str,
        patch: &RemoteProjectOrganizationPatch,
    ) -> Result<u64, NativeRemoteEffectError>;
    fn set_workspace_settings(
        &self,
        _patch: &RemoteWorkspaceSettingsPatch,
    ) -> Result<u64, NativeRemoteEffectError> {
        Err(NativeRemoteEffectError {
            result: ERROR_REMOTE,
            kind: "notApplied",
            code: "workspace_settings_unavailable",
            operation: "workspace settings",
            message: "Workspace settings are unavailable on this backend".into(),
        })
    }
    fn set_preset(&self, _patch: &RemotePresetPatch) -> Result<u64, NativeRemoteEffectError> {
        Err(NativeRemoteEffectError {
            result: ERROR_REMOTE,
            kind: "notApplied",
            code: "preset_edit_unavailable",
            operation: "preset edit",
            message: "Preset editing is unavailable on this backend".into(),
        })
    }
    fn create_session(
        &self,
        request: &RemoteSessionCreateRequest,
    ) -> Result<NativeCreatedSession, NativeRemoteEffectError>;
    fn pairing_invitation(&self, _request_json: &[u8]) -> Result<Vec<u8>, NativeRemoteEffectError> {
        Err(NativeRemoteEffectError {
            result: ERROR_REMOTE,
            kind: "notApplied",
            code: "pairing_invitation_unavailable",
            operation: "pairing invitation",
            message: "Pairing invitations are unavailable on this backend".into(),
        })
    }
    fn upload_attachment(
        &self,
        _session_id: Option<&str>,
        _content_type: &str,
        _bytes: Vec<u8>,
    ) -> Result<String, NativeRemoteEffectError> {
        Err(NativeRemoteEffectError {
            result: ERROR_REMOTE,
            kind: "notApplied",
            code: "upload_unavailable",
            operation: "attachment upload",
            message: "Attachment upload is unavailable on this backend".into(),
        })
    }
    fn list_archived_sessions(
        &self,
        project_id: &str,
    ) -> Result<Vec<RemoteSessionSummary>, NativeRemoteError>;
    fn read_transcript_markdown(
        &self,
        session_id: &str,
        entries: Option<u32>,
    ) -> Result<RemoteTranscriptMarkdown, NativeRemoteError>;
    fn read_session_metrics(
        &self,
        session_id: &str,
    ) -> Result<RemoteSessionMetrics, NativeRemoteError>;
    fn disconnect(&self);
}

/// Session lifecycle verbs that share the `(handle, session_id) → receipt`
/// effect shape. Naming matches the core operation strings so the JSON error
/// envelope is identical whichever layer produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteSessionVerb {
    Archive,
    Restore,
    Stop,
    Remove,
    Restart,
    RestartAgent,
    ResumeAgent,
}

impl RemoteSessionVerb {
    fn operation(self) -> &'static str {
        match self {
            Self::Archive => "session archive",
            Self::Restore => "session restore",
            Self::Stop => "session stop",
            Self::Remove => "session remove",
            Self::Restart => "session restart",
            Self::RestartAgent => "session agent restart",
            Self::ResumeAgent => "session agent resume",
        }
    }
}

/// Bridge-owned view of a Controller-created Session. Mirrors core's
/// `RemoteCreatedSession` without exposing its opaque receipt type through
/// the registry trait, so C-boundary tests can construct one.
struct NativeCreatedSession {
    request_id: u64,
    session_id: String,
    captured_at_unix_ms: Option<i64>,
    session: Option<RemoteSessionSummary>,
}

impl From<RemoteCreatedSession> for NativeCreatedSession {
    fn from(created: RemoteCreatedSession) -> Self {
        Self {
            request_id: created.receipt.request_id(),
            session_id: created.session_id,
            captured_at_unix_ms: created.captured_at_unix_ms,
            session: created.session,
        }
    }
}

enum RegisteredRemoteTransport {
    Ssh { target_uri: String },
    LocalGateway { unpeel_home: String },
    Direct { endpoint_uri: String },
    Link,
}

impl RegisteredRemoteTransport {
    fn target(&self) -> &str {
        match self {
            Self::Ssh { target_uri } => target_uri,
            Self::LocalGateway { unpeel_home } => unpeel_home,
            Self::Direct { endpoint_uri } => endpoint_uri,
            Self::Link => "Unpeel Link",
        }
    }

    fn recovery_hint(&self) -> &'static str {
        match self {
            Self::Ssh { .. } => {
                "Verify non-interactive SSH access and that `unpeel-host` is installed on the Host"
            }
            Self::LocalGateway { .. } => {
                "Verify this workspace's Unpeel data folder is accessible on this Mac"
            }
            Self::Direct { .. } => {
                "Verify the Host is running and this Controller is still on its trusted LAN or VPN"
            }
            Self::Link => {
                "Verify the Host is online, Access away from home is enabled, and this Controller is still paired"
            }
        }
    }
}

struct RegisteredCoreBackend {
    transport: RegisteredRemoteTransport,
    backend: RemoteSessionBackend,
}

impl RegisteredRemoteBackend for RegisteredCoreBackend {
    fn bootstrap_snapshot(&self) -> Result<RemoteBootstrapSnapshot, NativeRemoteError> {
        self.backend
            .bootstrap()
            .map(|bootstrap| bootstrap.snapshot)
            .map_err(|error| remote_bootstrap_error(&self.transport, error))
    }

    fn poll_output(
        &self,
        session_id: &str,
        options: RemoteOutputPollOptions,
    ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError> {
        self.backend
            .poll_output(session_id, options)
            .map(|page| {
                Box::new(RegisteredCoreOutputPage { page }) as Box<dyn RegisteredRemoteOutputPage>
            })
            .map_err(|error| native_remote_backend_error("output poll", error))
    }

    fn poll_output_from(
        &self,
        session_id: &str,
        requested_offset: Option<u64>,
        options: RemoteOutputPollOptions,
    ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError> {
        self.backend
            .poll_output_from(session_id, requested_offset, options)
            .map(|page| {
                Box::new(RegisteredCoreOutputPage { page }) as Box<dyn RegisteredRemoteOutputPage>
            })
            .map_err(|error| native_remote_backend_error("output poll", error))
    }

    fn reset_output_cursor(&self, session_id: &str) -> Result<(), NativeRemoteError> {
        self.backend
            .reset_output_cursor(session_id)
            .map_err(|error| native_remote_backend_error("output cursor reset", error))
    }

    fn write_terminal(&self, session_id: &str, data: &str) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .write_terminal(session_id, data)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn resize_desktop(
        &self,
        session_id: &str,
        resize: RemoteDesktopResize,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .resize_desktop(session_id, resize)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn mark_session_read(&self, session_id: &str) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .mark_session_read(session_id)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn set_session_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_session_title(session_id, title)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn set_session_pinned(
        &self,
        session_id: &str,
        pinned: bool,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_session_pinned(session_id, pinned)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn set_session_notify_when_done(
        &self,
        session_id: &str,
        enabled: bool,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_session_notify_when_done(session_id, enabled)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn answer_approval(
        &self,
        approval_id: &str,
        approved: bool,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .answer_approval(approval_id, approved)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn set_session_project(
        &self,
        session_id: &str,
        project_id: &str,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_session_project(session_id, project_id)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn session_verb(
        &self,
        verb: RemoteSessionVerb,
        session_id: &str,
    ) -> Result<u64, NativeRemoteEffectError> {
        let receipt = match verb {
            RemoteSessionVerb::Archive => self.backend.archive_session(session_id),
            RemoteSessionVerb::Restore => self.backend.restore_session(session_id),
            RemoteSessionVerb::Stop => self.backend.stop_session(session_id),
            RemoteSessionVerb::Remove => self.backend.remove_session(session_id),
            RemoteSessionVerb::Restart => self.backend.restart_session(session_id),
            RemoteSessionVerb::RestartAgent => self.backend.restart_agent(session_id),
            RemoteSessionVerb::ResumeAgent => self.backend.resume_agent(session_id),
        };
        receipt
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn set_session_order(
        &self,
        project_id: &str,
        ordered_session_ids: &[String],
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_session_order(project_id, ordered_session_ids)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn set_project_organization(
        &self,
        project_id: &str,
        patch: &RemoteProjectOrganizationPatch,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_project_organization(project_id, patch)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn set_preset(&self, patch: &RemotePresetPatch) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_preset(patch)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }
    fn set_workspace_settings(
        &self,
        patch: &RemoteWorkspaceSettingsPatch,
    ) -> Result<u64, NativeRemoteEffectError> {
        self.backend
            .set_workspace_settings(patch)
            .map(|receipt| receipt.request_id())
            .map_err(native_remote_effect_error)
    }

    fn create_session(
        &self,
        request: &RemoteSessionCreateRequest,
    ) -> Result<NativeCreatedSession, NativeRemoteEffectError> {
        self.backend
            .create_session(request)
            .map(NativeCreatedSession::from)
            .map_err(native_remote_effect_error)
    }

    fn pairing_invitation(&self, request_json: &[u8]) -> Result<Vec<u8>, NativeRemoteEffectError> {
        self.backend
            .pairing_invitation(request_json)
            .map_err(native_remote_effect_error)
    }

    fn upload_attachment(
        &self,
        session_id: Option<&str>,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<String, NativeRemoteEffectError> {
        self.backend
            .upload_attachment(session_id, content_type, bytes)
            .map(|uploaded| uploaded.path)
            .map_err(native_remote_effect_error)
    }

    fn list_archived_sessions(
        &self,
        project_id: &str,
    ) -> Result<Vec<RemoteSessionSummary>, NativeRemoteError> {
        self.backend
            .list_archived_sessions(project_id)
            .map_err(|error| native_remote_backend_error("archived Sessions read", error))
    }

    fn read_transcript_markdown(
        &self,
        session_id: &str,
        entries: Option<u32>,
    ) -> Result<RemoteTranscriptMarkdown, NativeRemoteError> {
        self.backend
            .read_transcript_markdown(session_id, entries)
            .map_err(|error| native_remote_backend_error("transcript read", error))
    }

    fn read_session_metrics(
        &self,
        session_id: &str,
    ) -> Result<RemoteSessionMetrics, NativeRemoteError> {
        self.backend
            .read_session_metrics(session_id)
            .map_err(|error| native_remote_backend_error("session metrics read", error))
    }

    fn disconnect(&self) {
        self.backend.disconnect();
    }
}

struct RegisteredOutputPageEntry {
    parent: RemoteHandle,
    session_id: String,
    page: Box<dyn RegisteredRemoteOutputPage>,
}

struct RegisteredRemoteBackendEntry {
    backend: Arc<dyn RegisteredRemoteBackend>,
    output_epochs: HashMap<String, u64>,
}

static REMOTE_BACKENDS: OnceLock<Mutex<HashMap<RemoteHandle, RegisteredRemoteBackendEntry>>> =
    OnceLock::new();
static REMOTE_OUTPUT_PAGES: OnceLock<
    Mutex<HashMap<RemoteOutputPageHandle, RegisteredOutputPageEntry>>,
> = OnceLock::new();
static NEXT_REMOTE_HANDLE: AtomicU64 = AtomicU64::new(1);
static NEXT_REMOTE_OUTPUT_PAGE_HANDLE: AtomicU64 = AtomicU64::new(1);
static PLATFORM_ADAPTER_CLIENTS: OnceLock<
    Mutex<HashMap<PlatformAdapterHandle, PlatformAdapterClient>>,
> = OnceLock::new();
static NEXT_PLATFORM_ADAPTER_HANDLE: AtomicU64 = AtomicU64::new(1);

fn remote_backends() -> &'static Mutex<HashMap<RemoteHandle, RegisteredRemoteBackendEntry>> {
    REMOTE_BACKENDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_remote_backends(
) -> std::sync::MutexGuard<'static, HashMap<RemoteHandle, RegisteredRemoteBackendEntry>> {
    remote_backends()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn remote_output_pages(
) -> &'static Mutex<HashMap<RemoteOutputPageHandle, RegisteredOutputPageEntry>> {
    REMOTE_OUTPUT_PAGES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_remote_output_pages(
) -> std::sync::MutexGuard<'static, HashMap<RemoteOutputPageHandle, RegisteredOutputPageEntry>> {
    remote_output_pages()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativePlatformAdapterConfig {
    unpeel_home: String,
    #[serde(rename = "instanceID")]
    instance_id: String,
    callback_port: u16,
    callback_token: String,
    capabilities: Vec<String>,
}

struct PlatformAdapterClient {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl PlatformAdapterClient {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn platform_adapter_clients(
) -> &'static Mutex<HashMap<PlatformAdapterHandle, PlatformAdapterClient>> {
    PLATFORM_ADAPTER_CLIENTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_platform_adapter_clients(
) -> std::sync::MutexGuard<'static, HashMap<PlatformAdapterHandle, PlatformAdapterClient>> {
    platform_adapter_clients()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn next_platform_adapter_handle() -> Result<PlatformAdapterHandle, NativeRemoteError> {
    NEXT_PLATFORM_ADAPTER_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| {
            NativeRemoteError::remote(
                "platform_adapter_handle_space_exhausted",
                "Platform adapter handle space is exhausted; restart Unpeel",
            )
        })
}

fn wait_for_platform_retry(stop: &AtomicBool, duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    while !stop.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn platform_adapter_control_call(
    stream: &mut std::os::unix::net::UnixStream,
    request_id: u64,
    body: serde_json::Value,
) -> Result<(), String> {
    let request = unpeel_core::relay_wire::TunnelRequest {
        id: request_id,
        method: "POST".into(),
        path: "/_unpeel/platform-adapter".into(),
        query: Vec::new(),
        auth: None,
        content_type: Some("application/json".into()),
        body: serde_json::to_vec(&body).map_err(|error| error.to_string())?,
    };
    unpeel_core::remote_stdio::write_frame(
        stream,
        unpeel_core::remote_stdio::FRAME_KIND_REQUEST,
        &unpeel_core::relay_wire::encode_tunnel_request(&request),
    )?;
    let frame = unpeel_core::remote_stdio::read_frame(stream)?
        .ok_or_else(|| "workspace Host closed the adapter connection".to_string())?;
    if frame.kind != unpeel_core::remote_stdio::FRAME_KIND_RESPONSE {
        return Err("workspace Host returned an invalid adapter frame".into());
    }
    let response = unpeel_core::relay_wire::parse_tunnel_response(&frame.payload)?;
    if response.id != request_id || response.status != 200 {
        return Err("workspace Host rejected the platform adapter".into());
    }
    Ok(())
}

#[cfg(unix)]
fn run_platform_adapter_client(config: NativePlatformAdapterConfig, stop: Arc<AtomicBool>) {
    let socket = unpeel_core::remote_stdio::local_host_socket_path(std::path::Path::new(
        &config.unpeel_home,
    ));
    let registration = serde_json::json!({
        "action": "register",
        "registration": {
            "version": 1,
            "instanceID": config.instance_id,
            "callbackPort": config.callback_port,
            "callbackToken": config.callback_token,
            "capabilities": config.capabilities,
        }
    });
    while !stop.load(Ordering::Acquire) {
        let mut stream = match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(stream) => stream,
            Err(_) => {
                wait_for_platform_retry(&stop, Duration::from_millis(500));
                continue;
            }
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        if platform_adapter_control_call(&mut stream, 1, registration.clone()).is_err() {
            wait_for_platform_retry(&stop, Duration::from_millis(500));
            continue;
        }
        let mut request_id = 2u64;
        while !stop.load(Ordering::Acquire) {
            wait_for_platform_retry(&stop, Duration::from_secs(2));
            if stop.load(Ordering::Acquire) {
                break;
            }
            if platform_adapter_control_call(
                &mut stream,
                request_id,
                serde_json::json!({ "action": "status" }),
            )
            .is_err()
            {
                break;
            }
            request_id = request_id.wrapping_add(1).max(2);
        }
    }
}

fn start_platform_adapter_client(
    config: &[u8],
) -> Result<PlatformAdapterHandle, NativeRemoteError> {
    let config: NativePlatformAdapterConfig = serde_json::from_slice(config).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_platform_adapter_config",
            format!("Platform adapter configuration is invalid: {error}"),
        )
    })?;
    let home = std::path::Path::new(&config.unpeel_home);
    if !home.is_absolute() || config.unpeel_home.contains('\0') {
        return Err(NativeRemoteError::invalid_input(
            "invalid_platform_adapter_home",
            "Platform adapter UNPEEL_HOME must be an absolute path",
        ));
    }
    if config.callback_port == 0 || config.capabilities.is_empty() {
        return Err(NativeRemoteError::invalid_input(
            "invalid_platform_adapter_config",
            "Platform adapter callback and capabilities are required",
        ));
    }
    #[cfg(not(unix))]
    {
        let _ = config;
        return Err(NativeRemoteError::invalid_input(
            "unsupported_platform_adapter",
            "Platform adapters require a Unix local Host socket",
        ));
    }
    #[cfg(unix)]
    {
        // Reserve the monotonically increasing handle before spawning so an
        // exhausted handle space cannot orphan a reconnecting worker thread.
        let handle = next_platform_adapter_handle()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = std::thread::Builder::new()
            .name("unpeel-native-platform-adapter".into())
            .spawn(move || run_platform_adapter_client(config, thread_stop))
            .map_err(|error| {
                NativeRemoteError::remote(
                    "platform_adapter_start_failed",
                    format!("Could not start the platform adapter: {error}"),
                )
            })?;
        let replaced = lock_platform_adapter_clients().insert(
            handle,
            PlatformAdapterClient {
                stop,
                worker: Some(worker),
            },
        );
        debug_assert!(replaced.is_none(), "platform adapter handle collision");
        Ok(handle)
    }
}

fn stop_platform_adapter_client(handle: PlatformAdapterHandle) -> Result<(), NativeRemoteError> {
    let client = lock_platform_adapter_clients()
        .remove(&handle)
        .ok_or_else(|| NativeRemoteError {
            result: ERROR_INVALID_HANDLE,
            code: "invalid_platform_adapter_handle",
            message: format!("Platform adapter handle {handle} is closed or unknown"),
        })?;
    client.stop();
    Ok(())
}

fn register_remote_backend(
    backend: Arc<dyn RegisteredRemoteBackend>,
) -> Result<RemoteHandle, NativeRemoteError> {
    loop {
        let handle = NEXT_REMOTE_HANDLE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| {
                NativeRemoteError::remote(
                    "remote_handle_space_exhausted",
                    "Remote Host handle space is exhausted; restart Unpeel",
                )
            })?;
        if handle == 0 {
            continue;
        }
        let mut registry = lock_remote_backends();
        if let std::collections::hash_map::Entry::Vacant(entry) = registry.entry(handle) {
            entry.insert(RegisteredRemoteBackendEntry {
                backend,
                output_epochs: HashMap::new(),
            });
            return Ok(handle);
        }
    }
}

fn remote_backend(
    handle: RemoteHandle,
) -> Result<Arc<dyn RegisteredRemoteBackend>, NativeRemoteError> {
    if handle == 0 {
        return Err(NativeRemoteError::invalid_handle(handle));
    }
    lock_remote_backends()
        .get(&handle)
        .map(|entry| Arc::clone(&entry.backend))
        .ok_or_else(|| NativeRemoteError::invalid_handle(handle))
}

fn remote_backend_with_output_epoch(
    handle: RemoteHandle,
    session_id: &str,
) -> Result<(Arc<dyn RegisteredRemoteBackend>, u64), NativeRemoteError> {
    if handle == 0 {
        return Err(NativeRemoteError::invalid_handle(handle));
    }
    lock_remote_backends()
        .get(&handle)
        .map(|entry| {
            (
                Arc::clone(&entry.backend),
                entry.output_epochs.get(session_id).copied().unwrap_or(0),
            )
        })
        .ok_or_else(|| NativeRemoteError::invalid_handle(handle))
}

fn next_output_page_handle() -> Result<RemoteOutputPageHandle, NativeRemoteError> {
    NEXT_REMOTE_OUTPUT_PAGE_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| {
            NativeRemoteError::remote(
                "remote_output_page_handle_space_exhausted",
                "Remote output page handle space is exhausted; reopen Unpeel",
            )
        })
}

fn register_remote_output_page(
    parent: RemoteHandle,
    expected_backend: &Arc<dyn RegisteredRemoteBackend>,
    expected_output_epoch: u64,
    session_id: String,
    page: Box<dyn RegisteredRemoteOutputPage>,
) -> Result<RemoteOutputPageHandle, NativeRemoteError> {
    let backends = lock_remote_backends();
    let current = backends
        .get(&parent)
        .ok_or_else(|| NativeRemoteError::invalid_handle(parent))?;
    if !Arc::ptr_eq(&current.backend, expected_backend) {
        return Err(NativeRemoteError::invalid_handle(parent));
    }
    if current.output_epochs.get(&session_id).copied().unwrap_or(0) != expected_output_epoch {
        return Err(NativeRemoteError::remote(
            "output_cursor_reset_during_poll",
            "The remote output cursor was reset while this page was loading; poll the Session again",
        ));
    }
    let mut pages = lock_remote_output_pages();
    loop {
        let handle = next_output_page_handle()?;
        if handle == 0 {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = pages.entry(handle) {
            entry.insert(RegisteredOutputPageEntry {
                parent,
                session_id,
                page,
            });
            return Ok(handle);
        }
    }
}

fn take_remote_output_page(
    parent: RemoteHandle,
    page_handle: RemoteOutputPageHandle,
) -> Result<RegisteredOutputPageEntry, NativeRemoteError> {
    if page_handle == 0 {
        return Err(NativeRemoteError::invalid_output_page_handle(page_handle));
    }
    let backends = lock_remote_backends();
    if !backends.contains_key(&parent) {
        return Err(NativeRemoteError::invalid_handle(parent));
    }
    let mut pages = lock_remote_output_pages();
    let entry = pages
        .get(&page_handle)
        .ok_or_else(|| NativeRemoteError::invalid_output_page_handle(page_handle))?;
    if entry.parent != parent {
        return Err(NativeRemoteError::wrong_output_page_parent(
            page_handle,
            entry.parent,
            parent,
        ));
    }
    Ok(pages
        .remove(&page_handle)
        .expect("page verified present while registry lock is held"))
}

fn remove_remote_output_pages(
    parent: RemoteHandle,
    session_id: Option<&str>,
) -> Vec<RegisteredOutputPageEntry> {
    let mut pages = lock_remote_output_pages();
    let handles: Vec<_> = pages
        .iter()
        .filter_map(|(handle, entry)| {
            (entry.parent == parent
                && session_id
                    .map(|session_id| entry.session_id == session_id)
                    .unwrap_or(true))
            .then_some(*handle)
        })
        .collect();
    handles
        .into_iter()
        .filter_map(|handle| pages.remove(&handle))
        .collect()
}

const RELAY_CALLBACK_OK: i32 = 1;
const RELAY_CALLBACK_GENERATION_CHANGED: i32 = 2;
const RELAY_CALLBACK_NOT_SENT: i32 = 3;
const RELAY_CALLBACK_OUTCOME_UNKNOWN: i32 = 4;
const RELAY_CALLBACK_TIMED_OUT_NOT_SENT: i32 = 5;
const RELAY_CALLBACK_TIMED_OUT_OUTCOME_UNKNOWN: i32 = 6;
const MAX_RELAY_CALLBACK_MESSAGE_BYTES: usize = 16 * 1024;

type RelayRequestCallback = unsafe extern "C" fn(
    context: *mut c_void,
    request_pointer: *const u8,
    request_length: usize,
    required_generation: u64,
    timeout_ms: u64,
    out_generation: *mut u64,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32;
type RelayBytesReleaseCallback =
    unsafe extern "C" fn(context: *mut c_void, pointer: *mut u8, length: usize);
type RelayDisconnectCallback = unsafe extern "C" fn(context: *mut c_void);
type RelayContextReleaseCallback = unsafe extern "C" fn(context: *mut c_void);

struct RelayContextOwner {
    context: *mut c_void,
    release: Option<RelayContextReleaseCallback>,
    armed: bool,
}

impl RelayContextOwner {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RelayContextOwner {
    fn drop(&mut self) {
        if self.armed && !self.context.is_null() {
            if let Some(release) = self.release {
                unsafe { release(self.context) };
            }
        }
    }
}

struct CallbackRelayExecutor {
    context: usize,
    request_callback: RelayRequestCallback,
    bytes_release_callback: RelayBytesReleaseCallback,
    disconnect_callback: RelayDisconnectCallback,
    context_release_callback: RelayContextReleaseCallback,
}

impl CallbackRelayExecutor {
    fn context(&self) -> *mut c_void {
        self.context as *mut c_void
    }

    fn take_callback_bytes(&self, pointer: *mut u8, length: usize) -> Vec<u8> {
        if pointer.is_null() || length == 0 {
            if !pointer.is_null() {
                unsafe { (self.bytes_release_callback)(self.context(), pointer, length) };
            }
            return Vec::new();
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec();
        unsafe { (self.bytes_release_callback)(self.context(), pointer, length) };
        bytes
    }
}

// The retained Swift context is an actor-backed, concurrency-safe transport.
// Function pointers are immutable and its lifetime is owned by this wrapper.
unsafe impl Send for CallbackRelayExecutor {}
unsafe impl Sync for CallbackRelayExecutor {}

impl RelayRequestExecutor for CallbackRelayExecutor {
    fn request(
        &self,
        encoded_request: &[u8],
        required_connection_generation: Option<u64>,
        timeout: std::time::Duration,
    ) -> Result<RelayTransportReply, RelayTransportError> {
        let timeout_ms = timeout.as_millis().min(u128::from(u64::MAX)) as u64;
        let mut generation = 0;
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unsafe {
            (self.request_callback)(
                self.context(),
                encoded_request.as_ptr(),
                encoded_request.len(),
                required_connection_generation.unwrap_or(0),
                timeout_ms,
                &mut generation,
                &mut pointer,
                &mut length,
            )
        };
        let bytes = self.take_callback_bytes(pointer, length);
        if code == RELAY_CALLBACK_OK {
            return Ok(RelayTransportReply {
                connection_generation: generation,
                encoded_response: bytes,
            });
        }
        let message = if bytes.len() <= MAX_RELAY_CALLBACK_MESSAGE_BYTES {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            "Link callback returned an oversized error".to_owned()
        };
        match code {
            RELAY_CALLBACK_GENERATION_CHANGED => Err(RelayTransportError::GenerationChanged),
            RELAY_CALLBACK_NOT_SENT => Err(RelayTransportError::Disconnected {
                delivery: DeliveryState::NotSent,
                message,
            }),
            RELAY_CALLBACK_OUTCOME_UNKNOWN => Err(RelayTransportError::Disconnected {
                delivery: DeliveryState::OutcomeUnknown,
                message,
            }),
            RELAY_CALLBACK_TIMED_OUT_NOT_SENT => Err(RelayTransportError::TimedOut {
                delivery: DeliveryState::NotSent,
            }),
            RELAY_CALLBACK_TIMED_OUT_OUTCOME_UNKNOWN => Err(RelayTransportError::TimedOut {
                delivery: DeliveryState::OutcomeUnknown,
            }),
            _ => Err(RelayTransportError::Disconnected {
                delivery: DeliveryState::OutcomeUnknown,
                message: if message.is_empty() {
                    format!("Link callback returned unknown result {code}")
                } else {
                    message
                },
            }),
        }
    }

    fn disconnect(&self) {
        unsafe { (self.disconnect_callback)(self.context()) };
    }
}

impl Drop for CallbackRelayExecutor {
    fn drop(&mut self) {
        unsafe { (self.context_release_callback)(self.context()) };
    }
}

fn open_ssh_remote(target: &[u8]) -> Result<RemoteHandle, NativeRemoteError> {
    let target_uri = std::str::from_utf8(target).map_err(|_| {
        NativeRemoteError::invalid_input(
            "invalid_ssh_target_utf8",
            "SSH Host target must be UTF-8 text such as ssh://studio",
        )
    })?;
    let target = SshTarget::parse(target_uri).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_ssh_target",
            format!("{error}. Use an SSH config alias such as ssh://studio or ssh://user@studio"),
        )
    })?;
    let backend = RemoteSessionBackend::new(Arc::new(SshHostConnection::new(target)));
    register_remote_backend(Arc::new(RegisteredCoreBackend {
        transport: RegisteredRemoteTransport::Ssh {
            target_uri: target_uri.to_owned(),
        },
        backend,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeSshOpenConfig {
    target: String,
    mode: NativeSshLaunchMode,
    askpass_program: Option<String>,
    secret: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum NativeSshLaunchMode {
    Command,
    InteractiveShell,
}

fn parse_native_ssh_config(
    config: &[u8],
) -> Result<(String, SshTarget, SshConnectionOptions), NativeRemoteError> {
    let config: NativeSshOpenConfig = serde_json::from_slice(config).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_ssh_config",
            format!("SSH Host configuration is invalid: {error}"),
        )
    })?;
    let target = SshTarget::parse(&config.target).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_ssh_target",
            format!("{error}. Use an SSH config alias such as ssh://studio or ssh://user@studio"),
        )
    })?;
    let askpass = match (config.askpass_program, config.secret) {
        (None, None) => None,
        (Some(program), Some(secret)) => {
            Some(SshAskpass::new(program, secret).map_err(|error| {
                NativeRemoteError::invalid_input("invalid_ssh_askpass", error.to_string())
            })?)
        }
        _ => {
            return Err(NativeRemoteError::invalid_input(
                "invalid_ssh_askpass",
                "SSH askpass program and secret must be provided together",
            ));
        }
    };
    let launch_mode = match config.mode {
        NativeSshLaunchMode::Command => SshLaunchMode::Command,
        NativeSshLaunchMode::InteractiveShell => SshLaunchMode::InteractiveShell,
    };
    Ok((
        config.target,
        target,
        SshConnectionOptions {
            launch_mode,
            askpass,
        },
    ))
}

fn open_ssh_remote_config(config: &[u8]) -> Result<RemoteHandle, NativeRemoteError> {
    let (target_uri, target, options) = parse_native_ssh_config(config)?;
    let connection = SshHostConnection::with_options(target, options).map_err(|error| {
        NativeRemoteError::invalid_input("invalid_ssh_config", error.to_string())
    })?;
    let backend = RemoteSessionBackend::new(Arc::new(connection));
    register_remote_backend(Arc::new(RegisteredCoreBackend {
        transport: RegisteredRemoteTransport::Ssh { target_uri },
        backend,
    }))
}

fn install_ssh_remote(config: &[u8]) -> Result<Vec<u8>, NativeRemoteError> {
    let (_, target, options) = parse_native_ssh_config(config)?;
    let result = install_unpeel_over_ssh(target, options)
        .map_err(|error| NativeRemoteError::remote("ssh_install_failed", error))?;
    serde_json::to_vec(&serde_json::json!({
        "mode": match result.launch_mode {
            SshLaunchMode::Command => "command",
            SshLaunchMode::InteractiveShell => "interactiveShell",
        }
    }))
    .map_err(|error| {
        NativeRemoteError::serialization(
            "ssh_install_result_serialization_failed",
            format!("Could not encode SSH install result: {error}"),
        )
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeLocalGatewayOpenConfig {
    host_program: String,
    unpeel_home: String,
    #[serde(default)]
    require_host_service: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeLocalHostControlConfig {
    unpeel_home: String,
    request: serde_json::Value,
}

fn local_host_control(config: &[u8]) -> Result<Vec<u8>, NativeRemoteError> {
    if config.len() > 64 * 1024 {
        return Err(NativeRemoteError::invalid_input(
            "invalid_local_host_control_config",
            "Local Host control request is too large",
        ));
    }
    let config: NativeLocalHostControlConfig = serde_json::from_slice(config).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_local_host_control_config",
            format!("Local Host control configuration is invalid: {error}"),
        )
    })?;
    let home = std::path::Path::new(&config.unpeel_home);
    if !home.is_absolute() || config.unpeel_home.contains('\0') || !config.request.is_object() {
        return Err(NativeRemoteError::invalid_input(
            "invalid_local_host_control_config",
            "Local Host control requires an absolute UNPEEL_HOME and an object request",
        ));
    }
    #[cfg(not(unix))]
    {
        let _ = (home, config.request);
        Err(NativeRemoteError::remote(
            "local_host_control_unsupported",
            "Local Host control requires a Unix socket",
        ))
    }
    #[cfg(unix)]
    {
        let socket = unpeel_core::remote_stdio::local_host_socket_path(home);
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).map_err(|error| {
            NativeRemoteError::remote(
                "local_host_control_unavailable",
                format!("Could not connect to {}: {error}", socket.display()),
            )
        })?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(6)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(6)));
        let body = serde_json::to_vec(&config.request).map_err(|error| {
            NativeRemoteError::serialization(
                "local_host_control_serialization_failed",
                error.to_string(),
            )
        })?;
        let request = unpeel_core::relay_wire::TunnelRequest {
            id: 1,
            method: "POST".into(),
            path: "/_unpeel/pairing".into(),
            query: Vec::new(),
            auth: None,
            content_type: Some("application/json".into()),
            body,
        };
        unpeel_core::remote_stdio::write_frame(
            &mut stream,
            unpeel_core::remote_stdio::FRAME_KIND_REQUEST,
            &unpeel_core::relay_wire::encode_tunnel_request(&request),
        )
        .map_err(|error| {
            NativeRemoteError::remote("local_host_control_failed", error.to_string())
        })?;
        let frame = unpeel_core::remote_stdio::read_frame(&mut stream)
            .map_err(|error| {
                NativeRemoteError::remote("local_host_control_failed", error.to_string())
            })?
            .ok_or_else(|| {
                NativeRemoteError::remote(
                    "local_host_control_failed",
                    "Workspace Host closed the control connection",
                )
            })?;
        if frame.kind != unpeel_core::remote_stdio::FRAME_KIND_RESPONSE {
            return Err(NativeRemoteError::remote(
                "local_host_control_failed",
                "Workspace Host returned an invalid control frame",
            ));
        }
        let response =
            unpeel_core::relay_wire::parse_tunnel_response(&frame.payload).map_err(|error| {
                NativeRemoteError::remote("local_host_control_failed", error.to_string())
            })?;
        if response.id != 1 {
            return Err(NativeRemoteError::remote(
                "local_host_control_failed",
                "Workspace Host returned a mismatched control response",
            ));
        }
        if response.status != 200 {
            let message = serde_json::from_slice::<serde_json::Value>(&response.body)
                .ok()
                .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| {
                    format!("Workspace Host rejected the request ({})", response.status)
                });
            return Err(NativeRemoteError::remote(
                "local_host_control_rejected",
                message,
            ));
        }
        Ok(response.body)
    }
}

/// Loopback workspace gateway (workspaces-unification phase 2): the same
/// `RemoteSessionBackend` as SSH over a directly spawned
/// `unpeel-host __remote_stdio__` child scoped to one workspace home. The
/// Controller supplies both absolute paths; nothing is guessed here.
fn open_local_gateway_remote(config: &[u8]) -> Result<RemoteHandle, NativeRemoteError> {
    let config: NativeLocalGatewayOpenConfig = serde_json::from_slice(config).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_local_gateway_config",
            format!("Workspace gateway configuration is invalid: {error}"),
        )
    })?;
    let connection = if config.require_host_service {
        LocalProcessConnection::local_host_service(&config.host_program, &config.unpeel_home)
    } else {
        LocalProcessConnection::local_gateway(&config.host_program, &config.unpeel_home)
    }
    .map_err(|error| {
        NativeRemoteError::invalid_input("invalid_local_gateway_config", error.to_string())
    })?;
    let backend = RemoteSessionBackend::new(Arc::new(connection));
    register_remote_backend(Arc::new(RegisteredCoreBackend {
        transport: RegisteredRemoteTransport::LocalGateway {
            unpeel_home: config.unpeel_home,
        },
        backend,
    }))
}

fn open_direct_remote(endpoint: &[u8], bearer: &[u8]) -> Result<RemoteHandle, NativeRemoteError> {
    let endpoint_uri = std::str::from_utf8(endpoint).map_err(|_| {
        NativeRemoteError::invalid_input(
            "invalid_host_endpoint_utf8",
            "Direct Host endpoint must be UTF-8 text such as http://studio.local:43117/mobile",
        )
    })?;
    let endpoint = DirectHostEndpoint::parse(endpoint_uri).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_host_endpoint",
            format!(
                "Invalid direct Host endpoint: {error}. Use the exact http://…/mobile endpoint returned by pairing"
            ),
        )
    })?;
    let bearer = std::str::from_utf8(bearer).map_err(|_| {
        NativeRemoteError::invalid_input(
            "invalid_host_bearer_utf8",
            "Direct Host bearer must be non-empty UTF-8 text",
        )
    })?;
    if bearer.is_empty() {
        return Err(NativeRemoteError::invalid_input(
            "invalid_host_bearer",
            "Direct Host bearer must not be empty; pair this Controller again",
        ));
    }
    let connection = DirectHostConnection::new(endpoint, bearer).map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_host_bearer",
            format!("Direct Host bearer is malformed: {error}. Pair this Controller again"),
        )
    })?;
    let backend = RemoteSessionBackend::new(Arc::new(connection));
    register_remote_backend(Arc::new(RegisteredCoreBackend {
        transport: RegisteredRemoteTransport::Direct {
            endpoint_uri: endpoint_uri.to_owned(),
        },
        backend,
    }))
}

fn open_link_remote(
    bearer: &[u8],
    executor: Arc<CallbackRelayExecutor>,
) -> Result<RemoteHandle, NativeRemoteError> {
    let bearer = std::str::from_utf8(bearer).map_err(|_| {
        NativeRemoteError::invalid_input(
            "invalid_link_bearer_utf8",
            "Link Host bearer must be non-empty UTF-8 text",
        )
    })?;
    let connection = RelayHostConnection::new(
        bearer,
        Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
    )
    .map_err(|error| {
        NativeRemoteError::invalid_input(
            "invalid_link_bearer",
            format!("Link Host bearer is malformed: {error}. Pair this Controller again"),
        )
    })?;
    let backend = RemoteSessionBackend::new(Arc::new(connection));
    register_remote_backend(Arc::new(RegisteredCoreBackend {
        transport: RegisteredRemoteTransport::Link,
        backend,
    }))
}

fn bootstrap_remote(handle: RemoteHandle) -> Result<Vec<u8>, NativeRemoteError> {
    let snapshot = remote_backend(handle)?.bootstrap_snapshot()?;
    serde_json::to_vec(&snapshot).map_err(|error| {
        NativeRemoteError::serialization(
            "bootstrap_serialization_failed",
            format!("Could not encode the validated remote Host bootstrap: {error}"),
        )
    })
}

struct PolledRemoteOutput {
    page_handle: RemoteOutputPageHandle,
    metadata: Vec<u8>,
    bytes: Vec<u8>,
}

fn poll_remote_output(
    handle: RemoteHandle,
    session_id: &str,
    limit: usize,
    wait_ms: u64,
) -> Result<PolledRemoteOutput, NativeRemoteError> {
    let (backend, output_epoch) = remote_backend_with_output_epoch(handle, session_id)?;
    let page = backend.poll_output(
        session_id,
        RemoteOutputPollOptions {
            limit,
            wait: std::time::Duration::from_millis(wait_ms),
        },
    )?;
    register_polled_remote_output(handle, backend, output_epoch, page)
}

fn poll_remote_output_from(
    handle: RemoteHandle,
    session_id: &str,
    requested_offset: Option<u64>,
    limit: usize,
    wait_ms: u64,
) -> Result<PolledRemoteOutput, NativeRemoteError> {
    let (backend, output_epoch) = remote_backend_with_output_epoch(handle, session_id)?;
    let page = backend.poll_output_from(
        session_id,
        requested_offset,
        RemoteOutputPollOptions {
            limit,
            wait: std::time::Duration::from_millis(wait_ms),
        },
    )?;
    register_polled_remote_output(handle, backend, output_epoch, page)
}

fn register_polled_remote_output(
    handle: RemoteHandle,
    backend: Arc<dyn RegisteredRemoteBackend>,
    output_epoch: u64,
    page: Box<dyn RegisteredRemoteOutputPage>,
) -> Result<PolledRemoteOutput, NativeRemoteError> {
    let metadata_value = page.metadata();
    let bytes = page.bytes().to_vec();
    let metadata = serde_json::to_vec(&metadata_value).map_err(|error| {
        NativeRemoteError::serialization(
            "output_metadata_serialization_failed",
            format!("Could not encode remote output page metadata: {error}"),
        )
    })?;
    let page_handle = register_remote_output_page(
        handle,
        &backend,
        output_epoch,
        metadata_value.session_id,
        page,
    )?;
    Ok(PolledRemoteOutput {
        page_handle,
        metadata,
        bytes,
    })
}

fn commit_remote_output(
    handle: RemoteHandle,
    page_handle: RemoteOutputPageHandle,
) -> Result<(), NativeRemoteError> {
    take_remote_output_page(handle, page_handle)?.page.commit()
}

fn discard_remote_output(
    handle: RemoteHandle,
    page_handle: RemoteOutputPageHandle,
) -> Result<(), NativeRemoteError> {
    take_remote_output_page(handle, page_handle)?.page.discard();
    Ok(())
}

fn reset_remote_output(handle: RemoteHandle, session_id: &str) -> Result<(), NativeRemoteError> {
    let backend = remote_backend(handle)?;
    backend.reset_output_cursor(session_id)?;

    // Keep the backend entry and its page registry linearized against close:
    // a close cannot slip between verifying this exact Arc and draining the
    // pages whose core tokens reset_output_cursor just made stale.
    let mut backends = lock_remote_backends();
    let current = backends
        .get_mut(&handle)
        .ok_or_else(|| NativeRemoteError::invalid_handle(handle))?;
    if !Arc::ptr_eq(&current.backend, &backend) {
        return Err(NativeRemoteError::invalid_handle(handle));
    }
    let output_epoch = current
        .output_epochs
        .entry(session_id.to_owned())
        .or_insert(0);
    *output_epoch = output_epoch.checked_add(1).ok_or_else(|| {
        NativeRemoteError::remote(
            "remote_output_epoch_exhausted",
            "Remote output reset state is exhausted; reopen this Host",
        )
    })?;
    let pages = remove_remote_output_pages(handle, Some(session_id));
    drop(backends);
    for entry in pages {
        entry.page.discard();
    }
    Ok(())
}

fn write_remote_terminal(
    handle: RemoteHandle,
    session_id: &str,
    data: &str,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("terminal write"))?
        .write_terminal(session_id, data)?;
    encode_effect_receipt("terminal write", request_id)
}

fn fit_remote_desktop(
    handle: RemoteHandle,
    session_id: &str,
    columns: u16,
    rows: u16,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("desktop resize"))?
        .resize_desktop(session_id, RemoteDesktopResize::Fit { columns, rows })?;
    encode_effect_receipt("desktop resize", request_id)
}

fn clear_remote_desktop(
    handle: RemoteHandle,
    session_id: &str,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("desktop resize"))?
        .resize_desktop(session_id, RemoteDesktopResize::Clear)?;
    encode_effect_receipt("desktop resize", request_id)
}

fn mark_remote_session_read(
    handle: RemoteHandle,
    session_id: &str,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("mark Session read"))?
        .mark_session_read(session_id)?;
    encode_effect_receipt("mark Session read", request_id)
}

/// Incoming Session-create parameters, decoded from the exact camelCase wire
/// JSON Swift builds. Unknown fields are rejected implicitly by the typed
/// core request; unknown JSON keys are ignored for forward compatibility.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeCreateSessionRequestWire {
    #[serde(rename = "projectID")]
    project_id: String,
    #[serde(rename = "presetID", default)]
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
    initial_text_submit_mode: Option<RemoteTextSubmitMode>,
}

impl From<NativeCreateSessionRequestWire> for RemoteSessionCreateRequest {
    fn from(wire: NativeCreateSessionRequestWire) -> Self {
        Self {
            project_id: wire.project_id,
            preset_id: wire.preset_id,
            command: wire.command,
            worktree_path: wire.worktree_path,
            worktree_branch: wire.worktree_branch,
            initial_text: wire.initial_text,
            initial_text_submit_mode: wire.initial_text_submit_mode.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeCreatedSessionWire<'a> {
    #[serde(rename = "requestID")]
    request_id: u64,
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    captured_at_unix_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<&'a RemoteSessionSummary>,
}

#[derive(Debug, Serialize)]
struct NativeArchivedSessionsWire<'a> {
    #[serde(rename = "projectID")]
    project_id: &'a str,
    sessions: &'a [RemoteSessionSummary],
}

#[derive(Debug, Serialize)]
struct NativeTranscriptMarkdownWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    markdown: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeSessionMetricsWire<'a> {
    #[serde(rename = "sessionID")]
    session_id: &'a str,
    columns: u16,
    rows: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_offset: Option<u64>,
    captured_at_unix_ms: i64,
}

fn encode_remote_read<T: Serialize>(
    operation: &'static str,
    value: &T,
) -> Result<Vec<u8>, NativeRemoteError> {
    serde_json::to_vec(value).map_err(|error| {
        NativeRemoteError::serialization(
            "remote_read_serialization_failed",
            format!("Could not encode the remote {operation} response: {error}"),
        )
    })
}

fn set_remote_session_title(
    handle: RemoteHandle,
    session_id: &str,
    title: &str,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("session title"))?
        .set_session_title(session_id, title)?;
    encode_effect_receipt("session title", request_id)
}

fn set_remote_session_pinned(
    handle: RemoteHandle,
    session_id: &str,
    pinned: bool,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("session pin"))?
        .set_session_pinned(session_id, pinned)?;
    encode_effect_receipt("session pin", request_id)
}

fn set_remote_session_notify_when_done(
    handle: RemoteHandle,
    session_id: &str,
    enabled: bool,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("notify when done"))?
        .set_session_notify_when_done(session_id, enabled)?;
    encode_effect_receipt("notify when done", request_id)
}

fn answer_remote_approval(
    handle: RemoteHandle,
    approval_id: &str,
    approved: bool,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("approval answer"))?
        .answer_approval(approval_id, approved)?;
    encode_effect_receipt("approval answer", request_id)
}

fn set_remote_session_project(
    handle: RemoteHandle,
    session_id: &str,
    project_id: &str,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("session project"))?
        .set_session_project(session_id, project_id)?;
    encode_effect_receipt("session project", request_id)
}

fn perform_remote_session_verb(
    handle: RemoteHandle,
    verb: RemoteSessionVerb,
    session_id: &str,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error(verb.operation()))?
        .session_verb(verb, session_id)?;
    encode_effect_receipt(verb.operation(), request_id)
}

fn set_remote_session_order(
    handle: RemoteHandle,
    project_id: &str,
    ordered_ids_json: &[u8],
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let ordered_session_ids: Vec<String> =
        serde_json::from_slice(ordered_ids_json).map_err(|error| {
            native_not_applied_effect_error("session order")(NativeRemoteError::invalid_input(
                "invalid_session_order_json",
                format!("Session order must be a JSON array of Session ids: {error}"),
            ))
        })?;
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("session order"))?
        .set_session_order(project_id, &ordered_session_ids)?;
    encode_effect_receipt("session order", request_id)
}

/// Decoded `project.organization.set` patch (camelCase wire keys, absent
/// fields left unchanged by the Host).
#[derive(Debug, Deserialize)]
struct NativeProjectOrganizationWire {
    #[serde(rename = "sortOrder")]
    sort_order: Option<i64>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "colorID")]
    color_id: Option<String>,
    #[serde(rename = "dateSorted")]
    date_sorted: Option<bool>,
    pinned: Option<bool>,
}

/// Decoded `settings.presets.set` patch (camelCase wire keys, absent fields
/// left unchanged by the Host).
#[derive(Debug, Deserialize)]
struct NativePresetPatchWire {
    #[serde(rename = "presetID")]
    preset_id: Option<String>,
    command: Option<String>,
    label: Option<String>,
    #[serde(rename = "quickLaunch")]
    quick_launch: Option<bool>,
    #[serde(rename = "sortOrder")]
    sort_order: Option<i64>,
    #[serde(default)]
    removed: bool,
}

fn set_remote_preset(
    handle: RemoteHandle,
    patch_json: &[u8],
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let wire: NativePresetPatchWire = serde_json::from_slice(patch_json).map_err(|error| {
        native_not_applied_effect_error("preset edit")(NativeRemoteError::invalid_input(
            "invalid_preset_patch_json",
            format!("Preset patch is malformed: {error}"),
        ))
    })?;
    let patch = RemotePresetPatch {
        preset_id: wire.preset_id,
        command: wire.command,
        label: wire.label,
        quick_launch: wire.quick_launch,
        sort_order: wire.sort_order,
        removed: wire.removed,
    };
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("preset edit"))?
        .set_preset(&patch)?;
    encode_effect_receipt("preset edit", request_id)
}

/// Decoded `settings.workspace.set` patch (camelCase wire keys, absent
/// fields left unchanged by the Host).
#[derive(Debug, Deserialize)]
struct NativeWorkspaceSettingsWire {
    #[serde(rename = "transcriptSettings")]
    transcript_settings: Option<NativeTranscriptSettingsWire>,
    #[serde(rename = "appearanceSettings")]
    appearance_settings: Option<NativeAppearanceSettingsWire>,
    #[serde(rename = "notificationSettings")]
    notification_settings: Option<NativeNotificationSettingsWire>,
    #[serde(rename = "experimentalSettings")]
    experimental_settings: Option<NativeExperimentalSettingsWire>,
    #[serde(rename = "autoStopArchiveMinutes")]
    auto_stop_archive_minutes: Option<i64>,
    #[serde(rename = "sidebarStoppedLimit")]
    sidebar_stopped_limit: Option<i64>,
    #[serde(rename = "browserDefaultAccess")]
    browser_default_access: Option<String>,
    #[serde(rename = "mcpNonchildWriteAccess")]
    mcp_nonchild_write_access: Option<String>,
    #[serde(rename = "computerAccess")]
    computer_access: Option<String>,
    #[serde(rename = "mcpWorktreeAccess")]
    mcp_worktree_access: Option<bool>,
    #[serde(rename = "mcpAutoAddBrowserScreenshots")]
    mcp_auto_add_browser_screenshots: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct NativeTranscriptSettingsWire {
    #[serde(rename = "includeUser")]
    include_user: Option<bool>,
    #[serde(rename = "includeAssistant")]
    include_assistant: Option<bool>,
    #[serde(rename = "includeReasoning")]
    include_reasoning: Option<bool>,
    #[serde(rename = "includeTools")]
    include_tools: Option<bool>,
    #[serde(rename = "includeFileChanges")]
    include_file_changes: Option<bool>,
    #[serde(rename = "includePlanUpdates")]
    include_plan_updates: Option<bool>,
    #[serde(rename = "includeSessionInfo")]
    include_session_info: Option<bool>,
    #[serde(rename = "maxEntries")]
    max_entries: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct NativeAppearanceSettingsWire {
    theme: Option<String>,
    #[serde(rename = "appTint")]
    app_tint: Option<String>,
    #[serde(rename = "backgroundOpacity")]
    background_opacity: Option<f64>,
    #[serde(rename = "surfaceOpacity")]
    surface_opacity: Option<f64>,
    #[serde(rename = "backgroundTone")]
    background_tone: Option<f64>,
    #[serde(rename = "surfaceTone")]
    surface_tone: Option<f64>,
    #[serde(rename = "sessionTitleMode")]
    session_title_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NativeNotificationSettingsWire {
    #[serde(rename = "menuAttentionDetection")]
    menu_attention_detection: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct NativeExperimentalSettingsWire {
    worktrees: Option<bool>,
    #[serde(rename = "sessionsMcp")]
    sessions_mcp: Option<bool>,
    #[serde(rename = "browserMcp")]
    browser_mcp: Option<bool>,
    #[serde(rename = "computerUse")]
    computer_use: Option<bool>,
    workspaces: Option<bool>,
}

fn set_remote_workspace_settings(
    handle: RemoteHandle,
    patch_json: &[u8],
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let wire: NativeWorkspaceSettingsWire =
        serde_json::from_slice(patch_json).map_err(|error| {
            native_not_applied_effect_error("workspace settings")(NativeRemoteError::invalid_input(
                "invalid_workspace_settings_json",
                format!("Workspace settings patch is malformed: {error}"),
            ))
        })?;
    let patch = RemoteWorkspaceSettingsPatch {
        transcript_settings: wire.transcript_settings.map(|nested| {
            unpeel_core::remote_session_backend::RemoteTranscriptSettingsUpdate {
                include_user: nested.include_user,
                include_assistant: nested.include_assistant,
                include_reasoning: nested.include_reasoning,
                include_tools: nested.include_tools,
                include_file_changes: nested.include_file_changes,
                include_plan_updates: nested.include_plan_updates,
                include_session_info: nested.include_session_info,
                max_entries: nested.max_entries,
            }
        }),
        appearance_settings: wire.appearance_settings.map(|nested| {
            unpeel_core::remote_session_backend::RemoteAppearanceSettingsUpdate {
                theme: nested.theme,
                app_tint: nested.app_tint,
                background_opacity: nested.background_opacity,
                surface_opacity: nested.surface_opacity,
                background_tone: nested.background_tone,
                surface_tone: nested.surface_tone,
                session_title_mode: nested.session_title_mode,
            }
        }),
        notification_settings: wire.notification_settings.map(|nested| {
            unpeel_core::remote_session_backend::RemoteNotificationSettingsUpdate {
                menu_attention_detection: nested.menu_attention_detection,
            }
        }),
        experimental_settings: wire.experimental_settings.map(|nested| {
            unpeel_core::remote_session_backend::RemoteExperimentalSettingsUpdate {
                worktrees: nested.worktrees,
                sessions_mcp: nested.sessions_mcp,
                browser_mcp: nested.browser_mcp,
                computer_use: nested.computer_use,
                workspaces: nested.workspaces,
            }
        }),
        auto_stop_archive_minutes: wire.auto_stop_archive_minutes,
        sidebar_stopped_limit: wire.sidebar_stopped_limit,
        browser_default_access: wire.browser_default_access,
        mcp_nonchild_write_access: wire.mcp_nonchild_write_access,
        computer_access: wire.computer_access,
        mcp_worktree_access: wire.mcp_worktree_access,
        mcp_auto_add_browser_screenshots: wire.mcp_auto_add_browser_screenshots,
    };
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("workspace settings"))?
        .set_workspace_settings(&patch)?;
    encode_effect_receipt("workspace settings", request_id)
}

fn set_remote_project_organization(
    handle: RemoteHandle,
    project_id: &str,
    patch_json: &[u8],
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let wire: NativeProjectOrganizationWire =
        serde_json::from_slice(patch_json).map_err(|error| {
            native_not_applied_effect_error("project organization")(
                NativeRemoteError::invalid_input(
                    "invalid_project_organization_json",
                    format!("Project organization patch is malformed: {error}"),
                ),
            )
        })?;
    let patch = RemoteProjectOrganizationPatch {
        sort_order: wire.sort_order,
        display_name: wire.display_name,
        color_id: wire.color_id,
        date_sorted: wire.date_sorted,
        pinned: wire.pinned,
    };
    let request_id = remote_backend(handle)
        .map_err(native_not_applied_effect_error("project organization"))?
        .set_project_organization(project_id, &patch)?;
    encode_effect_receipt("project organization", request_id)
}

fn create_remote_session(
    handle: RemoteHandle,
    request_json: &[u8],
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    let wire: NativeCreateSessionRequestWire =
        serde_json::from_slice(request_json).map_err(|error| {
            native_not_applied_effect_error("session create")(NativeRemoteError::invalid_input(
                "invalid_session_create_json",
                format!("Session create request is malformed: {error}"),
            ))
        })?;
    let request = RemoteSessionCreateRequest::from(wire);
    let created = remote_backend(handle)
        .map_err(native_not_applied_effect_error("session create"))?
        .create_session(&request)?;
    serde_json::to_vec(&NativeCreatedSessionWire {
        request_id: created.request_id,
        session_id: &created.session_id,
        captured_at_unix_ms: created.captured_at_unix_ms,
        session: created.session.as_ref(),
    })
    .map_err(|error| NativeRemoteEffectError {
        result: ERROR_SERIALIZATION,
        kind: "outcomeUnknown",
        code: "created_session_serialization_failed",
        operation: "session create",
        message: format!("The Session was created, but its receipt could not be encoded: {error}"),
    })
}

fn exchange_remote_pairing_invitation(
    handle: RemoteHandle,
    request_json: &[u8],
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    remote_backend(handle)
        .map_err(native_not_applied_effect_error("pairing invitation"))?
        .pairing_invitation(request_json)
}

fn upload_remote_attachment(
    handle: RemoteHandle,
    session_id: Option<&str>,
    content_type: &str,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    const OPERATION: &str = "attachment upload";
    let path = remote_backend(handle)
        .map_err(native_not_applied_effect_error(OPERATION))?
        .upload_attachment(session_id, content_type, bytes)?;
    serde_json::to_vec(&serde_json::json!({ "path": path })).map_err(|error| {
        native_not_applied_effect_error(OPERATION)(NativeRemoteError::invalid_input(
            "upload_receipt_encode_failed",
            error.to_string(),
        ))
    })
}

fn list_remote_archived_sessions(
    handle: RemoteHandle,
    project_id: &str,
) -> Result<Vec<u8>, NativeRemoteError> {
    let sessions = remote_backend(handle)?.list_archived_sessions(project_id)?;
    encode_remote_read(
        "archived Sessions",
        &NativeArchivedSessionsWire {
            project_id,
            sessions: &sessions,
        },
    )
}

fn read_remote_transcript_markdown(
    handle: RemoteHandle,
    session_id: &str,
    entries: Option<u32>,
) -> Result<Vec<u8>, NativeRemoteError> {
    let transcript = remote_backend(handle)?.read_transcript_markdown(session_id, entries)?;
    encode_remote_read(
        "transcript",
        &NativeTranscriptMarkdownWire {
            session_id: &transcript.session_id,
            markdown: &transcript.markdown,
        },
    )
}

fn read_remote_session_metrics(
    handle: RemoteHandle,
    session_id: &str,
) -> Result<Vec<u8>, NativeRemoteError> {
    let metrics = remote_backend(handle)?.read_session_metrics(session_id)?;
    encode_remote_read(
        "session metrics",
        &NativeSessionMetricsWire {
            session_id: &metrics.session_id,
            columns: metrics.columns,
            rows: metrics.rows,
            output_offset: metrics.output_offset,
            captured_at_unix_ms: metrics.captured_at_unix_ms,
        },
    )
}

fn close_remote(handle: RemoteHandle) -> Result<(), NativeRemoteError> {
    // Remote and page registry locks always use this order. Removing both
    // while they are held prevents an in-flight poll from publishing an
    // orphan page after close. Cleanup itself runs after the locks are gone.
    let mut backends = lock_remote_backends();
    let backend = backends
        .remove(&handle)
        .ok_or_else(|| NativeRemoteError::invalid_handle(handle))?
        .backend;
    let pages = remove_remote_output_pages(handle, None);
    drop(backends);

    let mut first_panic = None;
    for entry in pages {
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| entry.page.discard())) {
            if first_panic.is_none() {
                first_panic = Some(payload);
            }
        }
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| backend.disconnect())) {
        if first_panic.is_none() {
            first_panic = Some(payload);
        }
    }
    if let Some(payload) = first_panic {
        std::panic::resume_unwind(payload);
    }
    Ok(())
}

fn remote_connection_error_code(error: &HostConnectionError) -> &'static str {
    match error {
        HostConnectionError::InvalidTarget(_) => "invalid_host_target",
        HostConnectionError::Configuration(_) => "host_connection_configuration",
        HostConnectionError::Closed | HostConnectionError::ClosedRequest(_) => {
            "host_connection_closed"
        }
        HostConnectionError::RequestIdExhausted => "host_request_id_exhausted",
        HostConnectionError::WrongConnection(_) => "wrong_host_connection",
        HostConnectionError::WrongGeneration(_) | HostConnectionError::GenerationChanged { .. } => {
            "host_generation_changed"
        }
        HostConnectionError::RequestTooLarge { .. } => "host_request_too_large",
        HostConnectionError::TooManyInFlight { .. } => "host_too_many_requests",
        HostConnectionError::DuplicateRequestId(_) => "duplicate_host_request",
        HostConnectionError::Launch { .. } => "host_connection_launch_failed",
        HostConnectionError::Disconnected { .. } => "host_connection_disconnected",
        HostConnectionError::TimedOut { .. } => "host_connection_timed_out",
        _ => "host_connection_failed",
    }
}

fn remote_backend_error_code(error: &RemoteSessionBackendError) -> &'static str {
    match error {
        RemoteSessionBackendError::Connection(error) => remote_connection_error_code(error),
        RemoteSessionBackendError::HostStatus { .. } => "host_operation_rejected",
        RemoteSessionBackendError::InvalidResponse { .. } => "invalid_host_response",
        RemoteSessionBackendError::UnsupportedMobileProtocol { .. }
        | RemoteSessionBackendError::IncompatibleHostProtocol { .. } => {
            "incompatible_host_protocol"
        }
        RemoteSessionBackendError::MissingCapability(_) => "missing_host_capability",
        RemoteSessionBackendError::HostIdentityChanged { .. } => "host_identity_changed",
        RemoteSessionBackendError::InvalidSessionId => "invalid_remote_session_id",
        RemoteSessionBackendError::InvalidOutputOptions(_) => "invalid_output_options",
        RemoteSessionBackendError::InvalidEffectInput { .. } => "invalid_effect_input",
        RemoteSessionBackendError::OutputPagePending(_) => "output_page_pending",
        RemoteSessionBackendError::BootstrapChanged => "host_generation_changed",
        RemoteSessionBackendError::StaleOutputPage => "stale_output_page",
        RemoteSessionBackendError::StateExhausted => "remote_backend_state_exhausted",
        _ => "remote_backend_failed",
    }
}

fn remote_backend_error_result(error: &RemoteSessionBackendError) -> i32 {
    match error {
        RemoteSessionBackendError::InvalidSessionId
        | RemoteSessionBackendError::InvalidOutputOptions(_)
        | RemoteSessionBackendError::InvalidEffectInput { .. } => ERROR_INVALID_INPUT,
        _ => ERROR_REMOTE,
    }
}

fn native_remote_backend_error(
    operation: &'static str,
    error: RemoteSessionBackendError,
) -> NativeRemoteError {
    NativeRemoteError {
        result: remote_backend_error_result(&error),
        code: remote_backend_error_code(&error),
        message: format!("Remote {operation} failed: {error}"),
    }
}

fn native_remote_effect_error(error: RemoteEffectFailure) -> NativeRemoteEffectError {
    let kind = match error.kind() {
        RemoteEffectFailureKind::NotApplied => "notApplied",
        RemoteEffectFailureKind::OutcomeUnknown => "outcomeUnknown",
    };
    NativeRemoteEffectError {
        result: remote_backend_error_result(error.error()),
        kind,
        code: remote_backend_error_code(error.error()),
        operation: error.operation(),
        message: error.to_string(),
    }
}

fn native_not_applied_effect_error(
    operation: &'static str,
) -> impl FnOnce(NativeRemoteError) -> NativeRemoteEffectError {
    move |error| NativeRemoteEffectError {
        result: error.result,
        kind: "notApplied",
        code: error.code,
        operation,
        message: error.message,
    }
}

fn encode_effect_receipt(
    operation: &'static str,
    request_id: u64,
) -> Result<Vec<u8>, NativeRemoteEffectError> {
    serde_json::to_vec(&serde_json::json!({ "requestID": request_id })).map_err(|error| {
        NativeRemoteEffectError {
            result: ERROR_SERIALIZATION,
            kind: "outcomeUnknown",
            code: "effect_receipt_serialization_failed",
            operation,
            message: format!(
                "{operation} succeeded, but its Host receipt could not be encoded: {error}"
            ),
        }
    })
}

fn remote_bootstrap_error(
    transport: &RegisteredRemoteTransport,
    error: RemoteSessionBackendError,
) -> NativeRemoteError {
    let target = transport.target();
    let (code, message) = match &error {
        RemoteSessionBackendError::Connection(connection) => {
            let code = remote_connection_error_code(connection);
            (
                code,
                format!(
                    "Could not reach {target}: {error}. {}",
                    transport.recovery_hint()
                ),
            )
        }
        RemoteSessionBackendError::UnsupportedMobileProtocol { .. }
        | RemoteSessionBackendError::IncompatibleHostProtocol { .. }
        | RemoteSessionBackendError::MissingCapability(_) => (
            "incompatible_host_protocol",
            format!(
                "{target} is not compatible with this Unpeel Controller: {error}. Update Unpeel on the Host and Controller"
            ),
        ),
        RemoteSessionBackendError::HostIdentityChanged { .. } => (
            "host_identity_changed",
            format!(
                "Refusing the changed identity for {target}: {error}. Verify the Host before opening a new connection"
            ),
        ),
        RemoteSessionBackendError::HostStatus { .. } => (
            "host_bootstrap_rejected",
            format!("{target} rejected the bootstrap request: {error}"),
        ),
        RemoteSessionBackendError::InvalidResponse { .. } => (
            "invalid_host_bootstrap",
            format!(
                "{target} returned an invalid bootstrap: {error}. Update Unpeel on the Host and Controller"
            ),
        ),
        RemoteSessionBackendError::BootstrapChanged => (
            "host_generation_changed",
            format!("{target} reconnected while loading: {error}. Refresh the Host"),
        ),
        _ => (
            "remote_bootstrap_failed",
            format!("Could not load {target}: {error}"),
        ),
    };
    NativeRemoteError::remote(code, message)
}

fn encode_remote_error(error: NativeRemoteError) -> Vec<u8> {
    serde_json::json!({
        "code": error.code,
        "error": error.message,
    })
    .to_string()
    .into_bytes()
}

fn encode_remote_effect_error(error: NativeRemoteEffectError) -> Vec<u8> {
    serde_json::json!({
        "kind": error.kind,
        "code": error.code,
        "operation": error.operation,
        "message": error.message,
    })
    .to_string()
    .into_bytes()
}

fn remote_effect_panic_error(operation: &'static str) -> Vec<u8> {
    serde_json::json!({
        "kind": "outcomeUnknown",
        "code": "remote_bridge_panicked",
        "operation": operation,
        "message": "Rust remote Host bridge panicked; refresh Host state before retrying",
    })
    .to_string()
    .into_bytes()
}

fn remote_panic_error() -> Vec<u8> {
    br#"{"code":"remote_bridge_panicked","error":"Rust remote Host bridge panicked"}"#.to_vec()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeBootstrapContext {
    snapshot: Value,
    #[serde(default, rename = "hostID", alias = "hostId")]
    host_id: Option<String>,
    #[serde(default)]
    remote_server_port: Option<u16>,
    #[serde(default)]
    remote_server_certificate_fingerprint: Option<String>,
    #[serde(default)]
    pending_approvals: Vec<Value>,
}

impl From<NativeBootstrapContext> for HostBootstrapContext {
    fn from(value: NativeBootstrapContext) -> Self {
        Self {
            snapshot: value.snapshot,
            host_id: value.host_id,
            remote_server_port: value.remote_server_port,
            remote_server_certificate_fingerprint: value.remote_server_certificate_fingerprint,
            pending_approvals: value.pending_approvals,
            // The bridge is linked only into the native Host. Capability
            // metadata remains Rust-owned and cannot be supplied by Swift.
            protocol: HostProtocolDescriptor::native_v1(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeRouteContext {
    #[serde(default)]
    bootstrap: Option<NativeBootstrapContext>,
    #[serde(default)]
    archived_sessions_by_project: std::collections::HashMap<String, Vec<Value>>,
}

impl From<NativeRouteContext> for HostRouteContext {
    fn from(value: NativeRouteContext) -> Self {
        Self {
            bootstrap: value.bootstrap.map(HostBootstrapContext::from),
            archived_sessions_by_project: value.archived_sessions_by_project,
        }
    }
}

/// The bridge context originally accepted a bootstrap object directly. Keep
/// decoding that shape while the generalized envelope adds route-specific
/// data alongside an optional nested bootstrap. The legacy variant must stay
/// first: the current envelope's fields all default, so it would otherwise
/// accept and discard a legacy object's top-level `snapshot`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NativeRouteContextEnvelope {
    LegacyBootstrap(NativeBootstrapContext),
    Current(NativeRouteContext),
}

impl From<NativeRouteContextEnvelope> for HostRouteContext {
    fn from(value: NativeRouteContextEnvelope) -> Self {
        match value {
            NativeRouteContextEnvelope::LegacyBootstrap(bootstrap) => Self {
                bootstrap: Some(bootstrap.into()),
                ..Self::default()
            },
            NativeRouteContextEnvelope::Current(context) => context.into(),
        }
    }
}

enum RouteOutcome {
    Handled(Vec<u8>),
    Unhandled,
}

fn route_json(request: &[u8], context: Option<&[u8]>) -> Result<RouteOutcome, String> {
    let request: ControllerRequest = serde_json::from_slice(request)
        .map_err(|error| format!("invalid controller request: {error}"))?;
    let context = context
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            serde_json::from_slice::<NativeRouteContextEnvelope>(bytes)
                .map(HostRouteContext::from)
                .map_err(|error| format!("invalid route context: {error}"))
        })
        .transpose()?;
    let Some(response) = controller_api::route_with_context(&request, context.as_ref()) else {
        return Ok(RouteOutcome::Unhandled);
    };
    serde_json::to_vec(&response)
        .map(RouteOutcome::Handled)
        .map_err(|error| format!("controller response encoding failed: {error}"))
}

fn guarded_route(request: &[u8], context: Option<&[u8]>) -> (i32, Vec<u8>) {
    match catch_unwind(AssertUnwindSafe(|| route_json(request, context))) {
        Ok(Ok(RouteOutcome::Handled(bytes))) => (RESULT_HANDLED, bytes),
        Ok(Ok(RouteOutcome::Unhandled)) => (RESULT_UNHANDLED, Vec::new()),
        Ok(Err(message)) => (
            ERROR_INVALID_INPUT,
            serde_json::json!({ "error": message })
                .to_string()
                .into_bytes(),
        ),
        Err(_) => (
            ERROR_PANIC,
            br#"{"error":"Rust controller bridge panicked"}"#.to_vec(),
        ),
    }
}

unsafe fn input_bytes<'a>(pointer: *const u8, length: usize) -> Result<&'a [u8], String> {
    if length == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err("non-empty input has a null pointer".into());
    }
    Ok(std::slice::from_raw_parts(pointer, length))
}

unsafe fn remote_utf8_input<'a>(
    pointer: *const u8,
    length: usize,
    buffer_code: &'static str,
    utf8_code: &'static str,
    label: &'static str,
) -> Result<&'a str, NativeRemoteError> {
    let bytes = input_bytes(pointer, length)
        .map_err(|message| NativeRemoteError::invalid_input(buffer_code, message))?;
    std::str::from_utf8(bytes).map_err(|_| {
        NativeRemoteError::invalid_input(utf8_code, format!("{label} must be UTF-8 text"))
    })
}

unsafe fn return_bytes(bytes: Vec<u8>, out_pointer: *mut *mut u8, out_length: *mut usize) {
    if bytes.is_empty() {
        return;
    }
    let mut boxed = bytes.into_boxed_slice();
    *out_length = boxed.len();
    *out_pointer = boxed.as_mut_ptr();
    std::mem::forget(boxed);
}

unsafe fn finish_remote_effect_ffi(
    outcome: Result<
        Result<Vec<u8>, NativeRemoteEffectError>,
        Box<dyn std::any::Any + Send + 'static>,
    >,
    operation: &'static str,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    match outcome {
        Ok(Ok(bytes)) => {
            return_bytes(bytes, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_effect_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(
                remote_effect_panic_error(operation),
                out_pointer,
                out_length,
            );
            ERROR_PANIC
        }
    }
}

#[no_mangle]
pub extern "C" fn unpeel_native_bridge_abi_version() -> u32 {
    ABI_VERSION
}

/// Fold legacy sidebar pins into ordinary Pinned groups owned by the Host.
/// Returns the number of Sessions migrated, zero when there was nothing to
/// do, and a negative bridge error on failure. No Rust error allocation
/// crosses this small startup/scan helper.
#[no_mangle]
pub extern "C" fn unpeel_native_bridge_migrate_legacy_pins() -> i32 {
    match catch_unwind(AssertUnwindSafe(|| {
        unpeel_core::session_ops::migrate_legacy_pins_to_groups()
    })) {
        Ok(Ok(count)) => i32::try_from(count).unwrap_or(i32::MAX),
        Ok(Err(_)) => ERROR_INVALID_INPUT,
        Err(_) => ERROR_PANIC,
    }
}

/// Route one Controller request.
///
/// Return values: `1` handled, `0` unhandled (Swift compatibility fallback),
/// negative for a bridge error. When output is non-empty, the caller owns it
/// and must call `unpeel_native_bridge_free` exactly once.
///
/// The optional context input accepts the generalized route-context envelope;
/// a legacy top-level bootstrap object remains valid. Its position and C types
/// are unchanged from ABI v1.
///
/// # Safety
///
/// Every non-empty input must point to a readable allocation of its declared
/// length. Output pointers must both be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_route(
    request_pointer: *const u8,
    request_length: usize,
    context_pointer: *const u8,
    context_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let request = match input_bytes(request_pointer, request_length) {
        Ok(value) => value,
        Err(message) => {
            return_bytes(
                serde_json::json!({ "error": message })
                    .to_string()
                    .into_bytes(),
                out_pointer,
                out_length,
            );
            return ERROR_INVALID_INPUT;
        }
    };
    let context = match input_bytes(context_pointer, context_length) {
        Ok(value) => value,
        Err(message) => {
            return_bytes(
                serde_json::json!({ "error": message })
                    .to_string()
                    .into_bytes(),
                out_pointer,
                out_length,
            );
            return ERROR_INVALID_INPUT;
        }
    };
    let (code, bytes) = guarded_route(request, (!context.is_empty()).then_some(context));
    return_bytes(bytes, out_pointer, out_length);
    code
}

/// Open an SSH-backed remote Host scope without starting local Unpeel
/// services or reading local Unpeel state.
///
/// Success returns `1`, writes a non-zero registry handle, and leaves the
/// output buffer empty. A negative result leaves the handle at zero and may
/// return an owned, UTF-8 JSON error buffer. Opening validates only the
/// target; [`unpeel_native_bridge_remote_bootstrap`] performs the first SSH
/// request so the caller can schedule network work away from the main thread.
///
/// # Safety
///
/// A non-empty target must point to a readable allocation of its declared
/// length. All three output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_ssh_open(
    target_pointer: *const u8,
    target_length: usize,
    out_handle: *mut RemoteHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_handle.is_null() || out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_handle = 0;
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let target = input_bytes(target_pointer, target_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_target_buffer", message)
        })?;
        open_ssh_remote(target)
    }));
    match outcome {
        Ok(Ok(handle)) => {
            *out_handle = handle;
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Open an SSH-backed remote Host with an explicit launch mode and optional
/// askpass secret. The secret stays inside Rust/the owned SSH environment and
/// is never returned in diagnostics.
///
/// # Safety
///
/// A non-empty config must point to readable UTF-8 JSON. All output pointers
/// must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_ssh_config_open(
    config_pointer: *const u8,
    config_length: usize,
    out_handle: *mut RemoteHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_handle.is_null() || out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_handle = 0;
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let config = input_bytes(config_pointer, config_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_ssh_config_buffer", message)
        })?;
        open_ssh_remote_config(config)
    }));
    match outcome {
        Ok(Ok(handle)) => {
            *out_handle = handle;
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Install Unpeel on an SSH destination using a fixed, product-owned command.
/// The JSON configuration is identical to `remote_ssh_config_open`; no shell
/// command crosses the ABI. Success returns owned `{\"mode\": ...}` JSON.
///
/// # Safety
///
/// A non-empty config must point to readable UTF-8 JSON. Both output pointers
/// must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_ssh_install(
    config_pointer: *const u8,
    config_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let config = input_bytes(config_pointer, config_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_ssh_config_buffer", message)
        })?;
        install_ssh_remote(config)
    }));
    match outcome {
        Ok(Ok(output)) => {
            return_bytes(output, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Open the loopback workspace gateway from UTF-8 JSON containing the
/// absolute `unpeel-host` program path and the workspace's `UNPEEL_HOME`.
/// Opening validates only the configuration; the child is spawned by the
/// first [`unpeel_native_bridge_remote_bootstrap`].
///
/// # Safety
///
/// A non-empty config must point to readable UTF-8 JSON. All output pointers
/// must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_local_gateway_open(
    config_pointer: *const u8,
    config_length: usize,
    out_handle: *mut RemoteHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_handle.is_null() || out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_handle = 0;
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let config = input_bytes(config_pointer, config_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_local_gateway_config_buffer", message)
        })?;
        open_local_gateway_remote(config)
    }));
    match outcome {
        Ok(Ok(handle)) => {
            *out_handle = handle;
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Start a reconnecting, connection-scoped platform adapter registration for
/// one local workspace worker. Opening does not require `host.sock` to exist
/// yet; the background client registers as soon as `unpeel serve` is ready.
///
/// # Safety
///
/// A non-empty config must point to readable UTF-8 JSON. All output pointers
/// must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_platform_adapter_start(
    config_pointer: *const u8,
    config_length: usize,
    out_handle: *mut PlatformAdapterHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_handle.is_null() || out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_handle = 0;
    *out_pointer = ptr::null_mut();
    *out_length = 0;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let config = input_bytes(config_pointer, config_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_platform_adapter_config_buffer", message)
        })?;
        start_platform_adapter_client(config)
    }));
    match outcome {
        Ok(Ok(handle)) => {
            *out_handle = handle;
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Stop the reconnect loop and close its registration socket. The workspace
/// worker withdraws every capability bound to that connection before return.
///
/// # Safety
///
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_platform_adapter_stop(
    handle: PlatformAdapterHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;
    let outcome = catch_unwind(AssertUnwindSafe(|| stop_platform_adapter_client(handle)));
    match outcome {
        Ok(Ok(())) => RESULT_OK,
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Perform one same-user management request against the canonical workspace
/// worker over its mode-0600 `host.sock`. Success returns the Host's owned
/// JSON response; no pairing credentials or authorization hashes are exposed.
///
/// # Safety
///
/// A non-empty config must point to readable UTF-8 JSON. Both output pointers
/// must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_local_host_control(
    config_pointer: *const u8,
    config_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let config = input_bytes(config_pointer, config_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_local_host_control_buffer", message)
        })?;
        local_host_control(config)
    }));
    match outcome {
        Ok(Ok(bytes)) => {
            return_bytes(bytes, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Open a paired direct/LAN Host scope without performing network I/O.
///
/// The endpoint must be the exact plain-HTTP `/mobile` scope returned by
/// pairing and is suitable only for a trusted LAN or VPN. The bearer remains
/// inside the Rust transport; it is never included in output or error JSON.
/// The first network request occurs in
/// [`unpeel_native_bridge_remote_bootstrap`].
///
/// # Safety
///
/// Non-empty inputs must point to readable allocations of their declared
/// lengths. All three output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_direct_open(
    endpoint_pointer: *const u8,
    endpoint_length: usize,
    bearer_pointer: *const u8,
    bearer_length: usize,
    out_handle: *mut RemoteHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_handle.is_null() || out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_handle = 0;
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let endpoint = input_bytes(endpoint_pointer, endpoint_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_host_endpoint_buffer", message)
        })?;
        let bearer = input_bytes(bearer_pointer, bearer_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_host_bearer_buffer", message)
        })?;
        open_direct_remote(endpoint, bearer)
    }));
    match outcome {
        Ok(Ok(handle)) => {
            *out_handle = handle;
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Open an E2E Link-backed Host scope without performing network I/O.
///
/// The retained callback context is consumed by this call on both success and
/// failure, then released exactly once after the backend closes or opening is
/// rejected. Swift's callback delegates to the shared `RemoteRelayConnection`;
/// Rust keeps semantic generations, cursors, and at-most-once effects in the
/// same `RemoteSessionBackend` used by Direct and SSH.
///
/// # Safety
///
/// A non-empty bearer must point to readable bytes. `context` must be a valid
/// retained object for every callback. All output pointers must be non-null and
/// writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_relay_open(
    bearer_pointer: *const u8,
    bearer_length: usize,
    context: *mut c_void,
    request_callback: Option<RelayRequestCallback>,
    bytes_release_callback: Option<RelayBytesReleaseCallback>,
    disconnect_callback: Option<RelayDisconnectCallback>,
    context_release_callback: Option<RelayContextReleaseCallback>,
    out_handle: *mut RemoteHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    let mut context_owner = RelayContextOwner {
        context,
        release: context_release_callback,
        armed: true,
    };
    if out_handle.is_null() || out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_handle = 0;
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if context.is_null() {
            return Err(NativeRemoteError::invalid_input(
                "invalid_link_context",
                "Link transport context must not be null",
            ));
        }
        let request_callback = request_callback.ok_or_else(|| {
            NativeRemoteError::invalid_input(
                "invalid_link_request_callback",
                "Link request callback must not be null",
            )
        })?;
        let bytes_release_callback = bytes_release_callback.ok_or_else(|| {
            NativeRemoteError::invalid_input(
                "invalid_link_bytes_release_callback",
                "Link byte-release callback must not be null",
            )
        })?;
        let disconnect_callback = disconnect_callback.ok_or_else(|| {
            NativeRemoteError::invalid_input(
                "invalid_link_disconnect_callback",
                "Link disconnect callback must not be null",
            )
        })?;
        let context_release_callback = context_release_callback.ok_or_else(|| {
            NativeRemoteError::invalid_input(
                "invalid_link_context_release_callback",
                "Link context-release callback must not be null",
            )
        })?;
        let bearer = input_bytes(bearer_pointer, bearer_length).map_err(|message| {
            NativeRemoteError::invalid_input("invalid_link_bearer_buffer", message)
        })?;
        let executor = Arc::new(CallbackRelayExecutor {
            context: context as usize,
            request_callback,
            bytes_release_callback,
            disconnect_callback,
            context_release_callback,
        });
        context_owner.disarm();
        open_link_remote(bearer, executor)
    }));
    match outcome {
        Ok(Ok(handle)) => {
            *out_handle = handle;
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Refresh a remote Host and return its validated, typed bootstrap snapshot.
///
/// Success returns `1` and an owned UTF-8 JSON buffer containing exactly
/// `RemoteBootstrapSnapshot` (unknown wire fields are not forwarded). A
/// negative result may return an owned JSON error with stable `code` and
/// actionable `error` fields. The handle remains valid after ordinary
/// bootstrap failures so the caller can explicitly refresh it later.
///
/// # Safety
///
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_bootstrap(
    handle: RemoteHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| bootstrap_remote(handle)));
    match outcome {
        Ok(Ok(bytes)) => {
            return_bytes(bytes, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn remote_output_poll_ffi(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    requested_offset: Option<Option<u64>>,
    limit: usize,
    wait_ms: u64,
    out_page_handle: *mut RemoteOutputPageHandle,
    out_metadata_pointer: *mut *mut u8,
    out_metadata_length: *mut usize,
    out_bytes_pointer: *mut *mut u8,
    out_bytes_length: *mut usize,
) -> i32 {
    if out_page_handle.is_null()
        || out_metadata_pointer.is_null()
        || out_metadata_length.is_null()
        || out_bytes_pointer.is_null()
        || out_bytes_length.is_null()
    {
        return ERROR_INVALID_INPUT;
    }
    *out_page_handle = 0;
    *out_metadata_pointer = ptr::null_mut();
    *out_metadata_length = 0;
    *out_bytes_pointer = ptr::null_mut();
    *out_bytes_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )?;
        match requested_offset {
            Some(requested_offset) => {
                poll_remote_output_from(handle, session_id, requested_offset, limit, wait_ms)
            }
            None => poll_remote_output(handle, session_id, limit, wait_ms),
        }
    }));
    match outcome {
        Ok(Ok(output)) => {
            *out_page_handle = output.page_handle;
            return_bytes(output.metadata, out_metadata_pointer, out_metadata_length);
            return_bytes(output.bytes, out_bytes_pointer, out_bytes_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(
                encode_remote_error(error),
                out_metadata_pointer,
                out_metadata_length,
            );
            result
        }
        Err(_) => {
            return_bytes(
                remote_panic_error(),
                out_metadata_pointer,
                out_metadata_length,
            );
            ERROR_PANIC
        }
    }
}

/// Poll one bounded terminal-output page. Metadata and raw bytes are returned
/// as separate owned buffers so terminal bytes never pass through JSON or a
/// lossy string conversion. The page remains staged until an explicit commit
/// or discard using the returned page handle.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Every output
/// pointer must be non-null and writable. Both returned buffers are freed
/// independently with [`unpeel_native_bridge_free`].
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_output_poll(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    limit: usize,
    wait_ms: u64,
    out_page_handle: *mut RemoteOutputPageHandle,
    out_metadata_pointer: *mut *mut u8,
    out_metadata_length: *mut usize,
    out_bytes_pointer: *mut *mut u8,
    out_bytes_length: *mut usize,
) -> i32 {
    remote_output_poll_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        None,
        limit,
        wait_ms,
        out_page_handle,
        out_metadata_pointer,
        out_metadata_length,
        out_bytes_pointer,
        out_bytes_length,
    )
}

/// Poll from the Controller's exact rendered offset. `has_offset == 0`
/// explicitly requests a fresh bounded tail. The cursor replacement and page
/// reservation are atomic in core, so an older renderer cannot commit across
/// this call.
///
/// # Safety
///
/// Pointer ownership matches [`unpeel_native_bridge_remote_output_poll`].
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_output_poll_from(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    requested_offset: u64,
    has_offset: u8,
    limit: usize,
    wait_ms: u64,
    out_page_handle: *mut RemoteOutputPageHandle,
    out_metadata_pointer: *mut *mut u8,
    out_metadata_length: *mut usize,
    out_bytes_pointer: *mut *mut u8,
    out_bytes_length: *mut usize,
) -> i32 {
    remote_output_poll_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        Some((has_offset != 0).then_some(requested_offset)),
        limit,
        wait_ms,
        out_page_handle,
        out_metadata_pointer,
        out_metadata_length,
        out_bytes_pointer,
        out_bytes_length,
    )
}

/// Commit a staged output page after the renderer accepted every returned
/// byte. The backend cursor advances exactly once; the page handle is consumed
/// even when core rejects it as stale.
///
/// # Safety
///
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_output_commit(
    handle: RemoteHandle,
    page_handle: RemoteOutputPageHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        commit_remote_output(handle, page_handle)
    }));
    match outcome {
        Ok(Ok(())) => RESULT_OK,
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Explicitly discard a staged output page without advancing its cursor.
///
/// # Safety
///
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_output_discard(
    handle: RemoteHandle,
    page_handle: RemoteOutputPageHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        discard_remote_output(handle, page_handle)
    }));
    match outcome {
        Ok(Ok(())) => RESULT_OK,
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Reset one Session output cursor to a fresh bounded tail and discard any
/// staged bridge page for that Session.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_output_reset(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )?;
        reset_remote_output(handle, session_id)
    }));
    match outcome {
        Ok(Ok(())) => RESULT_OK,
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Send UTF-8 terminal input at most once on the generation accepted by the
/// latest bootstrap. Effects are never reconnected or replayed by this API.
///
/// # Safety
///
/// Non-empty inputs must point to readable UTF-8 bytes. Both output pointers
/// must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_terminal_write(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    data_pointer: *const u8,
    data_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )
        .map_err(native_not_applied_effect_error("terminal write"))?;
        let data = remote_utf8_input(
            data_pointer,
            data_length,
            "invalid_terminal_data_buffer",
            "invalid_terminal_data_utf8",
            "Terminal input",
        )
        .map_err(native_not_applied_effect_error("terminal write"))?;
        write_remote_terminal(handle, session_id, data)
    }));
    match outcome {
        Ok(Ok(bytes)) => {
            return_bytes(bytes, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_effect_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(
                remote_effect_panic_error("terminal write"),
                out_pointer,
                out_length,
            );
            ERROR_PANIC
        }
    }
}

/// Fit the Host's desktop/TUI presentation to this Controller. Core applies
/// the shipped v1 dimension clamps before sending the at-most-once effect.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_desktop_fit(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    columns: u16,
    rows: u16,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )
        .map_err(native_not_applied_effect_error("desktop resize"))?;
        fit_remote_desktop(handle, session_id, columns, rows)
    }));
    finish_remote_effect_ffi(outcome, "desktop resize", out_pointer, out_length)
}

/// Clear this Controller's Host desktop/TUI fit override.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_desktop_clear(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )
        .map_err(native_not_applied_effect_error("desktop resize"))?;
        clear_remote_desktop(handle, session_id)
    }));
    finish_remote_effect_ffi(outcome, "desktop resize", out_pointer, out_length)
}

/// Clear a remote Session's unread marker at most once.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_mark_read(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )
        .map_err(native_not_applied_effect_error("mark Session read"))?;
        mark_remote_session_read(handle, session_id)
    }));
    finish_remote_effect_ffi(outcome, "mark Session read", out_pointer, out_length)
}

/// Shared body for the `(handle, session_id) → effect receipt` FFI verbs.
///
/// # Safety
///
/// Same contract as the public effect FFI functions: non-empty inputs must be
/// readable for their length and both output pointers non-null and writable.
unsafe fn remote_session_effect_ffi(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    operation: &'static str,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
    effect: impl FnOnce(&str) -> Result<Vec<u8>, NativeRemoteEffectError>,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;
    let _ = handle;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )
        .map_err(native_not_applied_effect_error(operation))?;
        effect(session_id)
    }));
    finish_remote_effect_ffi(outcome, operation, out_pointer, out_length)
}

/// Rename a remote Session at most once (`session.title.set`).
///
/// # Safety
///
/// Non-empty inputs must point to readable UTF-8 bytes. Both output pointers
/// must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_title_set(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    title_pointer: *const u8,
    title_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        "session title",
        out_pointer,
        out_length,
        |session_id| {
            let title = remote_utf8_input(
                title_pointer,
                title_length,
                "invalid_title_buffer",
                "invalid_title_utf8",
                "Session title",
            )
            .map_err(native_not_applied_effect_error("session title"))?;
            set_remote_session_title(handle, session_id, title)
        },
    )
}

/// Pin or unpin a remote Session in the Host sidebar (`session.pin.set`).
/// `pinned` is non-zero to pin, zero to unpin.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_pinned_set(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    pinned: i32,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        "session pin",
        out_pointer,
        out_length,
        |session_id| set_remote_session_pinned(handle, session_id, pinned != 0),
    )
}

/// Opt a remote Session in or out of completion delivery through the Host's
/// currently registered platform adapter (`session.notify_when_done.set`).
/// `enabled` is non-zero to opt in, zero to opt out.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_notify_when_done_set(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    enabled: i32,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        "notify when done",
        out_pointer,
        out_length,
        |session_id| set_remote_session_notify_when_done(handle, session_id, enabled != 0),
    )
}

/// Answer one Host-owned MCP approval (`approval.answer`). `approved` is
/// non-zero to allow and zero to deny.
///
/// # Safety
///
/// A non-empty approval id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_approval_answer(
    handle: RemoteHandle,
    approval_id_pointer: *const u8,
    approval_id_length: usize,
    approved: i32,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        approval_id_pointer,
        approval_id_length,
        "approval answer",
        out_pointer,
        out_length,
        |approval_id| answer_remote_approval(handle, approval_id, approved != 0),
    )
}

/// File a remote Session under another project/group
/// (`session.project.set`) via the Host's shared project-override marker.
///
/// # Safety
///
/// Non-empty Session and project ids must point to readable UTF-8 bytes.
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_project_set(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    project_id_pointer: *const u8,
    project_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        const OPERATION: &str = "session project";
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )
        .map_err(native_not_applied_effect_error(OPERATION))?;
        let project_id = remote_utf8_input(
            project_id_pointer,
            project_id_length,
            "invalid_project_id_buffer",
            "invalid_project_id_utf8",
            "Remote project id",
        )
        .map_err(native_not_applied_effect_error(OPERATION))?;
        set_remote_session_project(handle, session_id, project_id)
    }));
    finish_remote_effect_ffi(outcome, "session project", out_pointer, out_length)
}

/// File a remote Session away non-destructively (`session.archive`).
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_archive(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        RemoteSessionVerb::Archive.operation(),
        out_pointer,
        out_length,
        |session_id| perform_remote_session_verb(handle, RemoteSessionVerb::Archive, session_id),
    )
}

/// Restore an archived remote Session to the sidebar (`session.restore`).
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_restore(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        RemoteSessionVerb::Restore.operation(),
        out_pointer,
        out_length,
        |session_id| perform_remote_session_verb(handle, RemoteSessionVerb::Restore, session_id),
    )
}

/// Stop a remote Session's hosted PTY, keeping the row restartable
/// (`session.stop`).
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_stop(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        RemoteSessionVerb::Stop.operation(),
        out_pointer,
        out_length,
        |session_id| perform_remote_session_verb(handle, RemoteSessionVerb::Stop, session_id),
    )
}

/// Remove a remote Session row and its on-Host artifacts (`session.remove`).
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_remove(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        RemoteSessionVerb::Remove.operation(),
        out_pointer,
        out_length,
        |session_id| perform_remote_session_verb(handle, RemoteSessionVerb::Remove, session_id),
    )
}

/// Restart a remote Session with the Host's resume behavior
/// (`session.restart`).
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_restart(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        RemoteSessionVerb::Restart.operation(),
        out_pointer,
        out_length,
        |session_id| perform_remote_session_verb(handle, RemoteSessionVerb::Restart, session_id),
    )
}

/// Restart only the managed agent inside a remote Session's existing hosted
/// terminal (`session.runtime.restart`).
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_restart_agent(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        RemoteSessionVerb::RestartAgent.operation(),
        out_pointer,
        out_length,
        |session_id| {
            perform_remote_session_verb(handle, RemoteSessionVerb::RestartAgent, session_id)
        },
    )
}

/// Resume an ended managed agent inside a remote Session's existing hosted
/// terminal (`session.runtime.resume`). The Host refuses active or unknown
/// foreground jobs without signaling them.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_resume_agent(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    remote_session_effect_ffi(
        handle,
        session_id_pointer,
        session_id_length,
        RemoteSessionVerb::ResumeAgent.operation(),
        out_pointer,
        out_length,
        |session_id| {
            perform_remote_session_verb(handle, RemoteSessionVerb::ResumeAgent, session_id)
        },
    )
}

/// Replace one project's hand-ordered Session ranks (`session.order.set`).
/// `ordered_ids_json` is a UTF-8 JSON array of Session id strings — the
/// combined pinned + regular order exactly as a desktop drag commits it.
///
/// # Safety
///
/// Non-empty inputs must point to readable bytes of their declared lengths.
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_order_set(
    handle: RemoteHandle,
    project_id_pointer: *const u8,
    project_id_length: usize,
    ordered_ids_json_pointer: *const u8,
    ordered_ids_json_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let project_id = remote_utf8_input(
            project_id_pointer,
            project_id_length,
            "invalid_project_id_buffer",
            "invalid_project_id_utf8",
            "Remote project id",
        )
        .map_err(native_not_applied_effect_error("session order"))?;
        let ordered_ids_json = input_bytes(ordered_ids_json_pointer, ordered_ids_json_length)
            .map_err(|message| {
                native_not_applied_effect_error("session order")(NativeRemoteError::invalid_input(
                    "invalid_session_order_buffer",
                    message,
                ))
            })?;
        set_remote_session_order(handle, project_id, ordered_ids_json)
    }));
    finish_remote_effect_ffi(outcome, "session order", out_pointer, out_length)
}

/// Edit the remote Host's flat preset list (`settings.presets.set`).
/// `patch_json` is the camelCase one-preset patch (presetID?, command?,
/// label?, quickLaunch?, sortOrder?, removed?). No `presetID` creates —
/// `command` required, the Host mints the id; `removed` deletes and cannot
/// combine with other fields; `sortOrder` moves the preset to that index in
/// the Host's display order.
///
/// # Safety
///
/// A non-empty patch must point to readable bytes of its declared length.
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_preset_set(
    handle: RemoteHandle,
    patch_json_pointer: *const u8,
    patch_json_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let patch_json = input_bytes(patch_json_pointer, patch_json_length).map_err(|message| {
            native_not_applied_effect_error("preset edit")(NativeRemoteError::invalid_input(
                "invalid_preset_patch_buffer",
                message,
            ))
        })?;
        set_remote_preset(handle, patch_json)
    }));
    finish_remote_effect_ffi(outcome, "preset edit", out_pointer, out_length)
}

/// Edit the remote workspace's behavior knobs (`settings.workspace.set`).
/// `patch_json` is the camelCase typed patch; absent fields are left
/// unchanged and every present field is validated before anything applies.
///
/// # Safety
///
/// A non-empty patch must point to readable bytes of its declared length.
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_workspace_settings_set(
    handle: RemoteHandle,
    patch_json_pointer: *const u8,
    patch_json_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let patch_json = input_bytes(patch_json_pointer, patch_json_length).map_err(|message| {
            native_not_applied_effect_error("workspace settings")(NativeRemoteError::invalid_input(
                "invalid_workspace_settings_buffer",
                message,
            ))
        })?;
        set_remote_workspace_settings(handle, patch_json)
    }));
    finish_remote_effect_ffi(outcome, "workspace settings", out_pointer, out_length)
}

/// Organize one remote project/group (`project.organization.set`).
/// `patch_json` is the camelCase one-project patch (sortOrder?, displayName?,
/// colorID?, dateSorted?, pinned?; absent fields are left unchanged). `sortOrder`
/// moves the project to that index among its same-parent siblings in the
/// Host's current display order.
///
/// # Safety
///
/// Non-empty inputs must point to readable bytes of their declared lengths.
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_project_organization_set(
    handle: RemoteHandle,
    project_id_pointer: *const u8,
    project_id_length: usize,
    patch_json_pointer: *const u8,
    patch_json_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let project_id = remote_utf8_input(
            project_id_pointer,
            project_id_length,
            "invalid_project_id_buffer",
            "invalid_project_id_utf8",
            "Remote project id",
        )
        .map_err(native_not_applied_effect_error("project organization"))?;
        let patch_json = input_bytes(patch_json_pointer, patch_json_length).map_err(|message| {
            native_not_applied_effect_error("project organization")(
                NativeRemoteError::invalid_input("invalid_project_organization_buffer", message),
            )
        })?;
        set_remote_project_organization(handle, project_id, patch_json)
    }));
    finish_remote_effect_ffi(outcome, "project organization", out_pointer, out_length)
}

/// Create a Session on the Host (`session.create`) from a user-initiated
/// Controller action. `request_json` is the camelCase create request
/// (projectID, presetID?, command?, worktreePath?, worktreeBranch?,
/// initialText?, initialTextSubmitMode?). Success returns owned JSON with
/// `requestID`, `sessionID`, optional `capturedAtUnixMs`, and the optional
/// optimistic `session` summary.
///
/// # Safety
///
/// A non-empty request must point to readable bytes of its declared length.
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_create(
    handle: RemoteHandle,
    request_json_pointer: *const u8,
    request_json_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let request_json =
            input_bytes(request_json_pointer, request_json_length).map_err(|message| {
                native_not_applied_effect_error("session create")(NativeRemoteError::invalid_input(
                    "invalid_session_create_buffer",
                    message,
                ))
            })?;
        create_remote_session(handle, request_json)
    }));
    finish_remote_effect_ffi(outcome, "session create", out_pointer, out_length)
}

/// Create or complete a controller-assisted pairing invitation on the
/// selected Host. `request_json` is `{action:"create", endpoint:...}` or
/// `{action:"complete", envelope:...}`. Success returns the Host's raw JSON
/// pairing payload/envelope; failures use the ordinary effect error envelope.
///
/// # Safety
///
/// A non-empty request must point to readable bytes of its declared length.
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_pairing_invitation(
    handle: RemoteHandle,
    request_json_pointer: *const u8,
    request_json_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let request_json =
            input_bytes(request_json_pointer, request_json_length).map_err(|message| {
                native_not_applied_effect_error("pairing invitation")(
                    NativeRemoteError::invalid_input("invalid_pairing_invitation_buffer", message),
                )
            })?;
        exchange_remote_pairing_invitation(handle, request_json)
    }));
    finish_remote_effect_ffi(outcome, "pairing invitation", out_pointer, out_length)
}

/// Upload raw image bytes to the selected Host (`artifact.upload`) and return
/// `{path}` — the HOST-side file the Controller pastes as an attachable
/// reference. An empty Session id targets the Host's shared dropped-images
/// dir; failures use the ordinary effect error envelope with no auto-replay.
///
/// # Safety
///
/// Non-empty inputs must point to readable bytes of their declared lengths
/// (the Session id and content type additionally to UTF-8). Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_upload_attachment(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    content_type_pointer: *const u8,
    content_type_length: usize,
    bytes_pointer: *const u8,
    bytes_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        const OPERATION: &str = "attachment upload";
        let session_id = if session_id_length == 0 {
            None
        } else {
            Some(
                remote_utf8_input(
                    session_id_pointer,
                    session_id_length,
                    "invalid_session_id_buffer",
                    "invalid_session_id_utf8",
                    "Remote Session id",
                )
                .map_err(native_not_applied_effect_error(OPERATION))?,
            )
        };
        let content_type = remote_utf8_input(
            content_type_pointer,
            content_type_length,
            "invalid_content_type_buffer",
            "invalid_content_type_utf8",
            "Upload content type",
        )
        .map_err(native_not_applied_effect_error(OPERATION))?;
        let bytes = input_bytes(bytes_pointer, bytes_length).map_err(|message| {
            native_not_applied_effect_error(OPERATION)(NativeRemoteError::invalid_input(
                "invalid_upload_buffer",
                message,
            ))
        })?;
        upload_remote_attachment(handle, session_id, content_type, bytes.to_vec())
    }));
    finish_remote_effect_ffi(outcome, "attachment upload", out_pointer, out_length)
}

/// List one project's archived Sessions (`session.archive.list`) — a
/// capability-gated read, not an effect. Success returns owned JSON with
/// `projectID` and `sessions`.
///
/// # Safety
///
/// A non-empty project id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_archived_sessions(
    handle: RemoteHandle,
    project_id_pointer: *const u8,
    project_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let project_id = remote_utf8_input(
            project_id_pointer,
            project_id_length,
            "invalid_project_id_buffer",
            "invalid_project_id_utf8",
            "Remote project id",
        )?;
        list_remote_archived_sessions(handle, project_id)
    }));
    match outcome {
        Ok(Ok(bytes)) => {
            return_bytes(bytes, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Read a remote Session's rendered conversation transcript
/// (`session.transcript.markdown`) — a capability-gated read, not an effect.
/// `entries` limits the transcript to the most recent N entries; `0` uses the
/// Host's configured setting. Success returns owned JSON with `sessionID` and
/// `markdown`.
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_transcript_markdown(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    entries: u32,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )?;
        read_remote_transcript_markdown(handle, session_id, (entries > 0).then_some(entries))
    }));
    match outcome {
        Ok(Ok(bytes)) => {
            return_bytes(bytes, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Read one live remote Session's current terminal grid
/// (`session.metrics.read`) — a capability-gated read, not an effect. Success
/// returns owned JSON with `sessionID`, `columns`, `rows`, `outputOffset?`,
/// and `capturedAtUnixMs` (`outputOffset` is omitted by Hosts that predate
/// it in the gateway metrics body).
///
/// # Safety
///
/// A non-empty Session id must point to readable UTF-8 bytes. Both output
/// pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_session_metrics(
    handle: RemoteHandle,
    session_id_pointer: *const u8,
    session_id_length: usize,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let session_id = remote_utf8_input(
            session_id_pointer,
            session_id_length,
            "invalid_session_id_buffer",
            "invalid_session_id_utf8",
            "Remote Session id",
        )?;
        read_remote_session_metrics(handle, session_id)
    }));
    match outcome {
        Ok(Ok(bytes)) => {
            return_bytes(bytes, out_pointer, out_length);
            RESULT_OK
        }
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Remove a remote Host handle and disconnect its owned SSH process.
///
/// Success returns `1` with no output. Closing an unknown/already-closed
/// handle returns [`ERROR_INVALID_HANDLE`] plus an owned JSON error.
///
/// # Safety
///
/// Both output pointers must be non-null and writable.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_remote_close(
    handle: RemoteHandle,
    out_pointer: *mut *mut u8,
    out_length: *mut usize,
) -> i32 {
    if out_pointer.is_null() || out_length.is_null() {
        return ERROR_INVALID_INPUT;
    }
    *out_pointer = ptr::null_mut();
    *out_length = 0;

    let outcome = catch_unwind(AssertUnwindSafe(|| close_remote(handle)));
    match outcome {
        Ok(Ok(())) => RESULT_OK,
        Ok(Err(error)) => {
            let result = error.result;
            return_bytes(encode_remote_error(error), out_pointer, out_length);
            result
        }
        Err(_) => {
            return_bytes(remote_panic_error(), out_pointer, out_length);
            ERROR_PANIC
        }
    }
}

/// Free a byte buffer returned by any `unpeel_native_bridge_*` function.
///
/// # Safety
///
/// The pointer/length pair must be an outstanding buffer returned by this
/// library and must not have been freed before.
#[no_mangle]
pub unsafe extern "C" fn unpeel_native_bridge_free(pointer: *mut u8, length: usize) {
    if pointer.is_null() || length == 0 {
        return;
    }
    let slice = ptr::slice_from_raw_parts_mut(pointer, length);
    drop(Box::from_raw(slice));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use unpeel_core::controller_api::{ControllerPrincipal, ControllerResponse};

    #[test]
    fn platform_adapter_config_accepts_the_swift_id_spelling_and_stops_cleanly() {
        let config = serde_json::to_vec(&json!({
            "unpeelHome": "/tmp/unpeel-platform-adapter-no-worker",
            "instanceID": "native-test",
            "callbackPort": 41001,
            "callbackToken": "0123456789abcdef0123456789abcdef",
            "capabilities": ["session.notify_when_done.set"]
        }))
        .unwrap();
        let handle = start_platform_adapter_client(&config).unwrap();
        assert!(lock_platform_adapter_clients().contains_key(&handle));
        stop_platform_adapter_client(handle).unwrap();
        assert!(!lock_platform_adapter_clients().contains_key(&handle));
    }

    #[cfg(unix)]
    #[test]
    fn platform_adapter_reregisters_and_callbacks_succeed_after_worker_restart() {
        use std::os::unix::net::{UnixListener, UnixStream};
        use unpeel_serve::platform_adapter::{PlatformAdapterHub, PlatformAdapterRegistration};

        struct AdapterGuard(Option<PlatformAdapterHandle>);
        impl Drop for AdapterGuard {
            fn drop(&mut self) {
                if let Some(handle) = self.0.take() {
                    let _ = stop_platform_adapter_client(handle);
                }
            }
        }

        fn accept_with_timeout(listener: &UnixListener) -> UnixStream {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        return stream;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "platform adapter did not reconnect after worker restart"
                        );
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => panic!("accept platform adapter: {error}"),
                }
            }
        }

        fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..count]);
                let complete = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .and_then(|separator| {
                        let head = std::str::from_utf8(&request[..separator]).ok()?;
                        let content_length = head.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })?;
                        Some(request.len() >= separator + 4 + content_length)
                    })
                    .unwrap_or(false);
                if complete || count == 0 {
                    return request;
                }
            }
        }

        fn register_and_invoke(
            listener: UnixListener,
            generation: u64,
            expected_token: &str,
        ) -> String {
            let mut stream = accept_with_timeout(&listener);
            let frame = unpeel_core::remote_stdio::read_frame(&mut stream)
                .unwrap()
                .expect("platform adapter registration frame");
            assert_eq!(frame.kind, unpeel_core::remote_stdio::FRAME_KIND_REQUEST);
            let request = unpeel_core::relay_wire::parse_tunnel_request(&frame.payload).unwrap();
            assert_eq!(request.path, "/_unpeel/platform-adapter");
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["action"], "register");
            let registration: PlatformAdapterRegistration =
                serde_json::from_value(body["registration"].clone()).unwrap();
            assert_eq!(registration.callback_token, expected_token);
            let instance_id = registration.instance_id.clone();
            let hub = PlatformAdapterHub::default();
            hub.register(generation, registration).unwrap();
            let response =
                unpeel_core::relay_wire::encode_tunnel_response(request.id, 200, br#"{"ok":true}"#);
            unpeel_core::remote_stdio::write_frame(
                &mut stream,
                unpeel_core::remote_stdio::FRAME_KIND_RESPONSE,
                &response,
            )
            .unwrap();
            let callback = hub.call("computer.status", serde_json::json!({})).unwrap();
            assert_eq!(callback.status, 200);
            assert_eq!(callback.body["ok"], true);
            instance_id
        }

        let home = std::env::temp_dir().join(format!(
            "unpeel-native-adapter-restart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let socket = unpeel_core::remote_stdio::local_host_socket_path(&home);
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let first_worker = UnixListener::bind(&socket).unwrap();

        let callback = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let callback_port = callback.local_addr().unwrap().port();
        let expected_token = "0123456789abcdef0123456789abcdef";
        let callback_thread = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = callback.accept().unwrap();
                let request = read_http_request(&mut stream);
                let request = String::from_utf8(request).unwrap();
                assert!(
                    request.contains("Authorization: Bearer 0123456789abcdef0123456789abcdef\r\n")
                );
                assert!(request.contains("\"operation\":\"computer.status\""));
                let body = br#"{"ok":true}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });

        let worker_socket = socket.clone();
        let worker = std::thread::spawn(move || {
            let first_instance = register_and_invoke(first_worker, 1, expected_token);
            std::fs::remove_file(&worker_socket).unwrap();
            let second_worker = UnixListener::bind(&worker_socket).unwrap();
            let second_instance = register_and_invoke(second_worker, 2, expected_token);
            assert_eq!(second_instance, first_instance);
            let _ = std::fs::remove_file(&worker_socket);
        });

        let config = serde_json::to_vec(&json!({
            "unpeelHome": home,
            "instanceID": "native-worker-restart-proof",
            "callbackPort": callback_port,
            "callbackToken": expected_token,
            "capabilities": ["computer.status"]
        }))
        .unwrap();
        let mut adapter = AdapterGuard(Some(start_platform_adapter_client(&config).unwrap()));
        worker.join().unwrap();
        callback_thread.join().unwrap();
        stop_platform_adapter_client(adapter.0.take().unwrap()).unwrap();
        assert_eq!(home.parent(), Some(std::env::temp_dir().as_path()));
        assert!(home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("unpeel-native-adapter-restart-")));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_host_control_uses_the_persistent_worker_socket() {
        let home = std::env::temp_dir().join(format!(
            "unpeel-native-control-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let socket = unpeel_core::remote_stdio::local_host_socket_path(&home);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let frame = unpeel_core::remote_stdio::read_frame(&mut stream)
                .unwrap()
                .unwrap();
            assert_eq!(frame.kind, unpeel_core::remote_stdio::FRAME_KIND_REQUEST);
            let request = unpeel_core::relay_wire::parse_tunnel_request(&frame.payload).unwrap();
            assert_eq!(request.path, "/_unpeel/pairing");
            assert_eq!(request.body, br#"{"action":"devices"}"#);
            unpeel_core::remote_stdio::write_frame(
                &mut stream,
                unpeel_core::remote_stdio::FRAME_KIND_RESPONSE,
                &unpeel_core::relay_wire::encode_tunnel_response(
                    request.id,
                    200,
                    br#"{"devices":[]}"#,
                ),
            )
            .unwrap();
        });
        let config = serde_json::to_vec(&json!({
            "unpeelHome": home,
            "request": { "action": "devices" }
        }))
        .unwrap();
        assert_eq!(local_host_control(&config).unwrap(), br#"{"devices":[]}"#);
        worker.join().unwrap();
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_dir(home);
    }

    struct StaticRemoteBackend {
        snapshot: RemoteBootstrapSnapshot,
        disconnected: Arc<AtomicBool>,
        panic_on_bootstrap: bool,
    }

    impl RegisteredRemoteBackend for StaticRemoteBackend {
        fn bootstrap_snapshot(&self) -> Result<RemoteBootstrapSnapshot, NativeRemoteError> {
            if self.panic_on_bootstrap {
                panic!("test remote backend panic");
            }
            Ok(self.snapshot.clone())
        }

        fn poll_output(
            &self,
            _session_id: &str,
            _options: RemoteOutputPollOptions,
        ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError> {
            Err(NativeRemoteError::remote(
                "test_output_unavailable",
                "static test backend has no output",
            ))
        }

        fn poll_output_from(
            &self,
            _session_id: &str,
            _requested_offset: Option<u64>,
            _options: RemoteOutputPollOptions,
        ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError> {
            Err(NativeRemoteError::remote(
                "test_output_unavailable",
                "static test backend has no output",
            ))
        }

        fn reset_output_cursor(&self, _session_id: &str) -> Result<(), NativeRemoteError> {
            Err(NativeRemoteError::remote(
                "test_output_unavailable",
                "static test backend has no output",
            ))
        }

        fn write_terminal(
            &self,
            _session_id: &str,
            _data: &str,
        ) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn resize_desktop(
            &self,
            _session_id: &str,
            _resize: RemoteDesktopResize,
        ) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn mark_session_read(&self, _session_id: &str) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn set_session_title(
            &self,
            _session_id: &str,
            _title: &str,
        ) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn set_session_pinned(
            &self,
            _session_id: &str,
            _pinned: bool,
        ) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn session_verb(
            &self,
            _verb: RemoteSessionVerb,
            _session_id: &str,
        ) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn set_session_order(
            &self,
            _project_id: &str,
            _ordered_session_ids: &[String],
        ) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn set_project_organization(
            &self,
            _project_id: &str,
            _patch: &RemoteProjectOrganizationPatch,
        ) -> Result<u64, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn create_session(
            &self,
            _request: &RemoteSessionCreateRequest,
        ) -> Result<NativeCreatedSession, NativeRemoteEffectError> {
            unreachable!("static test backend has no effects")
        }

        fn list_archived_sessions(
            &self,
            _project_id: &str,
        ) -> Result<Vec<RemoteSessionSummary>, NativeRemoteError> {
            Err(NativeRemoteError::remote(
                "test_reads_unavailable",
                "static test backend has no reads",
            ))
        }

        fn read_transcript_markdown(
            &self,
            _session_id: &str,
            _entries: Option<u32>,
        ) -> Result<RemoteTranscriptMarkdown, NativeRemoteError> {
            Err(NativeRemoteError::remote(
                "test_reads_unavailable",
                "static test backend has no reads",
            ))
        }

        fn read_session_metrics(
            &self,
            _session_id: &str,
        ) -> Result<RemoteSessionMetrics, NativeRemoteError> {
            Err(NativeRemoteError::remote(
                "test_reads_unavailable",
                "static test backend has no reads",
            ))
        }

        fn disconnect(&self) {
            self.disconnected.store(true, Ordering::Release);
        }
    }

    #[derive(Debug, Default)]
    struct TestOutputState {
        committed: Option<u64>,
        pending: bool,
        commits: usize,
        discards: usize,
        resets: usize,
    }

    struct TestRegisteredOutputPage {
        state: Arc<Mutex<TestOutputState>>,
        metadata: NativeRemoteOutputPageMetadata,
        bytes: Vec<u8>,
        resolved: bool,
    }

    impl TestRegisteredOutputPage {
        fn resolve(&mut self, commit: bool) {
            if self.resolved {
                return;
            }
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pending = false;
            if commit {
                state.committed = Some(self.metadata.next_offset);
                state.commits += 1;
            } else {
                state.discards += 1;
            }
            self.resolved = true;
        }
    }

    impl RegisteredRemoteOutputPage for TestRegisteredOutputPage {
        fn metadata(&self) -> NativeRemoteOutputPageMetadata {
            self.metadata.clone()
        }

        fn bytes(&self) -> &[u8] {
            &self.bytes
        }

        fn commit(mut self: Box<Self>) -> Result<(), NativeRemoteError> {
            self.resolve(true);
            Ok(())
        }

        fn discard(mut self: Box<Self>) {
            self.resolve(false);
        }
    }

    impl Drop for TestRegisteredOutputPage {
        fn drop(&mut self) {
            self.resolve(false);
        }
    }

    struct TestInteractiveRemoteBackend {
        output: Arc<Mutex<TestOutputState>>,
        disconnected: Arc<AtomicBool>,
        effects: Arc<Mutex<Vec<String>>>,
        panic_on_poll: bool,
        write_failure_kind: Option<&'static str>,
    }

    impl RegisteredRemoteBackend for TestInteractiveRemoteBackend {
        fn bootstrap_snapshot(&self) -> Result<RemoteBootstrapSnapshot, NativeRemoteError> {
            Ok(remote_snapshot())
        }

        fn poll_output(
            &self,
            session_id: &str,
            _options: RemoteOutputPollOptions,
        ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError> {
            if self.panic_on_poll {
                panic!("test output poll panic");
            }
            let mut state = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.pending {
                return Err(NativeRemoteError::remote(
                    "output_page_pending",
                    "test page is still pending",
                ));
            }
            let requested_offset = state.committed;
            let offset = requested_offset.unwrap_or(0);
            let bytes = b"hello".to_vec();
            state.pending = true;
            drop(state);
            Ok(Box::new(TestRegisteredOutputPage {
                state: Arc::clone(&self.output),
                metadata: NativeRemoteOutputPageMetadata {
                    session_id: session_id.to_owned(),
                    requested_offset,
                    offset,
                    next_offset: offset + bytes.len() as u64,
                    reset_before_feed: requested_offset.is_none(),
                    truncated: false,
                    captured_at_unix_ms: 1234,
                    byte_count: bytes.len(),
                },
                bytes,
                resolved: false,
            }))
        }

        fn poll_output_from(
            &self,
            session_id: &str,
            requested_offset: Option<u64>,
            options: RemoteOutputPollOptions,
        ) -> Result<Box<dyn RegisteredRemoteOutputPage>, NativeRemoteError> {
            {
                let mut state = self
                    .output
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.committed = requested_offset;
                state.pending = false;
            }
            self.poll_output(session_id, options)
        }

        fn reset_output_cursor(&self, _session_id: &str) -> Result<(), NativeRemoteError> {
            let mut state = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.committed = None;
            state.pending = false;
            state.resets += 1;
            Ok(())
        }

        fn write_terminal(
            &self,
            session_id: &str,
            data: &str,
        ) -> Result<u64, NativeRemoteEffectError> {
            if let Some(kind) = self.write_failure_kind {
                return Err(NativeRemoteEffectError {
                    result: ERROR_REMOTE,
                    kind,
                    code: "host_connection_disconnected",
                    operation: "terminal write",
                    message: "terminal write may have landed; refresh before retrying".into(),
                });
            }
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("write:{session_id}:{data}"));
            Ok(41)
        }

        fn resize_desktop(
            &self,
            session_id: &str,
            resize: RemoteDesktopResize,
        ) -> Result<u64, NativeRemoteEffectError> {
            let (description, request_id) = match resize {
                RemoteDesktopResize::Fit { columns, rows } => {
                    (format!("fit:{session_id}:{columns}x{rows}"), 42)
                }
                RemoteDesktopResize::Clear => (format!("clear:{session_id}"), 43),
            };
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(description);
            Ok(request_id)
        }

        fn mark_session_read(&self, session_id: &str) -> Result<u64, NativeRemoteEffectError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("read:{session_id}"));
            Ok(44)
        }

        fn set_session_title(
            &self,
            session_id: &str,
            title: &str,
        ) -> Result<u64, NativeRemoteEffectError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("title:{session_id}:{title}"));
            Ok(45)
        }

        fn set_session_pinned(
            &self,
            session_id: &str,
            pinned: bool,
        ) -> Result<u64, NativeRemoteEffectError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("pin:{session_id}:{pinned}"));
            Ok(46)
        }

        fn set_session_notify_when_done(
            &self,
            session_id: &str,
            enabled: bool,
        ) -> Result<u64, NativeRemoteEffectError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("notify:{session_id}:{enabled}"));
            Ok(51)
        }

        fn session_verb(
            &self,
            verb: RemoteSessionVerb,
            session_id: &str,
        ) -> Result<u64, NativeRemoteEffectError> {
            if let Some(kind) = self.write_failure_kind {
                return Err(NativeRemoteEffectError {
                    result: ERROR_REMOTE,
                    kind,
                    code: "host_connection_disconnected",
                    operation: verb.operation(),
                    message: "verb may have landed; refresh before retrying".into(),
                });
            }
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("{}:{session_id}", verb.operation()));
            Ok(47)
        }

        fn set_session_order(
            &self,
            project_id: &str,
            ordered_session_ids: &[String],
        ) -> Result<u64, NativeRemoteEffectError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!(
                    "order:{project_id}:{}",
                    ordered_session_ids.join(",")
                ));
            Ok(48)
        }

        fn set_project_organization(
            &self,
            project_id: &str,
            patch: &RemoteProjectOrganizationPatch,
        ) -> Result<u64, NativeRemoteEffectError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!(
                    "project-organization:{project_id}:{:?}:{:?}:{:?}:{:?}",
                    patch.sort_order, patch.display_name, patch.color_id, patch.date_sorted
                ));
            Ok(50)
        }

        fn create_session(
            &self,
            request: &RemoteSessionCreateRequest,
        ) -> Result<NativeCreatedSession, NativeRemoteEffectError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!(
                    "create:{}:{}",
                    request.project_id,
                    request.preset_id.as_deref().unwrap_or("-")
                ));
            let session: RemoteSessionSummary = serde_json::from_value(json!({
                "id": "session-created",
                "projectID": request.project_id,
                "title": "New Session",
                "command": "claude",
                "createdAtUnixMs": 1234,
                "status": "running",
                "activity": "starting",
                "unread": false,
                "pinned": false
            }))
            .unwrap();
            Ok(NativeCreatedSession {
                request_id: 49,
                session_id: "session-created".into(),
                captured_at_unix_ms: Some(1234),
                session: Some(session),
            })
        }

        fn pairing_invitation(
            &self,
            request_json: &[u8],
        ) -> Result<Vec<u8>, NativeRemoteEffectError> {
            let request: Value = serde_json::from_slice(request_json).unwrap();
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!(
                    "pairing:{}",
                    request["action"].as_str().unwrap_or("-")
                ));
            Ok(br#"{"protocolVersion":1,"macID":"host-1"}"#.to_vec())
        }

        fn list_archived_sessions(
            &self,
            project_id: &str,
        ) -> Result<Vec<RemoteSessionSummary>, NativeRemoteError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("archived:{project_id}"));
            Ok(vec![serde_json::from_value(json!({
                "id": "archived-1",
                "projectID": project_id,
                "title": "Archived",
                "command": "claude",
                "createdAtUnixMs": 1200,
                "status": "exited",
                "activity": "idle",
                "archived": true
            }))
            .unwrap()])
        }

        fn read_transcript_markdown(
            &self,
            session_id: &str,
            entries: Option<u32>,
        ) -> Result<RemoteTranscriptMarkdown, NativeRemoteError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!(
                    "transcript:{session_id}:{}",
                    entries.map(|value| value.to_string()).unwrap_or("-".into())
                ));
            Ok(RemoteTranscriptMarkdown {
                session_id: session_id.to_owned(),
                markdown: "# Transcript".into(),
            })
        }

        fn read_session_metrics(
            &self,
            session_id: &str,
        ) -> Result<RemoteSessionMetrics, NativeRemoteError> {
            self.effects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(format!("metrics:{session_id}"));
            Ok(RemoteSessionMetrics {
                session_id: session_id.to_owned(),
                columns: 120,
                rows: 34,
                output_offset: Some(4096),
                captured_at_unix_ms: 1234,
            })
        }

        fn disconnect(&self) {
            self.disconnected.store(true, Ordering::Release);
        }
    }

    struct TestRemoteControls {
        output: Arc<Mutex<TestOutputState>>,
        disconnected: Arc<AtomicBool>,
        effects: Arc<Mutex<Vec<String>>>,
    }

    fn register_interactive_remote(
        panic_on_poll: bool,
        write_failure_kind: Option<&'static str>,
    ) -> (RemoteHandle, TestRemoteControls) {
        let controls = TestRemoteControls {
            output: Arc::new(Mutex::new(TestOutputState::default())),
            disconnected: Arc::new(AtomicBool::new(false)),
            effects: Arc::new(Mutex::new(Vec::new())),
        };
        let handle = register_remote_backend(Arc::new(TestInteractiveRemoteBackend {
            output: Arc::clone(&controls.output),
            disconnected: Arc::clone(&controls.disconnected),
            effects: Arc::clone(&controls.effects),
            panic_on_poll,
            write_failure_kind,
        }))
        .unwrap();
        (handle, controls)
    }

    fn remote_snapshot() -> RemoteBootstrapSnapshot {
        serde_json::from_value(json!({
            "protocolVersion": 1,
            "hostProtocol": HostProtocolDescriptor::headless_v1(),
            "macID": "host-1",
            "macName": "Studio",
            "folders": [],
            "projects": [],
            "presets": [],
            "sessions": [{
                "id": "session-1",
                "projectID": "project-1",
                "activeRuntimeID": "claude",
                "title": "Shell",
                "command": "",
                "createdAtUnixMs": 1200,
                "status": "running",
                "activity": "working"
            }],
            "paneGroups": [{
                "id": "pane-group-1",
                "representativeSessionID": "session-1",
                "sessionIDs": ["session-1", "session-2"]
            }],
            "capturedAtUnixMs": 1234,
            "remoteServerPort": 43117,
            "unexpectedWireField": "must not cross the typed boundary"
        }))
        .unwrap()
    }

    unsafe fn take_owned_json(pointer: *mut u8, length: usize) -> Value {
        assert!(!pointer.is_null());
        assert!(length > 0);
        let bytes = std::slice::from_raw_parts(pointer, length).to_vec();
        unpeel_native_bridge_free(pointer, length);
        serde_json::from_slice(&bytes).unwrap()
    }

    unsafe fn take_owned_bytes(pointer: *mut u8, length: usize) -> Vec<u8> {
        if pointer.is_null() {
            assert_eq!(length, 0);
            return Vec::new();
        }
        let bytes = std::slice::from_raw_parts(pointer, length).to_vec();
        unpeel_native_bridge_free(pointer, length);
        bytes
    }

    unsafe fn poll_ffi_raw(
        handle: RemoteHandle,
        session_id: &[u8],
    ) -> (i32, RemoteOutputPageHandle, *mut u8, usize, *mut u8, usize) {
        let mut page_handle = 0;
        let mut metadata_pointer = ptr::null_mut();
        let mut metadata_length = 0;
        let mut bytes_pointer = ptr::null_mut();
        let mut bytes_length = 0;
        let code = unpeel_native_bridge_remote_output_poll(
            handle,
            session_id.as_ptr(),
            session_id.len(),
            4096,
            0,
            &mut page_handle,
            &mut metadata_pointer,
            &mut metadata_length,
            &mut bytes_pointer,
            &mut bytes_length,
        );
        (
            code,
            page_handle,
            metadata_pointer,
            metadata_length,
            bytes_pointer,
            bytes_length,
        )
    }

    unsafe fn poll_from_ffi_raw(
        handle: RemoteHandle,
        session_id: &[u8],
        requested_offset: Option<u64>,
    ) -> (i32, RemoteOutputPageHandle, *mut u8, usize, *mut u8, usize) {
        let mut page_handle = 0;
        let mut metadata_pointer = ptr::null_mut();
        let mut metadata_length = 0;
        let mut bytes_pointer = ptr::null_mut();
        let mut bytes_length = 0;
        let code = unpeel_native_bridge_remote_output_poll_from(
            handle,
            session_id.as_ptr(),
            session_id.len(),
            requested_offset.unwrap_or(0),
            u8::from(requested_offset.is_some()),
            4096,
            0,
            &mut page_handle,
            &mut metadata_pointer,
            &mut metadata_length,
            &mut bytes_pointer,
            &mut bytes_length,
        );
        (
            code,
            page_handle,
            metadata_pointer,
            metadata_length,
            bytes_pointer,
            bytes_length,
        )
    }

    unsafe fn resolve_page_ffi(
        handle: RemoteHandle,
        page_handle: RemoteOutputPageHandle,
        commit: bool,
    ) -> (i32, *mut u8, usize) {
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = if commit {
            unpeel_native_bridge_remote_output_commit(
                handle,
                page_handle,
                &mut pointer,
                &mut length,
            )
        } else {
            unpeel_native_bridge_remote_output_discard(
                handle,
                page_handle,
                &mut pointer,
                &mut length,
            )
        };
        (code, pointer, length)
    }

    unsafe fn close_ffi(handle: RemoteHandle) -> (i32, *mut u8, usize) {
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unpeel_native_bridge_remote_close(handle, &mut pointer, &mut length);
        (code, pointer, length)
    }

    unsafe fn direct_open_ffi(
        endpoint: &[u8],
        bearer: &[u8],
    ) -> (i32, RemoteHandle, *mut u8, usize) {
        let mut handle = 0;
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unpeel_native_bridge_remote_direct_open(
            endpoint.as_ptr(),
            endpoint.len(),
            bearer.as_ptr(),
            bearer.len(),
            &mut handle,
            &mut pointer,
            &mut length,
        );
        (code, handle, pointer, length)
    }

    struct TestRelayCallbackContext {
        disconnects: Arc<AtomicU64>,
        releases: Arc<AtomicU64>,
    }

    unsafe extern "C" fn test_relay_request_callback(
        _context: *mut c_void,
        _request_pointer: *const u8,
        _request_length: usize,
        _required_generation: u64,
        _timeout_ms: u64,
        out_generation: *mut u64,
        out_pointer: *mut *mut u8,
        out_length: *mut usize,
    ) -> i32 {
        *out_generation = 0;
        *out_pointer = ptr::null_mut();
        *out_length = 0;
        RELAY_CALLBACK_NOT_SENT
    }

    unsafe extern "C" fn test_relay_bytes_release_callback(
        _context: *mut c_void,
        pointer: *mut u8,
        length: usize,
    ) {
        if pointer.is_null() || length == 0 {
            return;
        }
        let slice = ptr::slice_from_raw_parts_mut(pointer, length);
        drop(Box::from_raw(slice));
    }

    unsafe extern "C" fn test_relay_disconnect_callback(context: *mut c_void) {
        let context = &*(context as *const TestRelayCallbackContext);
        context.disconnects.fetch_add(1, Ordering::Relaxed);
    }

    unsafe extern "C" fn test_relay_context_release_callback(context: *mut c_void) {
        let context = Box::from_raw(context as *mut TestRelayCallbackContext);
        context.releases.fetch_add(1, Ordering::Relaxed);
    }

    struct TestRelayDeliveryContext {
        result_code: i32,
        generation: u64,
        bytes: Vec<u8>,
        required_generation: Arc<AtomicU64>,
        timeout_ms: Arc<AtomicU64>,
        releases: Arc<AtomicU64>,
    }

    unsafe extern "C" fn test_relay_delivery_request_callback(
        context: *mut c_void,
        _request_pointer: *const u8,
        _request_length: usize,
        required_generation: u64,
        timeout_ms: u64,
        out_generation: *mut u64,
        out_pointer: *mut *mut u8,
        out_length: *mut usize,
    ) -> i32 {
        let context = &*(context as *const TestRelayDeliveryContext);
        context
            .required_generation
            .store(required_generation, Ordering::Relaxed);
        context.timeout_ms.store(timeout_ms, Ordering::Relaxed);
        *out_generation = context.generation;
        if context.bytes.is_empty() {
            *out_pointer = ptr::null_mut();
            *out_length = 0;
        } else {
            let mut bytes = context.bytes.clone().into_boxed_slice();
            *out_length = bytes.len();
            *out_pointer = bytes.as_mut_ptr();
            std::mem::forget(bytes);
        }
        context.result_code
    }

    unsafe extern "C" fn test_relay_delivery_context_release_callback(context: *mut c_void) {
        let context = Box::from_raw(context as *mut TestRelayDeliveryContext);
        context.releases.fetch_add(1, Ordering::Relaxed);
    }

    unsafe fn relay_open_ffi(
        bearer: &[u8],
        context: *mut c_void,
    ) -> (i32, RemoteHandle, *mut u8, usize) {
        let mut handle = 0;
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unpeel_native_bridge_remote_relay_open(
            bearer.as_ptr(),
            bearer.len(),
            context,
            Some(test_relay_request_callback),
            Some(test_relay_bytes_release_callback),
            Some(test_relay_disconnect_callback),
            Some(test_relay_context_release_callback),
            &mut handle,
            &mut pointer,
            &mut length,
        );
        (code, handle, pointer, length)
    }

    fn request(path: &str) -> Vec<u8> {
        serde_json::to_vec(&ControllerRequest {
            id: Some("native-request".into()),
            method: "GET".into(),
            path: path.into(),
            query: HashMap::new(),
            body: Value::Null,
            content_type: None,
            body_base64: None,
            principal: ControllerPrincipal::PairedDevice {
                device_id: "phone-1".into(),
                name: "Phone".into(),
                principal_id: None,
            },
        })
        .unwrap()
    }

    #[test]
    fn native_bootstrap_uses_rust_owned_capabilities_and_metadata() {
        let context = serde_json::to_vec(&json!({
            "snapshot": {
                "protocolVersion": 99,
                "hostProtocol": { "majorVersion": 99 },
                "sessions": []
            },
            "hostID": "host-1"
        }))
        .unwrap();
        let (code, bytes) = guarded_route(&request("/mobile/bootstrap"), Some(&context));
        assert_eq!(code, RESULT_HANDLED);
        let response: ControllerResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.id.as_deref(), Some("native-request"));
        assert_eq!(response.body["protocolVersion"], 1);
        assert_eq!(response.body["macID"], "host-1");
        assert!(response.body["hostProtocol"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "session.create"));
    }

    #[test]
    fn generalized_context_routes_archives_without_leaking_them_into_bootstrap() {
        let context = serde_json::to_vec(&json!({
            "bootstrap": {
                "snapshot": { "sessions": [] },
                "hostID": "host-1"
            },
            "archivedSessionsByProject": {
                "project-1": [{
                    "id": "session-1",
                    "title": "Archived secret"
                }]
            }
        }))
        .unwrap();
        let mut archive_request: ControllerRequest =
            serde_json::from_slice(&request("/mobile/archive")).unwrap();
        archive_request
            .query
            .insert("project_id".into(), "project-1".into());
        let archive_request = serde_json::to_vec(&archive_request).unwrap();

        let (code, bytes) = guarded_route(&archive_request, Some(&context));
        assert_eq!(code, RESULT_HANDLED);
        let response: ControllerResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body["projectID"], "project-1");
        assert_eq!(response.body["sessions"][0]["id"], "session-1");

        let (code, bytes) = guarded_route(&request("/mobile/bootstrap"), Some(&context));
        assert_eq!(code, RESULT_HANDLED);
        let response: ControllerResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body["macID"], "host-1");
        assert!(!response.body.to_string().contains("Archived secret"));
        assert!(response.body.get("archivedSessionsByProject").is_none());
    }

    #[test]
    fn generalized_context_distinguishes_known_empty_and_unknown_archives() {
        let context = serde_json::to_vec(&json!({
            "archivedSessionsByProject": { "project-1": [] }
        }))
        .unwrap();
        let mut archive_request: ControllerRequest =
            serde_json::from_slice(&request("/mobile/archive")).unwrap();
        archive_request
            .query
            .insert("project_id".into(), "project-1".into());

        let (code, bytes) = guarded_route(
            &serde_json::to_vec(&archive_request).unwrap(),
            Some(&context),
        );
        assert_eq!(code, RESULT_HANDLED);
        let response: ControllerResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body,
            json!({ "projectID": "project-1", "sessions": [] })
        );

        archive_request
            .query
            .insert("project_id".into(), "unknown".into());
        let (code, bytes) = guarded_route(
            &serde_json::to_vec(&archive_request).unwrap(),
            Some(&context),
        );
        assert_eq!(code, RESULT_HANDLED);
        let response: ControllerResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.body["error"], "unknown project");
    }

    #[test]
    fn handled_unhandled_and_invalid_inputs_are_distinct() {
        let (handled, _) = guarded_route(&request("/mobile/metrics"), None);
        assert_eq!(handled, RESULT_HANDLED);
        let (unhandled, bytes) = guarded_route(&request("/mobile/not-migrated"), None);
        assert_eq!(unhandled, RESULT_UNHANDLED);
        assert!(bytes.is_empty());
        let (invalid, bytes) = guarded_route(b"not json", None);
        assert_eq!(invalid, ERROR_INVALID_INPUT);
        assert!(String::from_utf8(bytes)
            .unwrap()
            .contains("invalid controller request"));
    }

    #[test]
    fn panic_guard_never_unwinds_to_the_caller() {
        let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<RouteOutcome, String> {
            panic!("test panic")
        }));
        assert!(outcome.is_err());
        let (code, body) = match outcome {
            Ok(_) => unreachable!(),
            Err(_) => (
                ERROR_PANIC,
                br#"{"error":"Rust controller bridge panicked"}"#.to_vec(),
            ),
        };
        assert_eq!(code, ERROR_PANIC);
        assert!(String::from_utf8(body).unwrap().contains("panicked"));
    }

    #[test]
    fn ffi_allocates_and_frees_owned_output() {
        let request = request("/mobile/metrics");
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unsafe {
            unpeel_native_bridge_route(
                request.as_ptr(),
                request.len(),
                ptr::null(),
                0,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_HANDLED);
        assert!(!pointer.is_null());
        assert!(length > 0);
        let response = unsafe { std::slice::from_raw_parts(pointer, length) };
        assert_eq!(
            serde_json::from_slice::<Value>(response).unwrap()["status"],
            400
        );
        unsafe { unpeel_native_bridge_free(pointer, length) };
    }

    #[test]
    fn remote_ssh_open_returns_an_opaque_registry_handle_and_close_is_final() {
        let target = b"ssh://studio";
        let mut handle = 0;
        let mut pointer = std::ptr::dangling_mut::<u8>();
        let mut length = usize::MAX;
        let code = unsafe {
            unpeel_native_bridge_remote_ssh_open(
                target.as_ptr(),
                target.len(),
                &mut handle,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_HANDLED);
        assert_ne!(handle, 0);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert!(remote_backend(handle).is_ok());

        let (code, pointer, length) = unsafe { close_ffi(handle) };
        assert_eq!(code, RESULT_HANDLED);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert!(remote_backend(handle).is_err());

        let (code, pointer, length) = unsafe { close_ffi(handle) };
        assert_eq!(code, ERROR_INVALID_HANDLE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "invalid_remote_handle");
        assert!(error["error"]
            .as_str()
            .unwrap()
            .contains("open the Host again"));
    }

    #[test]
    fn remote_ssh_open_rejects_unsafe_targets_with_actionable_json() {
        let target = b"ssh://studio:2222";
        let mut handle = u64::MAX;
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_ssh_open(
                target.as_ptr(),
                target.len(),
                &mut handle,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, ERROR_INVALID_INPUT);
        assert_eq!(handle, 0);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "invalid_ssh_target");
        assert!(error["error"]
            .as_str()
            .unwrap()
            .contains("SSH config alias"));
    }

    #[test]
    fn remote_ssh_config_open_accepts_interactive_askpass_without_returning_secret() {
        let config = br#"{
            "target":"ssh://managed",
            "mode":"interactiveShell",
            "askpassProgram":"/absolute/unpeel-host",
            "secret":"provider-api-key"
        }"#;
        let mut handle = 0;
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_ssh_config_open(
                config.as_ptr(),
                config.len(),
                &mut handle,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_OK);
        assert_ne!(handle, 0);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_local_gateway_open_validates_paths_and_registers_lazily() {
        let config = br#"{
            "hostProgram":"/bundle/Contents/MacOS/unpeel-host",
            "unpeelHome":"/homes/.unpeel/profiles/writing"
        }"#;
        let mut handle = 0;
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_local_gateway_open(
                config.as_ptr(),
                config.len(),
                &mut handle,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_OK);
        assert_ne!(handle, 0);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert!(remote_backend(handle).is_ok());
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
        assert!(remote_backend(handle).is_err());

        let required = br#"{
            "hostProgram":"/bundle/Contents/MacOS/unpeel-host",
            "unpeelHome":"/homes/.unpeel/profiles/writing",
            "requireHostService":true
        }"#;
        let mut required_handle = 0;
        let mut required_pointer = ptr::null_mut();
        let mut required_length = 0;
        let required_code = unsafe {
            unpeel_native_bridge_remote_local_gateway_open(
                required.as_ptr(),
                required.len(),
                &mut required_handle,
                &mut required_pointer,
                &mut required_length,
            )
        };
        assert_eq!(required_code, RESULT_OK);
        assert_ne!(required_handle, 0);
        assert!(required_pointer.is_null());
        assert_eq!(required_length, 0);
        assert_eq!(unsafe { close_ffi(required_handle) }.0, RESULT_OK);

        for config in [
            br#"{"hostProgram":"unpeel-host","unpeelHome":"/homes/w"}"#.as_slice(),
            br#"{"hostProgram":"/bundle/unpeel-host","unpeelHome":"profiles/w"}"#.as_slice(),
            br#"{"hostProgram":"/bundle/unpeel-host"}"#.as_slice(),
        ] {
            let mut handle = u64::MAX;
            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let code = unsafe {
                unpeel_native_bridge_remote_local_gateway_open(
                    config.as_ptr(),
                    config.len(),
                    &mut handle,
                    &mut pointer,
                    &mut length,
                )
            };
            assert_eq!(code, ERROR_INVALID_INPUT);
            assert_eq!(handle, 0);
            let error = unsafe { take_owned_json(pointer, length) };
            assert_eq!(error["code"], "invalid_local_gateway_config");
        }
    }

    #[test]
    fn remote_direct_open_registers_without_network_io_and_closes_finally() {
        // Port 1 is deliberately not serving a Host. Open still succeeds
        // because DirectHostConnection performs its first I/O at bootstrap.
        let (code, handle, pointer, length) =
            unsafe { direct_open_ffi(b"http://127.0.0.1:1/mobile", b"paired-device-secret") };
        assert_eq!(code, RESULT_OK);
        assert_ne!(handle, 0);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert!(remote_backend(handle).is_ok());

        let (code, pointer, length) = unsafe { close_ffi(handle) };
        assert_eq!(code, RESULT_OK);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert!(remote_backend(handle).is_err());
    }

    #[test]
    fn remote_direct_open_rejects_endpoints_outside_exact_http_mobile_scope() {
        for endpoint in [
            b"https://studio.local/mobile".as_slice(),
            b"http://studio.local/".as_slice(),
            b"http://studio.local/mobile?token=bad".as_slice(),
        ] {
            let (code, handle, pointer, length) =
                unsafe { direct_open_ffi(endpoint, b"paired-device-secret") };
            assert_eq!(code, ERROR_INVALID_INPUT);
            assert_eq!(handle, 0);
            let error = unsafe { take_owned_json(pointer, length) };
            assert_eq!(error["code"], "invalid_host_endpoint");
            assert!(error["error"].as_str().unwrap().contains("/mobile"));
        }

        let invalid_utf8 = [0xff];
        let (code, handle, pointer, length) =
            unsafe { direct_open_ffi(&invalid_utf8, b"paired-device-secret") };
        assert_eq!(code, ERROR_INVALID_INPUT);
        assert_eq!(handle, 0);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "invalid_host_endpoint_utf8");
    }

    #[test]
    fn remote_direct_open_rejects_empty_invalid_or_non_utf8_bearers_without_echoing_them() {
        let endpoint = b"http://studio.local:43117/mobile";
        let cases: [&[u8]; 3] = [b"", b"secret must never echo", &[0xff]];
        for bearer in cases {
            let (code, handle, pointer, length) = unsafe { direct_open_ffi(endpoint, bearer) };
            assert_eq!(code, ERROR_INVALID_INPUT);
            assert_eq!(handle, 0);
            let bytes = unsafe { take_owned_bytes(pointer, length) };
            let error: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(matches!(
                error["code"].as_str(),
                Some("invalid_host_bearer" | "invalid_host_bearer_utf8")
            ));
            if let Ok(secret) = std::str::from_utf8(bearer) {
                if !secret.is_empty() {
                    assert!(!String::from_utf8_lossy(&bytes).contains(secret));
                }
            }
        }
    }

    #[test]
    fn remote_relay_open_owns_context_until_close_and_releases_it_exactly_once() {
        let disconnects = Arc::new(AtomicU64::new(0));
        let releases = Arc::new(AtomicU64::new(0));
        let context = Box::into_raw(Box::new(TestRelayCallbackContext {
            disconnects: Arc::clone(&disconnects),
            releases: Arc::clone(&releases),
        })) as *mut c_void;

        let (code, handle, pointer, length) =
            unsafe { relay_open_ffi(b"paired-device-secret", context) };
        assert_eq!(code, RESULT_OK);
        assert_ne!(handle, 0);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert_eq!(disconnects.load(Ordering::Relaxed), 0);
        assert_eq!(releases.load(Ordering::Relaxed), 0);

        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
        assert_eq!(disconnects.load(Ordering::Relaxed), 1);
        assert_eq!(releases.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn remote_relay_open_rejects_bad_bearers_without_echo_and_releases_context() {
        for bearer in [
            b"".as_slice(),
            b"secret must never echo".as_slice(),
            &[0xff],
        ] {
            let disconnects = Arc::new(AtomicU64::new(0));
            let releases = Arc::new(AtomicU64::new(0));
            let context = Box::into_raw(Box::new(TestRelayCallbackContext {
                disconnects: Arc::clone(&disconnects),
                releases: Arc::clone(&releases),
            })) as *mut c_void;

            let (code, handle, pointer, length) = unsafe { relay_open_ffi(bearer, context) };
            assert_eq!(code, ERROR_INVALID_INPUT);
            assert_eq!(handle, 0);
            let bytes = unsafe { take_owned_bytes(pointer, length) };
            let error: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(matches!(
                error["code"].as_str(),
                Some("invalid_link_bearer" | "invalid_link_bearer_utf8")
            ));
            if let Ok(secret) = std::str::from_utf8(bearer) {
                if !secret.is_empty() {
                    assert!(!String::from_utf8_lossy(&bytes).contains(secret));
                }
            }
            assert_eq!(disconnects.load(Ordering::Relaxed), 0);
            assert_eq!(releases.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn relay_callback_executor_forwards_generation_and_preserves_delivery_certainty() {
        let required_generation = Arc::new(AtomicU64::new(0));
        let timeout_ms = Arc::new(AtomicU64::new(0));
        let releases = Arc::new(AtomicU64::new(0));
        let context = Box::into_raw(Box::new(TestRelayDeliveryContext {
            result_code: RELAY_CALLBACK_TIMED_OUT_NOT_SENT,
            generation: 0,
            bytes: b"must not become a transport message".to_vec(),
            required_generation: Arc::clone(&required_generation),
            timeout_ms: Arc::clone(&timeout_ms),
            releases: Arc::clone(&releases),
        })) as *mut c_void;
        let executor = CallbackRelayExecutor {
            context: context as usize,
            request_callback: test_relay_delivery_request_callback,
            bytes_release_callback: test_relay_bytes_release_callback,
            disconnect_callback: test_relay_disconnect_callback,
            context_release_callback: test_relay_delivery_context_release_callback,
        };

        let error = executor
            .request(b"request", Some(41), std::time::Duration::from_secs(35))
            .unwrap_err();
        assert_eq!(
            error,
            RelayTransportError::TimedOut {
                delivery: DeliveryState::NotSent
            }
        );
        assert_eq!(required_generation.load(Ordering::Relaxed), 41);
        assert_eq!(timeout_ms.load(Ordering::Relaxed), 35_000);
        assert_eq!(releases.load(Ordering::Relaxed), 0);
        drop(executor);
        assert_eq!(releases.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn remote_direct_bootstrap_uses_the_shared_typed_backend() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
                assert!(request.len() < 32 * 1024);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /mobile/bootstrap HTTP/1.1\r\n"));
            assert!(request.contains("Authorization: Bearer paired-device-secret\r\n"));

            let body = serde_json::to_vec(&remote_snapshot()).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });

        let endpoint = format!("http://{address}/mobile");
        let (code, handle, pointer, length) =
            unsafe { direct_open_ffi(endpoint.as_bytes(), b"paired-device-secret") };
        assert_eq!(code, RESULT_OK);
        assert!(pointer.is_null());
        assert_eq!(length, 0);

        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code =
            unsafe { unpeel_native_bridge_remote_bootstrap(handle, &mut pointer, &mut length) };
        assert_eq!(code, RESULT_OK);
        let snapshot = unsafe { take_owned_json(pointer, length) };
        assert_eq!(snapshot["macID"], "host-1");
        assert_eq!(snapshot["macName"], "Studio");
        assert_eq!(snapshot["capturedAtUnixMs"], 1234);
        assert_eq!(snapshot["sessions"][0]["activeRuntimeID"], "claude");
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
        server.join().unwrap();
    }

    #[test]
    fn remote_bootstrap_connection_errors_name_the_target_and_recovery() {
        let transport = RegisteredRemoteTransport::Ssh {
            target_uri: "ssh://studio".into(),
        };
        let error = remote_bootstrap_error(
            &transport,
            RemoteSessionBackendError::Connection(HostConnectionError::Launch {
                request_id: 7,
                message: "remote command not found".into(),
            }),
        );
        assert_eq!(error.result, ERROR_REMOTE);
        assert_eq!(error.code, "host_connection_launch_failed");
        assert!(error.message.contains("ssh://studio"));
        assert!(error.message.contains("non-interactive SSH"));
        assert!(error.message.contains("`unpeel-host`"));
    }

    #[test]
    fn remote_bootstrap_returns_owned_typed_json_and_close_disconnects() {
        let disconnected = Arc::new(AtomicBool::new(false));
        let handle = register_remote_backend(Arc::new(StaticRemoteBackend {
            snapshot: remote_snapshot(),
            disconnected: Arc::clone(&disconnected),
            panic_on_bootstrap: false,
        }))
        .unwrap();

        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code =
            unsafe { unpeel_native_bridge_remote_bootstrap(handle, &mut pointer, &mut length) };
        assert_eq!(code, RESULT_HANDLED);

        // The allocation is independent of the registry entry and remains
        // readable after the SSH/backend owner has been disconnected.
        let (close_code, close_pointer, close_length) = unsafe { close_ffi(handle) };
        assert_eq!(close_code, RESULT_HANDLED);
        assert!(close_pointer.is_null());
        assert_eq!(close_length, 0);
        assert!(disconnected.load(Ordering::Acquire));

        let snapshot = unsafe { take_owned_json(pointer, length) };
        assert_eq!(snapshot["protocolVersion"], 1);
        assert_eq!(snapshot["macID"], "host-1");
        assert_eq!(snapshot["macName"], "Studio");
        assert_eq!(snapshot["capturedAtUnixMs"], 1234);
        assert_eq!(snapshot["sessions"][0]["activeRuntimeID"], "claude");
        assert_eq!(
            snapshot["paneGroups"][0]["representativeSessionID"],
            "session-1"
        );
        assert_eq!(snapshot["paneGroups"][0]["sessionIDs"][1], "session-2");
        assert!(snapshot["sessions"][0]["providerID"].is_null());
        assert!(snapshot["hostProtocol"]["capabilities"].is_array());
        assert!(snapshot.get("unexpectedWireField").is_none());
    }

    #[test]
    fn remote_bootstrap_panics_are_contained_and_the_handle_can_close() {
        let disconnected = Arc::new(AtomicBool::new(false));
        let handle = register_remote_backend(Arc::new(StaticRemoteBackend {
            snapshot: remote_snapshot(),
            disconnected: Arc::clone(&disconnected),
            panic_on_bootstrap: true,
        }))
        .unwrap();
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code =
            unsafe { unpeel_native_bridge_remote_bootstrap(handle, &mut pointer, &mut length) };
        assert_eq!(code, ERROR_PANIC);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "remote_bridge_panicked");

        let (code, pointer, length) = unsafe { close_ffi(handle) };
        assert_eq!(code, RESULT_HANDLED);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        assert!(disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn remote_output_poll_commit_advances_only_after_explicit_commit() {
        let (handle, controls) = register_interactive_remote(false, None);
        let (code, page_handle, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_ffi_raw(handle, b"s1") };
        assert_eq!(code, RESULT_OK);
        assert_ne!(page_handle, 0);
        let metadata = unsafe { take_owned_json(metadata_pointer, metadata_length) };
        let bytes = unsafe { take_owned_bytes(bytes_pointer, bytes_length) };
        assert_eq!(metadata["sessionID"], "s1");
        assert_eq!(metadata["requestedOffset"], Value::Null);
        assert_eq!(metadata["offset"], 0);
        assert_eq!(metadata["nextOffset"], 5);
        assert_eq!(metadata["resetBeforeFeed"], true);
        assert_eq!(metadata["byteCount"], 5);
        assert_eq!(bytes, b"hello");
        {
            let state = controls.output.lock().unwrap();
            assert_eq!(state.committed, None);
            assert!(state.pending);
        }

        let (code, pointer, length) = unsafe { resolve_page_ffi(handle, page_handle, true) };
        assert_eq!(code, RESULT_OK);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        {
            let state = controls.output.lock().unwrap();
            assert_eq!(state.committed, Some(5));
            assert_eq!(state.commits, 1);
            assert!(!state.pending);
        }

        let (code, pointer, length) = unsafe { resolve_page_ffi(handle, page_handle, true) };
        assert_eq!(code, ERROR_INVALID_HANDLE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "invalid_remote_output_page_handle");

        let (code, pointer, length) = unsafe { close_ffi(handle) };
        assert_eq!(code, RESULT_OK);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
    }

    #[test]
    fn remote_output_discard_replays_the_uncommitted_cursor() {
        let (handle, controls) = register_interactive_remote(false, None);
        let (code, first_page, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_ffi_raw(handle, b"s1") };
        assert_eq!(code, RESULT_OK);
        let first = unsafe { take_owned_json(metadata_pointer, metadata_length) };
        unsafe { take_owned_bytes(bytes_pointer, bytes_length) };
        assert_eq!(first["requestedOffset"], Value::Null);

        let (code, pointer, length) = unsafe { resolve_page_ffi(handle, first_page, false) };
        assert_eq!(code, RESULT_OK);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        {
            let state = controls.output.lock().unwrap();
            assert_eq!(state.committed, None);
            assert_eq!(state.discards, 1);
        }

        let (code, replay_page, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_ffi_raw(handle, b"s1") };
        assert_eq!(code, RESULT_OK);
        let replay = unsafe { take_owned_json(metadata_pointer, metadata_length) };
        unsafe { take_owned_bytes(bytes_pointer, bytes_length) };
        assert_eq!(replay["requestedOffset"], Value::Null);
        assert_eq!(replay["offset"], 0);
        let (code, _, _) = unsafe { resolve_page_ffi(handle, replay_page, false) };
        assert_eq!(code, RESULT_OK);
        assert_eq!(controls.output.lock().unwrap().discards, 2);

        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_output_pages_reject_wrong_parents_and_invalid_handles_without_consuming() {
        let (first_handle, first_controls) = register_interactive_remote(false, None);
        let (second_handle, _) = register_interactive_remote(false, None);
        let (code, page_handle, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_ffi_raw(first_handle, b"s1") };
        assert_eq!(code, RESULT_OK);
        unsafe { take_owned_json(metadata_pointer, metadata_length) };
        unsafe { take_owned_bytes(bytes_pointer, bytes_length) };

        let (code, pointer, length) = unsafe { resolve_page_ffi(second_handle, page_handle, true) };
        assert_eq!(code, ERROR_INVALID_HANDLE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "wrong_remote_output_page_parent");
        assert!(first_controls.output.lock().unwrap().pending);

        let (code, pointer, length) = unsafe { resolve_page_ffi(first_handle, u64::MAX, true) };
        assert_eq!(code, ERROR_INVALID_HANDLE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "invalid_remote_output_page_handle");
        assert!(first_controls.output.lock().unwrap().pending);

        assert_eq!(
            unsafe { resolve_page_ffi(first_handle, page_handle, true) }.0,
            RESULT_OK
        );
        assert_eq!(first_controls.output.lock().unwrap().committed, Some(5));
        assert_eq!(unsafe { close_ffi(first_handle) }.0, RESULT_OK);
        assert_eq!(unsafe { close_ffi(second_handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_close_discards_and_removes_every_owned_output_page() {
        let (handle, controls) = register_interactive_remote(false, None);
        let (code, page_handle, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_ffi_raw(handle, b"s1") };
        assert_eq!(code, RESULT_OK);
        unsafe { take_owned_json(metadata_pointer, metadata_length) };
        unsafe { take_owned_bytes(bytes_pointer, bytes_length) };
        assert!(lock_remote_output_pages().contains_key(&page_handle));

        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
        assert!(controls.disconnected.load(Ordering::Acquire));
        assert_eq!(controls.output.lock().unwrap().discards, 1);
        assert!(!lock_remote_output_pages().contains_key(&page_handle));

        let (code, pointer, length) = unsafe { resolve_page_ffi(handle, page_handle, true) };
        assert_eq!(code, ERROR_INVALID_HANDLE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "invalid_remote_handle");
    }

    #[test]
    fn remote_output_reset_discards_the_staged_page_and_freshens_the_cursor() {
        let (handle, controls) = register_interactive_remote(false, None);
        let (code, page_handle, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_ffi_raw(handle, b"s1") };
        assert_eq!(code, RESULT_OK);
        unsafe { take_owned_json(metadata_pointer, metadata_length) };
        unsafe { take_owned_bytes(bytes_pointer, bytes_length) };

        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let session_id = b"s1";
        let code = unsafe {
            unpeel_native_bridge_remote_output_reset(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_OK);
        assert!(pointer.is_null());
        assert_eq!(length, 0);
        {
            let state = controls.output.lock().unwrap();
            assert_eq!(state.resets, 1);
            assert_eq!(state.discards, 1);
            assert_eq!(state.committed, None);
            assert!(!state.pending);
        }
        let (code, pointer, length) = unsafe { resolve_page_ffi(handle, page_handle, true) };
        assert_eq!(code, ERROR_INVALID_HANDLE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["code"], "invalid_remote_output_page_handle");
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_output_poll_from_preserves_exact_and_fresh_cursor_intent() {
        let (handle, controls) = register_interactive_remote(false, None);
        let (code, page_handle, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_from_ffi_raw(handle, b"s1", Some(42)) };
        assert_eq!(code, RESULT_OK);
        let metadata = unsafe { take_owned_json(metadata_pointer, metadata_length) };
        unsafe { take_owned_bytes(bytes_pointer, bytes_length) };
        assert_eq!(metadata["requestedOffset"], 42);
        assert_eq!(metadata["offset"], 42);
        assert_eq!(metadata["nextOffset"], 47);
        assert_eq!(
            unsafe { resolve_page_ffi(handle, page_handle, true) }.0,
            RESULT_OK
        );
        assert_eq!(controls.output.lock().unwrap().committed, Some(47));

        let (code, page_handle, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_from_ffi_raw(handle, b"s1", None) };
        assert_eq!(code, RESULT_OK);
        let metadata = unsafe { take_owned_json(metadata_pointer, metadata_length) };
        unsafe { take_owned_bytes(bytes_pointer, bytes_length) };
        assert!(metadata["requestedOffset"].is_null());
        assert_eq!(metadata["offset"], 0);
        assert_eq!(metadata["resetBeforeFeed"], true);
        assert_eq!(
            unsafe { resolve_page_ffi(handle, page_handle, false) }.0,
            RESULT_OK
        );
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn output_reset_epoch_rejects_a_late_page_without_affecting_other_sessions() {
        let (handle, _) = register_interactive_remote(false, None);
        let (backend, stale_epoch) = remote_backend_with_output_epoch(handle, "s1").unwrap();
        reset_remote_output(handle, "s1").unwrap();

        let late_state = Arc::new(Mutex::new(TestOutputState {
            pending: true,
            ..TestOutputState::default()
        }));
        let late_page = Box::new(TestRegisteredOutputPage {
            state: Arc::clone(&late_state),
            metadata: NativeRemoteOutputPageMetadata {
                session_id: "s1".into(),
                requested_offset: None,
                offset: 0,
                next_offset: 5,
                reset_before_feed: true,
                truncated: false,
                captured_at_unix_ms: 1234,
                byte_count: 5,
            },
            bytes: b"hello".to_vec(),
            resolved: false,
        });
        let error =
            register_remote_output_page(handle, &backend, stale_epoch, "s1".into(), late_page)
                .unwrap_err();
        assert_eq!(error.code, "output_cursor_reset_during_poll");
        assert_eq!(late_state.lock().unwrap().discards, 1);

        let (same_backend, s2_epoch) = remote_backend_with_output_epoch(handle, "s2").unwrap();
        let s2_state = Arc::new(Mutex::new(TestOutputState {
            pending: true,
            ..TestOutputState::default()
        }));
        let s2_page = Box::new(TestRegisteredOutputPage {
            state: Arc::clone(&s2_state),
            metadata: NativeRemoteOutputPageMetadata {
                session_id: "s2".into(),
                requested_offset: None,
                offset: 0,
                next_offset: 5,
                reset_before_feed: true,
                truncated: false,
                captured_at_unix_ms: 1234,
                byte_count: 5,
            },
            bytes: b"hello".to_vec(),
            resolved: false,
        });
        let s2_handle =
            register_remote_output_page(handle, &same_backend, s2_epoch, "s2".into(), s2_page)
                .unwrap();
        discard_remote_output(handle, s2_handle).unwrap();
        assert_eq!(s2_state.lock().unwrap().discards, 1);
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_output_poll_panics_are_contained_and_do_not_close_the_parent() {
        let (handle, controls) = register_interactive_remote(true, None);
        let (code, page_handle, metadata_pointer, metadata_length, bytes_pointer, bytes_length) =
            unsafe { poll_ffi_raw(handle, b"s1") };
        assert_eq!(code, ERROR_PANIC);
        assert_eq!(page_handle, 0);
        let error = unsafe { take_owned_json(metadata_pointer, metadata_length) };
        assert_eq!(error["code"], "remote_bridge_panicked");
        assert!(bytes_pointer.is_null());
        assert_eq!(bytes_length, 0);
        assert!(remote_backend(handle).is_ok());
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
        assert!(controls.disconnected.load(Ordering::Acquire));
    }

    #[test]
    fn remote_effect_apis_return_typed_receipts_and_reach_the_selected_backend() {
        let (handle, controls) = register_interactive_remote(false, None);
        let session_id = b"s1";
        let mut pointer = ptr::null_mut();
        let mut length = 0;

        let code = unsafe {
            unpeel_native_bridge_remote_terminal_write(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                b"x".as_ptr(),
                1,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_OK);
        assert_eq!(unsafe { take_owned_json(pointer, length) }["requestID"], 41);

        pointer = ptr::null_mut();
        length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_desktop_fit(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                80,
                24,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_OK);
        assert_eq!(unsafe { take_owned_json(pointer, length) }["requestID"], 42);

        pointer = ptr::null_mut();
        length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_desktop_clear(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_OK);
        assert_eq!(unsafe { take_owned_json(pointer, length) }["requestID"], 43);

        pointer = ptr::null_mut();
        length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_mark_read(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, RESULT_OK);
        assert_eq!(unsafe { take_owned_json(pointer, length) }["requestID"], 44);

        assert_eq!(
            *controls.effects.lock().unwrap(),
            vec!["write:s1:x", "fit:s1:80x24", "clear:s1", "read:s1"]
        );
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_session_verbs_and_reads_reach_the_backend_with_typed_results() {
        let (handle, controls) = register_interactive_remote(false, None);
        let session_id = b"s1";
        let project_id = b"project-1";

        unsafe {
            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let title = b"Renamed";
            let code = unpeel_native_bridge_remote_session_title_set(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                title.as_ptr(),
                title.len(),
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            assert_eq!(take_owned_json(pointer, length)["requestID"], 45);

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let code = unpeel_native_bridge_remote_session_pinned_set(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                1,
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            assert_eq!(take_owned_json(pointer, length)["requestID"], 46);

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let code = unpeel_native_bridge_remote_session_notify_when_done_set(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                1,
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            assert_eq!(take_owned_json(pointer, length)["requestID"], 51);

            for verb_ffi in [
                unpeel_native_bridge_remote_session_archive
                    as unsafe extern "C" fn(
                        RemoteHandle,
                        *const u8,
                        usize,
                        *mut *mut u8,
                        *mut usize,
                    ) -> i32,
                unpeel_native_bridge_remote_session_restore,
                unpeel_native_bridge_remote_session_stop,
                unpeel_native_bridge_remote_session_remove,
                unpeel_native_bridge_remote_session_restart,
                unpeel_native_bridge_remote_session_restart_agent,
                unpeel_native_bridge_remote_session_resume_agent,
            ] {
                let mut pointer = ptr::null_mut();
                let mut length = 0;
                let code = verb_ffi(
                    handle,
                    session_id.as_ptr(),
                    session_id.len(),
                    &mut pointer,
                    &mut length,
                );
                assert_eq!(code, RESULT_OK);
                assert_eq!(take_owned_json(pointer, length)["requestID"], 47);
            }

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let ids = br#"["s2","s1"]"#;
            let code = unpeel_native_bridge_remote_session_order_set(
                handle,
                project_id.as_ptr(),
                project_id.len(),
                ids.as_ptr(),
                ids.len(),
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            assert_eq!(take_owned_json(pointer, length)["requestID"], 48);

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let patch = br#"{"sortOrder":1,"dateSorted":false}"#;
            let code = unpeel_native_bridge_remote_project_organization_set(
                handle,
                project_id.as_ptr(),
                project_id.len(),
                patch.as_ptr(),
                patch.len(),
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            assert_eq!(take_owned_json(pointer, length)["requestID"], 50);

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let create = br#"{"projectID":"project-1","presetID":"preset-1","initialText":"hi","initialTextSubmitMode":"pasteAndSubmit"}"#;
            let code = unpeel_native_bridge_remote_session_create(
                handle,
                create.as_ptr(),
                create.len(),
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            let created = take_owned_json(pointer, length);
            assert_eq!(created["requestID"], 49);
            assert_eq!(created["sessionID"], "session-created");
            assert_eq!(created["session"]["projectID"], "project-1");

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let invitation = br#"{"action":"create","endpoint":"http://controller:1234/mobile/pairing-proxy/INVITE"}"#;
            let code = unpeel_native_bridge_remote_pairing_invitation(
                handle,
                invitation.as_ptr(),
                invitation.len(),
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            let pairing = take_owned_json(pointer, length);
            assert_eq!(pairing["macID"], "host-1");

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let code = unpeel_native_bridge_remote_archived_sessions(
                handle,
                project_id.as_ptr(),
                project_id.len(),
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            let archived = take_owned_json(pointer, length);
            assert_eq!(archived["projectID"], "project-1");
            assert_eq!(archived["sessions"][0]["id"], "archived-1");

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let code = unpeel_native_bridge_remote_transcript_markdown(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                20,
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            let transcript = take_owned_json(pointer, length);
            assert_eq!(transcript["sessionID"], "s1");
            assert_eq!(transcript["markdown"], "# Transcript");

            let mut pointer = ptr::null_mut();
            let mut length = 0;
            let code = unpeel_native_bridge_remote_session_metrics(
                handle,
                session_id.as_ptr(),
                session_id.len(),
                &mut pointer,
                &mut length,
            );
            assert_eq!(code, RESULT_OK);
            let metrics = take_owned_json(pointer, length);
            assert_eq!(metrics["sessionID"], "s1");
            assert_eq!(metrics["columns"], 120);
            assert_eq!(metrics["rows"], 34);
            assert_eq!(metrics["outputOffset"], 4096);
            assert_eq!(metrics["capturedAtUnixMs"], 1234);
        }

        assert_eq!(
            *controls.effects.lock().unwrap(),
            vec![
                "title:s1:Renamed",
                "pin:s1:true",
                "notify:s1:true",
                "session archive:s1",
                "session restore:s1",
                "session stop:s1",
                "session remove:s1",
                "session restart:s1",
                "session agent restart:s1",
                "session agent resume:s1",
                "order:project-1:s2,s1",
                "project-organization:project-1:Some(1):None:None:Some(false)",
                "create:project-1:preset-1",
                "pairing:create",
                "archived:project-1",
                "transcript:s1:20",
                "metrics:s1",
            ]
        );
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_session_verb_failures_preserve_delivery_classification() {
        let (handle, _) = register_interactive_remote(false, Some("outcomeUnknown"));
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_session_restart(
                handle,
                b"s1".as_ptr(),
                2,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, ERROR_REMOTE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["kind"], "outcomeUnknown");
        assert_eq!(error["operation"], "session restart");
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn remote_effect_failures_preserve_not_applied_and_outcome_unknown() {
        let (handle, _) = register_interactive_remote(false, Some("outcomeUnknown"));
        let mut pointer = ptr::null_mut();
        let mut length = 0;
        let code = unsafe {
            unpeel_native_bridge_remote_terminal_write(
                handle,
                b"s1".as_ptr(),
                2,
                b"x".as_ptr(),
                1,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, ERROR_REMOTE);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["kind"], "outcomeUnknown");
        assert_eq!(error["code"], "host_connection_disconnected");
        assert_eq!(error["operation"], "terminal write");
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("may have landed"));

        pointer = ptr::null_mut();
        length = 0;
        let invalid_utf8 = [0xff];
        let code = unsafe {
            unpeel_native_bridge_remote_terminal_write(
                handle,
                invalid_utf8.as_ptr(),
                invalid_utf8.len(),
                b"x".as_ptr(),
                1,
                &mut pointer,
                &mut length,
            )
        };
        assert_eq!(code, ERROR_INVALID_INPUT);
        let error = unsafe { take_owned_json(pointer, length) };
        assert_eq!(error["kind"], "notApplied");
        assert_eq!(error["code"], "invalid_session_id_utf8");
        assert_eq!(error["operation"], "terminal write");
        assert!(error["message"].as_str().unwrap().contains("UTF-8"));
        assert_eq!(unsafe { close_ffi(handle) }.0, RESULT_OK);
    }

    #[test]
    fn core_effect_failures_map_to_the_stable_bridge_kind_and_code() {
        let target = SshTarget::parse("ssh://studio").unwrap();
        let backend = RemoteSessionBackend::new(Arc::new(SshHostConnection::new(target)));
        let failure = backend.write_terminal("../escape", "x").unwrap_err();
        let native = native_remote_effect_error(failure);
        assert_eq!(native.kind, "notApplied");
        assert_eq!(native.code, "invalid_remote_session_id");
        assert_eq!(native.operation, "terminal write");
        assert_eq!(native.result, ERROR_INVALID_INPUT);
    }

    #[test]
    fn concurrent_calls_do_not_share_request_state() {
        let joins: Vec<_> = (0..8)
            .map(|_| thread::spawn(|| guarded_route(&request("/mobile/metrics"), None)))
            .collect();
        for join in joins {
            let (code, bytes) = join.join().unwrap();
            assert_eq!(code, RESULT_HANDLED);
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["status"],
                400
            );
        }
    }
}
