# SSH MCP API Reference

This document provides a complete API reference for all MCP tools exposed by the SSH MCP server. Designed for LLM consumption with clear identifier relationships and workflow guidance.

## Table of Contents

- [Quick Reference for LLMs](#quick-reference-for-llms)
- [Key Identifiers](#key-identifiers)
- [Tool Workflow](#tool-workflow)
- [Overview](#overview)
- [Tools](#tools)
  - [ssh_connect](#ssh_connect)
  - [ssh_execute](#ssh_execute)
  - [ssh_get_command_output](#ssh_get_command_output)
  - [ssh_list_commands](#ssh_list_commands)
  - [ssh_cancel_command](#ssh_cancel_command)
  - [ssh_forward](#ssh_forward)
  - [ssh_disconnect](#ssh_disconnect)
  - [ssh_list_sessions](#ssh_list_sessions)
  - [ssh_disconnect_agent](#ssh_disconnect_agent)
  - [ssh_shell_open](#ssh_shell_open)
  - [ssh_shell_write](#ssh_shell_write)
  - [ssh_shell_read](#ssh_shell_read)
  - [ssh_shell_close](#ssh_shell_close)
- [Response Types](#response-types)
- [Error Responses](#error-responses)
- [Examples](#examples)
- [Important Notes](#important-notes)
  - [Authentication](#authentication)
  - [Retry Logic](#retry-logic)
  - [Configuration Priority](#configuration-priority)
  - [Async Command Execution](#async-command-execution)

---

## Quick Reference for LLMs

**IMPORTANT RULES:**
1. **ALWAYS SAVE `session_id`** from `ssh_connect` - required for ALL subsequent operations
2. **ALWAYS SAVE `command_id`** from `ssh_execute` - required to get output or cancel
3. **USE `agent_id`** when multiple agents share the server - enables bulk cleanup
4. **CALL `ssh_disconnect`** when done to release resources
5. **POLL with `ssh_get_command_output`** for long-running commands (builds, deploys)
6. **SAVE `shell_id`** from `ssh_shell_open` - required for shell read/write/close
7. **USE `ssh_shell_*` tools** for interactive PTY sessions (SOL/IPMI/OOB consoles)

**Typical Workflow:**
```
ssh_connect → session_id → ssh_execute → command_id → ssh_get_command_output → ssh_disconnect
```

**Interactive Shell Workflow:**
```
ssh_connect → session_id → ssh_shell_open → shell_id → ssh_shell_write/read → ssh_shell_close
```

---

## Key Identifiers

| Identifier | Source | Used By | Purpose |
|------------|--------|---------|---------|
| `session_id` | `ssh_connect` returns | `ssh_execute`, `ssh_forward`, `ssh_disconnect`, `ssh_list_commands`, `ssh_shell_open` | Identifies SSH connection |
| `command_id` | `ssh_execute` returns | `ssh_get_command_output`, `ssh_cancel_command` | Tracks background command |
| `agent_id` | You provide to `ssh_connect` | `ssh_list_sessions`, `ssh_disconnect_agent` | Groups sessions for bulk operations |
| `shell_id` | `ssh_shell_open` returns | `ssh_shell_write`, `ssh_shell_read`, `ssh_shell_close` | Identifies interactive shell |

**Identifier Flow Diagram:**
```
┌──────────────┐     session_id      ┌──────────────┐     command_id     ┌─────────────────────┐
│ ssh_connect  │ ─────────────────── │ ssh_execute  │ ─────────────────  │ ssh_get_command_    │
│              │                     │              │                    │ output              │
└──────────────┘                     └──────────────┘                    └─────────────────────┘
       │                                    │                                     │
       │ session_id                         │                                     ▼
       ▼                                    │                              ssh_cancel_command
┌──────────────────┐    shell_id     ┌──────────────────┐
│ ssh_shell_open   │ ─────────────── │ ssh_shell_write  │
│                  │                 │ ssh_shell_read   │
└──────────────────┘                 │ ssh_shell_close  │
       │                             └──────────────────┘
       │ (optional)
       ▼
   agent_id ──────────────────────────────────────────────
       │
       ▼
┌──────────────────────┐
│ ssh_disconnect_agent │  ← Disconnects ALL sessions + shells with this agent_id
└──────────────────────┘
```

---

## Tool Workflow

### Basic Command Execution
```
1. ssh_connect(address, username) → SAVE session_id
2. ssh_execute(session_id, command) → SAVE command_id
3. ssh_get_command_output(command_id, wait=true) → GET stdout, stderr, exit_code
4. ssh_disconnect(session_id) → CLEANUP
```

### Parallel Command Execution
```
1. ssh_connect(address, username) → SAVE session_id
2. ssh_execute(session_id, "npm build") → SAVE build_cmd_id
3. ssh_execute(session_id, "npm test") → SAVE test_cmd_id
4. ssh_get_command_output(build_cmd_id, wait=true) → GET build result
5. ssh_get_command_output(test_cmd_id, wait=true) → GET test result
6. ssh_disconnect(session_id) → CLEANUP
```

### Interactive Shell Session
```
1. ssh_connect(address, username) → SAVE session_id
2. ssh_shell_open(session_id, term="xterm", cols=80, rows=24) → SAVE shell_id
3. ssh_shell_write(shell_id, "ls -la\n") → OK
4. ssh_shell_read(shell_id) → GET output data
5. ssh_shell_close(shell_id) → CLEANUP
6. ssh_disconnect(session_id) → CLEANUP
```

### Multi-Agent Cleanup
```
1. ssh_connect(address, username, agent_id="my-agent") → SAVE session_id
2. ssh_connect(address2, username, agent_id="my-agent") → SAVE session_id2
3. ... do work ...
4. ssh_disconnect_agent(agent_id="my-agent") → CLEANUP ALL sessions at once
```

---

## Overview

SSH MCP exposes 13 tools for managing SSH connections, commands, interactive shells, and port forwarding:

| Tool | Action | Returns | Feature Flag |
|------|--------|---------|--------------|
| `ssh_connect` | **CREATES** SSH connection | `session_id` to SAVE | - |
| `ssh_execute` | **STARTS** background command | `command_id` to SAVE | - |
| `ssh_get_command_output` | **RETRIEVES** command output/status | stdout, stderr, exit_code | - |
| `ssh_list_commands` | **LISTS** all commands | command metadata array | - |
| `ssh_cancel_command` | **STOPS** running command | partial output | - |
| `ssh_forward` | **CREATES** port forwarding tunnel | local/remote addresses | `port_forward` |
| `ssh_disconnect` | **CLOSES** single session | confirmation | - |
| `ssh_list_sessions` | **LISTS** active sessions | session metadata array | - |
| `ssh_disconnect_agent` | **CLOSES ALL** sessions for agent | cleanup summary | - |
| `ssh_shell_open` | **OPENS** interactive PTY shell | `shell_id` to SAVE | - |
| `ssh_shell_write` | **SENDS** input to shell | confirmation | - |
| `ssh_shell_read` | **READS** shell output | data, status | - |
| `ssh_shell_close` | **CLOSES** interactive shell | confirmation | - |

---

## Tools

### ssh_connect

**ACTION:** Creates a new SSH connection and returns a `session_id` that you MUST SAVE.

**LLM GUIDANCE:**
- **SAVE the `session_id`** from the response - you need it for ALL other operations
- **OPTIONALLY provide `agent_id`** if multiple agents share the server (enables `ssh_disconnect_agent`)
- **OPTIONALLY provide `name`** for human-readable session identification
- **USE `persistent: true`** for long-running sessions that shouldn't timeout

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
| `session_id` | `string` | No | `null` | Reuse existing session ID. If valid and connected, returns that session instead of creating new one. |
| `agent_id` | `string` | No | `null` | Agent identifier for grouping sessions. **USE THIS** when multiple agents share the server. Enables `ssh_disconnect_agent` for bulk cleanup. |

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

**⚠️ IMPORTANT: SAVE `session_id` - you need it for ssh_execute, ssh_forward, and ssh_disconnect**

```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "agent_id": "my-agent-id",
  "message": "SSH CONNECTION ESTABLISHED. REMEMBER THESE IDENTIFIERS:\n• session_id: '550e8400-...'\n• agent_id: 'my-agent-id'\n• host: user@192.168.1.1:22",
  "authenticated": true,
  "retry_attempts": 0
}
```

With persistent session and name:

```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "agent_id": "my-agent-id",
  "message": "SSH CONNECTION ESTABLISHED. REMEMBER THESE IDENTIFIERS:\n• session_id: '550e8400-...'\n• agent_id: 'my-agent-id'\n• name: 'production-db'\n• host: user@192.168.1.1:22\n• persistent: true",
  "authenticated": true,
  "retry_attempts": 0
}
```

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | `string` | **SAVE THIS** - Unique UUID v4 identifier required for all session operations |
| `agent_id` | `string \| null` | Agent ID if provided (for bulk operations via `ssh_disconnect_agent`) |
| `message` | `string` | Human-readable message with all identifiers to remember |
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

**ACTION:** Starts a command in background and returns a `command_id` that you MUST SAVE.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect` - pass it as parameter
- **SAVE the `command_id`** from the response - you need it for `ssh_get_command_output` or `ssh_cancel_command`
- **USE for long-running commands** (builds, deployments, data processing)
- **RUN MULTIPLE in parallel** on same session - each gets unique `command_id`

Starts a shell command in the background on a connected SSH session and returns immediately with a `command_id` for tracking. Use `ssh_get_command_output` to poll for status and retrieve output.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | - | Session ID returned from `ssh_connect` |
| `command` | `string` | Yes | - | Shell command to execute on the remote server |
| `timeout_secs` | `u64` | No | `180` | Maximum execution time in seconds. The command will be terminated if it exceeds this limit. Falls back to `SSH_COMMAND_TIMEOUT` env var. |

#### Response

Returns `SshExecuteResponse`:

**⚠️ IMPORTANT: SAVE `command_id` - you need it for ssh_get_command_output or ssh_cancel_command**

```json
{
  "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "agent_id": "my-agent-id",
  "command": "npm run build",
  "started_at": "2024-01-15T14:30:00.000Z",
  "message": "COMMAND STARTED. REMEMBER: command_id='a1b2c3d4-...' Use ssh_get_command_output to poll for results."
}
```

| Field | Type | Description |
|-------|------|-------------|
| `command_id` | `string` | **SAVE THIS** - Unique UUID v4 required for `ssh_get_command_output` and `ssh_cancel_command` |
| `session_id` | `string` | Session ID where the command is running |
| `agent_id` | `string \| null` | Agent ID if the session was created with one |
| `command` | `string` | The command that was started |
| `started_at` | `string` | ISO 8601 timestamp when the command started |
| `message` | `string` | Human-readable message with next steps |

#### Limits

- Maximum 100 concurrent commands per session
- Commands are automatically cancelled when the session is disconnected
- Default timeout: 180s (configurable via `timeout_secs` or `SSH_COMMAND_TIMEOUT` env)

#### Example Usage

Start a build process:

```json
{
  "tool": "ssh_execute",
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
  "tool": "ssh_execute",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "command": "npm run build"
  }
}
```

```json
{
  "tool": "ssh_execute",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "command": "npm test"
  }
}
```

---

### ssh_get_command_output

**ACTION:** Retrieves output and status of a background command.

**LLM GUIDANCE:**
- **REQUIRES `command_id`** from `ssh_execute` - pass it as parameter
- **USE `wait=false`** to poll immediately (check progress without blocking)
- **USE `wait=true`** to block until command completes (simplest approach)
- **CHECK `status` field**: `running` (still working), `completed` (done), `cancelled`, `failed`
- **CHECK `timed_out` field**: if `true`, command exceeded timeout but partial output is available

Retrieves the output and status of a command started with `ssh_execute`. Supports both polling (immediate return) and blocking (wait until complete) modes.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `command_id` | `string` | Yes | - | Command ID returned from `ssh_execute` |
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

**ACTION:** Lists all background commands, optionally filtered by session or status.

**LLM GUIDANCE:**
- **USE to find command_ids** if you lost track of running commands
- **FILTER by `session_id`** to see commands for a specific session
- **FILTER by `status`** to find only `running`, `completed`, `cancelled`, or `failed` commands
- **RETURNS array** of command metadata (not output - use `ssh_get_command_output` for that)

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

**ACTION:** Stops a running command and returns partial output collected so far.

**LLM GUIDANCE:**
- **REQUIRES `command_id`** from `ssh_execute` - pass it as parameter
- **USE to stop** commands that are taking too long or no longer needed
- **RETURNS partial stdout/stderr** collected before cancellation
- **ONLY works on `running` commands** - returns `cancelled: false` for already completed commands

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

**ACTION:** Creates a local port forwarding tunnel through SSH.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect` - pass it as parameter
- **USE to access** databases, internal APIs, or other services behind SSH
- **LOCAL PORT** is on your machine - connect your tools to `localhost:local_port`
- **REMOTE ADDRESS** is from the SSH server's perspective (often `localhost` for local services)

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

**ACTION:** Closes a single SSH session and releases all resources.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect` - pass it as parameter
- **ALWAYS CALL when done** with a session to free resources
- **AUTOMATICALLY CANCELS** all running commands for that session
- **USE `ssh_disconnect_agent`** instead to disconnect ALL sessions for an agent at once

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

**ACTION:** Lists all active SSH sessions with their metadata.

**LLM GUIDANCE:**
- **USE to find session_ids** if you lost track of active sessions
- **FILTER by `agent_id`** to see only your sessions (when multiple agents share server)
- **CHECK `healthy` field** to see if sessions are still responsive
- **RETURNS array** of session metadata including host, username, connected_at

Lists all active SSH sessions with their metadata.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `agent_id` | `string` | No | `null` | Filter sessions by agent ID. If omitted, returns all sessions. |

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
| `last_health_check` | `string` | Optional ISO 8601 timestamp of last health check (omitted when not set) |
| `healthy` | `bool` | Optional health status from last check (omitted when not set) |

#### Example Usage

```json
{
  "tool": "ssh_list_sessions",
  "arguments": {}
}
```

Filter by agent:

```json
{
  "tool": "ssh_list_sessions",
  "arguments": {
    "agent_id": "my-unique-agent-id"
  }
}
```

---

### ssh_disconnect_agent

**ACTION:** Disconnects ALL sessions belonging to a specific agent in one call.

**LLM GUIDANCE:**
- **REQUIRES `agent_id`** that you provided to `ssh_connect` - pass it as parameter
- **USE for bulk cleanup** when you have multiple sessions to close
- **AUTOMATICALLY CANCELS** all running commands across all disconnected sessions
- **BEST PRACTICE:** Always use `agent_id` when creating sessions, then call this for cleanup

Disconnects all SSH sessions associated with a specific agent identifier. This is a bulk cleanup operation that:
1. Finds all sessions with the matching `agent_id`
2. Cancels all running commands on those sessions
3. Disconnects all sessions
4. Returns a summary of what was cleaned up

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `agent_id` | `string` | Yes | - | The agent identifier used when creating sessions via `ssh_connect`. All sessions with this `agent_id` will be disconnected. |

#### Response

Returns `AgentDisconnectResponse`:

```json
{
  "agent_id": "my-unique-agent-id",
  "sessions_disconnected": 3,
  "commands_cancelled": 5,
  "message": "AGENT CLEANUP COMPLETE. SUMMARY:\n• agent_id: 'my-unique-agent-id'\n• sessions_disconnected: 3\n• commands_cancelled: 5\n\nAll sessions and commands for agent 'my-unique-agent-id' have been terminated."
}
```

| Field | Type | Description |
|-------|------|-------------|
| `agent_id` | `string` | The agent identifier that was cleaned up |
| `sessions_disconnected` | `usize` | Number of sessions that were disconnected |
| `commands_cancelled` | `usize` | Number of running commands that were cancelled |
| `message` | `string` | Human-readable summary of the cleanup |

#### Example Usage

```json
{
  "tool": "ssh_disconnect_agent",
  "arguments": {
    "agent_id": "my-unique-agent-id"
  }
}
```

#### Best Practices

1. **Always use `agent_id`** when calling `ssh_connect` if you might create multiple sessions
2. **Use a unique identifier** like your project folder path, UUID, or agent name
3. **Call `ssh_disconnect_agent`** at the end of your task instead of multiple `ssh_disconnect` calls
4. **Handles edge cases gracefully**: Returns `sessions_disconnected: 0` if no sessions found (not an error)

---

### ssh_shell_open

**ACTION:** Opens an interactive PTY shell session and returns a `shell_id` that you MUST SAVE.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect` - pass it as parameter
- **SAVE the `shell_id`** from the response - you need it for `ssh_shell_write`, `ssh_shell_read`, `ssh_shell_close`
- **USE for interactive sessions** - SOL/IPMI/OOB consoles, serial devices, commands requiring PTY (sudo, top)
- **USE `term_type: "vt100"`** for Serial Over LAN / IPMI / OOB access

Opens an interactive pseudo-terminal (PTY) shell session on a connected SSH session. The shell runs persistently and accepts input/output via `ssh_shell_write` and `ssh_shell_read`.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | - | Session ID returned from `ssh_connect` |
| `term_type` | `string` | No | `xterm` | Terminal type (e.g., `xterm`, `vt100`, `ansi`). Use `vt100` for SOL/IPMI/OOB. |
| `cols` | `u32` | No | `80` | Terminal width in columns |
| `rows` | `u32` | No | `24` | Terminal height in rows |

#### Response

Returns `SshShellOpenResponse`:

**⚠️ IMPORTANT: SAVE `shell_id` - you need it for ssh_shell_write, ssh_shell_read, and ssh_shell_close**

```json
{
  "shell_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "agent_id": "my-agent-id",
  "term_type": "xterm",
  "message": "INTERACTIVE SHELL OPENED. REMEMBER THESE IDENTIFIERS:\n• shell_id: 'a1b2c3d4-...'\n• session_id: '550e8400-...'\n• term: xterm (80x24)\n\nUse ssh_shell_write with shell_id 'a1b2c3d4-...' to send input.\nUse ssh_shell_read with shell_id 'a1b2c3d4-...' to read output.\nUse ssh_shell_close with shell_id 'a1b2c3d4-...' to close the shell."
}
```

| Field | Type | Description |
|-------|------|-------------|
| `shell_id` | `string` | **SAVE THIS** - Unique UUID v4 required for all shell operations |
| `session_id` | `string` | Session ID where the shell is running |
| `agent_id` | `string \| null` | Agent ID if the session was created with one |
| `term_type` | `string` | Terminal type used |
| `message` | `string` | Human-readable message with identifiers and next steps |

#### Limits

- Maximum 10 concurrent shells per session
- Shells are automatically closed when the session is disconnected

#### Example Usage

Standard interactive shell:

```json
{
  "tool": "ssh_shell_open",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "term_type": "xterm",
    "cols": 80,
    "rows": 24
  }
}
```

SOL / IPMI / OOB console:

```json
{
  "tool": "ssh_shell_open",
  "arguments": {
    "session_id": "550e8400-e29b-41d4-a716-446655440000",
    "term_type": "vt100",
    "cols": 80,
    "rows": 24
  }
}
```

---

### ssh_shell_write

**ACTION:** Sends input data to an interactive shell.

**LLM GUIDANCE:**
- **REQUIRES `shell_id`** from `ssh_shell_open` - pass it as parameter
- **SEND text with newlines** to execute commands (e.g., `"ls -la\n"`)
- **SEND escape sequences** for special keys (e.g., `"\x03"` for Ctrl+C)
- **DATA is sent as-is** to the shell's stdin

Sends input data (text, keystrokes, escape sequences) to an open interactive shell.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `shell_id` | `string` | Yes | - | Shell ID returned from `ssh_shell_open` |
| `data` | `string` | Yes | - | Input data to send to the shell (text, commands, escape sequences) |

#### Response

Returns plain text confirmation:

```
Data sent to shell a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

#### Example Usage

Execute a command:

```json
{
  "tool": "ssh_shell_write",
  "arguments": {
    "shell_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "data": "ls -la\n"
  }
}
```

Send Ctrl+C:

```json
{
  "tool": "ssh_shell_write",
  "arguments": {
    "shell_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "data": "\u0003"
  }
}
```

---

### ssh_shell_read

**ACTION:** Reads accumulated output from an interactive shell.

**LLM GUIDANCE:**
- **REQUIRES `shell_id`** from `ssh_shell_open` - pass it as parameter
- **RETURNS accumulated output** since the last read
- **CHECK `status` field**: `open` (shell active) or `closed` (shell terminated)
- **CALL after `ssh_shell_write`** to read command output

Reads and returns accumulated output from an open interactive shell. Output includes everything written to the shell's PTY since the last read.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `shell_id` | `string` | Yes | - | Shell ID returned from `ssh_shell_open` |

#### Response

Returns `SshShellReadResponse`:

```json
{
  "shell_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "data": "total 42\ndrwxr-xr-x  5 user group 160 Jan 15 10:30 .\ndrwxr-xr-x 12 user group 384 Jan 14 09:00 ..\n",
  "status": "open"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `shell_id` | `string` | The shell identifier |
| `data` | `string` | Accumulated output from the shell |
| `status` | `string` | Shell status: `open` (active) or `closed` (terminated) |

#### Example Usage

```json
{
  "tool": "ssh_shell_read",
  "arguments": {
    "shell_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
  }
}
```

---

### ssh_shell_close

**ACTION:** Closes an interactive shell session and releases resources.

**LLM GUIDANCE:**
- **REQUIRES `shell_id`** from `ssh_shell_open` - pass it as parameter
- **CALL when done** with the interactive session to free resources
- **SHELLS are also closed** when `ssh_disconnect` is called for the session

Closes an open interactive shell session, terminates the PTY channel, and releases all associated resources.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `shell_id` | `string` | Yes | - | Shell ID to close |

#### Response

Returns `SshShellCloseResponse`:

```json
{
  "shell_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "closed": true,
  "message": "Shell closed successfully"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `shell_id` | `string` | The shell identifier |
| `closed` | `bool` | `true` if the shell was successfully closed |
| `message` | `string` | Human-readable status message |

#### Example Usage

```json
{
  "tool": "ssh_shell_close",
  "arguments": {
    "shell_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
  }
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
  session_id: string;      // SAVE THIS - required for all session operations
  agent_id?: string;       // Present if agent_id was provided to ssh_connect
  message: string;         // Human-readable message with identifiers to remember
  authenticated: boolean;
  retry_attempts: number;
}

// SshCommandResponse is internal - not exposed via MCP tools

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
  last_health_check?: string;  // Optional, ISO 8601 timestamp of last health check
  healthy?: boolean;  // Optional, health status from last check
}

interface SessionListResponse {
  sessions: SessionInfo[];
  count: number;
}

interface SshExecuteResponse {
  command_id: string;      // SAVE THIS - required for ssh_get_command_output and ssh_cancel_command
  session_id: string;
  agent_id?: string;       // Present if session was created with agent_id
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

interface AgentDisconnectResponse {
  agent_id: string;                // The agent that was cleaned up
  sessions_disconnected: number;   // Number of sessions closed
  commands_cancelled: number;      // Number of commands stopped
  message: string;                 // Human-readable summary
}

interface SshShellOpenResponse {
  shell_id: string;        // SAVE THIS - required for all shell operations
  session_id: string;
  agent_id?: string;       // Present if session was created with agent_id
  term_type: string;       // Terminal type used (e.g., "xterm", "vt100")
  message: string;         // Human-readable message with identifiers
}

interface SshShellReadResponse {
  shell_id: string;
  data: string;            // Accumulated output from the shell
  status: "open" | "closed";
}

interface SshShellCloseResponse {
  shell_id: string;
  closed: boolean;
  message: string;
}

interface ShellInfo {
  shell_id: string;
  session_id: string;
  term_type: string;
  cols: number;
  rows: number;
  opened_at: string;       // ISO 8601 timestamp
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
| `Maximum concurrent commands (100) reached for session` | Session has too many running commands |
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
  "command_id": "cmd-789-xyz",
  "session_id": "abc-123-def-456",
  "command": "cd /app && git pull origin main",
  "started_at": "2024-01-15T14:30:00.000Z",
  "message": "COMMAND STARTED. REMEMBER: command_id='cmd-789-xyz'"
}
```

3. **Get command output**

```json
{
  "tool": "ssh_get_command_output",
  "arguments": {
    "command_id": "cmd-789-xyz",
    "wait": true
  }
}
```

Response:
```json
{
  "command_id": "cmd-789-xyz",
  "status": "completed",
  "stdout": "Already up to date.\n",
  "stderr": "",
  "exit_code": 0,
  "timed_out": false
}
```

4. **Setup database tunnel**

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

5. **Check active sessions**

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

6. **Disconnect when done**

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
  "tool": "ssh_execute",
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
  "tool": "ssh_execute",
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
  "tool": "ssh_execute",
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

### Async Command Execution

SSH MCP executes commands asynchronously, returning a `command_id` immediately that can be polled for status and output.

#### When to Use Different Patterns

| Scenario | Recommended Pattern |
|----------|---------------------|
| Quick commands | `ssh_execute` → `ssh_get_command_output(wait=true)` |
| Long-running commands | `ssh_execute` → poll with `ssh_get_command_output(wait=false)` |
| Parallel execution | Multiple `ssh_execute` calls → poll each command |
| Progress monitoring | Poll with `ssh_get_command_output(wait=false)` periodically |
| Cancellation needed | Use `ssh_cancel_command` to stop mid-execution |

#### Command Lifecycle

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│ ssh_execute     │     │ ssh_get_command │     │ ssh_cancel_     │
│                 │────>│ _output         │────>│ command         │
│                 │     │ (poll/wait)     │     │ (optional)      │
└─────────────────┘     └─────────────────┘     └─────────────────┘
        │                       │                       │
        v                       v                       v
   command_id             status/output            cancelled=true
   returned               returned                 partial output
```

#### Command Limits

| Limit | Value | Description |
|-------|-------|-------------|
| Max concurrent per session | 100 | Maximum running commands per SSH session |
| Default timeout | 180s | Configurable via `timeout_secs` or `SSH_COMMAND_TIMEOUT` |
| Max wait timeout | 300s | Maximum value for `wait_timeout_secs` parameter |
| Auto-cleanup | On disconnect | All commands cancelled when session disconnects |
