#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use unpeel_core::host_connection::{
    DeliveryState, HostCall, HostConnection, HostConnectionError, RequestSemantics,
};
use unpeel_core::remote_session_backend::{
    RemoteDesktopResize, RemoteEffectFailureKind, RemoteOutputPollOptions, RemoteSessionBackend,
};
use unpeel_core::ssh_connection::{
    SshConnectionOptions, SshHostConnection, SshLaunchMode, SshTarget,
};

const PURITY_CHILD_ENV: &str = "UNPEEL_REMOTE_PURITY_CHILD";
const PURITY_SSH_ENV: &str = "UNPEEL_REMOTE_PURITY_SSH";
const PURITY_TEST_NAME: &str = "remote_scope_process_uses_only_host_state";

struct Fixture {
    root: PathBuf,
    host_home: PathBuf,
    ssh_program: PathBuf,
    arguments: PathBuf,
    invocation_count: PathBuf,
    gateway_pid: PathBuf,
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
            PathBuf::from("/tmp").join(format!("u-sc-{}-{label}-{nonce:x}", std::process::id()));
        let host_home = root.join("host");
        let host_user = root.join("host-user");
        std::fs::create_dir_all(&host_user).unwrap();
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
              "session":{"id":"s1","project_id":"p1","label":"SSH Controller","command":"cat","created_at":1},
              "cwd":"/tmp","state":"exited","pid":null,"exit_code":0
            }"#,
        )
        .unwrap();
        std::fs::write(session_dir.join("output.bin"), b"hello").unwrap();

        let ssh_program = root.join("fake ssh");
        let arguments = root.join("ssh-arguments.bin");
        let invocation_count = root.join("ssh-count");
        let gateway_pid = root.join("ssh-pid");
        let script = format!(
            "#!/bin/sh\n\
             arguments={}\n\
             count_file={}\n\
             pid_file={}\n\
             : > \"$arguments\"\n\
             for argument in \"$@\"; do printf '%s\\0' \"$argument\" >> \"$arguments\"; done\n\
             count=0\n\
             if [ -f \"$count_file\" ]; then IFS= read -r count < \"$count_file\" || count=0; fi\n\
             count=$((count + 1))\n\
             printf '%s\\n' \"$count\" > \"$count_file\"\n\
             printf '%s\\n' \"$$\" > \"$pid_file\"\n\
             export HOME={}\n\
             export UNPEEL_HOME={}\n\
             exec {} __remote_stdio__\n",
            shell_quote(&arguments),
            shell_quote(&invocation_count),
            shell_quote(&gateway_pid),
            shell_quote(&host_user),
            shell_quote(&host_home),
            shell_quote(Path::new(env!("CARGO_BIN_EXE_unpeel-host"))),
        );
        std::fs::write(&ssh_program, script).unwrap();
        let mut permissions = std::fs::metadata(&ssh_program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ssh_program, permissions).unwrap();

        Self {
            root,
            host_home,
            ssh_program,
            arguments,
            invocation_count,
            gateway_pid,
        }
    }

    fn connection(&self) -> SshHostConnection {
        SshHostConnection::with_ssh_program(
            SshTarget::parse("ssh://studio").unwrap(),
            &self.ssh_program,
        )
        .unwrap()
    }

    fn replace_ssh_program(&self, script: &str) {
        std::fs::write(&self.ssh_program, script).unwrap();
        let mut permissions = std::fs::metadata(&self.ssh_program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&self.ssh_program, permissions).unwrap();
    }

    fn count(&self) -> usize {
        std::fs::read_to_string(&self.invocation_count)
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0)
    }

    fn kill_gateway(&self) {
        assert!(wait_until(Duration::from_secs(5), || self
            .gateway_pid
            .is_file()));
        let pid = std::fs::read_to_string(&self.gateway_pid).unwrap();
        let status = Command::new("/bin/kill")
            .arg("-KILL")
            .arg(pid.trim())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "could not kill gateway pid {}",
            pid.trim()
        );
    }

    fn cleanup(self) {
        assert_eq!(self.root.parent(), Some(Path::new("/tmp")));
        assert!(self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("u-sc-")));
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

