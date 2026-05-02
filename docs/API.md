# SSH MCP API Reference (v3.0.0)

Complete API reference for the 18 MCP tools and the 5 resource subscribe schemes exposed by the v3.0.0 ssh-mcp server (rmcp 1.6, protocol `V_2025_06_18`).

[[_TOC_]]

## Conventions

- **Response format**: every tool returns a single markdown `Text<String>`. The format is **block-only** — one `KEY: value` per line. There is no inline `KEY: v | KEY: v` form in v3.
- **Status case**: `SCREAMING_SNAKE_CASE` (`OK`, `RUNNING`, `MATCHED`, `TIMEOUT`, `CANCELLED`, `NOOP`, …).
- **Filter enum case**: `snake_case` for input enums (`reuse: "suggest" | "auto" | "force_new"`, `status: "running" | "completed" | "cancelled" | "failed"`).
- **Identifiers**: `*_ID` suffix in uppercase (`SESSION_ID`, `COMMAND_ID`, `SHELL_ID`, `TRANSFER_ID`, `AGENT_ID`).
- **Output blocks**: `--- stdout [<nonce>] ---`, `--- stderr [<nonce>] ---`, `--- data [<nonce>] ---`. The 8-hex `nonce` (regenerated per response) prevents the rendered content from forging the delimiter.
- **Errors**:
  ```
  TOOL_NAME: ERROR
  REASON: [CODE] human-readable message
  DETAIL: optional context
  ```

### Status values

| Status | Emitted by | Meaning |
|--------|-----------|---------|
| `OK` | most write/lifecycle tools (`ssh_connect`, `ssh_disconnect`, `ssh_disconnect_agent`, `ssh_list_sessions`, `ssh_list_commands`, `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_send_key`, `ssh_shell_close`, `ssh_forward`) | Success — see body for IDs. |
| `REUSED` | `ssh_connect` | Existing healthy session returned. |
| `SUGGESTED` | `ssh_connect` | Matching session(s) exist; LLM picks one or retries with `force_new`. |
| `STARTED` | `ssh_execute`, `ssh_upload`, `ssh_download` | Background work kicked off. |
| `RUNNING` | `ssh_get_command_output`, `ssh_get_transfer_progress` | Background work still in progress; output marked `(partial)`. |
| `COMPLETED` | `ssh_get_command_output`, `ssh_get_transfer_progress` | Background work finished. |
| `TIMEOUT` | `ssh_get_command_output`, `ssh_shell_read.wait`, `ssh_shell_wait_for` | Long-poll deadline expired or `wait_for` matched no pattern. |
| `FAILED` | `ssh_get_transfer_progress` | Transfer terminated with an error; `REASON:` line carries detail. |
| `CANCELLED` | `ssh_cancel_command` | Work cancelled by caller; partial output included. |
| `NOOP` | `ssh_cancel_command` | Idempotent cancel — command was not running. |
| `OPEN` / `CLOSED` | `ssh_shell_read` | Shell state during read. |
| `MATCHED` | `ssh_shell_wait_for` | Pattern hit. |
| `ERROR` | every tool that can fail | See `REASON` / `DETAIL`. |

### Output blocks

`ssh_get_command_output` and `ssh_cancel_command` emit `stdout` and `stderr` blocks. `ssh_shell_read` and `ssh_shell_wait_for` emit a single `data` block. Empty blocks render as `--- stdout [a3f2b1d7] (empty) ---`. Truncation marks the delimiter with `(partial, truncated: showing 16.0KB of 2.3MB)`. Content is UTF-8 safely truncated to the **tail** (most recent bytes) when `max_output_bytes` is exceeded.

### Capability handshake

`McpSshServer::get_info()` advertises:

```rust
ServerCapabilities::builder()
    .enable_tools()
    .enable_tool_list_changed()
    .enable_resources()
    .enable_resources_subscribe()
    .enable_resources_list_changed()
    .build()
```

Protocol version: `ProtocolVersion::V_2025_06_18`. Server name: `ssh-mcp` with version from `CARGO_PKG_VERSION`. The `instructions` field tells the LLM to prefer `resources/subscribe shell://<id>/output` over polling tools.

---

## Tools (18)

The catalogue below covers every tool: schema, defaults, response sample, status values, and error codes.

