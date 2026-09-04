#!/usr/bin/env bash
#
# Headless end-to-end verification of the native terminal pipeline:
#
#   unpeel-host (PTY + output.bin + session.sock)
#     → unpeel-attach (replay + kqueue live follow + stdin relay)
#       → ht (libghostty-vt screen, montanaflynn/headless-terminal)
#
# ht renders unpeel-attach through the same VT engine a Ghostty surface
# uses, but with no Metal/GUI — so this runs where ghostty_surface_new
# cannot initialize (CI, agent sandboxes; see memory note from 2026-06-12).
#
# Verifies:
#   1. replay   — output produced BEFORE attach appears on the screen
#   2. echo     — keys typed through the attach client round-trip
#                 (ht → attach stdin → control socket → host PTY →
#                  output.bin → kqueue follow → attach stdout → VT)
#   3. reattach — a second attach replays content from the first epoch
#   4. snapshot — attaching while a TUI-style incremental repaint is in
#                 progress yields a client screen equal to the Host's own
#                 viewport (VT state snapshot, not journal replay); the raw
#                 tail control arm (UNPEEL_ATTACH_SNAPSHOT=0, 64-byte tail)
#                 must NOT match, proving the comparison discriminates
#
# Usage: scripts/verify-attach.sh
# Exit:  0 = all checks pass; non-zero with a FAIL line otherwise.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ATTACH_BIN="$REPO_ROOT/crates/unpeel-attach/target/debug/unpeel-attach"
HOST_BIN="$REPO_ROOT/crates/target/debug/unpeel-host"
# ht (headless-terminal) lives on PATH, under scripts/tools/ (gitignored, the
# server-repo home).
HT="$(command -v ht || true)"
[ -n "$HT" ] && [ -x "$HT" ] || HT="$REPO_ROOT/scripts/tools/ht"

# Short id: the session dir name feeds a unix socket path (SUN_LEN ≤ ~104).
SID="vfy-$(uuidgen | cut -c1-8 | tr '[:upper:]' '[:lower:]')"
# Honour UNPEEL_HOME so the check can run against an isolated home (for
# example one with a `unpeel-host __pty_core__` running, which then hosts
# this session through the same launch-file spawn).
SESSION_DIR="${UNPEEL_HOME:-$HOME/.unpeel}/app-sessions/$SID"
# `ht run` hands the command to an ht daemon whose environment is not ours,
# so forward the override explicitly to the attach client.
ATTACH_CMD=("$ATTACH_BIN")
[ -n "${UNPEEL_HOME:-}" ] && ATTACH_CMD=(/usr/bin/env "UNPEEL_HOME=$UNPEEL_HOME" "$ATTACH_BIN")
LAUNCH_FILE="$(mktemp -t unpeel-verify-launch)"
REPAINT_SCRIPT="$(mktemp -t unpeel-verify-repaint)"
TIMING_FILE="$(mktemp -t unpeel-verify-timing)"
HT_NAME_1="unpeel-verify-1-$$"
HT_NAME_2="unpeel-verify-2-$$"
HT_NAME_3="unpeel-verify-3-$$"
HT_NAME_4="unpeel-verify-4-$$"

cleanup() {
  "$HT" kill "$HT_NAME_1" >/dev/null 2>&1 || true
  "$HT" kill "$HT_NAME_2" >/dev/null 2>&1 || true
  "$HT" remove "$HT_NAME_1" >/dev/null 2>&1 || true
  "$HT" remove "$HT_NAME_2" >/dev/null 2>&1 || true
  for name in "$HT_NAME_3" "$HT_NAME_4"; do
    "$HT" kill "$name" >/dev/null 2>&1 || true
    "$HT" remove "$name" >/dev/null 2>&1 || true
  done
  if [ -S "$SESSION_DIR/session.sock" ]; then
    printf '{"type":"kill"}\n' | nc -U -w 2 "$SESSION_DIR/session.sock" >/dev/null 2>&1 || true
    sleep 0.5
  fi
  rm -rf "$SESSION_DIR" "$LAUNCH_FILE" "$REPAINT_SCRIPT" "$TIMING_FILE"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  echo "--- last screen ($HT_NAME_1):" >&2
  "$HT" view "$HT_NAME_1" 2>/dev/null | tail -8 >&2 || true
  exit 1
}

step() { echo "==> $*"; }

# --- 0. Prerequisites -------------------------------------------------------

