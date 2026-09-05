#!/bin/bash
#
# build-app.sh — assemble an installable Unpeel.app from release builds.
#
# Produces apps/native/dist/Unpeel.app containing:
#   - the release UnpeelNative binary (GhosttyKit is statically linked)
#   - unpeel-host + unpeel-attach release binaries (embedded helpers the app
#     spawns; LaunchConfig resolves them via Bundle.main auxiliary executables)
#   - Sparkle.framework for Cloudflare/R2 appcast updates
#   - the SwiftPM resource bundle (for Bundle.module: the dock icon)
#   - AppIcon.icns + Info.plist
# Then code-signs the bundle. Defaults to ad-hoc signing for local builds; pass
# CODESIGN_IDENTITY="Developer ID Application: …" for distribution builds.
# Developer ID builds are signed with hardened runtime + timestamp so they can
# be notarized before public distribution.
#
# The server binaries (unpeel-host, unpeel, unpeel-attach) are built FROM
# THIS TREE by default (`cargo build --release --locked` in crates/ and
# crates/unpeel-attach), so the app can never skew from the server it
# bundles: one workspace version (crates/Cargo.toml) names both. For a
# reproducibility check against a published server release, bundle that
# release's archive instead:
#   UNPEEL_SERVER_ARCHIVE=<local .tar.gz>    a `unpeel-<version>-macos-universal`
#                                            CLI archive (from `release:cli`,
#                                            or its `--dry-run`); its `.sha256`
#                                            sidecar is verified when present
#                                            and BUILD_PROVENANCE.json must name
#                                            the workspace version
#
# Usage:
#   apps/native/build-app.sh
#   UNPEEL_BUILD=37 apps/native/build-app.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NATIVE_DIR="$REPO_ROOT/apps/native"
SWIFT_DIR="$NATIVE_DIR/UnpeelNative"
DIST="$NATIVE_DIR/dist"
APP="$DIST/Unpeel.app"
# One version number for the app, the bridge, and the server binaries: the
# crates workspace version. Bump crates/Cargo.toml (then `cargo update
# --workspace`) to release a new version; nothing here restates it.
SERVER_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$REPO_ROOT/crates/Cargo.toml" | head -n1)"
[ -n "$SERVER_VERSION" ] || {
  echo "FAIL: could not read the workspace version from crates/Cargo.toml" >&2
  exit 1
}
case "$SERVER_VERSION" in
  *[!A-Za-z0-9._-]*) echo "FAIL: the workspace version may only contain [A-Za-z0-9._-]" >&2; exit 1 ;;
esac
VERSION="${UNPEEL_VERSION:-$SERVER_VERSION}"
BUILD="${UNPEEL_BUILD:-5}"
# Empty by default on purpose: a dev build must not be a live Sparkle client
# pointed at the production feed (it would background-check and could replace
# itself with the published release). release.sh injects the channel feed URL.
SPARKLE_FEED_URL="${SPARKLE_FEED_URL:-}"
SPARKLE_PUBLIC_ED_KEY="${SPARKLE_PUBLIC_ED_KEY:-HbKIMOuEVJPtWViS7sbWhWOPj2qFRAiRG3Y4RP52PHg=}"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
CODESIGN_ENTITLEMENTS="${CODESIGN_ENTITLEMENTS:-}"
UNPEEL_DEV_BUILD="${UNPEEL_DEV_BUILD:-0}"

# Keep checkout, Cargo registry, and toolchain source paths out of shipped
# Rust panic/location strings. Caller-supplied Rust flags are retained.
. "$REPO_ROOT/scripts/rust-release-env.sh"
unpeel_enable_rust_path_remapping "$REPO_ROOT"

# Remap both debug metadata and compile-time file literals (including
# #filePath) in every Swift release object. -Xswiftc appends to any caller
# environment flags SwiftPM already honors.
SWIFT_PATH_REMAP_FLAGS=(
  -Xswiftc -debug-prefix-map
  -Xswiftc "$REPO_ROOT=/unpeel/source"
  -Xswiftc -file-prefix-map
  -Xswiftc "$REPO_ROOT=/unpeel/source"
)

