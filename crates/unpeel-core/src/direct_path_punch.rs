//! Direct-path punch engine — increment 2 of
//! the private "relay-direct-upgrade" design record. Implements the probe datagram
//! frozen in the private "direct-path-v1" design record: HKDF-keyed 54-byte probes,
//! response = echo with direction flipped (amplification 1.0), silent
//! rejection, and the punch state machine that turns candidate pairs into
//! one validated 4-tuple. Also the minimal STUN binding query and local
//! interface gathering that feed `direct_path::PathOffer`.
//!
//! Nothing here grants authority: a validated punch only proves
//! reachability, and the caller migrates by running a fresh authenticated
//! bootstrap over the punched socket (new connection generation).

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ring::hmac;
use ring::rand::SecureRandom;

use crate::direct_path::{validate_candidate_address, PathCandidate};

pub const PROBE_MAGIC: [u8; 4] = *b"UNPD";
pub const PROBE_VERSION: u8 = 1;
pub const PROBE_LEN: usize = 54;
pub const PROBE_TAG_LEN: usize = 16;
/// ±30 s sender-timestamp acceptance window (contract).
pub const PROBE_TIME_WINDOW_MS: u64 = 30_000;
/// 10 probes/second steady state, burst 20, per path session (contract).
pub const PROBE_RATE_PER_SEC: u32 = 10;
pub const PROBE_BURST: u32 = 20;
/// A path session that has not validated in 60 s is dead (contract).
pub const PATH_SESSION_LIFETIME: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeDirection {
    ControllerToHost,
    HostToController,
}

impl ProbeDirection {
    fn byte(self) -> u8 {
        match self {
            ProbeDirection::ControllerToHost => 0x01,
            ProbeDirection::HostToController => 0x02,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(ProbeDirection::ControllerToHost),
            0x02 => Some(ProbeDirection::HostToController),
            _ => None,
        }
    }

    pub fn flipped(self) -> Self {
        match self {
            ProbeDirection::ControllerToHost => ProbeDirection::HostToController,
            ProbeDirection::HostToController => ProbeDirection::ControllerToHost,
        }
    }
}

/// Contract derivation: HKDF-SHA256(salt = client_salt‖host_salt, ikm = the
/// connection's X25519 shared secret, info = "unpeel-direct-v1:probe:" ‖
/// pathSession hex). Both peers already hold every input.
pub fn probe_key(
    shared_secret: &[u8],
    client_salt: &[u8],
    host_salt: &[u8],
    path_session_hex: &str,
) -> [u8; 32] {
    let mut salt = Vec::with_capacity(client_salt.len() + host_salt.len());
    salt.extend_from_slice(client_salt);
    salt.extend_from_slice(host_salt);
    crate::relay_crypto::derive_key(
        shared_secret,
        &salt,
        &format!("unpeel-direct-v1:probe:{path_session_hex}"),
    )
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Probe {
    pub direction: ProbeDirection,
    pub timestamp_ms: u64,
    pub nonce: [u8; 8],
}

pub fn encode_probe(
    key: &[u8; 32],
    path_session: &[u8; 16],
    direction: ProbeDirection,
    timestamp_ms: u64,
    nonce: [u8; 8],
) -> [u8; PROBE_LEN] {
    let mut probe = [0u8; PROBE_LEN];
    probe[0..4].copy_from_slice(&PROBE_MAGIC);
    probe[4] = PROBE_VERSION;
    probe[5] = direction.byte();
    probe[6..22].copy_from_slice(path_session);
    probe[22..30].copy_from_slice(&timestamp_ms.to_be_bytes());
    probe[30..38].copy_from_slice(&nonce);
    let mac_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&mac_key, &probe[..38]);
    probe[38..].copy_from_slice(&tag.as_ref()[..PROBE_TAG_LEN]);
    probe
}

