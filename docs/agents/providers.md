<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Provider Hook Details

**Launching is provider-neutral.** A preset runs its command in the user's
login shell exactly as typed; the Host exports only Unpeel's generic session
environment (`UNPEEL_SESSION_ID`, `UNPEEL_SESSION_DIR`, `UNPEEL_APP_PORT`,
`UNPEEL_HOST_BIN`, the port registry and trace paths, the Apps bin on `PATH`,
the workspace accent). No wrapper is put on `PATH`, no flag is appended, no
provider id is minted, and no provider configuration is edited by a launch or
by observing a hand-typed agent. Everything provider-specific below is the
runtime's **integration**, which the user installs once per Host (per user
account: every local workspace shares it, see `app_paths::machine_home`) —
`unpeel integrations install <runtime>` or the `integrations.install` Host
verb behind Settings ▸ Agents ▸ Install integration — into that CLI's own global
configuration. Installers are idempotent, locked, and content-guarded; the
workspace worker re-runs the installers of already-installed integrations
after an upgrade (`integrations::install::refresh_installed`) so hook scripts
and the MCP shim keep pointing at the running build, and never installs one
on its own. The one exception is the upgrade from 0.6 and earlier, which
installed hooks at launch time: on start the worker adopts a runtime whose
descriptor-declared `integration.legacy_evidence` files (its hook script)
exist under the machine home as an installed integration
(`integrations::install::adopt_legacy_installs`, marker `adopted_from`),
and the same refresh then re-runs that installer so the MCP shim is
registered too — the old per-launch injection is replaced without a click,
and only for configuration Unpeel already edited. Every integration registers the same stable shim,
`~/.unpeel/bin/unpeel-mcp`, as the MCP server: it execs
`${UNPEEL_HOST_BIN:-<install-time unpeel-host>} __mcp_gate__ unified`, and
the gate reads the calling Session's manifest grants (or serves no tools
outside a hosted Session). Without the integration a recognized agent still
gets identity and tint from detection, Resume falls back to the CLI's own
continue-last, and busy/idle stays neutral — capability honesty, never
emulation.

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
  resumed conversation); the first such event is also what makes the
  Session archivable/resumable, since nothing mints an id at launch
- Registers the MCP shim as the user-scope server `unpeel` in
  `~/.claude.json` (the file `claude mcp add --scope user` writes), pruning
  the Unpeel-owned `unpeel-mcp`/`unpeel-sessions`/`unpeel-browser` names

Codex:

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
- `~/.codex/config.toml` gets the top-level `notify = ["bash", <normalizer>]`
  reporter (set only when absent or already Unpeel-owned) as the
  turn-completion (Stop/idle) source and a compatibility bridge for Codex
  builds without the `hooks` feature, plus `[mcp_servers.unpeel]` pointing
  at the MCP shim. Codex spawns MCP servers with a minimal environment, so
  the shim's gate recovers the Session from process ancestry.
- The Codex package's notify normalizer maps raw event `type`s onto Unpeel states before calling the provider-neutral transport: `agent-turn-complete`/`task_complete`/`turn_aborted` → Stop, `task_started`/`exec_command_begin` → Start, `request_permissions`/`exec_approval_request`/`apply_patch_approval_request`/`approval-requested` → PermissionRequest.
- Codex's descriptor declares the inherited `CODEX_*` identity variables the generic Host boundary strips, so nested Codex sessions do not cross-fire hooks.

Amp:

- The shared notify reporter is global; Amp itself reads plugins per project
  (`.amp/plugins/`, with `PLUGINS=all` in the environment), so the project
  plugin is written on request: `unpeel integrations install amp --project
  DIR`. The plugin maps agent start/end to Start/Stop notify events.

Gemini:

- Installs Gemini hook config/settings integration (registered events:
  BeforeAgent, AfterAgent, AfterTool, Notification)
- Emits start/stop events through the hook server; Notification events with
  `notification_type=ToolPermission` map to PermissionRequest (attention).
  Other notification types are deliberately ignored — a broad
  Notification→attention mapping sticks sessions yellow (same rationale as the
  Grok hook matchers)

OpenCode:

- Installs the notify plugin into OpenCode's own global plugin directory
  (`${XDG_CONFIG_HOME:-~/.config}/opencode/plugin/unpeel-notify.js`); it
  no-ops outside an Unpeel session
- Plugin tracks busy/idle/permission events and calls the notify hook

Copilot:

- Installs the shared hook script globally; Copilot reads hooks per
  repository, so `.github/hooks/unpeel-notify.json` is written on request:
  `unpeel integrations install github-copilot --project DIR`

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
- `GROK_SESSION_ID` is available to every hook; the first hook captures it,
  so restart is precise (`grok --resume <id>`) from the first prompt on —
  see Resume on Restart
