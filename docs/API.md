# SSH MCP API Reference

This document provides a complete API reference for all 16 MCP tools exposed by the SSH MCP server. Since v2.0 every tool returns a **plain markdown string** instead of a structured JSON payload — this reference describes both the tool inputs and the exact output format (status lines, keys, and output blocks) the server emits.

## Table of Contents

[[_TOC_]]

## Quick Reference for LLMs

**IMPORTANT RULES:**

1. **ALWAYS SAVE `SESSION_ID`** from `ssh_connect` — required for ALL subsequent operations.
2. **ALWAYS SAVE `COMMAND_ID`** from `ssh_execute` — required to get output or cancel.
3. **USE `agent_id`** when multiple agents share the server — enables bulk cleanup via `ssh_disconnect_agent`.
4. **CALL `ssh_disconnect`** when done to release resources.
5. **POLL with `ssh_get_command_output`** for long-running commands (builds, deploys).
6. **SAVE `SHELL_ID`** from `ssh_shell_open` — required for shell read/write/close.
7. **USE `ssh_shell_*` tools** for interactive PTY sessions (SOL/IPMI/OOB consoles).
8. **SAVE `TRANSFER_ID`** from `ssh_upload`/`ssh_download` — required for progress tracking.
9. **POLL with `ssh_get_transfer_progress`** for file transfer status.
10. **USE `reuse="auto"`** on repeated `ssh_connect` calls to the same host/user to avoid creating duplicate sessions.

**Typical Workflow:**
```
ssh_connect -> SESSION_ID -> ssh_execute -> COMMAND_ID -> ssh_get_command_output -> ssh_disconnect
```

**Interactive Shell Workflow:**
```
ssh_connect -> SESSION_ID -> ssh_shell_open -> SHELL_ID -> ssh_shell_write/read -> ssh_shell_close
```

**SFTP Transfer Workflow:**
```
ssh_connect -> SESSION_ID -> ssh_upload/ssh_download -> TRANSFER_ID -> ssh_get_transfer_progress -> done
```

## Key Identifiers

| Identifier | Source | Used By | Purpose |
|------------|--------|---------|---------|
| `SESSION_ID` | `ssh_connect` returns | `ssh_execute`, `ssh_forward`, `ssh_disconnect`, `ssh_list_commands`, `ssh_shell_open`, `ssh_upload`, `ssh_download` | Identifies SSH connection |
| `COMMAND_ID` | `ssh_execute` returns | `ssh_get_command_output`, `ssh_cancel_command` | Tracks background command |
| `AGENT` | You provide `agent_id` to `ssh_connect` | `ssh_list_sessions` (filter), `ssh_disconnect_agent` (bulk) | Groups sessions |
| `SHELL_ID` | `ssh_shell_open` returns | `ssh_shell_write`, `ssh_shell_read`, `ssh_shell_close` | Identifies interactive shell |
| `TRANSFER_ID` | `ssh_upload` / `ssh_download` return | `ssh_get_transfer_progress` | Tracks SFTP file transfer |

```
┌──────────────┐  SESSION_ID   ┌──────────────┐  COMMAND_ID   ┌──────────────────────┐
│ ssh_connect  │──────────────>│ ssh_execute  │──────────────>│ ssh_get_command_out  │
└──────┬───────┘               └──────────────┘               └──────────┬───────────┘
       │                                                                 │
       │ SESSION_ID                                                      ▼
       ▼                                                        ssh_cancel_command
┌──────────────────┐  SHELL_ID   ┌──────────────────┐
│ ssh_shell_open   │────────────>│ ssh_shell_write  │
│                  │             │ ssh_shell_read   │
└──────┬───────────┘             │ ssh_shell_close  │
       │                         └──────────────────┘
       │ agent_id (optional)
       ▼
┌──────────────────────┐
│ ssh_disconnect_agent │  <- Disconnects ALL sessions + shells + transfers for this agent
└──────────────────────┘
```

## Response Format (v2.0)

All 16 tools return a single markdown `Text<String>`. The format follows a small, predictable grammar.

### Status Values

The first line is always `TOOL_NAME: STATUS` (e.g. `SSH_CONNECT: OK`). The status word is one of:

| Status | Meaning |
|--------|---------|
| `OK` | Operation succeeded, see the body for identifiers. |
| `REUSED` | `ssh_connect` returned an existing healthy session instead of creating a new one. |
| `SUGGESTED` | `ssh_connect` found one or more matching sessions and stopped so the caller can decide. |
| `STARTED` | Background work kicked off (command or transfer) — poll for progress. |
| `RUNNING` | Background work is still in progress; any output block carries the `(partial)` marker. |
| `COMPLETED` | Background work finished. `EXIT:` (for commands) or `PROGRESS:` (for transfers) reports the result. |
| `TIMEOUT` | Command timed out; partial output is still reported. |
| `CANCELLED` | Work was cancelled by the caller; partial output is still reported. |
| `NOOP` | Idempotent cancel — command was not running. |
| `OPEN` / `CLOSED` | Shell state. |
| `ACTIVE` | Port forwarding tunnel is live. |
| `ERROR` | See the `REASON`/`DETAIL` lines below. |

### Layouts

- **Inline layout** — chosen when the response has ≤3 simple fields and no output block:
  ```
  TOOL: STATUS | KEY: value | KEY: value
  ```
- **Block layout** — used when there are 4+ fields or when any output block is embedded:
  ```
  TOOL: STATUS
  KEY: value
  KEY: value
  --- name [nonce] ---
  <content>
  ```

### Output Blocks

`ssh_get_command_output`, `ssh_cancel_command` and `ssh_shell_read` wrap raw byte output in a delimiter that carries a fresh 8-character lowercase hex nonce so content can never forge the delimiter. The delimiters are:

```
--- stdout [<nonce>] ---
--- stdout [<nonce>] (partial) ---
--- stdout [<nonce>] (empty) ---
--- stdout [<nonce>] (partial, empty) ---
--- stdout [<nonce>] (truncated: showing 16.0KB of 2.3MB) ---
--- stdout [<nonce>] (partial, truncated: showing 16.0KB of 2.3MB) ---
```

The content is UTF-8 safely truncated to the tail (most recent bytes) when `max_output_bytes` is exceeded. A trailing newline in the content is stripped so blocks can be joined without blank lines.

`ssh_shell_read` uses `--- data [<nonce>] ---` instead of `stdout`/`stderr`.

### Error Format

Every tool that can fail emits:

