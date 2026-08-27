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
- `←`, `h`, or `Backspace`: go to the parent
- `Home` / `End` or `g` / `G`: first / last item
- `Page Up` / `Page Down`: move by one viewport
- `Ctrl-H`: show or hide dotfiles
- `r`: refresh
- `q`: quit

The app intentionally does not enable terminal mouse capture. That leaves the
pointer with the terminal emulator so Unpeel can initiate a native macOS drag
using the exact terminal-cell regions published by `unpeel-tui-kit`.

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

The binary self-registers a development-only App manifest under an existing
`~/.unpeel/apps/unpeel.app.filetree/` when launched. If it is not installed on
`PATH`, that manifest points to the exact development binary that registered
it.
