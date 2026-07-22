#!/bin/sh
# AppLoad entry point for takeover mode. AppLoad runs this inside xochitl's
# world, which is about to be stopped — so detach the real launch into a
# transient systemd unit (PID-1-owned, survives xochitl) and exit immediately.
#
# Works wherever the bundle is installed: we resolve our own directory rather
# than hardcoding a path, so dropping this folder into AppLoad just works.
HERE=$(cd "$(dirname "$0")" && pwd)
systemctl is-active --quiet magicpaper-takeover && exit 0

# Prefer the persistent standalone service. It is independent of AppLoad/XOVI
# and points at /home/root/apps/magicpaper, so xochitl can be restored cleanly.
if systemctl cat magicpaper-takeover.service >/dev/null 2>&1; then
    systemctl start magicpaper-takeover.service
    exit $?
fi

# ExecStopPost is the safety net the in-script trap can't be: it runs even if
# magicpaper is SIGKILLed or OOM-killed, so the tablet never stays UI-less and the
# takeover wakelock is always released.
# (`systemctl start` on an already-running xochitl is a no-op; the leading
# "-" ignores failures.) Fall back to a plain launch if the property is
# rejected by an older systemd.
systemd-run --unit=magicpaper-takeover --collect \
    --property="ExecStopPost=-$HERE/magicpaper-restore.sh" \
    /bin/bash "$HERE/magicpaper-takeover.sh" \
  || systemd-run --unit=magicpaper-takeover --collect /bin/bash "$HERE/magicpaper-takeover.sh"
exit 0
