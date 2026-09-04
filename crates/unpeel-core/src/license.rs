//! Rust side of the Unpeel Link license flow, for the TUI / headless hosts.
//! Mirrors `LicenseManager.swift`: same key format (`CLRTY-<b64>.<b64>`,
//! Ed25519 over the encoded-payload string), same endpoints
//! (`/api/activate`, `/api/deactivate`, `/api/remote/entitlement`), same
//! entitlement cache file the relay uplink already reads. The key is stored
//! in `~/.unpeel/link-license.json` (0600) — headless boxes have no
//! Keychain; the file lives with the rest of the Host state.

use base64::Engine;
use std::io::{Read, Write};

const PUBLIC_KEY_B64: &str = "6RfwwHUhth8Ji7T7p/QbDOQjeN9Zrk1S34Hk85cpg54=";
const KEY_PREFIX: &str = "CLRTY-";

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LicensePayload {
    pub v: i64,
    pub id: String,
    pub email: String,
    pub plan: String,
    pub seats: i64,
    pub iat: i64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RelayEntitlementError {
    /// The service rejected this credential/binding. A previously cached
    /// entitlement must not keep the Host reachable after this response.
    Rejected(String),
    /// Connectivity, rate limiting, or a malformed server response. A still-
    /// valid cache may continue until its signed expiry while we retry.
    Transient(String),
}

impl RelayEntitlementError {
    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
}

impl std::fmt::Display for RelayEntitlementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Transient(message) => formatter.write_str(message),
        }
    }
}

/// Fold smart dashes back to ASCII and strip whitespace — same paste repair
/// as the app (`normalizeLicenseKey`).
pub fn normalize_key(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{200B}')
        .map(|c| match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' | '\u{FE58}' | '\u{FE63}' | '\u{FF0D}' => '-',
            other => other,
        })
        .collect()
}

fn b64url(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .ok()
}

/// Verify a key offline and return its payload (None = malformed/forged).
pub fn verify(raw: &str) -> Option<LicensePayload> {
    let key = normalize_key(raw);
    let body = key.strip_prefix(KEY_PREFIX)?;
    let (payload_b64, sig_b64) = body.split_once('.')?;
    let sig = b64url(sig_b64)?;
    let payload = b64url(payload_b64)?;
    let pubkey_env = std::env::var("UNPEEL_LICENSE_PUBLIC_KEY").ok();
    let pubkey_b64 = pubkey_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(PUBLIC_KEY_B64);
    let pubkey = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64)
        .ok()?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &pubkey)
        .verify(payload_b64.as_bytes(), &sig)
        .ok()?;
    serde_json::from_slice(&payload).ok()
}

fn license_path() -> std::path::PathBuf {
    crate::app_paths::unpeel_home().join("link-license.json")
}

