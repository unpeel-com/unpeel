"""Relay conformance, server half: the Host's Link uplink dials the relay
with its entitlement, announces paired devices in its hello frame, and the
Rust E2E crypto reproduces the cross-language known-answer vectors in
protocol/relay-kat-vectors-v1.json (the same vectors the Swift CryptoKit and
Worker WebCrypto implementations are pinned to).

The Swift-oracle half — the SHIPPED phone crypto (RelayProtocol.swift)
completing a forward-secret handshake and sealed /mobile round-trips against
this Host through a stand-in relay — builds the shipped Swift sources from
clients/shared/UnpeelShared and needs an Apple toolchain (CryptoKit). It is
skipped here with a NOTE unless UNPEEL_RELAY_SWIFT_ORACLE=1 is set."""

import sys, os, json, socket, base64, hashlib, struct, subprocess, time, threading, shutil

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, REPO  # noqa: E402

TESTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SHARED = os.path.join(REPO, "clients", "shared", "UnpeelShared", "Sources", "UnpeelShared")


VECTORS = os.path.join(REPO, "protocol", "relay-kat-vectors-v1.json")


def check_vectors(case):
    """Rust-side KAT: cargo test in unpeel-core replays the vectors; here we
    only prove the published contract file is present and well-formed so a
    drift shows up in this matrix, not only in cargo."""
    try:
        with open(VECTORS) as f:
            vectors = json.load(f)
    except (OSError, ValueError) as e:
        case.check("protocol/relay-kat-vectors-v1.json is present and valid JSON", False, str(e))
        return
    ok = all(isinstance(vectors.get(k), str) and base64.b64decode(vectors[k])
             for k in ("transcriptMAC", "sealedFrame"))
    case.check("protocol/relay-kat-vectors-v1.json carries transcriptMAC + sealedFrame", ok,
               str(vectors)[:120])
    r = subprocess.run(["cargo", "test", "-q", "--manifest-path",
                        os.path.join(REPO, "crates", "Cargo.toml"), "-p", "unpeel-core",
                        "--lib", "relay_crypto::tests::known_answer_vectors_match_swift_and_js"],
                       capture_output=True, text=True, timeout=900)
    case.check("Rust relay crypto reproduces the Swift/JS known-answer vectors",
               r.returncode == 0 and "1 passed" in r.stdout, (r.stdout + r.stderr)[-400:])


def build_oracle(dest):
    if os.environ.get("UNPEEL_RELAY_SWIFT_ORACLE") != "1":
        return None
    if not shutil.which("swiftc"):
        return None
    sources = [os.path.join(TESTS, "relayclient", "main.swift"),
               os.path.join(SHARED, "RelayProtocol.swift")]
    if not all(os.path.exists(s) for s in sources):
        return None
    os.makedirs(dest, exist_ok=True)
    binary = os.path.join(dest, "relayclient")
    r = subprocess.run(["swiftc", "-O", "-o", binary, *sources],
                       capture_output=True, text=True, timeout=300)
    return binary if r.returncode == 0 else None


