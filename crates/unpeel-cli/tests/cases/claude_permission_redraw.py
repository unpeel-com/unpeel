"""Claude approval stays attention through redraws, old menu flags, and restart."""

import fcntl
import hashlib
import json
import os
import re
import shlex
import socket
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import CRATES, run, run_cli  # noqa: E402
from hook_cancellation import control, fixture_started, read_json  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "hooks", home.root)
    package = os.path.join(os.path.dirname(CRATES), "runtimes", "claude-code")
    reporter = os.path.join(package, "assets", "hooks", "lifecycle.sh")
    with open(os.path.join(package, "fixtures", "approval-menu.txt")) as handle:
        menu = handle.read()
    os.makedirs(home.path("bin"), exist_ok=True)
    fake = home.path("bin", "claude")
    with open(fake, "w") as handle:
        handle.write(f"#!{sys.executable}\nREPORTER = {reporter!r}\nMENU = {menu!r}\n" + '''
import json, os, subprocess, tty
tty.setraw(0)
def hook(event):
    subprocess.run(["bash", REPORTER], input=json.dumps({"hook_event_name": event, "tool_name": "Bash"}),
                   text=True, check=True)
def screen(text):
    print("\\x1b[2J\\x1b[H" + text.replace("\\n", "\\r\\n"), end="", flush=True)
print("HOOK_FIXTURE_PROCESS_STARTED", flush=True)
hook("UserPromptSubmit")
redraw = 0
while True:
    data = os.read(0, 4096)
    if not data: break
    if data == b"p":
        hook("PermissionRequest")
        screen(MENU)
    elif data == b"r":
        redraw += 1
        screen("Approval redraw " + str(redraw) + "\\n" + MENU)
    elif data == b"a": screen("Running approved command")
    elif data == b"s":
        hook("Stop")
        screen("Completed")
    elif data == b"v": screen(MENU)
    elif data == b"i": screen("Ready for a prompt")
''')
    os.chmod(fake, 0o755)
    environment = {"HOME": home.root, "SHELL": "/bin/bash",
                   "PATH": home.path("bin") + os.pathsep + os.environ["PATH"]}
    home.preset(label="approval", command=shlex.quote(fake), preset_id="approval")
    service = case.serve(env=environment)
    if not case.check("isolated Host starts", bool(service.ready(timeout=25)), service.log()):
        return
    launched = run_cli(home, ["new", "--preset", "approval", "--project", "p"], env=environment)
    ids = re.findall(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", launched.stdout)
    if not case.check("Claude fixture launches", launched.returncode == 0 and bool(ids), launched.stderr):
        return
    sid = ids[0]
    directory = home.path("app-sessions", sid)

    def activity():
        return read_json(home.path("activity-state.json")).get("sessions", {}).get(sid, {})

    def expect(label, status):
        return case.check(label, bool(service.wait_for(lambda: activity().get("raw_status") == status)),
                          str(activity()))

    def menu_flag():
        return home.manifests().get(sid, {}).get("menu_prompt_active", False)

    try:
        if not fixture_started(case, service, directory):
            return
        expect("opening hook starts work", "busy")
        control(home, sid, "p")
        expect("approval hook requests attention", "attention")
        case.check("cancel/amend approval is recognized by PTY scanner", bool(service.wait_for(menu_flag)))
        service.read_for(1)
        control(home, sid, "r")
        service.read_for(1.5)
        expect("menu redraw preserves attention", "attention")

        # Emulate a retained older PTY core that misses this footer. Its
        # false flag must not let a modern worker mistake redraw for an answer.
        manifest_path = os.path.join(directory, "manifest.json")
        lock_path = home.path("session-manifest-locks", hashlib.sha256(sid.encode()).hexdigest() + ".lock")
        with open(lock_path, "a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            manifest = read_json(manifest_path)
            manifest["menu_prompt_active"] = False
            temporary = manifest_path + ".test-tmp"
            with open(temporary, "w") as handle:
                json.dump(manifest, handle)
            os.replace(temporary, manifest_path)
        control(home, sid, "r")
        service.read_for(1.5)
        case.check("older scanner flag remains false", not menu_flag())
        expect("worker viewport check protects a retained old PTY", "attention")
        service.close()
        service = case.serve(env=environment)
        case.check("Host restarts with same live PTY", bool(service.ready(timeout=25))
                   and home.manifests()[sid].get("state") == "running")
        expect("permission restores from hook seed", "attention")
        control(home, sid, "r")
        service.read_for(1.5)
        expect("redraw after Host restart still needs attention", "attention")
        # Temporarily hide only this fixture's socket address while the PTY
        # keeps serving on the renamed socket. A failed viewport check must
        # retry even when the answer produces just one output change.
        socket_path = os.path.join(directory, "session.sock")
        offline_path = os.path.join(directory, "offline.sock")
        os.rename(socket_path, offline_path)
        try:
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(5)
                client.connect(offline_path)
                client.sendall(b'{"type":"write","data":"a"}\n')
                case.check("answer reaches the temporarily hidden PTY", json.loads(client.makefile("rb").readline()).get("ok"))
            service.read_for(1.2)
            expect("unavailable viewport preserves attention", "attention")
        finally:
            os.rename(offline_path, socket_path)
        expect("viewport retry observes the answer without another redraw", "busy")
        control(home, sid, "s")
        expect("Stop completes normally", "idle")
        control(home, sid, "v")
        expect("same menu without a permission hook surfaces attention", "attention")
        control(home, sid, "i")
        expect("dismissed visual menu restores idle", "idle")
    finally:
        run_cli(home, ["rm", sid], env=environment, timeout=45)


if __name__ == "__main__":
    run("claude_permission_redraw", body)
