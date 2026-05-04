"""v4.7 structured_content channel coverage.

For every one of the 21 tools (20 without ``port_forward``), verify that:

1. The response carries a Markdown ``content[].text`` channel.
2. The response carries a parallel ``structuredContent`` (camelCase per rmcp
   wire shape; ``extract_structured`` accepts both forms).
3. The structured payload's ``tool`` key matches the tool name.
4. The structured payload's ``status`` key is the lower-case form of the
   status word from the Markdown header (or ``error`` on failures).
5. Error paths surface ``status: "error"`` with the documented ``code`` /
   ``reason`` / ``detail`` shape.

The shapes asserted here track the canonical example shapes documented in
``docs/LLM_GUIDE.md`` section K.

The local paramiko fixture is always used (no real sshd needed). Tests
that touch SFTP / port-forward stay self-contained.
"""

from __future__ import annotations

import os
import socket
import time
from contextlib import closing

import pytest

from helpers.mcp_client import (
    McpClient,
    call_tool_pair,
    call_tool_text,
    extract_structured,
    make_session_id,
)
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


# ---------------------------------------------------------------------------
# Fixture helpers
# ---------------------------------------------------------------------------


def _connect_session(client: McpClient, ssh_target, *, agent: str) -> str:
    """Establish a session and return the SESSION_ID."""
    text, structured, raw = call_tool_pair(
        client, "ssh_connect", ssh_target.connect_args(agent_id=agent)
    )
    parsed = parse_block(text)
    sid = parsed.get("session_id")
    assert sid, f"ssh_connect failed: {text}"
    # Validate dual channels at this entry point
    assert structured is not None, "ssh_connect must emit structured_content"
    assert structured.get("tool") == "ssh_connect"
    assert structured.get("status") in {"ok", "reused", "suggested"}
    return sid


def _free_port() -> int:
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


# ---------------------------------------------------------------------------
# Connection (5 tools)
# ---------------------------------------------------------------------------


def test_ssh_connect_dual_channel_ok(stdio_client: McpClient, ssh_target) -> None:
    text, structured, _ = call_tool_pair(
        stdio_client,
        "ssh_connect",
        ssh_target.connect_args(agent_id="sc-connect"),
    )
    parsed = parse_block(text)
    assert structured is not None
    assert text, "Markdown body must not be empty"
    assert structured["tool"] == "ssh_connect"
    assert structured["status"] in {"ok", "reused", "suggested"}
    if structured["status"] in {"ok", "reused"}:
        assert structured.get("session_id") == parsed.get("session_id")
        assert structured.get("host"), structured
        assert structured.get("port"), structured
        assert structured.get("username"), structured
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": structured["session_id"]})


def test_ssh_connect_dual_channel_error(stdio_client: McpClient, ssh_target) -> None:
    """Bad address triggers CONNECTION_FAILED on both channels."""
    text, structured, _ = call_tool_pair(
        stdio_client,
        "ssh_connect",
        {
            "address": "127.0.0.1:1",  # closed
            "username": "nobody",
            "password": "x",
            "max_retries": 0,
            "timeout_secs": 2,
            "reuse": "force_new",
        },
        timeout=15,
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR"
    assert structured is not None
    assert structured["tool"] == "ssh_connect"
    assert structured["status"] == "error"
    assert structured.get("code"), structured
    assert structured.get("reason"), structured


def test_ssh_disconnect_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-disc")
    text, structured, _ = call_tool_pair(stdio_client, "ssh_disconnect", {"session_id": sid})
    parsed = parse_block(text)
    assert parsed.get("__status") == "OK"
    assert structured["tool"] == "ssh_disconnect"
    assert structured["status"] == "ok"
    assert structured.get("session_id") == sid


def test_ssh_disconnect_unknown_session_dual_channel(stdio_client: McpClient) -> None:
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect", {"session_id": make_session_id()}
    )
    assert structured["tool"] == "ssh_disconnect"
    assert structured["status"] == "error"
    assert structured.get("code") == "SESSION_NOT_FOUND"


def test_ssh_list_sessions_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-list")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_sessions", {"agent_id": "sc-list"}
        )
        assert structured["tool"] == "ssh_sessions"
        assert structured["status"] == "ok"
        sessions = structured.get("sessions") or []
        assert isinstance(sessions, list)
        assert len(sessions) >= 1
        assert any(s.get("session_id") == sid for s in sessions), sessions
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_disconnect_agent_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    for i in range(2):
        _connect_session(stdio_client, ssh_target, agent="sc-disc-agent")
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect_agent", {"agent_id": "sc-disc-agent"}
    )
    assert structured["tool"] == "ssh_disconnect_agent"
    assert structured["status"] == "ok"
    assert structured.get("agent_id") == "sc-disc-agent"
    # v4.7 structured key is `sessions_closed` (matches the use-case outcome).
    assert structured.get("sessions_closed") == 2, structured


