"""Shared scaffolding for the CLI + `unpeel serve` end-to-end tests.

Every case runs the real `unpeel` binary inside a real PTY against a private
`UNPEEL_HOME` built from scratch, so cases can neither see nor corrupt each
other's fixtures — the failure mode that made the original throwaway suites
produce phantom passes.

Three things here are load-bearing and easy to get wrong:

* **Forcing a repaint.** ratatui only emits the cells that changed, so an
  assertion on "what's on screen now" would miss anything already drawn.
  `Pty.frame()` toggles the window width, which makes the app redraw
  everything, and returns only the bytes that arrived after the toggle.
* **Waiting.** Nothing is instant: the app polls disk, hooks arrive over
  HTTP, verbs round-trip to a bridge. Cases use `wait_for`/`wait_until`
  rather than a bare sleep, so a slow machine is slow, not red.
* **Cleanup.** A case that leaves a hosted process or an mDNS advertiser
  behind poisons the next one. `Pty.close()` and the runner both sweep.
"""

import fcntl
import glob
import json
import os
import pty
import re
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import termios
import threading
import time
import unicodedata
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer

REPO = os.path.realpath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
CRATES = os.path.join(REPO, "crates")
BINARY = os.environ.get("UNPEEL_TUI_BINARY", os.path.join(CRATES, "target", "debug", "unpeel"))

SPINNER_CHARS = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
UNREAD_DOT = "●"

_ANSI = re.compile(rb"\x1b\[[0-9;?<>=!]*[A-Za-z]|\x1b[\]P][^\x07\x1b]*(\x07|\x1b\\)?|\x1b[()][B0]")


def strip_ansi(raw: bytes) -> str:
    return _ANSI.sub(b"", raw).decode("utf-8", "replace")


class Screen:
    """A tiny VT that reconstructs the visible grid from a repaint stream.

    Needed because the PTY byte stream is not row-structured: ratatui paints
    by jumping the cursor, so "line 3 of the output" is not "row 3 of the
    screen". Column-accurate assertions (is this row in the *sidebar* or just
    echoed in the preview?) require the real grid. Only the sequences ratatui
    actually emits are handled — cursor positioning, erases, and text.
    """

    CSI = re.compile(rb"\x1b\[([0-9;?]*)([A-Za-z])")

    def __init__(self, raw: bytes, rows: int, cols: int):
        self.rows = rows
        self.cols = cols
        self.cells = [[" "] * cols for _ in range(rows)]
        self._row = 0
        self._col = 0
        self._feed(raw)

    def _put(self, char):
        if 0 <= self._row < self.rows and 0 <= self._col < self.cols:
            self.cells[self._row][self._col] = char
        width = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
        if width == 2 and 0 <= self._row < self.rows and self._col + 1 < self.cols:
            self.cells[self._row][self._col + 1] = ""
        self._col += width

    def _feed(self, raw):
        index = 0
        length = len(raw)
        while index < length:
            byte = raw[index : index + 1]
            if byte == b"\x1b":
                match = self.CSI.match(raw, index)
                if match:
                    self._csi(match.group(1).decode(), match.group(2).decode())
                    index = match.end()
                    continue
                # OSC / other escapes: skip to terminator.
                if raw[index : index + 2] in (b"\x1b]", b"\x1bP"):
                    end = raw.find(b"\x07", index)
                    end2 = raw.find(b"\x1b\\", index)
                    candidates = [e for e in (end, end2) if e != -1]
                    index = (min(candidates) + 1) if candidates else index + 2
                    continue
                index += 2
                continue
            if byte == b"\r":
                self._col = 0
                index += 1
                continue
            if byte == b"\n":
                self._row += 1
                index += 1
                continue
            # Decode one UTF-8 character.
            size = 1
            first = raw[index]
            if first >= 0xF0:
                size = 4
            elif first >= 0xE0:
                size = 3
            elif first >= 0xC0:
                size = 2
            try:
                char = raw[index : index + size].decode("utf-8")
            except UnicodeDecodeError:
                char = "?"
            if char.isprintable() or char == " ":
                self._put(char)
            index += size

    def _csi(self, params, final):
        numbers = [int(p) for p in params.split(";") if p.isdigit()]

        def get(position, default=1):
            return numbers[position] if len(numbers) > position else default

        if final == "H" or final == "f":
            self._row = max(0, get(0) - 1)
            self._col = max(0, get(1) - 1)
        elif final == "A":
            self._row = max(0, self._row - get(0))
        elif final == "B":
            self._row = min(self.rows - 1, self._row + get(0))
        elif final == "C":
            self._col = min(self.cols - 1, self._col + get(0))
        elif final == "D":
            self._col = max(0, self._col - get(0))
        elif final == "J":
            mode = get(0, 0)
            if mode == 2:
                self.cells = [[" "] * self.cols for _ in range(self.rows)]
            elif mode == 0:
                for col in range(self._col, self.cols):
                    if self._row < self.rows:
                        self.cells[self._row][col] = " "
                for row in range(self._row + 1, self.rows):
                    self.cells[row] = [" "] * self.cols
        elif final == "K":
            mode = get(0, 0)
            if self._row < self.rows:
                if mode == 0:
                    for col in range(self._col, self.cols):
                        self.cells[self._row][col] = " "
                elif mode == 2:
                    self.cells[self._row] = [" "] * self.cols

    def row(self, index):
        if 0 <= index < self.rows:
            return "".join(self.cells[index])
        return ""

    def lines(self):
        return [self.row(i) for i in range(self.rows)]

    def text(self):
        return squeeze("\n".join(self.lines()))

    def sidebar_width(self):
        """Where the sidebar ends — the preview (or settings detail) box
        still starts with ┌. Skip column 0 so a bordered settings sidebar
        doesn't count as the split."""
        top = self.row(0)
        preview = top.find("┌", 1)
        if preview > 0:
            return preview
        corner = top.find("┐")
        return corner + 1 if corner > 0 else self.cols

    def sidebar(self):
        width = self.sidebar_width()
        return squeeze(" ".join(row[:width] for row in self.lines()))

    def preview(self):
        width = self.sidebar_width()
        return squeeze(" ".join(row[width:] for row in self.lines()))

    def status(self):
        return squeeze(self.row(self.rows - 1))


