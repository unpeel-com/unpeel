# Agent notes for unpeel-app-kit

## Renderer priority

Focus on the Ratatui terminal components and the SwiftUI interpreters in
`swift/Sources/UnpeelAppKitUI`. Every new component or field ships with both
of those, shared fixtures in `protocol/unpeel-ui-v1.ndjson`, and the schema in
`protocol/unpeel-ui-v1.schema.json`.

The web interpreters in `web/src` come later. Keep `web/src/protocol.ts`
types and validation in step so the bundle keeps compiling and unknown fields
stay wire compatible, but do not block terminal or Swift work on web
rendering. Mark web gaps with a `TODO` in `docs/ui-components.md`.

## Writing Apps

New Apps use the runner: implement `App` (`page()` + `reduce()`), call
`run_app`, and never touch the terminal, the bridge, revisions, or deltas.
`docs/writing-an-app.md` is the complete guide; keep it short and keep the
`AppAction` enum closed.

## Screen style guide

Every screen root (Page, Tree, Markdown editor, TextBox pages) follows the
same shape so Apps feel like one product:

- **Title on top.** The first row is the screen title. When the screen has a
  `back` action it starts with a `‹` chevron; only the chevron takes the gray
  fill on hover, press, or keyboard focus, never the whole row. Up from the
  first row focuses the chevron, Enter or Escape there goes back. Always
  leave one empty row beneath the title (Page, Tree location, Explorer path,
  and the Markdown title all reserve it when the screen is tall enough).
- **Actions at the bottom.** App commands live in the shared `FooterActions`
  slot (`key label` pairs). Never repeat footer state in the title; the
  footer owns it (for example auto-save is a footer action, not title text).
  In-progress actions set `busy(true)` and animate the shared braille
  `Spinner`.
- **Rows.** Lists use `ListItem` rows: full-width selection background,
  a distinct lighter `hovered` background, two-cell left inset, one-cell right
  inset. Group rows with `ListItem::divider` / `divider_labeled`, and use
  bands (`top` / `bottom`) or a media column instead of ad hoc row painting.
- **Filters.** A search or filter field is a header input above the rows
  (Page `Input`, Tree `filter`). It behaves like a row for focus: Up from
  the first row lands on it, it takes the selection background while
  focused, and typing goes straight into it.
- **Theme.** Colors come from `KitTheme` / `PageTheme` (`selected`,
  `hovered`, `divider`, tones). Apps derive their palettes from it rather
  than hard-coding row colors.

## Verification

```sh
cargo test --no-default-features
cargo test --all-features
cargo clippy --all-targets --all-features
cargo build --no-default-features --all-targets
(cd swift && swift test)
(cd web && bun run check && bun test)
```
