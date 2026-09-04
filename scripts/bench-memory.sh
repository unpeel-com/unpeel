#!/usr/bin/env bash
# Reproduce the four memory comparison-chart numbers (macOS phys_footprint)
# for Unpeel's session Host, plus a teardown leak check, on a blank,
# short-path UNPEEL_HOME using release binaries.
#
#   (a) serve + first session        serve tree + one `sh` session host
#   (b) per empty session            N more `sh` sessions, delta / N
#   (c) per filled 10k-line session  fill every session with 10k 72-col
#                                    numbered prose lines via `unpeel send`,
#                                    delta / N
#   (d) server-side per client       attach C pty clients (`unpeel-attach`
#                                    under a Python `pty.fork` helper) to one
#                                    filled session, host delta / C
#   (e) leftovers after `unpeel rm`  processes and bytes still around
#
# The script only ever touches processes and files under its own
# UNPEEL_HOME: every pid it signals was started by it and is matched by that
# home path. Nothing here looks at ~/.unpeel or /Applications.
#
# Usage:
#   scripts/bench-memory.sh                      # release binaries of this tree
#   UNPEEL_PTY_CORE=1 scripts/bench-memory.sh    # measure the shared PTY core
#   UNPEEL_BIN_DIR=/path/to/release scripts/bench-memory.sh
#   BENCH_SESSIONS=50 BENCH_CLIENTS=10 BENCH_LINES=10000 scripts/bench-memory.sh
#   BENCH_CLIENTS=0 skips the attached-client row (Linux: unpeel-attach is
#   kqueue-based and does not build there yet).
#   BENCH_KEEP_HOME=1 keeps the home for inspection (serve is still stopped).
#   BENCH_JSON=path also writes the raw numbers (KiB) as JSON.
#   BENCH_THRESHOLDS=scripts/bench-thresholds.json checks the rows against the
#   ceilings for BENCH_PROFILE (core_off | core_on | linux; inferred when
#   unset) and exits 1 on any violation — the CI gate.
#
# Platforms: macOS is the chart platform (phys_footprint via footprint(1)).
# Linux reports PSS from /proc/<pid>/smaps_rollup instead; the two are not
# comparable, so Linux rows get their own ceilings.
set -euo pipefail

OS="$(uname -s)"
case "$OS" in
  Darwin) command -v footprint >/dev/null || { echo "footprint(1) not found" >&2; exit 2; } ;;
  Linux) [[ -r /proc/self/smaps_rollup ]] || { echo "/proc/<pid>/smaps_rollup not readable" >&2; exit 2; } ;;
  *) echo "bench-memory.sh supports macOS and Linux (got $OS)" >&2; exit 2 ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${UNPEEL_BIN_DIR:-$REPO_ROOT/crates/target/release}"
UNPEEL="${UNPEEL_BIN:-$BIN_DIR/unpeel}"
UNPEEL_HOST="${UNPEEL_HOST_BIN:-$BIN_DIR/unpeel-host}"
ATTACH="${UNPEEL_ATTACH_BIN:-$REPO_ROOT/crates/unpeel-attach/target/release/unpeel-attach}"
SESSIONS="${BENCH_SESSIONS:-50}"
CLIENTS="${BENCH_CLIENTS:-10}"
LINES="${BENCH_LINES:-10000}"
# After the fill, how long to let the filled sessions sit quiet before
# measuring (c') "per filled session after going idle": busy buffers
# (journal batch, recent ring, client outboxes) must have been released.
IDLE_WAIT="${BENCH_IDLE_WAIT:-10}"
SETTLE="${BENCH_SETTLE_SECONDS:-3}"
# Reclamation grace: a host that is still exiting SETTLE seconds after rm on a
# loaded machine is a slow exit, not a leak; poll up to this long before
# calling anything a leftover, and report both numbers.
LEFTOVER_GRACE="${BENCH_LEFTOVER_GRACE_SECONDS:-20}"
PROFILE="${BENCH_PROFILE:-}"
if [[ -z "$PROFILE" ]]; then
  if [[ "$OS" == "Linux" ]]; then
    if [[ "${UNPEEL_PTY_CORE:-1}" != "0" ]]; then PROFILE=linux_core_on; else PROFILE=linux_core_off; fi
  elif [[ "${UNPEEL_PTY_CORE:-1}" != "0" ]]; then PROFILE=core_on
  else PROFILE=core_off
  fi
