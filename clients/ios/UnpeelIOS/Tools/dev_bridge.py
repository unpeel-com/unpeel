#!/usr/bin/env python3
from __future__ import annotations

import argparse
import base64
import hmac
import json
import os
import plistlib
import re
import secrets
import shutil
import socket
import subprocess
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse
from urllib.error import HTTPError
from urllib.request import Request, urlopen

ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\a]*(?:\a|\x1b\\)")

# Session ids are opaque handles that get joined onto the app-sessions dir and
# passed to `unpeel-host` as argv; reject anything that could escape the dir.
_UNSAFE_SESSION_RE = re.compile(r"[/\\]|\.\.")


def is_safe_session_id(value) -> bool:
    return isinstance(value, str) and bool(value) and not _UNSAFE_SESSION_RE.search(value)
KNOWN_PROVIDERS = {
    "claude",
    "codex",
    "amp",
    "gemini",
    "pi",
    "opencode",
    "cursor-agent",
    "grok",
}
NATIVE_PREFERENCE_DOMAINS = (
    "com.unpeel.native",
    "UnpeelNative",
    "com.unpeel.appDev",
)
SESSION_TITLES_KEY = "unpeel.native.sessionTitles"
NATIVE_PROJECTS_KEY = "unpeel.native.projects"
NATIVE_PRESETS_KEY = "unpeel.native.presets"
NATIVE_PINS_KEY = "unpeel.sidebar.pins"
REMOVED_PROJECTS_KEY = "unpeel.native.removedProjects"
PROJECT_ORDER_KEY = "unpeel.native.projectOrder"
SESSION_ORDER_PREFIX = "unpeel.native.sessionOrder."
PINNED_ORDER_PREFIX = "unpeel.native.pinnedOrder."
BUSY_OUTPUT_WINDOW_SECONDS = 3.5
REPLAY_TAIL_ALIGNMENT_LOOKBACK_BYTES = 16 * 1024
LIVE_CHUNK_END_ALIGNMENT_LOOKBACK_BYTES = 128
NATIVE_APPEND_SORT_ORDER = 2**31 - 1


def now_ms() -> int:
    return int(time.time() * 1000)


def strip_ansi(value: str) -> str:
    return ANSI_RE.sub("", value).replace("\r", "")


def read_json(path: Path, fallback):
    try:
        return json.loads(path.read_text())
    except Exception:
        return fallback


def command_provider(command: str):
    head = (command or "").strip().split(" ", 1)[0].lower()
    return head if head in KNOWN_PROVIDERS else None


def parse_bool(value: str | None) -> bool:
    return (value or "").lower() in {"1", "true", "yes", "on"}


def project_name(project_id: str, path: str | None = None) -> str:
    if path:
        name = Path(path).name
        if name:
            return name
    return project_id[:8] if project_id else "Unknown"


def git_head_branch(repo_path: str):
    """Current branch by parsing .git/HEAD directly (no subprocess); follows
    the gitdir pointer file used by worktree checkouts."""
    if not repo_path:
        return None
    git_entry = Path(repo_path) / ".git"
    head_path = git_entry / "HEAD"
    try:
        if git_entry.is_file():
            for line in git_entry.read_text().splitlines():
                if line.startswith("gitdir:"):
                    gitdir = line[len("gitdir:"):].strip()
                    base = Path(gitdir)
                    if not base.is_absolute():
                        base = Path(repo_path) / base
                    head_path = base / "HEAD"
                    break
        head = head_path.read_text().strip()
    except Exception:
        return None
    if head.startswith("ref: refs/heads/"):
        return head[len("ref: refs/heads/"):]
    return head[:7] if head else None


def cell(text: str) -> dict:
    return {
        "text": text,
        "foreground": None,
        "background": None,
        "style": {
            "bold": False,
            "italic": False,
            "underline": False,
            "inverse": False,
            "dim": False,
            "strikethrough": False,
        },
    }


def _is_utf8_continuation_byte(value: int) -> bool:
    return (value & 0b1100_0000) == 0b1000_0000


def align_tail_start_in_window(window: bytes, scan_start: int, desired_start: int) -> int:
    """Return a replay start that is not mid-UTF8 or mid-ANSI sequence."""
    initial_index = 0
    while initial_index < len(window) and _is_utf8_continuation_byte(window[initial_index]):
        initial_index += 1

    last_boundary = scan_start + initial_index
    state = "ground"
    for index, value in enumerate(window[initial_index:], start=initial_index):
        absolute = scan_start + index
        if value in (0x18, 0x1A):
            last_boundary = absolute + 1
            state = "ground"
            continue
        if value == 0x1B and state in ("escape", "escape_intermediate", "csi"):
            last_boundary = absolute
            state = "escape"
            continue
        if state == "ground":
            if value == 0x1B:
                last_boundary = absolute
                state = "escape"
            elif value in (ord("\n"), ord("\r")):
                last_boundary = absolute + 1
        elif state == "escape":
            if value == ord("["):
                state = "csi"
            elif value == ord("]"):
                state = "osc"
            elif value == ord("P"):
                state = "dcs"
            elif value in (ord("X"), ord("^"), ord("_")):
                state = "sos_pm_apc"
            elif 0x20 <= value <= 0x2F:
                state = "escape_intermediate"
            else:
                last_boundary = absolute + 1
                state = "ground"
        elif state == "escape_intermediate":
            if 0x30 <= value <= 0x7E:
                last_boundary = absolute + 1
                state = "ground"
        elif state == "csi":
            if 0x40 <= value <= 0x7E:
                last_boundary = absolute + 1
                state = "ground"
        elif state == "osc":
            if value == 0x07:
                last_boundary = absolute + 1
                state = "ground"
            elif value == 0x1B:
                state = "osc_escape"
        elif state == "osc_escape":
            if value == ord("\\"):
                last_boundary = absolute + 1
                state = "ground"
            else:
                state = "osc"
        elif state == "dcs":
            if value == 0x1B:
                state = "dcs_escape"
        elif state == "dcs_escape":
            if value == ord("\\"):
                last_boundary = absolute + 1
                state = "ground"
            else:
                state = "dcs"
        elif state == "sos_pm_apc":
            if value == 0x1B:
                state = "sos_pm_apc_escape"
        elif state == "sos_pm_apc_escape":
            if value == ord("\\"):
                last_boundary = absolute + 1
                state = "ground"
            else:
                state = "sos_pm_apc"

    return min(last_boundary, desired_start)


def align_live_chunk_end(data: bytes) -> int:
    """Latest safe end (complete escape sequence / UTF-8 rune) for a live
    chunk — a chunk cut mid-repaint bisects erase+redraw across two polls
    (visible flash). Returns len(data) when the end is already safe, no
    boundary was found in the lookback, or withholding would empty the
    chunk — never stall; withheld bytes are re-requested next poll."""
    if len(data) <= 1:
        return len(data)
    lookback = min(LIVE_CHUNK_END_ALIGNMENT_LOOKBACK_BYTES, len(data))
    scan_start = len(data) - lookback
    boundary = align_tail_start_in_window(data[scan_start:], scan_start, len(data))
    return boundary if boundary > 0 else len(data)


def align_replay_tail_start(path: Path, desired_start: int, retained_from: int = 0) -> int:
    if desired_start <= retained_from:
        return retained_from
    scan_start = max(retained_from, desired_start - REPLAY_TAIL_ALIGNMENT_LOOKBACK_BYTES)
    scan_len = desired_start - scan_start
    if scan_len <= 0:
        return desired_start
    with path.open("rb") as handle:
        handle.seek(scan_start)
        window = handle.read(scan_len)
    return align_tail_start_in_window(window, scan_start, desired_start)


