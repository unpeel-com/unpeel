#!/usr/bin/env bash
# Build the panic-contained Rust static library linked into UnpeelNative.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
MODE="${1:-debug}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"

case "$MODE" in
  debug) ;;
  release) ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

if [ "$MODE" = "release" ]; then
  # build-app.sh already enables this for full app builds; the helper's guard
  # keeps this standalone release path deterministic without duplicating flags.
  . "$REPO_ROOT/scripts/rust-release-env.sh"
  unpeel_enable_rust_path_remapping "$REPO_ROOT"
fi

PROFILE_DIR="$MODE"
LINK_DIR="$REPO_ROOT/crates/target/native-bridge/$PROFILE_DIR"
BUILD_TARGET_DIR="$REPO_ROOT/crates/target/native-bridge-build"
mkdir -p "$LINK_DIR"

RUST_HEADER="$REPO_ROOT/crates/unpeel-native-bridge/include/unpeel_native_bridge.h"
SWIFT_HEADER="$REPO_ROOT/apps/native/UnpeelNative/Sources/CUnpeelNativeBridge/include/unpeel_native_bridge.h"
if ! cmp -s "$RUST_HEADER" "$SWIFT_HEADER"; then
  echo "error: native bridge C headers are out of sync" >&2
  exit 1
fi

build_bridge() {
  if [ "$MODE" = "release" ]; then
    cargo build \
      --manifest-path "$REPO_ROOT/crates/Cargo.toml" \
      --target-dir "$BUILD_TARGET_DIR" \
      -p unpeel-native-bridge \
      --release \
      --locked \
      "$@"
  else
    cargo build \
      --manifest-path "$REPO_ROOT/crates/Cargo.toml" \
      --target-dir "$BUILD_TARGET_DIR" \
      -p unpeel-native-bridge \
      "$@"
  fi
}

if [ "${UNPEEL_BRIDGE_UNIVERSAL:-0}" = "1" ]; then
  if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: universal native bridge builds require macOS and lipo" >&2
    exit 1
  fi
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    build_bridge --target "$target"
  done
  lipo -create \
    "$BUILD_TARGET_DIR/aarch64-apple-darwin/$PROFILE_DIR/libunpeel_native_bridge.a" \
    "$BUILD_TARGET_DIR/x86_64-apple-darwin/$PROFILE_DIR/libunpeel_native_bridge.a" \
    -output "$LINK_DIR/libunpeel_native_bridge.a"
else
  build_bridge
  cp \
    "$BUILD_TARGET_DIR/$PROFILE_DIR/libunpeel_native_bridge.a" \
    "$LINK_DIR/libunpeel_native_bridge.a"
fi

# SwiftPM does not track a static archive named only through `-L`/`-l` as a
# link-task input. Bump the C shim's mtime after replacing the archive so every
# sanctioned native build/test recompiles that tiny object and relinks against
# the exact Rust code just built.
touch "$REPO_ROOT/apps/native/UnpeelNative/Sources/CUnpeelNativeBridge/shim.c"

echo "Built $LINK_DIR/libunpeel_native_bridge.a"
