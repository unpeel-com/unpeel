#!/usr/bin/env bash
# Run `unpeel serve` on a Linux box or container as a computer-use-capable
# Host WITHOUT the real engine: an Xvfb display plus scripts/ci/fake-cua-driver.sh
# make the worker advertise `computerUseAvailable` in its bootstrap, so a
# Controller (a release Mac app, the phone, the matrix) can exercise the
# Computer tab, the launch gate, and approvals end to end. This is the
# "fake Host bootstrap fixture (stub driver)" from the Lane C D2 proof in
# the private "computer-use-release" design record; Lane E's matrix case and the Box recipe
# start from the same shape.
#
#   UNPEEL_BIN_DIR=~/cu-target/debug ./scripts/ci/computer-use-linux-host.sh [UNPEEL_HOME]
#
# Requires: Xvfb, python3, and `unpeel` + `unpeel-host` in UNPEEL_BIN_DIR (or on
# PATH). Pair from a Controller with `unpeel pair` in another shell against the
# same UNPEEL_HOME. Stop with Ctrl-C; Xvfb and the stub exit with the Host.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
home="${1:-${UNPEEL_HOME:-$HOME/.unpeel-cu-proof}}"
bin_dir="${UNPEEL_BIN_DIR:-}"
display="${UNPEEL_CU_DISPLAY:-:97}"

command -v Xvfb >/dev/null || { echo "Xvfb is required (apt-get install xvfb)" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }
if [[ -n "$bin_dir" ]]; then export PATH="$bin_dir:$PATH"; fi
command -v unpeel >/dev/null || { echo "unpeel not on PATH (set UNPEEL_BIN_DIR)" >&2; exit 1; }
command -v unpeel-host >/dev/null || { echo "unpeel-host not on PATH (set UNPEEL_BIN_DIR)" >&2; exit 1; }

mkdir -p "$home"
Xvfb "$display" -screen 0 1280x800x24 -nolisten tcp >"$home/xvfb.log" 2>&1 &
xvfb_pid=$!
trap 'kill "$xvfb_pid" 2>/dev/null || true' EXIT INT TERM
sleep 0.5

export DISPLAY="$display"
export UNPEEL_HOME="$home"
export UNPEEL_CUA_DRIVER_BIN="$here/fake-cua-driver.sh"
# The worker only starts the engine once the experiment is on and access is
# not Off; both are Host-owned settings, so a Controller (or this CLI) sets
# them. Availability is advertised regardless — that is what the D2 gate reads.
unpeel settings set computer_use true >/dev/null
unpeel settings set computer_access ask >/dev/null

echo "Host home: $home"
echo "display:   $DISPLAY (Xvfb pid $xvfb_pid)"
echo "driver:    $UNPEEL_CUA_DRIVER_BIN"
echo "pair from another shell: UNPEEL_HOME=$home unpeel pair"
exec unpeel serve
