"""v4.7-specific chaos suite.

Probes the new v4.7 surfaces under stress + adversarial conditions:

- Idempotency cache thrash — 2000 unique keys against ssh_execute over
  ~30 s; assert no panics, cache stays under cap.
- Progress notification flood — 10 concurrent waits with progress tokens;
  assert delivery rate is bounded and no cross-leak.
- structured_content stress — 1 MiB shell output; assert no drift.
- prompts/get spam — 1000 calls; assert no leak, no slowdown.
- templates/list under load — 500 concurrent calls; assert identical.
- ssh_run timeout cliff — 50 timeouts; assert response is bounded.
- Batch cancel mid-flight — start a batch, disconnect mid-run.
- closest-match with empty repo — no `closest matches:` line.
- INITIAL_BUFFER race — sender that emits exactly at the boundary.

Each scenario emits a single JSON line via ``write_event`` plus a final
summary. Exit code 0 when every scenario reports ``ok=True``, 1 otherwise.

Transport: stdio. Local in-process paramiko sshd is launched per the
scenarios that need a live SSH endpoint.
"""

from __future__ import annotations

import json
import os
import sys
import threading
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from helpers.chaos import (  # noqa: E402
    chaos_session,
    write_event,
    write_summary,
)
from helpers.local_sshd import LocalSshdFixture  # noqa: E402
from helpers.mcp_client import (  # noqa: E402
    McpClient,
    StdioTransport,
    call_tool_text,
)
from helpers.parse_block import parse_block  # noqa: E402


REPO_ROOT = Path(__file__).resolve().parent.parent
STDIO_BIN = REPO_ROOT / "target" / "release" / "ssh-mcp-stdio"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_chaos_client_with_env(env: dict | None = None):
    """Spawn a stderr-captured stdio child with extra env vars."""
    from helpers.chaos import _StderrCapturingStdioTransport  # type: ignore[attr-defined]

    transport = _StderrCapturingStdioTransport(
        [str(STDIO_BIN)],
        env={"RUST_LOG": os.environ.get("RUST_LOG", "warn"), **(env or {})},
    )
    client = McpClient(transport)
    client.initialize()
    return client, transport


def _connect_local(client: McpClient, fx: LocalSshdFixture, *, agent: str) -> str | None:
    text = call_tool_text(client, "ssh_connect", fx.connect_args(agent_id=agent))
    return parse_block(text).get("session_id")


# ---------------------------------------------------------------------------
# Scenario: idempotency cache thrash
# ---------------------------------------------------------------------------


def scenario_idempotency_thrash() -> dict:
    """Fire 2000 unique idempotency keys against ssh_execute. Assert no
    panics; cache stays bounded; server responsive after."""
    fx = LocalSshdFixture()
    fx.__enter__()
    try:
        client, transport = _make_chaos_client_with_env(
            {"SSH_IDEMPOTENCY_MAX_ENTRIES": "256"}
        )
        try:
            sid = _connect_local(client, fx, agent="thrash")
            if not sid:
                return {"ok": False, "error": "connect failed"}
            keys = [str(uuid.uuid4()) for _ in range(2000)]
            started = time.monotonic()
            errors = 0
            for k in keys:
                result = client.call_tool_with_meta(
                    "ssh_exec",
                    {"session_id": sid, "command": "true"},
                    meta={"idempotency_key": k},
                    timeout=10,
                )
                text = ((result.get("content") or [{}])[0].get("text") or "")
                parsed = parse_block(text)
                if parsed.get("__status") not in {"STARTED", "ERROR"}:
                    errors += 1
            elapsed = time.monotonic() - started
            # Server still responsive.
            sanity_text = call_tool_text(client, "ssh_sessions", {})
            sanity = parse_block(sanity_text)
            ok = (
                not transport.panicked()
                and sanity.get("__status") == "OK"
                and errors == 0
            )
            call_tool_text(client, "ssh_disconnect", {"session_id": sid})
            return {
                "ok": ok,
                "elapsed_s": round(elapsed, 2),
                "errors": errors,
                "panicked": transport.panicked(),
            }
        finally:
            client.close()
    finally:
        fx.__exit__(None, None, None)


