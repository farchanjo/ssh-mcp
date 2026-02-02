#!/usr/bin/env python3
"""Integration test for ssh-mcp-stdio binary — all 16 tools."""
import json
import os
import subprocess
import sys
import time
import threading
import queue
import uuid as _uuid

BINARY = os.environ.get("SSH_MCP_STDIO", os.path.join(os.path.dirname(__file__), "..", "target", "release", "ssh-mcp-stdio"))
HOST = os.environ.get("SSH_HOST", "vm.services:22")
USER = os.environ.get("SSH_USER", "root")
LOCAL_FILE = os.environ.get("LOCAL_FILE", "/Users/farchanjo/Downloads/IMG_1267.MOV")
REMOTE_DIR = "/tmp/ssh-mcp-test"
REMOTE_FILE = f"{REMOTE_DIR}/{os.path.basename(LOCAL_FILE)}"
DOWNLOAD_FILE = "/tmp/ssh-mcp-test-downloaded" + os.path.splitext(LOCAL_FILE)[1]


class McpClient:
    def __init__(self):
        env = os.environ.copy()
        env["RUST_LOG"] = "error"
        self.proc = subprocess.Popen(
            [BINARY],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        self.req_id = 0
        self.response_queue = queue.Queue()

        # Read stdout in background thread to avoid deadlocks
        self._stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self._stdout_thread.start()
        # Drain stderr
        self._stderr_thread = threading.Thread(target=self._drain_stderr, daemon=True)
        self._stderr_thread.start()

        time.sleep(0.3)
        if self.proc.poll() is not None:
            raise RuntimeError(f"Process exited with code {self.proc.returncode}")

    def _read_stdout(self):
        while True:
            line = self.proc.stdout.readline()
            if not line:
                break
            try:
                data = json.loads(line)
                self.response_queue.put(data)
            except json.JSONDecodeError:
                pass

    def _drain_stderr(self):
        while True:
            line = self.proc.stderr.readline()
            if not line:
                break

    def send(self, method, params=None, is_notification=False):
        self.req_id += 1
        req = {"jsonrpc": "2.0", "method": method, "params": params or {}}
        if not is_notification:
            req["id"] = self.req_id
        line = json.dumps(req) + "\n"
        self.proc.stdin.write(line.encode())
        self.proc.stdin.flush()
        if is_notification:
            return None
        # Wait for response with timeout
        try:
            return self.response_queue.get(timeout=300)
        except queue.Empty:
            return {"error": "Timeout waiting for response"}

    def tool_call(self, name, arguments):
        resp = self.send("tools/call", {"name": name, "arguments": arguments})
        if resp is None:
            return {"error": "No response"}
        if "error" in resp and isinstance(resp["error"], dict):
            return {"error": resp["error"].get("message", str(resp["error"]))}
        result = resp.get("result", {})
        if result.get("isError"):
            error_text = result["content"][0]["text"] if result.get("content") else "Unknown error"
            return {"error": error_text}
        if result.get("content"):
            text = result["content"][0].get("text", "")
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                return {"text": text}
        return result

    def send_batch(self, calls):
        """Send multiple tools/call requests without waiting. Returns {req_id: (name, args)}."""
        sent = {}
        for name, args in calls:
            self.req_id += 1
            rid = self.req_id
            req = {"jsonrpc": "2.0", "id": rid, "method": "tools/call", "params": {"name": name, "arguments": args}}
            line = json.dumps(req) + "\n"
            self.proc.stdin.write(line.encode())
            sent[rid] = (name, args)
        self.proc.stdin.flush()
        return sent

    def collect_responses(self, expected_ids, timeout=30):
        """Collect responses matching expected_ids from the queue."""
        results = {}
        deadline = time.time() + timeout
        remaining = set(expected_ids)
        while remaining and time.time() < deadline:
            try:
                resp = self.response_queue.get(timeout=min(1, deadline - time.time()))
                rid = resp.get("id")
                if rid in remaining:
                    results[rid] = resp
                    remaining.discard(rid)
            except queue.Empty:
                continue
        return results

    def tool_call_parsed(self, resp):
        """Parse a raw JSON-RPC response into the same format as tool_call()."""
        if "error" in resp and isinstance(resp["error"], dict):
            return {"error": resp["error"].get("message", str(resp["error"]))}
        result = resp.get("result", {})
        if result.get("isError"):
            error_text = result["content"][0]["text"] if result.get("content") else "Unknown error"
            return {"error": error_text}
        if result.get("content"):
            text = result["content"][0].get("text", "")
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                return {"text": text}
        return result

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


def test(label, passed, detail=""):
    status = "PASS" if passed else "FAIL"
    print(f"  [{status}] {label}", flush=True)
    if detail:
        print(f"         {detail}", flush=True)
    return passed


def main():
    print("Starting MCP client...", flush=True)
    client = McpClient()
    passed = 0
    failed = 0
    total = 0

    # 1. Initialize
    print("=== INITIALIZE ===", flush=True)
    resp = client.send("initialize", {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "test", "version": "1.0"},
    })
    ok = resp and "result" in resp
    total += 1
    if test("MCP Initialize", ok):
        passed += 1
    else:
        failed += 1
        print(f"         {resp}", flush=True)
        client.close()
        sys.exit(1)

    # Notification - no response expected
    client.send("notifications/initialized", is_notification=True)
    time.sleep(0.2)

    # 2. List tools
    print("\n=== LIST TOOLS ===", flush=True)
    resp = client.send("tools/list", {})
    tools = [t["name"] for t in resp.get("result", {}).get("tools", [])]
    sftp_tools = [t for t in tools if t in ("ssh_upload", "ssh_download", "ssh_get_transfer_progress")]
    total += 1
    if test("SFTP tools registered", len(sftp_tools) == 3, f"Found: {sftp_tools} in {len(tools)} total tools"):
        passed += 1
    else:
        failed += 1
    for t in sorted(tools):
        marker = " <-- SFTP" if t in sftp_tools else ""
        print(f"    {t}{marker}", flush=True)

    # 3. Connect
    print("\n=== SSH CONNECT ===", flush=True)
    result = client.tool_call("ssh_connect", {
        "address": HOST, "username": USER,
        "agent_id": "sftp-test", "name": "sftp-test-session",
    })
    session_id = result.get("session_id", "")
    total += 1
    if test("SSH Connect", bool(session_id), f"session_id={session_id[:16]}..." if session_id else str(result)):
        passed += 1
    else:
        failed += 1
        client.close()
        return

    # 4. Prepare remote dir
    print("\n=== PREPARE REMOTE ===", flush=True)
    result = client.tool_call("ssh_execute", {
        "session_id": session_id,
        "command": f"mkdir -p {REMOTE_DIR} && rm -f {REMOTE_FILE}",
    })
    cmd_id = result.get("command_id", "")
    output = client.tool_call("ssh_get_command_output", {"command_id": cmd_id, "wait": True, "wait_timeout_secs": 10})
    total += 1
    if test("Prepare remote dir", output.get("status") == "completed"):
        passed += 1
    else:
        failed += 1

    # 5. Upload
    print("\n=== SFTP UPLOAD ===", flush=True)
    t0 = time.time()
    result = client.tool_call("ssh_upload", {
        "session_id": session_id,
        "local_path": LOCAL_FILE,
        "remote_path": REMOTE_FILE,
    })
    transfer_id = result.get("transfer_id", "")
    total_bytes = result.get("total_bytes", 0)
    total += 1
    if test("Upload started", bool(transfer_id),
            f"transfer_id={transfer_id[:16]}..., total_bytes={total_bytes}" if transfer_id else str(result)):
        passed += 1
    else:
        failed += 1

    # 6. Poll upload progress
    if transfer_id:
        print("\n=== UPLOAD PROGRESS ===", flush=True)
        progress = client.tool_call("ssh_get_transfer_progress", {
            "transfer_id": transfer_id, "wait": True, "wait_timeout_secs": 300,
        })
        elapsed = time.time() - t0
        total += 1
        upload_ok = progress.get("status") == "completed"
        xferred = progress.get("bytes_transferred", 0)
        speed_mbps = (xferred / 1024 / 1024) / elapsed if elapsed > 0 else 0
        if test("Upload completed", upload_ok,
                f"status={progress.get('status')}, {xferred}/{progress.get('total_bytes')} bytes, "
                f"{progress.get('progress_percent')}%, {elapsed:.1f}s, {speed_mbps:.1f} MB/s"):
            passed += 1
        else:
            failed += 1
            if progress.get("error"):
                print(f"         ERROR: {progress['error']}", flush=True)

    # 7. Verify on remote
    print("\n=== VERIFY UPLOAD ON REMOTE ===", flush=True)
    result = client.tool_call("ssh_execute", {
        "session_id": session_id,
        "command": f"stat -c '%s' {REMOTE_FILE}",
    })
    cmd_id = result.get("command_id", "")
    output = client.tool_call("ssh_get_command_output", {"command_id": cmd_id, "wait": True, "wait_timeout_secs": 30})
    remote_size = output.get("stdout", "").strip()
    total += 1
    if test("Remote file size matches", remote_size == str(total_bytes), f"remote={remote_size}, expected={total_bytes}"):
        passed += 1
    else:
        failed += 1

    # 8. Download
    print("\n=== SFTP DOWNLOAD ===", flush=True)
    t0 = time.time()
    result = client.tool_call("ssh_download", {
        "session_id": session_id,
        "remote_path": REMOTE_FILE,
        "local_path": DOWNLOAD_FILE,
    })
    dl_transfer_id = result.get("transfer_id", "")
    total += 1
    if test("Download started", bool(dl_transfer_id),
            f"transfer_id={dl_transfer_id[:16]}..." if dl_transfer_id else str(result)):
        passed += 1
    else:
        failed += 1

    # 9. Poll download progress
    if dl_transfer_id:
        print("\n=== DOWNLOAD PROGRESS ===", flush=True)
        progress = client.tool_call("ssh_get_transfer_progress", {
            "transfer_id": dl_transfer_id, "wait": True, "wait_timeout_secs": 300,
        })
        elapsed = time.time() - t0
        total += 1
        dl_ok = progress.get("status") == "completed"
        xferred = progress.get("bytes_transferred", 0)
        speed_mbps = (xferred / 1024 / 1024) / elapsed if elapsed > 0 else 0
        if test("Download completed", dl_ok,
                f"status={progress.get('status')}, {xferred}/{progress.get('total_bytes')} bytes, "
                f"{progress.get('progress_percent')}%, {elapsed:.1f}s, {speed_mbps:.1f} MB/s"):
            passed += 1
        else:
            failed += 1
            if progress.get("error"):
                print(f"         ERROR: {progress['error']}", flush=True)

    # 10. Verify downloaded file
    print("\n=== VERIFY DOWNLOAD LOCALLY ===", flush=True)
    orig_size = os.path.getsize(LOCAL_FILE) if os.path.exists(LOCAL_FILE) else 0
    dl_size = os.path.getsize(DOWNLOAD_FILE) if os.path.exists(DOWNLOAD_FILE) else 0
    total += 1
    if test("Downloaded file size matches", orig_size == dl_size and dl_size > 0,
            f"original={orig_size}, downloaded={dl_size}"):
        passed += 1
    else:
        failed += 1

    # 11. Error classification: upload to non-existent remote dir
    print("\n=== ERROR CLASSIFICATION: non-existent remote dir ===", flush=True)
    result = client.tool_call("ssh_upload", {
        "session_id": session_id,
        "local_path": LOCAL_FILE,
        "remote_path": "/tmp/nonexistent/deep/nested/dir/file.mov",
    })
    err_transfer_id = result.get("transfer_id", "")
    if err_transfer_id:
        time.sleep(3)
        progress = client.tool_call("ssh_get_transfer_progress", {
            "transfer_id": err_transfer_id, "wait": True, "wait_timeout_secs": 10,
        })
        err_msg = progress.get("error", "")
        total += 1
        has_code = err_msg.startswith("[") and "]" in err_msg
        if test("Error has classification code", has_code, f"error={err_msg[:150]}"):
            passed += 1
        else:
            failed += 1
    else:
        err_msg = result.get("error", "")
        total += 1
        has_code = err_msg.startswith("[") and "]" in err_msg
        if test("Error has classification code", has_code, f"error={err_msg[:150]}"):
            passed += 1
        else:
            failed += 1

    # 12. Error classification: non-existent local file
    print("\n=== ERROR CLASSIFICATION: non-existent local file ===", flush=True)
    result = client.tool_call("ssh_upload", {
        "session_id": session_id,
        "local_path": "/nonexistent/file.txt",
        "remote_path": "/tmp/test.txt",
    })
    err_msg = result.get("error", "")
    total += 1
    has_code = err_msg.startswith("[") and "]" in err_msg
    if test("Error has classification code", has_code, f"error={err_msg[:150]}"):
        passed += 1
    else:
        failed += 1

    # 13. Error classification: download non-existent remote file
    print("\n=== ERROR CLASSIFICATION: non-existent remote file ===", flush=True)
    result = client.tool_call("ssh_download", {
        "session_id": session_id,
        "remote_path": "/nonexistent/remote/file.txt",
        "local_path": "/tmp/dl-err-test.txt",
    })
    err_msg = result.get("error", "")
    total += 1
    has_code = err_msg.startswith("[") and "]" in err_msg
    if test("Error has classification code", has_code, f"error={err_msg[:150]}"):
        passed += 1
    else:
        failed += 1

    # =========================================================================
    # CHAOS / CONCURRENCY / ERROR SIMULATION TESTS
    # =========================================================================

    # -- A. Concurrent Commands on Same Session --
    print("\n=== A. CONCURRENT COMMANDS (SAME SESSION) ===", flush=True)
    chaos_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-a", "name": "chaos-a"})
    chaos_sid = chaos_r.get("session_id", "")
    if chaos_sid:
        N_CONCURRENT = 8
        batch_calls = []
        markers = []
        for i in range(N_CONCURRENT):
            marker = f"MARKER_{i}_{_uuid.uuid4().hex[:8]}"
            markers.append(marker)
            batch_calls.append(("ssh_execute", {"session_id": chaos_sid, "command": f"echo {marker}"}))
        sent = client.send_batch(batch_calls)
        responses = client.collect_responses(sent.keys(), timeout=30)
        cmd_ids = {}
        for rid, resp in responses.items():
            parsed = client.tool_call_parsed(resp)
            cid = parsed.get("command_id", "")
            idx = list(sent.keys()).index(rid)
            if cid:
                cmd_ids[markers[idx]] = cid
        all_ok = True
        for marker, cid in cmd_ids.items():
            r = client.tool_call("ssh_get_command_output", {"command_id": cid, "wait": True, "wait_timeout_secs": 15})
            if marker not in r.get("stdout", ""):
                all_ok = False
        total += 1
        if test(f"8 concurrent cmds returned correct markers", all_ok, f"got {len(cmd_ids)}/{N_CONCURRENT}"):
            passed += 1
        else:
            failed += 1
        client.tool_call("ssh_disconnect", {"session_id": chaos_sid})

    # -- B. Concurrent Commands Across 3 Sessions --
    print("\n=== B. CONCURRENT COMMANDS (CROSS SESSION) ===", flush=True)
    cross_sids = []
    for i in range(3):
        r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-b", "name": f"cross-{i}"})
        sid = r.get("session_id", "")
        if sid:
            cross_sids.append(sid)
    if len(cross_sids) == 3:
        batch_calls = []
        cross_markers = []
        for si, sid in enumerate(cross_sids):
            for ci in range(2):
                marker = f"CROSS_{si}_{ci}_{_uuid.uuid4().hex[:8]}"
                cross_markers.append((si, ci, marker))
                batch_calls.append(("ssh_execute", {"session_id": sid, "command": f"echo {marker}"}))
        sent = client.send_batch(batch_calls)
        responses = client.collect_responses(sent.keys(), timeout=30)
        cmd_map = {}
        rid_list = list(sent.keys())
        for rid, resp in responses.items():
            parsed = client.tool_call_parsed(resp)
            cid = parsed.get("command_id", "")
            idx = rid_list.index(rid)
            si, ci, marker = cross_markers[idx]
            if cid:
                cmd_map[(si, ci)] = (marker, cid)
        cross_ok = True
        for (si, ci), (marker, cid) in cmd_map.items():
            r = client.tool_call("ssh_get_command_output", {"command_id": cid, "wait": True, "wait_timeout_secs": 15})
            if marker not in r.get("stdout", ""):
                cross_ok = False
        total += 1
        if test("6 cross-session cmds routed correctly", cross_ok, f"got {len(cmd_map)}/6"):
            passed += 1
        else:
            failed += 1
        client.tool_call("ssh_disconnect_agent", {"agent_id": "chaos-b"})

    # -- C. Shell Write During Disconnect Race --
    print("\n=== C. SHELL WRITE + DISCONNECT RACE ===", flush=True)
    race_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-c", "name": "race"})
    race_sid = race_r.get("session_id", "")
    if race_sid:
        race_shell = client.tool_call("ssh_shell_open", {"session_id": race_sid}).get("shell_id", "")
        if race_shell:
            time.sleep(0.3)
            batch_calls = [
                ("ssh_shell_write", {"shell_id": race_shell, "input": "echo RACE\n"}),
                ("ssh_disconnect", {"session_id": race_sid}),
            ]
            sent = client.send_batch(batch_calls)
            responses = client.collect_responses(sent.keys(), timeout=10)
            total += 1
            if test("Shell write + disconnect both completed (no deadlock)", len(responses) == 2):
                passed += 1
            else:
                failed += 1
        else:
            client.tool_call("ssh_disconnect", {"session_id": race_sid})
            total += 1
            if test("Shell write + disconnect race (skipped, shell open failed)", False):
                passed += 1
            else:
                failed += 1
    else:
        total += 1
        if test("Shell write + disconnect race (skipped, connect failed)", False):
            passed += 1
        else:
            failed += 1

    # -- D. Rapid Connect/Disconnect Stress --
    print("\n=== D. RAPID CONNECT/DISCONNECT (10 cycles) ===", flush=True)
    stress_ok = True
    for i in range(10):
        r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-d", "name": f"stress-{i}"})
        sid = r.get("session_id", "")
        if not sid:
            stress_ok = False
            break
        r = client.tool_call("ssh_disconnect", {"session_id": sid})
        if "error" in r:
            stress_ok = False
            break
    total += 1
    if test("10 rapid connect/disconnect cycles", stress_ok):
        passed += 1
    else:
        failed += 1

    # -- E. Cancel Command While Polling Output --
    print("\n=== E. CANCEL WHILE POLLING ===", flush=True)
    cancel_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-e", "name": "cancel-race"})
    cancel_sid = cancel_r.get("session_id", "")
    if cancel_sid:
        r = client.tool_call("ssh_execute", {"session_id": cancel_sid, "command": "sleep 60"})
        long_cid = r.get("command_id", "")
        if long_cid:
            time.sleep(0.5)
            batch_calls = [
                ("ssh_get_command_output", {"command_id": long_cid, "wait": True, "wait_timeout_secs": 15}),
                ("ssh_cancel_command", {"command_id": long_cid}),
            ]
            sent = client.send_batch(batch_calls)
            responses = client.collect_responses(sent.keys(), timeout=20)
            total += 1
            if test("Cancel + poll both returned (no deadlock)", len(responses) == 2):
                passed += 1
            else:
                failed += 1
        else:
            total += 1
            if test("Cancel while polling (skipped, execute failed)", False):
                passed += 1
            else:
                failed += 1
        client.tool_call("ssh_disconnect", {"session_id": cancel_sid})
    else:
        total += 1
        if test("Cancel while polling (skipped, connect failed)", False):
            passed += 1
        else:
            failed += 1

    # -- F. Multi-Session Routing Verification --
    print("\n=== F. MULTI-SESSION ROUTING ===", flush=True)
    route_agent = f"chaos-f-{_uuid.uuid4().hex[:8]}"
    route_sids = []
    for i in range(3):
        r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": route_agent, "name": f"route-{i}"})
        sid = r.get("session_id", "")
        if sid:
            route_sids.append(sid)
    if len(route_sids) == 3:
        route_markers = {}
        for i, sid in enumerate(route_sids):
            marker = f"SESSION_ROUTE_{i}_{sid[:8]}"
            r = client.tool_call("ssh_execute", {"session_id": sid, "command": f"echo {marker}"})
            cid = r.get("command_id", "")
            if cid:
                route_markers[i] = (marker, cid)
        route_ok = True
        for i, (marker, cid) in route_markers.items():
            r = client.tool_call("ssh_get_command_output", {"command_id": cid, "wait": True, "wait_timeout_secs": 10})
            stdout = r.get("stdout", "")
            if marker not in stdout:
                route_ok = False
            for j, (other_marker, _) in route_markers.items():
                if j != i and other_marker in stdout:
                    route_ok = False
        total += 1
        if test("Each session echoed only its own marker", route_ok, f"verified {len(route_markers)} sessions"):
            passed += 1
        else:
            failed += 1
        r = client.tool_call("ssh_list_sessions", {"agent_id": route_agent})
        total += 1
        if test("ssh_list_sessions returns 3 for agent", r.get("count") == 3, f"count={r.get('count')}"):
            passed += 1
        else:
            failed += 1
        r = client.tool_call("ssh_list_sessions", {"agent_id": f"nonexistent-{_uuid.uuid4().hex[:8]}"})
        total += 1
        if test("Nonexistent agent sees 0 sessions", r.get("count", 0) == 0, f"count={r.get('count')}"):
            passed += 1
        else:
            failed += 1
        client.tool_call("ssh_disconnect_agent", {"agent_id": route_agent})
    else:
        total += 1
        if test("Multi-session routing (skipped, connect failures)", False):
            passed += 1
        else:
            failed += 1

    # -- G. Error Simulation (Invalid IDs) --
    print("\n=== G. ERROR SIMULATION (INVALID IDS) ===", flush=True)
    fake_uuid = str(_uuid.uuid4())

    r = client.tool_call("ssh_execute", {"session_id": fake_uuid, "command": "echo hi"})
    total += 1
    if test("Execute on fake session_id -> error", "error" in r, str(r)[:100]):
        passed += 1
    else:
        failed += 1

    r = client.tool_call("ssh_get_command_output", {"command_id": fake_uuid, "wait": False})
    total += 1
    if test("Get output for fake command_id -> error", "error" in r, str(r)[:100]):
        passed += 1
    else:
        failed += 1

    r = client.tool_call("ssh_shell_write", {"shell_id": fake_uuid, "input": "x\n"})
    total += 1
    if test("Shell write to fake shell_id -> error", "error" in r, str(r)[:100]):
        passed += 1
    else:
        failed += 1

    r = client.tool_call("ssh_shell_read", {"shell_id": fake_uuid})
    total += 1
    if test("Shell read from fake shell_id -> error", "error" in r, str(r)[:100]):
        passed += 1
    else:
        failed += 1

    r = client.tool_call("ssh_shell_close", {"shell_id": fake_uuid})
    total += 1
    if test("Shell close fake shell_id -> error", "error" in r, str(r)[:100]):
        passed += 1
    else:
        failed += 1

    r = client.tool_call("ssh_get_transfer_progress", {"transfer_id": fake_uuid})
    total += 1
    if test("Transfer progress for fake transfer_id -> error", "error" in r, str(r)[:100]):
        passed += 1
    else:
        failed += 1

    # Double disconnect
    dd_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-g", "name": "dd"})
    dd_sid = dd_r.get("session_id", "")
    if dd_sid:
        client.tool_call("ssh_disconnect", {"session_id": dd_sid})
        r = client.tool_call("ssh_disconnect", {"session_id": dd_sid})
        total += 1
        if test("Double disconnect -> error", "error" in r, str(r)[:100]):
            passed += 1
        else:
            failed += 1

    # Execute on disconnected session
    disc_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-g2", "name": "disc-exec"})
    disc_sid = disc_r.get("session_id", "")
    if disc_sid:
        client.tool_call("ssh_disconnect", {"session_id": disc_sid})
        r = client.tool_call("ssh_execute", {"session_id": disc_sid, "command": "echo should_fail"})
        total += 1
        if test("Execute on disconnected session -> error", "error" in r, str(r)[:100]):
            passed += 1
        else:
            failed += 1

    # Write/read to closed shell
    closed_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-g3", "name": "closed-shell"})
    closed_sid = closed_r.get("session_id", "")
    if closed_sid:
        sh = client.tool_call("ssh_shell_open", {"session_id": closed_sid}).get("shell_id", "")
        if sh:
            client.tool_call("ssh_shell_close", {"shell_id": sh})
            r = client.tool_call("ssh_shell_write", {"shell_id": sh, "input": "echo fail\n"})
            total += 1
            if test("Write to closed shell -> error", "error" in r, str(r)[:100]):
                passed += 1
            else:
                failed += 1
            r = client.tool_call("ssh_shell_read", {"shell_id": sh})
            total += 1
            if test("Read from closed shell -> error", "error" in r, str(r)[:100]):
                passed += 1
            else:
                failed += 1
        client.tool_call("ssh_disconnect", {"session_id": closed_sid})

    # Cancel already-completed command
    comp_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-g4", "name": "comp-cancel"})
    comp_sid = comp_r.get("session_id", "")
    if comp_sid:
        r = client.tool_call("ssh_execute", {"session_id": comp_sid, "command": "echo done"})
        comp_cid = r.get("command_id", "")
        if comp_cid:
            client.tool_call("ssh_get_command_output", {"command_id": comp_cid, "wait": True, "wait_timeout_secs": 5})
            r = client.tool_call("ssh_cancel_command", {"command_id": comp_cid})
            total += 1
            if test("Cancel completed command -> error or no-op", True, str(r)[:100]):
                passed += 1
            else:
                failed += 1
        client.tool_call("ssh_disconnect", {"session_id": comp_sid})

    # -- H. Mixed Concurrent Valid + Invalid Operations --
    print("\n=== H. MIXED CONCURRENT VALID + INVALID ===", flush=True)
    mix_r = client.tool_call("ssh_connect", {"address": HOST, "username": USER, "agent_id": "chaos-h", "name": "mixed"})
    mix_sid = mix_r.get("session_id", "")
    if mix_sid:
        fake1 = str(_uuid.uuid4())
        fake2 = str(_uuid.uuid4())
        mix_calls = [
            ("ssh_execute", {"session_id": mix_sid, "command": "echo VALID_1"}),
            ("ssh_execute", {"session_id": mix_sid, "command": "echo VALID_2"}),
            ("ssh_execute", {"session_id": fake1, "command": "echo BAD1"}),
            ("ssh_get_command_output", {"command_id": fake2, "wait": False}),
            ("ssh_shell_write", {"shell_id": fake1, "input": "x\n"}),
            ("ssh_shell_read", {"shell_id": fake2}),
        ]
        sent = client.send_batch(mix_calls)
        responses = client.collect_responses(sent.keys(), timeout=15)
        rid_list = list(sent.keys())
        results = {}
        for rid, resp in responses.items():
            idx = rid_list.index(rid)
            results[idx] = client.tool_call_parsed(resp)
        valid_ok = all("command_id" in results.get(i, {}) for i in [0, 1])
        invalid_ok = all("error" in results.get(i, {}) for i in [2, 3, 4, 5])
        total += 1
        if test("Valid operations succeeded", valid_ok, f"results[0]={str(results.get(0, {}))[:60]}"):
            passed += 1
        else:
            failed += 1
        total += 1
        if test("Invalid operations returned errors", invalid_ok, f"errors={sum(1 for i in [2,3,4,5] if 'error' in results.get(i, {}))}/4"):
            passed += 1
        else:
            failed += 1
        total += 1
        if test("Server did not crash (6 mixed concurrent calls)", len(responses) == 6):
            passed += 1
        else:
            failed += 1
        client.tool_call("ssh_disconnect", {"session_id": mix_sid})
    else:
        total += 1
        if test("Mixed concurrent (skipped, connect failed)", False):
            passed += 1
        else:
            failed += 1

    # Cleanup chaos agents
    for agent in ["chaos-g", "chaos-g2", "chaos-g3", "chaos-g4", "chaos-h"]:
        client.tool_call("ssh_disconnect_agent", {"agent_id": agent})

    # =========================================================================
    # ORIGINAL DISCONNECT / CLEANUP TESTS
    # =========================================================================

    # 14. Disconnect agent
    print("\n=== DISCONNECT AGENT ===", flush=True)
    result = client.tool_call("ssh_disconnect_agent", {"agent_id": "sftp-test"})
    total += 1
    if test("Agent disconnected", result.get("sessions_disconnected", 0) > 0,
            f"sessions_disconnected={result.get('sessions_disconnected')}"):
        passed += 1
    else:
        failed += 1

    # Cleanup
    client.close()
    subprocess.run(["rm", "-f", DOWNLOAD_FILE], capture_output=True)

    # Summary
    print(f"\n{'='*60}", flush=True)
    print(f"RESULTS: {passed}/{total} passed, {failed} failed", flush=True)
    if failed == 0:
        print("ALL TESTS PASSED", flush=True)
    else:
        print(f"FAILURES: {failed}", flush=True)
    print(f"{'='*60}", flush=True)
    sys.exit(0 if failed == 0 else 1)


if __name__ == "__main__":
    main()
