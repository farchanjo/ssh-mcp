"""Stress test: lock contention via rapid execute/cancel bursts.

10 sessions x 50 concurrent ssh_execute + ssh_cancel_command per second.
Runs for 5 minutes. Asserts no deadlock by issuing a final ``tools/list``
within 5 s after the workload ends.

Transport selection (v4.3): set ``STRESS_TRANSPORT`` to ``stdio`` (default)
or ``http``. In stdio mode all 10 worker threads share a single coordinator
child process (the use-case state lives per-process).
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

from helpers.fixtures import (
    HTTP_BIN,
    SshTarget,
    find_free_port,
    make_stress_client,
    make_stress_client_http,
    stress_transport_mode,
    wait_for_port,
)
from helpers.mcp_client import McpClient, call_tool_text
from helpers.parse_block import parse_block


SESSIONS = 10
OPS_PER_SECOND = 50
DURATION_SECONDS = 300  # 5 minutes


def _spawn_http_server(port: int) -> subprocess.Popen:
    env = {**os.environ, "MCP_PORT": str(port), "MCP_HOST": "127.0.0.1", "RUST_LOG": "warn"}
    return subprocess.Popen([str(HTTP_BIN)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _make_client(transport: str, port: int | None) -> McpClient:
    return make_stress_client_http(port) if transport == "http" else make_stress_client()


def _worker(client: McpClient, sid: str, stop_at: float, stats: dict) -> None:
    executed = 0
    cancelled = 0
    errors = 0
    while time.monotonic() < stop_at:
        for _ in range(OPS_PER_SECOND // SESSIONS):
            text = call_tool_text(
                client, "ssh_exec", {"session_id": sid, "command": "sleep 30"}
            )
            cid = parse_block(text).get("command_id")
            executed += 1
            if not cid:
                errors += 1
                continue
            # Cancel almost immediately.
            cresp = parse_block(
                call_tool_text(client, "ssh_exec_cancel", {"command_id": cid})
            )
            if cresp.get("__status") == "CANCELLED":
                cancelled += 1
        time.sleep(0.2)
    stats["executed"] = executed
    stats["cancelled"] = cancelled
    stats["errors"] = errors


def _run(transport: str) -> int:
    server_proc: subprocess.Popen | None = None
    http_port: int | None = None
    if transport == "http":
        if not HTTP_BIN.exists():
            print(json.dumps({"status": "skip", "reason": "http binary not built"}))
            return 0
        http_port = find_free_port()
        server_proc = _spawn_http_server(http_port)
        if not wait_for_port("127.0.0.1", http_port, timeout=10.0):
            print(json.dumps({"status": "fail", "reason": "server failed to bind"}))
            return 1

    target = SshTarget.from_env()
    fixture_owner = None
    if target is None:
        try:
            from helpers.local_sshd import LocalSshdFixture  # type: ignore
            fixture_owner = LocalSshdFixture()
            fixture_owner.__enter__()
            target = SshTarget(
                address=fixture_owner.address,
                username=fixture_owner.username,
                key_path=None,
                password=fixture_owner.password,
            )
        except Exception:
            print(json.dumps({"status": "skip", "reason": "no SSH target available"}))
            return 0

    try:
        coordinator = _make_client(transport, http_port)

        sids: list[str] = []
        for i in range(SESSIONS):
            sid = parse_block(
                call_tool_text(
                    coordinator,
                    "ssh_connect",
                    target.connect_args(agent_id="stress-locks", name=f"sess-{i}"),
                )
            ).get("session_id")
            if sid:
                sids.append(sid)

        # In stdio mode workers share the coordinator (single process).
        # In HTTP mode each worker spins its own client (matches v4.0).
        worker_clients: list[McpClient] = []
        if transport == "stdio":
            worker_clients = [coordinator] * len(sids)
        else:
            worker_clients = [_make_client(transport, http_port) for _ in sids]

        stop_at = time.monotonic() + DURATION_SECONDS
        worker_stats: list[dict] = []
        threads: list[threading.Thread] = []
        for i, sid in enumerate(sids):
            stats = {"session_id": sid}
            worker_stats.append(stats)
            t = threading.Thread(
                target=_worker, args=(worker_clients[i], sid, stop_at, stats), daemon=True
            )
            t.start()
            threads.append(t)

        for t in threads:
            t.join(timeout=DURATION_SECONDS + 60)

        deadlock = any(t.is_alive() for t in threads)

        # Liveness probe within 5 s.
        live_start = time.monotonic()
        try:
            tools = coordinator.list_tools()
            live_ok = len(tools) >= 17  # 18 with port_forward, 17 without.
        except Exception:
            live_ok = False
        live_elapsed = time.monotonic() - live_start

        for sid in sids:
            try:
                call_tool_text(coordinator, "ssh_disconnect", {"session_id": sid})
            except Exception:
                pass
        if transport == "http":
            for c in worker_clients:
                try:
                    c.close()
                except Exception:
                    pass
        coordinator.close()

        server_alive = (server_proc is None) or (server_proc.poll() is None)
        summary = {
            "status": "ok" if not deadlock and live_ok and live_elapsed <= 5 and server_alive else "fail",
            "transport": transport,
            "deadlock": deadlock,
            "live_elapsed_s": live_elapsed,
            "live_ok": live_ok,
            "sessions": len(sids),
            "total_executed": sum(s.get("executed", 0) for s in worker_stats),
            "total_cancelled": sum(s.get("cancelled", 0) for s in worker_stats),
            "total_errors": sum(s.get("errors", 0) for s in worker_stats),
            "server_alive": server_alive,
        }
        print(json.dumps(summary))
        return 0 if summary["status"] == "ok" else 1
    finally:
        if server_proc is not None:
            server_proc.terminate()
            try:
                server_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server_proc.kill()
        if fixture_owner is not None:
            try:
                fixture_owner.__exit__(None, None, None)
            except Exception:
                pass


def main() -> int:
    transport = stress_transport_mode()
    return _run(transport)


if __name__ == "__main__":
    sys.exit(main())
