#!/bin/bash
# Build all Host slices from the signed vt-1.0.0 source tag. See README.md.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PIN=0fa3d577a5614150ff8e2c152de5fb9d5c6d2dec
GHOSTTY_SRC="${GHOSTTY_SRC:-}"
while [ $# -gt 0 ]; do
    case "$1" in
        --ghostty) GHOSTTY_SRC="$2"; shift 2 ;;
        --ghostty=*) GHOSTTY_SRC="${1#--ghostty=}"; shift ;;
        *) echo 'usage: build.sh --ghostty PATH' >&2; exit 2 ;;
    esac
done
SLICES="${SLICES:-all}"
case "$SLICES" in all|macos) ;; *) echo 'SLICES must be all or macos' >&2; exit 2 ;; esac
if [ -z "$GHOSTTY_SRC" ] || [ "$(git -C "$GHOSTTY_SRC" rev-parse HEAD)" != "$PIN" ]; then
    echo "Expected a Ghostty checkout at vt-1.0.0 ($PIN)" >&2; exit 1
fi
if [ -n "$(git -C "$GHOSTTY_SRC" status --porcelain --untracked-files=normal)" ]; then
    echo 'Expected a clean Ghostty source checkout' >&2; exit 1
fi
ZIG="${ZIG:-zig}"
case "$("$ZIG" version)" in 0.17.*) ;; *) echo 'Zig 0.17.x is required' >&2; exit 1 ;; esac
ZIG="$(command -v "$ZIG")"
GHOSTTY_SRC="$(cd "$GHOSTTY_SRC" && pwd)"

# Only undo patches this invocation applied. Install the trap before the
# first mutation, so a later patch/build failure also restores the checkout.
APPLIED=()
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/unpeel-vt-build.XXXXXX")"
cleanup() {
    for ((i=${#APPLIED[@]}-1; i>=0; i--)); do
        git -C "$GHOSTTY_SRC" apply -R "${APPLIED[$i]}"
    done
    rm -rf "$STAGE"
}
trap cleanup EXIT
for patch in "$HERE"/patches/*.patch; do
    [ -f "$patch" ] || continue
    git -C "$GHOSTTY_SRC" apply --check "$patch"
    git -C "$GHOSTTY_SRC" apply "$patch"
    APPLIED+=("$patch")
done

build() {
    local target="$1" output="$2"
    echo "[*] libghostty-vt 1.0.0: $target"
    (cd "$GHOSTTY_SRC" && "$ZIG" build -Demit-lib-vt \
        -Doptimize=ReleaseFast -Dstrip=true -Dcpu=baseline \
        -Dlib-version-string=1.0.0 -Dtarget="$target" --prefix "$STAGE/$output")
}
build aarch64-macos.13.0 macos-arm64
build x86_64-macos.13.0 macos-x86_64
mkdir -p "$STAGE/macos-universal/lib"
lipo -create "$STAGE/macos-arm64/lib/libghostty-vt.a" \
    "$STAGE/macos-x86_64/lib/libghostty-vt.a" \
    -output "$STAGE/macos-universal/lib/libghostty-vt.a"
if [ "$SLICES" = all ]; then
    build aarch64-linux-gnu.2.31 linux-aarch64
    build x86_64-linux-gnu.2.31 linux-x86_64
fi
python3 "$HERE/licenses.py" "$GHOSTTY_SRC" "$STAGE/THIRD_PARTY_NOTICES.txt"

# Publish only once every requested build and license collection succeeded.
for slice in macos-universal linux-aarch64 linux-x86_64; do
    [ -f "$STAGE/$slice/lib/libghostty-vt.a" ] || continue
    mkdir -p "$HERE/$slice"
    cp "$STAGE/$slice/lib/libghostty-vt.a" "$HERE/$slice/libghostty-vt.a"
done
cp "$STAGE/THIRD_PARTY_NOTICES.txt" "$HERE/THIRD_PARTY_NOTICES.txt"
echo "[+] rebuilt $SLICES slices from $PIN with Unpeel's small-page patch"
