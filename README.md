# SSH-MCP Server

[![Rust](https://img.shields.io/badge/rust-2024-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](Cargo.toml)
[![Tests](https://img.shields.io/badge/tests-832%20passing-brightgreen.svg)]()
[![Version](https://img.shields.io/badge/version-3.0.0-blue.svg)]()
[![Transport](https://img.shields.io/badge/transport-rmcp%201.6-purple.svg)]()

> [!caution]
> This is **NOT** the original [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp).
> Everything has been rewritten from scratch — different SSH library, different
> MCP transport (rmcp 1.6), different threading model, lock-free state.

A Rust SSH server with full Model Context Protocol (MCP) integration, enabling
LLMs to connect to SSH servers, execute commands, drive interactive shells with
**realtime resource subscriptions**, and stream files via SFTP — all over the
**rmcp 1.6** Streamable HTTP transport (axum-hosted) or stdio.

> **Upgrading from v2.x?** See [docs/MIGRATION_v2_to_v3.md](docs/MIGRATION_v2_to_v3.md).
> For LLM-side token-efficient usage patterns, see [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md).

[[_TOC_]]

## What's New in v3.0.0

- **Official Anthropic MCP SDK** — migrated from `poem-mcpserver` to **`rmcp` 1.6** (HTTP via `axum` 0.7 + `StreamableHttpService`; stdio via `rmcp::transport::io::stdio()`).
- **18 MCP tools** (was 16) — added `ssh_shell_send_key` (semantic keystrokes) and `ssh_shell_wait_for` (multi-pattern gate).
- **5 MCP resource subscribe schemes** — `shell://`, `command://`, `transfer://`, `session://`, `forward://`. Subscribe-first for shells: stop polling, get push notifications with debouncing, sequence numbers, and lagged auto-recovery.
- **Lock-free hot-path state** — every `RunningCommand` / `RunningShell` / `RunningTransfer` / `SessionRef` / `ForwardHandle` carries **zero `Mutex` fields**. Backed by `ArcSwap` + `OnceCell` + atomic counters + `tokio::sync::broadcast` + `Notify`.
- **Block-only markdown responses** — the v2 inline form was dropped. All 18 tools now return one `KEY: value` per line.
- **832 tests** (820 lib + 12 integration) + Python integration suites + 4 stress scripts.

## Features

- **rmcp 1.6 transport** — Streamable HTTP MCP (with SSE notifications) hosted by `axum`, plus stdio.
- **18 MCP tools** — full SSH lifecycle, async commands, interactive shells, semantic keystrokes, SFTP, port forwarding (feature-gated).
- **5 resource subscribe schemes** — push notifications for shell output, command output, transfer progress, session health, port-forward events.
- **Native async SSH** — `russh` 0.55, no `spawn_blocking`, no C dependencies.
- **Multiple auth methods** — Password, key file (explicit or auto-discovered in `~/.ssh/`), SSH agent.
- **Smart session reuse** — `ssh_connect.reuse = "suggest" | "auto" | "force_new"` (typed enum).
- **Smart retry** — exponential backoff via `backon`, retry only on transient failures.
- **Lock-free storage** — `DashMap` plus per-session secondary indices for O(1) lookups.
- **Bounded buffers** — head-drained caps for both command and shell output protect long-running processes from OOM.
- **Anti-injection nonces** — every output block carries an 8-hex-char nonce, UTF-8 safe truncation, and `(truncated …)` annotations.
- **Port forwarding** — efficient bidirectional tunneling (feature: `port_forward`, default on).
- **Strict clippy baseline** — 30+ deny lints + 6 v3 lock-free invariants enforced (`await_holding_lock`, `mutex_atomic`, …).

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture](docs/ARCHITECTURE.md) | rmcp 1.6 layout, module map, lock-free patterns |
| [API Reference](docs/API.md) | All 18 MCP tools (inputs, outputs, errors) |
| [Resources](docs/RESOURCES.md) | The 5 resource subscribe schemes + cursor / sequence semantics |
| [LLM Guide](docs/LLM_GUIDE.md) | Token-efficient subscribe-first patterns for LLM clients |
| [Flows](docs/FLOWS.md) | Sequence diagrams for connect / execute / shell / SFTP / subscribe |
| [Configuration](docs/CONFIGURATION.md) | Full env-var table (25+ vars, floors and caps) |
| [Errors](docs/ERRORS.md) | Error code catalogue (REASON codes + recovery hints) |
| [Locks](docs/LOCKS.md) | Lock-free invariants + clippy enforcement |
| [Migration v2 → v3](docs/MIGRATION_v2_to_v3.md) | Client upgrade guide |
| [ADR-0001](docs/adr/0001-migrate-to-rmcp.md) | Decision: migrate poem-mcpserver → rmcp 1.6 |

## Quick Start

### Build

```bash
git clone https://github.com/farchanjo/ssh-mcp.git
cd ssh-mcp
cargo build --release
cargo test --lib --quiet      # 820 tests
cargo test --tests --quiet    # 12 integration tests
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

`ssh-mcp` hosts the rmcp `StreamableHttpService` behind an `axum` router on `MCP_HOST:MCP_PORT` (default `0.0.0.0:8000`) at `MCP_HTTP_PATH` (default `/`). MCP sessions are tracked through the `Mcp-Session-Id` header per the Streamable HTTP spec; notifications are pushed over the SSE channel established by the same endpoint.

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

## MCP Tools (18)

| Group | Tools |
|-------|-------|
| **Connection** | `ssh_connect` (typed `ReusePolicy`), `ssh_disconnect`, `ssh_list_sessions`, `ssh_disconnect_agent` |
| **Commands** | `ssh_execute` (optional `pty=true`), `ssh_get_command_output`, `ssh_list_commands` (typed `CommandStatus`), `ssh_cancel_command` |
| **Shell** *(subscribe-first)* | `ssh_shell_open`, `ssh_shell_write`, **`ssh_shell_send_key`**, `ssh_shell_read` (long-poll: `wait` / `wait_timeout_secs` / `min_bytes`), **`ssh_shell_wait_for`**, `ssh_shell_close` |
| **SFTP** | `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress` |
| **Network** *(feature-gated)* | `ssh_forward` |

Each session serializes one russh channel at a time through a per-session semaphore, so rapid `execute + cancel` bursts never race OpenSSH's `MaxSessions` budget.

For the full input schema and response shape of each tool, see [docs/API.md](docs/API.md).

## Resources (subscribe-first)

The 5 resource schemes are the v3 way to consume realtime updates without polling. Subscribe via `resources/subscribe`; the server pushes `notifications/resources/updated` over your transport (SSE for HTTP, stdout for stdio) and you read with `resources/read`.

| URI Scheme | Description | Cursor |
|------------|-------------|--------|
| `shell://<shell-id>/output` | PTY output stream | yes (`?cursor=auto` or absolute byte offset) |
| `command://<command-id>/output` | Async command stdout/stderr | yes |
| `transfer://<transfer-id>/progress` | SFTP point-in-time progress | no (snapshot) |
| `session://<session-id>/health` | Session health snapshot | no |
| `forward://<forward-id>/events` | Port-forward event log (feature-gated) | yes |

The subscription registry **debounces** events (default 50 ms coalesce, 1 s force-flush, 30 s keepalive — all tunable via env vars). Each event carries a sequence number for gap detection; lagged subscribers automatically recover from a snapshot.

See [docs/RESOURCES.md](docs/RESOURCES.md) for the full contract and [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md) for token-efficient consumption patterns.

## Response Format

All 18 MCP tools return a **single markdown `String`** in block style:

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

> **v3 breaking change**: the v2 inline form (`TOOL: STATUS | KEY: v | KEY: v`) was dropped. All responses are block-style.

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
```

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
<summary>Interactive shell + semantic keystrokes (v3)</summary>

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

Priority: **Parameter > Environment Variable > Default**. The full table (25+ vars including the 9 added in v3 for broadcast caps, debouncer, and peer GC) lives in [docs/CONFIGURATION.md](docs/CONFIGURATION.md). Most-used vars:

| Variable | Default | Description |
|----------|---------|-------------|
| `MCP_HOST` / `MCP_PORT` | `0.0.0.0` / `8000` | HTTP transport bind |
| `MCP_HTTP_PATH` | `/` | HTTP transport path |
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

The library is split across `src/mcp/` into focused modules:

```
src/
├── main.rs                 — HTTP transport (axum + rmcp StreamableHttpService) + peer GC
├── bin/ssh_mcp_stdio.rs    — Stdio transport (rmcp::transport::io::stdio()) + peer GC
└── mcp/
    ├── server.rs           — McpSshServer (rmcp ServerHandler + #[tool_router])
    ├── tools/              — Per-domain tool impls (connection / execute / shell / sftp / forward)
    ├── resources.rs        — URI parser + 5 resource read handlers
    ├── subscription.rs     — SUBSCRIPTION_REGISTRY + debouncer + cursor + peer GC task
    ├── keys.rs             — Semantic keystroke encoder for ssh_shell_send_key
    ├── client.rs           — russh connect/auth/exec/PTY + retry (backon)
    ├── async_command.rs    — RunningCommand (lock-free)
    ├── shell.rs            — RunningShell (lock-free + writer-task ownership)
    ├── transfer.rs / sftp.rs — SFTP streaming + RunningTransfer (lock-free)
    ├── forward.rs          — Port forwarding (feature-gated)
    ├── session.rs          — SshClientHandler + SessionRef (broadcast)
    ├── storage/            — DashMap-backed storage + secondary indices
    ├── auth/               — AuthChain (password → keys → agent)
    ├── message/            — helpers (nonce, UTF-8 truncate, format_error) + builder
    ├── config.rs           — Param → Env → Default with floors and caps
    ├── error.rs            — Retryable vs non-retryable classification
    ├── schema.rs           — JSON schema helpers
    └── types.rs            — Internal data carriers
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full module map, dependency graph, and lock-free patterns.

## Testing

```bash
# Rust unit tests (820)
cargo test --lib --quiet

# Rust integration tests (12)
cargo test --tests --quiet

# Combined with all features
cargo test --all-features

# Python integration suites (require a reachable SSH server)
python3 scripts/test_http.py        # HTTP transport — all 18 tools + resources
python3 scripts/test_stdio.py       # Stdio transport — all 18 tools + resources
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
| Lib unit tests | **820** |
| Integration tests | **12** |
| Python integration scripts | 5 |
| Python stress scripts | 4 |
| Loom invariant tests | 8 (gated) |

## Binary Targets

| Binary | Description |
|--------|-------------|
| `ssh-mcp` | HTTP transport: axum + rmcp `StreamableHttpService` (default `0.0.0.0:8000/`) |
| `ssh-mcp-stdio` | Stdio transport: `rmcp::transport::io::stdio()` (logs to stderr) |

## Credits

- Original concept: [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp)
- SSH implementation: [russh](https://github.com/warp-tech/russh)
- SFTP implementation: [russh-sftp](https://github.com/AspectUnk/russh-sftp)
- Retry logic: [backon](https://github.com/Xuanwo/backon)
- MCP framework: [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (official Anthropic Rust SDK)
- HTTP host: [axum](https://github.com/tokio-rs/axum) + [tower](https://github.com/tower-rs/tower)
- Lock-free primitives: [arc-swap](https://github.com/vorner/arc-swap), [dashmap](https://github.com/xacrimon/dashmap)

## License

MIT — declared via `license = "MIT"` in `Cargo.toml`.

## Contributing

Contributions welcome. Please ensure:

- All tests pass: `cargo test --all-features`.
- No clippy warnings: `cargo clippy -- -D warnings`.
- Code formatted: `cargo fmt --all`.
- For changes touching shell / command / transfer hot-path state, see [docs/LOCKS.md](docs/LOCKS.md) before introducing any `Mutex`.