fi

for bin in "$UNPEEL" "$UNPEEL_HOST"; do
  [[ -x "$bin" ]] || { echo "missing release binary: $bin" >&2; exit 2; }
done
if (( CLIENTS > 0 )) && [[ ! -x "$ATTACH" ]]; then
  echo "missing release binary: $ATTACH (set BENCH_CLIENTS=0 to skip the client row)" >&2
  exit 2
fi

# Unix socket paths must stay short (sockaddr_un ~104 bytes), so the home is
# a short directory directly under $HOME, never under TMPDIR.
export UNPEEL_HOME="${BENCH_HOME:-$HOME/ubench-$$}"
# The worker installs the Browser MCP engine at start (a ~12 MB download):
# a benchmark must neither depend on the network nor measure that download
# in (a), so opt out — serve.json.browserEngine reads "disabled".
export UNPEEL_BROWSER_ENGINE_INSTALL="${UNPEEL_BROWSER_ENGINE_INSTALL:-0}"
# Same for the Computer Use engine (installed on demand once the experiment
# is on; a benchmark home never turns it on, but keep the gate explicit).
export UNPEEL_COMPUTER_ENGINE_INSTALL="${UNPEEL_COMPUTER_ENGINE_INSTALL:-0}"
if [[ -e "$UNPEEL_HOME" ]]; then
  echo "refusing to reuse existing home $UNPEEL_HOME" >&2
  exit 2
fi
mkdir -p "$UNPEEL_HOME"
chmod 700 "$UNPEEL_HOME"
# The session Host resolves its binary from PATH-independent config in the
# app, but the CLI spawns `unpeel-host` next to itself; make sure the pair
# under test is the one on PATH for anything that re-resolves by name.
export PATH="$BIN_DIR:$PATH"

SERVE_PID=""
CLIENT_HELPER_PID=""
declare -a SESSION_IDS=()

# ---------- helpers ---------------------------------------------------------

log() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" >&2; }

# Resident memory of one pid in KiB (0 when the process is gone): macOS
# phys_footprint, Linux PSS (proportional set size, so shared pages are not
# double-counted across the 50 hosts).
footprint_kib() {
  local pid="$1" line
  if [[ "$OS" == "Linux" ]]; then
    awk '/^Pss:/ { print $2; found=1 } END { if (!found) print 0 }' "/proc/$pid/smaps_rollup" 2>/dev/null || echo 0
    return
  fi
  # Exact bytes: the formatted output rounds a 100 MB process to 0.1 MB,
  # which is coarser than a whole attached client.
  line="$(footprint -f bytes "$pid" 2>/dev/null | grep -m1 'phys_footprint:' || true)"
  [[ -n "$line" ]] || { echo 0; return; }
  awk '{ printf "%d\n", $2 / 1024 }' <<<"$line"
}

thread_count() {
  local pid="$1"
  if [[ "$OS" == "Linux" ]]; then
    ls "/proc/$pid/task" 2>/dev/null | wc -l | tr -d ' '
  else
    ps -M -p "$pid" 2>/dev/null | tail -n +2 | wc -l | tr -d ' '
  fi
}

METRIC_NAME="phys_footprint via \`footprint(1)\`"
[[ "$OS" == "Linux" ]] && METRIC_NAME="PSS via \`/proc/<pid>/smaps_rollup\`"

# pid plus every descendant (our own serve tree, our own client helper).
descendants() {
  local pid="$1" child core
  core="$(core_pid)"
  [[ -n "$core" && "$pid" == "$core" ]] && return
  echo "$pid"
  for child in $(pgrep -P "$pid" 2>/dev/null || true); do
    descendants "$child"
  done
}

tree_footprint_kib() {
  local total=0 pid
  for pid in $(descendants "$1"); do
    total=$(( total + $(footprint_kib "$pid") ))
  done
  echo "$total"
}

# Host process pids for sessions of THIS home only: the launch-file argv
# lives under $UNPEEL_HOME/app-sessions/<id>/.
session_host_pids() {
  pgrep -f "__session_host__ $UNPEEL_HOME/app-sessions/" 2>/dev/null || true
}

