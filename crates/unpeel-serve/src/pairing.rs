//! Phone pairing, byte-compatible with the native `MobilePairingStore` +
//! `RemotePairingCrypto` so the shipped iOS app pairs with the TUI unchanged.
//!
//! Wire shape (all of it load-bearing — see the four different encodings):
//! - QR code text: `UNPEEL:1:<host>:<port>:<MACID-UPPER>:<token>:<expiresSec>`
//!   with an optional final proxy id for controller-assisted pairing. The
//!   phone rebuilds the endpoint byte-for-byte because it is authenticated
//!   data; the sealed response then supplies the Host's Direct endpoint.
//! - Token: 16 CSPRNG bytes in unpadded uppercase RFC4648 base32; the HKDF
//!   IKM is the UTF-8 **text** of that token, not the decoded bytes.
//! - Envelope: `{"v":1,"saltB64","sealedB64"}`, standard padded base64;
//!   `sealed` = nonce(12) ‖ ciphertext ‖ tag(16) (CryptoKit `combined`).
//! - Key: HKDF-SHA256(ikm=token, salt=16 random, info="unpeel-pairing-v1:"
//!   + direction), 32 bytes; direction is `phone-to-mac` / `mac-to-phone`.
//! - AAD: `unpeel-pairing-v1\0<direction>\0<macID>\0<endpoint>`.
//! - Issued tokens: 32 CSPRNG bytes as unpadded base64url; stored only as
//!   lowercase-hex SHA-256 in `devices.json`.

use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngCore;

use unpeel_core::app_paths;

const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
const PAIRING_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const BASE32: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub struct ActivePairing {
    pub token: String,
    pub endpoint: String,
    pub direct_endpoint: String,
    pub expires_at_ms: u64,
}

#[derive(Default)]
struct PairingState {
    active: Option<ActivePairing>,
    completed: bool,
    response_pending: bool,
}

#[derive(Default)]
pub struct PairingWindow {
    state: Mutex<PairingState>,
    changed: Condvar,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; n];
    rand::rng().fill_bytes(&mut buffer);
    buffer
}

/// Unpadded uppercase RFC4648 base32, MSB-first with the final partial group
/// left-shifted — matches `randomBase32Token`.
fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for byte in data {
        buffer = (buffer << 8) | *byte as u32;
        bits += 8;
        while bits >= 5 {
            let index = (buffer >> (bits - 5)) & 0x1f;
            out.push(BASE32[index as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        let index = (buffer << (5 - bits)) & 0x1f;
        out.push(BASE32[index as usize] as char);
    }
    out
}

/// 32 CSPRNG bytes as unpadded base64url — the `authToken`/`relayToken` form.
pub fn random_token() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(32))
}

pub fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn derive_key(token: &str, salt: &[u8], direction: &str) -> [u8; 32] {
    let info = format!("unpeel-pairing-v1:{direction}");
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), token.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(info.as_bytes(), &mut key)
        .expect("32 bytes is a valid HKDF length");
    key
}

fn associated_data(direction: &str, mac_id: &str, endpoint: &str) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(b"unpeel-pairing-v1");
    aad.push(0);
    aad.extend_from_slice(direction.as_bytes());
    aad.push(0);
    aad.extend_from_slice(mac_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(endpoint.as_bytes());
    aad
}

fn compact_http_path(value: &str) -> Option<&str> {
    if value.len() > 2048 || value.contains(['?', '#', '@']) {
        return None;
    }
    let remainder = value.strip_prefix("http://")?;
    let (authority, path) = remainder.split_once('/')?;
    let (host, port) = authority.rsplit_once(':')?;
    if host.is_empty()
        || host.contains(':')
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        || port.parse::<u16>().ok().is_none_or(|port| port == 0)
    {
        return None;
    }
    Some(path)
}

