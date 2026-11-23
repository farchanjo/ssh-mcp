# Research: Direct Dependency Refresh

Evidence for `plan.md`. Every measurement was taken on 2026-11-22 in
`/var/tmp/ssh-mcp/` with `RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` as
exported, one cargo invocation at a time, no `-j` and no `--release`.
`df -h /tmp /var/tmp` was read before the first build and after the last.

## 1. Setup

```
mkdir -p /var/tmp/ssh-mcp
rsync -a --exclude target --exclude .git --exclude .venv --exclude dist \
  --exclude '*.tar.gz' --exclude doc/.specify --exclude .pytest_cache \
  /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/bumps-base/
rsync -a ... /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/bumps-spike/
rsync -a ... /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/pristine/
```

`bumps-base` was the old-crate copy plus the new tests, used to prove the new
tests pass on the old crates; it was deleted once the results were recorded.
`bumps-spike` carries the whole change. The repository is the source of the
patch.

## 2. Q-1, trace capture

Command, once per binary and per `RUST_LOG` value:

```
/var/tmp/ssh-mcp/trace-probe2.sh <binary> <log> <unset|debug>
```

The script binds `MCP_HOST=127.0.0.1` on a free `MCP_PORT`, sends `GET /`
(406, `content-length: 52`) and then one `initialize` `POST`, and kills the
process. Binaries: `bumps-base/target/debug/ssh-mcp` and
`bumps-spike/target/debug/ssh-mcp`, both from `cargo build --bin ssh-mcp`.

Trace lines with `RUST_LOG=debug`, base 0.6.11:

```
DEBUG request{method=GET uri=/ version=HTTP/1.1}: tower_http::trace::on_request: started processing request
DEBUG request{method=GET uri=/ version=HTTP/1.1}: tower_http::trace::on_response: finished processing request latency=0 ms status=406
DEBUG request{method=POST uri=/mcp version=HTTP/1.1}: tower_http::trace::on_request: started processing request
DEBUG request{method=POST uri=/mcp version=HTTP/1.1}: tower_http::trace::on_response: finished processing request latency=1 ms status=200
DEBUG request{method=POST uri=/mcp version=HTTP/1.1}: tower_http::trace::on_eos: end of stream stream_duration=0 ms
```

Trace lines with `RUST_LOG=debug`, spike 0.7.1:

```
DEBUG request{method=GET uri=/ version=HTTP/1.1}: tower_http::trace::on_request: started processing request
DEBUG request{method=GET uri=/ version=HTTP/1.1}: tower_http::trace::on_response: finished processing request latency=0 ms status=406
DEBUG request{method=GET uri=/ version=HTTP/1.1}: tower_http::trace::on_eos: end of stream stream_duration=0 ms
DEBUG request{method=POST uri=/mcp version=HTTP/1.1}: tower_http::trace::on_request: started processing request
DEBUG request{method=POST uri=/mcp version=HTTP/1.1}: tower_http::trace::on_response: finished processing request latency=1 ms status=200
DEBUG request{method=POST uri=/mcp version=HTTP/1.1}: tower_http::trace::on_eos: end of stream stream_duration=0 ms
```

With `RUST_LOG` unset, neither binary prints a `tower_http` line.

Normalized `diff -u` (timestamp, duration, port and session id removed):
one added line, the fixed-length `on_eos`.

```
+DEBUG request{method=GET uri=/ version=HTTP/1.1}: tower_http::trace::on_eos: end of stream stream_duration=N ms
```

`OnEos` in 0.7.1 is
`fn on_eos(self, trailers: Option<&HeaderMap>, stream_duration: Duration, span: &Span)`
(docs.rs, upstream documentation, not measured here): no response headers reach
the callback, so a custom callback cannot filter the fixed-length line without
also dropping the streamed one. The difference is listed in
`docs/MIGRATION.md`.

## 3. Q-2, default features

`cargo info`:

| Version | `default` |
| --- | --- |
| base64 0.23.1 | `[std, simd-unsafe]` |
| sha2 0.11.1 | `[alloc, oid]` |
| md-5 0.11.0 | `[alloc, oid]` |

`cargo tree -e features -i <crate>@<version>`:

