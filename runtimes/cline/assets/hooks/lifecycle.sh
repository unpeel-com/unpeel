#!/bin/bash
umask 077
SOURCE_EVENT="$1"
INPUT="${2:-$(cat)}"
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
  _record_payload=$(printf '{"hook_event_name":"%s"' "$(json_escape_string "$_record_event_name")")
  if [ -n "$_record_tool_name" ]; then
    _record_payload="$_record_payload$(printf ',"tool_name":"%s"' "$(json_escape_string "$_record_tool_name")")"
  fi
  _record_payload="$_record_payload$(runtime_generation_json_field)"
  _record_payload="$_record_payload}"
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

case "$SOURCE_EVENT" in
  # Cline 3.0.44 does not dispatch UserPromptSubmit for the initial CLI prompt.
  # TaskStart/TaskResume occur when a run actually begins, so they are the
  # reliable busy edge as well as the first place the persisted session id is
  # available.
  TaskStart|TaskResume) EVENT_TYPE="UserPromptSubmit" ;;
  UserPromptSubmit) EVENT_TYPE="UserPromptSubmit" ;;
  PreToolUse|PostToolUse) EVENT_TYPE="Start" ;;
  TaskComplete|TaskCancel|SessionShutdown) EVENT_TYPE="Stop" ;;
  TaskError) EVENT_TYPE="StopFailure" ;;
  *) exit 0 ;;
esac

PROVIDER_SESSION_ID="$(json_string_value rootSessionId || true)"
[ -n "$PROVIDER_SESSION_ID" ] || PROVIDER_SESSION_ID="$(json_string_value taskId || true)"
PROVIDER_CWD="$(json_string_value rootPath || true)"
if [ -z "$PROVIDER_CWD" ]; then
  PROVIDER_CWD="$(printf '%s' "$INPUT" | grep -oE '"workspaceRoots"[[:space:]]*:[[:space:]]*\[[[:space:]]*"[^"]*"' | head -1 | sed 's/.*\[[[:space:]]*"\([^"]*\)".*/\1/' || true)"
fi
TOOL_NAME="$(json_string_value toolName || true)"
TRANSCRIPT_PATH=""
if [ -n "$PROVIDER_SESSION_ID" ]; then
  _sessions_root="${CLINE_SESSION_DATA_DIR:-}"
  if [ -z "$_sessions_root" ]; then
    _data_root="${CLINE_DATA_DIR:-${CLINE_DIR:-$HOME/.cline}/data}"
    _sessions_root="$_data_root/sessions"
  fi
  _candidate="$_sessions_root/$PROVIDER_SESSION_ID/$PROVIDER_SESSION_ID.messages.json"
  [ -f "$_candidate" ] && TRANSCRIPT_PATH="$_candidate"
fi

PAYLOAD=$(printf '{"hook_event_name":"%s"' "$(json_escape_string "$EVENT_TYPE")")
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
  UserPromptSubmit|Start|Stop|StopFailure)
    record_last_hook_event "$EVENT_TYPE" "$TOOL_NAME"
    ;;
esac

# Cline's hook files are global. Outside an Unpeel-hosted terminal they must be
# silent no-ops so ordinary Cline sessions keep their native behavior.
if [ -n "${UNPEEL_SESSION_ID:-}" ]; then
  (
    post_hook_payload "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" || true
    post_hook_payload_to_current_ports "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}"
  ) &
  printf '%s cline-hook session=%s port=%s event=%s provider_session=%s\n' \
    "$(date '+%Y-%m-%d %H:%M:%S')" \
    "$UNPEEL_SESSION_ID" \
    "${UNPEEL_APP_PORT:-}" \
    "$EVENT_TYPE" \
    "$PROVIDER_SESSION_ID" >> "$TRACE_FILE" 2>/dev/null || true
fi

exit 0
