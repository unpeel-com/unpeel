#!/bin/sh
# Build the native-app-only third-party notice file from checksum-pinned
# Swift and embedded-framework license sources.
set -eu

native_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest="$native_dir/licenses/swift-notices.manifest"
output=${1:?usage: collect-swift-notices.sh OUTPUT}
output_dir=$(dirname -- "$output")
mkdir -p "$output_dir"

tmp=$(mktemp "${TMPDIR:-/tmp}/unpeel-swift-notices.XXXXXX")
trap 'rm -f "$tmp"' EXIT INT TERM

cat > "$tmp" <<'HEADER'
UNPEEL THIRD-PARTY NOTICES — NATIVE APP / SWIFT

This file is generated deterministically from the checksum-pinned manifest
shipped with Unpeel. These dependencies are specific to the native app and are
not included in CLI archives.
HEADER

section=0
while IFS='|' read -r label relative_path expected_sha256 extra; do
  case "$label" in
    ''|'#'*) continue ;;
  esac
  if [ -n "${extra:-}" ] || [ -z "$relative_path" ] || [ -z "$expected_sha256" ]; then
    echo "FAIL: malformed Swift notice manifest row: $label" >&2
    exit 1
  fi
  source_path="$native_dir/$relative_path"
  if [ ! -s "$source_path" ]; then
    echo "FAIL: missing native license source for $label: $source_path" >&2
    exit 1
  fi
  actual_sha256=$(shasum -a 256 "$source_path" | awk '{print $1}')
  if [ "$actual_sha256" != "$expected_sha256" ]; then
    echo "FAIL: native license checksum mismatch for $label" >&2
    echo "      expected $expected_sha256" >&2
    echo "      actual   $actual_sha256 ($source_path)" >&2
    exit 1
  fi
  section=$((section + 1))
  {
    printf '\n================================================================================\n'
    printf 'NOTICE %s\n' "$section"
    printf 'Package: %s\n' "$label"
    printf 'Source file: %s\n' "$relative_path"
    printf '%s\n' '--------------------------------------------------------------------------------'
    cat "$source_path"
    printf '\n'
  } >> "$tmp"
done < "$manifest"

if [ "$section" -eq 0 ]; then
  echo "FAIL: native license manifest is empty: $manifest" >&2
  exit 1
fi

mv "$tmp" "$output"
trap - EXIT INT TERM
echo "Wrote $section native-app license texts to $output"