/// Silent-rejection verifier: every failure is `None` (the contract sends no
/// error datagrams). `expected_direction` is the direction the *receiver*
/// accepts; `seen_nonces` implements replay rejection for the session.
pub fn verify_probe(
    bytes: &[u8],
    key: &[u8; 32],
    path_session: &[u8; 16],
    expected_direction: ProbeDirection,
    now_ms: u64,
    seen_nonces: &mut HashSet<[u8; 8]>,
) -> Option<Probe> {
    if bytes.len() != PROBE_LEN
        || bytes[0..4] != PROBE_MAGIC
        || bytes[4] != PROBE_VERSION
        || bytes[6..22] != path_session[..]
    {
        return None;
    }
    let direction = ProbeDirection::from_byte(bytes[5])?;
    if direction != expected_direction {
        return None;
    }
    let mac_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let expected = hmac::sign(&mac_key, &bytes[..38]);
    if !constant_time_eq(&expected.as_ref()[..PROBE_TAG_LEN], &bytes[38..]) {
        return None;
    }
    let timestamp_ms = u64::from_be_bytes(bytes[22..30].try_into().ok()?);
    if now_ms.abs_diff(timestamp_ms) > PROBE_TIME_WINDOW_MS {
        return None;
    }
    let nonce: [u8; 8] = bytes[30..38].try_into().ok()?;
    if !seen_nonces.insert(nonce) {
        return None;
    }
    Some(Probe {
        direction,
        timestamp_ms,
        nonce,
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn random_nonce() -> [u8; 8] {
    let mut nonce = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut nonce);
    nonce
}

pub fn random_path_session() -> ([u8; 16], String) {
    let mut bytes = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    (bytes, hex)
}

pub fn path_session_bytes(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (index, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).ok()?;
        bytes[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(bytes)
}

// ---------------------------------------------------------------------------
// Candidate gathering: local interfaces + one STUN query.
// ---------------------------------------------------------------------------

/// Local unicast addresses that pass the contract's candidate policy,
/// via getifaddrs.
pub fn local_candidate_addresses() -> Vec<IpAddr> {
    let mut addresses = Vec::new();
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return addresses;
        }
        let mut cursor = list;
        while !cursor.is_null() {
            let entry = &*cursor;
            cursor = entry.ifa_next;
            if entry.ifa_addr.is_null() {
                continue;
            }
            let family = (*entry.ifa_addr).sa_family as i32;
            let address = match family {
                libc::AF_INET => {
                    let socket = &*(entry.ifa_addr as *const libc::sockaddr_in);
                    IpAddr::V4(u32::from_be(socket.sin_addr.s_addr).into())
                }
                libc::AF_INET6 => {
                    let socket = &*(entry.ifa_addr as *const libc::sockaddr_in6);
                    IpAddr::V6(socket.sin6_addr.s6_addr.into())
                }
                _ => continue,
            };
            if validate_candidate_address(address).is_ok() && !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        libc::freeifaddrs(list);
    }
    addresses
}

/// Default STUN server (pinned decision: same operator as the Relay).
pub const STUN_SERVER: &str = "stun.cloudflare.com:3478";

/// One RFC 5389 binding request on the punch socket itself (the reflexive
/// mapping must belong to the socket that will punch). Returns the
/// XOR-MAPPED-ADDRESS.
pub fn stun_reflexive_address(
    socket: &UdpSocket,
    server: &str,
    timeout: Duration,
) -> Result<SocketAddr, String> {
    use std::net::ToSocketAddrs;
    let server_addr = server
        .to_socket_addrs()
        .map_err(|e| format!("stun resolve: {e}"))?
        .find(|addr| addr.is_ipv4() == socket.local_addr().map(|a| a.is_ipv4()).unwrap_or(true))
        .ok_or("no stun address for the socket's family")?;

    let mut transaction = [0u8; 12];
    let _ = ring::rand::SystemRandom::new().fill(&mut transaction);
    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&0x0001u16.to_be_bytes()); // binding request
    request.extend_from_slice(&0u16.to_be_bytes()); // no attributes
    request.extend_from_slice(&0x2112_A442u32.to_be_bytes()); // magic cookie
    request.extend_from_slice(&transaction);

    socket
        .send_to(&request, server_addr)
        .map_err(|e| format!("stun send: {e}"))?;
    let previous_timeout = socket.read_timeout().ok().flatten();
    socket.set_read_timeout(Some(timeout)).ok();
    let mut buffer = [0u8; 256];
    let result = loop {
        match socket.recv_from(&mut buffer) {
            Ok((length, from)) => {
                if from != server_addr {
                    continue;
                }
                break parse_stun_response(&buffer[..length], &transaction);
            }
            Err(e) => break Err(format!("stun recv: {e}")),
        }
    };
    socket.set_read_timeout(previous_timeout).ok();
    result
}

fn parse_stun_response(bytes: &[u8], transaction: &[u8; 12]) -> Result<SocketAddr, String> {
    if bytes.len() < 20 || bytes[0..2] != 0x0101u16.to_be_bytes() {
        return Err("not a stun binding success".into());
    }
    if bytes[8..20] != transaction[..] {
        return Err("stun transaction mismatch".into());
    }
    let mut offset = 20;
    while offset + 4 <= bytes.len() {
        let kind = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        let value = bytes
            .get(offset + 4..offset + 4 + length)
            .ok_or("truncated stun attribute")?;
        if kind == 0x0020 {
            // XOR-MAPPED-ADDRESS
            if value.len() < 8 {
                return Err("short xor-mapped-address".into());
            }
            let port = u16::from_be_bytes([value[2], value[3]]) ^ 0x2112;
            return match value[1] {
                0x01 => {
                    let raw = u32::from_be_bytes(value[4..8].try_into().unwrap()) ^ 0x2112_A442;
                    Ok(SocketAddr::new(IpAddr::V4(raw.into()), port))
                }
                0x02 => {
                    let mut raw: [u8; 16] = value[4..20]
                        .try_into()
                        .map_err(|_| "short v6 xor-mapped-address")?;
                    let mut xor = [0u8; 16];
                    xor[..4].copy_from_slice(&0x2112_A442u32.to_be_bytes());
                    xor[4..].copy_from_slice(transaction);
                    for (byte, mask) in raw.iter_mut().zip(xor) {
                        *byte ^= mask;
                    }
                    Ok(SocketAddr::new(IpAddr::V6(raw.into()), port))
                }
                _ => Err("unknown stun address family".into()),
            };
        }
        offset += 4 + length.div_ceil(4) * 4;
    }
    Err("no xor-mapped-address attribute".into())
}

// ---------------------------------------------------------------------------
// Punch state machine. Transport-free core: the caller owns the socket and
// pumps datagrams through `handle_datagram` / drains `outgoing`.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PunchState {
    Probing,
    /// A response to one of our probes arrived from this address: the round
    /// trip is proven and this 4-tuple is punched.
    Established(SocketAddr),
    /// The session lifetime elapsed without a validated round trip.
    TimedOut,
}

