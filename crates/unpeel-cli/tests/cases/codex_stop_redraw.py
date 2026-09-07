"""A completed Codex turn stays idle when its terminal redraws after Stop."""

import json
import os
import re
import shlex
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
    fake = home.path("bin", "codex")
    with open(fake, "w") as handle:
        handle.write(f"#!{sys.executable}\n" + '''
import json, os, subprocess, tty
tty.setraw(0)
def hook(event):
    with open(os.path.join(os.environ["HOME"], ".codex/hooks.json")) as handle:
        registrations = json.load(handle)["hooks"][event]
    for entry in registrations:
        for command in entry["hooks"]:
            subprocess.run(command["command"], shell=True, executable="/bin/bash",
                           input=json.dumps({"hook_event_name": event}), text=True, check=True)
print("HOOK_FIXTURE_PROCESS_STARTED", flush=True)
hook("UserPromptSubmit")
redraw = 0
while True:
    data = os.read(0, 4096)
    if not data: break
    if data == b"s": hook("Stop")
    elif data == b"\\x1b": hook("Interrupt")
    elif data == b"r":
        redraw += 1
        print(f"\\r\\nIDLE_REDRAW_{redraw}\\r\\n", flush=True)
    elif b"\\r" in data: hook("UserPromptSubmit")
''')
    os.chmod(fake, 0o755)
    # Codex's managed launcher resolves its real executable through the Host
    # PATH probe before invoking the wrapper. Keep that lookup in this fixture.
    environment["PATH"] = home.path("bin") + os.pathsep + os.environ["PATH"]
    with open(home.path("path-probe-cache.json"), "w") as handle:
        json.dump({"dirs": environment["PATH"].split(os.pathsep),
                   "probed_at_unix_ms": int(time.time() * 1000)}, handle)
    home.preset(label="codex-redraw", command=shlex.quote(fake), preset_id="codex-redraw")
    service = case.serve(env=environment)
    ready = service.ready(timeout=25)
    if not case.check("isolated Host starts", bool(ready), service.log()):
        return
    launched = run_cli(home, ["new", "--preset", "codex-redraw", "--project", "p"], env=environment)
    ids = re.findall(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", launched.stdout)
    if not case.check("Codex fixture launches in a real PTY", launched.returncode == 0 and bool(ids), launched.stderr):
        return
    session_id = ids[0]
    session_dir = home.path("app-sessions", session_id)

    def activity():
        return read_json(home.path("activity-state.json")).get("sessions", {}).get(session_id, {})

    def expect(label, status, completed=False):
        return case.check(label, bool(service.wait_for(lambda: activity().get("raw_status") == status
                          and activity().get("completed") is completed)), str(activity()))

    def redraw(number, completed):
        control(home, session_id, "r")

        def rendered():
            with open(os.path.join(session_dir, "output.bin"), "rb") as handle:
                return f"IDLE_REDRAW_{number}".encode() in handle.read()

        case.check(f"idle redraw {number} reaches the PTY", bool(service.wait_for(rendered)))
        # Observe several worker scans: waiting only for an idle sample could
        # pass before the changed screen is ingested and incorrectly rearms it.
        service.read_for(1.2)
        expect(f"redraw {number} preserves the stopped outcome", "idle", completed)

    try:
        if not fixture_started(case, service, session_dir):
            return
        if not expect("opening hook makes Codex busy", "busy"):
            return
        control(home, session_id, "s")
        expect("Stop completes the turn", "idle", True)
        # Reproduce the real failure inside the former 5–90 second window.
        service.read_for(6)
        redraw(1, True)
        service.close()
        service = case.serve(env=environment)
        ready = service.ready(timeout=25)
        case.check("Host restarts while the same Codex PTY survives", bool(ready)
                   and home.manifests()[session_id].get("state") == "running")
        expect("completed turn recovers from its durable Stop", "idle", True)
        redraw(2, True)
        control(home, session_id, "next prompt\r")
        expect("next opening hook starts real work after restart", "busy")
        control(home, session_id, "\x1b")
        expect("native Interrupt clears busy without completion", "idle")
        redraw(3, False)
        control(home, session_id, "finish next prompt\r")
        expect("next opening hook works after cancellation", "busy")
        control(home, session_id, "s")
        expect("next successful turn completes normally", "idle", True)
    finally:
        run_cli(home, ["rm", session_id], env=environment, timeout=45)


if __name__ == "__main__":
    run("codex_stop_redraw", body)