def squeeze(text: str) -> str:
    """Collapse runs of whitespace — assertions care about words, not the
    column padding that shifts when the width toggles."""
    return re.sub(r"\s+", " ", text)


# ─────────────────────────── fixture home ───────────────────────────


# Leftover-process hygiene: a case must not leave `unpeel-host __session_host__`
# or `__mcp__` sidecars running against its home. A leaked host keeps writing
# to an unlinked output.bin (filling the disk) and re-runs the login-shell
# PATH probe every tick (the load-average blowup). We detect them by open home
# path so an untracked stray is caught, fail the case, then kill them so the
# next case starts clean. The shared `__pty_core__` is NOT a leak here: it is
# legitimate during a case and run.sh sweeps it by its lock file.
def _pids_with_arg(arg):
    try:
        out = subprocess.run(
            ["pgrep", "-f", arg], capture_output=True, text=True, check=False
        ).stdout
    except Exception:
        return []
    pids = []
    for line in out.split():
        try:
            pids.append(int(line))
        except ValueError:
            pass
    return pids


def _process_references_home(pid, home_real):
    # argv mentions the home (covers __session_host__ launch files)…
    try:
        cmd = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="],
            capture_output=True, text=True, check=False,
        ).stdout
        if home_real in cmd:
            return True
    except Exception:
        pass
    # …or its UNPEEL_HOME env points at the home (covers __mcp__ sidecars,
    # which carry no home in argv)…
    try:
        env = subprocess.run(
            ["ps", "eww", "-p", str(pid), "-o", "command="],
            capture_output=True, text=True, check=False,
        ).stdout
        if f"UNPEEL_HOME={home_real}" in env or f"UNPEEL_HOME={home_real}/" in env:
            return True
    except Exception:
        pass
    # …or it holds any file open under the home (covers a core by its lock).
    try:
        lsof = subprocess.run(
            ["lsof", "-p", str(pid)], capture_output=True, text=True, check=False
        ).stdout
        for line in lsof.splitlines():
            field = line.split()[-1] if line.split() else ""
            if field.startswith(home_real + "/") or field == home_real:
                return True
    except Exception:
        pass
    return False


def leftover_host_processes(home_root):
    """(pid, command) for host/sidecar/core processes still bound to this home."""
    home_real = os.path.realpath(home_root)
    seen = set()
    leftovers = []
    # The shared __pty_core__ is a legitimate detached process during a case
    # (core-hosted sessions run inside it) and run.sh sweeps it by its lock
    # file; only stray per-process hosts and mcp sidecars are a leak here.
    for arg in ("__session_host__", "__mcp__"):
        for pid in _pids_with_arg(arg):
            if pid in seen:
                continue
            if _process_references_home(pid, home_real):
                seen.add(pid)
                cmd = subprocess.run(
                    ["ps", "-p", str(pid), "-o", "command="],
                    capture_output=True, text=True, check=False,
                ).stdout.strip()
                leftovers.append((pid, cmd))
    return leftovers


