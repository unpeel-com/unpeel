#!/usr/bin/env bash
# dev-app.sh — build, stably sign, and launch Unpeel.app for local development.
#
# Why this exists: build-app.sh defaults to ad-hoc signing ("-") when no
# CODESIGN_IDENTITY is set. An ad-hoc signature's designated requirement is the
# binary's cdhash, which changes on every rebuild, so the macOS keychain ACL for
# com.unpeel.license never matches the new build and you get the
# "Unpeel wants to access key" password prompt after every rebuild — even after
# clicking "Always Allow", because that only trusts the old cdhash.
#
# Signing with a stable identity (your local "Apple Development" cert) anchors
# the designated requirement to the cert + Team ID, which is constant across
# rebuilds. After one final "Always Allow", the prompt stops for good.
#
# Override the identity with CODESIGN_IDENTITY=... if auto-detection picks wrong.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Pick a stable signing identity: explicit override, else the first local
# "Apple Development" cert. We deliberately avoid ad-hoc ("-") here.
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

echo "==> dev build, signing with: $CODESIGN_IDENTITY"
# Server binaries come from the published archive for apps/native/SERVER_VERSION
# (or UNPEEL_SERVER_ARCHIVE=<tar.gz>); there is no server source in this repo.
UNPEEL_DEV_BUILD=1 CODESIGN_IDENTITY="$CODESIGN_IDENTITY" "$HERE/build-app.sh"

APP="$HERE/dist/Unpeel.app"
EXE="$APP/Contents/MacOS/UnpeelNative"

# PIDs whose executable is this dist binary. Never matches
# /Applications/Unpeel.app — that process must stay running.
dist_unpeel_pids() {
  local exe="$1"
  ps -axo pid=,command= | awk -v exe="$exe" '
    {
      pid = $1
      sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "")
      if ($0 == exe || index($0, exe " ") == 1) print pid
    }
  '
}

wait_for_dist_exit() {
  local exe="$1"
  local deadline=$((SECONDS + $2))
  while [ -n "$(dist_unpeel_pids "$exe")" ] && [ "$SECONDS" -lt "$deadline" ]; do
    sleep 0.1
  done
}

# Replace the running Unpeel Dev after the new bundle is ready so the old
# instance stays usable during the compile. Quit by CFBundleName first (Cocoa
# termination), then signal leftover dist PIDs only — never `quit app "Unpeel"`
# or the shared bundle id, both of which would hit the installed app.
pids="$(dist_unpeel_pids "$EXE")"
if [ -n "$pids" ]; then
  echo "==> quitting running Unpeel Dev"
  osascript >/dev/null 2>&1 <<'APPLESCRIPT' || true
if application "Unpeel Dev" is running then
  tell application "Unpeel Dev" to quit
end if
APPLESCRIPT
  wait_for_dist_exit "$EXE" 5
  pids="$(dist_unpeel_pids "$EXE")"
  if [ -n "$pids" ]; then
    # Fallback for extra `open -n` instances / a release-flavored dist bundle
    # still named "Unpeel". xargs is BSD here: empty stdin does not run kill.
    echo "$pids" | xargs kill 2>/dev/null || true
    wait_for_dist_exit "$EXE" 2
  fi
  pids="$(dist_unpeel_pids "$EXE")"
  if [ -n "$pids" ]; then
    echo "$pids" | xargs kill -9 2>/dev/null || true
    sleep 0.1
  fi
  if [ -n "$(dist_unpeel_pids "$EXE")" ]; then
    echo "warning: Unpeel Dev still running from $EXE; launching another instance" >&2
  fi
fi

echo "==> launching $APP"
# Dev and release intentionally share a bundle id. Opening the bundle as a
# document lets Launch Services resolve that id back to /Applications. Pass the
# exact bundle as the application instead, and request a fresh instance.
open -n -a "$APP"
