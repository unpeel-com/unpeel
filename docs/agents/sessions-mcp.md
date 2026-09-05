<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Built-in Unpeel MCP server

`unpeel-host __mcp__` is one MCP server named **`unpeel`**. Do not call the
whole server “Sessions MCP” or rename it “Agents MCP”: `sessions` and `agents`
are sibling domains with different identities. A Session is the Host-owned
terminal container; an agent is a recognized runtime occurrence currently
occupying one. The other domains are `workspace`, `artifacts`, `browser`,
`computer` (Linux Hosts in every build; the Mac's own desktop development-only), preview `apps`, and the root `skills` registry.

**Experimental compatibility gate:** Settings ▸ Experimental ▸ Sessions use
(`ExperimentalFeature.sessionsMcp`, `UNPEEL_DEV_SESSIONS_MCP=1`) still owns
the saved `mcp_enabled` bit. For compatibility that one bit enables
`sessions`, `agents`, `workspace`, and `artifacts`; do not rename the persisted
field, environment variable, feature id, or provider config filenames. Existing
live Sessions keep their launch-time domain set until the runtime/MCP client is
restarted. Transcript display settings remain under Settings ▸ Transcripts.

Terminal messaging may later become channel-based — terminal↔terminal is the
default today; see the private "sessions-mcp-channels" design record. Every
`send_text` delivery carries `[message from id:<sender>, channel: terminal]`.
Route inter-session text through
`deliver_text_to_terminal`; do not bake “the other end is a PTY” into future
channel semantics.

> **Unified surface (2026-07-18, renamed 2026-07-25):** `unpeel-host __mcp__`
> is now the single
> **`unpeel`** MCP server for all built-in capabilities (named `unpeel-mcp`
> until 2026-07-25; the old name lives on only as pruned legacy config
> entries and in the pre-rename config *file names*, which are kept so
> restart commands recorded by older sessions keep resolving): **one action-enum
> tool per domain** — `sessions`, `agents`, `workspace`, `artifacts`,
> `browser`, `computer`, `apps`, and `skills` — instead of one server per domain
> with a dozen tools each. Schemas are terse (~1.5k tokens for both domains,
> enforced by a byte-ceiling test in `mcp_host.rs`); full per-action docs load
> lazily via `{"action":"help"}`. A domain is advertised only if the caller's
> saved domain grant (`mcp_enabled` / `browser_mcp_enabled` /
> `computer_mcp_enabled`) is set — a session launched without a domain never
> pays its context cost — and
> per-call gates still apply live. Legacy per-tool names and the standalone
> `__browser_mcp__` argv keep working for sessions launched pre-unification.
> The separate `*_client_registered` fields say whether Unpeel injected the
> provider configuration automatically. They remain false for a blank shell;
> a CLI configured manually with `unpeel-host __mcp__` still receives only the
> saved grants.
> Injection is **one config per provider** (claude `claude-unpeel-mcp.json`,
> codex wrapper `mcp_servers.unpeel` via `UNPEEL_MCP_BIN`, legacy kimi
> `kimi-unpeel-mcp.json`, Kimi Code `__mcp_gate__ unified`, cursor/cline a
> single `unpeel` entry, kiro's combined server delegates); the env var /
> config is present when *any* domain is enabled. Persistent configs
> (cursor `~/.cursor/mcp.json`, Kimi Code `~/.kimi-code/mcp.json`, kiro
> `settings/mcp.json`) prune the managed pre-rename `unpeel-mcp` entry the
> same way the unification pruned `unpeel-sessions`/`unpeel-browser`.
>
> **Computer domain (release rule D2, 2026-09-03: ships for Linux Hosts in
> every build; the macOS adapter driving the Mac's own desktop stays
> development-only under the 2026-08-14 containment; engine swapped
> 2026-07-22):** the
> `computer` action tool (`crates/unpeel-core/src/computer_mcp.rs`,
> **cua-driver** engine — see the private "computer-mcp" design record) gives a session
> **background** control of the user's REAL apps: `launch` → pid + windows,
> `see` → accessibility tree (`[N]` element indices) + screenshot artifact,
> then click/type/set_value by element index — no focus steal, the user's
> cursor never moves (a per-session overlay cursor glides instead).
> Desktop-wide scope needs an explicit `escalate` (cua's one-way window→
> desktop ladder; each Unpeel session is cua session `unpeel-<id>`). On macOS
> release builds hide the feature, force its launch flag off, stop stale
> daemon state, and omit cua-driver: the unrestricted TCC-bearing socket is
> not isolated from same-UID hosted code — that laundering does not exist on
> Linux, where the Host installs the engine itself and the release
> Controllers operate it. macOS development builds remain gated by
> `ExperimentalFeature.computerUse` (`UNPEEL_DEV_COMPUTER_USE=1`),
> `computer_default_access` (`off`/`ask` default/`allow` in state.rs), and
> under `ask` a one-time per-session approval alert (`/mcp/approve-computer`,
> `MCPComputerApproval.swift`; remembered in `computer_approvals`,
> pruned/carried like write approvals). On macOS the **native app owns the
> engine daemon** (`ComputerEngineManager.swift` spawns `cua-driver serve
> --embedded --socket ~/.unpeel/computer/daemon.sock` as a direct child so
> TCC attributes to Unpeel.app — never spawn it from a session host or via
> `open`). On Linux, canonical `unpeel serve` owns the equivalent supervised
> child only when Cua Driver, an X11/Wayland display, **and** the session
> D-Bus (AT-SPI) are available (`computer_engine::desktop_session`; `unpeel
> serve install --graphical` binds the service to the desktop session); it
> advertises availability/readiness to Controllers instead of making them
> guess from Host kind. `computer_mcp.rs` makes one-shot `cua-driver call … --socket`
> invocations against it. Grants are probed/requested natively
> (`ComputerPermissions.swift`); the daemon restarts on grant changes.
> Captures land in `artifacts/computer/screenshots/` (phone gallery kind
> `computer`). **The engine is pinned and Host-installed (Lane A,
> 2026-09-03):** `protocol/computer-engine-v1.json` pins cua-driver 0.23.2
> (release tarball sha256 + the extracted binary's own sha256 per platform;
> darwin-arm64/x64 share the universal archive), `unpeel_core::computer_engine`
> installs it into `~/.unpeel/computer/bin/cua-driver` (the serve adapter
> does it on demand once Computer use is on; `unpeel computer install
> [--check]` is the scripted verb; `docs/agents/serve.md`), and resolution
> is `UNPEEL_CUA_DRIVER_BIN` → verified managed copy → next to `unpeel-host`
> (the development app bundle) → PATH, a stale managed copy skipped. Every
> engine process Unpeel starts sets `CUA_DRIVER_RS_TELEMETRY_ENABLED=0`.
> Session cleanup rides `__computer_cleanup__ <id>` next to
> `__browser_cleanup__`. Not yet: `verify-computer.sh` and a CLI-matrix
> case (plan Lane E). Its Ask prompt is a cooperative agent control, not a
> sandbox boundary; see the private "computer-mcp" design record and
> the private "computer-use-release" design record. **Engine bump procedure:** update
> `version`, every `sha256` from the release's `checksums.txt`, every
> `binarySha256` from the extracted member, and the notice in
> `protocol/computer-engine-v1.json`, then `cargo test -p unpeel-core
> computer_engine` and (once it exists) `scripts/verify-computer.sh`.

- Server: `crates/unpeel-core/src/mcp_host.rs`, run as `unpeel-host __mcp__`. Speaks MCP JSON-RPC over stdio; hand-rolled, no SDK dependency.
- It talks directly to per-session artifacts (`manifest.json`, `output.bin`, `session.sock`) under `~/.unpeel/app-sessions/`; it does not need the app running, only the session hosts.
- Each provider/client starts its own stdio sidecar process. This is
  intentionally not embedded in the long-lived `unpeel serve` worker: MCP
  connection lifetime follows the agent client, while Host authority follows
  the workspace. Reusable implementation stays in `unpeel-core`; workspace
  policy, approvals, and semantic effects converge on the worker/capability
  adapters, while terminal data operations may remain direct to Session
  artifacts and `session.sock`.
- Caller identity comes from `UNPEEL_SESSION_ID` in the inherited env; when a
  launcher strips the environment from its MCP children (cursor-agent does),
  `self_session_id` falls back to walking the server's process ancestry against
  the running manifests — the hosted login shell (`manifest.pid`) is an
  ancestor of everything the session's agent spawns, and only a
  start-time-verified `PidIdentity::Matches` ancestor grants identity (fail
  closed on recycled pids and unverifiable legacy manifests). Writing into the
  calling session's own terminal is refused.
- `agents.read_transcript` uses the shared provider transcript API in
  `crates/unpeel-core/src/transcripts/mod.rs`, so adapter/parser changes affect
  MCP and remote clients together. It refuses a transcript when the observed
  runtime occupant is not bound to the saved launch runtime.

Advertised ownership (old mixed `sessions` spellings remain decode-only):

- `sessions`: `current`, `list`, `inspect`, `read_screen`, `read_output`,
  `wait_for_text`, `send_text`, `send_keys`, `report`. This domain reports
  terminal/container state only; `inspect` no
  longer smuggles in provider transcript or agent identity.
- `agents`: `list`, `get`, `read_transcript`, `wait`. Every action targets one
  explicit recognized occurrence; the unreleased group-wide wait and summary
  actions were removed. `list` returns an occurrence-bound `agent_ref`
  (`session_id`, runtime id, pid/start time, runtime launch generation).
  Follow-up operations validate it so a replacement foreground process cannot
  be mistaken for the same agent. `session_id` remains a weaker compatibility
  target.
- `workspace`: `list_presets`, `create_worktree`, `list_worktrees`.
  Worktree creation remains opt-in and never launches a Session.
- `artifacts`: `add_to_gallery` for a caller-owned image artifact.

Session creation and closing remain user-only. Stale start/delegate/close tool
calls are refused. Legacy mixed action names and per-tool names continue to
dispatch where safe for already-running/cached clients, but are absent from
the new schemas. `list_group` remains an organizational compatibility query;
`report_to_group` aliases `report` and uses the ordinary write policy. The
unreleased group-wide wait/summary spellings are intentionally not retained.

Preset/worktree effects and write-approval prompts are Host-owned: the
worker answers `POST /mcp/*` on its own hook port and mirrors approvals to
the app through the `approval.present` platform callback (answers return
over the Host approval verb). The native `MCPBridge.swift` compatibility
adapter was retired 2026-09-03; the historical route contract below is the
worker's.

- Bridge: `crates/unpeel-serve` hook port, authenticated `POST /mcp/*`
  calls. Public effects use `list-presets`,
  `create-worktree`, `list-worktrees`, `approve-write`, and
  `approve-app-open`; `start-session` remains reserved for user/controller
  launches. Approval routes reply asynchronously (150s bridge ceiling, ~130s
  MCP client timeout). The MCP host tries launch-time `UNPEEL_APP_PORT`, then
  `~/.unpeel/app-ports` newest-first. App-less `unpeel serve` Hosts serve the
  same routes and the same shared approval queue.
- Auth: unlike hook routes, `/mcp/*` requires the `x-unpeel-auth` header matching `~/.unpeel/mcp/auth-token` (0600, created at hook-server start by `mcp_auth.rs` / the native `MCPAuth`) — the endpoints can launch arbitrary commands, and localhost is reachable by browser CSRF.
- Worktree creation maps onto the same native path as its UI verb. The MCP host defaults `project_id` to the calling session's project.

> **Security scope (2026-08-14): these are cooperative controls, not
> same-UID isolation.** Hosted commands run as the user's account and are not
> sandboxed by Unpeel. The `0700` Unpeel home and `0600` MCP token protect
> against other local users and browser-origin CSRF; they do not stop code in
> a hosted session from reading same-user state or discovering local sockets.
> Consequently the Ask/Deny rules below govern agents that use
> the supported MCP surface, but must never be described as a security boundary
> against malicious shell code. A hard boundary requires a Host-owned broker
> plus OS-enforced session confinement.

Cooperative access policy — **open reads, approval-controlled writes to every
other session** (reworked 2026-08-31):

- **Reads are open across ALL sessions.** Any enabled caller can `list_sessions`/`inspect_session`/read any session in any project (`McpSecurity::permits_manifest` = caller known and not internally `Off`). The old project/worktree reach machinery was removed from the gate; `McpScope`/`mcp_default_access` survive only as decode-tolerant legacy fields (an explicit per-session `Off` override in `mcp_orchestrators` still disables a session's tools entirely).
- **Sidebar groups are organizational only.** Project roots, plain groups, and worktrees remain useful filing and layout context, but moving a session never grants or revokes authority.
- **Every write to another session goes through the app-wide write policy** stored under the compatibility key `AppState.mcp_nonchild_write_access` (`ask` default / `deny` / `allow`, `McpNonChildWriteAccess` in `state.rs`), re-read per call so changes apply live. Under `ask`, `require_session(_, Write)` first checks the persisted pair map `AppState.mcp_write_approvals` (`caller id → [target ids]`, directional); on a miss it POSTs `/mcp/approve-write` to the app with a 130s read timeout (`request_write_approval`) and the user answers the approval prompt — Allow persists the pair, Deny fails the tool call with a clear "don't retry" message. Prompts are FIFO and identical pairs coalesce; the exited-target check runs before the prompt so a dead session never asks.
- **Session lifecycle is user-owned.** Agents cannot create or close sessions. Cached `close` calls fail without performing an effect; write approval never grants termination authority.
- **Legacy lineage is decode-only.** `parent_session_id`, `session_parents`, and the remote protocol's `parentSessionID` remain tolerated for older manifests/controllers, but current hosts never write or enforce them and current clients render sessions flat.
- **Unified approval prompts, answerable from controllers:**
  `/mcp/approve-write|browser|computer|app-open` share one pending queue
  (`PendingMcpApproval` in `MCPApprovalCenter.swift`; route handlers keep fast
  paths). Desktop and phone both show an in-pane overlay on the Session the
  grant is about (write: the destination, otherwise the caller) plus that
  Session's attention badge — never a floating window and never
  `NSAlert.runModal()`, which stalls queued main-actor work including mobile
  bootstrap. Pending prompts ride phone bootstrap and are answerable through
  `POST /mobile/approvals/answer`; first answer wins. App launch approvals are
  remembered under `mcp_app_open_approvals` as caller Session → App ids,
  pruned/carried with caller replacement just like other Session-keyed grants.
- **Approval lifecycle:** pairs live in `~/.unpeel/app-state.json`; an in-place Resume Agent after the managed runtime returns to its shell keeps the same Session id and therefore needs no migration. Replacement Resume/handoff paths snapshot the map before `pruneNativeState` and re-add every pair under the new Session id (both directions), using the same read-before-prune discipline as the carried access grant.
- **Launch injection is unchanged:** `SessionHostLaunch.mcp_enabled` still decides both the saved Sessions-domain grant and whether a managed provider gets automatic configuration (Claude `--mcp-config`, Codex `-c mcp_servers.*`, Cursor `~/.cursor/mcp.json` + `--approve-mcps`, current Kimi's environment gate in persistent `~/.kimi-code/mcp.json`, legacy Kimi repeatable `--mcp-config-file`, Cline per-session `CLINE_MCP_SETTINGS_PATH`; other CLIs ignore it). The manifest records those as distinct `mcp_enabled` and `mcp_client_registered` facts.
- **Native UI:** Settings ▸ Sessions use explains open reads and per-target
  write approval, offers the app-wide write policy and gallery toggle, and lists both approved
  Session-write pairs and approved App launches with per-entry Revoke. Changes
  apply live; nothing here drives a restart banner.

> **Removed (2026-06-22):** the per-project MCP *block* feature (`mcp_blocked_projects`, `Project.mcp_blocked`, the Settings "Block individual projects" section, host/bridge block gates) is gone. The native `AppStateFile`/`Project` decoders still tolerate the old `mcp_blocked*` keys for backward-compatible reads, but nothing writes or enforces them.

Auto-registration per provider:

- Claude: `install_claude_hooks` writes `~/.unpeel/mcp/claude-unpeel-mcp.json` (rewritten each launch so the exe path — `unpeel-host` — stays current; the legacy `claude-mcp.json`/`claude-browser-mcp.json` are still rewritten for pre-unification live sessions); `claude::startup_command` appends one `--mcp-config <path>` when any domain is enabled (skipped if the user already passes `--mcp-config`).
- Codex: the wrapper at `~/.unpeel/hooks/bin/codex` injects `-c mcp_servers.unpeel.*` overrides when `UNPEEL_MCP_BIN` is set (exported by `codex::configure_host_command` when any domain is enabled, pointing at `unpeel-host`); session identity is passed via explicit `env` because Codex spawns MCP servers with a minimal environment.
- Kimi: `install_kimi_hooks` supports both generations. Current Kimi Code gets one merged `~/.kimi-code/mcp.json` entry `unpeel` pointing at `unpeel-host __mcp_gate__ unified` (enabled when either grant env var is set; managed legacy `unpeel-mcp`/`unpeel-sessions`/`unpeel-browser` gate entries are pruned); `kimi::startup_command` probes `kimi --help` and uses the old repeatable `--mcp-config-file` injection (now one `kimi-unpeel-mcp.json`) only for legacy Kimi, preserving its implicit `~/.kimi/mcp.json` behavior.
- Cline: `cline::configure_host_command` copies the current user MCP settings
  into `app-sessions/<id>/cline-mcp-settings.json`, adds only the servers
  granted to that launch, and selects the copy with
  `CLINE_MCP_SETTINGS_PATH`. Concurrent sessions can have different grants and
  the user's global file stays untouched.

## The `apps` and root `skills` domains (2026-08-24)

`apps_mcp.rs` is the first landed piece of the Unpeel Apps agent contract
(the private "unpeel-apps" design record "Agent access" is authoritative). An installed
Unpeel App is an entry in `protocol/app-registry.json` whose declared
CLI resolves on the Host's PATH. This catalog plus PATH check is the entire
current discovery contract; **no app ever runs its own MCP server**. The `apps`
domain advertises `list`, `describe`, `search`, `context`, and `open`.
`context` returns agent-safe attached/project relationships plus the same
caller-relative direct-neighbor snapshot as `sessions.current`. A neighboring
App includes its ordinary readable companion Session id so “check Design on
the left” resolves to an explicit target. Each neighbor entry is a one-call
identity card (2026-08-26): kind (terminal/agent/unpeel_app), label, `cwd`,
state, activity, and for agent panes the catalog `runtime_id` plus resolved
`runtime_name`; an App entry inlines the central catalog description. Future
package tool summaries and skill references remain reserved for the declared
tool-execution slice; the current catalog publishes neither. The snapshot
comes only from this Host's durable `windows["main"]["local"]` Controller tree and exposes no
pane ids, ratios, pixel geometry, focus, zoom, or transient visibility.
Because some provider CLIs (Codex) never surface MCP *server* instructions to
the model, the routing cues that map user language onto this snapshot —
spatial words ("left", "next to me") and selection words ("the selected …",
"what I have open", for any App: a design, document, note) — must live in the
per-tool descriptions themselves (`sessions`/`apps`, 2026-08-26). Keep new
routing guidance there, not only in server instructions. A pane
currently branded as an App also carries that App's self-published **live
context**: the App writes an `app-context.json` marker beside
`app-title.json` in its session dir (`{"app": id, "context": {…app-defined…},
"updated_at": ms}`, a JSON object ≤ 16 KB), and pane-context queries read it
fresh per call and surface it verbatim as the neighbor entry's `app_context`
(`session_host::read_app_context_marker`). It is never folded into Host
state (selection-frequency updates cost no manifest churn or state-bus
pings), never exposed for a pane that is not currently App-branded — a
marker left behind by an exited App must not speak for the shell that
remains — and always framed as app-authored data, never instructions; each
App's public documentation defines its own `context` schema (Unpeel Design:
selected file + line span; a markdown App: current file/heading). `open`
resolves only an installed catalog entry, derives caller/project/cwd Host-side,
requires remembered user approval per caller/App pair, ensures a Host-owned
project/resource App instance, and spawns/reuses its Horizon-A companion
Session. A caller-scoped `request_id` deduplicates retries; `reveal:false`
attaches without advancing the reveal revision. The root `skills` domain provides
`list`, `search`, and `get`; future App package guidance uses namespaced ids
there rather than adding an Apps action. Every App action rechecks the Host's
resolved PATH, so a mid-session install is visible without a restart.

Presentation state is the versioned `app_presentations` envelope in
`app-state.json`: App instances are project/resource identities; bindings pair
one caller with a view/`panel` target and monotonic reveal revision. Agent MCP
open receipts never expose the backing companion Session or claim placement;
a later `sessions.current` or `apps.context` snapshot may identify it only
when it is a direct neighbor in the local durable Controller tree. Native
Controllers consume the trusted binding and project a first reveal as their
own trailing/right split. Pane ids, ratios, focus, visibility, and durable
membership remain in Controller-owned pane state. Each Controller persists a
local handled/dismissed revision, so detaching stays detached until a later
intentional `open` increments the Host revision. Remote/phone projection still
needs an additive Host-bootstrap field; do not infer it from Session role,
commands, or pane files.

Advertising: the `apps` and `skills` tools appear whenever any other domain is advertised
(`McpDomainMask.apps`; the `__mcp_gate__` unified entry grants it when any
domain grant is present) and its tool description embeds the live installed
App/skill ids, computed at server launch. Declared RoomStore tool *execution* is intentionally
absent until RoomFS/the Host worker exist — `describe` says so and points
agents at the app's standalone command and root skill reference.

Reference convention: an app can hand agents a token like
`[mcp:unpeel.app.design hero.html LOC:12:32]`; the tool description and
server instructions teach agents to resolve it by fetching that app's
skill through `skills.get`. First installed app: `unpeel-design` (self-installs its manifest at
startup; see `~/Dev/unpeel-app-design/src/agent_hook.rs`, which also implements
"Send to agent" — pasting the token into an agent session through
`unpeel-host __mcp__` sessions `send_text` (and therefore the same approval
policy as any other inter-session write) — plus the project-local
`.presence/<participant>/` heartbeat/selection/claim bridge and advisory
`.presence/current-owner.json` its skill documents). This disposable
standalone bridge uses the Link profile display name when available and keeps
a one-release read fallback for `.unpeel-design/claims.json`; it is not the
future authoritative Host `room.presence` lease.

## Dual-era MCP transport (Cloudflare/MCP v2 review, 2026-08-23)

The local stdio server accepts both the shipped initialize-era protocol and
the 2026-07-28 discovery protocol described in Cloudflare's MCP v2 review:

- legacy `initialize`, `tools/list`, and `tools/call` response shapes remain
  unchanged for existing CLIs;
- modern clients start with `server/discover` and send
  `params._meta["io.modelcontextprotocol/protocolVersion"]` plus the client
  capabilities object on every request;
- unsupported versions fail with `-32022`; every modern success includes
  `resultType:"complete"` and server-info metadata;
- discovery/tool-list caching is `ttlMs:0`, `cacheScope:"private"`, because
  authorization, installed Apps, and skills are caller/Host-specific.

Do **not** advertise `io.modelcontextprotocol/ui`: Unpeel Apps are standalone
Host Apps, not MCP Apps iframe resources. Streamable-HTTP method headers and
OAuth are irrelevant to this local stdio transport. In-flight cancellation is
implemented (2026-08-23) as a reader/worker split, deliberately **not** full
concurrent dispatch: tool calls run strictly in submission order on one
worker thread, because pipelined callers depend on that ordering and one
caller's verbs must never race each other (the `apps.open` dedup proof
encodes this). The reader thread stays live to answer fast protocol methods
(`ping`, `tools/list`, discovery) and to observe `notifications/cancelled`:
cancelling a queued call skips it, cancelling the in-flight call unwinds its
poll loop within ~250ms (`mcp_cancel::bail_if_cancelled` in every wait
loop), and the response is dropped per spec. A cancellation cannot interrupt
a blocking approval-bridge read, but the approved effect is suppressed at
the post-approval boundary — an approval answered after cancellation never
types into the target or commits App state. EOF still drains the queue
completely, so piped batch callers keep exact sequential behavior. Process
proof: `crates/unpeel-host/tests/mcp_cancel_process.rs`.
MRTR/input-required should be added only for actual MCP-client elicitation,
not as a replacement for Unpeel's Host/Controller approval UI.

References: `https://blog.cloudflare.com/mcp-v2/` and the official
`https://modelcontextprotocol.io/specification/2026-07-28/server/discover` /
`basic/versioning` / `server/utilities/caching` sections.

Debugging: `mcp-host` lines in `~/.unpeel/hooks/trace.log`. Test with `printf '...' | unpeel-host __mcp__`.
