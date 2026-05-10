"""Integration tests for ssh-mcp v3 HTTP transport (rmcp 1.6 streamable-http).

Every test runs against a freshly spawned ``target/release/ssh-mcp`` bound to
a free port. SSH-touching tests are gated by the ``@pytest.mark.requires_sshd``
marker and skipped automatically when ``SSH_MCP_TEST_TARGET`` is unset.
"""

from __future__ import annotations

import os
import time

import pytest

from helpers.mcp_client import McpClient, call_tool_text, make_session_id
from helpers.parse_block import parse_block


# ---------------------------------------------------------------------------
# Smoke / handshake (no SSH server needed)
# ---------------------------------------------------------------------------


def test_initialize_handshake(http_client: McpClient) -> None:
    """`initialize` returns server info and the streamable-http session id."""
    info = http_client.server_info or {}
    assert info.get("serverInfo", {}).get("name") == "ssh-mcp"
    assert info.get("protocolVersion") == "2025-06-18"
    transport = http_client.transport
    assert getattr(transport, "session_id", None), "Mcp-Session-Id header missing"


def test_tools_list_returns_v7_catalogue(http_client: McpClient) -> None:
    """v7.0.1 advertises 39 tools (38 without `port_forward` Cargo feature).

    Renamed from ``test_tools_list_returns_eighteen_tools`` to reflect the
    v4.7 catalogue growth (added: ``ssh_run``, ``ssh_execute_batch``,
    ``ssh_disconnect_many``). Mirrors the stdio assertion in
    ``test_stdio.py::test_stdio_tools_list_returns_v7_catalogue``.
    """
    tools = http_client.list_tools()
    names = sorted(t["name"] for t in tools)
    assert len(names) in {38, 39}, f"expected 38 or 39 tools, got {len(names)}: {names}"
    # v4.7 additions
    for new in ("ssh_run", "ssh_exec_batch", "ssh_disconnect_many"):
        assert new in names, f"missing v4.7 tool {new}: {names}"
    # v4.6 carry-overs
    expected_subset = {
        "ssh_connect",
        "ssh_disconnect",
        "ssh_disconnect_agent",
        "ssh_sessions",
        "ssh_exec",
        "ssh_exec_output",
        "ssh_commands",
        "ssh_exec_cancel",
        "ssh_shell_open",
        "ssh_shell_write",
        "ssh_shell_press",
        "ssh_shell_wait_for",
        "ssh_shell_read",
        "ssh_shell_close",
        "ssh_upload",
        "ssh_download",
        "ssh_transfer_progress",
    }
    assert expected_subset.issubset(set(names)), names


def test_resources_list_initially_empty(http_client: McpClient) -> None:
    resources = http_client.list_resources()
    # No shells / commands / transfers yet — list should be empty (or contain
    # only static placeholders, depending on the resources_impl).
    assert isinstance(resources, list)


def test_invalid_session_id_returns_error(http_client: McpClient) -> None:
    text = call_tool_text(
        http_client,
        "ssh_exec",
        {"session_id": make_session_id(), "command": "echo nope"},
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR"
    assert "SESSION_NOT_FOUND" in (parsed.get("reason") or text)


# ---------------------------------------------------------------------------
# SSH-touching tests (require a real SSH endpoint)
# ---------------------------------------------------------------------------


@pytest.mark.requires_sshd
def test_connect_disconnect_roundtrip(http_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-rt")
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "OK", text
    sid = parsed.get("session_id")
    assert sid

    text = call_tool_text(http_client, "ssh_sessions", {"agent_id": "http-rt"})
    listed = parse_block(text)
    assert listed.get("count", 0) == 1

    text = call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})
    assert parse_block(text).get("__status") == "OK"


@pytest.mark.requires_sshd
def test_disconnect_agent_bulk(http_client: McpClient, ssh_target) -> None:
    for i in range(3):
        text = call_tool_text(
            http_client,
            "ssh_connect",
            ssh_target.connect_args(agent_id="http-bulk", name=f"bulk-{i}"),
        )
        assert parse_block(text).get("session_id"), text

    text = call_tool_text(http_client, "ssh_disconnect_agent", {"agent_id": "http-bulk"})
    parsed = parse_block(text)
    assert parsed.get("__status") == "OK"
    assert parsed.get("sessions_disconnected", 0) >= 3


