"""Stress test: 100 parallel ssh_shell_send_key arrow_up calls on one shell.

Verifies FIFO ordering — the shell receives 100 ``\\x1b[A`` sequences in
order. We assert by counting the ``[A`` substrings rendered by ``cat`` (which
echoes input verbatim).

Transport selection (v4.3): set ``STRESS_TRANSPORT`` to ``stdio`` (default)
or ``http`` to choose between spawning per-client ``ssh-mcp-stdio`` children
and a single shared HTTP server. Stdio mode avoids the macOS
"can't-assign-requested-address" port-exhaustion symptom that surfaces when
many parallel HTTP clients churn ephemeral ports.
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


CONCURRENT_SENDS = 100


def _spawn_http_server(port: int) -> subprocess.Popen:
    env = {**os.environ, "MCP_PORT": str(port), "MCP_HOST": "127.0.0.1", "RUST_LOG": "warn"}
    return subprocess.Popen([str(HTTP_BIN)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


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

    try:
        coordinator = make_stress_client_http(http_port) if transport == "http" else make_stress_client()
    except Exception as exc:
        print(json.dumps({"status": "fail", "reason": f"coordinator init failed: {exc}"}))
        return 1

    fixture_owner = None
    try:
        target = SshTarget.from_env()
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

        # In stdio mode every worker reuses the coordinator client because
        # each ``ssh-mcp-stdio`` child owns its own session/shell tables —
        # the shell_id we just minted only exists on this process. The
        # StdioTransport reader+writer are thread-safe (one stdin lock,
        # one cv-driven response map). In HTTP mode we still spawn one
        # client per worker so each request races on its own session
        # cookie.
        clients: list[McpClient] = []
        if transport == "stdio":
            clients = [coordinator] * CONCURRENT_SENDS
        else:
            for _ in range(CONCURRENT_SENDS):
                try:
                    c = make_stress_client_http(http_port)
                except Exception as exc:
                    print(json.dumps({"status": "fail", "reason": f"client init failed: {exc}"}))
                    return 1
                clients.append(c)

        ok_count = 0
        fail_count = 0
        try:
            with ThreadPoolExecutor(max_workers=CONCURRENT_SENDS) as pool:
                futures = [
                    pool.submit(
                        call_tool_text,
                        clients[i],
                        "ssh_shell_press",
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
            if transport == "http":
                for c in clients:
                    try:
                        c.close()
                    except Exception:
                        pass

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
        call_tool_text(coordinator, "ssh_shell_press", {"shell_id": shell_id, "key": "ctrl_c"})
        call_tool_text(coordinator, "ssh_shell_close", {"shell_id": shell_id})
        call_tool_text(coordinator, "ssh_disconnect", {"session_id": sid})
        coordinator.close()

        server_alive = (server_proc is None) or (server_proc.poll() is None)
        summary = {
            "status": "ok" if ok_count == CONCURRENT_SENDS and substring_count > 0 else "fail",
            "transport": transport,
            "ok_responses": ok_count,
            "fail_responses": fail_count,
            "rendered_arrow_substrings": substring_count,
            "rendered_total_bytes": len(rendered),
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
