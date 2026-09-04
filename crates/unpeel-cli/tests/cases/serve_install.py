"""`unpeel serve install|uninstall|status` — per-user service packaging.

Drives both service-manager flavors through fake `launchctl`/`systemctl`
shims on PATH with HOME redirected into the fixture, so the case never
registers (or even touches) a real system service. The shims record every
invocation and keep a tiny loaded/active state so idempotency and status
are observable.
"""

import fcntl
import json
import os
import stat
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, run  # noqa: E402


LAUNCHCTL_SHIM = """#!/bin/sh
echo "launchctl $@" >> "$SHIM_LOG"
case "$1" in
  bootstrap) touch "$SHIM_STATE/loaded" ;;
  bootout)
    if [ ! -e "$SHIM_STATE/loaded" ]; then exit 3; fi
    rm -f "$SHIM_STATE/loaded"
    ;;
  print) [ -e "$SHIM_STATE/loaded" ] || exit 113 ;;
esac
exit 0
"""

SYSTEMCTL_SHIM = """#!/bin/sh
echo "systemctl $@" >> "$SHIM_LOG"
case "$2" in
  enable|restart) touch "$SHIM_STATE/active" ;;
  disable) rm -f "$SHIM_STATE/active" ;;
  is-active)
    if [ -e "$SHIM_STATE/active" ]; then echo active; exit 0
    else echo inactive; exit 3; fi
    ;;
esac
exit 0
"""


