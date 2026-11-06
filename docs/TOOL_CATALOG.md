# Tool catalogue (generated)

Exported from a live `tools/list` handshake by `scripts/export_tool_catalog.py`. Regenerate with `make tools-export`.

- Server: `ssh-mcp` 7.1.2
- Tools: 39
- Parameter descriptions: 141

| Tool | Title | Params | Description bytes |
| --- | --- | --- | --- |
| `serial_active` | List currently open serial ports | 0 | 886 |
| `serial_close` | Close a serial port | 1 | 953 |
| `serial_open` | Open a serial port | 11 | 1585 |
| `serial_press` | Send a named keystroke to a serial port | 3 | 1458 |
| `serial_scan` | List OS-visible serial devices | 0 | 923 |
| `serial_write` | Write to a serial port | 3 | 1502 |
| `ssh_commands` | List async commands | 3 | 676 |
| `ssh_connect` | Connect to SSH server | 13 | 1708 |
| `ssh_disconnect` | Disconnect SSH session | 1 | 1218 |
| `ssh_disconnect_agent` | Disconnect all agent sessions | 1 | 675 |
| `ssh_disconnect_many` | Disconnect multiple SSH sessions | 1 | 759 |
| `ssh_download` | Download file via SFTP | 7 | 1668 |
| `ssh_exec` | Run remote command | 6 | 1349 |
| `ssh_exec_batch` | Execute a batch of commands on one session | 6 | 1174 |
| `ssh_exec_cancel` | Cancel running command | 2 | 886 |
| `ssh_exec_output` | Get command output | 4 | 1024 |
| `ssh_forward` | Forward TCP port | 4 | 1299 |
| `ssh_rsync` | Sync a directory tree over SSH (rsync) | 6 | 2336 |
| `ssh_rsync_cancel` | Cancel an in-flight rsync session | 1 | 875 |
| `ssh_rsync_stats` | Get rsync session stats | 1 | 1101 |
| `ssh_run` | Run remote command (one-shot) | 10 | 1901 |
| `ssh_sessions` | List SSH sessions | 2 | 694 |
| `ssh_shell_close` | Close PTY shell | 1 | 1108 |
| `ssh_shell_open` | Open PTY shell | 8 | 1501 |
| `ssh_shell_press` | Send keystroke to PTY | 6 | 1670 |
| `ssh_shell_read` | Read PTY buffer | 6 | 1445 |
| `ssh_shell_wait_for` | Wait for shell pattern | 5 | 1573 |
| `ssh_shell_write` | Write to PTY shell | 2 | 1671 |
| `ssh_transfer_progress` | Get transfer progress | 3 | 1221 |
| `ssh_upload` | Upload file via SFTP | 7 | 1787 |
| `sub_close` | Unsubscribe from a resource lane | 1 | 1408 |
| `sub_filter` | Hot-reload a subscription filter | 2 | 1200 |
| `sub_list` | List active subscription lanes | 2 | 913 |
| `sub_open` | Subscribe to a resource lane | 7 | 2557 |
| `sub_pause` | Pause a subscription lane | 1 | 1172 |
| `sub_replay` | Replay a subscription lane from cursor | 2 | 1276 |
| `sub_resume` | Resume a subscription lane | 1 | 947 |
| `sub_stats` | Snapshot subscription lane stats | 1 | 1039 |
| `sub_stats_all` | Aggregate daemon-wide subscription stats | 0 | 871 |

## `serial_active`

**Title:** List currently open serial ports

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Snapshot every serial port currently held by this process. Returns SERIAL_ID, path, baud_rate, optional label, and the subscribe URI (`serial://<SERIAL_ID>/output`) for each. No state mutation.

When to use:
- Inventory before reconnect / cleanup decisions.
- Pair with sub_list to correlate active ports with their subscription lanes.
- Detect SUB_LEAK_RISK candidates (active port without a corresponding lane in sub_list).

Workflow:
1. serial_active (no args; pure registry snapshot).
2. Cross-reference returned SERIAL_IDs with sub_list to find leaked ports.
3. serial_close per SERIAL_ID for cleanup.

Overlap guidance:
- Prefer serial_active for in-process port inventory. For OS-level discovery (paths not yet opened), use serial_scan.
- For sub-lane status across all schemes, use sub_list.

Status values: OK.

Errors: STORAGE_ERROR.

Cost: O(N) over the per-process registry.

### Parameters

No parameters.

## `serial_close`

**Title:** Close a serial port

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Cancel reader / writer tasks for the given SERIAL_ID, drop the lock-free buffers, release the OS handle, and remove the registry entry. Cascades to any open serial:// subscription lanes (they emit a final `closed` event before unsubscribing).

When to use:
- Releasing a UART / TTY / COM port allocated by serial_open.
- Idempotent — a second call returns NOOP.

Workflow:
1. Optional: sub_close any serial://<SERIAL_ID>/output lanes first (or rely on cascade).
2. serial_close serial_id=...
3. Open lanes emit a final `closed` event before unsubscribing.

Overlap guidance:
- Prefer serial_close to release a single port. For an inventory-driven cleanup, iterate serial_active and call serial_close on each SERIAL_ID.
- A second serial_close returns NOOP — safe to call defensively.

Status values: OK, NOOP.

Errors: SERIAL_NOT_FOUND (tolerate — already closed).

Cost: O(1) + 1 OS close().

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `serial_id` | `string` | yes | — | `SERIAL_ID` returned by `serial_open`. |

## `serial_open`

**Title:** Open a serial port

**Annotations:** destructiveHint=False, idempotentHint=False, readOnlyHint=False

### Description

Open a UART / TTY / COM serial port and start its lock-free reader / writer tasks. The returned SERIAL_ID can be passed to sub_open uri=serial://<SERIAL_ID>/output for push delivery (same debouncer + 64 KiB byte-threshold flush as shell:// / command://).

When to use:
- Driving an embedded board, GPS, RS-485 sensor, modem, or debug UART from an LLM host.
- Streaming bursty serial output without polling.

Full configuration: path, baud_rate (default 115200), data_bits (5..=8, default 8), stop_bits (1|2, default 1), parity (none|odd|even, default none), flow_control (none|software|hardware, default none), read_timeout_ms (default 100), max_buffer_size (0 = adapter default 1 MiB), initial_dtr / initial_rts (None = driver default), label.

Workflow:
1. Optional: serial_scan to discover available paths.
2. serial_open path=... baud_rate=... (returns SERIAL_ID).
3. sub_open uri=serial://<SERIAL_ID>/output (PREFERRED — push delivery).
4. Drive with serial_write / serial_press; await pushes; drain via cursor.
5. serial_close + sub_close on completion.

Overlap guidance:
- Prefer serial_open for direct local UART/TTY/COM. To talk to a remote host's USB-serial, use ssh_connect + ssh_exec on a remote `cu` / `screen` / `picocom` instead — serial_open is local-only.
- For port discovery before opening, run serial_scan.

Status values: OK.

Errors: INVALID_ARGUMENT (bad data_bits / parity / flow_control / stop_bits), SERIAL_ERROR (device busy / missing / permission denied / kernel control-line refusal).

Cost: 1 open() + 2 spawned tasks (reader + writer); typically <10 ms.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `baud_rate` | `integer` | no | — | Baud rate (default `115_200`). Common values: 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600. |
| `data_bits` | `integer` | no | — | Data bits (`5`, `6`, `7`, `8`). Default `8`. |
| `flow_control` | `string` | no | — | Flow control — accepts `"none"`, `"software"`/`"xon/xoff"`, `"hardware"`/`"rts/cts"`. Default `"none"`. |
| `initial_dtr` | `['boolean', 'null']` | no | — | Initial DTR (Data Terminal Ready) line. `None` leaves the driver default; `Some(true)` raises, `Some(false)` lowers.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: null (driver default). |
| `initial_rts` | `['boolean', 'null']` | no | — | Initial RTS (Request To Send) line. Same semantics as `initial_dtr`.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: null (driver default). |
| `label` | `['string', 'null']` | no | — | Optional human label (e.g. `"GPS-1"`) surfaced on `serial_active`. |
| `max_buffer_size` | `integer` | no | — | Maximum history buffer (bytes). `0` falls back to the adapter default (1 MiB). Default `0`. |
| `parity` | `string` | no | — | Parity — accepts `"none"`/`"N"`, `"odd"`/`"O"`, `"even"`/`"E"`. Default `"none"`. |
| `path` | `string` | yes | — | OS device path. Linux: `/dev/ttyUSB0` / `/dev/ttyACM0`. macOS: `/dev/tty.usbserial-XXXX`. Windows: `COM3`. |
| `read_timeout_ms` | `integer` | no | — | Per-read timeout in milliseconds. Default `100`. |
| `stop_bits` | `string` | no | — | Stop bits — accepts `"1"` or `"2"`. Default `"1"`. |

## `serial_press`

**Title:** Send a named keystroke to a serial port

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

### Description

Send a named control keystroke without crafting the bytes manually. Accepted: `enter`/`cr` (\r), `lf` (\n), `crlf` (\r\n), `esc` (\x1b), `tab` (\t), `backspace` (\x08), `ctrl_c` (\x03), `ctrl_d` (\x04), `ctrl_z` (\x1a). Optional `repeat` 1..=64.

When to use:
- Sending control keys to an interactive UART program (Ctrl+C to abort, Esc/Tab navigation, Enter to submit).
- Repeating a keystroke (e.g. `repeat=4` to send Backspace four times).
- Prefer serial_press over crafting raw byte sequences with serial_write.

Workflow:
1. serial_open path=... baud_rate=... (returns SERIAL_ID).
2. sub_open uri=serial://<SERIAL_ID>/output (PREFERRED — push delivery).
3. serial_press serial_id=... key=enter (or any accepted name).
4. Await `notifications/resources/updated` push; drain via `resources/read?cursor=auto`.
5. serial_close on completion.

Overlap guidance:
- Prefer serial_press for named keystrokes — typed UART command sequences belong in serial_write.
- For multi-byte sequences not in the accepted-key list, use serial_write bytes_base64=... instead.

After the keystroke — DO NOT poll. The response arrives via push on the existing `sub_open uri=serial://<SERIAL_ID>/output` subscription. If you have not subscribed yet, call `sub_open` FIRST. Wait for `notifications/resources/updated` and drain via `resources/read?cursor=auto`.

