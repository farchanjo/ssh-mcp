# CLAUDE.md

## Build Commands

```bash
cargo build --release                              # Build all binaries (default + port_forward)
cargo build --release --bin ssh-mcp                # HTTP server only (axum 0.8 + rmcp 1.6)
cargo build --release --bin ssh-mcp-stdio          # Stdio transport only (rmcp 1.6 stdio)
cargo build --release --no-default-features        # Without port forwarding
cargo test --lib --quiet                           # 1014 lib tests
cargo test --tests --quiet                         # 2 integration tests (incl. v4 smoke)
cargo test --all-features                          # Combined run
cargo test --features test-fixtures                # Use cases with deterministic in-memory adapters
cargo fmt --all -- --check                         # Check formatting
cargo clippy --all-features --all-targets --workspace -- -D warnings   # Lint (strict baseline)
```

## Architecture (v4.1.0 — Hexagonal / Ports and Adapters, deep-decoupled)

The public MCP API is unchanged from v3 / v4.0 (same 18 tools, 5 resource schemes, markdown shape, env vars). v4 is an internal restructuring; v4.1 closed the H17.6 deferred decouple — `src/mcp/` is gone, `async-trait` is no longer a direct dep. See `docs/MIGRATION_v3_to_v4.md` for the contributor guide and `docs/ARCHITECTURE.md` for the full layer-by-layer module map.

### Binary Targets

- **ssh-mcp** (`src/main.rs`): HTTP transport via `axum` 0.8 + `rmcp::transport::streamable_http_server::StreamableHttpService`. Tracks sessions through `Mcp-Session-Id` header. Default bind `0.0.0.0:8000`, path `/`. Root mount uses `Router::fallback_service` (axum 0.8 panics on a nested `/` mount).
- **ssh-mcp-stdio** (`src/bin/ssh_mcp_stdio.rs`): Stdio MCP transport via `rmcp::transport::io::stdio()`. Logs to stderr via `RUST_LOG`.

Both binaries are thin shells over `composition::prod` — only the transport differs. They each spawn a background **peer-GC task** that scans the subscription registry on `SSH_MCP_PEER_GC_INTERVAL_S` (default 30s) and drops peers whose rmcp transport closed (rmcp 1.6 does not surface a peer-disconnect callback).

### Layers

| Layer | Path | Responsibility |
|-------|------|----------------|
| **domain** | `src/domain/` | Pure entities, value objects, errors, live event variants. No I/O, no async. |
| **ports** | `src/ports/` | Trait skeletons. Sync via plain trait, async via `trait-variant` AFIT. No `Box<dyn Future>` for v4 ports. |
| **application** | `src/application/` | 22 use cases (one struct per business operation). Generic over the ports they depend on — static dispatch, no virtual calls in hot paths. Unit-testable against in-memory fakes with zero rmcp / russh / SFTP machinery. |
| **adapters** | `src/adapters/` | Concrete implementations of every port. Production: russh, russh-sftp, DashMap, env-config, UUID v4. Test: in-memory + `FakeClock` + `DeterministicIdGen` (gated by the `test-fixtures` feature). |
| **infra** | `src/infra/mcp/` | Inbound rmcp surface: `McpSshServer<UC>`, the 18 `#[tool]` entry points (`tool_router.rs`), `resources/*` handlers, `PeerHandle` plumbing, per-tool args (`Deserialize + JsonSchema`), per-domain markdown render, helpers (error / nonce / output). |
| **composition** | `src/composition/` | Wiring root. `prod.rs` pins concrete adapters at compile time so wiring errors surface at `cargo build` rather than runtime. `fixtures.rs` wires deterministic adapters for tests. |

### Adapters quick map

| Adapter | Path | Port |
|---------|------|------|
| `RusshClient` (+ `SshHandleRegistry`) | `src/adapters/ssh/` | `ports::ssh_client::SshClientPort` |
| `RusshSftpClient` (+ `InMemorySftp`) | `src/adapters/sftp/` | `ports::sftp_client::SftpClientPort` |
| `DashMap*Repo` (session, command, shell, transfer, forward) | `src/adapters/repo/dashmap/` | `ports::*_repo::*RepoPort` |
| `AuthChain` (`PasswordAuth` → `KeyAuth` → `AgentAuth`) | `src/adapters/auth/` | `ports::auth_strategy::AuthStrategyPort` |
| `MemoryRegistry<N>` (generic over notifier) | `src/adapters/subscription/memory_registry.rs` | `ports::subscriber_registry::SubscriberRegistryPort` |
| `RmcpAdapter` + `RmcpPeer` | `src/adapters/notifier/` | `ports::notifier::NotifierPort` + `PeerHandle` |
| `RusshOutput` + `InMemory` | `src/adapters/output_stream/` | `ports::output_stream::OutputStreamPort` |
| `SystemClock` + `FakeClock` | `src/adapters/clock/` | `ports::clock::ClockPort` |
| `EnvConfig` | `src/adapters/config/` | `ports::config::ConfigPort` |
| `UuidIdGenerator` + `DeterministicIdGenerator` | `src/adapters/id_generator/` | `ports::id_generator::IdGeneratorPort` |

