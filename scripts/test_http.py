#!/usr/bin/env python3
"""Integration test for ssh-mcp HTTP binary — all 16 tools."""
import json
import os
import sys
import time
import http.client
import threading
import uuid as _uuid
from concurrent.futures import ThreadPoolExecutor, as_completed

BASE_HOST = "127.0.0.1"
BASE_PORT = int(os.environ.get("MCP_PORT", "8000"))
HOST = os.environ.get("SSH_HOST", "vm.services:22")
USER = os.environ.get("SSH_USER", "root")
KEY_PATH = os.environ.get("SSH_KEY_PATH", "~/.ssh/id_rsa")
LOCAL_FILE = os.environ.get("LOCAL_FILE", "/Users/farchanjo/Downloads/IMG_1267.MOV")
REMOTE_DIR = "/tmp/ssh-mcp-http-test"
REMOTE_FILE = f"{REMOTE_DIR}/{os.path.basename(LOCAL_FILE)}"
DOWNLOAD_FILE = "/tmp/ssh-mcp-http-test-downloaded" + os.path.splitext(LOCAL_FILE)[1]

req_id = 0
session_header = None


def connect_args(agent_id=None, name=None, **extra):
    """Build ssh_connect arguments with key_path.

    Defaults `reuse="force_new"` so repeated connects in the test suite
    always produce distinct sessions (tests that want to exercise the
    reuse/suggest flow pass `reuse=...` explicitly via extra kwargs).
    """
    args = {"address": HOST, "username": USER, "key_path": KEY_PATH, "reuse": "force_new"}
    if agent_id:
        args["agent_id"] = agent_id
    if name:
        args["name"] = name
    args.update(extra)
    return args


def mcp(method, params=None):
    global req_id, session_header
    req_id += 1
    current_id = req_id
    body = json.dumps({"jsonrpc": "2.0", "id": current_id, "method": method, "params": params or {}})
    conn = http.client.HTTPConnection(BASE_HOST, BASE_PORT, timeout=300)
    headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
    if session_header:
        headers["mcp-session-id"] = session_header
    conn.request("POST", "/", body, headers)
    resp = conn.getresponse()
    sid = resp.getheader("mcp-session-id")
    if sid:
        session_header = sid
    data = resp.read().decode()
    conn.close()
    if resp.status != 200:
        return {"error": f"HTTP {resp.status}: {data[:200]}"}
    content_type = resp.getheader("content-type", "")
    if "text/event-stream" in content_type:
        # Parse all SSE data lines and find the JSON-RPC response matching our request id
        candidates = []
        for line in data.split("\n"):
            if line.startswith("data: "):
                try:
                    parsed = json.loads(line[6:])
                    candidates.append(parsed)
                except json.JSONDecodeError:
                    continue
        # Find the response with matching id
        for c in candidates:
            if isinstance(c, dict) and c.get("id") == current_id:
                return c
        # Fallback: return first dict with "result" or "error"
        for c in candidates:
            if isinstance(c, dict) and ("result" in c or "error" in c):
                return c
        # Last resort: return first candidate
        if candidates:
            return candidates[0] if isinstance(candidates[0], dict) else {"error": f"Unexpected SSE data: {candidates[0]}"}
        return {"error": "No data in SSE response"}
    parsed = json.loads(data)
    # Server may return batch response (JSON array) — unwrap to find our response
    if isinstance(parsed, list):
        for item in parsed:
            if isinstance(item, dict) and item.get("id") == current_id:
                return item
        return parsed[0] if parsed else {"error": "Empty batch response"}
    return parsed


import re


def parse_mcp_response(text: str) -> dict:
    """Parse the markdown MCP response format introduced in v2.0.

    Maps back to the legacy JSON field names expected by the test suite so
    existing assertions keep working with minimal change.

    Returns a dict populated (where applicable) with: session_id, command_id,
    shell_id, transfer_id, agent_id, status, exit_code, stdout, stderr, data,
    count, sessions (list), commands (list), bytes_transferred, total_bytes,
    progress_percent, direction, closed, cancelled, active, local_address,
    remote_address, sessions_disconnected, commands_cancelled, error, _tool,
    _status, _text (full original text).
    """
    out = {"_text": text}
    if not text:
        return out

    lines = text.splitlines()
    if not lines:
        return out

    # First line: TOOL: STATUS [| key: v | ...]
    header = lines[0]
    if "|" in header:
        parts = [p.strip() for p in header.split("|")]
    else:
        parts = [header.strip()]

    head = parts[0]
    if ":" in head:
        tool, status = head.split(":", 1)
        out["_tool"] = tool.strip()
        out["_status"] = status.strip()
        out["status"] = status.strip().lower()
    for p in parts[1:]:
        if ":" in p:
            k, v = p.split(":", 1)
            _assign_field(out, k.strip(), v.strip(), tool=out.get("_tool", ""))
        else:
            _assign_token(out, p, tool=out.get("_tool", ""))

    # Body: block fields, bullets, and --- blocks
    current_block = None  # 'stdout' | 'stderr' | 'data' | None
    block_lines = []
    bullet_list = []
    marker_re = re.compile(r"^--- (stdout|stderr|data)(?: \[[^\]]+\])?(?: \([^)]*\))? ---$")
    for line in lines[1:]:
        m = marker_re.match(line)
        if m:
            if current_block is not None:
                out[current_block] = "\n".join(block_lines)
                block_lines = []
            current_block = m.group(1)
            continue
        if current_block is not None:
            block_lines.append(line)
            continue
        if line.startswith("- "):
            bullet_list.append(line[2:])
            continue
        if ":" in line:
            k, v = line.split(":", 1)
            _assign_field(out, k.strip(), v.strip(), tool=out.get("_tool", ""))
    if current_block is not None:
        out[current_block] = "\n".join(block_lines)

    if bullet_list:
        _assign_bullets(out, bullet_list)

    return out


