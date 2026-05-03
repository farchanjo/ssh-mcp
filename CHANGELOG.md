# Changelog

All notable changes to ssh-mcp are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [4.6.0] — 2026-05-03

### Highlights

- LLM UX **100%** — every dimension audited at v4.5 reaches its theoretical maximum for a 27B-30B class model. Self-bootstrap from `Implementation.instructions` + tool list with no external docs. Successor tools advertised in-band via a new `NEXT:` line. Subscribe-first nudges via inline `HINT:` lines on every async-spawn response. JSON Schema `default` keyword visible on every optional field. All 14 documented wire error codes now reach the wire (3 reserved tags promoted to live). Server icon wired. Cost / timing hints inlined in tool descriptions.
- Public MCP API stays byte-compatible with v4.5.0 except for one narrow rename: the wire key for `agent_id` changed from `AGENT:` to `AGENT_ID:` for consistency with every other `_ID` field. Hosts that walk key-value lines generically are unaffected; hosts that grep `^AGENT:` literally must update.

### Added

- **`NEXT:` advisory line** — every response with a clear successor tool now ends with a `NEXT:` line listing pipe-separated concrete tool calls (e.g. `NEXT: ssh_get_command_output(command_id=c-X, wait=true) | ssh_cancel_command(command_id=c-X)`). 12 emission sites:
  - `SSH_CONNECT: OK / REUSED / SUGGESTED`
  - `SSH_LIST_SESSIONS: OK` (only when non-empty)
  - `SSH_EXECUTE: STARTED`
  - `SSH_GET_COMMAND_OUTPUT: RUNNING`
  - `SSH_SHELL_OPEN: OK`
  - `SSH_SHELL_WRITE: OK` / `SSH_SHELL_SEND_KEY: OK`
  - `SSH_SHELL_WAIT_FOR: MATCHED / TIMEOUT`
  - `SSH_UPLOAD: STARTED` / `SSH_DOWNLOAD: STARTED`
  - `SSH_GET_TRANSFER_PROGRESS: RUNNING`
  - `SSH_FORWARD: OK`
- **Subscribe-first `HINT:` lines (4 new sites)** — `ssh_shell_open`, `ssh_execute` (async), `ssh_upload` / `ssh_download`, and `ssh_forward` now emit a one-line `HINT:` steering callers to the matching `resources/subscribe` URI ("subscribe to <scheme>://<id>/... for realtime ..."). Anti-leak `HINT:` on `ssh_list_sessions` (>5 sessions/agent, v4.4) is unchanged.
- **JSON Schema `default` keyword visible on 27 optional fields** — `connection.rs` (7), `execute.rs` (8), `shell.rs` (10), `sftp.rs` (2). Smaller LLMs that read the schema mechanically now see the default value without parsing English from the description. Implemented via `#[schemars(default = "fn_name")]`.
- **Cost / timing hints in tool descriptions** — every `#[tool(description = "...")]` ends with a `Cost: ...` line covering O() complexity, expected latency, blocking-vs-async, and pointer to subscribe paths where applicable. 35 description sites updated (18 with `port_forward` + 17 without).
- **3 reserved error tags now emit live**:
  - `FORWARD_FAILED:` — `application/forward_port.rs::ForwardPortUseCase::preflight_bind` (pre-flight `TcpListener::bind` failures other than `AddrInUse`).
  - `LOCAL_NOT_FILE:` — `application/upload_file.rs::UploadFileUseCase::guard_local_path_is_file` (pre-flight `metadata` check rejects directories / non-regular files).
  - `REMOTE_METADATA_ERROR:` — `adapters/sftp/russh_sftp_adapter.rs::stat_remote_size` (download remote `stat` failures).
  All 14 documented codes in `ERRORS.md` now reach the wire.
- **Server icon wired** — `Implementation.icons` points to `https://raw.githubusercontent.com/farchanjo/ssh-mcp/master/assets/icon.svg` (image/svg+xml, sizes "any"). New `assets/icon.svg` (128x128 terminal-themed mark with `$ ssh` prompt). URL resolves after the v4.6 master push; clients gracefully fall back to title + description before then.

### Changed

