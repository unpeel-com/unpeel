<!-- Split out of the repo-root AGENTS.md (2026-08-31). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Unpeel Host service (`unpeel serve`)

`unpeel serve` is the canonical UI-free Host lifecycle. The public product
name is **Unpeel Host service**; “supervisor” and “workspace worker” describe
its internal processes, not separate products. The implementation is
`crates/unpeel-serve`, and both public binaries enter it:

- `unpeel serve` is the documented CLI spelling;
- bundled `unpeel-host __serve__` is the native app/service-manager entry;
- `__serve_workspace__` is a private child-process argument and must never be
  documented as a user command.

The runtime is deliberately not one process with several mutable
`UNPEEL_HOME`s. Much of the shipped state contract resolves paths process-wide.
One process per workspace preserves that isolation while one supervisor gives
the machine, app, and service manager a single lifecycle.

### Process and ownership model

```text
Unpeel Host service                     one per user/machine
  ├─ workspace Host worker: Default     one per UNPEEL_HOME
  │    ├─ Session unpeel-host           one persistent PTY per Session
  │    └─ Session unpeel-host …
  ├─ workspace Host worker: Writing
  │    └─ Session unpeel-host …
  └─ workspace Host worker: Research

Mac app / local gateway
  └─ unpeel-attach → session.sock        terminal data plane per visible pane
```

The machine supervisor owns discovery, worker restart, and clean worker
shutdown. A workspace worker owns activity, hooks, approvals, the framed local
Host contract, pairing, Direct, Link, and its workspace serving lease. It does
**not** own Session PTYs: each Session remains a separate `unpeel-host`
`__session_host__` process with its own journal and control socket. The app
is a frontend; `unpeel-attach` continues to connect directly to a
Session's `session.sock` for the terminal byte stream.

Package boundaries are intentional:

- `crates/unpeel-serve` is the standalone UI-free Host runtime package. It
  contains workspace serving plus the machine/workspace supervision layer.
- `crates/unpeel-core` contains reusable state, protocol, Session, transcript,
  Browser/Computer, and MCP domain implementations. Keeping those libraries
  below `unpeel-serve` lets Session hosts, gateways, tests, and future clients
  share one implementation without turning the supervisor into a monolith.
- `crates/unpeel-host` is the shipped multipurpose helper binary. In
  `__serve__` mode it launches `unpeel-serve`; in `__session_host__` mode one
  process owns one PTY; and each `__mcp__` invocation is a separate stdio MCP
  sidecar started by an agent client and scoped to its calling Session.
- `unpeel-attach` is only the direct terminal data plane.

MCP therefore belongs to the **Host architecture**, but not in the
machine-service process. Its stdio lifetime belongs to the agent client that
starts `unpeel-host __mcp__`; the implementation lives in
`unpeel-core::mcp_host`. Direct terminal reads and writes may use the same
per-Session artifacts and `session.sock` data plane as other Host-side tools.
Workspace policy, approvals, and semantic effects belong to the workspace
worker (with native capability adapters where required), so the MCP sidecar
must not become another long-lived Host authority. The Mac app is a
capability adapter and Controller, not a second MCP/serving engine; the
compatibility Swift Host was retired on 2026-09-03.

Controller-transport ownership now depends on the client generation:

1. for the native app, the Rust workspace worker owns pairing, Direct,
   Link, and the supervised TLS terminal streamer continuously—even while
   the app is open (every build and channel since 2026-09-03).

(The interactive terminal UI's compatibility Host path and its
development-gated loopback Controller path — `UNPEEL_DEV_LOCAL_HOST_CLIENT=1`,
which started/attached to the scoped or machine Host service before it could
create any compatibility hook, approval, pairing, Direct, or Link owner, using
the same `RemoteSessionBackend` over the worker's `host.sock` as the SSH
Controller while terminal output, input, and resize stayed on the direct
per-Session `session.sock` plane — were removed with the TUI on 2026-09-03.)

The Dev app advertises `X-Unpeel-Controller-Owner: serve` on its loopback
listener. The worker reads that explicit intent independently of the
connection-scoped platform adapter, so an adapter reconnect cannot bounce
Direct/Link ownership back to Swift. The private
`controller.transport.host-owned` adapter service supplies the same immediate
intent during registration but is never exposed as a public Host capability.
`serve.json.nativeAppOwnsControllers` records the effective compatibility
handoff result; it is `false` in the client-only path.

The native client-only migration is development-gated. In an explicitly
branded Unpeel Dev bundle, the app's own Local sidebar/projects/Sessions/preset
reads and the existing protocol-minor-8 lifecycle/organization verbs use its
workspace worker over `host.sock`, through the same `RemoteHostRuntime` used
for other Host scopes. This includes create, stop, archive/restore/remove,
restart/resume, rename, pin, move, and project/Session ordering. Once the
Local client starts, semantic effects fail closed during worker recovery;
an unavailable socket cannot silently reactivate the duplicate Swift engine.