fn assert_controller_home_is_empty() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("purity child HOME"));
    let unpeel_home =
        PathBuf::from(std::env::var_os("UNPEEL_HOME").expect("purity child UNPEEL_HOME"));
    let entries: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(entries.is_empty(), "Controller HOME changed: {entries:?}");
    assert!(
        !unpeel_home.exists(),
        "Controller UNPEEL_HOME was created: {}",
        unpeel_home.display()
    );
}

fn request(
    connection: &SshHostConnection,
    call: HostCall,
    timeout: Duration,
) -> Result<unpeel_core::host_connection::HostReply, HostConnectionError> {
    let prepared = connection.prepare(call)?;
    connection.request(prepared, timeout)
}

fn bootstrap_call() -> HostCall {
    HostCall::new("GET", "/mobile/bootstrap", RequestSemantics::ReadOnly)
}

fn output_call(offset: Option<u64>, wait_ms: u64) -> HostCall {
    let mut call = HostCall::new("GET", "/mobile/output", RequestSemantics::ReadOnly)
        .with_query("session_id", "s1")
        .with_query("wait_ms", wait_ms.to_string());
    if let Some(offset) = offset {
        call = call.with_query("offset", offset.to_string());
    }
    call
}

fn mark_read_call() -> HostCall {
    HostCall::new("POST", "/mobile/mark-read", RequestSemantics::Effect)
        .with_body("application/json", br#"{"sessionID":"s1"}"#.to_vec())
}

fn body_json(reply: &unpeel_core::host_connection::HostReply) -> serde_json::Value {
    serde_json::from_slice(&reply.body).unwrap()
}

#[test]
fn semantic_backend_bootstraps_and_commits_output_over_the_real_gateway() {
    let fixture = Fixture::new("backend");
    let backend = RemoteSessionBackend::new(Arc::new(fixture.connection()));

    let bootstrap = backend.bootstrap().unwrap();
    assert_eq!(bootstrap.snapshot.sessions.len(), 1);
    assert_eq!(bootstrap.snapshot.sessions[0].title, "SSH Controller");
    let initial = backend
        .poll_output("s1", RemoteOutputPollOptions::default())
        .unwrap();
    assert_eq!(initial.bytes(), b"hello");
    assert!(initial.reset_required());
    assert_eq!(backend.committed_output_offset("s1"), None);
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
    assert!(!next.reset_required());
    next.commit().unwrap();
    assert_eq!(backend.committed_output_offset("s1"), Some(11));
    backend.mark_session_read("s1").unwrap();
    assert!(fixture
        .host_home
        .join("app-sessions")
        .join("s1")
        .join("read.json")
        .is_file());
    assert_eq!(fixture.count(), 1);

    backend.disconnect();
    fixture.cleanup();
}

#[test]
fn remote_scope_process_uses_only_host_state() {
    if std::env::var_os(PURITY_CHILD_ENV).is_some() {
        assert_controller_home_is_empty();
        let ssh_program =
            PathBuf::from(std::env::var_os(PURITY_SSH_ENV).expect("purity child fake SSH program"));
        let connection = SshHostConnection::with_ssh_program(
            SshTarget::parse("ssh://studio").unwrap(),
            ssh_program,
        )
        .unwrap();
        let backend = RemoteSessionBackend::new(Arc::new(connection));

        let bootstrap = backend.bootstrap().unwrap();
        assert_eq!(bootstrap.snapshot.sessions[0].title, "SSH Controller");
        assert_controller_home_is_empty();
        backend
            .poll_output("s1", RemoteOutputPollOptions::default())
            .unwrap()
            .commit()
            .unwrap();
        assert_controller_home_is_empty();
        backend.write_terminal("s1", "remote only").unwrap();
        assert_controller_home_is_empty();
        backend
            .resize_desktop(
                "s1",
                RemoteDesktopResize::Fit {
                    columns: 80,
                    rows: 24,
                },
            )
            .unwrap();
        assert_controller_home_is_empty();
        backend.mark_session_read("s1").unwrap();
        assert_controller_home_is_empty();
        backend.disconnect();
        assert_controller_home_is_empty();
        return;
    }

    let fixture = Fixture::new("purity");
    let controller_home = fixture.root.join("controller-user");
    let controller_unpeel_home = controller_home.join(".unpeel");
    let controller_tmp = fixture.root.join("controller-tmp");
    std::fs::create_dir_all(&controller_home).unwrap();
    std::fs::create_dir_all(&controller_tmp).unwrap();

    let socket_path = fixture
        .host_home
        .join("app-sessions")
        .join("s1")
        .join("session.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    // This synthetic Host uses the parent test process plus its fake control
    // socket as the live Session. Make that durable state truthful before the
    // child bootstraps so pre-dispatch validation permits the write/resize.
    let manifest_path = socket_path.with_file_name("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["state"] = serde_json::Value::String("running".into());
    manifest["pid"] = serde_json::Value::from(std::process::id());
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let listener_stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&listener_stop);
    let (commands_tx, commands_rx) = std::sync::mpsc::channel();
    let socket_thread = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        continue;
                    }
                    commands_tx
                        .send(serde_json::from_str::<serde_json::Value>(line.trim()).unwrap())
                        .unwrap();
                    reader
                        .get_mut()
                        .write_all(b"{\"ok\":true,\"error\":null}\n")
                        .unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("purity session socket failed: {error}"),
            }
        }
    });

    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(PURITY_TEST_NAME)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env("HOME", &controller_home)
        .env("UNPEEL_HOME", &controller_unpeel_home)
        .env("TMPDIR", &controller_tmp)
        .env(PURITY_CHILD_ENV, "1")
        .env(PURITY_SSH_ENV, &fixture.ssh_program)
        .output()
        .unwrap();
    listener_stop.store(true, Ordering::Release);
    socket_thread.join().unwrap();
    assert!(
        output.status.success(),
        "purity child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let commands: Vec<_> = commands_rx
        .try_iter()
        .filter(|command| command["type"] != "ping")
        .collect();
    assert_eq!(
        commands,
        [
            serde_json::json!({ "type": "write", "data": "remote only" }),
            serde_json::json!({ "type": "resize", "cols": 80, "rows": 24 }),
        ]
    );
    assert_eq!(fixture.count(), 1);
    assert!(fixture
        .host_home
        .join("app-sessions")
        .join("s1")
        .join("read.json")
        .is_file());
    assert!(
        std::fs::read_dir(&controller_home)
            .unwrap()
            .next()
            .is_none(),
        "Controller HOME was not left empty"
    );
    assert!(!controller_unpeel_home.exists());

    fixture.cleanup();
}