```
TOOL_NAME: ERROR
REASON: [CODE] human-readable message
DETAIL: optional context (head-truncated at 2 KiB with " (truncated)" marker when longer)
```

Error codes used by the tools:

| Code | Emitted by | Meaning |
|------|-----------|---------|
| `SESSION_NOT_FOUND` | `ssh_execute`, `ssh_disconnect`, `ssh_shell_open`, `ssh_upload`, `ssh_download`, `ssh_forward` | No active session with the provided ID. |
| `COMMAND_NOT_FOUND` | `ssh_get_command_output`, `ssh_cancel_command` | Command already cleaned up or unknown ID. |
| `COMMAND_FAILED` | `ssh_get_command_output` | Command transitioned to `failed`; reason field carries the error message. |
| `MAX_COMMANDS_EXCEEDED` | `ssh_execute` | 100 running commands per session cap hit. |
| `MAX_SHELLS_EXCEEDED` | `ssh_shell_open` | 10 shells per session cap hit. |
| `MAX_TRANSFERS_EXCEEDED` | `ssh_upload`, `ssh_download` | 10 transfers per session cap hit. |
| `SHELL_NOT_FOUND` | `ssh_shell_write`, `ssh_shell_read`, `ssh_shell_close` | Shell already closed / unknown ID. |
| `CHANNEL_FAILED` | `ssh_shell_open` | PTY channel could not be opened. |
| `WRITE_FAILED` | `ssh_shell_write` | Writing to shell channel failed. |
| `TRANSFER_NOT_FOUND` | `ssh_get_transfer_progress` | Transfer not found / already cleaned up. |
| `CONNECTION_FAILED` | `ssh_connect` | All retries exhausted or auth failed. |
| `FORWARD_FAILED` | `ssh_forward` | Failed to bind local port / open direct-tcpip. |
| `FEATURE_DISABLED` | `ssh_forward` | Built without `--features port_forward`. |
| `LOCAL_FILE_ERROR` | `ssh_upload` | Cannot access the local file. |
| `LOCAL_NOT_FILE` | `ssh_upload` | Local path is not a regular file. |
| `REMOTE_METADATA_ERROR` | `ssh_download` | Cannot stat the remote file. |
| `SFTP_OPEN_FAILED` | `ssh_download` | Cannot open the SFTP subsystem. |
| `FILE_NOT_FOUND` / `PERMISSION_DENIED` / `DISK_FULL` / `CONNECTION_LOST` / `REMOTE_DIR_NOT_FOUND` / `READ_ONLY_FS` / `SFTP_PROTOCOL` / `TIMEOUT` / `IO_ERROR` | SFTP classifier | Applied automatically by `classify_transfer_error` when the raw SFTP error matches a known pattern. |

## Tool Workflow

### Basic Command Execution
```
1. ssh_connect(address, username) -> SESSION_ID
2. ssh_execute(session_id, command) -> COMMAND_ID
3. ssh_get_command_output(command_id, wait=true) -> stdout/stderr/EXIT
4. ssh_disconnect(session_id) -> cleanup
```

### Parallel Command Execution
```
1. ssh_connect(address, username) -> SESSION_ID
2. ssh_execute(session_id, "npm build") -> build_cmd_id
3. ssh_execute(session_id, "npm test")  -> test_cmd_id
4. ssh_get_command_output(build_cmd_id, wait=true)
5. ssh_get_command_output(test_cmd_id,  wait=true)
6. ssh_disconnect(session_id)
```

### Interactive Shell Session
```
1. ssh_connect(address, username) -> SESSION_ID
2. ssh_shell_open(session_id, term="xterm", cols=80, rows=24) -> SHELL_ID
3. ssh_shell_write(shell_id, "ls -la\n")
4. ssh_shell_read(shell_id, clear=true, max_output_bytes=16384)
5. ssh_shell_close(shell_id)
6. ssh_disconnect(session_id)
```

### Multi-Agent Cleanup
```
1. ssh_connect(address, username, agent_id="my-agent") -> SESSION_ID
2. ssh_connect(address2, username, agent_id="my-agent") -> SESSION_ID_2
3. ... do work ...
4. ssh_disconnect_agent(agent_id="my-agent") -> CLEANUP ALL sessions/shells/transfers at once
```

## Overview

SSH MCP exposes **16 tools** for managing SSH connections, commands, interactive shells, SFTP transfers, and port forwarding:

| Tool | Action | Key Output | Feature Flag |
|------|--------|------------|--------------|
| `ssh_connect` | **CREATES / REUSES** SSH connection | `SESSION_ID` | — |
| `ssh_execute` | **STARTS** background command | `COMMAND_ID` | — |
| `ssh_get_command_output` | **RETRIEVES** command output/status | `RUNNING`/`COMPLETED`/`TIMEOUT`, stdout/stderr blocks, `EXIT` | — |
| `ssh_list_commands` | **LISTS** all commands | Bullet list (`COUNT: N`) | — |
| `ssh_cancel_command` | **STOPS** running command | `CANCELLED` or `NOOP` + partial output | — |
| `ssh_forward` | **CREATES** port forwarding tunnel | `LOCAL`, `REMOTE`, `ACTIVE` | `port_forward` |
| `ssh_disconnect` | **CLOSES** single session | inline confirmation | — |
| `ssh_list_sessions` | **LISTS** active sessions | Bullet list (`COUNT: N`) | — |
| `ssh_disconnect_agent` | **CLOSES ALL** sessions for agent | `SESSIONS`, `COMMANDS` cleanup counts | — |
| `ssh_shell_open` | **OPENS** interactive PTY shell | `SHELL_ID` | — |
| `ssh_shell_write` | **SENDS** input to shell | `BYTES_SENT` | — |
| `ssh_shell_read` | **READS** shell output | `OPEN`/`CLOSED` + `data` block | — |
| `ssh_shell_close` | **CLOSES** interactive shell | inline confirmation | — |
| `ssh_upload` | **UPLOADS** file via SFTP | `TRANSFER_ID`, `SIZE` | — |
| `ssh_download` | **DOWNLOADS** file via SFTP | `TRANSFER_ID`, `SIZE` | — |
| `ssh_get_transfer_progress` | **CHECKS** transfer status | `RUNNING`/`COMPLETED`/`FAILED` + progress bytes | — |

## Tools

### ssh_connect

**ACTION:** Creates (or reuses) an SSH connection and returns a `SESSION_ID` that you MUST SAVE.

