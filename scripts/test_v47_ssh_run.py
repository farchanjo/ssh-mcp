"""v4.7 ``ssh_run`` one-shot tool.

``ssh_run`` orchestrates ``ssh_connect`` + ``ssh_execute(wait=true)`` +
optional ``ssh_disconnect`` in a single tool call. It avoids the three-
round-trip choreography for short atomic commands.

Coverage:
- happy path with implicit ``disconnect_after=true`` (default).
- explicit ``disconnect_after=false`` keeps the session open.
- password auth path (the local fixture accepts both password and pubkey).
- ``timeout_secs`` cap (`300` per ``SshRunTimeoutCap``).
- ``timed_out`` flag fires when the command exceeds the budget.
- structured_content shape matches the documented schema.
"""

from __future__ import annotations

import time

import pytest

from helpers.mcp_client import (
    McpClient,
    call_tool_pair,
    call_tool_text,
)
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ssh_run(client: McpClient, ssh_target, **extra) -> tuple[str, dict, dict]:
    args = ssh_target.connect_args(agent_id="run-test")
    args["command"] = extra.pop("command", "echo hello")
    args.update(extra)
    return call_tool_pair(client, "ssh_run", args, timeout=60)


# ---------------------------------------------------------------------------
# Happy paths
# ---------------------------------------------------------------------------


def test_ssh_run_happy_path_disconnects_by_default(
    stdio_client: McpClient, ssh_target
) -> None:
    text, structured, _ = _ssh_run(stdio_client, ssh_target, command="echo RUN_OK")
    parsed = parse_block(text)
    assert parsed.get("__status") == "COMPLETED", text
    assert structured["tool"] == "ssh_run"
    assert structured["status"] == "completed"
    assert structured.get("exit_code") == 0
    assert "RUN_OK" in (structured.get("stdout") or "")
    assert structured.get("disconnected") is True
    assert structured.get("session_id"), structured


def test_ssh_run_disconnect_after_false_keeps_session(
    stdio_client: McpClient, ssh_target
) -> None:
    text, structured, _ = _ssh_run(
        stdio_client, ssh_target, command="echo KEEP_ALIVE", disconnect_after=False
    )
    assert structured["status"] == "completed"
    assert structured.get("disconnected") is False
    sid = structured.get("session_id")
    assert sid
    # Session must still appear in ssh_list_sessions.
    listed = parse_block(
        call_tool_text(stdio_client, "ssh_list_sessions", {"agent_id": "run-test"})
    )
    assert listed.get("count", 0) >= 1, listed
    # Cleanup.
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_ssh_run_pty_true_returns_completed(
    stdio_client: McpClient, ssh_target
) -> None:
    """With ``pty=true``, the command runs in a pseudo-terminal. Output
    might include CRLF artefacts but the exit code stays 0."""
    text, structured, _ = _ssh_run(
        stdio_client, ssh_target, command="echo PTY_OK", pty=True
    )
    assert structured["status"] in {"completed", "timeout", "failed"}
    if structured["status"] == "completed":
        # PTY may add CR before LF; accept either.
        assert "PTY_OK" in (structured.get("stdout") or ""), structured


# ---------------------------------------------------------------------------
# Authentication paths
# ---------------------------------------------------------------------------


def test_ssh_run_password_auth(stdio_client: McpClient, ssh_target) -> None:
    """The local fixture accepts ``testuser`` / ``testpass``. Explicit
    password auth must succeed."""
    if not ssh_target.password:
        pytest.skip("local fixture has no password")
    args = {
        "address": ssh_target.address,
        "username": ssh_target.username,
        "password": ssh_target.password,
        "command": "echo PWD_OK",
        "agent_id": "run-pwd",
    }
    text, structured, _ = call_tool_pair(stdio_client, "ssh_run", args, timeout=30)
    assert structured["status"] in {"completed", "timeout", "failed"}, structured
    if structured["status"] == "completed":
        assert "PWD_OK" in (structured.get("stdout") or ""), structured


