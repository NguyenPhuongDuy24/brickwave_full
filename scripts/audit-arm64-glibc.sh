#!/bin/sh
set -eu

binary=${1:?usage: audit-arm64-glibc.sh BINARY}
readelf_tool=${READELF:-aarch64-linux-gnu-readelf}

file "$binary"
versions=$($readelf_tool --version-info "$binary" \
    | grep -o 'GLIBC_[0-9.]*' \
    | sort -Vu)
printf '%s\n' "$versions"
highest=$(printf '%s\n' "$versions" | tail -n 1)
test "$highest" = GLIBC_2.33 || {
    printf 'unexpected maximum GLIBC version: %s\n' "$highest" >&2
    exit 1
}
