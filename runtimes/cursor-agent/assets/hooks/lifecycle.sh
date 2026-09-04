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

json_string_value() {
  _key="$1"
  printf '%s' "$INPUT" | grep -oE "\"$_key\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" | head -1 | sed 's/.*: *"\([^"]*\)".*/\1/'
}

metadata_fields_json() {
  _skip_session_id="${1:-false}"
  for _key in session_id chatId chat_id provider_session_id providerSessionID providerSessionId thread_id threadID threadId conversation_id conversationID conversationId transcript_path transcriptPath provider_transcript_path providerTranscriptPath tool_name prompt_text; do
    [ "$_skip_session_id" = "true" ] && [ "$_key" = "session_id" ] && continue
    _value="$(json_string_value "$_key" || true)"
    if [ -z "$_value" ] && [ "$_key" = "session_id" ] && [ -n "${CURSOR_CONVERSATION_ID:-}" ]; then
      _value="${CURSOR_CONVERSATION_ID}"
    fi
    [ -n "$_value" ] || continue
    _escaped="$(json_escape_string "$_value")"
    printf ',"%s":"%s"' "$_key" "$_escaped"
  done
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

post_hook_event() {
  _hook_event_name="$1"
  _hook_session_id="$2"
  _hook_port="$3"
  [ -n "$_hook_port" ] || return 1
  hook_payload "$_hook_event_name" | curl -sS --max-time 2 -X POST -H "Content-Type: application/json" \
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
  _hook_any_posted=1
  for _hook_candidate_port in $(current_unpeel_ports); do
    [ -n "$_hook_candidate_port" ] || continue
    [ "$_hook_candidate_port" = "$_hook_skip_port" ] && continue
    if post_hook_event "$_hook_event_name" "$_hook_session_id" "$_hook_candidate_port"; then
      _hook_any_posted=0
    fi
  done
  return $_hook_any_posted
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
  Start|Stop) ;;
  PermissionRequest)
    NEEDS_RESPONSE=true
    ;;
  *) exit 0 ;;
esac

if [ "$NEEDS_RESPONSE" = "true" ]; then
  printf '{"continue":true}\n'
fi

record_last_hook_event "$EVENT_TYPE" "$(json_string_value tool_name || true)"

if [ -n "$UNPEEL_APP_PORT" ] && [ -n "$UNPEEL_SESSION_ID" ]; then
  (
    post_hook_event "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT"
    post_hook_event_to_current_ports "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT"
  ) &
fi

[ -n "$EVENT_TYPE" ] && printf '%s cursor-grok-hook session=%s port=%s event=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$EVENT_TYPE" >> "$TRACE_FILE" 2>/dev/null || true

exit 0