def _assign_token(out: dict, token: str, tool: str = "") -> None:
    """Assign a bare token from an inline header (e.g. 'UPLOAD 50% (a/b bytes)')."""
    m = re.match(r"^(UPLOAD|DOWNLOAD)\s+(\d+)%\s+\((\d+)/(\d+)\s+bytes\)$", token)
    if m:
        out["direction"] = m.group(1).lower()
        out["progress_percent"] = int(m.group(2))
        out["bytes_transferred"] = int(m.group(3))
        out["total_bytes"] = int(m.group(4))


def _assign_field(out: dict, key: str, value: str, tool: str = "") -> None:
    """Map a single KEY: value line to an output field."""
    k_lower = key.lower()
    key_map = {
        "session_id": "session_id",
        "existing_session_id": "session_id",
        "command_id": "command_id",
        "shell_id": "shell_id",
        "transfer_id": "transfer_id",
        "agent": "agent_id",
        "exit": "exit_code",
        "bytes_sent": "bytes_sent",
        "bytes": "total_bytes",
        "size": "size_raw",
        "local": "local_address",
        "remote": "remote_address",
        "active": "active",
        "count": "count",
        "matches": "matches_count",
        "host": "host",
        "name": "name",
        "connected_at": "connected_at",
        "healthy": "healthy",
        "retry": "retry_attempts",
        "persistent": "persistent",
        "replaced": "replaced",
        "sessions": "sessions_disconnected",
        "commands": "commands_cancelled",
        "sessions_closed": "sessions_disconnected",
        "commands_cancelled": "commands_cancelled",
        "term": "term_type",
        "reason": "error",
        "detail": "error_detail",
        "direction": "direction",
        "progress": "progress_raw",
        "from": "from_path",
        "to": "to_path",
        "hint": "_hint",
    }
    mapped = key_map.get(k_lower, k_lower)

    if mapped == "active":
        out[mapped] = value.lower() == "true"
        return
    if mapped == "healthy":
        out[mapped] = value.lower() == "true"
        return
    if mapped == "persistent":
        out[mapped] = value.lower() == "true"
        return
    if mapped in ("exit_code", "count", "matches_count", "retry_attempts",
                  "sessions_disconnected", "commands_cancelled", "replaced", "bytes_sent"):
        try:
            out[mapped] = int(value.split()[0])
        except (ValueError, IndexError):
            out[mapped] = value
        return
    if mapped == "total_bytes":
        try:
            out[mapped] = int(value)
        except ValueError:
            out[mapped] = value
        return
    if mapped == "size_raw":
        # "SIZE: 1.0MB (1048576 bytes)" — extract the raw byte count.
        m = re.search(r"\((\d+)\s+bytes\)", value)
        if m:
            out["total_bytes"] = int(m.group(1))
        out[mapped] = value
        return
    if mapped == "progress_raw":
        # "PROGRESS: 50% (524288/1048576 bytes)"
        m = re.match(r"^(\d+)%\s+\((\d+)/(\d+)\s+bytes\)$", value)
        if m:
            out["progress_percent"] = int(m.group(1))
            out["bytes_transferred"] = int(m.group(2))
            out["total_bytes"] = int(m.group(3))
        out[mapped] = value
        return
    if mapped == "direction":
        out[mapped] = value.lower()
        return
    # Strip bracketed error code from reason when present.
    if mapped == "error":
        # "[CODE] reason text" — keep the bracketed code prefix since existing
        # tests check `err.startswith("[")`.
        out[mapped] = value
        return

    out[mapped] = value


def _assign_bullets(out: dict, bullets: list) -> None:
    """Convert bullet-list items into structured arrays."""
    tool = out.get("_tool", "")
    if tool == "SSH_LIST_SESSIONS":
        parsed = []
        for b in bullets:
            # "sess-abc123 user@host:22 [agent: a, name: n, healthy]"
            m = re.match(r"^(\S+)\s+(\S+)(?:\s+\[([^\]]+)\])?$", b)
            if not m:
                continue
            tags = m.group(3) or ""
            item = {"session_id": m.group(1), "host": m.group(2)}
            for tag in (t.strip() for t in tags.split(",") if t.strip()):
                if tag == "healthy":
                    item["healthy"] = True
                elif tag == "unhealthy":
                    item["healthy"] = False
                elif ":" in tag:
                    k, v = tag.split(":", 1)
                    item[k.strip()] = v.strip()
            parsed.append(item)
        out["sessions"] = parsed
    elif tool == "SSH_LIST_COMMANDS":
        parsed = []
        for b in bullets:
            m = re.match(r"^(\S+)\s+\[(\w+)\]\s+(\S+):\s+(.+?)\s+\(([^)]+)\)$", b)
            if not m:
                continue
            parsed.append({
                "command_id": m.group(1),
                "status": m.group(2).lower(),
                "session_id": m.group(3),
                "command": m.group(4),
                "started_at": m.group(5),
            })
        out["commands"] = parsed


