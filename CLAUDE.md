# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Build release binaries
cargo build --release

# Build specific binary
cargo build --release --bin ssh-mcp        # HTTP server
cargo build --release --bin ssh-mcp-stdio  # Stdio transport for MCP

# Build without port forwarding feature
cargo build --release --no-default-features

# Run tests
cargo test --all-features

# Linting and formatting
cargo fmt --all -- --check
cargo clippy -- -D warnings

# Install binary (for MCP integration)
sudo cp ./target/release/ssh-mcp-stdio /usr/local/bin/
sudo codesign -f -s - /usr/local/bin/ssh-mcp-stdio  # Required on macOS
```

## Architecture

### Binary Targets
- **ssh-mcp** (`src/main.rs`): HTTP server using Poem framework on port 8000
- **ssh-mcp-stdio** (`src/bin/ssh_mcp_stdio.rs`): Stdio-based MCP transport with tracing (logs to stderr via `RUST_LOG` env filter)

### Core Modules (`src/mcp/`)
| Module | Lines | Description |
|--------|-------|-------------|
| **mod.rs** | 22 | Module declarations and re-exports |
| **types.rs** | 317 | Response types (`SessionInfo`, `SshConnectResponse`, `SshCommandResponse`) |
| **config.rs** | 601 | Duration constants and configuration resolution |
| **error.rs** | 359 | Error classification for retry logic |
| **session.rs** | 87 | Session storage and `SshClientHandler` |
| **client.rs** | 683 | SSH connection, authentication, command execution |
| **forward.rs** | 162 | Port forwarding (feature-gated) |
| **commands.rs** | 263 | `McpSSHCommands` MCP tool implementations |

### MCP Tools
- `ssh_connect`: Connection with retry logic (exponential backoff via `backon` crate)
  - `name: Option<String>` - Human-readable session name for LLM identification
  - `persistent: Option<bool>` - When true, disables inactivity timeout (keepalive still active)
- `ssh_execute`: Command execution with timeout (returns partial output with `timed_out: true` on timeout)
- `ssh_forward`: Port forwarding (feature-gated)
- `ssh_disconnect`: Session cleanup
- `ssh_list_sessions`: List active sessions (includes `name` when set)

### Key Types
- **`SessionInfo`**: Session metadata with optional `name` field (omitted from JSON when None)
- **`SshCommandResponse`**: Contains `stdout`, `stderr`, `exit_code`, and `timed_out: bool`
  - On timeout: returns partial output collected so far with `timed_out: true` (session stays alive)
  - On success: returns full output with `timed_out: false`
- **`SshConnectResponse`**: Message includes "[persistent session]" suffix when `persistent=true`

### Threading Model
- Tokio async runtime with native async SSH via `russh` crate
- Global session store: `Lazy<Mutex<HashMap<String, StoredSession>>>`

### Authentication
- RSA keys use `best_supported_rsa_hash()` to negotiate `rsa-sha2-256`/`rsa-sha2-512` instead of legacy `ssh-rsa`
- Supports password, public key (RSA, Ed25519), and SSH agent authentication

### Configuration Priority
All settings follow: **Parameter → Environment Variable → Default**

| Env Variable | Default | Description |
|--------------|---------|-------------|
| `SSH_CONNECT_TIMEOUT` | 30s | Connection timeout (`DEFAULT_CONNECT_TIMEOUT: Duration`) |
| `SSH_COMMAND_TIMEOUT` | 180s | Command execution timeout (`DEFAULT_COMMAND_TIMEOUT: Duration`) |
| `SSH_MAX_RETRIES` | 3 | Retry attempts |
| `SSH_RETRY_DELAY_MS` | 1000ms | Initial retry delay (`DEFAULT_RETRY_DELAY: Duration`) |
| `SSH_COMPRESSION` | true | Enable zlib compression |
| `MCP_PORT` | 8000 | HTTP server port |

### Error Handling Strategy
- **Retryable errors**: Connection refused, timeout, network unreachable
- **Non-retryable errors**: Authentication failures, permission denied
- Exponential backoff with jitter (min: 1s, max: `MAX_RETRY_DELAY: Duration` = 10s)

## Code Standards

- `#![deny(warnings)]` - All warnings are errors
- `#![deny(clippy::unwrap_used)]` - No unwrap, use proper error handling
- Methods should be < 30 lines
- Minimize lock scope for `SSH_SESSIONS` mutex
- 93 unit tests (`cargo test --all-features`)

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `port_forward` | enabled | SSH port forwarding support |
