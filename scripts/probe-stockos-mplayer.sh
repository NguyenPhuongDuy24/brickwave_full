#!/bin/sh
set -eu

root=${1:?usage: probe-stockos-mplayer.sh ROOTFS}
binary="$root/usr/trimui/bin/mplayer"
export QEMU_LD_PREFIX="$root"
export LD_LIBRARY_PATH=/usr/trimui/lib:/usr/lib:/lib

run_mplayer() {
    timeout 20 qemu-aarch64 -L "$root" "$binary" "$@"
}

printf '%s\n' '[version]'
run_mplayer -help 2>&1 | head -n 8 || true
printf '%s\n' '[audio-outputs]'
run_mplayer -ao help 2>&1 || true
printf '%s\n' '[demuxers]'
run_mplayer -demuxer help 2>&1 | grep -Ei 'hls|aac|lavf|ffmpeg' || true
printf '%s\n' '[audio-codecs]'
run_mplayer -ac help 2>&1 | grep -Ei 'aac' | head -40 || true
printf '%s\n' '[protocol-symbols]'
strings "$binary" | grep -E 'https://|ff_hls_demuxer|hls,applehttp|libcurl|tls_' | head -40 || true