class Home:
    """A private `~/.unpeel` built from scratch."""

    def __init__(self, root):
        self.root = root
        self._hosts = []
        self._socks = []
        os.makedirs(root, exist_ok=True)
        os.makedirs(self.path("app-sessions"), exist_ok=True)
        os.makedirs(self.path("mobile"), exist_ok=True)
        os.makedirs(self.path("mcp"), exist_ok=True)
        self.write_state({})
        self.auth_token = "test-token-123"
        with open(self.path("mcp/auth-token"), "w") as handle:
            handle.write(self.auth_token + "\n")
        os.chmod(self.path("mcp/auth-token"), 0o600)

    def path(self, *parts):
        return os.path.join(self.root, *parts)

    # ── shared app state ──

    def write_state(self, overrides):
        state = {
            "projects": [],
            "active_project_id": None,
            "presets": [],
            "active_tabs": {},
            "pinned_sessions": {},
        }
        state.update(overrides)
        with open(self.path("app-state.json"), "w") as handle:
            json.dump(state, handle, indent=2)
        return state

    def state(self):
        with open(self.path("app-state.json")) as handle:
            return json.load(handle)

    def project(self, project_id="p", name="unpeel", path="/tmp"):
        state = self.state()
        state.setdefault("projects", []).append(
            {"id": project_id, "name": name, "path": path}
        )
        with open(self.path("app-state.json"), "w") as handle:
            json.dump(state, handle, indent=2)
        return project_id

    def preset(self, label="cat", command="cat", quick_launch=False, preset_id=None):
        state = self.state()
        state.setdefault("presets", []).append(
            {
                "id": preset_id or f"preset-{len(state.get('presets', []))}",
                "label": label,
                "command": command,
                "project_id": None,
                "enabled": True,
                "quick_launch": quick_launch,
            }
        )
        with open(self.path("app-state.json"), "w") as handle:
            json.dump(state, handle, indent=2)

    # ── sessions ──

    def session(
        self,
        session_id,
        label="a session",
        command="claude --dangerously-skip-permissions",
        project_id="p",
        state="exited",
        created_at=1_754_300_000_000,
        cwd="/tmp",
        output="hello from the fixture\r\n",
        running=False,
        extra_manifest=None,
        settled=False,
    ):
        """Write a hosted-session dir. `running=True` parks a real child
        process so pid-liveness checks pass, and binds a control socket."""
        directory = self.path("app-sessions", session_id)
        os.makedirs(directory, exist_ok=True)
        pid = None
        if running:
            child = subprocess.Popen(["sleep", "600"])
            self._hosts.append(child)
            pid = child.pid
            state = "running"
            sock = socket.socket(socket.AF_UNIX)
            sock_path = os.path.join(directory, "session.sock")
            if os.path.exists(sock_path):
                os.unlink(sock_path)
            sock.bind(sock_path)
            sock.listen(4)
            self._socks.append(sock)
            # Honest-stop contract (session_ops): stop/archive/restart only
            # succeed once the manifest reaches "exited", so a fake session
            # must answer its control socket like a real host — ack the kill,
            # reap the parked child, and file the exited manifest.
            self._serve_fake_session_socket(
                sock, child, os.path.join(directory, "manifest.json")
            )
        now_ms = int(time.time() * 1000)
        manifest = {
            "session": {
                "id": session_id,
                "project_id": project_id,
                "label": label,
                "command": command,
                "created_at": created_at,
            },
            "cwd": cwd,
            "state": state,
            "pid": pid,
            "exit_code": None if running else 0,
            "has_been_written_to": True,
            "updated_at": now_ms,
            "heartbeat_at": now_ms,
        }
        if extra_manifest:
            manifest.update(extra_manifest)
        with open(os.path.join(directory, "manifest.json"), "w") as handle:
            json.dump(manifest, handle)
        with open(os.path.join(directory, "output.bin"), "w") as handle:
            handle.write(output)
        if settled:
            self.settle(session_id)
        return session_id

    def _serve_fake_session_socket(self, sock, child, manifest_path):
        """Minimal control-socket host for a fake running session: replies
        {"ok":true} to every line-JSON command, and on {"type":"kill"} also
        terminates the parked child and rewrites the manifest as exited."""

        def loop():
            while True:
                try:
                    conn, _ = sock.accept()
                except OSError:
                    return
                try:
                    conn.settimeout(2.0)
                    line = conn.makefile("rb").readline()
                    request = json.loads(line or b"{}")
                except Exception:
                    request = {}
                if request.get("type") == "kill":
                    try:
                        child.terminate()
                        child.wait(timeout=5)
                    except Exception:
                        pass
                    try:
                        with open(manifest_path) as handle:
                            manifest = json.load(handle)
                        manifest["state"] = "exited"
                        manifest["exit_code"] = 0
                        manifest["updated_at"] = int(time.time() * 1000)
                        with open(manifest_path, "w") as handle:
                            json.dump(manifest, handle)
                    except Exception:
                        pass
                try:
                    conn.sendall(b'{"ok":true,"error":null}\n')
                except Exception:
                    pass
                try:
                    conn.close()
                except Exception:
                    pass

        threading.Thread(target=loop, daemon=True).start()

    def pin(self, session_id, project_id="p", pinned_at=1):
        """Pin a session the way the shared state records it."""
        state = self.state()
        pins = state.setdefault("pinned_sessions", {})
        pins.setdefault(project_id, []).append(
            {
                "key": f"session:{session_id}",
                "project_id": project_id,
                "session_id": session_id,
                "pinned_at": pinned_at,
            }
        )
        with open(self.path("app-state.json"), "w") as handle:
            json.dump(state, handle, indent=2)

    def settle(self, session_id, event="Stop"):
        """Drop the durable hook seed a finished turn leaves behind."""
        with open(self.path("app-sessions", session_id, "last-hook-event.json"), "w") as handle:
            json.dump({"hook_event_name": event}, handle)

    def marker(self, session_id, name, body):
        with open(self.path("app-sessions", session_id, name), "w") as handle:
            json.dump(body, handle)

    def seed_resume_data(self, *session_ids):
        """Make claude-command fixtures archivable/resumable under the
        evidence-based gate: `can_archive_manifest` (and everything derived
        from it — Resume, Resume Agent, stop-and-archive) requires a managed
        storage dir holding a provider-created file, not just a
        resumable-looking command. Stamps each manifest with the storage
        path, like a real launch."""
        for session_id in session_ids:
            storage = self.path("managed-agents", session_id)
            os.makedirs(storage, exist_ok=True)
            with open(os.path.join(storage, "session.jsonl"), "w") as handle:
                handle.write("{}\n")
            manifest_path = self.path("app-sessions", session_id, "manifest.json")
            with open(manifest_path) as handle:
                manifest = json.load(handle)
            manifest["managed_storage_path"] = storage
            with open(manifest_path, "w") as handle:
                json.dump(manifest, handle)

    def read_marker(self, session_id, name):
        try:
            with open(self.path("app-sessions", session_id, name)) as handle:
                return json.load(handle)
        except (FileNotFoundError, ValueError):
            return None

    def has_marker(self, session_id, name):
        return os.path.exists(self.path("app-sessions", session_id, name))

    def manifests(self):
        found = {}
        for path in glob.glob(self.path("app-sessions", "*", "manifest.json")):
            try:
                with open(path) as handle:
                    manifest = json.load(handle)
                found[manifest["session"]["id"]] = manifest
            except (ValueError, KeyError, OSError):
                continue
        return found

    def running_sessions(self):
        return {k: v for k, v in self.manifests().items() if v.get("state") == "running"}

    def ports(self):
        try:
            with open(self.path("app-ports")) as handle:
                return [int(line) for line in handle.read().split()]
        except (FileNotFoundError, ValueError):
            return []

    def pair_device(self, token="phone-token-1", name="iPhone"):
        import hashlib

        with open(self.path("mobile", "devices.json"), "w") as handle:
            json.dump(
                {
                    "version": 1,
                    "devices": [
                        {
                            "id": "dev1",
                            "name": name,
                            "platform": "iOS",
                            "tokenHash": hashlib.sha256(token.encode()).hexdigest(),
                            "pairedAtUnixMs": 1,
                            "relayTokenHash": "x",
                        }
                    ],
                },
                handle,
            )
        return token

    def reserve_mobile_port(self):
        """Pick a free port and persist it as the phone endpoint.

        `server-port` is the *app's* rebinding contract: the Host binds it
        when free but must never write it, so a test that wants to reach the
        phone server has to publish the number itself — exactly as a desktop
        app would have."""
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        probe.close()
        with open(self.path("mobile", "server-port"), "w") as handle:
            handle.write(f"{port}\n")
        return port

    def cleanup(self):
        """Kill anything a fixture parked, and any host a case spawned."""
        for manifest in self.running_sessions().values():
            pid = manifest.get("pid")
            if not pid:
                continue
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        for child in self._hosts:
            try:
                child.kill()
                child.wait(timeout=2)
            except Exception:
                pass
        for sock in self._socks:
            try:
                sock.close()
            except OSError:
                pass