step() { echo "==> $*"; }
is_adhoc_signing() { [ "$CODESIGN_IDENTITY" = "-" ]; }

codesign_release() {
  local target="$1"
  shift

  local args=(--force --sign "$CODESIGN_IDENTITY")
  if ! is_adhoc_signing; then
    args+=(--timestamp --options runtime)
    if [ -n "$CODESIGN_ENTITLEMENTS" ]; then
      args+=(--entitlements "$CODESIGN_ENTITLEMENTS")
    fi
  fi

  codesign "${args[@]}" "$@" "$target"
}

verify_release_path_privacy() {
  local candidate forbidden_prefix

  # Source paths can survive stripping in panic metadata and Swift #filePath
  # literals. Treat leakage of either this checkout or the build operator's
  # home directory as a release packaging failure. Scan every Mach-O in the
  # finished bundle, including framework helpers and both universal slices.
  while IFS= read -r -d '' candidate; do
    file -b "$candidate" | grep -q 'Mach-O' || continue
    for forbidden_prefix in "$REPO_ROOT" "${HOME:?}/"; do
      if LC_ALL=C grep -aFq -- "$forbidden_prefix" "$candidate"; then
        echo "FAIL: release Mach-O embeds a private build path: $candidate" >&2
        echo "      matched forbidden prefix: $forbidden_prefix" >&2
        exit 1
      fi
    done
  done < <(find "$APP" -type f -print0)
}

verify_release_architectures() {
  local binary

  # The native app's declared support floor is Apple silicon. A release may
  # become universal later, but it must never silently inherit an Intel build
  # host's architecture and publish without an arm64 slice.
  for binary in UnpeelNative unpeel-host unpeel unpeel-attach; do
    if ! lipo "$APP/Contents/MacOS/$binary" -verify_arch arm64 >/dev/null 2>&1; then
      echo "FAIL: release binary does not contain the required arm64 slice: $binary" >&2
      lipo -info "$APP/Contents/MacOS/$binary" >&2 || true
      exit 1
    fi
  done

}

# --- 1. Release builds ------------------------------------------------------

step "building native Rust bridge (release)"
# The bridge is a workspace member with path deps on unpeel-core/unpeel-serve,
# so it is always built from the same tree as the server binaries below.
"$NATIVE_DIR/build-rust-bridge.sh" release

step "building UnpeelNative (release)"
# SwiftPM's generated Bundle.module accessor bakes its absolute .build path
# into executable targets. Packaged resources must go through
# ModuleResources so release binaries neither leak the checkout path nor fall
# back to a directory that exists only on the build Mac.
if grep -R --include='*.swift' -nE 'Bundle\.module\.' \
  "$SWIFT_DIR/Sources/UnpeelNative"
then
  echo "FAIL: native app source must use ModuleResources instead of Bundle.module" >&2
  exit 1
fi
(cd "$SWIFT_DIR" && swift build -c release "${SWIFT_PATH_REMAP_FLAGS[@]}")

# --- 1b. Server binaries: built from this tree (default) or a CLI archive ----
#
# Default: cargo-build unpeel-host, unpeel, and unpeel-attach from this
# checkout — the same commit as the bridge and the app. UNPEEL_SERVER_ARCHIVE
# bundles a published CLI archive instead (reproducibility checks, upgrade
# rehearsals); its provenance must name this workspace version.
SERVER_BIN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/unpeel-server-bins.XXXXXX")"
SERVER_BINARIES=(unpeel-host unpeel unpeel-attach)
sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }

