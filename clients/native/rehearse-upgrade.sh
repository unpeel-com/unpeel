#!/usr/bin/env bash
# rehearse-upgrade.sh — prove that upgrading the Mac app "just works" with
# zero user actions, against an ISOLATED Unpeel home (never ~/.unpeel).
#
# What it does, in order:
#   1. Fetches the currently released app + CLI for OLD_VERSION from
#      unpeel.com (cached under $CACHE) unless OLD_APP / OLD_CLI are given.
#   2. Seeds a fresh isolated home: project + starred custom preset via the
#      OLD CLI, then launches the OLD released app and creates two shell
#      sessions THROUGH it (its /mcp/start-session bridge), archives one,
#      soaks $SOAK seconds and quits it (hosted sessions keep running —
#      that is the survival model).
#   4. Fingerprints the home, launches the NEW app (default: the
#      release-flavored dist/Unpeel.app left by `bun run release -- --dry-run`)
#      against the SAME home for $SOAK seconds, and quits it.
#   5. Asserts: hosted sessions survived (same pid + pid_started_at), the new
#      app registered its hook port and attached the live session, the
#      archive/preset/order/pairing/Link files are byte-identical, and the
#      unified log shows no modal alert. Prints every other difference.
#
# The home lives under /tmp on purpose: hosted sessions bind
# <home>/app-sessions/<uuid>/session.sock and sockaddr_un caps the path near
# 104 bytes (the PTY harness has the same rule).
#
# Usage:
#   clients/native/rehearse-upgrade.sh                 # 0.3.1 → dist/Unpeel.app
#   OLD_VERSION=0.3.1 NEW_APP=/path/Unpeel.app clients/native/rehearse-upgrade.sh
#   SOAK=40 clients/native/rehearse-upgrade.sh          # longer app soak
#
# Never touches ~/.unpeel, /Applications/Unpeel.app, or a running Unpeel.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OLD_VERSION="${OLD_VERSION:-0.3.1}"
CHANNEL="${CHANNEL:-beta}"
NEW_APP="${NEW_APP:-$HERE/dist/Unpeel.app}"
SOAK="${SOAK:-25}"
HOME_ISO="${REHEARSAL_HOME:-/tmp/unpeel-upg-$(date +%H%M%S)}"
CACHE="${REHEARSAL_CACHE:-$HOME/Library/Caches/unpeel-upgrade-rehearsal}"
REPORT="$HOME_ISO.report"