def output_retained_from(session_dir: Path, logical_end: int) -> int:
    state = read_json(session_dir / "output-retention.json", {})
    if state.get("version") != 1:
        return 0
    try:
        return min(logical_end, max(0, int(state.get("retained_from", 0))))
    except (TypeError, ValueError):
        return 0


class UnpeelDevBridge:
    def __init__(self, root: Path):
        self.root = root.expanduser()
        self.sessions_dir = self.root / "app-sessions"
        self.state_path = self.root / "app-state.json"
        self.host_bin = self._host_binary()
        self.preferences_dir = Path.home() / "Library" / "Preferences"

    def bootstrap(self) -> dict:
        state = read_json(self.state_path, {})
        projects, folders = self._projects_and_folders(state)
        sessions = self._sessions(projects)
        presets = self._presets(state)
        return {
            "protocolVersion": 1,
            "macID": socket.gethostname(),
            "macName": socket.gethostname(),
            "folders": folders,
            "projects": projects,
            "presets": presets,
            "sessions": sessions,
            "capturedAtUnixMs": now_ms(),
        }

    def viewport(self, session_id: str, rows: int, columns: int) -> dict:
        session_dir = self.sessions_dir / session_id
        payload = self._viewport_payload(session_id, rows)
        viewport = payload.get("viewport") or {}
        rows = max(8, min(int(viewport.get("rows") or rows or 30), 80))
        columns = max(24, min(int(viewport.get("cols") or columns or 80), 180))
        rendered_rows = viewport.get("viewportRows") or []
        if len(rendered_rows) > rows:
            rendered_rows = rendered_rows[-rows:]
        visible = [row.get("text") or "" for row in rendered_rows if isinstance(row, dict)]
        while len(visible) < rows:
            visible.insert(0, "")

        cells = []
        first_rendered_row = max(0, rows - len(rendered_rows))
        for row_index, line in enumerate(visible):
            clipped = line[:columns]
            padded = clipped + (" " * max(0, columns - len(clipped)))
            row_cells = [cell(ch) for ch in padded[:columns]]
            source_index = row_index - first_rendered_row
            if 0 <= source_index < len(rendered_rows):
                self._apply_styles(row_cells, rendered_rows[source_index].get("styles") or [])
            cells.extend(row_cells)
        last = visible[-1] if visible else ""
        cursor_row = int(viewport.get("cursorRow") if viewport.get("cursorRow") is not None else rows - 1)
        cursor_col = int(viewport.get("cursorCol") if viewport.get("cursorCol") is not None else min(len(last), columns - 1))
        return {
            "sessionID": session_id,
            "sequence": int(viewport.get("outputOffset") or ((session_dir / "output.bin").stat().st_size if (session_dir / "output.bin").exists() else 0)),
            "rows": rows,
            "columns": columns,
            "cells": cells,
            "cursor": {
                "row": max(0, min(cursor_row, rows - 1)),
                "column": max(0, min(cursor_col, columns - 1)),
                "shape": "block",
                "visible": True,
            },
            "alternateScreen": False,
            "capturedAtUnixMs": now_ms(),
        }

    def _viewport_payload(
        self,
        session_id: str,
        requested_rows: int | None = None,
        requested_columns: int | None = None,
    ) -> dict:
        if requested_rows and requested_columns:
            rows = max(2, min(int(requested_rows), 120))
            columns = max(2, min(int(requested_columns), 300))
            return self._viewport_replay_payload(session_id, rows, columns)

        try:
            metrics_payload = self._control_command(
                session_id,
                {
                    "type": "viewport_snapshot",
                    "cols": 0,
                    "rows": 0,
                    "scroll_offset_rows": 0,
                    "viewport_rows": 1,
                },
            )
            metrics_viewport = metrics_payload.get("viewport") or {}
            remote_rows = max(2, min(int(metrics_viewport.get("rows") or requested_rows or 31), 120))
        except Exception:
            remote_rows = max(2, min(int(requested_rows or 31), 120))

        first = max(1, min(int(requested_rows or remote_rows), remote_rows, 80))
        for candidate_rows in [first, min(remote_rows, 36), 28, 20, 12, 8]:
            if candidate_rows > first:
                continue
            try:
                return self._control_command(
                    session_id,
                    {
                        "type": "viewport_snapshot",
                        "cols": 0,
                        "rows": 0,
                        "scroll_offset_rows": 0,
                        "viewport_rows": max(1, candidate_rows),
                    },
                )
            except Exception:
                continue
        raise RuntimeError("viewport snapshot unavailable")

    def _apply_styles(self, row_cells: list[dict], styles: list[dict]):
        for style in styles:
            try:
                start = int(style.get("start") or 0)
                length = int(style.get("len") or 0)
            except Exception:
                continue
            if length <= 0:
                continue
            foreground = self._remote_color(style.get("fg"))
            background = self._remote_color(style.get("bg"))
            for index in range(max(0, start), min(len(row_cells), start + length)):
                if foreground is not None:
                    row_cells[index]["foreground"] = foreground
                if background is not None:
                    row_cells[index]["background"] = background
                row_cells[index]["style"]["bold"] = bool(style.get("bold"))
                row_cells[index]["style"]["inverse"] = bool(style.get("inverse"))

    def _remote_color(self, value):
        if not isinstance(value, str) or not value:
            return None
        if value.startswith("rgb:"):
            parts = value[4:].split(",")
            if len(parts) == 3:
                try:
                    red, green, blue = [max(0, min(255, int(part))) for part in parts]
                    return {"kind": "rgb", "index": None, "red": red, "green": green, "blue": blue}
                except Exception:
                    return None
        if value.startswith("ansi256:"):
            try:
                index = max(0, min(255, int(value.split(":", 1)[1])))
                return {"kind": "ansi", "index": index, "red": None, "green": None, "blue": None}
            except Exception:
                return None
        return None

    def output_chunk(
        self,
        session_id: str,
        offset: int | None,
        limit: int,
        wait_ms: int = 0,
        _retention_retries: int = 3,
    ) -> dict:
        limit = max(1, min(limit, 8 * 1024 * 1024))
        session_dir = self.sessions_dir / session_id
        output_path = session_dir / "output.bin"
        size = output_path.stat().st_size if output_path.exists() else 0

        # Long-poll: when the client is caught up, hold the request until new
        # bytes land (or the wait expires) instead of returning empty — output
        # reaches the phone ~20ms after the TUI draws it, not a poll interval
        # later, and idle costs fewer requests than fixed-interval polling.
        if wait_ms > 0 and offset is not None and offset == size:
            deadline = time.time() + min(wait_ms, 25_000) / 1000
            while size <= offset and time.time() < deadline:
                time.sleep(0.02)
                size = output_path.stat().st_size if output_path.exists() else 0

        retained_from = output_retained_from(session_dir, size)
        explicit_offset_is_retained = (
            offset is not None and retained_from <= offset <= size
        )
        replay_tail = not explicit_offset_is_retained
        if replay_tail:
            desired_start = max(retained_from, size - limit)
            start = (
                align_replay_tail_start(output_path, desired_start, retained_from)
                if output_path.exists()
                else retained_from
            )
            read_limit = limit + max(0, desired_start - start)
        else:
            start = offset
            read_limit = limit
        truncated = (start > 0) if offset is None else (offset != start)

        if start == size:
            return {
                "sessionID": session_id,
                "offset": size,
                "nextOffset": size,
                "dataBase64": "",
                "truncated": truncated,
                "capturedAtUnixMs": now_ms(),
            }

        if output_path.exists():
            with output_path.open("rb") as handle:
                handle.seek(start)
                data = handle.read(min(read_limit, size - start))
        else:
            data = b""
        newest_size = output_path.stat().st_size if output_path.exists() else 0
        newest_retained_from = output_retained_from(session_dir, newest_size)
        if start < newest_retained_from:
            if _retention_retries > 0:
                return self.output_chunk(
                    session_id,
                    offset,
                    limit,
                    wait_ms,
                    _retention_retries - 1,
                )
            return {
                "sessionID": session_id,
                "offset": newest_retained_from,
                "nextOffset": newest_retained_from,
                "dataBase64": "",
                "truncated": True,
                "capturedAtUnixMs": now_ms(),
            }
        if not replay_tail and data:
            aligned_end = align_live_chunk_end(data)
            if aligned_end < len(data):
                data = data[:aligned_end]
        next_offset = start + len(data)
        return {
            "sessionID": session_id,
            "offset": start,
            "nextOffset": next_offset,
            "dataBase64": base64.b64encode(data).decode("ascii"),
            "truncated": truncated,
            "capturedAtUnixMs": now_ms(),
        }

    def metrics(self, session_id: str) -> dict:
        payload = self._control_command(
            session_id,
            {
                "type": "viewport_snapshot",
                "cols": 0,
                "rows": 0,
                "scroll_offset_rows": 0,
                # We only need cols/rows here. Asking for one rendered row
                # keeps the socket response small and avoids pulling a full
                # styled viewport just to size the iOS Ghostty canvas.
                "viewport_rows": 1,
            },
        )
        viewport = payload.get("viewport") or {}
        columns = int(viewport.get("cols") or 83)
        rows = int(viewport.get("rows") or 31)
        return {
            "sessionID": session_id,
            "columns": max(2, min(columns, 300)),
            "rows": max(2, min(rows, 120)),
            "capturedAtUnixMs": now_ms(),
        }

    def write(self, session_id: str, data: str) -> dict:
        self._control_command(session_id, {"type": "write", "data": data})
        return {"ok": True, "capturedAtUnixMs": now_ms()}

    def resize(self, session_id: str, columns: int, rows: int) -> dict:
        self._control_command(
            session_id,
            {
                "type": "resize",
                "cols": max(2, min(int(columns), 300)),
                "rows": max(2, min(int(rows), 120)),
            },
        )
        return {"ok": True, "capturedAtUnixMs": now_ms()}

    def save_uploaded_image(self, body: bytes, content_type) -> dict:
        """Phone image upload → dropped-images (the desktop drag-drop dir);
        the phone pastes the returned path into the agent's composer."""
        if not body:
            raise RuntimeError("empty upload")
        if len(body) > 4 * 1024 * 1024:
            raise RuntimeError("upload too large")
        ext = "png" if content_type and "png" in content_type.lower() else "jpg"
        directory = self.root / "dropped-images"
        directory.mkdir(parents=True, exist_ok=True)
        name = f"phone-{now_ms()}-{uuid.uuid4().hex[:8]}.{ext}"
        path = directory / name
        path.write_bytes(body)
        return {"path": str(path), "capturedAtUnixMs": now_ms()}

    def resize_desktop(self, session_id: str, payload: dict) -> dict:
        """Phone-driven temporary desktop resize: letterbox the desktop pane
        via the app (banner + revert), then nudge the PTY directly so a
        session with no mounted surface takes the phone size immediately."""
        if payload.get("clear") is True:
            self._mcp_post(
                "/mcp/phone-resize", {"session_id": session_id, "clear": True}
            )
            return {"ok": True, "capturedAtUnixMs": now_ms()}
        columns = payload.get("columns") or payload.get("cols")
        rows = payload.get("rows")
        if columns is None or rows is None:
            raise RuntimeError("missing columns or rows")
        cols = max(2, min(int(columns), 300))
        rows = max(2, min(int(rows), 120))
        self._mcp_post(
            "/mcp/phone-resize",
            {"session_id": session_id, "cols": cols, "rows": rows},
        )
        try:
            self._control_command(
                session_id, {"type": "resize", "cols": cols, "rows": rows}
            )
        except Exception:
            pass
        return {"ok": True, "capturedAtUnixMs": now_ms()}

    def organize_session(self, session_id: str, payload: dict) -> dict:
        """Phone-driven rename/pin: proxied to the running app's
        /mcp/organize-session maintenance endpoint so it lands in the same
        native title/pin overlays the desktop sidebar uses."""
        title = payload.get("title")
        pinned = payload.get("pinned")
        mcp_payload = {"session_id": session_id}
        if isinstance(title, str) and title.strip():
            mcp_payload["title"] = title.strip()
        if isinstance(pinned, bool):
            mcp_payload["pinned"] = pinned
        if len(mcp_payload) == 1:
            raise RuntimeError("missing title or pinned")
        self._mcp_post("/mcp/organize-session", mcp_payload)
        return {"ok": True, "capturedAtUnixMs": now_ms()}

    def restart_session(self, session_id: str) -> dict:
        """Phone-driven restart: proxied to the app's /mcp/restart-session
        maintenance endpoint, which reuses the desktop restart path (resume
        flag, title, pin, worktree, grants preserved)."""
        if not isinstance(session_id, str) or not session_id.strip():
            raise RuntimeError("missing session_id")
        self._mcp_post("/mcp/restart-session", {"session_id": session_id.strip()})
        return {"ok": True, "capturedAtUnixMs": now_ms()}

    def create_session(self, payload: dict) -> dict:
        project_id = payload.get("projectID") or payload.get("project_id")
        preset_id = payload.get("presetID") or payload.get("preset_id")
        command = payload.get("command")
        if not isinstance(project_id, str) or not project_id.strip():
            raise RuntimeError("missing projectID")
        if not isinstance(preset_id, str) and not isinstance(command, str):
            raise RuntimeError("missing presetID or command")

        mcp_payload = {"project_id": project_id.strip()}
        if isinstance(preset_id, str) and preset_id.strip():
            mcp_payload["preset_id"] = preset_id.strip()
        if isinstance(command, str):
            mcp_payload["command"] = command
        for source, target in [
            ("worktreeBranch", "worktree_branch"),
            ("worktree_branch", "worktree_branch"),
            ("worktreeName", "worktree_name"),
            ("worktree_name", "worktree_name"),
            ("worktreeBaseRef", "worktree_base_ref"),
            ("worktree_base_ref", "worktree_base_ref"),
        ]:
            value = payload.get(source)
            if isinstance(value, str) and value.strip():
                mcp_payload[target] = value.strip()

        response = self._mcp_post("/mcp/start-session", mcp_payload)
        session = response.get("session") if isinstance(response, dict) else None
        session_id = session.get("id") if isinstance(session, dict) else None
        if not isinstance(session_id, str) or not session_id:
            raise RuntimeError("start-session returned no session id")
        return {
            "sessionID": session_id,
            "session": session,
            "capturedAtUnixMs": now_ms(),
        }

    def transcript_snapshot(
        self,
        session_id: str,
        entries: int,
        include_tools: bool,
        max_bytes: int | None,
    ) -> dict:
        try:
            payload = self._transcript_payload(
                "snapshot",
                session_id,
                entries=entries,
                include_tools=include_tools,
                max_bytes=max_bytes,
            )
            return self._remote_transcript_snapshot(session_id, payload)
        except Exception as exc:
            return self._unresolved_transcript_snapshot(session_id, str(exc))

    def transcript_stream(
        self,
        session_id: str,
        offset: int,
        partial: str,
        include_tools: bool,
        max_bytes: int | None,
    ) -> dict:
        try:
            payload = self._transcript_payload(
                "stream",
                session_id,
                offset=offset,
                partial=partial,
                include_tools=include_tools,
                max_bytes=max_bytes,
            )
            return self._remote_transcript_stream(session_id, payload)
        except Exception as exc:
            return self._unresolved_transcript_stream(session_id, offset, partial, str(exc))

    def transcript_history(
        self,
        session_id: str,
        before_offset: int | None,
        entries: int,
        include_tools: bool,
        max_bytes: int | None,
    ) -> dict:
        try:
            payload = self._transcript_payload(
                "history",
                session_id,
                before_offset=before_offset,
                entries=entries,
                include_tools=include_tools,
                max_bytes=max_bytes,
            )
            return self._remote_transcript_history(session_id, payload)
        except Exception as exc:
            return self._unresolved_transcript_history(session_id, before_offset or 0, str(exc))

    def transcript_markdown(self, session_id: str, entries: int | None = None) -> dict:
        """Conversation as Markdown — mirrors /mobile/transcript-markdown.

        The markdown mode prints raw Markdown (not JSON) filtered by the
        app-wide transcript settings, so this shells out directly instead of
        going through _transcript_payload. `entries` overrides the settings
        range (count, 0 = whole conversation).
        """
        if self.host_bin is None:
            raise RuntimeError("unpeel-host binary unavailable; build crates/unpeel-host first")
        command = [str(self.host_bin), "__transcript__", "markdown", session_id]
        if entries is not None:
            command.extend(["--entries", str(max(0, int(entries)))])
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError((result.stderr or result.stdout or "transcript command failed").strip())
        return {"sessionID": session_id, "markdown": result.stdout.strip()}

    def _host_binary(self) -> Path | None:
        raw = os.environ.get("UNPEEL_HOST_BIN")
        if raw:
            path = Path(raw).expanduser()
            if path.is_file():
                return path

        repo_root = Path(__file__).resolve().parents[4]
        candidates = [
            repo_root / "crates" / "target" / "debug" / "unpeel-host",
            repo_root / "crates" / "target" / "release" / "unpeel-host",
            repo_root / "target" / "debug" / "unpeel-host",
            repo_root / "target" / "release" / "unpeel-host",
        ]
        for path in candidates:
            if path.is_file():
                return path

        resolved = shutil.which("unpeel-host")
        return Path(resolved) if resolved else None

    def _transcript_payload(
        self,
        mode: str,
        session_id: str,
        *,
        entries: int | None = None,
        offset: int | None = None,
        before_offset: int | None = None,
        partial: str | None = None,
        include_tools: bool = False,
        max_bytes: int | None = None,
    ) -> dict:
        if self.host_bin is None:
            raise RuntimeError("unpeel-host binary unavailable; build crates/unpeel-host first")
        command = [str(self.host_bin), "__transcript__", mode, session_id]
        if entries is not None:
            command.extend(["--entries", str(max(1, min(int(entries), 500)))])
        if offset is not None:
            command.extend(["--offset", str(max(0, int(offset)))])
        if before_offset is not None:
            command.extend(["--before-offset", str(max(0, int(before_offset)))])
        if partial:
            command.extend(["--partial", partial])
        if include_tools:
            command.append("--include-tools")
        if max_bytes is not None:
            command.extend(["--max-bytes", str(max(1, int(max_bytes)))])

        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError((result.stderr or result.stdout or "transcript command failed").strip())
        return json.loads(result.stdout)

    def _viewport_replay_payload(self, session_id: str, rows: int, columns: int) -> dict:
        if self.host_bin is None:
            raise RuntimeError("unpeel-host binary unavailable; build crates/unpeel-host first")
        rows = max(2, min(int(rows), 120))
        columns = max(2, min(int(columns), 300))
        command = [
            str(self.host_bin),
            "__viewport__",
            "snapshot",
            session_id,
            "--cols",
            str(columns),
            "--rows",
            str(rows),
            "--viewport-rows",
            str(rows),
            "--max-bytes",
            str(2 * 1024 * 1024),
        ]
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError((result.stderr or result.stdout or "viewport command failed").strip())
        return {"viewport": json.loads(result.stdout)}

    def _remote_transcript_snapshot(self, session_id: str, payload: dict) -> dict:
        return {
            "sessionID": session_id,
            "providerID": payload.get("provider"),
            "source": payload.get("source"),
            "resolved": True,
            "startOffset": int(payload.get("start_offset") or 0),
            "nextOffset": int(payload.get("next_offset") or 0),
            "entries": self._remote_transcript_entries(
                session_id,
                payload.get("entries") or [],
                int(payload.get("start_offset") or 0),
            ),
            "fallbackReason": None,
            "updatedAtUnixMs": int(payload.get("updated_at") or now_ms()),
        }

    def _remote_transcript_history(self, session_id: str, payload: dict) -> dict:
        offset = int(payload.get("offset") or 0)
        return {
            "sessionID": session_id,
            "providerID": payload.get("provider"),
            "source": payload.get("source"),
            "resolved": True,
            "startOffset": offset,
            "endOffset": int(payload.get("next_offset") or offset),
            "truncated": bool(payload.get("truncated")),
            "entries": self._remote_transcript_entries(session_id, payload.get("entries") or [], offset),
            "fallbackReason": None,
            "updatedAtUnixMs": int(payload.get("updated_at") or now_ms()),
        }

    def _remote_transcript_stream(self, session_id: str, payload: dict) -> dict:
        offset = int(payload.get("offset") or 0)
        return {
            "sessionID": session_id,
            "providerID": payload.get("provider"),
            "source": payload.get("source"),
            "resolved": True,
            "offset": offset,
            "nextOffset": int(payload.get("next_offset") or offset),
            "partial": payload.get("partial") or "",
            "truncated": bool(payload.get("truncated")),
            "entries": self._remote_transcript_entries(session_id, payload.get("entries") or [], offset),
            "fallbackReason": None,
            "updatedAtUnixMs": int(payload.get("updated_at") or now_ms()),
        }

    def _remote_transcript_entries(self, session_id: str, entries: list[dict], sequence_base: int) -> list[dict]:
        result = []
        for index, entry in enumerate(entries):
            role = self._remote_transcript_role(entry.get("role"))
            text = entry.get("text") if isinstance(entry.get("text"), str) else ""
            entry_id = str(entry.get("id") or f"{session_id}:transcript:{sequence_base}:{index}")
            sequence = entry.get("sequence")
            if not isinstance(sequence, int):
                sequence = max(0, sequence_base) + index
            created_at = entry.get("createdAtUnixMs")
            if not isinstance(created_at, int):
                created_at = entry.get("createdAt")
            if not isinstance(created_at, int):
                created_at = entry.get("created_at")
            remote_blocks = []
            raw_blocks = entry.get("blocks") if isinstance(entry.get("blocks"), list) else []
            for block_index, block in enumerate(raw_blocks):
                if not isinstance(block, dict):
                    continue
                kind = self._remote_transcript_block_kind(block.get("kind"), role)
                block_text = block.get("text") if isinstance(block.get("text"), str) else None
                metadata = block.get("metadata") if isinstance(block.get("metadata"), dict) else {}
                remote_blocks.append({
                    "id": str(block.get("id") or f"{entry_id}:block:{block_index}"),
                    "kind": kind,
                    "text": block_text,
                    "toolName": block.get("toolName") or block.get("tool_name"),
                    "status": block.get("status"),
                    "metadata": {str(key): str(value) for key, value in metadata.items()},
                })
            if not remote_blocks and text:
                remote_blocks = [
                    {
                        "id": f"{entry_id}:block:0",
                        "kind": self._remote_transcript_block_kind(None, role),
                        "text": text,
                        "toolName": None,
                        "status": None,
                        "metadata": {},
                    }
                ]
            result.append({
                "id": entry_id,
                "sequence": sequence,
                "role": role,
                "text": text,
                "blocks": remote_blocks,
                "createdAtUnixMs": created_at if isinstance(created_at, int) else None,
            })
        return result

    def _remote_transcript_role(self, value) -> str:
        role = str(value or "").strip().lower()
        if role in {"user", "assistant", "system", "tool", "reasoning", "info"}:
            return role
        return "unknown"

    def _remote_transcript_block_kind(self, value, role: str) -> str:
        kind = str(value or "").strip()
        allowed = {
            "text",
            "reasoning",
            "toolCall",
            "toolResult",
            "permission",
            "info",
            "fileChange",
            "diff",
            "planUpdate",
            "usage",
            "attachment",
        }
        if kind in allowed:
            return kind
        return {
            "reasoning": "reasoning",
            "tool": "toolCall",
            "info": "info",
        }.get(role, "text")

    def _unresolved_transcript_snapshot(self, session_id: str, reason: str) -> dict:
        return {
            "sessionID": session_id,
            "providerID": self._session_provider(session_id),
            "source": None,
            "resolved": False,
            "startOffset": 0,
            "nextOffset": 0,
            "entries": [],
            "fallbackReason": reason,
            "updatedAtUnixMs": now_ms(),
        }

    def _unresolved_transcript_stream(self, session_id: str, offset: int, partial: str, reason: str) -> dict:
        return {
            "sessionID": session_id,
            "providerID": self._session_provider(session_id),
            "source": None,
            "resolved": False,
            "offset": max(0, int(offset)),
            "nextOffset": max(0, int(offset)),
            "partial": partial,
            "truncated": False,
            "entries": [],
            "fallbackReason": reason,
            "updatedAtUnixMs": now_ms(),
        }

    def _unresolved_transcript_history(self, session_id: str, before_offset: int, reason: str) -> dict:
        offset = max(0, int(before_offset))
        return {
            "sessionID": session_id,
            "providerID": self._session_provider(session_id),
            "source": None,
            "resolved": False,
            "startOffset": offset,
            "endOffset": offset,
            "truncated": False,
            "entries": [],
            "fallbackReason": reason,
            "updatedAtUnixMs": now_ms(),
        }

    def _session_provider(self, session_id: str) -> str | None:
        manifest = read_json(self.sessions_dir / session_id / "manifest.json", {})
        session = manifest.get("session") or {}
        return command_provider(session.get("command") or "")

    def _control_command(self, session_id: str, command_value: dict) -> dict:
        session_dir = self.sessions_dir / session_id
        sock_path = session_dir / "session.sock"
        if not sock_path.exists():
            raise RuntimeError("session socket unavailable")
        command = json.dumps(command_value).encode("utf-8") + b"\n"
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(2)
            client.connect(str(sock_path))
            client.sendall(command)
            response = b""
            while not response.endswith(b"\n"):
                chunk = client.recv(4096)
                if not chunk:
                    break
                response += chunk
        payload = json.loads(response.decode("utf-8")) if response else {"ok": True}
        if payload.get("ok") is False:
            raise RuntimeError(payload.get("error") or "control command failed")
        return payload

    def _mcp_post(self, path: str, payload: dict) -> dict:
        token_path = self.root / "mcp" / "auth-token"
        try:
            token = token_path.read_text().strip()
        except Exception:
            raise RuntimeError("mcp auth token unavailable")
        if not token:
            raise RuntimeError("mcp auth token unavailable")

        errors = []
        body = json.dumps(payload).encode("utf-8")
        for port in self._candidate_app_ports():
            request = Request(
                f"http://127.0.0.1:{port}{path}",
                data=body,
                method="POST",
                headers={
                    "Content-Type": "application/json",
                    "x-unpeel-auth": token,
                },
            )
            try:
                with urlopen(request, timeout=5) as response:
                    return json.loads(response.read().decode("utf-8"))
            except HTTPError as exc:
                detail = exc.read().decode("utf-8", errors="ignore")
                try:
                    decoded = json.loads(detail)
                    if isinstance(decoded, dict) and decoded.get("error"):
                        raise RuntimeError(str(decoded["error"]))
                except json.JSONDecodeError:
                    pass
                raise RuntimeError(detail or f"HTTP {exc.code}")
            except Exception as exc:
                errors.append(f"{port}: {exc}")
        raise RuntimeError("Unpeel app is not reachable" + (f" ({'; '.join(errors[-3:])})" if errors else ""))

    def _candidate_app_ports(self) -> list[int]:
        path = self.root / "app-ports"
        try:
            raw = path.read_text()
        except Exception:
            return []
        ports = []
        for line in reversed(raw.splitlines()):
            try:
                port = int(line.strip())
            except Exception:
                continue
            if 0 < port <= 65535 and port not in ports:
                ports.append(port)
        return ports

    def _projects_and_folders(self, state: dict) -> tuple[list[dict], list[dict]]:
        raw_projects = list(state.get("projects") or [])
        existing_paths = {
            item.get("path")
            for item in raw_projects
            if isinstance(item, dict) and item.get("path")
        }
        for item in self._native_project_records():
            path = item.get("path")
            project_id = item.get("id")
            if not path or not project_id or path in existing_paths:
                continue
            existing_paths.add(path)
            raw_projects.append({
                "id": project_id,
                "name": item.get("name") or project_name(project_id, path),
                "path": path,
                "parent_project_id": item.get("parentProjectID"),
                "worktree_branch": item.get("worktreeBranch"),
                "sort_order": NATIVE_APPEND_SORT_ORDER,
                "is_folder": False,
                "mcp_blocked": False,
            })

        known_project_ids = {
            item.get("id")
            for item in raw_projects
            if isinstance(item, dict) and item.get("id")
        }
        removed_ids = self._removed_project_ids(known_project_ids)
        if removed_ids:
            raw_projects = [
                item
                for item in raw_projects
                if item.get("id") not in removed_ids
                and item.get("parent_project_id") not in removed_ids
            ]

        blocked = set(state.get("mcp_blocked_projects") or [])
        folder_ids = {
            item.get("id")
            for item in raw_projects
            if item.get("id") and item.get("is_folder") is True
        }
        projects = []
        folders = []
        seen = set()
        for item in raw_projects:
            project_id = item.get("id")
            if not project_id:
                continue
            seen.add(project_id)
            if item.get("is_folder") is True:
                folders.append({
                    "id": project_id,
                    "name": item.get("name") or project_name(project_id, item.get("path")),
                    "parentFolderID": item.get("parent_project_id"),
                    "colorID": None,
                    "sortOrder": item.get("sort_order"),
                })
            else:
                raw_parent_id = item.get("parent_project_id")
                folder_id = raw_parent_id if raw_parent_id in folder_ids else None
                parent_project_id = None if raw_parent_id in folder_ids else raw_parent_id
                projects.append({
                    "id": project_id,
                    "name": item.get("name") or project_name(project_id, item.get("path")),
                    "path": item.get("path") or "",
                    "folderID": folder_id,
                    "parentProjectID": parent_project_id,
                    "worktreeBranch": item.get("worktree_branch"),
                    "gitBranch": git_head_branch(item.get("path") or ""),
                    "mcpBlocked": bool(item.get("mcp_blocked") or project_id in blocked),
                    "sortOrder": item.get("sort_order"),
                })

        projects = self._sort_projects_like_native(projects)
        folders.sort(key=lambda item: (item.get("sortOrder") is None, item.get("sortOrder") or 0, item.get("name") or ""))
        return projects, folders

    def _presets(self, state: dict) -> list[dict]:
        raw = self._overlaid_presets(state.get("presets") or [])
        result = []
        for item in raw:
            if item.get("enabled", True) is False:
                continue
            command = item.get("command") or ""
            result.append({
                "id": item.get("id") or command or "terminal",
                "label": item.get("label") or command or "Terminal",
                "command": command,
                "cliID": command_provider(command),
                "enabled": bool(item.get("enabled", True)),
                "quickLaunch": bool(item.get("quick_launch", False)),
                "isDefault": False,
            })
        return result

    def _overlaid_presets(self, base: list[dict]) -> list[dict]:
        overlay = self._native_preset_overlay()
        if not overlay:
            return [item for item in base if isinstance(item, dict)]
        removed = {
            item
            for item in overlay.get("removedIDs", overlay.get("removed_ids", [])) or []
            if isinstance(item, str)
        }
        edited = {
            item.get("id"): item
            for item in overlay.get("edited", []) or []
            if isinstance(item, dict) and isinstance(item.get("id"), str)
        }
        result = []
        base_ids = set()
        for item in base:
            if not isinstance(item, dict):
                continue
            preset_id = item.get("id")
            if not isinstance(preset_id, str) or preset_id in removed:
                continue
            base_ids.add(preset_id)
            result.append(edited.get(preset_id, item))
        for item in overlay.get("added", []) or []:
            preset_id = item.get("id") if isinstance(item, dict) else None
            if isinstance(preset_id, str) and preset_id not in removed and preset_id not in base_ids:
                result.append(item)
        return [self._sanitize_preset(item) for item in result]

    def _native_preset_overlay(self) -> dict | None:
        raw = self._native_preferences_with_key(NATIVE_PRESETS_KEY).get(NATIVE_PRESETS_KEY)
        decoded = self._decode_json_preference(raw)
        if isinstance(decoded, dict) and (
            "added" in decoded or "edited" in decoded or "removedIDs" in decoded or "removed_ids" in decoded
        ):
            return decoded
        if isinstance(decoded, dict) and isinstance(decoded.get("global"), dict):
            return decoded["global"]
        return None

    def _sanitize_preset(self, item: dict) -> dict:
        result = dict(item)
        if command_provider(result.get("command") or "") is None:
            result["quick_launch"] = False
        return result

    def _sessions(self, projects: list[dict]) -> list[dict]:
        project_ids = {project["id"] for project in projects}
        title_overrides = self._session_title_overrides()
        sessions = []
        session_project_ids = {}
        for manifest_path in self.sessions_dir.glob("*/manifest.json"):
            manifest = read_json(manifest_path, {})
            info = manifest.get("session") or {}
            session_id = info.get("id") or manifest_path.parent.name
            project_id = info.get("project_id") or "unknown"
            command = info.get("command") or ""
            status = "running" if manifest.get("state") == "running" else "exited"
            output_path = manifest_path.parent / "output.bin"
            output_mtime = output_path.stat().st_mtime if output_path.exists() else manifest_path.stat().st_mtime
            activity = "idle"
            if status == "running" and time.time() - output_mtime < BUSY_OUTPUT_WINDOW_SECONDS:
                activity = "working"
            title = title_overrides.get(session_id) or info.get("label") or command or session_id[:8]
            if project_id not in project_ids:
                continue
            session_project_ids[session_id] = project_id
            sessions.append({
                "id": session_id,
                "projectID": project_id,
                "providerID": command_provider(command),
                "title": title,
                "command": command,
                "createdAtUnixMs": int(info.get("created_at") or manifest_path.stat().st_mtime * 1000),
                "updatedAtUnixMs": int(output_mtime * 1000),
                "status": status,
                "activity": activity,
                "unread": False,
                "pinned": False,
                "worktreePath": info.get("worktree_path"),
                "worktreeBranch": info.get("worktree_branch"),
                "lastOutputPreview": self._last_preview(output_path),
            })
        pins_by_project = self._pinned_records_by_project(session_project_ids)
        pinned_session_ids = {
            pin.get("session_id")
            for pins in pins_by_project.values()
            for pin in pins
            if isinstance(pin.get("session_id"), str)
        }
        for session in sessions:
            session["pinned"] = session["id"] in pinned_session_ids
        return self._sort_sessions_like_native(sessions, projects, pins_by_project)

    def _native_preference_values(self) -> list[tuple[str, dict]]:
        values = []
        for domain in NATIVE_PREFERENCE_DOMAINS:
            path = self.preferences_dir / f"{domain}.plist"
            if not path.exists():
                continue
            try:
                with path.open("rb") as handle:
                    loaded = plistlib.load(handle)
            except Exception:
                continue
            if isinstance(loaded, dict):
                values.append((domain, loaded))
        return values

    def _native_tree_preferences(self) -> dict:
        loaded = self._native_preference_values()
        for _, values in loaded:
            if values.get(NATIVE_PROJECTS_KEY) is not None or values.get(PROJECT_ORDER_KEY) is not None:
                return values
        for _, values in loaded:
            if values.get(REMOVED_PROJECTS_KEY) is not None:
                return values
        return {}

    def _native_preferences_with_key(self, key: str) -> dict:
        for _, values in self._native_preference_values():
            if values.get(key) is not None:
                return values
        return {}

    def _decode_json_preference(self, raw):
        if isinstance(raw, bytes):
            try:
                return json.loads(raw.decode("utf-8"))
            except Exception:
                return None
        if isinstance(raw, str):
            try:
                return json.loads(raw)
            except Exception:
                return None
        return raw

    def _native_project_records(self) -> list[dict]:
        raw = self._native_tree_preferences().get(NATIVE_PROJECTS_KEY)
        decoded = self._decode_json_preference(raw)
        if not isinstance(decoded, list):
            return []
        return [
            item
            for item in decoded
            if isinstance(item, dict) and isinstance(item.get("id"), str) and isinstance(item.get("path"), str)
        ]

    def _removed_project_ids(self, known_project_ids: set[str]) -> set[str]:
        raw = self._native_tree_preferences().get(REMOVED_PROJECTS_KEY)
        if not isinstance(raw, list):
            return set()
        return {
            item
            for item in raw
            if isinstance(item, str) and item in known_project_ids
        }

    def _project_order_ids(self, parent_id: str | None = None) -> list[str]:
        key = PROJECT_ORDER_KEY if parent_id is None else f"{PROJECT_ORDER_KEY}.{parent_id}"
        raw = self._native_tree_preferences().get(key)
        if not isinstance(raw, list):
            return []
        return [item for item in raw if isinstance(item, str)]

    def _session_order_ids(self, project_id: str) -> list[str]:
        key = f"{SESSION_ORDER_PREFIX}{project_id}"
        raw = self._native_tree_preferences().get(key)
        if raw is None:
            raw = self._native_preferences_with_key(key).get(key)
        if not isinstance(raw, list):
            return []
        return [item for item in raw if isinstance(item, str)]

    def _pinned_order_ids(self, project_id: str) -> list[str]:
        key = f"{PINNED_ORDER_PREFIX}{project_id}"
        raw = self._native_preferences_with_key(key).get(key)
        if not isinstance(raw, list):
            return []
        return [item for item in raw if isinstance(item, str)]

    def _sort_key(self, item: dict):
        sort_order = item.get("sortOrder")
        if not isinstance(sort_order, int):
            sort_order = NATIVE_APPEND_SORT_ORDER
        return (sort_order, str(item.get("name") or "").lower(), str(item.get("id") or ""))

    def _apply_order_overlay(self, items: list[dict], order_ids: list[str]) -> list[dict]:
        if not order_ids:
            return items
        ids = {item.get("id") for item in items}
        known = [item_id for item_id in order_ids if item_id in ids]
        if not known:
            return items
        rank = {item_id: index for index, item_id in enumerate(known)}
        ordered = sorted(
            [item for item in items if item.get("id") in rank],
            key=lambda item: rank[item["id"]],
        )
        rest = [item for item in items if item.get("id") not in rank]
        return ordered + rest

    def _sort_projects_like_native(self, projects: list[dict]) -> list[dict]:
        folder_child_ids = {project["id"] for project in projects if project.get("folderID") is not None}
        top_level = [
            project
            for project in projects
            if project.get("folderID") is None
            and project.get("parentProjectID") is None
            and project.get("worktreeBranch") is None
        ]
        top_level = self._apply_order_overlay(
            sorted(top_level, key=self._sort_key),
            self._project_order_ids(),
        )

        result = list(top_level)
        seen = {project["id"] for project in result}

        for folder_id in sorted({project.get("folderID") for project in projects if project.get("folderID") is not None}):
            folder_items = sorted(
                [project for project in projects if project.get("folderID") == folder_id],
                key=self._sort_key,
            )
            result.extend(folder_items)
            seen.update(project["id"] for project in folder_items)

        parent_ids = [project["id"] for project in top_level] + sorted({
            project.get("parentProjectID")
            for project in projects
            if project.get("parentProjectID") is not None
        })
        for parent_id in parent_ids:
            if not parent_id:
                continue
            child_items = [
                project
                for project in projects
                if project.get("parentProjectID") == parent_id
                and project.get("worktreeBranch") is not None
                and project["id"] not in seen
            ]
            if not child_items:
                continue
            child_items = self._apply_order_overlay(
                sorted(child_items, key=self._sort_key),
                self._project_order_ids(parent_id),
            )
            result.extend(child_items)
            seen.update(project["id"] for project in child_items)

        result.extend(
            sorted(
                [
                    project
                    for project in projects
                    if project["id"] not in seen and project["id"] not in folder_child_ids
                ],
                key=self._sort_key,
            )
        )
        return result

    def _sort_sessions_like_native(
        self,
        sessions: list[dict],
        projects: list[dict],
        pins_by_project: dict[str, list[dict]] | None = None,
    ) -> list[dict]:
        by_project: dict[str, list[dict]] = {}
        for session in sessions:
            by_project.setdefault(session["projectID"], []).append(session)
        session_by_id = {session["id"]: session for session in sessions}
        pins_by_project = pins_by_project or {}

        result = []
        project_order = [project["id"] for project in projects]
        project_order.extend(sorted(project_id for project_id in by_project.keys() if project_id not in project_order))
        for project_id in project_order:
            if project_id not in by_project:
                continue
            base = sorted(
                by_project[project_id],
                key=lambda item: -(item.get("createdAtUnixMs") or 0),
            )
            pinned_ids = {
                pin.get("session_id")
                for pin in pins_by_project.get(project_id, [])
                if isinstance(pin.get("session_id"), str)
            }
            pinned = [
                session_by_id[pin["session_id"]]
                for pin in pins_by_project.get(project_id, [])
                if isinstance(pin.get("session_id"), str) and pin["session_id"] in session_by_id
            ]
            regular = [item for item in base if item["id"] not in pinned_ids]
            order_ids = self._session_order_ids(project_id)
            if order_ids:
                session_ids = {item["id"] for item in regular}
                known = [item_id for item_id in order_ids if item_id in session_ids]
                rank = {item_id: index for index, item_id in enumerate(known)}
                rest = [item for item in regular if item["id"] not in rank]
                ordered = sorted(
                    [item for item in regular if item["id"] in rank],
                    key=lambda item: rank[item["id"]],
                )
                regular = rest + ordered
            result.extend(pinned + regular)
        return result

    def _session_title_overrides(self) -> dict[str, str]:
        overrides = {}
        for _, values in self._native_preference_values():
            raw = values.get(SESSION_TITLES_KEY)
            if not isinstance(raw, dict):
                continue
            for session_id, title in raw.items():
                if isinstance(session_id, str) and isinstance(title, str) and title.strip():
                    overrides[session_id] = title.strip()
        return overrides

    def _native_pin_overlay(self) -> dict:
        raw = self._native_preferences_with_key(NATIVE_PINS_KEY).get(NATIVE_PINS_KEY)
        decoded = self._decode_json_preference(raw)
        return decoded if isinstance(decoded, dict) else {}

    def _pinned_records_by_project(self, session_project_ids: dict[str, str]) -> dict[str, list[dict]]:
        state = read_json(self.state_path, {})
        overlay = self._native_pin_overlay()
        removed = {
            item
            for item in overlay.get("removedKeys", overlay.get("removed_keys", [])) or []
            if isinstance(item, str)
        }
        merged: dict[str, dict] = {}
        for items in (state.get("pinned_sessions") or {}).values():
            if isinstance(items, list):
                for item in items:
                    if not isinstance(item, dict):
                        continue
                    key = item.get("key")
                    session_id = item.get("session_id")
                    project_id = item.get("project_id")
                    if not isinstance(key, str) or key in removed:
                        continue
                    if not isinstance(session_id, str) or not isinstance(project_id, str):
                        continue
                    merged[key] = item
        for item in overlay.get("added", []) or []:
            if not isinstance(item, dict):
                continue
            key = item.get("key")
            session_id = item.get("session_id")
            project_id = item.get("project_id")
            if not isinstance(key, str) or key in removed:
                continue
            if not isinstance(session_id, str) or not isinstance(project_id, str):
                continue
            merged[key] = item

        by_project: dict[str, list[dict]] = {}
        for item in merged.values():
            session_id = item["session_id"]
            project_id = item["project_id"]
            if session_project_ids.get(session_id) != project_id:
                continue
            by_project.setdefault(project_id, []).append(item)

        for project_id, pins in list(by_project.items()):
            pins.sort(key=lambda item: (-(item.get("pinned_at") or 0), item.get("key") or ""))
            order_ids = self._pinned_order_ids(project_id)
            if order_ids:
                session_to_pin = {
                    item.get("session_id"): item
                    for item in pins
                    if isinstance(item.get("session_id"), str)
                }
                known = [item_id for item_id in order_ids if item_id in session_to_pin]
                ordered = [session_to_pin[item_id] for item_id in known]
                ordered_ids = set(known)
                rest = [item for item in pins if item.get("session_id") not in ordered_ids]
                by_project[project_id] = rest + ordered
        return by_project

    def _output_lines(self, path: Path) -> list[str]:
        if not path.exists():
            return ["No output captured yet."]
        with path.open("rb") as handle:
            handle.seek(0, os.SEEK_END)
            size = handle.tell()
            handle.seek(max(0, size - 64 * 1024))
            data = handle.read().decode("utf-8", errors="ignore")
        lines = [strip_ansi(line).strip("\n") for line in data.splitlines()]
        return [line for line in lines if line.strip()][-200:] or ["No visible output."]

    def _last_preview(self, path: Path) -> str | None:
        lines = self._output_lines(path)
        return lines[-1][:160] if lines else None