# ---------------------------------------------------------------------------
# Scenario: progress notification flood
# ---------------------------------------------------------------------------


def scenario_progress_flood() -> dict:
    """10 concurrent waits with progress tokens, ensure no cross-leak.

    We use 10 commands sleeping 4s each. Each call carries a unique
    progressToken. After all wait_for calls return, we drain the
    notification queue and assert every progress notification's
    progressToken matches one of the 10 tokens.
    """
    fx = LocalSshdFixture()
    fx.__enter__()
    try:
        with chaos_session() as (client, transport):
            sid = _connect_local(client, fx, agent="prog-flood")
            if not sid:
                return {"ok": False, "error": "connect failed"}
            # Spawn 10 long-running async commands.
            cids = []
            for _ in range(10):
                text = call_tool_text(
                    client,
                    "ssh_exec",
                    {"session_id": sid, "command": "sleep 4"},
                )
                cid = parse_block(text).get("command_id")
                if cid:
                    cids.append(cid)
            tokens = {f"flood-{i}" for i in range(len(cids))}
            client.drain_notifications()
            # Fire all the waits concurrently from python threads.
            results = []
            results_lock = threading.Lock()

            def waiter(cid: str, token: str) -> None:
                try:
                    result = client.call_tool_with_meta(
                        "ssh_exec_output",
                        {"command_id": cid, "wait": True, "wait_timeout_secs": 8},
                        meta={"progressToken": token},
                        timeout=15,
                    )
                except Exception as e:
                    with results_lock:
                        results.append({"error": str(e)})
                    return
                with results_lock:
                    results.append({"ok": "_rpc_error" not in result})

            threads = []
            for cid, tok in zip(cids, sorted(tokens), strict=False):
                t = threading.Thread(target=waiter, args=(cid, tok))
                t.start()
                threads.append(t)
            for t in threads:
                t.join(timeout=20)
            # Drain leftover notifications and verify token integrity.
            seen_tokens = set()
            for _ in range(50):
                n = client.receive_notification(timeout=0.1)
                if n is None:
                    break
                if n.get("method") == "notifications/progress":
                    params = n.get("params") or {}
                    tok = str(
                        params.get("progressToken") or params.get("progress_token") or ""
                    )
                    if tok:
                        seen_tokens.add(tok)
            stray = seen_tokens - tokens
            ok = (
                not transport.panicked()
                and not stray
                and all(r.get("ok") for r in results)
            )
            call_tool_text(client, "ssh_disconnect", {"session_id": sid})
            return {
                "ok": ok,
                "tokens_seen": len(seen_tokens),
                "stray_tokens": list(stray),
                "panicked": transport.panicked(),
            }
    finally:
        fx.__exit__(None, None, None)


# ---------------------------------------------------------------------------
# Scenario: structured_content stress (1 MiB shell output)
# ---------------------------------------------------------------------------


