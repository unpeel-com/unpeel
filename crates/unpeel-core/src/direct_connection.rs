//! Controller-side direct/LAN HTTP transport for the shared Host contract.
//!
//! Pairing yields an `http://…/mobile` endpoint and a per-device bearer
//! token. This is the currently shipped paired-LAN transport; it provides no
//! TLS and must remain on a trusted LAN or trusted VPN. It must not be
//! confused with the separately pinned TLS/WSS `__remote__` service. This
//! module keeps the bearer at the transport boundary and turns [`HostCall`]
//! values into bounded HTTP/1.1 requests. Each call uses a fresh TCP
//! connection, but calls share a logical generation: any transport loss
//! invalidates it, and only a later unconstrained bootstrap call may open the
//! next generation. Effects are attempted exactly once.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::host_connection::{
    ConnectionGeneration, DeliveryState, HostCall, HostConnection, HostConnectionError, HostReply,
    PreparedHostCall, RequestSemantics,
};
use crate::relay_wire;

const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const MAX_RESPONSE_HEADER_BYTES: usize = 32 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = relay_wire::MAX_PLAINTEXT_BYTES;
const MAX_CHUNKED_WIRE_BYTES: usize = MAX_RESPONSE_BODY_BYTES * 2;

/// Parsed paired-LAN endpoint. V1 direct Controller traffic is deliberately
/// plain HTTP and is safe only on a trusted LAN/VPN. Off-LAN HTTP-shaped calls
/// travel inside Link's E2E tunnel; this type never upgrades to or claims the
/// security properties of the pinned TLS/WSS terminal service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectHostEndpoint {
    uri: String,
    host: String,
    host_header: String,
    port: u16,
    scope_path: String,
}

impl DirectHostEndpoint {
    /// Parse the exact endpoint shape emitted by pairing:
    /// `http://host[:port]/mobile`. HTTPS and other schemes are rejected
    /// instead of silently changing the direct transport's trust contract.
    pub fn parse(uri: &str) -> Result<Self, HostConnectionError> {
        let rest = uri.strip_prefix("http://").ok_or_else(|| {
            HostConnectionError::InvalidTarget(
                "direct Host endpoint must use http:// and end in /mobile".to_string(),
            )
        })?;
        if uri.len() > 2048 {
            return Err(invalid_endpoint("endpoint is too long"));
        }
        if rest.is_empty() || rest.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(invalid_endpoint(
                "endpoint is empty or contains control bytes",
            ));
        }
        if rest.contains(['?', '#']) {
            return Err(invalid_endpoint(
                "endpoint must not contain a query string or fragment",
            ));
        }
        let (authority, raw_path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_string()),
        };
        if authority.is_empty() || authority.len() > 512 || authority.contains('@') {
            return Err(invalid_endpoint(
                "endpoint authority is empty or contains user information",
            ));
        }

        let (host, host_header, port) = parse_authority(authority)?;
        let scope_path = raw_path.trim_end_matches('/');
        if scope_path != "/mobile" {
            return Err(invalid_endpoint("endpoint path must be exactly /mobile"));
        }

        Ok(Self {
            uri: uri.to_owned(),
            host,
            host_header,
            port,
            scope_path: scope_path.to_string(),
        })
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

fn invalid_endpoint(message: impl Into<String>) -> HostConnectionError {
    HostConnectionError::InvalidTarget(message.into())
}

fn parse_authority(authority: &str) -> Result<(String, String, u16), HostConnectionError> {
    if let Some(after_open) = authority.strip_prefix('[') {
        let close = after_open
            .find(']')
            .ok_or_else(|| invalid_endpoint("IPv6 endpoint is missing a closing bracket"))?;
        let host = &after_open[..close];
        if host.is_empty() || host.contains('%') || host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(invalid_endpoint(
                "endpoint contains an invalid IPv6 address",
            ));
        }
        let suffix = &after_open[close + 1..];
        let port = if suffix.is_empty() {
            80
        } else {
            let raw = suffix
                .strip_prefix(':')
                .ok_or_else(|| invalid_endpoint("invalid characters after IPv6 address"))?;
            parse_port(raw)?
        };
        return Ok((host.to_string(), format!("[{host}]{suffix}"), port));
    }

    if authority.matches(':').count() > 1 {
        return Err(invalid_endpoint(
            "IPv6 addresses must be enclosed in brackets",
        ));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, raw_port)) => (host, parse_port(raw_port)?),
        None => (authority, 80),
    };
    if host.is_empty()
        || host.bytes().any(|byte| {
            !byte.is_ascii()
                || byte.is_ascii_whitespace()
                || byte.is_ascii_control()
                || matches!(byte, b'/' | b'\\' | b'[' | b']')
        })
    {
        return Err(invalid_endpoint("endpoint contains an invalid host"));
    }
    Ok((host.to_string(), authority.to_string(), port))
}

fn parse_port(raw: &str) -> Result<u16, HostConnectionError> {
    let port = raw
        .parse::<u16>()
        .map_err(|_| invalid_endpoint("endpoint contains an invalid port"))?;
    if port == 0 {
        return Err(invalid_endpoint("endpoint port must not be zero"));
    }
    Ok(port)
}

struct HttpGeneration {
    id: u64,
    alive: AtomicBool,
    /// Serializes the tiny transition from "callable" to "dispatched" with
    /// invalidation. Once a call leaves this gate it is already in flight in
    /// the old generation; a later prepared call cannot follow it after loss.
    dispatch_gate: Mutex<()>,
    sockets: Mutex<HashMap<u64, TcpStream>>,
}