**LLM GUIDANCE:**
- **SAVE the `SESSION_ID`** from the response — you need it for all other operations.
- **OPTIONALLY provide `agent_id`** if multiple agents share the server (enables `ssh_disconnect_agent`).
- **OPTIONALLY provide `name`** for human-readable session identification.
- **USE `persistent=true`** for long-running sessions that shouldn't timeout.
- **USE `reuse="auto"`** to transparently reuse an existing healthy session for the same identity triple `(host, port, username)`.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `address` | `string` | Yes | — | SSH server address in format `host:port` (e.g., `192.168.1.1:22`). Port defaults to 22 when omitted. |
| `username` | `string` | Yes | — | SSH username for authentication. |
| `password` | `string` | No | `null` | Password for password-based authentication. |
| `key_path` | `string` | No | `null` | Path to private key file. When omitted, the server auto-discovers the standard OpenSSH key names in `~/.ssh/` (`id_ed25519`, `id_ecdsa`, `id_ecdsa_sk`, `id_ed25519_sk`, `id_rsa`, `id_dsa`). |
| `name` | `string` | No | `null` | Human-readable name (e.g., `"production-db"`). Rendered in `ssh_list_sessions` / `SUGGESTED` responses. |
| `persistent` | `bool` | No | `false` | When `true`, disables inactivity timeout. Keepalive (30s / max 3) is still emitted. |
| `timeout_secs` | `u64` | No | `30` | Connection timeout in seconds. Falls back to `SSH_CONNECT_TIMEOUT`. |
| `max_retries` | `u32` | No | `3` | Max retry attempts for transient failures. Falls back to `SSH_MAX_RETRIES`. |
| `retry_delay_ms` | `u64` | No | `1000` | Initial delay between retries (ms). Exponential backoff capped at 10s with jitter. Falls back to `SSH_RETRY_DELAY_MS`. |
| `compress` | `bool` | No | `true` | Enable zlib compression. Falls back to `SSH_COMPRESSION`. |
| `session_id` | `string` | No | `null` | Reuse an existing session ID. If still connected and healthy, returns `SSH_CONNECT: REUSED` without opening a new connection. |
| `agent_id` | `string` | No | `null` | Agent identifier for grouping sessions. Enables bulk cleanup via `ssh_disconnect_agent`. |
| `reuse` | `string` | No | `"suggest"` | Reuse policy when existing sessions match the identity triple `(host, port, username)`. One of `"suggest"`, `"auto"`, `"force_new"`. See below. |

#### Smart Session Reuse

When called without a `session_id`, `ssh_connect` looks up sessions matching the `(host, port, username)` identity triple, runs a 5-second `echo 1` health check on each match, disconnects the ones that fail (freeing `MaxSessions` on the remote), and then applies the `reuse` policy:

| `reuse` | Behavior |
|---------|----------|
| `"suggest"` (default) | Returns `SSH_CONNECT: SUGGESTED` listing healthy matches. No new connection is opened. |
| `"auto"` | Returns `SSH_CONNECT: REUSED` for the most recent healthy match. No new connection is opened. |
| `"force_new"` | Skips the identity lookup entirely and always opens a new connection. |

In every mode, unhealthy matches are disconnected before the new connection is created and reported via the `REPLACED: N` field on the resulting `OK` response.

#### Authentication Chain

The `AuthChain` tries these strategies in order, stopping at the first that succeeds:

1. **Password** — added when `password` is provided.
2. **Key file** — added for each key path (`key_path` if set; otherwise all default OpenSSH keys found in `~/.ssh/`).
3. **SSH agent** — always appended as the final fallback. Accessed via the `SSH_AUTH_SOCK` environment variable. All identities are tried in sequence.

For RSA keys (both files and agent identities), the client calls `best_supported_rsa_hash()` and wraps the key with `PrivateKeyWithHashAlg`, selecting `rsa-sha2-512` or `rsa-sha2-256` when available (legacy `ssh-rsa` with SHA-1 is avoided).

#### Retry Behavior

Retry logic with exponential backoff applies **only to transient connection errors**. Authentication failures are **never retried** to avoid account lockouts.

**Retryable** (will retry up to `max_retries` times): connection refused, connection reset, connection/timed out, network unreachable, no route to host, host is down, temporary failure, resource temporarily unavailable, handshake failed, failed to connect, broken pipe, would block.

**Non-retryable** (fail immediately): authentication failed, password/key/agent authentication failed, permission denied, publickey, auth fail, no authentication, all authentication methods failed.

#### Response — `OK` (new connection)

```
SSH_CONNECT: OK
SESSION_ID: 550e8400-e29b-41d4-a716-446655440000
HOST: user@192.168.1.1:22
RETRY: 0
PERSISTENT: false
```

Additional fields:

- `AGENT: <agent_id>` — present when `agent_id` was provided.
- `REPLACED: <n>` — present when at least one unhealthy matching session was purged before creating the new one.

With `persistent=true`:

```
SSH_CONNECT: OK
SESSION_ID: ...
HOST: user@192.168.1.1:22
AGENT: my-agent
RETRY: 0
PERSISTENT: true
```

#### Response — `REUSED`

```
SSH_CONNECT: REUSED
SESSION_ID: 550e8400-e29b-41d4-a716-446655440000
HOST: user@192.168.1.1:22
AGENT: my-agent
```

`RETRY`/`PERSISTENT` are omitted — those were set on the original connection.

#### Response — `SUGGESTED` (single match)

```
SSH_CONNECT: SUGGESTED
EXISTING_SESSION_ID: 550e8400-...
HOST: user@192.168.1.1:22
AGENT: my-agent
NAME: prod-db
CONNECTED_AT: 2026-04-18T10:30:00Z
HEALTHY: true
HINT: use existing SESSION_ID, or retry with reuse="force_new"
```

#### Response — `SUGGESTED` (multiple matches)

```
SSH_CONNECT: SUGGESTED
MATCHES: 2
- 550e8400-... user@host:22 [agent: my-agent, name: prod-db, connected: 10:30:00, healthy]
- 6ba7b810-... user@host:22 [connected: 09:15:00, healthy]
HINT: pick an existing SESSION_ID, or retry with reuse="force_new"
```

#### Response — `ERROR`

```
SSH_CONNECT: ERROR
REASON: [CONNECTION_FAILED] SSH connection failed after 4 attempt(s). Last error: Connection refused
```

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

With key file and override:

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

