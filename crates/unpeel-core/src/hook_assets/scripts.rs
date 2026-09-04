//! Provider-neutral hook transport shared by runtime-owned setup adapters.

pub(crate) const NOTIFY_HOOK_SCRIPT: &str = r#"#!/bin/bash
umask 077
if [ -n "$1" ]; then
  INPUT="$1"
else
  INPUT=$(cat)
fi
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-$HOME/.unpeel/hooks/trace.log}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true
if [ -f "$TRACE_FILE" ]; then
  _unpeel_trace_size=$(wc -c < "$TRACE_FILE" 2>/dev/null | tr -d ' ')
  if [ -n "$_unpeel_trace_size" ] && [ "$_unpeel_trace_size" -gt 10485760 ]; then
    mv -f "$TRACE_FILE" "$TRACE_FILE.1" 2>/dev/null || true
  fi
fi
UNPEEL_PORT_REGISTRY_FILE="${UNPEEL_APP_PORT_REGISTRY_FILE:-$HOME/.unpeel/app-ports}"

post_hook_payload() {
  _hook_payload="$1"
  _hook_session_id="$2"
  _hook_port="$3"
  [ -n "$_hook_port" ] || return 1
  printf '%s' "$_hook_payload" | curl -sS --max-time 2 -X POST -H "Content-Type: application/json" \
    -d @- "http://127.0.0.1:$_hook_port/hook/$_hook_session_id" >/dev/null 2>&1
}

current_unpeel_ports() {
  [ -f "$UNPEEL_PORT_REGISTRY_FILE" ] || return 1
  tr -cs '0-9' '\n' < "$UNPEEL_PORT_REGISTRY_FILE" 2>/dev/null \
    | awk 'NF && !seen[$0]++ { print }'
}

post_hook_payload_to_current_ports() {
  _hook_payload="$1"
  _hook_session_id="$2"
  _hook_skip_port="$3"
  _hook_any_posted=1
  for _hook_candidate_port in $(current_unpeel_ports); do
    [ -n "$_hook_candidate_port" ] || continue
    [ "$_hook_candidate_port" = "$_hook_skip_port" ] && continue
    if post_hook_payload "$_hook_payload" "$_hook_session_id" "$_hook_candidate_port"; then
      _hook_any_posted=0
    fi
  done
  return $_hook_any_posted
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

add_hook_event_name_to_payload() {
  _hook_event_name="$(json_escape_string "$1")"
  _hook_payload="$2"
  if printf '%s' "$_hook_payload" | grep -q '^[[:space:]]*{'; then
    printf '%s' "$_hook_payload" | sed "1s/^[[:space:]]*{/&\"hook_event_name\":\"$_hook_event_name\",/"
  else
    printf '{"hook_event_name":"%s"}' "$_hook_event_name"
  fi
}

EVENT_TYPE=$(printf '%s' "$INPUT" | grep -oE '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
[ "$EVENT_TYPE" = "UserPromptSubmit" ] && EVENT_TYPE="Start"
if [ "${UNPEEL_HOOK_TRACE_VERBOSE:-}" = "1" ]; then
  TRACE_RAW="$INPUT"
else
  TRACE_RAW="<redacted; UNPEEL_HOOK_TRACE_VERBOSE=1 to log>"
fi
[ -n "$EVENT_TYPE" ] && printf '%s notify-hook session=%s port=%s event=%s raw=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$EVENT_TYPE" \
  "$TRACE_RAW" >> "$TRACE_FILE" 2>/dev/null || true
[ -z "$EVENT_TYPE" ] && exit 0

LAST_TOOL_NAME=$(printf '%s' "$INPUT" | grep -oE '"tool_name"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | grep -oE '"[^"]*"$' | tr -d '"')
record_last_hook_event "$EVENT_TYPE" "$LAST_TOOL_NAME"

HOOK_PAYLOAD="$INPUT"
if ! printf '%s' "$HOOK_PAYLOAD" | grep -q '"hook_event_name"[[:space:]]*:'; then
  HOOK_PAYLOAD=$(add_hook_event_name_to_payload "$EVENT_TYPE" "$HOOK_PAYLOAD")
fi
HOOK_PAYLOAD=$(add_runtime_generation_to_payload "$HOOK_PAYLOAD")

if [ -n "$UNPEEL_SESSION_ID" ]; then
  post_to_unpeel() {
    # Several Unpeel instances can run at once (e.g. a dev build next to the
    # installed app) and they share the port registry. Post to every known
    # port, not just the first that answers, so the instance that owns this
    # session always receives the event.
    post_hook_payload "$HOOK_PAYLOAD" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT"
    post_hook_payload_to_current_ports "$HOOK_PAYLOAD" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT"
  }
  if [ "${UNPEEL_HOOK_POST_SYNC:-}" = "1" ]; then
    post_to_unpeel
  else
    ( post_to_unpeel ) &
  fi
fi

exit 0
"#;
