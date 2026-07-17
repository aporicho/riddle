#!/bin/sh
# systemd ExecStopPost safety net for a takeover session.
rm -f /tmp/epframebuffer.lock
rmdir /run/riddle-takeover.lock 2>/dev/null || true
# xochitl allows only a few starts per ten minutes. Takeover apps legitimately
# stop/start it, so clear the counter before restoring the stock UI.
systemctl reset-failed xochitl.service paperweight.service 2>/dev/null || true
systemctl start xochitl
echo riddle-takeover > /sys/power/wake_unlock 2>/dev/null || true