# ─────────────────────────── mock desktop app ───────────────────────────


class MockApp:
    """Stands in for a running Unpeel desktop: owns a hook-server port in
    `app-ports` and answers `/mcp/*`. Cases assert on `calls`."""

    def __init__(self, home, sidebar=None, fail_routes=(), auth_token=None):
        self.home = home
        self.calls = []
        self.sidebar = sidebar
        self.fail_routes = set(fail_routes)
        self.auth_token = auth_token or home.auth_token
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("Content-Length", 0))
                raw = self.rfile.read(length) if length else b"{}"
                try:
                    body = json.loads(raw or b"{}")
                except ValueError:
                    body = {}
                outer.calls.append(
                    (self.path, self.headers.get("x-unpeel-auth"), body)
                )
                if self.path in outer.fail_routes:
                    # What an older app does with a route it has never heard
                    # of: 404, not a polite error body.
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                if self.path == "/mcp/sidebar" and outer.sidebar is not None:
                    payload = json.dumps(outer.sidebar).encode()
                elif self.path == "/mcp/list-presets":
                    payload = b'{"presets":[]}'
                else:
                    payload = b'{"ok":true}'
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_port
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        registry = home.path("app-ports")
        try:
            with open(registry) as handle:
                ports = [int(value) for value in handle.read().split()]
        except (OSError, ValueError):
            ports = []
        ports = [port for port in ports if port != self.port] + [self.port]
        with open(registry, "w") as handle:
            handle.write("".join(f"{port}\n" for port in ports[-16:]))

    def called(self, route, **body_matches):
        for path, _token, body in self.calls:
            if path != route:
                continue
            if all(body.get(k) == v for k, v in body_matches.items()):
                return True
        return False

    def count(self, route):
        return len([1 for path, _, _ in self.calls if path == route])

    def auth_for(self, route):
        for path, token, _ in self.calls:
            if path == route:
                return token
        return None

    def stop(self):
        self.close()

    def close(self):
        try:
            self.server.shutdown()
        except Exception:
            pass
        try:
            self.server.server_close()
        except Exception:
            pass
        self.thread.join(timeout=2)
        registry = self.home.path("app-ports")
        try:
            with open(registry) as handle:
                ports = [int(value) for value in handle.read().split()]
            ports = [port for port in ports if port != self.port]
            if ports:
                with open(registry, "w") as handle:
                    handle.write("".join(f"{port}\n" for port in ports))
            else:
                os.unlink(registry)
        except (OSError, ValueError):
            pass


