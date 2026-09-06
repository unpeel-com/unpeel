#!/bin/bash
umask 077
INPUT=$(cat)
SOURCE_EVENT="$1"
# Global provider hooks must be inert outside a hosted Unpeel Session.
[ -n "${UNPEEL_SESSION_ID:-}" ] || exit 0
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-${UNPEEL_HOME:-$HOME/.unpeel}/hooks/trace.log}"
UNPEEL_PORT_REGISTRY_FILE="${UNPEEL_APP_PORT_REGISTRY_FILE:-${UNPEEL_HOME:-$HOME/.unpeel}/app-ports}"
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

current_unpeel_ports() {
  [ -f "$UNPEEL_PORT_REGISTRY_FILE" ] || return 1
  awk '/^[[:space:]]*[0-9]+[[:space:]]*$/ && $1 > 0 && $1 <= 65535 && !seen[$1 + 0]++ { print $1 + 0 }' \
    "$UNPEEL_PORT_REGISTRY_FILE" 2>/dev/null
}

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
  SessionStart|agentSpawn|AgentSpawn) EVENT_TYPE="HookSeen" ;;
  UserPromptSubmit|userPromptSubmit) EVENT_TYPE="UserPromptSubmit" ;;
  PreToolUse|preToolUse|PostToolUse|postToolUse) EVENT_TYPE="HookSeen" ;;
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
_hook_post_results=""
if [ -n "${UNPEEL_SESSION_ID:-}" ]; then
  # Posts go out synchronously and in order: backgrounded fire-and-forget
  # posts could be reaped when the hook process exited (silently losing the
  # event), and concurrent posts could arrive out of order. Set
  # UNPEEL_HOOK_POST_SYNC=0 to restore backgrounded posts.
  if [ "${UNPEEL_HOOK_POST_SYNC:-1}" = "1" ]; then
    post_hook_payload "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" || true
    post_hook_payload_to_current_ports "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" || true
  else
    (
      post_hook_payload "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" || true
      post_hook_payload_to_current_ports "$PAYLOAD" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}" || true
    ) &
  fi
  printf '%s kiro-hook session=%s port=%s event=%s provider_session=%s post=%s\n' \
    "$(date '+%Y-%m-%d %H:%M:%S')" \
    "$UNPEEL_SESSION_ID" \
    "${UNPEEL_APP_PORT:-}" \
    "$EVENT_TYPE" \
    "$PROVIDER_SESSION_ID" \
    "${_hook_post_results:-none}" >> "$TRACE_FILE" 2>/dev/null || true
fi

exit 0
