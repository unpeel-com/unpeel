"""Muse interrupts without Stop; its scrubbed plugin hooks must still recover.

Exercise the shared ESC contract against Muse's actual reporter and runtime
adapter, including late hooks, next prompts, and a surviving PTY on restart.
"""

import os
import re
import shlex

from hook_cancellation import body, control, read_json
from harness import run, run_cli


def real_muse(case, binary):
    """Optional installed-binary check, using Muse's credential-free echo mode."""
    home = case.home
    home.project("p", "Muse cancellation", home.root)
    os.makedirs(home.path("bin"))
    launcher = home.path("bin", "muse")
    os.symlink(os.path.realpath(binary), launcher)
    environment = {
        "PATH": home.path("bin") + os.pathsep + os.environ["PATH"],
        "SHELL": "/bin/bash",
        "MUSE_NO_AUTO_UPDATE": "1",
    }
    home.preset(label="muse", preset_id="muse", command=shlex.quote(launcher)
                + " --provider echo --echo-delay-ms 8000 --yolo 'cancellation check'")
    service = case.serve(env=environment)
    ready = service.ready(timeout=20)
    if not case.check("isolated Host starts for real Muse", bool(ready), service.log()):
        return
    launched = run_cli(home, ["new", "--preset", "muse", "--project", "p"], env=environment)
    ids = re.findall(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", launched.stdout)
    if not case.check("real Muse launches with isolated plugins and echo provider",
                      launched.returncode == 0 and bool(ids), launched.stderr):
        return
    session_id = ids[0]
    session_dir = home.path("app-sessions", session_id)

    def activity():
        return read_json(home.path("activity-state.json")).get("sessions", {}).get(session_id, {})

    def busy():
        return activity().get("raw_status") == "busy"

    def cancelled():
        state = activity()
        return state.get("raw_status") == "idle" and state.get("completed") is False

    try:
        if not case.check("real Muse opening hook makes the Session busy",
                          bool(service.wait_for(busy, timeout=20)), str(activity())):
            return
        case.check("ESC reaches real Muse", control(home, session_id, "\x1b").get("ok"))
        case.check("real Muse cancellation settles without completion",
                   bool(service.wait_for(cancelled, timeout=3)), str(activity()))
        service.read_for(1)
        case.check("Muse emits no Stop for this interruption",
                   read_json(os.path.join(session_dir, "last-hook-event.json")).get("hook_event_name") == "UserPromptSubmit")
        case.check("cancelled Muse remains idle after its trailing output", cancelled())
        # Muse groups a burst of text+Enter as paste; model a typed submission
        # with a separate Enter instead of pasting a newline into the composer.
        control(home, session_id, "complete this echo")
        service.read_for(0.3)
        control(home, session_id, "\r")
        if not case.check("a new real Muse prompt resumes hook lifecycle", bool(service.wait_for(busy))):
            case.note("seed: " + str(read_json(os.path.join(session_dir, "last-hook-event.json"))))
            with open(os.path.join(session_dir, "output.bin"), "rb") as output:
                case.note("PTY: " + repr(output.read()[-3000:]))
            return
        case.check("real Muse normal completion still reports success",
                   bool(service.wait_for(lambda: activity().get("completed") is True, timeout=20)), str(activity()))
    finally:
        run_cli(home, ["rm", session_id], env=environment, timeout=45)


def check(case):
    binary = os.environ.get("UNPEEL_MUSE_TEST_BINARY")
    if binary:
        real_muse(case, binary)
    else:
        body(case, runtime="muse")


run("muse_cancellation", check)