fn valid_proxy_endpoint(value: &str) -> bool {
    let Some(proxy_id) =
        compact_http_path(value).and_then(|path| path.strip_prefix("mobile/pairing-proxy/"))
    else {
        return false;
    };
    !proxy_id.is_empty()
        && proxy_id.len() <= 128
        && proxy_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_direct_endpoint(value: &str) -> bool {
    compact_http_path(value) == Some("mobile")
}

pub fn open_envelope(
    envelope: &serde_json::Value,
    token: &str,
    mac_id: &str,
    endpoint: &str,
) -> Option<Vec<u8>> {
    if envelope.get("v").and_then(|v| v.as_u64()) != Some(1) {
        return None;
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let salt = engine
        .decode(envelope.get("saltB64")?.as_str()?)
        .ok()
        .filter(|s| s.len() == 16)?;
    let sealed = engine.decode(envelope.get("sealedB64")?.as_str()?).ok()?;
    if sealed.len() < 12 + 16 {
        return None;
    }
    let key = derive_key(token, &salt, "phone-to-mac");
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let (nonce, rest) = sealed.split_at(12);
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: rest,
                aad: &associated_data("phone-to-mac", mac_id, endpoint),
            },
        )
        .ok()
}

pub fn seal_envelope(
    plaintext: &[u8],
    token: &str,
    mac_id: &str,
    endpoint: &str,
) -> Option<serde_json::Value> {
    let salt = random_bytes(16);
    let key = derive_key(token, &salt, "mac-to-phone");
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let nonce_bytes = random_bytes(12);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &associated_data("mac-to-phone", mac_id, endpoint),
            },
        )
        .ok()?;
    let mut combined = nonce_bytes.clone();
    combined.extend_from_slice(&ciphertext);
    let engine = base64::engine::general_purpose::STANDARD;
    Some(serde_json::json!({
        "v": 1,
        "saltB64": engine.encode(&salt),
        "sealedB64": engine.encode(&combined),
    }))
}

impl PairingWindow {
    /// Open a 5-minute window and return (QR text, token). Overwrites any
    /// existing window — one pairing at a time, same as the app.
    pub fn begin(&self, host: &str, port: u16, mac_id: &str) -> Option<(String, String)> {
        if host.contains(':') || host.is_empty() || mac_id.is_empty() {
            return None; // IPv6 hosts can't be encoded in the compact form
        }
        let token = base32_encode(&random_bytes(16));
        let expires_at_ms = now_ms() + PAIRING_TTL.as_millis() as u64;
        let endpoint = format!("http://{host}:{port}/mobile");
        let code = format!(
            "UNPEEL:1:{host}:{port}:{}:{token}:{}",
            mac_id.to_uppercase(),
            expires_at_ms / 1000
        );
        let mut state = self.state.lock().ok()?;
        state.completed = false;
        state.response_pending = false;
        state.active = Some(ActivePairing {
            token: token.clone(),
            direct_endpoint: endpoint.clone(),
            endpoint,
            expires_at_ms,
        });
        Some((code, token))
    }

    /// Open the same one-time pairing window with a Controller-owned proxy
    /// endpoint. The real Host endpoint rides inside the sealed response so
    /// the phone uses the proxy only for this bootstrap transaction.
    pub fn begin_invitation(
        &self,
        endpoint: &str,
        direct_endpoint: &str,
        mac_id: &str,
        mac_name: &str,
    ) -> Option<serde_json::Value> {
        if mac_id.is_empty()
            || !valid_proxy_endpoint(endpoint)
            || !valid_direct_endpoint(direct_endpoint)
        {
            return None;
        }
        let token = base32_encode(&random_bytes(16));
        let expires_at_ms = now_ms() + PAIRING_TTL.as_millis() as u64;
        let mut state = self.state.lock().ok()?;
        state.completed = false;
        state.response_pending = false;
        state.active = Some(ActivePairing {
            token: token.clone(),
            endpoint: endpoint.to_owned(),
            direct_endpoint: direct_endpoint.to_owned(),
            expires_at_ms,
        });
        Some(serde_json::json!({
            "protocolVersion": 1,
            "macID": mac_id,
            "macName": mac_name,
            "endpoint": endpoint,
            "token": token,
            "expiresAtUnixMs": expires_at_ms,
        }))
    }

