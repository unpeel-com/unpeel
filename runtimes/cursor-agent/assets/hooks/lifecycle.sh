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

json_string_value() {
  _key="$1"
  printf '%s' "$INPUT" | grep -oE "\"$_key\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" | head -1 | sed 's/.*: *"\([^"]*\)".*/\1/'
}

metadata_fields_json() {
  # Extract bounded string fields in one process and preserve their JSON
  # escapes. Repeated grep/head/sed pipelines per key and per listener can
  # consume the provider's entire hook deadline before HTTP delivery starts.
  _skip_session_id="${1:-false}"
  _metadata_fields="$(printf '%s' "$INPUT" | awk -v skip="$_skip_session_id" '
    {
      rest = $0
      while (match(rest, /"(session_id|chatId|chat_id|provider_session_id|providerSessionID|providerSessionId|thread_id|threadID|threadId|conversation_id|conversationID|conversationId|transcript_path|transcriptPath|provider_transcript_path|providerTranscriptPath|tool_name|prompt_text)"[[:space:]]*:[[:space:]]*"([^"\\]|\\.)*"/)) {
        field = substr(rest, RSTART, RLENGTH)
        rest = substr(rest, RSTART + RLENGTH)
        key = field
        sub(/^"/, "", key)
        sub(/".*/, "", key)
        if (key == "session_id" && (skip == "true" || field ~ /:[[:space:]]*""$/)) continue
        if (!seen[key]++) printf ",%s", field
      }
    }
  ')"
  printf '%s' "$_metadata_fields"
  case "$_metadata_fields" in
    *'"session_id"'*) ;;
    *)
      if [ "$_skip_session_id" != "true" ] && [ -n "${CURSOR_CONVERSATION_ID:-}" ]; then
        printf ',"session_id":"%s"' "$(json_escape_string "$CURSOR_CONVERSATION_ID")"
      fi
      ;;
  esac
}

is_grok_hook() {
  [ -n "${GROK_SESSION_ID:-}" ] && return 0
  [ "$(basename "$0")" = "grok-hook.sh" ] && return 0
  return 1
}

hook_payload() {
  _hook_event_name="$(json_escape_string "$1")"
  if [ -n "${GROK_SESSION_ID:-}" ]; then
    _hook_provider_session_id="$(json_escape_string "$GROK_SESSION_ID")"
    _metadata="$(metadata_fields_json true)"
    _runtime_generation="$(runtime_generation_json_field)"
    printf '{"hook_event_name":"%s","session_id":"%s"%s%s}' "$_hook_event_name" "$_hook_provider_session_id" "$_metadata" "$_runtime_generation"
  else
    _metadata="$(metadata_fields_json false)"
    _runtime_generation="$(runtime_generation_json_field)"
    printf '{"hook_event_name":"%s"%s%s}' "$_hook_event_name" "$_metadata" "$_runtime_generation"
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
  [ -n "$_hook_port" ] || return 1
  _hook_http_code=$(printf '%s' "$HOOK_PAYLOAD" | curl -s -o /dev/null -w '%{http_code}' \
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
  _hook_post_pids=""
  for _hook_candidate_port in $(current_unpeel_ports); do
    [ -n "$_hook_candidate_port" ] || continue
    [ "$_hook_candidate_port" = "$_hook_skip_port" ] && continue
    ( post_hook_event "$_hook_event_name" "$_hook_session_id" "$_hook_candidate_port" || true ) &
    _hook_post_pids="$_hook_post_pids $!"
  done
  for _hook_post_pid in $_hook_post_pids; do
    wait "$_hook_post_pid" || true
  done
  return 0
}

EVENT_TYPE="$1"
NEEDS_RESPONSE=false

# Grok also runs ~/.cursor/hooks.json. Native grok-hook.sh owns that
# session; Cursor Start/Stop here are redundant, and PermissionRequest
# is auto-approved noise under --always-approve.
if is_grok_hook; then
  printf '{"continue":true}\n'
  printf '%s cursor-grok-hook session=%s port=%s event=%s ignored=grok\n' \
    "$(date '+%Y-%m-%d %H:%M:%S')" \
    "${UNPEEL_SESSION_ID:-}" \
    "${UNPEEL_APP_PORT:-}" \
    "${EVENT_TYPE:-}" >> "$TRACE_FILE" 2>/dev/null || true
  exit 0
fi

case "$EVENT_TYPE" in
  Start) ;;
  Stop)
    case "$(json_string_value status || true)" in
      aborted) EVENT_TYPE="StopCancelled" ;;
      error) EVENT_TYPE="StopFailure" ;;
    esac
    ;;
  PermissionRequest)
    NEEDS_RESPONSE=true
    ;;
  *) exit 0 ;;
esac

if [ "$NEEDS_RESPONSE" = "true" ]; then
  printf '{"continue":true}\n'
fi

record_last_hook_event "$EVENT_TYPE" "$(json_string_value tool_name || true)"

HOOK_PAYLOAD="$(hook_payload "$EVENT_TYPE")"
_hook_post_results=""
if [ -n "$UNPEEL_SESSION_ID" ]; then
  # Posts go out synchronously and in order: backgrounded fire-and-forget
  # posts could be reaped when the hook process exited (silently losing the
  # event), and concurrent posts could arrive out of order. Set
  # UNPEEL_HOOK_POST_SYNC=0 to restore backgrounded posts.
  if [ "${UNPEEL_HOOK_POST_SYNC:-1}" = "1" ]; then
    post_hook_event "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
    post_hook_event_to_current_ports "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
  else
    (
      post_hook_event "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
      post_hook_event_to_current_ports "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT" || true
    ) &
  fi
fi

[ -n "$EVENT_TYPE" ] && printf '%s cursor-grok-hook session=%s port=%s event=%s post=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$EVENT_TYPE" \
  "${_hook_post_results:-none}" >> "$TRACE_FILE" 2>/dev/null || true

exit 0