Smart reuse:

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "db.example.com:22",
    "username": "admin",
    "reuse": "auto"
  }
}
```

Persistent named session:

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

### ssh_execute

**ACTION:** Starts a command in background and returns a `COMMAND_ID`.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect`.
- **SAVE the `COMMAND_ID`** — needed for `ssh_get_command_output` / `ssh_cancel_command`.
- **USE for long-running commands** (builds, deployments, data processing).
- **RUN MULTIPLE in parallel** on the same session — each call gets a unique `COMMAND_ID`. Channels are serialized on the server side through a per-session semaphore, which keeps bursts friendly to OpenSSH's `MaxSessions` budget.
- **USE `pty=true`** for commands that need a terminal (e.g. `sudo`, `top`). In PTY mode everything the program prints goes to stdout — there is no separate stderr.

Returns immediately with a `COMMAND_ID` for polling or cancellation. Use `ssh_get_command_output` to retrieve results.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | — | Session ID returned from `ssh_connect`. |
| `command` | `string` | Yes | — | Shell command to execute. |
| `timeout_secs` | `u64` | No | `180` | Maximum execution time. Falls back to `SSH_COMMAND_TIMEOUT`. |
| `pty` | `bool` | No | `false` | Allocate a pseudo-terminal before running the command. Required for interactive tools. |

#### Response — `STARTED`

```
SSH_EXECUTE: STARTED
COMMAND_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
SESSION_ID: 550e8400-e29b-41d4-a716-446655440000
```

With `agent_id`:

```
SSH_EXECUTE: STARTED
COMMAND_ID: ...
SESSION_ID: ...
AGENT: my-agent
```

The command text is **not** echoed in the response; it is kept in the storage for `ssh_list_commands`.

#### Response — `ERROR`

```
SSH_EXECUTE: ERROR
REASON: [MAX_COMMANDS_EXCEEDED] maximum running async commands per session reached
DETAIL: limit=100
```

> [!note]
> **Limits:**
> - Maximum **100 running** async commands per session (cap applies to running commands only; completed commands do not consume the slot).
> - Each session serializes russh channel opens through a 1-permit semaphore — parallel executes queue instead of racing `MaxSessions`.
> - Default timeout: 180 s (configurable via `timeout_secs` or `SSH_COMMAND_TIMEOUT`).
> - Per-command stdout/stderr is bounded by `SSH_COMMAND_MAX_BUFFER_SIZE` (default 10 MiB) — oldest bytes are drained head-first.

#### Example Usage

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

With PTY (for `sudo`, `top`, etc.):

```json
{
  "tool": "ssh_execute",
  "arguments": {
    "session_id": "550e8400-...",
    "command": "sudo -S systemctl restart nginx",
    "pty": true
  }
}
```

### ssh_get_command_output

**ACTION:** Retrieves output and status of a background command.

**LLM GUIDANCE:**
- **REQUIRES `command_id`** from `ssh_execute`.
- **USE `wait=false`** to poll immediately (non-blocking progress check).
- **USE `wait=true`** to block until complete or `wait_timeout_secs` expires.
- **STATUS LINE** is one of `RUNNING`, `COMPLETED`, `TIMEOUT`; the `EXIT` field is only present on `COMPLETED`.
- **USE `max_output_bytes`** to cap how much tail output is returned.
- **OUTPUT IS REMOVED FROM STORAGE** after the first read. Once output is consumed, the command entry is cleaned up after a 1 s grace window. Reading twice is safe (the second call still returns the buffered content) as long as the call arrives before cleanup completes.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `command_id` | `string` | Yes | — | Command ID returned from `ssh_execute`. |
| `wait` | `bool` | No | `false` | If `true`, block until command completes or `wait_timeout_secs` is reached. |
| `wait_timeout_secs` | `u64` | No | `30` | Max seconds to wait when `wait=true`. Capped at 300. |
| `max_output_bytes` | `usize` | No | `16384` | Max bytes per output block (stdout, stderr). Hard cap 1 MiB. Content is truncated **head-side** so the tail (most recent output) is preserved. |

#### Response — `RUNNING`

```
SSH_GET_COMMAND_OUTPUT: RUNNING
COMMAND_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
--- stdout [c7e1f4a2] (partial) ---
Installing dependencies...
--- stderr [c7e1f4a2] (partial, empty) ---
```

#### Response — `COMPLETED`

```
SSH_GET_COMMAND_OUTPUT: COMPLETED
COMMAND_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
EXIT: 0
--- stdout [c7e1f4a2] ---
Build successful!
Output written to dist/
--- stderr [c7e1f4a2] (empty) ---
```

On timeout the status is `TIMEOUT`, `EXIT` is omitted, and the blocks are marked `(partial)`:

```
SSH_GET_COMMAND_OUTPUT: TIMEOUT
COMMAND_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
--- stdout [c7e1f4a2] (partial) ---
Partial output before timeout...
--- stderr [c7e1f4a2] (partial, empty) ---
```

When the command transitioned to `failed`, the tool returns a standardized error:

```
SSH_GET_COMMAND_OUTPUT: ERROR
REASON: [COMMAND_FAILED] Failed to open channel: session disconnected
```

When the command itself exited non-zero, that is not an error — the response is `COMPLETED` with `EXIT: <code>`.

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

Wait for completion with a larger output budget:

```json
{
  "tool": "ssh_get_command_output",
  "arguments": {
    "command_id": "a1b2c3d4-...",
    "wait": true,
    "wait_timeout_secs": 120,
    "max_output_bytes": 65536
  }
}
```

### ssh_list_commands

**ACTION:** Lists async commands, optionally filtered by session and/or status.

**LLM GUIDANCE:**
- **USE to recover lost `command_id`s** across sessions.
- **FILTER by `session_id` / `status`** to narrow the list.
- **RETURNS metadata only** (not output — use `ssh_get_command_output`).

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | No | `null` | Filter by session. |
| `status` | `string` | No | `null` | One of `"running"`, `"completed"`, `"cancelled"`, `"failed"`. Invalid values are ignored. |
| `max_items` | `usize` | No | `500` | Max number of items to return (hard cap 10 000). |

#### Response — `OK`

Empty list (inline):

```
SSH_LIST_COMMANDS: OK | COUNT: 0
```

Populated list:

```
SSH_LIST_COMMANDS: OK
COUNT: 2
- a1b2c3d4-... [RUNNING] 550e8400-...: cd /app && npm run build (14:30:00)
- b2c3d4e5-... [COMPLETED] 550e8400-...: cd /app && npm test (14:30:05)
```

