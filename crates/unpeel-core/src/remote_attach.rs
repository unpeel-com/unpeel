//! Remote attach client: `unpeel-host __remote_attach__`.
//!
//! Bridges the current terminal's stdio to a session hosted by ANOTHER
//! Unpeel's `__remote__` server — the network twin of `unpeel-attach` (which
//! bridges to a local session over its Unix socket). Because it is a plain
//! stdio program, any terminal can run it, including a session pane inside
//! a second Unpeel app: that is the "Unpeel controlling Unpeel" path, with
//! zero new rendering code on the client side.
//!
//!   unpeel-host __remote_attach__ --url https://mac:55280 --token T \
//!       [--fingerprint SHA256HEX] <session-id>
//!
//! With --url omitted it reads `~/.unpeel/remote.json` (attach through this
//! machine's own server — the loopback demo).
//!
//! Transport: the server's plain-GET long-poll (`/output?offset&wait_ms`)
//! for output and `POST /write` for raw input. One TLS connection per
//! request (the server is Connection: close); input is coalesced so fast
//! typing doesn't pay a handshake per keystroke.
//!
//! Sizing is FOLLOWER-ONLY by design: `/api/resize` is a raw PTY resize
//! with no host-side letterbox, so driving it would fight the host window
//! exactly like early phone builds did. We render at whatever grid the host
//! has and print a notice when it differs from the local terminal.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const REMOTE_ATTACH_ARG: &str = "__remote_attach__";

/// Feature flag: remote attach (this CLI and the server's raw `/write`
/// endpoint) is experimental and off by default. One env var gates both
/// halves so a stray build can't expose raw remote input.
pub const REMOTE_ATTACH_ENV: &str = "UNPEEL_REMOTE_ATTACH";

pub fn remote_attach_enabled() -> bool {
    std::env::var(REMOTE_ATTACH_ENV).ok().as_deref() == Some("1")
}

/// Detach key: Ctrl-\ (FS). Ctrl-C must pass through to the remote agent.
const DETACH_BYTE: u8 = 0x1C;
/// Input coalescing window: long enough to batch a paste or an escape
/// sequence into one request, short enough to feel instant.
const INPUT_COALESCE_MS: u64 = 8;
const OUTPUT_WAIT_MS: u64 = 25_000;
const OUTPUT_MAX_BYTES: u64 = 256 * 1024;

struct Endpoint {
    host: String,
    port: u16,
    token: String,
    /// Expected SHA-256 of the server's certificate (hex). None = accept
    /// any certificate (printed as a warning; fine for loopback).
    fingerprint: Option<String>,
}

pub fn run_cli(args: &[String]) -> i32 {
    if !remote_attach_enabled() {
        eprintln!(
            "remote attach is experimental and disabled; set {REMOTE_ATTACH_ENV}=1 to enable"
        );
        return 2;
    }
    let mut url: Option<String> = None;
    let mut token: Option<String> = None;
    let mut fingerprint: Option<String> = None;
    let mut session_id: Option<String> = None;

    let mut peer_file: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--url" => url = iter.next().cloned(),
            "--token" => token = iter.next().cloned(),
            "--fingerprint" => fingerprint = iter.next().cloned(),
            "--peer-file" => peer_file = iter.next().cloned(),
            other if !other.starts_with("--") => session_id = Some(other.to_string()),
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
    }

    // --peer-file keeps the token OUT of the command line: session commands
    // are recorded in manifests and appended to shell history, so inline
    // --token would leak the credential. The file is {url, token,
    // fingerprint} JSON, owner-readable (same shape as remote.json).
    if let Some(path) = peer_file {
        match read_peer_file(&path) {
            Some((peer_url, peer_token, peer_fp)) => {
                if url.is_none() {
                    url = Some(peer_url);
                }
                if token.is_none() {
                    token = Some(peer_token);
                }
                if fingerprint.is_none() {
                    fingerprint = peer_fp;
                }
            }
            None => {
                eprintln!("could not read peer file: {path}");
                return 2;
            }
        }
    }

    let Some(session_id) = session_id else {
        eprintln!(
            "usage: unpeel-host {REMOTE_ATTACH_ARG} [--url https://host:port] \
             [--token TOKEN] [--fingerprint SHA256HEX] <session-id>"
        );
        return 2;
    };

    // No --url: attach through this machine's own remote server.
    if url.is_none() || token.is_none() {
        match read_local_remote_state() {
            Some((local_url, local_token, local_fp)) => {
                if url.is_none() {
                    url = Some(local_url);
                    if fingerprint.is_none() {
                        fingerprint = local_fp;
                    }
                }
                if token.is_none() {
                    token = Some(local_token);
                }
            }
            None => {
                eprintln!(
                    "no --url/--token and ~/.unpeel/remote.json is unavailable \
                     (is the remote server running?)"
                );
                return 2;
            }
        }
    }
    let (url, token) = (url.unwrap(), token.unwrap());

    let Some((host, port)) = parse_https_url(&url) else {
        eprintln!("invalid --url (expected https://host:port): {url}");
        return 2;
    };
    if fingerprint.is_none() && host != "127.0.0.1" && host != "localhost" {
        eprintln!("warning: no --fingerprint; accepting any TLS certificate for {host}");
    }
    let endpoint = Arc::new(Endpoint {
        host,
        port,
        token,
        fingerprint,
    });

    match attach(&endpoint, &session_id) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("remote attach failed: {message}");
            1
        }
    }
}

