//! Host-side direct-path signaling (`unpeel-apple:docs/plans/relay-direct-upgrade.md`
//! increment 2): serves `POST /mobile/direct-path` (candidate negotiate)
//! and `POST /mobile/direct-path-result` for relay-tunneled Controllers,
//! per the frozen contract in `unpeel-apple:docs/feature/direct-path-v1.md`.
//!
//! Only a relayed connection can negotiate — the probe key derives from
//! that connection's E2E handshake material, which the relay loop registers
//! here per conn id. LAN/SSH requests get a coarse 409 (they are already
//! direct or carry no handshake). Deliberately NOT in the capability
//! ledger yet: the contract reserves `direct.path.negotiate`/`result`, and
//! the ledger advertises only when the native adapter reaches parity.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use unpeel_core::direct_path::{
    encode_path_offer, parse_path_offer_strict, parse_path_result_strict, CandidateKind,
    PathCandidate, PathOffer, PathRole, MAX_CANDIDATES,
};
use unpeel_core::direct_path_punch::{
    local_candidate_addresses, now_unix_ms, path_session_bytes, probe_key, stun_reflexive_address,
    ProbeDirection, PunchSession, PunchState, PATH_SESSION_LIFETIME, STUN_SERVER,
};

/// E2E handshake inputs retained per relay connection, exactly the probe-key
/// ikm/salts from the contract. Useless without a live pathSession and dead
/// with the connection, but still keyed material — never logged.
pub struct ConnMaterial {
    pub shared_secret: Vec<u8>,
    pub client_salt: Vec<u8>,
    pub host_salt: Vec<u8>,
}

struct ActivePath {
    path_session: String,
    cancel: Arc<AtomicBool>,
}

struct ConnEntry {
    material: ConnMaterial,
    active: Option<ActivePath>,
}

#[derive(Default)]
pub struct DirectPathHub {
    connections: Mutex<HashMap<u32, ConnEntry>>,
    /// Skip the STUN query (tests / airgapped): local candidates only.
    pub skip_stun: AtomicBool,
}

fn trace(message: &str) {
    crate::tracelog::trace("direct-path", message);
}

fn error_json(message: &str) -> serde_json::Value {
    serde_json::json!({ "error": message })
}

impl DirectPathHub {
    /// Called by the relay loop when a client handshake completes. A second
    /// hello for the same conn id (incarnation replacement) overwrites and
    /// cancels any punch keyed to the old material.
    pub fn register(&self, conn_id: u32, material: ConnMaterial) {
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(previous) = connections.insert(
            conn_id,
            ConnEntry {
                material,
                active: None,
            },
        ) {
            if let Some(active) = previous.active {
                active.cancel.store(true, Ordering::Release);
            }
        }
    }

    pub fn remove(&self, conn_id: u32) {
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = connections.remove(&conn_id) {
            if let Some(active) = entry.active {
                active.cancel.store(true, Ordering::Release);
            }
        }
    }

    /// `direct.path.negotiate`: the Controller's offer in, the Host's offer
    /// out, with the punch worker started before the response is sent so
    /// the Host is already listening when the Controller starts probing.
    pub fn negotiate(&self, conn_id: u32, body: &[u8]) -> (u16, serde_json::Value) {
        let offer = match parse_path_offer_strict(body) {
            Ok(offer) => offer,
            Err(error) => return (400, error_json(&error)),
        };
        if offer.role != PathRole::Controller {
            return (400, error_json("offer role must be controller"));
        }
        let Some(session_bytes) = path_session_bytes(&offer.path_session) else {
            return (400, error_json("pathSession must be 32 hex characters"));
        };

        let key = {
            let connections = self
                .connections
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(entry) = connections.get(&conn_id) else {
                return (409, error_json("direct path requires a relayed connection"));
            };
            probe_key(
                &entry.material.shared_secret,
                &entry.material.client_salt,
                &entry.material.host_salt,
                &offer.path_session,
            )
        };

        let socket = match UdpSocket::bind(("0.0.0.0", 0)) {
            Ok(socket) => socket,
            Err(error) => return (500, error_json(&format!("bind failed: {error}"))),
        };
        let local_port = match socket.local_addr() {
            Ok(address) => address.port(),
            Err(error) => return (500, error_json(&format!("bind failed: {error}"))),
        };
        let mut candidates: Vec<PathCandidate> = local_candidate_addresses()
            .into_iter()
            .map(|address| PathCandidate {
                kind: CandidateKind::Local,
                address,
                port: local_port,
            })
            .collect();
        if !self.skip_stun.load(Ordering::Acquire) {
            match stun_reflexive_address(&socket, STUN_SERVER, Duration::from_millis(1500)) {
                Ok(reflexive) => {
                    let candidate = PathCandidate {
                        kind: CandidateKind::Reflexive,
                        address: reflexive.ip(),
                        port: reflexive.port(),
                    };
                    if !candidates.contains(&candidate) {
                        candidates.push(candidate);
                    }
                }
                Err(error) => trace(&format!(
                    "stun unavailable ({error}); local candidates only"
                )),
            }
        }
        candidates.truncate(MAX_CANDIDATES);
        if candidates.is_empty() {
            return (500, error_json("no usable host candidates"));
        }

        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut connections = self
                .connections
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(entry) = connections.get_mut(&conn_id) else {
                return (409, error_json("direct path requires a relayed connection"));
            };
            // One live path session per connection: a fresh offer replaces
            // (and cancels) the previous one, per the contract.
            if let Some(previous) = entry.active.take() {
                previous.cancel.store(true, Ordering::Release);
            }
            entry.active = Some(ActivePath {
                path_session: offer.path_session.clone(),
                cancel: Arc::clone(&cancel),
            });
        }

