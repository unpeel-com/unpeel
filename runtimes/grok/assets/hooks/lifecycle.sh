#!/bin/bash
INPUT=$(cat)
# Global provider hooks must be inert outside a hosted Unpeel Session.
[ -n "${UNPEEL_SESSION_ID:-}" ] || exit 0
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-${UNPEEL_HOME:-$HOME/.unpeel}/hooks/trace.log}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true
UNPEEL_PORT_REGISTRY_FILE="${UNPEEL_APP_PORT_REGISTRY_FILE:-${UNPEEL_HOME:-$HOME/.unpeel}/app-ports}"

json_escape_string() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

runtime_generation_json_field() {
  case "${UNPEEL_RUNTIME_GENERATION:-}" in
    ''|*[!0-9]*) return 0 ;;
  esac
  printf ',"unpeel_runtime_generation":%s' "$UNPEEL_RUNTIME_GENERATION"
}

# Persist the last lifecycle event into the session dir so a restarted app can
# re-seed busy/attention state (hooks keep firing while no app is listening).
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

extract_hook_tool_name() {
  for _key in toolName tool_name notificationType notification_type; do
    _candidate=$(printf '%s' "$INPUT" | grep -oE "\"$_key\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" | head -1 | sed 's/.*: *"\([^"]*\)".*/\1/')
    [ -n "$_candidate" ] && {
      printf '%s\n' "$_candidate"
      return 0
    }
  done
  return 1
}

hook_payload() {
  _hook_event_name="$(json_escape_string "$1")"
  _tool_name="${2:-}"
  _runtime_generation="$(runtime_generation_json_field)"
  if [ -n "${GROK_SESSION_ID:-}" ]; then
    _hook_provider_session_id="$(json_escape_string "$GROK_SESSION_ID")"
    if [ -n "$_tool_name" ]; then
      _tool_json="$(json_escape_string "$_tool_name")"
      printf '{"hook_event_name":"%s","tool_name":"%s","session_id":"%s"%s}' \
        "$_hook_event_name" "$_tool_json" "$_hook_provider_session_id" "$_runtime_generation"
    else
      printf '{"hook_event_name":"%s","session_id":"%s"%s}' \
        "$_hook_event_name" "$_hook_provider_session_id" "$_runtime_generation"
    fi
  elif [ -n "$_tool_name" ]; then
    _tool_json="$(json_escape_string "$_tool_name")"
    printf '{"hook_event_name":"%s","tool_name":"%s"%s}' "$_hook_event_name" "$_tool_json" "$_runtime_generation"
  else
    printf '{"hook_event_name":"%s"%s}' "$_hook_event_name" "$_runtime_generation"
  fi
}

# POST one hook event synchronously and record the outcome in
# _hook_post_results ("<port>=<http-code>,..."). Loopback posts finish in
# milliseconds; the tight timeouts bound the worst case when a registry port
# is stale so a hook never burns its whole timeout budget on delivery.
post_hook_event() {
  _hook_event_name="$1"
  _hook_session_id="$2"
  _hook_port="$3"
  _tool_name="${4:-}"
  [ -n "$_hook_port" ] || return 1
  _hook_http_code=$(hook_payload "$_hook_event_name" "$_tool_name" | curl -s -o /dev/null -w '%{http_code}' \
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

post_hook_event_to_current_ports() {
  _hook_event_name="$1"
  _hook_session_id="$2"
  _hook_skip_port="$3"
  _tool_name="${4:-}"
  _hook_post_pids=""
  for _hook_candidate_port in $(current_unpeel_ports); do
    [ -n "$_hook_candidate_port" ] || continue
    [ "$_hook_candidate_port" = "$_hook_skip_port" ] && continue
    ( post_hook_event "$_hook_event_name" "$_hook_session_id" "$_hook_candidate_port" "$_tool_name" || true ) &
    _hook_post_pids="$_hook_post_pids $!"
  done
  for _hook_post_pid in $_hook_post_pids; do
    wait "$_hook_post_pid" || true
  done
  return 0
}

EVENT_TYPE="$1"
TOOL_NAME=""
case "$EVENT_TYPE" in
  HookSeen|Start|UserPromptSubmit|Stop|StopFailure|StopCancelled) ;;
  Attention)
    EVENT_TYPE="PermissionRequest"
    TOOL_NAME="$(extract_hook_tool_name || true)"
    ;;
  *) exit 0 ;;
esac

record_last_hook_event "$EVENT_TYPE" "$TOOL_NAME"

_hook_post_results=""
if [ -n "$UNPEEL_SESSION_ID" ]; then
  # Posts go out synchronously and in order: backgrounded fire-and-forget
  # posts could be reaped when the hook process exited (silently losing the
  # event), and concurrent posts could arrive out of order. Set
  # UNPEEL_HOOK_POST_SYNC=0 to restore backgrounded posts.
  if [ "${UNPEEL_HOOK_POST_SYNC:-1}" = "1" ]; then
    post_hook_event "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" "$TOOL_NAME" || true
    post_hook_event_to_current_ports "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" "$TOOL_NAME" || true
  else
    (
      post_hook_event "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" "$TOOL_NAME" || true
      post_hook_event_to_current_ports "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" "$TOOL_NAME" || true
    ) &
  fi
fi

[ -n "$EVENT_TYPE" ] && printf '%s grok-hook session=%s port=%s event=%s tool=%s post=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$EVENT_TYPE" \
  "${TOOL_NAME:-}" \
  "${_hook_post_results:-none}" >> "$TRACE_FILE" 2>/dev/null || true

exit 0