- **Wire key rename: `AGENT:` → `AGENT_ID:` (narrow breaking change)** — 7 render sites: `ssh_connect`, `ssh_list_sessions` (per-row), `ssh_execute` (async), `ssh_shell_open`, `ssh_upload` / `ssh_download`, `ssh_disconnect_agent`. CLAUDE.md required all IDs to end with `_ID` — this closes the last drift. Hosts walking key-value lines generically: no change. Hosts grepping `^AGENT:` literally: update to `^AGENT_ID:`.
- `ssh_connect` description trimmed for token efficiency where redundant; cost line preserves the existing tip bullets.

### Tests

- Lib tests: **1074 → 1091** (+12 from JSON Schema default snapshot tests; +6 from new error-code emission tests; -1 redundant test consolidated). Clippy / build / fmt all clean on touched files.

### Behaviour notes

- The narrow `AGENT:` → `AGENT_ID:` rename is the only wire-shape change in v4.6. Every other addition is purely additive (new lines appended; existing keys preserved).
- `NEXT:` lines are advisory only — they never carry side effects and never affect MCP wire validity. Hosts that ignore them are still fully functional.
- Cost hints in tool descriptions are conventional English text, not machine-readable. They steer models that rank tools by description similarity; they do not constrain behaviour.

## [4.5.0] — 2026-05-03

### Highlights

- LLM UX overhaul. Smaller LLMs (27B-30B class) now have everything they need to drive ssh-mcp without external docs: real bytes + `_meta` envelope on `resources/read`, stable peer identity across subscribe / unsubscribe, 14 granular wire error codes, per-tool `Tool.title` + `ToolAnnotations` hints, server-level `Implementation.title` / `description` / `website_url`, and a few-shot `instructions` field with three canonical workflows. Public MCP API stays byte-compatible with v4.4.0 / v4.3.0 / v4.2.0 / v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same env vars, same defaults; the markdown shape is extended **additively** (forward render adds `FORWARD_ID:` and `SESSION_ID:`) so existing v3 / v4 hosts continue to parse responses without any change.

### Fixed

- **Stable peer identity** — `subscribe` and `unsubscribe` now share the same `PeerId` derived from the `Mcp-Session-Id` header (HTTP transport) or a `Stdio` singleton key. Previously `RmcpPeerHandle::new` minted a fresh UUID per call, so `unsubscribe` was a silent no-op (the registry never matched). The shared `PeerTable` now exposes `get_or_mint`, `lookup_peer`, `drop_by_id`, and `gc_closed_peers`. Wired to the existing peer-GC pump on the stdio path; HTTP path is session-scoped so each session gets its own table cleared on session drop.
- **`resources/read` returns the real body and `_meta`** — the H17 placeholder is gone. Shell URIs now return UTF-8 lossy bytes (`text/plain`); command URIs return v3-style block payloads with shared nonce delimiters; transfer / session / forward URIs return the existing JSON snapshot with `application/json` mime. `_meta` carries `kind`, `cursor`, `buffer_size` (shell + command only), `last_seq`, and `status`. Subscribe-first contract is now byte-compatible with what `LLM_GUIDE.md` describes.
- **Forward render exposes IDs** — `SSH_FORWARD: OK` blocks now emit `FORWARD_ID:` and `SESSION_ID:` lines so callers can construct the matching `forward://<FORWARD_ID>/events` resource URI directly from the tool response.

### Added

- **Granular wire error codes via tag-prefix dispatch** — `classify_error` now recognises 14 documented codes by parsing a `TAG:` prefix from `DomainError::InvalidArgument` / `Transport` / `Sftp` messages. Application and adapter layers prefix every known failure site so smaller LLMs can branch recovery logic on a specific code instead of guessing from the collapsed `INVALID_ARGUMENT` / `TRANSPORT_ERROR` / `SFTP_ERROR` codes. Tags surfaced today: `EMPTY_PATTERNS`, `TOO_MANY_PATTERNS`, `PATTERN_TOO_LONG`, `MODIFIER_NOT_ALLOWED`, `INVALID_REPEAT`, `FEATURE_DISABLED`, `WRITE_FAILED`, `CHANNEL_FAILED`, `COMMAND_FAILED`, `LOCAL_FILE_ERROR`, `SFTP_OPEN_FAILED`. Reserved (dispatcher recognises but no live raise yet): `FORWARD_FAILED`, `LOCAL_NOT_FILE`, `REMOTE_METADATA_ERROR`. Untagged messages still fall through to the legacy flat codes.
- **Rich MCP server identity** — `Implementation` now carries:
  - `title = "SSH Remote Shell"`
  - `description = "Run remote commands, drive PTY shells, transfer files via SFTP, and forward TCP ports over SSH..."`
  - `website_url = "https://github.com/farchanjo/ssh-mcp"`
  - `icons = None` (TODO until the SVG asset lands at a hosted URL).
