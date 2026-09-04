#!/bin/bash
INPUT=$(cat)

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
  for _key in session_id provider_session_id providerSessionID providerSessionId thread_id threadID threadId conversation_id conversationID conversationId transcript_path transcriptPath provider_transcript_path providerTranscriptPath tool_name prompt_text; do
    _value="$(json_string_value "$_key" || true)"
    [ -n "$_value" ] || continue
    _escaped="$(json_escape_string "$_value")"
    printf ',"%s":"%s"' "$_key" "$_escaped"
  done
}

hook_payload() {
  _hook_event_name="$(json_escape_string "$1")"
  _metadata="$(metadata_fields_json)"
  _runtime_generation="$(runtime_generation_json_field)"
  printf '{"hook_event_name":"%s"%s%s}' "$_hook_event_name" "$_metadata" "$_runtime_generation"
}

post_hook_event() {
  _hook_event_name="$1"
  _hook_session_id="$2"
  _hook_port="$3"
  [ -n "$_hook_port" ] || return 1
  hook_payload "$_hook_event_name" | curl -sS --max-time 2 -X POST -H "Content-Type: application/json" \
    -d @- "http://127.0.0.1:$_hook_port/hook/$_hook_session_id" >/dev/null 2>&1
}

EVENT_TYPE="$1"

case "$EVENT_TYPE" in
  sessionStart) EVENT_TYPE="Start" ;;
  sessionEnd) EVENT_TYPE="Stop" ;;
  userPromptSubmitted) EVENT_TYPE="Start" ;;
  postToolUse) EVENT_TYPE="Start" ;;
  preToolUse) EVENT_TYPE="PermissionRequest" ;;
  *)
    printf '{}\n'
    exit 0
    ;;
esac

printf '{}\n'

record_last_hook_event "$EVENT_TYPE" "$(json_string_value tool_name || true)"

if [ -n "$UNPEEL_APP_PORT" ] && [ -n "$UNPEEL_SESSION_ID" ]; then
  (
    post_hook_event "$EVENT_TYPE" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT"
  ) &
fi

exit 0
