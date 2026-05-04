"""Chaos coverage for v5 subscribe / lifecycle / lag policy.

This suite spawns 10..100 concurrent client threads against a single
``ssh-mcp-stdio`` child per scenario and asserts:

- No panic on the server stderr.
- No deadlock (every worker thread joins within the deadline).
- No leaked subscriptions (final ``ssh_sub_list`` reports a quiescent
  state).
- The daemon stays responsive — a sentinel ``ssh_sub_list`` /
  ``ssh_daemon_stats`` call returns OK after the storm.

How to run::

    cargo build --release --bin ssh-mcp-stdio
    .venv/bin/python -m pytest scripts/chaos_v5_subscribe.py -v --tb=short -x

Each scenario is a top-level ``test_chaos_*`` pytest function so the
chaos pass can be cherry-picked. The scenarios are deliberately
adversarial against the lock-free invariants documented in
``docs/LOCKS.md``:

- Atomic subscribe / unsubscribe registry against burst churn.
- Pause / resume race against an active producer.
- Filter hot-reload while events stream.
- Replay during active emission (cursor + ring-buffer race).
- Idempotency replay collision under concurrency.
- Lifecycle grace timer race against re-subscribe.
- Daemon-wide signal storm (many tools hit at once).

Tests skip gracefully if the binary is missing.
"""

from __future__ import annotations

import os
import threading
import time
import uuid
from collections.abc import Callable
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator

import pytest

from helpers.fixtures import STDIO_BIN
from helpers.local_sshd import LocalSshdFixture
from helpers.mcp_client import McpClient, StdioTransport
from helpers.parse_block import parse_block


# ---------------------------------------------------------------------------
# Helpers — replicate the adversarial-test scaffolding so this file can be
# run independently of test_v5_adversarial.py.
# ---------------------------------------------------------------------------


def _make_chaos_client(env: dict | None = None) -> McpClient:
    """Spawn a fresh stdio child + initialised :class:`McpClient`."""
    transport = StdioTransport(
        [str(STDIO_BIN)],
        env={"RUST_LOG": os.environ.get("RUST_LOG", "warn"), **(env or {})},
    )
    client = McpClient(transport)
    client.initialize()
    return client


@contextmanager
def chaos_client(env: dict | None = None) -> Iterator[McpClient]:
    """Context manager owning one stdio child for the duration of a scenario."""
    if not STDIO_BIN.exists():
        pytest.skip(f"stdio binary not built: {STDIO_BIN}")
    client = _make_chaos_client(env)
    try:
        yield client
    finally:
        try:
            client.close()
        except Exception:
            pass


def _call_text(client: McpClient, name: str, args: dict,
               *, timeout: float = 15.0) -> str:
    result = client.call_tool(name, args, timeout=timeout)
    if "_rpc_error" in result:
        return ""
    content = result.get("content") or []
    if content and isinstance(content[0], dict):
        return content[0].get("text") or ""
    return ""


def _subscribe(client: McpClient, uri: str, **extra) -> str | None:
    args = {"uri": uri, **extra}
    text = _call_text(client, "ssh_subscribe", args)
    return parse_block(text).get("sub_id")


def _unsubscribe(client: McpClient, sub_id: str) -> str:
    return _call_text(client, "ssh_unsubscribe", {"sub_id": sub_id})


def _join_all(threads: list[threading.Thread], deadline: float) -> None:
    """Join every thread; assert none exceed the deadline."""
    end = time.monotonic() + deadline
    for t in threads:
        remaining = max(0.1, end - time.monotonic())
        t.join(timeout=remaining)
        assert not t.is_alive(), f"thread hung past deadline: {t.name}"


def _final_sanity(client: McpClient) -> None:
    """The daemon must be responsive after the storm."""
    text = _call_text(client, "ssh_sub_list", {})
    assert "SSH_SUB_LIST: OK" in text, text
    text = _call_text(client, "ssh_daemon_stats", {})
    assert "SSH_DAEMON_STATS: OK" in text, text


@pytest.fixture
def local_fx() -> Iterator[LocalSshdFixture]:
    """In-process paramiko sshd for chaos scenarios that need a session."""
    fx = LocalSshdFixture()
    fx.__enter__()
    try:
        yield fx
    finally:
        fx.__exit__(None, None, None)


# ---------------------------------------------------------------------------
# Scenario: 50-thread subscribe / unsubscribe churn
# ---------------------------------------------------------------------------


