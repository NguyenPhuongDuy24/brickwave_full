#!/bin/sh
# Brickwave for TrimUI Brick Pro StockOS. This launcher changes no firmware.
set -u

appdir=$(CDPATH= cd "$(dirname "$0")" && pwd) || exit 1
cd "$appdir" || exit 1
umask 077
mkdir -p logs run data || exit 1

log_file="$appdir/logs/brickwave.log"
lock_dir="$appdir/run/instance.lock"
pid_file="$lock_dir/launcher.pid"
display_marker="$appdir/run/screen-brightness"
stay_awake_marker="$appdir/run/stay-awake-owned"
stay_alive_marker="$appdir/run/stay-alive-owned"
led_snapshot_dir="$appdir/run/led-state-snapshot"
binary="$appdir/bin/brickwave"

exec >>"$log_file" 2>&1

log() {
    printf '%s component=launcher state=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$1"
}

fail() {
    log "ERROR reason=$1"
    exit 1
}

restore_display() {
    [ -r "$display_marker" ] || return 0

    saved_brightness=$(cat "$display_marker" 2>/dev/null || true)
    case "$saved_brightness" in
        0|1|2|3|4|5|6|7|8|9|10)
            if [ -d /tmp/system ] && printf '%s' "$saved_brightness" > /tmp/system/set_brightness; then
                log "DISPLAY_RESTORE brightness=$saved_brightness"
            else
                log "DISPLAY_RESTORE_ERROR reason=stockos-ipc-unavailable"
            fi
            ;;
        *)
            log "DISPLAY_RESTORE_ERROR reason=invalid-marker"
            ;;
    esac
    rm -f "$display_marker"
}

restore_led_state() {
    [ -d "$led_snapshot_dir" ] || return 0

    restored=0
    failed=0
    for attribute in \
        max_scale max_scale_lr max_scale_f1f2 max_scale_rear \
        effect_lr effect_m effect_f1 effect_f2 effect_rear \
        anim_frames_enable effect_enable
    do
        value_file="$led_snapshot_dir/$attribute"
        target="/sys/class/led_anim/$attribute"
        [ -r "$value_file" ] || continue
        value=$(cat "$value_file" 2>/dev/null || true)
        case "$value" in
            ''|*[!0-9]*)
                failed=$((failed + 1))
                ;;
            *)
                if [ "$value" -le 255 ] 2>/dev/null && [ -w "$target" ] \
                    && printf '%s\n' "$value" > "$target"; then
                    restored=$((restored + 1))
                else
                    failed=$((failed + 1))
                fi
                ;;
        esac
    done
    led_off=0
    for attribute in effect_lr effect_m effect_f1 effect_f2 effect_rear
    do
        target="/sys/class/led_anim/$attribute"
        if [ -w "$target" ] && printf '0\n' > "$target"; then
            led_off=$((led_off + 1))
        fi
    done
    rm -rf "$led_snapshot_dir"
    if [ "$failed" -eq 0 ]; then
        log "LED_RESTORE result=ok attributes=$restored"
    else
        log "LED_RESTORE_ERROR restored=$restored failed=$failed"
    fi
    if [ "$led_off" -eq 5 ]; then
        log "LED_EFFECTS state=off attributes=$led_off"
    else
        log "LED_EFFECTS_ERROR state=off attributes=$led_off"
    fi
}

cleanup_power() {
    restore_display
    restore_led_state
    # Remove the global StockOS guard only when this Brickwave instance left
    # its ownership marker. Do not disturb a guard owned by another service.
    if [ -e "$stay_awake_marker" ]; then
        rm -f /tmp/stay_awake
        rm -f "$stay_awake_marker"
        log "STAY_AWAKE state=released"
    fi
    if [ -e "$stay_alive_marker" ]; then
        rm -f /tmp/stay_alive
        rm -f "$stay_alive_marker"
        log "STAY_ALIVE state=released"
    fi
}

[ "$(uname -m)" = aarch64 ] || fail aarch64-required
[ -d /usr/trimui ] || fail stockos-runtime-missing

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

# Recover StockOS state after a previous crash or forced shutdown before the
# new process claims display power management.
cleanup_power

child_pid=
cleanup() {
    trap - EXIT TERM INT HUP
    if [ -n "$child_pid" ] && kill -0 "$child_pid" 2>/dev/null; then
        log "STOP_REQUEST pid=$child_pid"
        kill -TERM "$child_pid" 2>/dev/null || true
        wait "$child_pid" 2>/dev/null || true
    fi
    cleanup_power
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

[ -x "$binary" ] || fail binary-missing-or-not-executable
[ -x /lib/ld-linux-aarch64.so.1 ] || fail dynamic-loader-missing
[ -x /usr/trimui/bin/mplayer ] || fail stockos-mplayer-missing

export LD_LIBRARY_PATH="${LD_LIBRARY_PATH:+$LD_LIBRARY_PATH:}/usr/trimui/lib:/usr/lib:/lib"
export SDL_VIDEODRIVER="${SDL_VIDEODRIVER:-mali}"
# The UI does not open an SDL audio device. StockOS MPlayer remains the only
# ALSA owner. A bounded worker assembles each finite HLS VOD under run/ before
# MPlayer opens it, keeping network jitter out of the audio path. Completed
# files are reused only within this app session and are deleted on shutdown.
export SDL_AUDIODRIVER=dummy
export SOUNDCLOUD_MODE=live
export BRICKWAVE_DATA_DIR="$appdir/data"
export BRICKWAVE_RUNTIME_DIR="$appdir/run"
export BRICKWAVE_AUDIO_CACHE_MIB="${BRICKWAVE_AUDIO_CACHE_MIB:-256}"

log "APP_START build=00.4.29 mode=live platform=stockos session_store=atomic-file preferences=atomic-file audio=stockos-mplayer mixer=softvol hls_transport=session-audio-cache track_limit_mib=64 cache_limit_mib=$BRICKWAVE_AUDIO_CACHE_MIB cache_entries=16 liked_priority=true seek=waveform-local waveform=soundcloud-samples wake_recovery=wifi-grace,media-retry,incomplete-data-only backend_http=fresh-connections toast=auto-dismiss-5s render_policy=active60,efficient10/4,dimmed2,stockos-handoff stay_alive=app-lifetime stay_awake=always-on-only led_wake=effects-off always_on=stockos-logical-dim power_key=stockos-only menu_key=btn-mode-exit-confirm account_actions=like,add-owned-playlist,create-private-playlist,delete-owned-playlist,remove-playlist-track font_scale=1.18 sleep_timer=pause-preserve-position battery=sysfs-capacity formats=aac-hls,mp3-hls ui_modes=full,minimal controls=analog-pointer,a-click,dpad-scroll,b-back,menu-exit-confirm,y-select-pause,start-resume,l1-previous,r1-next"
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
