//! Integration tests: spin up a fake session host (manifest + output.bin +
//! a Unix socket server speaking the session_host.rs control protocol) and
//! run the real unpeel-attach binary against it.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const REPLAY_RESET: &[u8] = b"\x18\x1bc\x1b[3J\x1b[2J\x1b[H";

fn unique_temp_dir(prefix: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    // Use /tmp directly: macOS std::env::temp_dir() paths are long enough to
    // overflow the Unix socket path limit (SUN_LEN, ~104 bytes).
    let dir = PathBuf::from("/tmp").join(format!(
        "ca-test-{prefix}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_manifest(session_dir: &Path, state: &str, pid: Option<u32>) {
    let manifest = serde_json::json!({
        "session": {
            "id": "test-session",
            "project_id": "test-project",
            "label": "Test",
            "custom_title": false,
            "command": "claude",
            "created_at": 1,
            "tag_id": null,
            "worktree_path": null,
            "worktree_branch": null
        },
        "cwd": "/tmp",
        "state": state,
        "pid": pid,
        "exit_code": null,
        "heartbeat_at": 1,
        "updated_at": 1
    });
    fs::write(
        session_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

/// Fake host control-socket server: per connection, read one JSON line,
/// record it, reply with `{"ok":true,"error":null}` + newline — exactly the
/// session_host.rs contract.
fn spawn_fake_socket_server(session_dir: &Path) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(session_dir.join("session.sock")).unwrap();
    let received_for_thread = Arc::clone(&received);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let received = Arc::clone(&received_for_thread);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    return;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                    received.lock().unwrap().push(value);
                }
                let mut stream = stream;
                let _ = stream.write_all(b"{\"ok\":true,\"error\":null}\n");
            });
        }
    });
    received
}

fn spawn_delayed_fake_socket_server(
    session_dir: &Path,
    delay: Duration,
) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let socket_path = session_dir.join("session.sock");
    let received_for_thread = Arc::clone(&received);
    thread::spawn(move || {
        thread::sleep(delay);
        let listener = UnixListener::bind(socket_path).unwrap();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let received = Arc::clone(&received_for_thread);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    return;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                    received.lock().unwrap().push(value);
                }
                let mut stream = stream;
                let _ = stream.write_all(b"{\"ok\":true,\"error\":null}\n");
            });
        }
    });
    received
}

fn output_stream_frame(data: &[u8], next_offset: u64, exited: bool) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&(data.len() as u32).to_be_bytes());
    frame.extend_from_slice(&next_offset.to_be_bytes());
    frame.push(if exited { 0b11 } else { 0b10 });
    frame.extend_from_slice(data);
    frame
}

fn spawn_fake_socket_server_with_stream(
    session_dir: &Path,
    stream_data: &'static [u8],
) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(session_dir.join("session.sock")).unwrap();
    let received_for_thread = Arc::clone(&received);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let received = Arc::clone(&received_for_thread);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    return;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    return;
                };
                received.lock().unwrap().push(value.clone());
                let mut stream = stream;
                if value["type"] == "stream_output" {
                    let offset = value["offset"].as_u64().unwrap_or(0);
                    let frame =
                        output_stream_frame(stream_data, offset + stream_data.len() as u64, false);
                    let _ = stream.write_all(&frame);
                } else {
                    let _ = stream.write_all(b"{\"ok\":true,\"error\":null}\n");
                }
            });
        }
    });
    received
}

/// A snapshot-capable fake Host: answers `snapshot` with a header line and
/// raw VT bytes at `journal_offset`, `stream_output` with one live frame
/// continuing from the requested offset, everything else with ok.
fn spawn_fake_socket_server_with_snapshot(
    session_dir: &Path,
    snapshot: &'static [u8],
    journal_offset: u64,
    stream_data: &'static [u8],
) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(session_dir.join("session.sock")).unwrap();
    let received_for_thread = Arc::clone(&received);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let received = Arc::clone(&received_for_thread);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    return;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    return;
                };
                received.lock().unwrap().push(value.clone());
                let mut stream = stream;
                if value["type"] == "snapshot" {
                    let header = format!(
                        "{{\"ok\":true,\"snapshot\":{{\"journal_offset\":{journal_offset},\"cols\":80,\"rows\":24,\"bytes_len\":{}}}}}\n",
                        snapshot.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(snapshot);
                } else if value["type"] == "stream_output" {
                    let offset = value["offset"].as_u64().unwrap_or(0);
                    let frame =
                        output_stream_frame(stream_data, offset + stream_data.len() as u64, false);
                    let _ = stream.write_all(&frame);
                } else {
                    let _ = stream.write_all(b"{\"ok\":true,\"error\":null}\n");
                }
            });
        }
    });
    received
}

