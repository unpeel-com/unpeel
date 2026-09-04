//! Minimal Rust Controller relay downlink — route 1 of the direct-path
//! lab proof (`unpeel-apple:docs/plans/relay-direct-upgrade.md` increment 2), and the
//! first Rust sender of the shipped client-side relay protocol: the same
//! `/v1/client/` route, client hello, transcript MAC verification, and
//! sealed tunnel envelope the iOS app speaks (`RelayProtocol.swift`), so
//! the fixed transcript/crypto fixtures police both implementations.
//!
//! Deliberately minimal: blocking, single request in flight, no reconnect
//! policy, no output streaming — enough to authenticate, tunnel requests,
//! and hand `direct_path_client` its handshake material. Phase 8's full
//! Controller downlink (correlated dispatch, generations, resume) grows
//! from here rather than beside it.

use std::sync::atomic::AtomicBool;

use crate::direct_path_client::{
    negotiate_and_punch, NegotiateError, NegotiateOptions, NegotiatedPath, ProbeMaterial,
};
use crate::relay_crypto::{
    encode_client_hello, ephemeral_key, parse_host_hello, random_bytes, shared_secret,
    transcript_mac, CryptoSession,
};
use crate::relay_uplink::{connect_client_cancellable, RelayConnectError, RelaySocket};
use crate::relay_wire::{encode_tunnel_request, parse_tunnel_response, TunnelRequest};

pub struct RelayDownlink {
    socket: RelaySocket,
    crypto: CryptoSession,
    shared_secret: Vec<u8>,
    client_salt: Vec<u8>,
    host_salt: Vec<u8>,
    next_id: u64,
}

impl RelayDownlink {
    /// Dial the relay as a paired Controller device and complete the E2E
    /// handshake. `e2e_key` is the paired device's shared key; a MAC
    /// mismatch fails closed (wrong key or an impostor host — never
    /// downgraded to a warning).
    pub fn connect(
        url: &str,
        mac_id: &str,
        relay_token: &str,
        device_id: &str,
        e2e_key: &[u8],
    ) -> Result<Self, String> {
        let socket =
            connect_client_cancellable(url, mac_id, relay_token, || false).map_err(|error| {
                match error {
                    RelayConnectError::AuthorizationRejected(message) => message,
                    other => format!("{other:?}"),
                }
            })?;
        Self::handshake(socket, device_id, e2e_key)
    }

    /// The client half of the shipped handshake, over an already-open
    /// socket. Public so in-repo test harnesses can drive a fake relay end
    /// through this seam; production connects go through `connect`.
    pub fn handshake(
        mut socket: RelaySocket,
        device_id: &str,
        e2e_key: &[u8],
    ) -> Result<Self, String> {
        let client = ephemeral_key()?;
        let client_public = client.public.clone();
        let client_salt = random_bytes(16);
        socket.send(&encode_client_hello(
            device_id,
            &client_salt,
            &client_public,
        ))?;
        let reply = socket.receive()?;
        let hello = parse_host_hello(&reply).ok_or("malformed host hello")?;
        let expected = transcript_mac(
            e2e_key,
            device_id,
            &client_salt,
            &hello.salt,
            &client_public,
            &hello.ephemeral_public,
        );
        if expected.len() != hello.mac.len()
            || expected
                .iter()
                .zip(&hello.mac)
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                != 0
        {
            return Err("host hello MAC mismatch".into());
        }
        let shared = shared_secret(client, &hello.ephemeral_public)?;
        let crypto = CryptoSession::new(e2e_key, &shared, &client_salt, &hello.salt, false)?;
        Ok(Self {
            socket,
            crypto,
            shared_secret: shared,
            client_salt,
            host_salt: hello.salt,
            next_id: 0,
        })
    }

