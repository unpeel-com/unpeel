"""The native and headless launchers enter the same canonical Host runtime.

The unit adapters already run ``protocol/host-conformance-v1.json`` against
the historical Swift Host and the Rust Host router.  This case closes the
process-boundary gap: it sends every case in that same fixture over the real
``host.sock`` framed contract after launching the runtime exactly as each
product surface does:

* native app: bundled ``unpeel-host __serve__``;
* headless CLI: ``unpeel serve``.

Platform capability adapters are covered separately.  These two launches use
the same adapter-free fixture, so their status vector must be identical; any
launcher that accidentally enters a compatibility server or a second router
will diverge here.
"""

import base64
import json
import os
import signal
import socket
import struct
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, Home, mcp_post, mobile_request, run  # noqa: E402


HOST_BINARY = os.path.join(CRATES, "target", "debug", "unpeel-host")
CONFORMANCE_FIXTURE = os.path.join(
    os.path.dirname(CRATES), "protocol", "host-conformance-v1.json"
)


class HostLaunch:
    def __init__(self, home, executable, arguments):
        self.home = home
        self.log_path = home.path("host-launch.log")
        self.log = open(self.log_path, "w")
        environment = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1")
        self.process = subprocess.Popen(
            [executable, *arguments],
            cwd=CRATES,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=self.log,
            stderr=subprocess.STDOUT,
        )

    def ready(self, timeout=15):
        deadline = time.monotonic() + timeout
        status_path = self.home.path("serve.json")
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                return None
            try:
                with open(status_path) as handle:
                    status = json.load(handle)
                local_socket = status.get("localSocket")
                if (
                    status.get("pid") == self.process.pid
                    and local_socket
                    and os.path.exists(local_socket)
                ):
                    return status
            except (FileNotFoundError, OSError, ValueError):
                pass
            time.sleep(0.05)
        return None

    def output(self):
        try:
            self.log.flush()
            with open(self.log_path) as handle:
                return handle.read()
        except OSError:
            return ""

    def close(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if not self.log.closed:
            self.log.close()


class LocalHostClient:
    """Small UPL1 client for the persistent local Host socket."""

    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(20)
        self.socket.connect(path)

    def call(self, request_id, method, path, query=None, body=None):
        body_bytes = json.dumps(body).encode() if body is not None else b""
        payload = json.dumps(
            {
                "id": request_id,
                "method": method,
                "path": path,
                "query": query or {},
                "auth": None,
                "contentType": "application/json" if body is not None else None,
                "bodyB64": base64.b64encode(body_bytes).decode() if body_bytes else None,
            }
        ).encode()
        frame = b"UPL1" + bytes([1, 0, 0, 0]) + struct.pack(">I", len(payload)) + payload
        self.socket.sendall(frame)
        header = self._read_exact(12)
        if len(header) != 12 or header[:4] != b"UPL1" or header[4] != 2:
            raise RuntimeError(f"invalid Host response frame: {header!r}")
        length = struct.unpack(">I", header[8:12])[0]
        envelope = json.loads(self._read_exact(length))
        body_raw = (
            base64.b64decode(envelope["bodyB64"])
            if envelope.get("bodyB64")
            else b"{}"
        )
        try:
            response_body = json.loads(body_raw)
        except ValueError:
            response_body = {"raw": body_raw.decode("utf-8", "replace")}
        return envelope["status"], response_body

    def _read_exact(self, count):
        chunks = []
        remaining = count
        while remaining:
            chunk = self.socket.recv(remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)

    def close(self):
        try:
            self.socket.close()
        except OSError:
            pass


def seed_conformance_home(root):
    home = Home(root)
    home.project("conformance-project", "Conformance Project", "/tmp")
    home.preset("Claude", "claude", preset_id="conformance-preset")
    # Controller-assisted pairing exists only after one Controller already
    # trusts the Host. Seed that ordinary precondition so the worker owns a
    # live Direct endpoint while the fixture mints a second invitation.
    home.pair_device(token="conformance-controller-token", name="Controller")
    home.reserve_mobile_port()

    for session_id in [
        "conformance-session",
        "conformance-restart",
        "conformance-stop-exited",
        "conformance-action-restart",
        "conformance-restart-agent-exited",
        "conformance-resume-agent-exited",
        "conformance-remove",
        "conformance-broken",
    ]:
        home.session(
            session_id,
            label=session_id,
            command="claude",
            project_id="conformance-project",
        )

    for session_id in [
        "conformance-stop-live",
        "conformance-action-restart-agent",
        "conformance-action-resume-agent",
    ]:
        home.session(
            session_id,
            label=session_id,
            command="claude",
            project_id="conformance-project",
            running=True,
            extra_manifest={"host_protocol_version": 3},
        )
        home.seed_resume_data(session_id)
    return home


def run_fixture(home, launcher, fixture):
    ready = launcher.ready()
    if not ready:
        raise RuntimeError(f"Host launch never became ready: {launcher.output()}")
    client = LocalHostClient(ready["localSocket"])
    statuses = []
    bootstrap = None
    try:
        for index, item in enumerate(fixture["cases"], start=1):
            force_effect_failure = item["id"] == "session-action.effect-failure"
            order_path = home.path("session-order.json")
            if force_effect_failure:
                with open(order_path, "w") as handle:
                    json.dump([], handle)
            try:
                status, response = client.call(
                    index,
                    item["method"],
                    item["path"],
                    query=item.get("query"),
                    body=item.get("body"),
                )
            finally:
                if force_effect_failure:
                    try:
                        os.unlink(order_path)
                    except FileNotFoundError:
                        pass
            statuses.append((item["id"], status))
            if item["id"] == "bootstrap.valid":
                bootstrap = response
    finally:
        client.close()
    return statuses, bootstrap, ready


def answer_worker_approval_over_local_socket(home, ready, label):
    result = {}

    def request_approval():
        result["mcp"] = mcp_post(
            ready["hookPort"],
            "/mcp/approve-write",
            {
                "caller_session_id": f"{label}-caller",
                "target_session_id": "conformance-session",
            },
            token=home.auth_token,
        )

    waiter = threading.Thread(target=request_approval)
    waiter.start()
    client = LocalHostClient(ready["localSocket"])
    pending = None
    answer = (0, {})
    try:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            status, bootstrap = mobile_request(
                ready["directPort"],
                "/mobile/bootstrap",
                "conformance-controller-token",
            )
            if status == 200:
                pending = next(
                    (
                        item
                        for item in bootstrap.get("pendingApprovals", [])
                        if item.get("callerSessionID") == f"{label}-caller"
                    ),
                    None,
                )
            if pending:
                break
            time.sleep(0.05)
        if pending:
            answer = client.call(
                10_000,
                "POST",
                "/mobile/approvals/answer",
                body={"id": pending["id"], "approved": True},
            )
    finally:
        client.close()
    waiter.join(timeout=12)
    return pending, answer, result.get("mcp"), waiter.is_alive()


def body(case):
    with open(CONFORMANCE_FIXTURE) as handle:
        fixture = json.load(handle)
    case.check(
        "the canonical conformance fixture is versioned and nontrivial",
        fixture.get("schemaVersion") == 1 and len(fixture.get("cases", [])) >= 70,
        str((fixture.get("schemaVersion"), len(fixture.get("cases", [])))),
    )

    launch_specs = [
        ("native", HOST_BINARY, ["__serve__"]),
        ("headless", BINARY, ["serve"]),
    ]
    results = {}
    homes = []
    for name, executable, arguments in launch_specs:
        home = seed_conformance_home(case.home.path(name))
        homes.append(home)
        launcher = HostLaunch(home, executable, arguments)
        try:
            statuses, bootstrap, ready = run_fixture(home, launcher, fixture)
            results[name] = statuses
            descriptor = (bootstrap or {}).get("hostProtocol") or {}
            case.check(
                f"{name} launcher reaches the canonical Host protocol",
                len(statuses) == len(fixture["cases"])
                and descriptor.get("majorVersion") == 1
                and ready.get("localSocket"),
                str((ready, descriptor, statuses[-3:])),
            )
            # Lane D: additive isolation/environment bootstrap fields, both
            # Host kinds. The tier is always present and valid; a Linux CI
            # guest is a container/vm, a bare host reports host. hostEnvironment
            # is optional (only inside a Box), so absence is fine here.
            boot = bootstrap or {}
            case.check(
                f"{name} advertises a valid hostIsolationTier",
                boot.get("hostIsolationTier") in ("vm", "container", "host"),
                str(boot.get("hostIsolationTier")),
            )
            environment = boot.get("hostEnvironment")
            case.check(
                f"{name} hostEnvironment is absent or a Box descriptor",
                environment is None
                or (environment.get("kind") == "box" and bool(environment.get("id"))),
                str(environment),
            )
            pending, answer, mcp_result, waiter_alive = (
                answer_worker_approval_over_local_socket(home, ready, name)
            )
            case.check(
                f"{name} local client answers the worker-owned approval queue",
                bool(pending)
                and answer[0] == 200
                and mcp_result == (200, {"approved": True})
                and not waiter_alive,
                str((pending, answer, mcp_result, waiter_alive)),
            )
        finally:
            launcher.close()

    native = results.get("native", [])
    headless = results.get("headless", [])
    mismatches = [
        (left, right)
        for left, right in zip(native, headless)
        if left != right
    ]
    case.check(
        "native and headless launches run every shared conformance case identically",
        len(native) == len(headless) == len(fixture["cases"]) and not mismatches,
        str(mismatches[:8]),
    )
    expected = [
        (item["id"], item["expected"]["tui"])
        for item in fixture["cases"]
    ]
    expectation_mismatches = [
        (actual, wanted)
        for actual, wanted in zip(headless, expected)
        if actual != wanted
    ]
    case.check(
        "both process launches satisfy the adapter-free fixture expectations",
        len(headless) == len(expected) and not expectation_mismatches,
        str(expectation_mismatches[:8]),
    )

    for home in homes:
        home.cleanup()


if __name__ == "__main__":
    run("host_launch_conformance", body)
