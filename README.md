# unpeel-filetree

A deliberately small development TUI for proving terminal-to-terminal path
dragging in Unpeel. Its flat, borderless directory view is built entirely from
the reusable `Explorer` component in the sibling `../unpeel-tui-kit` crate.
The interaction model follows
[`ratatui-explorer`](https://github.com/tatounee/ratatui-explorer): the current
folder is a single list with a `../` parent row rather than an expanded
recursive tree.

This is a test harness, not a proposed built-in Unpeel file browser. It lives
outside the Unpeel repository and does not add a preset or file-tree chrome to
the product.

## Run

```sh
cargo run --release -- ~/Dev
```

Keyboard controls:

- `↑` / `↓` or `j` / `k`: move
- `→`, `l`, `Enter`, or `Space`: enter the selected directory
- `Home` / `End` or `g` / `G`: first / last item
- `Page Up` / `Page Down`: move by one viewport
- `/` or `Ctrl-F`: focus the current-folder filename filter
- while filtering, type normally; `Backspace` edits, `Ctrl-U` clears, and
  `Tab` returns focus to the file list
- `Esc`, `←`, `h`, or `Backspace`: go to the parent folder
- `Ctrl-H`: show or hide dotfiles
- `r`: refresh
- `q`: quit

Inside an Unpeel-hosted pane, right-click a file or folder for the shared gray
`PopupMenu`: it offers **Send to agent** when a same-group agent is available,
and **Copy path**. Sending pastes a safe absolute path reference into the
agent's input without pressing Enter. Outside Unpeel, the App leaves terminal
mouse capture disabled and stays keyboard-driven.

In a hosted pane the App enables mouse reporting for right-click and hover.
Unpeel's native terminal wrapper intercepts a mapped left-button drag before
the TUI receives it, so native path dragging still works; an ordinary click is
replayed to the TUI for selection. While the popup is open, the App publishes
an empty drag map so a click cannot drag a path hidden beneath the menu.

## Drag test

1. Build and run the current Unpeel Dev app.
2. Open two panes on the same local Host.
3. In one pane, run this binary against a small test folder.
4. In the other pane, start Claude Code or leave a shell prompt open.
5. Drag an Explorer row into the other terminal.

The destination should receive the absolute path, shell-quoted when needed,
as bracketed paste. No Enter is sent. Files and folders use the same path-only
operation; nothing is moved or copied by Unpeel.

The current-folder header and every visible row are drag sources. The shared
component publishes absolute Host-local paths through `DragSurface`, so the
same transferable item can be consumed by terminals now and by other Unpeel
Apps later. The kit also exposes `DraggablePath`, the generic `DragSource<W>`
wrapper, and the lower-level `DragSurface::register` API.

Appearance comes from the kit's dark/light defaults. Set
`UNPEEL_TUI_THEME=light` or `UNPEEL_TUI_THEME=dark` to override detection.
Selected rows span the full list width and keep the shared two-cell content
inset. The App title is the two-cell-inset uppercase `FILES` header used by
the other list Apps, with a full-width separator before the filter and folder
content.

The binary self-registers a development-only App manifest under an existing
`~/.unpeel/apps/unpeel.app.filetree/` when launched. If it is not installed on
`PATH`, that manifest points to the exact development binary that registered
it.