Pairing and paired-device administration also use the live worker's reserved
local control route: begin/status/cancel, list sanitized devices, revoke, and
toggle per-device Link allowance. The app never constructs
`MobileRemoteServer`, starts `RemoteControlManager`, or owns
`RelayUplinkManager` in this path. `unpeel-attach` remains the direct terminal
data plane and the worker supervises exactly one `__remote__` TLS streamer.

### Direct `/mobile` transport: TLS with the Host certificate

The direct `/mobile` listener (`crates/unpeel-serve/src/mobile.rs`) binds all
interfaces on the persisted phone port and serves HTTP/1.1 **over TLS** with
the Host certificate — the same self-signed material under
`<home>/remote/tls/` that the `__remote__` WSS streamer serves
(`unpeel_core::remote_server::ensure_tls_material`). A Controller therefore
holds one fingerprint pin for both transports: the sealed pairing response and
`/mobile/bootstrap` carry it as `remoteServerCertificateFingerprint`
(advertised whenever the direct listener has it, streamer alive or not),
`serve.json.directCertificateFingerprint` publishes it for operators, and the
bootstrap capability `host.mobile.tls` (`protocol/host-capabilities-v1.json`,
protocol 1.15) tells a Controller the endpoint is TLS. Bootstrap and the
sealed pairing response also carry the additive `serverVersion` (the Host's
crate version, e.g. `"0.5.3"`), the phone's fallback signal when the
capability list is unavailable (`serverVersion >= 0.5.3` means TLS), and the
pairing response adds `directTLS: true`. Without loadable certificate
material there is no listener (fail closed); a cleartext-only server would
refuse every paired device anyway.

One port carries both transports during the client transition. Each accepted
socket is classified by peeking its first byte (`0x16` is a TLS ClientHello;
`Transport::Tls` / `Transport::Plaintext`), and the plaintext gate runs before
routing and before any token lookup: a cleartext request that presents an
`Authorization` header is answered `426 Upgrade Required`
(`Upgrade: TLS/1.3, HTTP/1.1`, `Connection: close`, body
`{"error":"use https"}`) and the token is never hashed or compared. Plaintext
may still reach `POST /mobile/pair`, whose exchange is sealed at the
application layer, so the QR-code pairing client and `unpeel pair` work
unchanged; every other plaintext request gets the 401/404 it always got. The
advertised endpoint string stays `http://<lan>:<port>/mobile` for the shipped
Controllers' parsers; a TLS-aware Controller connects HTTPS to that authority
with its pin. Phones on builds that still speak cleartext fall back to the
relay path, which is unaffected.

Proofs: `mobile.rs` unit tests (first-byte classification, the gate's truth
table, plaintext bearer → 426 end to end, TLS bearer past the gate, pairing on
both transports, a wrong pin never completing the handshake), the process
tests `serve_command.rs`, `remote_streamer_supervision_process.rs`, and
`local_gateway_process.rs` (pinned TLS clients via
`unpeel_core::remote_attach::pinned_client_config`), and the PTY matrix, whose
`mobile_request` helper pins the private home's certificate the way a phone
does (`crates/unpeel-cli/tests/harness.py`).

### Terminal streamer supervision

The worker owns the `unpeel-host __remote__` TLS/WSS streamer's whole
lifetime (`crates/unpeel-serve/src/remote_streamer.rs`, stepped from every
`HostRuntime::tick`), with the policy the compatibility Swift
`RemoteControlManager` had: an exit is detected with `try_wait`, the child is
respawned after 2 s, an exit within 10 s of launch counts as a rapid failure,
and five consecutive rapid failures give up until the paired-device set
changes (a fresh pairing or revocation is the user actively trying) or the
mobile server restarts. Before every spawn the pid named in `remote.json` is
checked with `pid_started_at`; only a proven-stale instance that is not the
worker's own child receives `SIGTERM` (never `pkill -f`), so a mobile-server
restart, ownership handoff, or Direct rebind leaves neither two streamers nor
none. Stopping the mobile server still kills and reaps the child. The state
is published additively as `serve.json.terminalStreamer`
(`state` = `live` | `restarting` | `gaveUp` | `unavailable`, plus `pid`,
`port`, `restarts`, `rapidFailures`, `lastExit`) and therefore inside
`unpeel serve status`; bootstrap already hides a dead streamer, so a phone on
a `gaveUp` Host falls back to long-poll instead of getting a dead WSS
endpoint. Every exit, respawn, stale reap, and give-up is a `host-worker`
trace line (`terminal streamer exited …`, `… respawned (pid …)`), so the next
investigation does not need `ps` archaeology. Process proof:
`crates/unpeel-host/tests/remote_streamer_supervision_process.rs` (kill the
live streamer under a running worker; crash-loop ceiling and retry on pairing
change through a shim `unpeel-host`).

### PTY core (default since 0.4.4; `UNPEEL_PTY_CORE=0` opts out)

