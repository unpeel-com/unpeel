<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Session Activity State

Busy/idle/attention authority lives in the Rust workspace worker
(`session_activity.rs`); the native `SessionActivity.swift` engine is a
read-only startup seed of the Host-published `activity-state.json` /
`last-hook-event.json` (the no-flash fallback until the first complete Host
snapshot). Since 2026-09-03 the app ingests no hook events itself: the
`HookEvent`/`handleHookEvent` path, deferred Stop effects, the busy sweep,
`persistActivitySnapshot`, and the menu-prompt notification publisher are
deleted; every edge below describes the worker.

Animated Busy has one fail-closed authority: explicit lifecycle events. A
shell, build, server, pager, watcher, unknown CLI, or recognized hookless agent
remains visually idle no matter how often it prints or repaints — terminal
output is not evidence of semantic agent work. This keeps every non-authority
Session iTerm-like and prevents intermittent or perpetual sidebar spinners.
Runtime observation still provides identity, icon, tint, capabilities, and
safe hook-install repair; it does not grant activity authority. Hookless
agents such as Pi and fx therefore have no animated Busy state until they gain
an authoritative lifecycle source in their runtime package.

The Host's live foreground-runtime observation also grants hook authority
(2026-08-21). A hook-capable agent the user starts by hand inside a blank or
custom-command terminal becomes hook-owned once a live hook event latches:
provider hook installs are global and the hosted shell exports the session's
hook env (`UNPEEL_SESSION_ID`, `UNPEEL_APP_PORT`, the port registry), so a
typed `claude` reports exactly like a launched one. Until that first live
event the Session stays neutral: observing a hook-capable process is identity,
not proof that it is working.
Because live hook events carry no runtime identity, the latch in a reusable
shell is tied to the observed foreground process (`id:pid:pid_started_at` —
the kernel start time closes the pid-recycle window): a new observed
identity drops the previous latch (both frontends — `observe_foreground_runtime`
in the serve `ActivityEngine`, `observedForegroundIdentities` in
`UnpeelStore`), so a stale busy/attention latch from a killed run can never
speak for its replacement, and an old Claude latch never crosses to a later
Codex in the same shell. The host also self-heals hook installs on the
observation edge: observing a hook-capable runtime runs the same idempotent
`install_runtime_support` the managed spawn path uses, so a user who only
ever types agents into blank terminals gets hooks from their second
invocation onward (skipped under `UNPEEL_TEST`; the already-running process
stays neutral until it emits a live hook because providers read hook config at
startup). The first sighting after an engine/app start is
deliberately not an edge — it must keep a latch built from live events already
accepted. The disk seed (`last-hook-event.json`) also carries no runtime
identity and therefore stays launch-command-gated; observed-only sessions
latch from live events only. The observation still selects live
sidebar/icon/tint presentation, and `menu_prompt_active` still provides
attention. Output-policy flags (such as Grok's attention-clears-on-output opt-out)
follow the launch binding first, else the observed runtime.

Hook-driven sessions:

- `Start` and `UserPromptSubmit` mark the session busy
- `Stop` marks it idle
- `PermissionRequest` marks attention
- Child hooks update independent background activity; `SubagentStop` never
  completes the main turn or another child.
- Only a successful `Stop` counts as completed work. `StopFailure`, the idle
  timeout, and a user cancellation settle activity without a Finished event or
  a completion notification.
- `StopCancelled` is a provider-confirmed cancellation: it settles activity
  and suppresses late attention/completion hooks until the next opening hook.
- An installed App may separately POST a bounded `alert` to
  `/notify/<session_id>`. This appends shared Recent/unread activity and may
  deliver macOS/phone notifications, but it never enters this lifecycle
  reducer, changes busy/idle/attention, or updates `last-hook-event.json`.
- Known hook-capable tools (Claude, Codex, Cline, Cursor Agent, Grok, Kimi,
  Kiro, OpenCode, Amp, Gemini, Copilot) do not use raw output growth to enter busy while waiting for
  the first hook event. This avoids false spinners when a full-screen TUI
  repaints during user scroll or window resize after an app restart.
- The first hook event latches the session as hook-owned; ordinary terminal
  input never starts Busy. Activity follows hooks, the explicit runtime-owned
  Escape cancellation contract below, and the
  5-minute output-rearmed timeout.
- **Settled turns stay idle, including Codex.** Terminal redraws after a
  `Stop`, `StopFailure`, `StopCancelled`, or `Idle` never reopen the turn or
  erase its completion outcome. The old Codex output-based Stop workaround
  was removed after a real final Stop was followed by a redraw twelve seconds
  later, resurrecting Busy. A new turn needs an opening hook; independently
  tracked children keep their own lifecycle. This applies to the Host and
  the Mac startup seed, including after a restart.
- The latch survives app restarts via a durable seed: every provider hook
  script also writes its last lifecycle event to
  `~/.unpeel/app-sessions/<id>/last-hook-event.json` (atomic write; path from
  `UNPEEL_SESSION_DIR`, exported by the host next to `UNPEEL_SESSION_ID`).
  Hook scripts keep firing while no app instance is listening — the port POST
  just fails — so the file records transitions that happen with the app
  closed. On every rescan the Host checks this file for missed transitions.
  For an **open turn** (Start/UserPromptSubmit with no Stop recorded after
  it), the inactivity lease is anchored at `max(event mtime, output.bin
  mtime)` because turns routinely outlive the five-minute hook timeout.
  Event ordering always uses the event's own mtime, so a newer terminal
  repaint cannot suppress a subsequently recovered Stop. For other events
  only the event's own mtime is used, so
  a recorded Stop stays idle no matter how the TUI repaints and a dead
  mid-turn session (both timestamps stale) expires through the ordinary
  5-minute timeout on the first sweep. This restores busy/attention spinners
  for sessions that were mid-turn when the app closed, and correctly stays
  idle when the turn finished while it was closed.
  Once that lease expires, `hook-expiry.json` records a generation-scoped
  watermark under an exclusive lock and announces it on the state bus.
  Restarting the Host or repainting an idle terminal cannot revive that
  expired opener; a newer opening hook or a new runtime launch can.

Recognized non-hook agent sessions:

- Foreground observation still selects provider presentation and capabilities.
- Output growth and `screen_changed_at` remain terminal/recency telemetry, not
  lifecycle authority, and never start an animated Busy state.
- `menu_prompt_active` may still surface Attention when the Host positively
  recognizes an agent-drawn input menu.

Agent-drawn select menus (attention, host-side):

- Agent-rendered "pick an option" menus (Claude/Codex numbered prompts) fire
  **no** hook — no `Stop`, no `PermissionRequest`. The **host** closes this gap
  without guessing Busy from output: it
  already maintains a live parsed viewport per session
  (`TerminalViewportState`), so a 500ms scan thread in `session_host.rs` runs
  the shared detector (`crate::menu_prompt::viewport_has_menu_prompt`, the
  Rust twin of the iOS `menuPromptActive` scan — keep the marker lists aligned)
  over `current_screen_text()` and **edge-writes** `menu_prompt_active` into
  `manifest.json`. Because it lives in the host, it covers **every** session,
  not just ones with a warm Ghostty surface.
- Native reads the flag during `rescan()` and overrides `status → .attention`
  (in `UnpeelStore`), which swaps the busy spinner for the existing yellow
  `AttentionDot` and rolls up to collapsed folders + the iOS `blocked` status
  for free. A generation-bound false → true edge also emits the ordinary
  needs-input notification exactly once; a matching `PermissionRequest` hook
  and visual edge deduplicate whichever one arrives second. The initial app
  scan only seeds state; a session first discovered later can alert even when
  its first sample is already active. False re-arms the next menu. Both the
  badge and visual-edge notification are gated by
  `menuAttentionDetectionEnabled` (Settings ▸ Notifications, default on;
  the `unpeel.native.menuAttentionDetection` UserDefaults overlay).
- The iOS terminal's on-screen menu control bar keeps its own Swift viewport
  scan (it needs the option count + real-time keys); this host flag is the
  desktop badge path, not a replacement for it. Claude's persistent subagent
  selector (`↑/↓ to select · Enter to view`) is passive, including while its
  footer is only partially painted; neither detector may turn that status row
  into attention. Keep the Rust and Swift regression cases aligned.
- Approval menus may show only `Esc to cancel · Tab to amend`, without an
  arrow-key or Enter hint. That footer qualifies when a nearby numbered
  choice list has a selected row; the footer alone and transcript lists
  elsewhere do not. The Claude fixture lives in
  `runtimes/claude-code/fixtures/approval-menu.txt`.
- A `PermissionRequest` latch survives menu redraws. Before allowing changed
  output to clear hook-owned Attention, the workspace worker checks the live
  parsed viewport with the current detector. This read happens only on changed
  permission screens, has a 100ms socket timeout, and preserves the latch and
  output baseline for retry if the read fails. It also covers sessions retained
  by an older PTY core whose menu scanner predates the footer. Once the menu
  disappears, normal hook-owned activity resumes; a Stop remains authoritative.

Unread badges integrate with hook events and activity transitions (settles while unobserved → unread).

### Escape cancellation and hook delivery

Claude documents that Escape interrupts a response or tool call and that its
Stop hook **does not fire on a user interrupt**. Its runtime adapter opts into
`Integration::with_escape_cancellation()` for this missing lifecycle edge.
Other runtimes keep their own hook contracts until their Escape behavior is
verified and explicitly opted in. This currently covers managed Claude, Muse, and Gemini
launches; an agent typed into a blank terminal does not inherit this policy.
Sources: [interactive mode](https://code.claude.com/docs/en/interactive-mode)
and [hook reference](https://code.claude.com/docs/en/hooks).

Muse 1.0.3 was verified with its real `--provider echo` TUI: ESC ends the
foreground turn without firing Stop, so its adapter uses the same fallback.
This settles foreground activity; Muse's background tasks have their own
controls. Ctrl+C depends on composer contents and is not inferred as cancel.
See [Muse's interrupt behavior](https://dev.meta.ai/docs/muse-code/interactive#steering).

Gemini 0.57.0's installed `useGeminiStream` handler aborts on bare Escape.
Its request generator returns early on `signal.aborted`/AbortError, before
`AfterAgent`, so Gemini opts into the same fallback. `AfterTool` is metadata,
not a new turn: a delayed tool callback must not revive cancelled activity.
See [Gemini's source](https://github.com/google-gemini/gemini-cli).

Grok reports cancellation through its native `StopCancelled` hook (ESC,
Ctrl+C, and client stop buttons); `Stop` deliberately does not fire then.
Its installer registers `StopCancelled` separately from `Stop` and preserves
`StopFailure` as a failure. Grok uses these native lifecycle events, so ESC
dismissing a menu, leaving a composer mode, or navigating in vim mode cannot
falsely cancel Host activity. A native cancellation needs only the next
opening hook to resume, including prompts sent by another client.
See [Grok's hook contract](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/10-hooks.md#hook-events).

The attach client releases a standalone Escape after a 250 ms wait for a
possible focus-report continuation. The PTY Host independently tracks only
successfully delivered user input, across Write commands and StreamInput
frames. It waits 150 ms to distinguish bare Escape from a fragmented terminal
sequence, ignores bracketed paste and modified keys, and recognizes unmodified
Kitty Escape presses. Parsed menus have no cancellation authority. Input
parser state survives core handoff; query replies and launch commands bypass
the parser. The actual input bytes are neither consumed nor rewritten.

For an opted-in runtime, the Host writes `hook-cancellation.json` under the
Session directory, using the shared file lock and state bus. It records the
launch generation, cancellation time, and first later submitted Enter. The
worker settles an existing hook latch and rejects late activity hooks until a
new opening hook follows that submission. Enter alone never starts Busy. A
fast opening hook may arrive before the timer persists Enter; the reducer
retains it until submission evidence arrives. The paste recipe's second Enter
does not replace the first submission time. Cancellation survives worker
restart and never crosses a runtime generation.

This is cancellation **intent**, not process-exit proof: it sends no signal,
does not kill hook processes or uninstall hooks, and does not infer completion
from terminal text. Unrecognized provider modes that repurpose Escape remain
a limitation; menu detection only excludes the prompts it can identify.

Shell reporters are inert outside a hosted Session, honor `UNPEEL_HOME`, and
bypass HTTP proxy settings for loopback delivery. They finish delivery before
returning by default: the direct port gets a bounded attempt, then registry
ports are contacted concurrently and awaited. Invalid and duplicate registry
ports are ignored. Claude's installer migrates its owned `async: true` entries
to synchronous reporting while preserving unrelated user hooks. The explicit
`UNPEEL_HOOK_POST_SYNC=0` legacy opt-out forfeits ordering guarantees.
Script installation uses a locked, atomic replacement with executable
permissions set before publication; unchanged scripts keep their inode.
Concurrent writers use distinct temporary files, and shared hook settings
hold their file lock across the entire merge and replacement.

The worker also checks the durable lifecycle seed after hooks have latched,
so a lost HTTP Stop can recover on the next scan. It reads bounded bytes and
metadata from the same open file, rejects older generations and seeds older
than accepted activity transitions, and applies cancellation fences during recovery.
Metadata-only Notify events cannot erase the last durable lifecycle event.

Regression coverage: `hook_cancellation.py`, `muse_cancellation.py`, `gemini_cancellation.py`, and
`grok_cancellation.py` in the
CLI PTY matrix (input fallback and installed native hooks), the attach test `lone_escape_reaches_host_without_a_second_key_or_eof`, the core input
parser/marker tests, activity reducer tests, and runtime reporter conformance
tests (broadcast delivery, proxy isolation, silent peers, and no-op behavior).
The PTY harness isolates `HOME` and provider/XDG config roots as well as
`UNPEEL_HOME`: a private Unpeel directory alone does not prevent a real
provider settings file from receiving temporary hook registrations.
Set `UNPEEL_MUSE_TEST_BINARY` to an installed Muse binary when running
`crates/unpeel-cli/tests/run.sh muse_cancellation` to exercise the real echo
provider instead of the deterministic substitute, still in a private HOME.

### Background agents

The Host keeps foreground lifecycle and background hook activity separately.
A successful main Stop or a foreground ESC leaves the Session busy while a
tracked child is active. The Session settles after the last child stops;
foreground cancellation/failure still cannot produce a Finished notification.
Attention takes precedence over background busy state.

Claude's installed `SubagentStart`/`SubagentStop` hooks identify each child by
`agent_id` ([provider contract](https://code.claude.com/docs/en/hooks#subagentstart)).
The reporter atomically creates `background-hooks/<runtime-generation>/<id>.json`
on start and removes only that file on stop, then broadcasts the ordinary hook.
Separate files avoid concurrent child updates overwriting each other or the
main `last-hook-event.json`. The Host reconciles the markers on rescan, including
after a restart or missed POST; duplicate/unknown stops are harmless. A new
managed runtime generation cannot inherit its predecessor's children.
The current generation directory's modification time also advances the shared
unread, Recent, and archive clock when the last child finishes.

Background leases use the existing five-minute inactivity fallback: output
changes may maintain hook-established work, but cannot create or revive it.
An expired child never produces a completion notification. This fallback bounds
stale state when a provider fails to emit its child stop. The reporter rejects
missing/unsafe IDs and untagged launches. Background shells, servers, and
watchers have no inferred lifecycle authority, and other providers retain their
current behavior until their runtime packages report equivalent child events.

Existing Claude processes need their hook configuration reloaded or a new
provider invocation to register these two events. Children started before the
new hooks were loaded cannot be reconstructed from a terminal counter.

### Runtime cancellation coverage

Audited 2026-09-06 against all 14 shipped runtime packages. Native event
normalization belongs in each package; the Host only consumes the common
`Stop`, `StopFailure`, `StopCancelled`, and `Idle` outcomes. An `Idle` backstop
settles missing stops without changing an already-known outcome or creating
a completion. Neither cancellation nor
failure produces a Finished notification. Tool/session metadata does not
replace a durable cancellation seed or reopen the cancelled turn.

| Runtime | Cancellation and completion contract | Verification |
| --- | --- | --- |
| Amp | `agent.end.status`: `done`, `cancelled`, `error` become success, cancellation, failure. | [Plugin API](https://ampcode.com/docs/plugin-api); executable plugin tests. |
| Claude Code | Managed-launch ESC fallback because user interrupts skip Stop. | Hook reference; real Host/PTY regression. |
| Cline | `TaskCancel` becomes cancellation; `TaskError` failure; task completion stays success. Tool callbacks only carry metadata. | Installed hook contract; real reporter HTTP/seed tests. |
| Codex | Registers native `Interrupt` with a three-second timeout; legacy `turn_aborted` also becomes cancellation. | [Hooks reference](https://learn.chatgpt.com/docs/hooks); installed binary contract; real normalizer/transport tests. |
| Cursor Agent | `stop.status=aborted` becomes cancellation; `error` failure; `completed` success. | [Hooks reference](https://prod.cursor.com/docs/hooks); real reporter tests. |
| fx | No lifecycle authority; no animated busy state. | Shipped catalog and Host authority guards. |
| Gemini | Managed-launch ESC fallback for aborts that skip AfterAgent. | Installed 0.57.0 source; real Host/PTY regression with substitute provider. |
| GitHub Copilot | Registers per-turn `agentStop` and `permissionRequest`; `sessionEnd.reason` distinguishes cancellation and failure. Single ESC does not imply cancel. | [Hook reference](https://docs.github.com/en/copilot/reference/hooks-reference); reporter tests. CLI not installed here; in-turn interrupt emission remains unverified. |
| Grok | Native `StopCancelled` and `StopFailure` remain distinct from Stop. Managed ESC fallback covers early rewinds that omit cancellation hooks. `idle_prompt` repairs other omitted stops without claiming success; child turn events are ignored, and SessionStart preserves the durable outcome. | Installed-hook Host/PTY regression including early rewind, missed delivery, and repeated Host restarts; early rewind omission reproduced with live Grok 1.0.13. |
| Kimi | Native Kimi Code `Interrupt` becomes cancellation. The older Python configuration does not accept that event. | [Native hooks reference](https://moonshotai.github.io/kimi-code/en/customization/hooks.md); reporter/config tests. Installed legacy Python build has no hooks; native Kimi Code not live-tested. |
| Kiro | Uses native Stop; no inferred ESC because cancel can immediately submit queued steering. | Reporter/transport tests; [queue steering contract](https://kiro.dev/docs/cli/chat/queue-steering/). CLI absent; cancellation Stop emission remains unverified. |
| Muse | Managed-launch ESC fallback; background work retains provider-owned controls. | Real 1.0.3 echo-provider cancellation and next prompt, plus Host/PTY regression. |
| OpenCode | Root `session.error` records cancellation/failure; the following idle settles it. A successful response after recovery clears the error. Child sessions and duplicate idle events are ignored. | [Session processor](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/processor.ts); executable ordered-event plugin tests. |
| Pi | No lifecycle authority; no animated busy state. | Shipped catalog and Host authority guards. |

An app restart preserves running providers, including their loaded hook
registrations. After a Grok registration update, `/hooks` → `r` reloads the
files without restarting the agent. Legacy per-session PTY Hosts also keep
their original input policy while alive; a new Grok Session is needed to
pick up the immediate ESC fallback on those older Hosts. The native idle
backstop works in existing Sessions after reloading hooks, but Grok 1.0.13
can delay it for a minute after an early rewind.

Copilot's [cancel controls](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/cancel-and-roll-back)
use a second ESC, with queued prompts and dialogs taking precedence. Kiro also
has configurable interruption and automatic queued-prompt submission. Neither
can safely reuse a bare-ESC/next-Enter policy. Kiro's current hook docs and
the v3 migration table disagree on AgentSpawn versus SessionStart; retain the
migration-table registration until a real supported binary verifies a change.

`bun run test:runtimes` executes the Amp/OpenCode plugin callbacks, including
overlapping parent/child events, cancelled/error outcomes, and retry recovery.
Rust reporter tests exercise all ten shell transports against multiple stalled
ports, proxy variables, duplicate ports, generation tags, and restart seeds.
These tests exercise shipped integration code; they do not imply authenticated
end-to-end tests against every provider service.

After upgrading, provider hooks loaded at startup require a new provider
session or the provider's hook reload command. The ESC policy lives in the
persistent PTY Host: an old PTY keeps its old policy across a Controller/worker
restart, so test newly added fallback support in a newly created Session.

### Recent ordering and automatic cleanup

Recent ordering and auto-stop/archive consume the same provider-aware
lifecycle timestamp. Hook-capable tools use `last-hook-event.json`; raw
`output.bin` growth is never a fallback for them because attaching, resizing,
and idle TUI repaints can append bytes without real work. Hookless tools prefer
the Host's `screen_changed_at` and use output mtime only for legacy Hosts
without that field. Creation is the floor, and an exited manifest's final
`updated_at` records the exit event; a running manifest's heartbeat-driven
`updated_at` is never activity.

The cleanup clock advances only while the derived status is idle and only when
that canonical lifecycle timestamp advances. Selection, pins, unread results,
attention, active work, and plain shells retain their existing exemptions.

### Recent dropdown source

The titlebar and menu-bar dropdown share the sidebar's current Host projection
for the selected workspace, including its restart and attention overlays. The
native disk scan is only the startup seed; it must never supply active jobs or
unread rows after the sidebar switches to Host state. An empty current activity
slice clears an older cached Busy row.

Other workspaces use the background pool's Host snapshots. This includes the
app instance's own Local workspace while another workspace is selected; only
the workspace currently served by the foreground connection is excluded from
pooling. Rows remain qualified by workspace key, so switching scope cannot
relabel one workspace's sessions as another's.
