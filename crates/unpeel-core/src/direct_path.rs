//! Direct-path negotiation contract (v1) — the frozen message layer for
//! the private "relay-direct-upgrade" design record increment 1. Candidate exchange
//! rides the existing sealed tunnel envelope as capability-gated Host
//! operations, so the Relay never sees an address; this module owns only
//! the strict codec and address policy. Contract and threat model:
//! the private "direct-path-v1" design record. Fixtures: `protocol/direct-path-v1.json`.
//!
//! Deliberately crypto-free and std-only (like `relay_wire`) so every
//! Controller build — including wasm — compiles the exact contract even
//! where the punched transport itself cannot exist.

use std::net::IpAddr;

pub const DIRECT_PATH_VERSION: u64 = 1;
pub const MAX_CANDIDATES: usize = 16;
/// `pathSession` is 16 random bytes, wire-encoded as exactly 32 lowercase hex.
pub const PATH_SESSION_HEX_LEN: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateKind {
    /// A locally bound interface address.
    Local,
    /// A server-reflexive address learned via STUN.
    Reflexive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathRole {
    Controller,
    Host,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathCandidate {
    pub kind: CandidateKind,
    pub address: IpAddr,
    pub port: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathOffer {
    pub path_session: String,
    pub role: PathRole,
    pub candidates: Vec<PathCandidate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureReason {
    NoCandidates,
    PunchTimeout,
    ProbeAuthFailed,
    TransportRejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathOutcome {
    Established(PathCandidate),
    Failed(FailureReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathResult {
    pub path_session: String,
    pub outcome: PathOutcome,
}

/// Address policy for punch candidates. Numeric unicast only; every
/// rejected class is either meaningless to punch toward or a laundering
/// vector (the same families the forwarding plan's target policy rejects).
pub fn validate_candidate_address(address: IpAddr) -> Result<(), String> {
    if address.is_loopback() {
        return Err("loopback candidates are invalid".into());
    }
    if address.is_unspecified() {
        return Err("unspecified candidates are invalid".into());
    }
    if address.is_multicast() {
        return Err("multicast candidates are invalid".into());
    }
    match address {
        IpAddr::V4(v4) => {
            if v4.is_broadcast() {
                return Err("broadcast candidates are invalid".into());
            }
            if v4.is_link_local() {
                return Err("link-local candidates are invalid".into());
            }
        }
        IpAddr::V6(v6) => {
            if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                return Err("link-local candidates are invalid".into());
            }
            if v6.to_ipv4_mapped().is_some() || v6.to_ipv4().is_some() {
                return Err("v4-in-v6 candidates are invalid; send the IPv4 form".into());
            }
        }
    }
    Ok(())
}

fn parse_candidate(value: &serde_json::Value) -> Result<PathCandidate, String> {
    let object = value.as_object().ok_or("candidate must be an object")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "kind" | "address" | "port") {
            return Err(format!("unknown candidate field: {key}"));
        }
    }
    let kind = match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("local") => CandidateKind::Local,
        Some("reflexive") => CandidateKind::Reflexive,
        _ => return Err("kind must be \"local\" or \"reflexive\"".into()),
    };
    let text = object
        .get("address")
        .and_then(serde_json::Value::as_str)
        .ok_or("address must be a string")?;
    let address: IpAddr = text
        .parse()
        .map_err(|_| "address must be a numeric IP literal".to_string())?;
    // Canonical form only: "010.0.0.1", zone ids, and mixed-case or
    // expanded IPv6 spellings must not create second spellings of one
    // address (dedup and audit compare strings).
    if address.to_string() != text {
        return Err("address must be in canonical form".into());
    }
    validate_candidate_address(address)?;
    let port = object
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .ok_or("port must be an unsigned integer")?;
    if port == 0 || port > u64::from(u16::MAX) {
        return Err("port must be 1..=65535".into());
    }
    Ok(PathCandidate {
        kind,
        address,
        port: port as u16,
    })
}

fn candidate_json(candidate: &PathCandidate) -> serde_json::Value {
    serde_json::json!({
        "kind": match candidate.kind {
            CandidateKind::Local => "local",
            CandidateKind::Reflexive => "reflexive",
        },
        "address": candidate.address.to_string(),
        "port": candidate.port,
    })
}

fn require_version(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    match object.get("v").and_then(serde_json::Value::as_u64) {
        Some(DIRECT_PATH_VERSION) => Ok(()),
        _ => Err("unsupported direct-path version".into()),
    }
}

fn require_path_session(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, String> {
    let value = object
        .get("pathSession")
        .and_then(serde_json::Value::as_str)
        .ok_or("pathSession must be a string")?;
    if value.len() != PATH_SESSION_HEX_LEN
        || !value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err("pathSession must be 32 lowercase hex characters".into());
    }
    Ok(value.to_string())
}

pub fn encode_path_offer(offer: &PathOffer) -> Vec<u8> {
    serde_json::json!({
        "v": DIRECT_PATH_VERSION,
        "pathSession": offer.path_session,
        "role": match offer.role {
            PathRole::Controller => "controller",
            PathRole::Host => "host",
        },
        "candidates": offer
            .candidates
            .iter()
            .map(candidate_json)
            .collect::<Vec<_>>(),
    })
    .to_string()
    .into_bytes()
}

/// Strict decode: unknown fields, wrong version, and every non-canonical
/// or policy-violating candidate reject the whole message. Additive
/// evolution bumps `v`; there is no lenient dialect.
pub fn parse_path_offer_strict(body: &[u8]) -> Result<PathOffer, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "offer is not valid JSON")?;
    let object = value.as_object().ok_or("offer must be an object")?;
    for key in object.keys() {
        if !matches!(key.as_str(), "v" | "pathSession" | "role" | "candidates") {
            return Err(format!("unknown offer field: {key}"));
        }
    }
    require_version(object)?;
    let path_session = require_path_session(object)?;
    let role = match object.get("role").and_then(serde_json::Value::as_str) {
        Some("controller") => PathRole::Controller,
        Some("host") => PathRole::Host,
        _ => return Err("role must be \"controller\" or \"host\"".into()),
    };
    let raw = object
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .ok_or("candidates must be an array")?;
    if raw.is_empty() || raw.len() > MAX_CANDIDATES {
        return Err(format!("candidates must be 1..={MAX_CANDIDATES}"));
    }
    let mut candidates = Vec::with_capacity(raw.len());
    for value in raw {
        let candidate = parse_candidate(value)?;
        if candidates.contains(&candidate) {
            return Err("duplicate candidate".into());
        }
        candidates.push(candidate);
    }
    Ok(PathOffer {
        path_session,
        role,
        candidates,
    })
}

pub fn encode_path_result(result: &PathResult) -> Vec<u8> {
    let mut object = serde_json::json!({
        "v": DIRECT_PATH_VERSION,
        "pathSession": result.path_session,
    });
    match &result.outcome {
        PathOutcome::Established(candidate) => {
            object["outcome"] = "established".into();
            object["chosen"] = candidate_json(candidate);
        }
        PathOutcome::Failed(reason) => {
            object["outcome"] = "failed".into();
            object["reason"] = match reason {
                FailureReason::NoCandidates => "no_candidates",
                FailureReason::PunchTimeout => "punch_timeout",
                FailureReason::ProbeAuthFailed => "probe_auth_failed",
                FailureReason::TransportRejected => "transport_rejected",
            }
            .into();
        }
    }
    object.to_string().into_bytes()
}

pub fn parse_path_result_strict(body: &[u8]) -> Result<PathResult, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "result is not valid JSON")?;
    let object = value.as_object().ok_or("result must be an object")?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "v" | "pathSession" | "outcome" | "chosen" | "reason"
        ) {
            return Err(format!("unknown result field: {key}"));
        }
    }
    require_version(object)?;
    let path_session = require_path_session(object)?;
    let outcome = match object.get("outcome").and_then(serde_json::Value::as_str) {
        Some("established") => {
            if object.contains_key("reason") {
                return Err("established result must not carry a reason".into());
            }
            let chosen = object
                .get("chosen")
                .ok_or("established result needs chosen")?;
            PathOutcome::Established(parse_candidate(chosen)?)
        }
        Some("failed") => {
            if object.contains_key("chosen") {
                return Err("failed result must not carry chosen".into());
            }
            let reason = match object.get("reason").and_then(serde_json::Value::as_str) {
                Some("no_candidates") => FailureReason::NoCandidates,
                Some("punch_timeout") => FailureReason::PunchTimeout,
                Some("probe_auth_failed") => FailureReason::ProbeAuthFailed,
                Some("transport_rejected") => FailureReason::TransportRejected,
                _ => return Err("unknown failure reason".into()),
            };
            PathOutcome::Failed(reason)
        }
        _ => return Err("outcome must be \"established\" or \"failed\"".into()),
    };
    Ok(PathResult {
        path_session,
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../protocol/direct-path-v1.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
            .expect("fixture json")
    }

    #[test]
    fn fixture_cases_decode_exactly_as_recorded() {
        let file = fixtures();
        assert_eq!(file["schemaVersion"], 1);
        let cases = file["cases"].as_array().expect("cases");
        assert!(cases.len() >= 12, "fixture coverage shrank");
        for case in cases {
            let id = case["id"].as_str().expect("case id");
            let body = case["json"].to_string().into_bytes();
            let valid = case["valid"].as_bool().expect("valid flag");
            let outcome = match case["message"].as_str().expect("message kind") {
                "offer" => parse_path_offer_strict(&body).map(|_| ()),
                "result" => parse_path_result_strict(&body).map(|_| ()),
                other => panic!("unknown message kind {other} in {id}"),
            };
            assert_eq!(
                outcome.is_ok(),
                valid,
                "case {id}: expected valid={valid}, got {outcome:?}"
            );
        }
    }

    #[test]
    fn offer_round_trips_through_its_own_encoder() {
        let offer = PathOffer {
            path_session: "0123456789abcdef0123456789abcdef".into(),
            role: PathRole::Controller,
            candidates: vec![
                PathCandidate {
                    kind: CandidateKind::Local,
                    address: "192.168.1.20".parse().unwrap(),
                    port: 40123,
                },
                PathCandidate {
                    kind: CandidateKind::Reflexive,
                    address: "2001:db8::7".parse().unwrap(),
                    port: 40123,
                },
            ],
        };
        let decoded = parse_path_offer_strict(&encode_path_offer(&offer)).unwrap();
        assert_eq!(decoded, offer);
    }

    #[test]
    fn result_round_trips_both_outcomes() {
        let established = PathResult {
            path_session: "0123456789abcdef0123456789abcdef".into(),
            outcome: PathOutcome::Established(PathCandidate {
                kind: CandidateKind::Reflexive,
                address: "100.64.9.3".parse().unwrap(),
                port: 5123,
            }),
        };
        assert_eq!(
            parse_path_result_strict(&encode_path_result(&established)).unwrap(),
            established
        );
        let failed = PathResult {
            path_session: "0123456789abcdef0123456789abcdef".into(),
            outcome: PathOutcome::Failed(FailureReason::PunchTimeout),
        };
        assert_eq!(
            parse_path_result_strict(&encode_path_result(&failed)).unwrap(),
            failed
        );
    }

    #[test]
    fn address_policy_rejects_laundering_and_meaningless_classes() {
        for bad in [
            "127.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "169.254.10.9",
            "::1",
            "::",
            "ff02::1",
            "fe80::1",
            "::ffff:8.8.8.8",
        ] {
            let address: IpAddr = bad.parse().unwrap();
            assert!(
                validate_candidate_address(address).is_err(),
                "{bad} must be rejected"
            );
        }
        for good in [
            "8.8.8.8",
            "192.168.1.4",
            "10.0.0.9",
            "100.64.0.7",
            "2001:db8::1",
            "fd00::5",
        ] {
            let address: IpAddr = good.parse().unwrap();
            assert!(
                validate_candidate_address(address).is_ok(),
                "{good} must be accepted"
            );
        }
    }
}
