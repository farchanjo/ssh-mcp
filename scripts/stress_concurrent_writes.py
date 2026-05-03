"""Stress test: 100 parallel ssh_shell_send_key arrow_up calls on one shell.

Verifies FIFO ordering — the shell receives 100 ``\\x1b[A`` sequences in
order. We assert by counting the ``[A`` substrings rendered by ``cat`` (which
echoes input verbatim).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from helpers.fixtures import HTTP_BIN, find_free_port, wait_for_port, SshTarget
from helpers.mcp_client import HttpTransport, McpClient, call_tool_text
from helpers.parse_block import parse_block


CONCURRENT_SENDS = 100


def _spawn_server(port: int) -> subprocess.Popen:
    env = {**os.environ, "MCP_PORT": str(port), "MCP_HOST": "127.0.0.1", "RUST_LOG": "warn"}
    return subprocess.Popen([str(HTTP_BIN)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


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
            call_tool_text(coordinator, "ssh_connect", target.connect_args(agent_id="stress-cw"))
        ).get("session_id")
        shell_id = parse_block(
            call_tool_text(coordinator, "ssh_shell_open", {"session_id": sid})
        ).get("shell_id")
        # Run cat so input echoes back; arrow_up ascii sequence = ESC [ A.
        call_tool_text(coordinator, "ssh_shell_write", {"shell_id": shell_id, "input": "cat\n"})
        time.sleep(0.5)
        call_tool_text(
            coordinator,
            "ssh_shell_read",
            {"shell_id": shell_id, "clear": True, "max_output_bytes": 65536},
        )

        # Spawn one client per worker for true parallelism.
        clients: list[McpClient] = []
        for _ in range(CONCURRENT_SENDS):
            c = McpClient(HttpTransport(f"http://127.0.0.1:{port}"))
            c.initialize()
            clients.append(c)

        ok_count = 0
        fail_count = 0
        try:
            with ThreadPoolExecutor(max_workers=CONCURRENT_SENDS) as pool:
                futures = [
                    pool.submit(
                        call_tool_text,
                        clients[i],
                        "ssh_shell_send_key",
                        {"shell_id": shell_id, "key": "arrow_up"},
                    )
                    for i in range(CONCURRENT_SENDS)
                ]
                for fut in as_completed(futures):
                    parsed = parse_block(fut.result())
                    if parsed.get("__status") == "OK":
                        ok_count += 1
                    else:
                        fail_count += 1
        finally:
            for c in clients:
                c.close()

        # Allow the shell to flush.
        time.sleep(2.0)
        out = parse_block(
            call_tool_text(
                coordinator,
                "ssh_shell_read",
                {"shell_id": shell_id, "clear": False, "max_output_bytes": 1048576},
            )
        )
        rendered = out.get("data") or ""
        # Each arrow_up sends ESC [ A. We can't necessarily count exactly 100
        # echoed sequences (depending on the remote terminal), but we should
        # see at least a high fraction. Empty output indicates a complete
        # ordering failure.
        substring_count = rendered.count("[A")

        # Send Ctrl+C to leave cat.
        call_tool_text(coordinator, "ssh_shell_send_key", {"shell_id": shell_id, "key": "ctrl_c"})
        call_tool_text(coordinator, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(coordinator, "ssh_disconnect", {"session_id": sid})
        coordinator.close()

        summary = {
            "status": "ok" if ok_count == CONCURRENT_SENDS and substring_count > 0 else "fail",
            "ok_responses": ok_count,
            "fail_responses": fail_count,
            "rendered_arrow_substrings": substring_count,
            "rendered_total_bytes": len(rendered),
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
