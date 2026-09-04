# unpeel-core

The Unpeel session backend as a library — everything about running, watching,
and controlling hosted agent sessions, with no GUI dependency. The Mac app
(via `unpeel-native-bridge`), the `unpeel-host` binary, and the TUI are all
frontends over this crate.

Key modules:

- `session_host.rs` — hosted PTY lifecycle: each session is a separate host
  process writing `output.bin`, serving `session.sock`, and persisting
  `manifest.json` under `~/.unpeel/app-sessions/<id>/`; sessions survive app
  restarts. Critical invariant: never signal a recorded pid without verifying
  `pid_started_at` against the live process (pid recycling).
- `integrations/` — the per-provider registry (Claude, Codex, Gemini, …):
  capabilities, launch env, hook wiring.
- `hook_assets.rs` — installs the provider hook scripts/wrappers under
  `~/.unpeel/hooks` at spawn time.
- `mcp_host.rs` — the single `unpeel` MCP server (sessions/browser/computer
  domains) with cooperative open reads and approval-gated inter-session writes.
  Hosted commands run as the same user, so this is not a sandbox boundary.
- `browser_mcp.rs` — per-session isolated real browser via the bundled
  `agent-browser` native engine (never Node).
- `transcripts.rs` — reads provider-owned conversation storage and normalizes
  it for MCP, desktop copy, and phone views.
- `terminal_viewport.rs` + `ghostty_vt.rs` — host-side screen reads backed by
  the vendored libghostty-vt (`vendor/ghostty-vt/`, same VT engine the
  renderers use; note: ghostty's `max_scrollback` is bytes, not lines).
- `state.rs` / `state_bus.rs` — shared on-disk app state (`app-state.json`)
  and the cross-frontend change-announcement bus; every shared-state write
  must announce.
- `remote_server.rs` — the HTTPS+WSS remote-control server (`__remote__`).

Test: `cargo test` from `crates/`. Docs map: repo-root `AGENTS.md` and
`docs/agents/`.
