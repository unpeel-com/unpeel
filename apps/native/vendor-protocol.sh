#!/usr/bin/env bash
# vendor-protocol.sh — pull the contracts this client repo pins from the
# server release named by apps/native/SERVER_VERSION:
#
#   protocol/*                       ← the CLI archive's protocol/ directory
#   apps/shared/.../GeneratedRuntimeCatalog.swift
#                                    ← the server tree's generated/ copy
#
# The archive comes from the same download/verify/cache path build-app.sh
# uses (UNPEEL_SERVER_ARCHIVE=<tar.gz> for a local one, cache under
# ~/Library/Caches/unpeel-apple/cli/<version>/). The runtime catalog is not
# in the archive: it is copied from a server checkout (UNPEEL_SERVER_SOURCE,
# default ../unpeel, at the pinned tag) when one is present, otherwise from
# `generated/` inside the archive if a future release ships it.
#
# protocol/ at this repo's root is gitignored — the Swift conformance tests
# (PaneLayoutOperationsConformanceTests, RemoteControlProtocolTests) locate
# protocol/<name>.json by walking up from their own file, so vendoring it at
# the root needs no test code change; UNPEEL_PROTOCOL_DIR overrides that.
#
#   apps/native/vendor-protocol.sh            # vendor
#   apps/native/vendor-protocol.sh --check    # verify the vendored copies match
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NATIVE_DIR="$REPO_ROOT/apps/native"
CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1
SERVER_VERSION="$(tr -d '[:space:]' < "$NATIVE_DIR/SERVER_VERSION")"
[ -n "$SERVER_VERSION" ] || { echo "FAIL: could not read apps/native/SERVER_VERSION" >&2; exit 1; }
sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }

# --- locate the server archive (same rules as build-app.sh) ----------------
ARCHIVE=""
if [ -n "${UNPEEL_SERVER_ARCHIVE:-}" ]; then
  ARCHIVE="$UNPEEL_SERVER_ARCHIVE"
  [ -s "$ARCHIVE" ] || { echo "FAIL: UNPEEL_SERVER_ARCHIVE not found: $ARCHIVE" >&2; exit 1; }
  if [ -s "$ARCHIVE.sha256" ]; then
    expected="$(awk 'NR == 1 { print $1 }' "$ARCHIVE.sha256")"
    [ "$(sha256_of "$ARCHIVE")" = "$expected" ] || { echo "FAIL: sha256 mismatch for $ARCHIVE" >&2; exit 1; }
  fi
else
  channel="${UNPEEL_SERVER_CHANNEL:-beta}"
  base="${UNPEEL_RELEASE_BASE_URL:-https://unpeel.com}"; base="${base%/}"
  name="unpeel-$SERVER_VERSION-macos-universal.tar.gz"
  cache="${UNPEEL_SERVER_ARCHIVE_CACHE:-$HOME/Library/Caches/unpeel-apple/cli/$SERVER_VERSION}"
  mkdir -p "$cache"
  if [ ! -s "$cache/$name" ] || [ ! -s "$cache/$name.sha256" ]; then
    echo "==> fetching $base/releases/$channel/cli/$name"
    curl -fsSL -o "$cache/$name.partial" "$base/releases/$channel/cli/$name" || {
      echo "FAIL: could not download the server archive for $SERVER_VERSION; set UNPEEL_SERVER_ARCHIVE=<tar.gz>" >&2; exit 1; }
    curl -fsSL -o "$cache/$name.sha256.partial" "$base/releases/$channel/cli/$name.sha256" || {
      echo "FAIL: could not download the sha256 sidecar" >&2; exit 1; }
    mv "$cache/$name.partial" "$cache/$name"; mv "$cache/$name.sha256.partial" "$cache/$name.sha256"
  fi
  expected="$(awk 'NR == 1 { print $1 }' "$cache/$name.sha256")"
  [ "$(sha256_of "$cache/$name")" = "$expected" ] || { echo "FAIL: cached archive sha256 mismatch" >&2; exit 1; }
  ARCHIVE="$cache/$name"
fi

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/unpeel-vendor-protocol.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
tar -xzf "$ARCHIVE" -C "$STAGE" protocol BUILD_PROVENANCE.json || {
  echo "FAIL: $ARCHIVE carries no protocol/ directory (archives ship it from 0.4.4)" >&2; exit 1; }
tar -xzf "$ARCHIVE" -C "$STAGE" generated 2>/dev/null || true
version="$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$STAGE/BUILD_PROVENANCE.json" | head -n1)"
[ "$version" = "$SERVER_VERSION" ] || { echo "FAIL: archive is server $version, SERVER_VERSION is $SERVER_VERSION" >&2; exit 1; }

# --- runtime catalog source --------------------------------------------------
CATALOG_SRC=""
SERVER_SOURCE="${UNPEEL_SERVER_SOURCE:-$REPO_ROOT/../unpeel}"
if [ -s "$STAGE/generated/GeneratedRuntimeCatalog.swift" ]; then
  CATALOG_SRC="$STAGE/generated/GeneratedRuntimeCatalog.swift"
elif [ -s "$SERVER_SOURCE/generated/GeneratedRuntimeCatalog.swift" ]; then
  CATALOG_SRC="$SERVER_SOURCE/generated/GeneratedRuntimeCatalog.swift"
fi
CATALOG_DST="$REPO_ROOT/apps/shared/UnpeelShared/Sources/UnpeelShared/GeneratedRuntimeCatalog.swift"

sync_tree() { # sync_tree <src dir> <dst dir> <label>
  local src="$1" dst="$2" label="$3"
  if [ "$CHECK" = 1 ]; then
    [ -d "$dst" ] || { echo "FAIL: $label is not vendored ($dst); run apps/native/vendor-protocol.sh" >&2; exit 1; }
    if ! diff -rq "$src" "$dst" >/dev/null; then
      echo "FAIL: vendored $label differs from server $SERVER_VERSION; run apps/native/vendor-protocol.sh" >&2
      diff -rq "$src" "$dst" >&2 || true; exit 1
    fi
    echo "    $label matches server $SERVER_VERSION"
  else
    rm -rf "$dst"; mkdir -p "$dst"; cp -R "$src"/. "$dst"/
    echo "    vendored $label → $dst ($(ls "$dst" | wc -l | tr -d ' ') files)"
  fi
}
echo "==> server $SERVER_VERSION from $ARCHIVE"
sync_tree "$STAGE/protocol" "$REPO_ROOT/protocol" "protocol/"
if [ -n "$CATALOG_SRC" ]; then
  if [ "$CHECK" = 1 ]; then
    cmp -s "$CATALOG_SRC" "$CATALOG_DST" || { echo "FAIL: GeneratedRuntimeCatalog.swift differs from $CATALOG_SRC; run apps/native/vendor-protocol.sh" >&2; exit 1; }
    echo "    GeneratedRuntimeCatalog.swift matches $CATALOG_SRC"
  else
    cp "$CATALOG_SRC" "$CATALOG_DST"; echo "    vendored GeneratedRuntimeCatalog.swift ← $CATALOG_SRC"
  fi
else
  echo "    note: no runtime catalog source (no generated/ in the archive and no server checkout at $SERVER_SOURCE); GeneratedRuntimeCatalog.swift left as committed"
fi