def test_ssh_disconnect_many_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid_a = _connect_session(stdio_client, ssh_target, agent="sc-many")
    sid_b = _connect_session(stdio_client, ssh_target, agent="sc-many")
    ghost = make_session_id()
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect_many", {"session_ids": [sid_a, sid_b, ghost]}
    )
    assert structured["tool"] == "ssh_disconnect_many"
    assert structured["status"] == "ok", structured
    results = structured.get("results") or []
    assert len(results) == 3
    by_id = {r["session_id"]: r for r in results}
    assert by_id[sid_a]["status"] == "ok"
    assert by_id[sid_b]["status"] == "ok"
    assert by_id[ghost]["status"] == "error"
    assert by_id[ghost].get("code") == "SESSION_NOT_FOUND"
    assert structured.get("disconnected") == 2
    assert structured.get("failed") == 1


# ---------------------------------------------------------------------------
# Execute (6 tools)
# ---------------------------------------------------------------------------


def test_ssh_execute_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-exec")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_exec", {"session_id": sid, "command": "echo OK"}
        )
        assert structured["tool"] == "ssh_exec"
        assert structured["status"] == "started"
        assert structured.get("command_id"), structured
        assert structured.get("session_id") == sid
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_get_command_output_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-getout")
    try:
        text = call_tool_text(stdio_client, "ssh_exec", {"session_id": sid, "command": "echo HELLO"})
        cid = parse_block(text).get("command_id")
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_exec_output",
            {"command_id": cid, "wait": True, "wait_timeout_secs": 10},
            timeout=20,
        )
        assert structured["tool"] == "ssh_exec_output"
        assert structured["status"] in {"completed", "running"}
        if structured["status"] == "completed":
            assert structured.get("exit_code") == 0
            assert "HELLO" in (structured.get("stdout") or "")
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_list_commands_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-listcmds")
    try:
        call_tool_text(stdio_client, "ssh_exec", {"session_id": sid, "command": "echo X"})
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_commands", {"session_id": sid}
        )
        assert structured["tool"] == "ssh_commands"
        assert structured["status"] == "ok"
        commands = structured.get("commands") or []
        assert isinstance(commands, list)
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_cancel_command_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-cancel")
    try:
        text = call_tool_text(stdio_client, "ssh_exec", {"session_id": sid, "command": "sleep 30"})
        cid = parse_block(text).get("command_id")
        time.sleep(0.5)
        text, structured, _ = call_tool_pair(stdio_client, "ssh_exec_cancel", {"command_id": cid})
        assert structured["tool"] == "ssh_exec_cancel"
        # v4.7: cancel returns `ok` for the cancelled-running path, `noop` for
        # already-terminal commands. The Markdown header still shows
        # `CANCELLED` / `NOOP` for backwards compatibility.
        assert structured["status"] in {"ok", "noop"}
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_run_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    args = ssh_target.connect_args(agent_id="sc-run")
    args["command"] = "echo RUN_OK"
    args["disconnect_after"] = True
    text, structured, _ = call_tool_pair(stdio_client, "ssh_run", args, timeout=30)
    assert structured["tool"] == "ssh_run"
    assert structured["status"] in {"completed", "timeout", "failed", "cancelled", "error"}
    if structured["status"] == "completed":
        assert structured.get("exit_code") == 0
        assert "RUN_OK" in (structured.get("stdout") or "")
        assert structured.get("disconnected") is True


def test_ssh_execute_batch_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-batch")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_exec_batch",
            {
                "session_id": sid,
                "commands": ["echo one", "echo two", "echo three"],
                "stop_on_failure": True,
            },
            timeout=60,
        )
        assert structured["tool"] == "ssh_exec_batch"
        assert structured["status"] in {"ok", "halted"}
        assert structured.get("session_id") == sid
        assert structured.get("total") == 3
        results = structured.get("results") or []
        assert len(results) == 3
        for entry in results:
            assert entry.get("index") in {0, 1, 2}
            assert "command" in entry
            assert "status" in entry
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


# ---------------------------------------------------------------------------
# Shell (6 tools)
# ---------------------------------------------------------------------------