fn link_tombstone_path() -> std::path::PathBuf {
    crate::app_paths::unpeel_home().join("link-disabled.json")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTombstoneReason {
    UserDisabled,
    ActivationPending,
    AuthorizationRejected,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
struct LinkTombstone {
    version: u64,
    generation: String,
    reason: LinkTombstoneReason,
    #[serde(rename = "disabled_at")]
    _disabled_at: u64,
}

/// Durable headless-Link suppression state. Callers must treat `Err` as
/// suppressed: an unreadable or malformed deny marker can never mean access
/// is enabled.
pub fn link_tombstone_reason() -> Result<Option<LinkTombstoneReason>, String> {
    with_license_lock(|| Ok(link_tombstone_unlocked()?.map(|marker| marker.reason)))
}

/// Whether automatic stored-key entitlement recovery is permitted while a
/// tombstone exists. Cached relay startup remains blocked in every case.
pub fn link_tombstone_allows_refresh() -> Result<bool, String> {
    with_license_lock(|| {
        let Some(marker) = link_tombstone_unlocked()? else {
            return Ok(true);
        };
        Ok(match marker.reason {
            LinkTombstoneReason::UserDisabled => false,
            LinkTombstoneReason::AuthorizationRejected => true,
            LinkTombstoneReason::ActivationPending => {
                stored_activation_generation_unlocked()?.as_deref()
                    == Some(marker.generation.as_str())
            }
        })
    })
}

/// One linearized snapshot of the authority a relay connection may present:
/// no suppression marker, a durable Host id, and a still-valid cache bound
/// to that exact id. Every writer of the shared marker/cache uses the same
/// lock, so callers never combine a pre-deactivation marker read with a
/// post-deactivation retained bearer.
pub fn allowed_cached_relay_entitlement() -> Result<Option<(String, String)>, String> {
    with_license_lock(|| {
        if link_tombstone_unlocked()?.is_some() {
            return Ok(None);
        }
        let path = crate::app_paths::unpeel_home()
            .join("mobile")
            .join("mac-id");
        let mac_id = match std::fs::read_to_string(&path) {
            Ok(value) => value.trim().to_string(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("could not read Host identity: {error}")),
        };
        if mac_id.is_empty() {
            return Ok(None);
        }
        Ok(crate::relay_uplink::cached_entitlement(&mac_id))
    })
}

/// Recheck a previously snapshotted bearer immediately before/after blocking
/// transport phases. Errors fail closed.
pub fn relay_entitlement_is_allowed(mac_id: &str, entitlement: &str) -> bool {
    matches!(
        allowed_cached_relay_entitlement(),
        Ok(Some((current_entitlement, current_mac_id)))
            if current_mac_id == mac_id && current_entitlement == entitlement
    )
}

/// Serialize the headless key and entitlement cache commit point across both
/// threads and TUI processes. A refresh may perform its HTTP request outside
/// this lock, but its final key check + rename is one transaction with local
/// deactivation, so a late response cannot resurrect removed authority.
fn with_license_lock<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::{Mutex, OnceLock};

    static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _process = PROCESS_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = crate::app_paths::unpeel_home();
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("link-license.lock"))
        .map_err(|error| error.to_string())?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let result = operation();
    let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    result
}

pub fn stored_file_exists() -> bool {
    license_path().exists()
}

/// The stored, still-verifying license, if any.
pub fn stored() -> Option<(String, LicensePayload)> {
    let raw = std::fs::read(license_path()).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let key = value.get("key")?.as_str()?.to_string();
    let payload = verify(&key)?;
    Some((key, payload))
}

fn stored_activation_generation_unlocked() -> Result<Option<String>, String> {
    let raw = match std::fs::read(license_path()) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read Link activation: {error}")),
    };
    let value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("invalid Link activation: {error}"))?;
    let Some(key) = value.get("key").and_then(serde_json::Value::as_str) else {
        return Err("invalid Link activation: missing key".into());
    };
    if verify(key).is_none() {
        return Ok(None);
    }
    Ok(value
        .get("activation_generation")
        .and_then(serde_json::Value::as_str)
        .filter(|generation| !generation.is_empty())
        .map(str::to_string))
}

fn store_key(
    key: &str,
    observed_tombstone: &Option<LinkTombstone>,
) -> Result<ActivationCommit, String> {
    with_license_lock(|| {
        // The suppression snapshot was captured before `/api/activate`.
        // Deactivation/rejection during that request changes its generation,
        // so this late service response cannot overwrite the newer local
        // decision even if it uses the same license key.
        let current_tombstone = link_tombstone_unlocked()?;
        if &current_tombstone != observed_tombstone {
            return Err("Link state changed while activation was in progress".into());
        }
        let tombstone_generation = current_tombstone
            .as_ref()
            .map(|marker| marker.generation.clone());
        let path = license_path();
        let temporary = path.with_file_name(format!(".link-license.{}.tmp", uuid::Uuid::new_v4()));
        let body = serde_json::json!({
            "key": key,
            "activation_generation": tombstone_generation,
        })
        .to_string();
        write_private_file(&temporary, body.as_bytes())?;
        // A newly activated key must never inherit a still-valid cache minted
        // for a previous key. Remove that authority before publishing the new
        // key; a failed removal leaves the old key in place and activation
        // reports the local commit failure.
        if let Err(error) = remove_relay_entitlement_unlocked() {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        if let Some(generation) = &tombstone_generation {
            // The activation is locally committed but Link remains denied
            // until a fresh entitlement lands. This durable intermediate
            // state lets an unattended Host recover after a crash/transient
            // request without ever trusting its old cache.
            write_link_tombstone_generation_unlocked(
                LinkTombstoneReason::ActivationPending,
                generation,
            )?;
        }
        Ok(ActivationCommit {
            tombstone_generation,
        })
    })
}

fn write_private_file(path: &std::path::Path, body: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| error.to_string())?;
    if let Err(error) = file.write_all(body) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error.to_string());
    }
    Ok(())
}