When the total exceeds the returned page, a pagination marker is appended to the header line:

```
SSH_LIST_COMMANDS: OK
COUNT: 2 (showing 2 of 50)
...
```

Command text is sanitized — `\n`, `\r`, and `\t` are escaped to their literal two-character forms.

### ssh_cancel_command

**ACTION:** Stops a running command and returns partial output collected so far.

**LLM GUIDANCE:**
- **REQUIRES `command_id`** from `ssh_execute`.
- **USE to stop** commands that are no longer needed.
- **NOOP when the command is not running** — returns `SSH_CANCEL_COMMAND: NOOP` so you can call it safely without pre-checking.
- **WAITS up to 5 s** for the background task to confirm the russh channel is closed (protects subsequent executes from hitting `MaxSessions`), plus a 100 ms post-drain pause.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `command_id` | `string` | Yes | — | Command ID to cancel. |
| `max_output_bytes` | `usize` | No | `16384` | Max bytes per output block. Hard cap 1 MiB. |

#### Response — `CANCELLED`

```
SSH_CANCEL_COMMAND: CANCELLED
COMMAND_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
--- stdout [c7e1f4a2] (partial) ---
Partial output before cancellation...
--- stderr [c7e1f4a2] (partial, empty) ---
```

#### Response — `NOOP`

```
SSH_CANCEL_COMMAND: NOOP | COMMAND_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890 | REASON: not running
```

#### Response — `ERROR`

```
SSH_CANCEL_COMMAND: ERROR
REASON: [COMMAND_NOT_FOUND] no async command with the given ID
DETAIL: a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

#### Example Usage

```json
{
  "tool": "ssh_cancel_command",
  "arguments": {
    "command_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
  }
}
```

### ssh_forward

**ACTION:** Creates a local port forwarding tunnel through SSH (feature-gated: `port_forward`).

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect`.
- **LOCAL PORT** binds on `127.0.0.1` — connect your tools to `localhost:<local_port>`.
- **REMOTE ADDRESS** is resolved from the SSH server's perspective (often `localhost` for services on the same host).

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | — | Session ID returned from `ssh_connect`. |
| `local_port` | `u16` | Yes | — | Local port to listen on (e.g., `8080`). |
| `remote_address` | `string` | Yes | — | Remote host to forward to (e.g., `localhost` or `10.0.0.1`). |
| `remote_port` | `u16` | Yes | — | Remote port to forward to (e.g., `3306` for MySQL). |

#### Response — `OK`

```
SSH_FORWARD: OK | LOCAL: 127.0.0.1:8080 | REMOTE: localhost:3306 | ACTIVE: true
```

#### Response — `ERROR`

When built without `--features port_forward`:

```
SSH_FORWARD: ERROR
REASON: [FEATURE_DISABLED] port forwarding feature is not enabled
DETAIL: rebuild with --features port_forward
```

When the local port cannot be bound:

```
SSH_FORWARD: ERROR
REASON: [FORWARD_FAILED] Failed to bind to local port 8080: Address already in use
```

#### Use Cases

| Scenario | Local Port | Remote Address | Remote Port |
|----------|------------|----------------|-------------|
| MySQL tunnel | 3307 | localhost | 3306 |
| Redis tunnel | 6380 | localhost | 6379 |
| Internal API | 8080 | api.internal | 80 |
| PostgreSQL | 5433 | db-primary | 5432 |

#### Example Usage

```json
{
  "tool": "ssh_forward",
  "arguments": {
    "session_id": "550e8400-...",
    "local_port": 3307,
    "remote_address": "localhost",
    "remote_port": 3306
  }
}
```

### ssh_disconnect

**ACTION:** Closes a single SSH session and releases all resources (commands, shells, transfers).

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect`.
- **ALWAYS CALL when done** to free resources.
- **AUTOMATICALLY CANCELS** all running commands, closes shells, and cancels transfers for that session.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | — | Session ID to disconnect. |

#### Response — `OK`

```
SSH_DISCONNECT: OK | SESSION_ID: 550e8400-e29b-41d4-a716-446655440000
```

#### Response — `ERROR`

```
SSH_DISCONNECT: ERROR
REASON: [SESSION_NOT_FOUND] no active SSH session with the given ID
DETAIL: 550e8400-e29b-41d4-a716-446655440000
```

### ssh_list_sessions

**ACTION:** Lists all active SSH sessions, healing dead entries as a side effect.

**LLM GUIDANCE:**
- **USE to discover `session_id`s** and their health.
- **FILTER by `agent_id`** to only see your own sessions when multiple agents share the server.
- **DEAD SESSIONS ARE REMOVED** automatically — this tool double-duties as a health sweep (runs a 5 s `echo 1` on each session).

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `agent_id` | `string` | No | `null` | Filter by agent. |
| `max_items` | `usize` | No | `500` | Max sessions to return (hard cap 10 000). |

#### Response — `OK`

Empty (inline):

```
SSH_LIST_SESSIONS: OK | COUNT: 0
```

Populated:

```
SSH_LIST_SESSIONS: OK
COUNT: 2
- 550e8400-... admin@192.168.1.100:22 [agent: my-agent, name: production-db, healthy]
- 6ba7b810-... deploy@server.example.com:22 [compression: off, healthy]
```

Tags appear in the square brackets only when meaningful (agent, name, compression off, health result).

When paginated:

```
SSH_LIST_SESSIONS: OK
COUNT: 10 (showing 10 of 42)
...
```

### ssh_disconnect_agent

**ACTION:** Disconnects **all** sessions belonging to a specific agent in one call.

**LLM GUIDANCE:**
- **REQUIRES `agent_id`** you provided to `ssh_connect`.
- **USE for bulk cleanup** when finishing work.
- **AUTOMATICALLY CANCELS** running commands, transfers, and shells across all disconnected sessions.
- **BEST PRACTICE:** Always set `agent_id` when creating sessions, then call this once on shutdown.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `agent_id` | `string` | Yes | — | Agent identifier used when creating sessions. |

#### Response — `OK`

```
SSH_DISCONNECT_AGENT: OK | AGENT: my-unique-agent-id | SESSIONS: 3 | COMMANDS: 5
```

When no sessions match:

```
SSH_DISCONNECT_AGENT: OK | AGENT: my-unique-agent-id | SESSIONS: 0 | COMMANDS: 0
```

### ssh_shell_open

**ACTION:** Opens an interactive PTY shell session and returns a `SHELL_ID`.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect`.
- **SAVE the `SHELL_ID`** — needed for `ssh_shell_write`, `ssh_shell_read`, `ssh_shell_close`.
- **USE for interactive sessions** (SOL/IPMI/OOB consoles, serial devices, commands requiring a PTY such as `sudo` or `top`).
- **USE `term="vt100"`** for Serial Over LAN / IPMI / OOB access.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | — | Session ID returned from `ssh_connect`. |
| `term` | `string` | No | `"xterm"` | Terminal type. Use `"vt100"` or `"ansi"` for SOL/IPMI/serial consoles. |
| `cols` | `u32` | No | `80` | Terminal width. |
| `rows` | `u32` | No | `24` | Terminal height. |
| `inactivity_ttl` | `u64` | No | `600` | Seconds before the shell auto-closes if no read/write happens. Falls back to `SSH_SHELL_INACTIVITY_TTL`. |
| `max_buffer_size` | `string` | No | `"10m"` | Output buffer cap. Accepts human-readable sizes (`"512k"`, `"10m"`, `"1g"`, `"2tb"`, or plain bytes). Falls back to `SSH_SHELL_MAX_BUFFER_SIZE`. When exceeded, oldest bytes are trimmed head-first. |

