//! Remote control server: an HTTPS + WSS layer over the hosted-session
//! artifacts so a phone/tablet/another machine can list sessions, stream
//! terminal output, send input, and kill sessions.
//!
//! Runs as `unpeel-host __remote__` — like the Sessions MCP it talks directly
//! to `~/.unpeel/app-sessions/<id>/{manifest.json,output.bin,session.sock}`
//! and does not need the app window to be open.
//!
//! Security model (see the private "remote-control-server" design record):
//! - TLS always on: self-signed cert generated into `~/.unpeel/remote/tls/`,
//!   its SHA-256 fingerprint is surfaced so clients can pin it.
//! - Bearer token (regenerated per server start, constant-time compared)
//!   required on every `/api/*` request; WebSocket upgrades pass it as a
//!   `?token=` query parameter.
//! - Persistent paired-device tokens are accepted as an alternative
//!   credential: the native app's pairing flow (`MobilePairingStore` in
//!   `MobileRemoteServer.swift`) stores SHA-256 token hashes in
//!   `~/.unpeel/mobile/devices.json`, and this server verifies against that
//!   same file on every request — so devices paired in the app work here,
//!   and revoking a device in the app cuts it off live.
//! - Per-IP rate limiting on failed auth, with a hard block after repeated
//!   consecutive failures.
//! - Connected clients tracked (IP, timestamps, permissions) and expired
//!   after 30 minutes idle.
//! - Everything auditable: `~/.unpeel/remote/audit.log` (JSON lines).
//!
//! The transport is intentionally synchronous (thread per connection,
//! `Connection: close`) to match the rest of this crate — a remote controller
//! serves a handful of clients, not thousands.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rustls::pki_types::pem::{Error as PemError, PemObject};
use serde_json::{json, Value};

use crate::app_paths;
use crate::controller_api;
use crate::mcp_host;
use crate::session_host::{self, HostedSessionManifest, HostedSessionState, SessionHostCommand};
use crate::session_input;
use crate::state::current_timestamp_ms;

pub const REMOTE_ARG: &str = "__remote__";

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_WS_CLIENT_FRAME_BYTES: u64 = 1024 * 1024;
const REQUEST_READ_TIMEOUT_MS: u64 = 15_000;
/// How long each pump iteration waits on the session's output socket before
/// checking the WebSocket client for close/ping/input frames again. Client
/// input read off the WebSocket waits at most this long before it is written
/// to the PTY, so keep it small enough that remote typing feels immediate:
/// on an idle terminal (exactly the typing case — nothing else is printing)
/// every keystroke waits out this full window on average half the time, and
/// at 50ms that alternation alone put ~25ms of the typing echo round trip on
/// the host. The idle loop costs one timed read per window per viewer —
/// still negligible at 10ms.
const OUTPUT_PUMP_UPSTREAM_TIMEOUT_MS: u64 = 10;
const OUTPUT_PUMP_CLIENT_POLL_MS: u64 = 2;
/// Maximum history replayed when a WS client resumes from an explicit
/// `?offset=`. A client further behind than this — or holding an offset
/// beyond the current file — is rebased to a fresh aligned tail instead;
/// the hello frame reports `rebased: true` plus the true `start_offset`.
const WS_RESUME_REPLAY_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Output WebSocket wire protocol version, reported in the hello frame.
const WS_STREAM_PROTOCOL_VERSION: u32 = 1;
/// Server ping cadence on the output WebSocket, so half-dead clients turn
/// into write errors instead of leaked viewer registrations.
const WS_PING_INTERVAL_MS: u64 = 20_000;
/// Close the stream when the client has sent nothing (not even pongs) for
/// this long. Browsers and URLSessionWebSocketTask auto-reply to pings, so a
/// live client always stays far under this.
const WS_CLIENT_LIVENESS_TIMEOUT_MS: u64 = 75_000;
/// Max raw input bytes per WS `input` message (same cap as `POST /write`).
const WS_MAX_INPUT_BYTES: usize = 64 * 1024;
/// How many queued client frames are drained per pump iteration before the
/// upstream socket is checked again.
const WS_MAX_CLIENT_FRAMES_PER_TICK: usize = 32;
/// History replayed from `output.bin` before going live when the client does
/// not pass an offset. Replay must come from disk — the host's in-memory
/// broadcaster only serves recent output, and subscribing far behind it makes
/// the host push a burst large enough to kill the socket (the attach client
/// replays from disk for the same reason).
const OUTPUT_REPLAY_TAIL_BYTES: usize = 256 * 1024;
/// Catch-up read size per WebSocket frame when the client resumes from an
/// explicit offset.
const OUTPUT_REPLAY_CHUNK_BYTES: usize = 128 * 1024;
const OUTPUT_POLL_DEFAULT_BYTES: usize = 256 * 1024;
const OUTPUT_POLL_MAX_BYTES: usize = 1024 * 1024;
const OUTPUT_POLL_MAX_WAIT_MS: u64 = 25_000;
/// Re-check cadence while a long-poll waits for new output — this bounds the
/// typing-echo latency of the HTTP fallback path, so keep it tight.
const OUTPUT_POLL_SLEEP_MS: u64 = 10;

const AUTH_FAIL_WINDOW_MS: u64 = 60_000;
const AUTH_MAX_FAILS_PER_WINDOW: usize = 5;
const AUTH_CONSECUTIVE_BLOCK_THRESHOLD: u32 = 10;
const AUTH_BLOCK_DURATION_MS: u64 = 15 * 60_000;

const CLIENT_IDLE_EXPIRY_MS: u64 = 30 * 60_000;
/// Poll viewers count as "watching" only while they keep polling.
const POLL_VIEWER_TTL_MS: u64 = 15_000;

const AUDIT_MAX_BYTES: u64 = 50 * 1024 * 1024;
const AUDIT_ROTATED_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

const WEBSOCKET_ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

pub fn remote_dir() -> PathBuf {
    app_paths::unpeel_home().join("remote")
}

pub fn tls_dir() -> PathBuf {
    remote_dir().join("tls")
}

pub fn audit_log_path() -> PathBuf {
    remote_dir().join("audit.log")
}

/// Live viewer presence, written whenever viewers change; the native app
/// watches this to render viewer avatars on the session title bar.
pub fn presence_path() -> PathBuf {
    remote_dir().join("presence.json")
}

/// `~/.unpeel/remote.json` — the connection info the app/QR flow reads.
pub fn remote_state_path() -> PathBuf {
    app_paths::unpeel_home().join("remote.json")
}

/// The native app's paired-device store (`MobilePairingStore`); shared so a
/// device paired through the app UI is a valid credential here too.
pub fn paired_devices_path() -> PathBuf {
    app_paths::unpeel_home().join("mobile").join("devices.json")
}

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

/// 256 bits of randomness, hex encoded (two v4 UUIDs, same construction as
/// `mcp_auth::ensure_auth_token`). Regenerated on every server start.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Constant-time comparison so token checks don't leak match length via
/// timing. Length mismatch returns early — the token length is public.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// TLS material
// ---------------------------------------------------------------------------

pub struct TlsMaterial {
    pub certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    pub key: rustls::pki_types::PrivateKeyDer<'static>,
    /// Lowercase hex SHA-256 of the leaf certificate DER; shown in the QR
    /// payload so clients can pin the self-signed cert.
    pub fingerprint: String,
}

/// Load the persisted self-signed certificate, generating one on first use.
///
/// This is the one Host certificate every pinned transport serves — the
/// `__remote__` WSS streamer and the direct `/mobile` listener — so a
/// Controller keeps a single fingerprint pin for both.
pub fn ensure_tls_material() -> Result<TlsMaterial, String> {
    ensure_tls_material_in(&tls_dir())
}

/// [`ensure_tls_material`] against an explicit directory (tests and tools
/// that address a specific `UNPEEL_HOME`).
pub fn ensure_tls_material_in(dir: &std::path::Path) -> Result<TlsMaterial, String> {
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    if cert_path.exists() && key_path.exists() {
        return load_tls_material(&cert_path, &key_path);
    }

    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create TLS dir: {e}"))?;
    let mut names = vec!["localhost".to_string()];
    if let Some(ip) = local_lan_ip() {
        names.push(ip);
    }
    let certified = rcgen::generate_simple_self_signed(names)
        .map_err(|e| format!("Failed to generate TLS certificate: {e}"))?;
    write_private(&cert_path, certified.cert.pem().as_bytes())?;
    write_private(&key_path, certified.key_pair.serialize_pem().as_bytes())?;
    load_tls_material(&cert_path, &key_path)
}