def body(case):
    home = case.home
    shim_dir = home.path("shim-bin")
    state_dir = home.path("shim-state")
    os.makedirs(shim_dir, exist_ok=True)
    os.makedirs(state_dir, exist_ok=True)
    log_path = home.path("shim.log")
    for name, script in (("launchctl", LAUNCHCTL_SHIM), ("systemctl", SYSTEMCTL_SHIM)):
        path = os.path.join(shim_dir, name)
        with open(path, "w") as handle:
            handle.write(script)
        os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    def shim_log():
        try:
            with open(log_path) as handle:
                return handle.read()
        except FileNotFoundError:
            return ""

    def clear_log():
        with open(log_path, "w"):
            pass

    def cli(args, manager, unpeel_home=None, timeout=30):
        env = dict(
            os.environ,
            HOME=home.root,
            PATH=f"{shim_dir}:{os.environ.get('PATH', '')}",
            UNPEEL_TEST="1",
            UNPEEL_SERVICE_MANAGER=manager,
            SHIM_LOG=log_path,
            SHIM_STATE=state_dir,
            XDG_CONFIG_HOME=os.path.join(home.root, ".config"),
        )
        env.pop("UNPEEL_HOME", None)
        if unpeel_home is not None:
            env["UNPEEL_HOME"] = unpeel_home
        return subprocess.run(
            [BINARY, *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
            cwd=CRATES,
        )

    # Data that install/uninstall must never touch.
    real_dir = os.path.join(home.root, ".unpeel")
    os.makedirs(real_dir, exist_ok=True)
    sentinel = os.path.join(real_dir, "app-state.json")
    with open(sentinel, "w") as handle:
        json.dump({"projects": []}, handle)

    # ── launchd (macOS) flavor ────────────────────────────────────────────
    plist = os.path.join(
        home.root, "Library", "LaunchAgents", "com.unpeel.serve.plist"
    )
    installed = cli(["serve", "install"], "launchd")
    with open(plist) as handle:
        plist_body = handle.read() if os.path.exists(plist) else ""
    case.check(
        "launchd install writes the LaunchAgent with the resolved binary",
        installed.returncode == 0
        and os.path.exists(plist)
        and (
            f"<string>{os.path.realpath(BINARY)}</string>" in plist_body
            or f"<string>{BINARY}</string>" in plist_body
        ),
        installed.stdout[:300] + installed.stderr[:300] + plist_body[:200],
    )
    case.check(
        "launchd install bootstraps the per-user gui domain",
        f"launchctl bootstrap gui/{os.getuid()} {plist}" in shim_log(),
        shim_log()[-400:],
    )
    case.check(
        "install output carries the headless-Mac auto-login note",
        "automatic login" in installed.stdout,
        installed.stdout[:400],
    )

    clear_log()
    again = cli(["serve", "install"], "launchd")
    case.check(
        "re-running launchd install is idempotent (bootout, then bootstrap)",
        again.returncode == 0
        and "launchctl bootout" in shim_log()
        and "launchctl bootstrap" in shim_log(),
        shim_log()[-400:],
    )

    status = cli(["serve", "status"], "launchd")
    case.check(
        "status reports the loaded unit and a not-running Host service (exit 1)",
        status.returncode == 1
        and "installed" in status.stdout
        and "loaded" in status.stdout
        and "not running" in status.stdout,
        status.stdout[:400],
    )

    removed = cli(["serve", "uninstall"], "launchd")
    case.check(
        "launchd uninstall boots the job out and removes only the unit",
        removed.returncode == 0
        and not os.path.exists(plist)
        and "launchctl bootout" in shim_log()
        and os.path.exists(sentinel),
        removed.stdout[:300] + shim_log()[-300:],
    )
    gone = cli(["serve", "status"], "launchd")
    case.check(
        "status after uninstall reports not installed / not loaded",
        gone.returncode == 1
        and "not installed" in gone.stdout
        and "not loaded" in gone.stdout,
        gone.stdout[:300],
    )

    # ── systemd (Linux) flavor ────────────────────────────────────────────
    clear_log()
    unit = os.path.join(
        home.root, ".config", "systemd", "user", "unpeel-serve.service"
    )
    sysd = cli(["serve", "install"], "systemd")
    with open(unit) as handle:
        unit_body = handle.read() if os.path.exists(unit) else ""
    case.check(
        "systemd install writes the user unit and enables + starts it",
        sysd.returncode == 0
        and "ExecStart=" in unit_body
        and " serve" in unit_body
        and "systemctl --user daemon-reload" in shim_log()
        and "systemctl --user enable unpeel-serve.service" in shim_log()
        and "systemctl --user restart unpeel-serve.service" in shim_log(),
        sysd.stdout[:300] + shim_log()[-400:] + unit_body[:200],
    )
    case.check(
        "install output carries the enable-linger note",
        "enable-linger" in sysd.stdout,
        sysd.stdout[:300],
    )

    # ── desktop-session (graphical) variant ──────────────────────────────
    clear_log()
    graphical = cli(["serve", "install", "--graphical"], "systemd")
    with open(unit) as handle:
        graphical_body = handle.read() if os.path.exists(unit) else ""
    case.check(
        "systemd install --graphical writes the desktop-session unit and enables it",
        graphical.returncode == 0
        and "PartOf=graphical-session.target" in graphical_body
        and "WantedBy=graphical-session.target" in graphical_body
        and "WantedBy=default.target" not in graphical_body
        and f"ExecStart=" in graphical_body
        and "systemctl --user enable unpeel-serve.service" in shim_log()
        and "graphical-session.target" in graphical.stdout,
        graphical.stdout[:300] + shim_log()[-400:] + graphical_body[:300],
    )
    # The shim's coarse state says graphical-session.target is active (the
    # plain install above touched it), so the unit is started now.
    case.check(
        "a graphical install consults graphical-session.target and starts the unit while it is active",
        "systemctl --user is-active graphical-session.target" in shim_log()
        and "systemctl --user restart unpeel-serve.service" in shim_log(),
        shim_log()[-400:],
    )
    clear_log()
    os.remove(os.path.join(state_dir, "active"))
    graphical_idle = cli(["serve", "install", "--graphical"], "systemd")
    case.check(
        "with no desktop session up, the graphical unit is enabled but not started (the target will)",
        graphical_idle.returncode == 0
        and "systemctl --user enable unpeel-serve.service" in shim_log()
        and "systemctl --user is-active graphical-session.target" in shim_log()
        and "systemctl --user restart unpeel-serve.service" not in shim_log(),
        shim_log()[-400:],
    )
    graphical_status = cli(["serve", "status"], "systemd")
    case.check(
        "status names the desktop-session variant, the target state, and the visible session",
        graphical_status.returncode == 1
        and "unit variant: desktop session" in graphical_status.stdout
        and "graphical-session.target:" in graphical_status.stdout
        and "desktop session:" in graphical_status.stdout,
        graphical_status.stdout[:500],
    )
    launchd_graphical = cli(["serve", "install", "--graphical"], "launchd")
    case.check(
        "launchd refuses --graphical (the app owns the desktop daemon on macOS)",
        launchd_graphical.returncode != 0
        and "--graphical is a Linux" in launchd_graphical.stderr,
        launchd_graphical.stderr[:300],
    )
    # Back to the plain unit before the scoped checks below.
    clear_log()
    cli(["serve", "install"], "systemd")

    # ── workspace-scoped units ────────────────────────────────────────────
    added = cli(["workspaces", "add", "teama"], "systemd")
    workspace_home = os.path.join(real_dir, "profiles", "teama")
    case.check(
        "a workspace can be registered for a scoped unit",
        added.returncode == 0 and os.path.isdir(workspace_home),
        added.stdout[:200] + added.stderr[:200],
    )
    clear_log()
    scoped_unit = os.path.join(
        home.root, ".config", "systemd", "user", "unpeel-serve-teama.service"
    )
    scoped = cli(["serve", "install"], "systemd", unpeel_home=workspace_home)
    scoped_body = ""
    if os.path.exists(scoped_unit):
        with open(scoped_unit) as handle:
            scoped_body = handle.read()
    case.check(
        "a registered UNPEEL_HOME installs a scoped single-workspace unit",
        scoped.returncode == 0
        and "--workspace teama serve" in scoped_body
        and "systemctl --user enable unpeel-serve-teama.service" in shim_log(),
        scoped.stdout[:300] + scoped.stderr[:200] + scoped_body[:200],
    )

    # A live scoped worker (simulated by holding its lease) flips status to 0.
    lock_path = os.path.join(workspace_home, "serve.lock")
    with open(lock_path, "a+") as lease:
        fcntl.flock(lease, fcntl.LOCK_EX)
        live = cli(["serve", "status"], "systemd", unpeel_home=workspace_home)
        fcntl.flock(lease, fcntl.LOCK_UN)
    case.check(
        "scoped status reads the existing workspace serve lease (exit 0 while held)",
        live.returncode == 0 and "running" in live.stdout,
        live.stdout[:300],
    )
    scoped_removed = cli(["serve", "uninstall"], "systemd", unpeel_home=workspace_home)
    case.check(
        "scoped uninstall removes only its unit and leaves workspace data alone",
        scoped_removed.returncode == 0
        and not os.path.exists(scoped_unit)
        and os.path.isdir(workspace_home)
        and os.path.exists(sentinel)
        and os.path.exists(unit),
        scoped_removed.stdout[:300],
    )

    unregistered = cli(
        ["serve", "install"], "systemd", unpeel_home=home.path("not-registered")
    )
    case.check(
        "an unregistered UNPEEL_HOME is refused instead of minting a unit",
        unregistered.returncode != 0
        and "not a registered workspace" in unregistered.stderr,
        unregistered.stderr[:300],
    )

    launchd_scoped = cli(["serve", "install"], "launchd", unpeel_home=workspace_home)
    scoped_plist = os.path.join(
        home.root, "Library", "LaunchAgents", "com.unpeel.serve.teama.plist"
    )
    scoped_plist_body = ""
    if os.path.exists(scoped_plist):
        with open(scoped_plist) as handle:
            scoped_plist_body = handle.read()
    case.check(
        "a scoped LaunchAgent carries the workspace label and arguments",
        launchd_scoped.returncode == 0
        and "<string>com.unpeel.serve.teama</string>" in scoped_plist_body
        and "<string>--workspace</string>" in scoped_plist_body
        and "<string>teama</string>" in scoped_plist_body,
        scoped_plist_body[:400],
    )
    cli(["serve", "uninstall"], "launchd", unpeel_home=workspace_home)


run("serve_install", body)
