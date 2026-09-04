# Unpeel

Unpeel is an **agent-first terminal multiplexer**, written in Rust. Sessions
keep running on your own machines, know when the agent inside them needs you,
and give that agent a browser, a computer, and its sibling sessions to work
with. The Mac app, the iPhone/iPad app, and (later) a web app connect to it;
this repository is the server.

**Why this multiplexer**

- 🔁 **Sessions outlive everything.** Close the window, quit the app, drop the
  SSH connection, reboot the client, upgrade Unpeel: the agent keeps working.
  One shared PTY core per workspace runs every session as an event-driven
  task, and a new build takes over running terminals in place, no restart.
- 🚦 **It knows what the agent is doing.** Provider hooks, runtime detection,
  and a terminal-viewport scanner turn "some process is printing" into busy,
  idle, and *needs you*, per session, with notifications to whichever client
  you are holding. Resume re-runs the agent with its own conversation id
  after a crash or a Host upgrade.
- 🧰 **Built-in MCP tools.** Every session has the single `unpeel` MCP server:
  an isolated real browser with screenshots as reviewable artifacts, a real
  desktop through Computer Use on Linux Hosts, presets and worktrees, and the
  session gallery. The engines are Host-installed and pinned; nothing to set
  up per agent.
- 💬 **Agents can talk to each other.** From inside a session an agent can
  list its siblings, read their screens and transcripts, wait for one to go
  idle, and send text to another. Reads are open; the first write into
  another session asks you, and approved pairs are remembered. Sessions are
  created and closed by people, never by agents.
- 🧑‍💻 **Any agent, any task.** Claude Code, Codex, Gemini, Cursor Agent, Grok,
  Kimi, Kiro, Cline, Amp, OpenCode, Muse Code, Pi, or anything that runs in a
  terminal, for coding, research, writing, ops, or design. It is a terminal,
  not a code editor: you follow an agent through its terminal, its transcript,
  and the screenshots it takes, the same way whether it is fixing a bug or
  booking a trip.
- 📱 **Steer it from anywhere, on hardware you own.** Pair a Mac or a phone
  with a one-time code. On your own network or VPN the connection is direct;
  away from home it goes through Unpeel Link, an end-to-end encrypted relay
  that only ever sees ciphertext. Your sessions, transcripts, and screenshots
  never live on a server you don't control.
- 🗂️ **Plain files, one protocol.** Every session is a directory under
  `~/.unpeel/`: a manifest, a bounded output journal, a control socket. You
  can read, back up, or script it with ordinary tools. Every client speaks the
  same versioned protocol to every server, so a Mac hosting sessions and a
  Linux box hosting sessions look identical from the phone.
- 🪶 **Lean.** A few MiB for a Host with dozens of sessions; per empty session
  about 0.1 MiB, per 10k-line session about 0.4 MiB, measured and tracked in
  CI (`scripts/bench-memory.sh`).

**Clients**

