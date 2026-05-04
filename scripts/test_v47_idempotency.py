"""v4.7 ``_meta.idempotency_key`` cache + replay.

The 15 mutating tools accept an ``_meta.idempotency_key`` (1..=256 bytes).
When the key + tool tuple has been seen within the TTL window, the
server returns the cached response verbatim — the use case is NOT
re-executed. Read-only tools intentionally ignore the key.

Defaults:
- TTL: 300s (env ``SSH_IDEMPOTENCY_TTL_SECS``).
- Cap: 1024 entries (env ``SSH_IDEMPOTENCY_MAX_ENTRIES``).
- Key length cap: 256 bytes (``IDEMPOTENCY_KEY_TOO_LONG`` on overflow).
- Empty keys are treated as absent.

Coverage:
- Same key replays cached response (verbatim Markdown).
- Different keys re-execute.
- Without a key, every call re-executes.
- TTL expiry: with ``SSH_IDEMPOTENCY_TTL_SECS=2``, a replay past the
  window re-fires the use case (this requires a dedicated subprocess so
  the env var sticks; the default ``stdio_client`` fixture inherits the
  test-runner env).
- Oversized key rejected with ``IDEMPOTENCY_KEY_TOO_LONG``.
- Read-only tools ignore the key (same key → fresh execution).
"""

from __future__ import annotations

import os
import subprocess
import time

import pytest

from helpers.fixtures import STDIO_BIN
from helpers.mcp_client import McpClient, StdioTransport, call_tool_text
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _connect(client: McpClient, ssh_target, *, agent: str) -> str:
    sid = parse_block(
        call_tool_text(client, "ssh_connect", ssh_target.connect_args(agent_id=agent))
    ).get("session_id")
    assert sid
    return sid


def _make_client(env: dict | None = None) -> McpClient:
    transport = StdioTransport(
        [str(STDIO_BIN)],
        env={"RUST_LOG": os.environ.get("RUST_LOG", "warn"), **(env or {})},
    )
    client = McpClient(transport)
    client.initialize()
    return client


# ---------------------------------------------------------------------------
# Mutating-tool dedup
# ---------------------------------------------------------------------------


def test_same_key_dedups_ssh_execute(stdio_client: McpClient, ssh_target) -> None:
    """Two calls with the same key must replay the same command_id (no
    second async command spawned)."""
    sid = _connect(stdio_client, ssh_target, agent="idemp-exec")
    try:
        meta = {"idempotency_key": "exec-once-only"}
        first = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo IDEMP"},
            meta=meta,
        )
        second = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo IDEMP"},
            meta=meta,
        )
        text_a = ((first.get("content") or [{}])[0].get("text") or "")
        text_b = ((second.get("content") or [{}])[0].get("text") or "")
        # Cached -> identical body.
        assert text_a == text_b, (text_a, text_b)
        cid_a = parse_block(text_a).get("command_id")
        cid_b = parse_block(text_b).get("command_id")
        assert cid_a == cid_b, (cid_a, cid_b)
        # Verify with ssh_list_commands that only ONE async command was spawned.
        listed = parse_block(
            call_tool_text(stdio_client, "ssh_commands", {"session_id": sid})
        )
        # Count from bullet list.
        commands = listed.get("commands") or []
        assert len(commands) == 1, commands
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_different_keys_reexecute(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="idemp-diff")
    try:
        first = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo A"},
            meta={"idempotency_key": "k1"},
        )
        second = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo B"},
            meta={"idempotency_key": "k2"},
        )
        text_a = ((first.get("content") or [{}])[0].get("text") or "")
        text_b = ((second.get("content") or [{}])[0].get("text") or "")
        cid_a = parse_block(text_a).get("command_id")
        cid_b = parse_block(text_b).get("command_id")
        assert cid_a != cid_b, "different keys must spawn different commands"
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_no_key_reexecutes_each_call(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="idemp-none")
    try:
        first = stdio_client.call_tool(
            "ssh_exec", {"session_id": sid, "command": "echo X"}
        )
        second = stdio_client.call_tool(
            "ssh_exec", {"session_id": sid, "command": "echo X"}
        )
        cid_a = parse_block(((first.get("content") or [{}])[0].get("text") or "")).get(
            "command_id"
        )
        cid_b = parse_block(((second.get("content") or [{}])[0].get("text") or "")).get(
            "command_id"
        )
        assert cid_a != cid_b, "no key must always spawn a fresh command"
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_empty_key_treated_as_absent(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="idemp-empty")
    try:
        first = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo Y"},
            meta={"idempotency_key": ""},
        )
        second = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo Y"},
            meta={"idempotency_key": ""},
        )
        cid_a = parse_block(((first.get("content") or [{}])[0].get("text") or "")).get(
            "command_id"
        )
        cid_b = parse_block(((second.get("content") or [{}])[0].get("text") or "")).get(
            "command_id"
        )
        assert cid_a != cid_b, "empty key must be treated as absent"
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_oversized_key_rejected(stdio_client: McpClient, ssh_target) -> None:
    """Keys above 256 bytes must surface ``IDEMPOTENCY_KEY_TOO_LONG``."""
    sid = _connect(stdio_client, ssh_target, agent="idemp-toolong")
    try:
        too_long = "x" * 257
        result = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo nope"},
            meta={"idempotency_key": too_long},
        )
        text = ((result.get("content") or [{}])[0].get("text") or "")
        parsed = parse_block(text)
        assert parsed.get("__status") == "ERROR", text
        assert "IDEMPOTENCY_KEY_TOO_LONG" in (parsed.get("reason") or text)
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_key_at_exact_limit_accepted(stdio_client: McpClient, ssh_target) -> None:
    """A 256-byte key sits at the exact cap — must be accepted."""
    sid = _connect(stdio_client, ssh_target, agent="idemp-edge")
    try:
        key = "x" * 256
        result = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo OK"},
            meta={"idempotency_key": key},
        )
        text = ((result.get("content") or [{}])[0].get("text") or "")
        parsed = parse_block(text)
        assert parsed.get("__status") == "STARTED", text
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


