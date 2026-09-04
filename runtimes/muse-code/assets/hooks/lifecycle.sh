#!/bin/sh
umask 077
INPUT=$(cat)

# Muse runs plugin hooks with a scrubbed environment (verified 0.1.0: only
# MUSE_PLUGIN_*/PLUGIN_* survive), so the Unpeel identity exported into the
# session PTY never reaches this process directly. The parent of this hook IS
# the muse process, which does carry that environment — recover it from there
# (`ps eww` shows a same-user process's env). Values are UUID/port/path
# shaped; the path patterns stop at whitespace, which every Unpeel-managed
# home satisfies.
recover_parent_env() {
  ps eww $PPID 2>/dev/null | grep -oE "$1=$2" | head -1 | cut -d= -f2-
}
[ -n "${UNPEEL_SESSION_ID:-}" ] \
  || UNPEEL_SESSION_ID="$(recover_parent_env UNPEEL_SESSION_ID '[0-9a-fA-F-]+')"
[ -n "${UNPEEL_APP_PORT:-}" ] \
  || UNPEEL_APP_PORT="$(recover_parent_env UNPEEL_APP_PORT '[0-9]+')"
[ -n "${UNPEEL_SESSION_DIR:-}" ] \
  || UNPEEL_SESSION_DIR="$(recover_parent_env UNPEEL_SESSION_DIR '[^[:space:]]+')"
[ -n "${UNPEEL_APP_PORT_REGISTRY_FILE:-}" ] \
  || UNPEEL_APP_PORT_REGISTRY_FILE="$(recover_parent_env UNPEEL_APP_PORT_REGISTRY_FILE '[^[:space:]]+')"
[ -n "${UNPEEL_HOOK_TRACE_FILE:-}" ] \
  || UNPEEL_HOOK_TRACE_FILE="$(recover_parent_env UNPEEL_HOOK_TRACE_FILE '[^[:space:]]+')"
[ -n "${UNPEEL_RUNTIME_GENERATION:-}" ] \
  || UNPEEL_RUNTIME_GENERATION="$(recover_parent_env UNPEEL_RUNTIME_GENERATION '[0-9]+')"

TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-$HOME/.unpeel/hooks/trace.log}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true
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
  [ -n "${UNPEEL_SESSION_ID:-}" ] || return 0
  _record_dir="${UNPEEL_SESSION_DIR:-$HOME/.unpeel/app-sessions/$UNPEEL_SESSION_ID}"
  [ -d "$_record_dir" ] || return 0
  _record_name_json="$(json_escape_string "$_record_event_name")"
  _record_payload=$(printf '{"hook_event_name":"%s"%s}' "$_record_name_json" "$(runtime_generation_json_field)")
  _record_tmp="$_record_dir/.last-hook-event.json.$$"
  if printf '%s' "$_record_payload" > "$_record_tmp" 2>/dev/null; then
    mv -f "$_record_tmp" "$_record_dir/last-hook-event.json" 2>/dev/null \
      || rm -f "$_record_tmp" 2>/dev/null || true
  fi
}

printf '%s muse-hook session=%s port=%s payload=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$TRACE_PAYLOAD" >> "$TRACE_FILE" 2>/dev/null || true

LAST_EVENT_NAME=$(printf '%s' "$INPUT" | grep -oE '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
LAST_TOOL_NAME=$(printf '%s' "$INPUT" | grep -oE '"tool_name"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')

# Muse fires PermissionRequest for its internal reminder-decision tool; the
# decision is answered inside muse without any user-facing prompt and can
# arrive after Stop, so forwarding it would latch attention with nothing
# left to clear it.
if [ "$LAST_EVENT_NAME" = "PermissionRequest" ] \
  && [ "$LAST_TOOL_NAME" = "submit_reminder_decision" ]; then
  exit 0
fi

INPUT=$(add_runtime_generation_to_payload "$INPUT")

# SessionStart only latches provider metadata (like claude/grok); the durable
# seed records genuine busy/idle/attention transitions.
case "$LAST_EVENT_NAME" in
  UserPromptSubmit|Stop|StopFailure|PermissionRequest)
    record_last_hook_event "$LAST_EVENT_NAME"
    ;;
esac

if [ -n "${UNPEEL_SESSION_ID:-}" ]; then
  (
    # Several Unpeel instances can run at once (e.g. a dev build next to the
    # installed app) and they share the port registry. Post to every known
    # port, not just the first that answers, so the instance that owns this
    # session always receives the event.
    post_hook_payload "$INPUT" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}"
    post_hook_payload_to_current_ports "$INPUT" "$UNPEEL_SESSION_ID" "${UNPEEL_APP_PORT:-}"
  ) &
fi

exit 0
