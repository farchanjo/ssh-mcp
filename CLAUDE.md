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
| **mod.rs** | 29 | Module declarations and re-exports |
| **types.rs** | 1112 | Response types (`SessionInfo`, `SshConnectResponse`, async types) |
| **config.rs** | 601 | Duration constants and configuration resolution |
| **error.rs** | 359 | Error classification for retry logic |
| **session.rs** | 41 | `SshClientHandler` for russh client |
| **client.rs** | 785 | SSH connection, authentication, command execution |
| **async_command.rs** | 183 | Async command types (`RunningCommand`, `OutputBuffer`) |
| **forward.rs** | 155 | Port forwarding (feature-gated) |
| **commands.rs** | 787 | `McpSSHCommands` MCP tool implementations |

### SOLID Architecture Modules

#### Storage Layer (`src/mcp/storage/`)
| Module | Lines | Description |
|--------|-------|-------------|
| **mod.rs** | 18 | Module exports and global storage instances |
| **traits.rs** | 100 | `SessionStorage` and `CommandStorage` trait definitions |
| **session.rs** | 491 | `DashMapSessionStorage` with agent index and tests |
| **command.rs** | 812 | `DashMapCommandStorage` with session index and tests |

Storage abstractions enable dependency injection and testability:
- `SessionStorage`: CRUD for SSH sessions with agent grouping via secondary index
- `CommandStorage`: CRUD for async commands with O(1) session lookups
- Both use `DashMap` for lock-free concurrent access

**Key types:**
- `SessionRef`: Read-only reference containing `SessionInfo` and `Handle`
- `CommandRef`: Read-only reference containing `AsyncCommandInfo` and `RunningCommand`
- `StoredSession`: Internal storage struct combining metadata with session handle

**Usage example:**
```rust
use ssh_mcp::mcp::storage::{SESSION_STORAGE, COMMAND_STORAGE};

// Insert a session
SESSION_STORAGE.insert(session_id, info, handle);

// Register with agent for bulk operations
SESSION_STORAGE.register_agent(&agent_id, &session_id);

// Get all sessions for an agent
let sessions = SESSION_STORAGE.get_agent_sessions(&agent_id);
```

#### Authentication Layer (`src/mcp/auth/`)
| Module | Lines | Description |
|--------|-------|-------------|
| **mod.rs** | 36 | Module exports and usage examples |
| **traits.rs** | 40 | `AuthStrategy` trait definition |
| **password.rs** | 129 | `PasswordAuth` strategy with tests |
| **key.rs** | 205 | `KeyAuth` strategy (RSA, Ed25519) with tests |
| **agent.rs** | 139 | `AgentAuth` strategy (SSH agent) with tests |
| **chain.rs** | 323 | `AuthChain` for trying multiple strategies with tests |

Authentication uses the Strategy pattern (Open-Closed Principle):

**`AuthStrategy` trait:**
```rust
#[async_trait]
pub trait AuthStrategy: Send + Sync {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String>;

    fn name(&self) -> &'static str;
}
```

**Available strategies:**
- `PasswordAuth`: Username/password authentication
- `KeyAuth`: Private key file authentication (supports RSA with `rsa-sha2-256`/`rsa-sha2-512`, Ed25519, ECDSA)
- `AgentAuth`: SSH agent authentication via `SSH_AUTH_SOCK`
- `AuthChain`: Composite strategy that tries multiple methods in order

**Usage example:**
```rust
use ssh_mcp::mcp::auth::{AuthChain, PasswordAuth, KeyAuth, AgentAuth};

// Chain multiple strategies (tried in order)
let chain = AuthChain::new()
    .with_password("secret")
    .with_key("/path/to/key")
    .with_agent();

let result = chain.authenticate(&mut handle, "username").await?;
```

**Adding new strategies (Open-Closed Principle):**
```rust
pub struct MyCustomAuth { /* ... */ }

#[async_trait]
impl AuthStrategy for MyCustomAuth {
    async fn authenticate(&self, handle: &mut Handle, username: &str) -> Result<bool, String> {
        // Custom authentication logic
    }
    fn name(&self) -> &'static str { "custom" }
}
```

#### Message Layer (`src/mcp/message/`)
| Module | Lines | Description |
|--------|-------|-------------|
| **mod.rs** | 9 | Module exports |
| **builder.rs** | 424 | Fluent message builders with comprehensive tests |

Message builders construct human-readable responses that help LLMs remember important identifiers:

**Available builders:**
- `ConnectMessageBuilder`: Connection success with session identifiers
- `ExecuteMessageBuilder`: Command start with polling instructions
- `AgentDisconnectMessageBuilder`: Agent cleanup summary

**Usage examples:**

