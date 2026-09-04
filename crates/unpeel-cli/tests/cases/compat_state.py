"""Upgrade safety for people already running Unpeel.

The shared files are a contract across app versions AND across frontends. A
user who installs a newer `unpeel` beside an older desktop app (or updates
the app while sessions from the previous version are on disk) must lose
nothing. Each check here corresponds to a way that could go wrong.
"""

import sys, os, json

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, run_cli, mobile_request  # noqa: E402


def body(case):
    home = case.home

    # ── a state file as a shipped desktop writes it, including keys the
    #    Rust side has never modelled ──
    desktop_state = {
        "projects": [{"id": "p", "name": "unpeel", "path": "/tmp"}],
        "active_project_id": "p",
        "presets": [
            {"id": "c", "label": "claude", "command": "claude", "project_id": None,
             "enabled": True, "quick_launch": True}
        ],
        "active_tabs": {"p": "s-legacy"},
        "pinned_sessions": {
            "p": [
                {"key": "session:s-legacy", "project_id": "p",
                 "session_id": "s-legacy", "pinned_at": 1}
            ]
        },
        "theme": "midnight",
        "browser_default_access": "on",
        "a_key_from_a_future_version": {"nested": [1, 2, 3]},
    }
    with open(home.path("app-state.json"), "w") as handle:
        json.dump(desktop_state, handle, indent=2)

    # ── a session dir as an OLD host wrote it: none of the fields added
    #    since (pid_started_at, host_protocol_version, mcp flags…) ──
    legacy_dir = home.path("app-sessions", "s-legacy")
    os.makedirs(legacy_dir, exist_ok=True)
    with open(os.path.join(legacy_dir, "manifest.json"), "w") as handle:
        json.dump(
            {
                "session": {
                    "id": "s-legacy",
                    "project_id": "p",
                    "label": "legacy session",
                    "command": "claude",
                    "created_at": 1_700_000_000_000,
                },
                "cwd": "/tmp",
                "state": "exited",
                "pid": None,
                "exit_code": 0,
            },
            handle,
        )
    with open(os.path.join(legacy_dir, "output.bin"), "w") as handle:
        handle.write("legacy output\r\n")

    # ── a session dir from a FUTURE version: fields we don't know yet ──
    future_dir = home.path("app-sessions", "s-future")
    os.makedirs(future_dir, exist_ok=True)
    with open(os.path.join(future_dir, "manifest.json"), "w") as handle:
        json.dump(
            {
                "session": {
                    "id": "s-future",
                    "project_id": "p",
                    "label": "future session",
                    "command": "claude",
                    "created_at": 1_800_000_000_000,
                    "something_new": "value",
                },
                "cwd": "/tmp",
                "state": "exited",
                "pid": None,
                "exit_code": 0,
                "host_protocol_version": 99,
                "a_field_from_later": True,
            },
            handle,
        )
    with open(os.path.join(future_dir, "output.bin"), "w") as handle:
        handle.write("future output\r\n")

    # ── the Host service and the CLI read and write that document ──
    token = home.pair_device()
    mobile = home.reserve_mobile_port()
    service = case.serve()
    service.ready()
    listed = run_cli(home, ["ls"]).stdout
    case.check("a legacy session still lists", "legacy session" in listed, listed[:200])
    case.check("a future session still lists", "future session" in listed, listed[:200])

    def bootstrap():
        status, payload = mobile_request(mobile, "/mobile/bootstrap", token)
        return json.dumps(payload) if status == 200 else ""

    published = service.wait_for(lambda: "legacy session" in bootstrap(), timeout=10)
    case.check(
        "the Host publishes legacy and future sessions under their project",
        published and "future session" in bootstrap() and "unpeel" in bootstrap(),
        bootstrap()[:300],
    )

    # Writes from this version: a rename marker, a preset edit, and a
    # project add — the same read-modify-write paths the app exercises.
    home.marker("s-legacy", "title.json", {"title": "renamed by cli"})
    run_cli(home, ["presets", "add", "temp", "echo temp"], expect_ok=True)
    run_cli(home, ["presets", "remove", "temp"], expect_ok=True)
    service.read_for(1.5)
    service.close()

    after = home.state()
    case.check("unmodelled keys survive a Host run",
               after.get("a_key_from_a_future_version", {}).get("nested") == [1, 2, 3],
               str(after.keys()))
    case.check("theme survives", after.get("theme") == "midnight")
    case.check(
        "pins survive",
        after.get("pinned_sessions") == desktop_state["pinned_sessions"],
        str(after.get("pinned_sessions")),
    )
    case.check("active tabs survive", after.get("active_tabs") == {"p": "s-legacy"})
    case.check("presets survive", after.get("presets") == desktop_state["presets"])
    case.check("the project list survives", after.get("projects") == desktop_state["projects"])

    # ── the CLI must be just as careful ──
    run_cli(home, ["add", home.path("a-folder")]) if os.makedirs(
        home.path("a-folder"), exist_ok=True
    ) is None else None
    after_cli = home.state()
    case.check(
        "the CLI preserves unmodelled keys too",
        after_cli.get("theme") == "midnight"
        and after_cli.get("a_key_from_a_future_version") is not None,
        str(after_cli.keys()),
    )

    # ── a field whose SHAPE we don't recognise must not cost the document ──
    # `pinned_sessions` has already changed shape once in this product's
    # life. If a future app version changes another field, an older `unpeel`
    # must still find the user's projects rather than falling back to
    # `cwd:` buckets and looking like their setup vanished.
    future = dict(desktop_state)
    future["pinned_sessions"] = "a shape from the future"
    future["projects"] = [{"id": "p", "name": "unpeel", "path": "/tmp"}]
    with open(home.path("app-state.json"), "w") as handle:
        json.dump(future, handle, indent=2)
    survivor = run_cli(home, ["projects", "list"])
    listed = run_cli(home, ["ls"])
    case.check(
        "an unreadable field costs only that field",
        survivor.returncode == 0
        and "unpeel" in survivor.stdout
        and listed.returncode == 0
        and "future session" in listed.stdout,
        survivor.stdout[:200] + survivor.stderr[:200] + listed.stderr[:200],
    )

    # ── a corrupt state file must never be silently replaced ──
    with open(home.path("app-state.json"), "w") as handle:
        handle.write("{ half a write")
    result = run_cli(home, ["add", home.path("another-folder")])
    with open(home.path("app-state.json")) as handle:
        preserved = handle.read()
    case.check(
        "a corrupt state file is not overwritten",
        preserved == "{ half a write",
        preserved[:80],
    )
    case.check(
        "and the failure is reported",
        result.returncode != 0,
        f"rc={result.returncode}",
    )
    # The Host never "repairs" a corrupt document either: it must not seed
    # or rewrite it on start.
    corrupt_host = case.serve()
    corrupt_host.ready()
    corrupt_host.read_for(1.0)
    corrupt_host.close()
    with open(home.path("app-state.json")) as handle:
        still_preserved = handle.read()
    case.check(
        "a corrupt state file survives a Host start untouched",
        still_preserved == "{ half a write",
        still_preserved[:80],
    )


run("compat_state", body)
