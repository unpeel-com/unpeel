#!/bin/sh
# Unpeel CLI installer — served at https://unpeel.com/install.sh by the
# unpeel-release-updates worker (which substitutes __DEFAULT_CHANNEL__).
#
#   curl -fsSL https://unpeel.com/install.sh | sh
#
# Installs the `unpeel` CLI (the `unpeel serve` Host service plus scriptable
# session verbs) and its `unpeel-host` sibling (Sessions are hosted through
# it — resolve_host_binary looks for a sibling first)
# and, when the archive carries it (0.4.5+), the `unpeel-attach` terminal
# client; older archives without it still install.
# Tarballs live in the same R2 release bucket as the Mac app, under
# /releases/<channel>/cli/, published by scripts/release-cli.mjs.
#
# Overrides:
#   UNPEEL_CHANNEL      alpha | beta | stable   (default: __DEFAULT_CHANNEL__)
#   UNPEEL_INSTALL_DIR  target directory        (default: /usr/local/bin if
#                       writable, else ~/.local/bin)
set -eu

CHANNEL="${UNPEEL_CHANNEL:-__DEFAULT_CHANNEL__}"
# __BASE_URL__ is substituted by the worker with the origin the script was
# fetched from, so v1.unpeel.com hands out a script that installs from
# v1.unpeel.com — the whole preview lane stays self-contained.
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

url="$BASE/releases/$CHANNEL/cli/unpeel-latest-$target.tar.gz"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/unpeel-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Downloading unpeel ($CHANNEL, $target)…"
if ! curl -fsSL -o "$tmp/unpeel.tar.gz" "$url"; then
  echo "error: no prebuilt binary for $target on the $CHANNEL channel yet ($url)" >&2
  echo "       building from source: cargo build --release -p unpeel-cli -p unpeel-host" >&2
  exit 1
fi

# Integrity is mandatory: every release upload publishes this sidecar next to
# the mutable `-latest` alias. A missing/malformed sidecar must never turn into
# an unverified install.
if ! curl -fsSL -o "$tmp/unpeel.tar.gz.sha256" "$url.sha256"; then
  echo "error: checksum sidecar is unavailable for $url" >&2
  exit 1
fi
expected="$(awk 'NR == 1 { print $1 }' "$tmp/unpeel.tar.gz.sha256")"
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
  actual="$(sha256sum "$tmp/unpeel.tar.gz" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/unpeel.tar.gz" | awk '{print $1}')"
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

tar -xzf "$tmp/unpeel.tar.gz" -C "$tmp"
for bin in unpeel unpeel-host; do
  [ -f "$tmp/$bin" ] || { echo "error: $bin missing from archive" >&2; exit 1; }
done
extra_bins=""
[ -f "$tmp/unpeel-attach" ] && extra_bins="$tmp/unpeel-attach"

if [ -n "${UNPEEL_INSTALL_DIR:-}" ]; then
  dir="$UNPEEL_INSTALL_DIR"
  mkdir -p "$dir"
elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
  dir=/usr/local/bin
else
  dir="$HOME/.local/bin"
  mkdir -p "$dir"
fi

# shellcheck disable=SC2086
install -m 755 "$tmp/unpeel" "$tmp/unpeel-host" $extra_bins "$dir"

# Record where this install came from: the CLI's update check reads the
# channel from this marker (and stays silent for from-source builds, which
# never have one).
unpeel_home="${UNPEEL_HOME:-$HOME/.unpeel}"
mkdir -p "$unpeel_home"
printf '{"channel":"%s","install_dir":"%s","installed_at":"%s"}\n' \
  "$CHANNEL" "$dir" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$unpeel_home/cli-install.json"

ver="$("$dir/unpeel" --version 2>/dev/null || echo unpeel)"

# The block wordmark (unpeel-type.sh --type in the unpeel-mascot repo). On a
# live terminal, one wave of agent colors shimmers through the letters and
# settles back to the default color — the same effect as the website's
# wordmark hover. Piped/dumb terminals get the plain banner.
echo ""
U1='█   █'; N1='█▄  █'; P1='█▀▀▀█'; E1='█▀▀▀▀'; L1='█'
U2='█   █'; N2='█ ▀▄█'; P2='█▀▀▀▀'; E2='█▀▀▀ '; L2='█'
U3='▀▀▀▀▀'; N3='▀   ▀'; P3='▀    '; E3='▀▀▀▀▀'; L3='▀▀▀▀▀'
if [ -t 1 ] && [ "${TERM:-dumb}" != dumb ] && sleep 0.01 2>/dev/null; then
  # Agent palette (truecolor): claude, codex, green, kimi, cursor, kiro.
  C1='217;119;87'; C2='94;197;190'; C3='61;198;116'
  C4='79;168;255'; C5='170;110;245'; C6='193;154;255'
  # paint <frame> <letter#> <chunk>: the wave is two letters wide.
  paint() {
    if [ "$1" -ge "$2" ] && [ "$1" -le "$(($2 + 1))" ]; then
      eval "printf '\033[38;2;%sm%s\033[0m' \"\$C$2\" \"\$3\""
    else
      printf '%s' "$3"
    fi
  }
  f=0
  while [ "$f" -le 8 ]; do
    [ "$f" -gt 0 ] && printf '\033[3A'
    for r in 1 2 3; do
      eval "u=\$U$r; n=\$N$r; p=\$P$r; e=\$E$r; l=\$L$r"
      printf '%s %s %s %s %s %s\n' \
        "$(paint "$f" 1 "$u")" "$(paint "$f" 2 "$n")" "$(paint "$f" 3 "$p")" \
        "$(paint "$f" 4 "$e")" "$(paint "$f" 5 "$e")" "$(paint "$f" 6 "$l")"
    done
    sleep 0.06
    f=$((f + 1))
  done
else
  printf '%s %s %s %s %s %s\n' "$U1" "$N1" "$P1" "$E1" "$E1" "$L1"
  printf '%s %s %s %s %s %s\n' "$U2" "$N2" "$P2" "$E2" "$E2" "$L2"
  printf '%s %s %s %s %s %s\n' "$U3" "$N3" "$P3" "$E3" "$E3" "$L3"
fi
echo ""
echo "$ver ($CHANNEL) installed to $dir"

case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    echo ""
    echo "$dir is not on your PATH yet — add it with:"
    echo "  export PATH=\"$dir:\$PATH\""
    ;;
esac

echo ""
echo "Start it:"
echo "  unpeel serve    run the Host service on this machine (or open the Unpeel app)"
echo "  unpeel pair     pair a phone or another Mac with this Host"
echo "  unpeel --help   every command and flag"
echo ""
echo "Docs: https://unpeel.com/docs/cli"