    /// One sealed tunnel request/response. Frames that fail to open or
    /// carry another id (host pushes) are skipped with bounded patience —
    /// this downlink keeps a single request in flight by construction.
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), String> {
        self.next_id += 1;
        let id = self.next_id;
        let request = TunnelRequest {
            id,
            method: method.into(),
            path: path.into(),
            query: Vec::new(),
            auth: bearer.map(|token| format!("Bearer {token}")),
            content_type: Some("application/json".into()),
            body: body.to_vec(),
        };
        let sealed = self.crypto.seal(&encode_tunnel_request(&request))?;
        self.socket.send(&sealed)?;
        for _ in 0..64 {
            let frame = self.socket.receive()?;
            let Ok(plaintext) = self.crypto.open(&frame) else {
                continue;
            };
            if let Ok(response) = parse_tunnel_response(&plaintext) {
                if response.id == id {
                    return Ok((response.status, response.body));
                }
            }
        }
        Err("no response for tunneled request".into())
    }

    /// The handshake triple `direct_path_client` derives the probe key
    /// from — exactly what the Host retained on its side.
    pub fn probe_material(&self) -> ProbeMaterial {
        ProbeMaterial {
            shared_secret: self.shared_secret.clone(),
            client_salt: self.client_salt.clone(),
            host_salt: self.host_salt.clone(),
        }
    }

    /// Negotiate a punched direct path over this connection. Blocking for
    /// up to the option's punch timeout; the current relay connection is
    /// untouched either way (the punched socket is returned for a later
    /// migration bootstrap, per the contract).
    pub fn negotiate_direct_path(
        &mut self,
        bearer: Option<&str>,
        options: &NegotiateOptions,
        cancelled: &AtomicBool,
    ) -> Result<NegotiatedPath, NegotiateError> {
        let material = self.probe_material();
        let bearer = bearer.map(str::to_owned);
        let mut post = |path: &str, body: &[u8]| -> Result<(u16, Vec<u8>), String> {
            self.request("POST", path, bearer.as_deref(), body)
        };
        negotiate_and_punch(&mut post, &material, options, cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay_crypto::{encode_host_hello, parse_client_hello};
    use crate::relay_wire::encode_bounded_tunnel_response;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// Fake relay+host end: raw TCP carrying WS frames — server side sends
    /// unmasked frames and unmasks the client's, exactly the relay's view.
    struct FakeHostEnd {
        stream: TcpStream,
        buffer: Vec<u8>,
    }

    impl FakeHostEnd {
        fn send_payload(&mut self, payload: &[u8]) {
            let mut frame = vec![0x82u8];
            if payload.len() < 126 {
                frame.push(payload.len() as u8);
            } else {
                frame.push(126);
                frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
            }
            frame.extend_from_slice(payload);
            self.stream.write_all(&frame).expect("fake host write");
        }

        fn receive_payload(&mut self) -> Vec<u8> {
            loop {
                if self.buffer.len() >= 2 {
                    let masked = self.buffer[1] & 0x80 != 0;
                    assert!(masked, "client frames must be masked");
                    let (length, header) = match self.buffer[1] & 0x7f {
                        126 => {
                            if self.buffer.len() < 4 {
                                self.fill();
                                continue;
                            }
                            (
                                u16::from_be_bytes([self.buffer[2], self.buffer[3]]) as usize,
                                4,
                            )
                        }
                        127 => panic!("oversized test frame"),
                        small => (small as usize, 2),
                    };
                    let total = header + 4 + length;
                    if self.buffer.len() >= total {
                        let mask: [u8; 4] = self.buffer[header..header + 4].try_into().unwrap();
                        let payload: Vec<u8> = self.buffer[header + 4..total]
                            .iter()
                            .enumerate()
                            .map(|(index, byte)| byte ^ mask[index % 4])
                            .collect();
                        self.buffer.drain(..total);
                        return payload;
                    }
                }
                self.fill();
            }
        }

        fn fill(&mut self) {
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).expect("fake host read");
            assert!(read > 0, "downlink closed early");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn downlink_pair() -> (RelaySocket, FakeHostEnd) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let client = TcpStream::connect(listener.local_addr().unwrap()).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (
            RelaySocket::plain_for_tests(client),
            FakeHostEnd {
                stream: server,
                buffer: Vec::new(),
            },
        )
    }

    /// The full client↔host handshake using ONLY the host-side primitives
    /// the serve relay loop uses — proving the new client codecs are the
    /// same protocol, then a sealed request/response round trip.
    #[test]
    fn downlink_handshakes_and_tunnels_against_host_primitives() {
        let e2e = random_bytes(32);
        let e2e_host = e2e.clone();
        let (socket, mut host_end) = downlink_pair();

        let host = std::thread::spawn(move || {
            // Host side, verbatim shape of the serve relay hello handling.
            let hello_payload = host_end.receive_payload();
            let hello = parse_client_hello(&hello_payload).expect("client hello parses");
            assert_eq!(hello.device_id, "probe-device");
            let ephemeral = ephemeral_key().expect("host ephemeral");
            let host_public = ephemeral.public.clone();
            let shared = shared_secret(ephemeral, &hello.ephemeral_public).expect("host shared");
            let host_salt = random_bytes(16);
            let mac = transcript_mac(
                &e2e_host,
                &hello.device_id,
                &hello.salt,
                &host_salt,
                &hello.ephemeral_public,
                &host_public,
            );
            let mut session = CryptoSession::new(&e2e_host, &shared, &hello.salt, &host_salt, true)
                .expect("host session");
            host_end.send_payload(&encode_host_hello(&host_salt, &host_public, &mac));

            // One tunneled request.
            let sealed = host_end.receive_payload();
            let plaintext = session.open(&sealed).expect("host opens request");
            let request =
                crate::relay_wire::parse_tunnel_request_strict(&plaintext).expect("request");
            assert_eq!(request.path, "/mobile/direct-path");
            assert_eq!(request.auth.as_deref(), Some("Bearer test-bearer"));
            let response = session
                .seal(&encode_bounded_tunnel_response(
                    request.id,
                    200,
                    b"{\"ok\":true}",
                ))
                .expect("host seals response");
            host_end.send_payload(&response);
        });

        let mut downlink =
            RelayDownlink::handshake(socket, "probe-device", &e2e).expect("handshake");
        let material = downlink.probe_material();
        assert_eq!(material.client_salt.len(), 16);
        assert_eq!(material.host_salt.len(), 16);
        assert_eq!(material.shared_secret.len(), 32);
        let (status, body) = downlink
            .request("POST", "/mobile/direct-path", Some("test-bearer"), b"{}")
            .expect("tunneled request");
        assert_eq!(status, 200);
        assert_eq!(body, b"{\"ok\":true}");
        host.join().expect("host thread");
    }

    #[test]
    fn a_wrong_e2e_key_fails_the_handshake_closed() {
        let e2e = random_bytes(32);
        let wrong = random_bytes(32);
        let (socket, mut host_end) = downlink_pair();
        let host = std::thread::spawn(move || {
            let hello_payload = host_end.receive_payload();
            let hello = parse_client_hello(&hello_payload).expect("client hello parses");
            let ephemeral = ephemeral_key().expect("host ephemeral");
            let host_public = ephemeral.public.clone();
            let host_salt = random_bytes(16);
            // Host computes the MAC under a DIFFERENT key.
            let mac = transcript_mac(
                &wrong,
                &hello.device_id,
                &hello.salt,
                &host_salt,
                &hello.ephemeral_public,
                &host_public,
            );
            host_end.send_payload(&encode_host_hello(&host_salt, &host_public, &mac));
        });
        let error = RelayDownlink::handshake(socket, "probe-device", &e2e)
            .err()
            .expect("mismatched key must fail");
        assert!(error.contains("MAC mismatch"), "{error}");
        host.join().expect("host thread");
    }
}