stage_server_from_source() {
  step "building unpeel-host + unpeel (release, from source)"
  (cd "$REPO_ROOT/crates" && cargo build --release --locked --bin unpeel-host --bin unpeel)
  step "building unpeel-attach (release, from source)"
  (cd "$REPO_ROOT/crates/unpeel-attach" && cargo build --release --locked)
  cp "$REPO_ROOT/crates/target/release/unpeel-host" "$SERVER_BIN_DIR/unpeel-host"
  cp "$REPO_ROOT/crates/target/release/unpeel" "$SERVER_BIN_DIR/unpeel"
  cp "$REPO_ROOT/crates/unpeel-attach/target/release/unpeel-attach" "$SERVER_BIN_DIR/unpeel-attach"
  # The archive path ships THIRD_PARTY_NOTICES.txt for these three binaries;
  # a from-source build collects the same notices from the locked graphs.
  step "collecting server third-party notices"
  local notice_target
  notice_target="$(rustc -vV | sed -n 's/^host: //p')"
  [ -n "$notice_target" ] || { echo "FAIL: rustc did not report a host target" >&2; exit 1; }
  cargo run --quiet --locked \
    --manifest-path "$REPO_ROOT/crates/Cargo.toml" \
    -p unpeel-license-notices -- \
    --manifest-path "$REPO_ROOT/crates/Cargo.toml" \
    --package unpeel-cli \
    --package unpeel-host \
    --manifest-path "$REPO_ROOT/crates/unpeel-attach/Cargo.toml" \
    --package unpeel-attach \
    --target "$notice_target" \
    --output "$SERVER_BIN_DIR/THIRD_PARTY_NOTICES.txt"
}

verify_server_archive() { # verify_server_archive <archive> [<sha256 sidecar>]
  local archive="$1" sidecar="${2:-}" expected actual provenance
  if [ -n "$sidecar" ]; then
    expected="$(awk 'NR == 1 { print $1 }' "$sidecar")"
    case "$expected" in
      *[!0-9a-f]*|'') echo "FAIL: malformed sha256 sidecar $sidecar" >&2; exit 1 ;;
    esac
    [ "${#expected}" -eq 64 ] || { echo "FAIL: malformed sha256 sidecar $sidecar" >&2; exit 1; }
    actual="$(sha256_of "$archive")"
    [ "$actual" = "$expected" ] || {
      echo "FAIL: server archive sha256 mismatch for $archive" >&2
      echo "      expected $expected" >&2
      echo "      actual   $actual" >&2
      exit 1
    }
  fi
  tar -xzf "$archive" -C "$SERVER_BIN_DIR" "${SERVER_BINARIES[@]}" BUILD_PROVENANCE.json THIRD_PARTY_NOTICES.txt || {
    echo "FAIL: server archive $archive does not carry unpeel-host, unpeel, unpeel-attach, and BUILD_PROVENANCE.json" >&2
    echo "      (three-binary archives ship from CLI 0.4.5; earlier archives cannot feed the app build)" >&2
    exit 1
  }
  provenance="$SERVER_BIN_DIR/BUILD_PROVENANCE.json"
  PROVENANCE_VERSION="$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$provenance" | head -n1)"
  PROVENANCE_TARGET="$(sed -n 's/^[[:space:]]*"target"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$provenance" | head -n1)"
  PROVENANCE_COMMIT="$(sed -n 's/^[[:space:]]*"source_commit"[[:space:]]*:[[:space:]]*"\([0-9a-f]*\)".*/\1/p' "$provenance" | head -n1)"
  [ "$PROVENANCE_VERSION" = "$SERVER_VERSION" ] || {
    echo "FAIL: server archive BUILD_PROVENANCE.json is version '$PROVENANCE_VERSION', SERVER_VERSION is $SERVER_VERSION" >&2
    exit 1
  }
  [ "$PROVENANCE_TARGET" = "macos-universal" ] || {
    echo "FAIL: server archive target is '$PROVENANCE_TARGET', expected macos-universal" >&2
    exit 1
  }
  [ "${#PROVENANCE_COMMIT}" -eq 40 ] || {
    echo "FAIL: server archive BUILD_PROVENANCE.json has no source_commit" >&2
    exit 1
  }
  echo "    server $SERVER_VERSION macos-universal from source commit $PROVENANCE_COMMIT"
  echo "    archive sha256 $(sha256_of "$archive")"
}