fn attach(endpoint: &Arc<Endpoint>, session_id: &str) -> Result<(), String> {
    // Confirm the session and learn the host grid before touching the tty.
    let session = request(
        endpoint,
        "GET",
        &format!("/api/sessions/{session_id}"),
        None,
    )?;
    let label = session
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or(session_id)
        .to_string();
    let metrics = request(
        endpoint,
        "GET",
        &format!("/api/sessions/{session_id}/metrics"),
        None,
    )?;
    let cols = metrics.get("cols").and_then(Value::as_u64).unwrap_or(0);
    let rows = metrics.get("rows").and_then(Value::as_u64).unwrap_or(0);

    eprintln!("attached to \u{201c}{label}\u{201d} — remote grid {cols}x{rows}, Ctrl-\\ detaches");
    if let Some((local_cols, local_rows)) = local_grid() {
        if local_cols != cols || local_rows != rows {
            eprintln!(
                "note: this terminal is {local_cols}x{local_rows}; rendering follows the \
                 remote grid (host owns sizing)"
            );
        }
    }

    let raw = RawMode::enable().map_err(|e| format!("raw mode: {e}"))?;
    let done = Arc::new(AtomicBool::new(false));
    let offset = Arc::new(AtomicU64::new(0));

    // Output pump: long-poll loop on its own thread; stdout stays byte-exact.
    let pump_endpoint = Arc::clone(endpoint);
    let pump_done = Arc::clone(&done);
    let pump_offset = Arc::clone(&offset);
    let pump_session = session_id.to_string();
    let pump = std::thread::spawn(move || {
        let mut stdout = std::io::stdout();
        let mut have_offset = false;
        while !pump_done.load(Ordering::Relaxed) {
            let path = if have_offset {
                format!(
                    "/api/sessions/{pump_session}/output?offset={}&wait_ms={OUTPUT_WAIT_MS}&max_bytes={OUTPUT_MAX_BYTES}",
                    pump_offset.load(Ordering::Relaxed)
                )
            } else {
                format!(
                    "/api/sessions/{pump_session}/output?wait_ms=0&max_bytes={OUTPUT_MAX_BYTES}"
                )
            };
            match request(&pump_endpoint, "GET", &path, None) {
                Ok(chunk) => {
                    if let Some(data) = chunk.get("data_base64").and_then(Value::as_str) {
                        if let Ok(bytes) = BASE64.decode(data) {
                            if !bytes.is_empty() {
                                let _ = stdout.write_all(&bytes);
                                let _ = stdout.flush();
                            }
                        }
                    }
                    if let Some(next) = chunk.get("next_offset").and_then(Value::as_u64) {
                        pump_offset.store(next, Ordering::Relaxed);
                        have_offset = true;
                    }
                    if chunk.get("exited").and_then(Value::as_bool) == Some(true) {
                        pump_done.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(_) => {
                    // Transient network failure: back off briefly and resume
                    // from the same offset — the server replays from disk.
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    });

    // Input loop on this thread: raw stdin → coalesced POST /write.
    let mut stdin = std::io::stdin();
    let mut buffer = [0u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    while !done.load(Ordering::Relaxed) {
        match stdin.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let bytes = &buffer[..count];
                if bytes.contains(&DETACH_BYTE) {
                    break;
                }
                pending.extend_from_slice(bytes);
                // Coalesce: swallow anything else that arrives within the
                // window so pastes/escape sequences ship as one request.
                std::thread::sleep(Duration::from_millis(INPUT_COALESCE_MS));
                if let Some(more) = read_nonblocking(&mut stdin, &mut buffer) {
                    if more.contains(&DETACH_BYTE) {
                        break;
                    }
                    pending.extend_from_slice(&more);
                }
                // JSON carries text, not bytes: hold incomplete UTF-8 tails
                // for the next round instead of mangling a split rune.
                let split = utf8_boundary(&pending);
                if split > 0 {
                    let chunk = String::from_utf8_lossy(&pending[..split]).into_owned();
                    pending.drain(..split);
                    let body = json!({ "data": chunk });
                    if request(
                        endpoint,
                        "POST",
                        &format!("/api/sessions/{session_id}/write"),
                        Some(&body),
                    )
                    .is_err()
                    {
                        eprintln!("\r\ninput dropped (server unreachable)\r");
                    }
                }
            }
            Err(_) => break,
        }
    }

    done.store(true, Ordering::Relaxed);
    drop(raw);
    // The pump may be parked in a long-poll; don't wait the full 25s for it.
    let _ = pump.join_timeout(Duration::from_millis(300));
    eprintln!("\ndetached");
    Ok(())
}

trait JoinTimeout {
    fn join_timeout(self, timeout: Duration) -> Result<(), ()>;
}

impl JoinTimeout for std::thread::JoinHandle<()> {
    fn join_timeout(self, timeout: Duration) -> Result<(), ()> {
        let deadline = std::time::Instant::now() + timeout;
        while !self.is_finished() {
            if std::time::Instant::now() >= deadline {
                return Err(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.join();
        Ok(())
    }
}

/// Largest prefix length that ends on a UTF-8 rune boundary.
fn utf8_boundary(bytes: &[u8]) -> usize {
    let mut end = bytes.len();
    while end > 0 && end > bytes.len().saturating_sub(4) {
        if std::str::from_utf8(&bytes[..end]).is_ok() {
            return end;
        }
        end -= 1;
    }
    if std::str::from_utf8(&bytes[..end]).is_ok() {
        end
    } else {
        0
    }
}

fn read_nonblocking(stdin: &mut std::io::Stdin, buffer: &mut [u8]) -> Option<Vec<u8>> {
    let fd = 0;
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut poll, 1, 0) };
    if ready <= 0 || poll.revents & libc::POLLIN == 0 {
        return None;
    }
    match stdin.read(buffer) {
        Ok(count) if count > 0 => Some(buffer[..count].to_vec()),
        _ => None,
    }
}

fn local_grid() -> Option<(u64, u64)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) };
    if result == 0 && size.ws_col > 0 && size.ws_row > 0 {
        Some((u64::from(size.ws_col), u64::from(size.ws_row)))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Raw terminal mode
// ---------------------------------------------------------------------------

struct RawMode {
    original: libc::termios,
}

impl RawMode {
    fn enable() -> Result<RawMode, String> {
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut original) != 0 {
                return Err("stdin is not a terminal".into());
            }
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
                return Err("could not enter raw mode".into());
            }
            Ok(RawMode { original })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::tcsetattr(0, libc::TCSANOW, &self.original);
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal HTTPS client (rustls, one request per connection — the server is
// Connection: close)
// ---------------------------------------------------------------------------

fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(OUTPUT_WAIT_MS + 10_000)))
        .ok();

    let config = tls_config(endpoint.fingerprint.clone())?;
    let server_name = rustls::pki_types::ServerName::try_from(endpoint.host.clone())
        .map_err(|_| "invalid server name".to_string())?;
    let connection = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| format!("tls: {e}"))?;
    let mut tls = rustls::StreamOwned::new(connection, stream);

    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{payload}",
        host = endpoint.host,
        token = endpoint.token,
        len = payload.len(),
    );
    tls.write_all(request.as_bytes())
        .map_err(|e| format!("send: {e}"))?;

    let mut response = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => response.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(format!("read: {error}")),
        }
        if response.len() > 16 * 1024 * 1024 {
            return Err("response too large".into());
        }
    }

    let text = String::from_utf8_lossy(&response);
    let Some(header_end) = text.find("\r\n\r\n") else {
        return Err("malformed response".into());
    };
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body_text = &text[header_end + 4..];
    let value: Value = serde_json::from_str(body_text).map_err(|_| {
        format!(
            "HTTP {status}: {}",
            body_text.chars().take(120).collect::<String>()
        )
    })?;
    if !(200..300).contains(&status) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        return Err(format!("HTTP {status}: {message}"));
    }
    Ok(value)
}

