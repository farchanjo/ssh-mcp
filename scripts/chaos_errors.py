"""Chaos suite — force every tool's documented error path.

Each scenario is run against an isolated ``ssh-mcp-stdio`` child so leaks
or crashes do not cross-contaminate the next scenario. Every assertion
emits a single JSON line via :func:`helpers.chaos.write_event`. The final
line is the structured summary required by the v4.3 spec::

    {"chaos_errors": "ok", "tested": N, "passed": N, "failed": M}

Most assertions do not need a live SSH server — they probe the v4 use
cases directly through the rmcp markdown-error surface (bad ids, validation
failures, oversized inputs, oversized resource lists). Scenarios that need
a real sshd (auth-failed, large session lists, etc.) skip when
``SSH_MCP_TEST_TARGET`` is unset.

Transport: stdio only. The chaos suite does not need HTTP fan-out.
"""

from __future__ import annotations

import sys
import tempfile
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from helpers.chaos import (  # noqa: E402
    ChaosSshTarget,
    chaos_session,
    write_event,
    write_summary,
)
from helpers.mcp_client import McpClient, call_tool_text  # noqa: E402
from helpers.parse_block import parse_block  # noqa: E402


# ---------------------------------------------------------------------------
# Tiny per-scenario harness
# ---------------------------------------------------------------------------


def _record(stats: dict, ok: bool, panicked: bool) -> None:
    stats["tested"] += 1
    if ok and not panicked:
        stats["passed"] += 1
    else:
        stats["failed"] += 1
    if panicked:
        stats["panics"] += 1


def _assert(stats: dict, name: str, ok: bool, *, panicked: bool = False, **extra) -> None:
    payload: dict = {"test": name, "ok": bool(ok)}
    if panicked:
        payload["server_panicked"] = True
    payload.update(extra)
    write_event(payload)
    _record(stats, ok, panicked)


def _expect_error(stats: dict, name: str, parsed: dict, *expected_codes: str) -> None:
    reason = (parsed.get("reason") or "")
    status = parsed.get("__status")
    matched = status == "ERROR" and any(code in reason for code in expected_codes)
    _assert(
        stats,
        name,
        matched,
        expected_error="|".join(expected_codes),
        got=reason or status,
    )


def _expect_status(stats: dict, name: str, parsed: dict, *expected_status: str) -> None:
    matched = parsed.get("__status") in expected_status
    _assert(
        stats,
        name,
        matched,
        expected_status="|".join(expected_status),
        got=parsed.get("__status"),
    )


# ---------------------------------------------------------------------------
# Scenarios that DO NOT need a live sshd
# ---------------------------------------------------------------------------


def _scenario_connect_bad_host(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            {
                "address": "127.0.0.1:9",  # discard port — refused
                "username": "nobody",
                "password": "x",
                "max_retries": 0,
                "timeout_secs": 2,
                "reuse": "force_new",
            },
            timeout=15,
        )
    )
    _expect_error(stats, "connect_bad_host", parsed, "CONNECTION_FAILED", "TRANSPORT_ERROR")


def _scenario_connect_bad_address_format(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            {
                "address": "host:notaport",
                "username": "x",
                "password": "x",
                "max_retries": 0,
                "reuse": "force_new",
            },
            timeout=10,
        )
    )
    _expect_error(stats, "connect_bad_address_format", parsed, "INVALID_ARGUMENT")


def _scenario_execute_on_closed_session(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_exec",
            {"session_id": str(uuid.uuid4()), "command": "echo nope"},
        )
    )
    _expect_error(stats, "execute_on_closed_session", parsed, "SESSION_NOT_FOUND")


def _scenario_get_command_output_bad_id(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client, "ssh_exec_output", {"command_id": str(uuid.uuid4())}
        )
    )
    _expect_error(stats, "get_command_output_bad_id", parsed, "COMMAND_NOT_FOUND")


def _scenario_cancel_command_bad_id(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client, "ssh_exec_cancel", {"command_id": str(uuid.uuid4())}
        )
    )
    _expect_error(stats, "cancel_command_bad_id", parsed, "COMMAND_NOT_FOUND")


def _scenario_shell_write_on_closed_shell(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_write",
            {"shell_id": str(uuid.uuid4()), "input": "echo nope\n"},
        )
    )
    _expect_error(stats, "shell_write_closed_shell", parsed, "SHELL_NOT_FOUND")