```rust
use ssh_mcp::mcp::message::{ConnectMessageBuilder, ExecuteMessageBuilder, AgentDisconnectMessageBuilder};

// Connection message
let message = ConnectMessageBuilder::new("session-123", "user", "host:22")
    .with_agent_id(Some("my-agent"))
    .with_name(Some("production-db"))
    .with_retry_attempts(2)
    .with_persistent(true)
    .reused(false)
    .build();

// Command execution message
let message = ExecuteMessageBuilder::new("cmd-123", "session-456", "ls -la")
    .with_agent_id(Some("my-agent"))
    .build();

// Agent disconnect message
let message = AgentDisconnectMessageBuilder::new("my-agent")
    .with_sessions_disconnected(3)
    .with_commands_cancelled(5)
    .build();
```

**Example output (ConnectMessageBuilder):**
```
SSH CONNECTION ESTABLISHED. REMEMBER THESE IDENTIFIERS:
- agent_id: 'my-agent'
- session_id: 'session-123'
- name: 'production-db'
- host: user@host:22
- retry_attempts: 2
- persistent: true

Use ssh_execute with session_id 'session-123' to run commands.
Use ssh_disconnect_agent with agent_id 'my-agent' to disconnect all sessions for this agent.
```

### MCP Tools
- `ssh_connect`: Connection with retry logic (exponential backoff via `backon` crate)
  - `name: Option<String>` - Human-readable session name for LLM identification
  - `persistent: Option<bool>` - When true, disables inactivity timeout (keepalive still active)
  - `agent_id: Option<String>` - Agent identifier for grouping sessions (use with `ssh_disconnect_agent`)
- `ssh_execute`: Execute command, returns `command_id` for polling (includes `agent_id` in response)
- `ssh_get_command_output`: Poll for async command output/status (supports `wait` for blocking)
- `ssh_list_commands`: List all async commands (filterable by session/status)
- `ssh_cancel_command`: Cancel a running async command
- `ssh_forward`: Port forwarding (feature-gated)
- `ssh_disconnect`: Session cleanup (also cancels all async commands for the session)
- `ssh_list_sessions`: List active sessions (filterable by `agent_id`)
- `ssh_disconnect_agent`: Disconnect ALL sessions for a specific agent (bulk cleanup)

### Key Types
- **`SessionInfo`**: Session metadata with optional `name` and `agent_id` fields (omitted from JSON when None)
- **`SshCommandResponse`**: Contains `stdout`, `stderr`, `exit_code`, and `timed_out: bool`
  - On timeout: returns partial output collected so far with `timed_out: true` (session stays alive)
  - On success: returns full output with `timed_out: false`
- **`SshConnectResponse`**: Contains `session_id`, `agent_id` (if provided), descriptive `message`
- **`SshExecuteResponse`**: Response from `ssh_execute` with `command_id`, `session_id`, `agent_id`, descriptive `message`
- **`AsyncCommandInfo`**: Metadata for async commands including `command_id`, `session_id`, `command`, `status`, `started_at`
- **`AsyncCommandStatus`**: Enum with `Running`, `Completed`, `Cancelled`, `Failed`
- **`SshAsyncOutputResponse`**: Output from async command including `status`, `stdout`, `stderr`, `exit_code`
- **`AgentDisconnectResponse`**: Response from `ssh_disconnect_agent` with `agent_id`, `sessions_disconnected`, `commands_cancelled`

### Async Command Execution

Use async execution for long-running commands (builds, deployments, data processing) that may take longer than the default timeout or when you want to run multiple commands concurrently.

#### When to Use Async vs Sync

| Use `ssh_execute` (sync) | Use `ssh_execute` |
|--------------------------|-------------------------|
| Quick commands (< 30s) | Long-running commands (builds, deployments) |
| Need immediate result | Want to run multiple commands in parallel |
| Simple one-off commands | Need to monitor progress or cancel mid-execution |

#### Async Workflow

```
1. ssh_connect(address, username) → session_id
2. ssh_execute(session_id, command) → command_id
3. ssh_get_command_output(command_id, wait=false) → status: "running"
4. ssh_get_command_output(command_id, wait=true) → status: "completed", stdout, exit_code
5. ssh_disconnect(session_id) → cleans up all async commands
```

#### Tool Reference

**`ssh_execute`** - Start a background command
```json
{
  "session_id": "uuid-from-connect",
  "command": "npm run build",
  "timeout_secs": 300  // optional, default 180s
}
```
Returns: `{ "command_id": "uuid", "message": "Command started..." }`

**`ssh_get_command_output`** - Get output and status
```json
{
  "command_id": "uuid-from-async",
  "wait": false,           // false = poll immediately, true = block until done
  "wait_timeout_secs": 60  // max wait time when wait=true (default 30, max 300)
}
```
Returns: `{ "status": "running|completed|cancelled|failed", "stdout": "...", "stderr": "...", "exit_code": 0 }`