fn load_tls_material(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<TlsMaterial, String> {
    let cert_pem =
        std::fs::read(cert_path).map_err(|e| format!("Failed to read TLS certificate: {e}"))?;
    let certs = rustls::pki_types::CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse TLS certificate: {e}"))?;
    if certs.is_empty() {
        return Err("TLS certificate file contains no certificates".into());
    }
    let key_pem = std::fs::read(key_path).map_err(|e| format!("Failed to read TLS key: {e}"))?;
    let key = match rustls::pki_types::PrivateKeyDer::from_pem_slice(&key_pem) {
        Ok(key) => key,
        Err(PemError::NoItemsFound) => return Err("TLS key file contains no private key".into()),
        Err(error) => return Err(format!("Failed to parse TLS key: {error}")),
    };
    let fingerprint = cert_fingerprint(certs[0].as_ref());
    Ok(TlsMaterial {
        certs,
        key,
        fingerprint,
    })
}

fn cert_fingerprint(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(der);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_private(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    std::fs::write(path, data).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// A rustls server config that serves the Host certificate.
pub fn build_tls_config(material: TlsMaterial) -> Result<Arc<rustls::ServerConfig>, String> {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(material.certs, material.key)
        .map_err(|e| format!("Failed to build TLS config: {e}"))?;
    Ok(Arc::new(config))
}

/// Best-effort LAN IP: route lookup via an unconnected UDP socket (no packets
/// are sent). Falls back to loopback.
fn local_lan_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

pub(crate) struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl AuditLog {
    pub(crate) fn new(path: PathBuf) -> Self {
        // Prune a rotated log that has aged out (keep last 7 days).
        let rotated = rotated_path(&path);
        if let Ok(meta) = std::fs::metadata(&rotated) {
            let age_ms = meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|e| e.as_millis() as u64)
                .unwrap_or(0);
            if age_ms > AUDIT_ROTATED_MAX_AGE_MS {
                let _ = std::fs::remove_file(&rotated);
            }
        }
        AuditLog {
            path,
            lock: Mutex::new(()),
        }
    }

    pub(crate) fn log(&self, event: &str, mut fields: Value) {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Size-based rotation: one rotated generation, replaced in place.
        if let Ok(meta) = std::fs::metadata(&self.path) {
            if meta.len() > AUDIT_MAX_BYTES {
                let _ = std::fs::rename(&self.path, rotated_path(&self.path));
            }
        }
        if let Some(object) = fields.as_object_mut() {
            object.insert("ts".into(), json!(current_timestamp_ms()));
            object.insert("event".into(), json!(event));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{fields}");
        }
    }
}

fn rotated_path(path: &std::path::Path) -> PathBuf {
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    PathBuf::from(rotated)
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RateDecision {
    Allowed,
    /// Too many failures inside the rolling window — try again later.
    Limited,
    /// IP is hard-blocked after sustained consecutive failures.
    Blocked,
}

#[derive(Default)]
struct FailState {
    recent_ms: Vec<u64>,
    consecutive: u32,
    blocked_until_ms: u64,
}

#[derive(Default)]
pub(crate) struct RateLimiter {
    by_ip: HashMap<IpAddr, FailState>,
}

impl RateLimiter {
    pub(crate) fn check(&mut self, ip: IpAddr, now_ms: u64) -> RateDecision {
        let Some(state) = self.by_ip.get_mut(&ip) else {
            return RateDecision::Allowed;
        };
        if state.blocked_until_ms > now_ms {
            return RateDecision::Blocked;
        }
        state
            .recent_ms
            .retain(|at| now_ms.saturating_sub(*at) < AUTH_FAIL_WINDOW_MS);
        if state.recent_ms.len() >= AUTH_MAX_FAILS_PER_WINDOW {
            return RateDecision::Limited;
        }
        RateDecision::Allowed
    }

    pub(crate) fn record_failure(&mut self, ip: IpAddr, now_ms: u64) {
        let state = self.by_ip.entry(ip).or_default();
        state.recent_ms.push(now_ms);
        state.consecutive += 1;
        if state.consecutive >= AUTH_CONSECUTIVE_BLOCK_THRESHOLD {
            state.blocked_until_ms = now_ms + AUTH_BLOCK_DURATION_MS;
        }
    }

    pub(crate) fn record_success(&mut self, ip: IpAddr) {
        if let Some(state) = self.by_ip.get_mut(&ip) {
            state.consecutive = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Client tracking
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ClientRecord {
    id: String,
    ip: String,
    kind: &'static str,
    /// Paired-device label when the client authenticated with a device token.
    device: Option<String>,
    /// Session this client is viewing (output stream / poll viewers only).
    session_id: Option<String>,
    connected_at: u64,
    last_activity: u64,
    permissions: &'static str,
}

#[derive(Default)]
pub(crate) struct ClientRegistry {
    clients: HashMap<String, ClientRecord>,
    ws_counter: u64,
}

impl ClientRegistry {
    /// Track a REST caller (keyed per IP). Returns true when this is a new
    /// client (first request, or first after idle expiry).
    fn touch_http(&mut self, ip: &IpAddr, device: Option<&str>, now_ms: u64) -> bool {
        self.prune(now_ms);
        let id = format!("http:{ip}");
        let is_new = !self.clients.contains_key(&id);
        let record = self.clients.entry(id.clone()).or_insert(ClientRecord {
            id,
            ip: ip.to_string(),
            kind: "http",
            device: None,
            session_id: None,
            connected_at: now_ms,
            last_activity: now_ms,
            // All authenticated clients are read-write in phase 1; the field
            // exists so read-only clients (phase 6) don't change the shape.
            permissions: "read-write",
        });
        record.last_activity = now_ms;
        if let Some(device) = device {
            record.device = Some(device.to_string());
        }
        is_new
    }

    fn register_ws(
        &mut self,
        ip: &IpAddr,
        device: Option<&str>,
        session_id: &str,
        now_ms: u64,
    ) -> String {
        self.prune(now_ms);
        self.ws_counter += 1;
        let id = format!("ws:{}:{ip}", self.ws_counter);
        self.clients.insert(
            id.clone(),
            ClientRecord {
                id: id.clone(),
                ip: ip.to_string(),
                kind: "ws",
                device: device.map(str::to_string),
                session_id: Some(session_id.to_string()),
                connected_at: now_ms,
                last_activity: now_ms,
                permissions: "read-write",
            },
        );
        id
    }

    /// Poll-based viewers (plain-GET output reads) are tracked with a short
    /// TTL so presence stays accurate without a persistent connection.
    fn touch_poll_viewer(
        &mut self,
        ip: &IpAddr,
        device: Option<&str>,
        session_id: &str,
        now_ms: u64,
    ) {
        self.prune(now_ms);
        let id = format!("poll:{session_id}:{ip}");
        let record = self.clients.entry(id.clone()).or_insert(ClientRecord {
            id,
            ip: ip.to_string(),
            kind: "poll",
            device: None,
            session_id: Some(session_id.to_string()),
            connected_at: now_ms,
            last_activity: now_ms,
            permissions: "read-write",
        });
        record.last_activity = now_ms;
        if let Some(device) = device {
            record.device = Some(device.to_string());
        }
    }

    /// Everyone currently viewing the given session's output.
    fn viewers_for(&mut self, session_id: &str, now_ms: u64) -> Vec<Value> {
        self.prune(now_ms);
        let mut list: Vec<&ClientRecord> = self
            .clients
            .values()
            .filter(|c| c.session_id.as_deref() == Some(session_id))
            .collect();
        list.sort_by_key(|c| c.connected_at);
        list.iter()
            .map(|c| {
                json!({
                    "ip": c.ip,
                    "kind": c.kind,
                    "device": c.device,
                    "connected_at": c.connected_at,
                    "last_seen": c.last_activity,
                })
            })
            .collect()
    }

    /// Presence grouped by session, for the on-disk contract the native app
    /// watches (`~/.unpeel/remote/presence.json`).
    fn presence(&mut self, now_ms: u64) -> Value {
        self.prune(now_ms);
        let mut sessions: HashMap<String, Vec<Value>> = HashMap::new();
        for record in self.clients.values() {
            let Some(session_id) = &record.session_id else {
                continue;
            };
            sessions.entry(session_id.clone()).or_default().push(json!({
                "ip": record.ip,
                "kind": record.kind,
                "device": record.device,
                "last_seen": record.last_activity,
            }));
        }
        json!({ "version": 1, "updated_at": now_ms, "sessions": sessions })
    }

    fn touch(&mut self, id: &str, now_ms: u64) {
        if let Some(record) = self.clients.get_mut(id) {
            record.last_activity = now_ms;
        }
    }

    fn remove(&mut self, id: &str) {
        self.clients.remove(id);
    }

    fn prune(&mut self, now_ms: u64) {
        self.clients.retain(|_, c| {
            let ttl = if c.kind == "poll" {
                POLL_VIEWER_TTL_MS
            } else {
                CLIENT_IDLE_EXPIRY_MS
            };
            now_ms.saturating_sub(c.last_activity) < ttl
        });
    }

    fn snapshot(&mut self, now_ms: u64) -> Vec<Value> {
        self.prune(now_ms);
        let mut list: Vec<&ClientRecord> = self.clients.values().collect();
        list.sort_by_key(|c| c.connected_at);
        list.iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "ip": c.ip,
                    "kind": c.kind,
                    "device": c.device,
                    "connected_at": c.connected_at,
                    "last_activity": c.last_activity,
                    "permissions": c.permissions,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// HTTP plumbing
// ---------------------------------------------------------------------------

/// The minimal stream contract the request handler needs; lets tests drive
/// plain `TcpStream`s while production wraps everything in rustls.
pub(crate) trait ConnStream: Read + Write {
    fn set_read_timeout_ms(&self, ms: Option<u64>) -> std::io::Result<()>;
}

impl ConnStream for TcpStream {
    fn set_read_timeout_ms(&self, ms: Option<u64>) -> std::io::Result<()> {
        self.set_read_timeout(ms.map(Duration::from_millis))
    }
}

impl ConnStream for rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    fn set_read_timeout_ms(&self, ms: Option<u64>) -> std::io::Result<()> {
        self.sock.set_read_timeout(ms.map(Duration::from_millis))
    }
}

/// A stream with bytes that were over-read while parsing the request headers
/// stitched back in front (matters for pipelined bodies and WebSocket frames).
pub(crate) struct PrefixedStream<S: ConnStream> {
    prefix: Vec<u8>,
    pos: usize,
    inner: S,
}

impl<S: ConnStream> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        PrefixedStream {
            prefix,
            pos: 0,
            inner,
        }
    }
}

impl<S: ConnStream> Read for PrefixedStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.prefix.len() {
            let n = (self.prefix.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.prefix[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

impl<S: ConnStream> Write for PrefixedStream<S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<S: ConnStream> ConnStream for PrefixedStream<S> {
    fn set_read_timeout_ms(&self, ms: Option<u64>) -> std::io::Result<()> {
        self.inner.set_read_timeout_ms(ms)
    }
}

#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: HashMap<String, String>,
    /// Header names lowercased.
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Vec<u8>,
}

/// Read one HTTP/1.1 request. Returns the request plus any bytes read past
/// the end of the body (prefix for the connection's next protocol layer).
pub(crate) fn read_request<S: Read>(stream: &mut S) -> Result<(Request, Vec<u8>), String> {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let header_end;
    loop {
        if buffer.len() > MAX_HEADER_BYTES {
            return Err("Request headers too large".into());
        }
        let mut chunk = [0u8; 1024];
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("Failed to read request: {e}"))?;
        if n == 0 {
            return Err("Connection closed before request was complete".into());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buffer) {
            header_end = pos;
            break;
        }
    }

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_uppercase();
    let target = parts.next().unwrap_or_default().to_string();
    if method.is_empty() || target.is_empty() {
        return Err("Malformed request line".into());
    }

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_string());
        }
    }

    let (path, query) = split_target(&target);

    let mut rest = buffer[header_end + 4..].to_vec();
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err("Request body too large".into());
    }
    while rest.len() < content_length {
        let mut chunk = [0u8; 4096];
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("Failed to read request body: {e}"))?;
        if n == 0 {
            return Err("Connection closed before body was complete".into());
        }
        rest.extend_from_slice(&chunk[..n]);
    }
    let body = rest[..content_length].to_vec();
    let leftover = rest[content_length..].to_vec();

    Ok((
        Request {
            method,
            path,
            query,
            headers,
            body,
        },
        leftover,
    ))
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let (path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let mut query = HashMap::new();
    for pair in raw_query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(url_decode(key), url_decode(value));
    }
    (path.to_string(), query)
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3);
                match hex.and_then(|h| u8::from_str_radix(&String::from_utf8_lossy(h), 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn write_response<W: Write>(
    stream: &mut W,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn write_json<W: Write>(stream: &mut W, status: u16, reason: &str, value: &Value) {
    write_response(
        stream,
        status,
        reason,
        "application/json",
        value.to_string().as_bytes(),
    );
}

fn write_error<W: Write>(stream: &mut W, status: u16, reason: &str, message: &str) {
    write_json(stream, status, reason, &json!({ "error": message }));
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub struct RemoteServerConfig {
    /// Bind address; the plan default is all interfaces so a phone on the LAN
    /// can reach it. Restrict via `--bind` for locked-down setups.
    pub bind: String,
    /// 0 = OS-assigned.
    pub port: u16,
}

impl Default for RemoteServerConfig {
    fn default() -> Self {
        RemoteServerConfig {
            bind: "0.0.0.0".into(),
            port: 0,
        }
    }
}

pub(crate) struct ServerCtx {
    token: String,
    started_ms: u64,
    limiter: Mutex<RateLimiter>,
    clients: Mutex<ClientRegistry>,
    audit: AuditLog,
}

impl ServerCtx {
    pub(crate) fn new(token: String, audit_path: PathBuf) -> Self {
        ServerCtx {
            token,
            started_ms: current_timestamp_ms(),
            limiter: Mutex::new(RateLimiter::default()),
            clients: Mutex::new(ClientRegistry::default()),
            audit: AuditLog::new(audit_path),
        }
    }
}

fn persist_presence(ctx: &Arc<ServerCtx>) {
    let snapshot = ctx
        .clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .presence(current_timestamp_ms());
    let path = presence_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("{snapshot}\n"));
}

/// Start the remote control server and serve until the process is killed.
pub fn run(config: &RemoteServerConfig) -> Result<(), String> {
    let material = ensure_tls_material()?;
    let fingerprint = material.fingerprint.clone();
    let tls_config = build_tls_config(material)?;
    let token = generate_token();

    let listener = TcpListener::bind((config.bind.as_str(), config.port))
        .map_err(|e| format!("Failed to bind {}:{}: {e}", config.bind, config.port))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read bound address: {e}"))?
        .port();
    // A concrete bind address is also the reachable address; only an
    // all-interfaces bind needs the LAN IP looked up for the shareable URL.
    let display_host = match config.bind.parse::<IpAddr>() {
        Ok(addr) if !addr.is_unspecified() => config.bind.clone(),
        _ => local_lan_ip().unwrap_or_else(|| "127.0.0.1".into()),
    };
    let url = format!("https://{display_host}:{port}");

    write_remote_state(&url, &token, port, &fingerprint)?;

    let ctx = Arc::new(ServerCtx::new(token.clone(), audit_log_path()));
    ctx.audit.log(
        "server_start",
        json!({ "url": url, "bind": config.bind, "port": port }),
    );

    // Machine-readable connection info on stdout (the token is also in
    // remote.json with 0600 perms); human summary on stderr.
    println!(
        "{}",
        json!({ "url": url, "port": port, "fingerprint": fingerprint, "token": token })
    );
    eprintln!("unpeel remote control listening on {url} (TLS fingerprint sha256:{fingerprint})");

    for incoming in listener.incoming() {
        let Ok(tcp) = incoming else { continue };
        // Nagle + delayed ACK stalls small frames (keystroke echoes, output
        // chunks) ~40-50ms; measured on the relay path via __relay_probe__.
        tcp.set_nodelay(true).ok();
        let peer_ip = match tcp.peer_addr() {
            Ok(addr) => addr.ip(),
            Err(_) => continue,
        };
        let ctx = Arc::clone(&ctx);
        let tls_config = Arc::clone(&tls_config);
        thread::spawn(move || {
            let Ok(conn) = rustls::ServerConnection::new(tls_config) else {
                return;
            };
            let stream = rustls::StreamOwned::new(conn, tcp);
            handle_connection(stream, peer_ip, &ctx);
        });
    }
    Ok(())
}

fn write_remote_state(url: &str, token: &str, port: u16, fingerprint: &str) -> Result<(), String> {
    let path = remote_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create state dir: {e}"))?;
    }
    let state = json!({
        "url": url,
        "token": token,
        "port": port,
        "fingerprint": fingerprint,
        "pid": std::process::id(),
        // Lets a reaper prove this pid still belongs to this server before
        // signaling it — under load the pid counter wraps fast enough that a
        // bare pid routinely points at an unrelated process.
        "pid_started_at": crate::session_host::process_start_time_ms(std::process::id()),
    });
    write_private(&path, format!("{state}\n").as_bytes())
}

pub fn run_cli(args: &[String]) -> Result<(), String> {
    let mut config = RemoteServerConfig::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--bind" => {
                config.bind = iter
                    .next()
                    .ok_or("--bind requires an address argument")?
                    .clone();
            }
            "--port" => {
                config.port = iter
                    .next()
                    .ok_or("--port requires a port argument")?
                    .parse()
                    .map_err(|e| format!("Invalid --port value: {e}"))?;
            }
            other => return Err(format!("Unknown remote server argument '{other}'")),
        }
    }
    run(&config)
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

pub(crate) fn handle_connection<S: ConnStream>(mut stream: S, peer: IpAddr, ctx: &Arc<ServerCtx>) {
    let _ = stream.set_read_timeout_ms(Some(REQUEST_READ_TIMEOUT_MS));
    let (request, leftover) = match read_request(&mut stream) {
        Ok(parsed) => parsed,
        Err(message) => {
            write_error(&mut stream, 400, "Bad Request", &message);
            return;
        }
    };
    let stream = PrefixedStream::new(leftover, stream);
    route_request(stream, request, peer, ctx);
}

fn route_request<S: ConnStream>(
    mut stream: PrefixedStream<S>,
    request: Request,
    peer: IpAddr,
    ctx: &Arc<ServerCtx>,
) {
    // The web client shell is served unauthenticated (the token arrives via
    // the QR query string and lives in the browser, never in this page).
    if request.method == "GET" && request.path == "/" {
        write_response(
            &mut stream,
            200,
            "OK",
            "text/html; charset=utf-8",
            WEB_CLIENT_PLACEHOLDER.as_bytes(),
        );
        return;
    }

    let now_ms = current_timestamp_ms();
    match ctx
        .limiter
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .check(peer, now_ms)
    {
        RateDecision::Allowed => {}
        RateDecision::Limited | RateDecision::Blocked => {
            ctx.audit
                .log("auth_rate_limited", json!({ "ip": peer.to_string() }));
            write_error(
                &mut stream,
                429,
                "Too Many Requests",
                "Too many failed authentication attempts",
            );
            return;
        }
    }

    let Some(identity) = authorized(&request, &ctx.token) else {
        let mut limiter = ctx.limiter.lock().unwrap_or_else(|p| p.into_inner());
        limiter.record_failure(peer, now_ms);
        drop(limiter);
        ctx.audit.log(
            "auth_failure",
            json!({ "ip": peer.to_string(), "path": request.path }),
        );
        write_error(&mut stream, 401, "Unauthorized", "Invalid or missing token");
        return;
    };
    {
        let mut limiter = ctx.limiter.lock().unwrap_or_else(|p| p.into_inner());
        limiter.record_success(peer);
    }
    let device = identity.device_label();
    let websocket_lease = identity.websocket_lease();
    let is_new_client = ctx
        .clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .touch_http(&peer, device.as_deref(), now_ms);
    if is_new_client {
        ctx.audit.log(
            "client_connect",
            json!({ "ip": peer.to_string(), "device": device }),
        );
    }

    let segments: Vec<&str> = request.path.split('/').filter(|s| !s.is_empty()).collect();
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["api", "status"]) => api_status(&mut stream, ctx),
        ("GET", ["api", "sessions"]) => api_sessions(&mut stream),
        ("GET", ["api", "sessions", id]) => api_session(&mut stream, id),
        ("GET", ["api", "sessions", id, "activity"]) => api_activity(&mut stream, id),
        ("GET", ["api", "sessions", id, "metrics"]) => api_metrics(&mut stream, id),
        ("GET", ["api", "sessions", id, "viewers"]) => api_viewers(&mut stream, id, ctx),
        ("GET", ["api", "sessions", id, "artifacts", "browser"]) => {
            api_browser_artifacts_list(&mut stream, id)
        }
        ("GET", ["api", "sessions", id, "artifacts", "browser", "preview"]) => {
            api_browser_preview(&mut stream, id, &request)
        }
        ("GET", ["api", "sessions", id, "artifacts", "browser", kind, file]) => {
            api_browser_artifact_fetch(&mut stream, id, kind, file)
        }
        ("GET", ["api", "sessions", id, "output"]) => {
            if is_websocket_upgrade(&request) {
                let id = id.to_string();
                handle_output_upgrade(stream, &request, &id, peer, device, websocket_lease, ctx);
            } else {
                api_output_poll(&mut stream, id, &request.query, peer, device, ctx);
            }
        }
        ("POST", ["api", "sessions", id, "input"]) => {
            api_input(&mut stream, id, &request.body, peer, ctx)
        }
        ("POST", ["api", "sessions", id, "write"]) => {
            api_write_raw(&mut stream, id, &request.body, peer, ctx)
        }
        ("POST", ["api", "sessions", id, "resize"]) => {
            api_resize(&mut stream, id, &request.body, peer, ctx)
        }
        ("POST", ["api", "sessions", id, "kill"]) => api_kill(&mut stream, id, peer, ctx),
        ("GET", ["api", "clients"]) => api_clients(&mut stream, ctx),
        _ => write_error(&mut stream, 404, "Not Found", "Unknown endpoint"),
    }
}

