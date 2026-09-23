"""A Session whose disk fills up ends cleanly instead of sticking forever.

When the output journal write fails (ENOSPC), the Host ends the Session. It
must terminate and reap its child — a zombie still answers `kill(pid, 0)`,
so an unreaped child kept the manifest `running` with no socket behind it,
unremovable until the Host exited — and keep retrying the exited manifest,
whose write fails for the same reason, until space returns. `unpeel rm`
then removes the Session. Reproduces the operator's 2026-09-21 incident
(three Sessions stuck `running` with `<defunct>` children after "No space
left on device"). The home lives on a 4 MiB volume: an HFS+ disk image on
macOS, a tmpfs mount on Linux when passwordless sudo is available;
otherwise the case is SKIPPED with a NOTE."""

import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import (  # noqa: E402
    BINARY,
    CRATES,
    Home,
    leftover_host_processes,
    run,
    run_cli,
    wait_running,
)

HOST_BIN = os.path.join(CRATES, "target", "debug", "unpeel-host")
VOLUME_MB = 4


class Volume:
    """A tiny filesystem mounted at a short path; detached on close."""

    def __init__(self, mountpoint, image):
        self.mountpoint = mountpoint
        self.image = image
        self.device = None
        self.linux = sys.platform.startswith("linux")

    def attach(self):
        if self.linux:
            if subprocess.run(["sudo", "-n", "true"], capture_output=True).returncode != 0:
                return "no passwordless sudo for a tmpfs mount"
            os.makedirs(self.mountpoint, exist_ok=True)
            opts = f"size={VOLUME_MB}m,mode=0700,uid={os.getuid()},gid={os.getgid()}"
            mounted = subprocess.run(
                ["sudo", "-n", "mount", "-t", "tmpfs", "-o", opts, "tmpfs", self.mountpoint],
                capture_output=True, text=True,
            )
            if mounted.returncode != 0:
                return f"tmpfs mount failed: {mounted.stderr.strip()[:120]}"
            self.device = "tmpfs"
            return None
        if sys.platform != "darwin":
            return f"no tiny-volume recipe for {sys.platform}"
        created = subprocess.run(
            ["hdiutil", "create", "-quiet", "-size", f"{VOLUME_MB}m", "-fs", "HFS+",
             "-volname", "umx", "-ov", self.image],
            capture_output=True, text=True,
        )
        if created.returncode != 0:
            return f"hdiutil create failed: {created.stderr.strip()[:120]}"
        attached = subprocess.run(
            ["hdiutil", "attach", "-nobrowse", "-noverify", "-noautoopen",
             "-mountpoint", self.mountpoint, self.image],
            capture_output=True, text=True,
        )
        if attached.returncode != 0:
            return f"hdiutil attach failed: {attached.stderr.strip()[:120]}"
        for line in attached.stdout.splitlines():
            if line.startswith("/dev/"):
                self.device = line.split()[0]
        return None if self.device else "hdiutil attach reported no device"

    def close(self):
        if not self.device:
            return
        for _ in range(10):
            if self.linux:
                done = subprocess.run(["sudo", "-n", "umount", self.mountpoint], capture_output=True)
            else:
                done = subprocess.run(["hdiutil", "detach", "-quiet", self.device], capture_output=True)
            if done.returncode == 0:
                break
            time.sleep(0.5)
        if not self.linux:
            try:
                os.remove(self.image)
            except OSError:
                pass


class Cleanup:
    """Kill the case's core and hosts on the volume before it is detached."""

    def __init__(self, home, core):
        self.home = home
        self.core = core

    def close(self):
        self.home.cleanup()
        if self.core.poll() is None:
            self.core.kill()
            self.core.wait(timeout=5)
        for pid, _cmd in leftover_host_processes(self.home.root):
            try:
                os.kill(pid, 9)
            except (ProcessLookupError, PermissionError):
                pass


def launch_env(home):
    env = dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1")
    env.pop("UNPEEL_PTY_CORE", None)
    return env