def _scenario_send_key_invalid_repeat_zero(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_press",
            {"shell_id": str(uuid.uuid4()), "key": "arrow_up", "repeat": 0},
        )
    )
    _expect_error(stats, "send_key_repeat_zero", parsed, "INVALID_ARGUMENT", "INVALID_REPEAT")


def _scenario_send_key_invalid_repeat_high(client: McpClient, stats: dict) -> None:
    # repeat is u8 ∈ 1..=64. 65 must be rejected by the use case.
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_press",
            {"shell_id": str(uuid.uuid4()), "key": "arrow_up", "repeat": 65},
        )
    )
    _expect_error(stats, "send_key_repeat_too_high", parsed, "INVALID_ARGUMENT", "INVALID_REPEAT")


def _scenario_wait_for_empty_patterns(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {"shell_id": str(uuid.uuid4()), "patterns": [], "timeout_secs": 1},
        )
    )
    _expect_error(stats, "wait_for_empty_patterns", parsed, "INVALID_ARGUMENT", "EMPTY_PATTERNS")


def _scenario_wait_for_too_many_patterns(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {
                "shell_id": str(uuid.uuid4()),
                "patterns": [f"p{i}" for i in range(17)],
                "timeout_secs": 1,
            },
        )
    )
    _expect_error(stats, "wait_for_too_many_patterns", parsed, "TOO_MANY_PATTERNS", "INVALID_ARGUMENT")


def _scenario_wait_for_pattern_too_long(client: McpClient, stats: dict) -> None:
    big = "x" * 1100  # MAX_PATTERN_BYTES = 1024
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_shell_wait_for",
            {
                "shell_id": str(uuid.uuid4()),
                "patterns": [big],
                "timeout_secs": 1,
            },
        )
    )
    _expect_error(stats, "wait_for_pattern_too_long", parsed, "PATTERN_TOO_LONG", "INVALID_ARGUMENT")


def _scenario_shell_read_on_closed(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client, "ssh_shell_read", {"shell_id": str(uuid.uuid4())}
        )
    )
    _expect_error(stats, "shell_read_closed", parsed, "SHELL_NOT_FOUND")


def _scenario_upload_no_session(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_upload",
            {
                "session_id": str(uuid.uuid4()),
                "local_path": "/etc/hostname",
                "remote_path": "/tmp/x",
            },
        )
    )
    _expect_error(stats, "upload_no_session", parsed, "SESSION_NOT_FOUND")


def _scenario_download_no_session(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_download",
            {
                "session_id": str(uuid.uuid4()),
                "remote_path": "/etc/hostname",
                "local_path": "/tmp/dst.bin",
            },
        )
    )
    _expect_error(stats, "download_no_session", parsed, "SESSION_NOT_FOUND")


def _scenario_transfer_progress_bad_id(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client, "ssh_transfer_progress", {"transfer_id": str(uuid.uuid4())}
        )
    )
    _expect_error(stats, "transfer_progress_bad_id", parsed, "TRANSFER_NOT_FOUND")


def _scenario_disconnect_unknown_session(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client, "ssh_disconnect", {"session_id": str(uuid.uuid4())}
        )
    )
    _expect_error(stats, "disconnect_unknown_session", parsed, "SESSION_NOT_FOUND")


def _scenario_disconnect_agent_unknown(client: McpClient, stats: dict) -> None:
    parsed = parse_block(
        call_tool_text(
            client, "ssh_disconnect_agent", {"agent_id": "no-such-agent-xyz"}
        )
    )
    # Spec says: returns OK with sessions_disconnected = 0 (idempotent).
    ok = parsed.get("__status") == "OK" and (parsed.get("sessions_disconnected", 0) == 0)
    _assert(stats, "disconnect_agent_unknown_no_error", ok, got=parsed.get("__status"))


def _scenario_resource_read_invalid_uri(client: McpClient, stats: dict) -> None:
    result = client.read_resource("not-a-valid-uri-scheme://nope")
    err = result.get("_rpc_error")
    ok = err is not None
    _assert(stats, "resource_read_invalid_uri", ok, got=err)


def _scenario_resource_read_unknown_shell(client: McpClient, stats: dict) -> None:
    result = client.read_resource(f"shell://{uuid.uuid4()}/output")
    err = result.get("_rpc_error")
    ok = err is not None
    _assert(stats, "resource_read_unknown_shell", ok, got=err)


def _scenario_subscribe_invalid_uri(client: McpClient, stats: dict) -> None:
    try:
        client.subscribe("nonsense:::not-a-uri")
        # If we get here the server accepted a bogus URI — fail.
        _assert(stats, "subscribe_invalid_uri", False, got="accepted")
    except Exception as exc:
        _assert(stats, "subscribe_invalid_uri", True, got=str(exc)[:80])


