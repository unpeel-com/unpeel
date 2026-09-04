<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Session Model

`SessionInfo` in `crates/unpeel-core/src/state.rs` is the canonical app-level session record.

Important fields:

- `id`: Unpeel session id
- `command`: command Unpeel launched
- `label` / `custom_title`: display title; by default it follows the agent's own task summary when the foreground runtime publishes semantic terminal titles, falling back to the first typed prompt until one appears. Auto-titling is applied inside the session host (`apply_manifest_auto_title` in `session_host.rs`, on socket `write` commands), so every client — the native attach client, MCP `send_text` — titles sessions the same way. The shared parsing helpers (`extract_submitted_prompt`, `normalize_prompt_title`) live in `session_host.rs`. Slash-command lines (`/resume`, `/model opus`, …) are skipped — every agent CLI has them and none make a useful title — so a session whose first line is `/resume` titles from the next real prompt instead of being called "/resume" forever (absolute paths like `/tmp/build.log …` still title: the discriminator is a single-segment leading token). The native app also keeps user renames as a UserDefaults overlay (`UnpeelStore` session title overrides). The shared `session_title_mode` knob in `app-state.json` (Settings ▸ Appearance ▸ Session titles; `state.rs SessionTitleMode`) selects the driver: `agent` (default), `first_prompt` (one-shot from the first message), or `off` (no automatic titling). In agent mode, the host's `OscTitleScanner` passively watches the PTY output for `OSC 0/2` terminal titles and `apply_agent_terminal_title` folds them into the label, but only while the observed foreground runtime declares the `semantic_terminal_title` capability in its runtime package (Claude today — it publishes model-written task summaries; shell cwd spam, ssh hostnames, and static branding never retitle a row). Agent mode keeps following title updates like the App-title marker, falls back to the first-prompt title until the first OSC title arrives, and every mode loses permanently to a user rename. Hosts re-read the knob per title event, so a Settings change applies live.
- `created_at`: visible ordering timestamp
- `owner_principal_id`: immutable human/principal owner. Creation adapters
  derive it from authenticated Controller context; it is never accepted from
  `POST /mobile/sessions`. Local/SSH launches and older manifests resolve to
  the stable compatibility principal `host-owner:<host-id>`. A future Link
  account id can occupy the same field without tying ownership to one device.
- `created_by_device_id`: optional audit provenance for the independently
  revocable device that initiated creation. It is not an authorization key;
  all devices belonging to `owner_principal_id` are the same Session owner.
- `source_preset_id`: optional Host-owned preset selected at creation. The
  Host resolves the command from its own catalog before recording this id;
  clients cannot smuggle paths, commands, or ownership through preset
  metadata.
- `tag_id`: optional tag (carried in the model; the native app does not yet expose tag UI)

These ownership fields are additive. Old manifests decode with them absent;
Host summaries and native rescan expose an effective Host-owner principal
without rewriting historical files. New Session manifests are stamped before
publication. Restart/replacement preserves ownership; a fork preserves the
source Session's owner but records the device that initiated the fork.

This is only the persistence and attribution foundation for a future shared
Host. Today all paired Controllers remain owner-equivalent. Per-principal
Session filtering and authorization must land before non-owner Controller
enrollment is safe: list/bootstrap/archive/output/artifacts and every Session
effect must all enforce `request principal == session owner` on the Host.

`HostedSessionManifest` in `session_host.rs` is the on-disk host record.

Important fields:

- `state`: `running` or `exited`
- `pid`
- `pid_started_at`: kernel-reported start time (ms epoch) of the process `pid`
  refers to, captured when the pid was recorded. Kill/reap paths (native
  `terminateHost`, host `reap_dead_sessions`) must verify a live process
  against it before signaling anything — under agent load the pid counter
  wraps in under an hour, so a stale manifest's pid routinely points at an
  unrelated live process, and an unverified `kill(-pid, SIGKILL)` takes out an
  innocent session's process group (this killed random live agents until
  2026-07-09). Legacy manifests omit it: identity is then provable only via a
  positive argv match on the session id, and a session that can't be
  positively identified is cleaned up without being signaled.
