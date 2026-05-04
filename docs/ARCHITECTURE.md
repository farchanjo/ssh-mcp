# SSH MCP Architecture (v5.0 — subscribe-first hexagonal)

This document describes the architecture of the SSH Model Context Protocol (MCP) server. It reflects the **v5.0** codebase on `feat/v5-foundation`. The hexagonal (Ports and Adapters) layout from v4.1 is preserved — same six top-level layers, same dependency rules, same lock-free invariants — and three v5 layers are stacked on top: **lifecycle binding** ([ADR 0003](./adr/0003-lifecycle-binding.md), Phase 1 — merged), **channel mux + sub_id** ([ADR 0004](./adr/0004-channel-mux-fairness.md), Phase 2 — merged), **LLM UX overhaul** ([ADR 0005](./adr/0005-llm-ux-priorities.md), Phase 3 — in flight), and the **NDJSON daemon** ([ADR 0008](./adr/0008-ndjson-daemon-protocol.md), Phase 4 — in flight). The text channel stays byte-stable since v3 on the 21 carry-over tools; v5 adds 9 net-new MCP tools and a third binary `ssh-mcp-tail`.

### Version trail

- **v4.0 / v4.1** — original hexagonal restructuring; v3 monolith deleted; `src/mcp/` hard-deleted; `async-trait` direct dep dropped. See [adr/0002-adopt-hexagonal-architecture.md](./adr/0002-adopt-hexagonal-architecture.md).
- **v4.5 / v4.6 / v4.7 / v4.8** — LLM UX foundation (`PeerId`, `_meta` envelope, 14+1 wire error codes, `Implementation` identity, `ToolAnnotations`, `NEXT:` / `HINT:` lines, JSON Schema `default`, `Cost:` hints, `AGENT_ID:` rename), MCP inter-tool conversation surface (`structured_content`, `resources/templates/list`, `notifications/progress`, `prompts/list` + `prompts/get` (5 workflows), idempotency cache, NOT_FOUND closest-match), and full `output_schema` coverage on every tool. Tool catalogue grew 18 -> 21.
- **v5.0** — subscribe-first refresh. CAS-driven `ResourceLifecycle` + cascade refcount + grace timer ([ADR 0003](./adr/0003-lifecycle-binding.md)). `(SubId, Uri)` channel keying with per-lane `LagPolicy` / filter / replay / stats and round-robin `ChannelMux` ([ADR 0004](./adr/0004-channel-mux-fairness.md)). 9 net-new MCP tools (`ssh_subscribe` etc.), `HINT:` severity escalation, 10-prompt catalog, `SUB_LEAK_RISK` watcher, 38-code error taxonomy ([ADR 0005](./adr/0005-llm-ux-priorities.md), [ADR 0007](./adr/0007-error-taxonomy.md)). `ssh-mcp-tail` daemon binary ([ADR 0008](./adr/0008-ndjson-daemon-protocol.md)). MSRV bumped to Rust 1.95.

[[_TOC_]]

## High-level diagram

```text
                                         ┌─────────────────────────────┐
   LLM hosts                              │  ssh-mcp v5.0  (one crate)  │
   ───────────────────────                │                             │
   Claude Desktop  ──┐                    │   ┌─────────────────────┐   │
   mcp-inspector  ───┤   rmcp 1.6 HTTP    │   │   src/infra/mcp/    │   │
   Cline          ──┐│   (axum 0.8)       │   │   30 #[tool] fns    │   │
   custom rmcp ────┐│└──── ssh-mcp ──────►│   │   resources/*       │   │
                   │└────────────────────►│   │   prompts (10)      │   │
   IDE / stdio  ───┴──── ssh-mcp-stdio  ──┤   │   templates (4/5)   │   │
   Claude Code  ───┐                       │   │   idempotency       │   │
   pipelines    ───┴──── ssh-mcp-tail  ──►│   │   error_detail      │   │
                  NDJSON  +  duplex       │   └──────────┬──────────┘   │
                                          │              │              │
                                          │   ┌──────────▼───────────┐  │
                                          │   │  src/application/    │  │
                                          │   │  ~22 *UseCase<...>    │  │
                                          │   │  static dispatch      │  │
                                          │   └──────────┬───────────┘  │
                                          │              │              │
                                          │   ┌──────────▼───────────┐  │
                                          │   │   src/ports/         │  │
                                          │   │   trait skeletons    │  │
                                          │   │   v5 +3:             │  │
                                          │   │   lifecycle_policy   │  │
                                          │   │   channel_mux        │  │
                                          │   │   subscriber_lane    │  │
                                          │   └──────────▲───────────┘  │
                                          │              │              │
                                          │   ┌──────────┴───────────┐  │
                                          │   │   src/adapters/      │  │
                                          │   │   v4 set + v5:       │  │
                                          │   │   lifecycle/         │  │
                                          │   │   subscription/lane  │  │
                                          │   │   subscription/mux   │  │
                                          │   └──────────┬───────────┘  │
                                          │              │              │
                                          │   ┌──────────▼───────────┐  │
                                          │   │  src/composition/    │  │
                                          │   │  prod | embed | fixt │  │
                                          │   └──────────┬───────────┘  │
                                          │              │              │
                                          │   ┌──────────▼───────────┐  │
                                          │   │  src/main.rs    HTTP │  │
                                          │   │  src/bin/stdio  STDIO│  │
                                          │   │  src/bin/tail   NDJ  │  │
                                          │   └──────────────────────┘  │
                                          │                             │
                                          └─────────────────────────────┘
                                                       │
                              russh 0.55 (SSH + SFTP)  │
                                                       ▼
                                            sshd / SFTP / agent
```

The four layers from v4.1 (`domain`, `ports`, `application`, `adapters`) plus `infra` (inbound MCP) and `composition` (root wiring) are unchanged. v5 stacks new ports + adapters under each layer, never re-roots the layout.