class Handler(BaseHTTPRequestHandler):
    # HTTP/1.1 enables keep-alive (the default HTTP/1.0 closes per request —
    # constant handshake churn under the phone's continuous long-poll). Every
    # response must carry a correct Content-Length; _json does. The read
    # timeout closes idle kept-alive connections so they don't pin threads.
    protocol_version = "HTTP/1.1"
    timeout = 60

    bridge: UnpeelDevBridge

    def _authorized(self) -> bool:
        expected = getattr(Handler, "auth_token", "") or ""
        if not expected:
            return True
        header = self.headers.get("Authorization", "")
        if not header.startswith("Bearer "):
            return False
        return hmac.compare_digest(header[len("Bearer ") :], expected)

    def do_GET(self):
        if not self._authorized():
            return self._json({"error": "unauthorized"}, status=401)
        parsed = urlparse(self.path)
        try:
            if parsed.path == "/bootstrap":
                return self._json(self.bridge.bootstrap())
            if parsed.path == "/viewport":
                query = parse_qs(parsed.query)
                session_id = (query.get("session_id") or [""])[0]
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid session_id"}, status=400)
                rows = int((query.get("rows") or ["28"])[0])
                columns = int((query.get("columns") or ["86"])[0])
                return self._json(self.bridge.viewport(session_id, rows, columns))
            if parsed.path == "/output":
                query = parse_qs(parsed.query)
                session_id = (query.get("session_id") or [""])[0]
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid session_id"}, status=400)
                raw_offset = (query.get("offset") or [None])[0]
                offset = int(raw_offset) if raw_offset not in (None, "") else None
                limit = int((query.get("limit") or ["524288"])[0])
                wait_ms = int((query.get("wait_ms") or ["0"])[0])
                return self._json(
                    self.bridge.output_chunk(session_id, offset, limit, wait_ms)
                )
            if parsed.path == "/metrics":
                query = parse_qs(parsed.query)
                session_id = (query.get("session_id") or [""])[0]
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid session_id"}, status=400)
                return self._json(self.bridge.metrics(session_id))
            if parsed.path == "/transcript-markdown":
                query = parse_qs(parsed.query)
                session_id = (query.get("session_id") or [""])[0]
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid session_id"}, status=400)
                raw_entries = (query.get("entries") or [None])[0]
                entries = int(raw_entries) if raw_entries not in (None, "") else None
                return self._json(self.bridge.transcript_markdown(session_id, entries))
            if parsed.path == "/transcript":
                query = parse_qs(parsed.query)
                session_id = (query.get("session_id") or [""])[0]
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid session_id"}, status=400)
                mode = (query.get("mode") or ["snapshot"])[0]
                include_tools = parse_bool((query.get("include_tools") or [""])[0])
                raw_max_bytes = (query.get("max_bytes") or [None])[0]
                max_bytes = int(raw_max_bytes) if raw_max_bytes not in (None, "") else None
                if mode == "stream":
                    offset = int((query.get("offset") or ["0"])[0])
                    partial = (query.get("partial") or [""])[0]
                    return self._json(
                        self.bridge.transcript_stream(session_id, offset, partial, include_tools, max_bytes)
                    )
                if mode == "history":
                    raw_before_offset = (query.get("before_offset") or [None])[0]
                    before_offset = (
                        int(raw_before_offset) if raw_before_offset not in (None, "") else None
                    )
                    entries = int((query.get("entries") or ["50"])[0])
                    return self._json(
                        self.bridge.transcript_history(
                            session_id,
                            before_offset,
                            entries,
                            include_tools,
                            max_bytes,
                        )
                    )
                entries = int((query.get("entries") or ["50"])[0])
                return self._json(
                    self.bridge.transcript_snapshot(session_id, entries, include_tools, max_bytes)
                )
            return self._json({"error": "not found"}, status=404)
        except Exception as exc:
            return self._json({"error": str(exc)}, status=500)

    def do_POST(self):
        if not self._authorized():
            return self._json({"error": "unauthorized"}, status=401)
        parsed = urlparse(self.path)
        try:
            length = int(self.headers.get("Content-Length") or "0")
            body = self.rfile.read(length)
            # Raw-body route: image bytes, not JSON — must run before the
            # JSON decode below.
            if parsed.path == "/upload":
                return self._json(self.bridge.save_uploaded_image(
                    body, self.headers.get("Content-Type")
                ))
            payload = json.loads(body.decode("utf-8")) if body else {}
            if parsed.path == "/write":
                session_id = payload.get("sessionID") or payload.get("session_id")
                data = payload.get("data")
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid sessionID"}, status=400)
                if not isinstance(data, str):
                    return self._json({"error": "missing data"}, status=400)
                return self._json(self.bridge.write(session_id, data))
            if parsed.path == "/sessions":
                return self._json(self.bridge.create_session(payload))
            if parsed.path == "/resize":
                session_id = payload.get("sessionID") or payload.get("session_id")
                columns = payload.get("columns") or payload.get("cols")
                rows = payload.get("rows")
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid sessionID"}, status=400)
                if columns is None or rows is None:
                    return self._json({"error": "missing columns or rows"}, status=400)
                return self._json(self.bridge.resize(session_id, int(columns), int(rows)))
            if parsed.path == "/resize-desktop":
                session_id = payload.get("sessionID") or payload.get("session_id")
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid sessionID"}, status=400)
                return self._json(self.bridge.resize_desktop(session_id, payload))
            if parsed.path == "/session-organization":
                session_id = payload.get("sessionID") or payload.get("session_id")
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid sessionID"}, status=400)
                return self._json(self.bridge.organize_session(session_id, payload))
            if parsed.path == "/restart-session":
                session_id = payload.get("sessionID") or payload.get("session_id")
                if not is_safe_session_id(session_id):
                    return self._json({"error": "invalid sessionID"}, status=400)
                return self._json(self.bridge.restart_session(session_id))
            return self._json({"error": "not found"}, status=404)
        except Exception as exc:
            return self._json({"error": str(exc)}, status=500)

    def log_message(self, fmt, *args):
        print(f"[dev-bridge] {self.address_string()} {fmt % args}")

    def _json(self, value, status=200):
        body = json.dumps(value).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        # No CORS header: the bridge serves a native client (RemoteMacClient),
        # not browsers. Omitting Access-Control-Allow-Origin stops a random web
        # page from reading transcripts/output cross-origin via fetch.
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=17661)
    parser.add_argument("--root", type=Path, default=Path("~/.unpeel"))
    parser.add_argument(
        "--no-auth",
        action="store_true",
        help="disable bearer-token auth (use only on a trusted, isolated machine)",
    )
    args = parser.parse_args()
    Handler.bridge = UnpeelDevBridge(args.root)
    # Require a bearer token so a random local process (or a web page, now that
    # CORS is off) can't drive terminals / read transcripts. Reuse a stable
    # token from the env if provided (so the phone can be configured once),
    # otherwise mint a fresh one per run and print it.
    if args.no_auth:
        Handler.auth_token = ""
        print("[dev-bridge] WARNING: auth disabled (--no-auth)")
    else:
        token = os.environ.get("UNPEEL_DEV_BRIDGE_TOKEN") or secrets.token_urlsafe(24)
        Handler.auth_token = token
        print(f"[dev-bridge] auth token (send as 'Authorization: Bearer <token>'): {token}")
        # Drop the token where local dev clients can pick it up without
        # copy-paste: the Simulator shares the Mac filesystem, so the iOS
        # app's dev-bridge mode reads this file (via SIMULATOR_HOST_HOME).
        # Owner-only, same trust model as ~/.unpeel/mcp/auth-token.
        token_path = Handler.bridge.root / "dev-bridge-token"
        token_path.write_text(token + "\n")
        os.chmod(token_path, 0o600)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"[dev-bridge] listening on http://127.0.0.1:{args.port}")
    server.serve_forever()


if __name__ == "__main__":
    main()
