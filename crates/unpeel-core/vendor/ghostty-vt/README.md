# Vendored libghostty-vt

Three slices, one per host platform — `macos-universal/` (fat arm64 +
x86_64), `linux-aarch64/`, and `linux-x86_64/`. `build.rs` picks the slice
from the target triple, so a headless Linux host parses PTY output with the
exact same VT engine as the Mac app and the phone. The Linux slices are
cross-compiled from macOS by zig (`build.sh` builds all three); they come
from the generic static path in ghostty's `build.zig`, not the xcframework,
and link against `libstdc++` rather than `libc++`.

`macos-universal/libghostty-vt.a` is the standalone terminal-emulation C
library from ghostty (https://libghostty.tip.ghostty.org), used by
`src/terminal_viewport.rs` so the host parses PTY output with the exact same
VT engine that renders it on desktop and phone (GhosttyKit). The archive is
a fat arm64 + x86_64 static lib with the vendored SIMD deps (simdutf,
highway) combined in, so it links with no extra dependencies beyond libc++.

Built from ghostty commit `2da015cd6ac06cedc89e09756e895d2c1715205d` (tip,
2026-07-06 — the VT-throughput optimization batch), the same commit the Mac
app's GhosttyKit.xcframework is built from (that checkout lives in the Apple
client repo's vendor tree, not here). Rebuild with:

```sh
git clone https://github.com/ghostty-org/ghostty /tmp/ghostty && \
  git -C /tmp/ghostty checkout 2da015cd6ac06cedc89e09756e895d2c1715205d
./build.sh --ghostty /tmp/ghostty     # from this directory (or GHOSTTY_SRC=…)
```

Requirements (same as the GhosttyKit tip build):

- zig 0.15.2 exactly: Homebrew `zig@0.15` (the ziglang.org 0.15.2 tarball
  cannot link on macOS 26).
- Xcode Metal Toolchain is NOT needed for lib-vt (no renderer).

The C API is declared by hand in `src/ghostty_vt.rs` (no bindgen). If you
bump the vendored archive, diff `include/ghostty/vt/*.h` in the ghostty
checkout against the declarations there — the `layout_matches_type_json`
test cross-checks struct sizes against `ghostty_type_json()` at runtime.

## Unpeel source patches (`patches/`)

`build.sh` applies every `patches/*.patch` to the checkout before `zig build`
and reverts them on exit (even on failure), so the checkout the app's
GhosttyKit build uses is never left modified. Today there is one:

- `0001-unpeel-small-pages.patch` — `page_preheat` 4 → 0 and `std_capacity`
  215×215 → 215×56 cells (a 128 KiB standard page instead of 512 KiB). Why:
  `PageList.minMaxSize` forces at least two standard pages, so upstream's
  page size silently raised every `max_scrollback` under 1 MiB to 1 MiB
  (the Host's filled-terminal floor), and the pool preheat touched one OS
  page per preheated slot per screen before any output (the empty-grid
  floor). Rationale, numbers, and the measurement recipe:
  `unpeel-apple:docs/plans/pty-core.md` "Round 3, Lane 3". The patched constants do not
  change any C API type, so `layout_matches_type_json` still applies.
- `0002-unpeel-pack-page-metadata.patch` — `Page.layout` puts the metadata
  regions (style set, grapheme/string allocators and maps, hyperlink set and
  map) FIRST and the cell array LAST; `availableBitsForGrid` uses the
  equivalent front-of-page formula. Why: page memory is zeroed mmap, so only
  what init writes becomes resident; init writes every metadata region and
  the row headers but never the cells. With the 55 KiB cell array in the
  middle those writes landed on separate 16 KiB macOS pages, so an empty
  terminal committed 81 KiB; packed, 61 KiB (`vt_footprint_per_terminal`,
  2026-09-03). Filled terminals are unchanged (they touch every page
  anyway). Pure offset reorder: consumers only read `Page.Layout`. This is
  the upstreamable candidate described in `unpeel-apple:docs/plans/pty-core.md`
  "Round 4, Lane 2". Note: the alternate screen is already created lazily
  upstream (`Terminal.switchScreen` → `ScreenSet.getInit`), so no lazy-alt
  patch exists.

Slice status: `macos-universal/` was rebuilt with 0001+0002 on 2026-09-03;
the Linux slices carry 0001 only until Lane 3's rebuild picks up 0002.

`GHOSTTY_SRC=/path` builds from another copy of the checkout (a git
worktree has no `References/` checkout of its own — `rsync` the main
tree's checkout somewhere short-lived and point at it). `SLICES=macos`
rebuilds only the universal macOS archive; the default rebuilds all three.
All three slices were last rebuilt together on 2026-09-03 from ghostty
`2da015cd6ac06cedc89e09756e895d2c1715205d` with
`0001-unpeel-small-pages.patch` applied (`build.sh`, default `SLICES`, zig
0.15.2 cross-building the Linux slices from macOS).
