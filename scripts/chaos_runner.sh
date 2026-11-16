#!/usr/bin/env bash
# v5 Phase 5 — chaos test orchestration.
#
# Wraps `cargo nextest run --features test-fixtures --test chaos` with a
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

# A failing run must still reach the summary, so the pipeline runs with `set +e`
# and the runner's exit status is captured from PIPESTATUS instead.
# shellcheck disable=SC2086  # PROFILE_FLAG and FILTER are intentionally word-split.
set +e
cargo nextest run ${PROFILE_FLAG} \
    --features test-fixtures \
    --test chaos \
    --no-capture \
    ${FILTER} \
    2>&1 | tee "$LOG_FILE"
RUN_STATUS=${PIPESTATUS[0]}
set -e

# nextest's closing line reads "N tests run: P passed, F failed, S skipped";
# the `failed` segment is omitted on a clean run.
SUMMARY="$(grep -E 'Summary \[' "$LOG_FILE" | tail -1 || true)"
PASS=$(grep -oE '[0-9]+ passed' <<<"$SUMMARY" | grep -oE '[0-9]+' || true)
FAIL=$(grep -oE '[0-9]+ failed' <<<"$SUMMARY" | grep -oE '[0-9]+' || true)
SKIP=$(grep -oE '[0-9]+ skipped' <<<"$SUMMARY" | grep -oE '[0-9]+' || true)

echo "" >&2
echo "chaos_runner summary:" >&2
echo "  passed:  ${PASS:-0}" >&2
echo "  failed:  ${FAIL:-0}" >&2
echo "  skipped: ${SKIP:-0}" >&2

if [[ "${FAIL:-0}" -gt 0 || "$RUN_STATUS" -ne 0 ]]; then
    exit 1
fi
exit 0