- `host_build_id`: optional build identity for the `unpeel-host` binary that
  created the manifest. New hosts write it from the executable's mtime/size;
  older manifests omit it. This is diagnostic only; do not use it to decide
  whether to show restart UI.
- `host_protocol_version`: optional stable capability/protocol version for the
  host. New hosts write it. Native restart recommendations compare this field
  to the app's required host protocol.
- `runtime.currentObservation`: optional live, display-only identity for a
  recognized agent in the Host-owned PTY foreground job. The first slice uses
  existing integration IDs such as `claude`/`codex` and includes bounded
  process diagnostics. It may change or clear as a user runs programs inside
  a blank shell. It never rewrites `SessionInfo.command`, chooses a transcript,
  or grants restart/resume/fork/context capabilities. Exited manifests do not
  advertise it as current, and remote summaries expose it separately as
  additive `activeRuntimeID` while legacy `providerID` remains launch-derived.
- `runtime_launch_generation` / `runtime_launched_at`: stable managed-launch
  generation inside this PTY and the time that generation began. Direct
  managed launches start at generation 1; blank terminals stay at 0. A
  successful in-place Resume Agent advances the generation so activity and
  hook evidence from the prior process cannot latch onto the new one.
- `runtime_launch_output_offset`: the `output.bin` boundary captured just
  before the current in-place relaunch command is submitted. Resume-failure
  detection scans from this boundary rather than matching an error retained in
  older scrollback.
- `runtime_launch_pending`: Host-owned duplicate-submission latch. Managed
  startup begins pending, and Resume Agent publishes it before the irreversible
  PTY write. It clears only after the expected runtime is observed or the
  shell-side completion wrapper proves the launch returned without one. The
  Controller wire spelling is additive `runtimeLaunchPending`; omission from
  an older summary decodes as `false`. While true, every frontend suppresses
  Resume Agent and the Host rejects another submission.
- `heartbeat_at`

A live control-socket Kill stays inside the Host's owned PTY boundary. On
Unix, the Host reads the terminal's current foreground process group, verifies
with `getsid` that it belongs to the still-unreaped child session created by
`portable-pty`, sends TERM then bounded KILL to that verified group, and kills
the exact owned wrapper child. The output reader uses a bounded poll so a
descendant retaining the slave cannot pin the manifest in `running`; buffered
termination output is drained first. Never signal an unverified process group.

## Persistence, Restart, and Resume

- `~/.unpeel/app-state.json` holds projects, presets, theme, Session pins in
  `pinned_sessions`, and additive plain-group `pinned_at` markers. It is the
  shared on-disk contract. Desktop Pin/Unpin appears only in the Session or
  group row's context menu; the sidebar renders a passive pin icon only after
  that row is pinned. Session pins form the top section of their owning group;
  pinned child groups are stably partitioned above the parent's ordinary mixed
  rows without destroying manual order. In the native detached drag, either
  kind remains sortable within that pinned partition; crossing its sidebar
  boundary is refused on drop, while Session-to-pane drops remain available
  because pane membership is Controller presentation state, not Session
  organization. Previously migrated **Pinned** child
  groups remain ordinary groups. Presets are read and written
  there by both frontends as their single source of truth; some other native
  fields still use UserDefaults overlays or the shared marker/order files
  documented here.
- Manual sidebar session order is shared separately in
  `~/.unpeel/session-order.json`. Both desktop and the (now-removed) TUI preview
  row movement in memory while dragging, then take the shared lock and persist/broadcast the
  final order once on drop. Cancelling a drag restores the last durable order
  without writing.