## Connection (4)

### ssh_connect

Connect to an SSH server and store the session.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string?` | — | Optional `SESSION_ID` from a previous `ssh_connect`. When provided and alive (probe `echo 1`), short-circuits to `REUSED`. |
| `address` | `string` | — | `host[:port]` (e.g. `192.168.1.1:22`, `example.com`). Port defaults to `22`. |
| `username` | `string` | — | SSH username. |
| `password` | `string?` | — | Password for password auth. Optional when `key_path` or an SSH agent is available. |
| `key_path` | `string?` | — | Path to a private key. Auth chain: key → password → agent. |
| `timeout_secs` | `u64?` | `30` | Connection timeout (env `SSH_CONNECT_TIMEOUT`). |
| `max_retries` | `u32?` | `3` | Retry attempts (env `SSH_MAX_RETRIES`). |
| `retry_delay_ms` | `u64?` | `1000` | Initial retry delay; exponential backoff capped at 10 s (env `SSH_RETRY_DELAY_MS`). |
| `compress` | `bool?` | `true` | Enable zlib compression (env `SSH_COMPRESSION`). |
| `name` | `string?` | — | Human-readable session label. |
| `persistent` | `bool?` | `false` | Disable inactivity timeout. |
| `agent_id` | `string?` | — | Group sessions under an `AGENT_ID` for bulk cleanup via `ssh_disconnect_agent`. |
| `reuse` | `"suggest" \| "auto" \| "force_new"` | `"suggest"` | Smart reuse policy. `auto` returns the most recent healthy match. `force_new` skips the lookup. `suggest` (default) lists matching sessions and stops so the LLM picks one. |

**Status values**: `OK`, `REUSED`, `SUGGESTED`, `ERROR`.

**Response — OK (new session)**:
```
SSH_CONNECT: OK
SESSION_ID: a3f2b1d7-1234-5678-9abc-def012345678
HOST: alice@example.com:22
AGENT: claude-code-instance-abc123
RETRY: 0
PERSISTENT: false
```

`HOST` always renders as `username@host:port`. `AGENT` is omitted when no `agent_id` was passed. `REPLACED: N` is appended when stale matches were purged before creating the session.

**Response — REUSED**:
```
SSH_CONNECT: REUSED
SESSION_ID: a3f2b1d7-...
HOST: alice@example.com:22
AGENT: claude-code-instance-abc123
```

`RETRY` and `PERSISTENT` are omitted on `REUSED` (the original connect already set them).

**Response — SUGGESTED (single match)**:
```
SSH_CONNECT: SUGGESTED
EXISTING_SESSION_ID: a3f2b1d7-...
HOST: alice@example.com:22
AGENT: claude
NAME: prod-db
CONNECTED_AT: 2026-05-02T18:00:00Z
HEALTHY: true
HINT: use existing SESSION_ID, or retry with reuse="force_new"
```

**Response — SUGGESTED (multi-match)**:
```
SSH_CONNECT: SUGGESTED
MATCHES: 2
- a3f2b1d7-... alice@example.com:22 [agent: claude, name: prod-db, connected: 2026-05-02T18:00:00Z, healthy]
- 9b1c2d3e-... alice@example.com:22 [agent: claude, name: prod-db-2, connected: 2026-05-02T17:50:00Z, healthy]
HINT: pick an existing SESSION_ID, or retry with reuse="force_new"
```

**Errors**: `CONNECTION_FAILED` (handshake or all retries exhausted).

---

### ssh_disconnect

Disconnect a single SSH session and release every resource it owns.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID` from `ssh_connect`. |

Workflow:
1. Cancel every running async command for the session.
2. Close every interactive shell.
3. Abort every in-flight SFTP transfer.
4. Disconnect the SSH transport (`Disconnect::ByApplication`).

**Status values**: `OK`, `ERROR`.

**Response**:
```
SSH_DISCONNECT: OK
SESSION_ID: a3f2b1d7-...
```

**Errors**: `SESSION_NOT_FOUND`.

---

### ssh_list_sessions

List active SSH sessions with health-check metadata.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `agent_id` | `string?` | — | Filter to a single `AGENT_ID`. Omit to list every agent. |
| `max_items` | `usize?` | `500` | Cap entries (env `SSH_MCP_LIST_MAX_ITEMS`, hard cap `SSH_MCP_LIST_MAX_ITEMS_CAP=10000`). |

