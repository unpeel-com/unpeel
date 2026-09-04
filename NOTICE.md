# Third-party notices

Unpeel is MIT-licensed (`LICENSE`); the Unpeel name, logo, icon, and mascot
are covered by `TRADEMARK.md`. This file lists the third-party software this
repository vendors, bundles, or fetches at runtime, and where each license
text lives. Rust dependency licenses are generated, not hand-maintained.

## Rust dependencies

`THIRD_PARTY_NOTICES.txt` at the repository root is the checked-in snapshot
of every license text in the resolved dependency graphs of the three CLI
crates (`unpeel-cli`, `unpeel-host`, `unpeel-attach`) across all release
targets (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`). It is produced by
`crates/license-notices` from `cargo metadata --locked` and shipped inside
every CLI archive at publish time. CI (`.github/workflows/notices-check.yml`)
regenerates it and fails when the snapshot differs; refresh it with:

```bash
cargo run --quiet --locked --manifest-path crates/Cargo.toml -p unpeel-license-notices -- \
  --manifest-path crates/Cargo.toml --package unpeel-cli --package unpeel-host \
  --manifest-path crates/unpeel-attach/Cargo.toml --package unpeel-attach \
  --target aarch64-apple-darwin --target x86_64-apple-darwin \
  --target x86_64-unknown-linux-gnu --target aarch64-unknown-linux-gnu \
  --output THIRD_PARTY_NOTICES.txt
```

## Vendored: libghostty-vt (MIT)

`crates/unpeel-core/vendor/ghostty-vt/` carries prebuilt static slices of
libghostty-vt, the standalone terminal-emulation library from
[Ghostty](https://github.com/ghostty-org/ghostty) (MIT; the license text is
`crates/unpeel-core/vendor/ghostty-vt/LICENSE`). Built from ghostty commit
`2da015cd6ac06cedc89e09756e895d2c1715205d` with two small Unpeel patches
applied at build time (`patches/0001-unpeel-small-pages.patch`,
`patches/0002-unpeel-pack-page-metadata.patch`; rationale and the rebuild
recipe in the directory's `README.md`). The Host uses it to parse PTY output
with the exact engine that renders terminals on desktop and phone.

## Fetched at runtime: agent-browser (Apache-2.0)

The Browser MCP drives a real browser through
[agent-browser](https://github.com/vercel-labs/agent-browser) (Apache-2.0)
in its native CDP mode. The engine is **not** vendored here: the Host
resolves it from the pinned version in `protocol/browser-engine-v1.json`
(installed under `~/.unpeel/browser/`) or from `UNPEEL_BROWSER_BIN`, and its
LICENSE travels with the engine package. The Node-based "full engine" mode is
never used or required.

## Not in this repository: Ghostty / GhosttyKit

The Mac app renders terminals with GhosttyKit (Ghostty, MIT). That framework,
its Unpeel patch, and its license live with the Apple client repository, not
here; this repository only consumes libghostty-vt as described above.

## Runtime marks

`runtimes/<slug>/assets/icon.svg` are the marks of third-party agent CLIs
(Claude Code, Codex, Gemini, and others), used only to identify the runtime
they belong to. They remain the property of their respective owners.