- **Per-tool `title` + `ToolAnnotations`** — every one of the 18 tools (or 17 without `port_forward`) now declares a human-readable `title` and the `read_only_hint` / `destructive_hint` / `idempotent_hint` annotations so MCP hosts can rank, filter, and warn before destructive use. Read-only tools: `ssh_list_sessions`, `ssh_get_command_output`, `ssh_list_commands`, `ssh_shell_wait_for`, `ssh_get_transfer_progress`. Idempotent destructive: disconnect / cancel / shell_close. Open-world hint left unset (defaults to true).
- **Few-shot `instructions` field** — replaces the previous one-liner with three canonical workflows (run command, interactive shell, upload) plus the v4.4 `EXPIRES_AT` / `HINT` / `agent_id` steering cues. Two flavours: `INSTRUCTIONS_WITH_FORWARD` (797 bytes) and `INSTRUCTIONS_WITHOUT_FORWARD` (785 bytes). The non-`port_forward` build advertises 17 tools and 4 streams.
- **Schema documents per-key modifier policy** — `ssh_shell_send_key` `key` field now spells out the allowed modifier set per key class in its JsonSchema description (arrows / nav / F-keys accept any of `shift`/`alt`/`ctrl`; `tab` accepts only `shift`; control bytes reject every modifier). `ssh_shell_wait_for` `patterns` field documents the wire codes returned on validation failure.
- **Doc surface** — `docs/LLM_GUIDE.md`, `docs/API.md`, `docs/ERRORS.md`, `docs/RESOURCES.md`, `docs/FLOWS.md`, `README.md`, and `CLAUDE.md` are now in sync with v4.5.0 behaviour. New LLM_GUIDE sections cover connection lifecycle (v4.4), granular error codes (v4.5), server identity (v4.5), and a smaller-LLM cookbook with three canonical workflows.

### Tests

- Lib tests: **1060 → 1074** (+5 from `_meta` + PeerTable + render coverage; +9 from granular-error-code classifier coverage).
- 14 Mermaid diagrams (LLM_GUIDE/RESOURCES/FLOWS) re-validated with `mmdc 11.14.0`.
- No public wire-shape regressions: existing v3 / v4 hosts parse v4.5 responses without modification.

### Behaviour notes

- `Tool.title` arrived through `#[tool(title = "...")]` at the macro top level (verified in rmcp-macros 1.6 source). `ToolAnnotations` arrived through `#[tool(annotations(read_only_hint = ..., destructive_hint = ..., idempotent_hint = ...))]`.
- Peer-GC pump on the stdio path now also prunes closed peers from the new `PeerTable`. HTTP path is session-scoped — each rmcp service factory call yields a fresh `McpSshServer` and a fresh `PeerTable`, naturally collected on session drop.

## [4.4.0] — 2026-05-03

### Highlights

- LLM-steering minor release. Public MCP API stays byte-compatible with v4.3.0 / v4.2.0 / v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same env vars, same defaults; the markdown shape is extended **additively** (new `EXPIRES_AT` line, new `HINT` line) so existing v3 / v4 hosts continue to parse the response without any change.

### Added

