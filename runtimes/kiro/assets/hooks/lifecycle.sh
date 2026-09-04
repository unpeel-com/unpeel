#!/bin/bash
umask 077
INPUT=$(cat)
SOURCE_EVENT="$1"
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

kiro_transcript_path() {
  _provider_session_id="$1"
  _cwd="$2"
  [ -n "$_provider_session_id" ] || return 1
  _kiro_home="${KIRO_HOME:-$HOME/.kiro}"

  if [ -n "$_cwd" ]; then
    _canonical_cwd=$(cd "$_cwd" 2>/dev/null && pwd -P)
    [ -n "$_canonical_cwd" ] || _canonical_cwd="$_cwd"
    if command -v shasum >/dev/null 2>&1; then
      _cwd_hash=$(printf '%s' "$_canonical_cwd" | shasum -a 256 2>/dev/null | awk '{print substr($1,1,16)}')
    elif command -v openssl >/dev/null 2>&1; then
      _cwd_hash=$(printf '%s' "$_canonical_cwd" | openssl dgst -sha256 2>/dev/null | awk '{print substr($NF,1,16)}')
    fi
    if [ -n "$_cwd_hash" ]; then
      _v3_path="$_kiro_home/sessions/$_cwd_hash/$_provider_session_id/messages.jsonl"
      if [ -f "$_v3_path" ]; then
        printf '%s\n' "$_v3_path"
        return 0
      fi
    fi
  fi

  _v2_path="$_kiro_home/sessions/cli/$_provider_session_id.jsonl"
  if [ -f "$_v2_path" ]; then
    printf '%s\n' "$_v2_path"
    return 0
  fi
  return 1
}

case "$SOURCE_EVENT" in
  SessionStart|agentSpawn) EVENT_TYPE="HookSeen" ;;
  UserPromptSubmit|userPromptSubmit) EVENT_TYPE="UserPromptSubmit" ;;
  PreToolUse|preToolUse|PostToolUse|postToolUse) EVENT_TYPE="Start" ;;
  Stop|stop) EVENT_TYPE="Stop" ;;
  *) exit 0 ;;
esac

PROVIDER_SESSION_ID="$(json_string_value session_id || true)"
PROVIDER_CWD="$(json_string_value cwd || true)"
TOOL_NAME="$(json_string_value tool_name || true)"
TRANSCRIPT_PATH="$(kiro_transcript_path "$PROVIDER_SESSION_ID" "$PROVIDER_CWD" || true)"

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
  UserPromptSubmit|Start|Stop)
    record_last_hook_event "$EVENT_TYPE" "$TOOL_NAME"
    ;;
esac

# V3 hooks are global and also run in Kiro sessions launched outside Unpeel.
# In that case they intentionally do no work beyond returning successfully.
if [ -n "${UNPEEL_SESSION_ID:-}" ]; then
  (
    post_hook_payload "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" || true
    post_hook_payload_to_current_ports "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}"
  ) &
  printf '%s kiro-hook session=%s port=%s event=%s provider_session=%s\n' \
    "$(date '+%Y-%m-%d %H:%M:%S')" \
    "$UNPEEL_SESSION_ID" \
    "${UNPEEL_APP_PORT:-}" \
    "$EVENT_TYPE" \
    "$PROVIDER_SESSION_ID" >> "$TRACE_FILE" 2>/dev/null || true
fi

exit 0
