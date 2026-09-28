#!/usr/bin/env bash
set -euo pipefail

test -n "${BRICKWAVE_STREAM_PROBE_URL:-}" || {
    printf '%s\n' 'BRICKWAVE_STREAM_PROBE_URL is required' >&2
    exit 1
}

rootfs=${BRICKWAVE_STOCKOS_ROOTFS:-/mnt/d/volte/spotify_port_analysis/work/rootfs}
export QEMU_LD_PREFIX="$rootfs"
export LD_LIBRARY_PATH=/usr/trimui/lib:/usr/lib:/lib

timeout 35 qemu-aarch64 -L "$rootfs" "$rootfs/usr/trimui/bin/mplayer" \
    -noconsolecontrols -vo null -ao null -endpos 5 \
    "$BRICKWAVE_STREAM_PROBE_URL" 2>&1 \
    | tr '\r' '\n' \
    | sed -E 's#https://[^[:space:]]+#<signed-url-redacted>#g' \
    | grep -E '^(Playing |libavformat version|mp3_found|\[lavf\] stream|Opening audio decoder|AUDIO:|Selected audio codec|AO:|Starting playback|A: *5\.|Exiting)'
