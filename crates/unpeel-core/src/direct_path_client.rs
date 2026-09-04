//! Controller-side direct-path negotiation — the client half of
//! `unpeel-apple:docs/feature/direct-path-v1.md`, transport-agnostic by design: the
//! caller supplies (a) the connection's E2E handshake material and (b) a
//! closure that POSTs one authenticated tunnel request, so the same
//! negotiator rides the Swift-carried relay (via bridge), a future Rust
//! relay downlink, SSH stdio, or a test harness without caring which.
//!
//! The flow: mint a pathSession, bind the punch socket, gather candidates
//! (local interfaces + optional STUN), POST the offer to
//! `/mobile/direct-path`, strictly validate the Host's answer, run the
//! punch to a proven round trip or timeout, report via
//! `/mobile/direct-path-result`, and hand the punched 4-tuple (and its
//! socket) back to the caller. Migration — a fresh bootstrap over that
//! socket under a new connection generation — is deliberately NOT here.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::direct_path::{
    encode_path_offer, encode_path_result, parse_path_offer_strict, CandidateKind, FailureReason,
    PathCandidate, PathOffer, PathOutcome, PathResult, PathRole, MAX_CANDIDATES,
};
use crate::direct_path_punch::{
    local_candidate_addresses, now_unix_ms, path_session_bytes, probe_key, random_path_session,
    stun_reflexive_address, ProbeDirection, PunchSession, PunchState, PATH_SESSION_LIFETIME,
    STUN_SERVER,
};

/// The connection's E2E handshake inputs — the same triple the Host
/// retains. On a Swift-carried connection these cross the bridge once per
/// negotiation and are never persisted or logged.
pub struct ProbeMaterial {
    pub shared_secret: Vec<u8>,
    pub client_salt: Vec<u8>,
    pub host_salt: Vec<u8>,
}

/// One authenticated tunnel POST: `(path, body) -> (status, response body)`.
/// The transport owns auth, framing, and retries-never semantics.
pub type TunnelPost<'a> = &'a mut dyn FnMut(&str, &[u8]) -> Result<(u16, Vec<u8>), String>;

pub struct NegotiatedPath {
    /// The punched socket — already bound to the local port the Host knows.
    /// The caller migrates by bootstrapping a new connection generation
    /// over it; dropping it abandons the path.
    pub socket: UdpSocket,
    pub peer: std::net::SocketAddr,
    pub rtt: Option<Duration>,
    pub path_session: String,
}

#[derive(Debug)]
pub enum NegotiateError {
    /// The Host answered but the punch never proved a round trip; the
    /// failure was reported and the caller stays on the current transport.
    PunchFailed(FailureReason),
    /// The negotiate operation itself failed (transport error, Host
    /// refusal, malformed answer). Includes the coarse status when known.
    Operation(String),
}

impl std::fmt::Display for NegotiateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NegotiateError::PunchFailed(reason) => write!(f, "punch failed: {reason:?}"),
            NegotiateError::Operation(message) => write!(f, "{message}"),
        }
    }
}

pub struct NegotiateOptions {
    pub use_stun: bool,
    /// Overall cap on the punch phase; clamped to the contract's
    /// path-session lifetime.
    pub punch_timeout: Duration,
}

impl Default for NegotiateOptions {
    fn default() -> Self {
        Self {
            use_stun: true,
            punch_timeout: Duration::from_secs(10),
        }
    }
}

