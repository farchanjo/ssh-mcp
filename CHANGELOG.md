# Changelog

All notable changes to ssh-mcp are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [4.1.0] — 2026-05-03

### Highlights

- H17.6 deep decouple closes the v4.0.0 deferral window. Every foundational `crate::mcp::*` reference is gone; `src/mcp/` is deleted (~6 500 LOC removed from the runtime tree). The `async-trait` direct dependency is dropped — every adapter uses native AFIT (statically dispatched through enums where dyn-safety was previously required).
- Public MCP API stays byte-compatible with v4.0.0 (and v3.0.0). Same 18 tools, same 5 resource schemes, same markdown shape, same env vars, same defaults.
- Test count grew from **1023 (1021 lib + 2 integration)** in v4.0.0 to **1016 (1014 lib + 2 integration)** in v4.1 — a small reduction reflects the removal of `mcp::*` internal-state regression tests now redundant against the relocated adapter-internal modules; every public-surface test still passes.

### Removed

- `src/mcp/` tree (~6 500 LOC): `client.rs`, `session.rs`, `sftp.rs`, `shell.rs`, `async_command.rs`, `transfer.rs`, `subscription.rs`, `auth/`, `config.rs`, `error.rs`, `types.rs`. The `crate::mcp::*` namespace no longer exists in `src/lib.rs` — only `domain/`, `ports/`, `application/`, `adapters/`, `infra/`, `composition/` remain at the crate root.
- `async-trait` direct dependency dropped from `Cargo.toml`. The v3 strategy chain that pinned it has been rewritten to native AFIT inside the SSH adapter (`src/adapters/ssh/internal/auth/`); dyn dispatch is replaced by an enum exhaustively dispatching to the three concrete strategies. Any `async-trait` copies that remain in the dependency tree are transitive (e.g. via rmcp) and outside our control.

### Changed

- Foundational types relocated to adapter-internal paths so each adapter is self-contained:
  - `src/adapters/ssh/internal/{client,session,async_command,shell,types,error}.rs` — russh wiring helpers (`connect_to_ssh_with_retry`, `execute_ssh_command`, `open_pty_shell`, `SshClientHandler`), `RunningCommand`, `RunningShell` + `RingBuffer`, `SessionInfo` / `AsyncCommandInfo` / `ShellInfo` / `TransferInfo`, retry classification.
  - `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` — internal `AuthStrategyPort` + AFIT chain (no async-trait, statically dispatched).
  - `src/adapters/sftp/internal/{sftp,transfer,types}.rs` — streaming SFTP transfer state, `RunningTransfer`, shared payload structs.
  - `src/adapters/config/internal/mod.rs` — env-var resolvers feeding `EnvConfig`.
  - `src/adapters/subscription/legacy.rs` — transitional home for `SubscriptionRegistry` + `SUBSCRIPTION_REGISTRY` global + `spawn_peer_gc`. The hexagonal `MemoryRegistry<N>` adapter is the forward-looking replacement consumed by use cases; the legacy adapter coexists until the SSH/SFTP runtime adapters get wired through the port surface end to end.

### Internal

- H17.6 P1 `bf646f9` — relocate v3 internals to adapter-internal modules under `src/adapters/{ssh,sftp,config}/internal/`.
- H17.6 P2 `00009e3` — decouple `AuthChain` via internal `AuthStrategyPort`; drop `async-trait` direct dep.
- H17.6 P3+P4 `72f1ccd` — final `src/mcp/` delete; relocate `config`, `error`, and `subscription` (legacy) under their owning adapters.

### Migration

See [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) for the v4.1 contributor note (foundational `mcp::*` paths gone — code now lives at `crate::adapters::{ssh,sftp,config,subscription}::internal::*` and `crate::adapters::subscription::legacy`). **No client-side migration is required** — v3 / v4.0 hosts work against v4.1 servers without any change.

### Etapa trail (v4.1 commits)

