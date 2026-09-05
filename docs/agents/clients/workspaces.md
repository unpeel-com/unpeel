<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

### Workspaces (isolated homes and unified Controller scopes, experimental)

Workspaces productize the dev-blank mechanism: a **workspace = an isolated
`UNPEEL_HOME` and Host identity** with fully separate Sessions, projects,
presets, settings (per-home defaults suite), and **its own phone-pairing
identity** (`<home>/mobile/mac-id`). It may be served by its own app instance
or selected in another desktop Controller's existing window through the
loopback Host gateway; in both cases it is the same Host contract and state.
Each workspace therefore appears as its own "Mac" in the iOS app's multi-Mac
picker. Gated behind Settings ▸ Experimental
(`ExperimentalFeature.workspaces`, env `UNPEEL_DEV_WORKSPACES=1`; legacy
`UNPEEL_DEV_PROFILES=1` is also accepted); managed in Settings ▸ Workspaces
(`WorkspacesSettingsPanel.swift`). The feature's shipped UserDefaults key
remains `unpeel.experimental.profiles`.

**Terminal bytes never ride the gateway for a local workspace (2026-09-02).**
Selecting another local workspace scopes sidebar, verbs, and lifecycle to its
worker over the loopback gateway, but its panes render exactly like Local
scope: a real `unpeel-attach` surface on that workspace's own
`<home>/app-sessions/<id>/session.sock` (`LaunchConfig.attachCommand(sessionID:
sessionsDir:)`, launched with the workspace's `UNPEEL_HOME`), with on-disk
tail replay and PTY-driven resize. `SelectedHostScope.isLocalMachine` is the
transport key — only a true paired/SSH Host uses the paged remote transport —
and `RemoteHostTransport.localGateway` owns the direct data plane, so the
runtime never writes, fits, or pumps output for it. `SurfaceCache` records the
`app-sessions` directory per pane and prunes only the displayed scope's dead
sessions, so hopping between workspaces re-shows the same retained surface.

Creating a local workspace registers it and immediately selects it in the
current window through the loopback Host gateway. It does **not** implicitly
launch another app instance. The workspace dots and Settings scope picker are
available in release builds whenever Workspaces is enabled, remain visible
while Settings is open, and reconstruct their rows from the shared registry
after relaunch. Paired/SSH workspace rows remain behind the separate native
remote-Controller development gate. Opening a local workspace in its own
window is an explicit picker action and continues to use
`UnpeelWorkspaceLauncher`.

Core pieces (`UnpeelWorkspaceRegistry.swift`):

- **Legacy storage contract**: the registry remains at the **real**
  `~/.unpeel/profiles.json`, its array key remains `profiles`, and workspace
  homes remain under `~/.unpeel/profiles/<slug>`. These shipped names are
  compatibility identifiers, not current product terminology. Never resolve
  the registry through `LaunchConfig.unpeelDir`: every instance must see one
  registry. Homes are minted **permanently** because provider hook configs
  (`~/.claude/settings.json`,
  `~/.codex/hooks.json`, …) bake absolute script paths into whichever home
  installed hooks last; scripts are byte-identical across homes, so shared
  configs keep working as long as no home dir vanishes.
- **Launch** (`UnpeelWorkspaceLauncher.launch`): direct `Process` exec of
  `Bundle.main.executableURL` with `UNPEEL_HOME` in the env — **never
  `open`/NSWorkspace** (env not forwarded; same bundle id re-focuses).
- **Liveness**: each instance writes `<home>/app.pid`
  (`{pid, pidStartedAt}`); readers verify the recorded start time against the
  kernel (10s tolerance) before trusting it — the same pid-reuse discipline as
  session manifests. `AppDelegate` refuses to start when another live process
  owns the same home.
- **Single-updater rule**: `sparkleCanStart` requires the default instance
  (`UNPEEL_HOME` unset). Additional workspaces pick up an installed update on
  their next relaunch.
- **Scoped reap**: `RemoteControlManager.reapOrphanedServers` reads this
  home's `remote.json` (`pid` + `pid_started_at`, written by
  `unpeel-host __remote__`) and SIGTERMs only the identity-verified pid —
  never `pkill -f`, which would kill other workspaces' servers and set the two
  managers respawn-fighting forever.
- **Advertised name**: `UnpeelWorkspaceContext.advertisedHostName` (workspace
  name; host name for the default instance) is the single choke point feeding
  `MobilePairingStore.macName`, both bootstrap snapshot builders, and the
  Bonjour service name. Renames apply fully after the workspace restarts.
