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

`crates/unpeel-core/vendor/ghostty-vt/` carries libghostty-vt **1.0.0**
from [Ghostty](https://github.com/ghostty-org/ghostty), signed tag `vt-1.0.0`,
commit `0fa3d577a5614150ff8e2c152de5fb9d5c6d2dec`, with the small-page
Unpeel patch in `patches/0001-unpeel-small-pages.patch`. Its MIT license
is in `LICENSE` in that directory; `THIRD_PARTY_NOTICES.txt` there also
contains the bundled highway, simdutf, translate-c, uucode/Unicode, and
Wuffs licenses. The Rust notice generator includes those texts in CLI
archives and Mac bundles. Rebuild instructions are in the vendor README.
The Host uses this library for terminal emulation; the Apple renderer is a
separate GhosttyKit build.

## Fetched at runtime: agent-browser (Apache-2.0)

The Browser MCP drives a real browser through
[agent-browser](https://github.com/vercel-labs/agent-browser) (Apache-2.0)
in its native CDP mode. The engine is **not** vendored here: the Host
resolves it from the pinned version in `protocol/browser-engine-v1.json`
(installed under `~/.unpeel/browser/`) or from `UNPEEL_BROWSER_BIN`, and its
LICENSE travels with the engine package. The Node-based "full engine" mode is
never used or required.

## Apple renderer: Ghostty / GhosttyKit

The Apple apps render terminals with GhosttyKit (Ghostty, MIT). The patched
package, binary reference, and licenses live under
`clients/native/vendor/libghostty-spm/`. Its release cycle is independent
of the Host library above.

## Vendored: Termux terminal emulator (GPL-3.0-only)

`clients/android/termux/` is the terminal engine the Android Controller
draws with, compiled into the app module. It is termux-app **v0.118.3**,
commit `5b657c6adf4304e5198951ce815fe0205dcac29c`: the Java
`terminal-emulator` and `TerminalRenderer`, without `TerminalSession`,
JNI, or `TerminalView`.
Upstream terms are `LICENSE.md` and `COPYING` in that directory. The
emulator also contains Apache-2.0 code from jackpal/Android-Terminal-Emulator
(`Apache-2.0.txt`). Provenance and the omitted files are `UPSTREAM.txt`.

Unpeel itself stays MIT. An Android APK that links this module is a
GPL-3 combined work. The embed does not start a local shell; it only
displays the Host's terminal bytes. It is temporary until an MIT renderer
replaces it.

## Runtime marks

`runtimes/<slug>/assets/icon.svg` are the marks of third-party agent CLIs
(Claude Code, Codex, Gemini, and others), used only to identify the runtime
they belong to. They remain the property of their respective owners.