# The shared PTY core of THIS home (UNPEEL_PTY_CORE=1), if one is alive. It
# carries no home in its argv, so it is found through its record file.
core_pid() {
  local pid
  pid="$(python3 -c "import json,sys;print(json.load(open(sys.argv[1])).get('pid') or '')" "$UNPEEL_HOME/pty-core.json" 2>/dev/null || true)"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null && echo "$pid" || true
}

# Everything hosting sessions of THIS home: per-process hosts plus the core.
host_pids() {
  session_host_pids
  core_pid
}

host_pid_for() {
  local pid
  pid="$(pgrep -f "__session_host__ $UNPEEL_HOME/app-sessions/$1/" 2>/dev/null | head -1 || true)"
  [[ -n "$pid" ]] && { echo "$pid"; return; }
  core_pid
}

hosts_footprint_kib() {
  local total=0 pid
  for pid in $(host_pids); do
    total=$(( total + $(footprint_kib "$pid") ))
  done
  echo "$total"
}

kib_to_mib() { awk -v k="$1" 'BEGIN { printf "%.2f MiB", k/1024 }'; }
kib_to_kib() { awk -v k="$1" 'BEGIN { printf "%d KiB", k }'; }

# `unpeel new` waits 10 s for the host to publish a running manifest. On a
# loaded machine (parallel builds, other agents) a login-shell start can miss
# that even though the host comes up fine a moment later, so take the id from
# either the JSON or the timeout message and wait for the manifest ourselves.
new_sh_session() {
  local attempt out id
  for attempt in 1 2 3; do
    out="$("$UNPEEL" new --command sh --cwd "$UNPEEL_HOME" --json 2>&1 || true)"
    id="$(grep -oE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' <<<"$out" | head -1 || true)"
    if [[ -n "$id" ]] && wait_for_running "$id" 90; then
      echo "$id"
      return 0
    fi
    log "unpeel new failed (attempt $attempt): ${out//$'\n'/ }"
    [[ -n "$id" ]] && "$UNPEEL" rm "$id" >/dev/null 2>&1
    sleep 2
  done
  echo "could not create a session after 3 attempts" >&2
  return 1
}

wait_for_running() {
  local id="$1" deadline=$(( $(date +%s) + ${2:-90} )) manifest="$UNPEEL_HOME/app-sessions/$1/manifest.json"
  while (( $(date +%s) < deadline )); do
    grep -q '"state": *"running"' "$manifest" 2>/dev/null && return 0
    sleep 0.25
  done
  return 1
}

wait_for_text() {
  "$UNPEEL" wait "$1" --text "$2" --timeout "${3:-120}" >/dev/null
}

cleanup() {
  set +e
  if [[ -n "$CLIENT_HELPER_PID" ]] && kill -0 "$CLIENT_HELPER_PID" 2>/dev/null; then
    kill -TERM "$CLIENT_HELPER_PID" 2>/dev/null
    wait "$CLIENT_HELPER_PID" 2>/dev/null
  fi
  for id in "${SESSION_IDS[@]:-}"; do
    [[ -n "$id" ]] && "$UNPEEL" rm "$id" >/dev/null 2>&1
  done
  # Anything still hosting under OUR home after rm is ours to stop. The
  # core first gets the contract's shutdown request (refused while busy).
  if [[ -S "$UNPEEL_HOME/pty-core.sock" ]]; then
    python3 - "$UNPEEL_HOME/pty-core.sock" <<'PY' 2>/dev/null || true
import socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(3); s.connect(sys.argv[1])
s.sendall(b'{"op":"shutdown"}\n'); s.recv(4096)
PY
    sleep 0.5
  fi
  for pid in $(host_pids); do kill -TERM "$pid" 2>/dev/null; done
  if [[ -n "$SERVE_PID" ]] && kill -0 "$SERVE_PID" 2>/dev/null; then
    kill -TERM "$SERVE_PID" 2>/dev/null
    wait "$SERVE_PID" 2>/dev/null
  fi
  if [[ "${BENCH_KEEP_HOME:-0}" != "1" ]]; then
    rm -rf "$UNPEEL_HOME"
  else
    log "kept $UNPEEL_HOME"
  fi
}
trap cleanup EXIT INT TERM