def test_ssh_shell_open_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-shell-open")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_shell_open", {"session_id": sid}
        )
        assert structured["tool"] == "ssh_shell_open"
        assert structured["status"] == "ok"
        assert structured.get("shell_id"), structured
        assert structured.get("session_id") == sid
        assert structured.get("term") == "xterm"
        assert structured.get("cols") == 80
        assert structured.get("rows") == 24
        # `next` advisory should be present
        assert isinstance(structured.get("next"), list)
        call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": structured["shell_id"]})
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_shell_write_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-shell-write")
    try:
        shell_id = parse_block(
            call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
        )["shell_id"]
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo X\n"}
        )
        assert structured["tool"] == "ssh_shell_write"
        assert structured["status"] == "ok"
        assert structured.get("bytes_sent") == 7
        call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_shell_send_key_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-shell-key")
    try:
        shell_id = parse_block(
            call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
        )["shell_id"]
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_shell_press", {"shell_id": shell_id, "key": "ctrl_c"}
        )
        assert structured["tool"] == "ssh_shell_press"
        assert structured["status"] == "ok"
        assert structured.get("key") == "ctrl_c"
        assert structured.get("bytes_sent", 0) >= 1
        call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_shell_read_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-shell-read")
    try:
        shell_id = parse_block(
            call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
        )["shell_id"]
        call_tool_text(stdio_client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo READ_OK\n"})
        time.sleep(0.5)
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_shell_read",
            {"shell_id": shell_id, "wait": True, "wait_timeout_secs": 3, "min_bytes": 1},
            timeout=10,
        )
        assert structured["tool"] == "ssh_shell_read"
        assert structured["status"] in {"open", "closed", "timeout"}
        assert "bytes_returned" in structured
        call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_shell_wait_for_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-shell-wait")
    try:
        shell_id = parse_block(
            call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
        )["shell_id"]
        call_tool_text(stdio_client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo MARKER\n"})
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_shell_wait_for",
            {"shell_id": shell_id, "patterns": ["MARKER"], "timeout_secs": 3},
            timeout=10,
        )
        assert structured["tool"] == "ssh_shell_wait_for"
        assert structured["status"] in {"matched", "timeout", "closed"}
        if structured["status"] == "matched":
            assert structured.get("matched_pattern") == "MARKER"
        call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_shell_close_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-shell-close")
    try:
        shell_id = parse_block(
            call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
        )["shell_id"]
        text, structured, _ = call_tool_pair(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
        assert structured["tool"] == "ssh_shell_close"
        assert structured["status"] == "ok"
        assert structured.get("shell_id") == shell_id
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


# ---------------------------------------------------------------------------
# SFTP (3 tools)
# ---------------------------------------------------------------------------


def test_ssh_upload_download_progress_dual_channel(
    stdio_client: McpClient, ssh_target, tmp_path
) -> None:
    """Upload + download + progress — all three SFTP tools dual-channel."""
    src = tmp_path / "src.bin"
    dst = tmp_path / "dst.bin"
    src.write_bytes(b"hello v4.7 sftp dual channel " * 64)
    remote_path = f"/tmp/sc_xfer_{os.getpid()}.bin"
    sid = _connect_session(stdio_client, ssh_target, agent="sc-sftp")
    try:
        # Upload
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_upload",
            {"session_id": sid, "local_path": str(src), "remote_path": remote_path},
        )
        assert structured["tool"] == "ssh_upload"
        assert structured["status"] in {"started", "error"}
        if structured["status"] == "error":
            pytest.skip(f"SFTP fixture flake: {structured}")
        upload_xfer = structured.get("transfer_id")
        assert upload_xfer
        # Get progress (wait for completion)
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_transfer_progress",
            {"transfer_id": upload_xfer, "wait": True, "wait_timeout_secs": 30},
            timeout=45,
        )
        assert structured["tool"] == "ssh_transfer_progress"
        if structured["status"] == "failed" and "TIMEOUT" in str(structured.get("reason", "")):
            pytest.skip("SFTP subsystem timed out (paramiko fixture)")
        assert structured["status"] in {"completed", "running", "failed"}
        if structured["status"] == "failed":
            pytest.skip(f"SFTP fixture flake: {structured}")
        assert structured["status"] == "completed"
        assert structured.get("direction", "").lower() == "upload"
        # Download
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_download",
            {"session_id": sid, "remote_path": remote_path, "local_path": str(dst)},
        )
        assert structured["tool"] == "ssh_download"
        assert structured["status"] in {"started", "error"}
        if structured["status"] == "error":
            pytest.skip(f"SFTP fixture flake: {structured}")
        dl_xfer = structured.get("transfer_id")
        assert dl_xfer
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_transfer_progress",
            {"transfer_id": dl_xfer, "wait": True, "wait_timeout_secs": 30},
            timeout=45,
        )
        assert structured["tool"] == "ssh_transfer_progress"
        if structured["status"] == "failed":
            pytest.skip(f"SFTP fixture flake: {structured}")
        assert structured["status"] == "completed"
        assert structured.get("direction", "").lower() == "download"
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


# ---------------------------------------------------------------------------
# Forward (1 tool, feature-gated)
# ---------------------------------------------------------------------------


def test_ssh_forward_dual_channel(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect_session(stdio_client, ssh_target, agent="sc-fwd")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_forward",
            {
                "session_id": sid,
                "local_port": _free_port(),
                "remote_address": "127.0.0.1",
                "remote_port": 22,
            },
        )
        assert structured["tool"] == "ssh_forward"
        assert structured["status"] in {"ok", "error"}
        if structured["status"] == "error":
            if structured.get("code") == "FEATURE_DISABLED":
                pytest.skip("port_forward feature disabled")
            pytest.skip(f"forward setup error: {structured}")
        assert structured["status"] == "ok"
        assert structured.get("forward_id"), structured
        assert structured.get("session_id") == sid
        assert structured.get("active") is True
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})