The shared `unpeel-host __pty_core__` process hosts N Sessions in one
detached process (setsid, stdio null) instead of one `__session_host__` per
Session. Its on-disk contract is `$UNPEEL_HOME/pty-core.json`
(`{pid, pid_started_at, socket, host_build_id, protocol}`), a 0600
`pty-core.sock` speaking one newline-delimited JSON request per connection
(`ping`, `launch`, `shutdown`), and a single-instance flock on
`pty-core.lock` (a second core exits 0 at once). Manifests, `session.sock`,
`output.bin`, the attach protocol, and hook env are byte-for-byte unchanged;
`session_host::spawn_host_process_from_launch_file` routes a launch to the
core when the socket exists and falls back to a per-process host on any
failure. `UNPEEL_PTY_CORE=0` forces per-process everywhere; unset (the
default since 0.4.4, 65f7a76) manages a core, exactly like routing.

The workspace worker owns the core's *start*, never its *stop*
(`crates/unpeel-serve/src/pty_core_supervisor.rs`, stepped from every
`HostRuntime::tick`). While the gate is on: at start the worker reads
`pty-core.json`, verifies the recorded pid with `pid_started_at`
(`recorded_pid_identity`; a record without a start time is *unknown*, never
*refuted*), and requires a `ping` on the socket to answer with that pid —
then it **adopts** the core. Otherwise it spawns `unpeel-host __pty_core__`
detached exactly like a session host (setsid, stdio null, leaked `HERDR_*`
removed) and waits non-blocking for `ping`; a spawned core that stays silent
is warned about after 15 s and left alone. An exit of the worker's own child
is re-checked against the record first (a lock-losing duplicate exits 0 and
the survivor is adopted); a real exit respawns after 2 s with the streamer's
rapid-failure shape (an exit within 10 s of launch counts; five in a row →
`failed` until the worker restarts). An adopted core is pinged every 5 s to
refresh `sessions`; it counts as lost only when its pid is provably gone.
The supervisor never sends SIGTERM/SIGKILL to a core, never issues socket
`shutdown`, and dropping it does nothing to the process.

**`unpeel serve` stopping (or being killed) does not stop terminals** —
exactly like per-process hosts today. A clean worker stop reports
`worker stopping; PTY core pid … left running`, and the next worker adopts
it. The gate variable is inherited unchanged by everything the worker spawns
(workers, streamer, session hosts), so routing and the escape hatch agree
across the whole process tree.

Status is additive in `serve.json.ptyCore` next to `terminalStreamer`:
`{state: "off"|"starting"|"live"|"adopted"|"failed", pid, sessions,
rapidFailures, lastExit}` — absent while the gate is off. Every adopt, start,
ready, exit, loss, give-up, and leave-running is a `host-worker` trace line
(`PTY core adopted (pid …)`, `PTY core exited (…)`). Proof:
`pty_core_supervisor.rs` unit tests (adopt vs spawn vs stale record,
lock-race adoption, crash-loop ceiling) and the PTY case
`crates/unpeel-cli/tests/cases/pty_core_adopt.py` (gate off publishes
nothing; gate on adopts a fake core; `kill -9` of the worker leaves the core,
socket, and record untouched; the restarted worker re-adopts).

### Auto-stop-and-archive sweep: why a Session is not archived

