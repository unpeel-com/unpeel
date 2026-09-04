//! Persistent same-user Controller endpoint for one workspace Host worker.
//!
//! The wire is exactly `remote_stdio`'s framed Host contract. A native local
//! workspace connection therefore gets the same generation-bound semantics
//! as SSH/Direct/Link, but the semantic runtime lives here instead of in one
//! throwaway gateway process per Controller connection.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use unpeel_core::controller_host::ControllerHostRuntime;
use unpeel_core::remote_stdio;

use crate::computer::{ComputerAdapter, SharedComputerStatus};
use crate::platform_adapter::{
    PlatformAdapterError, PlatformAdapterHub, PlatformAdapterRegistration,
    PLATFORM_ADAPTER_CONTROL_PATH,
};

const ACCEPT_INTERVAL: Duration = Duration::from_millis(25);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const PAIRING_CONTROL_PATH: &str = "/_unpeel/pairing";
/// Verbs the disk-backed runtime omits from its descriptor but this socket
/// serves from the worker's live authorities; keep in step with
/// `crate::mobile::handle_local_live_route`.
const LIVE_LOCAL_CAPABILITIES: &[&str] = &["approval.answer", "pairing.invitation"];

#[derive(Debug)]
pub(crate) enum LocalControlRequest {
    BeginPairing {
        advertised_host: Option<String>,
        advertised_port: Option<u16>,
        reply: mpsc::SyncSender<Result<serde_json::Value, String>>,
    },
    PairingStatus {
        reply: mpsc::SyncSender<Result<serde_json::Value, String>>,
    },
    CancelPairing {
        reply: mpsc::SyncSender<Result<serde_json::Value, String>>,
    },
    ListDevices {
        reply: mpsc::SyncSender<Result<serde_json::Value, String>>,
    },
    RevokeDevice {
        device_id: String,
        reply: mpsc::SyncSender<Result<serde_json::Value, String>>,
    },
    SetDeviceRelayAllowed {
        device_id: String,
        allowed: bool,
        reply: mpsc::SyncSender<Result<serde_json::Value, String>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingStatus {
    Active,
    Completed,
    Closed,
}

pub struct LocalGatewayServer {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    active: Arc<Mutex<HashMap<u64, UnixStream>>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept_thread: Option<JoinHandle<()>>,
}

impl LocalGatewayServer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        home: &Path,
        hook_port: u16,
        control_tx: mpsc::Sender<LocalControlRequest>,
        platform_adapters: Arc<PlatformAdapterHub>,
        computer_status: SharedComputerStatus,
        approvals: Arc<crate::approvals::ApprovalHub>,
        pairing: Arc<crate::pairing::PairingWindow>,
        snapshot: crate::mobile::SharedSnapshot,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(home)
            .map_err(|error| format!("prepare local Host socket directory: {error}"))?;
        let path = remote_stdio::local_host_socket_path(home);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| {
                format!("remove stale local Host socket {}: {error}", path.display())
            })?;
        }
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("bind local Host socket {}: {error}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect local Host socket {}: {error}", path.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("configure local Host socket: {error}"))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(HashMap::new()));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_active = Arc::clone(&active);
        let thread_workers = Arc::clone(&workers);
        // Same hook port Direct/Link create uses. `None` unsets
        // `UNPEEL_APP_PORT` in the hosted PTY, so provider hooks write
        // `last-hook-event.json` but never POST Busy/Idle and the sidebar
        // spinner never starts.
        let runtime = Arc::new(ControllerHostRuntime::owner_transport(
            "local",
            Some(remote_stdio::owner_subject()),
            Some(hook_port),
        ));
        let connection_ids = Arc::new(AtomicU64::new(1));
        let accept_thread = std::thread::Builder::new()
            .name("unpeel-local-host-accept".into())
            .spawn(move || {
                while !thread_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if thread_shutdown.load(Ordering::Acquire) {
                                let _ = stream.shutdown(std::net::Shutdown::Both);
                                break;
                            }
                            // macOS can propagate O_NONBLOCK from the listener
                            // to accepted Unix sockets. The framed protocol is
                            // persistent; a moment with no next frame is not a
                            // disconnect (and adapter registrations depend on
                            // that connection lifetime), so make it explicit.
                            if stream.set_nonblocking(false).is_err() {
                                continue;
                            }
                            let id = connection_ids.fetch_add(1, Ordering::Relaxed);
                            let tracked = match stream.try_clone() {
                                Ok(stream) => stream,
                                Err(_) => continue,
                            };
                            if let Ok(mut active) = thread_active.lock() {
                                active.insert(id, tracked);
                            }
                            let runtime = Arc::clone(&runtime);
                            let active = Arc::clone(&thread_active);
                            let control_tx = control_tx.clone();
                            let platform_adapters = Arc::clone(&platform_adapters);
                            let computer_status = Arc::clone(&computer_status);
                            let approvals = Arc::clone(&approvals);
                            let pairing = Arc::clone(&pairing);
                            let snapshot = Arc::clone(&snapshot);
                            let worker = std::thread::Builder::new()
                                .name(format!("unpeel-local-host-{id}"))
                                .spawn(move || {
                                    serve_connection(
                                        id,
                                        stream,
                                        runtime,
                                        active,
                                        control_tx,
                                        platform_adapters,
                                        computer_status,
                                        approvals,
                                        pairing,
                                        snapshot,
                                    )
                                });
                            if let Ok(worker) = worker {
                                if let Ok(mut workers) = thread_workers.lock() {
                                    workers.push(worker);
                                }
                            } else if let Ok(mut active) = thread_active.lock() {
                                active.remove(&id);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(ACCEPT_INTERVAL);
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| format!("start local Host accept loop: {error}"))?;
        Ok(Self {
            path,
            shutdown,
            active,
            workers,
            accept_thread: Some(accept_thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stop(&mut self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = UnixStream::connect(&self.path);
        if let Ok(active) = self.active.lock() {
            for stream in active.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        if let Some(thread) = self.accept_thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut workers) = self.workers.lock() {
            for worker in workers.drain(..) {
                let _ = worker.join();
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for LocalGatewayServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn serve_connection(
    id: u64,
    stream: UnixStream,
    runtime: Arc<ControllerHostRuntime>,
    active: Arc<Mutex<HashMap<u64, UnixStream>>>,
    control_tx: mpsc::Sender<LocalControlRequest>,
    platform_adapters: Arc<PlatformAdapterHub>,
    computer_status: SharedComputerStatus,
    approvals: Arc<crate::approvals::ApprovalHub>,
    pairing: Arc<crate::pairing::PairingWindow>,
    snapshot: crate::mobile::SharedSnapshot,
) {
    let mut reader = match stream.try_clone() {
        Ok(reader) => reader,
        Err(_) => return,
    };
    let mut writer = stream;
    let namespace = format!("local:{}:{id}", remote_stdio::owner_subject());
    let handler = |request: unpeel_core::relay_wire::TunnelRequest, cancelled: &AtomicBool| {
        if request.path == PAIRING_CONTROL_PATH {
            return handle_local_control(request, &control_tx);
        }
        if request.path == PLATFORM_ADAPTER_CONTROL_PATH {
            return handle_platform_adapter_control(id, request, &platform_adapters);
        }
        if let Some(response) =
            crate::mobile::handle_local_live_route(&request, &approvals, &pairing, &snapshot)
        {
            return response;
        }
        let is_bootstrap = request.method == "GET" && request.path == "/mobile/bootstrap";
        let (request, notify_when_done) =
            match platform_session_organization(request, &platform_adapters) {
                Ok(value) => value,
                Err(response) => return response,
            };
        let had_notify_when_done = notify_when_done.is_some();
        let write_project_color = |project_id: &str, color: Option<&str>| -> Result<(), String> {
            let adapter = platform_adapters
                .call(
                    "overlay.project-color.set",
                    serde_json::json!({
                        "projectID": project_id,
                        "colorID": color.unwrap_or_default(),
                    }),
                )
                .map_err(|error| error.to_string())?;
            if adapter.status != 200 {
                return Err(adapter
                    .body
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("native folder-color adapter rejected the write")
                    .to_owned());
            }
            unpeel_core::state_bus::announce(
                unpeel_core::state_bus::Change::AppState,
                unpeel_core::session_ops::own_listener_port_public(),
            );
            Ok(())
        };
        let project_color_writer: Option<unpeel_core::controller_host::ProjectColorWriter<'_>> =
            platform_adapters
                .supports("overlay.project-color.set")
                .then_some(&write_project_color);
        let mut response = runtime.handle_tunnel_with_project_color_writer(
            &namespace,
            request,
            cancelled,
            project_color_writer,
        );
        if response.status == 200 {
            if is_bootstrap {
                ComputerAdapter::decorate_shared_workspace_settings(
                    &computer_status,
                    &mut response.body,
                );
            }
            if let Some(request) = notify_when_done {
                response = match platform_adapters.call("session.notify_when_done.set", request) {
                    Ok(adapter) => unpeel_core::controller_api::ControllerResponse {
                        id: response.id,
                        status: adapter.status,
                        body: adapter.body,
                    },
                    Err(PlatformAdapterError::Unavailable) => {
                        unpeel_core::controller_api::ControllerResponse {
                            id: response.id,
                            status: 501,
                            body: serde_json::json!({
                                "error": "notifyWhenDone is not supported by this Host"
                            }),
                        }
                    }
                    Err(error) => unpeel_core::controller_api::ControllerResponse {
                        id: response.id,
                        status: 503,
                        body: serde_json::json!({ "error": error.to_string() }),
                    },
                };
            }
            if let Some(protocol) = response
                .body
                .get("hostProtocol")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
            {
                let mut protocol: unpeel_core::controller_protocol::HostProtocolDescriptor =
                    protocol;
                // The disk-backed runtime omits the verbs it cannot serve, but
                // this socket routes them to the worker's live authorities
                // (`handle_local_live_route`), so advertise them here.
                for capability in LIVE_LOCAL_CAPABILITIES {
                    if !protocol.supports(capability) {
                        protocol.capabilities.push((*capability).to_owned());
                    }
                }
                platform_adapters.decorate_protocol(&mut protocol);
                response.body["hostProtocol"] =
                    serde_json::to_value(protocol).unwrap_or(serde_json::Value::Null);
            }
        }
        let body = serde_json::to_vec(&response.body)
            .unwrap_or_else(|_| br#"{"error":"response encoding failed"}"#.to_vec());
        if had_notify_when_done {
            crate::tracelog::trace(
                "local-gateway",
                &format!(
                    "connection {id} completed platform organization with {}",
                    response.status
                ),
            );
        }
        (response.status, body)
    };
    if let Err(error) = remote_stdio::serve(&mut reader, &mut writer, &handler) {
        crate::tracelog::trace(
            "local-gateway",
            &format!("connection {id} closed with framing error: {error}"),
        );
    }
    platform_adapters.unregister(id);
    if let Ok(mut active) = active.lock() {
        active.remove(&id);
    }
}

fn handle_platform_adapter_control(
    connection_id: u64,
    request: unpeel_core::relay_wire::TunnelRequest,
    hub: &PlatformAdapterHub,
) -> (u16, Vec<u8>) {
    if request.method != "POST" {
        return (405, br#"{"error":"method not allowed"}"#.to_vec());
    }
    let body = match serde_json::from_slice::<serde_json::Value>(&request.body) {
        Ok(value) => value,
        Err(_) => return (400, br#"{"error":"invalid adapter request"}"#.to_vec()),
    };
    let action = body
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let response = match action {
        "register" => {
            let registration = body
                .get("registration")
                .cloned()
                .ok_or_else(|| "platform adapter registration required".to_string())
                .and_then(|value| {
                    serde_json::from_value::<PlatformAdapterRegistration>(value)
                        .map_err(|_| "invalid platform adapter registration".to_string())
                })
                .and_then(|registration| hub.register(connection_id, registration));
            match registration {
                Ok(capabilities) => Ok(serde_json::json!({
                    "ok": true,
                    "version": crate::platform_adapter::PLATFORM_ADAPTER_VERSION,
                    "capabilities": capabilities,
                })),
                Err(error) => Err(error),
            }
        }
        "status" if hub.contains_connection(connection_id) => Ok(serde_json::json!({
            "ok": true,
            "version": crate::platform_adapter::PLATFORM_ADAPTER_VERSION,
            "capabilities": hub.capabilities(),
        })),
        "status" => return (409, br#"{"error":"adapter is not registered"}"#.to_vec()),
        _ => return (400, br#"{"error":"invalid adapter action"}"#.to_vec()),
    };
    match response {
        Ok(value) => (
            200,
            serde_json::to_vec(&value).unwrap_or_else(|_| br#"{"ok":true}"#.to_vec()),
        ),
        Err(error) => (
            400,
            serde_json::to_vec(&serde_json::json!({ "error": error }))
                .unwrap_or_else(|_| br#"{"error":"adapter registration failed"}"#.to_vec()),
        ),
    }
}

/// Strip the platform field before dispatching common organization semantics.
/// All ordinary validation/effects still happen in the shared Host; only a
/// successful common response may reach the registered native adapter.
fn platform_session_organization(
    mut request: unpeel_core::relay_wire::TunnelRequest,
    platform_adapters: &PlatformAdapterHub,
) -> Result<
    (
        unpeel_core::relay_wire::TunnelRequest,
        Option<serde_json::Value>,
    ),
    (u16, Vec<u8>),
> {
    if request.method != "POST" || request.path != "/mobile/session-organization" {
        return Ok((request, None));
    }
    let mut body = match serde_json::from_slice::<serde_json::Value>(&request.body) {
        Ok(serde_json::Value::Object(object)) => object,
        _ => return Ok((request, None)),
    };
    let notify_value = body.get("notifyWhenDone").cloned();
    if notify_value.is_none()
        || notify_value
            .as_ref()
            .is_some_and(serde_json::Value::is_null)
    {
        return Ok((request, None));
    }

    // The disk Host's organization contract resolves the resource before it
    // validates fields, then validates title/pin/archive/project before the
    // platform field. Repeat that pure preflight here before removing the
    // native field, otherwise an adapter-free compound request could mutate
    // ordinary Host state before returning 501.
    let session_id = body
        .get("sessionID")
        .or_else(|| body.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && !value.contains('/')
                && !value.contains('\\')
                && !value.contains("..")
        })
        .ok_or_else(|| (400, br#"{"error":"invalid session id"}"#.to_vec()))?
        .to_owned();
    if unpeel_core::session_host::load_manifest(&session_id).is_none() {
        return Err((404, br#"{"error":"unknown session"}"#.to_vec()));
    }
    match body.get("title") {
        None | Some(serde_json::Value::Null | serde_json::Value::String(_)) => {}
        Some(_) => return Err((400, br#"{"error":"title must be a string"}"#.to_vec())),
    }
    match body.get("pinned") {
        None | Some(serde_json::Value::Null | serde_json::Value::Bool(_)) => {}
        Some(_) => return Err((400, br#"{"error":"pinned must be a boolean"}"#.to_vec())),
    }
    match body.get("archived") {
        None | Some(serde_json::Value::Null | serde_json::Value::Bool(_)) => {}
        Some(_) => return Err((400, br#"{"error":"archived must be a boolean"}"#.to_vec())),
    }
    match body.get("projectID") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {}
        Some(serde_json::Value::String(_)) => {
            return Err((400, br#"{"error":"projectID must not be empty"}"#.to_vec()))
        }
        Some(_) => return Err((400, br#"{"error":"projectID must be a string"}"#.to_vec())),
    }
    let notify_when_done = match notify_value {
        Some(serde_json::Value::Bool(value)) => value,
        _ => {
            return Err((
                400,
                br#"{"error":"notifyWhenDone must be a boolean"}"#.to_vec(),
            ))
        }
    };
    if !platform_adapters.supports("session.notify_when_done.set") {
        return Err((
            501,
            br#"{"error":"notifyWhenDone is not supported by this Host"}"#.to_vec(),
        ));
    }
    body.remove("notifyWhenDone");
    let notify = Some(serde_json::json!({
        "sessionID": session_id,
        "notifyWhenDone": notify_when_done,
    }));
    request.body =
        serde_json::to_vec(&serde_json::Value::Object(body)).unwrap_or_else(|_| b"{}".to_vec());
    Ok((request, notify))
}

fn handle_local_control(
    request: unpeel_core::relay_wire::TunnelRequest,
    control_tx: &mpsc::Sender<LocalControlRequest>,
) -> (u16, Vec<u8>) {
    if request.method != "POST" {
        return (405, br#"{"error":"method not allowed"}"#.to_vec());
    }
    let body = serde_json::from_slice::<serde_json::Value>(&request.body).unwrap_or_default();
    let action = body
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let device_id = || {
        body.get("deviceID")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && !value.contains('/')
                    && !value.contains('\\')
                    && !value.bytes().any(|byte| byte.is_ascii_control())
            })
            .map(str::to_owned)
    };
    let control = match action {
        "begin" => LocalControlRequest::BeginPairing {
            advertised_host: body
                .get("advertisedHost")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            advertised_port: body
                .get("advertisedPort")
                .and_then(serde_json::Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port > 0),
            reply: reply_tx,
        },
        "status" => LocalControlRequest::PairingStatus { reply: reply_tx },
        "cancel" => LocalControlRequest::CancelPairing { reply: reply_tx },
        "devices" => LocalControlRequest::ListDevices { reply: reply_tx },
        "revoke-device" => {
            let Some(device_id) = device_id() else {
                return (400, br#"{"error":"valid deviceID required"}"#.to_vec());
            };
            LocalControlRequest::RevokeDevice {
                device_id,
                reply: reply_tx,
            }
        }
        "set-relay-allowed" => {
            let Some(device_id) = device_id() else {
                return (400, br#"{"error":"valid deviceID required"}"#.to_vec());
            };
            let Some(allowed) = body.get("allowed").and_then(serde_json::Value::as_bool) else {
                return (400, br#"{"error":"boolean allowed required"}"#.to_vec());
            };
            LocalControlRequest::SetDeviceRelayAllowed {
                device_id,
                allowed,
                reply: reply_tx,
            }
        }
        _ => return (400, br#"{"error":"invalid pairing action"}"#.to_vec()),
    };
    if control_tx.send(control).is_err() {
        return (
            503,
            br#"{"error":"Host control loop unavailable"}"#.to_vec(),
        );
    }
    match reply_rx.recv_timeout(CONTROL_TIMEOUT) {
        Ok(Ok(value)) => (
            200,
            serde_json::to_vec(&value).unwrap_or_else(|_| br#"{"ok":true}"#.to_vec()),
        ),
        Ok(Err(error)) => (
            409,
            serde_json::to_vec(&serde_json::json!({ "error": error }))
                .unwrap_or_else(|_| br#"{"error":"pairing failed"}"#.to_vec()),
        ),
        Err(_) => (503, br#"{"error":"Host control timed out"}"#.to_vec()),
    }
}

fn pairing_control_call(home: &Path, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let mut stream = UnixStream::connect(remote_stdio::local_host_socket_path(home))
        .map_err(|error| format!("connect to the workspace Host: {error}"))?;
    let request = unpeel_core::relay_wire::TunnelRequest {
        id: 1,
        method: "POST".into(),
        path: PAIRING_CONTROL_PATH.into(),
        query: Vec::new(),
        auth: None,
        content_type: Some("application/json".into()),
        body: serde_json::to_vec(&body).map_err(|error| error.to_string())?,
    };
    remote_stdio::write_frame(
        &mut stream,
        remote_stdio::FRAME_KIND_REQUEST,
        &unpeel_core::relay_wire::encode_tunnel_request(&request),
    )?;
    let frame = remote_stdio::read_frame(&mut stream)?.ok_or("workspace Host closed")?;
    if frame.kind != remote_stdio::FRAME_KIND_RESPONSE {
        return Err("workspace Host returned an invalid response".into());
    }
    let response = unpeel_core::relay_wire::parse_tunnel_response(&frame.payload)?;
    let value = serde_json::from_slice::<serde_json::Value>(&response.body)
        .map_err(|_| "workspace Host returned invalid JSON".to_string())?;
    if response.status == 200 {
        Ok(value)
    } else {
        Err(value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("workspace Host rejected pairing")
            .to_owned())
    }
}

pub fn begin_pairing(
    home: &Path,
    advertised_host: Option<&str>,
    advertised_port: Option<u16>,
) -> Result<String, String> {
    let response = pairing_control_call(
        home,
        serde_json::json!({
            "action": "begin",
            "advertisedHost": advertised_host,
            "advertisedPort": advertised_port,
        }),
    )?;
    response
        .get("code")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or("workspace Host omitted the pairing code".into())
}

pub fn pairing_status(home: &Path) -> Result<PairingStatus, String> {
    let response = pairing_control_call(home, serde_json::json!({ "action": "status" }))?;
    match response.get("status").and_then(serde_json::Value::as_str) {
        Some("active") => Ok(PairingStatus::Active),
        Some("completed") => Ok(PairingStatus::Completed),
        Some("closed") => Ok(PairingStatus::Closed),
        _ => Err("workspace Host returned an invalid pairing status".into()),
    }
}

pub fn cancel_pairing(home: &Path) -> Result<(), String> {
    pairing_control_call(home, serde_json::json!({ "action": "cancel" })).map(|_| ())
}

/// Sanitized paired-Controller rows owned by the workspace worker. The
/// response deliberately contains no bearer hashes or E2E key material; it is
/// the same projection the worker uses for same-user native clients.
pub fn paired_devices(home: &Path) -> Result<Vec<serde_json::Value>, String> {
    let response = pairing_control_call(home, serde_json::json!({ "action": "devices" }))?;
    response
        .get("devices")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or("workspace Host omitted its paired devices".into())
}

/// Revoke one Controller through the worker so Direct and Link ownership are
/// reconciled in the same turn as the authorization-file mutation.
pub fn revoke_device(home: &Path, device_id: &str) -> Result<(), String> {
    pairing_control_call(
        home,
        serde_json::json!({
            "action": "revoke-device",
            "deviceID": device_id,
        }),
    )
    .map(|_| ())
}

/// Narrow or restore one Controller's Link scope through the worker. `true`
/// removes the legacy opt-out key, matching the canonical device-store
/// semantics rather than persisting a redundant affirmative value.
pub fn set_device_relay_allowed(home: &Path, device_id: &str, allowed: bool) -> Result<(), String> {
    pairing_control_call(
        home,
        serde_json::json!({
            "action": "set-relay-allowed",
            "deviceID": device_id,
            "allowed": allowed,
        }),
    )
    .map(|_| ())
}
