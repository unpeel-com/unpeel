//! App-less `/mobile/*` server: the same wire contract as the native
//! `MobileRemoteServer.swift`, so an already-paired phone keeps working when
//! only the TUI runs. HTTP/1.1 over TLS with the Host certificate (the same
//! pinned certificate the `__remote__` WSS streamer serves), Bearer auth
//! against the shared `~/.unpeel/mobile/devices.json` token hashes, JSON keys
//! in the Swift dialect (camelCase with capital-ID suffixes, optionals
//! omitted).
//!
//! Transport rule: one port, TLS by default, plaintext tolerated only for the
//! pairing exchange, which is sealed at the application layer. A connection is
//! classified by its first byte (`0x16` is a TLS ClientHello). A plaintext
//! request that presents any credential is refused with `426 Upgrade Required`
//! and `"use https"` *before* the token is looked up: a paired device's
//! long-lived bearer never authenticates over a cleartext connection.
//!
//! Single-owner rule: the exact persisted listener is the phone + Link lease.
//! A TUI yields that lease only to a validated native sidebar, then retries it
//! when the native frontend disappears; concurrent TUIs cannot bind it twice.
//! The native app remains the platform owner for APNs when it runs.
//! Pairing and approvals work app-lessly. Artifacts, resumable uploads, and
//! session creation use the shared Host contract; bootstrap advertises the
//! exact supported subset through the versioned capability descriptor. Push
//! tokens and Relay credential rotation remain platform-owned, but their
//! public routes live here and delegate through a live native adapter.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::FromRawFd;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use unpeel_core::app_paths;
use unpeel_core::controller_api::{
    ControllerEffects, ControllerPrincipal, ControllerRequest, HostBootstrapContext,
    HostCreateContext, HostCreateProject, HostRouteContext,
};
use unpeel_core::rustls;

use crate::platform_adapter::{PlatformAdapterError, PlatformAdapterHub};

const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// How a connection reached the listener, decided from its first byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport {
    /// A TLS ClientHello: served over the Host certificate.
    Tls,
    /// Cleartext HTTP/1.1: only the sealed pairing exchange belongs here.
    Plaintext,
}

/// The first byte of a TLS record is its content type; a ClientHello is a
/// handshake record (`0x16`). No HTTP method token starts with that byte.
pub(crate) fn transport_for_first_byte(first: u8) -> Transport {
    if first == 0x16 {
        Transport::Tls
    } else {
        Transport::Plaintext
    }
}

/// The plaintext gate, evaluated before any credential is looked up. A
/// cleartext request that presents an `Authorization` header ends here with
/// `426 Upgrade Required` and a body naming the fix; the token is never
/// hashed, compared, or authenticated. TLS requests pass through untouched.
pub(crate) fn plaintext_credential_rejection(
    transport: Transport,
    request: &Request,
) -> Option<(u16, String)> {
    if transport == Transport::Tls || !request.headers.contains_key("authorization") {
        return None;
    }
    Some((
        426,
        serde_json::json!({
            "error": "use https",
            "detail": "the direct /mobile endpoint serves TLS with the Host \
                       certificate; a bearer token is never accepted over \
                       plaintext",
        })
        .to_string(),
    ))
}

/// One accepted socket, wrapped for its transport. Both arms are blocking
/// `std` streams, so the request loop is transport-agnostic.
enum MobileStream {
    Plaintext(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Read for MobileStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            MobileStream::Plaintext(stream) => stream.read(buf),
            MobileStream::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for MobileStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            MobileStream::Plaintext(stream) => stream.write(buf),
            MobileStream::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            MobileStream::Plaintext(stream) => stream.flush(),
            MobileStream::Tls(stream) => stream.flush(),
        }
    }
}

/// Classify an accepted socket by peeking (`MSG_PEEK`) its first byte, so the
/// TLS layer still reads the whole ClientHello itself. `None` when the peer
/// hung up or the read timeout elapsed before sending anything.
fn accept_transport(
    stream: TcpStream,
    tls: &Arc<rustls::ServerConfig>,
) -> Option<(Transport, MobileStream)> {
    let mut first = [0u8; 1];
    if !matches!(stream.peek(&mut first), Ok(1)) {
        return None;
    }
    match transport_for_first_byte(first[0]) {
        Transport::Tls => {
            let connection = rustls::ServerConnection::new(Arc::clone(tls)).ok()?;
            Some((
                Transport::Tls,
                MobileStream::Tls(Box::new(rustls::StreamOwned::new(connection, stream))),
            ))
        }
        Transport::Plaintext => Some((Transport::Plaintext, MobileStream::Plaintext(stream))),
    }
}

/// The Host certificate this listener serves, as loaded by `start_impl`.
/// Bootstrap and the sealed pairing response advertise it so a Controller
/// pins one fingerprint for `/mobile` and the WSS streamer alike.
static DIRECT_CERTIFICATE_FINGERPRINT: std::sync::OnceLock<Mutex<Option<String>>> =
    std::sync::OnceLock::new();

fn remember_direct_certificate_fingerprint(fingerprint: &str) {
    if let Ok(mut guard) = DIRECT_CERTIFICATE_FINGERPRINT
        .get_or_init(|| Mutex::new(None))
        .lock()
    {
        *guard = Some(fingerprint.to_owned());
    }
}

/// Lowercase hex SHA-256 of the certificate the direct listener serves, once
/// a listener has started in this process.
pub(crate) fn direct_certificate_fingerprint() -> Option<String> {
    DIRECT_CERTIFICATE_FINGERPRINT
        .get()
        .and_then(|cell| cell.lock().ok())
        .and_then(|guard| guard.clone())
}

/// Load the Host certificate and build the listener's TLS config. Failing
/// closed here (no listener) beats a cleartext-only listener that would refuse
/// every paired device anyway.
fn direct_tls_config() -> Option<(Arc<rustls::ServerConfig>, String)> {
    let material = match unpeel_core::remote_server::ensure_tls_material() {
        Ok(material) => material,
        Err(error) => {
            crate::tracelog::trace("mobile-tls", &format!("certificate unavailable: {error}"));
            return None;
        }
    };
    let fingerprint = material.fingerprint.clone();
    match unpeel_core::remote_server::build_tls_config(material) {
        Ok(config) => Some((config, fingerprint)),
        Err(error) => {
            crate::tracelog::trace("mobile-tls", &format!("tls config failed: {error}"));
            None
        }
    }
}
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Wall-clock budget for a connection to authenticate, measured from accept.
/// The per-read `IO_TIMEOUT` alone lets a slow sender hold a worker thread
/// indefinitely by pacing one byte per read; the accept loop shuts down any
/// connection still unauthenticated past this deadline, which interrupts
/// the worker's blocking read regardless of how the bytes were paced.
const PRE_AUTH_DEADLINE: Duration = Duration::from_secs(5);
/// Ceiling on concurrently served connections. The excess is answered 503
/// and closed on the accept thread, so it never earns a worker thread.
const MAX_CONNECTIONS: usize = 64;
/// Per-peer-address share of `MAX_CONNECTIONS`, so one client (a phone opens
/// a handful of URLSession connections, never more) cannot fill the pool.
const MAX_CONNECTIONS_PER_PEER: usize = 16;
/// How often the accept loop looks for expired unauthenticated connections.
const LEDGER_SWEEP_INTERVAL: Duration = Duration::from_millis(100);
/// The TUI main loop publishes the phone-facing snapshot here every rescan:
/// pre-built bootstrap arrays plus the project-scoped archive catalog, all in
/// the Swift wire dialect and desktop sidebar order.
pub type SharedSnapshot = Arc<Mutex<crate::sessions::MobileSnapshot>>;
/// session id → when a phone last resized it via this server (drives the
/// "Resized for mobile" tag in the TUI preview).
pub type MobileResizes = Arc<Mutex<HashMap<String, Instant>>>;
type ProjectColorWriter<'a> = &'a dyn Fn(&str, Option<&str>) -> Result<(), String>;

fn valid_native_artifact_chunk(value: &serde_json::Value, query: &HashMap<String, String>) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    const KEYS: [&str; 9] = [
        "sessionID",
        "kind",
        "name",
        "contentType",
        "offset",
        "nextOffset",
        "totalSize",
        "dataBase64",
        "capturedAtUnixMs",
    ];
    if object.len() != KEYS.len() || object.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return false;
    }
    let expected_session = query
        .get("session_id")
        .or_else(|| query.get("sessionID"))
        .map(|value| value.trim());
    let expected_kind = query.get("kind").map(|value| value.trim());
    let expected_name = query.get("name").map(|value| value.trim());
    if object.get("sessionID").and_then(serde_json::Value::as_str) != expected_session
        || object.get("kind").and_then(serde_json::Value::as_str) != expected_kind
        || object.get("name").and_then(serde_json::Value::as_str) != expected_name
        || object
            .get("contentType")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
        || object
            .get("capturedAtUnixMs")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|value| value == 0)
    {
        return false;
    }
    let Some(offset) = object.get("offset").and_then(serde_json::Value::as_u64) else {
        return false;
    };
    let Some(next_offset) = object.get("nextOffset").and_then(serde_json::Value::as_u64) else {
        return false;
    };
    let Some(total_size) = object.get("totalSize").and_then(serde_json::Value::as_u64) else {
        return false;
    };
    let Some(encoded) = object.get("dataBase64").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let max_bytes = query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(unpeel_core::session_artifacts::ARTIFACT_READ_MAX_CHUNK_BYTES)
        .clamp(
            1,
            unpeel_core::session_artifacts::ARTIFACT_READ_MAX_CHUNK_BYTES,
        );
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    offset <= next_offset
        && next_offset <= total_size
        && bytes.len() <= max_bytes
        && next_offset.saturating_sub(offset) == bytes.len() as u64
}

pub struct MobileServer {
    pub port: u16,
    /// Lowercase hex SHA-256 of the Host certificate this listener serves;
    /// the pin a Controller holds for `/mobile` and the WSS streamer alike.
    pub certificate_fingerprint: String,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    bonjour: Arc<Mutex<Option<std::process::Child>>>,
    remote: Arc<Mutex<crate::remote_streamer::RemoteStreamer>>,
    accept_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    active_connections: Arc<Mutex<HashMap<u64, TcpStream>>>,
    worker_threads: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

impl MobileServer {
    /// The listener is the serving lease, while the persisted endpoint is
    /// the handoff rendezvous. If another frontend publishes a different
    /// endpoint, this server must retire before it can own Link or mutate
    /// shared authorization state.
    pub fn owns_configured_endpoint(&self) -> bool {
        configured_server_port_at(&mobile_dir()) == Some(self.port)
    }

    /// Released native builds can fall back to a random port when this TUI
    /// owns the saved endpoint, then overwrite `server-port`. The paired phone
    /// will not adopt that unauthenticated replacement. While legacy native
    /// owns Link, keep this listener serving Direct and repair its rendezvous
    /// under the lock shared with capability-aware native/TUI claimers.
    pub fn restore_legacy_configured_endpoint(&self) -> bool {
        restore_server_port_at(&mobile_dir(), self.port)
    }

    /// Stand down: stop accepting, kill the Bonjour advertisement. Called
    /// the moment the app becomes reachable again (it owns the phone
    /// endpoint) and on TUI exit.
    pub fn stop(&self) {
        if self
            .shutdown
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        // Retire the shared claim before releasing the socket. Contenders
        // still fail the exact bind until the accept loop exits, but once a
        // successor publishes the same port this older owner must never
        // compare-delete the successor's lease.
        clear_tui_owner_port_at(&mobile_dir(), self.port);
        if let Ok(mut guard) = self.bonjour.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if let Ok(mut streamer) = self.remote.lock() {
            streamer.stop();
        }
        // `pair --serve` hands this exact endpoint to the interactive TUI.
        // Wait for the accept loop to drop its listener before the next
        // server tries to bind the same port; otherwise the Controller's
        // freshly paired endpoint can be stale before its first bootstrap.
        if let Ok(mut guard) = self.accept_thread.lock() {
            if let Some(thread) = guard.take() {
                let _ = thread.join();
            }
        }
        // The accept loop is now gone, so this set cannot grow. Interrupt
        // keep-alive reads/writes before joining every worker; otherwise a
        // polite app↔TUI takeover can retain the old endpoint for 30 seconds.
        if let Ok(connections) = self.active_connections.lock() {
            for stream in connections.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        if let Ok(mut workers) = self.worker_threads.lock() {
            for worker in workers.drain(..) {
                let _ = worker.join();
            }
        }
    }
}

impl MobileServer {
    /// One supervision step for the worker-owned `__remote__` streamer:
    /// reap an exit, respawn with backoff, or hold after a crash loop.
    pub fn supervise_streamer(&self) -> Vec<crate::remote_streamer::StreamerEvent> {
        self.remote
            .lock()
            .map(|mut streamer| streamer.poll())
            .unwrap_or_default()
    }

    /// A pairing change is the user actively trying: lift a crash-loop hold
    /// and respawn immediately if no streamer is running.
    pub fn retry_streamer_after_pairing_change(
        &self,
    ) -> Vec<crate::remote_streamer::StreamerEvent> {
        self.remote
            .lock()
            .map(|mut streamer| streamer.retry_after_pairing_change())
            .unwrap_or_default()
    }

    pub fn streamer_status(&self) -> Option<crate::remote_streamer::StreamerStatus> {
        self.remote.lock().ok().map(|streamer| streamer.status())
    }
}

impl Drop for MobileServer {
    fn drop(&mut self) {
        // Pairing/setup can fail after the listener and helper processes are
        // live. Keep every exceptional return on the same cleanup path as an
        // ordinary TUI hand-back or exit.
        self.stop();
    }
}

/// One-process endpoint handoff used by `unpeel pair --serve`.
///
/// This is deliberately not Bonjour rediscovery: the paired Controller must
/// never send its long-lived bearer token to a plaintext candidate based only
/// on an unauthenticated TXT record. The pairing listener records its exact
/// port here, releases it, and the interactive TUI binds that same port before
/// serving the newly paired Controller.
static NEXT_START_PORT: std::sync::OnceLock<Mutex<Option<u16>>> = std::sync::OnceLock::new();

fn next_start_port() -> &'static Mutex<Option<u16>> {
    NEXT_START_PORT.get_or_init(|| Mutex::new(None))
}

pub fn remember_paired_port(port: u16, hand_off_to_tui: bool) {
    if port == 0 {
        return;
    }
    // Keep a headless Host reachable at the endpoint sealed into the pairing
    // response across later TUI restarts. `server-port` is the canonical
    // app/TUI handoff endpoint: establish it only when absent, never overwrite
    // the native app's existing choice. The headless fallback records the
    // paired port separately for installations that already have another
    // native endpoint.
    persist_paired_port_at(&mobile_dir(), port);
    if hand_off_to_tui {
        if let Ok(mut guard) = next_start_port().lock() {
            *guard = Some(port);
        }
    }
}

fn persist_paired_port_at(dir: &std::path::Path, port: u16) {
    if port == 0 || std::fs::create_dir_all(dir).is_err() {
        return;
    }
    {
        if let Ok(mut canonical) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join("server-port"))
        {
            let _ = canonical.write_all(format!("{port}\n").as_bytes());
        }
        let _ = std::fs::write(dir.join("headless-server-port"), format!("{port}\n"));
    }
}

