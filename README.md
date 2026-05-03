# SSH-MCP Server

[![Rust](https://img.shields.io/badge/rust-2024-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](Cargo.toml)
[![Tests](https://img.shields.io/badge/tests-1156%20passing-brightgreen.svg)]()
[![Version](https://img.shields.io/badge/version-4.7.0-blue.svg)]()
[![Architecture](https://img.shields.io/badge/architecture-hexagonal-purple.svg)]()
[![Transport](https://img.shields.io/badge/transport-rmcp%201.6-purple.svg)]()

> [!caution]
> This is **NOT** the original [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp).
> Everything has been rewritten from scratch — different SSH library, different
> MCP transport (rmcp 1.6), different threading model, lock-free state, and a
> full hexagonal (Ports and Adapters) layout.

A Rust SSH server with full Model Context Protocol (MCP) integration, enabling
LLMs to connect to SSH servers, execute commands, drive interactive shells with
**realtime resource subscriptions**, and stream files via SFTP — all over the
**rmcp 1.6** Streamable HTTP transport (axum-hosted) or stdio.

> **Codebase contributors upgrading from v3.x or v4.0?** See [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) (includes the v4.1 deep-decouple addendum). The public MCP API is unchanged — v4.x is an internal restructuring to a full hexagonal architecture, not a wire-format break.
> For LLM-side token-efficient usage patterns, see [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md).

[[_TOC_]]

## What's New in v4.7.0

- **MCP inter-tool conversation surface** — every tool now emits a parallel `structured_content` JSON object next to the existing block-style Markdown body. 6 tools advertise an `output_schema` JSON Schema (`ssh_connect`, `ssh_execute`, `ssh_get_command_output`, `ssh_shell_open`, `ssh_shell_read`, `ssh_get_transfer_progress`); the other 15 emit a free-form structured payload. Errors land in the structured channel as `{ tool, status: "error", code, reason, detail }`. Text channel is byte-identical with v4.6 — every existing host keeps working without change. See [docs/LLM_GUIDE.md section K](docs/LLM_GUIDE.md#k-structured_content-channel-v47).
- **Three new tools** — `ssh_run` (one-shot connect + execute + optional disconnect), `ssh_execute_batch` (sequential 1..=16 commands per session, stop-on-failure), `ssh_disconnect_many` (best-effort batch disconnect 1..=64 ids). Tool count moves from 18 to 21 (or 17 to 20 without `port_forward`). See [docs/API.md](docs/API.md).
- **Resource templates** — `resources/templates/list` advertises 4 RFC 6570 URI shapes (5 with `port_forward`) so smaller LLMs can scan the URI catalogue without enumerating live instances. See [docs/RESOURCES.md - Resource Templates (v4.7)](docs/RESOURCES.md#resource-templates-v47).
- **Progress notifications** — when a request includes `_meta.progressToken`, the server fires periodic `notifications/progress` updates during long async waits: `ssh_get_command_output(wait=true)` (5 s cadence), `ssh_get_transfer_progress(wait=true)` (5 s), `ssh_shell_wait_for` (1 s). Best-effort — transport errors are swallowed. See [docs/LLM_GUIDE.md section L](docs/LLM_GUIDE.md#l-progress-notifications-v47).
- **MCP prompts catalog** — the server advertises `prompts/list` + `prompts/get` with 5 canonical workflows: `run_one_shot_command`, `investigate_session`, `upload_and_verify`, `interactive_shell_drive`, `cleanup_agent`. See [docs/LLM_GUIDE.md section M](docs/LLM_GUIDE.md#m-prompts-catalog-v47).
- **Idempotency cache** — mutating tools (15 total) accept a request `_meta.idempotency_key` (1..=256 bytes). Cached response replays within the TTL window (default 300 s, env `SSH_IDEMPOTENCY_TTL_SECS`; cap 1024 entries, env `SSH_IDEMPOTENCY_MAX_ENTRIES`). New error code `IDEMPOTENCY_KEY_TOO_LONG`. Read-only tools ignore the key. See [docs/LLM_GUIDE.md section J](docs/LLM_GUIDE.md#j-idempotency-v47).
- **NOT_FOUND closest-match suggestions** — when `SESSION_NOT_FOUND` / `SHELL_NOT_FOUND` / `COMMAND_NOT_FOUND` / `TRANSFER_NOT_FOUND` / `FORWARD_NOT_FOUND` fires and the relevant repo is non-empty, the `DETAIL:` line carries `closest matches: <id1>, <id2>, <id3>` (top-3 Levenshtein neighbors). Smaller LLMs recover from typos without round-tripping `ssh_list_*`. See [docs/ERRORS.md](docs/ERRORS.md#v47-not_found-closest-match-suggestions).
- **`INITIAL_BUFFER:` line on `ssh_shell_open`** — when the PTY emits stdout within ~100 ms after open, the response embeds the head-truncated bytes (cap 4 KiB) so the LLM can sometimes skip the first `resources/read` round-trip. Tunables: `SSH_SHELL_OPEN_INITIAL_PEEK_MS`, `SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS`, `SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES`. See [docs/LLM_GUIDE.md section O](docs/LLM_GUIDE.md#o-initial_buffer-on-ssh_shell_open-v47).
- **Public MCP API additive** — every v4.6 wire shape is preserved. The text channel is byte-identical; the new `structured_content`, `prompts/*`, `resources/templates/*`, `notifications/progress`, and `_meta.idempotency_key` surfaces are additive. v3 / v4.0 / v4.1 / v4.5 / v4.6 hosts that walk the markdown body line-by-line into a key-value map keep working without change.

### Carried forward from v4.6.0

- **`NEXT:` advisory line** — every response with a clear successor ends with `NEXT: <pipe-separated tool calls>` listing concrete next-step calls. Coverage matrix in [docs/LLM_GUIDE.md section E](docs/LLM_GUIDE.md#e-next-advisory-line-v46).
- **Subscribe-first `HINT:` lines** — `SSH_SHELL_OPEN: OK`, `SSH_EXECUTE: STARTED`, `SSH_UPLOAD/DOWNLOAD: STARTED`, `SSH_FORWARD: OK` — each carries `HINT: subscribe to <uri> for realtime ...`.
- **`AGENT:` -> `AGENT_ID:` (narrow wire change)** — renamed for consistency with every other ID field. Affects 7 render sites; hosts that grep `^AGENT:` literally must update.
- **JSON Schema `default` keywords + one-line `Cost:` hints** — every optional arg surfaces its default on the schema; every tool description ends with a `Cost:` line.
- **`Implementation.icons` wired** — single hosted SVG entry on the `Implementation` advertised on `initialize`.
- **All 14 wire error tags live** — `FORWARD_FAILED`, `LOCAL_NOT_FILE`, `REMOTE_METADATA_ERROR` reached the wire in v4.6.

### Carried forward from v4.5.0

- **LLM UX overhaul** — the wire contract documented for years is now actually emitted on every read.
- **Stable `PeerId`** — derived from `Mcp-Session-Id` (HTTP) or a `Stdio` singleton key. Subscribe and unsubscribe addressed to the same connection share a single id, so per-peer cursors no longer reset between requests.
- **`_meta` envelope on `resources/read`** — every response embeds `kind`, `cursor`, `buffer_size`, `last_seq`, and `status` (cursor + `buffer_size` only on the byte-stream resources). MIME types are explicit: `text/plain` for `shell://`, block-style `text/plain` for `command://`, `application/json` for `transfer:// session:// forward://`.
- **Granular wire error codes** — 14 error tags reach the wire as of v4.6 (the v4.5 set of 11 emitted plus `FORWARD_FAILED`, `LOCAL_NOT_FILE`, `REMOTE_METADATA_ERROR` newly wired). Untagged messages still fall through to the flat codes (`INVALID_ARGUMENT`, `TRANSPORT_ERROR`, `SFTP_ERROR`).
- **Server identity** — the `Implementation` advertised on `initialize` carries `title = "SSH Remote Shell"`, a multi-line `description`, `website_url = "https://github.com/farchanjo/ssh-mcp"`, and (v4.6) the icon URL above.
- **18 tool annotations + few-shot `instructions`** — every tool emits a `Tool.title` plus `ToolAnnotations.{read_only_hint, destructive_hint, idempotent_hint}` so MCP hosts can rank, filter, and warn before destructive use. The `instructions` field carries three canonical workflows (run command, interactive shell, upload) for smaller LLMs.
- **`ssh_forward` emits `FORWARD_ID` + `SESSION_ID`** — callers can construct the matching `forward://<FORWARD_ID>/events` subscribe URI without a round-trip through `resources/list`.

### Carried forward from v4.4.0

- **Connection lifecycle steering** — `EXPIRES_AT` (RFC3339), `PERSISTENT: true|false`, and an anti-leak `HINT:` line when an `agent_id` owns more than 5 sessions. `agent_id` is now first-class on `ssh_connect` and ranks reuse matches when set.

### Carried forward from v4.1.0

- **Deep decouple complete** — H17.6 removed the entire `src/mcp/` foundational tree (~6 500 LOC). Every former `crate::mcp::*` reference now lives at `crate::adapters::{ssh,sftp,config,subscription}::internal::*` (or `adapters::subscription::legacy` for the transitional global registry). Each adapter is self-contained.
- **`async-trait` direct dep dropped** — the surviving v3 strategy chain was rewritten to native AFIT inside `src/adapters/ssh/internal/auth/` with an enum dispatcher replacing dyn. Any `async-trait` copies left in the dependency tree are transitive (rmcp, etc.) and outside our control.

### Carried forward from v4.0.0

- **Hexagonal (Ports and Adapters) architecture** — `src/{domain, ports, application, adapters, infra, composition}/`. Use cases live under `src/application/` (one struct per business operation), take their ports as generic parameters (static dispatch via `trait-variant` AFIT — **no `Box<dyn Trait>` in hot paths**), and are unit-tested against in-memory fakes with **zero rmcp / russh / SFTP machinery in the test path**.
- **Compile-time wiring root** — `src/composition/{prod, fixtures}.rs` pins concrete adapters at compile time so wiring errors surface at `cargo build` rather than runtime.
- **PeerHandle abstraction** — use cases interact with rmcp peers through a sync handle (`subscribe`, `unsubscribe`, `notify`) instead of holding `Peer<RoleServer>` inside hot DashMap values.
- **Shared SSH handle registry** — `SshHandleRegistry` lets the SFTP adapter reuse the russh handle from `RusshClient` instead of opening a second connection per session.
- **HTTP root-mount fix** under `axum` 0.8 — `composition::prod` switches to `Router::fallback_service` when `MCP_HTTP_PATH = "/"`.

## Features

- **rmcp 1.6 transport** — Streamable HTTP MCP (with SSE notifications) hosted by `axum`, plus stdio.
- **21 MCP tools** (20 without `port_forward`) — full SSH lifecycle, async commands, interactive shells, semantic keystrokes, SFTP, port forwarding (feature-gated). v4.7 adds `ssh_run` (one-shot connect+execute+disconnect), `ssh_execute_batch` (sequential 1..=16 commands), `ssh_disconnect_many` (batch disconnect 1..=64 ids).
- **5 resource subscribe schemes** — push notifications for shell output, command output, transfer progress, session health, port-forward events.
- **Native async SSH** — `russh` 0.55, no `spawn_blocking`, no C dependencies.
- **Multiple auth methods** — Password, key file (explicit or auto-discovered in `~/.ssh/`), SSH agent.
- **Smart session reuse** — `ssh_connect.reuse = "suggest" | "auto" | "force_new"` (typed enum).
- **Smart retry** — exponential backoff via `backon`, retry only on transient failures.
- **Lock-free hot-path state** — `DashMap`, `ArcSwap`, `OnceCell`, atomic counters, `tokio::sync::broadcast`, `tokio::sync::Notify`. Zero `Mutex` fields on hot-path types — enforced by clippy invariants.
- **Bounded buffers** — head-drained caps for both command and shell output protect long-running processes from OOM.
- **Anti-injection nonces** — every output block carries an 8-hex-char nonce, UTF-8 safe truncation, and `(truncated …)` annotations.
- **Port forwarding** — efficient bidirectional tunneling (feature: `port_forward`, default on).
- **Strict clippy baseline** — Layer A forbid, Layer B deny (`pedantic`, `nursery`, `cargo`), plus 6 lock-free invariants (`await_holding_lock`, `mutex_atomic`, …). All `#[allow]` attributes carry `reason = "..."`.

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture](docs/ARCHITECTURE.md) | v4 hexagonal layout, layer-by-layer module map, dependency graph, sequence diagrams |
| [API Reference](docs/API.md) | All 21 MCP tools (inputs, outputs, structured_content, errors) |
| [Resources](docs/RESOURCES.md) | The 5 resource subscribe schemes + cursor / sequence semantics |
| [LLM Guide](docs/LLM_GUIDE.md) | Token-efficient subscribe-first patterns for LLM clients |
| [Flows](docs/FLOWS.md) | Sequence diagrams for connect / execute / shell / SFTP / subscribe |
| [Configuration](docs/CONFIGURATION.md) | Full env-var table (25+ vars, floors and caps) |
| [Errors](docs/ERRORS.md) | Error code catalogue (REASON codes + recovery hints) |
| [Locks](docs/LOCKS.md) | Lock-free invariants per layer + clippy enforcement |
| [Migration v3 → v4](docs/MIGRATION_v3_to_v4.md) | Codebase contributor migration guide (the public MCP API is unchanged) |
| [Migration v2 → v3](docs/MIGRATION_v2_to_v3.md) | Historical client upgrade guide |
| [ADR-0001](docs/adr/0001-migrate-to-rmcp.md) | Decision: migrate poem-mcpserver → rmcp 1.6 (v3) |
| [ADR-0002](docs/adr/0002-adopt-hexagonal-architecture.md) | Decision: adopt Hexagonal (Ports and Adapters) for v4 |

## Quick Start

### Build

```bash
git clone https://github.com/farchanjo/ssh-mcp.git
cd ssh-mcp
cargo build --release
cargo test --lib --quiet      # 1156 tests
cargo test --tests --quiet    # 2 integration tests (incl. v4 composition smoke)
```

### Install

```bash
sudo cp ./target/release/ssh-mcp-stdio /usr/local/bin/
sudo cp ./target/release/ssh-mcp /usr/local/bin/
sudo codesign -f -s - /usr/local/bin/ssh-mcp{,-stdio}  # macOS only
```

### Option 1: Stdio Transport (recommended)

`ssh-mcp-stdio` reads JSON-RPC frames from stdin and writes responses + notifications on stdout. Logs go to stderr (set `RUST_LOG=debug` for verbose tracing). The MCP transport is `rmcp::transport::io::stdio()` — no custom JSON-RPC shim.

```bash
# Run directly (for development)
cargo run --release --bin ssh-mcp-stdio
```

<details>
<summary>MCP client configuration</summary>

#### Claude Code / Claude Desktop / Cursor / OpenCode

```json
{
  "mcpServers": {
    "ssh": {
      "command": "ssh-mcp-stdio",
      "args": []
    }
  }
}
```

</details>

### Option 2: HTTP Transport (`ssh-mcp`)

`ssh-mcp` hosts the rmcp `StreamableHttpService` behind an `axum` 0.8 router on `MCP_HOST:MCP_PORT` (default `0.0.0.0:8000`) at `MCP_HTTP_PATH` (default `/`). MCP sessions are tracked through the `Mcp-Session-Id` header per the Streamable HTTP spec; notifications are pushed over the SSE channel established by the same endpoint.

> **v4 fix**: `MCP_HTTP_PATH = "/"` is now wired via `Router::fallback_service` (axum 0.8 panics on a nested `/` mount). Tunable to any path (`/mcp`, `/api/v1/mcp`, …) without touching the binary.

```bash
# Default
ssh-mcp

# Custom bind
MCP_HOST=127.0.0.1 MCP_PORT=9000 ssh-mcp

# Verbose tracing
RUST_LOG=debug ssh-mcp
```

<details>
<summary>HTTP client configuration</summary>

#### Claude Code

```json
{
  "mcpServers": {
    "ssh": {
      "url": "http://localhost:8000/"
    }
  }
}
```

#### Direct HTTP — initialize then call

The Streamable HTTP transport requires a session handshake. Capture `Mcp-Session-Id` from the `initialize` response and replay it on every subsequent request:

```bash
# 1. initialize → server returns Mcp-Session-Id header
curl -i -X POST http://localhost:8000/ \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}
      }'

# 2. list sessions, replaying Mcp-Session-Id from step 1
curl -X POST http://localhost:8000/ \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: <id-from-step-1>" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call",
       "params":{"name":"ssh_list_sessions","arguments":{}}}'
```

</details>

<details>
<summary>macOS launchd service</summary>

Install as a system service that starts on boot:

```bash
sudo mkdir -p /usr/local/var/log /usr/local/var/ssh-mcp
sudo cp com.farchanjo.ssh-mcp.plist /Library/LaunchDaemons/
sudo launchctl load /Library/LaunchDaemons/com.farchanjo.ssh-mcp.plist
sudo launchctl list | grep ssh-mcp
tail -f /usr/local/var/log/ssh-mcp.log
```

For a user-level service (no sudo, runs only when logged in):

```bash
cp com.farchanjo.ssh-mcp.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.farchanjo.ssh-mcp.plist
```

</details>

## MCP Tools (21 with `port_forward`, 20 without)

| Group | Tools |
|-------|-------|
| **Connection** | `ssh_connect` (typed `ReusePolicy`), `ssh_disconnect`, `ssh_disconnect_many` *(v4.7)*, `ssh_list_sessions`, `ssh_disconnect_agent` |
| **Commands** | `ssh_execute` (optional `pty=true`), `ssh_execute_batch` *(v4.7)*, `ssh_run` *(v4.7)*, `ssh_get_command_output`, `ssh_list_commands` (typed `CommandStatus`), `ssh_cancel_command` |
| **Shell** *(subscribe-first)* | `ssh_shell_open` *(v4.7 `INITIAL_BUFFER:`)*, `ssh_shell_write`, `ssh_shell_send_key`, `ssh_shell_read` (long-poll: `wait` / `wait_timeout_secs` / `min_bytes`), `ssh_shell_wait_for`, `ssh_shell_close` |
| **SFTP** | `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress` |
| **Network** *(feature-gated)* | `ssh_forward` |

Each session serializes one russh channel at a time through a per-session semaphore, so rapid `execute + cancel` bursts never race OpenSSH's `MaxSessions` budget. The shared `SshHandleRegistry` lets the SFTP adapter reuse the same russh handle for file transfers.

For the full input schema and response shape of each tool, see [docs/API.md](docs/API.md).

## Resources (subscribe-first)

The 5 resource schemes are the canonical way to consume realtime updates without polling. Subscribe via `resources/subscribe`; the server pushes `notifications/resources/updated` over your transport (SSE for HTTP, stdout for stdio) and you read with `resources/read`.

| URI Scheme | Description | Cursor |
|------------|-------------|--------|
| `shell://<shell-id>/output` | PTY output stream | yes (`?cursor=auto` or absolute byte offset) |
| `command://<command-id>/output` | Async command stdout/stderr | yes |
| `transfer://<transfer-id>/progress` | SFTP point-in-time progress | no (snapshot) |
| `session://<session-id>/health` | Session health snapshot | no |
| `forward://<forward-id>/events` | Port-forward event log (feature-gated) | yes |

The subscription registry **debounces** events (default 50 ms coalesce, 1 s force-flush, 30 s keepalive — all tunable via env vars). Each event carries a sequence number for gap detection; lagged subscribers automatically recover from a snapshot. The v4 adapter `MemoryRegistry<N>` is generic over the notifier port — no `Box<dyn>` in the push hot path.

See [docs/RESOURCES.md](docs/RESOURCES.md) for the full contract and [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md) for token-efficient consumption patterns.

## Response Format

All 21 (or 20) MCP tools return BOTH a **single markdown `String`** in block style AND (v4.7) a typed JSON object on `structured_content`:

- **First line**: `TOOL_NAME: STATUS` (`OK`, `REUSED`, `SUGGESTED`, `STARTED`, `RUNNING`, `COMPLETED`, `FAILED`, `TIMEOUT`, `CANCELLED`, `NOOP`, `OPEN`, `CLOSED`, `ACTIVE`, `ERROR`, …).
- **Body**: one `KEY: value` per line. All identifiers carry the `_ID` suffix (`SESSION_ID`, `COMMAND_ID`, `SHELL_ID`, `TRANSFER_ID`).
- **Output blocks** carry an 8-hex-char nonce per response to prevent injection from content that imitates the markers:

  ```
  --- stdout [a3f2b1d7] ---
  <content>
  --- stderr [a3f2b1d7] (empty) ---
  ```

  Truncation is annotated as `(truncated: showing 16.0KB of 2.3MB)`; partial output of a still-running command is annotated as `(partial)`.
- **Errors**:

  ```
  SSH_EXECUTE: ERROR
  REASON: [CODE] human-readable reason
  DETAIL: optional context (truncated to 2 KiB)
  ```

  The full error code catalogue lives in [docs/ERRORS.md](docs/ERRORS.md).

> **Wire compatibility**: the v4 / v4.7 markdown shape is byte-identical to v3 on the text channel (verified by snapshot tests in `tests/v4_smoke.rs`). v3 / v4.x clients keep working without any change. The v4.7 `structured_content` payload sits next to the text body — clients that ignore it stay byte-compatible.

A Python helper (`parse_mcp_response`) ships in `scripts/test_http.py` and `scripts/test_stdio.py` for clients that need to map markdown back into legacy field names.

## Usage Examples

<details>
<summary>Connect with SSH agent</summary>

```json
{
  "tool": "ssh_connect",
  "arguments": {
    "address": "example.com:22",
    "username": "user"
  }
}
```

```
SSH_CONNECT: OK
SESSION_ID: 550e8400-e29b-41d4-a716-446655440000
HOST: user@example.com:22
RETRY: 0
PERSISTENT: false
EXPIRES_AT: 2026-05-03T18:30:00+00:00
NEXT: ssh_execute(session_id=550e8400-..., command=...) | ssh_shell_open(session_id=550e8400-...) | ssh_disconnect(session_id=550e8400-...)
```

The `AGENT_ID:` line is appended when `agent_id` is passed (renamed from `AGENT:` in v4.6). The trailing `NEXT:` line (v4.6) names the three most-likely successor calls.

</details>

<details>
<summary>Smart reuse</summary>

```json
{ "tool": "ssh_connect",
  "arguments": { "address": "example.com:22", "username": "user", "reuse": "auto" } }
```

- `"suggest"` (default): returns `SSH_CONNECT: SUGGESTED` listing matching sessions without connecting.
- `"auto"`: reuses the most recent healthy match → `SSH_CONNECT: REUSED`.
- `"force_new"`: skips the lookup, always creates a new connection.

In every mode, unhealthy matches are disconnected and reported as `REPLACED: N`.

</details>

<details>
<summary>Async command execution</summary>

```json
{ "tool": "ssh_execute",
  "arguments": { "session_id": "<sid>", "command": "npm run build", "timeout_secs": 300 } }
```

```
SSH_EXECUTE: STARTED
COMMAND_ID: abc123
SESSION_ID: <sid>
```

Poll or block:

```json
{ "tool": "ssh_get_command_output",
  "arguments": { "command_id": "abc123", "wait": true,
                 "wait_timeout_secs": 60, "max_output_bytes": 32768 } }
```

For long-running output, **subscribe to `command://abc123/output`** instead of polling.

</details>

<details>
<summary>Interactive shell + semantic keystrokes</summary>

Open a PTY:

```json
{ "tool": "ssh_shell_open",
  "arguments": { "session_id": "<sid>", "term": "xterm", "cols": 80, "rows": 24 } }
```

Send semantic keystrokes (no manual `\x03` / `\x1b[A` mapping):

```json
{ "tool": "ssh_shell_send_key",
  "arguments": { "shell_id": "<shid>", "key": "ctrl_c" } }

{ "tool": "ssh_shell_send_key",
  "arguments": { "shell_id": "<shid>", "key": "tab", "modifiers": ["shift"] } }   // back-tab

{ "tool": "ssh_shell_send_key",
  "arguments": { "shell_id": "<shid>", "key": "down", "repeat": 5 } }
```

Wait for a prompt instead of sleeping:

```json
{ "tool": "ssh_shell_wait_for",
  "arguments": { "shell_id": "<shid>", "patterns": ["$ ", "# "], "timeout_secs": 30 } }
```

Or **subscribe to `shell://<shid>/output`** for push notifications with cursor-based replay.

</details>

<details>
<summary>SFTP upload with progress subscription</summary>

```json
{ "tool": "ssh_upload",
  "arguments": { "session_id": "<sid>",
                 "local_path": "~/Downloads/backup.tar.gz",
                 "remote_path": "/var/backups/backup.tar.gz" } }
```

```
SSH_UPLOAD: STARTED
TRANSFER_ID: xfer-1
SIZE: 245.7MB (257632256 bytes)
```

Either poll `ssh_get_transfer_progress` or subscribe to `transfer://xfer-1/progress` for push.

</details>

<details>
<summary>Port forward (feature-gated)</summary>

```json
{ "tool": "ssh_forward",
  "arguments": { "session_id": "<sid>", "local_port": 8080,
                 "remote_address": "localhost", "remote_port": 3000 } }
```

```
SSH_FORWARD: OK
LOCAL: 127.0.0.1:8080
REMOTE: localhost:3000
ACTIVE: true
```

Subscribe to `forward://<fid>/events` for accept / close / error events.

</details>

## Configuration

Priority: **Parameter > Environment Variable > Default**. The full table (25+ vars including the broadcast caps, debouncer, and peer GC vars) lives in [docs/CONFIGURATION.md](docs/CONFIGURATION.md). Most-used vars:

| Variable | Default | Description |
|----------|---------|-------------|
| `MCP_HOST` / `MCP_PORT` | `0.0.0.0` / `8000` | HTTP transport bind |
| `MCP_HTTP_PATH` | `/` | HTTP transport path (root mount uses fallback service in v4) |
| `RUST_LOG` | `info` | Log filter |
| `SSH_CONNECT_TIMEOUT` | 30s | Connection timeout |
| `SSH_COMMAND_TIMEOUT` | 180s | Command execution timeout |
| `SSH_INACTIVITY_TIMEOUT` | 300s | Session inactivity timeout (disabled when `persistent=true`) |
| `SSH_SHELL_INACTIVITY_TTL` | 600s | Shell auto-close on idle |
| `SSH_SHELL_MAX_BUFFER_SIZE` | 10m | Shell output buffer cap (`b`/`k`/`m`/`g`/`t`) |
| `SSH_COMMAND_MAX_BUFFER_SIZE` | 10m | Per-command output cap |
| `SSH_NOTIFY_DEBOUNCE_MS` | 50 | Subscribe debounce window |
| `SSH_NOTIFY_FORCE_FLUSH_MS` | 1000 | Subscribe force-flush |
| `SSH_NOTIFY_KEEPALIVE_S` | 30 | Subscribe keepalive |
| `SSH_MCP_PEER_GC_INTERVAL_S` | 30 | Peer-GC scan interval |
| `SSH_*_BROADCAST_CAP` | 1024 / 256 | Broadcast channel capacity |

## Limits

> [!note]
> - Max 100 concurrent async commands per session.
> - Max 10 concurrent interactive shells per session.
> - Max 10 concurrent SFTP transfers per session.
> - One russh channel at a time per session (semaphore-serialized → OpenSSH `MaxSessions` friendly).
> - Output blocks default to 16 KiB per response (cap 1 MiB) — tune with `max_output_bytes`.
> - List tools default to 500 items (cap 10 000) — tune with `max_items`.

## Architecture

v4.1.0 ships the deep-decoupled hexagonal layout (no surviving `src/mcp/` tree):

```
src/
├── main.rs                — HTTP transport thin shell (axum 0.8 + StreamableHttpService); calls composition::prod
├── bin/ssh_mcp_stdio.rs   — Stdio transport thin shell (rmcp::transport::io::stdio()); calls composition::prod
├── domain/                — pure entities + value objects + errors + live event variants (no I/O, no async)
├── ports/                 — trait skeletons (sync via plain trait, async via trait-variant AFIT)
├── application/           — 22 use cases (one struct per business operation; generic over ports)
├── adapters/              — concrete adapters; each one self-contained (own internal/ subtree where needed)
│   ├── ssh/               — RusshClient + SshHandleRegistry; internal/{client,session,async_command,shell,types,error}.rs + internal/auth/ (AFIT chain)
│   ├── sftp/              — RusshSftpClient + InMemorySftp; internal/{sftp,transfer,types}.rs
│   ├── repo/dashmap/      — lock-free in-memory repos for every domain entity
│   ├── auth/              — port-side AuthChainAdapter (delegates to ssh/internal/auth)
│   ├── clock/             — system + fake
│   ├── config/            — EnvConfig + internal/ env-var resolvers
│   ├── id_generator/      — uuid + deterministic
│   ├── notifier/          — RmcpAdapter + RmcpPeer (PeerHandle abstraction)
│   ├── output_stream/     — RusshOutput + InMemory PTY broadcast
│   └── subscription/      — MemoryRegistry<N> + legacy.rs (transitional SUBSCRIPTION_REGISTRY + spawn_peer_gc)
├── infra/mcp/             — inbound rmcp surface
│   ├── server.rs          — McpSshServer<UC> (generic over the use-case set)
│   ├── tool_router.rs     — the 18 #[tool] entry points
│   ├── resource_handlers.rs — resources/list, read, subscribe, unsubscribe
│   ├── peer_handle.rs     — PeerTable re-export for binaries
│   ├── args/              — per-tool Deserialize + JsonSchema structs
│   ├── render/            — per-domain markdown builders
│   └── helpers/           — error / nonce / output rendering primitives
└── composition/           — wiring root
    ├── prod.rs            — production adapter set (russh + russh-sftp + DashMap + env-config + UUID v4)
    └── fixtures.rs        — deterministic test adapter set (gated by `test-fixtures` feature)
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full layer-by-layer module map, dependency graph, and sequence diagrams.

## Testing

```bash
# Rust unit tests (1156)
cargo test --lib --quiet

# Rust integration tests (2 — incl. v4 composition smoke)
cargo test --tests --quiet

# Combined with all features
cargo test --all-features

# With deterministic test fixtures (use cases composed against in-memory adapters)
cargo test --features test-fixtures

# Python integration suites (require a reachable SSH server)
python3 scripts/test_http.py        # HTTP transport — all 21 tools + resources
python3 scripts/test_stdio.py       # Stdio transport — all 21 tools + resources
python3 scripts/test_send_key.py    # ssh_shell_send_key coverage
python3 scripts/test_wait_for.py    # ssh_shell_wait_for coverage
python3 scripts/test_resources.py   # 5 resource schemes + subscribe + cursor

# Stress scripts (Python)
python3 scripts/stress_subscribe.py          # subscribe burst
python3 scripts/stress_concurrent_writes.py  # writer-task ownership
python3 scripts/stress_lagged_sub.py         # lagged-subscriber recovery
python3 scripts/stress_locks.py              # lock-free hot-path

# Loom invariants (gated; currently blocked upstream)
RUSTFLAGS="--cfg loom" cargo test --test lockfree_invariants
```

| Suite | Count |
|-------|-------|
| Lib unit tests | **1156** |
| Integration tests | **2** (incl. `tests/v4_smoke.rs`) |
| Python integration scripts | 5 |
| Python stress scripts | 4 |
| Loom invariant tests | 8 (gated) |

## Binary Targets

| Binary | Description |
|--------|-------------|
| `ssh-mcp` | HTTP transport: axum 0.8 + rmcp `StreamableHttpService` (default `0.0.0.0:8000/`); composition::prod |
| `ssh-mcp-stdio` | Stdio transport: `rmcp::transport::io::stdio()` (logs to stderr); composition::prod |

Both binaries are thin shells over `composition::prod` — the same wiring root, only the transport differs.

## Credits

- Original concept: [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp)
- SSH implementation: [russh](https://github.com/warp-tech/russh)
- SFTP implementation: [russh-sftp](https://github.com/AspectUnk/russh-sftp)
- Retry logic: [backon](https://github.com/Xuanwo/backon)
- MCP framework: [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Anthropic Rust SDK)
- HTTP host: [axum](https://github.com/tokio-rs/axum) + [tower](https://github.com/tower-rs/tower)
- AFIT trait pattern: [trait-variant](https://github.com/rust-lang/impl-trait-utils)
- Lock-free primitives: [arc-swap](https://github.com/vorner/arc-swap), [dashmap](https://github.com/xacrimon/dashmap)

## License

MIT — declared via `license = "MIT"` in `Cargo.toml`.

## Contributing

Contributions welcome. Please ensure:

- All tests pass: `cargo test --all-features`.
- No clippy warnings: `cargo clippy --all-features --all-targets --workspace -- -D warnings`.
- Code formatted: `cargo fmt --all -- --check`.
- For changes touching shell / command / transfer hot-path state, see [docs/LOCKS.md](docs/LOCKS.md) before introducing any `Mutex`.
- For changes that span multiple layers, read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) first to find the right layer for your change (domain / port / use case / adapter / infra) and [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) for the per-layer responsibility table.
