#!/usr/bin/env bash
set -euo pipefail

package_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
swift build --package-path "$package_dir"
bin_dir="$(swift build --package-path "$package_dir" --show-bin-path)"
app_path="$bin_dir/App Kit Kitchen Sink.app"

case "$app_path" in
    "$package_dir"/.build/*/"App Kit Kitchen Sink.app") ;;
    *)
        echo "Refusing to replace unexpected app path: $app_path" >&2
        exit 1
        ;;
esac

if [[ -e "$app_path" ]]; then
    rm -rf -- "$app_path"
fi
mkdir -p "$app_path/Contents/MacOS"
cp "$bin_dir/KitchenSink" "$app_path/Contents/MacOS/KitchenSink"
cp "$package_dir/Support/Info.plist" "$app_path/Contents/Info.plist"
codesign --force --sign - "$app_path"

open_args=(-n)
if [[ -n "${UNPEEL_KITCHEN_SINK_SESSION:-}" ]]; then
    open_args+=(--env "UNPEEL_KITCHEN_SINK_SESSION=$UNPEEL_KITCHEN_SINK_SESSION")
fi
open "${open_args[@]}" "$app_path"
