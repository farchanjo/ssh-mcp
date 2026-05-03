#!/usr/bin/env bash
# Smoke + stress runner for ssh-mcp v3 Python integration tests.
#
# Workflow:
# 1. Build the release binaries.
# 2. Run pytest across all `scripts/test_*.py` files.
# 3. Run the four stress scripts (each prints JSON to stdout for grep-ability).
#
# SSH-touching tests are gated by `SSH_MCP_TEST_TARGET`. When unset they are
# automatically skipped.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "=== build release binaries ==="
cargo build --release --bins

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
