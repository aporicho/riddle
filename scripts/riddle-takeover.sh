#!/bin/bash
# Launch the diary in full-takeover mode: stop xochitl, run riddle against the
# vendor e-ink engine (instant ink), ALWAYS restore xochitl on exit.
#
# Exit the diary: power button, 5-finger tap, or SIGTERM. Escape hatch if
# anything wedges: ssh root@10.11.99.1 'systemctl start xochitl'.

set -u

WAKELOCK=riddle-takeover
RUN_LOCK=/run/riddle-takeover.lock

# Resolve our own install directory so the bundle works wherever it lives.
HERE=$(cd "$(dirname "$0")" && pwd)

release_wakelock() {
    echo "$WAKELOCK" > /sys/power/wake_unlock 2>/dev/null || true
}

restore() {
    rm -f /tmp/epframebuffer.lock
    rmdir "$RUN_LOCK" 2>/dev/null || true
    # A persistent systemd unit owns restoration through ExecStopPost. Keep
    # the in-script fallback only for direct/legacy launches.
    if [ -z "${REMAGIC_SESSION:-}" ] && [ -z "${RIDDLE_SYSTEMD_MANAGED:-}" ]; then
        systemctl reset-failed xochitl.service paperweight.service 2>/dev/null || true
        systemctl start xochitl
    fi
    release_wakelock
}

# Fail before touching xochitl if the standalone bundle is incomplete.
if [ ! -x "$HERE/riddle" ]; then
    echo "riddle-takeover: missing executable: $HERE/riddle" >&2
    exit 1
fi
if [ ! -r "$HERE/libquill.so" ]; then
    echo "riddle-takeover: missing display library: $HERE/libquill.so" >&2
    exit 1
fi
if [ ! -r /usr/lib/plugins/scenegraph/libqsgepaper.so ]; then
    echo "riddle-takeover: missing device display engine" >&2
    exit 1
fi
if ! mkdir "$RUN_LOCK" 2>/dev/null; then
    echo "riddle-takeover: another takeover session is already active" >&2
    exit 1
fi

# Under the Remagic Home session host (REMAGIC_SESSION=1), xochitl is already
# stopped and the session owns its restore — skip our own stop/restart.
trap restore EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# xochitl normally prevents autosleep. Once it is stopped, keep the tablet
# awake so resume cannot start a second display engine over this one.
echo "$WAKELOCK" > /sys/power/wake_lock 2>/dev/null || true

# Oracle config: standalone installs keep secrets outside the app directory:
#   /home/root/.config/riddle/oracle.env
# A legacy oracle.env next to the binary remains supported as a fallback.
#   RIDDLE_OPENAI_KEY=sk-...
#   RIDDLE_OPENAI_BASE=https://api.openai.com/v1     # optional
#   RIDDLE_OPENAI_MODEL=gpt-4o-mini                  # optional
# Without it, riddle falls back to the pi backend (if pi is installed).
CONFIG=${RIDDLE_CONFIG:-/home/root/.config/riddle/oracle.env}
if [ -r "$CONFIG" ]; then
    set -a; . "$CONFIG"; set +a
elif [ -r "$HERE/oracle.env" ]; then
    set -a; . "$HERE/oracle.env"; set +a
fi

if [ -z "${REMAGIC_SESSION:-}" ]; then
    systemctl stop xochitl
fi
rm -f /tmp/epframebuffer.lock      # stale EPD lock blocks the engine
[ -z "${REMAGIC_SESSION:-}" ] && sleep 1

cd "$HERE"
# libquill.so ships in this bundle; libqsgepaper.so (reMarkable's proprietary
# engine) comes from the device's own scenegraph plugin dir. We search the
# bundle first, then a standalone /home/root/quill install, then the plugin dir.
LD_LIBRARY_PATH="$HERE:/home/root/quill:/usr/lib/plugins/scenegraph" \
    PAPERTERM_SHELL= HOME=/home/root \
    "$HERE/riddle"
status=$?
echo "riddle-takeover: diary closed ($status), restoring xochitl"
exit "$status"
