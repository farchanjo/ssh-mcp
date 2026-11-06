#!/usr/bin/env python3
"""Export the live ``tools/list`` catalogue to Markdown and JSON.

Spawns ``ssh-mcp-stdio``, completes the MCP handshake, and dumps every tool
with its title, description, annotations, and parameter descriptors. The
result is the exact surface a host sees, so the export can be diffed against
a previous revision or fed to a model as a selection aid.

Usage::

    python3 scripts/export_tool_catalog.py                 # write both files
    python3 scripts/export_tool_catalog.py --check         # fail on drift
    python3 scripts/export_tool_catalog.py --binary target/debug/ssh-mcp-stdio

Outputs::

    docs/TOOL_CATALOG.md
    docs/tool-catalog.json
"""

from __future__ import annotations

import argparse
import json
import selectors
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = ROOT / "target" / "release" / "ssh-mcp-stdio"
FALLBACK_BINARY = Path("/usr/local/bin/ssh-mcp-stdio")
PROTOCOL_VERSION = "2025-06-18"
HANDSHAKE_TIMEOUT_S = 60.0
MARKDOWN_PATH = ROOT / "docs" / "TOOL_CATALOG.md"
JSON_PATH = ROOT / "docs" / "tool-catalog.json"

# JSON Schema keywords that carry the allowed-value contract. Kept next to the
# renderer so a new keyword lands in both the Markdown and the JSON view.
VALUE_KEYWORDS = ("enum", "const", "default", "format", "minimum", "maximum")


def _request(request_id: int, method: str, params: dict[str, Any] | None = None) -> bytes:
    payload = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        payload["params"] = params
    return (json.dumps(payload) + "\n").encode()


class _Reader:
    """Newline-delimited JSON reader that keeps its buffer across responses."""

    def __init__(self, stream: Any) -> None:
        self._stream = stream
        self._buffer = b""
        self._selector = selectors.DefaultSelector()
        self._selector.register(stream, selectors.EVENT_READ)

    def close(self) -> None:
        self._selector.close()

    def response(self, wanted_id: int) -> dict[str, Any]:
        """Return the response carrying ``wanted_id``, skipping notifications."""
        while True:
            while b"\n" in self._buffer:
                line, self._buffer = self._buffer.split(b"\n", 1)
                if not line.strip():
                    continue
                message = json.loads(line)
                if message.get("id") != wanted_id:
                    continue
                if "error" in message:
                    raise RuntimeError(f"request id={wanted_id} failed: {message['error']}")
                return message["result"]
            if not self._selector.select(timeout=HANDSHAKE_TIMEOUT_S):
                raise TimeoutError(f"no response to id={wanted_id} within {HANDSHAKE_TIMEOUT_S}s")
            chunk = self._stream.read1(65536)
            if not chunk:
                raise RuntimeError("server closed stdout before answering")
            self._buffer += chunk


