#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn temp_home() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Hosted-session control sockets have a short sockaddr_un ceiling on macOS.
    PathBuf::from("/tmp").join(format!(
        "uo-{}-{:x}",
        std::process::id(),
        nonce & 0xffff_ffff
    ))
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    condition()
}

fn write_caller_launch(home: &Path, session_id: &str) -> PathBuf {
    let session_dir = home.join("app-sessions").join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let path = session_dir.join("launch.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "session": {
                "id": session_id,
                "project_id": "project",
                "label": "Agent terminal",
                "custom_title": false,
                "command": "",
                "created_at": 1
            },
            "cwd": "/tmp",
            "dark_mode": true,
            "hook_port": null,
            "mcp_enabled": true,
            "browser_mcp_enabled": false,
            "computer_mcp_enabled": false,
            "initial_cols": 80,
            "initial_rows": 24
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn spawn_caller(home: &Path, launch: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg("__session_host__")
        .arg(launch)
        .env("UNPEEL_HOME", home)
        .env("HOME", home)
        .env("SHELL", "/bin/bash")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn socket_command(home: &Path, session_id: &str, value: Value) -> Value {
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .write_all(format!("{}\n", serde_json::to_string(&value).unwrap()).as_bytes())
        .unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    serde_json::from_str(response.trim()).unwrap()
}

fn stop_and_reap(home: &Path, session_id: &str, child: &mut Child) {
    if child.try_wait().unwrap().is_none() {
        let _ = socket_command(home, session_id, json!({ "type": "kill" }));
        let _ = wait_until(Duration::from_secs(4), || {
            child.try_wait().ok().flatten().is_some()
        });
    }
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn install_fixture_app(home: &Path) -> &'static str {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("unpeel-filetree");
    fs::write(&executable, "#!/bin/bash\nexec /bin/sleep 300\n").unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    "unpeel-filetree"
}

fn manifest_state(home: &Path, session_id: &str) -> Option<String> {
    let raw = fs::read(
        home.join("app-sessions")
            .join(session_id)
            .join("manifest.json"),
    )
    .ok()?;
    let manifest: Value = serde_json::from_slice(&raw).ok()?;
    manifest["state"].as_str().map(str::to_string)
}

/// An agent's `apps.open` is the one bounded exception to user-only Session
/// creation, and it asks nobody: with no app port and no bridge reachable at
/// all, the Host creates the project/resource instance, starts the companion
/// App Session, and binds the caller's panel. A replay with the same
/// `request_id` is an exact deduplication (no second companion), and the
/// receipt never leaks the companion Session id.
#[test]
fn apps_open_creates_the_companion_session_without_approval() {
    let home = temp_home();
    install_fixture_app(&home);

    let caller_id = "caller";
    let launch = write_caller_launch(&home, caller_id);
    let mut caller = spawn_caller(&home, &launch);
    let caller_socket = home.join("app-sessions/caller/session.sock");
    assert!(wait_until(Duration::from_secs(10), || caller_socket.exists()));

    let mut mcp = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg("__mcp__")
        .env("UNPEEL_HOME", &home)
        .env("HOME", &home)
        .env("SHELL", "/bin/bash")
        .env("UNPEEL_SESSION_ID", caller_id)
        // Keep the companion a plain per-process host: this home has no
        // workspace worker to adopt a PTY core.
        .env("UNPEEL_PTY_CORE", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let calls = [1, 2].map(|id| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "apps",
                "arguments": {
                    "action": "open",
                    "app": "Files",
                    "target": "panel",
                    "request_id": "open-1"
                }
            }
        })
    });
    {
        let mut stdin = mcp.stdin.take().unwrap();
        for call in calls {
            writeln!(stdin, "{}", serde_json::to_string(&call).unwrap()).unwrap();
        }
    }
    let output = mcp.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "MCP stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    let receipts = responses
        .iter()
        .map(|response| {
            assert_ne!(
                response["result"]["isError"], true,
                "open failed: {}",
                response["result"]["content"][0]["text"]
            );
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(!text.contains("companion_session_id"), "{text}");
            serde_json::from_str::<Value>(text).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts[0]["app"]["id"], "unpeel.app.filetree");
    assert!(matches!(
        receipts[0]["process_state"].as_str(),
        Some("running" | "starting")
    ));
    // Same request id → the exact same instance and binding, deduplicated
    // rather than revealed a second time.
    assert_eq!(receipts[0]["presentation"]["created_instance"], true);
    assert_eq!(receipts[1]["presentation"]["deduplicated_request"], true);
    assert_eq!(receipts[1]["presentation"]["created_instance"], false);
    assert_eq!(receipts[1]["presentation"]["reveal_requested"], false);
    for key in ["instance_id", "presentation_id", "reveal_revision"] {
        assert_eq!(
            receipts[0]["presentation"][key], receipts[1]["presentation"][key],
            "{key}"
        );
    }

    let state: Value =
        serde_json::from_slice(&fs::read(home.join("app-state.json")).unwrap()).unwrap();
    assert!(
        state.get("mcp_app_open_approvals").is_none(),
        "an App open records no grant"
    );
    let instances = state["app_presentations"]["instances"].as_array().unwrap();
    assert_eq!(instances.len(), 1, "one instance per project/resource");
    assert_eq!(
        state["app_presentations"]["presentations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let companion_id = instances[0]["companion_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(companion_id, caller_id);

    let mut session_ids = fs::read_dir(home.join("app-sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    session_ids.sort();
    let mut expected = vec![caller_id.to_string(), companion_id.clone()];
    expected.sort();
    assert_eq!(session_ids, expected);
    assert!(
        wait_until(Duration::from_secs(10), || {
            manifest_state(&home, &companion_id).as_deref() == Some("running")
        }),
        "companion manifest: {:?}",
        manifest_state(&home, &companion_id)
    );
    let companion_manifest: Value = serde_json::from_slice(
        &fs::read(
            home.join("app-sessions")
                .join(&companion_id)
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(companion_manifest["session"]["role"], "app-panel");
    assert_eq!(companion_manifest["session"]["spawned_by"], caller_id);

    let companion_socket = home
        .join("app-sessions")
        .join(&companion_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(10), || companion_socket.exists()));
    let _ = socket_command(&home, &companion_id, json!({ "type": "kill" }));
    let _ = wait_until(Duration::from_secs(5), || {
        manifest_state(&home, &companion_id).as_deref() == Some("exited")
    });
    stop_and_reap(&home, caller_id, &mut caller);
    let _ = fs::remove_dir_all(home);
}
