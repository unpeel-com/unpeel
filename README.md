# unpeel-diffs

A small, standalone Ratatui App for reviewing the current Git working tree.
The default screen is a compact list of changed files; select one and press
Enter to open its unified diff.

```text
  M  ui.rs                                               unstaged
  A  git.rs                                                 staged
  ?  notes.txt                                           untracked
```

The UI follows the shared `../unpeel-app-kit` design conventions used by the
Explorer and Usage Apps:

- borderless, transparent ordinary surfaces
- two-cell row-label and muted absolute-path footer inset
- full-width gray selected rows, with dark/light defaults
- the shared capless proportional scrollbar
- a pinned transparent Back action with a full-width click target in detail views
- transparent diff surface with green/red row tints behind added and removed lines
- changed-file rows are native path drag sources, matching the Explorer App
- right-click context menus with adjacent-agent handoff, like the Explorer App

Unpeel's Session title owns the App name, so the content has no repeated
in-App title. The bottom row contains only the selected or open file's muted
absolute path; it has no shortcut help.

The default list shows only each basename to stay scannable in narrow panes.
Opening a diff reveals its full repository-relative path.

This repository is intentionally separate from the core Unpeel client. It is
a standalone terminal App, not built-in diff or source-editor chrome.

## Install

```sh
curl -fsSL https://unpeel.com/install/diffs/install.sh | sh
```

The checksum-verified installer registers the versioned App manifest
immediately under `~/.unpeel/apps/unpeel.app.diffs/`.

## Run

```sh
cargo run --release -- ~/Dev/my-repository
```

With no path, it discovers the Git repository containing the current folder.
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
diff. Dragging a changed-file row into an agent terminal drops its absolute
file path using the same App Kit primitive as Filetree. The Back action
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
In the file list, right-clicking a row offers the same **Send to agent**
(bare repo-relative path) / **Copy path** pair as the Explorer App.

Set `UNPEEL_TUI_THEME=light` or `UNPEEL_TUI_THEME=dark` to override theme
detection. When launched inside Unpeel, the binary best-effort registers a
local `unpeel.app.diffs` manifest and publishes the selected file as App
context; all Git inspection remains local and read-only.