| Crate | Base graph | Spike graph |
| --- | --- | --- |
| base64 0.23.1 | alloc, default, simd-unsafe, std; `default` from `rmcp 3.11.0` | alloc, default, simd-unsafe, std; direct and from `rmcp` |
| sha2 0.11.1 | alloc, default, oid, zeroize | alloc, default, oid, zeroize |
| md-5 | 0.10.6: default, std | 0.11.0: alloc, default, oid |

`simd-unsafe` is already on through `rmcp`; the sha2 feature set does not
change; md-5 changes `std` to `alloc, oid` because 0.11 has no `std`. Keep the
defaults. `unsafe_code = "deny"` (`Cargo.toml:217`) covers this package, not a
dependency.

## 4. Known-answer vectors

The constants come from the standards and were checked with `arithma`; the
256-byte input was checked with the shell tool because `arithma` takes strings.

| Producer | Input | Constant | Source | Check |
| --- | --- | --- | --- | --- |
| `inline_payload_params` | empty | `` (empty) | RFC 4648 §10 | `mcp__arithma__base64Encode` → empty |
| `inline_payload_params` | `hello` | `aGVsbG8=` | RFC 4648 §10 | `mcp__arithma__base64Encode` → `aGVsbG8=` |
| `inline_payload_params` | `0x00..=0xff` | `AAECAw…/w==` (344 chars, in the test) | RFC 4648 alphabet | `base64 -w0 bytes0-255.bin` |
| `sha256_local_prefix` | empty | `e3b0c442…b855` | FIPS 180-4, SHA-256("") | `mcp__arithma__hashSha256` → same |
| `sha256_local_prefix` | `abc` | `ba7816bf…15ad` | FIPS 180-4, SHA-256("abc") | `mcp__arithma__hashSha256` → same |
| `FileHasher::for_protocol` | empty | `d41d8cd9…427e` | RFC 1321 A.5, MD5("") | `mcp__arithma__hashMd5` → same |
| `FileHasher::for_protocol` | `abc` | `90015098…7f72` | RFC 1321 A.5, MD5("abc") | `mcp__arithma__hashMd5` → same |

`bytes0-255.bin` was written with
`python3 -I -c 'open("...","wb").write(bytes(range(256)))'`; `md5sum` and
`sha256sum` on it agree with the values above where used.

## 5. New tests on base and on spike

The three known-answer tests and the trace test were copied to `bumps-base`
unchanged.

```
cargo nextest run --test-threads num-cpus --lib \
  -E 'test(inline_payload_params_known_answers) | test(file_hasher_known_answers) | test(sha256_local_prefix_hashes_only_the_prefix)'
```

- Base: 3 run, 3 passed, exit 0.
- Spike: 3 run, 3 passed, exit 0.

```
cargo nextest run --test-threads num-cpus --test v7_protocol_http \
  -E 'test(trace_layer_emits_six_debug_lines_for_two_responses_and_none_under_info)'
```

- Base: 1 failed, exit 100, timed out after 10 s; the captured log holds five
  `tower_http::trace` lines and lacks the GET `on_eos`. This is the listed
  0.7.1 difference, not a defect.
- Spike: 1 passed, exit 0.

## 6. Stage measurements

Fresh copy of `bumps-base`, steps 1 to 4 in order. After each, the check and
the three known-answer tests, plus `cargo tree -d` for the four crates.

| Step | Update | Lock change | `cargo check --all-features --all-targets` | Known answers | `cargo tree -d` |
| --- | --- | --- | --- | --- | --- |
| 1, tests first | none | none | exit 0 | 3 passed | base64 2, sha2 2, digest 2 |
| 2, base64 | `cargo update -p base64@0.22.1` | removes `base64 0.22.1` | exit 0 | 3 passed | sha2 2, digest 2 |
| 3, sha2 + md-5 + line | `cargo update -p sha2@0.10.9 -p md-5@0.10.6` | removes `block-buffer 0.10.4`, `cpufeatures 0.2.17`, `crypto-common 0.1.7`, `digest 0.10.7`, `generic-array 0.14.7`, `sha2 0.10.9`, `version_check 0.9.5`; `md-5 0.10.6` → `0.11.0` | exit 0 | 3 passed | none |
| 4, tower-http + trace test | `cargo update -p tower-http@0.6.11` | `tower-http 0.6.11` → `0.7.1` | exit 0 | trace test 1 passed | none |

