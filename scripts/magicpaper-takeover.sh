#!/bin/bash
# Launch the diary in full-takeover mode: stop xochitl, run magicpaper against the
# vendor e-ink engine (instant ink), ALWAYS restore xochitl on exit.
#
# Exit the diary: power button, 5-finger tap, or SIGTERM. Escape hatch if
# anything wedges: ssh root@10.11.99.1 'systemctl start xochitl'.

set -u

WAKELOCK=magicpaper-takeover
RUN_LOCK=/run/magicpaper-takeover.lock
READER_REQUEST=/run/magicpaper-koreader.request
READER_ERROR=/run/magicpaper-koreader.error
RM_LIBRARY=/home/root/.local/share/remarkable/xochitl
KO_LIBRARY=/home/root/koreader

# Resolve our own install directory so the bundle works wherever it lives.
HERE=$(cd "$(dirname "$0")" && pwd)

release_wakelock() {
    echo "$WAKELOCK" > /sys/power/wake_unlock 2>/dev/null || true
}

# xochitl may be pulled back in by a vendor health/recovery path while Quill
# owns the panel. A runtime-only mask makes that impossible without changing
# the stock unit on disk. `/run` is cleared at boot, and both restore paths
# below explicitly remove the mask as an additional safety net.
block_xochitl() {
    systemctl mask --runtime xochitl.service >/dev/null 2>&1 || true
}

allow_xochitl() {
    systemctl unmask --runtime xochitl.service >/dev/null 2>&1 || true
}

restore() {
    rm -f "$READER_REQUEST"
    rm -f /tmp/epframebuffer.lock
    rmdir "$RUN_LOCK" 2>/dev/null || true
    allow_xochitl
    # A persistent systemd unit owns restoration through ExecStopPost. Keep
    # the in-script fallback only for direct/legacy launches.
    if [ -z "${REMAGIC_SESSION:-}" ] && [ -z "${MAGICPAPER_SYSTEMD_MANAGED:-}" ]; then
        systemctl reset-failed xochitl.service paperweight.service 2>/dev/null || true
        systemctl start xochitl
    fi
    release_wakelock
}

# Fail before touching xochitl if the standalone bundle is incomplete.
if [ ! -x "$HERE/magicpaper" ]; then
    echo "magicpaper-takeover: missing executable: $HERE/magicpaper" >&2
    exit 1
fi
if [ ! -r "$HERE/libquill.so" ]; then
    echo "magicpaper-takeover: missing display library: $HERE/libquill.so" >&2
    exit 1
fi
if [ ! -r /usr/lib/plugins/scenegraph/libqsgepaper.so ]; then
    echo "magicpaper-takeover: missing device display engine" >&2
    exit 1
fi
if ! mkdir "$RUN_LOCK" 2>/dev/null; then
    echo "magicpaper-takeover: another takeover session is already active" >&2
    exit 1
fi

# Under the ReMagic Home session host (REMAGIC_SESSION=1), xochitl is already
# stopped and the session owns its restore — skip our own stop/restart.
trap restore EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# xochitl normally prevents autosleep. Once it is stopped, keep the tablet
# awake so resume cannot start a second display engine over this one.
echo "$WAKELOCK" > /sys/power/wake_lock 2>/dev/null || true

# Oracle config: standalone installs keep secrets outside the app directory:
#   /home/root/.config/magicpaper/oracle.env
# A legacy oracle.env next to the binary remains supported as a fallback.
#   MAGICPAPER_OPENAI_KEY=sk-...
#   MAGICPAPER_OPENAI_BASE=https://api.openai.com/v1     # optional
#   MAGICPAPER_OPENAI_MODEL=gpt-4o-mini                  # optional
# Without it, magicpaper falls back to the pi backend (if pi is installed).
CONFIG=${MAGICPAPER_CONFIG:-/home/root/.config/magicpaper/oracle.env}
if [ "${MAGICPAPER_TEST_MODE:-}" = 1 ]; then
    # Device automation must not even parse the owner's credential file. The
    # Rust process independently selects its deterministic offline backend.
    unset MAGICPAPER_OPENAI_KEY MAGICPAPER_OCR_TOKEN
elif [ -r "$CONFIG" ]; then
    set -a; . "$CONFIG"; set +a
elif [ -r "$HERE/oracle.env" ]; then
    set -a; . "$HERE/oracle.env"; set +a
fi

if [ -z "${REMAGIC_SESSION:-}" ]; then
    systemctl stop xochitl
    block_xochitl
fi
rm -f /tmp/epframebuffer.lock      # stale EPD lock blocks the engine
[ -z "${REMAGIC_SESSION:-}" ] && sleep 1

cd "$HERE"

find_koreader() {
    for script in \
        /home/root/.paperweight/services/koreader/koreader/koreader.sh \
        /home/root/xovi/exthome/appload/koreader/koreader.sh \
        /home/root/koreader/koreader.sh
    do
        [ -x "$script" ] && { echo "$script"; return 0; }
    done
    return 1
}

