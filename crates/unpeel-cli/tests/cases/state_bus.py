"""Cross-frontend sync: the app and the terminal are two UIs over one
state, so a change in either shows up in the other AT ONCE.

Shared state lives on disk, but disk alone is slow to notice: the app
coalesces file events at 0.5s and falls back to a 5s rescan, and the TUI
polls at 1s. So whoever writes also pings every other instance registered
in ~/.unpeel/app-ports (unpeel-core::state_bus). This case drives that bus
from both ends."""

import sys, os, json, time, urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from harness import run, tui_hook_port, run_cli, mobile_request  # noqa: E402


def ping(port, change):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/state-changed",
        data=json.dumps({"change": change}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    return urllib.request.urlopen(request, timeout=5).status


def body(case):
    home = case.home
    home.project("p", "unpeel", "/tmp")
    home.session("s1", label="first session", project_id="p")

    token = home.pair_device()
    mobile = home.reserve_mobile_port()
    service = case.serve()
    service.ready()
    port = tui_hook_port(home)
    case.check("serve is on the bus", port is not None)

    def sidebar_labels():
        try:
            status, payload = mobile_request(mobile, "/mobile/bootstrap", token)
        except Exception:
            return ""
        if status != 200:
            return ""
        return json.dumps(payload)

    case.check("it serves the ping route", ping(port, "order") == 200)

    # ── another frontend renames a session on disk, then pings ──
    home.marker("s1", "title.json", {"title": "renamed elsewhere"})
    ping(port, "session-markers")
    # Fast: this must not wait for the 1s poll, let alone a 5s safety net.
    deadline = time.time() + 1.5
    seen = False
    while time.time() < deadline and not seen:
        seen = "renamed elsewhere" in sidebar_labels()
        if not seen:
            time.sleep(0.1)
    case.check(
        "a marker change lands almost immediately",
        seen,
        sidebar_labels()[:200],
    )

    # ── and the TUI announces its own writes, so peers hear them ──
    # A second listener stands in for the desktop app.
    import threading, socket

    heard = []
    peer = socket.socket()
    peer.bind(("127.0.0.1", 0))
    peer.listen(4)
    peer_port = peer.getsockname()[1]

    def serve():
        while True:
            try:
                conn, _ = peer.accept()
            except OSError:
                return
            data = conn.recv(4096).decode("utf-8", "replace")
            if "/state-changed" in data:
                heard.append(data)
            conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            conn.close()

    threading.Thread(target=serve, daemon=True).start()
    with open(home.path("app-ports"), "a") as handle:
        handle.write(f"{peer_port}\n")

    # Archiving writes a shared marker — the peer must be told.
    run_cli(home, ["archive", "s1"])
    deadline = time.time() + 3
    while time.time() < deadline and not heard:
        time.sleep(0.1)
    case.check("a write from the CLI announces itself", bool(heard), str(heard)[:160])
    if heard:
        case.check(
            "the ping names what changed",
            "session-markers" in heard[0],
            heard[0][:200],
        )

    # ── every kind of shared write announces, not just markers ──
    def kinds():
        return {k for note in heard for k in
                ("session-markers", "lifecycle", "app-state", "order")
                if f'"change":"{k}"' in note}

    home.preset(label="cat", command="cat")
    started = run_cli(home, ["new", "--preset", "cat", "--project", "p"])
    case.check("spawn returns", started.returncode == 0, started.stderr[:120])
    deadline = time.time() + 3
    while time.time() < deadline and "lifecycle" not in kinds():
        time.sleep(0.1)
    case.check("a spawn announces lifecycle", "lifecycle" in kinds(), str(kinds()))

    os.makedirs(home.path("bus-folder"), exist_ok=True)
    run_cli(home, ["add", home.path("bus-folder")])
    deadline = time.time() + 3
    while time.time() < deadline and "app-state" not in kinds():
        time.sleep(0.1)
    case.check(
        "an app-state write announces (the save choke point)",
        "app-state" in kinds(),
        str(kinds()),
    )

    # And it must never ping itself into a loop.
    case.check(
        "it does not ping its own port",
        all(f":{port}/" not in note for note in heard),
    )
    peer.close()


run("state_bus", body)
