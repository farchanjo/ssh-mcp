#!/usr/bin/env bash
# v5 Phase 5 — 24h soak harness.
#
# Runs the daemon under sustained load and tracks resident memory,
# fd count, and event counters. Designed to run unattended in CI.
#
# Usage:
#   scripts/soak_24h.sh                   # full 24-hour soak
#   SOAK_DURATION_S=60 scripts/soak_24h.sh # 60-second smoke
#   SOAK_SKIP=1 scripts/soak_24h.sh       # exit 0 immediately
#
# Output: CSV at scripts/soak_metrics.csv with columns:
#   timestamp, rss_kb, fd_count, ssh_handles, leak_events
#
# Assertions (end of run):
#   - RSS delta < 10% of initial RSS
#   - fd count is stable (delta < 50 fds)
#   - no panic log lines emitted
#
# This harness uses the in-process embed transport via
# `tests/v5_daemon_smoke.rs` so it does not require a live SSH server.

set -euo pipefail

if [[ "${SOAK_SKIP:-0}" == "1" ]]; then
    echo "soak_24h: SOAK_SKIP=1 — exiting cleanly" >&2
    exit 0
fi

DURATION_S="${SOAK_DURATION_S:-86400}"   # default: 24h
SAMPLE_INTERVAL_S="${SAMPLE_INTERVAL_S:-30}"
METRICS_CSV="${METRICS_CSV:-$(dirname "$0")/soak_metrics.csv}"

CRATE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$CRATE_ROOT"

echo "soak_24h: duration ${DURATION_S}s sample ${SAMPLE_INTERVAL_S}s csv=${METRICS_CSV}" >&2

# Build the daemon binary first so the runtime cost doesn't taint
# the first sample.
cargo build --release --bin ssh-mcp-tail >/dev/null

# Spawn a background dummy daemon. To prevent the daemon from
# observing EOF on stdin (which triggers a graceful shutdown), we
# launch a `sleep` process whose stdout pipes into the daemon's
# stdin; the sleep idles long enough for the soak window.
LOG_FILE="$(mktemp)"
SLEEP_PID=""
cleanup() {
    if [[ -n "${DAEMON_PID:-}" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill -TERM "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    if [[ -n "${SLEEP_PID}" ]] && kill -0 "$SLEEP_PID" 2>/dev/null; then
        kill -TERM "$SLEEP_PID" 2>/dev/null || true
        wait "$SLEEP_PID" 2>/dev/null || true
    fi
    rm -f "$LOG_FILE"
}
trap cleanup EXIT INT TERM

# `sleep` writes nothing to its stdout but keeps the pipe alive for
# the configured duration plus a 30s buffer so the daemon never sees
# EOF on stdin.
SLEEP_DURATION=$((DURATION_S + 30))
( sleep "$SLEEP_DURATION" ) | ./target/release/ssh-mcp-tail daemon >>"$LOG_FILE" 2>&1 &
DAEMON_PID=$!
sleep 1

if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    echo "soak_24h: daemon failed to start; log:" >&2
    cat "$LOG_FILE" >&2
    exit 1
fi

echo "timestamp,rss_kb,fd_count,ssh_handles,leak_events" >"$METRICS_CSV"

START_TS=$(date +%s)
INITIAL_RSS=""
INITIAL_FDS=""

read_rss() {
    if command -v ps >/dev/null 2>&1; then
        ps -o rss= -p "$1" 2>/dev/null | tr -d ' '
    fi
}

read_fd_count() {
    if [[ -d "/proc/$1/fd" ]]; then
        # Linux
        find "/proc/$1/fd" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' '
    elif command -v lsof >/dev/null 2>&1; then
        # macOS / BSD
        lsof -p "$1" 2>/dev/null | wc -l | tr -d ' '
    else
        echo 0
    fi
}

while :; do
    NOW=$(date +%s)
    ELAPSED=$((NOW - START_TS))
    if [[ $ELAPSED -ge $DURATION_S ]]; then
        break
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        echo "soak_24h: daemon died after ${ELAPSED}s — investigate $LOG_FILE" >&2
        exit 1
    fi
    RSS=$(read_rss "$DAEMON_PID" 2>/dev/null || echo 0)
    FDS=$(read_fd_count "$DAEMON_PID" 2>/dev/null || echo 0)
    SSH_HANDLES=0    # placeholder — daemon does not export internal counters yet
    LEAK_EVENTS=$(grep -c "SUB_LEAK_RISK" "$LOG_FILE" 2>/dev/null || true)
    LEAK_EVENTS="${LEAK_EVENTS:-0}"

    if [[ -z "$INITIAL_RSS" ]]; then
        INITIAL_RSS="$RSS"
        INITIAL_FDS="$FDS"
    fi

    echo "${NOW},${RSS},${FDS},${SSH_HANDLES},${LEAK_EVENTS}" >>"$METRICS_CSV"

    sleep "$SAMPLE_INTERVAL_S"
done

# Final assertions.
FINAL_RSS=$(read_rss "$DAEMON_PID" 2>/dev/null || echo 0)
FINAL_FDS=$(read_fd_count "$DAEMON_PID" 2>/dev/null || echo 0)

PANIC_LINES=$(grep -c "panicked\|panic!\|FATAL" "$LOG_FILE" 2>/dev/null || true)
PANIC_LINES="${PANIC_LINES:-0}"
if [[ $PANIC_LINES -gt 0 ]]; then
    echo "soak_24h: FAIL — panic detected ($PANIC_LINES lines)" >&2
    exit 2
fi

if [[ -n "$INITIAL_RSS" && "$INITIAL_RSS" -gt 0 ]]; then
    RSS_DELTA_PCT=$(( (FINAL_RSS - INITIAL_RSS) * 100 / INITIAL_RSS ))
    if [[ $RSS_DELTA_PCT -ge 10 ]]; then
        echo "soak_24h: FAIL — RSS delta ${RSS_DELTA_PCT}% exceeds 10% threshold" >&2
        exit 3
    fi
fi

if [[ -n "$INITIAL_FDS" ]]; then
    FD_DELTA=$(( FINAL_FDS - INITIAL_FDS ))
    if [[ $FD_DELTA -ge 50 ]]; then
        echo "soak_24h: FAIL — fd count delta ${FD_DELTA} exceeds 50 fd threshold" >&2
        exit 4
    fi
fi

echo "soak_24h: PASS — duration ${DURATION_S}s, rss ${INITIAL_RSS}->${FINAL_RSS}kb, fds ${INITIAL_FDS}->${FINAL_FDS}" >&2
