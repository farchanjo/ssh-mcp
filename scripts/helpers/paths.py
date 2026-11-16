"""Cargo build-profile resolution shared by the Python suites.

The dev loop never runs a release build (see AGENTS.md → Working agreement), so
every fixture, stress script and chaos orchestrator drives ``target/debug`` by
default. Two environment overrides are honoured:

``SSH_MCP_BUILD_PROFILE``
    ``debug`` (default) or ``release`` — retargets the whole suite at once,
    which is what CI does, since its ``build-test`` job compiles release.

``SSH_MCP_BIN`` / ``SSH_MCP_STDIO_BIN`` / ``SSH_MCP_TAIL_BIN``
    Absolute path to one binary, outranking the profile for that binary alone.

This module deliberately imports neither ``pytest`` nor any sibling helper:
``helpers.chaos`` and the CLI stress scripts pull it in without dragging the
pytest collector machinery along.
"""

from __future__ import annotations

import os
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent.parent

DEFAULT_PROFILE = "debug"


def build_profile() -> str:
    """Return the active cargo profile directory name under ``target/``."""
    raw = os.environ.get("SSH_MCP_BUILD_PROFILE", DEFAULT_PROFILE).strip()
    return raw or DEFAULT_PROFILE


def bin_dir() -> Path:
    """Return the directory holding the binaries for the active profile."""
    return REPO_ROOT / "target" / build_profile()


def binary(name: str, env_var: str) -> Path:
    """Resolve ``name`` under the active profile, honouring ``env_var``.

    On Windows cargo writes ``<name>.exe``; callers check ``Path.exists()``
    before spawning and that check does not append the extension the way
    ``CreateProcess`` does, so the suffixed path is tried as well.
    """
    override = os.environ.get(env_var)
    if override:
        return Path(override).expanduser()
    resolved = bin_dir() / name
    if resolved.exists():
        return resolved
    suffixed = resolved.with_name(f"{resolved.name}.exe")
    return suffixed if suffixed.exists() else resolved


def http_bin() -> Path:
    """``ssh-mcp`` — the HTTP/streamable-http server binary."""
    return binary("ssh-mcp", "SSH_MCP_BIN")


def stdio_bin() -> Path:
    """``ssh-mcp-stdio`` — the stdio MCP transport binary."""
    return binary("ssh-mcp-stdio", "SSH_MCP_STDIO_BIN")


def tail_bin() -> Path:
    """``ssh-mcp-tail`` — the NDJSON daemon binary."""
    return binary("ssh-mcp-tail", "SSH_MCP_TAIL_BIN")
