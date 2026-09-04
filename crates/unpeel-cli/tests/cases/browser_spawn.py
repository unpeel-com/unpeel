"""Headless launches must preserve Browser MCP grants from shared policy.

The CLI and the interactive TUI share ``session_ops::spawn_session``. Driving
the CLI here keeps the regression focused on that app-less launch choke point
while still exercising a real detached Host and its published manifest.
"""

import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, run_cli, wait_running  # noqa: E402


def set_browser_access(home, access):
    state = home.state()
    state["browser_default_access"] = access
    home.write_state(state)


def new_session(case, command=""):
    started = run_cli(
        case.home,
        ["new", "--command", command, "--project", "p"],
        timeout=40,
    )
    session_id = started.stdout.strip().split()[-1] if started.stdout.strip() else ""
    case.check(
        "headless new returns a session id",
        started.returncode == 0 and len(session_id) == 36,
        started.stdout[:160] + started.stderr[:160],
    )
    if session_id:
        case.check(
            "headless new publishes a running Host",
            wait_running(case.home, session_id),
            session_id,
        )
    return session_id


def wait_for_output(home, session_id, needle, timeout=10):
    path = home.path("app-sessions", session_id, "output.bin")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with open(path, "rb") as handle:
                if needle.encode() in handle.read():
                    return True
        except OSError:
            pass
        time.sleep(0.2)
    return False


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")

    # Off suppresses the saved domain grant for a fresh blank terminal.
    set_browser_access(home, "off")
    off_id = new_session(case)
    if off_id:
        case.check(
            "Browser Off suppresses the domain grant",
            home.manifests()[off_id].get("browser_mcp_enabled") is False
            and home.manifests()[off_id].get("browser_client_registered") is False,
            str(home.manifests()[off_id]),
        )

    # A live hookless command has no safe agent-only restart recipe. Stop the
    # terminal first, then Resume through the replacement-Host path that
    # recomputes current app-wide launch policy.
    set_browser_access(home, "on")
    stopped = run_cli(home, ["stop", off_id], timeout=20)
    case.check(
        "the hookless terminal stops before replacement Resume",
        stopped.returncode == 0,
        stopped.stdout[:160] + stopped.stderr[:160],
    )
    resumed = run_cli(home, ["resume", off_id], timeout=45)
    on_id = resumed.stdout.strip().split()[-1] if resumed.stdout.strip() else ""
    case.check(
        "Resume returns a replacement session id",
        resumed.returncode == 0 and len(on_id) == 36 and on_id != off_id,
        resumed.stdout[:160] + resumed.stderr[:160],
    )
    if on_id:
        case.check(
            "replacement Resume recomputes Browser On policy",
            wait_running(home, on_id)
            and home.manifests()[on_id].get("browser_mcp_enabled") is True
            and home.manifests()[on_id].get("browser_client_registered") is False,
            str(home.manifests().get(on_id)),
        )

    # Ask still advertises the tool: approval is enforced on the first action,
    # not by hiding Browser MCP at provider startup. This command also proves
    # app-less launches export caller identity even without a hook-server port.
    set_browser_access(home, "ask")
    ask_id = new_session(
        case,
        "sh -c 'printf \"identity=%s\\n\" \"$UNPEEL_SESSION_ID\"; exec cat'",
    )
    if ask_id:
        ask_manifest = home.manifests()[ask_id]
        case.check(
            "Browser Ask preserves the domain grant",
            ask_manifest.get("browser_mcp_enabled") is True,
            str(ask_manifest),
        )
        case.check(
            "a hookless headless launch exports its session identity",
            wait_for_output(home, ask_id, f"identity={ask_id}"),
            ask_id,
        )


run("browser_spawn", body)