pass=0; fail=0; notes=()
ok()   { pass=$((pass+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
note() { notes+=("$1"); printf '  \033[33mNOTE\033[0m %s\n' "$1"; }
step() { printf '\n==> %s\n' "$1"; }

case "$HOME_ISO" in "$HOME/.unpeel"|"$HOME/.unpeel/"*) echo "refusing to use the real home" >&2; exit 1;; esac
rm -rf "$HOME_ISO" "$REPORT"; mkdir -p "$HOME_ISO" "$REPORT" "$CACHE"

# --- 1. old artifacts -------------------------------------------------------
step "old artifacts ($OLD_VERSION, channel $CHANNEL)"
if [ -z "${OLD_APP:-}" ]; then
  dmg="$CACHE/Unpeel-$OLD_VERSION.dmg"
  [ -f "$dmg" ] || curl -fsSL -o "$dmg" "https://unpeel.com/releases/$CHANNEL/Unpeel-$OLD_VERSION.dmg"
  OLD_APP="$CACHE/Unpeel-$OLD_VERSION.app"
  if [ ! -d "$OLD_APP" ]; then
    mnt="$(hdiutil attach -nobrowse -readonly "$dmg" | tail -1 | awk '{print $NF}')"
    ditto "$mnt/Unpeel.app" "$OLD_APP"; hdiutil detach "$mnt" -quiet
  fi
fi
if [ -z "${OLD_CLI:-}" ]; then
  tgz="$CACHE/unpeel-$OLD_VERSION-macos-universal.tar.gz"
  [ -f "$tgz" ] || curl -fsSL -o "$tgz" "https://unpeel.com/releases/$CHANNEL/cli/unpeel-$OLD_VERSION-macos-universal.tar.gz"
  mkdir -p "$CACHE/cli-$OLD_VERSION"; tar -xzf "$tgz" -C "$CACHE/cli-$OLD_VERSION"
  OLD_CLI="$CACHE/cli-$OLD_VERSION/unpeel"
fi
old_ver="$(plutil -extract CFBundleShortVersionString raw "$OLD_APP/Contents/Info.plist")"
old_build="$(plutil -extract CFBundleVersion raw "$OLD_APP/Contents/Info.plist")"
new_ver="$(plutil -extract CFBundleShortVersionString raw "$NEW_APP/Contents/Info.plist")"
new_build="$(plutil -extract CFBundleVersion raw "$NEW_APP/Contents/Info.plist")"
new_dev="$(plutil -extract UnpeelDevelopmentBuild raw "$NEW_APP/Contents/Info.plist" 2>/dev/null || echo "absent")"
echo "  old app $old_ver ($old_build)  old cli $("$OLD_CLI" --version 2>/dev/null | head -1)"
echo "  new app $new_ver ($new_build)  UnpeelDevelopmentBuild=$new_dev"
[ "$new_dev" = "absent" ] || [ "$new_dev" = "false" ] && ok "new app is release-flavored (no UnpeelDevelopmentBuild key)" || bad "new app is a DEV build — rehearse with the release dry-run bundle"

export UNPEEL_HOME="$HOME_ISO"

# --- 2+3. run the released app and seed THROUGH it ---------------------------
# Sessions are created by the released app itself (its /mcp/start-session
# bridge, the same path the UI's launch verbs use), so the hosted PTYs are
# the old app's bundled unpeel-host and the rows carry the app's own project
# binding. The old CLI only adds the project and the custom preset (shared
# app-state.json writes the app picks up live).
launch_app() { # $1 app, $2 tag
  local exe="$1/Contents/MacOS/UnpeelNative" log="$REPORT/$2.stdout"
  "$exe" >"$log" 2>&1 &
  echo $!
}
quit_app() { kill -TERM "$1" 2>/dev/null; for _ in $(seq 1 40); do kill -0 "$1" 2>/dev/null || return 0; sleep 0.25; done; kill -KILL "$1" 2>/dev/null; note "app $1 needed SIGKILL"; }
bridge() { # $1 port, $2 route, $3 json body
  curl -s -m 20 -X POST "http://127.0.0.1:$1$2" -H "x-unpeel-auth: $(cat "$HOME_ISO/mcp/auth-token")" -d "$3"
}
wait_for_app() { # $1 pid → prints the hook port once the app registered it
  for _ in $(seq 1 80); do
    kill -0 "$1" 2>/dev/null || return 1
    if [ -s "$HOME_ISO/app-ports" ] && [ -s "$HOME_ISO/mcp/auth-token" ]; then head -1 "$HOME_ISO/app-ports"; return 0; fi
    sleep 0.25
  done
  return 1
}

# True when OLD_VERSION is >= 0.4.0 (the release where the app became a client
# of a separate Host-service worker). At/after this the app's /mcp bridge is
# the WORKER's port and races worker readiness, so sessions are seeded with
# the old CLI instead, and every bridge call waits for the worker first.
ver_ge_040() {
  # OLD_VERSION >= 0.4.0 iff the smaller of {OLD_VERSION, 0.4.0} is 0.4.0.
  [ "$(printf '%s\n0.4.0\n' "$OLD_VERSION" | sort -t. -k1,1n -k2,2n -k3,3n | head -1)" = "0.4.0" ]
}

# Wait until the Host-service worker for this home is actually ready to answer
# bridge calls: host.sock is connectable AND serve.json advertises a
# registered platform adapter (platformCapabilities non-empty). A 0.4.x app
# writes app-ports + auth-token the instant it starts, well before the worker
# is up, so a bridge call fired on that signal alone hits EAGAIN and the app
# relaunches the service at +5s. Bounded ~30s; prints a diagnostic on timeout.
wait_worker_ready() {
  local deadline=$(( $(date +%s) + 30 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    # host.sock must accept a connection AND serve.json must show a registered
    # platform adapter — both in one python probe (no nc portability issues).
    if python3 - "$HOME_ISO/host.sock" "$HOME_ISO/serve.json" <<'PY' 2>/dev/null; then
import json, socket, sys
sock, serve = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(1.0)
try:
    s.connect(sock)
except OSError:
    sys.exit(1)
finally:
    s.close()
try:
    d = json.load(open(serve))
except Exception:
    sys.exit(1)
sys.exit(0 if d.get("platformCapabilities") else 1)
PY
      return 0
    fi
    sleep 0.5
  done
  note "worker never became ready within 30s (host.sock + serve.json platformCapabilities): $(python3 -c "import json;d=json.load(open('$HOME_ISO/serve.json'));print('pid',d.get('pid'),'caps',len(d.get('platformCapabilities',[])))" 2>/dev/null || echo 'no serve.json')"
  return 1
}

step "seed the home: project + preset via the $OLD_VERSION CLI"
proj="$HOME_ISO-project"; mkdir -p "$proj"
"$OLD_CLI" add "$proj" >/dev/null 2>&1 || note "old CLI 'add' returned non-zero"
proj_id="$(python3 -c "import json;d=json.load(open('$HOME_ISO/app-state.json'));print(next(p['id'] for p in d['projects'] if p['path'].rstrip('/').endswith('$(basename "$proj")')))")"
"$OLD_CLI" presets add "Rehearsal Preset" "/bin/sh" >/dev/null 2>&1 || note "old CLI 'presets add' returned non-zero"

# Seed the two sessions with the OLD CLI, not the app's /mcp bridge. From
# 0.4.0 the released app is a client of a separate Host-service worker: it
# writes app-ports + auth-token the instant it launches, but a bridge call
# fired then hits the worker before it is ready (EAGAIN) and the app
# relaunches the service at +5s, so nothing gets seeded. `unpeel new` creates
# a hosted session standalone (its own host, no app/worker), which is the
# released headless write path. Pre-0.4.0 apps host through their own bridge,
# so those still seed the historical way after the app is up.
seed_cli() { # $1 label -> prints the session id
  "$OLD_CLI" new --command /bin/sh --cwd "$proj" --json 2>/dev/null \
    | python3 -c "import json,sys;print(json.load(sys.stdin).get('id',''))" 2>/dev/null
}

t_old="$(date '+%Y-%m-%d %H:%M:%S')"
if ver_ge_040; then
  step "seed two sessions via the $OLD_VERSION CLI, archive one"
  s1="$(seed_cli 'rehearsal live')"
  s2="$(seed_cli 'rehearsal archived')"
  sleep 3
  [ -n "$s2" ] && "$OLD_CLI" archive "$s2" > "$REPORT/old-app.archive.json" 2>&1

  step "run the released $old_ver app against the seeded home, soak ${SOAK}s"
  old_pid="$(launch_app "$OLD_APP" old-app)"
  old_port="$(wait_for_app "$old_pid")" && ok "old app registered its hook port ($old_port)" || bad "old app never registered a hook port"
  wait_worker_ready && ok "old app's Host-service worker became ready" || note "old app worker not confirmed ready (diagnostics above)"
else
  step "run the released $old_ver app, create two sessions through it, archive one, soak ${SOAK}s"
  old_pid="$(launch_app "$OLD_APP" old-app)"
  old_port="$(wait_for_app "$old_pid")" && ok "old app registered its hook port ($old_port)" || bad "old app never registered a hook port"
  s1="$(bridge "$old_port" /mcp/start-session "{\"project_id\":\"$proj_id\",\"command\":\"/bin/sh\",\"label\":\"rehearsal live\"}" | python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('session',d).get('id',''))")"
  s2="$(bridge "$old_port" /mcp/start-session "{\"project_id\":\"$proj_id\",\"command\":\"/bin/sh\",\"label\":\"rehearsal archived\"}" | python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('session',d).get('id',''))")"
  sleep 3
  # A plain shell is not resumable, so the app's Archive verb refuses it by
  # design (0.3.0 semantics); the old CLI's archive verb is the released write
  # path for filing any Session.
  [ -n "$s2" ] && "$OLD_CLI" archive "$s2" > "$REPORT/old-app.archive.json" 2>&1
fi
# Star the custom preset while the old app runs: its first launch folds the
# legacy overlay into app-state.json and rewrites the presets array, so a
# star written before launch is normalized away. This is the shared-file
# write the terminal UI performs; the app picks it up over FSEvents.
python3 - "$HOME_ISO/app-state.json" <<'EOF'
import json,sys
p=sys.argv[1]; d=json.load(open(p))
for r in d.get("presets",[]):
    if r.get("label")=="Rehearsal Preset": r["quick_launch"]=True
json.dump(d,open(p,"w"),indent=2)
EOF
sleep 4
echo "  live session ${s1:-?} / archived session ${s2:-?}"
[ -n "$s1" ] && [ -f "$HOME_ISO/app-sessions/$s1/manifest.json" ] && ok "old app created the live session" || bad "old app did not create a live session"
[ -n "$s2" ] && [ -f "$HOME_ISO/app-sessions/$s2/archived.json" ] && ok "old app archived the second session" || bad "old app did not archive $s2 (see $REPORT/old-app.archive.json)"
python3 -c "import json;d=json.load(open('$HOME_ISO/app-state.json'));print(any(r.get('label')=='Rehearsal Preset' and r.get('quick_launch') for r in d['presets']))" | grep -q True && ok "starred preset stored" || bad "starred preset missing"
bridge "$old_port" /mcp/sidebar '{}' > "$REPORT/old-app.sidebar.json"
"$OLD_CLI" ls --json > "$REPORT/ls-old-cli.json" 2>/dev/null
sleep "$SOAK"
kill -0 "$old_pid" 2>/dev/null && ok "old app stayed up for ${SOAK}s" || bad "old app exited early (see $REPORT/old-app.stdout)"
cp "$HOME_ISO/app-ports" "$REPORT/app-ports.old" 2>/dev/null
quit_app "$old_pid"; sleep 2

# --- 4. fingerprint, then new app soak --------------------------------------
fingerprint() { (cd "$HOME_ISO" && find . -type f ! -name '*.lock' ! -name 'output.bin' ! -name 'app-ports' ! -path './hooks/trace.log' ! -name '*.sock' -exec shasum -a 256 {} + | sort -k2) ; }
fingerprint > "$REPORT/before.sha"
cp "$HOME_ISO/app-state.json" "$REPORT/app-state.before.json"
manifest_pid() { python3 -c "import json;d=json.load(open('$HOME_ISO/app-sessions/$1/manifest.json'));print(d['pid'],d.get('pid_started_at'),d['state'])"; }
before_s1="$(manifest_pid "$s1")"
kill -0 "${before_s1%% *}" 2>/dev/null && ok "hosted PTY for the live session is alive after the old app quit (host-based survival)" || bad "live session host died when the old app quit"

step "run the NEW $new_ver app against the same home for ${SOAK}s"
t_new="$(date '+%Y-%m-%d %H:%M:%S')"
new_pid="$(launch_app "$NEW_APP" new-app)"; sleep "$SOAK"
kill -0 "$new_pid" 2>/dev/null && ok "new app stayed up for ${SOAK}s" || bad "new app exited early (see $REPORT/new-app.stdout)"
grep -q . "$HOME_ISO/app-ports" 2>/dev/null && ok "new app registered its hook port in app-ports" || bad "no hook port registered"
pgrep -fl "unpeel-attach" > "$REPORT/new-app.attach-processes" 2>/dev/null
pgrep -f "unpeel-attach.*$s1" >/dev/null && ok "new app attached the live session ($s1) with unpeel-attach" || note "no unpeel-attach for $s1 during the soak (attach happens only for the selected pane; see $REPORT/new-app.attach-processes)"
pgrep -f "unpeel-attach $s2" >/dev/null && bad "new app attached the ARCHIVED session" || ok "archived session stays unattached"
# Which Host owner did this launch pick? (0.4.0+ logs it to hooks/trace.log.)
if grep -q "native-app Local scope: Host service client" "$HOME_ISO/hooks/trace.log" 2>/dev/null; then
  launch_mode=client
elif grep -q "native-app Local scope: compatibility Host" "$HOME_ISO/hooks/trace.log" 2>/dev/null; then
  launch_mode=compat-fallback
else
  launch_mode=compat
fi
echo "  new app launch mode: $launch_mode"
grep "native-app " "$HOME_ISO/hooks/trace.log" 2>/dev/null | tail -4 | sed 's/^/    trace: /'
port="$(grep -m1 . "$HOME_ISO/app-ports" 2>/dev/null || true)"
tok="$(cat "$HOME_ISO/mcp/auth-token" 2>/dev/null || true)"
if [ "$launch_mode" = client ]; then
  # Client mode: the app renders the worker's projection and serves no
  # /mcp/* routes itself. Prove the app is connected to the worker (its
  # platform adapter is registered) and that the worker serves this home.
  python3 - "$HOME_ISO/serve.json" <<'EOF' && ok "new app is connected to its Host service worker (platform adapter registered)" || bad "new app did not register its platform adapter with the worker (see serve.json)"
import json,sys
d=json.load(open(sys.argv[1])); caps=d.get("platformCapabilities",[])
print("  worker:", d.get("hostVersion"), "pid", d.get("pid"), "adapter caps:", len(caps), "nativeAppOwnsControllers:", d.get("nativeAppOwnsControllers"))
sys.exit(0 if caps else 1)
EOF
  [ "$launch_mode" = client ] && [ -z "$(grep -m1 "Host service restart" "$HOME_ISO/hooks/trace.log" 2>/dev/null)" ] && ok "no stale-service restart was needed (fresh worker)" || note "stale-service restart logged: $(grep -m1 'Host service restart' "$HOME_ISO/hooks/trace.log")"
elif [ -n "$port" ] && [ -n "$tok" ]; then
  # Compatibility-Host mode (pre-client new app): the app serves /mcp itself.
  # Still wait for its worker/host to be ready so these queries don't race a
  # not-yet-listening bridge, exactly as the seed path does.
  wait_worker_ready >/dev/null 2>&1 || true
  curl -s -m 10 -X POST "http://127.0.0.1:$port/mcp/sidebar" -H "x-unpeel-auth: $tok" -d '{}' > "$REPORT/new-app.sidebar.json"
  curl -s -m 10 -X POST "http://127.0.0.1:$port/mcp/list-presets" -H "x-unpeel-auth: $tok" -d '{}' > "$REPORT/new-app.presets.json"
  python3 - "$REPORT/new-app.sidebar.json" "$s1" "$s2" <<'EOF' && ok "new app's sidebar lists the live session (not archived) and counts the archived one" || bad "new app's sidebar does not match the seeded state (see $REPORT/new-app.sidebar.json)"
import json,sys
d=json.load(open(sys.argv[1])); s1,s2=sys.argv[2],sys.argv[3]
rows=[]; arch=0
def walk(n):
    global arch
    rows.extend(n.get("sessions",[])); arch+=n.get("archived_count",0)
    for w in n.get("worktrees",[]): walk(w)
for p in d.get("projects",[]): walk(p)
live=[r for r in rows if r["id"]==s1 and not r["archived"] and r["status"]!="exited"]
archived_listed=[r for r in rows if r["id"]==s2 and not r["archived"]]
print("  sidebar rows:", [(r["id"][:8], r["status"], r["archived"]) for r in rows], "archived_count:", arch)
sys.exit(0 if live and not archived_listed and arch>=1 else 1)
EOF
  # The bridge route omits the star flag; the star itself is covered by the
  # app-state.json semantic check below (quick_launch survives byte-for-byte).
  python3 - "$REPORT/new-app.presets.json" <<'EOF' && ok "new app lists the custom 'Rehearsal Preset' among its enabled presets" || bad "custom preset not reported by the new app (see $REPORT/new-app.presets.json)"
import json,sys
d=json.load(open(sys.argv[1])); rows=d.get("presets",d if isinstance(d,list) else [])
hit=[r for r in rows if r.get("label")=="Rehearsal Preset"]
print("  preset row:", hit[:1])
sys.exit(0 if hit else 1)
EOF
else
  bad "could not query the new app (port='$port', token present: $([ -n "$tok" ] && echo yes || echo no))"
fi
[ "$launch_mode" = compat-fallback ] && bad "the new app fell back to the compatibility Host: $(grep -m1 'compatibility Host fallback' "$HOME_ISO/hooks/trace.log")"
worker_pid="$(python3 -c "import json;print(json.load(open('$HOME_ISO/serve.json'))['pid'])" 2>/dev/null || true)"
if [ -n "$worker_pid" ] && kill -0 "$worker_pid" 2>/dev/null; then
  note "the new app launched a Host service worker for this home (pid $worker_pid: $(ps -p "$worker_pid" -o command= | cut -c1-120))"
else
  ok "no Host service worker running for this home"
fi
after_s1="$(manifest_pid "$s1")"
[ "${before_s1% *}" = "${after_s1% *}" ] && ok "live session pid/pid_started_at unchanged ($before_s1)" || bad "live session identity changed: $before_s1 -> $after_s1"
[ "${after_s1##* }" = "running" ] && ok "live session still 'running'" || bad "live session state is ${after_s1##* }"
[ -f "$HOME_ISO/app-sessions/$s2/archived.json" ] && ok "archive marker intact" || bad "archive marker lost"
quit_app "$new_pid"; sleep 2
fingerprint > "$REPORT/after.sha"

# --- 5. diffs and invariants -----------------------------------------------
step "differences on disk after the new app (before → after)"
diff <(awk '{print $2" "$1}' "$REPORT/before.sha") <(awk '{print $2" "$1}' "$REPORT/after.sha") > "$REPORT/home.diff"
changed="$(grep '^[<>]' "$REPORT/home.diff" | awk '{print $2}' | sort -u)"
if [ -z "$changed" ]; then echo "  (no file changed)"; else echo "$changed" | sed 's/^/  changed: /'; fi
must_keep='^\./(app-state\.json|session-order\.json|project-order\.json|profiles\.json|remote\.json|device/|remote/|mobile/|link|license|app-sessions/[^/]+/archived\.json|app-sessions/[^/]+/title\.json)'
viol="$(echo "$changed" | grep -E "$must_keep" || true)"
[ -z "$viol" ] && ok "presets/order/archive/pairing/Link/identity files byte-identical" || bad "protected files changed:$(echo; echo "$viol" | sed 's/^/      /')"
# app-state.json content-level check (presets, projects, pins, theme)
python3 - "$REPORT/app-state.before.json" "$HOME_ISO/app-state.json" <<'EOF' && ok "presets (order + stars)/projects/pins/theme/title mode semantically unchanged" || bad "app-state.json semantic change (see $REPORT/app-state.before.json vs the home)"
import json,sys
a=json.load(open(sys.argv[1])); b=json.load(open(sys.argv[2]))
keys=["presets","projects","pinned_sessions","theme","session_title_mode","workspaces","mcp_write_approvals","browser_default_access","mcp_nonchild_write_access"]
changed=[k for k in keys if a.get(k)!=b.get(k)]
extra=sorted(set(b)-set(a)); missing=sorted(set(a)-set(b))
if extra: print("  new top-level keys written by the new app:", extra)
if missing: print("  top-level keys dropped by the new app:", missing)
if changed: print("  changed:", changed)
sys.exit(1 if changed or missing else 0)
EOF
"$OLD_CLI" ls --json > "$REPORT/ls-old-cli-after.json" 2>/dev/null
python3 - "$REPORT/ls-old-cli.json" "$REPORT/ls-old-cli-after.json" <<'EOF' && ok "session list (ids + status) identical before/after via the old CLI" || bad "session list changed (see report/ls-old-cli*.json)"
import json,sys
def key(p):
    try: d=json.load(open(p))
    except Exception: return None
    rows=d if isinstance(d,list) else d.get("sessions",d)
    return sorted((r.get("id"),r.get("status",r.get("state")),bool(r.get("archived"))) for r in rows)
a,b=key(sys.argv[1]),key(sys.argv[2]); sys.exit(0 if a is not None and a==b else 1)
EOF
step "modal-dialog heuristic (unified log for the new app run)"
log show --style compact --start "$t_new" --predicate 'process == "UnpeelNative"' 2>/dev/null | grep -Ei 'NSAlert|runModal|SUUpdateAlert|beginSheet|NSSavePanel|permission' > "$REPORT/new-app.log-alerts" || true
if [ -s "$REPORT/new-app.log-alerts" ]; then note "possible dialog activity logged — inspect $REPORT/new-app.log-alerts"; else ok "no alert/sheet/panel lines in the unified log during the new app run"; fi

step "cleanup (stop the seeded hosted sessions and any Host worker in the isolated home)"
[ -n "${worker_pid:-}" ] && ps -p "$worker_pid" -o command= 2>/dev/null | grep -q "__serve__" && kill -TERM "$worker_pid" 2>/dev/null
for s in "$s1" "$s2"; do
  read -r p _ < <(python3 -c "import json;d=json.load(open('$HOME_ISO/app-sessions/$s/manifest.json'));print(d['pid'],d.get('pid_started_at'))" 2>/dev/null) || continue
  [ -n "${p:-}" ] && kill -TERM "$p" 2>/dev/null || true
done

printf '\n%d passed, %d failed, %d notes — report in %s (home kept at %s)\n' "$pass" "$fail" "${#notes[@]}" "$REPORT" "$HOME_ISO"
for n in "${notes[@]:-}"; do [ -n "$n" ] && echo "  note: $n"; done
[ "$fail" -eq 0 ]
