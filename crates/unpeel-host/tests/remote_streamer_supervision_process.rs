//! Real-process proof that the workspace worker supervises its own
//! `unpeel-host __remote__` TLS terminal streamer
//! (the private "remote-streamer-supervision" design record, deliverable 1).
//!
//! Case 1 runs this build's `unpeel-host __serve__` against an isolated
//! workspace with one paired device, kills the streamer named in
//! `remote.json`, and asserts a replacement appears, `serve.json` reports it
//! live, and `/mobile/bootstrap` advertises a WSS endpoint again — with no
//! service restart. Case 2 swaps in a shim `unpeel-host` whose `__remote__`
//! exits immediately and asserts the crash-loop ceiling holds until the
//! paired-device set changes.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            let stopped = wait_until(Duration::from_secs(60), || {
                self.child.try_wait().ok().flatten().is_some()
            });
            if !stopped {
                unsafe {
                    libc::kill(self.child.id() as libc::pid_t, libc::SIGKILL);
                }
                let _ = self.child.wait();
            }
        }
        // A streamer the worker was still supervising exits with it; a
        // stale one from a failed assertion must not outlive the test.
        if let Some((pid, _)) = remote_record(&self.home) {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        // The worker's detached PTY core survives the worker on purpose; a
        // fixture must ask it to exit or every run leaks one core.
        unpeel_core::pty_core::shutdown_cores_under(&self.home, Duration::from_secs(15));
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Condition waits here are bounded generously (60 s; 120 s for the crash
/// loop, which is five spawn/exit cycles plus backoff): a worker spawning its
/// TLS streamer takes ~2 s idle and many times that beside a workspace run.
/// A bound only costs time when the condition never holds; never a sleep as
/// synchronization.
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

/// One home per test, never shared. The name used to be pid + a nanosecond
/// timestamp — whose real resolution is microseconds — so two tests starting
/// on the same tick got the SAME directory: one worker refused with "an
/// Unpeel Host is already serving this workspace" and the other test read
/// its neighbour's serve.json (the "both tests fail together" signature under
/// load). A process-wide counter makes the name unique by construction, and
/// `create_dir` (not `create_dir_all`) proves it.
fn isolated_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("unpeel-streamer-supervision-{}", uuid_like()));
    std::fs::create_dir(&home)
        .unwrap_or_else(|e| panic!("isolated home {} must be fresh: {e}", home.display()));
    home
}

fn uuid_like() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// A port the worker will bind LATER (the fixture persists it as the paired
/// Direct endpoint, exact-or-nothing). A kernel-chosen `:0` port released
/// here can be handed to any other `:0` binder on the machine before the
/// worker gets to it (a workspace test run beside this one does that
/// constantly), and the worker then never publishes. Pick a random port
/// outside every OS ephemeral range instead (macOS 49152–65535, Linux
/// 32768–60999), proven free by binding it once.
fn reserve_port() -> u16 {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64).rotate_left(32);
    loop {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let port = 20_000 + (seed % 10_000) as u16;
        if TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return port;
        }
    }
}

fn sha256_hex(input: &str) -> String {
    unpeel_serve::pairing::sha256_hex(input)
}

fn write_pairing_fixture(home: &Path, port: u16, token: &str) {
    let mobile = home.join("mobile");
    std::fs::create_dir_all(&mobile).unwrap();
    std::fs::write(mobile.join("server-port"), format!("{port}\n")).unwrap();
    write_devices(home, &[("streamer-test-phone", token)]);
}

fn write_devices(home: &Path, devices: &[(&str, &str)]) {
    let devices: Vec<serde_json::Value> = devices
        .iter()
        .map(|(id, token)| {
            serde_json::json!({
                "id": id,
                "name": format!("Phone {id}"),
                "platform": "iOS",
                "tokenHash": sha256_hex(token),
                "pairedAtUnixMs": 1,
                "relayAllowed": false
            })
        })
        .collect();
    let store = serde_json::json!({ "version": 1, "devices": devices });
    let mobile = home.join("mobile");
    let path = mobile.join("devices.json");
    // Unique temp name (the worker's own atomic writes use unique names too)
    // and every failure names the OS error plus what the directory held —
    // a bare `rename(...).unwrap()` once failed in a solo staging run with
    // its cause swallowed.
    let temporary = mobile.join(format!(
        ".devices.json.{}.{}.tmp",
        std::process::id(),
        uuid_like()
    ));
    let describe = |what: &str, error: &std::io::Error| {
        let listing = std::fs::read_dir(&mobile)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|e| format!("<unreadable: {e}>"));
        format!(
            "{what} {} ({error}); {} holds [{listing}]",
            path.display(),
            mobile.display()
        )
    };
    std::fs::create_dir_all(&mobile)
        .unwrap_or_else(|e| panic!("{}", describe("create mobile dir for", &e)));
    std::fs::write(&temporary, serde_json::to_vec(&store).unwrap())
        .unwrap_or_else(|e| panic!("{}", describe("write temp for", &e)));
    std::fs::rename(&temporary, &path)
        .unwrap_or_else(|e| panic!("{}", describe("rename temp into", &e)));
}

