"""Shared Link test fixtures: fake license API, fake relay, tombstone
helpers, and the signed test license key. Used by link_lifecycle.py and
link_enroll.py; lives outside cases/ so the runner does not execute it."""

import base64
import fcntl
import hashlib
import json
import os
import socket
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer


PUBLIC_KEY = "zt52Q3kzJSkUNuU8jrYCKlTDHycltUBp+siGzZ6ovDw="
LICENSE_KEY = (
    "CLRTY-eyJ2IjoxLCJpZCI6ImxpbmstdGVzdC1saWNlbnNlIiwiZW1haWwiOiJsaW5r"
    "LXRlc3RAZXhhbXBsZS5jb20iLCJwbGFuIjoicHJvIiwic2VhdHMiOjEsImlhdCI6"
    "MTc1NTEyOTYwMH0.iVrzCnPH8MSEvjIq1qVUnoQ7BLSCCd3AqVvwZ2IHfvTt6FHn"
    "l6Beo7aMKDW2AqbLb55_76YY3hMzftqFbd0kCQ"
)


class LicenseAPI:
    def __init__(self):
        self.requests = []
        self.reject_entitlement = False
        self.empty_rejection = False
        self.transient_entitlement = False
        self.block_activation = False
        self.block_entitlement = False
        self.activation_started = threading.Event()
        self.release_activation = threading.Event()
        self.entitlement_started = threading.Event()
        self.release_entitlement = threading.Event()
        self._lock = threading.Lock()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802 - BaseHTTPRequestHandler API
                length = int(self.headers.get("content-length", "0"))
                try:
                    body = json.loads(self.rfile.read(length) or b"{}")
                except ValueError:
                    body = {}
                with owner._lock:
                    owner.requests.append((self.path, body))
                    entitlement_number = sum(
                        path == "/api/remote/entitlement"
                        for path, _ in owner.requests
                    )
                    reject = owner.reject_entitlement
                    empty_rejection = owner.empty_rejection
                    transient = owner.transient_entitlement
                    block_activation = owner.block_activation
                    block = owner.block_entitlement
                if self.path == "/api/activate":
                    if block_activation:
                        owner.activation_started.set()
                        owner.release_activation.wait(timeout=10)
                    self.respond(200, {"ok": True})
                elif self.path == "/api/deactivate":
                    self.respond(200, {"ok": True})
                elif self.path == "/api/remote/entitlement" and reject:
                    if empty_rejection:
                        self.respond_empty(403)
                    else:
                        self.respond(
                            403,
                            {"error": "revoked", "reason": "license revoked"},
                        )
                elif self.path == "/api/remote/entitlement" and transient:
                    self.respond(503, {"error": "temporarily unavailable"})
                elif self.path == "/api/remote/entitlement":
                    if block:
                        owner.entitlement_started.set()
                        owner.release_entitlement.wait(timeout=10)
                    self.respond(
                        200,
                        {
                            "entitlement": f"UNPRE-issued-{entitlement_number}",
                            "expires_at": int(time.time()) + 30 * 24 * 60 * 60,
                        },
                    )
                else:
                    self.respond(404, {"error": "not found"})

            def respond(self, status, value):
                encoded = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(encoded)))
                self.send_header("connection", "close")
                self.end_headers()
                self.wfile.write(encoded)

            def respond_empty(self, status):
                self.send_response(status)
                self.send_header("content-length", "0")
                self.send_header("connection", "close")
                self.end_headers()

            def log_message(self, _format, *_args):
                pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def count(self, path):
        with self._lock:
            return sum(request_path == path for request_path, _ in self.requests)

    def set_rejected(self, rejected, empty=False):
        with self._lock:
            self.reject_entitlement = rejected
            self.empty_rejection = empty

    def set_transient(self, transient):
        with self._lock:
            self.transient_entitlement = transient

    def block_next_activation(self):
        with self._lock:
            self.block_activation = True
        self.activation_started.clear()
        self.release_activation.clear()

    def release_blocked_activation(self):
        with self._lock:
            self.block_activation = False
        self.release_activation.set()

    def block_next_entitlement(self):
        with self._lock:
            self.block_entitlement = True
        self.entitlement_started.clear()
        self.release_entitlement.clear()

    def release_blocked_entitlement(self):
        with self._lock:
            self.block_entitlement = False
        self.release_entitlement.set()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class FakeRelay:
    def __init__(self):
        self.server = socket.socket()
        self.server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.server.bind(("127.0.0.1", 0))
        self.server.listen()
        self.server.settimeout(0.2)
        self.port = self.server.getsockname()[1]
        self.accepted = 0
        self.active = 0
        self.authorizations = []
        # (uplink ordinal, [(deviceID, tokenHash), ...]) per hello frame, in
        # arrival order. A second entry for the same ordinal is an in-place
        # re-announcement over a still-open uplink.
        self.hellos = []
        self.rejected_authorizations = set()
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        while not self._stop.is_set():
            try:
                connection, _ = self.server.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=self._connection, args=(connection,), daemon=True).start()

    def _connection(self, connection):
        active = False
        try:
            connection.settimeout(0.5)
            head = b""
            while b"\r\n\r\n" not in head and len(head) < 16 * 1024:
                head += connection.recv(4096)
            text = head.decode("utf-8", "replace")
            headers = {}
            for line in text.split("\r\n")[1:]:
                if ":" in line:
                    name, value = line.split(":", 1)
                    headers[name.lower()] = value.strip()
            key = headers.get("sec-websocket-key", "")
            authorization = headers.get("authorization", "")
            with self._lock:
                self.authorizations.append(authorization)
                rejected = authorization in self.rejected_authorizations
            if rejected:
                connection.sendall(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                return
            accept = base64.b64encode(
                hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
                ).digest()
            ).decode()
            connection.sendall(
                (
                    "HTTP/1.1 101 Switching Protocols\r\n"
                    "Upgrade: websocket\r\n"
                    "Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
                ).encode()
            )
            with self._lock:
                self.accepted += 1
                self.active += 1
                ordinal = self.accepted
                active = True
            buffered = b""
            while not self._stop.is_set():
                try:
                    chunk = connection.recv(65536)
                except socket.timeout:
                    continue
                if not chunk:
                    return
                buffered += chunk
                while True:
                    frame, buffered = _pop_websocket_frame(buffered)
                    if frame is None:
                        break
                    opcode, payload = frame
                    if opcode == 0x2 and payload[:1] == b"\x01":
                        try:
                            hello = json.loads(payload[1:].decode("utf-8"))
                        except (UnicodeDecodeError, ValueError):
                            continue
                        devices = [
                            (entry.get("deviceID"), entry.get("tokenHash"))
                            for entry in hello.get("devices", [])
                        ]
                        with self._lock:
                            self.hellos.append((ordinal, devices))
        except OSError:
            pass
        finally:
            if active:
                with self._lock:
                    self.active = max(0, self.active - 1)
            try:
                connection.close()
            except OSError:
                pass

    def snapshot(self):
        with self._lock:
            return self.accepted, self.active, list(self.authorizations)

    def hello_log(self):
        with self._lock:
            return list(self.hellos)

    def reject_entitlement(self, entitlement):
        with self._lock:
            self.rejected_authorizations.add(f"Bearer {entitlement}")

    def close(self):
        self._stop.set()
        try:
            self.server.close()
        except OSError:
            pass
        self.thread.join(timeout=2)


