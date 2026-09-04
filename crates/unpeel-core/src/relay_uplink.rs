//! The transport half of the host relay uplink: one outbound WSS to the
//! relay Worker, speaking RFC 6455 as a client (masked frames out, plain
//! in). Protocol frames and crypto live in `relay_crypto`; the runtime that
//! owns sessions and dispatches tunneled requests lives in the frontend
//! (it needs the mobile server's state).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;

/// Where the relay lives. `UNPEEL_RELAY_URL` overrides for dev
/// (`ws://127.0.0.1:8787` against `wrangler dev`).
pub fn relay_url() -> String {
    std::env::var("UNPEEL_RELAY_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "wss://relay.unpeel.com".into())
}

/// Match the native app's refresh window: keep using a valid entitlement,
/// but replace it before there is any realistic chance of expiring while a
/// headless Host is unattended.
pub const ENTITLEMENT_REFRESH_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntitlementCacheState {
    Missing,
    HostMismatch,
    Fresh,
    RefreshDue,
    Expired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedEntitlement {
    pub entitlement: String,
    pub mac_id: String,
    pub expires_at: i64,
}

fn entitlement_cache_path() -> std::path::PathBuf {
    crate::app_paths::unpeel_home()
        .join("mobile")
        .join("relay-entitlement.json")
}

/// Return the durable Host identity shared with the native app. Both
/// implementations serialize first creation through `mac-id.lock`; Rust uses
/// an atomic private temp-file rename and never returns an unpersisted id.
pub fn ensure_host_id() -> Result<String, String> {
    ensure_host_id_at(
        &crate::app_paths::unpeel_home()
            .join("mobile")
            .join("mac-id"),
    )
}

fn ensure_host_id_at(path: &std::path::Path) -> Result<String, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let read_existing = || -> Result<Option<String>, String> {
        let existing = match std::fs::read_to_string(path) {
            Ok(value) => value.trim().to_string(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("could not read Host identity: {error}")),
        };
        if existing.is_empty() {
            return Ok(None);
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("could not secure Host identity: {error}"))?;
        Ok(Some(existing))
    };
    if let Some(existing) = read_existing()? {
        return Ok(existing);
    }

    let dir = path
        .parent()
        .ok_or("Host identity has no parent directory")?;
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create Host identity directory: {error}"))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"))
        .map_err(|error| format!("could not open Host identity lock: {error}"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "could not acquire Host identity lock: {}",
            std::io::Error::last_os_error()
        ));
    }

    let result = (|| {
        use std::io::Write;

        if let Some(existing) = read_existing()? {
            return Ok(existing);
        }
        let id = uuid::Uuid::new_v4().to_string().to_lowercase();
        let temporary = dir.join(format!(".mac-id.{}.tmp", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("could not create Host identity: {error}"))?;
        if let Err(error) = file.write_all(format!("{id}\n").as_bytes()) {
            drop(file);
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("could not write Host identity: {error}"));
        }
        drop(file);
        if let Err(error) = std::fs::rename(&temporary, path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("could not publish Host identity: {error}"));
        }
        Ok(id)
    })();
    let unlock = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    if unlock != 0 && result.is_ok() {
        return Err(format!(
            "could not release Host identity lock: {}",
            std::io::Error::last_os_error()
        ));
    }
    result
}

/// Read the cache without applying expiry policy. The license client needs
/// the bound Host id even when an old token has expired so it can refresh the
/// same identity rather than accidentally minting a different one.
pub fn cached_entitlement_record() -> Option<CachedEntitlement> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entitlement_cache_path()).ok()?).ok()?;
    Some(CachedEntitlement {
        entitlement: value.get("entitlement")?.as_str()?.to_string(),
        mac_id: value.get("macID")?.as_str()?.to_string(),
        expires_at: value.get("expiresAt")?.as_i64()?,
    })
}

fn cache_state_at(
    record: Option<&CachedEntitlement>,
    expected_mac_id: &str,
    now: i64,
) -> EntitlementCacheState {
    let Some(record) = record else {
        return EntitlementCacheState::Missing;
    };
    if record.mac_id != expected_mac_id {
        return EntitlementCacheState::HostMismatch;
    }
    if record.expires_at <= now {
        EntitlementCacheState::Expired
    } else if record.expires_at - now <= ENTITLEMENT_REFRESH_WINDOW_SECS {
        EntitlementCacheState::RefreshDue
    } else {
        EntitlementCacheState::Fresh
    }
}

pub fn entitlement_cache_state(expected_mac_id: &str) -> EntitlementCacheState {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let record = cached_entitlement_record();
    cache_state_at(record.as_ref(), expected_mac_id, now)
}

