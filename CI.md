# Continuous Integration (CI)

## ssh-mcp Testing

Local + GitHub Actions gates run the same canonical command set. The gates that every PR must pass before review are listed below — each one is the exact command CI executes.

### Pre-commit gates (must pass)

| Gate | Command | What it verifies |
| --- | --- | --- |
| Build | `cargo build --release --all-features` | All three binaries (`ssh-mcp`, `ssh-mcp-stdio`, `ssh-mcp-tail`) compile clean with `port_forward` enabled |
| Format | `cargo fmt --all -- --check` | Source tree matches `rustfmt` (Rust 2024 edition style) |
| Lint | `cargo clippy --release --all-features --all-targets --workspace -- -D warnings` | Strict baseline: forbid (`unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `exit`, `mem_forget`, `infinite_loop`, `print_*`) + deny (`pedantic`, `nursery`, `cargo`, lock-free invariants `await_holding_lock`, `mutex_atomic`, `mutex_integer`, `significant_drop_*`) |
| Lib tests | `cargo test --lib --quiet` | 1966 unit tests against use cases, ports, adapters, domain entities |
| Integration tests | `cargo test --tests --features test-fixtures --quiet` | 134 integration tests across 9 binaries (see matrix below) |

### PR Testing Workflows

The following gates run on every pull request:

| Job | Description | Toolchain | Features |
| --- | --- | --- | --- |
| Build | Compiles all three binaries on Linux + macOS | rust 1.95+ | `default,port_forward` |
| Build (no-default) | Compiles without `port_forward` (38 tools, smaller binary) | rust 1.95+ | `--no-default-features` |
| Fmt | `cargo fmt --all -- --check` | rust 1.95+ | n/a |
| Clippy | Strict lint gate | rust 1.95+ | `--all-features --all-targets` |
| Lib tests | 1966 unit tests | rust 1.95+ | default |
| Integration tests | 134 integration tests across 9 binaries | rust 1.95+ | `test-fixtures` |
| Property tests | proptest suites (`property` 32 + `property_rsync` 9) | rust 1.95+ | `test-fixtures` |
| Chaos tests | Adversarial scheduler suites (`chaos` 41 + `chaos_rsync` 16) | rust 1.95+ | `test-fixtures` |
| Loom invariants | Lock-free model checking (`tests/lockfree_invariants*.rs`, gated `#[cfg(loom)]`) | rust 1.95+ + `RUSTFLAGS="--cfg loom"` | `test-fixtures` |
| Python integration | `pytest scripts/test_*.py` against running daemon / HTTP / VM | python 3.13 | n/a |

### Integration test matrix

The 134 integration tests are split across 9 binaries — each one targets a specific subsystem and runs against deterministic fixtures (no real SSH server needed for default suites; `e2e-vm` is gated and opt-in).

| Binary | Tests | Covers |
| --- | ---: | --- |
| `tests/v4_smoke.rs` | 2 | v4 wire-format smoke (legacy `ssh_*` channel) |
| `tests/v5_smoke.rs` | 8 | v5 lifecycle binding + channel mux + sub_id |
| `tests/v5_daemon_smoke.rs` | 5 | NDJSON daemon protocol end-to-end |
| `tests/v6_resume_smoke.rs` | 12 | SFTP resume + verify (ADR 0010) |
| `tests/v7_rsync_smoke.rs` | 9 | rsync hybrid transport (ADR 0011) |
| `tests/chaos.rs` | 41 | Adversarial scheduling — backpressure, lifecycle races, lane fairness |
| `tests/chaos_rsync.rs` | 16 | rsync transport adversarial paths |
| `tests/property.rs` | 32 | proptest invariants on cursor monotonicity, lag policies, lifecycle CAS |
| `tests/property_rsync.rs` | 9 | proptest invariants on rsync block-match path, hash kernels |

### End-to-end VM tests (gated)

The `e2e-vm` feature unlocks 8 wire-real tests that connect to a Linux VM running OpenSSH + `rsync 3.2.7`. Off by default; opt-in with `--features e2e-vm` and a reachable `vm.services` host.

| Binary | Tests | Covers |
| --- | ---: | --- |
| `tests/v7_rsync_e2e_vm.rs` | 2 | SFTP transport against live OpenSSH |
| `tests/v7_rsync_wire_e2e_vm.rs` | 6 | Wire transport — push and pull byte-identical against `rsync 3.2.7` |

### Loom invariants

Lock-free correctness is checked under the `loom` model checker. 27 invariants across two files, gated `#[cfg(loom)]` and run with `RUSTFLAGS="--cfg loom"`:

| File | Invariants | Covers |
| --- | ---: | --- |
| `tests/lockfree_invariants.rs` | 20 | Lifecycle CAS race, grace fire vs re-subscribe, cascade double-disconnect, cursor monotonicity, mux fairness, lane mpsc full + drop_oldest, concurrent lane add/remove during drain |
| `tests/lockfree_invariants_rsync.rs` | 7 | rsync block-match path under contention, hashtable rebuild, sliding-window cursor advance |

Full loom mode (model-checking the whole binary) is currently blocked by upstream `tokio` / `loom` incompatibility in `russh` + `axum` deps — invariants run in scoped harnesses instead.

### Python integration suites

`scripts/test_*.py` exercises the live MCP wire format end-to-end against a running ssh-mcp instance. The v7.0 `ssh_rsync` surface is covered by three transports — HTTP (`scripts/test_v7_rsync_http.py`), stdio (`scripts/test_v7_rsync_stdio.py`), and VM (`scripts/test_v7_rsync_vm.py`). 21 tests / 19 passed + 2 xfailed (the xfails cover the deferred local-FS adapter for `RsyncSftpFsPort`).

### Stress scripts

Optional load profiles — not part of the PR gate, used for performance regression hunts:

- `scripts/stress_concurrent_sessions.py`
- `scripts/stress_lane_fanout.py`
- `scripts/stress_rsync_throughput.py`
- `scripts/stress_subscription_churn.py`
- `scripts/stress_transfer_resume.py`

## Reproducibility

The full PR gate can be reproduced locally in one command:

```bash
cargo build --release --all-features                                      \
 && cargo fmt --all -- --check                                            \
 && cargo clippy --release --all-features --all-targets --workspace -- -D warnings \
 && cargo test --lib --quiet                                              \
 && cargo test --tests --features test-fixtures --quiet
```

If any step fails, fix the code — never disable a lint to silence a warning, and never bypass a failing test with `#[ignore]` without an issue link.