## Hexagonal layer map

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart TB
    subgraph BIN["bin — entry points"]
        HTTP["src/main.rs"]
        STDIO["src/bin/ssh_mcp_stdio.rs"]
        TAIL["src/bin/ssh_mcp_tail.rs<br/>(Phase 4)"]
    end
    subgraph COMP["composition — wiring root"]
        PROD["prod.rs"]
        EMB["embed.rs<br/>(Phase 4)"]
        FIX["fixtures.rs"]
    end
    subgraph EMBEDLAYER["embed — daemon transport"]
        DUPLEX["duplex_transport.rs"]
        DISP["dispatcher.rs"]
        EVMUX["event_mux.rs"]
        FMT["formatter.rs"]
    end
    subgraph INFRA["infra/mcp — inbound MCP"]
        SERVER["server.rs"]
        TR["tool_router.rs<br/>30 #[tool] fns"]
        RES["resource_handlers.rs"]
        PROMPTS["prompts.rs"]
        RESULTS["results.rs"]
        ERR["error_detail.rs"]
    end
    subgraph APP["application — use cases"]
        UCONN["connect_session<br/>disconnect_*"]
        UEXE["execute_command<br/>get_command_output"]
        USHELL["open_shell<br/>read_shell"]
        USFTP["upload_file<br/>download_file"]
        URES["subscribe_resource<br/>unsubscribe_resource"]
    end
    subgraph PORTS["ports — trait skeletons"]
        SSHP["ssh_client / sftp_client"]
        REGP["subscriber_registry"]
        LIFEP["lifecycle_policy<br/>(Phase 1)"]
        MUXP["channel_mux<br/>(Phase 2)"]
        LANEP["subscriber_lane<br/>(Phase 2)"]
    end
    subgraph ADAPT["adapters — concrete impls"]
        RUSSH["ssh/russh_adapter"]
        SFTP["sftp/russh_sftp_adapter"]
        REPO["repo/dashmap"]
        MEMREG["subscription/memory_registry"]
        LIFE["lifecycle/<br/>refcount + grace_timer +<br/>cascade<br/>(Phase 1)"]
        LANE["subscription/<br/>subscriber_lane +<br/>channel_mux + filter +<br/>replay<br/>(Phase 2)"]
        NOTIF["notifier/rmcp_*"]
    end
    subgraph DOMAIN["domain — pure"]
        ENT["entities + ids +<br/>events + lifecycle.rs +<br/>subscription.rs"]
    end

    BIN --> COMP
    COMP --> EMBEDLAYER
    COMP --> INFRA
    INFRA --> APP
    APP --> PORTS
    PORTS -.-> ADAPT
    ADAPT --> DOMAIN
    APP --> DOMAIN
    PORTS --> DOMAIN

    style BIN fill:#161b22,color:#f0f6fc,stroke:#30363d
    style COMP fill:#161b22,color:#f0f6fc,stroke:#30363d
    style EMBEDLAYER fill:#161b22,color:#f0f6fc,stroke:#30363d
    style INFRA fill:#161b22,color:#f0f6fc,stroke:#30363d
    style APP fill:#161b22,color:#f0f6fc,stroke:#30363d
    style PORTS fill:#161b22,color:#f0f6fc,stroke:#30363d
    style ADAPT fill:#161b22,color:#f0f6fc,stroke:#30363d
    style DOMAIN fill:#161b22,color:#f0f6fc,stroke:#30363d
    style HTTP fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style STDIO fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style TAIL fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style LIFE fill:#238636,color:#f0f6fc,stroke:#2ea043
    style LANE fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style LIFEP fill:#238636,color:#f0f6fc,stroke:#2ea043
    style MUXP fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style LANEP fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
```

## Layer rules

| Layer | What lives here | Allowed deps | Forbidden deps |
|-------|-----------------|--------------|----------------|
| `domain` | Entities, value objects, errors, live event variants. v5: `lifecycle.rs`, `subscription.rs`. | `std`, `serde`, `serde_json`, `chrono`, `thiserror`, `schemars`, `bytes` | `tokio`, `russh`, `rmcp`, `axum`, `dashmap` |
| `ports` | Trait skeletons (sync + async via `#[trait_variant::make(Port: Send)]`). v5: `lifecycle_policy.rs`, `channel_mux.rs`, `subscriber_lane.rs`. | `domain`, `bytes`, `chrono`, `std`, `trait_variant` | `tokio`, `russh`, `rmcp`, `axum`, `dashmap` |
| `application` | Use cases (`*UseCase<Ports...>`) — one struct + DTO per business operation. | `domain`, `ports`, `tokio` (for `select!` / `spawn`) | `russh`, `rmcp`, `axum`, `dashmap` |
| `adapters` | Concrete implementations of every port. v5: `lifecycle/`, `subscription/{subscriber_lane,channel_mux,filter,replay}`. | All runtime crates (`tokio`, `russh`, `russh_sftp`, `dashmap`, `arc_swap`) | `rmcp` (only `notifier/rmcp_*`), `axum` |
| `infra` | Inbound MCP transport (`#[tool_router]`, `ServerHandler`, render, args, helpers). v5: `error_detail.rs`. | `rmcp`, `application`, `domain`, `adapters::notifier::rmcp_peer` | `russh`, `russh_sftp`, `dashmap` |
| `composition` | Root wiring — pins concrete adapters via `type ConcreteX = …`, builds the `UseCases` container. v5: `embed.rs` (Phase 4 — in flight). | All adapters + `infra::mcp` + `application` | — (it is the leaf) |

### Static dispatch + AFIT (no async-trait, no dyn on hot path)

Async ports use the `trait-variant` macro pattern. The dyn-safe slices (`LaneAdmin`, async slice on `LifecyclePolicyAsync`, …) live alongside the AFIT trait surface for cold-path admin operations only. Use cases stay generic over the static-dispatch port and never over `dyn Trait`. The composition root pins one concrete adapter per port. Wiring errors surface at `cargo build`, not at runtime.

## Module map

The crate root is `src/`. Library code lives under `src/{domain,ports,application,adapters,infra,composition}/`. The three binaries are thin shells that delegate to `composition::prod` (and, for the daemon, `composition::embed`).

### Binary entry points

| File | Role |
|------|------|
| `src/main.rs` | HTTP transport (`ssh-mcp`). Calls `composition::prod::run_http()`. Default bind `0.0.0.0:8000`, path `/`. |
| `src/bin/ssh_mcp_stdio.rs` | Stdio transport (`ssh-mcp-stdio`). Calls `composition::prod::run_stdio()`. Logs to stderr via `RUST_LOG`. |
| `src/bin/ssh_mcp_tail.rs` (Phase 4 — in flight) | NDJSON daemon (`ssh-mcp-tail`). Embeds rmcp client + server pair across `tokio::io::duplex` via `composition::embed::wire()`. |

All three spawn the peer-GC task on `SSH_MCP_PEER_GC_INTERVAL_S` (default 30 s). rmcp 1.6 does not expose a peer-disconnect callback, so the periodic scan is the only way to reclaim subscription state for closed transports.

### `src/domain/` — pure layer