**`ssh_list_commands`** - List async commands
```json
{
  "session_id": "uuid",  // optional: filter by session
  "status": "running"    // optional: filter by status
}
```
Returns: `{ "commands": [...], "count": 2 }`

**`ssh_cancel_command`** - Stop a running command
```json
{
  "command_id": "uuid-to-cancel"
}
```
Returns: `{ "cancelled": true, "stdout": "partial output...", "stderr": "" }`

#### Status Values

| Status | Description | Available Fields |
|--------|-------------|------------------|
| `running` | Command still executing | `stdout`, `stderr` (partial) |
| `completed` | Finished successfully | `stdout`, `stderr`, `exit_code` |
| `cancelled` | Stopped by user | `stdout`, `stderr` (partial) |
| `failed` | Failed to start | `error` message |

#### Example: Build and Deploy

```
# Start build in background
ssh_execute(session_id, "cd /app && npm run build") → build_cmd_id

# Start tests in parallel
ssh_execute(session_id, "cd /app && npm test") → test_cmd_id

# Wait for both to complete
ssh_get_command_output(build_cmd_id, wait=true, wait_timeout_secs=120)
ssh_get_command_output(test_cmd_id, wait=true, wait_timeout_secs=60)

# Check results and deploy if successful
```

#### Example: Monitor Long Process

```
# Start long-running process
ssh_execute(session_id, "python train_model.py") → cmd_id

# Poll periodically to show progress
while True:
    result = ssh_get_command_output(cmd_id, wait=false)
    print(result.stdout)  # Show latest output
    if result.status != "running":
        break
    sleep(5)
```

#### Example: Timeout and Cancel

```
# Start potentially slow command
ssh_execute(session_id, "find / -name '*.log'") → cmd_id

# Wait with timeout
result = ssh_get_command_output(cmd_id, wait=true, wait_timeout_secs=10)

if result.status == "running":
    # Still running after timeout, cancel it
    ssh_cancel_command(cmd_id)
```

#### Limits
- Max 100 concurrent multiplexed commands per session
- Commands auto-cleanup when session disconnects
- Default timeout: 180s (configurable via `timeout_secs` or `SSH_COMMAND_TIMEOUT` env)

### Agent ID Support

When multiple LLM agents share an SSH MCP server, use `agent_id` to isolate sessions:

#### Connect with agent_id
```json
{
  "tool": "ssh_connect",
  "params": {
    "address": "server:22",
    "username": "user",
    "agent_id": "my-unique-agent-id"
  }
}
```

Response includes descriptive message with all identifiers:
```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "agent_id": "my-unique-agent-id",
  "message": "SSH CONNECTION ESTABLISHED. REMEMBER THESE IDENTIFIERS:\n• agent_id: 'my-unique-agent-id'\n• session_id: '550e8400-...'\n• host: user@server:22\n\nUse ssh_execute with session_id '550e8400-...' to run commands.\nUse ssh_disconnect_agent with agent_id 'my-unique-agent-id' to disconnect all sessions for this agent."
}
```

#### List only your sessions
```json
{
  "tool": "ssh_list_sessions",
  "params": {
    "agent_id": "my-unique-agent-id"
  }
}
```

#### Cleanup all your sessions
```json
{
  "tool": "ssh_disconnect_agent",
  "params": {
    "agent_id": "my-unique-agent-id"
  }
}
```

Response:
```json
{
  "agent_id": "my-unique-agent-id",
  "sessions_disconnected": 3,
  "commands_cancelled": 5,
  "message": "AGENT CLEANUP COMPLETE. SUMMARY:\n• agent_id: 'my-unique-agent-id'\n• sessions_disconnected: 3\n• commands_cancelled: 5\n\nAll sessions and commands for agent 'my-unique-agent-id' have been terminated."
}
```

**Best practice:** Always use `agent_id` when multiple agents might share the MCP server. Use a unique identifier like your project folder path or a UUID.

### Threading Model
- Tokio async runtime with native async SSH via `russh` crate
- Lock-free storage via `DashMap` implementations of `SessionStorage` and `CommandStorage` traits
- Secondary indices for O(1) lookups: agent-to-sessions, session-to-commands

### Authentication
- Strategy pattern via `AuthStrategy` trait with `AuthChain` for fallback
- RSA keys use `best_supported_rsa_hash()` to negotiate `rsa-sha2-256`/`rsa-sha2-512`
- Strategies: `PasswordAuth`, `KeyAuth` (RSA, Ed25519), `AgentAuth` (SSH agent)

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
- Lock-free data structures (`DashMap`) for concurrent access
- 308 unit tests (`cargo test --all-features`)

## Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `port_forward` | enabled | SSH port forwarding support |