/// Who a request authenticated as: the per-start server token (QR/web flow)
/// or a persistent paired device from the app's pairing store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthIdentity {
    ServerToken,
    PairedDevice {
        id: String,
        name: String,
        token_hash: String,
    },
}

impl AuthIdentity {
    fn device_label(&self) -> Option<String> {
        match self {
            AuthIdentity::ServerToken => None,
            AuthIdentity::PairedDevice { id, name, .. } => Some(format!("{name} ({id})")),
        }
    }

    fn websocket_lease(&self) -> WsAuthorizationLease {
        match self {
            AuthIdentity::ServerToken => WsAuthorizationLease::ServerToken,
            AuthIdentity::PairedDevice { id, token_hash, .. } => {
                WsAuthorizationLease::PairedDevice {
                    id: id.clone(),
                    token_hash: token_hash.clone(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WsAuthorizationLease {
    ServerToken,
    PairedDevice { id: String, token_hash: String },
}

/// The same header/token forms the app's mobile server accepts: `Bearer x` or
/// a bare token, in `authorization` or `x-unpeel-mobile-auth`, plus the
/// `?token=` query parameter for WebSocket upgrades.
fn provided_token(request: &Request) -> Option<&str> {
    fn from_header(value: &str) -> Option<&str> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        match trimmed.split_once(' ') {
            Some((scheme, token)) if scheme.eq_ignore_ascii_case("bearer") => Some(token.trim()),
            Some(_) => None,
            None => Some(trimmed),
        }
    }
    request
        .headers
        .get("authorization")
        .and_then(|value| from_header(value))
        .or_else(|| {
            request
                .headers
                .get("x-unpeel-mobile-auth")
                .and_then(|value| from_header(value))
        })
        .or_else(|| request.query.get("token").map(String::as_str))
}

fn authorized(request: &Request, expected: &str) -> Option<AuthIdentity> {
    let token = provided_token(request)?;
    if constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Some(AuthIdentity::ServerToken);
    }
    paired_device_for_token(&paired_devices_path(), token).map(|(id, name)| {
        AuthIdentity::PairedDevice {
            id,
            name,
            token_hash: sha256_hex(token),
        }
    })
}

fn websocket_lease_is_current(path: &std::path::Path, lease: &WsAuthorizationLease) -> bool {
    let WsAuthorizationLease::PairedDevice { id, token_hash } = lease else {
        return true;
    };
    let Ok(raw) = std::fs::read(path) else {
        return false;
    };
    let Ok(file) = serde_json::from_slice::<Value>(&raw) else {
        return false;
    };
    file.get("devices")
        .and_then(Value::as_array)
        .and_then(|devices| {
            devices
                .iter()
                .find(|device| device.get("id").and_then(Value::as_str) == Some(id.as_str()))
        })
        .and_then(|device| device.get("tokenHash"))
        .and_then(Value::as_str)
        .is_some_and(|stored| constant_time_eq(stored.as_bytes(), token_hash.as_bytes()))
}

fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Look the token up in the app's paired-device store (re-read per request so
/// pairing and revocation in the app apply live). Matches
/// `MobilePairingStore.verifyAuthorizationHeader`: the file stores lowercase
/// hex SHA-256 of the raw token.
pub(crate) fn paired_device_for_token(
    path: &std::path::Path,
    token: &str,
) -> Option<(String, String)> {
    if token.is_empty() {
        return None;
    }
    let raw = std::fs::read(path).ok()?;
    let file: Value = serde_json::from_slice(&raw).ok()?;
    let hash = sha256_hex(token);
    file.get("devices")?.as_array()?.iter().find_map(|device| {
        let stored = device.get("tokenHash")?.as_str()?;
        if !constant_time_eq(stored.as_bytes(), hash.as_bytes()) {
            return None;
        }
        let id = device.get("id")?.as_str()?.to_string();
        let name = device
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("device")
            .to_string();
        Some((id, name))
    })
}

/// Reject ids that could escape the app-sessions directory (same guard as the
/// MCP host's manifest loader).
fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && !session_id.contains('/')
        && !session_id.contains("..")
        && !session_id.contains('\\')
}

fn load_manifest_checked(session_id: &str) -> Option<HostedSessionManifest> {
    if !valid_session_id(session_id) {
        return None;
    }
    session_host::load_manifest(session_id)
}

// ---------------------------------------------------------------------------
// REST handlers
// ---------------------------------------------------------------------------

fn session_json(manifest: &HostedSessionManifest, activity: &str) -> Value {
    json!({
        "id": manifest.session.id,
        "command": manifest.session.command,
        "label": manifest.session.label,
        "project_id": manifest.session.project_id,
        "created_at": manifest.session.created_at,
        "state": match manifest.state {
            HostedSessionState::Running => "running",
            HostedSessionState::Exited => "exited",
        },
        "activity": activity,
        "worktree_branch": manifest.session.worktree_branch,
    })
}

fn api_status<W: Write>(stream: &mut W, ctx: &Arc<ServerCtx>) {
    let session_count = session_host::list_manifests()
        .iter()
        .filter(|m| m.state == HostedSessionState::Running)
        .count();
    let client_count = ctx
        .clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot(current_timestamp_ms())
        .len();
    let uptime_secs = current_timestamp_ms().saturating_sub(ctx.started_ms) / 1000;
    write_json(
        stream,
        200,
        "OK",
        &json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_secs": uptime_secs,
            "session_count": session_count,
            "client_count": client_count,
        }),
    );
}

fn api_sessions<W: Write>(stream: &mut W) {
    let activity = mcp_host::load_activity_state();
    let mut manifests = session_host::list_manifests();
    manifests.retain(|m| m.state == HostedSessionState::Running);
    manifests.sort_by(|a, b| b.session.created_at.cmp(&a.session.created_at));
    let sessions: Vec<Value> = manifests
        .iter()
        .map(|m| session_json(m, &mcp_host::activity_status_for_manifest(&activity, m)))
        .collect();
    write_json(stream, 200, "OK", &json!({ "sessions": sessions }));
}

fn api_session<W: Write>(stream: &mut W, session_id: &str) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    let activity = mcp_host::load_activity_state();
    let status = mcp_host::activity_status_for_manifest(&activity, &manifest);
    write_json(stream, 200, "OK", &session_json(&manifest, &status));
}

fn api_activity<W: Write>(stream: &mut W, session_id: &str) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    let activity = mcp_host::load_activity_state();
    let status = mcp_host::activity_status_for_manifest(&activity, &manifest);
    write_json(stream, 200, "OK", &json!({ "state": status }));
}

fn api_input<W: Write>(
    stream: &mut W,
    session_id: &str,
    body: &[u8],
    peer: IpAddr,
    ctx: &Arc<ServerCtx>,
) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    if manifest.state != HostedSessionState::Running {
        write_error(stream, 409, "Conflict", "Session has exited");
        return;
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            write_error(stream, 400, "Bad Request", "Body must be JSON");
            return;
        }
    };
    let Some(text) = parsed.get("text").and_then(Value::as_str) else {
        write_error(stream, 400, "Bad Request", "Missing string field 'text'");
        return;
    };
    let submit = parsed
        .get("submit")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let sanitized = session_input::sanitize_paste_text(text);
    if sanitized.is_empty() && !submit {
        write_error(
            stream,
            400,
            "Bad Request",
            "Text is empty after removing control characters",
        );
        return;
    }

    let result = session_input::deliver_sanitized_text(&manifest.session.id, &sanitized, submit);

    let truncated: String = sanitized.chars().take(80).collect();
    ctx.audit.log(
        "input_sent",
        json!({
            "ip": peer.to_string(),
            "session_id": manifest.session.id,
            "chars": sanitized.chars().count(),
            "preview": truncated,
            "submit": submit,
            "ok": result.is_ok(),
        }),
    );

    match result {
        Ok(()) => write_json(stream, 200, "OK", &json!({ "ok": true })),
        Err(message) => write_error(stream, 502, "Bad Gateway", &message),
    }
}

/// Raw byte passthrough for interactive terminal clients (Mac-as-client,
/// web): unlike `/input` (bracketed-paste prompt recipe, control bytes
/// stripped), this writes exactly what the client's terminal emitted —
/// arrow keys, Esc, Ctrl-C — so TUI menus and interrupts work. Mirrors the
/// app server's /mobile/write. Audit-logs byte count only: raw keystrokes
/// can contain secrets typed into the terminal, so no preview.
fn api_write_raw<W: Write>(
    stream: &mut W,
    session_id: &str,
    body: &[u8],
    peer: IpAddr,
    ctx: &Arc<ServerCtx>,
) {
    // Feature-flagged with the remote-attach client: answers as if the
    // endpoint does not exist until the operator opts in.
    if !crate::remote_attach::remote_attach_enabled() {
        write_error(stream, 404, "Not Found", "Unknown endpoint");
        return;
    }
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    if manifest.state != HostedSessionState::Running {
        write_error(stream, 409, "Conflict", "Session has exited");
        return;
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            write_error(stream, 400, "Bad Request", "Body must be JSON");
            return;
        }
    };
    let Some(data) = parsed.get("data").and_then(Value::as_str) else {
        write_error(stream, 400, "Bad Request", "Missing string field 'data'");
        return;
    };
    if data.is_empty() {
        write_error(stream, 400, "Bad Request", "Empty data");
        return;
    }
    if data.len() > 64 * 1024 {
        write_error(stream, 400, "Bad Request", "Data exceeds 64KB");
        return;
    }

    let write_id = parsed
        .get("wid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .map(str::to_string);
    let result = write_session(&manifest.session.id, data, write_id);
    ctx.audit.log(
        "raw_input_sent",
        json!({
            "ip": peer.to_string(),
            "session_id": manifest.session.id,
            "bytes": data.len(),
            "ok": result.is_ok(),
        }),
    );
    match result {
        Ok(()) => write_json(stream, 200, "OK", &json!({ "ok": true })),
        Err(message) => write_error(stream, 502, "Bad Gateway", &message),
    }
}

fn write_session(session_id: &str, data: &str, write_id: Option<String>) -> Result<(), String> {
    session_host::send_command(
        session_id,
        &SessionHostCommand::Write {
            data: data.to_string(),
            write_id,
        },
    )
}

/// Resize the session PTY. Callers that render a shared session should
/// prefer follower-mode (render the current grid, see `/metrics`) and only
/// resize with explicit user intent — the desktop letterboxes in response
/// (`PhoneResizeOverride` in the native app), it does not fight the resize.
fn api_resize<W: Write>(
    stream: &mut W,
    session_id: &str,
    body: &[u8],
    peer: IpAddr,
    ctx: &Arc<ServerCtx>,
) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    if manifest.state != HostedSessionState::Running {
        write_error(stream, 409, "Conflict", "Session has exited");
        return;
    }
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            write_error(stream, 400, "Bad Request", "Body must be JSON");
            return;
        }
    };
    let cols = parsed.get("cols").and_then(Value::as_u64).unwrap_or(0);
    let rows = parsed.get("rows").and_then(Value::as_u64).unwrap_or(0);
    if !(2..=1000).contains(&cols) || !(2..=1000).contains(&rows) {
        write_error(
            stream,
            400,
            "Bad Request",
            "cols and rows must be between 2 and 1000",
        );
        return;
    }
    let result = session_host::send_command(
        &manifest.session.id,
        &SessionHostCommand::Resize {
            cols: cols as u16,
            rows: rows as u16,
        },
    );
    ctx.audit.log(
        "session_resized",
        json!({
            "ip": peer.to_string(),
            "session_id": manifest.session.id,
            "cols": cols,
            "rows": rows,
            "ok": result.is_ok(),
        }),
    );
    match result {
        Ok(()) => write_json(stream, 200, "OK", &json!({ "ok": true })),
        Err(message) => write_error(stream, 502, "Bad Gateway", &message),
    }
}

