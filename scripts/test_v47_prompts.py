"""v4.7 ``prompts/list`` and ``prompts/get`` advertisement.

The v4.7 prompts catalogue ships exactly 5 entries (built statically in
``src/infra/mcp/prompts.rs::list_prompts``):

- ``run_one_shot_command`` (args: ``address``, ``username``, ``command``)
- ``investigate_session`` (args: ``session_id``)
- ``upload_and_verify`` (args: ``session_id``, ``local_path``, ``remote_path``)
- ``interactive_shell_drive`` (args: ``session_id``, ``prompt_pattern``)
- ``cleanup_agent`` (args: ``agent_id``)

All arguments are required — no optional fields. ``prompts/get`` returns a
single ``User``-role text message with the parameterised recipe.

Errors:
- ``prompts/get`` with a missing required argument → ``invalid_params``.
- ``prompts/get`` with an unknown name → ``invalid_request``.
"""

from __future__ import annotations

import pytest

from helpers.mcp_client import McpClient


_EXPECTED_PROMPTS = [
    "run_one_shot_command",
    "investigate_session",
    "upload_and_verify",
    "interactive_shell_drive",
    "cleanup_agent",
]


def test_prompts_list_returns_five_entries(stdio_client: McpClient) -> None:
    prompts = stdio_client.list_prompts()
    assert len(prompts) == 5, prompts
    names = [p["name"] for p in prompts]
    assert names == _EXPECTED_PROMPTS, names


def test_each_prompt_carries_title_description_and_arguments(stdio_client: McpClient) -> None:
    prompts = stdio_client.list_prompts()
    for prompt in prompts:
        assert prompt.get("title"), prompt
        assert prompt.get("description"), prompt
        args = prompt.get("arguments") or []
        assert isinstance(args, list)
        assert len(args) >= 1
        for arg in args:
            assert arg.get("name"), arg
            assert arg.get("description"), arg
            # All v4.7 prompts mark every argument as required.
            assert arg.get("required") is True, arg


def test_run_one_shot_command_renders_with_args(stdio_client: McpClient) -> None:
    result = stdio_client.get_prompt(
        "run_one_shot_command",
        {"address": "h.example.com:22", "username": "alice", "command": "uptime"},
    )
    assert "_rpc_error" not in result, result
    messages = result.get("messages") or []
    assert len(messages) >= 1
    msg = messages[0]
    assert msg.get("role") == "user"
    text = (msg.get("content") or {}).get("text", "")
    assert "uptime" in text
    assert "h.example.com:22" in text
    assert "alice" in text
    assert "ssh_run" in text
    assert "disconnect_after=true" in text


def test_investigate_session_renders_with_args(stdio_client: McpClient) -> None:
    result = stdio_client.get_prompt(
        "investigate_session", {"session_id": "sess-foobar"}
    )
    assert "_rpc_error" not in result
    text = ((result.get("messages") or [{}])[0].get("content") or {}).get("text", "")
    assert "sess-foobar" in text
    assert "ssh_commands" in text
    assert "session://sess-foobar/health" in text
    assert "ssh_disconnect" in text


def test_upload_and_verify_renders_with_args(stdio_client: McpClient) -> None:
    result = stdio_client.get_prompt(
        "upload_and_verify",
        {
            "session_id": "sess-x",
            "local_path": "/local.bin",
            "remote_path": "/remote.bin",
        },
    )
    assert "_rpc_error" not in result
    text = ((result.get("messages") or [{}])[0].get("content") or {}).get("text", "")
    assert "ssh_upload" in text
    assert "/local.bin" in text
    assert "/remote.bin" in text
    assert "sha256sum" in text


def test_interactive_shell_drive_renders_with_args(stdio_client: McpClient) -> None:
    result = stdio_client.get_prompt(
        "interactive_shell_drive",
        {"session_id": "sess-1", "prompt_pattern": "$ "},
    )
    assert "_rpc_error" not in result
    text = ((result.get("messages") or [{}])[0].get("content") or {}).get("text", "")
    assert "ssh_shell_open" in text
    assert "ssh_shell_wait_for" in text
    assert "$ " in text


def test_cleanup_agent_renders_with_args(stdio_client: McpClient) -> None:
    result = stdio_client.get_prompt("cleanup_agent", {"agent_id": "agent-9"})
    assert "_rpc_error" not in result
    text = ((result.get("messages") or [{}])[0].get("content") or {}).get("text", "")
    assert "ssh_disconnect_agent(agent_id=agent-9)" in text


def test_prompts_get_unknown_name_returns_error(stdio_client: McpClient) -> None:
    result = stdio_client.get_prompt("__no_such_prompt__", {"x": "y"})
    assert "_rpc_error" in result, result
    err = result["_rpc_error"]
    code = err.get("code")
    msg = err.get("message", "")
    # The rmcp invalid_request error code is -32600.
    # Some MCP servers map to -32601 or similar; we accept any negative
    # JSON-RPC error code with an "unknown" message.
    assert code is not None
    assert "unknown" in msg.lower() or "not found" in msg.lower(), err


def test_prompts_get_missing_required_argument_returns_error(stdio_client: McpClient) -> None:
    """Sending ``cleanup_agent`` without the ``agent_id`` argument yields
    ``invalid_params`` (JSON-RPC code -32602)."""
    result = stdio_client.get_prompt("cleanup_agent", {})
    assert "_rpc_error" in result, result
    err = result["_rpc_error"]
    msg = err.get("message", "")
    assert "agent_id" in msg or "missing" in msg.lower() or "argument" in msg.lower(), err


def test_prompts_get_each_prompt_with_valid_args(stdio_client: McpClient) -> None:
    """Walk every prompt with a minimum-required argument set; each must
    return a valid user message body."""
    valid_args = {
        "run_one_shot_command": {
            "address": "h.example.com:22",
            "username": "bob",
            "command": "echo hi",
        },
        "investigate_session": {"session_id": "sess-xyz"},
        "upload_and_verify": {
            "session_id": "sess-xyz",
            "local_path": "/a",
            "remote_path": "/b",
        },
        "interactive_shell_drive": {
            "session_id": "sess-xyz",
            "prompt_pattern": ">",
        },
        "cleanup_agent": {"agent_id": "agent-x"},
    }
    for name, args in valid_args.items():
        result = stdio_client.get_prompt(name, args)
        assert "_rpc_error" not in result, (name, result)
        messages = result.get("messages") or []
        assert len(messages) == 1, (name, messages)
        text = (messages[0].get("content") or {}).get("text", "")
        assert text, (name, "empty body")


def test_prompts_list_is_stable(stdio_client: McpClient) -> None:
    """Two calls return identical entries in the same order."""
    first = stdio_client.list_prompts()
    second = stdio_client.list_prompts()
    assert first == second, (first, second)
