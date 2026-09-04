//! Relay latency probe (`unpeel-host __relay_probe__`) — increment 0 of
//! `unpeel-apple:docs/plans/relay-direct-upgrade.md`. Decomposes off-LAN latency into
//! peer→edge and edge→DO legs and benches seal/unseal locally, so the
//! direct-path upgrade has a measured baseline to beat.
//!
//! Passive mode (default) is side-effect free: it never reaches the Durable
//! Object and never authenticates. `--full` deliberately occupies the Host
//! uplink slot for a few seconds — the relay closes the previous uplink when
//! a new host connects — so a live app/serve uplink is displaced and will
//! auto-reconnect after the probe exits. That trade is the only way to
//! measure the DO forward path: the DO is a pure router and echoes nothing
//! to a lone peer, so the probe must be both ends of a real host↔client pair.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;

pub const RELAY_PROBE_ARG: &str = "__relay_probe__";

const READ_TIMEOUT: Duration = Duration::from_secs(10);

pub fn run_cli(args: &[String]) -> Result<(), String> {
    let mut url = crate::relay_uplink::relay_url();
    let mut samples: usize = 5;
    let mut frames: usize = 20;
    let mut full = false;
    let mut entitlement_override: Option<String> = None;
    let mut mac_override: Option<String> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--url" => url = required_value(&mut iter, "--url")?,
            "--samples" => samples = parse_count(&required_value(&mut iter, "--samples")?)?,
            "--frames" => frames = parse_count(&required_value(&mut iter, "--frames")?)?,
            "--full" => full = true,
            "--entitlement" => {
                entitlement_override = Some(required_value(&mut iter, "--entitlement")?)
            }
            "--mac" => mac_override = Some(required_value(&mut iter, "--mac")?),
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}\n{}", usage())),
        }
    }

    let endpoint = Endpoint::parse(&url)?;
    println!("relay probe → {url}");
    println!();

    passive_pass(&endpoint, samples)?;
    seal_bench();

    if !full {
        println!(
            "\npassive pass complete. Run with --full to measure the Durable Object\n\
             forward path (connects an entitled host + client pair; a live Host\n\
             uplink on this identity is displaced for a few seconds and then\n\
             reconnects on its own)."
        );
        return Ok(());
    }

    let (entitlement, mac_id) = match (entitlement_override, mac_override) {
        (Some(entitlement), Some(mac)) => (entitlement, mac),
        (None, None) => match crate::license::allowed_cached_relay_entitlement()? {
            Some(pair) => pair,
            None => {
                return Err(
                    "no cached relay entitlement (is Link enabled on this Host?); \
                     for a dev relay pass --entitlement and --mac explicitly"
                        .into(),
                )
            }
        },
        _ => return Err("--entitlement and --mac must be given together".into()),
    };

    println!("\nfull pass: measuring the DO forward path as mac '{mac_id}'");
    println!("(the live uplink for this identity is displaced until the probe exits)");
    full_pass(&endpoint, &mac_id, &entitlement, frames)
}

fn usage() -> String {
    "usage: unpeel-host __relay_probe__ [options]\n\
     \n\
     --url <ws(s)://host[:port]>  relay to probe (default: UNPEEL_RELAY_URL or production)\n\
     --samples <n>                passive samples per measurement (default 5)\n\
     --frames <n>                 echo frames per size in --full (default 20)\n\
     --full                       also measure the DO forward path (displaces the\n\
                                  live Host uplink for a few seconds)\n\
     --entitlement <t> --mac <id> explicit credentials (dev relay / wrangler dev)\n"
        .into()
}