    pub fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.completed = false;
            state.response_pending = false;
            state.active = None;
        }
        self.changed.notify_all();
    }

    /// True only after a request was authenticated, credentials were
    /// persisted, and a sealed success response was produced. This remains
    /// correct when an existing stable Controller device is re-paired and its
    /// record is replaced rather than increasing the device count.
    pub fn completed(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.completed)
            .unwrap_or(false)
    }

    /// Finish the successful one-shot exchange only after the sealed response
    /// was written to the Controller socket. Until then `is_open` keeps the
    /// CLI alive so `pair --serve` cannot tear the server down mid-response.
    pub fn finish_response(&self, sent: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.completed = sent;
            state.response_pending = false;
        }
        self.changed.notify_all();
    }

    pub fn is_open(&self) -> bool {
        self.state
            .lock()
            .map(|state| {
                state.response_pending
                    || state
                        .active
                        .as_ref()
                        .is_some_and(|pairing| pairing.expires_at_ms > now_ms())
            })
            .unwrap_or(false)
    }

    /// Block the one-shot CLI without a polling gap. State and wakeups share
    /// one mutex/condition variable, so the success transition cannot race a
    /// stale `is_open` observation or leave the Controller talking to the
    /// setup listener for another half second.
    pub fn wait_until_closed(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        loop {
            if state.response_pending {
                let Ok((next, timeout)) =
                    self.changed.wait_timeout(state, PAIRING_RESPONSE_TIMEOUT)
                else {
                    return;
                };
                state = next;
                if timeout.timed_out() && state.response_pending {
                    state.response_pending = false;
                    state.completed = false;
                    return;
                }
                continue;
            }
            let Some(expires_at_ms) = state.active.as_ref().map(|pairing| pairing.expires_at_ms)
            else {
                return;
            };
            let remaining_ms = expires_at_ms.saturating_sub(now_ms());
            if remaining_ms == 0 {
                return;
            }
            let Ok((next, _)) = self
                .changed
                .wait_timeout(state, Duration::from_millis(remaining_ms))
            else {
                return;
            };
            state = next;
        }
    }

    /// Handle `POST /mobile/pair`. Returns (status, body). Mirrors the app's
    /// status/message table so the phone's error copy stays accurate.
    pub fn handle_pair(&self, body: &[u8], mac_id: &str, mac_name: &str) -> (u16, String) {
        let error = |message: &str| serde_json::json!({ "error": message }).to_string();
        let Ok(mut state) = self.state.lock() else {
            return (500, error("pairing unavailable"));
        };
        let Some(active) = state.active.as_ref() else {
            return (401, error("pairing is not active"));
        };
        if active.expires_at_ms <= now_ms() {
            state.active = None;
            return (401, error("pairing token expired"));
        }
        let Ok(envelope) = serde_json::from_slice::<serde_json::Value>(body) else {
            return (400, error("request failed"));
        };
        let Some(plaintext) = open_envelope(&envelope, &active.token, mac_id, &active.endpoint)
        else {
            return (401, error("invalid encrypted pairing request"));
        };
        let Ok(request) = serde_json::from_slice::<serde_json::Value>(&plaintext) else {
            return (401, error("invalid encrypted pairing request"));
        };
        if request.get("token").and_then(|v| v.as_str()) != Some(active.token.as_str()) {
            return (401, error("invalid pairing token"));
        }

        let device = request.get("device").cloned().unwrap_or_default();
        let device_id = device
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().to_lowercase());
        let auth_token = random_token();
        let relay_token = random_token();
        let e2e_key = random_bytes(32);

        if let Err(message) = register_device(
            &device_id,
            device
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Phone"),
            device
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("iOS"),
            device.get("appVersion").and_then(|v| v.as_str()),
            &auth_token,
            &relay_token,
            &e2e_key,
            mac_id,
        ) {
            return (500, error(&message));
        }

        let (remote_port, fingerprint) = crate::mobile::remote_server_advertisement();
        // The direct `/mobile` listener serves the same Host certificate as
        // the WSS streamer, so a freshly paired device always receives the
        // pin — even while the streamer is down.
        let fingerprint = fingerprint.or_else(crate::mobile::direct_certificate_fingerprint);
        let engine = base64::engine::general_purpose::STANDARD;
        let mut response = serde_json::json!({
            "protocolVersion": 1,
            "macID": mac_id,
            "macName": mac_name,
            "endpoint": active.endpoint,
            "deviceID": device_id,
            "authToken": auth_token,
            "pairedAtUnixMs": now_ms(),
            // Additive: the Host version, the phone's fallback TLS signal
            // (`>= 0.5.3` serves the direct endpoint over TLS).
            "serverVersion": env!("CARGO_PKG_VERSION"),
            // Required by the phone's decoder even on LAN-only pairings.
            "relayCredentials": {
                "relayURL": relay_url(),
                "macID": mac_id,
                "relayToken": relay_token,
                "e2eKeyB64": engine.encode(&e2e_key),
            },
        });
        if active.direct_endpoint != active.endpoint {
            response["directEndpoint"] = active.direct_endpoint.clone().into();
        }
        if let Some(obj) = response.as_object_mut() {
            // Additive: the direct endpoint above is served over TLS with the
            // certificate named by `remoteServerCertificateFingerprint`.
            obj.insert("directTLS".into(), true.into());
            if let Some(port) = remote_port {
                obj.insert("remoteServerPort".into(), port.into());
            }
            if let Some(fp) = fingerprint {
                obj.insert("remoteServerCertificateFingerprint".into(), fp.into());
            }
        }
        let Some(sealed) = seal_envelope(
            response.to_string().as_bytes(),
            &active.token,
            mac_id,
            &active.endpoint,
        ) else {
            return (500, error("failed to seal pairing response"));
        };
        // Seal the token before returning, but keep the CLI's pairing window
        // logically open until the HTTP adapter confirms that this response
        // reached the socket.
        state.response_pending = true;
        state.active = None; // single use
        drop(state);
        self.changed.notify_all();
        (200, sealed.to_string())
    }
}