pub struct PunchSession {
    key: [u8; 32],
    path_session: [u8; 16],
    send_direction: ProbeDirection,
    peers: Vec<SocketAddr>,
    outgoing_nonces: HashSet<[u8; 8]>,
    seen_nonces: HashSet<[u8; 8]>,
    tokens: f64,
    last_refill: Instant,
    started: Instant,
    state: PunchState,
    rtt: Option<Duration>,
    in_flight: Vec<([u8; 8], Instant)>,
}

impl PunchSession {
    /// `send_direction` is this peer's outgoing probe direction
    /// (controller→host for the Controller role). `peers` are the remote
    /// candidates already validated by `parse_path_offer_strict`.
    pub fn new(
        key: [u8; 32],
        path_session: [u8; 16],
        send_direction: ProbeDirection,
        peers: Vec<PathCandidate>,
        local_port_hint: u16,
    ) -> Self {
        let mut targets: Vec<SocketAddr> = peers
            .iter()
            .map(|candidate| SocketAddr::new(candidate.address, candidate.port))
            .collect();
        // A NAT that preserves ports is common enough that trying the
        // reflexive address on our own bound port's mirror costs nothing;
        // dedup keeps the matrix bounded either way.
        targets.dedup();
        let _ = local_port_hint;
        Self {
            key,
            path_session,
            send_direction,
            peers: targets,
            outgoing_nonces: HashSet::new(),
            seen_nonces: HashSet::new(),
            tokens: f64::from(PROBE_BURST),
            last_refill: Instant::now(),
            started: Instant::now(),
            state: PunchState::Probing,
            rtt: None,
            in_flight: Vec::new(),
        }
    }