- Mac app: [unpeel.com/download/mac](https://unpeel.com/download/mac)
- iPhone / iPad: [unpeel.com/ios](https://unpeel.com/ios)
- Docs, including headless hosting: [unpeel.com/docs](https://unpeel.com/docs)

This repository holds the Host service, the shared PTY core, the unified
`unpeel` MCP server, the built-in agent runtimes, the CLI, and the Host
protocol. The clients consume its tagged releases and CLI archives.

## Install

Mac or Linux:

```bash
curl -fsSL https://unpeel.com/install.sh | sh
```

That installs `unpeel`, `unpeel-host`, and `unpeel-attach` (Apple silicon and
Intel Macs, Linux x86_64 and aarch64; Ubuntu 20.04 / Debian 11 or newer). The
installer verifies the archive against its SHA-256 sidecar and refuses to
install otherwise.

## Run a Host

```bash
unpeel serve            # the Host service: every registered workspace, one worker each
unpeel serve install    # per-user boot service (launchd on macOS, systemd --user on Linux)
unpeel pair             # show a one-time code / QR for a Controller (Mac app, iPhone)
unpeel new --command "claude" --cwd ~/project
unpeel ls               # sessions, status, project, command
```

`unpeel serve` owns the machine lease, supervises one worker per workspace,
ingests provider hooks, answers Controllers over the local socket, LAN
(Direct), and Unpeel Link, and keeps every session alive across upgrades.
`unpeel --workspace NAME serve` runs a single isolated workspace (the container
spelling). The full CLI: `unpeel help`.

The one-shot verbs, `unpeel settings`, `unpeel presets`, and the test gates
they share with the clients are documented in
[`docs/agents/cli.md`](docs/agents/cli.md).

## How sessions survive

- **Shared PTY core.** The worker starts one detached `unpeel-host
  __pty_core__` per workspace and every session runs inside it as an
  event-driven task, not as a process of its own. Per empty session it costs
  0.12 MiB, per filled 10k-line session 0.39 MiB, and each attached client
  about 1 KiB on the server side (measured on macOS 26; the recipe is
  `scripts/bench-memory.sh`; the targets are tracked in the private design
  records).
- **Journal + sockets.** Each session lives under
  `~/.unpeel/app-sessions/<id>/`: `manifest.json` (identity, state, pid with
  start-time identity so a recycled pid is never signalled), `output.bin` (a
  logically append-only journal with monotonic lifetime offsets and a bounded
  retained tail), and `session.sock` (write / resize / ping / kill, and an
  exact VT snapshot for attaching clients from the Host's resident
  libghostty-vt grid).
- **In-place core upgrade.** A newer core takes over a running one over
  `SCM_RIGHTS` (`__pty_core__ --takeover`, triggered by the service on build
  skew), so upgrading Unpeel never restarts a terminal. Sessions never depend
  on the worker: the service can stop and restart while every PTY keeps
  running.
- **Clients are attachments.** `unpeel-attach <id>` replays the journal tail
  (or the snapshot) and then bridges stdio to `session.sock`; the Mac app
  runs it inside its Ghostty surfaces, and remote Controllers stream the same
  journal over the Host protocol. Any client can restart without touching the
  agent.

Detail: [`docs/agents/pty-core.md`](docs/agents/pty-core.md),
[`docs/agents/serve.md`](docs/agents/serve.md),
[`docs/agents/session-model.md`](docs/agents/session-model.md).

## Host protocol

One protocol for every Controller and every Host. A Controller never cares
whether it is talking to a Mac app Host or a headless Linux box; SSH, LAN
(Direct), and Unpeel Link are transports for the same contract, never second
sets of verbs. Capabilities are advertised, not guessed: bootstrap carries a
major-versioned, additive `hostProtocol` descriptor whose stable operation
ids come from [`protocol/host-capabilities-v1.json`](protocol/host-capabilities-v1.json),
and every Host implementation runs the same
[`protocol/host-conformance-v1.json`](protocol/host-conformance-v1.json)
cases. The `protocol/` directory (capability ledger, conformance fixtures,
pane-layout operations, direct-path rules, relay KAT vectors, the App
registry) ships verbatim inside every CLI archive so a pinned client can read
the contracts it was built against. A protocol change is a public pull
request here plus a version bump on the client side.

## Runtimes

Provider knowledge lives in one package per agent under
[`runtimes/<slug>/`](runtimes/): a `runtime.toml` descriptor, a Rust adapter
(launch and setup, resume identity, transcript discovery), hook assets, and
fixtures. The build discovers the packages and generates the registry, so
adding an agent never touches a central list. Contribution contract:
[`runtimes/README.md`](runtimes/README.md); per-provider notes:
[`docs/agents/providers.md`](docs/agents/providers.md).

Busy / idle / needs-attention state comes from real provider hook
integrations, never from guessing at output; select menus drawn by agents are
detected from the parsed viewport.

## The `unpeel` MCP server

`unpeel-host __mcp__` is one MCP server every capable session gets: `sessions`
(inspect, read the screen, wait for text, send input to sibling sessions under
an approval-controlled write policy), `agents` (occurrence-bound runtime
occupants and their transcripts), `workspace` (presets, git worktrees),
`artifacts` (screenshots as reviewable gallery items), `browser` (a real,
isolated browser per session driven over CDP), `apps`, and `skills`. Reads are
open; every write to another session follows the user's policy and asks by
default; session creation stays user-only. Detail:
[`docs/agents/sessions-mcp.md`](docs/agents/sessions-mcp.md),
[`docs/agents/browser-mcp.md`](docs/agents/browser-mcp.md).

## Clients

Listed at the top of this file. Every client speaks the Host protocol in
[`protocol/`](protocol/); a headless Linux Host is driven from the Mac app or
the phone exactly like a Mac Host: [unpeel.com/docs/headless-host](https://unpeel.com/docs/headless-host).

## Open source boundary

The server — this repository — is public under the MIT license
(`LICENSE`), with the Unpeel name, logo, icon, and mascot covered by
[`TRADEMARK.md`](TRADEMARK.md). The Apple clients are closed for now and
consume this repository's tagged releases and CLI archives. Long-term the only
closed component is the backend of the operated Unpeel Link service
(accounts, seats, entitlements, rendezvous, relay, push): everything local and
direct is free and has no Link dependency. Design records and plans live in
a private docs repository; this repository documents what ships.

## Development

Rust 1.88 or newer. No Node runtime is required to run Unpeel; Bun is only
used by the release scripts.

```bash
cargo build --manifest-path crates/Cargo.toml -p unpeel-cli -p unpeel-host
cargo build --manifest-path crates/unpeel-attach/Cargo.toml   # standalone crate
UNPEEL_HOME=/tmp/unpeel-dev crates/target/debug/unpeel serve  # isolated state
cargo test --manifest-path crates/Cargo.toml --workspace
crates/unpeel-cli/tests/run.sh          # the real-PTY case matrix (~8 min)
scripts/verify-attach.sh                # attach replay / echo / snapshot smoke
```

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) first; [`AGENTS.md`](AGENTS.md) is
the map of how the session system fits together and what must stay aligned.

## Releases

Server releases are CLI archives per channel on Cloudflare R2 behind
unpeel.com (`bun run release:cli`); the installer above reads the same
bucket. Every archive carries `BUILD_PROVENANCE.json`,
`THIRD_PARTY_NOTICES.txt`, and `protocol/`. Details:
[`docs/agents/releases.md`](docs/agents/releases.md). Third-party licenses
for what this repository vendors or fetches at runtime: [`NOTICE.md`](NOTICE.md).
