"""Stdio-transport coverage for the v7.0 ``ssh_rsync*`` MCP surface.

Mirrors :mod:`test_v7_rsync_http` but drives ``ssh-mcp-stdio`` over the
line-delimited JSON-RPC channel. The wire shape is identical so we only
re-run the highest-signal cases — the HTTP suite covers the long tail.

Cases here:

1. tools/list pins the three new tools.
2. SESSION_NOT_FOUND surfaces with the expected wire code.
3. SFTP transport pushes 3 files byte-identical against the local sshd.
4. Wire transport against a paramiko fixture surfaces a categorical
   error (RSYNC_NOT_FOUND / RSYNC_VERSION_TOO_OLD / RSYNC_PROTOCOL_ERROR)
   OR cleanly STARTS — the slice-3 wire client is still landing.
5. Progress lane emits ``notifications/resources/updated`` events.
6. Cancel returns OK and is idempotent on a second call.
7. Stats returns the canonical counter shape.
"""

from __future__ import annotations

import hashlib
import os
import time
from pathlib import Path

import pytest

from helpers.fixtures import SshTarget
from helpers.mcp_client import McpClient, call_tool_pair, call_tool_text
from helpers.parse_block import parse_block
from helpers.rsync_client import RsyncTestClient, preserve_all_off


pytestmark = pytest.mark.requires_sshd


def _connect(client: McpClient, target: SshTarget, *, agent: str) -> str:
    text = call_tool_text(client, "ssh_connect", target.connect_args(agent_id=agent))
    sid = parse_block(text).get("session_id")
    assert sid, f"ssh_connect did not return a session_id: {text!r}"
    return sid


def _disconnect(client: McpClient, sid: str) -> None:
    try:
        call_tool_text(client, "ssh_disconnect", {"session_id": sid})
    except Exception:
        pass


def _make_tree(root: Path, files: dict[str, bytes]) -> None:
    root.mkdir(parents=True, exist_ok=True)
    for rel, blob in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(blob)


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    h.update(path.read_bytes())
    return h.hexdigest()


def _wait_for_files(dst: Path, expected: set[str], *, timeout: float = 15.0) -> set[str]:
    deadline = time.monotonic() + timeout
    present: set[str] = set()
    while time.monotonic() < deadline:
        present = {p.name for p in dst.iterdir()} if dst.exists() else set()
        if expected.issubset(present):
            return present
        time.sleep(0.2)
    return present


# ---------------------------------------------------------------------------
# 1. tools/list pins the three new tools
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stdio_lists_three_new_tools(stdio_client: McpClient) -> None:
    tools = stdio_client.list_tools()
    names = {t["name"] for t in tools}
    for tool in ("ssh_rsync", "ssh_rsync_cancel", "ssh_rsync_stats"):
        assert tool in names, f"{tool} missing in stdio tools/list: {sorted(names)}"


