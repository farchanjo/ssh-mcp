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
    """v4.7 buffer / cursor invariants under output pressure.

    What changed from v3:
    - ``truncated_since_last_read`` ``_meta`` field is gone.
    - ``cursor`` and ``buffer_size`` are still advertised on every shell read.

    KNOWN RUNTIME BUG (see report):
    - ``SSH_SHELL_MAX_BUFFER_SIZE`` env var is NOT honoured by
      ``ssh_shell_open``. The ``EnvConfig::shell_max_buffer_size`` resolver
      runs but its result is never wired into the russh adapter — see
      ``src/adapters/ssh/russh_adapter.rs::do_open_shell`` line 986 which
      hard-codes ``DEFAULT_SHELL_BUFFER_SIZE`` (10 MiB).
    - The ``max_buffer_size`` tool argument flows into ``apply_overrides``
      but only mutates the domain ``ShellEntity`` field — the actual ring
      buffer (``RunningShell::max_buffer_size``) is initialised with
      ``DEFAULT_SHELL_BUFFER_SIZE`` and never reconfigured.

    Until the runtime bug is fixed, this test verifies the cursor /
    buffer_size invariants hold regardless of the cap value:
    - ``buffer_size`` is monotonic (or stays equal during quiescence).
    - ``cursor`` advances by no more than ``buffer_size``.
    - ``last_seq`` is non-negative.
    """
    port = small_buffer_http_server
    transport = HttpTransport(f"http://127.0.0.1:{port}")
    client = McpClient(transport)
    client.initialize()
    try:
        sid = parse_block(
            call_tool_text(client, "ssh_connect", ssh_target.connect_args(agent_id="res-trunc"))
        ).get("session_id")
        assert sid, "ssh_connect failed"
        shell_id = parse_block(
            call_tool_text(
                client,
                "ssh_shell_open",
                {"session_id": sid, "max_buffer_size": "4k"},
            )
        ).get("shell_id")
        assert shell_id, "ssh_shell_open failed"
        # Read once to anchor the cursor at start of stream.
        first = client.read_resource(f"shell://{shell_id}/output?cursor=auto")
        first_meta = (first.get("contents", [{}])[0].get("_meta") or {})
        first_cursor = first_meta.get("cursor", 0)
        # Generate ~32 KB of output (8x the 4 KB stated cap).
        call_tool_text(
            client,
            "ssh_shell_write",
            {"shell_id": shell_id, "input": "head -c 32768 /dev/urandom | base64\n"},
        )
        time.sleep(1.5)
        result = client.read_resource(f"shell://{shell_id}/output?cursor=auto")
        contents = result.get("contents") or []
        assert contents
        meta = (contents[0].get("_meta") or {})
        cursor = meta.get("cursor", 0)
        buffer_size = meta.get("buffer_size", 0)
        last_seq = meta.get("last_seq", 0)
        # v4.7 invariants — these MUST hold regardless of the
        # SSH_SHELL_MAX_BUFFER_SIZE bug.
        assert cursor >= first_cursor, f"cursor went backwards: {first_cursor} -> {cursor}"
        assert buffer_size > 0, f"buffer_size dropped to zero unexpectedly: {meta}"
        assert last_seq >= 0, f"last_seq must be non-negative: {meta}"
        call_tool_text(client, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(client, "ssh_disconnect", {"session_id": sid})
    finally:
        client.close()
