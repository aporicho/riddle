#!/bin/sh
# Stable entry point for Paperweight, SSH, or any future launcher integration.
if systemctl is-active --quiet magicpaper-takeover.service; then
    exit 0
fi
systemctl reset-failed magicpaper-takeover.service xochitl.service paperweight.service 2>/dev/null || true
exec systemctl start magicpaper-takeover.service