| File | Role |
|------|------|
| `mod.rs` | Re-export tree. **MUST NOT** import `tokio`, `russh`, `rmcp`, `axum`, or `dashmap`. |
| `auth.rs` | `AuthError` variant for credential rejection. |
| `command.rs` | `CommandEntity`, `CommandRequest`, `CommandStatus`. |
| `error.rs` | `DomainError` (top-level) — variants per failure class plus v5 additions (`ResourceGone`, `LifecycleStateConflict`, `SessionRefcountUnderflow`, `SubNotFound`, `SubMaxPerUriExceeded`, `SubMaxTotalExceeded`, `LaneBufferFull`, `LagBackpressure`, …). |
| `events.rs` | Live event enums shipped by the runtime ports — `OutputChunk`, `ProgressEvent`, `HealthEvent`, `ForwardEvent`. Every variant carries `seq: u64` allocated by the subscription registry. |
| `forward.rs` | `ForwardEntity`. |
| `identity.rs` | `Address`, `Credentials`. |
| `ids.rs` | Newtype ids — `SessionId`, `CommandId`, `ShellId`, `TransferId`, `ForwardId`, `AgentId`, `PeerId`. |
| `keys.rs` | `ShellKey`, `KeyModifiers`, semantic keystroke encoder. |
| `lifecycle.rs` (v5) | `LifecycleState` enum (`Owned | Observed | Releasing | Closed`), `LifecyclePolicy`, `LifecycleSnapshot`, `SessionPolicy`, `DEFAULT_GRACE_MS`. Pure data; the atomic state machine lives in the adapter. |
| `policy.rs` | `CommandStatusFilter`, `MaxItemsPolicy`, etc. |
| `ringbuffer.rs` | `RingBuffer` value type. |
| `session.rs` | `SessionEntity`. |
| `shell.rs` | `ShellEntity`, `ShellTerminal`. |
| `subscription.rs` (v5) | `SubId` (UUIDv7 wrapper), `LagPolicy` enum (`BlockSlow | DropOldest | DropNewest | Snapshot`), `LogLevel`, `FilterRule`, `SubscriptionLifetime`, `SubscriberStats` (8 atomics). |
| `transfer.rs` | `TransferEntity`, `TransferStatus`, `TransferDirection`. |

### `src/ports/` — trait skeletons

| File | Role |
|------|------|
| `ssh_client.rs` | `SshClientPort` — connect, disconnect, execute (sync + async), open/write/close shell, health check. |
| `sftp_client.rs` | `SftpClientPort` — upload, download, cancel. |
| `session_repo.rs` | `SessionRepository`. |
| `command_repo.rs` | `CommandRepository`. |
| `shell_repo.rs` | `ShellRepository`. |
| `transfer_repo.rs` | `TransferRepository`. |
| `forward_repo.rs` | `ForwardRepository` (feature-gated). |
| `notifier.rs` | `NotifierPort` + `PeerHandle`. |
| `output_stream.rs` | `OutputStreamPort` — `snapshot_command`, `snapshot_shell`. |
| `subscriber_registry.rs` | `SubscriberRegistryPort` (sync) + `SubscriberRegistryAsync`. |
| `lifecycle_policy.rs` (v5) | `LifecyclePolicyPort` (sync slice — `track_resource`, `on_subscribe`, `on_unsubscribe`, `force_close`, `snapshot`) + `LifecyclePolicyAsync` (`arm_grace_timer`, `cancel_grace_timer`). |
| `channel_mux.rs` (v5) | `ChannelMuxPort` — `active_lane_count`, `aggregate_stats`. Drives the round-robin drainer. |
| `subscriber_lane.rs` (v5) | `SubscriberLanePort` (sync — `stats_snapshot`, `current_cursor`, `advance_cursor`, `list_subs`) + `SubscriberLaneAsync` (`open_lane`, `close_lane`, `pause_lane`, `resume_lane`, `set_filter`, `replay_from_cursor`) + `LaneAdmin` (dyn-safe shim for cold-path lane open/close). |
| `auth_strategy.rs` | `AuthStrategyPort`. |
| `clock.rs` | `ClockPort`. |
| `config.rs` | `ConfigPort` (~33 typed accessors in v5). |
| `id_generator.rs` | `IdGeneratorPort`. |

### `src/application/` — use cases

22 files, one per business operation. Each is a `*UseCase<Ports...>` struct with a single `pub async fn execute(&self, req: Request) -> Result<Outcome, DomainError>` entry point. Phase 1 wired the lifecycle layer into `open_shell`, `execute_command`, `upload_file`, `download_file` (commit `525b795`); Phase 2 wired the subscriber lane into `subscribe_resource` and `unsubscribe_resource` (commit `30fbdd9`). Phase 3 (in flight) will add 9 new use case files for the new MCP tools (`ssh_subscribe`, `ssh_unsubscribe`, `ssh_sub_pause`, `ssh_sub_resume`, `ssh_sub_filter`, `ssh_sub_replay`, `ssh_sub_list`, `ssh_sub_stats`, `ssh_daemon_stats`).

| Domain | Use cases |
|--------|-----------|
| Connection | `connect_session`, `disconnect_session`, `list_sessions`, `disconnect_agent` |
| Commands | `execute_command`, `get_command_output`, `list_commands`, `cancel_command` |
| Shell | `open_shell`, `write_shell`, `send_key`, `read_shell`, `wait_for_pattern`, `close_shell` |
| SFTP | `upload_file`, `download_file`, `get_transfer_progress` |
| Network | `forward_port` (feature-gated `port_forward`) |
| Resources | `list_resources`, `read_resource`, `subscribe_resource`, `unsubscribe_resource` |
| Background | `peer_gc` |

### `src/adapters/` — concrete adapters