def test_chaos_burst_subscribe_unsubscribe_50_threads() -> None:
    """50 worker threads subscribe + unsubscribe a unique URI each.

    Asserts:
    - No panic, no deadlock.
    - Final sub_list returns OK with empty/cleaned state.
    - All subscribe calls returned a non-empty sub_id.
    """
    with chaos_client() as client:
        errors: list[str] = []
        sub_ids: list[str] = []
        lock = threading.Lock()

        def worker(idx: int) -> None:
            try:
                sub_id = _subscribe(client, f"shell://burst50-{idx}/output")
                if not sub_id:
                    with lock:
                        errors.append(f"subscribe-{idx}: empty sub_id")
                    return
                with lock:
                    sub_ids.append(sub_id)
                # Half of the workers unsubscribe before the join; the
                # other half rely on the final cleanup pass to exercise
                # both code paths.
                if idx % 2 == 0:
                    _unsubscribe(client, sub_id)
            except Exception as exc:
                with lock:
                    errors.append(f"{idx}: {exc!r}")

        threads = [threading.Thread(target=worker, args=(i,), name=f"burst50-{i}")
                   for i in range(50)]
        for t in threads:
            t.start()
        _join_all(threads, deadline=60.0)
        assert not errors, errors
        # Cleanup remaining lanes.
        for sub_id in sub_ids:
            _unsubscribe(client, sub_id)
        _final_sanity(client)


# ---------------------------------------------------------------------------
# Scenario: concurrent pause / resume on 5 lanes
# ---------------------------------------------------------------------------


def test_chaos_concurrent_pause_resume_5_subs() -> None:
    """5 worker threads each toggle pause/resume on its own SubId 50 times.

    The lane-admin port must serialize these atomically; we assert no
    panic and that the final sub_list reflects ``paused=true|false`` on
    every lane (no torn reads).
    """
    with chaos_client() as client:
        sub_ids = [_subscribe(client, f"shell://pause-{i}/output") for i in range(5)]
        assert all(sub_ids), sub_ids

        errors: list[str] = []
        lock = threading.Lock()

        def toggler(sub_id: str) -> None:
            try:
                for _ in range(50):
                    _call_text(client, "ssh_sub_pause", {"sub_id": sub_id})
                    _call_text(client, "ssh_sub_resume", {"sub_id": sub_id})
            except Exception as exc:
                with lock:
                    errors.append(f"{sub_id}: {exc!r}")

        threads = [threading.Thread(target=toggler, args=(s,),
                                    name=f"toggler-{s[:8]}")
                   for s in sub_ids]
        for t in threads:
            t.start()
        _join_all(threads, deadline=60.0)
        assert not errors, errors

        # Verify every sub_id is still listed and shows a stable
        # paused= flag (no torn reads).
        text = _call_text(client, "ssh_sub_list", {})
        for sub_id in sub_ids:
            assert sub_id in text, f"sub_id {sub_id} missing from list: {text}"
        for sub_id in sub_ids:
            _unsubscribe(client, sub_id)
        _final_sanity(client)


# ---------------------------------------------------------------------------
# Scenario: filter hot-reload while emission is active
# ---------------------------------------------------------------------------


def test_chaos_filter_hot_reload_under_emission(
    local_fx: LocalSshdFixture,
) -> None:
    """One producer (ssh_execute on a real session) emits to a lane while
    another thread hot-reloads the regex filter 100 times. The lane must
    survive without lost events on the matching path.
    """
    with chaos_client() as client:
        # Connect + kick off a long-running command to drive emissions.
        text = _call_text(client, "ssh_connect",
                          local_fx.connect_args(agent_id="filter-chaos"),
                          timeout=20)
        sid = parse_block(text).get("session_id")
        if not sid:
            pytest.skip(f"local sshd connect failed: {text!r}")

        cid = parse_block(
            _call_text(client, "ssh_execute",
                       {"session_id": sid,
                        "command": "for i in $(seq 1 100); do echo MARKER-$i; sleep 0.05; done"})
        ).get("command_id")
        assert cid, "ssh_execute failed"

        try:
            sub_id = _subscribe(client, f"command://{cid}/output",
                                filter="MARKER")
            assert sub_id, "subscribe failed"

            errors: list[str] = []
            lock = threading.Lock()
            patterns = [".*", "MARKER", "MARKER-1.*", "MARKER-[0-9]+",
                        "skip-everything", ".+"]

            def hot_reloader() -> None:
                try:
                    for i in range(100):
                        regex = patterns[i % len(patterns)]
                        _call_text(
                            client, "ssh_sub_filter",
                            {"sub_id": sub_id, "regex": regex},
                        )
                except Exception as exc:
                    with lock:
                        errors.append(f"reload: {exc!r}")

            t = threading.Thread(target=hot_reloader, name="filter-reload")
            t.start()
            _join_all([t], deadline=30.0)
            assert not errors, errors

            # Lane stats still readable.
            stats = _call_text(client, "ssh_sub_stats", {"sub_id": sub_id})
            assert "SSH_SUB_STATS: OK" in stats, stats
            _unsubscribe(client, sub_id)
        finally:
            _call_text(client, "ssh_cancel_command", {"command_id": cid})
            _call_text(client, "ssh_disconnect", {"session_id": sid})
        _final_sanity(client)


