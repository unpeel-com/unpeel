"""Closed local Controller connections release their OS thread resources.

The Host stays up for days. Keeping every completed connection JoinHandle
retains its pthread stack until shutdown on macOS, eventually making new
connections time out even while existing terminals keep working.
"""

import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run  # noqa: E402
from host_launch_conformance import LocalHostClient  # noqa: E402


def stack_regions(pid):
    if sys.platform != "darwin":
        return None
    result = subprocess.run(
        ["vmmap", "-summary", str(pid)], capture_output=True, text=True, timeout=20
    )
    if result.returncode:
        raise RuntimeError(result.stderr)
    for line in result.stdout.splitlines():
        if re.match(r"^Stack\s", line):
            return int(line.split()[-1])
    raise RuntimeError("vmmap did not report stack regions")


def status(client, request_id):
    return client.call(
        request_id, "POST", "/_unpeel/platform-adapter", body={"action": "status"}
    )[0]


def body(case):
    case.home.project("churn-project", "Churn", "/tmp")
    service = case.serve()
    service.ready()
    with open(case.home.path("serve.json")) as handle:
        serving = json.load(handle)
    socket_path = serving["localSocket"]
    persistent = LocalHostClient(socket_path)
    try:
        case.check("persistent Controller answers", status(persistent, 1) == 409)
        baseline = stack_regions(service.pid)
        for _ in range(200):
            client = LocalHostClient(socket_path)
            try:
                if status(client, 1) != 409:
                    raise RuntimeError("short-lived Controller did not answer")
            finally:
                client.close()

        case.check("persistent Controller survives connection churn", status(persistent, 2) == 409)
        if baseline is not None:
            deadline = time.monotonic() + 10
            after = stack_regions(service.pid)
            while after > baseline + 32 and time.monotonic() < deadline:
                time.sleep(0.05)
                after = stack_regions(service.pid)
            case.check(
                "closed Controllers release their thread stacks without a Host restart",
                after <= baseline + 32,
                f"stack regions: {baseline} before, {after} after 200 connections",
            )

        code, created = persistent.call(
            3, "POST", "/mobile/sessions", body={"projectID": "churn-project", "command": "cat"}
        )
        case.check(
            "sessions can still start through the Controller after connection churn",
            code == 200 and bool(created.get("sessionID")),
            str(created),
        )
    finally:
        persistent.close()


if __name__ == "__main__":
    run("host_gateway_churn", body)