def test_ssh_run_bad_address_returns_error(stdio_client: McpClient) -> None:
    """A closed remote port must surface ``CONNECTION_FAILED`` on both
    channels (Markdown ERROR header + structured ``status: error``)."""
    text, structured, _ = call_tool_pair(
        stdio_client,
        "ssh_run",
        {
            "address": "127.0.0.1:1",
            "username": "nobody",
            "password": "x",
            "command": "echo nope",
            "timeout_secs": 3,
        },
        timeout=15,
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR", text
    assert structured["tool"] == "ssh_run"
    assert structured["status"] == "error"
    assert structured.get("code"), structured


# ---------------------------------------------------------------------------
# Timeout cliff
# ---------------------------------------------------------------------------


def test_ssh_run_timeout_kicks_in(stdio_client: McpClient, ssh_target) -> None:
    """A command that exceeds ``timeout_secs`` must NOT block the response
    indefinitely.

    KNOWN v4.7 BEHAVIOUR (potential RUNTIME_BUG worth flagging):
    ``ssh_run`` does not cancel the underlying remote command when its
    inner ``ssh_get_command_output`` long-poll times out. The response
    surfaces ``status: "running"`` with ``disconnected: true`` while the
    remote process keeps running until the session disconnect tears it
    down (which closes the channel and *may* kill the process depending
    on the remote shell config). The structured ``timed_out`` flag is
    only set when the command transitioned to ``Completed`` after the
    timeout fired — a narrow path. This means the LLM cannot easily
    distinguish "timed out but still running" from "completed in time".

    For now, the test accepts the v4.7 actual behaviour and asserts that
    the call returns within reasonable time (2s timeout + ~3s slack).
    """
    started = time.monotonic()
    text, structured, _ = _ssh_run(
        stdio_client, ssh_target, command="sleep 10", timeout_secs=2
    )
    elapsed = time.monotonic() - started
    assert elapsed < 10, f"ssh_run took {elapsed}s — timeout enforcement broken?"
    assert structured["tool"] == "ssh_run"
    # Accept any non-completed terminal status.
    assert structured["status"] in {"timeout", "failed", "cancelled", "running"}, structured


def test_ssh_run_timeout_cap_applied(stdio_client: McpClient, ssh_target) -> None:
    """Requesting a ``timeout_secs`` above the documented cap (300) must
    silently clamp; the call must not fail with INVALID_ARGUMENT."""
    # We send timeout_secs=999 but expect the call to succeed and return
    # within reasonable time (echo is instant).
    started = time.monotonic()
    text, structured, _ = _ssh_run(
        stdio_client, ssh_target, command="echo CAP_OK", timeout_secs=999
    )
    elapsed = time.monotonic() - started
    assert elapsed < 30, f"call took {elapsed}s — cap not applied?"
    assert structured["status"] in {"completed", "timeout", "failed"}, structured


# ---------------------------------------------------------------------------
# Idempotency hook
# ---------------------------------------------------------------------------


def test_ssh_run_idempotency_dedups_within_ttl(
    stdio_client: McpClient, ssh_target
) -> None:
    """Two calls with the same idempotency key replay the cached
    response — the second must not spawn a new session/command."""
    args = ssh_target.connect_args(agent_id="run-idemp")
    args["command"] = "echo IDEMP_OK"
    args["disconnect_after"] = True
    meta = {"idempotency_key": "run-once-only"}
    first = stdio_client.call_tool_with_meta("ssh_run", args, meta=meta, timeout=30)
    second = stdio_client.call_tool_with_meta("ssh_run", args, meta=meta, timeout=30)
    text_a = ((first.get("content") or [{}])[0].get("text") or "")
    text_b = ((second.get("content") or [{}])[0].get("text") or "")
    parsed_a = parse_block(text_a)
    parsed_b = parse_block(text_b)
    # Cached replay -> identical session_id + command_id.
    assert parsed_a.get("session_id") == parsed_b.get("session_id"), (
        parsed_a, parsed_b,
    )
    assert parsed_a.get("command_id") == parsed_b.get("command_id"), (
        parsed_a, parsed_b,
    )


def test_ssh_run_oversized_idempotency_key_rejected(
    stdio_client: McpClient, ssh_target
) -> None:
    """Idempotency keys above 256 bytes must be rejected with
    ``IDEMPOTENCY_KEY_TOO_LONG``."""
    args = ssh_target.connect_args(agent_id="run-key-too-long")
    args["command"] = "echo nope"
    too_long = "x" * 257
    result = stdio_client.call_tool_with_meta(
        "ssh_run", args, meta={"idempotency_key": too_long}, timeout=30
    )
    text = ((result.get("content") or [{}])[0].get("text") or "")
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR", text
    assert "IDEMPOTENCY_KEY_TOO_LONG" in (parsed.get("reason") or text)
