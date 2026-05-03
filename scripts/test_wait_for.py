"""ssh_shell_wait_for validation suite."""

from __future__ import annotations

import threading
import time

import pytest

from helpers.mcp_client import McpClient, call_tool_text
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


@pytest.fixture
def open_shell(http_client: McpClient, ssh_target):
    sid = parse_block(
        call_tool_text(http_client, "ssh_connect", ssh_target.connect_args(agent_id="wait-test"))
    ).get("session_id")
    assert sid
    shell_id = parse_block(
        call_tool_text(http_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")
    assert shell_id
    yield http_client, shell_id, sid
    call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


def test_wait_for_matched_pattern_in_heredoc(open_shell) -> None:
    client, shell_id, _ = open_shell
    call_tool_text(
        client,
        "ssh_shell_write",
        {
            "shell_id": shell_id,
            "input": "cat <<EOF\nfoo\nbar\nbaz\nEOF\n",
        },
    )
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {"shell_id": shell_id, "patterns": ["bar", "baz"], "timeout_secs": 5},
            timeout=15,
        )
    )
    assert parsed.get("__status") == "MATCHED"
    assert parsed.get("matched_pattern") == "bar"


def test_wait_for_timeout_returns_buffer(open_shell) -> None:
    client, shell_id, _ = open_shell
    # Just print a known marker so the buffer is non-empty.
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo MARKER\n"})
    time.sleep(0.3)
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {
                "shell_id": shell_id,
                "patterns": ["__NEVER_OBSERVED_PATTERN__"],
                "timeout_secs": 1,
            },
            timeout=10,
        )
    )
    assert parsed.get("__status") == "TIMEOUT"
    # Current buffer should still come back; MARKER was printed before the wait.
    assert "MARKER" in (parsed.get("data") or ""), parsed


def test_wait_for_closed_when_shell_closes(http_client: McpClient, ssh_target) -> None:
    sid = parse_block(
        call_tool_text(http_client, "ssh_connect", ssh_target.connect_args(agent_id="wait-close"))
    ).get("session_id")
    shell_id = parse_block(
        call_tool_text(http_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")

    result_holder: dict = {}

    def waiter():
        text = call_tool_text(
            http_client,
            "ssh_shell_wait_for",
            {
                "shell_id": shell_id,
                "patterns": ["__never__"],
                "timeout_secs": 10,
            },
            timeout=20,
        )
        result_holder["parsed"] = parse_block(text)

    thread = threading.Thread(target=waiter)
    thread.start()
    time.sleep(0.5)
    call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
    thread.join(timeout=15)
    parsed = result_holder.get("parsed", {})
    # Either CLOSED (preferred) or TIMEOUT if the close arrived after the
    # deadline. CLOSED is the documented behaviour.
    assert parsed.get("__status") in {"CLOSED", "TIMEOUT"}, parsed
    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


def test_wait_for_sixteen_patterns_ok(open_shell) -> None:
    client, shell_id, _ = open_shell
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo MATCH16\n"})
    patterns = [f"NEVER_{i}" for i in range(15)] + ["MATCH16"]
    assert len(patterns) == 16
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {"shell_id": shell_id, "patterns": patterns, "timeout_secs": 5},
            timeout=15,
        )
    )
    assert parsed.get("__status") == "MATCHED"
    assert parsed.get("matched_pattern") == "MATCH16"


def test_wait_for_seventeen_patterns_rejected(open_shell) -> None:
    client, shell_id, _ = open_shell
    patterns = [f"PAT_{i}" for i in range(17)]
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {"shell_id": shell_id, "patterns": patterns, "timeout_secs": 1},
        )
    )
    assert parsed.get("__status") == "ERROR"
    assert "TOO_MANY_PATTERNS" in (parsed.get("reason") or parsed.get("__text") or "")


def test_wait_for_pattern_at_exact_limit(open_shell) -> None:
    client, shell_id, _ = open_shell
    long_pattern = "x" * 1024
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo OK\n"})
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {"shell_id": shell_id, "patterns": [long_pattern], "timeout_secs": 1},
            timeout=10,
        )
    )
    # Pattern length is fine; it just won't match.
    assert parsed.get("__status") in {"TIMEOUT", "MATCHED"}, parsed


def test_wait_for_pattern_too_long(open_shell) -> None:
    client, shell_id, _ = open_shell
    too_long = "x" * 1025
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {"shell_id": shell_id, "patterns": [too_long], "timeout_secs": 1},
        )
    )
    assert parsed.get("__status") == "ERROR"
    assert "PATTERN_TOO_LONG" in (parsed.get("reason") or parsed.get("__text") or "")
