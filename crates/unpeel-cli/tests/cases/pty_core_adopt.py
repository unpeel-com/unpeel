"""The workspace worker adopts a running PTY core and never takes it down.

With ``UNPEEL_PTY_CORE=1`` the worker looks for ``pty-core.json``; when the
record names a live process whose socket answers ``ping`` with that pid, the
worker adopts it and publishes ``serve.json.ptyCore.state == "adopted"``.
Killing the worker with SIGKILL and restarting it must leave the core, its
record, and its socket untouched, and the restarted worker adopts it again.
With the gate off (today's default) nothing is published and nothing is
started.

The core here is a fake: a newline-delimited JSON listener per the shared
contract, fronting a real ``sleep`` child so the recorded pid is alive.
"""

import json
import os
import signal
import socket
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run  # noqa: E402


class FakeCore:
    """A tiny ``pty-core.sock`` answering ``ping`` per the contract."""

    def __init__(self, home, sessions=2):
        self.home = home
        self.sessions = sessions
        self.pings = 0
        self.process = subprocess.Popen(
            ["sleep", "300"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.pid = self.process.pid
        self.path = home.path("pty-core.sock")
        try:
            os.unlink(self.path)
        except FileNotFoundError:
            pass
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(self.path)
        os.chmod(self.path, 0o600)
        self.server.listen(16)
        self.server.settimeout(0.3)
        self._stop = False
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()
        with open(home.path("pty-core.json"), "w") as handle:
            json.dump(
                {
                    "pid": self.pid,
                    "socket": self.path,
                    "host_build_id": "fake-core",
                    "protocol": 1,
                },
                handle,
            )

    def _serve(self):
        while not self._stop:
            try:
                conn, _ = self.server.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with conn:
                try:
                    conn.settimeout(2.0)
                    line = b""
                    while not line.endswith(b"\n"):
                        chunk = conn.recv(4096)
                        if not chunk:
                            break
                        line += chunk
                    request = json.loads(line.decode() or "{}")
                    if request.get("op") == "ping":
                        self.pings += 1
                        reply = {
                            "ok": True,
                            "pid": self.pid,
                            "sessions": self.sessions,
                            "host_build_id": "fake-core",
                        }
                    elif request.get("op") == "shutdown":
                        reply = {"ok": False, "error": "busy", "sessions": self.sessions}
                    else:
                        reply = {"ok": False, "error": "unsupported"}
                    conn.sendall((json.dumps(reply) + "\n").encode())
                except (OSError, ValueError):
                    pass

    def alive(self):
        return self.process.poll() is None

    def close(self):
        self._stop = True
        try:
            self.server.close()
        except OSError:
            pass
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)


def ping(path):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(2.0)
        client.connect(path)
        client.sendall(b'{"op":"ping"}\n')
        return json.loads(client.recv(4096).decode().strip() or "{}")


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")

    # Gate off: today's behavior, nothing published.
    # Force the gate off for this phase: the matrix may run with
    # UNPEEL_PTY_CORE=1 exported to exercise the core everywhere else, and
    # the supervisor treats "0" exactly like an absent variable.
    plain = case.serve(env={"UNPEEL_PTY_CORE": "0"})
    plain_ready = plain.ready(timeout=15.0)
    case.check(
        "without the gate the worker publishes no ptyCore",
        bool(plain_ready) and "ptyCore" not in plain_ready,
        str(plain_ready or plain.log()),
    )
    plain.close()
    case.check(
        "without the gate no core record appears",
        not os.path.exists(home.path("pty-core.json")),
    )

    core = case.track(FakeCore(home))
    service = case.serve(env={"UNPEEL_PTY_CORE": "1"})
    ready = service.ready(timeout=15.0)
    adopted = service.wait_for(
        lambda: (
            service.status()
            if service.status().get("ptyCore", {}).get("state") == "adopted"
            else None
        ),
        timeout=15,
    )
    case.check(
        "with the gate on the worker adopts the running core",
        bool(ready)
        and bool(adopted)
        and adopted["ptyCore"].get("pid") == core.pid
        and adopted["ptyCore"].get("sessions") == 2
        and adopted["ptyCore"].get("rapidFailures") == 0,
        str((ready, adopted, service.log())),
    )
    case.check(
        "adoption went through the contract ping",
        core.pings >= 1,
        str(core.pings),
    )

    # The worker dies hard; the core must not notice.
    record_before = open(home.path("pty-core.json")).read()
    os.kill(service.pid, signal.SIGKILL)
    case.check("the worker was killed", service.exited(timeout=10))
    time.sleep(1.0)
    survived = core.alive() and os.path.exists(core.path)
    reply = {}
    try:
        reply = ping(core.path)
    except OSError as error:
        reply = {"error": str(error)}
    case.check(
        "SIGKILL of the worker leaves the core, its socket, and its record untouched",
        survived
        and reply.get("ok") is True
        and reply.get("pid") == core.pid
        and open(home.path("pty-core.json")).read() == record_before,
        str((survived, reply)),
    )

    restarted = case.serve(env={"UNPEEL_PTY_CORE": "1"})
    restarted_ready = restarted.ready(timeout=15.0)
    readopted = restarted.wait_for(
        lambda: (
            restarted.status()
            if restarted.status().get("ptyCore", {}).get("state") == "adopted"
            else None
        ),
        timeout=15,
    )
    case.check(
        "a restarted worker adopts the same core again",
        bool(restarted_ready)
        and bool(readopted)
        and readopted["ptyCore"].get("pid") == core.pid,
        str((restarted_ready, readopted, restarted.log())),
    )

    # A clean stop reports that the core was left alone and does not touch it.
    restarted.close()
    case.check(
        "a clean worker stop leaves the core running and says so",
        restarted.exited()
        and core.alive()
        and "left running" in restarted.log(),
        restarted.log(),
    )


if __name__ == "__main__":
    run("pty_core_adopt", body)
