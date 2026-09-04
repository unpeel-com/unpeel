#!/bin/bash
if [ "$1" = "read" ] && { [ "$2" = "-g" ] || [ "$2" = "NSGlobalDomain" ]; } && [ "$3" = "AppleInterfaceStyle" ]; then
  APPEARANCE_FILE="${UNPEEL_APP_APPEARANCE_FILE:-$HOME/.unpeel/app-appearance}"
  MODE="$(tr -d '[:space:]' < "$APPEARANCE_FILE" 2>/dev/null | head -c 16)"
  if [ -z "$MODE" ]; then
    MODE="${UNPEEL_GROK_APP_APPEARANCE:-}"
  fi
  case "$MODE" in
    dark|Dark)
      printf 'Dark\n'
      exit 0
      ;;
    light|Light)
      # macOS light mode is represented by AppleInterfaceStyle being unset.
      exit 1
      ;;
  esac
fi

exec /usr/bin/defaults "$@"
