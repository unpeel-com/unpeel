"""Ctrl-C into a fresh preset Session lands in the fallback shell.

The Host runs ``$SHELL -l -i -c '<startup script>'``. Under zsh, a foreground
job killed by SIGINT used to make the wrapper abandon the rest of the list
before its fallback ``exec``, so the Session died with the runtime (0.4.3
and main alike; bash never did). The startup script now traps INT with a
no-op handler around the command block. Checked for a per-process Host and
for a Session hosted by the shared PTY core.
"""

import json
import os
import socket
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, run  # noqa: E402

HOST_BIN = os.path.join(os.path.dirname(BINARY), "unpeel-host")
CTRL_C = "\u0003"


def control(home, session_id, request):
    path = home.path("app-sessions", session_id, "session.sock")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(5)
        client.connect(path)
        client.sendall((json.dumps(request) + "\n").encode())
        return client.recv(4096)


def sleeping_child(pid):
    out = subprocess.run(
        ["pgrep", "-P", str(pid), "-x", "sleep"], capture_output=True, text=True
    )
    return out.stdout.split()


def new_sleep_session(home, env):
    started = subprocess.run(
        [BINARY, "new", "--preset", "sleep", "--project", "p"],
        capture_output=True,
        text=True,
        timeout=45,
        env=dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1", **env),
        cwd=CRATES,
    )
    for token in started.stdout.split():
        if len(token) == 36 and token.count("-") == 4:
            return token, started
    return "", started


def wait_for_runtime(home, session_id, timeout=60):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        pid = home.manifests().get(session_id, {}).get("pid")
        if pid and sleeping_child(pid):
            return pid
        time.sleep(0.25)
    return None


def check_phase(case, home, label, env):
    session_id, started = new_sleep_session(home, env)
    case.check(f"{label}: new starts a sleep session", bool(session_id), started.stderr[:200])
    if not session_id:
        return
    shell_pid = wait_for_runtime(home, session_id)
    case.check(f"{label}: the runtime runs under the wrapper shell", bool(shell_pid))
    if not shell_pid:
        return
    reply = control(home, session_id, {"type": "write", "data": CTRL_C})
    case.check(f"{label}: Ctrl-C is accepted", b'"ok":true' in reply, reply[:120])
    end = time.monotonic() + 15
    runtime_gone = False
    while time.monotonic() < end:
        if home.manifests().get(session_id, {}).get("state") != "running":
            break
        if not sleeping_child(shell_pid):
            runtime_gone = True
            break
        time.sleep(0.25)
    time.sleep(2.0)
    manifest = home.manifests().get(session_id, {})
    case.check(
        f"{label}: the runtime is gone but the Session keeps running",
        runtime_gone and manifest.get("state") == "running" and not sleeping_child(shell_pid),
        str({"state": manifest.get("state"), "exit_code": manifest.get("exit_code")}),
    )
    reply = control(home, session_id, {"type": "ping"})
    case.check(f"{label}: the Host still answers", b'"ok":true' in reply, reply[:120])
    control(home, session_id, {"type": "kill"})


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.preset(label="sleep", command="sleep 120", preset_id="sleep")

    check_phase(case, home, "per-process host", {"UNPEEL_PTY_CORE": "0"})

    core_env = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1")
    core_env.pop("UNPEEL_PTY_CORE", None)
    core = subprocess.Popen(
        [HOST_BIN, "__pty_core__"],
        env=core_env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    end = time.monotonic() + 10
    while time.monotonic() < end and not os.path.exists(home.path("pty-core.sock")):
        time.sleep(0.05)
    case.check("a PTY core is up", os.path.exists(home.path("pty-core.sock")))
    try:
        check_phase(case, home, "PTY core", {"UNPEEL_PTY_CORE": "1"})
    finally:
        time.sleep(1.0)
        try:
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(3)
                client.connect(home.path("pty-core.sock"))
                client.sendall(b'{"op":"shutdown"}\n')
                client.recv(256)
        except OSError:
            pass
        try:
            core.wait(timeout=5)
        except subprocess.TimeoutExpired:
            core.kill()


if __name__ == "__main__":
    run("ctrlc_fallback", body)
