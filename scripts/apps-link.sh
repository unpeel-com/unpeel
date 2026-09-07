#!/usr/bin/env bash
# Dev mode for the first-party Apps: build every App in crates/apps and point
# the Host's managed slot (~/.unpeel/apps/bin) at the build with
# `unpeel apps link`, so each rebuild is what the next launch runs. Pass a
# subset of slugs to build/link only those.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UNPEEL="${UNPEEL_BIN:-$ROOT/crates/target/release/unpeel}"
if [ ! -x "$UNPEEL" ]; then
  echo "==> building unpeel CLI"
  cargo build --release --manifest-path "$ROOT/crates/Cargo.toml" -p unpeel-cli
fi
slugs=("$@")
if [ ${#slugs[@]} -eq 0 ]; then
  slugs=(markdown filetree diffs usage)
fi
echo "==> building Apps: ${slugs[*]}"
args=()
for slug in "${slugs[@]}"; do args+=(-p "unpeel-$slug"); done
cargo build --release --manifest-path "$ROOT/crates/apps/Cargo.toml" "${args[@]}"
for slug in "${slugs[@]}"; do
  "$UNPEEL" apps link "unpeel.app.$slug" "$ROOT/crates/apps/target/release/unpeel-$slug"
done
"$UNPEEL" apps list