[ -x "$HT" ] || fail "ht not found; install with: mkdir -p scripts/tools && cd scripts/tools && \
curl -sL https://github.com/montanaflynn/headless-terminal/releases/download/v0.3.0/ht-v0.3.0-darwin-arm64.tar.gz | tar xz"

if [ ! -x "$ATTACH_BIN" ]; then
  step "building unpeel-attach"
  (cd "$REPO_ROOT/crates/unpeel-attach" && cargo build --quiet)
fi
if [ ! -x "$HOST_BIN" ]; then
  step "building unpeel-host"
  (cd "$REPO_ROOT/crates" && cargo build --quiet --bin unpeel-host)
fi

# --- 1. Start a hosted session ----------------------------------------------

step "starting hosted session $SID"
cat > "$LAUNCH_FILE" <<EOF
{
  "session": {
    "id": "$SID",
    "project_id": "verify-attach",
    "label": "verify attach",
    "custom_title": false,
    "command": "",
    "created_at": $(date +%s)000,
    "tag_id": null,
    "worktree_path": null,
    "worktree_branch": null
  },
  "cwd": "/tmp",
  "dark_mode": true,
  "hook_port": null,
  "initial_cols": 100,
  "initial_rows": 30
}
EOF
"$HOST_BIN" "$LAUNCH_FILE" &

# Up to 15 s: a cold PTY core warms its App index on the first launch, and
# a login shell under a loaded Mac is slow to reach its prompt.
for _ in $(seq 1 150); do
  [ -S "$SESSION_DIR/session.sock" ] && break
  sleep 0.1
done
[ -S "$SESSION_DIR/session.sock" ] || fail "session host never created its control socket"

# --- 2. Replay: produce output BEFORE attaching ------------------------------

# The typed command renders as 'REPLAY_$((40+2))' on screen, so the marker
# 'REPLAY_42' can only come from real shell OUTPUT — no false match on the
# echoed command line.
#
# Wait for the shell to draw its prompt (output.bin non-empty) before typing:
# keystrokes that land while zsh is still initializing zle can be discarded,
# which used to make this step flaky under load.
step "seeding pre-attach output"
for _ in $(seq 1 300); do
  [ -s "$SESSION_DIR/output.bin" ] && break
  sleep 0.1
done
[ -s "$SESSION_DIR/output.bin" ] || fail "shell never produced a prompt"
printf '{"type":"write","data":"echo REPLAY_$((40+2))\\r"}\n' \
  | nc -U -w 2 "$SESSION_DIR/session.sock" >/dev/null \
  || fail "could not write to the session control socket"
sleep 1

step "attaching (ht session $HT_NAME_1)"
"$HT" run --name "$HT_NAME_1" --size 100x30 "${ATTACH_CMD[@]}" "$SID" >/dev/null
"$HT" wait "$HT_NAME_1" --text "REPLAY_42" \
  || fail "replayed output never appeared on the attached screen"
echo "    replay OK"

# --- 3. Live echo: type through the attach client ----------------------------

step "typing through the attach client"
"$HT" send "$HT_NAME_1" 'echo LIVE_$((50+5))<CR>' >/dev/null
"$HT" wait "$HT_NAME_1" --text "LIVE_55" \
  || fail "live round-trip output never appeared (input relay or kqueue follow broken)"
echo "    live echo OK"

# --- 4. Reattach: a fresh attach replays history ------------------------------

step "detaching and reattaching (ht session $HT_NAME_2)"
"$HT" kill "$HT_NAME_1" >/dev/null
"$HT" run --name "$HT_NAME_2" --size 100x30 "${ATTACH_CMD[@]}" "$SID" >/dev/null
"$HT" wait "$HT_NAME_2" --text "LIVE_55" \
  || { HT_NAME_1="$HT_NAME_2"; fail "reattach replay missing first-epoch output"; }
echo "    reattach replay OK"

# --- 5. Snapshot attach during an incremental repaint --------------------------

# A ratatui-style workload: clear once, hide the cursor, then paint one
# styled line every 80 ms with absolute cursor addressing. Nothing after the
# clear is a full-screen repaint, so a client that arrives mid-way can only
# show the right screen if it received the Host's resident VT state.
cat > "$REPAINT_SCRIPT" <<'REPAINT'
printf '\033[2J\033[?25l'
i=1
while [ "$i" -le 18 ]; do
  printf '\033[%d;3H\033[1;3%dmFRAME LINE %02d\033[0m  static text' "$i" $((i % 8)) "$i"
  sleep 0.08
  i=$((i + 1))