- Each group (project, plain group, worktree, cwd bucket) can instead sort its
  sessions by shared lifecycle recency ("Recently updated" — pinned Sessions
  remain in their separate top section; live rows use the Recent rank, then
  the capped stopped/archive preview forms a bottom section; no read-receipt
  priority).
  The lifecycle timestamp is `created_at` as the floor, command-aware real
  activity (hook seed, otherwise parsed-screen change / legacy output), and
  final manifest `updated_at` only after exit; a running heartbeat never
  counts. The row renders that same timestamp, so the age shown beside it
  always explains its position. The `"date"` mode value is kept for
  compatibility: `session_sort_modes` in `app-state.json`
  maps group id → `"date"` (absent = custom, the manual order). Both frontends
  offer it from the folder context menu ("Sort sessions"); while date sort is
  active drag re-ordering is disabled, and the stored manual order is kept so
  switching back restores the old arrangement.
- Lifecycle operations are serialized across processes by a per-session file
  lock. **Resume Agent** is offered only after a managed runtime has exited or
  crashed back to its still-live interactive shell. The Host freshly verifies
  that the original shell executable and interactive-login invocation still
  identify the owned Session leader, that it owns the terminal foreground, and
  that a complete owned-session scan contains no retained expected job,
  stopped/background job, or different recognized runtime. A process-scan
  failure is also a denial. Only then may it write the Host-derived resume
  command into the same PTY; it never stops or replaces an active runtime. The
  Session id, host pid, socket, `output.bin`, artifacts, `created_at`, title,
  pin/order metadata, grants, and approvals stay put. Passive observation in a
  blank terminal cannot authorize it. If the **Host** crashed instead, its
  health refresh marks the terminal stopped only when the recorded child is
  definitively absent or its PID was recycled; healthy or unknown-live process
  ownership remains non-resumable. Ordinary **Resume** then creates a
  replacement Host. **Reload
  Terminal** remains the distinct maintenance/recovery action for replacing a
  healthy live Host. Archived resumable Sessions use **Restore & Resume**;
  a legacy unknown/non-resumable archive can only be restored. New archive
  requests fail closed unless the exact managed launch has durable resume
  evidence. Replacement paths retain the existing metadata/reference transfer
  rules. Archive state deliberately does not carry when a Session is restored.
  Remove prunes all shared references. See Resume recipes below.

### Recent ordering (sidebar + shared ⌘K/^K contract)

