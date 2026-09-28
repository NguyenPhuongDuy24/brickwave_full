#!/bin/sh
set -eu

root=${1:?usage: probe-stockos-ffmpeg.sh ROOTFS}
export QEMU_LD_PREFIX="$root"
export LD_LIBRARY_PATH=/usr/trimui/lib:/usr/lib:/lib

run_ffmpeg() {
    timeout 20 qemu-aarch64 -L "$root" "$root/usr/bin/ffmpeg" -hide_banner "$@"
}

report_matches() {
    label=$1
    pattern=$2
    shift 2
    printf '[%s]\n' "$label"
    output=$(run_ffmpeg "$@" 2>&1) || {
        printf '%s\n' "$output"
        return 1
    }
    matches=$(printf '%s\n' "$output" | grep -E "$pattern" || true)
    if [ -n "$matches" ]; then
        printf '%s\n' "$matches"
    else
        printf '%s\n' MISSING
    fi
}

printf '%s\n' '[version]'
run_ffmpeg -version
report_matches protocols '(^|[[:space:]])(http|https|tls|tcp|crypto)([[:space:]]|$)' -protocols
report_matches demuxers '(^|[[:space:]])(hls|aac)([[:space:]]|$)' -demuxers
report_matches decoders '(^|[[:space:]])aac([[:space:]]|$)' -decoders
report_matches devices '(^|[[:space:]])alsa([[:space:]]|$)' -devices

printf '%s\n' '[curl]'
timeout 20 qemu-aarch64 -L "$root" "$root/usr/bin/curl" --version
printf '%s\n' '[aplay]'
timeout 20 qemu-aarch64 -L "$root" "$root/usr/bin/aplay" --version
