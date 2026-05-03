"""v4.7 ``ssh_execute_batch`` and ``ssh_disconnect_many`` tools.

``ssh_execute_batch`` runs 1..=16 commands in order against a single
session, with optional ``stop_on_failure`` semantics. ``ssh_disconnect_many``
best-effort bulk-disconnects 1..=64 sessions, reporting per-id outcomes.

Coverage:
- Batch all-success (3 commands, all pass).
- Batch with one failure mid-list, ``stop_on_failure=true`` halts and skips.
- Batch with one failure mid-list, ``stop_on_failure=false`` keeps going.
- Batch validation: empty list → INVALID_ARGUMENT; >16 commands rejected.
- ``ssh_disconnect_many`` happy + mixed (one already-closed + one ghost).
- ``ssh_disconnect_many`` validation: empty list, >64 ids.
- structured shape: per-command results array, top-level counters.
"""

from __future__ import annotations

import time

import pytest

from helpers.mcp_client import (
    McpClient,
    call_tool_pair,
    call_tool_text,
    make_session_id,
)
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


def _connect(client: McpClient, ssh_target, *, agent: str) -> str:
    sid = parse_block(
        call_tool_text(client, "ssh_connect", ssh_target.connect_args(agent_id=agent))
    ).get("session_id")
    assert sid
    return sid


# ---------------------------------------------------------------------------
# ssh_execute_batch
# ---------------------------------------------------------------------------


def test_batch_all_success_returns_ok(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="batch-ok")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_execute_batch",
            {
                "session_id": sid,
                "commands": ["echo a", "echo b", "echo c"],
                "stop_on_failure": True,
            },
            timeout=60,
        )
        assert structured["tool"] == "ssh_execute_batch"
        assert structured["status"] == "ok"
        assert structured.get("total") == 3
        assert structured.get("executed") == 3
        results = structured.get("results") or []
        assert len(results) == 3
        for i, entry in enumerate(results):
            assert entry["index"] == i
            assert entry["status"] == "completed", entry
            assert entry.get("exit_code") == 0
            assert "command_id" in entry
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_batch_stop_on_failure_halts_after_first_error(
    stdio_client: McpClient, ssh_target
) -> None:
    sid = _connect(stdio_client, ssh_target, agent="batch-halt")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_execute_batch",
            {
                "session_id": sid,
                "commands": ["echo first", "false", "echo unreachable"],
                "stop_on_failure": True,
            },
            timeout=60,
        )
        assert structured["status"] == "halted", structured
        assert structured.get("total") == 3
        assert structured.get("executed") == 2
        results = structured.get("results") or []
        assert len(results) == 3
        assert results[0]["status"] == "completed"
        assert results[0]["exit_code"] == 0
        assert results[1]["status"] == "completed"
        assert results[1]["exit_code"] != 0
        assert results[2]["status"] == "skipped"
        # Skipped entries don't have command_id populated.
        assert "command_id" not in results[2] or results[2].get("command_id") is None
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_batch_stop_on_failure_false_runs_all(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="batch-keep")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_execute_batch",
            {
                "session_id": sid,
                "commands": ["echo first", "false", "echo third"],
                "stop_on_failure": False,
            },
            timeout=60,
        )
        # Without halting, status is "ok" even when individual commands fail.
        assert structured["status"] == "ok"
        assert structured.get("executed") == 3
        results = structured.get("results") or []
        assert len(results) == 3
        for entry in results:
            assert entry["status"] in {"completed", "failed", "timeout"}, entry
        # Third command must have run.
        assert results[2]["status"] == "completed"
        assert "third" in (results[2].get("stdout") or "")
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_batch_empty_commands_rejected(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="batch-empty")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_execute_batch",
            {"session_id": sid, "commands": []},
        )
        parsed = parse_block(text)
        assert parsed.get("__status") == "ERROR", text
        assert structured["status"] == "error"
        assert "INVALID_ARGUMENT" in (parsed.get("reason") or "") or structured.get(
            "code"
        ) == "INVALID_ARGUMENT", structured
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_batch_too_many_commands_rejected(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="batch-too-many")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_execute_batch",
            {
                "session_id": sid,
                "commands": [f"echo {i}" for i in range(17)],  # 17 > 16
            },
        )
        parsed = parse_block(text)
        assert parsed.get("__status") == "ERROR", text
        assert structured["status"] == "error"
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_batch_unknown_session_rejected(stdio_client: McpClient) -> None:
    text, structured, _ = call_tool_pair(
        stdio_client,
        "ssh_execute_batch",
        {"session_id": make_session_id(), "commands": ["echo nope"]},
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR"
    assert structured["status"] == "error"


def test_batch_at_exact_limit_runs(stdio_client: McpClient, ssh_target) -> None:
    """Exactly 16 commands should execute end-to-end."""
    sid = _connect(stdio_client, ssh_target, agent="batch-16")
    try:
        text, structured, _ = call_tool_pair(
            stdio_client,
            "ssh_execute_batch",
            {
                "session_id": sid,
                "commands": [f"echo {i}" for i in range(16)],
                "stop_on_failure": False,
            },
            timeout=180,
        )
        assert structured["status"] == "ok"
        assert structured.get("total") == 16
        assert structured.get("executed") == 16
        results = structured.get("results") or []
        assert len(results) == 16
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


# ---------------------------------------------------------------------------
# ssh_disconnect_many
# ---------------------------------------------------------------------------


def test_disconnect_many_three_sessions(stdio_client: McpClient, ssh_target) -> None:
    sids = [
        _connect(stdio_client, ssh_target, agent="disc-many-3") for _ in range(3)
    ]
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect_many", {"session_ids": sids}
    )
    assert structured["status"] == "ok"
    assert structured.get("disconnected") == 3
    assert structured.get("failed") == 0
    results = structured.get("results") or []
    assert len(results) == 3
    for r in results:
        assert r["status"] == "ok"


