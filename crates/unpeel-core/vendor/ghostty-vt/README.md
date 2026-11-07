# Vendored libghostty-vt 1.0.0

The Host's terminal-emulation engine is pinned to the signed upstream
[`vt-1.0.0`](https://github.com/ghostty-org/ghostty/tree/vt-1.0.0)
tag, commit `0fa3d577a5614150ff8e2c152de5fb9d5c6d2dec` (November 6,
2026). This is the stable C library described in the
[1.0 announcement](https://mitchellh.com/writing/libghostty-1-0).

`macos-universal/` contains arm64 and x86_64; `linux-aarch64/` and
`linux-x86_64/` contain the two headless Host slices. All three are built
together with Zig 0.17.0, ReleaseFast, baseline CPUs, stripped debug
information, SIMD enabled, and the small-page patch below. macOS targets
13.0; Linux targets GLIBC 2.31. SIMD dependencies are bundled in the static
archives and require only libc at runtime, without a C++ standard library.
`build.rs` selects the archive for the Rust target.

This release is **libghostty-vt**, not the Apple Metal renderer. The Mac
and iOS apps use the separately patched GhosttyKit build in
`clients/native/vendor/libghostty-spm/` (Ghostty commit `2da015cd6`).
The Host communicates terminal snapshots and replay as standard VT bytes;
the libraries do not share a C ABI or need matching source revisions.

## Rebuild

Requirements: macOS with Xcode command-line tools (`lipo`) and Python 3,
and Zig **0.17.x**. No Metal Toolchain is needed. The build fetches upstream
hash-pinned Zig dependencies into the checkout's package cache.

```sh
git clone --depth 1 --branch vt-1.0.0 https://github.com/ghostty-org/ghostty /tmp/ghostty-vt
ZIG=/path/to/zig-0.17.0/zig ./build.sh --ghostty /tmp/ghostty-vt
```

`GHOSTTY_SRC` can replace `--ghostty`. The script checks the exact source
commit and a clean checkout, applies the patch for the duration of the build, and reverses only
patches it applied on exit. All requested outputs are staged before
replacing the vendored archives. `SLICES=macos` rebuilds only the universal
Mac archive; a release update must rebuild all three.

`licenses.py` collects the linked dependencies' license texts from
upstream's `deps/libghostty-vt-third-party.json` and fetched packages into
`THIRD_PARTY_NOTICES.txt`. The uucode package omits the two files under its
`licenses/` directory; our copies in `licenses/uucode/` are from the same
pinned uucode commit, `1fb73433bba5d93366c57f23ff2e9d7939746500`.
`crates/license-notices` includes these notices whenever the selected Rust
graph contains `unpeel-core`, so CLI archives and Mac bundles carry them.

The hand-written C bindings in `src/ghostty_vt.rs` cover only the Host's
needs. When upgrading, audit signatures against `include/ghostty/vt/*.h`.
Tests check the actual library version and required features, enum values,
struct sizes/alignment, and style field offsets against
`ghostty_type_json()`. The 1.0 migration replaces formatter option structs
with `ghostty_formatter_set` and uses compact, fixed-layout style flags.
Snapshot replay tests cover full cell styles, modes, cursor, margins,
scrollback, and blank bottom rows.

## Unpeel source patch

`0001-unpeel-small-pages.patch` changes standard pages from 215×215 to
215×32 cells and grapheme storage from 8 KiB to 2 KiB. Upstream's minimum
of two pages otherwise raises a small scrollback budget to roughly 1 MiB.
The patch keeps the Host's 256 KiB budget effective without changing the C
ABI; pages that need more space still grow normally. It affects the Host
only, not GhosttyKit.

Historical measurements on an 80×24 Host terminal with that budget:

| | July `2da015cd` + old patches | October `6ba144158` unpatched | October `6ba144158` + small pages |
| --- | --- | --- | --- |
| Empty terminal | 61 KiB | 20 KiB | 20 KiB |
| Filled terminal (10k lines) | 286 KiB | 778 KiB | 225 KiB |
| Feed throughput | 357 MiB/s | 493 MiB/s | 735 MiB/s |

These are measurements of the earlier builds. On November 7, the patched
1.0.0 build measured 225 KiB per filled terminal with the Host's 256 KiB
budget and journal, and about 1,000 MiB/s feed throughput on macOS arm64.
Throughput and allocator residency vary by machine and run. Re-measure:

```sh
UNPEEL_HOME=$(mktemp -d /tmp/up-vt.XXXXXX) cargo test --manifest-path crates/Cargo.toml \
  --release -p unpeel-core --lib vt_footprint -- --ignored --nocapture
```