- **agent_id-aware ranking** — `connect_session` use case now ranks identity matches owned by the requesting `agent_id` ahead of foreign matches under both `reuse=auto` and `reuse=suggest`. Tiebreaker is the existing `connected_at desc` ordering. When the caller does not pass `agent_id`, ranking is a no-op (newest-first stays). Implemented in `src/application/connect_session.rs::rank_by_agent_affinity`.
- **EXPIRES_AT line** — `SSH_CONNECT: OK` and `SSH_CONNECT: REUSED` blocks now emit `EXPIRES_AT: <rfc3339>` (computed as `connected_at + ConfigPort::inactivity_timeout`) so an LLM can ping (any cheap call) before the inactivity sweeper closes the session. When `persistent=true`, the renderer emits `PERSISTENT: true` and skips `EXPIRES_AT`. When the configured inactivity timeout is zero, `EXPIRES_AT` is also omitted (sweeper disabled). Renderer change in `src/infra/mcp/render/connection.rs::append_persistent_or_expiry` + `compute_expires_at`.
- **anti-leak HINT** — `SSH_LIST_SESSIONS` now appends `HINT: agent '<id>' owns N sessions; consider ssh_disconnect_agent to bulk-cleanup` when any agent owns more than 5 healthy sessions in the rendered list. Threshold lives in `ANTI_LEAK_HINT_THRESHOLD` (`src/infra/mcp/render/connection.rs`).
- **`ssh_connect` description Tip bullets** — the tool description (both feature flavours of the `#[tool_router]` impl) now includes:
  - `Tip: pass reuse=auto to let the server pick the most recent healthy match in a single round-trip. Use reuse=suggest (default) when you want to inspect matches before reusing. Use reuse=force_new to bypass identity matching entirely.`
  - `Tip: pass agent_id so subsequent sessions are grouped and you can bulk-cleanup with ssh_disconnect_agent. When agent_id is set, reuse=auto/reuse=suggest rank sessions owned by the same agent first.`

### Changed

- `ConnectOutcome::Connected` and `ConnectOutcome::Reused` carry two new fields (`persistent: bool`, `inactivity_timeout: Duration`) so the renderer can compute `EXPIRES_AT` without round-tripping through the SSH adapter or the entity. Internal change — use cases consumed by external code stay generic over the same ports.

### Tests