/// Current PTY grid plus the output.bin tail offset, so a client can size its
/// surface to the real grid (follower mode) and start a stream at the tail.
fn api_metrics<W: Write>(stream: &mut W, session_id: &str) {
    match controller_api::read_session_metrics(session_id) {
        Ok(metrics) => write_json(
            stream,
            200,
            "OK",
            &json!({
                "cols": metrics.columns,
                "rows": metrics.rows,
                "output_offset": metrics.output_offset,
            }),
        ),
        Err(error) => {
            let reason = match error.status {
                400 => "Bad Request",
                404 => "Not Found",
                409 => "Conflict",
                502 => "Bad Gateway",
                _ => "Error",
            };
            write_error(stream, error.status, reason, &error.message);
        }
    }
}

/// Plain-GET fallback for the output endpoint: one bounded chunk as JSON,
/// with optional long-polling via `wait_ms` when the caller passes an
/// explicit offset. Lets simple HTTP clients (the iOS app's polling loop)
/// use this server without speaking WebSocket.
fn api_output_poll<W: Write>(
    stream: &mut W,
    session_id: &str,
    query: &HashMap<String, String>,
    peer: IpAddr,
    device: Option<String>,
    ctx: &Arc<ServerCtx>,
) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    ctx.clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .touch_poll_viewer(
            &peer,
            device.as_deref(),
            &manifest.session.id,
            current_timestamp_ms(),
        );
    persist_presence(ctx);
    let offset: Option<u64> = query.get("offset").and_then(|v| v.parse().ok());
    let max_bytes: usize = query
        .get("max_bytes")
        .and_then(|v| v.parse().ok())
        .unwrap_or(OUTPUT_POLL_DEFAULT_BYTES)
        .min(OUTPUT_POLL_MAX_BYTES);
    let wait_ms: u64 = query
        .get("wait_ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .min(OUTPUT_POLL_MAX_WAIT_MS);

    let deadline = std::time::Instant::now() + Duration::from_millis(wait_ms);
    let chunk = loop {
        let read = match offset {
            Some(from) => session_host::read_output_chunk(
                &manifest.session.id,
                Some(from),
                Some(max_bytes),
                None,
            ),
            None => session_host::read_output_chunk(
                &manifest.session.id,
                None,
                Some(max_bytes),
                Some(max_bytes),
            ),
        };
        let chunk = match read {
            Ok(chunk) => chunk,
            Err(message) => {
                write_error(stream, 502, "Bad Gateway", &message);
                return;
            }
        };
        // Long-poll only applies to explicit-offset reads with nothing new;
        // tail reads always return immediately.
        let chunk_start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
        if !chunk.data.is_empty()
            || chunk.exited
            || offset.is_none()
            || offset.is_some_and(|requested| requested != chunk_start)
            || std::time::Instant::now() >= deadline
        {
            break chunk;
        }
        thread::sleep(Duration::from_millis(OUTPUT_POLL_SLEEP_MS));
    };

    let encoded = {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        STANDARD.encode(&chunk.data)
    };
    let start_offset = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
    let mut body = json!({
        "data_base64": encoded,
        "offset": start_offset,
        "next_offset": chunk.next_offset,
        "truncated": offset.map_or(start_offset > 0, |requested| requested != start_offset),
        "exited": chunk.exited,
        "exists": chunk.exists,
    });
    // A fresh tail read is a replay baseline (the client resets first), so
    // it carries the mode-restore preamble like the WebSocket hello does.
    if offset.is_none() {
        if let Some(preamble) =
            replay_mode_preamble_base64(manifest.terminal_modes.as_ref(), start_offset)
        {
            body["mode_preamble_base64"] = json!(preamble);
        }
    }
    write_json(stream, 200, "OK", &body);
}

fn api_kill<W: Write>(stream: &mut W, session_id: &str, peer: IpAddr, ctx: &Arc<ServerCtx>) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    let result = session_host::send_command(&manifest.session.id, &SessionHostCommand::Kill);
    ctx.audit.log(
        "session_killed",
        json!({
            "ip": peer.to_string(),
            "session_id": manifest.session.id,
            "ok": result.is_ok(),
        }),
    );
    match result {
        Ok(()) => write_json(stream, 200, "OK", &json!({ "ok": true })),
        Err(message) => write_error(stream, 502, "Bad Gateway", &message),
    }
}

fn api_viewers<W: Write>(stream: &mut W, session_id: &str, ctx: &Arc<ServerCtx>) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    };
    let viewers = ctx
        .clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .viewers_for(&manifest.session.id, current_timestamp_ms());
    write_json(stream, 200, "OK", &json!({ "viewers": viewers }));
}

fn browser_artifacts_dir(session_id: &str) -> PathBuf {
    session_host::session_dir(session_id)
        .join("artifacts")
        .join("browser")
}

const BROWSER_ARTIFACT_KINDS: [&str; 2] = ["screenshots", "downloads"];

/// Same traversal rules as session ids, applied to artifact path segments.
fn valid_artifact_segment(value: &str) -> bool {
    valid_session_id(value)
}

fn api_browser_artifacts_list<W: Write>(stream: &mut W, session_id: &str) {
    if load_manifest_checked(session_id).is_none() {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    }
    let mut artifacts = Vec::new();
    for kind in BROWSER_ARTIFACT_KINDS {
        let dir = browser_artifacts_dir(session_id).join(kind);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            artifacts.push(json!({
                "kind": kind,
                "name": entry.file_name().to_string_lossy(),
                "size": meta.len(),
                "modified_at": modified_ms,
            }));
        }
    }
    artifacts.sort_by_key(|a| std::cmp::Reverse(a["modified_at"].as_u64().unwrap_or(0)));
    write_json(stream, 200, "OK", &json!({ "artifacts": artifacts }));
}

fn artifact_content_type(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain; charset=utf-8",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

fn api_browser_artifact_fetch<W: Write>(stream: &mut W, session_id: &str, kind: &str, file: &str) {
    if load_manifest_checked(session_id).is_none()
        || !BROWSER_ARTIFACT_KINDS.contains(&kind)
        || !valid_artifact_segment(file)
    {
        write_error(stream, 404, "Not Found", "Unknown artifact");
        return;
    }
    match std::fs::read(browser_artifacts_dir(session_id).join(kind).join(file)) {
        Ok(bytes) => write_response(stream, 200, "OK", artifact_content_type(file), &bytes),
        Err(_) => write_error(stream, 404, "Not Found", "Unknown artifact"),
    }
}

/// Newest screenshot as a poll-friendly preview: the ETag is mtime:size, so
/// a ~1 fps poller sending If-None-Match pays a 304 while the frame is
/// unchanged (the interim stand-in for real viewport streaming).
fn api_browser_preview<W: Write>(stream: &mut W, session_id: &str, request: &Request) {
    if load_manifest_checked(session_id).is_none() {
        write_error(stream, 404, "Not Found", "Unknown session");
        return;
    }
    let dir = browser_artifacts_dir(session_id).join("screenshots");
    let newest = std::fs::read_dir(&dir).ok().and_then(|entries| {
        entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                let modified = meta.modified().ok()?;
                Some((modified, meta.len(), entry.path()))
            })
            .max_by_key(|(modified, _, _)| *modified)
    });
    let Some((modified, size, path)) = newest else {
        write_error(stream, 404, "Not Found", "No screenshots captured yet");
        return;
    };
    let mtime_ms = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let etag = format!("\"{mtime_ms}:{size}\"");
    if request.headers.get("if-none-match") == Some(&etag) {
        let header =
            format!("HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nConnection: close\r\n\r\n");
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.flush();
        return;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        write_error(stream, 404, "Not Found", "Screenshot unreadable");
        return;
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
        artifact_content_type(&name),
        bytes.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&bytes);
    let _ = stream.flush();
}

fn api_clients<W: Write>(stream: &mut W, ctx: &Arc<ServerCtx>) {
    let clients = ctx
        .clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot(current_timestamp_ms());
    write_json(stream, 200, "OK", &json!({ "clients": clients }));
}

const WEB_CLIENT_PLACEHOLDER: &str = "<!doctype html>\n<html><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>Unpeel Remote</title></head>\
<body style=\"font-family: -apple-system, sans-serif; padding: 2rem;\">\
<h1>Unpeel Remote Control</h1>\
<p>The server is running. The built-in web client ships in phase 2; \
use the REST/WebSocket API with your bearer token for now.</p>\
</body></html>\n";

// ---------------------------------------------------------------------------
// WebSocket output streaming
// ---------------------------------------------------------------------------
//
// Wire protocol (v1) on `GET /api/sessions/:id/output` with a WebSocket
// upgrade (auth via `?token=`, which accepts both the per-start server token
// and paired-device tokens):
//
// server → client
//   1. One text frame immediately after the upgrade:
//        { "type": "hello", "protocol": 1, "session_id": "...",
//          "state": "running", "output_size": <u64>,
//          "requested_offset": <u64|null>, "start_offset": <u64>,
//          "rebased": <bool>, "cols": <u16|null>, "rows": <u16|null> }
//      `start_offset` is where the following binary stream begins;
//      `rebased: true` means the requested offset was unusable (beyond the
//      file or > WS_RESUME_REPLAY_MAX_BYTES behind) and the stream was
//      restarted from an aligned tail instead.
//      Snapshot baseline (additive, 2026-09-03): a client that advertises
//      `?snapshot=1` on the upgrade request and subscribes fresh (no
//      `?offset=`) may get `"snapshot": true` in hello, in which case
//      `start_offset` is the Host's VT snapshot offset, no
//      `mode_preamble_base64` is sent (the snapshot carries every
//      non-default mode), and exactly one text frame
//        { "type": "baseline", "journal_offset": <u64>, "cols": <u16>,
//          "rows": <u16>, "bytes_base64": "..." }
//      follows hello before any binary frame: reset the VT, feed the bytes,
//      set the cursor to `journal_offset`. `"snapshot": false` (older Host,
//      exited session, transport error) means the raw tail replay below
//      follows exactly as for a client without the capability. Clients
//      that do not advertise it never see either key.
//   2. Binary frames: 8-byte big-endian u64 offset (position of the first
//      payload byte in output.bin) + raw terminal bytes. Frames are
//      contiguous: next expected offset = frame offset + payload length.
//   3. Text frames `{ "type": "error", "message": "..." }` in response to a
//      bad client message (the stream keeps going).
//   4. Ping frames every WS_PING_INTERVAL_MS; a close frame with a status
//      code + reason when the session exits (1000 "session exited"), the
//      host goes away (1011), or the client stops answering (1001).
//
// client → server (text frames, JSON)
//   { "type": "input", "data": "<raw terminal input>" }   — write to PTY
//   { "type": "resize", "cols": N, "rows": N }            — resize PTY
//   plus standard close/ping/pong control frames.

pub fn websocket_accept_key(key: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(key.trim().as_bytes());
    hasher.update(WEBSOCKET_ACCEPT_GUID.as_bytes());
    STANDARD.encode(hasher.finalize())
}

fn is_websocket_upgrade(request: &Request) -> bool {
    let upgrade = request
        .headers
        .get("upgrade")
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    let connection = request
        .headers
        .get("connection")
        .map(|v| v.to_lowercase().contains("upgrade"))
        .unwrap_or(false);
    upgrade && connection && request.headers.contains_key("sec-websocket-key")
}

fn handle_output_upgrade<S: ConnStream>(
    mut stream: PrefixedStream<S>,
    request: &Request,
    session_id: &str,
    peer: IpAddr,
    device: Option<String>,
    authorization_lease: WsAuthorizationLease,
    ctx: &Arc<ServerCtx>,
) {
    let Some(manifest) = load_manifest_checked(session_id) else {
        write_error(&mut stream, 404, "Not Found", "Unknown session");
        return;
    };
    if manifest.state != HostedSessionState::Running {
        write_error(&mut stream, 409, "Conflict", "Session has exited");
        return;
    }
    let offset: Option<u64> = request.query.get("offset").and_then(|v| v.parse().ok());

    let key = request
        .headers
        .get("sec-websocket-key")
        .cloned()
        .unwrap_or_default();
    let accept = websocket_accept_key(&key);
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if stream.write_all(response.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    let now_ms = current_timestamp_ms();
    let client_id = ctx
        .clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .register_ws(&peer, device.as_deref(), &manifest.session.id, now_ms);
    persist_presence(ctx);
    ctx.audit.log(
        "output_stream_start",
        json!({
            "ip": peer.to_string(),
            "session_id": manifest.session.id,
            "offset": offset,
        }),
    );

    // Additive capability: the upgrade request is the client's only
    // "hello", so `?snapshot=1` is how it advertises baseline support.
    let snapshot_capable = request
        .query
        .get("snapshot")
        .is_some_and(|value| value == "1");
    let result = pump_session_output(
        &mut stream,
        &manifest.session.id,
        offset,
        snapshot_capable,
        peer,
        ctx,
        &client_id,
        &authorization_lease,
    );

    ctx.clients
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&client_id);
    persist_presence(ctx);
    ctx.audit.log(
        "output_stream_end",
        json!({
            "ip": peer.to_string(),
            "session_id": manifest.session.id,
            "ok": result.is_ok(),
            "error": result.err(),
        }),
    );
}

