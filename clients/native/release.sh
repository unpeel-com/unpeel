#!/bin/bash
#
# release.sh — one command to cut a public Unpeel release from a Mac.
#
# Chains the existing release steps into a single pipeline:
#   1. build-app.sh      build + Developer ID sign Unpeel.app (hardened runtime)
#   2. notarize app      submit a ZIP of the app, staple the ticket onto the .app
#   3. make-dmg.sh       package + sign the install DMG (from the stapled app)
#   4. notarize-dmg.sh   notarize + staple the DMG
#   5. Sparkle ZIP       zip the stapled app as the self-update archive
#   6. generate_appcast  EdDSA-sign the ZIP and write the channel appcast.xml
#   7. release:cloudflare upload DMG + ZIP + appcast + latest.json to R2
#
# The app is notarized and stapled BEFORE the DMG is packaged, so both the app
# inside the shipped DMG and the app inside the Sparkle ZIP carry a stapled
# ticket (offline Gatekeeper passes without a round-trip to Apple).
#
# Everything (build, sign, notarize, Sparkle-sign, upload) runs locally; the
# only network calls are Apple's notary service and the Wrangler R2 upload.
#
# Usage:
#   CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID1234)" \
#   NOTARY_KEYCHAIN_PROFILE=unpeel-notary \
#   clients/native/release.sh --channel beta --build 6
#
# The version comes from the crates workspace (crates/Cargo.toml) — the app
# and the `unpeel` CLI are versioned in lockstep. --version is optional and
# must MATCH the workspace version when given; to release a new version, bump
# crates/Cargo.toml (then `cargo update --workspace`) so both release
# pipelines move together.
#
#   # with release notes shown in the Sparkle update dialog (HTML body):
#   ... clients/native/release.sh --channel beta --version 0.1.0-beta.4 --build 6 \
#       --notes clients/native/release-notes/0.1.0-beta.4.html
#
#   # rehearse without touching Apple notary or R2 (still builds + signs locally):
#   ... clients/native/release.sh --channel beta --version 0.1.0-beta.4 --build 6 --dry-run
#
# Flags:
#   --dry-run         no notarization, no upload; Sparkle artifacts are staged
#                     under dist/sparkle-dryrun/ so they can never leak into a
#                     real channel appcast.
#   --skip-notarize   skip Apple notary for fast local iteration. Implies no
#                     publish: an un-notarized build must never reach R2. Pass
#                     --force-publish-unnotarized if you really must override.
#   --force           skip the already-published / build-monotonicity preflight
#                     (also forwarded to the publish script's overwrite guard).
#
# Required env for a real release:
#   CODESIGN_IDENTITY        Developer ID Application identity (not ad-hoc "-")
#   NOTARY_KEYCHAIN_PROFILE  notarytool keychain profile (or the NOTARY_APPLE_ID /
#                            NOTARY_TEAM_ID / NOTARY_PASSWORD trio — see notarize-dmg.sh)
#   plus the Sparkle EdDSA private key in the login Keychain (from generate_keys).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NATIVE_DIR="$REPO_ROOT/clients/native"
SWIFT_DIR="$NATIVE_DIR/UnpeelNative"
DIST="$NATIVE_DIR/dist"

CHANNEL="${UNPEEL_CHANNEL:-beta}"
VERSION="${UNPEEL_VERSION:-}"
BUILD="${UNPEEL_BUILD:-}"
NOTES=""
DRY_RUN=0
SKIP_NOTARIZE=0
FORCE=0
FORCE_PUBLISH_UNNOTARIZED=0

while [ $# -gt 0 ]; do
  case "$1" in
    --channel) CHANNEL="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --build)   BUILD="$2";   shift 2 ;;
    --notes)   NOTES="$2";   shift 2 ;;
    --dry-run) DRY_RUN=1;        shift ;;
    --skip-notarize) SKIP_NOTARIZE=1; shift ;;
    --force) FORCE=1; shift ;;
    --force-publish-unnotarized) FORCE_PUBLISH_UNNOTARIZED=1; shift ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

