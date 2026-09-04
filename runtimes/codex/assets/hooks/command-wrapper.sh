#!/bin/bash
REAL_BIN="${UNPEEL_REAL_CODEX_BIN:-}"
TRACE_FILE="${UNPEEL_HOOK_TRACE_FILE:-$HOME/.unpeel/hooks/trace.log}"
mkdir -p "$(dirname "$TRACE_FILE")" >/dev/null 2>&1 || true
printf '%s codex-wrapper-start session=%s port=%s argv=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "${UNPEEL_APP_PORT:-}" \
  "$*" >> "$TRACE_FILE" 2>/dev/null || true
if [ -z "$REAL_BIN" ]; then
  _unpeel_original_path="${UNPEEL_ORIGINAL_PATH:-$PATH}"
  REAL_BIN="$(PATH="$_unpeel_original_path" command -v codex 2>/dev/null || true)"
fi

# Never recurse into this wrapper, even if a stale shell PATH or explicit
# UNPEEL_REAL_CODEX_BIN points back at it.
if [ -n "$REAL_BIN" ] && [ "$REAL_BIN" -ef "$0" ]; then
  REAL_BIN=""
fi

if [ -z "$REAL_BIN" ] || [ ! -x "$REAL_BIN" ]; then
  echo "Unpeel: failed to resolve real codex binary" >&2
  exit 127
fi

UNPEEL_CODEX_NOTIFY_PATH="{{NOTIFY_PATH}}"
printf '%s codex-wrapper-resolved session=%s real_bin=%s notify=%s mcp_bin=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" \
  "$REAL_BIN" \
  "$UNPEEL_CODEX_NOTIFY_PATH" \
  "${UNPEEL_MCP_BIN:-}" >> "$TRACE_FILE" 2>/dev/null || true

# Register the unified Unpeel MCP server for this launch only (the host
# exports UNPEEL_MCP_BIN when any Unpeel MCP domain is enabled; the server
# advertises only the domains in this session's manifest). Codex spawns MCP
# servers with a minimal environment, so the session identity is passed
# explicitly instead of relying on env inheritance.
UNPEEL_MCP_ARGS=()
if [ -n "${UNPEEL_MCP_BIN:-}" ] && [ -x "${UNPEEL_MCP_BIN:-}" ]; then
  UNPEEL_MCP_ARGS+=(
    -c "mcp_servers.unpeel.command=\"$UNPEEL_MCP_BIN\""
    -c "mcp_servers.unpeel.args=[\"__mcp__\"]"
    -c "mcp_servers.unpeel.env={UNPEEL_SESSION_ID=\"${UNPEEL_SESSION_ID:-}\",UNPEEL_APP_PORT=\"${UNPEEL_APP_PORT:-}\"}"
  )
fi

printf '%s codex-wrapper-exec session=%s\n' \
  "$(date '+%Y-%m-%d %H:%M:%S')" \
  "${UNPEEL_SESSION_ID:-}" >> "$TRACE_FILE" 2>/dev/null || true

if [ "${UNPEEL_WAIT_FOR_ATTACH:-}" = "1" ] && [ -n "${UNPEEL_SESSION_DIR:-}" ]; then
  _unpeel_attach_ready="$UNPEEL_SESSION_DIR/.attach-ready"
  for _unpeel_attach_wait_i in {1..100}; do
    [ -e "$_unpeel_attach_ready" ] && break
    sleep 0.02
  done
fi

exec "$REAL_BIN" "${UNPEEL_MCP_ARGS[@]}" -c "notify=[\"bash\",\"$UNPEEL_CODEX_NOTIFY_PATH\"]" "$@"