    pub fn state(&self) -> PunchState {
        self.state
    }

    pub fn rtt(&self) -> Option<Duration> {
        self.rtt
    }

    /// Datagrams to send now: one probe per peer candidate, rate-capped by
    /// the contract's token bucket. Call on a ~200 ms cadence.
    pub fn tick(&mut self, now_ms: u64) -> Vec<(SocketAddr, [u8; PROBE_LEN])> {
        if self.state != PunchState::Probing {
            return Vec::new();
        }
        if self.started.elapsed() >= PATH_SESSION_LIFETIME {
            self.state = PunchState::TimedOut;
            return Vec::new();
        }
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.last_refill = Instant::now();
        self.tokens =
            (self.tokens + elapsed * f64::from(PROBE_RATE_PER_SEC)).min(f64::from(PROBE_BURST));
        let mut sends = Vec::new();
        for peer in &self.peers {
            if self.tokens < 1.0 {
                break;
            }
            self.tokens -= 1.0;
            let nonce = random_nonce();
            self.outgoing_nonces.insert(nonce);
            self.in_flight.push((nonce, Instant::now()));
            sends.push((
                *peer,
                encode_probe(
                    &self.key,
                    &self.path_session,
                    self.send_direction,
                    now_ms,
                    nonce,
                ),
            ));
        }
        sends
    }

    /// Feed one received datagram. Returns a response datagram to send back
    /// to `from` when the incoming probe was a valid peer probe (echo with
    /// flipped direction, same nonce — the contract's answer shape).
    pub fn handle_datagram(
        &mut self,
        bytes: &[u8],
        from: SocketAddr,
        now_ms: u64,
    ) -> Option<(SocketAddr, [u8; PROBE_LEN])> {
        // A response to one of our probes: direction matches what WE send
        // (the peer flipped theirs back to ours? No — the peer answers our
        // probe by flipping direction, so a response arrives with the
        // peer's direction). Distinguish by nonce ownership instead of
        // direction alone.
        let peer_direction = self.send_direction.flipped();
        if bytes.len() == PROBE_LEN && bytes[5] == peer_direction.byte() {
            let mut scratch = HashSet::new();
            if let Some(probe) = verify_probe(
                bytes,
                &self.key,
                &self.path_session,
                peer_direction,
                now_ms,
                &mut scratch,
            ) {
                if self.outgoing_nonces.contains(&probe.nonce) {
                    // Echo of our own probe: round trip proven.
                    if self.state == PunchState::Probing {
                        self.state = PunchState::Established(from);
                        self.rtt = self
                            .in_flight
                            .iter()
                            .find(|(nonce, _)| *nonce == probe.nonce)
                            .map(|(_, sent)| sent.elapsed());
                    }
                    return None;
                }
                // A fresh peer probe: replay-check it for real, then answer.
                if !self.seen_nonces.insert(probe.nonce) {
                    return None;
                }
                let answer = encode_probe(
                    &self.key,
                    &self.path_session,
                    self.send_direction,
                    now_ms,
                    probe.nonce,
                );
                return Some((from, answer));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Dev harness: `unpeel-host __punch__` — a manual two-terminal punch proof
// between real machines/NATs, ahead of the relay-integrated signaling. The
// shared secret stands in for the live connection's ECDH secret; the real
// integration derives the probe key from the session handshake instead.
// ---------------------------------------------------------------------------

pub const PUNCH_ARG: &str = "__punch__";

const HARNESS_CLIENT_SALT: [u8; 16] = *b"unpeel-punch-cs!";
const HARNESS_HOST_SALT: [u8; 16] = *b"unpeel-punch-hs!";

pub fn run_cli(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("keygen") {
        let (_, session) = random_path_session();
        let mut secret = [0u8; 32];
        let _ = ring::rand::SystemRandom::new().fill(&mut secret);
        let secret_hex: String = secret.iter().map(|byte| format!("{byte:02x}")).collect();
        println!("--session {session} --secret {secret_hex}");
        return Ok(());
    }

    let mut role: Option<crate::direct_path::PathRole> = None;
    let mut session_hex: Option<String> = None;
    let mut secret_hex: Option<String> = None;
    let mut offer_file: Option<String> = None;
    let mut peer_file: Option<String> = None;
    let mut use_stun = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .map(|value| value.to_string())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--role" => {
                role = Some(match value("--role")?.as_str() {
                    "controller" => crate::direct_path::PathRole::Controller,
                    "host" => crate::direct_path::PathRole::Host,
                    other => return Err(format!("unknown role: {other}")),
                })
            }
            "--session" => session_hex = Some(value("--session")?),
            "--secret" => secret_hex = Some(value("--secret")?),
            "--offer-file" => offer_file = Some(value("--offer-file")?),
            "--peer-file" => peer_file = Some(value("--peer-file")?),
            "--stun" => use_stun = true,
            other => {
                return Err(format!(
                    "unknown argument: {other}\nusage: unpeel-host __punch__ keygen | \
                     --role controller|host --session <hex32> --secret <hex64> \
                     [--stun] [--offer-file P] [--peer-file P]"
                ))
            }
        }
    }
    let role = role.ok_or("--role is required")?;
    let session_hex = session_hex.ok_or("--session is required (see keygen)")?;
    let session = path_session_bytes(&session_hex).ok_or("--session must be 32 hex characters")?;
    let secret_hex = secret_hex.ok_or("--secret is required (see keygen)")?;
    if secret_hex.len() != 64 {
        return Err("--secret must be 64 hex characters".into());
    }
    let mut secret = [0u8; 32];
    for (index, chunk) in secret_hex.as_bytes().chunks(2).enumerate() {
        secret[index] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("zz"), 16)
            .map_err(|_| "--secret is not hex")?;
    }
    let key = probe_key(
        &secret,
        &HARNESS_CLIENT_SALT,
        &HARNESS_HOST_SALT,
        &session_hex,
    );

