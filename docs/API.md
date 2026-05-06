# SSH MCP API Reference (v6.0)

Complete API reference for the **36 MCP tools** (or 35 without `port_forward`) split across three semantic eixos — **`ssh_*`** (21), **`sub_*`** (9), **`serial_*`** (6) — the **6 resource subscribe schemes** (`shell` · `command` · `transfer` · `session` · `forward` · `serial`), and the v4.7 inter-tool conversation surface (`structured_content` JSON channel, `resources/templates/list`, `notifications/progress`, `prompts/*` catalog, idempotency cache, NOT_FOUND closest-match suggestions, `INITIAL_BUFFER` line) exposed by the v6.0 ssh-mcp server (rmcp 1.6, protocol `V_2025_06_18`). Text channel is byte-compatible with v3.0.0 / v4.x / v5.x on every legacy tool; v5.0 added 9 net-new `sub_*` tools, v5.2 added 6 native `serial_*` tools ([ADR 0009](./adr/0009-serial-transport.md)), and v6.0 only renames tool strings (resource URIs / push narrative / error taxonomy / structured content payloads carry forward unchanged). v4.8 lifted typed `output_schema` advertisement to all carry-over tools; v5.x extends it to every new tool. See [ARCHITECTURE.md](./ARCHITECTURE.md) and [MIGRATION.md → v5 → v6](./MIGRATION.md#v5--v6).

> **v6.0 — wire-breaking on tool name strings only.** Resource URI schemes, `_meta` envelope, structured-content schemas, error taxonomy (38 codes), and env vars are byte-identical to v5.3.x. Apply the sed snippet in [MIGRATION.md → v5 → v6](./MIGRATION.md#v5--v6) and ship.

> **v4.7 conversation surface.** Every tool emits Markdown + typed JSON (`structured_content`). v4.8 expanded `output_schema` advertisement from 9 / 21 to 21 / 21 tools. Errors render as `{ tool, status: "error", code, reason, detail }` on the structured channel. See [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47).

> **v4.7 new tools — `ssh_run`, `ssh_exec_batch`, `ssh_disconnect_many`.** Tool count moves from 18 to 21 (or 17 to 20 without `port_forward`). Per-tool sections below.

> **v4.6 wire change** — `AGENT:` -> `AGENT_ID:` (7 render sites). Hosts that grep `^AGENT:` literally must update; generic key-value parsers unaffected.

> **Subscribe-first** — prefer `sub_open uri=<scheme>://<id>/<sub-path>` over long-poll `wait=true`. The legacy `resources/subscribe` JSON-RPC method still works (v3 → v6 wire-compatible) but `sub_open` returns a `SUB_ID` you can drive (`sub_pause`, `sub_filter`, `sub_replay`, `sub_close`). `HINT:` + `NEXT:` lines reinforce this on every async-spawn response. See [LLM_GUIDE.md](./LLM_GUIDE.md).

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
| `OK` | most write/lifecycle tools (`ssh_connect`, `ssh_disconnect`, `ssh_disconnect_agent`, `ssh_sessions`, `ssh_commands`, `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_press`, `ssh_shell_close`, `ssh_forward`) | Success — see body for IDs. |
| `REUSED` | `ssh_connect` | Existing healthy session returned. |
| `SUGGESTED` | `ssh_connect` | Matching session(s) exist; LLM picks one or retries with `force_new`. |
| `STARTED` | `ssh_exec`, `ssh_upload`, `ssh_download` | Background work kicked off. |
| `RUNNING` | `ssh_exec_output`, `ssh_transfer_progress` | Background work still in progress; output marked `(partial)`. |
| `COMPLETED` | `ssh_exec_output`, `ssh_transfer_progress` | Background work finished. |
| `TIMEOUT` | `ssh_exec_output`, `ssh_shell_read.wait`, `ssh_shell_wait_for` | Long-poll deadline expired or `wait_for` matched no pattern. |
| `FAILED` | `ssh_transfer_progress` | Transfer terminated with an error; `REASON:` line carries detail. |
| `CANCELLED` | `ssh_exec_cancel` | Work cancelled by caller; partial output included. |
| `NOOP` | `ssh_exec_cancel` | Idempotent cancel — command was not running. |
| `OPEN` / `CLOSED` | `ssh_shell_read` | Shell state during read. |
| `MATCHED` | `ssh_shell_wait_for` | Pattern hit. |
| `ERROR` | every tool that can fail | See `REASON` / `DETAIL`. |

### Output blocks

`ssh_exec_output` and `ssh_exec_cancel` emit `stdout` and `stderr` blocks. `ssh_shell_read` and `ssh_shell_wait_for` emit a single `data` block. Empty blocks render as `--- stdout [a3f2b1d7] (empty) ---`. Truncation marks the delimiter with `(partial, truncated: showing 16.0KB of 2.3MB)`. Content is UTF-8 safely truncated to the **tail** (most recent bytes) when `max_output_bytes` is exceeded.

### Capability handshake

`McpSshServer::get_info()` advertises tools (`listChanged: true`), resources (`subscribe: true`, `listChanged: true`), protocol `V_2025_06_18`, and the v4.5 `Implementation` identity (title / description / website_url) — plus the v4.6 `icons` entry — and a few-shot `instructions` block. See [Capability handshake (full payload)](#capability-handshake-1) below for the wire shape.

---

## Tools (39 with `port_forward`, 38 without)

**v7.0 / ADR 0011** adds three live tools (`ssh_rsync`, `ssh_rsync_cancel`, `ssh_rsync_stats`) on top of the v6.x carry-over surface. Three semantic eixos: `ssh_*` (24 tools — was 21), `sub_*` (9 tools), `serial_*` (6 tools).

v5.2 adds **6 serial / UART / TTY / COM tools** (ADR 0009): `serial_open`, `serial_close`, `serial_write`, `serial_press`, `serial_scan`, `serial_active`. The legacy 21-tool surface is unchanged; existing v3 / v4 / v5.0 / v5.1 hosts continue to work without changes.

The catalogue below covers every tool. v5.2 adds Serial (6); v4.7 added `ssh_run`, `ssh_exec_batch`, `ssh_disconnect_many`. Groups: Connection (5), Commands (6), Shell (6), SFTP (3), **Serial (6, v5.2)**, Network (1, feature-gated), plus the 9 v5.1 subscription primitives.

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
NEXT: ssh_exec(session_id=a3f2b1d7-..., command=...) | ssh_shell_open(session_id=a3f2b1d7-...) | ssh_disconnect(session_id=a3f2b1d7-...)
```

`HOST` always renders as `username@host:port`. `AGENT_ID` (renamed from `AGENT:` in v4.6) is omitted when no `agent_id` was passed. `REPLACED: N` is appended when stale matches were purged before creating the session. `PERSISTENT: false` is followed by an `EXPIRES_AT:` RFC3339 deadline (= `connected_at + SSH_INACTIVITY_TIMEOUT`); ping the session before this fires (any cheap call works) to extend it. When the caller passes `persistent=true`, the response emits `PERSISTENT: true` and omits `EXPIRES_AT`. The trailing `NEXT:` line (v4.6) lists the three most-likely successor calls pre-filled with the freshly minted `SESSION_ID`.

**Response — REUSED**:
```
SSH_CONNECT: REUSED
SESSION_ID: a3f2b1d7-...
HOST: alice@example.com:22
AGENT_ID: claude-code-instance-abc123
NEXT: ssh_exec(session_id=a3f2b1d7-..., command=...) | ssh_shell_open(session_id=a3f2b1d7-...) | ssh_disconnect(session_id=a3f2b1d7-...)
```

`RETRY`, `PERSISTENT`, and `EXPIRES_AT` are omitted on `REUSED` (the original connect already set them; query `ssh_sessions` to refresh).

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

When more than 5 healthy sessions are owned by the same `agent_id`, an extra anti-leak hint is appended on `SUGGESTED` and on `ssh_sessions`:

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

### ssh_sessions

List active SSH sessions with health-check metadata.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `agent_id` | `string?` | — | Filter to a single `AGENT_ID`. Omit to list every agent. |
| `max_items` | `usize?` | `500` | Cap entries (env `SSH_MCP_LIST_MAX_ITEMS`, hard cap `SSH_MCP_LIST_MAX_ITEMS_CAP=10000`). |

The tool runs an `echo 1` health probe against every candidate session and removes any that fail before returning. Each successful probe also fires a `HealthEvent::Healthy` so subscribers of `session://<id>/health` see a fresh tick.

**Status values**: `OK`.

**Response**:
```
SSH_SESSIONS: OK
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

### ssh_exec

Execute a shell command asynchronously. Returns immediately with a `COMMAND_ID`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `command` | `string` | — | Shell command to run on the remote host. |
| `timeout_secs` | `u64?` | `180` | Command timeout (env `SSH_COMMAND_TIMEOUT`). |
| `pty` | `bool?` | `false` | Allocate a PTY for the command (e.g. `sudo`, `top`). All output merges to stdout in PTY mode. |
| `release_when_no_subs` | `bool?` | `false` | v5 lifecycle: when `true`, the `command://<id>/output` resource auto-closes after the last `sub_close` ([ADR 0003](./adr/0003-lifecycle-binding.md)). Default `false` preserves v4 manual-cancel semantics. |
| `grace_ms` | `u32?` | `2000` | v5 lifecycle: grace window (ms) between last `sub_close` and `Closed` when `release_when_no_subs=true`. Per-call only — no env-var override. |

Limits: up to 100 concurrent multiplexed commands per session.

**Status values**: `STARTED`, `ERROR`.

**Response**:
```
SSH_EXEC: STARTED
COMMAND_ID: 7d4c8e2a-...
SESSION_ID: a3f2b1d7-...
AGENT_ID: claude-code-instance-abc123
HINT: subscribe to command://7d4c8e2a-.../output for realtime output (preferred over polling)
NEXT: ssh_exec_output(command_id=7d4c8e2a-..., wait=true) | ssh_exec_cancel(command_id=7d4c8e2a-...)
```

`AGENT_ID:` is omitted when the session has no agent. The v4.6 `HINT:` line steers the LLM to subscribe rather than poll; `NEXT:` lists the two successor calls pre-filled with `COMMAND_ID`.

**Errors**: `SESSION_NOT_FOUND`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_COMMANDS_EXCEEDED`, `TRANSPORT_ERROR` (untagged), `CHANNEL_FAILED` (tagged transport).

---

### ssh_exec_output

Read the current output and status of an async command.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command_id` | `string` | — | `COMMAND_ID` from `ssh_exec`. |
| `wait` | `bool?` | `false` | Block until completion or timeout. |
| `wait_timeout_secs` | `u64?` | `30` | Long-poll deadline; cap `300`. |
| `max_output_bytes` | `usize?` | `16384` | Hard cap `1 048 576` (env `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`). Tail (most recent) preserved. |

**Status values**: `RUNNING`, `COMPLETED`, `TIMEOUT`, `ERROR`.

**Response — COMPLETED**:
```
SSH_EXEC_OUTPUT: COMPLETED
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
SSH_EXEC_OUTPUT: RUNNING
COMMAND_ID: 7d4c8e2a-...
--- stdout [a3f2b1d7] (partial) ---
... bytes so far ...
--- stderr [a3f2b1d7] (empty) ---
NEXT: sub_open uri=command://7d4c8e2a-.../output (PREFERRED — push-first) | ssh_exec_output(command_id=7d4c8e2a-..., wait=true)
```

The v4.6 `NEXT:` on `RUNNING` steers toward subscribe-first push or a single long-poll instead of a tight polling loop.

**Errors**: `COMMAND_NOT_FOUND`, `COMMAND_FAILED`.

**Wire codes**: `COMMAND_NOT_FOUND`, `COMMAND_FAILED` (tagged from transport, e.g. exec channel died).

---

### ssh_commands

List async commands across one or all sessions.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string?` | — | Filter to a single session. |
| `status` | `"running" \| "completed" \| "cancelled" \| "failed"?` | — | Filter by status (typed enum — typos rejected at deserialisation). |
| `max_items` | `usize?` | `500` | Cap entries; hard cap `10 000`. |

**Response**:
```
SSH_COMMANDS: OK
COUNT: 2
- 7d4c8e2a-... [COMPLETED] a3f2b1d7-...: ls -la (18:00:00)
- 9f8e7d6c-... [RUNNING] a3f2b1d7-...: tail -f /var/log/syslog (18:01:00)
```

Each item is `<COMMAND_ID> [<STATUS>] <SESSION_ID>: <command> (HH:MM:SS)`. STATUS uses uppercase (`RUNNING`, `COMPLETED`, `CANCELLED`, `FAILED`). Trailing parenthesis carries the started-at time-of-day.

**Wire codes**: `STORAGE_ERROR` (none on the happy path).

---

### ssh_exec_cancel

Cancel a running async command.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command_id` | `string` | — | `COMMAND_ID`. |
| `max_output_bytes` | `usize?` | `16384` | Hard cap `1 048 576`. Returns partial stdout/stderr collected before cancellation. |

**Status values**: `CANCELLED`, `NOOP`, `ERROR`.

**Response — CANCELLED**:
```
SSH_EXEC_CANCEL: CANCELLED
COMMAND_ID: 9f8e7d6c-...
--- stdout [b2e7c9d1] (partial) ---
last line before cancel
--- stderr [b2e7c9d1] (empty) ---
```

**Errors**: `COMMAND_NOT_FOUND`.

**Wire codes**: `COMMAND_NOT_FOUND`.

---

### ssh_run (v4.7)

One-shot orchestration of `ssh_connect` + `ssh_exec(wait=true)` + (optional) `ssh_disconnect`. Avoids the three-round-trip `connect -> execute -> wait` choreography for short atomic commands like `uptime`, `hostname`, `cat /etc/release`. The session is minted (or reused via `reuse=auto`) under the hood.

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
| `disconnect_after` | `bool?` | `true` | Disconnect the session after the command finishes (one-shot mode). Set `false` to keep the session open for follow-up `ssh_exec` calls. |

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

### ssh_exec_batch (v4.7)

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
SSH_EXEC_BATCH: HALTED
SESSION_ID: a3f2b1d7-...
TOTAL: 3
EXECUTED: 2
- [0] mkdir /tmp/foo: COMPLETED exit=0
- [1] tar -xzf bundle.tgz -C /tmp/foo: FAILED exit=2
- [2] chown -R svc /tmp/foo: SKIPPED
```

Each `results[]` entry carries its own `command_id`, `exit_code`, stdout/stderr blocks, and (optional) `error` string.

**structured_content shape**: `{ tool: "ssh_exec_batch", status, session_id, total, executed, results: [{ index, command, status: "completed|failed|timeout|cancelled|skipped", command_id?, exit_code?, stdout, stderr, stdout_truncated, stderr_truncated, timed_out, error? }, ...] }`. Full schema in [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47). Idempotency: pass `_meta.idempotency_key` to dedup retried batches.

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
| `release_when_no_subs` | `bool?` | `false` | v5 lifecycle: when `true`, the `shell://<id>/output` resource auto-closes after the last `sub_close` ([ADR 0003](./adr/0003-lifecycle-binding.md)). Default `false` preserves v4 manual-close semantics. |
| `grace_ms` | `u32?` | `2000` | v5 lifecycle: grace window (ms) between last `sub_close` and `Closed` when `release_when_no_subs=true`. Per-call only — no env-var override. |

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
NEXT: sub_open uri=shell://4b9c8e2a-.../output (PREFERRED — push-first) | ssh_shell_write | ssh_shell_press
```

`TERM` carries the terminal type and the geometry on a single line (`<term> <cols>x<rows>`). `AGENT_ID:` (renamed from `AGENT:` in v4.6) is omitted when no agent owns the session. The v4.6 `HINT:` steers toward push notifications; `NEXT:` names the three successor calls.

**v4.7 `INITIAL_BUFFER:` line.** When the PTY emits stdout within `SSH_SHELL_OPEN_INITIAL_PEEK_MS` (default 100 ms; tick `SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS` default 5 ms) of the open call, the response embeds a single `INITIAL_BUFFER:` line with the head-truncated bytes (cap `SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES`, default 4 KiB). CR/LF escaped to `\r`/`\n`. Structured twin emits `initial_buffer`. Omitted when no stdout arrived within the budget. Smaller LLMs sometimes skip the first `resources/read` round-trip when the prompt is already visible. Reference: `src/infra/mcp/render/shell.rs::shell_open_render_with_initial`. See [LLM_GUIDE.md section O](./LLM_GUIDE.md#o-initial_buffer-on-ssh_shell_open-v47).

**Errors**: `SESSION_NOT_FOUND`, `MAX_SHELLS_EXCEEDED`, `CHANNEL_FAILED`, `TRANSPORT_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_SHELLS_EXCEEDED`, `CHANNEL_FAILED` (tagged transport — typically remote `MaxSessions` exhaustion), `TRANSPORT_ERROR` (untagged).

---

### ssh_shell_write

Send raw bytes to an interactive shell. Use `ssh_shell_press` for named keystrokes whenever possible.

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

### ssh_shell_press

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
SSH_SHELL_PRESS: OK
SHELL_ID: 4b9c8e2a-...
KEY: ctrl_c
REPEAT: 1
BYTES_SENT: 1
NEXT: resources/read shell://4b9c8e2a-.../output?cursor=auto | ssh_shell_wait_for | ssh_shell_read
```

**Response — modified key**:
```
SSH_SHELL_PRESS: OK
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
NEXT: ssh_shell_write(shell_id=4b9c8e2a-..., ...) | ssh_shell_press(shell_id=4b9c8e2a-..., ...) | ssh_shell_close(shell_id=4b9c8e2a-...)
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
| `release_when_no_subs` | `bool?` | `false` | v5 lifecycle: when `true`, the `transfer://<id>/progress` resource auto-closes after the last `sub_close` ([ADR 0003](./adr/0003-lifecycle-binding.md)). |
| `grace_ms` | `u32?` | `2000` | v5 lifecycle: grace window (ms) before auto-release fires. Per-call only — no env-var override. |

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
NEXT: ssh_transfer_progress(transfer_id=8f7e6d5c-..., wait=true)
```

`FROM` is the source (local for upload, remote for download); `TO` is the destination. `SIZE` is the human-readable + raw byte count; `BYTES` is the raw count again. `AGENT_ID:` (renamed from `AGENT:` in v4.6) is omitted when the session has no agent. v4.6 `HINT:` steers toward subscribe; `NEXT:` names the long-poll fallback.

**Errors**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `LOCAL_FILE_ERROR`, `LOCAL_NOT_FILE`, `SFTP_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `LOCAL_FILE_ERROR` (tagged SFTP — `fs::metadata` failed), `LOCAL_NOT_FILE` (live in v4.6 — emitted from `application/upload_file.rs::guard_local_path_is_file` when the local path resolves but is not a regular file), `SFTP_ERROR` (untagged catch-all).

#### v6.1 / ADR 0010 — resume + verify

Two opt-in flags layer onto `ssh_upload`. Defaults preserve v6.0 byte-for-byte behaviour; both fields are wire-additive (`#[serde(default)]`).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `resume` | `bool?` | `false` | Pre-flight remote size; resume from the first non-overlapping byte. When `false`, every upload truncates the destination (v6.0 semantics). |
| `verify` | `bool?` | `false` | When `resume = true`, hash the resume prefix on both sides (sha256) and abort with `RESUME_MISMATCH` on divergence. When `false`, the prefix is trusted verbatim. |

Resumed responses gain one extra line:

```
SSH_UPLOAD: STARTED
TRANSFER_ID: 8f7e6d5c-...
SESSION_ID: a3f2b1d7-...
FROM: /home/alice/data.tar.gz
TO: /var/incoming/data.tar.gz
SIZE: 5.0 GB (5368709120 bytes)
BYTES: 5368709120
RESUMED_FROM: 4294967296          # NEW — emitted only when resume offset > 0
HINT: subscribe to transfer://8f7e6d5c-.../progress for realtime progress
NEXT: sub_open uri=transfer://8f7e6d5c-.../progress
```

`RESUMED_FROM:` is suppressed for fresh transfers (offset `0`) so v6.0 callers see byte-identical wires. The structured payload always carries `"resumed_from": <u64>`.

**Skip plan** — when `resume = true` and remote size already equals local size, the transfer reaches `Completed` synchronously: `bytes_transferred = total_bytes`, `resumed_from = total_bytes`, no progress events emitted. Subscribers connecting after the call get a single replay event.

**New error codes (extends [ADR 0007](./adr/0007-error-taxonomy.md))**:

| Code | Category | Retry | Detail |
|---|---|---|---|
| `RESUME_OVERSHOOT` | `STATE` | no | Destination is larger than source; refusing to resume. Re-run with `resume=false` to overwrite. |
| `RESUME_MISMATCH` | `STATE` | no | Resume prefix hash mismatch (local vs remote diverged); re-run with `resume=false` to overwrite, or fix the partial file. |

`verify = true` adds one extra `ssh_exec` round-trip plus O(offset) bytes hashed remotely (`sha256sum -b -- '<remote>' 2>/dev/null`). For a 10 GiB transfer interrupted at 4 GiB, the prefix hash takes 30–60 s on a 1 Gbps disk. Default-off keeps the fast path fast.

**mtime-drift caveat** — when the local file is modified between the original failure and the resume call, the resume prefix bytes diverge. With `verify = false` the destination is silently corrupt; `verify = true` is the supported guard.

---

### ssh_download

Download a remote file to a local path via SFTP. Streams in 32 KiB chunks.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | `string` | — | `SESSION_ID`. |
| `remote_path` | `string` | — | Remote source. |
| `local_path` | `string` | — | Local destination. Relative paths resolve against `$HOME`. |
| `release_when_no_subs` | `bool?` | `false` | v5 lifecycle: when `true`, the `transfer://<id>/progress` resource auto-closes after the last `sub_close` ([ADR 0003](./adr/0003-lifecycle-binding.md)). |
| `grace_ms` | `u32?` | `2000` | v5 lifecycle: grace window (ms) before auto-release fires. Per-call only — no env-var override. |

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
NEXT: ssh_transfer_progress(transfer_id=1a2b3c4d-..., wait=true)
```

Same shape as upload — `FROM` is the remote source, `TO` is the local destination. `AGENT_ID:`, `HINT:`, and `NEXT:` lines mirror upload.

**Errors**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `SFTP_OPEN_FAILED`, `REMOTE_METADATA_ERROR`, `SFTP_ERROR`.

**Wire codes**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `SFTP_OPEN_FAILED` (tagged SFTP), `REMOTE_METADATA_ERROR` (live in v4.6 — emitted from `adapters/sftp/russh_sftp_adapter.rs::stat_remote_size` when the remote stat call fails), `SFTP_ERROR` (untagged catch-all).

#### v6.1 / ADR 0010 — resume + verify

Mirror of the upload contract with directions swapped. Two opt-in flags; defaults preserve v6.0 behaviour.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `resume` | `bool?` | `false` | Pre-flight local size; resume the download from the first non-overlapping byte. When `false`, every download truncates the local destination (v6.0 semantics). |
| `verify` | `bool?` | `false` | When `resume = true`, hash the resume prefix on both sides (sha256 over `dd if='<remote>' bs=1 count=<offset>` on the remote) and abort with `RESUME_MISMATCH` on divergence. |

Resumed responses gain a `RESUMED_FROM:` line identical in shape to `ssh_upload` (suppressed when offset is `0`). Skip plan, structured payload, and the `RESUME_OVERSHOOT` / `RESUME_MISMATCH` error codes mirror the upload direction. See the upload section above for the full contract.

---

### ssh_transfer_progress

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
SSH_TRANSFER_PROGRESS: RUNNING
TRANSFER_ID: 8f7e6d5c-...
DIRECTION: UPLOAD
PROGRESS: 47% (1153024/2412544 bytes)
NEXT: sub_open uri=transfer://8f7e6d5c-.../progress (PREFERRED — push-first) | ssh_transfer_progress(transfer_id=8f7e6d5c-..., wait=true)
```

**Response — COMPLETED**:
```
SSH_TRANSFER_PROGRESS: COMPLETED
TRANSFER_ID: 8f7e6d5c-...
DIRECTION: UPLOAD
PROGRESS: 100% (2412544/2412544 bytes)
```

**Response — FAILED**:
```
SSH_TRANSFER_PROGRESS: FAILED
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

## Serial (6, v5.2 — ADR 0009)

Native UART / TTY / COM transport. Lock-free reader / writer split: `ArcSwap<RingBuffer>` history (subscribers slice `O(1)`, no contention with the OS reader); writes funnel through a bounded `mpsc::channel(64)` and surface `SERIAL_BACKPRESSURE` instead of stalling. Producer hooks into the same `SUBSCRIPTION_REGISTRY` debouncer as `command://*/output`, including the ADR 0006 Amendment 1 byte-threshold flush. Subscribe via `sub_open uri=serial://<SERIAL_ID>/output`.

### serial_open (v5.2)

Open a serial port and start its lock-free reader / writer tasks. Returns `SERIAL_ID` + the subscribe URI. Full `stty` parameter coverage:

| Field | Type | Default | Notes |
|---|---|---|---|
| `path` | string | required | `/dev/ttyUSB0` / `/dev/ttyACM0`, `/dev/tty.usbserial-XXXX`, `COM3`, … |
| `baud_rate` | u32 | `115_200` | 9 600 / 19 200 / 38 400 / 57 600 / 115 200 / 230 400 / 460 800 / 921 600 |
| `data_bits` | u8 | `8` | 5 / 6 / 7 / 8 |
| `stop_bits` | string | `"1"` | `"1"` / `"2"` |
| `parity` | string | `"none"` | `"none"` / `"odd"` / `"even"` (also `"N"` / `"O"` / `"E"`) |
| `flow_control` | string | `"none"` | `"none"` / `"software"` (also `"xon/xoff"`) / `"hardware"` (also `"rts/cts"`) |
| `read_timeout_ms` | u64 | `100` | OS read timeout |
| `max_buffer_size` | u64 | `0` (= 1 MiB) | history cap; once full, head-truncates on append |
| `initial_dtr` | bool? | `null` | `null` = driver default, `true` = raise, `false` = lower |
| `initial_rts` | bool? | `null` | same semantics as `initial_dtr` |
| `label` | string? | `null` | optional human label surfaced on `serial_active` |

**Status values:** OK.
**Errors:** INVALID_ARGUMENT (bad data_bits / parity / flow_control / stop_bits), SERIAL_ERROR (device busy / missing / permission denied / kernel control-line refusal).
**Cost:** 1 OS open + 2 spawned tasks; typically <10 ms.
**structured_content shape:** `{ tool: "serial_open", status, serial_id, path, baud_rate, data_bits, stop_bits, parity, flow_control, uri, next: [...] }`.

### serial_close (v5.2)

Cancel reader / writer tasks for the given `SERIAL_ID` and remove the registry entry. Idempotent — second call returns `NOOP`.

**Status values:** OK, NOOP.
**Cost:** O(1).

### serial_write (v5.2)

Enqueue bytes for transmission. Pass either `text` (UTF-8) OR `bytes_base64` (RFC 4648 standard alphabet). Newlines are NOT auto-appended; use `serial_press` for `enter` / `cr` / `lf` / `crlf`.

After the write, the response arrives via push on the existing `serial://<id>/output` subscription. Wait for `notifications/resources/updated` and drain via `resources/read?cursor=auto` — never poll.

**Status values:** OK.
**Errors:** INVALID_ARGUMENT (no payload OR both supplied OR malformed base64), SERIAL_NOT_FOUND, SERIAL_BACKPRESSURE (write queue full — local sleep + retry).
**Cost:** O(input.len). Sub-ms typical.

### serial_press (v5.2)

Send a named control keystroke without crafting bytes manually. Optional `repeat` 1..=64.

| Key | Bytes |
|---|---|
| `enter` / `cr` | `\r` |
| `lf` | `\n` |
| `crlf` | `\r\n` |
| `esc` | `\x1b` |
| `tab` | `\t` |
| `backspace` | `\x08` |
| `ctrl_c` | `\x03` |
| `ctrl_d` | `\x04` |
| `ctrl_z` | `\x1a` |

**Status values:** OK.
**Errors:** INVALID_ARGUMENT (unknown key), SERIAL_NOT_FOUND, SERIAL_BACKPRESSURE.
**Cost:** O(repeat).

### serial_scan (v5.2)

Snapshot the OS-visible serial devices. Returns the device paths the caller can pass to `serial_open`. No state mutation.

**Status values:** OK.
**Cost:** 1 OS enumeration (typically <5 ms).

### serial_active (v5.2)

Snapshot every serial port currently held by this process. Returns `SERIAL_ID`, path, baud rate, optional label, and the subscribe URI for each. No state mutation.

**Status values:** OK.
**Cost:** O(N) over the per-process registry.

## Rsync (3, v7.0 — ADR 0011)

Two-tier transport behind a single `ssh_rsync` tool, both running in-process inside the host binary. `transport=Wire` is a canonical port of OpenBSD `openrsync` (BSD/ISC) speaking rsync wire protocol v31 against a remote `rsync --server`. `transport=Sftp` is the universal SFTP fallback driving plain `readdir` + `stat` + `read` + `write` + `setstat`. `transport=Auto` probes the remote and prefers Wire when rsync >= 3.2.0 is present, otherwise routes to Sftp.

**Status (v7.0.0)**: both transports are live for the supported feature set. Push and pull are byte-identical against `rsync 3.2.7` on a real Linux VM (six wire e2e tests in `tests/v7_rsync_wire_e2e_vm.rs`, gated `e2e-vm`). Slices 11–12 (`-c` checksum delta over the wire-format extension and `-H` hardlinks) are deferred — both surface `SFTP_FEATURE_MISSING` today; successor ADR will scope. See [ADR 0011 → Final slice status](./adr/0011-rsync-hybrid-transport.md#final-slice-status) for the per-layer table.

**When to use which transport**:

- **`transport=Sftp`** — pick when the remote has no rsync, when `ssh_upload` already works against the host, or when you only need a recursive deploy with `--delete` + dry-run + exclude/include + attribute preservation. SFTP path copies whole blocks; `bytes_skipped` only fires on whole-file size+mtime matches.
- **`transport=Wire`** — pick when you need delta-sync of large append-only files (logs, tarballs, RDB dumps), when you want the rolling-checksum block-match path to collapse unchanged blocks, or when the SFTP server refuses `SSH_FXP_SETSTAT` / `SSH_FXP_SYMLINK`. Requires rsync >= 3.2.0 on the remote.
- **`transport=Auto`** (default) — probe + prefer Wire when v31+ is available. Most callers should leave the default in place.

### SFTP capability gates

Before driving the sync, the use case probes the remote SFTP server and caches the result for the lifetime of the session. The probe runs three round-trips (mkdir + setstat + symlink) under `/tmp/.ssh-mcp-probe-<session>`, then maps the outcome to a [`SftpFeatures`](../src/adapters/rsync/sftp/probe.rs) snapshot. The use case fires three capability gates:

| Caller request | Probe result | Outcome |
|---|---|---|
| `opts.preserve.symlinks=true` | `symlink_supported=false` | `SFTP_FEATURE_MISSING` — "Remote SFTP server does not support symlink op; pass preserve.symlinks=false or use transport=Wire" |
| `opts.preserve.perms=true` | `setstat_supported=false` | `SFTP_FEATURE_MISSING` — "Remote SFTP server does not support setstat; pass preserve.perms=false or use transport=Wire" |
| `opts.preserve.hardlinks=true` | (always — Wire-only) | `SFTP_FEATURE_MISSING` — "hardlink preservation needs transport=Wire; rerun with transport=Wire or drop preserve.hardlinks" |
| `opts.verify_checksum=true` | (always — Wire-only) | `SFTP_FEATURE_MISSING` — "delta-sync needs transport=Wire; rerun with transport=Wire or drop verify_checksum / delta_sync" |

The probe is cached per-session in a `DashMap<SessionId, SftpFeatures>` inside the use case, so repeated `ssh_rsync` calls against the same SSH session pay the round-trips only once.

### ssh_rsync

Start a recursive sync between a local path and a remote path. Returns immediately with `STARTED`; progress streams via the `rsync://<id>/progress` push lane.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | string | — | `SESSION_ID` from `ssh_connect`. |
| `src` | string | — | Source path. Either a plain local path or a `host:` prefixed remote path. Exactly one of `src` / `dst` must be remote. |
| `dst` | string | — | Destination path. Same shape as `src`. |
| `opts.recursive` | bool | `false` | `-r` — recurse into directories. |
| `opts.archive` | bool | `false` | `-a` — alias for `-rlptgoD` (pre-empts `recursive` and `preserve` when set). |
| `opts.delete` | bool | `false` | `--delete` — remove dst paths missing from src. |
| `opts.exclude` | `[string]` | `[]` | gitignore-style exclude patterns. |
| `opts.include` | `[string]` | `[]` | gitignore-style include patterns (override `exclude` on match). |
| `opts.dry_run` | bool | `false` | `-n` — emit `FileSkipped { reason: DryRun }` per would-be op; no destructive side-effects. |
| `opts.bwlimit_kbps` | `u64?` | `null` | Token-bucket bandwidth cap on the writer side (60 ms tick granularity on the Wire path; SFTP path uses the same envelope around its `write` calls). |
| `opts.compress` | bool | `false` | `-z` — request rsync-side compression (Wire path only; SFTP path ignores). |
| `opts.partial` | bool | `false` | `--partial` — inherits ADR 0010 resume semantics. Default `false` truncates the dst on retry. |
| `opts.verify_checksum` | bool | `false` | `-c` — force a content checksum even when size + mtime match. **Wire-only**; the SFTP path returns `SFTP_FEATURE_MISSING` when this flag is set. |
| `opts.preserve.perms` | bool | `true` | `-p` — preserve POSIX mode bits. |
| `opts.preserve.mtime` | bool | `true` | `-t` — preserve modification time. |
| `opts.preserve.owner` | bool | `true` | `-o` — preserve numeric owner (root only on remote). |
| `opts.preserve.group` | bool | `true` | `-g` — preserve numeric group. |
| `opts.preserve.links` | bool | `true` | `-l` — preserve symlinks as-is. |
| `opts.preserve.hardlinks` | bool | `false` | `-H` — preserve the hard-link graph. **Wire-only**; the SFTP path returns `SFTP_FEATURE_MISSING` when this flag is set. |
| `opts.preserve.sparse` | bool | `false` | `-S` — preserve sparse holes. |
| `opts.preserve.devices` | bool | `false` | `-D` — preserve devices, fifos, sockets (root only). |
| `transport` | enum (`auto` / `wire` / `sftp`) | `auto` | Auto probes; prefer wire-compat (rsync v31+); fall back to SFTP. `wire` forces the wire-compat client (returns `RSYNC_VERSION_TOO_OLD` if rsync is missing or older than v3.2.0). `sftp` skips the probe and uses the universal SFTP fallback. |
| `release_when_no_subs` | `bool?` | `false` | ADR 0003 lifecycle binding — auto-release the rsync session when the last subscriber detaches. |

**Response — STARTED**:
```
SSH_RSYNC: STARTED
SESSION_ID: 8f2b3c4a-...
RSYNC_ID: 01902e76-9f1a-7c3d-bd9b-f5a30c7df0ab
TRANSPORT: sftp
SUBSCRIBE: rsync://01902e76-.../progress

NEXT: sub_open uri=rsync://01902e76-.../progress | ssh_rsync_stats rsync_id=01902e76-... | ssh_rsync_cancel rsync_id=01902e76-...
```

`TRANSPORT` is `wire` or `sftp`. The structured-content payload mirrors the wire shape: `{ tool: "ssh_rsync", status: "started", session_id, rsync_id, transport, files_planned, bytes_planned }`. `files_planned` / `bytes_planned` are `0` until the transport's list-end frame lands; subscribe to `rsync://<id>/progress` to observe them.

**Errors**: `SESSION_NOT_FOUND`, `MAX_TRANSFERS_EXCEEDED`, `RSYNC_NOT_FOUND`, `RSYNC_VERSION_TOO_OLD`, `RSYNC_PROTOCOL_ERROR`, `RSYNC_FILE_LIST_TOO_LARGE`, `RSYNC_PARTIAL_TRANSFER`, `SFTP_FEATURE_MISSING`. Each error response carries a one-sentence `DETAIL:` line tuned for direct LLM consumption — see [LLM_GUIDE.md → Error handbook](./LLM_GUIDE.md#error-handbook).

---

### ssh_rsync_cancel

Cancel a live rsync session. Idempotent: cancelling an already-terminated session is a no-op. Drives `RsyncSession::cancel()` (CAS to `Cancelled`) and tears down the russh channel through the `RsyncTransportPort::close` path.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `rsync_id` | string | — | `RSYNC_ID` from `ssh_rsync`. |

**Response — OK**:
```
SSH_RSYNC_CANCEL: OK
RSYNC_ID: 01902e76-9f1a-7c3d-bd9b-f5a30c7df0ab
```

**Errors**: `RSYNC_NOT_FOUND`. Surfaces as `Internal` with `RSYNC_NOT_FOUND` tag in the detail line.

---

### ssh_rsync_stats

Read the live aggregate counters from a running rsync session. Pure read; mirrors `ssh_transfer_progress` semantics.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `rsync_id` | string | — | `RSYNC_ID` from `ssh_rsync`. |

**Response — OK**:
```
SSH_RSYNC_STATS: OK
RSYNC_ID: 01902e76-9f1a-7c3d-bd9b-f5a30c7df0ab
STATUS: running
FILES_TOTAL: 1248
FILES_DONE: 412
BYTES_TOTAL: 314572800
BYTES_TRANSFERRED: 102993920
BYTES_SKIPPED: 0
FILES_DELETED: 0
FILES_FAILED: 0
```

**Status values**: `pending`, `probing`, `running`, `completed`, `failed`, `cancelled`. Terminal statuses (`completed` / `failed` / `cancelled`) omit the `NEXT:` advisory.

**Errors**: `RSYNC_NOT_FOUND`.

---

## Subscription administration (9, v5 — ADR 0004 / 0005)

The 9 `sub_*` tools manage per-`SubId` lanes (Channel Mux v5 Phase 2). They work cross-resource: any of the six push schemes (`shell://`, `command://`, `transfer://`, `session://`, `forward://`, `serial://`) can be opened, paused, filtered, replayed, listed, or statted through the same verb-uniform surface. Wire idempotent for retries via `_meta.idempotency_key` (mutating tools only — `sub_list` / `sub_stats` / `sub_stats_all` are pure reads).

### sub_open

Open a Channel Mux lane for a resource URI. Returns `SUB_ID` (UUIDv7) + the bound URI.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `uri` | string | — | Resource URI (`shell://<id>/output`, `command://<id>/output`, `transfer://<id>/progress`, `session://<id>/health`, `forward://<id>/events`, `serial://<id>/output`). |
| `lifetime` | enum (`manual` / `auto_close` / `lease`) | `manual` | `manual` keeps the lane until explicit `sub_close`; `auto_close` releases after `grace_ms` of consumer inactivity; `lease` enforces a hard `ttl_secs` cap regardless of activity. |
| `grace_ms` | `u32?` | `2000` | Honoured only when `lifetime=auto_close`. Grace window after consumer inactivity before the lane releases. |
| `ttl_secs` | `u32?` | — | Honoured only when `lifetime=lease`. Hard TTL on the lane regardless of activity. |
| `lag_policy` | enum (`block_slow` / `drop_oldest` / `drop_newest` / `snapshot`) | env `SSH_LAG_POLICY_DEFAULT` (`snapshot`) | Lane backpressure policy. |
| `filter` | string? | `null` | Optional regex string compiled at subscribe time; empty / absent means no filter. Char cap: `SSH_FILTER_REGEX_MAX` (default 1024). |

**Response**:
```
SUB_OPEN: OK
SUB_ID: <uuidv7>
URI: <uri>
LIFETIME: manual|auto_close|lease
LAG_POLICY: block_slow|drop_oldest|drop_newest|snapshot
HINT: REQUIRED: track this SUB_ID; sub_close when done.
NEXT: sub_close sub_id=<uuidv7> | sub_stats sub_id=<uuidv7>
```

**Errors**: `RESOURCE_GONE`, `LANE_LIMIT_PER_URI`, `LANE_LIMIT_TOTAL`, `FILTER_INVALID`, `INVALID_ARGUMENT`.

**Cost**: O(1) lane open + per-event mpsc thereafter. **Idempotency**: `_meta.idempotency_key` supported.

### sub_close

Close a lane by `SUB_ID`. Triggers grace timer if last subscriber and `release_when_no_subs=true`.

| Field | Type | Description |
|-------|------|-------------|
| `sub_id` | string | `SUB_ID` from `sub_open`. |

**Response**: `SUB_CLOSE: OK` or `SUB_CLOSE: NOT_FOUND` (idempotent).

**Errors**: `INVALID_ARGUMENT` (malformed `sub_id`).

**Idempotency**: closing a non-existent lane returns `NOT_FOUND` instead of an error.

### sub_pause

Suspend the lane drain loop. Producer keeps emitting; the mpsc fills under the lane's lag policy.

| Field | Type | Description |
|-------|------|-------------|
| `sub_id` | string | Target lane. |

**Response**: `SUB_PAUSE: OK`. **Errors**: `SUB_NOT_FOUND`.

### sub_resume

Resume the drain loop after `sub_pause`.

| Field | Type | Description |
|-------|------|-------------|
| `sub_id` | string | Target lane. |

**Response**: `SUB_RESUME: OK`. **Errors**: `SUB_NOT_FOUND`.

### sub_filter

Hot-reload the lane's regex filter. Empty string clears the filter (forwards every event).

| Field | Type | Description |
|-------|------|-------------|
| `sub_id` | string | Target lane. |
| `regex` | string | New regex pattern. Empty string clears the filter. Char cap: `SSH_FILTER_REGEX_MAX` (default 1024). |

**Response**: `SUB_FILTER: OK`. **Errors**: `SUB_NOT_FOUND`, `FILTER_INVALID`.

### sub_replay

Replay events from a cursor (byte offset) within the lane's ring-buffer window.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `sub_id` | string | — | Target lane. |
| `from_cursor` | `u64?` | `0` | Cursor (byte offset) to replay from. `0` replays from the start of the available ring-buffer window. |

**Response**: `SUB_REPLAY: OK\nBYTES: <N>`. **Errors**: `SUB_NOT_FOUND`, `RING_BUFFER_OVERFLOW`.

### sub_list

List every lane in scope. Optional URI prefix and peer-id filters (peer-id reserved for forward-compat).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `uri_prefix` | string? | `null` | Filter to lanes whose canonical URI starts with this prefix. |
| `peer_id` | string? | `null` | Reserved — currently a no-op placeholder. |

**Response**: `SUB_LIST: OK` plus per-lane rows (`SUB_ID`, `URI`, `LIFETIME`, `LAG_POLICY`, `events_sent`, `bytes_sent`). **Cost**: O(N) over the lane registry.

### sub_stats

Per-lane atomic counter snapshot.

| Field | Type | Description |
|-------|------|-------------|
| `sub_id` | string | Target lane. |

**Response**: `SUB_STATS: OK` with `events_sent`, `bytes_sent`, `lag_drops`, `queue_depth`, `last_event_at`. **Errors**: `SUB_NOT_FOUND`.

### sub_stats_all

Aggregate counters across every lane in scope. No arguments today (the schema reserves a `_reserved` field for forward-compat).

**Response**: `SUB_STATS_ALL: OK` with totals (`lanes_total`, `events_sent`, `bytes_sent`, `lag_drops`, `queue_depth_max`).

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
NEXT: sub_open uri=forward://fwd-1/events (PREFERRED — push-first)
```

`FORWARD_ID` + `SESSION_ID` (added in v4.5) let callers construct the `forward://<FORWARD_ID>/events` URI without round-tripping `resources/list`. v4.6 `HINT:` and `NEXT:` reinforce subscribe-first.

**Errors**: `SESSION_NOT_FOUND`, `FORWARD_FAILED`, `FEATURE_DISABLED`, `PORT_IN_USE`.

**Wire codes**: `SESSION_NOT_FOUND`, `PORT_IN_USE`, `FEATURE_DISABLED` (tagged invalid argument — emitted only on the no-`port_forward` build when subscribing to `forward://`), `FORWARD_FAILED` (live in v4.6 — emitted from `application/forward_port.rs::ForwardPortUseCase::preflight_bind` when the local listener bind fails for reasons other than `AddrInUse`).

---

## Resources (6 schemes)

URIs follow `<scheme>://<id>/<sub-path>[?cursor=auto|<N>|0]`. The `?cursor=` query string is silently ignored on point-in-time resources (`transfer://`, `session://`).

| Scheme | URI | MIME | Cursor | Producer |
|--------|-----|------|--------|----------|
| `shell` | `shell://<shell_id>/output` | `text/plain` | yes | PTY reader (`RunningShell`) |
| `command` | `command://<command_id>/output` | `text/plain` | yes | Async command reader (`RunningCommand`) |
| `transfer` | `transfer://<transfer_id>/progress` | `application/json` | no | SFTP loop (`RunningTransfer`) |
| `session` | `session://<session_id>/health` | `application/json` | no | Health probes / connect reuse path |
| `forward` | `forward://<forward_id>/events` | `application/json` | yes | Forward task (feature-gated) |
| **`serial`** | **`serial://<serial_id>/output`** | `text/plain` | **yes** | **Serial reader (`SerialPortState`, v5.2 — ADR 0009)** |

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
  "instructions": "SSH MCP. 36 tools, 6 push streams (shell://, command://, transfer://, session://, forward://, serial://). All tools return block markdown (KEY: value, --- name [nonce] ---) + a typed JSON in structured_content. IDs end in _ID. NEXT: line lists successor tools.\n\nHappy paths (PREFERRED — keep sessions alive, never re-handshake):\n1) Run async (DEFAULT): ssh_connect (agent_id, reuse=Auto). Then ssh_exec. Then sub_open command://<id>/output for push. sub_close when done. Reuse the SESSION_ID for every follow-up call against this host.\n2) Interactive shell: ssh_connect, ssh_shell_open (returns INITIAL_BUFFER if the prompt arrives within 100ms). Then sub_open shell://<id>/output. Drive with ssh_shell_write or ssh_shell_press. Read deltas via resources/read?cursor=auto on each notification. ssh_shell_close, ssh_disconnect.\n3) Upload: ssh_upload. Then sub_open transfer://<id>/progress, OR ssh_transfer_progress wait=true.\n4) Local serial (no SSH): serial_open path=/dev/ttyUSB0 baud_rate=115200. Then sub_open serial://<id>/output for push. Drive with serial_write or serial_press. serial_close when done.\n5) PENALIZED FALLBACK — ssh_run(address, username, command): only when you will NEVER touch this host again. Pays a full handshake every call and tears the session down. Two ssh_run calls cost as much as one ssh_connect + two ssh_exec calls. Default to path 1.\n\nCleanup: agent_id on connect, ssh_disconnect_agent for bulk-close. Watch HINT lines and EXPIRES_AT. Pass _meta.idempotency_key on retries to dedup."
}
```

The build without `port_forward` advertises `35 tools, 5 push streams (shell://, command://, transfer://, session://, serial://)` instead.

`Implementation.icons` is wired in v4.6 to a single hosted SVG entry (`https://raw.githubusercontent.com/farchanjo/ssh-mcp/master/assets/icon.svg`, `image/svg+xml`, `sizes=["any"]`). Source: `assets/icon.svg`. Implementation: `src/infra/mcp/tool_router.rs::build_implementation`.

Each of the 36 tools (or 35 without `port_forward`) carries a `Tool.title` plus `ToolAnnotations.{read_only_hint, destructive_hint, idempotent_hint}`. See [LLM_GUIDE.md section C](./LLM_GUIDE.md#c-server-identity-for-the-host-v45-icon-wired-in-v46) for the matrix. v4.7 also advertises `prompts/list` (10 entries — 5 v4 carry-overs + 5 v5 push-first per ADR 0005) and `resources/templates/list` (5 / 6 entries depending on `port_forward`).

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
| `ssh_sessions` | `SshSessionsResult` (with `SessionEntry`) |
| `ssh_disconnect_agent` | `SshDisconnectAgentResult` |
| `ssh_exec` | `SshExecResult` |
| `ssh_exec_batch` | `SshExecBatchResult` |
| `ssh_run` | `SshRunResult` |
| `ssh_exec_output` | `SshExecOutputResult` |
| `ssh_commands` | `SshCommandsResult` (with `CommandEntry`) |
| `ssh_exec_cancel` | `SshExecCancelResult` |
| `ssh_shell_open` | `SshShellOpenResult` (with optional `initial_buffer`) |
| `ssh_shell_write` | `SshShellWriteResult` |
| `ssh_shell_press` | `SshShellPressResult` |
| `ssh_shell_read` | `SshShellReadResult` |
| `ssh_shell_wait_for` | `SshShellWaitForResult` |
| `ssh_shell_close` | `SshShellCloseResult` |
| `ssh_upload` | `SshUploadResult` |
| `ssh_download` | `SshDownloadResult` |
| `ssh_transfer_progress` | `SshTransferProgressResult` |
| `ssh_forward` *(feature `port_forward`)* | `SshForwardResult` |

Each struct is `#[non_exhaustive]` so callers cannot match exhaustively across versions; new optional fields can be added without bumping the major version. Optional fields use `#[serde(skip_serializing_if = "Option::is_none")]` so absent values are not surfaced as JSON `null` on the wire.

Full canonical example shapes per tool — including `ssh_run`, `ssh_exec_batch`, `ssh_disconnect_many` — live in [LLM_GUIDE.md section K](./LLM_GUIDE.md#k-structured_content-channel-v47). Error shape on every tool: `{ tool, status: "error", code, reason, detail }` (when the source repo has live entries, `detail` carries the v4.7 NOT_FOUND closest-match suggestion). Reference: `src/infra/mcp/helpers/structured.rs` (dual-channel render) + `src/infra/mcp/suggestions.rs` (Levenshtein picker).

---

## Resource templates / Progress / Prompts / Idempotency (v4.7)

- **Resource templates** — `resources/templates/list` advertises 4 RFC 6570 URI shapes without `port_forward`, 5 with. Full payload + MIME table in [RESOURCES.md - Resource Templates (v4.7)](./RESOURCES.md#resource-templates-v47).
- **Progress notifications** — when a request includes `_meta.progressToken`, the server fires periodic `notifications/progress` updates during `ssh_exec_output(wait=true)` (5 s cadence), `ssh_transfer_progress(wait=true)` (5 s), and `ssh_shell_wait_for` (1 s). Payload `{progress_token, progress, total, message}`. Best-effort — transport errors swallowed. See [LLM_GUIDE.md section L](./LLM_GUIDE.md#l-progress-notifications-v47). Reference: `src/infra/mcp/progress.rs::ProgressEmitter`.
- **Prompts catalog** — `prompts/list` advertises 5 canonical workflows; `prompts/get` returns a parameterised tool-sequence recipe. See [LLM_GUIDE.md section M](./LLM_GUIDE.md#m-prompts-catalog-v47). Reference: `src/infra/mcp/prompts.rs`.
- **Idempotency** — mutating tools (15 total) accept `_meta.idempotency_key` (1..=256 bytes). Cached response replays within the TTL window (default 300 s, env `SSH_IDEMPOTENCY_TTL_SECS`; cap 1024 entries, env `SSH_IDEMPOTENCY_MAX_ENTRIES`). Read-only tools ignore the key. Oversized keys raise `IDEMPOTENCY_KEY_TOO_LONG`. See [LLM_GUIDE.md → Idempotency](./LLM_GUIDE.md#idempotency) + [OPERATIONS.md → v4.7 idempotency error](./OPERATIONS.md#v47-idempotency-error). Reference: `src/infra/mcp/idempotency.rs`.

---

## NEXT: advisory coverage matrix (v4.6)

Every response with a clear successor tool ends with `NEXT: <pipe-separated tool calls>`. Per-tool sections above document the literal hints; the full coverage matrix lives in [LLM_GUIDE.md section E](./LLM_GUIDE.md#e-next-advisory-line-v46). Terminal statuses (`COMPLETED`, `CLOSED`, `CANCELLED`, `NOOP`, etc.) deliberately omit `NEXT:`. Reference: `src/infra/mcp/render/{connection,execute,shell,sftp,forward}.rs::next_hint_for_*`.

## Cost hints and JSON Schema defaults (v4.6)

Every tool description ends with a single-line `Cost:` hint (O() + latency + blocking/async). Optional `Option<T>` fields whose doc comment cites a default emit the JSON Schema `default` keyword via `#[schemars(default = "fn_name")]`. Full coverage in [LLM_GUIDE.md sections H + I](./LLM_GUIDE.md#h-json-schema-defaults-v46). Reference: `src/infra/mcp/tool_router.rs` + `src/infra/mcp/args/{connection,execute,shell,sftp}.rs`.

## Cross-reference — keyboard input

Use the table below to choose between `ssh_shell_press` and `ssh_shell_write`:

| Goal | Tool | Notes |
|------|------|-------|
| Interrupt running command | `ssh_shell_press key=ctrl_c` | Preferred. |
| Submit Enter | `ssh_shell_press key=enter` | Or `ssh_shell_write input="\n"`. |
| Type a command line | `ssh_shell_write input="ls -la\n"` | `send_key` rejects bulk text. |
| Arrow key navigation | `ssh_shell_press key=arrow_up` | Modifiers allowed. |
| Function keys | `ssh_shell_press key=f5` | Modifiers allowed. |
| Back-tab in completion menu | `ssh_shell_press key=tab shift=true` | Tab accepts Shift only. |
| Send Ctrl+Shift+End in a TUI | `ssh_shell_press key=end shift=true ctrl=true` | Modifier rule honored. |
| Send Alt+B (word back in readline) | `ssh_shell_write input="\x1bb"` | `send_key` does not expose every escape; raw bytes when needed. |