fn read_port(path: &std::path::Path) -> Option<u16> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|port| *port > 0)
}

fn configured_server_port_at(dir: &std::path::Path) -> Option<u16> {
    read_port(&dir.join("server-port")).or_else(|| read_port(&dir.join("headless-server-port")))
}

fn atomic_write_server_port_at(dir: &std::path::Path, port: u16) -> bool {
    let temporary = dir.join(format!(
        ".server-port.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(format!("{port}\n").as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, dir.join("server-port"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.is_ok()
}

fn atomic_write_tui_owner_port_at(dir: &std::path::Path, port: u16) -> bool {
    let temporary = dir.join(format!(
        ".tui-server-port.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(format!("{port}\n").as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, dir.join("tui-server-port"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.is_ok()
}

fn publish_tui_owner_port_at(dir: &std::path::Path, port: u16) -> bool {
    if port == 0 || std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let lock_path = dir.join("server-port.lock");
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
    else {
        return false;
    };
    #[cfg(unix)]
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
        return false;
    }
    let published = atomic_write_tui_owner_port_at(dir, port);
    #[cfg(unix)]
    unsafe {
        libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN);
    }
    published
}

fn clear_tui_owner_port_at(dir: &std::path::Path, port: u16) {
    let lock_path = dir.join("server-port.lock");
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
    else {
        return;
    };
    #[cfg(unix)]
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
        return;
    }
    if read_port(&dir.join("tui-server-port")) == Some(port) {
        let _ = std::fs::remove_file(dir.join("tui-server-port"));
    }
    #[cfg(unix)]
    unsafe {
        libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN);
    }
}

/// Publish a first endpoint without letting two concurrently-starting TUIs
/// both believe their OS-assigned listener won. The socket is already bound
/// when this runs; the file lock chooses one durable winner, and the rename
/// makes readers see either the old file or the complete new value.
fn claim_initial_server_port_at(dir: &std::path::Path, port: u16) -> bool {
    if port == 0 || std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let lock_path = dir.join("server-port.lock");
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
    else {
        return false;
    };
    #[cfg(unix)]
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
        return false;
    }

    let claimed = match configured_server_port_at(dir) {
        Some(existing) => existing == port,
        None => atomic_write_server_port_at(dir, port),
    };
    #[cfg(unix)]
    unsafe {
        libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN);
    }
    claimed
}

fn restore_server_port_at(dir: &std::path::Path, port: u16) -> bool {
    if port == 0 || std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let lock_path = dir.join("server-port.lock");
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
    else {
        return false;
    };
    #[cfg(unix)]
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX) } != 0 {
        return false;
    }
    let restored =
        read_port(&dir.join("server-port")) == Some(port) || atomic_write_server_port_at(dir, port);
    #[cfg(unix)]
    unsafe {
        libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN);
    }
    restored
}

pub fn canonical_server_port() -> Option<u16> {
    read_port(&mobile_dir().join("server-port"))
}

pub(crate) fn configured_server_port() -> Option<u16> {
    configured_server_port_at(&mobile_dir())
}

pub fn local_endpoint_is_listening(port: u16) -> bool {
    port > 0
        && TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(100),
        )
        .is_ok()
}

/// Bind the shared IPv4 endpoint with the same pre-bind reuse policy as the
/// native Swift server. This is required for an immediate pair→TUI or
/// app→TUI handoff after an accepted socket enters TIME_WAIT.
fn bind_reusable_ipv4_listener(port: u16) -> Option<TcpListener> {
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return None;
        }
        let close_on_error = || {
            libc::close(fd);
            None
        };
        let enabled: libc::c_int = 1;
        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &enabled as *const _ as *const libc::c_void,
            std::mem::size_of_val(&enabled) as libc::socklen_t,
        ) != 0
        {
            return close_on_error();
        }
        let descriptor_flags = libc::fcntl(fd, libc::F_GETFD);
        if descriptor_flags < 0
            || libc::fcntl(fd, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) != 0
        {
            return close_on_error();
        }
        let mut address: libc::sockaddr_in = std::mem::zeroed();
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        ))]
        {
            address.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        }
        address.sin_family = libc::AF_INET as libc::sa_family_t;
        address.sin_port = port.to_be();
        address.sin_addr = libc::in_addr {
            s_addr: libc::INADDR_ANY,
        };
        if libc::bind(
            fd,
            &address as *const _ as *const libc::sockaddr,
            std::mem::size_of_val(&address) as libc::socklen_t,
        ) != 0
            || libc::listen(fd, 128) != 0
        {
            return close_on_error();
        }
        Some(TcpListener::from_raw_fd(fd))
    }
}