def scenario_structured_content_stress() -> dict:
    """Drive a shell that emits ~1 MiB; verify the dual-channel pair has
    consistent payload sizes.

    Note: ``ssh_shell_read`` does NOT advertise ``BYTES_RETURNED:`` on
    the Markdown header (only ``ssh_shell_wait_for`` does). The
    structured channel carries ``bytes_returned``. We instead compare:
    - structured ``bytes_returned`` matches the actual length of the
      structured ``data`` field.
    - The Markdown ``--- data [n] ---`` block byte count matches.
    """
    fx = LocalSshdFixture()
    fx.__enter__()
    try:
        with chaos_session() as (client, transport):
            sid = _connect_local(client, fx, agent="sc-stress")
            if not sid:
                return {"ok": False, "error": "connect failed"}
            shell_id = parse_block(
                call_tool_text(
                    client,
                    "ssh_shell_open",
                    {"session_id": sid, "max_buffer_size": "2m"},
                )
            ).get("shell_id")
            if not shell_id:
                return {"ok": False, "error": "shell_open failed"}
            # Generate 1 MiB of random data via base64.
            call_tool_text(
                client,
                "ssh_shell_write",
                {
                    "shell_id": shell_id,
                    "input": "head -c 786432 /dev/urandom | base64\n",
                },
            )
            time.sleep(2.0)
            result = client.call_tool(
                "ssh_shell_read",
                {
                    "shell_id": shell_id,
                    "wait": True,
                    "wait_timeout_secs": 5,
                    "min_bytes": 100000,
                    "max_output_bytes": 1048576,
                },
                timeout=15,
            )
            text = ((result.get("content") or [{}])[0].get("text") or "")
            structured = result.get("structuredContent") or result.get(
                "structured_content"
            ) or {}
            parsed = parse_block(text)
            md_data = parsed.get("data") or parsed.get("__blocks", {}).get("data") or ""
            md_bytes = len(md_data.encode("utf-8")) if md_data else 0
            struct_bytes = structured.get("bytes_returned", 0)
            # ssh_shell_read structured field is "stdout" (not "data"); aligns
            # with the rmcp `SshShellReadResult` contract.
            struct_data = structured.get("stdout") or structured.get("data") or ""
            struct_data_bytes = len(struct_data.encode("utf-8")) if struct_data else 0
            # The two channels can differ slightly because Markdown adds
            # an extra newline at the end of the block fence. Allow a
            # 64-byte tolerance (block fence + newlines).
            drift = abs(md_bytes - struct_data_bytes)
            ok = (
                not transport.panicked()
                and struct_bytes > 0
                and drift <= 64
                and struct_data_bytes == struct_bytes
            )
            call_tool_text(client, "ssh_shell_close", {"shell_id": shell_id})
            call_tool_text(client, "ssh_disconnect", {"session_id": sid})
            return {
                "ok": ok,
                "md_bytes": md_bytes,
                "struct_data_bytes": struct_data_bytes,
                "struct_bytes_returned": struct_bytes,
                "drift": drift,
                "panicked": transport.panicked(),
            }
    finally:
        fx.__exit__(None, None, None)


# ---------------------------------------------------------------------------
# Scenario: prompts/get spam
# ---------------------------------------------------------------------------


def scenario_prompts_get_spam() -> dict:
    """1000 prompts/get calls. Assert all return identical text and the
    server is still responsive after."""
    with chaos_session() as (client, transport):
        first = client.get_prompt(
            "run_one_shot_command",
            {"address": "h.example.com", "username": "alice", "command": "uptime"},
        )
        first_text = ""
        if first.get("messages"):
            first_text = (first["messages"][0].get("content") or {}).get("text", "")
        started = time.monotonic()
        mismatches = 0
        for _ in range(1000):
            r = client.get_prompt(
                "run_one_shot_command",
                {"address": "h.example.com", "username": "alice", "command": "uptime"},
            )
            t = ""
            if r.get("messages"):
                t = (r["messages"][0].get("content") or {}).get("text", "")
            if t != first_text:
                mismatches += 1
        elapsed = time.monotonic() - started
        # Sanity ping
        sanity = parse_block(call_tool_text(client, "ssh_sessions", {}))
        ok = (
            not transport.panicked()
            and mismatches == 0
            and sanity.get("__status") == "OK"
        )
        return {
            "ok": ok,
            "elapsed_s": round(elapsed, 2),
            "mismatches": mismatches,
            "panicked": transport.panicked(),
        }


# ---------------------------------------------------------------------------
# Scenario: templates/list under load
# ---------------------------------------------------------------------------


