#!/usr/bin/env bash
#
# Headless end-to-end verification of the Browser MCP pipeline:
#
#   unpeel-host __browser_mcp__ (tool → argv/env translation, grants)
#     → agent-browser native daemon (CDP)
#       → system Chrome/Chromium
#
# The engine's `--native` mode is experimental upstream, and integration
# already survived multiple engine landmines (profile+allowlist daemon wedge,
# historical mock-keychain cookie purges, STREAM_PORT no-op) — run this after every
# agent-browser version bump BEFORE raising the pinned version, and after
# changes to browser_mcp.rs. See the private "browser-mcp-deep-check" design record
# ("Native-Mode Findings") for why each check exists.
#
# Verifies:
#   1. engine    — binary resolves; open/snapshot-refs/screenshot/close work
#   2. rules     — ALLOWED_DOMAINS blocks an off-list navigation
#   3. logins    — SESSION_NAME + ENCRYPTION_KEY persist a cookie across a
#                  full browser restart, state file is encrypted (.json.enc)
#   4. profile   — persistent project-profile cookies survive a full restart
#   5. mcp       — __browser_mcp__ end to end: grant gate, one project browser,
#                  independent pinned tabs, per-tab close, session screenshot
#   6. remote    — when UNPEEL_TEST_REMOTE_CDP_URL is set, the same MCP tools
#                  attach to that authenticated WSS endpoint without local Chrome
#
# This is a SERVER-side check: it needs only this repo's crates (it builds
# `unpeel-host` debug itself), an agent-browser engine, and a system Chrome.
# No app checkout, GhosttyKit, or Swift toolchain is involved.
#
# Usage: scripts/verify-browser.sh
#        UNPEEL_TEST_REMOTE_CDP_URL='wss://…' scripts/verify-browser.sh
#        UNPEEL_BROWSER_BIN=/path/to/agent-browser scripts/verify-browser.sh
# Exit:  0 = all checks pass; non-zero with a FAIL line otherwise.
#        Exits 0 with a SKIP line when no engine or no Chrome is available
#        (set UNPEEL_VERIFY_BROWSER_STRICT=1 to turn that into a failure).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST_BIN="$REPO_ROOT/crates/target/debug/unpeel-host"

step() { printf '\n==> %s\n' "$1"; }
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; exit 1; }

# --- engine resolution (same order as browser_mcp.rs / build-app.sh) --------
ENGINE="${UNPEEL_BROWSER_BIN:-}"
if [ -z "$ENGINE" ]; then
  for candidate in \
    "$REPO_ROOT/node_modules/.bin/agent-browser" \
    "$HOME/.unpeel/browser/bin/agent-browser" \
    "$(command -v agent-browser 2>/dev/null || true)"; do
    [ -n "$candidate" ] && [ -e "$candidate" ] || continue
    resolved="$(readlink -f "$candidate" 2>/dev/null || echo "$candidate")"
    if head -c 2 "$resolved" 2>/dev/null | grep -q '#!'; then
      arch="$(uname -m | sed 's/x86_64/x64/')"
      native="$(dirname "$resolved")/agent-browser-darwin-$arch"
      [ -f "$native" ] && resolved="$native" || continue
    fi
    # bun/npm sometimes install the native slice without the exec bit; a
    # non-executable candidate is skipped so the next one (or
    # UNPEEL_BROWSER_BIN) can win instead of failing every engine call.
    [ -f "$resolved" ] && [ -x "$resolved" ] && { ENGINE="$resolved"; break; }
  done
fi
skip() {
  if [ "${UNPEEL_VERIFY_BROWSER_STRICT:-0}" = "1" ]; then fail "$1"; fi
  printf 'SKIP: %s\n' "$1"
  exit 0
}
[ -n "$ENGINE" ] && [ -x "$ENGINE" ] || skip "no executable agent-browser engine found (bun install; chmod +x the native slice, or set UNPEEL_BROWSER_BIN)"
pass "engine resolved: $ENGINE"
chrome_found=0
for candidate in "/Applications/Google Chrome.app" "/Applications/Chromium.app" \
    "$HOME/Applications/Google Chrome.app" "$HOME/Applications/Chromium.app"; do
  [ -d "$candidate" ] && { chrome_found=1; break; }
done
if [ "$chrome_found" = 0 ] \
  && ! command -v google-chrome >/dev/null 2>&1 \
  && ! command -v chromium >/dev/null 2>&1; then
  skip "no system Chrome/Chromium found for the native engine"
fi

step "building unpeel-host (debug)"
(cd "$REPO_ROOT/crates" && cargo build -q -p unpeel-host)

