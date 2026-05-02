# Changelog

All notable changes to ssh-mcp are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [3.0.0] — 2026-05-02

### Breaking
- Migrate MCP transport layer from `poem-mcpserver` 0.3.1 to `rmcp` 1.6 (official Anthropic Rust SDK). HTTP transport now uses `axum` + `rmcp::transport::streamable_http_server::StreamableHttpService` with `Mcp-Session-Id` header tracking. Stdio transport uses `rmcp::transport::io::stdio()`.
- `ssh_connect.reuse` is now a typed enum `ReusePolicy { Suggest, Auto, ForceNew }` instead of `Option<String>`. Wire format unchanged for valid values; typos now produce a JSON-schema validation error.
- `ssh_list_commands.status` is now a typed enum `CommandStatus { Running, Completed, Cancelled, Failed }` instead of `Option<String>`.
- All MCP responses are now block-style markdown only (drops the v2 inline `KEY: value | KEY: value` form).
- Stdio binary's custom JSON-RPC quirks (cancel-id parser, fallback responses) removed — handled natively by rmcp. ~250 LOC of workarounds dropped.

### Added
- 5 MCP resource subscribe schemes:
  - `shell://<id>/output` (PTY output, cursor support)
  - `command://<id>/output` (async command stdout/stderr, cursor support)
  - `transfer://<id>/progress` (SFTP point-in-time progress)
  - `session://<id>/health` (session health snapshot)
  - `forward://<id>/events` (port-forward event log, feature-gated)
- `ssh_shell_send_key` MCP tool — semantic keystrokes (ctrl_a..ctrl_z, enter, tab, escape, backspace, space, delete, arrows, nav keys, F1-F12) with shift/alt/ctrl modifier support and 1..=64 repeat. Tab+Shift produces back-tab (`\x1b[Z`).
- `ssh_shell_wait_for` MCP tool — multi-pattern (up to 16) substring gate with timeout.
- `ssh_shell_read` long-poll extension: `wait` / `wait_timeout_secs` / `min_bytes` parameters for fallback over subscribe.
- Subscription registry (`src/mcp/subscription.rs`) with per-resource debouncer (50 ms coalesce, 1 s force-flush, 30 s keepalive), per-(peer,uri) cursor tracking, sequence numbers per event for gap detection, lagged auto-recovery via snapshot, periodic peer GC.
- 9 new env vars:
  - `SSH_COMMAND_BROADCAST_CAP` (default 1024, floor 16, cap 65536)
  - `SSH_SHELL_BROADCAST_CAP` (default 1024, floor 16, cap 65536)
  - `SSH_TRANSFER_BROADCAST_CAP` (default 256, floor 8, cap 4096)
  - `SSH_SESSION_BROADCAST_CAP` (default 256, floor 8, cap 4096)
  - `SSH_FORWARD_BROADCAST_CAP` (default 256, floor 8, cap 4096; feature-gated)
  - `SSH_NOTIFY_DEBOUNCE_MS` (default 50, floor 5, cap 5000)
  - `SSH_NOTIFY_FORCE_FLUSH_MS` (default 1000, floor 100, cap 60000)
  - `SSH_NOTIFY_KEEPALIVE_S` (default 30, floor 5, cap 300)
  - `SSH_MCP_PEER_GC_INTERVAL_S` (default 30, floor 5, cap 300)
- 6 new docs files: `LLM_GUIDE.md`, `RESOURCES.md`, `ERRORS.md`, `LOCKS.md`, `MIGRATION_v2_to_v3.md`, `adr/0001-migrate-to-rmcp.md`.
- 4 new Python stress test scripts.
- ~145 new Rust unit + integration tests; 8 loom invariant tests (gated, blocked by upstream tokio/loom incompatibility in russh+axum — documented).

### Changed
- Lock-free refactor of all hot-path state types:
  - `RunningCommand` — `ArcSwap<OutputBuffer>` + `broadcast::Sender<OutputChunk>` + `OnceCell<i32>` exit_code + `OnceCell<String>` error.
  - `RunningShell` — `ArcSwap<RingBuffer>` + `broadcast::Sender<Bytes>` + `mpsc::Sender<WriteRequest>` (writer task owns ChannelWriter exclusively) + `AtomicU64` last_activity_ms + `Notify` data_notify.
  - `RunningTransfer` — `OnceCell<String>` error + `broadcast::Sender<ProgressEvent>` + `Notify`.
  - `SessionRef` — `broadcast::Sender<HealthEvent>`.
  - `ForwardHandle` — `broadcast::Sender<ForwardEvent>` (feature-gated).
- 0 `Mutex` fields on hot-path state types after this release (verified via `grep`).
- Strict clippy baseline expanded with v3 lock-free invariants: `await_holding_lock`, `await_holding_refcell_ref`, `significant_drop_in_scrutinee`, `significant_drop_tightening`, `mutex_atomic`, `mutex_integer`.
- Tool count: 16 → 18.
- Test count: 502 → 832 (lib + integration).

### Fixed
- `ssh_get_command_output.command_id` doc no longer references the non-existent `ssh_execute_async` tool.
- `ssh_disconnect`/`ssh_shell_close` parameter docs now use the canonical "_ID returned from X" phrasing.

### Removed
- `poem`, `poem-mcpserver` deps.
- `src/mcp/commands.rs` (the v2 monolithic 2272-LOC tools file) — split into `src/mcp/tools/{connection,execute,shell,sftp,forward,legacy_helpers}.rs`.
- The custom stdio cancel/fallback shim (~250 LOC).

### Migration
See `docs/MIGRATION_v2_to_v3.md` for client upgrade instructions.

### Commit hashes (v3 etapa trail)
- E1 `3f65152` rmcp foundation
- E3 `a956191` ssh_connect canary
- E4 `e17be5d` 15 remaining tools
- E7 `ce8497d` RunningCommand lock-free
- E8 `4bee2c9` RunningShell lock-free
- E9 `21b8fe1` RunningTransfer + Session/Forward broadcast
- E10 `f9652c7` keys + ssh_shell_send_key
- E11 `83aa193` long-poll + ssh_shell_wait_for
- E12 `d87980d` subscription registry + backpressure
- E13 `d2798ff` resources.rs URI handlers
- E14 `f7b7683` ServerHandler wiring + peer GC
- E6 `b96d724` format consistency
- E15 `545a680` Rust unit + loom + integration tests
- E16 `6fbc97f` Python integration + stress
- E17 `e3051f6` docs rewrite
- E18 `a197422` docs new (LLM/RESOURCES/ERRORS/LOCKS/MIGRATION/ADR)

## [2.0.1] — 2025-10-19

See `git log` for the v2.x changes; this CHANGELOG was introduced in v3.0.0.

[3.0.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v3.0.0
[2.0.1]: https://github.com/farchanjo/ssh-mcp/releases/tag/v2.0.1