#[test]
fn structured_ssh_argv_and_out_of_order_responses() {
    let fixture = Fixture::new("argv");
    let connection = Arc::new(fixture.connection());

    let bootstrap = request(&connection, bootstrap_call(), Duration::from_secs(5)).unwrap();
    assert_eq!((bootstrap.request_id, bootstrap.status), (1, 200));
    assert_eq!(
        body_json(&bootstrap)["sessions"][0]["title"],
        "SSH Controller"
    );

    let arguments: Vec<String> = std::fs::read(&fixture.arguments)
        .unwrap()
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8(argument.to_vec()).unwrap())
        .collect();
    assert_eq!(
        arguments,
        [
            "-T",
            "-a",
            "-x",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "NumberOfPasswordPrompts=1",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "ForwardX11Trusted=no",
            "-o",
            "GSSAPIDelegateCredentials=no",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "EscapeChar=none",
            "-o",
            "RemoteCommand=none",
            "-o",
            "StdinNull=no",
            "--",
            "studio",
            "env",
            "UNPEEL_LOCAL_GATEWAY=1",
            "unpeel-host",
            "__remote_stdio__",
        ]
    );

    let waiting_call = connection.prepare(output_call(Some(5), 5_000)).unwrap();
    let waiting_connection = Arc::clone(&connection);
    let waiting = std::thread::spawn(move || {
        waiting_connection
            .request(waiting_call, Duration::from_secs(5))
            .unwrap()
    });
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let second_bootstrap = request(&connection, bootstrap_call(), Duration::from_secs(5)).unwrap();
    assert_eq!(
        (second_bootstrap.request_id, second_bootstrap.status),
        (3, 200)
    );
    assert_eq!(second_bootstrap.generation, bootstrap.generation);
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(
        !waiting.is_finished(),
        "long-poll did not remain outstanding"
    );

    std::fs::write(
        fixture
            .host_home
            .join("app-sessions")
            .join("s1")
            .join("output.bin"),
        b"hello world",
    )
    .unwrap();
    let output = waiting.join().unwrap();
    assert_eq!((output.request_id, output.status), (2, 200));
    assert_eq!(output.generation, bootstrap.generation);
    assert_eq!(body_json(&output)["nextOffset"], 11);
    assert_eq!(body_json(&output)["dataBase64"], "IHdvcmxk");
    assert_eq!(fixture.count(), 1);

    let prepared_before_close = connection.prepare(bootstrap_call()).unwrap();
    connection.disconnect();
    assert!(matches!(
        connection
            .request(prepared_before_close, Duration::from_secs(1))
            .unwrap_err(),
        HostConnectionError::ClosedRequest(4)
    ));
    assert_eq!(
        connection.prepare(bootstrap_call()).unwrap_err(),
        HostConnectionError::Closed
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        fixture.count(),
        1,
        "closed connection spawned a new SSH child"
    );
    drop(connection);
    fixture.cleanup();
}

