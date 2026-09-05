#!/bin/bash
#
# notarize-dmg.sh — submit a release artifact (DMG, or a ZIP of the app) to
# Apple's notary service, verify the verdict, staple the ticket, and validate
# Gatekeeper acceptance.
#
# The submitted file and the stapled target can differ: a .zip cannot carry a
# staple, so for app notarization you submit the ZIP and staple the .app.
#
# Preferred setup:
#   xcrun notarytool store-credentials unpeel-notary \
#     --apple-id you@example.com \
#     --team-id TEAMID1234
#
# Usage:
#   NOTARY_KEYCHAIN_PROFILE=unpeel-notary apps/native/notarize-dmg.sh
#   NOTARY_KEYCHAIN_PROFILE=unpeel-notary apps/native/notarize-dmg.sh apps/native/dist/Unpeel.dmg
#   # submit a ZIP of the app, staple the app itself:
#   NOTARY_KEYCHAIN_PROFILE=unpeel-notary apps/native/notarize-dmg.sh \
#     /tmp/Unpeel-notary.zip --staple apps/native/dist/Unpeel.app
#
# Alternative: an App Store Connect API key (most reliable headless — the
# keychain profile lives in the data-protection keychain, which can be
# unreadable from non-UI sessions; the key path carries no secret in argv):
#   NOTARY_KEY_PATH=~/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8 \
#   NOTARY_KEY_ID=XXXXXXXXXX \
#   NOTARY_ISSUER=00000000-0000-0000-0000-000000000000 \
#   apps/native/notarize-dmg.sh
#
# Alternative non-interactive credentials (prefer the keychain profile — the
# trio puts the app-specific password in the process argv, visible in ps):
#   NOTARY_APPLE_ID=you@example.com \
#   NOTARY_TEAM_ID=TEAMID1234 \
#   NOTARY_PASSWORD=app-specific-password \
#   apps/native/notarize-dmg.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARTIFACT="$REPO_ROOT/apps/native/dist/Unpeel.dmg"
STAPLE_TARGET=""
NOTARY_TIMEOUT="${NOTARY_TIMEOUT:-30m}"

while [ $# -gt 0 ]; do
  case "$1" in
    --staple) STAPLE_TARGET="$2"; shift 2 ;;
    *) ARTIFACT="$1"; shift ;;
  esac
done
[ -n "$STAPLE_TARGET" ] || STAPLE_TARGET="$ARTIFACT"

step() { echo "==> $*"; }

[ -e "$ARTIFACT" ] || { echo "FAIL: artifact not found: $ARTIFACT" >&2; exit 1; }
[ -e "$STAPLE_TARGET" ] || { echo "FAIL: staple target not found: $STAPLE_TARGET" >&2; exit 1; }

NOTARY_ARGS=()
if [ -n "${NOTARY_KEY_PATH:-}" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER:-}" ]; then
  NOTARY_ARGS+=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
elif [ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]; then
  NOTARY_ARGS+=(--keychain-profile "$NOTARY_KEYCHAIN_PROFILE")
elif [ -n "${NOTARY_APPLE_ID:-}" ] && [ -n "${NOTARY_TEAM_ID:-}" ] && [ -n "${NOTARY_PASSWORD:-}" ]; then
  NOTARY_ARGS+=(--apple-id "$NOTARY_APPLE_ID" --team-id "$NOTARY_TEAM_ID" --password "$NOTARY_PASSWORD")
else
  cat >&2 <<'EOF'
FAIL: missing notary credentials.

Recommended:
  xcrun notarytool store-credentials unpeel-notary \
    --apple-id you@example.com \
    --team-id TEAMID1234

Then run:
  NOTARY_KEYCHAIN_PROFILE=unpeel-notary apps/native/notarize-dmg.sh
EOF
  exit 1
fi

if [ -n "${NOTARY_KEYCHAIN:-}" ]; then
  NOTARY_ARGS+=(--keychain "$NOTARY_KEYCHAIN")
fi

json_field() { # json_field <key> — first string value for the key from stdin
  sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -n1
}

step "submitting $ARTIFACT to Apple notary service"
# --wait exits 0 even when the final verdict is Invalid on some notarytool
# versions, so parse the JSON verdict instead of trusting the exit code.
set +e
SUBMIT_JSON="$(xcrun notarytool submit "$ARTIFACT" "${NOTARY_ARGS[@]}" \
  --wait --timeout "$NOTARY_TIMEOUT" --output-format json 2>&1)"
SUBMIT_RC=$?
set -e
echo "$SUBMIT_JSON"

SUBMISSION_ID="$(printf '%s' "$SUBMIT_JSON" | json_field id)"
VERDICT="$(printf '%s' "$SUBMIT_JSON" | json_field status)"

if [ "$SUBMIT_RC" -ne 0 ] || [ "$VERDICT" != "Accepted" ]; then
  echo "FAIL: notarization did not complete with status Accepted (status: ${VERDICT:-unknown}, exit: $SUBMIT_RC)" >&2
  if [ -n "$SUBMISSION_ID" ]; then
    echo "==> fetching notary log for $SUBMISSION_ID" >&2
    xcrun notarytool log "$SUBMISSION_ID" "${NOTARY_ARGS[@]}" >&2 || true
  fi
  exit 1
fi

step "stapling notary ticket onto $STAPLE_TARGET"
xcrun stapler staple "$STAPLE_TARGET"

step "validating stapled ticket"
xcrun stapler validate "$STAPLE_TARGET"

step "checking Gatekeeper assessment"
case "$STAPLE_TARGET" in
  # DMGs need the primary-signature context or newer macOS rejects the
  # assessment with the false negative "Insufficient Context".
  *.dmg) spctl -a -vv -t open --context context:primary-signature "$STAPLE_TARGET" ;;
  *.app) spctl -a -vv "$STAPLE_TARGET" ;;
esac

echo
echo "Notarized and stapled: $STAPLE_TARGET"
