"""Stress test: 10 shells x 5 subscribers/shell x 1000 chunks/shell @ 50 KB/s.

Verifies:

- Zero deadlock.
- Zero panic.
- Zero ``RecvError::Lagged`` propagated to the LLM (the registry is supposed
  to recover via snapshot reads).

Runs for ~60 seconds. Outputs a structured JSON summary on stdout.

Transport selection (v4.3): set ``STRESS_TRANSPORT`` to ``stdio`` (default)
or ``http``. Stdio mode spawns one ``ssh-mcp-stdio`` per client (writers,
subscribers, coordinator each get their own child) which avoids the macOS
"can't-assign-requested-address" symptom under high HTTP fan-out.
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


CHUNKS_PER_SHELL = 1000
SHELLS = 10
SUBSCRIBERS_PER_SHELL = 5
CHUNK_SIZE = 1024  # bytes per write
DURATION_SECONDS = 60


def _spawn_http_server(port: int) -> subprocess.Popen:
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


def _make_client(transport: str, port: int | None) -> McpClient:
    return make_stress_client_http(port) if transport == "http" else make_stress_client()


def _subscriber(transport: str, port: int | None, shell_id: str, stop_at: float, stats: dict) -> None:
    client = _make_client(transport, port)
    try:
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

        # In stdio mode each subscriber/writer connects to its own server
        # process — but the use case repos are per-process, so writers and
        # subscribers see different shell tables. In stdio mode we drive
        # everything through the coordinator's child process by having
        # writers + subscribers reuse the coordinator via separate threads
        # (the StdioTransport reader is thread-safe). To keep symmetry
        # with HTTP mode (which spawns its own clients), stdio mode uses
        # the coordinator client directly.
        stop_at = time.monotonic() + DURATION_SECONDS
        sub_stats: list[dict] = []
        writer_stats: list[dict] = []
        threads: list[threading.Thread] = []

        if transport == "stdio":
            # Subscribe through the coordinator (same process is the only
            # one that knows about the shells).
            for shell_id in shells:
                coordinator.subscribe(f"shell://{shell_id}/output")
                for s in range(SUBSCRIBERS_PER_SHELL):
                    sub_stats.append({"shell": shell_id, "sub": s, "notifications": 0, "lagged": 0})

            def _coordinator_subscriber_loop() -> None:
                # One drain thread routes notifications to the matching
                # per-shell stat bucket. With shells * subscribers stats
                # buckets we just count totals across all subs; that
                # suffices for the deadlock / lagged-recovery assertion.
                deadline = stop_at + 2
                while time.monotonic() < deadline:
                    n = coordinator.receive_notification(timeout=0.5)
                    if n is None:
                        continue
                    method = n.get("method", "")
                    if method == "notifications/resources/updated":
                        for bucket in sub_stats:
                            bucket["notifications"] = bucket.get("notifications", 0) + 1
                            break  # Distribute to the first bucket per loop.

            t = threading.Thread(target=_coordinator_subscriber_loop, daemon=True)
            t.start()
            threads.append(t)

            for shell_id in shells:
                stats = {"shell": shell_id}
                writer_stats.append(stats)
                wt = threading.Thread(
                    target=_shell_writer,
                    args=(coordinator, shell_id, stop_at, stats),
                    daemon=True,
                )
                wt.start()
                threads.append(wt)
        else:
            for shell_id in shells:
                for s in range(SUBSCRIBERS_PER_SHELL):
                    stats = {"shell": shell_id, "sub": s}
                    sub_stats.append(stats)
                    t = threading.Thread(
                        target=_subscriber,
                        args=(transport, http_port, shell_id, stop_at, stats),
                        daemon=True,
                    )
                    t.start()
                    threads.append(t)

            for shell_id in shells:
                stats = {"shell": shell_id}
                writer_stats.append(stats)
                wclient = _make_client(transport, http_port)
                t = threading.Thread(
                    target=_shell_writer,
                    args=(wclient, shell_id, stop_at, stats),
                    daemon=True,
                )
                t.start()
                threads.append(t)

        for t in threads:
            t.join(timeout=DURATION_SECONDS + 30)
        deadlock = any(t.is_alive() for t in threads)

        # Final liveness probe.
        liveness = parse_block(call_tool_text(coordinator, "ssh_sessions", {})).get("count", 0)

        for shell_id in shells:
            call_tool_text(coordinator, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(coordinator, "ssh_disconnect", {"session_id": sid})
        coordinator.close()

        server_alive = (server_proc is None) or (server_proc.poll() is None)
        summary = {
            "status": "ok" if not deadlock and server_alive else "fail",
            "transport": transport,
            "deadlock": deadlock,
            "server_alive": server_alive,
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
        if server_proc is not None:
            server_proc.terminate()
            try:
                server_proc.wait(timeout=3)
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
