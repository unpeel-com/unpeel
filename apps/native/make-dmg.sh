#!/bin/bash
#
# make-dmg.sh — package dist/Unpeel.app into a drag-to-install DMG.
#
# Produces apps/native/dist/Unpeel.dmg: a Finder window holding Unpeel.app
# next to an /Applications alias, so the user drags the app across to install.
# Run build-app.sh first (or pass --build to do it here), then open the DMG.
#
# Usage:
#   apps/native/make-dmg.sh          # package the already-built app
#   apps/native/make-dmg.sh --build  # build the app first, then package
#   apps/native/make-dmg.sh --open   # also open the finished DMG
#   CODESIGN_IDENTITY="Developer ID Application: …" apps/native/make-dmg.sh --build
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NATIVE_DIR="$REPO_ROOT/apps/native"
DIST="$NATIVE_DIR/dist"
APP="$DIST/Unpeel.app"
VOLNAME="Unpeel"
FINAL_DMG="$DIST/Unpeel.dmg"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"

DO_BUILD=0
DO_OPEN=0
for arg in "$@"; do
  case "$arg" in
    --build) DO_BUILD=1 ;;
    --open)  DO_OPEN=1 ;;
  esac
done

step() { echo "==> $*"; }
is_adhoc_signing() { [ "$CODESIGN_IDENTITY" = "-" ]; }

codesign_dmg() {
  local args=(--force --sign "$CODESIGN_IDENTITY")
  if ! is_adhoc_signing; then
    args+=(--timestamp)
  fi

  codesign "${args[@]}" "$FINAL_DMG"
}

if [ "$DO_BUILD" -eq 1 ]; then
  "$NATIVE_DIR/build-app.sh"
fi

[ -d "$APP" ] || { echo "FAIL: $APP not found — run build-app.sh first" >&2; exit 1; }

# --- 1. Staging tree: the app + an /Applications alias ----------------------

step "staging DMG contents"
STAGE="$(mktemp -d)/dmg"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

# Drag-to-install background (dark gradient + arrow), rendered fresh each
# build and combined into a HiDPI TIFF so it stays crisp on retina. Lives in
# the hidden .background dir; the Finder layout step below points the window
# at it. Geometry (window size, icon positions) is shared with
# dmg-background.swift — change them together.
step "rendering window background"
BG_TMP="$(mktemp -d)"
swift "$NATIVE_DIR/dmg-background.swift" "$BG_TMP"
mkdir -p "$STAGE/.background"
tiffutil -cathidpicheck "$BG_TMP/bg.png" "$BG_TMP/bg@2x.png" \
  -out "$STAGE/.background/background.tiff" >/dev/null 2>&1

# --- 2. Read-write DMG we can lay out, then compress ------------------------

TMP_DMG="$(mktemp -d)/rw.dmg"
step "creating writable image"
# Roomy size so Finder can write its .DS_Store; trimmed on convert.
hdiutil create -srcfolder "$STAGE" -volname "$VOLNAME" -fs HFS+ \
  -format UDRW -size 256m -ov "$TMP_DMG" >/dev/null

# Layout must happen at the volume's real mount path (/Volumes/$VOLNAME):
# Finder stores the window background picture in .DS_Store as an alias whose
# resolution is tied to the mount path, so a layout done at a temporary
# mountpoint ships a DMG whose background silently fails to appear for end
# users (verified 2026-07-10). If something is already mounted there (usually
# a previously opened Unpeel install DMG), eject it first — non-forced, so a
# busy or non-disk-image volume fails loudly instead of being yanked. Also
# no -nobrowse: a nobrowse volume is invisible to Finder scripting entirely
# (Finder addresses disks by mount folder name).
MOUNT="/Volumes/$VOLNAME"
if [ -d "$MOUNT" ]; then
  step "ejecting existing $MOUNT"
  hdiutil detach "$MOUNT" >/dev/null 2>&1 || {
    echo "FAIL: $MOUNT is mounted and could not be ejected — eject it and re-run" >&2
    exit 1
  }
fi
step "mounting"
hdiutil attach "$TMP_DMG" -mountpoint "$MOUNT" -noautoopen >/dev/null

# --- 3. Drag-to-install window layout (best effort) -------------------------
#
# Finder automation needs a GUI session + Automation permission; if it isn't
# available (headless/CI), the DMG still works — it just opens with default
# icon placement instead of the side-by-side layout.
step "laying out window (best effort)"
osascript <<APPLESCRIPT 2>/dev/null || echo "    (skipped Finder layout — DMG still installs fine)"
tell application "Finder"
  tell disk "$(basename "$MOUNT")"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    -- Content area 520x360 (+28 title bar) to match the background image.
    set the bounds of container window to {200, 120, 720, 508}
    set opts to the icon view options of container window
    set arrangement of opts to not arranged
    set icon size of opts to 112
    set text size of opts to 12
    -- Fallback fill if the picture alias ever fails to resolve. Note: label
    -- text is ALWAYS black when a background picture is set (Finder ignores
    -- dark mode here) — the artwork draws light plates under the labels.
    set background color of opts to {3084, 3084, 3855}
    set background picture of opts to file ".background:background.tiff"
    set position of item "Unpeel.app" of container window to {140, 165}
    set position of item "Applications" of container window to {380, 165}
    -- Housekeeping items are dotfiles (invisible normally), but users with
    -- "show hidden files" on would see them — park them out of the visible
    -- 520x360 area.
    repeat with hiddenName in {".background", ".fseventsd", ".Trashes", ".VolumeIcon.icns", ".DS_Store"}
      try
        set position of item hiddenName of container window to {140, 620}
      end try
    end repeat
    update without registering applications
    delay 1
    close
  end tell
end tell
APPLESCRIPT

sync
step "unmounting"
# Retry the detach and fail loudly if it never succeeds — a swallowed failure
# here used to surface later as a misleading "resource busy" from hdiutil
# convert.
DETACHED=0
for i in 1 2 3 4 5; do
  if hdiutil detach "$MOUNT" >/dev/null 2>&1; then DETACHED=1; break; fi
  sleep 2
done
if [ "$DETACHED" -eq 0 ]; then
  hdiutil detach "$MOUNT" -force >/dev/null 2>&1 || {
    echo "FAIL: could not detach $MOUNT" >&2; exit 1
  }
fi

# --- 4. Compress to the final read-only DMG ---------------------------------

step "compressing $FINAL_DMG"
rm -f "$FINAL_DMG"
hdiutil convert "$TMP_DMG" -format UDZO -imagekey zlib-level=9 -o "$FINAL_DMG" >/dev/null
codesign_dmg
codesign --verify --strict "$FINAL_DMG" && echo "    signature OK"

echo
echo "Built: $FINAL_DMG"
du -sh "$FINAL_DMG" | awk '{print "Size:  "$1}'

if [ "$DO_OPEN" -eq 1 ]; then
  open "$FINAL_DMG"
fi
