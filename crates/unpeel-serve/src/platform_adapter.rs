//! Connection-scoped native capability adapters for the canonical Host.
//!
//! A platform client registers over the workspace's mode-0600 `host.sock`.
//! The registration can name only native-only operations from the canonical
//! Host capability ledger and supplies a loopback callback port plus an
//! ephemeral bearer. Registrations disappear with their local Host connection;
//! a callback transport failure also withdraws them immediately. The Rust Host
//! therefore remains the capability authority and never advertises an app
//! operation merely because a Mac process happens to exist.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use unpeel_core::controller_protocol::{HostProtocolDescriptor, NATIVE_HOST_CAPABILITIES};

pub const PLATFORM_ADAPTER_CONTROL_PATH: &str = "/_unpeel/platform-adapter";
pub const PLATFORM_ADAPTER_CALLBACK_PATH: &str = "/_unpeel/platform-adapter/call";
pub const PLATFORM_ADAPTER_VERSION: u16 = 1;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Link entitlement refresh may cross the network through the native app so
/// the legacy license key never leaves Keychain. It always runs on a worker
/// thread (never the Host tick), and gets a bounded ceiling just above the
/// app's 15-second service request timeout.
const LINK_ENTITLEMENT_CALLBACK_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CALLBACK_BYTES: usize = 512 * 1024;
const MAX_CAPABILITIES: usize = 16;
const MAX_INSTANCE_ID_BYTES: usize = 128;
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 256;

/// Controller operations which genuinely require a running native adapter.
/// Keep this bounded and exact: ordinary Host operations must never be
/// withdrawn merely because the app closes. Add entries only after the Rust
/// route delegates that operation through [`PlatformAdapterHub::call`].
pub const ADAPTER_HOST_CAPABILITIES: &[&str] = &[
    "push.register",
    "relay.credentials.recover",
    "session.notify_when_done.set",
];

/// Host-internal platform services. These share the same authenticated,
/// connection-scoped callback channel but are not Controller operations and
/// therefore never appear in `hostProtocol.capabilities`.
pub const ADAPTER_SERVICES: &[&str] = &[
    "approval.present",
    "app.open-in-editor",
    "artifact.thumbnail",
    "computer.status",
    "controller.transport.host-owned",
    "link.entitlement.refresh",
    "mobile.e2e-key.reconcile",
    "notification.deliver",
    "overlay.snapshot",
    "overlay.project-color.set",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformAdapterRegistration {
    pub version: u16,
    #[serde(rename = "instanceID")]
    pub instance_id: String,
    pub callback_port: u16,
    pub callback_token: String,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug)]
struct LiveRegistration {
    connection_id: u64,
    serial: u64,
    instance_id: String,
    callback_port: u16,
    callback_token: String,
    capabilities: HashSet<String>,
}

#[derive(Default)]
struct HubState {
    registrations: HashMap<u64, LiveRegistration>,
    next_serial: u64,
}