The sidebar and both palettes use one lifecycle rank: working sessions
(starting/busy) lead, then latest shared lifecycle timestamp. The sidebar
adds its persistent filing structure around that rank: all live rows first,
then a combined preview of naturally stopped rows followed by the most recent
archived rows. The inactive preview is a user-set window
(`sidebar_stopped_limit` remains the compatibility key in `app-state.json`,
default 5, shared by the app's Settings ▸ Advanced ▸ Cleanup and, before its
2026-09-03 removal, the TUI's Settings ▸ Cleanup). A Session that dies or stops on its own is never archived
implicitly; past the preview it is hidden from the sidebar only and remains
available through search/history until the user archives, resumes, removes, or
pins it. Reading/marking a Session does not move it. Everything is derived
from shared Host artifacts and `updatedAtUnixMs` is published to Controllers,
so local, headless, and remote views do not invent their own recency from
Controller-local files. The Rust helpers are
`session_ops::latest_lifecycle_ms` / `recents_recency_ms`; the now-removed
TUI's rows were sorted by `sessions::compare_recent`. The app's in-memory `sessionMRU` survives only
for the ⌃Tab switcher, which is deliberately within-app selection order, not
the shared list.

The full **All recent** page remains an event history: Active work first, then
persisted start/input/finish/exit events newest-first. A session list can show
only one row per session, so its lifecycle rank is the latest shared event that
corresponds to that history.

### Archiving sessions

**Archive is the non-destructive "stop and file away" verb; Remove stays the
destructive one — and it is also THE stop verb (the separate "Stop"
context-menu item was folded into it 2026-08-05: the live-session item reads
"Stop and archive"; `stopSession` survives for the phone).**
Archiving stops the hosted PTY (the same identity-guarded
kill path as Remove, plus `__browser_cleanup__`) but keeps the whole session
dir — manifest, `output.bin`, artifacts, provider-id overlay — so the archive
can use **Restore & Resume** to bring back the provider conversation via
`ResumeCommand`. Plain **Restore** remains only for legacy archives created
before exact resume evidence became mandatory.
That
replacement does not carry the old terminal scrollback; the saved output is
readable only until replacement Resume removes the old Session directory. The
shared `archived.json` marker inside the Session directory is the
cross-frontend truth. The native UserDefaults overlay
(`NativeOverlay.archivedSessionsKey`) remains a compatibility/cache layer and
is adopted from markers during rescan. Archive state is pruned on true removal
and deliberately NOT carried across replacement Resume (resuming an archived
session is "bring it back").

**Sidebar model (reworked 2026-08-05, `sidebarLists` in `UnpeelStore`):**
each project's list is ACTIVE (live) subtree blocks first — never truncated —
then naturally stopped blocks followed by archived blocks, with that combined
inactive tail capped at `sidebarVisibleSessionLimit`; the default is 5.
Selected, unread, keep-visible, and in-flight inactive blocks always stay past
the cap.
"Recently updated" applies the shared lifecycle rank inside the live and
naturally stopped sections without removing the archive boundary. A user-initiated
archive stamps recency
(`NativeOverlay.archivedAtKey`) so the row glides to the TOP of the fixed
bottom archive section (live hosts first show a muted spinner
"archiving" row via `archivingSessionIDs` while stopping). The final exited
manifest timestamp also makes a newly stopped row recent, but never archives
it. Hidden natural stops remain searchable; only a real `archived.json` marker
places a Session in the archive library,
opened from the project context menu's **"Archived (N)"** on desktop (the
now-removed TUI used **`a`** or the same menu) — there is no sidebar footer row
(2026-08-10; the row itself
had replaced "Show N more"; the show-all state and
`UNPEEL_SHOW_ALL_SESSIONS` snapshot hook are gone). Recent archived rows
therefore DO render in the sidebar (primary action: "Restore & Resume";
plain "Restore" is retained for legacy unknown/non-resumable archives and does
not relaunch them).
**Archive wins over Session pin/manual order:** an archived Session always
renders in the fixed bottom archive section, newest-filed first, and exposes no
drag slots. Its durable pin/order metadata is retained so restoring it returns
the row to the prior regular section. Plain-group pins affect only sibling
display order; they do not alter Session ownership or archive state.
The phone mirrors the model: the snapshot sends the same
displayed list (`displayedSessions` per node, `archived` flag on
`RemoteSessionSummary`), and the iOS sidebar applies the same
partition/window itself so it also behaves against older Macs; the phone's
"Show more" is gone and its session sheet offers Stop and archive, **Restore &
Resume** for a resumable archive, or plain **Restore** otherwise (standalone
Stop only against Macs without archive support).
Entry points: the row's hover
archive button and the context-menu item (`requestArchiveSession` — actively
working sessions get an inline "Stop and archive?" confirm; settled ones
archive directly). Archive is offered only when the launch names a managed
runtime with a resume recipe AND this exact Session has durable provider-owned
state: a non-empty provider transcript, a real lifecycle event paired with its
provider id, or populated managed per-Session storage. A pre-minted id or empty
directory alone is not enough. Non-resumable rows show Remove/X instead, and
the Host revalidates the same evidence before accepting an archive request.
Neither inactivity cleanup nor natural process exit archives such a Session.
The single
Settings ▸ Advanced ▸ Cleanup picker (2026-08-12: the old separate
auto-stop-minutes and auto-archive-days pair was merged) is **"Auto-stop
and archive inactive terminals"**: sessions continuously idle past the
cutoff get the same `archiveSession` treatment as clicking "Stop and
archive" — skipping pinned, selected, unread, busy/attention, and any Session
without exact durable resume evidence — and never delete anything. Its continuous-idle clock
uses the same shared lifecycle timestamp as Recently updated; raw terminal
repaints cannot keep a hook-owned idle agent alive forever. The knob is shared
state: `auto_stop_archive_minutes` in `app-state.json` (absent = on at 1 day,
explicit 0 = off; the app folds its legacy
`unpeel.native.autoSessionStopMinutes` UserDefaults value in once); the sweep
now runs in the `unpeel-serve` driver (`auto_archive.rs`), previously also in
the now-removed TUI.

