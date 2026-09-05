import base64
import importlib.util
import json
import plistlib
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "Tools" / "dev_bridge.py"
SPEC = importlib.util.spec_from_file_location("dev_bridge", MODULE_PATH)
dev_bridge = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(dev_bridge)


class DevBridgeOutputTests(unittest.TestCase):
    def test_tail_alignment_moves_start_to_escape_sequence_boundary(self):
        data = b"before\r\nprefix \x1b[31mRED\x1b[0m tail\r\n"
        escape_index = data.index(b"\x1b[31m")
        desired = escape_index + 2

        aligned = dev_bridge.align_tail_start_in_window(data[:desired], 0, desired)

        self.assertEqual(aligned, escape_index)

    def test_tail_alignment_skips_utf8_continuation_bytes(self):
        data = "before\r\n🙂 emoji tail\r\n".encode()
        emoji_index = data.index("🙂".encode())
        desired = emoji_index + 2

        aligned = dev_bridge.align_tail_start_in_window(data[:desired], 0, desired)

        self.assertEqual(aligned, emoji_index)

    def test_output_chunk_replays_aligned_tail_for_initial_read(self):
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            session_dir = root / "app-sessions" / "s1"
            session_dir.mkdir(parents=True)
            data = b"old line\r\nprefix \x1b[31mRED\x1b[0m tail\r\n"
            output_path = session_dir / "output.bin"
            output_path.write_bytes(data)
            bridge = dev_bridge.UnpeelDevBridge(root)

            escape_index = data.index(b"\x1b[31m")
            limit = len(data) - escape_index - 2
            chunk = bridge.output_chunk("s1", None, limit)
            decoded = base64.b64decode(chunk["dataBase64"])

            self.assertEqual(chunk["offset"], escape_index)
            self.assertEqual(chunk["nextOffset"], len(data))
            self.assertEqual(decoded, data[escape_index:])
            self.assertTrue(chunk["truncated"])

    def test_output_chunk_reads_from_requested_offset(self):
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            session_dir = root / "app-sessions" / "s1"
            session_dir.mkdir(parents=True)
            output_path = session_dir / "output.bin"
            output_path.write_bytes(b"abcdef")
            bridge = dev_bridge.UnpeelDevBridge(root)

            chunk = bridge.output_chunk("s1", 2, 3)
            decoded = base64.b64decode(chunk["dataBase64"])

            self.assertEqual(chunk["offset"], 2)
            self.assertEqual(chunk["nextOffset"], 5)
            self.assertEqual(decoded, b"cde")
            self.assertFalse(chunk["truncated"])

    def test_output_chunk_returns_empty_when_caught_up(self):
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            session_dir = root / "app-sessions" / "s1"
            session_dir.mkdir(parents=True)
            output_path = session_dir / "output.bin"
            output_path.write_bytes(b"abcdef")
            bridge = dev_bridge.UnpeelDevBridge(root)

            chunk = bridge.output_chunk("s1", 6, 3)

            self.assertEqual(chunk["offset"], 6)
            self.assertEqual(chunk["nextOffset"], 6)
            self.assertEqual(chunk["dataBase64"], "")
            self.assertFalse(chunk["truncated"])

    def test_output_chunk_rebases_stale_cursor_at_retention_floor(self):
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            session_dir = root / "app-sessions" / "s1"
            session_dir.mkdir(parents=True)
            floor = 8192
            tail = b"retained line\r\n"
            (session_dir / "output.bin").write_bytes(b"\0" * floor + tail)
            (session_dir / "output-retention.json").write_text(json.dumps({
                "version": 1,
                "retained_from": floor,
            }))
            bridge = dev_bridge.UnpeelDevBridge(root)

            rebased = bridge.output_chunk("s1", 0, len(tail))
            self.assertEqual(rebased["offset"], floor)
            self.assertEqual(base64.b64decode(rebased["dataBase64"]), tail)
            self.assertTrue(rebased["truncated"])

            retained = bridge.output_chunk("s1", floor, len(tail))
            self.assertEqual(retained["offset"], floor)
            self.assertEqual(base64.b64decode(retained["dataBase64"]), tail)
            self.assertFalse(retained["truncated"])

    def test_tail_alignment_does_not_split_escape_intermediate_or_restarted_csi(self):
        intermediate = b"before\r\n\x1b(Bafter"
        escape = intermediate.index(b"\x1b")
        self.assertEqual(
            dev_bridge.align_tail_start_in_window(
                intermediate[: escape + 2], 0, escape + 2
            ),
            escape,
        )

        restarted = b"before\r\n\x1b[1;\x1b[31mred"
        second = restarted.index(b"\x1b", restarted.index(b"\x1b") + 1)
        self.assertEqual(
            dev_bridge.align_tail_start_in_window(
                restarted[: second + 3], 0, second + 3
            ),
            second,
        )