#### Response — `OK`

```
SSH_SHELL_OPEN: OK
SHELL_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
SESSION_ID: 550e8400-e29b-41d4-a716-446655440000
TERM: xterm 80x24
```

With `agent_id`:

```
SSH_SHELL_OPEN: OK
SHELL_ID: ...
SESSION_ID: ...
TERM: vt100 80x24
AGENT: my-agent
```

> [!note]
> **Limits:**
> - Maximum 10 concurrent shells per session.
> - Idle shells auto-close after `inactivity_ttl`.
> - Shell output buffer is capped at `max_buffer_size`; oldest bytes are discarded on overflow.
> - Shells are closed automatically when the session disconnects.

### ssh_shell_write

**ACTION:** Sends raw input to an interactive shell.

**LLM GUIDANCE:**
- **REQUIRES `shell_id`** from `ssh_shell_open`.
- **SEND text with newlines** to execute commands (e.g., `"ls -la\n"`).
- **SEND control chars** for special keys (`"\u0003"` for Ctrl+C, `"\u0004"` for Ctrl+D).
- **RESETS the inactivity timer** on every successful write.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `shell_id` | `string` | Yes | — | Shell ID returned from `ssh_shell_open`. |
| `input` | `string` | Yes | — | Input to send (text, control chars, escape sequences). |

#### Response — `OK`

```
SSH_SHELL_WRITE: OK | SHELL_ID: a1b2c3d4-... | BYTES_SENT: 7
```

#### Example Usage

Execute a command:

```json
{
  "tool": "ssh_shell_write",
  "arguments": {
    "shell_id": "a1b2c3d4-...",
    "input": "ls -la\n"
  }
}
```

Send Ctrl+C:

```json
{
  "tool": "ssh_shell_write",
  "arguments": {
    "shell_id": "a1b2c3d4-...",
    "input": "\u0003"
  }
}
```

### ssh_shell_read

**ACTION:** Reads accumulated output from an interactive shell.

**LLM GUIDANCE:**
- **REQUIRES `shell_id`** from `ssh_shell_open`.
- **STATUS LINE** is `OPEN` while the shell is running and `CLOSED` once the background reader has ended.
- **WITH `clear=true` (default)** the tool drains only the bytes it actually returned (head-based pagination) — the rest stays available for the next call. This prevents losing output when the buffer is larger than `max_output_bytes`.
- **WITH `clear=false`** the buffer is not modified ("peek" mode).
- **RESETS the inactivity timer** on every successful read.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `shell_id` | `string` | Yes | — | Shell ID returned from `ssh_shell_open`. |
| `clear` | `bool` | No | `true` | Drain the bytes shown by this response. |
| `max_output_bytes` | `usize` | No | `16384` | Max bytes rendered. Hard cap 1 MiB. Renders the tail (most recent output). |

#### Response — `OPEN`

```
SSH_SHELL_READ: OPEN
SHELL_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
--- data [c7e1f4a2] ---
total 42
drwxr-xr-x  5 user group 160 Jan 15 10:30 .
drwxr-xr-x 12 user group 384 Jan 14 09:00 ..
```

#### Response — `CLOSED`

```
SSH_SHELL_READ: CLOSED
SHELL_ID: a1b2c3d4-...
--- data [c7e1f4a2] (empty) ---
```

### ssh_shell_close

**ACTION:** Closes an interactive shell session and releases resources.

**LLM GUIDANCE:**
- **REQUIRES `shell_id`** from `ssh_shell_open`.
- **CALL when done** with the interactive session (or let the inactivity TTL / `ssh_disconnect` handle it).

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `shell_id` | `string` | Yes | — | Shell ID to close. |

#### Response — `OK`

```
SSH_SHELL_CLOSE: OK | SHELL_ID: a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

### ssh_upload

**ACTION:** Uploads a local file to a remote path via SFTP. Returns a `TRANSFER_ID` for progress polling.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect`.
- **SAVE the `TRANSFER_ID`** — needed for `ssh_get_transfer_progress`.
- **Relative / `~` local paths** are resolved against the home directory on the MCP server's host.
- **THE LOCAL PATH MUST BE A REGULAR FILE** — directories, symlinks to non-files, and devices return `LOCAL_NOT_FILE`.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | — | Session ID returned from `ssh_connect`. |
| `local_path` | `string` | Yes | — | Local file path (`~` and relative paths resolve to home). |
| `remote_path` | `string` | Yes | — | Destination path on the remote server. |

#### Response — `STARTED`

```
SSH_UPLOAD: STARTED
TRANSFER_ID: xfer-abc
SESSION_ID: 550e8400-...
FROM: /Users/me/Downloads/backup.tar.gz
TO: /var/backups/backup.tar.gz
SIZE: 245.7MB (257632256 bytes)
BYTES: 257632256
```

With `agent_id`:

```
SSH_UPLOAD: STARTED
TRANSFER_ID: xfer-abc
SESSION_ID: 550e8400-...
AGENT: my-agent
FROM: /Users/me/...
TO: /var/backups/...
SIZE: ...
BYTES: ...
```

#### Response — `ERROR`

```
SSH_UPLOAD: ERROR
REASON: [LOCAL_FILE_ERROR] [FILE_NOT_FOUND] access local file '/tmp/missing.bin': file does not exist (raw: No such file or directory (os error 2))
```

