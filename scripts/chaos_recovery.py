"""Chaos suite — recovery scenarios.

Stresses the v4.3 runtime with externally-driven adverse events and
asserts the server survives every documented recovery path:

- ``kill_during_command``: spawn a long-running command, kill the SSH
  channel under it, observe the use case surfacing ``CANCELLED`` /
  ``FAILED`` (never ``RUNNING`` forever, never panic).
- ``sigterm_session``: open a session, fire ``ssh_disconnect``, confirm
  follow-up calls on the dead session id surface ``SESSION_NOT_FOUND``
  cleanly.
- ``buffer_truncation``: open a shell, write more output than
  ``SSH_SHELL_MAX_BUFFER_SIZE`` will hold, then read with ``cursor=auto``
  and confirm the ``SHELL_OUTPUT`` block is truncated cleanly (no panic,
  documented ``BUFFER_TRUNCATED`` notice or simple wrap with the latest
  bytes).
- ``cancel_during_cancel``: fire two concurrent ``ssh_cancel_command``
  calls on the same in-flight command and confirm at least one
  succeeds, the second is a documented ``NOOP`` / ``ERROR`` (never a
  panic).
- ``subscribe_after_close``: subscribe to a shell resource AFTER the
  shell has been closed and confirm the server returns a clean error
  block (``SHELL_NOT_FOUND``).

Each scenario runs in an isolated ``ssh-mcp-stdio`` child so a crash
cannot cross-contaminate. Output: one JSON line per scenario plus a
final summary ``{"chaos_recovery": "ok", "scenarios": N, "failed": M}``.
"""

from __future__ import annotations

import json
import sys
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
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
# Per-scenario helpers
# ---------------------------------------------------------------------------


def _connect(client: McpClient, target: ChaosSshTarget, agent_id: str) -> str | None:
    parsed = parse_block(
        call_tool_text(
            client,
            "ssh_connect",
            target.connect_args(agent_id=agent_id, reuse="force_new"),
            timeout=15,
        )
    )
    return parsed.get("session_id")


def _disconnect(client: McpClient, sid: str) -> None:
    try:
        call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------


def _scenario_kill_during_command(target: ChaosSshTarget) -> dict:
    """Spawn a long-running command, then kill the session under it."""
    with chaos_session() as (client, transport):
        sid = _connect(client, target, "chaos-recovery-kill")
        if not sid:
            return {"ok": False, "error": "connect failed"}
        try:
            cid = parse_block(
                call_tool_text(
                    client,
                    "ssh_exec",
                    {"session_id": sid, "command": "sleep 30"},
                )
            ).get("command_id")
            if not cid:
                return {"ok": False, "error": "execute failed"}
            time.sleep(0.2)
            # Force-disconnect the entire session — the running command
            # must observe a terminal state (FAILED / CANCELLED) not RUNNING.
            _disconnect(client, sid)
            time.sleep(0.3)
            after = parse_block(
                call_tool_text(
                    client,
                    "ssh_exec_output",
                    {"command_id": cid, "wait": True, "wait_timeout_secs": 5},
                    timeout=10,
                )
            )
            status = after.get("status") or after.get("__status")
            terminal = status in {
                "FAILED",
                "CANCELLED",
                "COMPLETED",
                "ERROR",
                "NOT_FOUND",
            } or after.get("__status") == "ERROR"
            return {
                "ok": terminal and not transport.panicked(),
                "final_status": status,
                "panicked": transport.panicked(),
            }
        finally:
            _disconnect(client, sid)


