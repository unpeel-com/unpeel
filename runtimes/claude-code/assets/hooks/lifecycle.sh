#!/bin/bash
umask 077
INPUT=$(cat)
# Global provider hooks must be inert outside a hosted Unpeel Session.
[ -n "${UNPEEL_SESSION_ID:-}" ] || exit 0
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-${UNPEEL_HOME:-$HOME/.unpeel}/hooks/trace.log}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true
# Cap trace growth so the log can never grow without bound.
if [ -f "$TRACE_FILE" ]; then
  _unpeel_trace_size=$(wc -c < "$TRACE_FILE" 2>/dev/null | tr -d ' ')
  if [ -n "$_unpeel_trace_size" ] && [ "$_unpeel_trace_size" -gt 10485760 ]; then
    mv -f "$TRACE_FILE" "$TRACE_FILE.1" 2>/dev/null || true
  fi
fi
# The payload contains the user's prompt text; the trace line at the end of
# this script logs only the event/tool/post metadata unless
# UNPEEL_HOOK_TRACE_VERBOSE=1 explicitly opts into full-payload logging.
UNPEEL_PORT_REGISTRY_FILE="${UNPEEL_APP_PORT_REGISTRY_FILE:-${UNPEEL_HOME:-$HOME/.unpeel}/app-ports}"

# POST one hook payload synchronously and record the outcome in
# _hook_post_results ("<port>=<http-code>,..."). Loopback posts finish in
# milliseconds; the tight timeouts bound the worst case when a registry port
# is stale so a hook never burns its whole timeout budget on delivery.
post_hook_payload() {
  _hook_payload="$1"
  _hook_session_id="$2"
  _hook_port="$3"
  [ -n "$_hook_port" ] || return 1
  _hook_http_code=$(printf '%s' "$_hook_payload" | curl -s -o /dev/null -w '%{http_code}' \
    --noproxy '*' --connect-timeout 0.2 --max-time 1 -X POST -H "Content-Type: application/json" \
    -d @- "http://127.0.0.1:$_hook_port/hook/$_hook_session_id" 2>/dev/null) \
    || _hook_http_code="curl-fail"
  [ -n "$_hook_http_code" ] || _hook_http_code="curl-fail"
  if [ -z "${_hook_post_results:-}" ]; then
    _hook_post_results="$_hook_port=$_hook_http_code"
  else
    _hook_post_results="$_hook_post_results,$_hook_port=$_hook_http_code"
  fi
  printf 'hook-post session=%s port=%s status=%s\n' \
    "$_hook_session_id" "$_hook_port" "$_hook_http_code" >> "$TRACE_FILE" 2>/dev/null || true
  case "$_hook_http_code" in
    2*) return 0 ;;
    *) return 1 ;;
  esac
}

current_unpeel_ports() {
  [ -f "$UNPEEL_PORT_REGISTRY_FILE" ] || return 1
  awk '/^[[:space:]]*[0-9]+[[:space:]]*$/ && $1 > 0 && $1 <= 65535 && !seen[$1 + 0]++ { print $1 + 0 }' \
    "$UNPEEL_PORT_REGISTRY_FILE" 2>/dev/null
}

post_hook_payload_to_current_ports() {
  _hook_payload="$1"
  _hook_session_id="$2"
  _hook_skip_port="$3"
  _hook_post_pids=""
  for _hook_candidate_port in $(current_unpeel_ports); do
    [ -n "$_hook_candidate_port" ] || continue
    [ "$_hook_candidate_port" = "$_hook_skip_port" ] && continue
    ( post_hook_payload "$_hook_payload" "$_hook_session_id" "$_hook_candidate_port" || true ) &
    _hook_post_pids="$_hook_post_pids $!"
  done
  for _hook_post_pid in $_hook_post_pids; do
    wait "$_hook_post_pid" || true
  done
  return 0
}

json_escape_string() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