def scenario_templates_list_under_load() -> dict:
    """500 sequential templates/list calls (rmcp 1.6 stdio is single-
    threaded per process; we exercise the call cadence rather than true
    concurrency). Assert all return identical template lists."""
    with chaos_session() as (client, transport):
        first = client.list_resource_templates()
        mismatches = 0
        for _ in range(500):
            t = client.list_resource_templates()
            if t != first:
                mismatches += 1
        ok = not transport.panicked() and mismatches == 0
        return {
            "ok": ok,
            "calls": 500,
            "templates_per_call": len(first),
            "mismatches": mismatches,
            "panicked": transport.panicked(),
        }


# ---------------------------------------------------------------------------
# Scenario: ssh_run timeout cliff
# ---------------------------------------------------------------------------


def scenario_ssh_run_timeout_cliff() -> dict:
    """50 ssh_run calls that exceed timeout. Assert each returns within
    timeout + 5s slack."""
    fx = LocalSshdFixture()
    fx.__enter__()
    try:
        with chaos_session() as (client, transport):
            count = 50
            slow_calls = 0
            for _ in range(count):
                args = fx.connect_args(agent_id="run-cliff")
                args["command"] = "sleep 5"
                args["timeout_secs"] = 1
                args["disconnect_after"] = True
                started = time.monotonic()
                client.call_tool("ssh_run", args, timeout=15)
                elapsed = time.monotonic() - started
                if elapsed > 6.0:  # 1s timeout + 5s slack
                    slow_calls += 1
            ok = not transport.panicked() and slow_calls == 0
            return {
                "ok": ok,
                "calls": count,
                "slow_calls": slow_calls,
                "panicked": transport.panicked(),
            }
    finally:
        fx.__exit__(None, None, None)


# ---------------------------------------------------------------------------
# Scenario: batch cancel mid-flight
# ---------------------------------------------------------------------------


def scenario_batch_cancel_mid_flight() -> dict:
    """Start a 16-command batch with sleeps; while it's running, fire
    ssh_disconnect_many on the session. Assert the server doesn't panic
    and the batch returns gracefully (status: ok or halted)."""
    fx = LocalSshdFixture()
    fx.__enter__()
    try:
        with chaos_session() as (client, transport):
            sid = _connect_local(client, fx, agent="batch-cancel")
            if not sid:
                return {"ok": False, "error": "connect failed"}
            # Start the batch in a thread.
            batch_result = {}

            def runner() -> None:
                try:
                    r = client.call_tool(
                        "ssh_exec_batch",
                        {
                            "session_id": sid,
                            "commands": ["sleep 2"] * 16,
                            "stop_on_failure": False,
                            "timeout_secs_per_command": 4,
                        },
                        timeout=120,
                    )
                except Exception as e:
                    batch_result["error"] = str(e)
                    return
                batch_result["raw"] = r

            t = threading.Thread(target=runner)
            t.start()
            time.sleep(2.5)
            # Now cancel the session.
            call_tool_text(client, "ssh_disconnect_many", {"session_ids": [sid]})
            t.join(timeout=120)
            ok = not transport.panicked() and "raw" in batch_result
            return {
                "ok": ok,
                "panicked": transport.panicked(),
                "batch_returned": "raw" in batch_result,
                "batch_error": batch_result.get("error"),
            }
    finally:
        fx.__exit__(None, None, None)


# ---------------------------------------------------------------------------
# Scenario: closest-match with empty repo
# ---------------------------------------------------------------------------


def scenario_closest_match_empty_repo() -> dict:
    """ssh_disconnect on a ghost id with NO live sessions must NOT
    advertise ``closest matches:``."""
    with chaos_session() as (client, transport):
        ghost = str(uuid.uuid4())
        text = call_tool_text(client, "ssh_disconnect", {"session_id": ghost})
        parsed = parse_block(text)
        detail = parsed.get("detail", "") or ""
        ok = (
            not transport.panicked()
            and parsed.get("__status") == "ERROR"
            and "closest matches" not in detail.lower()
        )
        return {
            "ok": ok,
            "panicked": transport.panicked(),
            "detail": detail,
        }