impl HttpGeneration {
    fn new(id: u64) -> Self {
        Self {
            id,
            alive: AtomicBool::new(true),
            dispatch_gate: Mutex::new(()),
            sockets: Mutex::new(HashMap::new()),
        }
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn register_socket(
        self: &Arc<Self>,
        request_id: u64,
        stream: &TcpStream,
    ) -> Result<ActiveSocketPermit, HttpFailure> {
        let _gate = self
            .dispatch_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.is_alive() {
            return Err(HttpFailure::GenerationChanged);
        }
        let socket = stream.try_clone().map_err(|error| HttpFailure::Io {
            delivery: DeliveryState::NotSent,
            message: format!("track direct Host request socket: {error}"),
        })?;
        let previous = self
            .sockets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(request_id, socket);
        debug_assert!(previous.is_none());
        Ok(ActiveSocketPermit {
            generation: Arc::clone(self),
            request_id,
        })
    }

    fn invalidate(&self) {
        let _gate = self
            .dispatch_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.alive.store(false, Ordering::Release);
        let sockets: Vec<TcpStream> = self
            .sockets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, socket)| socket)
            .collect();
        for socket in sockets {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

enum ReserveFailure {
    AtCapacity,
    Duplicate,
}

struct ActiveSocketPermit {
    generation: Arc<HttpGeneration>,
    request_id: u64,
}

impl Drop for ActiveSocketPermit {
    fn drop(&mut self) {
        self.generation
            .sockets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.request_id);
    }
}

struct InFlightPermit<'a> {
    in_flight: &'a Mutex<HashSet<u64>>,
    request_id: u64,
}

impl Drop for InFlightPermit<'_> {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.request_id);
    }
}

fn reserve_in_flight(
    in_flight: &Mutex<HashSet<u64>>,
    request_id: u64,
    limit: usize,
) -> Result<InFlightPermit<'_>, ReserveFailure> {
    let mut pending = in_flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if pending.len() >= limit {
        return Err(ReserveFailure::AtCapacity);
    }
    if !pending.insert(request_id) {
        return Err(ReserveFailure::Duplicate);
    }
    Ok(InFlightPermit {
        in_flight,
        request_id,
    })
}

struct ConnectionState {
    generation: Option<Arc<HttpGeneration>>,
}

/// One paired direct/LAN Controller connection.
pub struct DirectHostConnection {
    connection_id: uuid::Uuid,
    endpoint: DirectHostEndpoint,
    auth_token: String,
    closed: AtomicBool,
    state: Mutex<ConnectionState>,
    in_flight: Mutex<HashSet<u64>>,
    next_generation: AtomicU64,
    next_request_id: AtomicU64,
    max_in_flight: usize,
}

impl DirectHostConnection {
    pub fn new(
        endpoint: DirectHostEndpoint,
        auth_token: impl Into<String>,
    ) -> Result<Self, HostConnectionError> {
        Self::with_in_flight_limit(endpoint, auth_token.into(), MAX_IN_FLIGHT_REQUESTS)
    }

    fn with_in_flight_limit(
        endpoint: DirectHostEndpoint,
        auth_token: String,
        max_in_flight: usize,
    ) -> Result<Self, HostConnectionError> {
        if auth_token.is_empty()
            || auth_token.len() > 4096
            || auth_token.bytes().any(|byte| {
                !byte.is_ascii() || byte.is_ascii_whitespace() || byte.is_ascii_control()
            })
        {
            return Err(HostConnectionError::Configuration(
                "direct Host bearer token is empty or malformed".to_string(),
            ));
        }
        if max_in_flight == 0 {
            return Err(HostConnectionError::Configuration(
                "direct Host in-flight limit must be positive".to_string(),
            ));
        }
        Ok(Self {
            connection_id: uuid::Uuid::new_v4(),
            endpoint,
            auth_token,
            closed: AtomicBool::new(false),
            state: Mutex::new(ConnectionState { generation: None }),
            in_flight: Mutex::new(HashSet::new()),
            next_generation: AtomicU64::new(1),
            next_request_id: AtomicU64::new(1),
            max_in_flight,
        })
    }

    pub fn endpoint(&self) -> &DirectHostEndpoint {
        &self.endpoint
    }

    fn allocate_request_id(&self) -> Result<u64, HostConnectionError> {
        allocate_counter(&self.next_request_id).ok_or(HostConnectionError::RequestIdExhausted)
    }

    fn allocate_generation_id(&self) -> Result<u64, String> {
        allocate_counter(&self.next_generation)
            .ok_or_else(|| "direct Host generation id space is exhausted".to_string())
    }

    fn generation_token(&self, generation: &HttpGeneration) -> ConnectionGeneration {
        ConnectionGeneration {
            connection_id: self.connection_id,
            sequence: generation.id,
        }
    }

