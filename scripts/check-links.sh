#!/bin/sh
# Verify that relative Markdown links in the public-facing docs resolve:
# the root docs plus every docs/agents page README links (the public
# documentation boundary — a link from a kept page into a moved design
# record must fail here, not on GitHub).
set -eu
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
status=0
files="README.md CONTRIBUTING.md NOTICE.md TRADEMARK.md AGENTS.md SECURITY.md runtimes/README.md"
for page in "$repo_root"/docs/agents/*.md; do
  [ -e "$page" ] && files="$files docs/agents/$(basename "$page")"
done
for file in $files; do
  [ -e "$repo_root/$file" ] || continue
  dir=$(dirname -- "$file")
  grep -oE '\]\(([^)#]+)' "$repo_root/$file" | sed 's/^](//' | grep -vE '^(https?:|mailto:)' | sort -u | while read -r target; do
    case "$target" in
      /*) resolved="$repo_root$target" ;;
      *) resolved="$repo_root/$dir/$target" ;;
    esac
    if [ ! -e "$resolved" ]; then
      echo "broken link in $file: $target" >&2
      exit 1
    fi
  done || status=1
done
[ "$status" = 0 ] && echo "links ok"
exit $status