if [ -n "${UNPEEL_SERVER_ARCHIVE:-}" ]; then
  if [ "${UNPEEL_BUILD_SERVER_FROM_SOURCE:-0}" = "1" ]; then
    echo "FAIL: UNPEEL_SERVER_ARCHIVE and UNPEEL_BUILD_SERVER_FROM_SOURCE=1 contradict each other" >&2
    exit 1
  fi
  step "using local server archive $UNPEEL_SERVER_ARCHIVE"
  [ -s "$UNPEEL_SERVER_ARCHIVE" ] || { echo "FAIL: UNPEEL_SERVER_ARCHIVE not found: $UNPEEL_SERVER_ARCHIVE" >&2; exit 1; }
  if [ -s "$UNPEEL_SERVER_ARCHIVE.sha256" ]; then
    verify_server_archive "$UNPEEL_SERVER_ARCHIVE" "$UNPEEL_SERVER_ARCHIVE.sha256"
  else
    verify_server_archive "$UNPEEL_SERVER_ARCHIVE"
  fi
else
  # The default (UNPEEL_BUILD_SERVER_FROM_SOURCE=1 is accepted as the explicit
  # spelling of it): the server binaries come from this tree.
  stage_server_from_source
fi
for bin in "${SERVER_BINARIES[@]}"; do
  [ -x "$SERVER_BIN_DIR/$bin" ] || { echo "FAIL: server binary missing after staging: $bin" >&2; exit 1; }
done

SWIFT_BIN_DIR="$(cd "$SWIFT_DIR" && swift build -c release --show-bin-path "${SWIFT_PATH_REMAP_FLAGS[@]}")"
APP_BIN="$SWIFT_BIN_DIR/UnpeelNative"
HOST_BIN="$SERVER_BIN_DIR/unpeel-host"
CLI_BIN="$SERVER_BIN_DIR/unpeel"
ATTACH_BIN="$SERVER_BIN_DIR/unpeel-attach"
RES_BUNDLE="$SWIFT_BIN_DIR/UnpeelNative_UnpeelNative.bundle"
SPARKLE_FRAMEWORK="$SWIFT_DIR/.build/artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework"

for f in "$APP_BIN" "$HOST_BIN" "$CLI_BIN" "$ATTACH_BIN"; do
  [ -x "$f" ] || { echo "FAIL: missing build product $f" >&2; exit 1; }
done
[ -d "$SPARKLE_FRAMEWORK" ] || {
  echo "FAIL: missing Sparkle framework $SPARKLE_FRAMEWORK" >&2
  echo "      run: cd $SWIFT_DIR && swift package resolve" >&2
  exit 1
}

# --- 2. App icon (Icon Composer .icon → asset catalog) ----------------------
#
# macOS 26 (Tahoe) gives the modern full-size Dock/Launchpad icon treatment only
# to icons shipped as an Icon Composer ".icon" compiled into an asset catalog
# (Assets.car, referenced by Info.plist's CFBundleIconName) — the same packaging
# Claude/Codex use. A bare .icns, or even a classic multi-size .appiconset, is
# treated as legacy and drawn smaller/inset, no matter how full-bleed the art
# is. We synthesize a single-layer .icon from the 1024px square source; actool
# compiles it into Assets.car (the Tahoe "iconstack") plus a fallback
# AppIcon.icns for macOS < 26. The solid fill matches the icon's dark base.

step "preparing AppIcon.icon"
SRC_PNG="$SWIFT_DIR/Sources/UnpeelNative/Resources/AppIcon.png"
ICON_DIR="$(mktemp -d)/AppIcon.icon"
mkdir -p "$ICON_DIR/Assets"
cp "$SRC_PNG" "$ICON_DIR/Assets/AppIcon.png"
# The icon art is transparent, so this fill is the visible background. Dev
# builds get a burnt-orange base so dist and /Applications are tellable apart
# in the Dock at a glance; UNPEEL_ICON_FILL overrides either.
# Release fill = the main dark background (#1A1A1F, Theme.swift), matching
# the app frame while the lighter Surface now lifts above it.
ICON_FILL="srgb:0.102,0.102,0.122,1.0"
[ "$UNPEEL_DEV_BUILD" = "1" ] && ICON_FILL="srgb:0.55,0.25,0.02,1.0"
ICON_FILL="${UNPEEL_ICON_FILL:-$ICON_FILL}"
cat > "$ICON_DIR/icon.json" <<JSON
{
  "fill" : { "solid" : "$ICON_FILL" },
  "groups" : [
    { "layers" : [ { "image-name" : "AppIcon.png", "name" : "AppIcon" } ] }
  ],
  "supported-platforms" : { "circles" : [ ], "squares" : [ "macOS" ] }
}
JSON