#[test]
fn interactive_shell_compatibility_enters_the_same_gateway_protocol() {
    let fixture = Fixture::new("interactive");
    let host_binary = Path::new(env!("CARGO_BIN_EXE_unpeel-host"));
    let binary_dir = host_binary.parent().unwrap();
    fixture.replace_ssh_program(&format!(
        "#!/bin/sh\nexport HOME={}\nexport UNPEEL_HOME={}\nexport PATH={}:\"$PATH\"\nexec /bin/sh\n",
        shell_quote(&fixture.root.join("host-user")),
        shell_quote(&fixture.host_home),
        shell_quote(binary_dir),
    ));
    std::fs::create_dir_all(fixture.root.join("host-user")).unwrap();
    let connection = SshHostConnection::with_options_and_ssh_program(
        SshTarget::parse("ssh://managed-shell").unwrap(),
        SshConnectionOptions {
            launch_mode: SshLaunchMode::InteractiveShell,
            askpass: None,
        },
        &fixture.ssh_program,
    )
    .unwrap();
    let backend = RemoteSessionBackend::new(Arc::new(connection));

    let bootstrap = backend.bootstrap().unwrap();
    assert_eq!(bootstrap.snapshot.sessions.len(), 1);
    assert_eq!(bootstrap.snapshot.sessions[0].title, "SSH Controller");
    backend.disconnect();
    fixture.cleanup();
}

#[test]
fn request_deadline_interrupts_a_peer_that_never_reads_stdin() {
    let fixture = Fixture::new("blocked-write");
    fixture.replace_ssh_program("#!/bin/sh\nexec /bin/sleep 30\n");
    let connection = fixture.connection();
    // Large enough to fill a normal pipe, but still below the framed request
    // ceiling after base64/JSON encoding.
    let call = HostCall::new("POST", "/mobile/write", RequestSemantics::Effect)
        .with_body("application/octet-stream", vec![b'x'; 300 * 1024]);
    let started = Instant::now();
    let error = request(&connection, call, Duration::from_secs(1)).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5), "{error:?}");
    assert!(matches!(
        error,
        HostConnectionError::TimedOut {
            semantics: RequestSemantics::Effect,
            delivery: DeliveryState::OutcomeUnknown,
            ..
        }
    ));

    connection.disconnect();
    fixture.cleanup();
}

