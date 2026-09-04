# unpeel-cli

The `unpeel` binary: a terminal UI over the same hosted sessions, shared
state, and markers as the Mac app — two UIs, one state. Best experienced in
[Ghostty](https://ghostty.org) (the same terminal engine Unpeel's own
surfaces use); any modern terminal works. On a Linux server or
an app-less Mac it is also the **headless host**: it hosts sessions, pairs
phones, serves the `/mobile` protocol, and supervises the `__remote__`
server. As a controller, `unpeel --host ssh://HOST` scopes the whole UI to a
remote host over the SSH transport (pure client — creates no local state).

Notable modules: `sessions.rs`/`ui.rs` (sidebar + Ghostty-fed terminal),
`remote_scope.rs` (remote-host controller scope), `herdr.rs` (aggregate
status authority when running inside a Herdr pane; hosted children are
stripped of inherited `HERDR_*` env).

Shared-state rules live in repo-root `AGENTS.md` (Cross-Frontend Sync): every
shared-state write announces on the state bus; presets/order/markers are
file-backed and locked.

Tests: `tests/run.sh` — 24 cases driving the real binary in a real PTY
(~7 min; `./run.sh <filter>` for a subset; see `tests/README.md`). The two
`compat_*` cases are the app↔CLI version-skew upgrade guard: a failure there
means a user's install would break, not that the test needs adjusting.
