#![cfg(unix)]
//! Real-process proof of stdio MCP cancellation: while a `tools/call` blocks
//! in a wait loop, the reader thread still answers `ping`, and a
//! `notifications/cancelled` for the blocked request unwinds it without a
//! response, letting the server exit promptly on EOF.

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn temp_home() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Hosted-session control sockets have a short sockaddr_un ceiling on macOS.
    PathBuf::from("/tmp").join(format!(
        "uc-{}-{:x}",
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

fn recv_response(responses: &mpsc::Receiver<Value>, expect_id: u64) -> Value {
    let response = responses
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("timed out waiting for response id {expect_id}"));
    assert_eq!(
        response["id"], expect_id,
        "unexpected response order: {response}"
    );
    response
}

#[test]
fn cancelled_wait_unblocks_without_response_while_reader_stays_live() {
    let home = temp_home();
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
    let mut stdin = mcp.stdin.take().unwrap();
    let stdout = mcp.stdout.take().unwrap();
    let (sender, responses) = mpsc::channel::<Value>();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender
                .send(serde_json::from_str(line.trim()).unwrap())
                .is_err()
            {
                break;
            }
        }
    });
    let mut send = |message: Value| {
        writeln!(stdin, "{}", serde_json::to_string(&message).unwrap()).unwrap();
    };

    send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-03-26" }
    }));
    recv_response(&responses, 1);

    // Blocks in the wait loop far longer than this test runs.
    send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "sessions",
            "arguments": {
                "action": "wait_for_text",
                "session_id": caller_id,
                "text": "UNPEEL-CANCEL-NEVER-APPEARS",
                "timeout_ms": 60000
            }
        }
    }));
    // The reader must stay live while the worker blocks on request 2.
    send(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }));
    recv_response(&responses, 3);

    send(json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": 2, "reason": "user aborted" }
    }));
    send(json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }));
    recv_response(&responses, 4);

    // EOF: the worker must drain promptly because the wait was cancelled —
    // nowhere near the 60s wait timeout.
    drop(stdin);
    let exited = wait_until(Duration::from_secs(10), || {
        mcp.try_wait().ok().flatten().is_some()
    });
    if !exited {
        let _ = mcp.kill();
    }
    assert!(exited, "cancelled wait kept the MCP server alive past EOF");
    let status = mcp.wait().unwrap();
    assert!(status.success());
    reader.join().unwrap();

    // A cancelled request gets no response at all.
    let leftover: Vec<Value> = responses.try_iter().collect();
    assert!(
        leftover.iter().all(|response| response["id"] != 2),
        "cancelled request 2 was answered: {leftover:?}"
    );

    stop_and_reap(&home, caller_id, &mut caller);
    let _ = fs::remove_dir_all(home);
}
