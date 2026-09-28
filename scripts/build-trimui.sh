#!/bin/sh
set -eu

project_root=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
sdk=${BRICKWAVE_TRIMUI_SDK:-/mnt/d/volte/spotify_port_analysis/spotifast_port/poc/sdk}
export PATH="$HOME/.cargo/bin:$PATH"

test -f "$sdk/pkgconfig/sdl2.pc" || {
    printf 'missing TrimUI SDL2 pkg-config file: %s\n' "$sdk/pkgconfig/sdl2.pc" >&2
    exit 1
}
command -v zig >/dev/null 2>&1 || {
    printf '%s\n' 'zig is required' >&2
    exit 1
}
command -v cargo-zigbuild >/dev/null 2>&1 || {
    printf '%s\n' 'cargo-zigbuild is required' >&2
    exit 1
}

test -n "${SOUNDCLOUD_BACKEND_URL:-}" || {
    printf '%s\n' 'SOUNDCLOUD_BACKEND_URL is required for a LIVE TrimUI build' >&2
    exit 1
}
case "$SOUNDCLOUD_BACKEND_URL" in
    https://*) ;;
    *)
        printf '%s\n' 'SOUNDCLOUD_BACKEND_URL must use HTTPS' >&2
        exit 1
        ;;
esac

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="$sdk/pkgconfig"

cd "$project_root"
cargo zigbuild --locked --release --target aarch64-unknown-linux-gnu.2.33 --features trimui-sdl2
