# SSH MCP API Reference (v4.8.0)

Complete API reference for the 21 MCP tools (or 20 without `port_forward`), the 5 resource subscribe schemes, and the v4.7 inter-tool conversation surface (`structured_content` JSON channel, `resources/templates/list`, `notifications/progress`, `prompts/*` catalog, idempotency cache, NOT_FOUND closest-match suggestions, `INITIAL_BUFFER` line) exposed by the v4.8.0 ssh-mcp server (rmcp 1.6, protocol `V_2025_06_18`). Text channel is byte-compatible with v3.0.0 / v4.0.x / v4.6.0 / v4.7.x; the v4.7 `structured_content` payload sits next to it. v4.8 lifts typed `output_schema` advertisement to **all 21 tools** (was 9 in v4.7). v4.7 added three new tools — `ssh_run` (one-shot connect + execute + optional disconnect), `ssh_execute_batch` (sequential 1..=16 commands per session), `ssh_disconnect_many` (best-effort batch, 1..=64 ids). v4.6 carry-forward: `AGENT_ID:` (was `AGENT:`), `NEXT:` advisory lines, four subscribe-first `HINT:` sites, JSON Schema `default` keywords, one-line `Cost:` hints, wired `Implementation.icons`. v4.5 carry-forward: `EXPIRES_AT` / `PERSISTENT` / `HINT` on connect, `_meta` envelope on `resources/read`, granular wire error codes, server identity, tool annotations, `FORWARD_ID` / `SESSION_ID` on `ssh_forward`. See [ARCHITECTURE.md](./ARCHITECTURE.md) and [MIGRATION.md → v3 → v4](./MIGRATION.md#v3--v4).

> **v4.8 — full `output_schema` coverage.** Every tool now publishes a typed JSON Schema on `tools/list[].outputSchema` mirroring its `structured_content` payload byte-for-byte. Smaller LLMs (Haiku / Llama / Qwen 7B-30B) can validate every tool response against the published shape without hard-coding any field names. Strictly additive on the `tools/list` metadata; the Markdown body and `structured_content` JSON shape are byte-identical to v4.7.1. Reference: `src/infra/mcp/results.rs` (21 typed structs).

> **v4.7 conversation surface.** Every tool emits Markdown + typed JSON (`structured_content`). v4.8 expanded `output_schema` advertisement from 9 / 21 to 21 / 21 tools. Errors render as `{ tool, status: "error", code, reason, detail }` on the structured channel. See [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47).

> **v4.7 new tools — `ssh_run`, `ssh_execute_batch`, `ssh_disconnect_many`.** Tool count moves from 18 to 21 (or 17 to 20 without `port_forward`). Per-tool sections below.

> **v4.6 wire change** — `AGENT:` -> `AGENT_ID:` (7 render sites). Hosts that grep `^AGENT:` literally must update; generic key-value parsers unaffected.

> **Subscribe-first** — prefer `resources/subscribe <scheme>://<id>/<sub-path>` over long-poll `wait=true`. v4.6 `HINT:` + `NEXT:` lines reinforce this on every async-spawn response. See [LLM_GUIDE.md](./LLM_GUIDE.md).

[[_TOC_]]

## Conventions

- **Response format**: every tool returns a single markdown `Text<String>`. The format is **block-only** — one `KEY: value` per line. There is no inline `KEY: v | KEY: v` form in v3.
- **Status case**: `SCREAMING_SNAKE_CASE` (`OK`, `RUNNING`, `MATCHED`, `TIMEOUT`, `CANCELLED`, `NOOP`, …).
- **Filter enum case**: `snake_case` for input enums (`reuse: "suggest" | "auto" | "force_new"`, `status: "running" | "completed" | "cancelled" | "failed"`).
- **Identifiers**: `*_ID` suffix in uppercase (`SESSION_ID`, `COMMAND_ID`, `SHELL_ID`, `TRANSFER_ID`, `AGENT_ID`, `FORWARD_ID`). v4.6 renamed the agent line key from `AGENT:` to `AGENT_ID:` for consistency.
- **Output blocks**: `--- stdout [<nonce>] ---`, `--- stderr [<nonce>] ---`, `--- data [<nonce>] ---`. The 8-hex `nonce` (regenerated per response) prevents the rendered content from forging the delimiter.
- **HINT:** lines steer the LLM toward subscribe-first resource URIs (one per async-spawn response: shell open / execute / upload / download / forward) and toward bulk-cleanup when an agent leaks sessions. v4.6 ships four new subscribe-first HINT sites.
- **NEXT:** lines (v4.6) end every response with a clear successor tool. Format: `NEXT: <tool>(args=...) | <tool>(args=...)` — pipe-separated concrete tool calls a smaller LLM can chain without consulting the docs. Terminal statuses (`COMPLETED`, `CLOSED`, `CANCELLED`, etc. — see the v4.6 coverage matrix in [LLM_GUIDE.md section E](./LLM_GUIDE.md#e-next-advisory-line-v46)) deliberately omit `NEXT:`.
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

`McpSshServer::get_info()` advertises tools (`listChanged: true`), resources (`subscribe: true`, `listChanged: true`), protocol `V_2025_06_18`, and the v4.5 `Implementation` identity (title / description / website_url) — plus the v4.6 `icons` entry — and a few-shot `instructions` block. See [Capability handshake (full payload)](#capability-handshake-1) below for the wire shape.

---

## Tools (21 with `port_forward`, 20 without)

The catalogue below covers every tool. v4.7 adds `ssh_run`, `ssh_execute_batch`, `ssh_disconnect_many`. Groups: Connection (5), Commands (6), Shell (6), SFTP (3), Network (1, feature-gated).

## Connection (5)

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
AGENT_ID: claude-code-instance-abc123
RETRY: 0
PERSISTENT: false
EXPIRES_AT: 2026-05-03T12:05:00+00:00
NEXT: ssh_execute(session_id=a3f2b1d7-..., command=...) | ssh_shell_open(session_id=a3f2b1d7-...) | ssh_disconnect(session_id=a3f2b1d7-...)
```

`HOST` always renders as `username@host:port`. `AGENT_ID` (renamed from `AGENT:` in v4.6) is omitted when no `agent_id` was passed. `REPLACED: N` is appended when stale matches were purged before creating the session. `PERSISTENT: false` is followed by an `EXPIRES_AT:` RFC3339 deadline (= `connected_at + SSH_INACTIVITY_TIMEOUT`); ping the session before this fires (any cheap call works) to extend it. When the caller passes `persistent=true`, the response emits `PERSISTENT: true` and omits `EXPIRES_AT`. The trailing `NEXT:` line (v4.6) lists the three most-likely successor calls pre-filled with the freshly minted `SESSION_ID`.

**Response — REUSED**:
```
SSH_CONNECT: REUSED
SESSION_ID: a3f2b1d7-...
HOST: alice@example.com:22
AGENT_ID: claude-code-instance-abc123
NEXT: ssh_execute(session_id=a3f2b1d7-..., command=...) | ssh_shell_open(session_id=a3f2b1d7-...) | ssh_disconnect(session_id=a3f2b1d7-...)
```

`RETRY`, `PERSISTENT`, and `EXPIRES_AT` are omitted on `REUSED` (the original connect already set them; query `ssh_list_sessions` to refresh).

**Response — SUGGESTED (single match)**:
```
SSH_CONNECT: SUGGESTED
EXISTING_SESSION_ID: a3f2b1d7-...
HOST: alice@example.com:22
AGENT_ID: claude
NAME: prod-db
CONNECTED_AT: 2026-05-02T18:00:00Z
HEALTHY: true
HINT: use existing SESSION_ID, or retry with reuse="force_new"
NEXT: ssh_connect(session_id=a3f2b1d7-...) | ssh_connect(reuse="force_new")
```

**Response — SUGGESTED (multi-match)**:
```
SSH_CONNECT: SUGGESTED
MATCHES: 2
- a3f2b1d7-... alice@example.com:22 [agent: claude, name: prod-db, connected: 2026-05-02T18:00:00Z, healthy]
- 9b1c2d3e-... alice@example.com:22 [agent: claude, name: prod-db-2, connected: 2026-05-02T17:50:00Z, healthy]
HINT: pick an existing SESSION_ID, or retry with reuse="force_new"
NEXT: ssh_connect(session_id=<existing>) | ssh_connect(reuse="force_new")
```

When more than 5 healthy sessions are owned by the same `agent_id`, an extra anti-leak hint is appended on `SUGGESTED` and on `ssh_list_sessions`:

```
HINT: agent 'X' owns N sessions; consider ssh_disconnect_agent to bulk-cleanup
```

When `agent_id` is set on `ssh_connect`, `reuse=auto` and `reuse=suggest` rank sessions owned by the same agent first.

**Errors**: `CONNECTION_FAILED` (handshake or all retries exhausted), `AUTH_FAILED`.

**Wire codes**: `CONNECTION_FAILED`, `AUTH_FAILED`.

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

`SSH_DISCONNECT: OK` is terminal — no `NEXT:`.

**Errors**: `SESSION_NOT_FOUND`, `TRANSPORT_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `TRANSPORT_ERROR`.

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
NEXT: ssh_disconnect_agent(agent_id=claude) | ssh_disconnect(session_id=a3f2b1d7-...)
```

Each item is `<SESSION_ID> <username>@<host>` followed by an optional `[…]` annotation block. Annotations include any of `agent: <id>`, `name: <label>`, `compression: off`, and the health label (`healthy` / `unhealthy`). When `max_items` truncates, the COUNT line becomes `COUNT: N (showing N of M)`. When >5 healthy sessions are owned by one `agent_id`, an anti-leak `HINT: agent 'X' owns N sessions; consider ssh_disconnect_agent to bulk-cleanup` line is appended. v4.6 `NEXT:` is emitted on non-empty lists (suggesting `ssh_disconnect_agent` when an agent owns sessions, else `ssh_disconnect`); empty list (`COUNT: 0`) omits `NEXT:`.

**Wire codes**: `STORAGE_ERROR` (none on the happy path).

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
AGENT_ID: claude-code-instance-abc123
SESSIONS: 3
COMMANDS: 5
```

The `AGENT_ID:` key replaces the v4.5 `AGENT:` (v4.6 rename for consistency). Terminal — no `NEXT:`.

**Wire codes**: `STORAGE_ERROR` (none on the happy path; unknown agent returns `SESSIONS: 0`).

---

### ssh_disconnect_many (v4.7)

Best-effort batch disconnect of 1..=64 sessions in a single call. Per-id failures are reported in the response but do not abort the remaining disconnects.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_ids` | `string[]` | — | 1..=64 `SESSION_ID`s previously returned by `ssh_connect`. Empty list rejected with `INVALID_ARGUMENT`. |

When to use:
- Cleaning up a fan-out of sessions when bulk-by-agent is not appropriate.
- Tearing down an explicit subset of an agent's sessions without affecting the rest.

**Status values**: `OK`, `ERROR`.

**Response**:
```
SSH_DISCONNECT_MANY: OK
DISCONNECTED: 2
FAILED: 1
- a3f2b1d7-...: ok
- 9b1c2d3e-...: ok
- f0e1d2c3-...: error [SESSION_NOT_FOUND] no session with id f0e1d2c3-...
```

Each item carries the session id followed by `ok` (success) or `error [<CODE>] <reason>` (per-id failure). Counters at the top mirror the `disconnected` / `failed` fields in the structured channel.

**structured_content shape**: `{ tool: "ssh_disconnect_many", status: "ok", results: [{ session_id, status: "ok"|"error", code?, reason? }, ...], disconnected, failed }`. Top-level `status` is always `"ok"` — per-id failures live inside `results`. Full schema in [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47). Idempotency: pass `_meta.idempotency_key` to dedup retried bulk-disconnect calls.

**Errors**: `INVALID_ARGUMENT` (empty list / `>64` ids), `IDEMPOTENCY_KEY_TOO_LONG`.

**Wire codes**: `INVALID_ARGUMENT`, `IDEMPOTENCY_KEY_TOO_LONG`.

---

## Execute (6)

### ssh_execute

Execute a shell command asynchronously. Returns immediately with a `COMMAND_ID`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `command` | `string` | — | Shell command to run on the remote host. |
| `timeout_secs` | `u64?` | `180` | Command timeout (env `SSH_COMMAND_TIMEOUT`). |
| `pty` | `bool?` | `false` | Allocate a PTY for the command (e.g. `sudo`, `top`). All output merges to stdout in PTY mode. |

Limits: up to 100 concurrent multiplexed commands per session.

**Status values**: `STARTED`, `ERROR`.

**Response**:
```
SSH_EXECUTE: STARTED
COMMAND_ID: 7d4c8e2a-...
SESSION_ID: a3f2b1d7-...
AGENT_ID: claude-code-instance-abc123
HINT: subscribe to command://7d4c8e2a-.../output for realtime output (preferred over polling)
NEXT: ssh_get_command_output(command_id=7d4c8e2a-..., wait=true) | ssh_cancel_command(command_id=7d4c8e2a-...)
```

`AGENT_ID:` is omitted when the session has no agent. The v4.6 `HINT:` line steers the LLM to subscribe rather than poll; `NEXT:` lists the two successor calls pre-filled with `COMMAND_ID`.

**Errors**: `SESSION_NOT_FOUND`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR` (untagged), `CHANNEL_FAILED` (tagged transport).

---

### ssh_get_command_output

Read the current output and status of an async command.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command_id` | `string` | — | `COMMAND_ID` from `ssh_execute`. |
| `wait` | `bool?` | `false` | Block until completion or timeout. |
| `wait_timeout_secs` | `u64?` | `30` | Long-poll deadline; cap `300`. |
| `max_output_bytes` | `usize?` | `16384` | Hard cap `1 048 576` (env `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`). Tail (most recent) preserved. |

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

`COMPLETED` is terminal — no `NEXT:` line.

**Response — RUNNING**:
```
SSH_GET_COMMAND_OUTPUT: RUNNING
COMMAND_ID: 7d4c8e2a-...
--- stdout [a3f2b1d7] (partial) ---
... bytes so far ...
--- stderr [a3f2b1d7] (empty) ---
NEXT: resources/subscribe command://7d4c8e2a-.../output | ssh_get_command_output(command_id=7d4c8e2a-..., wait=true)
```

The v4.6 `NEXT:` on `RUNNING` steers toward subscribe-first push or a single long-poll instead of a tight polling loop.

**Errors**: `COMMAND_NOT_FOUND`, `COMMAND_FAILED`.

**Wire codes**: `COMMAND_NOT_FOUND`, `COMMAND_FAILED` (tagged from transport, e.g. exec channel died).

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

**Wire codes**: `STORAGE_ERROR` (none on the happy path).

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

**Wire codes**: `COMMAND_NOT_FOUND`.

---

### ssh_run (v4.7)

One-shot orchestration of `ssh_connect` + `ssh_execute(wait=true)` + (optional) `ssh_disconnect`. Avoids the three-round-trip `connect -> execute -> wait` choreography for short atomic commands like `uptime`, `hostname`, `cat /etc/release`. The session is minted (or reused via `reuse=auto`) under the hood.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `address` | `string` | — | `host[:port]` (e.g. `192.168.1.1:22`, `example.com`). Port defaults to `22`. |
| `username` | `string` | — | SSH login user. |
| `command` | `string` | — | Shell command to run on the remote host. |
| `password` | `string?` | — | Optional password for password authentication. |
| `key_path` | `string?` | — | Optional path to a private key file. Auth chain: key -> password -> agent (`SSH_AUTH_SOCK`). |
| `agent_id` | `string?` | — | Optional `AGENT_ID` for grouping the underlying session. |
| `pty` | `bool?` | `false` | Allocate a pseudo-terminal for the command. |
| `timeout_secs` | `u64?` | `30` | Maximum seconds to wait for the command. Cap `300` (`SshRunTimeoutCap`). |
| `max_output_bytes` | `usize?` | `16384` | Max bytes returned in stdout/stderr. Cap `1 048 576`. |
| `disconnect_after` | `bool?` | `true` | Disconnect the session after the command finishes (one-shot mode). Set `false` to keep the session open for follow-up `ssh_execute` calls. |

Behaviour:
1. `ssh_run` mints (or reuses) a session via `reuse=auto`, ranking matches by `agent_id` when set.
2. Spawns the command and blocks until completion or `timeout_secs` fires.
3. With `disconnect_after=true` (default) tears the session down after the command terminates.

**Status values**: `COMPLETED`, `TIMEOUT`, `FAILED`, `CANCELLED`, `ERROR`.

**Response — COMPLETED**:
```
SSH_RUN: COMPLETED
SESSION_ID: a3f2b1d7-...
COMMAND_ID: 7d4c8e2a-...
EXIT: 0
DISCONNECTED: true
--- stdout [c8d9e0f1] ---
14:22:01 up 12 days,  3:14,  1 user,  load average: 0.21, 0.14, 0.10
--- stderr [c8d9e0f1] (empty) ---
```

`DISCONNECTED:` is `true` when `disconnect_after=true` (default) and the post-execute disconnect succeeded; `false` when the caller opted to keep the session alive. The resolved `SESSION_ID:` is preserved in either case so the caller can reuse it. Terminal — no `NEXT:` line on `COMPLETED`.

**structured_content shape**: `{ tool: "ssh_run", status, session_id, command_id, disconnected, exit_code?, stdout, stderr, stdout_truncated, stderr_truncated, timed_out, error? }`. Full schema in [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47). Idempotency: pass `_meta.idempotency_key` to dedup retried `ssh_run` calls (connect + execute + disconnect form one logical operation).

**Errors**: `CONNECTION_FAILED`, `AUTH_FAILED`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR`, `IDEMPOTENCY_KEY_TOO_LONG`.

**Wire codes**: `CONNECTION_FAILED`, `AUTH_FAILED`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR`, `IDEMPOTENCY_KEY_TOO_LONG`.

---

### ssh_execute_batch (v4.7)

Sequential execution of 1..=16 commands against a single session, with optional stop-on-failure semantics. Trades the per-command round-trip for a single tool call when a small linear pipeline (`mkdir`, `tar -xzf`, `chown -R`) needs to run in order.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `commands` | `string[]` | — | 1..=16 commands, executed in order. |
| `stop_on_failure` | `bool?` | `true` | Halt the loop on the first non-zero exit code; remaining slots surface as `skipped`. |
| `timeout_secs_per_command` | `u64?` | `30` | Per-command wait timeout. Cap `300`. |
| `max_output_bytes_per_command` | `usize?` | `16384` | Per-command max bytes returned in stdout/stderr. Cap `1 048 576`. |
| `pty` | `bool?` | `false` | Allocate a PTY for each command. |

**Status values**: `OK`, `HALTED`, `ERROR`. `OK` when every command ran (regardless of exit code); `HALTED` when `stop_on_failure=true` short-circuited the loop after the first non-zero exit.

**Response — HALTED**:
```
SSH_EXECUTE_BATCH: HALTED
SESSION_ID: a3f2b1d7-...
TOTAL: 3
EXECUTED: 2
- [0] mkdir /tmp/foo: COMPLETED exit=0
- [1] tar -xzf bundle.tgz -C /tmp/foo: FAILED exit=2
- [2] chown -R svc /tmp/foo: SKIPPED
```

Each `results[]` entry carries its own `command_id`, `exit_code`, stdout/stderr blocks, and (optional) `error` string.

**structured_content shape**: `{ tool: "ssh_execute_batch", status, session_id, total, executed, results: [{ index, command, status: "completed|failed|timeout|cancelled|skipped", command_id?, exit_code?, stdout, stderr, stdout_truncated, stderr_truncated, timed_out, error? }, ...] }`. Full schema in [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47). Idempotency: pass `_meta.idempotency_key` to dedup retried batches.

**Errors**: `SESSION_NOT_FOUND`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR`, `INVALID_ARGUMENT` (empty / `>16` commands), `IDEMPOTENCY_KEY_TOO_LONG`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR` (untagged), `CHANNEL_FAILED` (tagged transport), `INVALID_ARGUMENT`, `IDEMPOTENCY_KEY_TOO_LONG`.

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

**Status values**: `OK`, `ERROR`.

**Response**:
```
SSH_SHELL_OPEN: OK
SHELL_ID: 4b9c8e2a-...
SESSION_ID: a3f2b1d7-...
TERM: xterm 80x24
AGENT_ID: claude-code-instance-abc123
INITIAL_BUFFER: Last login: Sat May  3 14:22:01 2026 from 10.0.0.4\r\n$ 
HINT: subscribe to shell://4b9c8e2a-.../output for realtime output (preferred over polling)
NEXT: resources/subscribe shell://4b9c8e2a-.../output | ssh_shell_write | ssh_shell_send_key
```

`TERM` carries the terminal type and the geometry on a single line (`<term> <cols>x<rows>`). `AGENT_ID:` (renamed from `AGENT:` in v4.6) is omitted when no agent owns the session. The v4.6 `HINT:` steers toward push notifications; `NEXT:` names the three successor calls.

**v4.7 `INITIAL_BUFFER:` line.** When the PTY emits stdout within `SSH_SHELL_OPEN_INITIAL_PEEK_MS` (default 100 ms; tick `SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS` default 5 ms) of the open call, the response embeds a single `INITIAL_BUFFER:` line with the head-truncated bytes (cap `SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES`, default 4 KiB). CR/LF escaped to `\r`/`\n`. Structured twin emits `initial_buffer`. Omitted when no stdout arrived within the budget. Smaller LLMs sometimes skip the first `resources/read` round-trip when the prompt is already visible. Reference: `src/infra/mcp/render/shell.rs::shell_open_render_with_initial`. See [LLM_GUIDE.md section O](./LLM_GUIDE.md#o-initial_buffer-on-ssh_shell_open-v47).

**Errors**: `SESSION_NOT_FOUND`, `MAX_SHELLS_EXCEEDED`, `CHANNEL_FAILED`, `TRANSPORT_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_SHELLS_EXCEEDED`, `CHANNEL_FAILED` (tagged transport — typically remote `MaxSessions` exhaustion), `TRANSPORT_ERROR` (untagged).

---

### ssh_shell_write

Send raw bytes to an interactive shell. Use `ssh_shell_send_key` for named keystrokes whenever possible.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `shell_id` | `string` | — | `SHELL_ID`. |
| `input` | `string` | — | Bytes to send. Append `\n` to submit a typed line. Use `\x03` for Ctrl+C, `\x1b[A` for arrow up. |

**Response**:
```
SSH_SHELL_WRITE: OK
SHELL_ID: 4b9c8e2a-...
BYTES_SENT: 7
NEXT: resources/read shell://4b9c8e2a-.../output?cursor=auto | ssh_shell_wait_for | ssh_shell_read
```

The v4.6 `NEXT:` names the three ways to consume the response: cursor-based push read, prompt-gating wait, or pull-mode read.

**Errors**: `SHELL_NOT_FOUND`, `WRITE_FAILED`, `TRANSPORT_ERROR`.

**Wire codes**: `SHELL_NOT_FOUND`, `WRITE_FAILED` (tagged transport — writer task closed), `TRANSPORT_ERROR` (untagged).

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

**Response — plain key**:
```
SSH_SHELL_SEND_KEY: OK
SHELL_ID: 4b9c8e2a-...
KEY: ctrl_c
REPEAT: 1
BYTES_SENT: 1
NEXT: resources/read shell://4b9c8e2a-.../output?cursor=auto | ssh_shell_wait_for | ssh_shell_read
```

**Response — modified key**:
```
SSH_SHELL_SEND_KEY: OK
SHELL_ID: 4b9c8e2a-...
KEY: arrow_up
MODIFIERS: shift+ctrl
REPEAT: 3
BYTES_SENT: 18
NEXT: resources/read shell://4b9c8e2a-.../output?cursor=auto | ssh_shell_wait_for | ssh_shell_read
```

`MODIFIERS:` is omitted when no modifier flag is set; `BYTES_SENT` is `repeat * encoded_len(key, mods)`. v4.6 `NEXT:` mirrors `ssh_shell_write` (three ways to consume the response).

**Errors**: `SHELL_NOT_FOUND`, `MODIFIER_NOT_ALLOWED`, `INVALID_REPEAT`, `WRITE_FAILED`, `TRANSPORT_ERROR`.

**Wire codes**: `SHELL_NOT_FOUND`, `MODIFIER_NOT_ALLOWED` (tagged invalid argument), `INVALID_REPEAT` (tagged invalid argument), `WRITE_FAILED` (tagged transport), `TRANSPORT_ERROR` (untagged).

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

**Wire codes**: `SHELL_NOT_FOUND`.

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

**Status values**: `MATCHED`, `TIMEOUT`, `CLOSED`, `ERROR`.

**Response — MATCHED**:
```
SSH_SHELL_WAIT_FOR: MATCHED
SHELL_ID: 4b9c8e2a-...
MATCHED_PATTERN: $
BYTES_RETURNED: 30
--- data [d8e9f0a1] ---
some output
followed by
the prompt $
NEXT: ssh_shell_write(shell_id=4b9c8e2a-..., ...) | ssh_shell_send_key(shell_id=4b9c8e2a-..., ...) | ssh_shell_close(shell_id=4b9c8e2a-...)
```

**Response — TIMEOUT**:
```
SSH_SHELL_WAIT_FOR: TIMEOUT
SHELL_ID: 4b9c8e2a-...
BYTES_RETURNED: 12
--- data [d8e9f0a1] ---
output collected so far
NEXT: ssh_shell_wait_for(shell_id=4b9c8e2a-..., ...) | ssh_shell_read(shell_id=4b9c8e2a-...) | ssh_shell_close(shell_id=4b9c8e2a-...)
```

`MATCHED` and `TIMEOUT` both emit `NEXT:` (different successors per status). `CLOSED` is terminal — no `NEXT:`.

**Errors**: `SHELL_NOT_FOUND`, `EMPTY_PATTERNS`, `TOO_MANY_PATTERNS`, `PATTERN_TOO_LONG`.

**Wire codes**: `SHELL_NOT_FOUND`, `EMPTY_PATTERNS`, `TOO_MANY_PATTERNS`, `PATTERN_TOO_LONG` (all three tagged from invalid argument), `INVALID_ARGUMENT` (untagged catch-all).

---

### ssh_shell_close

Close an interactive shell and release its PTY channel.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `shell_id` | `string` | — | `SHELL_ID`. |

Active subscribers receive a final closed event when this tool runs.

**Response**:
```
SSH_SHELL_CLOSE: OK
SHELL_ID: 4b9c8e2a-...
```

**Errors**: `SHELL_NOT_FOUND`, `TRANSPORT_ERROR`.

**Wire codes**: `SHELL_NOT_FOUND`, `TRANSPORT_ERROR`.

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

**Status values**: `STARTED`, `ERROR`.

**Response**:
```
SSH_UPLOAD: STARTED
TRANSFER_ID: 8f7e6d5c-...
SESSION_ID: a3f2b1d7-...
AGENT_ID: claude
FROM: /home/alice/data.csv
TO: /tmp/data.csv
SIZE: 2.3 MB (2412544 bytes)
BYTES: 2412544
HINT: subscribe to transfer://8f7e6d5c-.../progress for realtime progress
NEXT: ssh_get_transfer_progress(transfer_id=8f7e6d5c-..., wait=true)
```

`FROM` is the source (local for upload, remote for download); `TO` is the destination. `SIZE` is the human-readable + raw byte count; `BYTES` is the raw count again. `AGENT_ID:` (renamed from `AGENT:` in v4.6) is omitted when the session has no agent. v4.6 `HINT:` steers toward subscribe; `NEXT:` names the long-poll fallback.

**Errors**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `LOCAL_FILE_ERROR`, `LOCAL_NOT_FILE`, `SFTP_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `LOCAL_FILE_ERROR` (tagged SFTP — `fs::metadata` failed), `LOCAL_NOT_FILE` (live in v4.6 — emitted from `application/upload_file.rs::guard_local_path_is_file` when the local path resolves but is not a regular file), `SFTP_ERROR` (untagged catch-all).

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
HINT: subscribe to transfer://1a2b3c4d-.../progress for realtime progress
NEXT: ssh_get_transfer_progress(transfer_id=1a2b3c4d-..., wait=true)
```

Same shape as upload — `FROM` is the remote source, `TO` is the local destination. `AGENT_ID:`, `HINT:`, and `NEXT:` lines mirror upload.

**Errors**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `SFTP_OPEN_FAILED`, `REMOTE_METADATA_ERROR`, `SFTP_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `SFTP_OPEN_FAILED` (tagged SFTP), `REMOTE_METADATA_ERROR` (live in v4.6 — emitted from `adapters/sftp/russh_sftp_adapter.rs::stat_remote_size` when the remote stat call fails), `SFTP_ERROR` (untagged catch-all).

---

### ssh_get_transfer_progress

Read the current progress of an SFTP transfer.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `transfer_id` | `string` | — | `TRANSFER_ID`. |
| `wait` | `bool?` | `false` | Block until termination or timeout. |
| `wait_timeout_secs` | `u64?` | `30` | Long-poll deadline; cap `300`. |

Terminated transfers are cleaned from storage after `SSH_TRANSFER_CLEANUP_TTL` (default 300 s).

**Status values**: `RUNNING`, `COMPLETED`, `FAILED`, `CANCELLED`, `ERROR`.

**Response — RUNNING**:
```
SSH_GET_TRANSFER_PROGRESS: RUNNING
TRANSFER_ID: 8f7e6d5c-...
DIRECTION: UPLOAD
PROGRESS: 47% (1153024/2412544 bytes)
NEXT: resources/subscribe transfer://8f7e6d5c-.../progress | ssh_get_transfer_progress(transfer_id=8f7e6d5c-..., wait=true)
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

`DIRECTION` is uppercase (`UPLOAD` / `DOWNLOAD`). `PROGRESS` is rendered as `<integer>% (<bytes_transferred>/<total_bytes> bytes)` — raw bytes, easy to parse. v4.6 `NEXT:` emitted only on `RUNNING`; terminal statuses (`COMPLETED` / `FAILED` / `CANCELLED`) omit it.

**v4.8.1 fix**: prior to v4.8.1 the `RUNNING` snapshot always reported `bytes_transferred = 0` until terminal hand-off (the streaming task incremented a live atomic but no path mirrored it to the repo entity). v4.8.1 adds a per-transfer progress watcher that consumes the `progress_tx` broadcast and syncs the repo within ~250 ms of the latest chunk, so `RUNNING` snapshots and `transfer://<id>/progress` resource reads now reflect real live bytes. The wire shape is unchanged — only the *value* of `bytes_transferred` during running snapshots is now correct.

**Errors**: `TRANSFER_NOT_FOUND`. SFTP failure codes (carried in `REASON: [...]` of a `FAILED` transfer body, not a tool ERROR): `FILE_NOT_FOUND`, `PERMISSION_DENIED`, `DISK_FULL`, `CONNECTION_LOST`, `REMOTE_DIR_NOT_FOUND`, `READ_ONLY_FS`, `SFTP_PROTOCOL`, `TIMEOUT`, `IO_ERROR`.

**Wire codes**: `TRANSFER_NOT_FOUND`.

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
FORWARD_ID: fwd-1
SESSION_ID: a3f2b1d7-...
LOCAL: 0.0.0.0:8080
REMOTE: 10.0.0.1:3306
ACTIVE: true
HINT: subscribe to forward://fwd-1/events for realtime event log
NEXT: resources/subscribe forward://fwd-1/events
```

`FORWARD_ID` + `SESSION_ID` (added in v4.5) let callers construct the `forward://<FORWARD_ID>/events` URI without round-tripping `resources/list`. v4.6 `HINT:` and `NEXT:` reinforce subscribe-first.

**Errors**: `SESSION_NOT_FOUND`, `FORWARD_FAILED`, `FEATURE_DISABLED`, `PORT_IN_USE`.

**Wire codes**: `SESSION_NOT_FOUND`, `PORT_IN_USE`, `FEATURE_DISABLED` (tagged invalid argument — emitted only on the no-`port_forward` build when subscribing to `forward://`), `FORWARD_FAILED` (live in v4.6 — emitted from `application/forward_port.rs::ForwardPortUseCase::preflight_bind` when the local listener bind fails for reasons other than `AddrInUse`).

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

### `_meta` payload (v4.5)

Every `resources/read` response embeds a `_meta` object on `ResourceContents`. Stream resources (`shell` / `command`) carry the cursor pair; snapshots (`transfer` / `session` / `forward`) omit them.

| Key | Type | Carried on | Notes |
|-----|------|------------|-------|
| `kind` | `string` | all | One of `"shell" | "command" | "transfer" | "session" | "forward"`. |
| `cursor` | `u64` | shell, command | Next cursor value to pass on the following `?cursor=` read. |
| `buffer_size` | `u64` | shell, command | Bytes currently held in the resource history. |
| `last_seq` | `u64` | all | Last allocated sequence for `(kind, id)`. |
| `status` | `string` | all | Kind-specific (`open` / `closed` / `running` / `completed` / `failed` / `healthy` / `unhealthy`). |

The peer identity used to track per-peer cursors is derived from transport: `Mcp-Session-Id` header on HTTP, singleton `Stdio` key on stdio. See [RESOURCES.md](./RESOURCES.md) for the full subscribe + truncation contract.

### `resources/list`, `read`, `subscribe`, `unsubscribe`, notifications

These four methods plus the outbound `notifications/resources/updated` / `list_changed` / `cancelled` notifications are documented in full in [RESOURCES.md](./RESOURCES.md) (URI grammar, list aggregation, `_meta` envelope per scheme, JSON / text body shape, subscribe lifecycle, debouncer + force-flush + keepalive timings, peer-GC). The wire shape is byte-compatible with v3.0.0 / v4.0.x; v4.5 made the `_meta` envelope live on every read; v4.6 leaves the resource pipeline unchanged.

### Capability handshake

Returned by `get_info()`:

```json
{
  "protocolVersion": "2025-06-18",
  "serverInfo": {
    "name": "ssh-mcp",
    "version": "<CARGO_PKG_VERSION>",
    "title": "SSH Remote Shell",
    "description": "Run remote commands, drive PTY shells, transfer files via SFTP, and forward TCP ports over SSH. Subscribe to shell, command, transfer, session, and forward streams for push notifications.",
    "websiteUrl": "https://github.com/farchanjo/ssh-mcp",
    "icons": [
      {
        "src": "https://raw.githubusercontent.com/farchanjo/ssh-mcp/master/assets/icon.svg",
        "mimeType": "image/svg+xml",
        "sizes": ["any"]
      }
    ]
  },
  "capabilities": {
    "tools": { "listChanged": true },
    "resources": { "subscribe": true, "listChanged": true }
  },
  "instructions": "SSH MCP. 21 tools, 5 push streams (shell://, command://, transfer://, session://, forward://). All tools return block markdown (KEY: value, --- name [nonce] ---) + a typed JSON in structured_content. IDs end in _ID. NEXT: line lists successor tools.\n\nHappy paths:\n1) One-shot: ssh_run(address, username, command). Returns exit_code in one call.\n2) Run async: ssh_connect (agent_id, reuse=Auto). Then ssh_execute. Then ssh_get_command_output wait=true (subscribe command://<id>/output for push).\n3) Interactive shell: ssh_connect, ssh_shell_open (returns INITIAL_BUFFER if the prompt arrives within 100ms). Then resources/subscribe shell://<id>/output. Drive with ssh_shell_write or ssh_shell_send_key. Read deltas via resources/read?cursor=auto on each notification. ssh_shell_close, ssh_disconnect.\n4) Upload: ssh_upload. Then ssh_get_transfer_progress wait=true.\n\nCleanup: agent_id on connect, ssh_disconnect_agent for bulk-close. Watch HINT lines and EXPIRES_AT. Pass _meta.idempotency_key on retries to dedup."
}
```

The build without `port_forward` advertises `20 tools, 4 push streams (shell://, command://, transfer://, session://)` instead.

`Implementation.icons` is wired in v4.6 to a single hosted SVG entry (`https://raw.githubusercontent.com/farchanjo/ssh-mcp/master/assets/icon.svg`, `image/svg+xml`, `sizes=["any"]`). The URL only resolves after the v4.6 push to `origin/master` lands; clients gracefully fall back to the title + description when the asset is unreachable. Implementation: `src/infra/mcp/tool_router.rs::build_implementation`.

Each of the 21 tools (or 20 without `port_forward`) carries a `Tool.title` plus `ToolAnnotations.{read_only_hint, destructive_hint, idempotent_hint}`. See [LLM_GUIDE.md section C](./LLM_GUIDE.md#c-server-identity-for-the-host-v45-icon-wired-in-v46) for the matrix. v4.7 also advertises `prompts/list` (5 entries) and `resources/templates/list` (4 / 5 entries depending on `port_forward`).

See [ARCHITECTURE.md](./ARCHITECTURE.md#subscribe-pipeline-v5-layered-view) for the producer → debouncer → notification pipeline and [DEVELOPMENT.md → Hot-path sequence diagrams](./DEVELOPMENT.md#hot-path-sequence-diagrams) for end-to-end sequence diagrams.

---

## structured_content channel and output_schema (v4.7 channel, v4.8 full schema)

Every tool response carries BOTH the block-style Markdown (`content[].text`) AND a typed JSON object (`structured_content`). Text channel byte-identical with v4.7.1.

**v4.8 — full coverage on `tools/list[].outputSchema`.** All 21 tools (or 20 without `port_forward`) now advertise a typed JSON Schema on `tools/list`. The schemas live as Rust structs in `src/infra/mcp/results.rs`:

| Tool | Result struct |
|:---|:---|
| `ssh_connect` | `SshConnectResult` (with `SessionEntry` for `matches`) |
| `ssh_disconnect` | `SshDisconnectResult` |
| `ssh_disconnect_many` | `SshDisconnectManyResult` |
| `ssh_list_sessions` | `SshListSessionsResult` (with `SessionEntry`) |
| `ssh_disconnect_agent` | `SshDisconnectAgentResult` |
| `ssh_execute` | `SshExecuteResult` |
| `ssh_execute_batch` | `SshExecuteBatchResult` |
| `ssh_run` | `SshRunResult` |
| `ssh_get_command_output` | `SshGetCommandOutputResult` |
| `ssh_list_commands` | `SshListCommandsResult` (with `CommandEntry`) |
| `ssh_cancel_command` | `SshCancelCommandResult` |
| `ssh_shell_open` | `SshShellOpenResult` (with optional `initial_buffer`) |
| `ssh_shell_write` | `SshShellWriteResult` |
| `ssh_shell_send_key` | `SshShellSendKeyResult` |
| `ssh_shell_read` | `SshShellReadResult` |
| `ssh_shell_wait_for` | `SshShellWaitForResult` |
| `ssh_shell_close` | `SshShellCloseResult` |
| `ssh_upload` | `SshUploadResult` |
| `ssh_download` | `SshDownloadResult` |
| `ssh_get_transfer_progress` | `SshGetTransferProgressResult` |
| `ssh_forward` *(feature `port_forward`)* | `SshForwardResult` |

Each struct is `#[non_exhaustive]` so callers cannot match exhaustively across versions; new optional fields can be added without bumping the major version. Optional fields use `#[serde(skip_serializing_if = "Option::is_none")]` so absent values are not surfaced as JSON `null` on the wire.

Full canonical example shapes per tool — including `ssh_run`, `ssh_execute_batch`, `ssh_disconnect_many` — live in [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47). Error shape on every tool: `{ tool, status: "error", code, reason, detail }` (when the source repo has live entries, `detail` carries the v4.7 NOT_FOUND closest-match suggestion). Reference: `src/infra/mcp/helpers/structured.rs` (dual-channel render) + `src/infra/mcp/suggestions.rs` (Levenshtein picker).

---

## Resource templates / Progress / Prompts / Idempotency (v4.7)

- **Resource templates** — `resources/templates/list` advertises 4 RFC 6570 URI shapes without `port_forward`, 5 with. Full payload + MIME table in [RESOURCES.md - Resource Templates (v4.7)](./RESOURCES.md#resource-templates-v47).
- **Progress notifications** — when a request includes `_meta.progressToken`, the server fires periodic `notifications/progress` updates during `ssh_get_command_output(wait=true)` (5 s cadence), `ssh_get_transfer_progress(wait=true)` (5 s), and `ssh_shell_wait_for` (1 s). Payload `{progress_token, progress, total, message}`. Best-effort — transport errors swallowed. See [LLM_GUIDE.md section L](./LLM_GUIDE.md#l-progress-notifications-v47). Reference: `src/infra/mcp/progress.rs::ProgressEmitter`.
- **Prompts catalog** — `prompts/list` advertises 5 canonical workflows; `prompts/get` returns a parameterised tool-sequence recipe. See [LLM_GUIDE.md section M](./LLM_GUIDE.md#m-prompts-catalog-v47). Reference: `src/infra/mcp/prompts.rs`.
- **Idempotency** — mutating tools (15 total) accept `_meta.idempotency_key` (1..=256 bytes). Cached response replays within the TTL window (default 300 s, env `SSH_IDEMPOTENCY_TTL_SECS`; cap 1024 entries, env `SSH_IDEMPOTENCY_MAX_ENTRIES`). Read-only tools ignore the key. Oversized keys raise `IDEMPOTENCY_KEY_TOO_LONG`. See [LLM_GUIDE.md → Idempotency](./LLM_GUIDE.md#idempotency) + [OPERATIONS.md → v4.7 idempotency error](./OPERATIONS.md#v47-idempotency-error). Reference: `src/infra/mcp/idempotency.rs`.

---

## NEXT: advisory coverage matrix (v4.6)

Every response with a clear successor tool ends with `NEXT: <pipe-separated tool calls>`. Per-tool sections above document the literal hints; the full coverage matrix lives in [LLM_GUIDE.md section E](./LLM_GUIDE.md#e-next-advisory-line-v46). Terminal statuses (`COMPLETED`, `CLOSED`, `CANCELLED`, `NOOP`, etc.) deliberately omit `NEXT:`. Reference: `src/infra/mcp/render/{connection,execute,shell,sftp,forward}.rs::next_hint_for_*`.

## Cost hints and JSON Schema defaults (v4.6)

Every tool description ends with a single-line `Cost:` hint (O() + latency + blocking/async). Optional `Option<T>` fields whose doc comment cites a default emit the JSON Schema `default` keyword via `#[schemars(default = "fn_name")]`. Full coverage in [LLM_GUIDE.md sections H + I](./LLM_GUIDE.md#h-json-schema-defaults-v46). Reference: `src/infra/mcp/tool_router.rs` + `src/infra/mcp/args/{connection,execute,shell,sftp}.rs`.

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