step() { echo; echo "==> $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# --- Preflight --------------------------------------------------------------

case "$CHANNEL" in
  alpha|beta|stable) ;;
  *) fail "--channel must be alpha, beta, or stable (got '$CHANNEL')" ;;
esac
# Lockstep versioning (decided 2026-08-13): one version number for the app and
# the CLI, sourced from the crates workspace. Derive it when --version is
# omitted; refuse a mismatch when it isn't — the number is bumped in
# crates/Cargo.toml, never on the command line, so the two release pipelines
# (this script and scripts/release-cli.mjs) can never drift.
WORKSPACE_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$REPO_ROOT/crates/Cargo.toml" | head -n1)"
[ -n "$WORKSPACE_VERSION" ] || fail "could not read the workspace version from crates/Cargo.toml"
# The bundled server binaries (unpeel-host, unpeel, unpeel-attach) and the
# bridge are built by build-app.sh from THIS tree at the same commit as the
# app, so a release can never skew from its server. UNPEEL_SERVER_ARCHIVE
# instead bundles a published CLI archive of the same version (a
# reproducibility check or an upgrade rehearsal); build-app.sh verifies its
# sha256 sidecar and BUILD_PROVENANCE.json against the workspace version.
if [ -z "$VERSION" ]; then
  VERSION="$WORKSPACE_VERSION"
elif [ "$VERSION" != "$WORKSPACE_VERSION" ]; then
  fail "--version $VERSION does not match the crates workspace version $WORKSPACE_VERSION.
      The app and CLI are versioned in lockstep — bump the version in
      crates/Cargo.toml (then 'cargo update --workspace') instead of passing
      a different one here."
fi
[ -n "$BUILD" ]   || fail "--build is required (monotonic integer, e.g. 6)"
case "$VERSION" in
  *[!A-Za-z0-9._-]*) fail "--version may only contain [A-Za-z0-9._-] — it becomes an R2 object key and public URL" ;;
esac
case "$BUILD" in
  ''|0|*[!0-9]*) fail "--build must be a positive integer (got '$BUILD')" ;;
esac

# A real artifact must be reproducibly tied to the reviewed source on main.
# Dry runs intentionally permit local edits so pipeline changes can be tested
# before committing; the publish path fails before any expensive build.
if [ "$DRY_RUN" -eq 0 ]; then
  node "$REPO_ROOT/scripts/release-source-preflight.mjs" || \
    fail "release source is not clean and aligned with pushed main. Commit and push the final
      source, fetch origin/main, then rerun the release."
fi

# Every published release must have a website changelog entry (a `## <version>`
# heading in the website's changelog.md) — the site's /changelog page renders
# it. The changelog lives with the website: scripts/release-changelog.mjs
# resolves UNPEEL_CHANGELOG, then the ../unpeel-website sibling checkout
# (apps/website/app/changelog.md), then apps/website/app/changelog.md (monorepo), and fails
# naming the sibling checkout when none exists. Dry runs are exempt from the
# entry check (local iteration) but still need the file to exist. After
# publishing, deploy the website so the entry goes live.
CHANGELOG_MD="$(node "$REPO_ROOT/scripts/release-changelog.mjs" --repo-root "$REPO_ROOT")" || \
  fail "could not locate the website changelog (see the message above)."
if [ "$DRY_RUN" -eq 0 ] && ! grep -qE "^## $VERSION( |$)" "$CHANGELOG_MD" 2>/dev/null; then
  fail "no changelog entry for $VERSION — add a '## $VERSION — <date>' section to
      $CHANGELOG_MD (newest first), then deploy the website after release."
fi

# Computer Use is deliberately absent from release bundles until its
# TCC-bearing daemon is isolated from same-UID hosted code. build-app.sh
# includes cua-driver only when UNPEEL_DEV_BUILD=1.

# An un-notarized build must never be published: Gatekeeper rejects it on every
# user's machine and latest.json/appcast would point at a broken release.
if [ "$SKIP_NOTARIZE" -eq 1 ] && [ "$DRY_RUN" -eq 0 ] && [ "$FORCE_PUBLISH_UNNOTARIZED" -eq 0 ]; then
  fail "--skip-notarize would publish an un-notarized build. Add --dry-run for local
      iteration, or --force-publish-unnotarized if you really intend to publish."
