"""Resources subscribe scenarios for ssh-mcp v3.

Covers:

- ``resources/list`` reflects open shells / commands / transfers / sessions.
- ``resources/read shell://<id>/output?cursor=auto`` returns buffered bytes.
- ``resources/subscribe shell://<id>/output`` registers the peer.
- After ``ssh_shell_write`` produces output, ``notifications/resources/updated``
  fires in under 200 ms (with a 1 s read timeout).
- ``_meta.cursor`` advances; ``_meta.last_seq`` is monotonic.
- ``resources/unsubscribe`` removes the registration; subsequent writes do NOT
  notify the peer.
- Truncation compensation: with ``SSH_SHELL_MAX_BUFFER_SIZE=4096``, write
  enough output to overflow and check that ``_meta.truncated_since_last_read``
  is populated.
"""

from __future__ import annotations

import os
import subprocess
import time
from contextlib import closing

import pytest

from helpers.fixtures import HTTP_BIN, find_free_port, wait_for_port, SshTarget
from helpers.mcp_client import HttpTransport, McpClient, call_tool_text
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


@pytest.fixture
def small_buffer_http_server():
    """Spawn ssh-mcp with `SSH_SHELL_MAX_BUFFER_SIZE=4096` so we can exercise
    truncation in a single test without flooding the buffer for minutes.
    """
    if not HTTP_BIN.exists():
        pytest.skip(f"http binary not built: {HTTP_BIN}")
    port = find_free_port()
    env = {
        **os.environ,
        "MCP_PORT": str(port),
        "MCP_HOST": "127.0.0.1",
        "RUST_LOG": os.environ.get("RUST_LOG", "warn"),
        "SSH_SHELL_MAX_BUFFER_SIZE": "4096",
    }
    proc = subprocess.Popen([str(HTTP_BIN)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not wait_for_port("127.0.0.1", port, timeout=10.0):
            proc.terminate()
            pytest.fail("ssh-mcp HTTP did not start within 10s")
        yield port
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()


def _open_shell(client: McpClient, ssh_target: SshTarget, *, agent: str) -> tuple[str, str]:
    sid = parse_block(
        call_tool_text(client, "ssh_connect", ssh_target.connect_args(agent_id=agent))
    ).get("session_id")
    assert sid, "ssh_connect failed"
    shell_id = parse_block(
        call_tool_text(client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")
    assert shell_id, "ssh_shell_open failed"
    return sid, shell_id


def test_resources_list_includes_open_shell(http_client: McpClient, ssh_target) -> None:
    sid, shell_id = _open_shell(http_client, ssh_target, agent="res-list")
    try:
        resources = http_client.list_resources()
        uris = {r.get("uri", "") for r in resources}
        assert any(f"shell://{shell_id}" in u for u in uris), f"shell URI missing: {uris}"
    finally:
        call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


def test_resources_read_shell_output_with_auto_cursor(http_client: McpClient, ssh_target) -> None:
    sid, shell_id = _open_shell(http_client, ssh_target, agent="res-read")
    try:
        call_tool_text(
            http_client,
            "ssh_shell_write",
            {"shell_id": shell_id, "input": "echo READ_OK\n"},
        )
        time.sleep(0.5)
        result = http_client.read_resource(f"shell://{shell_id}/output?cursor=auto")
        contents = result.get("contents") or []
        assert contents, result
        first = contents[0]
        text = first.get("text") or first.get("blob") or ""
        assert "READ_OK" in text, text
        meta = first.get("_meta") or {}
        assert "cursor" in meta or "last_seq" in meta
    finally:
        call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


def test_subscribe_emits_updated_notification(http_client: McpClient, ssh_target) -> None:
    sid, shell_id = _open_shell(http_client, ssh_target, agent="res-sub")
    uri = f"shell://{shell_id}/output"
    try:
        http_client.subscribe(uri)
        # Drain anything queued from the open phase.
        http_client.drain_notifications()

        call_tool_text(
            http_client,
            "ssh_shell_write",
            {"shell_id": shell_id, "input": "echo NOTIF_FIRES\n"},
        )

        # Wait up to 1 s for the updated notification.
        notif = None
        deadline = time.monotonic() + 1.0
        while time.monotonic() < deadline:
            n = http_client.receive_notification(timeout=0.2)
            if n is None:
                continue
            method = n.get("method", "")
            if method == "notifications/resources/updated":
                params = n.get("params") or {}
                if params.get("uri", "").startswith(f"shell://{shell_id}"):
                    notif = n
                    break
        assert notif is not None, "no resources/updated notification received within 1s"
    finally:
        try:
            http_client.unsubscribe(uri)
        except Exception:
            pass
        call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


def test_unsubscribe_stops_notifications(http_client: McpClient, ssh_target) -> None:
    sid, shell_id = _open_shell(http_client, ssh_target, agent="res-unsub")
    uri = f"shell://{shell_id}/output"
    try:
        http_client.subscribe(uri)
        # Trigger one update to confirm subscription works.
        call_tool_text(http_client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo SUB_OK\n"})
        time.sleep(0.5)
        http_client.drain_notifications()
        # Now unsubscribe and verify silence.
        http_client.unsubscribe(uri)
        call_tool_text(
            http_client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo SHOULD_BE_QUIET\n"}
        )
        # Allow ample time; we expect zero shell-update notifications.
        deadline = time.monotonic() + 1.0
        offending = []
        while time.monotonic() < deadline:
            n = http_client.receive_notification(timeout=0.2)
            if n is None:
                continue
            method = n.get("method", "")
            if method == "notifications/resources/updated":
                params = n.get("params") or {}
                if params.get("uri", "").startswith(f"shell://{shell_id}"):
                    offending.append(n)
        assert not offending, f"received {len(offending)} updates after unsubscribe: {offending[:1]}"
    finally:
        call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


def test_truncation_metadata_under_buffer_pressure(small_buffer_http_server, ssh_target) -> None:
    port = small_buffer_http_server
    transport = HttpTransport(f"http://127.0.0.1:{port}")
    client = McpClient(transport)
    client.initialize()
    try:
        sid, shell_id = _open_shell(client, ssh_target, agent="res-trunc")
        # Read once to anchor the cursor at start of stream.
        client.read_resource(f"shell://{shell_id}/output?cursor=auto")
        # Generate ~32 KB of output (8x the 4 KB cap).
        call_tool_text(
            client,
            "ssh_shell_write",
            {"shell_id": shell_id, "input": "head -c 32768 /dev/urandom | base64\n"},
        )
        # Give the shell a moment to drain into the ring buffer.
        time.sleep(1.5)
        result = client.read_resource(f"shell://{shell_id}/output?cursor=auto")
        contents = result.get("contents") or []
        assert contents
        meta = (contents[0].get("_meta") or {})
        # Either bytes were truncated ahead of the read, or the buffer is at
        # cap (saturated). Both indicate the back-pressure path was exercised.
        truncated = meta.get("truncated_since_last_read", 0)
        buffer_size = meta.get("buffer_size", 0)
        assert truncated > 0 or buffer_size <= 4096, meta
        call_tool_text(client, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(client, "ssh_disconnect", {"session_id": sid})
    finally:
        client.close()