/// A pre-snapshot fake Host: an unknown command closes the connection
/// without a reply line (serde rejects the variant in session_host.rs).
fn spawn_fake_socket_server_rejecting_unknown(
    session_dir: &Path,
    stream_data: &'static [u8],
) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(session_dir.join("session.sock")).unwrap();
    let received_for_thread = Arc::clone(&received);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let received = Arc::clone(&received_for_thread);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    return;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    return;
                };
                received.lock().unwrap().push(value.clone());
                let mut stream = stream;
                match value["type"].as_str() {
                    Some("stream_output") => {
                        let offset = value["offset"].as_u64().unwrap_or(0);
                        let frame = output_stream_frame(
                            stream_data,
                            offset + stream_data.len() as u64,
                            false,
                        );
                        let _ = stream.write_all(&frame);
                    }
                    Some("write") | Some("resize") | Some("ping") | Some("stream_input") => {
                        let _ = stream.write_all(b"{\"ok\":true,\"error\":null}\n");
                    }
                    _ => {} // unknown: close silently
                }
            });
        }
    });
    received
}

fn spawn_fake_socket_server_with_input_stream(
    session_dir: &Path,
) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let received: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(session_dir.join("session.sock")).unwrap();
    let received_for_thread = Arc::clone(&received);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let received = Arc::clone(&received_for_thread);
            thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    return;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    return;
                };
                received.lock().unwrap().push(value.clone());
                let mut stream = stream;
                if value["type"] == "stream_input" {
                    let _ = stream.write_all(&[0]);
                    loop {
                        let mut header = [0u8; 4];
                        if reader.read_exact(&mut header).is_err() {
                            break;
                        }
                        let len = u32::from_be_bytes(header) as usize;
                        let mut data = vec![0u8; len];
                        if reader.read_exact(&mut data).is_err() {
                            break;
                        }
                        received.lock().unwrap().push(serde_json::json!({
                            "type": "input_stream_frame",
                            "data": String::from_utf8_lossy(&data).to_string()
                        }));
                    }
                } else {
                    let _ = stream.write_all(b"{\"ok\":true,\"error\":null}\n");
                }
            });
        }
    });
    received
}

struct AttachProcess {
    child: Child,
    stdout_bytes: Arc<Mutex<Vec<u8>>>,
}

fn spawn_attach(sessions_dir: &Path, replay_bytes: u64, mute_input_ms: u64) -> AttachProcess {
    spawn_attach_with_args(sessions_dir, replay_bytes, mute_input_ms, &[])
}

fn spawn_attach_with_args(
    sessions_dir: &Path,
    replay_bytes: u64,
    mute_input_ms: u64,
    extra_args: &[&str],
) -> AttachProcess {
    let mut child = Command::new(env!("CARGO_BIN_EXE_unpeel-attach"))
        .arg("test-session")
        .arg("--sessions-dir")
        .arg(sessions_dir)
        .arg("--replay-bytes")
        .arg(replay_bytes.to_string())
        .arg("--mute-input-ms")
        .arg(mute_input_ms.to_string())
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();

    let stdout_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let mut stdout = child.stdout.take().unwrap();
    let sink = Arc::clone(&stdout_bytes);
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.lock().unwrap().extend_from_slice(&buf[..n]),
            }
        }
    });

    AttachProcess {
        child,
        stdout_bytes,
    }
}

fn wait_for<F: Fn() -> bool>(timeout: Duration, condition: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Reap a child within a hard bound so a hung attach becomes a fast, clearly
/// labelled test failure instead of a stalled CI job. The Linux
/// `unpeel-attach` job twice sat >1h on a single attach that never exited
/// (the whole `cargo test` step blocked, not one test), so every
/// `child.wait()` in this file goes through here: on timeout the child is
/// killed and the test panics rather than hanging forever.
#[track_caller]
fn wait_bounded(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("attach did not exit within 30s — hang guard tripped");
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("failed to wait for attach: {error}"),
        }
    }
}

#[test]
fn attach_waits_for_manifest_created_during_startup() {
    let sessions_dir = unique_temp_dir("delayed-manifest");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    let _received = spawn_fake_socket_server(&session_dir);
    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);

    thread::sleep(Duration::from_millis(100));
    let log = b"delayed startup replay\r\n";
    fs::write(session_dir.join("output.bin"), log).unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));

    let mut expected = REPLAY_RESET.to_vec();
    expected.extend_from_slice(log);
    assert!(
        wait_for(Duration::from_secs(5), || {
            *attach.stdout_bytes.lock().unwrap() == expected
        }),
        "delayed manifest replay mismatch: {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );

    write_manifest(&session_dir, "exited", None);
    wait_bounded(&mut attach.child);
    let _ = fs::remove_dir_all(&sessions_dir);
}