/// A still-valid cached entitlement. Both native- and TUI-issued cache files
/// share this contract; the relay service verifies the signature itself.
pub fn cached_entitlement(expected_mac_id: &str) -> Option<(String, String)> {
    let record = cached_entitlement_record()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    if record.expires_at <= now || record.mac_id != expected_mac_id {
        return None;
    }
    Some((record.entitlement, record.mac_id))
}

/// A valid cache bound to the durable Host identity pairing and the LAN
/// server use. Missing/malformed identity fails closed rather than trusting
/// the `macID` self-asserted by the cache file.
pub fn cached_entitlement_for_host() -> Option<(String, String)> {
    let mac_id = std::fs::read_to_string(
        crate::app_paths::unpeel_home()
            .join("mobile")
            .join("mac-id"),
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())?;
    cached_entitlement(&mac_id)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelayConnectError {
    Cancelled,
    AuthorizationRejected(String),
    Other(String),
}

impl RelayConnectError {
    pub fn is_authorization_rejected(&self) -> bool {
        matches!(self, Self::AuthorizationRejected(_))
    }
}

impl std::fmt::Display for RelayConnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("relay connection cancelled"),
            Self::AuthorizationRejected(message) | Self::Other(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl From<String> for RelayConnectError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl From<&str> for RelayConnectError {
    fn from(message: &str) -> Self {
        Self::Other(message.to_string())
    }
}

enum Transport {
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
    Plain(TcpStream),
}

impl Transport {
    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            Transport::Tls(stream) => stream.write_all(data),
            Transport::Plain(stream) => stream.write_all(data),
        }
    }
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Tls(stream) => stream.read(buffer),
            Transport::Plain(stream) => stream.read(buffer),
        }
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Transport::Tls(stream) => stream.get_ref().set_read_timeout(timeout),
            Transport::Plain(stream) => stream.set_read_timeout(timeout),
        }
    }
}

/// Result of a bounded receive attempt. An idle socket remains healthy and
/// may be polled again; a closed or malformed socket is still returned as an
/// error.
#[derive(Debug, Eq, PartialEq)]
pub enum ReceiveOutcome {
    Message(Vec<u8>),
    /// A ping or pong proved the WebSocket is still responsive, but did not
    /// carry a relay protocol message.
    Control,
    Idle,
}

/// A connected host uplink socket. Reads yield relay protocol frames;
/// writes are WS-masked as RFC 6455 requires of clients.
pub struct RelaySocket {
    transport: Transport,
    buffer: Vec<u8>,
}

impl RelaySocket {
    /// Plain-TCP construction for test harnesses only (the downlink
    /// handshake proofs in this repo drive a fake relay end over a socket
    /// pair). Never a production entry point: real connects negotiate the
    /// WS upgrade and TLS through `connect_*`.
    #[doc(hidden)]
    pub fn plain_for_tests(stream: TcpStream) -> Self {
        Self {
            transport: Transport::Plain(stream),
            buffer: Vec::new(),
        }
    }
}

/// Dial, TLS-handshake (unless ws://), and upgrade to WebSocket with the
/// entitlement as the bearer. Blocking; callers own reconnect policy.
pub fn connect(mac_id: &str, entitlement: &str) -> Result<RelaySocket, RelayConnectError> {
    connect_cancellable(mac_id, entitlement, || false)
}

/// Cancellable form used by Host owners. Cancellation is checked around
/// every blocking transport phase and again before the bearer request is
/// written, so a deactivation/handoff that wins a stalled connect cannot go
/// on to announce devices or return a usable socket.
pub fn connect_cancellable(
    mac_id: &str,
    entitlement: &str,
    cancelled: impl Fn() -> bool,
) -> Result<RelaySocket, RelayConnectError> {
    connect_cancellable_to_url(&relay_url(), mac_id, entitlement, cancelled)
}

fn connect_cancellable_to_url(
    url: &str,
    mac_id: &str,
    entitlement: &str,
    cancelled: impl Fn() -> bool,
) -> Result<RelaySocket, RelayConnectError> {
    connect_upgrade(
        url,
        &format!("/v1/host/{mac_id}"),
        &format!("Authorization: Bearer {entitlement}\r\n"),
        cancelled,
    )
}

