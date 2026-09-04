"""Computer use on a Linux Host, end to end, with the stub engine.

`unpeel serve` on Linux owns the Computer Use engine child: it resolves
`cua-driver` (here `scripts/ci/fake-cua-driver.sh`), needs a graphical
session (`DISPLAY`), and supervises `cua-driver serve --embedded`. This case
drives that adapter through real hosted sessions and the unified MCP server
and proves the D2 contract from unpeel-apple:docs/plans/computer-use-release.md:

- the Host advertises availability, and readiness only once the daemon is up;
- a Session launched before readiness never sees the `computer` tool, one
  launched after it does (the launch gate is evaluated once, at spawn);
- `see` asks once under Ask, the request is published to Controllers and
  answered through the ordinary approval route, and the screenshot lands in
  the session gallery;
- the answered approval persists in `computer_approvals` and a second call
  no longer blocks;
- Remove runs `unpeel-host __computer_cleanup__`, which asks the engine to end
  the driver session, and prunes the approval.

**Linux only.** On macOS the serve adapter routes computer status through the
native app's platform adapter and never spawns the engine itself, so the stub
driver cannot stand in; the case records a SKIP NOTE there. No X server is
required: the adapter only checks that `DISPLAY` is set, and the stub never
talks to X.
"""

import json
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import (  # noqa: E402
    CRATES,
    McpClient,
    mobile_request,
    run,
    run_cli,
    wait_running,
)

REPO_ROOT = os.path.dirname(CRATES)
FAKE_DRIVER = os.path.join(REPO_ROOT, "scripts", "ci", "fake-cua-driver.sh")


def new_session(case):
    started = run_cli(case.home, ["new", "--command", "", "--project", "p"], timeout=40)
    session_id = started.stdout.strip().split()[-1] if started.stdout.strip() else ""
    ok = started.returncode == 0 and len(session_id) == 36 and wait_running(case.home, session_id)
    case.check("headless new publishes a running Host", ok, started.stdout[:160] + started.stderr[:160])
    return session_id if ok else ""


def experimental(phone_port, token):
    status, boot = mobile_request(phone_port, "/mobile/bootstrap", token)
    if status != 200:
        return {}
    return boot.get("workspaceSettings", {}).get("experimentalSettings", {}) or {}


