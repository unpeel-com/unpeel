<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Provider Hook Details

Provider processes hosted by Unpeel never inherit an outer Herdr pane
identity. The generic Host launch chain strips every `HERDR_*` variable before
the detached Host starts and again before the provider PTY starts as general
child-env hygiene (the Herdr supervisor integration this once protected
 against a race with the interactive terminal UI's aggregate `custom:unpeel`
authority was itself removed 2026-09-03 along with that TUI).

Claude:

- Installs hook config into Claude settings (`SessionStart`,
  `UserPromptSubmit`, `Stop`, `StopFailure`, `PermissionRequest`)
- Emits lifecycle and permission events through the Unpeel hook server
- `SessionStart` is forwarded as `HookSeen` (metadata-only latch, like
  grok/kiro) — it fires at launch and on in-tool `/resume`, `/clear`,
  `/compact` with the new `session_id` + `transcript_path`, so a session
  where the user resumes a different conversation inside Claude re-links to
  it immediately (precise restart-resume and transcript reads follow the
  resumed conversation, not the minted-at-launch id)

Codex:

- Installs `~/.unpeel/hooks/bin/codex`
- Prepends wrapper dir into `PATH`
- Resolves and preserves the real Codex binary path
- **Primary lifecycle source is native Codex hooks**, not just notify. The
  Codex package installer (`runtimes/codex/adapter/setup.rs`) registers
  Unpeel-managed `SessionStart`, `UserPromptSubmit`, and `PermissionRequest`
  entries in `~/.codex/hooks.json` (pointing at the package wrapper/reporter)
  and enables Codex's `hooks` feature. This gives authoritative
  start/busy/approval events.
- Codex hook commands guard missing scripts and are reconciled whenever hook
  assets install: live hooks for side-by-side `UNPEEL_HOME` instances are
  preserved, while obsolete Unpeel entries are pruned. This prevents a deleted
  temporary/blank-instance hook from producing exit `127` after a reboot.
- The wrapper also still injects `-c notify=[...]` as the turn-completion (Stop/idle) source and a compatibility bridge for Codex builds without the `hooks` feature.
- The Codex package's notify normalizer maps raw event `type`s onto Unpeel states before calling the provider-neutral transport: `agent-turn-complete`/`task_complete`/`turn_aborted` → Stop, `task_started`/`exec_command_begin` → Start, `request_permissions`/`exec_approval_request`/`apply_patch_approval_request`/`approval-requested` → PermissionRequest.
- Codex's descriptor declares the inherited `CODEX_*` identity variables the generic Host boundary strips, so nested Codex sessions do not cross-fire hooks.

Amp:

- Installs an Amp plugin that maps agent start/end to Start/Stop notify events

Gemini:

- Installs Gemini hook config/settings integration (registered events:
  BeforeAgent, AfterAgent, AfterTool, Notification)
- Emits start/stop events through the hook server; Notification events with
  `notification_type=ToolPermission` map to PermissionRequest (attention).
  Other notification types are deliberately ignored — a broad
  Notification→attention mapping sticks sessions yellow (same rationale as the
  Grok hook matchers)

OpenCode:

- Installs a plugin under Unpeel-managed OpenCode config
- Plugin tracks busy/idle/permission events and calls the notify hook

Copilot:

- Installs a hook script
- Writes project-local hook config under `.github/hooks/unpeel-notify.json`

Cursor Agent:

- Installs cursor hook config/script

Grok (xAI `grok` CLI):

- Installs the generic event-posting script as `~/.unpeel/hooks/grok-hook.sh`
- Writes Grok-native hooks to `~/.grok/hooks/unpeel.json` (global hooks are
  always trusted, so no project-trust step is needed)
- Maps Grok lifecycle events onto Unpeel state: `SessionStart` → HookSeen
  (latch only — the CLI opened, not a turn), `UserPromptSubmit` → Start
  (busy), `Stop`/`StopFailure`/`SessionEnd` → Stop (idle),
  `Notification` `approval_required` and `PreToolUse` `ask_user_question` →
  PermissionRequest (attention)
