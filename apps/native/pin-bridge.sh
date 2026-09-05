#!/usr/bin/env bash
# pin-bridge.sh — select how crates/unpeel-native-bridge finds unpeel-core and
# unpeel-serve, rewriting the two dependency lines in its Cargo.toml:
#
#   dev      (default) path deps into ../unpeel, or UNPEEL_SERVER_SOURCE
#   release  git deps pinned at tag v<SERVER_VERSION> of
#            https://github.com/unpeel-com/unpeel (build-app.sh release builds)
#
# build-rust-bridge.sh calls this before every cargo invocation, so the
# manifest always states the form that was actually built; build-app.sh
# asserts the pinned tag equals apps/native/SERVER_VERSION when the git form
# is active. Until the public tag exists, the git form can only be validated
# with `cargo metadata --no-deps` (cargo has to fetch a git source to resolve
# it, even when a [patch] would replace it) — release builds wait for the tag.
#
#   apps/native/pin-bridge.sh [dev|release]   # rewrite the two lines
#   apps/native/pin-bridge.sh --print         # show which form is active
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST="$REPO_ROOT/crates/unpeel-native-bridge/Cargo.toml"
SERVER_VERSION="$(tr -d '[:space:]' < "$REPO_ROOT/apps/native/SERVER_VERSION")"
GIT_URL="https://github.com/unpeel-com/unpeel"

MODE="${1:-dev}"
if [ "$MODE" = "--print" ]; then
  grep -E '^unpeel-(core|serve) = ' "$MANIFEST"
  exit 0
fi

case "$MODE" in
  dev)
    # Relative path deps keep the committed manifest clean in the usual
    # ~/Dev/unpeel-apple + ~/Dev/unpeel layout; UNPEEL_SERVER_SOURCE pins
    # another checkout (absolute path).
    if [ -n "${UNPEEL_SERVER_SOURCE:-}" ]; then
      SOURCE="$(cd "$UNPEEL_SERVER_SOURCE" && pwd)"
      [ -f "$SOURCE/crates/unpeel-core/Cargo.toml" ] || { echo "FAIL: $SOURCE is not a server checkout (no crates/unpeel-core)" >&2; exit 1; }
      prefix="$SOURCE/crates"
    else
      [ -f "$REPO_ROOT/../unpeel/crates/unpeel-core/Cargo.toml" ] || {
        echo "FAIL: no server checkout at ../unpeel; set UNPEEL_SERVER_SOURCE=<checkout> (or use 'release' mode)" >&2; exit 1; }
      prefix="../../../unpeel/crates"
    fi
    core_line="unpeel-core = { path = \"$prefix/unpeel-core\" }"
    serve_line="unpeel-serve = { path = \"$prefix/unpeel-serve\" }"
    mode="path ($prefix)"
    ;;
  release)
    core_line="unpeel-core = { git = \"$GIT_URL\", tag = \"v$SERVER_VERSION\" }"
    serve_line="unpeel-serve = { git = \"$GIT_URL\", tag = \"v$SERVER_VERSION\" }"
    mode="git tag v$SERVER_VERSION"
    ;;
  *) echo "usage: $0 [dev|release|--print]" >&2; exit 2 ;;
esac
python3 - "$MANIFEST" "$core_line" "$serve_line" <<'PY'
import re, sys
path, core, serve = sys.argv[1:]
s = open(path).read()
s, n1 = re.subn(r'^unpeel-core = \{[^\n]*\}', core, s, flags=re.M)
s, n2 = re.subn(r'^unpeel-serve = \{[^\n]*\}', serve, s, flags=re.M)
assert n1 == 1 and n2 == 1, "expected exactly one unpeel-core and one unpeel-serve dependency line"
open(path, "w").write(s)
PY
echo "bridge deps: $mode"
