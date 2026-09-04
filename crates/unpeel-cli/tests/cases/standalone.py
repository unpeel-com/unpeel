"""No desktop app or TUI: ``unpeel serve`` is a complete persistent Host.

``compat_standalone.py`` keeps the released TUI-as-server behavior covered
until the client-only migration ships.
"""

import sys, os

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import mobile_request, run  # noqa: E402


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.preset(label="cat", command="cat", preset_id="cat")
    token = home.pair_device()
    phone_port = home.reserve_mobile_port()

    case.check("no app is running", home.ports() == [])
    service = case.serve()
    ready = service.ready(timeout=15.0)
    case.check(
        "serve starts as the only workspace Host engine",
        bool(ready)
        and service.process.args[-1] == "serve"
        and ready.get("pid") == service.pid
        and ready.get("directPort") == phone_port,
        str(ready or service.log()),
    )
    if not ready:
        return

    status, boot = mobile_request(phone_port, "/mobile/bootstrap", token)
    case.check(
        "the app-less Host publishes the shared Controller contract",
        status == 200 and boot.get("hostProtocol", {}).get("majorVersion") == 1,
        str((status, boot)),
    )

    def create(request_id):
        return mobile_request(
            phone_port,
            "/mobile/sessions",
            token,
            method="POST",
            body={"projectID": "p", "presetID": "cat"},
            timeout=15,
            headers={"X-Unpeel-Request-ID": request_id},
        )

    first_status, first = create("standalone-create-1")
    first_id = first.get("sessionID")
    first_live = service.wait_for(
        lambda: (
            home.manifests().get(first_id, {})
            if home.manifests().get(first_id, {}).get("state") == "running"
            and os.path.exists(home.path("app-sessions", first_id or "", "session.sock"))
            else None
        ),
        timeout=20,
    )
    case.check(
        "create launches a real per-Session Host app-lessly",
        first_status == 200
        and bool(first_id)
        and bool(first_live)
        and first_live.get("session", {}).get("command") == "cat",
        str((first_status, first, first_live)),
    )

    second_status, second = create("standalone-create-2")
    second_id = second.get("sessionID")
    two_live = service.wait_for(
        lambda: len(home.running_sessions()) == 2 and home.running_sessions(),
        timeout=20,
    )
    case.check(
        "one Host service owns multiple independent Sessions",
        second_status == 200
        and bool(second_id)
        and second_id != first_id
        and bool(two_live),
        str((second_status, second, home.manifests())),
    )

    stop_status, _ = mobile_request(
        phone_port,
        "/mobile/session-action",
        token,
        method="POST",
        body={"sessionID": first_id, "action": "stop"},
        timeout=15,
        headers={"X-Unpeel-Request-ID": "standalone-stop-1"},
    )
    stopped = service.wait_for(
        lambda: home.manifests().get(first_id, {}).get("state") == "exited",
        timeout=15,
    )
    remove_status, _ = mobile_request(
        phone_port,
        "/mobile/session-action",
        token,
        method="POST",
        body={"sessionID": first_id, "action": "remove"},
        timeout=15,
        headers={"X-Unpeel-Request-ID": "standalone-remove-1"},
    )
    case.check(
        "stop and remove work through the Host contract",
        stop_status == 200
        and bool(stopped)
        and remove_status == 200
        and first_id not in home.manifests()
        and set(home.running_sessions()) == {second_id},
        str((stop_status, remove_status, home.manifests())),
    )

    case.check(
        "serve is the only hook-bus owner",
        home.ports() == [ready["hookPort"]],
        str(home.ports()),
    )

    surviving_pid = home.manifests().get(second_id, {}).get("pid")
    service.close()
    still_alive = False
    if surviving_pid:
        try:
            os.kill(surviving_pid, 0)
            still_alive = True
        except (ProcessLookupError, PermissionError):
            pass
    case.check(
        "stopping serve leaves Session hosts alive",
        service.exited()
        and not os.path.exists(home.path("serve.json"))
        and still_alive,
        service.log(),
    )

    restarted = case.serve()
    restarted_ready = restarted.ready(timeout=15.0)
    restart_status, restart_boot = mobile_request(
        phone_port, "/mobile/bootstrap", token
    )
    sessions = {item.get("id"): item for item in restart_boot.get("sessions", [])}
    case.check(
        "a restarted service rediscovers the surviving Session",
        bool(restarted_ready)
        and restart_status == 200
        and sessions.get(second_id, {}).get("status") == "running",
        str((restarted_ready, restart_status, sessions.get(second_id))),
    )


if __name__ == "__main__":
    run("standalone", body)