fn relay_url() -> String {
    std::env::var("UNPEEL_RELAY_URL").unwrap_or_else(|_| "wss://relay.unpeel.com".into())
}

fn mobile_dir() -> std::path::PathBuf {
    app_paths::unpeel_home().join("mobile")
}

/// Cross-process transaction lock shared with native MobilePairingStore.
/// Atomic rename prevents torn JSON, but only this stable lock prevents a
/// native and TUI read-modify-write from restoring each other's revoked
/// records. The process mutex covers BSD flock implementations whose locks
/// are process-associated rather than open-file-description-associated.
fn with_device_store_lock<T>(
    operation: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _process = PROCESS_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = mobile_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mobile dir: {e}"))?;
    let lock_path = dir.join("devices.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        // flock uses the file as a stable inode only; never alter its data.
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| format!("device store lock open failed: {e}"))?;
    let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
    if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(format!(
            "device store lock failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let result = operation(&dir);
    let _ = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN) };
    result
}

/// Append (or replace) the device record in the shared devices.json, and
/// persist the per-device E2E key. The app keeps that key in the Keychain;
/// app-lessly we store it 0600 next to the device file — pairing must fail
/// if it can't be persisted, because the relay path depends on it later.
#[allow(clippy::too_many_arguments)]
fn register_device(
    device_id: &str,
    name: &str,
    platform: &str,
    app_version: Option<&str>,
    auth_token: &str,
    relay_token: &str,
    e2e_key: &[u8],
    mac_id: &str,
) -> Result<(), String> {
    with_device_store_lock(|dir| {
        let engine = base64::engine::general_purpose::STANDARD;
        let keys_path = dir.join("e2e-keys.json");
        let mut keys = read_json_or_default(&keys_path, || serde_json::json!({}))?;
        let key_map = keys
            .as_object_mut()
            .ok_or("e2e-keys.json is not an object")?;
        key_map.insert(
            format!("{mac_id}.{device_id}"),
            engine.encode(e2e_key).into(),
        );
        write_private(&keys_path, &keys).map_err(|e| format!("e2e key save failed: {e}"))?;

        let devices_path = dir.join("devices.json");
        let mut store = read_json_or_default(
            &devices_path,
            || serde_json::json!({ "version": 1, "devices": [] }),
        )?;
        let devices = store
            .get_mut("devices")
            .and_then(|v| v.as_array_mut())
            .ok_or("devices.json has no devices array")?;
        devices.retain(|d| d.get("id").and_then(|v| v.as_str()) != Some(device_id));
        let mut record = serde_json::json!({
            "id": device_id,
            "name": name,
            "platform": platform,
            // Current pairing grants owner/controller scope. Persist the
            // human principal separately from the device now so future Link
            // accounts can attach several devices to one Session owner.
            "principalID": unpeel_core::state::host_owner_principal_id(mac_id),
            "tokenHash": sha256_hex(auth_token),
            "pairedAtUnixMs": now_ms(),
            "lastSeenAtUnixMs": now_ms(),
            "relayTokenHash": sha256_hex(relay_token),
        });
        if let (Some(obj), Some(version)) = (record.as_object_mut(), app_version) {
            obj.insert("appVersion".into(), version.into());
        }
        devices.push(record);
        write_private(&devices_path, &store).map_err(|e| format!("devices.json: {e}"))
    })
}