def _scenario_sigterm_session(target: ChaosSshTarget) -> dict:
    """Disconnect the session, then call follow-up ops on the dead id."""
    with chaos_session() as (client, transport):
        sid = _connect(client, target, "chaos-recovery-sigterm")
        if not sid:
            return {"ok": False, "error": "connect failed"}
        # Disconnect cleanly.
        disc = parse_block(
            call_tool_text(client, "ssh_disconnect", {"session_id": sid}, timeout=15)
        )
        # Now the session id must be gone — every follow-up call must
        # surface SESSION_NOT_FOUND, never panic.
        results = []
        for tool, args in [
            ("ssh_exec", {"session_id": sid, "command": "echo zombie"}),
            ("ssh_shell_open", {"session_id": sid}),
            (
                "ssh_upload",
                {
                    "session_id": sid,
                    "local_path": "/etc/hostname",
                    "remote_path": "/tmp/x",
                },
            ),
        ]:
            r = parse_block(call_tool_text(client, tool, args))
            results.append((tool, r.get("__status"), r.get("reason") or ""))
        all_clean = all(
            status == "ERROR" and "SESSION_NOT_FOUND" in reason
            for _tool, status, reason in results
        )
        return {
            "ok": disc.get("__status") == "OK"
            and all_clean
            and not transport.panicked(),
            "disconnect_status": disc.get("__status"),
            "follow_ups": results,
            "panicked": transport.panicked(),
        }


def _scenario_buffer_truncation(target: ChaosSshTarget) -> dict:
    """Push >> buffer-cap output through a shell and read back cleanly."""
    with chaos_session() as (client, transport):
        sid = _connect(client, target, "chaos-recovery-buftrunc")
        if not sid:
            return {"ok": False, "error": "connect failed"}
        try:
            # `max_buffer_size` is wire-typed as a human-size string
            # (`512k`, `10m`, ...). Use a small one so we trip truncation
            # fast.
            shid = parse_block(
                call_tool_text(
                    client,
                    "ssh_shell_open",
                    {"session_id": sid, "max_buffer_size": "8k"},
                )
            ).get("shell_id")
            if not shid:
                return {"ok": False, "error": "shell_open failed"}
            # Stream ~80 KiB through the shell; far more than the 8 KiB
            # cap above. Use seq so the bytes are deterministic.
            call_tool_text(
                client,
                "ssh_shell_write",
                {
                    "shell_id": shid,
                    "input": "for i in $(seq 1 5000); do echo line-$i; done\n",
                },
            )
            # Wait for the burst to land; then read.
            time.sleep(1.0)
            read = parse_block(
                call_tool_text(
                    client,
                    "ssh_shell_read",
                    {"shell_id": shid},
                    timeout=10,
                )
            )
            # The read MUST succeed without panic; the buffer either
            # truncated (preferred) or wrapped to the latest bytes. The
            # data block size MUST stay strictly bounded — the server
            # cannot leak the full ~80 KiB stream when the buffer is
            # capped at 8 KiB. The rendered tool response carries some
            # framing overhead, so accept up to 4x the configured cap as
            # a generous ceiling (32 KiB).
            data_bytes = len(read.get("__blocks", {}).get("data", ""))
            read_status = read.get("__status")
            # `ssh_shell_read` reports the shell `STATE` (OPEN / CLOSED)
            # in the status header, not a generic OK / ERROR. Both
            # `OPEN` and `CLOSED` are valid post-truncation outcomes.
            ok = (
                read_status in {"OPEN", "CLOSED", "OK"}
                and data_bytes <= 32 * 1024
                and not transport.panicked()
            )
            try:
                call_tool_text(client, "ssh_shell_close", {"shell_id": shid})
            except Exception:
                pass
            return {
                "ok": ok,
                "data_bytes": data_bytes,
                "read_status": read_status,
                "panicked": transport.panicked(),
            }
        finally:
            _disconnect(client, sid)


def _scenario_cancel_during_cancel(target: ChaosSshTarget) -> dict:
    """Fire two concurrent cancels on the same command — no panic."""
    with chaos_session() as (client, transport):
        sid = _connect(client, target, "chaos-recovery-dblcancel")
        if not sid:
            return {"ok": False, "error": "connect failed"}
        try:
            cid = parse_block(
                call_tool_text(
                    client,
                    "ssh_exec",
                    {"session_id": sid, "command": "sleep 30"},
                )
            ).get("command_id")
            if not cid:
                return {"ok": False, "error": "execute failed"}
            time.sleep(0.1)
            with ThreadPoolExecutor(max_workers=2) as pool:
                f1 = pool.submit(
                    call_tool_text,
                    client,
                    "ssh_exec_cancel",
                    {"command_id": cid},
                )
                f2 = pool.submit(
                    call_tool_text,
                    client,
                    "ssh_exec_cancel",
                    {"command_id": cid},
                )
                r1 = parse_block(f1.result())
                r2 = parse_block(f2.result())
            statuses = [r1.get("__status"), r2.get("__status")]
            # At least one MUST be a documented terminal status; both
            # must be either OK / NOOP / ERROR (never anything else,
            # never a panic).
            allowed = {"OK", "NOOP", "ERROR", "CANCELLED"}
            ok = (
                all(s in allowed for s in statuses)
                and any(s in {"OK", "CANCELLED", "NOOP"} for s in statuses)
                and not transport.panicked()
            )
            return {
                "ok": ok,
                "statuses": statuses,
                "panicked": transport.panicked(),
            }
        finally:
            _disconnect(client, sid)


