<!-- Split out of the repo-root AGENTS.md (2026-08-05). The root AGENTS.md holds the map, hard rules, and invariants; this file is the full detail for its topic. -->

## Git Worktrees

Sessions can run in a git worktree of the project instead of the main checkout, so multiple agents can work the same repo in parallel without touching each other's files.

- Worktree git operations (native): `apps/native/UnpeelNative/Sources/UnpeelNative/WorktreeGit.swift` shells out to stock `git worktree`.
- Unpeel-created worktrees live in `~/.unpeel/worktrees/<repo-name>-<hash>/<worktree-name-slug>/`, outside the repo (same convention as Codex's `~/.codex/worktrees`). If no custom worktree name is provided, the branch slug is used. Legacy in-repo `<repo>/.worktrees/` checkouts are still recognized as Unpeel-managed.
- Each used worktree becomes a child `Project` (`worktree_branch` + `parent_project_id` fields), so it groups multiple sessions and reuses all project UI.
- The native app also discovers worktrees created by Claude,
  Codex, or any other tool. Discovery is provider-neutral and project-scoped:
  every five seconds it reads `git worktree list` from an existing top-level
  project, adopts only checkouts linked to that repository, and renders them
  through the same child-folder UI. The checkout folder name becomes the
  label (falling back to the branch when the folder just repeats the repo
  name). Detached worktrees (which Codex sometimes creates) use a visible
  `detached@<commit>` marker in the existing worktree-branch field.
  Automatically adopted records are mirrored to `app-state.json` for
  remote projections (previously also the TUI's) and are forgotten when Git no longer lists the
  worktree; explicitly created/adopted projects are never pruned by discovery.
  Discovery is additionally **opt-in** (2026-08-23): the "Show agent
  worktrees" toggle in Settings ▸ Worktrees
  (`UnpeelStore.showAgentWorktrees`, default off) gates it — off skips
  discovery and purges previously auto-discovered records (safe: they are
  reconstructible; explicit worktree projects are untouched), on rediscovers
  them on the next rescan. That same Settings tab lists this workspace's
  worktree child projects and offers create / reveal / remove.
- Worktrees are created before Sessions launch. The project context menu's **"New worktree…"** dialog and Settings ▸ Worktrees provide explicit branch/base-ref control and register the child project without starting a Session. Alternatively — opt-in, 2026-07-19 — the sessions tool's **`create_worktree`**/`list_worktrees` actions can prepare one (gated on `AppState.mcp_worktree_access`, the "Let sessions create worktrees" toggle in Settings ▸ Worktrees / Settings ▸ Sessions use; default off, host re-reads per call; bridge routes `/mcp/create-worktree|list-worktrees`). Once the child project appears, launch the Session from that row. The MCP path never launches a Session and never exposes removal; agent inputs are constrained — `branch` via `git check-ref-format`, folder `name` slugified to `[A-Za-z0-9-_.]`, flag-shaped `base_ref` refused at the bridge.
- **Default base ref is the mainline, not HEAD** (2026-07-19): when no base is picked, `WorktreeGit.createWorktree` forks new branches from `defaultBaseRef` — `origin/<default>` (via the `origin/HEAD` symref, falling back to `origin/main`/`origin/master`, then local `main`/`master`), freshened by `bestEffortFetch` (non-interactive `GIT_TERMINAL_PROMPT=0`, 5s timeout, failure ignored — offline must never block creation); no recognizable mainline → HEAD, the old behavior. The "New worktree…" dialog's Start-from popup lists local + remote-tracking branches and pre-selects the mainline. Controller-driven starts with explicit worktree fields and MCP creation reuse `sessionLaunchTarget` (`MCPBridge.swift`): reuse existing child project → adopt existing checkout → create branch + worktree.
- `SessionInfo` carries optional `worktree_path` / `worktree_branch`; stopped
  **Resume**, archived **Restore & Resume**, and terminal reload re-spawn inside
  the worktree, while same-PTY **Resume Agent** never leaves it.
- No special agent integration is needed: inside a worktree, git behaves like a normal checkout, so provider CLIs work unchanged.

## Inline child folders (2026-08-10)

Worktree children render as **inline collapsible folder rows** directly under
the parent project's header, above the parent's own sessions — in the Mac app
and phone sidebar (previously also the TUI, removed 2026-09-03). There is no
drill-in navigation (the app's slide-in "Worktrees" panel and the TUI's
scoped view + back row — the latter moot now — are gone). Folder rows
lead with a disclosure chevron in the session-mark gutter, align their names
with the parent's normal session labels, render those names at full foreground
contrast (top-level project names remain muted), default collapsed, show a
`+` badge on hover, and surface busy shimmer / attention from folded sessions.
Sessions inside a folder use one standard indentation step beneath
its label, without an additional child-folder offset. (The now-removed TUI's
child-folder chevrons occupied the left gutter so plain group names aligned
with normal session labels.) Project-chevron backgrounds appear only when that project has an
explicit folder color.
Expansion state: `expandedProjectIDs` (app, persisted) /
`App.expanded_worktrees` (the now-removed TUI's in-memory equivalent). Child rows drag-reorder among
same-parent siblings only. Both frontends persist those moves in the flat
`project-order.json` rank list (filter by parent to recover each sibling
order), under the shared file lock; a folder move in either UI is visible in
the other immediately. (The now-removed TUI kept Git worktrees
keyboard-selectable as destinations, with plain groups as structural
disclosure headers that keyboard navigation skipped.)

## Groups (2026-08-10)

A **group** is a plain organizational child folder: a child `Project` with
`parent_project_id` + `is_folder: true` and **no** `worktree_branch`, sharing
the parent's path. Groups render exactly like worktree folder rows minus the
branch glyph, and sort among them. Creation: project context menu
**"New group…"** (app: `promptCreateGroup`; the now-removed TUI used
`Modal::GroupInput` → `app_state::edit()`).

Moving a session into a group is the session context menu's **"Move to"** —
Git worktrees are intentionally excluded because changing checkout requires
an explicit restart/resume. The group move writes the shared per-session marker
`~/.unpeel/app-sessions/<id>/project-override.json`
(`{"project_id": "<target>", "moved_at": ms}`), display + ordering only; the
manifest's `project_id` stays the launch truth. Helpers:
`session_ops::{set,clear}_project_override` (Rust) and
`SharedMarker.projectOverride` (Swift); writes announce `session-markers`.
A stale target falls back to the manifest project. The explicit **Remove
group** verb first unpins, rehomes, and archives every contained session under
the parent, then deletes the group record; this keeps sessions launched from a
group's own "+" reachable even though their manifest project is the removed
group id. In the native app,
session rows can also be dragged directly onto plain group rows; worktree rows
intentionally reject session drops for the same reason.
Project-scoped pins follow the session to its destination.

(The now-removed TUI's group menu exposed **Rename group…** and **Remove
group…**, routed through `/mcp/rename-group|remove-group` when the app was
running, updating either the native record or the shared-file record, or
editing `app-state.json` directly in standalone mode.) The desktop group menu still lacks the rename
verb. The remote bootstrap sends plain groups as project children with additive
`isGroup` and `colorID` fields, and resolves each session's effective group
placement before sending it. Older controllers ignore those fields; the phone
uses them to render groups inline while preserving the legacy top-level folder
accordion.
