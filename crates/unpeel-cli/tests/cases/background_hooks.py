"""Installed Claude subagent hooks preserve busy state independently of its main turn."""

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
    fake = home.path("bin", "claude")
    with open(fake, "w") as handle:
        handle.write(f"#!{sys.executable}\n" + '''
import json, os, subprocess, tty
tty.setraw(0)
def hook():
    with open(os.path.join(os.environ["HOME"], ".claude/settings.json")) as handle:
        registrations = json.load(handle)["hooks"]["UserPromptSubmit"]
    for entry in registrations:
        for command in entry["hooks"]:
            subprocess.run(command["command"], shell=True, executable="/bin/bash",
                           input=json.dumps({"hook_event_name": "UserPromptSubmit"}), text=True, check=True)
print("HOOK_FIXTURE_PROCESS_STARTED", flush=True)
hook()
print("BACKGROUND_HOOK_FIXTURE_READY", flush=True)
while True:
    data = os.read(0, 4096)
    if not data: break
    if b"\\r" in data: hook()
''')
    os.chmod(fake, 0o755)
    environment["PATH"] = home.path("bin") + os.pathsep + os.environ["PATH"]
    home.preset(label="background", command=shlex.quote(fake), preset_id="background")
    service = case.serve(env=environment)
    ready = service.ready(timeout=25)
    case.check("isolated Host starts", bool(ready), service.log())
    if not ready:
        return
    launched = run_cli(home, ["new", "--preset", "background", "--project", "p"], env=environment)
    ids = re.findall(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", launched.stdout)
    case.check("Claude fixture launches in a real PTY", launched.returncode == 0 and bool(ids), launched.stderr)
    if not ids:
        return
    session_id = ids[0]
    session_dir = home.path("app-sessions", session_id)
    settings = read_json(home.path(".claude", "settings.json"))
    for event in ["SubagentStart", "SubagentStop"]:
        case.check(f"installer registers {event}", bool(settings.get("hooks", {}).get(event)))

    def activity():
        return read_json(home.path("activity-state.json")).get("sessions", {}).get(session_id, {})

    def expect(label, status, completed=False):
        return case.check(label, bool(service.wait_for(lambda: activity().get("raw_status") == status
                          and activity().get("completed") is completed)), str(activity()))

    def report(event, child=None, deliver=True, generation=None):
        manifest = home.manifests()[session_id]
        payload = {"hook_event_name": event}
        if child is not None:
            payload["agent_id"] = child
        env = dict(os.environ, HOME=home.root, UNPEEL_HOME=home.root,
                   UNPEEL_HOOK_TRACE_FILE=home.path("hooks", "trace.log"),
                   UNPEEL_SESSION_ID=session_id, UNPEEL_SESSION_DIR=session_dir,
                   UNPEEL_RUNTIME_GENERATION=str(generation if generation is not None else manifest["runtime_launch_generation"]),
                   UNPEEL_APP_PORT=str(ready["hookPort"]) if deliver else "",
                   UNPEEL_APP_PORT_REGISTRY_FILE=home.path("no-ports"))
        for entry in settings["hooks"][event]:
            for hook in entry["hooks"]:
                subprocess.run(hook["command"], shell=True, executable="/bin/bash",
                               input=json.dumps(payload), text=True, env=env,
                               check=True, capture_output=True, timeout=5)

    try:
        if not fixture_started(case, service, session_dir):
            return
        if not expect("main opening hook makes the Session busy", "busy"):
            with open(os.path.join(session_dir, "output.bin"), "rb") as output:
                case.note("PTY startup: " + repr(output.read()[-6000:]))
            return
        report("SubagentStart", "first")
        report("SubagentStart", "second")
        report("Stop")
        service.read_for(0.5)
        expect("main Stop leaves children busy without completion", "busy")
        case.check("child markers do not replace the main durable Stop",
                   read_json(os.path.join(session_dir, "last-hook-event.json")).get("hook_event_name") == "Stop")
        report("SubagentStop", "unknown")
        report("SubagentStop", "first")
        report("SubagentStop", "first")
        service.read_for(0.5)
        expect("duplicate and unknown stops cannot finish another child", "busy")
        service.close()
        service = case.serve(env=environment)
        ready = service.ready(timeout=25)
        case.check("Host restarts with the same running PTY", bool(ready) and home.manifests()[session_id].get("state") == "running")
        expect("background activity recovers after Host restart", "busy")
        with open(os.path.join(session_dir, "read.json"), "w") as handle:
            json.dump({"read_at": int(time.time() * 1000)}, handle)
        service.read_for(0.1)
        report("SubagentStop", "second", deliver=False)
        expect("last child finishing settles even when its HTTP delivery is missed", "idle", True)
        case.check("child finish advances unread beyond the main Stop",
                   bool(service.wait_for(lambda: activity().get("unread") is True)), str(activity()))

        control(home, session_id, "next prompt\r")
        expect("next foreground turn starts", "busy")
        report("SubagentStart", "after-escape")
        control(home, session_id, "\x1b")
        case.check("foreground ESC is recorded", bool(service.wait_for(lambda: os.path.exists(os.path.join(session_dir, "hook-cancellation.json")))))
        expect("foreground ESC preserves running background work", "busy")
        report("Stop")
        report("SubagentStop", "after-escape", deliver=False)
        expect("last child Stop after foreground cancellation stays uncompleted", "idle")
        report("SubagentStart", "old-runtime", generation=0)
        service.read_for(0.5)
        expect("stale runtime generation cannot revive background activity", "idle")
        report("SubagentStart", "../escape")
        case.check("invalid child IDs cannot escape the marker directory", not os.path.exists(os.path.join(session_dir, "background-hooks", "escape.json")))
    finally:
        run_cli(home, ["rm", session_id], env=environment, timeout=45)


if __name__ == "__main__":
    run("background_hooks", body)
