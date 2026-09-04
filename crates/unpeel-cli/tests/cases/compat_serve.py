"""Upgrade guard across CLI/Host versions: the SHIPPED 0.4.3 `unpeel` beside
this tree's binaries, over one shared home.

People update the app and the CLI independently, and a headless box may run
a `unpeel serve` that is older or newer than the `unpeel` a script calls.
Both directions must leave every unmodelled key in the shared files intact,
list each other's sessions, and never refuse to start. The pinned archive is
fetched once into a cache (sha256 pinned in this file), or taken from
`UNPEEL_MATRIX_COMPAT_ARCHIVE=<path>` for offline/CI runs; when neither is
available the case SKIPS with a NOTE line — never a silent pass.
"""

import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, run, run_cli  # noqa: E402

PINNED_VERSION = "0.4.3"
PINNED_CHANNEL = "beta"
# sha256 of unpeel-0.4.3-<target>.tar.gz as recorded by release-cli.mjs in
# the channel's latest.json at publish time (the versioned .sha256 sidecars
# were introduced after 0.4.3).
PINNED_SHA256 = {
    "macos-universal": "a69b91c296a7cb828a4bb06906ae6e916c781fc9105d83e26b30b030e5c9ae93",
    "linux-x86_64": "be180dcfecc51f0b3d4528eab5f0d32d9c0c19779d44f6c01340e9e5ca48b36c",
    "linux-aarch64": "c6a6b4392755b90a687798ba88a67186e9bc38293c21ba7a17495e55b155444a",
}


def archive_target():
    system = platform.system()
    if system == "Darwin":
        return "macos-universal"
    machine = platform.machine()
    if system == "Linux" and machine in ("arm64", "aarch64"):
        return "linux-aarch64"
    if system == "Linux" and machine in ("x86_64", "amd64"):
        return "linux-x86_64"
    return None


def cache_dir():
    if platform.system() == "Darwin":
        base = os.path.expanduser("~/Library/Caches")
    else:
        base = os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache")
    return os.path.join(base, "unpeel-matrix", PINNED_VERSION)


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def resolve_archive(note):
    """Path to a verified archive, or None with the reason noted."""
    target = archive_target()
    override = os.environ.get("UNPEEL_MATRIX_COMPAT_ARCHIVE")
    if override:
        if not os.path.isfile(override):
            note(f"compat_serve SKIPPED: UNPEEL_MATRIX_COMPAT_ARCHIVE={override} is not a file")
            return None
        return override
    if target is None or target not in PINNED_SHA256:
        note(f"compat_serve SKIPPED: no pinned {PINNED_VERSION} archive for {target}")
        return None
    path = os.path.join(cache_dir(), f"unpeel-{PINNED_VERSION}-{target}.tar.gz")
    if os.path.isfile(path) and sha256_of(path) == PINNED_SHA256[target]:
        return path
    os.makedirs(os.path.dirname(path), exist_ok=True)
    url = (
        f"https://unpeel.com/releases/{PINNED_CHANNEL}/cli/"
        f"unpeel-{PINNED_VERSION}-{target}.tar.gz"
    )
    partial = path + ".part"
    try:
        # Cloudflare answers the default python-urllib agent with 403; name ourselves.
        request = urllib.request.Request(
            url, headers={"User-Agent": "unpeel-matrix/compat_serve (+https://unpeel.com)"}
        )
        with urllib.request.urlopen(request, timeout=60) as response, open(partial, "wb") as out:
            shutil.copyfileobj(response, out)
    except Exception as error:  # noqa: BLE001
        note(f"compat_serve SKIPPED: could not fetch {url}: {error}")
        try:
            os.remove(partial)
        except OSError:
            pass
        return None
    actual = sha256_of(partial)
    if actual != PINNED_SHA256[target]:
        os.remove(partial)
        note(f"compat_serve SKIPPED: {url} sha256 {actual} != pinned {PINNED_SHA256[target]}")
        return None
    os.replace(partial, path)
    return path


def extract(archive, into):
    os.makedirs(into, exist_ok=True)
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            if member.name in ("unpeel", "unpeel-host"):
                tar.extract(member, into)
    for name in ("unpeel", "unpeel-host"):
        os.chmod(os.path.join(into, name), 0o755)
    return os.path.join(into, "unpeel")


