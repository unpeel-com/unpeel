"""Grok's installed native hooks settle ESC/Ctrl+C without a successful Stop.

A fake Grok runs in a real PTY and dispatches the registrations installed by
the runtime adapter. All provider settings and hook assets stay in test HOME.
"""

import json
import os
import re
import shlex
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, run_cli  # noqa: E402
from hook_cancellation import control, read_json  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "hooks", home.root)
    environment = {"SHELL": "/bin/bash"}
    os.makedirs(home.path("bin"), exist_ok=True)
    fake = home.path("bin", "grok")
    with open(fake, "w") as handle:
        handle.write(f"#!{sys.executable}\n" + '''
import json, os, subprocess, tty
tty.setraw(0)
def hook(event):
    with open(os.path.join(os.environ["HOME"], ".grok/hooks/unpeel.json")) as handle:
        registrations = json.load(handle)["hooks"].get(event, [])
    for entry in registrations:
        for command in entry["hooks"]:
            subprocess.run(command["command"], shell=True, executable="/bin/bash",
                           input=json.dumps({"hook_event_name": event}), text=True, check=True)
hook("UserPromptSubmit")
print("FAKE_GROK_READY", flush=True)
while True:
    data = os.read(0, 4096)
    if not data: break
    if data in (b"\\x1b", b"\\x03"):
        hook("StopCancelled")
        print("CANCELLED", flush=True)
    elif data == b"\\x04": hook("Stop")
    elif data == b"\\x06": hook("StopFailure")
    elif b"\\r" in data: hook("UserPromptSubmit")
''')
    os.chmod(fake, 0o755)
    # The Grok runtime launches through its appearance wrapper, which resolves
    # the provider from the Host's PATH probe. Pin that probe to the fixture.
    environment["PATH"] = home.path("bin") + os.pathsep + os.environ["PATH"]
    with open(home.path("path-probe-cache.json"), "w") as handle:
        json.dump({"dirs": environment["PATH"].split(os.pathsep),
                   "probed_at_unix_ms": int(time.time() * 1000)}, handle)
    home.preset(label="grok-native", command=shlex.quote(fake), preset_id="grok-native")
    service = case.serve(env=environment)
    ready = service.ready(timeout=20)
    case.check("isolated Host starts", bool(ready), service.log())
    if not ready:
        return
    launched = run_cli(home, ["new", "--preset", "grok-native", "--project", "p"], env=environment)
    ids = re.findall(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", launched.stdout)
    case.check("fake Grok launches with the runtime's real hook configuration", launched.returncode == 0 and bool(ids), launched.stderr)
    if launched.returncode != 0 or not ids:
        return
    session_id = ids[0]
    session_dir = home.path("app-sessions", session_id)

    def activity():
        return read_json(home.path("activity-state.json")).get("sessions", {}).get(session_id, {})

    def idle_cancelled():
        state = activity()
        return state.get("raw_status") == "idle" and state.get("completed") is False

    def busy():
        return activity().get("raw_status") == "busy"

    def report(event, deliver=True):
        settings = read_json(home.path(".grok", "hooks", "unpeel.json"))
        command = settings["hooks"][event][0]["hooks"][0]["command"]
        env = dict(os.environ, UNPEEL_SESSION_ID=session_id, UNPEEL_SESSION_DIR=session_dir,
                   UNPEEL_RUNTIME_GENERATION=str(home.manifests()[session_id]["runtime_launch_generation"]),
                   UNPEEL_APP_PORT=str(ready["hookPort"]) if deliver else "",
                   UNPEEL_APP_PORT_REGISTRY_FILE=home.path("no-ports"))
        subprocess.run(command, shell=True, executable="/bin/bash", input="{}", text=True,
                       env=env, capture_output=True, check=True, timeout=5)

    try:
        if not case.check("opening hook makes Grok busy", bool(service.wait_for(busy)), str(activity())):
            with open(os.path.join(session_dir, "output.bin"), "rb") as output:
                case.note("PTY startup: " + repr(output.read()[-6000:]))
            return
        control(home, session_id, "\x1b[A")
        service.read_for(0.4)
        case.check("arrow navigation leaves the turn busy", busy())
        control(home, session_id, "\x1b")
        case.check("ESC native cancellation clears the spinner without completion", bool(service.wait_for(idle_cancelled)), str(activity()))
        case.check("Grok cancellation is hook-owned, without an input-inference marker", not os.path.exists(os.path.join(session_dir, "hook-cancellation.json")))
        report("Notification")
        report("Stop")
        service.read_for(0.4)
        case.check("late attention and Stop cannot revive or complete cancellation", idle_cancelled(), str(activity()))
        control(home, session_id, "next prompt\r")
        case.check("next prompt resumes Grok lifecycle", bool(service.wait_for(busy)))
        control(home, session_id, "\x03")
        case.check("Ctrl+C uses the same native cancellation contract", bool(service.wait_for(idle_cancelled)))
        service.close()
        service = case.serve(env=environment)
        ready = service.ready(timeout=20)
        case.check("cancelled state survives a Host worker restart", bool(ready) and bool(service.wait_for(idle_cancelled)), str(activity()))
        control(home, session_id, "complete this\r")
        case.check("next prompt starts after worker restart", bool(service.wait_for(busy)))
        control(home, session_id, "\x04")
        case.check("normal Stop still counts as successful completion", bool(service.wait_for(lambda: activity().get("completed") is True)))
        control(home, session_id, "failure case\r")
        case.check("failure case starts busy", bool(service.wait_for(busy)))
        control(home, session_id, "\x06")
        case.check("StopFailure clears activity without claiming success", bool(service.wait_for(idle_cancelled)))
        control(home, session_id, "lost post\r")
        case.check("missed-delivery case starts busy", bool(service.wait_for(busy)))
        report("StopCancelled", deliver=False)
        case.check("durable StopCancelled recovers a missed HTTP delivery", bool(service.wait_for(idle_cancelled)), str(activity()))
    finally:
        run_cli(home, ["rm", session_id], env=environment, timeout=45)


run("grok_cancellation", body)