### Restart Recommendation API

Long-lived sessions can survive an app update and keep running an older
`unpeel-host`. The native app does not force-kill them. Instead it exposes a
small session-level restart recommendation API:

- `crates/unpeel-core/src/session_host.rs` writes `host_build_id`,
  `host_protocol_version`, the managed runtime generation, and registration
  evidence plus the saved MCP domain grants into every hosted-session
  manifest. Treat `host_build_id` as
  diagnostic only. `host_protocol_version` is the stable compatibility lever;
  bump it only when an existing live host should recommend restart to pick up a
  protocol/capability change. `mcp_client_registered` records whether the
  Sessions MCP client was actually injected into a managed launch; a blank
  shell is `false` even if `mcp_enabled` lets a manually configured provider
  use the local server. Browser and Computer use the same grant/evidence split.
- `UnpeelStore.restartRecommendations` is the native derived API:
  `[session id: SessionRestartRecommendation]` (a `{ token, message, action }`
  value). It
  is rebuilt during `rescan()` from live manifests by
  `restartRecommendation(for:)`. Missing
  `host_protocol_version` is treated as unknown and does **not** recommend
  restart by itself, because older sessions may be functionally fine but
  predate the metadata field.
- **One trigger today:** the live host's `host_protocol_version` is older than
  the app's required version (token `host-protocol:<n>`).
  (An earlier `mcp-access:on` trigger was removed — sessions launch with the
  MCP client by default, and the host's per-call gate handles revokes live.)
- `TerminalArea.swift` renders `RestartRecommendedBar` directly under the
  title bar and above the terminal when the selected live session has a
  recommendation. The recommendation carries its operation: an old known
  Host protocol calls **Reload Terminal**, because only replacing the Host can
  update it.
- The X button only dismisses the current recommendation token. Dismissals are
  stored in `NativeOverlay.restartRecommendationDismissalsKey` and pruned with
  the session. Raising the required host protocol changes the token, so the bar
  can appear again for sessions that report an older known protocol.

Use this API for "session should restart, but does not have to restart right
now" cases. If a future feature needs a restart to apply safely, add a stable
recommendation token/reason/action through
`restartRecommendation(for:)`
instead of introducing a second banner path. Avoid build timestamp/mtime
comparisons for restart UI; they are too noisy after ordinary rebuilds and
signing.

### Resume recipes

`session_ops::relaunch_command` is the canonical Rust derivation used by both
in-place Resume Agent and replacement restore/handoff paths. The live Host
owns `SessionHostCommand::ResumeAgent`; local callers use
`session_ops::resume_agent` / `unpeel-host __resume_agent__`, and Controllers
use the additive protocol-minor-6 `session.runtime.resume` capability with the
`resume_agent` action. Legacy `restartAgent`/`restart_agent` decoding and route
support remain for compatibility only: current summaries omit the old
per-Session capability and current clients do not surface it. The older
`unpeel-host __resume__` derivation remains for operations that need a command
before spawning a Host. `ResumeCommand.hostRelaunchCommand` is the native thin
caller; its Swift derivation remains a shipped fallback until the Host path has
lived for one release. The result makes the agent pick up its previous
conversation through three mechanisms, in order of strength:

The Host is authoritative even when a Controller cached an eligible summary.
Immediately before input it repeats the owned-login-shell, foreground, retained
job, and pending-launch proofs described above. It sets
`runtime_launch_pending` before writing Ctrl-U plus the command as one PTY
submission; duplicates receive a conflict until the expected runtime appears or
the completion wrapper returns control. This preserves terminal output and
scrollback but does not add a promised entry to the user's shell command
history.

- **Minted at launch** (claude, gemini, grok): `spawnSession` pre-assigns the provider conversation id itself — `ResumeCommand.mintedLaunch` appends `--session-id <uuid>` to every fresh launch that supports pre-assignment and records the uuid in the provider-id map. Precise resume is then guaranteed from second zero, independent of hook delivery (a session whose provider crashes before its first hook event still resumes the exact conversation). `resumed()` treats the creation flag in the command as the precise target and rewrites it to the provider's resume form where necessary; `fresh()` strips it so the respawn mints a new one. Current Kimi Code is deliberately excluded: its `--session` option only resumes an id Kimi already created.
- **Hook-captured precise** (all hook/plugin providers): the provider forwards its conversation id in hook POSTs (`session_id`/`thread_id`/`conversation_id`…). The native hook server captures it (`HookEvent.providerSessionID`, `HookServer.swift`) and `UnpeelStore` persists a `unpeel-id → provider-id` map in a UserDefaults overlay (`NativeOverlay.providerSessionIDsKey`), pruned with the session in `pruneNativeState`; replacement Resume reads it **before** the prune, with the manifest's `provider_session_id` as fallback. Precise forms per CLI: `claude`/`gemini`/`grok`/`cursor-agent`/`copilot --resume <id>`, `cline --id <id>`, `codex resume <id>`, `amp threads continue <id>`, `opencode --session <id>`. Capture is latest-wins and outranks a minted id in `resumed()`, so a user who switches conversations *inside* the tool (`/resume`, `/clear`) re-targets later resume to the conversation they actually ended up in; claude reports the switch immediately (its `SessionStart` hook, forwarded as metadata-only `HookSeen`, fires on in-tool resume/clear with the new id), other providers on their next hook-bearing event.
- **pi storage pinning**: pi exposes no conversation id to hooks, so `spawnSession` pins each pi session to its own storage dir instead (`ResumeCommand.pinningPiSessionDir` appends `--session-dir ~/.unpeel/pi-sessions/<session-id>`). Resume Agent's `--continue` is then exact by construction — the dir holds only that session's conversations. The dir rides the relaunch command across later resumes; `confirmRemoveSession` reaps it on true removal (`unpeelManagedPiSessionDir`).

Sessions with none of the above (or launched by older builds before an id was captured) fall back to the provider's **continue-last** flag (`codex resume --last`, `gemini --resume latest`, `amp threads continue --last`, `--continue`). Exact for worktree sessions (own cwd); in a shared project root it resumes whichever conversation ran there most recently. Cline is the exception: it has no continue-last flag, so an older id-less session opens `cline history` for an explicit choice.

The rewrite is idempotent (skips when a resume flag is already present;
replaces a stale id with the freshly-captured one) and shared by native and
headless lifecycle paths. An in-place relaunch commits the rewritten stable
command only after its bytes were accepted by the same PTY, advances
`runtime_launch_generation`, and clears the old durable hook seed. Fresh
Sessions launched by user/controller flows never resume; they do get minted
ids because minting happens in `spawnSession`.


### Titling from a resumed conversation

When a provider-id capture *changes* a session's conversation identity (the user ran `/resume` inside the tool — the typed line was a slash command, which auto-titling skips), the still-untitled session is titled from the conversation it now points at: `transcripts::auto_title_session_from_transcript` reads the transcript head (Claude's `summary` record when present, else the first real user prompt; compact-continuation preambles and anything `normalize_prompt_title` rejects fall through) and applies it via `apply_manifest_auto_title`, so all the settled/custom-title rules hold. It refuses the `cwd_match` transcript fallback — only an id- or captured-path-anchored resolution may title, since cwd discovery can land on a different conversation. Triggers: the app shells `unpeel-host __auto_title__ <id>` from `recordProviderMetadata` when the provider-id map changes; the `unpeel serve` hook listener calls it in-process when `session_ops::set_provider_session` reports a marker change (so headless hosts title too; the now-removed TUI's hook listener did the same). Best-effort and idempotent — no transcript yet, settled title, or nothing normalizable all no-op.