- Grok also natively scans `~/.cursor/hooks.json` and `~/.claude/settings.json`
  for compatibility. Those Unpeel hooks are Claude/Cursor-shaped:
  `session_start` used to normalize to busy `Start` and, with Grok's idle TUI
  re-arming the 5-minute timeout, left every Grok session spinning. The
  Claude/Cursor Unpeel hook scripts therefore no-op when `GROK_SESSION_ID`
  is set, and the hook server treats `session_start` as HookSeen. Native
  `unpeel.json` is the lifecycle source. Grok runs exactly as the user
  types it: Unpeel no longer overlays `GROK_HOME` or toggles its
  `[compat.*]` hooks, so a Claude hook that interpolates an unset `$VAR`
  shows Grok's own red `required env var(s) not set` line, as it would in
  any terminal.

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
- Current Kimi Code receives the MCP shim as a persistent `unpeel` entry in
  `~/.kimi-code/mcp.json` (Unpeel-owned legacy entries are pruned, user
  names are never replaced); legacy Kimi only took per-launch MCP flags, so
  it keeps hooks and detection but no MCP

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
- Merges the MCP shim as `unpeel` into Cline's own user MCP settings
  (`CLINE_MCP_SETTINGS_PATH`, else `<CLINE_DATA_DIR>/settings/`, else
  `~/.cline/data/settings/cline_mcp_settings.json`); other entries are kept
- Cline runs exactly as typed, including its shared detached hub. The hook
  and the shim's gate resolve the calling Session from the hosted
  environment or process ancestry, so concurrent sessions stay distinct
  without per-session hubs.
- Does not use Cline 3.0.44's advertised `--hooks-dir`: current source assigns
  `CLINE_HOOKS_DIR` but never reads it when resolving hook paths
- Cline exposes no approval-request hook, so custom `--auto-approve false`
  prompts have no distinct hook-driven attention state
- Full findings: the private "cline-cli-integration" design record

Pi:

- No hook-port integration today; Pi has no animated Busy authority. Its
  output remains terminal/recency telemetry and menu detection may still
  surface Attention.
- Nothing to install: Pi reports no lifecycle and registers no MCP, so
  `unpeel integrations install pi` is refused. `pi` runs exactly as typed;
  Resume uses Pi's own `--continue`. Older launches that recorded a managed
  `--session-dir` beneath the Unpeel home keep it across resume and cleanup.

