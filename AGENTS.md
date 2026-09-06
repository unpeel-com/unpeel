# AGENTS.md

Unpeel is an AI-native, terminal-first workspace for running CLI agents —
for any task, not just code. This repository is the whole product apart from
the website and the operated Link service: the **server** (the Rust
multiplexer that hosts terminal sessions, the Host protocol, the runtime
packages, the unified Unpeel MCP server, and the CLI packaging) and the
**Apple clients** (the Mac app, the iPhone/iPad app, the Swift package they
share, and the C-ABI bridge the Mac app links). App and server build from one
tree at one version, so they can never skew.

This file is the practical map for anyone (human or agent) changing this
repository. Design records, decision logs, and plans live in a private
archive repository; this file documents what ships and what must stay
aligned.

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
| `crates/unpeel-native-bridge` | Panic-contained C ABI over `unpeel-core` that the Mac app links (workspace member, path deps) |
| `clients/native` | The macOS app (Swift + SwiftUI + libghostty) and its build/release scripts |
| `clients/ios` | The iPhone/iPad Controller (xcodegen project `UnpeelIOS/`) |
| `clients/shared/UnpeelShared` | Swift package shared by both apps: pairing, Host protocol client, Relay E2E, icon art, the runtime catalog copy |
| `protocol/` | Host protocol contracts: capabilities, conformance cases, engine pins, App registry, relay test vectors |
| `runtimes/` | Built-in agent runtime packages (descriptor, adapter, hook assets, fixtures); `runtimes/README.md` is the contribution contract |
| `generated/` | The client-safe runtime catalog, regenerated from `runtimes/` and shipped in every CLI archive |
| `packaging/` | Service templates for `unpeel serve install` |
| `scripts/` | Release, CI, and verification scripts (`release-cli.mjs`, the app publisher, `build-cli-linux.sh`, `verify-*.sh`, `scripts/ci/`) |
| `docs/agents/` | Deep detail for the areas README links: serve, PTY core, session model, providers, MCP, browser use, releases, the `unpeel` CLI; `docs/agents/clients/` covers the apps |

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
- One version for everything: `[workspace.package] version` in
  `crates/Cargo.toml` names the CLI archives, the bridge, and the Mac app.
  Bump it there (then `cargo update --workspace`); nothing else restates it.

## Tests to run

```sh
cargo test --manifest-path crates/Cargo.toml --workspace          # includes unpeel-native-bridge
cargo test --manifest-path crates/unpeel-attach/Cargo.toml
cargo clippy --manifest-path crates/Cargo.toml --workspace --all-targets --all-features -- -D warnings
scripts/ci/check-portable-core.sh          # unpeel-core without the Host (controller-core + wasm32 clippy)
crates/unpeel-cli/tests/run.sh            # the PTY matrix (real binaries, ~10 min); ./run.sh <filter> for a subset
scripts/verify-attach.sh                   # attach end to end
scripts/verify-browser.sh                  # browser engine + MCP (needs Chrome)
scripts/verify-computer.sh                 # computer use with the real engine (Linux, Xvfb + Openbox)
bun run test:release                       # release/publish scripts
bun run check:runtimes                     # both generated catalog copies match runtimes/
scripts/check-notices.sh && scripts/check-links.sh
```

After changing the CLI, `unpeel serve`, or anything they share with the
clients (shared markers, `app-state.json`, the `/mcp` bridge, the `/mobile`
protocol), run the full matrix. `compat_*` cases are upgrade guards: a
failure there means someone's install would break, not that the test needs
adjusting. The client suites are listed in the next section.

## Apple clients

