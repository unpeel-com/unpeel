# unpeel-diffs

A small, standalone Ratatui App for reviewing the current Git working tree.
The default screen is a compact list of changed files; select one and press
Enter to open its unified diff.

It remains a complete plain-terminal app with no Unpeel process present. When
the Host injects an App Kit UI socket, the same binary additionally publishes
its `Page`/`List` tree so SwiftUI, web, and scoped agent participants share the
terminal-owned selection and can open or close diffs. Renderer-only selection
changes travel as compact list deltas; the Ratatui view remains the fallback.

```text
  M  ui.rs                                               unstaged
  A  git.rs                                                 staged
  ?  notes.txt                                           untracked
```

The UI follows the shared `../unpeel-app-kit` design conventions used by the
Explorer and Usage Apps:

- borderless, transparent ordinary surfaces
- two-cell row-label and muted project-relative footer inset
- full-width gray selected rows, with dark/light defaults
- the shared capless proportional scrollbar
- a pinned transparent Back action with a full-width click target in detail views
- transparent diff surface with green/red row tints behind added and removed lines
- syntect syntax colors on diff code lines (by file type; dark/light themes),
  falling back to plain text for unknown types or very large diffs
- changed-file rows are native path drag sources, matching the Explorer App
- right-click context menus with preferred-editor opening and adjacent-agent handoff

Unpeel's Session title owns the App name, so the content has no repeated
in-App title. The bottom row contains only the selected or open file's muted
repository-relative path (`.` when no file is selected); it has no shortcut
help.

The default list shows only each basename to stay scannable in narrow panes.
Opening a diff reveals its full repository-relative path.

This repository is intentionally separate from the core Unpeel client. It is
a standalone terminal App, not built-in diff or source-editor chrome.

## Install

The hosted binary route is ready for the App release channel but its artifact
has not been published yet. For now, install from source with App Kit checked
out beside this repository:

```sh
mkdir -p ~/Dev && cd ~/Dev
git clone https://github.com/unpeel-com/unpeel-app-kit.git
git clone https://github.com/unpeel-com/unpeel-app-diffs.git
cargo install --locked --path unpeel-app-diffs
```

Once the release artifact is published, the checksum-verified binary installer
will be:

```sh
curl -fsSL https://unpeel.com/install/diffs/install.sh | sh
```

Unpeel detects the installed `unpeel-diffs` CLI directly from `PATH`; no
registration command or `~/.unpeel/apps` write is needed.

## Run

```sh
unpeel-diffs ~/Dev/my-repository
```

With no path, a hosted pane first discovers Git from App Kit's Host-owned
`AppContext::current_root()` and then follows the neighboring/main agent's
actual checkout: it switches into that agent's worktree and back to the main
checkout automatically. A standalone run discovers from its process working
directory. Passing a path always wins and keeps Diffs pinned to that
repository.
The viewer combines staged and unstaged tracked changes against `HEAD` and
shows untracked files as additions. While idle it quietly follows the
working tree (about once a second), so the list and the open diff update as
the project changes; `r` still forces an immediate reload.

Keyboard controls:

- `↑` / `↓` or `j` / `k`: select a changed file or scroll its diff
- `Enter`: open the selected diff; in detail, activate Back
- `Esc`: return from a diff; it does not exit the file list
- `Home` / `End` or `g` / `G`: first/last file or top/bottom of a diff
- `Page Up` / `Page Down`: move one viewport
- `←` / `→` or `h` / `l`: pan wide diff lines
- `r`: reload Git status and the open diff
- `q` or `Ctrl-C`: quit

One mouse click selects a full row; double-clicking that same row opens its
diff. Dragging a changed-file row into an agent terminal drops a concise path
(project-relative first, then `~/…`, then absolute) using the same App Kit
primitive as Filetree. The Back action
activates with one click, and the wheel scrolls the current list or diff.

## Selecting diff lines and sending them to an agent

Inside a diff, click a line to select it and drag (or Shift-click) to grow
the range; selected lines use the shared full-width gray highlight.
Right-clicking offers **Send to agent** (when an agent pane is nearby —
same-group peers are preferred, and the Host asks for approval before a
cross-group write) plus **Copy lines**; `Enter` or `s` on a selection sends
directly. Sending pastes only the filename and line numbers — the
repo-relative path with the file lines the hunks map the selection to, such
as `src/ui.rs:120-134` — into the agent's input without submitting, so the
comment and the final prompt are written in the agent chat. Without an
agent, the reference is copied to the clipboard instead.
In the file list, right-clicking a row offers **Open in editor**, **Send to
agent** (bare repo-relative path), and **Copy path**. The diff-line menu also
offers **Open in editor** for the current file. The shared editor action uses
Unpeel's configured editor when hosted and the platform opener when standalone.

Set `UNPEEL_TUI_THEME=light` or `UNPEEL_TUI_THEME=dark` to override theme
detection. When launched inside Unpeel, the Host detects the CLI from `PATH`
and App Kit's shared `AppReporter` publishes the selected file as
agent-readable context; all Git inspection remains local and read-only.
