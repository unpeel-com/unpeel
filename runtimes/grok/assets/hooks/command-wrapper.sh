#!/bin/bash
set -e

wrapper_realpath() {
  cd "$(dirname "$1")" >/dev/null 2>&1 && printf '%s/%s\n' "$(pwd -P)" "$(basename "$1")"
}

find_real_grok() {
  if [ -n "${UNPEEL_REAL_GROK_BIN:-}" ] && [ -x "$UNPEEL_REAL_GROK_BIN" ]; then
    printf '%s\n' "$UNPEEL_REAL_GROK_BIN"
    return 0
  fi

  _wrapper_path="$(wrapper_realpath "$0")"
  _old_ifs="$IFS"
  IFS=:
  for _dir in ${PATH:-}; do
    [ -n "$_dir" ] || _dir="."
    _candidate="$_dir/grok"
    [ -x "$_candidate" ] || continue
    _candidate_path="$(wrapper_realpath "$_candidate")"
    [ "$_candidate_path" = "$_wrapper_path" ] && continue
    printf '%s\n' "$_candidate"
    IFS="$_old_ifs"
    return 0
  done
  IFS="$_old_ifs"

  _home="${GROK_HOME:-$HOME/.grok}"
  if [ -x "$_home/bin/grok" ]; then
    printf '%s\n' "$_home/bin/grok"
    return 0
  fi
  return 1
}

toml_ui_value() {
  _key="$1"
  _file="$2"
  [ -f "$_file" ] || return 1
  awk -v key="$_key" '
    /^[[:space:]]*\[[^]]+\][[:space:]]*$/ {
      in_ui = ($0 ~ /^[[:space:]]*\[ui\][[:space:]]*$/)
      next
    }
    in_ui {
      line = $0
      sub(/[[:space:]]+#.*/, "", line)
      if (line ~ "^[[:space:]]*" key "[[:space:]]*=") {
        sub("^[^=]*=[[:space:]]*", "", line)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
        gsub(/^\"|\"$/, "", line)
        print line
        exit
      }
    }
  ' "$_file"
}

normalize_theme_name() {
  printf '%s' "$1" | tr '[:upper:]_' '[:lower:]-'
}

write_resolved_config() {
  _source="$1"
  _target="$2"
  _dest="$3"
  if [ ! -f "$_source" ]; then
    {
      printf '[ui]\n'
      printf 'theme = "%s"\n' "$_target"
    } > "$_dest"
    return 0
  fi

  awk -v target="$_target" '
    BEGIN { in_ui = 0; saw_ui = 0; wrote_theme = 0 }
    /^[[:space:]]*\[[^]]+\][[:space:]]*$/ {
      if (in_ui && !wrote_theme) {
        print "theme = \"" target "\""
        wrote_theme = 1
      }
      in_ui = ($0 ~ /^[[:space:]]*\[ui\][[:space:]]*$/)
      if (in_ui) saw_ui = 1
      print
      next
    }
    in_ui && $0 ~ /^[[:space:]]*theme[[:space:]]*=/ {
      print "theme = \"" target "\""
      wrote_theme = 1
      next
    }
    { print }
    END {
      if (!saw_ui) {
        print ""
        print "[ui]"
        print "theme = \"" target "\""
      } else if (in_ui && !wrote_theme) {
        print "theme = \"" target "\""
      }
    }
  ' "$_source" > "$_dest"
}

disable_compat_vendor_hooks() {
  # Grok interpolates $VAR in hook command strings and refuses to run the
  # hook when the var is unset. Claude/Cursor settings often use that as a
  # skip-if-missing check. Native unpeel.json owns
  # hosted lifecycle, so hosted Grok must not scan those vendor files.
  export GROK_CLAUDE_HOOKS_ENABLED=false
  export GROK_CURSOR_HOOKS_ENABLED=false
}

append_compat_hook_disable() {
  {
    printf '\n'
    printf '[compat.claude]\n'
    printf 'hooks = false\n'
    printf '[compat.cursor]\n'
    printf 'hooks = false\n'
  } >> "$1"
}

prepare_grok_home_overlay() {
  _mode="${UNPEEL_GROK_APP_APPEARANCE:-}"
  _session="${UNPEEL_SESSION_ID:-}"
  case "$_mode" in
    light|Light) _auto_key="auto_light_theme"; _fallback_theme="grokday" ;;
    dark|Dark) _auto_key="auto_dark_theme"; _fallback_theme="groknight" ;;
    *) _auto_key=""; _fallback_theme="" ;;
  esac

  _real_home="${GROK_HOME:-$HOME/.grok}"
  _real_config="$_real_home/config.toml"
  _theme="$(normalize_theme_name "$(toml_ui_value theme "$_real_config" || true)")"
  _resolve_theme=0
  if [ -n "$_auto_key" ]; then
    case "$_theme" in
      auto|system) _resolve_theme=1 ;;
    esac
  fi

  # Overlay for theme resolution and/or hosted compat-hook disable.
  if [ -z "$_session" ] && [ "$_resolve_theme" != 1 ]; then
    return 1
  fi

  _overlay_id="${_session:-$$}"
  _overlay="${UNPEEL_HOME:-$HOME/.unpeel}/hooks/grok-home/$_overlay_id"
  rm -rf "$_overlay"
  mkdir -p "$_overlay"

  for _entry in "$_real_home"/* "$_real_home"/.[!.]* "$_real_home"/..?*; do
    [ -e "$_entry" ] || continue
    _base="$(basename "$_entry")"
    case "$_base" in
      config.toml|leader.sock|leader.pid|*.sock) continue ;;
    esac
    [ -e "$_overlay/$_base" ] || ln -s "$_entry" "$_overlay/$_base" 2>/dev/null || true
  done

  if [ "$_resolve_theme" = 1 ]; then
    _target="$(toml_ui_value "$_auto_key" "$_real_config" || true)"
    [ -n "$_target" ] || _target="$_fallback_theme"
    _target="$(normalize_theme_name "$_target")"
    write_resolved_config "$_real_config" "$_target" "$_overlay/config.toml"
  elif [ -f "$_real_config" ]; then
    cp "$_real_config" "$_overlay/config.toml"
  else
    : > "$_overlay/config.toml"
  fi
  append_compat_hook_disable "$_overlay/config.toml"
  export GROK_HOME="$_overlay"
  disable_compat_vendor_hooks
  return 0
}

REAL_GROK="$(find_real_grok)" || {
  echo "unpeel grok wrapper: could not find real grok binary" >&2
  exit 127
}

# Hosted sessions disable Claude/Cursor hook scans even if overlay creation
# is skipped, so unset vendor env vars cannot paint a red hook error.
if [ -n "${UNPEEL_SESSION_ID:-}" ]; then
  disable_compat_vendor_hooks
fi
prepare_grok_home_overlay || true
exec "$REAL_GROK" "$@"
