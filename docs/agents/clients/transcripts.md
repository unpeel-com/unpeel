<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Remote Transcript API

Shared provider transcript logic lives in `crates/unpeel-core/src/transcripts/`.
It reads provider-owned conversation storage and normalizes it for both MCP and
remote clients. This is separate from terminal rendering and never replaces the
hosted PTY/control socket.

Current supported semantic sources:

- Claude JSONL under `~/.claude/projects`
- Codex JSONL under `~/.codex/sessions`
- Cursor Agent JSONL under `~/.cursor/projects`
- Gemini chat JSON/JSONL under `~/.gemini/tmp`
- Grok JSONL under `~/.grok/sessions`
- Current Kimi Code JSONL under `~/.kimi-code/sessions/<work-dir-key>/<session-id>/agents/main/wire.jsonl`, plus legacy Kimi `~/.kimi/sessions/<md5(canonical-cwd)>/<session-id>/context.jsonl`
- Kiro v3 `messages.jsonl` plus legacy v2 session JSONL under `~/.kiro/sessions`
- Cline JSON under `~/.cline/data/sessions/<session-id>/<session-id>.messages.json`
- Muse Code event-sourced JSONL under `${XDG_DATA_HOME:-~/.local/share}/muse/sessions/YYYY/MM/DD/<session-id>/session.jsonl` (subagent logs nested below the session dir are excluded)

OpenCode is detected but still needs an adapter for its JSON session storage
(`~/.local/share/opencode/storage/session/<projectID>/ses_<id>.json`; the old
"needs SQLite" note is outdated — current OpenCode stores JSON files, and
`opencode session list --format json` exists). Amp threads sync to ampcode.com
with no documented local store, so Amp stays terminal fallback. Pi writes
JSONL to its session dir — Unpeel now pins each pi session to
`~/.unpeel/pi-sessions/<session-id>/` (see Resume on Restart), which makes a
future pi adapter trivial (newest file in the pinned dir *is* the session).
Raw shells and unknown commands stay terminal fallback.

Four read modes exist:

- **Snapshot**: `unpeel-host __transcript__ snapshot <session-id>` resolves the
  provider transcript and returns the latest normalized entries.
- **Stream**: `unpeel-host __transcript__ stream <session-id> --offset N
  --partial ...` reads only appended JSONL bytes, carries unfinished lines, and
  returns the next offset. This follows the same offset/partial pattern as
  Touchgrass and is the lightweight path for polling.
- **History**: `unpeel-host __transcript__ history <session-id> --before-offset N`
  reads a bounded JSONL window before the current top offset so remote clients
  can lazy-load older transcript pages while scrolling up.
- **Markdown**: `unpeel-host __transcript__ markdown <session-id>` renders the
  conversation as Markdown (`format_transcript_markdown`), filtered by the
  app-wide `transcript_settings` in `app-state.json` (content-type toggles +
  range + `include_session_info`, which prepends a session-info header —
  title, Unpeel session id as a Sessions MCP target, CLI, best-effort model
  from the provider JSONL, and command; `session_info_header` /
  `detect_model_in_lines` in the `transcripts` module). This is the shared formatter
  behind the session context menu's
  **Copy transcript** action (`UnpeelStore.copyTranscriptMarkdown` on desktop;
  the iOS session edit sheet fetches the same render via
  `GET /mobile/transcript-markdown`, now a `controller_api` operation shared by
  the native and `unpeel serve` Host adapters (previously also the TUI's own
  session context menu, which called the shared
  `read_session_transcript_markdown` wrapper and published it to the
  controller terminal via OSC 52, before the TUI's 2026-09-03 removal) and the
  Sessions MCP `read_transcript` tool,
  which uses `transcript_settings` as its defaults (an agent's explicit
  `entries`/`include_tools` args still override). Configure the toggles in
  **Settings ▸ Transcripts** (`TranscriptsSettingsPanel`,
  `UnpeelStore.updateTranscriptSettings`) — a general tab, deliberately not
  part of the Sessions MCP settings. `--include-tools`/`--entries` flags
  override the settings for ad-hoc CLI use.

Transcript entries preserve a compact `role`/`text` fallback plus structured
blocks for supported provider data: text, reasoning, tool calls/results,
file changes, diffs, plan updates, usage, permissions, attachments, and info.
The iOS bridge maps those blocks into `RemoteTranscriptEntry` /
`RemoteTranscriptBlock` and requests `include_tools=true` for supported
file-backed providers.

The iOS dev bridge exposes this as
`GET /transcript?session_id=...&mode=snapshot|stream|history`, and
`RemoteMacClient` decodes it into `RemoteTranscriptSnapshot`,
`RemoteTranscriptStreamChunk`, or `RemoteTranscriptHistoryPage` from
`UnpeelShared`.