fn required_value(iter: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String, String> {
    iter.next()
        .map(|value| value.to_string())
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_count(value: &str) -> Result<usize, String> {
    let parsed: usize = value.parse().map_err(|_| format!("not a count: {value}"))?;
    if parsed == 0 || parsed > 1000 {
        return Err(format!("count out of range (1..=1000): {value}"));
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// Passive pass: edge decomposition without auth or DO contact.
// ---------------------------------------------------------------------------

fn passive_pass(endpoint: &Endpoint, samples: usize) -> Result<(), String> {
    let mut dns = Vec::new();
    let mut tcp = Vec::new();
    let mut tls = Vec::new();
    let mut health_ttfb = Vec::new();
    let mut reject = Vec::new();
    let mut edge_colo: Option<String> = None;

    for _ in 0..samples {
        let started = Instant::now();
        let addr = endpoint.resolve()?;
        dns.push(started.elapsed());

        // Health request: TCP → TLS → GET /v1/health, TTFB from request
        // write to first response byte. The health handler does no I/O, so
        // TTFB minus the TCP RTT is edge compute plus scheduling.
        let started = Instant::now();
        let stream = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
        tcp.push(started.elapsed());
        // Without nodelay, Nagle + delayed ACK stalls small writes ~40ms+
        // and poisons every timing below (measured before this was set).
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
        stream.set_write_timeout(Some(READ_TIMEOUT)).ok();

        let started = Instant::now();
        let mut transport = endpoint.wrap_tls(stream)?;
        if endpoint.secure {
            tls.push(started.elapsed());
        }

        let request = format!(
            "GET /v1/health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            endpoint.host
        );
        let started = Instant::now();
        transport
            .write_all(request.as_bytes())
            .map_err(|e| format!("health write: {e}"))?;
        let head = read_http_head(&mut transport, &mut Vec::new())?;
        health_ttfb.push(started.elapsed());
        if edge_colo.is_none() {
            edge_colo = cf_ray_colo(&head);
        }

        // Upgrade-reject timing: a syntactically valid host upgrade with a
        // bearer that cannot verify. The worker answers 403 from the edge
        // without addressing the DO, so this is the authenticated-route
        // worker cost with zero side effects on any real identity.
        let stream = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
        stream.set_write_timeout(Some(READ_TIMEOUT)).ok();
        let mut transport = endpoint.wrap_tls(stream)?;
        let key =
            base64::engine::general_purpose::STANDARD.encode(crate::relay_crypto::random_bytes(16));
        let request = format!(
            "GET /v1/host/probe0timing0reject HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\
             Authorization: Bearer UNPRE-invalid.invalid\r\n\r\n",
            endpoint.host
        );
        let started = Instant::now();
        transport
            .write_all(request.as_bytes())
            .map_err(|e| format!("reject write: {e}"))?;
        let head = read_http_head(&mut transport, &mut Vec::new())?;
        reject.push(started.elapsed());
        let status = head.lines().next().unwrap_or("").to_string();
        if !status.contains("403") && !status.contains("401") {
            println!("note: reject probe got unexpected status: {status}");
        }
    }

    println!("passive ({samples} samples):");
    if let Some(colo) = &edge_colo {
        println!("  edge colo (cf-ray)        {colo}");
    }
    print_stat("dns resolve", &dns);
    print_stat("tcp connect (≈RTT)", &tcp);
    if endpoint.secure {
        print_stat("tls handshake", &tls);
    }
    print_stat("health TTFB", &health_ttfb);
    print_stat("host-route 403 (edge)", &reject);
    Ok(())
}

// ---------------------------------------------------------------------------
// Local crypto bench: seal/unseal are on the per-frame path for every
// transport, so their cost bounds how much of the latency is ours vs. the
// network's.
// ---------------------------------------------------------------------------

fn seal_bench() {
    println!("\nseal/unseal (local, AES-256-GCM per relay_crypto):");
    for (label, size, iterations) in [
        ("64 B", 64usize, 2000usize),
        ("8 KiB", 8 * 1024, 2000),
        ("512 KiB", crate::relay_wire::MAX_PLAINTEXT_BYTES, 200),
    ] {
        let Some((mut sealer, mut opener)) = bench_sessions() else {
            println!("  {label:<10} unavailable (crypto init failed)");
            continue;
        };
        let plaintext = vec![0xa5u8; size];
        let mut sealed_frames = Vec::with_capacity(iterations);
        let started = Instant::now();
        for _ in 0..iterations {
            match sealer.seal(&plaintext) {
                Ok(frame) => sealed_frames.push(frame),
                Err(_) => break,
            }
        }
        let seal_elapsed = started.elapsed();
        let started = Instant::now();
        for frame in &sealed_frames {
            if opener.open(frame).is_err() {
                break;
            }
        }
        let open_elapsed = started.elapsed();
        let per_seal = seal_elapsed.as_secs_f64() / iterations as f64;
        let per_open = open_elapsed.as_secs_f64() / iterations as f64;
        let throughput = size as f64 / per_seal / (1024.0 * 1024.0);
        println!(
            "  {label:<10} seal {:>8.1} µs   open {:>8.1} µs   ({throughput:>7.0} MiB/s seal)",
            per_seal * 1e6,
            per_open * 1e6,
        );
    }
}

fn bench_sessions() -> Option<(
    crate::relay_crypto::CryptoSession,
    crate::relay_crypto::CryptoSession,
)> {
    let e2e = crate::relay_crypto::random_bytes(32);
    let client = crate::relay_crypto::ephemeral_key().ok()?;
    let host = crate::relay_crypto::ephemeral_key().ok()?;
    let client_public = client.public.clone();
    let host_public = host.public.clone();
    let client_secret = crate::relay_crypto::shared_secret(client, &host_public).ok()?;
    let host_secret = crate::relay_crypto::shared_secret(host, &client_public).ok()?;
    let client_salt = crate::relay_crypto::random_bytes(16);
    let host_salt = crate::relay_crypto::random_bytes(16);
    let sealer =
        crate::relay_crypto::CryptoSession::new(&e2e, &host_secret, &client_salt, &host_salt, true)
            .ok()?;
    let opener = crate::relay_crypto::CryptoSession::new(
        &e2e,
        &client_secret,
        &client_salt,
        &host_salt,
        false,
    )
    .ok()?;
    Some((sealer, opener))
}

// ---------------------------------------------------------------------------
// Full pass: be both ends of a host↔client pair and time frames through the
// DO forward path. The DO leg falls out as loop RTT minus the passive edge
// numbers.
// ---------------------------------------------------------------------------

fn full_pass(
    endpoint: &Endpoint,
    mac_id: &str,
    entitlement: &str,
    frames: usize,
) -> Result<(), String> {
    // Host side: the upgrade-to-101 includes the worker's entitlement
    // verification plus the edge→DO subrequest and DO wake, so its delta vs.
    // the passive 403 number is the DO admission cost.
    let started = Instant::now();
    let mut host = WsProbeSocket::connect_host(endpoint, mac_id, entitlement)?;
    let host_upgrade = started.elapsed();

    // Register a throwaway probe device so a client leg can attach. The
    // uplink slot was already reset by our connect; the displaced real
    // uplink re-announces its devices when it reconnects after we exit.
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(crate::relay_crypto::random_bytes(32));
    let hello = format!(
        "{{\"v\":1,\"devices\":[{{\"deviceID\":\"relay-probe\",\"tokenHash\":\"{}\"}}]}}",
        sha256_hex(token.as_bytes())
    );
    let mut hello_frame = vec![0x01u8];
    hello_frame.extend_from_slice(hello.as_bytes());
    host.send_binary(&hello_frame)?;

    // The hello is fire-and-forget, so give the DO a moment to persist the
    // device token before the client presents it.
    std::thread::sleep(Duration::from_millis(300));

    let started = Instant::now();
    let mut client = WsProbeSocket::connect_client(endpoint, mac_id, &token)?;
    let client_upgrade = started.elapsed();

    // The host learns the client's connID from the client's first frame.
    client.send_binary(b"probe-hello")?;
    let conn_id = loop {
        match host.receive_event(READ_TIMEOUT)? {
            WsEvent::Message(frame) if frame.first() == Some(&0x04) && frame.len() >= 6 => {
                break u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);
            }
            WsEvent::Message(_) | WsEvent::Pong => continue,
            WsEvent::Idle => return Err("host never saw the probe client's frame".into()),
        }
    };

    println!(
        "  host upgrade → 101        {}",
        format_duration(host_upgrade)
    );
    println!(
        "  client upgrade → 101      {}",
        format_duration(client_upgrade)
    );

    // WS ping RTT on the host socket: if this comes back well under the
    // frame loop RTT, protocol pings are answered nearer than the DO and
    // prove less about liveness than the uplink code assumes.
    let mut ping_rtt = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        host.send_ping()?;
        loop {
            match host.receive_event(READ_TIMEOUT)? {
                WsEvent::Pong => {
                    ping_rtt.push(started.elapsed());
                    break;
                }
                WsEvent::Message(_) => continue,
                WsEvent::Idle => return Err("ping never answered".into()),
            }
        }
    }
    print_stat("ws ping RTT (host sock)", &ping_rtt);

    for (label, size) in [("64 B", 64usize), ("8 KiB", 8 * 1024)] {
        let payload = vec![0x5au8; size];

        let mut client_to_host = Vec::new();
        for _ in 0..frames {
            let started = Instant::now();
            client.send_binary(&payload)?;
            loop {
                match host.receive_event(READ_TIMEOUT)? {
                    WsEvent::Message(frame) if frame.first() == Some(&0x04) => {
                        client_to_host.push(started.elapsed());
                        break;
                    }
                    WsEvent::Message(_) | WsEvent::Pong => continue,
                    WsEvent::Idle => return Err("client→host frame lost".into()),
                }
            }
        }
        print_stat(&format!("client→DO→host {label}"), &client_to_host);

        let mut host_to_client = Vec::new();
        let mut data_frame = vec![0x02u8];
        data_frame.extend_from_slice(&conn_id.to_be_bytes());
        data_frame.extend_from_slice(&payload);
        for _ in 0..frames {
            let started = Instant::now();
            host.send_binary(&data_frame)?;
            loop {
                match client.receive_event(READ_TIMEOUT)? {
                    WsEvent::Message(_) => {
                        host_to_client.push(started.elapsed());
                        break;
                    }
                    WsEvent::Pong => continue,
                    WsEvent::Idle => return Err("host→client frame lost".into()),
                }
            }
        }
        print_stat(&format!("host→DO→client {label}"), &host_to_client);
    }

    println!(
        "\ninterpretation: loop times traverse probe→edge→DO→edge→probe; subtract\n\
         the passive tcp-connect RTT to estimate the edge↔DO leg. The displaced\n\
         Host uplink reconnects on its own."
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Minimal WebSocket client used only by this probe. Deliberately separate
// from the shipped uplink so measurement stages stay visible and the
// production connect path stays untouched.
// ---------------------------------------------------------------------------

struct Endpoint {
    secure: bool,
    host: String,
    port: u16,
}

impl Endpoint {
    fn parse(url: &str) -> Result<Self, String> {
        let (secure, rest) = if let Some(rest) = url.strip_prefix("wss://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("ws://") {
            (false, rest)
        } else {
            return Err(format!("unsupported relay url: {url}"));
        };
        let host_port = rest.trim_end_matches('/');
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => (
                host.to_string(),
                port.parse::<u16>().map_err(|e| e.to_string())?,
            ),
            None => (host_port.to_string(), if secure { 443 } else { 80 }),
        };
        Ok(Self { secure, host, port })
    }

    fn resolve(&self) -> Result<std::net::SocketAddr, String> {
        use std::net::ToSocketAddrs;
        (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve: {e}"))?
            .next()
            .ok_or_else(|| format!("no address for {}", self.host))
    }

    fn wrap_tls(&self, tcp: TcpStream) -> Result<ProbeTransport, String> {
        if !self.secure {
            return Ok(ProbeTransport::Plain(tcp));
        }
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|e| format!("server name: {e}"))?;
        let mut connection = rustls::ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("tls: {e}"))?;
        let mut tcp = tcp;
        while connection.is_handshaking() {
            connection
                .complete_io(&mut tcp)
                .map_err(|e| format!("tls handshake: {e}"))?;
        }
        Ok(ProbeTransport::Tls(Box::new(rustls::StreamOwned::new(
            connection, tcp,
        ))))
    }
}

enum ProbeTransport {
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
    Plain(TcpStream),
}

impl ProbeTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        match self {
            ProbeTransport::Tls(stream) => stream.write_all(data),
            ProbeTransport::Plain(stream) => stream.write_all(data),
        }
        .map_err(|e| format!("write: {e}"))
    }

    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ProbeTransport::Tls(stream) => stream.read(buffer),
            ProbeTransport::Plain(stream) => stream.read(buffer),
        }
    }
}