# Keep these under /tmp: agent-browser uses Unix-domain sockets, whose macOS
# path limit is only 103 bytes. The real ~/.unpeel path is similarly short.
SCRATCH="$(mktemp -d /tmp/unpeel-browser-mcp.XXXXXX)"
ENGINE_HOME="$(mktemp -d /tmp/unpeel-browser-engine.XXXXXX)"
cleanup() {
  for sess in vb-core vb-rules vb-state vb-keychain; do
    env -i HOME="$ENGINE_HOME" PATH=/usr/bin:/bin \
      AGENT_BROWSER_SESSION="$sess" AGENT_BROWSER_NATIVE=1 \
      "$ENGINE" close >/dev/null 2>&1 || true
  done
  for sess in vb-session-a vb-session-b; do
    UNPEEL_HOME="$SCRATCH" UNPEEL_BROWSER_BIN="$ENGINE" \
      "$HOST_BIN" __browser_cleanup__ "$sess" >/dev/null 2>&1 || true
  done
  # The MCP check's engine daemon runs with the real HOME, so its login-state
  # file lands in the real ~/.agent-browser — remove the test project's.
  rm -f "$HOME/.agent-browser/sessions/unpeel-proj-vb-proj-"* 2>/dev/null || true
  rm -rf "$SCRATCH" "$ENGINE_HOME"
}
trap cleanup EXIT

engine() { # engine <session> [VAR=value ...] <engine subcommand + args...>
  local sess="$1"; shift
  local envs=()
  while [ $# -gt 0 ] && [[ "$1" == *=* ]]; do envs+=("$1"); shift; done
  env -i HOME="$ENGINE_HOME" PATH=/usr/bin:/bin \
    AGENT_BROWSER_SESSION="$sess" AGENT_BROWSER_NATIVE=1 \
    AGENT_BROWSER_SOCKET_DIR="$ENGINE_HOME/sockets" \
    ${envs[@]+"${envs[@]}"} "$ENGINE" "$@"
}

# --- 1. core engine loop ------------------------------------------------------
step "engine core loop"
out="$(engine vb-core open https://example.com 2>&1)" \
  || fail "open example.com: $out"
snap="$(engine vb-core snapshot -i 2>&1)"
echo "$snap" | grep -q "ref=" || fail "snapshot has no refs: $snap"
shot="$ENGINE_HOME/shot.png"
engine vb-core screenshot "$shot" >/dev/null 2>&1
[ -s "$shot" ] || fail "screenshot not written"
engine vb-core close >/dev/null 2>&1
pass "open + snapshot refs + screenshot + close"

# --- 2. site rules enforced ---------------------------------------------------
step "site rules"
blocked="$(engine vb-rules AGENT_BROWSER_ALLOWED_DOMAINS=example.com \
  open https://vercel.com 2>&1 || true)"
echo "$blocked" | grep -qi "not in the allowed domains" \
  || fail "allowlist did not block off-list domain: $blocked"
engine vb-rules close >/dev/null 2>&1 || true
pass "ALLOWED_DOMAINS blocks navigation"

# --- 3. login persistence (state save/restore + encryption) -------------------
step "login persistence"
KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
state() { engine vb-state AGENT_BROWSER_SESSION_NAME=vb-proj \
  AGENT_BROWSER_ENCRYPTION_KEY="$KEY" "$@"; }
state open https://example.com >/dev/null 2>&1
state eval "document.cookie='vb_login=alive; max-age=86400; path=/'" >/dev/null 2>&1
state close >/dev/null 2>&1; sleep 1
ls "$ENGINE_HOME/.agent-browser/sessions/"vb-proj-*.json.enc >/dev/null 2>&1 \
  || fail "encrypted state file (.json.enc) not written"
state open https://example.com >/dev/null 2>&1
restored="$(state eval "document.cookie" 2>/dev/null | head -1)"
state close >/dev/null 2>&1
echo "$restored" | grep -q "vb_login=alive" \
  || fail "cookie did not survive browser restart via state restore: $restored"
pass "cookie survives restart; state encrypted at rest"

# --- 4. persistent profile cookie ---------------------------------------------
step "persistent profile cookie"
KPROF="$ENGINE_HOME/kprof"
kc() { engine vb-keychain AGENT_BROWSER_PROFILE="$KPROF" "$@"; }
kc open https://example.com >/dev/null 2>&1
kc eval "document.cookie='vb_keychain=probe; max-age=86400; path=/'" >/dev/null 2>&1
kc close >/dev/null 2>&1; sleep 1
kc open https://example.com >/dev/null 2>&1
kprobe="$(kc eval "document.cookie" 2>/dev/null | head -1)"
kc close >/dev/null 2>&1
echo "$kprobe" | grep -q "vb_keychain=probe" \
  || fail "project profile cookie did not survive browser restart: $kprobe"
pass "project profile cookie survives browser restart"

# --- 5. MCP server end to end --------------------------------------------------
step "mcp server"
for sess in vb-session-a vb-session-b; do
  mkdir -p "$SCRATCH/app-sessions/$sess"
  printf '%s\n' \
    "{\"session\":{\"id\":\"$sess\",\"project_id\":\"vb-proj\",\"label\":\"t\",\"custom_title\":false,\"command\":\"claude\",\"created_at\":1},\"cwd\":\"/tmp\",\"state\":\"running\",\"pid\":1,\"exit_code\":null,\"heartbeat_at\":1,\"updated_at\":1}" \
    > "$SCRATCH/app-sessions/$sess/manifest.json"
done
cat > "$SCRATCH/app-state.json" <<'EOF'
{"projects":[{"id":"vb-proj","name":"VB","path":"/tmp"}],"presets":[],"active_tabs":{},"browser_access":{"vb-session-a":"off","vb-session-b":"off"}}
EOF
mcp() { local sess="$1"; UNPEEL_HOME="$SCRATCH" UNPEEL_SESSION_ID="$sess" \
  UNPEEL_BROWSER_BIN="$ENGINE" "$HOST_BIN" __browser_mcp__; }

refused="$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_open","arguments":{"url":"https://example.com"}}}' \
  | mcp vb-session-a)"
