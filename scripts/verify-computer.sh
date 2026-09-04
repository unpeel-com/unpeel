#!/usr/bin/env bash
#
# End-to-end verification of Computer Use on a Linux Host with the REAL
# engine: `unpeel serve` in an Xvfb + Openbox session (cua's canonical Linux
# baseline) supervising the pinned cua-driver, a hosted session driving a GTK
# fixture (zenity) through the unified MCP `computer` tool, and the Ask
# approval answered through the Host's approval route.
#
#   unpeel serve (Xvfb :96, Openbox, session bus, AT-SPI)
#     └─ cua-driver serve --embedded --socket <home>/computer/daemon.sock
#   unpeel-host __mcp__  ──computer see/set_value/click──▶  zenity --entry
#
# Verifies (scripts/verify-computer.py, which reuses the CLI matrix harness):
#   1. readiness  — the Host reports computerUseReady once the engine is up
#   2. gate       — a Session launched after readiness carries the tool
#   3. approval   — `see` under Ask blocks, is published to Controllers,
#                   and proceeds when answered through /mobile/approvals/answer
#   4. see        — a non-empty accessibility tree for the GTK fixture and a
#                   screenshot in the session gallery
#   5. act        — set_value/type into the entry, click OK by element index,
#                   and the fixture prints exactly the typed text
#   6. cleanup    — Remove ends the driver session (no leftover sockets)
#
# Engine resolution, in order (unpeel-apple:docs/plans/computer-use-release.md D3):
#   UNPEEL_CUA_DRIVER_BIN            an explicit engine binary
#   unpeel computer install          the pinned, hash-verified managed copy
#                                    (Lane A; absent on older trees)
# With neither, the script exits 0 with a SKIP line — set
# UNPEEL_VERIFY_COMPUTER_STRICT=1 to make that a failure (CI).
#
# Requires (Linux): Xvfb, openbox, zenity, dbus-launch (dbus-x11), at-spi2-core,
# python3. Builds `unpeel` + `unpeel-host` (debug) itself. Runs against a
# private UNPEEL_HOME (/tmp/uvc by default; short because of sockaddr_un) and
# never touches ~/.unpeel. Set DISPLAY to reuse a desktop session instead of
# starting Xvfb (the AT-SPI bus is then expected to be that session's).
#
# Usage: scripts/verify-computer.sh
#        UNPEEL_CUA_DRIVER_BIN=/path/to/cua-driver scripts/verify-computer.sh
#        UNPEEL_VERIFY_COMPUTER_STRICT=1 scripts/verify-computer.sh
# Exit:  0 = all checks pass (or SKIP); non-zero with a FAIL line otherwise.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATES="$REPO_ROOT/crates"
HOME_DIR="${UNPEEL_VERIFY_COMPUTER_HOME:-/tmp/uvc}"
DISPLAY_NUM="${UNPEEL_VERIFY_COMPUTER_DISPLAY:-:96}"
STRICT="${UNPEEL_VERIFY_COMPUTER_STRICT:-0}"

step() { printf '\n==> %s\n' "$1"; }
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }
skip() {
  if [ "$STRICT" = "1" ]; then fail "$1 (strict)"; fi
  printf 'SKIP: %s\n' "$1"
  exit 0
}

case "$(uname -s)" in
  Linux) ;;
  *) skip "Computer Use with a Host-owned engine is Linux-only here (macOS engine is app-owned)" ;;
esac

for tool in python3 Xvfb openbox zenity dbus-launch; do
  command -v "$tool" >/dev/null 2>&1 || skip "$tool is required (apt-get install xvfb openbox zenity dbus-x11 at-spi2-core)"
done

# --- binaries -----------------------------------------------------------------
step "Building unpeel + unpeel-host (debug)"
(cd "$CRATES" && cargo build -q --locked -p unpeel-cli -p unpeel-host)
TARGET_DIR="${CARGO_TARGET_DIR:-$CRATES/target}"
UNPEEL_BIN="$TARGET_DIR/debug/unpeel"
[ -x "$UNPEEL_BIN" ] || fail "no unpeel binary at $UNPEEL_BIN"
[ -x "$TARGET_DIR/debug/unpeel-host" ] || fail "no unpeel-host binary next to $UNPEEL_BIN"

# --- private home -------------------------------------------------------------
rm -rf "$HOME_DIR"
mkdir -p "$HOME_DIR"
export UNPEEL_HOME="$HOME_DIR"

# --- engine -------------------------------------------------------------------
step "Resolving the Computer Use engine"
ENGINE="${UNPEEL_CUA_DRIVER_BIN:-}"
if [ -n "$ENGINE" ]; then
  [ -x "$ENGINE" ] || fail "UNPEEL_CUA_DRIVER_BIN=$ENGINE is not executable"
  echo "engine: $ENGINE (UNPEEL_CUA_DRIVER_BIN)"
