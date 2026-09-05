#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
# The protocol contracts and the runtime catalog come from the pinned server
# release (apps/native/SERVER_VERSION): vendor them first, then verify the
# committed catalog matches (UNPEEL_VENDOR_PROTOCOL=check only verifies).
if [ "${UNPEEL_VENDOR_PROTOCOL:-vendor}" = "check" ]; then
  "$HERE/vendor-protocol.sh" --check
else
  "$HERE/vendor-protocol.sh"
fi
"$HERE/build-rust-bridge.sh" debug
cd "$HERE/UnpeelNative"
swift test "$@"