/// A full-screen mouse app's modes (alt screen, mouse tracking, bracketed
/// paste) are negotiated before the replayed tail, and the replay reset
/// wipes them — the manifest's `terminal_modes` must be re-asserted between
/// reset and replay or the reattached app renders but never receives
/// wheel/click reports again.
#[test]
fn replay_reasserts_terminal_modes_from_manifest() {
    let sessions_dir = unique_temp_dir("mode-restore");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    let log = b"\x1b[2J\x1b[Happ screen\r\n";
    fs::write(session_dir.join("output.bin"), log).unwrap();
    let mut manifest = serde_json::json!({
        "session": {
            "id": "test-session",
            "project_id": "test-project",
            "label": "Test",
            "custom_title": false,
            "command": "terminal-browser",
            "created_at": 1,
            "tag_id": null,
            "worktree_path": null,
            "worktree_branch": null
        },
        "cwd": "/tmp",
        "state": "exited",
        "pid": null,
        "exit_code": null,
        "heartbeat_at": 1,
        "updated_at": 1
    });
    manifest["terminal_modes"] = serde_json::json!({
        "alt_screen": true,
        "set": [1002, 1006, 2004],
        "reset": [25]
    });
    fs::write(
        session_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    wait_bounded(&mut attach.child);

    let mut expected = REPLAY_RESET.to_vec();
    expected.extend_from_slice(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?25l");
    expected.extend_from_slice(log);
    assert_eq!(
        *attach.stdout_bytes.lock().unwrap(),
        expected,
        "mode preamble must sit between reset and replay: {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );
    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn full_attach_lifecycle_against_fake_host() {
    let sessions_dir = unique_temp_dir("lifecycle");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    // Output log whose tail cut lands mid-escape-sequence: with
    // --replay-bytes sized to start inside "\x1b[31m", alignment must pull
    // the replay start back to the ESC byte.
    let log: &[u8] = b"old line one\r\nprefix \x1b[31mRED\x1b[0m tail\r\n";
    let escape_index = log.iter().position(|b| *b == 0x1b).unwrap();
    // Desired tail start = escape_index + 2 (inside the CSI sequence).
    let replay_bytes = (log.len() - (escape_index + 2)) as u64;
    fs::write(session_dir.join("output.bin"), log).unwrap();

    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, replay_bytes, 0);

    // 1. Replay tail correctness: starts at the ESC byte, not inside it,
    //    preceded by the screen/scrollback wipe that hides any preamble the
    //    spawning terminal printed (e.g. login(1) banners).
    let mut expected_replay = REPLAY_RESET.to_vec();
    expected_replay.extend_from_slice(&log[escape_index..]);
    assert!(
        wait_for(Duration::from_secs(5), || {
            *attach.stdout_bytes.lock().unwrap() == expected_replay
        }),
        "replay tail mismatch: got {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );

    // 2. stdin → write-command framing.
    let mut stdin = attach.child.stdin.take().unwrap();
    stdin.write_all(b"hello").unwrap();
    stdin.flush().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || {
            received
                .lock()
                .unwrap()
                .iter()
                .any(|cmd| cmd["type"] == "write" && cmd["data"] == "hello")
        }),
        "fake host never received write command; got {:?}",
        received.lock().unwrap()
    );

    // 3. Live-append following.
    let mut output = OpenOptions::new()
        .append(true)
        .open(session_dir.join("output.bin"))
        .unwrap();
    output.write_all(b"LIVE-APPEND").unwrap();
    output.flush().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes
                .windows(b"LIVE-APPEND".len())
                .any(|w| w == b"LIVE-APPEND")
        }),
        "live append never reached stdout"
    );

    // 4. Clean exit when the manifest flips to exited.
    write_manifest(&session_dir, "exited", None);
    let status = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = attach.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "attach did not exit after host death"
            );
            thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(status.success(), "expected exit 0, got {status:?}");

    let bytes = attach.stdout_bytes.lock().unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("[session ended]"),
        "missing session-ended banner in: {text:?}"
    );

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn live_output_prefers_socket_stream_over_output_file_following() {
    let sessions_dir = unique_temp_dir("stream-output");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    let replay = b"prompt> ";
    fs::write(session_dir.join("output.bin"), replay).unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server_with_stream(&session_dir, b"STREAM-LIVE");

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);

    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes
                .windows(b"STREAM-LIVE".len())
                .any(|w| w == b"STREAM-LIVE")
        }),
        "streamed live output never reached stdout: {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );
    assert!(
        received
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| { cmd["type"] == "stream_output" && cmd["offset"] == replay.len() as u64 }),
        "attach did not subscribe to output stream: {:?}",
        received.lock().unwrap()
    );

    write_manifest(&session_dir, "exited", None);
    wait_bounded(&mut attach.child);
    let _ = fs::remove_dir_all(&sessions_dir);
}

