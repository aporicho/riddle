#!/usr/bin/env bash
# Stage the AppLoad bundle into dist/riddle/, ready for `remagic publish`.
# Prereq: ./build-takeover.sh has produced the takeover binary.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=target/aarch64-unknown-linux-gnu/release/riddle-takeover
QUILL=${QUILL_DIR:-../quill-move}
[ -f "$BIN" ] || { echo "build first: ./build-takeover.sh" >&2; exit 1; }
[ -f "$QUILL/build/libquill.so" ] || { echo "missing $QUILL/build/libquill.so" >&2; exit 1; }

rm -rf dist/riddle
mkdir -p dist/riddle
install -m 755 "$BIN" dist/riddle/riddle
install -m 755 "$QUILL/build/libquill.so" dist/riddle/
install -m 755 scripts/appload-launch.sh scripts/riddle-launch.sh scripts/riddle-takeover.sh scripts/riddle-restore.sh dist/riddle/
install -m 644 systemd/riddle-takeover.service systemd/riddle-power-launcher.service dist/riddle/
install -m 644 external.manifest.json icon.png oracle.env.default oracle.env.example settings.schema.json dist/riddle/

# Optional local-only handwriting fonts. Their licenses are not part of this
# repository, so callers must provide extracted TTF paths explicitly. The app
# remains usable with its embedded OFL ChenYuluoyan font when either is absent.
mkdir -p dist/riddle/fonts
if [ -n "${MAGICPAPER_BUTTER_FONT:-}" ]; then
    [ -f "$MAGICPAPER_BUTTER_FONT" ] || { echo "missing $MAGICPAPER_BUTTER_FONT" >&2; exit 1; }
    install -m 644 "$MAGICPAPER_BUTTER_FONT" dist/riddle/fonts/ButterShiSan.ttf
else
    echo "note: MAGICPAPER_BUTTER_FONT not set; 黄油拾叁体 will be unavailable" >&2
fi
if [ -n "${MAGICPAPER_851_FONT:-}" ]; then
    [ -f "$MAGICPAPER_851_FONT" ] || { echo "missing $MAGICPAPER_851_FONT" >&2; exit 1; }
    install -m 644 "$MAGICPAPER_851_FONT" dist/riddle/fonts/851LakeusNightWriting.ttf
else
    echo "note: MAGICPAPER_851_FONT not set; 851 远星夜行 will be unavailable" >&2
fi

echo "staged: $(du -sh dist/riddle | cut -f1) in dist/riddle/"
echo "publish with: remagic publish dist/riddle -catalog-dir <remagic checkout>"
