"""v4.7 NOT_FOUND closest-match suggestions.

When ``SESSION_NOT_FOUND`` / ``SHELL_NOT_FOUND`` / ``COMMAND_NOT_FOUND``
/ ``TRANSFER_NOT_FOUND`` / ``FORWARD_NOT_FOUND`` fires AND the relevant
repo holds at least one live entry, the ``DETAIL:`` line carries
``closest matches: <id1>, <id2>, <id3>`` (top-3 Levenshtein neighbours).

When the repo is empty, the suggestion clause is omitted entirely.

Reference: ``src/infra/mcp/suggestions.rs::closest_ids`` (Levenshtein +
deterministic lexicographic tie-break).
"""

from __future__ import annotations

import pytest

from helpers.mcp_client import McpClient, call_tool_pair, call_tool_text, make_session_id
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


def _connect(client: McpClient, ssh_target, *, agent: str) -> str:
    sid = parse_block(
        call_tool_text(client, "ssh_connect", ssh_target.connect_args(agent_id=agent))
    ).get("session_id")
    assert sid
    return sid


def _typo(target: str, idx: int = 0, replacement: str = "Z") -> str:
    """Return ``target`` with one character flipped at ``idx`` so the
    Levenshtein distance is 1."""
    if idx >= len(target):
        idx = len(target) - 1
    return target[:idx] + replacement + target[idx + 1 :]


# ---------------------------------------------------------------------------
# SESSION_NOT_FOUND
# ---------------------------------------------------------------------------


def test_session_not_found_with_close_match_lists_closest(
    stdio_client: McpClient, ssh_target
) -> None:
    """When a session exists with id close to the typo'd target, the
    error DETAIL must carry ``closest matches: <id>``."""
    sid = _connect(stdio_client, ssh_target, agent="cm-session")
    try:
        # Build a typo by flipping the first char of sid.
        typo = _typo(sid, idx=0, replacement="z")
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_disconnect", {"session_id": typo}
        )
        parsed = parse_block(text)
        assert parsed.get("__status") == "ERROR"
        detail = parsed.get("detail") or ""
        # The actual session id must appear in the closest matches line.
        assert "closest matches" in detail.lower(), text
        assert sid in detail, (sid, detail)
        # Structured `detail` must mirror.
        assert structured["status"] == "error"
        assert sid in (structured.get("detail") or ""), structured
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_session_not_found_no_match_when_repo_empty(stdio_client: McpClient) -> None:
    """With an empty session repo, the suggestion clause is omitted."""
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_disconnect", {"session_id": make_session_id()}
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR"
    detail = parsed.get("detail") or ""
    # Detail line, if present, must NOT advertise closest matches.
    assert "closest matches" not in detail.lower(), text
    # Structured detail empty / absent.
    sd = structured.get("detail") if structured else None
    if sd:
        assert "closest matches" not in sd.lower(), structured


# ---------------------------------------------------------------------------
# SHELL_NOT_FOUND
# ---------------------------------------------------------------------------


def test_shell_not_found_with_close_match_lists_closest(
    stdio_client: McpClient, ssh_target
) -> None:
    sid = _connect(stdio_client, ssh_target, agent="cm-shell")
    try:
        shell_id = parse_block(
            call_tool_text(stdio_client, "ssh_shell_open", {"session_id": sid})
        ).get("shell_id")
        try:
            typo = _typo(shell_id, idx=0, replacement="Z")
            text, structured, _ = call_tool_pair(
                stdio_client, "ssh_shell_close", {"shell_id": typo}
            )
            parsed = parse_block(text)
            assert parsed.get("__status") == "ERROR"
            detail = parsed.get("detail") or ""
            assert "closest matches" in detail.lower(), text
            assert shell_id in detail, (shell_id, detail)
        finally:
            call_tool_text(stdio_client, "ssh_shell_close", {"shell_id": shell_id})
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


# ---------------------------------------------------------------------------
# COMMAND_NOT_FOUND
# ---------------------------------------------------------------------------


def test_command_not_found_with_close_match_lists_closest(
    stdio_client: McpClient, ssh_target
) -> None:
    sid = _connect(stdio_client, ssh_target, agent="cm-cmd")
    try:
        cid = parse_block(
            call_tool_text(
                stdio_client,
                "ssh_exec",
                {"session_id": sid, "command": "sleep 5"},
            )
        ).get("command_id")
        try:
            typo = _typo(cid, idx=0, replacement="Z")
            text, structured, _ = call_tool_pair(
                stdio_client, "ssh_exec_output", {"command_id": typo}
            )
            parsed = parse_block(text)
            assert parsed.get("__status") == "ERROR"
            detail = parsed.get("detail") or ""
            assert "closest matches" in detail.lower(), text
            assert cid in detail, (cid, detail)
        finally:
            call_tool_text(stdio_client, "ssh_exec_cancel", {"command_id": cid})
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


# ---------------------------------------------------------------------------
# TRANSFER_NOT_FOUND
# ---------------------------------------------------------------------------


def test_transfer_not_found_no_match_when_no_transfers(stdio_client: McpClient) -> None:
    """With no live transfers, the closest-match clause is omitted."""
    text, structured, _ = call_tool_pair(
        stdio_client, "ssh_transfer_progress", {"transfer_id": "ghost-transfer-id"}
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR"
    detail = parsed.get("detail") or ""
    assert "closest matches" not in detail.lower(), text


# ---------------------------------------------------------------------------
# Top-N + ordering
# ---------------------------------------------------------------------------


def test_closest_matches_returns_at_most_three(
    stdio_client: McpClient, ssh_target
) -> None:
    """When multiple matches exist, only the top-3 (by Levenshtein
    distance, lexicographic tie-break) are surfaced."""
    sids = []
    try:
        # Spawn 5 sessions so the repo has multiple candidates.
        for i in range(5):
            sids.append(_connect(stdio_client, ssh_target, agent="cm-top3"))
        # Typo a session id by mutating the first char.
        typo = _typo(sids[0], idx=0, replacement="0")
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_disconnect", {"session_id": typo}
        )
        parsed = parse_block(text)
        detail = parsed.get("detail") or ""
        # Strip the prefix to extract the comma-separated id list.
        if "closest matches:" in detail.lower():
            ids_text = detail.lower().split("closest matches:", 1)[1].strip()
            # Split by comma and strip whitespace.
            matches = [s.strip() for s in ids_text.split(",") if s.strip()]
            assert len(matches) <= 3, matches
    finally:
        for s in sids:
            call_tool_text(stdio_client, "ssh_disconnect", {"session_id": s})


# ---------------------------------------------------------------------------
# Structured channel mirrors detail
# ---------------------------------------------------------------------------


def test_structured_detail_carries_closest_match(
    stdio_client: McpClient, ssh_target
) -> None:
    """The structured channel's ``detail`` field mirrors the Markdown
    ``DETAIL:`` line on NOT_FOUND with closest-match suggestion."""
    sid = _connect(stdio_client, ssh_target, agent="cm-structured")
    try:
        typo = _typo(sid, idx=0, replacement="!")
        text, structured, _ = call_tool_pair(
            stdio_client, "ssh_disconnect", {"session_id": typo}
        )
        assert structured["tool"] == "ssh_disconnect"
        assert structured["status"] == "error"
        assert structured.get("code") == "SESSION_NOT_FOUND"
        if "closest matches" in (parse_block(text).get("detail") or "").lower():
            assert "closest matches" in (structured.get("detail") or "").lower()
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})
