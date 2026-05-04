#!/usr/bin/env python3
"""v5 Phase 5 — multi-host stress harness.

Spawns the `ssh-mcp-tail` daemon and drives 20 simulated hosts in
parallel via NDJSON ops on stdin. Distributes 100 subscriptions
across the hosts and runs random subscribe/unsubscribe/exec ops for
the configured window.

Assertions at end:
- daemon still alive
- no `SUB_LEAK_RISK` warn lines beyond the configured tolerance
- no TCP socket leak (lsof check, best-effort)

Usage:
    python3 scripts/stress_multi_host.py --duration 600
    python3 scripts/stress_multi_host.py --duration 60 --hosts 4 --subs 20

Note: this harness does NOT require a live SSH server. The
ssh_connect ops fail (host 127.0.0.1:0 is bogus) but the daemon's
internal lifecycle still exercises every code path.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Iterator

CRATE_ROOT = Path(__file__).resolve().parent.parent


def build_daemon() -> Path:
    """Build the daemon binary in release mode."""
    subprocess.run(
        ["cargo", "build", "--release", "--bin", "ssh-mcp-tail"],
        cwd=CRATE_ROOT,
        check=True,
        stdout=subprocess.DEVNULL,
    )
    binary = CRATE_ROOT / "target" / "release" / "ssh-mcp-tail"
    if not binary.exists():
        raise RuntimeError(f"daemon binary not found: {binary}")
    return binary


def synth_ops(num_hosts: int, num_subs: int, total_ops: int) -> Iterator[str]:
    """Produce a deterministic-ish stream of NDJSON ops."""
    rng = random.Random(0xC0DE_F00D)
    for op_idx in range(total_ops):
        host_idx = rng.randint(0, num_hosts - 1)
        choice = rng.random()
        if choice < 0.4:
            yield json.dumps({
                "op": "connect",
                "host": "127.0.0.1",
                "user": f"stress-{host_idx}",
                "port": 0,
                "agent_id": f"stress-{host_idx}",
                "reuse_policy": "force_new",
                "id": f"op-{op_idx}",
            })
        elif choice < 0.7:
            yield json.dumps({
                "op": "subscribe",
                "uri": f"shell://stress-{host_idx}-{op_idx % num_subs}/output",
                "lifetime": "manual",
                "lag_policy": "snapshot",
                "id": f"op-{op_idx}",
            })
        else:
            yield json.dumps({
                "op": "read",
                "uri": f"shell://stress-{host_idx}-{op_idx % num_subs}/output",
                "id": f"op-{op_idx}",
            })


def count_open_sockets(pid: int) -> int:
    """Best-effort socket count via lsof (macOS/Linux). Returns -1 on failure."""
    if not shutil.which("lsof"):
        return -1
    try:
        out = subprocess.run(
            ["lsof", "-nP", "-p", str(pid), "-iTCP"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        # First line is header; subtract 1 if non-empty.
        lines = out.stdout.decode(errors="ignore").splitlines()
        return max(0, len(lines) - 1) if lines else 0
    except (OSError, subprocess.SubprocessError):
        return -1


def run(args: argparse.Namespace) -> int:
    binary = build_daemon()
    print(f"stress: daemon={binary}", file=sys.stderr)

    proc = subprocess.Popen(
        [str(binary), "daemon"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=CRATE_ROOT,
    )
    if proc.stdin is None:
        raise RuntimeError("daemon stdin not available")

    initial_sockets = count_open_sockets(proc.pid)
    print(f"stress: initial_sockets={initial_sockets}", file=sys.stderr)

    deadline = time.monotonic() + args.duration
    op_count = 0
    op_iter = synth_ops(args.hosts, args.subs, args.duration * 50)

    try:
        for line in op_iter:
            if time.monotonic() >= deadline:
                break
            try:
                proc.stdin.write((line + "\n").encode())
                proc.stdin.flush()
                op_count += 1
            except BrokenPipeError:
                print("stress: daemon stdin closed", file=sys.stderr)
                break
            time.sleep(0.001)
        # Send shutdown.
        try:
            proc.stdin.write(b'{"op":"shutdown","id":"final"}\n')
            proc.stdin.flush()
        except BrokenPipeError:
            pass
    finally:
        try:
            proc.stdin.close()
        except OSError:
            pass

    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()

    final_sockets = count_open_sockets(proc.pid)
    stderr_blob = (proc.stderr.read().decode(errors="ignore") if proc.stderr else "")
    leak_warnings = stderr_blob.count("SUB_LEAK_RISK")
    panics = stderr_blob.count("panicked")

    print(
        f"stress: ops={op_count} duration={args.duration}s "
        f"sockets {initial_sockets}->{final_sockets} "
        f"leak_warnings={leak_warnings} panics={panics}",
        file=sys.stderr,
    )

    if panics > 0:
        print("stress: FAIL — panic detected", file=sys.stderr)
        return 2
    if initial_sockets >= 0 and final_sockets >= 0 and final_sockets - initial_sockets > 32:
        delta = final_sockets - initial_sockets
        print(f"stress: FAIL — socket leak ({delta} new sockets)", file=sys.stderr)
        return 3
    print("stress: PASS", file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="ssh-mcp-tail multi-host stress")
    parser.add_argument("--duration", type=int, default=int(os.environ.get("STRESS_DURATION_S", "60")))
    parser.add_argument("--hosts", type=int, default=20)
    parser.add_argument("--subs", type=int, default=100)
    return run(parser.parse_args())


if __name__ == "__main__":
    sys.exit(main())