    fn current_live_generation(&self) -> Option<Arc<HttpGeneration>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
            .as_ref()
            .filter(|generation| generation.is_alive())
            .map(Arc::clone)
    }

    fn generation(&self) -> Result<Arc<HttpGeneration>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.closed.load(Ordering::Acquire) {
            return Err("Host connection is closed".to_string());
        }
        if let Some(generation) = state.generation.as_ref() {
            if generation.is_alive() {
                return Ok(Arc::clone(generation));
            }
        }
        let generation = Arc::new(HttpGeneration::new(self.allocate_generation_id()?));
        state.generation = Some(Arc::clone(&generation));
        Ok(generation)
    }

    fn invalidate(&self, generation: &Arc<HttpGeneration>) {
        generation.invalidate();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .generation
            .as_ref()
            .is_some_and(|current| current.id == generation.id)
        {
            state.generation = None;
        }
    }

    pub fn disconnect(&self) {
        self.closed.store(true, Ordering::Release);
        let generation = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
            .take();
        if let Some(generation) = generation {
            generation.invalidate();
        }
    }

    fn validate_call(&self, call: &HostCall) -> Result<(), HostConnectionError> {
        if !matches!(call.method.as_str(), "GET" | "POST") {
            return Err(HostConnectionError::Configuration(
                "direct Host calls support only GET and POST".to_string(),
            ));
        }
        let scoped_prefix = format!("{}/", self.endpoint.scope_path);
        if !call.path.starts_with(&scoped_prefix) || call.path == "/mobile/pair" {
            return Err(HostConnectionError::Configuration(
                "direct Host call is outside the paired /mobile scope".to_string(),
            ));
        }
        let route = call
            .path
            .strip_prefix(&scoped_prefix)
            .expect("checked direct Host scope prefix");
        if route.is_empty()
            || route
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            return Err(HostConnectionError::Configuration(
                "direct Host call contains an empty or relative path segment".to_string(),
            ));
        }
        if call.path.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~'))
        }) {
            return Err(HostConnectionError::Configuration(
                "direct Host call contains an invalid path".to_string(),
            ));
        }
        if let Some(content_type) = &call.content_type {
            if content_type.is_empty()
                || content_type.len() > 256
                || content_type
                    .bytes()
                    .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
            {
                return Err(HostConnectionError::Configuration(
                    "direct Host call contains an invalid content type".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn perform_http(
        &self,
        generation: &Arc<HttpGeneration>,
        request: &crate::relay_wire::TunnelRequest,
        request_head: &[u8],
        timeout: Duration,
    ) -> Result<HttpResponse, HttpFailure> {
        let deadline = Deadline::new(timeout)?;

        let mut stream = connect(&self.endpoint.host, self.endpoint.port, &deadline)?;
        let _active_socket = generation.register_socket(request.id, &stream)?;
        let mut wrote_any = false;
        write_all_deadline(&mut stream, request_head, &deadline, &mut wrote_any)?;
        write_all_deadline(&mut stream, &request.body, &deadline, &mut wrote_any)?;
        if let Err(error) = stream.flush() {
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) {
                return Err(HttpFailure::Timeout {
                    delivery: delivery(wrote_any),
                });
            }
            return Err(HttpFailure::Io {
                delivery: delivery(wrote_any),
                message: format!("flush direct Host request: {error}"),
            });
        }
        read_response(&mut stream, &deadline).map_err(|failure| failure.with_sent_request())
    }
}

impl HostConnection for DirectHostConnection {
    fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::Closed);
        }
        self.validate_call(&call)?;
        Ok(PreparedHostCall {
            connection_id: self.connection_id,
            request_id: self.allocate_request_id()?,
            required_generation: None,
            call,
        })
    }

    fn prepare_in_generation(
        &self,
        generation: ConnectionGeneration,
        call: HostCall,
    ) -> Result<PreparedHostCall, HostConnectionError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::Closed);
        }
        self.validate_call(&call)?;
        if generation.connection_id != self.connection_id {
            return Err(HostConnectionError::WrongGeneration(generation));
        }
        let request_id = self.allocate_request_id()?;
        let current = self.current_live_generation();
        if current
            .as_ref()
            .is_none_or(|current| current.id != generation.sequence)
        {
            return Err(HostConnectionError::GenerationChanged {
                request_id,
                expected: generation,
            });
        }
        Ok(PreparedHostCall {
            connection_id: self.connection_id,
            request_id,
            required_generation: Some(generation),
            call,
        })
    }

    fn request(
        &self,
        call: PreparedHostCall,
        timeout: Duration,
    ) -> Result<HostReply, HostConnectionError> {
        let request_id = call.request_id;
        let semantics = call.call.semantics;
        let required_generation = call.required_generation;
        if call.connection_id != self.connection_id {
            return Err(HostConnectionError::WrongConnection(request_id));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::ClosedRequest(request_id));
        }
        let request = call.into_tunnel();
        let encoded = relay_wire::encode_tunnel_request(&request);
        if !relay_wire::plaintext_frame_fits(encoded.len()) {
            return Err(HostConnectionError::RequestTooLarge {
                request_id,
                encoded_bytes: encoded.len(),
                max_bytes: relay_wire::MAX_PLAINTEXT_BYTES,
            });
        }
        let target = request_target(&request.path, &request.query);
        let request_head = build_request_head(
            &request,
            &target,
            &self.endpoint.host_header,
            &self.auth_token,
            self.connection_id,
        );
        let http_bytes = request_head.len().saturating_add(request.body.len());
        if http_bytes > relay_wire::MAX_PLAINTEXT_BYTES {
            return Err(HostConnectionError::RequestTooLarge {
                request_id,
                encoded_bytes: http_bytes,
                max_bytes: relay_wire::MAX_PLAINTEXT_BYTES,
            });
        }

        let generation = match required_generation {
            Some(expected) => match self.current_live_generation() {
                Some(current) if current.id == expected.sequence => current,
                _ => {
                    return Err(HostConnectionError::GenerationChanged {
                        request_id,
                        expected,
                    });
                }
            },
            None => match self.generation() {
                Ok(generation) => generation,
                Err(_) if self.closed.load(Ordering::Acquire) => {
                    return Err(HostConnectionError::ClosedRequest(request_id));
                }
                Err(message) => {
                    return Err(HostConnectionError::Launch {
                        request_id,
                        message,
                    });
                }
            },
        };
        let _permit = match reserve_in_flight(&self.in_flight, request_id, self.max_in_flight) {
            Ok(permit) => permit,
            Err(ReserveFailure::AtCapacity) => {
                return Err(HostConnectionError::TooManyInFlight {
                    request_id,
                    limit: self.max_in_flight,
                });
            }
            Err(ReserveFailure::Duplicate) => {
                return Err(HostConnectionError::DuplicateRequestId(request_id));
            }
        };
        if !generation.is_alive() {
            return Err(HostConnectionError::GenerationChanged {
                request_id,
                expected: self.generation_token(&generation),
            });
        }

        match self.perform_http(&generation, &request, request_head.as_bytes(), timeout) {
            Ok(response) => Ok(HostReply {
                request_id,
                generation: self.generation_token(&generation),
                status: response.status,
                body: response.body,
            }),
            Err(HttpFailure::GenerationChanged) => Err(HostConnectionError::GenerationChanged {
                request_id,
                expected: self.generation_token(&generation),
            }),
            Err(failure) => {
                self.invalidate(&generation);
                Err(failure.into_public(request_id, semantics))
            }
        }
    }

    fn disconnect(&self) {
        DirectHostConnection::disconnect(self);
    }
}

impl Drop for DirectHostConnection {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn allocate_counter(counter: &AtomicU64) -> Option<u64> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current == u64::MAX {
            return None;
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(current),
            Err(actual) => current = actual,
        }
    }
}

fn request_target(path: &str, query: &[(String, String)]) -> String {
    let mut target = path.to_string();
    for (index, (key, value)) in query.iter().enumerate() {
        target.push(if index == 0 { '?' } else { '&' });
        percent_encode_component(&mut target, key.as_bytes());
        target.push('=');
        percent_encode_component(&mut target, value.as_bytes());
    }
    target
}

fn percent_encode_component(out: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
}

