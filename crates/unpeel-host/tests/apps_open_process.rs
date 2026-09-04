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
    let executable = bin.join("unpeel-design");
    fs::write(&executable, "#!/bin/bash\nexec /bin/sleep 300\n").unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    "unpeel-design"
}

#[test]
fn apps_open_fails_before_approval_and_never_creates_a_companion_session() {
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
        .env("UNPEEL_SESSION_ID", caller_id)
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
                    "app": "Unpeel Design",
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
    let tool_text = |response: &Value| {
        assert_eq!(response["result"]["isError"], true);
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let first_text = tool_text(&responses[0]);
    let second_text = tool_text(&responses[1]);
    for error in [&first_text, &second_text] {
        assert!(
            error.contains("Agents cannot create App Sessions"),
            "{error}"
        );
        assert!(error.contains("ask the user to open it first"), "{error}");
        assert!(!error.contains("companion_session_id"));
    }

    assert!(!home.join("app-state.json").exists());
    let session_ids = fs::read_dir(home.join("app-sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(session_ids, vec![caller_id]);
    stop_and_reap(&home, caller_id, &mut caller);
    let _ = fs::remove_dir_all(home);
}
