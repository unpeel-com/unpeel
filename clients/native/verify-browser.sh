#!/bin/sh
# Moved to scripts/verify-browser.sh (Host-side Browser MCP smoke test; it drives
# unpeel-host and the engine, not the app — 2026-09-03).
exec "$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)/scripts/verify-browser.sh" "$@"