The tool runs an `echo 1` health probe against every candidate session and removes any that fail before returning. Each successful probe also fires a `HealthEvent::Healthy` so subscribers of `session://<id>/health` see a fresh tick.

**Status values**: `OK`.

**Response**:
```
SSH_LIST_SESSIONS: OK
COUNT: 2
- a3f2b1d7-... alice@prod-db:22 [agent: claude, healthy]
- 9b1c2d3e-... alice@stage-db:22 [agent: claude, name: stage, healthy]
```

Each item is `<SESSION_ID> <username>@<host>` followed by an optional `[…]` annotation block. Annotations include any of `agent: <id>`, `name: <label>`, `compression: off`, and the health label (`healthy` / `unhealthy`). When `max_items` truncates, the COUNT line becomes `COUNT: N (showing N of M)`.

---

### ssh_disconnect_agent

Bulk-disconnect every session owned by an `AGENT_ID`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `agent_id` | `string` | — | `AGENT_ID` provided at connect time. |

Cancels commands, closes shells, aborts transfers, and disconnects each session's transport. Sessions owned by other agents are not touched.

**Response**:
```
SSH_DISCONNECT_AGENT: OK
AGENT: claude-code-instance-abc123
SESSIONS: 3
COMMANDS: 5
```

---

## Execute (4)

### ssh_execute

Execute a shell command asynchronously. Returns immediately with a `COMMAND_ID`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `command` | `string` | — | Shell command to run on the remote host. |
| `timeout_secs` | `u64?` | `180` | Command timeout (env `SSH_COMMAND_TIMEOUT`). |
| `pty` | `bool?` | `false` | Allocate a PTY for the command (e.g. `sudo`, `top`). All output merges to stdout in PTY mode. |

Limits: up to 100 concurrent multiplexed commands per session.