class FakeHost:
    """A stand-in session host: answers the control socket the way a real
    `unpeel-host` does, and records everything written to it.

    Lets cases assert on what a client *sends* a session (forwarded mouse
    reports, resize geometry, typed input) without launching a real agent.
    """

    def __init__(
        self,
        home,
        session_id,
        cols=80,
        rows=24,
        content="live session",
        mouse_reporting=False,
        alternate_screen=False,
        mouse_alternate_scroll=False,
        application_cursor=False,
        response_delay=0,
    ):
        self.path = home.path("app-sessions", session_id, "session.sock")
        self._manifest_path = home.path("app-sessions", session_id, "manifest.json")
        self.writes = []
        self.resizes = []
        self.resume_agent_generations = []
        self.snapshot_requests = 0
        self.cols = cols
        self.rows = rows
        self.content = content
        self.cursor_row = 0
        self.cursor_col = 0
        self.mouse_reporting = mouse_reporting
        self.mouse_any_motion = False
        self.alternate_screen = alternate_screen
        self.mouse_alternate_scroll = mouse_alternate_scroll
        self.application_cursor = application_cursor
        self.response_delay = response_delay
        self._stop = threading.Event()
        if os.path.exists(self.path):
            os.unlink(self.path)
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(self.path)
        self.server.listen(16)
        self.server.settimeout(0.5)
        threading.Thread(target=self._serve, daemon=True).start()

    def _viewport(self):
        blank = {"text": " " * self.cols, "styles": []}
        first = {"text": self.content.ljust(self.cols), "styles": []}
        return {
            "cols": self.cols,
            "rows": self.rows,
            "outputOffset": 0,
            "truncated": False,
            "cursorRow": self.cursor_row,
            "cursorCol": self.cursor_col,
            "scrollbackRows": 0,
            "viewportStartRow": 0,
            "scrollOffsetRows": 0,
            "inputModesKnown": True,
            "mouseReporting": self.mouse_reporting,
            "mouseButtonMotion": self.mouse_reporting,
            "mouseAnyMotion": self.mouse_any_motion,
            "alternateScreen": self.alternate_screen,
            "mouseAlternateScroll": self.mouse_alternate_scroll,
            "applicationCursor": self.application_cursor,
            "viewportRows": [first] + [dict(blank) for _ in range(self.rows - 1)],
        }

    def _serve(self):
        while not self._stop.is_set():
            try:
                conn, _ = self.server.accept()
            except (socket.timeout, OSError):
                continue
            try:
                data = b""
                while not data.endswith(b"\n"):
                    chunk = conn.recv(65536)
                    if not chunk:
                        break
                    data += chunk
                command = json.loads(data or b"{}")
                response = {"ok": True, "error": None}
                kind = command.get("type")
                if kind == "write":
                    self.writes.append(command.get("data", ""))
                elif kind == "resize":
                    self.cols = command.get("cols", self.cols)
                    self.rows = command.get("rows", self.rows)
                    self.resizes.append((self.cols, self.rows))
                elif kind == "viewport_snapshot":
                    self.snapshot_requests += 1
                    response["viewport"] = self._viewport()
                elif kind == "resume_agent":
                    self.resume_agent_generations.append(
                        command.get("expected_generation")
                    )
                elif kind == "kill":
                    # Honest-stop contract (session_ops): stop/archive/restart
                    # block until the manifest reaches "exited", so a fake
                    # host must file it the way a real one does.
                    try:
                        with open(self._manifest_path) as fh:
                            manifest = json.load(fh)
                        manifest["state"] = "exited"
                        manifest["exit_code"] = 0
                        manifest["updated_at"] = int(time.time() * 1000)
                        with open(self._manifest_path, "w") as fh:
                            json.dump(manifest, fh)
                    except Exception:
                        pass
                if self.response_delay:
                    time.sleep(self.response_delay)
                conn.sendall((json.dumps(response) + "\n").encode())
            except Exception:  # noqa: BLE001 — a bad frame is the case's problem
                pass
            finally:
                conn.close()

    def written(self):
        return "".join(self.writes)

    def close(self):
        self._stop.set()
        try:
            self.server.close()
        except OSError:
            pass


# ─────────────────────── the Host service under test ──────────────────────


class Serve:
    """A foreground, scoped `unpeel serve` process for process-level tests.

    The process gets the case's isolated `UNPEEL_HOME`, so it is one
    workspace worker rather than the real machine supervisor.  Keeping this
    separate from `Pty` is deliberate: serve conformance must prove there is
    no terminal UI in the Host process tree.
    """

    def __init__(self, home, env=None):
        self.home = home
        self.returncode = None
        self._log = open(home.path("serve-test.log"), "w")
        process_env = dict(
            os.environ,
            UNPEEL_HOME=home.root,
            UNPEEL_TEST="1",
        )
        process_env.update(env or {})
        self.process = subprocess.Popen(
            [BINARY, "serve"],
            cwd=CRATES,
            env=process_env,
            stdin=subprocess.DEVNULL,
            stdout=self._log,
            stderr=subprocess.STDOUT,
        )
        self.pid = self.process.pid

    def read_for(self, seconds):
        """Match the PTY wait helper without inventing a rendering loop."""
        end = time.monotonic() + seconds
        while time.monotonic() < end and self.process.poll() is None:
            time.sleep(min(0.1, max(0.0, end - time.monotonic())))
        self.returncode = self.process.poll()

    def wait_for(self, predicate, timeout=12.0, poll=0.3):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if self.process.poll() is not None:
                self.returncode = self.process.returncode
                return None
            value = predicate()
            if value:
                return value
            time.sleep(poll)
        return None

    def status(self):
        try:
            with open(self.home.path("serve.json")) as handle:
                return json.load(handle)
        except (FileNotFoundError, ValueError, OSError):
            return {}

    def ready(self, timeout=15.0):
        return self.wait_for(
            lambda: (
                self.status()
                if self.status().get("pid") == self.pid
                and self.status().get("hookPort")
                and self.status().get("localSocket")
                else None
            ),
            timeout=timeout,
        )

    def exited(self, timeout=5.0):
        if self.returncode is not None:
            return True
        try:
            self.returncode = self.process.wait(timeout=timeout)
            return True
        except subprocess.TimeoutExpired:
            return False

    def log(self):
        try:
            if not self._log.closed:
                self._log.flush()
            with open(self.home.path("serve-test.log")) as handle:
                return handle.read()
        except (OSError, ValueError):
            return ""

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.returncode = self.process.returncode
        try:
            self._log.close()
        except OSError:
            pass