fi

# Feed URL the app checks for this channel — must match where we publish the
# appcast so a build of channel X actually sees channel X's updates.
case "$CHANNEL" in
  stable) FEED_URL="https://unpeel.com/appcast.xml" ;;
  beta)   FEED_URL="https://unpeel.com/appcast-beta.xml" ;;
  alpha)  FEED_URL="https://unpeel.com/appcast-alpha.xml" ;;
esac
BASE_URL="${UNPEEL_RELEASE_BASE_URL:-https://unpeel.com}"
DOWNLOAD_PREFIX="$BASE_URL/releases/$CHANNEL/"

CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
if [ "$CODESIGN_IDENTITY" = "-" ] && [ "$DRY_RUN" -eq 0 ]; then
  fail "CODESIGN_IDENTITY must be a Developer ID Application identity for a real release.
      Set it, or pass --dry-run to rehearse with ad-hoc signing.
      List identities with: security find-identity -v -p codesigning"
fi
if [ "$CODESIGN_IDENTITY" != "-" ]; then
  security find-identity -v -p codesigning | grep -Fq "$CODESIGN_IDENTITY" || \
    fail "signing identity not found in the keychain: $CODESIGN_IDENTITY
      List identities with: security find-identity -v -p codesigning"
fi

if [ "$DRY_RUN" -eq 0 ] && [ "$SKIP_NOTARIZE" -eq 0 ]; then
  { [ -n "${NOTARY_KEY_PATH:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER:-}" ]; } || \
    [ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ] || \
    { [ -n "${NOTARY_APPLE_ID:-}" ] && [ -n "${NOTARY_TEAM_ID:-}" ] && [ -n "${NOTARY_PASSWORD:-}" ]; } || \
    fail "missing notary credentials (set the NOTARY_KEY_PATH / NOTARY_KEY_ID /
      NOTARY_ISSUER API-key trio, NOTARY_KEYCHAIN_PROFILE, or the NOTARY_APPLE_ID /
      NOTARY_TEAM_ID / NOTARY_PASSWORD trio). See clients/native/notarize-dmg.sh."
fi

if [ "$DRY_RUN" -eq 0 ]; then
  # These fail 30+ notary-wait minutes into the run if not caught here.
  security find-generic-password -l "Private key for signing Sparkle updates" >/dev/null 2>&1 || \
    fail "Sparkle EdDSA private key not found in the login Keychain — generate_appcast
      would fail after the notary wait. Import it, or run Sparkle's generate_keys."
  (cd "$REPO_ROOT" && npx wrangler whoami >/dev/null 2>&1) || \
    fail "wrangler is not authenticated — the R2 upload would fail after the notary
      wait. Run 'npx wrangler login' (or set CLOUDFLARE_API_TOKEN)."
fi

# Refuse to re-publish an existing immutable DMG/ZIP key and refuse a
# non-monotonic build number (a beta user switching to stable would otherwise
# look "newer" than the stable appcast and get stuck). The Node preflight
# validates every channel manifest and HEADs the target versioned object URLs;
# network, HTTP, and malformed-manifest errors fail closed. --force is the only
# bypass, for an intentional operator recovery.
if [ "$DRY_RUN" -eq 0 ] && [ "$FORCE" -eq 0 ]; then
  node "$REPO_ROOT/scripts/release-app-preflight.mjs" \
    --base-url "$BASE_URL" --channel "$CHANNEL" --version "$VERSION" --build "$BUILD" || \
    fail "published-state preflight failed. Nothing was built or uploaded; retry when the
      release endpoint is healthy, bump version/build for an existing artifact, or use
      --force only for an intentional recovery."
fi