| Adapter | Port(s) | File |
|---------|---------|------|
| `RusshAdapter` | `SshClientPort` | `ssh/russh_adapter.rs` |
| `RusshSftpAdapter` | `SftpClientPort` | `sftp/russh_sftp_adapter.rs` |
| `DashMap*Repo` | `*Repository` (5) | `repo/dashmap/{session,command,shell,transfer,forward}.rs` |
| `RmcpNotifier` + `RmcpPeerHandle` + `PeerTable` | `NotifierPort` + `PeerHandle` | `notifier/{rmcp_adapter,rmcp_peer}.rs` |
| `MemoryRegistry<N>` | `SubscriberRegistryPort` + `SubscriberRegistryAsync` | `subscription/memory_registry.rs` |
| `RefcountedLifecycleAdapter` (v5) | `LifecyclePolicyPort` + async slice | `lifecycle/{mod,refcount,grace_timer,cascade}.rs` |
| `SubscriberLane` (v5) | `SubscriberLanePort` + async slice | `subscription/subscriber_lane.rs` |
| `ChannelMux` (v5) | `ChannelMuxPort` | `subscription/channel_mux.rs` |
| `FilterPipeline` (v5) | internal to lane | `subscription/filter.rs` |
| `Replay` helper (v5) | internal to lane | `subscription/replay.rs` |
| `RusshOutputAdapter` | `OutputStreamPort` | `output_stream/russh_output.rs` |
| `AuthChainAdapter` | `AuthStrategyPort` | `auth/{password,key,agent,chain}.rs` |
| `SystemClock` (+ `FakeClock` under `#[cfg(any(test, feature = "test-fixtures"))]`) | `ClockPort` | `clock/{system,fake}.rs` |
| `EnvConfig` (+ `MemoryConfig`) | `ConfigPort` | `config/{env,memory}.rs` |
| `UuidIds` (+ `DeterministicIds`) | `IdGeneratorPort` | `id_generator/{uuid,deterministic}.rs` |

### `src/infra/mcp/` — inbound MCP transport

| File | Role |
|------|------|
| `server.rs` | `McpSshServer<UC>` — generic over the `UseCases<…>` container. |
| `tool_router.rs` | `#[tool_router]` impl populating the 21 v4 + 9 v5 = 30 `#[tool]` entry points (29 without `port_forward`). Phase 3 (in flight) wires the 9 new tools and updates `HINT:` severity. |
| `results.rs` | Typed Rust output structs for every tool's `structured_content` payload. |
| `prompts.rs` | `prompts/list` + `prompts/get` catalog — 10 workflows in v5 (5 carry-overs + 5 push-first; Phase 3 in flight). See [docs/llm-ux/PROMPTS_CATALOG.md](./llm-ux/PROMPTS_CATALOG.md). |
| `idempotency.rs` | DashMap-backed LRU cache — dedup mutating tool calls by `_meta.idempotency_key`. |
| `progress.rs` | `notifications/progress` emitter — best-effort, mid-flight on long async waits. |
| `suggestions.rs` | NOT_FOUND closest-match picker (Levenshtein top-3, lock-free). |
| `error_detail.rs` (v5) | Code-by-code `DETAIL:` line renderer aligned with [ADR 0007](./adr/0007-error-taxonomy.md) and [docs/llm-ux/ERROR_HANDBOOK.md](./llm-ux/ERROR_HANDBOOK.md). |
| `resource_templates.rs` | `resources/templates/list` static catalogue (4 / 5 RFC 6570 URI shapes). |
| `resource_handlers.rs` | Adapters between rmcp `resources/{list,read,subscribe,unsubscribe}` payloads and the matching `*_resource` use cases. |
| `peer_handle.rs` | Type aliases (`PeerTable`, `RmcpPeerHandle`). |
| `args/` | One module per tool domain (`connection`, `execute`, `shell`, `sftp`, `forward`; v5 adds `subscription`). |
| `render/` | One module per tool domain rendering the use case outcome into the markdown body. |
| `helpers/` | Shared primitives — `error::format_error`, `nonce::generate_nonce`, `output::render_output_block`, plus UTF-8 safe truncation. |

### `src/composition/` — root wiring

| File | Role |
|------|------|
| `mod.rs` | The generic `UseCases<…>` container (compiled twice via `#[cfg]` — `port_forward` toggle). |
| `prod.rs` | Production wiring — pins concrete adapters via `type ConcreteX = …`; exposes `build_use_cases()`, `build_server()`, `run_http()`, `run_stdio()`. v5 commit `4730066` injected `SubscriberLane` + `ChannelMux` adapters. |
| `embed.rs` (Phase 4 — in flight) | In-process duplex transport for `ssh-mcp-tail`. Spawns the rmcp server task; the daemon's main loop owns the rmcp client side. |
| `fixtures.rs` | Deterministic adapters for tests (gated by `test-fixtures`). |
| `id_lister.rs` | Helper consumed by the closest-match suggestion path. |
| `status_sinks.rs` | Adapter wiring for the v4.8.1 transfer progress watcher. |

### Adapter-internal modules

Each runtime adapter owns its private internals under `internal/` (the v4.1 deep-decouple closed `src/mcp/`).

| File | Owner adapter | Role |
|------|---------------|------|
| `src/adapters/ssh/internal/{client,session,async_command,shell,types,error}.rs` | SSH | russh wiring — `connect_to_ssh_with_retry`, `execute_ssh_command`, `open_pty_shell`, `RunningCommand`, `RunningShell` + `RingBuffer`, retry classification. |
| `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` | SSH | Internal auth chain (PasswordAuth -> KeyAuth -> AgentAuth) statically dispatched through an enum (no `async-trait`). |
| `src/adapters/sftp/internal/{sftp,transfer,types}.rs` | SFTP | Streaming SFTP session helpers; `RunningTransfer` lock-free state. |
| `src/adapters/config/internal/mod.rs` | Config | Env-var resolvers (33+ tunables in v5). |
| `src/adapters/subscription/legacy.rs` | Subscription (transitional) | `SubscriptionRegistry` global + `spawn_peer_gc`. Coexists with `MemoryRegistry<N>` until the SSH/SFTP runtime adapters are wired through the port surface end-to-end. |

## Phase 1 — Lifecycle binding (merged)

ADR: [adr/0003-lifecycle-binding.md](./adr/0003-lifecycle-binding.md). Every long-lived resource (shell, async command, SFTP transfer, port forward) is wrapped in a CAS state machine plus a per-session refcount. The atomic state machine lives in `src/adapters/lifecycle/refcount.rs`; the grace timer task lives in `src/adapters/lifecycle/grace_timer.rs`; cascade through `SessionLifecycle.active_refs` lives in `src/adapters/lifecycle/cascade.rs`.

### State machine