OMP (oh-my-pi's `omp`, omp.sh; npm `@oh-my-pi/pi-coding-agent`):

- OMP is the oh-my-pi fork of Pi with its own extension event bus, so it is a
  hook-capable runtime rather than a Pi alias: the integration is an extension
  module Unpeel writes into `<agent dir>/extensions/`, which OMP's native
  discovery loads, and nothing is added to OMP's own configuration. Detection
  is the `omp` command/process alias plus the
  `@oh-my-pi/pi-coding-agent` script path signature.
- Event map (`runtimes/omp/assets/extensions/unpeel-lifecycle.ts`):
  `session_start` → HookSeen carrying the conversation id and transcript path
  from `ctx.sessionManager`; `before_agent_start` → UserPromptSubmit, so a
  steer or queued batch opens a turn like any other prompt; `agent_end` →
  Stop, suppressed while `willContinue` reports a scheduled continuation;
  `tool_approval_requested` → PermissionRequest with `tool_name`;
  `session_shutdown` → Stop.
- Escape aborts the foreground turn and OMP still emits `turn_end` and
  `agent_end` (measured on 18.2.3: Escape sent 10.0 s into a 90 s `sleep`, both
  events 0.27 s later), so an interrupted turn settles without the Host's
  Escape-cancellation fence and `escape_cancels_turn` stays off. `session_stop`
  fires only for a completed turn, while `agent_end` fires for both, which is
  why the reporter settles on `agent_end` and suppresses it only when
  `willContinue` reports a scheduled continuation.
- MCP: OMP reads its own `mcp.json` and does not import another tool's user
  configuration (foreign user sources are opt-in), so the integration merges
  the `unpeel` stdio entry there. A user's own `unpeel` server is never
  replaced; the managed entry falls back to `omp-unpeel`.
- Resume: a captured id becomes `omp --resume '<id>'`, otherwise the
  documented continue-last `omp --continue`. A fresh launch drops `-r`,
  `--resume`, `-c`, and `--continue`
  (`runtimes/omp/adapter/resume.rs`).
- Transcript: `<agent dir>/sessions/<cwd slug>/<timestamp>_<session id>.jsonl`,
  one `{type:"message", message:{role, content}}` entry per turn, with the
  session title in its own `title` entry. Task and subagent logs live in a
  nested directory of the same session id and are not conversation
  transcripts (`runtimes/omp/adapter/transcript.rs`).
- Agent directory: `PI_CODING_AGENT_DIR` is honored for the extension, the MCP
  config, and the transcript root, matching OMP's own resolution. A user who
  redirects it from a shell alias gets the integration there rather than in
  `~/.omp/agent`.
- `[updates]` probes `omp --version`, which prints `omp/<version>`; version
  extraction accepts the tail of a bare-identifier head, so the installed
  version resolves to `18.2.3` against the npm registry.
- Screen fallback: the loader row above the composer reads `esc Working…`
  while a turn runs, and the `❯` composer prompt is present in both states, so
  the working marker decides the verdict.
- Verified against a host built from this tree (isolated `UNPEEL_HOME`): install, detection, the
  MCP mount, busy → done with `completed`/`unread`, captured identity, `unpeel transcript`, and
  `unpeel resume` relaunching `omp --resume '<captured id>'`. A stale `unpeel_runtime_generation`
  is acknowledged and ignored. The provider event stream and Escape behavior were measured against
  a real 18.2.3 TUI session, as recorded above.

Antigravity (Google's `agy`, antigravity.google/product/antigravity-cli;
community request orgs/unpeel-com discussions #13):

- Detection + presets: the binary is `agy` (one Go binary the installer puts
  in `~/.local/bin`; `curl -fsSL https://antigravity.google/cli/install.sh |
  bash`). No documented lifecycle hook or notify mechanism (CLI reference,
  2026-09-17), so busy/idle has no authority and the runtime is
  `lifecycle.authority = "none"`; identity and tint come from detection.
- Resume: no hooks means no captured conversation id, so restart uses the
  documented continue-last (`agy --continue`); a captured id would become
  `--conversation <id>`. A command already carrying `-c`, `--continue`, or
  `--conversation` is kept exact (`runtimes/antigravity/adapter/resume.rs`).
- MCP: the integration merges the `unpeel` stdio entry (the shim, no `env`)
  into Antigravity's global `~/.gemini/config/mcp_config.json`
  (`mcpServers`), which its `/mcp` overlay reads; a workspace-level
  `.agents/mcp_config.json` is left to the user. No `[updates]`: `agy update`
  self-updates and there is no public latest-version endpoint.
- Not yet verified on a machine with the CLI installed: the preset flag
  `--dangerously-skip-permissions`, the exact `--version` output, and whether
  `agy` strips the environment of its MCP children (the shim's ancestry
  fallback covers that case).

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
  the integration merges a managed `unpeel` entry pointing at the MCP shim.
  The entry deliberately declares **no** `environment` block: fx replaces the
  child's entire environment when one is declared and inherits the parent's
  otherwise, and that inheritance is what carries `UNPEEL_SESSION_ID` into
  each session's gate process. Outside a granted hosted Session — including
  fx runs outside Unpeel — the gate serves a valid endpoint with no tools.
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
- Plugins are experimental in muse and load only with
  `MUSE_EXPERIMENTAL_PLUGINS=1` in muse's environment. Unpeel no longer
  exports it at launch; set it in your shell (`export
  MUSE_EXPERIMENTAL_PLUGINS=1`) or the plugin stays inert and no hook fires.
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
- The plugin manifest registers the MCP shim as its `unpeel` server. Muse
  spawns MCP servers with a stripped environment, so the shim's gate
  recovers the Session from process ancestry and serves no tools outside
  Unpeel.

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
2. Put provider behavior beside it in optional `adapter/setup.rs`
   (`pub fn install`), `resume.rs`, `transcript.rs`, and `tests.rs`; the
   build generates the package module. Keep generic PTY, hook-ingress,
   locking, MCP authorization, transcript security, activity, and protocol
   enforcement in core.
3. Put scripts and plugins in `assets/hooks/`. The installer is the whole
   integration: it must be idempotent, preserve user configuration, register
   the MCP shim (`integrations::install::write_mcp_shim`) through the
   provider's persistent mechanism, and never depend on a launch-time
   environment, wrapper, or flag. Every owned reporter includes numeric
   `unpeel_runtime_generation` in both the event and durable seed.
4. Registration evidence on a Session is "the user installed this runtime's
   integration and the launch granted the domain", never a synonym for the
   Session's saved grants.
5. Model exact resume from hook-captured ids, continue-last, or picker
   honestly; nothing mints an id or pins storage at launch. Passive
   foreground-process observation never creates a relaunch binding.
6. Keep transcript path/root validation in shared core and return normalized
   blocks rather than a provider-specific Markdown-only result.
7. Run `bun run generate:runtimes` and `bun run check:runtimes`, then the full
   core/Host/CLI/native/iOS suites. Deep integrations require a real hosted-PTY
   proof for lifecycle, conversation capture, same-PTY Resume Agent after the
   managed runtime returns to its shell, stale
   generation rejection, transcript resolution, and the blank-terminal
   observation-only negative case.
