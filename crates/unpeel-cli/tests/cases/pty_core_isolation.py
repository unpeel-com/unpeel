"""Per-session isolation in the shared PTY core. One terminal that stops
reading its input (a raw-mode child flooding device queries without
consuming the answers, a raw-mode child that never reads at all) fills only
its own bounded input queue: a sibling's control socket keeps answering
within its normal timeout, its output keeps flowing, and the core's own
ping stays honest. A `write` into the stuck terminal is refused promptly
instead of parking the caller. Write ids are deduplicated at the reactor.
Every ended Session's child is reaped, whether it ended through `kill` or
(macOS) through the reactor observing the child's exit while a grandchild
still holds the slave: no <defunct> child under the core, a real exit code
in the manifest."""

import json
import os
import socket
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import (  # noqa: E402
    CRATES,
    run,
    wait_running,
)

HOST_BIN = os.path.join(CRATES, "target", "debug", "unpeel-host")

# The sibling's control socket must answer well inside the clients' own
# timeouts (the CLI and serve use 2-5 s); the audit measured >700 ms stalls.
SIBLING_PING_BUDGET = 0.7


def request(path, body, timeout=5.0):
    sock = socket.socket(socket.AF_UNIX)
    sock.settimeout(timeout)
    sock.connect(path)
    sock.sendall((json.dumps(body) + "\n").encode())
    chunks = b""
    while not chunks.endswith(b"\n"):
        piece = sock.recv(65536)
        if not piece:
            break
        chunks += piece
    sock.close()
    return json.loads(chunks.decode() or "{}")


def core_request(home, body, timeout=12.0):
    return request(home.path("pty-core.sock"), body, timeout)


def session_request(home, session_id, body, timeout=5.0):
    return request(home.path("app-sessions", session_id, "session.sock"), body, timeout)


def timed_ping(home, session_id, timeout=SIBLING_PING_BUDGET):
    """(ok, seconds) for a ping on a Session's control socket."""
    started = time.monotonic()
    try:
        reply = session_request(home, session_id, {"type": "ping"}, timeout)
        return bool(reply.get("ok")), time.monotonic() - started
    except (OSError, ValueError):
        return False, time.monotonic() - started


def core_record(home):
    with open(home.path("pty-core.json")) as handle:
        return json.load(handle)


