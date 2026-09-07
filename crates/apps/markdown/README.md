# unpeel-markdown

A beautiful terminal markdown editor built with [Ratatui](https://ratatui.rs).
Live block styling, a headings picker, slash commands on empty lines
(`/` for headings, lists, to-dos, quotes, code, dividers), `[] ` to start a
to-do, full mouse support (click, drag-select, double/triple-click), and a
searchable Markdown Explorer when you open a folder.

Edits auto-save after a short typing pause. A status row shows the current
document state, while the semantic action footer exposes **New**, **Save**,
and the current auto-save state. Click the auto-save action to toggle it, or
choose **Toggle auto-save** from the `\\` command palette. The preference
persists across launches, and `Ctrl+S` always saves manually.

```
unpeel-markdown notes/hello.md    # edit one file
unpeel-markdown notes/            # vault mode: scoped Markdown Explorer
unpeel-markdown                   # remembered notes folder: open or create
```

The first bare launch asks you to choose a notes folder. In a hosted App,
App Kit's `AppContext::current_root()` pre-fills the active project's or
worktree's `docs` folder; a normal terminal pre-fills `./docs`. This is only a
suggestion: an explicit path or remembered notes folder always wins. Later
bare launches return to that folder's searchable note list, which includes a
**New note** action (`Ctrl+N`). The picker is App Kit's shared scoped
`Explorer`: it keeps folders navigable, admits only `.md` files, and cannot
leave the chosen notes root. Its shared borderless input supports a native
cursor, word movement, Shift selection, paste, mouse drag selection, and
double-click word selection. Typing while the file list is active immediately
focuses and writes into the filter; click the filter or press Up from the first
row to place the cursor there, then press Down or Tab to return to the list.
Rows keep the two-cell inset, full-width adaptive gray selection, double-click
activation, path dragging, and proportional scrollbar. No current directory
or bundled demo is opened implicitly. The
choice uses the same persisted start-state shape as Unpeel Design under
`~/.config/unpeel-apps/unpeel.app.markdown/start.json` (or `$XDG_CONFIG_HOME`
/ `$UNPEEL_APP_CONFIG_HOME`). A command-line path always bypasses the launcher.

Standalone first: it is a complete editor in any terminal with no Unpeel
present. When [Unpeel](https://unpeel.com) is installed, Unpeel recognizes
the `unpeel-markdown` CLI directly from `PATH`: the session row takes the
App's name and live project/workspace accent, and the sidebar shows which note
you're editing. No App registry write is required.

The binary builds one authoritative App Kit `MarkdownEditor` tree. Ratatui is
its standalone interpreter; when a Host injects a UI endpoint, SwiftUI/AppKit
and web become peer interpreters of that exact structure. Unicode-safe text deltas and
oriented selection deltas keep the caret and multi-line selection synchronized
without replacing the full document on each keystroke. Terminal drag and
Shift-selection, double-click word selection, and triple-click line selection
all enter that same revision stream. A renderer with an `edit` grant—including
an attached agent—uses the same semantic actions and acknowledgements.

The `/` insert picker remains a closed Markdown menu rather than a generic
widget tree. In the TUI it is a compact, caret-anchored bordered dropdown with
a full-width gray selection. In the native editor it stays left-aligned and
supports Up/Down, Home/End, Return/Tab, and Escape without stealing focus from
the document. `/` and the `\` command palette are opened through one optional
App Kit `openMenu` action, and the App publishes the resulting semantic Menu;
Swift, web, terminal, and agent participants therefore invoke the same Rust
reducer. Right-click selection actions are the same semantic Menu vocabulary,
mapped to the existing TUI PopupMenu, native `NSMenu`, and an ARIA web menu.
Auto-save and the file itself remain authoritative, so renderer disconnects,
native-view restarts, and terminal visibility changes do not lose the
document.

Vault browsing uses the same rule. The App owns one closed Tree with opaque
entry ids, filter/parent/open actions, selection, and compact deltas; Ratatui
uses App Kit's Tree/Explorer interpretation, native uses SwiftUI Tree/List
controls, and web uses an ARIA tree. Creating a note transitions to one
App-owned Page + Input and then one MarkdownEditor. The first-run chooser,
picker, new-note form, editor, slash menu, context menu, command hint, status
row, action footer, and task edits are all represented in the shared component structure;
the Kitchen Sink screen audit reports no terminal-only Markdown surface.

Status integration remains the documented plain-file plus local HTTP contract,
implemented once by `unpeel-app-kit`'s `AppReporter`. The optional semantic UI
uses App Kit's local, scoped UI bridge and stays inert when no endpoint is
injected. Reusable UI also comes from App Kit: live dark/light/accent colors,
gray selection, keyboard mode, scrollbars, native drop destinations,
`MarkdownTextArea`, and `MarkdownEditorInteraction`.

## Install

```sh
curl -fsSL https://unpeel.com/install/markdown/install.sh | sh
```

The checksum-verified installer places the CLI on `PATH`; Unpeel discovers it
without running the App or mutating `~/.unpeel`.

With Unpeel 0.6+, install the official build on the active workspace Host and
open a document through its typed resource dispatcher:

```sh
unpeel apps install unpeel.app.markdown
unpeel open README.md
```

The install command asks for confirmation in a terminal. Noninteractive
user-owned automation must pass `--yes` explicitly.

The same commands target the selected scoped/remote Host, so the binary and
file stay together on that workspace. A Controller can offer the same install
then-open flow when Markdown is missing. Agents can discover and suggest this
App through Unpeel MCP, but that MCP surface never installs or creates a
Session: those remain explicit user actions. Like all same-user terminal
policy, this is cooperative rather than an OS sandbox around arbitrary CLI
commands.

## Release

The public `unpeel` server repository owns the shared App publisher and the
official registry entry. From clean sibling checkouts on a Mac:

```sh
cd ../unpeel
cargo test --manifest-path ../unpeel-app-markdown/Cargo.toml
bun run release:app -- --app markdown --channel beta --dry-run
bun run release:app -- --app markdown --channel beta
```

The publisher builds an ad-hoc-signed macOS universal binary and accepts
Linux x86_64/aarch64 archives through its documented `--linux-*` flags. It
uploads immutable versioned archives plus the mutable `-latest` archive and
mandatory SHA-256 sidecar under `<channel>/markdown/`. Each tarball contains
one root member named `unpeel-markdown`, which is the exact contract used by
the standalone installer (and, from Unpeel 0.6, the Host-side App installer).

Publishing is intentionally separate from source tagging. Test the beta
installer and Host install before promoting the same version to stable.
