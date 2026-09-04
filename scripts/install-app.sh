#!/bin/sh
# __BIN__ installer — served at https://unpeel.com/install/__APP__/install.sh by
# the unpeel-release-updates worker (which substitutes __DEFAULT_CHANNEL__
# and the app placeholders).
#
#   curl -fsSL https://unpeel.com/install/__APP__/install.sh | sh
#
# Installs `__BIN__`, an Unpeel App (a standalone terminal tool that lights
# up inside Unpeel). Tarballs live in the same R2 release bucket as the Mac
# app, under /releases/<channel>/__APP__/, published by
# scripts/release-app.mjs.
#
# Overrides:
#   UNPEEL_CHANNEL      alpha | beta | stable   (default: __DEFAULT_CHANNEL__)
#   UNPEEL_INSTALL_DIR  target directory        (default: /usr/local/bin if
#                       writable, else ~/.local/bin)
set -eu

CHANNEL="${UNPEEL_CHANNEL:-__DEFAULT_CHANNEL__}"
BASE="${UNPEEL_INSTALL_BASE:-__BASE_URL__}"

case "$CHANNEL" in
  alpha|beta|stable) ;;
  *) echo "error: UNPEEL_CHANNEL must be alpha, beta, or stable (got: $CHANNEL)" >&2; exit 1 ;;
esac

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) target="macos-universal" ;;
  Linux)
    case "$arch" in
      arm64|aarch64) target="linux-aarch64" ;;
      x86_64|amd64) target="linux-x86_64" ;;
      *) echo "error: unsupported Linux architecture: $arch" >&2; exit 1 ;;
    esac
    ;;
  *) echo "error: unsupported platform: $os (macOS and Linux only)" >&2; exit 1 ;;
esac

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "error: tar is required" >&2; exit 1; }

url="$BASE/releases/$CHANNEL/__APP__/__BIN__-latest-$target.tar.gz"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/__BIN__-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Downloading __BIN__ ($CHANNEL, $target)…"
if ! curl -fsSL -o "$tmp/__BIN__.tar.gz" "$url"; then
  echo "error: no prebuilt binary for $target on the $CHANNEL channel yet ($url)" >&2
  echo "       building from source: cargo build --release in __BIN__" >&2
  exit 1
fi

# Integrity is mandatory: every publish uploads this sidecar next to the
# mutable `-latest` alias. A missing/malformed sidecar must never turn into
# an unverified install.
if ! curl -fsSL -o "$tmp/__BIN__.tar.gz.sha256" "$url.sha256"; then
  echo "error: checksum sidecar is unavailable for $url" >&2
  exit 1
fi
expected="$(awk 'NR == 1 { print $1 }' "$tmp/__BIN__.tar.gz.sha256")"
case "$expected" in
  *[!0-9a-fA-F]*|'')
    echo "error: invalid checksum sidecar for $url" >&2
    exit 1
    ;;
esac
if [ "${#expected}" -ne 64 ]; then
  echo "error: invalid checksum sidecar for $url" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/__BIN__.tar.gz" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/__BIN__.tar.gz" | awk '{print $1}')"
else
  echo "error: sha256sum or shasum is required to verify $url" >&2
  exit 1
fi
if [ "$actual" != "$expected" ]; then
  echo "error: checksum mismatch for $url" >&2
  echo "       expected $expected" >&2
  echo "       got      $actual" >&2
  exit 1
fi

tar -xzf "$tmp/__BIN__.tar.gz" -C "$tmp"
[ -f "$tmp/__BIN__" ] || { echo "error: __BIN__ missing from archive" >&2; exit 1; }

if [ -n "${UNPEEL_INSTALL_DIR:-}" ]; then
  dir="$UNPEEL_INSTALL_DIR"
  mkdir -p "$dir"
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
  dir=/usr/local/bin
else
  dir="$HOME/.local/bin"
  mkdir -p "$dir"
fi

install -m 755 "$tmp/__BIN__" "$dir"

echo ""
echo "__BIN__ installed to $dir/__BIN__"
echo "Unpeel detects it automatically while that directory is on PATH."
__TRY_LINES__
case ":$PATH:" in
  *:"$dir":*) ;;
  *) echo "note: $dir is not on your PATH" ;;
esac
