"""Master chaos / stress runner.

Aggregates the v4.3 release-gate Python suites into a single run with
per-suite JSON-line output and a final summary table:

    test_stdio:         13/13
    stress_concurrent:  ok
    stress_lagged:      ok
    stress_locks:       ok (5min)
    stress_subscribe:   ok
    chaos_errors:       27/27
    chaos_locks:        7/7
    chaos_recovery:     ok
    chaos_exhaustion:   ok

Each suite is a separate ``python3 scripts/<name>.py`` invocation so a
crash in one cannot poison the next. Suites that depend on a live sshd
(see ``SSH_MCP_TEST_TARGET``) skip cleanly when the env var is unset
and the runner reports the skip in the final table.

Exit code: ``0`` when every suite reports ``ok``, ``1`` otherwise.

Usage::

    python3 scripts/chaos_runner.py
    python3 scripts/chaos_runner.py --skip-stress    # skip the long stress suites
    python3 scripts/chaos_runner.py --skip-chaos     # skip the chaos suites
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO_ROOT = ROOT.parent

# (suite_name, script, label, kind) where kind is one of:
#   "stdio_pytest" — pytest module (test_stdio.py)
#   "stress"       — stress_*.py (one JSON line summary on stdout)
#   "chaos"        — chaos_*.py  (per-event JSON lines + final summary)
SUITES: list[tuple[str, str, str, str]] = [
    ("test_stdio", "test_stdio.py", "test_stdio", "stdio_pytest"),
    ("stress_concurrent", "stress_concurrent_writes.py", "stress_concurrent", "stress"),
    ("stress_lagged", "stress_lagged_sub.py", "stress_lagged", "stress"),
    ("stress_locks", "stress_locks.py", "stress_locks", "stress"),
    ("stress_subscribe", "stress_subscribe.py", "stress_subscribe", "stress"),
    ("chaos_errors", "chaos_errors.py", "chaos_errors", "chaos"),
    ("chaos_locks", "chaos_locks.py", "chaos_locks", "chaos"),
    ("chaos_recovery", "chaos_recovery.py", "chaos_recovery", "chaos"),
    ("chaos_exhaustion", "chaos_exhaustion.py", "chaos_exhaustion", "chaos"),
    ("chaos_v47", "chaos_v47.py", "chaos_v47", "chaos"),
    ("chaos_v47_subscribe", "chaos_v47_subscribe.py", "chaos_v47_subscribe", "chaos"),
]


def _last_json_line(text: str) -> dict | None:
    """Return the last well-formed JSON object on its own line, or None."""
    last: dict | None = None
    for line in text.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            last = obj
    return last


def _format_stress(summary: dict | None) -> str:
    if summary is None:
        return "no-summary"
    status = summary.get("status") or summary.get("stress")
    if status == "skip":
        return "skip"
    return status or "fail"


def _format_chaos(summary: dict | None, key: str) -> str:
    if summary is None:
        return "no-summary"
    if summary.get("skipped"):
        return "skip"
    state = summary.get(key) or summary.get("status")
    return state or "fail"


def _format_stdio_pytest(returncode: int, elapsed: float) -> str:
    if returncode == 0:
        return f"ok ({elapsed:.1f}s)"
    return f"fail ({elapsed:.1f}s)"


def _run_pytest(script: str, env: dict) -> tuple[int, str, str, float]:
    cmd = [sys.executable, "-m", "pytest", str(ROOT / script), "-v"]
    t0 = time.monotonic()
    proc = subprocess.run(
        cmd,
        cwd=str(REPO_ROOT),
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.returncode, proc.stdout, proc.stderr, time.monotonic() - t0


def _run_script(script: str, env: dict) -> tuple[int, str, str, float]:
    cmd = [sys.executable, str(ROOT / script)]
    t0 = time.monotonic()
    proc = subprocess.run(
        cmd,
        cwd=str(REPO_ROOT),
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.returncode, proc.stdout, proc.stderr, time.monotonic() - t0


def main() -> int:
    parser = argparse.ArgumentParser(description="ssh-mcp v4.3 chaos / stress runner")
    parser.add_argument(
        "--skip-stress", action="store_true", help="skip the long stress suites"
    )
    parser.add_argument(
        "--skip-chaos", action="store_true", help="skip the chaos suites"
    )
    parser.add_argument(
        "--skip-stdio", action="store_true", help="skip the test_stdio pytest suite"
    )
    parser.add_argument(
        "--quiet", action="store_true", help="hide per-suite stdout streams"
    )
    args = parser.parse_args()

    env = os.environ.copy()
    env.setdefault("PATH", f"{REPO_ROOT / 'target' / 'release'}:{env.get('PATH', '')}")
    env.setdefault("SSH_MCP_BIN", str(REPO_ROOT / "target" / "release" / "ssh-mcp"))
    env.setdefault(
        "SSH_MCP_STDIO_BIN",
        str(REPO_ROOT / "target" / "release" / "ssh-mcp-stdio"),
    )

    results: list[tuple[str, str, float]] = []
    overall_ok = True

    for name, script, label, kind in SUITES:
        if kind == "stress" and args.skip_stress:
            results.append((label, "skip", 0.0))
            continue
        if kind == "chaos" and args.skip_chaos:
            results.append((label, "skip", 0.0))
            continue
        if kind == "stdio_pytest" and args.skip_stdio:
            results.append((label, "skip", 0.0))
            continue

        print(f"\n=== {label} ({script}) ===", flush=True)
        if kind == "stdio_pytest":
            rc, out, err, elapsed = _run_pytest(script, env)
        else:
            rc, out, err, elapsed = _run_script(script, env)

        if not args.quiet:
            sys.stdout.write(out)
            if err:
                sys.stderr.write(err)
                sys.stderr.flush()
            sys.stdout.flush()

        if kind == "stdio_pytest":
            status = _format_stdio_pytest(rc, elapsed)
        elif kind == "stress":
            status = _format_stress(_last_json_line(out))
        else:  # chaos
            key = name  # chaos_errors -> "chaos_errors", etc.
            status = _format_chaos(_last_json_line(out), key)

        if rc != 0 and "skip" not in status:
            status = f"{status} (rc={rc})"
        if "ok" not in status and "skip" not in status:
            overall_ok = False
        results.append((label, status, elapsed))

    print("\n=== summary ===", flush=True)
    width = max(len(r[0]) for r in results) + 2
    for label, status, elapsed in results:
        suffix = f"  ({elapsed:.1f}s)" if elapsed > 0.0 else ""
        print(f"  {label:<{width}}{status}{suffix}", flush=True)

    # Final JSON summary line per the v4.7 spec.
    suites_passed = sum(1 for _, s, _ in results if "ok" in s)
    suites_failed = sum(1 for _, s, _ in results if "fail" in s.lower())
    suites_skipped = sum(1 for _, s, _ in results if "skip" in s.lower())
    final = {
        "status": "ok" if overall_ok else "fail",
        "suites_passed": suites_passed,
        "suites_failed": suites_failed,
        "suites_skipped": suites_skipped,
        "total_scenarios": len(results),
        "errors": [
            {"label": label, "status": status}
            for label, status, _ in results
            if "fail" in status.lower()
        ],
    }
    print(json.dumps(final), flush=True)
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main())
