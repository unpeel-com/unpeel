"""Scripted headless Link enrollment: `unpeel link enroll <key>`.

`link_lifecycle.py` proves the live-serve Link lifecycle ladder (refresh,
reject, race, and recovery, against a running or restarted `unpeel serve`);
this case proves the scripted provisioning spelling shares the same durable
state: key + entitlement files, the locked suppression record, rejection and
deactivation semantics, and that a live `unpeel serve` picks a fresh
enrollment up without a restart.
"""

import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, run  # noqa: E402
from link_fixtures import (  # noqa: E402
    LICENSE_KEY,
    PUBLIC_KEY,
    FakeRelay,
    LicenseAPI,
    read_tombstone,
    tombstone_path,
    write_tombstone,
)


def link_cli(home, env, args, timeout=30):
    process_env = dict(
        os.environ,
        UNPEEL_HOME=home.root,
        UNPEEL_TEST="1",
        **env,
    )
    return subprocess.run(
        [BINARY, "link", *args],
        capture_output=True,
        text=True,
        timeout=timeout,
        env=process_env,
        cwd=CRATES,
    )


def body(case):
    home = case.home
    with open(home.path("mobile", "mac-id"), "w") as handle:
        handle.write("headless-link-host\n")

    api = case.track(LicenseAPI())
    relay = case.track(FakeRelay())
    env = {
        "UNPEEL_LICENSE_PUBLIC_KEY": PUBLIC_KEY,
        "UNPEEL_LICENSE_API_BASE_URL": f"http://127.0.0.1:{api.port}",
        "UNPEEL_RELAY_URL": f"ws://127.0.0.1:{relay.port}",
    }
    key_path = home.path("link-license.json")
    cache_path = home.path("mobile", "relay-entitlement.json")

    before = link_cli(home, env, ["status", "--json"])
    try:
        before_state = json.loads(before.stdout)
    except ValueError:
        before_state = {}
    case.check(
        "status reports an unenrolled Host with exit 1",
        before.returncode == 1 and before_state.get("enrolled") is False,
        before.stdout[:300] + before.stderr[:200],
    )

    # A previous user deactivation is durable, but an explicit scripted
    # enrollment must recover exactly like the interactive path does.
    write_tombstone(home)

    enrolled = link_cli(home, env, ["enroll", LICENSE_KEY])
    case.check(
        "enroll activates, stores the key, and fetches the entitlement",
        enrolled.returncode == 0
        and api.count("/api/activate") == 1
        and api.count("/api/remote/entitlement") == 1
        and os.path.exists(key_path)
        and os.path.exists(cache_path),
        enrolled.stdout[:400] + enrolled.stderr[:300],
    )
    case.check(
        "enrollment state files are private",
        (os.stat(key_path).st_mode & 0o777) == 0o600
        and (os.stat(cache_path).st_mode & 0o777) == 0o600,
    )
    case.check(
        "enroll clears the user-disable marker only through the shared commit path",
        not os.path.exists(tombstone_path(home)),
        str(read_tombstone(home)),
    )
    case.check(
        "enroll reports the resulting entitlement state",
        "fresh" in enrolled.stdout and "link-test@example.com" in enrolled.stdout,
        enrolled.stdout[:400],
    )

    again = link_cli(home, env, ["enroll", LICENSE_KEY])
    case.check(
        "re-running enroll is idempotent",
        again.returncode == 0 and os.path.exists(key_path) and os.path.exists(cache_path),
        again.stdout[:300] + again.stderr[:200],
    )

    status = link_cli(home, env, ["status", "--json"])
    try:
        state = json.loads(status.stdout)
    except ValueError:
        state = {}
    case.check(
        "status --json reports usable Link authority with exit 0",
        status.returncode == 0
        and state.get("enrolled") is True
        and state.get("entitlement") == "fresh"
        and state.get("host_id") == "headless-link-host"
        and state.get("license", {}).get("plan") == "pro",
        status.stdout[:400],
    )

    # A live serve on this workspace starts Link from the fresh enrollment
    # without any restart — the headless provisioning promise.
    token = home.pair_device()
    with open(home.path("mobile", "devices.json")) as handle:
        devices = json.load(handle)
    devices["devices"][0]["relayTokenHash"] = "ab" * 32
    with open(home.path("mobile", "devices.json"), "w") as handle:
        json.dump(devices, handle)
    service = case.serve(env=env)
    ready = service.ready()
    connected = service.wait_for(lambda: relay.snapshot()[1] == 1, timeout=15)
    case.check(
        "a running serve starts Link from a scripted enrollment without restart",
        bool(ready) and bool(connected),
        str((ready, relay.snapshot()))[-400:],
    )
    service.process.terminate()
    service.exited(timeout=10)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and relay.snapshot()[1] != 0:
        time.sleep(0.1)

    # Deactivation: local revocation first, then the best-effort seat release.
    deactivated = link_cli(home, env, ["deactivate"])
    marker = read_tombstone(home)
    case.check(
        "deactivate removes key + cache and records the durable user-disable marker",
        deactivated.returncode == 0
        and api.count("/api/deactivate") == 1
        and not os.path.exists(key_path)
        and not os.path.exists(cache_path)
        and marker is not None
        and marker.get("reason") == "user_disabled",
        deactivated.stdout[:300] + str(marker),
    )
    after = link_cli(home, env, ["status"])
    case.check(
        "status reflects deactivation with exit 1",
        after.returncode == 1 and "disabled (user-disabled)" in after.stdout,
        after.stdout[:300],
    )

    # A definitive service rejection fails closed with the shared durable
    # suppression record, exactly like the interactive/refresh paths.
    api.set_rejected(True)
    rejected = link_cli(home, env, ["enroll", LICENSE_KEY])
    rejected_marker = read_tombstone(home)
    case.check(
        "an entitlement rejection exits 1 and durably suppresses Link",
        rejected.returncode == 1
        and rejected_marker is not None
        and rejected_marker.get("reason") == "authorization_rejected"
        and not os.path.exists(cache_path),
        str((rejected.returncode, rejected.stderr[:200], rejected_marker)),
    )
    api.set_rejected(False)

    # A transient outage after activation leaves the durable
    # activation_pending intermediate state and exits 2 (retryable).
    api.set_transient(True)
    transient = link_cli(home, env, ["enroll", LICENSE_KEY])
    pending_marker = read_tombstone(home) or {}
    case.check(
        "a transient entitlement failure exits 2 with the key durably committed",
        transient.returncode == 2
        and os.path.exists(key_path)
        and pending_marker.get("reason") == "activation_pending",
        str((transient.returncode, transient.stderr[:300], pending_marker)),
    )
    api.set_transient(False)

    retried = link_cli(home, env, ["enroll", LICENSE_KEY])
    case.check(
        "a retry finishes the pending enrollment",
        retried.returncode == 0
        and not os.path.exists(tombstone_path(home))
        and os.path.exists(cache_path),
        retried.stdout[:300] + retried.stderr[:200],
    )

    garbage = link_cli(home, env, ["enroll", "CLRTY-not-a-key"])
    case.check(
        "a malformed key is rejected offline with exit 1",
        garbage.returncode == 1 and "valid Unpeel license key" in garbage.stderr,
        garbage.stderr[:200],
    )


run("link_enroll", body)
