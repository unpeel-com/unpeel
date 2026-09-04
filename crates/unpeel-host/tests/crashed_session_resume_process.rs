#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use unpeel_core::session_host::{
    manifest_pid_identity, HostedSessionManifest, HostedSessionState, PidIdentity,
    SESSION_HOST_RESUME_AGENT_PROTOCOL_VERSION,
};
use unpeel_core::state::SessionInfo;

fn temp_home() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Keep the replacement Host's Unix socket below macOS sockaddr_un's
    // short path limit even after adding app-sessions/<uuid>/session.sock.
    PathBuf::from("/tmp").join(format!("u-cr-{}-{nonce:x}", std::process::id()))
}

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

fn manifest(session_id: &str, pid: u32) -> HostedSessionManifest {
    HostedSessionManifest {
        session: SessionInfo {
            id: session_id.into(),
            project_id: "project".into(),
            label: "Terminal".into(),
            custom_title: false,
            command: String::new(),
            created_at: 1,
            owner_principal_id: None,
            created_by_device_id: None,
            source_preset_id: None,
            tag_id: None,
            worktree_path: None,
            worktree_branch: None,
            parent_session_id: None,
            spawned_by: None,
            role: None,
            task: None,
        },
        cwd: "/tmp".into(),
        state: HostedSessionState::Running,
        pid: Some(pid),
        pid_started_at: None,
        host_pid: None,
        host_pid_started_at: None,
        exit_code: None,
        host_build_id: None,
        host_protocol_version: Some(SESSION_HOST_RESUME_AGENT_PROTOCOL_VERSION),
        has_been_written_to: false,
        provider_session_id: None,
        provider_transcript_path: None,
        managed_storage_path: None,
        resume_failure_markers: Vec::new(),
        runtime: None,
        active_app: None,
        runtime_launch_generation: 0,
        runtime_launch_pending: false,
        runtime_launched_at: None,
        runtime_launch_output_offset: 0,
        mcp_enabled: None,
        browser_mcp_enabled: None,
        computer_mcp_enabled: None,
        mcp_client_registered: false,
        browser_client_registered: false,
        computer_client_registered: false,
        menu_prompt_active: false,
        terminal_modes: None,
        screen_changed_at: None,
        detected_local_urls: Vec::new(),
        heartbeat_at: 1,
        updated_at: 1,
    }
}

fn create_stale_socket(home: &Path, session_id: &str) {
    let path = home
        .join("app-sessions")
        .join(session_id)
        .join("session.sock");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"stale").unwrap();
}

#[test]
fn crashed_running_manifest_resumes_but_a_healthy_running_manifest_is_rejected() {
    let home = temp_home();
    fs::create_dir_all(&home).unwrap();
    let old_unpeel_home = std::env::var_os("UNPEEL_HOME");
    let old_home = std::env::var_os("HOME");
    let old_shell = std::env::var_os("SHELL");
    let old_host_cmd = std::env::var_os("UNPEEL_HOST_CMD");
    unsafe {
        std::env::set_var("UNPEEL_HOME", &home);
        std::env::set_var("HOME", &home);
        std::env::set_var("SHELL", "/bin/bash");
        std::env::set_var("UNPEEL_HOST_CMD", env!("CARGO_BIN_EXE_unpeel-host"));
    }

    let healthy_id = "healthy-running";
    create_stale_socket(&home, healthy_id);
    unpeel_core::session_host::save_manifest(&manifest(healthy_id, std::process::id())).unwrap();
    let healthy_error = unpeel_core::session_ops::resume_session(healthy_id, None, 80, 24)
        .expect_err("a healthy running Host must never be replaced");
    assert!(healthy_error.contains("still running"), "{healthy_error}");
    assert!(home.join("app-sessions").join(healthy_id).exists());

    // A missing control socket makes the Host unhealthy, but it is not proof
    // that its child died. Keep a real child blocked on stdin with this exact
    // Session id in argv so the legacy identity fallback positively matches.
    // Resume must fail closed without signaling it or deleting its directory.
    let live_missing_socket_id = "live-child-no-socket";
    let mut live_child = Command::new("/bin/sh")
        .args(["-c", "read _", live_missing_socket_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let live_manifest = manifest(live_missing_socket_id, live_child.id());
    assert_eq!(manifest_pid_identity(&live_manifest), PidIdentity::Matches);
    unpeel_core::session_host::save_manifest(&live_manifest).unwrap();
    assert!(!unpeel_core::session_host::socket_path(live_missing_socket_id).exists());
    let live_error = unpeel_core::session_ops::resume_session(live_missing_socket_id, None, 80, 24)
        .expect_err("a matching live child without a socket must not be replaced");
    assert!(live_error.contains("still running"), "{live_error}");
    assert!(live_child.try_wait().unwrap().is_none());
    assert_eq!(
        unpeel_core::session_host::load_manifest(live_missing_socket_id)
            .unwrap()
            .state,
        HostedSessionState::Running
    );
    assert!(home
        .join("app-sessions")
        .join(live_missing_socket_id)
        .exists());
    live_child.kill().unwrap();
    live_child.wait().unwrap();

    let crashed_id = "crashed-running";
    create_stale_socket(&home, crashed_id);
    // Deliberately impossible live ownership: the stale manifest still says
    // Running, but its recorded child is gone.
    unpeel_core::session_host::save_manifest(&manifest(crashed_id, i32::MAX as u32)).unwrap();
    let replacement_id =
        unpeel_core::session_ops::resume_session(crashed_id, None, 80, 24).unwrap();
    assert_ne!(replacement_id, crashed_id);
    assert!(!home.join("app-sessions").join(crashed_id).exists());
    assert!(wait_until(Duration::from_secs(5), || {
        unpeel_core::session_host::socket_path(&replacement_id).exists()
            && unpeel_core::session_host::load_manifest(&replacement_id)
                .is_some_and(|manifest| manifest.state == HostedSessionState::Running)
    }));
    let replacement_error = unpeel_core::session_ops::resume_session(&replacement_id, None, 80, 24)
        .expect_err("the healthy replacement Host must not be replaced again");
    assert!(
        replacement_error.contains("still running"),
        "{replacement_error}"
    );
    assert!(unpeel_core::session_host::socket_path(&replacement_id).exists());

    unpeel_core::session_ops::stop_session(&replacement_id).unwrap();
    let _ = fs::remove_dir_all(home.join("app-sessions").join(healthy_id));
    let _ = fs::remove_dir_all(home.join("app-sessions").join(live_missing_socket_id));
    let _ = fs::remove_dir_all(&home);

    unsafe {
        match old_unpeel_home {
            Some(value) => std::env::set_var("UNPEEL_HOME", value),
            None => std::env::remove_var("UNPEEL_HOME"),
        }
        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match old_shell {
            Some(value) => std::env::set_var("SHELL", value),
            None => std::env::remove_var("SHELL"),
        }
        match old_host_cmd {
            Some(value) => std::env::set_var("UNPEEL_HOST_CMD", value),
            None => std::env::remove_var("UNPEEL_HOST_CMD"),
        }
    }
}