/// How a WS stream starts relative to the client's requested offset.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WsReplayPlan {
    /// `Some(offset)` = replay `output.bin` from exactly this offset (the
    /// client's held offset was usable). `None` = start from a fresh aligned
    /// tail read instead.
    pub(crate) explicit_start: Option<u64>,
    /// True when the client asked for an offset the server refused (beyond
    /// the file, or more than `WS_RESUME_REPLAY_MAX_BYTES` behind) — the
    /// stream was rebased to a tail and the client's scrollback has a gap.
    pub(crate) rebased: bool,
}

pub(crate) fn plan_ws_replay(
    requested: Option<u64>,
    file_len: u64,
    retained_from: u64,
) -> WsReplayPlan {
    match requested {
        None => WsReplayPlan {
            explicit_start: None,
            rebased: false,
        },
        Some(from) if from < retained_from => WsReplayPlan {
            explicit_start: None,
            rebased: true,
        },
        Some(from) if from > file_len => WsReplayPlan {
            explicit_start: None,
            rebased: true,
        },
        Some(from) if file_len - from > WS_RESUME_REPLAY_MAX_BYTES => WsReplayPlan {
            explicit_start: None,
            rebased: true,
        },
        Some(from) => WsReplayPlan {
            explicit_start: Some(from),
            rebased: false,
        },
    }
}

/// The DEC-mode restore preamble a client must feed into its freshly reset
/// VT before a replay that does not start at the session's origin: the
/// sequences that established mouse tracking, alt screen, bracketed paste,
/// and friends usually precede the retained tail, so a phone connecting
/// after a Host/service restart would otherwise believe mouse tracking is
/// off and never forward wheel scrolling. Same bytes `unpeel-attach` emits
/// (`mode_restore_preamble`). Empty at the origin, or when the host
/// publishes no non-default modes. Never counted as journal bytes.
pub(crate) fn replay_mode_preamble(
    modes: Option<&crate::terminal_viewport::TerminalModeState>,
    start_offset: u64,
) -> Vec<u8> {
    if start_offset == 0 {
        return Vec::new();
    }
    modes
        .filter(|modes| !modes.is_default())
        .map(|modes| modes.restore_sequence())
        .unwrap_or_default()
}

/// Base64 the preamble for a JSON field; `None` keeps the field absent so
/// older clients (and the wire) see no difference when there is nothing to
/// restore.
pub(crate) fn replay_mode_preamble_base64(
    modes: Option<&crate::terminal_viewport::TerminalModeState>,
    start_offset: u64,
) -> Option<String> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let preamble = replay_mode_preamble(modes, start_offset);
    (!preamble.is_empty()).then(|| STANDARD.encode(preamble))
}

/// Server→client output frame: 8-byte big-endian u64 byte offset of the
/// first payload byte in `output.bin`, followed by the raw terminal bytes.
pub(crate) fn encode_output_frame(offset: u64, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + data.len());
    frame.extend_from_slice(&offset.to_be_bytes());
    frame.extend_from_slice(data);
    frame
}

/// Server→client `baseline` text frame: the Host's VT snapshot at
/// `journal_offset` (see the wire protocol header). Base64 keeps the binary
/// frame contract untouched: binary frames stay "journal bytes at an offset".
pub(crate) fn encode_baseline_frame(
    journal_offset: u64,
    snapshot: &crate::terminal_viewport::SnapshotVt,
) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    json!({
        "type": "baseline",
        "journal_offset": journal_offset,
        "cols": snapshot.cols,
        "rows": snapshot.rows,
        "bytes_base64": STANDARD.encode(&snapshot.bytes),
    })
    .to_string()
}

/// RFC 6455 close payload: 2-byte big-endian status code + UTF-8 reason.
pub(crate) fn ws_close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + reason.len());
    payload.extend_from_slice(&code.to_be_bytes());
    payload.extend_from_slice(reason.as_bytes());
    payload
}

/// A parsed client→server text-frame message on the output WebSocket.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WsClientMessage {
    /// Raw terminal input (exactly what the client's keyboard emitted —
    /// escape sequences, control chars). Same semantics as `POST /write`,
    /// but always available to authenticated stream clients. `write_id` is
    /// an optional idempotency key the controller reuses if it retries this
    /// send over HTTP after an ambiguous WebSocket delivery.
    Input {
        data: String,
        write_id: Option<String>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
}

pub(crate) fn parse_ws_client_message(payload: &[u8]) -> Result<WsClientMessage, String> {
    let value: Value =
        serde_json::from_slice(payload).map_err(|_| "Client message must be JSON".to_string())?;
    match value.get("type").and_then(Value::as_str) {
        Some("input") => {
            let data = value
                .get("data")
                .and_then(Value::as_str)
                .ok_or("input requires string field 'data'")?;
            if data.is_empty() {
                return Err("input data is empty".into());
            }
            if data.len() > WS_MAX_INPUT_BYTES {
                return Err("input data exceeds 64KB".into());
            }
            let write_id = value
                .get("wid")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128)
                .map(str::to_string);
            Ok(WsClientMessage::Input {
                data: data.to_string(),
                write_id,
            })
        }
        Some("resize") => {
            let cols = value.get("cols").and_then(Value::as_u64).unwrap_or(0);
            let rows = value.get("rows").and_then(Value::as_u64).unwrap_or(0);
            if !(2..=1000).contains(&cols) || !(2..=1000).contains(&rows) {
                return Err("cols and rows must be between 2 and 1000".into());
            }
            Ok(WsClientMessage::Resize {
                cols: cols as u16,
                rows: rows as u16,
            })
        }
        Some(other) => Err(format!("Unknown client message type '{other}'")),
        None => Err("Client message requires string field 'type'".into()),
    }
}

fn send_ws_error<W: Write>(stream: &mut W, message: &str) {
    let frame = json!({ "type": "error", "message": message });
    let _ = write_ws_frame(stream, 0x1, frame.to_string().as_bytes());
}

#[cfg(unix)]
fn websocket_lease_remains_current<S: ConnStream>(
    stream: &mut PrefixedStream<S>,
    authorization_lease: &WsAuthorizationLease,
    session_id: &str,
    peer: IpAddr,
    ctx: &Arc<ServerCtx>,
) -> bool {
    if websocket_lease_is_current(&paired_devices_path(), authorization_lease) {
        return true;
    }
    let _ = write_ws_frame(
        stream,
        0x8,
        &ws_close_payload(1008, "authorization revoked"),
    );
    ctx.audit.log(
        "authorization_revoked",
        json!({
            "ip": peer.to_string(),
            "session_id": session_id,
            "transport": "ws",
        }),
    );
    false
}

