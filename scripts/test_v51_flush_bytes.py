"""Integration test for ADR 0006 Amendment 1 (v5.1.0) — byte-threshold debouncer flush.

Spawns the production HTTP binary with a tiny `SSH_NOTIFY_FLUSH_BYTES=4096`
threshold and a deliberately wide debounce window so the time-based path
cannot mask the byte-threshold path. Then drives `ssh_execute` for a
command that emits ~16 KiB of output in a single chunk and confirms the
HTTP `ssh_subscribe` HINT line surfaces the new knob to LLM hosts.

The full byte-triggered-flush observability is exposed via the
process-wide counter on `MemoryRegistry::byte_triggered_flushes_total`
(reachable from `ssh_daemon_stats`); a smaller-blast-radius assertion
on the wire HINT keeps this test self-contained without requiring a
real `sshd` target. Workflows touching a real remote live in
`scripts/test_resources.py` and `scripts/chaos_v5_subscribe.py`.
"""

from __future__ import annotations

import os
import subprocess

import pytest

from helpers.fixtures import HTTP_BIN, find_free_port, wait_for_port
from helpers.mcp_client import HttpTransport, McpClient, call_tool_text


@pytest.fixture
def flush_bytes_http_server():
    """Spawn `ssh-mcp` with `SSH_NOTIFY_FLUSH_BYTES=4096` (4 KiB)."""
    if not HTTP_BIN.exists():
        pytest.skip(f"http binary not built: {HTTP_BIN}")
    port = find_free_port()
    env = {
        **os.environ,
        "MCP_PORT": str(port),
        "MCP_HOST": "127.0.0.1",
        "RUST_LOG": os.environ.get("RUST_LOG", "warn"),
        # 4 KiB byte threshold + wide debounce window so the byte path
        # is the only realistic way push notifications can fire on a
        # short test budget.
        "SSH_NOTIFY_FLUSH_BYTES": "4k",
        "SSH_NOTIFY_DEBOUNCE_MS": "5000",
    }
    proc = subprocess.Popen(
        [str(HTTP_BIN)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        if not wait_for_port("127.0.0.1", port, timeout=10.0):
            proc.terminate()
            pytest.fail(f"ssh-mcp HTTP did not bind to port {port} within 10s")
        yield proc, port
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()


def test_subscribe_hint_mentions_flush_bytes_knob(flush_bytes_http_server):
    """A successful `ssh_subscribe` response must surface both the
    debounce and byte-threshold knobs in its `HINT: RECOMMENDED` line so
    LLM hosts know which budget governs their first push."""
    _proc, port = flush_bytes_http_server
    transport = HttpTransport(host="127.0.0.1", port=port)
    with McpClient(transport) as client:
        # Subscribe to a synthetic resource. The server happily mints a
        # SubId for any URI; the wire shape (HINT line) is what we
        # assert here, not the resource liveness.
        body = call_tool_text(
            client,
            "ssh_subscribe",
            {"uri": "command://placeholder/output"},
        )
        assert "SSH_SUBSCRIBE" in body, body
        # New v5.1 HINT line: must mention both knobs explicitly.
        assert "SSH_NOTIFY_FLUSH_BYTES" in body, body
        assert "SSH_NOTIFY_DEBOUNCE_MS" in body, body
        # Local-sleep guidance: must steer the LLM away from MCP-tool
        # sleep loops.
        assert "Do NOT use any MCP tool as a sleep" in body, body
        # Specific sleep examples for both shells.
        assert "Start-Sleep -Milliseconds" in body, body
        assert "sleep 0.05" in body, body