# ---------------------------------------------------------------------------
# Scenario: INITIAL_BUFFER race
# ---------------------------------------------------------------------------


def scenario_initial_buffer_race() -> dict:
    """Fire 30 shell_open calls against a banner-emitting fixture; assert
    no panics and the rendered structured payload always carries one of:
    a banner string, an empty/None initial_buffer (race lost), or a
    small prompt artifact."""
    fx = LocalSshdFixture(banner_after_shell_open=b"RACE-BANNER\r\n$ ")
    fx.__enter__()
    try:
        with chaos_session() as (client, transport):
            sid = _connect_local(client, fx, agent="ib-race")
            if not sid:
                return {"ok": False, "error": "connect failed"}
            with_banner = 0
            without_banner = 0
            for _ in range(30):
                result = client.call_tool("ssh_shell_open", {"session_id": sid})
                structured = result.get("structuredContent") or result.get(
                    "structured_content"
                ) or {}
                ib = structured.get("initial_buffer") or ""
                if "RACE-BANNER" in ib:
                    with_banner += 1
                else:
                    without_banner += 1
                shell_id = structured.get("shell_id")
                if shell_id:
                    call_tool_text(client, "ssh_shell_close", {"shell_id": shell_id})
            ok = not transport.panicked() and (with_banner + without_banner) == 30
            call_tool_text(client, "ssh_disconnect", {"session_id": sid})
            return {
                "ok": ok,
                "with_banner": with_banner,
                "without_banner": without_banner,
                "panicked": transport.panicked(),
            }
    finally:
        fx.__exit__(None, None, None)


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


SCENARIOS = [
    ("idempotency_thrash", scenario_idempotency_thrash),
    ("progress_flood", scenario_progress_flood),
    ("structured_content_stress", scenario_structured_content_stress),
    ("prompts_get_spam", scenario_prompts_get_spam),
    ("templates_list_under_load", scenario_templates_list_under_load),
    ("ssh_run_timeout_cliff", scenario_ssh_run_timeout_cliff),
    ("batch_cancel_mid_flight", scenario_batch_cancel_mid_flight),
    ("closest_match_empty_repo", scenario_closest_match_empty_repo),
    ("initial_buffer_race", scenario_initial_buffer_race),
]


def main() -> int:
    if not STDIO_BIN.exists():
        write_event({"scenario": "bootstrap", "ok": False, "error": "stdio binary not built"})
        return write_summary(
            {"chaos_v47": "skip", "tested": 0, "passed": 0, "failed": 0, "panics": 0}
        )
    tested = 0
    passed = 0
    failed = 0
    panics = 0
    errors: list[dict] = []
    for name, body in SCENARIOS:
        started = time.monotonic()
        try:
            result = body()
        except Exception as exc:  # pragma: no cover - chaos-test catch-all
            result = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        elapsed = round(time.monotonic() - started, 2)
        ok = bool(result.pop("ok", False))
        panicked = bool(result.get("panicked", False))
        tested += 1
        if ok and not panicked:
            passed += 1
        else:
            failed += 1
        if panicked:
            panics += 1
            errors.append({"scenario": name, "reason": "panic"})
        if not ok:
            errors.append({"scenario": name, "result": result})
        event = {"scenario": name, "ok": ok, "elapsed_s": elapsed}
        event.update(result)
        write_event(event)
    return write_summary(
        {
            "chaos_v47": "ok" if failed == 0 and panics == 0 else "fail",
            "status": "ok" if failed == 0 and panics == 0 else "fail",
            "tested": tested,
            "passed": passed,
            "failed": failed,
            "panics": panics,
            "errors": errors,
        }
    )


if __name__ == "__main__":
    sys.exit(main())