/// Live platform registrations for one workspace worker.
#[derive(Default)]
pub struct PlatformAdapterHub {
    state: Mutex<HubState>,
    generation: AtomicU64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlatformAdapterResponse {
    pub status: u16,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformAdapterError {
    Unavailable,
    Transport(String),
    InvalidResponse(String),
}

impl std::fmt::Display for PlatformAdapterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("platform capability is unavailable"),
            Self::Transport(message) | Self::InvalidResponse(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl PlatformAdapterHub {
    pub fn register(
        &self,
        connection_id: u64,
        registration: PlatformAdapterRegistration,
    ) -> Result<Vec<String>, String> {
        validate_registration(&registration)?;
        let capabilities: HashSet<String> = registration.capabilities.iter().cloned().collect();
        let mut state = self
            .state
            .lock()
            .map_err(|_| "platform adapter registry is unavailable".to_string())?;
        state.next_serial = state.next_serial.wrapping_add(1).max(1);
        let serial = state.next_serial;
        state.registrations.insert(
            connection_id,
            LiveRegistration {
                connection_id,
                serial,
                instance_id: registration.instance_id,
                callback_port: registration.callback_port,
                callback_token: registration.callback_token,
                capabilities,
            },
        );
        drop(state);
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(self.capabilities())
    }

    pub fn unregister(&self, connection_id: u64) -> bool {
        let removed = self
            .state
            .lock()
            .map(|mut state| state.registrations.remove(&connection_id).is_some())
            .unwrap_or(false);
        if removed {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        removed
    }

    pub fn contains_connection(&self, connection_id: u64) -> bool {
        self.state
            .lock()
            .map(|state| state.registrations.contains_key(&connection_id))
            .unwrap_or(false)
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn supports(&self, capability: &str) -> bool {
        self.state
            .lock()
            .map(|state| {
                state
                    .registrations
                    .values()
                    .any(|registration| registration.capabilities.contains(capability))
            })
            .unwrap_or(false)
    }

    pub fn capabilities(&self) -> Vec<String> {
        ADAPTER_HOST_CAPABILITIES
            .iter()
            .filter(|capability| self.supports(capability))
            .map(|capability| (*capability).to_owned())
            .collect()
    }

    /// Add live adapter operations to an existing Host descriptor, retaining
    /// the canonical ledger order and every transport-specific omission.
    pub fn decorate_protocol(&self, protocol: &mut HostProtocolDescriptor) {
        let mut wanted: HashSet<String> = protocol.capabilities.iter().cloned().collect();
        wanted.extend(self.capabilities());
        let mut ordered = NATIVE_HOST_CAPABILITIES
            .iter()
            .filter(|capability| wanted.remove(**capability))
            .map(|capability| (*capability).to_owned())
            .collect::<Vec<_>>();
        // Future descriptors can carry ids unknown to this build. Never erase
        // them while inserting the adapter subset.
        ordered.extend(
            protocol
                .capabilities
                .iter()
                .filter(|capability| wanted.remove(capability.as_str()))
                .cloned(),
        );
        protocol.capabilities = ordered;
    }

    /// Invoke the newest live adapter that registered `capability`.
    pub fn call(
        &self,
        capability: &str,
        request: Value,
    ) -> Result<PlatformAdapterResponse, PlatformAdapterError> {
        let registration = self
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .registrations
                    .values()
                    .filter(|registration| registration.capabilities.contains(capability))
                    .max_by_key(|registration| registration.serial)
                    .cloned()
            })
            .ok_or(PlatformAdapterError::Unavailable)?;
        let body = serde_json::to_vec(&serde_json::json!({
            "version": PLATFORM_ADAPTER_VERSION,
            "operation": capability,
            "request": request,
        }))
        .map_err(|error| PlatformAdapterError::InvalidResponse(error.to_string()))?;
        crate::tracelog::trace(
            "platform-adapter",
            &format!(
                "invoke {capability} through {} on 127.0.0.1:{}",
                registration.instance_id, registration.callback_port
            ),
        );
        let timeout = if capability == "link.entitlement.refresh" {
            LINK_ENTITLEMENT_CALLBACK_TIMEOUT
        } else {
            CALLBACK_TIMEOUT
        };
        let result = callback_request(&registration, &body, timeout);
        match &result {
            Ok(response) => crate::tracelog::trace(
                "platform-adapter",
                &format!("{capability} completed with status {}", response.status),
            ),
            Err(error) => {
                crate::tracelog::trace("platform-adapter", &format!("{capability} failed: {error}"))
            }
        }
        if matches!(
            result,
            Err(PlatformAdapterError::Transport(_) | PlatformAdapterError::InvalidResponse(_))
        ) {
            self.unregister(registration.connection_id);
        }
        result
    }
}

fn validate_registration(registration: &PlatformAdapterRegistration) -> Result<(), String> {
    if registration.version != PLATFORM_ADAPTER_VERSION {
        return Err("unsupported platform adapter version".into());
    }
    let instance_id = registration.instance_id.trim();
    if instance_id.is_empty()
        || instance_id.len() > MAX_INSTANCE_ID_BYTES
        || instance_id.contains(['\r', '\n', '\0'])
    {
        return Err("invalid platform adapter instance id".into());
    }
    if registration.callback_port == 0 {
        return Err("invalid platform adapter callback port".into());
    }
    let token = registration.callback_token.as_bytes();
    if !(MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&token.len())
        || !token.iter().all(|byte| byte.is_ascii_graphic())
    {
        return Err("invalid platform adapter callback token".into());
    }
    if registration.capabilities.is_empty() || registration.capabilities.len() > MAX_CAPABILITIES {
        return Err("invalid platform adapter capability list".into());
    }
    let mut seen = HashSet::new();
    for capability in &registration.capabilities {
        if !ADAPTER_HOST_CAPABILITIES.contains(&capability.as_str())
            && !ADAPTER_SERVICES.contains(&capability.as_str())
        {
            return Err(format!("unknown platform adapter capability: {capability}"));
        }
        if !seen.insert(capability) {
            return Err(format!(
                "duplicate platform adapter capability: {capability}"
            ));
        }
    }
    Ok(())
}

fn callback_request(
    registration: &LiveRegistration,
    body: &[u8],
    timeout: Duration,
) -> Result<PlatformAdapterResponse, PlatformAdapterError> {
    let address = SocketAddr::from(([127, 0, 0, 1], registration.callback_port));
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|error| {
        PlatformAdapterError::Transport(format!(
            "platform adapter {} is unreachable: {error}",
            registration.instance_id
        ))
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|_| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| PlatformAdapterError::Transport(error.to_string()))?;
    let head = format!(
        "POST {PLATFORM_ADAPTER_CALLBACK_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        registration.callback_token,
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body))
        .and_then(|_| stream.flush())
        .map_err(|error| PlatformAdapterError::Transport(error.to_string()))?;

    let mut raw = Vec::new();
    stream
        .take((MAX_CALLBACK_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|error| PlatformAdapterError::Transport(error.to_string()))?;
    if raw.len() > MAX_CALLBACK_BYTES {
        return Err(PlatformAdapterError::InvalidResponse(
            "platform adapter response is too large".into(),
        ));
    }
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| {
            PlatformAdapterError::InvalidResponse(
                "platform adapter returned an invalid HTTP response".into(),
            )
        })?;
    let head = std::str::from_utf8(&raw[..separator]).map_err(|_| {
        PlatformAdapterError::InvalidResponse(
            "platform adapter returned a non-UTF-8 response header".into(),
        )
    })?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|status| (100..=599).contains(status))
        .ok_or_else(|| {
            PlatformAdapterError::InvalidResponse(
                "platform adapter returned an invalid status".into(),
            )
        })?;
    let body = &raw[separator + 4..];
    let body = serde_json::from_slice(body).map_err(|_| {
        PlatformAdapterError::InvalidResponse(
            "platform adapter returned an invalid JSON body".into(),
        )
    })?;
    Ok(PlatformAdapterResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;

    fn registration(port: u16) -> PlatformAdapterRegistration {
        PlatformAdapterRegistration {
            version: PLATFORM_ADAPTER_VERSION,
            instance_id: "native-test".into(),
            callback_port: port,
            callback_token: "0123456789abcdef0123456789abcdef".into(),
            capabilities: vec!["session.notify_when_done.set".into()],
        }
    }

    #[test]
    fn registration_is_bounded_known_and_connection_scoped() {
        let hub = PlatformAdapterHub::default();
        let mut invalid = registration(42);
        invalid.capabilities = vec!["host.bootstrap".into()];
        assert!(hub.register(1, invalid).unwrap_err().contains("unknown"));
        assert!(!hub.supports("session.notify_when_done.set"));

        assert_eq!(
            hub.register(7, registration(42)).unwrap(),
            vec!["session.notify_when_done.set"]
        );
        assert!(hub.supports("session.notify_when_done.set"));
        let generation = hub.generation();
        assert!(hub.unregister(7));
        assert!(hub.generation() > generation);
        assert!(!hub.supports("session.notify_when_done.set"));
    }

    #[test]
    fn every_declared_platform_capability_can_register_in_ledger_order() {
        let hub = PlatformAdapterHub::default();
        let mut full = registration(42);
        full.capabilities = ADAPTER_HOST_CAPABILITIES
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect();
        assert_eq!(
            hub.register(8, full).unwrap(),
            ADAPTER_HOST_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn internal_services_register_but_never_leak_into_host_protocol() {
        let hub = PlatformAdapterHub::default();
        let mut service = registration(42);
        service.capabilities = ADAPTER_SERVICES
            .iter()
            .map(|service| (*service).to_owned())
            .collect();
        assert!(hub.register(9, service).unwrap().is_empty());
        for service in ADAPTER_SERVICES {
            assert!(hub.supports(service), "missing internal service {service}");
        }
        let mut protocol = HostProtocolDescriptor::headless_v1();
        let before = protocol.clone();
        hub.decorate_protocol(&mut protocol);
        assert_eq!(protocol, before);
    }

    #[test]
    fn protocol_addition_uses_canonical_order_and_preserves_transport_omissions() {
        let hub = PlatformAdapterHub::default();
        hub.register(1, registration(42)).unwrap();
        let mut protocol = HostProtocolDescriptor::headless_v1();
        protocol
            .capabilities
            .retain(|capability| capability != "approval.answer");
        let mut expected = protocol.capabilities.clone();
        expected.push("session.notify_when_done.set".into());
        expected.sort_by_key(|capability| {
            NATIVE_HOST_CAPABILITIES
                .iter()
                .position(|known| known == &capability.as_str())
                .unwrap_or(usize::MAX)
        });
        hub.decorate_protocol(&mut protocol);
        assert!(!protocol.supports("approval.answer"));
        assert!(protocol.supports("session.notify_when_done.set"));
        assert_eq!(protocol.capabilities, expected);
    }

    #[test]
    fn callback_is_authenticated_and_transport_failure_withdraws_capability() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let thread_capture = Arc::clone(&captured);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
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
                    break;
                }
            }
            *thread_capture.lock().unwrap() = request;
            let body = br#"{"ok":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let hub = PlatformAdapterHub::default();
        hub.register(9, registration(port)).unwrap();
        let response = hub
            .call(
                "session.notify_when_done.set",
                serde_json::json!({ "sessionID": "s1", "notifyWhenDone": true }),
            )
            .unwrap();
        assert_eq!(response.status, 200);
        server.join().unwrap();
        let request = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(request.contains(&format!("POST {PLATFORM_ADAPTER_CALLBACK_PATH}")));
        assert!(request.contains("Authorization: Bearer 0123456789abcdef0123456789abcdef"));
        assert!(request.contains("\"operation\":\"session.notify_when_done.set\""));

        let error = hub
            .call("session.notify_when_done.set", serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(error, PlatformAdapterError::Transport(_)));
        assert!(!hub.supports("session.notify_when_done.set"));
    }
}