/// Focus reports are stripped for workloads that never enabled focus
/// reporting, but forwarded once the manifest's terminal_modes says DEC
/// 1004 is active — apps like terminal-browser stay in a 4fps background
/// throttle until their focus-in arrives.
#[test]
fn focus_reports_forwarded_when_workload_enabled_focus_reporting() {
    let sessions_dir = unique_temp_dir("focus-forward");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    let mut manifest = serde_json::json!({
        "session": {
            "id": "test-session",
            "project_id": "test-project",
            "label": "Test",
            "custom_title": false,
            "command": "terminal-browser",
            "created_at": 1,
            "tag_id": null,
            "worktree_path": null,
            "worktree_branch": null
        },
        "cwd": "/tmp",
        "state": "running",
        "pid": std::process::id(),
        "exit_code": null,
        "heartbeat_at": 1,
        "updated_at": 1
    });
    manifest["terminal_modes"] = serde_json::json!({
        "alt_screen": true,
        "set": [1003, 1004, 1006],
        "reset": []
    });
    fs::write(
        session_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let received = spawn_fake_socket_server_with_input_stream(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));

    let mut stdin = attach.child.stdin.take().unwrap();
    stdin.write_all(b"\x1b[Iafter").unwrap();
    stdin.flush().unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || received
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| {
                cmd["type"] == "input_stream_frame"
                    && cmd["data"].as_str().is_some_and(|d| d.contains("\u{1b}[I"))
            })),
        "focus-in was stripped despite 1004 being active: {:?}",
        received.lock().unwrap()
    );

    drop(stdin);
    write_manifest(&session_dir, "exited", None);
    wait_bounded(&mut attach.child);
    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn stdin_prefers_persistent_input_stream_over_write_commands() {
    let sessions_dir = unique_temp_dir("input-stream");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server_with_input_stream(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));

    let mut stdin = attach.child.stdin.take().unwrap();
    stdin.write_all(b"\x1b[<64;10;20M").unwrap();
    stdin.flush().unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || received
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| {
                cmd["type"] == "input_stream_frame" && cmd["data"] == "\u{1b}[<64;10;20M"
            })),
        "stdin bytes were not sent over input stream: {:?}",
        received.lock().unwrap()
    );
    assert!(
        received
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| cmd["type"] == "stream_input"),
        "attach did not open stream_input: {:?}",
        received.lock().unwrap()
    );

    drop(stdin);
    write_manifest(&session_dir, "exited", None);
    wait_bounded(&mut attach.child);
    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn follows_output_file_created_after_attach_starts() {
    let sessions_dir = unique_temp_dir("late-output");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    write_manifest(&session_dir, "running", Some(std::process::id()));
    let _received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    assert!(
        wait_for(Duration::from_secs(5), || {
            *attach.stdout_bytes.lock().unwrap() == REPLAY_RESET
        }),
        "attach did not start cleanly without output.bin: {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );

    fs::write(session_dir.join("output.bin"), b"created later\r\n").unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes
                .windows(b"created later".len())
                .any(|w| w == b"created later")
        }),
        "late-created output.bin was not followed"
    );

    write_manifest(&session_dir, "exited", None);
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn transient_bad_manifest_does_not_end_live_attach_when_socket_pings() {
    let sessions_dir = unique_temp_dir("bad-manifest");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"ready\r\n").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let _received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes.windows(b"ready".len()).any(|w| w == b"ready")
        }),
        "initial replay missing"
    );

    fs::write(session_dir.join("manifest.json"), b"{ this is not json").unwrap();
    thread::sleep(Duration::from_millis(1_300));
    assert!(
        attach.child.try_wait().unwrap().is_none(),
        "attach exited on a transient malformed manifest even though socket ping works"
    );

    let mut output = OpenOptions::new()
        .append(true)
        .open(session_dir.join("output.bin"))
        .unwrap();
    output
        .write_all(b"still alive after bad manifest\r\n")
        .unwrap();
    output.flush().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes
                .windows(b"still alive".len())
                .any(|w| w == b"still alive")
        }),
        "attach stopped following output after malformed manifest"
    );

    write_manifest(&session_dir, "exited", None);
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn final_output_is_drained_when_manifest_flips_to_exited() {
    let sessions_dir = unique_temp_dir("final-drain");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"before\r\n").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let _received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    let _stdin = attach.child.stdin.take().unwrap();
    assert!(wait_for(Duration::from_secs(5), || {
        let bytes = attach.stdout_bytes.lock().unwrap();
        bytes.windows(b"before".len()).any(|w| w == b"before")
    }));

    let mut output = OpenOptions::new()
        .append(true)
        .open(session_dir.join("output.bin"))
        .unwrap();
    output.write_all(b"final bytes before exit\r\n").unwrap();
    output.flush().unwrap();
    write_manifest(&session_dir, "exited", None);

    let status = wait_bounded(&mut attach.child);
    assert!(status.success());
    let bytes = attach.stdout_bytes.lock().unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("final bytes before exit"),
        "final bytes were not drained before attach exit: {text:?}"
    );
    assert!(
        text.contains("[session ended]"),
        "exit banner missing after final drain: {text:?}"
    );

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn large_live_appends_are_fully_drained() {
    let sessions_dir = unique_temp_dir("large-append");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"start\r\n").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let _received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    assert!(wait_for(Duration::from_secs(5), || {
        let bytes = attach.stdout_bytes.lock().unwrap();
        bytes.windows(b"start".len()).any(|w| w == b"start")
    }));

    let mut payload = vec![b'x'; 96 * 1024];
    payload.extend_from_slice(b"END-OF-LARGE-APPEND\r\n");
    let mut output = OpenOptions::new()
        .append(true)
        .open(session_dir.join("output.bin"))
        .unwrap();
    output.write_all(&payload).unwrap();
    output.flush().unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes
                .windows(b"END-OF-LARGE-APPEND".len())
                .any(|w| w == b"END-OF-LARGE-APPEND")
        }),
        "attach did not drain a live append larger than its read buffer"
    );

    write_manifest(&session_dir, "exited", None);
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn exited_session_prints_replay_tail_and_exits_zero() {
    let sessions_dir = unique_temp_dir("exited");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    let log = b"final output of a finished session\r\n";
    fs::write(session_dir.join("output.bin"), log).unwrap();
    write_manifest(&session_dir, "exited", None);

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let mut expected = REPLAY_RESET.to_vec();
    expected.extend_from_slice(log);
    assert!(
        wait_for(Duration::from_secs(2), || {
            *attach.stdout_bytes.lock().unwrap() == expected
        }),
        "exited replay mismatch: {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn split_utf8_input_is_forwarded_intact() {
    let sessions_dir = unique_temp_dir("split-utf8");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024, 0);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));

    let mut stdin = attach.child.stdin.take().unwrap();
    let text = "pre-é-post";
    let bytes = text.as_bytes();
    let split = bytes.iter().position(|b| *b == 0xc3).unwrap() + 1;
    stdin.write_all(&bytes[..split]).unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));
    stdin.write_all(&bytes[split..]).unwrap();
    stdin.flush().unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || received_write_data(&received)
            == text),
        "split UTF-8 input was not forwarded intact: {:?}",
        received_write_data(&received)
    );

    drop(stdin);
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn mute_window_drops_query_responses_but_passes_keystrokes() {
    let sessions_dir = unique_temp_dir("mute");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server(&session_dir);

    // Long mute window so the whole test runs inside it.
    let mut attach = spawn_attach(&sessions_dir, 1024, 5_000);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));

    // A DA response plus a CPR response (what a fresh terminal would answer
    // to replayed queries), interleaved with real typed characters.
    let mut stdin = attach.child.stdin.take().unwrap();
    stdin.write_all(b"\x1b[?62;22cab\x1b[24;80Rcd").unwrap();
    stdin.flush().unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || {
            received
                .lock()
                .unwrap()
                .iter()
                .any(|cmd| cmd["type"] == "write" && cmd["data"] == "abcd")
        }),
        "expected muted write 'abcd'; got {:?}",
        received.lock().unwrap()
    );

    // No write command should ever contain an escape byte.
    for cmd in received.lock().unwrap().iter() {
        if cmd["type"] == "write" {
            assert!(
                !cmd["data"].as_str().unwrap().contains('\u{1b}'),
                "escape leaked through mute window: {cmd:?}"
            );
        }
    }

    drop(stdin); // EOF → attach exits.
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn attach_ready_waits_until_replay_response_filter_expires() {
    let sessions_dir = unique_temp_dir("ready-after-mute");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let _received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024, 400);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));
    assert!(
        !session_dir.join(".attach-ready").exists(),
        "attach released the provider while terminal responses were muted"
    );
    assert!(
        wait_for(Duration::from_secs(2), || session_dir
            .join(".attach-ready")
            .exists()),
        "attach did not release the provider after terminal response filtering ended"
    );

    drop(attach.child.stdin.take());
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

