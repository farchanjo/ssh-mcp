"""Stress test: lock contention via rapid execute/cancel bursts.

10 sessions x 50 concurrent ssh_execute + ssh_cancel_command per second.
Runs for 5 minutes. Asserts no deadlock by issuing a final ``tools/list``
within 5 s after the workload ends.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from helpers.fixtures import HTTP_BIN, find_free_port, wait_for_port, SshTarget
from helpers.mcp_client import HttpTransport, McpClient, call_tool_text
from helpers.parse_block import parse_block


SESSIONS = 10
OPS_PER_SECOND = 50
DURATION_SECONDS = 300  # 5 minutes


def _spawn_server(port: int) -> subprocess.Popen:
    env = {**os.environ, "MCP_PORT": str(port), "MCP_HOST": "127.0.0.1", "RUST_LOG": "warn"}
    return subprocess.Popen([str(HTTP_BIN)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _worker(port: int, sid: str, stop_at: float, stats: dict) -> None:
    transport = HttpTransport(f"http://127.0.0.1:{port}")
    client = McpClient(transport)
    try:
        client.initialize()
        executed = 0
        cancelled = 0
        errors = 0
        while time.monotonic() < stop_at:
            for _ in range(OPS_PER_SECOND // SESSIONS):
                text = call_tool_text(
                    client, "ssh_execute", {"session_id": sid, "command": "sleep 30"}
                )
                cid = parse_block(text).get("command_id")
                executed += 1
                if not cid:
                    errors += 1
                    continue
                # Cancel almost immediately.
                cresp = parse_block(
                    call_tool_text(client, "ssh_cancel_command", {"command_id": cid})
                )
                if cresp.get("__status") == "CANCELLED":
                    cancelled += 1
            time.sleep(0.2)
        stats["executed"] = executed
        stats["cancelled"] = cancelled
        stats["errors"] = errors
    finally:
        client.close()


def main() -> int:
    if not HTTP_BIN.exists():
        print(json.dumps({"status": "skip", "reason": "http binary not built"}))
        return 0
    target = SshTarget.from_env()
    if target is None:
        print(json.dumps({"status": "skip", "reason": "SSH_MCP_TEST_TARGET unset"}))
        return 0

    port = find_free_port()
    proc = _spawn_server(port)
    try:
        if not wait_for_port("127.0.0.1", port, timeout=10.0):
            print(json.dumps({"status": "fail", "reason": "server failed to bind"}))
            return 1

        coordinator = McpClient(HttpTransport(f"http://127.0.0.1:{port}"))
        coordinator.initialize()

        sids: list[str] = []
        for i in range(SESSIONS):
            sid = parse_block(
                call_tool_text(coordinator, "ssh_connect", target.connect_args(agent_id="stress-locks", name=f"sess-{i}"))
            ).get("session_id")
            if sid:
                sids.append(sid)

        stop_at = time.monotonic() + DURATION_SECONDS
        worker_stats: list[dict] = []
        threads: list[threading.Thread] = []
        for sid in sids:
            stats = {"session_id": sid}
            worker_stats.append(stats)
            t = threading.Thread(target=_worker, args=(port, sid, stop_at, stats), daemon=True)
            t.start()
            threads.append(t)

        for t in threads:
            t.join(timeout=DURATION_SECONDS + 60)

        deadlock = any(t.is_alive() for t in threads)

        # Liveness probe within 5 s.
        live_start = time.monotonic()
        try:
            tools = coordinator.list_tools()
            live_ok = len(tools) == 18
        except Exception:
            live_ok = False
        live_elapsed = time.monotonic() - live_start

        for sid in sids:
            try:
                call_tool_text(coordinator, "ssh_disconnect", {"session_id": sid})
            except Exception:
                pass
        coordinator.close()

        summary = {
            "status": "ok" if not deadlock and live_ok and live_elapsed <= 5 else "fail",
            "deadlock": deadlock,
            "live_elapsed_s": live_elapsed,
            "live_ok": live_ok,
            "sessions": len(sids),
            "total_executed": sum(s.get("executed", 0) for s in worker_stats),
            "total_cancelled": sum(s.get("cancelled", 0) for s in worker_stats),
            "total_errors": sum(s.get("errors", 0) for s in worker_stats),
            "server_alive": proc.poll() is None,
        }
        print(json.dumps(summary))
        return 0 if summary["status"] == "ok" else 1
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
