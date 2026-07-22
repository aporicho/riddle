#!/usr/bin/env bash
set -euo pipefail
umask 022

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TARGET=${TARGET:-aarch64-unknown-linux-gnu}
PACKAGE_TARGET=${PACKAGE_TARGET:-universal_aarch64}
BIN=${MAGICPAPER_BIN:-$ROOT/target/$TARGET/release/magicpaper}
OUT=${OUT_DIR:-$ROOT/dist/remagic-store}
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
BUNDLE=$OUT/magicpaper-$VERSION-$PACKAGE_TARGET
ARCHIVE=$BUNDLE.tar.gz

require_file() {
    local description=$1 path=$2
    if [[ ! -f "$path" ]]; then
        printf 'missing %s: %s\n' "$description" "$path" >&2
        exit 1
    fi
}

require_file 'release binary' "$BIN"
require_file 'bundle manifest generator' "$ROOT/scripts/remagic-bundle.py"
require_file 'UI font (MAGICPAPER_UI_FONT)' "${MAGICPAPER_UI_FONT:-}"
require_file '851 font (MAGICPAPER_851_FONT)' "${MAGICPAPER_851_FONT:-}"
require_file 'Butter font (MAGICPAPER_BUTTER_FONT)' "${MAGICPAPER_BUTTER_FONT:-}"
require_file 'coverage font (MAGICPAPER_COVERAGE_FONT)' "${MAGICPAPER_COVERAGE_FONT:-}"
grep -qx "version = \"$VERSION\"" "$ROOT/manifests/magicpaper.toml" || {
    echo "manifest version does not match Cargo.toml ($VERSION)" >&2
    exit 1
}

rm -rf "$BUNDLE" "$ARCHIVE"
mkdir -p "$BUNDLE/payload/bin" "$BUNDLE/payload/libexec" \
    "$BUNDLE/payload/share/fonts"

install -m 0644 "$ROOT/manifests/magicpaper.toml" "$BUNDLE/manifest.toml"
install -m 0755 "$BIN" "$BUNDLE/payload/bin/magicpaper"
install -m 0755 "$ROOT/scripts/remagic-launch.sh" \
    "$BUNDLE/payload/bin/magicpaper-launch"
install -m 0755 "$ROOT/scripts/magicpaper-env" \
    "$BUNDLE/payload/libexec/magicpaper-env"
install -m 0755 "$ROOT/scripts/magicpaper-data-migrate" \
    "$BUNDLE/payload/libexec/magicpaper-data-migrate"
install -m 0644 "$ROOT/icon.png" "$BUNDLE/payload/share/icon.png"
install -m 0644 "$ROOT/oracle.env.example" "$ROOT/settings.schema.json" \
    "$BUNDLE/payload/share/"
install -m 0644 "$MAGICPAPER_UI_FONT" \
    "$BUNDLE/payload/share/fonts/FZPingXianYaSong.ttf"
install -m 0644 "$MAGICPAPER_851_FONT" \
    "$BUNDLE/payload/share/fonts/851LakeusNightWriting.ttf"
install -m 0644 "$MAGICPAPER_BUTTER_FONT" \
    "$BUNDLE/payload/share/fonts/ButterShiSan.ttf"
install -m 0644 "$MAGICPAPER_COVERAGE_FONT" \
    "$BUNDLE/payload/share/fonts/CoverageFallback.ttf"

python3 "$ROOT/scripts/remagic-bundle.py" create "$BUNDLE" \
    --app-id magicpaper --package magicpaper --version "$VERSION"
python3 "$ROOT/scripts/remagic-bundle.py" verify "$BUNDLE" \
    --app-id magicpaper --package magicpaper --version "$VERSION"

tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 \
    --numeric-owner -C "$BUNDLE" -cf - bundle.json manifest.toml payload | gzip -n > "$ARCHIVE"
printf 'MagicPaper Store bundle: %s\n' "$ARCHIVE"
sha256sum "$ARCHIVE"
