#!/bin/sh
# Deterministic Rust release paths. Source this file, then call
# unpeel_enable_rust_path_remapping with the checkout root before Cargo builds.

unpeel_enable_rust_path_remapping() {
  if [ "${UNPEEL_RUST_PATH_REMAP_ENABLED:-0}" = 1 ]; then
    return 0
  fi

  unpeel_remap_repo_root=$1
  unpeel_remap_cargo_home=${CARGO_HOME:-"${HOME:?HOME is required when CARGO_HOME is unset}/.cargo"}
  unpeel_remap_rust_sysroot=$(rustc --print sysroot)

  # `CARGO_ENCODED_RUSTFLAGS` takes precedence over `RUSTFLAGS`. Append to
  # whichever representation the caller already uses so no caller flags are
  # dropped. Plain RUSTFLAGS follows Cargo's own whitespace-splitting rules;
  # encoded flags also support checkout paths containing spaces.
  if [ "${CARGO_ENCODED_RUSTFLAGS+x}" = x ]; then
    unpeel_remap_separator=$(printf '\037')
    for unpeel_remap_flag in \
      "--remap-path-prefix=$unpeel_remap_repo_root=/unpeel/source" \
      "--remap-path-prefix=$unpeel_remap_cargo_home=/cargo" \
      "--remap-path-prefix=$unpeel_remap_rust_sysroot=/rust/toolchain"
    do
      if [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
        CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS}${unpeel_remap_separator}${unpeel_remap_flag}"
      else
        CARGO_ENCODED_RUSTFLAGS=$unpeel_remap_flag
      fi
    done
    export CARGO_ENCODED_RUSTFLAGS
  else
    for unpeel_remap_flag in \
      "--remap-path-prefix=$unpeel_remap_repo_root=/unpeel/source" \
      "--remap-path-prefix=$unpeel_remap_cargo_home=/cargo" \
      "--remap-path-prefix=$unpeel_remap_rust_sysroot=/rust/toolchain"
    do
      RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$unpeel_remap_flag"
    done
    export RUSTFLAGS
  fi

  UNPEEL_RUST_PATH_REMAP_ENABLED=1
  export UNPEEL_RUST_PATH_REMAP_ENABLED
}