fn read_http_head(
    transport: &mut ProbeTransport,
    leftover: &mut Vec<u8>,
) -> Result<String, String> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err("oversized response head".into());
        }
        let n = transport
            .read(&mut chunk)
            .map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            return Err("closed during response head".into());
        }
        head.extend_from_slice(&chunk[..n]);
    }
    let split = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    *leftover = head[split..].to_vec();
    Ok(String::from_utf8_lossy(&head[..split]).to_string())
}

fn cf_ray_colo(head: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("cf-ray") {
            return None;
        }
        let value = value.trim();
        let (_, colo) = value.rsplit_once('-')?;
        if colo.is_empty() {
            None
        } else {
            Some(colo.to_string())
        }
    })
}

enum WsEvent {
    Message(Vec<u8>),
    Pong,
    Idle,
}

struct WsProbeSocket {
    transport: ProbeTransport,
    buffer: Vec<u8>,
}

impl WsProbeSocket {
    fn connect_host(endpoint: &Endpoint, mac_id: &str, entitlement: &str) -> Result<Self, String> {
        Self::connect(
            endpoint,
            &format!("/v1/host/{mac_id}"),
            &format!("Authorization: Bearer {entitlement}\r\n"),
        )
    }

    fn connect_client(endpoint: &Endpoint, mac_id: &str, token: &str) -> Result<Self, String> {
        Self::connect(
            endpoint,
            &format!("/v1/client/{mac_id}"),
            &format!("Sec-WebSocket-Protocol: unpeel-relay-token.{token}\r\n"),
        )
    }