**Tip**: subscribe to `command://<command_id>/output` (see [Resources](#resources-5-schemes)) to observe stdout/stderr in realtime instead of polling.

**Status values**: `STARTED`, `ERROR`.

**Response**:
```
SSH_EXECUTE: STARTED
COMMAND_ID: 7d4c8e2a-...
SESSION_ID: a3f2b1d7-...
```

**Errors**: `SESSION_NOT_FOUND`, `MAX_COMMANDS_EXCEEDED`.

---

### ssh_get_command_output

Read the current output and status of an async command.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command_id` | `string` | — | `COMMAND_ID` from `ssh_execute`. |
| `wait` | `bool?` | `false` | Block until completion or timeout. |
| `wait_timeout_secs` | `u64?` | `30` | Long-poll deadline; cap `300`. |
| `max_output_bytes` | `usize?` | `16384` | Hard cap `1 048 576` (env `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`). Tail (most recent) preserved. |

**FALLBACK note**: prefer `resources/subscribe command://<command_id>/output` for realtime push; `wait=true` here is a long-poll fallback for clients that cannot subscribe.

**Status values**: `RUNNING`, `COMPLETED`, `TIMEOUT`, `ERROR`.

**Response — COMPLETED**:
```
SSH_GET_COMMAND_OUTPUT: COMPLETED
COMMAND_ID: 7d4c8e2a-...
EXIT: 0
--- stdout [a3f2b1d7] ---
total 8
drwxr-xr-x  2 alice alice 4096 May  2 18:00 src
--- stderr [a3f2b1d7] (empty) ---
```

**Errors**: `COMMAND_NOT_FOUND`, `COMMAND_FAILED`.

---

### ssh_list_commands

List async commands across one or all sessions.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string?` | — | Filter to a single session. |
| `status` | `"running" \| "completed" \| "cancelled" \| "failed"?` | — | Filter by status (typed enum — typos rejected at deserialisation). |
| `max_items` | `usize?` | `500` | Cap entries; hard cap `10 000`. |

**Response**:
```
SSH_LIST_COMMANDS: OK
COUNT: 2
- 7d4c8e2a-... [COMPLETED] a3f2b1d7-...: ls -la (18:00:00)
- 9f8e7d6c-... [RUNNING] a3f2b1d7-...: tail -f /var/log/syslog (18:01:00)
```

Each item is `<COMMAND_ID> [<STATUS>] <SESSION_ID>: <command> (HH:MM:SS)`. STATUS uses uppercase (`RUNNING`, `COMPLETED`, `CANCELLED`, `FAILED`). Trailing parenthesis carries the started-at time-of-day.

---

### ssh_cancel_command

Cancel a running async command.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command_id` | `string` | — | `COMMAND_ID`. |
| `max_output_bytes` | `usize?` | `16384` | Hard cap `1 048 576`. Returns partial stdout/stderr collected before cancellation. |

**Status values**: `CANCELLED`, `NOOP`, `ERROR`.

**Response — CANCELLED**:
```
SSH_CANCEL_COMMAND: CANCELLED
COMMAND_ID: 9f8e7d6c-...
--- stdout [b2e7c9d1] (partial) ---
last line before cancel
--- stderr [b2e7c9d1] (empty) ---
```

**Errors**: `COMMAND_NOT_FOUND`.

---

## Shell (6)

### ssh_shell_open

Open an interactive PTY shell on a session.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `term` | `string?` | `"xterm"` | Terminal type. Use `vt100` / `ansi` for SOL/IPMI/serial consoles. |
| `cols` | `u32?` | `80` | Terminal width. |
| `rows` | `u32?` | `24` | Terminal height. |
| `inactivity_ttl` | `u64?` | `600` | Auto-close after N seconds of no activity (env `SSH_SHELL_INACTIVITY_TTL`). |
| `max_buffer_size` | `string?` | `"10m"` | Output buffer cap (`b/k/m/g/t` suffixes; env `SSH_SHELL_MAX_BUFFER_SIZE`). |

Limits: up to 10 shells per session (`MAX_SHELLS_PER_SESSION`).

**FALLBACK note**: prefer `resources/subscribe shell://<shell_id>/output` for realtime PTY streams; the polling tools below are fallbacks.

**Status values**: `OK`, `ERROR`.

**Response**:
```
SSH_SHELL_OPEN: OK
SHELL_ID: 4b9c8e2a-...
SESSION_ID: a3f2b1d7-...
TERM: xterm 80x24
AGENT: claude-code-instance-abc123
```

`TERM` carries the terminal type and the geometry on a single line (`<term> <cols>x<rows>`). `AGENT` is omitted when no agent owns the session.

**Errors**: `SESSION_NOT_FOUND`, `MAX_SHELLS_EXCEEDED`, `CHANNEL_FAILED`.

---

### ssh_shell_write

Send raw bytes to an interactive shell. Use `ssh_shell_send_key` for named keystrokes whenever possible.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `shell_id` | `string` | — | `SHELL_ID`. |
| `input` | `string` | — | Bytes to send. Append `\n` to submit a typed line. Use `\x03` for Ctrl+C, `\x1b[A` for arrow up. |

**FALLBACK note**: prefer `resources/subscribe shell://<shell_id>/output` to observe the shell after writing.

**Response**:
```
SSH_SHELL_WRITE: OK
SHELL_ID: 4b9c8e2a-...
BYTES_SENT: 7
```

**Errors**: `SHELL_NOT_FOUND`, `WRITE_FAILED`.

---

### ssh_shell_send_key

Send a named keystroke to an interactive shell. Convenience wrapper over `ssh_shell_write` that maps semantic key names (e.g. `ctrl_c`, `arrow_up`, `f5`) to xterm-compatible byte sequences.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `shell_id` | `string` | — | `SHELL_ID`. |
| `key` | `ShellKey` | — | One of: `ctrl_a`, `ctrl_c`, `ctrl_d`, `ctrl_e`, `ctrl_k`, `ctrl_l`, `ctrl_r`, `ctrl_u`, `ctrl_w`, `ctrl_z`, `enter`, `tab`, `escape`, `backspace`, `space`, `delete`, `arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`, `home`, `end`, `page_up`, `page_down`, `insert`, `f1`..`f12`. |
| `shift` | `bool?` | `false` | Apply Shift modifier. |
| `alt` | `bool?` | `false` | Apply Alt modifier. |
| `ctrl` | `bool?` | `false` | Apply Ctrl modifier. |
| `repeat` | `u8?` | `1` | Repeat the keystroke N times. Range `1..=64`. |

**Modifier rules**:

| Key class | Allowed modifiers |
|-----------|--------------------|
| Arrows, navigation (`home`, `end`, `page_up`, `page_down`, `insert`, `delete`), `f1`..`f12` | Any combination of `shift`, `alt`, `ctrl`. |
| `tab` | `shift` only (produces back-tab `\x1b[Z`). |
| `ctrl_*`, `enter`, `escape`, `backspace`, `space` | None (modifiers are baked into the C0 code). |

`Backspace` always encodes as `0x7f` (modern xterm). Clients that need the legacy `0x08` form should use `ssh_shell_write` with raw bytes.

**FALLBACK note**: prefer `resources/subscribe shell://<shell_id>/output` to observe the shell after sending the key.

**Response — plain key**:
```
SSH_SHELL_SEND_KEY: OK
SHELL_ID: 4b9c8e2a-...
KEY: ctrl_c
REPEAT: 1
BYTES_SENT: 1
```

**Response — modified key**:
```
SSH_SHELL_SEND_KEY: OK
SHELL_ID: 4b9c8e2a-...
KEY: arrow_up
MODIFIERS: shift+ctrl
REPEAT: 3
BYTES_SENT: 18
```

`MODIFIERS:` is omitted when no modifier flag is set; `BYTES_SENT` is `repeat * encoded_len(key, mods)`.

**Errors**: `SHELL_NOT_FOUND`, `MODIFIER_NOT_ALLOWED`, `INVALID_REPEAT`, `WRITE_FAILED`.

---

### ssh_shell_read

Read accumulated output from an interactive shell. Snapshot mode by default; long-poll fallback via `wait=true`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `shell_id` | `string` | — | `SHELL_ID`. |
| `clear` | `bool?` | `true` | Drain the rendered bytes (head pagination). `false` keeps the buffer for inspection. |
| `max_output_bytes` | `usize?` | `16384` | Hard cap `1 048 576`. Tail rendered. |
| `wait` | `bool?` | `false` | FALLBACK long-poll. Block until `min_bytes` of new output arrive, the shell closes, or `wait_timeout_secs` expires. |
| `wait_timeout_secs` | `u64?` | `30` | Long-poll deadline; cap `300`. |
| `min_bytes` | `usize?` | `1` | Minimum new bytes to wait for; floor `1`, capped at the resolved `max_output_bytes`. |

**FALLBACK note**: prefer `resources/subscribe shell://<shell_id>/output` (realtime push) over polling. The long-poll mode here is a fallback for clients that cannot subscribe.

**Status values**: `OPEN`, `CLOSED`, `TIMEOUT`, `ERROR`.

**Response — OPEN**:
```
SSH_SHELL_READ: OPEN
SHELL_ID: 4b9c8e2a-...
--- data [c4d5e6f7] ---
$ ls -la
total 8
drwxr-xr-x  2 alice alice 4096 May  2 18:00 src
$
```

**Errors**: `SHELL_NOT_FOUND`.

---

### ssh_shell_wait_for

Wait for one of up to 16 substring patterns to appear in shell output. Single-shot prompt-gating fallback when subscribe is not available.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `shell_id` | `string` | — | `SHELL_ID`. |
| `patterns` | `string[]` | — | 1..=16 substring patterns. Each pattern up to 1024 bytes. First match wins. |
| `timeout_secs` | `u64?` | `30` | Maximum seconds to wait; cap `300`. |
| `max_output_bytes` | `usize?` | `16384` | Hard cap `1 048 576`. |
| `clear` | `bool?` | `true` | Drain matched output from the shell history (head) on match. |

**FALLBACK note**: prefer `resources/subscribe shell://<shell_id>/output` for continuous observation; use this for single-shot gating only.

**Status values**: `MATCHED`, `TIMEOUT`, `CLOSED`, `ERROR`.

**Response — MATCHED**:
```
SSH_SHELL_WAIT_FOR: MATCHED
SHELL_ID: 4b9c8e2a-...
MATCHED_PATTERN: $
--- data [d8e9f0a1] ---
some output
followed by
the prompt $
```

**Response — TIMEOUT**:
```
SSH_SHELL_WAIT_FOR: TIMEOUT
SHELL_ID: 4b9c8e2a-...
--- data [d8e9f0a1] ---
output collected so far
```

**Errors**: `SHELL_NOT_FOUND`, `EMPTY_PATTERNS`, `TOO_MANY_PATTERNS`, `PATTERN_TOO_LONG`.

---

### ssh_shell_close

Close an interactive shell and release its PTY channel.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `shell_id` | `string` | — | `SHELL_ID`. |

**FALLBACK note**: prefer `resources/subscribe shell://<shell_id>/output` while the shell is alive; this tool finalises the lifecycle. Active subscribers receive a final closed event.

**Response**:
```
SSH_SHELL_CLOSE: OK
SHELL_ID: 4b9c8e2a-...
```

**Errors**: `SHELL_NOT_FOUND`.

---

## SFTP (3)

### ssh_upload

Upload a local file to a remote path via SFTP. Streams in 32 KiB chunks.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `local_path` | `string` | — | Local file. Relative paths resolve against `$HOME`. |
| `remote_path` | `string` | — | Remote destination. |

Limits: up to 10 concurrent transfers per session.

**Tip**: subscribe to `transfer://<transfer_id>/progress` for realtime progress.

**Status values**: `STARTED`, `ERROR`.

**Response**:
```
SSH_UPLOAD: STARTED
TRANSFER_ID: 8f7e6d5c-...
SESSION_ID: a3f2b1d7-...
AGENT: claude
FROM: /home/alice/data.csv
TO: /tmp/data.csv
SIZE: 2.3 MB (2412544 bytes)
BYTES: 2412544
```

`FROM` is the source (local for upload, remote for download); `TO` is the destination. `SIZE` is the human-readable + raw byte count; `BYTES` is the raw count again for easy parsing. `AGENT` is omitted when the session has no agent.

**Errors**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `LOCAL_FILE_ERROR`, `LOCAL_NOT_FILE`.

---

### ssh_download

Download a remote file to a local path via SFTP. Streams in 32 KiB chunks.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `remote_path` | `string` | — | Remote source. |
| `local_path` | `string` | — | Local destination. Relative paths resolve against `$HOME`. |

**Response**:
```
SSH_DOWNLOAD: STARTED
TRANSFER_ID: 1a2b3c4d-...
SESSION_ID: a3f2b1d7-...
FROM: /var/backups/backup.tar.gz
TO: /home/alice/backup.tar.gz
SIZE: 105.0 MB (110100480 bytes)
BYTES: 110100480
```

Same shape as upload — `FROM` is the remote source for downloads, `TO` is the local destination.

**Errors**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `SFTP_OPEN_FAILED`, `REMOTE_METADATA_ERROR`.

---

### ssh_get_transfer_progress

Read the current progress of an SFTP transfer.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `transfer_id` | `string` | — | `TRANSFER_ID`. |
| `wait` | `bool?` | `false` | Block until termination or timeout. |
| `wait_timeout_secs` | `u64?` | `30` | Long-poll deadline; cap `300`. |

Terminated transfers are cleaned from storage after `SSH_TRANSFER_CLEANUP_TTL` (default 300 s).

**Status values**: `RUNNING`, `COMPLETED`, `FAILED`, `ERROR`.

**Response — RUNNING**:
```
SSH_GET_TRANSFER_PROGRESS: RUNNING
TRANSFER_ID: 8f7e6d5c-...
DIRECTION: UPLOAD
PROGRESS: 47% (1153024/2412544 bytes)
```

**Response — COMPLETED**:
```
SSH_GET_TRANSFER_PROGRESS: COMPLETED
TRANSFER_ID: 8f7e6d5c-...
DIRECTION: UPLOAD
PROGRESS: 100% (2412544/2412544 bytes)
```

**Response — FAILED**:
```
SSH_GET_TRANSFER_PROGRESS: FAILED
TRANSFER_ID: 8f7e6d5c-...
DIRECTION: UPLOAD
PROGRESS: 12% (307200/2412544 bytes)
REASON: [PERMISSION_DENIED] write '/tmp/locked.csv': permission denied
```

`DIRECTION` is uppercase (`UPLOAD` / `DOWNLOAD`). `PROGRESS` is rendered as `<integer>% (<bytes_transferred>/<total_bytes> bytes)` — raw bytes, not human-readable, so the value is easy to parse.

**Errors**: `TRANSFER_NOT_FOUND`. SFTP failure codes: `FILE_NOT_FOUND`, `PERMISSION_DENIED`, `DISK_FULL`, `CONNECTION_LOST`, `REMOTE_DIR_NOT_FOUND`, `READ_ONLY_FS`, `SFTP_PROTOCOL`, `TIMEOUT`, `IO_ERROR`.

---

## Forward (1)

### ssh_forward

Local-to-remote TCP port forwarding through an SSH session. Feature-gated under `port_forward` (default-on).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `local_port` | `u16` | — | Local TCP port to listen on. |
| `remote_address` | `string` | — | Remote host (e.g. `localhost`, `10.0.0.1`). |
| `remote_port` | `u16` | — | Remote TCP port. |

**Response**:
```
SSH_FORWARD: OK
LOCAL: 0.0.0.0:8080
REMOTE: 10.0.0.1:3306
ACTIVE: true
```

**Errors**: `SESSION_NOT_FOUND`, `FORWARD_FAILED`, `FEATURE_DISABLED`.

---

## Resources (5 schemes)

URIs follow `<scheme>://<id>/<sub-path>[?cursor=auto|<N>|0]`. The `?cursor=` query string is silently ignored on point-in-time resources (`transfer://`, `session://`).

| Scheme | URI | MIME | Cursor | Producer |
|--------|-----|------|--------|----------|
| `shell` | `shell://<shell_id>/output` | `text/plain` | yes | PTY reader (`RunningShell`) |
| `command` | `command://<command_id>/output` | `text/plain` | yes | Async command reader (`RunningCommand`) |
| `transfer` | `transfer://<transfer_id>/progress` | `application/json` | no | SFTP loop (`RunningTransfer`) |
| `session` | `session://<session_id>/health` | `application/json` | no | Health probes / connect reuse path |
| `forward` | `forward://<forward_id>/events` | `application/json` | yes | Forward task (feature-gated) |

### `?cursor` semantics (byte-stream resources)

| Value | Meaning |
|-------|---------|
| _omitted_ or `cursor=0` | Full snapshot. |
| `cursor=auto` | Server-tracked per-peer delta. Returns only the bytes newer than the previous read for THIS peer. |
| `cursor=<N>` | Explicit absolute byte offset into the buffer. |

### `_meta` payload

Every `resources/read` response embeds a `_meta` object on `ResourceContents`:

| Key | Type | Notes |
|-----|------|-------|
| `cursor` | `u64` | Next cursor value to use. |
| `buffer_size` | `u64` | Bytes currently held in the resource history (`0` for point-in-time). |
| `last_seq` | `u64` | Last allocated sequence for `(kind, id)`. |
| `<kind>_status` | `string` | `shell_status`, `command_status`, `transfer_status`, `session_status`, `forward_status`. |
| `truncated_since_last_read` | `u64` | Bytes dropped from the head since previous read (only when positive). |
| `keepalive` | `bool` | `true` when no fresh bytes / events were available. |
| `lagged_since_last_read` | `u64` | Reserved for future broadcast-`Lagged` recovery telemetry (currently omitted). |

### `resources/list`

Aggregates open shells, running commands, active transfers, connected sessions. `forward://` is intentionally empty until `ForwardStorage` ships (see [ARCHITECTURE.md Future work](./ARCHITECTURE.md#future-work)). Each entry shape:

```json
{
  "uri": "shell://4b9c8e2a-.../output",
  "name": "Shell 4b9c8e2a-... (session a3f2b1d7-...)",
  "description": "PTY output buffer for shell 4b9c8e2a-... (xterm, 80x24).",
  "mimeType": "text/plain"
}
```

### `resources/read`

Returns a single `TextResourceContents`:

```json
{
  "uri": "shell://4b9c8e2a-.../output",
  "mimeType": "text/plain",
  "text": "$ ls -la\ntotal 8\n...",
  "_meta": {
    "cursor": 4096,
    "buffer_size": 4096,
    "last_seq": 17,
    "shell_status": "open"
  }
}
```

For `transfer://` and `session://`, `text` is the JSON payload itself (since the MIME is `application/json`):

```json
{
  "uri": "transfer://8f7e6d5c-.../progress",
  "mimeType": "application/json",
  "text": "{\"transfer_id\":\"8f7e6d5c-...\",\"session_id\":\"a3f2b1d7-...\",\"direction\":\"upload\",\"local_path\":\"/home/alice/data.csv\",\"remote_path\":\"/tmp/data.csv\",\"started_at\":\"2026-05-02T18:11:00Z\",\"status\":\"running\",\"bytes_transferred\":1153024,\"total_bytes\":2412544,\"error\":null,\"last_seq\":42}",
  "_meta": {
    "cursor": 0,
    "buffer_size": 0,
    "last_seq": 42,
    "transfer_status": "running"
  }
}
```

### `resources/subscribe`

Body: `{ "uri": "<scheme>://<id>/<sub-path>" }`.

Server validates the URI and confirms the resource exists (`SESSION_NOT_FOUND` / `SHELL_NOT_FOUND` / `COMMAND_NOT_FOUND` / `TRANSFER_NOT_FOUND` map to `McpError::resource_not_found`). The first subscriber for `(kind, id)` spawns a debouncer task. Re-subscribing the same peer refreshes the live `Peer` handle without duplicating subscribers.

Returns `()` on success.

### `resources/unsubscribe`

Body: `{ "uri": "<scheme>://<id>/<sub-path>" }`.

Idempotent — unknown URIs / not-subscribed peers silently no-op (per MCP spec). The last unsubscribe drops the debouncer task.

### Notifications outbound

| Notification | Trigger |
|--------------|---------|
| `notifications/resources/updated` | Per debounce window per resource (50 ms default), plus force-flush every `SSH_NOTIFY_FORCE_FLUSH_MS` (default `1000`) and keepalive every `SSH_NOTIFY_KEEPALIVE_S` (default `30`). |
| `notifications/resources/list_changed` | Capability is advertised in `get_info()`. Wiring through tool entry points that create / destroy resources lands in a follow-up — see [ARCHITECTURE.md Future work](./ARCHITECTURE.md#future-work). |
| `notifications/cancelled` | Routed natively by rmcp 1.6 — no custom handling required. Tools observing a `CancellationToken` (e.g. `ssh_cancel_command`'s internal cancel path) react as expected. |

### Capability handshake

Returned by `get_info()`:

```json
{
  "protocolVersion": "2025-06-18",
  "serverInfo": { "name": "ssh-mcp", "version": "3.0.0" },
  "capabilities": {
    "tools": { "listChanged": true },
    "resources": { "subscribe": true, "listChanged": true }
  },
  "instructions": "SSH MCP server — 18 SSH tools and 5 resource subscribe schemes (shell://, command://, transfer://, session://, forward://). Prefer `resources/subscribe shell://<shell_id>/output` for realtime PTY streams over polling-based ssh_shell_read; the long-poll variants (ssh_shell_read.wait, ssh_shell_wait_for) are FALLBACKS for clients that cannot subscribe. Use resources/list to enumerate active shells, commands, transfers, and sessions."
}
```

See [ARCHITECTURE.md](./ARCHITECTURE.md#subscription-pipeline) for the producer → debouncer → notification pipeline and [FLOWS.md](./FLOWS.md) for end-to-end sequence diagrams.

---

## Cross-reference — keyboard input

Use the table below to choose between `ssh_shell_send_key` and `ssh_shell_write`:

| Goal | Tool | Notes |
|------|------|-------|
| Interrupt running command | `ssh_shell_send_key key=ctrl_c` | Preferred. |
| Submit Enter | `ssh_shell_send_key key=enter` | Or `ssh_shell_write input="\n"`. |
| Type a command line | `ssh_shell_write input="ls -la\n"` | `send_key` rejects bulk text. |
| Arrow key navigation | `ssh_shell_send_key key=arrow_up` | Modifiers allowed. |
| Function keys | `ssh_shell_send_key key=f5` | Modifiers allowed. |
| Back-tab in completion menu | `ssh_shell_send_key key=tab shift=true` | Tab accepts Shift only. |
| Send Ctrl+Shift+End in a TUI | `ssh_shell_send_key key=end shift=true ctrl=true` | Modifier rule honored. |
| Send Alt+B (word back in readline) | `ssh_shell_write input="\x1bb"` | `send_key` does not expose every escape; raw bytes when needed. |