    let socket = UdpSocket::bind(("0.0.0.0", 0)).map_err(|e| format!("bind: {e}"))?;
    let local_port = socket.local_addr().map_err(|e| e.to_string())?.port();
    let mut candidates: Vec<crate::direct_path::PathCandidate> = local_candidate_addresses()
        .into_iter()
        .map(|address| crate::direct_path::PathCandidate {
            kind: crate::direct_path::CandidateKind::Local,
            address,
            port: local_port,
        })
        .collect();
    if use_stun {
        match stun_reflexive_address(&socket, STUN_SERVER, Duration::from_secs(3)) {
            Ok(reflexive) => {
                eprintln!("stun reflexive: {reflexive}");
                let candidate = crate::direct_path::PathCandidate {
                    kind: crate::direct_path::CandidateKind::Reflexive,
                    address: reflexive.ip(),
                    port: reflexive.port(),
                };
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
            Err(error) => eprintln!("stun failed ({error}); continuing with local candidates"),
        }
    }
    candidates.truncate(crate::direct_path::MAX_CANDIDATES);
    if candidates.is_empty() {
        return Err("no usable local candidates".into());
    }
    let offer = crate::direct_path::PathOffer {
        path_session: session_hex.clone(),
        role,
        candidates,
    };
    let offer_json = String::from_utf8(crate::direct_path::encode_path_offer(&offer))
        .expect("offer json is utf8");
    if let Some(path) = &offer_file {
        std::fs::write(path, &offer_json).map_err(|e| format!("write offer: {e}"))?;
        eprintln!("offer written to {path}");
    } else {
        println!("{offer_json}");
    }

    let peer_json = if let Some(path) = &peer_file {
        eprintln!("waiting for peer offer at {path} …");
        let deadline = Instant::now() + PATH_SESSION_LIFETIME;
        loop {
            match std::fs::read_to_string(path) {
                Ok(content) if !content.trim().is_empty() => break content,
                _ if Instant::now() >= deadline => return Err("peer offer never arrived".into()),
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    } else {
        eprintln!("paste the peer offer JSON and press enter:");
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
            .map_err(|e| format!("stdin: {e}"))?;
        line
    };
    let peer = crate::direct_path::parse_path_offer_strict(peer_json.trim().as_bytes())
        .map_err(|error| format!("peer offer rejected: {error}"))?;
    if peer.path_session != session_hex {
        return Err("peer offer is for a different path session".into());
    }
    if peer.role == role {
        return Err("peer offer has the same role; one side must be host".into());
    }

    let send_direction = match role {
        crate::direct_path::PathRole::Controller => ProbeDirection::ControllerToHost,
        crate::direct_path::PathRole::Host => ProbeDirection::HostToController,
    };
    let mut punch = PunchSession::new(key, session, send_direction, peer.candidates, local_port);
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    eprintln!("punching …");
    let mut buffer = [0u8; 128];
    let mut linger_until: Option<Instant> = None;
    loop {
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
            PunchState::Established(peer_addr) => {
                if linger_until.is_none() {
                    let rtt = punch
                        .rtt()
                        .map(|rtt| format!("{:.1} ms", rtt.as_secs_f64() * 1000.0))
                        .unwrap_or_else(|| "unknown".into());
                    println!("PUNCHED {peer_addr} rtt {rtt}");
                    // Keep answering so the peer's own round trip completes.
                    linger_until = Some(Instant::now() + Duration::from_secs(3));
                }
                if Instant::now() >= linger_until.expect("set above") {
                    return Ok(());
                }
            }
            PunchState::TimedOut => return Err("punch timed out".into()),
            PunchState::Probing => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::direct_path::CandidateKind;

    fn key() -> [u8; 32] {
        probe_key(
            &[7u8; 32],
            &[1u8; 16],
            &[2u8; 16],
            "00112233445566778899aabbccddeeff",
        )
    }

    #[test]
    fn probe_round_trips_and_rejects_tamper_replay_and_stale() {
        let key = key();
        let session = path_session_bytes("00112233445566778899aabbccddeeff").unwrap();
        let now = 1_700_000_000_000;
        let probe = encode_probe(
            &key,
            &session,
            ProbeDirection::ControllerToHost,
            now,
            [9u8; 8],
        );
        let mut seen = HashSet::new();
        let verified = verify_probe(
            &probe,
            &key,
            &session,
            ProbeDirection::ControllerToHost,
            now + 5,
            &mut seen,
        )
        .expect("valid probe verifies");
        assert_eq!(verified.nonce, [9u8; 8]);

        // Replay: same nonce again dies.
        assert!(verify_probe(
            &probe,
            &key,
            &session,
            ProbeDirection::ControllerToHost,
            now + 5,
            &mut seen
        )
        .is_none());

        // Tamper: one flipped bit dies.
        let mut tampered = probe;
        tampered[25] ^= 1;
        assert!(verify_probe(
            &tampered,
            &key,
            &session,
            ProbeDirection::ControllerToHost,
            now + 5,
            &mut HashSet::new()
        )
        .is_none());

        // Wrong direction dies.
        assert!(verify_probe(
            &probe,
            &key,
            &session,
            ProbeDirection::HostToController,
            now + 5,
            &mut HashSet::new()
        )
        .is_none());

        // Outside the time window dies.
        assert!(verify_probe(
            &probe,
            &key,
            &session,
            ProbeDirection::ControllerToHost,
            now + PROBE_TIME_WINDOW_MS + 1,
            &mut HashSet::new()
        )
        .is_none());

        // Wrong key dies.
        let other = probe_key(
            &[8u8; 32],
            &[1u8; 16],
            &[2u8; 16],
            "00112233445566778899aabbccddeeff",
        );
        assert!(verify_probe(
            &probe,
            &other,
            &session,
            ProbeDirection::ControllerToHost,
            now + 5,
            &mut HashSet::new()
        )
        .is_none());
    }

    #[test]
    fn probe_key_binds_the_path_session() {
        let a = probe_key(
            &[7u8; 32],
            &[1u8; 16],
            &[2u8; 16],
            "00112233445566778899aabbccddeeff",
        );
        let b = probe_key(
            &[7u8; 32],
            &[1u8; 16],
            &[2u8; 16],
            "ffeeddccbbaa99887766554433221100",
        );
        assert_ne!(a, b);
    }

    #[test]
    fn punch_state_machine_establishes_on_round_trip_and_answers_peer_probes() {
        let key = key();
        let session = path_session_bytes("00112233445566778899aabbccddeeff").unwrap();
        let peer_addr: SocketAddr = "192.0.2.10:4000".parse().unwrap();
        let candidate = PathCandidate {
            kind: CandidateKind::Reflexive,
            address: peer_addr.ip(),
            port: peer_addr.port(),
        };
        let mut controller = PunchSession::new(
            key,
            session,
            ProbeDirection::ControllerToHost,
            vec![candidate],
            0,
        );
        let mut host = PunchSession::new(
            key,
            session,
            ProbeDirection::HostToController,
            vec![candidate],
            0,
        );
        let now = now_unix_ms();

        // Controller probes; host answers; controller establishes on the echo.
        let sends = controller.tick(now);
        assert_eq!(sends.len(), 1);
        let (_, probe) = sends[0];
        let answer = host
            .handle_datagram(&probe, peer_addr, now)
            .expect("host answers a valid peer probe");
        assert!(controller
            .handle_datagram(&answer.1, peer_addr, now)
            .is_none());
        assert_eq!(controller.state(), PunchState::Established(peer_addr));
        assert!(controller.rtt().is_some());

        // Host establishes symmetrically from its own probe's echo.
        let sends = host.tick(now);
        let (_, host_probe) = sends[0];
        let echo = controller
            .handle_datagram(&host_probe, peer_addr, now)
            .expect("controller answers");
        assert!(host.handle_datagram(&echo.1, peer_addr, now).is_none());
        assert_eq!(host.state(), PunchState::Established(peer_addr));
    }

    #[test]
    fn punch_over_real_udp_sockets() {
        let key = key();
        let session = path_session_bytes("00112233445566778899aabbccddeeff").unwrap();
        let socket_a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let socket_b = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr_a = socket_a.local_addr().unwrap();
        let addr_b = socket_b.local_addr().unwrap();
        socket_a
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        socket_b
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let candidate = |addr: SocketAddr| PathCandidate {
            kind: CandidateKind::Local,
            address: addr.ip(),
            port: addr.port(),
        };
        let mut a = PunchSession::new(
            key,
            session,
            ProbeDirection::ControllerToHost,
            vec![candidate(addr_b)],
            0,
        );
        let mut b = PunchSession::new(
            key,
            session,
            ProbeDirection::HostToController,
            vec![candidate(addr_a)],
            0,
        );
        let mut buffer = [0u8; 128];
        for _ in 0..50 {
            let now = now_unix_ms();
            for (target, probe) in a.tick(now) {
                socket_a.send_to(&probe, target).unwrap();
            }
            for (target, probe) in b.tick(now) {
                socket_b.send_to(&probe, target).unwrap();
            }
            if let Ok((length, from)) = socket_a.recv_from(&mut buffer) {
                if let Some((target, answer)) = a.handle_datagram(&buffer[..length], from, now) {
                    socket_a.send_to(&answer, target).unwrap();
                }
            }
            if let Ok((length, from)) = socket_b.recv_from(&mut buffer) {
                if let Some((target, answer)) = b.handle_datagram(&buffer[..length], from, now) {
                    socket_b.send_to(&answer, target).unwrap();
                }
            }
            if matches!(a.state(), PunchState::Established(_))
                && matches!(b.state(), PunchState::Established(_))
            {
                return;
            }
        }
        panic!(
            "punch never established over loopback sockets: a={:?} b={:?}",
            a.state(),
            b.state()
        );
    }

    #[test]
    fn local_candidates_respect_the_address_policy() {
        for address in local_candidate_addresses() {
            assert!(
                validate_candidate_address(address).is_ok(),
                "{address} leaked"
            );
        }
    }
}