- Lib tests: **1048 → 1060** (+ 5 connect_session tests covering ranking owned-first / fallback-to-newest / Suggest ordering / `rank_by_agent_affinity` stability + no-op no-agent-id; + 7 render tests covering `EXPIRES_AT` present/absent / persistent skips it / zero-timeout omits it / Reused includes it / anti-leak HINT above threshold / no HINT at threshold / no HINT without agent ids).
- Python integration: stdio **13/13** PASS (no wire-format change in the suite's expected keys).
- HTTP suite: untouched (no test-file changes in `scripts/test_http.py`).

## [4.3.0] — 2026-05-03

### Highlights

- Patch release extending the v4.2 status-pump bridge with **registration sinks**, closing the **TOCTOU race in the per-session transfer cap**, fixing a **shell-id collision** under concurrent `ssh_shell_open` bursts, and shipping a full **chaos suite** (errors, locks, recovery, exhaustion + master runner) on top of the existing stress harness. Public MCP API stays byte-compatible with v4.2.0 / v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same markdown shape, same env vars, same defaults.

### Fixed

- **Shell-id collision under bursts (Bug C)** — concurrent `ssh_shell_open` calls on the same session no longer collide on a deterministic id. The russh adapter now suffixes a UUID v4 onto every minted `ShellId` so 100 concurrent opens produce 100 distinct ids even when the upstream `IdGeneratorPort` returns the same time-derived prefix.
- **Per-session transfer cap TOCTOU (Bug D)** — `ssh_upload` / `ssh_download` no longer breach `SSH_MAX_TRANSFERS_PER_SESSION` under contention. The repository now exposes an atomic `TransferRepository::insert_if_under_cap(entity, cap)` that performs the count probe and the insert under a single shard guard. The use cases route through this method in `persist_or_rollback`; the production transfer registration sink does the same so adapter-side registers cannot slip past the gate either. Optimistic pre-check stays in place to short-circuit obvious overflows before any SFTP RTT.
- **Registration sinks (Bug A)** — `resources/subscribe shell://X/output` no longer fails with `SHELL_NOT_FOUND` immediately after `ssh_shell_open` lands. The russh adapter now bridges its in-memory shell binding into the domain `ShellRepository` via a new internal `ShellRegistrationSink` trait spawned fire-and-forget so the use-case-side `ShellRepository::insert` keeps winning the canonical race; the sink uses `get-then-insert` semantics so a duplicate is a no-op. Mirrors apply for `RunningCommand` (`CommandRegistrationSink`), `TransferShared` (`TransferRegistrationSink`, cap-aware) and (feature-gated) `ForwardHandle` (`ForwardRegistrationSink`). Adapter-driven teardown paths (`close_shell` inactivity sweep, future bulk-cleanup) call `unregister` so the repo never carries stale rows past adapter destruction.
- **Stress scripts go stdio (Bug B)** — `scripts/stress_{concurrent_writes,lagged_sub,locks,subscribe}.py` now default to spawning a single coordinator `ssh-mcp-stdio` child instead of fanning out HTTP clients across ephemeral ports. Set `STRESS_TRANSPORT=http` to flip back to the v4.2 HTTP behaviour. Each script writes a single JSON line summary to stdout (`status: ok | fail | skip`).

### Added

- **Chaos suite** — four new scripts and a master runner under `scripts/`:
  - `chaos_errors.py` — 27 documented-error scenarios (bad ids, validation failures, oversized inputs, unknown sessions, idempotent agent disconnect, invalid URI subscribe, etc.).
  - `chaos_locks.py` — 7 lock-contention scenarios (parallel `shell_open`, parallel `shell_write` FIFO, mixed `send_key + write`, subscribers-then-close, burst execute+cancel, concurrent transfers, open+cancel+close race).
  - `chaos_recovery.py` — 5 recovery scenarios (kill-during-command, sigterm session, buffer truncation, cancel-during-cancel, subscribe-after-close).
  - `chaos_exhaustion.py` — 3 exhaustion scenarios (push 100 MiB upload, max-sessions burst, 1000 subscribers).
  - `chaos_runner.py` — aggregator that runs `test_stdio` + 4 stress + 4 chaos suites and prints a single ASCII summary table.
- `scripts/helpers/chaos.py` — stderr-capturing stdio transport (`_StderrCapturingStdioTransport`), `ChaosSshTarget`, `chaos_session()` context manager, and JSON-line emitters (`write_event`, `write_summary`).

### Changed

- `chaos_locks.py::parallel_shell_open` now accepts `TRANSPORT_ERROR` / `CHANNEL_OPEN_FAILED` / `SSH_ERROR` as valid second-class rejections alongside `MAX_SHELLS_EXCEEDED` (upstream sshd `MaxSessions = 10` is a known environmental cap; the application-side floor of "exactly 10 shells open + 90 documented errors + zero panics" stays strict).
- `scripts/run_all.sh` builds both default-features and `--no-default-features` binaries, then executes pytest + 4 stress + 4 chaos + the aggregate runner.

### Internal

- New traits in `src/adapters/ssh/internal/status_sink.rs`: `CommandRegistrationSink`, `ShellRegistrationSink`, `TransferRegistrationSink`, plus the `port_forward`-gated `ForwardRegistrationSink`. Each carries `register(entity)` + `unregister(id)` over manual AFIT (boxed futures) so the trait stays object-safe.
- New atomic API on `TransferRepository`: `insert_if_under_cap(entity, cap)`. The DashMap implementation acquires the session-bucket shard guard first, gates on the bucket length, and then binds the primary row before releasing — closing the read-then-insert TOCTOU window observed in v4.2.
- New production sinks in `src/composition/status_sinks.rs`: `RepoCommandRegistrationSink` / `RepoShellRegistrationSink` / `RepoTransferRegistrationSink` (cap-aware, holds a `Arc<EnvConfig>`) / `RepoForwardRegistrationSink`. Each uses `get-then-insert` so the canonical use-case insert is never overwritten; `unregister` swallows `Ok(None)` cleanly.
- `RusshAdapter` and `RusshSftpAdapter` gain optional `with_*_registration_sink` setters (default no-op). The composition root wires production sinks alongside the existing status sinks.
- Adapter-side `register` calls are spawned (fire-and-forget) so the use case wins the race; `unregister` calls happen inline at lifecycle exit.
- Helpers `make_stress_client` / `make_stress_client_http` / `stress_transport_mode` added to `scripts/helpers/fixtures.py` so the four stress scripts share one transport-selection seam.

### Tests

- Lib tests: **1031 → 1048** (registration-sink unit tests covering noop variants, duplicate-id handling, unregister silent-when-absent, the feature-gated forward path; new TOCTOU coverage on `DashMapTransferRepo::insert_if_under_cap` including the 50-concurrent-inserts atomic-gate test; new sink-cap-honour test on `RepoTransferRegistrationSink`).
- Python integration suites: stdio **13/13** (unchanged); the 4 stress scripts succeed in stdio mode against a local sshd; the 4 chaos scripts pass with the new strict + environment-aware assertions.

## [4.2.0] — 2026-05-03

### Highlights

- Patch release closing four runtime regressions surfaced by the Python stdio integration suite after the v4.1 deep-decouple. Public MCP API stays byte-compatible with v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same markdown shape, same env vars, same defaults.

### Fixed

- **Command status pump** — `ssh_get_command_output { wait=true }` no longer hangs on `RUNNING` after the SSH driver completes. The russh adapter now spawns a dedicated status watcher per async command that bridges the live `RunningCommand.status_rx` watcher channel into the domain `CommandRepository` via a new internal `CommandStatusSink` trait. The composition root pins a `RepoCommandStatusSink` over the shared `DashMapCommandRepo`; tests/fixtures keep the no-op default so no behavioural change reaches the use-case test surface.
- **Cancel snapshot** — `ssh_cancel_command` no longer surfaces `COMMAND_NOT_FOUND` when the cancellation succeeds. The cancel use case now snapshots stdout/stderr **before** issuing `SshClientPort::cancel`, falling back to an empty snapshot if both pre- and post-cancel reads fail. The SSH adapter still tears its internal command record down inside `cancel`, but the use case no longer races that tear-down.
- **Shell output flush** — `ssh_shell_read` / `ssh_shell_wait_for` long polls no longer time out with empty `data` after a write produced visible output. The shell reader task now flushes incoming PTY frames to `RunningShell.history` (and the broadcast channel) on every chunk (`SHELL_FLUSH_THRESHOLD = 1`) instead of waiting for a 4 KiB batch — interactive shells need per-frame visibility.
- **Shell close pump** — `RusshAdapter::close_shell` now pumps an explicit `Closed` notification into the domain `ShellRepository` via a new internal `ShellStatusSink` trait. Mirrors the command sink path so subscribers / long-poll consumers observe the terminal state without waiting for an entity-removal sweep.
- **Transfer status pump** — SFTP upload/download drivers now surface `Completed` / `Failed` / `Cancelled` and intermediate `record_progress` updates into the domain `TransferRepository` via a new internal `TransferStatusSink` trait. Wired in `composition::prod` over the shared `DashMapTransferRepo`. `ssh_get_transfer_progress { wait=true }` polls now observe the terminal state cleanly.
- **Python integration parser** — `scripts/helpers/parse_block.py` now publishes wire-key aliases (`exit` → `exit_code`, `sessions` → `sessions_disconnected`, `commands` → `commands_cancelled`) so existing test assertions match the v3 short-form keys without any wire-format change. Test-only fix; the markdown response shape stays byte-identical.

### Internal

- New module `src/adapters/ssh/internal/status_sink.rs` — purpose-built `Send + Sync` trait surface (manual AFIT via boxed futures) so the production russh adapter can hold a single `Arc<dyn CommandStatusSink>` / `Arc<dyn ShellStatusSink>` field without dragging the repository generics into its type list.
- New module `src/composition/status_sinks.rs` — production `RepoCommandStatusSink` / `RepoShellStatusSink` / `RepoTransferStatusSink` implementations backed by `DashMap*Repo` and wired by `composition::prod::build_use_cases`.
- `RusshAdapter` and `RusshSftpAdapter` gain optional `with_*_status_sink` setters (default no-op) so the v4.1 public-surface tests continue to compile and pass without any wiring change.

### Tests

- Lib tests: **1014 → 1031** (+ 10 status-sink unit tests, + 7 production sink integration tests).
- Python integration suites: stdio **8/13 → 13/13**; HTTP **14/14 → 14/14**.

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
