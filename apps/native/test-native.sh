#!/usr/bin/env bash
# Native app unit suite: builds the debug bridge from this tree, then runs
# `swift test` in apps/native/UnpeelNative. The Swift conformance tests read
# the protocol contracts from this checkout's protocol/ directory (walking up
# from their own file; UNPEEL_PROTOCOL_DIR overrides), and the runtime catalog
# copy in apps/shared is checked against runtimes/ by `bun run check:runtimes`.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
"$HERE/build-rust-bridge.sh" debug
cd "$HERE/UnpeelNative"
swift test "$@"
