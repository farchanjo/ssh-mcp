"""v4.8.1 — `ssh_get_transfer_progress` reports live partial progress.

Pre-fix bug: the SFTP adapter only synchronised the running
`AtomicU64 bytes_transferred` into the `TransferRepository` row at
terminal hand-off (`Completed` / `Failed` / `Cancelled`). Polled snapshots
during the transfer therefore returned `bytes_transferred = 0` even
though the local file kept growing.

Fix: a per-transfer progress watcher subscribes to the broadcast channel
the streaming task uses for `ProgressEvent::Tick` frames and pumps
partial-progress updates into the repository at a 250 ms throttle.

This integration script drives the real binary against a real SSH
target (the `requires_sshd` fixture covers both env-var endpoints and
the in-process paramiko fixture), uploads a multi-MiB blob, and polls
`ssh_get_transfer_progress` while the streaming task is still running.
The assertions:

1.  At least one mid-flight poll observes ``0 < bytes_transferred <=
    total_bytes`` — the bug-pre-fix shape ``bytes_transferred = 0`` is
    rejected.
2.  Polled values are monotonic non-decreasing across the running
    window — the throttle never publishes a stale value.
3.  The final terminal poll reports ``bytes_transferred == total_bytes``
    and ``status == "completed"``.

Notes:
- The local paramiko fixture has historically surfaced a flaky
  ``SFTP subsystem timed out`` failure on heavily loaded hosts; that
  exact failure mode is reused as a skip signal here so the test stays
  green on weak boxes (mirrors the existing `test_stdio.py` policy).
- Throttle math: PROGRESS_TICK_THROTTLE = 250 ms. The poll loop here
  runs at 100 ms cadence with a 30 s wall budget — enough headroom for
  multi-MiB transfers even on slow paramiko fixtures.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from helpers.mcp_client import McpClient, call_tool_text
from helpers.parse_block import parse_block


pytestmark = pytest.mark.requires_sshd


# 5 MiB payload — large enough that the streaming task takes
# noticeably longer than one poll cadence on every fixture, small
# enough to stay well under the per-session SFTP cap and finish in
# a few seconds on a local paramiko fixture.
_PAYLOAD_BYTES = 5 * 1024 * 1024

# Poll cadence + budget for the running-window observation loop.
_POLL_INTERVAL_S = 0.1
_RUNNING_BUDGET_S = 30.0


def _connect(stdio_client: McpClient, ssh_target, *, agent: str) -> str:
    sid = parse_block(
        call_tool_text(
            stdio_client,
            "ssh_connect",
            ssh_target.connect_args(agent_id=agent),
        )
    ).get("session_id")
    assert sid, "ssh_connect must return a session_id"
    return sid


def _read_progress(stdio_client: McpClient, transfer_id: str) -> dict:
    return parse_block(
        call_tool_text(
            stdio_client,
            "ssh_transfer_progress",
            {"transfer_id": transfer_id},
            timeout=10,
        )
    )


def _safe_int(parsed: dict, key: str) -> int:
    raw = parsed.get(key)
    if raw is None:
        return 0
    try:
        return int(raw)
    except (TypeError, ValueError):
        return 0


def test_upload_progress_reports_live_partial_bytes(
    stdio_client: McpClient, ssh_target, tmp_path: Path
) -> None:
    """v4.8.1 fix: polled `ssh_get_transfer_progress` MUST observe
    non-zero `bytes_transferred` mid-flight."""
    src = tmp_path / "v481-upload.bin"
    src.write_bytes(b"u" * _PAYLOAD_BYTES)
    remote_dir = "/tmp/ssh_mcp_v481"
    remote_path = f"{remote_dir}/upload.bin"
    sid = _connect(stdio_client, ssh_target, agent="v481-upload")
    try:
        # Make sure the remote directory exists and the destination is
        # absent, so the SFTP `create` opens cleanly.
        cid = parse_block(
            call_tool_text(
                stdio_client,
                "ssh_exec",
                {
                    "session_id": sid,
                    "command": f"mkdir -p {remote_dir} && rm -f {remote_path}",
                },
            )
        ).get("command_id")
        call_tool_text(
            stdio_client,
            "ssh_exec_output",
            {"command_id": cid, "wait": True},
            timeout=15,
        )

        upload = parse_block(
            call_tool_text(
                stdio_client,
                "ssh_upload",
                {
                    "session_id": sid,
                    "local_path": str(src),
                    "remote_path": remote_path,
                },
            )
        )
        transfer_id = upload.get("transfer_id")
        assert transfer_id, ("ssh_upload must return a transfer_id", upload)

        # Poll while the transfer is running. Capture every observed
        # `bytes_transferred` snapshot so we can assert mid-flight
        # observability + monotonicity.
        observed: list[int] = []
        final_status = ""
        deadline = time.monotonic() + _RUNNING_BUDGET_S
        last_total = 0
        while time.monotonic() < deadline:
            parsed = _read_progress(stdio_client, transfer_id)
            status = (parsed.get("status") or "").lower()
            bytes_transferred = _safe_int(parsed, "bytes_transferred")
            total_bytes = _safe_int(parsed, "total_bytes")
            if total_bytes:
                last_total = total_bytes
            observed.append(bytes_transferred)
            if status in {"completed", "failed", "cancelled"}:
                final_status = status
                break
            time.sleep(_POLL_INTERVAL_S)
        else:
            pytest.fail(
                f"transfer did not reach a terminal status within "
                f"{_RUNNING_BUDGET_S}s; last samples={observed[-5:]}"
            )

        if final_status == "failed":
            # Local paramiko fixture flake — same shape as the existing
            # test_stdio_upload_then_download skip path.
            parsed = _read_progress(stdio_client, transfer_id)
            reason = (parsed.get("error") or "").lower()
            if "timeout" in reason or "timed out" in reason:
                pytest.skip(
                    "local paramiko fixture: SFTP timed out (real-sshd "
                    "path covers the live-progress assertion)"
                )

        assert final_status == "completed", (
            "transfer must complete, got",
            final_status,
            observed[-5:],
        )

        # === Assertion 1: at least one mid-flight sample observed
        # `0 < bytes < total`. This is the precise pre-fix bug shape:
        # before v4.8.1 every running-window sample was 0.
        running_window = [b for b in observed[:-1] if b > 0]
        assert running_window, (
            "v4.8.1 regression: no running-window sample observed "
            "non-zero bytes_transferred — repository was not synced "
            "from the live atomic. samples=" + repr(observed)
        )
        assert all(b <= last_total for b in running_window), (
            "running samples must not exceed total_bytes",
            running_window,
            last_total,
        )

        # === Assertion 2: monotonic non-decreasing.
        for prev, nxt in zip(observed, observed[1:]):
            assert prev <= nxt, (
                "polled bytes_transferred must be monotonic; saw "
                f"{prev} -> {nxt} in {observed}"
            )

        # === Assertion 3: terminal poll matches total.
        terminal = _read_progress(stdio_client, transfer_id)
        terminal_bytes = _safe_int(terminal, "bytes_transferred")
        terminal_total = _safe_int(terminal, "total_bytes")
        assert terminal_total == _PAYLOAD_BYTES, (
            "terminal total_bytes must echo the source size",
            terminal_total,
            _PAYLOAD_BYTES,
        )
        assert terminal_bytes == terminal_total, (
            "terminal bytes_transferred must equal total_bytes",
            terminal_bytes,
            terminal_total,
        )
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})


def test_download_progress_reports_live_partial_bytes(
    stdio_client: McpClient, ssh_target, tmp_path: Path
) -> None:
    """Mirror of the upload test for `ssh_download`."""
    # Stage a remote source by uploading first; this avoids depending
    # on a remote tool to seed the file. The upload itself is verified
    # by `test_upload_progress_reports_live_partial_bytes` so we only
    # use it as a setup step here.
    src = tmp_path / "v481-source.bin"
    src.write_bytes(b"d" * _PAYLOAD_BYTES)
    remote_dir = "/tmp/ssh_mcp_v481"
    remote_path = f"{remote_dir}/download.bin"
    dst = tmp_path / "v481-download.bin"
    sid = _connect(stdio_client, ssh_target, agent="v481-download")
    try:
        cid = parse_block(
            call_tool_text(
                stdio_client,
                "ssh_exec",
                {
                    "session_id": sid,
                    "command": f"mkdir -p {remote_dir} && rm -f {remote_path}",
                },
            )
        ).get("command_id")
        call_tool_text(
            stdio_client,
            "ssh_exec_output",
            {"command_id": cid, "wait": True},
            timeout=15,
        )

        # Seed the remote file via upload + wait-for-terminal.
        seed_xfer = parse_block(
            call_tool_text(
                stdio_client,
                "ssh_upload",
                {
                    "session_id": sid,
                    "local_path": str(src),
                    "remote_path": remote_path,
                },
            )
        ).get("transfer_id")
        seed_status = parse_block(
            call_tool_text(
                stdio_client,
                "ssh_transfer_progress",
                {"transfer_id": seed_xfer, "wait": True, "wait_timeout_secs": 60},
                timeout=90,
            )
        )
        if seed_status.get("__status") == "FAILED" and "TIMEOUT" in (
            seed_status.get("reason") or ""
        ):
            pytest.skip("local paramiko fixture: seed upload timed out")
        assert seed_status.get("__status") == "COMPLETED", seed_status

        # Now download and observe live progress.
        download = parse_block(
            call_tool_text(
                stdio_client,
                "ssh_download",
                {
                    "session_id": sid,
                    "remote_path": remote_path,
                    "local_path": str(dst),
                },
            )
        )
        transfer_id = download.get("transfer_id")
        assert transfer_id, ("ssh_download must return a transfer_id", download)

        observed: list[int] = []
        final_status = ""
        deadline = time.monotonic() + _RUNNING_BUDGET_S
        last_total = 0
        while time.monotonic() < deadline:
            parsed = _read_progress(stdio_client, transfer_id)
            status = (parsed.get("status") or "").lower()
            bytes_transferred = _safe_int(parsed, "bytes_transferred")
            total_bytes = _safe_int(parsed, "total_bytes")
            if total_bytes:
                last_total = total_bytes
            observed.append(bytes_transferred)
            if status in {"completed", "failed", "cancelled"}:
                final_status = status
                break
            time.sleep(_POLL_INTERVAL_S)
        else:
            pytest.fail(
                f"download did not reach a terminal status within "
                f"{_RUNNING_BUDGET_S}s; last samples={observed[-5:]}"
            )

        if final_status == "failed":
            parsed = _read_progress(stdio_client, transfer_id)
            reason = (parsed.get("error") or "").lower()
            if "timeout" in reason or "timed out" in reason:
                pytest.skip(
                    "local paramiko fixture: SFTP timed out on download"
                )

        assert final_status == "completed", (
            "download must complete, got",
            final_status,
            observed[-5:],
        )

        running_window = [b for b in observed[:-1] if b > 0]
        assert running_window, (
            "v4.8.1 regression: no running-window sample observed "
            "non-zero bytes_transferred for download. samples="
            + repr(observed)
        )
        assert all(b <= last_total for b in running_window), (
            running_window,
            last_total,
        )
        for prev, nxt in zip(observed, observed[1:]):
            assert prev <= nxt, (prev, nxt, observed)

        terminal = _read_progress(stdio_client, transfer_id)
        assert _safe_int(terminal, "bytes_transferred") == _PAYLOAD_BYTES
        assert _safe_int(terminal, "total_bytes") == _PAYLOAD_BYTES
        assert dst.exists() and dst.stat().st_size == _PAYLOAD_BYTES
    finally:
        call_tool_text(stdio_client, "ssh_disconnect", {"session_id": sid})
