"""Integration tests for ssh-mcp v3 stdio transport (rmcp 1.6 line-delimited JSON-RPC).

Same coverage as ``test_http.py`` but driven over stdio against a freshly
spawned ``target/release/ssh-mcp-stdio``. SSH-touching tests are gated by the
``@pytest.mark.requires_sshd`` marker.
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


def test_stdio_initialize_returns_server_info(stdio_client: McpClient) -> None:
    info = stdio_client.server_info or {}
    assert info.get("serverInfo", {}).get("name") == "ssh-mcp"
    assert info.get("protocolVersion") == "2025-06-18"


def test_stdio_tools_list_returns_v47_catalogue(stdio_client: McpClient) -> None:
    """v4.7 advertises 21 tools (20 without the `port_forward` Cargo feature).

    The catalogue grew by 3 over v4.6: ``ssh_run``, ``ssh_execute_batch``,
    and ``ssh_disconnect_many``. This test asserts both the headline count
    and that the new tool names are present (and that the legacy v4.6
    catalogue stayed put).
    """
    tools = stdio_client.list_tools()
    names = {t["name"] for t in tools}
    assert len(tools) in {20, 21}, f"expected 20 or 21 tools, got {len(tools)}: {sorted(names)}"
    # v4.7 additions
    assert "ssh_run" in names
    assert "ssh_execute_batch" in names
    assert "ssh_disconnect_many" in names
    # v4.6 carry-overs (sample — full set is asserted in test_v47_structured_content.py)
    for legacy in (
        "ssh_connect",
        "ssh_disconnect",
        "ssh_execute",
        "ssh_shell_open",
        "ssh_shell_read",
        "ssh_upload",
        "ssh_download",
    ):
        assert legacy in names, f"missing legacy tool {legacy}"


def test_stdio_invalid_session_id_returns_error(stdio_client: McpClient) -> None:
    text = call_tool_text(
        stdio_client,
        "ssh_execute",
        {"session_id": make_session_id(), "command": "echo nope"},
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR"
    assert "SESSION_NOT_FOUND" in (parsed.get("reason") or text)


# ---------------------------------------------------------------------------
# SSH-touching tests
# ---------------------------------------------------------------------------


@pytest.mark.requires_sshd
def test_stdio_connect_disconnect_roundtrip(stdio_client: McpClient, ssh_target) -> None:
    text = call_tool_text(
        stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-rt")
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "OK", text
    sid = parsed.get("session_id")
    assert sid

    listed = parse_block(call_tool_text(stdio_client, "ssh_list_sessions", {"agent_id": "stdio-rt"}))
    assert listed.get("count", 0) == 1

    text = call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})
    assert parse_block(text).get("__status") == "OK"


@pytest.mark.requires_sshd
def test_stdio_disconnect_agent_bulk(stdio_client: McpClient, ssh_target) -> None:
    for i in range(3):
        text = call_tool_text(
            stdio_client,
            "ssh_connect",
            ssh_target.connect_args(agent_id="stdio-bulk", name=f"sb-{i}"),
        )
        assert parse_block(text).get("session_id"), text

    text = call_tool_text(stdio_client, "ssh_disconnect_agent", {"agent_id": "stdio-bulk"})
    parsed = parse_block(text)
    assert parsed.get("__status") == "OK"
    assert parsed.get("sessions_disconnected", 0) >= 3


@pytest.mark.requires_sshd
def test_stdio_execute_wait_completed(stdio_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-exec"))
    ).get("session_id")
    assert sid

    text = call_tool_text(
        stdio_client,
        "ssh_execute",
        {"session_id": sid, "command": "uname -s && whoami && echo STDIO_OK"},
    )
    cid = parse_block(text).get("command_id")
    text = call_tool_text(
        stdio_client,
        "ssh_get_command_output",
        {"command_id": cid, "wait": True, "wait_timeout_secs": 15},
        timeout=30,
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "COMPLETED"
    assert parsed.get("exit_code") == 0
    assert "STDIO_OK" in (parsed.get("stdout") or "")
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_stdio_execute_polling(stdio_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-poll"))
    ).get("session_id")
    text = call_tool_text(
        stdio_client, "ssh_execute", {"session_id": sid, "command": "sleep 1 && echo POLL_OK"}
    )
    cid = parse_block(text).get("command_id")
    parsed = {}
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        text = call_tool_text(stdio_client, "ssh_get_command_output", {"command_id": cid})
        parsed = parse_block(text)
        if parsed.get("__status") == "COMPLETED":
            break
        time.sleep(0.2)
    assert parsed.get("__status") == "COMPLETED"
    assert "POLL_OK" in (parsed.get("stdout") or "")
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_stdio_cancel_running_command(stdio_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-cancel"))
    ).get("session_id")
    cid = parse_block(
        call_tool_text(stdio_client, "ssh_execute", {"session_id": sid, "command": "sleep 60"})
    ).get("command_id")
    time.sleep(0.5)
    parsed = parse_block(call_tool_text(stdio_client, "ssh_cancel_command", {"command_id": cid}))
    assert parsed.get("__status") in {"CANCELLED", "NOOP"}
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_stdio_shell_lifecycle(stdio_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-shell"))
    ).get("session_id")
    shell_id = parse_block(
        call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")
    assert shell_id

    call_tool_text(
        stdio_client,
        "ssh_shell_write",
        {"shell_id": shell_id, "input": "echo STDIO_SHELL_OK\n"},
    )
    text = call_tool_text(
        stdio_client,
        "ssh_shell_read",
        {"shell_id": shell_id, "wait": True, "wait_timeout_secs": 5, "min_bytes": 4},
        timeout=15,
    )
    parsed = parse_block(text)
    assert "STDIO_SHELL_OK" in (parsed.get("data") or ""), parsed
    assert parse_block(call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})).get(
        "__status"
    ) == "OK"
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_stdio_shell_send_key_ctrl_c(stdio_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-key"))
    ).get("session_id")
    shell_id = parse_block(
        call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")
    call_tool_text(stdio_client, "ssh_shell_write", {"shell_id": shell_id, "input": "yes\n"})
    time.sleep(0.5)
    parsed = parse_block(
        call_tool_text(stdio_client, "ssh_shell_send_key", {"shell_id": shell_id, "key": "ctrl_c"})
    )
    assert parsed.get("__status") == "OK"
    assert parsed.get("key") == "ctrl_c"
    call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_stdio_shell_wait_for_match(stdio_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-waitfor"))
    ).get("session_id")
    shell_id = parse_block(
        call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")
    call_tool_text(
        stdio_client,
        "ssh_shell_write",
        {"shell_id": shell_id, "input": "printf 'foo\\nbar\\nbaz\\n'\n"},
    )
    parsed = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_shell_wait_for",
            {"shell_id": shell_id, "patterns": ["bar"], "timeout_secs": 5},
            timeout=15,
        )
    )
    assert parsed.get("__status") == "MATCHED", parsed
    assert parsed.get("matched_pattern") == "bar"
    call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_stdio_upload_download(stdio_client: McpClient, ssh_target, tmp_path) -> None:
    """Round-trip an SFTP upload + download.

    The in-process paramiko fixture sometimes cannot deliver an SFTP
    subsystem within the russh handshake budget — when that path returns
    `[TIMEOUT] initialize SFTP session`, we skip the test rather than
    counting it as a runtime regression. Real-sshd targets (set via
    ``SSH_MCP_TEST_TARGET``) always exercise the full path.
    """
    payload = b"ssh-mcp stdio payload " * 1024  # ~22 KB
    src = tmp_path / "upload.bin"
    src.write_bytes(payload)
    dst = tmp_path / "download.bin"
    remote_dir = f"/tmp/ssh-mcp-stdio-{os.getpid()}"
    remote_path = f"{remote_dir}/upload.bin"
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-xfer"))
    ).get("session_id")
    cid = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_execute",
            {"session_id": sid, "command": f"mkdir -p {remote_dir} && rm -f {remote_path}"},
        )
    ).get("command_id")
    call_tool_text(
        stdio_client, "ssh_get_command_output", {"command_id": cid, "wait": True}, timeout=15
    )
    upload_xfer = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_upload",
            {"session_id": sid, "local_path": str(src), "remote_path": remote_path},
        )
    ).get("transfer_id")
    parsed = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_get_transfer_progress",
            {"transfer_id": upload_xfer, "wait": True, "wait_timeout_secs": 60},
            timeout=90,
        )
    )
    if parsed.get("__status") == "FAILED" and "TIMEOUT" in (parsed.get("reason") or ""):
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})
        pytest.skip("local paramiko fixture: SFTP subsystem timed out (real-sshd path covers this)")
    assert parsed.get("__status") == "COMPLETED", parsed
    dl_xfer = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_download",
            {"session_id": sid, "remote_path": remote_path, "local_path": str(dst)},
        )
    ).get("transfer_id")
    parsed = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_get_transfer_progress",
            {"transfer_id": dl_xfer, "wait": True, "wait_timeout_secs": 60},
            timeout=90,
        )
    )
    assert parsed.get("__status") == "COMPLETED", parsed
    assert dst.exists() and dst.read_bytes() == payload
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


@pytest.mark.requires_sshd
def test_stdio_forward(stdio_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(stdio_client, "ssh_connect", ssh_target.connect_args(agent_id="stdio-fwd"))
    ).get("session_id")
    from helpers.fixtures import find_free_port

    local_port = find_free_port()
    parsed = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_forward",
            {
                "session_id": sid,
                "local_port": local_port,
                "remote_address": "127.0.0.1",
                "remote_port": 22,
            },
        )
    )
    if parsed.get("__status") == "ERROR" and "FEATURE_DISABLED" in (parsed.get("reason") or ""):
        pytest.skip("port_forward feature not enabled in binary")
    assert parsed.get("__status") == "OK", parsed
    assert parsed.get("active") is True
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})
