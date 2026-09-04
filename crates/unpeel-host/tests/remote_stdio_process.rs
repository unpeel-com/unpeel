use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use unpeel_core::relay_crypto::{
    encode_tunnel_request, parse_tunnel_response, TunnelRequest, TunnelResponse,
};
use unpeel_core::remote_stdio::{
    read_frame, write_frame, FRAME_KIND_REQUEST, FRAME_KIND_RESPONSE, REMOTE_STDIO_ARG,
};

fn temp_home() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // macOS Unix-domain sockets have a short path limit. `$TMPDIR` is deeply
    // nested there, so use the stable short alias just as session-host tests
    // do when they need a real control socket.
    std::path::PathBuf::from("/tmp").join(format!("u-rs-{}-{nonce:x}", std::process::id()))
}

fn request(id: u64, method: &str, path: &str) -> TunnelRequest {
    TunnelRequest {
        id,
        method: method.into(),
        path: path.into(),
        query: Vec::new(),
        // Owner authority is derived by the gateway; this must be ignored.
        auth: Some("spoofed-controller-token".into()),
        content_type: None,
        body: Vec::new(),
    }
}

fn send(stdin: &mut impl Write, request: &TunnelRequest) {
    write_frame(stdin, FRAME_KIND_REQUEST, &encode_tunnel_request(request)).unwrap();
}

fn receive(stdout: &mut impl std::io::Read) -> TunnelResponse {
    let frame = read_frame(stdout).unwrap().expect("response frame");
    assert_eq!(frame.kind, FRAME_KIND_RESPONSE);
    parse_tunnel_response(&frame.payload).unwrap()
}

