# unpeel-filetree

A deliberately small development TUI for proving terminal-to-terminal path
dragging in Unpeel. Its flat, borderless directory view is built entirely from
the reusable `Explorer` component in the sibling `../unpeel-app-kit` crate.
The interaction model follows
[`ratatui-explorer`](https://github.com/tatounee/ratatui-explorer): the current
folder is a single list with a `../` parent row rather than an expanded
recursive tree. Its launch directory is the project boundary: `../` appears
only after entering a child folder, and neither parent navigation nor a
directory symlink can escape above that root.

This is a test harness, not a proposed built-in Unpeel file browser. It lives
outside the Unpeel repository and does not add a preset or file-tree chrome to
the product.

When a Host injects App Kit's optional UI endpoint, the same Explorer publishes
the closed semantic Tree projection. SwiftUI and web render native/ARIA tree
rows with the same filter, selection, parent, directory, file, and activation
semantics; ids are opaque and absolute paths remain inside the App for trusted
local opening and dragging. With no Host present the bridge is inert and this
remains the unchanged standalone Ratatui TUI.

## Install

The hosted binary route is ready for the App release channel but its artifact
has not been published yet. For now, install from source with App Kit checked
out beside this repository:

```sh
mkdir -p ~/Dev && cd ~/Dev
git clone https://github.com/unpeel-com/unpeel-app-kit.git
git clone https://github.com/unpeel-com/unpeel-app-filetree.git
cargo install --locked --path unpeel-app-filetree
```

Once the release artifact is published, the checksum-verified binary installer
will be:

```sh
curl -fsSL https://unpeel.com/install/filetree/install.sh | sh
```

Unpeel detects the installed `unpeel-filetree` CLI directly from `PATH`; no
registration command or `~/.unpeel/apps` write is needed.

## Run

```sh
unpeel-filetree ~/Dev
unpeel-filetree --ext md ~/Notes
unpeel-filetree --ext md,mdx .
```

With no explicit path, App Kit's `AppContext` gives a hosted Files pane its
Host-owned project or active worktree immediately. It then follows its
neighboring/main agent's checkout: if that agent moves into a Git worktree,
Files rebinds its scoped root there; when the agent returns to the main
checkout, Files follows back. A standalone run falls back to its process
working directory. Supplying a path always wins and pins the Explorer to that
root.

`-e` / `--ext` is repeatable and accepts a leading dot or comma-separated
values. When present, the Explorer lists only files with those extensions and
directories containing at least one matching file somewhere below them. The
recursive check follows the hidden-file toggle and never follows directory
symlinks. Without `--ext`, the ordinary all-file view remains the default.

Keyboard controls:

- `↑` / `↓`: move; `↑` from the first row focuses the filter
- `→` or `Enter`: enter the selected directory
- `Home` / `End`: first / last item
- `Page Up` / `Page Down`: move by one viewport
- start typing anywhere in the Explorer to focus and write into the filter;
  `/`, `Tab`, `Ctrl-F`, or clicking the filter focuses it without inserting text
- while filtering, type normally; `←` / `→` and `Home` / `End` move the text
  cursor, `Shift` extends selection, and `Ctrl`/`Option` + arrows move by word
- `Backspace` / `Delete` replace or remove selected text, `Ctrl`/`Cmd-A`
  selects all, `Ctrl-U` clears, paste inserts at the cursor, and `Tab` returns
  focus to the file list; `↓` also returns to the list
- with the file list focused, `Esc`, `←`, or `Backspace`: go to the parent
  folder
- `Ctrl-H`: show or hide dotfiles
- `Ctrl-R`: refresh
- `Ctrl-C`: quit

One click selects a file or folder. Double-click a folder (including `../`) to
enter it, or a file to activate it. In the filter, click to place the native
text cursor, drag to select text, Shift-click to extend a selection, and
double-click to select a word. Right-click for the shared gray
`PopupMenu`: it offers **Open in editor**, **Send to agent** when a same-group
Unpeel agent is available, and **Copy path**. The shared editor action follows
Unpeel's configured editor when hosted and the platform opener when standalone.
Sending pastes a safe absolute path reference into the agent's input without
pressing Enter.

The App enables mouse reporting in ordinary terminals as well as hosted panes.
Inside Unpeel, the native terminal wrapper intercepts a mapped left-button
drag before the TUI receives it, so native path dragging still works; an
ordinary click is replayed to the TUI for selection. While the popup is open,
the App publishes an empty drag map so a click cannot drag a path hidden
beneath the menu.

## Drag test

1. Build and run the current Unpeel Dev app.
2. Open two panes on the same local Host.
3. In one pane, run this binary against a small test folder.
4. In the other pane, start Claude Code or leave a shell prompt open.
5. Drag an Explorer row into the other terminal.

The destination should receive a shell-quoted path as bracketed paste: relative
to the destination Session's project when possible, `~/…` elsewhere below the
home folder, and absolute only outside both roots. No Enter is sent. Files and
folders use the same path-only operation; nothing is moved or copied by Unpeel.

The launch path (or current working directory when no path is passed) is
canonicalized as the Explorer's hard root. The current-folder path at the
bottom and every visible row are drag sources.
The shared component publishes absolute Host-local paths through `DragSurface`,
so the same transferable item can be consumed by terminals now and by other
Unpeel Apps later. The kit also exposes `DraggablePath`, the generic
`DragSource<W>` wrapper, and the lower-level `DragSurface::register` API.

Appearance comes from the kit's dark/light defaults. Set
`UNPEEL_TUI_THEME=light` or `UNPEEL_TUI_THEME=dark` to override detection.
Selected rows span the full list width and keep the shared two-cell content
inset. Unpeel's Session title owns the App name, so content starts immediately
without a repeated in-App title. The bottom row contains only the muted,
project-relative current-folder path (`.` at the launch root); that path is not
repeated below the filter, and there is no shortcut help.

For development inside Unpeel, build once and prepend the debug output folder
to `PATH` before launching. The Host uses the same central CLI catalog as an
installed build; running the App never writes registration state:

```sh
cargo build
PATH="$PWD/target/debug:$PATH" unpeel-filetree .
```
