# Contributing to Unpeel

Thanks for wanting to help. A few things about this codebase are unusual;
reading this first will save you a closed pull request.

## Ground rules

- **Unpeel is never an IDE.** No diff viewers, file trees, editor panes,
  language tooling, or any code-editor chrome, in any client or in the
  server's protocol. Agents here are for every kind of work, and the review
  surface is the terminal, transcripts, and screenshots. Pull requests adding
  code-centric surfaces are declined regardless of quality. (Full
  philosophy: `AGENTS.md`.)
- **Nothing leaves the user's machines.** No cloud dependencies, telemetry,
  or server-side state for session content. The only operated service is
  Unpeel Link (rendezvous, relay, push); its relay is an opaque end-to-end
  encrypted transport, and everything local and direct works without it. Do
  not add client-side entitlement gates.
- **Compatibility is load-bearing.** Unpeel has paying users. The on-disk
  contracts under `~/.unpeel/`, the Host protocol, licensing behavior, and
  the resume machinery have documented invariants; `AGENTS.md` is the map and
  the `compat_*` PTY cases are the guard. A failure there means a real
  install would break, not that the test needs adjusting.
- **One Host protocol.** Every Controller and every Host speak the same
  contract; transports (local socket, Direct, SSH, Link) never grow their own
  verbs. Capabilities are advertised in the bootstrap descriptor
  (`protocol/host-capabilities-v1.json`) and every Host runs
  `protocol/host-conformance-v1.json`. **A protocol change lands Host-first
  and additively**: change `crates/` and `protocol/` (advertised in
  bootstrap), then the Swift side under `apps/` in the same tree, and
  document it in `docs/agents/serve.md`. Out-of-tree clients read
  `protocol/` from the CLI archive they pin.

## Where contributions land best

**Agent runtimes.** Each agent is one package under `runtimes/<slug>/`
(descriptor, adapter, hook assets, fixtures); the build discovers it and the
compiler walks you through the adapter. Follow the contract in
`runtimes/README.md` and the per-provider notes in `docs/agents/providers.md`.
A runtime ships in a new Unpeel build; downloadable third-party adapters are
not supported yet.

Also welcome: Linux Host hardening, PTY core and journal work, provider hook
reliability, the unified MCP, docs. For anything architectural, open an issue
first: much of the direction is already decided in private design records
(plans and decision logs live in a private docs repository) and it is cheaper
to align before writing code.

## Prerequisites

- Rust 1.88 (the declared MSRV; CI checks the workspace at exactly that
  toolchain, plus `rustfmt` and strict `clippy`).
- No Node runtime is required to run or test Unpeel. Bun is used only by the
  release scripts (`scripts/release-*.mjs`).
- Linux archives target GLIBC 2.31 (Ubuntu 20.04 / Debian 11); the release
  build checks that floor.

## Build and test

```bash
cargo build --manifest-path crates/Cargo.toml -p unpeel-cli -p unpeel-host
cargo build --manifest-path crates/unpeel-attach/Cargo.toml   # standalone crate, never a workspace member
UNPEEL_HOME=/tmp/unpeel-dev crates/target/debug/unpeel serve  # isolated state, never your real ~/.unpeel

cargo test --manifest-path crates/Cargo.toml --workspace
cargo test --manifest-path crates/unpeel-attach/Cargo.toml
crates/unpeel-cli/tests/run.sh          # real-PTY case matrix (~8 min); ./run.sh <filter> for a subset
scripts/bench-memory.sh                 # memory ceilings after anything touching session hosting
scripts/verify-attach.sh                # attach replay / live echo / reattach / snapshot
scripts/verify-browser.sh               # Browser MCP end to end (needs an engine + Chrome; skips otherwise)
```

PTY matrix rules:

- Never run two `run.sh` invocations at once; the process suites share ports.
- Use a short private base when several people or lanes share a machine:
  `UNPEEL_TUI_TEST_BASE=/tmp/u1 crates/unpeel-cli/tests/run.sh`. Unix socket
  paths are capped near 104 bytes and the case name is part of the path, so a
  long base makes sessions fail to bind with no visible error.
- The harness and every case are documented in `crates/unpeel-cli/tests/README.md`.
- A Rust test fixture that starts a worker (`unpeel-host __serve__` or the
  local Host service under a temp home) must tear down the detached PTY cores
  it created in `Drop` — call `unpeel_core::pty_core::shutdown_cores_under`
  on the fixture root before removing it, so a passing or panicking test
  never leaks `__pty_core__` processes.

Which gates to run for which change (session launch and hooks, the CLI, the
protocol, transcripts, the browser engine) is listed under "Tests To Run" in
`AGENTS.md`.

## Pull requests

- Match the surrounding code: naming, idiom, and comment density. Comments
  state constraints the code cannot show, not narration. Commit messages
  explain *why*.
- Keep `AGENTS.md` and the `docs/agents/*.md` file that owns the subsystem in
  step with a behavior change in the same pull request; say in the pull
  request what changed for the maintainers' private design records.
- Run `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings`
  before pushing; CI runs both.
- Every new vendored artifact arrives with its LICENSE next to it, and
  `THIRD_PARTY_NOTICES.txt` is regenerated when the dependency graph of the
  three CLI crates changes (CI diffs it; see `NOTICE.md`).
- Secret scanning (gitleaks) runs on every push and pull request over the full
  history; never allowlist a real credential in `.gitleaks.toml`.
- Contributions are accepted under the repository's MIT license (inbound =
  outbound). There is no CLA or DCO requirement.

## What is not in this repository

The Mac and iPhone apps live in a private repository for now and consume this
repository's tagged releases. Their release process, build ledger, and the
account service behind Unpeel Link are private as well, so a few pointers in
`AGENTS.md` and `docs/` refer to a "private operational repo" or a
"private account-service repo" you will not have; nothing in the server
build depends on them.