# --- 3. Assemble the bundle -------------------------------------------------

step "assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"

cp "$APP_BIN"    "$APP/Contents/MacOS/UnpeelNative"
cp "$HOST_BIN"   "$APP/Contents/MacOS/unpeel-host"
cp "$CLI_BIN"    "$APP/Contents/MacOS/unpeel"
cp "$ATTACH_BIN" "$APP/Contents/MacOS/unpeel-attach"

# Swift's release linker retains local object/archive provenance (including
# absolute checkout and Cargo paths) even when source locations are prefix-
# mapped. Remove those debug symbols from the staged copy before the privacy
# gate and final code signing. Compile-time #filePath strings remain covered
# by SWIFT_PATH_REMAP_FLAGS above.
if [ "$UNPEEL_DEV_BUILD" != "1" ]; then
  step "stripping native release debug symbols"
  strip -S "$APP/Contents/MacOS/UnpeelNative"
fi

# Browser MCP engine: NOT bundled (since 0.5.0). The Host installs and
# hash-verifies the pinned agent-browser into ~/.unpeel/browser/bin at start
# (protocol/browser-engine-v1.json, unpeel_core::browser_engine; also
# `unpeel browser install`) and writes the Apache-2.0 notice next to it, so
# the app carries no engine copy and no engine notice. A copy next to
# unpeel-host is still honoured as a compatibility resolution candidate.

# Computer Use is development-build-only until hosted sessions have a kernel-
# enforced broker boundary. The embedded unrestricted daemon inherits the
# app's TCC grants, so shipping it in a release would let same-UID hosted code
# bypass Unpeel's cooperative approval UI by calling its raw socket directly.
if [ "$UNPEEL_DEV_BUILD" = "1" ]; then
  CUA_DRIVER_SRC="${UNPEEL_CUA_DRIVER_BIN:-}"
  if [ -z "$CUA_DRIVER_SRC" ]; then
    for candidate in \
      "$HOME/.unpeel/computer/bin/cua-driver" \
      "$(command -v cua-driver 2>/dev/null || true)" \
      "$HOME/.local/bin/cua-driver"; do
      [ -n "$candidate" ] && [ -e "$candidate" ] || continue
      resolved="$(readlink -f "$candidate" 2>/dev/null || echo "$candidate")"
      if [ -f "$resolved" ]; then CUA_DRIVER_SRC="$resolved"; break; fi
    done
  fi
  if [ -n "$CUA_DRIVER_SRC" ] && [ -f "$CUA_DRIVER_SRC" ]; then
    step "bundling development-only cua-driver engine ($CUA_DRIVER_SRC)"
    cp -L "$CUA_DRIVER_SRC" "$APP/Contents/MacOS/cua-driver"
    # MIT notice ships alongside when the source layout carries one.
    CUA_DRIVER_LICENSE="$(dirname "$CUA_DRIVER_SRC")/../LICENSE"
    if [ -f "$CUA_DRIVER_LICENSE" ]; then
      cp "$CUA_DRIVER_LICENSE" "$APP/Contents/Resources/cua-driver-LICENSE.txt"
    fi
  else
    echo "note: cua-driver engine not found — development Computer Use is unavailable"
  fi
else
  step "excluding cua-driver from release build (Computer Use is security-blocked)"
fi

# Source guard for the production containment above. Keep this independent of
# the branch that chooses the engine so a future refactor cannot accidentally
# place the TCC-bearing helper back into a customer bundle.
if [ "$UNPEEL_DEV_BUILD" != "1" ] && [ -e "$APP/Contents/MacOS/cua-driver" ]; then
  echo "FAIL: release bundle contains security-blocked cua-driver" >&2
  exit 1
fi