        let host_offer = PathOffer {
            path_session: offer.path_session.clone(),
            role: PathRole::Host,
            candidates,
        };
        spawn_punch_worker(
            socket,
            key,
            session_bytes,
            offer.candidates,
            local_port,
            cancel,
        );
        let response: serde_json::Value =
            serde_json::from_slice(&encode_path_offer(&host_offer)).expect("offer encodes");
        (200, response)
    }

    /// `direct.path.result`: advisory telemetry from the Controller; coarse
    /// by contract and never a security signal.
    pub fn result(&self, conn_id: u32, body: &[u8]) -> (u16, serde_json::Value) {
        let result = match parse_path_result_strict(body) {
            Ok(result) => result,
            Err(error) => return (400, error_json(&error)),
        };
        let known = {
            let connections = self
                .connections
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            connections.get(&conn_id).is_some_and(|entry| {
                entry
                    .active
                    .as_ref()
                    .is_some_and(|active| active.path_session == result.path_session)
            })
        };
        if !known {
            return (409, error_json("unknown path session"));
        }
        trace(&format!(
            "controller reported {:?} for conn {conn_id}",
            result.outcome
        ));
        (200, serde_json::json!({ "ok": true }))
    }
}

/// The Host half of the punch: pump probes/answers until established (then
/// keep answering so the Controller's own round trip completes), timeout,
/// or cancellation. Addresses never reach the log; outcomes do.
fn spawn_punch_worker(
    socket: UdpSocket,
    key: [u8; 32],
    session_bytes: [u8; 16],
    peers: Vec<PathCandidate>,
    local_port: u16,
    cancel: Arc<AtomicBool>,
) {
    let _ = std::thread::Builder::new()
        .name("unpeel-direct-punch".into())
        .spawn(move || {
            let mut punch = PunchSession::new(
                key,
                session_bytes,
                ProbeDirection::HostToController,
                peers,
                local_port,
            );
            socket
                .set_read_timeout(Some(Duration::from_millis(100)))
                .ok();
            let started = std::time::Instant::now();
            let mut buffer = [0u8; 128];
            let mut reported = false;
            while started.elapsed() < PATH_SESSION_LIFETIME {
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                let now = now_unix_ms();
                for (target, probe) in punch.tick(now) {
                    let _ = socket.send_to(&probe, target);
                }
                if let Ok((length, from)) = socket.recv_from(&mut buffer) {
                    if let Some((target, answer)) =
                        punch.handle_datagram(&buffer[..length], from, now)
                    {
                        let _ = socket.send_to(&answer, target);
                    }
                }
                match punch.state() {
                    PunchState::Established(_) if !reported => {
                        reported = true;
                        let rtt = punch
                            .rtt()
                            .map(|rtt| format!("{:.1}ms", rtt.as_secs_f64() * 1000.0))
                            .unwrap_or_else(|| "unknown".into());
                        trace(&format!("host punch established, rtt {rtt}"));
                    }
                    PunchState::TimedOut => {
                        trace("host punch timed out");
                        return;
                    }
                    _ => {}
                }
            }
        });
}

