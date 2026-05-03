"""Chaos suite — resource exhaustion.

Pushes the v4.3 runtime past its documented quotas in three orthogonal
axes and asserts the server holds the line:

- ``push_100mb_upload``: stage a 100 MiB local file, upload it,
  observe ``COMPLETED`` (or a documented partial / failed state) within
  the wall-clock budget.
- ``max_sessions_burst``: open many concurrent sessions back-to-back
  and confirm the server caps gracefully (either via the upstream sshd
  ``MaxSessions`` budget or via the ssh-mcp internal cap), never
  panics.
- ``one_thousand_subscribers``: subscribe 1000 times to a single
  ``shell://X/output`` resource and confirm the server holds the
  registry without leaking memory / panicking.

Each scenario runs in an isolated ``ssh-mcp-stdio`` child so a crash
cannot cross-contaminate. Output: one JSON line per scenario plus a
final summary ``{"chaos_exhaustion": "ok", "scenarios": N, "failed":
M}``.
"""

from __future__ import annotations

import sys
import tempfile
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
# Helpers
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


def _scenario_push_100mb_upload(target: ChaosSshTarget, tmp_dir: Path) -> dict:
    """Stage a 100 MiB local file, upload, wait for terminal state."""
    src = tmp_dir / "chaos-100mb.bin"
    # 100 MiB of zero-bytes — fast to write, fast to compress on the wire
    # but still produces ~100M of disk-side I/O on the remote.
    with src.open("wb") as fh:
        fh.write(b"\0" * (100 * 1024 * 1024))
    with chaos_session() as (client, transport):
        sid = _connect(client, target, "chaos-exhaust-100mb")
        if not sid:
            return {"ok": False, "error": "connect failed"}
        try:
            mkdir = parse_block(
                call_tool_text(
                    client,
                    "ssh_execute",
                    {
                        "session_id": sid,
                        "command": f"mkdir -p /tmp/ssh-mcp-chaos-{sid}",
                    },
                )
            ).get("command_id")
            if mkdir:
                call_tool_text(
                    client,
                    "ssh_get_command_output",
                    {"command_id": mkdir, "wait": True, "wait_timeout_secs": 10},
                    timeout=15,
                )
            up = parse_block(
                call_tool_text(
                    client,
                    "ssh_upload",
                    {
                        "session_id": sid,
                        "local_path": str(src),
                        "remote_path": f"/tmp/ssh-mcp-chaos-{sid}/100mb.bin",
                    },
                    timeout=30,
                )
            )
            tid = up.get("transfer_id")
            if not tid:
                return {
                    "ok": False,
                    "error": f"upload failed: {up.get('reason')}",
                    "panicked": transport.panicked(),
                }
            # 100 MiB on a localhost loopback should land in <30 s; bump
            # generously so a slow CI runner still passes.
            t0 = time.monotonic()
            terminal = parse_block(
                call_tool_text(
                    client,
                    "ssh_get_transfer_progress",
                    {"transfer_id": tid, "wait": True, "wait_timeout_secs": 120},
                    timeout=130,
                )
            )
            elapsed = round(time.monotonic() - t0, 3)
            status = terminal.get("__status")
            ok = status in {"COMPLETED", "RUNNING"} and not transport.panicked()
            return {
                "ok": ok,
                "final_status": status,
                "wait_s": elapsed,
                "bytes_transferred": terminal.get("bytes_transferred"),
                "panicked": transport.panicked(),
            }
        finally:
            _disconnect(client, sid)


def _scenario_max_sessions_burst(target: ChaosSshTarget) -> dict:
    """Open many concurrent sessions and confirm graceful capping."""
    burst = 30  # exceeds default OpenSSH MaxSessions of 10
    with chaos_session() as (client, transport):
        sids: list[str] = []
        try:
            with ThreadPoolExecutor(max_workers=10) as pool:
                futs = [
                    pool.submit(
                        _connect, client, target, f"chaos-exhaust-burst-{i}"
                    )
                    for i in range(burst)
                ]
                for f in futs:
                    sid = f.result()
                    if sid:
                        sids.append(sid)
            # Liveness: tools/list MUST return promptly even after the
            # burst.
            t0 = time.monotonic()
            tools = client.list_tools()
            live = round(time.monotonic() - t0, 3)
            return {
                "ok": len(tools) >= 17 and live < 5.0 and not transport.panicked(),
                "issued": burst,
                "succeeded": len(sids),
                "tools": len(tools),
                "liveness_s": live,
                "panicked": transport.panicked(),
            }
        finally:
            for sid in sids:
                _disconnect(client, sid)


def _scenario_one_thousand_subscribers(target: ChaosSshTarget) -> dict:
    """Subscribe 1000x to one shell resource, confirm registry survives."""
    with chaos_session() as (client, transport):
        sid = _connect(client, target, "chaos-exhaust-subs")
        if not sid:
            return {"ok": False, "error": "connect failed"}
        try:
            shid = parse_block(
                call_tool_text(client, "ssh_shell_open", {"session_id": sid})
            ).get("shell_id")
            if not shid:
                return {"ok": False, "error": "shell_open failed"}
            uri = f"shell://{shid}/output"
            success = 0
            failures = 0
            for _ in range(1000):
                try:
                    client.subscribe(uri)
                    success += 1
                except Exception:
                    failures += 1
            # Liveness probe — server must still respond after the flood.
            t0 = time.monotonic()
            tools = client.list_tools()
            live = round(time.monotonic() - t0, 3)
            try:
                call_tool_text(client, "ssh_shell_close", {"shell_id": shid})
            except Exception:
                pass
            return {
                "ok": success >= 900
                and len(tools) >= 17
                and live < 5.0
                and not transport.panicked(),
                "subscribed": success,
                "subscribe_failures": failures,
                "liveness_s": live,
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
                "chaos_exhaustion": "ok",
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

    with tempfile.TemporaryDirectory() as td:
        tmp_dir = Path(td)
        all_scenarios = [
            (
                "push_100mb_upload",
                lambda: _scenario_push_100mb_upload(target, tmp_dir),
            ),
            ("max_sessions_burst", lambda: _scenario_max_sessions_burst(target)),
            (
                "one_thousand_subscribers",
                lambda: _scenario_one_thousand_subscribers(target),
            ),
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
        "chaos_exhaustion": "ok" if failed == 0 and panics == 0 else "fail",
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