```text
       subscribe (sub_count++)
     ┌────────────────────────┐
     │                        │
     ▼                        │
┌──────────────┐      ┌───────┴────────┐
│    Owned     │ ───▶ │   Observed     │
│ (sub_count=0)│ first│ (sub_count>=1) │
└──────┬───────┘ sub  └────────┬───────┘
       │                       │
       │ explicit close        │ last unsubscribe
       ▼                       ▼
┌──────────────┐      ┌────────────────┐
│    Closed    │ ◀─── │   Releasing    │
│ (released)   │grace │ (sub_count=0,  │
└──────────────┘timer │  timer armed)  │
       ▲      fires   └────────┬───────┘
       │                       │
       │                       │ new subscribe
       └───────────────────────┘  within grace
                                  ─▶ Observed
                                  (cancel timer)
```

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
stateDiagram-v2
    [*] --> Owned: open / execute / upload<br/>(sub_count = 0)
    Owned --> Observed: first on_subscribe<br/>sub_count 0 -> 1
    Observed --> Observed: on_subscribe<br/>on_unsubscribe<br/>(sub_count > 0)
    Observed --> Releasing: last on_unsubscribe<br/>policy.release_when_no_subs<br/>(arm grace_until_ms)
    Releasing --> Observed: on_subscribe within grace<br/>(cancel timer)
    Releasing --> Closed: grace timer fires<br/>(state CAS Releasing -> Closed)
    Owned --> Closed: explicit close
    Observed --> Closed: explicit close
    Closed --> [*]: cascade<br/>SessionLifecycle.active_refs--

    classDef owned fill:#21262d,color:#8b949e,stroke:#30363d
    classDef observed fill:#238636,color:#f0f6fc,stroke:#2ea043
    classDef releasing fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    classDef closed fill:#cf222e,color:#f0f6fc,stroke:#f85149

    class Owned owned
    class Observed observed
    class Releasing releasing
    class Closed closed
```

### Per-resource refcount

Per-resource state lives behind a single `Arc<ResourceLifecycle>`:

| Field | Type | Ordering |
|---|---|---|
| `state` | `AtomicU8` (encoded `LifecycleState`) | AcqRel writers, Acquire readers |
| `sub_count` | `AtomicUsize` | AcqRel `fetch_add` / `fetch_sub` |
| `grace_until_ms` | `AtomicU64` (epoch ms; 0 not armed) | Relaxed reads, Release writes |
| `policy` | `ArcSwap<LifecyclePolicy>` | hot-reload of grace_ms / cascade flag |
| `waker` | `Arc<tokio::sync::Notify>` | wakes the grace timer task |
| `session_id` | `SessionId` | parent ref for cascade |

The hot path takes zero `Mutex`. The `await_holding_lock`, `significant_drop_in_scrutinee`, and `mutex_atomic` clippy denials in `Cargo.toml` continue to enforce this.

### Cascade chain

`SessionLifecycle` aggregates resource refs:

| Field | Type |
|---|---|
| `active_refs` | `AtomicUsize` (shells + commands + transfers + manual `pin`) |
| `idle_until_ms` | `AtomicU64` |
| `state` | `AtomicU8` (`Active | Idle | Releasing | Closed`) |
| `policy` | `ArcSwap<SessionPolicy { persistent: bool, idle_grace_ms: u32 }>` |

`release_resource` is the single chokepoint:

1. CAS resource `state: Releasing -> Closed`.
2. Drop the owner handle (delegates to existing `Disconnect*UseCase`).
3. `session.active_refs.fetch_sub(1, AcqRel)`.
4. If `active_refs == 0` AND `!session.policy.persistent` AND not already `Releasing`, arm the session-level grace timer.

The session reaper consults `active_refs > 0` before honouring the inactivity TTL — refcount supersedes TTL. A session with active resources is never reaped by TTL alone.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
sequenceDiagram
    participant UC as Use case
    participant RL as ResourceLifecycle
    participant GT as Grace timer
    participant SL as SessionLifecycle
    participant Reaper as SessionReaper

    UC->>RL: track_resource(session_id)
    RL->>SL: active_refs.fetch_add(1, AcqRel)
    UC->>RL: on_subscribe()
    RL->>RL: state CAS Owned -> Observed
    Note over UC,RL: ... time passes, work happens ...
    UC->>RL: on_unsubscribe() (last)
    RL->>RL: state CAS Observed -> Releasing
    RL->>GT: arm grace_until_ms (= now + grace_ms)
    GT-->>RL: sleep_until(deadline)
    Note over GT: timer fires
    GT->>RL: state CAS Releasing -> Closed
    RL->>SL: active_refs.fetch_sub(1, AcqRel)
    alt active_refs == 0 AND not persistent
        SL->>SL: arm session-level grace
        Reaper->>SL: tick (consults active_refs)
        SL-->>Reaper: 0 - proceed disconnect
    else active_refs nonzero
        SL-->>Reaper: tick - defer, refcount positive
    end
```

### Defaults preserve v4 semantics

```text
LifecyclePolicy {
    release_when_no_subs: false,       // v4-compat: callers must close explicitly
    grace_ms: 2_000,                    // 2 s grace window when opted in
    cascade_session: true,
}

SessionPolicy {
    persistent: false,                  // honours the existing ssh_connect persistent flag
    idle_grace_ms: 5_000,               // 5 s before the session-level reaper fires
}
```

`release_when_no_subs` is exposed on `ssh_shell_open` / `ssh_execute` / `ssh_upload` / `ssh_download` (Phase 3 surfaces the parameter in the MCP tool schema). Phase 1 wired the layer with the v4-compat default; existing MCP hosts see no behaviour change.

### Loom invariants (Phase 1 — 4 new interleavings)

`tests/lockfree_invariants.rs` ships four new loom tests (commit `c01156d`): subscribe / unsubscribe race, grace fire vs re-subscribe, cascade double-disconnect, cursor monotonicity. See [LOCKS.md](./LOCKS.md) for the full lock-free invariants summary.

## Phase 2 — Channel mux + sub_id (merged)

ADR: [adr/0004-channel-mux-fairness.md](./adr/0004-channel-mux-fairness.md). The cursor key shifts from `(PeerId, Uri)` (v4) to `(SubId, Uri)` (v5). Each subscription now owns its own state. A new `ChannelMux` provides round-robin fair drain.

### SubId

`SubId` is a UUIDv7 wrapper (`src/domain/subscription.rs`). UUIDv7 carries a unix-ms timestamp prefix, making subscriptions sortable by creation time without a separate timestamp column.

### Per-lane state

Per-lane state lives behind `Arc<MultiplexLane>`:

| Field | Type | Why |
|---|---|---|
| `byte_cursor` | `Arc<AtomicU64>` | per-sub_id byte position (independent from peer cursor) |
| `tx` | `mpsc::Sender<SubscriptionMessage>` | bounded, single-producer (debouncer), single-consumer (lane task) |
| `policy` | `LagPolicy` (`BlockSlow | DropOldest | DropNewest | Snapshot`) | per-lane backpressure choice |
| `filter` | `ArcSwap<FilterRule>` | hot-reloadable regex / level pipeline |
| `lifecycle` | `Arc<ResourceLifecycle>` | links sub to ADR 0003 refcount |
| `stats` | `SubscriberStats` (8 atomics) | events_sent, lag_drops, queue_depth, ... |
| `pause_flag` | `AtomicBool` | suspend drain without disconnecting |