/// Concatenated data of all write commands the fake host has received so far.
fn received_write_data(received: &Arc<Mutex<Vec<serde_json::Value>>>) -> String {
    received
        .lock()
        .unwrap()
        .iter()
        .filter(|cmd| cmd["type"] == "write")
        .filter_map(|cmd| cmd["data"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn focus_events_are_dropped_outside_the_mute_window() {
    let sessions_dir = unique_temp_dir("focus");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server(&session_dir);

    // Mute window of zero: focus filtering must work on its own, at any time.
    let mut attach = spawn_attach(&sessions_dir, 1024, 0);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));

    let mut stdin = attach.child.stdin.take().unwrap();

    // Mixed buffer: keystrokes, focus in/out, an arrow key, and a
    // parameterized CSI ending in I (cursor-forward-tab) interleaved.
    stdin
        .write_all(b"ab\x1b[Icd\x1b[A\x1b[Oef\x1b[2Ig")
        .unwrap();
    stdin.flush().unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || {
            received_write_data(&received) == "abcd\x1b[Aef\x1b[2Ig"
        }),
        "unexpected forwarded input: {:?}",
        received_write_data(&received)
    );

    // Focus events split across separate reads: ESC | [I and ESC[ | O. The
    // sleeps ensure each fragment is consumed as its own read(2) chunk.
    stdin.write_all(b"\x1b").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));
    stdin.write_all(b"[I").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));
    stdin.write_all(b"\x1b[").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));
    stdin.write_all(b"Oz").unwrap();
    stdin.flush().unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || {
            received_write_data(&received) == "abcd\x1b[Aef\x1b[2Igz"
        }),
        "split focus events leaked: {:?}",
        received_write_data(&received)
    );

    drop(stdin); // EOF → attach exits.
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn forward_focus_events_flag_disables_the_filter() {
    let sessions_dir = unique_temp_dir("focus-fwd");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach_with_args(&sessions_dir, 1024, 0, &["--forward-focus-events"]);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));

    let mut stdin = attach.child.stdin.take().unwrap();
    stdin.write_all(b"a\x1b[Ib\x1b[Oc").unwrap();
    stdin.flush().unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || {
            received_write_data(&received) == "a\x1b[Ib\x1b[Oc"
        }),
        "focus events should pass through with --forward-focus-events: {:?}",
        received_write_data(&received)
    );

    drop(stdin);
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

