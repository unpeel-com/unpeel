#!/bin/bash
umask 077
INPUT=$(cat)
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-$HOME/.unpeel/hooks/trace.log}"
UNPEEL_PORT_REGISTRY_FILE="${UNPEEL_APP_PORT_REGISTRY_FILE:-$HOME/.unpeel/app-ports}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true

json_escape_string() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

runtime_generation_json_field() {
  case "${UNPEEL_RUNTIME_GENERATION:-}" in
    ''|*[!0-9]*) return 0 ;;
  esac
  printf ',"unpeel_runtime_generation":%s' "$UNPEEL_RUNTIME_GENERATION"
}

json_string_value() {
  _key="$1"
  printf '%s' "$INPUT" | grep -oE "\"$_key\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" | head -1 | sed 's/.*: *"\([^"]*\)".*/\1/'
}

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

current_unpeel_ports() {
  [ -f "$UNPEEL_PORT_REGISTRY_FILE" ] || return 1
  tr -cs '0-9' '\n' < "$UNPEEL_PORT_REGISTRY_FILE" 2>/dev/null \
    | awk 'NF && !seen[$0]++ { print }'
}

post_hook_payload() {
  _hook_payload="$1"
  _hook_session_id="$2"
  _hook_port="$3"
  [ -n "$_hook_port" ] || return 1
  printf '%s' "$_hook_payload" | curl -sS --max-time 2 -X POST -H "Content-Type: application/json" \
    -d @- "http://127.0.0.1:$_hook_port/hook/$_hook_session_id" >/dev/null 2>&1
}

post_hook_payload_to_current_ports() {
  _hook_payload="$1"
  _hook_session_id="$2"
  _hook_skip_port="$3"
  for _hook_candidate_port in $(current_unpeel_ports); do
    [ -n "$_hook_candidate_port" ] || continue
    [ "$_hook_candidate_port" = "$_hook_skip_port" ] && continue
    post_hook_payload "$_hook_payload" "$_hook_session_id" "$_hook_candidate_port" || true
  done
}

kimi_transcript_path() {
  _provider_session_id="$1"
  _cwd="$2"
  [ -n "$_provider_session_id" ] && [ -n "$_cwd" ] || return 1

  # Standalone Kimi Code (0.x) stores the main Agent's wire transcript below
  # ~/.kimi-code. Session buckets are derived from cwd but their format is an
  # implementation detail, so locate the exact session-id component instead of
  # reproducing the hash/slug algorithm in shell.
  _code_home="${KIMI_CODE_HOME:-$HOME/.kimi-code}"
  if [ -d "$_code_home/sessions" ]; then
    _code_path=$(find "$_code_home/sessions" -type f \
      -path "*/$_provider_session_id/agents/main/wire.jsonl" \
      -print -quit 2>/dev/null)
    if [ -n "$_code_path" ] && [ -f "$_code_path" ]; then
      printf '%s\n' "$_code_path"
      return 0
    fi
  fi

  # Legacy Python Kimi CLI stores context.jsonl below ~/.kimi.
  if command -v md5 >/dev/null 2>&1; then
    _cwd_hash=$(printf '%s' "$_cwd" | md5 -q 2>/dev/null)
  elif command -v md5sum >/dev/null 2>&1; then
    _cwd_hash=$(printf '%s' "$_cwd" | md5sum 2>/dev/null | awk '{print $1}')
  else
    return 1
  fi
  [ -n "$_cwd_hash" ] || return 1
  _share_dir="${KIMI_SHARE_DIR:-$HOME/.kimi}"
  _path="$_share_dir/sessions/$_cwd_hash/$_provider_session_id/context.jsonl"
  [ -f "$_path" ] || return 1
  printf '%s\n' "$_path"
}

EVENT_TYPE="$1"
TOOL_NAME=""
case "$EVENT_TYPE" in
  HookSeen|UserPromptSubmit|Stop|StopFailure) ;;
  SessionEnd) EVENT_TYPE="Stop" ;;
  Attention)
    EVENT_TYPE="PermissionRequest"
    TOOL_NAME="$(json_string_value tool_name || true)"
    if [ -z "$TOOL_NAME" ]; then
      TOOL_NAME="$(json_string_value notification_type || true)"
    fi
    ;;
  *) exit 0 ;;
esac

PROVIDER_SESSION_ID="$(json_string_value session_id || true)"
PROVIDER_CWD="$(json_string_value cwd || true)"
TRANSCRIPT_PATH="$(kimi_transcript_path "$PROVIDER_SESSION_ID" "$PROVIDER_CWD" || true)"

EVENT_JSON="$(json_escape_string "$EVENT_TYPE")"
PAYLOAD=$(printf '{"hook_event_name":"%s"' "$EVENT_JSON")
if [ -n "$TOOL_NAME" ]; then
  PAYLOAD="$PAYLOAD$(printf ',"tool_name":"%s"' "$(json_escape_string "$TOOL_NAME")")"
fi
if [ -n "$PROVIDER_SESSION_ID" ]; then
  PAYLOAD="$PAYLOAD$(printf ',"session_id":"%s"' "$(json_escape_string "$PROVIDER_SESSION_ID")")"
fi
if [ -n "$TRANSCRIPT_PATH" ]; then
  PAYLOAD="$PAYLOAD$(printf ',"transcript_path":"%s"' "$(json_escape_string "$TRANSCRIPT_PATH")")"
fi
PAYLOAD="$PAYLOAD$(runtime_generation_json_field)"
PAYLOAD="$PAYLOAD}"

case "$EVENT_TYPE" in
  UserPromptSubmit|Stop|StopFailure|PermissionRequest)
    record_last_hook_event "$EVENT_TYPE" "$TOOL_NAME"
    ;;
esac

if [ -n "${UNPEEL_SESSION_ID:-}" ]; then
  (
    post_hook_payload "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" || true
    post_hook_payload_to_current_ports "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}"
  ) &
fi

printf '%s kimi-hook session=%s port=%s event=%s provider_session=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$EVENT_TYPE" \
  "$PROVIDER_SESSION_ID" >> "$TRACE_FILE" 2>/dev/null || true

exit 0
