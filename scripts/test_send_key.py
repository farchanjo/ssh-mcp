"""ssh_shell_send_key validation suite.

Covers the documented modifier rules and a few of the more exotic key
encodings.
"""

from __future__ import annotations

import time

import pytest

from helpers.mcp_client import McpClient, call_tool_text
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


@pytest.fixture
def open_shell(http_client: McpClient, ssh_target):
    sid = parse_block(
        call_tool_text(http_client, "ssh_connect", ssh_target.connect_args(agent_id="key-test"))
    ).get("session_id")
    assert sid
    shell_id = parse_block(
        call_tool_text(http_client, "ssh_shell_open", {"session_id": sid})
    ).get("shell_id")
    assert shell_id
    yield http_client, shell_id
    call_tool_text(http_client, "ssh_shell_close", {"shell_id": shell_id})
    call_tool_text(http_client, "ssh_disconnect", {"session_id": sid})


def test_ctrl_c_breaks_cat(open_shell) -> None:
    client, shell_id = open_shell
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "cat\n"})
    time.sleep(0.4)
    parsed = parse_block(
        call_tool_text(client, "ssh_shell_send_key", {"shell_id": shell_id, "key": "ctrl_c"})
    )
    assert parsed.get("__status") == "OK"
    assert parsed.get("key") == "ctrl_c"

    # Verify the shell is responsive afterwards.
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo SHELL_LIVES\n"})
    out = parse_block(
        call_tool_text(
            client,
            "ssh_shell_read",
            {"shell_id": shell_id, "wait": True, "wait_timeout_secs": 3, "min_bytes": 4},
            timeout=10,
        )
    )
    assert "SHELL_LIVES" in (out.get("data") or ""), out


def test_arrow_up_after_history(open_shell) -> None:
    client, shell_id = open_shell
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo first\n"})
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "echo second\n"})
    time.sleep(0.6)
    # Drain initial output so the next read only contains arrow-induced bytes.
    call_tool_text(
        client, "ssh_shell_read", {"shell_id": shell_id, "clear": True, "max_output_bytes": 65536}
    )
    parsed = parse_block(
        call_tool_text(client, "ssh_shell_send_key", {"shell_id": shell_id, "key": "arrow_up"})
    )
    assert parsed.get("__status") == "OK"
    assert parsed.get("key") == "arrow_up"
    # bash echoes the recalled command; xterm-style \x1b[A bytes also visible.
    time.sleep(0.4)
    out = parse_block(
        call_tool_text(client, "ssh_shell_read", {"shell_id": shell_id, "max_output_bytes": 65536})
    )
    # Either the recalled command text appears, or the escape sequence was
    # delivered (depends on whether the remote shell has readline enabled).
    rendered = out.get("data") or ""
    assert "second" in rendered or "first" in rendered or "[A" in rendered, rendered


def test_f1_no_modifier_emits_bytes(open_shell) -> None:
    client, shell_id = open_shell
    # Run cat so the bytes echo back deterministically.
    call_tool_text(client, "ssh_shell_write", {"shell_id": shell_id, "input": "cat\n"})
    time.sleep(0.3)
    call_tool_text(
        client, "ssh_shell_read", {"shell_id": shell_id, "clear": True, "max_output_bytes": 65536}
    )
    parsed = parse_block(
        call_tool_text(client, "ssh_shell_send_key", {"shell_id": shell_id, "key": "f1"})
    )
    assert parsed.get("__status") == "OK"
    # F1 = ESC O P (xterm) or ESC [ 1 1 ~ (some terms).
    assert parsed.get("bytes_sent", 0) >= 3
    # Send Ctrl+C to leave cat.
    call_tool_text(client, "ssh_shell_send_key", {"shell_id": shell_id, "key": "ctrl_c"})


def test_shift_tab_emits_back_tab(open_shell) -> None:
    client, shell_id = open_shell
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_send_key",
            {"shell_id": shell_id, "key": "tab", "shift": True},
        )
    )
    assert parsed.get("__status") == "OK"
    # back-tab = \x1b[Z = 3 bytes
    assert parsed.get("bytes_sent", 0) == 3


def test_modifier_rejected_on_ctrl_c(open_shell) -> None:
    client, shell_id = open_shell
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_send_key",
            {"shell_id": shell_id, "key": "ctrl_c", "shift": True},
        )
    )
    assert parsed.get("__status") == "ERROR"
    assert "MODIFIER_NOT_ALLOWED" in (parsed.get("reason") or parsed.get("__text") or "")