fn get_termios(fd: libc::c_int) -> libc::termios {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::tcgetattr(fd, &mut termios) },
        0,
        "tcgetattr failed"
    );
    termios
}

#[test]
fn startup_resize_waits_for_session_socket_after_preliminary_manifest() {
    use std::os::unix::io::FromRawFd;

    let sessions_dir = unique_temp_dir("delayed-socket-resize");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    // This mirrors the real host startup race: the preliminary manifest is
    // visible before session.sock exists. Attach must keep retrying the
    // startup resize until the socket is ready, otherwise Codex can stay at
    // the guessed launch grid until the user manually resizes the window.
    write_manifest(&session_dir, "running", Some(std::process::id()));
    fs::write(session_dir.join("output.bin"), b"").unwrap();
    let received = spawn_delayed_fake_socket_server(&session_dir, Duration::from_millis(300));

    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut winsize = libc::winsize {
        ws_row: 33,
        ws_col: 101,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            // `*const winsize` on Linux, `*mut winsize` on macOS: infer the cast.
            &mut winsize as *mut libc::winsize as _,
        )
    };
    assert_eq!(rc, 0, "openpty failed");

    let child_stdin = unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
    let child_stdout = unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
    let mut child = Command::new(env!("CARGO_BIN_EXE_unpeel-attach"))
        .arg("test-session")
        .arg("--sessions-dir")
        .arg(&sessions_dir)
        .arg("--mute-input-ms")
        .arg("0")
        .stdin(child_stdin)
        .stdout(child_stdout)
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    unsafe { libc::close(slave) };

    let drain_master = master;
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(drain_master, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
    });

    assert!(
        wait_for(Duration::from_secs(5), || {
            received
                .lock()
                .unwrap()
                .iter()
                .any(|cmd| cmd["type"] == "resize" && cmd["cols"] == 101 && cmd["rows"] == 33)
        }),
        "startup resize was not retried after socket bind; got {:?}",
        received.lock().unwrap()
    );
    assert!(
        wait_for(Duration::from_secs(5), || session_dir
            .join(".attach-ready")
            .exists()),
        "attach did not publish its ready marker"
    );

    write_manifest(&session_dir, "exited", None);
    let status = wait_bounded(&mut child);
    assert!(status.success(), "expected exit 0, got {status:?}");

    unsafe { libc::close(master) };
    let _ = fs::remove_dir_all(&sessions_dir);
}