def body(case):
    if not sys.platform.startswith("linux"):
        case.note(
            "computer SKIPPED: Linux only — on macOS `unpeel serve` routes computer "
            "status through the native app's platform adapter and never spawns the engine"
        )
        case.check("computer skipped on this platform (see NOTE)", True)
        return

    home = case.home
    home.project("p", "unpeel", "/tmp")
    token = home.pair_device()
    phone_port = home.reserve_mobile_port()
    driver_log = home.path("fake-driver.log")

    # Every process in this case — serve, the one-shot CLI (whose launch gate
    # resolves the engine itself), `unpeel-host __mcp__`, and the cleanup
    # helper — must see the same fake desktop and the same stub engine.
    os.environ["DISPLAY"] = os.environ.get("UNPEEL_CU_DISPLAY", ":97")
    os.environ["UNPEEL_CUA_DRIVER_BIN"] = FAKE_DRIVER
    os.environ["FAKE_CUA_DRIVER_LOG"] = driver_log

    service = case.serve()
    ready = service.ready(timeout=15.0)
    case.check("serve comes up", bool(ready) and ready.get("directPort") == phone_port, str(ready or service.log()))
    if not ready:
        return

    # 1. Advertised, not ready: the experiment is off, so the worker never
    #    starts the engine, but a Controller can already see the capability.
    settings = service.wait_for(lambda: experimental(phone_port, token) or None, timeout=10.0) or {}
    case.check(
        "the Host advertises computer use as available but not ready while the experiment is off",
        settings.get("computerUseAvailable") is True and settings.get("computerUseReady") is not True,
        str(settings),
    )

    early = new_session(case)
    if not early:
        return
    case.check(
        "a Session launched before readiness has no computer grant",
        home.manifests()[early].get("computer_mcp_enabled") is not True,
        str(home.manifests()[early].get("computer_mcp_enabled")),
    )
    early_mcp = McpClient(home, early)
    early_tools = early_mcp.tool_names()
    early_mcp.close()
    case.check(
        "the computer tool is absent from that Session's MCP surface",
        "sessions" in early_tools and "computer" not in early_tools,
        str(early_tools),
    )

    # 2. Turn it on with Ask: the worker starts the stub engine and reports
    #    readiness in its bootstrap.
    on = run_cli(home, ["settings", "set", "computer_use", "true"], timeout=20)
    ask = run_cli(home, ["settings", "set", "computer_access", "ask"], timeout=20)
    case.check("settings accept computer_use/computer_access", on.returncode == 0 and ask.returncode == 0, on.stderr + ask.stderr)

    settings = service.wait_for(
        lambda: (lambda s: s if s.get("computerUseReady") is True else None)(experimental(phone_port, token)),
        timeout=20.0,
    ) or {}
    case.check("the Host reports the engine ready once the experiment is on", settings.get("computerUseReady") is True, str(settings or experimental(phone_port, token)))
    case.check("the daemon socket exists under the Host home", os.path.exists(home.path("computer", "daemon.sock")))
    if settings.get("computerUseReady") is not True:
        return

    session_id = new_session(case)
    if not session_id:
        return
    case.check(
        "a Session launched after readiness carries the computer grant",
        home.manifests()[session_id].get("computer_mcp_enabled") is True,
        str(home.manifests()[session_id]),
    )
    mcp = McpClient(home, session_id)
    case.track(mcp)
    tools = mcp.tool_names()
    case.check("the computer tool is advertised to that Session", "computer" in tools, str(tools))

    # 3. `see` under Ask blocks on the first call; the request shows up for
    #    Controllers and is answered through the approval route.
    result = {}

    def see():
        result["reply"] = mcp.call("computer", {"action": "see", "pid": 4242, "window_id": 1})

    worker = threading.Thread(target=see)
    worker.start()

    def pending_computer():
        status, boot = mobile_request(phone_port, "/mobile/bootstrap", token)
        if status != 200:
            return None
        return next(
            (a for a in boot.get("pendingApprovals", []) if a.get("kind") == "computer" and a.get("callerSessionID") == session_id),
            None,
        )

    pending = service.wait_for(pending_computer, timeout=15.0)
    case.check("the Ask gate publishes a computer approval to Controllers", bool(pending), str(pending))
    case.check("the call is still blocked while the approval is pending", worker.is_alive())
    if pending:
        status, _ = mobile_request(
            phone_port, "/mobile/approvals/answer", token, method="POST", body={"id": pending["id"], "approved": True}
        )
        case.check("the approval route accepts the answer", status == 200, str(status))
    worker.join(timeout=30)
    reply = result.get("reply", {})
    text = McpClient.text(reply)
    case.check(
        "see returns the window state after approval",
        not worker.is_alive() and "error" not in reply and "Saved screenshot to" in text and "OK" in text,
        json.dumps(reply)[:400],
    )

    screenshots = home.path("app-sessions", session_id, "artifacts", "computer", "screenshots")
    pngs = sorted(os.listdir(screenshots)) if os.path.isdir(screenshots) else []
    case.check("the screenshot lands under the session's computer artifacts", any(n.startswith("see-") and n.endswith(".png") for n in pngs), str(pngs))
    status, artifacts = mobile_request(phone_port, f"/mobile/artifacts?session_id={session_id}", token)
    listed = artifacts.get("artifacts", []) if status == 200 else []
    case.check(
        "the gallery lists the screenshot",
        any(str(item.get("name", "")).startswith("see-") and str(item.get("name", "")).endswith(".png") for item in listed),
        str(artifacts)[:400],
    )

    # 4. The answered approval persists and a second call no longer asks.
    case.check(
        "computer_approvals persists the approved Session",
        session_id in (home.state().get("computer_approvals") or []),
        str(home.state().get("computer_approvals")),
    )
    started = time.monotonic()
    second = mcp.call("computer", {"action": "see", "pid": 4242, "window_id": 1, "screenshot": False})
    case.check(
        "a second call is granted without a new approval",
        "error" not in second and "OK" in McpClient.text(second) and time.monotonic() - started < 20 and pending_computer() is None,
        json.dumps(second)[:300],
    )

    # 5. Remove ends the driver session through __computer_cleanup__ and
    #    prunes the approval.
    mcp.close()
    removed = run_cli(home, ["rm", session_id], timeout=30)
    case.check("headless rm removes the Session", removed.returncode == 0, removed.stdout[:160] + removed.stderr[:160])

    def cleanup_called():
        try:
            with open(driver_log, encoding="utf-8") as handle:
                return "call end_session" in handle.read() or None
        except OSError:
            return None

    case.check("Remove runs __computer_cleanup__ (the engine saw end_session)", bool(service.wait_for(cleanup_called, timeout=10.0)), str(cleanup_called()))
    case.check(
        "Remove prunes the Session from computer_approvals",
        session_id not in (home.state().get("computer_approvals") or []),
        str(home.state().get("computer_approvals")),
    )

    # Off stops the engine: the worker withdraws readiness.
    run_cli(home, ["settings", "set", "computer_use", "false"], timeout=20)
    off = service.wait_for(
        lambda: (lambda s: s if s and s.get("computerUseReady") is not True else None)(experimental(phone_port, token)),
        timeout=20.0,
    )
    case.check("turning the experiment off withdraws readiness", bool(off), str(off or experimental(phone_port, token)))
    service.close()


run("computer", body)
