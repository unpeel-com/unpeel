# unpeel-host

The standalone host binary built on `unpeel-core`. Clients (the Mac app, the
TUI, `unpeel-attach`) spawn it in different argv modes rather than linking a
daemon:

- `__session_host__` — host one PTY session (the process that owns the
  terminal and survives app restarts)
- `__mcp__` — the `unpeel` MCP server a session's agent talks to
  (sessions/browser/computer domains)
- `__remote__` — the TLS/WSS remote-control server for paired controllers
  (phones, other Macs) + relay uplink
- `__remote_attach__` — stdio bridge into another Unpeel's `__remote__`
  server (gated by `UNPEEL_REMOTE_ATTACH=1`)
- `__transcript__` — provider transcript reads (`snapshot`, `stream`,
  `history`, `markdown`)
- `__viewport__` — parsed-screen snapshots of a hosted session

Distributed two ways: bundled inside Unpeel.app, and as part of the CLI
install (`curl -fsSL https://unpeel.com/install.sh | sh`) alongside the
`unpeel` TUI.