/// Pins the user-visible arrow-key bug: spawned on a real PTY (as a Ghostty
/// surface does), attach must switch the PTY out of canonical/echo mode.
/// Before the fix, the kernel caret-echoed Up/Down as literal `^[[A`/`^[[B`
/// and line-buffered them until Enter, so they never reached the workload.
#[test]
fn arrow_keys_reach_the_host_intact_on_a_real_pty() {
    use std::os::unix::io::FromRawFd;

    let sessions_dir = unique_temp_dir("pty-arrows");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"prompt> ").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server(&session_dir);

    // A PTY pair with untouched default termios — exactly what a fresh
    // terminal surface hands the command it spawns.
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    assert_ne!(
        get_termios(slave).c_lflag & libc::ICANON,
        0,
        "fresh pty must start canonical for this test to mean anything"
    );

    // Child gets its own dup of the slave as stdin; we keep ours to inspect
    // termios. Stdout stays piped so replay does not loop back into the PTY.
    let child_stdin = unsafe { Stdio::from_raw_fd(libc::dup(slave)) };
    let mut child = Command::new(env!("CARGO_BIN_EXE_unpeel-attach"))
        .arg("test-session")
        .arg("--sessions-dir")
        .arg(&sessions_dir)
        .arg("--mute-input-ms")
        .arg("0")
        .stdin(child_stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut child_stdout = child.stdout.take().unwrap();
    thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = child_stdout.read_to_end(&mut sink);
    });

    // Attach must put the PTY into raw mode shortly after starting.
    assert!(
        wait_for(Duration::from_secs(5), || {
            get_termios(slave).c_lflag & (libc::ICANON | libc::ECHO) == 0
        }),
        "attach never switched its PTY into raw mode"
    );

    // Normal (CSI) Up arrow must arrive at the host as a single intact
    // write command — not caret-echoed, not buffered, not split.
    assert_eq!(
        unsafe { libc::write(master, b"\x1b[A".as_ptr().cast(), 3) },
        3
    );
    assert!(
        wait_for(Duration::from_secs(5), || {
            received
                .lock()
                .unwrap()
                .iter()
                .any(|cmd| cmd["type"] == "write" && cmd["data"] == "\u{1b}[A")
        }),
        "CSI Up arrow never reached the fake host intact; got {:?}",
        received.lock().unwrap()
    );

    // Application-cursor-mode (SS3) Up arrow must pass through too.
    assert_eq!(
        unsafe { libc::write(master, b"\x1bOA".as_ptr().cast(), 3) },
        3
    );
    assert!(
        wait_for(Duration::from_secs(5), || {
            received
                .lock()
                .unwrap()
                .iter()
                .any(|cmd| cmd["type"] == "write" && cmd["data"] == "\u{1b}OA")
        }),
        "SS3 Up arrow never reached the fake host intact; got {:?}",
        received.lock().unwrap()
    );

    // No fragment of an arrow may have been sent as its own command.
    for cmd in received.lock().unwrap().iter() {
        if cmd["type"] == "write" {
            let data = cmd["data"].as_str().unwrap();
            assert!(
                data == "\u{1b}[A" || data == "\u{1b}OA",
                "unexpected write fragment: {data:?}"
            );
        }
    }

    // Exit via host death; the original termios must be restored.
    write_manifest(&session_dir, "exited", None);
    let status = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "attach did not exit after host death"
            );
            thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(status.success(), "expected exit 0, got {status:?}");
    assert!(
        wait_for(Duration::from_secs(2), || {
            get_termios(slave).c_lflag & libc::ICANON != 0
        }),
        "attach did not restore the original termios on exit"
    );

    unsafe {
        libc::close(master);
        libc::close(slave);
    }
    let _ = fs::remove_dir_all(&sessions_dir);
}

#[test]
fn initial_resize_is_skipped_when_stdout_is_not_a_tty() {
    // With stdout piped (no tty), the attach client must not send a bogus
    // resize. The resize *encoding* itself is unit-tested in the lib; the
    // SIGWINCH→resize path needs a real PTY, which the fake-host harness
    // does not provide.
    let sessions_dir = unique_temp_dir("notty");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"x").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server(&session_dir);

    let mut attach = spawn_attach(&sessions_dir, 1024, 0);
    assert!(wait_for(Duration::from_secs(5), || {
        !attach.stdout_bytes.lock().unwrap().is_empty()
    }));
    thread::sleep(Duration::from_millis(100));
    assert!(
        received
            .lock()
            .unwrap()
            .iter()
            .all(|cmd| cmd["type"] != "resize"),
        "unexpected resize without a tty: {:?}",
        received.lock().unwrap()
    );

    drop(attach.child.stdin.take());
    let status = wait_bounded(&mut attach.child);
    assert!(status.success());

    let _ = fs::remove_dir_all(&sessions_dir);
}