# ---------------------------------------------------------------------------
# Scenarios that DO need a live sshd
# ---------------------------------------------------------------------------


def _scenario_connect_bad_credentials(client: McpClient, stats: dict, target: ChaosSshTarget) -> None:
    # Force password-only path: drop the key so the auth chain cannot fall
    # through to public-key, then use a username unlikely to exist on the
    # target host. The chain order is Password -> Key -> Agent — without a
    # key path and without ssh-agent socket the chain reduces to a single
    # password attempt that the server must reject.
    args = {
        "address": target.address,
        "username": f"nobody-chaos-{uuid.uuid4().hex[:8]}",
        "password": "definitely-wrong-password-xyz",
        "agent_id": "chaos-bad-creds",
        "max_retries": 0,
        "reuse": "force_new",
        "timeout_secs": 5,
    }
    parsed = parse_block(call_tool_text(client, "ssh_connect", args, timeout=15))
    _expect_error(stats, "connect_bad_credentials", parsed, "AUTH_FAILED", "CONNECTION_FAILED")


def _scenario_max_commands_exceeded(client: McpClient, stats: dict, target: ChaosSshTarget) -> None:
    sid = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            target.connect_args(agent_id="chaos-maxcmd", reuse="force_new"),
            timeout=15,
        )
    ).get("session_id")
    if not sid:
        _assert(stats, "max_commands_exceeded", False, got="connect failed")
        return
    accepted = 0
    error_seen = False
    last_reason = None
    try:
        # Limit is 100 — fire 105 sleep-30 commands so they stay RUNNING.
        for _ in range(105):
            parsed = parse_block(
                call_tool_text(
                    client,
                    "ssh_exec",
                    {"session_id": sid, "command": "sleep 30"},
                )
            )
            # `ssh_execute` reports STARTED on success.
            if parsed.get("__status") in {"STARTED", "OK"}:
                accepted += 1
            else:
                last_reason = parsed.get("reason") or ""
                if "MAX_COMMANDS_EXCEEDED" in last_reason:
                    error_seen = True
                    break
    finally:
        # Best-effort cleanup so we don't leak ssh sessions.
        call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)
    _assert(
        stats,
        "max_commands_exceeded",
        error_seen and accepted >= 100,
        accepted=accepted,
        last_reason=last_reason,
    )


def _scenario_max_shells_exceeded(client: McpClient, stats: dict, target: ChaosSshTarget) -> None:
    sid = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            target.connect_args(agent_id="chaos-maxshell", reuse="force_new"),
            timeout=15,
        )
    ).get("session_id")
    if not sid:
        _assert(stats, "max_shells_exceeded", False, got="connect failed")
        return
    accepted = 0
    error_seen = False
    opened: list[str] = []
    try:
        # Limit is 10 — try 11.
        for _ in range(11):
            parsed = parse_block(
                call_tool_text(client, "ssh_shell_open", {"session_id": sid})
            )
            if parsed.get("__status") == "OK":
                accepted += 1
                opened.append(parsed.get("shell_id"))
            elif "MAX_SHELLS_EXCEEDED" in (parsed.get("reason") or ""):
                error_seen = True
                break
    finally:
        for shid in opened:
            try:
                call_tool_text(client, "ssh_shell_close", {"shell_id": shid})
            except Exception:
                pass
        call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)
    _assert(
        stats,
        "max_shells_exceeded",
        error_seen and accepted == 10,
        accepted=accepted,
    )


def _scenario_cancel_completed_command(client: McpClient, stats: dict, target: ChaosSshTarget) -> None:
    sid = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            target.connect_args(agent_id="chaos-cancel-completed", reuse="force_new"),
            timeout=15,
        )
    ).get("session_id")
    if not sid:
        _assert(stats, "cancel_completed_command", False, got="connect failed")
        return
    cid = parse_block(
        call_tool_text(client, "ssh_exec", {"session_id": sid, "command": "true"})
    ).get("command_id")
    # Wait for completion.
    parse_block(
        call_tool_text(
            client,
            "ssh_exec_output",
            {"command_id": cid, "wait": True, "wait_timeout_secs": 10},
            timeout=15,
        )
    )
    parsed = parse_block(
        call_tool_text(client, "ssh_exec_cancel", {"command_id": cid})
    )
    # Cancel after completion is idempotent — accepts NOOP / CANCELLED / completed status.
    ok = parsed.get("__status") in {"NOOP", "CANCELLED", "OK"} or (
        parsed.get("__status") == "ERROR"
        and "NOT_RUNNING" in (parsed.get("reason") or "")
    )
    _assert(
        stats,
        "cancel_completed_command_idempotent",
        ok,
        got=parsed.get("__status"),
        reason=parsed.get("reason"),
    )
    call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)