    fn connect(endpoint: &Endpoint, path: &str, extra_headers: &str) -> Result<Self, String> {
        let addr = endpoint.resolve()?;
        let tcp = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
        tcp.set_nodelay(true).ok();
        tcp.set_read_timeout(Some(READ_TIMEOUT)).ok();
        tcp.set_write_timeout(Some(READ_TIMEOUT)).ok();
        let mut transport = endpoint.wrap_tls(tcp)?;
        let key =
            base64::engine::general_purpose::STANDARD.encode(crate::relay_crypto::random_bytes(16));
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\
             {extra_headers}\r\n",
            endpoint.host
        );
        transport.write_all(request.as_bytes())?;
        let mut leftover = Vec::new();
        let head = read_http_head(&mut transport, &mut leftover)?;
        if !head.starts_with("HTTP/1.1 101") {
            let status = head.lines().next().unwrap_or("").to_string();
            return Err(format!("upgrade refused: {status}"));
        }
        let expected = crate::remote_server::websocket_accept_key(&key);
        if !head.lines().any(|line| {
            line.to_ascii_lowercase()
                .starts_with("sec-websocket-accept:")
                && line.split(':').nth(1).map(str::trim) == Some(expected.as_str())
        }) {
            return Err("bad Sec-WebSocket-Accept".into());
        }
        Ok(Self {
            transport,
            buffer: leftover,
        })
    }

    fn send_binary(&mut self, payload: &[u8]) -> Result<(), String> {
        self.send_opcode(0x2, payload)
    }

    fn send_ping(&mut self) -> Result<(), String> {
        self.send_opcode(0x9, b"")
    }

    fn send_opcode(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut frame = vec![0x80 | opcode];
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len < 65_536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask: [u8; 4] = crate::relay_crypto::random_bytes(4).try_into().unwrap();
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.transport.write_all(&frame)
    }

    /// One WS event within `timeout`: a binary/text message, a pong (pings
    /// are answered inline), or Idle on timeout.
    fn receive_event(&mut self, timeout: Duration) -> Result<WsEvent, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((opcode, payload, consumed)) = parse_ws_frame(&self.buffer)? {
                self.buffer.drain(..consumed);
                match opcode {
                    0x1 | 0x2 => return Ok(WsEvent::Message(payload)),
                    0x9 => {
                        self.send_opcode(0xA, &payload)?;
                        continue;
                    }
                    0xA => return Ok(WsEvent::Pong),
                    0x8 => return Err("relay closed the socket".into()),
                    other => return Err(format!("unsupported ws opcode {other:#x}")),
                }
            }
            if Instant::now() >= deadline {
                return Ok(WsEvent::Idle);
            }
            let mut chunk = [0u8; 8192];
            match self.transport.read(&mut chunk) {
                Ok(0) => return Err("relay closed the connection".into()),
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Ok(WsEvent::Idle)
                }
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }
}