/// Snapshot attach: the client applies the Host's VT snapshot instead of the
/// journal tail and streams from the snapshot's journal offset.
#[test]
fn snapshot_attach_replaces_tail_replay_and_streams_from_its_offset() {
    let sessions_dir = unique_temp_dir("snapshot-attach");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    // A journal tail that must NOT reach the screen when a snapshot exists.
    fs::write(session_dir.join("output.bin"), b"STALE-TAIL-BYTES").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server_with_snapshot(
        &session_dir,
        b"\x1b[?1049h\x1b[2;3HSNAPSHOT-FRAME\x1b[5;1H",
        4_242,
        b"LIVE-AFTER",
    );

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes.windows(10).any(|w| w == b"LIVE-AFTER")
        }),
        "live output never followed the snapshot: {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );
    let out = attach.stdout_bytes.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("SNAPSHOT-FRAME"),
        "snapshot bytes not written: {text:?}"
    );
    assert!(
        !text.contains("STALE-TAIL"),
        "journal tail replayed despite snapshot: {text:?}"
    );
    let reset_at = out
        .windows(2)
        .position(|w| w == b"\x1bc")
        .expect("reset emitted");
    let snap_at = text.find("SNAPSHOT-FRAME").unwrap();
    let live_at = text.find("LIVE-AFTER").unwrap();
    assert!(
        reset_at < snap_at && snap_at < live_at,
        "order: reset, snapshot, live"
    );
    assert!(
        received
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| { cmd["type"] == "stream_output" && cmd["offset"] == 4_242u64 }),
        "stream must start at the snapshot's journal offset: {:?}",
        received.lock().unwrap()
    );

    write_manifest(&session_dir, "exited", None);
    wait_bounded(&mut attach.child);
    let _ = fs::remove_dir_all(&sessions_dir);
}

/// A Host from before the snapshot command closes the connection without a
/// reply; the client falls back to the raw tail replay exactly as before.
#[test]
fn pre_snapshot_host_falls_back_to_tail_replay() {
    let sessions_dir = unique_temp_dir("snapshot-fallback");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    let tail = b"OLD-HOST-TAIL";
    fs::write(session_dir.join("output.bin"), tail).unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received = spawn_fake_socket_server_rejecting_unknown(&session_dir, b"LIVE-OLD");

    let mut attach = spawn_attach(&sessions_dir, 1024 * 1024, 0);
    assert!(
        wait_for(Duration::from_secs(5), || {
            let bytes = attach.stdout_bytes.lock().unwrap();
            bytes.windows(8).any(|w| w == b"LIVE-OLD")
        }),
        "live output never arrived on the fallback path: {:?}",
        String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap())
    );
    let text = String::from_utf8_lossy(&attach.stdout_bytes.lock().unwrap()).into_owned();
    assert!(
        text.contains("OLD-HOST-TAIL"),
        "tail replay missing: {text:?}"
    );
    let commands = received.lock().unwrap();
    assert!(
        commands.iter().any(|cmd| cmd["type"] == "snapshot"),
        "snapshot was requested"
    );
    assert!(
        commands
            .iter()
            .any(|cmd| cmd["type"] == "stream_output" && cmd["offset"] == tail.len() as u64),
        "stream must start at the tail offset: {commands:?}"
    );
    drop(commands);

    write_manifest(&session_dir, "exited", None);
    wait_bounded(&mut attach.child);
    let _ = fs::remove_dir_all(&sessions_dir);
}

/// `UNPEEL_ATTACH_SNAPSHOT=0` is the escape hatch back to raw tail replay
/// even against a snapshot-capable Host.
#[test]
fn snapshot_attach_escape_hatch_uses_tail_replay() {
    let sessions_dir = unique_temp_dir("snapshot-off");
    let session_dir = sessions_dir.join("test-session");
    fs::create_dir_all(&session_dir).unwrap();

    fs::write(session_dir.join("output.bin"), b"TAIL-WINS").unwrap();
    write_manifest(&session_dir, "running", Some(std::process::id()));
    let received =
        spawn_fake_socket_server_with_snapshot(&session_dir, b"SNAPSHOT-LOSES", 99, b"LIVE-OFF");

    let mut child = Command::new(env!("CARGO_BIN_EXE_unpeel-attach"))
        .arg("test-session")
        .arg("--sessions-dir")
        .arg(&sessions_dir)
        .arg("--mute-input-ms")
        .arg("0")
        .env("UNPEEL_ATTACH_SNAPSHOT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout_bytes: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let mut stdout = child.stdout.take().unwrap();
    let sink = Arc::clone(&stdout_bytes);
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.lock().unwrap().extend_from_slice(&buf[..n]),
            }
        }
    });
    assert!(wait_for(Duration::from_secs(5), || {
        stdout_bytes
            .lock()
            .unwrap()
            .windows(8)
            .any(|w| w == b"LIVE-OFF")
    }));
    let text = String::from_utf8_lossy(&stdout_bytes.lock().unwrap()).into_owned();
    assert!(
        text.contains("TAIL-WINS") && !text.contains("SNAPSHOT-LOSES"),
        "{text:?}"
    );
    assert!(
        !received
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| cmd["type"] == "snapshot"),
        "snapshot must not be requested with the escape hatch set"
    );

    write_manifest(&session_dir, "exited", None);
    wait_bounded(&mut child);
    let _ = fs::remove_dir_all(&sessions_dir);
}