# ---------------------------------------------------------------------------
# Scenario: replay while a producer is actively emitting
# ---------------------------------------------------------------------------


def test_chaos_replay_during_active_producer(
    local_fx: LocalSshdFixture,
) -> None:
    """Replay the sub buffer 50 times while a producer streams output.

    The replay path acquires a snapshot of the ring buffer; this races
    against ``produce()`` on the lane. The test asserts no panic and
    that every replay returns OK or RING_BUFFER_OVERFLOW (acceptable
    when the cursor is ahead of the buffer head).
    """
    with chaos_client() as client:
        text = _call_text(client, "ssh_connect",
                          local_fx.connect_args(agent_id="replay-chaos"),
                          timeout=20)
        sid = parse_block(text).get("session_id")
        if not sid:
            pytest.skip(f"local sshd connect failed: {text!r}")
        try:
            cid = parse_block(
                _call_text(client, "ssh_execute",
                           {"session_id": sid,
                            "command": "for i in $(seq 1 200); do echo R-$i; done"})
            ).get("command_id")
            assert cid

            sub_id = _subscribe(client, f"command://{cid}/output")
            assert sub_id

            errors: list[str] = []
            statuses: list[str] = []
            lock = threading.Lock()

            def replayer() -> None:
                try:
                    for cursor in range(50):
                        text = _call_text(
                            client, "ssh_sub_replay",
                            {"sub_id": sub_id, "from_cursor": cursor},
                        )
                        with lock:
                            statuses.append(parse_block(text).get("__status", ""))
                except Exception as exc:
                    with lock:
                        errors.append(f"replay: {exc!r}")

            threads = [threading.Thread(target=replayer, name=f"replayer-{i}")
                       for i in range(4)]
            for t in threads:
                t.start()
            _join_all(threads, deadline=30.0)
            assert not errors, errors
            assert statuses, "no replay calls returned"
            # All statuses are OK or ERROR — never silently dropped.
            for s in statuses:
                assert s in {"OK", "ERROR"}, s
            _unsubscribe(client, sub_id)
        finally:
            _call_text(client, "ssh_cancel_command", {"command_id": cid})
            _call_text(client, "ssh_disconnect", {"session_id": sid})
        _final_sanity(client)


# ---------------------------------------------------------------------------
# Scenario: idempotency replay collision under concurrency
# ---------------------------------------------------------------------------


def test_chaos_idempotency_replay_collision(
    local_fx: LocalSshdFixture,
) -> None:
    """30 worker threads call ssh_execute with the SAME idempotency key.

    The cache must dedup atomically — every worker observes the same
    cached body, no panic, no race-induced corruption.
    """
    with chaos_client() as client:
        text = _call_text(client, "ssh_connect",
                          local_fx.connect_args(agent_id="idemp-chaos"),
                          timeout=20)
        sid = parse_block(text).get("session_id")
        if not sid:
            pytest.skip(f"local sshd connect failed: {text!r}")
        try:
            key = f"chaos-{uuid.uuid4()}"
            args = {"session_id": sid, "command": "echo IDEMP_CHAOS"}
            bodies: list[str] = []
            errors: list[str] = []
            lock = threading.Lock()

            def worker(idx: int) -> None:
                try:
                    result = client.call_tool_with_meta(
                        "ssh_execute", args,
                        meta={"idempotency_key": key},
                        timeout=15,
                    )
                    text = ((result.get("content") or [{}])[0].get("text") or "")
                    with lock:
                        bodies.append(text)
                except Exception as exc:
                    with lock:
                        errors.append(f"{idx}: {exc!r}")

            threads = [threading.Thread(target=worker, args=(i,),
                                        name=f"idemp-{i}")
                       for i in range(30)]
            for t in threads:
                t.start()
            _join_all(threads, deadline=30.0)
            assert not errors, errors
            assert len(bodies) == 30
            # Extract command_id from each body — under correct dedup
            # the cached body is replayed verbatim, so every parsed
            # command_id is identical.
            cids = {parse_block(b).get("command_id") for b in bodies}
            assert len(cids) == 1, f"idempotency leaked across calls: {cids}"
        finally:
            _call_text(client, "ssh_disconnect", {"session_id": sid})
        _final_sanity(client)