# ---------------------------------------------------------------------------
# 2. SESSION_NOT_FOUND
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stdio_no_session_returns_session_not_found(stdio_client: McpClient) -> None:
    text, structured, _ = call_tool_pair(
        stdio_client,
        "ssh_rsync",
        {
            "session_id": "ghost-stdio",
            "src": "/tmp/x",
            "dst": "/tmp/y",
            "transport": "sftp",
            "opts": {"recursive": True, "preserve": preserve_all_off()},
        },
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR", text
    assert structured is not None
    assert structured.get("code") == "SESSION_NOT_FOUND", structured


# ---------------------------------------------------------------------------
# 3. SFTP transport pushes 3 files byte-identical
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stdio_sftp_byte_identical(
    stdio_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {f"f{i}.bin": os.urandom(1024 + i * 11) for i in range(3)}
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(stdio_client, ssh_target, agent="rsync-stdio-sftp")
    rs = RsyncTestClient(stdio_client, sid)
    try:
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        present = _wait_for_files(dst, set(files), timeout=15.0)
        assert set(files).issubset(present), present

        for rel in files:
            assert _sha256(src / rel) == _sha256(dst / rel), f"sha256 mismatch on {rel}"
    finally:
        _disconnect(stdio_client, sid)


# ---------------------------------------------------------------------------
# 4. Wire transport against paramiko fixture surfaces a known error
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stdio_wire_against_paramiko_fixture(
    stdio_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    _make_tree(src, {"a.txt": b"wire-stdio-probe"})
    dst.mkdir()

    sid = _connect(stdio_client, ssh_target, agent="rsync-stdio-wire")
    rs = RsyncTestClient(stdio_client, sid)
    try:
        text, structured, _ = rs.start_rsync(
            src=str(src), dst=str(dst), transport="wire", timeout=30.0
        )
        parsed = parse_block(text)
        if parsed.get("__status") == "ERROR":
            assert structured is not None
            assert structured.get("code") in {
                "RSYNC_VERSION_TOO_OLD",
                "RSYNC_NOT_FOUND",
                "RSYNC_PROTOCOL_ERROR",
            }, structured
        elif parsed.get("__status") == "STARTED":
            rsync_id = parsed.get("rsync_id")
            assert rsync_id, text
        else:
            pytest.fail(f"unexpected status for wire path: {parsed.get('__status')!r}: {text!r}")
    finally:
        _disconnect(stdio_client, sid)


# ---------------------------------------------------------------------------
# 5. Progress lane emits notifications
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stdio_progress_lane_emits_notifications(
    stdio_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {f"f{i}.bin": os.urandom(2048) for i in range(3)}
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(stdio_client, ssh_target, agent="rsync-stdio-lane")
    rs = RsyncTestClient(stdio_client, sid)
    try:
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        rs.subscribe_progress(rsync_id)
        snapshots = rs.drain_progress(rsync_id, timeout=10.0)
        assert snapshots, "no rsync progress notifications observed within 10s"

        first = snapshots[0]
        assert first["rsync_id"] == rsync_id, first
        assert "files_total" in first and "files_done" in first, first

        # Files must have landed on disk.
        present = _wait_for_files(dst, set(files), timeout=10.0)
        assert set(files).issubset(present), present
    finally:
        _disconnect(stdio_client, sid)


# ---------------------------------------------------------------------------
# 6. Cancel returns OK and is idempotent
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stdio_cancel_returns_ok(
    stdio_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    _make_tree(src, {"a.bin": os.urandom(64 * 1024)})
    dst.mkdir()

    sid = _connect(stdio_client, ssh_target, agent="rsync-stdio-cancel")
    rs = RsyncTestClient(stdio_client, sid)
    try:
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        ctxt, cstr, _ = rs.cancel(rsync_id)
        cparsed = parse_block(ctxt)
        assert cparsed.get("__status") == "OK", ctxt
        assert cstr is not None and cstr.get("status") == "ok", cstr

        ctxt2, cstr2, _ = rs.cancel(rsync_id)
        cparsed2 = parse_block(ctxt2)
        if cparsed2.get("__status") == "OK":
            assert cstr2 is not None and cstr2.get("status") == "ok"
        else:
            assert cstr2 is not None
            assert cstr2.get("code") in {"RSYNC_NOT_FOUND", "RESOURCE_GONE"}, cstr2
    finally:
        _disconnect(stdio_client, sid)


# ---------------------------------------------------------------------------
# 7. Stats returns canonical counters
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stdio_stats_returns_canonical_counters(
    stdio_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {f"f{i}.bin": os.urandom(2048) for i in range(2)}
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(stdio_client, ssh_target, agent="rsync-stdio-stats")
    rs = RsyncTestClient(stdio_client, sid)
    try:
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        # Drive bytes through to disk.
        _wait_for_files(dst, set(files), timeout=15.0)

        stxt, sstr, _ = rs.stats(rsync_id)
        sparsed = parse_block(stxt)
        assert sparsed.get("__status") == "OK", stxt
        assert sparsed.get("rsync_id") == rsync_id, sparsed
        for field in (
            "files_total",
            "files_done",
            "files_deleted",
            "files_failed",
            "bytes_total",
            "bytes_transferred",
            "bytes_skipped",
        ):
            assert field in sparsed, (field, sparsed)
        assert sstr is not None
        assert sstr.get("status") == "ok", sstr
    finally:
        _disconnect(stdio_client, sid)