### LagPolicy variants

| Variant | Behaviour | When to use |
|---|---|---|
| `BlockSlow` | Producer (debouncer) `.await`s on `mpsc::Sender::send` until consumer drains. Zero loss; latency cost = lag duration. | Forensic / audit log capture. |
| `DropOldest` | Pop oldest event from the lane's mpsc; push the new one. Emit `{"ev":"lagged","sub_id":...,"dropped":N}` marker. | Monitoring with gap tolerance. |
| `DropNewest` | Ignore the new event when full. Emit lagged marker. | Rare; prefer historical context. |
| `Snapshot` (default) | Drop the lane's mpsc backlog. Next drain triggers a `read_resource(uri, cursor=current_seq)` rebuild from the ring buffer. Emit `{"ev":"snapshot","sub_id":...,"cursor":N,"delta":<bytes>}`. Zero loss as long as the ring buffer covers the gap. | Default — best tradeoff. |

`Snapshot` is the default because the ring buffer already holds the live tail and rebuild is sub-ms on local memory. See [ADR 0006](./adr/0006-backpressure-policies.md) for the per-fronteira behaviour matrix.

### ChannelMux fairness

`ChannelMux` owns a `DashMap<SubId, MultiplexLane>` plus `cursor_lane: AtomicUsize` for round-robin drain (`src/adapters/subscription/channel_mux.rs`):

1. Snapshot active lanes via the DashMap iterator.
2. Park on `Notify` if no lanes are active.
3. Round-robin from `cursor_lane.load(Acquire)`. For each lane in order, `try_recv`. First non-empty wins.
4. Forward to the outbound writer (rmcp peer, NDJSON formatter, …).
5. `cursor_lane.store((idx + 1) % lanes.len(), Release)` (wrapping). Park if no lane had work.

Fairness invariant: between two adjacent backlogged lanes A and B, the mux drains them in alternation. A 10x faster lane will not starve a slower one. Verified under loom (commit `c48a0ba` — 4 new interleavings).

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    Prod["Producer<br/>russh / SFTP /<br/>health"]
    Deb["per-resource debouncer<br/>SSH_NOTIFY_DEBOUNCE_MS=50<br/>SSH_NOTIFY_FORCE_FLUSH_MS=1000"]

    subgraph LANES["per-(SubId, Uri) lanes"]
        direction TB
        L1["MultiplexLane A<br/>byte_cursor + tx<br/>filter + LagPolicy<br/>stats + pause"]
        L2["MultiplexLane B<br/>byte_cursor + tx<br/>filter + LagPolicy<br/>stats + pause"]
        L3["MultiplexLane C<br/>..."]
    end

    Mux["ChannelMux<br/>DashMap&lt;SubId,Lane&gt;<br/>cursor_lane AtomicUsize<br/>mux_waker Notify<br/>round-robin"]
    OutA["rmcp Peer<br/>notifications/<br/>resources/updated"]
    OutB["NDJSON formatter<br/>(ssh-mcp-tail stdout)"]

    Prod --> Deb
    Deb --> L1
    Deb --> L2
    Deb --> L3
    L1 --> Mux
    L2 --> Mux
    L3 --> Mux
    Mux --> OutA
    Mux --> OutB

    style Prod fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style Deb fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style L1 fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style L2 fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style L3 fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style Mux fill:#238636,color:#f0f6fc,stroke:#2ea043
    style OutA fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style OutB fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style LANES fill:#161b22,color:#f0f6fc,stroke:#30363d
```

### Resource lifecycle integration

When a `SubId` is registered, `lifecycle_policy.on_subscribe(kind, resource_id)` (ADR 0003) fires. When the last `SubId` on a `(kind, resource_id)` pair unsubscribes, `lifecycle_policy.on_unsubscribe` fires. The lifecycle layer decides whether to arm the grace timer based on the resource's policy (set at creation time by `open_shell` / `execute` / `upload` / `download`). Subscribers can never extend or shorten a resource's lifetime — only its creator can.

## Resource scheme topology

The 5 push-capable URI schemes map to the 4 long-running tools. Subscribers fan in through `(SubId, Uri)` lanes and exit through the `ChannelMux`.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    subgraph TOOLS["Long-running tools"]
        TShell["ssh_shell_open"]
        TExec["ssh_execute"]
        TSftp["ssh_upload<br/>ssh_download"]
        TFwd["ssh_forward<br/>(feature-gated)"]
    end
    subgraph SCHEMES["Resource URI schemes"]
        Sshell["shell://&lt;id&gt;/output<br/>(cursor)"]
        Scmd["command://&lt;id&gt;/output<br/>(cursor)"]
        Stransfer["transfer://&lt;id&gt;/progress<br/>(snapshot)"]
        Ssession["session://&lt;id&gt;/health<br/>(snapshot)"]
        Sforward["forward://&lt;id&gt;/events<br/>(cursor, gated)"]
    end
    Sub["ssh_subscribe<br/>(SubId, Uri) lane"]
    Push["resources/updated<br/>push events"]

    TShell --> Sshell
    TExec --> Scmd
    TSftp --> Stransfer
    TFwd --> Sforward
    Sshell --> Sub
    Scmd --> Sub
    Stransfer --> Sub
    Ssession --> Sub
    Sforward --> Sub
    Sub --> Push

    style TShell fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style TExec fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style TSftp fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style TFwd fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style Sshell fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Scmd fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Stransfer fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Ssession fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Sforward fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Sub fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style Push fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style TOOLS fill:#161b22,color:#f0f6fc,stroke:#30363d
    style SCHEMES fill:#161b22,color:#f0f6fc,stroke:#30363d
```