fn tls_config(fingerprint: Option<String>) -> Result<rustls::ClientConfig, String> {
    let verifier = Arc::new(FingerprintVerifier { fingerprint });
    Ok(rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// Pins the server certificate to a SHA-256 fingerprint (the one the remote
/// server prints at startup and stores in remote.json). Without a pin it
/// accepts anything — self-signed dev certs — which run_cli warns about for
/// non-loopback hosts.
#[derive(Debug)]
struct FingerprintVerifier {
    fingerprint: Option<String>,
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Some(expected) = &self.fingerprint {
            let mut hasher = Sha256::new();
            hasher.update(end_entity.as_ref());
            let actual = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            if !expected.eq_ignore_ascii_case(&actual) {
                return Err(rustls::Error::General(format!(
                    "certificate fingerprint mismatch (expected {expected}, got {actual})"
                )));
            }
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

// ---------------------------------------------------------------------------
// Config parsing
// ---------------------------------------------------------------------------

fn parse_https_url(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("https://")?;
    let host_port = rest.split('/').next()?;
    let (host, port) = host_port.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

fn read_local_remote_state() -> Option<(String, String, Option<String>)> {
    read_peer_file(
        crate::app_paths::unpeel_home()
            .join("remote.json")
            .to_str()?,
    )
}

fn read_peer_file(path: &str) -> Option<(String, String, Option<String>)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let url = value.get("url")?.as_str()?.to_string();
    let token = value.get("token")?.as_str()?.to_string();
    let fingerprint = value
        .get("fingerprint")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((url, token, fingerprint))
}