def _scenario_upload_local_missing(client: McpClient, stats: dict, target: ChaosSshTarget) -> None:
    sid = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            target.connect_args(agent_id="chaos-upload-missing", reuse="force_new"),
            timeout=15,
        )
    ).get("session_id")
    if not sid:
        _assert(stats, "upload_local_missing", False, got="connect failed")
        return
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_upload",
            {
                "session_id": sid,
                "local_path": f"/tmp/does-not-exist-{uuid.uuid4()}",
                "remote_path": "/tmp/x.bin",
            },
        )
    )
    transfer_id = parsed.get("transfer_id")
    if transfer_id:
        # Drift to the eventual FAILED snapshot.
        terminal = parse_block(
            call_tool_text(
                client,
                "ssh_transfer_progress",
                {"transfer_id": transfer_id, "wait": True, "wait_timeout_secs": 10},
                timeout=15,
            )
        )
        ok = terminal.get("__status") == "FAILED"
        _assert(stats, "upload_local_missing", ok, got=terminal.get("__status"))
    else:
        # Direct error path is also acceptable. v4.6+ uses more granular
        # codes: LOCAL_FILE_ERROR (fs::metadata failed) is preferred,
        # SFTP_ERROR remains as the untagged fallback.
        reason = parsed.get("reason") or ""
        ok = parsed.get("__status") == "ERROR" and (
            "SFTP_ERROR" in reason or "LOCAL_FILE_ERROR" in reason
        )
        _assert(stats, "upload_local_missing", ok, got=reason)
    call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)


def _scenario_upload_directory(client: McpClient, stats: dict, target: ChaosSshTarget) -> None:
    sid = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            target.connect_args(agent_id="chaos-upload-dir", reuse="force_new"),
            timeout=15,
        )
    ).get("session_id")
    if not sid:
        _assert(stats, "upload_directory_not_file", False, got="connect failed")
        return
    with tempfile.TemporaryDirectory() as td:
        parsed = parse_block(
            call_tool_text(
                client,
                "ssh_upload",
                {
                    "session_id": sid,
                    "local_path": td,  # a directory, not a file
                    "remote_path": "/tmp/xdir.bin",
                },
            )
        )
        transfer_id = parsed.get("transfer_id")
        if transfer_id:
            terminal = parse_block(
                call_tool_text(
                    client,
                    "ssh_transfer_progress",
                    {"transfer_id": transfer_id, "wait": True, "wait_timeout_secs": 10},
                    timeout=15,
                )
            )
            ok = terminal.get("__status") == "FAILED"
            _assert(stats, "upload_directory_not_file", ok, got=terminal.get("__status"))
        else:
            # v4.6+ live: LOCAL_NOT_FILE for the directory case.
            reason = parsed.get("reason") or ""
            ok = parsed.get("__status") == "ERROR" and (
                "SFTP_ERROR" in reason or "LOCAL_NOT_FILE" in reason
            )
            _assert(stats, "upload_directory_not_file", ok, got=reason)
    call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)


def _scenario_download_remote_missing(client: McpClient, stats: dict, target: ChaosSshTarget) -> None:
    sid = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            target.connect_args(agent_id="chaos-download-missing", reuse="force_new"),
            timeout=15,
        )
    ).get("session_id")
    if not sid:
        _assert(stats, "download_remote_missing", False, got="connect failed")
        return
    with tempfile.TemporaryDirectory() as td:
        parsed = parse_block(
            call_tool_text(
                client,
                "ssh_download",
                {
                    "session_id": sid,
                    "remote_path": f"/tmp/does-not-exist-{uuid.uuid4()}.bin",
                    "local_path": str(Path(td) / "out.bin"),
                },
            )
        )
        transfer_id = parsed.get("transfer_id")
        if transfer_id:
            terminal = parse_block(
                call_tool_text(
                    client,
                    "ssh_transfer_progress",
                    {"transfer_id": transfer_id, "wait": True, "wait_timeout_secs": 10},
                    timeout=15,
                )
            )
            ok = terminal.get("__status") == "FAILED"
            _assert(stats, "download_remote_missing", ok, got=terminal.get("__status"))
        else:
            # v4.6+ live: REMOTE_METADATA_ERROR for the missing-remote case.
            reason = parsed.get("reason") or ""
            ok = parsed.get("__status") == "ERROR" and (
                "SFTP_ERROR" in reason or "REMOTE_METADATA_ERROR" in reason
            )
            _assert(stats, "download_remote_missing", ok, got=reason)
    call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)