#[test]
fn process_loss_requires_explicit_bootstrap_and_cursor_resume() {
    let fixture = Fixture::new("resume");
    let connection = Arc::new(fixture.connection());

    let bootstrap = request(&connection, bootstrap_call(), Duration::from_secs(5)).unwrap();
    assert_eq!(bootstrap.request_id, 1);
    let initial = request(&connection, output_call(None, 0), Duration::from_secs(5)).unwrap();
    assert_eq!(initial.generation, bootstrap.generation);
    assert_eq!(body_json(&initial)["nextOffset"], 5);

    let waiting_call = connection.prepare(output_call(Some(5), 5_000)).unwrap();
    let waiting_connection = Arc::clone(&connection);
    let waiting = std::thread::spawn(move || {
        waiting_connection.request(waiting_call, Duration::from_secs(10))
    });
    std::thread::sleep(Duration::from_millis(100));
    fixture.kill_gateway();
    let lost = waiting.join().unwrap().unwrap_err();
    assert!(matches!(
        lost,
        HostConnectionError::Disconnected {
            request_id: 3,
            semantics: RequestSemantics::ReadOnly,
            delivery: DeliveryState::OutcomeUnknown,
            ..
        }
    ));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fixture.count(), 1, "transport retried a lost read itself");

    std::fs::write(
        fixture
            .host_home
            .join("app-sessions")
            .join("s1")
            .join("output.bin"),
        b"hello world",
    )
    .unwrap();
    let resynced = request(&connection, bootstrap_call(), Duration::from_secs(5)).unwrap();
    assert_eq!(resynced.request_id, 4);
    assert_ne!(resynced.generation, bootstrap.generation);
    let resumed = request(&connection, output_call(Some(5), 0), Duration::from_secs(5)).unwrap();
    assert_eq!(resumed.request_id, 5);
    assert_eq!(resumed.generation, resynced.generation);
    assert_eq!(body_json(&resumed)["dataBase64"], "IHdvcmxk");
    assert_eq!(body_json(&resumed)["nextOffset"], 11);
    assert_eq!(fixture.count(), 2);

    connection.disconnect();
    drop(connection);
    fixture.cleanup();
}

