use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use unpeel_core::relay_wire::{self, TunnelRequest};
use unpeel_core::remote_stdio;

struct ServiceProcess {
    child: Child,
    root: PathBuf,
    homes: Vec<PathBuf>,
}

impl ServiceProcess {
    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            if !wait_until(Duration::from_secs(10), || {
                self.child.try_wait().ok().flatten().is_some()
            }) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        // A test panic may interrupt the supervisor before its graceful
        // worker teardown. Signal only pids read from this generated fixture's
        // private homes, then let remove_dir_all reclaim the private root.
        for home in &self.homes {
            let pid = std::fs::read(home.join("serve.json"))
                .ok()
                .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                .and_then(|status| status.get("pid").and_then(|pid| pid.as_i64()));
            if let Some(pid) = pid.filter(|pid| *pid > 1) {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        self.stop();
        // The worker detaches one PTY core per workspace home; removing the
        // home without shutting them down leaks a core per run.
        unpeel_core::pty_core::shutdown_cores_under(&self.root, std::time::Duration::from_secs(15));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture_root() -> PathBuf {
    // AF_UNIX paths are short on macOS. Keep this real-process fixture under
    // an explicit compact, UUID-owned root rather than the long per-user
    // Darwin temporary directory.
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    PathBuf::from("/tmp").join(format!("upsvc-{}", &suffix[..12]))
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

fn bootstrap_over_socket(home: &Path) -> (u16, serde_json::Value) {
    let mut stream = UnixStream::connect(remote_stdio::local_host_socket_path(home)).unwrap();
    let request = TunnelRequest {
        id: 1,
        method: "GET".into(),
        path: "/mobile/bootstrap".into(),
        query: Vec::new(),
        auth: None,
        content_type: None,
        body: Vec::new(),
    };
    remote_stdio::write_frame(
        &mut stream,
        remote_stdio::FRAME_KIND_REQUEST,
        &relay_wire::encode_tunnel_request(&request),
    )
    .unwrap();
    let frame = remote_stdio::read_frame(&mut stream).unwrap().unwrap();
    assert_eq!(frame.kind, remote_stdio::FRAME_KIND_RESPONSE);
    let response = relay_wire::parse_tunnel_response(&frame.payload).unwrap();
    (
        response.status,
        serde_json::from_slice(&response.body).unwrap(),
    )
}

#[test]
fn one_service_supervises_and_serves_every_registered_workspace() {
    let root = fixture_root();
    let real_home = root.join(".unpeel");
    let writing = real_home.join("profiles/writing");
    let research = real_home.join("profiles/research");
    std::fs::create_dir_all(&writing).unwrap();
    std::fs::create_dir_all(&research).unwrap();
    let registry = serde_json::json!({
        "version": 1,
        "profiles": [
            {"id":"writing-id","name":"Writing","home":writing,"createdAt":1},
            {"id":"research-id","name":"Research","home":research,"createdAt":2}
        ]
    });
    std::fs::write(
        real_home.join("profiles.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_unpeel"))
        .arg("serve")
        .env("HOME", &root)
        .env_remove("UNPEEL_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start unified Host service");
    let homes = vec![real_home.clone(), writing.clone(), research.clone()];
    let mut process = ServiceProcess {
        child,
        root,
        homes: homes.clone(),
    };

    let status_path = real_home.join("host-service.json");
    assert!(
        wait_until(Duration::from_secs(15), || {
            std::fs::read(&status_path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
                .and_then(|status| status["workspaces"].as_array().cloned())
                .is_some_and(|workspaces| {
                    workspaces.len() == 3
                        && workspaces.iter().all(|workspace| {
                            workspace["state"] == "running"
                                && workspace["serve"]["localSocket"].is_string()
                        })
                })
        }),
        "service never reported all workspace workers"
    );
    assert!(
        wait_until(Duration::from_secs(5), || {
            std::fs::read_to_string(real_home.join("hooks/trace.log")).is_ok_and(|trace| {
                trace.contains("host-service Unpeel Host service started")
                    && trace.contains("host-worker Unpeel Host serving")
            })
        }),
        "app-style null stdio left no durable Host diagnostics"
    );

    for home in &homes {
        let (status, bootstrap) = bootstrap_over_socket(home);
        assert_eq!(status, 200, "{} did not answer", home.display());
        assert_eq!(bootstrap["hostProtocol"]["majorVersion"], 1);
        assert!(bootstrap["hostProtocol"]["capabilities"].is_array());
    }

    let duplicate = Command::new(env!("CARGO_BIN_EXE_unpeel"))
        .arg("serve")
        .env("HOME", process.root.as_path())
        .env_remove("UNPEEL_HOME")
        .output()
        .expect("run duplicate unified service");
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("Host service is already running"));

    process.stop();
    assert!(!status_path.exists());
    for home in &homes {
        assert!(!home.join("serve.json").exists());
        assert!(!remote_stdio::local_host_socket_path(home).exists());
    }
}
