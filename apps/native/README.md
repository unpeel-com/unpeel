# Unpeel Native

The macOS app — Unpeel's primary client and every desktop install is also a
session **Host**. Swift + SwiftUI embedding libghostty (GhosttyKit, Metal)
terminal surfaces; the session backend it drives lives in the `crates/` Rust
workspace.

Layout:

- `UnpeelNative/` — the Swift macOS app: sidebar/session UI
  (`UnpeelStore.swift`), hook HTTP server (`HookServer.swift`), session
  activity engine (`SessionActivity.swift`), licensing
  (`Licensing/LicenseManager.swift`), remote control supervision
  (`RemoteControlManager.swift`).
- `../../crates/unpeel-attach/` — Rust attach client (moved out of this dir 2026-09-03): replays a hosted session's
  retained `output.bin` tail, then pumps bytes between stdio and `session.sock`.
  The journal keeps monotonic logical cursors while reclaiming old physical
  blocks for always-on sessions.
  Runs *inside* the Ghostty surface's PTY (tmux-style client/server split).
- `dev-app.sh` / `dev-blank.sh` — dev builds (stable signing identity; dev
  builds show "Unpeel Dev" with the burnt-orange icon). `release.sh` — the
  full release pipeline (`bun run release`).
- `verify-attach.sh` (shim → `scripts/verify-attach.sh`) / `verify-browser.sh` — end-to-end smoke tests.
- The standalone `unpeel-host` binary lives in `crates/unpeel-host`
  (built on `crates/unpeel-core`, which owns the session-host module and
  its tests). It also serves the Unpeel MCP via `unpeel-host __mcp__`.

Hard rules (AGENTS.md is authoritative): never write to or quit
`/Applications/Unpeel.app`; always develop against `dist/Unpeel.app` built by
`bun run dev:native`, and check the menu bar says "Unpeel Dev".

Build: `swift build` in `UnpeelNative/` for a compile check; `bun run
dev:native` from the repo root for a runnable signed app. Ghostty surfaces
cannot initialize in headless agent runs — verify Metal rendering
interactively only.
