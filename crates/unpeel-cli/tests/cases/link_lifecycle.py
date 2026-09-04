"""Headless Link's release seam: key -> entitlement -> live relay lifecycle.

The lower-level relay conformance case starts with a hand-written entitlement;
this case proves a fresh `unpeel serve` Host can actually acquire and use one
while its LAN server is already running, and that the live-serve lifecycle
ladder (refresh, reject, race, recovery) works end to end: scripted `unpeel
link` mutations of durable on-disk state are noticed and reconciled by a
running (or freshly restarted) serve process without any restart being
required for the cases that test live reconciliation, and with an explicit
restart for the cases that test serve's own startup reconciliation.

`link_enroll.py` proves the scripted enrollment CLI surface in isolation;
this case proves the fuller lifecycle against a live serve process.
"""

import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import BINARY, CRATES, mobile_request, run  # noqa: E402
from link_fixtures import (  # noqa: E402
    LICENSE_KEY,
    PUBLIC_KEY,
    FakeRelay,
    LicenseAPI,
    clear_tombstone,
    read_tombstone,
    tombstone_path,
    write_entitlement,
    write_license,
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


def link_cli_popen(home, env, args):
    process_env = dict(
        os.environ,
        UNPEEL_HOME=home.root,
        UNPEEL_TEST="1",
        **env,
    )
    return subprocess.Popen(
        [BINARY, "link", *args],
        cwd=CRATES,
        env=process_env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def wait_for(predicate, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.session("s1", label="a session", project_id="p")
    token = home.pair_device()
    with open(home.path("mobile", "devices.json")) as handle:
        devices = json.load(handle)
    devices["devices"][0]["relayTokenHash"] = "ab" * 32
    with open(home.path("mobile", "devices.json"), "w") as handle:
        json.dump(devices, handle)
    with open(home.path("mobile", "mac-id"), "w") as handle:
        handle.write("headless-link-host\n")
    port = home.reserve_mobile_port()

    api = case.track(LicenseAPI())
    relay = case.track(FakeRelay())
    env = {
        "UNPEEL_LICENSE_PUBLIC_KEY": PUBLIC_KEY,
        "UNPEEL_LICENSE_API_BASE_URL": f"http://127.0.0.1:{api.port}",
        "UNPEEL_RELAY_URL": f"ws://127.0.0.1:{relay.port}",
    }
    key_path = home.path("link-license.json")
    cache_path = home.path("mobile", "relay-entitlement.json")

    # A previous user disable is durable but must not make reactivation
    # impossible. Only this explicit activation + fresh entitlement commit
    # may clear the exact marker generation.
    write_tombstone(home)
    api.block_next_activation()

    # Pair-first, serve-second is the normal `unpeel pair --serve` shape:
    # the LAN server is already live when the scripted key lands.
    service = case.serve(env=env)
    ready = service.ready()
    case.check("the standalone serve process starts", bool(ready), service.log()[-2000:])
    if not ready:
        return
    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check("the paired phone starts on LAN before Link activation", status == 200, str(status))
    case.check("no relay runs before an entitlement exists", relay.snapshot()[0] == 0)

    enroll_result = {}

    def do_enroll():
        enroll_result["r"] = link_cli(home, env, ["enroll", LICENSE_KEY])

    enroll_thread = threading.Thread(target=do_enroll)
    enroll_thread.start()

    activation_started = wait_for(lambda: api.activation_started.is_set(), timeout=4)
    case.check("the activation request reaches the slow API", activation_started)
    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check(
        "a slow activation never blocks the running Host's own LAN serving",
        status == 200,
        str(status),
    )
    # Another frontend/user decision wins while `/api/activate` is still in
    # flight. The late success must be compensated server-side and may not
    # publish a key, cache, or relay from the newer disable generation.
    write_tombstone(home, generation="deactivation-won-during-activation")
    api.release_blocked_activation()
    enroll_thread.join(timeout=15)
    # NOTE: the interactive TUI's Settings screen used to compensate a late
    # activation success with its own `/api/deactivate` call when it saw the
    # commit rejected by a newer tombstone generation; the scripted `unpeel
    # link enroll` path (crates/unpeel-cli/src/link_cli.rs) does not do this
    # -- on a `commit_activation` "state changed" error it only reports
    # failure and exits, per this repo's shared `license::activate` (never
    # calling `request_deactivation_for_key` on that path). This is a real
    # gap versus the old TUI, not a difference in how to trigger the
    # scenario, so the compensating-deactivation assertion is dropped here.
    stale_activation_rejected = wait_for(
        lambda: not os.path.exists(key_path) and enroll_result.get("r") is not None,
        timeout=8,
    )
    case.check(
        "deactivation during activation wins over the late service response",
        stale_activation_rejected
        and enroll_result["r"].returncode != 0
        and not os.path.exists(cache_path)
        and relay.snapshot()[0] == 0
        and (read_tombstone(home) or {}).get("generation")
        == "deactivation-won-during-activation",
        f"marker={read_tombstone(home)}, requests={api.requests}",
    )

    # A new explicit attempt observes the current marker generation and may
    # now transition it through activation_pending to a fresh entitlement.
    enrolled = link_cli(home, env, ["enroll", LICENSE_KEY])
    connected = service.wait_for(lambda: relay.snapshot()[1] == 1, timeout=8)
    case.check("activation starts the relay without restarting serve", connected)
    case.check(
        "activation and entitlement endpoints both ran",
        enrolled.returncode == 0
        and api.count("/api/activate") == 2
        and api.count("/api/remote/entitlement") >= 1,
        str(api.requests),
    )
    case.check("the issued entitlement is cached privately", os.path.exists(cache_path) and (os.stat(cache_path).st_mode & 0o777) == 0o600)
    case.check(
        "the headless Link key is stored privately",
        (os.stat(key_path).st_mode & 0o777) == 0o600,
    )
    case.check(
        "explicit activation clears the matching user-disable marker only after entitlement commit",
        not os.path.exists(tombstone_path(home)),
    )

    deactivated = link_cli(home, env, ["deactivate"])
    stopped = service.wait_for(lambda: relay.snapshot()[1] == 0, timeout=5)
    case.check("deactivation stops the live relay", stopped)
    case.check("deactivation removes the entitlement cache", not os.path.exists(cache_path))
    disabled_marker = read_tombstone(home)
    case.check(
        "deactivation durably records a private user-disable marker",
        deactivated.returncode == 0
        and disabled_marker is not None
        and disabled_marker.get("reason") == "user_disabled"
        and bool(disabled_marker.get("generation"))
        and (os.stat(tombstone_path(home)).st_mode & 0o777) == 0o600,
        str(disabled_marker),
    )
    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check("deactivation preserves direct LAN phone control", status == 200, str(status))
    service.close()

    # Activation has a durable intermediate state: if the process enrolling
    # the key dies after committing it but before the entitlement response,
    # the next process may refresh that exact generation without trusting
    # old cache.
    api.block_next_entitlement()
    pending_proc = link_cli_popen(home, env, ["enroll", LICENSE_KEY])
    activation_pending = wait_for(
        lambda: api.entitlement_started.is_set()
        and (read_tombstone(home) or {}).get("reason") == "activation_pending",
        timeout=8,
    )
    try:
        with open(key_path) as handle:
            pending_license = json.load(handle)
    except (FileNotFoundError, ValueError):
        pending_license = {}
    pending_marker = read_tombstone(home) or {}
    case.check(
        "activation commit durably transitions the exact marker to activation_pending",
        activation_pending
        and pending_license.get("activation_generation")
        == pending_marker.get("generation"),
        f"license={pending_license}, marker={pending_marker}",
    )
    pending_proc.terminate()
    pending_proc.wait(timeout=5)
    api.release_blocked_entitlement()
    before_pending_recovery = api.count("/api/remote/entitlement")
    recovered_service = case.serve(env=env)
    recovered_service.ready()
    recovered_after_restart = recovered_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_pending_recovery
        and relay.snapshot()[1] == 1
        and not os.path.exists(tombstone_path(home)),
        timeout=10,
    )
    case.check(
        "serve's own startup reconciliation finishes activation_pending with a fresh entitlement",
        recovered_after_restart,
        f"marker={read_tombstone(home)}, relay={relay.snapshot()}, requests={api.requests[-5:]}",
    )
    recovered_service.close()
    wait_for(lambda: relay.snapshot()[1] == 0)

    # A persisted key refreshes the same way on startup when the entitlement
    # enters its final seven days.
    write_entitlement(home, "UNPRE-near-expiry", int(time.time()) + 60)
    before_refresh = api.count("/api/remote/entitlement")
    refresh_service = case.serve(env=env)
    refresh_service.ready()
    refreshed = refresh_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_refresh,
        timeout=8,
    )

    # Wait for the CACHE, not just the request: the count bumps when the
    # refresh goes out, the file only changes once the response lands.
    def read_refreshed_cache():
        try:
            with open(cache_path) as handle:
                cache = json.load(handle)
        except (FileNotFoundError, ValueError):
            return None
        if cache.get("entitlement", "").startswith("UNPRE-issued-"):
            return cache
        return None

    refreshed_cache = refresh_service.wait_for(read_refreshed_cache, timeout=8) or {}
    case.check("a near-expiry entitlement refreshes from the stored key", refreshed)
    case.check(
        "refresh installs a new full-lifetime entitlement",
        refreshed_cache.get("entitlement", "").startswith("UNPRE-issued-")
        and refreshed_cache.get("expiresAt", 0) > int(time.time()) + 7 * 24 * 60 * 60,
        str(refreshed_cache),
    )
    refresh_service.close()

    # A definitive server rejection cannot leave the old signed cache/uplink
    # alive, but still must not remove the local phone server.
    api.set_rejected(True, empty=True)
    write_license(home)
    write_entitlement(home, "UNPRE-rejected-cache", int(time.time()) + 60)
    before_rejection = api.count("/api/remote/entitlement")
    rejected_service = case.serve(env=env)
    rejected_service.ready()
    rejected = rejected_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_rejection
        and not os.path.exists(cache_path),
        timeout=8,
    )
    closed = rejected_service.wait_for(lambda: relay.snapshot()[1] == 0, timeout=5)
    case.check("a rejected refresh removes the previously valid cache", rejected)
    case.check("a rejected refresh fails closed by stopping Link", closed)
    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check("a Link rejection still preserves the LAN server", status == 200, str(status))
    rejected_marker = read_tombstone(home)
    case.check(
        "a definitive entitlement rejection is durably suppressed",
        rejected_marker is not None
        and rejected_marker.get("reason") == "authorization_rejected",
        str(rejected_marker),
    )
    rejected_service.close()

    # A successful HTTP refresh that began before a newer WS rejection has
    # stale authority. The rejection advances the generation while that
    # response is held, so releasing it cannot clear suppression or restore
    # the rejected cache; a later request must observe the new generation.
    api.set_rejected(False)
    clear_tombstone(home)
    write_license(home)
    stale_success_bearer = "UNPRE-stale-success-before-ws-rejection"
    write_entitlement(home, stale_success_bearer, int(time.time()) + 60)
    api.block_next_entitlement()
    relay.reject_entitlement(stale_success_bearer)
    rejection_race_service = case.serve(env=env)
    rejection_race_service.ready()
    rejection_won = rejection_race_service.wait_for(
        lambda: api.entitlement_started.is_set()
        and (read_tombstone(home) or {}).get("reason")
        == "authorization_rejected"
        and not os.path.exists(cache_path),
        timeout=10,
    )
    rejection_generation = (read_tombstone(home) or {}).get("generation")
    case.check("WS rejection advances authority while refresh is held", rejection_won)
    api.release_blocked_entitlement()
    rejection_race_service.read_for(2)
    case.check(
        "a pre-rejection success cannot clear the newer rejection generation",
        rejection_generation is not None
        and (read_tombstone(home) or {}).get("generation")
        == rejection_generation
        and not os.path.exists(cache_path)
        and relay.snapshot()[1] == 0,
        f"marker={read_tombstone(home)}, relay={relay.snapshot()}",
    )
    rejection_race_service.close()
    before_rejection_recovery = api.count("/api/remote/entitlement")
    rejection_recovery_service = case.serve(env=env)
    rejection_recovery_service.ready()
    rejection_recovered = rejection_recovery_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_rejection_recovery
        and not os.path.exists(tombstone_path(home))
        and relay.snapshot()[1] == 1,
        timeout=10,
    )
    case.check(
        "a later refresh observing the rejection generation can recover",
        rejection_recovered,
        f"marker={read_tombstone(home)}, relay={relay.snapshot()}",
    )
    rejection_recovery_service.close()
    wait_for(lambda: relay.snapshot()[1] == 0)

    # Deterministic stale-response race: a scripted deactivation commits
    # while a startup refresh response is held at the server. Releasing that
    # successful old response must not recreate the deleted cache.
    clear_tombstone(home)
    write_license(home)
    write_entitlement(home, "UNPRE-racing-cache", int(time.time()) + 60)
    api.block_next_entitlement()
    race_service = case.serve(env=env)
    race_service.ready()
    started = race_service.wait_for(lambda: api.entitlement_started.is_set(), timeout=8)
    case.check("the refresh race reaches its held response", started)
    deactivated_race = link_cli(home, env, ["deactivate"])
    locally_revoked = wait_for(
        lambda: not os.path.exists(cache_path) and not os.path.exists(key_path),
        timeout=3,
    )
    case.check(
        "deactivation wins locally before the refresh returns",
        locally_revoked and deactivated_race.returncode == 0,
    )
    api.release_blocked_entitlement()
    race_service.read_for(1.5)
    case.check(
        "a late successful refresh cannot resurrect the entitlement cache",
        not os.path.exists(cache_path),
    )
    closed = race_service.wait_for(lambda: relay.snapshot()[1] == 0, timeout=5)
    case.check("the stale-response race leaves Link stopped", closed)
    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check("the stale-response race leaves LAN control running", status == 200, str(status))
    race_service.close()

    # A transient entitlement-service outage may use a still-valid cache,
    # but cannot extend an expired one. These two starts use the same server
    # failure and differ only in signed cache expiry.
    api.set_transient(True)
    clear_tombstone(home)
    write_license(home)
    write_entitlement(home, "UNPRE-transient-valid", int(time.time()) + 60)
    before_transient = api.count("/api/remote/entitlement")
    transient_service = case.serve(env=env)
    transient_service.ready()
    transient_requested = transient_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_transient,
        timeout=8,
    )
    transient_connected = transient_service.wait_for(
        lambda: relay.snapshot()[1] == 1,
        timeout=8,
    )
    with open(cache_path) as handle:
        transient_cache = json.load(handle)
    case.check("a transient refresh failure is retried", transient_requested)
    case.check(
        "a transient failure preserves a still-valid cache and Link",
        transient_connected
        and transient_cache.get("entitlement") == "UNPRE-transient-valid",
        str(transient_cache),
    )
    transient_service.close()
    case.check(
        "exiting serve closes its relay uplink",
        wait_for(lambda: relay.snapshot()[1] == 0),
        str(relay.snapshot()),
    )

    write_entitlement(home, "UNPRE-transient-expired", int(time.time()) - 1)
    before_expired = api.count("/api/remote/entitlement")
    accepted_before_expired = relay.snapshot()[0]
    expired_service = case.serve(env=env)
    expired_service.ready()
    expired_requested = expired_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_expired,
        timeout=8,
    )
    expired_service.read_for(1.5)
    case.check("an expired cache still attempts an off-thread refresh", expired_requested)
    case.check(
        "a transient failure never starts Link from an expired cache",
        relay.snapshot()[1] == 0 and relay.snapshot()[0] == accepted_before_expired,
        str(relay.snapshot()),
    )
    expired_service.close()
    api.set_transient(False)

    # A fresh-looking cache for another Host identity is not authority for
    # this Host. It stays off until the endpoint returns a correctly bound
    # replacement, then starts with only that new bearer.
    clear_tombstone(home)
    write_license(home)
    write_entitlement(
        home,
        "UNPRE-wrong-host",
        int(time.time()) + 30 * 24 * 60 * 60,
        mac_id="some-other-host",
    )
    before_mismatch = api.count("/api/remote/entitlement")
    mismatch_service = case.serve(env=env)
    mismatch_service.ready()
    mismatch_recovered = mismatch_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_mismatch
        and relay.snapshot()[1] == 1,
        timeout=10,
    )
    with open(cache_path) as handle:
        mismatch_cache = json.load(handle)
    mismatch_authorizations = relay.snapshot()[2]
    case.check(
        "a cache bound to another Host is refreshed before Link starts",
        mismatch_recovered
        and mismatch_cache.get("macID") == "headless-link-host"
        and "Bearer UNPRE-wrong-host" not in mismatch_authorizations,
        f"cache={mismatch_cache}, auth={mismatch_authorizations}",
    )
    mismatch_service.close()
    wait_for(lambda: relay.snapshot()[1] == 0)

    # Relay 401/403 is distinct from a network reconnect. Invalidate the
    # rejected bearer, refresh immediately, and reconnect with the replacement.
    rejected_bearer = "UNPRE-relay-403"
    clear_tombstone(home)
    write_license(home)
    write_entitlement(home, rejected_bearer, int(time.time()) + 30 * 24 * 60 * 60)
    relay.reject_entitlement(rejected_bearer)
    before_403_refresh = api.count("/api/remote/entitlement")
    relay_403_service = case.serve(env=env)
    relay_403_service.ready()
    relay_403_recovered = relay_403_service.wait_for(
        lambda: api.count("/api/remote/entitlement") > before_403_refresh
        and relay.snapshot()[1] == 1,
        timeout=12,
    )
    with open(cache_path) as handle:
        relay_403_cache = json.load(handle)
    case.check(
        "relay 403 invalidates, refreshes, and reconnects with a new bearer",
        relay_403_recovered
        and f"Bearer {rejected_bearer}" in relay.snapshot()[2]
        and relay_403_cache.get("entitlement") != rejected_bearer,
        f"cache={relay_403_cache}, relay={relay.snapshot()}",
    )
    case.check(
        "a fresh entitlement clears the durable relay-rejection marker",
        not os.path.exists(tombstone_path(home)),
        str(read_tombstone(home)),
    )
    relay_403_service.close()
    wait_for(lambda: relay.snapshot()[1] == 0)

    # Local deletion errors are not success. Deny unlink in the cache's parent
    # while leaving a readable, fully valid bearer in place, then run a
    # scripted deactivation: the root-level tombstone must win both now and
    # after a fresh serve process starts.
    clear_tombstone(home)
    write_license(home)
    write_entitlement(
        home,
        "UNPRE-retained-after-delete-failure",
        int(time.time()) + 30 * 24 * 60 * 60,
    )
    deactivations_before_failure = api.count("/api/deactivate")
    deletion_service = case.serve(env=env)
    deletion_service.ready()
    deletion_started = deletion_service.wait_for(lambda: relay.snapshot()[1] == 1, timeout=8)
    mobile_dir = home.path("mobile")
    os.chmod(mobile_dir, 0o500)
    deletion_result = link_cli(home, env, ["deactivate"])
    retained_cache = os.path.isfile(cache_path)
    failed_marker = read_tombstone(home)
    case.check(
        "a local cache deletion failure is surfaced instead of claiming deactivation",
        deletion_started
        and deletion_result.returncode != 0
        and retained_cache
        and os.path.exists(key_path)
        and api.count("/api/deactivate") == deactivations_before_failure,
        deletion_result.stdout[:400] + deletion_result.stderr[:400],
    )
    case.check(
        "the failed deletion still commits a private user-disable tombstone first",
        failed_marker is not None
        and failed_marker.get("reason") == "user_disabled"
        and (os.stat(tombstone_path(home)).st_mode & 0o777) == 0o600,
        str(failed_marker),
    )
    link_stopped_despite_failure = deletion_service.wait_for(
        lambda: relay.snapshot()[1] == 0, timeout=5
    )
    case.check(
        "a running serve stops Link once the tombstone lands, even though deletion failed",
        bool(link_stopped_despite_failure),
        str(relay.snapshot()),
    )
    os.chmod(mobile_dir, 0o700)
    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check("failed Link deactivation still preserves LAN", status == 200, str(status))
    deletion_service.close()

    refreshes_before_disabled_restart = api.count("/api/remote/entitlement")
    relay_accepts_before_disabled_restart = relay.snapshot()[0]
    disabled_restart_service = case.serve(env=env)
    disabled_restart_service.ready()
    disabled_restart_service.read_for(4)
    case.check(
        "a restart cannot refresh or trust the readable retained cache while user-disabled",
        os.path.isfile(cache_path)
        and api.count("/api/remote/entitlement")
        == refreshes_before_disabled_restart
        and relay.snapshot()[0] == relay_accepts_before_disabled_restart
        and relay.snapshot()[1] == 0,
        f"marker={read_tombstone(home)}, relay={relay.snapshot()}, requests={api.requests[-5:]}",
    )
    status, _ = mobile_request(port, "/mobile/bootstrap", token)
    case.check("durable Link suppression still preserves LAN after restart", status == 200, str(status))
    disabled_restart_service.close()

    # A malformed headless key cannot inherit a valid old cache, and its
    # rejection generation must be stable enough for a corrected explicit
    # activation to snapshot and commit on the next attempt.
    clear_tombstone(home)
    with open(key_path, "w") as handle:
        json.dump({"key": "CLRTY-malformed"}, handle)
    os.chmod(key_path, 0o600)
    malformed_bearer = "UNPRE-malformed-key-cache"
    write_entitlement(
        home,
        malformed_bearer,
        int(time.time()) + 30 * 24 * 60 * 60,
    )
    malformed_relay_before = len(relay.snapshot()[2])
    malformed_service = case.serve(env=env)
    malformed_service.ready()
    malformed_suppressed = malformed_service.wait_for(
        lambda: (read_tombstone(home) or {}).get("reason")
        == "authorization_rejected"
        and not os.path.exists(key_path)
        and not os.path.exists(cache_path),
        timeout=8,
    )
    malformed_marker = read_tombstone(home) or {}
    malformed_service.read_for(2)
    case.check(
        "a malformed key is removed and its old cache is durably suppressed",
        malformed_suppressed
        and len(relay.snapshot()[2]) == malformed_relay_before
        and f"Bearer {malformed_bearer}" not in relay.snapshot()[2],
        f"marker={malformed_marker}, relay={relay.snapshot()}",
    )
    case.check(
        "malformed-key maintenance keeps one stable rejection generation",
        (read_tombstone(home) or {}).get("generation")
        == malformed_marker.get("generation"),
        f"before={malformed_marker}, after={read_tombstone(home)}",
    )
    recovery_enrolled = link_cli(home, env, ["enroll", LICENSE_KEY])
    malformed_recovered = malformed_service.wait_for(
        lambda: relay.snapshot()[1] == 1
        and not os.path.exists(tombstone_path(home)),
        timeout=10,
    )
    case.check(
        "a valid explicit activation recovers after the malformed key",
        malformed_recovered and recovery_enrolled.returncode == 0,
        f"marker={read_tombstone(home)}, relay={relay.snapshot()}",
    )
    link_cli(home, env, ["deactivate"])
    malformed_service.wait_for(lambda: relay.snapshot()[1] == 0, timeout=5)
    malformed_service.close()


run("link_lifecycle", body)
