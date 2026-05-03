"""Chaos-suite helpers: stderr-capturing stdio transport + JSON line writer.

The chaos scripts (``scripts/chaos_*.py``) extend the v4.3 stress harness by
running each scenario against an *isolated* ``ssh-mcp-stdio`` child whose
stderr is buffered so the test asserts no ``panicked`` line was emitted by
the server during the scenario. Ordinary stress scripts pipe stderr to
``/dev/null`` because they only care about the stdout JSON-RPC stream;
chaos scripts treat any panic in stderr as a hard test fail.

This module also exposes a tiny structured-output helper so every chaos
scenario writes a single JSON line per assertion (``write_event``) plus a
final summary (``write_summary``) — both go to stdout so callers can pipe
the script output through ``jq`` without having to parse mixed text.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from contextlib import contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from dataclasses import dataclass  # noqa: E402

from helpers.mcp_client import McpClient, StdioTransport  # noqa: E402

# Resolve binary paths directly so this module never pulls in `pytest`
# (which `helpers.fixtures` imports). Chaos scripts are CLI tools, not
# pytest collectors.
REPO_ROOT = Path(__file__).resolve().parent.parent.parent
STDIO_BIN = REPO_ROOT / "target" / "release" / "ssh-mcp-stdio"
HTTP_BIN = REPO_ROOT / "target" / "release" / "ssh-mcp"


# ---------------------------------------------------------------------------
# Lightweight SSH target — duplicates `helpers.fixtures.SshTarget.from_env`
# so chaos scripts (which are not pytest collectors) avoid pulling in
# `pytest` as a runtime dependency.
# ---------------------------------------------------------------------------


@dataclass
class ChaosSshTarget:
    """SSH endpoint coordinates for chaos scenarios."""

    address: str
    username: str
    key_path: str | None
    password: str | None

    @classmethod
    def from_env(cls) -> "ChaosSshTarget | None":
        target = os.environ.get("SSH_MCP_TEST_TARGET")
        if not target:
            return None
        username = os.environ.get("SSH_MCP_TEST_USER") or os.environ.get("USER", "root")
        key_path = os.environ.get("SSH_MCP_TEST_KEY") or os.path.expanduser("~/.ssh/id_rsa")
        password = os.environ.get("SSH_MCP_TEST_PASSWORD")
        resolved_key = key_path if Path(os.path.expanduser(key_path)).exists() else None
        return cls(address=target, username=username, key_path=resolved_key, password=password)

    def connect_args(self, **extra) -> dict:
        args: dict = {"address": self.address, "username": self.username, "reuse": "force_new"}
        if self.key_path:
            args["key_path"] = self.key_path
        if self.password:
            args["password"] = self.password
        args.update(extra)
        return args


# ---------------------------------------------------------------------------
# JSON line emission
# ---------------------------------------------------------------------------


def write_event(event: dict) -> None:
    """Emit a single JSON line to stdout. Used per chaos assertion."""
    print(json.dumps(event, default=str), flush=True)


def write_summary(summary: dict) -> int:
    """Emit a final JSON summary line and return the process exit code."""
    print(json.dumps(summary, default=str), flush=True)
    return 0 if summary.get("status") == "ok" else 1


# ---------------------------------------------------------------------------
# Stderr-capturing stdio transport
# ---------------------------------------------------------------------------


class _StderrCapturingStdioTransport(StdioTransport):
    """:class:`StdioTransport` that buffers the child stderr in memory.

    The chaos suite asserts ``"panicked"`` never appears on the server
    stderr during a scenario; the existing stress fixture pipes stderr to
    ``/dev/null`` (no panic visibility), so we cannot reuse it as-is.
    """

    def __init__(self, cmd: list[str], env: dict | None = None) -> None:
        super().__init__(cmd, env=env)
        self._stderr_buf: list[str] = []
        self._stderr_thread: threading.Thread | None = None

    def start(self) -> None:
        if self._proc is not None:
            return
        self._proc = subprocess.Popen(
            self.cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.env,
            bufsize=1,
            text=True,
        )
        self._stdout_thread = threading.Thread(
            target=self._reader_loop, name="stdio-reader", daemon=True
        )
        self._stdout_thread.start()
        self._stderr_thread = threading.Thread(
            target=self._stderr_loop, name="stdio-stderr", daemon=True
        )
        self._stderr_thread.start()

    def _stderr_loop(self) -> None:
        proc = self._proc
        if proc is None or proc.stderr is None:
            return
        try:
            for line in proc.stderr:
                if self._stop.is_set():
                    return
                self._stderr_buf.append(line)
        except (ValueError, OSError):
            return

    def stderr_text(self) -> str:
        return "".join(self._stderr_buf)

    def panicked(self) -> bool:
        return "panicked" in self.stderr_text()


def make_chaos_client() -> tuple[McpClient, _StderrCapturingStdioTransport]:
    """Spawn a fresh ``ssh-mcp-stdio`` child with stderr capture and
    return an initialised :class:`McpClient` + the underlying transport."""
    if not STDIO_BIN.exists():
        raise FileNotFoundError(f"stdio binary not built: {STDIO_BIN}")
    transport = _StderrCapturingStdioTransport(
        [str(STDIO_BIN)],
        env={"RUST_LOG": os.environ.get("RUST_LOG", "warn")},
    )
    client = McpClient(transport)
    client.initialize()
    return client, transport


@contextmanager
def chaos_session():
    """Context manager owning one isolated ``ssh-mcp-stdio`` child.

    Usage::

        with chaos_session() as (client, transport):
            ...

    On exit the child is terminated and its stderr can be inspected via
    ``transport.panicked()`` / ``transport.stderr_text()``.
    """
    client, transport = make_chaos_client()
    try:
        yield client, transport
    finally:
        try:
            client.close()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Scenario runner
# ---------------------------------------------------------------------------


def run_scenario(name: str, body: Callable[[], dict | None]) -> dict:
    """Run a chaos scenario function under a single try/except.

    Returns the merged event dict (``{"scenario": name, "ok": bool, ...}``)
    and emits it via ``write_event``. ``body`` is expected to return a dict
    of structured fields (or ``None`` for an implicit ``ok``). Exceptions
    are captured and reported as ``ok=false``.
    """
    started = time.monotonic()
    try:
        result = body() or {}
        ok = result.pop("ok", True) if isinstance(result, dict) else True
    except Exception as exc:  # pragma: no cover - chaos-test catch-all
        result = {"error": f"{type(exc).__name__}: {exc}"}
        ok = False
    elapsed = round(time.monotonic() - started, 3)
    event = {"scenario": name, "ok": bool(ok), "elapsed_s": elapsed}
    if isinstance(result, dict):
        event.update(result)
    write_event(event)
    return event


__all__ = [
    "ChaosSshTarget",
    "HTTP_BIN",
    "STDIO_BIN",
    "chaos_session",
    "make_chaos_client",
    "run_scenario",
    "write_event",
    "write_summary",
]