pub fn forget_key() -> Result<(), String> {
    with_license_lock(forget_key_unlocked)
}

pub fn remove_relay_entitlement() -> Result<(), String> {
    with_license_lock(remove_relay_entitlement_unlocked)
}

fn remove_file_if_present(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn forget_key_unlocked() -> Result<(), String> {
    remove_file_if_present(&license_path())
}

fn remove_relay_entitlement_unlocked() -> Result<(), String> {
    remove_file_if_present(
        &crate::app_paths::unpeel_home()
            .join("mobile")
            .join("relay-entitlement.json"),
    )
}

fn link_tombstone_unlocked() -> Result<Option<LinkTombstone>, String> {
    let path = link_tombstone_path();
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read Link disable marker: {error}")),
    };
    let marker: LinkTombstone = serde_json::from_slice(&raw)
        .map_err(|error| format!("invalid Link disable marker: {error}"))?;
    if marker.version != 1 {
        return Err(format!(
            "unsupported Link disable marker version {}",
            marker.version
        ));
    }
    if marker.generation.is_empty() {
        return Err("invalid Link disable marker: missing generation".into());
    }
    Ok(Some(marker))
}

fn write_link_tombstone_unlocked(reason: LinkTombstoneReason) -> Result<String, String> {
    let generation = uuid::Uuid::new_v4().to_string();
    write_link_tombstone_generation_unlocked(reason, &generation)?;
    Ok(generation)
}

fn write_link_tombstone_generation_unlocked(
    reason: LinkTombstoneReason,
    generation: &str,
) -> Result<(), String> {
    let path = link_tombstone_path();
    let temporary = path.with_file_name(format!(".link-disabled.{}.tmp", uuid::Uuid::new_v4()));
    let body = serde_json::json!({
        "version": 1,
        "generation": generation,
        "reason": reason,
        "disabled_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })
    .to_string();
    write_private_file(&temporary, body.as_bytes())?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

pub fn write_link_tombstone() -> Result<String, String> {
    with_license_lock(|| write_link_tombstone_unlocked(LinkTombstoneReason::UserDisabled))
}

fn ensure_stable_invalid_key_tombstone_unlocked() -> Result<(), String> {
    match link_tombstone_unlocked()? {
        Some(LinkTombstone {
            reason: LinkTombstoneReason::UserDisabled | LinkTombstoneReason::AuthorizationRejected,
            ..
        }) => Ok(()),
        Some(_) | None => {
            let _ = write_link_tombstone_unlocked(LinkTombstoneReason::AuthorizationRejected)?;
            Ok(())
        }
    }
}

/// Fail closed after a definitive entitlement/relay rejection. The durable
/// marker is published before cache deletion under the same lock used by
/// entitlement commit, so a late writer cannot leave an unmarked stale
/// bearer behind.
pub fn reject_relay_entitlement() -> Result<(), String> {
    with_license_lock(|| {
        if !matches!(
            link_tombstone_unlocked()?,
            Some(LinkTombstone {
                reason: LinkTombstoneReason::UserDisabled,
                ..
            })
        ) {
            // Every real service/WS rejection advances the generation. Any
            // older successful entitlement response captured before this
            // point must be unable to clear the new denial.
            let _ = write_link_tombstone_unlocked(LinkTombstoneReason::AuthorizationRejected)?;
        }
        remove_relay_entitlement_unlocked()
    })
}

/// Quarantine a malformed headless key without rotating the rejection
/// generation every maintenance tick. A valid explicit activation can then
/// snapshot that stable marker and recover normally.
pub fn reject_invalid_stored_key() -> Result<bool, String> {
    with_license_lock(|| {
        let invalid = match std::fs::read(license_path()) {
            Ok(raw) => serde_json::from_slice::<serde_json::Value>(&raw)
                .ok()
                .and_then(|value| {
                    value
                        .get("key")
                        .and_then(serde_json::Value::as_str)
                        .map(verify)
                })
                .flatten()
                .is_none(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(format!("could not recheck Link key: {error}")),
        };
        if !invalid {
            return Ok(false);
        }
        ensure_stable_invalid_key_tombstone_unlocked()?;
        remove_relay_entitlement_unlocked()?;
        forget_key_unlocked()?;
        Ok(true)
    })
}

fn clear_link_tombstone_unlocked() -> Result<(), String> {
    remove_file_if_present(&link_tombstone_path())
}

/// Stable per-machine id (headless equivalent of the app's hashed hardware
/// UUID): a random id minted once and kept in `~/.unpeel/device-id`.
pub fn device_id() -> Result<String, String> {
    with_license_lock(|| {
        use std::os::unix::fs::PermissionsExt;

        let path = crate::app_paths::unpeel_home().join("device-id");
        match std::fs::read_to_string(&path) {
            Ok(existing) if !existing.trim().is_empty() => {
                let trimmed = existing.trim().to_string();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|error| error.to_string())?;
                return Ok(trimmed);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not read Link device identity: {error}")),
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let temporary = path.with_file_name(format!(".device-id.{}.tmp", uuid::Uuid::new_v4()));
        write_private_file(&temporary, id.as_bytes())?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        Ok(id)
    })
}

fn api_base() -> String {
    std::env::var("UNPEEL_LICENSE_API_BASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://unpeel.com".into())
}

/// Minimal HTTPS/HTTP JSON POST (rustls + webpki roots; `http://` allowed
/// for local dev against `wrangler dev`). Connection: close, so the body is
/// read to EOF.
fn post_json(path: &str, body: &serde_json::Value) -> Result<(u16, serde_json::Value), String> {
    let base = api_base();
    let (tls, rest) = if let Some(rest) = base.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = base.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err("bad UNPEEL_LICENSE_API_BASE_URL".into());
    };
    let host = rest.split('/').next().unwrap_or(rest).to_string();
    let (host_name, port) = match host.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|e| e.to_string())?),
        None => (host.clone(), if tls { 443 } else { 80 }),
    };
    let payload = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    use std::net::ToSocketAddrs;
    let addresses = (host_name.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("resolve: {error}"))?;
    let mut last_connect_error = None;
    let mut stream = None;
    for address in addresses {
        match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(15)) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_connect_error = Some(error),
        }
    }
    let stream = stream.ok_or_else(|| {
        format!(
            "connect: {}",
            last_connect_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "host resolved to no addresses".into())
        )
    })?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .ok();
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(15)))
        .ok();
    let mut raw = Vec::new();
    if tls {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server = rustls::pki_types::ServerName::try_from(host_name.clone())
            .map_err(|e| e.to_string())?;
        let conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server)
            .map_err(|e| e.to_string())?;
        let mut stream = rustls::StreamOwned::new(conn, stream);
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("send: {e}"))?;
        // close_notify-less servers make read_to_end error at EOF; keep what
        // was read.
        let _ = stream.read_to_end(&mut raw);
    } else {
        let mut stream = stream;
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("send: {e}"))?;
        let _ = stream.read_to_end(&mut raw);
    }
    parse_http_json_response(&raw)
}