`sweep_auto_archive` runs on every rescan tick (1 s). A running row is due
only when ALL hold: status Idle, `archive_available` (a resumable
conversation: `session_ops::can_archive_manifest` — a provider session id
plus transcript resume data or a real lifecycle hook; plain shells and an
agent that has not completed a turn never qualify), not pinned / archived,
not **unread attention** (a Session waiting on the user who has not seen
the question is never archived unseen; a merely unread idle Session DOES
archive after the cutoff — decided 2026-09-04 for headless Hosts, where
nobody views anything, ships in 0.5.1; archive is non-destructive and the
unread badge survives it), no attempt pending, and idle for the whole cutoff as observed by
THIS worker (the clock starts at the later of the row's idle start and the
worker's start, so a Host restart is never a mass archive). The cutoff is
`auto_stop_archive_minutes` and accepts only 0/30/60/120/240/480/1440 —
any other value reads as off. Since 2026-09-03 the sweep traces the FIRST
blocking condition per row, once per reason change (`host-worker
auto-archive skips <id>: unread attention (waiting on the user; never
auto-archived unseen)` / `no resumable conversation …` / `idle for less than the
cutoff` / `pinned` / …), so "why did this never archive" is one grep.

### Browser engine install (`serve.json.browserEngine`)

At start the worker spawns one `browser-engine-install` thread that runs
`unpeel_core::browser_engine::ensure_installed(home)` (pin:
`protocol/browser-engine-v1.json`; detail: `docs/agents/browser-mcp.md`).
`serve.json.browserEngine` is additive: `{state: "installing"}` until the
thread reports, then `{state: "ready", version, path}` or `{state:
"failed", version, error}`. It is never a startup failure; a failure is
also a `browser-engine` trace line. `UNPEEL_BROWSER_ENGINE_INSTALL=0`
(also `false`/`off`/`no`) starts no thread and publishes `{state:
"disabled"}` — `scripts/bench-memory.sh` sets it so the start-up
footprint never includes a download (the install streams through a 64 KiB
buffer to a `.part` file and hashes as it goes, but a benchmark must not
depend on the network at all); resolution still finds an engine installed
by hand or by `unpeel browser install`. The install is flock-serialised on
`~/.unpeel/browser/bin/.lock`, so sibling workspace workers and a manual
`unpeel browser install` wait and re-verify rather than racing.

### Computer Use engine install (`serve.json.computerEngine`)

The Computer Use adapter (`crates/unpeel-serve/src/computer.rs`) owns the
engine install and does it **on demand**, not at start: most Hosts never
turn the `computer` domain on and the cua-driver archive is 30–40 MiB. Each
reconcile tick keeps a resolved engine path cached (`computer_engine::resolve`
— override, verified managed copy, bundled sibling, PATH; re-checked for
existence, re-resolved every 5 s while missing). When no engine resolves
and `experimental_features.computer_use` is on in `app-state.json`, the
adapter spawns one `computer-engine-install` thread running
`unpeel_core::computer_engine::ensure_installed(home)` (pin:
`protocol/computer-engine-v1.json`; flock-serialised with `unpeel computer
install`) and reports `computerUseUnavailableReason` = "Installing Cua
Driver …" until it lands. `serve.json.computerEngine` is additive:
`{state: "missing"}` until requested, then `installing` → `ready` (`version`,
`path`) or `failed` (`error`); a failure stays visible until the next policy
edit or restart and is also a `computer-engine` trace line.
`UNPEEL_COMPUTER_ENGINE_INSTALL=0` (also `false`/`off`/`no`) never spawns the
thread and publishes `{state: "disabled"}` — `scripts/bench-memory.sh` and
the worker-spawning process tests set it beside the browser opt-out. Every
engine process the worker starts (daemon, `status`, `stop`) carries
`CUA_DRIVER_RS_TELEMETRY_ENABLED=0`: nothing about a Host's activity leaves
the user's machines.

### Computer Use readiness and the desktop-session unit (Lane B)

On Linux the adapter's "available" gate is `computer_engine::desktop_session()`,
not `DISPLAY` alone: a display (`DISPLAY`, or `WAYLAND_DISPLAY` with a
resolvable socket) **and** a session D-Bus, discovered the way cua-driver
does — `DBUS_SESSION_BUS_ADDRESS` → `$XDG_RUNTIME_DIR/bus` →
`/run/user/<uid>/bus` (a socket). The AT-SPI accessibility bus the engine
reads window trees from lives on that bus, so a serve started from a unit
with a display but no bus would otherwise advertise ready and fail at the
first `see`. The reason names the missing piece and points at
`cua-driver doctor --json`. The resolved session is re-evaluated every
reconcile and handed to the daemon child (`DBUS_SESSION_BUS_ADDRESS`, plus
`CUA_DRIVER_RS_ENABLE_WAYLAND=1` when the chosen session is Wayland).
`serve.json.computerUse` mirrors the bootstrap's
`{computerUseAvailable, computerUseReady, computerUseUnavailableReason?}`
so a headless Host is diagnosable from the file alone.

**The Linux engine is an X11 client** (it links libXi, libXtst, libX11 and
friends dynamically), so a bare image can verify every hash and still fail
to start the binary (`libXi.so.6: cannot open shared object file`). Both
the adapter (on every resolve and after an on-demand install) and `unpeel
computer install [--check]` therefore run the engine once (`--version`,
bounded, telemetry off; `computer_engine::probe`) and, on an exec failure,
report `failed` naming the missing libraries from the loader error or
`ldd` plus the Debian/Ubuntu line
`sudo apt-get install -y libxi6 libxtst6 libx11-6 libxext6 libxrandr2
libxinerama1 libxcursor1 libxfixes3 libxkbcommon0 libxcb1`
(`computer_engine::LINUX_RUNTIME_PACKAGES`). The adapter re-probes on its
next resolve, so installing the packages heals readiness without a
restart. Install those packages before `unpeel computer install` on any
minimal Debian/Ubuntu Host (the Box recipe in
`unpeel-apple:docs/plans/computer-use-release.md` does).

`unpeel serve install --graphical` (systemd only; launchd refuses) writes
`packaging/service/unpeel-serve-graphical.service` under the same unit name:
`PartOf=`/`After=`/`WantedBy=graphical-session.target`, so the Host starts
inside the desktop session and inherits the display the session manager
imported into the user manager. It is enabled, and started immediately only
if the target is already active. A hand-rolled session (Xvfb script, a
streamed Xorg desktop such as a Box) must run `systemctl --user
import-environment DISPLAY XAUTHORITY` and start a session target that
binds `graphical-session.target` (which refuses manual start by design):
`packaging/service/unpeel-desktop-session.target` is that target; an
`ExecStartPre=` import cannot substitute, because it runs with the
manager's own environment. `serve status` prints the unit
variant, the target's `is-active` state, and the desktop session (or its
missing piece) visible to the calling shell. Wayland stays best-effort for
0.5.0 (X11/Xvfb is the proven baseline).

### Link uplink: token rotation without eviction