fn build_request_head(
    request: &crate::relay_wire::TunnelRequest,
    target: &str,
    host_header: &str,
    auth_token: &str,
    request_namespace: uuid::Uuid,
) -> String {
    let mut head = format!(
        "{} {target} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: unpeel/{}\r\nAccept: application/json\r\nAuthorization: Bearer {auth_token}\r\nX-Unpeel-Request-ID: {}\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n",
        request.method,
        env!("CARGO_PKG_VERSION"),
        format_args!("{request_namespace}:{}", request.id),
        request.body.len(),
    );
    if let Some(content_type) = &request.content_type {
        head.push_str("Content-Type: ");
        head.push_str(content_type);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    head
}

struct Deadline(Instant);

impl Deadline {
    fn new(timeout: Duration) -> Result<Self, HttpFailure> {
        if timeout.is_zero() {
            return Err(HttpFailure::Timeout {
                delivery: DeliveryState::NotSent,
            });
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(HttpFailure::Timeout {
                delivery: DeliveryState::NotSent,
            })?;
        Ok(Self(deadline))
    }

    fn remaining(&self, sent: bool) -> Result<Duration, HttpFailure> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or(HttpFailure::Timeout {
                delivery: delivery(sent),
            })
    }
}

fn connect(host: &str, port: u16, deadline: &Deadline) -> Result<TcpStream, HttpFailure> {
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|error| HttpFailure::Io {
            delivery: DeliveryState::NotSent,
            message: format!("resolve direct Host {host}: {error}"),
        })?
        .collect();
    if addresses.is_empty() {
        return Err(HttpFailure::Io {
            delivery: DeliveryState::NotSent,
            message: format!("resolve direct Host {host}: no addresses"),
        });
    }
    let mut last_error = None;
    for address in addresses {
        let remaining = deadline.remaining(false)?;
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }
    let error = last_error.expect("non-empty direct Host address list");
    if error.kind() == io::ErrorKind::TimedOut {
        Err(HttpFailure::Timeout {
            delivery: DeliveryState::NotSent,
        })
    } else {
        Err(HttpFailure::Io {
            delivery: DeliveryState::NotSent,
            message: format!("connect to direct Host: {error}"),
        })
    }
}

fn write_all_deadline(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: &Deadline,
    wrote_any: &mut bool,
) -> Result<(), HttpFailure> {
    let mut offset = 0;
    while offset < bytes.len() {
        stream
            .set_write_timeout(Some(deadline.remaining(*wrote_any)?))
            .map_err(|error| HttpFailure::Io {
                delivery: delivery(*wrote_any),
                message: format!("set direct Host write timeout: {error}"),
            })?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(HttpFailure::Io {
                    delivery: delivery(*wrote_any),
                    message: "direct Host connection closed during request write".to_string(),
                });
            }
            Ok(count) => {
                offset += count;
                *wrote_any = true;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(HttpFailure::Timeout {
                    delivery: delivery(*wrote_any),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(HttpFailure::Io {
                    delivery: delivery(*wrote_any),
                    message: format!("write direct Host request: {error}"),
                });
            }
        }
    }
    Ok(())
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

fn read_response(stream: &mut TcpStream, deadline: &Deadline) -> Result<HttpResponse, HttpFailure> {
    let mut buffer = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() >= MAX_RESPONSE_HEADER_BYTES {
            return Err(HttpFailure::protocol(
                "direct Host response headers are too large",
            ));
        }
        let remaining = MAX_RESPONSE_HEADER_BYTES - buffer.len();
        read_some(stream, deadline, &mut buffer, remaining.min(8192), false)?;
    };

    let head = std::str::from_utf8(&buffer[..header_end - 4])
        .map_err(|_| HttpFailure::protocol("direct Host response headers are not UTF-8"))?;
    let parsed = parse_response_head(head)?;
    let mut body_prefix = buffer.split_off(header_end);
    if body_prefix.len() > MAX_RESPONSE_BODY_BYTES {
        return Err(HttpFailure::protocol(
            "direct Host response body is too large",
        ));
    }

    let body = if parsed.chunked {
        read_chunked_body(stream, deadline, body_prefix)?
    } else if let Some(length) = parsed.content_length {
        if length > MAX_RESPONSE_BODY_BYTES {
            return Err(HttpFailure::protocol(
                "direct Host response body is too large",
            ));
        }
        while body_prefix.len() < length {
            let missing = length - body_prefix.len();
            read_some(stream, deadline, &mut body_prefix, missing.min(8192), true)?;
        }
        body_prefix.truncate(length);
        body_prefix
    } else {
        read_to_eof_bounded(stream, deadline, body_prefix)?
    };
    Ok(HttpResponse {
        status: parsed.status,
        body,
    })
}

struct ParsedResponseHead {
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
}