H17.6 P1 `bf646f9`, P2 `00009e3`, P3+P4 `72f1ccd`.

## [4.0.0] — 2026-05-03

### Highlights

- Internal restructuring to a full **Hexagonal (Ports and Adapters)** architecture. **The public MCP API is unchanged** — every v3 client / host / LLM keeps working without any change to wire format, tool catalogue, resource schemes, env vars, or markdown response shape. v4 is a codebase migration, not a protocol break.
- Test count grew from **832 (820 lib + 12 integration)** in v3 to **1023 (1021 lib + 2 integration)** in v4.
- ~14k LOC of v3 monolith deleted in H17.5a after every consumer migrated to the new layers.

### Breaking (internal architecture only — public MCP surface stable)

- Migrate to a full hexagonal layout: `src/{domain, ports, application, adapters, infra, composition}/`. The v3 `src/mcp/{tools, server, resources, message, storage, schema, keys}` modules are gone; only the foundational `src/mcp/{client, session, sftp, shell, async_command, transfer, subscription, auth, config, error, types}` set survives runtime-active and is slated for absorption in v4.1 (etapa H17.6).
- Use cases moved out of `src/mcp/tools/<domain>.rs` into one struct per business operation under `src/application/`. Each use case takes its ports as generic type parameters (static dispatch via `trait-variant` AFIT, **no `Box<dyn Trait>` in hot paths**) and is unit-tested against in-memory fakes — **zero rmcp / russh / SFTP machinery in the test path**.
- Concrete adapters land under `src/adapters/{ssh, sftp, repo/dashmap, auth, clock, config, id_generator, subscription, notifier, output_stream}/`. The composition root `src/composition/{prod, fixtures}.rs` pins concrete types at compile time so wiring errors surface at `cargo build` rather than runtime.
- `src/infra/mcp/{server, tool_router, resource_handlers, peer_handle, args, render, helpers}` owns the inbound rmcp surface (the 18 `#[tool]` entry points + `resources/*` handlers). Args are split out of v3 `schema.rs` into per-tool `Deserialize + JsonSchema` structs; markdown rendering is split out of v3 `message::builder` into per-domain modules.
- `async-trait` direct dep retained transitionally for the orphaned v3 `src/mcp/auth/` chain (runtime-unreachable in v4, deleted in H17.6 alongside the foundational `mcp::*` modules).

### Added

