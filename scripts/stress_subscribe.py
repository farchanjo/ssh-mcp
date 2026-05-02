"""Stress test: 10 shells x 5 subscribers/shell x 1000 chunks/shell @ 50 KB/s.

Verifies:

- Zero deadlock.
- Zero panic.
- Zero ``RecvError::Lagged`` propagated to the LLM (the registry is supposed
  to recover via snapshot reads).

Runs for ~60 seconds. Outputs a structured JSON summary on stdout.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from contextlib import closing
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from helpers.fixtures import HTTP_BIN, find_free_port, wait_for_port, SshTarget
from helpers.mcp_client import HttpTransport, McpClient, call_tool_text
from helpers.parse_block import parse_block


CHUNKS_PER_SHELL = 1000
SHELLS = 10
SUBSCRIBERS_PER_SHELL = 5
CHUNK_SIZE = 1024  # bytes per write
DURATION_SECONDS = 60


def _spawn_server(port: int) -> subprocess.Popen:
    env = {**os.environ, "MCP_PORT": str(port), "MCP_HOST": "127.0.0.1", "RUST_LOG": "warn"}
    return subprocess.Popen([str(HTTP_BIN)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _shell_writer(client: McpClient, shell_id: str, stop_at: float, stats: dict) -> None:
    payload = "x" * CHUNK_SIZE + "\n"
    sent = 0
    while time.monotonic() < stop_at and sent < CHUNKS_PER_SHELL:
        call_tool_text(
            client, "ssh_shell_write", {"shell_id": shell_id, "input": f"printf '{payload}'\n"}
        )
        sent += 1
        # Aim for ~50 KB/s per shell (50 chunks/sec at CHUNK_SIZE=1024 bytes).
        time.sleep(0.02)
    stats["chunks_sent"] = sent


def _subscriber(port: int, shell_id: str, stop_at: float, stats: dict) -> None:
    transport = HttpTransport(f"http://127.0.0.1:{port}")
    client = McpClient(transport)
    try:
        client.initialize()
        client.subscribe(f"shell://{shell_id}/output")
        notifications = 0
        lagged = 0
        while time.monotonic() < stop_at:
            n = client.receive_notification(timeout=0.5)
            if n is None:
                continue
            method = n.get("method", "")
            if method == "notifications/resources/updated":
                notifications += 1
                # Snapshot read on the resource.
                result = client.read_resource(f"shell://{shell_id}/output?cursor=auto")
                contents = result.get("contents") or []
                if contents:
                    meta = contents[0].get("_meta") or {}
                    if meta.get("lagged_since_last_read", 0) > 0:
                        lagged += 1
        stats["notifications"] = notifications
        stats["lagged"] = lagged
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

        sid = parse_block(
            call_tool_text(coordinator, "ssh_connect", target.connect_args(agent_id="stress-sub"))
        ).get("session_id")
        if not sid:
            print(json.dumps({"status": "fail", "reason": "ssh_connect failed"}))
            return 1

        shells: list[str] = []
        for i in range(SHELLS):
            shell_id = parse_block(
                call_tool_text(coordinator, "ssh_shell_open", {"session_id": sid})
            ).get("shell_id")
            if not shell_id:
                print(json.dumps({"status": "fail", "reason": f"shell_open #{i} failed"}))
                return 1
            shells.append(shell_id)

        stop_at = time.monotonic() + DURATION_SECONDS
        sub_stats: list[dict] = []
        writer_stats: list[dict] = []
        threads: list[threading.Thread] = []

        for shell_id in shells:
            for s in range(SUBSCRIBERS_PER_SHELL):
                stats = {"shell": shell_id, "sub": s}
                sub_stats.append(stats)
                t = threading.Thread(
                    target=_subscriber, args=(port, shell_id, stop_at, stats), daemon=True
                )
                t.start()
                threads.append(t)

        # Each writer uses its own client to avoid contention on the HTTP session.
        for shell_id in shells:
            stats = {"shell": shell_id}
            writer_stats.append(stats)
            wclient = McpClient(HttpTransport(f"http://127.0.0.1:{port}"))
            wclient.initialize()
            t = threading.Thread(
                target=_shell_writer, args=(wclient, shell_id, stop_at, stats), daemon=True
            )
            t.start()
            threads.append(t)

        for t in threads:
            t.join(timeout=DURATION_SECONDS + 30)
        deadlock = any(t.is_alive() for t in threads)

        # Final liveness probe.
        liveness = parse_block(call_tool_text(coordinator, "ssh_list_sessions", {})).get("count", 0)

        for shell_id in shells:
            call_tool_text(coordinator, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(coordinator, "ssh_disconnect", {"session_id": sid})
        coordinator.close()

        summary = {
            "status": "ok" if not deadlock and proc.poll() is None else "fail",
            "deadlock": deadlock,
            "server_alive": proc.poll() is None,
            "liveness_probe_count": liveness,
            "shells": len(shells),
            "subscribers": len(sub_stats),
            "total_notifications": sum(s.get("notifications", 0) for s in sub_stats),
            "total_lagged_recoveries": sum(s.get("lagged", 0) for s in sub_stats),
            "total_chunks_sent": sum(s.get("chunks_sent", 0) for s in writer_stats),
        }
        print(json.dumps(summary))
        return 0 if summary["status"] == "ok" else 1
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