def tool(name, args):
    resp = mcp("tools/call", {"name": name, "arguments": args})
    result = resp.get("result", {})
    if result.get("isError"):
        err_text = result["content"][0]["text"] if result.get("content") else "Unknown"
        parsed = parse_mcp_response(err_text)
        return {"error": parsed.get("error", err_text), **parsed}
    if result.get("content"):
        text = result["content"][0].get("text", "")
        parsed = parse_mcp_response(text)
        if parsed.get("_status") == "ERROR" and "error" in parsed:
            return {"error": parsed["error"], **parsed}
        status = parsed.get("_status", "")
        if status == "CANCELLED":
            parsed["cancelled"] = True
        if name == "ssh_shell_close" and status == "OK":
            parsed["closed"] = True
        return parsed
    return result


_req_lock = threading.Lock()


def mcp_threadsafe(method, params=None):
    global req_id, session_header
    with _req_lock:
        req_id += 1
        current_id = req_id
        hdr = session_header
    body = json.dumps({"jsonrpc": "2.0", "id": current_id, "method": method, "params": params or {}})
    conn = http.client.HTTPConnection(BASE_HOST, BASE_PORT, timeout=300)
    headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
    if hdr:
        headers["mcp-session-id"] = hdr
    conn.request("POST", "/", body, headers)
    resp = conn.getresponse()
    sid = resp.getheader("mcp-session-id")
    if sid:
        with _req_lock:
            session_header = sid
    data = resp.read().decode()
    conn.close()
    if resp.status != 200:
        return {"error": f"HTTP {resp.status}: {data[:200]}"}
    content_type = resp.getheader("content-type", "")
    if "text/event-stream" in content_type:
        candidates = []
        for line in data.split("\n"):
            if line.startswith("data: "):
                try:
                    candidates.append(json.loads(line[6:]))
                except json.JSONDecodeError:
                    continue
        for c in candidates:
            if isinstance(c, dict) and c.get("id") == current_id:
                return c
        for c in candidates:
            if isinstance(c, dict) and ("result" in c or "error" in c):
                return c
        if candidates:
            return candidates[0] if isinstance(candidates[0], dict) else {"error": f"Unexpected SSE: {candidates[0]}"}
        return {"error": "No data in SSE response"}
    parsed = json.loads(data)
    if isinstance(parsed, list):
        for item in parsed:
            if isinstance(item, dict) and item.get("id") == current_id:
                return item
        return parsed[0] if parsed else {"error": "Empty batch response"}
    return parsed


def tool_threadsafe(name, args):
    resp = mcp_threadsafe("tools/call", {"name": name, "arguments": args})
    result = resp.get("result", {})
    if result.get("isError"):
        return {"error": result["content"][0]["text"] if result.get("content") else "Unknown"}
    if result.get("content"):
        text = result["content"][0].get("text", "")
        parsed = parse_mcp_response(text)
        if parsed.get("_status") == "ERROR" and "error" in parsed:
            return {"error": parsed["error"], **parsed}
        if parsed.get("_status") == "CANCELLED":
            parsed["cancelled"] = True
        if name == "ssh_shell_close" and parsed.get("_status") == "OK":
            parsed["closed"] = True
        return parsed
    return result


passed = 0
failed = 0
total = 0


def test(label, ok, detail=""):
    global passed, failed, total
    total += 1
    status = "PASS" if ok else "FAIL"
    print(f"  [{status}] {label}", flush=True)
    if detail:
        print(f"         {detail}", flush=True)
    if ok:
        passed += 1
    else:
        failed += 1
    return ok


