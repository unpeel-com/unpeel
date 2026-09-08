<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Presets and Quick Presets

Preset model (`Preset` in `crates/unpeel-core/src/state.rs`):

- `id`, `label`, `command`, `project_id` (optional), `enabled`, `quick_launch`.
  `enabled` remains encoded for compatibility with older clients, but the
  native app's preset product is present-or-deleted and treats every stored
  global preset as enabled.

Where presets are stored (since the overlay migration, 2026-08-08):

- **`~/.unpeel/app-state.json` `presets` is the single source of truth — the
  array order defines command order within each plugin.** `plugin_order`
  defines the plugin rows. Both UIs read and write the shared file: the app
  edits it through `PresetStateFile.swift` (raw-JSON read-modify-write that
  preserves unmodelled keys, atomic temp+rename — the Swift twin of the Rust
  `app_state::edit`), and the CLI (`unpeel presets`) through `app_state::edit`.
  The app notices CLI writes via its FSEvents watcher on the file.
- The one-time fold: `migrateOverlayPresetsToSharedFile` (UnpeelStore) folds
  the legacy UserDefaults overlay (`unpeel.native.presets` added/edited/
  removedIDs + `unpeel.native.presetOrder`) into the file at launch and sets
  the top-level marker `native_preset_overlay_migrated: true`. The overlay
  keys are left in place (defaults are shared by bundle id — an older build
  running side by side must keep its state) but are never read again once
  the marker is set; every reader (app `rebuildPresets`, the CLI's
  `fallback_presets`, `unpeel presets list`) skips overlay presets when the
  marker is present. A file that exists but fails the typed decode is never
  folded over (`allowFold` guard). **Do not add new preset UserDefaults
  overlays or client-side preset caches — edit the file.**
- Un-migrated installs (app not yet run since the change): `unpeel presets
  list` shows overlay-held presets read-only, tagged "in the app — open it
  once to migrate".
- The native app is **global-presets-only** by design: Tauri-era
  project-scoped rows (`project_id != null`) are dropped from its view on
  decode but preserved in the file across rewrites. It does not read the
  Tauri-era per-project `<project>/.unpeel.json` presets.

Quick preset selection rules (`Presets.swift`):

- Only supported tool commands can be marked `quick_launch` (`sanitized()`).
- Quick access is selected once per agent or App. The
  sidebar strip shows **one chip per agent or App** (`collectQuickPresetGroups` →
  `QuickPresetGroup`): one command launches directly; multiple commands
  render the chip as a dropdown menu
  (`QuickPresetMenuChip` in `SidebarView.swift`).
- A blank-terminal pseudo-preset (`command == ""`) launches a plain shell instead of an agent CLI.

### Agents & Apps

`AgentsAppsSettingsPanel` combines installation, activation, and launch command
editing for the selected Host, including local loopback. One searchable list
contains compact agent and App rows under Active and Inactive. Each row aligns
its icon, commands, and controls in columns with a 5-point gap between rows.
The app name is available on icon hover and to accessibility. The default
Overview shows installed agents and all Apps, including Apps available to
install; the Not Installed filter also exposes uninstalled agents. Commands
edit inline; the “+” shown on command hover inserts another command below.
Each agent or App has one cursor-shaped Quick Launch toggle for all its
commands. New variants inherit that choice. Controls appear in this order:
Install/Update, Quick Launch, activation. Single-command rows are 36 points
tall; additional commands expand only the command column and row height.
The legacy Presets settings route redirects to Agents & Apps.

- **Host inventory:** bootstrap `workspaceSettings.availableAgents` reports
  installed agent binaries and the Host catalog's install commands. App metadata
  uses `availableApps`. Controllers never infer remote installation from local
  PATH. Install and Update run in a terminal on the selected Host.
  Installed agents use their runtime's dedicated update recipe when supplied;
  Claude uses its native installer for new installs and `claude update` for
  existing installs, avoiding npm overwriting a native launcher.
- **Activation:** `settings.plugins.set` advertises the additive
  `pluginActivation: {id, active}` patch on `/mobile/workspace-settings`.
  `app-state.json` stores `plugin_activation`, keyed by catalog identity (custom
  commands use `preset:<id>`). Missing entries mean active. Updates merge under
  the shared file lock and announce through the state bus. Deactivation preserves
  commands, quick-launch choices, binaries, and running sessions. It removes
  launch choices and excludes Apps from App/MCP discovery and resource opens.
- **Defaults:** both Host implementations call `plugins::project_presets`.
  Installed agents and Apps without saved commands receive a generated default.
  Editing it materializes a saved preset. Adding a variant preserves the default;
  the first global command per plugin is the default. Deleting the last saved
  command restores the generated one. Activation hides the whole plugin.
- **Order:** `settings.plugins.order` advertises `pluginOrder: [id, ...]` on the
  workspace-settings route, stored as `plugin_order`. Preset rows carry optional
  `pluginID`. Commands stay grouped in plugin order, with their saved array order
  inside each group. Reordering a filtered subset preserves hidden entries and
  unknown identities. The native list uses a detached drag card, sidebar spring
  motion, variable-height slots, cancellation, and edge auto-scroll. Only leaf row
  modifiers observe drag state; the full settings pane never rebuilds on
  insertion changes. The gap stays open until the card lands, then order and
  offsets swap in one transaction without animation. Toggle
  changes animate the same row between Active and Inactive. Reduced Motion is
  respected. Move Up/Down accessibility actions provide a drag alternative.
- **Commands:** editing, adding variants, making a command the default, and Quick
  Launch use `settings.presets.set`. The shared preset array remains the single
  truth, preserving unknown fields and legacy project rows.
- **Compatibility:** older Hosts retain supported operations. Clients check each
  advertised capability and require a Host update for unsupported controls.

Legacy UserDefaults preset migration and first-run usage seeding continue to
fold into the shared file once; no new Controller-side preset overlay is added.

Install and Update open a live Host terminal below the Agents & Apps list.
Update is shown only after a successful Host check finds a newer release.
Opening this pane starts `settings.plugins.updates.read` on
`GET /mobile/plugin-updates`; bootstrap never starts probes or network work.
The Host caches results for 15 minutes by installation identity, invalidates
them when a binary/record changes, and limits checks to three at a time.
Release Apps compare their recorded archive digest to the channel's latest
checksum; agent version probes and upstream metadata come from their runtime
packages. Linked builds, unknown versions, and failed checks do not offer an
update. Closing Settings cancels polling; all checks remain scoped to the Host.
The installer is a regular background-created session; Settings and the current
workspace selection stay open. A separate presentation owner keeps its terminal
stream alive across workspace refreshes. Hiding it only unmounts the pane; the
session remains available in the workspace. App actions run the Host's
`unpeel apps install/update` command through that same terminal. The Host
publishes the absolute installer command in `availableApps[].installCommand`,
using its bundled sibling CLI so shell PATH changes cannot select an older CLI.

The preset wire carries optional `projectID` for legacy rows. Controllers show
only global rows; editing, removing, and reordering global commands preserve
project overrides, including overrides that reuse a global preset ID.

For a private-home UI snapshot, combine `UNPEEL_SNAPSHOT` and
`UNPEEL_OPEN_SETTINGS=agentsApps` with `UNPEEL_TEST_SETTINGS_COMMAND=<command>`
to exercise the embedded terminal through the real Host session path.