After step 4 the four `diff` commands against `bumps-spike`
(`src`, `tests`, `Cargo.toml`, `Cargo.lock`) print nothing. The lock reached
through the three narrow updates equals the spike lock, so the spike was not
rebuilt.

## 7. Gate on the spike

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo check --all-features --all-targets` | 0 | clean |
| `cargo clippy --all-features -- -D warnings` | 0 | clean |
| `cargo build --no-default-features --target-dir target/no-default-features` | 0 | clean |
| `cargo build --bins` | 0 | clean |
| `cargo nextest run --test-threads num-cpus --lib` | 0 | 2125 passed |
| `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 0 | 2299 passed |
| `cargo audit` | 0 | no vulnerability; `chrono` warning only |
| `python3 scripts/export_tool_catalog.py --check` | 0 | both `unchanged` |

## 8. Python suites

Run from the spike with `SSH_MCP_BIN`, `SSH_MCP_STDIO_BIN` and
`SSH_MCP_TAIL_BIN` pointing at its `target/debug` binaries.

| Suite | Result |
| --- | --- |
| `scripts/test_http.py` | 15 passed |
| `scripts/test_transfer_progress.py` | 1 passed, 1 skipped (paramiko fixture, `:192`) |
| `scripts/test_v7_rsync_http.py` | 10 passed |
| `scripts/test_v47_subscriptions.py` | 20 passed |
| `scripts/test_v47_idempotency.py` | 9 passed |

## 9. Lock difference

`cargo update -p base64@0.22.1 -p sha2@0.10.9 -p md-5@0.10.6 -p tower-http@0.6.11`:

Removed: `base64 0.22.1`, `block-buffer 0.10.4`, `cpufeatures 0.2.17`,
`crypto-common 0.1.7`, `digest 0.10.7`, `generic-array 0.14.7`,
`md-5 0.10.6`, `sha2 0.10.9`, `tower-http 0.6.11`, `version_check 0.9.5`.
Added: `md-5 0.11.0`, `tower-http 0.7.1`. Every other entry kept its version.

## 10. Edits made in the spike

| File | Edit |
| --- | --- |
| `Cargo.toml:89`, `:110`, `:129`, `:131` | four version requirements |
| `Cargo.lock` | the resolution of §9 |
| `src/adapters/notifier/rmcp_adapter.rs:310` | `inline_payload_params_known_answers` |
| `src/adapters/rsync/wire/hash.rs:604` | `file_hasher_known_answers` |
| `src/adapters/sftp/internal/sftp.rs:1553` | `sha256_local_prefix_hashes_only_the_prefix` |
| `tests/v7_protocol_http.rs:26`, `:65`, `:71`, `:318`-`:360` | `launch`, `start_logged`, `request`, `strip_ansi`, `trace_lines`, `wait_for_lines` |
| `tests/v7_protocol_http.rs:364`-`:399` | `trace_layer_emits_six_debug_lines_for_two_responses_and_none_under_info` |
| `tests/v7_rsync_wire_e2e_vm.rs:500` | `LowerHex` formatting of the finalized digest |

No production code changes; no `#[allow]` and no `#[expect]` added. The async
test returns `Result` and uses `?`; its file lives in a `tempfile::tempdir()`.

## 11. Patch

`/var/tmp/ssh-mcp/bumps-spike.patch`, from the repository to `bumps-spike`,
paths `Cargo.toml`, `Cargo.lock`, `src/`, `tests/`, in `a/<path>` and
`b/<path>` form. Seven files:

```
 Cargo.toml                            |    8 +
 Cargo.lock                            |  180 ++++++++++-----------------------
 src/adapters/notifier/rmcp_adapter.rs |   31 ++++++
 src/adapters/rsync/wire/hash.rs       |   16 +++
 src/adapters/sftp/internal/sftp.rs    |   24 ++++
 tests/v7_protocol_http.rs             |  171 ++++++++++++++++++++++++++++---
 tests/v7_rsync_wire_e2e_vm.rs         |    6 +
 7 files changed, 285 insertions(+), 151 deletions(-)
```

`git apply --check -p1` against the repository exits 0. SHA-256:
`41d0506fec230d904698fd435ac9294155634b9f0a0a7e06a7c28169ac54a23f`.