The uplink loop in `crates/unpeel-serve/src/relay.rs` re-reads the
`(deviceID, relayTokenHash)` registrations every poll. Before 2026-09-02 any
difference tore the Host socket down, and the Relay then closed every
established phone; a phone reconnecting inside that window presented a token
the Relay no longer knew, called `relay.credentials.recover`, which minted a
new token, rewrote `devices.json`, and tore the uplink down again (observed
looping 2026-09-01). Now a change that keeps the announced device-id set and
only rotates token hashes (`token_rotation_only`) is re-announced **in
place**: the Host resends the ordinary hello frame over the same socket
(rate-limited to one per second), and the deployed Relay replaces its
registered token set on that repeated hello without closing any client
(`unpeel-relay:apps/relay` integration test "a repeated hello rotates a device token in
place without evicting clients"). No new Relay message exists; an older
hello-only Relay sees the same frame it always accepted. Pairing, unpairing,
or re-scoping a device (`relayAllowed`) still replaces the uplink, because
the Relay must forget a revoked token and never admit an unannounced one.
The authenticated `GET /mobile/relay-credentials` route additionally keeps a
15-second per-device replay window: a phone that retries recovery inside it
receives the credential already minted instead of rotating the Host again
(`relay-recovery` trace line). Proofs: PTY case
`crates/unpeel-cli/tests/cases/link_token_rotation.py` (two rotations, one
uplink, one re-announcement each; scope change still tears down) and the
`relay_credential_recovery_replays_a_fresh_mint…` unit test in `mobile.rs`.

The native `HookServer` is therefore a client callback listener, not a Host.
It dispatches only `/_unpeel/platform-adapter/call`, `/state-changed`,
`/show-window`, and `/reload-appearance`; provider hooks, `/mcp/*`,
`/notify/*`, hosted-App context/theme/opener routes, and every mobile Host
route return 404, and no Swift handler for them exists any more. Every
response carries `X-Unpeel-Controller-Owner: serve`.

Controller-assisted pairing is Controller-owned and deliberately separate
from both surfaces. `ControllerPairingProxy` opens one short-lived listener
for exactly `POST /mobile/pairing-proxy/<id>/pair`, forwards one opaque sealed
exchange through the selected Host connection, and then expires. It has no
bootstrap, Session, pairing authority, credentials, or other Host route.

The already-scanned disk view stays visible until a complete typed snapshot
arrives, so startup or worker recovery never flashes an empty sidebar. A
failed connection asks `HostServiceManager` to relaunch the machine service
with a bounded retry. `RemoteHostRuntime` never polls terminal output, marks a
Session read, fits a desktop, or sends terminal input for this Local transport.
Local rendering remains the direct `unpeel-attach` + `session.sock` data
plane. Provider lifecycle broadcasts are consumed only by the worker after
Local connects; the Swift `SessionActivityEngine` and its disk scan are frozen
as the no-flash startup fallback. The worker writes `activity-state.json` for
legacy/MCP readers and `activity-log.jsonl` for history; the app refreshes that
Host-published feed instead of appending duplicate edges. Native notification
delivery and other platform-owned capabilities remain adapters during this
transition, and project/worktree record creation still uses the sanctioned
locked shared-state path until matching Host verbs exist.
Since 0.4.0 this path is the release default in every build and channel,
and since 2026-09-03 it is the ONLY path: the compatibility Swift Host
(`MobileRemoteServer`, `RemoteControlManager`, `MCPBridge`, the Swift Relay
uplink, hook ingestion, provider hook-asset installs, and the store's
spawn/kill/reap/auto-archive half) is deleted, there is no Settings switch,
and the app never reads `UNPEEL_DEV_LOCAL_HOST_CLIENT` (the terminal UI that
once did was removed 2026-09-03).
`LocalHostClientFeature.resolveForLaunch` runs once per launch: it restarts
a stale service (`HostServiceIdentity`: version skew either way, a replaced
image of the bundled `unpeel-host`, or a pre-0.4.0 record —
identity-verified SIGTERM, at most once per launch, never a same-version
foreign service), starts the bundled service, and probes `host.sock` off the
main thread for up to 5 s. The outcome is only a status
(`HostServiceManager.serviceState`: `starting` → `live` or
`unavailable(reason)`), logged to NSLog and `hooks/trace.log`. A service
that never answers FAILS CLOSED: the app stays a client, the already-scanned
disk view stays visible, the content area shows a small non-modal "Host
service unavailable — Retry" banner ("Host service starting…" appears only
after 2.5 s of silence), Retry forces one relaunch past the cooldown, and the
store's Local connection loop keeps requesting the bounded relaunch. A
successful Local connection flips the state to `live` regardless of what the
launch probe concluded.

### Machine service and workspace workers

Bare `unpeel serve` (with no non-empty `UNPEEL_HOME`) acquires the machine
lease under the real `~/.unpeel`, ignoring any scoped workspace path. It reads
the compatibility registry `~/.unpeel/profiles.json`, always includes the
implicit Default workspace, and reconciles the registry once per second. A new
record gets a worker; an unregistered record's owned worker is stopped without
deleting that workspace's data. A failed worker is restarted after a bounded
delay. If an independently launched scoped Host already owns a workspace
lease, the machine service reports it as `external` and does not compete.

`unpeel --workspace NAME serve` and a direct `unpeel serve` invocation with a
non-empty `UNPEEL_HOME` intentionally run one foreground workspace Host. This
is the container/specialized service-unit form. It does not take the machine
lease or enumerate sibling workspaces.