/// Extract the relay conn id from the namespaced request id the relay
/// dispatcher mints (`relay:{namespace}:{conn}:{tunneled id}`). A request
/// that did not come through the relay has no conn id and cannot negotiate.
pub fn relay_conn_id(request_id: Option<&str>) -> Option<u32> {
    let request_id = request_id?;
    let rest = request_id.strip_prefix("relay:")?;
    let (namespaced, _tunneled) = rest.rsplit_once(':')?;
    namespaced.rsplit(':').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unpeel_core::direct_path::PathResult;
    use unpeel_core::direct_path_punch::random_path_session;

    fn hub_with_conn(conn_id: u32) -> Arc<DirectPathHub> {
        let hub = Arc::new(DirectPathHub::default());
        hub.skip_stun.store(true, Ordering::Release);
        hub.register(
            conn_id,
            ConnMaterial {
                shared_secret: vec![7u8; 32],
                client_salt: vec![1u8; 16],
                host_salt: vec![2u8; 16],
            },
        );
        hub
    }

    #[test]
    fn relay_conn_id_parses_only_relay_request_ids() {
        assert_eq!(relay_conn_id(Some("relay:ns:42:7")), Some(42));
        assert_eq!(relay_conn_id(Some("relay:deep:ns:42:7")), Some(42));
        assert_eq!(relay_conn_id(Some("lan-123")), None);
        assert_eq!(relay_conn_id(None), None);
    }

    #[test]
    fn negotiate_rejects_unknown_connections_and_bad_offers() {
        let hub = hub_with_conn(1);
        let (_, session) = random_path_session();
        let offer = format!(
            r#"{{"v":1,"pathSession":"{session}","role":"controller","candidates":[{{"kind":"local","address":"192.0.2.7","port":4000}}]}}"#
        );
        let (status, _) = hub.negotiate(9, offer.as_bytes());
        assert_eq!(status, 409, "unregistered conn must not negotiate");
        let (status, _) = hub.negotiate(1, b"not json");
        assert_eq!(status, 400);
        let host_role = offer.replace("controller", "host");
        let (status, _) = hub.negotiate(1, host_role.as_bytes());
        assert_eq!(status, 400, "host-role offers reject");
    }

    #[test]
    fn full_negotiate_then_real_punch_against_the_host_worker() {
        let hub = hub_with_conn(1);
        let (session_bytes, session) = random_path_session();

        // Controller side: a real socket whose candidate goes in the offer.
        let controller_socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let controller_port = controller_socket.local_addr().expect("addr").port();
        // Loopback candidates are policy-rejected on the wire, so the test
        // offers the port under an allowed address and rewrites the target
        // below — the punch engine takes SocketAddrs, the codec owns policy.
        let offer = format!(
            r#"{{"v":1,"pathSession":"{session}","role":"controller","candidates":[{{"kind":"local","address":"192.0.2.7","port":{controller_port}}}]}}"#
        );
        let (status, host_offer) = hub.negotiate(1, offer.as_bytes());
        assert_eq!(status, 200, "negotiate failed: {host_offer}");
        let host_offer =
            parse_path_offer_strict(host_offer.to_string().as_bytes()).expect("host offer decodes");
        assert_eq!(host_offer.role, PathRole::Host);
        assert_eq!(host_offer.path_session, session);
        assert!(!host_offer.candidates.is_empty());

        // The host worker probes 192.0.2.7 (a blackhole) — but the
        // controller can still reach the host's real socket, and the
        // host answers on the 4-tuple the probe arrived from, which is
        // exactly how punching handles NAT'd reality.
        let host_port = host_offer.candidates[0].port;
        let key = probe_key(&[7u8; 32], &[1u8; 16], &[2u8; 16], &session);
        let mut controller = PunchSession::new(
            key,
            session_bytes,
            ProbeDirection::ControllerToHost,
            host_offer.candidates.clone(),
            controller_port,
        );
        controller_socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok();
        let target: std::net::SocketAddr = format!("127.0.0.1:{host_port}").parse().unwrap();
        let mut buffer = [0u8; 128];
        let mut established = false;
        for _ in 0..100 {
            let now = now_unix_ms();
            for (_, probe) in controller.tick(now) {
                // Redirect to loopback: same socket, reachable address.
                let _ = controller_socket.send_to(&probe, target);
            }
            if let Ok((length, from)) = controller_socket.recv_from(&mut buffer) {
                if let Some((answer_target, answer)) =
                    controller.handle_datagram(&buffer[..length], from, now)
                {
                    let _ = controller_socket.send_to(&answer, answer_target);
                }
            }
            if let PunchState::Established(_) = controller.state() {
                established = true;
                break;
            }
        }
        assert!(
            established,
            "controller never punched: {:?}",
            controller.state()
        );

        // Result reporting round-trips.
        let result = PathResult {
            path_session: session.clone(),
            outcome: unpeel_core::direct_path::PathOutcome::Established(host_offer.candidates[0]),
        };
        let body = unpeel_core::direct_path::encode_path_result(&result);
        let (status, response) = hub.result(1, &body);
        assert_eq!(status, 200, "{response}");
        let (status, _) = hub.result(2, &body);
        assert_eq!(status, 409, "unknown conn result rejects");
    }

    #[test]
    fn controller_negotiator_punches_the_host_through_the_live_handlers() {
        // The full composition: unpeel_core::direct_path_client (the
        // Controller half) against this hub's handlers (the Host half),
        // candidates gathered from the machine's real interfaces, probes
        // over real UDP. Requires at least one non-loopback interface,
        // which every dev machine has.
        let hub = hub_with_conn(1);
        let mut statuses = Vec::new();
        let mut post = |path: &str, body: &[u8]| -> Result<(u16, Vec<u8>), String> {
            let (status, value) = match path {
                "/mobile/direct-path" => hub.negotiate(1, body),
                "/mobile/direct-path-result" => hub.result(1, body),
                other => return Err(format!("unexpected path {other}")),
            };
            statuses.push((path.to_string(), status));
            Ok((status, value.to_string().into_bytes()))
        };
        let material = unpeel_core::direct_path_client::ProbeMaterial {
            shared_secret: vec![7u8; 32],
            client_salt: vec![1u8; 16],
            host_salt: vec![2u8; 16],
        };
        let options = unpeel_core::direct_path_client::NegotiateOptions {
            use_stun: false,
            punch_timeout: Duration::from_secs(8),
        };
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let path = unpeel_core::direct_path_client::negotiate_and_punch(
            &mut post, &material, &options, &cancelled,
        )
        .expect("controller negotiates and punches the host");
        assert!(path.rtt.is_some(), "established path should carry an RTT");
        assert_eq!(
            statuses,
            vec![
                ("/mobile/direct-path".to_string(), 200),
                ("/mobile/direct-path-result".to_string(), 200),
            ],
            "negotiate then result, both accepted"
        );
    }

    #[test]
    fn downlink_negotiates_a_real_punch_through_sealed_signaling() {
        // The definitive in-process Rust↔Rust proof: the Controller
        // downlink performs the real E2E handshake against the host-side
        // primitives, BOTH sides derive probe material from that same
        // handshake (the host registers its half in a live DirectPathHub,
        // exactly as the relay loop does), the offer and result travel as
        // sealed tunnel frames, and the punch runs over real UDP with
        // real interface candidates.
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use unpeel_core::relay_crypto as proto;

        let e2e = proto::random_bytes(32);
        let e2e_host = e2e.clone();
        let hub = Arc::new(DirectPathHub::default());
        hub.skip_stun.store(true, Ordering::Release);
        let host_hub = Arc::clone(&hub);

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let client = TcpStream::connect(listener.local_addr().unwrap()).expect("connect");
        let (server, _) = listener.accept().expect("accept");

        let host = std::thread::spawn(move || {
            let mut stream = server;
            let mut buffer: Vec<u8> = Vec::new();
            let receive_payload = |stream: &mut TcpStream, buffer: &mut Vec<u8>| -> Vec<u8> {
                loop {
                    if buffer.len() >= 2 {
                        let (length, header) = match buffer[1] & 0x7f {
                            126 if buffer.len() >= 4 => {
                                (u16::from_be_bytes([buffer[2], buffer[3]]) as usize, 4)
                            }
                            126 | 127 => {
                                let mut chunk = [0u8; 4096];
                                let read = stream.read(&mut chunk).expect("read");
                                assert!(read > 0);
                                buffer.extend_from_slice(&chunk[..read]);
                                continue;
                            }
                            small => (small as usize, 2),
                        };
                        let total = header + 4 + length;
                        if buffer.len() >= total {
                            let mask: [u8; 4] = buffer[header..header + 4].try_into().unwrap();
                            let payload: Vec<u8> = buffer[header + 4..total]
                                .iter()
                                .enumerate()
                                .map(|(index, byte)| byte ^ mask[index % 4])
                                .collect();
                            buffer.drain(..total);
                            return payload;
                        }
                    }
                    let mut chunk = [0u8; 4096];
                    let read = stream.read(&mut chunk).expect("read");
                    assert!(read > 0, "downlink closed early");
                    buffer.extend_from_slice(&chunk[..read]);
                }
            };
            let send_payload = |stream: &mut TcpStream, payload: &[u8]| {
                let mut frame = vec![0x82u8];
                if payload.len() < 126 {
                    frame.push(payload.len() as u8);
                } else {
                    frame.push(126);
                    frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                }
                frame.extend_from_slice(payload);
                stream.write_all(&frame).expect("write");
            };

            // Handshake, host side — the serve relay loop's exact shape,
            // including the material registration.
            let hello_payload = receive_payload(&mut stream, &mut buffer);
            let hello = proto::parse_client_hello(&hello_payload).expect("client hello");
            let ephemeral = proto::ephemeral_key().expect("ephemeral");
            let host_public = ephemeral.public.clone();
            let shared = proto::shared_secret(ephemeral, &hello.ephemeral_public).expect("ecdh");
            let host_salt = proto::random_bytes(16);
            let mac = proto::transcript_mac(
                &e2e_host,
                &hello.device_id,
                &hello.salt,
                &host_salt,
                &hello.ephemeral_public,
                &host_public,
            );
            let mut session =
                proto::CryptoSession::new(&e2e_host, &shared, &hello.salt, &host_salt, true)
                    .expect("host session");
            host_hub.register(
                1,
                ConnMaterial {
                    shared_secret: shared.clone(),
                    client_salt: hello.salt.clone(),
                    host_salt: host_salt.clone(),
                },
            );
            send_payload(
                &mut stream,
                &proto::encode_host_hello(&host_salt, &host_public, &mac),
            );

            // Serve the two sealed direct-path operations, relay-dispatch
            // style, then exit after the result lands.
            loop {
                let sealed = receive_payload(&mut stream, &mut buffer);
                let plaintext = session.open(&sealed).expect("open request");
                let request = unpeel_core::relay_wire::parse_tunnel_request_strict(&plaintext)
                    .expect("tunnel request");
                let (status, value) = match request.path.as_str() {
                    "/mobile/direct-path" => host_hub.negotiate(1, &request.body),
                    "/mobile/direct-path-result" => host_hub.result(1, &request.body),
                    other => panic!("unexpected path {other}"),
                };
                let response = session
                    .seal(&unpeel_core::relay_wire::encode_bounded_tunnel_response(
                        request.id,
                        status,
                        value.to_string().as_bytes(),
                    ))
                    .expect("seal response");
                send_payload(&mut stream, &response);
                if request.path == "/mobile/direct-path-result" {
                    return;
                }
            }
        });

        let socket = unpeel_core::relay_uplink::RelaySocket::plain_for_tests(client);
        let mut downlink =
            unpeel_core::relay_downlink::RelayDownlink::handshake(socket, "probe-device", &e2e)
                .expect("downlink handshake");
        let options = unpeel_core::direct_path_client::NegotiateOptions {
            use_stun: false,
            punch_timeout: Duration::from_secs(8),
        };
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let path = downlink
            .negotiate_direct_path(Some("test-bearer"), &options, &cancelled)
            .expect("punched through sealed signaling");
        assert!(path.rtt.is_some());
        host.join().expect("host thread");
    }

    #[test]
    fn a_second_offer_replaces_and_cancels_the_first() {
        let hub = hub_with_conn(1);
        let (_, first) = random_path_session();
        let (_, second) = random_path_session();
        let offer = |session: &str| {
            format!(
                r#"{{"v":1,"pathSession":"{session}","role":"controller","candidates":[{{"kind":"local","address":"192.0.2.7","port":4000}}]}}"#
            )
        };
        let (status, _) = hub.negotiate(1, offer(&first).as_bytes());
        assert_eq!(status, 200);
        let (status, _) = hub.negotiate(1, offer(&second).as_bytes());
        assert_eq!(status, 200);
        // Result for the replaced session no longer matches.
        let stale = PathResult {
            path_session: first,
            outcome: unpeel_core::direct_path::PathOutcome::Failed(
                unpeel_core::direct_path::FailureReason::PunchTimeout,
            ),
        };
        let (status, _) = hub.result(1, &unpeel_core::direct_path::encode_path_result(&stale));
        assert_eq!(status, 409);
    }
}