done
printf '\033[20;1H\033[?25h'
sleep 60
REPAINT

# The Host's own view of the screen (the same resident VT the MCP
# read_screen and the phone grid feed use), trailing blanks trimmed.
host_screen() {
  printf '{"type":"viewport_snapshot","cols":0,"rows":0}\n' \
    | nc -U -w 3 "$SESSION_DIR/session.sock" \
    | python3 -c '
import json, sys
reply = json.loads(sys.stdin.readline())
rows = [row["text"].rstrip() for row in reply["viewport"]["viewportRows"]]
while rows and not rows[-1]:
    rows.pop()
print("\n".join(rows))'
}

# The attached client screen as rendered by ht (libghostty-vt), same trim.
client_screen() {
  "$HT" view --json "$1" | python3 -c '
import json, sys
screen = json.load(sys.stdin)["screen"]
rows = [row.rstrip() for row in screen.split("\n")]
while rows and not rows[-1]:
    rows.pop()
print("\n".join(rows))'
}

step "starting an incremental repaint inside the session"
"$HT" kill "$HT_NAME_2" >/dev/null
# Foreground on purpose: a backgrounded job lets zsh redraw its prompt (and
# clear below it) on job notices/SIGWINCH, which would wipe painted rows on
# the Host as well and make the frame timing-dependent.
printf '{"type":"write","data":"clear; sh %s\\r"}\n' "$REPAINT_SCRIPT" \
  | nc -U -w 2 "$SESSION_DIR/session.sock" >/dev/null \
  || fail "could not start the repaint workload"
sleep 0.6   # ~6 of 18 lines painted: the frame is mid-repaint

step "snapshot attach mid-repaint (ht session $HT_NAME_3, 64-byte journal tail)"
"$HT" run --name "$HT_NAME_3" --size 100x30 \
  /usr/bin/env "UNPEEL_ATTACH_TIMING_FILE=$TIMING_FILE" ${UNPEEL_HOME:+"UNPEEL_HOME=$UNPEEL_HOME"} \
  "$ATTACH_BIN" --replay-bytes 64 "$SID" >/dev/null
"$HT" wait "$HT_NAME_3" --text "FRAME LINE 18" \
  || { HT_NAME_1="$HT_NAME_3"; fail "repaint never completed on the snapshot-attached client"; }
sleep 0.3   # let the final cursor park flush on both sides
HOST_VIEW="$(host_screen)"
CLIENT_VIEW="$(client_screen "$HT_NAME_3")"
if [ "$HOST_VIEW" != "$CLIENT_VIEW" ]; then
  echo "--- host screen:" >&2; echo "$HOST_VIEW" >&2
  echo "--- client screen:" >&2; echo "$CLIENT_VIEW" >&2
  HT_NAME_1="$HT_NAME_3"
  fail "snapshot-attached client screen differs from the Host's viewport"
fi
grep -q "FRAME LINE 01" <<<"$CLIENT_VIEW" \
  || { HT_NAME_1="$HT_NAME_3"; fail "lines painted before attach are missing (snapshot not applied?)"; }
echo "    snapshot attach screen == host viewport OK"

step "control: raw tail attach with the same 64-byte tail must NOT match"
"$HT" run --name "$HT_NAME_4" --size 100x30 \
  /usr/bin/env "UNPEEL_ATTACH_SNAPSHOT=0" "UNPEEL_ATTACH_TIMING_FILE=$TIMING_FILE" \
  ${UNPEEL_HOME:+"UNPEEL_HOME=$UNPEEL_HOME"} \
  "$ATTACH_BIN" --replay-bytes 64 "$SID" >/dev/null
sleep 1.5
CONTROL_VIEW="$(client_screen "$HT_NAME_4")"
if [ "$HOST_VIEW" = "$CONTROL_VIEW" ]; then
  HT_NAME_1="$HT_NAME_4"
  fail "raw 64-byte tail replay reproduced the full frame; the comparison is not discriminating"
fi
echo "    raw-tail control differs as expected OK"

echo "--- attach latency (request → last replay byte flushed):"
sed 's/^/    /' "$TIMING_FILE"

echo "PASS: replay, live echo, reattach, and snapshot attach all verified"
