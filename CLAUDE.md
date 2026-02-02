# CLAUDE.md

## Build Commands

```bash
cargo build --release                              # Build all binaries
cargo build --release --bin ssh-mcp                # HTTP server only
cargo build --release --bin ssh-mcp-stdio           # Stdio transport only
cargo build --release --no-default-features         # Without port forwarding
cargo test --all-features                           # Run tests (435 tests)
cargo fmt --all -- --check                          # Check formatting
cargo clippy -- -D warnings                         # Lint
```

## Architecture

### Binary Targets
- **ssh-mcp** (`src/main.rs`): HTTP server via Poem on port 8000
- **ssh-mcp-stdio** (`src/bin/ssh_mcp_stdio.rs`): Stdio MCP transport (logs to stderr via `RUST_LOG`)

### Module Structure (`src/mcp/`)

| Module | Description |
|--------|-------------|
| **types.rs** | All response types (`SessionInfo`, `SshConnectResponse`, `ShellInfo`, `TransferInfo`, etc.) |
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

Fluent builders in `builder.rs`: `ConnectMessageBuilder`, `ExecuteMessageBuilder`, `AgentDisconnectMessageBuilder`, `ShellOpenMessageBuilder`, `UploadMessageBuilder`, `DownloadMessageBuilder`, `TransferProgressMessageBuilder`

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
| `MCP_PORT` | 8000 | HTTP server port |
| `RUST_LOG` | info | Log level filter |

### Error Handling
- **Retryable**: Connection refused, timeout, network unreachable (exponential backoff, max 10s)
- **Non-retryable**: Authentication failures, permission denied
- All errors use `Result<T, String>`

## Code Standards

- `#![deny(warnings)]` and `#![deny(clippy::unwrap_used)]`
- Methods < 30 lines, SOLID principles
- Lock-free data structures (`DashMap`) for concurrent access
- 435 unit tests (`cargo test --all-features`)
- Feature flag: `port_forward` (default: enabled)