fn parse_http_json_response(raw: &[u8]) -> Result<(u16, serde_json::Value), String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or("malformed HTTP status")?;
    let body = &raw[split + 4..];
    let chunked = head.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let json_body = if chunked {
        match dechunk_http_body(body) {
            Ok(body) => body,
            // Authorization classification comes from the HTTP status, never
            // from whether an intermediary preserved a well-formed body.
            Err(_) if !(200..300).contains(&status) => Vec::new(),
            Err(error) => return Err(error),
        }
    } else {
        body.to_vec()
    };
    let value = match serde_json::from_slice(&json_body) {
        Ok(value) => value,
        // Authorization classification comes from the HTTP status, never
        // from whether an intermediary happened to preserve a JSON body.
        Err(_) if !(200..300).contains(&status) => serde_json::Value::Null,
        Err(error) => return Err(format!("bad JSON from server: {error}")),
    };
    Ok((status, value))
}

fn dechunk_http_body(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("malformed chunked response: missing size terminator")?;
        let size_line = std::str::from_utf8(&body[..line_end])
            .map_err(|error| format!("malformed chunk size: {error}"))?;
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|error| format!("malformed chunk size: {error}"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(output);
        }
        let framed_size = size.checked_add(2).ok_or("chunk size overflow")?;
        if body.len() < framed_size {
            return Err("truncated chunked response".into());
        }
        if &body[size..framed_size] != b"\r\n" {
            return Err("malformed chunked response: missing data terminator".into());
        }
        output.extend_from_slice(&body[..size]);
        body = &body[framed_size..];
    }
}

