#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

scripts/check-architecture.sh
cargo fmt --all --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo check --release --all-features

test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT
offline_output=$(
    RIDDLE_TEST_MODE=1 \
    RIDDLE_DATA_DIR="$test_root/data" \
    RIDDLE_OPENAI_KEY=test-placeholder \
    RIDDLE_OPENAI_BASE=http://127.0.0.1:1 \
    RIDDLE_OCR_TOKEN=test-placeholder \
    cargo run --quiet -- --oracle-test /definitely/missing.png 2>&1
)
grep -q 'deterministic offline test backend' <<<"$offline_output"
grep -q '測試回覆' <<<"$offline_output"

set +e
ocr_output=$(
    RIDDLE_TEST_MODE=1 \
    RIDDLE_DATA_DIR="$test_root/data" \
    RIDDLE_OCR_TOKEN=test-placeholder \
    cargo run --quiet -- --ocr-test /definitely/missing.png 2>&1
)
ocr_status=$?
set -e
if (( ocr_status == 0 )) || ! grep -q 'disabled in RIDDLE_TEST_MODE' <<<"$ocr_output"; then
    echo 'deterministic test mode did not block PaddleOCR' >&2
    exit 1
fi

for script in scripts/*.sh; do
    bash -n "$script"
done

echo "MagicPaper checks passed"
