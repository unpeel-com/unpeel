//! `unpeel link` — scripted headless Unpeel Link enrollment and status.
//!
//! This is the SSH/provisioning spelling of the interactive TUI Settings ▸
//! Remote activation path. It deliberately reuses the exact
//! `unpeel_core::license` request/commit primitives (and therefore the same
//! locked durable suppression record), so deactivation and authoritative
//! rejection semantics are identical to the interactive path — never a
//! second activation implementation, and never a client-side gate on
//! anything local.
//!
//! Exit codes are script-meaningful:
//!   0  success (`status`: enrolled with usable Link authority)
//!   1  definitive failure (invalid key, rejection, usage, local commit error)
//!   2  transient failure — retry later (network/service outage; for
//!      `enroll` the activation may already be durably committed, see below)

use unpeel_core::license;
use unpeel_core::relay_uplink;

pub const HELP: &str = "\
unpeel link — Unpeel Link enrollment for this Host machine

  unpeel link enroll <key> [--json]   activate this machine and fetch its
                                      relay entitlement (idempotent)
  unpeel link status [--json]         show enrollment and entitlement state
  unpeel link deactivate              release this machine's seat and stop Link

Runs on the Host machine (the box that runs `unpeel serve`). A running
Host service picks the new entitlement up on its next tick — no restart.
Local/LAN use never requires enrollment; Link only adds off-LAN relay access.

Exit codes: 0 success · 1 definitive failure · 2 transient (retry).
If `enroll` exits 2 after \"activation committed\", the key is stored durably
and the entitlement finishes on retry or automatically once a Host service
can reach the licensing service.";

fn device_name() -> String {
    hostname().unwrap_or_else(|| "unpeel (terminal)".into())
}

fn hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let end = buffer.iter().position(|byte| *byte == 0)?;
    let name = std::str::from_utf8(&buffer[..end]).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn cache_state_word(state: relay_uplink::EntitlementCacheState) -> &'static str {
    match state {
        relay_uplink::EntitlementCacheState::Missing => "missing",
        relay_uplink::EntitlementCacheState::HostMismatch => "host-mismatch",
        relay_uplink::EntitlementCacheState::Fresh => "fresh",
        relay_uplink::EntitlementCacheState::RefreshDue => "refresh-due",
        relay_uplink::EntitlementCacheState::Expired => "expired",
    }
}

fn tombstone_word(reason: license::LinkTombstoneReason) -> &'static str {
    match reason {
        license::LinkTombstoneReason::UserDisabled => "user-disabled",
        license::LinkTombstoneReason::ActivationPending => "activation-pending",
        license::LinkTombstoneReason::AuthorizationRejected => "authorization-rejected",
    }
}

struct LinkState {
    payload: Option<license::LicensePayload>,
    tombstone: Option<license::LinkTombstoneReason>,
    host_id: Option<String>,
    cache: Option<relay_uplink::CachedEntitlement>,
    cache_state: Option<relay_uplink::EntitlementCacheState>,
}

/// Read-only snapshot: never mints a Host identity or mutates any Link file.
fn read_state() -> Result<LinkState, String> {
    let payload = license::stored().map(|(_, payload)| payload);
    let tombstone = license::link_tombstone_reason()?;
    let host_id = license::known_mac_id();
    let cache = relay_uplink::cached_entitlement_record();
    let cache_state = host_id
        .as_deref()
        .map(relay_uplink::entitlement_cache_state);
    Ok(LinkState {
        payload,
        tombstone,
        host_id,
        cache,
        cache_state,
    })
}

fn state_json(state: &LinkState) -> serde_json::Value {
    serde_json::json!({
        "enrolled": state.payload.is_some(),
        "license": state.payload.as_ref().map(|payload| serde_json::json!({
            "email": payload.email,
            "plan": payload.plan,
            "seats": payload.seats,
        })),
        "suppressed": state.tombstone.map(tombstone_word),
        "host_id": state.host_id,
        "entitlement": state.cache_state.map(cache_state_word),
        "entitlement_expires_at": state
            .cache
            .as_ref()
            .filter(|record| Some(record.mac_id.as_str()) == state.host_id.as_deref())
            .map(|record| record.expires_at),
    })
}

fn print_state(state: &LinkState, json: bool) {
    if json {
        println!("{}", state_json(state));
        return;
    }
    match &state.payload {
        Some(payload) => println!(
            "license: {} ({}, {} seat{})",
            payload.email,
            payload.plan,
            payload.seats,
            if payload.seats == 1 { "" } else { "s" }
        ),
        None => println!("license: none stored"),
    }
    match state.tombstone {
        Some(reason) => println!("link: disabled ({})", tombstone_word(reason)),
        None => println!("link: enabled"),
    }
    match &state.host_id {
        Some(host_id) => println!("host id: {host_id}"),
        None => println!("host id: not yet minted (pair a device or run `unpeel serve`)"),
    }
    match (state.cache_state, &state.cache) {
        (Some(cache_state), Some(record))
            if Some(record.mac_id.as_str()) == state.host_id.as_deref() =>
        {
            let days = (record.expires_at - now_secs()).max(0) / (24 * 60 * 60);
            println!(
                "relay entitlement: {} (expires in {days} day{})",
                cache_state_word(cache_state),
                if days == 1 { "" } else { "s" }
            );
        }
        (Some(cache_state), _) => {
            println!("relay entitlement: {}", cache_state_word(cache_state))
        }
        (None, _) => println!("relay entitlement: unknown (no Host identity yet)"),
    }
}