/// Activate this machine's seat. Returns the payload on success (key stored).
pub fn activate(raw_key: &str, device_name: &str) -> Result<LicensePayload, String> {
    let pending = request_activation(raw_key, device_name)?;
    let _ = commit_activation(&pending)?;
    Ok(pending.payload)
}

#[derive(Debug, Clone)]
pub struct PendingActivation {
    pub key: String,
    pub payload: LicensePayload,
    observed_tombstone: Option<LinkTombstone>,
}

/// Proof of the local activation commit that may clear one exact durable
/// disable marker after its entitlement is safely published. The generation
/// is intentionally opaque to callers.
#[derive(Debug, Clone)]
pub struct ActivationCommit {
    tombstone_generation: Option<String>,
}

/// Perform the service-side seat activation without touching shared Host
/// files. Interactive owners re-check app/TUI ownership before committing a
/// response that may have taken seconds to arrive.
pub fn request_activation(raw_key: &str, device_name: &str) -> Result<PendingActivation, String> {
    let key = normalize_key(raw_key);
    let payload = verify(&key).ok_or("that doesn't look like a valid Unpeel license key")?;
    // Capture suppression before crossing the network. The commit refuses a
    // response if another frontend deactivated/rejected Link meanwhile.
    let observed_tombstone = with_license_lock(link_tombstone_unlocked)?;
    let (status, result) = post_json(
        "/api/activate",
        &serde_json::json!({
            "key": key,
            "device_id": device_id()?,
            "device_name": device_name,
        }),
    )?;
    if (200..300).contains(&status) && result.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(PendingActivation {
            key,
            payload,
            observed_tombstone,
        })
    } else {
        Err(match result.get("error").and_then(|v| v.as_str()) {
            Some("seat_limit") => "all seats for this license are in use".into(),
            Some("revoked") => "this license has been revoked".into(),
            Some("unknown") => "this key isn't recognized".into(),
            _ => result
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("activation failed")
                .to_string(),
        })
    }
}

/// Commit a previously accepted activation to the private headless key file.
pub fn commit_activation(pending: &PendingActivation) -> Result<ActivationCommit, String> {
    store_key(&pending.key, &pending.observed_tombstone)
}

/// Release this machine's seat and forget the stored key.
pub fn deactivate() -> Result<(), String> {
    let key = deactivate_local()?;
    if let Some(key) = key {
        let _ = request_deactivation_for_key(&key);
    }
    Ok(())
}

/// Revoke local authority synchronously and return the former key for a
/// background best-effort seat release. This is intentionally filesystem-
/// only so a Settings action can fail closed without blocking rendering on
/// DNS/TCP/HTTP.
pub fn deactivate_local() -> Result<Option<String>, String> {
    // Revoke local authority first. The network call is best-effort and must
    // never leave a live cached relay credential behind while it is slow or
    // unreachable.
    with_license_lock(|| {
        let key = stored().map(|(key, _)| key);
        // Durable deny first. If any later unlink fails, a process restart
        // still cannot trust the retained cache-only credential.
        let _ = write_link_tombstone_unlocked(LinkTombstoneReason::UserDisabled)?;
        // Cache first is fail-closed: if deleting the key then failed, there
        // is still no bearer credential the relay can accept while the user
        // retries the local deactivation.
        remove_relay_entitlement_unlocked()?;
        forget_key_unlocked()?;
        Ok(key)
    })
}

/// Service-side half of deactivation. Callers already removed local relay
/// authority; failure is diagnostic/retryable and cannot reopen Link.
pub fn request_deactivation_for_key(key: &str) -> Result<(), String> {
    let device_id = device_id()?;
    let (status, result) = post_json(
        "/api/deactivate",
        &serde_json::json!({ "key": key, "device_id": device_id }),
    )?;
    if (200..300).contains(&status)
        && result.get("ok").and_then(|value| value.as_bool()) == Some(true)
    {
        Ok(())
    } else {
        Err(result
            .get("reason")
            .or_else(|| result.get("error"))
            .and_then(|value| value.as_str())
            .unwrap_or("server could not release the Link seat")
            .to_string())
    }
}

