"""pytest configuration shared by all ssh-mcp v3 integration suites."""

from __future__ import annotations

import os
import sys
import tempfile
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
    # Every ssh-mcp server spawned by the suites inherits this process env.
    #
    # SSH_MCP_KNOWN_HOSTS — the TOFU learn path appends to the resolved
    # known_hosts file (default ~/.ssh/known_hosts). Parallel fixtures
    # appending concurrently interleave bytes into half-written lines, and
    # since the in-process paramiko sshd listens on a fresh ephemeral port
    # per test while ports get recycled, a later connect can match a
    # corrupted line and reject with `Unknown server key`. A per-session
    # scratch file makes host verification deterministic AND keeps the
    # operator's real known_hosts clean.
    #
    # SSH_NOTIFY_{DEBOUNCE,FORCE_FLUSH}_MS — suites assert notification
    # arrival inside 1 s deadlines; production defaults (1000/5000 ms)
    # race those assertions on loaded runners. Fast cadence here; an
    # explicit operator/test env value still wins (tests that study
    # debounce behaviour set it themselves).
    scratch = Path(tempfile.gettempdir()) / f"ssh-mcp-pytest-{os.getpid()}.known_hosts"
    scratch.touch(exist_ok=True)
    os.environ.setdefault("SSH_MCP_KNOWN_HOSTS", str(scratch))
    os.environ.setdefault("SSH_NOTIFY_DEBOUNCE_MS", "50")
    os.environ.setdefault("SSH_NOTIFY_FORCE_FLUSH_MS", "250")

    config.addinivalue_line(
        "markers",
        "requires_sshd: test needs a real SSH server (set SSH_MCP_TEST_TARGET to enable)",
    )
    config.addinivalue_line(
        "markers",
        "requires_vm: test needs a live VM with rsync 3.2.x (set SSH_MCP_E2E_HOST or skip)",
    )
