#!/bin/bash
# Rebuild the vendored libghostty-vt.a from a ghostty checkout at the commit
# recorded in README.md. See README.md for requirements.
#
#   ./build.sh --ghostty /path/to/ghostty      # or GHOSTTY_SRC=/path ./build.sh
#
# The checkout is NOT part of this repo (the Mac app's GhosttyKit build keeps
# its own under the Apple repo's vendor tree; use the same commit so Host and
# renderer parse identically).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GHOSTTY_SRC="${GHOSTTY_SRC:-}"
while [ $# -gt 0 ]; do
    case "$1" in
        --ghostty) GHOSTTY_SRC="$2"; shift 2 ;;
        --ghostty=*) GHOSTTY_SRC="${1#--ghostty=}"; shift ;;
        *) echo "[-] unknown argument: $1 (usage: build.sh [--ghostty PATH])" >&2; exit 2 ;;
    esac
done
# SLICES=macos builds only the universal macOS archive; default builds all
# three slices.
SLICES="${SLICES:-all}"

if [ -z "$GHOSTTY_SRC" ]; then
    echo "[-] no ghostty checkout given: pass --ghostty PATH or set GHOSTTY_SRC" >&2
    echo "    (clone https://github.com/ghostty-org/ghostty at the commit in README.md)" >&2
    exit 2
fi
if [ ! -d "$GHOSTTY_SRC" ] || [ ! -f "$GHOSTTY_SRC/build.zig" ]; then
    echo "[-] not a ghostty checkout: $GHOSTTY_SRC" >&2
    exit 1
fi

ZIG="${ZIG:-/opt/homebrew/opt/zig@0.15/bin/zig}"
if ! "$ZIG" version >/dev/null 2>&1; then
    echo "[-] zig not found at $ZIG (brew install zig@0.15)" >&2
    exit 1
fi

echo "[*] zig: $("$ZIG" version) building lib-vt from $GHOSTTY_SRC"

# Unpeel-specific source patches (patches/*.patch, applied in order) shape
# the Host-side VT — smaller standard pages, no page preheat — without
# touching the checkout the app's GhosttyKit build uses: they are applied
# for the duration of this script and reverted on exit, even on failure.
PATCHES=("$HERE"/patches/*.patch)
revert_patches() {
    for ((i=${#PATCHES[@]}-1; i>=0; i--)); do
        [ -f "${PATCHES[$i]}" ] || continue
        git -C "$GHOSTTY_SRC" apply -R "${PATCHES[$i]}" 2>/dev/null || true
    done
}
for patch in "${PATCHES[@]}"; do
    [ -f "$patch" ] || continue
    echo "[*] applying $(basename "$patch")"
    git -C "$GHOSTTY_SRC" apply --check "$patch"
    git -C "$GHOSTTY_SRC" apply "$patch"
done
trap revert_patches EXIT

# macOS universal (fat xcframework slice, needs xcodebuild).
(cd "$GHOSTTY_SRC" && PATH="$(dirname "$ZIG"):$PATH" zig build -Demit-lib-vt -Doptimize=ReleaseFast)
SRC_A="$GHOSTTY_SRC/zig-out/lib/ghostty-vt.xcframework/macos-arm64_x86_64/libghostty-vt.a"
mkdir -p "$HERE/macos-universal"
cp "$SRC_A" "$HERE/macos-universal/libghostty-vt.a"
lipo -info "$HERE/macos-universal/libghostty-vt.a"
echo "[+] vendored: $HERE/macos-universal/libghostty-vt.a"

if [ "$SLICES" = "macos" ]; then
    echo "[!] SLICES=macos: Linux slices not rebuilt"
    echo "[!] update the commit hash in README.md: $(git -C "$GHOSTTY_SRC" rev-parse HEAD)"
    exit 0
fi

# Linux slices for headless hosts (cross-compiled from macOS by zig; the
# generic static path in build.zig emits zig-out/lib/libghostty-vt.a).
for pair in "aarch64-linux-gnu:linux-aarch64" "x86_64-linux-gnu:linux-x86_64"; do
    target="${pair%%:*}"
    slice="${pair##*:}"
    echo "[*] building $target"
    (cd "$GHOSTTY_SRC" && PATH="$(dirname "$ZIG"):$PATH" \
        zig build -Demit-lib-vt -Doptimize=ReleaseFast -Dtarget="$target")
    mkdir -p "$HERE/$slice"
    cp "$GHOSTTY_SRC/zig-out/lib/libghostty-vt.a" "$HERE/$slice/libghostty-vt.a"
    echo "[+] vendored: $HERE/$slice/libghostty-vt.a"
done
echo "[!] update the commit hash in README.md: $(git -C "$GHOSTTY_SRC" rev-parse HEAD)"
