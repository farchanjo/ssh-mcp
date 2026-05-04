#!/usr/bin/env bash
# v5 Phase 5 — chaos test orchestration.
#
# Wraps `cargo test --features test-fixtures --test chaos` with a
# summary report. Optional `--release` flag runs the suite under
# release optimisations.
#
# Usage:
#   scripts/chaos_runner.sh
#   scripts/chaos_runner.sh --release
#   CHAOS_FILTER=chaos05 scripts/chaos_runner.sh

set -euo pipefail

CRATE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$CRATE_ROOT"

PROFILE_FLAG=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE_FLAG="--release"
fi

FILTER="${CHAOS_FILTER:-}"

LOG_FILE="$(mktemp)"
cleanup() {
    rm -f "$LOG_FILE"
}
trap cleanup EXIT

echo "chaos_runner: profile=${PROFILE_FLAG:-debug} filter='${FILTER}'" >&2

# Shellcheck appeases on the array vs string `$FILTER` expansion.
if [[ -n "$FILTER" ]]; then
    cargo test ${PROFILE_FLAG} \
        --features test-fixtures \
        --test chaos \
        -- "$FILTER" --nocapture \
        2>&1 | tee "$LOG_FILE"
else
    cargo test ${PROFILE_FLAG} \
        --features test-fixtures \
        --test chaos \
        -- --nocapture \
        2>&1 | tee "$LOG_FILE"
fi

PASS=$(grep -c "^test .* \.\.\. ok$" "$LOG_FILE" || true)
FAIL=$(grep -c "^test .* \.\.\. FAILED$" "$LOG_FILE" || true)
SKIP=$(grep -c "^test .* \.\.\. ignored$" "$LOG_FILE" || true)

echo "" >&2
echo "chaos_runner summary:" >&2
echo "  passed:  ${PASS}" >&2
echo "  failed:  ${FAIL}" >&2
echo "  skipped: ${SKIP}" >&2

if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
exit 0