/// Parse one complete server frame from `buffer`, if present. Servers send
/// unmasked frames; the relay never fragments, so FIN-less frames are an
/// error rather than a state machine.
fn parse_ws_frame(buffer: &[u8]) -> Result<Option<(u8, Vec<u8>, usize)>, String> {
    if buffer.len() < 2 {
        return Ok(None);
    }
    let fin = buffer[0] & 0x80 != 0;
    let opcode = buffer[0] & 0x0f;
    if !fin {
        return Err("unexpected fragmented ws frame".into());
    }
    if buffer[1] & 0x80 != 0 {
        return Err("server frame unexpectedly masked".into());
    }
    let (length, header) = match buffer[1] & 0x7f {
        126 => {
            if buffer.len() < 4 {
                return Ok(None);
            }
            (u16::from_be_bytes([buffer[2], buffer[3]]) as usize, 4)
        }
        127 => {
            if buffer.len() < 10 {
                return Ok(None);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&buffer[2..10]);
            (u64::from_be_bytes(bytes) as usize, 10)
        }
        small => (small as usize, 2),
    };
    if length > 2 * 1024 * 1024 {
        return Err("oversized ws frame".into());
    }
    if buffer.len() < header + length {
        return Ok(None);
    }
    Ok(Some((
        opcode,
        buffer[header..header + length].to_vec(),
        header + length,
    )))
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn print_stat(label: &str, samples: &[Duration]) {
    if samples.is_empty() {
        println!("  {label:<26}(no samples)");
        return;
    }
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort();
    let min = sorted[0];
    let p50 = percentile(&sorted, 50);
    let p95 = percentile(&sorted, 95);
    let max = *sorted.last().unwrap();
    println!(
        "  {label:<26}min {:>9}  p50 {:>9}  p95 {:>9}  max {:>9}",
        format_duration(min),
        format_duration(p50),
        format_duration(p95),
        format_duration(max),
    );
}

fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    let index = (sorted.len().saturating_sub(1)) * pct / 100;
    sorted[index]
}

