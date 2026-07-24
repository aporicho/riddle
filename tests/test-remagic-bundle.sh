#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/magicpaper-bundle-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
FIXTURE=$TMP/fixture
OUT=$TMP/out
mkdir -p "$FIXTURE"
printf '#!/bin/sh\nexit 0\n' > "$FIXTURE/magicpaper"
chmod 0755 "$FIXTURE/magicpaper"
printf 'fixture-font\n' > "$FIXTURE/font.ttf"

# Independently exercise the documented canonical hashes on a tiny fixture.
CANONICAL=$TMP/canonical
mkdir -p "$CANONICAL/payload"
printf 'manifest\n' > "$CANONICAL/manifest.toml"
printf 'payload\n' > "$CANONICAL/payload/app"
chmod 0644 "$CANONICAL/manifest.toml"
chmod 0755 "$CANONICAL/payload/app"
manifest_sha=$(sha256sum "$CANONICAL/manifest.toml" | awk '{print $1}')
payload_file_sha=$(sha256sum "$CANONICAL/payload/app" | awk '{print $1}')
manifest_size=$(stat -c %s "$CANONICAL/manifest.toml")
payload_size=$(stat -c %s "$CANONICAL/payload/app")
expected_payload_sha=$(
    printf '%s\0%s\0%s\0%s\n' \
        payload/app 755 "$payload_size" "$payload_file_sha" | sha256sum | awk '{print $1}'
)
expected_content_id=$(
    {
        printf 'remagic-bundle-content-v1\0'
        printf '%s\0%s\0%s\0' magicpaper magicpaper 0.8.1
        printf '%s\0%s\0%s\0%s\n' \
            manifest.toml 644 "$manifest_size" "$manifest_sha"
        printf '%s\0%s\0%s\0%s\n' \
            payload/app 755 "$payload_size" "$payload_file_sha"
    } | sha256sum | awk '{print $1}'
)
python3 "$ROOT/scripts/remagic-bundle.py" create "$CANONICAL" \
    --app-id magicpaper --package magicpaper --version 0.8.1
python3 - "$CANONICAL/bundle.json" "$expected_payload_sha" "$expected_content_id" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    bundle = json.load(stream)
assert bundle["payload_sha256"] == sys.argv[2]
assert bundle["content_id"] == sys.argv[3]
PY

build_bundle() {
    OUT_DIR=$OUT \
    MAGICPAPER_BIN=$FIXTURE/magicpaper \
    MAGICPAPER_UI_FONT=$FIXTURE/font.ttf \
    MAGICPAPER_851_FONT=$FIXTURE/font.ttf \
    MAGICPAPER_BUTTER_FONT=$FIXTURE/font.ttf \
    MAGICPAPER_COVERAGE_FONT=$FIXTURE/font.ttf \
    SOURCE_DATE_EPOCH=0 \
        "$ROOT/scripts/make-remagic-package.sh" >/dev/null
}

(umask 077; build_bundle)
ARCHIVE=$OUT/magicpaper-0.8.1-universal_aarch64.tar.gz
[ -s "$ARCHIVE" ]
first_sha=$(sha256sum "$ARCHIVE" | awk '{print $1}')
(umask 022; build_bundle)
[ "$first_sha" = "$(sha256sum "$ARCHIVE" | awk '{print $1}')" ]

mkdir -p "$TMP/extracted"
tar -xzf "$ARCHIVE" -C "$TMP/extracted"
python3 "$ROOT/scripts/remagic-bundle.py" verify "$TMP/extracted" \
    --app-id magicpaper --package magicpaper --version 0.8.1

python3 - "$TMP/extracted/bundle.json" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    bundle = json.load(stream)
assert bundle["schema"] == 1
assert bundle["app_id"] == bundle["package"] == "magicpaper"
assert bundle["manifest_path"] == "manifest.toml"
assert re.fullmatch(r"[0-9a-f]{64}", bundle["content_id"])
assert re.fullmatch(r"[0-9a-f]{64}", bundle["payload_sha256"])
paths = [entry["path"] for entry in bundle["files"]]
assert paths == sorted(paths, key=lambda value: value.encode("utf-8"))
assert "bundle.json" not in paths
assert "manifest.toml" in paths
assert all(re.fullmatch(r"0[67][0-7]{2}", entry["mode"]) for entry in bundle["files"])
PY

# Every application-owned absolute path in the installed manifest must resolve
# inside the Store package's `current/payload` tree. This catches a package
# that verifies cryptographically but cannot be launched after publication.
python3 - "$TMP/extracted" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest = tomllib.loads((root / "manifest.toml").read_text(encoding="utf-8"))
prefix = "/home/root/apps/magicpaper/current/"
assert manifest["runtime"]["fonts"]["directories"] == [
    prefix + "payload/share/fonts",
]

def packaged(path, kind="file"):
    assert path.startswith(prefix), path
    resolved = root / path.removeprefix(prefix)
    if kind == "dir":
        assert resolved.is_dir(), resolved
    else:
        assert resolved.is_file(), resolved

packaged(manifest["icon"])
packaged(manifest["exec"])
packaged(manifest["working_dir"], "dir")
packaged(manifest["background_service"]["exec"])
packaged(manifest["background_service"]["working_dir"], "dir")
packaged(manifest["data_schema"]["migrator"])
for directory in manifest["runtime"]["fonts"]["directories"]:
    if directory.startswith(prefix):
        packaged(directory, "dir")
packaged(manifest["environment"]["MAGICPAPER_FONT_DIR"], "dir")
PY

# Execute the packaged wrapper against the fixture payload. The fixture app is
# a no-op shell program, so success proves the wrapper can find its loader,
# working tree and executable after Store publication.
MAGICPAPER_APP_ROOT="$TMP/extracted/payload" \
MAGICPAPER_ENV_LOADER="$TMP/extracted/payload/libexec/magicpaper-env" \
MAGICPAPER_TEST_MODE=1 \
    "$TMP/extracted/payload/bin/magicpaper-launch"

# A changed payload invalidates both the file list and content-addressed ID.
printf 'tampered\n' >> "$TMP/extracted/payload/bin/magicpaper"
if python3 "$ROOT/scripts/remagic-bundle.py" verify "$TMP/extracted" \
    --app-id magicpaper --package magicpaper --version 0.8.1 >/dev/null 2>&1; then
    echo "bundle verifier accepted modified payload" >&2
    exit 1
fi

# Links and special files are never representable in a Store bundle.
cp "$ROOT/manifests/magicpaper.toml" "$TMP/extracted/manifest.toml"
ln -s magicpaper "$TMP/extracted/payload/bin/unsafe-link"
if python3 "$ROOT/scripts/remagic-bundle.py" create "$TMP/extracted" \
    --app-id magicpaper --package magicpaper --version 0.8.1 >/dev/null 2>&1; then
    echo "bundle generator accepted a symlink" >&2
    exit 1
fi

echo "MagicPaper ReMagic bundle tests passed"
