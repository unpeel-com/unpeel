#!/bin/bash
INPUT=$(cat)
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-$HOME/.unpeel/hooks/trace.log}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true
UNPEEL_PORT_REGISTRY_FILE="${UNPEEL_APP_PORT_REGISTRY_FILE:-$HOME/.unpeel/app-ports}"

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
  _record_dir="${UNPEEL_SESSION_DIR:-$HOME/.unpeel/app-sessions/$UNPEEL_SESSION_ID}"
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

post_hook_event() {
  _hook_event_name="$1"
  _hook_session_id="$2"
  _hook_port="$3"
  _tool_name="${4:-}"
  [ -n "$_hook_port" ] || return 1
  hook_payload "$_hook_event_name" "$_tool_name" | curl -sS --max-time 2 -X POST -H "Content-Type: application/json" \
    -d @- "http://127.0.0.1:$_hook_port/hook/$_hook_session_id" >/dev/null 2>&1
}

current_unpeel_ports() {
  [ -f "$UNPEEL_PORT_REGISTRY_FILE" ] || return 1
  tr -cs '0-9' '\n' < "$UNPEEL_PORT_REGISTRY_FILE" 2>/dev/null \
    | awk 'NF && !seen[$0]++ { print }'
}

post_hook_event_to_current_ports() {
  _hook_event_name="$1"
  _hook_session_id="$2"
  _hook_skip_port="$3"
  _tool_name="${4:-}"
  _hook_any_posted=1
  for _hook_candidate_port in $(current_unpeel_ports); do
    [ -n "$_hook_candidate_port" ] || continue
    [ "$_hook_candidate_port" = "$_hook_skip_port" ] && continue
    if post_hook_event "$_hook_event_name" "$_hook_session_id" "$_hook_candidate_port" "$_tool_name"; then
      _hook_any_posted=0
    fi
  done
  return $_hook_any_posted
}

EVENT_TYPE="$1"
TOOL_NAME=""
case "$EVENT_TYPE" in
  HookSeen|Start|UserPromptSubmit|Stop) ;;
  Attention)
    EVENT_TYPE="PermissionRequest"
    TOOL_NAME="$(extract_hook_tool_name || true)"
    ;;
  *) exit 0 ;;
esac

record_last_hook_event "$EVENT_TYPE" "$TOOL_NAME"

if [ -n "$UNPEEL_SESSION_ID" ]; then
  (
    post_hook_event "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" "$TOOL_NAME"
    post_hook_event_to_current_ports "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" "$TOOL_NAME"
  ) &
fi

[ -n "$EVENT_TYPE" ] && printf '%s grok-hook session=%s port=%s event=%s tool=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$EVENT_TYPE" \
  "${TOOL_NAME:-}" >> "$TRACE_FILE" 2>/dev/null || true

exit 0
