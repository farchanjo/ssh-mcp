"""HTTP transport coverage for the v7.0 ``ssh_rsync*`` MCP surface.

ADR 0011 deltas verified by this suite:

- Tools/list now exposes three additional tools:
  ``ssh_rsync`` / ``ssh_rsync_cancel`` / ``ssh_rsync_stats``.
- ``ssh_rsync`` returns a STARTED block carrying ``RSYNC_ID``,
  ``TRANSPORT``, ``FROM`` / ``TO`` and a HINT/NEXT advisory pair.
- ``ssh_rsync_cancel`` is idempotent on missing / terminal sessions.
- ``ssh_rsync_stats`` snapshots the live aggregate counters.
- ``rsync://<RSYNC_ID>/progress`` is a push lane that fires
  ``notifications/resources/updated`` events; reading the URI returns
  a JSON snapshot of the same counters.

The local sshd fixture (paramiko) is used for the SFTP transport tests.
The Wire transport against a real ``rsync 3.x`` is exercised by the
sibling :mod:`test_v7_rsync_vm` suite — the local paramiko fixture has
no ``rsync --server`` available, so wire-transport tests against it
must xfail with ``RSYNC_NOT_FOUND`` / ``RSYNC_VERSION_TOO_OLD``.

Notes on assertions:

- ``files_total`` / ``files_done`` counters often stay at ``0`` on the
  v7.0.0-alpha.4 SFTP path even after the bytes have landed (the SFTP
  transport does not yet update the domain aggregate's atomic
  counters from its background pump). Tests assert on the destination
  filesystem state rather than counters.
- ``status`` similarly can stay at ``pending`` on the SFTP path. The
  lane fires notifications, but the snapshot status field is not
  driven through to ``completed`` yet. Tests treat non-terminal status
  as acceptable when the destination FS shows the expected files.
"""

from __future__ import annotations

import hashlib
import os
import time
from pathlib import Path

import pytest

from helpers.fixtures import SshTarget
from helpers.mcp_client import McpClient, call_tool_pair, call_tool_text, extract_structured
from helpers.parse_block import parse_block
from helpers.rsync_client import RsyncTestClient, preserve_all_off


pytestmark = pytest.mark.requires_sshd


# ---------------------------------------------------------------------------
# Tiny local helpers — kept here (not in a shared module) so the test
# file reads as a self-contained narrative.
# ---------------------------------------------------------------------------


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
    """Materialise ``files`` (relative-path -> content) under ``root``."""
    root.mkdir(parents=True, exist_ok=True)
    for rel, blob in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(blob)


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    h.update(path.read_bytes())
    return h.hexdigest()