# License payloads are part of the signed app. Rust notices follow the exact
# locked dependency graphs for every embedded Rust component. Native-only
# Swift/framework notices stay separate so CLI archives do not inherit them.
step "collecting release licenses and third-party notices"
cp "$REPO_ROOT/LICENSE" "$APP/Contents/Resources/LICENSE.txt"
RUST_NOTICE_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
[ -n "$RUST_NOTICE_TARGET" ] || { echo "FAIL: rustc did not report a host target" >&2; exit 1; }
# The bridge is the only Rust the app builds from source; the notices for
# the bundled server binaries ship inside the server archive itself.
cargo run --quiet --locked \
  --manifest-path "$REPO_ROOT/crates/Cargo.toml" \
  -p unpeel-license-notices -- \
  --manifest-path "$REPO_ROOT/crates/Cargo.toml" \
  --package unpeel-native-bridge \
  --target "$RUST_NOTICE_TARGET" \
  --output "$APP/Contents/Resources/THIRD_PARTY_NOTICES_RUST.txt"
if [ -s "$SERVER_BIN_DIR/THIRD_PARTY_NOTICES.txt" ]; then
  cp "$SERVER_BIN_DIR/THIRD_PARTY_NOTICES.txt" "$APP/Contents/Resources/THIRD_PARTY_NOTICES_SERVER.txt"
fi
sh "$NATIVE_DIR/collect-swift-notices.sh" \
  "$APP/Contents/Resources/THIRD_PARTY_NOTICES_SWIFT.txt"

chmod +x "$APP/Contents/MacOS/"*
if ! otool -l "$APP/Contents/MacOS/UnpeelNative" | grep -q "@loader_path/../Frameworks"; then
  install_name_tool -add_rpath "@loader_path/../Frameworks" "$APP/Contents/MacOS/UnpeelNative"
fi
# SwiftPM resource bundle: Contents/Resources keeps codesign treating it as a
# resource, not stray code in MacOS/. Code must resolve it via ModuleResources
# (Bundle.main.resourceURL) — NOT Bundle.module, whose executable-target
# accessor only checks the .app root and the build machine's .build path, then
# fatalErrors (the beta.6–25 Settings ▸ Mobile crash).
[ -d "$RES_BUNDLE" ] && cp -R "$RES_BUNDLE" "$APP/Contents/Resources/"
ditto "$SPARKLE_FRAMEWORK" "$APP/Contents/Frameworks/Sparkle.framework"

# Compile the .icon → Assets.car (Tahoe iconstack) + AppIcon.icns (legacy
# fallback), both into Contents/Resources.
xcrun actool "$ICON_DIR" \
  --compile "$APP/Contents/Resources" \
  --app-icon AppIcon \
  --platform macosx \
  --minimum-deployment-target 13.0 \
  --output-partial-info-plist "$(mktemp)" >/dev/null
[ -f "$APP/Contents/Resources/Assets.car" ] || {
  echo "FAIL: actool did not produce Assets.car" >&2; exit 1
}
# Force the Dock to use the Assets.car iconstack (the full-size Liquid Glass
# render on macOS 26), not the legacy .icns: if a loose AppIcon.icns sits next
# to it AND CFBundleIconFile names it, the system resolves "AppIcon" to that
# legacy file and draws the classic, smaller icon. Drop the loose .icns and the
# CFBundleIconFile key (below) so only CFBundleIconName → Assets.car remains.
rm -f "$APP/Contents/Resources/AppIcon.icns"

# Sparkle update keys only when a feed URL is set (release builds). Without
# them the app's updater never starts (sparkleCanStart requires a feed).
SPARKLE_PLIST_KEYS=""
if [ -n "$SPARKLE_FEED_URL" ]; then
  SPARKLE_PLIST_KEYS="    <key>SUFeedURL</key>               <string>$SPARKLE_FEED_URL</string>
    <key>SUPublicEDKey</key>           <string>$SPARKLE_PUBLIC_ED_KEY</string>
    <key>SUEnableAutomaticChecks</key> <true/>"