# ─────────────────────────── the PTY under test ───────────────────────────


# ────────────────────────── the PTY under test ──────────────────────────


class Pty:
    def __init__(self, home, args=(), rows=45, cols=150, env=None):
        self.home = home
        self.rows = rows
        self.cols = cols
        self._alt_cols = cols + 1
        self.buffer = b""
        self.returncode = None
        self.pid, self.fd = pty.fork()
        if self.pid == 0:  # child
            os.environ["TERM"] = "xterm-256color"
            os.environ["UNPEEL_HOME"] = home.root
            os.environ["UNPEEL_TEST"] = "1"
            for key, value in (env or {}).items():
                os.environ[key] = value
            os.chdir(CRATES)
            os.execv(BINARY, ["unpeel", *args])
        self._set_size(cols)

    def _set_size(self, cols):
        fcntl.ioctl(
            self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, cols, 0, 0)
        )

    def resize_window(self, cols, rows=None):
        """Change the real window size (not the repaint toggle)."""
        if rows is not None:
            self.rows = rows
        self.cols = cols
        self._alt_cols = cols + 1
        self._set_size(cols)
        self.read_for(0.8)

    def read_for(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.1)
            if not ready:
                continue
            try:
                chunk = os.read(self.fd, 65536)
            except OSError:
                return
            if not chunk:
                return
            self.buffer += chunk

    def frame(self, settle=0.9):
        """Force a full repaint and return only what it drew."""
        mark = len(self.buffer)
        self._alt_cols, self.cols = self.cols, self._alt_cols
        self._set_size(self.cols)
        self.read_for(settle)
        self._alt_cols, self.cols = self.cols, self._alt_cols
        self._set_size(self.cols)
        self.read_for(settle)
        return strip_ansi(self.buffer[mark:])

    def grid(self, settle=0.9):
        """Force a repaint and parse it into a real screen grid."""
        mark = len(self.buffer)
        self._alt_cols, self.cols = self.cols, self._alt_cols
        self._set_size(self.cols)
        self.read_for(settle)
        self._alt_cols, self.cols = self.cols, self._alt_cols
        self._set_size(self.cols)
        self.read_for(settle)
        return Screen(self.buffer[mark:], self.rows, self.cols)

    def screen(self, settle=0.9):
        """`frame()` with whitespace collapsed — the usual assertion form."""
        return squeeze(self.frame(settle))

    def sidebar(self, settle=0.9):
        """Only the sidebar column of a fresh frame."""
        return self.grid(settle).sidebar()

    def preview_text(self, settle=0.9):
        """Only the preview pane of a fresh frame."""
        return self.grid(settle).preview()

    def selected_visible(self, settle=0.9):
        """True when some sidebar row carries the selection bar. The bar is
        a solid background, so it shows up as a run of reversed cells — we
        detect it by the marker column the renderer keeps for it."""
        grid = self.grid(settle)
        width = grid.sidebar_width()
        return any(row[:width].strip() for row in grid.lines()[1:])

    def all_text(self):
        return strip_ansi(self.buffer)

    def send(self, data, settle=0.4):
        if isinstance(data, str):
            data = data.encode()
        os.write(self.fd, data)
        if settle:
            self.read_for(settle)

    def type(self, text, per_char=0.04, settle=0.4):
        """Type like a person — the app coalesces input, and a single
        write of a whole string is not what a user does."""
        for char in text.encode():
            os.write(self.fd, bytes([char]))
            time.sleep(per_char)
        if settle:
            self.read_for(settle)

    def backspace(self, count=1):
        for _ in range(count):
            os.write(self.fd, b"\x7f")
            time.sleep(0.03)
        self.read_for(0.3)

    def click(self, col, row, button=0):
        """SGR mouse press+release at 0-based screen cell (col,row)."""
        press = f"\x1b[<{button};{col + 1};{row + 1}M".encode()
        release = f"\x1b[<{button};{col + 1};{row + 1}m".encode()
        os.write(self.fd, press + release)
        self.read_for(0.5)

    def drag(self, from_cell, to_cell, button=0):
        fcol, frow = from_cell
        tcol, trow = to_cell
        os.write(self.fd, f"\x1b[<{button};{fcol + 1};{frow + 1}M".encode())
        self.read_for(0.25)
        os.write(self.fd, f"\x1b[<{button + 32};{tcol + 1};{trow + 1}M".encode())
        self.read_for(0.25)
        os.write(self.fd, f"\x1b[<{button};{tcol + 1};{trow + 1}m".encode())
        self.read_for(0.5)

    def scroll(self, col, row, up=True, times=1):
        button = 64 if up else 65
        for _ in range(times):
            os.write(self.fd, f"\x1b[<{button};{col + 1};{row + 1}M".encode())
            self.read_for(0.15)

    def expect(self, *needles, timeout=10.0, absent=()):
        """Poll fresh frames until every needle is on screen (and every
        `absent` string is gone). Returns the matching screen text, or the
        last one seen on timeout — so a failed check can show what was there.

        Every content assertion should go through this: panes populate
        asynchronously (disk polls, bridge round-trips), and a single frame
        grabbed at the wrong moment is the classic source of a flaky suite.
        """
        end = time.monotonic() + timeout
        text = ""
        while True:
            text = self.grid(0.5).text()
            if all(n in text for n in needles) and not any(a in text for a in absent):
                return text
            if time.monotonic() >= end:
                return text

    def expect_missing(self, *needles, timeout=10.0):
        return self.expect(absent=needles, timeout=timeout)

    def wait_for(self, predicate, timeout=12.0, poll=0.3):
        """Poll `predicate()` while draining the PTY. Returns its truthy
        value, or None on timeout."""
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.read_for(poll)
            value = predicate()
            if value:
                return value
        return None

    def wait_for_text(self, needle, timeout=12.0):
        return self.wait_for(lambda: needle in squeeze(self.all_text()), timeout=timeout)

    def exited(self, timeout=5.0):
        if self.returncode is not None:
            return True
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            try:
                done, status = os.waitpid(self.pid, os.WNOHANG)
            except ChildProcessError:
                # The process is gone even if another caller reaped it.
                return True
            if done == self.pid:
                self.returncode = os.waitstatus_to_exitcode(status)
                return True
            time.sleep(0.1)
        return False

    def close(self):
        if self.returncode is not None:
            try:
                os.close(self.fd)
            except OSError:
                pass
            return
        try:
            os.write(self.fd, b"q")
        except OSError:
            pass
        self.read_for(0.6)
        try:
            done, status = os.waitpid(self.pid, os.WNOHANG)
            if done == self.pid:
                self.returncode = os.waitstatus_to_exitcode(status)
        except ChildProcessError:
            pass
        if self.returncode is None:
            try:
                os.kill(self.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                done, status = os.waitpid(self.pid, os.WNOHANG)
                if done == self.pid:
                    self.returncode = os.waitstatus_to_exitcode(status)
            except ChildProcessError:
                pass
        try:
            os.close(self.fd)
        except OSError:
            pass


# ─────────────────────────── hook + phone clients ───────────────────────────


def post_hook(port, session_id, event="Stop", body=None):
    """POST a provider hook event the way the installed hook scripts do."""
    payload = {"hook_event_name": event}
    payload.update(body or {})
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/hook/{session_id}",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        return urllib.request.urlopen(request, timeout=5).status
    except urllib.error.HTTPError as error:
        return error.code
    except OSError:
        return 0


def mcp_post(port, route, body, token=None, timeout=25):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{route}",
        data=json.dumps(body).encode(),
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    if token:
        request.add_header("x-unpeel-auth", token)
    try:
        response = urllib.request.urlopen(request, timeout=timeout)
        return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        try:
            return error.code, json.loads(error.read() or b"{}")
        except ValueError:
            return error.code, {}
    except Exception as error:  # noqa: BLE001 - surfaced as a failed check
        return 0, {"error": str(error)}


def mobile_request(port, path, token, method="GET", body=None, timeout=10, headers=None):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=json.dumps(body).encode() if body is not None else None,
        method=method,
    )
    request.add_header("Authorization", f"Bearer {token}")
    request.add_header("Content-Type", "application/json")
    for name, value in (headers or {}).items():
        request.add_header(name, value)
    try:
        response = urllib.request.urlopen(request, timeout=timeout)
        return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        try:
            return error.code, json.loads(error.read() or b"{}")
        except ValueError:
            return error.code, {}
    except Exception as error:  # noqa: BLE001
        return 0, {"error": str(error)}