Per-scheme cursor + sequence semantics: [RESOURCES.md](./RESOURCES.md). Per-`(SubId, Uri)` lane atomics: [LOCKS.md](./LOCKS.md#subscription-mux-invariants-phase-2).

## Phase 3 — LLM UX surface (in flight)

ADR: [adr/0005-llm-ux-priorities.md](./adr/0005-llm-ux-priorities.md). 9 net-new MCP tools advertised as `ssh_subscribe`, `ssh_unsubscribe`, `ssh_sub_pause`, `ssh_sub_resume`, `ssh_sub_filter`, `ssh_sub_replay`, `ssh_sub_list`, `ssh_sub_stats`, `ssh_daemon_stats`. Plus `HINT:` line severity escalation (REQUIRED / RECOMMENDED / informational), 10-prompt catalog (5 carry-overs + 5 push-first), `SUB_LEAK_RISK` auto-warning watcher, and a 38-code error taxonomy ([ADR 0007](./adr/0007-error-taxonomy.md)).

The implementation is in flight on `feat/v5-foundation` by a separate agent. This document tracks the names; the canonical reference is [docs/llm-ux/PROMPTS_CATALOG.md](./llm-ux/PROMPTS_CATALOG.md), [docs/llm-ux/GOLDEN_RULES.md](./llm-ux/GOLDEN_RULES.md), [docs/llm-ux/ANTIPATTERNS.md](./llm-ux/ANTIPATTERNS.md), [docs/llm-ux/ERROR_HANDBOOK.md](./llm-ux/ERROR_HANDBOOK.md).

## Phase 4 — NDJSON daemon (in flight)

ADR: [adr/0008-ndjson-daemon-protocol.md](./adr/0008-ndjson-daemon-protocol.md). New binary `ssh-mcp-tail` with three subcommands (`run`, `shell`, `daemon`). Embeds an rmcp client + server pair across `tokio::io::duplex` via `composition::embed::wire()`. Stdin reads NDJSON ops; stdout emits NDJSON events (`ack`, `push`, `completed`, `lagged`, `snapshot`, `warn`, `heartbeat`, `daemon_stats`). The full op + event schema is enumerated in [docs/INSTRUCTIONS_DAEMON.md](./INSTRUCTIONS_DAEMON.md).

The daemon exists for tier-2 / tier-3 LLM hosts (Claude Desktop, Claude Code CLI, IDE integrations) that do not surface `notifications/resources/updated` to the model. Driving it as a subprocess gives the LLM real push delivery without host-level subscribe support.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart TB
    subgraph PIPE["Unix pipeline (host process)"]
        Stdin["stdin: NDJSON ops"]
        Stdout["stdout: NDJSON events"]
        Stderr["stderr: tracing"]
    end

    subgraph DAEMON["ssh-mcp-tail (one process)"]
        direction TB
        Disp["dispatcher<br/>parse NDJSON op"]
        EvMux["event_mux<br/>fan-in MCP notifs +<br/>tool replies"]
        Fmt["formatter<br/>JSON line writer"]
        subgraph BRIDGE["composition::embed::wire()"]
            ClientSide["rmcp client<br/>(stdin half)"]
            Duplex{{"tokio::io::duplex<br/>buffered"}}
            ServerSide["rmcp server<br/>(stdout half)"]
        end
        UC["UseCases&lt;...&gt;<br/>composition::prod adapters"]
    end

    Stdin --> Disp
    Disp --> ClientSide
    ClientSide <--> Duplex
    Duplex <--> ServerSide
    ServerSide --> UC
    UC --> ServerSide
    ServerSide --> EvMux
    EvMux --> Fmt
    Fmt --> Stdout
    UC -.-> Stderr

    style PIPE fill:#161b22,color:#f0f6fc,stroke:#30363d
    style DAEMON fill:#161b22,color:#f0f6fc,stroke:#30363d
    style BRIDGE fill:#161b22,color:#f0f6fc,stroke:#30363d
    style Stdin fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style Stdout fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style Stderr fill:#21262d,color:#8b949e,stroke:#30363d
    style Duplex fill:#238636,color:#f0f6fc,stroke:#2ea043
    style UC fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
```

Full op + event schema: [INSTRUCTIONS_DAEMON.md](./INSTRUCTIONS_DAEMON.md).

## Subscribe pipeline (v5 layered view)

```text
┌─────────────────────────────────────────────────────────────────┐
│ Producer (russh PTY reader, async cmd reader, SFTP, health)     │
│   poke(kind, id) -> next_seq -> RunningX broadcast::Sender      │
└─────────────────────┬───────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────────────┐
│ MemoryRegistry<N> (per-resource debouncer task)                 │
│   coalesce on SSH_NOTIFY_DEBOUNCE_MS (50 ms default)            │
│   force-flush on SSH_NOTIFY_FORCE_FLUSH_MS (1 s default)        │
│   keepalive on SSH_NOTIFY_KEEPALIVE_S (30 s default)            │
└─────────────────────┬───────────────────────────────────────────┘
                      │
                      ▼     ─── v5 fan-out ────────────────────
┌─────────────────────────────────────────────────────────────────┐
│ For each (SubId, Uri) — bounded mpsc lane                       │
│                                                                  │
│   filter(FilterRule)                                             │
│         ↓                                                        │
│   apply LagPolicy (BlockSlow / DropOldest / DropNewest / Snap)   │
│         ↓                                                        │
│   record SubscriberStats atomics                                 │
│         ↓                                                        │
│   mpsc::Sender<SubscriptionMessage>(N=SSH_LANE_BUFFER, def 1024) │
└─────────────────────┬───────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────────────┐
│ ChannelMux (round-robin fair drain)                             │
│   cursor_lane: AtomicUsize  (Acquire load, Release store)       │
│   wakes on Notify; no spinning                                  │
│   try_recv each lane in order; bump cursor on success           │
└─────────────────────┬───────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────────────┐
│ Outbound writer                                                  │
│   - rmcp Peer (notifications/resources/updated)                  │
│   - or NDJSON formatter (ssh-mcp-tail stdout)                    │
└─────────────────────────────────────────────────────────────────┘
```

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart TB
    P1["Producer<br/>russh PTY / async cmd /<br/>SFTP / health"]
    P2["MemoryRegistry&lt;N&gt;<br/>per-resource debouncer task<br/>coalesce 50 ms / flush 1 s /<br/>keepalive 30 s"]
    P3["per (SubId, Uri) lane<br/>filter -> LagPolicy -><br/>stats atomics -><br/>mpsc(SSH_LANE_BUFFER=1024)"]
    P4["ChannelMux<br/>cursor_lane AtomicUsize<br/>round-robin try_recv<br/>mux_waker Notify"]
    P5["Outbound writer<br/>rmcp Peer notifications/<br/>resources/updated<br/>or NDJSON formatter"]

    P1 --> P2
    P2 --> P3
    P3 --> P4
    P4 --> P5

    style P1 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style P2 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style P3 fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style P4 fill:#238636,color:#f0f6fc,stroke:#2ea043
    style P5 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
```

## Lock-free invariants summary

The full table — every atomic, every channel, every guard — lives in [LOCKS.md](./LOCKS.md). v5 adds three new categories:

- **Lifecycle adapter atomics** (Phase 1): `state` AtomicU8, `sub_count` AtomicUsize, `grace_until_ms` AtomicU64, `policy` ArcSwap, `waker` Arc<Notify>.
- **Subscription mux atomics** (Phase 2): per-lane mpsc, `cursor_lane` AtomicUsize, `pause_flag` AtomicBool, 8 stats atomics.
- **Cascade refcount** (Phase 1): `SessionLifecycle.active_refs` AtomicUsize. CAS Active -> Idle -> Releasing -> Closed.

`cargo clippy --release --all-features -- -D warnings` exit-0 invariant holds under v5. Loom coverage extends from v4's 8 invariants to 16 with Phase 1 + Phase 2.

## Configuration

The full env-var table (33+ tunables) lives in [CONFIGURATION.md](./CONFIGURATION.md). v5 adds five categories of new vars: lifecycle (`SSH_LIFECYCLE_*`, `SSH_SESSION_IDLE_GRACE_MS`), lane / mux (`SSH_LAG_POLICY_DEFAULT`, `SSH_LANE_BUFFER`, `SSH_MUX_BUFFER`, `SSH_BP_BLOCK_TIMEOUT_MS`, `SSH_REPLAY_WINDOW_BYTES`, `SSH_FILTER_REGEX_MAX`, `SSH_MAX_SUBS_PER_URI`, `SSH_MAX_SUBS_TOTAL`), LLM hygiene (`SSH_SUB_LEAK_RISK_WARN_S`, `SSH_SUB_LEAK_RISK_KILL_S`), daemon (`SSH_NDJSON_LINE_MAX`, `SSH_HEARTBEAT_INTERVAL_S`, `SSH_DAEMON_STATS_INTERVAL_S`, `SSH_GRACE_HARD_TIMEOUT_S`).

Defaults preserve v4 behaviour. v4 hosts pointed at v5 servers see no behavioural change unless they opt into the new flags / tools / env vars.

## Capabilities

`McpSshServer` declares the following capabilities through the rmcp `ServerHandler::get_info()` contract:

| Capability | Status |
|------------|--------|
| `tools/list` | 30 tools registered (29 without `port_forward`); every tool advertises a typed `outputSchema` |
| `tools/call` | One handler per tool, all returning `Result<CallToolResult, McpError>` |
| `resources/list` | 5 schemes (`shell`, `command`, `transfer`, `session`, `forward`) — see [RESOURCES.md](./RESOURCES.md) |
| `resources/read` | All 5 schemes with `_meta` envelope; cursor support on `shell` / `command` / `forward` |
| `resources/subscribe` + `resources/unsubscribe` | Per `(SubId, Uri)` cursor in v5; `(PeerId, Uri)` synthesised for legacy hosts |
| `notifications/resources/updated` | Emitted by the per-resource debouncer task |
| `notifications/resources/list_changed` | Capability advertised; emission still deferred (tracked under [Future work](#future-work)) |
| `prompts/list` + `prompts/get` | 10 workflows in v5 (5 carry-overs + 5 push-first) |
| `notifications/progress` | Best-effort; fires on long async waits when `_meta.progressToken` is supplied |
| Cancellation | Native via rmcp 1.6 |

## Cross references

- [LOCKS.md](./LOCKS.md) — lock-free patterns, acquisition order, channel sizing, loom invariants.
- [API.md](./API.md) — MCP tool reference (schemas, response shape).
- [RESOURCES.md](./RESOURCES.md) — `resources/*` contract, cursor semantics, `_meta` envelope.
- [ERRORS.md](./ERRORS.md) — wire-format error envelope (REASON + DETAIL).
- [CONFIGURATION.md](./CONFIGURATION.md) — env-var table.
- [FLOWS.md](./FLOWS.md) — end-to-end sequence diagrams.
- [INSTRUCTIONS_DAEMON.md](./INSTRUCTIONS_DAEMON.md) — `ssh-mcp-tail daemon` NDJSON op + event schema.
- [TROUBLESHOOTING.md](./TROUBLESHOOTING.md) — symptom -> cause -> cure runbook.
- [llm-ux/](./llm-ux/) — LLM UX kit: golden rules, prompts catalog, anti-patterns, error handbook, 27B / 70B root prompts.
- [MIGRATION_v4_to_v5.md](./MIGRATION_v4_to_v5.md) — host migration guide.
- [MIGRATION_v3_to_v4.md](./MIGRATION_v3_to_v4.md) — contributor migration narrative (v4.1 deep-decouple addendum).
- [adr/0001-migrate-to-rmcp.md](./adr/0001-migrate-to-rmcp.md) — v3 transport choice.
- [adr/0002-adopt-hexagonal-architecture.md](./adr/0002-adopt-hexagonal-architecture.md) — v4 architecture choice.
- [adr/0003-lifecycle-binding.md](./adr/0003-lifecycle-binding.md) — v5 lifecycle layer.
- [adr/0004-channel-mux-fairness.md](./adr/0004-channel-mux-fairness.md) — v5 sub_id + mux.
- [adr/0005-llm-ux-priorities.md](./adr/0005-llm-ux-priorities.md) — v5 LLM UX surface.
- [adr/0006-backpressure-policies.md](./adr/0006-backpressure-policies.md) — v5 LagPolicy.
- [adr/0007-error-taxonomy.md](./adr/0007-error-taxonomy.md) — v5 error categorisation.
- [adr/0008-ndjson-daemon-protocol.md](./adr/0008-ndjson-daemon-protocol.md) — v5 daemon binary.

## Future work

- **`notifications/resources/list_changed`** — wire emission through every use case that creates / destroys a tracked resource (capability advertised; `Notify` plumbing pending).
- **Subscription registry consolidation** — migrate the SSH/SFTP runtime adapters off `adapters::subscription::legacy::SUBSCRIPTION_REGISTRY` (current poker) and onto the `MemoryRegistry<N>` port handle, then delete the legacy adapter.
- **Multi-tenant scoping** — `tenant_id` field on `Subscriber` and the daemon's NDJSON envelope. Out of scope for v5.0.
- **HTTP/SSE bridge as separate binary** — alternative to `ssh-mcp-tail` for hosts that prefer HTTP. May ship in a later release.
- **Per-tenant rate-limit middleware** — when multi-tenant lands.