fi
DEV_BUILD_PLIST_KEYS=""
# Dev builds are named "Unpeel Dev" (menu bar, Dock tooltip, force-quit list)
# so they're tellable from the installed release app; the bundle id stays
# com.unpeel.native either way. Quit them with `osascript -e 'quit app
# "Unpeel Dev"'` — plain "Unpeel" targets the installed app.
APP_NAME="Unpeel"
if [ "$UNPEEL_DEV_BUILD" = "1" ]; then
  DEV_BUILD_PLIST_KEYS="    <key>UnpeelDevelopmentBuild</key> <true/>"
  APP_NAME="Unpeel Dev"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>     <string>$APP_NAME</string>
    <key>CFBundleExecutable</key>      <string>UnpeelNative</string>
    <key>CFBundleIdentifier</key>      <string>com.unpeel.native</string>
    <key>CFBundleIconName</key>        <string>AppIcon</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>$VERSION</string>
    <key>CFBundleVersion</key>         <string>$BUILD</string>
    <key>NSHumanReadableCopyright</key> <string>© $(date +%Y) UX Themes AS</string>
    <key>LSMinimumSystemVersion</key>  <string>13.0</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key> <true/>
    <key>LSApplicationCategoryType</key> <string>public.app-category.developer-tools</string>
$DEV_BUILD_PLIST_KEYS
$SPARKLE_PLIST_KEYS
    <!-- Finder right-click ▸ Services ▸ "New Unpeel Session Here". Shows on
         folders (NSSendFileTypes = public.folder); AppKit routes the message
         to AppDelegate.newUnpeelSession. After first install macOS may need a
         Launch Services refresh: /System/Library/CoreServices/pbs -update -->
    <key>NSServices</key>
    <array>
        <dict>
            <key>NSMenuItem</key>
            <dict>
                <key>default</key>
                <string>New Unpeel Session Here</string>
            </dict>
            <key>NSMessage</key>      <string>newUnpeelSession</string>
            <key>NSPortName</key>     <string>Unpeel</string>
            <key>NSSendFileTypes</key>
            <array>
                <string>public.folder</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
PLIST

if [ "$UNPEEL_DEV_BUILD" != "1" ]; then
  step "checking release binary architectures"
  verify_release_architectures
  step "checking release binaries for private build paths"
  verify_release_path_privacy
fi

# --- 4. Code sign -----------------------------------------------------------

# Sparkle's nested executables are signed individually (inside-out), never
# with --deep and never with the app's entitlements: --deep is deprecated and
# would stamp CODESIGN_ENTITLEMENTS onto Sparkle's XPC services, while
# --preserve-metadata keeps their own entitlements (e.g. Downloader.xpc's
# sandbox) intact — per Sparkle's own signing guidance.
codesign_sparkle() {
  local args=(--force --sign "$CODESIGN_IDENTITY" --preserve-metadata=entitlements)
  if ! is_adhoc_signing; then
    args+=(--timestamp --options runtime)
  fi
  codesign "${args[@]}" "$1"
}

step "code signing"
# Sign the embedded helpers first, then the app bundle (inside-out).
codesign_release "$APP/Contents/MacOS/unpeel-host"
codesign_release "$APP/Contents/MacOS/unpeel"
codesign_release "$APP/Contents/MacOS/unpeel-attach"
if [ -f "$APP/Contents/MacOS/agent-browser" ]; then
  # Re-sign the third-party engines with our identity so notarization covers them.
  codesign_release "$APP/Contents/MacOS/agent-browser"
fi
if [ -f "$APP/Contents/MacOS/cua-driver" ]; then
  codesign_release "$APP/Contents/MacOS/cua-driver"
fi
SPARKLE_FW="$APP/Contents/Frameworks/Sparkle.framework"
codesign_sparkle "$SPARKLE_FW/Versions/B/XPCServices/Downloader.xpc"
codesign_sparkle "$SPARKLE_FW/Versions/B/XPCServices/Installer.xpc"
codesign_sparkle "$SPARKLE_FW/Versions/B/Autoupdate"
codesign_sparkle "$SPARKLE_FW/Versions/B/Updater.app"
codesign_sparkle "$SPARKLE_FW"
codesign_release "$APP"
codesign --verify --deep --strict "$APP" && echo "    signature OK"

echo
echo "Built: $APP"
du -sh "$APP" | awk '{print "Size:  "$1}'