- `GROK_SESSION_ID` is available to every hook and Unpeel mints the id at
  launch (`grok --session-id <uuid>`), so restart is precise
  (`grok --resume <id>`) — see Resume on Restart
- Grok also natively scans `~/.cursor/hooks.json` and `~/.claude/settings.json`
  for compatibility. Those Unpeel hooks are Claude/Cursor-shaped:
  `session_start` used to normalize to busy `Start` and, with Grok's idle TUI
  re-arming the 5-minute timeout, left every Grok session spinning. Hosted
  Grok therefore (1) ignores Claude/Cursor Unpeel hooks when
  `GROK_SESSION_ID` is set, (2) treats `session_start` as HookSeen at the
  hook server, and (3) disables `[compat.claude]` / `[compat.cursor]` hooks
  for every hosted session (per-session `GROK_HOME` overlay plus
  `GROK_CLAUDE_HOOKS_ENABLED` / `GROK_CURSOR_HOOKS_ENABLED`). That overlay
  is not limited to auto-theme resolution: Claude settings often interpolate
  unset `$VAR`s as a skip-if-missing check, and Grok refuses to run those
  hooks with a red `required env var(s) not set` line. Native `unpeel.json`
  is the lifecycle source.

Kimi (Moonshot `kimi` CLI, current Kimi Code and legacy Python generations):

- Installs `~/.unpeel/hooks/kimi-hook.sh` and reconciles Unpeel-managed
  `[[hooks]]` entries in both `~/.kimi-code/config.toml` and legacy
  `~/.kimi/config.toml` without removing user hooks
- Maps `UserPromptSubmit` to busy, `Stop`/`StopFailure`/`SessionEnd` to idle,
  current Kimi's `Interrupt` to idle, and permission requests/notifications to
  attention; question menus remain controllable through the shared rendered-
  viewport menu detector
- Forwards Kimi's provider-created `session_id` and exact current
  `wire.jsonl` or legacy `context.jsonl` path to Unpeel
- Uses exact `kimi --session <id>` restart after SessionStart captures the id;
  `--continue` is the fallback before capture or for older sessions
- Current Kimi Code receives Sessions and Browser MCP through environment-gated
  persistent entries in `~/.kimi-code/mcp.json`; legacy Kimi receives
  repeatable `--mcp-config-file` flags while preserving `~/.kimi/mcp.json`

Cline (`cline` CLI):

- Installs managed native global event files under `~/.cline/hooks` plus
  `~/.unpeel/hooks/cline-hook.sh`; the hooks no-op unless
  `UNPEEL_SESSION_ID` is present and coexist with other supported filename slots
- Maps TaskStart/TaskResume, tool, completion, cancellation, shutdown, and
  error hooks to UserPromptSubmit/Start/Stop/StopFailure and forwards
  `sessionContext.rootSessionId`
- Resumes exactly with `cline --id <id>` once TaskStart reports the id; older
  sessions without one open `cline history` because Cline has no continue-last
  flag
- Reads semantic `<id>.messages.json` transcripts (messages, reasoning, tools,
  model, and usage)
- Injects Sessions and Browser MCP through a merged per-session copy selected
  by `CLINE_MCP_SETTINGS_PATH`; the user's global MCP file is never rewritten
- Isolates Cline's detached hub per hosted session with
  `CLINE_HUB_DISCOVERY_PATH` plus an ephemeral port. The shared default hub
  retains its starter's environment and would otherwise reuse the wrong
  Unpeel identity/MCP grants. The scoped hub is stopped on provider/shell exit.
- Does not use Cline 3.0.44's advertised `--hooks-dir`: current source assigns
  `CLINE_HOOKS_DIR` but never reads it when resolving hook paths