def tui_hook_port(home, exclude=(), timeout=15):
    """The port the Host service registered, ignoring any mock app's.

    Historical name; `serve_hook_port` is the same function."""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        ports = [p for p in home.ports() if p not in exclude]
        if ports:
            return ports[-1]
        time.sleep(0.3)
    return None


serve_hook_port = tui_hook_port


def mobile_port(home, timeout=25):
    path = home.path("mobile", "server-port")
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            with open(path) as handle:
                value = handle.read().strip()
            if value:
                return int(value)
        except (FileNotFoundError, ValueError):
            pass
        time.sleep(0.3)
    return None


def wait_running(home, session_id, timeout=25):
    """Block until a session's manifest says running. `unpeel new` returns
    as soon as the id is minted; the host comes up a moment later."""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        manifest = home.manifests().get(session_id)
        if manifest and manifest.get("state") == "running":
            return True
        time.sleep(0.3)
    return False



class McpClient:
    """A `unpeel-host __mcp__` child for one caller Session: newline-delimited
    JSON-RPC 2.0 on stdio, exactly what an agent CLI speaks to the unified
    Unpeel MCP server. Caller identity is ``UNPEEL_SESSION_ID``; the child
    inherits the harness environment (so ``UNPEEL_CUA_DRIVER_BIN``,
    ``DISPLAY``, and friends reach the engine calls it makes)."""

    def __init__(self, home, session_id, host_binary=None):
        host = host_binary or os.path.join(os.path.dirname(BINARY), "unpeel-host")
        env = {
            **os.environ,
            "UNPEEL_HOME": home.root,
            "UNPEEL_TEST": "1",
            "UNPEEL_SESSION_ID": session_id,
        }
        self.proc = subprocess.Popen(
            [host, "__mcp__"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env,
            cwd=CRATES,
            text=True,
        )
        self.next_id = 1
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}})

    def request(self, method, params, timeout=150):
        rid = self.next_id
        self.next_id += 1
        self.proc.stdin.write(
            json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}) + "\n"
        )
        self.proc.stdin.flush()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                return {"error": "mcp server closed stdout"}
            try:
                message = json.loads(line)
            except ValueError:
                continue
            if message.get("id") == rid:
                return message
        return {"error": "timeout"}

    def tool_names(self):
        reply = self.request("tools/list", {})
        return [tool.get("name") for tool in reply.get("result", {}).get("tools", [])]

    def call(self, name, arguments):
        return self.request("tools/call", {"name": name, "arguments": arguments})

    @staticmethod
    def text(reply):
        """The concatenated text content of a tools/call reply."""
        content = reply.get("result", {}).get("content", [])
        return "\n".join(item.get("text", "") for item in content if isinstance(item, dict))

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def run_cli(home, args, timeout=30, expect_ok=None):
    """Run `unpeel <args>` against the fixture home."""
    env = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1")
    result = subprocess.run(
        [BINARY, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
        cwd=CRATES,
    )
    if expect_ok is not None:
        assert (result.returncode == 0) == expect_ok, result.stderr
    return result


# ─────────────────────────── case scaffolding ───────────────────────────


class Case:
    """Collects named checks and reports them in the runner's format."""

    def __init__(self, name):
        self.name = name
        self.checks = []
        self._case_started = time.monotonic()
        self.notes = []
        root = os.environ.get("UNPEEL_TUI_TEST_HOME") or f"/tmp/ut-{os.getpid()}"
        # A hosted session binds `<home>/app-sessions/<uuid>/session.sock`,
        # which adds ~55 bytes to the home path; sockaddr_un caps the total
        # near 104. Over that, bind() fails and the host dies with nothing on
        # screen to explain it — so fail loudly here instead.
        budget = len(root) + len("/app-sessions/") + 36 + len("/session.sock")
        if budget > 100:
            raise RuntimeError(
                f"test home {root!r} is too long ({budget} bytes with a socket "
                "suffix, limit ~104) — hosted sessions could not bind"
            )
        shutil.rmtree(root, ignore_errors=True)
        self.home = Home(root)
        self._closables = []

    def check(self, name, passed, detail=""):
        # Seconds since the case started: a stalled wait shows up as a jump
        # between consecutive checks in the runner's verbose output.
        elapsed = time.monotonic() - self._case_started
        self.checks.append((name, bool(passed), detail, elapsed))
        return bool(passed)

    def note(self, text):
        self.notes.append(text)

    def track(self, closable):
        self._closables.append(closable)
        return closable

    def pty(self, **kwargs):
        return self.track(Pty(self.home, **kwargs))

    def serve(self, **kwargs):
        return self.track(Serve(self.home, **kwargs))

    def app(self, **kwargs):
        return self.track(MockApp(self.home, **kwargs))

    def host(self, session_id, **kwargs):
        return self.track(FakeHost(self.home, session_id, **kwargs))

    def finish(self):
        for closable in reversed(self._closables):
            try:
                closable.close()
            except Exception:
                try:
                    closable.stop()
                except Exception:
                    pass
        self.home.cleanup()
        # mDNS advertisers are spawned children, not tracked objects.
        subprocess.run(
            ["pkill", "-f", "dns-sd -R"], capture_output=True, check=False
        )
        # After the case's own tracked cleanup, no host/sidecar/core may still
        # be bound to this home. A stray one is a leak: fail the case, then
        # kill it so it cannot poison the next case's home.
        leftovers = leftover_host_processes(self.home.root)
        self.check(
            "no leftover session hosts / mcp sidecars under the home",
            not leftovers,
            "; ".join(f"pid {pid}: {cmd}" for pid, cmd in leftovers),
        )
        for pid, _cmd in leftovers:
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        failed = 0
        for name, passed, detail, elapsed in self.checks:
            suffix = f"  [{detail}]" if detail and not passed else ""
            suffix += f"  (+{elapsed:.1f}s)"
            print(("PASS " if passed else "FAIL ") + name + suffix, flush=True)
            failed += 0 if passed else 1
        for text in self.notes:
            print("NOTE " + text, flush=True)
        if not self.checks:
            print("FAIL case recorded no checks", flush=True)
            failed += 1
        sys.exit(1 if failed else 0)


def run(name, body):
    """Entry point every case file ends with."""
    case = Case(name)
    try:
        body(case)
    except Exception:  # noqa: BLE001 — a crash is a failure, not a traceback
        import traceback

        case.check(f"{name} raised", False, traceback.format_exc().strip().splitlines()[-1])
        traceback.print_exc()
    finally:
        case.finish()