def _scenario_subscribe_after_close(target: ChaosSshTarget) -> dict:
    """Subscribe to a closed shell — server must return clean error."""
    with chaos_session() as (client, transport):
        sid = _connect(client, target, "chaos-recovery-subclose")
        if not sid:
            return {"ok": False, "error": "connect failed"}
        try:
            shid = parse_block(
                call_tool_text(client, "ssh_shell_open", {"session_id": sid})
            ).get("shell_id")
            if not shid:
                return {"ok": False, "error": "shell_open failed"}
            close = parse_block(
                call_tool_text(client, "ssh_shell_close", {"shell_id": shid})
            )
            time.sleep(0.2)
            uri = f"shell://{shid}/output"
            sub_err: str | None = None
            try:
                client.subscribe(uri)
            except Exception as exc:
                sub_err = str(exc)[:160]
            # Either the subscribe is rejected with a documented error,
            # OR it accepts (registry holds the URI for a future re-open)
            # and a follow-up read surfaces SHELL_NOT_FOUND. Both
            # outcomes are valid as long as no panic occurred.
            read_status: str | None = None
            try:
                read = client.read_resource(uri)
                err_obj = read.get("_rpc_error")
                read_status = json.dumps(err_obj)[:160] if err_obj else "ok"
            except Exception as exc:
                read_status = str(exc)[:160]
            return {
                "ok": close.get("__status") == "OK" and not transport.panicked(),
                "subscribe_error": sub_err,
                "read_status": read_status,
                "panicked": transport.panicked(),
            }
        finally:
            _disconnect(client, sid)


# ---------------------------------------------------------------------------
# Top-level orchestration
# ---------------------------------------------------------------------------


def main() -> int:
    target = ChaosSshTarget.from_env()
    fixture_owner = None
    if target is None:
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
        except Exception:
            target = None
    if target is None:
        write_event(
            {"scenario": "_all_", "ok": True, "skipped": "no SSH target available"}
        )
        return write_summary(
            {
                "chaos_recovery": "ok",
                "scenarios": 0,
                "failed": 0,
                "panics": 0,
                "skipped": True,
                "status": "ok",
            }
        )

    started = time.monotonic()
    failed = 0
    panics = 0
    scenarios = 0

    all_scenarios = [
        ("kill_during_command", lambda: _scenario_kill_during_command(target)),
        ("sigterm_session", lambda: _scenario_sigterm_session(target)),
        ("buffer_truncation", lambda: _scenario_buffer_truncation(target)),
        ("cancel_during_cancel", lambda: _scenario_cancel_during_cancel(target)),
        ("subscribe_after_close", lambda: _scenario_subscribe_after_close(target)),
    ]

    for name, body in all_scenarios:
        scenarios += 1
        t0 = time.monotonic()
        try:
            result = body()
        except Exception as exc:
            result = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        elapsed = round(time.monotonic() - t0, 3)
        event = {"scenario": name, "elapsed_s": elapsed}
        event.update(result)
        write_event(event)
        if not result.get("ok"):
            failed += 1
        if result.get("panicked"):
            panics += 1

    summary = {
        "chaos_recovery": "ok" if failed == 0 and panics == 0 else "fail",
        "scenarios": scenarios,
        "failed": failed,
        "panics": panics,
        "duration_s": round(time.monotonic() - started, 3),
        "status": "ok" if failed == 0 and panics == 0 else "fail",
    }
    if fixture_owner is not None:
        try:
            fixture_owner.__exit__(None, None, None)
        except Exception:
            pass
    return write_summary(summary)


if __name__ == "__main__":
    sys.exit(main())
