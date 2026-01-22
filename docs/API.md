# SSH MCP API Reference

This document provides a complete API reference for all MCP tools exposed by the SSH MCP server.

## Table of Contents

- [Overview](#overview)
- [Tools](#tools)
  - [ssh_connect](#ssh_connect)
  - [ssh_execute](#ssh_execute)
  - [ssh_execute_async](#ssh_execute_async)
  - [ssh_get_command_output](#ssh_get_command_output)
  - [ssh_list_commands](#ssh_list_commands)
  - [ssh_cancel_command](#ssh_cancel_command)
  - [ssh_forward](#ssh_forward)
  - [ssh_disconnect](#ssh_disconnect)
  - [ssh_list_sessions](#ssh_list_sessions)
- [Response Types](#response-types)
- [Error Responses](#error-responses)
- [Examples](#examples)
- [Important Notes](#important-notes)
  - [Authentication](#authentication)
  - [Retry Logic](#retry-logic)
  - [Configuration Priority](#configuration-priority)
  - [Async vs Sync Command Execution](#async-vs-sync-command-execution)

---

## Overview

SSH MCP exposes 9 tools for managing SSH connections and operations:

| Tool | Description | Feature Flag |
|------|-------------|--------------|
| `ssh_connect` | Establish SSH connection | - |
| `ssh_execute` | Execute remote command (synchronous) | - |
| `ssh_execute_async` | Start command in background | - |
| `ssh_get_command_output` | Poll or wait for async command output | - |
| `ssh_list_commands` | List all async commands | - |
| `ssh_cancel_command` | Cancel a running async command | - |
| `ssh_forward` | Setup port forwarding | `port_forward` |
| `ssh_disconnect` | Close SSH session | - |
| `ssh_list_sessions` | List active sessions | - |

---

## Tools

### ssh_connect

Establishes an SSH connection to a remote server with automatic retry logic.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `address` | `string` | Yes | - | SSH server address in format `host:port` (e.g., `192.168.1.1:22`). Port defaults to 22 if omitted. |
| `username` | `string` | Yes | - | SSH username for authentication |
| `password` | `string` | No | `null` | Password for password-based authentication |
| `key_path` | `string` | No | `null` | Absolute path to private key file for key-based authentication |
| `name` | `string` | No | `null` | Human-readable name for the session (e.g., "production-db", "staging-server"). Helps LLMs identify sessions more easily. |
| `persistent` | `bool` | No | `false` | When `true`, disables inactivity timeout, keeping the session open indefinitely until explicitly disconnected via `ssh_disconnect` or the process dies. Keepalive still works (30s interval, 3 max attempts). |
| `timeout_secs` | `u64` | No | `30` | Connection timeout in seconds. Falls back to `SSH_CONNECT_TIMEOUT` env var. |
| `max_retries` | `u32` | No | `3` | Maximum retry attempts for transient failures. Falls back to `SSH_MAX_RETRIES` env var. |
| `retry_delay_ms` | `u64` | No | `1000` | Initial delay between retries in milliseconds. Uses exponential backoff (capped at 10s). Falls back to `SSH_RETRY_DELAY_MS` env var. |
| `compress` | `bool` | No | `true` | Enable zlib compression. Falls back to `SSH_COMPRESSION` env var. |

#### Authentication Priority

Authentication methods are attempted in this order:

1. **Password** - If `password` is provided, password authentication is used
2. **Key File** - If `key_path` is provided (and no password), public key authentication is used
3. **SSH Agent** - If neither password nor key_path is provided, SSH agent authentication is attempted (tries all available identities)

> **Note on RSA Keys**: For RSA keys, the server's preferred hash algorithm is automatically negotiated (`rsa-sha2-256` or `rsa-sha2-512`). The legacy `ssh-rsa` (SHA1) signature algorithm is avoided for security reasons.

#### Retry Behavior

Retry logic with exponential backoff only applies to **transient connection errors**. Authentication failures are **never retried** to avoid account lockouts.

**Retryable errors** (will be retried up to `max_retries` times):
- Connection refused
- Connection timeout
- Network is unreachable
- No route to host
- Host is down
- Temporary DNS failures
- Broken pipe

**Non-retryable errors** (fail immediately):
- Authentication failed
- Permission denied
- Invalid credentials
- Key authentication failed
- No identities in SSH agent

#### Response

Returns `SshConnectResponse`:

```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "message": "Successfully connected to user@192.168.1.1:22",
  "authenticated": true,
  "retry_attempts": 0
}
```

With persistent session:

```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "message": "Successfully connected to user@192.168.1.1:22 [persistent session]",
  "authenticated": true,
  "retry_attempts": 0
}
```

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | `string` | Unique UUID v4 identifier for the session |
| `message` | `string` | Human-readable success message. Includes "[persistent session]" suffix when `persistent=true`. |
| `authenticated` | `bool` | Always `true` on success |
| `retry_attempts` | `u32` | Number of retry attempts needed |

#### Example Usage

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "192.168.1.100:22",
    "username": "admin",
    "password": "secret123"
  }
}
```

With key file:

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "server.example.com",
    "username": "deploy",
    "key_path": "/home/user/.ssh/id_rsa",
    "timeout_secs": 60,
    "compress": true
  }
}
```

With session name (for LLM identification):

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "db.example.com:22",
    "username": "admin",
    "name": "production-database"
  }
}
```

With persistent session (no inactivity timeout):

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "worker.example.com:22",
    "username": "deploy",
    "name": "long-running-job",
    "persistent": true
  }
}
```

---

### ssh_execute

Executes a shell command on a connected SSH session.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | - | Session ID returned from `ssh_connect` |
| `command` | `string` | Yes | - | Shell command to execute on the remote server |
| `timeout_secs` | `u64` | No | `180` | Command execution timeout in seconds (default: 180s / 3 minutes). Falls back to `SSH_COMMAND_TIMEOUT` env var. |

#### Response

Returns `SshCommandResponse`:

```json
{
  "stdout": "total 48\ndrwxr-xr-x  12 user user 4096 Jan 15 10:30 .\n...",
  "stderr": "",
  "exit_code": 0,
  "timed_out": false
}
```

| Field | Type | Description |
|-------|------|-------------|
| `stdout` | `string` | Standard output from the command |
| `stderr` | `string` | Standard error output from the command |
| `exit_code` | `i32` | Command exit code. `-1` if the command timed out or exit status was not received. |
| `timed_out` | `bool` | `true` if the command exceeded `timeout_secs` and was terminated |

#### Timeout Behavior

When a command exceeds the configured `timeout_secs`:

- `timed_out` is set to `true`
- `exit_code` is set to `-1`
- `stdout` and `stderr` contain any partial output collected before the timeout
- The SSH session **remains connected** and can be reused for subsequent commands
- No error is returned; the response is a successful result with timeout indication

This graceful timeout handling allows you to:
- Detect long-running commands without losing the session
- Retrieve partial output for debugging
- Continue using the same session for other commands

#### Example Usage

```json
{
  "tool": "ssh_execute",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "command": "ls -la /var/log",
    "timeout_secs": 30
  }
}
```

Multiple commands:

```json
{
  "tool": "ssh_execute",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "command": "cd /app && git pull && npm install && npm run build"
  }
}
```

---

### ssh_execute_async

Starts a shell command in the background on a connected SSH session and returns immediately with a `command_id` for tracking. Use this for long-running commands (builds, deployments, data processing) or when you want to run multiple commands concurrently.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | - | Session ID returned from `ssh_connect` |
| `command` | `string` | Yes | - | Shell command to execute on the remote server |
| `timeout_secs` | `u64` | No | `180` | Maximum execution time in seconds. The command will be terminated if it exceeds this limit. Falls back to `SSH_COMMAND_TIMEOUT` env var. |

#### Response

Returns `SshExecuteAsyncResponse`:

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "command": "npm run build",
  "started_at": "2024-01-15T14:30:00.000Z",
  "message": "Command started in background. Use ssh_get_command_output to poll for results."
}
```

| Field | Type | Description |
|-------|------|-------------|
| `command_id` | `string` | Unique UUID v4 identifier for tracking this command |
| `session_id` | `string` | Session ID where the command is running |
| `command` | `string` | The command that was started |
| `started_at` | `string` | ISO 8601 timestamp when the command started |
| `message` | `string` | Human-readable message with next steps |

#### Limits

- Maximum 10 concurrent async commands per session
- Commands are automatically cancelled when the session is disconnected
- Default timeout: 180s (configurable via `timeout_secs` or `SSH_COMMAND_TIMEOUT` env)

#### Example Usage

Start a build process:

```json
{
  "tool": "ssh_execute_async",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "command": "cd /app && npm run build",
    "timeout_secs": 300
  }
}
```

Start multiple commands in parallel:

```json
{
  "tool": "ssh_execute_async",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "command": "npm run build"
  }
}
```

```json
{
  "tool": "ssh_execute_async",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "command": "npm test"
  }
}
```

---

### ssh_get_command_output

Retrieves the output and status of an async command started with `ssh_execute_async`. Supports both polling (immediate return) and blocking (wait until complete) modes.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `command_id` | `string` | Yes | - | Command ID returned from `ssh_execute_async` |
| `wait` | `bool` | No | `false` | If `false`, returns immediately with current status. If `true`, blocks until the command completes or `wait_timeout_secs` is reached. |
| `wait_timeout_secs` | `u64` | No | `30` | Maximum time to wait when `wait=true`. Range: 1-300 seconds. |

#### Response

Returns `SshAsyncOutputResponse`:

When command is still running (`wait=false` or wait timeout reached):

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "status": "running",
  "stdout": "Installing dependencies...\n",
  "stderr": "",
  "exit_code": null,
  "error": null,
  "timed_out": false
}
```

When command has completed:

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "status": "completed",
  "stdout": "Build successful!\nOutput written to dist/\n",
  "stderr": "",
  "exit_code": 0,
  "error": null,
  "timed_out": false
}
```

When command was cancelled:

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "status": "cancelled",
  "stdout": "Partial output before cancellation...\n",
  "stderr": "",
  "exit_code": null,
  "error": null,
  "timed_out": false
}
```

When command failed to start:

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "status": "failed",
  "stdout": "",
  "stderr": "",
  "exit_code": null,
  "error": "Failed to open channel: session disconnected",
  "timed_out": false
}
```

When command execution timed out:

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "status": "completed",
  "stdout": "Partial output before timeout...\n",
  "stderr": "",
  "exit_code": -1,
  "error": null,
  "timed_out": true
}
```

| Field | Type | Description |
|-------|------|-------------|
| `command_id` | `string` | The command identifier |
| `status` | `string` | Current status: `running`, `completed`, `cancelled`, or `failed` |
| `stdout` | `string` | Standard output (may be partial if still running) |
| `stderr` | `string` | Standard error output (may be partial if still running) |
| `exit_code` | `i32 \| null` | Exit code when completed, `null` if running/cancelled/failed, `-1` if timed out |
| `error` | `string \| null` | Error message if status is `failed`, otherwise `null` |
| `timed_out` | `bool` | `true` if the command exceeded its `timeout_secs` limit |

#### Status Values

| Status | Description | Available Fields |
|--------|-------------|------------------|
| `running` | Command is still executing | `stdout`, `stderr` (partial output collected so far) |
| `completed` | Command finished execution | `stdout`, `stderr`, `exit_code`, `timed_out` |
| `cancelled` | Command was stopped by user via `ssh_cancel_command` | `stdout`, `stderr` (partial output) |
| `failed` | Command failed to start | `error` message describing the failure |

#### Example Usage

Poll for status (non-blocking):

```json
{
  "tool": "ssh_get_command_output",
  "arguments": {
    "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "wait": false
  }
}
```

Wait for completion (blocking):

```json
{
  "tool": "ssh_get_command_output",
  "arguments": {
    "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "wait": true,
    "wait_timeout_secs": 120
  }
}
```

---

### ssh_list_commands

Lists all async commands across all sessions or filtered by session and/or status.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | No | `null` | Filter commands by session ID. If omitted, returns commands from all sessions. |
| `status` | `string` | No | `null` | Filter by status: `running`, `completed`, `cancelled`, or `failed`. If omitted, returns all statuses. |

#### Response

Returns `AsyncCommandListResponse`:

```json
{
  "commands": [
    {
      "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
      "session_id": "550e8400-e29b-41d4-a716-446655440000",
      "command": "npm run build",
      "status": "running",
      "started_at": "2024-01-15T14:30:00.000Z"
    },
    {
      "command_id": "b2c3d4e5-f6a7-8901-bcde-f23456789012",
      "session_id": "550e8400-e29b-41d4-a716-446655440000",
      "command": "npm test",
      "status": "completed",
      "started_at": "2024-01-15T14:30:05.000Z"
    }
  ],
  "count": 2
}
```

| Field | Type | Description |
|-------|------|-------------|
| `commands` | `AsyncCommandInfo[]` | Array of command metadata objects |
| `count` | `usize` | Total number of commands matching the filter |

#### AsyncCommandInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `command_id` | `string` | Unique command identifier |
| `session_id` | `string` | Session where the command is running |
| `command` | `string` | The shell command being executed |
| `status` | `string` | Current status: `running`, `completed`, `cancelled`, or `failed` |
| `started_at` | `string` | ISO 8601 timestamp when the command started |

#### Example Usage

List all commands:

```json
{
  "tool": "ssh_list_commands",
  "arguments": {}
}
```

List running commands for a specific session:

```json
{
  "tool": "ssh_list_commands",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "status": "running"
  }
}
```

List all completed commands:

```json
{
  "tool": "ssh_list_commands",
  "arguments": {
    "status": "completed"
  }
}
```

---

### ssh_cancel_command

Cancels a running async command and returns any partial output collected before cancellation.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `command_id` | `string` | Yes | - | Command ID to cancel |

#### Response

Returns `SshCancelCommandResponse`:

When successfully cancelled:

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "cancelled": true,
  "message": "Command cancelled successfully",
  "stdout": "Partial output before cancellation...\n",
  "stderr": ""
}
```

When command was not running (already completed/cancelled/failed):

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "cancelled": false,
  "message": "Command is not running (status: completed)",
  "stdout": "Full output from completed command...\n",
  "stderr": ""
}
```

| Field | Type | Description |
|-------|------|-------------|
| `command_id` | `string` | The command identifier |
| `cancelled` | `bool` | `true` if the command was successfully cancelled, `false` if it was not running |
| `message` | `string` | Human-readable status message |
| `stdout` | `string` | Standard output collected before cancellation (or full output if already completed) |
| `stderr` | `string` | Standard error collected before cancellation |

#### Example Usage

```json
{
  "tool": "ssh_cancel_command",
  "arguments": {
    "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
  }
}
```

---

### ssh_forward

Sets up local port forwarding through an SSH tunnel. Only available when compiled with the `port_forward` feature (enabled by default).

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | - | Session ID returned from `ssh_connect` |
| `local_port` | `u16` | Yes | - | Local port to listen on (e.g., `8080`) |
| `remote_address` | `string` | Yes | - | Remote host to forward to (e.g., `localhost` or `10.0.0.1`) |
| `remote_port` | `u16` | Yes | - | Remote port to forward to (e.g., `3306` for MySQL) |

#### Response

Returns `PortForwardingResponse`:

```json
{
  "local_address": "127.0.0.1:8080",
  "remote_address": "localhost:3306",
  "active": true
}
```

| Field | Type | Description |
|-------|------|-------------|
| `local_address` | `string` | Actual local address bound (includes resolved port) |
| `remote_address` | `string` | Remote destination address |
| `active` | `bool` | Whether forwarding is active |

#### Use Cases

| Scenario | Local Port | Remote Address | Remote Port |
|----------|------------|----------------|-------------|
| MySQL tunnel | 3307 | localhost | 3306 |
| Redis tunnel | 6380 | localhost | 6379 |
| Internal API | 8080 | api.internal | 80 |
| PostgreSQL | 5433 | db-primary | 5432 |

#### Example Usage

Forward local port 3307 to remote MySQL on localhost:3306:

```json
{
  "tool": "ssh_forward",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "local_port": 3307,
    "remote_address": "localhost",
    "remote_port": 3306
  }
}
```

Access internal service through bastion:

```json
{
  "tool": "ssh_forward",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "local_port": 8080,
    "remote_address": "internal-api.vpc",
    "remote_port": 80
  }
}
```

---

### ssh_disconnect

Gracefully disconnects an SSH session and releases all resources.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | - | Session ID to disconnect |

#### Response

Returns plain text confirmation:

```
Session 550e8400-e29b-41d4-a716-446655440000 disconnected successfully
```

#### Example Usage

```json
{
  "tool": "ssh_disconnect",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000"
  }
}
```

---

### ssh_list_sessions

Lists all active SSH sessions with their metadata.

#### Parameters

No parameters required.

#### Response

Returns `SessionListResponse`:

```json
{
  "sessions": [
    {
      "session_id": "550e8400-e29b-41d4-a716-446655440000",
      "name": "production-db",
      "host": "192.168.1.100:22",
      "username": "admin",
      "connected_at": "2024-01-15T10:30:00.000Z",
      "default_timeout_secs": 30,
      "retry_attempts": 1,
      "compression_enabled": true
    },
    {
      "session_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
      "host": "server.example.com:22",
      "username": "deploy",
      "connected_at": "2024-01-15T11:45:00.000Z",
      "default_timeout_secs": 60,
      "retry_attempts": 0,
      "compression_enabled": false
    }
  ],
  "count": 2
}
```

| Field | Type | Description |
|-------|------|-------------|
| `sessions` | `SessionInfo[]` | Array of session metadata objects |
| `count` | `usize` | Total number of active sessions |

#### SessionInfo Fields

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | `string` | Unique session identifier |
| `name` | `string` | Optional human-readable session name (omitted from JSON when not set) |
| `host` | `string` | SSH server address |
| `username` | `string` | Authenticated username |
| `connected_at` | `string` | ISO 8601 timestamp of connection |
| `default_timeout_secs` | `u64` | Connection timeout used |
| `retry_attempts` | `u32` | Retries needed to connect |
| `compression_enabled` | `bool` | Whether compression is enabled |

#### Example Usage

```json
{
  "tool": "ssh_list_sessions",
  "arguments": {}
}
```

---

## Response Types

### Common Response Structure

All tools return responses wrapped in MCP's structured content format:

```json
{
  "content": [
    {
      "type": "resource",
      "resource": {
        "mimeType": "application/json",
        "text": "{ ... structured response ... }"
      }
    }
  ]
}
```

### Type Definitions

```typescript
interface SshConnectResponse {
  session_id: string;
  message: string;
  authenticated: boolean;
  retry_attempts: number;
}

interface SshCommandResponse {
  stdout: string;
  stderr: string;
  exit_code: number;
  timed_out: boolean;
}

interface PortForwardingResponse {
  local_address: string;
  remote_address: string;
  active: boolean;
}

interface SessionInfo {
  session_id: string;
  name?: string;  // Optional, omitted when not set
  host: string;
  username: string;
  connected_at: string;
  default_timeout_secs: number;
  retry_attempts: number;
  compression_enabled: boolean;
}

interface SessionListResponse {
  sessions: SessionInfo[];
  count: number;
}

interface SshExecuteAsyncResponse {
  command_id: string;
  session_id: string;
  command: string;
  started_at: string;
  message: string;
}

interface SshAsyncOutputResponse {
  command_id: string;
  status: "running" | "completed" | "cancelled" | "failed";
  stdout: string;
  stderr: string;
  exit_code: number | null;
  error: string | null;
  timed_out: boolean;
}

interface AsyncCommandInfo {
  command_id: string;
  session_id: string;
  command: string;
  status: "running" | "completed" | "cancelled" | "failed";
  started_at: string;
}

interface AsyncCommandListResponse {
  commands: AsyncCommandInfo[];
  count: number;
}

interface SshCancelCommandResponse {
  command_id: string;
  cancelled: boolean;
  message: string;
  stdout: string;
  stderr: string;
}
```

---

## Error Responses

All errors are returned as string messages. Common error patterns:

### Connection Errors

| Error | Cause | Retryable |
|-------|-------|-----------|
| `Connection timed out after Xs` | Server unreachable or timeout too short | Yes |
| `Connection refused` | Server not listening on port | Yes |
| `Network is unreachable` | Network connectivity issue | Yes |
| `No route to host` | Routing problem | Yes |
| `Host is down` | Server offline | Yes |

### Authentication Errors

| Error | Cause | Retryable |
|-------|-------|-----------|
| `Authentication failed` | Wrong credentials | No |
| `Password authentication failed` | Invalid password | No |
| `Key authentication failed` | Invalid or unauthorized key | No |
| `Permission denied` | User not authorized | No |
| `Failed to load private key` | Key file not found or invalid format | No |
| `No identities found in SSH agent` | SSH agent has no keys | No |

### Session Errors

| Error | Cause |
|-------|-------|
| `No active SSH session with ID: xxx` | Session not found or already disconnected |
| `Failed to open channel` | SSH session corrupted |

> **Note**: Command timeouts are **not errors**. When a command times out, `ssh_execute` returns a successful response with `timed_out: true`, `exit_code: -1`, and any partial output collected. The session remains connected.

### Async Command Errors

| Error | Cause |
|-------|-------|
| `No async command found with ID: xxx` | Command ID not found or already cleaned up |
| `Maximum concurrent commands (10) reached for session` | Session has too many running commands |
| `No active SSH session with ID: xxx` | Session not found when starting async command |
| `Wait timeout must be between 1 and 300 seconds` | Invalid `wait_timeout_secs` value |

### Port Forwarding Errors

| Error | Cause |
|-------|-------|
| `Failed to bind to local port X: Address already in use` | Port already in use |
| `Failed to open direct-tcpip channel` | Remote destination unreachable |

### Example Error Response

```json
{
  "error": {
    "code": -1,
    "message": "SSH connection failed after 4 attempt(s). Last error: Connection refused"
  }
}
```

---

## Examples

### Complete Workflow

1. **Connect to server**

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "prod-server.example.com:22",
    "username": "deploy",
    "key_path": "/home/user/.ssh/deploy_key"
  }
}
```

Response:
```json
{
  "session_id": "abc-123-def-456",
  "message": "Successfully connected to deploy@prod-server.example.com:22",
  "authenticated": true,
  "retry_attempts": 0
}
```

2. **Execute deployment commands**

```json
{
  "tool": "ssh_execute",
  "arguments": {
    "session_id": "abc-123-def-456",
    "command": "cd /app && git pull origin main"
  }
}
```

Response:
```json
{
  "stdout": "Already up to date.\n",
  "stderr": "",
  "exit_code": 0,
  "timed_out": false
}
```

3. **Setup database tunnel**

```json
{
  "tool": "ssh_forward",
  "arguments": {
    "session_id": "abc-123-def-456",
    "local_port": 5433,
    "remote_address": "db.internal",
    "remote_port": 5432
  }
}
```

Response:
```json
{
  "local_address": "127.0.0.1:5433",
  "remote_address": "db.internal:5432",
  "active": true
}
```

4. **Check active sessions**

```json
{
  "tool": "ssh_list_sessions",
  "arguments": {}
}
```

Response:
```json
{
  "sessions": [
    {
      "session_id": "abc-123-def-456",
      "name": "prod-deploy",
      "host": "prod-server.example.com:22",
      "username": "deploy",
      "connected_at": "2024-01-15T14:30:00.000Z",
      "default_timeout_secs": 30,
      "retry_attempts": 0,
      "compression_enabled": true
    }
  ],
  "count": 1
}
```

5. **Disconnect when done**

```json
{
  "tool": "ssh_disconnect",
  "arguments": {
    "session_id": "abc-123-def-456"
  }
}
```

Response:
```
Session abc-123-def-456 disconnected successfully
```

### Async Command Workflow

This example demonstrates running multiple commands in parallel and monitoring their progress.

1. **Connect to server**

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "build-server.example.com:22",
    "username": "ci",
    "name": "build-pipeline"
  }
}
```

Response:
```json
{
  "session_id": "build-session-123",
  "message": "Successfully connected to ci@build-server.example.com:22",
  "authenticated": true,
  "retry_attempts": 0
}
```

2. **Start build and test in parallel**

```json
{
  "tool": "ssh_execute_async",
  "arguments": {
    "session_id": "build-session-123",
    "command": "cd /app && npm run build",
    "timeout_secs": 300
  }
}
```

Response:
```json
{
  "command_id": "build-cmd-456",
  "session_id": "build-session-123",
  "command": "cd /app && npm run build",
  "started_at": "2024-01-15T14:30:00.000Z",
  "message": "Command started in background. Use ssh_get_command_output to poll for results."
}
```

```json
{
  "tool": "ssh_execute_async",
  "arguments": {
    "session_id": "build-session-123",
    "command": "cd /app && npm test",
    "timeout_secs": 180
  }
}
```

Response:
```json
{
  "command_id": "test-cmd-789",
  "session_id": "build-session-123",
  "command": "cd /app && npm test",
  "started_at": "2024-01-15T14:30:01.000Z",
  "message": "Command started in background. Use ssh_get_command_output to poll for results."
}
```

3. **Check running commands**

```json
{
  "tool": "ssh_list_commands",
  "arguments": {
    "session_id": "build-session-123",
    "status": "running"
  }
}
```

Response:
```json
{
  "commands": [
    {
      "command_id": "build-cmd-456",
      "session_id": "build-session-123",
      "command": "cd /app && npm run build",
      "status": "running",
      "started_at": "2024-01-15T14:30:00.000Z"
    },
    {
      "command_id": "test-cmd-789",
      "session_id": "build-session-123",
      "command": "cd /app && npm test",
      "status": "running",
      "started_at": "2024-01-15T14:30:01.000Z"
    }
  ],
  "count": 2
}
```

4. **Wait for build to complete**

```json
{
  "tool": "ssh_get_command_output",
  "arguments": {
    "command_id": "build-cmd-456",
    "wait": true,
    "wait_timeout_secs": 120
  }
}
```

Response:
```json
{
  "command_id": "build-cmd-456",
  "status": "completed",
  "stdout": "> app@1.0.0 build\n> webpack --mode production\n\nBuild successful!\nOutput: dist/bundle.js (245kb)\n",
  "stderr": "",
  "exit_code": 0,
  "error": null,
  "timed_out": false
}
```

5. **Wait for tests to complete**

```json
{
  "tool": "ssh_get_command_output",
  "arguments": {
    "command_id": "test-cmd-789",
    "wait": true,
    "wait_timeout_secs": 60
  }
}
```

Response:
```json
{
  "command_id": "test-cmd-789",
  "status": "completed",
  "stdout": "> app@1.0.0 test\n> jest\n\nPASS src/app.test.js\nTests: 42 passed, 42 total\n",
  "stderr": "",
  "exit_code": 0,
  "error": null,
  "timed_out": false
}
```

6. **Disconnect (cleans up all async commands)**

```json
{
  "tool": "ssh_disconnect",
  "arguments": {
    "session_id": "build-session-123"
  }
}
```

Response:
```
Session build-session-123 disconnected successfully
```

### Cancelling a Long-Running Command

1. **Start a potentially slow command**

```json
{
  "tool": "ssh_execute_async",
  "arguments": {
    "session_id": "abc-123-def-456",
    "command": "find / -name '*.log' -type f"
  }
}
```

Response:
```json
{
  "command_id": "search-cmd-111",
  "session_id": "abc-123-def-456",
  "command": "find / -name '*.log' -type f",
  "started_at": "2024-01-15T15:00:00.000Z",
  "message": "Command started in background. Use ssh_get_command_output to poll for results."
}
```

2. **Check progress after a few seconds**

```json
{
  "tool": "ssh_get_command_output",
  "arguments": {
    "command_id": "search-cmd-111",
    "wait": false
  }
}
```

Response:
```json
{
  "command_id": "search-cmd-111",
  "status": "running",
  "stdout": "/var/log/syslog\n/var/log/auth.log\n/var/log/kern.log\n",
  "stderr": "",
  "exit_code": null,
  "error": null,
  "timed_out": false
}
```

3. **Cancel the command (taking too long)**

```json
{
  "tool": "ssh_cancel_command",
  "arguments": {
    "command_id": "search-cmd-111"
  }
}
```

Response:
```json
{
  "command_id": "search-cmd-111",
  "cancelled": true,
  "message": "Command cancelled successfully",
  "stdout": "/var/log/syslog\n/var/log/auth.log\n/var/log/kern.log\n/var/log/daemon.log\n",
  "stderr": ""
}
```

---

## Important Notes

### Authentication

#### Priority Order

Authentication methods are attempted in strict priority order:

1. **Password authentication** - If the `password` parameter is provided
2. **Key file authentication** - If `key_path` is provided (and no password)
3. **SSH agent authentication** - If neither password nor key_path is provided

Only one authentication method is attempted per connection. The server is not consulted for which methods are available.

#### RSA Key Signature Algorithm

For RSA keys (both key files and SSH agent identities), the server's preferred hash algorithm is automatically negotiated:

- Preferred: `rsa-sha2-512` or `rsa-sha2-256`
- Avoided: Legacy `ssh-rsa` (SHA1) for security reasons

This is handled automatically by querying `best_supported_rsa_hash()` from the server during key exchange.

#### SSH Agent

When using SSH agent authentication:
- The agent is accessed via the `SSH_AUTH_SOCK` environment variable
- All available identities are tried in sequence until one succeeds
- If no identities are found, the error "No identities found in SSH agent" is returned

### Retry Logic

#### Retry Behavior

The retry mechanism with exponential backoff **only applies to transient connection errors**. Authentication failures are **never retried** to prevent:

- Account lockouts from repeated failed password attempts
- Wasting time on permanently invalid credentials
- Unnecessary load on authentication systems

#### Exponential Backoff Configuration

| Parameter | Default | Description |
|-----------|---------|-------------|
| Initial delay | 1000ms | First retry delay (configurable via `retry_delay_ms`) |
| Maximum delay | 10s | Backoff is capped at 10 seconds |
| Jitter | Enabled | Random jitter is added to prevent thundering herd |
| Maximum retries | 3 | Total retry attempts (configurable via `max_retries`) |

#### Error Classification

**Retryable errors** (transient, will be retried):
- `connection refused`
- `connection reset`
- `connection timed out` / `timeout`
- `network is unreachable`
- `no route to host`
- `host is down`
- `temporary failure`
- `resource temporarily unavailable`
- `handshake failed`
- `failed to connect`
- `broken pipe`
- `would block`

**Non-retryable errors** (permanent, fail immediately):
- `authentication failed`
- `password authentication failed`
- `key authentication failed`
- `agent authentication failed`
- `permission denied`
- `publickey`
- `auth fail`
- `no authentication`
- `all authentication methods failed`

### Configuration Priority

All configuration values follow a three-tier priority system:

1. **Parameter** (highest) - Explicitly provided function parameter
2. **Environment Variable** - Value from environment variable
3. **Default** (lowest) - Built-in default value

| Setting | Parameter | Environment Variable | Default |
|---------|-----------|---------------------|---------|
| Connection timeout | `timeout_secs` | `SSH_CONNECT_TIMEOUT` | 30s |
| Command timeout | `timeout_secs` | `SSH_COMMAND_TIMEOUT` | 180s |
| Max retries | `max_retries` | `SSH_MAX_RETRIES` | 3 |
| Retry delay | `retry_delay_ms` | `SSH_RETRY_DELAY_MS` | 1000ms |
| Compression | `compress` | `SSH_COMPRESSION` | true |

### Async vs Sync Command Execution

SSH MCP provides two ways to execute commands: synchronous (`ssh_execute`) and asynchronous (`ssh_execute_async`).

#### When to Use `ssh_execute` (Synchronous)

| Scenario | Reason |
|----------|--------|
| Quick commands (< 30s) | No need for background execution overhead |
| Need immediate result | Blocks until command completes |
| Simple one-off commands | Simpler workflow without polling |
| Interactive debugging | Direct feedback loop |

#### When to Use `ssh_execute_async` (Asynchronous)

| Scenario | Reason |
|----------|--------|
| Long-running commands | Builds, deployments, data processing |
| Parallel execution | Run multiple commands concurrently |
| Progress monitoring | Poll for partial output during execution |
| Cancellation needed | Ability to stop mid-execution |
| Timeout management | Monitor and cancel commands that exceed expectations |

#### Async Command Lifecycle

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│ ssh_execute_    │     │ ssh_get_command │     │ ssh_cancel_     │
│ async           │────>│ _output         │────>│ command         │
│                 │     │ (poll/wait)     │     │ (optional)      │
└─────────────────┘     └─────────────────┘     └─────────────────┘
        │                       │                       │
        v                       v                       v
   command_id             status/output            cancelled=true
   returned               returned                 partial output
```

#### Async Command Limits

| Limit | Value | Description |
|-------|-------|-------------|
| Max concurrent per session | 10 | Maximum running commands per SSH session |
| Default timeout | 180s | Configurable via `timeout_secs` or `SSH_COMMAND_TIMEOUT` |
| Max wait timeout | 300s | Maximum value for `wait_timeout_secs` parameter |
| Auto-cleanup | On disconnect | All async commands cancelled when session disconnects |