class DevBridgeSidebarProjectTests(unittest.TestCase):
    def test_bootstrap_matches_native_project_overlay_instead_of_manifest_placeholders(self):
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            prefs = root / "Library" / "Preferences"
            prefs.mkdir(parents=True)
            (root / "app-sessions").mkdir()
            (root / "app-state.json").write_text(json.dumps({
                "projects": [
                    {
                        "id": "app-one",
                        "name": "flatsome-theme",
                        "path": "/Users/test/Dev/flatsome-theme",
                        "parent_project_id": None,
                        "sort_order": 1,
                        "is_folder": False,
                    },
                    {
                        "id": "removed-project",
                        "name": "clarity",
                        "path": "/Users/test/Dev/clarity",
                        "parent_project_id": None,
                        "sort_order": 0,
                        "is_folder": False,
                    },
                ],
                "presets": [],
            }))
            native_projects = [
                {
                    "id": "native-b",
                    "name": "unpeel",
                    "path": "/Users/test/Dev/unpeel",
                },
                {
                    "id": "native-worktree",
                    "name": "example/worktree-job",
                    "path": "/Users/test/.unpeel/worktrees/unpeel/example-worktree-job",
                    "parentProjectID": "native-b",
                    "worktreeBranch": "example/worktree-job",
                },
                {
                    "id": "duplicate-path",
                    "name": "duplicate",
                    "path": "/Users/test/Dev/flatsome-theme",
                },
            ]
            with (prefs / "com.unpeel.native.plist").open("wb") as handle:
                plistlib.dump({
                    "unpeel.native.projects": json.dumps(native_projects).encode("utf-8"),
                    "unpeel.native.removedProjects": ["removed-project"],
                    "unpeel.native.projectOrder": ["native-b", "app-one"],
                }, handle)

            self._write_manifest(root, "s-unpeel", "native-b", "codex --dangerously-bypass-approvals-and-sandbox")
            self._write_manifest(root, "s-worktree", "native-worktree", "claude --dangerously-skip-permissions")
            self._write_manifest(root, "s-orphan", "missing-project", "codex --dangerously-bypass-approvals-and-sandbox")
            self._write_manifest(root, "s-removed", "removed-project", "codex --dangerously-bypass-approvals-and-sandbox")

            bridge = dev_bridge.UnpeelDevBridge(root)
            bridge.preferences_dir = prefs
            snapshot = bridge.bootstrap()

            project_ids = [project["id"] for project in snapshot["projects"]]
            self.assertEqual(project_ids[:3], ["native-b", "app-one", "native-worktree"])
            self.assertNotIn("removed-project", project_ids)
            self.assertNotIn("duplicate-path", project_ids)
            self.assertNotIn("missing-project", project_ids)

            unpeel_project = next(project for project in snapshot["projects"] if project["id"] == "native-b")
            self.assertEqual(unpeel_project["name"], "unpeel")
            self.assertEqual(unpeel_project["path"], "/Users/test/Dev/unpeel")

            worktree_project = next(project for project in snapshot["projects"] if project["id"] == "native-worktree")
            self.assertEqual(worktree_project["parentProjectID"], "native-b")
            self.assertEqual(worktree_project["worktreeBranch"], "example/worktree-job")

            session_ids = {session["id"] for session in snapshot["sessions"]}
            self.assertEqual(session_ids, {"s-unpeel", "s-worktree"})

    def test_bootstrap_merges_native_preset_overlay(self):
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            prefs = root / "Library" / "Preferences"
            prefs.mkdir(parents=True)
            (root / "app-sessions").mkdir()
            (root / "app-state.json").write_text(json.dumps({
                "projects": [],
                "presets": [
                    {
                        "id": "claude",
                        "label": "Claude old",
                        "command": "claude",
                        "enabled": True,
                        "quick_launch": True,
                    },
                    {
                        "id": "codex",
                        "label": "Codex",
                        "command": "codex",
                        "enabled": True,
                        "quick_launch": True,
                    },
                ],
            }))
            overlay = {
                "removedIDs": ["codex"],
                "edited": [
                    {
                        "id": "claude",
                        "label": "Claude edited",
                        "command": "claude --dangerously-skip-permissions",
                        "enabled": True,
                        "quick_launch": True,
                    }
                ],
                "added": [
                    {
                        "id": "custom",
                        "label": "Custom shell",
                        "command": "echo hi",
                        "enabled": True,
                        "quick_launch": True,
                    }
                ],
            }
            with (prefs / "com.unpeel.native.plist").open("wb") as handle:
                plistlib.dump({"unpeel.native.presets": json.dumps(overlay).encode("utf-8")}, handle)

            bridge = dev_bridge.UnpeelDevBridge(root)
            bridge.preferences_dir = prefs
            snapshot = bridge.bootstrap()

            presets = {preset["id"]: preset for preset in snapshot["presets"]}
            self.assertEqual(set(presets), {"claude", "custom"})
            self.assertEqual(presets["claude"]["label"], "Claude edited")
            self.assertFalse(presets["custom"]["quickLaunch"])

    def test_bootstrap_merges_native_pin_overlay_and_orders_pins_above_regular_sessions(self):
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            prefs = root / "Library" / "Preferences"
            prefs.mkdir(parents=True)
            (root / "app-sessions").mkdir()
            (root / "app-state.json").write_text(json.dumps({
                "projects": [
                    {
                        "id": "project-unpeel",
                        "name": "unpeel",
                        "path": "/Users/test/Dev/unpeel",
                        "parent_project_id": None,
                        "sort_order": 0,
                        "is_folder": False,
                    },
                ],
                "presets": [],
                "pinned_sessions": {
                    "project-unpeel": [
                        {
                            "key": "session:state-pin",
                            "project_id": "project-unpeel",
                            "session_id": "state-pin",
                            "pinned_at": 30,
                        },
                        {
                            "key": "session:removed-pin",
                            "project_id": "project-unpeel",
                            "session_id": "removed-pin",
                            "pinned_at": 50,
                        },
                    ],
                },
            }))
            overlay = {
                "removedKeys": ["session:removed-pin"],
                "added": [
                    {
                        "key": "session:native-pin",
                        "project_id": "project-unpeel",
                        "session_id": "native-pin",
                        "pinned_at": 20,
                    },
                ],
            }
            with (prefs / "com.unpeel.native.plist").open("wb") as handle:
                plistlib.dump({
                    "unpeel.sidebar.pins": json.dumps(overlay).encode("utf-8"),
                    "unpeel.native.pinnedOrder.project-unpeel": [
                        "native-pin",
                        "state-pin",
                    ],
                }, handle)

            self._write_manifest(root, "regular", "project-unpeel", "codex", created_at=4000)
            self._write_manifest(root, "native-pin", "project-unpeel", "codex", created_at=3000)
            self._write_manifest(root, "state-pin", "project-unpeel", "codex", created_at=2000)
            self._write_manifest(root, "removed-pin", "project-unpeel", "codex", created_at=1000)

            bridge = dev_bridge.UnpeelDevBridge(root)
            bridge.preferences_dir = prefs
            snapshot = bridge.bootstrap()

            sessions = snapshot["sessions"]
            self.assertEqual([session["id"] for session in sessions], [
                "native-pin",
                "state-pin",
                "regular",
                "removed-pin",
            ])
            self.assertTrue(sessions[0]["pinned"])
            self.assertTrue(sessions[1]["pinned"])
            self.assertFalse(sessions[2]["pinned"])
            self.assertFalse(sessions[3]["pinned"])

    def test_resize_clamps_grid_and_sends_control_command(self):
        with tempfile.TemporaryDirectory() as raw_root:
            bridge = dev_bridge.UnpeelDevBridge(Path(raw_root))
            sent = []

            def fake_control(session_id, command):
                sent.append((session_id, command))
                return {"ok": True}

            bridge._control_command = fake_control

            result = bridge.resize("s1", 999, 0)

            self.assertTrue(result["ok"])
            self.assertEqual(sent, [
                ("s1", {"type": "resize", "cols": 300, "rows": 2}),
            ])

    def _write_manifest(self, root: Path, session_id: str, project_id: str, command: str, created_at: int = 1000):
        session_dir = root / "app-sessions" / session_id
        session_dir.mkdir(parents=True)
        (session_dir / "output.bin").write_text(f"{session_id}\n")
        (session_dir / "manifest.json").write_text(json.dumps({
            "state": "running",
            "session": {
                "id": session_id,
                "project_id": project_id,
                "label": session_id,
                "custom_title": False,
                "command": command,
                "created_at": created_at,
                "tag_id": None,
                "worktree_path": None,
                "worktree_branch": None,
            },
        }))


if __name__ == "__main__":
    unittest.main()
