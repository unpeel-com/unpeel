#!/usr/bin/env bash
# dev-blank.sh — build, stably sign, and launch Unpeel.app against a BLANK,
# throwaway state dir so you can test first-run behavior (builtin presets
# seeded, superpowers on by default — there is no onboarding wizard anymore)
# without touching your real ~/.unpeel (sessions, projects, presets, license).
#
# How it works: the app and the hosted `unpeel-host` it spawns both honor the
# UNPEEL_HOME env var (LaunchConfig.unpeelDir / app_paths::unpeel_home). We
# point it at a fresh dir, so the app boots as if installed for the first
# time. UNPEEL_HOME deliberately does NOT start with
# UNPEEL_TEST_/UNPEEL_SNAPSHOT, which would arm the snapshot harnesses.
#
# We launch the executable directly (not `open`) so the env var is inherited —
# `open` does not forward arbitrary environment, and homeDirectoryForCurrentUser
# ignores $HOME, which is why a dedicated UNPEEL_HOME var is needed.
#
#   ./dev-blank.sh                 # fresh temp dir each run (auto-cleaned hint)
#   UNPEEL_HOME=~/unpeel-dev ./dev-blank.sh   # reuse a fixed dir across runs
#   RESET=1 UNPEEL_HOME=~/unpeel-dev ./dev-blank.sh   # wipe the fixed dir first
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Stable signing identity (same rationale as dev-app.sh): never ad-hoc, so the
# license keychain ACL keeps matching across rebuilds.
if [ -z "${CODESIGN_IDENTITY:-}" ]; then
  CODESIGN_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep "Apple Development" \
    | head -n1 \
    | sed -E 's/^[[:space:]]*[0-9]+\)[[:space:]]+[0-9A-F]+[[:space:]]+"(.*)"$/\1/')"
fi

if [ -z "$CODESIGN_IDENTITY" ] || [ "$CODESIGN_IDENTITY" = "-" ]; then
  echo "error: no stable code-signing identity found." >&2
  echo "       Set CODESIGN_IDENTITY to a local cert, e.g.:" >&2
  echo "       security find-identity -v -p codesigning" >&2
  exit 1
fi

# Resolve the blank state dir. Default: a fresh per-run temp dir.
if [ -z "${UNPEEL_HOME:-}" ]; then
  UNPEEL_HOME="$(mktemp -d "${TMPDIR:-/tmp}/unpeel-blank.XXXXXX")"
  echo "==> blank state dir (temp): $UNPEEL_HOME"
else
  # Expand a leading ~ for convenience.
  UNPEEL_HOME="${UNPEEL_HOME/#\~/$HOME}"
  if [ "${RESET:-0}" = "1" ] && [ -d "$UNPEEL_HOME" ]; then
    echo "==> RESET: wiping $UNPEEL_HOME"
    rm -rf "$UNPEEL_HOME"
  fi
  mkdir -p "$UNPEEL_HOME"
  echo "==> blank state dir: $UNPEEL_HOME"
fi
export UNPEEL_HOME

echo "==> dev build, signing with: $CODESIGN_IDENTITY"
UNPEEL_DEV_BUILD=1 CODESIGN_IDENTITY="$CODESIGN_IDENTITY" "$HERE/build-app.sh"

EXE="$HERE/dist/Unpeel.app/Contents/MacOS/UnpeelNative"
echo "==> launching $EXE (UNPEEL_HOME=$UNPEEL_HOME)"
echo "    quit the app to return to the shell; the app boots blank on first launch."
exec "$EXE"
