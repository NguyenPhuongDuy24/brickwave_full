#!/bin/sh
set -eu

project_root=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
sdk=${BRICKWAVE_TRIMUI_SDK:-/mnt/d/volte/spotify_port_analysis/spotifast_port/poc/sdk}
rootfs=${BRICKWAVE_STOCKOS_ROOTFS:-/mnt/d/volte/spotify_port_analysis/work/rootfs}
export PATH="$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="$sdk/pkgconfig"

cd "$project_root"
cargo zigbuild --locked --release --target aarch64-unknown-linux-gnu.2.33 \
    --features trimui-sdl2 --tests

test_binary=
for candidate in $(ls -1t target/aarch64-unknown-linux-gnu/release/deps/brickwave-*); do
    name=$(basename "$candidate")
    if printf '%s\n' "$name" | grep -Eq '^brickwave-[0-9a-f]{16}$' \
        && file "$candidate" | grep -q 'ELF 64-bit.*ARM aarch64'; then
        highest=$(aarch64-linux-gnu-readelf --version-info "$candidate" \
            | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -n 1)
        ceiling=$(printf '%s\n%s\n' "$highest" GLIBC_2.33 | sort -V | tail -n 1)
        if [ "$ceiling" = GLIBC_2.33 ]; then
            test_binary=$candidate
            break
        fi
    fi
done
test -n "$test_binary" || {
    printf '%s\n' 'ARM64 test binary was not produced' >&2
    exit 1
}

export QEMU_LD_PREFIX="$rootfs"
export LD_LIBRARY_PATH=/usr/trimui/lib:/usr/lib:/lib
qemu-aarch64 -L "$rootfs" "$test_binary" 'session_store::platform::tests' --nocapture
qemu-aarch64 -L "$rootfs" "$test_binary" 'trimui_input::tests' --nocapture
qemu-aarch64 -L "$rootfs" "$test_binary" 'playback_engine::tests' --nocapture
qemu-aarch64 -L "$rootfs" "$test_binary" 'hls_spool::tests' --nocapture
qemu-aarch64 -L "$rootfs" "$test_binary" \
    'state::tests::enabled_live_playback_requests_a_descriptor_and_keeps_queue_order' --nocapture