fn parse_response_head(head: &str) -> Result<ParsedResponseHead, HttpFailure> {
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| HttpFailure::protocol("direct Host response has no status line"))?;
    if status_line
        .bytes()
        .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
    {
        return Err(HttpFailure::protocol(
            "direct Host response has an invalid status line",
        ));
    }
    let mut status_parts = status_line.split_whitespace();
    let version = status_parts.next().unwrap_or_default();
    let status_text = status_parts.next().unwrap_or_default();
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || status_text.len() != 3
        || !status_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(HttpFailure::protocol(
            "direct Host response has an invalid status line",
        ));
    }
    let status = status_text
        .parse::<u16>()
        .map_err(|_| HttpFailure::protocol("direct Host response has an invalid status"))?;
    if !(100..=599).contains(&status) || status < 200 {
        return Err(HttpFailure::protocol(
            "direct Host response has an unsupported status",
        ));
    }

    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| HttpFailure::protocol("direct Host response has a malformed header"))?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || !name.bytes().all(is_http_token_byte) {
            return Err(HttpFailure::protocol(
                "direct Host response has an invalid header name",
            ));
        }
        if value
            .bytes()
            .any(|byte| (!byte.is_ascii() || byte.is_ascii_control()) && byte != b'\t')
        {
            return Err(HttpFailure::protocol(
                "direct Host response has an invalid header value",
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value.parse::<usize>().map_err(|_| {
                HttpFailure::protocol("direct Host response has an invalid content length")
            })?;
            if content_length
                .replace(parsed)
                .is_some_and(|previous| previous != parsed)
            {
                return Err(HttpFailure::protocol(
                    "direct Host response has conflicting content lengths",
                ));
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if !value.eq_ignore_ascii_case("chunked") {
                return Err(HttpFailure::protocol(
                    "direct Host response uses an unsupported transfer encoding",
                ));
            }
            chunked = true;
        }
    }
    if chunked && content_length.is_some() {
        return Err(HttpFailure::protocol(
            "direct Host response has both transfer encoding and content length",
        ));
    }
    Ok(ParsedResponseHead {
        status,
        content_length,
        chunked,
    })
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn read_some(
    stream: &mut TcpStream,
    deadline: &Deadline,
    out: &mut Vec<u8>,
    max: usize,
    sent: bool,
) -> Result<(), HttpFailure> {
    if max == 0 {
        return Err(HttpFailure::protocol(
            "direct Host response read exceeded its bound",
        ));
    }
    let mut chunk = [0u8; 8192];
    let read_limit = max.min(chunk.len());
    loop {
        stream
            .set_read_timeout(Some(deadline.remaining(sent)?))
            .map_err(|error| HttpFailure::Io {
                delivery: delivery(sent),
                message: format!("set direct Host read timeout: {error}"),
            })?;
        match stream.read(&mut chunk[..read_limit]) {
            Ok(0) => {
                return Err(HttpFailure::Eof {
                    delivery: delivery(sent),
                });
            }
            Ok(count) => {
                out.extend_from_slice(&chunk[..count]);
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(HttpFailure::Timeout {
                    delivery: delivery(sent),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(HttpFailure::Io {
                    delivery: delivery(sent),
                    message: format!("read direct Host response: {error}"),
                });
            }
        }
    }
}

fn read_to_eof_bounded(
    stream: &mut TcpStream,
    deadline: &Deadline,
    mut body: Vec<u8>,
) -> Result<Vec<u8>, HttpFailure> {
    loop {
        if body.len() == MAX_RESPONSE_BODY_BYTES {
            let mut one = Vec::new();
            match read_some(stream, deadline, &mut one, 1, true) {
                Ok(()) => {
                    return Err(HttpFailure::protocol(
                        "direct Host response body is too large",
                    ));
                }
                Err(HttpFailure::Eof { .. }) => return Ok(body),
                Err(error) => return Err(error),
            }
        }
        let before = body.len();
        let read_limit = (MAX_RESPONSE_BODY_BYTES - body.len()).min(8192);
        match read_some(stream, deadline, &mut body, read_limit, true) {
            Ok(()) => debug_assert!(body.len() > before),
            Err(HttpFailure::Eof { .. }) => return Ok(body),
            Err(error) => return Err(error),
        }
    }
}

fn read_chunked_body(
    stream: &mut TcpStream,
    deadline: &Deadline,
    mut wire: Vec<u8>,
) -> Result<Vec<u8>, HttpFailure> {
    let mut body = Vec::new();
    let mut consumed = 0usize;
    loop {
        let line_end = loop {
            if let Some(relative) = wire[consumed..]
                .windows(2)
                .position(|window| window == b"\r\n")
            {
                break consumed + relative;
            }
            if wire.len() >= MAX_CHUNKED_WIRE_BYTES {
                return Err(HttpFailure::protocol(
                    "direct Host chunked response is too large",
                ));
            }
            let read_limit = (MAX_CHUNKED_WIRE_BYTES - wire.len()).min(8192);
            read_some(stream, deadline, &mut wire, read_limit, true)?;
        };
        let size_text = std::str::from_utf8(&wire[consumed..line_end])
            .map_err(|_| HttpFailure::protocol("direct Host chunk size is not UTF-8"))?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        if size_text.is_empty() || size_text.len() > 16 {
            return Err(HttpFailure::protocol("direct Host chunk size is invalid"));
        }
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| HttpFailure::protocol("direct Host chunk size is invalid"))?;
        consumed = line_end + 2;
        if size == 0 {
            // The shipped Hosts send no trailers. Accept an immediate final
            // CRLF, and reject arbitrary bytes as a malformed response.
            while wire.len() < consumed + 2 {
                read_some(stream, deadline, &mut wire, 2, true)?;
            }
            if &wire[consumed..consumed + 2] != b"\r\n" {
                return Err(HttpFailure::protocol(
                    "direct Host chunked response has malformed trailers",
                ));
            }
            if wire.len() != consumed + 2 {
                return Err(HttpFailure::protocol(
                    "direct Host chunked response has bytes after its terminator",
                ));
            }
            return Ok(body);
        }
        let next_len = body
            .len()
            .checked_add(size)
            .ok_or_else(|| HttpFailure::protocol("direct Host response body is too large"))?;
        if next_len > MAX_RESPONSE_BODY_BYTES {
            return Err(HttpFailure::protocol(
                "direct Host response body is too large",
            ));
        }
        let required = consumed
            .checked_add(size)
            .and_then(|value| value.checked_add(2))
            .ok_or_else(|| HttpFailure::protocol("direct Host chunk is too large"))?;
        while wire.len() < required {
            if wire.len() >= MAX_CHUNKED_WIRE_BYTES {
                return Err(HttpFailure::protocol(
                    "direct Host chunked response is too large",
                ));
            }
            let read_limit = (MAX_CHUNKED_WIRE_BYTES - wire.len()).min(8192);
            read_some(stream, deadline, &mut wire, read_limit, true)?;
        }
        if &wire[consumed + size..required] != b"\r\n" {
            return Err(HttpFailure::protocol(
                "direct Host chunk is missing its terminator",
            ));
        }
        body.extend_from_slice(&wire[consumed..consumed + size]);
        consumed = required;
        if consumed > MAX_RESPONSE_HEADER_BYTES {
            wire.drain(..consumed);
            consumed = 0;
        }
    }
}

enum HttpFailure {
    GenerationChanged,
    Eof {
        delivery: DeliveryState,
    },
    Timeout {
        delivery: DeliveryState,
    },
    Io {
        delivery: DeliveryState,
        message: String,
    },
}

impl HttpFailure {
    fn protocol(message: impl Into<String>) -> Self {
        Self::Io {
            delivery: DeliveryState::NotSent,
            message: message.into(),
        }
    }

    fn with_sent_request(self) -> Self {
        match self {
            Self::GenerationChanged => Self::GenerationChanged,
            Self::Eof { .. } => Self::Eof {
                delivery: DeliveryState::OutcomeUnknown,
            },
            Self::Timeout { .. } => Self::Timeout {
                delivery: DeliveryState::OutcomeUnknown,
            },
            Self::Io { message, .. } => Self::Io {
                delivery: DeliveryState::OutcomeUnknown,
                message,
            },
        }
    }

    fn into_public(self, request_id: u64, semantics: RequestSemantics) -> HostConnectionError {
        match self {
            Self::GenerationChanged => {
                unreachable!("generation changes are translated with their expected token")
            }
            Self::Eof { delivery } => HostConnectionError::Disconnected {
                request_id,
                semantics,
                delivery,
                message: "direct Host closed before the response was complete".to_string(),
            },
            Self::Timeout { delivery } => HostConnectionError::TimedOut {
                request_id,
                semantics,
                delivery,
            },
            Self::Io { delivery, message } => HostConnectionError::Disconnected {
                request_id,
                semantics,
                delivery,
                message,
            },
        }
    }
}

fn delivery(wrote_any: bool) -> DeliveryState {
    if wrote_any {
        DeliveryState::OutcomeUnknown
    } else {
        DeliveryState::NotSent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    struct TestServer {
        endpoint: DirectHostEndpoint,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn scripted<F>(connection_count: usize, script: F) -> Self
        where
            F: Fn(usize, TcpStream) + Send + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let endpoint =
                DirectHostEndpoint::parse(&format!("http://127.0.0.1:{port}/mobile")).unwrap();
            let thread = thread::spawn(move || {
                for index in 0..connection_count {
                    let (stream, _) = listener.accept().unwrap();
                    script(index, stream);
                }
            });
            Self {
                endpoint,
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if !thread::panicking() {
                self.thread.take().unwrap().join().unwrap();
            }
        }
    }

    struct CapturedRequest {
        head: String,
        body: Vec<u8>,
    }

    fn capture_request(stream: &mut TcpStream) -> CapturedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            let mut chunk = [0u8; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
        };
        let head = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let length = head
            .lines()
            .find_map(|line| {
                line.strip_prefix("Content-Length: ")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap();
        while bytes.len() < header_end + length {
            let mut chunk = [0u8; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
        }
        CapturedRequest {
            head,
            body: bytes[header_end..header_end + length].to_vec(),
        }
    }

    fn respond(stream: &mut TcpStream, status: u16, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    fn wire_request_id(head: &str) -> String {
        head.lines()
            .find_map(|line| line.strip_prefix("X-Unpeel-Request-ID: "))
            .expect("request id header")
            .trim()
            .to_string()
    }

    fn call(
        connection: &DirectHostConnection,
        generation: Option<ConnectionGeneration>,
        call: HostCall,
    ) -> Result<HostReply, HostConnectionError> {
        let prepared = match generation {
            Some(generation) => connection.prepare_in_generation(generation, call)?,
            None => connection.prepare(call)?,
        };
        connection.request(prepared, Duration::from_secs(2))
    }

    #[test]
    fn endpoint_accepts_only_plain_http_mobile_scope() {
        let endpoint = DirectHostEndpoint::parse("http://host.local:1234/mobile/").unwrap();
        assert_eq!(endpoint.host(), "host.local");
        assert_eq!(endpoint.port(), 1234);
        assert_eq!(endpoint.uri(), "http://host.local:1234/mobile/");
        assert!(DirectHostEndpoint::parse("https://host.local/mobile").is_err());
        assert!(DirectHostEndpoint::parse("http://host.local/other").is_err());
        assert!(DirectHostEndpoint::parse("http://user@host.local/mobile").is_err());
        assert!(DirectHostEndpoint::parse("http://host.local/mobile?q=1").is_err());
        assert!(DirectHostEndpoint::parse("http://host.local/mobile#fragment").is_err());
    }

    #[test]
    fn bootstrap_output_and_effect_preserve_http_shape_and_auth() {
        let namespace = Arc::new(Mutex::new(None::<String>));
        let observed_namespace = Arc::clone(&namespace);
        let server = TestServer::scripted(3, move |index, mut stream| {
            let request = capture_request(&mut stream);
            assert!(request
                .head
                .contains("Authorization: Bearer device-secret\r\n"));
            let wire_id = wire_request_id(&request.head);
            let (prefix, counter) = wire_id.rsplit_once(':').expect("namespaced request id");
            uuid::Uuid::parse_str(prefix).expect("UUID connection namespace");
            assert_eq!(counter, (index + 1).to_string());
            let mut prior = observed_namespace.lock().unwrap();
            if let Some(prior) = prior.as_ref() {
                assert_eq!(prior, prefix);
            } else {
                *prior = Some(prefix.to_string());
            }
            match index {
                0 => {
                    assert!(request
                        .head
                        .starts_with("GET /mobile/bootstrap HTTP/1.1\r\n"));
                    assert!(request.body.is_empty());
                    respond(&mut stream, 200, br#"{"hostID":"host-1"}"#);
                }
                1 => {
                    assert!(request.head.starts_with(
                        "GET /mobile/output?session_id=s%201%2F%3F&limit=3 HTTP/1.1\r\n"
                    ));
                    assert!(request.body.is_empty());
                    respond(&mut stream, 200, br#"{"dataBase64":"b2s="}"#);
                }
                2 => {
                    assert!(request.head.starts_with("POST /mobile/write HTTP/1.1\r\n"));
                    assert!(request.head.contains("Content-Type: application/json\r\n"));
                    assert_eq!(request.body, br#"{"sessionID":"s1","data":"x"}"#);
                    respond(&mut stream, 200, br#"{"ok":true}"#);
                }
                _ => unreachable!(),
            }
        });
        let connection =
            DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap();
        let bootstrap = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        assert_eq!(bootstrap.body, br#"{"hostID":"host-1"}"#);

        let output = call(
            &connection,
            Some(bootstrap.generation),
            HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly)
                .with_query("session_id", "s 1/?")
                .with_query("limit", "3"),
        )
        .unwrap();
        assert_eq!(output.generation, bootstrap.generation);
        let effect = call(
            &connection,
            Some(bootstrap.generation),
            HostCall::new("POST", "/mobile/write", RequestSemantics::Effect).with_body(
                "application/json",
                br#"{"sessionID":"s1","data":"x"}"#.to_vec(),
            ),
        )
        .unwrap();
        assert_eq!(effect.generation, bootstrap.generation);
    }

    #[test]
    fn fresh_direct_connections_never_collide_in_host_replay_cache() {
        let seen = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
        let applied = Arc::new(AtomicU64::new(0));
        let captured_ids = Arc::new(Mutex::new(Vec::<String>::new()));
        let server_seen = Arc::clone(&seen);
        let server_applied = Arc::clone(&applied);
        let server_ids = Arc::clone(&captured_ids);
        let server = TestServer::scripted(2, move |_, mut stream| {
            let request = capture_request(&mut stream);
            let request_id = wire_request_id(&request.head);
            server_ids.lock().unwrap().push(request_id.clone());
            let mut cache = server_seen.lock().unwrap();
            match cache.get(&request_id) {
                Some(prior) if prior != &request.body => {
                    respond(&mut stream, 409, br#"{"error":"request id reused"}"#);
                }
                Some(_) => respond(&mut stream, 200, br#"{"ok":true,"replayed":true}"#),
                None => {
                    cache.insert(request_id, request.body.clone());
                    server_applied.fetch_add(1, Ordering::Relaxed);
                    respond(&mut stream, 200, br#"{"ok":true}"#);
                }
            }
        });

        let first = DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap();
        let second = DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap();
        for (connection, body) in [
            (&first, br#"{"sessionID":"s1","data":"a"}"#.as_slice()),
            (&second, br#"{"sessionID":"s1","data":"b"}"#.as_slice()),
        ] {
            let reply = call(
                connection,
                None,
                HostCall::new("POST", "/mobile/write", RequestSemantics::Effect)
                    .with_body("application/json", body.to_vec()),
            )
            .unwrap();
            assert_eq!(reply.status, 200);
        }

        assert_eq!(applied.load(Ordering::Relaxed), 2);
        let ids = captured_ids.lock().unwrap();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert!(ids.iter().all(|id| id.ends_with(":1")));
    }

    #[test]
    fn remote_session_backend_runs_bootstrap_output_and_effect_over_direct_http() {
        let server = TestServer::scripted(3, |index, mut stream| {
            let request = capture_request(&mut stream);
            match index {
                0 => respond(
                    &mut stream,
                    200,
                    serde_json::to_string(&serde_json::json!({
                        "protocolVersion": 1,
                        "macID": "host-1",
                        "macName": "TUI Host",
                        "folders": [],
                        "projects": [],
                        "presets": [],
                        "sessions": [{
                            "id": "s1",
                            "projectID": "p1",
                            "providerID": "claude",
                            "title": "Research",
                            "command": "claude",
                            "createdAtUnixMs": 1,
                            "status": "running",
                            "activity": "working"
                        }],
                        "capturedAtUnixMs": 10,
                        "hostProtocol": {
                            "majorVersion": 1,
                            "minorVersion": 0,
                            "capabilities": [
                                "host.bootstrap",
                                "session.output.read",
                                "session.input.write"
                            ]
                        }
                    }))
                    .unwrap()
                    .as_bytes(),
                ),
                1 => {
                    assert!(request.head.starts_with("GET /mobile/output?"));
                    respond(
                        &mut stream,
                        200,
                        br#"{"sessionID":"s1","offset":0,"nextOffset":2,"dataBase64":"aGk=","truncated":false,"capturedAtUnixMs":20}"#,
                    );
                }
                2 => {
                    assert!(request.head.starts_with("POST /mobile/write "));
                    assert_eq!(request.body, br#"{"sessionID":"s1","data":"hello"}"#);
                    respond(&mut stream, 200, br#"{"ok":true}"#);
                }
                _ => unreachable!(),
            }
        });
        let connection =
            Arc::new(DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap());
        let backend = crate::remote_session_backend::RemoteSessionBackend::new(connection);

        let bootstrap = backend.bootstrap().unwrap();
        assert_eq!(bootstrap.snapshot.host_id.as_deref(), Some("host-1"));
        let page = backend
            .poll_output(
                "s1",
                crate::remote_session_backend::RemoteOutputPollOptions::default(),
            )
            .unwrap();
        assert_eq!(page.bytes(), b"hi");
        page.commit().unwrap();
        assert_eq!(backend.committed_output_offset("s1"), Some(2));
        assert!(backend.write_terminal("s1", "hello").is_ok());
    }

    #[test]
    fn authorization_failure_is_a_semantic_reply() {
        let server = TestServer::scripted(1, |_index, mut stream| {
            let request = capture_request(&mut stream);
            assert!(request
                .head
                .contains("Authorization: Bearer wrong-device\r\n"));
            respond(&mut stream, 401, br#"{"error":"unauthorized"}"#);
        });
        let connection =
            DirectHostConnection::new(server.endpoint.clone(), "wrong-device").unwrap();
        let reply = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        assert_eq!(reply.status, 401);
        assert_eq!(reply.body, br#"{"error":"unauthorized"}"#);
    }

    #[test]
    fn transport_loss_invalidates_generation_and_effect_is_not_replayed() {
        let effect_count = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&effect_count);
        let server = TestServer::scripted(3, move |index, mut stream| {
            let request = capture_request(&mut stream);
            match index {
                0 => respond(&mut stream, 200, b"{}"),
                1 => {
                    assert!(request.head.starts_with("POST /mobile/write "));
                    observed.fetch_add(1, Ordering::SeqCst);
                    // Drop without a response: the effect may have landed.
                }
                2 => respond(&mut stream, 200, b"{}"),
                _ => unreachable!(),
            }
        });
        let connection =
            DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap();
        let first = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        let error = call(
            &connection,
            Some(first.generation),
            HostCall::new("POST", "/mobile/write", RequestSemantics::Effect)
                .with_body("application/json", b"{}".to_vec()),
        )
        .unwrap_err();
        assert!(error.effect_outcome_is_unknown());
        assert_eq!(effect_count.load(Ordering::SeqCst), 1);

        let stale = connection
            .prepare_in_generation(
                first.generation,
                HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly),
            )
            .unwrap_err();
        assert!(matches!(
            stale,
            HostConnectionError::GenerationChanged { .. }
        ));

        let second = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        assert_ne!(second.generation, first.generation);
        assert_eq!(effect_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn malformed_and_oversized_responses_fail_closed() {
        for response in [
            b"NOT HTTP\r\n\r\n".to_vec(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_RESPONSE_BODY_BYTES + 1
            )
            .into_bytes(),
        ] {
            let server = TestServer::scripted(1, move |_index, mut stream| {
                let _ = capture_request(&mut stream);
                stream.write_all(&response).unwrap();
            });
            let connection =
                DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap();
            let error = call(
                &connection,
                None,
                HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                HostConnectionError::Disconnected {
                    delivery: DeliveryState::OutcomeUnknown,
                    ..
                }
            ));
        }
    }

    #[test]
    fn timeout_invalidates_generation_without_retrying_effect() {
        let server = TestServer::scripted(2, |index, mut stream| {
            let _ = capture_request(&mut stream);
            if index == 0 {
                respond(&mut stream, 200, b"{}");
            } else {
                thread::sleep(Duration::from_millis(150));
            }
        });
        let connection =
            DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap();
        let bootstrap = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        let prepared = connection
            .prepare_in_generation(
                bootstrap.generation,
                HostCall::new("POST", "/mobile/mark-read", RequestSemantics::Effect)
                    .with_body("application/json", b"{}".to_vec()),
            )
            .unwrap();
        let error = connection
            .request(prepared, Duration::from_millis(30))
            .unwrap_err();
        assert!(matches!(error, HostConnectionError::TimedOut { .. }));
        assert!(error.effect_outcome_is_unknown());
        assert!(connection
            .prepare_in_generation(
                bootstrap.generation,
                HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly),
            )
            .is_err());
    }

    #[test]
    fn in_flight_limit_rejects_before_opening_another_socket() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let server = TestServer::scripted(2, move |index, mut stream| {
            let _ = capture_request(&mut stream);
            if index == 0 {
                respond(&mut stream, 200, b"{}");
            } else {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                respond(&mut stream, 200, b"{}");
            }
        });
        let connection = Arc::new(
            DirectHostConnection::with_in_flight_limit(
                server.endpoint.clone(),
                "device-secret".to_string(),
                1,
            )
            .unwrap(),
        );
        let bootstrap = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        let first_connection = Arc::clone(&connection);
        let generation = bootstrap.generation;
        let first = thread::spawn(move || {
            call(
                &first_connection,
                Some(generation),
                HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly),
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = call(
            &connection,
            Some(generation),
            HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly),
        )
        .unwrap_err();
        assert!(matches!(
            second,
            HostConnectionError::TooManyInFlight { limit: 1, .. }
        ));
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
    }

    #[test]
    fn disconnect_cancels_an_active_request_and_closes_the_connection() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let server = TestServer::scripted(2, move |index, mut stream| {
            let _ = capture_request(&mut stream);
            if index == 0 {
                respond(&mut stream, 200, b"{}");
            } else {
                entered_tx.send(()).unwrap();
                let mut byte = [0u8; 1];
                assert_eq!(stream.read(&mut byte).unwrap(), 0);
            }
        });
        let connection =
            Arc::new(DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap());
        let bootstrap = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        let active_connection = Arc::clone(&connection);
        let active = thread::spawn(move || {
            call(
                &active_connection,
                Some(bootstrap.generation),
                HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly),
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        connection.disconnect();
        assert!(matches!(
            active.join().unwrap().unwrap_err(),
            HostConnectionError::Disconnected { .. }
        ));
        assert_eq!(
            connection
                .prepare(HostCall::new(
                    "GET",
                    "/mobile/bootstrap",
                    RequestSemantics::ReadOnly,
                ))
                .unwrap_err(),
            HostConnectionError::Closed
        );
    }

    #[test]
    fn response_framing_supports_chunked_and_connection_close() {
        let server = TestServer::scripted(2, |index, mut stream| {
            let _ = capture_request(&mut stream);
            if index == 0 {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ntest\r\n0\r\n\r\n",
                    )
                    .unwrap();
            } else {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ndone")
                    .unwrap();
            }
        });
        let connection =
            DirectHostConnection::new(server.endpoint.clone(), "device-secret").unwrap();
        let first = call(
            &connection,
            None,
            HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly),
        )
        .unwrap();
        assert_eq!(first.body, b"test");
        let second = call(
            &connection,
            Some(first.generation),
            HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly),
        )
        .unwrap();
        assert_eq!(second.body, b"done");
    }

    #[test]
    fn request_ids_and_encoded_http_requests_are_bounded() {
        let endpoint = DirectHostEndpoint::parse("http://127.0.0.1:9/mobile").unwrap();
        let connection = DirectHostConnection::new(endpoint, "device-secret").unwrap();
        connection
            .next_request_id
            .store(u64::MAX, Ordering::Relaxed);
        assert_eq!(
            connection
                .prepare(HostCall::new(
                    "GET",
                    "/mobile/bootstrap",
                    RequestSemantics::ReadOnly,
                ))
                .unwrap_err(),
            HostConnectionError::RequestIdExhausted
        );

        connection.next_request_id.store(1, Ordering::Relaxed);
        let oversized_query = "é".repeat(120_000);
        let prepared = connection
            .prepare(
                HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly)
                    .with_query("q", oversized_query),
            )
            .unwrap();
        assert!(matches!(
            connection
                .request(prepared, Duration::from_secs(1))
                .unwrap_err(),
            HostConnectionError::RequestTooLarge { .. }
        ));
        assert!(connection.current_live_generation().is_none());
    }

    #[test]
    fn request_validation_never_reaches_the_network_or_pair_route() {
        let endpoint = DirectHostEndpoint::parse("http://127.0.0.1:9/mobile").unwrap();
        let connection = DirectHostConnection::new(endpoint, "device-secret").unwrap();
        assert!(connection
            .prepare(HostCall::new(
                "POST",
                "/mobile/pair",
                RequestSemantics::Effect
            ))
            .is_err());
        assert!(connection
            .prepare(HostCall::new("GET", "/outside", RequestSemantics::ReadOnly))
            .is_err());
        assert!(connection
            .prepare(HostCall::new(
                "GET",
                "/mobile/../bootstrap",
                RequestSemantics::ReadOnly
            ))
            .is_err());
        assert!(connection
            .prepare(HostCall::new(
                "GET",
                "/mobile//bootstrap",
                RequestSemantics::ReadOnly
            ))
            .is_err());
        assert!(connection
            .prepare(HostCall::new(
                "DELETE",
                "/mobile/session",
                RequestSemantics::Effect
            ))
            .is_err());
    }
}
