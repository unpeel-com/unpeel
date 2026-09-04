use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct ServeProcess {
    child: Child,
    home: PathBuf,
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

struct FakeNativeFrontend {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeNativeFrontend {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                        let mut request = [0u8; 4096];
                        let _ = stream.read(&mut request);
                        let body = r#"{"projects":[],"mobile_endpoint_handoff":1}"#;
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for FakeNativeFrontend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn isolated_home() -> PathBuf {
    std::env::temp_dir().join(format!("unpeel-serve-test-{}", uuid::Uuid::new_v4()))
}

fn reserve_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    condition()
}

/// The pin a paired phone holds: the Host certificate under this workspace's
/// `remote/tls`. Generating it here first means serve loads this exact file.
fn host_certificate_fingerprint(home: &Path) -> String {
    unpeel_core::remote_server::ensure_tls_material_in(&home.join("remote").join("tls"))
        .expect("workspace Host certificate")
        .fingerprint
}

/// A bearer request over the direct `/mobile` endpoint: TLS, pinned to the
/// Host certificate, the way the phone speaks to it.
fn mobile_request(home: &Path, port: u16, token: &str) -> (u16, serde_json::Value) {
    use unpeel_core::rustls;
    let config = Arc::new(unpeel_core::remote_attach::pinned_client_config(Some(
        host_certificate_fingerprint(home),
    )));
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let connection = rustls::ClientConnection::new(config, name).unwrap();
    let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut stream = rustls::StreamOwned::new(connection, tcp);
    write!(
        stream,
        "GET /mobile/bootstrap HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.flush().unwrap();
    let mut raw = Vec::new();
    // The server closes without a TLS close_notify; keep what arrived.
    let _ = stream.read_to_end(&mut raw);
    let response = String::from_utf8(raw).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let body = serde_json::from_str(body).unwrap();
    (status, body)
}

/// The pre-TLS phone's request shape: cleartext HTTP with the bearer.
fn plaintext_mobile_request(port: u16, token: &str) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /mobile/bootstrap HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let response = String::from_utf8(raw).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    (status, serde_json::from_str(body).unwrap())
}

fn write_pairing_fixture(home: &Path, port: u16, token: &str) {
    let mobile = home.join("mobile");
    std::fs::create_dir_all(&mobile).unwrap();
    std::fs::write(mobile.join("server-port"), format!("{port}\n")).unwrap();
    let devices = serde_json::json!({
        "version": 1,
        "devices": [{
            "id": "serve-test-phone",
            "name": "Serve Test Phone",
            "platform": "iOS",
            "tokenHash": unpeel_serve::pairing::sha256_hex(token),
            "pairedAtUnixMs": 1,
            "relayAllowed": false
        }]
    });
    std::fs::write(
        mobile.join("devices.json"),
        serde_json::to_vec(&devices).unwrap(),
    )
    .unwrap();
}

#[test]
fn serve_runs_the_host_protocol_and_holds_one_workspace_lease() {
    let home = isolated_home();
    let port = reserve_port();
    let token = "serve-command-controller-token";
    write_pairing_fixture(&home, port, token);

    let child = Command::new(env!("CARGO_BIN_EXE_unpeel"))
        .arg("serve")
        .env("UNPEEL_HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start unpeel serve");
    let mut process = ServeProcess {
        child,
        home: home.clone(),
    };

    let status_path = home.join("serve.json");
    assert!(
        wait_until(Duration::from_secs(10), || {
            std::fs::read(&status_path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                .and_then(|status| status.get("directPort").and_then(|value| value.as_u64()))
                == Some(u64::from(port))
        }),
        "serve never published its Direct endpoint"
    );

    let (status, bootstrap) = mobile_request(&home, port, token);
    assert_eq!(status, 200);
    assert_eq!(bootstrap["hostProtocol"]["majorVersion"], 1);
    assert!(bootstrap["hostProtocol"]["capabilities"].is_array());
    assert!(
        bootstrap["hostProtocol"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "host.mobile.tls"),
        "bootstrap advertises the TLS direct endpoint: {bootstrap}"
    );
    assert_eq!(
        bootstrap["serverVersion"],
        env!("CARGO_PKG_VERSION"),
        "bootstrap carries the Host version as the phone's fallback TLS signal"
    );
    assert_eq!(
        bootstrap["remoteServerCertificateFingerprint"],
        host_certificate_fingerprint(&home),
        "bootstrap advertises the pinned Host certificate"
    );

    // The same paired token in the clear is refused before it is looked up.
    let (status, body) = plaintext_mobile_request(port, token);
    assert_eq!(status, 426, "{body}");
    assert_eq!(body["error"], "use https");

    let duplicate = Command::new(env!("CARGO_BIN_EXE_unpeel"))
        .arg("serve")
        .env("UNPEEL_HOME", &home)
        .output()
        .expect("run duplicate serve");
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already serving this workspace"));

    let pairing_code = unpeel_serve::local_gateway::begin_pairing(&home, None, None)
        .expect("the running Host opens pairing without a second listener");
    assert!(pairing_code.starts_with("UNPEEL:1:"));
    assert_eq!(
        unpeel_serve::local_gateway::pairing_status(&home).unwrap(),
        unpeel_serve::local_gateway::PairingStatus::Active
    );
    unpeel_serve::local_gateway::cancel_pairing(&home).unwrap();
    assert_eq!(
        unpeel_serve::local_gateway::pairing_status(&home).unwrap(),
        unpeel_serve::local_gateway::PairingStatus::Closed
    );

    let initial_status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&status_path).unwrap()).unwrap();
    let hook_port = initial_status["hookPort"].as_u64().unwrap() as u16;
    let native = FakeNativeFrontend::start();
    std::fs::write(
        home.join("app-ports"),
        format!("{hook_port}\n{}\n", native.port),
    )
    .unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || {
            std::fs::read(&status_path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                .is_some_and(|status| {
                    status["nativeAppOwnsControllers"] == true && status.get("directPort").is_none()
                })
        }),
        "serve did not yield Controller ownership to the native app"
    );
    drop(native);
    std::fs::write(home.join("app-ports"), format!("{hook_port}\n")).unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || {
            std::fs::read(&status_path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                .is_some_and(|status| {
                    status["nativeAppOwnsControllers"] == false
                        && status["directPort"] == u64::from(port)
                })
        }),
        "serve did not reclaim Controller ownership after the native app left"
    );

    unsafe {
        libc::kill(process.child.id() as libc::pid_t, libc::SIGTERM);
    }
    assert!(
        wait_until(Duration::from_secs(10), || process
            .child
            .try_wait()
            .ok()
            .flatten()
            .is_some()),
        "serve did not stop after SIGTERM"
    );
    assert!(
        !status_path.exists(),
        "serve left a live-looking status file"
    );

    process.child = Command::new(env!("CARGO_BIN_EXE_unpeel"))
        .arg("serve")
        .env("UNPEEL_HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("restart unpeel serve");
    assert!(
        wait_until(Duration::from_secs(10), || {
            std::fs::read(&status_path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                .and_then(|status| status.get("pid").and_then(|value| value.as_u64()))
                == Some(u64::from(process.child.id()))
        }),
        "serve did not reacquire the workspace after clean shutdown"
    );
    unsafe {
        libc::kill(process.child.id() as libc::pid_t, libc::SIGTERM);
    }
    assert!(
        wait_until(Duration::from_secs(10), || process
            .child
            .try_wait()
            .ok()
            .flatten()
            .is_some()),
        "restarted serve did not stop"
    );
}