runtime_generation_json_field() {
  case "${UNPEEL_RUNTIME_GENERATION:-}" in
    ''|*[!0-9]*) return 0 ;;
  esac
  printf ',"unpeel_runtime_generation":%s' "$UNPEEL_RUNTIME_GENERATION"
}

add_runtime_generation_to_payload() {
  _generation_payload="$1"
  case "${UNPEEL_RUNTIME_GENERATION:-}" in
    ''|*[!0-9]*) printf '%s' "$_generation_payload"; return 0 ;;
  esac
  if printf '%s' "$_generation_payload" | grep -q '"unpeel_runtime_generation"[[:space:]]*:'; then
    printf '%s' "$_generation_payload"
  elif printf '%s' "$_generation_payload" | grep -q '^[[:space:]]*{'; then
    printf '%s' "$_generation_payload" | sed "1s/^[[:space:]]*{/&\"unpeel_runtime_generation\":$UNPEEL_RUNTIME_GENERATION,/"
  else
    printf '%s' "$_generation_payload"
  fi
}

# Persist the last lifecycle event into the session dir so a restarted app can
# re-seed busy/attention state: hooks keep firing while no app instance is
# listening, so this file is the durable record of the final transition.
# Written atomically; never creates the session dir (the session may be gone).
record_last_hook_event() {
  _record_event_name="$1"
  _record_tool_name="$2"
  [ -n "${UNPEEL_SESSION_ID:-}" ] || return 0
  _record_dir="${UNPEEL_SESSION_DIR:-${UNPEEL_HOME:-$HOME/.unpeel}/app-sessions/$UNPEEL_SESSION_ID}"
  [ -d "$_record_dir" ] || return 0
  _record_name_json="$(json_escape_string "$_record_event_name")"
  _record_generation="$(runtime_generation_json_field)"
  if [ -n "$_record_tool_name" ]; then
    _record_tool_json="$(json_escape_string "$_record_tool_name")"
    _record_payload=$(printf '{"hook_event_name":"%s","tool_name":"%s"%s}' "$_record_name_json" "$_record_tool_json" "$_record_generation")
  else
    _record_payload=$(printf '{"hook_event_name":"%s"%s}' "$_record_name_json" "$_record_generation")
  fi
  _record_tmp="$_record_dir/.last-hook-event.json.$$"
  if printf '%s' "$_record_payload" > "$_record_tmp" 2>/dev/null; then
    mv -f "$_record_tmp" "$_record_dir/last-hook-event.json" 2>/dev/null \
      || rm -f "$_record_tmp" 2>/dev/null || true
  fi
}

trace_claude_hook() {
  _trace_reason="$1"
  if [ -n "$_trace_reason" ]; then
    printf '%s claude-hook session=%s event=%s tool=%s post=%s ignored=%s\n' \
      "$(date '+%Y-%m-%d %H:%M:%S')" \
      "${UNPEEL_SESSION_ID:-}" \
      "$LAST_EVENT_NAME" \
      "$LAST_TOOL_NAME" \
      "${_hook_post_results:-none}" \
      "$_trace_reason" >> "$TRACE_FILE" 2>/dev/null || true
  else
    printf '%s claude-hook session=%s event=%s tool=%s post=%s\n' \
      "$(date '+%Y-%m-%d %H:%M:%S')" \
      "${UNPEEL_SESSION_ID:-}" \
      "$LAST_EVENT_NAME" \
      "$LAST_TOOL_NAME" \
      "${_hook_post_results:-none}" >> "$TRACE_FILE" 2>/dev/null || true
  fi
  if [ "${UNPEEL_HOOK_TRACE_VERBOSE:-}" = "1" ]; then
    printf '%s claude-hook-payload session=%s payload=%s\n' \
      "$(date '+%Y-%m-%d %H:%M:%S')" \
      "${UNPEEL_SESSION_ID:-}" \
      "$INPUT" >> "$TRACE_FILE" 2>/dev/null || true
  fi
}