#[cfg(unix)]
fn handle_ws_client_text<S: ConnStream>(
    stream: &mut PrefixedStream<S>,
    session_id: &str,
    payload: &[u8],
    peer: IpAddr,
    ctx: &Arc<ServerCtx>,
) {
    let message = match parse_ws_client_message(payload) {
        Ok(message) => message,
        Err(error) => {
            send_ws_error(stream, &error);
            return;
        }
    };
    match message {
        WsClientMessage::Input { data, write_id } => {
            let result = write_session(session_id, &data, write_id);
            // Byte count only: raw keystrokes can contain secrets typed into
            // the terminal, so no preview (same rule as `POST /write`).
            ctx.audit.log(
                "raw_input_sent",
                json!({
                    "ip": peer.to_string(),
                    "session_id": session_id,
                    "bytes": data.len(),
                    "transport": "ws",
                    "ok": result.is_ok(),
                }),
            );
            if let Err(message) = result {
                send_ws_error(stream, &message);
            }
        }
        WsClientMessage::Resize { cols, rows } => {
            let result =
                session_host::send_command(session_id, &SessionHostCommand::Resize { cols, rows });
            ctx.audit.log(
                "session_resized",
                json!({
                    "ip": peer.to_string(),
                    "session_id": session_id,
                    "cols": cols,
                    "rows": rows,
                    "transport": "ws",
                    "ok": result.is_ok(),
                }),
            );
            if let Err(message) = result {
                send_ws_error(stream, &message);
            }
        }
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn pump_session_output<S: ConnStream>(
    stream: &mut PrefixedStream<S>,
    session_id: &str,
    requested_offset: Option<u64>,
    snapshot_capable: bool,
    peer: IpAddr,
    ctx: &Arc<ServerCtx>,
    client_id: &str,
    authorization_lease: &WsAuthorizationLease,
) -> Result<(), String> {
    use crate::session_host::OutputStreamRead;

    if !websocket_lease_remains_current(stream, authorization_lease, session_id, peer, ctx) {
        return Ok(());
    }

    let file_len = std::fs::metadata(session_host::output_path(session_id))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let retained_from = session_host::output_retained_from(session_id).min(file_len);
    let plan = plan_ws_replay(requested_offset, file_len, retained_from);

    // Best-effort current PTY grid for the hello frame; hosts from older
    // builds (or a briefly unreachable socket) just omit it.
    let grid = session_host::request_current_viewport_snapshot(session_id, 0, Some(1))
        .ok()
        .filter(|snapshot| snapshot.cols > 2 || snapshot.rows > 2);

    // Snapshot baseline: only for a fresh subscribe by a client that
    // advertised it. An explicit `?offset=` resume already holds state, and
    // any failure (older Host without the command, exited, timeout) simply
    // degrades to the raw tail path below.
    let snapshot = (snapshot_capable && plan.explicit_start.is_none())
        .then(|| session_host::request_terminal_snapshot_vt(session_id).ok())
        .flatten();

    // Stage the first disk page before hello for BOTH paths. Retention may
    // advance between the planning stat and this read; only the actual page
    // start can truthfully tell the client whether its cursor was rebased.
    let mut baseline: Option<(u64, crate::terminal_viewport::SnapshotVt)> = None;
    let (start_offset, first_chunk, continue_explicit_replay, rebased) =
        match (plan.explicit_start, snapshot) {
            // Baseline: stream from the snapshot's journal offset through the
            // same explicit-replay machinery an `?offset=` resume uses (journal
            // pages first, then the live subscribe at the resulting tail), so
            // the broadcaster-ring race needs no separate handling. If retention
            // already passed the offset the snapshot cannot be joined to the
            // stream; fall back to the raw tail.
            (None, Some((journal_offset, vt))) => {
                let chunk = session_host::read_output_chunk(
                    session_id,
                    Some(journal_offset),
                    Some(OUTPUT_REPLAY_CHUNK_BYTES),
                    None,
                )?;
                let actual = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
                if actual == journal_offset {
                    baseline = Some((journal_offset, vt));
                    (journal_offset, chunk, true, false)
                } else {
                    let chunk = session_host::read_output_chunk(
                        session_id,
                        None,
                        Some(OUTPUT_REPLAY_TAIL_BYTES),
                        Some(OUTPUT_REPLAY_TAIL_BYTES),
                    )?;
                    let start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
                    (start, chunk, false, plan.rebased)
                }
            }
            (Some(from), _) => {
                let chunk = session_host::read_output_chunk(
                    session_id,
                    Some(from),
                    Some(OUTPUT_REPLAY_CHUNK_BYTES),
                    None,
                )?;
                let actual = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
                (actual, chunk, true, plan.rebased || actual != from)
            }
            (None, None) => {
                let chunk = session_host::read_output_chunk(
                    session_id,
                    None,
                    Some(OUTPUT_REPLAY_TAIL_BYTES),
                    Some(OUTPUT_REPLAY_TAIL_BYTES),
                )?;
                let start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
                (start, chunk, false, plan.rebased)
            }
        };

    // Read fresh: the manifest's mode flags follow the host's viewport scan,
    // and this replay's baseline is what the client resets to.
    let mode_preamble = if baseline.is_some() {
        None
    } else {
        replay_mode_preamble_base64(
            session_host::load_manifest(session_id)
                .and_then(|manifest| manifest.terminal_modes)
                .as_ref(),
            start_offset,
        )
    };
    let mut hello = json!({
        "type": "hello",
        "protocol": WS_STREAM_PROTOCOL_VERSION,
        "session_id": session_id,
        "state": "running",
        "output_size": file_len.max(first_chunk.next_offset),
        "requested_offset": requested_offset,
        "start_offset": start_offset,
        "rebased": rebased,
        "cols": grid.as_ref().map(|snapshot| snapshot.cols),
        "rows": grid.as_ref().map(|snapshot| snapshot.rows),
    });
    if let Some(preamble) = mode_preamble {
        // Additive: unknown hello keys are ignored by older clients.
        hello["mode_preamble_base64"] = json!(preamble);
    }
    if snapshot_capable {
        hello["snapshot"] = json!(baseline.is_some());
    }
    if !websocket_lease_remains_current(stream, authorization_lease, session_id, peer, ctx) {
        return Ok(());
    }
    write_ws_frame(stream, 0x1, hello.to_string().as_bytes())
        .map_err(|e| format!("Failed to write hello frame: {e}"))?;
    if let Some((journal_offset, vt)) = baseline.take() {
        if !websocket_lease_remains_current(stream, authorization_lease, session_id, peer, ctx) {
            return Ok(());
        }
        write_ws_frame(
            stream,
            0x1,
            encode_baseline_frame(journal_offset, &vt).as_bytes(),
        )
        .map_err(|e| format!("Failed to write baseline frame: {e}"))?;
    }

    // Replay history from output.bin first (bounded tail, or catch-up from
    // the client's offset), then subscribe the live socket at the resulting
    // tail offset — the same split the attach client uses. Subscribing the
    // in-memory broadcaster far behind its tail would kill the socket.
    if !first_chunk.data.is_empty() {
        if !websocket_lease_remains_current(stream, authorization_lease, session_id, peer, ctx) {
            return Ok(());
        }
        write_ws_frame(
            stream,
            0x2,
            &encode_output_frame(start_offset, &first_chunk.data),
        )
        .map_err(|e| format!("Failed to write replay frame: {e}"))?;
    }
    let first_page_full = first_chunk.data.len() >= OUTPUT_REPLAY_CHUNK_BYTES;
    let mut live_offset = first_chunk.next_offset;
    if continue_explicit_replay && first_page_full {
        loop {
            let chunk = session_host::read_output_chunk(
                session_id,
                Some(live_offset),
                Some(OUTPUT_REPLAY_CHUNK_BYTES),
                None,
            )?;
            let chunk_start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
            if chunk_start != live_offset {
                // Retention overtook this catch-up between pages. Binary
                // frames have offsets but no in-stream reset flag, so close
                // before emitting a discontinuity; reconnect gets a truthful
                // rebased hello and fresh retained tail.
                let _ = write_ws_frame(
                    stream,
                    0x8,
                    &ws_close_payload(1012, "output history evicted"),
                );
                return Ok(());
            }
            if !chunk.data.is_empty() {
                if !websocket_lease_remains_current(
                    stream,
                    authorization_lease,
                    session_id,
                    peer,
                    ctx,
                ) {
                    return Ok(());
                }
                write_ws_frame(stream, 0x2, &encode_output_frame(chunk_start, &chunk.data))
                    .map_err(|e| format!("Failed to write replay frame: {e}"))?;
            }
            let done = chunk.data.len() < OUTPUT_REPLAY_CHUNK_BYTES;
            live_offset = chunk.next_offset;
            if done {
                break;
            }
        }
    }

    let mut upstream = match session_host::connect_output_stream(
        session_id,
        live_offset,
        Some(Duration::from_millis(OUTPUT_PUMP_UPSTREAM_TIMEOUT_MS)),
    ) {
        Ok(upstream) => upstream,
        Err(message) => {
            let _ = write_ws_frame(stream, 0x8, &ws_close_payload(1011, "session host gone"));
            return Err(message);
        }
    };

    let mut last_client_frame_ms = current_timestamp_ms();
    let mut last_ping_ms = last_client_frame_ms;

    loop {
        if !websocket_lease_remains_current(stream, authorization_lease, session_id, peer, ctx) {
            return Ok(());
        }
        // Drain pending client frames (close/ping/pong/input/resize) between
        // upstream reads so the single-threaded pump honors graceful
        // disconnects and keeps remote input latency low.
        for _ in 0..WS_MAX_CLIENT_FRAMES_PER_TICK {
            let _ = stream.set_read_timeout_ms(Some(OUTPUT_PUMP_CLIENT_POLL_MS));
            match read_ws_frame(stream) {
                Ok(Some((0x8, _))) => {
                    let _ = write_ws_frame(stream, 0x8, &ws_close_payload(1000, ""));
                    return Ok(());
                }
                Ok(Some((0x9, payload))) => {
                    last_client_frame_ms = current_timestamp_ms();
                    let _ = write_ws_frame(stream, 0xA, &payload);
                }
                Ok(Some((0x1, payload))) => {
                    last_client_frame_ms = current_timestamp_ms();
                    // Revalidate immediately before every input/resize
                    // effect, not only once per drain batch. A revocation
                    // racing a queued frame therefore cannot borrow the old
                    // upgraded connection for another terminal command.
                    if !websocket_lease_remains_current(
                        stream,
                        authorization_lease,
                        session_id,
                        peer,
                        ctx,
                    ) {
                        return Ok(());
                    }
                    handle_ws_client_text(stream, session_id, &payload, peer, ctx);
                }
                Ok(Some(_)) => {
                    // Pongs and any unrecognized frame types count as
                    // liveness but carry no meaning here.
                    last_client_frame_ms = current_timestamp_ms();
                }
                Ok(None) => break,
                Err(_) => return Ok(()), // client vanished
            }
        }

        let now_ms = current_timestamp_ms();
        if now_ms.saturating_sub(last_client_frame_ms) > WS_CLIENT_LIVENESS_TIMEOUT_MS {
            let _ = write_ws_frame(stream, 0x8, &ws_close_payload(1001, "client unresponsive"));
            return Ok(());
        }
        if now_ms.saturating_sub(last_ping_ms) >= WS_PING_INTERVAL_MS {
            last_ping_ms = now_ms;
            write_ws_frame(stream, 0x9, b"ka")
                .map_err(|e| format!("Failed to write ping frame: {e}"))?;
        }

        match session_host::read_output_stream_frame(&mut upstream) {
            Ok(OutputStreamRead::Chunk(chunk)) => {
                if !websocket_lease_remains_current(
                    stream,
                    authorization_lease,
                    session_id,
                    peer,
                    ctx,
                ) {
                    return Ok(());
                }
                if !chunk.data.is_empty() {
                    let frame_offset = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
                    write_ws_frame(stream, 0x2, &encode_output_frame(frame_offset, &chunk.data))
                        .map_err(|e| format!("Failed to write output frame: {e}"))?;
                    ctx.clients
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .touch(client_id, current_timestamp_ms());
                }
                if chunk.exited {
                    let _ = write_ws_frame(stream, 0x8, &ws_close_payload(1000, "session exited"));
                    return Ok(());
                }
            }
            Ok(OutputStreamRead::TimedOut) => {}
            Ok(OutputStreamRead::Closed) => {
                let _ =
                    write_ws_frame(stream, 0x8, &ws_close_payload(1000, "output stream closed"));
                return Ok(());
            }
            Err(message) => {
                let _ = write_ws_frame(stream, 0x8, &ws_close_payload(1011, "output stream error"));
                return Err(message);
            }
        }
    }
}

#[cfg(not(unix))]
fn pump_session_output<S: ConnStream>(
    _stream: &mut PrefixedStream<S>,
    _session_id: &str,
    _requested_offset: Option<u64>,
    _snapshot_capable: bool,
    _peer: IpAddr,
    _ctx: &Arc<ServerCtx>,
    _client_id: &str,
    _authorization_lease: &WsAuthorizationLease,
) -> Result<(), String> {
    Err("Output streaming is currently only supported on Unix.".into())
}

pub(crate) fn write_ws_frame<W: Write>(
    stream: &mut W,
    opcode: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | (opcode & 0x0f));
    let len = payload.len();
    if len < 126 {
        header.push(len as u8);
    } else if len <= u16::MAX as usize {
        header.push(126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }
    stream.write_all(&header)?;
    stream.write_all(payload)?;
    stream.flush()
}

/// Read one client frame. `Ok(None)` when no frame arrived inside the current
/// read timeout; `Err` when the connection is gone or the frame is invalid.
pub(crate) fn read_ws_frame<S: ConnStream>(
    stream: &mut PrefixedStream<S>,
) -> Result<Option<(u8, Vec<u8>)>, String> {
    let mut first = [0u8; 1];
    match stream.read(&mut first) {
        Ok(0) => return Err("Connection closed".into()),
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(format!("Failed to read frame: {error}")),
    }

    // A frame has started — give the rest of it a real timeout so a slow
    // client can't wedge us, but a fragmented TCP read doesn't error.
    let _ = stream.set_read_timeout_ms(Some(1_000));
    let mut second = [0u8; 1];
    read_exact(stream, &mut second)?;
    let opcode = first[0] & 0x0f;
    let masked = second[0] & 0x80 != 0;
    let mut len = (second[0] & 0x7f) as u64;
    if len == 126 {
        let mut ext = [0u8; 2];
        read_exact(stream, &mut ext)?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        read_exact(stream, &mut ext)?;
        len = u64::from_be_bytes(ext);
    }
    if len > MAX_WS_CLIENT_FRAME_BYTES {
        return Err("Client frame too large".into());
    }
    if !masked {
        return Err("Client frames must be masked".into());
    }
    let mut mask = [0u8; 4];
    read_exact(stream, &mut mask)?;
    let mut payload = vec![0u8; len as usize];
    read_exact(stream, &mut payload)?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
    Ok(Some((opcode, payload)))
}

fn read_exact<R: Read>(stream: &mut R, buf: &mut [u8]) -> Result<(), String> {
    stream
        .read_exact(buf)
        .map_err(|e| format!("Failed to read frame bytes: {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn replay_preamble_restores_alt_screen_and_mouse_modes_off_origin() {
        let modes = crate::terminal_viewport::TerminalModeState {
            alt_screen: true,
            set: vec![1000, 1002, 1006, 2004],
            reset: vec![25],
        };
        let preamble = replay_mode_preamble(Some(&modes), 4_096);
        assert!(
            preamble.starts_with(b"\x1b[?1049h"),
            "alt screen must come first: {preamble:?}"
        );
        assert_eq!(preamble, modes.restore_sequence());
        let encoded = replay_mode_preamble_base64(Some(&modes), 4_096).expect("encoded");
        assert!(!encoded.is_empty());
    }

    #[test]
    fn replay_preamble_is_empty_at_origin_or_without_modes() {
        let modes = crate::terminal_viewport::TerminalModeState {
            alt_screen: true,
            set: vec![1000],
            reset: vec![],
        };
        assert!(replay_mode_preamble(Some(&modes), 0).is_empty());
        assert!(replay_mode_preamble(None, 4_096).is_empty());
        assert!(replay_mode_preamble(
            Some(&crate::terminal_viewport::TerminalModeState::default()),
            4_096
        )
        .is_empty());
        assert!(replay_mode_preamble_base64(None, 4_096).is_none());
    }

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    #[test]
    fn constant_time_eq_matches_and_rejects() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secre"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn generated_token_is_long_hex() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, generate_token());
    }

    #[test]
    fn rate_limiter_limits_window_and_blocks_consecutive() {
        let mut limiter = RateLimiter::default();
        let addr = ip(1);
        let now = 1_000_000;
        assert_eq!(limiter.check(addr, now), RateDecision::Allowed);
        for i in 0..AUTH_MAX_FAILS_PER_WINDOW {
            limiter.record_failure(addr, now + i as u64);
        }
        assert_eq!(limiter.check(addr, now + 10), RateDecision::Limited);
        // Window expires -> allowed again.
        assert_eq!(
            limiter.check(addr, now + AUTH_FAIL_WINDOW_MS + 10),
            RateDecision::Allowed
        );
        // Ten consecutive failures -> hard block, even after the window.
        for i in 0..AUTH_CONSECUTIVE_BLOCK_THRESHOLD as u64 {
            limiter.record_failure(addr, now + i);
        }
        assert_eq!(
            limiter.check(addr, now + AUTH_FAIL_WINDOW_MS + 10),
            RateDecision::Blocked
        );
        assert_eq!(
            limiter.check(addr, now + AUTH_BLOCK_DURATION_MS + 100),
            RateDecision::Allowed
        );
        // Success resets the consecutive counter.
        limiter.record_success(addr);
        limiter.record_failure(addr, now + AUTH_BLOCK_DURATION_MS + 200);
        assert_ne!(
            limiter.check(addr, now + AUTH_BLOCK_DURATION_MS + 300),
            RateDecision::Blocked
        );
    }

    #[test]
    fn other_ips_are_not_limited() {
        let mut limiter = RateLimiter::default();
        let now = 5_000;
        for i in 0..20 {
            limiter.record_failure(ip(2), now + i);
        }
        assert_eq!(limiter.check(ip(3), now + 50), RateDecision::Allowed);
    }

    #[test]
    fn parses_get_request_with_query() {
        let raw = b"GET /api/sessions/abc/output?offset=42&token=t%20ok HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer zzz\r\n\r\n";
        let (request, leftover) = read_request(&mut raw.as_slice()).expect("parse");
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/sessions/abc/output");
        assert_eq!(request.query.get("offset").unwrap(), "42");
        assert_eq!(request.query.get("token").unwrap(), "t ok");
        assert_eq!(request.headers.get("authorization").unwrap(), "Bearer zzz");
        assert!(leftover.is_empty());
    }

    #[test]
    fn parses_post_body_and_preserves_leftover() {
        let raw = b"POST /api/x HTTP/1.1\r\nContent-Length: 4\r\n\r\nbodyEXTRA";
        let (request, leftover) = read_request(&mut raw.as_slice()).expect("parse");
        assert_eq!(request.method, "POST");
        assert_eq!(request.body, b"body");
        assert_eq!(leftover, b"EXTRA");
    }

    #[test]
    fn rejects_oversized_body_declaration() {
        let raw = format!(
            "POST /api/x HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert!(read_request(&mut raw.as_bytes()).is_err());
    }

    #[test]
    fn websocket_accept_key_matches_rfc_vector() {
        // RFC 6455 section 1.3 handshake example.
        assert_eq!(
            websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn authorized_accepts_bearer_query_and_mobile_header_tokens() {
        let mut request = Request {
            method: "GET".into(),
            path: "/api/status".into(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        assert_eq!(authorized(&request, "tok"), None);
        request
            .headers
            .insert("authorization".into(), "Bearer tok".into());
        assert_eq!(authorized(&request, "tok"), Some(AuthIdentity::ServerToken));
        request
            .headers
            .insert("authorization".into(), "Bearer wrong".into());
        assert_eq!(authorized(&request, "tok"), None);
        request.headers.remove("authorization");
        request.query.insert("token".into(), "tok".into());
        assert_eq!(authorized(&request, "tok"), Some(AuthIdentity::ServerToken));
        request.query.clear();
        // The app's mobile header, bare-token form.
        request
            .headers
            .insert("x-unpeel-mobile-auth".into(), "tok".into());
        assert_eq!(authorized(&request, "tok"), Some(AuthIdentity::ServerToken));
    }

    #[test]
    fn paired_device_tokens_verify_against_the_app_store_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("devices.json");
        // Same shape MobilePairingStore persists: hex SHA-256 of the token.
        let file = json!({
            "version": 1,
            "devices": [{
                "id": "device-1",
                "name": "Tommy's iPhone",
                "platform": "ios",
                "tokenHash": sha256_hex("devtok"),
                "pairedAtUnixMs": 1,
            }],
        });
        std::fs::write(&path, file.to_string()).expect("write");

        assert_eq!(
            paired_device_for_token(&path, "devtok"),
            Some(("device-1".into(), "Tommy's iPhone".into()))
        );
        assert_eq!(paired_device_for_token(&path, "wrong"), None);
        assert_eq!(paired_device_for_token(&path, ""), None);
        assert_eq!(
            paired_device_for_token(&dir.path().join("missing.json"), "devtok"),
            None
        );

        let lease = WsAuthorizationLease::PairedDevice {
            id: "device-1".into(),
            token_hash: sha256_hex("devtok"),
        };
        assert!(websocket_lease_is_current(&path, &lease));

        // Re-pairing the stable device id rotates its token and must revoke
        // the already-upgraded socket, just like removing the record.
        let rotated = json!({
            "version": 1,
            "devices": [{
                "id": "device-1",
                "name": "Tommy's iPhone",
                "tokenHash": sha256_hex("new-token"),
            }],
        });
        std::fs::write(&path, rotated.to_string()).expect("rotate token");
        assert!(!websocket_lease_is_current(&path, &lease));
        assert!(websocket_lease_is_current(
            &dir.path().join("missing.json"),
            &WsAuthorizationLease::ServerToken
        ));
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // printf 'abc' | shasum -a 256
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn session_id_traversal_is_rejected() {
        assert!(valid_session_id("abc-123"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("../etc"));
        assert!(!valid_session_id("a/b"));
        assert!(!valid_session_id("a\\b"));
    }

    #[test]
    fn tls_material_is_generated_and_fingerprint_is_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let generated = ensure_tls_material_in(dir.path()).expect("generate");
        assert_eq!(generated.fingerprint.len(), 64);
        assert!(dir.path().join("cert.pem").exists());
        assert!(dir.path().join("key.pem").exists());
        let reloaded = ensure_tls_material_in(dir.path()).expect("reload");
        assert_eq!(generated.fingerprint, reloaded.fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("key.pem"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn audit_log_appends_json_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.log");
        let audit = AuditLog::new(path.clone());
        audit.log("test_event", json!({ "ip": "10.0.0.1" }));
        audit.log("test_event", json!({ "ip": "10.0.0.2" }));
        let raw = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(lines[0]).expect("json");
        assert_eq!(first["event"], "test_event");
        assert_eq!(first["ip"], "10.0.0.1");
        assert!(first["ts"].as_u64().unwrap() > 0);
    }

    #[test]
    fn client_registry_tracks_prunes_and_snapshots() {
        let mut registry = ClientRegistry::default();
        let now = 1_000_000;
        assert!(registry.touch_http(&ip(4), None, now));
        assert!(!registry.touch_http(&ip(4), Some("Tommy's iPhone (device-1)"), now + 1));
        let ws_id = registry.register_ws(&ip(5), None, "sess-a", now);
        let snapshot = registry.snapshot(now + 2);
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot
            .iter()
            .any(|c| c["device"] == "Tommy's iPhone (device-1)"));
        registry.remove(&ws_id);
        assert_eq!(registry.snapshot(now + 2).len(), 1);
        // Idle expiry.
        assert_eq!(registry.snapshot(now + CLIENT_IDLE_EXPIRY_MS + 1).len(), 0);
        assert!(registry.touch_http(&ip(4), None, now + CLIENT_IDLE_EXPIRY_MS + 2));
    }

    #[test]
    fn viewer_tracking_groups_by_session_and_expires_poll_viewers() {
        let mut registry = ClientRegistry::default();
        let now = 2_000_000;
        registry.register_ws(&ip(6), Some("MacBook (mac-1)"), "sess-a", now);
        registry.touch_poll_viewer(&ip(7), Some("iPhone (ios-1)"), "sess-a", now);
        registry.touch_poll_viewer(&ip(7), None, "sess-b", now);
        assert_eq!(registry.viewers_for("sess-a", now + 1).len(), 2);
        assert_eq!(registry.viewers_for("sess-b", now + 1).len(), 1);
        // Poll viewers expire quickly; the WS viewer stays.
        let later = now + POLL_VIEWER_TTL_MS + 1;
        assert_eq!(registry.viewers_for("sess-a", later).len(), 1);
        let presence = registry.presence(later);
        assert_eq!(presence["sessions"]["sess-a"].as_array().unwrap().len(), 1);
        assert!(presence["sessions"].get("sess-b").is_none());
    }

    // ---- end-to-end routing over plain TCP (handler is TLS-agnostic) ----

    struct TestServer {
        port: u16,
        ctx: Arc<ServerCtx>,
        _audit_dir: tempfile::TempDir,
    }

    fn spawn_test_server() -> TestServer {
        let audit_dir = tempfile::tempdir().expect("tempdir");
        let ctx = Arc::new(ServerCtx::new(
            "testtoken".into(),
            audit_dir.path().join("audit.log"),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let thread_ctx = Arc::clone(&ctx);
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(stream) = incoming else { continue };
                let peer = stream.peer_addr().map(|a| a.ip()).unwrap_or(ip(9));
                let ctx = Arc::clone(&thread_ctx);
                thread::spawn(move || handle_connection(stream, peer, &ctx));
            }
        });
        TestServer {
            port,
            ctx,
            _audit_dir: audit_dir,
        }
    }

    fn http_roundtrip(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        response
    }

    #[test]
    fn rejects_missing_and_wrong_tokens_and_accepts_valid() {
        let server = spawn_test_server();
        let unauth = http_roundtrip(server.port, "GET /api/status HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(unauth.starts_with("HTTP/1.1 401"), "got: {unauth}");
        let wrong = http_roundtrip(
            server.port,
            "GET /api/status HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer nope\r\n\r\n",
        );
        assert!(wrong.starts_with("HTTP/1.1 401"), "got: {wrong}");
        let ok = http_roundtrip(
            server.port,
            "GET /api/status HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n",
        );
        assert!(ok.starts_with("HTTP/1.1 200"), "got: {ok}");
        assert!(ok.contains("uptime_secs"));
        // Failed attempts were audited and rate-limit state recorded.
        let limited = {
            let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
            let mut limiter = server.ctx.limiter.lock().unwrap();
            for i in 0..AUTH_MAX_FAILS_PER_WINDOW {
                limiter.record_failure(loopback, current_timestamp_ms() + i as u64);
            }
            drop(limiter);
            http_roundtrip(
                server.port,
                "GET /api/status HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n",
            )
        };
        // Loopback shares the recorded failures -> now rate limited.
        assert!(limited.starts_with("HTTP/1.1 429"), "got: {limited}");
    }

    #[test]
    fn root_serves_placeholder_without_auth() {
        let server = spawn_test_server();
        let response = http_roundtrip(server.port, "GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        assert!(response.contains("Unpeel Remote Control"));
    }

    #[test]
    fn unknown_api_route_is_404_and_unknown_session_is_404() {
        let server = spawn_test_server();
        let missing = http_roundtrip(
            server.port,
            "GET /api/nope HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n",
        );
        assert!(missing.starts_with("HTTP/1.1 404"), "got: {missing}");
        let unknown = http_roundtrip(
            server.port,
            "GET /api/sessions/definitely-not-a-session-id-xyz HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n",
        );
        assert!(unknown.starts_with("HTTP/1.1 404"), "got: {unknown}");
        let resize = http_roundtrip(
            server.port,
            "POST /api/sessions/definitely-not-a-session-id-xyz/resize HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\nContent-Length: 22\r\n\r\n{\"cols\":80,\"rows\":24}\n",
        );
        assert!(resize.starts_with("HTTP/1.1 404"), "got: {resize}");
        let metrics = http_roundtrip(
            server.port,
            "GET /api/sessions/definitely-not-a-session-id-xyz/metrics HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n",
        );
        assert!(metrics.starts_with("HTTP/1.1 404"), "got: {metrics}");
    }

    #[test]
    fn output_poll_serves_chunk_from_session_artifacts() {
        use crate::session_host::{self as sh, HostedSessionManifest, HostedSessionState};
        use crate::state::SessionInfo;

        let session_id = format!("remote-poll-test-{}", std::process::id());
        let manifest = HostedSessionManifest {
            session: SessionInfo {
                id: session_id.clone(),
                project_id: "p1".into(),
                label: "poll test".into(),
                custom_title: false,
                command: "bash".into(),
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
            state: HostedSessionState::Exited,
            pid: None,
            pid_started_at: None,
            host_pid: None,
            host_pid_started_at: None,
            exit_code: Some(0),
            host_build_id: None,
            host_protocol_version: None,
            has_been_written_to: true,
            provider_session_id: None,
            provider_transcript_path: None,
            managed_storage_path: None,
            resume_failure_markers: Vec::new(),
            runtime: None,
            active_app: None,
            runtime_launch_generation: 0,
            runtime_launch_pending: false,
            runtime_launched_at: None,
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
        };
        sh::save_manifest(&manifest).expect("save manifest");
        std::fs::write(sh::output_path(&session_id), b"hello world").expect("write output");

        let server = spawn_test_server();
        let response = http_roundtrip(
            server.port,
            &format!(
                "GET /api/sessions/{session_id}/output?offset=0 HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n"
            ),
        );
        let shots = super::browser_artifacts_dir(&session_id).join("screenshots");
        std::fs::create_dir_all(&shots).expect("mkdir");
        std::fs::write(shots.join("page.png"), b"PNGDATA").expect("shot");
        let list = http_roundtrip(
            server.port,
            &format!("GET /api/sessions/{session_id}/artifacts/browser HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n"),
        );
        assert!(
            list.contains("page.png") && list.contains("screenshots"),
            "got: {list}"
        );
        let fetch = http_roundtrip(
            server.port,
            &format!("GET /api/sessions/{session_id}/artifacts/browser/screenshots/page.png HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n"),
        );
        assert!(
            fetch.starts_with("HTTP/1.1 200")
                && fetch.contains("image/png")
                && fetch.ends_with("PNGDATA"),
            "got: {fetch}"
        );
        let preview = http_roundtrip(
            server.port,
            &format!("GET /api/sessions/{session_id}/artifacts/browser/preview HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n"),
        );
        assert!(
            preview.starts_with("HTTP/1.1 200") && preview.contains("ETag: "),
            "got: {preview}"
        );
        let etag = preview
            .lines()
            .find(|l| l.starts_with("ETag: "))
            .unwrap()
            .trim_start_matches("ETag: ")
            .trim()
            .to_string();
        let cached = http_roundtrip(
            server.port,
            &format!("GET /api/sessions/{session_id}/artifacts/browser/preview HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\nIf-None-Match: {etag}\r\n\r\n"),
        );
        assert!(cached.starts_with("HTTP/1.1 304"), "got: {cached}");
        let traversal = http_roundtrip(
            server.port,
            &format!("GET /api/sessions/{session_id}/artifacts/browser/screenshots/..%2Fmanifest.json HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer testtoken\r\n\r\n"),
        );
        assert!(traversal.starts_with("HTTP/1.1 404"), "got: {traversal}");
        sh::cleanup_session_artifacts(&session_id).expect("cleanup");

        assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
        let body = response.split("\r\n\r\n").nth(1).expect("body");
        let parsed: Value = serde_json::from_str(body).expect("json");
        let decoded = {
            use base64::engine::general_purpose::STANDARD;
            use base64::Engine;
            STANDARD
                .decode(parsed["data_base64"].as_str().expect("data"))
                .expect("base64")
        };
        assert_eq!(decoded, b"hello world");
        assert_eq!(parsed["next_offset"], 11);
        assert_eq!(parsed["exited"], true);
    }

    #[test]
    fn ws_frames_roundtrip_masked_client_frame() {
        // Build a masked client close frame and parse it via PrefixedStream.
        let payload = b"bye".to_vec();
        let mask = [1u8, 2, 3, 4];
        let mut frame = vec![0x88, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i % 4]);
        }
        // Server never reads the TcpStream side; the frame arrives via prefix.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let _client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        let (server_side, _) = listener.accept().expect("accept");
        let mut stream = PrefixedStream::new(frame, server_side);
        let (opcode, parsed) = read_ws_frame(&mut stream).expect("frame").expect("some");
        assert_eq!(opcode, 0x8);
        assert_eq!(parsed, payload);
    }

    #[test]
    fn ws_frame_write_uses_extended_length() {
        let mut out = Vec::new();
        write_ws_frame(&mut out, 0x2, &[7u8; 200]).expect("write");
        assert_eq!(out[0], 0x82);
        assert_eq!(out[1], 126);
        assert_eq!(u16::from_be_bytes([out[2], out[3]]), 200);
        assert_eq!(out.len(), 4 + 200);
    }

    #[test]
    fn ws_replay_plan_resumes_rebases_and_bounds() {
        // No offset: fresh tail, not a rebase.
        assert_eq!(
            plan_ws_replay(None, 5_000, 0),
            WsReplayPlan {
                explicit_start: None,
                rebased: false
            }
        );
        // Held offset within the resume bound: replay from it exactly.
        assert_eq!(
            plan_ws_replay(Some(4_000), 5_000, 0),
            WsReplayPlan {
                explicit_start: Some(4_000),
                rebased: false
            }
        );
        // Exactly at the bound is still a resume.
        let len = WS_RESUME_REPLAY_MAX_BYTES + 100;
        assert_eq!(
            plan_ws_replay(Some(100), len, 0),
            WsReplayPlan {
                explicit_start: Some(100),
                rebased: false
            }
        );
        // One past the bound: rebase.
        assert_eq!(
            plan_ws_replay(Some(99), len, 0),
            WsReplayPlan {
                explicit_start: None,
                rebased: true
            }
        );
        // Offset beyond the file (stale/rotated): rebase.
        assert_eq!(
            plan_ws_replay(Some(6_000), 5_000, 0),
            WsReplayPlan {
                explicit_start: None,
                rebased: true
            }
        );
        // Offset == file length: valid live-tail resume, nothing to replay.
        assert_eq!(
            plan_ws_replay(Some(5_000), 5_000, 0),
            WsReplayPlan {
                explicit_start: Some(5_000),
                rebased: false
            }
        );
        // A cursor still numerically inside the logical file can nevertheless
        // refer to physically evicted history in the sparse journal.
        assert_eq!(
            plan_ws_replay(Some(2_000), 5_000, 3_000),
            WsReplayPlan {
                explicit_start: None,
                rebased: true
            }
        );
    }

    #[test]
    fn output_frame_carries_big_endian_offset_prefix() {
        let frame = encode_output_frame(0x0102_0304_0506_0708, b"abc");
        assert_eq!(frame.len(), 11);
        assert_eq!(
            u64::from_be_bytes(frame[..8].try_into().unwrap()),
            0x0102_0304_0506_0708
        );
        assert_eq!(&frame[8..], b"abc");
        let empty = encode_output_frame(7, b"");
        assert_eq!(empty, 7u64.to_be_bytes());
    }

    #[test]
    fn close_payload_is_code_plus_reason() {
        let payload = ws_close_payload(1000, "session exited");
        assert_eq!(u16::from_be_bytes([payload[0], payload[1]]), 1000);
        assert_eq!(&payload[2..], b"session exited");
        assert_eq!(ws_close_payload(1001, ""), 1001u16.to_be_bytes());
    }

    #[test]
    fn parses_ws_client_messages_and_rejects_bad_ones() {
        assert_eq!(
            parse_ws_client_message(br#"{"type":"input","data":"ls\r"}"#),
            Ok(WsClientMessage::Input {
                data: "ls\r".into(),
                write_id: None
            })
        );
        assert_eq!(
            parse_ws_client_message(br#"{"type":"input","data":"ls\r","wid":"abc123"}"#),
            Ok(WsClientMessage::Input {
                data: "ls\r".into(),
                write_id: Some("abc123".into())
            })
        );
        assert_eq!(
            parse_ws_client_message(br#"{"type":"resize","cols":81,"rows":24}"#),
            Ok(WsClientMessage::Resize { cols: 81, rows: 24 })
        );
        assert!(parse_ws_client_message(b"not json").is_err());
        assert!(parse_ws_client_message(br#"{"type":"input"}"#).is_err());
        assert!(parse_ws_client_message(br#"{"type":"input","data":""}"#).is_err());
        assert!(parse_ws_client_message(br#"{"type":"resize","cols":1,"rows":24}"#).is_err());
        assert!(parse_ws_client_message(br#"{"type":"resize","cols":80,"rows":2000}"#).is_err());
        assert!(parse_ws_client_message(br#"{"type":"launch-missiles"}"#).is_err());
        assert!(parse_ws_client_message(br#"{"data":"x"}"#).is_err());
    }

    /// Read one unmasked server frame off a raw TCP stream (test client side).
    fn read_server_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut header = [0u8; 2];
        stream.read_exact(&mut header).expect("frame header");
        let opcode = header[0] & 0x0f;
        let mut len = (header[1] & 0x7f) as u64;
        if len == 126 {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext).expect("len16");
            len = u16::from_be_bytes(ext) as u64;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            stream.read_exact(&mut ext).expect("len64");
            len = u64::from_be_bytes(ext);
        }
        let mut payload = vec![0u8; len as usize];
        stream.read_exact(&mut payload).expect("payload");
        (opcode, payload)
    }

    /// A Running manifest (live pid) for WebSocket tests. Whether its
    /// control socket exists decides between "host gone" and a fake host.
    #[cfg(unix)]
    fn running_ws_test_manifest(session_id: &str) -> crate::session_host::HostedSessionManifest {
        use crate::session_host::{HostedSessionManifest, HostedSessionState};
        use crate::state::SessionInfo;
        HostedSessionManifest {
            session: SessionInfo {
                id: session_id.to_string(),
                project_id: "p1".into(),
                label: "ws test".into(),
                custom_title: false,
                command: "bash".into(),
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
            // Running (with a live pid) so the upgrade path accepts it; the
            // pump then fails to reach the (nonexistent) control socket and
            // must close the stream cleanly after the disk replay.
            state: HostedSessionState::Running,
            pid: Some(std::process::id()),
            pid_started_at: None,
            host_pid: None,
            host_pid_started_at: None,
            exit_code: None,
            host_build_id: None,
            host_protocol_version: None,
            has_been_written_to: true,
            provider_session_id: None,
            provider_transcript_path: None,
            managed_storage_path: None,
            resume_failure_markers: Vec::new(),
            runtime: None,
            active_app: None,
            runtime_launch_generation: 0,
            runtime_launch_pending: false,
            runtime_launched_at: None,
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
            heartbeat_at: current_timestamp_ms(),
            updated_at: current_timestamp_ms(),
        }
    }

    /// Fake `session.sock`: answers `snapshot` with a header line + raw bytes
    /// at `journal_offset`, `viewport_snapshot` with a minimal grid, and
    /// `stream_output` with one live frame continuing from the requested
    /// offset followed by EOF (which the pump reports as a clean close).
    #[cfg(unix)]
    fn spawn_fake_session_socket(
        session_id: &str,
        snapshot_bytes: &'static [u8],
        journal_offset: u64,
        live: &'static [u8],
    ) -> Arc<Mutex<Vec<String>>> {
        use std::io::BufRead;
        use std::os::unix::net::UnixListener;
        let path = session_host::socket_path(session_id);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind fake session socket");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { break };
                let mut reader = std::io::BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let request: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
                record.lock().unwrap().push(line.trim().to_string());
                match request["type"].as_str() {
                    Some("snapshot") => {
                        let header = json!({"ok": true, "snapshot": {
                            "journal_offset": journal_offset, "cols": 80, "rows": 24,
                            "bytes_len": snapshot_bytes.len()}});
                        let _ = conn.write_all(header.to_string().as_bytes());
                        let _ = conn.write_all(b"\n");
                        let _ = conn.write_all(snapshot_bytes);
                    }
                    Some("stream_output") => {
                        let offset = request["offset"].as_u64().unwrap_or(0);
                        let mut frame = Vec::new();
                        frame.extend_from_slice(&(live.len() as u32).to_be_bytes());
                        frame.extend_from_slice(&(offset + live.len() as u64).to_be_bytes());
                        frame.push(0b10);
                        frame.extend_from_slice(live);
                        let _ = conn.write_all(&frame);
                    }
                    _ => {
                        let _ = conn.write_all(b"{\"ok\":false,\"error\":\"unsupported\"}\n");
                    }
                }
            }
        });
        seen
    }

    #[cfg(unix)]
    fn ws_upgrade(stream: &mut TcpStream, session_id: &str, query: &str) -> String {
        let upgrade = format!(
            "GET /api/sessions/{session_id}/output?{query}&token=testtoken HTTP/1.1\r\n\
             Host: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(upgrade.as_bytes()).expect("send upgrade");
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).expect("read status");
            head.push(byte[0]);
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    /// `?snapshot=1` against a live Host: hello starts at the snapshot's
    /// journal offset, exactly one baseline text frame carries the VT bytes,
    /// and binary frames begin at that offset with no journal bytes below it.
    #[cfg(unix)]
    #[test]
    fn ws_snapshot_capable_client_gets_baseline_then_stream_from_its_offset() {
        use crate::session_host as sh;
        let session_id = format!("remote-ws-base-{}", std::process::id());
        sh::save_manifest(&running_ws_test_manifest(&session_id)).expect("save manifest");
        // 20 bytes of journal: the snapshot covers the first 12, so only
        // "-after-snap" (offset 12..20) may reach the client as journal.
        std::fs::write(sh::output_path(&session_id), b"before-snap!-after-snap").expect("write");
        let seen = spawn_fake_session_socket(
            &session_id,
            b"\x1b[?1049h\x1b[3;4HFRAME\x1b[5;1H",
            12,
            b"LIVE",
        );

        let server = spawn_test_server();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
        let head = ws_upgrade(&mut stream, &session_id, "snapshot=1");
        assert!(head.starts_with("HTTP/1.1 101"), "got: {head}");

        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x1);
        let hello: Value = serde_json::from_slice(&payload).expect("hello json");
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["snapshot"], true);
        assert_eq!(hello["start_offset"], 12);
        assert_eq!(hello["rebased"], false);
        assert!(
            hello.get("mode_preamble_base64").is_none(),
            "snapshot carries modes: {hello}"
        );

        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x1, "baseline is a text frame");
        let baseline: Value = serde_json::from_slice(&payload).expect("baseline json");
        assert_eq!(baseline["type"], "baseline");
        assert_eq!(baseline["journal_offset"], 12);
        assert_eq!(baseline["cols"], 80);
        assert_eq!(baseline["rows"], 24);
        {
            use base64::engine::general_purpose::STANDARD;
            use base64::Engine;
            let bytes = STANDARD
                .decode(baseline["bytes_base64"].as_str().unwrap())
                .expect("base64");
            assert_eq!(bytes, b"\x1b[?1049h\x1b[3;4HFRAME\x1b[5;1H");
        }

        // Journal catch-up from the snapshot offset, then the live frame.
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x2);
        assert_eq!(u64::from_be_bytes(payload[..8].try_into().unwrap()), 12);
        assert_eq!(&payload[8..], b"-after-snap");
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x2);
        assert_eq!(u64::from_be_bytes(payload[..8].try_into().unwrap()), 23);
        assert_eq!(&payload[8..], b"LIVE");
        let (opcode, _) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x8, "fake host EOF closes the stream");

        let seen = seen.lock().unwrap().clone();
        assert!(
            seen.iter().any(|line| line.contains("\"snapshot\"")),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|line| line.contains("stream_output") && line.contains("\"offset\":23")),
            "live subscribe must start at the journal tail after catch-up: {seen:?}"
        );
        sh::cleanup_session_artifacts(&session_id).expect("cleanup");
    }

    /// Without `?snapshot=1` the same live Host yields the raw tail path
    /// byte-for-byte: no `snapshot` key, no baseline frame, journal from the
    /// aligned tail start, and no snapshot request on the control socket.
    #[cfg(unix)]
    #[test]
    fn ws_client_without_capability_keeps_raw_tail_replay() {
        use crate::session_host as sh;
        let session_id = format!("remote-ws-raw-{}", std::process::id());
        sh::save_manifest(&running_ws_test_manifest(&session_id)).expect("save manifest");
        std::fs::write(sh::output_path(&session_id), b"before-snap!-after-snap").expect("write");
        let seen = spawn_fake_session_socket(&session_id, b"SNAP", 12, b"LIVE");

        let server = spawn_test_server();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
        let head = ws_upgrade(&mut stream, &session_id, "x=1");
        assert!(head.starts_with("HTTP/1.1 101"), "got: {head}");

        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x1);
        let hello: Value = serde_json::from_slice(&payload).expect("hello json");
        assert!(hello.get("snapshot").is_none(), "{hello}");
        assert_eq!(hello["start_offset"], 0);

        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x2, "raw path: first frame is journal, no baseline");
        assert_eq!(u64::from_be_bytes(payload[..8].try_into().unwrap()), 0);
        assert_eq!(&payload[8..], b"before-snap!-after-snap");
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x2);
        assert_eq!(&payload[8..], b"LIVE");

        let seen = seen.lock().unwrap().clone();
        assert!(
            !seen.iter().any(|line| line.contains("\"snapshot\"")),
            "{seen:?}"
        );
        sh::cleanup_session_artifacts(&session_id).expect("cleanup");
    }

    /// `?snapshot=1` against a Host that cannot answer (socket gone) reports
    /// `snapshot: false` and degrades to the raw tail path.
    #[cfg(unix)]
    #[test]
    fn ws_snapshot_capable_client_degrades_to_raw_tail_without_a_host() {
        use crate::session_host as sh;
        let session_id = format!("remote-ws-degrade-{}", std::process::id());
        sh::save_manifest(&running_ws_test_manifest(&session_id)).expect("save manifest");
        std::fs::write(sh::output_path(&session_id), b"hello websocket").expect("write");

        let server = spawn_test_server();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
        let head = ws_upgrade(&mut stream, &session_id, "snapshot=1");
        assert!(head.starts_with("HTTP/1.1 101"), "got: {head}");

        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x1);
        let hello: Value = serde_json::from_slice(&payload).expect("hello json");
        assert_eq!(hello["snapshot"], false);
        assert_eq!(hello["start_offset"], 0);
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x2, "no baseline frame; raw journal tail");
        assert_eq!(&payload[8..], b"hello websocket");
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x8);
        assert_eq!(u16::from_be_bytes([payload[0], payload[1]]), 1011);
        sh::cleanup_session_artifacts(&session_id).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn ws_output_stream_sends_hello_framed_replay_and_close() {
        use crate::session_host as sh;

        let session_id = format!("remote-ws-test-{}", std::process::id());
        let manifest = running_ws_test_manifest(&session_id);
        sh::save_manifest(&manifest).expect("save manifest");
        std::fs::write(sh::output_path(&session_id), b"hello websocket").expect("write output");

        let server = spawn_test_server();
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
        let upgrade = format!(
            "GET /api/sessions/{session_id}/output?offset=0&token=testtoken HTTP/1.1\r\n\
             Host: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(upgrade.as_bytes()).expect("send upgrade");

        // Read the 101 response headers byte by byte (frames follow at once).
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).expect("read status");
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        assert!(head.starts_with("HTTP/1.1 101"), "got: {head}");
        assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));

        // 1) hello text frame with stream metadata.
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x1, "expected hello text frame");
        let hello: Value = serde_json::from_slice(&payload).expect("hello json");
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["protocol"], 1);
        assert_eq!(hello["session_id"], session_id.as_str());
        assert_eq!(hello["requested_offset"], 0);
        assert_eq!(hello["start_offset"], 0);
        assert_eq!(hello["rebased"], false);
        assert_eq!(hello["output_size"], 15);
        assert!(hello["cols"].is_null(), "no live host: grid unknown");

        // 2) binary output frame: 8-byte BE offset prefix + raw bytes.
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x2, "expected binary output frame");
        assert_eq!(u64::from_be_bytes(payload[..8].try_into().unwrap()), 0);
        assert_eq!(&payload[8..], b"hello websocket");

        // 3) clean close frame (1011: the session host socket is gone).
        let (opcode, payload) = read_server_frame(&mut stream);
        assert_eq!(opcode, 0x8, "expected close frame");
        assert_eq!(u16::from_be_bytes([payload[0], payload[1]]), 1011);

        sh::cleanup_session_artifacts(&session_id).expect("cleanup");
    }
}