def _pop_websocket_frame(buffered):
    """Parse one client-to-server WebSocket frame (RFC 6455, masked).

    Returns ((opcode, payload), rest) or (None, buffered) when incomplete.
    """
    if len(buffered) < 2:
        return None, buffered
    opcode = buffered[0] & 0x0F
    masked = bool(buffered[1] & 0x80)
    length = buffered[1] & 0x7F
    offset = 2
    if length == 126:
        if len(buffered) < 4:
            return None, buffered
        length = int.from_bytes(buffered[2:4], "big")
        offset = 4
    elif length == 127:
        if len(buffered) < 10:
            return None, buffered
        length = int.from_bytes(buffered[2:10], "big")
        offset = 10
    mask = b""
    if masked:
        if len(buffered) < offset + 4:
            return None, buffered
        mask = buffered[offset : offset + 4]
        offset += 4
    if len(buffered) < offset + length:
        return None, buffered
    payload = buffered[offset : offset + length]
    if masked:
        payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    return (opcode, payload), buffered[offset + length :]


def write_license(home):
    with open(home.path("link-license.json"), "w") as handle:
        json.dump({"key": LICENSE_KEY}, handle)
    os.chmod(home.path("link-license.json"), 0o600)


def write_entitlement(home, entitlement, expires_at, mac_id="headless-link-host"):
    with open(home.path("mobile", "relay-entitlement.json"), "w") as handle:
        json.dump(
            {
                "entitlement": entitlement,
                "expiresAt": expires_at,
                "macID": mac_id,
            },
            handle,
        )
    os.chmod(home.path("mobile", "relay-entitlement.json"), 0o600)


def tombstone_path(home):
    return home.path("link-disabled.json")


def write_tombstone(home, reason="user_disabled", generation="test-disable"):
    lock_path = home.path("link-license.lock")
    with open(lock_path, "a+") as lock:
        os.chmod(lock_path, 0o600)
        fcntl.flock(lock, fcntl.LOCK_EX)
        temporary = home.path(f".link-disabled.{uuid.uuid4()}.tmp")
        descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(descriptor, "w") as handle:
            json.dump(
                {
                    "version": 1,
                    "generation": generation,
                    "reason": reason,
                    "disabled_at": int(time.time()),
                },
                handle,
            )
        os.replace(temporary, tombstone_path(home))
        fcntl.flock(lock, fcntl.LOCK_UN)


def clear_tombstone(home):
    try:
        os.unlink(tombstone_path(home))
    except FileNotFoundError:
        pass


def read_tombstone(home):
    try:
        with open(tombstone_path(home)) as handle:
            return json.load(handle)
    except (FileNotFoundError, ValueError):
        return None