def _wait_terminal_or_files(
    rs: RsyncTestClient,
    rsync_id: str,
    dst: Path,
    expected_files: set[str],
    *,
    timeout: float = 15.0,
) -> str:
    """Drive the SFTP-path completion check.

    Polls ``ssh_rsync_stats`` AND the destination filesystem until either
    the snapshot status flips to a terminal value OR every expected
    file has shown up on disk. Returns the final observed status — which
    may still be ``pending`` on the v7.0.0-alpha.4 SFTP path even when
    every file has landed (see the file's module docstring).
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        snap = rs.read_snapshot(rsync_id) or {}
        status = (snap.get("status") or "").lower()
        if status in {"completed", "failed", "cancelled"}:
            return status
        present = {p.name for p in dst.iterdir()} if dst.exists() else set()
        if expected_files.issubset(present):
            return status or "pending"
        time.sleep(0.2)
    return (rs.read_snapshot(rsync_id) or {}).get("status", "") or ""


# ---------------------------------------------------------------------------
# 1. tools/list pins the three new tools
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_initial_lists_three_new_tools(http_client: McpClient) -> None:
    """ADR 0011 phase 11 — the new surface MUST be advertised verbatim."""
    tools = http_client.list_tools()
    by_name = {t["name"]: t for t in tools}
    for name in ("ssh_rsync", "ssh_rsync_cancel", "ssh_rsync_stats"):
        assert name in by_name, f"{name} missing from tools/list: {sorted(by_name)}"

    # `ssh_rsync` schema must require session_id, src, dst.
    schema = by_name["ssh_rsync"].get("inputSchema") or by_name["ssh_rsync"].get("input_schema")
    assert schema, by_name["ssh_rsync"]
    required = set(schema.get("required") or [])
    for field in ("session_id", "src", "dst"):
        assert field in required, (field, required)

    # Cancel + Stats schemas need rsync_id only.
    for name in ("ssh_rsync_cancel", "ssh_rsync_stats"):
        schema = by_name[name].get("inputSchema") or by_name[name].get("input_schema")
        assert schema, name
        assert "rsync_id" in (schema.get("required") or []), (name, schema)


# ---------------------------------------------------------------------------
# 2. SESSION_NOT_FOUND surfaces with the expected wire code
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_no_session_returns_session_not_found(http_client: McpClient) -> None:
    """Bogus session id -> SSH_RSYNC: ERROR with code SESSION_NOT_FOUND."""
    text, structured, _ = call_tool_pair(
        http_client,
        "ssh_rsync",
        {
            "session_id": "ghost-session-id",
            "src": "/tmp/x",
            "dst": "/tmp/y",
            "transport": "sftp",
            "opts": {"recursive": True, "preserve": preserve_all_off()},
        },
    )
    parsed = parse_block(text)
    assert parsed.get("__status") == "ERROR", text
    assert "SESSION_NOT_FOUND" in (parsed.get("reason") or text), text
    assert structured is not None
    assert structured.get("status") == "error"
    assert structured.get("code") == "SESSION_NOT_FOUND", structured


# ---------------------------------------------------------------------------
# 3. dry-run path (SFTP) emits STARTED + DRY_RUN and leaves dst empty
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
@pytest.mark.xfail(
    reason="v7.0.0-alpha.8 SFTP transport bug: opts.dry_run=true is ignored "
    "by the SFTP path; bytes are still written to the destination. The "
    "STARTED block correctly carries DRY_RUN: true and the structured "
    "twin reports dry_run=true, so the inbound DTO -> domain hop is OK; "
    "the SFTP transport's writer just doesn't honour the flag yet.",
    strict=False,
)
def test_rsync_dry_run_against_local_sshd(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    """``opts.dry_run=true`` must NOT touch the destination tree."""
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {f"file{i}.txt": f"dry-run-{i}".encode() * 100 for i in range(3)}
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-dryrun")
    rs = RsyncTestClient(http_client, sid)
    try:
        text, structured, _ = rs.start_rsync(
            src=str(src), dst=str(dst), transport="sftp", dry_run=True
        )
        parsed = parse_block(text)
        assert parsed.get("__status") == "STARTED", text
        assert parsed.get("dry_run") == "true", parsed
        assert structured is not None
        assert structured.get("dry_run") is True, structured
        rsync_id = parsed.get("rsync_id")
        assert rsync_id, text

        # Settle the background task; on dry-run there should be no
        # filesystem mutation regardless of whether the snapshot is
        # terminal.
        time.sleep(2.0)
        leftover = list(dst.iterdir())
        assert leftover == [], f"dry-run wrote to dst: {leftover}"
    finally:
        _disconnect(http_client, sid)


# ---------------------------------------------------------------------------
# 4. SFTP transport — recursive sync of 3 files, byte-identical
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_sftp_transport_against_local_sshd(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    """SFTP transport mirror: every file lands byte-identical."""
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {f"f{i}.bin": os.urandom(4096 + i * 13) for i in range(3)}
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-sftp")
    rs = RsyncTestClient(http_client, sid)
    try:
        text, structured, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        parsed = parse_block(text)
        assert parsed.get("__status") == "STARTED", text
        assert parsed.get("transport") == "sftp", parsed
        rsync_id = parsed.get("rsync_id")
        assert rsync_id, text

        _wait_terminal_or_files(rs, rsync_id, dst, set(files), timeout=15.0)

        for rel in files:
            sp = src / rel
            dp = dst / rel
            assert dp.exists(), f"missing on dst: {rel}"
            assert _sha256(sp) == _sha256(dp), f"sha256 mismatch on {rel}"
    finally:
        _disconnect(http_client, sid)


# ---------------------------------------------------------------------------
# 5. Wire transport — local paramiko has no rsync; expect graceful error
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_wire_transport_against_local_sshd(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    """Forced ``transport=wire`` against a paramiko fixture must surface
    a categorical wire error: either ``RSYNC_VERSION_TOO_OLD`` (probe
    classified the host) or ``RSYNC_PROTOCOL_ERROR`` (probe found rsync
    but the in-progress wire client could not handshake yet)."""
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    _make_tree(src, {"a.txt": b"wire-probe"})
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-wire")
    rs = RsyncTestClient(http_client, sid)
    try:
        text, structured, _ = rs.start_rsync(
            src=str(src), dst=str(dst), transport="wire", timeout=30.0
        )
        parsed = parse_block(text)
        # Two acceptable outcomes — both are categorical errors.
        if parsed.get("__status") == "ERROR":
            assert structured is not None
            code = structured.get("code", "")
            assert code in {
                "RSYNC_VERSION_TOO_OLD",
                "RSYNC_NOT_FOUND",
                "RSYNC_PROTOCOL_ERROR",
            }, structured
        elif parsed.get("__status") == "STARTED":
            # Wire path actually started — accept and let it run; the
            # paramiko fixture sometimes has rsync on PATH, in which
            # case slice-3 wire client kicks in. We do NOT assert
            # completion here (the wire client is still landing).
            rsync_id = parsed.get("rsync_id")
            assert rsync_id, text
            time.sleep(1.0)
        else:
            pytest.fail(f"unexpected status for wire path: {parsed.get('__status')!r}: {text!r}")
    finally:
        _disconnect(http_client, sid)


# ---------------------------------------------------------------------------
# 6. progress lane fires notifications during a recursive sync
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_progress_lane_emits_notifications(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    """Subscribed lane MUST receive ``notifications/resources/updated``
    while the SFTP transport drives the transfer."""
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {f"f{i}.bin": os.urandom(2048 + i * 7) for i in range(4)}
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-lane")
    rs = RsyncTestClient(http_client, sid)
    try:
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        rs.subscribe_progress(rsync_id)
        snapshots = rs.drain_progress(rsync_id, timeout=10.0)
        assert snapshots, "no rsync progress notifications observed within 10s"

        # Every snapshot is a JSON object with at least the canonical fields.
        first = snapshots[0]
        for key in (
            "rsync_id",
            "status",
            "files_total",
            "files_done",
            "bytes_total",
            "bytes_transferred",
        ):
            assert key in first, (key, first)
        assert first["rsync_id"] == rsync_id, first

        # Files must have landed on disk regardless of snapshot counters.
        for rel in files:
            assert (dst / rel).exists(), f"missing dst file: {rel}"
    finally:
        _disconnect(http_client, sid)


# ---------------------------------------------------------------------------
# 7. cancel is idempotent and emits OK
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_cancel_returns_ok(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    """``ssh_rsync_cancel`` MUST return OK and be safe to call twice.

    NOTE: race between the SFTP background task finishing and the cancel
    request landing means the snapshot may report ``completed`` instead
    of ``cancelled``; the contract is that cancel never errors AND
    that idempotent re-calls also do not error.
    """
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    _make_tree(src, {f"f{i}.bin": os.urandom(64 * 1024) for i in range(2)})
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-cancel")
    rs = RsyncTestClient(http_client, sid)
    try:
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        # First cancel — must always be OK.
        ctxt, cstr, _ = rs.cancel(rsync_id)
        cparsed = parse_block(ctxt)
        assert cparsed.get("__status") == "OK", ctxt
        assert cstr is not None and cstr.get("status") == "ok", cstr
        assert cstr.get("rsync_id") == rsync_id, cstr

        # Second cancel — idempotent; must also surface OK or a
        # well-formed RSYNC_NOT_FOUND error (server may have already
        # garbage-collected the session).
        ctxt2, cstr2, _ = rs.cancel(rsync_id)
        cparsed2 = parse_block(ctxt2)
        if cparsed2.get("__status") == "OK":
            assert cstr2 is not None and cstr2.get("status") == "ok"
        else:
            assert cstr2 is not None
            assert cstr2.get("code") in {"RSYNC_NOT_FOUND", "RESOURCE_GONE"}, cstr2
    finally:
        _disconnect(http_client, sid)


# ---------------------------------------------------------------------------
# 8. stats returns the canonical counter shape
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
def test_rsync_stats_returns_canonical_counters(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    """``ssh_rsync_stats`` returns OK + every counter field."""
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {f"f{i}.bin": os.urandom(4096) for i in range(3)}
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-stats")
    rs = RsyncTestClient(http_client, sid)
    try:
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        # Drive the bytes through to disk before snapshotting.
        _wait_terminal_or_files(rs, rsync_id, dst, set(files), timeout=15.0)

        stxt, sstr, _ = rs.stats(rsync_id)
        sp = parse_block(stxt)
        assert sp.get("__status") == "OK", stxt
        assert sp.get("rsync_id") == rsync_id, sp
        for field in (
            "files_total",
            "files_done",
            "files_deleted",
            "files_failed",
            "bytes_total",
            "bytes_transferred",
            "bytes_skipped",
        ):
            assert field in sp, (field, sp)

        assert sstr is not None
        assert sstr.get("status") == "ok", sstr
        assert sstr.get("rsync_id") == rsync_id, sstr
        for field in (
            "files_total",
            "files_done",
            "bytes_total",
            "bytes_transferred",
            "bytes_skipped",
            "session_status",
        ):
            assert field in sstr, (field, sstr)
    finally:
        _disconnect(http_client, sid)


# ---------------------------------------------------------------------------
# 9. exclude pattern keeps matching files off the destination
# ---------------------------------------------------------------------------


@pytest.mark.timeout(60)
@pytest.mark.xfail(
    reason="v7.0.0-alpha.8 SFTP transport bug: opts.exclude patterns are "
    "not enforced by the SFTP comparator; every source file lands on the "
    "destination regardless of pattern. The pattern reaches the use case "
    "(opts.exclude is wired through RsyncOptsArg) but the SFTP-side walk "
    "does not consult it.",
    strict=False,
)
def test_rsync_exclude_pattern_skips_matching_files(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files = {
        "keep1.txt": b"keep1",
        "keep2.txt": b"keep2",
        "drop.tmp": b"drop-me",
        "also_drop.tmp": b"drop-me-too",
    }
    _make_tree(src, files)
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-exclude")
    rs = RsyncTestClient(http_client, sid)
    try:
        text, _, _ = rs.start_rsync(
            src=str(src), dst=str(dst), transport="sftp", exclude=["*.tmp"]
        )
        rsync_id = parse_block(text).get("rsync_id")
        assert rsync_id, text

        _wait_terminal_or_files(rs, rsync_id, dst, {"keep1.txt", "keep2.txt"}, timeout=15.0)

        present = {p.name for p in dst.iterdir()}
        assert "keep1.txt" in present, present
        assert "keep2.txt" in present, present
        assert "drop.tmp" not in present, f"exclude failed: {present}"
        assert "also_drop.tmp" not in present, f"exclude failed: {present}"
    finally:
        _disconnect(http_client, sid)


# ---------------------------------------------------------------------------
# 10. delete=true removes destination files no longer present in src
# ---------------------------------------------------------------------------


@pytest.mark.timeout(90)
def test_rsync_delete_removes_extra_destination_files(
    http_client: McpClient, ssh_target: SshTarget, tmp_path: Path
) -> None:
    """First push 3 files; second push with one removed src file +
    ``delete=true`` must drop the extra file from the destination."""
    src = tmp_path / "src"
    dst = tmp_path / "dst"
    files_first = {
        "a.txt": b"AAAA",
        "b.txt": b"BBBB",
        "c.txt": b"CCCC",
    }
    _make_tree(src, files_first)
    dst.mkdir()

    sid = _connect(http_client, ssh_target, agent="rsync-delete")
    rs = RsyncTestClient(http_client, sid)
    try:
        # Push 1: full tree.
        text, _, _ = rs.start_rsync(src=str(src), dst=str(dst), transport="sftp")
        rsync_id_1 = parse_block(text).get("rsync_id")
        assert rsync_id_1, text
        _wait_terminal_or_files(rs, rsync_id_1, dst, set(files_first), timeout=15.0)
        present = {p.name for p in dst.iterdir()}
        assert present == set(files_first), present

        # Drop c.txt locally, then push with delete=true.
        (src / "c.txt").unlink()

        text2, _, _ = rs.start_rsync(
            src=str(src), dst=str(dst), transport="sftp", delete=True
        )
        rsync_id_2 = parse_block(text2).get("rsync_id")
        assert rsync_id_2, text2
        # Wait for c.txt to be removed; this is the precise delete signal.
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline:
            present = {p.name for p in dst.iterdir()}
            if "c.txt" not in present:
                break
            time.sleep(0.2)
        present = {p.name for p in dst.iterdir()}
        assert "c.txt" not in present, f"delete=true did not drop c.txt: {present}"
        assert "a.txt" in present and "b.txt" in present, present
    finally:
        _disconnect(http_client, sid)