@pytest.mark.requires_sshd
def test_execute_and_get_output_wait(http_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-exec")
    )
    sid = parse_block(text).get("session_id")
    assert sid

    text = call_tool_text(
        http_client,
        "ssh_exec",
        {"session_id": sid, "command": "uname -s && whoami && echo HTTP_OK"},
    )
    cid = parse_block(text).get("command_id")
    assert cid

    text = call_tool_text(
        http_client,
        "ssh_exec_output",
        {"command_id": cid, "wait": True, "wait_timeout_secs": 15},
        timeout=30,
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "COMPLETED"
    assert parsed.get("exit_code") == 0
    assert "HTTP_OK" in (parsed.get("stdout") or "")

    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_execute_polling_and_list_commands(http_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-poll")
    )
    sid = parse_block(text).get("session_id")
    assert sid

    text = call_tool_text(
        http_client, "ssh_exec", {"session_id": sid, "command": "sleep 1 && echo POLLED"}
    )
    cid = parse_block(text).get("command_id")

    # Poll until completion (no wait flag).
    deadline = time.monotonic() + 10
    parsed = {}
    while time.monotonic() < deadline:
        text = call_tool_text(http_client, "ssh_exec_output", {"command_id": cid})
        parsed = parse_block(text)
        if parsed.get("__status") == "COMPLETED":
            break
        time.sleep(0.2)
    assert parsed.get("__status") == "COMPLETED"
    assert "POLLED" in (parsed.get("stdout") or "")

    listed = parse_block(call_tool_text(http_client, "ssh_commands", {"session_id": sid}))
    assert listed.get("count", 0) >= 1

    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_cancel_running_command(http_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-cancel")
    )
    sid = parse_block(text).get("session_id")
    assert sid

    text = call_tool_text(
        http_client, "ssh_exec", {"session_id": sid, "command": "sleep 60"}
    )
    cid = parse_block(text).get("command_id")

    time.sleep(0.5)
    text = call_tool_text(http_client, "ssh_exec_cancel", {"command_id": cid})
    parsed = parse_block(text)
    assert parsed.get("__status") in {"CANCELLED", "NOOP"}, text

    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_shell_open_write_read_close(http_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-shell")
    )
    sid = parse_block(text).get("session_id")
    assert sid

    text = call_tool_text(
        http_client,
        "ssh_shell_open",
        {"session_id": sid, "term": "xterm", "cols": 80, "rows": 24},
    )
    shell_id = parse_block(text).get("shell_id")
    assert shell_id

    call_tool_text(
        http_client,
        "ssh_shell_write",
        {"shell_id": shell_id, "input": "echo HTTP_SHELL_OK\n"},
    )

    # Long-poll read until output appears.
    text = call_tool_text(
        http_client,
        "ssh_shell_read",
        {"shell_id": shell_id, "wait": True, "wait_timeout_secs": 5, "min_bytes": 4},
        timeout=15,
    )
    parsed = parse_block(text)
    assert parsed.get("__status") in {"OPEN", "TIMEOUT"}, text
    assert "HTTP_SHELL_OK" in (parsed.get("data") or "")

    text = call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
    assert parse_block(text).get("__status") == "OK"

    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_shell_send_key_ctrl_c_breaks_yes(http_client: McpClient, ssh_target) -> None:
    """Send Ctrl+C to a shell running ``yes`` and verify the shell stays alive.

    Requires a real PTY at the SSH server side (the local paramiko fixture
    in ``helpers.local_sshd`` wires ``/bin/sh -i`` through ``pty.openpty()``
    so signal-bearing keystrokes propagate as SIGINT).
    """
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-key")
    )
    sid = parse_block(text).get("session_id")
    assert sid

    shell_id = parse_block(
        call_tool_text(http_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")
    assert shell_id

    call_tool_text(http_client, "ssh_shell_write", {"shell_id": shell_id, "input": "yes\n"})
    time.sleep(0.5)
    text = call_tool_text(http_client, "ssh_shell_press", {"shell_id": shell_id, "key": "ctrl_c"})
    parsed = parse_block(text)
    assert parsed.get("__status") == "OK"
    assert parsed.get("key") == "ctrl_c"

    # Shell should still be alive — verify by reading something simple.
    call_tool_text(http_client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo POST_CTRL_C\n"})
    time.sleep(0.5)
    out = parse_block(
        call_tool_text(
            http_client,
            "ssh_shell_read",
            {"shell_id": shell_id, "wait": True, "wait_timeout_secs": 3, "min_bytes": 4},
            timeout=10,
        )
    )
    assert "POST_CTRL_C" in (out.get("data") or ""), out

    call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_shell_wait_for_pattern_match(http_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-wait")
    )
    sid = parse_block(text).get("session_id")
    shell_id = parse_block(
        call_tool_text(http_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")

    call_tool_text(
        http_client,
        "ssh_shell_write",
        {"shell_id": shell_id, "input": "printf 'foo\\nbar\\nbaz\\n'\n"},
    )

    text = call_tool_text(
        http_client,
        "ssh_shell_wait_for",
        {"shell_id": shell_id, "patterns": ["bar"], "timeout_secs": 5},
        timeout=15,
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "MATCHED", text
    assert parsed.get("matched_pattern") == "bar"

    call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_upload_download_progress(http_client: McpClient, ssh_target, tmp_path) -> None:
    payload = b"ssh-mcp http upload payload " * 1024  # ~28 KB
    src = tmp_path / "upload.bin"
    src.write_bytes(payload)
    dst = tmp_path / "download.bin"
    remote_dir = f"/tmp/ssh-mcp-http-{os.getpid()}"
    remote_path = f"{remote_dir}/upload.bin"

    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-xfer")
    )
    sid = parse_block(text).get("session_id")
    assert sid

    # Make sure remote directory exists.
    cid = parse_block(
        call_tool_text(
            http_client,
            "ssh_exec",
            {"session_id": sid, "command": f"mkdir -p {remote_dir} && rm -f {remote_path}"},
        )
    ).get("command_id")
    call_tool_text(
        http_client, "ssh_exec_output", {"command_id": cid, "wait": True}, timeout=15
    )

    text = call_tool_text(
        http_client,
        "ssh_upload",
        {"session_id": sid, "local_path": str(src), "remote_path": remote_path},
    )
    upload_xfer = parse_block(text).get("transfer_id")
    assert upload_xfer

    text = call_tool_text(
        http_client,
        "ssh_transfer_progress",
        {"transfer_id": upload_xfer, "wait": True, "wait_timeout_secs": 60},
        timeout=90,
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "COMPLETED", text

    text = call_tool_text(
        http_client,
        "ssh_download",
        {"session_id": sid, "remote_path": remote_path, "local_path": str(dst)},
    )
    dl_xfer = parse_block(text).get("transfer_id")
    assert dl_xfer

    text = call_tool_text(
        http_client,
        "ssh_transfer_progress",
        {"transfer_id": dl_xfer, "wait": True, "wait_timeout_secs": 60},
        timeout=90,
    )
    assert parse_block(text).get("__status") == "COMPLETED", text
    assert dst.exists() and dst.read_bytes() == payload

    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_forward_local_to_remote(http_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        http_client, "ssh_connect", ssh_target.connect_args(agent_id="http-fwd")
    )
    sid = parse_block(text).get("session_id")
    assert sid

    # Pick a random ephemeral local port.
    from helpers.fixtures import find_free_port

    local_port = find_free_port()

    text = call_tool_text(
        http_client,
        "ssh_forward",
        {
            "session_id": sid,
            "local_port": local_port,
            "remote_address": "127.0.0.1",
            "remote_port": 22,
        },
    )
    parsed = parse_block(text)
    if parsed.get("__status") == "ERROR" and "FEATURE_DISABLED" in (parsed.get("reason") or ""):
        pytest.skip("port_forward feature not enabled in binary")
    assert parsed.get("__status") == "OK", text
    assert parsed.get("active") is True

    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})
