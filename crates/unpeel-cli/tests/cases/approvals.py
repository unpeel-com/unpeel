"""MCP write approvals owned by the UI-free Host service.

``compat_approvals.py`` imports the historical terminal-keyboard scenario;
this canonical case proves that serve publishes and resolves the same queue
through the shared Controller contract with no TUI process.
"""

import sys, os, threading, time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, mcp_post, mobile_request, tui_hook_port  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.session("s1", label="a session", project_id="p")
    token = home.pair_device()
    phone_port = home.reserve_mobile_port()
    state = home.state()
    state["mcp_write_approvals"] = {}
    home.write_state(state)

    service = case.serve()
    ready = service.ready(timeout=15.0)
    port = ready.get("hookPort") if ready else None
    case.check(
        "serve owns the approval bridge without a TUI",
        bool(ready)
        and ready.get("pid") == service.pid
        and ready.get("directPort") == phone_port
        and isinstance(port, int),
        str(ready or service.log()),
    )
    if not port:
        return

    status, _ = mcp_post(
        port,
        "/mcp/approve-write",
        {"caller_session_id": "a", "target_session_id": "b"},
    )
    case.check("an unauthenticated call is rejected", status == 401, str(status))

    ready_status, _ = mobile_request(phone_port, "/mobile/bootstrap", token)
    case.check("the Controller endpoint is up", ready_status == 200, str(ready_status))
    if ready_status != 200:
        return

    results = {}

    def ask(name, caller, target):
        results[name] = mcp_post(
            port,
            "/mcp/approve-write",
            {"caller_session_id": caller, "target_session_id": target},
            token=home.auth_token,
        )

    def wait_pending(caller):
        found = {}

        def poll():
            nonlocal found
            boot_status, boot = mobile_request(
                phone_port, "/mobile/bootstrap", token
            )
            if boot_status != 200:
                return False
            found = next(
                (
                    approval
                    for approval in boot.get("pendingApprovals", [])
                    if approval.get("callerSessionID") == caller
                ),
                {},
            )
            return found

        return service.wait_for(poll, timeout=10.0)

    thread = threading.Thread(target=ask, args=("grant", "caller-1", "target-1"))
    thread.start()
    pending = wait_pending("caller-1")
    case.check(
        "serve publishes the approval to Controllers",
        bool(pending) and pending.get("kind") == "write",
        str(pending),
    )
    if pending:
        answer_status, _ = mobile_request(
            phone_port,
            "/mobile/approvals/answer",
            token,
            method="POST",
            body={"id": pending["id"], "approved": True},
        )
        repeated_status, repeated_body = mobile_request(
            phone_port,
            "/mobile/approvals/answer",
            token,
            method="POST",
            body={"id": pending["id"], "approved": True},
        )
        case.check("the first Controller answer is accepted", answer_status == 200)
        case.check(
            "an already-answered approval conflicts",
            repeated_status == 409
            and repeated_body.get("error") == "approval no longer pending",
            str((repeated_status, repeated_body)),
        )
    thread.join(timeout=15)
    case.check(
        "the Controller answer releases the blocked MCP call",
        results.get("grant", (0, {}))[1] == {"approved": True},
        str(results.get("grant")),
    )
    persisted = home.state().get("mcp_write_approvals", {})
    case.check(
        "the grant is persisted in shared Host state",
        persisted.get("caller-1") == ["target-1"],
        str(persisted),
    )

    ask("fast", "caller-1", "target-1")
    case.check(
        "an approved pair returns immediately",
        results["fast"][1] == {"approved": True},
        str(results["fast"]),
    )

    thread = threading.Thread(target=ask, args=("deny", "caller-2", "target-2"))
    thread.start()
    pending = wait_pending("caller-2")
    if pending:
        deny_status, _ = mobile_request(
            phone_port,
            "/mobile/approvals/answer",
            token,
            method="POST",
            body={"id": pending["id"], "approved": False},
        )
    else:
        deny_status = 0
    thread.join(timeout=15)
    case.check(
        "a Controller can deny without persisting a grant",
        deny_status == 200
        and results.get("deny", (0, {}))[1] == {"approved": False}
        and "caller-2" not in home.state().get("mcp_write_approvals", {}),
        str((deny_status, results.get("deny"))),
    )
    case.check(
        "the Host service remains live after the client interaction",
        service.process.poll() is None and bool(service.status()),
        service.log(),
    )


if __name__ == "__main__":
    run("approvals", body)