> [!note]
> **Limits:**
> - Maximum 10 concurrent transfers per session.
> - Streamed in 32 KiB chunks (`CHUNK_SIZE = 32 * 1024`).
> - Terminated transfers stay in storage for `SSH_TRANSFER_CLEANUP_TTL` (default 300 s) so the LLM can poll the final state before it disappears.

### ssh_download

**ACTION:** Downloads a remote file to a local path via SFTP. Returns a `TRANSFER_ID`.

**LLM GUIDANCE:**
- **REQUIRES `session_id`** from `ssh_connect`.
- **SAVE the `TRANSFER_ID`** — needed for `ssh_get_transfer_progress`.
- **Relative / `~` local paths** resolve against the home directory on the MCP server's host.
- **THE TOOL PRE-FETCHES REMOTE METADATA** so the response can include the total `SIZE`; missing remote files return `REMOTE_METADATA_ERROR`.

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `session_id` | `string` | Yes | — | Session ID returned from `ssh_connect`. |
| `remote_path` | `string` | Yes | — | Remote file path. |
| `local_path` | `string` | Yes | — | Local destination path (`~` and relative paths resolve to home). |

#### Response — `STARTED`

```
SSH_DOWNLOAD: STARTED
TRANSFER_ID: xfer-def
SESSION_ID: 550e8400-...
FROM: /var/log/app.log
TO: /Users/me/logs/app.log
SIZE: 3.2MB (3348429 bytes)
BYTES: 3348429
```

Note that for `SSH_DOWNLOAD`, `FROM` is the remote path and `TO` is the resolved local path.

### ssh_get_transfer_progress

**ACTION:** Retrieves progress and status of an SFTP transfer.

**LLM GUIDANCE:**
- **REQUIRES `transfer_id`** from `ssh_upload`/`ssh_download`.
- **USE `wait=true`** to block until the transfer reaches a terminal state.
- **PROGRESS** is reported as `<percent>% (<transferred>/<total> bytes)`. `percent` is 0 when `total` is unknown (0 bytes pre-discovered).

#### Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `transfer_id` | `string` | Yes | — | Transfer ID returned from upload/download. |
| `wait` | `bool` | No | `false` | Block until complete or `wait_timeout_secs` expires. |
| `wait_timeout_secs` | `u64` | No | `30` | Max seconds to wait. Capped at 300. |

#### Response — `RUNNING` / `COMPLETED` (inline)

```
SSH_GET_TRANSFER_PROGRESS: RUNNING | TRANSFER_ID: xfer-abc | UPLOAD 42% (108200100/257632256 bytes)
```

```
SSH_GET_TRANSFER_PROGRESS: COMPLETED | TRANSFER_ID: xfer-abc | UPLOAD 100% (257632256/257632256 bytes)
```

#### Response — `FAILED` (block)

```
SSH_GET_TRANSFER_PROGRESS: FAILED
TRANSFER_ID: xfer-abc
DIRECTION: UPLOAD
PROGRESS: 10% (25763225/257632256 bytes)
REASON: [CONNECTION_LOST] ...
```

A cancelled transfer also maps to `FAILED` with `REASON: transfer cancelled`.

#### Response — `ERROR`

```
SSH_GET_TRANSFER_PROGRESS: ERROR
REASON: [TRANSFER_NOT_FOUND] no transfer with the given ID
DETAIL: xfer-abc
```

## Examples

### Complete Workflow

<details>
<summary>Complete workflow example</summary>

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
```
SSH_CONNECT: OK
SESSION_ID: abc-123-def-456
HOST: deploy@prod-server.example.com:22
RETRY: 0
PERSISTENT: false
```

2. **Execute deployment command**

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
```
SSH_EXECUTE: STARTED
COMMAND_ID: cmd-789-xyz
SESSION_ID: abc-123-def-456
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
```
SSH_GET_COMMAND_OUTPUT: COMPLETED
COMMAND_ID: cmd-789-xyz
EXIT: 0
--- stdout [nonce] ---
Already up to date.
--- stderr [nonce] (empty) ---
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
```
SSH_FORWARD: OK | LOCAL: 127.0.0.1:5433 | REMOTE: db.internal:5432 | ACTIVE: true
```

5. **Check active sessions**

```json
{
  "tool": "ssh_list_sessions",
  "arguments": {}
}
```

Response:
```
SSH_LIST_SESSIONS: OK
COUNT: 1
- abc-123-def-456 deploy@prod-server.example.com:22 [healthy]
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
SSH_DISCONNECT: OK | SESSION_ID: abc-123-def-456
```

</details>

### Async Command Workflow (parallel build + test)

<details>
<summary>Parallel build + test</summary>

1. **Connect**

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "build-server.example.com:22",
    "username": "ci",
    "name": "build-pipeline",
    "agent_id": "ci-bot-123"
  }
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

Both return `SSH_EXECUTE: STARTED` with their own `COMMAND_ID`s.

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
```
SSH_LIST_COMMANDS: OK
COUNT: 2
- build-cmd-456 [RUNNING] build-session-123: cd /app && npm run build (14:30:00)
- test-cmd-789  [RUNNING] build-session-123: cd /app && npm test (14:30:01)
```

4. **Wait for each to finish**

```json
{ "tool": "ssh_get_command_output", "arguments": { "command_id": "build-cmd-456", "wait": true, "wait_timeout_secs": 120 } }
```

```json
{ "tool": "ssh_get_command_output", "arguments": { "command_id": "test-cmd-789",  "wait": true, "wait_timeout_secs": 60  } }
```

5. **Bulk cleanup when done**

```json
{ "tool": "ssh_disconnect_agent", "arguments": { "agent_id": "ci-bot-123" } }
```

Response:
```
SSH_DISCONNECT_AGENT: OK | AGENT: ci-bot-123 | SESSIONS: 1 | COMMANDS: 0
```

</details>

### Cancelling a Long-Running Command

<details>
<summary>Cancelling a command</summary>

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
```
SSH_EXECUTE: STARTED
COMMAND_ID: search-cmd-111
SESSION_ID: abc-123-def-456
```

2. **Check progress**

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
```
SSH_GET_COMMAND_OUTPUT: RUNNING
COMMAND_ID: search-cmd-111
--- stdout [nonce] (partial) ---
/var/log/syslog
/var/log/auth.log
--- stderr [nonce] (partial, empty) ---
```

3. **Cancel**

