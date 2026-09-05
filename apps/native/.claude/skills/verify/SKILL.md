---
name: verify
description: Build, launch, and drive the native Unpeel macOS app to verify session/UI changes end-to-end without clicking through the UI by hand.
---

# Verifying native app changes live

## Build + launch the dev app

```sh
apps/native/dev-app.sh        # = `bun run dev:native` (bun may not be on PATH in agent shells)
```

Builds release, signs with the stable dev cert, quits any already-running
Unpeel Dev from that dist bundle, then `open -n`s
`apps/native/dist/Unpeel.app` ("Unpeel Dev", burnt-orange icon).

Gotchas:

- **Same bundle id everywhere**: `open` without `-n` re-focuses whichever
  `com.unpeel.native` is already running (often `/Applications/Unpeel.app`).
  The script uses `open -n` and only quits **Unpeel Dev**; NEVER quit the
  installed app without operator approval (AGENTS.md never-quit rule).
- Confirm which binary is live: `pgrep -fl "Unpeel.app/Contents/MacOS/UnpeelNative"`
  (path shows dist vs /Applications).

## Drive the Host without clicking

The app is a pure Controller of the bundled `unpeel serve` worker (the Swift
Host was retired 2026-09-03), so lifecycle verbs go to the WORKER's hook
port, not the app's loopback listener (which answers 404 for every Host
route):

- Port: the worker's entry in `~/.unpeel/app-ports` (newline list of ports;
  the app's own listener is also there — it returns 404 for `/mcp/*`). Auth:
  header `x-unpeel-auth: $(cat ~/.unpeel/mcp/auth-token)`.
- `POST /mcp/restart-session {"session_id": "..."}` — same Host path as the
  UI Restart verb.
- `POST /mcp/list-presets`, `/mcp/close-session` also exist
  (`crates/unpeel-core/src/mcp_host.rs` for shapes). Session creation is
  user-only; use the sidebar/launcher or `unpeel new`.
- Service health: `~/.unpeel/serve.json` and `unpeel serve status`. Kill the
  supervisor (`pkill -f "unpeel-host __serve__"`) to exercise the app's
  "Host service unavailable — Retry" banner; the app relaunches it.

Type into a session's PTY without the app (marks `has_been_written_to`, fires
auto-title — harmless at a shell prompt):

```sh
printf '{"type":"write","data":"\\r"}\n' | nc -U ~/.unpeel/app-sessions/<id>/session.sock -w 2
```

Session ground truth is `~/.unpeel/app-sessions/<id>/manifest.json` (state,
command, `has_been_written_to`) and `output.bin` (raw PTY bytes — grep it for
expected CLI output). A restarted session keeps its `created_at`, so find the
replacement id by matching `created_at` across manifests.

## Observing the UI itself

Published-store UI (banners, sidebar state) has no external API — use the
unpeel computer MCP tool (`see`/`click`/`screenshot`, target app "Unpeel Dev")
to select the session row and screenshot. The first computer action blocks on
a one-time approval dialog the operator must answer.
