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
attention. Output-policy flags (grok's attention-clears-on-output opt-out,
codex's stop-distrust) follow the launch binding first, else the observed
runtime.

Hook-driven sessions:

- `Start` and `UserPromptSubmit` mark the session busy
- `Stop` marks it idle
- `PermissionRequest` marks attention
- An installed App may separately POST a bounded `alert` to
  `/notify/<session_id>`. This appends shared Recent/unread activity and may
  deliver macOS/phone notifications, but it never enters this lifecycle
  reducer, changes busy/idle/attention, or updates `last-hook-event.json`.
- Known hook-capable tools (Claude, Codex, Cline, Cursor Agent, Grok, Kimi,
  Kiro, OpenCode, Amp, Gemini, Copilot) do not use raw output growth to enter busy while waiting for
  the first hook event. This avoids false spinners when a full-screen TUI
  repaints during user scroll or window resize after an app restart.
- The first hook event latches the session as hook-owned; from then on raw
  terminal input never changes its busy/idle state — only hooks and the
  5-minute output-rearmed timeout do.
- **Codex exception — the stop-distrust guard (2026-08-11):** codex fires
  agent-turn-complete `Stop` notifications for *internal sub-turns* of one
  long run, so its long agentic turns used to show idle the whole time. For
  codex only, a hook-idle session whose `output.bin` keeps growing between
  5s and 90s after its latest Stop flips back to busy (then settles through
  the ordinary output-rearmed timeout). The 5s grace skips the turn's
  trailing render burst; the 90s window keeps later user scroll repaints
  from faking busy on a finished session. Implemented identically in
  `SessionActivityEngine` (`distrustStops`); the interactive terminal UI's own
  `ActivityEngine` (`distrust_stops`) implemented the same logic before it was
  removed 2026-09-03. The native scan additionally stats hook-idle codex
  sessions, which are otherwise skipped.
- The latch survives app restarts via a durable seed: every provider hook
  script also writes its last lifecycle event to
  `~/.unpeel/app-sessions/<id>/last-hook-event.json` (atomic write; path from
  `UNPEEL_SESSION_DIR`, exported by the host next to `UNPEEL_SESSION_ID`).
  Hook scripts keep firing while no app instance is listening — the port POST
  just fails — so the file records transitions that happen with the app
  closed. On rescan, `UnpeelStore.seedHookActivity` re-seeds an unlatched
  hook-capable session from this file (`LastHookEvent` in
  `SessionActivity.swift`). Seed timestamp: for an **open turn**
  (Start/UserPromptSubmit with no Stop recorded after it) the seed is
  anchored at `max(event mtime, output.bin mtime)` — turns routinely outlive
  the 5-minute hook timeout, and a fresh output.bin means the agent is still
  streaming right now; for everything else the event's own mtime is used, so
  a recorded Stop stays idle no matter how the TUI repaints and a dead
  mid-turn session (both timestamps stale) expires through the ordinary
  5-minute timeout on the first sweep. This restores busy/attention spinners
  for sessions that were mid-turn when the app closed, and correctly stays
  idle when the turn finished while it was closed.

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

Unread badges integrate with hook events and activity transitions (settles while unobserved → unread).

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