# ---------- (a) serve + first session ----------------------------------------

log "home $UNPEEL_HOME; binaries $BIN_DIR; attach $ATTACH"
"$UNPEEL" serve >"$UNPEEL_HOME/serve.log" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 100); do
  [[ -f "$UNPEEL_HOME/serve.json" ]] && break
  kill -0 "$SERVE_PID" 2>/dev/null || { echo "serve exited early:"; cat "$UNPEEL_HOME/serve.log"; exit 1; } >&2
  sleep 0.1
done
sleep "$SETTLE"
SERVE_KIB=$(tree_footprint_kib "$SERVE_PID")
log "serve tree: $(kib_to_mib "$SERVE_KIB")"

FIRST_ID=$(new_sh_session)
SESSION_IDS+=("$FIRST_ID")
FIRST_HOST_PID=$(host_pid_for "$FIRST_ID")
[[ -n "$FIRST_HOST_PID" ]] || { echo "no host process for $FIRST_ID" >&2; exit 1; }
wait_for_text "$FIRST_ID" '$' 30 || true
sleep "$SETTLE"
FIRST_HOST_KIB=$(footprint_kib "$FIRST_HOST_PID")
FIRST_HOST_THREADS=$(thread_count "$FIRST_HOST_PID")
SERVE_AFTER_FIRST_KIB=$(tree_footprint_kib "$SERVE_PID")
A_KIB=$(( SERVE_AFTER_FIRST_KIB + FIRST_HOST_KIB ))
log "first session host: $(kib_to_mib "$FIRST_HOST_KIB") ($FIRST_HOST_THREADS threads); serve now $(kib_to_mib "$SERVE_AFTER_FIRST_KIB")"

# ---------- (b) per empty session -------------------------------------------

BEFORE_EMPTY_KIB=$(hosts_footprint_kib)
log "spawning $SESSIONS empty sh sessions"
for _ in $(seq 1 "$SESSIONS"); do
  SESSION_IDS+=("$(new_sh_session)")
done
sleep "$SETTLE"
AFTER_EMPTY_KIB=$(hosts_footprint_kib)
B_KIB=$(( (AFTER_EMPTY_KIB - BEFORE_EMPTY_KIB) / SESSIONS ))
log "per empty session: $(kib_to_mib "$B_KIB")"

# ---------- (c) per filled 10k-line session ---------------------------------

# 72 columns: "00001 " + 66 chars of prose. Generated inside the session's
# own sh so one `unpeel send` fills a terminal; the sentinel line lets us wait
# for completion through the Host's own viewport.
PROSE="the quick brown fox jumps over the lazy dog while the agent keeps on"
PROSE="${PROSE:0:66}"
FILL_CMD="i=0; while [ \$i -lt $LINES ]; do i=\$((i+1)); printf '%05d %s\\n' \$i '$PROSE'; done; echo BENCH-FILL-DONE"
FILL_IDS=("${SESSION_IDS[@]:1}")
BEFORE_FILL_KIB=$(hosts_footprint_kib)
log "filling ${#FILL_IDS[@]} sessions with $LINES lines each"
for id in "${FILL_IDS[@]}"; do
  "$UNPEEL" send "$id" "$FILL_CMD" --enter >/dev/null
done
for id in "${FILL_IDS[@]}"; do
  wait_for_text "$id" BENCH-FILL-DONE 300 || { echo "session $id never finished filling" >&2; exit 1; }