def _handshake(binary: Path) -> tuple[dict[str, str], dict[str, Any]]:
    """Run the handshake; return the server identity and the ``tools/list`` result."""
    proc = subprocess.Popen(  # noqa: S603 - fixed argv, no shell
        [str(binary)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    assert proc.stdin is not None
    assert proc.stdout is not None
    requests = [
        _request(
            1,
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "export-tool-catalog", "version": "1"},
            },
        ),
        (json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n").encode(),
        _request(2, "tools/list", {}),
    ]
    try:
        proc.stdin.write(b"".join(requests))
        proc.stdin.flush()
        reader = _Reader(proc.stdout)
        try:
            info = reader.response(wanted_id=1)["serverInfo"]
            tools = reader.response(wanted_id=2)
        finally:
            reader.close()
    finally:
        proc.stdin.close()
        proc.wait(timeout=HANDSHAKE_TIMEOUT_S)
    server = {
        "name": info.get("name", "ssh-mcp"),
        "version": info.get("version", "0.0.0"),
    }
    return server, tools


def _normalize_parameters(schema: dict[str, Any]) -> list[dict[str, Any]]:
    """Flatten one tool's properties into a stable, renderable list."""
    required = set(schema.get("required", []))
    defs = schema.get("$defs", {})
    rows: list[dict[str, Any]] = []
    for name, spec in sorted(schema.get("properties", {}).items()):
        row = {
            "name": name,
            "type": _type_of(spec),
            "required": name in required,
            "description": spec.get("description", ""),
            **{key: spec[key] for key in VALUE_KEYWORDS if key in spec},
        }
        resolved = _resolve_ref_values(spec, defs)
        if resolved:
            row["resolvedValues"] = resolved
        rows.append(row)
    return rows


def _resolve_ref_values(spec: dict[str, Any], defs: dict[str, Any]) -> list[str]:
    """Pull the value domain out of a ``$ref`` so the table stays self-contained."""
    values: list[str] = []
    for node in [spec, *spec.get("anyOf", []), *spec.get("oneOf", [])]:
        target = defs.get(node["$ref"].rsplit("/", 1)[-1], {}) if "$ref" in node else node
        branches = target.get("oneOf") or target.get("anyOf") or []
        values += [str(branch["const"]) for branch in branches if "const" in branch]
        values += [str(value) for value in target.get("enum", [])]
        values += [str(node["const"])] if "const" in node else []
    return list(dict.fromkeys(values))


def _type_of(spec: dict[str, Any]) -> str:
    """Render a property's type, resolving the nullable ``anyOf`` shape."""
    if "$ref" in spec:
        return spec["$ref"].rsplit("/", 1)[-1] + " (via $ref)"
    if "anyOf" in spec:
        return " | ".join(_type_of(branch) for branch in spec["anyOf"])
    if "oneOf" in spec:
        return " | ".join(_type_of(branch) for branch in spec["oneOf"])
    if "const" in spec:
        return f'"{spec["const"]}"'
    return str(spec.get("type", "any"))


def _normalize(tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Sort tools and their parameters so output diffs stay meaningful."""
    normalized = []
    for tool in sorted(tools, key=lambda item: item["name"]):
        schema = tool.get("inputSchema", {})
        normalized.append(
            {
                "name": tool["name"],
                "title": tool.get("title", ""),
                "description": tool.get("description", ""),
                "annotations": tool.get("annotations", {}),
                "parameters": _normalize_parameters(schema),
                "additionalProperties": schema.get("additionalProperties"),
                "defs": sorted(schema.get("$defs", {})),
                "outputSchemaBytes": len(json.dumps(tool.get("outputSchema", {}))),
            }
        )
    return normalized


def _render_markdown(server: dict[str, str], tools: list[dict[str, Any]]) -> str:
    lines = [
        "# Tool catalogue (generated)",
        "",
        "Exported from a live `tools/list` handshake by "
        "`scripts/export_tool_catalog.py`. Regenerate with `make tools-export`.",
        "",
        f"- Server: `{server['name']}` {server['version']}",
        f"- Tools: {len(tools)}",
        f"- Parameter descriptions: {sum(len(t['parameters']) for t in tools)}",
        "",
        "| Tool | Title | Params | Description bytes |",
        "| --- | --- | --- | --- |",
    ]
    for tool in tools:
        lines.append(
            f"| `{tool['name']}` | {tool['title'] or '—'} | "
            f"{len(tool['parameters'])} | {len(tool['description'])} |"
        )
    lines.append("")

    for tool in tools:
        annotations = ", ".join(f"{k}={v}" for k, v in sorted(tool["annotations"].items())) or "—"
        lines += [
            f"## `{tool['name']}`",
            "",
            f"**Title:** {tool['title'] or '—'}",
            "",
            f"**Annotations:** {annotations}",
            "",
        ]
        if tool["defs"]:
            lines += [f"**Reused definitions (`$defs`):** {', '.join(tool['defs'])}", ""]
        lines += ["### Description", "", tool["description"] or "—", "", "### Parameters", ""]
        if not tool["parameters"]:
            lines += ["No parameters.", ""]
            continue
        lines += ["| Name | Type | Required | Values | Description |", "| --- | --- | --- | --- | --- |"]
        for param in tool["parameters"]:
            values = _render_values(param)
            description = param["description"].replace("|", "\\|").replace("\n", " ")
            param_type = param["type"].replace("|", "\\|")
            lines.append(
                f"| `{param['name']}` | `{param_type}` | "
                f"{'yes' if param['required'] else 'no'} | {values} | {description or '—'} |"
            )
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def _render_values(param: dict[str, Any]) -> str:
    if "resolvedValues" in param:
        return ", ".join(f"`{value}`" for value in param["resolvedValues"])
    if "enum" in param:
        return ", ".join(f"`{value}`" for value in param["enum"])
    if "const" in param:
        return f"`{param['const']}`"
    return "—"


def _write(path: Path, content: str, check: bool) -> bool:
    """Write `content` to `path`; return True when the file changed."""
    previous = path.read_text() if path.exists() else None
    if previous == content:
        print(f"unchanged: {path.relative_to(ROOT)}")
        return False
    print(f"{'drifted' if check else 'wrote'}: {path.relative_to(ROOT)}")
    if not check:
        path.write_text(content)
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=None, help="ssh-mcp-stdio binary to drive")
    parser.add_argument("--check", action="store_true", help="exit 1 when the committed export drifted")
    args = parser.parse_args()

    binary = args.binary or (DEFAULT_BINARY if DEFAULT_BINARY.exists() else FALLBACK_BINARY)
    if not binary.exists():
        print(f"error: no binary at {binary}; run `make dev` or `make release`", file=sys.stderr)
        return 2

    server, result = _handshake(binary)
    tools = _normalize(result["tools"])
    markdown = _render_markdown(server, tools)
    document = json.dumps({"server": server, "tools": tools}, indent=2) + "\n"

    drifted = _write(MARKDOWN_PATH, markdown, args.check)
    drifted = _write(JSON_PATH, document, args.check) or drifted
    return 1 if args.check and drifted else 0


if __name__ == "__main__":
    sys.exit(main())