fn format_duration(duration: Duration) -> String {
    let micros = duration.as_micros();
    if micros < 1000 {
        format!("{micros} µs")
    } else {
        format!("{:.1} ms", micros as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_frame_parsing_handles_partials_lengths_and_control() {
        assert!(parse_ws_frame(&[]).unwrap().is_none());
        assert!(parse_ws_frame(&[0x82]).unwrap().is_none());

        // Small unmasked binary frame.
        let mut frame = vec![0x82, 3];
        frame.extend_from_slice(b"abc");
        let (opcode, payload, consumed) = parse_ws_frame(&frame).unwrap().unwrap();
        assert_eq!(
            (opcode, payload.as_slice(), consumed),
            (0x2, &b"abc"[..], 5)
        );

        // Extended 16-bit length, incomplete then complete.
        let mut frame = vec![0x82, 126, 0x01, 0x00];
        assert!(parse_ws_frame(&frame).unwrap().is_none());
        frame.extend(std::iter::repeat_n(0u8, 256));
        let (_, payload, consumed) = parse_ws_frame(&frame).unwrap().unwrap();
        assert_eq!((payload.len(), consumed), (256, 260));

        // Masked server frames and fragmentation are protocol errors.
        assert!(parse_ws_frame(&[0x82, 0x83, 0, 0, 0, 0, 1, 2, 3]).is_err());
        assert!(parse_ws_frame(&[0x02, 1, 0]).is_err());
    }

    #[test]
    fn cf_ray_colo_extracts_the_edge_code() {
        let head = "HTTP/1.1 200 OK\r\nCF-Ray: 8cbb01234abcd-OSL\r\n\r\n";
        assert_eq!(cf_ray_colo(head), Some("OSL".into()));
        assert_eq!(cf_ray_colo("HTTP/1.1 200 OK\r\n\r\n"), None);
    }

    #[test]
    fn percentile_is_index_stable_at_the_edges() {
        let sorted: Vec<Duration> = (1..=10).map(Duration::from_millis).collect();
        assert_eq!(percentile(&sorted, 50), Duration::from_millis(5));
        assert_eq!(percentile(&sorted, 95), Duration::from_millis(9));
        assert_eq!(percentile(&sorted[..1], 95), Duration::from_millis(1));
    }
}