reader_error() {
    printf '%s\n' "$1" > "$READER_ERROR"
    echo "magicpaper-takeover: $1" >&2
}

launch_koreader() {
    target=$1
    case "$target" in
        "$RM_LIBRARY"|"$RM_LIBRARY"/*|"$KO_LIBRARY"|"$KO_LIBRARY"/*) ;;
        *) reader_error "閱讀路徑不安全，已取消開啟。"; return 1 ;;
    esac
    if [ ! -e "$target" ]; then
        reader_error "要閱讀的書已不存在。"
        return 1
    fi
    if ! ko_script=$(find_koreader); then
        reader_error "沒有找到已安裝的 KOReader。"
        return 1
    fi
    einkface=/home/root/.paperweight/services/core/einkfaceclient
    if [ ! -r "$einkface" ]; then
        reader_error "沒有找到 KOReader 所需的 einkface 顯示橋接器。"
        return 1
    fi

    ko_dir=${ko_script%/*}
    echo "magicpaper-takeover: starting KOReader — $target"
    rm -f /tmp/epframebuffer.lock

    # The Move build of KOReader intentionally refuses a naked framebuffer.
    # Start xochitl as the display host, wait for Paperweight's einkface socket,
    # then inject only the local client shim. This mirrors the installed app's
    # qtfb contract without using Paperweight's gated CLI or MCP service.
    allow_xochitl
    systemctl reset-failed xochitl.service paperweight.service 2>/dev/null || true
    if ! pidof xochitl >/dev/null 2>&1; then
        systemctl restart xochitl
    else
        systemctl start xochitl
    fi
    attempts=0
    while { [ ! -S /tmp/pwei-einkface.sock ] \
        || ! pidof xochitl >/dev/null 2>&1 \
        || ! pidof core >/dev/null 2>&1; } \
        && [ "$attempts" -lt 60 ]
    do
        sleep 0.1
        attempts=$((attempts + 1))
    done
    if [ ! -S /tmp/pwei-einkface.sock ] \
        || ! pidof xochitl >/dev/null 2>&1 \
        || ! pidof core >/dev/null 2>&1
    then
        systemctl stop xochitl 2>/dev/null || true
        block_xochitl
        reader_error "KOReader 顯示橋接器沒有就緒。"
        return 1
    fi

    (
        cd "$ko_dir" || exit 126
        LD_PRELOAD="$einkface" \
            EINKFACE_WINDOW_TITLE=KOReader \
            EINKFACE_SHIM_MODEL=false \
            EINKFACE_SHIM_FB=true \
            EINKFACE_SHIM_INPUT=false \
            EINK_WIDTH=960 EINK_HEIGHT=1696 \
            QTFB_SHIM_MODEL=false \
            QTFB_SHIM_INPUT_MODE=NATIVE \
            QTFB_SHIM_MODE=N_RGB565 \
            QTFB_SHIM_RESPECT_FULL_REFRESH_REQUESTS=1 \
            KO_DONT_GRAB_INPUT=1 \
            KO_DONT_SET_DEPTH=1 \
            KOREADER_DIR="$ko_dir" HOME=/home/root LC_ALL=en_US.UTF-8 \
            "$ko_script" "$target"
    )
    ko_status=$?
    systemctl stop xochitl 2>/dev/null || true
    block_xochitl
    rm -f /tmp/epframebuffer.lock
    if [ "$ko_status" -ne 0 ]; then
        reader_error "KOReader 異常退出，已返回 MagicPaper。"
    else
        echo "magicpaper-takeover: KOReader closed; returning to MagicPaper"
    fi
    # Give xochitl/einkface a brief moment to release the panel before libquill
    # reopens the vendor display engine.
    sleep 1
    return 0
}

# Keep one systemd-owned session across app switches. This preserves the
# wakelock and the ExecStopPost safety net while ensuring only one program owns
# the display and input devices at a time.
while true; do
    rm -f "$READER_REQUEST"
    # libquill.so ships in this bundle; libqsgepaper.so (reMarkable's
    # proprietary engine) comes from the device's scenegraph plugin directory.
    LD_LIBRARY_PATH="$HERE:/home/root/quill:/usr/lib/plugins/scenegraph" \
        PAPERTERM_SHELL= HOME=/home/root \
        "$HERE/magicpaper" --legacy-takeover
    status=$?

    if [ "$status" -eq 42 ]; then
        if [ ! -s "$READER_REQUEST" ]; then
            reader_error "沒有收到有效的 KOReader 開啟請求。"
            continue
        fi
        IFS= read -r target < "$READER_REQUEST"
        rm -f "$READER_REQUEST"
        launch_koreader "$target"
        continue
    fi

    echo "magicpaper-takeover: diary closed ($status), restoring xochitl"
    exit "$status"
done