fn read_json_or_default(
    path: &std::path::Path,
    default: impl FnOnce() -> serde_json::Value,
) -> Result<serde_json::Value, String> {
    match std::fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw)
            .map_err(|e| format!("{} is malformed: {e}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn write_private(path: &std::path::Path, value: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let body = serde_json::to_vec_pretty(value)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store");
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            // Rename is the authorization commit point. Directory fsync is
            // durability reinforcement, but a failure here must not report
            // that revocation failed after the new authority is already live.
            let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Remove one device under the same transaction lock the native app uses.
/// The authority file is committed before best-effort E2E-key cleanup, so a
/// cleanup failure can leave only an unusable orphan key, never live access.
pub fn unpair_device(device_id: &str) -> Result<(), String> {
    with_device_store_lock(|dir| {
        let devices_path = dir.join("devices.json");
        let mut store = read_json_or_default(
            &devices_path,
            || serde_json::json!({ "version": 1, "devices": [] }),
        )?;
        let devices = store
            .get_mut("devices")
            .and_then(|value| value.as_array_mut())
            .ok_or("devices.json has no devices array")?;
        let before = devices.len();
        devices
            .retain(|device| device.get("id").and_then(|value| value.as_str()) != Some(device_id));
        if devices.len() == before {
            return Err("device not found".into());
        }
        write_private(&devices_path, &store).map_err(|e| format!("devices.json: {e}"))?;

        let keys_path = dir.join("e2e-keys.json");
        if let Ok(mut keys) = read_json_or_default(&keys_path, || serde_json::json!({})) {
            if let Some(map) = keys.as_object_mut() {
                let suffix = format!(".{device_id}");
                map.retain(|key, _| !key.ends_with(&suffix));
                let _ = write_private(&keys_path, &keys);
            }
        }
        Ok(())
    })
}

/// Flip a paired device between Direct-only and Direct + Link without racing
/// a native metadata update or pairing transaction.
pub fn set_device_relay_allowed(device_id: &str, allowed: bool) -> Result<(), String> {
    with_device_store_lock(|dir| {
        let devices_path = dir.join("devices.json");
        let mut store = read_json_or_default(
            &devices_path,
            || serde_json::json!({ "version": 1, "devices": [] }),
        )?;
        let devices = store
            .get_mut("devices")
            .and_then(|value| value.as_array_mut())
            .ok_or("devices.json has no devices array")?;
        let device = devices
            .iter_mut()
            .find(|device| device.get("id").and_then(|value| value.as_str()) == Some(device_id))
            .ok_or("device not found")?;
        let map = device
            .as_object_mut()
            .ok_or("device record is not an object")?;
        if allowed {
            map.remove("relayAllowed");
        } else {
            map.insert("relayAllowed".into(), serde_json::Value::Bool(false));
        }
        write_private(&devices_path, &store).map_err(|e| format!("devices.json: {e}"))
    })
}

/// Sanitized paired-Controller rows for same-user Host management clients.
/// Authorization hashes and E2E material never cross `host.sock`.
pub fn device_summaries() -> Result<Vec<serde_json::Value>, String> {
    with_device_store_lock(|dir| {
        let store = read_json_or_default(
            &dir.join("devices.json"),
            || serde_json::json!({ "version": 1, "devices": [] }),
        )?;
        let devices = store
            .get("devices")
            .and_then(serde_json::Value::as_array)
            .ok_or("devices.json has no devices array")?;
        let summaries = devices
            .iter()
            .take(256)
            .filter_map(|device| {
                let id = device.get("id")?.as_str()?;
                let name = device.get("name")?.as_str()?;
                let platform = device.get("platform")?.as_str()?;
                let paired_at = device.get("pairedAtUnixMs")?.as_i64()?;
                let mut summary = serde_json::json!({
                    "id": id,
                    "name": name,
                    "platform": platform,
                    "pairedAtUnixMs": paired_at,
                });
                let object = summary.as_object_mut()?;
                if let Some(version) = device.get("appVersion").and_then(serde_json::Value::as_str)
                {
                    object.insert("appVersion".into(), version.into());
                }
                if let Some(last_seen) = device
                    .get("lastSeenAtUnixMs")
                    .and_then(serde_json::Value::as_i64)
                {
                    object.insert("lastSeenAtUnixMs".into(), last_seen.into());
                }
                if device
                    .get("relayAllowed")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
                {
                    object.insert("relayAllowed".into(), false.into());
                }
                Some(summary)
            })
            .collect();
        Ok(summaries)
    })
}

/// Render the QR as unicode half-blocks sized for a terminal.
pub fn qr_lines(code: &str) -> Vec<String> {
    use qrcode::{EcLevel, QrCode};
    let Ok(qr) = QrCode::with_error_correction_level(code.as_bytes(), EcLevel::L) else {
        return vec!["(failed to render QR)".into()];
    };
    let width = qr.width();
    let modules: Vec<bool> = qr
        .to_colors()
        .iter()
        .map(|c| *c == qrcode::Color::Dark)
        .collect();
    let quiet = 2usize;
    let full = width + quiet * 2;
    let dark = |x: usize, y: usize| -> bool {
        if x < quiet || y < quiet || x >= quiet + width || y >= quiet + width {
            return false;
        }
        modules[(y - quiet) * width + (x - quiet)]
    };
    let mut lines = Vec::new();
    let mut y = 0;
    while y < full {
        let mut line = String::new();
        for x in 0..full {
            let top = dark(x, y);
            let bottom = if y + 1 < full { dark(x, y + 1) } else { false };
            // Dark modules must render as the LIGHT terminal color for
            // scanners: use inverted half blocks on a light background.
            line.push(match (top, bottom) {
                (true, true) => ' ',
                (true, false) => '▄',
                (false, true) => '▀',
                (false, false) => '█',
            });
        }
        lines.push(line);
        y += 2;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc4648_uppercase_unpadded() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_encode(&[0u8; 16]).len(), 26);
    }

    #[test]
    fn seal_open_round_trip_with_aad_binding() {
        let token = "K7ZP2Q4RSTUVWXYZABCDEFGH23";
        let mac = "b5b9a1ff-c0e2-42f1-9801-316d331ddfd3";
        let endpoint = "http://192.168.1.20:49152/mobile";
        // Our own seal uses the response direction, so round-trip through a
        // hand-built request envelope instead (mirrors the phone).
        let salt = random_bytes(16);
        let key = derive_key(token, &salt, "phone-to-mac");
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let nonce_bytes = random_bytes(12);
        let plaintext = br#"{"token":"K7ZP2Q4RSTUVWXYZABCDEFGH23"}"#;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext.as_slice(),
                    aad: &associated_data("phone-to-mac", mac, endpoint),
                },
            )
            .unwrap();
        let mut combined = nonce_bytes.clone();
        combined.extend_from_slice(&ciphertext);
        let engine = base64::engine::general_purpose::STANDARD;
        let envelope = serde_json::json!({
            "v": 1,
            "saltB64": engine.encode(&salt),
            "sealedB64": engine.encode(&combined),
        });
        let opened = open_envelope(&envelope, token, mac, endpoint).expect("opens");
        assert_eq!(opened, plaintext);
        // Wrong macID (AAD) must fail.
        assert!(open_envelope(&envelope, token, "other-mac", endpoint).is_none());
        // Wrong endpoint (AAD) must fail.
        assert!(open_envelope(&envelope, token, mac, "http://x/mobile").is_none());
        // Wrong token (key) must fail.
        assert!(open_envelope(&envelope, "WRONGTOKEN", mac, endpoint).is_none());
    }

    #[test]
    fn qr_code_shape() {
        let window = PairingWindow::default();
        let (code, token) = window
            .begin(
                "192.168.1.20",
                49152,
                "b5b9a1ff-c0e2-42f1-9801-316d331ddfd3",
            )
            .expect("opens");
        let parts: Vec<&str> = code.split(':').collect();
        assert_eq!(parts[0], "UNPEEL");
        assert_eq!(parts[1], "1");
        assert_eq!(parts[2], "192.168.1.20");
        assert_eq!(parts[3], "49152");
        assert_eq!(parts[4], "B5B9A1FF-C0E2-42F1-9801-316D331DDFD3");
        assert_eq!(parts[5], token);
        assert_eq!(token.len(), 26);
        assert!(parts[6].parse::<u64>().unwrap() > 1_700_000_000);
        assert!(window.is_open());
        assert!(!window.completed());
        window.cancel();
        assert!(!window.is_open());
        assert!(!window.completed());
        // IPv6 hosts have no compact encoding.
        assert!(window.begin("fe80::1", 1, "mac").is_none());
    }

    #[test]
    fn controller_assisted_invitation_binds_proxy_and_direct_endpoints() {
        let window = PairingWindow::default();
        let proxy = "http://192.168.1.20:49152/mobile/pairing-proxy/INVITE-1";
        let direct = "http://10.0.0.8:17661/mobile";
        let payload = window
            .begin_invitation(proxy, direct, "host-1", "Upstash Host")
            .expect("opens invitation");

        assert_eq!(payload["endpoint"], proxy);
        assert_eq!(payload["macID"], "host-1");
        let state = window.state.lock().unwrap();
        let active = state.active.as_ref().expect("active pairing");
        assert_eq!(active.endpoint, proxy);
        assert_eq!(active.direct_endpoint, direct);
        drop(state);

        assert!(window
            .begin_invitation(
                "http://192.168.1.20:49152/not-mobile/pairing-proxy/INVITE-1",
                direct,
                "host-1",
                "Host",
            )
            .is_none());
        assert!(window
            .begin_invitation(
                "http://192.168.1.20/mobile/pairing-proxy/INVITE-1",
                direct,
                "host-1",
                "Host",
            )
            .is_none());
    }

    #[test]
    fn successful_window_waits_for_response_write_completion() {
        let window = PairingWindow::default();
        window.state.lock().unwrap().response_pending = true;
        assert!(window.is_open());
        assert!(!window.completed());

        window.finish_response(true);

        assert!(!window.is_open());
        assert!(window.completed());
    }

    #[test]
    fn token_and_hash_encodings() {
        let token = random_token();
        assert_eq!(token.len(), 43); // 32 bytes, base64url unpadded
        assert!(!token.contains('=') && !token.contains('+') && !token.contains('/'));
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn private_store_write_is_atomic_and_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "unpeel-pairing-writer-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let path = dir.join("devices.json");
        let value = serde_json::json!({ "version": 1, "devices": [] });

        write_private(&path, &value).expect("commit private store");
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read committed store"))
                .expect("valid committed json");
        assert_eq!(persisted, value);
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(&dir)
                .expect("read fixture dir")
                .filter_map(Result::ok)
                .count(),
            1,
            "successful commit must not leave a temporary credential file"
        );

        std::fs::remove_dir_all(&dir).expect("remove fixture dir");
    }
}
