# Git

A standalone Git sidebar App with **Changes** and **History** tabs at the top.
The terminal and native renderers use the same App Kit Page, tabs, lists, and
patch content. The display name is Git; `unpeel-diffs`, the `diffs` install
slug, and `unpeel.app.diffs` remain stable for existing installs and presets.

**Changes** shows staged, unstaged, conflicted, and untracked files. Open a
file to review its unified diff. Tracked changes compare the working tree to
`HEAD`; untracked files appear as additions. The list and open patch quietly
refresh about once a second. File names have compact trailing status symbols:
green `⊞` for new files, red `⊟` for deletions, yellow `⊡` for modifications,
an arrow for renames, and `!` for conflicts. Status labels preserve staging
information for accessible renderers.

Patch code uses language-aware syntax highlighting, including Swift, Rust,
JavaScript, TypeScript/TSX, Python, JSON, and shell. Keywords, names, strings,
numbers, and comments use App Kit's shared foreground tones while additions
and deletions retain full-width row tints. The parsed runs are cached until
the patch changes, so scrolling and selection do not reparse code. The old
and new sides keep separate multiline parser state, reset between hunks.
Unknown file types and patches exceeding 4,000 lines, 1 MiB, or a 16 KiB line
retain plain diff colors. Syntax definitions come from
[two-face](https://docs.rs/two-face/); `unpeel-diffs --syntax-licenses` prints
the bundled collection and grammar acknowledgements.

**History** shows the current checkout's commits, newest first, with subject,
author, date, and short commit ID. Open a commit to browse its changed files,
then open a file to review that commit's patch. Root commits compare to the
empty tree; merges compare to their first parent. The first 100 commits load
initially; **load older** fetches another 100. Historical patches remain pinned
to their commit while the working tree and history change.

The branch (or detached commit ID), change count, and history count provide
repository context. Empty repositories show **No commits yet**; clean trees
show **Working tree clean**. Both tabs are available on detail pages.

The top-right control shows **Fetch**, **Pull ↓N** when behind, or **Push ↑N**
when ahead. Its dropdown always lists the three operations. Git uses the
branch's configured upstream; without one, only Fetch is available. With no
remote the control is disabled. Fetch runs on demand; periodic UI refreshes
only read local Git state.

Remote operations run in the background with a busy indicator. Pull fetches
then fast-forwards without autostashing; diverged branches need resolving
outside this App. Push sends the captured commit to the exact upstream ref,
without force. Checkout changes during an operation are detected before a
pull updates the working tree. Existing Git credential helpers are used;
terminal password prompts are disabled. Failures appear in the title.

## Build and run

From this repository:

```sh
cargo build --release --manifest-path crates/apps/Cargo.toml -p unpeel-diffs
crates/apps/target/release/unpeel-diffs ~/Dev/my-repository
```

For local Unpeel development, `bun run apps:link diffs` builds and links the
managed App slot. Use the pane menu's **Restart App** after a rebuild; running
processes keep their loaded binary. The App is released independently with
the `diffs` slug; linking a development build does not publish it to R2.

Without a path, hosted panes use App Kit's `AppContext::current_root()` and
follow the adjacent agent between checkouts and worktrees. Standalone runs
use the process working directory. An explicit path pins the repository.

## Navigation

- Click **Changes** or **History**, or use `1` / `2` or `Tab` / `Shift-Tab`.
- `↑` / `↓` or `j` / `k` select files or commits, or scroll a patch.
- `Enter` opens the selected row; in a patch without a selection it goes back.
- `Esc` clears a patch selection, then goes back one level. It keeps root lists open.
- `Home` / `End`, `g` / `G`, and `Page Up` / `Page Down` navigate lists or patches.
- `←` / `→` or `h` / `l` pan wide patch lines.
- `F10` opens the remote-action dropdown; arrows, Enter, and Esc operate it.
- `r` refreshes; `n` loads older commits when available in History.
- `q` or `Ctrl-C` quits.

Working-tree file rows support path dragging and context actions to open in
an editor, send a path to the adjacent agent, or copy a path. Historical files
open their committed patches; they are not drag sources for checkout files.

Inside a patch, click and drag or Shift-click to select lines. The context
menu offers **Copy lines** and **Send to agent**; `s` or `Enter` sends a
selection's file reference without submitting the agent's prompt. History
references include the full commit ID (`commit:path:line`), so they identify
that revision. If no agent is available, the reference is copied instead.

## Verification

```sh
cargo test --manifest-path crates/apps/Cargo.toml -p unpeel-diffs
```