The native `HostServiceManager` launches bundled `unpeel-host __serve__` and
does not retain or terminate it; the service survives app/window exit. The
lease makes simultaneous launches harmless. Launch rules are load-bearing:

- the default app instance and a registry-backed workspace instance remove
  `UNPEEL_HOME`, so every app instance addresses the same machine service;
- an unregistered dev/blank home preserves `UNPEEL_HOME` and gets one scoped
  worker, keeping test state out of the real registry;
- `UNPEEL_TEST_*` and `UNPEEL_SNAPSHOT*` launches never start a persistent
  service;
- stdout/stderr are null for app launches, so durable trace logging below is
  mandatory.

### Leases and status files

All leases use a non-blocking advisory `flock`; status JSON is written
atomically and removed only by the PID that published it.

| Scope | Lease | Status | Meaning |
| --- | --- | --- | --- |
| Machine | `~/.unpeel/host-service.lock` | `~/.unpeel/host-service.json` | One supervisor, its executable/PID/start time, and every desired workspace/worker state. |
| Workspace | `<home>/serve.lock` | `<home>/serve.json` | One semantic Host runtime for that home, hook port, local socket, Direct/Link ownership, and native handoff state. |
| Session | `<home>/app-sessions/<id>/session.sock` plus manifest PID identity | `<home>/app-sessions/<id>/manifest.json` | One persistent hosted PTY. This is independent of both serve leases. |

`SIGINT` and `SIGTERM` stop a foreground supervisor or scoped worker cleanly.
The supervisor sends `SIGTERM` to only the child workers it spawned, waits a
bounded grace period, then kills/reaps a child that did not exit. It never
signals an `external` workspace owner. Stopping the service removes serving
status/sockets but deliberately leaves Session hosts running.

### Local Host contract and pairing control

Every worker exposes the same bounded `UPL1` framed Host contract used by the
SSH stdio gateway. The preferred endpoint is `<home>/host.sock`. macOS Unix
socket paths are capped near 104 bytes, so a deep workspace home uses the
stable same-user fallback
`/tmp/unpeel-host-<uid>-<home-hash>.sock`; `serve.json.localSocket` is the
authoritative resolved path. The socket is mode `0600`.

`LocalProcessConnection` keeps its released ABI by spawning a tiny
`unpeel-host __remote_stdio__` compatibility child with
`UNPEEL_LOCAL_GATEWAY=1`. If the worker socket is reachable, that child is only
a stdin/stdout byte proxy; it does not instantiate another
`ControllerHostRuntime`. If no worker exists, it falls back to the historical
on-demand semantic gateway so older installs and narrow tests keep working.
The native Local client sets the additional private
`UNPEEL_LOCAL_HOST_REQUIRED=1` launch policy. That strict form fails closed
when `host.sock` is unavailable and can never instantiate the historical
in-child semantic Host; service recovery belongs to `HostServiceManager`.

The compatibility proxy and worker socket are persistent transports, not
one-request pipes. Accepted sockets must be put explicitly into blocking mode
on macOS, where a nonblocking listener can leak that flag to its accepted
socket. The proxy must flush every binary response chunk to stdout; a
line-buffered short effect receipt otherwise sits in the child until disconnect
and the Controller incorrectly reports a completed effect as outcome-unknown.

Most local requests retain `ControllerHostRuntime`'s disk-catalog and
generation-bound replay semantics. Two routes cannot be reconstructed from
disk: controller-assisted pairing and approval answers. The socket delegates
those directly to the worker's one live `PairingWindow` and `ApprovalHub`, the
same authorities Direct and Link use. A local client therefore cannot mint a
second pairing service or answer a stale frontend-owned approval queue.

### Native platform capability registration

Platform behavior is injected into the worker over the same mode-`0600`
`host.sock`, never inferred from the operating system or Host kind. A native
process keeps one framed connection open and registers through the reserved
`POST /_unpeel/platform-adapter` control route. Version 1 carries a bounded
`instanceID`, loopback `callbackPort`, ephemeral `callbackToken`, and an exact
list of allowed capability ids. The worker accepts only native-only operations
from the canonical Host capability ledger plus a bounded allowlist of
Host-internal services that are actually delegated by its router.

Registration is scoped to that socket connection. Disconnecting the app,
restarting the worker, or failing the callback transport withdraws the
registration immediately. The reconnecting native bridge registers again
after a worker/service restart and sends status heartbeats while connected.
The worker dynamically adds only the live subset to bootstrap
`hostProtocol.capabilities` and records it in `serve.json.platformCapabilities`;
Controllers never probe a route or branch on Host kind.

Calls go from the worker to
`POST http://127.0.0.1:<callbackPort>/_unpeel/platform-adapter/call` with the
ephemeral bearer. The callback envelope is versioned and names one operation;
the app validates its bounded type before applying the platform effect; the
Rust Host retains resource ownership and semantic validation. Three public
operations and ten internal services/ownership markers currently cross this
boundary:

- `session.notify_when_done.set` keeps Local UI intent on the ordinary Host
  verb while only the native preference half runs in Swift;
