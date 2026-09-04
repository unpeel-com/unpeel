//! Real-process proof for the loopback workspace gateway
//! (unpeel-apple:docs/plans/workspaces-unification.md phase 2): the Controller-side
//! `LocalProcessConnection` spawns this build's `unpeel-host __remote_stdio__`
//! directly against a workspace home and drives the same semantic
//! `RemoteSessionBackend` as SSH — including a home that does not exist yet.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use unpeel_core::host_connection::{
    HostCall, HostConnection, HostConnectionError, RequestSemantics,
};
use unpeel_core::remote_session_backend::{
    RemoteEffectFailureKind, RemoteOutputPollOptions, RemoteProjectOrganizationPatch,
    RemoteSessionBackend, RemoteSessionBackendError,
};
use unpeel_core::ssh_connection::LocalProcessConnection;

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    condition()
}

fn adapter_control(
    stream: &mut UnixStream,
    id: u64,
    body: serde_json::Value,
) -> unpeel_core::relay_wire::TunnelResponse {
    let request = unpeel_core::relay_wire::TunnelRequest {
        id,
        method: "POST".into(),
        path: "/_unpeel/platform-adapter".into(),
        query: Vec::new(),
        auth: None,
        content_type: Some("application/json".into()),
        body: serde_json::to_vec(&body).unwrap(),
    };
    unpeel_core::remote_stdio::write_frame(
        stream,
        unpeel_core::remote_stdio::FRAME_KIND_REQUEST,
        &unpeel_core::relay_wire::encode_tunnel_request(&request),
    )
    .unwrap();
    let frame = unpeel_core::remote_stdio::read_frame(stream)
        .unwrap()
        .expect("adapter control response");
    assert_eq!(frame.kind, unpeel_core::remote_stdio::FRAME_KIND_RESPONSE);
    unpeel_core::relay_wire::parse_tunnel_response(&frame.payload).unwrap()
}

fn read_fixed_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let count = stream.read(&mut chunk).unwrap();
        request.extend_from_slice(&chunk[..count]);
        let complete = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .and_then(|separator| {
                let head = std::str::from_utf8(&request[..separator]).ok()?;
                let length = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })?;
                Some(request.len() >= separator + 4 + length)
            })
            .unwrap_or(false);
        if complete || count == 0 {
            return request;
        }
    }
}

// Deadlines in this file are upper bounds under a fully parallel
// `cargo test --workspace` (dozens of process tests spawning Hosts at once):
// every wait returns the moment its condition holds, so a generous bound
// costs nothing on the happy path and removes load-induced flakes.
fn recv_platform_callback(receiver: &mpsc::Receiver<Vec<u8>>, operation: &str) -> String {
    let needle = format!("\"operation\":\"{operation}\"");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let request = receiver
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("platform callback for {operation}"));
        let request = String::from_utf8(request).unwrap();
        if request.contains(&needle) {
            return request;
        }
    }
}

fn recv_platform_callbacks(
    receiver: &mpsc::Receiver<Vec<u8>>,
    operations: &[&str],
) -> std::collections::HashMap<String, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut found = std::collections::HashMap::new();
    while found.len() < operations.len() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let request = receiver
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("platform callbacks: {operations:?}, found={found:?}"));
        let request = String::from_utf8(request).unwrap();
        if let Some(operation) = operations
            .iter()
            .find(|operation| request.contains(&format!("\"operation\":\"{operation}\"")))
        {
            found.insert((*operation).to_string(), request);
        }
    }
    found
}

fn direct_mobile_request(
    port: u16,
    method: &str,
    path: &str,
    bearer: &str,
    body: &[u8],
) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {bearer}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}

struct ScopedChild(Child);