Status values: OK.

Errors: INVALID_ARGUMENT (unknown key), SERIAL_NOT_FOUND, SERIAL_BACKPRESSURE.

Cost: O(repeat).

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `key` | `string` | yes | — | Key name. Accepted: `"enter"` / `"cr"` (`\r`), `"lf"` (`\n`), `"crlf"` (`\r\n`), `"esc"` (`\x1b`), `"tab"` (`\t`), `"backspace"` (`\x08`), `"ctrl_c"` (`\x03`), `"ctrl_d"` (`\x04`), `"ctrl_z"` (`\x1a`). |
| `repeat` | `integer` | no | — | Repeat count. `1..=64`. Default `1`. |
| `serial_id` | `string` | yes | — | `SERIAL_ID` returned by `serial_open`. |

## `serial_scan`

**Title:** List OS-visible serial devices

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Snapshot the OS-visible serial devices. Returns `path` strings the caller can pass to serial_open. No state mutation.

When to use:
- Discover available UART / TTY / COM ports before serial_open.
- Diagnose missing-device cases (USB-serial unplugged, permission denied at the kernel layer).

Path format by OS:
- Linux: `/dev/ttyUSB*`, `/dev/ttyACM*`, `/dev/ttyS*`.
- macOS: `/dev/tty.usbserial-*`, `/dev/tty.usbmodem*`, `/dev/cu.*` (callout side preferred for clients).
- Windows: `COM1`, `COM2`, ...

Workflow:
1. serial_scan (no args; pure OS enumeration).
2. Pick a path; serial_open path=... baud_rate=...

Overlap guidance:
- Prefer serial_scan for discovery (OS-level). For an inventory of ports already opened by this process, use serial_active.
- serial_scan is read-only; safe to call repeatedly.

Status values: OK.

Errors: SERIAL_ERROR (rare — OS enumeration failed).

Cost: 1 OS enumeration (typically <5 ms).

### Parameters

No parameters.

## `serial_write`

**Title:** Write to a serial port

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

### Description

Enqueue bytes for transmission on a SERIAL_ID. Pass either `text` (UTF-8) OR `bytes_base64` (RFC 4648, for binary protocols). Newlines are NOT auto-appended; use serial_press for `enter`/`cr`/`lf`/`crlf`.

When to use:
- Submitting a typed command or text payload to a UART / TTY / COM device.
- Sending raw bytes (binary protocols) via `bytes_base64`.
- Prefer serial_press for named keystrokes (Ctrl+C, Enter, Esc, arrow keys) — saves crafting escape sequences.

Workflow:
1. serial_open path=... baud_rate=... (returns SERIAL_ID).
2. sub_open uri=serial://<SERIAL_ID>/output (PREFERRED — push delivery).
3. serial_write text=... (or bytes_base64=...).
4. Await `notifications/resources/updated` push; drain via `resources/read?cursor=auto`.
5. serial_close on completion.

Overlap guidance:
- Prefer serial_write for human-readable input + arbitrary bytes. Use serial_press for control keys (Ctrl+C, Enter alone, arrow keys).
- Pass exactly one of `text` / `bytes_base64` — both supplied is INVALID_ARGUMENT.

After the write — DO NOT poll. The response arrives via push on the existing `sub_open uri=serial://<SERIAL_ID>/output` subscription. If you have not subscribed yet, call `sub_open` FIRST. Wait for `notifications/resources/updated` and drain via `resources/read?cursor=auto`.

Status values: OK.

Errors: INVALID_ARGUMENT (no payload OR both supplied OR malformed base64), SERIAL_NOT_FOUND, SERIAL_BACKPRESSURE (write queue full — local sleep + retry).

Cost: O(input.len). Sub-ms typical.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `bytes_base64` | `['string', 'null']` | no | — | Base64-encoded raw bytes (binary protocols, packed frames). |
| `serial_id` | `string` | yes | — | `SERIAL_ID` returned by `serial_open`. |
| `text` | `['string', 'null']` | no | — | UTF-8 text payload. Newlines are NOT auto-appended; pass `"foo\n"` or use `serial_press` for `enter` / `cr` / `lf`. |

## `ssh_commands`

**Title:** List async commands

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

**Reused definitions (`$defs`):** CommandStatus

### Description

List asynchronous commands tracked on the server.

When to use:
- One-shot inventory of every command (optionally filtered by session and/or status).
- For RUNNING entries the response NEXT line steers you to `sub_open uri=command://<COMMAND_ID>/output` — strictly preferred over chained ssh_exec_output(wait=true) polling.

Workflow:
1. Optional `session_id` to narrow to one session.
2. Optional `status` filter (`running`, `completed`, `cancelled`, `failed`).
3. Optional `max_items` cap (default 500).
4. For each running command_id, open a push lane via sub_open before consuming output.

Status values: OK.

Errors: STORAGE_ERROR.

Cost: O(N) over async commands. Cheap.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `max_items` | `['integer', 'null']` | no | — | Maximum entries returned. Default: 500. Cap: 10000. Env: `SSH_MCP_LIST_MAX_ITEMS` / `SSH_MCP_LIST_MAX_ITEMS_CAP`. |
| `session_id` | `['string', 'null']` | no | — | `SESSION_ID` returned from `ssh_connect`. Optional filter; when omitted returns commands across all sessions. |
| `status` | `CommandStatus (via $ref) \| null` | no | `running`, `completed`, `cancelled`, `failed` | Filter by command status. Values: `running`, `completed`, `cancelled`, `failed`. When omitted returns every status. |

## `ssh_connect`

**Title:** Connect to SSH server

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=False

### Description

Connect to an SSH server and store the session.

When to use:
- Establishing a new SSH connection to run commands, open shells, or transfer files.
- Reusing an already-connected session by passing its `session_id`.

Important identifiers in response:
- `SESSION_ID`: passed to ssh_exec, ssh_shell_open, ssh_upload, ssh_download, ssh_disconnect, ssh_forward. ALSO subscribe via `sub_open uri=session://<SESSION_ID>/health` for live transport health pushes (PREFERRED over polling ssh_sessions).
- `AGENT_ID`: optional grouping; passed to ssh_sessions (filter) and ssh_disconnect_agent (cleanup).
- `EXPIRES_AT`: RFC3339 deadline when the session is auto-reaped by the inactivity sweeper. Ping (e.g. ssh_exec `: ` or any cheap call) before this fires to keep the session alive. Replaced by `PERSISTENT: true` when the caller opted out.

Workflow:
1. Call ssh_connect once per remote host.
2. Use the returned SESSION_ID for subsequent tool calls.
3. Call ssh_disconnect (or ssh_disconnect_agent) when done.

Tip: pass `reuse=auto` to let the server pick the most recent healthy match in a single round-trip. Use `reuse=suggest` (default) when you want to inspect matches before reusing. Use `reuse=force_new` to bypass identity matching entirely.
Tip: pass `agent_id` so subsequent sessions are grouped and you can bulk-cleanup with `ssh_disconnect_agent`. When `agent_id` is set, `reuse=auto`/`reuse=suggest` rank sessions owned by the same agent first.

Status values: OK, REUSED, SUGGESTED.

Errors: CONNECTION_FAILED, AUTH_FAILED.

Cost: 1 SSH handshake (typical 200-2000ms). Cheap to retry with reuse=auto.

Idempotency: pass `_meta.idempotency_key` to dedup retried calls within the v4.7-step5 cache TTL.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `address` | `string` | yes | — | SSH server address in the form `host:port` (e.g. `192.168.1.1:22`, `example.com:2222`). Port defaults to 22 when omitted. |
| `agent_id` | `['string', 'null']` | no | — | Optional `AGENT_ID` for grouping sessions (e.g. `host-instance-abc123`). Use `ssh_disconnect_agent` to bulk-disconnect all sessions for an agent. |
| `compress` | `['boolean', 'null']` | no | — | Enable zlib compression for the SSH transport. Default: true. Env: `SSH_COMPRESSION`.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: true. |
| `key_path` | `['string', 'null']` | no | — | Path to a private key file for key-based authentication. Optional. Authentication chain order: key -> password -> agent. |
| `max_retries` | `['integer', 'null']` | no | — | Maximum retry attempts for transient connection failures. Default: 3. Env: `SSH_MAX_RETRIES`. |
| `name` | `['string', 'null']` | no | — | Optional human-readable name for the session (e.g. `production-db`, `staging-server`). Surfaces in `ssh_sessions` to help disambiguate identical hosts. |
| `password` | `['string', 'null']` | no | — | Password for password-based authentication. Optional when `key_path` or an SSH agent (env `SSH_AUTH_SOCK`) is available. |
| `persistent` | `['boolean', 'null']` | no | — | Keep the session open indefinitely (disables inactivity timeout). Default: false. Set true for long-lived backends or daemons.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `retry_delay_ms` | `['integer', 'null']` | no | — | Initial delay between retries in milliseconds (exponential backoff, capped at 10s). Default: 1000. Env: `SSH_RETRY_DELAY_MS`. |
| `reuse` | `"suggest" \| "auto" \| "force_new" \| null` | no | `suggest`, `auto`, `force_new` | Reuse policy applied when an existing session matches the `(host, port, username)` identity triple. Values: `suggest` (default), `auto`, `force_new`. |
| `session_id` | `['string', 'null']` | no | — | Optional `SESSION_ID` returned by a previous `ssh_connect`. When provided and the session is still alive (health-check `echo 1`), short-circuits reuse evaluation and returns it as `REUSED`. |
| `timeout_secs` | `['integer', 'null']` | no | — | Connection timeout in seconds. Default: 30. Env: `SSH_CONNECT_TIMEOUT`. |
| `username` | `string` | yes | — | SSH username for authentication. |

## `ssh_disconnect`

**Title:** Disconnect SSH session

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Disconnect an SSH session. Cancels every async command, closes every PTY, aborts every in-flight SFTP transfer, tears down every port-forward listener, and decrements the SessionLifecycle refcount per ADR 0003 cascade.

When to use:
- Tearing down a single SSH session previously opened with ssh_connect.
- For BULK cleanup of an agent's sessions, prefer ssh_disconnect_agent (one call per agent_id).
- For multiple unrelated sessions, prefer ssh_disconnect_many (batched).