def start_core(home):
    env = {k: v for k, v in os.environ.items() if not k.startswith("UNPEEL_")}
    env.update(UNPEEL_HOME=home.root, UNPEEL_TEST="1")
    core = subprocess.Popen(
        [HOST_BIN, "__pty_core__"],
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
    def __init__(self, core):
        self.core = core

    def close(self):
        if self.core.poll() is None:
            self.core.kill()
            self.core.wait(timeout=10)


def launch(case, home, session_id, command):
    """Launch a Session in the core straight from a launch file (no
    preset/CLI in between, so the command is exactly `command`)."""
    launch_file = home.path(f"launch-{session_id}.json")
    with open(launch_file, "w") as handle:
        json.dump(
            {
                "session": {
                    "id": session_id,
                    "project_id": "p",
                    "label": session_id,
                    "command": command,
                },
                "cwd": home.root,
                "mcp_enabled": False,
            },
            handle,
        )
    reply = core_request(home, {"op": "launch", "launch_file": launch_file}, timeout=30)
    case.check(f"the core launches {session_id}", reply.get("ok"), str(reply))
    case.check(f"{session_id} comes up running", wait_running(home, session_id), "never running")
    wait_for(lambda: os.path.exists(home.path("app-sessions", session_id, "session.sock")))
    return session_id


def journal(home, session_id):
    try:
        with open(home.path("app-sessions", session_id, "output.bin"), "rb") as handle:
            return handle.read()
    except FileNotFoundError:
        return b""


def wait_for(predicate, timeout=15.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(0.1)
    return predicate()


def process_state(pid):
    """ps STAT for a pid: '' once it is gone, contains 'Z' while a zombie."""
    return subprocess.run(
        ["ps", "-o", "stat=", "-p", str(pid)],
        capture_output=True, text=True, check=False,
    ).stdout.strip()


def check_reaped(case, home, session_id, child_pid, what):
    manifest = home.manifests().get(session_id, {})
    case.check(
        f"{what}: the manifest is exited with a real exit code",
        manifest.get("state") == "exited" and manifest.get("exit_code") is not None,
        str({k: manifest.get(k) for k in ("state", "exit_code", "pid")}),
    )
    case.check(
        f"{what}: the child {child_pid} is reaped (no zombie, not running)",
        wait_for(lambda: process_state(child_pid) == "", timeout=10),
        f"STAT={process_state(child_pid)!r}",
    )


def body(case):
    home = case.home
    home.project("p", "unpeel", home.root)

    flood_program = home.path("flood.py")
    with open(flood_program, "w") as handle:
        handle.write(
            "import os, sys, time, tty\n"
            "tty.setraw(0)\n"
            "os.write(1, b'READY')\n"
            "while not os.path.exists(sys.argv[1]): time.sleep(0.02)\n"
            # 30k DA1 queries; the Host answers each one and this child
            # never reads a byte of the answers.
            "os.write(1, b'\\x1b[c' * 30000)\n"
            "os.write(1, b'FLOODED')\n"
            "while True: time.sleep(1)\n"
        )
    trigger = home.path("flood-trigger")

    core = start_core(home)
    case.track(CoreStopper(core))
    case.check("the core is up", core.poll() is None and core_record(home)["pid"] == core.pid)

    ticker = launch(case, home, "ticker", "sh -c 'while :; do echo tick; sleep 0.05; done'")
    flood = launch(case, home, "flood", f"python3 {flood_program} {trigger}")
    stuck = launch(case, home, "stuck", "sh -c 'stty raw -echo; exec sleep 300'")
    echo = launch(case, home, "echo", "cat")
    case.check("the flood child is ready", wait_for(lambda: b"READY" in journal(home, flood)))
    time.sleep(1.0)

    # Baseline.
    ok, seconds = timed_ping(home, ticker)
    case.check("a healthy sibling answers ping promptly", ok and seconds < SIBLING_PING_BUDGET, f"{seconds:.3f}s")
    before = len(journal(home, ticker))
    time.sleep(0.5)
    case.check("a healthy sibling produces output", len(journal(home, ticker)) > before)

    # 1. A query flood whose answers are never consumed.
    with open(trigger, "w"):
        pass
    case.check("the flood child emitted its queries", wait_for(lambda: b"FLOODED" in journal(home, flood), timeout=20))
    time.sleep(0.5)
    ok, seconds = timed_ping(home, ticker)
    case.check(
        "during the flood the sibling's control socket still answers within budget",
        ok and seconds < SIBLING_PING_BUDGET,
        f"ok={ok} {seconds:.3f}s",
    )
    before = len(journal(home, ticker))
    time.sleep(1.0)
    case.check("during the flood the sibling's output keeps flowing", len(journal(home, ticker)) > before)
    ok, seconds = timed_ping(home, flood, timeout=2.0)
    case.check("the flooding session's own control socket still answers", ok, f"{seconds:.3f}s")
    case.check("the core's ping stays healthy", core_request(home, {"op": "ping"}).get("sessions") == 4)

    # 2. A raw-mode child that never reads: writes into it are bounded and
    #    refused promptly; nobody else notices.
    started = time.monotonic()
    accepted = refused = 0
    refusal = ""
    chunk = "x" * (64 * 1024)
    for _ in range(40):
        reply = session_request(home, stuck, {"type": "write", "data": chunk}, timeout=15)
        if reply.get("ok"):
            accepted += 1
        else:
            refused += 1
            refusal = reply.get("error") or ""
            break
    elapsed = time.monotonic() - started
    case.check(
        "writes into a terminal that never reads are accepted up to the bound, then refused",
        accepted >= 1 and refused == 1 and "not accepting input" in refusal,
        f"accepted={accepted} refused={refused} error={refusal!r}",
    )
    case.check("the refusal comes promptly, not after a blocked write", elapsed < 10.0, f"{elapsed:.1f}s")
    ok, seconds = timed_ping(home, ticker)
    case.check(
        "a stuck terminal's full queue never touches the sibling's control socket",
        ok and seconds < SIBLING_PING_BUDGET,
        f"ok={ok} {seconds:.3f}s",
    )
    ok, seconds = timed_ping(home, stuck, timeout=2.0)
    case.check("the stuck session's own control socket still answers", ok, f"{seconds:.3f}s")
    before = len(journal(home, ticker))
    time.sleep(0.5)
    case.check("the sibling's output still flows", len(journal(home, ticker)) > before)

    # 3. Write ids are deduplicated at the reactor (echo + cat = 2 copies per delivery).
    dedup = {"type": "write", "data": "dedup-once\n", "write_id": "isolation-dedup-1"}
    first = session_request(home, echo, dedup)
    retry = session_request(home, echo, dedup)
    time.sleep(0.8)
    case.check(
        "a retried write id is applied once",
        first.get("ok") and retry.get("ok") and journal(home, echo).count(b"dedup-once") == 2,
        f"first={first} retry={retry} copies={journal(home, echo).count(b'dedup-once')}",
    )
    plain = session_request(home, echo, {"type": "write", "data": "plain\n"})
    case.check("a write without an id still lands", plain.get("ok") and wait_for(lambda: journal(home, echo).count(b"plain") >= 2))

    # 4. Ending Sessions whose children are alive: every child is reaped
    #    and the manifest carries a real exit code.
    children = {sid: home.manifests()[sid]["pid"] for sid in (flood, stuck, echo)}
    for sid in (flood, stuck, echo):
        reply = session_request(home, sid, {"type": "kill"}, timeout=15)
        case.check(f"kill {sid} is acknowledged", reply.get("ok"), str(reply))
    for sid in (flood, stuck, echo):
        case.check(
            f"{sid} reaches the exited state",
            wait_for(lambda sid=sid: home.manifests().get(sid, {}).get("state") == "exited", timeout=20),
            str(home.manifests().get(sid, {}).get("state")),
        )
        check_reaped(case, home, sid, children[sid], f"kill {sid}")
    case.check("the core releases the ended sessions", wait_for(lambda: core_request(home, {"op": "ping"}).get("sessions") == 1, timeout=20))
    ok, seconds = timed_ping(home, ticker)
    case.check("the survivor still answers after its siblings ended", ok and seconds < SIBLING_PING_BUDGET, f"{seconds:.3f}s")

    # 5. (macOS) The child exits while a grandchild keeps the slave open,
    #    so the PTY never reaches EOF; the reactor's process watch ends the
    #    Session and its teardown reaps the child.
    if sys.platform == "darwin":
        # The launch command runs inside the hosted login shell, which then
        # execs the interactive fallback shell (same pid); the background
        # job it leaves behind ignores HUP and keeps the slave open. `exit 3`
        # typed into that shell ends the child without any PTY EOF.
        bg = launch(case, home, "bg", 'trap "" HUP; sleep 120 & echo grandchild=$!')
        case.check("the background job was started", wait_for(lambda: b"grandchild=" in journal(home, bg), timeout=20))
        time.sleep(2.0)
        bg_child = home.manifests()[bg]["pid"]
        typed = session_request(home, bg, {"type": "write", "data": "exit 3\n"}, timeout=15)
        case.check("exit is typed into the fallback shell", typed.get("ok"), str(typed))
        case.check(
            "a child that exits behind a live grandchild still ends its session",
            wait_for(lambda: home.manifests().get(bg, {}).get("state") == "exited", timeout=15),
            str(home.manifests().get(bg, {})),
        )
        manifest = home.manifests().get(bg, {})
        case.check("that session records the child's real exit code", manifest.get("exit_code") == 3, str(manifest.get("exit_code")))
        check_reaped(case, home, bg, bg_child, "child exit without EOF")
        text = journal(home, bg).decode(errors="replace")
        grandchild = 0
        if "grandchild=" in text:
            digits = ""
            for ch in text.split("grandchild=", 1)[1]:
                if not ch.isdigit():
                    break
                digits += ch
            grandchild = int(digits or 0)
        case.check("the grandchild was still alive (EOF is not what ended the session)", grandchild and process_state(grandchild) not in ("", "Z"), f"pid={grandchild} STAT={process_state(grandchild)!r}")
        if grandchild:
            try:
                os.kill(grandchild, 9)
            except ProcessLookupError:
                pass

    ticker_child = home.manifests()[ticker]["pid"]
    session_request(home, ticker, {"type": "kill"}, timeout=15)
    case.check("the last session exits", wait_for(lambda: home.manifests().get(ticker, {}).get("state") == "exited", timeout=20))
    check_reaped(case, home, ticker, ticker_child, "kill ticker")
    case.check("the core is empty", wait_for(lambda: core_request(home, {"op": "ping"}).get("sessions") == 0, timeout=20))
    case.check("the core shuts down cleanly", core_request(home, {"op": "shutdown"}).get("ok"))
    try:
        core.wait(timeout=15)
        exited = core.returncode == 0
    except subprocess.TimeoutExpired:
        exited = False
    case.check("the core exited 0", exited, str(core.returncode))


run("pty_core_isolation", body)
