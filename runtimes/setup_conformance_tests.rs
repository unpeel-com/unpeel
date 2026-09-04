#[cfg(test)]
mod tests {
    use super::{
        kiro_mcp_server_value, AMP_PLUGIN_SCRIPT, CLAUDE_HOOK_SCRIPT, CLINE_HOOK_SCRIPT,
        CODEX_NOTIFY_NORMALIZER_SCRIPT, COPILOT_HOOK_SCRIPT, CURSOR_HOOK_SCRIPT,
        GEMINI_HOOK_SCRIPT, KIMI_HOOK_SCRIPT, KIRO_HOOK_SCRIPT, NOTIFY_HOOK_SCRIPT,
        OPENCODE_PLUGIN_SCRIPT,
    };
    use serde_json::{json, Value};
    use std::fs;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus, Stdio};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    struct CaptureServer {
        port: u16,
        events: Arc<Mutex<Vec<Value>>>,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl CaptureServer {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind capture server");
            listener
                .set_nonblocking(true)
                .expect("set capture server nonblocking");
            let port = listener.local_addr().expect("capture addr").port();
            let events = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let events_for_thread = Arc::clone(&events);
            let stop_for_thread = Arc::clone(&stop);
            let handle = thread::spawn(move || {
                while !stop_for_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // BSD/macOS accepted sockets inherit the
                            // listener's O_NONBLOCK; a non-blocking read here
                            // races the request body and drops the post.
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let mut reader = BufReader::new(&stream);
                            let mut request_line = String::new();
                            if reader.read_line(&mut request_line).is_err() {
                                continue;
                            }

                            let mut content_length = 0usize;
                            loop {
                                let mut line = String::new();
                                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                                    break;
                                }
                                if let Some(value) = line
                                    .to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().to_string())
                                {
                                    content_length = value.parse().unwrap_or(0);
                                }
                            }

                            let mut body = vec![0u8; content_length];
                            if content_length > 0 && reader.read_exact(&mut body).is_err() {
                                continue;
                            }
                            if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                                events_for_thread.lock().unwrap().push(value);
                            }

                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{{\"ok\":true}}"
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                port,
                events,
                stop,
                handle: Some(handle),
            }
        }

        fn wait_for_event(&self, expected: &str, timeout: Duration) -> bool {
            self.wait_for_event_payload(expected, timeout).is_some()
        }

        fn wait_for_event_payload(&self, expected: &str, timeout: Duration) -> Option<Value> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(event) = self.events.lock().unwrap().iter().find(|value| {
                    value
                        .get("hook_event_name")
                        .and_then(|field| field.as_str())
                        == Some(expected)
                }) {
                    return Some(event.clone());
                }
                thread::sleep(Duration::from_millis(50));
            }
            None
        }

        fn event_names(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|value| {
                    value
                        .get("hook_event_name")
                        .and_then(|field| field.as_str())
                        .map(ToString::to_string)
                })
                .collect()
        }

        fn events_snapshot(&self) -> Vec<Value> {
            self.events.lock().unwrap().clone()
        }
    }

    impl Drop for CaptureServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repo root")
            .to_path_buf()
    }

    fn temp_path(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "unpeel-live-cli-{}-{}-{}",
            label,
            std::process::id(),
            crate::state::current_timestamp_ms()
        ));
        fs::create_dir_all(&root).expect("create temp dir");
        root
    }

    #[cfg(unix)]
    #[test]
    fn project_hook_write_refuses_committed_symlink() {
        let project = temp_path("symlink-repo");
        let outside = temp_path("symlink-outside");
        let secret = outside.join("victim.txt");
        fs::write(&secret, "original secret\n").expect("seed victim file");

        // A malicious repo commits a symlink at the hook directory location
        // pointing at a directory outside the repo.
        let github = project.join(".github");
        fs::create_dir_all(&github).expect("create .github");
        std::os::unix::fs::symlink(&outside, github.join("hooks")).expect("plant symlink dir");

        let result = super::write_project_file_no_symlinks(
            &project,
            Path::new(".github/hooks/victim.txt"),
            b"overwritten by unpeel\n",
        );
        assert!(result.is_err(), "expected symlink traversal to be refused");
        assert_eq!(
            fs::read_to_string(&secret).unwrap(),
            "original secret\n",
            "file outside the repo must not be overwritten"
        );

        // A symlink at the leaf itself is also refused.
        let safe_dir = project.join(".amp").join("plugins");
        fs::create_dir_all(&safe_dir).expect("create plugins dir");
        std::os::unix::fs::symlink(&secret, safe_dir.join("unpeel-notify.js"))
            .expect("plant symlink leaf");
        let leaf = super::write_project_file_no_symlinks(
            &project,
            Path::new(".amp/plugins/unpeel-notify.js"),
            b"overwritten\n",
        );
        assert!(leaf.is_err(), "expected leaf symlink to be refused");
        assert_eq!(fs::read_to_string(&secret).unwrap(), "original secret\n");

        // A hard link at the leaf must be replaced as a directory entry, not
        // opened and truncated. The outside inode keeps its original bytes.
        let hardlink_project = temp_path("hardlink-hook-repo");
        let hardlink_dir = hardlink_project.join(".amp").join("plugins");
        fs::create_dir_all(&hardlink_dir).expect("create hardlink plugins dir");
        let hardlink_leaf = hardlink_dir.join("unpeel-notify.js");
        fs::hard_link(&secret, &hardlink_leaf).expect("plant hardlink leaf");
        super::write_project_file_no_symlinks(
            &hardlink_project,
            Path::new(".amp/plugins/unpeel-notify.js"),
            b"installed hook\n",
        )
        .expect("hardlink leaf should be safely replaced");
        assert_eq!(fs::read_to_string(&secret).unwrap(), "original secret\n");
        assert_eq!(
            fs::read_to_string(&hardlink_leaf).unwrap(),
            "installed hook\n"
        );

        // The best-effort git exclude update uses the same anchored writer;
        // a repo-controlled `.git` symlink must not turn that secondary write
        // into an overwrite outside the project either.
        let exclude_project = temp_path("symlink-exclude-repo");
        let outside_info = outside.join("info");
        fs::create_dir_all(&outside_info).expect("create outside git info dir");
        let outside_exclude = outside_info.join("exclude");
        fs::write(&outside_exclude, "outside exclude sentinel\n").expect("seed outside exclude");
        std::os::unix::fs::symlink(&outside, exclude_project.join(".git"))
            .expect("plant .git symlink");
        super::ensure_project_exclude_entry(&exclude_project.to_string_lossy(), "victim.txt");
        assert_eq!(
            fs::read_to_string(&outside_exclude).unwrap(),
            "outside exclude sentinel\n",
            "the exclude writer must not traverse a symlinked .git directory"
        );

        // The exclude leaf itself may also be a repo-controlled symlink. Its
        // read must fail closed before the append path has any chance to
        // replace or truncate the file outside the project.
        let symlink_exclude_project = temp_path("symlink-exclude-leaf-repo");
        let symlink_git_info = symlink_exclude_project.join(".git").join("info");
        fs::create_dir_all(&symlink_git_info).expect("create symlink git info dir");
        let symlink_exclude = symlink_git_info.join("exclude");
        std::os::unix::fs::symlink(&outside_exclude, &symlink_exclude)
            .expect("plant symlinked exclude leaf");
        super::ensure_project_exclude_entry(
            &symlink_exclude_project.to_string_lossy(),
            "generated-hook.json",
        );
        assert_eq!(
            fs::read_to_string(&outside_exclude).unwrap(),
            "outside exclude sentinel\n",
            "the exclude writer must not follow a symlinked exclude file"
        );
        assert_eq!(
            fs::read_link(&symlink_exclude).unwrap(),
            outside_exclude,
            "a refused exclude symlink must remain untouched"
        );

        // A hard-linked exclude file is safe for the same reason as a hook
        // leaf: read its current content, then atomically replace only the
        // repository's directory entry.
        let hardlink_exclude_project = temp_path("hardlink-exclude-repo");
        let git_info = hardlink_exclude_project.join(".git").join("info");
        fs::create_dir_all(&git_info).expect("create git info dir");
        let linked_exclude = git_info.join("exclude");
        fs::hard_link(&secret, &linked_exclude).expect("plant hardlinked exclude");
        super::ensure_project_exclude_entry(
            &hardlink_exclude_project.to_string_lossy(),
            "generated-hook.json",
        );
        assert_eq!(fs::read_to_string(&secret).unwrap(), "original secret\n");
        assert_eq!(
            fs::read_to_string(&linked_exclude).unwrap(),
            "original secret\ngenerated-hook.json\n"
        );

        // A clean repo path still installs normally.
        super::write_project_file_no_symlinks(
            &project,
            Path::new(".config/hooks/unpeel.json"),
            b"{}\n",
        )
        .expect("clean install should succeed");
        assert_eq!(
            fs::read_to_string(project.join(".config/hooks/unpeel.json")).unwrap(),
            "{}\n"
        );
    }

    fn write_temp_hook_script(label: &str, script: &str) -> PathBuf {
        let root = temp_path(label);
        let path = root.join(format!("{label}.sh"));
        super::write_executable_script(&path, script, label).expect("write hook script");
        path
    }

    fn write_temp_codex_notify_hook(label: &str) -> PathBuf {
        let root = temp_path(label);
        let transport_path = root.join("notify-transport.sh");
        super::write_executable_script(
            &transport_path,
            super::NOTIFY_HOOK_SCRIPT,
            "generic notify transport",
        )
        .expect("write generic notify transport");
        let normalizer_path = root.join("codex-notify.sh");
        let normalizer = super::CODEX_NOTIFY_NORMALIZER_SCRIPT.replace(
            "{{NOTIFY_PATH}}",
            transport_path.to_string_lossy().as_ref(),
        );
        super::write_executable_script(&normalizer_path, &normalizer, "Codex notify normalizer")
            .expect("write Codex notify normalizer");
        normalizer_path
    }

    fn hook_env_home(label: &str) -> PathBuf {
        temp_path(&format!("{label}-home"))
    }

    fn hook_trace_file(label: &str) -> PathBuf {
        temp_path(&format!("{label}-trace")).join("trace.log")
    }

    fn run_hook_script_with_stdin(
        script: &PathBuf,
        args: &[&str],
        input: &str,
        capture: &CaptureServer,
        label: &str,
    ) -> std::process::Output {
        let mut command = Command::new("bash");
        command
            .arg(script)
            .args(args)
            .env("HOME", hook_env_home(label))
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("UNPEEL_RUNTIME_GENERATION", "7")
            .env("UNPEEL_HOOK_TRACE_FILE", hook_trace_file(label))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn hook script");
        child
            .stdin
            .take()
            .expect("hook stdin")
            .write_all(input.as_bytes())
            .expect("write hook stdin");
        child.wait_with_output().expect("wait for hook script")
    }

    fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<ExitStatus, String> {
        let mut child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn failed: {e}"))?;

        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("command timed out".to_string());
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("try_wait failed: {e}")),
            }
        }
    }

    fn real_command_path(name: &str) -> Option<String> {
        let path = std::env::var("PATH").ok()?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    std::fs::metadata(candidate)
                        .map(|metadata| {
                            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                        })
                        .unwrap_or(false)
                }

                #[cfg(not(unix))]
                {
                    candidate.is_file()
                }
            })
            .map(|path| path.to_string_lossy().to_string())
    }

    #[test]
    fn codex_notify_normalizer_maps_provider_events_before_generic_transport() {
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("agent-turn-complete"));
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("task_complete"));
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("turn_aborted"));
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("request_permissions"));
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("apply_patch_approval_request"));
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("EVENT_TYPE=\"Stop\""));
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("EVENT_TYPE=\"PermissionRequest\""));
        assert!(CODEX_NOTIFY_NORMALIZER_SCRIPT.contains("{{NOTIFY_PATH}}"));
        assert!(!NOTIFY_HOOK_SCRIPT.contains("agent-turn-complete"));
        assert!(NOTIFY_HOOK_SCRIPT.contains("UNPEEL_PORT_REGISTRY_FILE"));
        assert!(NOTIFY_HOOK_SCRIPT.contains("current_unpeel_ports"));
        assert!(NOTIFY_HOOK_SCRIPT.contains("post_hook_payload_to_current_ports"));
    }

    #[test]
    fn notify_hook_script_preserves_codex_session_and_transcript_for_busy_events() {
        let capture = CaptureServer::start();
        let script = write_temp_codex_notify_hook("notify-busy");
        let payload = json!({
            "type": "task_started",
            "session_id": "codex-provider-session",
            "transcript_path": "/tmp/codex-provider-session.jsonl",
            "cwd": "/tmp/project"
        })
        .to_string();

        let output = Command::new("bash")
            .arg(script)
            .arg(&payload)
            .env("HOME", hook_env_home("notify-busy"))
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("UNPEEL_HOOK_POST_SYNC", "1")
            .env("UNPEEL_HOOK_TRACE_FILE", hook_trace_file("notify-busy"))
            .output()
            .expect("run notify hook");

        assert!(output.status.success(), "notify hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("Start", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("expected Start event, got {:?}", capture.events_snapshot()));
        assert_eq!(
            event.get("type").and_then(Value::as_str),
            Some("task_started")
        );
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("codex-provider-session")
        );
        assert_eq!(
            event.get("transcript_path").and_then(Value::as_str),
            Some("/tmp/codex-provider-session.jsonl")
        );
        assert_eq!(
            event.get("cwd").and_then(Value::as_str),
            Some("/tmp/project")
        );
    }

    #[test]
    fn notify_hook_script_preserves_codex_metadata_for_permission_events() {
        let capture = CaptureServer::start();
        let script = write_temp_codex_notify_hook("notify-permission");
        let payload = json!({
            "type": "apply_patch_approval_request",
            "session_id": "codex-approval-session",
            "transcript_path": "/tmp/codex-approval-session.jsonl",
            "tool_name": "apply_patch"
        })
        .to_string();

        let output = Command::new("bash")
            .arg(&script)
            .arg(&payload)
            .env("HOME", hook_env_home("notify-permission"))
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("UNPEEL_HOOK_POST_SYNC", "1")
            .env(
                "UNPEEL_HOOK_TRACE_FILE",
                hook_trace_file("notify-permission"),
            )
            .output()
            .expect("run notify hook");

        assert!(output.status.success(), "notify hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("PermissionRequest", Duration::from_secs(5))
            .unwrap_or_else(|| {
                panic!(
                    "expected PermissionRequest event, got {:?}",
                    capture.events_snapshot()
                )
            });
        assert_eq!(
            event.get("type").and_then(Value::as_str),
            Some("apply_patch_approval_request")
        );
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("codex-approval-session")
        );
        assert_eq!(
            event.get("transcript_path").and_then(Value::as_str),
            Some("/tmp/codex-approval-session.jsonl")
        );
        assert_eq!(
            event.get("tool_name").and_then(Value::as_str),
            Some("apply_patch")
        );
    }

    #[test]
    fn all_hook_scripts_record_last_hook_event() {
        for (label, script) in [
            ("claude", super::CLAUDE_HOOK_SCRIPT),
            ("notify", super::NOTIFY_HOOK_SCRIPT),
            ("gemini", super::GEMINI_HOOK_SCRIPT),
            ("kimi", super::KIMI_HOOK_SCRIPT),
            ("copilot", super::COPILOT_HOOK_SCRIPT),
            ("cursor", super::CURSOR_HOOK_SCRIPT),
            ("grok", super::GROK_HOOK_SCRIPT),
            ("muse", super::MUSE_HOOK_SCRIPT),
        ] {
            assert!(
                script.contains("record_last_hook_event() {"),
                "{label} hook script must define record_last_hook_event"
            );
            assert!(
                script.contains("record_last_hook_event \""),
                "{label} hook script must call record_last_hook_event"
            );
            assert!(
                script.contains("last-hook-event.json"),
                "{label} hook script must write last-hook-event.json"
            );
            assert!(
                script.contains("UNPEEL_SESSION_DIR"),
                "{label} hook script must honor UNPEEL_SESSION_DIR"
            );
        }
    }

    fn run_hook_script_recording(
        script_src: &str,
        label: &str,
        args: &[&str],
        input: &str,
        session_dir: &Path,
    ) -> std::process::Output {
        let script = write_temp_hook_script(label, script_src);
        run_hook_path_recording(&script, label, args, input, session_dir)
    }

    fn run_hook_path_recording(
        script: &Path,
        label: &str,
        args: &[&str],
        input: &str,
        session_dir: &Path,
    ) -> std::process::Output {
        let mut command = Command::new("bash");
        command
            .arg(script)
            .args(args)
            .env("HOME", hook_env_home(label))
            .env("UNPEEL_SESSION_ID", "unpeel-record-session")
            .env("UNPEEL_SESSION_DIR", session_dir)
            .env("UNPEEL_RUNTIME_GENERATION", "7")
            .env("UNPEEL_HOOK_TRACE_FILE", hook_trace_file(label))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn hook script");
        child
            .stdin
            .take()
            .expect("hook stdin")
            .write_all(input.as_bytes())
            .expect("write hook stdin");
        child.wait_with_output().expect("wait for hook script")
    }

    fn read_last_hook_event(session_dir: &Path) -> Value {
        let raw = fs::read_to_string(session_dir.join("last-hook-event.json"))
            .expect("read last-hook-event.json");
        serde_json::from_str(&raw).expect("parse last-hook-event.json")
    }

    #[test]
    fn every_owned_hook_reporter_tags_http_payload_and_durable_seed_generation() {
        struct Case {
            label: &'static str,
            script: &'static str,
            args: &'static [&'static str],
            input: &'static str,
        }

        let cases = [
            Case {
                label: "claude-generation",
                script: super::CLAUDE_HOOK_SCRIPT,
                args: &[],
                input: r#"{"hook_event_name":"Stop"}"#,
            },
            Case {
                label: "notify-generation",
                script: super::NOTIFY_HOOK_SCRIPT,
                args: &[r#"{"hook_event_name":"Stop"}"#],
                input: "",
            },
            Case {
                label: "gemini-generation",
                script: super::GEMINI_HOOK_SCRIPT,
                args: &[],
                input: r#"{"hook_event_name":"AfterAgent"}"#,
            },
            Case {
                label: "kimi-generation",
                script: super::KIMI_HOOK_SCRIPT,
                args: &["Stop"],
                input: "{}",
            },
            Case {
                label: "kiro-generation",
                script: super::KIRO_HOOK_SCRIPT,
                args: &["Stop"],
                input: "{}",
            },
            Case {
                label: "cline-generation",
                script: super::CLINE_HOOK_SCRIPT,
                args: &["TaskComplete", "{}"],
                input: "",
            },
            Case {
                label: "copilot-generation",
                script: super::COPILOT_HOOK_SCRIPT,
                args: &["sessionEnd"],
                input: "{}",
            },
            Case {
                label: "cursor-generation",
                script: super::CURSOR_HOOK_SCRIPT,
                args: &["Stop"],
                input: "{}",
            },
            Case {
                label: "grok-generation",
                script: super::GROK_HOOK_SCRIPT,
                args: &["Stop"],
                input: "{}",
            },
            Case {
                label: "muse-generation",
                script: super::MUSE_HOOK_SCRIPT,
                args: &[],
                input: r#"{"hook_event_name":"Stop"}"#,
            },
        ];

        for case in cases {
            let capture = CaptureServer::start();
            let session_dir = temp_path(&format!("{}-session", case.label));
            let script = write_temp_hook_script(case.label, case.script);
            let mut command = Command::new("bash");
            command
                .arg(&script)
                .args(case.args)
                .env("HOME", hook_env_home(case.label))
                .env("UNPEEL_APP_PORT", capture.port.to_string())
                .env("UNPEEL_SESSION_ID", "unpeel-generation-session")
                .env("UNPEEL_SESSION_DIR", &session_dir)
                .env("UNPEEL_RUNTIME_GENERATION", "42")
                .env("UNPEEL_HOOK_POST_SYNC", "1")
                .env(
                    "UNPEEL_APP_PORT_REGISTRY_FILE",
                    session_dir.join("no-extra-ports"),
                )
                .env("UNPEEL_HOOK_TRACE_FILE", hook_trace_file(case.label))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().expect("spawn generation hook reporter");
            child
                .stdin
                .take()
                .expect("hook stdin")
                .write_all(case.input.as_bytes())
                .expect("write generation hook stdin");
            let output = child.wait_with_output().expect("wait for hook reporter");
            assert!(
                output.status.success(),
                "{} reporter failed: {output:?}",
                case.label
            );

            let posted = capture
                .wait_for_event_payload("Stop", Duration::from_secs(5))
                .unwrap_or_else(|| {
                    panic!(
                        "{} did not post Stop; got {:?}",
                        case.label,
                        capture.events_snapshot()
                    )
                });
            assert_eq!(
                posted
                    .get("unpeel_runtime_generation")
                    .and_then(Value::as_u64),
                Some(42),
                "{} HTTP payload",
                case.label
            );
            let seed = read_last_hook_event(&session_dir);
            assert_eq!(
                seed.get("unpeel_runtime_generation")
                    .and_then(Value::as_u64),
                Some(42),
                "{} durable seed",
                case.label
            );
        }

        // OpenCode and Amp both invoke the Notify reporter above instead of
        // maintaining an independent HTTP/seed implementation.
        assert!(super::OPENCODE_PLUGIN_SCRIPT.contains("bash ${notifyPath}"));
        assert!(super::AMP_PLUGIN_SCRIPT.contains("Bun.spawn([\"bash\", notifyPath, body]"));
    }

    #[test]
    fn muse_plugin_manifest_registers_each_event_with_a_distinct_source() {
        let manifest: Value =
            serde_json::from_str(&super::muse_plugin_manifest_json().expect("manifest"))
                .expect("parse manifest");
        let hooks = manifest["capabilities"]["hooks"]
            .as_array()
            .expect("hooks array");
        let events: Vec<&str> = hooks
            .iter()
            .map(|hook| hook["event"].as_str().expect("event"))
            .collect();
        assert_eq!(
            events,
            [
                "SessionStart",
                "UserPromptSubmit",
                "Stop",
                "PermissionRequest"
            ]
        );
        // The muse plugin validator rejects two hooks sharing one source path.
        let sources: std::collections::HashSet<&str> = hooks
            .iter()
            .map(|hook| hook["command"][1].as_str().expect("source path"))
            .collect();
        assert_eq!(sources.len(), hooks.len());
        assert_eq!(manifest["name"], "unpeel");
        assert_eq!(manifest["compat"]["manifestDir"], ".muse-plugin");
    }

    #[test]
    fn muse_hook_script_records_stop_but_not_session_start() {
        let session_dir = temp_path("muse-record-session");
        let stop_payload = json!({
            "hook_event_name": "Stop",
            "session_id": "muse-provider-1",
            "last_assistant_message": "done"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::MUSE_HOOK_SCRIPT,
            "muse-record",
            &[],
            &stop_payload,
            &session_dir,
        );
        assert!(output.status.success(), "muse hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("Stop")
        );

        // SessionStart only latches metadata; it must not overwrite the seed.
        let start_payload = json!({
            "hook_event_name": "SessionStart",
            "session_id": "muse-provider-1",
            "source": "startup"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::MUSE_HOOK_SCRIPT,
            "muse-record-start",
            &[],
            &start_payload,
            &session_dir,
        );
        assert!(output.status.success(), "muse hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("Stop")
        );
    }

    #[test]
    fn muse_hook_script_drops_internal_reminder_permission_requests() {
        // Muse fires PermissionRequest for its internal reminder-decision
        // tool; the decision is auto-answered inside muse (no user prompt)
        // and can land after Stop, which used to latch attention forever.
        let session_dir = temp_path("muse-reminder-session");
        let stop_payload = json!({
            "hook_event_name": "Stop",
            "session_id": "muse-provider-2"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::MUSE_HOOK_SCRIPT,
            "muse-reminder-stop",
            &[],
            &stop_payload,
            &session_dir,
        );
        assert!(output.status.success(), "muse hook failed: {output:?}");

        let reminder_payload = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "submit_reminder_decision",
            "session_id": "muse-provider-2"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::MUSE_HOOK_SCRIPT,
            "muse-reminder-perm",
            &[],
            &reminder_payload,
            &session_dir,
        );
        assert!(output.status.success(), "muse hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("Stop")
        );

        // A genuine tool approval must still record attention.
        let bash_payload = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "bash",
            "session_id": "muse-provider-2"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::MUSE_HOOK_SCRIPT,
            "muse-reminder-bash",
            &[],
            &bash_payload,
            &session_dir,
        );
        assert!(output.status.success(), "muse hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("PermissionRequest")
        );
    }

    #[test]
    fn claude_hook_script_records_last_hook_event() {
        let session_dir = temp_path("claude-record-session");
        let payload = json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "provider-1"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::CLAUDE_HOOK_SCRIPT,
            "claude-record",
            &[],
            &payload,
            &session_dir,
        );
        assert!(output.status.success(), "claude hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("UserPromptSubmit")
        );
        assert!(event.get("tool_name").is_none());
    }

    #[test]
    fn claude_hook_script_ignores_grok_compat_session_start() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("claude-ignore-grok", super::CLAUDE_HOOK_SCRIPT);
        let output = Command::new("bash")
            .arg(&script)
            .env("HOME", hook_env_home("claude-ignore-grok"))
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("GROK_SESSION_ID", "grok-provider-session")
            .env(
                "UNPEEL_HOOK_TRACE_FILE",
                hook_trace_file("claude-ignore-grok"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn claude hook");
        let mut child = output;
        child
            .stdin
            .take()
            .expect("claude hook stdin")
            .write_all(br#"{"hookEventName":"session_start","sessionId":"grok-provider-session"}"#)
            .expect("write claude hook stdin");
        let finished = child.wait_with_output().expect("wait for claude hook");
        assert!(
            finished.status.success(),
            "claude hook failed: {finished:?}"
        );
        thread::sleep(Duration::from_millis(200));
        assert!(
            capture.events_snapshot().is_empty(),
            "Grok-compat SessionStart must not be posted as a Claude hook event, got {:?}",
            capture.events_snapshot()
        );
    }

    #[test]
    fn stale_tmp_claude_hooks_are_pruned() {
        let current = "/Users/me/.unpeel/hooks/claude-hooks.sh";
        assert!(!super::is_stale_unpeel_claude_hook(current, current));
        assert!(super::is_stale_unpeel_claude_hook(
            "/tmp/ur-c8eem28y/hooks/claude-hooks.sh",
            current
        ));
        assert!(!super::is_stale_unpeel_claude_hook(
            "/usr/bin/echo hello",
            current
        ));

        let mut array = vec![
            super::build_hook_entry("SessionStart", "/tmp/old/hooks/claude-hooks.sh"),
            super::build_hook_entry("SessionStart", current),
            json!({"hooks":[{"type":"command","command":"echo keep-me"}]}),
        ];
        assert!(super::prune_stale_unpeel_claude_hooks(&mut array, current));
        assert_eq!(array.len(), 2);
        assert_eq!(array[0]["hooks"][0]["command"].as_str(), Some(current));
        assert_eq!(
            array[1]["hooks"][0]["command"].as_str(),
            Some("echo keep-me")
        );
    }

    #[test]
    fn claude_hook_script_forwards_session_start_as_hook_seen() {
        // The settings hook subscribes SessionStart so an in-tool /resume or
        // /clear re-links the session to the new conversation id. The script
        // must forward it as HookSeen (metadata-only latch) and must not let
        // it overwrite the durable busy/idle seed.
        assert!(super::HOOK_EVENTS.contains(&"SessionStart"));
        assert!(super::CLAUDE_HOOK_SCRIPT.contains(r#""hook_event_name":"HookSeen""#));

        let session_dir = temp_path("claude-record-start");
        let stop_payload = json!({
            "hook_event_name": "Stop",
            "session_id": "provider-1"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::CLAUDE_HOOK_SCRIPT,
            "claude-record-stop-seed",
            &[],
            &stop_payload,
            &session_dir,
        );
        assert!(output.status.success(), "claude hook failed: {output:?}");

        let start_payload = json!({
            "hook_event_name": "SessionStart",
            "session_id": "provider-2-resumed",
            "source": "resume"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::CLAUDE_HOOK_SCRIPT,
            "claude-record-start",
            &[],
            &start_payload,
            &session_dir,
        );
        assert!(output.status.success(), "claude hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("Stop")
        );
    }

    #[test]
    fn claude_hook_script_records_permission_tool_name() {
        let session_dir = temp_path("claude-record-perm");
        let payload = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "AskUserQuestion"
        })
        .to_string();
        let output = run_hook_script_recording(
            super::CLAUDE_HOOK_SCRIPT,
            "claude-record-perm",
            &[],
            &payload,
            &session_dir,
        );
        assert!(output.status.success(), "claude hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("PermissionRequest")
        );
        assert_eq!(
            event.get("tool_name").and_then(Value::as_str),
            Some("AskUserQuestion")
        );
    }

    #[test]
    fn notify_hook_script_records_mapped_last_hook_event() {
        let session_dir = temp_path("notify-record-session");
        let payload = json!({ "type": "agent-turn-complete" }).to_string();
        let script = write_temp_codex_notify_hook("notify-record");
        let output = run_hook_path_recording(
            &script,
            "notify-record",
            &[&payload],
            "",
            &session_dir,
        );
        assert!(output.status.success(), "notify hook failed: {output:?}");
        let event = read_last_hook_event(&session_dir);
        assert_eq!(
            event.get("hook_event_name").and_then(Value::as_str),
            Some("Stop")
        );
    }

    #[test]
    fn record_last_hook_event_skips_missing_session_dir() {
        let missing_dir = temp_path("claude-record-missing").join("gone");
        let payload = json!({ "hook_event_name": "Stop" }).to_string();
        let output = run_hook_script_recording(
            super::CLAUDE_HOOK_SCRIPT,
            "claude-record-missing",
            &[],
            &payload,
            &missing_dir,
        );
        assert!(output.status.success(), "claude hook failed: {output:?}");
        assert!(
            !missing_dir.exists(),
            "record must never create the session dir"
        );
    }

    #[test]
    fn gemini_hook_script_preserves_provider_metadata_for_lifecycle_events() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("gemini-metadata", super::GEMINI_HOOK_SCRIPT);
        let payload = json!({
            "hook_event_name": "BeforeAgent",
            "session_id": "gemini-provider-session",
            "conversationId": "gemini-conversation",
            "transcript_path": "/tmp/gemini-provider-session.jsonl",
            "tool_name": "gemini"
        })
        .to_string();

        let output =
            run_hook_script_with_stdin(&script, &[], &payload, &capture, "gemini-metadata");

        assert!(output.status.success(), "gemini hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("Start", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("expected Start event, got {:?}", capture.events_snapshot()));
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("gemini-provider-session")
        );
        assert_eq!(
            event.get("conversationId").and_then(Value::as_str),
            Some("gemini-conversation")
        );
        assert_eq!(
            event.get("transcript_path").and_then(Value::as_str),
            Some("/tmp/gemini-provider-session.jsonl")
        );
        assert_eq!(
            event.get("tool_name").and_then(Value::as_str),
            Some("gemini")
        );
    }

    #[test]
    fn copilot_hook_script_preserves_metadata_for_permission_events() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("copilot-metadata", super::COPILOT_HOOK_SCRIPT);
        let payload = json!({
            "session_id": "copilot-provider-session",
            "conversationID": "copilot-conversation",
            "transcriptPath": "/tmp/copilot-provider-session.jsonl",
            "tool_name": "shell"
        })
        .to_string();

        let output = run_hook_script_with_stdin(
            &script,
            &["preToolUse"],
            &payload,
            &capture,
            "copilot-metadata",
        );

        assert!(output.status.success(), "copilot hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("PermissionRequest", Duration::from_secs(5))
            .unwrap_or_else(|| {
                panic!(
                    "expected PermissionRequest event, got {:?}",
                    capture.events_snapshot()
                )
            });
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("copilot-provider-session")
        );
        assert_eq!(
            event.get("conversationID").and_then(Value::as_str),
            Some("copilot-conversation")
        );
        assert_eq!(
            event.get("transcriptPath").and_then(Value::as_str),
            Some("/tmp/copilot-provider-session.jsonl")
        );
        assert_eq!(
            event.get("tool_name").and_then(Value::as_str),
            Some("shell")
        );
    }

    #[test]
    fn cursor_hook_script_preserves_metadata_for_permission_events() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("cursor-metadata", super::CURSOR_HOOK_SCRIPT);
        let payload = json!({
            "session_id": "cursor-provider-session",
            "providerTranscriptPath": "/tmp/cursor-provider-session.jsonl",
            "tool_name": "terminal"
        })
        .to_string();

        let output = run_hook_script_with_stdin(
            &script,
            &["PermissionRequest"],
            &payload,
            &capture,
            "cursor-metadata",
        );

        assert!(output.status.success(), "cursor hook failed: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("\"continue\":true"),
            "cursor hook should auto-confirm permission request: {output:?}"
        );
        let event = capture
            .wait_for_event_payload("PermissionRequest", Duration::from_secs(5))
            .unwrap_or_else(|| {
                panic!(
                    "expected PermissionRequest event, got {:?}",
                    capture.events_snapshot()
                )
            });
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("cursor-provider-session")
        );
        assert_eq!(
            event.get("providerTranscriptPath").and_then(Value::as_str),
            Some("/tmp/cursor-provider-session.jsonl")
        );
        assert_eq!(
            event.get("tool_name").and_then(Value::as_str),
            Some("terminal")
        );
    }

    #[test]
    fn cursor_hook_script_uses_cursor_conversation_id_for_start_events() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("cursor-start-env", super::CURSOR_HOOK_SCRIPT);

        let mut child = Command::new("bash")
            .arg(&script)
            .arg("Start")
            .env("HOME", hook_env_home("cursor-start-env"))
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("CURSOR_CONVERSATION_ID", "cursor-chat-123")
            .env(
                "UNPEEL_HOOK_TRACE_FILE",
                hook_trace_file("cursor-start-env"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cursor hook");
        child
            .stdin
            .take()
            .expect("cursor hook stdin")
            .write_all(b"{}")
            .expect("write cursor hook stdin");
        let output = child.wait_with_output().expect("wait for cursor hook");

        assert!(output.status.success(), "cursor hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("Start", Duration::from_secs(5))
            .unwrap_or_else(|| panic!("expected Start event, got {:?}", capture.events_snapshot()));
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("cursor-chat-123")
        );
    }

    #[test]
    fn grok_hook_script_carries_provider_session_and_tool_for_attention_events() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("grok-attention", super::GROK_HOOK_SCRIPT);

        let mut child = Command::new("bash")
            .arg(&script)
            .arg("Attention")
            .env("HOME", hook_env_home("grok-attention"))
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("GROK_SESSION_ID", "grok-provider-session")
            .env("UNPEEL_HOOK_TRACE_FILE", hook_trace_file("grok-attention"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn grok hook");
        child
            .stdin
            .take()
            .expect("grok hook stdin")
            .write_all(br#"{"toolName":"shell","notificationType":"approval_required"}"#)
            .expect("write grok hook stdin");
        let output = child.wait_with_output().expect("wait for grok hook");

        assert!(output.status.success(), "grok hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("PermissionRequest", Duration::from_secs(5))
            .unwrap_or_else(|| {
                panic!(
                    "expected PermissionRequest event, got {:?}",
                    capture.events_snapshot()
                )
            });
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("grok-provider-session")
        );
        assert_eq!(
            event.get("tool_name").and_then(Value::as_str),
            Some("shell")
        );
    }

    #[test]
    fn grok_hook_script_forwards_user_prompt_submit_as_busy_event() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("grok-user-prompt-submit", super::GROK_HOOK_SCRIPT);

        let output = run_hook_script_with_stdin(
            &script,
            &["UserPromptSubmit"],
            "{}",
            &capture,
            "grok-user-prompt-submit",
        );

        assert!(output.status.success(), "grok hook failed: {output:?}");
        capture
            .wait_for_event_payload("UserPromptSubmit", Duration::from_secs(5))
            .unwrap_or_else(|| {
                panic!(
                    "expected UserPromptSubmit event, got {:?}",
                    capture.events_snapshot()
                )
            });
    }

    #[test]
    fn grok_hook_script_posts_through_port_registry_without_app_port() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("grok-registry-only", super::GROK_HOOK_SCRIPT);
        let registry_dir = temp_path("grok-registry-only-ports");
        let registry = registry_dir.join("app-ports");
        fs::write(&registry, format!("{}\n", capture.port)).expect("write port registry");

        let mut command = Command::new("bash");
        command
            .arg(&script)
            .arg("UserPromptSubmit")
            .env("HOME", hook_env_home("grok-registry-only"))
            .env_remove("UNPEEL_APP_PORT")
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("UNPEEL_APP_PORT_REGISTRY_FILE", &registry)
            .env(
                "UNPEEL_HOOK_TRACE_FILE",
                hook_trace_file("grok-registry-only"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn grok hook");
        child
            .stdin
            .take()
            .expect("grok hook stdin")
            .write_all(b"{}")
            .expect("write grok hook stdin");
        let output = child.wait_with_output().expect("wait for grok hook");

        assert!(output.status.success(), "grok hook failed: {output:?}");
        capture
            .wait_for_event_payload("UserPromptSubmit", Duration::from_secs(5))
            .unwrap_or_else(|| {
                panic!(
                    "expected UserPromptSubmit via app-ports, got {:?}",
                    capture.events_snapshot()
                )
            });
    }

    #[test]
    fn codex_wrapper_script_installs_notify_hook() {
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("UNPEEL_REAL_CODEX_BIN"));
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("UNPEEL_ORIGINAL_PATH"));
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("[ \"$REAL_BIN\" -ef \"$0\" ]"));
        assert!(super::CODEX_WRAPPER_SCRIPT
            .contains("notify=[\\\"bash\\\",\\\"$UNPEEL_CODEX_NOTIFY_PATH\\\"]"));
    }

    #[test]
    fn codex_wrapper_script_can_wait_for_attach_ready() {
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("UNPEEL_WAIT_FOR_ATTACH"));
        assert!(super::CODEX_WRAPPER_SCRIPT.contains(".attach-ready"));
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("sleep 0.02"));
    }

    #[test]
    fn codex_wrapper_script_registers_the_unified_unpeel_mcp_server() {
        assert!(super::CODEX_WRAPPER_SCRIPT
            .contains("mcp_servers.unpeel.command=\\\"$UNPEEL_MCP_BIN\\\""));
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("mcp_servers.unpeel.args=[\\\"__mcp__\\\"]"));
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("mcp_servers.unpeel.env={UNPEEL_SESSION_ID="));
        // Registration must be skipped gracefully when the binary is unknown.
        assert!(super::CODEX_WRAPPER_SCRIPT.contains("[ -x \"${UNPEEL_MCP_BIN:-}\" ]"));
        // One server, one block: the legacy per-domain registrations are gone.
        assert!(!super::CODEX_WRAPPER_SCRIPT.contains("mcp_servers.unpeel-sessions"));
        assert!(!super::CODEX_WRAPPER_SCRIPT.contains("mcp_servers.unpeel-browser"));
        assert!(!super::CODEX_WRAPPER_SCRIPT.contains("UNPEEL_BROWSER_MCP_BIN"));
    }

    #[test]
    fn gemini_hook_script_maps_agent_events() {
        assert!(GEMINI_HOOK_SCRIPT.contains("BeforeAgent"));
        assert!(GEMINI_HOOK_SCRIPT.contains("AfterAgent"));
        assert!(GEMINI_HOOK_SCRIPT.contains("AfterTool"));
        assert!(GEMINI_HOOK_SCRIPT.contains("Notification"));
        assert!(GEMINI_HOOK_SCRIPT.contains("EVENT_TYPE=\"Start\""));
        assert!(GEMINI_HOOK_SCRIPT.contains("EVENT_TYPE=\"Stop\""));
        assert!(GEMINI_HOOK_SCRIPT.contains("EVENT_TYPE=\"PermissionRequest\""));
        assert!(GEMINI_HOOK_SCRIPT.contains("UNPEEL_APP_PORT"));
    }

    #[test]
    fn kimi_config_reconciliation_preserves_user_hooks_and_replaces_managed_hooks() {
        let raw = r#"
theme = "light"

[[hooks]]
event = "PostToolUse"
matcher = "WriteFile"
command = "prettier"

[[hooks]]
event = "Stop"
command = "\"${UNPEEL_HOME:-$HOME/.unpeel}/hooks/kimi-hook.sh\" old"
timeout = 5
"#;
        let updated = super::reconcile_kimi_config(raw, true).expect("reconcile Kimi Code config");
        let parsed = updated.parse::<toml::Value>().expect("valid TOML");
        assert_eq!(
            parsed.get("theme").and_then(toml::Value::as_str),
            Some("light")
        );
        assert!(updated.contains("command = \"prettier\""));
        assert!(!updated.contains("kimi-hook.sh\\\" old"));
        assert_eq!(updated.matches("[[hooks]]").count(), 10);
        assert_eq!(
            updated
                .matches("${UNPEEL_HOME:-$HOME/.unpeel}/hooks/kimi-hook.sh")
                .count(),
            9
        );
        assert!(updated.contains("event = \"SessionStart\""));
        assert!(updated.contains("event = \"PermissionRequest\""));
        assert!(updated.contains("event = \"Interrupt\""));
        assert!(updated.contains("matcher = \"permission_prompt\""));
        assert!(updated.contains("matcher = \"ask_user_question|AskUserQuestion\""));
    }

    #[test]
    fn legacy_kimi_config_omits_kimi_code_only_hook_events() {
        let updated = super::reconcile_kimi_config("", false).expect("reconcile legacy Kimi");
        assert_eq!(updated.matches("[[hooks]]").count(), 7);
        assert!(!updated.contains("event = \"PermissionRequest\""));
        assert!(!updated.contains("event = \"Interrupt\""));
    }

    #[test]
    fn kimi_code_mcp_entries_preserve_user_name_collisions() {
        let mut servers = serde_json::Map::new();
        servers.insert(
            "unpeel-sessions".to_string(),
            json!({"command":"user-owned-server"}),
        );
        super::upsert_kimi_code_managed_mcp(
            &mut servers,
            "unpeel-sessions",
            crate::mcp_gate::SESSIONS_KIND,
            json!({
                "command":"/tmp/unpeel-host",
                "args":[crate::mcp_gate::MCP_GATE_ARG, crate::mcp_gate::SESSIONS_KIND]
            }),
        );
        assert_eq!(
            servers["unpeel-sessions"]["command"],
            Value::String("user-owned-server".to_string())
        );
        assert_eq!(
            servers["unpeel-sessions-unpeel"]["args"][0],
            crate::mcp_gate::MCP_GATE_ARG
        );
    }

    #[test]
    fn kimi_hook_forwards_provider_session_and_attention_metadata() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("kimi-metadata", KIMI_HOOK_SCRIPT);
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "kimi-provider-session",
            "cwd": "/tmp/kimi-project",
            "tool_name": "AskUserQuestion"
        })
        .to_string();
        let output = run_hook_script_with_stdin(
            &script,
            &["Attention"],
            &payload,
            &capture,
            "kimi-metadata",
        );
        assert!(output.status.success(), "Kimi hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("PermissionRequest", Duration::from_secs(3))
            .expect("Kimi hook event");
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("kimi-provider-session")
        );
        assert_eq!(
            event.get("tool_name").and_then(Value::as_str),
            Some("AskUserQuestion")
        );
    }

    #[test]
    fn cline_native_hook_forwards_root_session_id_and_busy_edge() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("cline-native", CLINE_HOOK_SCRIPT);
        let payload = json!({
            "hookName": "agent_start",
            "taskId": "conv_ephemeral",
            "sessionContext": { "rootSessionId": "cline-persisted-session" },
            "workspaceRoots": ["/tmp/cline-project"]
        })
        .to_string();
        let output =
            run_hook_script_with_stdin(&script, &["TaskStart"], &payload, &capture, "cline-native");
        assert!(output.status.success(), "Cline hook failed: {output:?}");
        let event = capture
            .wait_for_event_payload("UserPromptSubmit", Duration::from_secs(3))
            .expect("Cline hook event");
        assert_eq!(
            event.get("session_id").and_then(Value::as_str),
            Some("cline-persisted-session")
        );
    }

    #[test]
    fn cline_hook_installer_never_overwrites_a_user_hook_slot() {
        let hooks_dir = temp_path("cline-hook-slot");
        let user_hook = hooks_dir.join("TaskStart.bash");
        fs::write(&user_hook, "#!/bin/bash\necho user-owned\n").expect("write user hook");

        super::write_cline_event_hook(
            &hooks_dir,
            "TaskStart",
            "#!/bin/bash\n# Managed by Unpeel.\necho managed\n",
        )
        .expect("install managed hook");
        assert_eq!(
            fs::read_to_string(&user_hook).expect("read user hook"),
            "#!/bin/bash\necho user-owned\n"
        );
        assert!(fs::read_to_string(hooks_dir.join("TaskStart.zsh"))
            .expect("read managed hook")
            .contains("# Managed by Unpeel."));
    }

    #[test]
    fn kiro_hook_maps_v3_and_v2_lifecycle_events() {
        assert!(KIRO_HOOK_SCRIPT.contains("SessionStart|agentSpawn"));
        assert!(KIRO_HOOK_SCRIPT.contains("UserPromptSubmit|userPromptSubmit"));
        assert!(KIRO_HOOK_SCRIPT.contains("PreToolUse|preToolUse|PostToolUse|postToolUse"));
        assert!(KIRO_HOOK_SCRIPT.contains("messages.jsonl"));
        assert!(KIRO_HOOK_SCRIPT.contains("sessions/cli/$_provider_session_id.jsonl"));
        assert!(KIRO_HOOK_SCRIPT.contains("UNPEEL_SESSION_ID"));
    }

    #[test]
    fn kiro_mcp_config_explicitly_maps_session_grants_and_identity() {
        let server = kiro_mcp_server_value(std::path::Path::new("/tmp/unpeel-host"));
        assert_eq!(server["args"][0], crate::mcp_gate::MCP_GATE_ARG);
        assert_eq!(server["args"][1], crate::mcp_gate::UNIFIED_KIND);
        assert_eq!(
            server["env"]["UNPEEL_SESSIONS_MCP_ENABLED"],
            "${UNPEEL_KIRO_SESSIONS_MCP_ENABLED}"
        );
        assert_eq!(
            server["env"]["UNPEEL_SESSION_ID"],
            "${UNPEEL_KIRO_SESSION_ID}"
        );
        assert_eq!(server["env"]["UNPEEL_HOME"], "${UNPEEL_KIRO_UNPEEL_HOME}");
    }

    #[test]
    fn copilot_hook_script_maps_lifecycle_and_permission_events() {
        assert!(COPILOT_HOOK_SCRIPT.contains("sessionStart"));
        assert!(COPILOT_HOOK_SCRIPT.contains("sessionEnd"));
        assert!(COPILOT_HOOK_SCRIPT.contains("PermissionRequest"));
        assert!(COPILOT_HOOK_SCRIPT.contains("printf '{}\\n'"));
        assert!(COPILOT_HOOK_SCRIPT.contains("UNPEEL_APP_PORT"));
    }

    #[test]
    fn cursor_hook_script_ignores_all_grok_events() {
        let capture = CaptureServer::start();
        let script = write_temp_hook_script("cursor-ignore-grok", super::CURSOR_HOOK_SCRIPT);
        let mut child = Command::new("bash")
            .arg(&script)
            .arg("Start")
            .env("HOME", hook_env_home("cursor-ignore-grok"))
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "unpeel-route-session")
            .env("GROK_SESSION_ID", "grok-provider-session")
            .env(
                "UNPEEL_HOOK_TRACE_FILE",
                hook_trace_file("cursor-ignore-grok"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cursor hook");
        child
            .stdin
            .take()
            .expect("cursor hook stdin")
            .write_all(br#"{}"#)
            .expect("write cursor hook stdin");
        let finished = child.wait_with_output().expect("wait for cursor hook");
        assert!(
            finished.status.success(),
            "cursor hook failed: {finished:?}"
        );
        assert!(
            String::from_utf8_lossy(&finished.stdout).contains(r#"{"continue":true}"#),
            "grok-ignored cursor hooks must fail-open, got {:?}",
            String::from_utf8_lossy(&finished.stdout)
        );
        thread::sleep(Duration::from_millis(200));
        assert!(
            capture.events_snapshot().is_empty(),
            "Grok must not inherit Cursor Start, got {:?}",
            capture.events_snapshot()
        );
    }

    #[test]
    fn cursor_hook_script_auto_confirms_permission_requests() {
        assert!(CURSOR_HOOK_SCRIPT.contains("PermissionRequest"));
        assert!(CURSOR_HOOK_SCRIPT.contains("{\"continue\":true}"));
        assert!(CURSOR_HOOK_SCRIPT.contains("UNPEEL_APP_PORT"));
        assert!(CURSOR_HOOK_SCRIPT.contains("UNPEEL_PORT_REGISTRY_FILE"));
        assert!(CURSOR_HOOK_SCRIPT.contains("post_hook_event_to_current_ports"));
        assert!(CURSOR_HOOK_SCRIPT.contains("GROK_SESSION_ID"));
        assert!(CURSOR_HOOK_SCRIPT.contains("\"session_id\""));
        assert!(super::GROK_HOOK_SCRIPT.contains("Attention"));
        let grok_hooks = super::grok_hooks_json(&PathBuf::from("/tmp/grok-hook.sh"))
            .expect("serialize grok hooks");
        assert!(grok_hooks.contains("approval_required"));
        assert!(grok_hooks.contains("\"command\": \"/tmp/grok-hook.sh HookSeen\""));
        assert!(grok_hooks.contains("\"command\": \"/tmp/grok-hook.sh UserPromptSubmit\""));
        assert!(!grok_hooks.contains("\"command\": \"/tmp/grok-hook.sh Start\""));
        assert!(super::GROK_HOOK_SCRIPT.contains("tool_name"));
        assert!(super::GROK_DEFAULTS_WRAPPER_SCRIPT.contains("AppleInterfaceStyle"));
        assert!(super::GROK_DEFAULTS_WRAPPER_SCRIPT.contains("UNPEEL_APP_APPEARANCE_FILE"));
    }

    #[test]
    fn grok_command_wrapper_resolves_auto_theme_from_unpeel_appearance() {
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("UNPEEL_REAL_GROK_BIN"));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("UNPEEL_GROK_APP_APPEARANCE"));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("auto_light_theme"));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("auto_dark_theme"));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("GROK_HOME=\"$_overlay\""));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("theme = \\\"\" target \"\\\"\""));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("GROK_CLAUDE_HOOKS_ENABLED=false"));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("GROK_CURSOR_HOOKS_ENABLED=false"));
        assert!(super::GROK_COMMAND_WRAPPER_SCRIPT.contains("disable_compat_vendor_hooks"));
    }

    #[test]
    fn grok_command_wrapper_executes_with_resolved_light_overlay_config() {
        let root = temp_path("grok-wrapper");
        let real_home = root.join("real-home");
        fs::create_dir_all(&real_home).expect("create real grok home");
        fs::write(
            real_home.join("config.toml"),
            "[ui]\ntheme = \"auto\"\nauto_light_theme = \"grokday\"\nauto_dark_theme = \"groknight\"\n",
        )
        .expect("write real config");

        let fake_grok = root.join("real-grok");
        super::write_executable_script(
            &fake_grok,
            "#!/bin/sh\nprintf 'GROK_HOME=%s\\n' \"$GROK_HOME\"\nprintf 'GROK_CLAUDE_HOOKS_ENABLED=%s\\n' \"$GROK_CLAUDE_HOOKS_ENABLED\"\nprintf 'GROK_CURSOR_HOOKS_ENABLED=%s\\n' \"$GROK_CURSOR_HOOKS_ENABLED\"\ncat \"$GROK_HOME/config.toml\"\n",
            "fake grok",
        )
        .expect("write fake grok");

        let wrapper = root.join("grok");
        super::write_executable_script(
            &wrapper,
            super::GROK_COMMAND_WRAPPER_SCRIPT,
            "grok wrapper",
        )
        .expect("write grok wrapper");

        let output = Command::new(&wrapper)
            .env("HOME", &root)
            .env("GROK_HOME", &real_home)
            .env("UNPEEL_REAL_GROK_BIN", &fake_grok)
            .env("UNPEEL_GROK_APP_APPEARANCE", "light")
            .env("UNPEEL_SESSION_ID", "test-session")
            .output()
            .expect("run wrapper");

        assert!(output.status.success(), "wrapper failed: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("GROK_HOME="));
        assert!(stdout.contains("theme = \"grokday\""), "{stdout}");
        assert!(stdout.contains("[compat.claude]"), "{stdout}");
        assert!(stdout.contains("[compat.cursor]"), "{stdout}");
        assert!(stdout.contains("hooks = false"), "{stdout}");
        assert!(
            stdout.contains("GROK_CLAUDE_HOOKS_ENABLED=false"),
            "{stdout}"
        );
        assert!(
            stdout.contains("GROK_CURSOR_HOOKS_ENABLED=false"),
            "{stdout}"
        );
        assert!(fs::read_to_string(real_home.join("config.toml"))
            .expect("read real config")
            .contains("theme = \"auto\""));
    }

    #[test]
    fn grok_command_wrapper_disables_compat_hooks_when_theme_is_already_set() {
        let root = temp_path("grok-wrapper-fixed-theme");
        let real_home = root.join("real-home");
        fs::create_dir_all(&real_home).expect("create real grok home");
        fs::write(
            real_home.join("config.toml"),
            "[ui]\ntheme = \"groknight\"\n",
        )
        .expect("write real config");

        let fake_grok = root.join("real-grok");
        super::write_executable_script(
            &fake_grok,
            "#!/bin/sh\nprintf 'GROK_HOME=%s\\n' \"$GROK_HOME\"\nprintf 'GROK_CLAUDE_HOOKS_ENABLED=%s\\n' \"$GROK_CLAUDE_HOOKS_ENABLED\"\ncat \"$GROK_HOME/config.toml\"\n",
            "fake grok",
        )
        .expect("write fake grok");

        let wrapper = root.join("grok");
        super::write_executable_script(
            &wrapper,
            super::GROK_COMMAND_WRAPPER_SCRIPT,
            "grok wrapper",
        )
        .expect("write grok wrapper");

        let output = Command::new(&wrapper)
            .env("HOME", &root)
            .env("GROK_HOME", &real_home)
            .env("UNPEEL_HOME", root.join("unpeel-home"))
            .env("UNPEEL_REAL_GROK_BIN", &fake_grok)
            .env("UNPEEL_GROK_APP_APPEARANCE", "dark")
            .env("UNPEEL_SESSION_ID", "fixed-theme-session")
            .output()
            .expect("run wrapper");

        assert!(output.status.success(), "wrapper failed: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("theme = \"groknight\""), "{stdout}");
        assert!(stdout.contains("[compat.claude]"), "{stdout}");
        assert!(stdout.contains("hooks = false"), "{stdout}");
        assert!(
            stdout.contains("GROK_CLAUDE_HOOKS_ENABLED=false"),
            "{stdout}"
        );
        assert!(fs::read_to_string(real_home.join("config.toml"))
            .expect("read real config")
            .contains("theme = \"groknight\""));
        assert!(
            !fs::read_to_string(real_home.join("config.toml"))
                .expect("read real config")
                .contains("[compat.claude]"),
            "must not rewrite the user's real Grok config"
        );
    }

    #[test]
    fn grok_command_wrapper_disables_compat_hooks_without_appearance() {
        let root = temp_path("grok-wrapper-no-appearance");
        let real_home = root.join("real-home");
        fs::create_dir_all(&real_home).expect("create real grok home");
        fs::write(
            real_home.join("config.toml"),
            "[ui]\ntheme = \"groknight\"\n",
        )
        .expect("write real config");

        let fake_grok = root.join("real-grok");
        super::write_executable_script(
            &fake_grok,
            "#!/bin/sh\nprintf 'GROK_HOME=%s\\n' \"$GROK_HOME\"\nprintf 'GROK_CLAUDE_HOOKS_ENABLED=%s\\n' \"$GROK_CLAUDE_HOOKS_ENABLED\"\ncat \"$GROK_HOME/config.toml\"\n",
            "fake grok",
        )
        .expect("write fake grok");

        let wrapper = root.join("grok");
        super::write_executable_script(
            &wrapper,
            super::GROK_COMMAND_WRAPPER_SCRIPT,
            "grok wrapper",
        )
        .expect("write grok wrapper");

        let output = Command::new(&wrapper)
            .env("HOME", &root)
            .env("GROK_HOME", &real_home)
            .env("UNPEEL_HOME", root.join("unpeel-home"))
            .env("UNPEEL_REAL_GROK_BIN", &fake_grok)
            .env_remove("UNPEEL_GROK_APP_APPEARANCE")
            .env("UNPEEL_SESSION_ID", "no-appearance-session")
            .output()
            .expect("run wrapper");

        assert!(output.status.success(), "wrapper failed: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("[compat.claude]"), "{stdout}");
        assert!(stdout.contains("hooks = false"), "{stdout}");
        assert!(
            stdout.contains("GROK_CLAUDE_HOOKS_ENABLED=false"),
            "{stdout}"
        );
    }

    #[test]
    fn claude_hook_script_posts_to_registered_app_ports() {
        assert!(CLAUDE_HOOK_SCRIPT.contains("UNPEEL_PORT_REGISTRY_FILE"));
        assert!(CLAUDE_HOOK_SCRIPT.contains("current_unpeel_ports"));
        assert!(CLAUDE_HOOK_SCRIPT.contains("post_hook_payload_to_current_ports"));
        assert!(super::HOOK_EVENTS.contains(&"StopFailure"));
    }

    #[test]
    fn read_mergeable_json_skips_update_on_malformed_settings() {
        let dir = std::env::temp_dir().join(format!("unpeel-merge-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        // Missing file → an empty object to merge into.
        let missing = dir.join("missing.json");
        assert_eq!(
            super::read_mergeable_json_object(&missing, "missing").unwrap(),
            Some(serde_json::json!({}))
        );

        // Malformed JSON (e.g. trailing comma / torn write) → None, so callers
        // skip the update and leave the user's file intact instead of clobbering
        // it with an Unpeel-only object.
        let malformed = dir.join("malformed.json");
        std::fs::write(&malformed, "{ \"hooks\": {,,, ").unwrap();
        assert_eq!(
            super::read_mergeable_json_object(&malformed, "malformed").unwrap(),
            None
        );

        // A non-object root (array) is also treated as "do not touch".
        let array = dir.join("array.json");
        std::fs::write(&array, "[1,2,3]").unwrap();
        assert_eq!(
            super::read_mergeable_json_object(&array, "array").unwrap(),
            None
        );

        // Valid object → returned for merging.
        let valid = dir.join("valid.json");
        std::fs::write(&valid, "{\"existing\":true}").unwrap();
        assert_eq!(
            super::read_mergeable_json_object(&valid, "valid").unwrap(),
            Some(serde_json::json!({"existing": true}))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_config_toml_enables_hooks_feature_without_rewriting_other_sections() {
        let raw = r#"model = "gpt-5.5"

[features]
mcp_client = true

[profiles.default]
approval_policy = "on-request"
"#;
        let updated = super::enable_codex_hooks_feature_in_toml(raw).expect("enable hooks feature");
        assert!(updated.contains("[features]\nhooks = true\nmcp_client = true"));
        assert!(updated.contains("[profiles.default]\napproval_policy = \"on-request\""));
        assert!(super::codex_hooks_feature_is_enabled(&updated).expect("parse updated"));
        assert_eq!(
            super::enable_codex_hooks_feature_in_toml(&updated).expect("idempotent enable"),
            updated
        );
    }

    #[test]
    fn codex_config_toml_replaces_disabled_hooks_feature() {
        let raw = r#"[features]
codex_hooks = false
some_other_flag = true
"#;
        let updated = super::enable_codex_hooks_feature_in_toml(raw).expect("enable hooks feature");
        assert!(updated.contains("hooks = true"));
        assert!(!updated.contains("codex_hooks"));
        assert!(updated.contains("some_other_flag = true"));
    }

    #[test]
    fn codex_config_toml_migrates_deprecated_feature_flags() {
        let raw = r#"[features]
codex_hooks = true
collab = true
js_repl = false
"#;
        let updated = super::enable_codex_hooks_feature_in_toml(raw).expect("migrate features");
        assert!(updated.contains("hooks = true"));
        assert!(updated.contains("multi_agent = true"));
        assert!(updated.contains("js_repl = false"));
        assert!(!updated.contains("codex_hooks"));
        assert!(!updated.contains("collab"));
    }

    #[test]
    fn codex_permission_request_hook_entry_matches_all_tools() {
        let script_path = PathBuf::from("/tmp/unpeel notify's hook");
        let entry = super::build_codex_hook_entry("PermissionRequest", &script_path);
        assert_eq!(entry.get("matcher").and_then(Value::as_str), Some("*"));
        let command = entry
            .get("hooks")
            .and_then(Value::as_array)
            .and_then(|hooks| hooks.first())
            .and_then(|hook| hook.get("command"))
            .and_then(Value::as_str)
            .expect("hook command");
        assert!(command.contains("[ -x "));
        assert!(command.ends_with(super::CODEX_MANAGED_HOOK_SUFFIX));
        assert_eq!(
            super::parse_managed_codex_hook_command(command).as_deref(),
            Some(script_path.as_path())
        );
    }

    #[test]
    fn codex_managed_hook_command_guards_only_a_missing_script() {
        let root = temp_path("codex-managed-guard");
        let script_path = root.join("hook path's notify.sh");
        let command = super::build_codex_hook_command(&script_path);
        let missing_status = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .status()
            .expect("run guarded hook command");
        assert!(missing_status.success());

        super::write_executable_script(&script_path, "#!/bin/sh\nexit 23\n", "failing hook")
            .expect("write failing hook");
        let failing_status = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .status()
            .expect("run existing hook command");
        assert_eq!(failing_status.code(), Some(23));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codex_hooks_prune_only_stale_unpeel_entries() {
        let root = temp_path("codex-hook-cleanup");
        let current = root.join(".unpeel/hooks/notify-hook.sh");
        let other_live = root.join("unpeel-dev/hooks/notify-hook.sh");
        let stale = root.join("unpeel-color-probe-test/hooks/notify-hook.sh");
        let foreign = root.join(".clarity/hooks/notify-hook.sh");
        for path in [&current, &other_live] {
            fs::create_dir_all(path.parent().unwrap()).expect("create hook dir");
            fs::write(path, "#!/bin/sh\n").expect("write hook");
        }

        let mut hooks_json = json!({
            "hooks": {
                "UserPromptSubmit": [
                    { "hooks": [{ "type": "command", "command": current.to_string_lossy() }] },
                    { "hooks": [{ "type": "command", "command": other_live.to_string_lossy() }] },
                    { "hooks": [{ "type": "command", "command": stale.to_string_lossy() }] },
                    { "hooks": [{ "type": "command", "command": foreign.to_string_lossy() }] }
                ]
            }
        });

        assert!(super::reconcile_codex_hooks_json(&mut hooks_json, &current));

        let prompt_commands: Vec<_> = hooks_json["hooks"]["UserPromptSubmit"]
            .as_array()
            .expect("prompt hooks")
            .iter()
            .filter_map(|entry| entry["hooks"][0]["command"].as_str())
            .collect();
        assert!(prompt_commands.contains(&current.to_string_lossy().as_ref()));
        assert!(prompt_commands.contains(&other_live.to_string_lossy().as_ref()));
        assert!(prompt_commands.contains(&foreign.to_string_lossy().as_ref()));
        assert!(!prompt_commands.contains(&stale.to_string_lossy().as_ref()));

        let stop_command = hooks_json["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .expect("managed stop command");
        assert_eq!(
            super::parse_managed_codex_hook_command(stop_command).as_deref(),
            Some(current.as_path())
        );
        assert!(!super::reconcile_codex_hooks_json(
            &mut hooks_json,
            &current
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn opencode_plugin_tracks_busy_idle_and_permission_events() {
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("session.status"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("permission.ask"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("notify('Start', sessionID)"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("notify('Stop', sessionID)"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("notify('PermissionRequest', rootSessionID)"));
        assert!(OPENCODE_PLUGIN_SCRIPT.contains("session_id: sessionID"));
    }

    #[test]
    fn amp_plugin_tracks_agent_lifecycle_and_prompt_hints() {
        assert!(AMP_PLUGIN_SCRIPT.contains("@i-know-the-amp-plugin-api-is-wip"));
        assert!(AMP_PLUGIN_SCRIPT.contains("amp.on(\"agent.start\""));
        assert!(AMP_PLUGIN_SCRIPT.contains("amp.on(\"agent.end\""));
        assert!(AMP_PLUGIN_SCRIPT.contains("tool_name: \"amp\""));
        assert!(AMP_PLUGIN_SCRIPT.contains("prompt_text"));
        assert!(AMP_PLUGIN_SCRIPT.contains("hook_event_name"));
        assert!(AMP_PLUGIN_SCRIPT.contains("threadIDFrom"));
        assert!(AMP_PLUGIN_SCRIPT.contains("session_id: threadIDFrom"));
    }

    #[test]
    #[ignore = "runs real Claude CLI against live auth"]
    fn live_claude_print_emits_start_and_stop() {
        if real_command_path("claude").is_none() {
            return;
        }

        let capture = CaptureServer::start();
        let script_path = super::claude_hook_script_path();
        super::write_executable_script(
            &script_path,
            super::CLAUDE_HOOK_SCRIPT,
            "Claude hook script",
        )
        .expect("write claude hook script");

        let settings_dir = temp_path("claude-settings");
        let settings_path = settings_dir.join("settings.json");
        let command = script_path.to_string_lossy().to_string();
        let settings = json!({
            "hooks": {
                "UserPromptSubmit": [super::build_hook_entry("UserPromptSubmit", &command)],
                "Stop": [super::build_hook_entry("Stop", &command)],
                "PermissionRequest": [super::build_hook_entry("PermissionRequest", &command)]
            }
        });
        fs::write(
            &settings_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&settings).expect("serialize settings")
            ),
        )
        .expect("write claude settings");

        let mut command = Command::new("claude");
        command
            .current_dir(repo_root())
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "live-claude")
            .arg("-p")
            .arg("--settings")
            .arg(&settings_path)
            .arg("--permission-mode")
            .arg("acceptEdits")
            .arg("Reply with exactly OK and nothing else.");

        let status =
            run_with_timeout(command, Duration::from_secs(120)).expect("claude live smoke");
        assert!(status.success(), "claude exited unsuccessfully: {status}");
        assert!(
            capture.wait_for_event("UserPromptSubmit", Duration::from_secs(10)),
            "expected UserPromptSubmit event, got {:?}",
            capture.event_names()
        );
        assert!(
            capture.wait_for_event("Stop", Duration::from_secs(10)),
            "expected Stop event, got {:?}",
            capture.event_names()
        );
    }

    #[test]
    #[ignore = "runs real OpenCode CLI against live auth"]
    fn live_opencode_run_emits_stop() {
        if real_command_path("opencode").is_none() {
            return;
        }

        super::install_opencode_plugin().expect("install opencode plugin");
        let capture = CaptureServer::start();

        let mut command = Command::new("opencode");
        command
            .current_dir(repo_root())
            .env("UNPEEL_APP_PORT", capture.port.to_string())
            .env("UNPEEL_SESSION_ID", "live-opencode")
            .env(
                "OPENCODE_CONFIG_DIR",
                super::opencode_config_dir().to_string_lossy().to_string(),
            )
            .arg("run")
            .arg("--dir")
            .arg(repo_root())
            .arg("--format")
            .arg("json")
            .arg("Reply with exactly OK and nothing else.");

        let status =
            run_with_timeout(command, Duration::from_secs(180)).expect("opencode live smoke");
        assert!(status.success(), "opencode exited unsuccessfully: {status}");
        assert!(
            capture.wait_for_event("Start", Duration::from_secs(10)),
            "expected Start event, got {:?}",
            capture.event_names()
        );
        assert!(
            capture.wait_for_event("Stop", Duration::from_secs(10)),
            "expected Stop event, got {:?}",
            capture.event_names()
        );
    }

    #[test]
    fn cursor_mcp_config_merges_and_prunes_unpeel_servers() {
        let home = hook_env_home("cursor-mcp-config");
        let mcp_path = home.join(".cursor").join("mcp.json");
        fs::create_dir_all(mcp_path.parent().unwrap()).expect("create cursor dir");
        // Seed with a user server plus every legacy Unpeel entry generation
        // (pre-rename unified `unpeel-mcp` and the per-domain pair): the merge
        // must adopt/replace with one `unpeel` entry and prune all legacy
        // names while leaving user servers alone.
        fs::write(
            &mcp_path,
            r#"{
  "mcpServers": {
    "GmailCompany": { "url": "https://example.com/mcp" },
    "unpeel-mcp": { "type": "stdio", "command": "/stale", "args": ["__mcp__"] },
    "unpeel-sessions": { "type": "stdio", "command": "/stale", "args": ["__mcp__"] },
    "unpeel-browser": { "type": "stdio", "command": "/stale", "args": ["__browser_mcp__"] }
  }
}
"#,
        )
        .expect("seed cursor mcp.json");

        super::merge_cursor_mcp_servers_at(
            Some(&mcp_path),
            [
                (
                    "unpeel",
                    Some(json!({
                        "type": "stdio",
                        "command": "/Applications/Unpeel.app/Contents/MacOS/unpeel-host",
                        "args": ["__mcp__"],
                    })),
                ),
                ("unpeel-mcp", None),
                ("unpeel-sessions", None),
                ("unpeel-browser", None),
            ],
        )
        .expect("merge cursor mcp.json");

        let merged: Value =
            serde_json::from_str(&fs::read_to_string(&mcp_path).expect("read mcp.json"))
                .expect("parse mcp.json");
        let servers = merged["mcpServers"].as_object().expect("mcpServers object");
        assert!(servers.contains_key("GmailCompany"));
        assert_eq!(servers["unpeel"]["args"][0], "__mcp__");
        assert!(!servers.contains_key("unpeel-mcp"));
        assert!(!servers.contains_key("unpeel-sessions"));
        assert!(!servers.contains_key("unpeel-browser"));
    }
}