- `push.register` keeps the authenticated `/mobile/push-token` route in the
  Rust Host and delegates its locked APNs-token persistence to the app;
- `relay.credentials.recover` keeps the authenticated public route in Rust
  while native atomically rotates the device's Relay token and E2E key across
  Keychain plus the shared locked authority file.
- `notification.deliver` keeps lifecycle edges, observation policy, unread,
  exact viewing-device suppression, and phone targeting in the Host while the
  app performs only macOS banner and APNs delivery.
- `approval.present` mirrors the Host's bounded approval queue into native
  presentation. Answers return through the generation-bound Host approval
  verb; Swift is not another approval authority.
- `computer.status` returns only bounded availability/readiness/reason fields
  from the app-owned macOS Cua Driver. The worker polls it asynchronously,
  binds every result to the exact adapter generation, and publishes the same
  state into its Direct/Link snapshot and independently generated local
  `host.sock` bootstrap. Dropping the app connection immediately withdraws the
  ready state. Cua Driver execution, TCC prompts, and the responsibility chain
  remain in the app; no Computer action crosses this callback.
- `overlay.snapshot` projects an exact allowlist of workspace-scoped native
  UserDefaults as one bounded plist. The worker refreshes it asynchronously,
  binds the result to the adapter generation, and caches it for sidebar,
  bootstrap, `/app-theme`, and `/app-context` reads; hook request threads
  never wait on the app or inspect another workspace's defaults domain.
- `overlay.project-color.set` keeps project lookup, main-project eligibility,
  color validation, compound-effect ordering, and the Controller response in
  Rust while the app writes only its workspace-scoped UserDefaults value.
  Both local `host.sock` and Direct/Link routes use this same seam. An
  adapter-free default-macOS Host retains the historical `defaults` writer
  for app-less operation; isolated workspaces require their live native
  adapter.
- `artifact.thumbnail` is optional ImageIO enrichment after Rust has already
  authenticated the Controller and validated/read the selected Session
  artifact. Rust then requires the returned chunk to match the exact Session,
  kind, name, range, size, and JSON schema; a missing, failed, or malformed
  adapter falls back to the original Host-owned bytes.
- `link.entitlement.refresh` sends only the public Host id to the app. Swift
  reads the legacy license key from Keychain, uses the existing service and
  shared-authority transaction, and returns only `{available}`; the Host
  independently verifies that a fresh allowed cache revision was committed
  before starting Link. Neither the key nor bearer crosses the callback.
- `mobile.e2e-key.reconcile` mirrors worker-created shared phone E2E revisions
  into the workspace-scoped Keychain account and removes a revoked mirror
  only after a fresh locked authority read proves the device is still absent.
  `devices.json` remains authorization authority, so stale callbacks cannot
  delete a newly re-paired device's key.
- `app.open-in-editor` keeps the hosted-App route and Session/path validation
  in the worker, then delegates only the existing macOS preferred-opener
  effect. It is absent on a headless Host and does not make Swift another App
  router.
- `controller.transport.host-owned` is a live ownership marker rather than a
  callback. While registered, the worker keeps pairing, Direct, Link, and its
  TLS streamer across an adapter reconnect.

The overlay, Link, and Keychain maintenance callbacks run off the Host tick;
connection generation changes discard late results. Adapter timeout or app
exit cannot block snapshot publication, Session PTYs, or headless operation.

Both public phone operations derive `deviceID` from the authenticated paired-device
principal; the request body cannot select another device. Adapter-free Hosts
continue to omit the capabilities and answer 404. Ordinary Session validation,
mobile authentication, and route ownership remain in Rust. The direct terminal
data plane never crosses this callback.

The reserved framed route `POST /_unpeel/pairing` is same-user local control,
not part of the remote Host capability ledger. It accepts `begin`, `status`,
`cancel`, `devices`, `revoke-device`, and `set-relay-allowed`, allowing the CLI
and native client to manage pairing inside an already-running worker without a
second Direct listener or service restart. Device listings are bounded and
sanitized: pairing hashes, bearer material, Relay credentials, and E2E keys
never cross this management response.
Remote Controllers continue to use the ordinary authenticated pairing
protocol; they never see this control route.

### Environment boundaries

- `UNPEEL_HOME` selects one workspace for a worker and every child it owns.
- The machine supervisor uses `app_paths::real_unpeel_home()` so registry and
  machine coordination never drift into a selected workspace.
- Default workers have `UNPEEL_HOME` removed; named workers receive the
  absolute registry home.
- Worker/session spawning preserves the existing `UNPEEL_*`/`HERDR_*`
  containment rules. Never switch homes in-process.
- `UNPEEL_LOCAL_GATEWAY=1` is private to the app's loopback compatibility
  gateway and must not be forwarded over SSH.
- `UNPEEL_DEV_LOCAL_HOST_CLIENT=1` selected the interactive terminal UI's
  now-removed loopback Controller before any compatibility Host ownership
  started. It was a development/conformance gate, never a documented release
  setting, and no longer has a consumer.

### Durable diagnostics and debugging

