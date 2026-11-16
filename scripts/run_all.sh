#!/usr/bin/env bash
# Smoke + stress + chaos runner for ssh-mcp v4.3 Python integration tests.
#
# Workflow:
# 1. Build the dev binaries (default features + no-default-features).
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

echo "=== build dev binaries ==="
cargo build --bins
# The 38-tool surface gets its own target dir. Building it in the main tree
# overwrites target/<profile>/ssh-mcp with the reduced catalogue, and every
# fixture that resolves a binary by profile then drives the wrong server
# without saying so — which is what the old release-last ordering did.
cargo build --bins --no-default-features --target-dir target/no-default-features

# Dev profile is the default for the whole Python suite; the fixtures resolve
# target/debug unless SSH_MCP_BUILD_PROFILE says otherwise. A caller-supplied
# per-binary override wins over the profile directory.
BIN_DIR="$REPO_ROOT/target/${SSH_MCP_BUILD_PROFILE:-debug}"
EXT=""
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;;
esac
export PATH="$BIN_DIR:$PATH"
export SSH_MCP_BIN="${SSH_MCP_BIN:-$BIN_DIR/ssh-mcp$EXT}"
export SSH_MCP_STDIO_BIN="${SSH_MCP_STDIO_BIN:-$BIN_DIR/ssh-mcp-stdio$EXT}"
export SSH_MCP_TAIL_BIN="${SSH_MCP_TAIL_BIN:-$BIN_DIR/ssh-mcp-tail$EXT}"

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