impl ScopedChild {
    fn stop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        if !wait_until(Duration::from_secs(15), || {
            self.0.try_wait().ok().flatten().is_some()
        }) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl Drop for ScopedChild {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Fixture {
    root: PathBuf,
    host_home: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Keep the Host home short: macOS Unix-domain socket paths have a
        // small fixed budget, unlike the deeply nested default TMPDIR.
        let root =
            PathBuf::from("/tmp").join(format!("u-lg-{}-{label}-{nonce:x}", std::process::id()));
        let host_home = root.join("workspace-home");
        let session_dir = host_home.join("app-sessions").join("s1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            host_home.join("app-state.json"),
            br#"{
              "projects":[{"id":"p1","name":"Project","path":"/tmp","sort_order":0}],
              "presets":[],
              "pinned_sessions":{},
              "active_tabs":{}
            }"#,
        )
        .unwrap();
        std::fs::write(
            session_dir.join("manifest.json"),
            br#"{
              "session":{"id":"s1","project_id":"p1","label":"Workspace Session","command":"cat","created_at":1},
              "cwd":"/tmp","state":"exited","pid":null,"exit_code":0
            }"#,
        )
        .unwrap();
        std::fs::write(session_dir.join("output.bin"), b"hello").unwrap();
        Self { root, host_home }
    }

    fn connection(&self) -> LocalProcessConnection {
        LocalProcessConnection::local_gateway(
            Path::new(env!("CARGO_BIN_EXE_unpeel-host")),
            &self.host_home,
        )
        .unwrap()
    }

    fn service_connection(&self) -> LocalProcessConnection {
        LocalProcessConnection::local_host_service(
            Path::new(env!("CARGO_BIN_EXE_unpeel-host")),
            &self.host_home,
        )
        .unwrap()
    }

    fn gateway_start_count(&self) -> usize {
        std::fs::read_to_string(self.host_home.join("remote").join("audit.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("ssh_stdio_start"))
            .count()
    }

    /// Explicit teardown at the end of a passing test; `Drop` runs the same
    /// steps when a test panics, so a failed assertion cannot leak the
    /// detached PTY cores a worker started under this root.
    fn cleanup(self) {
        drop(self);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        teardown_fixture_root(&self.root);
    }
}

/// A worker started under a fixture root leaves detached `__pty_core__`
/// processes behind by design (terminals must survive the worker). Ask each
/// of them to exit, then remove the root. Guarded so a wrong `root` can never
/// delete anything but a `/tmp/u-lg-*` fixture.
fn teardown_fixture_root(root: &Path) {
    assert_eq!(root.parent(), Some(Path::new("/tmp")));
    assert!(root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("u-lg-")));
    unpeel_core::pty_core::shutdown_cores_under(root, Duration::from_secs(15));
    let _ = std::fs::remove_dir_all(root);
}

/// Owns a bare fixture root (no pre-seeded workspace home) with the same
/// panic-safe teardown as `Fixture`.
struct FixtureRoot(PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        teardown_fixture_root(&self.0);
    }
}

#[test]
fn semantic_backend_serves_the_configured_home_not_the_inherited_one() {
    let fixture = Fixture::new("backend");
    // A Controller can itself run as a workspace instance with UNPEEL_HOME
    // set. The gateway must serve ONLY the configured home; the inherited
    // env is stripped at spawn.
    let decoy_home = fixture.root.join("decoy-home");
    std::env::set_var("UNPEEL_HOME", &decoy_home);
    std::env::set_var("HERDR_SOCKET", "/tmp/never-used");
    let backend = RemoteSessionBackend::new(Arc::new(fixture.connection()));

    let bootstrap = backend.bootstrap().unwrap();
    assert_eq!(bootstrap.snapshot.sessions.len(), 1);
    assert_eq!(bootstrap.snapshot.sessions[0].title, "Workspace Session");

    let initial = backend
        .poll_output("s1", RemoteOutputPollOptions::default())
        .unwrap();
    assert_eq!(initial.bytes(), b"hello");
    assert!(initial.reset_required());
    initial.commit().unwrap();
    assert_eq!(backend.committed_output_offset("s1"), Some(5));

    std::fs::write(
        fixture
            .host_home
            .join("app-sessions")
            .join("s1")
            .join("output.bin"),
        b"hello world",
    )
    .unwrap();
    let next = backend
        .poll_output("s1", RemoteOutputPollOptions::default())
        .unwrap();
    assert_eq!(next.bytes(), b" world");
    next.commit().unwrap();
    assert_eq!(backend.committed_output_offset("s1"), Some(11));

    backend.mark_session_read("s1").unwrap();
    assert!(fixture
        .host_home
        .join("app-sessions")
        .join("s1")
        .join("read.json")
        .is_file());
    assert!(
        !decoy_home.exists(),
        "gateway leaked the Controller's inherited UNPEEL_HOME"
    );
    assert_eq!(fixture.gateway_start_count(), 1);

    backend.disconnect();
    std::env::remove_var("UNPEEL_HOME");
    std::env::remove_var("HERDR_SOCKET");
    fixture.cleanup();
}

