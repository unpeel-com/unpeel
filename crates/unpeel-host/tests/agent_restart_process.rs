#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn temp_home(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Keep the Unix socket below macOS's short sockaddr_un limit.
    PathBuf::from("/tmp").join(format!("u-ar-{label}-{}-{nonce:x}", std::process::id()))
}

/// Condition waits in this file are bounded at 30 s: the Host's login shell,
/// wrapper, and observer take ~1 s idle but several times that under a
/// parallel workspace run, and a bound only costs time when the condition
/// never holds. Never a sleep as synchronization.
fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
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
                "id": session_id,
                "project_id": "project",
                "label": if command.is_empty() { "Terminal" } else { command },
                "custom_title": false,
                "command": command,
                "created_at": 1
            },
            "cwd": "/tmp",
            "dark_mode": true,
            "hook_port": null,
            "mcp_enabled": true,
            "browser_mcp_enabled": true,
            "computer_mcp_enabled": true,
            "initial_cols": 80,
            "initial_rows": 24
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn spawn_host(home: &Path, launch: &Path, path: Option<&str>) -> HostGuard {
    let mut command = Command::new(env!("CARGO_BIN_EXE_unpeel-host"));
    command
        .arg("__session_host__")
        .arg(launch)
        .env("UNPEEL_HOME", home)
        .env("HOME", home)
        .env("SHELL", "/bin/bash")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    HostGuard {
        child: command.spawn().unwrap(),
    }
}

/// One control-socket round trip. Every failure names the command, the
/// session, and what the Host had on disk at that moment — a bare EOF on
/// `session.sock` (seen once on Linux CI) otherwise reads as an opaque
/// `unwrap` on an empty string.
fn socket_command(home: &Path, session_id: &str, value: Value) -> Value {
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    let describe = |what: &str| {
        let manifest = fs::read_to_string(
            home.join("app-sessions")
                .join(session_id)
                .join("manifest.json"),
        )
        .unwrap_or_else(|e| format!("<no manifest: {e}>"));
        format!(
            "{what} for {value} on {}; manifest: {manifest}",
            socket.display()
        )
    };
    // The socket FILE appears a beat before the Host accepts on it, and a
    // loaded machine widens that beat: a successful connect is the readiness
    // signal, bounded, never the file's existence.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(e)
                if Instant::now() < deadline
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("{}", describe(&format!("connect failed ({e})"))),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
        .write_all(format!("{}\n", serde_json::to_string(&value).unwrap()).as_bytes())
        .unwrap_or_else(|e| panic!("{}", describe(&format!("write failed ({e})"))));
    let mut response = String::new();
    match BufReader::new(stream).read_line(&mut response) {
        Ok(0) => panic!(
            "{}",
            describe("Host closed the socket without a reply (EOF)")
        ),
        Ok(_) => {}
        Err(e) => panic!("{}", describe(&format!("read failed ({e})"))),
    }
    serde_json::from_str(response.trim()).unwrap_or_else(|e| {
        panic!(
            "{}",
            describe(&format!("unparseable reply {response:?} ({e})"))
        )
    })
}

/// The Host's socket bind is the readiness signal for every test here. A
/// loaded machine (a workspace run beside a serve loop) takes several times
/// the idle ~1 s, so the bound is generous; the failure text carries the
/// Host's stderr instead of a bare `assert!`.
fn wait_for_host_socket(home: &Path, session_id: &str, host: &mut Child) -> PathBuf {
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    if wait_until(Duration::from_secs(30), || socket.exists()) {
        return socket;
    }
    let _ = host.kill();
    let status = host.wait().ok();
    let mut stderr = String::new();
    if let Some(pipe) = host.stderr.as_mut() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    panic!(
        "Host did not bind {} within 30 s (status {status:?}): {stderr}",
        socket.display()
    );
}

/// Wait for a manifest condition with the manifest in the failure text.
fn wait_for_manifest(
    home: &Path,
    session_id: &str,
    what: &str,
    condition: impl Fn(&Value) -> bool,
) {
    if wait_until(Duration::from_secs(30), || {
        condition(&manifest(home, session_id))
    }) {
        return;
    }
    panic!(
        "{what} never held within 30 s; manifest: {}",
        manifest(home, session_id)
    );
}