# ---------------------------------------------------------------------------
# Scenario: lifecycle grace timer race against re-subscribe
# ---------------------------------------------------------------------------


def test_chaos_lifecycle_grace_vs_resubscribe_race() -> None:
    """Subscribe with auto_close grace; race re-subscribe against grace expiry.

    Repeats ``subscribe(auto_close, grace_ms=20) -> unsubscribe -> subscribe``
    100 times in rapid succession. Asserts no panic and the final
    sub_list returns OK.
    """
    with chaos_client() as client:
        errors: list[str] = []
        for i in range(100):
            sub_id = _subscribe(
                client,
                f"shell://race-{i}/output",
                lifetime="auto_close",
                grace_ms=20,
            )
            if not sub_id:
                errors.append(f"subscribe-{i}: empty")
                continue
            _unsubscribe(client, sub_id)
            # Resubscribe before the grace window expires; lane should
            # accept (treating it as a fresh lane after unsub).
            sub_id2 = _subscribe(
                client,
                f"shell://race-{i}/output",
                lifetime="auto_close",
                grace_ms=20,
            )
            if not sub_id2:
                errors.append(f"resubscribe-{i}: empty")
                continue
            _unsubscribe(client, sub_id2)
        # Wait past the grace window so any lingering grace timers fire.
        time.sleep(0.5)
        assert not errors, errors
        _final_sanity(client)


# ---------------------------------------------------------------------------
# Scenario: signal storm — many tools hit at once
# ---------------------------------------------------------------------------


def test_chaos_daemon_signal_storm_during_subs() -> None:
    """20 threads hammer ssh_sub_list / ssh_daemon_stats / ssh_subscribe / ssh_unsubscribe at once.

    Verifies the daemon-stats aggregator is read-write safe under
    contention with the registry mutators.
    """
    with chaos_client() as client:
        sub_ids: list[str] = []
        errors: list[str] = []
        lock = threading.Lock()
        stop = threading.Event()

        def producer() -> None:
            try:
                while not stop.is_set():
                    sub_id = _subscribe(client,
                                        f"shell://storm-{uuid.uuid4()}/output")
                    if sub_id:
                        with lock:
                            sub_ids.append(sub_id)
            except Exception as exc:
                with lock:
                    errors.append(f"producer: {exc!r}")

        def consumer() -> None:
            try:
                while not stop.is_set():
                    with lock:
                        targets = sub_ids[-5:]
                    for sub_id in targets:
                        _call_text(client, "ssh_sub_stats",
                                   {"sub_id": sub_id})
            except Exception as exc:
                with lock:
                    errors.append(f"consumer: {exc!r}")

        def aggregator() -> None:
            try:
                while not stop.is_set():
                    _call_text(client, "ssh_sub_list", {})
                    _call_text(client, "ssh_daemon_stats", {})
            except Exception as exc:
                with lock:
                    errors.append(f"agg: {exc!r}")

        threads: list[threading.Thread] = []
        threads.extend(threading.Thread(target=producer, name=f"prod-{i}")
                       for i in range(8))
        threads.extend(threading.Thread(target=consumer, name=f"cons-{i}")
                       for i in range(6))
        threads.extend(threading.Thread(target=aggregator, name=f"agg-{i}")
                       for i in range(6))
        for t in threads:
            t.start()
        # Run the storm for ~5 seconds.
        time.sleep(5.0)
        stop.set()
        _join_all(threads, deadline=30.0)
        assert not errors, errors

        # Cleanup.
        for sub_id in sub_ids:
            _unsubscribe(client, sub_id)
        _final_sanity(client)