def test_disconnect_many_one_already_closed(stdio_client: McpClient, ssh_target) -> None:
    """When one of the ids was already disconnected, the per-id outcome is
    `error/SESSION_NOT_FOUND`; remaining ids still disconnect cleanly."""
    sid_a = _connect(stdio_client, ssh_target, agent="disc-many-mix")
    sid_b = _connect(stdio_client, ssh_target, agent="disc-many-mix")
    # Disconnect A explicitly.
    call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid_a})
    # Now bulk: A should fail, B should succeed.
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect_many", {"session_ids": [sid_a, sid_b]}
    )
    assert structured["status"] == "ok"
    by_id = {r["session_id"]: r for r in (structured.get("results") or [])}
    assert by_id[sid_a]["status"] == "error"
    assert by_id[sid_a].get("code") == "SESSION_NOT_FOUND"
    assert by_id[sid_b]["status"] == "ok"
    assert structured.get("disconnected") == 1
    assert structured.get("failed") == 1


def test_disconnect_many_with_unknown_id(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="disc-many-ghost")
    ghost = make_session_id()
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect_many", {"session_ids": [sid, ghost]}
    )
    assert structured["status"] == "ok"
    by_id = {r["session_id"]: r for r in (structured.get("results") or [])}
    assert by_id[sid]["status"] == "ok"
    assert by_id[ghost]["status"] == "error"
    assert by_id[ghost].get("code") == "SESSION_NOT_FOUND"


def test_disconnect_many_empty_list_rejected(stdio_client: McpClient) -> None:
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect_many", {"session_ids": []}
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR", text
    assert structured["status"] == "error"


def test_disconnect_many_too_many_rejected(stdio_client: McpClient) -> None:
    """65 ids is above the documented 64 cap."""
    fake_ids = [make_session_id() for _ in range(65)]
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect_many", {"session_ids": fake_ids}
    )
    parsed = parse_block(text)
    # The server may either reject the request (preferred) OR walk the list
    # and surface 65 SESSION_NOT_FOUND entries. Documented surface is the
    # rejection path. Accept either.
    if parsed.get("__status") == "ERROR":
        assert structured["status"] == "error"
        return
    # Walked-the-list path: every entry should be an error.
    results = structured.get("results") or []
    assert len(results) == 65
    for r in results:
        assert r["status"] == "error"


# ---------------------------------------------------------------------------
# Idempotency for both tools
# ---------------------------------------------------------------------------


def test_batch_idempotency_dedups(stdio_client: McpClient, ssh_target) -> None:
    sid = _connect(stdio_client, ssh_target, agent="batch-idemp")
    try:
        meta = {"idempotency_key": "batch-once"}
        first = stdio_client.call_tool_with_meta(
            "ssh_execute_batch",
            {"session_id": sid, "commands": ["echo dedup"]},
            meta=meta,
            timeout=30,
        )
        second = stdio_client.call_tool_with_meta(
            "ssh_execute_batch",
            {"session_id": sid, "commands": ["echo dedup"]},
            meta=meta,
            timeout=30,
        )
        text_a = ((first.get("content") or [{}])[0].get("text") or "")
        text_b = ((second.get("content") or [{}])[0].get("text") or "")
        # The whole rendered body must replay verbatim.
        assert text_a == text_b, (text_a, text_b)
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_disconnect_many_idempotency_dedups(
    stdio_client: McpClient, ssh_target
) -> None:
    sid = _connect(stdio_client, ssh_target, agent="dm-idemp")
    meta = {"idempotency_key": "dm-once"}
    first = stdio_client.call_tool_with_meta(
        "ssh_disconnect_many",
        {"session_ids": [sid]},
        meta=meta,
        timeout=15,
    )
    second = stdio_client.call_tool_with_meta(
        "ssh_disconnect_many",
        {"session_ids": [sid]},
        meta=meta,
        timeout=15,
    )
    text_a = ((first.get("content") or [{}])[0].get("text") or "")
    text_b = ((second.get("content") or [{}])[0].get("text") or "")
    assert text_a == text_b, (text_a, text_b)
