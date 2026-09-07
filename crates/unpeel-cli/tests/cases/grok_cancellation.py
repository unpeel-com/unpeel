"""Grok's native outcomes and Host ESC fallback cover cancellation and rewind.

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
from hook_cancellation import control, fixture_started, read_json  # noqa: E402


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
print("HOOK_FIXTURE_PROCESS_STARTED", flush=True)
hook("UserPromptSubmit")
print("FAKE_GROK_READY", flush=True)
rewind = False
while True:
    data = os.read(0, 4096)
    if not data: break
    if data == b"\\x12":
        rewind = True
        print("REWIND_MODE_READY", flush=True)
        continue
    if data in (b"\\x1b", b"\\x03"):
        if not (rewind and data == b"\\x1b"):
            hook("StopCancelled")
        rewind = False
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

    def report(event, deliver=True, payload=None, matcher=None):
        settings = read_json(home.path(".grok", "hooks", "unpeel.json"))
        registrations = settings["hooks"][event]
        registration = next((entry for entry in registrations if matcher in entry.get("matcher", "")), None) if matcher else registrations[0]
        command = registration["hooks"][0]["command"]
        env = dict(os.environ, HOME=home.root, UNPEEL_HOME=home.root,
                   UNPEEL_HOOK_TRACE_FILE=home.path("hooks", "trace.log"),
                   UNPEEL_SESSION_ID=session_id, UNPEEL_SESSION_DIR=session_dir,
                   UNPEEL_RUNTIME_GENERATION=str(home.manifests()[session_id]["runtime_launch_generation"]),
                   UNPEEL_APP_PORT=str(ready["hookPort"]) if deliver else "",
                   UNPEEL_APP_PORT_REGISTRY_FILE=home.path("no-ports"))
        subprocess.run(command, shell=True, executable="/bin/bash", input=json.dumps(payload or {}), text=True,
                       env=env, capture_output=True, check=True, timeout=5)

    try:
        if not fixture_started(case, service, session_dir):
            return
        if not case.check("opening hook makes Grok busy", bool(service.wait_for(busy)), str(activity())):
            with open(os.path.join(session_dir, "output.bin"), "rb") as output:
                case.note("PTY startup: " + repr(output.read()[-6000:]))
            return
        control(home, session_id, "\x1b[A")
        service.read_for(0.4)
        case.check("arrow navigation leaves the turn busy", busy())
        control(home, session_id, "\x1b")
        case.check("ESC native cancellation clears the spinner without completion", bool(service.wait_for(idle_cancelled)), str(activity()))
        case.check("ESC fallback is persisted for rewinds that omit native hooks",
                   bool(service.wait_for(lambda: os.path.exists(os.path.join(session_dir, "hook-cancellation.json")))))
        report("Notification", matcher="approval_required")
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
        report("SessionStart", deliver=False)
        case.check("SessionStart metadata preserves the cancellation seed",
                   read_json(os.path.join(session_dir, "last-hook-event.json")).get("hook_event_name") == "StopCancelled")
        report("UserPromptSubmit", payload={"subagentType": "explore"})
        service.read_for(0.3)
        case.check("a subagent opening hook cannot revive the cancelled main turn", idle_cancelled())
        control(home, session_id, "backstop case\r")
        case.check("backstop case starts busy", bool(service.wait_for(busy)))
        report("Stop", payload={"subagentType": "explore"})
        service.read_for(0.3)
        case.check("a subagent Stop cannot finish the main turn", busy())
        report("Notification", matcher="idle_prompt", deliver=False)
        case.check("idle backstop repairs a missing stop without claiming completion",
                   bool(service.wait_for(idle_cancelled)), str(activity()))
        report("SessionStart", deliver=False)
        service.close()
        service = case.serve(env=environment)
        ready = service.ready(timeout=20)
        case.check("idle backstop survives Host restart and later metadata",
                   bool(ready) and bool(service.wait_for(idle_cancelled)), str(activity()))
        control(home, session_id, "complete after backstop\r")
        case.check("new prompt is detected through the changed Host port", bool(service.wait_for(busy)))
        control(home, session_id, "\x04")
        case.check("successful stop after reconnect completes", bool(service.wait_for(lambda: activity().get("completed") is True)))
        seed_path = os.path.join(session_dir, "last-hook-event.json")
        seed_mtime = os.stat(seed_path).st_mtime_ns
        report("Notification", matcher="idle_prompt")
        service.read_for(0.3)
        case.check("idle pings preserve successful completion and its recency",
                   activity().get("completed") is True and os.stat(seed_path).st_mtime_ns == seed_mtime)
        control(home, session_id, "rewind before response\r")
        case.check("early-rewind case starts busy", bool(service.wait_for(busy)))
        control(home, session_id, "\x12")
        def rewind_ready():
            with open(os.path.join(session_dir, "output.bin"), "rb") as output:
                return b"REWIND_MODE_READY" in output.read()
        case.check("fixture will omit both stop and idle hooks", bool(service.wait_for(rewind_ready)))
        control(home, session_id, "\x1b")
        case.check("early ESC clears busy promptly without any Grok stop hook",
                   bool(service.wait_for(idle_cancelled, timeout=2)), str(activity()))
        case.check("the provider really left an unfinished opening seed",
                   read_json(seed_path).get("hook_event_name") == "UserPromptSubmit")
        service.close()
        service = case.serve(env=environment)
        ready = service.ready(timeout=20)
        case.check("early-rewind cancellation survives Host restart",
                   bool(ready) and bool(service.wait_for(idle_cancelled)), str(activity()))
    finally:
        run_cli(home, ["rm", session_id], env=environment, timeout=45)


run("grok_cancellation", body)