Machine-service and Default-worker lifecycle events append to
`~/.unpeel/hooks/trace.log`. A named workspace worker writes to
`<home>/hooks/trace.log`. The same trace carries hook, Direct-path, and Relay
diagnostics; core rotates it at 10 MiB to one `trace.log.1` generation. This is
the diagnostic source for an app-launched service because its stdout/stderr
are intentionally disconnected.

Useful read-only checks:

```sh
cat ~/.unpeel/host-service.json
cat ~/.unpeel/serve.json
tail -f ~/.unpeel/hooks/trace.log
ps -axo pid,args | grep -E '__serve__|__serve_workspace__|__session_host__|unpeel-attach' | grep -v grep
```

For a named workspace, read its `home` from `profiles.json`, then inspect that
home's `serve.json` and `hooks/trace.log`. `host-service.json` may briefly say
`starting` between a child spawn and its first atomic `serve.json` publish.
The complete manual stop/recovery commands live in the website troubleshooting
page; never delete a workspace merely to clear a stale process.

### Service packaging and boot units

`unpeel serve install|uninstall|status` (implementation:
`crates/unpeel-serve/src/service_install.rs`; CLI notes:
`docs/agents/cli.md`) wraps the checked-in unit templates in
`packaging/service/` — a macOS per-user LaunchAgent and Linux systemd
`--user` units usable verbatim in containers/golden images with `unpeel` at
`/usr/local/bin/unpeel`. Always per-user, never a root daemon: the service
owns `~/.unpeel`, the user Keychain, and the per-user machine lease, so a
headless Mac needs auto-login and a headless Linux user needs
`loginctl enable-linger`. Machine scope installs `com.unpeel.serve` /
`unpeel-serve.service`; a registered workspace home installs a scoped
`--workspace NAME serve` unit. Uninstall stops the managed service and
removes only the unit file — never workspace data, and Session hosts keep
running. Scripted Link activation for the same provisioning lane is
`unpeel link enroll <key>` (shared `unpeel_core::license` implementation
with the interactive path — see `docs/agents/cli.md` for the invariants).

### Change gates

Serve lifecycle, protocol, or launch changes must run these gates serially
(the process suites share ports):

```sh
(cd crates && cargo test --workspace)
(cd crates && cargo test -p unpeel-cli --test serve_command --test unified_service)
crates/unpeel-cli/tests/run.sh
unpeel-apple:apps/native/build-rust-bridge.sh debug
(cd unpeel-apple:apps/native/UnpeelNative && swift build)
scripts/verify-attach.sh
```

The full `crates/unpeel-cli/tests/run.sh` matrix (20 cases) is required,
including `compat_bridge`, `compat_state`, and `compat_serve`; do not replace
it with a filtered run. `compat_serve` guards version skew against the last
shipped TUI-era 0.4.3 archive (cached, sha256-pinned,
`UNPEEL_MATRIX_COMPAT_ARCHIVE` override, SKIP with NOTE when unavailable) —
the interactive terminal UI and its own `compat_standalone.py`/
`compat_mobile.py`/`compat_pairing.py`/`compat_approvals.py` cases were
removed 2026-09-03. The focused real process proofs must continue to show:

- one machine service supervises Default plus multiple registry workspaces;
- duplicate machine/workspace leases fail cleanly;
- every local socket answers a typed bootstrap;
- pairing opens through the live worker;
- canonical `standalone`, `mobile`, `pairing`, and `approvals` behavior runs
  directly on serve with no terminal UI in its Host process tree;
- a native loopback gateway proxies the persistent worker instead of starting
  a second semantic Host;
- `host_launch_conformance` runs every case in
  `protocol/host-conformance-v1.json` through both the exact native app launch
  (`unpeel-host __serve__`) and the headless CLI launch (`unpeel serve`), and
  resolves a real worker-owned approval over each `host.sock`;
- a live platform registration adds its exact operation to bootstrap, invokes
  the authenticated callback, and withdraws it when the socket closes;
- the Swift listener rejects every Host route while retaining its exact
  platform/frontend callback surface (`HookServerParsingTests`,
  `PlatformAdapterCallbackTests`);
- the Controller pairing proxy completes one real sealed phone exchange,
  rejects replay, and exposes no Host bootstrap route;
- `SIGTERM` removes service/worker status and sockets without ending Session
  hosts;
- app-style null stdio still leaves durable trace diagnostics;
- `serve_install` drives install/uninstall/status through fake
  `launchctl`/`systemctl` shims (never a real service registration), and
  `link_enroll` proves scripted enrollment shares the interactive path's
  durable suppression/pending semantics and that a live serve starts Link
  from it without restart;
- `remote_streamer_supervision_process` respawns a killed `__remote__`
  streamer under a running worker with no service restart, re-advertises it
  in bootstrap, and holds a crash loop at the ceiling until pairing changes;
- `verify-attach.sh` preserves replay, live echo, and reattach independently
  of the serving lifecycle.

Website documentation and `unpeel-apple:docs/plans/headless-serve.md` must stay aligned
with this file, but this `docs/agents/serve.md` is the authoritative current
subsystem map. Directional app-as-client migration belongs in the plan until
each increment ships.