/// `unpeel link status` — exit 0 when this Host holds usable Link authority
/// (stored valid key, no suppression, entitlement present for this Host and
/// not expired), 1 otherwise.
fn status(json: bool) -> i32 {
    let state = match read_state() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    print_state(&state, json);
    let usable = state.payload.is_some()
        && state.tombstone.is_none()
        && matches!(
            state.cache_state,
            Some(
                relay_uplink::EntitlementCacheState::Fresh
                    | relay_uplink::EntitlementCacheState::RefreshDue
            )
        );
    if usable {
        0
    } else {
        1
    }
}

/// `unpeel link enroll <key>` — the scripted equivalent of pasting the key
/// in Settings ▸ Remote: seat activation, durable key commit, then the first
/// relay entitlement. Safe to re-run; the service treats re-activation of
/// the same machine as idempotent and a re-run simply refreshes.
fn enroll(raw_key: &str, json: bool) -> i32 {
    // Same durable identity every other Link consumer binds to.
    let mac_id = match relay_uplink::ensure_host_id() {
        Ok(mac_id) => mac_id,
        Err(error) => {
            eprintln!("could not establish this Host's identity: {error}");
            return 1;
        }
    };
    let pending = match license::request_activation(raw_key, &device_name()) {
        Ok(pending) => pending,
        Err(error) => {
            eprintln!("activation failed: {error}");
            return 1;
        }
    };
    let activation = match license::commit_activation(&pending) {
        Ok(activation) => activation,
        Err(error) => {
            // The seat was already taken on the service side; a local state
            // change (a newer user disable, a concurrent commit) won, so
            // release that seat instead of leaving it orphaned — the same
            // thing the interactive activation did.
            match license::request_deactivation_for_key(&pending.key) {
                Ok(()) => eprintln!("activation could not be saved: {error} (seat released)"),
                Err(release_error) => eprintln!(
                    "activation could not be saved: {error}; the Link seat could not be released: {release_error}"
                ),
            }
            return 1;
        }
    };
    if !json {
        println!("activation committed for {}", pending.payload.email);
    }
    let entitlement = match license::request_relay_entitlement_for_key(&mac_id, &pending.key) {
        Ok(entitlement) => entitlement,
        Err(error) if error.is_rejected() => {
            // Same authoritative-rejection semantics as the interactive
            // path: fail closed durably; cached authority cannot survive.
            if let Err(suppress_error) = license::reject_relay_entitlement() {
                eprintln!("Link could not be suppressed after rejection: {suppress_error}");
            }
            eprintln!("Link authorization rejected: {error}");
            return 1;
        }
        Err(error) => {
            // The activation is durably committed (activation_pending); a
            // retry or a running Host service finishes it once the service
            // is reachable again.
            eprintln!("relay entitlement request failed: {error}");
            eprintln!("the key is stored; re-run `unpeel link enroll` or leave `unpeel serve` running to finish");
            return 2;
        }
    };
    if let Err(error) =
        license::commit_relay_entitlement_for_activation(&pending.key, &entitlement, &activation)
    {
        eprintln!("relay entitlement could not be saved: {error}");
        return 1;
    }
    match read_state() {
        Ok(state) => print_state(&state, json),
        Err(error) => eprintln!("enrolled, but state could not be re-read: {error}"),
    }
    if !json {
        println!("enrolled — a running `unpeel serve` starts Link on its next tick");
    }
    0
}

/// `unpeel link deactivate` — local revocation first (durable user-disable
/// marker, cache and key removal), then the best-effort seat release.
fn deactivate() -> i32 {
    let key = match license::deactivate_local() {
        Ok(key) => key,
        Err(error) => {
            eprintln!("deactivation could not finish locally: {error}");
            return 1;
        }
    };
    println!("Unpeel Link deactivated on this machine");
    if let Some(key) = key {
        if let Err(error) = license::request_deactivation_for_key(&key) {
            eprintln!("seat release did not reach the service: {error}");
            eprintln!("this machine is already disabled locally; free the seat later from unpeel.com/account");
            return 2;
        }
        println!("seat released");
    }
    0
}

/// Dispatch `unpeel link …`. `args` excludes the leading `link`.
pub fn run(args: &[String], json: bool) -> i32 {
    match args.first().map(String::as_str) {
        Some("enroll") => match args.get(1) {
            Some(key) if !key.trim().is_empty() => enroll(key, json),
            _ => {
                eprintln!("usage: unpeel link enroll <key>");
                1
            }
        },
        Some("status") | None => status(json),
        Some("deactivate") => deactivate(),
        Some("--help" | "-h" | "help") => {
            println!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("unknown link subcommand: {other}\n\n{HELP}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_stable_script_vocabulary() {
        assert_eq!(
            cache_state_word(relay_uplink::EntitlementCacheState::Fresh),
            "fresh"
        );
        assert_eq!(
            cache_state_word(relay_uplink::EntitlementCacheState::HostMismatch),
            "host-mismatch"
        );
        assert_eq!(
            tombstone_word(license::LinkTombstoneReason::AuthorizationRejected),
            "authorization-rejected"
        );
    }

    #[test]
    fn device_name_never_empty() {
        assert!(!device_name().trim().is_empty());
    }
}
