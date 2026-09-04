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

# The canonical seated mascot from the unpeel-mascot repository. Interactive
# terminals get its agent-color gradient; piped/dumb terminals get the same
# silhouette in three monochrome shades and no escape sequences.
mascot_color=false
if [ -t 1 ] && [ "${TERM:-dumb}" != dumb ]; then
  mascot_color=true
fi

# Set mascot_rgb to the mascot's horizontal six-stop gradient. Face and feet
# use the midpoint between the body color and white.
mascot_gradient() {
  mascot_gradient_pos=$(( ($1 - 1) * 500 ))
  [ "$mascot_gradient_pos" -lt 0 ] && mascot_gradient_pos=0
  [ "$mascot_gradient_pos" -gt 5000 ] && mascot_gradient_pos=5000
  mascot_gradient_segment=$(( mascot_gradient_pos / 1000 ))
  if [ "$mascot_gradient_segment" -ge 5 ]; then
    mascot_gradient_segment=4
  fi
  mascot_gradient_fraction=$(( mascot_gradient_pos - mascot_gradient_segment * 1000 ))
  case "$mascot_gradient_segment" in
    0) mascot_ar=217; mascot_ag=119; mascot_ab=87;  mascot_br=0;   mascot_bg=196; mascot_bb=196 ;;
    1) mascot_ar=0;   mascot_ag=196; mascot_ab=196; mascot_br=67;  mascot_bg=194; mascot_bb=81  ;;
    2) mascot_ar=67;  mascot_ag=194; mascot_ab=81;  mascot_br=79;  mascot_bg=168; mascot_bb=255 ;;
    3) mascot_ar=79;  mascot_ag=168; mascot_ab=255; mascot_br=76;  mascot_bg=125; mascot_bb=247 ;;
    4) mascot_ar=76;  mascot_ag=125; mascot_ab=247; mascot_br=155; mascot_bg=97;  mascot_bb=234 ;;
  esac
  mascot_r=$(( mascot_ar + (mascot_br - mascot_ar) * mascot_gradient_fraction / 1000 ))
  mascot_g=$(( mascot_ag + (mascot_bg - mascot_ag) * mascot_gradient_fraction / 1000 ))
  mascot_b=$(( mascot_ab + (mascot_bb - mascot_ab) * mascot_gradient_fraction / 1000 ))
  if [ "$2" = light ]; then
    mascot_r=$(( mascot_r + (255 - mascot_r) / 2 ))
    mascot_g=$(( mascot_g + (255 - mascot_g) / 2 ))
    mascot_b=$(( mascot_b + (255 - mascot_b) / 2 ))
  fi
  mascot_rgb="$mascot_r;$mascot_g;$mascot_b"
}

mascot_row() {
  mascot_cells=$1
  mascot_column=0
  printf '   '
  while [ -n "$mascot_cells" ]; do
    mascot_cell=${mascot_cells%"${mascot_cells#?}"}
    mascot_cells=${mascot_cells#?}
    case "$mascot_cell" in
      B)
        if $mascot_color; then printf '\033[38;2;0;0;0m██'; else printf '  '; fi
        ;;
      L)
        if $mascot_color; then
          mascot_gradient "$mascot_column" light
          printf '\033[38;2;%sm██' "$mascot_rgb"
        else
          printf '▓▓'
        fi
        ;;
      M|D)
        if $mascot_color; then
          mascot_gradient "$mascot_column" body
          printf '\033[38;2;%sm██' "$mascot_rgb"
        else
          printf '██'
        fi
        ;;
      *)
        if $mascot_color; then printf '\033[0m  '; else printf '  '; fi
        ;;
    esac
    mascot_column=$((mascot_column + 1))
  done
  if $mascot_color; then printf '\033[0m\n'; else printf '\n'; fi
}

echo ""
mascot_row '....DDDDD........'
mascot_row '...DDDDDDD.......'
mascot_row '..MLLLLLLLM......'
mascot_row '.LMLBLLLBLML.....'
mascot_row '.LMLBLLLBLMLMM...'
mascot_row '..MLLLLLLLM..M...'
mascot_row '...MMLLLMM....M..'
mascot_row '....MMMMM.....M..'
mascot_row '....MMMMMM....M..'
mascot_row '...MMMMMMMM..M...'
mascot_row '...MMMMMMMMMM....'
mascot_row '...MMMMMMMMM.....'
mascot_row '...LLMMMLLM......'
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