echo "$refused" | grep -q '"isError":true' && echo "$refused" | grep -qi "browser access" \
  || fail "grant-off call was not refused: $refused"
pass "grant gate refuses when access is off"

python3 - "$SCRATCH/app-state.json" <<'EOF'
import json, sys
p = sys.argv[1]
d = json.load(open(p)); d["browser_access"] = {"vb-session-a": "on", "vb-session-b": "on"}; json.dump(d, open(p, "w"))
EOF
result_a="$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_open","arguments":{"url":"https://example.com/?agent=a"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"browser_screenshot","arguments":{}}}' \
  | mcp vb-session-a)"
echo "$result_a" | grep -q "Saved screenshot to" || fail "mcp screenshot failed: $result_a"
ls "$SCRATCH/app-sessions/vb-session-a/artifacts/browser/screenshots/"*.png >/dev/null 2>&1 \
  || fail "screenshot artifact missing from session dir"

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_open","arguments":{"url":"https://example.com/?agent=b"}}}' \
  | mcp vb-session-b >/dev/null
url_a="$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_get","arguments":{"what":"url"}}}' \
  | mcp vb-session-a)"
url_b="$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_get","arguments":{"what":"url"}}}' \
  | mcp vb-session-b)"
echo "$url_a" | grep -q 'agent=a' || fail "session A lost its pinned tab: $url_a"
echo "$url_b" | grep -q 'agent=b' || fail "session B lost its pinned tab: $url_b"

owner_pid_file="$(find "$SCRATCH/browser/sockets" -name 'unpeel-project-*.pid' -print -quit)"
[ -n "$owner_pid_file" ] || fail "shared project browser owner was not created"
[ "$(find "$SCRATCH/browser/projects" -name browser.json | wc -l | tr -d ' ')" = "1" ] \
  || fail "expected exactly one browser owner for the project"

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_close","arguments":{}}}' \
  | mcp vb-session-a >/dev/null
url_b_after="$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_get","arguments":{"what":"url"}}}' \
  | mcp vb-session-b)"
echo "$url_b_after" | grep -q 'agent=b' \
  || fail "closing session A disturbed session B: $url_b_after"
[ -s "$owner_pid_file" ] || fail "project browser closed while session B was live"

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_close","arguments":{}}}' \
  | mcp vb-session-b >/dev/null
for _ in 1 2 3 4 5; do
  [ ! -e "$owner_pid_file" ] && break
  sleep 1
done
[ ! -e "$owner_pid_file" ] || fail "project browser stayed alive after its final tab closed"
pass "one project window, independent pinned tabs, per-tab close, session artifact"

# --- 6. optional provider-owned remote CDP -----------------------------------
if [ -n "${UNPEEL_TEST_REMOTE_CDP_URL:-}" ]; then
  step "remote CDP MCP"
  mkdir -p "$SCRATCH/browser"
  python3 - "$SCRATCH/browser/remote-cdp.json" <<'EOF'
import json, os, sys
path = sys.argv[1]
with open(path, "w") as f:
    json.dump({
        "schema": 1,
        "endpoint": os.environ["UNPEEL_TEST_REMOTE_CDP_URL"],
        "provider": "test",
    }, f)
os.chmod(path, 0o600)
EOF
  remote_result="$(printf '%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_open","arguments":{"url":"https://example.com"}}}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"browser_screenshot","arguments":{"gallery":true}}}' \
    '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"browser_close","arguments":{}}}' \
    | mcp vb-session-a)"
  echo "$remote_result" | grep -q "Saved screenshot to" \
    || fail "remote CDP MCP screenshot failed: $remote_result"
  case "$remote_result" in
    *"$UNPEEL_TEST_REMOTE_CDP_URL"*) fail "remote CDP credential leaked into MCP output" ;;
  esac
  pass "browser MCP attaches to provider-owned remote CDP and redacts its credential"
else
  echo "note: remote CDP check skipped (set UNPEEL_TEST_REMOTE_CDP_URL to run it)"
fi

printf '\nAll browser MCP checks passed.\n'