/// Fetch and cache a relay entitlement for `mac_id` — the file
/// `relay_uplink::cached_entitlement` reads. This is what turns a key into
/// working Link on a headless Host.
pub fn fetch_relay_entitlement(mac_id: &str) -> Result<(), RelayEntitlementError> {
    let (key, _) = stored()
        .ok_or_else(|| RelayEntitlementError::Rejected("no valid license stored".into()))?;
    fetch_relay_entitlement_for_key(mac_id, &key)
}

/// Explicit-key variant for synchronous callers. Background refreshers use
/// `request_relay_entitlement_for_key` and commit on their owning UI thread so
/// an app/TUI ownership handoff cannot mutate the shared cache late.
pub fn fetch_relay_entitlement_for_key(
    mac_id: &str,
    key: &str,
) -> Result<(), RelayEntitlementError> {
    let pending = request_relay_entitlement_for_key(mac_id, key)?;
    commit_relay_entitlement_for_key(key, &pending)
}

#[derive(Clone, Debug)]
pub struct PendingRelayEntitlement {
    pub entitlement: crate::relay_uplink::CachedEntitlement,
    observed_tombstone: Option<LinkTombstone>,
}

/// Perform only the service request. This deliberately does not touch disk:
/// a caller can re-check frontend ownership and the current key after a slow
/// response before entering the serialized cache commit point.
pub fn request_relay_entitlement_for_key(
    mac_id: &str,
    key: &str,
) -> Result<PendingRelayEntitlement, RelayEntitlementError> {
    let device_id = device_id().map_err(RelayEntitlementError::Transient)?;
    // Bind the eventual commit to the exact suppression generation observed
    // before this request. A definitive rejection/deactivation while HTTP is
    // in flight mints a new generation and makes the stale success harmless.
    let observed_tombstone =
        with_license_lock(link_tombstone_unlocked).map_err(RelayEntitlementError::Transient)?;
    let (status, result) = post_json(
        "/api/remote/entitlement",
        &serde_json::json!({ "key": key, "mac_id": mac_id, "device_id": device_id }),
    )
    .map_err(RelayEntitlementError::Transient)?;
    if !(200..300).contains(&status) {
        let message = result
            .get("reason")
            .or_else(|| result.get("error"))
            .and_then(|value| value.as_str())
            .unwrap_or("server refused the entitlement")
            .to_string();
        let error = if matches!(status, 400 | 401 | 402 | 403 | 404 | 409 | 410 | 422) {
            RelayEntitlementError::Rejected(message)
        } else {
            RelayEntitlementError::Transient(message)
        };
        return Err(error);
    }
    let entitlement = result
        .get("entitlement")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            RelayEntitlementError::Transient("server returned no relay entitlement".into())
        })?;
    let expires = result
        .get("expires_at")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| {
            RelayEntitlementError::Transient("server returned no entitlement expiry".into())
        })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if expires <= now {
        return Err(RelayEntitlementError::Transient(
            "server returned an expired relay entitlement".into(),
        ));
    }
    Ok(PendingRelayEntitlement {
        entitlement: crate::relay_uplink::CachedEntitlement {
            entitlement: entitlement.to_string(),
            mac_id: mac_id.to_string(),
            expires_at: expires,
        },
        observed_tombstone,
    })
}

/// Publish an issued entitlement only while `key` is still authoritative.
/// This key check and the atomic rename share the same process/file lock as
/// deactivation, so whichever operation wins leaves the final local state.
pub fn commit_relay_entitlement_for_key(
    key: &str,
    pending: &PendingRelayEntitlement,
) -> Result<(), RelayEntitlementError> {
    commit_relay_entitlement(key, pending, None)
}

/// Publish the entitlement obtained immediately after a user-initiated
/// activation. Only this exact activation commit may clear its durable
/// `activation_pending` generation; a newer deactivation always wins.
pub fn commit_relay_entitlement_for_activation(
    key: &str,
    pending: &PendingRelayEntitlement,
    activation: &ActivationCommit,
) -> Result<(), RelayEntitlementError> {
    commit_relay_entitlement(key, pending, Some(activation))
}