Workflow:
1. Pass the `session_id` returned from ssh_connect.
2. Subsequent tool calls against that id return SESSION_NOT_FOUND.
3. Open subscription lanes on child resources (shell://, command://) emit a final `closed` event before unsubscribing.

Lifecycle interaction: SessionLifecycle.active_refs is decremented once per child resource closed during teardown. The session reaper consults active_refs > 0 before honouring the inactivity TTL — refcount supersedes TTL.

Status values: OK, NOOP (already closed by reaper).

Errors: SESSION_NOT_FOUND, TRANSPORT_ERROR.

Cost: O(N) over child resources. Always succeeds locally; TRANSPORT_ERROR only on dead-transport teardown races.

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |

## `ssh_disconnect_agent`

**Title:** Disconnect all agent sessions

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Disconnect every session bound to a given agent.

When to use:
- Bulk-cleanup of every SSH session tagged with a given AGENT_ID.
- Cancels async commands, closes shells, and aborts transfers per disconnected session.

Overlap guidance:
- Sessions share an AGENT_ID (group cleanup): use this tool — single round-trip filter.
- Ad-hoc list of unrelated SESSION_IDs: prefer ssh_disconnect_many (up to 64 explicit ids).
- Single session: prefer ssh_disconnect.

Workflow:
1. Pass the AGENT_ID returned from a previous ssh_connect.
2. Sessions owned by other agents are not affected.

Status values: OK.

Errors: STORAGE_ERROR.

Cost: O(N) over agent sessions. Tens of ms typical.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `agent_id` | `string` | yes | — | `AGENT_ID` returned from `ssh_connect`. All sessions owned by this agent are disconnected; sessions owned by other agents are not affected. |

## `ssh_disconnect_many`

**Title:** Disconnect multiple SSH sessions

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Best-effort bulk disconnect of up to 64 sessions in a single call.

When to use:
- Cleaning up a fan-out of unrelated SESSION_IDs (no shared AGENT_ID).
- Per-id failures are reported in the response and do not abort the remaining disconnects.

Overlap guidance:
- Sessions share an AGENT_ID: prefer ssh_disconnect_agent — server-side filter, no id list needed.
- Single session: prefer ssh_disconnect.
- This tool: ad-hoc explicit list (mixed agents or no agent_id).

Workflow:
1. Pass the list of SESSION_IDs returned from prior ssh_connect calls.
2. Inspect the per-id `results` array for any `error` entries.

Status values: OK.

Errors: INVALID_ARGUMENT (empty / >64 ids).

Cost: O(N) disconnect calls. Best-effort: per-id failures do not abort the batch.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `session_ids` | `array` | yes | — | 1..=64 `SESSION_ID`s to disconnect. Per-id failures are reported in the response and do not abort the remaining disconnects. |

## `ssh_download`

**Title:** Download file via SFTP

**Annotations:** destructiveHint=False, idempotentHint=False, readOnlyHint=False

### Description

Download a remote file via SFTP.

When to use:
- Streaming a remote file to the local host in 32 KiB chunks.
- IMMEDIATELY after download start, call `sub_open uri=transfer://<TRANSFER_ID>/progress` for live byte-counter pushes — strictly preferred over polling ssh_transfer_progress(wait=true).
- Set `resume=true` (ADR 0010) to retry an interrupted download from the first non-overlapping byte; `RESUMED_FROM:` in the response reports the offset.
- Pair `resume=true` with `verify=true` to guard against silent prefix corruption.

Important identifiers in response:
- `TRANSFER_ID`: passed to ssh_transfer_progress (poll fallback) AND to `sub_open uri=transfer://<TRANSFER_ID>/progress` (PREFERRED — push delivery).
- `RESUMED_FROM`: byte offset the transfer resumed from (`0` for fresh transfers).

Workflow:
1. ssh_connect (returns SESSION_ID).
2. ssh_download session_id=... remote_path=... local_path=... [resume=...] [verify=...].
3. sub_open uri=transfer://<TRANSFER_ID>/progress (PREFERRED — push delivery).
4. Drain progress events via `notifications/resources/updated`; final event is terminal.
5. sub_close on completion.

Overlap guidance:
- Prefer ssh_download for a single file. For >5 files or directory-tree mirrors, prefer ssh_rsync (pull direction inferred from src=remote / dest=local).
- For polling fallback only, ssh_transfer_progress wait=true blocks until terminal.
- Pair resume=true + verify=true on flaky links to guard against silent prefix corruption.

Status values: STARTED.

Errors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_ERROR, RESUME_OVERSHOOT, RESUME_MISMATCH.

Cost: O(file.size). Returns immediately, transfer runs async.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `grace_ms` | `['integer', 'null']` | no | — | v5 Phase 3 — grace window in ms before auto-release fires. Default: 2000. |
| `local_path` | `string` | yes | — | Local destination path. Relative paths resolve against the home directory. |
| `release_when_no_subs` | `['boolean', 'null']` | no | — | v5 Phase 3 — auto-release when the transfer resource has zero subscribers. Default: false (legacy v4 behaviour).  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `remote_path` | `string` | yes | — | Remote file path to download from the SSH server. |
| `resume` | `['boolean', 'null']` | no | — | ADR 0010 — opt-in resume from the local tail. See [`SshUploadArgs::resume`] for the full contract.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |
| `verify` | `['boolean', 'null']` | no | — | ADR 0010 — when `resume=true`, hash the resume prefix on both sides and abort with `RESUME_MISMATCH` on divergence. See [`SshUploadArgs::verify`] for the cost trade-off.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |

## `ssh_exec`

**Title:** Run remote command

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

### Description

Spawn an asynchronous command on an SSH session.

When to use:
- Starting a command and observing its output. Prefer subscribing to `command://<COMMAND_ID>/output` over polling (see sub_open).
- Set `pty=true` for commands requiring a controlling terminal (e.g. sudo).

Important identifiers in response:
- `COMMAND_ID`: target of sub_open `command://<COMMAND_ID>/output` (push, preferred) or ssh_exec_output (poll fallback). Also accepted by ssh_exec_cancel.

Workflow (push-first, recommended):
1. Call ssh_exec with the SESSION_ID and command line.
2. sub_open uri=command://<COMMAND_ID>/output to stream stdout/stderr without polling.
3. sub_close sub_id=... when the command terminates (or set release_when_no_subs=true).
4. Optional ssh_exec_cancel to interrupt.

Workflow (poll fallback):
1. ssh_exec, then ssh_exec_output wait=true in a loop.

Overlap guidance:
- For atomic short commands, STILL prefer ssh_exec on a session opened with ssh_connect(reuse=auto). Avoid ssh_run unless you will never touch this host again — it pays a fresh handshake every call.
- For >=3 sequential commands on the same session, prefer ssh_exec_batch (saves ~70% wire vs N x ssh_exec).

Status values: STARTED.

Errors: SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED, TRANSPORT_ERROR.

Cost: 1 SSH channel open. Returns immediately when wait=false (default async).

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `command` | `string` | yes | — | Shell command to execute on the remote host. |
| `grace_ms` | `['integer', 'null']` | no | — | v5 Phase 3 — grace window in ms before auto-release fires. Default: 2000. |
| `pty` | `['boolean', 'null']` | no | — | Allocate a pseudo-terminal (PTY) for the command. Default: false. Use for commands that require a controlling terminal (e.g. `sudo`, `top`). All output is merged into stdout in PTY mode (no stderr separation).  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `release_when_no_subs` | `['boolean', 'null']` | no | — | v5 Phase 3 — auto-release when the command resource has zero subscribers. Default: false (legacy v4 behaviour).  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |
| `timeout_secs` | `['integer', 'null']` | no | — | Command timeout in seconds. Default: 180. Env: `SSH_COMMAND_TIMEOUT`. |

## `ssh_exec_batch`

**Title:** Execute a batch of commands on one session

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

### Description

Run up to 16 commands sequentially against a single session, with stop-on-failure semantics.

When to use:
- A small linear pipeline (`mkdir /tmp/foo`, `tar -xzf bundle.tgz -C /tmp/foo`, `chown -R svc /tmp/foo`).
- Short bursts where the round-trip cost of one ssh_exec per command dominates. Saves ~70% wire vs N x ssh_exec when N>=3.

Overlap guidance:
- Atomic single command on a host you will not revisit: ssh_run is the only justification, and even then only N=1. For any reuse, do ssh_connect(reuse=auto) + ssh_exec.
- Long-running streaming command: prefer ssh_exec + sub_open `command://<id>/output`.
- Truly parallel commands: open multiple ssh_exec (this batch is serial by design).

Workflow:
1. Each command runs synchronously with the per-command `timeout_secs_per_command` budget.
2. With `stop_on_failure=true` (default) the loop halts on the first non-zero exit code; remaining slots surface as `skipped`.
3. Each entry carries its own command_id, exit_code, stdout/stderr blocks.

Status values: OK, HALTED.

Errors: SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED, TRANSPORT_ERROR.

Cost: 1 SSH channel per command. Stops early on first non-zero exit by default.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `commands` | `array` | yes | — | Commands to execute, in order. 1..=16 entries. |
| `max_output_bytes_per_command` | `['integer', 'null']` | no | — | Per-command max bytes shown in stdout/stderr. Default: 16384. Cap: 1048576. |
| `pty` | `['boolean', 'null']` | no | — | Allocate a PTY for each command. Default: false.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |
| `stop_on_failure` | `['boolean', 'null']` | no | — | Halt the loop on the first non-zero exit code. Default: true.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: true. |
| `timeout_secs_per_command` | `['integer', 'null']` | no | — | Per-command wait timeout in seconds. Default: 30. Cap: 300. |

## `ssh_exec_cancel`

**Title:** Cancel running command

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Cancel an asynchronous command. Sends SIGINT (or closes the channel for non-PTY) and marks the command Cancelled.

When to use:
- Interrupting a long-running command spawned with ssh_exec.
- Returns the partial stdout/stderr captured so far at the time of cancel.

Push interaction: if a `command://<COMMAND_ID>/output` lane is open, the lane drains the partial buffer and emits a final `cancelled` status event before unsubscribing. The cursor at cancel is preserved for sub_replay.

Workflow:
1. ssh_exec_cancel command_id=<COMMAND_ID>.
2. Drain remaining lane events via the existing subscription (no extra polling needed).
3. sub_close when done.

Status values: CANCELLED, NOOP (already finished — terminal status preserved).

Errors: COMMAND_NOT_FOUND.

Cost: O(1). Always succeeds (NOOP for already-finished commands).

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `command_id` | `string` | yes | — | `COMMAND_ID` returned from `ssh_exec`. |
| `max_output_bytes` | `['integer', 'null']` | no | — | Maximum bytes shown in stdout/stderr. Default: 16384. Cap: 1048576. Content head-truncated; tail (most recent output) preserved. Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`. |

## `ssh_exec_output`

**Title:** Get command output

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Fetch the current output of an asynchronous command. Polling fallback path; prefer sub_open `command://<COMMAND_ID>/output` for streaming output without poll loops.

When to use:
- One-shot status check on a spawned command without opening a subscription.
- Hosts that lack `resources/subscribe` capability (subscribe is the canonical path otherwise).
- Optionally blocking until the command completes (`wait=true`).

Workflow:
1. Pass the COMMAND_ID returned from ssh_exec.
2. Set `wait=true` to block; capped at `wait_timeout_secs` (default 30, max 300).
3. `max_output_bytes` head-truncates very large outputs (default 16384).

Status values: RUNNING, COMPLETED, TIMEOUT, CANCELLED, FAILED.

Errors: COMMAND_NOT_FOUND.

Cost: O(buffer). Cheap with wait=false. With wait=true blocks up to wait_timeout_secs. Subscribe is cheaper for streams >1 update.

Progress: when `wait=true` and `_meta.progressToken` is set, mid-flight `notifications/progress` updates fire every ~5s with the running stdout byte count (best-effort).

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `command_id` | `string` | yes | — | `COMMAND_ID` returned from `ssh_exec`. |
| `max_output_bytes` | `['integer', 'null']` | no | — | Maximum bytes shown in stdout/stderr. Default: 16384. Cap: 1048576. Content head-truncated; tail (most recent output) preserved. Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`. |
| `wait` | `['boolean', 'null']` | no | — | Block until completion or `wait_timeout_secs` expires. Default: false.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `wait_timeout_secs` | `['integer', 'null']` | no | — | Maximum seconds to block when `wait=true`. Default: 30. Cap: 300. |

## `ssh_forward`

**Title:** Forward TCP port

**Annotations:** destructiveHint=False, idempotentHint=False, readOnlyHint=False

### Description

Set up a TCP port forwarder backed by an SSH session.

When to use:
- Tunnelling local TCP traffic over the SSH transport to a remote host:port.
- Available only when the `port_forward` Cargo feature is enabled.
- IMMEDIATELY after the forwarder is up, call `sub_open uri=forward://<FORWARD_ID>/events` for live accept/connect/close pushes — strictly preferred over polling resources/read.

Important identifiers in response:
- `FORWARD_ID`: target of `sub_open uri=forward://<FORWARD_ID>/events` (PREFERRED — push event log).

Workflow:
1. ssh_connect (returns SESSION_ID).
2. ssh_forward session_id=... local_port=... remote_address=... remote_port=...
3. sub_open uri=forward://<FORWARD_ID>/events (PREFERRED — push lifecycle events).
4. Connect local clients to localhost:<local_port>; observe accept/connect/close events on the lane.
5. ssh_disconnect (cascades to forwarder teardown) or close session — sub_close lane.

Overlap guidance:
- Prefer ssh_forward for tunnelled TCP. For ad-hoc port use, prefer the underlying ssh_exec + remote daemon if you control both endpoints.
- Cascading teardown via ssh_disconnect is simpler than tracking individual FORWARD_IDs in long-running agents.

Status values: OK.

Errors: SESSION_NOT_FOUND, PORT_IN_USE.

Cost: 1 listener bind + SSH tcpip-forward.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `local_port` | `integer` | yes | — | Local TCP port to listen on (e.g. `8080`). |
| `remote_address` | `string` | yes | — | Remote host on the server side to forward to (e.g. `localhost` or `10.0.0.1`). |
| `remote_port` | `integer` | yes | — | Remote TCP port to forward to (e.g. `3306` for `MySQL`). |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |

## `ssh_rsync`

**Title:** Sync a directory tree over SSH (rsync)

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

**Reused definitions (`$defs`):** PreserveFlagsArg, RsyncOptsArg, RsyncTransportArg

### Description

Drives a recursive sync over an existing SSH session. Both transports are LIVE: Wire (rsync v32 wire-compat, downgrades to v31 — openrsync-faithful port, byte-identical against rsync 3.2.x and 3.4.x) drives delta-sync via remote `rsync --server`; Sftp (universal fallback) walks plain SFTP for hosts without rsync v3.2+.

Transport selection:
- `transport=auto` (default) — probe the remote; rsync v31+ routes to the Wire transport (local advertises v32, downgrades to v31 against legacy servers), otherwise routes to the SFTP fallback.
- `transport=wire` — force the wire-compat client (advertises v32, downgrades to v31); returns `RSYNC_VERSION_TOO_OLD` if the remote rsync is missing or older than v3.2.0.
- `transport=sftp` — universal fallback; drives plain SFTP `readdir` + `stat` + `read` + `write` + `setstat` against any host with a working SFTP subsystem.

When to use:
- Recursive directory mirror over SSH (with `--delete`, `--exclude`, `--bwlimit`, attribute preservation).
- Delta-syncing large append-only files (logs, dumps, container layers) — only changed blocks cross the wire (Wire transport only).

Overlap guidance:
- Prefer ssh_rsync over chained ssh_upload calls when transferring >5 files or syncing a directory tree — single SSH channel + delta-sync (Wire) or capability-probed SFTP batch (Sftp).
- For a single file, ssh_upload is simpler (no transport probe, no per-file lane events).
- Cancel mid-flight via ssh_rsync_cancel; snapshot progress via ssh_rsync_stats (or PREFERRED — subscribe to `rsync://<RSYNC_ID>/progress`).

Identifiers:
- `RSYNC_ID`: pass to ssh_rsync_stats (snapshot) and ssh_rsync_cancel (terminal cancel). ALSO subscribe via `sub_open uri=rsync://<RSYNC_ID>/progress` for live per-file + aggregate push events (PREFERRED over polling stats).

Workflow:
1. ssh_connect.
2. ssh_rsync (returns STARTED + RSYNC_ID).
3. sub_open uri=rsync://<RSYNC_ID>/progress.
4. Drain `notifications/resources/updated` events.
5. sub_close + ssh_rsync_cancel (or wait for `SyncCompleted`).

Status values: STARTED.

Errors: RSYNC_NOT_FOUND, RSYNC_VERSION_TOO_OLD, RSYNC_PROTOCOL_ERROR, RSYNC_FILE_LIST_TOO_LARGE, RSYNC_PARTIAL_TRANSFER, SFTP_FEATURE_MISSING, SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED.

Cost: O(N) over file count + delta bytes. Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `dst` | `string` | yes | — | See `src`. Exactly one of `src` / `dst` must be remote. |
| `opts` | `RsyncOptsArg (via $ref)` | no | — | Rsync feature flags. Default mirrors `rsync -a` minus `-D` / `-H` (see [`PreserveFlagsArg::default`]). |
| `release_when_no_subs` | `['boolean', 'null']` | no | — | ADR 0003 lifecycle binding — auto-release the rsync session when the last subscriber detaches. Default `false` preserves v6.1 manual-close semantics.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |
| `src` | `string` | yes | — | Local path or remote path. Direction inferred from which side has the `host:` prefix — exactly one of `src` / `dst` must be remote. |
| `transport` | `RsyncTransportArg (via $ref)` | no | `auto`, `wire`, `sftp` | Transport selection. Default `Auto` (probe; prefer wire-compat). |

## `ssh_rsync_cancel`

**Title:** Cancel an in-flight rsync session

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Cancel a running rsync session by RSYNC_ID. Flips the session aggregate to `Cancelled` and tears down the russh channel + reader/writer task pair. Idempotent: cancelling a missing or already-terminal session returns OK.

When to use:
- Operator-driven abort (subscribed lane shows runaway file list).
- Cleanup before ssh_disconnect.

Overlap guidance:
- Prefer ssh_rsync_cancel for operator-driven abort. For natural completion, await the `SyncCompleted` event on the rsync://<RSYNC_ID>/progress lane instead.
- Cancelling a session that already finished is a no-op (returns OK) — safe to call defensively.

Workflow:
1. Look up RSYNC_ID from ssh_rsync's STARTED response.
2. Call ssh_rsync_cancel.
3. Drain any final lane events; sub_close.

Status values: OK.

Errors: RSYNC_NOT_FOUND.

Cost: O(1) — flips status atom + drops russh channel handle. Always succeeds locally.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `rsync_id` | `string` | yes | — | `RSYNC_ID` returned from `ssh_rsync`. |

## `ssh_rsync_stats`

**Title:** Get rsync session stats

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Snapshot stats for an in-flight or completed rsync session. Returns the live `RsyncStats` aggregate (files_total, files_done, files_deleted, files_failed, bytes_total, bytes_transferred, bytes_skipped) plus the current session status (pending / probing / running / completed / failed / cancelled).

When to use:
- Polling caller without a subscription lane.
- Aggregate snapshot after `SyncCompleted` event for final summary.

Push: subscribe to `rsync://<RSYNC_ID>/progress` instead of polling — same data flows through the lane debouncer at 200ms granularity.

Overlap guidance:
- Prefer sub_open uri=rsync://<RSYNC_ID>/progress over polling ssh_rsync_stats — the lane carries the same RsyncStats aggregate plus per-file events.
- Use ssh_rsync_stats for one-shot final summary after `SyncCompleted` (or when no subscription lane is desired).

Workflow:
1. Capture RSYNC_ID from ssh_rsync's STARTED response.
2. Call ssh_rsync_stats.
3. Inspect status field; loop or move on based on terminal vs running state.

Status values: OK.

Errors: RSYNC_NOT_FOUND.

Cost: O(1) atomic loads — no network I/O.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `rsync_id` | `string` | yes | — | `RSYNC_ID` returned from `ssh_rsync`. |

## `ssh_run`

**Title:** Run remote command (one-shot)

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

### Description

PENALIZED FALLBACK — connect, execute one command, disconnect, all in one call. Pays a full SSH handshake (200-2000 ms) and tears down the session on every call. Use ONLY when you genuinely cannot keep state across calls.

DO NOT USE WHEN:
- You will touch the same host more than once. Open a session with ssh_connect (reuse=auto) and reuse it.
- You expect any follow-up: tail logs, retry, run a sibling command, open a shell. Re-handshaking throws away cwd, env, agent forwarding, multiplexed channels, and any subscription you might want next.
- The command emits more than a few KiB of output. Stream via ssh_exec + sub_open `command://<id>/output` instead.

Preferred path (use this 95 %% of the time):
1. ssh_connect address username [agent_id] reuse=auto  -> SESSION_ID
2. ssh_exec SESSION_ID command  -> COMMAND_ID
3. sub_open uri=command://<COMMAND_ID>/output  -> stream stdout/stderr
4. sub_close sub_id=...  (or release_when_no_subs=true on step 2)
5. Keep SESSION_ID for the next call. ssh_disconnect only at the end of the workload.

When ssh_run is acceptable:
- A truly one-shot probe from a context that will never touch this host again (e.g. capability sniffing across many distinct hosts, with N=1 command per host).
- A test harness asserting end-to-end happy path in a single call.

Cost: 1 SSH handshake (200-2000 ms) + 1 channel + (optional) full session teardown. Multiply this by every call you make. Two ssh_run calls to the same host cost as much as one ssh_connect + two ssh_exec calls.

Workflow (if you must):
1. ssh_run mints (or with reuse=auto, reuses) a session.
2. Spawns the command and blocks until completion or `timeout_secs` fires.
3. With `disconnect_after=true` (default) tears the session down — this is the expensive bit.

Status values: COMPLETED, TIMEOUT, FAILED, CANCELLED.

Errors: CONNECTION_FAILED, AUTH_FAILED, MAX_COMMANDS_EXCEEDED, TRANSPORT_ERROR.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `address` | `string` | yes | — | SSH server address in the form `host:port` (e.g. `192.168.1.1:22`, `example.com:2222`). Port defaults to 22. |
| `agent_id` | `['string', 'null']` | no | — | Optional `AGENT_ID` for grouping the underlying session. |
| `command` | `string` | yes | — | Command to run on the remote host. |
| `disconnect_after` | `['boolean', 'null']` | no | — | Disconnect the session after the command finishes. Default: true (one-shot mode). Set false to keep the session open and reuse it for subsequent `ssh_exec` calls.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: true. |
| `key_path` | `['string', 'null']` | no | — | Path to a private key file. Optional. Auth chain: key -> password -> agent (`SSH_AUTH_SOCK`). |
| `max_output_bytes` | `['integer', 'null']` | no | — | Maximum bytes shown in stdout/stderr. Default: 16384. Cap: 1048576. |
| `password` | `['string', 'null']` | no | — | Password for password-based authentication. Optional. |
| `pty` | `['boolean', 'null']` | no | — | Allocate a pseudo-terminal for the command. Default: false.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `timeout_secs` | `['integer', 'null']` | no | — | Maximum seconds to wait for the command to complete. Default: 30. Capped at 300 by the inbound adapter. |
| `username` | `string` | yes | — | SSH login username. |

## `ssh_sessions`

**Title:** List SSH sessions

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

List active SSH sessions on the server.

When to use:
- One-shot inventory of sessions known to this server (optionally narrowed to one agent).
- For LIVE health observation prefer `sub_open uri=session://<SESSION_ID>/health` per session — push delivers HealthEvent ticks without poll loops.

Workflow:
1. Optional `agent_id` filter to scope the list to sessions tagged with that AGENT_ID.
2. Optional `max_items` cap (default 500, env `SSH_MCP_LIST_MAX_ITEMS`).
3. Each session id can be wrapped in `sub_open uri=session://<id>/health` for live monitoring; cancel via sub_close when done.

Status values: OK.

Errors: STORAGE_ERROR.

Cost: O(N) over current sessions. Cheap to call repeatedly.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `agent_id` | `['string', 'null']` | no | — | `AGENT_ID` returned from `ssh_connect`. Optional filter; when omitted returns sessions from every agent on this server. |
| `max_items` | `['integer', 'null']` | no | — | Maximum entries returned. Default: 500. Cap: 10000. Env: `SSH_MCP_LIST_MAX_ITEMS` / `SSH_MCP_LIST_MAX_ITEMS_CAP`. |

## `ssh_shell_close`

**Title:** Close PTY shell

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Close a PTY shell and free its resources. Cancels the russh channel, drops the per-shell registry entry, and cascades to any open shell:// subscription lanes.

When to use:
- Tearing down a PTY allocated by ssh_shell_open.
- Aborting an interactive program stuck on a prompt.

Lifecycle interaction: when the shell was opened with `release_when_no_subs=true`, ssh_shell_close fires before the grace window — open lanes transition straight to Closed. Otherwise, lanes survive the close and emit a final `closed` event before unsubscribing.

Workflow:
1. Capture SHELL_ID from ssh_shell_open's response.
2. ssh_shell_close shell_id=...
3. Open shell:// lanes emit a final `closed` event; sub_close on each.

Overlap guidance:
- Prefer ssh_shell_close to free a single PTY. For bulk teardown, ssh_disconnect (full session) cascades to every shell on that session.

Status values: OK, NOOP (already closed).

Errors: SHELL_NOT_FOUND, TRANSPORT_ERROR.

Cost: O(1). Always succeeds locally; TRANSPORT_ERROR only if russh teardown races a dead transport.

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `shell_id` | `string` | yes | — | `SHELL_ID` returned from `ssh_shell_open`. |

## `ssh_shell_open`

**Title:** Open PTY shell

**Annotations:** destructiveHint=False, idempotentHint=False, readOnlyHint=False

### Description

Open an interactive PTY shell on an SSH session.

When to use:
- Driving an interactive program (vim, htop, REPL, sudo prompt) that needs a TTY.
- IMMEDIATELY after open, call `sub_open uri=shell://<SHELL_ID>/output` for push delivery — strictly preferred over polling ssh_shell_read. Push fires on whichever fires first: ~200 ms debounce window OR 64 KiB byte-threshold.

Important identifiers in response:
- `SHELL_ID`: passed to ssh_shell_write, ssh_shell_press, ssh_shell_read, ssh_shell_wait_for, ssh_shell_close. ALSO the target of `sub_open uri=shell://<SHELL_ID>/output` (PREFERRED for streaming output).

Workflow:
1. ssh_connect (returns SESSION_ID).
2. ssh_shell_open session_id=... (returns SHELL_ID; INITIAL_BUFFER may include the prompt if it arrived within 100ms).
3. sub_open uri=shell://<SHELL_ID>/output (REQUIRED — push delivery).
4. Drive with ssh_shell_write / ssh_shell_press; await `notifications/resources/updated` and drain via `resources/read?cursor=auto`.
5. ssh_shell_close + sub_close on completion.

Overlap guidance:
- Prefer ssh_shell_open for interactive (TTY-needing) workloads. For non-interactive commands, use ssh_exec — its own channel, no PTY allocation cost, parallelism bound by `MaxSessions`.
- For one-shot commands across many distinct hosts, ssh_run is a PENALIZED fallback (full handshake teardown).

Status values: OK.

Errors: SESSION_NOT_FOUND, MAX_SHELLS_EXCEEDED, TRANSPORT_ERROR.

Cost: 1 SSH PTY allocation (typical 50-500ms). One PTY per shell_id.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `cols` | `['integer', 'null']` | no | — | Terminal width in columns. Default: 80. |
| `grace_ms` | `['integer', 'null']` | no | — | v5 Phase 3 — grace window in ms before auto-release fires. Default: 2000. |
| `inactivity_ttl` | `['integer', 'null']` | no | — | Inactivity TTL in seconds. Shell auto-closes if no read or write happens within this window. Default: 600. Env: `SSH_SHELL_INACTIVITY_TTL`. |
| `max_buffer_size` | `['string', 'null']` | no | — | Maximum output buffer size. Accepts human sizes like `512k`, `10m`, `1g`, `1t`. Default: `10m`. Env: `SSH_SHELL_MAX_BUFFER_SIZE`. Oldest bytes dropped first when full. |
| `release_when_no_subs` | `['boolean', 'null']` | no | — | v5 Phase 3 — auto-release when the resource has zero subscribers. Default: false (legacy v4 behaviour).  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `rows` | `['integer', 'null']` | no | — | Terminal height in rows. Default: 24. |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |
| `term` | `['string', 'null']` | no | — | Terminal type. Default: `xterm`. Use `vt100` or `ansi` for SOL/IPMI/serial consoles. |

## `ssh_shell_press`

**Title:** Send keystroke to PTY

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

**Reused definitions (`$defs`):** ShellKey

### Description

Send a named keystroke (with optional modifiers) to a PTY shell.

When to use:
- Sending arrows, function keys, control codes, navigation keys without crafting the bytes manually.
- Optional Shift / Alt / Ctrl modifiers; optional `repeat` (1..=64).

Workflow:
1. ssh_shell_open returned SHELL_ID and a shell://<SHELL_ID>/output lane is open via sub_open.
2. ssh_shell_press shell_id=... key=<name> [modifiers=...] [repeat=...].
3. AWAIT `notifications/resources/updated` push on the lane (DO NOT chain ssh_shell_wait_for).
4. Drain via `resources/read shell://<SHELL_ID>/output?cursor=auto`.

Overlap guidance:
- Prefer ssh_shell_press for named keystrokes (Ctrl+C, arrow keys, Tab, Esc). Use ssh_shell_write for typed text or arbitrary byte sequences.
- For programmatic control codes, ssh_shell_write input_base64=... is sometimes simpler than chaining ssh_shell_press.

Push-first:
- After ssh_shell_write/press, AWAIT a `notifications/resources/updated` push on the shell://<SHELL_ID>/output lane already opened via sub_open. Do NOT chain ssh_shell_wait_for — that's a poll-loop fallback.

After the keystroke — DO NOT chain ssh_shell_wait_for back-to-back. The response arrives via push on the existing shell://<id>/output subscription. Wait for `notifications/resources/updated` and then drain with `resources/read shell://<id>/output?cursor=auto`. If you have not subscribed yet, call sub_open first; ssh_shell_wait_for is reserved for explicit regex prompt-gating.

Status values: OK.

Errors: SHELL_NOT_FOUND, INVALID_ARGUMENT (bad repeat / modifier combination), TRANSPORT_ERROR.

Cost: O(repeat). Sub-ms typical. Subscribe to shell://<id>/output for response.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `alt` | `['boolean', 'null']` | no | — | Apply Alt modifier. Default: false. Valid on: arrows, navigation keys, and F1-F12. See the `key` field for the full per-key policy.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `ctrl` | `['boolean', 'null']` | no | — | Apply Ctrl modifier. Default: false. Valid on: arrows, navigation keys, and F1-F12. Cannot be combined with `ctrl_*` keys (already a complete control byte). See the `key` field for the full per-key policy.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `key` | `ShellKey (via $ref)` | yes | `ctrl_a`, `ctrl_c`, `ctrl_d`, `ctrl_e`, `ctrl_k`, `ctrl_l`, `ctrl_r`, `ctrl_u`, `ctrl_w`, `ctrl_z`, `enter`, `tab`, `escape`, `backspace`, `space`, `delete`, `arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`, `home`, `end`, `page_up`, `page_down`, `insert`, `f1`, `f2`, `f3`, `f4`, `f5`, `f6`, `f7`, `f8`, `f9`, `f10`, `f11`, `f12` | Named keystroke to send.  Per-key modifier policy (modifiers beyond the listed set return error code `MODIFIER_NOT_ALLOWED`):  - Arrow keys (`arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`)   accept any of `shift` / `alt` / `ctrl`. - Navigation (`home`, `end`, `page_up`, `page_down`, `insert`,   `delete`) accept any of `shift` / `alt` / `ctrl`. - Function keys `f1`-`f12` accept any of `shift` / `alt` / `ctrl`. - `tab` accepts only `shift` (back-tab); rejects `alt` / `ctrl`. - All other keys (`ctrl_c`, `ctrl_d`, `ctrl_z`, `ctrl_l`,   `ctrl_r`, `ctrl_w`, `ctrl_u`, `enter`, `escape`, `backspace`,   `space`) reject every modifier — the keystroke is already a   complete control byte.  Examples: `ctrl_c`, `arrow_up`, `f5`, `enter`, `tab`. |
| `repeat` | `['integer', 'null']` | no | — | Repeat the keystroke N times. Default: 1. Range: 1..=64. Values outside this range return error code `INVALID_REPEAT`. |
| `shell_id` | `string` | yes | — | `SHELL_ID` returned from `ssh_shell_open`. |
| `shift` | `['boolean', 'null']` | no | — | Apply Shift modifier. Default: false. Valid on: arrows, navigation keys, F1-F12, and `tab` (back-tab). See the `key` field for the full per-key policy.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |

## `ssh_shell_read`

**Title:** Read PTY buffer

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=False

### Description

Read the buffered output of a PTY shell. POLLING FALLBACK — strictly cheaper to use `sub_open uri=shell://<shell_id>/output` (push delivery, no round-trip per delta).

When to use:
- ONLY when the host genuinely cannot consume `notifications/resources/updated` push events. Every other situation, prefer `sub_open uri=shell://<shell_id>/output` + `resources/read?cursor=auto` on each push.
- `clear=true` (default) drains the rendered head; `clear=false` keeps the buffer for re-inspection.
- Optional long-poll via `wait=true` (`min_bytes` / `wait_timeout_secs`).

Workflow:
1. ssh_shell_open returned SHELL_ID.
2. ssh_shell_read shell_id=... [clear=...] [wait=...].
3. Decide based on STATUS (OPEN / CLOSED / TIMEOUT).

Overlap guidance:
- Prefer sub_open uri=shell://<SHELL_ID>/output over ssh_shell_read for any workflow with >1 expected delta — push is one tool call total, ssh_shell_read is one per delta.
- For one-shot buffer inspection (no future deltas), ssh_shell_read clear=false is acceptable.

Push-first:
- After ssh_shell_write/press, AWAIT a `notifications/resources/updated` push on the shell://<SHELL_ID>/output lane already opened via sub_open. Do NOT chain ssh_shell_wait_for — that's a poll-loop fallback.

Status values: OPEN, CLOSED, TIMEOUT.

Errors: SHELL_NOT_FOUND.

Cost: O(buffer) per call + 1 tool round-trip. Subscribe is one tool call total per shell — strictly cheaper for any workflow with >1 expected response.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `clear` | `['boolean', 'null']` | no | — | Drain the bytes that were rendered (head-based pagination). Default: true. With false the buffer is preserved (peek mode) for inspecting the same window multiple times.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: true. |
| `max_output_bytes` | `['integer', 'null']` | no | — | Maximum bytes shown in data block. Default: 16384. Cap: 1048576. Output rendered as the tail (most recent bytes). Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`. |
| `min_bytes` | `['integer', 'null']` | no | — | Minimum new bytes to wait for before returning (only with `wait=true`). Default: 1 (any new byte returns). Capped at the resolved `max_output_bytes`. Floor: 1. |
| `shell_id` | `string` | yes | — | `SHELL_ID` returned from `ssh_shell_open`. |
| `wait` | `['boolean', 'null']` | no | — | FALLBACK long-poll. Block until `min_bytes` of new output arrive, the shell closes, or `wait_timeout_secs` expires. Default: false. Prefer `resources/subscribe shell://<shell_id>/output` (realtime push) over polling.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `wait_timeout_secs` | `['integer', 'null']` | no | — | Maximum seconds to block when `wait=true`. Default: 30. Cap: 300. |

## `ssh_shell_wait_for`

**Title:** Wait for shell pattern

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Block until a substring pattern appears in the shell output. PREFER sub_open (with optional filter regex) over this tool — wait_for is a single-shot regex GATE, not a 'wait for output' primitive.

DO NOT USE WHEN:
- You just called ssh_shell_write or ssh_shell_press and want the response. The response arrives via push on shell://<id>/output. Wait for `notifications/resources/updated` and drain with `resources/read?cursor=auto`. Chaining wait_for after every drive call is the most common 27B-class anti-pattern.
- You want any output at all (no specific prompt). sub_open is strictly cheaper.
- You want repeated pattern matches. sub_open with `filter=<regex>` pushes every match without a tool call per gate.

When to use (rare, explicit):
- One-time synchronization on a known prompt before issuing the next command (e.g. wait for `"$ "` after sudo).
- Up to 16 patterns (≤1024 bytes each); first match wins.

Preferred path (use this 95 %% of the time):
1. sub_open uri=shell://<shell_id>/output [filter=<regex>] -> SUB_ID
2. Wait for `notifications/resources/updated` (push).
3. resources/read shell://<shell_id>/output?cursor=auto -> drain delta.

Status values: MATCHED, TIMEOUT, CLOSED.

Errors: SHELL_NOT_FOUND, INVALID_ARGUMENT.

Cost: blocks up to timeout_secs. One tool call per gate. Subscribe is one tool call total per shell — strictly cheaper for any workflow with >1 expected response.

Progress: when `_meta.progressToken` is set, emits `notifications/progress` once per second carrying `(elapsed_secs, timeout_secs)` while the loop runs (best-effort).

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `clear` | `['boolean', 'null']` | no | — | Drain matched output from the shell history (head) after returning so subsequent reads start fresh. Default: true.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: true. |
| `max_output_bytes` | `['integer', 'null']` | no | — | Maximum bytes shown when matched. Default: 16384. Cap: 1048576. Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`. |
| `patterns` | `array` | yes | — | 1..=16 substring patterns. First match returns immediately as `MATCHED_PATTERN`. Each pattern up to 1024 bytes. Prefer `resources/subscribe shell://<shell_id>/output` (realtime push) over polling — use this for single-shot prompt gating.  Errors: empty list returns code `EMPTY_PATTERNS`. List with more than 16 entries returns `TOO_MANY_PATTERNS`. Any single pattern over 1024 bytes returns `PATTERN_TOO_LONG`. |
| `shell_id` | `string` | yes | — | `SHELL_ID` returned from `ssh_shell_open`. |
| `timeout_secs` | `['integer', 'null']` | no | — | Maximum seconds to wait. Default: 30. Cap: 300. |

## `ssh_shell_write`

**Title:** Write to PTY shell

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

### Description

Write raw bytes to a PTY shell.

When to use:
- Submitting a typed command (append `\n`).
- Sending raw control sequences (e.g. `\x03` for Ctrl+C, `\x1b[A` for arrow up).
- Prefer ssh_shell_press for named keystrokes.

Workflow:
1. ssh_shell_open returned SHELL_ID and you have already opened a shell://<SHELL_ID>/output subscription.
2. ssh_shell_write shell_id=... text="command\n" (or input_base64=... for raw bytes).
3. AWAIT `notifications/resources/updated` push on the lane (DO NOT chain ssh_shell_wait_for).
4. Drain via `resources/read shell://<SHELL_ID>/output?cursor=auto`.

Overlap guidance:
- Prefer ssh_shell_write for human-readable input. Use ssh_shell_press for control keys (Ctrl+C, arrow keys, Enter alone).
- For commands you don't need to interact with mid-run, prefer ssh_exec — opens its own channel, parallelism is bound by `MaxSessions` not the shell PTY.

Push-first:
- After ssh_shell_write/press, AWAIT a `notifications/resources/updated` push on the shell://<SHELL_ID>/output lane already opened via sub_open. Do NOT chain ssh_shell_wait_for — that's a poll-loop fallback.

After the write — DO NOT chain ssh_shell_wait_for back-to-back. The response arrives via push on the existing shell://<id>/output subscription. Wait for `notifications/resources/updated` and then drain with `resources/read shell://<id>/output?cursor=auto`. If you have not subscribed yet, call sub_open first; ssh_shell_wait_for is reserved for explicit regex prompt-gating, not a generic 'now wait for output' primitive.

Status values: OK.

Errors: SHELL_NOT_FOUND, TRANSPORT_ERROR.

Cost: O(input.len). Sub-ms typical. Subscribe to shell://<id>/output for response.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `input` | `string` | yes | — | Bytes to send to the PTY. Append `\n` to submit a typed command. Use control sequences directly (e.g. `\x03` for Ctrl+C, `\x1b[A` for arrow up). Prefer `ssh_shell_press` for named keystrokes. |
| `shell_id` | `string` | yes | — | `SHELL_ID` returned from `ssh_shell_open`. |

## `ssh_transfer_progress`

**Title:** Get transfer progress

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Snapshot the progress of an SFTP transfer. Polling fallback path; prefer sub_open `transfer://<TRANSFER_ID>/progress` for live progress events.

When to use:
- One-shot status check on an upload/download without opening a subscription.
- Hosts that lack `resources/subscribe` capability.
- Optional `wait=true` blocks until the transfer reaches a terminal state.

Workflow:
1. Capture TRANSFER_ID from ssh_upload / ssh_download's STARTED response.
2. ssh_transfer_progress transfer_id=... [wait=true] [wait_timeout_secs=...].
3. Inspect status; loop or move on.

Overlap guidance:
- Prefer sub_open uri=transfer://<TRANSFER_ID>/progress over ssh_transfer_progress for any workflow with >1 expected progress event.
- For one-shot terminal-state polling (no streaming), ssh_transfer_progress wait=true is a single tool call.

Status values: RUNNING, COMPLETED, FAILED, CANCELLED.

Errors: TRANSFER_NOT_FOUND.

Cost: O(1). Cheap with wait=false. With wait=true blocks until done or wait_timeout_secs. Subscribe is cheaper for streams >1 update.

Progress: when `wait=true` and `_meta.progressToken` is set, mid-flight `notifications/progress` updates fire every ~5s carrying `(bytes_transferred, total_bytes)` (best-effort).

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `transfer_id` | `string` | yes | — | `TRANSFER_ID` returned from `ssh_upload` or `ssh_download`. |
| `wait` | `['boolean', 'null']` | no | — | Block until completion or `wait_timeout_secs` expires. Default: false.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `wait_timeout_secs` | `['integer', 'null']` | no | — | Maximum seconds to block when `wait=true`. Default: 30. Cap: 300. |

## `ssh_upload`

**Title:** Upload file via SFTP

**Annotations:** destructiveHint=True, idempotentHint=False, readOnlyHint=False

### Description

Upload a local file to the remote host via SFTP.

When to use:
- Streaming a local file to the remote host in 32 KiB chunks.
- IMMEDIATELY after upload start, call `sub_open uri=transfer://<TRANSFER_ID>/progress` for live byte-counter pushes — strictly preferred over polling ssh_transfer_progress(wait=true).
- Set `resume=true` (ADR 0010) to retry an interrupted upload from the first non-overlapping byte; `RESUMED_FROM:` in the response reports the offset.
- Pair `resume=true` with `verify=true` to guard against silent prefix corruption — costs one extra ssh_exec round-trip plus O(offset) bytes hashed remotely.

Important identifiers in response:
- `TRANSFER_ID`: passed to ssh_transfer_progress (poll fallback) AND to `sub_open uri=transfer://<TRANSFER_ID>/progress` (PREFERRED — push delivery).
- `RESUMED_FROM`: byte offset the transfer resumed from (`0` for fresh transfers).

Workflow:
1. ssh_connect (returns SESSION_ID).
2. ssh_upload session_id=... local_path=... remote_path=... [resume=...] [verify=...].
3. sub_open uri=transfer://<TRANSFER_ID>/progress (PREFERRED — push delivery).
4. Drain progress events via `notifications/resources/updated`; final event is terminal.
5. sub_close on completion.

Overlap guidance:
- Prefer ssh_upload for a single file. For >5 files or directory-tree mirrors, prefer ssh_rsync (transport=auto picks Wire delta-sync or SFTP fallback) — single SSH session, batched walk.
- For polling fallback only, ssh_transfer_progress wait=true blocks until terminal.
- Pair resume=true + verify=true on flaky links to guard against silent prefix corruption.

Status values: STARTED.

Errors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_ERROR, RESUME_OVERSHOOT, RESUME_MISMATCH.

Cost: O(file.size). Returns immediately, transfer runs async.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `grace_ms` | `['integer', 'null']` | no | — | v5 Phase 3 — grace window in ms before auto-release fires. Default: 2000. |
| `local_path` | `string` | yes | — | Local file path to upload. Relative paths resolve against the home directory. |
| `release_when_no_subs` | `['boolean', 'null']` | no | — | v5 Phase 3 — auto-release when the transfer resource has zero subscribers. Default: false (legacy v4 behaviour).  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `remote_path` | `string` | yes | — | Remote destination path on the SSH server. |
| `resume` | `['boolean', 'null']` | no | — | ADR 0010 — opt-in resume from the remote tail. When `true`, the adapter pre-flights the remote file size and resumes from the first non-overlapping byte. When `false` (default), every upload truncates the destination and starts from byte zero (v6.0 semantics). Set together with `verify=true` for a stronger guarantee that the remote prefix matches the local prefix.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |
| `session_id` | `string` | yes | — | `SESSION_ID` returned from `ssh_connect`. |
| `verify` | `['boolean', 'null']` | no | — | ADR 0010 — when `resume=true`, hash the resume prefix on both sides and abort with `RESUME_MISMATCH` if the hashes diverge. Default `false` trusts the prefix verbatim. The hash compare costs one extra `ssh_exec` round-trip plus O(offset) bytes hashed remotely; only worth enabling if mid-transfer corruption has been observed in deployment.  Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false. |

## `sub_close`

**Title:** Unsubscribe from a resource lane

**Annotations:** destructiveHint=True, idempotentHint=True, readOnlyHint=False

### Description

Close a Channel Mux lane previously opened with sub_open. Detaches the SubId, drops the mpsc, releases the per-resource subscriber refcount, and (if last subscriber) arms the resource grace timer per ADR 0003.

When to use:
- Cleanup after subscription work completes.
- This IS the cleanup tool — call it for every SUB_ID you obtained.

Lifecycle interaction: when the resource was opened with `release_when_no_subs=true`, the last sub_close transitions Observed → Releasing and starts the grace window. A new sub_open for the same URI within the window cancels the timer (Releasing → Observed).

Workflow:
1. Capture SUB_ID from sub_open's response.
2. Drain remaining lane events (optional — final events fire on close).
3. sub_close sub_id=...
4. The lane refcount drops; resource may transition Observed → Releasing if last subscriber.

Overlap guidance:
- Prefer sub_close (manual cleanup) for deterministic teardown. Use lifetime=auto_close at sub_open if you want grace_ms-driven auto-cleanup.
- For temporary suppression without dropping the lane state, prefer sub_pause + sub_resume.

Status values: OK, NOOP (already closed).

Errors: SUB_NOT_FOUND — tolerate silently; lifetime auto_close / lease may have closed the lane already.

Cost: O(1).

Idempotency: pass `_meta.idempotency_key` to dedup retries.

Hygiene: tolerate `SUB_NOT_FOUND`; never re-open the same URI without first unsubscribing.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `sub_id` | `string` | yes | — | `SUB_ID` returned from `sub_open`. |

## `sub_filter`

**Title:** Hot-reload a subscription filter

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=False

### Description

Replace the lane regex filter without re-opening the lane. Empty regex clears the filter (forward all events).

When to use:
- Mutate match criteria mid-stream (e.g. tighten a tail to ERROR-only after seeing the boot phase).
- Strictly cheaper than sub_close + sub_open + sub_replay.

Syntax: Rust `regex` crate (RE2-style; no lookaround / backref). Compiled at call time and atomically swapped — events in flight before the swap may still match the previous filter.

Workflow:
1. Capture SUB_ID from sub_open's response.
2. sub_filter sub_id=... regex=<new pattern> (or empty to clear).
3. Continue draining via the existing notification channel — filter applies to subsequent events.

Overlap guidance:
- Prefer sub_filter (compile-time regex on the lane) over post-receive client-side filter. Saves wire bandwidth + LLM tokens.
- For full-rebuild after pattern change, sub_filter mutates in place — no need to sub_close + sub_open.
- For temporary suppression without dropping the regex, prefer sub_pause + sub_resume.

Status values: OK.

Errors: SUB_NOT_FOUND, SUB_INVALID_FILTER (regex compile).

Cost: 1 regex compile + atomic swap.

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `regex` | `string` | yes | — | New regex filter. Empty string clears the filter (forward all events). |
| `sub_id` | `string` | yes | — | `SUB_ID` returned from `sub_open` — the lane whose filter is being updated. |

## `sub_list`

**Title:** List active subscription lanes

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Snapshot the per-`SubId` lane registry. Returns sub_id, uri, lag_policy, lifetime, filter (if any), state (active/paused), creation_at, last_push_at.

When to use:
- Discover SubIds on reconnect / debugging.
- Audit pre-cleanup before sub_close batches.
- Pair with sub_stats / sub_stats_all for live counters.

Optional `uri_prefix` filter narrows the result (e.g. `shell://` for PTY lanes only).

Workflow:
1. sub_list [uri_prefix=<scheme>://] (cheap snapshot, no mutation).
2. Iterate the response and call sub_close on each leaked SUB_ID.
3. Pair with sub_stats per-SubId for queue / lag counters.

Overlap guidance:
- Prefer sub_list for an inventory snapshot. For aggregate counters across all lanes, use sub_stats_all (single tool call, no per-lane fan-out).
- For per-lane drop / queue diagnostics, use sub_stats sub_id=<id>.

Status values: OK.

Errors: STORAGE_ERROR.

Cost: O(N) over open lanes. Cheap.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `peer_id` | `['string', 'null']` | no | — | Optional peer-id filter. Currently a placeholder — lanes are not peer-keyed today; reserved for future peer-aware filtering. |
| `uri_prefix` | `['string', 'null']` | no | — | Optional URI prefix filter. Returns only subs whose canonical URI starts with this prefix. |

## `sub_open`

**Title:** Subscribe to a resource lane

**Annotations:** destructiveHint=False, idempotentHint=False, readOnlyHint=False

**Reused definitions (`$defs`):** LagPolicy, LifetimeKind

### Description

Open a Channel Mux lane for an ssh-mcp resource URI.

When to use:
- Push-first observation of `shell://`, `command://`, `transfer://`, `session://`, `forward://`, `serial://`, `rsync://` without polling.
- Strictly preferred over polling tools (ssh_shell_read, ssh_exec_output, ssh_transfer_progress, ssh_rsync_stats) for any stream with >1 expected delta.

Lag policies (decision matrix):
- `snapshot` (default) — drop lane backlog, rebuild from per-resource ring buffer on next push. Zero loss while ring covers the gap; emits `LAG_DETECTED` otherwise. Pick for general observation.
- `drop_oldest` — FIFO rotation when lane mpsc full. Marks `lagged`. Pick for tail logs / monitoring with gap tolerance.
- `drop_newest` — drop new event when full. Pick when start-of-stream matters more than current.
- `block_slow` — producer awaits drain. Zero loss, latency cost. Pick for audit / every-event-matters workloads.

Lifetime knobs:
- `manual` (default) — close via sub_close.
- `auto_close` — closes after `grace_ms` of consumer inactivity (default 2000).
- `lease` — hard cap via `ttl_secs`.

Filter:
- Optional Rust-regex compiled at subscribe time; mutate live via sub_filter without re-opening.

Replay:
- sub_replay re-emits from `from_cursor` within the per-resource ring window. Independent of what the lane already dropped.

Push: events fan out to the lane peer via `notifications/resources/updated` on stdio/HTTP transports; the channel-mux outbound sink is the drain target for the `ssh-mcp-tail` NDJSON daemon.

Workflow:
1. Open the source resource (ssh_shell_open / ssh_exec / ssh_upload / ssh_rsync / serial_open / ssh_forward).
2. sub_open uri=<scheme>://<id>/<lane> (returns SUB_ID).
3. Await `notifications/resources/updated` and drain via `resources/read?cursor=auto`.
4. sub_close sub_id=... (REQUIRED — leaks become SUB_LEAK_RISK).

Successor tools:
- sub_filter — mutate lane regex without re-subscribing
- sub_pause / sub_resume — temporary throttle without dropping the lane
- sub_replay — recover events dropped by lag_policy
- sub_stats — diagnose drops + queue pressure
- sub_close — cleanup (REQUIRED — leaks become SUB_LEAK_RISK)

Status values: OK.

Errors: SUB_INVALID_FILTER (regex compile), SUB_LIMIT_EXCEEDED, RESOURCE_NOT_FOUND.

Cleanup: sub_close sub_id=... when done. Skip and the lane becomes a zombie (watcher emits SUB_LEAK_RISK).

Cost: O(1) lane open + per-event mpsc.

Idempotency: pass `_meta.idempotency_key` to dedup retries.

Hygiene: hold the SUB_ID; never re-open the same URI without first unsubscribing.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `filter` | `['string', 'null']` | no | — | Optional regex filter compiled at subscribe time. Empty / absent means no filter. |
| `grace_ms` | `['integer', 'null']` | no | — | Grace window in ms — only honoured when `lifetime=auto_close`. Default: 2000. |
| `inline_push` | `boolean` | no | — | ADR 0012 phase 5 -- request inline-push delivery.  When `true` AND the client advertised the `experimental.ssh_inline_push` capability at `initialize`, the server delivers coalesced bytes inline via the custom `notifications/ssh/output` method ON TOP OF the legacy `notifications/resources/updated` debouncer cadence.  Default `false` -- every v7.0.x host sees byte-identical wires. When `true` but the capability was not echoed, the server downgrades the lane silently and emits a `HINT: RECOMMENDED:` line on the `sub_open` response. |
| `lag_policy` | `LagPolicy (via $ref) \| null` | no | `block_slow`, `drop_oldest`, `drop_newest`, `snapshot` | Lag policy applied when the lane mpsc fills. Default: `snapshot`. |
| `lifetime` | `LifetimeKind (via $ref) \| null` | no | `manual`, `auto_close`, `lease` | Lifetime kind. Default: `manual`. |
| `ttl_secs` | `['integer', 'null']` | no | — | TTL in seconds — only honoured when `lifetime=lease`. |
| `uri` | `string` | yes | — | Resource URI (e.g. `shell://<id>/output`, `command://<id>/output`). |

## `sub_pause`

**Title:** Pause a subscription lane

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=False

### Description

Suspend lane drain. Subsequent events accumulate in the lane mpsc until sub_resume; lag_policy still applies — `drop_oldest` rotates the queue while paused.

When to use:
- Pause delivery to a slow consumer without losing the SubId / cursor / filter / lifetime state.
- Combine with sub_resume to gate batched processing.

Interaction: pause does NOT freeze the producer; events still hit the lane and get queued (or rotated, per lag_policy). High-frequency streams may exhaust the mpsc — monitor via sub_stats.

Workflow:
1. sub_open returned SUB_ID and the lane is delivering pushes.
2. sub_pause sub_id=... (atomic flag flip).
3. Process accumulated work; monitor mpsc pressure via sub_stats if needed.
4. sub_resume to flush queued events in order.

Overlap guidance:
- Prefer sub_pause + sub_resume for transient throttling — preserves SUB_ID, cursor, filter, lifetime. sub_close + sub_open re-mints the SubId and loses cursor state.
- For permanent filter changes, prefer sub_filter (no need to pause first).

Status values: OK, NOOP (already paused).

Errors: SUB_NOT_FOUND.

Cost: O(1) atomic flag flip.

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `sub_id` | `string` | yes | — | `SUB_ID` returned from `sub_open` — the lane to pause. |

## `sub_replay`

**Title:** Replay a subscription lane from cursor

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=False

### Description

Re-emit lane events from `from_cursor` (byte offset) within the per-resource ring-buffer window. Independent of `lag_policy` — sources from the ring, not the lane mpsc.

When to use:
- Recover events dropped by `drop_oldest` / `drop_newest` / `snapshot` policies.
- Resume from a known cursor after consumer restart.
- `from_cursor=0` (default) replays from the start of the available window.

Window size is per-resource (see RESOURCES.md). Cursors past the window head return whatever the ring still holds.

Workflow:
1. sub_stats reports `lagged_snapshot > 0` (or consumer noticed a gap in the cursor sequence).
2. sub_replay sub_id=... [from_cursor=N] (defaults to 0 — start of window).
3. Drain replayed events via the existing push channel.

Overlap guidance:
- Prefer sub_replay over re-subscribing when the consumer dropped events (lag_policy=drop_oldest/drop_newest/snapshot triggered) — sources from the per-resource ring buffer, not the lane mpsc.
- For window-cursor recovery use from_cursor=N (byte offset).
- For a fresh subscription that ignores history, use sub_close + sub_open.

Status values: OK.

Errors: SUB_NOT_FOUND, RESOURCE_GONE.

Cost: O(window-bytes) snapshot rebuild + N push deliveries.

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `from_cursor` | `['integer', 'null']` | no | — | Cursor (byte offset) to replay from. Default: 0 — replays from the start of the available ring-buffer window. |
| `sub_id` | `string` | yes | — | `SUB_ID` returned from `sub_open` — the lane to replay. |

## `sub_resume`

**Title:** Resume a subscription lane

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=False

### Description

Resume lane drain after a previous sub_pause. Queued events flush in order, subject to lag_policy.

When to use:
- Re-enable push delivery on a paused lane.

Interaction: if `lag_policy=snapshot` and the ring buffer rotated past the lane cursor during pause, the next push emits `LAG_DETECTED`. Inspect via sub_stats.

Workflow:
1. sub_pause put the lane in Paused state.
2. sub_resume sub_id=... flushes queued events in order.
3. Inspect sub_stats — if `lagged_snapshot > 0`, follow with sub_replay from_cursor=N to recover.

Overlap guidance:
- Prefer sub_resume for unpause. To recover events that the lag_policy dropped during pause, follow with sub_replay (sources from the per-resource ring, not the lane mpsc).
- For an immediate fresh stream from now (drop backlog), pair sub_close + sub_open instead.

Status values: OK, NOOP (not paused).

Errors: SUB_NOT_FOUND.

Cost: O(1).

Idempotency: pass `_meta.idempotency_key` to dedup retries.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `sub_id` | `string` | yes | — | `SUB_ID` returned from `sub_open` — the lane to resume. |

## `sub_stats`

**Title:** Snapshot subscription lane stats

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Per-lane atomic counter snapshot — `events_sent`, `bytes_sent`, `lagged_snapshot`, `lagged_drop_oldest`, `lagged_drop_newest`, `lagged_block`, `queue_depth`, `queue_high_watermark`, `replay_count`.

When to use:
- Diagnose drops (compare lagged_* counters against the lag_policy chosen at sub_open).
- Detect mpsc pressure (queue_high_watermark approaching channel size = consumer too slow → consider drop_oldest or block_slow).
- Validate filter efficacy (events_sent vs source rate).

Workflow:
1. Capture SUB_ID from sub_open's response.
2. sub_stats sub_id=... — read atomic counters.
3. Decide: high lagged_* → sub_replay or change lag_policy via sub_close + sub_open with new policy.

Overlap guidance:
- Prefer sub_stats for per-lane diagnostics. For daemon-wide aggregate (sum + max), use sub_stats_all (single call, no SUB_ID needed).
- For a structural inventory (uri / state / filter / lifetime), use sub_list — it omits counters but lists every active lane.

Status values: OK.

Errors: SUB_NOT_FOUND.

Cost: O(1) atomic loads.

### Parameters

| Name | Type | Required | Values | Description |
| --- | --- | --- | --- | --- |
| `sub_id` | `string` | yes | — | `SUB_ID` returned from `sub_open` — the lane to inspect. |

## `sub_stats_all`

**Title:** Aggregate daemon-wide subscription stats

**Annotations:** destructiveHint=False, idempotentHint=True, readOnlyHint=True

### Description

Aggregate snapshot across every open lane in the channel mux. Returns sum (events_sent, bytes_sent, lagged_*) and max (queue_depth, queue_high_watermark) plus open lane count.

When to use:
- Cluster-wide health check before scaling decisions.
- SUB_LEAK_RISK investigation (lane count drifting up over time).
- Compare with sub_stats per-lane to find outliers.

Workflow:
1. sub_stats_all (no args required — daemon-wide).
2. Inspect open_lane_count + sum(lagged_*) for early warning.
3. Drill down with sub_list to find offending SUB_IDs; then sub_stats per-id.

Overlap guidance:
- Prefer sub_stats_all for cheap aggregate health. For per-lane breakdown, follow with sub_list + sub_stats per SUB_ID.
- If the goal is just to detect leaks, sub_list with no filter is also enough (counts entries).

Status values: OK.

Errors: STORAGE_ERROR.

Cost: O(N) over open lanes.

### Parameters

No parameters.
