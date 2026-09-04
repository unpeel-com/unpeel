"""The body of scripts/verify-computer.sh: Computer Use with the REAL engine.

Run by the wrapper only (it sets up the display, the session bus, the engine,
`UNPEEL_TUI_BINARY`, and `UNPEEL_TUI_TEST_HOME`). Reuses the CLI matrix
harness so the Host, the pairing, the approval route, and the MCP client are
the same ones the `computer` matrix case drives with the stub — the only
difference here is that cua-driver is real and the window is a GTK fixture
(`zenity --entry`) whose stdout proves the typed text and the click landed.
"""

import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "crates",
        "unpeel-cli",
        "tests",
    ),
)
from harness import McpClient, mobile_request, run, run_cli, wait_running  # noqa: E402

TYPED = "unpeel-ok"


def experimental(phone_port, token):
    status, boot = mobile_request(phone_port, "/mobile/bootstrap", token)
    if status != 200:
        return {}
    return boot.get("workspaceSettings", {}).get("experimentalSettings", {}) or {}


def payload(reply):
    """The engine JSON inside a computer tool reply (after any artifact line)."""
    text = McpClient.text(reply)
    start = text.find("{")
    if start < 0:
        return {}
    try:
        return json.loads(text[start:])
    except ValueError:
        return {}


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    token = home.pair_device()
    phone_port = home.reserve_mobile_port()
    engine = os.environ.get("UNPEEL_CUA_DRIVER_BIN", "")
    case.check("an engine is selected", bool(engine) and os.access(engine, os.X_OK), engine)

    on = run_cli(home, ["settings", "set", "computer_use", "true"], timeout=20)
    ask = run_cli(home, ["settings", "set", "computer_access", "ask"], timeout=20)
    case.check("settings accept computer_use/computer_access", on.returncode == 0 and ask.returncode == 0, on.stderr + ask.stderr)

    service = case.serve()
    ready = service.ready(timeout=20.0)
    case.check("serve comes up in the desktop session", bool(ready), str(ready or service.log()))
    if not ready:
        return

    # 1. readiness — the worker installs nothing here; it supervises the
    #    selected engine and reports readiness once `status` succeeds.
    settings = service.wait_for(
        lambda: (lambda s: s if s.get("computerUseReady") is True else None)(experimental(phone_port, token)),
        timeout=30.0,
    ) or {}
    case.check("the Host reports the real engine ready", settings.get("computerUseReady") is True, str(settings or experimental(phone_port, token)))
    if settings.get("computerUseReady") is not True:
        case.note(service.log()[-1500:])
        return

    # The GTK fixture: an entry dialog whose stdout is the entered text once
    # OK is activated. Started here, on the same display the engine sees.
    fixture = subprocess.Popen(
        ["zenity", "--entry", "--title", "Unpeel fixture", "--text", "Name"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    case.track(type("Fixture", (), {"close": staticmethod(lambda: fixture.kill() if fixture.poll() is None else None)})())
    time.sleep(1.5)

    # 2. gate — a Session launched after readiness carries the tool.
    started = run_cli(home, ["new", "--command", "", "--project", "p"], timeout=40)
    session_id = started.stdout.strip().split()[-1] if started.stdout.strip() else ""
    case.check("headless new publishes a running Host", started.returncode == 0 and len(session_id) == 36 and wait_running(home, session_id), started.stderr[:200])
    if not session_id:
        return
    case.check("the Session carries the computer grant", home.manifests()[session_id].get("computer_mcp_enabled") is True)
    mcp = McpClient(home, session_id)
    case.track(mcp)
    case.check("the computer tool is advertised", "computer" in mcp.tool_names())

    # 3. approval — the first call under Ask blocks until a Controller answers.
    result = {}

    def windows():
        result["reply"] = mcp.call("computer", {"action": "windows", "pid": fixture.pid})

    worker = threading.Thread(target=windows)
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
    case.check("the Ask gate publishes a computer approval", bool(pending) and worker.is_alive(), str(pending))
    if pending:
        status, _ = mobile_request(phone_port, "/mobile/approvals/answer", token, method="POST", body={"id": pending["id"], "approved": True})
        case.check("the approval route accepts the answer", status == 200, str(status))
    worker.join(timeout=30)
    listed = payload(result.get("reply", {}))
    window = next((w for w in listed.get("windows", []) if w.get("title") == "Unpeel fixture"), None)
    case.check("windows lists the GTK fixture after approval", bool(window), McpClient.text(result.get("reply", {}))[:300])
    case.check("computer_approvals persists the Session", session_id in (home.state().get("computer_approvals") or []))
    if not window:
        return
    window_id = window["window_id"]

    # 4. see — a real AT-SPI tree plus a screenshot in the gallery.
    seen = mcp.call("computer", {"action": "see", "pid": fixture.pid, "window_id": window_id})
    tree = payload(seen)
    elements = tree.get("elements", [])
    # AT-SPI role names differ by toolkit generation: GTK4 zenity (Ubuntu
    # 24.04+) reports "text box" / "button", GTK3 zenity 3.42 (Ubuntu 22.04,
    # the CI runner) reports "text" with no label / "push button". Match by
    # substring, and the entry by role alone.
    entry = next((e for e in elements if "text" in str(e.get("role", "")) and e.get("element_token")), None)
    ok_button = next(
        (e for e in elements if "button" in str(e.get("role", "")) and e.get("label") == "OK" and e.get("element_token")),
        None,
    )
    case.check("see returns a non-empty accessibility tree with the entry and the OK button", bool(entry) and bool(ok_button), McpClient.text(seen)[:400])
    case.check("see reports a non-degraded capture", tree.get("degraded") is not True, str(tree.get("degraded")))
    status, artifacts = mobile_request(phone_port, f"/mobile/artifacts?session_id={session_id}", token)
    names = [str(item.get("name", "")) for item in (artifacts.get("artifacts", []) if status == 200 else [])]
    case.check("the gallery lists the see screenshot", any(n.startswith("see-") and n.endswith(".png") for n in names), str(names))
    if not (entry and ok_button):
        return

    # 5. act — set the entry, click OK by token, and the fixture prints it.
    set_reply = mcp.call("computer", {"action": "set_value", "pid": fixture.pid, "window_id": window_id, "element_token": entry["element_token"], "value": TYPED})
    case.check("set_value is accepted", payload(set_reply).get("status") != "refused" and "error" not in set_reply, McpClient.text(set_reply)[:300])
    click_reply = mcp.call("computer", {"action": "click", "pid": fixture.pid, "window_id": window_id, "element_token": ok_button["element_token"]})
    case.check("click OK is accepted", payload(click_reply).get("status") != "refused" and "error" not in click_reply, McpClient.text(click_reply)[:300])
    try:
        out, _ = fixture.communicate(timeout=15)
        exited = True
    except subprocess.TimeoutExpired:
        out, exited = "", False
    case.check("the fixture closed and printed exactly the typed text", exited and out.strip() == TYPED, f"exited={exited} out={out!r}")

    # 6. cleanup — Remove ends the driver session and prunes the approval.
    mcp.close()
    removed = run_cli(home, ["rm", session_id], timeout=30)
    case.check("headless rm removes the Session", removed.returncode == 0, removed.stderr[:200])
    case.check("Remove prunes computer_approvals", session_id not in (home.state().get("computer_approvals") or []))
    service.close()


run("verify-computer", body)