fn commit_relay_entitlement(
    key: &str,
    pending: &PendingRelayEntitlement,
    activation: Option<&ActivationCommit>,
) -> Result<(), RelayEntitlementError> {
    with_license_lock(|| {
        let key_is_current = stored()
            .map(|(stored_key, _)| stored_key == key)
            .unwrap_or(false);
        if !key_is_current {
            return Err("license changed while refreshing the relay entitlement".into());
        }
        let tombstone = link_tombstone_unlocked()?;
        if tombstone != pending.observed_tombstone {
            return Err("Link authority changed while refreshing the relay entitlement".into());
        }
        let clear_tombstone = match (&tombstone, activation) {
            (None, _) => false,
            (Some(marker), Some(activation))
                if marker.reason == LinkTombstoneReason::ActivationPending
                    && activation.tombstone_generation.as_deref()
                        == Some(marker.generation.as_str()) =>
            {
                true
            }
            (Some(marker), None)
                if marker.reason == LinkTombstoneReason::ActivationPending
                    && stored_activation_generation_unlocked()?.as_deref()
                        == Some(marker.generation.as_str()) =>
            {
                true
            }
            (Some(marker), None) if marker.reason == LinkTombstoneReason::AuthorizationRejected => {
                true
            }
            (Some(_), Some(_)) => {
                return Err("Link was disabled again while activation was authorizing".into());
            }
            (Some(_), None) => {
                return Err("Link is disabled until it is activated again".into());
            }
        };
        let dir = crate::app_paths::unpeel_home().join("mobile");
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let path = dir.join("relay-entitlement.json");
        let temporary = dir.join(format!(".relay-entitlement.{}.tmp", uuid::Uuid::new_v4()));
        let body = serde_json::json!({
            "entitlement": pending.entitlement.entitlement,
            "expiresAt": pending.entitlement.expires_at,
            "macID": pending.entitlement.mac_id,
        })
        .to_string();
        write_private_file(&temporary, body.as_bytes())?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        if clear_tombstone {
            // Activation-pending and authorization-rejection generations may
            // recover only after this fresh entitlement is durably in place.
            clear_link_tombstone_unlocked()?;
        }
        Ok(())
    })
    .map_err(RelayEntitlementError::Transient)
}

/// The macID this Host is known by to its paired devices: the cached
/// entitlement's, else the one in the paired-device E2E key registry.
pub fn known_mac_id() -> Option<String> {
    let mobile = crate::app_paths::unpeel_home().join("mobile");
    if let Some(mac_id) = std::fs::read_to_string(mobile.join("mac-id"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(mac_id);
    }
    if let Some(record) = crate::relay_uplink::cached_entitlement_record() {
        return Some(record.mac_id);
    }
    let path = mobile.join("e2e-keys.json");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value
        .as_object()?
        .keys()
        .find_map(|k| k.split_once('.').map(|(mac, _)| mac.to_string()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalize_repairs_smart_dashes_and_whitespace() {
        assert_eq!(super::normalize_key("CLRTY\u{2013}a b\nc"), "CLRTY-abc");
    }

    #[test]
    fn verify_rejects_garbage() {
        assert!(super::verify("CLRTY-abc.def").is_none());
        assert!(super::verify("nope").is_none());
    }

    #[test]
    fn decodes_native_link_tombstone_contract() {
        let marker: super::LinkTombstone = serde_json::from_str(
            r#"{
                "version": 1,
                "generation": "swift-generation",
                "reason": "authorization_rejected",
                "disabled_at": 1786665600
            }"#,
        )
        .expect("decode Swift-shaped Link tombstone");
        assert_eq!(marker.version, 1);
        assert_eq!(marker.generation, "swift-generation");
        assert_eq!(
            marker.reason,
            super::LinkTombstoneReason::AuthorizationRejected
        );
    }

    #[test]
    fn parses_chunked_license_json_across_multiple_chunks() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=yes\r\n{\"ok\"\r\n6\r\n:true}\r\n0\r\nX-Trailer: ignored\r\n\r\n";
        let (status, value) = super::parse_http_json_response(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(value, serde_json::json!({ "ok": true }));
    }

    #[test]
    fn rejects_truncated_successful_chunked_license_json() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\n{\"ok\":tru";
        assert!(super::parse_http_json_response(raw)
            .unwrap_err()
            .contains("truncated chunked response"));
    }

    #[test]
    fn preserves_authorization_status_when_chunked_error_body_is_malformed() {
        let raw = b"HTTP/1.1 403 Forbidden\r\nTransfer-Encoding: chunked\r\n\r\nnot-a-size\r\n";
        let (status, value) = super::parse_http_json_response(raw).unwrap();
        assert_eq!(status, 403);
        assert!(value.is_null());
    }
}
