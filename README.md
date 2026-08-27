# unpeel-diffs

A small, standalone Ratatui App for reviewing the current Git working tree.
The default screen is a compact list of changed files; select one and press
Enter to open its unified diff.

```text
  DIFFS                                                   3 changed
────────────────────────────────────────────────────────────────────
  M  src/ui.rs                                           unstaged
  A  src/git.rs                                             staged
  ?  notes.txt                                           untracked
```

The UI follows the shared `../unpeel-tui-kit` design conventions used by the
Explorer and Usage Apps:

- borderless, transparent ordinary surfaces
- two-cell title and row-label inset
- full-width gray selected rows, with dark/light defaults
- the shared capless proportional scrollbar
- a pinned full-width gray Back row in detail views

This repository is intentionally separate from the core Unpeel client. It is
a standalone terminal App, not built-in diff or source-editor chrome.

## Run

```sh
cargo run --release -- ~/Dev/my-repository
```

With no path, it discovers the Git repository containing the current folder.
The viewer combines staged and unstaged tracked changes against `HEAD`, shows
untracked files as additions, and refreshes only when requested.

Keyboard controls:

- `↑` / `↓` or `j` / `k`: select a changed file or scroll its diff
- `Enter`: open the selected diff; in detail, activate Back
- `Esc`: return from a diff; it does not exit the file list
- `Home` / `End` or `g` / `G`: first/last file or top/bottom of a diff
- `Page Up` / `Page Down`: move one viewport
- `←` / `→` or `h` / `l`: pan wide diff lines
- `r`: reload Git status and the open diff
- `q` or `Ctrl-C`: quit

Mouse clicks select full rows and activate the Back row. The wheel scrolls the
current list or diff.

Set `UNPEEL_TUI_THEME=light` or `UNPEEL_TUI_THEME=dark` to override theme
detection. When launched inside Unpeel, the binary best-effort registers a
local `unpeel.app.diffs` manifest and publishes the selected file as App
context; all Git inspection remains local and read-only.
