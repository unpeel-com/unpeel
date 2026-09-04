#!/usr/bin/env bash
# A stand-in for `cua-driver` so a Linux `unpeel serve` in a headless desktop
# session (Xvfb) advertises computer use without the real engine. It speaks
# exactly the subset the Host and the `computer` MCP domain use:
#
#   fake-cua-driver serve --embedded --socket <path>   create the socket, run
#   fake-cua-driver status --socket <path>             exit 0 while the socket exists
#   fake-cua-driver stop   --socket <path>             remove the socket
#   fake-cua-driver call <tool> '<json>' --socket <path> [--screenshot-out-file <png>]
#       get_window_state  → a fixed, non-degraded element tree (and, like
#                           the engine, a 1×1 PNG when --screenshot-out-file
#                           is given — that is how `see` captures)
#       screenshot        → writes a 1×1 PNG to --screenshot-out-file
#       anything else     → {"ok":true,"tool":"<tool>"}
#
# Point the Host at it with UNPEEL_CUA_DRIVER_BIN=$PWD/scripts/ci/fake-cua-driver.sh.
# Used by the Lane C D2 proof and Lane E's matrix case (unpeel-apple:docs/plans/computer-use-release.md).
set -euo pipefail

subcommand="${1:-}"
shift || true

socket=""
screenshot_out=""
positional=()
while (($#)); do
  case "$1" in
    --socket) socket="$2"; shift 2 ;;
    --screenshot-out-file) screenshot_out="$2"; shift 2 ;;
    --embedded) shift ;;
    *) positional+=("$1"); shift ;;
  esac
done

# FAKE_CUA_DRIVER_LOG=<file>: append one line per invocation ("<subcommand>
# <tool>") so a test can prove which engine calls a Host made (for example
# `call end_session` from `unpeel-host __computer_cleanup__` on Remove).
if [[ -n "${FAKE_CUA_DRIVER_LOG:-}" ]]; then
  printf '%s %s\n' "$subcommand" "${positional[0]:-}" >> "$FAKE_CUA_DRIVER_LOG" 2>/dev/null || true
fi

write_png() {
  [[ -n "$screenshot_out" ]] || return 0
  mkdir -p "$(dirname "$screenshot_out")"
  # A valid 1×1 opaque PNG.
  printf '\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde\x00\x00\x00\x0cIDATx\x9cc\xf8\xcf\xc0\x00\x00\x03\x01\x01\x00\x18\xdd\x8d\xb0\x00\x00\x00\x00IEND\xaeB`\x82' > "$screenshot_out"
}

case "$subcommand" in
  serve)
    [[ -n "$socket" ]] || { echo "fake-cua-driver: --socket is required" >&2; exit 2; }
    mkdir -p "$(dirname "$socket")"
    rm -f "$socket"
    # A real UNIX socket so `exists()` and any connect probe behave like the
    # engine's; nothing is ever read from it.
    python3 - "$socket" <<'PY' &
import os, signal, socket, sys, time
path = sys.argv[1]
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(path)
server.listen(4)
os.chmod(path, 0o600)
def bye(*_):
    try:
        os.remove(path)
    finally:
        sys.exit(0)
signal.signal(signal.SIGTERM, bye)
signal.signal(signal.SIGINT, bye)
parent = os.getppid()
# Never outlive the engine process or the socket: a Host that is SIGKILLed
# (the matrix sweep) or that ran `stop --socket` must leave nothing behind.
while os.getppid() == parent and os.path.exists(path):
    time.sleep(0.5)
bye()
PY
    child=$!
    trap 'kill "$child" 2>/dev/null; rm -f "$socket"; exit 0' TERM INT
    wait "$child"
    ;;
  status)
    [[ -n "$socket" && -S "$socket" ]]
    ;;
  stop)
    [[ -n "$socket" ]] && rm -f "$socket"
    ;;
  doctor)
    printf '{"ok":true,"fake":true,"display":"%s"}\n' "${DISPLAY:-}"
    ;;
  call)
    tool="${positional[0]:-}"
    case "$tool" in
      get_window_state)
        write_png
        cat <<'JSON'
{"ok":true,"degraded":false,"window":{"title":"Fake Window","app":"fake","bounds":{"x":0,"y":0,"width":800,"height":600}},"elements":[{"id":"e1","role":"button","label":"OK","bounds":{"x":10,"y":10,"width":80,"height":30}},{"id":"e2","role":"textfield","label":"Name","bounds":{"x":10,"y":50,"width":200,"height":30}}]}
JSON
        ;;
      screenshot)
        write_png
        printf '{"ok":true,"screenshot":"%s"}\n' "$screenshot_out"
        ;;
      *)
        printf '{"ok":true,"tool":"%s"}\n' "$tool"
        ;;
    esac
    ;;
  --version|version)
    echo "fake-cua-driver 0.0.0"
    ;;
  *)
    echo "fake-cua-driver: unknown subcommand '$subcommand'" >&2
    exit 2
    ;;
esac
