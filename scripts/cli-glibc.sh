#!/bin/sh
# Shared GLIBC release helpers. This file is sourced by build-cli-linux.sh;
# keep it POSIX so the version tests also run on macOS release workstations.

unpeel_highest_glibc_version() {
  sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' \
    | awk '
      function newer(left, right, left_n, right_n, count, i, left_v, right_v) {
        left_n = split(left, left_parts, ".")
        right_n = split(right, right_parts, ".")
        count = left_n > right_n ? left_n : right_n
        for (i = 1; i <= count; i++) {
          left_v = i <= left_n ? left_parts[i] + 0 : 0
          right_v = i <= right_n ? right_parts[i] + 0 : 0
          if (left_v > right_v) return 1
          if (left_v < right_v) return 0
        }
        return 0
      }
      NR == 1 { highest = $0; next }
      newer($0, highest) { highest = $0 }
      END { if (highest != "") print highest }
    '
}

unpeel_glibc_version_at_most() {
  actual=$1
  ceiling=$2
  awk -v actual="$actual" -v ceiling="$ceiling" '
    function valid(version) {
      return version ~ /^[0-9]+([.][0-9]+)*$/
    }
    BEGIN {
      if (!valid(actual) || !valid(ceiling)) exit 2
      actual_n = split(actual, actual_parts, ".")
      ceiling_n = split(ceiling, ceiling_parts, ".")
      count = actual_n > ceiling_n ? actual_n : ceiling_n
      for (i = 1; i <= count; i++) {
        actual_v = i <= actual_n ? actual_parts[i] + 0 : 0
        ceiling_v = i <= ceiling_n ? ceiling_parts[i] + 0 : 0
        if (actual_v < ceiling_v) exit 0
        if (actual_v > ceiling_v) exit 1
      }
      exit 0
    }
  '
}