fn bind_mobile_listener(
    handoff: Option<u16>,
    persisted: Option<u16>,
    headless_persisted: Option<u16>,
) -> Option<TcpListener> {
    if let Some(port) = handoff.or(persisted).or(headless_persisted) {
        // Exact or nothing: paired Controllers persist this endpoint and do
        // not trust Bonjour to adopt a replacement. EADDRINUSE means another
        // frontend still owns serving; the TUI retries after it releases the
        // socket instead of becoming an unreachable second owner.
        return bind_reusable_ipv4_listener(port);
    }
    bind_reusable_ipv4_listener(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn mobile_dir() -> std::path::PathBuf {
    app_paths::unpeel_home().join("mobile")
}

fn sha256_hex(token: &str) -> String {
    // Minimal SHA-256 (FIPS 180-4). Local, dependency-free; auth compares
    // against lowercase-hex tokenHash values in devices.json.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bytes = token.as_bytes();
    let bit_len = (bytes.len() as u64) * 8;
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

pub fn paired_device_count() -> usize {
    std::fs::read(mobile_dir().join("devices.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("devices").and_then(|d| d.as_array()).map(|a| a.len()))
        .unwrap_or(0)
}

/// Cheap fingerprint of the paired-device set (ids in file order). Used by
/// the streamer supervisor to notice a pairing/revocation without parsing
/// credentials; any change means the user is actively re-pairing.
pub fn paired_device_signature() -> String {
    std::fs::read(mobile_dir().join("devices.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            v.get("devices").and_then(|d| d.as_array()).map(|devices| {
                devices
                    .iter()
                    .filter_map(|device| device.get("id").and_then(|id| id.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })
        .unwrap_or_default()
}

fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

pub(crate) fn principal_for_bearer(
    headers: &HashMap<String, String>,
) -> Option<ControllerPrincipal> {
    let token = bearer_token(headers.get("authorization")?)?;
    let hash = sha256_hex(token);
    let host_id = unpeel_core::relay_uplink::ensure_host_id().ok()?;
    let host_owner_principal_id = unpeel_core::state::host_owner_principal_id(&host_id);
    std::fs::read(mobile_dir().join("devices.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("devices").cloned())
        .and_then(|d| d.as_array().cloned())
        .and_then(|devices| {
            devices.iter().find_map(|device| {
                if device.get("tokenHash").and_then(|value| value.as_str()) != Some(hash.as_str()) {
                    return None;
                }
                Some(ControllerPrincipal::PairedDevice {
                    device_id: device.get("id")?.as_str()?.to_owned(),
                    name: device
                        .get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("device")
                        .to_owned(),
                    principal_id: Some(
                        device
                            .get("principalID")
                            .and_then(|value| value.as_str())
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or(&host_owner_principal_id)
                            .to_owned(),
                    ),
                })
            })
        })
}

/// ANSI/UTF-8 boundary scan (port of `align_tail_start_in_window`): returns
/// the last safe boundary at or before `data.len()`, scanning from 0.
fn last_safe_boundary(data: &[u8]) -> usize {
    #[derive(PartialEq)]
    enum S {
        Ground,
        Esc,
        Csi,
        Osc,
        OscEsc,
    }
    let mut state = S::Ground;
    let mut boundary = 0usize;
    let mut i = 0usize;
    while i < data.len() {
        let b = data[i];
        match state {
            S::Ground => match b {
                0x1b => state = S::Esc,
                0x80..=0xbf => {} // utf-8 continuation: not a boundary start
                _ => {
                    // boundary after complete utf-8 scalar
                    let len = if b < 0x80 {
                        1
                    } else if b >= 0xf0 {
                        4
                    } else if b >= 0xe0 {
                        3
                    } else {
                        2
                    };
                    if i + len <= data.len() {
                        i += len;
                        boundary = i;
                        continue;
                    } else {
                        break;
                    }
                }
            },
            S::Esc => match b {
                b'[' => state = S::Csi,
                b']' | b'P' | b'X' | b'^' | b'_' => state = S::Osc,
                _ => {
                    state = S::Ground;
                    boundary = i + 1;
                }
            },
            S::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    state = S::Ground;
                    boundary = i + 1;
                }
            }
            S::Osc => match b {
                0x07 => {
                    state = S::Ground;
                    boundary = i + 1;
                }
                0x1b => state = S::OscEsc,
                _ => {}
            },
            S::OscEsc => {
                state = if b == b'\\' {
                    boundary = i + 1;
                    S::Ground
                } else {
                    S::Osc
                };
            }
        }
        i += 1;
    }
    boundary
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub(crate) struct Request {
    pub(crate) request_id: Option<String>,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: HashMap<String, String>,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: Vec<u8>,
    pub(crate) keep_alive: bool,
}

fn read_request<S: Read>(stream: &mut S, pending: &mut Vec<u8>) -> Option<Request> {
    let header_end = loop {
        if let Some(pos) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if pending.len() > 8 * 1024 * 1024 {
            return None;
        }
        let mut chunk = [0u8; 16 * 1024];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&pending[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY {
        return None;
    }
    while pending.len() < header_end + content_length {
        let mut chunk = [0u8; 16 * 1024];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
        }
    }
    let body = pending[header_end..header_end + content_length].to_vec();
    pending.drain(..header_end + content_length);

    let (path, query_string) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut query = HashMap::new();
    for pair in query_string.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(k.to_string(), urldecode(v));
    }
    let connection = headers.get("connection").map(|s| s.to_lowercase());
    let keep_alive = match connection.as_deref() {
        Some("close") => false,
        Some(v) if v.contains("keep-alive") => true,
        _ => version == "HTTP/1.1",
    };
    let request_id = headers
        .get("x-unpeel-request-id")
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .cloned();
    Some(Request {
        request_id,
        method,
        path,
        query,
        headers,
        body,
        keep_alive,
    })
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond<S: Write>(stream: &mut S, status: u16, body: &str, keep_alive: bool) -> bool {
    respond_with(stream, status, "", body, keep_alive)
}

/// `respond` with extra raw header lines (each `\r\n`-terminated).
fn respond_with<S: Write>(
    stream: &mut S,
    status: u16,
    extra_headers: &str,
    body: &str,
    keep_alive: bool,
) -> bool {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        405 => "Method Not Allowed",
        426 => "Upgrade Required",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    };
    let connection = if keep_alive { "keep-alive" } else { "close" };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\n{extra_headers}Content-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
        body.len()
    )
    .and_then(|_| stream.flush())
    .is_ok()
}

/// The plaintext-credential refusal: `426` names TLS as the upgrade and closes
/// the connection so a client cannot keep streaming its token in the clear.
fn respond_upgrade_required<S: Write>(stream: &mut S, body: &str) -> bool {
    respond_with(stream, 426, "Upgrade: TLS/1.3, HTTP/1.1\r\n", body, false)
}

fn error_body(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn safe_session_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/') && !id.contains('\\') && !id.contains("..")
}

fn session_dir(id: &str) -> std::path::PathBuf {
    app_paths::app_sessions_root().join(id)
}

/// Live `__remote__` advertisement (port + TLS fingerprint) when that server
/// runs — the app or a future TUI supervisor spawns it; we just relay state.
pub(crate) fn remote_server_advertisement() -> (Option<u64>, Option<String>) {
    remote_server_advertisement_at(&app_paths::unpeel_home())
}

/// The record names its own pid and kernel start time (`pid_started_at`,
/// written by `__remote__` at startup). Only a pid that provably started
/// when the record says is the streamer: a bare liveness probe would relay
/// a dead streamer's port and fingerprint whenever the pid was recycled onto
/// an unrelated process, and a record without a start time proves nothing.
fn remote_server_advertisement_at(home: &std::path::Path) -> (Option<u64>, Option<String>) {
    let Ok(raw) = std::fs::read(home.join("remote.json")) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return (None, None);
    };
    let pid = value
        .get("pid")
        .and_then(|v| v.as_u64())
        .and_then(|pid| u32::try_from(pid).ok());
    let started_at = value.get("pid_started_at").and_then(|v| v.as_u64());
    let is_streamer = pid.is_some_and(|pid| {
        unpeel_core::session_host::recorded_pid_identity(pid, started_at)
            == unpeel_core::session_host::PidIdentity::Matches
    });
    if !is_streamer {
        return (None, None);
    }
    (
        value.get("port").and_then(|v| v.as_u64()),
        value
            .get("fingerprint")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    )
}

fn handle_output(request: &Request) -> (u16, String) {
    let Some(session_id) = request
        .query
        .get("session_id")
        .or_else(|| request.query.get("sessionID"))
        .filter(|s| safe_session_id(s))
    else {
        return (400, error_body("invalid session id"));
    };
    let path = session_dir(session_id).join("output.bin");
    let limit = request
        .query
        .get("limit")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(512 * 1024)
        .clamp(1, 8 * 1024 * 1024);
    let offset = request
        .query
        .get("offset")
        .and_then(|v| v.parse::<u64>().ok());
    let wait_ms = request
        .query
        .get("wait_ms")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        .min(25_000);

    let mut size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if wait_ms > 0 {
        if let Some(offset) = offset {
            if offset == size {
                let deadline = Instant::now() + Duration::from_millis(wait_ms);
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                    size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    if size > offset {
                        break;
                    }
                }
            }
        }
    }

    let chunk = match unpeel_core::session_host::read_output_chunk(
        session_id,
        offset,
        Some(limit as usize),
        Some(limit as usize),
    ) {
        Ok(chunk) => chunk,
        Err(error) => return (500, error_body(&error)),
    };
    let start = chunk.next_offset.saturating_sub(chunk.data.len() as u64);
    let truncated = offset.map_or(start > 0, |requested| requested != start);
    let mut data = chunk.data;
    if !truncated {
        let boundary = last_safe_boundary(&data);
        data.truncate(boundary);
    }
    let body = serde_json::json!({
        "sessionID": session_id,
        "offset": start,
        "nextOffset": start + data.len() as u64,
        "dataBase64": base64(&data),
        "truncated": truncated,
        "capturedAtUnixMs": now_ms(),
    });
    (200, body.to_string())
}

fn body_json(request: &Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

fn controller_body(request: &Request) -> (serde_json::Value, Option<String>) {
    if request.body.is_empty() {
        return (serde_json::Value::Null, None);
    }
    match serde_json::from_slice(&request.body) {
        Ok(value) => (value, None),
        Err(_) => (serde_json::Value::Null, Some(base64(&request.body))),
    }
}

fn body_session_id(body: &serde_json::Value) -> Option<String> {
    let session_id = body.get("sessionID")?.as_str()?.trim();
    safe_session_id(session_id).then(|| session_id.to_owned())
}

fn paired_device_id(principal: &ControllerPrincipal) -> Option<&str> {
    match principal {
        ControllerPrincipal::PairedDevice { device_id, .. } => Some(device_id.as_str()),
        ControllerPrincipal::OwnerTransport { .. } => None,
    }
}

fn platform_adapter_response(
    capability: &str,
    request: serde_json::Value,
    platform_adapters: &PlatformAdapterHub,
) -> (u16, String) {
    match platform_adapters.call(capability, request) {
        Ok(response) => (response.status, response.body.to_string()),
        Err(PlatformAdapterError::Unavailable) => (404, error_body("not found")),
        Err(error) => (503, error_body(&error.to_string())),
    }
}

fn handle_push_registration(
    request: &Request,
    principal: &ControllerPrincipal,
    platform_adapters: &PlatformAdapterHub,
) -> (u16, String) {
    // Adapter-free Hosts retain the shipped 404 dialect, including for a
    // malformed body. Capability presence is the discovery mechanism.
    if !platform_adapters.supports("push.register") {
        return (404, error_body("not found"));
    }
    let body = body_json(request);
    let (Some(raw_token), Some(raw_environment)) = (
        body.get("apnsToken").and_then(serde_json::Value::as_str),
        body.get("environment").and_then(serde_json::Value::as_str),
    ) else {
        return (400, error_body("request failed"));
    };
    let token = raw_token.trim_matches(|character| character == ' ' || character == '\t');
    if !(16..=200).contains(&token.len()) || !token.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return (400, error_body("invalid apns token"));
    }
    let Some(device_id) = paired_device_id(principal) else {
        return (403, error_body("paired device required"));
    };
    let environment = if raw_environment == "production" {
        "production"
    } else {
        "sandbox"
    };
    platform_adapter_response(
        "push.register",
        serde_json::json!({
            "deviceID": device_id,
            "apnsToken": token,
            "environment": environment,
        }),
        platform_adapters,
    )
}

/// How long a freshly minted recovery credential is handed back to the same
/// device instead of minting again. Each mint rewrites `devices.json`, which
/// the uplink must announce; a phone retrying inside this window (its retry
/// cadence on 2026-09-01 was seconds) therefore receives the token it was
/// already given rather than rotating the Host a second time.
const RECOVERY_REPLAY_WINDOW: Duration = Duration::from_secs(15);

static RECENT_RECOVERIES: std::sync::OnceLock<Mutex<HashMap<String, (Instant, String)>>> =
    std::sync::OnceLock::new();

fn recent_recoveries() -> &'static Mutex<HashMap<String, (Instant, String)>> {
    RECENT_RECOVERIES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn clear_recent_recoveries() {
    if let Ok(mut recent) = recent_recoveries().lock() {
        recent.clear();
    }
}

fn handle_relay_credentials_recovery(
    principal: &ControllerPrincipal,
    platform_adapters: &PlatformAdapterHub,
) -> (u16, String) {
    if !platform_adapters.supports("relay.credentials.recover") {
        return (404, error_body("not found"));
    }
    let Some(device_id) = paired_device_id(principal) else {
        return (403, error_body("paired device required"));
    };
    let device_id = device_id.to_string();
    if let Ok(mut recent) = recent_recoveries().lock() {
        recent.retain(|_, (minted_at, _)| minted_at.elapsed() < RECOVERY_REPLAY_WINDOW);
        if let Some((_, body)) = recent.get(&device_id) {
            crate::tracelog::trace(
                "relay-recovery",
                &format!("device {device_id} retried within the replay window; returning the current credential"),
            );
            return (200, body.clone());
        }
    }
    let (status, body) = platform_adapter_response(
        "relay.credentials.recover",
        serde_json::json!({ "deviceID": device_id.clone() }),
        platform_adapters,
    );
    if status == 200 {
        if let Ok(mut recent) = recent_recoveries().lock() {
            recent.insert(device_id, (Instant::now(), body.clone()));
        }
    }
    (status, body)
}

/// Resolve the headless create catalog from the published Host-owned snapshot.
/// Preset scope travels beside the public bootstrap DTO because that DTO does
/// not expose it. Controller-supplied paths never enter this catalog.
fn headless_create_context(
    snapshot: &SharedSnapshot,
    hook_port: Option<u16>,
) -> Option<HostCreateContext> {
    let host_id = unpeel_core::relay_uplink::ensure_host_id().ok()?;
    let host_owner_principal_id = unpeel_core::state::host_owner_principal_id(&host_id);
    let (bootstrap, presets) = {
        let snapshot = snapshot.lock().ok()?;
        (snapshot.bootstrap.clone(), snapshot.create_presets.clone())
    };

    let projects = bootstrap
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|project| {
            let id = project.get("id")?.as_str()?.to_owned();
            let path = project
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let worktree_branch = project
                .get("worktreeBranch")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Some(HostCreateProject {
                id,
                path: path.clone(),
                is_folder: project
                    .get("isGroup")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                    || project
                        .get("isFolder")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                // A worktree project publishes its canonical checkout as its
                // own path. Treat the branch as the Host-owned discriminator;
                // a top-level project never accepts an arbitrary request path.
                worktree_path: worktree_branch.as_ref().map(|_| path),
                worktree_branch,
            })
        })
        .collect();
    let executor = Arc::new(move |resolved| {
        unpeel_core::controller_api::execute_headless_session_create(resolved, hook_port)
    });
    Some(HostCreateContext::new(
        host_owner_principal_id,
        projects,
        presets,
        executor,
    ))
}

fn headless_controller_effects(hook_port: Option<u16>) -> ControllerEffects {
    ControllerEffects::new(Arc::new(move |request| {
        unpeel_core::controller_api::execute_headless_session_action(request, hook_port)
    }))
}

// Keep the transport-owned inputs explicit at this adapter boundary: tests
// override only the effect executor, while production supplies the same
// snapshot/auth/resize components used by the LAN and Relay entry points.
#[allow(clippy::too_many_arguments)]
fn handle_with_effects(
    request: &Request,
    principal: &ControllerPrincipal,
    snapshot: &SharedSnapshot,
    mark_read: &Sender<String>,
    hook_port: Option<u16>,
    resizes: &MobileResizes,
    approvals: &Arc<crate::approvals::ApprovalHub>,
    platform_adapters: &PlatformAdapterHub,
    controller_effects_override: Option<&ControllerEffects>,
) -> (u16, String) {
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/mobile/push-token") => {
            return handle_push_registration(request, principal, platform_adapters)
        }
        ("GET", "/mobile/relay-credentials") => {
            return handle_relay_credentials_recovery(principal, platform_adapters)
        }
        _ => {}
    }
    let route_context = if request.method == "GET" && request.path == "/mobile/bootstrap" {
        let core = snapshot
            .lock()
            .ok()
            .map(|guard| guard.bootstrap.clone())
            .unwrap_or_default();
        let (port, fingerprint) = remote_server_advertisement();
        let mut context = HostBootstrapContext::headless(core);
        context.host_id = std::fs::read_to_string(mobile_dir().join("mac-id"))
            .map(|value| value.trim().to_owned())
            .ok();
        context.remote_server_port = port.and_then(|value| u16::try_from(value).ok());
        // One certificate serves both pinned transports, so the fingerprint
        // is advertised whenever the direct listener has it — a Controller
        // pins `/mobile` even while the streamer is down.
        context.remote_server_certificate_fingerprint =
            fingerprint.or_else(direct_certificate_fingerprint);
        context.pending_approvals = approvals.list_json();
        platform_adapters.decorate_protocol(&mut context.protocol);
        Some(HostRouteContext {
            bootstrap: Some(context),
            archived_sessions_by_project: HashMap::new(),
        })
    } else if request.method == "GET" && request.path == "/mobile/archive" {
        snapshot.lock().ok().map(|guard| HostRouteContext {
            bootstrap: None,
            archived_sessions_by_project: guard.archived_sessions_by_project.clone(),
        })
    } else {
        None
    };
    let (body, body_base64) = controller_body(request);
    let controller_request = ControllerRequest {
        id: request.request_id.clone(),
        method: request.method.clone(),
        path: request.path.clone(),
        query: request.query.clone(),
        body,
        content_type: request.headers.get("content-type").cloned(),
        body_base64,
        principal: principal.clone(),
    };
    let create_context = (request.method == "POST" && request.path == "/mobile/sessions")
        .then(|| headless_create_context(snapshot, hook_port))
        .flatten();
    let owned_controller_effects = matches!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/mobile/restart-session") | ("POST", "/mobile/session-action")
    )
    .then(|| headless_controller_effects(hook_port));
    let controller_effects = controller_effects_override.or(owned_controller_effects.as_ref());
    if let Some(response) = unpeel_core::controller_api::route_with_effects(
        &controller_request,
        route_context.as_ref(),
        create_context.as_ref(),
        controller_effects,
    ) {
        // The core owns the resize semantics; the TUI adapter retains only
        // its presentation-side ownership timer so it does not immediately
        // resize the shared PTY back to the local preview grid.
        if response.status == 200 && request.path == "/mobile/resize" {
            if let Some(session_id) = body_session_id(&controller_request.body) {
                if let Ok(mut guard) = resizes.lock() {
                    guard.insert(session_id, Instant::now());
                }
            }
        }
        if response.status == 200 && request.path == "/mobile/mark-read" {
            if let Some(session_id) = body_session_id(&controller_request.body) {
                let _ = mark_read.send(session_id);
            }
        }
        // `max_dim` is optional native ImageIO enrichment. The shared core
        // has already validated the Controller principal, Session, artifact
        // path, range, and original-byte read above; only then may the Mac
        // adapter derive one bounded response chunk. Headless or failed
        // enrichment falls back to the core's original bytes.
        if response.status == 200
            && request.method == "GET"
            && request.path == "/mobile/artifact"
            && request
                .query
                .get("max_dim")
                .and_then(|value| value.parse::<u32>().ok())
                .is_some_and(|value| value > 0)
            && platform_adapters.supports("artifact.thumbnail")
        {
            let mut query = serde_json::Map::new();
            for key in [
                "session_id",
                "sessionID",
                "kind",
                "name",
                "offset",
                "limit",
                "max_dim",
            ] {
                if let Some(value) = request.query.get(key) {
                    query.insert(key.to_owned(), value.clone().into());
                }
            }
            if let Ok(native) =
                platform_adapters.call("artifact.thumbnail", serde_json::json!({ "query": query }))
            {
                if native.status == 200 && valid_native_artifact_chunk(&native.body, &request.query)
                {
                    return (200, native.body.to_string());
                }
            }
        }
        return (response.status, response.body_json());
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/mobile/output") => handle_output(request),
        ("POST", "/mobile/session-organization") => {
            let body = body_json(request);
            let Some(session_id) = body_session_id(&body) else {
                return (400, error_body("invalid session id"));
            };

            let title = match body.get("title") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(value)) => {
                    let value = value.trim();
                    (!value.is_empty()).then(|| value.to_owned())
                }
                Some(_) => return (400, error_body("title must be a string")),
            };
            let pinned = match body.get("pinned") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::Bool(value)) => Some(*value),
                Some(_) => return (400, error_body("pinned must be a boolean")),
            };
            let archived = match body.get("archived") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::Bool(value)) => Some(*value),
                Some(_) => return (400, error_body("archived must be a boolean")),
            };
            let project_id = match body.get("projectID") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(value)) => {
                    let value = value.trim();
                    if value.is_empty() {
                        return (400, error_body("projectID must not be empty"));
                    }
                    Some(value.to_owned())
                }
                Some(_) => return (400, error_body("projectID must be a string")),
            };
            let notify_when_done = match body.get("notifyWhenDone") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::Bool(value)) => Some(*value),
                Some(_) => {
                    return (400, error_body("notifyWhenDone must be a boolean"));
                }
            };
            let published = snapshot
                .lock()
                .ok()
                .and_then(|snapshot| snapshot.bootstrap.get("sessions").cloned())
                .and_then(|sessions| sessions.as_array().cloned())
                .is_some_and(|sessions| {
                    sessions.iter().any(|session| {
                        session.get("id").and_then(serde_json::Value::as_str)
                            == Some(session_id.as_str())
                    })
                });
            if !published && unpeel_core::session_host::load_manifest(&session_id).is_none() {
                return (404, error_body("unknown session"));
            }
            // Preserve the shipped Host resource/effect ordering when no
            // native adapter is live: resolve and type-check the Session
            // first, then reject the platform-only field before any shared
            // title/pin/archive/project write can land. A registration can
            // still disappear after this preflight; that later callback
            // failure is effect-unknown and Controllers must refresh.
            if notify_when_done.is_some()
                && !platform_adapters.supports("session.notify_when_done.set")
            {
                return (
                    501,
                    error_body("notifyWhenDone is not supported by this Host"),
                );
            }
            if let Some(target) = project_id.as_deref() {
                // Known target, and inside the Session's own checkout: the
                // shared guard every Host kind applies. Clearing back to the
                // manifest project is always legal.
                let manifest_project = unpeel_core::session_host::load_manifest(&session_id)
                    .map(|manifest| manifest.session.project_id)
                    .unwrap_or_default();
                if target != manifest_project {
                    let projects = snapshot
                        .lock()
                        .ok()
                        .and_then(|snapshot| snapshot.bootstrap.get("projects").cloned())
                        .and_then(|projects| projects.as_array().cloned())
                        .unwrap_or_default();
                    if let Err(message) =
                        unpeel_core::controller_host::validate_session_project_target(
                            &projects,
                            &manifest_project,
                            target,
                        )
                    {
                        return (400, error_body(&message));
                    }
                }
            }
            if title.is_none()
                && pinned.is_none()
                && archived.is_none()
                && project_id.is_none()
                && notify_when_done.is_none()
            {
                // Match the shipped native DTO: an empty patch, explicit
                // nulls, and a title that trims to empty are successful no-ops.
                return (200, r#"{"ok":true}"#.into());
            }

            // This v1 route spans app-state.json, title.json, and the archive
            // marker/control socket, so it cannot be a cross-file transaction.
            // Put the fallible shared-state placement/pin preconditions first:
            // if either fails, title/archive are untouched. Once placement,
            // pin, or title lands, any later failure is effect-unknown;
            // Controllers must refresh Host state before deciding whether to
            // retry and must not manufacture a fresh request id blindly.
            if let Some(target) = project_id.as_deref() {
                let manifest_project = unpeel_core::session_host::load_manifest(&session_id)
                    .map(|manifest| manifest.session.project_id)
                    .unwrap_or_default();
                let result = if target == manifest_project {
                    unpeel_core::session_ops::clear_project_override(&session_id)
                } else {
                    unpeel_core::session_ops::set_project_override(&session_id, target)
                };
                if let Err(e) = result {
                    return (
                        500,
                        error_body(&format!("organization project preflight failed: {e}")),
                    );
                }
            }
            if let Some(pinned) = pinned {
                if let Err(e) = unpeel_core::session_ops::set_pinned(&session_id, pinned) {
                    return (
                        500,
                        error_body(&format!("organization pin preflight failed: {e}")),
                    );
                }
            }
            if let Some(title) = title {
                if let Err(e) = unpeel_core::session_ops::set_title(&session_id, &title) {
                    return (
                        500,
                        error_body(&format!(
                            "organization update effect unknown; refresh Host state: {e}"
                        )),
                    );
                }
            }
            match archived {
                Some(true) => {
                    if let Err(e) = unpeel_core::session_ops::archive_session(&session_id) {
                        return (
                            500,
                            error_body(&format!(
                                "organization update effect unknown; refresh Host state: {e}"
                            )),
                        );
                    }
                }
                Some(false) => {
                    if let Err(e) = unpeel_core::session_ops::restore_session(&session_id) {
                        return (
                            500,
                            error_body(&format!(
                                "organization update effect unknown; refresh Host state: {e}"
                            )),
                        );
                    }
                }
                None => {}
            }
            // This remains a Host operation: shared Host semantics land first,
            // then only the platform-owned notification preference crosses a
            // live connection-scoped adapter. A callback failure after a
            // compound patch is effect-unknown, matching the route's existing
            // cross-file contract; Controllers refresh and never auto-replay.
            if let Some(notify_when_done) = notify_when_done {
                let response = platform_adapters.call(
                    "session.notify_when_done.set",
                    serde_json::json!({
                        "sessionID": session_id,
                        "notifyWhenDone": notify_when_done,
                    }),
                );
                match response {
                    Ok(response) if response.status == 200 => {}
                    Ok(response) => return (response.status, response.body.to_string()),
                    Err(PlatformAdapterError::Unavailable) => {
                        return (
                            501,
                            error_body("notifyWhenDone is not supported by this Host"),
                        )
                    }
                    Err(error) => return (503, error_body(&error.to_string())),
                }
            }
            (200, r#"{"ok":true}"#.into())
        }
        ("POST", "/mobile/project-organization") => {
            handle_project_organization(&body_json(request), snapshot, platform_adapters)
        }
        ("POST", "/mobile/presets") => handle_presets(&body_json(request), snapshot),
        // Capability `settings.workspace.set`: shared disk-backed semantics —
        // the same function the SSH gateway serves.
        ("POST", "/mobile/workspace-settings") => {
            let (status, body) =
                unpeel_core::controller_host::workspace_settings_response(&body_json(request));
            (status, body.to_string())
        }
        ("POST", "/mobile/resize-desktop") => {
            // This is the phone's FIT verb: on the desktop it letterboxes
            // the surface AND resizes the PTY to the phone grid. App-less
            // the letterbox half doesn't exist (the TUI letterboxes its
            // preview on its own), but the PTY resize is the part that makes
            // the terminal fit the phone — do it for real.
            let body = body_json(request);
            let Some(session_id) = body_session_id(&body) else {
                return (400, error_body("invalid session id"));
            };
            // The fit is also recorded beside the Session (`phone-fit.json`)
            // and published in its summary, so a desktop Controller — the
            // Mac app in Host-service client mode — letterboxes its surface
            // to the same grid and offers "fit to desktop". `clear` removes
            // it; a worker restart keeps an active fit.
            let dir = session_dir(&session_id);
            if body.get("clear").and_then(|v| v.as_bool()) == Some(true) {
                if let Ok(mut guard) = resizes.lock() {
                    guard.remove(&session_id);
                }
                let _ = unpeel_core::session_ops::clear_phone_fit_marker_in(&dir);
                return (200, r#"{"ok":true}"#.into());
            }
            let cols = body
                .get("columns")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .clamp(2, 300);
            let rows = body
                .get("rows")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .clamp(2, 120);
            match crate::control::send_resize(&dir, cols as u16, rows as u16) {
                Ok(()) => {
                    if let Ok(mut guard) = resizes.lock() {
                        guard.insert(session_id, Instant::now());
                    }
                    let _ = unpeel_core::session_ops::write_phone_fit_marker_in(
                        &dir,
                        &unpeel_core::session_ops::PhoneFitMarker {
                            columns: cols as u16,
                            rows: rows as u16,
                            since_unix_ms: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                        },
                    );
                    (200, r#"{"ok":true}"#.into())
                }
                Err(_) => (404, error_body("session host unavailable")),
            }
        }
        ("POST", "/mobile/approvals/answer") => handle_approval_answer(request, approvals),
        _ => (404, error_body("not found")),
    }
}

fn handle_approval_answer(
    request: &Request,
    approvals: &Arc<crate::approvals::ApprovalHub>,
) -> (u16, String) {
    let body = body_json(request);
    let (Some(id), Some(approved)) = (
        body.get("id").and_then(|v| v.as_str()),
        body.get("approved").and_then(|v| v.as_bool()),
    ) else {
        return (400, error_body("request failed"));
    };
    if approvals.answer(id, approved) {
        (200, r#"{"ok":true}"#.into())
    } else {
        (409, error_body("approval no longer pending"))
    }
}

/// Live worker-owned routes that the persistent local Host socket cannot
/// satisfy from a disk catalog. Keep this deliberately narrow: ordinary
/// lifecycle/organization verbs still flow through `ControllerHostRuntime`
/// and retain its generation-bound replay semantics, while pairing and
/// approvals use the exact in-memory authorities shared with Direct/Link.
pub(crate) fn handle_local_live_route(
    request: &unpeel_core::relay_wire::TunnelRequest,
    approvals: &Arc<crate::approvals::ApprovalHub>,
    pairing: &crate::pairing::PairingWindow,
    snapshot: &SharedSnapshot,
) -> Option<(u16, Vec<u8>)> {
    if request.path != "/mobile/pairing-invitation"
        && !(request.path == "/mobile/approvals/answer" && request.method == "POST")
    {
        return None;
    }
    let request = Request {
        request_id: Some(request.id.to_string()),
        method: request.method.clone(),
        path: request.path.clone(),
        query: request.query.iter().cloned().collect(),
        headers: request
            .content_type
            .as_ref()
            .map(|value| HashMap::from([("content-type".into(), value.clone())]))
            .unwrap_or_default(),
        body: request.body.clone(),
        keep_alive: false,
    };
    let (status, body) = if request.path == "/mobile/approvals/answer" {
        handle_approval_answer(&request, approvals)
    } else {
        let direct_endpoint = canonical_server_port()
            .map(|port| format!("http://{}:{port}/mobile", preferred_lan_address()));
        match direct_endpoint {
            Some(endpoint) => handle_pairing_invitation(&request, pairing, &endpoint, snapshot),
            None => (503, error_body("pairing service unavailable")),
        }
    };
    Some((status, body.into_bytes()))
}

/// POST /mobile/project-organization (capability `project.organization.set`):
/// shared disk-backed semantics live in
/// `unpeel_core::controller_host::project_organization_response` (the SSH
/// gateway serves the same function), resolved against the published
/// bootstrap — display-ordered, so sibling indices mean exactly what the
/// Controller saw. The TUI adapter adds the one platform capability a bare
/// gateway lacks: folder colors, written into the desktop app's UserDefaults
/// on macOS with a state-bus ping so every frontend re-reads.
fn handle_project_organization(
    body: &serde_json::Value,
    snapshot: &SharedSnapshot,
    platform_adapters: &PlatformAdapterHub,
) -> (u16, String) {
    let projects: Vec<serde_json::Value> = snapshot
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .bootstrap
                .get("projects")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    let write_color = |project_id: &str, color: Option<&str>| -> Result<(), String> {
        if platform_adapters.supports("overlay.project-color.set") {
            let response = platform_adapters
                .call(
                    "overlay.project-color.set",
                    serde_json::json!({
                        "projectID": project_id,
                        "colorID": color.unwrap_or_default(),
                    }),
                )
                .map_err(|error| error.to_string())?;
            if response.status != 200 {
                return Err(response
                    .body
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("native folder-color adapter rejected the write")
                    .to_owned());
            }
        } else {
            // Released compatibility clients have no registration. Preserve
            // the historical default-workspace `defaults` writer until the
            // client-only gate flips; isolated native workspaces reach their
            // own UserDefaults suite only through the live adapter above.
            crate::overlay::write_project_folder_color(project_id, color)?;
        }
        // Colors live in UserDefaults, outside the app-state choke point's
        // own announce — ping peers explicitly, like the local color menu.
        unpeel_core::state_bus::announce(
            unpeel_core::state_bus::Change::AppState,
            unpeel_core::session_ops::own_listener_port_public(),
        );
        Ok(())
    };
    let color_writer: Option<ProjectColorWriter<'_>> = if platform_adapters
        .supports("overlay.project-color.set")
        || crate::overlay::project_folder_color_supported()
    {
        Some(&write_color)
    } else {
        None
    };
    let (status, body) =
        unpeel_core::controller_host::project_organization_response(body, &projects, color_writer);
    (status, body.to_string())
}

/// POST /mobile/presets (capability `settings.presets.set`): shared
/// disk-backed semantics live in
/// `unpeel_core::controller_host::preset_patch_response` (the SSH gateway
/// serves the same function), resolved against the published bootstrap's
/// preset list so ids and sort indices mean exactly what the Controller saw.
/// The write itself goes through `app_state::edit` — flock + state-bus
/// announce — so a running app and this TUI pick the change up live.
fn handle_presets(body: &serde_json::Value, snapshot: &SharedSnapshot) -> (u16, String) {
    let presets: Vec<serde_json::Value> = snapshot
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .bootstrap
                .get("presets")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    let (status, body) = unpeel_core::controller_host::preset_patch_response(body, &presets);
    (status, body.to_string())
}

/// The authenticated adapter boundary shared by LAN and Relay transports.
/// Keep method/path semantics here so every transport — and the conformance
/// harness — observes the same response rather than bypassing route guards.
#[allow(clippy::too_many_arguments)]
fn handle_authenticated_with_effects(
    request: &Request,
    principal: &ControllerPrincipal,
    snapshot: &SharedSnapshot,
    mark_read: &Sender<String>,
    hook_port: Option<u16>,
    resizes: &MobileResizes,
    approvals: &Arc<crate::approvals::ApprovalHub>,
    pairing: Option<&crate::pairing::PairingWindow>,
    direct_endpoint: Option<&str>,
    platform_adapters: &PlatformAdapterHub,
    presence: Option<&crate::presence::PresenceHub>,
    controller_effects_override: Option<&ControllerEffects>,
    direct_path: Option<&Arc<crate::direct_path::DirectPathHub>>,
) -> (u16, String) {
    if !request.path.starts_with("/mobile/") || request.path == "/mobile/pair" {
        return (404, error_body("not found"));
    }
    if request.method != "GET" && request.method != "POST" {
        return (405, error_body("method not allowed"));
    }
    // Reserved `direct.path.*` operations (unpeel-apple:docs/feature/direct-path-v1.md):
    // connection-scoped like pairing, handled here where transport context
    // lives. Only a relay-tunneled request carries a conn id and handshake
    // material; LAN and SSH get the contract's coarse 409. Not in the
    // capability ledger until the native adapter reaches parity.
    if request.path == "/mobile/direct-path" || request.path == "/mobile/direct-path-result" {
        if request.method != "POST" {
            return (405, error_body("method not allowed"));
        }
        let conn_id = crate::direct_path::relay_conn_id(request.request_id.as_deref());
        let (Some(hub), Some(conn_id)) = (direct_path, conn_id) else {
            return (409, error_body("direct path requires a relayed connection"));
        };
        let (status, value) = if request.path == "/mobile/direct-path" {
            hub.negotiate(conn_id, &request.body)
        } else {
            hub.result(conn_id, &request.body)
        };
        return (status, value.to_string());
    }
    if request.path == "/mobile/pairing-invitation" {
        let (Some(pairing), Some(direct_endpoint)) = (pairing, direct_endpoint) else {
            return (503, error_body("pairing service unavailable"));
        };
        return handle_pairing_invitation(request, pairing, direct_endpoint, snapshot);
    }
    let output_session_id = (request.method == "GET" && request.path == "/mobile/output")
        .then(|| {
            request
                .query
                .get("session_id")
                .or_else(|| request.query.get("sessionID"))
                .filter(|session_id| safe_session_id(session_id))
                .cloned()
        })
        .flatten();
    let response = handle_with_effects(
        request,
        principal,
        snapshot,
        mark_read,
        hook_port,
        resizes,
        approvals,
        platform_adapters,
        controller_effects_override,
    );
    if response.0 == 200 {
        if let (Some(session_id), Some(presence)) = (output_session_id, presence) {
            if presence.touch_output(&session_id, principal, crate::presence::now_ms()) {
                // Treat a phone actually rendering output as a read receipt,
                // just as selecting the Session in another Controller does.
                // Presence buckets this to one write/second rather than one
                // state-bus broadcast per long-poll response.
                let _ = mark_read.send(session_id);
            }
        }
    }
    response
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_authenticated(
    request: &Request,
    principal: &ControllerPrincipal,
    snapshot: &SharedSnapshot,
    mark_read: &Sender<String>,
    hook_port: Option<u16>,
    resizes: &MobileResizes,
    approvals: &Arc<crate::approvals::ApprovalHub>,
    pairing: &crate::pairing::PairingWindow,
    direct_endpoint: &str,
    platform_adapters: &PlatformAdapterHub,
    presence: Option<&crate::presence::PresenceHub>,
    direct_path: Option<&Arc<crate::direct_path::DirectPathHub>>,
) -> (u16, String) {
    handle_authenticated_with_effects(
        request,
        principal,
        snapshot,
        mark_read,
        hook_port,
        resizes,
        approvals,
        Some(pairing),
        Some(direct_endpoint),
        platform_adapters,
        presence,
        None,
        direct_path,
    )
}

fn handle_pairing_invitation(
    request: &Request,
    pairing: &crate::pairing::PairingWindow,
    direct_endpoint: &str,
    snapshot: &SharedSnapshot,
) -> (u16, String) {
    if request.method != "POST" {
        return (405, error_body("method not allowed"));
    }
    let body = body_json(request);
    let Some(action @ ("create" | "complete")) =
        body.get("action").and_then(|value| value.as_str())
    else {
        return (400, error_body("invalid pairing invitation action"));
    };
    let snapshot_identity = snapshot.lock().ok().and_then(|guard| {
        Some((
            guard.bootstrap.get("macID")?.as_str()?.to_owned(),
            guard.bootstrap.get("macName")?.as_str()?.to_owned(),
        ))
    });
    let (mac_id, mac_name) = snapshot_identity.unwrap_or_else(|| {
        (
            std::fs::read_to_string(mobile_dir().join("mac-id"))
                .map(|value| value.trim().to_owned())
                .unwrap_or_default(),
            hostname(),
        )
    });
    if mac_id.is_empty() {
        return (503, error_body("Host identity unavailable"));
    }
    match action {
        "create" => {
            let Some(endpoint) = body.get("endpoint").and_then(|value| value.as_str()) else {
                return (400, error_body("pairing proxy endpoint required"));
            };
            let Some(payload) =
                pairing.begin_invitation(endpoint, direct_endpoint, &mac_id, &mac_name)
            else {
                return (400, error_body("invalid pairing proxy endpoint"));
            };
            (200, payload.to_string())
        }
        "complete" => {
            let Some(envelope) = body.get("envelope") else {
                return (400, error_body("pairing envelope required"));
            };
            let Ok(encoded) = serde_json::to_vec(envelope) else {
                return (400, error_body("invalid pairing envelope"));
            };
            let (status, response) = pairing.handle_pair(&encoded, &mac_id, &mac_name);
            if status == 200 {
                // The sealed receipt reached an already-authorized proxy.
                // That proxy remains responsible for the final phone write.
                pairing.finish_response(true);
            }
            (status, response)
        }
        _ => unreachable!("action is validated above"),
    }
}

/// One admitted connection as the accept loop tracks it: who it came from,
/// when, and whether its worker has seen a valid bearer token yet.
struct ConnectionSlot {
    peer: std::net::IpAddr,
    accepted_at: Instant,
    authenticated: Arc<std::sync::atomic::AtomicBool>,
    /// Set once the sweep has cut this connection, so a slow worker that
    /// has not yet released the slot is not shut down on every tick.
    expired: bool,
}

/// Connection accounting for the accept loop: a global cap, a per-peer cap,
/// and a pre-authentication deadline. Pure bookkeeping — it never touches a
/// socket itself — so the policy is testable without a listener.
pub(crate) struct ConnectionLedger {
    max_total: usize,
    max_per_peer: usize,
    pre_auth_deadline: Duration,
    slots: HashMap<u64, ConnectionSlot>,
    per_peer: HashMap<std::net::IpAddr, usize>,
    last_sweep: Instant,
}

impl ConnectionLedger {
    pub(crate) fn new(max_total: usize, max_per_peer: usize, pre_auth_deadline: Duration) -> Self {
        Self {
            max_total,
            max_per_peer,
            pre_auth_deadline,
            slots: HashMap::new(),
            per_peer: HashMap::new(),
            last_sweep: Instant::now(),
        }
    }

    fn with_defaults() -> Self {
        Self::new(MAX_CONNECTIONS, MAX_CONNECTIONS_PER_PEER, PRE_AUTH_DEADLINE)
    }

    /// Admit `id` from `peer` at `now`, returning the flag the worker flips
    /// once the connection authenticates; `None` means the caps refuse it.
    pub(crate) fn admit(
        &mut self,
        id: u64,
        peer: std::net::IpAddr,
        now: Instant,
    ) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        let peer_count = self.per_peer.get(&peer).copied().unwrap_or(0);
        if self.slots.len() >= self.max_total || peer_count >= self.max_per_peer {
            return None;
        }
        let authenticated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.slots.insert(
            id,
            ConnectionSlot {
                peer,
                accepted_at: now,
                authenticated: Arc::clone(&authenticated),
                expired: false,
            },
        );
        self.per_peer.insert(peer, peer_count + 1);
        Some(authenticated)
    }

    /// The worker for `id` exited; free its global and per-peer share.
    pub(crate) fn release(&mut self, id: u64) {
        let Some(slot) = self.slots.remove(&id) else {
            return;
        };
        if let Some(count) = self.per_peer.get_mut(&slot.peer) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.per_peer.remove(&slot.peer);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.len()
    }

    /// Connections still unauthenticated past the deadline as of `now`, each
    /// reported once. Rate-limited to `LEDGER_SWEEP_INTERVAL` so a flood of
    /// accepts does not turn every iteration into a full scan.
    pub(crate) fn expired(&mut self, now: Instant) -> Vec<u64> {
        if now.saturating_duration_since(self.last_sweep) < LEDGER_SWEEP_INTERVAL {
            return Vec::new();
        }
        self.last_sweep = now;
        let deadline = self.pre_auth_deadline;
        self.slots
            .iter_mut()
            .filter(|(_, slot)| {
                !slot.expired
                    && !slot
                        .authenticated
                        .load(std::sync::atomic::Ordering::Relaxed)
                    && now.saturating_duration_since(slot.accepted_at) >= deadline
            })
            .map(|(id, slot)| {
                slot.expired = true;
                *id
            })
            .collect()
    }
}

/// Refuse a connection the ledger would not admit: a one-line 503 written
/// on the accept thread (a fresh socket's send buffer is empty, so this does
/// not block), then the stream drops and closes. Never spawns a worker.
fn refuse_overloaded(mut stream: TcpStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    respond(&mut stream, 503, &error_body("too many connections"), false);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Cut every connection the ledger reports as expired: `shutdown(Both)` on
/// the control clone interrupts the worker's blocking read, so the worker
/// exits and releases its slot through the normal path.
fn cut_expired_connections(
    ledger: &Mutex<ConnectionLedger>,
    connections: &Mutex<HashMap<u64, TcpStream>>,
) {
    let expired = ledger
        .lock()
        .map(|mut ledger| ledger.expired(Instant::now()))
        .unwrap_or_default();
    if expired.is_empty() {
        return;
    }
    if let Ok(connections) = connections.lock() {
        for id in expired {
            if let Some(stream) = connections.get(&id) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_connection(
    stream: TcpStream,
    tls: Arc<rustls::ServerConfig>,
    snapshot: SharedSnapshot,
    mark_read: Sender<String>,
    hook_port: Option<u16>,
    resizes: MobileResizes,
    approvals: Arc<crate::approvals::ApprovalHub>,
    pairing: Arc<crate::pairing::PairingWindow>,
    platform_adapters: Arc<PlatformAdapterHub>,
    presence: Option<Arc<crate::presence::PresenceHub>>,
    direct_endpoint: Arc<str>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    authenticated: Arc<std::sync::atomic::AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    // The first byte decides the transport; the TLS handshake itself happens
    // on the first read of the wrapped stream.
    let Some((transport, mut stream)) = accept_transport(stream, &tls) else {
        return;
    };
    let mut pending = Vec::new();
    for _ in 0..1000 {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Some(request) = read_request(&mut stream, &mut pending) else {
            return;
        };
        let keep = request.keep_alive;
        // Before routing, before any token lookup: cleartext never carries a
        // credential past this point.
        if let Some((_, body)) = plaintext_credential_rejection(transport, &request) {
            respond_upgrade_required(&mut stream, &body);
            return;
        }
        if !request.path.starts_with("/mobile/") {
            respond(&mut stream, 404, &error_body("not found"), keep);
        } else if request.path == "/mobile/pair" {
            if request.method != "POST" {
                respond(&mut stream, 405, &error_body("method not allowed"), false);
            } else {
                let mac_id = std::fs::read_to_string(mobile_dir().join("mac-id"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let (status, body) = pairing.handle_pair(&request.body, &mac_id, &hostname());
                // Pairing is a one-shot exchange. Force-close even when the
                // URLSession client requested HTTP/1.1 keep-alive so a
                // `pair --serve` handoff never retains this listener's port.
                let sent = respond(&mut stream, status, &body, false);
                if status == 200 {
                    pairing.finish_response(sent);
                }
            }
            return;
        } else if request.method != "GET" && request.method != "POST" {
            respond(&mut stream, 405, &error_body("method not allowed"), keep);
        } else {
            match principal_for_bearer(&request.headers) {
                None => {
                    respond(&mut stream, 401, &error_body("unauthorized"), keep);
                }
                Some(principal) => {
                    authenticated.store(true, std::sync::atomic::Ordering::Relaxed);
                    let (status, body) = handle_authenticated(
                        &request,
                        &principal,
                        &snapshot,
                        &mark_read,
                        hook_port,
                        &resizes,
                        &approvals,
                        &pairing,
                        &direct_endpoint,
                        &platform_adapters,
                        presence.as_deref(),
                        None,
                    );
                    respond(&mut stream, status, &body, keep);
                }
            }
        }
        if !keep {
            return;
        }
    }
}

/// The LAN address a phone can reach us on: first AF_INET interface that
/// isn't loopback/tunnel/awdl/bridge (mirrors `preferredLANAddress`).
pub fn preferred_lan_address() -> String {
    // Portable getifaddrs walk (no `ipconfig` shellout, so this works on
    // Linux hosts too): first running AF_INET interface that isn't loopback
    // or a tunnel/virtual device — same selection rule as the app.
    const SKIP: [&str; 7] = ["lo", "utun", "awdl", "llw", "bridge", "docker", "veth"];
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return "127.0.0.1".into();
    }
    let mut best = None;
    let mut current = list;
    while !current.is_null() {
        let entry = unsafe { &*current };
        current = entry.ifa_next;
        if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
            continue;
        }
        let addr = unsafe { &*entry.ifa_addr };
        if addr.sa_family as i32 != libc::AF_INET {
            continue;
        }
        let flags = entry.ifa_flags as i32;
        if flags & libc::IFF_UP == 0 || flags & libc::IFF_RUNNING == 0 {
            continue;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
            .to_string_lossy()
            .into_owned();
        if SKIP.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let sockaddr: &libc::sockaddr_in =
            unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
        let octets = u32::from_be(sockaddr.sin_addr.s_addr).to_be_bytes();
        if octets[0] == 127 {
            continue;
        }
        best = Some(format!(
            "{}.{}.{}.{}",
            octets[0], octets[1], octets[2], octets[3]
        ));
        break;
    }
    unsafe { libc::freeifaddrs(list) };
    best.unwrap_or_else(|| "127.0.0.1".into())
}

/// Claim the configured endpoint and start its Bonjour advertisement. A known
/// occupied endpoint returns None so the caller can retry the same address;
/// an OS-assigned port is used only for a first run with no configured port,
/// then published atomically before the listener becomes authoritative.
pub fn start(
    snapshot: SharedSnapshot,
    mark_read: Sender<String>,
    hook_port: Option<u16>,
    resizes: MobileResizes,
    approvals: Arc<crate::approvals::ApprovalHub>,
    pairing: Arc<crate::pairing::PairingWindow>,
) -> Option<MobileServer> {
    start_with_platform(
        snapshot,
        mark_read,
        hook_port,
        resizes,
        approvals,
        pairing,
        Arc::new(PlatformAdapterHub::default()),
    )
}

/// Canonical worker form with live native capability injection. The public
/// compatibility `start` above keeps the TUI/pairing callers adapter-free.
#[allow(clippy::too_many_arguments)]
pub fn start_with_platform(
    snapshot: SharedSnapshot,
    mark_read: Sender<String>,
    hook_port: Option<u16>,
    resizes: MobileResizes,
    approvals: Arc<crate::approvals::ApprovalHub>,
    pairing: Arc<crate::pairing::PairingWindow>,
    platform_adapters: Arc<PlatformAdapterHub>,
) -> Option<MobileServer> {
    start_impl(
        snapshot,
        mark_read,
        hook_port,
        resizes,
        approvals,
        pairing,
        platform_adapters,
        None,
    )
}

/// Canonical workspace-worker form. Unlike compatibility frontends, Direct
/// and Link share this one presence authority with the activity engine.
#[allow(clippy::too_many_arguments)]
pub fn start_with_runtime(
    snapshot: SharedSnapshot,
    mark_read: Sender<String>,
    hook_port: Option<u16>,
    resizes: MobileResizes,
    approvals: Arc<crate::approvals::ApprovalHub>,
    pairing: Arc<crate::pairing::PairingWindow>,
    platform_adapters: Arc<PlatformAdapterHub>,
    presence: Arc<crate::presence::PresenceHub>,
) -> Option<MobileServer> {
    start_impl(
        snapshot,
        mark_read,
        hook_port,
        resizes,
        approvals,
        pairing,
        platform_adapters,
        Some(presence),
    )
}

#[allow(clippy::too_many_arguments)]
fn start_impl(
    snapshot: SharedSnapshot,
    mark_read: Sender<String>,
    hook_port: Option<u16>,
    resizes: MobileResizes,
    approvals: Arc<crate::approvals::ApprovalHub>,
    pairing: Arc<crate::pairing::PairingWindow>,
    platform_adapters: Arc<PlatformAdapterHub>,
    presence: Option<Arc<crate::presence::PresenceHub>>,
) -> Option<MobileServer> {
    let dir = mobile_dir();
    let persisted = read_port(&dir.join("server-port"));
    let headless_persisted = read_port(&dir.join("headless-server-port"));
    let previous_tui_owner = read_port(&dir.join("tui-server-port"));
    let handoff = next_start_port().lock().ok().and_then(|guard| *guard);
    // The persisted endpoint is the cross-process ownership claim. A paired
    // Controller will not trust Bonjour to adopt a different plaintext URL,
    // so an occupied known port is retryable ownership loss, never permission
    // to start a second server elsewhere. If no usable endpoint exists, the
    // first TUI binds an OS port and atomically publishes it; concurrent TUIs
    // close their losing listeners and retry the winner's exact endpoint.
    // A released native app can rewrite canonical A→fallback B while a TUI
    // still owns the phone's saved Direct endpoint A. Remembering the active
    // TUI lease prevents a second TUI from claiming B and making that stale
    // rewrite self-sustaining. If the original TUI crashed, its listener is
    // gone and the next TUI reclaims A, then repairs the canonical file.
    let listener = bind_mobile_listener(
        handoff,
        previous_tui_owner.or(persisted),
        headless_persisted,
    )?;
    let port = listener.local_addr().ok()?.port();
    // The Host certificate is the same material the `__remote__` streamer
    // serves, so a Controller's existing pin covers this listener. Without it
    // there is no listener: a cleartext-only server would refuse every paired
    // device anyway (see `plaintext_credential_rejection`).
    let (tls, certificate_fingerprint) = direct_tls_config()?;
    remember_direct_certificate_fingerprint(&certificate_fingerprint);
    let direct_endpoint: Arc<str> =
        format!("http://{}:{port}/mobile", preferred_lan_address()).into();
    if handoff.is_none()
        && persisted.is_none()
        && headless_persisted.is_none()
        && !claim_initial_server_port_at(&dir, port)
    {
        return None;
    }
    if handoff.is_none()
        && previous_tui_owner == Some(port)
        && persisted != Some(port)
        && !restore_server_port_at(&dir, port)
    {
        return None;
    }
    if !publish_tui_owner_port_at(&dir, port) {
        return None;
    }
    if handoff == Some(port) {
        if let Ok(mut guard) = next_start_port().lock() {
            if *guard == Some(port) {
                *guard = None;
            }
        }
    }
    // `stop()` joins the accept loop. Never start it in blocking mode or a
    // rare fcntl failure could make shutdown wait forever with no incoming
    // connection to wake `accept`.
    if listener.set_nonblocking(true).is_err() {
        return None;
    }

    // Bonjour: same service/TXT contract as the app. macOS ships `dns-sd`;
    // Linux hosts use avahi's `avahi-publish-service` when present. Neither
    // is required — the phone's saved endpoint still works, and rediscovery
    // only needs this after an address change.
    let mac_id = std::fs::read_to_string(mobile_dir().join("mac-id"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let name = hostname();
    let bonjour_child = std::process::Command::new("dns-sd")
        .args([
            "-R",
            &name,
            "_unpeel-remote._tcp",
            ".",
            &port.to_string(),
            &format!("macid={mac_id}"),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .or_else(|| {
            std::process::Command::new("avahi-publish-service")
                .args([
                    &name,
                    "_unpeel-remote._tcp",
                    &port.to_string(),
                    &format!("macid={mac_id}"),
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()
        });
    let bonjour = Arc::new(Mutex::new(bonjour_child));
    // The WSS terminal server: standalone, verifies paired-device tokens
    // itself, writes its port + TLS fingerprint into ~/.unpeel/remote.json —
    // which /mobile/bootstrap relays so the phone gets its full terminal
    // (control bar, resize, live stream) instead of the long-poll fallback.
    // The worker supervises it (`remote_streamer.rs`): exit detection,
    // backoff respawn, crash-loop ceiling, retry on pairing change, and an
    // identity-verified reap of a stale instance before every spawn.
    let (remote_streamer, streamer_events) = crate::remote_streamer::RemoteStreamer::start();
    for event in streamer_events {
        crate::tracelog::trace("remote-streamer", &format!("{event:?}"));
    }
    let remote = Arc::new(Mutex::new(remote_streamer));
    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let active_connections = Arc::new(Mutex::new(HashMap::<u64, TcpStream>::new()));
    let worker_threads = Arc::new(Mutex::new(Vec::<std::thread::JoinHandle<()>>::new()));

    let accept_shutdown = Arc::clone(&shutdown);
    let accept_connections = Arc::clone(&active_connections);
    let accept_workers = Arc::clone(&worker_threads);
    // Connection accounting: the global and per-peer caps refuse the excess
    // with a 503 before it earns a thread, and the sweep cuts connections
    // that are still unauthenticated past `PRE_AUTH_DEADLINE` however slowly
    // they paced their bytes.
    let ledger = Arc::new(Mutex::new(ConnectionLedger::with_defaults()));
    let accept_thread = std::thread::spawn(move || loop {
        if accept_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            return; // listener drops here, releasing the port
        }
        cut_expired_connections(&ledger, &accept_connections);
        match listener.accept() {
            Ok((stream, peer)) => {
                let _ = stream.set_nonblocking(false);
                let connection_id = {
                    static NEXT_CONNECTION_ID: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(1);
                    NEXT_CONNECTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                };
                let admitted = ledger
                    .lock()
                    .ok()
                    .and_then(|mut ledger| ledger.admit(connection_id, peer.ip(), Instant::now()));
                let Some(authenticated) = admitted else {
                    refuse_overloaded(stream);
                    continue;
                };
                let tls = Arc::clone(&tls);
                let snapshot = Arc::clone(&snapshot);
                let mark_read = mark_read.clone();
                let resizes = Arc::clone(&resizes);
                let approvals = Arc::clone(&approvals);
                let pairing = Arc::clone(&pairing);
                let platform_adapters = Arc::clone(&platform_adapters);
                let presence = presence.as_ref().map(Arc::clone);
                let direct_endpoint = Arc::clone(&direct_endpoint);
                let connection_shutdown = Arc::clone(&accept_shutdown);
                let connections = Arc::clone(&accept_connections);
                let worker_ledger = Arc::clone(&ledger);
                if let Ok(control) = stream.try_clone() {
                    if let Ok(mut guard) = connections.lock() {
                        guard.insert(connection_id, control);
                    }
                }
                let worker = std::thread::spawn(move || {
                    handle_connection(
                        stream,
                        tls,
                        snapshot,
                        mark_read,
                        hook_port,
                        resizes,
                        approvals,
                        pairing,
                        platform_adapters,
                        presence,
                        direct_endpoint,
                        connection_shutdown,
                        authenticated,
                    );
                    if let Ok(mut guard) = connections.lock() {
                        guard.remove(&connection_id);
                    }
                    if let Ok(mut ledger) = worker_ledger.lock() {
                        ledger.release(connection_id);
                    }
                });
                if let Ok(mut workers) = accept_workers.lock() {
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].is_finished() {
                            let finished = workers.swap_remove(index);
                            let _ = finished.join();
                        } else {
                            index += 1;
                        }
                    }
                    workers.push(worker);
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return,
        }
    });
    Some(MobileServer {
        port,
        certificate_fingerprint,
        shutdown,
        bonjour,
        remote,
        accept_thread: Mutex::new(Some(accept_thread)),
        active_connections,
        worker_threads,
    })
}

pub(crate) fn hostname() -> String {
    let mut buffer = [0u8; 256];
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) };
    if rc == 0 {
        let name = buffer.split(|&b| b == 0).next().unwrap_or(&[]);
        let name = String::from_utf8_lossy(name).into_owned();
        name.trim_end_matches(".local").to_string()
    } else {
        "Mac".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("unpeel-mobile-{label}-{}", uuid::Uuid::new_v4()))
    }

    /// A live process that is NOT the `__remote__` streamer, standing in for
    /// whatever the OS recycled a dead streamer's pid onto.
    fn unrelated_live_process() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep")
    }

    #[test]
    fn remote_advertisement_requires_the_recorded_streamer_identity() {
        let mut stranger = unrelated_live_process();
        let pid = stranger.id();
        let actual_start = unpeel_core::session_host::process_start_time_ms(pid)
            .expect("start time of a live child");
        let home = scratch_dir("remote-advertisement");
        std::fs::create_dir_all(&home).unwrap();
        let write = |pid_started_at: Option<u64>| {
            let mut record = serde_json::json!({
                "pid": pid,
                "port": 45_123,
                "fingerprint": "ab:cd",
            });
            if let Some(started) = pid_started_at {
                record["pid_started_at"] = serde_json::json!(started);
            }
            std::fs::write(home.join("remote.json"), record.to_string()).unwrap();
        };

        // The recorded streamer died and its pid was recycled: nothing to
        // advertise, even though `kill(pid, 0)` would succeed.
        write(Some(actual_start - 3_600_000));
        assert_eq!(remote_server_advertisement_at(&home), (None, None));
        // A record without a start time proves nothing either.
        write(None);
        assert_eq!(remote_server_advertisement_at(&home), (None, None));
        // The live process is the recorded streamer: relay its endpoint.
        write(Some(actual_start));
        assert_eq!(
            remote_server_advertisement_at(&home),
            (Some(45_123), Some("ab:cd".to_string()))
        );

        let _ = stranger.kill();
        let _ = stranger.wait();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn connection_ledger_enforces_caps_and_pre_auth_deadline() {
        let deadline = Duration::from_millis(500);
        let mut ledger = ConnectionLedger::new(3, 2, deadline);
        let t0 = Instant::now();
        let a: std::net::IpAddr = "10.0.0.1".parse().unwrap();
        let b: std::net::IpAddr = "10.0.0.2".parse().unwrap();

        let a1 = ledger.admit(1, a, t0).expect("first from a");
        assert!(ledger.admit(2, a, t0).is_some(), "second from a");
        assert!(
            ledger.admit(3, a, t0).is_none(),
            "per-peer cap refuses a third"
        );
        assert!(ledger.admit(4, b, t0).is_some(), "b still has room");
        assert!(
            ledger.admit(5, b, t0).is_none(),
            "global cap refuses the fourth"
        );
        assert_eq!(ledger.len(), 3);

        // Nothing has expired before the deadline; the first sweep runs.
        assert!(ledger.expired(t0 + Duration::from_millis(200)).is_empty());
        // An authenticated connection outlives the pre-auth deadline.
        a1.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut expired = ledger.expired(t0 + deadline + LEDGER_SWEEP_INTERVAL);
        expired.sort_unstable();
        assert_eq!(expired, vec![2, 4]);
        // Each expiry is reported once.
        assert!(ledger
            .expired(t0 + deadline + 2 * LEDGER_SWEEP_INTERVAL)
            .is_empty());

        // Releasing frees the per-peer and global share.
        ledger.release(2);
        ledger.release(4);
        assert_eq!(ledger.len(), 1);
        assert!(ledger.admit(6, a, t0).is_some());
        assert!(ledger.admit(7, b, t0).is_some());
        assert!(
            ledger.admit(8, b, t0).is_none(),
            "global cap holds after reuse"
        );
    }

    /// End to end on a real listener: N slow unauthenticated clients are
    /// admitted, the (N+1)th is refused with a 503 immediately, and the slow
    /// ones are cut at the pre-auth deadline — after which a new client is
    /// admitted again. Mirrors the production accept loop in `start_impl`
    /// (ledger admit → worker → release, expiry sweep on every iteration).
    #[test]
    fn slow_unauthenticated_connections_are_capped_and_cut_at_the_deadline() {
        const CAP: usize = 4;
        let deadline = Duration::from_millis(500);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ledger = Arc::new(Mutex::new(ConnectionLedger::new(CAP, CAP, deadline)));
        let connections = Arc::new(Mutex::new(HashMap::<u64, TcpStream>::new()));

        let accept_shutdown = Arc::clone(&shutdown);
        let accept_ledger = Arc::clone(&ledger);
        let accept_connections = Arc::clone(&connections);
        let accept_thread = std::thread::spawn(move || {
            let mut workers = Vec::new();
            let mut next_id = 1u64;
            loop {
                if accept_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                cut_expired_connections(&accept_ledger, &accept_connections);
                match listener.accept() {
                    Ok((stream, peer)) => {
                        stream.set_nonblocking(false).unwrap();
                        let id = next_id;
                        next_id += 1;
                        let admitted =
                            accept_ledger
                                .lock()
                                .unwrap()
                                .admit(id, peer.ip(), Instant::now());
                        let Some(authenticated) = admitted else {
                            refuse_overloaded(stream);
                            continue;
                        };
                        accept_connections
                            .lock()
                            .unwrap()
                            .insert(id, stream.try_clone().unwrap());
                        let worker_ledger = Arc::clone(&accept_ledger);
                        let worker_connections = Arc::clone(&accept_connections);
                        let worker_shutdown = Arc::clone(&accept_shutdown);
                        workers.push(std::thread::spawn(move || {
                            let (mark_read, _receiver) = std::sync::mpsc::channel();
                            handle_connection(
                                stream,
                                test_tls_config(),
                                Arc::new(Mutex::new(crate::sessions::MobileSnapshot::default())),
                                mark_read,
                                None,
                                Arc::new(Mutex::new(HashMap::new())),
                                Arc::new(crate::approvals::ApprovalHub::default()),
                                Arc::new(crate::pairing::PairingWindow::default()),
                                Arc::new(PlatformAdapterHub::default()),
                                None,
                                "http://127.0.0.1:17661/mobile".into(),
                                worker_shutdown,
                                authenticated,
                            );
                            worker_connections.lock().unwrap().remove(&id);
                            worker_ledger.lock().unwrap().release(id);
                        }));
                    }
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            for stream in accept_connections.lock().unwrap().values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            for worker in workers {
                let _ = worker.join();
            }
        });

        let connect = || {
            let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client
        };
        // N slow senders, then silence: half a partial plaintext request
        // head, half the first bytes of a TLS ClientHello — a stalled TLS
        // handshake sits under the same pre-auth deadline as a stalled
        // request.
        let opened_at = Instant::now();
        let mut slow: Vec<TcpStream> = (0..CAP)
            .map(|index| {
                let mut client = connect();
                let partial: &[u8] = if index % 2 == 0 {
                    b"GET /mob"
                } else {
                    b"\x16\x03\x01\x00"
                };
                client.write_all(partial).unwrap();
                client
            })
            .collect();
        let admitted_deadline = Instant::now() + Duration::from_secs(5);
        while ledger.lock().unwrap().len() < CAP {
            assert!(
                Instant::now() < admitted_deadline,
                "slow clients were admitted"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        // The (N+1)th is refused promptly, before any slow client expires.
        let refused_at = Instant::now();
        let mut refused = connect();
        let mut response = Vec::new();
        refused.read_to_end(&mut response).unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 503 "), "{response}");
        assert!(response.contains("Connection: close"), "{response}");
        assert!(
            refused_at.elapsed() < Duration::from_secs(2),
            "refusal was immediate, not queued behind the slow clients"
        );

        // The slow ones are cut at the deadline: each read ends (EOF or
        // reset) well before the 30 s per-read timeout would.
        for client in &mut slow {
            let mut buffer = [0u8; 64];
            match client.read(&mut buffer) {
                Ok(0) | Err(_) => {}
                Ok(n) => panic!("unexpected bytes for a slow client: {:?}", &buffer[..n]),
            }
        }
        let cut_after = opened_at.elapsed();
        assert!(
            cut_after >= deadline,
            "slow clients were not cut before the deadline ({cut_after:?})"
        );
        assert!(
            cut_after < Duration::from_secs(4),
            "slow clients were cut near the deadline, not the read timeout ({cut_after:?})"
        );

        // Their workers exit and release their share: a new client is served.
        let released_deadline = Instant::now() + Duration::from_secs(5);
        while ledger.lock().unwrap().len() > 0 {
            assert!(Instant::now() < released_deadline, "slots were released");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut fresh = connect();
        fresh
            .write_all(b"GET /elsewhere HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        fresh.read_to_end(&mut response).unwrap();
        assert!(String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 404 "));

        shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
        accept_thread.join().unwrap();
    }

    /// A Host certificate generated into a scratch directory through the real
    /// material loader — never this process's `~/.unpeel`. Returns the server
    /// config and the fingerprint a Controller would pin.
    fn test_tls_material() -> (Arc<rustls::ServerConfig>, String) {
        let dir = scratch_dir("tls");
        let material = unpeel_core::remote_server::ensure_tls_material_in(&dir)
            .expect("scratch Host certificate");
        let fingerprint = material.fingerprint.clone();
        let config =
            unpeel_core::remote_server::build_tls_config(material).expect("scratch TLS config");
        let _ = std::fs::remove_dir_all(dir);
        (config, fingerprint)
    }

    fn test_tls_config() -> Arc<rustls::ServerConfig> {
        test_tls_material().0
    }

    /// Accept one connection and run it through the production handler.
    fn serve_one_connection(
        listener: TcpListener,
        tls: Arc<rustls::ServerConfig>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let snapshot = Arc::new(Mutex::new(crate::sessions::MobileSnapshot::default()));
            let (mark_read, _receiver) = std::sync::mpsc::channel();
            handle_connection(
                stream,
                tls,
                snapshot,
                mark_read,
                None,
                Arc::new(Mutex::new(HashMap::new())),
                Arc::new(crate::approvals::ApprovalHub::default()),
                Arc::new(crate::pairing::PairingWindow::default()),
                Arc::new(PlatformAdapterHub::default()),
                None,
                "http://127.0.0.1:17661/mobile".into(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            );
        })
    }

    /// A Controller-side TLS stream pinned the way the phone pins: to the
    /// certificate fingerprint, not a CA chain.
    fn tls_client(
        port: u16,
        fingerprint: Option<String>,
    ) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
        let config = Arc::new(unpeel_core::remote_attach::pinned_client_config(
            fingerprint,
        ));
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let connection = rustls::ClientConnection::new(config, name).unwrap();
        let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        rustls::StreamOwned::new(connection, tcp)
    }

    /// Send one request, read until the server closes: (status, head, body).
    fn http_exchange<S: Read + Write>(stream: &mut S, request: &str) -> (u16, String, String) {
        // A refused certificate surfaces here: the handshake runs on the
        // first write and fails before any HTTP byte crosses.
        if let Err(error) = stream
            .write_all(request.as_bytes())
            .and_then(|_| stream.flush())
        {
            return (0, String::new(), format!("write failed: {error}"));
        }
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
            }
        }
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        (status, head.to_owned(), body.to_owned())
    }

    fn request_with_headers(path: &str, headers: &[(&str, &str)]) -> Request {
        Request {
            request_id: None,
            method: "GET".into(),
            path: path.into(),
            query: HashMap::new(),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: Vec::new(),
            keep_alive: true,
        }
    }

    #[test]
    fn first_byte_classifies_the_transport() {
        assert_eq!(transport_for_first_byte(0x16), Transport::Tls);
        for byte in [b'G', b'P', b'H', b'O', b'D', 0x00, 0xff] {
            assert_eq!(transport_for_first_byte(byte), Transport::Plaintext);
        }
    }

    #[test]
    fn plaintext_gate_refuses_credentials_and_passes_tls_through() {
        let bearer = [("authorization", "Bearer phone-token-1")];
        let (status, body) = plaintext_credential_rejection(
            Transport::Plaintext,
            &request_with_headers("/mobile/bootstrap", &bearer),
        )
        .expect("plaintext + bearer is refused");
        assert_eq!(status, 426);
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["error"], "use https");

        // Any credential shape, any route — even pairing — is refused in the
        // clear; the pairing exchange itself carries no Authorization header.
        assert!(plaintext_credential_rejection(
            Transport::Plaintext,
            &request_with_headers("/mobile/pair", &[("authorization", "Basic abc")]),
        )
        .is_some());

        // TLS carries the bearer on to authentication.
        assert!(plaintext_credential_rejection(
            Transport::Tls,
            &request_with_headers("/mobile/bootstrap", &bearer),
        )
        .is_none());
        // Unauthenticated plaintext requests keep their existing answers
        // (the pairing route, or 401/404 further down).
        assert!(plaintext_credential_rejection(
            Transport::Plaintext,
            &request_with_headers("/mobile/pair", &[]),
        )
        .is_none());
    }

    #[test]
    fn plaintext_bearer_is_refused_before_authentication() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = serve_one_connection(listener, test_tls_config());

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (status, head, body) = http_exchange(
            &mut client,
            "GET /mobile/bootstrap HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer phone-token-1\r\nConnection: keep-alive\r\n\r\n",
        );
        handler.join().unwrap();

        // 426, never 401: the token was refused for its transport, not looked
        // up and found unknown.
        assert_eq!(status, 426, "{head}\n{body}");
        assert!(head.contains("Upgrade: TLS/1.3"), "{head}");
        assert!(head.contains("Connection: close"), "{head}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["error"], "use https");
    }

    #[test]
    fn tls_carries_the_bearer_past_the_plaintext_gate() {
        let (tls, fingerprint) = test_tls_material();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = serve_one_connection(listener, tls);

        // Pinned to the served certificate, exactly as a paired phone pins.
        // A non-/mobile path answers 404 only after the gate let the bearer
        // through; the same request in the clear ends in 426 above. (The
        // paired-token lookup itself is exercised by the process tests with
        // a private UNPEEL_HOME.)
        let mut client = tls_client(port, Some(fingerprint));
        let (status, head, body) = http_exchange(
            &mut client,
            "GET /not-mobile HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer phone-token-1\r\nConnection: close\r\n\r\n",
        );
        handler.join().unwrap();
        assert_eq!(status, 404, "{head}\n{body}");
        assert!(body.contains("not found"), "{body}");
    }

    #[test]
    fn pairing_route_answers_on_both_transports() {
        let (tls, fingerprint) = test_tls_material();
        let pair = "POST /mobile/pair HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";

        // Over TLS: the sealed exchange reaches the pairing window.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = serve_one_connection(listener, Arc::clone(&tls));
        let mut client = tls_client(port, Some(fingerprint));
        let (status, _, body) = http_exchange(&mut client, pair);
        handler.join().unwrap();
        assert_eq!(status, 401, "{body}");
        assert!(body.contains("pairing is not active"), "{body}");

        // In the clear: the QR-code pairing client still reaches it, since
        // the exchange is sealed at the application layer.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = serve_one_connection(listener, tls);
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (status, _, body) = http_exchange(&mut client, pair);
        handler.join().unwrap();
        assert_eq!(status, 401, "{body}");
        assert!(body.contains("pairing is not active"), "{body}");
    }

    #[test]
    fn a_wrong_pin_never_completes_the_handshake() {
        let (tls, _) = test_tls_material();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = serve_one_connection(listener, tls);
        let mut client = tls_client(port, Some("00".repeat(32)));
        let (status, _, body) = http_exchange(
            &mut client,
            "GET /mobile/pair HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        handler.join().unwrap();
        assert_eq!(
            status, 0,
            "no HTTP answer crosses a rejected certificate: {body}"
        );
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ConformanceFixture {
        schema_version: u16,
        cases: Vec<ConformanceCase>,
    }

    #[derive(serde::Deserialize)]
    struct ConformanceCase {
        id: String,
        method: String,
        path: String,
        #[serde(default)]
        query: HashMap<String, String>,
        #[serde(default)]
        body: serde_json::Value,
        expected: ConformanceExpected,
    }

    #[derive(serde::Deserialize)]
    struct ConformanceExpected {
        tui: u16,
    }

    #[test]
    fn sha256_known_answer() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn bearer_parser_rejects_malformed_unicode_without_slicing() {
        assert_eq!(bearer_token("Bearer token"), Some("token"));
        assert_eq!(bearer_token("bearer   token"), Some("token"));
        assert_eq!(bearer_token("Bærer token"), None);
        assert_eq!(bearer_token("Bearer "), None);
    }

    #[test]
    fn paired_port_preserves_existing_native_canonical_endpoint() {
        let dir = scratch_dir("canonical-port");
        std::fs::create_dir_all(&dir).unwrap();
        let original = b"41234\n";
        std::fs::write(dir.join("server-port"), original).unwrap();

        persist_paired_port_at(&dir, 42345);

        assert_eq!(std::fs::read(dir.join("server-port")).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(dir.join("headless-server-port")).unwrap(),
            "42345\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn persisted_headless_port_rebinds_after_process_style_restart() {
        let _ports = serialized_ports();
        on_a_reclaimable_port(
            "the headless record alone must restore the endpoint",
            |probe| {
                let dir = scratch_dir("headless-restart");
                let _ = std::fs::remove_dir_all(&dir);
                let port = probe.local_addr().unwrap().port();
                drop(probe);
                persist_paired_port_at(&dir, port);
                // Simulate an older/separate headless installation with no native
                // canonical file: the headless record alone must restore the endpoint.
                std::fs::remove_file(dir.join("server-port")).unwrap();

                let restored = read_port(&dir.join("headless-server-port"));
                let Some(rebound) = bind_mobile_listener(None, None, restored) else {
                    let _ = std::fs::remove_dir_all(&dir);
                    return None;
                };

                assert_eq!(configured_server_port_at(&dir), Some(port));
                assert_eq!(rebound.local_addr().unwrap().port(), port);
                drop(rebound);
                let _ = std::fs::remove_dir_all(dir);
                Some(())
            },
        );
    }

    /// Port-using tests in this module run one at a time — across threads
    /// AND across processes: they bind real `0.0.0.0` sockets, and several
    /// release a port and expect to reclaim it, which only holds while no
    /// other test (a parallel `cargo test -p unpeel-serve` next to a
    /// workspace run, say) is grabbing ephemeral ports. The in-process mutex
    /// covers threads; the exclusive flock on a well-known temp file covers
    /// sibling test processes. Both are released on drop, and by the OS if a
    /// process dies mid-test.
    static PORT_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct PortTestGuard {
        _threads: std::sync::MutexGuard<'static, ()>,
        _processes: std::fs::File,
    }

    fn serialized_ports() -> PortTestGuard {
        use std::os::fd::AsRawFd;
        let threads = PORT_TESTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A FIXED path, not `std::env::temp_dir()`: that honours each
        // process's own TMPDIR, and two test processes with different TMPDIRs
        // (a staging script exporting its own, a harness that scopes one)
        // would never share the lock. /tmp is the one place every unix
        // process of this user agrees on.
        let path = std::path::PathBuf::from("/tmp/unpeel-serve-port-tests.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .expect("open the port-test lock file");
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) },
            0,
            "flock {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        // Test binaries inherit a 256-descriptor soft limit on macOS; a
        // loaded suite can exhaust it and `socket()` then fails with EMFILE,
        // which looks exactly like a bind failure. Raise it to the hard
        // limit once so descriptor pressure never masquerades as a port race.
        unsafe {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0
                && limit.rlim_cur < limit.rlim_max
            {
                limit.rlim_cur = limit.rlim_max.min(65_536);
                let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
            }
        }
        PortTestGuard {
            _threads: threads,
            _processes: file,
        }
    }

    /// Release-then-reclaim scenarios can only be proven on a port nothing
    /// else takes in between. The guard above keeps sibling PORT tests off
    /// it, but other test binaries in a workspace run (unpeel-core,
    /// unpeel-host process tests) and anything else on the machine bind `:0`
    /// freely and may be handed a just-released port. So a failed reclaim is
    /// treated as an invalidated premise — `scenario` returns `None` — and the
    /// scenario is retried on a fresh `:0` port, bounded. No second probe
    /// decides whether the steal was "real": a probe is itself a race (the
    /// thief may have let go already). A genuine product regression fails
    /// every attempt and still surfaces, with `expectation` in the message.
    ///
    /// The port is a RANDOM one outside every OS ephemeral range (macOS
    /// 49152–65535, Linux 32768–60999), proven free by binding it first:
    /// `:0` binders anywhere on the machine can never be handed it, so a
    /// release-then-reclaim only competes with an explicit bind of that
    /// exact number — vanishingly rare, and retried. A kernel-chosen `:0`
    /// port was stolen on 12 consecutive attempts under a two-checkout
    /// test storm (2026-09-03), which is why this is not simply `:0`.
    fn on_a_reclaimable_port<T>(
        expectation: &str,
        mut scenario: impl FnMut(TcpListener) -> Option<T>,
    ) -> T {
        const ATTEMPTS: usize = 12;
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            ^ (std::process::id() as u64).rotate_left(32);
        let mut next_free_port = || loop {
            // xorshift: cheap, dependency-free; only spread matters here.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let port = 20_000 + (seed % 10_000) as u16;
            if let Ok(listener) = TcpListener::bind(("0.0.0.0", port)) {
                return listener;
            }
        };
        let mut failures = Vec::new();
        for _ in 0..ATTEMPTS {
            let listener = next_free_port();
            let port = listener.local_addr().unwrap().port();
            if let Some(outcome) = scenario(listener) {
                return outcome;
            }
            // bind_reusable_ipv4_listener closes its fd after the failing
            // call; close() does not touch errno, so this is the real cause.
            let errno = std::io::Error::last_os_error();
            // Who holds it right now (diagnostic only; lsof is optional).
            let holders = std::process::Command::new("lsof")
                .args(["-nP", &format!("-iTCP:{port}")])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            failures.push(format!("{port}: {errno}; holders: [{holders}]"));
        }
        panic!(
            "{expectation}: the exact port could not be reclaimed on any of {ATTEMPTS} random \
non-ephemeral ports — a product regression, not a port race. Attempts: {failures:?}"
        );
    }

    #[test]
    fn occupied_exact_handoff_fails_closed() {
        let _ports = serialized_ports();
        let occupied = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();

        assert!(bind_mobile_listener(Some(port), None, None).is_none());
    }

    #[test]
    fn occupied_persisted_endpoint_waits_for_the_same_port() {
        let _ports = serialized_ports();
        on_a_reclaimable_port(
            "the released persisted endpoint must become claimable",
            |occupied| {
                let port = occupied.local_addr().unwrap().port();
                assert!(bind_mobile_listener(None, Some(port), None).is_none());
                drop(occupied);
                let claimed = bind_mobile_listener(None, Some(port), None)?;
                assert_eq!(claimed.local_addr().unwrap().port(), port);
                Some(())
            },
        );
    }

    #[test]
    fn active_tui_owner_blocks_a_stale_legacy_fallback_claim() {
        let _ports = serialized_ports();
        on_a_reclaimable_port("the released owner port must be reclaimable", |direct| {
            let dir = scratch_dir("tui-owner-port");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let direct_port = direct.local_addr().unwrap().port();
            // The legacy fallback endpoint is HELD for the whole scenario, so
            // "the binder never claims it" is observable without ever
            // releasing and re-binding a port.
            let fallback_held = TcpListener::bind(("0.0.0.0", 0)).unwrap();
            let fallback_port = fallback_held.local_addr().unwrap().port();
            std::fs::write(dir.join("server-port"), format!("{fallback_port}\n")).unwrap();
            assert!(publish_tui_owner_port_at(&dir, direct_port));

            let canonical = read_port(&dir.join("server-port"));
            let owner = read_port(&dir.join("tui-server-port"));
            assert_eq!(owner, Some(direct_port));
            // Exact-or-nothing on the owner's port: no listener while the
            // owner holds it, and no fall back to the legacy endpoint.
            assert!(bind_mobile_listener(None, owner.or(canonical), None).is_none());

            drop(direct);
            let Some(reclaimed) = bind_mobile_listener(None, owner.or(canonical), None) else {
                let _ = std::fs::remove_dir_all(&dir);
                return None;
            };
            assert_eq!(reclaimed.local_addr().unwrap().port(), direct_port);
            assert!(restore_server_port_at(&dir, direct_port));
            assert_eq!(read_port(&dir.join("server-port")), Some(direct_port));
            clear_tui_owner_port_at(&dir, direct_port);
            assert!(!dir.join("tui-server-port").exists());
            drop(reclaimed);
            drop(fallback_held);
            let _ = std::fs::remove_dir_all(dir);
            Some(())
        });
    }

    #[test]
    fn stopping_an_old_tui_cannot_clear_a_newer_owner() {
        let dir = scratch_dir("tui-owner-compare-delete");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(publish_tui_owner_port_at(&dir, 41_001));
        assert!(publish_tui_owner_port_at(&dir, 41_002));

        clear_tui_owner_port_at(&dir, 41_001);

        assert_eq!(read_port(&dir.join("tui-server-port")), Some(41_002));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn first_dynamic_endpoint_has_one_durable_winner() {
        let dir = scratch_dir("initial-port-race");
        std::fs::create_dir_all(&dir).unwrap();
        let first = bind_mobile_listener(None, None, None).unwrap();
        let second = bind_mobile_listener(None, None, None).unwrap();
        let first_port = first.local_addr().unwrap().port();
        let second_port = second.local_addr().unwrap().port();
        assert_ne!(first_port, second_port);

        assert!(claim_initial_server_port_at(&dir, first_port));
        assert!(!claim_initial_server_port_at(&dir, second_port));
        assert_eq!(configured_server_port_at(&dir), Some(first_port));
        assert_eq!(
            std::fs::read_to_string(dir.join("server-port")).unwrap(),
            format!("{first_port}\n")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_initial_endpoint_is_atomically_replaced() {
        let dir = scratch_dir("corrupt-initial-port");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server-port"), b"not-a-port\n").unwrap();
        let listener = bind_mobile_listener(None, None, None).unwrap();
        let port = listener.local_addr().unwrap().port();

        assert!(claim_initial_server_port_at(&dir, port));
        assert_eq!(configured_server_port_at(&dir), Some(port));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_native_random_fallback_cannot_replace_owned_direct_endpoint() {
        let dir = scratch_dir("legacy-native-port-repair");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server-port"), b"41234\n").unwrap();

        assert!(restore_server_port_at(&dir, 42345));
        assert_eq!(read_port(&dir.join("server-port")), Some(42345));
        assert_eq!(
            std::fs::read_to_string(dir.join("server-port")).unwrap(),
            "42345\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pairing_route_closes_keep_alive_socket_before_exact_rebind() {
        let _ports = serialized_ports();
        on_a_reclaimable_port(
            "the one-shot pairing worker released the exact endpoint",
            |listener| {
                let port = listener.local_addr().unwrap().port();
                let handler = std::thread::spawn(move || {
                    let (stream, _) = listener.accept().unwrap();
                    let snapshot = Arc::new(Mutex::new(crate::sessions::MobileSnapshot::default()));
                    let (mark_read, _receiver) = std::sync::mpsc::channel();
                    handle_connection(
                        stream,
                        test_tls_config(),
                        snapshot,
                        mark_read,
                        None,
                        Arc::new(Mutex::new(HashMap::new())),
                        Arc::new(crate::approvals::ApprovalHub::default()),
                        Arc::new(crate::pairing::PairingWindow::default()),
                        Arc::new(PlatformAdapterHub::default()),
                        None,
                        "http://127.0.0.1:17661/mobile".into(),
                        Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    );
                });
                let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                client
            .write_all(
                b"GET /mobile/pair HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n",
            )
            .unwrap();
                let mut response = Vec::new();
                client.read_to_end(&mut response).unwrap();
                handler.join().unwrap();

                let response = String::from_utf8(response).unwrap();
                assert!(response.contains("HTTP/1.1 405"));
                assert!(response.contains("Connection: close"));
                let rebound = bind_mobile_listener(Some(port), None, None)?;
                assert_eq!(rebound.local_addr().unwrap().port(), port);
                Some(())
            },
        );
    }

    #[test]
    fn stop_joins_accept_loop_before_exact_rebind() {
        let _ports = serialized_ports();
        on_a_reclaimable_port(
            "stop returned only after the listener released its exact port",
            |listener| {
                let port = listener.local_addr().unwrap().port();
                listener.set_nonblocking(true).unwrap();
                let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let accept_shutdown = Arc::clone(&shutdown);
                let accept_thread = std::thread::spawn(move || loop {
                    if accept_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    match listener.accept() {
                        Ok(_) => {}
                        Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                });
                let server = MobileServer {
                    port,
                    certificate_fingerprint: String::new(),
                    shutdown,
                    bonjour: Arc::new(Mutex::new(None)),
                    remote: Arc::new(Mutex::new(
                        crate::remote_streamer::RemoteStreamer::stopped_for_tests(),
                    )),
                    accept_thread: Mutex::new(Some(accept_thread)),
                    active_connections: Arc::new(Mutex::new(HashMap::new())),
                    worker_threads: Arc::new(Mutex::new(Vec::new())),
                };

                server.stop();
                let rebound = bind_mobile_listener(Some(port), None, None)?;
                assert_eq!(rebound.local_addr().unwrap().port(), port);
                Some(())
            },
        );
    }

    #[test]
    fn base64_known_answer() {
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn native_artifact_enrichment_requires_the_exact_bounded_chunk_contract() {
        let query = HashMap::from([
            ("session_id".into(), "s1".into()),
            ("kind".into(), "screenshots".into()),
            ("name".into(), "shot.png".into()),
            ("limit".into(), "3".into()),
            ("max_dim".into(), "128".into()),
        ]);
        let valid = serde_json::json!({
            "sessionID": "s1",
            "kind": "screenshots",
            "name": "shot.png",
            "contentType": "image/jpeg",
            "offset": 0,
            "nextOffset": 3,
            "totalSize": 3,
            "dataBase64": "dGh1",
            "capturedAtUnixMs": 1,
        });
        assert!(valid_native_artifact_chunk(&valid, &query));

        let mut wrong_session = valid.clone();
        wrong_session["sessionID"] = "other".into();
        assert!(!valid_native_artifact_chunk(&wrong_session, &query));
        let mut oversized = valid.clone();
        oversized["dataBase64"] = "dGh1bWI=".into();
        oversized["nextOffset"] = 5.into();
        oversized["totalSize"] = 5.into();
        assert!(!valid_native_artifact_chunk(&oversized, &query));
        let mut unknown = valid;
        unknown["secret"] = true.into();
        assert!(!valid_native_artifact_chunk(&unknown, &query));
    }

    #[test]
    fn boundary_withholds_partial_escape() {
        assert_eq!(last_safe_boundary(b"hello \x1b[31m red"), 15);
        assert_eq!(last_safe_boundary(b"hello \x1b[3"), 6);
        assert_eq!(last_safe_boundary(b"plain"), 5);
        // partial utf-8 tail withheld
        assert_eq!(last_safe_boundary(&[b'a', 0xe2, 0x82]), 1);
    }

    #[test]
    fn live_platform_adapter_owns_push_and_relay_credential_routes() {
        let _ports = serialized_ports();
        fn read_request(stream: &mut TcpStream) -> serde_json::Value {
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
                        (request.len() >= separator + 4 + content_length)
                            .then_some((separator, content_length))
                    });
                if let Some((separator, content_length)) = complete {
                    return serde_json::from_slice(
                        &request[separator + 4..separator + 4 + content_length],
                    )
                    .unwrap();
                }
            }
        }

        fn respond(stream: &mut TcpStream, body: &serde_json::Value) {
            let body = body.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }

        let missing = PlatformAdapterHub::default();
        let paired = ControllerPrincipal::PairedDevice {
            device_id: "phone-1".into(),
            name: "iPhone".into(),
            principal_id: None,
        };
        let invalid = Request {
            request_id: Some("push-invalid".into()),
            method: "POST".into(),
            path: "/mobile/push-token".into(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: br#"{"apnsToken":"no","environment":"sandbox"}"#.to_vec(),
            keep_alive: false,
        };
        assert_eq!(handle_push_registration(&invalid, &paired, &missing).0, 404);

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let server_captured = Arc::clone(&captured);
        let server = std::thread::spawn(move || {
            for response in [
                serde_json::json!({ "ok": true }),
                serde_json::json!({
                    "relayURL": "wss://relay.example.test/v1",
                    "macID": "host-1",
                    "relayToken": "rotated-token",
                    "e2eKeyB64": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                }),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                server_captured.lock().unwrap().push(request);
                respond(&mut stream, &response);
            }
        });
        let hub = PlatformAdapterHub::default();
        hub.register(
            9,
            crate::platform_adapter::PlatformAdapterRegistration {
                version: crate::platform_adapter::PLATFORM_ADAPTER_VERSION,
                instance_id: "native-mobile-test".into(),
                callback_port: port,
                callback_token: "0123456789abcdef0123456789abcdef".into(),
                capabilities: vec!["push.register".into(), "relay.credentials.recover".into()],
            },
        )
        .unwrap();

        assert_eq!(handle_push_registration(&invalid, &paired, &hub).0, 400);
        let valid = Request {
            request_id: Some("push-valid".into()),
            body: br#"{"apnsToken":" 0011223344556677 ","environment":"unexpected"}"#.to_vec(),
            ..invalid
        };
        assert_eq!(handle_push_registration(&valid, &paired, &hub).0, 200);
        let (status, body) = handle_relay_credentials_recovery(&paired, &hub);
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["relayToken"],
            "rotated-token"
        );
        server.join().unwrap();

        let captured = captured.lock().unwrap();
        assert_eq!(captured[0]["operation"], "push.register");
        assert_eq!(captured[0]["request"]["deviceID"], "phone-1");
        assert_eq!(captured[0]["request"]["apnsToken"], "0011223344556677");
        assert_eq!(captured[0]["request"]["environment"], "sandbox");
        assert_eq!(captured[1]["operation"], "relay.credentials.recover");
        assert_eq!(captured[1]["request"]["deviceID"], "phone-1");
    }

    #[test]
    fn relay_credential_recovery_replays_a_fresh_mint_instead_of_rotating_again() {
        let _ports = serialized_ports();
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls = Arc::new(Mutex::new(0usize));
        let server_calls = Arc::clone(&calls);
        let server = std::thread::spawn(move || {
            // Exactly one mint is served; a second adapter call would hang
            // the test instead of silently rotating the device again.
            let (mut stream, _) = listener.accept().unwrap();
            let mut pending = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                pending.extend_from_slice(&buffer[..count]);
                let Some(separator) = pending.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&pending[..separator]).to_string();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if pending.len() >= separator + 4 + length {
                    break;
                }
            }
            *server_calls.lock().unwrap() += 1;
            let body = serde_json::json!({
                "relayURL": "wss://relay.example.test/v1",
                "macID": "host-1",
                "relayToken": "minted-once",
                "e2eKeyB64": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let hub = PlatformAdapterHub::default();
        hub.register(
            11,
            crate::platform_adapter::PlatformAdapterRegistration {
                version: crate::platform_adapter::PLATFORM_ADAPTER_VERSION,
                instance_id: "native-recovery-replay".into(),
                callback_port: port,
                callback_token: "0123456789abcdef0123456789abcdef".into(),
                capabilities: vec!["relay.credentials.recover".into()],
            },
        )
        .unwrap();
        let paired = ControllerPrincipal::PairedDevice {
            device_id: "phone-recovery-replay".into(),
            name: "iPhone".into(),
            principal_id: None,
        };
        clear_recent_recoveries();
        let (first_status, first_body) = handle_relay_credentials_recovery(&paired, &hub);
        assert_eq!(first_status, 200);
        let (second_status, second_body) = handle_relay_credentials_recovery(&paired, &hub);
        assert_eq!(second_status, 200);
        assert_eq!(
            first_body, second_body,
            "the retry receives the credential already minted"
        );
        server.join().unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "only one mint reaches the native adapter"
        );

        // Another device is never served a neighbour's credential.
        let other = ControllerPrincipal::PairedDevice {
            device_id: "phone-recovery-other".into(),
            name: "iPad".into(),
            principal_id: None,
        };
        let (other_status, _) = handle_relay_credentials_recovery(&other, &hub);
        assert_ne!(
            other_status, 200,
            "a different device must reach the adapter itself"
        );
        clear_recent_recoveries();
    }

    #[test]
    fn headless_adapter_runs_the_shared_conformance_fixture() {
        let fixture: ConformanceFixture =
            serde_json::from_str(include_str!("../../../protocol/host-conformance-v1.json"))
                .expect("valid host conformance fixture");
        assert_eq!(fixture.schema_version, 1);

        let snapshot = Arc::new(Mutex::new(crate::sessions::MobileSnapshot {
            bootstrap: serde_json::json!({
                "macID": "conformance-host",
                "macName": "Conformance Host",
                "folders": [],
                "projects": [{ "id": "conformance-project" }],
                "presets": [{
                    "id": "conformance-preset", "label": "Claude",
                    "command": "claude", "enabled": true,
                    "quickLaunch": false, "isDefault": false
                }],
                "sessions": [
                    { "id": "conformance-session" },
                    { "id": "conformance-restart" },
                    { "id": "conformance-stop-live" },
                    { "id": "conformance-stop-exited" },
                    { "id": "conformance-action-restart" },
                    { "id": "conformance-action-restart-agent" },
                    { "id": "conformance-restart-agent-exited" },
                    { "id": "conformance-action-resume-agent" },
                    { "id": "conformance-resume-agent-exited" },
                    { "id": "conformance-remove" },
                ],
            }),
            archived_sessions_by_project: HashMap::from([(
                "conformance-project".into(),
                Vec::new(),
            )]),
            create_presets: Vec::new(),
        }));
        let (mark_read, _receiver) = std::sync::mpsc::channel();
        let resizes = Arc::new(Mutex::new(HashMap::new()));
        let approvals = Arc::new(crate::approvals::ApprovalHub::default());
        let principal = ControllerPrincipal::OwnerTransport {
            transport: "conformance".into(),
            subject: None,
            principal_id: None,
        };
        let effects = ControllerEffects::new(Arc::new(|request| {
            use unpeel_core::controller_api::{ControllerEffectError, ControllerSessionAction};
            match (request.session_id.as_str(), request.action) {
                ("conformance-restart", ControllerSessionAction::Restart)
                | ("conformance-stop-live", ControllerSessionAction::Stop)
                | ("conformance-action-restart", ControllerSessionAction::Restart)
                | ("conformance-action-restart-agent", ControllerSessionAction::RestartAgent)
                | ("conformance-action-resume-agent", ControllerSessionAction::ResumeAgent)
                | ("conformance-remove", ControllerSessionAction::Remove) => Ok(()),
                ("conformance-stop-exited", ControllerSessionAction::Stop)
                | ("conformance-restart-agent-exited", ControllerSessionAction::RestartAgent)
                | ("conformance-resume-agent-exited", ControllerSessionAction::ResumeAgent) => {
                    Err(ControllerEffectError::SessionNotRunning)
                }
                ("conformance-broken", _) => Err(ControllerEffectError::Failed(
                    "conformance lifecycle failure".into(),
                )),
                ("conformance-unknown", _) => Err(ControllerEffectError::UnknownSession),
                _ => Err(ControllerEffectError::Failed(
                    "unexpected conformance lifecycle request".into(),
                )),
            }
        }));
        let pairing = crate::pairing::PairingWindow::default();
        let platform_adapters = PlatformAdapterHub::default();

        for case in fixture.cases {
            let request = Request {
                request_id: Some(format!("conformance-{}", case.id)),
                method: case.method,
                path: case.path,
                query: case.query,
                headers: HashMap::new(),
                body: if case.body.is_null() {
                    Vec::new()
                } else {
                    serde_json::to_vec(&case.body).expect("fixture body")
                },
                keep_alive: false,
            };
            let (status, body) = handle_authenticated_with_effects(
                &request,
                &principal,
                &snapshot,
                &mark_read,
                None,
                &resizes,
                &approvals,
                Some(&pairing),
                Some("http://127.0.0.1:17661/mobile"),
                &platform_adapters,
                None,
                Some(&effects),
                None,
            );
            assert_eq!(status, case.expected.tui, "conformance case {}", case.id);
            if case.id == "bootstrap.valid" {
                let response: serde_json::Value =
                    serde_json::from_str(&body).expect("bootstrap response json");
                assert_eq!(
                    response.get("hostProtocol"),
                    Some(
                        &serde_json::to_value(
                            unpeel_core::controller_protocol::HostProtocolDescriptor::headless_v1()
                        )
                        .expect("descriptor json")
                    )
                );
                // Both TLS signals the phone reads: the capability id, and
                // the Host version as the fallback (`>= 0.5.3` means TLS).
                assert!(
                    response["hostProtocol"]["capabilities"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|id| id == "host.mobile.tls")),
                    "bootstrap advertises host.mobile.tls"
                );
                assert_eq!(response["serverVersion"], env!("CARGO_PKG_VERSION"));
                // Lane D additive fields: the isolation tier is always
                // present and one of the three values; the environment is
                // optional and, when present, a Box descriptor.
                let tier = response
                    .get("hostIsolationTier")
                    .and_then(serde_json::Value::as_str);
                assert!(
                    matches!(tier, Some("vm" | "container" | "host")),
                    "hostIsolationTier must be present and valid, got {tier:?}"
                );
                if let Some(environment) = response.get("hostEnvironment") {
                    assert_eq!(
                        environment.get("kind").and_then(|v| v.as_str()),
                        Some("box")
                    );
                    assert!(environment
                        .get("id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| !id.is_empty()));
                }
            }
            if case.id == "archive.known-empty" {
                let response: serde_json::Value =
                    serde_json::from_str(&body).expect("archive response json");
                assert_eq!(
                    response,
                    serde_json::json!({
                        "projectID": "conformance-project",
                        "sessions": [],
                    })
                );
            }
        }
    }
}
