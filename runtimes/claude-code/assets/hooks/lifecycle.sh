#!/bin/bash
umask 077
INPUT=$(cat)
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-$HOME/.unpeel/hooks/trace.log}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true
# Cap trace growth so the log can never grow without bound.
if [ -f "$TRACE_FILE" ]; then
  _unpeel_trace_size=$(wc -c < "$TRACE_FILE" 2>/dev/null | tr -d ' ')
  if [ -n "$_unpeel_trace_size" ] && [ "$_unpeel_trace_size" -gt 10485760 ]; then
    mv -f "$TRACE_FILE" "$TRACE_FILE.1" 2>/dev/null || true
  fi
fi
# The payload contains the user's prompt text; only log it when explicitly
# opted in. Otherwise record that an event fired without its contents.
if [ "${UNPEEL_HOOK_TRACE_VERBOSE:-}" = "1" ]; then
  TRACE_PAYLOAD="$INPUT"
else
  TRACE_PAYLOAD="<redacted; UNPEEL_HOOK_TRACE_VERBOSE=1 to log>"
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

printf '%s claude-hook session=%s port=%s payload=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$TRACE_PAYLOAD" >> "$TRACE_FILE" 2>/dev/null || true

# Grok scans ~/.claude/settings.json for compatibility and injects
# GROK_SESSION_ID on every hook. Unpeel's grok-hook.sh already maps
# Grok-native events. Forwarding Grok's camelCase session_start here
# is normalized to a busy Start and spins the sidebar from launch;
# Grok's idle TUI then re-arms that busy state forever.
if [ -n "${GROK_SESSION_ID:-}" ]; then
  printf '%s claude-hook session=%s port=%s ignored=grok\n' \
    "$(date '+%Y-%m-%d %H:%M:%S')" \
    "${UNPEEL_SESSION_ID:-}" \
    "${UNPEEL_APP_PORT:-}" >> "$TRACE_FILE" 2>/dev/null || true
  exit 0
fi

LAST_EVENT_NAME=$(printf '%s' "$INPUT" | grep -oE '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
if [ -z "$LAST_EVENT_NAME" ]; then
  LAST_EVENT_NAME=$(printf '%s' "$INPUT" | grep -oE '"hookEventName"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
fi
LAST_TOOL_NAME=$(printf '%s' "$INPUT" | grep -oE '"tool_name"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | grep -oE '"[^"]*"$' | tr -d '"')
if [ -z "$LAST_TOOL_NAME" ]; then
  LAST_TOOL_NAME=$(printf '%s' "$INPUT" | grep -oE '"toolName"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | grep -oE '"[^"]*"$' | tr -d '"')
fi

# SessionStart fires at launch and on in-tool /resume, /clear, /compact,
# carrying the (new) session_id + transcript_path. Forward it as HookSeen so
# it only latches provider metadata — posted verbatim the server would
# treat a Claude-shaped SessionStart as busy. This is what re-links an
# Unpeel session to the conversation the user resumed inside claude,
# before any prompt is typed. Also accept Grok/Cursor camelCase names.
case "$LAST_EVENT_NAME" in
  SessionStart|session_start|sessionStart)
    INPUT=$(printf '%s' "$INPUT" | sed \
      -e 's/"hook_event_name"[[:space:]]*:[[:space:]]*"SessionStart"/"hook_event_name":"HookSeen"/' \
      -e 's/"hookEventName"[[:space:]]*:[[:space:]]*"session_start"/"hookEventName":"HookSeen"/' \
      -e 's/"hookEventName"[[:space:]]*:[[:space:]]*"SessionStart"/"hookEventName":"HookSeen"/' \
      -e 's/"hookEventName"[[:space:]]*:[[:space:]]*"sessionStart"/"hookEventName":"HookSeen"/')
    LAST_EVENT_NAME="HookSeen"
    ;;
esac

INPUT=$(add_runtime_generation_to_payload "$INPUT")

case "$LAST_EVENT_NAME" in
  Start|UserPromptSubmit|Stop|StopFailure|PermissionRequest)
    record_last_hook_event "$LAST_EVENT_NAME" "$LAST_TOOL_NAME"
    ;;
esac

if [ -n "$UNPEEL_SESSION_ID" ]; then
  (
    # Several Unpeel instances can run at once (e.g. a dev build next to the
    # installed app) and they share the port registry. Post to every known
    # port, not just the first that answers, so the instance that owns this
    # session always receives the event.
    post_hook_payload "$INPUT" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT"
    post_hook_payload_to_current_ports "$INPUT" "$UNPEEL_SESSION_ID" "$UNPEEL_APP_PORT"
  ) &
fi

exit 0
