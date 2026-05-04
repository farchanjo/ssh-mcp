# ssh-mcp v5.0 — Subscribe-First SSH Model Context Protocol Server

[![Version](https://img.shields.io/badge/version-5.0.0--rc1-blue.svg)]()
[![Rust](https://img.shields.io/badge/rust-2024%20%E2%80%94%20MSRV%201.95-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](Cargo.toml)
[![MCP](https://img.shields.io/badge/MCP-2025--06--18-purple.svg)]()
[![Architecture](https://img.shields.io/badge/architecture-hexagonal-purple.svg)]()

> Push-first observability. Lock-free hot path. Lifecycle-bound resources.
> Self-cleaning on disconnect. NDJSON daemon for hosts without subscribe.

> [!CAUTION]
> Not the original [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp). Rewritten on `russh` 0.55 + `rmcp` 1.6 with a strict hexagonal layout. v5.0 is wire-compatible with every v3 / v4 host on the legacy 21-tool catalogue and adds 9 net-new MCP tools plus a third binary (`ssh-mcp-tail`) for tier-2 / tier-3 LLM hosts that do not surface push notifications to the model.

## TL;DR

| You want | You call |
|---|---|
| One-shot command | `ssh_run` |
| Long command + push | `ssh_execute` + `ssh_subscribe` |
| Interactive shell | `ssh_shell_open` + `ssh_subscribe` + `ssh_shell_*` |
| File transfer + progress | `ssh_upload` + `ssh_subscribe` |
| Cleanup my sessions | `ssh_disconnect_agent` |
| Hosts without subscribe | use `ssh-mcp-tail daemon` (NDJSON pipeline) |

Why subscribe-first? See [docs/llm-ux/GOLDEN_RULES.md](docs/llm-ux/GOLDEN_RULES.md) and [ADR 0003](docs/adr/0003-lifecycle-binding.md).

## Quickstart (push-first, ~30 s)

```bash
git clone https://github.com/farchanjo/ssh-mcp.git && cd ssh-mcp
cargo build --release
sudo cp target/release/ssh-mcp{,-stdio,-tail} /usr/local/bin/
ssh-mcp-stdio          # stdio transport (recommended for local MCP hosts)
```

In your MCP host (mcp-inspector / Claude Desktop / Cline) call:

```text
ssh_connect         host=vm.example.com user=root key=~/.ssh/id_rsa agent_id=alice
ssh_execute         session_id=...        command="tail -f /var/log/app.log" release_when_no_subs=true
ssh_subscribe       uri=command://<cid>/output  lifetime=auto-close lag_policy=snapshot
# drain push events on resources/updated until ev=completed
ssh_unsubscribe     sub_id=...
ssh_disconnect_agent agent_id=alice
```

Every long-running tool emits a `HINT: REQUIRED NEXT STEP:` line that names the exact successor. See [docs/llm-ux/PROMPTS_CATALOG.md](docs/llm-ux/PROMPTS_CATALOG.md) for the full 10-prompt library.

## Quickstart (NDJSON daemon)

For hosts without `resources/subscribe` (Claude Code CLI, several IDE integrations) drive `ssh-mcp-tail` as a subprocess:

```bash
cat <<'EOF' | ssh-mcp-tail daemon | jq 'select(.ev == "push")' | tee log.ndjson
{"op":"connect","host":"vm.example.com","user":"root","key":"/home/user/.ssh/id_rsa","id":"c1"}
{"op":"exec","sid":"<sid-from-ack>","cmd":"tail -f /var/log/app.log","id":"c2"}
{"op":"subscribe","uri":"command://<cid>/output","lifetime":"auto-close","lag_policy":"snapshot","id":"c3"}
EOF
```

Stdin is NDJSON ops; stdout is NDJSON events (`ack`, `push`, `completed`, `lagged`, `snapshot`, `warn`, `heartbeat`, `daemon_stats`); stderr is `RUST_LOG`-controlled tracing. Full op + event schema: [docs/INSTRUCTIONS_DAEMON.md](docs/INSTRUCTIONS_DAEMON.md).

## Architecture (1 paragraph)

ssh-mcp v5.0 keeps the hexagonal v4.1 split (`domain` / `ports` / `application` / `adapters` / `infra` / `composition`) and adds three v5 layers: a **lifecycle adapter** ([ADR 0003](docs/adr/0003-lifecycle-binding.md)) wiring every long-lived resource to a CAS state machine + cascade refcount + grace timer; a **subscription mux** ([ADR 0004](docs/adr/0004-channel-mux-fairness.md)) keying push lanes on `(SubId, Uri)` with per-lane LagPolicy / filter / stats; and an **embed transport** ([ADR 0008](docs/adr/0008-ndjson-daemon-protocol.md)) that lets `ssh-mcp-tail` host an in-process MCP client + server pair across `tokio::io::duplex`. The hot path stays lock-free — every new field is `AtomicU8` / `AtomicUsize` / `AtomicU64` / `ArcSwap<Policy>` / `Notify`, never `Mutex`. Full layer-by-layer module map: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## When to use which transport

| Transport | Best for |
|---|---|
| `ssh-mcp` | HTTP host (Claude Desktop, browsers, axum-fronted services) |
| `ssh-mcp-stdio` | stdio MCP host (mcp-inspector, IDEs, Cline) |
| `ssh-mcp-tail` | Unix pipelines, hosts without `resources/subscribe` (Claude Code CLI, IDE integrations) |

All three reuse `composition::prod` adapters. Only the transport binding differs. See [docs/ARCHITECTURE.md#binary-targets](docs/ARCHITECTURE.md).

## MCP tool catalogue

v5.0 ships **30 tools** (29 without `port_forward`):

- **21 carry-over from v4.7 / v4.8** — `ssh_connect`, `ssh_disconnect`, `ssh_disconnect_many`, `ssh_list_sessions`, `ssh_disconnect_agent`, `ssh_execute`, `ssh_execute_batch`, `ssh_run`, `ssh_get_command_output`, `ssh_list_commands`, `ssh_cancel_command`, `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_send_key`, `ssh_shell_read`, `ssh_shell_wait_for`, `ssh_shell_close`, `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress`, `ssh_forward`.
- **9 net-new in v5.0 (Phase 3)** — `ssh_subscribe`, `ssh_unsubscribe`, `ssh_sub_pause`, `ssh_sub_resume`, `ssh_sub_filter`, `ssh_sub_replay`, `ssh_sub_list`, `ssh_sub_stats`, `ssh_daemon_stats`. All key on the new `SubId` (UUIDv7) introduced by [ADR 0004](docs/adr/0004-channel-mux-fairness.md).

Per-tool input / output schemas, response shape, and structured payload: [docs/API.md](docs/API.md). Phase 3 implementation is in flight on `feat/v5-foundation` — Phase 1 + 2 (lifecycle + mux) are merged.

## Resource streams (5 schemes, subscribe-first)

| URI scheme | Pushes | Cursor | MIME |
|---|---|---|---|
| `shell://<shell-id>/output` | PTY output stream | yes | `text/plain` |
| `command://<command-id>/output` | Async command stdout/stderr | yes | `text/plain` |
| `transfer://<transfer-id>/progress` | SFTP point-in-time progress | no | `application/json` |
| `session://<session-id>/health` | Session health snapshot | no | `application/json` |
| `forward://<forward-id>/events` (feature `port_forward`) | Port-forward event log | yes | `application/json` |

Producer events coalesce on `SSH_NOTIFY_DEBOUNCE_MS` (default 50 ms), force-flush after `SSH_NOTIFY_FORCE_FLUSH_MS` (default 1 s), keepalive every `SSH_NOTIFY_KEEPALIVE_S` (default 30 s). Each event carries a sequence number for gap detection; v5 lagged subscribers auto-recover via the per-lane `Snapshot` policy ([ADR 0006](docs/adr/0006-backpressure-policies.md)). Full contract: [docs/RESOURCES.md](docs/RESOURCES.md). Token-efficient subscribe-first patterns: [docs/llm-ux/GOLDEN_RULES.md](docs/llm-ux/GOLDEN_RULES.md).

## Configuration

Three-tier resolution: **Parameter > Environment Variable > Built-in default**. Full env-var table (33+ vars including v5 lifecycle / lane / mux / daemon knobs): [docs/CONFIGURATION.md](docs/CONFIGURATION.md). The five v5 categories:

- Lifecycle ([ADR 0003](docs/adr/0003-lifecycle-binding.md)) — `SSH_LIFECYCLE_GRACE_MS`, `SSH_LIFECYCLE_OWN_GRACE_MS`, `SSH_SESSION_IDLE_GRACE_MS`.
- Lane / mux ([ADR 0006](docs/adr/0006-backpressure-policies.md)) — `SSH_LAG_POLICY_DEFAULT`, `SSH_LANE_BUFFER`, `SSH_MUX_BUFFER`, `SSH_BP_BLOCK_TIMEOUT_MS`, `SSH_REPLAY_WINDOW_BYTES`, `SSH_FILTER_REGEX_MAX`, `SSH_MAX_SUBS_PER_URI`, `SSH_MAX_SUBS_TOTAL`.
- LLM hygiene ([ADR 0005](docs/adr/0005-llm-ux-priorities.md)) — `SSH_SUB_LEAK_RISK_WARN_S`, `SSH_SUB_LEAK_RISK_KILL_S`.
- NDJSON daemon ([ADR 0008](docs/adr/0008-ndjson-daemon-protocol.md)) — `SSH_NDJSON_LINE_MAX`, `SSH_HEARTBEAT_INTERVAL_S`, `SSH_DAEMON_STATS_INTERVAL_S`, `SSH_GRACE_HARD_TIMEOUT_S`.

## Troubleshooting

Common failure shapes (`SUB_LEAK_RISK`, `LAG_BACKPRESSURE`, `RESOURCE_GONE`, `LANE_BUFFER_FULL`, ...): [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md). Code-by-code error handbook: [docs/llm-ux/ERROR_HANDBOOK.md](docs/llm-ux/ERROR_HANDBOOK.md). Anti-patterns: [docs/llm-ux/ANTIPATTERNS.md](docs/llm-ux/ANTIPATTERNS.md).

## Development

```bash
cargo build --release                                              # all three binaries
cargo build --release --bin ssh-mcp                                # HTTP only
cargo build --release --bin ssh-mcp-stdio                          # stdio only
cargo build --release --bin ssh-mcp-tail                           # NDJSON daemon only
cargo build --release --no-default-features                        # without port_forward
cargo test --lib --quiet                                           # ~1378 unit tests (Phase 1+2 merged)
cargo test --tests --quiet                                         # 2 integration tests
cargo test --features test-fixtures                                # use cases against in-memory adapters
cargo fmt --all -- --check
cargo clippy --release --all-features -- -D warnings               # production gate (must exit 0)
```

The clippy gate is **production-only**. The `forbid(unwrap_used / expect_used)` policy is structurally incompatible with `#[tokio::test]` macro expansion; test targets are gated by `cargo test --lib` and `cargo build --release --all-targets`. Details: [CLAUDE.md](CLAUDE.md).

For changes touching shell / command / transfer hot-path state, read [docs/LOCKS.md](docs/LOCKS.md) before introducing any `Mutex`. For changes spanning multiple layers, [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) shows where each layer's responsibility lies.

## Documentation map

| Document | Description |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Hexagonal layout, v5 layers (lifecycle / mux / embed), per-module map, sequence diagrams |
| [docs/API.md](docs/API.md) | All 30 MCP tools (inputs, outputs, structured_content, errors) |
| [docs/RESOURCES.md](docs/RESOURCES.md) | 5 resource schemes, cursor / sequence semantics, `_meta` envelope |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | Full env-var table (33+ vars, floors and caps, tuning profiles) |
| [docs/ERRORS.md](docs/ERRORS.md) | Wire-format error envelope (REASON + DETAIL) |
| [docs/LOCKS.md](docs/LOCKS.md) | Lock-free invariants per layer + Clippy enforcement |
| [docs/FLOWS.md](docs/FLOWS.md) | Sequence diagrams for connect / execute / shell / SFTP / subscribe |
| [docs/INSTRUCTIONS_DAEMON.md](docs/INSTRUCTIONS_DAEMON.md) | `ssh-mcp-tail daemon` NDJSON op + event schema |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Symptom -> cause -> cure runbook |
| [docs/MIGRATION_v4_to_v5.md](docs/MIGRATION_v4_to_v5.md) | v4.x to v5.0 host migration guide |
| [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) | Contributor migration guide (v4.1 deep-decouple addendum) |
| [docs/llm-ux/](docs/llm-ux/) | LLM UX kit: golden rules, prompts catalog, anti-patterns, error handbook, 27B / 70B root prompts |
| [docs/adr/](docs/adr/) | 8 architecture decision records (0001 rmcp, 0002 hexagonal, 0003 lifecycle, 0004 mux, 0005 LLM UX, 0006 backpressure, 0007 errors, 0008 daemon) |

## Contributing

See [docs/adr/0002-adopt-hexagonal-architecture.md](docs/adr/0002-adopt-hexagonal-architecture.md) for the architecture invariants this project enforces. Open issues at <https://github.com/farchanjo/ssh-mcp/issues>; pre-commit must pass `cargo fmt --all -- --check` and `cargo clippy --release --all-features -- -D warnings`.

## License

MIT — declared via `license = "MIT"` in `Cargo.toml`.

## Author and links

- Author: Fabricio Archanjo (<fabricio@archanjo.com>)
- Issues: <https://github.com/farchanjo/ssh-mcp/issues>
- Releases: <https://github.com/farchanjo/ssh-mcp/releases>
- Original concept: [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp)
- SSH library: [russh](https://github.com/warp-tech/russh) — SFTP via [russh-sftp](https://github.com/AspectUnk/russh-sftp)
- MCP framework: [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Rust SDK)
- HTTP host: [axum](https://github.com/tokio-rs/axum) + [tower](https://github.com/tower-rs/tower)
- Lock-free primitives: [arc-swap](https://github.com/vorner/arc-swap), [dashmap](https://github.com/xacrimon/dashmap)
