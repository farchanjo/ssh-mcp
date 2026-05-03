#!/usr/bin/env bash
# Smoke + stress + chaos runner for ssh-mcp v4.3 Python integration tests.
#
# Workflow:
# 1. Build the release binaries (default features + no-default-features).
# 2. Run pytest across all `scripts/test_*.py` files.
# 3. Run the four stress scripts (each prints JSON to stdout for grep-ability).
# 4. Run the four chaos scripts (per-event JSON lines + final summary).
# 5. Run the master `chaos_runner.py` which aggregates everything into a
#    single ASCII summary table.
#
# SSH-touching tests are gated by `SSH_MCP_TEST_TARGET`. When unset they are
# automatically skipped.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "=== build release binaries ==="
cargo build --release --bins
cargo build --release --bins --no-default-features

export PATH="$REPO_ROOT/target/release:$PATH"
export SSH_MCP_BIN="$REPO_ROOT/target/release/ssh-mcp"
export SSH_MCP_STDIO_BIN="$REPO_ROOT/target/release/ssh-mcp-stdio"

echo
echo "=== pytest suite ==="
python3 -m pytest scripts/test_*.py -v

echo
echo "=== stress: subscribe (60 s) ==="
python3 scripts/stress_subscribe.py

echo
echo "=== stress: locks (5 min) ==="
python3 scripts/stress_locks.py

echo
echo "=== stress: concurrent writes ==="
python3 scripts/stress_concurrent_writes.py

echo
echo "=== stress: lagged subscriber ==="
python3 scripts/stress_lagged_sub.py

echo
echo "=== chaos: errors ==="
python3 scripts/chaos_errors.py

echo
echo "=== chaos: locks ==="
python3 scripts/chaos_locks.py

echo
echo "=== chaos: recovery ==="
python3 scripts/chaos_recovery.py

echo
echo "=== chaos: exhaustion ==="
python3 scripts/chaos_exhaustion.py

echo
echo "=== aggregate (chaos_runner.py) ==="
python3 scripts/chaos_runner.py --quiet