/// Negotiate and punch. Blocking (bounded by `punch_timeout`); run it off
/// the caller's UI/owner thread. `cancelled` is checked each pump tick so a
/// scope change or connection loss can abandon the punch immediately.
pub fn negotiate_and_punch(
    post: TunnelPost<'_>,
    material: &ProbeMaterial,
    options: &NegotiateOptions,
    cancelled: &AtomicBool,
) -> Result<NegotiatedPath, NegotiateError> {
    let (session_bytes, path_session) = random_path_session();
    let socket = UdpSocket::bind(("0.0.0.0", 0))
        .map_err(|e| NegotiateError::Operation(format!("bind: {e}")))?;
    let local_port = socket
        .local_addr()
        .map_err(|e| NegotiateError::Operation(format!("bind: {e}")))?
        .port();

    let mut candidates: Vec<PathCandidate> = local_candidate_addresses()
        .into_iter()
        .map(|address| PathCandidate {
            kind: CandidateKind::Local,
            address,
            port: local_port,
        })
        .collect();
    if options.use_stun {
        if let Ok(reflexive) =
            stun_reflexive_address(&socket, STUN_SERVER, Duration::from_millis(1500))
        {
            let candidate = PathCandidate {
                kind: CandidateKind::Reflexive,
                address: reflexive.ip(),
                port: reflexive.port(),
            };
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates.truncate(MAX_CANDIDATES);
    if candidates.is_empty() {
        return Err(NegotiateError::Operation(
            "no usable local candidates".into(),
        ));
    }

    let offer = PathOffer {
        path_session: path_session.clone(),
        role: PathRole::Controller,
        candidates,
    };
    let (status, body) = post("/mobile/direct-path", &encode_path_offer(&offer))
        .map_err(NegotiateError::Operation)?;
    if status != 200 {
        return Err(NegotiateError::Operation(format!(
            "negotiate refused with status {status}"
        )));
    }
    let host_offer = parse_path_offer_strict(&body)
        .map_err(|error| NegotiateError::Operation(format!("host offer rejected: {error}")))?;
    if host_offer.role != PathRole::Host {
        return Err(NegotiateError::Operation(
            "host offer has wrong role".into(),
        ));
    }
    if host_offer.path_session != path_session {
        return Err(NegotiateError::Operation(
            "host offer is for a different path session".into(),
        ));
    }

    let key = probe_key(
        &material.shared_secret,
        &material.client_salt,
        &material.host_salt,
        &path_session,
    );
    let session_bytes = path_session_bytes(&path_session).unwrap_or(session_bytes);
    let mut punch = PunchSession::new(
        key,
        session_bytes,
        ProbeDirection::ControllerToHost,
        host_offer.candidates,
        local_port,
    );
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    let deadline = std::time::Instant::now() + options.punch_timeout.min(PATH_SESSION_LIFETIME);
    let mut buffer = [0u8; 128];
    let outcome = loop {
        if cancelled.load(Ordering::Acquire) {
            break PathOutcome::Failed(FailureReason::TransportRejected);
        }
        if std::time::Instant::now() >= deadline {
            break PathOutcome::Failed(FailureReason::PunchTimeout);
        }
        let now = now_unix_ms();
        for (target, probe) in punch.tick(now) {
            let _ = socket.send_to(&probe, target);
        }
        if let Ok((length, from)) = socket.recv_from(&mut buffer) {
            if let Some((target, answer)) = punch.handle_datagram(&buffer[..length], from, now) {
                let _ = socket.send_to(&answer, target);
            }
        }
        match punch.state() {
            PunchState::Established(peer) => {
                break PathOutcome::Established(PathCandidate {
                    kind: CandidateKind::Reflexive,
                    address: peer.ip(),
                    port: peer.port(),
                })
            }
            PunchState::TimedOut => break PathOutcome::Failed(FailureReason::PunchTimeout),
            PunchState::Probing => {}
        }
    };

    // Report honestly either way; the result is advisory telemetry, so a
    // failed report never un-establishes a punched path.
    let result = PathResult {
        path_session: path_session.clone(),
        outcome: outcome.clone(),
    };
    let _ = post("/mobile/direct-path-result", &encode_path_result(&result));

    match outcome {
        PathOutcome::Established(candidate) => {
            // Answer the Host's own probes briefly so its round trip
            // completes even if ours finished first.
            let linger_deadline = std::time::Instant::now() + Duration::from_millis(1500);
            while std::time::Instant::now() < linger_deadline {
                let now = now_unix_ms();
                if let Ok((length, from)) = socket.recv_from(&mut buffer) {
                    if let Some((target, answer)) =
                        punch.handle_datagram(&buffer[..length], from, now)
                    {
                        let _ = socket.send_to(&answer, target);
                        break;
                    }
                } else {
                    break;
                }
            }
            Ok(NegotiatedPath {
                rtt: punch.rtt(),
                socket,
                peer: std::net::SocketAddr::new(candidate.address, candidate.port),
                path_session,
            })
        }
        PathOutcome::Failed(reason) => Err(NegotiateError::PunchFailed(reason)),
    }
}