#[test]
fn generation_bound_effect_never_reconnects_after_idle_process_loss() {
    let fixture = Fixture::new("generation");
    let connection = fixture.connection();

    let bootstrap = request(&connection, bootstrap_call(), Duration::from_secs(5)).unwrap();
    let first_generation = bootstrap.generation;
    let prepared_before_loss = connection
        .prepare_in_generation(first_generation, mark_read_call())
        .unwrap();
    fixture.kill_gateway();

    // Wait until the stdout reader observes process death without sending a
    // probe request. Preparing an exact-generation call is local and cannot
    // spawn or write to SSH.
    let deadline = Instant::now() + Duration::from_secs(5);
    let changed = loop {
        match connection.prepare_in_generation(first_generation, mark_read_call()) {
            Err(error @ HostConnectionError::GenerationChanged { .. }) => break error,
            Ok(prepared) => {
                drop(prepared);
                assert!(Instant::now() < deadline, "SSH generation stayed live");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("unexpected generation preparation error: {error:?}"),
        }
    };
    assert_eq!(changed.delivery(), Some(DeliveryState::NotSent));
    assert!(matches!(
        &changed,
        HostConnectionError::GenerationChanged {
            expected,
            ..
        } if *expected == first_generation
    ));
    assert_eq!(
        fixture.count(),
        1,
        "generation-bound call opened a replacement SSH process"
    );

    let prepared_error = connection
        .request(prepared_before_loss, Duration::from_secs(5))
        .unwrap_err();
    assert!(matches!(
        &prepared_error,
        HostConnectionError::GenerationChanged { expected, .. }
            if *expected == first_generation
    ));
    assert_eq!(prepared_error.delivery(), Some(DeliveryState::NotSent));
    assert_eq!(
        fixture.count(),
        1,
        "a call prepared before process loss opened a replacement SSH process"
    );

    let recovered = request(&connection, bootstrap_call(), Duration::from_secs(5)).unwrap();
    assert_ne!(recovered.generation, first_generation);
    assert_eq!(fixture.count(), 2);

    let stale = connection
        .prepare_in_generation(first_generation, mark_read_call())
        .unwrap_err();
    assert!(matches!(
        &stale,
        HostConnectionError::GenerationChanged {
            expected,
            ..
        } if *expected == first_generation
    ));
    assert_eq!(stale.delivery(), Some(DeliveryState::NotSent));
    assert_eq!(fixture.count(), 2);

    let mark_read = connection
        .prepare_in_generation(recovered.generation, mark_read_call())
        .unwrap();
    let receipt = connection
        .request(mark_read, Duration::from_secs(5))
        .unwrap();
    assert_eq!(receipt.status, 200);
    assert_eq!(receipt.generation, recovered.generation);
    assert_eq!(body_json(&receipt)["ok"], true);
    assert_eq!(fixture.count(), 2);

    connection.disconnect();
    fixture.cleanup();
}

#[test]
fn ambiguous_backend_write_reaches_host_once_and_is_never_replayed() {
    let fixture = Fixture::new("effect");
    let socket_path = fixture
        .host_home
        .join("app-sessions")
        .join("s1")
        .join("session.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let write_count = Arc::new(AtomicUsize::new(0));
    let listener_stop = Arc::clone(&stop);
    let listener_count = Arc::clone(&write_count);
    let pid_file = fixture.gateway_pid.clone();
    let socket_thread = std::thread::spawn(move || {
        let mut held_connections = Vec::new();
        while !listener_stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) > 0 && line.contains("once") {
                        let seen = listener_count.fetch_add(1, Ordering::AcqRel) + 1;
                        if seen == 1 {
                            assert!(wait_until(Duration::from_secs(5), || pid_file.is_file()));
                            let pid = std::fs::read_to_string(&pid_file).unwrap();
                            let status = Command::new("/bin/kill")
                                .arg("-KILL")
                                .arg(pid.trim())
                                .status()
                                .unwrap();
                            assert!(status.success());
                        }
                    }
                    // Keep the socket open without acknowledging the command:
                    // the PTY write boundary was crossed, but no tunnel receipt
                    // can be produced before the gateway is killed.
                    held_connections.push(reader.into_inner());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("fake session socket failed: {error}"),
            }
        }
    });

    let connection = fixture.connection();
    let backend = RemoteSessionBackend::new(Arc::new(connection));
    backend.bootstrap().unwrap();
    // The shared router proves 404/409 before dispatch from manifest state.
    // Mark this synthetic socket live only after bootstrap so the test reaches
    // the deliberately ambiguous post-dispatch/lost-acknowledgement boundary.
    let manifest_path = socket_path.with_file_name("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["state"] = serde_json::Value::String("running".into());
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let failure = backend.write_terminal("s1", "once").unwrap_err();
    assert_eq!(failure.kind(), RemoteEffectFailureKind::OutcomeUnknown);
    assert!(backend.needs_bootstrap());
    assert_eq!(write_count.load(Ordering::Acquire), 1);
    assert_eq!(
        fixture.count(),
        1,
        "effect was retried after a lost receipt"
    );

    let bootstrap = backend.bootstrap().unwrap();
    assert_eq!(bootstrap.snapshot.sessions.len(), 1);
    assert!(!backend.needs_bootstrap());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(write_count.load(Ordering::Acquire), 1);
    assert_eq!(fixture.count(), 2);

    backend.disconnect();
    stop.store(true, Ordering::Release);
    socket_thread.join().unwrap();
    fixture.cleanup();
}
