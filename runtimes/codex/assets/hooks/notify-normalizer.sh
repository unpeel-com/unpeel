#!/bin/bash
if [ -n "$1" ]; then
  INPUT="$1"
else
  INPUT=$(cat)
fi

# Codex's notify callback reports its own `type` vocabulary. Normalize that
# provider contract here, inside the Codex runtime package, before handing the
# payload to Unpeel's provider-neutral hook transport.
EVENT_TYPE=$(printf '%s' "$INPUT" | grep -oE '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
if [ -z "$EVENT_TYPE" ]; then
  CODEX_TYPE=$(printf '%s' "$INPUT" | grep -oE '"type"[[:space:]]*:[[:space:]]*"[^"]*"' | grep -oE '"[^"]*"$' | tr -d '"')
  case "$CODEX_TYPE" in
    agent-turn-complete|task_complete)
      EVENT_TYPE="Stop"
      ;;
    turn_aborted)
      EVENT_TYPE="StopCancelled"
      ;;
    task_started)
      EVENT_TYPE="Start"
      ;;
    exec_command_begin)
      EVENT_TYPE="HookSeen"
      ;;
    request_permissions|exec_approval_request|apply_patch_approval_request|approval-requested)
      EVENT_TYPE="PermissionRequest"
      ;;
  esac
fi

if [ "$EVENT_TYPE" = "Interrupt" ]; then
  EVENT_TYPE="StopCancelled"
  INPUT=$(printf '%s' "$INPUT" | sed 's/"hook_event_name"[[:space:]]*:[[:space:]]*"Interrupt"/"hook_event_name":"StopCancelled"/')
fi

[ -n "$EVENT_TYPE" ] || exit 0
if ! printf '%s' "$INPUT" | grep -q '"hook_event_name"[[:space:]]*:'; then
  INPUT=$(printf '%s' "$INPUT" | sed "1s/^[[:space:]]*{/&\"hook_event_name\":\"$EVENT_TYPE\",/")
fi
exec bash "{{NOTIFY_PATH}}" "$INPUT"
