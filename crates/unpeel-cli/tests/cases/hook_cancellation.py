"""Real PTY cancellation, late hooks, worker restart, and missed-POST recovery.

The fake Claude runs from a private HOME and deliberately emits no Stop on
Escape, matching Claude's documented interrupt contract. No provider auth or
real user settings are used.
"""

import json
import os
import re
import shlex
import socket
import struct
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import CRATES, run, run_cli  # noqa: E402


def read_json(path):
    try:
        with open(path) as handle:
            return json.load(handle)
    except (OSError, ValueError):
        return {}


def control(home, session_id, data, write_id=None):
    request = {"type": "write", "data": data}
    if write_id:
        request["write_id"] = write_id
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(5)
        client.connect(home.path("app-sessions", session_id, "session.sock"))
        client.sendall((json.dumps(request) + "\n").encode())
        return json.loads(client.makefile("rb").readline())


def body(case, runtime="claude"):
    home = case.home
    home.project("p", "hooks", home.root)
    environment = {"HOME": home.root, "SHELL": "/bin/bash"}
    binary_dir = home.path("bin")
    os.makedirs(binary_dir, exist_ok=True)
    fake = os.path.join(binary_dir, runtime)
    runtime_package = {"claude": "claude-code", "muse": "muse-code", "gemini": "gemini"}[runtime]
    reporter = os.path.join(os.path.dirname(CRATES), "runtimes", runtime_package, "assets", "hooks", "lifecycle.sh")
    with open(fake, "w") as handle:
        handle.write(f"#!{sys.executable}\n" + f"REPORTER = {reporter!r}\n" + '''
import json, os, subprocess, sys, tty
# Muse's installer invokes its CLI before launching the interactive provider.
# Keep plugin management inside this substitute as well as the private HOME.
if len(sys.argv) > 1 and sys.argv[1] == "plugins":
    print("{}")
    sys.exit(0)
tty.setraw(0)
def hook():
    # Real Muse scrubs Unpeel's environment out of hook subprocesses. Exercise
    # the reporter's parent-environment recovery through the same boundary.
    env = {key: os.environ[key] for key in ("HOME", "PATH")} if "muse-code" in REPORTER else None
    event = "BeforeAgent" if "/gemini/" in REPORTER else "UserPromptSubmit"
    subprocess.run(["bash", REPORTER], input=json.dumps({"hook_event_name": event}), text=True, check=True, env=env)
hook()
print("FAKE_AGENT_READY", flush=True)
while True:
    data = os.read(0, 4096)
    if not data:
        break
    if b"\\r" in data:
        hook()
    if data == b"\\x1b":
        print("INTERRUPTED_WITHOUT_STOP", flush=True)
''')
    os.chmod(fake, 0o755)
    environment["PATH"] = binary_dir + os.pathsep + os.environ["PATH"]
    home.preset(label="hook", command=shlex.quote(fake), preset_id="hook")
    service = case.serve(env=environment)
    ready = service.ready(timeout=20)
    case.check("isolated Host starts", bool(ready), service.log())
    if not ready:
        return
    launched = run_cli(home, ["new", "--preset", "hook", "--project", "p"], env=environment)
    ids = re.findall(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", launched.stdout)
    case.check(f"fake {runtime} launches in a real PTY", launched.returncode == 0 and bool(ids), launched.stderr)
    if not ids:
        return
    session_id = ids[0]
    session_dir = home.path("app-sessions", session_id)

    def activity():
        return read_json(home.path("activity-state.json")).get("sessions", {}).get(session_id, {})

    def status_is(status):
        return activity().get("raw_status") == status

    def hook(event, deliver=True):
        manifest = home.manifests()[session_id]
        env = dict(os.environ, **environment, UNPEEL_HOME=home.root,
                   UNPEEL_SESSION_ID=session_id, UNPEEL_SESSION_DIR=session_dir,
                   UNPEEL_RUNTIME_GENERATION=str(manifest["runtime_launch_generation"]),
                   UNPEEL_APP_PORT=str(ready["hookPort"]) if deliver else "",
                   UNPEEL_APP_PORT_REGISTRY_FILE=home.path("no-ports"),
                   UNPEEL_HOOK_TRACE_FILE=home.path("hooks", "trace.log"))
        payload = {"hook_event_name": event}
        if runtime == "gemini":
            payload = {"hook_event_name": {"UserPromptSubmit": "BeforeAgent", "Stop": "AfterAgent", "PermissionRequest": "Notification"}[event], "notification_type": "ToolPermission"}
        subprocess.run(["bash", reporter], input=json.dumps(payload),
                       text=True, env=env, check=True, timeout=5, capture_output=True)

    try:
        if not case.check("opening hook makes the Session busy", bool(service.wait_for(lambda: status_is("busy"))), str(activity())):
            with open(os.path.join(session_dir, "output.bin"), "rb") as output:
                case.note("PTY startup: " + repr(output.read()[-6000:]))
            return
        for label, data in [("arrow", "\x1b[A"), ("paste", "\x1b[200~literal \x1b\x1b[201~"), ("Alt key", "\x1bb")]:
            control(home, session_id, data)
            service.read_for(0.6)
            case.check(f"{label} is not cancellation", status_is("busy") and not os.path.exists(os.path.join(session_dir, "hook-cancellation.json")), str(activity()))

        case.check("Escape write is accepted", control(home, session_id, "\x1b", "cancel-once").get("ok"))
        case.check("Escape clears busy without a provider Stop", bool(service.wait_for(lambda: status_is("idle") and activity().get("completed") is False)), str(activity()))
        hook("PermissionRequest")
        hook("UserPromptSubmit")
        hook("Stop")
        service.read_for(0.8)
        case.check("late hooks cannot revive or complete the cancelled turn", status_is("idle") and activity().get("completed") is False, str(activity()))

        control(home, session_id, "next prompt\r")
        case.check("next submitted prompt resumes lifecycle", bool(service.wait_for(lambda: status_is("busy"))), str(activity()))
        control(home, session_id, "\x1b", "cancel-once")
        service.read_for(0.6)
        case.check("retrying the same write ID does not cancel twice", status_is("busy"), str(activity()))

        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(5)
            client.connect(os.path.join(session_dir, "session.sock"))
            client.sendall(b'{"type":"stream_input"}\n')
            client.recv(1)
            client.sendall(struct.pack(">I", 1) + b"\x1b")
            case.check("attach stream Escape uses the same cancellation path", bool(service.wait_for(lambda: status_is("idle") and activity().get("completed") is False)), str(activity()))

        service.close()
        service = case.serve(env=environment)
        ready = service.ready(timeout=20)
        case.check("worker restarts while the PTY survives", bool(ready) and home.manifests()[session_id].get("state") == "running")
        case.check("cancelled state survives worker restart", bool(service.wait_for(lambda: status_is("idle") and activity().get("completed") is False)), str(activity()))

        control(home, session_id, "another prompt\r")
        case.check("hooks resume after worker restart", bool(service.wait_for(lambda: status_is("busy"))), str(activity()))
        hook("Stop", deliver=False)
        case.check("durable Stop repairs a missed HTTP delivery", bool(service.wait_for(lambda: status_is("idle") and activity().get("completed") is True)), str(activity()))
    finally:
        run_cli(home, ["rm", session_id], env=environment, timeout=45)


if __name__ == "__main__":
    run("hook_cancellation", body)
