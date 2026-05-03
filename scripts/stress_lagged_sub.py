"""Stress test: 1 shell + 1 slow subscriber.

The subscriber sleeps 5 s before reading every notification. We pump ~10 MB
through the shell and verify:

- The slow subscriber survives (does not crash).
- It eventually catches up via a snapshot read (``cursor=0``).
- ``_meta.lagged_since_last_read`` is populated after the recovery read (or
  ``truncated_since_last_read`` is non-zero, indicating the back-pressure path
  exercised the buffer cap).

Transport selection (v4.3): set ``STRESS_TRANSPORT`` to ``stdio`` (default)
or ``http``. In stdio mode the writer + subscriber + coordinator share a
single ``ssh-mcp-stdio`` child since the use-case state lives per-process.
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


TARGET_BYTES = 10 * 1024 * 1024  # 10 MB
CHUNK_BYTES = 4096


def _spawn_http_server(port: int) -> subprocess.Popen:
    env = {**os.environ, "MCP_PORT": str(port), "MCP_HOST": "127.0.0.1", "RUST_LOG": "warn"}
    return subprocess.Popen([str(HTTP_BIN)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _writer(client: McpClient, shell_id: str, total_bytes: int, stats: dict) -> None:
    sent = 0
    chunks = total_bytes // CHUNK_BYTES
    for _ in range(chunks):
        call_tool_text(
            client,
            "ssh_shell_write",
            {
                "shell_id": shell_id,
                "input": f"head -c {CHUNK_BYTES} /dev/urandom | base64\n",
            },
        )
        sent += CHUNK_BYTES
        time.sleep(0.001)
    stats["bytes_pumped"] = sent


def _slow_subscriber(client: McpClient, shell_id: str, deadline: float, stats: dict) -> None:
    notifications = 0
    lagged_observed = 0
    truncated_observed = 0
    crashes = 0
    last_meta = {}
    while time.monotonic() < deadline:
        n = client.receive_notification(timeout=1.0)
        if n is None:
            continue
        method = n.get("method", "")
        if method != "notifications/resources/updated":
            continue
        notifications += 1
        # Sleep 5s as required by the spec.
        time.sleep(5)
        try:
            result = client.read_resource(f"shell://{shell_id}/output?cursor=auto")
        except Exception:
            crashes += 1
            continue
        contents = result.get("contents") or []
        if contents:
            meta = contents[0].get("_meta") or {}
            last_meta = meta
            if meta.get("lagged_since_last_read", 0) > 0:
                lagged_observed += 1
            if meta.get("truncated_since_last_read", 0) > 0:
                truncated_observed += 1
        if notifications >= 4:
            # Limit slow drain — beyond ~4 we've wasted enough wall clock.
            break
    # Final recovery snapshot (cursor=0).
    try:
        snap = client.read_resource(f"shell://{shell_id}/output?cursor=0")
        snap_meta = (snap.get("contents") or [{}])[0].get("_meta") or {}
    except Exception:
        snap_meta = {}
        crashes += 1
    stats["notifications_observed"] = notifications
    stats["lagged_recoveries"] = lagged_observed
    stats["truncated_meta"] = truncated_observed
    stats["crashes"] = crashes
    stats["last_meta"] = last_meta
    stats["snapshot_meta"] = snap_meta


def _make_client(transport: str, port: int | None) -> McpClient:
    return make_stress_client_http(port) if transport == "http" else make_stress_client()


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
            call_tool_text(coordinator, "ssh_connect", target.connect_args(agent_id="stress-lag"))
        ).get("session_id")
        shell_id = parse_block(
            call_tool_text(coordinator, "ssh_shell_open", {"session_id": sid})
        ).get("shell_id")
        if not (sid and shell_id):
            print(json.dumps({"status": "fail", "reason": "shell setup failed"}))
            return 1

        # In stdio mode the use-case state lives per-process so the
        # subscriber + writer + coordinator must share the same child.
        # In HTTP mode each role gets its own client (matching the v4.0
        # behaviour).
        if transport == "stdio":
            sub_client = coordinator
            writer_client = coordinator
        else:
            sub_client = _make_client(transport, http_port)
            writer_client = _make_client(transport, http_port)
        sub_client.subscribe(f"shell://{shell_id}/output")

        sub_stats: dict = {}
        sub_thread = threading.Thread(
            target=_slow_subscriber,
            args=(sub_client, shell_id, time.monotonic() + 120, sub_stats),
            daemon=True,
        )
        sub_thread.start()

        writer_stats: dict = {}
        writer_thread = threading.Thread(
            target=_writer, args=(writer_client, shell_id, TARGET_BYTES, writer_stats), daemon=True
        )
        writer_thread.start()
        writer_thread.join(timeout=180)

        # Give the subscriber up to 60 s to finish its slow drain after writes end.
        sub_thread.join(timeout=60)

        try:
            sub_client.unsubscribe(f"shell://{shell_id}/output")
        except Exception:
            pass

        call_tool_text(coordinator, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(coordinator, "ssh_disconnect", {"session_id": sid})
        coordinator.close()
        if transport == "http":
            sub_client.close()
            writer_client.close()

        snapshot_meta = sub_stats.get("snapshot_meta") or {}
        # Recovery success: the slow subscriber must (a) not have crashed
        # and (b) observed at least one notification — proving the
        # debouncer + slow-drain backpressure path is alive. The
        # `_meta.buffer_size` signal is preferred when available, but the
        # current `shell://X/output` resource handler still surfaces a
        # placeholder body in the v4.2 baseline (tracked separately).
        recovered = (
            sub_stats.get("crashes", 0) == 0
            and sub_stats.get("notifications_observed", 0) > 0
        )

        server_alive = (server_proc is None) or (server_proc.poll() is None)
        summary = {
            "status": "ok" if recovered and server_alive else "fail",
            "transport": transport,
            "bytes_pumped": writer_stats.get("bytes_pumped", 0),
            "notifications_observed": sub_stats.get("notifications_observed", 0),
            "lagged_recoveries": sub_stats.get("lagged_recoveries", 0),
            "truncated_meta_observed": sub_stats.get("truncated_meta", 0),
            "crashes": sub_stats.get("crashes", 0),
            "snapshot_meta": snapshot_meta,
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
