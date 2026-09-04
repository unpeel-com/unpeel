"""The shared PTY core: `unpeel-host __pty_core__` hosts every Session of a
home in one process. Launches route to it through the ordinary
`unpeel-host <launch-file>` spawn, the on-disk/socket contract is unchanged,
`shutdown` is refused while Sessions are hosted, and the per-process Host
remains the fallback when the core is absent or disabled."""

import json
import os
import socket
import subprocess
import sys
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


def core_sessions(home):
    return core_request(home, {"op": "ping"}).get("sessions")


def wait_core_sessions(home, expected, timeout=15.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if core_sessions(home) == expected:
            return True
        time.sleep(0.2)
    return False


def per_process_hosts(home):
    out = subprocess.run(["ps", "-eo", "pid,command"], capture_output=True, text=True).stdout
    return [line for line in out.splitlines() if "__session_host__" in line and home.root + "/" in line]


def start_core(home):
    env = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1")
    env.pop("UNPEEL_PTY_CORE", None)
    core = subprocess.Popen(
        [HOST_BIN, "__pty_core__"],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    end = time.monotonic() + 10
    while time.monotonic() < end and not os.path.exists(home.path("pty-core.sock")):
        time.sleep(0.05)
    return core


class CoreStopper:
    """Kills a core this case started if it is somehow still alive at the end."""

    def __init__(self, core):
        self.core = core

    def close(self):
        if self.core.poll() is None:
            self.core.kill()


def launch_env(home, extra=None):
    """The case drives its own hand-started core, so the gate the matrix may
    export (UNPEEL_PTY_CORE=0 for a core-off run) must not leak into these
    launches; only an explicit per-call value applies."""
    env = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1")
    env.pop("UNPEEL_PTY_CORE", None)
    env.update(extra or {})
    return env


def new_session(case, home, label, env=None):
    extra = env or {}
    started = subprocess.run(
        [BINARY, "new", "--preset", "cat", "--project", "p"],
        capture_output=True,
        text=True,
        timeout=45,
        env=launch_env(home, extra),
        cwd=CRATES,
    )
    session_id = ""
    for token in started.stdout.split():
        if len(token) == 36 and token.count("-") == 4:
            session_id = token
    case.check(f"new starts {label}", started.returncode == 0 and bool(session_id),
               started.stdout[:160] + started.stderr[:200])
    case.check(f"{label} host comes up", wait_running(home, session_id), "never running")
    return session_id


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.preset(label="cat", command="cat")

    core = start_core(home)
    case.track(CoreStopper(core))
    record = json.load(open(home.path("pty-core.json")))
    case.check(
        "the core publishes its record after bind",
        record["pid"] == core.pid
        and record["protocol"] == 1
        and record["socket"] == home.path("pty-core.sock")
        and "pid_started_at" in record,
        str(record),
    )
    case.check(
        "the core socket is private",
        (os.stat(home.path("pty-core.sock")).st_mode & 0o777) == 0o600,
    )
    ping = core_request(home, {"op": "ping"})
    case.check("ping answers with the core pid and zero sessions",
               ping.get("ok") and ping.get("pid") == core.pid and ping.get("sessions") == 0, str(ping))

    # A second core for the same home exits 0 at once and leaves the first alone.
    second = subprocess.run(
        [HOST_BIN, "__pty_core__"],
        env=launch_env(home),
        capture_output=True, text=True, timeout=15,
    )
    case.check(
        "a second core for the same home exits 0 immediately",
        second.returncode == 0 and json.load(open(home.path("pty-core.json")))["pid"] == core.pid,
        second.stderr[:200],
    )

    # Ordinary launches route through the core.
    first = new_session(case, home, "first")
    manifest = home.manifests()[first]
    case.check(
        "the manifest records the child pid, never the core pid",
        manifest["pid"] not in (None, core.pid) and manifest.get("host_pid") == core.pid,
        str({k: manifest.get(k) for k in ("pid", "host_pid", "state")}),
    )
    case.check("no per-process host was spawned", per_process_hosts(home) == [], str(per_process_hosts(home)))
    case.check("the core counts the hosted session", wait_core_sessions(home, 1), str(core_sessions(home)))
    case.check("the launch file was consumed", not os.path.exists(home.path("app-sessions", first, "launch.json")))

    short = first[:8]
    case.check("send works through the shared socket contract",
               run_cli(home, ["send", short, "hello-from-core", "--enter"]).returncode == 0)
    time.sleep(1.5)
    screen = run_cli(home, ["screen", short])
    case.check("screen shows the echoed text", "hello-from-core" in screen.stdout, screen.stdout[:200])
    case.check("logs tails output.bin", "hello-from-core" in run_cli(home, ["logs", short]).stdout)
    waited = run_cli(home, ["wait", short, "--idle"], timeout=40)
    case.check("wait --idle settles", waited.returncode == 0, f"rc={waited.returncode}")

    second_id = new_session(case, home, "second")
    case.check("two sessions share one core", wait_core_sessions(home, 2), str(core_sessions(home)))

    # The TUI sees core-hosted Sessions exactly like per-process ones.
    listed = run_cli(home, ["ls"])
    case.check("ls lists core-hosted sessions", "cat" in listed.stdout, listed.stdout[:200])

    busy = core_request(home, {"op": "shutdown"})
    case.check("shutdown is refused while sessions are hosted",
               busy.get("ok") is False and busy.get("error") == "busy" and busy.get("sessions") == 2, str(busy))
    case.check("the core survived the refused shutdown", core.poll() is None and core_sessions(home) == 2)

    removed = run_cli(home, ["rm", first], timeout=45)
    case.check("rm stops a core-hosted session and deletes its dir",
               removed.returncode == 0 and not os.path.exists(home.path("app-sessions", first)),
               removed.stderr[:200])
    case.check("the core releases the removed session", wait_core_sessions(home, 1), str(core_sessions(home)))

    archived = run_cli(home, ["archive", second_id], timeout=45)
    case.check(
        "archive stops a core-hosted session and keeps its dir",
        archived.returncode == 0
        and home.has_marker(second_id, "archived.json")
        and home.manifests().get(second_id, {}).get("state") == "exited",
        archived.stderr[:200] + str(home.manifests().get(second_id, {}).get("state")),
    )
    case.check("the core is empty again", wait_core_sessions(home, 0), str(core_sessions(home)))

    # Fallback 1: UNPEEL_PTY_CORE=0 forces a per-process Host even with a core up.
    forced = new_session(case, home, "forced per-process", env={"UNPEEL_PTY_CORE": "0"})
    hosts = per_process_hosts(home)
    case.check("UNPEEL_PTY_CORE=0 spawns a per-process host", len(hosts) == 1 and core_sessions(home) == 0, str(hosts))
    case.check(
        "a per-process manifest keeps today's shape",
        home.manifests()[forced]["pid"] not in (None, core.pid)
        and home.manifests()[forced].get("host_pid") not in (None, core.pid),
    )
    run_cli(home, ["rm", forced], timeout=45)

    done = core_request(home, {"op": "shutdown"})
    case.check("shutdown succeeds with zero sessions", done.get("ok") is True, str(done))
    try:
        core.wait(timeout=10)
        exited = True
    except subprocess.TimeoutExpired:
        exited = False
    case.check(
        "the core exits cleanly and removes its socket and record",
        exited and core.returncode == 0
        and not os.path.exists(home.path("pty-core.sock"))
        and not os.path.exists(home.path("pty-core.json")),
        f"rc={core.returncode} sock={os.path.exists(home.path('pty-core.sock'))}",
    )

    # Fallback 2: without a core, launches take the per-process path silently.
    plain = new_session(case, home, "no-core fallback")
    trace_path = home.path("hooks", "trace.log")
    trace = open(trace_path).read() if os.path.exists(trace_path) else ""
    case.check(
        "no core means the per-process host, with no fallback noise in the trace",
        len(per_process_hosts(home)) == 1 and "pty-core launch fallback" not in trace,
        str(per_process_hosts(home)),
    )
    run_cli(home, ["rm", plain], timeout=45)


run("pty_core_parity", body)