- 22 use cases under `src/application/`: `connect_session`, `disconnect_session`, `list_sessions`, `disconnect_agent`, `execute_command`, `get_command_output`, `list_commands`, `cancel_command`, `open_shell`, `write_shell`, `send_key`, `read_shell`, `wait_for_pattern`, `close_shell`, `upload_file`, `download_file`, `get_transfer_progress`, `forward_port`, `list_resources`, `read_resource`, `subscribe_resource`, `unsubscribe_resource`, plus the `peer_gc` background sweep.
- 14 ports under `src/ports/`: `ssh_client`, `sftp_client`, `output_stream`, `session_repo`, `command_repo`, `shell_repo`, `transfer_repo`, `forward_repo`, `subscriber_registry`, `notifier`, `id_generator`, `clock`, `config`, `auth_strategy`. All sync slices via plain trait, async slices via `trait-variant` AFIT (replaces `async-trait`'s `Box<dyn Future>` boxing).
- Adapters under `src/adapters/`:
  - `ssh/russh_adapter.rs` (production) + shared `SshHandleRegistry` so the SFTP adapter reuses the russh handle from `RusshClient` instead of opening a second connection.
  - `sftp/russh_sftp_adapter.rs` (production) + `sftp/in_memory.rs` (test fixture).
  - `repo/dashmap/{session, command, shell, transfer, forward}.rs` — lock-free in-memory implementations of every repo port.
  - `auth/{password, key, agent, chain}.rs` — `trait-variant` AFIT rewrite of the v3 strategy chain.
  - `clock/{system, fake}.rs`, `config/env.rs`, `id_generator/{uuid, deterministic}.rs`.
  - `subscription/memory_registry.rs` — `MemoryRegistry<N>` generic over the notifier port (no `Box<dyn>`).
  - `notifier/{rmcp_adapter, rmcp_peer}.rs` — `PeerHandle` abstraction so use cases never see `rmcp::Peer<RoleServer>` directly.
  - `output_stream/{russh_output, in_memory}.rs` — abstract over PTY broadcast streams.
- Infra MCP layer (`src/infra/mcp/`):
  - `server.rs` (`McpSshServer<UC>` generic).
  - `tool_router.rs` (the 18 `#[tool]` entry points; one per use case).
  - `resource_handlers.rs` (`resources/list`, `resources/read`, `resources/subscribe`, `resources/unsubscribe`, `notifications/resources/updated`).
  - `peer_handle.rs` (re-exports the `PeerTable` so binaries can plumb peer-to-handle mapping into the registry).
  - `args/` (per-tool argument structs — replaces v3 `schema.rs`).
  - `render/` (per-domain markdown builders — replaces v3 `message::builder`).
  - `helpers/{error, nonce, output}.rs` (rendering primitives — replaces v3 `message::helpers`).
- Composition root `src/composition/`:
  - `prod.rs` — wires the production adapter set (russh + russh-sftp + DashMap + env config + UUID v4 + tokio mpsc); used by both `ssh-mcp` (HTTP) and `ssh-mcp-stdio`.
  - `fixtures.rs` — wires the deterministic in-memory adapter set (FakeClock + DeterministicIdGen + InMemorySftp + …) for use cases under `cargo test --features test-fixtures`.
- `PeerHandle` abstraction so use cases interact with rmcp peers through a sync handle (`subscribe`, `unsubscribe`, `notify`) instead of holding a live `Peer<RoleServer>` inside a hot `DashMap` value.
- `SshHandleRegistry` shared between the `russh_adapter` and the `russh_sftp_adapter` to fix the v3 dual-connection cost when SFTP and PTY ran on the same session.
- New test fixture feature `test-fixtures` (off by default) — exposes `FakeClock`, `DeterministicIdGen`, `InMemorySftp`, `MemoryRegistry`, etc. so downstream crates can compose the same use cases against deterministic adapters.
- Documentation:
  - `docs/MIGRATION_v3_to_v4.md` — codebase-contributor migration guide (file-path table, per-layer responsibility map, before/after dependency graphs).
  - `docs/adr/0002-adopt-hexagonal-architecture.md` — design rationale, alternatives considered (modular monolith / SOLID-light extension), trade-offs, lock-free invariants under the new layout.
  - `docs/LOCKS.md` — full rewrite for v4. Maps every lock-free invariant to the layer that enforces it (domain types, repo adapters, registry adapter, output-stream adapter).
  - `docs/ARCHITECTURE.md` — full rewrite for v4 hexagonal layout (layer-by-layer module map, dependency graph, sequence diagrams). Mermaid validated via `mmdc`.
- Integration smoke `tests/v4_smoke.rs` exercises the full composition root end to end (replaces the v3 `tests/server_integration.rs` which was bound to the old globals).

### Changed

- All 18 MCP tool descriptions, JSON schemas, and response markdown bodies are **byte-compatible** with v3 (verified by snapshot tests in `tests/v4_smoke.rs`). The 8-hex-char nonce and `--- stdout [<nonce>] ---` delimiter format is unchanged.
- The 5 `resources/*` schemes (`shell://`, `command://`, `transfer://`, `session://`, `forward://`) keep identical URIs, cursor semantics, debounce timings, sequence-number gap detection, and lagged auto-recovery behaviour.
- All 25+ env vars (`SSH_*`, `MCP_*`, `RUST_LOG`) keep identical names, defaults, floors, and caps.
- HTTP transport bind / path defaults (`0.0.0.0:8000` on `/`) and stdio transport behaviour are identical.
- The strict Clippy lint baseline (forbid layer A + deny layer B + lock-free invariants) is identical to v3 — every v3 lint kept; the new layers comply without any new `#[allow]` exceptions.

### Fixed

- HTTP root-mount panic under `axum` 0.8: `composition::prod` now uses `Router::fallback_service` when `MCP_HTTP_PATH = "/"` (axum 0.7 silently accepted nested `/` mounts; axum 0.8 panics). The fallback path keeps the path tunable via `MCP_HTTP_PATH` (e.g. `/mcp`) without touching the binary code.

### Removed

- ~14k LOC of v3 monolith hard-deleted in H17.5a (commit `95ddc5b`):
  - `src/mcp/tools/` (per-domain tool implementations — moved to `src/application/` + `src/infra/mcp/`).
  - `src/mcp/storage/` (DashMap traits + globals — moved to `src/ports/*_repo.rs` + `src/adapters/repo/dashmap/`).
  - `src/mcp/server.rs` (`McpSshServer` + `#[tool_router]` — moved to `src/infra/mcp/{server,tool_router,resource_handlers}.rs`).
  - `src/mcp/message/` (helpers + per-tool builders — moved to `src/infra/mcp/{helpers,render}/`).
  - `src/mcp/resources.rs` (URI parser + reader handlers — moved to `src/application/{list,read,subscribe,unsubscribe}_resource.rs` + `src/infra/mcp/resource_handlers.rs`).
  - `src/mcp/schema.rs` (JSON schema helpers — replaced by per-tool args structs under `src/infra/mcp/args/`).
  - `src/mcp/keys.rs` (semantic keystroke encoder — relocated to `src/domain/keys.rs`; the encoder now lives in the domain layer).
  - `src/mcp/forward.rs` (port-forward state — moved to `src/domain/forward.rs` + `src/adapters/repo/dashmap/forward.rs`).
- `tests/server_integration.rs` (replaced by the leaner `tests/v4_smoke.rs` which exercises the full composition root).

### Deferred to v4.1 (etapa H17.6)

Shipped — see the [4.1.0] entry above for the H17.6 P1+P2+P3+P4 outcome (src/mcp/ deleted, async-trait dropped, AuthChain decoupled). Cross-adapter SFTP refinements (shared transfer scheduler, per-session SFTP semaphore tuning) remain on the v4.2 backlog.

### Migration

See [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) for the contributor migration guide. **No client-side migration is required** — v3 hosts work against v4 servers without any change.

### Etapa trail (v4 commits)

H0 `5a70845`, H1 `e96eca0`, H2 `bf4b446`, H3a `9c48fbc`, H3b `a220897`, H3c `32751ab`, H4 `da5c88d`, H5a `69a9bb7`, H5b `60b0b6d`, H5c `63e76fb`, H5d `abe8b2b`, H6 `679f307`, H6.5 `4e87758`, H7 `3a83f4c`, H8 `a9ea90c`, H9 `c5bd3af`, H10 `86d690e`, scaffold `eb197e3`, H11a `f42cf00`, H11b `d22044c`, H11c `96b8808`, H12a `2b05c8b`, H12b `33b4477`, H12c `a0571d0`, H12d `7149a54`, H13 scaffold `587c91d`, H13a `07d9574`, H13b `8b0e366`, H13c `ceaea1f`, H13d `5a1b0ee`, H13e `b612b24`, H13f `cd0b257`, H13 cleanup `1d997dc`, H14 `7591324`, H15 `b6f9178`, H16 `1c2c889`, H17 `c535992`, H17.5a `95ddc5b`, H18 `ddfbfeb`, H19 `b6f117e`.

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

[4.1.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.1.0
[4.0.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.0.0
[3.0.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v3.0.0
[2.0.1]: https://github.com/farchanjo/ssh-mcp/releases/tag/v2.0.1