### Adapter-internal modules

Each runtime adapter owns its private internals under `internal/` (or, for the subscription registry, a transitional `legacy.rs`). Use cases never touch these — they consume the port surface only.

- `src/adapters/ssh/internal/{client,session,async_command,shell,types,error}.rs` — russh wiring (`connect_to_ssh_with_retry`, `execute_ssh_command`, `open_pty_shell`, `SshClientHandler`), `RunningCommand`, `RunningShell` + `RingBuffer`, shared payload structs (`SessionInfo`, `AsyncCommandInfo`, `ShellInfo`, `TransferInfo`), retry classification.
- `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` — internal `AuthStrategyPort` trait + native AFIT chain (PasswordAuth -> KeyAuth -> AgentAuth) statically dispatched through an enum (no `async-trait`).
- `src/adapters/sftp/internal/{sftp,transfer,types}.rs` — streaming SFTP session helpers, `RunningTransfer` lock-free state, shared payload structs.
- `src/adapters/config/internal/mod.rs` — env-var resolvers backing `EnvConfig`.
- `src/adapters/subscription/legacy.rs` — transitional `SubscriptionRegistry` + `SUBSCRIPTION_REGISTRY` global + `spawn_peer_gc`. The hexagonal `MemoryRegistry<N>` adapter at `memory_registry.rs` is the forward-looking replacement consumed by use cases; the legacy adapter coexists until the SSH/SFTP runtime adapters are wired through the port surface end to end.

### Response Format (block-only, byte-compatible with v3)

All 18 MCP tools return a single markdown `Text<String>`:

- First line: `TOOL_NAME: STATUS` (e.g. `SSH_CONNECT: OK`).
- One `KEY: value` per line.
- All IDs suffixed with `_ID` (`SESSION_ID`, `COMMAND_ID`, `SHELL_ID`, `TRANSFER_ID`).
- Output blocks use an 8-hex-char nonce per response: `--- stdout [a3f2b1d7] ---\n<content>\n--- stderr [a3f2b1d7] (empty) ---`.
- Errors: `SSH_X: ERROR\nREASON: [CODE] description\nDETAIL: optional detail`.

The v4 markdown shape is byte-identical to v3 (verified by snapshot tests in `tests/v4_smoke.rs`). v3 hosts work against v4 servers without any change.

### MCP Tools (18 total — unchanged from v3)

- **Connection**: `ssh_connect` (typed `ReusePolicy { Suggest, Auto, ForceNew }`), `ssh_disconnect`, `ssh_list_sessions`, `ssh_disconnect_agent`.
- **Commands**: `ssh_execute` (optional `pty=true`), `ssh_get_command_output`, `ssh_list_commands` (typed `CommandStatus`), `ssh_cancel_command`.
- **Shell** (subscribe-first via `shell://<id>/output`): `ssh_shell_open` (tunable `inactivity_ttl`, `max_buffer_size`), `ssh_shell_write`, `ssh_shell_send_key` (semantic keystrokes + modifiers + repeat), `ssh_shell_read` (long-poll: `wait` / `wait_timeout_secs` / `min_bytes`; head-paginated with `clear=true`), `ssh_shell_wait_for` (multi-pattern gate), `ssh_shell_close`.
- **SFTP**: `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress`.
- **Network**: `ssh_forward` (feature-gated: `port_forward`).

Each session serializes one russh channel at a time through a per-session semaphore (`CHANNEL_CONCURRENCY_PER_SESSION = 1`) so rapid `execute + cancel` bursts never race OpenSSH's `MaxSessions` budget. The shared `SshHandleRegistry` lets the SFTP adapter reuse the russh handle for file transfers.

### MCP Resources (5 schemes, subscribe-first — unchanged from v3)

| Scheme | Description | Cursor |
|--------|-------------|--------|
| `shell://<id>/output` | PTY output stream | yes (`?cursor=auto` or absolute byte offset) |
| `command://<id>/output` | Async command stdout/stderr | yes |
| `transfer://<id>/progress` | SFTP point-in-time progress | no (snapshot) |
| `session://<id>/health` | Session health snapshot | no |
| `forward://<id>/events` | Port-forward event log (feature-gated) | yes |

