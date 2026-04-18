# CLAUDE.md

## Build Commands

```bash
cargo build --release                              # Build all binaries
cargo build --release --bin ssh-mcp                # HTTP server only
cargo build --release --bin ssh-mcp-stdio           # Stdio transport only
cargo build --release --no-default-features         # Without port forwarding
cargo test --all-features                           # Run tests (480+ tests)
cargo fmt --all -- --check                          # Check formatting
cargo clippy -- -D warnings                         # Lint
cargo dupes                                          # Code duplication detection (cargo-dupes)
```

## Architecture

### Binary Targets
- **ssh-mcp** (`src/main.rs`): HTTP server via Poem on port 8000
- **ssh-mcp-stdio** (`src/bin/ssh_mcp_stdio.rs`): Stdio MCP transport (logs to stderr via `RUST_LOG`)

### Module Structure (`src/mcp/`)

| Module | Description |
|--------|-------------|
| **types.rs** | Internal data carriers (`SessionInfo`, `AsyncCommandInfo`, `ShellInfo`, `TransferInfo`, status enums). Response types were removed in v2.0 — tools now return markdown strings built by `message::builder`. |
| **config.rs** | Configuration resolution: Parameter -> Env Var -> Default |
| **error.rs** | Error classification for retry logic (retryable vs non-retryable) |
| **client.rs** | SSH connection, authentication, command execution, PTY channels |
| **commands.rs** | `McpSSHCommands` - all 16 MCP tool implementations |
| **sftp.rs** | SFTP session management and streaming file transfer helpers |
| **transfer.rs** | Transfer tracking types (`RunningTransfer`, `TransferStatus`, `TransferDirection`) |
| **async_command.rs** | Async command types (`RunningCommand`, `OutputBuffer`) |
| **shell.rs** | Interactive PTY shell types (`RunningShell`, `ChannelWriter`) |
| **session.rs** | `SshClientHandler` for russh client callbacks |
| **schema.rs** | JSON schema helpers for LLM-friendly schemas |
| **forward.rs** | Port forwarding (feature-gated: `port_forward`) |

### Storage Layer (`src/mcp/storage/`)

All traits defined in `traits.rs`, implementations use `DashMap` for lock-free concurrent access:

| Trait | Implementation | Global Instance |
|-------|---------------|-----------------|
| `SessionStorage` | `DashMapSessionStorage` | `SESSION_STORAGE` |
| `CommandStorage` | `DashMapCommandStorage` | `COMMAND_STORAGE` |
| `ShellStorage` | `DashMapShellStorage` | `SHELL_STORAGE` |
| `TransferStorage` | `DashMapTransferStorage` | `TRANSFER_STORAGE` |

Secondary indices for O(1) lookups: agent-to-sessions, session-to-commands, session-to-shells, session-to-transfers.

### Authentication Layer (`src/mcp/auth/`)

Strategy pattern via `AuthStrategy` trait with `AuthChain` for fallback:
- `PasswordAuth`, `KeyAuth` (RSA/Ed25519), `AgentAuth` (SSH agent)

### Message Layer (`src/mcp/message/`)

**helpers.rs** — shared primitives:
- `generate_nonce()` (8-hex-char random from UUIDv4) for delimiter anti-injection
- `truncate_utf8_safe_tail` / `truncate_utf8_safe_head` for UTF-8 safe cropping
- `sanitize_value` (escapes `\n`, `\r`, `\t`)
- `format_bytes_human` (human-readable B/KB/MB/GB)
- `format_error(tool, code, reason, detail?)` standardized error format
- `render_output_block(name, nonce, &[u8], max_bytes, status_hint?)` borrow-based stdout/stderr/data renderer

**builder.rs** — per-tool markdown builders (all return full response string):
- `ConnectOkBuilder` (OK / REUSED), `ConnectSuggestedBuilder` (single- and multi-match) + `SessionMatch`
- `ExecuteStartedBuilder`
- `GetCommandOutputBuilder` (`Running` / `Completed(i32)` / `Timeout`)
- `CancelCommandCancelledBuilder` + `render_cancel_command_noop`
- `ShellOpenBuilder`, `ShellReadBuilder` (`Open` / `Closed`)
- `TransferStartedBuilder` (upload/download), `TransferProgressBuilder` (`Running` / `Completed` / `Failed`)
- `ListSessionsBuilder`, `ListCommandsBuilder`
- `render_disconnect_ok`, `render_disconnect_agent`, `render_shell_write_ok`, `render_shell_close_ok`, `render_forward_ok`

### Response Format (v2.0)

All 16 MCP tools return a single markdown `Text<String>` — no structured JSON. Format:

- First line: `TOOL_NAME: STATUS` (e.g. `SSH_CONNECT: OK`)
- Block style: one `KEY: value` per line when 4+ fields or an output block is embedded
- Inline style: `TOOL: STATUS | KEY: v | KEY: v` for ≤3 simple fields
- All IDs suffixed with `_ID` (`SESSION_ID`, `COMMAND_ID`, `SHELL_ID`, `TRANSFER_ID`)
- Output blocks use an 8-hex-char nonce per response: `--- stdout [a3f2b1d7] ---\n<content>\n--- stderr [a3f2b1d7] (empty) ---`
- Errors: `SSH_X: ERROR\nREASON: [CODE] description\nDETAIL: optional detail`

