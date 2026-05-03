"""Shared pytest fixtures for ssh-mcp v3 integration tests.

The HTTP fixture spawns ``target/release/ssh-mcp`` on a free port; the stdio
fixture spawns ``target/release/ssh-mcp-stdio`` directly.

Tests that exercise real SSH connectivity must be marked with
``@pytest.mark.requires_sshd`` and read the target endpoint from the
``SSH_MCP_TEST_TARGET`` environment variable. When that variable is unset the
fixture skips the test.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import time
from contextlib import closing
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

import pytest

from .mcp_client import HttpTransport, McpClient, StdioTransport


REPO_ROOT = Path(__file__).resolve().parent.parent.parent
HTTP_BIN = REPO_ROOT / "target" / "release" / "ssh-mcp"
STDIO_BIN = REPO_ROOT / "target" / "release" / "ssh-mcp-stdio"


def stress_transport_mode() -> str:
    """Return the transport selector for stress scripts.

    Reads ``STRESS_TRANSPORT`` (case-insensitive), defaulting to ``stdio``.
    Recognised values are ``stdio`` and ``http``; anything else raises a
    ``ValueError`` so typos surface immediately.
    """
    raw = os.environ.get("STRESS_TRANSPORT", "stdio").strip().lower()
    if raw in ("stdio", "http"):
        return raw
    raise ValueError(
        f"unknown STRESS_TRANSPORT={raw!r}; expected 'stdio' or 'http'"
    )


def make_stress_client():
    """Build a fresh, initialised :class:`McpClient` per the active mode.

    Stdio mode spawns a dedicated ``ssh-mcp-stdio`` child per client (each
    process IS the session). Http mode shares the caller-supplied port via
    keyword argument — see :func:`make_stress_client_http`.
    """
    if not STDIO_BIN.exists():
        raise FileNotFoundError(f"stdio binary not built: {STDIO_BIN}")
    transport = StdioTransport(
        [str(STDIO_BIN)],
        env={"RUST_LOG": os.environ.get("RUST_LOG", "warn")},
    )
    client = McpClient(transport)
    client.initialize()
    return client


def make_stress_client_http(port: int) -> "McpClient":
    """Build a fresh, initialised :class:`McpClient` over HTTP on ``port``."""
    transport = HttpTransport(f"http://127.0.0.1:{port}")
    client = McpClient(transport)
    client.initialize()
    return client


@dataclass
class SshTarget:
    """SSH endpoint coordinates pulled from environment variables."""

    address: str
    username: str
    key_path: str | None
    password: str | None

    @classmethod
    def from_env(cls) -> "SshTarget | None":
        target = os.environ.get("SSH_MCP_TEST_TARGET")
        if not target:
            return None
        username = os.environ.get("SSH_MCP_TEST_USER") or os.environ.get("USER", "root")
        key_path = os.environ.get("SSH_MCP_TEST_KEY") or os.path.expanduser("~/.ssh/id_rsa")
        password = os.environ.get("SSH_MCP_TEST_PASSWORD")
        return cls(address=target, username=username, key_path=key_path if Path(os.path.expanduser(key_path)).exists() else None, password=password)

    def connect_args(self, **extra) -> dict:
        args: dict = {"address": self.address, "username": self.username, "reuse": "force_new"}
        if self.key_path:
            args["key_path"] = self.key_path
        if self.password:
            args["password"] = self.password
        args.update(extra)
        return args


def find_free_port() -> int:
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_for_port(host: str, port: int, *, timeout: float = 10.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as sock:
            sock.settimeout(0.5)
            try:
                sock.connect((host, port))
                return True
            except OSError:
                time.sleep(0.1)
    return False


# ---------------------------------------------------------------------------
# HTTP fixture
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def http_server() -> Iterator[tuple[subprocess.Popen, int]]:
    """Spawn ``ssh-mcp`` on a free port for the duration of the module."""
    if not HTTP_BIN.exists():
        pytest.skip(f"http binary not built: {HTTP_BIN}")
    port = find_free_port()
    env = {**os.environ, "MCP_PORT": str(port), "MCP_HOST": "127.0.0.1", "RUST_LOG": os.environ.get("RUST_LOG", "warn")}
    proc = subprocess.Popen(
        [str(HTTP_BIN)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        if not wait_for_port("127.0.0.1", port, timeout=10.0):
            proc.terminate()
            pytest.fail(f"ssh-mcp HTTP did not bind to port {port} within 10s")
        yield proc, port
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()


@pytest.fixture
def http_client(http_server) -> Iterator[McpClient]:
    _proc, port = http_server
    transport = HttpTransport(f"http://127.0.0.1:{port}")
    client = McpClient(transport)
    client.initialize()
    try:
        yield client
    finally:
        client.close()


# ---------------------------------------------------------------------------
# Stdio fixture
# ---------------------------------------------------------------------------


@pytest.fixture
def stdio_client() -> Iterator[McpClient]:
    if not STDIO_BIN.exists():
        pytest.skip(f"stdio binary not built: {STDIO_BIN}")
    transport = StdioTransport([str(STDIO_BIN)], env={"RUST_LOG": os.environ.get("RUST_LOG", "warn")})
    client = McpClient(transport)
    client.initialize()
    try:
        yield client
    finally:
        client.close()


# ---------------------------------------------------------------------------
# SSH endpoint fixture
# ---------------------------------------------------------------------------


@pytest.fixture
def ssh_target(request) -> SshTarget:
    target = SshTarget.from_env()
    if target is None:
        pytest.skip("SSH_MCP_TEST_TARGET not set; requires_sshd test skipped")
    return target


def requires_sshd(func):
    """Marker shortcut combining the pytest mark and the ssh_target fixture."""
    return pytest.mark.requires_sshd(func)


__all__ = [
    "REPO_ROOT",
    "HTTP_BIN",
    "STDIO_BIN",
    "SshTarget",
    "find_free_port",
    "wait_for_port",
    "http_server",
    "http_client",
    "stdio_client",
    "ssh_target",
    "requires_sshd",
    "stress_transport_mode",
    "make_stress_client",
    "make_stress_client_http",
]
