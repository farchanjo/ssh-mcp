# CLAUDE.md

## Build Commands

```bash
cargo build --release                              # Build all binaries (default + port_forward)
cargo build --release --bin ssh-mcp                # HTTP server only (axum + rmcp 1.6)
cargo build --release --bin ssh-mcp-stdio          # Stdio transport only (rmcp 1.6 stdio)
cargo build --release --no-default-features        # Without port forwarding
cargo test --lib --quiet                           # 820 lib tests
cargo test --tests --quiet                         # 12 integration tests
cargo test --all-features                          # Combined run
cargo fmt --all -- --check                         # Check formatting
cargo clippy -- -D warnings                        # Lint (strict baseline)
```

## Architecture (v3.0.0)

### Binary Targets
- **ssh-mcp** (`src/main.rs`): HTTP transport via `axum` 0.7 + `rmcp::transport::streamable_http_server::StreamableHttpService`. Tracks sessions through `Mcp-Session-Id` header. Default bind `0.0.0.0:8000`, path `/`.
- **ssh-mcp-stdio** (`src/bin/ssh_mcp_stdio.rs`): Stdio MCP transport via `rmcp::transport::io::stdio()`. Logs to stderr via `RUST_LOG`. The legacy custom JSON-RPC shim (cancel-id parser, fallback responses) was removed in v3 — rmcp handles it natively.

Both binaries spawn a background **peer-GC task** (`spawn_peer_gc`) that scans `SUBSCRIPTION_REGISTRY` on `SSH_MCP_PEER_GC_INTERVAL_S` (default 30s) and drops peers whose rmcp transport closed. rmcp 1.6 does not surface a peer-disconnect callback.

### Module Structure (`src/mcp/`)