/// Controller-role connect: the same socket/framing as the host uplink, on
/// the relay's `/v1/client/` route with the paired relay token in the
/// subprotocol header (never the URL, so it stays out of access logs).
/// Consumers: the Rust Controller downlink (`relay_downlink`).
pub fn connect_client_cancellable(
    url: &str,
    mac_id: &str,
    relay_token: &str,
    cancelled: impl Fn() -> bool,
) -> Result<RelaySocket, RelayConnectError> {
    connect_upgrade(
        url,
        &format!("/v1/client/{mac_id}"),
        &format!("Sec-WebSocket-Protocol: unpeel-relay-token.{relay_token}\r\n"),
        cancelled,
    )
}

fn connect_upgrade(
    url: &str,
    request_path: &str,
    extra_headers: &str,
    cancelled: impl Fn() -> bool,
) -> Result<RelaySocket, RelayConnectError> {
    let check_cancelled = || {
        if cancelled() {
            Err(RelayConnectError::Cancelled)
        } else {
            Ok(())
        }
    };
    check_cancelled()?;
    let (secure, rest) = if let Some(rest) = url.strip_prefix("wss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        (false, rest)
    } else {
        return Err(format!("unsupported relay url: {url}").into());
    };
    let host_port = rest.trim_end_matches('/');
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>().map_err(|e| e.to_string())?,
        ),
        None => (host_port.to_string(), if secure { 443 } else { 80 }),
    };

    let tcp = TcpStream::connect((host.as_str(), port)).map_err(|e| format!("connect: {e}"))?;
    check_cancelled()?;
    // Nagle + delayed ACK stalls small sealed frames ~40-50ms whenever a
    // write follows unacked data (measured via __relay_probe__, 2026-08-29);
    // terminal frames are exactly that shape.
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(Duration::from_secs(40))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(15))).ok();

    let mut transport = if secure {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| format!("server name: {e}"))?;
        let mut connection = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("tls: {e}"))?;
        let mut tcp = tcp;
        while connection.is_handshaking() {
            connection
                .complete_io(&mut tcp)
                .map_err(|e| format!("tls handshake: {e}"))?;
        }
        // The bearer request is application data, so check again after the
        // potentially blocking TLS exchange and before constructing/writing
        // the HTTP upgrade that contains it.
        check_cancelled()?;
        Transport::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        Transport::Plain(tcp)
    };

    // WebSocket client handshake.
    let key_bytes = crate::relay_crypto::random_bytes(16);
    let key = base64::engine::general_purpose::STANDARD.encode(&key_bytes);
    let request = format!(
        "GET {request_path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\
         {extra_headers}\r\n"
    );
    check_cancelled()?;
    transport
        .write_all(request.as_bytes())
        .map_err(|e| format!("handshake write: {e}"))?;
    check_cancelled()?;

    let mut head = Vec::new();
    let mut chunk = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err("oversized upgrade response".into());
        }
        let n = transport
            .read(&mut chunk)
            .map_err(|e| format!("handshake read: {e}"))?;
        check_cancelled()?;
        if n == 0 {
            return Err("relay closed during handshake".into());
        }
        head.extend_from_slice(&chunk[..n]);
    }
    let split = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let header_text = String::from_utf8_lossy(&head[..split]).to_string();
    if !header_text.starts_with("HTTP/1.1 101") {
        let status = header_text.lines().next().unwrap_or("").to_string();
        let message = format!("relay refused upgrade: {status}");
        let status_code = status
            .split_ascii_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok());
        return Err(if matches!(status_code, Some(401 | 403)) {
            RelayConnectError::AuthorizationRejected(message)
        } else {
            RelayConnectError::Other(message)
        });
    }
    let expected = crate::remote_server::websocket_accept_key(&key);
    if !header_text.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("sec-websocket-accept:")
            && line.split(':').nth(1).map(str::trim) == Some(expected.as_str())
    }) {
        return Err("bad Sec-WebSocket-Accept".into());
    }
    check_cancelled()?;

    Ok(RelaySocket {
        transport,
        buffer: head[split..].to_vec(),
    })
}

impl RelaySocket {
    /// Send one binary WS frame, client-masked.
    pub fn send(&mut self, payload: &[u8]) -> Result<(), String> {
        self.send_opcode(0x2, payload)
    }

    pub fn send_ping(&mut self) -> Result<(), String> {
        self.send_opcode(0x9, b"")
    }

    fn send_opcode(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut frame = vec![0x80 | opcode];
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len < 65_536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask: [u8; 4] = crate::relay_crypto::random_bytes(4).try_into().unwrap();
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.transport
            .write_all(&frame)
            .map_err(|e| format!("ws send: {e}"))
    }

    /// Next binary message. Answers pings, skips pongs, errors on close.
    pub fn receive(&mut self) -> Result<Vec<u8>, String> {
        loop {
            match self.receive_timeout(Duration::from_secs(40))? {
                ReceiveOutcome::Message(payload) => return Ok(payload),
                ReceiveOutcome::Control | ReceiveOutcome::Idle => {}
            }
        }
    }