- **Phone E2E keys**: `MobileE2EKeychainStore` accounts are
  `"<macID>.<deviceID>"` (the phone reuses one deviceID across all Macs it
  pairs with — a bare-deviceID account would let workspace B's pairing
  overwrite workspace A's relay key). Legacy bare-deviceID items are read as
  fallback and copied forward.
- **Relay entitlements**: `relay_bindings` (the website D1, migration
  `0012_relay_bindings_per_mac.sql`) allows up to **6** relay Mac ids per
  activated seat — one per workspace; `relay_mac_id` stays UNIQUE across seats.
  Over-cap returns 429, a mac id owned by another seat 409. Licensing is
  untouched: the seat device id is hardware-derived, so workspaces share one
  seat.
- Hook isolation: sessions get `UNPEEL_APP_PORT_REGISTRY_FILE` and
  `UNPEEL_HOOK_TRACE_FILE` injected (`integrations/mod.rs`) so a workspace's
  hook broadcasts/traces stay in its own home instead of the real `~/.unpeel`.

### Controller-owned pane layouts and windows

Desktop multi-pane views have parity in every selected Host scope: main local,
another local workspace over loopback, paired, and SSH. They are not workspace
or Host state. `PaneLayoutState` is the transport-neutral value model and each
AppKit window owns a `PaneLayoutController` for its current scope. The
Controller atomically stores `DurablePaneLayout` values in its own
`<controllerHome>/pane-layouts.json`, keyed first by stable `windowID` and then
stable `scopeID`; it never writes them to the selected workspace's
`UNPEEL_HOME`. Scope changes clear transient drop/focus/reveal state before
restoring that slot. Launcher panes are transient and never persist.

The **Split Pane** action creates the view; pane and multi-pane view are the
resulting product nouns. Detach and close alter only window presentation —
never project/folder organization or Session lifecycle on the Host. Host-side
Sessions MCP also cannot report a Controller window's private arrangement.
The full ownership and rendering invariants live in `docs/agents/panes.md`.

The native app currently owns one window (`windowID = "main"`). Future
workspace drag-out / **Open in New Window** should initially reuse
`UnpeelWorkspaceLauncher`, move one scope and its layout to one owning window,
and remount Host-backed Sessions there. Never clone live terminal `NSView`s.

**Move a project to another local workspace** (native project context
menu, 2026-08-18): when this Mac has two or more local workspaces, a
top-level project's right-click menu offers **Move to ▸ Workspace ▸
\<name\>**. The project record (plus its groups and worktree children)
and every session filed under that subtree — archived ones included —
move from the source home's `app-state.json` / `app-sessions/` /
`session-order.json` / `project-order.json` into the destination home.
Live hosts keep running: the session dir is renamed on the same volume,
so the PTY, `output.bin`, and `session.sock` survive (same host-based
survival model as an app restart). Hook broadcasts stay on the source
home's ports until that Host is later replaced. Hidden for groups,
worktree children, and a true remote Host scope. Same-Mac only — remote
Hosts are not destinations. Headless-serve parity is unbuilt.

Known v1 gaps (accepted): UserNotifications banner taps may activate the
wrong instance (bundle-id keyed; the wrong store just no-ops); Finder
service/Dock/`open` route to whichever instance LaunchServices picks;
`profiles.json` writes are atomic last-writer-wins.

### Workspaces from the CLI (`unpeel --workspace`, `unpeel workspaces`)

The CLI is a peer control surface over the same registry and homes
(Rust side: `crates/unpeel-cli/src/workspaces.rs`, added 2026-08-13):

- **`unpeel --workspace NAME [command]`** — resolves NAME (workspace name,
  case-insensitive, or slug = home dir basename) against the real
  `~/.unpeel/profiles.json`, then sets `UNPEEL_HOME` in-process **before any
  dispatch** — so it works for `unpeel serve` and every headless verb
  (`unpeel --workspace work ls`). Spawned hosts inherit the env, which is what
  keeps sessions, state, hook broadcasts (`UNPEEL_APP_PORT_REGISTRY_FILE` /
  `UNPEEL_HOOK_TRACE_FILE`), and pairing identity inside the workspace home.
  Also accepts `--workspace=NAME`. An unknown name offers to create the
  workspace on the spot (`create it? [y/N]`) — but only when stdin and stderr
  are TTYs; piped/scripted invocations get the hard error (exit 2, listing
  known slugs) instead of hanging on an invisible prompt, and the prompt
  itself lives on stderr so a piped `--json` stdout stays clean. If a
  registered home dir has vanished it is recreated empty rather than
  erroring.
- **`unpeel workspaces [list | add <name> | remove <name>]`** — manages the
  shared registry. `add` mirrors the app's create exactly: unique slug
  (`slugify` parity with `UnpeelWorkspaceRegistry.swift`), permanent home
  minted under `~/.unpeel/profiles/<slug>`, atomic `0600` write, identical JSON
  shape (`{version, profiles: [{id, name, home, createdAt}]}`), so app and
  CLI can each launch what the other created. `remove` only unregisters —
  the home dir is **always** kept (hook-config path permanence; the app's
  delete-data option stays app-only). `list` stars the active workspace and
  supports `--json`.
- **`unpeel --workspace NAME settings ...` and `presets ...`** — mutate that
  workspace's raw `app-state.json` through `app_state::edit`; settings never
  fall back to the default workspace. The bounded settings grammar, preset
  selectors/order, state-bus flush rule, and tests live in
  `docs/agents/cli.md`.

CLI-side rules:

- The registry path is always the **real** `~/.unpeel/profiles.json` —
  `workspaces.rs` must never resolve it through `app_paths::unpeel_home()`,
  which honors `UNPEEL_HOME` (that indirection would fork the registry the
  moment a workspace instance edits it).
- Registry reads/writes are unknown-key tolerant (serde `flatten` on the
  file and each record), so a newer app writing extra fields survives a CLI
  rewrite. Covered by the unit tests in `workspaces.rs`.
- No experimental gate on the CLI: the flag is pure env plumbing over the
  already-ungated `UNPEEL_HOME` mechanism (the app's
  `ExperimentalFeature.workspaces` gate is UI visibility, not a capability
  boundary). `unpeel serve` also has no per-home single-instance guard —
  multiple frontends on one home is the normal peer-frontend model, unlike a
  second app instance.