# ---------------------------------------------------------------------------
# TTL expiry
# ---------------------------------------------------------------------------


def test_ttl_expiry_reexecutes(ssh_target) -> None:
    """With ``SSH_IDEMPOTENCY_TTL_SECS=2``, a replay 3s later must NOT
    return the cached response — the use case fires fresh."""
    client = _make_client(env={"SSH_IDEMPOTENCY_TTL_SECS": "2"})
    try:
        sid = _connect(client, ssh_target, agent="idemp-ttl")
        try:
            meta = {"idempotency_key": "ttl-key"}
            first = client.call_tool_with_meta(
                "ssh_exec",
                {"session_id": sid, "command": "echo TTL_A"},
                meta=meta,
            )
            time.sleep(3.0)
            second = client.call_tool_with_meta(
                "ssh_exec",
                {"session_id": sid, "command": "echo TTL_B"},
                meta=meta,
            )
            text_a = ((first.get("content") or [{}])[0].get("text") or "")
            text_b = ((second.get("content") or [{}])[0].get("text") or "")
            cid_a = parse_block(text_a).get("command_id")
            cid_b = parse_block(text_b).get("command_id")
            # A and B must be DIFFERENT command_ids -> the TTL fired.
            assert cid_a != cid_b, (
                "TTL did not expire — same command_id replayed",
                cid_a,
                cid_b,
            )
        finally:
            call_tool_text(client, "ssh_disconnect", {"session_id": sid})
    finally:
        client.close()


# ---------------------------------------------------------------------------
# Read-only tools ignore the key
# ---------------------------------------------------------------------------


def test_readonly_list_sessions_ignores_key(stdio_client: McpClient, ssh_target) -> None:
    """``ssh_list_sessions`` is read-only; the cache must NOT replay
    stale results."""
    sid = _connect(stdio_client, ssh_target, agent="idemp-ro")
    try:
        meta = {"idempotency_key": "ro-key"}
        first = stdio_client.call_tool_with_meta(
            "ssh_sessions", {"agent_id": "idemp-ro"}, meta=meta
        )
        # Add a second session.
        sid2 = _connect(stdio_client, ssh_target, agent="idemp-ro")
        second = stdio_client.call_tool_with_meta(
            "ssh_sessions", {"agent_id": "idemp-ro"}, meta=meta
        )
        text_a = ((first.get("content") or [{}])[0].get("text") or "")
        text_b = ((second.get("content") or [{}])[0].get("text") or "")
        # Read-only -> the second call must report the new count, NOT the
        # cached one.
        count_a = parse_block(text_a).get("count", 0)
        count_b = parse_block(text_b).get("count", 0)
        assert count_b > count_a, (
            "read-only tool incorrectly replayed cached result",
            count_a,
            count_b,
        )
    finally:
        for s in (sid, sid2):
            call_tool_text(stdio_client, "ssh_disconnect", {"session_id": s})


# ---------------------------------------------------------------------------
# Cross-tool isolation
# ---------------------------------------------------------------------------


def test_same_key_different_tools_no_collision(
    stdio_client: McpClient, ssh_target
) -> None:
    """The cache is keyed on (tool_name, key). Using the same key against
    two different tools must NOT collide."""
    sid = _connect(stdio_client, ssh_target, agent="idemp-cross")
    try:
        meta = {"idempotency_key": "shared-key"}
        # First: ssh_execute
        result_exec = stdio_client.call_tool_with_meta(
            "ssh_exec",
            {"session_id": sid, "command": "echo EXEC"},
            meta=meta,
        )
        # Second: ssh_shell_open with same key
        result_open = stdio_client.call_tool_with_meta(
            "ssh_shell_open",
            {"session_id": sid},
            meta=meta,
        )
        text_exec = ((result_exec.get("content") or [{}])[0].get("text") or "")
        text_open = ((result_open.get("content") or [{}])[0].get("text") or "")
        parsed_exec = parse_block(text_exec)
        parsed_open = parse_block(text_open)
        # Each call hit its own use case path.
        assert parsed_exec.get("__tool") == "SSH_EXECUTE"
        assert parsed_open.get("__tool") == "SSH_SHELL_OPEN"
        # Cleanup
        if parsed_open.get("shell_id"):
            call_tool_text(
                stdio_client, "ssh_shell_close", {"shell_id": parsed_open["shell_id"]}
            )
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})