# Locate the Sparkle CLI tools shipped with the resolved SwiftPM artifact.
SPARKLE_BIN="$SWIFT_DIR/.build/artifacts/sparkle/Sparkle/bin"
GENERATE_APPCAST="$SPARKLE_BIN/generate_appcast"
if [ ! -x "$GENERATE_APPCAST" ]; then
  # Fall back to the source checkout (older SwiftPM layouts).
  ALT="$(find "$SWIFT_DIR/.build" -name generate_appcast -type f 2>/dev/null | head -n1 || true)"
  [ -n "$ALT" ] && GENERATE_APPCAST="$ALT"
fi
[ -x "$GENERATE_APPCAST" ] || fail "generate_appcast not found under $SWIFT_DIR/.build —
      run 'swift build' in $SWIFT_DIR once to resolve the Sparkle artifact."

if [ -n "$NOTES" ]; then
  NOTES="$(cd "$(dirname "$NOTES")" && pwd)/$(basename "$NOTES")"
  [ -f "$NOTES" ] || fail "--notes file not found: $NOTES"
fi

NOTARIZE=1
if [ "$DRY_RUN" -eq 1 ] || [ "$SKIP_NOTARIZE" -eq 1 ]; then NOTARIZE=0; fi

echo "Channel:   $CHANNEL"
echo "Version:   $VERSION (build $BUILD)"
echo "Feed:      $FEED_URL"
echo "Signing:   $CODESIGN_IDENTITY"
echo "Notarize:  $([ "$NOTARIZE" -eq 1 ] && echo "yes" || echo "skipped")"
echo "Publish:   $([ "$DRY_RUN" -eq 1 ] && echo "dry-run (no upload)" || echo "R2 ($CHANNEL)")"

# --- 1. Build + sign the app ------------------------------------------------

step "[1/7] building + signing Unpeel.app"
UNPEEL_VERSION="$VERSION" UNPEEL_BUILD="$BUILD" \
  UNPEEL_DEV_BUILD=0 \
  CODESIGN_IDENTITY="$CODESIGN_IDENTITY" SPARKLE_FEED_URL="$FEED_URL" \
  "$NATIVE_DIR/build-app.sh"

# --- 2. Notarize + staple the app -------------------------------------------
# Before packaging, so the app inside the DMG and the app inside the Sparkle
# ZIP both carry a stapled ticket (offline installs pass Gatekeeper). A ZIP
# cannot hold a staple, so submit a throwaway ZIP and staple the .app itself.

if [ "$NOTARIZE" -eq 1 ]; then
  step "[2/7] notarizing + stapling Unpeel.app"
  NOTARY_ZIP="$(mktemp -d)/Unpeel-notary.zip"
  ditto -c -k --keepParent "$DIST/Unpeel.app" "$NOTARY_ZIP"
  "$NATIVE_DIR/notarize-dmg.sh" "$NOTARY_ZIP" --staple "$DIST/Unpeel.app"
  rm -f "$NOTARY_ZIP"
else
  step "[2/7] skipping app notarization (dry-run/--skip-notarize) — app will NOT be stapled"
fi

# --- 3. Package the install DMG (from the stapled app) -----------------------

step "[3/7] packaging install DMG"
CODESIGN_IDENTITY="$CODESIGN_IDENTITY" "$NATIVE_DIR/make-dmg.sh"
DMG="$DIST/Unpeel-$VERSION.dmg"
cp "$DIST/Unpeel.dmg" "$DMG"

# --- 4. Notarize + staple the DMG --------------------------------------------

if [ "$NOTARIZE" -eq 1 ]; then
  step "[4/7] notarizing + stapling DMG"
  "$NATIVE_DIR/notarize-dmg.sh" "$DMG"
else
  step "[4/7] skipping DMG notarization (dry-run/--skip-notarize)"
fi

# --- 5. Sparkle self-update ZIP ---------------------------------------------

step "[5/7] creating Sparkle update ZIP"
# Real releases and rehearsals stage in different trees: a dry-run ZIP (often
# ad-hoc signed, never uploaded) sitting in the channel dir would otherwise be
# picked up by the next real generate_appcast run and advertised as a
# 404-ing — possibly higher-versioned — update.
if [ "$DRY_RUN" -eq 1 ]; then
  SPARKLE_DIR="$DIST/sparkle-dryrun/$CHANNEL"