#[test]
fn local_gateway_becomes_a_proxy_when_the_unified_host_service_is_live() {
    let fixture = Fixture::new("service-proxy");
    let real_home = fixture.root.join(".unpeel");
    std::fs::create_dir_all(&real_home).unwrap();
    let registry = serde_json::json!({
        "version": 1,
        "profiles": [{
            "id": "service-proxy",
            "name": "Service Proxy",
            "home": fixture.host_home,
            "createdAt": 1
        }]
    });
    std::fs::write(
        real_home.join("profiles.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let service = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg(unpeel_serve::service::SERVICE_ARG)
        // Test workers never download the Browser MCP or Computer Use engine.
        .env("UNPEEL_BROWSER_ENGINE_INSTALL", "0")
        .env("UNPEEL_COMPUTER_ENGINE_INSTALL", "0")
        .env("HOME", &fixture.root)
        .env_remove("UNPEEL_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start unified Host service");
    let mut service = ScopedChild(service);
    assert!(
        wait_until(Duration::from_secs(15), || {
            unpeel_core::remote_stdio::local_host_socket_path(&fixture.host_home).exists()
                && fixture.host_home.join("serve.json").exists()
        }),
        "workspace Host worker never became ready"
    );

    let backend = RemoteSessionBackend::new(Arc::new(fixture.service_connection()));
    let bootstrap = backend.bootstrap().unwrap();
    assert_eq!(bootstrap.snapshot.sessions.len(), 1);
    assert_eq!(bootstrap.snapshot.sessions[0].title, "Workspace Session");
    assert_eq!(
        fixture.gateway_start_count(),
        0,
        "the compatibility child ran a second semantic Host instead of proxying the service"
    );
    backend.disconnect();

    unsafe {
        libc::kill(service.0.id() as libc::pid_t, libc::SIGTERM);
    }
    assert!(wait_until(Duration::from_secs(15), || service
        .0
        .try_wait()
        .ok()
        .flatten()
        .is_some()));
    fixture.cleanup();
}

#[test]
fn unified_worker_advertises_and_withdraws_connection_scoped_platform_adapter() {
    let fixture = Fixture::new("platform-adapter");
    let real_home = fixture.root.join(".unpeel");
    std::fs::create_dir_all(&real_home).unwrap();
    std::fs::write(
        real_home.join("profiles.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "profiles": [{
                "id": "platform-adapter",
                "name": "Platform Adapter",
                "home": fixture.host_home,
                "createdAt": 1
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let mobile_dir = fixture.host_home.join("mobile");
    std::fs::create_dir_all(&mobile_dir).unwrap();
    // SHA-256("platform-device-token"). The raw bearer never lands on disk.
    std::fs::write(
        mobile_dir.join("devices.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "devices": [{
                "id": "phone-process",
                "name": "iPhone",
                "platform": "iOS",
                "tokenHash": "80779dabc1f0099aacf8224626b3fea53386691433b405bdf15a8e122889069e",
                "pairedAtUnixMs": 1,
                "relayTokenHash": "unused"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let screenshot_dir = fixture
        .host_home
        .join("app-sessions/s1/artifacts/browser/screenshots");
    std::fs::create_dir_all(&screenshot_dir).unwrap();
    std::fs::write(screenshot_dir.join("shot.png"), b"original-image").unwrap();
    let app_session_dir = fixture.host_home.join("app-sessions/app1");
    std::fs::create_dir_all(&app_session_dir).unwrap();
    std::fs::write(
        app_session_dir.join("manifest.json"),
        br#"{
          "session":{"id":"app1","project_id":"p1","label":"Hosted App","command":"notes","created_at":2},
          "cwd":"/tmp","state":"running","pid":null,
          "active_app":{"id":"notes","name":"Notes"}
        }"#,
    )
    .unwrap();
    std::fs::write(app_session_dir.join("output.bin"), b"").unwrap();
    let open_target = fixture.root.join("open-me.txt");
    std::fs::write(&open_target, b"open me").unwrap();
    let service = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg(unpeel_serve::service::SERVICE_ARG)
        // Test workers never download the Browser MCP or Computer Use engine.
        .env("UNPEEL_BROWSER_ENGINE_INSTALL", "0")
        .env("UNPEEL_COMPUTER_ENGINE_INSTALL", "0")
        .env("HOME", &fixture.root)
        .env_remove("UNPEEL_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start unified Host service");
    let mut service = ScopedChild(service);
    let socket = unpeel_core::remote_stdio::local_host_socket_path(&fixture.host_home);
    assert!(
        wait_until(Duration::from_secs(15), || socket.exists()),
        "workspace Host worker never became ready"
    );

    let callback = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let callback_port = callback.local_addr().unwrap().port();
    callback.set_nonblocking(true).unwrap();
    let (captured_tx, captured_rx) = mpsc::channel();
    let (callback_stop_tx, callback_stop_rx) = mpsc::channel();
    let overlay_plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>unpeel.native.appTint</key><string>teal</string><key>unpeel.native.sessionTitles</key><dict><key>s1</key><string>Native Overlay Title</string></dict></dict></plist>"#;
    let overlay_body = serde_json::to_vec(&serde_json::json!({
        "defaultsPlistBase64": base64::engine::general_purpose::STANDARD.encode(overlay_plist),
    }))
    .unwrap();
    let callback_worker = std::thread::spawn(move || loop {
        if callback_stop_rx.try_recv().is_ok() {
            break;
        }
        let (mut stream, _) = match callback.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => panic!("platform callback accept: {error}"),
        };
        // macOS may propagate O_NONBLOCK from the listener to an accepted
        // socket. The fake callback is ordinary request/response HTTP; make
        // its read deterministic when several adapter calls arrive quickly.
        stream.set_nonblocking(false).unwrap();
        let request = read_fixed_http_request(&mut stream);
        let request_text = String::from_utf8_lossy(&request);
        let body = if request_text.contains("\"operation\":\"computer.status\"") {
            br#"{"available":true,"ready":true}"#.as_slice()
        } else if request_text.contains("\"operation\":\"overlay.snapshot\"") {
            overlay_body.as_slice()
        } else if request_text.contains("\"operation\":\"link.entitlement.refresh\"") {
            br#"{"available":false}"#.as_slice()
        } else if request_text.contains("\"operation\":\"artifact.thumbnail\"") {
            br#"{"sessionID":"s1","kind":"screenshots","name":"shot.png","contentType":"image/jpeg","offset":0,"nextOffset":3,"totalSize":3,"dataBase64":"dGh1","capturedAtUnixMs":1}"#.as_slice()
        } else if request_text.contains("\"operation\":\"relay.credentials.recover\"") {
            br#"{"relayURL":"wss://relay.example.test/v1","macID":"process-host","relayToken":"rotated-token","e2eKeyB64":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#.as_slice()
        } else {
            br#"{"ok":true}"#.as_slice()
        };
        captured_tx.send(request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });

    let mut registration = UnixStream::connect(&socket).unwrap();
    registration
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    registration
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut capabilities = vec![
        "app.open-in-editor",
        "artifact.thumbnail",
        "link.entitlement.refresh",
        "mobile.e2e-key.reconcile",
        "overlay.snapshot",
        "overlay.project-color.set",
        "push.register",
        "relay.credentials.recover",
        "session.notify_when_done.set",
    ];
    if cfg!(target_os = "macos") {
        capabilities.push("computer.status");
    }
    let registered = adapter_control(
        &mut registration,
        1,
        serde_json::json!({
            "action": "register",
            "registration": {
                "version": 1,
                "instanceID": "native-process-proof",
                "callbackPort": callback_port,
                "callbackToken": "0123456789abcdef0123456789abcdef",
                "capabilities": capabilities
            }
        }),
    );
    assert_eq!(registered.status, 200);
    let status = adapter_control(
        &mut registration,
        2,
        serde_json::json!({ "action": "status" }),
    );
    assert_eq!(
        status.status,
        200,
        "adapter status: {}",
        String::from_utf8_lossy(&status.body)
    );
    assert!(String::from_utf8_lossy(&status.body).contains("session.notify_when_done.set"));
    assert!(String::from_utf8_lossy(&status.body).contains("push.register"));
    assert!(String::from_utf8_lossy(&status.body).contains("relay.credentials.recover"));

    let backend = RemoteSessionBackend::new(Arc::new(fixture.service_connection()));
    let initial_operations = if cfg!(target_os = "macos") {
        vec![
            "computer.status",
            "link.entitlement.refresh",
            "mobile.e2e-key.reconcile",
            "overlay.snapshot",
        ]
    } else {
        vec![
            "link.entitlement.refresh",
            "mobile.e2e-key.reconcile",
            "overlay.snapshot",
        ]
    };
    let initial_callbacks = recv_platform_callbacks(&captured_rx, &initial_operations);
    assert!(initial_callbacks["link.entitlement.refresh"].contains("\"macID\":"));
    assert!(initial_callbacks["mobile.e2e-key.reconcile"].contains("\"action\":\"sync\""));
    assert!(initial_callbacks["overlay.snapshot"].contains("\"request\":{}"));
    #[cfg(target_os = "macos")]
    {
        let request = &initial_callbacks["computer.status"];
        assert!(request.contains("\"request\":{}"));
        let mut observed_status = None;
        let published = wait_until(Duration::from_secs(30), || {
            observed_status = backend.bootstrap().ok().and_then(|bootstrap| {
                bootstrap
                    .snapshot
                    .workspace_settings
                    .and_then(|settings| settings.experimental_settings)
                    .map(|settings| {
                        (
                            settings.computer_use_available,
                            settings.computer_use_ready,
                            settings.computer_use_unavailable_reason,
                        )
                    })
            });
            observed_status == Some((Some(true), Some(true), None))
        });
        assert!(
            published,
            "worker did not publish native Computer Use status: {observed_status:?}"
        );
    }
    let bootstrap = backend.bootstrap().unwrap();
    assert!(
        bootstrap
            .snapshot
            .host_protocol
            .as_ref()
            .is_some_and(|protocol| protocol.supports("session.notify_when_done.set")),
        "protocol={:?} status={}",
        bootstrap.snapshot.host_protocol,
        std::fs::read_to_string(fixture.host_home.join("serve.json")).unwrap_or_default()
    );
    // The local socket routes these to the worker's live authorities, so a
    // Controller must see them advertised or it refuses the effect client-side.
    for capability in ["approval.answer", "pairing.invitation"] {
        assert!(
            bootstrap
                .snapshot
                .host_protocol
                .as_ref()
                .is_some_and(|protocol| protocol.supports(capability)),
            "local bootstrap must advertise {capability}: {:?}",
            bootstrap.snapshot.host_protocol
        );
    }
    backend
        .set_session_notify_when_done("s1", true)
        .expect("registered native adapter handles the Host verb");
    let request = recv_platform_callback(&captured_rx, "session.notify_when_done.set");
    assert!(request.contains("POST /_unpeel/platform-adapter/call HTTP/1.1"));
    assert!(request.contains("Authorization: Bearer 0123456789abcdef0123456789abcdef"));
    assert!(request.contains("\"operation\":\"session.notify_when_done.set\""));

    backend
        .set_project_organization(
            "p1",
            &RemoteProjectOrganizationPatch {
                color_id: Some("amber".into()),
                ..RemoteProjectOrganizationPatch::default()
            },
        )
        .expect("folder-color write reaches the native overlay adapter");
    let request = recv_platform_callback(&captured_rx, "overlay.project-color.set");
    assert!(request.contains("\"operation\":\"overlay.project-color.set\""));
    assert!(request.contains("\"projectID\":\"p1\""));
    assert!(request.contains("\"colorID\":\"amber\""));

    let mut direct_port = None;
    assert!(wait_until(Duration::from_secs(30), || {
        direct_port = std::fs::read(fixture.host_home.join("serve.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .and_then(|status| status.get("directPort").and_then(serde_json::Value::as_u64))
            .and_then(|port| u16::try_from(port).ok());
        direct_port.is_some()
    }));
    let direct_port = direct_port.unwrap();
    let hook_port = std::fs::read(fixture.host_home.join("serve.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|status| status.get("hookPort").and_then(serde_json::Value::as_u64))
        .and_then(|port| u16::try_from(port).ok())
        .expect("worker hook port");
    let mut theme_response = String::new();
    assert!(
        wait_until(Duration::from_secs(30), || {
            theme_response = String::from_utf8(direct_mobile_request(
                hook_port,
                "POST",
                "/app-theme/app1",
                "",
                br#"{"scheme":"dark"}"#,
            ))
            .unwrap();
            theme_response.contains("#4EC3C9")
        }),
        "native overlay theme response: {theme_response}"
    );
    let open_body = serde_json::to_vec(&serde_json::json!({
        "path": open_target,
    }))
    .unwrap();
    let open = direct_mobile_request(hook_port, "POST", "/open-in-editor/app1", "", &open_body);
    assert!(
        String::from_utf8_lossy(&open).starts_with("HTTP/1.1 200"),
        "open-in-editor response: {}",
        String::from_utf8_lossy(&open)
    );
    let request = recv_platform_callback(&captured_rx, "app.open-in-editor");
    assert!(request.contains("open-me.txt"));

    let thumbnail = direct_mobile_request(
        direct_port,
        "GET",
        "/mobile/artifact?session_id=s1&kind=screenshots&name=shot.png&offset=0&limit=3&max_dim=128",
        "platform-device-token",
        b"",
    );
    let thumbnail = String::from_utf8(thumbnail).unwrap();
    assert!(
        thumbnail.starts_with("HTTP/1.1 200"),
        "thumbnail: {thumbnail}"
    );
    assert!(thumbnail.contains("\"dataBase64\":\"dGh1\""));
    let request = recv_platform_callback(&captured_rx, "artifact.thumbnail");
    assert!(request.contains("\"session_id\":\"s1\""));
    assert!(request.contains("\"max_dim\":\"128\""));
    #[cfg(target_os = "macos")]
    {
        let direct_bootstrap = direct_mobile_request(
            direct_port,
            "GET",
            "/mobile/bootstrap",
            "platform-device-token",
            b"",
        );
        let separator = direct_bootstrap
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("Direct bootstrap HTTP separator");
        assert!(
            String::from_utf8_lossy(&direct_bootstrap[..separator]).starts_with("HTTP/1.1 200"),
            "Direct bootstrap response: {}",
            String::from_utf8_lossy(&direct_bootstrap)
        );
        let body: serde_json::Value =
            serde_json::from_slice(&direct_bootstrap[separator + 4..]).unwrap();
        assert_eq!(
            body["workspaceSettings"]["experimentalSettings"]["computerUseAvailable"],
            true
        );
        assert_eq!(
            body["workspaceSettings"]["experimentalSettings"]["computerUseReady"],
            true
        );
    }
    let direct_color = direct_mobile_request(
        direct_port,
        "POST",
        "/mobile/project-organization",
        "platform-device-token",
        br#"{"projectID":"p1","colorID":"teal"}"#,
    );
    assert!(
        String::from_utf8_lossy(&direct_color).starts_with("HTTP/1.1 200"),
        "Direct folder-color response: {}",
        String::from_utf8_lossy(&direct_color)
    );
    let request = recv_platform_callback(&captured_rx, "overlay.project-color.set");
    assert!(request.contains("\"projectID\":\"p1\""));
    assert!(request.contains("\"colorID\":\"teal\""));

    let push = direct_mobile_request(
        direct_port,
        "POST",
        "/mobile/push-token",
        "platform-device-token",
        br#"{"apnsToken":"0011223344556677","environment":"production"}"#,
    );
    assert!(
        String::from_utf8_lossy(&push).starts_with("HTTP/1.1 200"),
        "push response: {}",
        String::from_utf8_lossy(&push)
    );
    let request = recv_platform_callback(&captured_rx, "push.register");
    assert!(request.contains("\"operation\":\"push.register\""));
    assert!(request.contains("\"deviceID\":\"phone-process\""));

    let relay = direct_mobile_request(
        direct_port,
        "GET",
        "/mobile/relay-credentials",
        "platform-device-token",
        b"",
    );
    let relay = String::from_utf8(relay).unwrap();
    assert!(relay.starts_with("HTTP/1.1 200"), "relay response: {relay}");
    assert!(relay.contains("rotated-token"));
    let request = recv_platform_callback(&captured_rx, "relay.credentials.recover");
    assert!(request.contains("\"operation\":\"relay.credentials.recover\""));
    assert!(request.contains("\"deviceID\":\"phone-process\""));
    unpeel_serve::local_gateway::revoke_device(&fixture.host_home, "phone-process")
        .expect("worker revokes paired Controller");
    let request = recv_platform_callback(&captured_rx, "mobile.e2e-key.reconcile");
    assert!(request.contains("\"action\":\"remove\""));
    assert!(request.contains("\"deviceID\":\"phone-process\""));
    callback_stop_tx.send(()).unwrap();
    callback_worker.join().unwrap();

    drop(registration);
    assert!(wait_until(Duration::from_secs(30), || {
        backend.bootstrap().ok().is_some_and(|bootstrap| {
            let capabilities_withdrawn =
                bootstrap
                    .snapshot
                    .host_protocol
                    .as_ref()
                    .is_some_and(|protocol| {
                        !protocol.supports("session.notify_when_done.set")
                            && !protocol.supports("push.register")
                            && !protocol.supports("relay.credentials.recover")
                    });
            let computer_withdrawn = !cfg!(target_os = "macos")
                || bootstrap
                    .snapshot
                    .workspace_settings
                    .as_ref()
                    .and_then(|settings| settings.experimental_settings.as_ref())
                    .is_some_and(|settings| {
                        settings.computer_use_available == Some(false)
                            && settings.computer_use_ready == Some(false)
                    });
            capabilities_withdrawn && computer_withdrawn
        })
    }));

    // A compound request must fail before ordinary Host fields mutate when
    // the platform registration is gone. This protects both the historical
    // adapter-free TUI contract and the app-relaunch window.
    let raw = fixture.service_connection();
    let compound = HostCall::new(
        "POST",
        "/mobile/session-organization",
        RequestSemantics::Effect,
    )
    .with_body(
        "application/json",
        br#"{"sessionID":"s1","title":"must not land","notifyWhenDone":false}"#.to_vec(),
    );
    let prepared = raw.prepare(compound).unwrap();
    let reply = raw.request(prepared, Duration::from_secs(5)).unwrap();
    assert_eq!(reply.status, 501);
    assert!(
        !fixture
            .host_home
            .join("app-sessions/s1/title.json")
            .exists(),
        "unsupported platform field partially applied the title"
    );
    let unknown = HostCall::new(
        "POST",
        "/mobile/session-organization",
        RequestSemantics::Effect,
    )
    .with_body(
        "application/json",
        br#"{"sessionID":"missing","notifyWhenDone":false}"#.to_vec(),
    );
    let prepared = raw.prepare(unknown).unwrap();
    let reply = raw.request(prepared, Duration::from_secs(5)).unwrap();
    assert_eq!(
        reply.status, 404,
        "Session resolution must precede capability rejection"
    );
    raw.disconnect();

    let error = backend
        .set_session_notify_when_done("s1", false)
        .expect_err("withdrawn capability must fail closed");
    assert_eq!(error.kind(), RemoteEffectFailureKind::NotApplied);

    backend.disconnect();
    service.stop();
    fixture.cleanup();
}

#[test]
fn required_host_service_fails_closed_when_the_workspace_worker_is_absent() {
    let fixture = Fixture::new("service-required");
    let backend = RemoteSessionBackend::new(Arc::new(fixture.service_connection()));

    let error = backend
        .bootstrap()
        .expect_err("strict Local transport must not construct a fallback Host");
    assert!(
        error
            .to_string()
            .contains("local Host service is unavailable"),
        "unexpected strict Local transport error: {error}"
    );
    assert_eq!(
        fixture.gateway_start_count(),
        0,
        "strict Local transport constructed the historical semantic gateway"
    );

    backend.disconnect();
    fixture.cleanup();
}

#[test]
fn session_metrics_read_rides_the_gateway_and_maps_host_status() {
    // The Controller-side `session.metrics.read` request over the real
    // gateway process: the fixture Session is exited, so the Host's shared
    // metrics route answers a semantic 409. Reaching it proves the capability
    // is advertised by the gateway's bootstrap, the read rides the same
    // generation-bound request path as every other proxied route, and a
    // correlated rejection keeps the connection callable (no re-bootstrap, no
    // replacement gateway process).
    let fixture = Fixture::new("metrics");
    let backend = RemoteSessionBackend::new(Arc::new(fixture.connection()));
    backend.bootstrap().unwrap();

    match backend.read_session_metrics("s1") {
        Err(RemoteSessionBackendError::HostStatus { status: 409, .. }) => {}
        other => panic!("expected a 409 exited-session metrics rejection, got {other:?}"),
    }
    assert!(!backend.needs_bootstrap());
    // The same accepted generation still serves later work.
    backend.mark_session_read("s1").unwrap();
    assert_eq!(fixture.gateway_start_count(), 1);

    backend.disconnect();
    fixture.cleanup();
}

#[test]
fn app_state_project_addition_surfaces_on_re_bootstrap() {
    // The Swift `.localWorkspace` Add Project verb writes the project into the
    // SCOPED home's app-state.json (local-against-home) and nudges a
    // re-bootstrap. Prove the gateway reflects that addition: bootstrap once,
    // append a project to the same home's app-state.json exactly as the Swift
    // side does, then bootstrap again on the SAME connection and see it.
    let fixture = Fixture::new("addproject");
    let backend = RemoteSessionBackend::new(Arc::new(fixture.connection()));

    let first = backend.bootstrap().unwrap();
    assert_eq!(first.snapshot.projects.len(), 1, "seed project only");
    assert!(
        !first
            .snapshot
            .projects
            .iter()
            .any(|p| p.id == "native-added"),
        "added project must not exist before the write"
    );

    // Rewrite app-state.json with the new project appended — the same
    // `projects` array the Swift local-against-home edit mutates.
    std::fs::write(
        fixture.host_home.join("app-state.json"),
        br#"{
          "projects":[
            {"id":"p1","name":"Project","path":"/tmp","sort_order":0},
            {"id":"native-added","name":"Added","path":"/tmp/added","sort_order":1}
          ],
          "presets":[],
          "pinned_sessions":{},
          "active_tabs":{}
        }"#,
    )
    .unwrap();

    let second = backend.bootstrap().unwrap();
    let added = second
        .snapshot
        .projects
        .iter()
        .find(|p| p.id == "native-added")
        .expect("added project surfaces on re-bootstrap");
    assert_eq!(added.name, "Added");
    assert_eq!(added.path, "/tmp/added");
    // No second gateway process was spawned for the re-bootstrap.
    assert_eq!(fixture.gateway_start_count(), 1);

    backend.disconnect();
    fixture.cleanup();
}

#[test]
fn fresh_workspace_home_comes_up_empty_and_disconnect_spawns_no_replacement() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = FixtureRoot(
        PathBuf::from("/tmp").join(format!("u-lg-{}-fresh-{nonce:x}", std::process::id())),
    );
    let root = &root.0;
    // The home itself does not exist yet: a never-started workspace must
    // still come up and serve an empty Session list.
    let host_home = root.join("brand-new-home");
    std::fs::create_dir_all(root).unwrap();
    assert!(!host_home.exists());

    let connection = Arc::new(
        LocalProcessConnection::local_gateway(
            Path::new(env!("CARGO_BIN_EXE_unpeel-host")),
            &host_home,
        )
        .unwrap(),
    );
    let backend = RemoteSessionBackend::new(Arc::clone(&connection) as Arc<dyn HostConnection>);
    let bootstrap = backend.bootstrap().unwrap();
    assert!(bootstrap.snapshot.sessions.is_empty());
    assert!(
        host_home.is_dir(),
        "gateway did not initialize the fresh home"
    );

    backend.disconnect();
    let prepared = connection.prepare(HostCall::new(
        "GET",
        "/mobile/bootstrap",
        RequestSemantics::ReadOnly,
    ));
    assert!(matches!(prepared, Err(HostConnectionError::Closed)));
    std::thread::sleep(Duration::from_millis(50));
    let starts = std::fs::read_to_string(host_home.join("remote").join("audit.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("ssh_stdio_start"))
        .count();
    assert_eq!(starts, 1, "closed connection spawned a replacement gateway");
}