fn spawn_serve(home: &Path, host_cmd: Option<&Path>) -> ServeProcess {
    let mut command = Command::new(env!("CARGO_BIN_EXE_unpeel-host"));
    command
        .arg("__serve__")
        // Test workers never download the Browser MCP or Computer Use engine.
        .env("UNPEEL_BROWSER_ENGINE_INSTALL", "0")
        .env("UNPEEL_COMPUTER_ENGINE_INSTALL", "0")
        .env("UNPEEL_HOME", home)
        .stdout(Stdio::null())
        // The worker's stderr is the only place a failed start explains
        // itself; keep it next to the home for the failure messages.
        .stderr(
            std::fs::File::create(home.join("serve.stderr"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        );
    match host_cmd {
        Some(path) => {
            command.env("UNPEEL_HOST_CMD", path);
        }
        None => {
            command.env_remove("UNPEEL_HOST_CMD");
        }
    }
    let child = command.spawn().expect("start unpeel-host __serve__");
    ServeProcess {
        child,
        home: home.to_path_buf(),
    }
}

fn serve_status(home: &Path) -> Option<serde_json::Value> {
    std::fs::read(home.join("serve.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
}

fn streamer_status(home: &Path) -> Option<serde_json::Value> {
    serve_status(home).and_then(|status| status.get("terminalStreamer").cloned())
}

fn streamer_state(home: &Path) -> Option<String> {
    streamer_status(home).and_then(|streamer| {
        streamer
            .get("state")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    })
}

fn streamer_pid(home: &Path) -> Option<u32> {
    streamer_status(home)
        .and_then(|streamer| streamer.get("pid").and_then(|v| v.as_u64()))
        .and_then(|pid| u32::try_from(pid).ok())
}

/// (pid, port) named by `remote.json`.
fn remote_record(home: &Path) -> Option<(u32, u16)> {
    let raw = std::fs::read(home.join("remote.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let pid = u32::try_from(value.get("pid")?.as_u64()?).ok()?;
    let port = u16::try_from(value.get("port")?.as_u64()?).ok()?;
    Some((pid, port))
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// The Direct port comes from the fixture before the worker has necessarily
/// bound it (and a respawned streamer/worker rebinds it): connect with a
/// bounded wait for the listener instead of an immediate `connect().unwrap()`
/// that once failed in a solo staging run.
fn bootstrap(home: &Path, port: u16, token: &str) -> (u16, serde_json::Value) {
    use unpeel_core::rustls;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last_error = None;
    let tcp = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() < deadline => {
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!(
                "nothing listening on 127.0.0.1:{port} within 60 s (last error: {error}; earlier: {last_error:?})"
            ),
        }
    };
    tcp.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    // The direct endpoint is TLS, pinned to the workspace's Host certificate
    // — the same file the worker's streamer serves.
    let fingerprint =
        unpeel_core::remote_server::ensure_tls_material_in(&home.join("remote").join("tls"))
            .expect("workspace Host certificate")
            .fingerprint;
    let config = Arc::new(unpeel_core::remote_attach::pinned_client_config(Some(
        fingerprint,
    )));
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let connection = rustls::ClientConnection::new(config, name).unwrap();
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
    (status, serde_json::from_str(body).unwrap())
}

fn advertised_remote_port(home: &Path, port: u16, token: &str) -> Option<u64> {
    let (status, body) = bootstrap(home, port, token);
    assert_eq!(status, 200, "bootstrap failed: {body}");
    body.get("remoteServerPort").and_then(|v| v.as_u64())
}

fn worker_stderr(home: &Path) -> String {
    std::fs::read_to_string(home.join("serve.stderr")).unwrap_or_default()
}

fn trace_log(home: &Path) -> String {
    std::fs::read_to_string(home.join("hooks").join("trace.log")).unwrap_or_default()
}

#[test]
fn worker_respawns_a_killed_streamer_without_a_service_restart() {
    let home = isolated_home();
    let port = reserve_port();
    let token = "streamer-supervision-controller-token";
    write_pairing_fixture(&home, port, token);
    let process = spawn_serve(&home, None);

    assert!(
        wait_until(Duration::from_secs(60), || {
            streamer_state(&home).as_deref() == Some("live")
                && remote_record(&home).is_some_and(|(pid, _)| Some(pid) == streamer_pid(&home))
        }),
        "worker never published a live streamer: serve.json={:?} remote.json={:?} stderr={}",
        serve_status(&home),
        remote_record(&home),
        worker_stderr(&home)
    );
    let (first_pid, first_port) = remote_record(&home).unwrap();
    assert!(process_alive(first_pid));
    assert_ne!(first_pid, process.child.id());
    assert!(
        wait_until(Duration::from_secs(60), || {
            advertised_remote_port(&home, port, token) == Some(u64::from(first_port))
        }),
        "bootstrap never advertised the first streamer"
    );

    // The 2026-09-01 failure: a dead streamer nobody respawned.
    unsafe {
        libc::kill(first_pid as libc::pid_t, libc::SIGKILL);
    }
    assert!(
        wait_until(Duration::from_secs(60), || !process_alive(first_pid)),
        "the streamer ignored SIGKILL"
    );
    assert!(
        wait_until(Duration::from_secs(60), || {
            streamer_state(&home).as_deref() == Some("live")
                && streamer_pid(&home).is_some_and(|pid| pid != first_pid)
                && remote_record(&home).is_some_and(|(pid, _)| Some(pid) == streamer_pid(&home))
        }),
        "worker never respawned the streamer: serve.json={:?} remote.json={:?} trace={}",
        serve_status(&home),
        remote_record(&home),
        trace_log(&home)
    );
    let (second_pid, second_port) = remote_record(&home).unwrap();
    assert!(process_alive(second_pid));
    assert_eq!(
        serve_status(&home).unwrap()["pid"].as_u64(),
        Some(u64::from(process.child.id())),
        "the worker itself must not have restarted"
    );
    let streamer = streamer_status(&home).unwrap();
    assert_eq!(streamer["restarts"].as_u64(), Some(1));
    // The port is copied from `remote.json` into `serve.json` on the tick
    // after the new streamer publishes it.
    assert!(
        wait_until(Duration::from_secs(60), || {
            streamer_status(&home).and_then(|streamer| streamer["port"].as_u64())
                == Some(u64::from(second_port))
        }),
        "serve.json never published the respawned streamer's port: {:?}",
        streamer_status(&home)
    );
    assert!(
        wait_until(Duration::from_secs(60), || {
            advertised_remote_port(&home, port, token) == Some(u64::from(second_port))
        }),
        "bootstrap never advertised the respawned streamer"
    );

    let trace = trace_log(&home);
    assert!(
        trace.contains("terminal streamer exited (signal 9"),
        "trace lacks the exit line: {trace}"
    );
    assert!(
        trace.contains("terminal streamer respawned (pid"),
        "trace lacks the respawn line: {trace}"
    );

    // Clean shutdown still takes the supervised streamer with it.
    unsafe {
        libc::kill(process.child.id() as libc::pid_t, libc::SIGTERM);
    }
    let mut process = process;
    assert!(
        wait_until(Duration::from_secs(60), || process
            .child
            .try_wait()
            .ok()
            .flatten()
            .is_some()),
        "serve did not stop after SIGTERM"
    );
    assert!(
        wait_until(Duration::from_secs(60), || !process_alive(second_pid)),
        "the supervised streamer outlived the worker"
    );
}

fn write_crashing_shim(home: &Path) -> (PathBuf, PathBuf) {
    let shim = home.join("crashing-unpeel-host.sh");
    let counter = home.join("remote-launches.log");
    let real = env!("CARGO_BIN_EXE_unpeel-host");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"__remote__\" ]; then\n  echo launch >> '{}'\n  exit 3\nfi\nexec '{real}' \"$@\"\n",
            counter.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    (shim, counter)
}

fn launch_count(counter: &Path) -> usize {
    std::fs::read_to_string(counter)
        .map(|raw| raw.lines().count())
        .unwrap_or(0)
}

#[test]
fn crash_looping_streamer_hits_the_ceiling_and_retries_on_pairing_change() {
    let home = isolated_home();
    let port = reserve_port();
    let token = "streamer-crash-loop-controller-token";
    write_pairing_fixture(&home, port, token);
    let (shim, counter) = write_crashing_shim(&home);
    let _process = spawn_serve(&home, Some(&shim));

    let ceiling = 5;
    assert!(
        wait_until(Duration::from_secs(120), || {
            streamer_state(&home).as_deref() == Some("gaveUp")
        }),
        "worker never gave up on the crash loop: serve.json={:?} launches={} trace={} stderr={}",
        serve_status(&home),
        launch_count(&counter),
        trace_log(&home),
        worker_stderr(&home)
    );
    let streamer = streamer_status(&home).unwrap();
    assert_eq!(streamer["rapidFailures"].as_u64(), Some(ceiling));
    assert_eq!(streamer["lastExit"].as_str(), Some("code 3"));
    assert!(streamer.get("pid").is_none());
    let launches_at_ceiling = launch_count(&counter);
    assert_eq!(launches_at_ceiling, ceiling as usize);

    // The hold is real: no further launches while nothing changes.
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(launch_count(&counter), launches_at_ceiling);
    assert_eq!(streamer_state(&home).as_deref(), Some("gaveUp"));
    assert!(
        advertised_remote_port(&home, port, token).is_none(),
        "a dead streamer must not be advertised"
    );
    assert!(trace_log(&home).contains("giving up until pairing changes"));

    // A fresh pairing is the user actively trying: retry once more.
    write_devices(
        &home,
        &[
            ("streamer-test-phone", token),
            ("streamer-second-phone", "second-token"),
        ],
    );
    assert!(
        wait_until(Duration::from_secs(60), || {
            launch_count(&counter) > launches_at_ceiling
        }),
        "pairing change did not lift the crash-loop hold: {:?}",
        serve_status(&home)
    );
}