fn json_string_field(body: &[u8], key: &str) -> String {
    let body = std::str::from_utf8(body).unwrap();
    let marker = format!("\"{key}\":\"");
    body.split_once(&marker)
        .unwrap_or_else(|| panic!("missing {key} in {body}"))
        .1
        .split_once('"')
        .unwrap()
        .0
        .to_owned()
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

#[test]
fn real_gateway_bootstraps_pages_output_and_dispatches_concurrently() {
    let home = temp_home();
    let session_dir = home.join("app-sessions").join("s1");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        home.join("app-state.json"),
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
          "session":{"id":"s1","project_id":"p1","label":"Remote test","command":"cat","created_at":1},
          "cwd":"/tmp","state":"exited","pid":null,"exit_code":0
        }"#,
    )
    .unwrap();
    std::fs::write(session_dir.join("output.bin"), b"hello").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_unpeel-host"))
        .arg(REMOTE_STDIO_ARG)
        .env("UNPEEL_HOME", &home)
        .env("USER", "spoofed-environment-user")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    send(&mut stdin, &request(1, "GET", "/mobile/bootstrap"));
    let bootstrap = receive(&mut stdout);
    assert_eq!((bootstrap.id, bootstrap.status), (1, 200));
    let bootstrap_body = String::from_utf8(bootstrap.body).unwrap();
    assert!(bootstrap_body.contains("Remote test"), "{bootstrap_body}");
    assert!(bootstrap_body.contains(r#""status":"exited""#));
    assert!(bootstrap_body.contains(r#""activity":"idle""#));
    assert!(!bootstrap_body.contains(r#""activity":"exited""#));
    assert!(bootstrap_body.contains("session.output.read"));
    assert!(!bootstrap_body.contains("approval.answer"));
    assert!(!bootstrap_body.contains("pairing.create"));
    assert!(!bootstrap_body.contains("session.output.subscribe"));

    let mut output = request(2, "GET", "/mobile/output");
    output.query = vec![("session_id".into(), "s1".into())];
    send(&mut stdin, &output);
    let first_output = receive(&mut stdout);
    assert_eq!((first_output.id, first_output.status), (2, 200));
    let first_output_body = String::from_utf8(first_output.body).unwrap();
    assert!(first_output_body.contains(r#""nextOffset":5"#));

    let mut waiting = request(3, "GET", "/mobile/output");
    waiting.query = vec![
        ("session_id".into(), "s1".into()),
        ("offset".into(), "5".into()),
        ("wait_ms".into(), "2000".into()),
    ];
    send(&mut stdin, &waiting);
    send(&mut stdin, &request(4, "GET", "/mobile/bootstrap"));
    let fast = receive(&mut stdout);
    assert_eq!((fast.id, fast.status), (4, 200));

    std::fs::OpenOptions::new()
        .append(true)
        .open(session_dir.join("output.bin"))
        .unwrap()
        .write_all(b" world")
        .unwrap();
    let resumed = receive(&mut stdout);
    assert_eq!((resumed.id, resumed.status), (3, 200));
    assert!(String::from_utf8(resumed.body)
        .unwrap()
        .contains(r#""nextOffset":11"#));

    let invalid = br#"{"id":5,"method":"POST","path":"/mobile/write","bodyB64":"%%%"}"#;
    write_frame(&mut stdin, FRAME_KIND_REQUEST, invalid).unwrap();
    assert_eq!(receive(&mut stdout).status, 400);
    send(&mut stdin, &request(6, "GET", "/mobile/not-a-route"));
    let unknown = receive(&mut stdout);
    assert_eq!((unknown.id, unknown.status), (6, 404));

    let large = vec![b'x'; 300 * 1024];
    std::fs::write(session_dir.join("output.bin"), &large).unwrap();
    let mut large_page = request(7, "GET", "/mobile/output");
    large_page.query = vec![
        ("session_id".into(), "s1".into()),
        ("offset".into(), "0".into()),
        ("limit".into(), large.len().to_string()),
    ];
    send(&mut stdin, &large_page);
    let large_response = receive(&mut stdout);
    assert_eq!((large_response.id, large_response.status), (7, 200));
    let large_body = String::from_utf8(large_response.body).unwrap();
    assert!(large_body.contains(r#""nextOffset":262144"#));
    let encoded = large_body
        .split_once("\"dataBase64\":\"")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    assert_eq!(encoded.len(), (256usize * 1024).div_ceil(3) * 4);
    assert!(encoded.starts_with("eHh4"));
    assert!(encoded.ends_with("eA=="));

    let mut first_mutation = request(8, "POST", "/mobile/write");
    first_mutation.auth = Some("wire-principal-a".into());
    first_mutation.body = br#"{"sessionID":"missing","data":"first"}"#.to_vec();
    send(&mut stdin, &first_mutation);
    assert_eq!(receive(&mut stdout).status, 404);
    let mut reused_id = first_mutation.clone();
    reused_id.auth = Some("wire-principal-b".into());
    reused_id.body = br#"{"sessionID":"missing","data":"different"}"#.to_vec();
    send(&mut stdin, &reused_id);
    assert_eq!(
        receive(&mut stdout).status,
        409,
        "wire auth must not select a different replay principal"
    );

    let mut create = request(9, "POST", "/mobile/sessions");
    create.body = br#"{"projectID":"p1","command":"printf ssh-create-proof"}"#.to_vec();
    send(&mut stdin, &create);
    let created = receive(&mut stdout);
    assert_eq!(created.status, 200);
    let created_id = json_string_field(&created.body, "sessionID");
    let created_dir = home.join("app-sessions").join(&created_id);
    assert!(wait_until(Duration::from_secs(10), || created_dir
        .join("manifest.json")
        .is_file()));
    assert!(wait_until(Duration::from_secs(10), || std::fs::read(
        created_dir.join("output.bin")
    )
    .is_ok_and(|bytes| bytes
        .windows(b"ssh-create-proof".len())
        .any(|window| window == b"ssh-create-proof"))));

    let mut remove = request(10, "POST", "/mobile/session-action");
    remove.body = format!(r#"{{"sessionID":"{created_id}","action":"remove"}}"#).into_bytes();
    send(&mut stdin, &remove);
    let removed = receive(&mut stdout);
    assert_eq!(
        removed.status,
        200,
        "{}",
        String::from_utf8_lossy(&removed.body)
    );
    assert!(wait_until(Duration::from_secs(5), || !created_dir.exists()));

    drop(stdin);
    let mut trailing_stdout = Vec::new();
    stdout.read_to_end(&mut trailing_stdout).unwrap();
    assert!(trailing_stdout.is_empty());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let audit = std::fs::read_to_string(home.join("remote").join("audit.log")).unwrap();
    assert!(audit.contains("ssh_stdio_start"));
    let euid = String::from_utf8(Command::new("id").arg("-u").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert!(audit.contains(&format!(r#""subject":"uid:{euid}""#)));
    assert!(!audit.contains("spoofed-environment-user"));
    assert!(!audit.contains("spoofed-controller-token"));

    std::fs::remove_dir_all(home).unwrap();
}