fn manifest(home: &Path, session_id: &str) -> Value {
    serde_json::from_slice(
        &fs::read(
            home.join("app-sessions")
                .join(session_id)
                .join("manifest.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

/// Reaps the Host (and its process group) even when a test panics before
/// `stop_and_reap`: orphaned session hosts from failed runs otherwise pile
/// up across a loop and load the next ones.
struct HostGuard {
    child: Child,
}

impl std::ops::Deref for HostGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl std::ops::DerefMut for HostGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let pid = self.child.id() as libc::pid_t;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

fn stop_and_reap(home: &Path, session_id: &str, child: &mut Child) {
    if child.try_wait().unwrap().is_none() {
        let _ = socket_command(home, session_id, json!({ "type": "kill" }));
        let _ = wait_until(Duration::from_secs(30), || {
            child.try_wait().ok().flatten().is_some()
        });
    }
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn recorded_pids(path: &Path) -> Vec<u32> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

fn write_executable(path: &Path, body: impl AsRef<[u8]>) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn initial_runtime_preparation_persists_host_minted_identity_before_ready() {
    let home = temp_home("initial-prep");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_claude = bin.join("claude");
    fs::write(
        &fake_claude,
        "#!/bin/bash\nprintf 'fixture claude launch\\n'\nexec -a claude /bin/sleep 300\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_claude).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_claude, permissions).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "prepared-session";
    let launch = write_launch(&home, session_id, "claude --model fixture");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(30), || socket.exists()));

    let ready = manifest(&home, session_id);
    let command = ready["session"]["command"].as_str().unwrap();
    let provider_id = ready["provider_session_id"].as_str().unwrap();
    assert_eq!(
        command,
        format!("claude --model fixture --session-id '{provider_id}'")
    );
    assert!(ready.get("managed_storage_path").is_none());
    let marker: Value = serde_json::from_slice(
        &fs::read(
            home.join("app-sessions")
                .join(session_id)
                .join("provider-session.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(marker["provider_session_id"], provider_id);

    // Once the minted conversation exists, the same Host-owned plan switches
    // from creation to exact resume and publishes its verified failure text
    // as opaque markers. Native never carries these provider strings.
    let transcript_dir = home.join(".claude").join("projects").join("fixture");
    fs::create_dir_all(&transcript_dir).unwrap();
    fs::write(transcript_dir.join(format!("{provider_id}.jsonl")), b"\n").unwrap();
    assert_eq!(
        socket_command(&home, session_id, json!({ "type": "write", "data": "x" }))["ok"],
        true
    );
    let plan_output = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg("__resume__")
        .arg(session_id)
        .env("UNPEEL_HOME", &home)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        plan_output.status.success(),
        "resume plan stderr: {}",
        String::from_utf8_lossy(&plan_output.stderr)
    );
    let plan: Value = serde_json::from_slice(&plan_output.stdout).unwrap();
    assert_eq!(
        plan["command"],
        format!("claude --model fixture --resume '{provider_id}'")
    );
    assert_eq!(
        plan["failure_markers"],
        json!(["No conversation found with session ID", provider_id])
    );

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

/// Is `pid` in the stopped (job-control `T`) state right now?
fn process_stopped(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim_start()
                .starts_with('T')
        })
        .unwrap_or(false)
}

fn process_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn resume_agent_keeps_host_identity_and_relaunches_exactly_from_owned_shell() {
    let home = temp_home("live");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    let generations = home.join("runtime-generations");
    // Keep a native foreground process with argv[0] `pi`, but deliberately
    // ignore every argument so the second launch tolerates Pi's --continue
    // flag and remains observable after restart.
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/bash\nprintf '%s\\n' \"${{UNPEEL_RUNTIME_GENERATION:-unset}}\" >> '{}'\nprintf 'fixture runtime launch\\n'\nif [ \"${{UNPEEL_RUNTIME_GENERATION:-unset}}\" = 1 ]; then exec -a pi /bin/sleep 2; fi\nexec -a pi /bin/sleep 300\n",
            generations.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_pi).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_pi, permissions).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "same-session";
    let launch = write_launch(&home, session_id, "pi 300");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    if !wait_until(Duration::from_secs(30), || socket.exists()) {
        let _ = host.kill();
        let status = host.wait().ok();
        let mut stderr = String::new();
        if let Some(pipe) = host.stderr.as_mut() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        panic!("Host did not bind socket (status {status:?}): {stderr}");
    }
    assert!(wait_until(Duration::from_secs(30), || {
        manifest(&home, session_id)["runtime"]["currentObservation"]["id"] == "pi"
    }));
    assert!(wait_until(Duration::from_secs(30), || {
        fs::read_to_string(&generations).is_ok_and(|value| value.lines().eq(["1"]))
    }));

    // Make restart choose the resume path, and seed the exact stale activity
    // record that must not survive into the new process generation.
    assert_eq!(
        socket_command(&home, session_id, json!({ "type": "write", "data": "x" }))["ok"],
        true
    );
    let session_dir = home.join("app-sessions").join(session_id);
    fs::write(
        session_dir.join("last-hook-event.json"),
        br#"{"event":"Stop"}"#,
    )
    .unwrap();
    let before = manifest(&home, session_id);
    let managed_storage = home.join("pi-sessions").join(session_id);
    assert_eq!(
        before["session"]["command"],
        format!(
            "pi 300 --session-dir '{}'",
            managed_storage.to_string_lossy()
        )
    );
    assert_eq!(
        before["managed_storage_path"],
        managed_storage.to_string_lossy().as_ref()
    );
    assert!(managed_storage.is_dir());
    // The resumable-conversation gate requires provider-created files inside
    // the pinned storage (directory existence alone is not evidence). The
    // fake pi ignores --session-dir, so seed the session data a real run
    // would have written.
    fs::write(managed_storage.join("session.jsonl"), b"{}\n").unwrap();
    assert_eq!(before["mcp_enabled"], true);
    assert_eq!(before["browser_mcp_enabled"], true);
    assert_eq!(before["computer_mcp_enabled"], true);
    assert_eq!(before["mcp_client_registered"], false);
    assert_eq!(before["browser_client_registered"], false);
    assert_eq!(before["computer_client_registered"], false);
    let before_pid = before["pid"].as_u64().unwrap();
    let before_runtime_pid = before["runtime"]["currentObservation"]["pid"]
        .as_u64()
        .unwrap();

    assert!(wait_until(Duration::from_secs(30), || {
        manifest(&home, session_id)["runtime"].is_null()
    }));

    let output = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg("__resume_agent__")
        .arg(session_id)
        .env("UNPEEL_HOME", &home)
        .env("PATH", &test_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "resume stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(wait_until(Duration::from_secs(30), || {
        let current = manifest(&home, session_id);
        current["runtime_launch_generation"] == 2
            && current["runtime"]["currentObservation"]["id"] == "pi"
            && current["runtime"]["currentObservation"]["pid"].as_u64() != Some(before_runtime_pid)
    }));
    assert!(wait_until(Duration::from_secs(30), || {
        fs::read_to_string(&generations).is_ok_and(|value| value.lines().eq(["1", "2"]))
    }));

    let after = manifest(&home, session_id);
    assert_eq!(after["session"]["id"], session_id);
    assert_eq!(after["pid"].as_u64(), Some(before_pid));
    assert_eq!(after["state"], "running");
    assert_eq!(
        after["session"]["command"],
        format!(
            "pi 300 --session-dir '{}' --continue",
            managed_storage.to_string_lossy()
        )
    );
    assert_eq!(after["runtime_launch_generation"], 2);
    assert_eq!(after["mcp_client_registered"], false);
    assert_eq!(after["browser_client_registered"], false);
    assert_eq!(after["computer_client_registered"], false);
    assert!(after["runtime_launched_at"].as_u64().is_some());
    let launch_offset = after["runtime_launch_output_offset"].as_u64().unwrap();
    assert!(
        launch_offset > 0,
        "in-place generation keeps an output boundary"
    );
    assert!(wait_until(Duration::from_secs(30), || {
        fs::read(session_dir.join("output.bin")).is_ok_and(|output| {
            launch_offset <= output.len() as u64
                && String::from_utf8_lossy(&output[launch_offset as usize..])
                    .contains("fixture runtime launch")
        })
    }));
    assert!(!session_dir.join("last-hook-event.json").exists());
    assert!(socket.exists(), "same control socket remains live");

    // A request prepared concurrently from the preceding generation cannot
    // queue a second resume command after the first request releases its Host
    // mutex. The generation compare-and-swap rejects it before any signal.
    let current_runtime_pid = after["runtime"]["currentObservation"]["pid"]
        .as_u64()
        .unwrap();
    let stale = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(stale["ok"], false);
    assert!(stale["error"]
        .as_str()
        .is_some_and(|error| error.contains("generation changed")));
    std::thread::sleep(Duration::from_millis(150));
    let unchanged = manifest(&home, session_id);
    assert_eq!(unchanged["runtime_launch_generation"], 2);
    assert_eq!(
        unchanged["runtime"]["currentObservation"]["pid"].as_u64(),
        Some(current_runtime_pid)
    );

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn blank_terminal_never_claims_mcp_registration_or_agent_restart() {
    let home = temp_home("blank");
    let session_id = "blank-session";
    let launch = write_launch(&home, session_id, "");
    let mut host = spawn_host(&home, &launch, None);
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(30), || socket.exists()));

    let running = manifest(&home, session_id);
    assert_eq!(running["mcp_client_registered"], false);
    assert_eq!(running["browser_client_registered"], false);
    assert_eq!(running["computer_client_registered"], false);
    assert_eq!(running["mcp_enabled"], true);
    assert_eq!(running["browser_mcp_enabled"], true);
    assert_eq!(running["computer_mcp_enabled"], true);
    assert_eq!(running["runtime_launch_generation"], 0);

    let inherited_generation = home.join("blank-inherited-generation");
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({
                "type": "write",
                "data": format!(
                    "printf '%s' \"${{UNPEEL_RUNTIME_GENERATION-unset}}\" > '{}'\r",
                    inherited_generation.display()
                )
            }),
        )["ok"],
        true
    );
    assert!(wait_until(Duration::from_secs(30), || {
        fs::read_to_string(&inherited_generation).is_ok_and(|value| value == "unset")
    }));

    // Automatic injection evidence stays false, but a provider the user
    // configured manually can start the same stdio server and receive only
    // the domains this blank Session was granted at launch.
    let mut mcp = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg("__mcp__")
        .env("UNPEEL_HOME", &home)
        .env("UNPEEL_SESSION_ID", session_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    mcp.stdin
        .take()
        .unwrap()
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        )
        .unwrap();
    let mcp_output = mcp.wait_with_output().unwrap();
    assert!(
        mcp_output.status.success(),
        "manual MCP stderr: {}",
        String::from_utf8_lossy(&mcp_output.stderr)
    );
    let tools_response = String::from_utf8_lossy(&mcp_output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response["id"] == 2)
        .expect("tools/list response");
    let tool_names: Vec<&str> = tools_response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        tool_names,
        vec![
            "agents",
            "sessions",
            "workspace",
            "artifacts",
            "browser",
            "computer",
            "apps",
            "skills"
        ]
    );

    // Persisted Kiro configs from before the generic gate rename invoke the
    // legacy argv and carry only Kiro's Sessions/Browser aliases. Even though
    // this Session manifest also grants Computer, that unregistered domain
    // must remain absent and uncallable.
    let mut legacy_kiro_mcp = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg("__kiro_mcp__")
        .env("UNPEEL_HOME", &home)
        .env("UNPEEL_SESSION_ID", session_id)
        .env("UNPEEL_KIRO_SESSIONS_MCP_ENABLED", "yes")
        .env("UNPEEL_KIRO_BROWSER_MCP_ENABLED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    legacy_kiro_mcp
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"computer\",\"arguments\":{\"action\":\"help\"}}}\n",
        )
        .unwrap();
    let legacy_output = legacy_kiro_mcp.wait_with_output().unwrap();
    assert!(
        legacy_output.status.success(),
        "legacy Kiro MCP stderr: {}",
        String::from_utf8_lossy(&legacy_output.stderr)
    );
    let legacy_responses = String::from_utf8_lossy(&legacy_output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let legacy_tools = legacy_responses
        .iter()
        .find(|response| response["id"] == 2)
        .expect("legacy Kiro tools/list response");
    let legacy_tool_names = legacy_tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        legacy_tool_names,
        vec![
            "agents",
            "sessions",
            "workspace",
            "artifacts",
            "browser",
            "apps",
            "skills"
        ]
    );
    let denied_computer = legacy_responses
        .iter()
        .find(|response| response["id"] == 3)
        .expect("legacy Kiro computer denial");
    assert_eq!(denied_computer["result"]["isError"], true);

    let rejected = socket_command(
        &home,
        session_id,
        json!({ "type": "restart_agent", "expected_generation": 0 }),
    );
    assert_eq!(rejected["ok"], false);

    stop_and_reap(&home, session_id, &mut host);
    assert!(wait_until(Duration::from_secs(30), || {
        manifest(&home, session_id)["state"] == "exited"
    }));
    let exited = manifest(&home, session_id);
    assert_eq!(exited["mcp_client_registered"], false);
    assert_eq!(exited["browser_client_registered"], false);
    assert_eq!(exited["computer_client_registered"], false);
    assert_eq!(exited["mcp_enabled"], true);
    assert_eq!(exited["browser_mcp_enabled"], true);
    assert_eq!(exited["computer_mcp_enabled"], true);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn kiro_registration_evidence_excludes_the_unimplemented_computer_domain() {
    let home = temp_home("kiro-mcp");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_kiro = bin.join("kiro-cli");
    fs::write(
        &fake_kiro,
        b"#!/bin/bash\nexec -a kiro-cli /bin/sleep 300\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_kiro).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_kiro, permissions).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "kiro-registration";
    let launch = write_launch(&home, session_id, "kiro-cli");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(30), || socket.exists()));
    let running = manifest(&home, session_id);
    assert_eq!(running["mcp_enabled"], true);
    assert_eq!(running["browser_mcp_enabled"], true);
    assert_eq!(running["computer_mcp_enabled"], true);
    assert_eq!(running["mcp_client_registered"], true);
    assert_eq!(running["browser_client_registered"], true);
    assert_eq!(running["computer_client_registered"], false);

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn restart_agent_rejects_a_different_live_foreground_runtime() {
    let home = temp_home("mismatch");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    symlink("/bin/sleep", bin.join("pi")).unwrap();
    let manual_generation = home.join("manual-runtime-generation");
    let mystery_pid = home.join("mystery-pid");
    let fake_claude = bin.join("claude");
    fs::write(
        &fake_claude,
        format!(
            "#!/bin/bash\nprintf '%s' \"${{UNPEEL_RUNTIME_GENERATION:-unset}}\" > '{}'\nexec -a claude /bin/sleep \"$@\"\n",
            manual_generation.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_claude).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_claude, permissions).unwrap();
    let mystery = bin.join("mystery");
    fs::write(
        &mystery,
        format!(
            "#!/bin/bash\nprintf '%s' \"$$\" > '{}'\nexec -a mystery /bin/sleep 300\n",
            mystery_pid.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&mystery).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&mystery, permissions).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "mismatch-session";
    let launch = write_launch(&home, session_id, "pi 1");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let _socket = wait_for_host_socket(&home, session_id, &mut host);
    // Once the stable pi command returns, start a different recognized agent
    // manually in the fallback shell. Observation may change presentation,
    // but must never authorize the stable pi restart recipe against Claude.
    // Readiness, not a sleep: the marker command queues in the PTY behind
    // `pi 1` and runs the moment the fallback shell owns the foreground,
    // however long the stable command takes on a loaded machine.
    let fallback_ready = home.join("mismatch-fallback-ready");
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({
                "type": "write",
                "data": format!("printf ready > '{}'\r", fallback_ready.display())
            })
        )["ok"],
        true
    );
    assert!(
        wait_until(Duration::from_secs(30), || fallback_ready.exists()),
        "fallback shell never ran the readiness marker; manifest: {}",
        manifest(&home, session_id)
    );
    wait_for_manifest(&home, session_id, "stable pi exit observed", |current| {
        current["runtime"].is_null() && current["runtime_launch_pending"] == false
    });
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "claude 300\r" })
        )["ok"],
        true
    );
    wait_for_manifest(
        &home,
        session_id,
        "manual claude observed in the foreground",
        |current| current["runtime"]["currentObservation"]["id"] == "claude",
    );
    assert_eq!(
        fs::read_to_string(&manual_generation).unwrap(),
        "unset",
        "generation must not leak into the fallback shell or a manually typed runtime"
    );

    let rejected = socket_command(
        &home,
        session_id,
        json!({ "type": "restart_agent", "expected_generation": 1 }),
    );
    assert_eq!(rejected["ok"], false);
    assert!(rejected["error"]
        .as_str()
        .is_some_and(|error| error.contains("terminal foreground is claude")));
    assert_eq!(manifest(&home, session_id)["runtime_launch_generation"], 1);

    // A different recognized runtime remains a blocker after Ctrl-Z returns
    // Bash to the foreground. The full-session proof must not look only for
    // the stable command's expected `pi` identity.
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "\u{1a}" })
        )["ok"],
        true
    );
    let shell_ready = home.join("mismatch-shell-ready");
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({
                "type": "write",
                "data": format!("printf ready > '{}'\r", shell_ready.display())
            })
        )["ok"],
        true
    );
    assert!(
        wait_until(Duration::from_secs(30), || shell_ready.exists()),
        "shell never returned after Ctrl-Z; manifest: {}",
        manifest(&home, session_id)
    );
    let background_rejected = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(
        background_rejected["ok"], false,
        "background response: {background_rejected}"
    );
    assert!(background_rejected["error"]
        .as_str()
        .is_some_and(|error| error.contains("claude")));
    assert_eq!(manifest(&home, session_id)["runtime_launch_generation"], 1);

    // Terminate Claude through ordinary job control, then start a foreground
    // job that is not a known runtime. Missing observation is not permission:
    // an isolated unknown process group must also survive Resume Agent.
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "fg\r" })
        )["ok"],
        true
    );
    // Ctrl-C only reaches Claude once `fg` has actually resumed it. The
    // observer still reports the STOPPED job as claude, so the manifest is
    // no signal here; the kernel's process state is: wait for Claude's pid
    // to leave the stopped state.
    let claude_pid = manifest(&home, session_id)["runtime"]["currentObservation"]["pid"]
        .as_u64()
        .expect("claude pid observed") as u32;
    assert!(
        wait_until(Duration::from_secs(30), || !process_stopped(claude_pid)),
        "fg never resumed claude (pid {claude_pid}); manifest: {}",
        manifest(&home, session_id)
    );

    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "\u{3}" })
        )["ok"],
        true
    );
    wait_for_manifest(&home, session_id, "claude gone after Ctrl-C", |current| {
        current["runtime"].is_null()
    });
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "mystery\r" })
        )["ok"],
        true
    );
    // `printf '%s' "$$" > file` creates the file BEFORE writing it: wait for
    // the pid itself, not the file's existence.
    let mut mystery_pid_value = None;
    assert!(
        wait_until(Duration::from_secs(30), || {
            mystery_pid_value = fs::read_to_string(&mystery_pid)
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok());
            mystery_pid_value.is_some()
        }),
        "mystery job never recorded its pid; manifest: {}",
        manifest(&home, session_id)
    );
    let mystery_pid: u32 = mystery_pid_value.unwrap();
    let rejected = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(rejected["ok"], false);
    assert!(rejected["error"]
        .as_str()
        .is_some_and(|error| error.contains("unrecognized foreground job")));
    assert!(process_alive(mystery_pid));
    assert_eq!(manifest(&home, session_id)["runtime_launch_generation"], 1);

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn resume_and_legacy_restart_reject_an_active_expected_runtime_without_signaling_it() {
    let home = temp_home("kill-race");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let launches = home.join("runtime-launches");
    let term_seen = home.join("restart-term-seen");
    let fake_pi = bin.join("pi");
    // argv[0] remains `pi` for observation. The TERM trap is positive proof
    // that neither the new action nor the legacy spelling signaled the live
    // expected runtime before rejecting it.
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/bash\nprintf '%s\\n' \"$$\" >> '{}'\nexec -a pi /bin/bash -c 'trap \"/usr/bin/touch {}\" TERM; while :; do /bin/sleep 1; done'\n",
            launches.display(),
            term_seen.display()
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&fake_pi).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_pi, permissions).unwrap();
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "kill-race-session";
    let launch = write_launch(&home, session_id, "pi");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(30), || socket.exists()));
    assert!(wait_until(Duration::from_secs(30), || {
        manifest(&home, session_id)["runtime"]["currentObservation"]["id"] == "pi"
    }));
    assert_eq!(
        socket_command(&home, session_id, json!({ "type": "write", "data": "x" }))["ok"],
        true
    );

    let runtime_pid = manifest(&home, session_id)["runtime"]["currentObservation"]["pid"]
        .as_u64()
        .unwrap() as u32;
    let resumed = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(resumed["ok"], false, "resume response: {resumed}");
    assert!(
        resumed["error"]
            .as_str()
            .is_some_and(|error| error.contains("agent is still running")),
        "resume response: {resumed}"
    );

    let legacy = socket_command(
        &home,
        session_id,
        json!({ "type": "restart_agent", "expected_generation": 1 }),
    );
    assert_eq!(legacy["ok"], false, "legacy response: {legacy}");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !term_seen.exists(),
        "Resume Agent signaled the live runtime"
    );
    assert!(process_alive(runtime_pid));
    assert_eq!(recorded_pids(&launches), vec![runtime_pid]);
    assert_eq!(manifest(&home, session_id)["runtime_launch_generation"], 1);

    let killed = socket_command(&home, session_id, json!({ "type": "kill" }));
    assert_eq!(killed["ok"], true, "kill response: {killed}");

    assert!(wait_until(Duration::from_secs(30), || {
        host.try_wait().ok().flatten().is_some()
    }));
    assert!(wait_until(Duration::from_secs(30), || {
        manifest(&home, session_id)["state"] == "exited"
    }));
    let exited = manifest(&home, session_id);
    assert_eq!(exited["runtime_launch_generation"], 1);

    // Explicit Host stop is allowed to terminate the runtime, but neither
    // rejected in-place request may have launched a replacement.
    std::thread::sleep(Duration::from_millis(300));
    let launched_pids = recorded_pids(&launches);
    assert_eq!(launched_pids, vec![runtime_pid]);
    assert!(wait_until(Duration::from_secs(30), || {
        launched_pids.iter().all(|pid| !process_alive(*pid))
    }));
    let launches_after_exit = fs::read(&launches).unwrap_or_default();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fs::read(&launches).unwrap_or_default(),
        launches_after_exit,
        "an agent launched after the Host had exited"
    );
    assert!(!socket.exists());

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn stopped_background_runtime_keeps_resume_unadvertised_and_is_never_injected_into() {
    let home = temp_home("ctrl-z");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let runtime_pid_path = home.join("runtime-pids");
    write_executable(
        &bin.join("pi"),
        format!(
            "#!/bin/bash\nprintf '%s\\n' \"$$\" >> '{}'\nif [ \"${{UNPEEL_RUNTIME_GENERATION:-unset}}\" = 1 ]; then exec -a pi /bin/sleep 1; fi\nexec -a pi /bin/sleep 300\n",
            runtime_pid_path.display()
        ),
    );
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "ctrl-z-session";
    let launch = write_launch(&home, session_id, "pi");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(30), || socket.exists()));
    assert!(wait_until(Duration::from_secs(30), || {
        let current = manifest(&home, session_id);
        current["runtime"]["currentObservation"]["id"] == "pi"
            && current["runtime_launch_pending"] == false
    }));
    assert!(wait_until(Duration::from_secs(30), || {
        let current = manifest(&home, session_id);
        current["runtime"].is_null() && current["runtime_launch_pending"] == false
    }));
    // Start the same expected runtime as an ordinary interactive-shell job.
    // Unlike the initial `-c` startup script, this gives Bash normal job
    // control so Ctrl-Z leaves a real stopped background process behind.
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "pi\r" }),
        )["ok"],
        true
    );
    wait_for_manifest(
        &home,
        session_id,
        "hand-typed pi observed with its pid recorded",
        |current| {
            current["runtime"]["currentObservation"]["id"] == "pi"
                && fs::read_to_string(&runtime_pid_path).is_ok_and(|pids| pids.lines().count() == 2)
        },
    );
    let runtime_pid = recorded_pids(&runtime_pid_path)[1];

    // Ctrl-Z stops the foreground runtime job and returns terminal control to
    // Bash. A foreground-only check would now misclassify this as OwnedShell.
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "\u{1a}" }),
        )["ok"],
        true
    );
    let shell_ready = home.join("shell-ready");
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({
                "type": "write",
                "data": format!("printf ready > '{}'\r", shell_ready.display())
            }),
        )["ok"],
        true
    );
    assert!(wait_until(Duration::from_secs(30), || shell_ready.exists()));

    // Stay beyond the old six-miss hysteresis window. The all-session process
    // proof must retain the expected runtime observation, keeping summaries'
    // Resume Agent capability false before the request is attempted.
    std::thread::sleep(Duration::from_millis(2_200));
    let stopped = manifest(&home, session_id);
    assert!(process_alive(runtime_pid), "stopped runtime disappeared");
    assert_eq!(
        stopped["runtime"]["currentObservation"]["id"], "pi",
        "stopped manifest: {stopped}"
    );
    assert_eq!(
        stopped["runtime"]["currentObservation"]["pid"].as_u64(),
        Some(u64::from(runtime_pid))
    );
    assert_eq!(stopped["runtime_launch_pending"], false);

    let rejected = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(rejected["ok"], false, "resume response: {rejected}");
    assert!(rejected["error"]
        .as_str()
        .is_some_and(|error| error.contains("agent is still running")));
    assert!(process_alive(runtime_pid));
    assert_eq!(manifest(&home, session_id)["runtime_launch_generation"], 1);

    // Bring the stopped job back to the foreground so the ordinary Host Kill
    // path can terminate the complete job without leaving a stopped orphan.
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "fg\r" }),
        )["ok"],
        true
    );
    std::thread::sleep(Duration::from_millis(200));
    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn background_runtime_exec_rename_retains_exact_job_blocker() {
    let home = temp_home("rename");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let runtime_pid_path = home.join("runtime-pids");
    let renamed_marker = home.join("runtime-renamed");
    write_executable(
        &bin.join("pi"),
        format!(
            "#!/bin/bash\nprintf '%s\\n' \"$$\" >> '{}'\nif [ \"${{UNPEEL_RUNTIME_GENERATION:-unset}}\" = 1 ]; then exec -a pi /bin/sleep 1; fi\nexec -a pi /bin/bash -c \"trap '/bin/mkdir \\\"{}\\\"; exec -a mystery /bin/sleep 300' CONT; while :; do /bin/sleep 1; done\"\n",
            runtime_pid_path.display(),
            renamed_marker.display()
        ),
    );
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "rename-session";
    let launch = write_launch(&home, session_id, "pi");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let _socket = wait_for_host_socket(&home, session_id, &mut host);
    wait_for_manifest(&home, session_id, "stable pi exit observed", |current| {
        current["runtime"].is_null() && current["runtime_launch_pending"] == false
    });

    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "pi\r" }),
        )["ok"],
        true
    );
    assert!(wait_until(Duration::from_secs(30), || {
        manifest(&home, session_id)["runtime"]["currentObservation"]["id"] == "pi"
            && fs::read_to_string(&runtime_pid_path).is_ok_and(|pids| pids.lines().count() == 2)
    }));
    let runtime_pid = recorded_pids(&runtime_pid_path)[1];
    let observed = manifest(&home, session_id);
    assert_eq!(
        observed["runtime"]["currentObservation"]["processGroupID"].as_u64(),
        Some(u64::from(runtime_pid)),
        "interactive runtime owns an isolated job group: {observed}"
    );

    // Stop the recognized runtime, return Bash to the foreground, then resume
    // it as a background job. Its CONT trap execs the exact same PID into an
    // unrecognized argv/name. Fresh catalog matching now returns nothing; the
    // retained PID/start/PGID evidence must remain authoritative.
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "\u{1a}" }),
        )["ok"],
        true
    );
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "bg\r" }),
        )["ok"],
        true
    );
    assert!(
        wait_until(Duration::from_secs(30), || renamed_marker.is_dir()),
        "CONT trap never renamed the job; manifest: {}",
        manifest(&home, session_id)
    );
    // Negative proof: the observer must re-run after the rename and STILL
    // retain the identity. A retained observation leaves no fresh stamp to
    // wait on (nothing changes), so this is a cadence window — longer than
    // one observer period — not a synchronization point; the assertions
    // below explain themselves if it ever proves too short.
    std::thread::sleep(Duration::from_millis(2_200));

    let retained = manifest(&home, session_id);
    assert!(process_alive(runtime_pid), "renamed runtime disappeared");
    assert_eq!(
        retained["runtime"]["currentObservation"]["id"], "pi",
        "renamed exact identity must stay retained: {retained}"
    );
    assert_eq!(
        retained["runtime"]["currentObservation"]["pid"].as_u64(),
        Some(u64::from(runtime_pid))
    );

    let rejected = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(rejected["ok"], false, "resume response: {rejected}");
    assert!(rejected["error"]
        .as_str()
        .is_some_and(|error| error.contains("agent is still running")));
    assert!(process_alive(runtime_pid));
    assert_eq!(recorded_pids(&runtime_pid_path).len(), 2);
    assert_eq!(manifest(&home, session_id)["runtime_launch_generation"], 1);

    // Put the renamed job back in front so Host Kill owns and reaps it.
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": "fg\r" }),
        )["ok"],
        true
    );
    std::thread::sleep(Duration::from_millis(200));
    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn same_pid_pgid_shell_exec_command_is_not_the_owned_interactive_shell() {
    let home = temp_home("same-shell-exec");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let generations = home.join("runtime-generations");
    write_executable(
        &bin.join("pi"),
        format!(
            "#!/bin/bash\nprintf '%s\\n' \"${{UNPEEL_RUNTIME_GENERATION:-unset}}\" >> '{}'\nexec -a pi /bin/sleep 1\n",
            generations.display()
        ),
    );
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "same-shell-exec-session";
    let launch = write_launch(&home, session_id, "pi");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(30), || socket.exists()));
    assert!(wait_until(Duration::from_secs(30), || {
        let current = manifest(&home, session_id);
        current["runtime"].is_null() && current["runtime_launch_pending"] == false
    }));

    let exec_pid_path = home.join("exec-pid");
    let exec_command = format!(
        "exec /bin/bash -c 'printf %s \"$$\" > \"{}\"; trap : TERM; while :; do /bin/sleep 1; done'\r",
        exec_pid_path.display()
    );
    assert_eq!(
        socket_command(
            &home,
            session_id,
            json!({ "type": "write", "data": exec_command }),
        )["ok"],
        true
    );
    // `printf '%s' "$$" > file` creates the file before the write lands:
    // wait for a parseable pid, not for the file to exist.
    let mut exec_pid_parsed: Option<u32> = None;
    assert!(
        wait_until(Duration::from_secs(30), || {
            exec_pid_parsed = fs::read_to_string(&exec_pid_path)
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok());
            exec_pid_parsed.is_some()
        }),
        "exec pid file never held a pid: {}",
        exec_pid_path.display()
    );
    let exec_pid = exec_pid_parsed.unwrap();
    assert_eq!(
        manifest(&home, session_id)["pid"].as_u64(),
        Some(u64::from(exec_pid)),
        "exec retains the Host-owned session leader PID"
    );

    let rejected = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(rejected["ok"], false, "resume response: {rejected}");
    assert!(rejected["error"]
        .as_str()
        .is_some_and(|error| error.contains("owned interactive login shell")));
    assert!(process_alive(exec_pid));
    assert_eq!(
        fs::read_to_string(&generations)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec!["1"]
    );
    assert_eq!(manifest(&home, session_id)["runtime_launch_generation"], 1);

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn launch_pending_rejects_duplicate_initial_and_post_resume_submissions() {
    let home = temp_home("launch-pending");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let generations = home.join("runtime-generations");
    let release_initial = home.join("release-initial");
    let release_resume = home.join("release-resume");
    write_executable(
        &bin.join("pi"),
        format!(
            "#!/bin/bash\ngeneration=${{UNPEEL_RUNTIME_GENERATION:-unset}}\nprintf '%s\\n' \"$generation\" >> '{}'\nif [ \"$generation\" = 1 ]; then release='{}'; duration=1; else release='{}'; duration=300; fi\nwhile [ ! -e \"$release\" ]; do /bin/sleep 0.05; done\nexec -a pi /bin/sleep \"$duration\"\n",
            generations.display(),
            release_initial.display(),
            release_resume.display()
        ),
    );
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{inherited_path}", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "launch-pending-session";
    let launch = write_launch(&home, session_id, "pi");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let socket = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    assert!(wait_until(Duration::from_secs(30), || socket.exists()));
    assert!(wait_until(Duration::from_secs(30), || {
        fs::read_to_string(&generations).is_ok_and(|value| value.lines().eq(["1"]))
    }));
    let initial = manifest(&home, session_id);
    assert_eq!(initial["runtime_launch_generation"], 1);
    assert_eq!(initial["runtime_launch_pending"], true);
    assert!(initial["runtime"].is_null());
    let duplicate_initial = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(duplicate_initial["ok"], false);
    assert!(duplicate_initial["error"]
        .as_str()
        .is_some_and(|error| error.contains("resume launch is pending")));
    assert_eq!(fs::read_to_string(&generations).unwrap().lines().count(), 1);

    fs::write(&release_initial, b"go").unwrap();
    assert!(wait_until(Duration::from_secs(30), || {
        let current = manifest(&home, session_id);
        current["runtime"]["currentObservation"]["id"] == "pi"
            && current["runtime_launch_pending"] == false
    }));
    assert!(wait_until(Duration::from_secs(30), || {
        let current = manifest(&home, session_id);
        current["runtime"].is_null() && current["runtime_launch_pending"] == false
    }));

    let resumed = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 1 }),
    );
    assert_eq!(resumed["ok"], true, "resume response: {resumed}");
    assert!(wait_until(Duration::from_secs(30), || {
        fs::read_to_string(&generations).is_ok_and(|value| value.lines().eq(["1", "2"]))
    }));
    let post_submit = manifest(&home, session_id);
    assert_eq!(post_submit["runtime_launch_generation"], 2);
    assert_eq!(post_submit["runtime_launch_pending"], true);
    assert!(post_submit["runtime"].is_null());

    let duplicate_resume = socket_command(
        &home,
        session_id,
        json!({ "type": "resume_agent", "expected_generation": 2 }),
    );
    assert_eq!(duplicate_resume["ok"], false);
    assert!(duplicate_resume["error"]
        .as_str()
        .is_some_and(|error| error.contains("resume launch is pending")));
    assert_eq!(fs::read_to_string(&generations).unwrap().lines().count(), 2);

    fs::write(&release_resume, b"go").unwrap();
    assert!(wait_until(Duration::from_secs(30), || {
        let current = manifest(&home, session_id);
        current["runtime"]["currentObservation"]["id"] == "pi"
            && current["runtime_launch_pending"] == false
    }));
    assert_eq!(fs::read_to_string(&generations).unwrap().lines().count(), 2);

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn missing_initial_runtime_clears_pending_on_definitive_wrapper_completion() {
    let home = temp_home("missing-runtime");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let test_path = format!("{}:/usr/bin:/bin", bin.display());
    fs::write(
        home.join(".bash_profile"),
        format!("export PATH='{}'\n", test_path.replace('\'', "'\\''")),
    )
    .unwrap();

    let session_id = "missing-runtime-session";
    let launch = write_launch(&home, session_id, "pi");
    let mut host = spawn_host(&home, &launch, Some(&test_path));
    let _socket = wait_for_host_socket(&home, session_id, &mut host);
    wait_for_manifest(
        &home,
        session_id,
        "definitive wrapper completion clears pending with no runtime",
        |current| {
            current["state"] == "running"
                && current["runtime_launch_generation"] == 1
                && current["runtime_launch_pending"] == false
                && current["runtime"].is_null()
        },
    );
    // The observer publishes the cleared manifest and then consumes the
    // marker; those are two steps, so wait for the second rather than
    // asserting it happened in the same instant as the first.
    let marker = home
        .join("app-sessions")
        .join(session_id)
        .join(".runtime-launch-complete");
    assert!(
        wait_until(Duration::from_secs(30), || !marker.exists()),
        "observer never consumed the definitive completion marker; manifest: {}",
        manifest(&home, session_id)
    );

    stop_and_reap(&home, session_id, &mut host);
    let _ = fs::remove_dir_all(home);
}