Output-returning tools (`ssh_get_command_output`, `ssh_shell_read`, `ssh_cancel_command`) accept an optional `max_output_bytes` parameter (default 16 KiB, cap 1 MiB). `ssh_list_sessions` / `ssh_list_commands` accept `max_items` (default 500, cap 10 000). `ssh_connect` accepts `reuse: "suggest" | "auto" | "force_new"` (default `suggest`) for smart reuse detection via the identity triple (`host`, `port`, `username`).

### MCP Tools (16 total)
- **Connection**: `ssh_connect`, `ssh_disconnect`, `ssh_list_sessions`, `ssh_disconnect_agent`
- **Commands**: `ssh_execute`, `ssh_get_command_output`, `ssh_list_commands`, `ssh_cancel_command`
- **Shell**: `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_read`, `ssh_shell_close`
- **SFTP**: `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress`
- **Network**: `ssh_forward` (feature-gated)

### Configuration

All settings follow: **Parameter -> Environment Variable -> Default**

| Env Variable | Default | Description |
|---|---|---|
| `SSH_CONNECT_TIMEOUT` | 30s | Connection timeout |
| `SSH_COMMAND_TIMEOUT` | 180s | Command execution timeout |
| `SSH_MAX_RETRIES` | 3 | Retry attempts |
| `SSH_RETRY_DELAY_MS` | 1000ms | Initial retry delay |
| `SSH_INACTIVITY_TIMEOUT` | 300s | Session inactivity timeout |
| `SSH_COMPRESSION` | true | Enable zlib compression |
| `SSH_COMMAND_CLEANUP_TTL` | 60s | TTL before unread command output is cleaned up |
| `SSH_SHELL_INACTIVITY_TTL` | 600s | Shell auto-close after inactivity (no read/write) |
| `SSH_SHELL_MAX_BUFFER_SIZE` | 10m | Max shell output buffer size (supports b/k/m/g/t suffixes) |
| `SSH_COMMAND_MAX_BUFFER_SIZE` | 10m | Max per-command stdout/stderr buffer (head-drained when exceeded) |
| `SSH_TRANSFER_CLEANUP_TTL` | 300s | TTL before terminated (completed/failed/cancelled) transfers are removed from storage |
| `SSH_MCP_OUTPUT_DEFAULT_BYTES` | 16384 | Default `max_output_bytes` for output-returning tools |
| `SSH_MCP_OUTPUT_MAX_BYTES_CAP` | 1048576 | Hard cap on `max_output_bytes` |
| `SSH_MCP_LIST_MAX_ITEMS` | 500 | Default `max_items` for list tools |
| `SSH_MCP_LIST_MAX_ITEMS_CAP` | 10000 | Hard cap on `max_items` |
| `MCP_HOST` | 0.0.0.0 | HTTP server bind address |
| `MCP_PORT` | 8000 | HTTP server port |
| `RUST_LOG` | info | Log level filter |

### Error Handling
- **Retryable**: Connection refused, timeout, network unreachable (exponential backoff, max 10s)
- **Non-retryable**: Authentication failures, permission denied
- All errors use `Result<T, String>`

## Code Standards

### Clippy Configuration

Strict clippy enforcement via `src/lib.rs` deny attributes:

- **Lint groups**: `clippy::all`, `clippy::pedantic`, `clippy::nursery`, `clippy::cargo`
- **Safety denials**: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `exit`, `mem_forget`, `infinite_loop`
- **Output denials**: `print_stdout`, `print_stderr`
- **Code quality**: `wildcard_enum_match_arm`, `as_conversions`, `clone_on_ref_ptr`, `implicit_clone`, `ref_patterns`, `absolute_paths`, `pub_use`, `allow_attributes_without_reason`
- **Thresholds** (`clippy.toml`): `cognitive-complexity-threshold = 25`, `too-many-lines-threshold = 30`, `too-many-arguments-threshold = 7`, `type-complexity-threshold = 250`
- **Allowed**: `multiple_crate_versions` (transitive deps from russh/poem)

All `#[allow(...)]` attributes **must** include a `reason = "..."`. Never disable a lint rule to silence a warning — fix the code instead.

### General

- Methods < 30 lines, SOLID principles
- Lock-free data structures (`DashMap`) for concurrent access
- 480+ unit tests (`cargo test --all-features`)
- Feature flag: `port_forward` (default: enabled)

## v2.0.0 Migration Notes

Breaking change: all MCP tool responses are now plain markdown strings
(`Text<String>`) instead of structured JSON. Clients that parsed the
old response fields directly must update. Legacy JSON responses can no
longer be produced — the old `SshConnectResponse`, `SshExecuteResponse`,
etc. structs have been removed from `src/mcp/types.rs`.

Integration clients wanting the old keys can use the
`parse_mcp_response` helper added to `scripts/test_http.py` /
`scripts/test_stdio.py` as a reference for reconstructing them.

New optional parameters (safe to omit):
- `ssh_connect.reuse` (`suggest` | `auto` | `force_new`)
- `ssh_get_command_output.max_output_bytes` (usize)
- `ssh_shell_read.max_output_bytes` (usize)
- `ssh_cancel_command.max_output_bytes` (usize)
- `ssh_list_sessions.max_items` (usize)
- `ssh_list_commands.max_items` (usize)
