# SSH MCP API Reference

This document provides a complete API reference for all MCP tools exposed by the SSH MCP server.

## Table of Contents

- [Overview](#overview)
- [Tools](#tools)
  - [ssh_connect](#ssh_connect)
  - [ssh_execute](#ssh_execute)
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

---

## Overview

SSH MCP exposes 5 tools for managing SSH connections and operations:

| Tool | Description | Feature Flag |
|------|-------------|--------------|
| `ssh_connect` | Establish SSH connection | - |
| `ssh_execute` | Execute remote command | - |
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

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | `string` | Unique UUID v4 identifier for the session |
| `message` | `string` | Human-readable success message |
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
  "exit_code": 0
}
```

| Field | Type | Description |
|-------|------|-------------|
| `stdout` | `string` | Standard output from the command |
| `stderr` | `string` | Standard error output from the command |
| `exit_code` | `i32` | Command exit code. `-1` if exit status was not received. |

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
}

interface PortForwardingResponse {
  local_address: string;
  remote_address: string;
  active: boolean;
}

interface SessionInfo {
  session_id: string;
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
| `Command execution timed out after Xs` | Command exceeded timeout |

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
  "exit_code": 0
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