```json
{
  "tool": "ssh_cancel_command",
  "arguments": {
    "command_id": "search-cmd-111"
  }
}
```

Response:
```
SSH_CANCEL_COMMAND: CANCELLED
COMMAND_ID: search-cmd-111
--- stdout [nonce] (partial) ---
/var/log/syslog
/var/log/auth.log
/var/log/kern.log
--- stderr [nonce] (partial, empty) ---
```

</details>

### SFTP Upload with Progress Polling

<details>
<summary>SFTP upload</summary>

```json
{
  "tool": "ssh_upload",
  "arguments": {
    "session_id": "abc-123",
    "local_path": "~/Downloads/artifact.tar.gz",
    "remote_path": "/srv/releases/artifact.tar.gz"
  }
}
```

Response:
```
SSH_UPLOAD: STARTED
TRANSFER_ID: xfer-up-1
SESSION_ID: abc-123
FROM: /Users/me/Downloads/artifact.tar.gz
TO: /srv/releases/artifact.tar.gz
SIZE: 128.0MB (134217728 bytes)
BYTES: 134217728
```

Poll:
```json
{
  "tool": "ssh_get_transfer_progress",
  "arguments": {
    "transfer_id": "xfer-up-1",
    "wait": true,
    "wait_timeout_secs": 120
  }
}
```

Response while running:
```
SSH_GET_TRANSFER_PROGRESS: RUNNING | TRANSFER_ID: xfer-up-1 | UPLOAD 57% (76824576/134217728 bytes)
```

Response after completion:
```
SSH_GET_TRANSFER_PROGRESS: COMPLETED | TRANSFER_ID: xfer-up-1 | UPLOAD 100% (134217728/134217728 bytes)
```

</details>

## Client-side Parser

Every integration-test script (`scripts/test_http.py`, `scripts/test_stdio.py`) ships a reference `parse_mcp_response(text)` function that reads the markdown response back into a Python dict. Clients that prefer structured access can copy that helper verbatim — it already handles:

- `TOOL: STATUS [| key: v | ...]` header lines.
- `KEY: VALUE` body fields.
- `- item` bullet lists for `ssh_list_sessions` / `ssh_list_commands`.
- `--- stdout|stderr|data [nonce] (...) ---` output blocks and their annotations.
- `REASON: [CODE] ...` / `DETAIL: ...` error lines.

## Important Notes

### Authentication Priority

Authentication methods are tried in strict order and stop at the first that succeeds:

1. **Password authentication** — when the `password` parameter is provided.
2. **Explicit key file** — when `key_path` is provided.
3. **Default `~/.ssh/` keys** — auto-discovered when `key_path` is not set (tries `id_ed25519`, `id_ecdsa`, `id_ecdsa_sk`, `id_ed25519_sk`, `id_rsa`, `id_dsa`).
4. **SSH agent** — always appended as the final fallback.

For RSA keys the hash algorithm is negotiated via `best_supported_rsa_hash()` so `rsa-sha2-256` / `rsa-sha2-512` are used when available and the legacy `ssh-rsa` / SHA-1 is avoided.

### Retry Logic

Retries use `backon::ExponentialBuilder` with jitter:

| Parameter | Default | Description |
|-----------|---------|-------------|
| Initial delay | 1000 ms | Configurable via `retry_delay_ms` / `SSH_RETRY_DELAY_MS`. |
| Maximum delay | 10 s | Hard cap (`MAX_RETRY_DELAY`). |
| Jitter | Enabled | Random jitter is added to prevent thundering herd. |
| Maximum retries | 3 | Configurable via `max_retries` / `SSH_MAX_RETRIES`. |

**Auth failures are never retried.**

### Configuration Priority

```
Parameter > Environment Variable > Default
```

| Setting | Parameter | Environment Variable | Default |
|---------|-----------|----------------------|---------|
| Connection timeout | `timeout_secs` | `SSH_CONNECT_TIMEOUT` | 30 s |
| Command timeout | `timeout_secs` | `SSH_COMMAND_TIMEOUT` | 180 s |
| Max retries | `max_retries` | `SSH_MAX_RETRIES` | 3 |
| Retry delay | `retry_delay_ms` | `SSH_RETRY_DELAY_MS` | 1000 ms |
| Compression | `compress` | `SSH_COMPRESSION` | true |
| Inactivity timeout | — | `SSH_INACTIVITY_TIMEOUT` | 300 s |
| Command cleanup TTL | — | `SSH_COMMAND_CLEANUP_TTL` | 60 s |
| Command buffer cap | — | `SSH_COMMAND_MAX_BUFFER_SIZE` | 10 MiB |
| Shell inactivity TTL | `inactivity_ttl` | `SSH_SHELL_INACTIVITY_TTL` | 600 s |
| Shell buffer cap | `max_buffer_size` | `SSH_SHELL_MAX_BUFFER_SIZE` | 10 MiB |
| Transfer cleanup TTL | — | `SSH_TRANSFER_CLEANUP_TTL` | 300 s |
| Output bytes default | `max_output_bytes` | `SSH_MCP_OUTPUT_DEFAULT_BYTES` | 16 KiB |
| Output bytes cap | — | `SSH_MCP_OUTPUT_MAX_BYTES_CAP` | 1 MiB |
| List items default | `max_items` | `SSH_MCP_LIST_MAX_ITEMS` | 500 |
| List items cap | — | `SSH_MCP_LIST_MAX_ITEMS_CAP` | 10 000 |

### Limits Cheat Sheet

| Resource | Limit | Source |
|----------|-------|--------|
| Concurrent running commands per session | 100 | `MAX_ASYNC_COMMANDS_PER_SESSION` |
| Concurrent shells per session | 10 | `MAX_SHELLS_PER_SESSION` |
| Concurrent transfers per session | 10 | `MAX_TRANSFERS_PER_SESSION` |
| Concurrent russh channels per session | 1 | `CHANNEL_CONCURRENCY_PER_SESSION` (serialized via semaphore) |
| SFTP streaming chunk | 32 KiB | `CHUNK_SIZE` |
| `max_output_bytes` default / cap | 16 KiB / 1 MiB | `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP` |
| `max_items` default / cap | 500 / 10 000 | `SSH_MCP_LIST_MAX_ITEMS` / `SSH_MCP_LIST_MAX_ITEMS_CAP` |
| Error `DETAIL` truncation | 2 KiB | `DEFAULT_ERROR_DETAIL_MAX_BYTES` |
| Nonce entropy | 32 bits (8 hex chars) | `NONCE_LEN` |
