#![cfg(unix)]

//! The serve worker's orphan-host reaper (unpeel_core::session_host::
//! reap_orphan_session_hosts): a per-process `__session_host__` still running
//! after its session is filed is terminated, and a running one is left alone.
//!
//! One `#[test]` on purpose: it sets this process's own `UNPEEL_HOME` (the
//! reaper reads it), so a second test in the same binary would race the env.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn temp_home(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    PathBuf::from("/tmp").join(format!("u-reap-{label}-{}-{nonce:x}", std::process::id()))
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

fn write_launch(home: &Path, session_id: &str, command: &str) -> PathBuf {
    let session_dir = home.join("app-sessions").join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    let path = session_dir.join("launch.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "session": {
                "id": session_id, "project_id": "project", "label": command,
                "custom_title": false, "command": command, "created_at": 1
            },
            "cwd": "/tmp", "dark_mode": true, "hook_port": null,
            "mcp_enabled": false, "browser_mcp_enabled": false,
            "computer_mcp_enabled": false, "initial_cols": 80, "initial_rows": 24
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn spawn_host(home: &Path, launch: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg("__session_host__")
        .arg(launch)
        .env("UNPEEL_HOME", home)
        .env("HOME", home)
        .env("SHELL", "/bin/bash")
        .env("UNPEEL_PTY_CORE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn manifest(home: &Path, id: &str) -> Option<Value> {
    let raw = fs::read(home.join("app-sessions").join(id).join("manifest.json")).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn host_pid(home: &Path, id: &str) -> Option<u32> {
    manifest(home, id)?
        .get("host_pid")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[test]
fn reaps_a_filed_host_and_leaves_a_running_one() {
    let home = temp_home("mix");
    fs::create_dir_all(home.join("app-sessions")).unwrap();
    // The reaper reads UNPEEL_HOME from this process.
    std::env::set_var("UNPEEL_HOME", &home);
    std::env::set_var("UNPEEL_PTY_CORE", "0");

    let filed_launch = write_launch(&home, "filed", "sleep 600");
    let mut filed_host = spawn_host(&home, &filed_launch);
    let running_launch = write_launch(&home, "running", "sleep 600");
    let mut running_host = spawn_host(&home, &running_launch);

    // Both hosts record their host_pid and come up running.
    assert!(
        wait_until(Duration::from_secs(30), || host_pid(&home, "filed")
            .is_some()
            && host_pid(&home, "running").is_some()),
        "hosts never recorded host_pid: {:?} / {:?}",
        manifest(&home, "filed"),
        manifest(&home, "running")
    );
    let filed_pid = host_pid(&home, "filed").unwrap();
    let running_pid = host_pid(&home, "running").unwrap();
    assert!(
        alive(filed_pid) && alive(running_pid),
        "hosts should be alive"
    );

    // File the first session while its host keeps running: drop an
    // `archived.json` marker beside it. (The manifest state is left to the
    // live host — it heartbeats `running` continuously, so editing state here
    // would just be overwritten; the archived marker is the stable "filed"
    // signal the reaper also acts on, and the host never clears it.)
    fs::write(
        home.join("app-sessions")
            .join("filed")
            .join("archived.json"),
        b"{\"archived_at\":1}",
    )
    .unwrap();

    let reaped = unpeel_core::session_host::reap_orphan_session_hosts();

    assert!(
        reaped
            .iter()
            .any(|r| r.session_id == "filed" && r.host_pid == filed_pid),
        "the filed session's host was not reaped: {reaped:?}"
    );
    // The reaper SIGKILLed the host; reap the zombie so kill(pid, 0) stops
    // reporting the defunct entry as alive.
    let _ = filed_host.wait();
    assert!(
        wait_until(Duration::from_secs(10), || !alive(filed_pid)),
        "the filed host {filed_pid} is still alive after the reap"
    );
    // The running session is untouched: not reaped, still alive, still running.
    assert!(
        !reaped.iter().any(|r| r.session_id == "running"),
        "a running session must not be reaped: {reaped:?}"
    );
    assert!(alive(running_pid), "the running host must stay alive");
    assert_eq!(
        manifest(&home, "running")
            .and_then(|m| m.get("state").and_then(|s| s.as_str()).map(String::from)),
        Some("running".to_string())
    );

    // Cleanup: stop the running host and reap the (already-dead) filed child.
    unsafe {
        libc::kill(-(running_pid as i32), libc::SIGKILL);
        libc::kill(running_pid as i32, libc::SIGKILL);
    }
    let _ = running_host.wait();
    let _ = filed_host.wait();
    let _ = fs::remove_dir_all(&home);
}
