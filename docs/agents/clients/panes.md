# Pane Layouts

> Status (2026-08-24): the pane model is a **recursive split tree** (mixed
> horizontal/vertical splits, up to 8 session leaves per group, zoom, spatial
> focus navigation), replaced pre-release from the earlier flat 4-pane row.
> Native is a client of `<controllerHome>/pane-layouts.json`
> `windows["main"][scopeID]`, conformance-tested against
> `protocol/pane-layout-operations-v1.json` — the normative operation
> contract. `windowID` remains the future native tear-out key. Transient
> focus, zoom, drop highlight, and launchers stay process-local. (Before
> 2026-09-03 the interactive terminal UI held its own Rust client of the same
> contract; that client was removed with the TUI, but the contract and file
> format are unchanged.) Mobile bootstrap carries only an optional sidebar summary:
> each group's representative Session and ordered Session members, never its
> geometry or transient state.

## Vocabulary

- **Split Pane** is an action: drop a Session on a pane edge or content edge,
  or invoke the menu command (Split Pane Right ⌘D / Split Pane Down ⌘⇧D).
- **Pane** and **multi-pane view** are the resulting objects. Do not assign
  elevated semantics to any one pane of a group.

## Ownership and model

`PaneLayoutState.swift` is the transport-neutral value model; the interactive
terminal UI's now-removed `panes` module was its Rust twin. **The shared fixture
`protocol/pane-layout-operations-v1.json` is the normative contract** — its
`contract` notes define every operation's semantics, the durable schema, the
v1→v2 migration, equalize weights, and spatial navigation. Both
implementations run every fixture case in their unit-test suites
(`PaneLayoutOperationsConformanceTests.swift`, the panes module's
`conformance` test). Change the fixture first; a semantics change that lands
in only one implementation fails that side's suite.

Model essentials:

- `PaneNode = leaf(Pane) | split(direction, ratio, left, right)` — a binary
  tree per group. Ratio is the left/top child's share, clamped to
  **[0.1, 0.9]**. A vertical split's *left* child is the *top* pane.
- `Pane` has a stable UUID id and content: a Session id or a transient
  launcher. Pane ids remain stable when a launcher binds to its new Session.
- `PaneGroup` has a stable id, the tree root, and one `representativePaneID`
  used to carry the collapsed sidebar row. The representative is a
  presentation anchor only — no special lifecycle, authorization, or Host
  meaning. Promotion picks the first session leaf in **preorder** (left-first
  traversal), which is also the sidebar enumeration order and the cap-trim
  order.
- A Session appears at most once within one layout state; a group holds up to
  **8 session leaves** (`maximumSessionLeafCount`). Session ids may collide
  across Hosts because window and scope identify the layout first.
- Inserting splits the target leaf 50/50 (left/up edges put the new leaf on
  the left/top side). A group-edge insert splits the root and gives the new
  leaf `1/(leafCount+1)`. Detaching collapses the parent split into the
  surviving sibling, so single-child splits never exist; a group dissolves
  below 2 session leaves (unless 1 session + a live launcher).
