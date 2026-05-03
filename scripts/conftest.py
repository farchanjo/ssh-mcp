"""pytest configuration shared by all ssh-mcp v3 integration suites."""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

# Re-export shared fixtures (pytest discovers them via conftest).
from helpers.fixtures import (  # noqa: E402,F401
    http_client,
    http_server,
    stdio_client,
    ssh_target,
    local_sshd,
)


def pytest_configure(config) -> None:
    config.addinivalue_line(
        "markers",
        "requires_sshd: test needs a real SSH server (set SSH_MCP_TEST_TARGET to enable)",
    )
