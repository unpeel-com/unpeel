"""Retired computer use stays unavailable even with saved grants and a driver override."""
import json
import os
import sys
from unittest.mock import patch

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import McpClient, mobile_request, run, run_cli, wait_running

def body(case):
    home = case.home
    home.write_state({
        "experimental_features": {"computer_use": True},
        "computer_default_access": "allow",
    })
    home.project("p", "unpeel", "/tmp")
    token = home.pair_device()
    port = home.reserve_mobile_port()
    driver = home.path("old-driver")
    marker = home.path("driver-was-run")
    with open(driver, "w") as handle:
        handle.write("#!/bin/sh\ntouch " + marker + "\nexit 1\n")
    os.chmod(driver, 0o700)
    with patch.dict(os.environ, {"UNPEEL_CUA_DRIVER_BIN": driver, "UNPEEL_COMPUTER_ENGINE_INSTALL": "1", "DISPLAY": ":97"}):
        service = case.serve()
        ready = service.ready(timeout=20.0)
        case.check("Host starts with saved computer settings", bool(ready), str(ready))
        if not ready:
            return
        status, boot = mobile_request(port, "/mobile/bootstrap", token)
        settings = boot.get("workspaceSettings", {}).get("experimentalSettings", {})
        case.check("legacy bootstrap reports computer use unavailable", status == 200 and settings.get("computerUseAvailable") is False and settings.get("computerUseReady") is False and settings.get("computerUse") is False, str(settings))
        started = run_cli(home, ["new", "--command", "", "--project", "p"], timeout=40)
        session_id = started.stdout.strip().split()[-1] if started.stdout.strip() else ""
        ok = started.returncode == 0 and len(session_id) == 36 and wait_running(home, session_id)
        case.check("new Session starts normally", ok, started.stdout + started.stderr)
        if not ok:
            return
        case.check("new Session has no computer grant", home.manifests()[session_id].get("computer_mcp_enabled") is not True)
        # Simulate a manifest from an earlier release with every old grant set.
        manifest_path = home.path("app-sessions", session_id, "manifest.json")
        with open(manifest_path) as handle:
            manifest = json.load(handle)
        manifest.update(computer_mcp_enabled=True, computer_client_registered=True)
        with open(manifest_path, "w") as handle:
            json.dump(manifest, handle)
        mcp = McpClient(home, session_id)
        case.track(mcp)
        names = mcp.tool_names()
        case.check("saved grant cannot advertise the removed tool", "computer" not in names and "sessions" in names, str(names))
        reply = mcp.call("computer", {"action": "screenshot"})
        case.check("cached computer calls are rejected", reply.get("result", {}).get("isError") is True or "error" in reply, str(reply))
        result = run_cli(home, ["computer", "install", "--json"], timeout=10)
        case.check("old installer gives a retirement diagnostic", result.returncode != 0 and json.loads(result.stdout).get("state") == "removed", result.stdout)
        case.check("Host and CLI never run an old engine", not os.path.exists(marker))
        case.check("Host never installs an engine", not os.path.exists(home.path("computer")))

run("computer", body)