done
sleep "$SETTLE"
AFTER_FILL_KIB=$(hosts_footprint_kib)
C_KIB=$(( (AFTER_FILL_KIB - BEFORE_FILL_KIB) / ${#FILL_IDS[@]} ))
FILLED_TOTAL_KIB=$AFTER_FILL_KIB
log "per filled session: $(kib_to_mib "$C_KIB"); all hosts now $(kib_to_mib "$FILLED_TOTAL_KIB")"
# (c') the same sessions after IDLE_WAIT seconds of silence: what a busy
# terminal keeps once it goes quiet (the VT grid and scrollback stay; the
# I/O buffers used while it printed should not).
sleep "$IDLE_WAIT"
AFTER_IDLE_KIB=$(hosts_footprint_kib)
C_IDLE_KIB=$(( (AFTER_IDLE_KIB - BEFORE_FILL_KIB) / ${#FILL_IDS[@]} ))
log "per filled session after ${IDLE_WAIT}s idle: $(kib_to_mib "$C_IDLE_KIB"); all hosts now $(kib_to_mib "$AFTER_IDLE_KIB")"

# ---------- (d) server-side per attached client ------------------------------

D_KIB=0
ATTACHED=0
if (( CLIENTS > 0 )); then
TARGET_ID="${FILL_IDS[0]}"
TARGET_HOST_PID=$(host_pid_for "$TARGET_ID")
BEFORE_CLIENTS_KIB=$(footprint_kib "$TARGET_HOST_PID")
HELPER="$UNPEEL_HOME/attach-clients.py"
cat >"$HELPER" <<'PY'
import fcntl, os, pty, select, signal, struct, sys, termios

count, attach, session_id = int(sys.argv[1]), sys.argv[2], sys.argv[3]
children = []
for _ in range(count):
    pid, fd = pty.fork()
    if pid == 0:
        # 120x32 like `unpeel new`'s default grid, so attaching never resizes
        # the hosted PTY and the measurement is the client bookkeeping alone.
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        os.execv(attach, [attach, session_id])
    children.append((pid, fd))

def stop(*_):
    for pid, _ in children:
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError:
            pass
    sys.exit(0)

signal.signal(signal.SIGTERM, stop)
print("READY", flush=True)
fds = [fd for _, fd in children]
while fds:
    ready, _, _ = select.select(fds, [], [], 1.0)
    for fd in ready:
        try:
            if not os.read(fd, 65536):
                fds.remove(fd)
        except OSError:
            fds.remove(fd)
PY
log "attaching $CLIENTS pty clients to $TARGET_ID"
python3 "$HELPER" "$CLIENTS" "$ATTACH" "$TARGET_ID" >"$UNPEEL_HOME/clients.log" 2>&1 &
CLIENT_HELPER_PID=$!
for _ in $(seq 1 100); do
  grep -q READY "$UNPEEL_HOME/clients.log" 2>/dev/null && break
  sleep 0.1
done
sleep "$SETTLE"
ATTACHED=$(pgrep -P "$CLIENT_HELPER_PID" | wc -l | tr -d ' ')
AFTER_CLIENTS_KIB=$(footprint_kib "$TARGET_HOST_PID")
D_KIB=$(( (AFTER_CLIENTS_KIB - BEFORE_CLIENTS_KIB) / CLIENTS ))
log "server-side per client: $(kib_to_kib "$D_KIB") ($ATTACHED clients live)"
kill -TERM "$CLIENT_HELPER_PID" 2>/dev/null || true
wait "$CLIENT_HELPER_PID" 2>/dev/null || true
CLIENT_HELPER_PID=""
else
  log "skipping the attached-client row (BENCH_CLIENTS=0)"
fi

# ---------- (e) leftovers after unpeel rm -----------------------------------

log "removing ${#SESSION_IDS[@]} sessions"
# Reclamation is the number that matters most, so make it diagnosable: record
# every rm result, retry the failures once after a short wait (an rm can lose
# a race with a host still starting up on a loaded machine), then list every
# leftover id with its manifest state and whether a host process survives.
RM_FAILED=()
RM_RETRIED=0
for id in "${SESSION_IDS[@]}"; do
  if ! "$UNPEEL" rm "$id" >>"$UNPEEL_HOME/rm.log" 2>&1; then
    RM_FAILED+=("$id")
    echo "rm failed: $id" >>"$UNPEEL_HOME/rm.log"
  fi
done
if (( ${#RM_FAILED[@]} > 0 )); then
  log "${#RM_FAILED[@]} rm calls failed; retrying once after ${SETTLE}s"
  sleep "$SETTLE"
  for id in "${RM_FAILED[@]}"; do
    RM_RETRIED=$(( RM_RETRIED + 1 ))
    "$UNPEEL" rm "$id" >>"$UNPEEL_HOME/rm.log" 2>&1 || echo "rm retry failed: $id" >>"$UNPEEL_HOME/rm.log"
  done
fi
SESSION_IDS=()
sleep "$SETTLE"
SLOW_EXITS=$(session_host_pids | wc -l | tr -d ' ')
if (( SLOW_EXITS > 0 )); then
  log "$SLOW_EXITS hosts still exiting ${SETTLE}s after rm; waiting up to ${LEFTOVER_GRACE}s"
  for _ in $(seq 1 $(( LEFTOVER_GRACE * 2 ))); do
    [[ -z "$(session_host_pids)" ]] && break
    sleep 0.5
  done
fi
LEFT_PIDS=$(session_host_pids | wc -l | tr -d ' ')
LEFT_KIB=0
LEFTOVER_HOST_DETAIL=""
for pid in $(session_host_pids); do
  LEFT_KIB=$(( LEFT_KIB + $(footprint_kib "$pid") ))
  LEFTOVER_HOST_DETAIL+="  - host pid $pid: $(ps -o etime=,args= -p "$pid" 2>/dev/null | sed 's/  */ /g' | cut -c1-160)"$'\n'
  # What is it waiting on? Children still alive and the host's own stacks.
  {
    echo "leftover host $pid process tree:"
    ps -o pid=,ppid=,stat=,etime=,command= -p "$pid" $(pgrep -P "$pid" 2>/dev/null) 2>/dev/null | cut -c1-140
    if [[ "$OS" == "Darwin" ]]; then
      sample "$pid" 1 2>/dev/null | sed -n '/Call graph/,/Total number/p' | head -40
    fi
  } >>"$UNPEEL_HOME/leftovers.log" 2>&1
done
[[ -s "$UNPEEL_HOME/leftovers.log" ]] && cat "$UNPEEL_HOME/leftovers.log" >&2
CORE_END_PID="$(core_pid)"
CORE_END_KIB=0
CORE_END_INSTANT_KIB=0
CORE_END_THREADS=0
if [[ -n "$CORE_END_PID" ]]; then
  # The core releases freed heap on its idle tick after a teardown and the
  # allocator returns pages gradually, so sample until the footprint stops
  # falling (up to BENCH_CORE_SETTLE_SECONDS) and report both the instant
  # and the settled value; the ceiling applies to the settled one.
  CORE_END_INSTANT_KIB=$(footprint_kib "$CORE_END_PID")
  CORE_END_KIB=$CORE_END_INSTANT_KIB
  core_settle_deadline=$(( $(date +%s) + ${BENCH_CORE_SETTLE_SECONDS:-30} ))
  core_stable=0
  while (( $(date +%s) < core_settle_deadline )); do
    sleep 2
    sample=$(footprint_kib "$CORE_END_PID")
    if (( sample < CORE_END_KIB )); then CORE_END_KIB=$sample; core_stable=0; else core_stable=$(( core_stable + 1 )); fi
    (( core_stable >= 3 )) && break
  done
  CORE_END_THREADS=$(thread_count "$CORE_END_PID")
  log "PTY core after teardown: $(kib_to_mib "$CORE_END_INSTANT_KIB") at the instant, $(kib_to_mib "$CORE_END_KIB") settled (pid $CORE_END_PID, $CORE_END_THREADS threads)"
  if [[ "${BENCH_VMMAP:-0}" == "1" && "$OS" == "Darwin" ]]; then
    log "vmmap of the idle core:"
    vmmap -summary "$CORE_END_PID" 2>/dev/null | grep -E "^(MALLOC|Stack|VM_ALLOCATE|TOTAL|__DATA|__LINKEDIT|__TEXT)" >&2 || true
  fi
fi
LEFT_DIRS=$(find "$UNPEEL_HOME/app-sessions" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' ')
SERVE_END_KIB=$(tree_footprint_kib "$SERVE_PID")
LEFTOVER_DETAIL=""
for dir in "$UNPEEL_HOME"/app-sessions/*/; do
  [[ -d "$dir" ]] || continue
  id="$(basename "$dir")"
  state="$(grep -oE '"state": *"[a-z]+"' "$dir/manifest.json" 2>/dev/null | grep -oE '[a-z]+"$' | tr -d '"' || true)"
  hostpid="$(host_pid_for "$id")"
  LEFTOVER_DETAIL+="  - $id manifest=${state:-missing} host_pid=${hostpid:-none}"$'\n'
done
LEFTOVER_DETAIL+="$LEFTOVER_HOST_DETAIL"
if [[ -n "$LEFTOVER_DETAIL" ]]; then
  log "leftover sessions/hosts:"
  printf '%s' "$LEFTOVER_DETAIL" >&2
  grep -E 'failed' "$UNPEEL_HOME/rm.log" >&2 || true
fi
log "leftover hosts: $LEFT_PIDS ($(kib_to_mib "$LEFT_KIB")), session dirs: $LEFT_DIRS, slow exits: $SLOW_EXITS, rm failures: ${#RM_FAILED[@]} (retried $RM_RETRIED); serve now $(kib_to_mib "$SERVE_END_KIB")"

# ---------- (g)/(h) the VT grid itself --------------------------------------

# In-process cost of one libghostty-vt grid at 80x24 (empty, and after 10k
# lines), from the ignored release test in terminal_viewport.rs. Needs the
# source tree and cargo (BENCH_VT=1; CI sets it); the release binaries alone
# cannot answer this.
VT_EMPTY_KIB=""
VT_FILLED_KIB=""
VT_FEED_MIB_S=""
if [[ "${BENCH_VT:-0}" == "1" ]]; then
  log "measuring the VT grid (cargo test --release vt_footprint)"
  vt_line="$(cd "$REPO_ROOT/crates" && cargo test --release -q -p unpeel-core --lib vt_footprint -- --ignored --nocapture 2>/dev/null | grep -m1 '^VT_ROW ' || true)"
  if [[ -n "$vt_line" ]]; then
    VT_EMPTY_KIB="$(sed -E 's/.*empty_kib=([0-9]+).*/\1/' <<<"$vt_line")"
    VT_FILLED_KIB="$(sed -E 's/.*filled_kib=([0-9]+).*/\1/' <<<"$vt_line")"
    VT_FEED_MIB_S="$(sed -E 's/.*feed_mib_s=([0-9.]+).*/\1/' <<<"$vt_line")"
    log "VT grid: empty $(kib_to_kib "$VT_EMPTY_KIB"), filled $(kib_to_kib "$VT_FILLED_KIB"), feed $VT_FEED_MIB_S MiB/s"
  else
    log "VT grid measurement produced no VT_ROW line"
  fi
fi

# ---------- report -----------------------------------------------------------

VERSION=$("$UNPEEL" --version 2>/dev/null | head -1)
cat <<EOF

### Unpeel memory benchmark — $VERSION ($(date +%Y-%m-%d))

$METRIC_NAME, blank home, release binaries from \`$BIN_DIR\`, PTY core $( [[ "${UNPEEL_PTY_CORE:-0}" == "1" ]] && echo on || echo off ) (profile \`$PROFILE\`).

| Measurement | Value | Detail |
| --- | ---: | --- |
| (a) serve + first session | $(kib_to_mib "$A_KIB") | serve $(kib_to_mib "$SERVE_AFTER_FIRST_KIB") + host $(kib_to_mib "$FIRST_HOST_KIB"), $FIRST_HOST_THREADS host threads |
| (b) per empty session | $(kib_to_mib "$B_KIB") | $SESSIONS × \`sh\`, one host process each |
| (c) per filled $LINES-line session | $(kib_to_mib "$C_KIB") | delta over (b), 72-col numbered prose; all ${#FILL_IDS[@]} filled hosts $(kib_to_mib "$FILLED_TOTAL_KIB") |
| (c') per filled session after ${IDLE_WAIT}s idle | $(kib_to_mib "$C_IDLE_KIB") | same sessions once quiet; all hosts $(kib_to_mib "$AFTER_IDLE_KIB") |
| (d) server-side per attached client | $( (( CLIENTS > 0 )) && kib_to_kib "$D_KIB" || echo "skipped" ) | $CLIENTS × \`unpeel-attach\` on one filled session ($ATTACHED live) |
| (e) leftovers after \`unpeel rm\` | $LEFT_PIDS hosts, $(kib_to_mib "$LEFT_KIB") | $LEFT_DIRS session dirs left; slow exits $SLOW_EXITS; rm failures ${#RM_FAILED[@]} (retried $RM_RETRIED); serve $(kib_to_mib "$SERVE_END_KIB") |
EOF
if [[ -n "$CORE_END_PID" ]]; then
  echo "| (f) PTY core after teardown | $(kib_to_mib "$CORE_END_KIB") | settled (instant $(kib_to_mib "$CORE_END_INSTANT_KIB")); zero sessions, $CORE_END_THREADS threads (pid $CORE_END_PID) |"
fi
if [[ -n "$VT_EMPTY_KIB" ]]; then
  echo "| (g) empty VT grid (80×24, in-process) | $(kib_to_kib "$VT_EMPTY_KIB") | one libghostty-vt terminal + render state, before any output |"
  echo "| (h) filled VT grid (10k lines) | $(kib_to_kib "$VT_FILLED_KIB") | delta over (g), journal attached; feed $VT_FEED_MIB_S MiB/s |"
fi
if [[ -n "$LEFTOVER_DETAIL" ]]; then
  printf '\nLeftover sessions (id, manifest state, surviving host pid):\n\n%s' "$LEFTOVER_DETAIL"
fi

# ---------- machine-readable numbers + CI gate ----------------------------------

NUMBERS_JSON=$(cat <<EOF
{
  "profile": "$PROFILE",
  "os": "$OS",
  "metric": "$( [[ "$OS" == "Linux" ]] && echo pss || echo phys_footprint )",
  "pty_core": $( [[ "${UNPEEL_PTY_CORE:-0}" == "1" ]] && echo true || echo false ),
  "sessions": $SESSIONS,
  "clients": $CLIENTS,
  "lines": $LINES,
  "serve_plus_first_session_kib": $A_KIB,
  "per_empty_session_kib": $B_KIB,
  "per_filled_session_kib": $C_KIB,
  "per_client_kib": $D_KIB,
  "leftover_hosts": $LEFT_PIDS,
  "leftover_dirs": $LEFT_DIRS,
  "slow_exits": $SLOW_EXITS,
  "rm_failures": ${#RM_FAILED[@]},
  "core_after_teardown_kib": $CORE_END_KIB,
  "core_after_teardown_instant_kib": $CORE_END_INSTANT_KIB,
  "first_host_threads": $FIRST_HOST_THREADS,
  "vt_empty_grid_kib": ${VT_EMPTY_KIB:-null},
  "vt_filled_grid_kib": ${VT_FILLED_KIB:-null},
  "vt_feed_mib_s": ${VT_FEED_MIB_S:-null}
}
EOF
)
if [[ -n "${BENCH_JSON:-}" ]]; then
  printf '%s\n' "$NUMBERS_JSON" >"$BENCH_JSON"
  log "wrote $BENCH_JSON"
fi

if [[ -n "${BENCH_THRESHOLDS:-}" ]]; then
  NUMBERS_FILE="$(mktemp "${TMPDIR:-/tmp}/bench-numbers.XXXXXX")"  # GNU mktemp needs the X template
  printf '%s\n' "$NUMBERS_JSON" >"$NUMBERS_FILE"
  python3 - "$BENCH_THRESHOLDS" "$PROFILE" "$CLIENTS" "$NUMBERS_FILE" <<'PY' || exit 1
import json, sys
thresholds_path, profile, clients = sys.argv[1], sys.argv[2], int(sys.argv[3])
numbers = json.load(open(sys.argv[4]))
ceilings = json.load(open(thresholds_path)).get(profile)
if ceilings is None:
    print(f"no thresholds for profile {profile!r} in {thresholds_path}", file=sys.stderr)
    sys.exit(1)
failed = []
for key, ceiling in ceilings.items():
    if key.startswith("_"):
        continue
    if key == "per_client_kib" and clients == 0:
        continue
    value = numbers.get(key)
    if value is None:
        if key.startswith("vt_"):
            continue  # only measured with BENCH_VT=1 (CI)
        failed.append(f"{key}: not measured")
    elif value > ceiling:
        failed.append(f"{key}: {value} > ceiling {ceiling}")
print()
if failed:
    print(f"THRESHOLDS FAILED ({profile}):")
    for line in failed:
        print(f"  - {line}")
    sys.exit(1)
print(f"thresholds ok ({profile}): every measured row within its ceiling")
PY
  rm -f "$NUMBERS_FILE"
fi
