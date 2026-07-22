#!/bin/sh
set -eu

# ReMagic supplies every platform-owned variable, including DeviceProfileV1,
# QTFB_KEY and the lifecycle channel. This wrapper only loads MagicPaper's
# allow-listed application configuration before starting the requested role.
APP_ROOT=${MAGICPAPER_APP_ROOT:-/home/root/apps/magicpaper/current/payload}
ENV_LOADER=${MAGICPAPER_ENV_LOADER:-$APP_ROOT/libexec/magicpaper-env}
CONFIG=${MAGICPAPER_CONFIG:-${XDG_CONFIG_HOME:-/home/root/.config/magicpaper}/oracle.env}
LEGACY_CONFIG=${MAGICPAPER_LEGACY_CONFIG:-/home/root/.config/riddle/oracle.env}
LEGACY_APP_CONFIG=${MAGICPAPER_LEGACY_CONFIG_FILE:-/home/root/apps/riddle/oracle.env}
umask 077

. "$ENV_LOADER"
load_magicpaper_config \
    "$CONFIG" "$LEGACY_CONFIG" "$LEGACY_APP_CONFIG" \
    "${MAGICPAPER_TEST_MODE:-0}"

export MAGICPAPER_FONT_DIR=${MAGICPAPER_FONT_DIR:-$APP_ROOT/share/fonts}
cd "$APP_ROOT"
exec "$APP_ROOT/bin/magicpaper" "$@"