The Mac app is a **Controller** of the bundled `unpeel serve` Host service
plus a platform-capability adapter (notifications, Keychain, APNs, approval
dialogs, Computer Use on the Mac's own desktop); the iOS app is a remote
Controller only. Neither hosts sessions: "the server" is exactly `crates/`.
Detail lives in `docs/agents/clients/` (`dev-builds.md`, `terminal.md`,
`panes.md`, `workspaces.md`, `presets.md`, `session-activity.md`,
`transcripts.md`, `remote-control.md`, `controller-assisted-pairing.md`,
`worktrees.md`); the root rules below always apply.

### Layout

- `clients/native/UnpeelNative/` — the Swift macOS app (`UnpeelStore.swift` is
  the Controller projection; `HookServer.swift` serves only the platform-
  adapter callback — no Host routes; `LaunchConfig.swift` resolves the bundled
  `unpeel-host`/`unpeel`/`unpeel-attach`). `Sources/CUnpeelNativeBridge/`
  is the C shim; its header must stay identical to
  `crates/unpeel-native-bridge/include/unpeel_native_bridge.h`
  (`build-rust-bridge.sh` checks). `clients/native/vendor/libghostty-spm/` is
  the vendored, patched libghostty package; every bump is a deliberate event.
- `clients/ios/UnpeelIOS/` — xcodegen project (`project.yml` → `xcodegen` after
  adding or renaming Swift files); `Tools/dev_bridge.py` drives the
  simulator against a local Host.
- `clients/shared/UnpeelShared/` — `RemoteControlProtocol.swift`,
  `RemotePairingClient.swift`, `RelayProtocol.swift` /
  `RemoteRelayConnection.swift` (forward-secret E2E over the Link relay,
  pinned to `protocol/relay-kat-vectors-v1.json`), `PairedHostRecord.swift`,
  and `GeneratedRuntimeCatalog.swift` (identical to `generated/`; both are
  written by `bun run generate:runtimes`).
- `crates/unpeel-native-bridge` — keep it a thin translation layer: logic
  belongs in `unpeel-core`; every entry point catches panics at the FFI edge.

### Build, run, test

- Bridge: `clients/native/build-rust-bridge.sh debug|release` (into
  `crates/target/native-bridge/<profile>/`, linked by `Package.swift`).
- Mac app compile check: the bridge, then `swift build` in
  `clients/native/UnpeelNative`. Full bundle without signing secrets:
  `CODESIGN_IDENTITY=- clients/native/build-app.sh` (ad-hoc; builds the server
  binaries and the bridge **from this tree** by default;
  `UNPEEL_SERVER_ARCHIVE=<tar.gz>` bundles a published CLI archive of the
  same version instead, for reproducibility checks). Never launch it.
- Dev app: `bun run dev:native` (`clients/native/dev-app.sh`) builds and signs
  `clients/native/dist/Unpeel.app` with a **stable** local identity (never
  ad-hoc — an ad-hoc rebuild re-triggers the license Keychain prompt), quits
  only an already-running **Unpeel Dev**, and launches the new one. Dev
  builds say "Unpeel Dev" in the menu bar with a burnt-orange icon; a
  release-flavored build in the same `dist/` path says plain "Unpeel" with
  the Sparkle feed baked in — rebuild before testing. `bun run
  dev:native:blank` runs against a throwaway `UNPEEL_HOME` (isolates
  UserDefaults too). Ghostty surfaces cannot initialize headless: verify
  Metal rendering interactively only.
- Tests: `clients/native/test-native.sh` (debug bridge + `swift test`; the
  conformance tests read this checkout's `protocol/`, `UNPEEL_PROTOCOL_DIR`
  overrides); `swift test --package-path clients/shared/UnpeelShared`;
  `clients/ios/test-ios.sh` (xcodebuild against a simulator —
  `UNPEEL_IOS_TEST_DESTINATION` picks it; plain `swift test` is broken for
  the iOS package on macOS, never use it as the iOS gate);
  `python3 -m py_compile clients/ios/UnpeelIOS/Tools/dev_bridge.py`;
  `bun run check:runtimes`. CI: `.github/workflows/clients.yml` (unsigned
  Mac build, iOS simulator tests, shared package tests; no signing secrets).
- iOS device builds need the signing team from `project.yml`
  (`UNPEEL_DEVELOPMENT_TEAM`, overridable per build) and a registered device;
  simulator builds stay unsigned. Recipes: `docs/agents/clients/dev-builds.md`.

### Client rules

- **Never write to, launch, or quit `/Applications/Unpeel.app`.** It is the
  released, notarized, Sparkle-updating install and the operator's daily
  driver. Develop against `clients/native/dist/Unpeel.app` and check the menu
  bar says "Unpeel Dev". If a workflow truly needs the dev build to be the
  only instance, ask the operator first and restore the prior state after.
- **The app is a pure Controller.** It never hosts sessions, installs hook
  assets, or falls back to hosting: a service that does not answer within
  its deadline shows "Host service unavailable — Retry" and keeps retrying
  the bounded relaunch. Remote-Host scope is a pure client too — while a
  remote Host is selected, never spawn local sessions or install hook
  assets; keep the backend check at the few spawn/install choke-points.
- **Remote scope is pixel-identical to local.** Selecting another Host in
  the sidebar picker scopes the same sidebar, terminal, and verbs to it; the
  only visible difference is the green bottom Host button with the Host's
  name. No forked remote view hierarchies.
- **Perceived speed over latency numbers.** No flash, no blank frames, no
  layout jump: keep the already-scanned disk view visible while the Host
  connects, pre-warm surfaces, and never let a reconnect visibly end a
  session. A change that improves a benchmark but adds a flash is a
  regression.
- **Hooks are the busy/idle authority on the client too.** The app renders
  Host-published activity; the Swift rescan/activity engine is only the
  no-flash startup seed. Terminal output never flips busy/idle. Agent-drawn
  select menus fire no hooks — they are detected from the parsed viewport
  (shared detector, Host-side) and surfaced as attention.
- **iOS terminal rules.** The phone detail screen stays a live terminal,
  never a semantic chat UI; keyboard focus must never resize the remote grid
  (only the keyboard-avoidance lift); present modals at the root, not over
  the Metal surface. Mock data is preview-only, never runtime.
- **Shared state contract.** The app reads `app-state.json` and layers its
  own edits as UserDefaults overlays, except presets, which it reads and
  writes in the file (the single preset truth for the app and `unpeel
  presets`; never add a new preset UserDefaults overlay). Every shared-file
  edit takes the same `<file>.lock` flock as Rust
  (`PresetStateFile.withExclusiveLock`) and announces on the state bus
  (`UnpeelStore.announceStateChange`). Restart recommendations go through
  `UnpeelStore.restartRecommendations`, never a second banner path.
- **Protocol changes are additive and land Host-first.** A new field,
  route, or capability is a change to `crates/` + `protocol/` (advertised in
  bootstrap) before the Swift side consumes it; the conformance fixtures are
  the contract. A Controller must not care which kind of Host it talks to.
- **Handoff is restart-with-resume**, never live PTY migration; screenshots
  are the review surface (video is deferred, never by re-adding Node).
- **Keep `UnpeelShared` free of closed-service dependencies.** Crypto and
  wire behavior are what users audit; the relay known-answer vectors pin the
  Swift, Rust, and relay implementations to identical bytes.

### Release order

One version, three cuts, in this order: **CLI** (`bun run release:cli --
--channel <ch>`: three archives from one commit, published to R2) → **Mac
app** (`bun run release:mac -- --channel <ch> --build <n>`: builds the server
binaries and the bridge from this tree at the same commit, Developer ID
signs, notarizes, staples, packages the DMG and Sparkle ZIP, writes the
appcast, publishes; `--dry-run` rehearses without secrets) → **website**
(the `## <version>` changelog entry that `release.sh` requires goes live from
the separate `unpeel-website` repo; `release-changelog.mjs` resolves
`UNPEEL_CHANGELOG`, then the `../unpeel-website` sibling). `CFBundleVersion`
is one monotonic space across channels. Agents cannot cut a real release (it
needs the operator's Developer ID, notary, Sparkle, and R2 credentials);
validate pipeline changes with `--dry-run`. Detail:
`docs/agents/releases.md`.

## Contributing

See `CONTRIBUTING.md` for the workflow, `runtimes/README.md` for adding a
built-in agent runtime, and `docs/agents/releases.md` for how releases are
cut and published.

## Private design records

Design records, decision logs, and plans are not part of this repository;
they live in a private archive repository. This file and `docs/agents/`
document what ships.
