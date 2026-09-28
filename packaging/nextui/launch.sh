#!/bin/sh
# Brickwave standalone Tool Pak for NextUI on TrimUI Brick Pro.
set -u

pakdir=$(CDPATH= cd "$(dirname "$0")" && pwd) || exit 1
binary="$pakdir/bin/brickwave"

fallback_sd=${SDCARD_PATH:-/mnt/SDCARD}
nextui_userdata=${USERDATA_PATH:-$fallback_sd/.userdata/tg5040}
nextui_shared=${SHARED_USERDATA_PATH:-$fallback_sd/.userdata/shared}
nextui_logs=${LOGS_PATH:-$nextui_userdata/logs}
data_root="$nextui_shared/BrickwaveNextUI/data"
artwork_root="$nextui_shared/BrickwaveNextUI/artwork-cache"
runtime_root="$nextui_userdata/BrickwaveNextUI/run"
lock_dir="$runtime_root/instance.lock"
pid_file="$lock_dir/launcher.pid"

mkdir -p "$nextui_logs" "$data_root" "$artwork_root" "$runtime_root" || exit 1
log_file="$nextui_logs/brickwave-nextui.log"
exec >>"$log_file" 2>&1

log() {
    printf '%s component=brickwave-nextui-launcher state=%s\n' \
        "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$1"
}

fail() {
    log "ERROR reason=$1"
    exit 1
}

[ "${PLATFORM:-}" = "tg5040" ] || fail nextui-tg5040-required
[ "${DEVICE:-}" = "brickpro" ] || fail trimui-brick-pro-required
[ "${IS_NEXT:-}" = "yes" ] || fail nextui-runtime-required
[ -x "$binary" ] || fail binary-missing-or-not-executable
[ -x /lib/ld-linux-aarch64.so.1 ] || fail dynamic-loader-missing
[ -x /usr/trimui/bin/mplayer ] || fail stockos-mplayer-missing

if ! mkdir "$lock_dir" 2>/dev/null; then
    old_pid=
    [ -r "$pid_file" ] && old_pid=$(cat "$pid_file" 2>/dev/null || true)
    if [ -n "$old_pid" ] && kill -0 "$old_pid" 2>/dev/null; then
        fail app-already-running
    fi
    rm -f "$pid_file"
    rmdir "$lock_dir" 2>/dev/null || fail stale-lock-invalid
    mkdir "$lock_dir" || fail lock-create
fi
printf '%s\n' "$$" >"$pid_file" || fail pid-write

child_pid=
cleanup() {
    trap - EXIT TERM INT HUP
    if [ -n "$child_pid" ] && kill -0 "$child_pid" 2>/dev/null; then
        log "STOP_REQUEST pid=$child_pid"
        kill -TERM "$child_pid" 2>/dev/null || true
        wait "$child_pid" 2>/dev/null || true
    fi
    rm -f "$pid_file"
    rmdir "$lock_dir" 2>/dev/null || true
    log CLEANUP_COMPLETE
}

on_signal() {
    cleanup
    exit 128
}

trap cleanup EXIT
trap on_signal TERM INT HUP

cd "$pakdir" || fail pak-directory-unavailable
# Keep HOME from NextUI so stock MPlayer sees NextUI's .asoundrc routing.
export LD_LIBRARY_PATH="${SYSTEM_PATH:+$SYSTEM_PATH/lib:}/usr/trimui/lib:/usr/lib:/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export SDL_VIDEODRIVER=mali
export SDL_AUDIODRIVER=dummy
export SOUNDCLOUD_MODE=live
export BRICKWAVE_HOST=nextui
export BRICKWAVE_DATA_DIR="$data_root"
export BRICKWAVE_ARTWORK_CACHE_DIR="$artwork_root"
export BRICKWAVE_RUNTIME_DIR="$runtime_root"
export BRICKWAVE_AUDIO_CACHE_MIB="${BRICKWAVE_AUDIO_CACHE_MIB:-256}"

log "APP_START build=nextui-00.2.0 app=0.4.25 platform=${PLATFORM:-unknown} device=${DEVICE:-unknown} power_policy=nextui-owned data=separate-userdata artwork=persistent-cache audio=stockos-mplayer"
/lib/ld-linux-aarch64.so.1 "$binary" &
child_pid=$!
log "PROCESS_STARTED pid=$child_pid"

if wait "$child_pid"; then
    status=0
else
    status=$?
fi
child_pid=
log "APP_EXIT code=$status"
exit "$status"
