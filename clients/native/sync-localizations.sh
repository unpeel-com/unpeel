#!/usr/bin/env bash
# sync-localizations.sh — compile Localizable.xcstrings → zh-Hans.lproj
# for the SwiftPM resource bundle.
#
# String Catalog is the source of truth (editable in Xcode). SwiftPM's
# executable-target resource pipeline only copies .xcstrings into the
# bundle; Bundle lookups need compiled .lproj/.strings. Run this before
# `swift build` / build-app.sh (build-app.sh invokes it automatically).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CATALOG="$HERE/UnpeelNative/Localization/Localizable.xcstrings"
OUT_RES="$HERE/UnpeelNative/Sources/UnpeelNative/Resources"

[ -f "$CATALOG" ] || {
  echo "FAIL: missing $CATALOG" >&2
  exit 1
}

XCSTRINGSTOOL="$(xcrun --find xcstringstool)"
rm -rf "$OUT_RES/zh-Hans.lproj"
"$XCSTRINGSTOOL" compile \
  --output-directory "$OUT_RES" \
  --language zh-Hans \
  --serialization-format binary \
  "$CATALOG"

[ -f "$OUT_RES/zh-Hans.lproj/Localizable.strings" ] || {
  echo "FAIL: xcstringstool did not produce zh-Hans.lproj/Localizable.strings" >&2
  exit 1
}

echo "Synced $OUT_RES/zh-Hans.lproj/Localizable.strings from Localization/Localizable.xcstrings"