def start_core(home):
    core = subprocess.Popen(
        [HOST_BIN, "__pty_core__"],
        env=launch_env(home),
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    end = time.monotonic() + 10
    while time.monotonic() < end and not os.path.exists(home.path("pty-core.sock")):
        time.sleep(0.05)
    return core


def fill(path):
    """Write until ENOSPC, down to the last byte; returns the bytes it took."""
    written = 0
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        for chunk in (65536, 4096, 512, 1):
            try:
                while True:
                    written += os.write(fd, b"x" * chunk)
            except OSError:
                continue
    finally:
        try:
            os.close(fd)
        except OSError:
            pass
    return written


def process_state(pid):
    """`ps` STAT column for pid, '' once it is reaped and gone."""
    out = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
    return out.stdout.strip()


def wait_until(predicate, timeout, poll=0.2):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(poll)
    return False


def body(case):
    mountpoint = case.home.root + "-fd"
    volume = case.track(Volume(mountpoint, case.home.path("fd.dmg")))
    reason = volume.attach()
    if reason:
        case.note(f"full_disk SKIPPED: {reason}")
        case.check("full_disk skipped: no tiny volume available (see NOTE)", True)
        return

    home = Home(mountpoint)
    home.project("p", "unpeel", "/tmp")
    # The child waits for the go-file (on the case's ordinary home — the
    # full volume could not hold a new file), then floods its terminal.
    go = case.home.path("go")
    home.preset(
        label="flood",
        command=f"sh -c 'until [ -e {go} ]; do sleep 0.2; done; yes | head -c 2000000; sleep 120'",
    )
    core = start_core(home)
    case.track(Cleanup(home, core))
    case.check("the core comes up on the tiny volume", os.path.exists(home.path("pty-core.sock")))

    started = subprocess.run(
        [BINARY, "new", "--preset", "flood", "--project", "p"],
        capture_output=True, text=True, timeout=45, env=launch_env(home), cwd=CRATES,
    )
    session_id = next(
        (t for t in started.stdout.split() if len(t) == 36 and t.count("-") == 4), ""
    )
    case.check("new starts a Session on the volume", started.returncode == 0 and bool(session_id),
               started.stdout[:160] + started.stderr[:200])
    case.check("its host comes up", wait_running(home, session_id), "never running")
    manifest = home.manifests().get(session_id) or {}
    child = manifest.get("pid")
    case.check("the manifest records the child pid", bool(child) and manifest.get("host_pid") == core.pid,
               str({k: manifest.get(k) for k in ("pid", "host_pid", "state")}))
    if not child:
        return

    filler = home.path("fill")
    filled = fill(filler)
    def volume_is_full():
        try:
            fd = os.open(home.path("fill2"), os.O_WRONLY | os.O_CREAT, 0o600)
        except OSError:
            return True
        try:
            os.write(fd, b"x" * 4096)
        except OSError:
            return True
        finally:
            os.close(fd)
        return False

    case.check("the volume is full", filled > 0 and volume_is_full(), str(filled))
    with open(go, "w") as handle:
        handle.write("go\n")

    # The journal write fails on the flood; the Host ends the Session and
    # reaps its child. A zombie would show STAT 'Z' here forever. Sample the
    # whole way so a failure shows the sequence, not just the end state.
    timeline = []
    sock = home.path("app-sessions", session_id, "session.sock")
    began = time.monotonic()
    while time.monotonic() - began < 40:
        sample = (
            round(time.monotonic() - began, 1),
            (home.manifests().get(session_id) or {}).get("state"),
            process_state(child),
            os.path.exists(sock),
        )
        if not timeline or sample[1:] != timeline[-1][1:]:
            timeline.append(sample)
        if sample[2] == "":
            break
        time.sleep(0.2)
    case.check("the child is terminated and reaped, not left a zombie", process_state(child) == "",
               f"pid {child} timeline (t, state, STAT, sock)={timeline}")
    case.check("the core no longer hosts the Session", wait_until(lambda: not os.path.exists(sock), 10))
    state_while_full = (home.manifests().get(session_id) or {}).get("state")
    case.check("the manifest is still running while the disk is full (the write cannot land)",
               state_while_full == "running", f"{state_while_full} timeline={timeline}")

    # Space returns; the retried write lands and the Session reads exited.
    for name in (filler, home.path("fill2")):
        try:
            os.remove(name)
        except OSError:
            pass
    exited = wait_until(lambda: (home.manifests().get(session_id) or {}).get("state") == "exited", 20)
    case.check("the exited manifest lands once space returns", exited,
               str(home.manifests().get(session_id, {}).get("state")))
    trace = ""
    try:
        with open(home.path("hooks", "trace.log")) as handle:
            trace = handle.read()
    except OSError:
        pass
    case.check("the trace names the retried publish", "published the exited manifest on attempt" in trace,
               trace[-400:])
    case.check("the trace carries the OS reason", "No space left on device" in trace, trace[-400:])

    removed = run_cli(home, ["rm", session_id], timeout=45)
    case.check("rm removes the Session", removed.returncode == 0
               and not os.path.exists(home.path("app-sessions", session_id)), removed.stderr[:200])
    case.check("no stray host under the volume", leftover_host_processes(home.root) == [],
               str(leftover_host_processes(home.root)))


run("full_disk", body)
