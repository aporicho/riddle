#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/magicpaper-package-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

# The config reader accepts input-side settings but cannot import model keys or
# override ReMagic's display, lifecycle, identity or path ownership contract.
cat > "$TMP/oracle.env" <<'EOF'
MAGICPAPER_OPENAI_KEY=test-placeholder
MAGICPAPER_OPENAI_MODEL=test-model
REMAGIC_DEVICE_PROFILE=attacker-value
QTFB_KEY=1
PATH=/attacker
RIDDLE_OCR_TOKEN=legacy-placeholder
EOF
(
    unset MAGICPAPER_OPENAI_KEY MAGICPAPER_OPENAI_MODEL MAGICPAPER_OCR_TOKEN
    trusted_path=$PATH
    REMAGIC_DEVICE_PROFILE=trusted-profile
    QTFB_KEY=77
    . "$ROOT/scripts/magicpaper-env"
    load_magicpaper_env "$TMP/oracle.env"
    [ -z "${MAGICPAPER_OPENAI_KEY+x}" ]
    [ -z "${MAGICPAPER_OPENAI_MODEL+x}" ]
    [ "$MAGICPAPER_OCR_TOKEN" = legacy-placeholder ]
    [ "$REMAGIC_DEVICE_PROFILE" = trusted-profile ]
    [ "$QTFB_KEY" = 77 ]
    [ "$PATH" = "$trusted_path" ]
)

# Test mode clears credentials and never falls back to production files.
(
    MAGICPAPER_OPENAI_KEY=should-be-cleared
    MAGICPAPER_OCR_TOKEN=should-be-cleared
    . "$ROOT/scripts/magicpaper-env"
    load_magicpaper_config "$TMP/oracle.env" /missing /missing 1
    [ -z "${MAGICPAPER_OPENAI_KEY+x}" ]
    [ -z "${MAGICPAPER_OCR_TOKEN+x}" ]
)

run_migrator() {
    MAGICPAPER_DATA_ROOT=$TMP/new/data \
    MAGICPAPER_CONFIG_ROOT=$TMP/new/config \
    MAGICPAPER_LEGACY_DATA_ROOTS=$TMP/legacy/data \
    MAGICPAPER_LEGACY_CONFIG_ROOTS=$TMP/legacy/config \
    MAGICPAPER_LEGACY_CONFIG_FILE=$TMP/legacy-app/oracle.env \
        sh "$ROOT/scripts/magicpaper-data-migrate"
}

mkdir -p "$TMP/legacy/data/tasks" "$TMP/legacy/config"
printf 'legacy task\n' > "$TMP/legacy/data/tasks/tasks.json"
printf 'MAGICPAPER_OPENAI_KEY=legacy-placeholder\n' > "$TMP/legacy/config/oracle.env"
run_migrator
grep -qx 'legacy task' "$TMP/new/data/tasks/tasks.json"
grep -qx 'MAGICPAPER_OPENAI_KEY=legacy-placeholder' "$TMP/new/config/oracle.env"
[ "$(stat -c %a "$TMP/new/data")" = 700 ]
[ "$(stat -c %a "$TMP/new/config")" = 700 ]

# Current data is authoritative and cannot be overwritten by another run.
printf 'current task\n' > "$TMP/new/data/tasks/tasks.json"
run_migrator
grep -qx 'current task' "$TMP/new/data/tasks/tasks.json"

# Symlinked targets and legacy special objects are refused.
rm -rf "$TMP/new"
mkdir -p "$TMP/elsewhere"
ln -s "$TMP/elsewhere" "$TMP/new"
if run_migrator >/dev/null 2>&1; then
    echo "MagicPaper migrator accepted a symlinked target" >&2
    exit 1
fi

rm -rf "$TMP/new" "$TMP/legacy/data"
mkdir -p "$TMP/legacy/data"
ln -s "$TMP/elsewhere" "$TMP/legacy/data/unsafe-link"
if run_migrator >/dev/null 2>&1; then
    echo "MagicPaper migrator accepted a symlinked legacy object" >&2
    exit 1
fi

echo "MagicPaper ReMagic package tests passed"