    /// Wait at most one socket read timeout for the next binary message.
    ///
    /// A timeout is an idle poll, not a broken connection. Any partial TLS
    /// record remains buffered by rustls and any partial WebSocket frame
    /// remains in `self.buffer`, so the next call resumes the same frame.
    pub fn receive_timeout(&mut self, timeout: Duration) -> Result<ReceiveOutcome, String> {
        self.receive_timeout_cancellable(timeout, || false)
    }

    /// Bounded/cancellable receive for an authority-owning Host. A peer that
    /// drip-feeds a partial frame cannot reset the deadline, and authority is
    /// rechecked at least every 100ms while a socket read is blocked.
    pub fn receive_timeout_cancellable(
        &mut self,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<ReceiveOutcome, String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let Some((opcode, payload)) = self.read_frame_until(deadline, &cancelled)? else {
                if cancelled() {
                    return Err("relay receive cancelled".into());
                }
                return Ok(ReceiveOutcome::Idle);
            };
            match opcode {
                0x2 | 0x1 => return Ok(ReceiveOutcome::Message(payload)),
                0x9 => {
                    self.send_opcode(0xA, &payload)?; // ping → pong
                    return Ok(ReceiveOutcome::Control);
                }
                0xA => return Ok(ReceiveOutcome::Control),
                0x8 => return Err("relay closed the socket".into()),
                _ => {}
            }
        }
    }

    fn fill_until(
        &mut self,
        needed: usize,
        deadline: std::time::Instant,
        cancelled: &impl Fn() -> bool,
    ) -> Result<bool, String> {
        let mut chunk = [0u8; 16 * 1024];
        while self.buffer.len() < needed {
            if cancelled() {
                return Ok(false);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(now);
            self.transport
                .set_read_timeout(Some(remaining.min(Duration::from_millis(100))))
                .map_err(|e| format!("set relay read timeout: {e}"))?;
            let n = match self.transport.read(&mut chunk) {
                Ok(n) => n,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(format!("ws read: {error}")),
            };
            if n == 0 {
                return Err("relay socket ended".into());
            }
            self.buffer.extend_from_slice(&chunk[..n]);
        }
        Ok(true)
    }

    fn read_frame_until(
        &mut self,
        deadline: std::time::Instant,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Option<(u8, Vec<u8>)>, String> {
        if !self.fill_until(2, deadline, cancelled)? {
            return Ok(None);
        }
        let opcode = self.buffer[0] & 0x0f;
        let masked = self.buffer[1] & 0x80 != 0;
        let mut len = (self.buffer[1] & 0x7f) as usize;
        let mut offset = 2;
        if len == 126 {
            if !self.fill_until(4, deadline, cancelled)? {
                return Ok(None);
            }
            len = u16::from_be_bytes([self.buffer[2], self.buffer[3]]) as usize;
            offset = 4;
        } else if len == 127 {
            if !self.fill_until(10, deadline, cancelled)? {
                return Ok(None);
            }
            len = u64::from_be_bytes(self.buffer[2..10].try_into().unwrap()) as usize;
            offset = 10;
        }
        if len > crate::relay_wire::MAX_FRAME_BYTES + 1024 {
            return Err("oversized ws frame".into());
        }
        let mask_len = if masked { 4 } else { 0 };
        if !self.fill_until(offset + mask_len + len, deadline, cancelled)? {
            return Ok(None);
        }
        let mut payload = self.buffer[offset + mask_len..offset + mask_len + len].to_vec();
        if masked {
            let mask: [u8; 4] = self.buffer[offset..offset + 4].try_into().unwrap();
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        self.buffer.drain(..offset + mask_len + len);
        Ok(Some((opcode, payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn socket_pair() -> (RelaySocket, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test relay");
        let client =
            TcpStream::connect(listener.local_addr().unwrap()).expect("connect test relay");
        let (server, _) = listener.accept().expect("accept test relay");
        (
            RelaySocket {
                transport: Transport::Plain(client),
                buffer: Vec::new(),
            },
            server,
        )
    }

    fn server_frame(payload: &[u8]) -> Vec<u8> {
        server_frame_with_opcode(0x2, payload)
    }

    fn server_frame_with_opcode(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x80 | opcode];
        if payload.len() < 126 {
            frame.push(payload.len() as u8);
        } else {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn entitlement_cache_is_host_bound_and_refreshes_before_expiry() {
        let now = 1_000_000;
        let mut record = CachedEntitlement {
            entitlement: "signed-token".into(),
            mac_id: "host-a".into(),
            expires_at: now,
        };
        assert_eq!(
            cache_state_at(None, "host-a", now),
            EntitlementCacheState::Missing
        );
        assert_eq!(
            cache_state_at(Some(&record), "host-b", now),
            EntitlementCacheState::HostMismatch
        );
        assert_eq!(
            cache_state_at(Some(&record), "host-a", now),
            EntitlementCacheState::Expired
        );
        record.expires_at = now + ENTITLEMENT_REFRESH_WINDOW_SECS;
        assert_eq!(
            cache_state_at(Some(&record), "host-a", now),
            EntitlementCacheState::RefreshDue
        );
        record.expires_at += 1;
        assert_eq!(
            cache_state_at(Some(&record), "host-a", now),
            EntitlementCacheState::Fresh
        );
    }

    #[test]
    fn concurrent_host_identity_creation_publishes_one_private_id() {
        use std::collections::HashSet;
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("unpeel-host-id-{}", uuid::Uuid::new_v4()));
        let path = root.join("mobile").join("mac-id");
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                ensure_host_id_at(&path).expect("create stable Host identity")
            }));
        }
        let ids: HashSet<String> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            ids.iter().next().unwrap()
        );
        let temporary_count = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .count();
        assert_eq!(temporary_count, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_after_tcp_connect_sends_no_bearer_request() {
        use std::cell::Cell;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test relay");
        let address = listener.local_addr().unwrap();
        let checks = Cell::new(0usize);
        let result = connect_cancellable_to_url(
            &format!("ws://{address}"),
            "host-a",
            "must-not-leave-process",
            || {
                let next = checks.get() + 1;
                checks.set(next);
                next >= 2
            },
        );
        assert!(matches!(result, Err(RelayConnectError::Cancelled)));

        let (mut peer, _) = listener.accept().expect("accept cancelled connect");
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut bytes = [0u8; 64];
        assert_eq!(peer.read(&mut bytes).unwrap_or(0), 0);
    }

    #[test]
    fn timed_receive_distinguishes_idle_from_a_later_message() {
        let (mut socket, mut peer) = socket_pair();
        assert_eq!(
            socket.receive_timeout(Duration::from_millis(20)).unwrap(),
            ReceiveOutcome::Idle
        );

        peer.write_all(&server_frame(b"after idle")).unwrap();
        assert_eq!(
            socket.receive_timeout(Duration::from_millis(100)).unwrap(),
            ReceiveOutcome::Message(b"after idle".to_vec())
        );
    }

    #[test]
    fn partial_frame_receive_observes_cancellation_before_deadline() {
        let (mut socket, mut peer) = socket_pair();
        peer.write_all(&[0x82]).unwrap();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            trigger.store(true, std::sync::atomic::Ordering::Release);
        });
        let started = std::time::Instant::now();
        let result = socket.receive_timeout_cancellable(Duration::from_secs(5), || {
            cancelled.load(std::sync::atomic::Ordering::Acquire)
        });
        worker.join().unwrap();
        assert_eq!(result.unwrap_err(), "relay receive cancelled");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn partial_extended_frame_survives_multiple_idle_polls() {
        let (mut socket, mut peer) = socket_pair();
        let payload = vec![0x5a; 130];
        let frame = server_frame(&payload);

        peer.write_all(&frame[..3]).unwrap();
        assert_eq!(
            socket.receive_timeout(Duration::from_millis(20)).unwrap(),
            ReceiveOutcome::Idle
        );

        peer.write_all(&frame[3..17]).unwrap();
        assert_eq!(
            socket.receive_timeout(Duration::from_millis(20)).unwrap(),
            ReceiveOutcome::Idle
        );

        peer.write_all(&frame[17..]).unwrap();
        assert_eq!(
            socket.receive_timeout(Duration::from_millis(100)).unwrap(),
            ReceiveOutcome::Message(payload)
        );
    }

    #[test]
    fn timed_receive_keeps_peer_close_fatal() {
        let (mut socket, peer) = socket_pair();
        drop(peer);
        let error = socket
            .receive_timeout(Duration::from_millis(100))
            .unwrap_err();
        assert!(error.contains("relay socket ended"), "{error}");
    }

    #[test]
    fn pong_is_reported_as_control_activity() {
        let (mut socket, mut peer) = socket_pair();
        peer.write_all(&server_frame_with_opcode(0xA, b"alive"))
            .unwrap();
        assert_eq!(
            socket.receive_timeout(Duration::from_millis(100)).unwrap(),
            ReceiveOutcome::Control
        );
    }
}