# Grok scans ~/.claude/settings.json for compatibility and injects
# GROK_SESSION_ID on every hook. Unpeel's grok-hook.sh already maps
# Grok-native events. Forwarding Grok's camelCase session_start here
# is normalized to a busy Start and spins the sidebar from launch;
# Grok's idle TUI then re-arms that busy state forever.
if [ -n "${GROK_SESSION_ID:-}" ]; then
  printf '%s claude-hook session=%s port=%s ignored=grok\n' \
    "$(date '+%Y-%m-%d %H:%M:%S')" \
    "${UNPEEL_SESSION_ID:-}" \
    "${UNPEEL_APP_PORT:-}" >> "$TRACE_FILE" 2>/dev/null || true
  exit 0
fi

LAST_EVENT_NAME=$(printf '%s' "$INPUT" | grep -oE '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
if [ -z "$LAST_EVENT_NAME" ]; then
  LAST_EVENT_NAME=$(printf '%s' "$INPUT" | grep -oE '"hookEventName"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
fi
LAST_TOOL_NAME=$(printf '%s' "$INPUT" | grep -oE '"tool_name"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | grep -oE '"[^"]*"$' | tr -d '"')
if [ -z "$LAST_TOOL_NAME" ]; then
  LAST_TOOL_NAME=$(printf '%s' "$INPUT" | grep -oE '"toolName"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | grep -oE '"[^"]*"$' | tr -d '"')
fi

# SessionStart fires at launch and on in-tool /resume, /clear, /compact,
# carrying the (new) session_id + transcript_path. Forward it as HookSeen so
# it only latches provider metadata — posted verbatim the server would
# treat a Claude-shaped SessionStart as busy. This is what re-links an
# Unpeel session to the conversation the user resumed inside claude,
# before any prompt is typed. Also accept Grok/Cursor camelCase names.
case "$LAST_EVENT_NAME" in
  SessionStart|session_start|sessionStart)
    INPUT=$(printf '%s' "$INPUT" | sed \
      -e 's/"hook_event_name"[[:space:]]*:[[:space:]]*"SessionStart"/"hook_event_name":"HookSeen"/' \
      -e 's/"hookEventName"[[:space:]]*:[[:space:]]*"session_start"/"hookEventName":"HookSeen"/' \
      -e 's/"hookEventName"[[:space:]]*:[[:space:]]*"SessionStart"/"hookEventName":"HookSeen"/' \
      -e 's/"hookEventName"[[:space:]]*:[[:space:]]*"sessionStart"/"hookEventName":"HookSeen"/')
    LAST_EVENT_NAME="HookSeen"
    ;;
esac

INPUT=$(add_runtime_generation_to_payload "$INPUT")

case "$LAST_EVENT_NAME" in
  Start|UserPromptSubmit|Stop|StopFailure|PermissionRequest)
    record_last_hook_event "$LAST_EVENT_NAME" "$LAST_TOOL_NAME"
    ;;
esac

_hook_post_results=""
if [ -n "$UNPEEL_SESSION_ID" ]; then
  # Several Unpeel instances can run at once (e.g. a dev build next to the
  # installed app) and they share the port registry. Post to every known
  # port, not just the first that answers, so the instance that owns this
  # session always receives the event. Posts go out synchronously and in
  # order: backgrounded fire-and-forget posts could be reaped when the hook
  # process exited (silently losing the event), and concurrent posts could
  # arrive out of order. Set UNPEEL_HOOK_POST_SYNC=0 to restore backgrounded
  # posts.
  if [ "${UNPEEL_HOOK_POST_SYNC:-1}" = "1" ]; then
    post_hook_payload "$INPUT" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
    post_hook_payload_to_current_ports "$INPUT" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
  else
    (
      post_hook_payload "$INPUT" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
      post_hook_payload_to_current_ports "$INPUT" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
    ) &
  fi
fi

trace_claude_hook ""

exit 0
