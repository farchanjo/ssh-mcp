"""Convenience wrapper around the v7.0 ``ssh_rsync*`` MCP surface.

Mirrors ``helpers.mcp_client`` style: takes an :class:`McpClient` plus a
session id and exposes a small number of high-signal methods so tests
stay short. The wrapper does NOT shadow any wire details — calls return
the same ``(text, structured, raw)`` shape as ``call_tool_pair`` so
callers can still cross-check the markdown body if they need to.

Notes on the v7.0 SFTP-transport surface (matters for assertions):

- ``ssh_rsync`` returns ``STATUS: STARTED`` synchronously and mints a
  ``RSYNC_ID``; the actual transfer runs on a background task.
- Progress notifications fire on ``rsync://<RSYNC_ID>/progress`` —
  but the snapshot the lane carries is rebuilt on every read from the
  domain aggregate's atomic counters.
- Today (v7.0.0-alpha.4) the SFTP transport's status field can stay
  at ``pending`` even after the bytes have landed; tests that need to
  verify completion should also check the destination filesystem.
- The Wire transport surfaces ``RSYNC_PROTOCOL_ERROR`` until slice 3
  lands — ``transport=wire`` calls against a real rsync remote are
  expected to error with that code on most VMs.
"""

from __future__ import annotations

import json
import time
from typing import Any

from .mcp_client import McpClient, call_tool_pair, extract_structured
from .parse_block import parse_block


_DEFAULT_PRESERVE_ALL_OFF: dict[str, bool] = {
    "perms": False,
    "mtime": False,
    "owner": False,
    "group": False,
    "links": False,
    "hardlinks": False,
    "sparse": False,
    "devices": False,
}


def preserve_all_off() -> dict[str, bool]:
    """Return a fresh copy of the "preserve nothing" mask.

    Useful against the local paramiko fixture, whose tiny SFTP server
    does not support symlink / setstat / chown — leaving any preserve
    flag at its default ``true`` will trip ``SFTP_FEATURE_MISSING``.
    """
    return dict(_DEFAULT_PRESERVE_ALL_OFF)


class RsyncTestClient:
    """Thin facade exposing ``ssh_rsync`` happy-path helpers."""

    def __init__(self, client: McpClient, session_id: str) -> None:
        self.client = client
        self.session_id = session_id

    # -- starters ------------------------------------------------------------

    def start_rsync(
        self,
        *,
        src: str,
        dst: str,
        transport: str = "sftp",
        recursive: bool = True,
        delete: bool = False,
        dry_run: bool = False,
        exclude: list[str] | None = None,
        include: list[str] | None = None,
        preserve: dict[str, bool] | None = None,
        timeout: float = 30.0,
    ) -> tuple[str, dict, dict]:
        """Drive ``ssh_rsync`` and return ``(text, structured, raw)``.

        ``preserve`` defaults to "all-off" — see :func:`preserve_all_off`
        for why this is safe against a paramiko SFTP fixture.
        """
        opts: dict[str, Any] = {
            "recursive": recursive,
            "preserve": preserve if preserve is not None else preserve_all_off(),
        }
        if delete:
            opts["delete"] = True
        if dry_run:
            opts["dry_run"] = True
        if exclude:
            opts["exclude"] = list(exclude)
        if include:
            opts["include"] = list(include)
        return call_tool_pair(
            self.client,
            "ssh_rsync",
            {
                "session_id": self.session_id,
                "src": src,
                "dst": dst,
                "transport": transport,
                "opts": opts,
            },
            timeout=timeout,
        )

    def stats(self, rsync_id: str, *, timeout: float = 10.0) -> tuple[str, dict, dict]:
        return call_tool_pair(
            self.client, "ssh_rsync_stats", {"rsync_id": rsync_id}, timeout=timeout
        )

    def cancel(self, rsync_id: str, *, timeout: float = 10.0) -> tuple[str, dict, dict]:
        return call_tool_pair(
            self.client, "ssh_rsync_cancel", {"rsync_id": rsync_id}, timeout=timeout
        )

    # -- progress lane -------------------------------------------------------

    def progress_uri(self, rsync_id: str) -> str:
        return f"rsync://{rsync_id}/progress"

    def subscribe_progress(self, rsync_id: str) -> str:
        uri = self.progress_uri(rsync_id)
        self.client.subscribe(uri)
        return uri

    def drain_progress(
        self,
        rsync_id: str,
        *,
        timeout: float = 8.0,
        terminal_only: bool = False,
    ) -> list[dict]:
        """Drain progress snapshots from the lane until the deadline.

        Each entry is a parsed JSON dict with ``status``, ``files_total``,
        ``files_done``, ``files_failed`` etc. (per
        ``rsync_progress_body``).

        When ``terminal_only`` is ``True`` we keep draining until we see
        a terminal ``status`` (completed / failed / cancelled) or the
        deadline elapses — useful for tests that need a deterministic
        finish signal.
        """
        uri = self.progress_uri(rsync_id)
        snapshots: list[dict] = []
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            n = self.client.receive_notification(timeout=0.5)
            if n is None:
                continue
            if n.get("method") != "notifications/resources/updated":
                continue
            params = n.get("params") or {}
            if not str(params.get("uri", "")).startswith(f"rsync://{rsync_id}"):
                continue
            snap = self._read_snapshot(uri)
            if snap is not None:
                snapshots.append(snap)
                if terminal_only and snap.get("status") in {"completed", "failed", "cancelled"}:
                    return snapshots
        return snapshots

    def read_snapshot(self, rsync_id: str) -> dict | None:
        return self._read_snapshot(self.progress_uri(rsync_id))

    def _read_snapshot(self, uri: str) -> dict | None:
        result = self.client.read_resource(uri)
        contents = result.get("contents") or []
        if not contents:
            return None
        text = contents[0].get("text", "") or ""
        if not text:
            return None
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            return None


def parse_started(text: str) -> dict:
    """Parse a ``SSH_RSYNC: STARTED`` block-markdown body.

    Returns a flat dict; the ``rsync_id`` / ``session_id`` / ``transport``
    keys are the convenient ones for assertions.
    """
    return parse_block(text)


def parse_stats(text: str) -> dict:
    return parse_block(text)


__all__ = [
    "RsyncTestClient",
    "extract_structured",
    "parse_started",
    "parse_stats",
    "preserve_all_off",
]
