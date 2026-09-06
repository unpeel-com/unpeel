#!/usr/bin/env bash
# Portable Controller core gate.
#
# `unpeel-core` must build without the Host: `--no-default-features
# --features controller-core` is what a Controller (the Swift bridge's tests,
# a wasm32 Controller) compiles. A Host-only module referenced from portable
# code only fails here, never in the workspace test run, so CI and the local
# pre-push checklist both run this script (the unit test in
# `crates/unpeel-core/src/portable_gating_tests.rs` catches the same class
# lexically). Both commands mirror `.github/workflows/linux-cli.yml`.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="$repo/crates/Cargo.toml"

if ! rustup target list --installed 2>/dev/null | grep -qx wasm32-unknown-unknown; then
  echo "check-portable-core: installing the wasm32-unknown-unknown target" >&2
  rustup target add wasm32-unknown-unknown
fi

echo "check-portable-core: cargo test (controller-core)"
cargo test --manifest-path "$manifest" -p unpeel-core \
  --no-default-features --features controller-core --locked "$@"

echo "check-portable-core: cargo clippy (controller-core, wasm32-unknown-unknown)"
cargo clippy --manifest-path "$manifest" -p unpeel-core \
  --target wasm32-unknown-unknown --all-targets \
  --no-default-features --features controller-core --locked -- -D warnings

echo "check-portable-core: OK"
