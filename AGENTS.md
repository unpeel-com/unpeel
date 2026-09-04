# AGENTS.md

Unpeel is an AI-native, terminal-first workspace for running CLI agents —
for any task, not just code. This repository is the **server**: the Rust
multiplexer that hosts terminal sessions, the Host protocol, the runtime
packages, the unified Unpeel MCP server, and the CLI packaging. The macOS
and iOS clients, the relay, and the website live in separate repositories
that consume this one as a pinned release.

This file is the practical map for anyone (human or agent) changing this
repository. Design records, decision logs, and plans live in the private clients
repository (`unpeel-apple`); operators who have it can point agents at its full map through
a gitignored `CLAUDE.local.md` (see the end of this file).

## Product philosophy

Unpeel is like Codex or Claude, but **for everything**. The terminal-hosted
agent is the product surface for any task a CLI agent can do. Unpeel will
**never** grow code-editor affordances: no diff viewers, no file trees, no
source editors, no language tooling, no IDE chrome. The review surface is
screenshots, demos, the terminal, and the transcript. If a feature only
makes sense for coding, it does not belong here.

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/unpeel-core` | Session backend: hosted PTYs, manifests, control sockets, app state, hook ingestion, MCP domains, transcripts, engines |
| `crates/unpeel-serve` | The Host service (`unpeel serve`): workspace workers, `/mobile`, pairing, approvals, Link, remote streamer, computer-use adapter |
| `crates/unpeel-host` | The `unpeel-host` binary: session host, PTY core, unified MCP server, remote server, one-shot helpers |
| `crates/unpeel-cli` | The `unpeel` CLI and its PTY test matrix (`crates/unpeel-cli/tests`) |
| `crates/unpeel-attach` | Terminal attach client (standalone crate, ships next to `unpeel-host`) |
| `protocol/` | Host protocol contracts: capabilities, conformance cases, engine pins, App registry, relay test vectors |
| `runtimes/` | Built-in agent runtime packages (descriptor, adapter, hook assets, fixtures); `runtimes/README.md` is the contribution contract |
| `generated/` | The client-safe runtime catalog, regenerated from `runtimes/` and shipped in every CLI archive |
| `packaging/` | Service templates for `unpeel serve install` |
| `scripts/` | Release, CI, and verification scripts (`release-cli.mjs`, `build-cli-linux.sh`, `verify-*.sh`, `scripts/ci/`) |
| `docs/agents/` | Deep detail for the areas README links: serve, PTY core, session model, providers, MCP, browser use, releases, the `unpeel` CLI (`cli.md`) |

## Hard invariants

- **Hosts and Controllers, one protocol.** A Host is the Rust workspace
  worker; Controllers (the apps, the phone, the CLI) drive it over one
  shipped protocol. A Linux Host is a new implementation of the same server,
  never a new protocol. Host capabilities are **advertised** in bootstrap
  (`protocol/host-capabilities-v1.json`), never guessed from Host kind or a
  404 probe. Native and headless Hosts run the same conformance cases
  (`protocol/host-conformance-v1.json`).
- **Terminals survive restarts through the Host, not the renderer.** Each
  session is a hosted PTY with `manifest.json`, an append-only `output.bin`
  journal, and a `session.sock` control socket under
  `~/.unpeel/app-sessions/<id>/`. Sessions run as threads of one detached
  PTY core per workspace (`docs/agents/pty-core.md`).
- **Process identity before any signal.** Every kill/reap path verifies a
  live pid against its recorded kernel start time (`pid_started_at`). Under
  agent load the pid counter wraps in under an hour; an unverified kill
  takes out an innocent session. Ambiguous ownership fails closed.
- **The state bus.** Shared state lives on disk (`app-state.json`,
  `session-order.json`, per-session markers). Every read-modify-write of a
  shared file takes an exclusive flock on `<file>.lock`; every shared-state
  write announces on the state bus (`state_bus::announce`); one-shot CLI
  verbs call `state_bus::flush()` before exit. Never add a second
  notification channel.
- **Cooperative MCP policy, not a sandbox.** The unified `unpeel` MCP server
  (`unpeel-host __mcp__`) has open reads and approval-controlled writes to
  other Sessions; browser and computer use are Off/Ask/Allow. Hosted
  commands run as the user's account, so Ask/Deny is a cooperative control
  for agents using Unpeel's tools, never a hard boundary. Agent Session
  creation and closing are user-only.
- **Engines are Host-owned and pinned.** The browser engine
  (`protocol/browser-engine-v1.json`) and the computer-use engine
  (`protocol/computer-engine-v1.json`) are installed and hash-verified by
  the Host. Unpeel never ships or requires a Node runtime.
- **Hooks are the busy/idle authority.** Provider hook assets under
  `runtimes/<slug>/assets/hooks/` report lifecycle to the Host's hook port;
  terminal output never flips busy/idle. Hook scripts broadcast to every
  port in `~/.unpeel/app-ports`.

## Working in this repository

- Never write to a real `~/.unpeel` from tests: every test case and verify
  script uses a private `UNPEEL_HOME` (short paths — `sockaddr_un` caps a
  socket path near 104 bytes).
- Provider-specific code, assets, and fixtures belong together under
  `runtimes/<slug>/`; only provider-neutral enforcement lives in core.
- Changing launching or hooks means updating `session_host.rs`, the
  runtime package, and `hook_assets/` together; the failure modes are hooks
  that never fire, busy state that never clears, and sessions that are not
  persisted or cleaned up.

## Tests to run

```sh
cargo test --manifest-path crates/Cargo.toml --workspace
cargo test --manifest-path crates/unpeel-attach/Cargo.toml
cargo clippy --manifest-path crates/Cargo.toml --workspace --all-targets --all-features -- -D warnings
crates/unpeel-cli/tests/run.sh            # the PTY matrix (real binaries, ~10 min); ./run.sh <filter> for a subset
scripts/verify-attach.sh                   # attach end to end
scripts/verify-browser.sh                  # browser engine + MCP (needs Chrome)
scripts/verify-computer.sh                 # computer use with the real engine (Linux, Xvfb + Openbox)
bun run test:release                       # release/publish scripts
bun run check:runtimes                     # generated catalog matches runtimes/
scripts/check-notices.sh && scripts/check-links.sh
```

After changing the CLI, `unpeel serve`, or anything they share with the
clients (shared markers, `app-state.json`, the `/mcp` bridge, the `/mobile`
protocol), run the full matrix. `compat_*` cases are upgrade guards: a
failure there means someone's install would break, not that the test needs
adjusting.

## Contributing

See `CONTRIBUTING.md` for the workflow, `runtimes/README.md` for adding a
built-in agent runtime, and `docs/agents/releases.md` for how releases are
cut and published.

## Private design records

Plans, decision logs, and implementation histories are not part of this
repository. If you have the private `unpeel-apple` repository checked out next to
this one, create a gitignored `CLAUDE.local.md` containing
`@../unpeel-apple/AGENTS.md` so agents load the full map alongside this file.