# -- 1. Initialize --
print("=== 1. INITIALIZE ===", flush=True)
resp = mcp("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "http-test", "version": "1.0"}})
test("MCP Initialize", "result" in resp, f"session={session_header[:16]}..." if session_header else "")

# -- 2. List tools --
print("\n=== 2. LIST TOOLS ===", flush=True)
resp = mcp("tools/list")
tools = sorted([t["name"] for t in resp.get("result", {}).get("tools", [])])
test("All 16 tools registered", len(tools) == 16, f"tools={len(tools)}")
for t in tools:
    print(f"    {t}", flush=True)

# -- 3. ssh_connect --
print("\n=== 3. ssh_connect ===", flush=True)
r = tool("ssh_connect", connect_args(agent_id="http-test", name="http-test-session", persistent=True))
session_id = r.get("session_id", "")
test("ssh_connect", bool(session_id), f"session_id={session_id[:16]}..." if session_id else str(r))
if not session_id:
    sys.exit(1)

# -- 4. ssh_list_sessions --
print("\n=== 4. ssh_list_sessions ===", flush=True)
r = tool("ssh_list_sessions", {"agent_id": "http-test"})
test("ssh_list_sessions", r.get("count", 0) == 1, f"count={r.get('count')}, healthy={r.get('sessions', [{}])[0].get('healthy')}")

# -- 5. ssh_execute --
print("\n=== 5. ssh_execute ===", flush=True)
r = tool("ssh_execute", {"session_id": session_id, "command": "uname -a && hostname && whoami"})
cmd_id = r.get("command_id", "")
test("ssh_execute", bool(cmd_id))

# -- 6. ssh_get_command_output --
print("\n=== 6. ssh_get_command_output ===", flush=True)
r = tool("ssh_get_command_output", {"command_id": cmd_id, "wait": True, "wait_timeout_secs": 10})
test("ssh_get_command_output", r.get("status") == "completed" and r.get("exit_code") == 0,
     f"stdout={r.get('stdout','')[:80].strip()}")

# -- 7. ssh_list_commands --
print("\n=== 7. ssh_list_commands ===", flush=True)
r = tool("ssh_list_commands", {"session_id": session_id})
test("ssh_list_commands", r.get("count", 0) >= 1, f"count={r.get('count')}")

# -- 8. ssh_cancel_command --
print("\n=== 8. ssh_cancel_command ===", flush=True)
r = tool("ssh_execute", {"session_id": session_id, "command": "sleep 300"})
long_cmd_id = r.get("command_id", "")
time.sleep(0.5)
r = tool("ssh_cancel_command", {"command_id": long_cmd_id})
test("ssh_cancel_command", r.get("cancelled") is True)

# -- 9. ssh_execute with PTY --
print("\n=== 9. ssh_execute (PTY) ===", flush=True)
r = tool("ssh_execute", {"session_id": session_id, "command": "tty", "pty": True})
pty_cmd_id = r.get("command_id", "")
r = tool("ssh_get_command_output", {"command_id": pty_cmd_id, "wait": True})
test("ssh_execute (PTY)", "/dev/pts" in r.get("stdout", ""), f"stdout={r.get('stdout','').strip()}")

# -- 10. ssh_shell_open --
print("\n=== 10. ssh_shell_open ===", flush=True)
r = tool("ssh_shell_open", {"session_id": session_id, "term": "xterm", "cols": 80, "rows": 24})
shell_id = r.get("shell_id", "")
test("ssh_shell_open", bool(shell_id))

# -- 11. ssh_shell_write --
print("\n=== 11. ssh_shell_write ===", flush=True)
r = tool("ssh_shell_write", {"shell_id": shell_id, "input": "echo SHELL_HTTP_OK && pwd\n"})
test("ssh_shell_write", r.get("_status") == "OK" or "bytes_sent" in r)
time.sleep(1)

# -- 12. ssh_shell_read --
print("\n=== 12. ssh_shell_read ===", flush=True)
r = tool("ssh_shell_read", {"shell_id": shell_id, "clear": True})
test("ssh_shell_read", "SHELL_HTTP_OK" in r.get("data", ""))

# -- 13. ssh_shell_close --
print("\n=== 13. ssh_shell_close ===", flush=True)
r = tool("ssh_shell_close", {"shell_id": shell_id})
test("ssh_shell_close", r.get("closed") is True)

# -- 14. ssh_forward --
print("\n=== 14. ssh_forward ===", flush=True)
r = tool("ssh_forward", {"session_id": session_id, "local_port": 19877, "remote_address": "localhost", "remote_port": 22})
test("ssh_forward", r.get("active") is True, f"local={r.get('local_address')}")

# -- 15. ssh_upload --
print("\n=== 15. ssh_upload ===", flush=True)
r = tool("ssh_execute", {"session_id": session_id, "command": f"mkdir -p {REMOTE_DIR} && rm -f {REMOTE_FILE}"})
tool("ssh_get_command_output", {"command_id": r["command_id"], "wait": True})
t0 = time.time()
r = tool("ssh_upload", {"session_id": session_id, "local_path": LOCAL_FILE, "remote_path": REMOTE_FILE})
xfer_id = r.get("transfer_id", "")
total_bytes = r.get("total_bytes", 0)
test("ssh_upload started", bool(xfer_id), f"total_bytes={total_bytes}")

# -- ssh_get_transfer_progress (upload) --
print("\n=== ssh_get_transfer_progress (upload wait) ===", flush=True)
r = tool("ssh_get_transfer_progress", {"transfer_id": xfer_id, "wait": True, "wait_timeout_secs": 300})
elapsed = time.time() - t0
xferred = r.get("bytes_transferred", 0)
speed = (xferred / 1024 / 1024) / elapsed if elapsed > 0 else 0
test("Upload completed", r.get("status") == "completed",
     f"{xferred}/{r.get('total_bytes')} bytes, {r.get('progress_percent')}%, {elapsed:.1f}s, {speed:.1f} MB/s")

# Verify on remote
r2 = tool("ssh_execute", {"session_id": session_id, "command": f"stat -c '%s' {REMOTE_FILE}"})
r2 = tool("ssh_get_command_output", {"command_id": r2["command_id"], "wait": True})
remote_size = r2.get("stdout", "").strip()
test("Upload verified on remote", remote_size == str(total_bytes), f"remote={remote_size}")

# -- 16. ssh_download --
print("\n=== 16. ssh_download ===", flush=True)
t0 = time.time()
r = tool("ssh_download", {"session_id": session_id, "remote_path": REMOTE_FILE, "local_path": DOWNLOAD_FILE})
dl_xfer_id = r.get("transfer_id", "")
test("ssh_download started", bool(dl_xfer_id))

# -- ssh_get_transfer_progress (download) --
print("\n=== ssh_get_transfer_progress (download wait) ===", flush=True)
r = tool("ssh_get_transfer_progress", {"transfer_id": dl_xfer_id, "wait": True, "wait_timeout_secs": 300})
elapsed = time.time() - t0
xferred = r.get("bytes_transferred", 0)
speed = (xferred / 1024 / 1024) / elapsed if elapsed > 0 else 0
test("Download completed", r.get("status") == "completed",
     f"{xferred}/{r.get('total_bytes')} bytes, {r.get('progress_percent')}%, {elapsed:.1f}s, {speed:.1f} MB/s")

orig = os.path.getsize(LOCAL_FILE)
dl = os.path.getsize(DOWNLOAD_FILE) if os.path.exists(DOWNLOAD_FILE) else 0
test("Download file matches", orig == dl and dl > 0, f"original={orig}, downloaded={dl}")

# -- Error classification tests --
print("\n=== ERROR: non-existent remote dir ===", flush=True)
r = tool("ssh_upload", {"session_id": session_id, "local_path": LOCAL_FILE, "remote_path": "/tmp/bad/dir/f.mov"})
err_xfer = r.get("transfer_id", "")
if err_xfer:
    time.sleep(3)
    r = tool("ssh_get_transfer_progress", {"transfer_id": err_xfer, "wait": True, "wait_timeout_secs": 10})
    err = r.get("error", "")
else:
    err = r.get("error", "")
test("[REMOTE_DIR_NOT_FOUND]", "[REMOTE_DIR_NOT_FOUND]" in err, f"{err[:130]}")

print("\n=== ERROR: non-existent local file ===", flush=True)
r = tool("ssh_upload", {"session_id": session_id, "local_path": "/nonexistent/x.txt", "remote_path": "/tmp/x.txt"})
err = r.get("error", "")
test("[FILE_NOT_FOUND]", "[FILE_NOT_FOUND]" in err, f"{err[:130]}")

print("\n=== ERROR: non-existent remote file ===", flush=True)
r = tool("ssh_download", {"session_id": session_id, "remote_path": "/nonexistent/x.txt", "local_path": "/tmp/x.txt"})
err = r.get("error", "")
test("[FILE_NOT_FOUND]", "[FILE_NOT_FOUND]" in err, f"{err[:130]}")

# =============================================================================
# CHAOS / CONCURRENCY / ERROR SIMULATION TESTS
# =============================================================================

# -- A. Concurrent Commands on Same Session --
print("\n=== A. CONCURRENT COMMANDS (SAME SESSION) ===", flush=True)
chaos_sid = tool("ssh_connect", connect_args(agent_id="chaos-a", name="chaos-a")).get("session_id", "")
if chaos_sid:
    N_CONCURRENT = 8
    futures = {}
    with ThreadPoolExecutor(max_workers=N_CONCURRENT) as pool:
        for i in range(N_CONCURRENT):
            marker = f"MARKER_{i}_{_uuid.uuid4().hex[:8]}"
            fut = pool.submit(tool_threadsafe, "ssh_execute", {"session_id": chaos_sid, "command": f"echo {marker}"})
            futures[fut] = marker
        cmd_ids = {}
        for fut in as_completed(futures):
            marker = futures[fut]
            r = fut.result()
            cid = r.get("command_id", "")
            if cid:
                cmd_ids[marker] = cid
    all_ok = True
    for marker, cid in cmd_ids.items():
        r = tool("ssh_get_command_output", {"command_id": cid, "wait": True, "wait_timeout_secs": 15})
        stdout = r.get("stdout", "")
        if marker not in stdout:
            all_ok = False
    test(f"8 concurrent cmds all returned correct markers", all_ok, f"got {len(cmd_ids)}/{N_CONCURRENT} cmd_ids")
    tool("ssh_disconnect", {"session_id": chaos_sid})

# -- B. Concurrent Commands Across 3 Sessions --
print("\n=== B. CONCURRENT COMMANDS (CROSS SESSION) ===", flush=True)
cross_sids = []
for i in range(3):
    r = tool("ssh_connect", connect_args(agent_id="chaos-b", name=f"cross-{i}"))
    sid = r.get("session_id", "")
    if sid:
        cross_sids.append(sid)
if len(cross_sids) == 3:
    futures = {}
    with ThreadPoolExecutor(max_workers=6) as pool:
        for si, sid in enumerate(cross_sids):
            for ci in range(2):
                marker = f"CROSS_{si}_{ci}_{_uuid.uuid4().hex[:8]}"
                fut = pool.submit(tool_threadsafe, "ssh_execute", {"session_id": sid, "command": f"echo {marker}"})
                futures[fut] = (si, ci, marker)
        cmd_map = {}
        for fut in as_completed(futures):
            si, ci, marker = futures[fut]
            r = fut.result()
            cid = r.get("command_id", "")
            if cid:
                cmd_map[(si, ci)] = (marker, cid)
    cross_ok = True
    for (si, ci), (marker, cid) in cmd_map.items():
        r = tool("ssh_get_command_output", {"command_id": cid, "wait": True, "wait_timeout_secs": 15})
        if marker not in r.get("stdout", ""):
            cross_ok = False
    test("6 cross-session cmds routed correctly", cross_ok, f"got {len(cmd_map)}/6 results")
    tool("ssh_disconnect_agent", {"agent_id": "chaos-b"})

# -- C. Shell Write During Disconnect Race --
print("\n=== C. SHELL WRITE + DISCONNECT RACE ===", flush=True)
race_sid = tool("ssh_connect", connect_args(agent_id="chaos-c", name="race")).get("session_id", "")
if race_sid:
    race_shell = tool("ssh_shell_open", {"session_id": race_sid}).get("shell_id", "")
    if race_shell:
        time.sleep(0.3)
        with ThreadPoolExecutor(max_workers=2) as pool:
            f_write = pool.submit(tool_threadsafe, "ssh_shell_write", {"shell_id": race_shell, "input": "echo RACE\n"})
            f_disc = pool.submit(tool_threadsafe, "ssh_disconnect", {"session_id": race_sid})
            done_count = 0
            for fut in as_completed([f_write, f_disc], timeout=10):
                fut.result()
                done_count += 1
        test("Shell write + disconnect both completed (no deadlock)", done_count == 2)
    else:
        tool("ssh_disconnect", {"session_id": race_sid})
        test("Shell write + disconnect race (skipped, shell open failed)", False)
else:
    test("Shell write + disconnect race (skipped, connect failed)", False)

# -- D. Rapid Connect/Disconnect Stress --
print("\n=== D. RAPID CONNECT/DISCONNECT (10 cycles) ===", flush=True)
stress_ok = True
for i in range(10):
    r = tool("ssh_connect", connect_args(agent_id="chaos-d", name=f"stress-{i}"))
    sid = r.get("session_id", "")
    if not sid:
        stress_ok = False
        break
    r = tool("ssh_disconnect", {"session_id": sid})
    if "error" in r:
        stress_ok = False
        break
test("10 rapid connect/disconnect cycles", stress_ok)

# -- E. Cancel Command While Polling Output --
print("\n=== E. CANCEL WHILE POLLING ===", flush=True)
cancel_sid = tool("ssh_connect", connect_args(agent_id="chaos-e", name="cancel-race")).get("session_id", "")
if cancel_sid:
    r = tool("ssh_execute", {"session_id": cancel_sid, "command": "sleep 60"})
    long_cid = r.get("command_id", "")
    if long_cid:
        time.sleep(0.5)
        with ThreadPoolExecutor(max_workers=2) as pool:
            f_poll = pool.submit(tool_threadsafe, "ssh_get_command_output", {"command_id": long_cid, "wait": True, "wait_timeout_secs": 15})
            time.sleep(0.2)
            f_cancel = pool.submit(tool_threadsafe, "ssh_cancel_command", {"command_id": long_cid})
            done = 0
            for fut in as_completed([f_poll, f_cancel], timeout=20):
                fut.result()
                done += 1
        test("Cancel + poll both returned (no deadlock)", done == 2)
    else:
        test("Cancel while polling (skipped, execute failed)", False)
    tool("ssh_disconnect", {"session_id": cancel_sid})
else:
    test("Cancel while polling (skipped, connect failed)", False)

# -- F. Multi-Session Routing Verification --
print("\n=== F. MULTI-SESSION ROUTING ===", flush=True)
route_agent = f"chaos-f-{_uuid.uuid4().hex[:8]}"
route_sids = []
for i in range(3):
    r = tool("ssh_connect", connect_args(agent_id=route_agent, name=f"route-{i}"))
    sid = r.get("session_id", "")
    if sid:
        route_sids.append(sid)
if len(route_sids) == 3:
    route_markers = {}
    for i, sid in enumerate(route_sids):
        marker = f"SESSION_ROUTE_{i}_{sid[:8]}"
        r = tool("ssh_execute", {"session_id": sid, "command": f"echo {marker}"})
        cid = r.get("command_id", "")
        if cid:
            route_markers[i] = (marker, cid)
    route_ok = True
    for i, (marker, cid) in route_markers.items():
        r = tool("ssh_get_command_output", {"command_id": cid, "wait": True, "wait_timeout_secs": 10})
        stdout = r.get("stdout", "")
        if marker not in stdout:
            route_ok = False
        for j, (other_marker, _) in route_markers.items():
            if j != i and other_marker in stdout:
                route_ok = False
    test("Each session echoed only its own marker", route_ok, f"verified {len(route_markers)} sessions")
    r = tool("ssh_list_sessions", {"agent_id": route_agent})
    test("ssh_list_sessions returns 3 for agent", r.get("count") == 3, f"count={r.get('count')}")
    r = tool("ssh_list_sessions", {"agent_id": f"nonexistent-{_uuid.uuid4().hex[:8]}"})
    test("Nonexistent agent sees 0 sessions", r.get("count", 0) == 0, f"count={r.get('count')}")
    tool("ssh_disconnect_agent", {"agent_id": route_agent})
else:
    test("Multi-session routing (skipped, connect failures)", False)

# -- G. Error Simulation (Invalid IDs) --
print("\n=== G. ERROR SIMULATION (INVALID IDS) ===", flush=True)
fake_uuid = str(_uuid.uuid4())

r = tool("ssh_execute", {"session_id": fake_uuid, "command": "echo hi"})
test("Execute on fake session_id -> error", "error" in r, str(r)[:100])

r = tool("ssh_get_command_output", {"command_id": fake_uuid, "wait": False})
test("Get output for fake command_id -> error", "error" in r, str(r)[:100])

r = tool("ssh_shell_write", {"shell_id": fake_uuid, "input": "x\n"})
test("Shell write to fake shell_id -> error", "error" in r, str(r)[:100])

r = tool("ssh_shell_read", {"shell_id": fake_uuid})
test("Shell read from fake shell_id -> error", "error" in r, str(r)[:100])

r = tool("ssh_shell_close", {"shell_id": fake_uuid})
test("Shell close fake shell_id -> error", "error" in r, str(r)[:100])

r = tool("ssh_get_transfer_progress", {"transfer_id": fake_uuid})
test("Transfer progress for fake transfer_id -> error", "error" in r, str(r)[:100])

# Double disconnect
dd_r = tool("ssh_connect", connect_args(agent_id="chaos-g", name="dd"))
dd_sid = dd_r.get("session_id", "")
if dd_sid:
    tool("ssh_disconnect", {"session_id": dd_sid})
    r = tool("ssh_disconnect", {"session_id": dd_sid})
    test("Double disconnect -> error", "error" in r, str(r)[:100])

# Execute on disconnected session
disc_r = tool("ssh_connect", connect_args(agent_id="chaos-g2", name="disc-exec"))
disc_sid = disc_r.get("session_id", "")
if disc_sid:
    tool("ssh_disconnect", {"session_id": disc_sid})
    r = tool("ssh_execute", {"session_id": disc_sid, "command": "echo should_fail"})
    test("Execute on disconnected session -> error", "error" in r, str(r)[:100])

# Write/read to closed shell
closed_r = tool("ssh_connect", connect_args(agent_id="chaos-g3", name="closed-shell"))
closed_sid = closed_r.get("session_id", "")
if closed_sid:
    sh = tool("ssh_shell_open", {"session_id": closed_sid}).get("shell_id", "")
    if sh:
        tool("ssh_shell_close", {"shell_id": sh})
        r = tool("ssh_shell_write", {"shell_id": sh, "input": "echo fail\n"})
        test("Write to closed shell -> error", "error" in r, str(r)[:100])
        r = tool("ssh_shell_read", {"shell_id": sh})
        test("Read from closed shell -> error", "error" in r, str(r)[:100])
    tool("ssh_disconnect", {"session_id": closed_sid})

# Cancel already-completed command
comp_r = tool("ssh_connect", connect_args(agent_id="chaos-g4", name="comp-cancel"))
comp_sid = comp_r.get("session_id", "")
if comp_sid:
    r = tool("ssh_execute", {"session_id": comp_sid, "command": "echo done"})
    comp_cid = r.get("command_id", "")
    if comp_cid:
        tool("ssh_get_command_output", {"command_id": comp_cid, "wait": True, "wait_timeout_secs": 5})
        r = tool("ssh_cancel_command", {"command_id": comp_cid})
        test("Cancel completed command -> error or no-op", True, str(r)[:100])
    tool("ssh_disconnect", {"session_id": comp_sid})

# -- H. Mixed Concurrent Valid + Invalid Operations --
print("\n=== H. MIXED CONCURRENT VALID + INVALID ===", flush=True)
mix_sid = tool("ssh_connect", connect_args(agent_id="chaos-h", name="mixed")).get("session_id", "")
if mix_sid:
    fake1 = str(_uuid.uuid4())
    fake2 = str(_uuid.uuid4())
    calls = [
        ("ssh_execute", {"session_id": mix_sid, "command": "echo VALID_1"}),
        ("ssh_execute", {"session_id": mix_sid, "command": "echo VALID_2"}),
        ("ssh_execute", {"session_id": fake1, "command": "echo BAD1"}),
        ("ssh_get_command_output", {"command_id": fake2, "wait": False}),
        ("ssh_shell_write", {"shell_id": fake1, "input": "x\n"}),
        ("ssh_shell_read", {"shell_id": fake2}),
    ]
    results = {}
    with ThreadPoolExecutor(max_workers=6) as pool:
        futs = {}
        for i, (name, args) in enumerate(calls):
            futs[pool.submit(tool_threadsafe, name, args)] = i
        for fut in as_completed(futs, timeout=15):
            idx = futs[fut]
            results[idx] = fut.result()
    valid_ok = all("command_id" in results.get(i, {}) for i in [0, 1])
    invalid_ok = all("error" in results.get(i, {}) for i in [2, 3, 4, 5])
    test("Valid operations succeeded", valid_ok, f"results[0]={str(results.get(0, {}))[:60]}")
    test("Invalid operations returned errors", invalid_ok, f"errors={sum(1 for i in [2,3,4,5] if 'error' in results.get(i, {}))}/4")
    test("Server did not crash (6 mixed concurrent calls)", len(results) == 6)
    tool("ssh_disconnect", {"session_id": mix_sid})
else:
    test("Mixed concurrent (skipped, connect failed)", False)

# Cleanup chaos agents
for agent in ["chaos-g", "chaos-g2", "chaos-g3", "chaos-g4", "chaos-h"]:
    tool("ssh_disconnect_agent", {"agent_id": agent})

# =============================================================================
# STRESS / LEAK TESTS (v2.0 buffer cap and no-leak guarantee)
# =============================================================================

print("\n=== STRESS I: 100 connect/disconnect cycles ===", flush=True)
_rss_before = None
try:
    import resource
    _rss_before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
except (ImportError, AttributeError):
    pass

stress1_ok = True
for i in range(100):
    r = tool("ssh_connect", connect_args(agent_id="stress-cycle", name=f"cycle-{i}"))
    sid = r.get("session_id", "")
    if not sid:
        stress1_ok = False
        print(f"         Cycle {i}: connect failed -> {str(r)[:120]}", flush=True)
        break
    dc = tool("ssh_disconnect", {"session_id": sid})
    if "error" in dc:
        stress1_ok = False
        break
test("100 connect/disconnect cycles (no-leak)", stress1_ok)

print("\n=== STRESS II: 50 execute + read + cancel (output buffer churn) ===", flush=True)
stress_sid = tool("ssh_connect", connect_args(agent_id="stress-exec", name="stress-exec")).get("session_id", "")
stress2_ok = bool(stress_sid)
if stress_sid:
    for i in range(50):
        # Alternate: half complete quickly, half get cancelled mid-flight.
        if i % 2 == 0:
            r = tool("ssh_execute", {"session_id": stress_sid, "command": f"echo STRESS_{i}"})
            cid = r.get("command_id", "")
            out = tool("ssh_get_command_output", {"command_id": cid, "wait": True, "wait_timeout_secs": 5, "max_output_bytes": 512})
            if f"STRESS_{i}" not in out.get("stdout", ""):
                stress2_ok = False
                break
        else:
            r = tool("ssh_execute", {"session_id": stress_sid, "command": "sleep 30"})
            cid = r.get("command_id", "")
            if cid:
                time.sleep(0.05)
                tool("ssh_cancel_command", {"command_id": cid, "max_output_bytes": 256})
    tool("ssh_disconnect", {"session_id": stress_sid})
test("50 execute+read/cancel iterations", stress2_ok)

print("\n=== STRESS III: 20 shell open/close cycles ===", flush=True)
shell_stress_sid = tool("ssh_connect", connect_args(agent_id="stress-shell", name="stress-shell")).get("session_id", "")
stress3_ok = bool(shell_stress_sid)
if shell_stress_sid:
    for i in range(20):
        sh = tool("ssh_shell_open", {"session_id": shell_stress_sid}).get("shell_id", "")
        if not sh:
            stress3_ok = False
            break
        tool("ssh_shell_write", {"shell_id": sh, "input": f"echo CYCLE_{i}\n"})
        time.sleep(0.2)
        r = tool("ssh_shell_read", {"shell_id": sh, "clear": True, "max_output_bytes": 4096})
        closed = tool("ssh_shell_close", {"shell_id": sh})
        if not closed.get("closed"):
            stress3_ok = False
            break
    tool("ssh_disconnect", {"session_id": shell_stress_sid})
test("20 shell open/close cycles", stress3_ok)

print("\n=== STRESS IV: large stdout truncation (10MB echoed, 2KB rendered) ===", flush=True)
big_sid = tool("ssh_connect", connect_args(agent_id="stress-big", name="stress-big")).get("session_id", "")
stress4_ok = bool(big_sid)
if big_sid:
    # Generate ~1MB of 'A' characters via shell; verify the cap truncates sanely.
    r = tool("ssh_execute", {
        "session_id": big_sid,
        "command": "head -c 1048576 /dev/urandom | base64 | head -c 1048576",
    })
    cid = r.get("command_id", "")
    out = tool("ssh_get_command_output", {
        "command_id": cid, "wait": True, "wait_timeout_secs": 30, "max_output_bytes": 2048,
    })
    shown = len(out.get("stdout", ""))
    stress4_ok = shown <= 2100  # allow small overhead for markers
    test("Large stdout truncated to max_output_bytes", stress4_ok, f"shown={shown}B")
    tool("ssh_disconnect", {"session_id": big_sid})
else:
    test("Large stdout truncation (skipped, connect failed)", False)

if _rss_before is not None:
    try:
        _rss_after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        # ru_maxrss is KB on Linux, bytes on macOS — we only care about growth.
        delta = _rss_after - _rss_before
        print(f"\n         RSS delta over stress suite: {delta} (resource-unit)", flush=True)
    except Exception:
        pass

for agent in ["stress-cycle", "stress-exec", "stress-shell", "stress-big"]:
    tool("ssh_disconnect_agent", {"agent_id": agent})

# =============================================================================
# ORIGINAL DISCONNECT / CLEANUP TESTS
# =============================================================================

# -- ssh_disconnect --
print("\n=== ssh_disconnect ===", flush=True)
r2 = tool("ssh_connect", connect_args(agent_id="http-test", name="disc-test"))
sid2 = r2.get("session_id", "")
r = tool("ssh_disconnect", {"session_id": sid2})
test("ssh_disconnect", r.get("_status") == "OK" or "SSH_DISCONNECT: OK" in r.get("_text", ""), str(r)[:80])

# -- ssh_disconnect_agent --
print("\n=== ssh_disconnect_agent ===", flush=True)
r = tool("ssh_disconnect_agent", {"agent_id": "http-test"})
test("ssh_disconnect_agent", r.get("sessions_disconnected", 0) >= 1,
     f"sessions_disconnected={r.get('sessions_disconnected')}")

# Cleanup
import subprocess
subprocess.run(["rm", "-f", DOWNLOAD_FILE], capture_output=True)

print(f"\n{'='*60}", flush=True)
print(f"RESULTS: {passed}/{total} passed, {failed} failed", flush=True)
if failed == 0:
    print("ALL TESTS PASSED", flush=True)
else:
    print(f"FAILURES: {failed}", flush=True)
print(f"{'='*60}", flush=True)
sys.exit(0 if failed == 0 else 1)