# ---------------------------------------------------------------------------
# Top-level orchestration — one stdio child per scenario
# ---------------------------------------------------------------------------


_SCENARIOS_NO_SSHD = [
    _scenario_connect_bad_host,
    _scenario_connect_bad_address_format,
    _scenario_execute_on_closed_session,
    _scenario_get_command_output_bad_id,
    _scenario_cancel_command_bad_id,
    _scenario_shell_write_on_closed_shell,
    _scenario_send_key_invalid_repeat_zero,
    _scenario_send_key_invalid_repeat_high,
    _scenario_wait_for_empty_patterns,
    _scenario_wait_for_too_many_patterns,
    _scenario_wait_for_pattern_too_long,
    _scenario_shell_read_on_closed,
    _scenario_upload_no_session,
    _scenario_download_no_session,
    _scenario_transfer_progress_bad_id,
    _scenario_disconnect_unknown_session,
    _scenario_disconnect_agent_unknown,
    _scenario_resource_read_invalid_uri,
    _scenario_resource_read_unknown_shell,
    _scenario_subscribe_invalid_uri,
]


_SCENARIOS_WITH_SSHD = [
    _scenario_connect_bad_credentials,
    _scenario_max_commands_exceeded,
    _scenario_max_shells_exceeded,
    _scenario_cancel_completed_command,
    _scenario_upload_local_missing,
    _scenario_upload_directory,
    _scenario_download_remote_missing,
]


def main() -> int:
    stats = {"tested": 0, "passed": 0, "failed": 0, "panics": 0, "skipped": 0}
    started = time.monotonic()

    for scenario in _SCENARIOS_NO_SSHD:
        with chaos_session() as (client, transport):
            try:
                scenario(client, stats)
            except Exception as exc:
                _assert(stats, scenario.__name__, False, got=f"{type(exc).__name__}: {exc}")
            time.sleep(0.05)
            if transport.panicked():
                _assert(
                    stats,
                    f"{scenario.__name__}_no_panic",
                    False,
                    panicked=True,
                    stderr=transport.stderr_text()[-200:],
                )

    target = ChaosSshTarget.from_env()
    fixture_owner = None
    if target is None:
        # Auto-fallback: spin up the in-process paramiko sshd so the
        # SSHD-touching scenarios run end-to-end without requiring
        # SSH_MCP_TEST_TARGET. The fixture owns its own port and lifetime.
        try:
            from helpers.local_sshd import LocalSshdFixture  # type: ignore
            fixture_owner = LocalSshdFixture()
            fixture_owner.__enter__()
            target = ChaosSshTarget(
                address=fixture_owner.address,
                username=fixture_owner.username,
                key_path=None,
                password=fixture_owner.password,
            )
        except Exception as exc:
            for scenario in _SCENARIOS_WITH_SSHD:
                stats["skipped"] += 1
                write_event({
                    "test": scenario.__name__,
                    "ok": True,
                    "skipped": f"local fixture failed: {exc}",
                })
            target = None
    if target is None:
        pass  # fallback failed; scenarios already marked skipped above
    else:
        for scenario in _SCENARIOS_WITH_SSHD:
            with chaos_session() as (client, transport):
                try:
                    scenario(client, stats, target)
                except Exception as exc:
                    _assert(
                        stats, scenario.__name__, False, got=f"{type(exc).__name__}: {exc}"
                    )
                time.sleep(0.1)
                if transport.panicked():
                    _assert(
                        stats,
                        f"{scenario.__name__}_no_panic",
                        False,
                        panicked=True,
                        stderr=transport.stderr_text()[-200:],
                    )

    summary = {
        "chaos_errors": "ok" if stats["failed"] == 0 and stats["panics"] == 0 else "fail",
        "tested": stats["tested"],
        "passed": stats["passed"],
        "failed": stats["failed"],
        "panics": stats["panics"],
        "skipped": stats["skipped"],
        "duration_s": round(time.monotonic() - started, 3),
        "status": "ok" if stats["failed"] == 0 and stats["panics"] == 0 else "fail",
    }
    if fixture_owner is not None:
        try:
            fixture_owner.__exit__(None, None, None)
        except Exception:
            pass
    return write_summary(summary)


if __name__ == "__main__":
    sys.exit(main())