| Module | Description |
|--------|-------------|
| **server.rs** | `McpSshServer` — the rmcp `ServerHandler` + `#[tool_router]`. Wires all 18 tools and the 5 resource handlers. |
| **tools/** | Per-domain tool implementations split out of the v2 `commands.rs` monolith. |
| **tools/connection.rs** | `ssh_connect` (typed `ReusePolicy` enum), `ssh_disconnect`, `ssh_list_sessions`, `ssh_disconnect_agent`. |
| **tools/execute.rs** | `ssh_execute`, `ssh_get_command_output`, `ssh_list_commands` (typed `CommandStatus` enum), `ssh_cancel_command`. |
| **tools/shell.rs** | `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_send_key`, `ssh_shell_read` (long-poll: `wait`/`wait_timeout_secs`/`min_bytes`), `ssh_shell_wait_for`, `ssh_shell_close`. |
| **tools/sftp.rs** | `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress`. |
| **tools/forward.rs** | `ssh_forward` (feature-gated: `port_forward`). |
| **tools/legacy_helpers.rs** | Shared helpers re-exported across the tool modules. |
| **resources.rs** | URI parser (`parse_resource_uri`) + read handlers for the 5 resource schemes. |
| **subscription.rs** | `SUBSCRIPTION_REGISTRY` + per-resource debouncer + per-(peer, uri) cursor + sequence numbers + lagged auto-recovery + peer GC task. |
| **keys.rs** | Semantic keystroke encoder for `ssh_shell_send_key` (control codes, navigation, F1-F12, modifier composition, back-tab). |
| **types.rs** | Internal data carriers (`SessionInfo`, `AsyncCommandInfo`, `ShellInfo`, `TransferInfo`, status enums). All MCP responses are markdown strings built by `message::builder`. |
| **config.rs** | Configuration resolution: Parameter -> Env Var -> Default (with floors and caps). |
| **error.rs** | Error classification for retry logic (retryable vs non-retryable). |
| **client.rs** | SSH connection, authentication, command execution, PTY channels (russh). |
| **sftp.rs** | SFTP session management and streaming file transfer (32 KiB chunks). |
| **transfer.rs** | `RunningTransfer` (lock-free: `OnceCell<String>` error + `broadcast::Sender<ProgressEvent>` + `Notify`), `TransferStatus`, `TransferDirection`. |
| **async_command.rs** | `RunningCommand` (lock-free: `ArcSwap<OutputBuffer>` + `broadcast::Sender<OutputChunk>` + `OnceCell<i32>` exit + `OnceCell<String>` error). |
| **shell.rs** | `RunningShell` (lock-free: `ArcSwap<RingBuffer>` + `broadcast::Sender<Bytes>` + `mpsc::Sender<WriteRequest>` writer-task ownership + `AtomicU64` activity + `Notify`). |
| **session.rs** | `SshClientHandler` for russh callbacks; `SessionRef` carries `broadcast::Sender<HealthEvent>`. |
| **schema.rs** | JSON schema helpers for LLM-friendly schemas. |
| **forward.rs** | Port forwarding (feature-gated). `ForwardHandle` carries `broadcast::Sender<ForwardEvent>`. |

### Storage Layer (`src/mcp/storage/`)

All traits in `traits.rs`, implementations in `DashMap` for lock-free concurrent access:

| Trait | Implementation | Global Instance |
|-------|---------------|-----------------|
| `SessionStorage` | `DashMapSessionStorage` | `SESSION_STORAGE` |
| `CommandStorage` | `DashMapCommandStorage` | `COMMAND_STORAGE` |
| `ShellStorage` | `DashMapShellStorage` | `SHELL_STORAGE` |
| `TransferStorage` | `DashMapTransferStorage` | `TRANSFER_STORAGE` |

Secondary indices for O(1) lookups: agent-to-sessions, session-to-commands, session-to-shells, session-to-transfers.

### Authentication Layer (`src/mcp/auth/`)

Strategy pattern via `AuthStrategy` trait with `AuthChain` for fallback:
- `PasswordAuth`, `KeyAuth` (RSA / Ed25519), `AgentAuth` (SSH agent).

### Message Layer (`src/mcp/message/`)

**helpers.rs** — shared primitives:
- `generate_nonce()` (8-hex-char random from UUIDv4) for delimiter anti-injection.
- `truncate_utf8_safe_tail` / `truncate_utf8_safe_head` for UTF-8 safe cropping.
- `sanitize_value` (escapes `\n`, `\r`, `\t`).
- `format_bytes_human` (human-readable B/KB/MB/GB).
- `format_error(tool, code, reason, detail?)` standardized error format.
- `render_output_block(name, nonce, &[u8], max_bytes, status_hint?)` borrow-based stdout/stderr/data renderer.

**builder.rs** — per-tool markdown builders.

### Response Format (block-only since v3)

All 18 MCP tools return a single markdown `Text<String>` — block-style only:

- First line: `TOOL_NAME: STATUS` (e.g. `SSH_CONNECT: OK`).
- One `KEY: value` per line.
- All IDs suffixed with `_ID` (`SESSION_ID`, `COMMAND_ID`, `SHELL_ID`, `TRANSFER_ID`).
- Output blocks use an 8-hex-char nonce per response: `--- stdout [a3f2b1d7] ---\n<content>\n--- stderr [a3f2b1d7] (empty) ---`.
- Errors: `SSH_X: ERROR\nREASON: [CODE] description\nDETAIL: optional detail`.

The v2 inline form (`TOOL: STATUS | KEY: v | KEY: v`) was dropped in v3.

### MCP Tools (18 total)

- **Connection**: `ssh_connect` (typed `ReusePolicy { Suggest, Auto, ForceNew }`), `ssh_disconnect`, `ssh_list_sessions`, `ssh_disconnect_agent`.
- **Commands**: `ssh_execute` (optional `pty=true`), `ssh_get_command_output`, `ssh_list_commands` (typed `CommandStatus`), `ssh_cancel_command`.
- **Shell** (subscribe-first via `shell://<id>/output`): `ssh_shell_open` (tunable `inactivity_ttl`, `max_buffer_size`), `ssh_shell_write`, **`ssh_shell_send_key`** (semantic keystrokes + modifiers + repeat), `ssh_shell_read` (long-poll: `wait` / `wait_timeout_secs` / `min_bytes`; head-paginated with `clear=true`), **`ssh_shell_wait_for`** (multi-pattern gate), `ssh_shell_close`.
- **SFTP**: `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress`.
- **Network**: `ssh_forward` (feature-gated: `port_forward`).

Each session serializes one russh channel at a time through a per-session semaphore (`CHANNEL_CONCURRENCY_PER_SESSION = 1`) so rapid `execute + cancel` bursts never race OpenSSH's `MaxSessions` budget.

### MCP Resources (5 schemes, subscribe-first)

| Scheme | Description | Cursor |
|--------|-------------|--------|
| `shell://<id>/output` | PTY output stream | yes (`?cursor=auto` or absolute byte offset) |
| `command://<id>/output` | Async command stdout/stderr | yes |
| `transfer://<id>/progress` | SFTP point-in-time progress | no (snapshot) |
| `session://<id>/health` | Session health snapshot | no |
| `forward://<id>/events` | Port-forward event log (feature-gated) | yes |

Subscriptions go through `SUBSCRIPTION_REGISTRY`. The debouncer coalesces events on `SSH_NOTIFY_DEBOUNCE_MS` (default 50ms), force-flushes after `SSH_NOTIFY_FORCE_FLUSH_MS` (default 1000ms), and sends a keepalive every `SSH_NOTIFY_KEEPALIVE_S` (default 30s). Each event carries a sequence number for gap detection; lagged subscribers auto-recover by serving a snapshot from the buffer.

See `docs/RESOURCES.md` for the full resource contract.

### Configuration

All settings follow: **Parameter -> Environment Variable -> Default**. The full table (now 25+ env vars including the 9 added in v3) lives in `docs/CONFIGURATION.md`.

v3 additions:

| Env Variable | Default | Floor | Cap |
|--------------|---------|-------|-----|
| `SSH_COMMAND_BROADCAST_CAP` | 1024 | 16 | 65536 |
| `SSH_SHELL_BROADCAST_CAP` | 1024 | 16 | 65536 |
| `SSH_TRANSFER_BROADCAST_CAP` | 256 | 8 | 4096 |
| `SSH_SESSION_BROADCAST_CAP` | 256 | 8 | 4096 |
| `SSH_FORWARD_BROADCAST_CAP` | 256 | 8 | 4096 (feature-gated) |
| `SSH_NOTIFY_DEBOUNCE_MS` | 50 | 5 | 5000 |
| `SSH_NOTIFY_FORCE_FLUSH_MS` | 1000 | 100 | 60000 |
| `SSH_NOTIFY_KEEPALIVE_S` | 30 | 5 | 300 |
| `SSH_MCP_PEER_GC_INTERVAL_S` | 30 | 5 | 300 |

### Error Handling
- **Retryable**: Connection refused, timeout, network unreachable (exponential backoff via `backon`, max 10s).
- **Non-retryable**: Authentication failures, permission denied.
- All tool returns are `Result<CallToolResult, McpError>` (rmcp). Internal layers still use `Result<T, String>`.

## Code Standards

### Clippy Configuration

Strict clippy enforcement via `Cargo.toml` `[lints.clippy]`:

- **Lint groups**: `clippy::all`, `clippy::pedantic`, `clippy::nursery`, `clippy::cargo` at `deny`.
- **Layer A (forbid)**: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `exit`, `mem_forget`, `infinite_loop`, `print_stdout`, `print_stderr`.
- **v3 lock-free invariants** (deny): `await_holding_lock`, `await_holding_refcell_ref`, `significant_drop_in_scrutinee`, `significant_drop_tightening`, `mutex_atomic`, `mutex_integer`. Hot-path state types (`RunningCommand`, `RunningShell`, `RunningTransfer`, `SessionRef`, `ForwardHandle`) carry **zero** `Mutex` fields.
- **Quality denies**: `wildcard_enum_match_arm`, `as_conversions`, `clone_on_ref_ptr`, `implicit_clone`, `ref_patterns`, `absolute_paths`, `pub_use`, `allow_attributes_without_reason`, `format_push_string`, `if_then_some_else_none`, `rc_mutex`, `redundant_type_annotations`, `same_name_method`, `tests_outside_test_module`, etc.
- **Thresholds** (`clippy.toml`): `cognitive-complexity-threshold = 25`, `too-many-lines-threshold = 30`, `too-many-arguments-threshold = 7`, `type-complexity-threshold = 250`.
- **Allowed**: `multiple_crate_versions` (transitive deps from russh / axum).

All `#[allow(...)]` attributes **must** include a `reason = "..."`. Never disable a lint to silence a warning — fix the code instead.

See `docs/LOCKS.md` for the lock-free invariants enforced by these lints.

### General

- Methods < 30 lines, SOLID principles.
- Lock-free everywhere on the hot path: `DashMap`, `ArcSwap`, `OnceCell`, `Atomic*`, `tokio::sync::broadcast`, `tokio::sync::Notify`, `mpsc` for owned-resource serialization.
- 820 lib tests + 12 integration tests + Python integration suites (`scripts/test_*.py`) + 4 stress scripts (`scripts/stress_*.py`).
- Feature flag: `port_forward` (default: enabled).
- 8 loom invariant tests in `tests/lockfree_invariants.rs` (gated `#[cfg(loom)]`; full loom mode currently blocked by upstream tokio/loom incompatibility in russh + axum — documented in the test file and `Cargo.toml`).

## v3 Migration Notes

- `poem` and `poem-mcpserver` removed; HTTP transport is now `axum` + `rmcp::transport::streamable_http_server`.
- `commands.rs` (the v2 monolithic 2272-LOC tools file) split into `src/mcp/tools/{connection,execute,shell,sftp,forward,legacy_helpers}.rs`.
- Inline response form removed — all responses are block-style markdown.
- `ssh_connect.reuse` and `ssh_list_commands.status` are now typed enums; the wire format is unchanged for valid values, but typos now produce a JSON-schema validation error instead of falling through to the default branch.
- Two new tools (`ssh_shell_send_key`, `ssh_shell_wait_for`); shell consumers should prefer subscribing to `shell://<id>/output` over polling `ssh_shell_read`.

See `docs/MIGRATION_v2_to_v3.md` for the full client-facing migration guide.