Subscriptions go through the `MemoryRegistry<N>` (generic over the notifier port — no `Box<dyn>`). The debouncer coalesces events on `SSH_NOTIFY_DEBOUNCE_MS` (default 50ms), force-flushes after `SSH_NOTIFY_FORCE_FLUSH_MS` (default 1000ms), and sends a keepalive every `SSH_NOTIFY_KEEPALIVE_S` (default 30s). Each event carries a sequence number for gap detection; lagged subscribers auto-recover by serving a snapshot from the buffer.

See `docs/RESOURCES.md` for the full resource contract.

### Configuration

All settings follow: **Parameter -> Environment Variable -> Default**. The full table (25+ env vars) lives in `docs/CONFIGURATION.md`. Identical to v3 — every name, default, floor, and cap kept.

### Error Handling

- **Retryable**: Connection refused, timeout, network unreachable (exponential backoff via `backon`, max 10s).
- **Non-retryable**: Authentication failures, permission denied.
- All tool returns are `Result<CallToolResult, McpError>` (rmcp). Internal layers use `Result<T, DomainError>` (`thiserror`) with structured variants per failure class.

## Code Standards

### Clippy Configuration

Strict clippy enforcement via `Cargo.toml` `[lints.clippy]`:

- **Lint groups**: `clippy::all`, `clippy::pedantic`, `clippy::nursery`, `clippy::cargo` at `deny`.
- **Layer A (forbid)**: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `exit`, `mem_forget`, `infinite_loop`, `print_stdout`, `print_stderr`.
- **Lock-free invariants** (deny): `await_holding_lock`, `await_holding_refcell_ref`, `significant_drop_in_scrutinee`, `significant_drop_tightening`, `mutex_atomic`, `mutex_integer`. Hot-path state types (`RunningCommand`, `RunningShell`, `RunningTransfer`, `SessionRef`, `ForwardHandle`) carry **zero** `Mutex` fields.
- **Quality denies**: `wildcard_enum_match_arm`, `as_conversions`, `clone_on_ref_ptr`, `implicit_clone`, `ref_patterns`, `absolute_paths`, `pub_use`, `allow_attributes_without_reason`, `format_push_string`, `if_then_some_else_none`, `rc_mutex`, `redundant_type_annotations`, `same_name_method`, `tests_outside_test_module`, etc.
- **Thresholds** (`clippy.toml`): `cognitive-complexity-threshold = 25`, `too-many-lines-threshold = 30`, `too-many-arguments-threshold = 7`, `type-complexity-threshold = 250`.
- **Allowed**: `multiple_crate_versions` (transitive deps from russh / axum).

All `#[allow(...)]` attributes **must** include a `reason = "..."`. Never disable a lint to silence a warning — fix the code instead.

See `docs/LOCKS.md` for the lock-free invariants enforced by these lints (rewritten for v4 — maps every invariant to the layer that owns it).

### General

- Methods < 30 lines, SOLID principles.
- Lock-free everywhere on the hot path: `DashMap`, `ArcSwap`, `OnceCell`, `Atomic*`, `tokio::sync::broadcast`, `tokio::sync::Notify`, `mpsc` for owned-resource serialization.
- v4 use cases generic over their ports — **no `Box<dyn Trait>` in hot paths**. Async ports use `trait-variant` AFIT.
- Match exhaustively (no `_ =>` for closed enums; use `wildcard_enum_match_arm = "deny"`).
- `Arc::clone(&x)` — never `x.clone()` on an `Arc` (`clone_on_ref_ptr = "deny"`).
- 1014 lib tests + 2 integration tests + Python integration suites (`scripts/test_*.py`) + 4 stress scripts (`scripts/stress_*.py`).
- Feature flags: `port_forward` (default: enabled), `test-fixtures` (off — exposes deterministic adapters for downstream tests).
- 8 loom invariant tests in `tests/lockfree_invariants.rs` (gated `#[cfg(loom)]`; full loom mode currently blocked by upstream tokio/loom incompatibility in russh + axum — documented in the test file and `Cargo.toml`).

## v4 Migration Notes

- Public MCP API is **unchanged** from v3 and v4.0. v3 / v4.0 hosts work against v4.1 servers without any change to wire format, tool catalogue, env vars, or markdown response shape.
- The v3 monolith modules `src/mcp/{tools, server, resources, message, storage, schema, keys, forward}` were hard-deleted in H17.5a (~14k LOC).
- The remaining foundational `src/mcp/{client, session, sftp, shell, async_command, transfer, subscription, auth, config, error, types}` set was relocated under the owning adapters in v4.1 (etapa H17.6 P1+P2+P3+P4) and the `src/mcp/` directory deleted entirely.
- `async-trait` is no longer a direct dependency. The v3 strategy chain that pinned it now uses native AFIT inside `src/adapters/ssh/internal/auth/` with an enum dispatcher.

See `docs/MIGRATION_v3_to_v4.md` for the full contributor migration guide (file-path table, per-layer responsibility map, v4.1 deep-decouple addendum).