class OldServe:
    """A foreground `unpeel serve` from the pinned archive."""

    def __init__(self, binary, home):
        self.home = home
        self._log = open(home.path("old-serve.log"), "w")
        self.process = subprocess.Popen(
            [binary, "serve"],
            cwd=CRATES,
            env=dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1"),
            stdin=subprocess.DEVNULL,
            stdout=self._log,
            stderr=subprocess.STDOUT,
        )

    def status(self):
        try:
            with open(self.home.path("serve.json")) as handle:
                return json.load(handle)
        except (FileNotFoundError, ValueError, OSError):
            return {}

    def ready(self, timeout=20.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end and self.process.poll() is None:
            if self.status().get("pid") == self.process.pid and self.status().get("hookPort"):
                return True
            time.sleep(0.3)
        return False

    def alive(self):
        return self.process.poll() is None

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self._log.close()

    def log(self):
        try:
            with open(self.home.path("old-serve.log")) as handle:
                return handle.read()
        except OSError:
            return ""


def old_cli(binary, home, args, timeout=30):
    return subprocess.run(
        [binary, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        env=dict(os.environ, UNPEEL_HOME=home.root, UNPEEL_TEST="1"),
        cwd=CRATES,
    )


FUTURE_KEYS = {
    "a_key_from_a_future_version": {"nested": [1, 2, 3]},
    "experimental_features": {"sessions_mcp": True, "future_gate": "keep"},
    "theme": "midnight",
}


def body(case):
    home = case.home
    archive = resolve_archive(case.note)
    if archive is None:
        # The NOTE above carries the reason; record the skip as an explicit
        # check so the harness does not report "case recorded no checks".
        case.check("compat_serve skipped: pinned archive unavailable (see NOTE)", True)
        return
    old_bin = extract(archive, os.path.join(cache_dir(), "bin"))

    # A home as THIS version writes it, plus keys neither version models.
    state = home.state()
    state.update(FUTURE_KEYS)
    state["projects"] = [{"id": "p", "name": "unpeel", "path": "/tmp", "future_field": 1}]
    state["presets"] = [
        {"id": "c", "label": "claude", "command": "claude", "project_id": None,
         "enabled": True, "quick_launch": True, "from_later": {"x": 1}}
    ]
    with open(home.path("app-state.json"), "w") as handle:
        json.dump(state, handle, indent=2)
    with open(home.path("session-order.json"), "w") as handle:
        json.dump({"p": ["s-new"], "unknown_future_key": True}, handle)
    home.session("s-new", label="written by this version", project_id="p",
                 created_at=1_800_000_000_000, settled=True)
    manifest_path = home.path("app-sessions", "s-new", "manifest.json")
    with open(manifest_path) as handle:
        manifest = json.load(handle)
    manifest["a_field_from_later"] = True
    manifest["session"]["something_new"] = "value"
    with open(manifest_path, "w") as handle:
        json.dump(manifest, handle)

    # ── direction 1: the SHIPPED serve hosts a home written by this version,
    #    while this version's CLI edits it ──
    old = OldServe(old_bin, home)
    ready = old.ready()
    case.check(f"the shipped {PINNED_VERSION} serve starts on a newer home", ready, old.log()[-400:])
    if ready:
        listed = run_cli(home, ["ls"])
        case.check(
            "this CLI lists the session while the old Host runs",
            listed.returncode == 0 and "written by this version" in listed.stdout,
            listed.stdout[:200] + listed.stderr[:200],
        )
        run_cli(home, ["presets", "add", "skew", "echo skew"], expect_ok=True)
        run_cli(home, ["presets", "edit", "skew", "--command", "echo skewed"], expect_ok=True)
        run_cli(home, ["presets", "remove", "skew"], expect_ok=True)
        run_cli(home, ["settings", "set", "auto_stop_archive_minutes", "60"], expect_ok=True)
        old_listed = old_cli(old_bin, home, ["ls"])
        case.check(
            "the old CLI lists a manifest carrying fields it has never seen",
            old_listed.returncode == 0 and "written by this version" in old_listed.stdout,
            old_listed.stdout[:200] + old_listed.stderr[:200],
        )
        time.sleep(1.5)
        case.check("the old Host keeps running through newer writes", old.alive(), old.log()[-400:])
    old.close()
    after = home.state()
    case.check(
        "unmodelled app-state keys survive the old Host and both CLIs",
        after.get("a_key_from_a_future_version") == FUTURE_KEYS["a_key_from_a_future_version"]
        and after.get("experimental_features", {}).get("future_gate") == "keep"
        and after.get("theme") == "midnight"
        and after.get("projects", [{}])[0].get("future_field") == 1
        and after.get("presets", [{}])[0].get("from_later") == {"x": 1},
        str(after)[:400],
    )
    case.check(
        "a newer setting written beside the old Host is intact",
        after.get("auto_stop_archive_minutes") == 60,
        str(after.get("auto_stop_archive_minutes")),
    )
    with open(home.path("session-order.json")) as handle:
        order = json.load(handle)
    case.check("unmodelled session-order keys survive", order.get("unknown_future_key") is True, str(order))
    with open(manifest_path) as handle:
        manifest_after = json.load(handle)
    case.check(
        "unmodelled manifest fields survive the old Host",
        manifest_after.get("a_field_from_later") is True
        and manifest_after["session"].get("something_new") == "value",
        str(manifest_after)[:300],
    )

    # ── direction 2: THIS serve hosts the same home while the shipped CLI
    #    writes into it ──
    service = case.serve()
    ready = service.ready()
    case.check("this serve starts after the old Host released the home", bool(ready), service.log()[-400:])
    if ready:
        old_added = old_cli(old_bin, home, ["presets", "add", "old-skew", "echo old"])
        case.check("the old CLI can add a preset beside this Host", old_added.returncode == 0, old_added.stderr[:200])
        os.makedirs(home.path("old-folder"), exist_ok=True)
        old_project = old_cli(old_bin, home, ["add", home.path("old-folder")])
        case.check("the old CLI can add a project beside this Host", old_project.returncode == 0, old_project.stderr[:200])
        listed = run_cli(home, ["ls"])
        case.check(
            "this CLI still lists the home after the old CLI's writes",
            listed.returncode == 0 and "written by this version" in listed.stdout,
            listed.stdout[:200],
        )
        service.read_for(1.0)
        case.check("this Host keeps running through the old CLI's writes", not service.exited(timeout=0.2), service.log()[-400:])
    service.close()
    final = home.state()
    case.check(
        "unmodelled keys survive the old CLI's writes",
        final.get("a_key_from_a_future_version") == FUTURE_KEYS["a_key_from_a_future_version"]
        and final.get("theme") == "midnight"
        and final.get("experimental_features", {}).get("future_gate") == "keep",
        str(final)[:400],
    )
    case.check(
        "both versions' presets and projects coexist",
        any(p.get("label") == "old-skew" for p in final.get("presets", []))
        and any(
            os.path.realpath(p.get("path", "")) == os.path.realpath(home.path("old-folder"))
            for p in final.get("projects", [])
        )
        and final.get("presets", [{}])[0].get("from_later") == {"x": 1},
        str(final.get("presets"))[:300],
    )


run("compat_serve", body)
