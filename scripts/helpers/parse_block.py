"""Parser for ssh-mcp v3 block-style markdown responses.

ssh-mcp v3 (rmcp 1.6) returns plain markdown `Text<String>` content blocks for
all 18 tools. The format is strictly block-style after E6 (legacy `|` inline
form is gone):

  TOOL_NAME: STATUS
  KEY: value
  KEY: value
  --- stdout [a3f2b1d7] ---
  <content>
  --- stderr [a3f2b1d7] (empty) ---

This helper parses such a response into a flat dict keyed by lower-cased
field names, plus two convenience entries:

- ``__status``  — the STATUS portion of the header (e.g. ``OK``, ``ERROR``).
- ``__tool``    — the TOOL_NAME portion (e.g. ``SSH_CONNECT``).
- ``__blocks``  — dict of ``{name: text}`` for each ``--- name [...] ---``
                  block found in the body.
- ``__text``    — the original markdown text (used for diagnostics).
"""

from __future__ import annotations

import re

# Match a block delimiter line:
#   --- stdout [a3f2b1d7] ---           (open)
#   --- stdout [a3f2b1d7] (empty) ---   (open, marked empty)
#   --- stderr [a3f2b1d7] (truncated 1024) ---
_BLOCK_RE = re.compile(
    r"^---\s+(?P<name>[a-z_]+)(?:\s+\[(?P<nonce>[^\]]+)\])?(?:\s+\((?P<hint>[^)]*)\))?\s+---$"
)

# Bullet list items used by SSH_LIST_SESSIONS / SSH_LIST_COMMANDS.
_BULLET_RE = re.compile(r"^-\s+")

# Pre-parsed integer keys (kept as int when present).
_INT_KEYS = frozenset(
    {
        "exit",
        "exit_code",
        "count",
        "matches",
        "matches_count",
        "retry",
        "retry_attempts",
        "sessions",
        "commands",
        "sessions_disconnected",
        "sessions_closed",
        "commands_cancelled",
        "replaced",
        "bytes_sent",
        "bytes",
        "bytes_returned",
        "total_bytes",
        "bytes_transferred",
        "progress_percent",
        "repeat",
        "buffer_size",
        "last_seq",
        "cursor",
    }
)

# Pre-parsed boolean keys.
_BOOL_KEYS = frozenset({"active", "healthy", "persistent", "closed", "cancelled"})


def parse_block(text: str) -> dict:
    """Parse an ssh-mcp v3 markdown response into a dict.

    The first line is the header ``TOOL_NAME: STATUS``. Subsequent lines are
    either ``KEY: value`` rows, ``- bullet`` items (lists), or ``--- name [n]
    ---`` block markers (output buffers). All keys are lower-cased; numeric
    and boolean fields are coerced where they have a single canonical form.
    """
    result: dict = {"__text": text, "__blocks": {}}
    if not text:
        return result

    lines = text.splitlines()
    if not lines:
        return result

    header = lines[0].strip()
    if ":" in header:
        tool, status = header.split(":", 1)
        result["__tool"] = tool.strip()
        result["__status"] = status.strip()

    bullets: list[str] = []
    current_block: str | None = None
    current_nonce: str | None = None
    block_lines: list[str] = []

    for raw in lines[1:]:
        match = _BLOCK_RE.match(raw)
        if match is not None:
            name = match.group("name")
            nonce = match.group("nonce")
            hint = match.group("hint")
            # Same nonce as the open marker => this is the close marker.
            if (
                current_block is not None
                and current_block == name
                and current_nonce == nonce
            ):
                result["__blocks"][name] = "\n".join(block_lines)
                current_block = None
                current_nonce = None
                block_lines = []
                continue
            # Empty/closed-only sentinel — record empty block.
            if hint and "empty" in hint:
                result["__blocks"][name] = ""
                continue
            # Otherwise: opening a new block.
            current_block = name
            current_nonce = nonce
            block_lines = []
            continue

        if current_block is not None:
            block_lines.append(raw)
            continue

        if _BULLET_RE.match(raw):
            bullets.append(_BULLET_RE.sub("", raw, count=1))
            continue

        if ":" in raw:
            key, value = raw.split(":", 1)
            _assign(result, key.strip().lower(), value.strip())

    # Flush pending block (if the close marker was elided).
    if current_block is not None:
        result["__blocks"][current_block] = "\n".join(block_lines)

    if bullets:
        result["__bullets"] = bullets
        _hydrate_lists(result, bullets)

    # Promote canonical block aliases for convenience.
    blocks = result["__blocks"]
    for k in ("stdout", "stderr", "data"):
        if k in blocks and k not in result:
            result[k] = blocks[k]

    return result


def _assign(out: dict, key: str, value: str) -> None:
    """Coerce value into int/bool/str and store it under ``key``."""
    if key in _BOOL_KEYS:
        out[key] = value.lower() == "true"
        return
    if key in _INT_KEYS:
        try:
            # Strip any trailing units before converting.
            out[key] = int(value.split()[0])
            return
        except (ValueError, IndexError):
            pass
    out[key] = value


def _hydrate_lists(result: dict, bullets: list[str]) -> None:
    """Map bullet entries to structured arrays for known list tools."""
    tool = result.get("__tool", "")
    if tool == "SSH_LIST_SESSIONS":
        sessions: list[dict] = []
        for item in bullets:
            match = re.match(
                r"^(?P<sid>\S+)\s+(?P<endpoint>\S+)(?:\s+\[(?P<tags>[^\]]+)\])?$",
                item,
            )
            if not match:
                continue
            entry: dict = {"session_id": match.group("sid"), "endpoint": match.group("endpoint")}
            tags = match.group("tags") or ""
            for tag in (t.strip() for t in tags.split(",") if t.strip()):
                if tag in {"healthy", "unhealthy"}:
                    entry["healthy"] = tag == "healthy"
                elif ":" in tag:
                    k, v = tag.split(":", 1)
                    entry[k.strip()] = v.strip()
            sessions.append(entry)
        result["sessions"] = sessions
    elif tool == "SSH_LIST_COMMANDS":
        commands: list[dict] = []
        for item in bullets:
            match = re.match(
                r"^(?P<cid>\S+)\s+\[(?P<status>\w+)\]\s+(?P<sid>\S+):\s+(?P<cmd>.+?)\s+\((?P<ts>[^)]+)\)$",
                item,
            )
            if not match:
                continue
            commands.append(
                {
                    "command_id": match.group("cid"),
                    "status": match.group("status").lower(),
                    "session_id": match.group("sid"),
                    "command": match.group("cmd"),
                    "started_at": match.group("ts"),
                }
            )
        result["commands"] = commands


__all__ = ["parse_block"]