- **Equalize** sets each split's ratio by direction-aware leaf weights
  (Ghostty's algorithm). **Swap** exchanges two leaves' positions. **Resize**
  addresses a split by path (`[left|right]*` from the root).
- Opening a transient launcher snapshots the group's tree
  (`preLauncherRoot`). Canceling restores it exactly; binding, resizing,
  swapping, equalizing, or inserting a session makes the visible geometry
  durable instead.
- **Spatial focus navigation** (left/right/up/down) is a pure query over the
  tree's artificial grid dimensions — never over rendered pixels — so both
  frontends always agree on the neighbor.

`PaneLayoutController.swift` (and, before 2026-09-03, the terminal UI's own
controller) are/were clients of the same file. The Mac controller is an `@MainActor`, window-owned
`ObservableObject` publishing the current `PaneLayoutState` plus transient
presentation: the drop target (a pane edge or group edge), nonce-bearing
focus, one-shot reveal, active pane, and **zoom**. Zoom (temporarily
maximizing one pane, ⇧⌘↩) is process-local, never persisted, cleared on scope
switch, and cleared by any structural change to the zoomed group. Mutations
are copy-based transactions: a throwing or no-op mutation publishes and
persists nothing.

Durable membership and geometry are shared Controller-home state, same as
session order: writers flock `pane-layouts.json.lock` and ping
`/state-changed` (`pane-layouts`) so the other UI re-reads. Never put pane
membership in a Host capability, Session manifest, or Host operation. The
deliberate presentation-only exception is mobile bootstrap's optional
`paneGroups` projection: stable group id, representative Session id, and
ordered Session ids. It is additive, may be absent (which means render the
ordinary flat Session list), and excludes tree geometry, focus, zoom, drop
state, and launchers.

## Persistence and scope

Durable state lives at:

```text
<controllerHome>/pane-layouts.json
```

The versioned envelope is keyed in this order:

```text
windows[windowID][scopeID] = DurablePaneLayout
```

`DurablePaneLayout` is **version 2**: stable group ids, the representative,
and the tree as nested `{"pane":{"id","sessionID"}}` /
`{"split":{"direction","ratio","left","right"}}` nodes (exact key spellings
are normative — the v1 era had a Swift/Rust casing mismatch that broke
cross-frontend reads; v2 pins them). It deliberately removes launcher panes
(encoding the pre-launcher snapshot while one is open) and omits any group
with fewer than two session leaves.

**Version-1 flat layouts migrate on read** (both frontends, both legacy key
spellings): the pane list folds into a right-leaning horizontal chain with
ratios derived from the old fractions. Writes are always v2. Unknown future
versions fail closed: the Controller keeps the live in-memory layout but
refuses to rewrite the file, and never discards unknown sibling window/scope
state. Scope switches retain live state for the process even when a
persistence write could not complete.

`controllerHome` is the Controller process's home. It is never replaced with
a selected local workspace's `UNPEEL_HOME`, a paired/SSH Host path, or a
Session directory. Switching Host scope clears drop/focus/reveal/active/zoom
state and restores only the current `windowID` + `scopeID` slot.
Reconciliation must use eligible Session ids from that selected scope, never
the local Host's background scan.

## Rendering and Host boundary

The desktop Controller renders the tree recursively (leaf panes inside
GeometryReader-driven H/V stacks with the 8pt gap-divider as the resize
strip; live divider drags are local view state and commit one ratio on
release). The SwiftUI subtree is keyed by the tree's **structural identity**
— shape, directions, and leaf ids, deliberately excluding ratios — so a
divider drag never remounts a retained Metal terminal surface.

Multi-pane views work in every Host scope:

- this Controller's local Sessions use retained `SurfaceCache` terminals;
- loopback-workspace, paired, and SSH scopes use runtime-owned in-memory
  Ghostty panes with independent output cursor and resize state per visible
  Session; and
- remote panes never launch a local `unpeel-attach` or local hosted Session.

Dragging a Session over the content offers **four-sided drop zones on every
pane** (nearest-edge triangular hit test; short panes refuse up/down) plus a
narrow band at each content edge for group-edge splits. Dragging works in
every scope. When the current project's right sidebar is empty, dragging any
eligible Session or pane — including the ordinary solo pane — onto its
distinct green top-trailing Pin square files that Session into the project
Sidebar group and detaches it from the pane layout when it belonged to a
split. The square appears in a quiet armed state as soon as that empty-sidebar
drag starts. On hover it strengthens and morphs into the exact full-height
pane footprint produced by that root project's persisted sidebar width; the
expanded footprint remains the sticky hit area, its centered Pin glyph leans
toward the pointer, and the dragged card remains cursor-locked. Once open,
the panel's real pane frames remain the insertion targets. A sidebar
mouse-down still selects immediately for fast ordinary
navigation; once that press crosses the drag threshold, native restores the
terminal that was selected before the press so it remains the pane receiving
the dragged Session. (Historical gap, moot since the TUI's removal 2026-09-03: its remote-Host
scope had 4-edge drag/drop parity but not the `\`/`z`/`=`/alt-nav keyboard
verbs, because its navigation path would have called `mark_read`, a
local-marker write the remote-scope purity rule forbids.) The menu-driven empty pane launcher remains local-only because
choosing a preset creates a Session; remote preset creation needs its own
Host-routed binding and must never fall back to a local spawn.

Pane-layout operations do not mutate Host organization or lifecycle.
**Detach Pane** and **Exit Multi-Pane View** must not move a Session between
projects/groups, stop it, archive it, restart it, or close it. The Host-backed
Sessions continue independently and their ordinary sidebar rows reappear when
the view no longer collapses them under its representative row.

⌘W is intentionally a Session lifecycle command, not a pane-layout verb. Like
Ghostty's active-surface close, it targets the focused pane (including a solo
terminal): an empty launcher detaches immediately, a plain/non-resumable
terminal stops and removes immediately, and a resumable agent asks before it
is stopped and archived. Confirmation preserves its conversation and
artifacts; **Detach Pane** remains the non-lifecycle alternative.
The collapsed row's busy spinner aggregates every member Session; the
representative is only the row anchor, so activity in another pane must remain
visible in the sidebar.

Host-side Sessions MCP does not own or expose the full Controller pane tree.
There is no `panes`/`list_panes` action, Session-list `split_with`, inspect
`split_role`, or pane mutation, and no Host protocol capability is required
for panes. The narrow read-only exception is agent self-context:
`sessions.current` and `apps.context` re-read this Host's own
`windows["main"]["local"]` durable tree and return only the caller's direct
left/right/up/down neighbors. Each neighbor is classified as terminal, agent,
or Unpeel App and carries the ordinary Session id needed for an open read.
They expose no pane ids, ratios, pixel geometry, focus, zoom, or transient
visibility. A remote Controller's arrangement is not inferred from Host state;
that needs an explicit future protocol projection. A Mac frontend serving
mobile bootstrap may expose only the sidebar projection for its `main`
window and selected scope; the phone cannot read or mutate the private split
tree or its geometry.

Unpeel App panels use one deliberate semantic seam without weakening that
boundary. `apps.open` records that an App instance is associated with its
calling Session, a `panel` target, and a monotonic reveal revision. It does not
record a side, ratio, focus, visibility, or pane id. Local native Controllers
read the trusted binding, insert the companion on their own trailing/right
edge, and retain a Controller-local receipt when the user detaches it. A repeated transport request cannot undo that dismissal; a new
intentional open advances the revision and may reveal it again. The immediate
`apps.open` receipt returns only the association and makes no layout claim.
Once a local Controller has projected the binding, `apps.context` can identify
the companion Session only if it is one of the caller's direct durable
neighbors, using the narrow self-context projection above. Remote and phone
projection must travel as an additive, capability-advertised Host protocol
projection before those Controllers can render or report it; they must never
infer it from commands, Session roles, or pane summaries.

The phone still presents one terminal per screen. In its sidebar, a pane
group's representative Session keeps the ordinary main row and the remaining
members appear immediately below it in preorder, indented with a `└` marker.
Selecting any child opens that Session's own full-screen terminal. Malformed,
overlapping, or stale summaries fail open to the ordinary flat Session list.
This is a Controller UI choice, not a limitation of the selected Host.

## Window tear-out seam

Today `AppDelegate` creates one window and one `PaneLayoutController` with
`windowID = "main"`. Future workspace drag-out and **Open in New Window** must
move one scope and its saved layout to one destination window with its own
stable id. Initially reuse `UnpeelWorkspaceLauncher` for local workspaces; do
not invent another launch path.

The destination window remounts panes from Host-backed Session identities.
Never clone, share, or reparent live Ghostty/AppKit terminal `NSView`s between
windows. Session hosts survive the presentation handoff unchanged.