- Cline exposes no approval-request hook, so custom `--auto-approve false`
  prompts have no distinct hook-driven attention state
- Full findings: `unpeel-apple:docs/feature/cline-cli-integration.md` (private)

Pi:

- No hook-port integration today; Pi has no animated Busy authority. Its
  output remains terminal/recency telemetry and menu detection may still
  surface Attention.
- Each pi session is pinned to its own storage dir at launch
  (`--session-dir ~/.unpeel/pi-sessions/<session-id>`), which makes restart's
  `--continue` exact — see Resume on Restart

fx (Vercel's `fx`, fx.sh):

- No hook/notify mechanism (verified against vercel-labs/fx and the fx.sh
  docs, 2026-08-21); fx has no animated Busy authority. Its output remains
  terminal/recency telemetry.
- Sessions live in provider-owned `~/.fx/sessions/<id>/` (`events.jsonl` is
  the conversation) and cannot be relocated, so there is no storage pinning;
  restart's resume uses the documented workspace-scoped continue-last
  (`fx --continue` resumes the latest session for the cwd). A launch command
  that already carries any resume form (`fx resume [last|<id>]`,
  `fx session resume …`, `--resume[-last|-<id>]`, `-c`, `-r`) is kept exact
  (`runtimes/fx/adapter/resume.rs`)
- MCP (2026-08-21): fx loads MCP servers only from the persistent global
  `~/.fx/mcp.json` (no per-launch flag, env override, or project source), so
  `install_fx_runtime_support` merges a managed `unpeel` entry pointing at
  the provider-neutral environment gate (`__mcp_gate__ unified`). The entry
  deliberately declares **no** `environment` block: fx replaces the child's
  entire environment when one is declared and inherits the parent's
  otherwise, and that inheritance is what carries `UNPEEL_SESSION_ID` plus
  the per-launch grant variables (exported by the adapter's
  `configure_host_command`, kimi-style) into each session's gate process.
  Outside a granted hosted Session — including fx runs outside Unpeel and
  hand-typed fx in a blank terminal — the gate serves a valid endpoint with
  no tools.
- Detection caveat: `fx` is also the name of a popular JSON viewer
  (antonmedv/fx). Detection is alias-based and cannot tell them apart; a
  misdetected viewer only ever gains foreground presentation — output/screen
  changes never promote either binary to Busy

Muse Code (Meta `muse` CLI):

- Hooks run only as **native-plugin capabilities** (no settings-file hook
  registry, verified against Muse Code 0.1.0), so `install_muse_hooks`
  (`runtimes/muse-code/adapter/setup.rs`) stages a plugin package at
  `~/.unpeel/hooks/muse-plugin/`
  — `.muse-plugin/plugin.json` plus one script per event, because the muse
  validator refuses two hooks sharing a source file — and registers it with
  `muse plugins install` + `muse plugins approve unpeel` (idempotent; skipped
  via a content-digest marker while muse's `plugins/installed.json` lockfile
  still lists the plugin). Supported events cover exactly Unpeel's lifecycle:
  `SessionStart`, `UserPromptSubmit`, `Stop`, `PermissionRequest` (also
  Pre/PostToolUse, Pre/PostLLMCall, PreCompact; `SessionEnd`/`Notification`
  are rejected by the validator).
- Plugins are experimental in muse, so `runtimes/muse-code/adapter/mod.rs` exports
  `MUSE_EXPERIMENTAL_PLUGINS=1` into every muse launch — without it the
  runtime never loads the plugin and no hook fires.
- Hook payloads are Claude-compatible stdin JSON (`hook_event_name`,
  `session_id`, `prompt`, `last_assistant_message`, `cwd`), so the muse hook
  scripts forward them verbatim; the native hook server normalizes
  `SessionStart` → `Start` and captures `session_id` as the provider
  conversation id. Muse runs hook subprocesses with a **scrubbed
  environment** (only `MUSE_PLUGIN_*`/`PLUGIN_*` survive), so the script
  recovers the `UNPEEL_*` identity from its parent muse process via
  `ps eww $PPID` — the parent does carry the PTY's exported env.
- **Terminal probes:** the muse TUI exits cleanly ~4s after launch if
  nothing answers its startup terminal queries (CPR `ESC[6n`, kitty
  keyboard, OSC 10/11/4 color queries). The **host answers these itself**
  whenever no answering surface is attached (`OutputQueryScanner` in
  `session_host.rs`, extended 2026-08-06 from the DA1-only fish fix): the
  probes are excised from the recorded stream and answered with the
  viewport's real cursor position, kitty flags 0, and the app palette —
  which is what makes muse launchable from the phone, via MCP, or unviewed.
  The attach client's `stream_output` carries `answers_queries: true`, and
  while such a client is connected the host passes probes through untouched
  (the real Ghostty surface answers them — Claude genuinely negotiates the
  kitty protocol, so the host must not shadow it).
- Restarts precisely with the `muse resume <id>` subcommand once SessionStart
  forwards the conversation id (the muse **TUI rejects `--session-id`** — that
  flag is `muse exec`-only, so there is no minted-launch tier);
  `muse resume --last` is the fallback before capture.
- Transcripts are event-sourced JSONL at
  `${XDG_DATA_HOME:-~/.local/share}/muse/sessions/YYYY/MM/DD/<id>/session.jsonl`
  (subagent logs nest below the session dir and are excluded); the adapter
  reads the run events — `started` (user prompt), `assistant_message_committed`,
  `reasoning_committed` (often provider-encrypted and empty),
  `assistant_tool_calls_committed`, `tool_result_batch_committed` — and the
  model from `run.model.configured`'s `model_id`.
- Sessions/Browser MCP injection is deferred: muse's
  plugin `mcpServers` entries have no env gating, so an always-on entry would
  error for muse runs outside Unpeel.

## Adding a built-in agent runtime

Built-in provider knowledge lives under one discoverable source package:
`runtimes/<slug>/`. The build validates every `runtime.toml`, generates the
compiled Rust registry, and generates client-safe presentation/setup metadata.
There is no handwritten central list to update for a new package.

This is a source contribution boundary, so adding a runtime still requires a
new Unpeel build. Downloadable third-party adapters are planned separately and
are not implied by this layout. The exact schema, directory shape, capability
rules, and verification checklist live in `runtimes/README.md`.

The short checklist:

1. Add `runtimes/<slug>/runtime.toml` with a stable reverse-DNS ID, explicit
   legacy slug, conservative command/process recognition, lifecycle policy,
   suggested presets, presentation/install metadata, and only implemented
   capabilities.
2. Put provider behavior beside it in optional `adapter/setup.rs`,
   `resume.rs`, `context.rs`, and `transcript.rs` modules. Keep generic PTY,
   hook-ingress, locking, MCP authorization, transcript security, activity,
   and protocol enforcement in core.
3. Put scripts, wrappers, and plugins in `assets/hooks/`. Installers must be
   idempotent and preserve user configuration. Every owned reporter includes
   numeric `unpeel_runtime_generation` in both the event and durable seed.
4. Treat automatic MCP registration as launch evidence per domain, not as a
   synonym for the Session's saved grants.
5. Model exact resume, continue-last, picker, or pinned storage honestly.
   Passive foreground-process observation never creates a relaunch binding.
6. Keep transcript path/root validation in shared core and return normalized
   blocks rather than a provider-specific Markdown-only result.
7. Run `bun run generate:runtimes` and `bun run check:runtimes`, then the full
   core/Host/CLI/native/iOS suites. Deep integrations require a real hosted-PTY
   proof for lifecycle, conversation capture, same-PTY Resume Agent after the
   managed runtime returns to its shell, stale
   generation rejection, transcript resolution, and the blank-terminal
   observation-only negative case.
