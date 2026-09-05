#!/usr/bin/env bash
# Build and run the iOS unit suite in an actual simulator environment.
# The app target uses UIKit/Ghostty and is intentionally not a macOS target,
# so `swift test` on the host is not a valid iOS gate.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/UnpeelIOS" && pwd)"
DESTINATION="${UNPEEL_IOS_TEST_DESTINATION:-platform=iOS Simulator,name=iPhone 17 Pro,OS=latest}"
DERIVED_DATA="${UNPEEL_IOS_TEST_DERIVED_DATA:-${TMPDIR:-/tmp}/unpeel-ios-tests}"

xcodebuild \
  -project "$HERE/UnpeelIOS.xcodeproj" \
  -scheme UnpeelIOSApp \
  -configuration Debug \
  -destination "$DESTINATION" \
  -derivedDataPath "$DERIVED_DATA" \
  CODE_SIGNING_ALLOWED=NO \
  test
