"""Restart-free core upgrades: `unpeel-host __pty_core__ --takeover` moves
every Session (PTY, control socket, attached stream clients) from the
running core to a new one over SCM_RIGHTS. Terminals keep their screens
byte for byte, the journal stays continuous, an attached client keeps
streaming, the old core exits, and the serve supervisor drives the same
takeover when it adopts a core built from a different binary."""

import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, run, run_cli, wait_running  # noqa: E402

HOST_BIN = os.path.join(CRATES, "target", "debug", "unpeel-host")


def core_request(home, request, timeout=12.0):
    sock = socket.socket(socket.AF_UNIX)
    sock.settimeout(timeout)
    sock.connect(home.path("pty-core.sock"))
    sock.sendall((json.dumps(request) + "\n").encode())
    chunks = b""
    while not chunks.endswith(b"\n"):
        piece = sock.recv(4096)
        if not piece:
            break
        chunks += piece
    sock.close()
    return json.loads(chunks.decode() or "{}")


def core_record(home):
    with open(home.path("pty-core.json")) as handle:
        return json.load(handle)


def start_core(home, binary=HOST_BIN, extra_args=()):
    env = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1")
    env.pop("UNPEEL_PTY_CORE", None)
    core = subprocess.Popen(
        [binary, "__pty_core__", *extra_args],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    end = time.monotonic() + 15
    while time.monotonic() < end:
        try:
            if core_record(home)["pid"] == core.pid:
                return core
        except (FileNotFoundError, ValueError, KeyError):
            pass
        if core.poll() is not None:
            break
        time.sleep(0.05)
    return core


class CoreStopper:
    def __init__(self, *cores):
        self.cores = list(cores)

    def close(self):
        for core in self.cores:
            if core.poll() is None:
                core.kill()


class StreamClient:
    """A raw StreamOutput client: records every frame it receives."""

    def __init__(self, home, session_id, offset):
        self.frames = []
        self.closed = False
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(60)
        self.sock.connect(home.path("app-sessions", session_id, "session.sock"))
        self.sock.sendall(
            (json.dumps({"type": "stream_output", "offset": offset, "answers_queries": False}) + "\n").encode()
        )
        self.thread = threading.Thread(target=self._pump, daemon=True)
        self.thread.start()

    def _read(self, n):
        buf = b""
        while len(buf) < n:
            try:
                piece = self.sock.recv(n - len(buf))
            except OSError:
                return None
            if not piece:
                return None
            buf += piece
        return buf

    def _pump(self):
        while True:
            header = self._read(13)
            if header is None:
                self.closed = True
                return
            length, next_offset, flags = struct.unpack(">IQB", header)
            data = self._read(length) if length else b""
            self.frames.append((next_offset, flags, data or b""))
            if flags & 1:
                self.closed = True
                return

    def received(self):
        return b"".join(data for _, _, data in self.frames)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def new_session(case, home, label, preset="cat"):
    started = run_cli(home, ["new", "--preset", preset, "--project", "p"], timeout=45)
    session_id = ""
    for token in started.stdout.split():
        if len(token) == 36 and token.count("-") == 4:
            session_id = token
    case.check(f"new starts {label}", started.returncode == 0 and bool(session_id),
               started.stdout[:160] + started.stderr[:200])
    case.check(f"{label} host comes up", wait_running(home, session_id), "never running")
    return session_id


def screen(home, session_id):
    return run_cli(home, ["screen", session_id]).stdout


def journal(home, session_id):
    with open(home.path("app-sessions", session_id, "output.bin"), "rb") as handle:
        return handle.read()


def wait_for(predicate, timeout=15.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(0.1)
    return predicate()


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.preset(label="cat", command="cat")
    home.preset(label="sh", command="sh")

    old_core = start_core(home)
    stopper = CoreStopper(old_core)
    case.track(stopper)
    case.check("the old core is up", old_core.poll() is None and core_record(home)["pid"] == old_core.pid)

    ids = [new_session(case, home, f"session {index}", preset="sh" if index == 1 else "cat") for index in range(5)]
    for index, session_id in enumerate(ids):
        run_cli(home, ["send", session_id, f"marker-{index}-before", "--enter"])
    time.sleep(1.5)
    case.check("all five sessions are hosted by the old core", core_request(home, {"op": "ping"}).get("sessions") == 5)

    # A live stream client attached to the first session at the journal tail.
    first = ids[0]
    tail = len(journal(home, first))
    client = StreamClient(home, first, tail)
    case.track(client)
    time.sleep(0.5)
    run_cli(home, ["send", first, "streamed-before", "--enter"])
    case.check(
        "the client streams from the old core",
        wait_for(lambda: b"streamed-before" in client.received()),
        repr(client.received()[-200:]),
    )

    # Output in flight during the move: a loop printing lines in one session.
    busy = ids[1]
    run_cli(home, ["send", busy, "i=0; while [ $i -lt 400 ]; do echo tick-$i; sleep 0.01; i=$((i+1)); done", "--enter"])
    time.sleep(0.3)

    before_screens = {session_id: screen(home, session_id) for session_id in ids if session_id != busy}
    before_journals = {session_id: len(journal(home, session_id)) for session_id in ids}
    before_children = {session_id: home.manifests()[session_id]["pid"] for session_id in ids}

    new_core = start_core(home, extra_args=("--takeover",))
    stopper.cores.append(new_core)
    case.check(
        "the takeover core publishes the record under its own pid",
        wait_for(lambda: core_record(home)["pid"] == new_core.pid, timeout=20),
        str(core_record(home)),
    )
    try:
        old_core.wait(timeout=20)
        old_exited = True
    except subprocess.TimeoutExpired:
        old_exited = False
    case.check("the old core exits 0 after the handoff", old_exited and old_core.returncode == 0,
               f"rc={old_core.returncode}")
    case.check("the socket path still answers, from the new core",
               core_request(home, {"op": "ping"}).get("pid") == new_core.pid)
    case.check("the new core hosts all five sessions", core_request(home, {"op": "ping"}).get("sessions") == 5)

    for session_id, text in before_screens.items():
        case.check(
            f"screen of {session_id[:8]} is byte-identical after the move",
            screen(home, session_id) == text,
            f"before={text[-120:]!r} after={screen(home, session_id)[-120:]!r}",
        )
    for session_id in ids:
        manifest = home.manifests()[session_id]
        case.check(
            f"{session_id[:8]} keeps its shell (pid) and is running under the new core",
            manifest["state"] == "running"
            and manifest["pid"] == before_children[session_id]
            and manifest.get("host_pid") == new_core.pid,
            str({k: manifest.get(k) for k in ("state", "pid", "host_pid")}),
        )
        case.check(
            f"{session_id[:8]} journal only grew",
            len(journal(home, session_id)) >= before_journals[session_id],
        )

    # The busy session's loop keeps printing through the move with no gap
    # or duplicate in the journal.
    def ticks_done():
        data = journal(home, busy)
        return b"tick-399" in data
    case.check("output in flight completes on the new core", wait_for(ticks_done, timeout=30))
    ticks = [int(line.split(b"tick-")[1]) for line in journal(home, busy).split(b"\r\n") if line.startswith(b"tick-")]
    expected = list(range(400))
    case.check(
        "the busy journal is continuous across the move (no gap, no duplicate)",
        ticks == expected,
        f"got {len(ticks)} ticks; first mismatch at {next((i for i, (a, b) in enumerate(zip(ticks, expected)) if a != b), None)}",
    )

    # The attached client kept its socket and keeps receiving.
    run_cli(home, ["send", first, "streamed-after", "--enter"])
    case.check(
        "the attached client keeps streaming after the move on the same socket",
        wait_for(lambda: b"streamed-after" in client.received()) and not client.closed,
        repr(client.received()[-200:]) + f" closed={client.closed}",
    )
    offsets = [next_offset for next_offset, _, _ in client.frames]
    case.check("the client's frame offsets are monotonic", offsets == sorted(offsets) and len(set(offsets)) == len(offsets))
    received = client.received()
    wait_for(lambda: len(journal(home, first)) >= tail + len(received))
    case.check(
        "the client's byte stream matches the journal tail exactly once",
        received == journal(home, first)[tail:tail + len(received)],
        f"received={received[-160:]!r} journal={journal(home, first)[tail:tail + len(received)][-160:]!r}",
    )

    # Input still reaches the moved PTYs through the same control socket.
    run_cli(home, ["send", ids[2], "after-move-input", "--enter"])
    case.check("input after the move reaches the terminal", wait_for(lambda: "after-move-input" in screen(home, ids[2])))

    # A second takeover works the same way (the lock and listener moved).
    third_core = start_core(home, extra_args=("--takeover",))
    stopper.cores.append(third_core)
    case.check("a second takeover succeeds", wait_for(lambda: core_record(home)["pid"] == third_core.pid, timeout=20))
    try:
        new_core.wait(timeout=20)
        second_exited = new_core.returncode == 0
    except subprocess.TimeoutExpired:
        second_exited = False
    case.check("the intermediate core exits 0", second_exited)
    case.check("sessions survive two moves", core_request(home, {"op": "ping"}).get("sessions") == 5)

    # rm still stops and cleans a moved session.
    removed = run_cli(home, ["rm", ids[3]], timeout=45)
    case.check(
        "rm stops a moved session and deletes its dir",
        removed.returncode == 0 and not os.path.exists(home.path("app-sessions", ids[3])),
        removed.stderr[:200],
    )
    case.check("the core releases it", wait_for(lambda: core_request(home, {"op": "ping"}).get("sessions") == 4))

    # Serve supervisor: adopt a core built from a different binary and take
    # it over automatically (UNPEEL_PTY_CORE=1). The "different build" is a
    # copy of unpeel-host with an older mtime.
    for session_id in (ids[0], ids[1], ids[2], ids[4]):
        run_cli(home, ["rm", session_id], timeout=45)
    case.check("all sessions removed before the supervisor test", wait_for(lambda: core_request(home, {"op": "ping"}).get("sessions") == 0))
    core_request(home, {"op": "shutdown"})
    try:
        third_core.wait(timeout=15)
    except subprocess.TimeoutExpired:
        pass

    stale_bin = home.path("stale-unpeel-host")
    shutil.copy2(HOST_BIN, stale_bin)
    os.utime(stale_bin, (1_600_000_000, 1_600_000_000))
    stale_core = start_core(home, binary=stale_bin)
    stopper.cores.append(stale_core)
    stale_record = core_record(home)
    case.check("a core from the stale binary is up", stale_core.poll() is None and stale_record["pid"] == stale_core.pid)
    stale_session = new_session(case, home, "session under the stale core")
    run_cli(home, ["send", stale_session, "stale-core-text", "--enter"])
    case.check("the stale core's session echoes", wait_for(lambda: screen(home, stale_session).count("stale-core-text") >= 2))
    time.sleep(1.0)
    stale_screen = screen(home, stale_session)

    serve = case.serve(env={"UNPEEL_PTY_CORE": "1", "UNPEEL_PTY_CORE_TAKEOVER": "1"})
    serve.ready()

    def serve_json():
        try:
            with open(home.path("serve.json")) as handle:
                return json.load(handle)
        except (FileNotFoundError, ValueError):
            return {}

    states = []

    def observe():
        state = serve_json().get("ptyCore", {}).get("state")
        if state and (not states or states[-1] != state):
            states.append(state)
        return state == "live" and core_record(home)["pid"] != stale_core.pid

    case.check(
        "serve adopts the stale-build core and takes it over to live",
        wait_for(observe, timeout=45),
        f"states={states} ptyCore={serve_json().get('ptyCore')}",
    )
    case.check("serve reported handing_off on the way", "handing_off" in states, str(states))
    try:
        stale_core.wait(timeout=20)
        stale_exited = stale_core.returncode == 0
    except subprocess.TimeoutExpired:
        stale_exited = False
    case.check("the stale core exited 0 after the supervisor's takeover", stale_exited)
    case.check(
        "the session under the stale core survived with its screen intact",
        home.manifests().get(stale_session, {}).get("state") == "running"
        and screen(home, stale_session) == stale_screen,
        f"state={home.manifests().get(stale_session, {}).get('state')} before={stale_screen[-160:]!r} after={screen(home, stale_session)[-160:]!r}",
    )
    run_cli(home, ["rm", stale_session], timeout=45)
    serve.close()

    # Default policy since 0.5.2 (no UNPEEL_PTY_CORE_TAKEOVER): the supervisor
    # never takes an older-build core over in place. It keeps serving its
    # Sessions, new Sessions run one process each, and once it is empty it is
    # asked to exit so a current-build core starts.
    core_request(home, {"op": "shutdown"})
    time.sleep(1.0)
    drain_core = start_core(home, binary=stale_bin)
    stopper.cores.append(drain_core)
    case.check("a second stale-build core is up", drain_core.poll() is None and core_record(home)["pid"] == drain_core.pid)
    drain_session = new_session(case, home, "session under the draining core")
    run_cli(home, ["send", drain_session, "drain-core-text", "--enter"])
    case.check("the draining core's session echoes", wait_for(lambda: screen(home, drain_session).count("drain-core-text") >= 2))

    serve = case.serve(env={"UNPEEL_PTY_CORE": "1"})
    serve.ready()
    case.check(
        "serve adopts the stale-build core without taking it over",
        wait_for(lambda: serve_json().get("ptyCore", {}).get("state") == "adopted", timeout=30),
        str(serve_json().get("ptyCore")),
    )
    time.sleep(3.0)
    case.check(
        "no takeover is started while the older core holds Sessions",
        serve_json().get("ptyCore", {}).get("state") == "adopted" and core_record(home)["pid"] == drain_core.pid,
        str(serve_json().get("ptyCore")),
    )
    fresh_session = new_session(case, home, "session spawned while the older core drains")
    run_cli(home, ["send", fresh_session, "one-process-text", "--enter"])
    case.check(
        "a new Session spawned meanwhile runs one process each, not in the older core",
        wait_for(lambda: screen(home, fresh_session).count("one-process-text") >= 2)
        and home.manifests().get(fresh_session, {}).get("host_pid") != drain_core.pid,
        f"host_pid={home.manifests().get(fresh_session, {}).get('host_pid')} core={drain_core.pid}",
    )
    time.sleep(6.0)
    case.check(
        "that Session is still running six seconds later and its exit is not pending",
        home.manifests().get(fresh_session, {}).get("state") == "running",
        str(home.manifests().get(fresh_session, {}).get("state")),
    )
    case.check("the older core's own session still echoes", wait_for(lambda: screen(home, drain_session).count("drain-core-text") >= 2))

    run_cli(home, ["rm", drain_session], timeout=45)
    case.check(
        "once empty, the older core exits and a current-build core takes its place",
        wait_for(lambda: serve_json().get("ptyCore", {}).get("state") == "live" and core_record(home)["pid"] != drain_core.pid, timeout=45),
        f"ptyCore={serve_json().get('ptyCore')} record={core_record(home)}",
    )
    try:
        drain_core.wait(timeout=20)
        drained_exit = drain_core.returncode
    except subprocess.TimeoutExpired:
        drained_exit = None
    case.check("the drained core exited 0", drained_exit == 0, str(drained_exit))
    after_session = new_session(case, home, "session in the current-build core")
    run_cli(home, ["send", after_session, "fresh-core-text", "--enter"])
    case.check(
        "a Session created afterwards lands in the current-build core and runs",
        wait_for(lambda: screen(home, after_session).count("fresh-core-text") >= 2)
        and home.manifests().get(after_session, {}).get("host_pid") == core_record(home)["pid"],
        f"host_pid={home.manifests().get(after_session, {}).get('host_pid')} core={core_record(home)['pid']}",
    )
    for session_id in (fresh_session, after_session):
        run_cli(home, ["rm", session_id], timeout=45)
    serve.close()


run("pty_core_handoff", body)
