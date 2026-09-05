<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Remote Control Server (feature-flagged, dark)

An HTTPS+WSS layer over the hosted-session artifacts so phones/other Macs can
list sessions, stream terminal output, send input, resize, and kill. Current
contract: the route and wire-format headers in `remote_server.rs`.

> **Hosts are not only desktop apps (updated 2026-08-10).** `unpeel serve`
> (previously also served by the interactive TUI, removed 2026-09-03) serves
> the same `/mobile` protocol app-lessly and pairs phones with the same
> crypto — on a headless box it *is* the terminal server. It now supervises
> `__remote__` (since 2026-09-02 with exit detection, backoff respawn, a
> crash-loop ceiling, retry on pairing change, and `pid_started_at`-verified
> stale reaps — `docs/agents/serve.md` "Terminal streamer supervision") and
> has a Rust relay uplink that re-announces a rotated device token in place
> instead of evicting every phone (same file, "Link uplink: token rotation
> without eviction"). Its core Session route behavior is
> now conformant; push registration, Relay credential recovery,
> `notifyWhenDone`, and the credential/entitlement lifecycle are not yet at
> native-app parity. Anything added here must work for a headless host too,
> since controllers cannot tell the difference. Design records and plans live in the private archive repository.
>
> **Native Controller status (2026-08-11, branch only):** the first App→TUI
> (TUI since removed 2026-09-03) and App→App paired Direct → Link slice is
> implemented: shared macOS pairing and the
> Share This Mac/Add Host picker, a direct `HostConnection` and panic-contained
> native bridge, plus the shipped iOS Relay client/E2E handshake exposed from
> `UnpeelShared` through a generation-bound Link `HostConnection`,
> Host-scoped sidebar/runtime, remote-only in-memory Ghostty panes,
> commit-gated bounded output, ordered at-most-once input, desktop fit/clear,
> mark-read, and explicit connection states. `unpeel pair --serve` pairs a
> Controller against the running `unpeel serve` (this originally read "opens
> the Host TUI"; the TUI is gone). This is not the secure-direct exit:
> the desktop data path is bearer-authenticated **plaintext HTTP** on a trusted
> LAN/VPN and ignores the certificate pin. It reconnects only to the persisted
> trusted endpoint, and the picker is visible only in explicitly branded
> **Unpeel Dev** bundles. Automatic Bonjour rediscovery is disabled until pinned TLS
> or another proof-of-possession can authenticate a candidate without exposing
> the saved bearer. For an already-paired Host, the runtime falls back to Link
> only on Direct reachability failure, probes back to Direct, and shows only
> **Direct** or **Via Link**. Link currently uses the shipped legacy entitlement
> and pairing credentials—not target Link accounts, assertions, or rendezvous.
> Full remote verb parity and physical two-Mac/release QA remain (the TUI's
> own direct/Link selection is moot: the TUI was removed 2026-09-03).

### Versioned Host capabilities and conformance

`/mobile/bootstrap` now carries an optional `hostProtocol` descriptor:

- `majorVersion` must match the Controller's supported major;
- `minorVersion` is additive;
- `capabilities` is a sorted set of stable operation ids; unknown ids are
  ignored and absence means the operation must not be offered;
- a missing descriptor identifies a legacy Host, so shipped v1 Controller
  fallback behavior continues to work.

The canonical machine-readable ledger is
`protocol/host-capabilities-v1.json`. Native and headless capability constants
are tested against it; the current additive level is minor 8, including
`project.pin.set`, `session.runtime.resume`, and `pairing.invitation`. Project
summaries advertise additive group `pinned` state. Current Session summaries
advertise `resumeAgent` and current clients send `resume_agent`; legacy
`restartAgent`/`restart_agent` decoding is
compatibility-only. Session summaries also expose additive
`runtimeLaunchPending` (disk `runtime_launch_pending`); omission from an older
Host decodes as `false`. Controllers suppress `resumeAgent` while it is true,
but the Host remains the final duplicate-submission authority.
`protocol/host-conformance-v1.json` is executed against
both authenticated route adapters, and
`protocol/host-bootstrap-compatibility-v1.json` covers legacy omission, future
minor/unknown fields, and incompatible major versions. Add or change a Host
operation in the ledger and both adapters' fixtures in the same patch; a 404
probe is not capability discovery.

The native-only capability delta is explicit: push registration,
`notifyWhenDone`, and Relay credential recovery are dynamically advertised
only while the connection-scoped native adapter is live. Their authenticated
public routes now live in the canonical Rust Host and have real worker/process
coverage; an adapter-free headless Host still omits them and answers 404.
Session
creation, title organization, Session and plain-group pinning, in-place Resume
Agent, and lifecycle stop/terminal-restart/remove now have real headless effects and common
conformance. Archive listing is a shared
router operation too: both adapters use the same validation/envelope, and
`unpeel serve` publishes every archived row in native-compatible newest-first
project buckets. Transcript Markdown, artifact list/read/delete, and the typed
screenshot request work on both Host implementations. Original artifact byte
ranges now use the shared no-follow reader; positive `max_dim` thumbnail
generation remains a native in-memory ImageIO enrichment sourced only from
those secured bytes. `unpeel serve` advertises none of the remaining gaps
even where a compatibility route accepts a partial no-op. Phase 3 closes these through the
shared Host router.

The shared router lives in `unpeel-core::controller_api`. It defines the
transport-neutral authenticated request/response envelope (including binary
bodies), owns bootstrap protocol metadata, and owns read-only terminal metrics
plus transcript Markdown, archived-session listing, raw terminal write/resize,
typed screenshot requests, read receipts, and artifact
list/read/resumable-upload/delete. It also owns the typed headless lifecycle
effect boundary for shell-only Resume Agent, terminal/session restart, and
session-action stop/restart/remove. The legacy
one-shot native upload remains a compatibility exception. `unpeel serve`'s
`/mobile` adapter sends those operations through the router; the Rust secure server uses
the same metrics operation while preserving its shipped `/api/*` response
shape. The native Host enters it after
bearer authentication through the panic-contained JSON C ABI in
`unpeel-native-bridge`. Authentication, HTTP/Relay framing, Keychain/UI work,
and platform enrichment remain adapter concerns; Swift falls back to its
existing handlers for unmigrated routes. In particular, native deliberately
supplies no Rust lifecycle effects yet, so the router returns unhandled and
Swift performs its richer cleanup. Original artifact reads are shared; native
bypasses the outer router only to derive its optional in-memory ImageIO
thumbnail from Rust-supplied bytes.

Session creation is also an attribution boundary. `ControllerPrincipal`
separates a stable `principal_id` (the human) from `device_id` (one revocable
credential). The shared create resolver ignores ownership-like request-body
fields and stamps `owner_principal_id`, `created_by_device_id`, and the
Host-resolved `source_preset_id` only after authentication/catalog resolution.
Native's compatibility creator receives the same authenticated principal.
Old device records and owner transports resolve to `host-owner:<host-id>`.
The resulting optional fields are exposed in Session summaries; older
Controllers ignore them and older summaries decode without them.

Raw terminal writes are an at-most-once boundary. Link request ids survive the
native and `unpeel serve` adapters; the shared router keeps a bounded five-minute,
per-principal replay cache and returns the first response for an identical
resend instead of applying the mutation twice. A two-second Host command
timeout bounds wedged sockets, but timeout/response loss is still ambiguous:
the PTY may already have received the bytes. Native therefore never replays an
uncertain operation through its Swift compatibility handler (replay safety is
classified per route, because Relay credential recovery is a mutating GET).
Controllers must not invent a new id and automatically retry a failed raw
write; cache loss across a Host restart also remains effect-unknown.

Headless lifecycle requests use the shared stable-id single-flight cache. The
native Swift compatibility boundary now applies the same rule with a bounded
five-minute, per-device cache before invoking its richer lifecycle cleanup:
an identical request-id retry receives the exact first receipt, a concurrent
retry joins the first operation, and reusing an id with different content
receives `409`. Legacy Direct requests without an id remain compatible but do
not claim replay safety. The shared contract is malformed request `400`,
unknown Session `404`, stopping an already-exited Session `409`, adapter
failure `500`, and `{ "ok": true }` on success. Per-session cross-process
locks serialize lifecycle changes. Legacy
terminal/session restart re-points title/custom-title, Session pin metadata
and its `pinned_at` timestamp, manual order, Sessions MCP grant and directional
write approvals, and Browser/Computer approvals; archive state is intentionally not
carried. The additive `resume_agent` action instead keeps all of that state and
the existing Session/PTY. It verifies that the original owned interactive login
shell has the foreground and that no retained expected runtime,
stopped/background job, or different recognized runtime remains, then injects
the Host-derived resume command. It publishes `runtimeLaunchPending` before the
PTY submission; an active runtime, pending launch, blank terminal, ambiguous
process scan, or retained/different job returns `409`. Stopped-Session Resume
refreshes a stale `running` manifest under the same lifecycle lock and replaces
it only when the child is definitively absent or its PID was recycled. Healthy
or unknown-live ownership returns `409` and is never torn down.
Remove prunes shared references.

Artifact list/read/delete use directory-handle-relative `openat`/`unlinkat`
with no-follow semantics on Unix. Original reads anchor at the configured
app-sessions root and reject symlinked Session, artifact, kind, and leaf
components; native thumbnails are derived only from those bytes. A Controller
therefore cannot redirect a read or mutation outside the Host-owned tree.

Resumable artifact upload landed on 2026-08-11. The native-only compatibility
route remains exactly `POST /mobile/upload`; Controllers use the new
`artifact.upload.resumable` capability and `POST /mobile/upload-chunk` only
when advertised. Each raw JPEG/PNG chunk is at most 256 KiB and the complete
image at most 4 MiB. Requests carry a stable UUIDv4 upload id, exact offset,
total size, whole-file SHA-256, and MIME type. The Host binds durable hidden
staging state to the authenticated principal, accepts an identical committed
range as a no-op, rejects gaps/partial overlaps/conflicts, validates the whole
digest and file signature, then atomically publishes one server-named file
under the existing Session's `artifacts/uploads/`. The receipt survives Host
restart and is independent of the router's short replay cache.

The storage walk is directory-handle-relative and no-follow on Unix. A real
matching Session manifest is required before the Host may securely create the
`artifacts/uploads` leaves; there is no dropped-images or cloud fallback.
Incomplete uploads are bounded by per-Session count and declared-byte quotas.
One per-Session upload lock serializes chunks, quota decisions, and cleanup;
staging with no accepted activity for 24 hours is securely expired on the next
upload request, while complete and failed receipts retain their id binding.

The Rust relay adapter now preserves tunneled `contentType` and treats the
shipped phone's `auth` value as the complete `Authorization` header, with
shipped-Swift conformance guarding both. Frame safety is also enforced at the
ends: the iPhone preflights the complete encoded request before sealing and
returns a local data-length error when it cannot fit; both Host implementations
replace an oversized encoded route response with a small same-request-id `413`,
and native drops oversized relay pushes before sealing.

On the Rust headless Host, decrypted routes run through a bounded concurrent
dispatcher, so an output long-poll cannot block terminal input or resize on
the same Link socket. One relay owner still performs every E2E open/seal and
WebSocket send in order. A full dispatch queue yields a correlated `503`, and
results from a superseded connection generation are discarded after reconnect.

### SSH stdio Host gateway

`unpeel-host __remote_stdio__` is the free, accountless Host-side transport
used by native App Controllers (originally also the interactive TUI, removed
2026-09-03). Standard system
SSH launches it as `ssh -T host unpeel-host __remote_stdio__`. It is an
on-demand gateway over the same Host contract, not a daemon, session owner,
second route dialect, TCP listener, or Relay feature.

(The now-removed TUI Controller transport required a non-interactive remote
command.) The native Controller also has a deliberate interactive-shell compatibility mode
for managed SSH services: it allocates a PTY, sends a fixed non-interpolated
gateway command, waits for a random marker on its own line within a bounded
64-KiB/20-second preamble, switches the remote PTY to raw/no-echo, and then
hands the exact same framed Host protocol to `RemoteSessionBackend`. Upstash
Box was verified in this category. Passwords/provider API keys are stored in
Keychain and supplied by the embedded `unpeel-host` SSH_ASKPASS helper; they
never enter SSH argv, UserDefaults, Host metadata, or diagnostics.

The Add Host sheet accepts only `user@host` or an SSH config alias. The system
SSH client retains the user's config, key/agent, ProxyJump, and VPN while
Unpeel uses `StrictHostKeyChecking=accept-new` (new keys are recorded; changed
keys fail), and disables agent/X11/credential forwarding and arbitrary local
commands. The native Controller probes command mode first, falls back to
interactive mode, bootstraps and pins the Host identity before saving the row,
and reconnects using the successful mode. (The now-removed TUI's
`unpeel --host ssh://…` continued to require ordinary remote-command
support; that dispatch mode no longer exists.)

Each frame uses `[UPL1][kind u8][flags u8][reserved u16][length u32 BE]`
followed by a payload within the shared Relay plaintext bound. Kind `1` is a
request and kind `2` a response; flags/reserved are zero. Payloads reuse the
strict Relay tunnel JSON envelopes without Relay AEAD. Stdout is protocol-only
and diagnostics go to stderr. Eight bounded workers allow request ids to
complete out of order without an output wait blocking input/bootstrap; the
writer serializes and flushes every frame. Correlated malformed envelopes,
queue saturation, and oversized responses become `400`, `503`, and `413`;
uncorrelatable or malformed framing closes the process.

SSH authenticates the remote Unix account. The Host always injects
`OwnerTransport { transport: "ssh", subject: "uid:<effective uid>",
principal_id: "host-owner:<host-id>" }`; wire
`auth`, `$USER`, and other client-controlled identity fields cannot select a
principal. Start/stop records use the shared rotating audit log at
`~/.unpeel/remote/audit.log`, including the effective Unix identity and the
diagnostic remote address but no request body or credential.

The gateway currently builds a capability-advertised subset from Host-owned
disk state and control sockets. Pairing, approval queues, and WSS output
subscription are omitted; output uses bounded cursor long-polling. Session
creation is real, installs normal Host-side assets, and broadcasts hooks to
any registered app listener, but the gateway does not itself own a live
hook/approval listener when no frontend runs. Native UserDefaults overlays
also remain outside this disk adapter. The process integration test covers
bootstrap, large output paging, concurrent waits, stable owner identity,
malformed-request recovery, real create/remove, clean EOF, and audit privacy.

The Controller-side `unpeel-core::SshHostConnection` invokes `/usr/bin/ssh`
with fixed, structured, noninteractive arguments and a validated config alias;
it never invokes a local shell or interpolates the target into the fixed remote
command. Calls receive connection-owned one-use ids, are bounded in flight,
and complete out of order. A watchdog covers writer-lock wait, frame write,
and response; stderr is drained into a bounded diagnostic tail, and disconnect
kills/reaps the child and permanently closes that connection. Transport loss
never replays a call. Every successful reply carries an opaque connection-
generation token. A semantic backend may open a generation only with an
unconstrained bootstrap; every later read or effect must bind to the token from
that accepted bootstrap. If the SSH process died or was replaced while idle,
`prepare_in_generation` returns `GenerationChanged` + `NotSent` without
spawning or writing. A semantic read loop can then explicitly bootstrap and
resume output from its committed cursor; an effect is outcome-unknown after
any request byte was written.

### Paired direct/LAN Host connection

`unpeel-core::DirectHostConnection` is the first non-SSH desktop Controller
transport beneath the same `HostConnection` contract. It accepts only the
exact `http://HOST[:PORT]/mobile` endpoint emitted by pairing, applies the
per-device bearer at the transport boundary, bounds requests/responses and
in-flight calls, and binds reads/effects to an opaque logical connection
generation. Transport loss invalidates that generation. Effects are attempted
once; failures after request bytes may have left the Controller are outcome
unknown and are never automatically replayed.

This v1 direct transport is intentionally honest about its trust boundary: it
is plaintext HTTP for a trusted LAN or trusted VPN. It neither upgrades to nor
uses the separately advertised certificate pin for `__remote__` TLS/WSS. It
must not be described as secure direct networking or Phase 5 completion.
Likewise, automatic Bonjour endpoint adoption remains disabled: a spoofed TXT
record can claim a stable Host id, and probing that plaintext candidate with
the saved bearer would disclose the credential. Re-enable rediscovery only
after the candidate proves the paired Host key through pinned TLS or an
equivalent proof-of-possession exchange.

**iPhone/iPad Direct `/mobile` over pinned HTTPS (iOS build 23).** The
phone's `RemoteMacClient` no longer sends the bearer in plaintext to a Host
that serves TLS on its `/mobile` port. The decision is per paired Host and
persisted on `PairedMacRecord.directTLSFingerprint`
(`RemoteDirectTransport.swift`): the Host advertises capability
`host.mobile.tls` in `hostProtocol`, or a `serverVersion` at/after `0.5.3`
(both additive bootstrap/pairing-response fields), and the pin is the same
`remoteServerCertificateFingerprint` the `__remote__` WSS stream already
verifies, through the same `RemoteCertificatePinningDelegate`. Every Direct
client is built by `PairedMacRecord.directClient(token:)`; a pinned client
rewrites the stored `http://` endpoint to `https://` on the same port per
request (`RemoteMacClient.directRequestURL`). Learning order: a sealed pairing
response that carries the version pins before the first bearer request; an
E2E Relay or pinned-TLS bootstrap is authenticated and may set or clear the
pin; a plaintext LAN bootstrap may only upgrade to TLS, never downgrade. A
transition-era Host that answers a plaintext bearer with `426` (or a `401`
naming https/TLS) triggers `RemoteConnectionStore
.upgradeDirectTransportAfterPlaintextRefusal`, which pins and republishes a
new Direct generation instead of painting an outage. Hosts that report
neither signal (≤ 0.5.2) stay on plaintext HTTP exactly as before. Pairing
itself is unchanged (sealed exchange over the scanned endpoint).

**iOS Link stability (same build).** Over the relay the bootstrap deadline
is `RemoteBootstrapDeadline` (10 s, scaled up to 20 s by the connection's
last measured round-trip) instead of the 4 s LAN health budget; a request
that misses its deadline retires the relay socket only when the socket has
also been silent past the keepalive cadence
(`RemoteRelayRequestExpiryPolicy`, 20 s), never on one slow Host reply; the
connection store keeps one Link connection per active Host and reuses it
across fallback attempts and push-token registration
(`PushTokenRegistrationRoute`: the active Host on the relay registers over
the live connection only — no 10 s LAN wait, no throwaway second socket);
and terminal surfaces stage their output stream behind the current client
generation's first bootstrap (`RemotePreviewStore.hasBootstrapForCurrentClient`).

On macOS, Add Host uses the shared `UnpeelShared` sealed pairing exchange,
stores command credentials in Keychain, and records non-secret Host metadata
separately. The native bridge opens the Rust direct connection and exposes the
shared semantic backend without moving credentials into Swift route logic.
The same sidebar menu exposes **Share This Mac…**, which mints the app Host's
one-time code directly—there is no Host mode and no Mobile detour. Nearby
discovery filters the current logical Host id, while `RemoteHostStore` also
rejects a pasted self-code and removes legacy self-pair records before they can
be selected.

For a headless first pairing, `unpeel pair --serve` closes the one-shot HTTP
exchange, preserves the exact canonical Host port, and hands that endpoint to
`unpeel serve` (originally the interactive TUI); it refuses to pair through a random fallback while an
existing canonical endpoint is occupied. That is continuity for this
trusted-network slice, not Host authentication—the pinned transport remains
the security completion.

When the Mac app is already scoped to a paired remote Host, **Pair Another
Controller…** uses capability `pairing.invitation`. The Host opens its normal
single-use pairing window, but binds the sealed request/response to a random
short-lived `/mobile/pairing-proxy/<id>` endpoint on the Controller Mac. The
phone reaches that Mac once; the Mac forwards the still-sealed envelope over
its accepted, generation-bound Direct or Link Host connection. Only the Host
mints and persists the phone bearer, E2E key, and Link token. The sealed reply
also carries the Host's real Direct endpoint, which the phone stores instead
of the proxy URL before entering ordinary Direct → Link routing. This is
controller-assisted pairing, not an operated Relay invitation: the phone must
reach the Controller Mac over LAN/VPN for that one exchange, and the Mac sees
the displayed one-time QR secret even though the implementation does not open
the forwarded envelope.
The provider-neutral setup guide, protocol sequence, trust boundary, and
troubleshooting reference live in
`docs/agents/controller-assisted-pairing.md`.

`RemoteHostRuntime` keeps remote bootstrap/sidebar state separate from Local,
owns the connection and FIFO effect lifecycle, retains the last valid snapshot
while reconnecting, and never falls back to Local execution. Remote terminals
use Host-and-Session-keyed in-memory Ghostty panes rather than launching a
local `unpeel-attach`/hosted Session. Output pages advance their Host cursor
only after the attached pane accepts the full byte sequence; reset pages reset
the local VT first. Input, fit/clear, and mark-read remain generation-bound and
at most once.

The paired-direct native slice currently covers bootstrap/sidebar state,
output, input, desktop fit/clear, mark-read, Session create/lifecycle and
organization, transcript/archive reads, and artifact operations. Remote Host
settings and preset editing, native SSH, pinned
TLS/WSS, target Link identity/rendezvous, and physical two-Mac/release QA
remain. (The TUI's own Direct/Link selection is moot: the TUI was removed
2026-09-03.)

### Native Mac Link downlink and automatic route selection

The Mac Controller does not implement a second Relay client. The shipped iOS
`RemoteRelayConnection` now lives beside `RelayProtocol` in `UnpeelShared`.
Both clients therefore use the same outbound WebSocket, authenticated
forward-secret handshake, AEAD framing, limits, push handling, and canonical
Relay tunnel DTOs. A callback-backed native bridge adapts that shared Swift
actor into `unpeel-core::RelayHostConnection`; semantic state and effects
remain owned by the same Rust `RemoteSessionBackend` used by Direct and SSH.

Link calls bind to the socket generation accepted by bootstrap. A bound call
cannot silently open or cross onto a replacement socket, and an effect that
may have been sent is outcome-unknown and never replayed. For paired Mac
scope, the saved `HostRecord.hostID` is a required expected bootstrap identity
on Direct and Link. A missing or different id fails closed before state or
effects are accepted. The paired Relay static key authenticates the E2E
handshake; `certificateFingerprint` remains the pin for the still-unfinished
secure Direct TLS transport and is not reinterpreted as a Relay key.

Route choice is silent in the normal product UI. Selecting a Host starts at
its persisted Direct endpoint. Direct gets a 750 ms head start; if its
bootstrap is still blocked, an identity-checked Link probe starts concurrently
instead of waiting for the Direct request's full deadline. Direct cancels the
speculative Link probe when it wins. Only reachability failures can move the
same Host scope to Link; authentication, Host identity, protocol, capability,
and semantic Host failures do not qualify. While connected Via Link, the
runtime periodically probes Direct with a separate backend and promotes it
only after an accepted identity-checked bootstrap. Explicit disconnect/Host
switching waits for an in-flight effect tail before retiring its transport.
The Host's existing **Access away from home** opt-in owns the uplink; the
Controller has no Relay toggle and reports only **Direct** or **Via Link**.

Deterministic development/QA can launch a picker-enabled **Unpeel Dev** build
with `-unpeel.native.forceLink YES`. That forces the selected paired Host onto
Link so the downlink and route UI can be exercised without making transport
choice a customer setting; non-Dev/picker-disabled builds ignore the override.

This branch still uses the shipped legacy Ed25519 entitlement and credentials.
Unpeel Link browser/device sign-in, account seats, short-lived assertions on
both Relay sides, Host publication/rendezvous, headless-serve downlink
(previously also a TUI downlink, removed 2026-09-03), and production
rollout are defined by the Link service contract rather than this adapter.

### Transport-neutral Controller session backend

`unpeel-core::remote_session_backend` is now the shared semantic
Controller layer above `HostConnection`. It decodes the shipped mobile-v1
bootstrap into typed projects, presets, Sessions, activity, capabilities, and
approvals; requires a compatible advertised Host major; validates a supplied
expected Host identity or otherwise pins the first advertised identity; and
preserves the raw bootstrap for forward-compatible callers. Paired native
scope always supplies the durable `HostRecord.hostID`, so missing identity is
a mismatch rather than a best-effort legacy acceptance. A missing Host
descriptor invokes only the documented legacy-v1
bootstrap/output fallback. Capability absence fails before a bound transport
call, and no 404 probe is used for discovery.

The backend retains the last validated snapshot for stale UI display while
tracking the callable connection generation separately. Output polls are
generation-bound, capped at 200 KiB with at most 25 seconds of long-poll wait,
and resume each Session from its own last committed `nextOffset`. An initial
tail may include the Host's 16 KiB escape/UTF-8 alignment allowance. Decoded
pages carry arbitrary terminal bytes and a renderer-reset flag when the Host
rebases or truncates. Feeding is explicitly two-phase: dropping/discarding a
page leaves the cursor unchanged, and `commit()` advances it only after the
renderer accepted every byte.

Transport loss, a replaced generation, malformed output, and non-success Host
responses are never retried inside the raw call. A later explicit poll may
single-flight a new bootstrap and then request the exact committed offset.
Fetching reservations from a lost generation are cleared; already staged
pages may still commit because their bytes were validated before replacement.

The first effect slice is also implemented: UTF-8 terminal write,
desktop-viewer fit/clear, and mark-read. Each call validates its exact
advertised capability, binds to the accepted generation, carries `Effect`
semantics, and is dispatched at most once. Terminal/effect dispatch is FIFO so
a multiplexed Host cannot apply later keyboard chunks ahead of an earlier one.
Only a `200` JSON receipt with `ok: true` is success. Validation, missing
capability, transport proof that nothing was sent, correlated semantic `4xx`,
and the transports' reserved pre-dispatch saturation `503` report **not
applied**; the backend does not retry them and keeps a healthy generation
callable. Uncorrelated loss, a post-send timeout, malformed/mismatched receipt,
and uncertain `5xx` report **outcome unknown**, invalidate the callable
generation, and are never retried or replayed by the backend. Output polling
allows the Host's full 25-second long-poll plus 10 seconds of transport
headroom so ordinary Link latency does not tear down a healthy socket.
Descriptor-less legacy Hosts remain read-only.

The module depends only on `HostConnection` and protocol types—there is no
local app-state, Session, hook, or filesystem fallback. (The now-removed TUI
selected it through strict `unpeel --host ssh://HOST`; that dispatch mode no
longer exists.) On this branch the native app selects it through the bridge
for its paired Direct/Link terminal slice. The
remaining lifecycle, organization, transcript/artifact, and settings
operations remain the next layer for remote desktop scope.

For a developer-only reachability check,
`crates/unpeel-core/examples/ssh_host_probe.rs` constructs that shared backend
over the production system-SSH connection, performs one validated read-only
bootstrap, and prints the remote Session list. From the repository root:
`cargo run --manifest-path crates/Cargo.toml -p unpeel-core --example ssh_host_probe -- ssh://studio`;
add `--json` for the full bootstrap JSON. The example is not installed as
the `unpeel` CLI and does not yet exercise attach, input, semantic reconnect,
or verbs; the native app's SSH workspaces consume the same connection for
their interactive slice (originally also the now-removed TUI Controller).

The process harness substitutes only the SSH executable, then runs the real
gateway. It proves exact argv, out-of-order bootstrap/output, blocked-pipe
timeout, explicit bootstrap plus cursor resume after killing the gateway, and
a generation-bound mutation that cannot cross idle process loss, including the
prepare→death→request race. It also proves a raw write reaches the Host control
socket once before its receipt is lost. Real-gateway cases now drive the
semantic backend through typed bootstrap and committed output at exact offsets,
plus write, desktop-fit, and mark-read. A self-spawned child starts with an
empty isolated Controller `HOME`, a nonexistent Controller `UNPEEL_HOME`, and
a distinct Host home; it proves the Controller stays untouched while
write/resize commands and the read marker land only on the Host. This is not an
actual sshd/two-machine proof. The `remote_host` PTY case drives the real
gateway (substituting only the SSH executable, no longer through the removed
`unpeel --host ssh://HOST` dispatch) and proves Host-only sidebar state,
in-memory VT output, ordered input, fit/clear, and blank Controller homes.
Focused backend tests cover commit-gated output, mark-read, reconnect, and
ambiguity halting. The native UI now consumes the backend for paired
Direct/Link bootstrap/sidebar, output, input, fit/clear, and mark-read
through remote-only Ghostty panes. Native SSH,
target Link identity/rendezvous, secure pinned direct
networking, and
remote lifecycle/organization/read verbs remain open, so the broader
four-quadrant and Phase 5 exits are not complete.

The Worker accepts at most 512 KiB from a Controller and 512 KiB + 5 bytes for
the Host's outer data envelope. A canonical forwarded Controller frame is at
most 512 KiB + 134 bytes; both Host implementations temporarily accept +139
bytes for rolling compatibility with an older Worker allowance. Automated
forced-Link conformance now sends a maximum 256 KiB chunk through the shipped
Swift crypto, retries it after a discarded application receipt, completes the
upload, and proves encrypted gallery list/read/delete with exact bytes and
MIME. Physical-phone/production-Relay QA remains a release gate, not a missing
protocol or Host implementation.

### Typed screenshot request

`POST /mobile/request-screenshot` accepts `{ "sessionID": "…" }` and returns
an acknowledgement timestamp. Both Hosts translate that semantic request into
the same provider-neutral prompt through `session_input`: sanitized bracketed
paste, settle, then the proven double-Enter submission. Controllers never
construct terminal escapes. The prompt asks the active agent to use Unpeel
Browser's screenshot action with `gallery=true` and save a session screenshot
artifact, while explicitly allowing the agent to report that a non-visual task
has nothing to capture. That explicit Controller request overrides the ordinary
Sessions use auto-gallery preference.

The iOS terminal surface exposes the action only when bootstrap advertises
`artifact.request_screenshot` and `artifact.list`. It samples the current
capture ids before sending, acknowledges while waiting, polls metadata at a
shorter interval, then pulses and opens the existing gallery when a new
capture appears. After 45 seconds it reports that the task may not have a
visual result. The headless Host serves the matching read-only
`/mobile/artifacts` and `/mobile/artifact` routes, so the review flow does not
depend on the native app being present.

- Server: `crates/unpeel-core/src/remote_server.rs`, run as
  `unpeel-host __remote__ [--bind ADDR] [--port N]`. Like the Sessions MCP it
  talks directly to session artifacts and needs no app window. Hand-rolled
  sync HTTP/1.1 + RFC 6455 WS over rustls (no tokio/axum).
- Security: TLS always (self-signed cert + fingerprint in
  `~/.unpeel/remote/tls/`), per-start bearer token (`~/.unpeel/remote.json`,
  0600), **plus** the app's paired-device tokens (verified live against
  `~/.unpeel/mobile/devices.json`, so app-side pairing/revocation applies
  immediately). Per-IP rate limiting, audit log at
  `~/.unpeel/remote/audit.log`.
- Routes: `/api/status|sessions[/:id[/activity|metrics|viewers]]`, output
  (WS stream or plain-GET JSON long-poll), `input`, `resize`, `kill`,
  `/api/clients`, browser artifacts (`.../artifacts/browser[...]` +
  ETag-friendly `/preview`).
- **Key constraint:** any output streamer must replay history from
  `output.bin` on disk and only subscribe the live control socket at the tail
  offset — subscribing far behind the host's in-memory broadcaster kills the
  socket (the attach client splits replay/live the same way).
- **Unpeel Link Relay (originally shipped as Unpeel Remote, 2026-07-02,
  dark)** — off-LAN phone access through
  a Cloudflare Worker + Durable Object (`unpeel-relay:apps/relay`, plain `.mjs`, no deps).
  Both sides dial outbound; interactive session frames are end-to-end encrypted.
  APNs notification metadata is a separately disclosed, bounded relay path.
  The LAN pairing request/response is AES-GCM sealed with the scanned QR secret
  and bound to the scanned Mac id + endpoint. Native keeps Mac E2E keys in
  Keychain and, for app → headless-serve handoff (previously also
  app → standalone-TUI handoff), reconciles authorized
  entries into the shared `~/.unpeel/mobile/e2e-keys.json` registry. That
  compatibility copy is an atomically replaced 0600 file readable by
  processes running as the same local user; it is not an opaque Keychain
  broker or a crash-atomic multi-store transaction.
  **Forward-secret** handshake: per-device static key (minted at pairing) +
  ephemeral X25519 per connection, HMAC transcript MAC authenticating the
  ephemeral keys against a relay MITM/downgrade; HKDF per-direction keys,
  AES-256-GCM with direction-tagged counter nonces (`RelayProtocol.swift` in
  UnpeelShared). Verified by a cross-language known-answer test (Swift
  CryptoKit vs JS WebCrypto, byte-identical) and a live workerd integration
  test driving every auth gate adversarially — `npm test` in `unpeel-relay:apps/relay`. Mac uplink: the workspace worker
  (`crates/unpeel-serve`, relay URL override `unpeel.native.relayURL` read
  through the overlay). `RelayUplinkManager.swift` keeps only the client
  half: the shared Link authority record, the `link.entitlement.refresh`
  platform callback, and APNs push through the Link service. The phone and Mac
  Controller try the saved Direct endpoint first, fall back automatically to
  Relay, probe the saved Direct endpoint on the way back, and display
  Direct/Via Link without a manual transport selector. Neither adopts an
  unauthenticated Bonjour endpoint. Starting with iOS build 15, a healthy
  authenticated Direct connection automatically repairs missing, malformed,
  legacy-unmarked, or Relay-rejected per-Host credentials without changing
  the phone/Host pairing identity; a headless Host's unsupported recovery
  route leaves an otherwise-valid legacy credential intact.
  **Paid-service gate**: the
  uplink needs an Ed25519-signed entitlement from
  `POST unpeel.com/api/remote/entitlement` (active-seat checked and persistently
  bound to the licensing device id + relay Mac id; an active Pro license is
  the whole **shipped** gate — the old `REMOTE_ACCESS_MODE` knob is retired).
  The target Link identity/seat/login model and the Relay deploy runbook
  are not part of this repository. Relay tests:
  `npm test` in `unpeel-relay:apps/relay`.
  - **Public website doc (live since 2026-07-23):** a user-facing doc lives
    at `unpeel-website:apps/website/app/docs/unpeel-remote.md`, registered in
    `unpeel-website:apps/website/app/docs/manifest.ts` as the "Remote access" group and
    published at `unpeel.com/docs/unpeel-remote`. The build-time gate
    `VITE_UNPEEL_REMOTE` is now set permanently via the committed
    `unpeel-website:apps/website/.env` (Vite inlines it) — don't remove that file or the group
    drops from `DOC_GROUPS` again (absent from the sidebar and unroutable;
    `isDocSlug` derives from `DOC_GROUPS`). The security section is
    intentionally accurate-not-overclaimed: it states
    E2E/forward-secret/zero-knowledge + standard primitives, and does **not**
    claim an independent audit or "Tailscale-level" assurance. It also
    includes a "How is this different from SSH?" comparison.
- Viewer presence: the server writes `~/.unpeel/remote/presence.json` and
  the worker writes `mobile-presence.json` beside it; `ViewerPresence.swift`
  merges both files and `ViewerAvatarsView` renders avatar chips in the
  terminal title bar.
- Connection resilience (security correction 2026-08-14): the phone persists
  the Mac's `/mobile` endpoint at pairing time, and the worker's Direct
  listener normally re-binds that port from `~/.unpeel/mobile/server-port`. When
  bootstrap polls fail, the phone shows an explicit "Connection lost" state,
  hides stale sessions, retries only that persisted endpoint, and falls back
  to the E2E Relay when enrolled. It does **not** adopt Bonjour candidates.
  `_unpeel-remote._tcp` TXT data is unauthenticated; sending the saved bearer
  to a discovered plaintext URL would disclose it before any bootstrap macID
  check could run. An exact-identity E2E Relay bootstrap instead advertises
  the Mac app Host's current Direct `/mobile` endpoint. iOS strictly validates
  and persists that authenticated hint, gives it a 750 ms opportunistic probe,
  and otherwise keeps the working Relay while later Direct retries continue.
  This repairs a stale saved IP/port without a second visible reconnect or
  re-pairing; a port/IP change **without** Relay still requires explicit
  re-pairing. Both iOS and macOS Controllers keep automatic Bonjour discovery off
  until pinned TLS or an equivalent proof-of-possession authenticates the
  candidate before credentials are sent. The per-run pinned WSS port still
  rides authenticated bootstrap responses from the persisted/Relay path.
- Lifecycle: the workspace worker spawns/supervises the server
  (`crates/unpeel-serve/src/remote_streamer.rs`) whenever paired mobile
  devices exist (`~/.unpeel/mobile/devices.json` non-empty) — auto-start on
  launch/pairing, stop when the last device unpairs. `RemoteControlManager`
  (Swift) was retired 2026-09-03. The
  hidden default `unpeel.native.remoteControlServer` is a force override
  (true = always run, false = never, absent = automatic). No settings UI yet
  (that is phase 3). Paired phones discover the server via the optional
  `remoteServerPort` + `remoteServerCertificateFingerprint` fields on the
  pair response and `/mobile/bootstrap` (port is OS-assigned per run;
  fingerprint is stable). The output WS is the phone transport: hello text
  frame, then binary frames prefixed with an 8-byte BE u64 `output.bin`
  offset; client text frames carry raw `input`/`resize` (wire contract
  documented at the "WebSocket output streaming" header in
  `remote_server.rs`).
- **Snapshot baseline (additive, 2026-09-03, Round 3).** The phone's fresh
  subscribe no longer has to replay a journal tail into its VT and lean on
  the hello's `mode_preamble_base64`: a client that appends `snapshot=1` to
  the upgrade query (the request line is the client's only advertisement;
  the protocol has no client hello) may receive hello `"snapshot": true`
  with `start_offset` = the Host's VT snapshot offset and no mode preamble,
  followed by exactly one text frame
  `{"type":"baseline","journal_offset":N,"cols":C,"rows":R,"bytes_base64":"…"}`
  before any binary frame. The client resets its VT, feeds the decoded
  bytes (the same libghostty-vt formatter output `unpeel-attach` applies:
  cells, styles, scrollback, every non-default mode, scroll region, cursor),
  sets its cursor to `N`, and then consumes binary frames from `N` exactly
  as today. The streamer obtains the snapshot over the Session's control
  socket (`session_host::request_terminal_snapshot_vt`) and joins it to the
  stream through the same explicit-replay-from-offset path an `?offset=`
  resume uses, so the broadcaster-ring race and retention eviction (close
  1012) are handled identically. `"snapshot": false` means the Host could
  not answer (older `unpeel-host`, exited session, transport error): the raw
  tail path follows unchanged. An explicit `?offset=` resume never gets a
  baseline (the client already holds state), and a client that does not
  advertise sees neither key nor frame — byte-identical to before. Rust
  conformance: `ws_snapshot_capable_client_gets_baseline_then_stream_from_its_offset`,
  `ws_client_without_capability_keeps_raw_tail_replay`,
  `ws_snapshot_capable_client_degrades_to_raw_tail_without_a_host` in
  `remote_server.rs`. The iOS client adoption is the phone lane's work.