else
  set +e
  "$UNPEEL_BIN" computer install --check >/dev/null 2>&1
  probe=$?
  set -e
fi
if [ -z "$ENGINE" ] && { [ "$probe" -eq 0 ] || [ "$probe" -eq 3 ] || [ "$probe" -eq 4 ]; }; then
  # Lane A's verb exists (0 ready · 3 missing/stale · 4 no desktop yet, fine
  # before Xvfb is up): install the pinned, hash-verified copy into a
  # sibling engine home (the proof's own home is re-created by the harness).
  ENGINE_HOME="$HOME_DIR-engine"
  mkdir -p "$ENGINE_HOME"
  set +e
  # A GitHub download outage must not turn an unrelated CI push red: skip
  # with a warning on a network failure, but a pinned-hash mismatch or any
  # other installer error still fails (strict) — same shape as the archive
  # engine step in linux-cli.yml.
  set +e
  install_out=$(UNPEEL_HOME="$ENGINE_HOME" "$UNPEEL_BIN" computer install 2>&1)
  code=$?
  set -e
  printf '%s\n' "$install_out"
  if [ "$code" != 0 ] && [ "$code" != 4 ]; then
    if printf '%s' "$install_out" | grep -q "does not match the pinned sha256"; then
      fail "pinned cua-driver hash mismatch"
    fi
    if printf '%s' "$install_out" | grep -qiE "download|http|resolve|timed out"; then
      echo "::warning::cua-driver download unavailable (GitHub releases); verify-computer skipped — not a code failure"
      exit 0
    fi
  fi
  case "$code" in
    0|4) ;;
    *) fail "unpeel computer install exited $code" ;;
  esac
  ENGINE="$ENGINE_HOME/computer/bin/cua-driver"
  [ -x "$ENGINE" ] || fail "unpeel computer install left no engine at $ENGINE"
  echo "engine: $ENGINE (unpeel computer install)"
elif [ -z "$ENGINE" ]; then
  skip "no engine: set UNPEEL_CUA_DRIVER_BIN or use a tree with \`unpeel computer install\`"
fi
"$ENGINE" --version >/dev/null 2>&1 || fail "$ENGINE --version failed"
export UNPEEL_CUA_DRIVER_BIN="$ENGINE"

# --- desktop session ----------------------------------------------------------
cleanup() {
  set +e
  [ -n "${SERVE_PID:-}" ] && kill "$SERVE_PID" 2>/dev/null
  [ -n "${OPENBOX_PID:-}" ] && kill "$OPENBOX_PID" 2>/dev/null
  [ -n "${XVFB_PID:-}" ] && kill "$XVFB_PID" 2>/dev/null
  [ -n "${DBUS_SESSION_BUS_PID:-}" ] && kill "$DBUS_SESSION_BUS_PID" 2>/dev/null
}
trap cleanup EXIT INT TERM

if [ -z "${DISPLAY:-}" ]; then
  step "Starting Xvfb $DISPLAY_NUM + Openbox + a session bus"
  Xvfb "$DISPLAY_NUM" -screen 0 1280x800x24 -nolisten tcp >"$HOME_DIR/xvfb.log" 2>&1 &
  XVFB_PID=$!
  export DISPLAY="$DISPLAY_NUM"
  for _ in $(seq 1 50); do
    if xdpyinfo >/dev/null 2>&1 || [ -e "/tmp/.X11-unix/X${DISPLAY_NUM#:}" ]; then break; fi
    sleep 0.1
  done
  eval "$(dbus-launch --sh-syntax)"
  export DBUS_SESSION_BUS_ADDRESS
  # cua-driver reads the accessibility tree over AT-SPI; GTK apps only
  # export it when the toolkit thinks accessibility is on.
  export GTK_MODULES=gail:atk-bridge NO_AT_BRIDGE=0
  openbox >"$HOME_DIR/openbox.log" 2>&1 &
  OPENBOX_PID=$!
  sleep 0.5
else
  echo "using existing DISPLAY=$DISPLAY"
fi
echo "display: $DISPLAY  bus: ${DBUS_SESSION_BUS_ADDRESS:-<inherited>}"

# --- the proof (python, reusing the CLI matrix harness) -------------------------
step "Driving the Host, the engine, and the GTK fixture"
export UNPEEL_TUI_BINARY="$UNPEEL_BIN"
export UNPEEL_TUI_TEST_HOME="$HOME_DIR"
python3 "$REPO_ROOT/scripts/verify-computer.py" || fail "verify-computer.py reported failures"
pass "Computer Use end to end with $ENGINE on $DISPLAY"