def body(case):
    check_vectors(case)
    oracle = build_oracle(case.home.path("build"))
    if not oracle:
        if os.environ.get("UNPEEL_RELAY_SWIFT_ORACLE") == "1":
            case.check("Swift oracle requested but swiftc/clients/shared unavailable", False)
        else:
            case.note("Swift-oracle handshake half skipped (needs an Apple toolchain and "
                      "clients/shared); set "
                      "UNPEEL_RELAY_SWIFT_ORACLE=1 to require it")
        return
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.preset(label="Relay cat", command="cat", preset_id="relay-cat")
    home.session("s1", label="a session", project_id="p")
    home.session("s-live", label="live relay session", command="cat",
                 project_id="p", running=True)
    live_host = case.host("s-live")
    token = home.pair_device()          # devices.json with real tokenHash
    # relay registration + e2e key + entitlement, as TUI pairing would write
    dv = json.load(open(home.path("mobile/devices.json")))
    dv["devices"][0]["relayTokenHash"] = "ab" * 32
    json.dump(dv, open(home.path("mobile/devices.json"), "w"))
    e2e = os.urandom(32)
    open(home.path("mobile/mac-id"), "w").write("mac-1\n")
    json.dump({"mac-1.dev1": base64.b64encode(e2e).decode()},
              open(home.path("mobile/e2e-keys.json"), "w"))
    json.dump({"expiresAt": int(time.time()) + 3600, "entitlement": "UNPRE-test",
               "macID": "mac-1"}, open(home.path("mobile/relay-entitlement.json"), "w"))
    home.reserve_mobile_port()

    # ── fake relay: WS server speaking the Worker's framing ──
    srv = socket.socket(); srv.bind(("127.0.0.1", 0)); srv.listen(1)
    relay_port = srv.getsockname()[1]
    state = {}
    def ws_recv(c):
        h = c.recv(2); ln = h[1] & 0x7f; off = 0
        if ln == 126: ln = struct.unpack(">H", c.recv(2))[0]
        elif ln == 127: ln = struct.unpack(">Q", c.recv(8))[0]
        mask = c.recv(4) if h[1] & 0x80 else b""
        p = b""
        while len(p) < ln: p += c.recv(ln - len(p))
        if mask: p = bytes(b ^ mask[i % 4] for i, b in enumerate(p))
        return h[0] & 0x0f, p
    def ws_send(c, p, op=0x2):
        hdr = bytes([0x80 | op])
        if len(p) < 126: hdr += bytes([len(p)])
        elif len(p) < 65536: hdr += bytes([126]) + struct.pack(">H", len(p))
        else: hdr += bytes([127]) + struct.pack(">Q", len(p))
        c.sendall(hdr + p)
    def accept_host():
        c, _ = srv.accept()
        head = b""
        while b"\r\n\r\n" not in head: head += c.recv(4096)
        text = head.decode(errors="replace")
        state["auth_header"] = "Bearer UNPRE-test" in text
        state["path"] = text.split(" ")[1]
        key = [l.split(":",1)[1].strip() for l in text.split("\r\n") if l.lower().startswith("sec-websocket-key")][0]
        acc = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        c.sendall(f"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {acc}\r\n\r\n".encode())
        c.settimeout(20)
        state["conn"] = c
    threading.Thread(target=accept_host, daemon=True).start()

    service = case.serve(env={"UNPEEL_RELAY_URL": f"ws://127.0.0.1:{relay_port}"})
    service.ready()
    deadline = time.time() + 30
    while "conn" not in state and time.time() < deadline: time.sleep(0.3)
    case.check("host uplink connected to the relay", "conn" in state)
    case.check("with the entitlement as bearer", state.get("auth_header"))
    case.check("to /v1/host/<macID>", state.get("path") == "/v1/host/mac-1")

    c = state["conn"]
    op, hello = ws_recv(c)
    while op != 0x2: op, hello = ws_recv(c)
    h = json.loads(hello[1:])
    case.check("hello announces the paired device",
               hello[0] == 1 and h["devices"][0]["deviceID"] == "dev1"
               and h["devices"][0]["tokenHash"] == "ab" * 32, str(h)[:120])

    oracle = subprocess.Popen([oracle], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, text=True, bufsize=1)
    def ask(o):
        oracle.stdin.write(json.dumps(o) + "\n"); oracle.stdin.flush()
        return json.loads(oracle.stdout.readline())
    r = ask({"op": "hello", "e2eKeyB64": base64.b64encode(e2e).decode(), "deviceID": "dev1"})
    payload = base64.b64decode(r["payloadB64"])
    frame = bytes([4]) + struct.pack(">I", 7) + bytes([4]) + b"dev1" + payload
    ws_send(c, frame)
    def host_data():
        while True:
            op, f = ws_recv(c)
            if op == 0x9: ws_send(c, f, 0xA); continue
            if op == 0x2 and f[0] == 2: return f[5:]
    # RemoteMacClient sends the complete HTTP Authorization value through
    # RelayTunnelRequest (not a bare token), along with the original MIME type.
    # Successful bootstrap proves Relay enters the authenticated LAN-equivalent
    # mobile pipeline instead of manufacturing `Bearer Bearer ...`.
    binary_body = b"\x89PNG\r\n\x1a\n\x00\xffrelay-fixture"
    binary_body_b64 = base64.b64encode(binary_body).decode()
    content_type = "image/png"
    authorization = f"Bearer {token}"
    r = ask({"op": "finish", "hostHelloB64": base64.b64encode(host_data()).decode(),
             "auth": authorization, "contentType": content_type,
             "bodyB64": binary_body_b64})
    case.check("shipped crypto accepts the host's handshake MAC", "error" not in r, str(r)[:120])
    encoded_request = json.loads(base64.b64decode(r["requestJSONB64"]))
    case.check("shipped request preserves auth, MIME type, and arbitrary binary bytes",
               encoded_request.get("auth") == authorization
               and encoded_request.get("contentType") == content_type
               and encoded_request.get("bodyB64") == binary_body_b64,
               str(encoded_request)[:180])
    ws_send(c, bytes([4]) + struct.pack(">I", 7) + bytes([4]) + b"dev1" + base64.b64decode(r["frameB64"]))
    resp = ask({"op": "open", "frameB64": base64.b64encode(host_data()).decode()})
    body = json.loads(resp["plaintext"])
    inner = json.loads(base64.b64decode(body["bodyB64"]))
    case.check("a shipped-iOS-style authenticated request reaches the mobile pipeline",
               body["status"] == 200 and inner.get("protocolVersion") == 1, str(body)[:140])

    # A phone keeps an output long-poll outstanding for the session it is
    # viewing while its input and resize calls use the same Link connection.
    # The Host must keep receiving and dispatching later request ids rather
    # than parking the whole encrypted connection behind that poll.
    output_path = home.path("app-sessions", "s1", "output.bin")
    output_offset = os.path.getsize(output_path)

    def seal(request_id, method, path, query=None, body_bytes=None,
             request_content_type=None):
        request = {
            "op": "seal",
            "id": request_id,
            "method": method,
            "path": path,
            "query": query or {},
            "auth": authorization,
        }
        if body_bytes is not None:
            request["bodyB64"] = base64.b64encode(body_bytes).decode()
        if request_content_type is not None:
            request["contentType"] = request_content_type
        encoded = ask(request)
        case.check(f"shipped crypto seals tunnel request {request_id}",
                   "error" not in encoded, str(encoded)[:120])
        return base64.b64decode(encoded["frameB64"])

    def send_phone_payload(payload):
        ws_send(c, bytes([4]) + struct.pack(">I", 7)
                + bytes([4]) + b"dev1" + payload)

    def open_response(payload):
        opened = ask({"op": "open", "frameB64": base64.b64encode(payload).decode()})
        return json.loads(opened["plaintext"])

    poll_frame = seal(
        2,
        "GET",
        "/mobile/output",
        query={
            "session_id": "s1",
            "offset": str(output_offset),
            "limit": "1024",
            "wait_ms": "5000",
        },
    )
    write_text = "relay stays interactive"
    write_frame = seal(
        3,
        "POST",
        "/mobile/write",
        body_bytes=json.dumps({
            "sessionID": "s-live",
            "data": write_text,
        }).encode(),
        request_content_type="application/json",
    )
    send_phone_payload(poll_frame)
    started = time.monotonic()
    send_phone_payload(write_frame)

    # Do not release the poll until this deadline has passed. The old serial
    # dispatcher produces no response and no write here; a multiplexed Host
    # returns id=3 immediately even though id=2 is still asleep.
    quick_response = None
    c.settimeout(1.5)
    try:
        quick_response = open_response(host_data())
    except socket.timeout:
        pass
    finally:
        c.settimeout(20)
    quick_elapsed = time.monotonic() - started
    write_before_release = live_host.writes[-1:] == [write_text]
    case.check(
        "an output long-poll does not head-of-line block a later Link write",
        quick_response is not None
        and quick_response.get("id") == 3
        and quick_response.get("status") == 200
        and write_before_release
        and quick_elapsed < 1.5,
        f"response={quick_response} elapsed={quick_elapsed:.2f} "
        f"writes={live_host.writes!r}",
    )

    # Release the held request and drain both ids even on failure, keeping the
    # crypto receive counter in actual wire order. On the healthy path id=3
    # is already open and id=2 now returns the newly appended bytes.
    release_bytes = b"relay-long-poll-release\r\n"
    with open(output_path, "ab") as output_file:
        output_file.write(release_bytes)
    responses = {}
    if quick_response is not None:
        responses[quick_response.get("id")] = quick_response
    drain_deadline = time.monotonic() + 8
    while not {2, 3}.issubset(responses) and time.monotonic() < drain_deadline:
        try:
            response = open_response(host_data())
            responses[response.get("id")] = response
        except socket.timeout:
            break

    poll_response = responses.get(2, {})
    try:
        poll_body = json.loads(base64.b64decode(poll_response.get("bodyB64", "")))
        poll_bytes = base64.b64decode(poll_body.get("dataBase64", ""))
    except (ValueError, TypeError):
        poll_body = {}
        poll_bytes = b""
    case.check(
        "the outstanding encrypted poll resumes at its original offset",
        poll_response.get("status") == 200
        and poll_body.get("offset") == output_offset
        and poll_body.get("nextOffset") == output_offset + len(release_bytes)
        and poll_bytes == release_bytes,
        str({"responses": responses, "poll": poll_body})[:240],
    )

    # The resumable upload operation is deliberately sized for Link's full
    # JSON/base64 + AEAD envelope, not merely direct HTTP. Send the largest
    # permitted raw chunk through the shipped Swift crypto, ignore its first
    # application receipt as if it were lost, retry the same range, publish,
    # and then prove gallery read/delete across the same encrypted tunnel.
    upload_id = "11111111-2222-4333-8444-555555555555"
    upload_bytes = b"\x89PNG\r\n\x1a\n" + b"r" * (262_144 - 8) + b"relay-final"
    upload_sha256 = hashlib.sha256(upload_bytes).hexdigest()
    upload_query = {
        "session_id": "s1",
        "upload_id": upload_id,
        "total_size": str(len(upload_bytes)),
        "sha256": upload_sha256,
    }

    def encrypted_round_trip(request_id, method, path, query=None,
                             body_bytes=None, mime=None):
        frame = seal(
            request_id,
            method,
            path,
            query=query,
            body_bytes=body_bytes,
            request_content_type=mime,
        )
        send_phone_payload(frame)
        response = open_response(host_data())
        try:
            response_body = json.loads(base64.b64decode(response.get("bodyB64", "")))
        except (ValueError, TypeError):
            response_body = {}
        return response, response_body

    first_query = dict(upload_query, offset="0")
    first_frame = seal(
        4,
        "POST",
        "/mobile/upload-chunk",
        query=first_query,
        body_bytes=upload_bytes[:262_144],
        request_content_type="image/png",
    )
    case.check(
        "a maximum-sized upload chunk fits the shipped Link frame",
        len(first_frame) <= 512 * 1024,
        f"sealed bytes={len(first_frame)}",
    )
    send_phone_payload(first_frame)
    lost_response = open_response(host_data())
    # Decrypt to preserve the forward-only counter, but intentionally ignore
    # the semantic receipt before retrying under a fresh transport request id.
    lost_body = json.loads(base64.b64decode(lost_response.get("bodyB64", "")))
    retry_response, retry_body = encrypted_round_trip(
        5,
        "POST",
        "/mobile/upload-chunk",
        query=first_query,
        body_bytes=upload_bytes[:262_144],
        mime="image/png",
    )
    case.check(
        "Link upload retry after response loss is durably idempotent",
        lost_response.get("status") == 200
        and lost_body.get("nextOffset") == 262_144
        and retry_response.get("status") == 200
        and retry_body.get("nextOffset") == 262_144
        and retry_body.get("complete") is False,
        str((lost_response, lost_body, retry_response, retry_body))[:260],
    )

    final_response, final_body = encrypted_round_trip(
        6,
        "POST",
        "/mobile/upload-chunk",
        query=dict(upload_query, offset="262144"),
        body_bytes=upload_bytes[262_144:],
        mime="image/png",
    )
    upload_name = final_body.get("name", "")
    case.check(
        "Link publishes one validated Host-side upload",
        final_response.get("status") == 200
        and final_body.get("complete") is True
        and final_body.get("kind") == "uploads"
        and final_body.get("contentType") == "image/png"
        and final_body.get("sha256") == upload_sha256
        and bool(upload_name),
        str((final_response, final_body))[:260],
    )

    list_response, list_body = encrypted_round_trip(
        7,
        "GET",
        "/mobile/artifacts",
        query={"session_id": "s1"},
    )
    read1_response, read1_body = encrypted_round_trip(
        8,
        "GET",
        "/mobile/artifact",
        query={
            "session_id": "s1",
            "kind": "uploads",
            "name": upload_name,
            "limit": "200000",
        },
    )
    read2_response, read2_body = encrypted_round_trip(
        9,
        "GET",
        "/mobile/artifact",
        query={
            "session_id": "s1",
            "kind": "uploads",
            "name": upload_name,
            "offset": str(read1_body.get("nextOffset", 0)),
            "limit": "200000",
        },
    )
    round_trip_bytes = (
        base64.b64decode(read1_body.get("dataBase64", ""))
        + base64.b64decode(read2_body.get("dataBase64", ""))
    )
    case.check(
        "Link gallery list and ranged reads preserve uploaded bytes and MIME",
        list_response.get("status") == 200
        and sum(item.get("kind") == "uploads" and item.get("name") == upload_name
                for item in list_body.get("artifacts", [])) == 1
        and read1_response.get("status") == 200
        and read2_response.get("status") == 200
        and read1_body.get("contentType") == "image/png"
        and read2_body.get("nextOffset") == len(upload_bytes)
        and read2_body.get("totalSize") == len(upload_bytes)
        and round_trip_bytes == upload_bytes,
        str((list_response, list_body, read1_response, read2_response))[:300],
    )
    delete_response, delete_body = encrypted_round_trip(
        10,
        "POST",
        "/mobile/artifact-delete",
        query={
            "session_id": "s1",
            "kind": "uploads",
            "name": upload_name,
        },
    )
    case.check(
        "Link deletes the uploaded Host file without a cloud copy",
        delete_response.get("status") == 200 and delete_body.get("ok") == "true",
        str((delete_response, delete_body)),
    )

    # Session creation uses the same semantic request id as the shared Host
    # router. Re-seal an identical request under the same id after consuming
    # (but pretending to lose) its first receipt: only one Host PTY may exist.
    create_before = set(home.manifests())
    create_payload = json.dumps({
        "projectID": "p",
        "presetID": "relay-cat",
        "initialText": "hello from Link create",
        "initialTextSubmitMode": "pasteAndSubmit",
    }).encode()
    first_create_response, first_create_body = encrypted_round_trip(
        11,
        "POST",
        "/mobile/sessions",
        body_bytes=create_payload,
        mime="application/json",
    )
    replay_create_response, replay_create_body = encrypted_round_trip(
        11,
        "POST",
        "/mobile/sessions",
        body_bytes=create_payload,
        mime="application/json",
    )
    created_id = first_create_body.get("sessionID")
    create_deadline = time.monotonic() + 12
    created_output = b""
    while created_id and time.monotonic() < create_deadline:
        try:
            created_output = open(
                home.path("app-sessions", created_id, "output.bin"), "rb"
            ).read()
        except OSError:
            created_output = b""
        if b"hello from Link create" in created_output:
            break
        time.sleep(0.1)
    create_after = home.manifests()
    created_manifest = create_after.get(created_id, {})
    case.check(
        "Link create response-loss replay launches exactly one Host Session",
        first_create_response.get("status") == 200
        and replay_create_response.get("status") == 200
        and replay_create_body == first_create_body
        and bool(created_id)
        and set(create_after).difference(create_before) == {created_id}
        and created_manifest.get("session", {}).get("project_id") == "p"
        and created_manifest.get("session", {}).get("command") == "cat"
        and created_manifest.get("cwd") == "/tmp"
        and b"hello from Link create" in created_output,
        str((first_create_response, first_create_body,
             replay_create_response, replay_create_body, created_manifest))[:360],
    )
    oracle.kill()
    case.check("done", True)


run("relay_conformance", body)