else
  SPARKLE_DIR="$DIST/sparkle/$CHANNEL"
fi
# Start from a clean dir: generate_appcast signs EVERY archive it finds, and
# stale ZIPs/deltas from earlier runs would be advertised at URLs that were
# never published (deltas are not uploaded at all). One ZIP in, one item out.
rm -rf "$SPARKLE_DIR"
mkdir -p "$SPARKLE_DIR"
ZIP="$SPARKLE_DIR/Unpeel-$VERSION.zip"
ditto -c -k --keepParent "$DIST/Unpeel.app" "$ZIP"
# generate_appcast embeds a same-named .html next to the ZIP as the
# <description> (release notes) for that version. Always stage one: every
# update ends in a relaunch prompt, and sessions are hosted PTYs that
# survive it, so every dialog carries that reassurance (appended below the
# release's own notes). The notes body is --notes when given, otherwise it
# is derived from this version's website changelog section — preflight
# already guarantees the `## $VERSION` heading exists, so the dialog always
# shows what changed instead of the footer alone.
NOTES_HTML="$SPARKLE_DIR/Unpeel-$VERSION.html"
if [ -n "$NOTES" ]; then
  cp "$NOTES" "$NOTES_HTML"
else
  awk -v ver="$VERSION" '
    /^## / { if (found) exit }
    $0 == "## " ver || index($0, "## " ver " ") == 1 { found = 1; next }
    found { print }
  ' "$CHANGELOG_MD" | sed -E \
      -e 's/\*\*([^*]+)\*\*/<b>\1<\/b>/g' \
      -e 's/`([^`]+)`/<code>\1<\/code>/g' \
      -e 's/^- (.*)$/<li>\1<\/li>/' \
    | awk 'BEGIN { print "<ul>" } /^<li>/ { print } END { print "</ul>" }' \
    > "$NOTES_HTML"
fi
cat >> "$NOTES_HTML" <<'EOF'
<p><i>No need to wrap anything up — your terminals keep running during the update, and sessions reconnect automatically after Unpeel relaunches.</i></p>
EOF

# --- 6. EdDSA-sign + appcast ------------------------------------------------

step "[6/7] signing ZIP + generating appcast"
# generate_appcast reads the EdDSA private key from the login Keychain, signs
# the archive, and writes the appcast with enclosure URLs rooted at
# DOWNLOAD_PREFIX. Without -o it names the file after the app's feed URL
# (e.g. appcast-beta.xml); pin it to appcast.xml so the path is deterministic.
APPCAST="$SPARKLE_DIR/appcast.xml"
"$GENERATE_APPCAST" --download-url-prefix "$DOWNLOAD_PREFIX" -o "$APPCAST" "$SPARKLE_DIR"
[ -f "$APPCAST" ] || fail "generate_appcast did not produce $APPCAST"

# --- 7. Publish to Cloudflare R2 --------------------------------------------

step "[7/7] publishing to Cloudflare R2"
PUBLISH_ARGS=(
  --channel "$CHANNEL"
  --version "$VERSION"
  --build "$BUILD"
  --dmg "${DMG#"$REPO_ROOT"/}"
  --zip "${ZIP#"$REPO_ROOT"/}"
  --appcast "${APPCAST#"$REPO_ROOT"/}"
)
[ "$DRY_RUN" -eq 1 ] && PUBLISH_ARGS+=(--dry-run)
[ "$FORCE" -eq 1 ] && PUBLISH_ARGS+=(--force)
node "$REPO_ROOT/scripts/publish-cloudflare-release.mjs" "${PUBLISH_ARGS[@]}"

echo
if [ "$DRY_RUN" -eq 1 ]; then
  echo "Dry run complete. Artifacts staged under $DIST (nothing uploaded, app not notarized)."
else
  echo "Released $CHANNEL $VERSION."
  echo "  DMG:     $DMG"
  echo "  ZIP:     $ZIP"
  echo "  appcast: $FEED_URL"
fi
