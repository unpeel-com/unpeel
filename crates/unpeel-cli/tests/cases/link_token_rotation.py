"""Relay credential rotation keeps the live uplink and its clients.

`relay.credentials.recover` rotates one paired device's Relay token and
rewrites `devices.json`. Before 2026-09-02 the uplink loop treated any
registration change as authority loss and tore the Host socket down, which
made the Relay evict every connected phone; a phone reconnecting inside that
window recovered again and looped. This case drives the exact on-disk effect
of a recovery against the live uplink and proves the Host re-announces the
rotated token in place: same uplink, one extra hello, no replacement. Adding
or removing a device still replaces the uplink, because the Relay must forget
a revoked token and refuse a device that was never announced.
"""

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run  # noqa: E402
from link_fixtures import (  # noqa: E402
    PUBLIC_KEY,
    FakeRelay,
    LicenseAPI,
    write_entitlement,
    write_license,
)


def wait_for(predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def rewrite_devices(home, mutate):
    path = home.path("mobile", "devices.json")
    with open(path) as handle:
        store = json.load(handle)
    mutate(store)
    temporary = path + ".tmp"
    with open(temporary, "w") as handle:
        json.dump(store, handle)
    os.replace(temporary, path)


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.session("s1", label="a session", project_id="p")
    home.pair_device()
    first_hash = "ab" * 32
    rewrite_devices(home, lambda store: store["devices"][0].__setitem__("relayTokenHash", first_hash))
    with open(home.path("mobile", "mac-id"), "w") as handle:
        handle.write("headless-link-host\n")
    home.reserve_mobile_port()
    write_license(home)
    write_entitlement(home, "UNPRE-rotation-valid", int(time.time()) + 3600)

    api = case.track(LicenseAPI())
    # A successful startup refresh would replace the cached entitlement and
    # therefore the uplink; a transient failure keeps the valid cache and
    # leaves exactly one uplink for this case to observe.
    api.set_transient(True)
    relay = case.track(FakeRelay())
    env = {
        "UNPEEL_LICENSE_PUBLIC_KEY": PUBLIC_KEY,
        "UNPEEL_LICENSE_API_BASE_URL": f"http://127.0.0.1:{api.port}",
        "UNPEEL_RELAY_URL": f"ws://127.0.0.1:{relay.port}",
    }
    service = case.serve(env=env)
    connected = service.wait_for(lambda: relay.snapshot()[1] == 1, timeout=15)
    case.check("a cached entitlement starts the relay uplink", connected, str(relay.snapshot()))
    if not connected:
        return
    announced = wait_for(lambda: len(relay.hello_log()) == 1)
    device_id = "dev1"  # the harness's paired-device id
    case.check(
        "the uplink announces the paired device once",
        announced and relay.hello_log()[0][1] == [(device_id, first_hash)],
        str(relay.hello_log()),
    )
    if not announced:
        return
    accepted_before = relay.snapshot()[0]

    # The on-disk effect of relay.credentials.recover: same device, new token.
    rotated_hash = "cd" * 32
    rewrite_devices(home, lambda store: store["devices"][0].__setitem__("relayTokenHash", rotated_hash))
    re_announced = wait_for(
        lambda: any(
            ordinal == accepted_before and devices == [(device_id, rotated_hash)]
            for ordinal, devices in relay.hello_log()
        )
    )
    case.check(
        "a token rotation is re-announced over the same uplink",
        re_announced,
        f"hellos={relay.hello_log()} relay={relay.snapshot()}",
    )
    time.sleep(1.5)
    case.check(
        "a token rotation never replaces the uplink",
        relay.snapshot()[0] == accepted_before and relay.snapshot()[1] == 1,
        str(relay.snapshot()),
    )
    case.check(
        "one rotation produces exactly one re-announcement",
        len([1 for ordinal, _ in relay.hello_log() if ordinal == accepted_before]) == 2,
        str(relay.hello_log()),
    )

    # Two rotations in one write are still one re-announcement.
    second_rotated = "ef" * 32
    rewrite_devices(home, lambda store: store["devices"][0].__setitem__("relayTokenHash", second_rotated))
    wait_for(
        lambda: any(devices == [(device_id, second_rotated)] for _, devices in relay.hello_log())
    )
    time.sleep(1.0)
    case.check(
        "a second rotation also stays on the same uplink",
        relay.snapshot()[0] == accepted_before and relay.snapshot()[1] == 1,
        str(relay.snapshot()),
    )

    # Re-scoping a device (Link allowance off) is an authorization change,
    # not a rotation: the Relay must forget the token, so the uplink is torn
    # down rather than re-announced. Re-allowing it reconnects with a fresh
    # announcement on a new uplink.
    rewrite_devices(home, lambda store: store["devices"][0].__setitem__("relayAllowed", False))
    torn_down = wait_for(lambda: relay.snapshot()[1] == 0, timeout=10)
    case.check(
        "revoking Link scope tears the uplink down instead of re-announcing",
        torn_down
        and not any(devices == [] for _, devices in relay.hello_log()),
        f"hellos={relay.hello_log()} relay={relay.snapshot()}",
    )
    rewrite_devices(home, lambda store: store["devices"][0].__setitem__("relayAllowed", True))
    replaced = wait_for(
        lambda: relay.snapshot()[0] == accepted_before + 1 and relay.snapshot()[1] == 1,
        timeout=15,
    )
    case.check(
        "re-allowing Link scope reconnects with a fresh announcement",
        replaced
        and any(
            ordinal == accepted_before + 1 and devices == [(device_id, second_rotated)]
            for ordinal, devices in relay.hello_log()
        ),
        f"hellos={relay.hello_log()} relay={relay.snapshot()}",
    )

    service.close()
    case.check("serve exits cleanly on SIGTERM", service.returncode is not None)
    case.check(
        "exiting closes the relay uplink",
        wait_for(lambda: relay.snapshot()[1] == 0),
        str(relay.snapshot()),
    )


run("link_token_rotation", body)
