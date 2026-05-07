# Migration Guide

Single source for every migration path through ssh-mcp's history. Three self-contained sections cover the v2 → v3 client migration, the v3 → v4 contributor migration (including the v4.1 deep-decouple addendum and the v4.7 → v4.8 / v4.8 → v4.8.1 addenda), and the v4 → v5 host migration.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    V2["v2.0<br/>poem-mcpserver"]
    V3["v3.0<br/>rmcp 1.6 + resources"]
    V4["v4.x<br/>hexagonal layout"]
    V5["v5.x<br/>subscribe-first"]
    V6["v6.0<br/>tool name eixos"]

    V2 -->|client migration<br/>5 breaking changes| V3
    V3 -->|contributor migration<br/>file-path table| V4
    V4 -->|host migration<br/>0 breaking| V5
    V5 -->|host migration<br/>tool name strings only| V6

    style V2 fill:#21262d,color:#8b949e,stroke:#30363d
    style V3 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style V4 fill:#238636,color:#f0f6fc,stroke:#2ea043
    style V5 fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style V6 fill:#cf222e,color:#f0f6fc,stroke:#f85149
```

| Section | Audience | Scope |
|---|---|---|
| [v2 → v3](#v2--v3) | MCP client / host implementors | rmcp 1.6 transport, 18 tools, 5 `resources/*` schemes |
| [v3 → v4](#v3--v4) | Codebase contributors | Hexagonal restructuring, `src/mcp/` deletion, AFIT ports, v4.1 deep decouple, v4.7→v4.8 addendum |
| [v4 → v5](#v4--v5) | MCP host operators / contributors / downstream automations | Subscribe-first, lifecycle binding, channel mux, daemon binary |
| [v5 → v6](#v5--v6) | MCP host operators / contributors / downstream automations | Tool name eixos (`ssh_*` / `sub_*` / `serial_*`); wire-breaking on tool name strings only |

The 9 ADRs at [adr/](./adr/) are the canonical source for every design decision. Read in order: [0001 rmcp](./adr/0001-migrate-to-rmcp.md), [0002 hexagonal](./adr/0002-adopt-hexagonal-architecture.md), [0003 lifecycle](./adr/0003-lifecycle-binding.md), [0004 mux+sub_id](./adr/0004-channel-mux-fairness.md), [0005 LLM UX](./adr/0005-llm-ux-priorities.md), [0006 backpressure](./adr/0006-backpressure-policies.md), [0007 errors](./adr/0007-error-taxonomy.md), [0008 daemon](./adr/0008-ndjson-daemon-protocol.md), [0009 serial](./adr/0009-serial-transport.md).

---

## v2 → v3

For **client / host implementors** upgrading from `ssh-mcp` 2.0.x to 3.0.0. If you only run the server binary you do not need to change anything except your transport library version. Design rationale: [ADR 0001 — Migrate to rmcp](./adr/0001-migrate-to-rmcp.md).

### Breaking changes

1. **Transport library changed from `poem-mcpserver` 0.3 to `rmcp` 1.6** (the official Anthropic Rust SDK). HTTP transport now follows the **Streamable HTTP MCP** wire format with an SSE channel for server-initiated notifications. Default endpoint is `/` (configurable via `MCP_HTTP_PATH`).
2. **The stdio binary's custom JSON-RPC quirks are gone.** The v2 stdio loop carried a hand-rolled `notifications/cancelled` parser that swallowed responses for cancelled IDs (`camelCase` and `snake_case` both). rmcp handles cancellation natively, so the wire shape is now purely spec-compliant.
3. **Response markdown is now block-only.** v2 mixed inline (`KEY: V | KEY: V`) and block forms depending on field count; v3 always emits one `KEY: value` per line. Parsers that only support the block form keep working; parsers that special-cased the inline form must be updated.
4. **`ReusePolicy` and `CommandStatus` are typed enums** in the JSON schema. v2 accepted `Option<String>` and silently fell back on typos; v3 returns a schema validation error.
5. **Two new tools were added.** `ssh_shell_press` and `ssh_shell_wait_for`. The total is 18 (was 16).
6. **Five `resources/*` schemes are now exposed.** `shell://`, `command://`, `transfer://`, `session://`, `forward://`. Subscribing yields `notifications/resources/updated` per debounce window.

### Compatibility matrix

| Feature                          | v2.0                                           | v3.0                                                                  |
| -------------------------------- | ---------------------------------------------- | --------------------------------------------------------------------- |
| Server SDK                       | `poem-mcpserver` 0.3                           | `rmcp` 1.6                                                            |
| HTTP transport                   | Poem streamable HTTP                           | rmcp `StreamableHttpService` (axum-hosted) + SSE notification channel |
| HTTP path                        | `/`                                            | `/` (configurable via `MCP_HTTP_PATH`)                                |
| `Mcp-Session-Id` header          | not used                                       | tracked by rmcp's `LocalSessionManager`                               |
| Tool count                       | 16                                             | **18** (`ssh_shell_press`, `ssh_shell_wait_for` added)             |
| `resources/*`                    | not implemented                                | 5 schemes (`shell`, `command`, `transfer`, `session`, `forward`)      |
| Server-initiated notifications   | none                                           | `notifications/resources/updated` (deferred: `list_changed`); cancellation handled natively by rmcp |
| Response format                  | mixed inline / block                           | block-only                                                            |
| `ssh_connect.reuse`              | `Option<String>`                               | `ReusePolicy` enum (`suggest \| auto \| force_new`)                   |
| `ssh_commands.status`       | `Option<String>`                               | `CommandStatus` enum (`running \| completed \| cancelled \| failed`)  |
| Stdio cancel-id parser           | custom (`camelCase` + `snake_case`)            | removed — rmcp native                                                 |

### Code changes for clients

#### Connect

v2 (loose schema):

```json
{
  "name": "ssh_connect",
  "arguments": {
    "address": "host:22",
    "username": "root",
    "reuse": "auto"
  }
}
```

v3 (typed schema — same wire JSON, schema-validated):

```json
{
  "name": "ssh_connect",
  "arguments": {
    "address": "host:22",
    "username": "root",
    "reuse": "auto"
  }
}
```

If you previously sent `"reuse": "Auto"` or `"reuse": "AUTO"`, rmcp will now reject the call with an `INVALID_PARAMS` JSON-RPC error. Use the `snake_case` literal `"auto"`.

#### Execute and poll

Wire format unchanged; the response markdown is now strictly block style:

```
SSH_EXEC_OUTPUT: COMPLETED
COMMAND_ID: 7d31...
EXIT: 0
--- stdout [a3f2b1d7] ---
foo bar
--- stderr [a3f2b1d7] (empty) ---
```

#### Interactive shell — old (poll loop)

```
ssh_shell_open
loop {
  ssh_shell_read (clear=true)
  ssh_shell_write
}
ssh_shell_close
```

#### Interactive shell — new (subscribe-first)

```
ssh_shell_open                            -> SHELL_ID
resources/subscribe shell://SHELL_ID/output

# parallel:
ssh_shell_write / ssh_shell_press

# notifications/resources/updated arrives ->
resources/read shell://SHELL_ID/output?cursor=auto

ssh_shell_close
```

The polling path still works (`ssh_shell_read` is a documented FALLBACK for hosts that cannot consume notifications), but you give up roughly half the tokens you would otherwise spend.

### ReusePolicy enum

v2 used `Option<String>` accepting `"suggest" | "auto" | "force_new"`. v3 promotes this to a tagged enum rendered into the JSON schema:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReusePolicy {
    Suggest,    // default
    Auto,
    ForceNew,
}
```

Wire format for valid values is unchanged. Typos now produce a schema validation error.

### CommandStatus enum

Same treatment for `ssh_commands.status`:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}
```

### Recommended upgrade path

1. **Update your MCP host** to a release that supports the Streamable HTTP MCP transport (or compatible stdio). Anthropic Claude Desktop 0.7+, MCP Inspector 0.4+, and any rmcp-based host work out of the box.
2. **Switch tool argument deserialization** to typed enums for `ReusePolicy` and `CommandStatus` if your client validates schemas before sending.
3. **Subscribe to `resources/*`** for realtime UX wins. On long-running shells the token spend drops by roughly 50% versus the v2 polling loop because deltas are pulled with `?cursor=auto` instead of full snapshots.
4. **Drop your custom cancel-id handling** if you previously worked around the stdio quirks. rmcp follows the spec.

### Server-side env var changes

See [CONFIGURATION.md](./CONFIGURATION.md) for the full list. Highlights:

| New in v3                          | Default | Purpose                                                                          |
| ---------------------------------- | ------- | -------------------------------------------------------------------------------- |
| `SSH_NOTIFY_DEBOUNCE_MS`           | 200     | Debounce window for `notifications/resources/updated`.                           |
| `SSH_NOTIFY_FORCE_FLUSH_MS`        | 1000    | Maximum gap between notifications under continuous activity.                     |
| `SSH_NOTIFY_KEEPALIVE_S`           | 30      | Idle keepalive interval per resource.                                            |
| `SSH_MCP_PEER_GC_INTERVAL_S`       | 30      | Period of the peer-GC scan (drops subscriptions for closed transports).          |
| `SSH_SHELL_BROADCAST_CAP`          | 1024    | Capacity of the shell `output_tx` broadcast channel.                             |
| `SSH_COMMAND_BROADCAST_CAP`        | 1024    | Capacity of the command `output_tx` broadcast channel.                           |
| `SSH_TRANSFER_BROADCAST_CAP`       | 256     | Capacity of the transfer `progress_tx` broadcast channel.                        |
| `SSH_SESSION_BROADCAST_CAP`        | 256     | Capacity of the session `health_tx` broadcast channel.                           |
| `SSH_FORWARD_BROADCAST_CAP`        | 256     | Capacity of the forward `events_tx` broadcast channel.                           |
| `MCP_HTTP_PATH`                    | `/`     | HTTP route prefix for the rmcp `StreamableHttpService`.                          |

### Removed env vars

None. All v2 knobs continue to work with the same semantics.

### Wire-format gotchas

- `Mcp-Session-Id`: sent by the rmcp HTTP transport as a session correlation header. Hosts that do not understand it can ignore it; hosts that drop unknown headers will still work because the server falls back to the SSE channel for state.
- `notifications/resources/updated`: payload is `{ "uri": "<uri>" }`. There is no diff in the notification — the host must call `resources/read?cursor=auto` to get the bytes.
- `notifications/resources/list_changed`: capability is advertised but not currently emitted. Treat as forward-compatible.

---

## v3 → v4

For **codebase contributors** moving from v3.0.x to the v4.x hexagonal layout (current: v4.1.0). The **public MCP API is stable** across the v4 line — every external client / host / LLM continues to work without any change. v4 is an internal restructuring, not a wire-format break.

If you only consume the MCP server (HTTP or stdio), stop here — see [API.md](./API.md) for the unchanged tool catalogue.

> **v4.1 update.** The H17.6 deep decouple shipped: the foundational `src/mcp/` tree is gone, the `async-trait` direct dependency is dropped, and every former `crate::mcp::*` reference now lives at `crate::adapters::{ssh,sftp,config,subscription}::internal::*` (or `adapters::subscription::legacy` for the transitional global registry). See the [v4.1 deep decouple addendum](#v41-deep-decouple-addendum) below.

Cross references: [ARCHITECTURE.md](./ARCHITECTURE.md) (full v4 hexagonal layout), [DEVELOPMENT.md → Lock-free invariants](./DEVELOPMENT.md#lock-free-invariants), [adr/0002-adopt-hexagonal-architecture.md](./adr/0002-adopt-hexagonal-architecture.md) (design rationale).

### What stayed identical (zero client work)

| Surface | v3 → v4 status |
|---------|----------------|
| MCP tool count | **18** (same) |
| MCP tool names + signatures | identical |
| MCP tool response markdown shape | identical (block-only, 8-hex nonce delimiters, `KEY: value` per line) |
| 5 `resources/*` schemes | identical URIs + `_meta` envelope + cursor semantics |
| `notifications/resources/updated` | identical (per debounce window) |
| `notifications/cancelled` | identical (rmcp 1.6 native) |
| Capability handshake (`get_info()`) | identical |
| 25+ env vars (`SSH_*`, `MCP_*`, `RUST_LOG`) | identical names, defaults, floors, caps |
| HTTP transport bind / path defaults (`0.0.0.0:8000`, `/`) | identical |
| Stdio transport behaviour | identical |
| Strict Clippy lint baseline (forbid / deny + lock-free invariants) | identical (every v3 lint kept) |
| Cargo features (`port_forward` default-on) | identical |

In short: a v3 host pointed at a v4 server sees no observable difference. Even error markdown bodies and the 8-hex nonce randomisation are byte-identical.

### What moved (file-path table)

The v3.0.0 codebase shipped under `src/mcp/` only. v4.0.0 introduces six top-level layers under `src/`:

```
src/
  domain/         # pure entities, value objects, errors, live event variants
  ports/          # trait skeletons (sync + async via trait-variant)
  application/    # use cases (one struct + DTO per business operation)
  adapters/       # concrete implementations of every port
  infra/          # inbound MCP transport (#[tool_router], ServerHandler, render, args, helpers)
  composition/    # root wiring (concrete adapter pinning + binary entry points)
  bin/            # ssh-mcp-stdio thin shell
  main.rs         # ssh-mcp HTTP thin shell
  lib.rs
```

#### Tool implementations

| v3 path | v4 path |
|---------|---------|
| `src/mcp/tools/connection.rs` | application: `src/application/{connect_session,disconnect_session,list_sessions,disconnect_agent}.rs` ; render: `src/infra/mcp/{args,render}/connection.rs` ; rmcp wiring: `src/infra/mcp/tool_router.rs` |
| `src/mcp/tools/execute.rs` | application: `src/application/{execute_command,get_command_output,list_commands,cancel_command}.rs` ; render: `src/infra/mcp/{args,render}/execute.rs` |
| `src/mcp/tools/shell.rs` | application: `src/application/{open_shell,write_shell,send_key,read_shell,wait_for_pattern,close_shell}.rs` ; render: `src/infra/mcp/{args,render}/shell.rs` |
| `src/mcp/tools/sftp.rs` | application: `src/application/{upload_file,download_file,get_transfer_progress}.rs` ; render: `src/infra/mcp/{args,render}/sftp.rs` |
| `src/mcp/tools/forward.rs` (feature `port_forward`) | application: `src/application/forward_port.rs` ; render: `src/infra/mcp/{args,render}/forward.rs` |
| `src/mcp/tools/legacy_helpers.rs` | deleted in H17.5a; helpers now under `src/infra/mcp/helpers/{error,nonce,output}.rs` |

#### Storage layer

| v3 path | v4 path |
|---------|---------|
| `src/mcp/storage/{traits,session,command,shell,transfer,forward}.rs` + globals | ports: `src/ports/{session_repo,command_repo,shell_repo,transfer_repo,forward_repo}.rs` ; adapters: `src/adapters/repo/dashmap/{session,command,shell,transfer,forward}.rs` (no globals — wired by `composition::prod`) |

#### MCP server + resources

| v3 path | v4 path |
|---------|---------|
| `src/mcp/server.rs` (`McpSshServer` + `#[tool_router]`) | `src/infra/mcp/server.rs` (`McpSshServer<UC>` generic) + `src/infra/mcp/tool_router.rs` (the 18 `#[tool]` entry points) + `src/infra/mcp/resource_handlers.rs` (resources/list, read, subscribe, unsubscribe) |
| `src/mcp/resources.rs` (URI parser + reader handlers) | application: `src/application/{list_resources,read_resource,subscribe_resource,unsubscribe_resource}.rs` ; rmcp wiring: `src/infra/mcp/resource_handlers.rs` |
| `src/mcp/message/{helpers,builder}.rs` | helpers: `src/infra/mcp/helpers/{error,nonce,output}.rs` ; builders: `src/infra/mcp/render/{connection,execute,shell,sftp,forward}.rs` |
| `src/mcp/schema.rs` | inlined into per-tool args structs under `src/infra/mcp/args/*.rs` |
| `src/mcp/keys.rs` | `src/domain/keys.rs` (domain layer owns the semantic keystroke encoder) |

#### Authentication

| v3 path | v4 path |
|---------|---------|
| `src/mcp/auth/{traits,password,key,agent,chain}.rs` (uses `#[async_trait]`) | port: `src/ports/auth_strategy.rs` ; adapters: `src/adapters/auth/{password,key,agent,chain}.rs` ; runtime chain (post v4.1): `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` (native AFIT + enum dispatcher; `async-trait` direct dep dropped) |

#### Subscription registry

| v3 path | v4 path |
|---------|---------|
| `src/mcp/subscription.rs` (`SUBSCRIPTION_REGISTRY` global + per-resource debouncer + peer-GC) | port: `src/ports/subscriber_registry.rs` ; hexagonal adapter: `src/adapters/subscription/memory_registry.rs` (`MemoryRegistry<N>` generic over the notifier) ; transitional global: `src/adapters/subscription/legacy.rs` (`SUBSCRIPTION_REGISTRY` + `spawn_peer_gc`) |

#### Notifier

| v3 path | v4 path |
|---------|---------|
| `rmcp::Peer<RoleServer>` plumbed by hand into every tool | port: `src/ports/notifier.rs` (`NotifierPort` async + `PeerHandle` sync) ; adapters: `src/adapters/notifier/{rmcp_adapter,rmcp_peer}.rs` ; `PeerTable` re-exposed under `src/infra/mcp/peer_handle.rs` |

### What changed for contributors

#### Static dispatch + AFIT

Every async port is declared via `#[trait_variant::make(Port: Send)]`. Use cases stay generic over the port type — no `Box<dyn Trait>`, no `async-trait` boxing. Example:

```rust
// src/ports/ssh_client.rs
#[trait_variant::make(SshClientPort: Send)]
pub trait LocalSshClientPort: Send + Sync {
    async fn connect(&self, ...) -> Result<SessionEntity, DomainError>;
}

// src/application/connect_session.rs
pub struct ConnectSessionUseCase<S, SR, C, Idg, Cfg>
where
    S: SshClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    // ...
{
    ssh: Arc<S>,
    sessions: Arc<SR>,
    // ...
}
```

The composition root pins one concrete adapter per port (see `src/composition/prod.rs`):

```rust
type ConcreteSsh = RusshAdapter;
type ConcreteSessionRepo = DashMapSessionRepo;
// ...
pub type ProdUseCases = UseCases<ConcreteSsh, ConcreteSftp, /* ... */>;
```

Wiring errors surface at the composition root, not at runtime. The legacy `mcp::auth` module that pinned `#[async_trait]` was deleted in v4.1 H17.6 P2; the runtime chain now lives at `adapters/ssh/internal/auth/` with native AFIT + enum dispatch.

#### Layer import rules

| Layer | Allowed deps | Forbidden deps |
|-------|--------------|----------------|
| `domain` | `std`, `serde`, `serde_json`, `chrono`, `thiserror`, `schemars`, `bytes` | `tokio`, `russh`, `rmcp`, `axum`, `dashmap` |
| `ports` | `domain`, `bytes`, `chrono`, `std`, `trait_variant` | `tokio`, `russh`, `rmcp`, `axum`, `dashmap` |
| `application` | `domain`, `ports`, `tokio` (`select!` / `spawn`) | `russh`, `rmcp`, `axum`, `dashmap` |
| `adapters` | All runtime crates | `rmcp` (only `notifier/rmcp_*`), `axum` |
| `infra` | `rmcp`, `application`, `domain`, `adapters::notifier::rmcp_peer` | `russh`, `russh_sftp`, `dashmap` |
| `composition` | All adapters + `infra::mcp` + `application` | — (it is the leaf) |

When in doubt, run `cargo clippy --all-features --all-targets --workspace -- -D warnings` and read the import error.

#### Adding a new tool

In v3 you would touch `src/mcp/tools/<domain>.rs` (args + business logic + render + rmcp wiring all in one file), `src/mcp/server.rs` (route entry), and `src/mcp/storage/<repo>.rs` (state).

In v4 the same change spans:

1. `src/domain/<entity>.rs` — entity + DTO + status enums (if any).
2. `src/ports/<port>.rs` — extend the trait surface (only if a new capability is needed).
3. `src/adapters/<adapter>.rs` — implement the new port method.
4. `src/application/<use_case>.rs` — write the use case (single `pub async fn execute(&self, req: Request) -> Result<Outcome, DomainError>`).
5. `src/infra/mcp/args/<domain>.rs` — `#[derive(Deserialize, JsonSchema)]` argument struct.
6. `src/infra/mcp/render/<domain>.rs` — markdown body builder.
7. `src/infra/mcp/tool_router.rs` — add the `#[tool]` entry, parse args, call the use case, render the outcome, return `CallToolResult`.
8. `src/composition/mod.rs` — extend `UseCases<…>` if a new use case generic appears.
9. `src/composition/prod.rs` — wire the new `Arc<UseCase<…>>` into `build_use_cases`.

The tradeoff is real (more files per change) and intentional: every layer is independently testable with fakes (`adapters::ssh::fake`, `adapters::sftp::fake`, `adapters::clock::fake`, etc.).

#### Writing a use case test

Use cases never instantiate `RusshAdapter` directly — they take `Arc<S: SshClientPort>`. Tests load `Arc<FakeSshAdapter>` instead:

```rust
#[tokio::test]
async fn connect_session_returns_reused_when_session_alive() {
    let ssh = Arc::new(FakeSshAdapter::default());
    let sessions = Arc::new(DashMapSessionRepo::default());
    let clock = Arc::new(FakeClock::new(...));
    let ids = Arc::new(DeterministicIds::new(...));
    let cfg = Arc::new(MemoryConfig::default());

    let uc = ConnectSessionUseCase::new(ssh, sessions, clock, ids, cfg);
    let outcome = uc.execute(ConnectRequest { /* ... */ }).await.expect("ok");
    assert!(matches!(outcome, ConnectOutcome::Reused(_)));
}
```

Composition isolation makes these tests fast (no russh, no real SFTP, deterministic IDs).

#### Lints (unchanged baseline)

Every v3 lint stays. The lock-free invariants (`await_holding_lock`, `mutex_atomic`, `mutex_integer`, `significant_drop_in_scrutinee`, `significant_drop_tightening`) now apply to `src/adapters/repo/dashmap/*`, `src/adapters/subscription/{memory_registry,legacy}.rs`, and the adapter-internal carriers under `src/adapters/{ssh,sftp}/internal/*`. See [DEVELOPMENT.md](./DEVELOPMENT.md#lock-free-invariants).

### Recommended workflow for an in-flight v3 patch

If you have a v3 patch sitting on a branch:

1. Identify which `src/mcp/tools/<domain>.rs` function the patch touches.
2. Use the [Tool implementations](#tool-implementations) table above to find the v4 split.
3. Move business logic into the use case; move markdown builders into `render/`; move arg structs into `args/`.
4. If the patch touches global storage, port the writes onto the relevant `*Repository` port and let the composition root pick `DashMap*Repo`.
5. Run `cargo test --all-features` (must pass) and `cargo clippy --all-features --all-targets --workspace -- -D warnings` (must be clean).

### Build / test / run (unchanged)

Same commands as v3:

```bash
cargo build --release                              # both binaries
cargo build --release --bin ssh-mcp                # HTTP only
cargo build --release --bin ssh-mcp-stdio          # stdio only
cargo build --release --no-default-features        # without port forwarding

cargo test --lib --quiet                           # 1014 lib tests
cargo test --tests --quiet                         # integration tests
cargo test --all-features                          # combined run

cargo fmt --all -- --check
cargo clippy --all-features --all-targets --workspace -- -D warnings
```

`MSRV` is now `1.85` (Rust 2024 edition + AFIT stable). `axum` was upgraded from `0.7` to `0.8`.

### v4.1 deep decouple addendum

H17.6 P1+P2+P3+P4 (commits `bf646f9`, `00009e3`, `72f1ccd`) shipped in v4.1.0. The foundational `mcp::*` paths are gone:

| Former v4.0 path | v4.1 path |
|------------------|-----------|
| `src/mcp/client.rs` | `src/adapters/ssh/internal/client.rs` |
| `src/mcp/session.rs` | `src/adapters/ssh/internal/session.rs` |
| `src/mcp/async_command.rs` | `src/adapters/ssh/internal/async_command.rs` |
| `src/mcp/shell.rs` | `src/adapters/ssh/internal/shell.rs` |
| `src/mcp/types.rs` (SSH-side) | `src/adapters/ssh/internal/types.rs` |
| `src/mcp/error.rs` | `src/adapters/ssh/internal/error.rs` |
| `src/mcp/auth/{traits,password,key,agent,chain}.rs` (`#[async_trait]`) | `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` (native AFIT + enum dispatcher; `async-trait` direct dep dropped) |
| `src/mcp/sftp.rs` | `src/adapters/sftp/internal/sftp.rs` |
| `src/mcp/transfer.rs` | `src/adapters/sftp/internal/transfer.rs` |
| `src/mcp/types.rs` (SFTP-side) | `src/adapters/sftp/internal/types.rs` |
| `src/mcp/config.rs` | `src/adapters/config/internal/mod.rs` |
| `src/mcp/subscription.rs::SUBSCRIPTION_REGISTRY` + `::spawn_peer_gc` | `src/adapters/subscription/legacy.rs::SUBSCRIPTION_REGISTRY` + `::spawn_peer_gc` (transitional, alongside `MemoryRegistry<N>`) |

For in-flight v4.0 patches: `git grep crate::mcp::` will surface every callsite that needs a path rewrite. Adapter-internal modules are private to their owning adapter; use cases never reach in.

The remaining v4.x backlog:

- Migrate the SSH/SFTP runtime adapters off `adapters::subscription::legacy::SUBSCRIPTION_REGISTRY` (current poker) and onto the `MemoryRegistry<N>` port handle, then delete the legacy adapter.
- Cross-adapter SFTP refinements (shared transfer scheduler, per-session SFTP semaphore tuning).

### v4.7 → v4.8 addendum

**v4.8 is strictly additive on `tools/list[].outputSchema` metadata.** No source-level migration steps are required for contributors of in-flight v4.7 patches.

What changed:

- 12 new typed result structs landed in `src/infra/mcp/results.rs` covering the tools that previously emitted free-form `structured_content` (`ssh_disconnect`, `ssh_sessions`, `ssh_disconnect_agent`, `ssh_commands`, `ssh_exec_cancel`, `ssh_shell_write`, `ssh_shell_press`, `ssh_shell_wait_for`, `ssh_shell_close`, `ssh_upload`, `ssh_download`, `ssh_forward`).
- 24 new `output_schema = schema_for_type::<…Result>()` attributes on `#[tool]` macro sites in `src/infra/mcp/tool_router.rs` (12 with `port_forward`, 12 without).
- `SshConnectResult` schema gained 4 additive optional fields (`name`, `replaced`, `matches`, `count`) covering the existing runtime payload variants on `status = "ok"` / `"reused"` / `"suggested"`.
- `SshShellOpenResult` gained `initial_buffer: Option<String>` mirroring the v4.7 `INITIAL_BUFFER:` Markdown line.
- `src/infra/mcp/results.rs` head doc-comment "Coverage" section rewritten to claim 21 / 21 (or 20 / 20 without `port_forward`).

What did NOT change:

- Markdown response body: byte-identical to v4.7.1.
- `structured_content` JSON payload: byte-identical to v4.7.1 on every existing field.
- Env vars, error codes, runtime behaviour, lock-free invariants, channel sizes, port surface: all unchanged.
- Test suite: 1168 lib tests + 2 integration tests pass unchanged.

For external crates implementing one of the 21 typed result structs as a deserialisation target: every struct is `#[non_exhaustive]` so callers cannot match exhaustively across versions. Optional fields use `#[serde(skip_serializing_if = "Option::is_none")]` so absent values are not surfaced as JSON `null` on the wire.

### v4.8 → v4.8.1 addendum

**v4.8.1 is a wire-correctness patch.** No source-level migration steps required.

What changed:

- `ssh_transfer_progress` now reports live `bytes_transferred` mid-flight (was always 0 until the terminal hand-off through v4.8.0). The `transfer://<id>/progress` resource read path is transparently fixed too.
- A new per-transfer `spawn_progress_watcher` task in `src/adapters/sftp/russh_sftp_adapter.rs` consumes the `progress_tx` broadcast and calls `TransferStatusSink::record_progress(...)` (a sink hook present since v4.2 with no producer until now), throttled at 250 ms.
- 1168 -> 1172 lib tests (4 new `progress_watcher_tests` unit tests). New file `scripts/test_transfer_progress.py` adds 2 `requires_sshd` Python integration tests.

What did NOT change:

- `ssh_transfer_progress` Markdown body and `structured_content` shape: byte-identical to v4.8.0 on every field name. Only the *value* of `bytes_transferred` during running snapshots — it now reports real live bytes instead of the stale 0.
- `TransferStatusSink::record_progress`, `RepoTransferStatusSink::record_progress`, `NoopTransferStatusSink::record_progress`, `TransferEntity::with_progress(bytes)`: all already declared since v4.2; this patch simply wires a producer.

**No client-side migration required** — every v3 / v4.x host works against v4.8.1 servers without any change.

---

## v5 → v6

For **MCP host operators, contributors, and downstream automations** moving from any v5.x to v6.0. **First wire-breaking release since v5.0** — only the `tools/list` name strings change. Every other surface (resource URI schemes, push narrative, error taxonomy, structured-content payloads, env vars, MSRV, dependency graph) is byte-identical to v5.3.2.

### Why namespaces split

The v5.x catalogue grew organically: SSH ops, lane administration, and local serial all sat under the `ssh_*` prefix even though `sub_*` is cross-resource (works for `serial://` too) and `serial_*` doesn't run over SSH at all. Smaller LLMs got confused — they'd attempt `ssh_serial_open` over an SSH session_id, or treat `ssh_subscribe` as SSH-specific and skip subscribing to a serial port. v6.0 splits the catalogue across three semantic eixos so the tool name itself encodes the transport.

### Three semantic eixos in v6

- **`ssh_*` (21 tools)** — operations that travel over SSH: `connect / disconnect / disconnect_agent / disconnect_many / run / sessions / exec / exec_batch / exec_output / exec_cancel / commands / shell_open / shell_write / shell_press / shell_read / shell_wait_for / shell_close / upload / download / transfer_progress / forward`.
- **`sub_*` (9 tools)** — subscription / lane management (cross-resource, works for shell / command / transfer / session / forward / serial alike): `sub_open / sub_close / sub_pause / sub_resume / sub_filter / sub_replay / sub_list / sub_stats / sub_stats_all`. Verb-uniform with `open / close / pause / resume / filter / replay / list / stats / stats_all`.
- **`serial_*` (6 tools)** — local UART / TTY / COM (no SSH involved): `serial_open / serial_close / serial_write / serial_press / serial_scan / serial_active`.

### One-shot host-side migration (sed)

Apply this in any host configuration / prompt template / log scrubber that mentions tool names. **Order matters** — the longest / most-specific renames must run first to avoid substring traps (e.g. `ssh_subscribe` is a prefix of `ssh_unsubscribe`):

```sed
s/\bssh_unsubscribe\b/sub_close/g          # do BEFORE ssh_subscribe — substring trap
s/\bssh_subscribe\b/sub_open/g
s/\bssh_sub_/sub_/g
s/\bssh_daemon_stats\b/sub_stats_all/g
s/\bssh_serial_send_key\b/serial_press/g
s/\bssh_serial_list_ports\b/serial_scan/g
s/\bssh_serial_list_open\b/serial_active/g
s/\bssh_serial_/serial_/g
s/\bssh_execute_batch\b/ssh_exec_batch/g   # do BEFORE ssh_execute
s/\bssh_execute\b/ssh_exec/g
s/\bssh_get_command_output\b/ssh_exec_output/g
s/\bssh_cancel_command\b/ssh_exec_cancel/g
s/\bssh_list_commands\b/ssh_commands/g
s/\bssh_list_sessions\b/ssh_sessions/g
s/\bssh_get_transfer_progress\b/ssh_transfer_progress/g
s/\bssh_shell_send_key\b/ssh_shell_press/g
```

### Renames table

| v5.x | v6.0.0 | Eixo |
|---|---|---|
| `ssh_list_sessions` | `ssh_sessions` | ssh |
| `ssh_execute` | `ssh_exec` | ssh |
| `ssh_execute_batch` | `ssh_exec_batch` | ssh |
| `ssh_get_command_output` | `ssh_exec_output` | ssh |
| `ssh_cancel_command` | `ssh_exec_cancel` | ssh |
| `ssh_list_commands` | `ssh_commands` | ssh |
| `ssh_shell_send_key` | `ssh_shell_press` | ssh |
| `ssh_get_transfer_progress` | `ssh_transfer_progress` | ssh |
| `ssh_subscribe` | `sub_open` | sub |
| `ssh_unsubscribe` | `sub_close` | sub |
| `ssh_sub_pause/resume/filter/replay/list/stats` | `sub_pause/resume/filter/replay/list/stats` | sub |
| `ssh_daemon_stats` | `sub_stats_all` | sub |
| `ssh_serial_open` | `serial_open` | serial |
| `ssh_serial_close` | `serial_close` | serial |
| `ssh_serial_write` | `serial_write` | serial |
| `ssh_serial_send_key` | `serial_press` | serial |
| `ssh_serial_list_ports` | `serial_scan` | serial |
| `ssh_serial_list_open` | `serial_active` | serial |

### What does NOT change

- Resource URI schemes: `shell://`, `command://`, `transfer://`, `session://`, `forward://`, `serial://`.
- Push narrative: `notifications/resources/updated` + `resources/read?cursor=auto`.
- HINT severities + 38-code error taxonomy.
- Tool descriptions / `When/Push/Cleanup/Cost/Idempotency/Hygiene` blocks (cross-references update to new names).
- `_meta.idempotency_key` semantics + cache TTL.
- 33+ env vars (`SSH_NOTIFY_*`, `SSH_LANE_*`, `SSH_GRACE_*`, etc.).
- All v5.3 lane fanout, port-forward listener, and lifecycle-cascade-close behaviours carry forward unchanged.

---

## v4 → v5

For **MCP host operators, contributors, and downstream automations** moving from ssh-mcp v4.8.x to v5.0.x. Wire compatibility, additive surface, default-behaviour deltas, and recipes for the workflows that change shape under v5.

If you only consume the v4 MCP surface and never opt into the new tools, env vars, or `release_when_no_subs` flag, no host-side change is required. v5 is wire-compatible with v4 on every legacy path. The expansions are additive.

> **Status.** v5.x is shipped on `master`. Phases 1 / 2 / 3 / 4 are merged; the v5.2 release added the 6 serial / UART / TTY / COM tools (ADR 0009). v6.0.0 builds on v5.3.2 and only renames tool strings (`ssh_*` / `sub_*` / `serial_*` eixos) — see [v5 → v6](#v5--v6). All v4 → v5 surfaces below are wire-compatible.

### Wire compatibility summary

| Surface | v4.8.x | v5.0.x | Compat? |
|---|---|---|---|
| Tool catalogue (legacy 21 tools — 20 without `port_forward`) | 21 | 21 carried over + 9 new (additive) | yes |
| Tool response markdown shape (`KEY: value`, 8-hex nonce, `--- name [nonce] ---`) | block-only | identical | yes |
| Structured `_meta` channel on every tool response | typed JSON | identical (extended for new tools) | yes |
| Resources schemes | 5 | identical (no new schemes in v5.0) | yes |
| `notifications/resources/updated` debounce semantics (`SSH_NOTIFY_*`) | 200 ms / 1 s / 30 s | bumped debounce default from 50 → 200 ms (other timers identical) | partial — debounce default changed |
| `notifications/cancelled` (rmcp 1.6 native) | yes | yes | yes |
| `prompts/list` + `prompts/get` | 5 prompts | 10 prompts (5 new) | yes — additive |
| `resources/templates/list` | 4 / 5 templates | identical (no new templates in v5.0) | yes |
| Capability handshake | yes | identical | yes |
| Error wire envelope (`SSH_X: ERROR\nREASON: [CODE] description\nDETAIL: ...`) | yes | identical (codes added; format unchanged) | yes |
| Idempotency (`_meta.idempotency_key`) | 15 mutating tools | 15 carried over + 8 of the 9 new tools (the read-only `sub_list` / `sub_stats` / `sub_stats_all` are pure reads) | yes |
| Cursor key on resource subscriptions | `(PeerId, Uri)` | `(SubId, Uri)` internally; `(PeerId, Uri)` synthesised for legacy hosts | yes — synthesised |
| HTTP transport bind / path defaults | `0.0.0.0:8000` `/` | identical | yes |
| Stdio transport | identical | identical | yes |

**Net result.** A v4 host pointed at a v5 server gets the same wire bytes on every legacy tool and resource. v5 ships nine new tools and a second binary (`ssh-mcp-tail`) that the v4 host can simply ignore.

### Breaking changes

There is **no breaking change on the wire** between v4.8 and v5.0. There are zero tool removals, zero schema-narrowing edits, and zero behaviour changes for any unmodified host.

The deltas are introduced as new defaults or new env vars — never as forced behaviour changes:

- New optional argument `release_when_no_subs: bool` on `ssh_shell_open`, `ssh_exec`, `ssh_upload`, `ssh_download` (default: `false` to match v4 semantics).
- New optional argument `lifetime: Lifetime` and `lag_policy: LagPolicy` on `sub_open` (the new tool — see [ADR 0004](./adr/0004-channel-mux-fairness.md)).
- New optional `filter` (regex / level) argument on `sub_open`.
- New env vars per [ADR 0003](./adr/0003-lifecycle-binding.md), [ADR 0006](./adr/0006-backpressure-policies.md), and [ADR 0008](./adr/0008-ndjson-daemon-protocol.md).

If your host parses the wire format byte-for-byte (snapshot tests, audit pipelines), no replacement test fixture is required — every legacy assertion still holds.

### Additive surface

v5.0 adds nine net-new MCP tools and one second binary (`ssh-mcp-tail`). All are additive — older hosts that ignore them continue to work.

#### Nine new tools (Phase 3)

The tool catalogue grows from 21 to 30 (or from 20 to 29 without `port_forward`) at the end of Phase 3; v5.2 then layers 6 serial tools on top, taking the v5.2+ surface to 36 / 35. All nine Phase 3 tools are subscription-management primitives that key on the new `SubId` (UUIDv7 per `resources/subscribe` or `sub_open` call) introduced by [ADR 0004](./adr/0004-channel-mux-fairness.md).

| Tool | Purpose | Returns | Idempotency |
|---|---|---|---|
| `sub_open` | Open a push channel against a `shell://` / `command://` / `transfer://` / `session://` / `forward://` URI. Accepts `lifetime`, `lag_policy`, `filter`. | `sub_id` | yes |
| `sub_close` | Close a push channel by `sub_id`. Triggers grace timer if last subscriber and `release_when_no_subs = true`. | OK / NOT_FOUND | yes |
| `sub_pause` | Suspend the lane's drain loop. Producer keeps emitting; mpsc fills under the lane's lag policy. | OK | yes |
| `sub_resume` | Resume the drain loop. | OK | yes |
| `sub_filter` | Hot-reload the lane's filter regex / level. | OK | yes |
| `sub_replay` | Re-emit events from a chosen cursor (within the ring buffer window). | event count | no |
| `sub_list` | Enumerate active sub_ids with summary stats. | array of `{sub_id, uri, queue_depth, lag_policy}` | n/a (read-only) |
| `sub_stats` | Per-sub_id counter snapshot (events_sent, lag_drops, queue_depth, ...). | typed `SubscriberStats` | n/a |
| `sub_stats_all` | Global stats aggregating across all sub_ids (active sessions, total subs, mux backlog, peer GC pace, ...). | typed `DaemonStats` | n/a |

Every new tool emits the same dual channel as the v4 tools: a markdown body with `KEY: value` lines and an 8-hex-char nonce framing block, plus a parallel `structured_content` JSON object.

#### New binary: `ssh-mcp-tail` (Phase 4)

`ssh-mcp-tail` is a single binary with three subcommands (`run`, `shell`, `daemon`). Its primary mode (`daemon`) reads NDJSON commands on stdin and emits NDJSON events on stdout. It embeds the same `composition::prod` adapters used by `ssh-mcp` and `ssh-mcp-stdio`, wired to itself via an in-process `tokio::io::duplex` MCP transport.

The binary exists for hosts that **do not** surface `notifications/resources/updated` to the LLM (Claude Code CLI as of 2026-Q1, and several IDE integrations). Driving it from such a host gives the LLM real push delivery without any host-level subscribe support.

The full reference is at [DAEMON.md](./DAEMON.md).

#### New env vars

Defaults preserve v4 behaviour. The new env vars are listed exhaustively in [CONFIGURATION.md](./CONFIGURATION.md). Highlights:

- `SSH_LAG_POLICY_DEFAULT` (default `snapshot`) — lane LagPolicy for subscribers that do not specify.
- `SSH_LANE_BUFFER` (default 1024) — per-lane mpsc capacity.
- `SSH_MUX_BUFFER` (default 8192) — global mux mpsc capacity.
- `SSH_MAX_SUBS_PER_URI` (default 16), `SSH_MAX_SUBS_TOTAL` (default 1024) — subscriber caps.
- `SSH_BP_BLOCK_TIMEOUT_MS` (default 5000) — `BlockSlow` escape hatch.
- `SSH_FILTER_REGEX_MAX` (default 1024 chars) — filter regex length cap.
- `SSH_REPLAY_WINDOW_BYTES` (default 1 MiB) — default `sub_replay` window.
- `SSH_SUB_LEAK_RISK_WARN_S` (default 2) — `SUB_LEAK_RISK` watcher scan period.
- `SSH_SUB_LEAK_RISK_KILL_S` (default 0 = off) — operator-opt-in hard kill threshold.
- Daemon-only: `SSH_NDJSON_LINE_MAX`, `SSH_HEARTBEAT_INTERVAL_S`, `SSH_DAEMON_STATS_INTERVAL_S`, `SSH_GRACE_HARD_TIMEOUT_S`, `SSH_NDJSON_PRETTY` — see [DAEMON.md](./DAEMON.md).

> **Lifecycle grace windows** are programmatic per `release_when_no_subs` call (per-resource grace held on `LifecyclePolicy.grace_ms`); v5/v6 do **not** ship `SSH_LIFECYCLE_*` / `SSH_SESSION_IDLE_GRACE_MS` env vars. Future ADRs may add env-var overrides.
- `SSH_NDJSON_LINE_MAX` (default 1 MB) — daemon stdin line size limit.
- `SSH_HEARTBEAT_INTERVAL_S` (default 30) — daemon heartbeat cadence.
- `SSH_DAEMON_STATS_INTERVAL_S` (default 60) — daemon stats auto-emit cadence.
- `SSH_GRACE_HARD_TIMEOUT_S` (default 30) — daemon graceful shutdown deadline.

### Default-behaviour deltas

The following defaults change between v4.8 and v5.0. None affect a host that does not opt into the new flag or env var; v4 idioms are preserved.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    subgraph On["Always-on (transparent)"]
        T1["(SubId, Uri) cursor key<br/>(legacy hosts get<br/>synthesised sub_id)"]
        T2["per-lane mpsc<br/>(Snapshot default)"]
        T3["refcount-aware<br/>session reaper"]
        T4["WARN: SUB_LEAK_RISK<br/>(emitted on resource list)"]
    end

    subgraph Opt["Opt-in (per-call)"]
        O1["release_when_no_subs<br/>= true<br/>(default false)"]
        O2["lifetime=auto-close<br/>or =lease<br/>(default manual)"]
        O3["lag_policy=block_slow<br/>(default snapshot)"]
        O4["ssh-mcp-tail daemon<br/>(separate binary)"]
    end

    classDef on fill:#238636,color:#f0f6fc,stroke:#2ea043
    classDef opt fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    class T1,T2,T3,T4 on
    class O1,O2,O3,O4 opt
```

| Behaviour | v4.8 | v5.0 default | Opt-in flag |
|---|---|---|---|
| Resource auto-cleanup when no subscriber | n/a (manual close required) | unchanged for v4 idioms (`release_when_no_subs = false`) | `release_when_no_subs: true` per call |
| Cursor key on resource subscriptions | `(PeerId, Uri)` | `(SubId, Uri)` internally; legacy hosts get a synthesised `sub_id` per `(PeerId, Uri)` pair | always on (transparent) |
| Lane backpressure policy | one global broadcast channel; `RecvError::Lagged` triggers manual snapshot rebuild | per-lane mpsc with `Snapshot` default | `lag_policy` per `sub_open` call |
| Peer GC interval | 30 s | 30 s (`SSH_MCP_PEER_GC_INTERVAL_S`) | n/a |
| Session-level reaper | inactivity TTL only | refcount-aware (active_refs supersedes TTL) | always on |
| Inactivity TTL on shell | unchanged (`SSH_SHELL_INACTIVITY_TTL_SECS`) | unchanged | n/a |
| Shutdown sequence | abrupt for stdio; HTTP graceful via axum | NDJSON daemon adds explicit drain (`SSH_GRACE_HARD_TIMEOUT_S`) | `daemon` subcommand only |
| Auto-warning for leak risk | none | `WARN: SUB_LEAK_RISK` line on next `ssh_sessions` / `ssh_commands` call referencing the resource | always on (Phase 3 merged) |

The `release_when_no_subs = false` default means v5 hosts that do **not** add the flag inherit v4 leak semantics: a long-running shell persists until manually closed (or until the inactivity TTL fires). This is intentional. v6.0 will flip the default to `true`; v5 ships the flag wired but defaulted off so that hosts upgrade their prompts and idempotency strategy first.

### Recipes (before / after)

The recipes below show the same workflow under v4.8 and under v5.0 push-first. Both are valid in v5.0 — the v4 path remains supported. The v5 path is recommended once your host's prompt and the LLM tooling expose `sub_open`.

#### Open a shell + drain push (Claude Desktop, full-spec host)

**v4.8 — wait + read polling fallback**

```text
ssh_connect(address, username)
  -> SESSION_ID
ssh_shell_open(session_id, cols=80, rows=24)
  -> SHELL_ID
ssh_shell_read(shell_id, wait=true, wait_timeout_secs=30, min_bytes=1)
  # repeat until done; manually close
ssh_shell_close(shell_id)
ssh_disconnect(session_id)
```

**v5.0 — push-first**

```text
ssh_connect(address, username, agent_id="my-claude-agent")
  -> SESSION_ID
ssh_shell_open(session_id, cols=80, rows=24, release_when_no_subs=true)
  -> SHELL_ID  (returns INITIAL_BUFFER if the prompt arrives within 100 ms)
sub_open(uri="shell://<SHELL_ID>/output", lifetime="auto-close", lag_policy="snapshot")
  -> SUB_ID
# drive the shell; drain push events as they arrive
ssh_shell_write(shell_id, bytes="ls -la\n")
# ... events drain via notifications/resources/updated ...
sub_close(sub_id)            # release_when_no_subs triggers grace timer
# shell auto-closes after the per-call lifecycle grace window expires
ssh_disconnect_agent(agent_id="my-claude-agent")
```

#### Run a long command + sub + drain until completed

**v4.8 — wait fallback**

```text
ssh_connect -> SESSION_ID
ssh_exec(session_id, command="run-long-job") -> COMMAND_ID
ssh_exec_output(command_id, wait=true, wait_timeout_secs=300)
  # blocks until exit or timeout; one tool call burns one round trip
ssh_disconnect(session_id)
```

**v5.0 — push-first with auto-cleanup**

```text
ssh_connect -> SESSION_ID
ssh_exec(session_id, command="run-long-job", release_when_no_subs=true) -> COMMAND_ID
sub_open(uri="command://<COMMAND_ID>/output",
              lifetime="auto-close",
              lag_policy="snapshot")
  -> SUB_ID
# drain events until { ev: "completed", exit: <int> } arrives
# resource auto-releases (Owned -> Releasing -> Closed) after grace timer
sub_close(sub_id)
ssh_disconnect(session_id)
```

#### Upload a file + sub progress

**v4.8 — poll**

```text
ssh_upload(session_id, local="/tmp/file", remote="/srv/file") -> TRANSFER_ID
ssh_transfer_progress(transfer_id, wait=true, wait_timeout_secs=300)
  # blocks until completion
```

**v5.0 — push-first**

```text
ssh_upload(session_id, local="/tmp/file", remote="/srv/file",
           release_when_no_subs=true) -> TRANSFER_ID
sub_open(uri="transfer://<TRANSFER_ID>/progress",
              lifetime="auto-close",
              lag_policy="snapshot")
  -> SUB_ID
# drain { ev: "transfer_progress", bytes: ..., total: ... } events
sub_close(sub_id)
```

#### Audit my owned subscriptions (v5.0 only)

```text
sub_list(filter_by_uri="shell://*")
  -> [{sub_id, uri, queue_depth, lag_policy, lagged_drops}, ...]
# decide which are stale, then:
sub_close(sub_id)
```

A `subscription_hygiene_audit` prompt published via `prompts/list` automates this loop. See [LLM_GUIDE.md → Prompts catalogue](./LLM_GUIDE.md#prompts-catalogue).

#### Replay after disconnect (v5.0 only)

```text
# after a network blip, reconnect:
ssh_connect(...) -> SESSION_ID
# the prior shell/command is still alive (refcount > 0 because the
# resource was created with release_when_no_subs=false OR the grace
# window has not elapsed):
sub_open(uri="shell://<SHELL_ID>/output", lifetime="auto-close")
  -> SUB_ID
# the lane initialises with lag_policy=snapshot; the first event is a
# `{ ev: "snapshot", cursor: N, delta: <bytes> }` with the live ring
# buffer contents from cursor 0 (or `last_seen_cursor` if you provided it).
sub_replay(sub_id, from_cursor=last_seen)
  # for explicit replay outside the snapshot rebuild
```

If the resource has `Closed` in the meantime (grace timer fired), `sub_open` returns `RESOURCE_GONE` with a `DETAIL: Resource closed (lifecycle Releasing/Closed); recreate via ssh_shell_open / ssh_exec / ssh_upload.` line. See [LLM_GUIDE.md → Error handbook](./LLM_GUIDE.md#error-handbook) for the full code-by-code retry policy.

#### Daemon-mode equivalent (Claude Code CLI, no-subscribe host)

When the host's LLM cannot consume `notifications/resources/updated`, a Claude Code shell can pipe NDJSON through `ssh-mcp-tail`:

```bash
ssh-mcp-tail run --host vm.example.com --user root -- "tail -f /var/log/app.log" \
  | jq 'select(.ev == "push") | .delta'
```

The daemon enforces the same lifecycle and lag policy as the in-process server. See [DAEMON.md](./DAEMON.md) for the full op + event schema and pipeline recipes.

### LLM prompt updates

If you embed `Implementation.instructions` in your host's system prompt or fine-tune a model on the v4 surface, refresh your prompt with the v5 root text. The canonical sources are [LLM_GUIDE.md → Root prompt 27B](./LLM_GUIDE.md#root-prompt--27b-class-models) and [LLM_GUIDE.md → Root prompt 70B](./LLM_GUIDE.md#root-prompt--70b-class-models).

The five golden rules ([LLM_GUIDE.md → Golden rules](./LLM_GUIDE.md#golden-rules)) are subscribe-first, always unsubscribe, watch lag_drops, cleanup on error, never hot-poll. If your fine-tuning recipe references rules by number, treat that section as the definitive list.

The 10 prompts published via `prompts/list` change shape: 5 v4 carryovers (`run_one_shot_command`, `investigate_session`, `upload_and_verify`, `interactive_shell_drive`, `cleanup_agent`) plus 5 v5 additions (`push_first_long_command`, `push_first_interactive_shell`, `push_first_file_transfer`, `subscription_hygiene_audit`, `chaos_resume_after_disconnect`). The catalog is documented at [LLM_GUIDE.md → Prompts catalogue](./LLM_GUIDE.md#prompts-catalogue).

The 10 documented anti-patterns (hot-poll, leak-on-error, lag-blindness, ...) live in [LLM_GUIDE.md → Anti-patterns](./LLM_GUIDE.md#anti-patterns). Use this when the LLM produces a workflow that compiles but leaks.

### Deprecation timeline

| Version | Status | Notes |
|---|---|---|
| **v5.0** | Nothing deprecated. | The legacy `(PeerId, Uri)` cursor key is kept; it is synthesised internally so v4 hosts work unchanged. The v4 `resources/subscribe` flow auto-mints a `sub_id`. The v4 tools, idempotency cache, debouncer defaults, and HTTP/stdio binaries are all preserved with identical semantics. |
| **v5.x** (minor releases) | Legacy `(PeerId, Uri)` cursor key remains supported. | New tools may add optional fields; existing fields keep their semantics. The default lag policy stays `snapshot`. |
| **v6.0** (future, no date) | `release_when_no_subs = true` may become default. | Once empirical data from v5.x confirms the leak rate falls under the auto-cleanup default, v6.0 may flip the flag. The v5 default (`false`) is intentionally conservative so existing hosts inherit v4 behaviour. v6.0 will publish a separate migration guide if the default changes. |

No v4 idiom is forbidden in v5.0. No tool, env var, or wire format is removed. Hosts that never opt into the new surface should not need to update code.

### References

The 8 ADRs at [adr/](./adr/) are the canonical source for every design decision behind v5.0:

- [ADR 0003 — Lifecycle Binding](./adr/0003-lifecycle-binding.md) — refcount + grace-timer state machine, `release_when_no_subs` flag.
- [ADR 0004 — Channel Mux + SubId](./adr/0004-channel-mux-fairness.md) — `(SubId, Uri)` cursor key, per-lane mpsc fan-out.
- [ADR 0005 — LLM UX Priorities](./adr/0005-llm-ux-priorities.md) — layered escalation surface, prompt catalog growth, `SUB_LEAK_RISK` warning.
- [ADR 0006 — Backpressure Policies](./adr/0006-backpressure-policies.md) — four `LagPolicy` variants, per-frontier failure mode matrix, `BlockSlow` timeout escape hatch.
- [ADR 0007 — Error Taxonomy](./adr/0007-error-taxonomy.md) — 7 categories, new codes (`RESOURCE_GONE`, `SUB_NOT_FOUND`, `LAG_*`, `INVALID_OP`, ...), canonical `DETAIL` phrasings.
- [ADR 0008 — NDJSON Daemon Protocol](./adr/0008-ndjson-daemon-protocol.md) — `ssh-mcp-tail` op + event schema, in-process duplex transport, graceful shutdown.

Operational follow-on:

- [DAEMON.md](./DAEMON.md) — daemon NDJSON reference.
- [OPERATIONS.md](./OPERATIONS.md) — symptom-driven diagnostic guide.
- [LLM_GUIDE.md](./LLM_GUIDE.md) — golden rules, prompts, anti-patterns, error handbook, 27B / 70B root prompts.
- [ARCHITECTURE.md](./ARCHITECTURE.md) — full hexagonal layout.
- [DEVELOPMENT.md](./DEVELOPMENT.md) — lock-free invariants enforced by Clippy.
- [RESOURCES.md](./RESOURCES.md) — resource scheme contract.
- [CONFIGURATION.md](./CONFIGURATION.md) — full env var table.

## v6.1 → v7.0

For **MCP host operators, contributors, and downstream automations** moving from v6.1 to v7.0. **Wire-additive on the MCP surface** — the existing 36 tool catalogue is unchanged. Three new tools (`ssh_rsync`, `ssh_rsync_cancel`, `ssh_rsync_stats`) and a new `rsync://<id>/progress` resource scheme go live in v7.0. Two integrated transports ship in-process inside the host crate: `WireRsyncTransport` (canonical port of OpenBSD `openrsync` speaking rsync wire protocol v31 against a remote `rsync --server`) and `SftpRsyncTransport` (universal SFTP fallback). Both are live for the supported feature set; push and pull both byte-identical against `rsync 3.2.7` on a real Linux VM. Reference: [ADR 0011 — rsync hybrid transport](./adr/0011-rsync-hybrid-transport.md).

### v7.0.0 — final

Final state shipped in v7.0.0 (2026-05-06). The original v7.0 plan (cross-compiled deployed-agent transport with sha256 verification) was retracted in v7.0.0-alpha.2 in favour of "tudo integrado". The slice-by-slice narrative below the [v7.0.0-alpha.4](#v700-alpha4--sftp-transport-live) section is kept for traceability — they document the incremental landings; this section is the canonical migration contract.

> **Post-release fixes (2026-05-06).** Five follow-up commits closed the gap between the v7.0.0 transport bodies and the live MCP-host call path. Hosts using the wire transport via the MCP surface (`ssh_rsync transport=Wire` or `transport=Auto` routing to Wire) need binaries built from [`81e3573`](https://github.com/farchanjo/ssh-mcp/commit/81e3573) or later — earlier v7.0.0 binaries surface `RSYNC_PROTOCOL_ERROR` "Wire transport (rsync v31 wire-compat) is being implemented" on every call. See [Post-release fixes](#v700--post-release-fixes-2026-05-06) below for the per-bug breakdown.

#### v7.0.0 — post-release fixes (2026-05-06)

Wire-additive — no DTO field shapes changed, no error codes added, no env var defaults moved. After the four fixes below, the Python integration suite (`scripts/test_v7_rsync_{http,stdio,vm}.py`) reports **19 passed + 2 xfailed**, and `ssh_rsync transport=Wire` against a live `rsync 3.2.7` on `vm.services` produces byte-identical sha256 across the 3-file fixture (`a.txt`, `b.txt`, `nested/c.txt`).

| # | Bug | Commit | Symptom on v7.0.0 binaries before the fix |
|---|---|---|---|
| 1 | Composition root reached the no-registry stub. `composition::prod` instantiated `WireRsyncTransport::new()` instead of `with_registry(sftp.handle_registry().clone())`. | [`bfc68c6`](https://github.com/farchanjo/ssh-mcp/commit/bfc68c6) | Every `transport=Wire` (and every `transport=Auto` route picking Wire) returned `RSYNC_PROTOCOL_ERROR` "Wire transport (rsync v31 wire-compat) is being implemented". |
| 2 | `RsyncSyncUseCase::execute()` never spawned a task to fold `RsyncProgressEvent` into the `RsyncSession` aggregate. | [`4fb0b35`](https://github.com/farchanjo/ssh-mcp/commit/4fb0b35) | `ssh_rsync_stats` reported `STATUS: pending` indefinitely; the lane closed without a terminal status flip. |
| 3 | `opts.dry_run` / `opts.exclude` / `opts.include` rode `SshRsyncArgs.opts` correctly but were not threaded through `RsyncSyncRequest` → `RsyncStartRequest` to the per-call adapter merge. | [`4fb0b35`](https://github.com/farchanjo/ssh-mcp/commit/4fb0b35) | The SFTP walker received `dry_run=false` + empty exclude/include; every "preview" call wrote real bytes; every "skip `.git/`" call uploaded the directory. |
| 4 | Wire transport shipped the full `host:path` spec into `build_rsync_server_cmdline`. The remote `rsync --server` interprets `host:path` as a path, not a directive. | [`81e3573`](https://github.com/farchanjo/ssh-mcp/commit/81e3573) | Server aborted with fatal mplex `MSG_IO_ERROR`; `0 files transferred`; lane terminated with `SessionFailed`. |

**Action required:** rebuild from [`81e3573`](https://github.com/farchanjo/ssh-mcp/commit/81e3573) or later. No host-side DTO change, no env var change, no migration step beyond rebuild + redeploy.

A fifth issue (Bug 3 in the Python suite numbering) remains a documented architectural limitation rather than a regression — the SFTP transport walks both `src` and `dst` through a single `RsyncSftpFsPort`, so end-to-end push from a local source onto a remote dst needs a local-FS adapter that lands in a follow-up slice. Today the e2e-vm test pre-creates the source on the remote side; the two related Python tests (`test_rsync_vm_sftp_push` / `test_rsync_vm_sftp_pull`) carry `@pytest.mark.xfail(strict=False)` and auto-pass when the bridge ships.

#### What's new

- **In-process Wire + SFTP transports.** Both run inside the host binary. `transport=Auto` (default) probes the remote and prefers Wire when rsync >= 3.2.0 is installed, otherwise routes to SFTP. `transport=Wire` forces the wire-compat client (returns `RSYNC_VERSION_TOO_OLD` if the remote rsync is missing or older). `transport=Sftp` skips the probe and uses the universal SFTP fallback.
- **3 new MCP tools.** `ssh_rsync` returns `STARTED` immediately + `RSYNC_ID`; `ssh_rsync_stats` reads the live aggregate; `ssh_rsync_cancel` is idempotent. Block-markdown wire shape with the same `KEY: value` + 8-hex-char nonce conventions as the v5/v6 carry-over surface.
- **1 new push scheme.** `rsync://<RSYNC_ID>/progress` carries `application/json` events: `SessionStarted`, `FileStarted`, `FileProgress`, `FileCompleted`, `FileSkipped { reason: SizeMatch | MtimeMatch | DryRun }`, `FileFailed`, `SyncProgress`, `SyncCompleted`, `SessionFailed`. Default lag policy `Snapshot`; ADR 0006 byte-threshold flush (default 64 KiB) hooks the lane.
- **`ResourceKind::Rsync`** variant on both `adapters::subscription::legacy` and `ports::subscriber_registry` enums. URIs of shape `rsync://<id>/progress` parse + format through the registry helpers.
- **6 new error codes** — `RSYNC_NOT_FOUND` (RESOURCE), `RSYNC_VERSION_TOO_OLD` (POLICY), `RSYNC_PROTOCOL_ERROR` (TRANSPORT), `RSYNC_FILE_LIST_TOO_LARGE` (POLICY), `RSYNC_PARTIAL_TRANSFER` (TRANSPORT), `SFTP_FEATURE_MISSING` (POLICY). Total error-taxonomy size grows from 40 to 46. See [ADR 0007](./adr/0007-error-taxonomy.md) and [LLM_GUIDE.md → Error handbook](./LLM_GUIDE.md#error-handbook).
- **3 new env vars** — `SSH_RSYNC_PROBE_TIMEOUT_MS` (default `2000`), `SSH_RSYNC_BLOCK_SIZE` (default auto), `SSH_RSYNC_FILE_LIST_LIMIT` (default `1_000_000`). The agent-cache env vars from the original plan (`SSH_RSYNC_AGENT_CACHE_TTL_DAYS`, `SSH_RSYNC_AGENT_CACHE_DIR`) were dropped during the v7.0.0-alpha.2 retrenchment.

#### DTO surface (`src/infra/mcp/args/rsync.rs`)

```rust
pub struct SshRsyncArgs {
    pub session_id: String,
    pub src: String,
    pub dst: String,
    #[serde(default)] pub opts: RsyncOptsArg,
    #[serde(default)] pub transport: RsyncTransportArg,        // Auto | Wire | Sftp
    pub release_when_no_subs: Option<bool>,                    // ADR 0003 binding
}

pub struct RsyncOptsArg {
    pub recursive: bool,
    pub archive: bool,                  // -a (alias for -rlptgoD)
    pub delete: bool,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub dry_run: bool,
    pub bwlimit_kbps: Option<u64>,
    pub compress: bool,                 // wire-only
    pub partial: bool,                  // ADR 0010 resume semantics
    pub verify_checksum: bool,          // wire-only -> SFTP_FEATURE_MISSING on Sftp
    pub preserve: PreserveFlagsArg,
}

pub struct PreserveFlagsArg {
    pub perms: bool,        // -p (default true)
    pub mtime: bool,        // -t (default true)
    pub owner: bool,        // -o (default true; root only on remote)
    pub group: bool,        // -g (default true)
    pub links: bool,        // -l (default true)
    pub hardlinks: bool,    // -H (default false; wire-only -> SFTP_FEATURE_MISSING on Sftp)
    pub sparse: bool,       // -S (default false)
    pub devices: bool,      // -D (default false; root only)
}

pub enum RsyncTransportArg { Auto, Wire, Sftp }
pub struct SshRsyncCancelArgs { pub rsync_id: String }
pub struct SshRsyncStatsArgs  { pub rsync_id: String }
```

#### What does NOT change

- Tool catalogue: still 36 tools (35 without `port_forward`) for the v6.x carry-over surface. Three new tools land on top.
- Resource URI schemes for v6.x: byte-identical — `shell://`, `command://`, `transfer://`, `session://`, `forward://`, `serial://` all unchanged.
- Default behaviour: every existing knob (`release_when_no_subs`, lag policy, debouncer windows, broadcast caps, resume / verify) keeps its v6.1 value.
- MSRV: still Rust 1.95.

#### Recommended upgrade path for hosts that want to opt into rsync

1. **Bump dependency** — `ssh-mcp = "7.0"` in your host (or rebuild from source). No code change required to keep v6.1 behaviour.
2. **Try the SFTP path against any host** — `ssh_rsync(session_id, src, dst, opts={ recursive: true })` with `transport=Auto` falls back to SFTP when the remote has no rsync. Works against every host where `ssh_upload` already works.
3. **Use the Wire path for delta-sync** — install `rsync >= 3.2.0` on the remote and let `transport=Auto` pick it. Block-match path collapses unchanged blocks; `bytes_skipped` accumulates the wire-saved bytes.
4. **Subscribe to progress** — `sub_open uri=rsync://<RSYNC_ID>/progress` immediately after `ssh_rsync` returns. The lane streams per-file + aggregate events; `SyncCompleted` is the terminal frame.
5. **Cancel + retry with idempotency** — pass `_meta.idempotency_key` on the retry path. `ssh_rsync_cancel rsync_id=<id>` is idempotent; combining with `opts.partial=true` resumes from the deterministic tempfile.
6. **Branch on the new error codes** — `SFTP_FEATURE_MISSING` for Sftp + hardlinks / verify_checksum / unsupported setstat / unsupported symlink; `RSYNC_VERSION_TOO_OLD` for forced Wire on a host without rsync >= 3.2.0; `RSYNC_PARTIAL_TRANSFER` for mid-flight session loss (re-run with `partial=true`).

#### Deferred (out of v7.0 scope)

- `-c` checksum delta over wire-format extension — surfaces as `SFTP_FEATURE_MISSING` on both transports. Successor ADR.
- `-H` hardlinks — same.
- Encoder symmetry for owner/group bytes when `-o`/`-g` negotiated — partial; decode honoured, encode skips bytes the strict round-trip would expect.
- `chown` syscall wiring on the receiver (root-only path).
- `-D` devices, `-X` xattrs, `-A` ACLs — out of scope for v7.x.

### What was retracted in v7.0.0-alpha.2

The original v7.0 plan shipped a deployed-agent transport (linux-x86_64 binary cross-compiled in CI, embedded via `include_bytes!`, SFTP-uploaded on first run with sha256 verification). v7.0.0-alpha.2 retracts that path entirely in favour of "tudo integrado" — both transports live inside the host binary. Concrete deletions:

- The `crates/ssh-mcp-rsync-agent/` sub-crate (the deployed binary) is gone.
- The `crates/ssh-mcp-rsync-proto/` sub-crate is gone; the workspace is single-package again. Surviving value-object types (`FileKind`, `PreserveFlags`, `RsyncStats`, `RsyncProgressEvent`, `RsyncTransportKind`, `SkipReason`) moved to `src/adapters/rsync/types.rs` (with `RsyncStats` re-homed in `src/domain/rsync.rs` so domain rules stay clean). Wire-format types (`RsyncOp`, `RsyncOpPayload`, frame codec, op-code constants) had a single producer / single consumer and were dropped along with the agent.
- `RsyncAgentDeployPort` + `EmbeddedAgentDeployAdapter` + `AgentRsyncTransport` + `SshShellExecAdapter` are gone. The use-case generic `RsyncSyncUseCase<T, D, R, SR, Idg, Cfg, Sh>` simplifies to `RsyncSyncUseCase<W, Sf, R, SR, Ssh, Idg, Cfg>` where `W: RsyncTransportPort` is the wire-compat transport and `Sf: RsyncTransportPort` is the SFTP fallback.
- `transport=Agent` is no longer accepted; the inbound `RsyncTransportArg` enum is now `Auto | Wire | Sftp`.
- `agent_path` request override is no longer accepted (no agent to override).
- Cargo features `embed-linux-x86_64`, `embed-linux-aarch64`, `embed-macos-aarch64`, `embed-all` are all gone.
- 4 error codes removed: `AGENT_DEPLOY_FAILED`, `AGENT_ARCH_UNSUPPORTED`, `AGENT_TRUST_VIOLATION`, `AGENT_NOEXEC_TARGET`. 1 new code added: `SFTP_FEATURE_MISSING` (returned when the SFTP path is asked to deliver a Wire-only feature like hardlinks or delta-sync).

### Pre-existing bug fix

v6.1 `cargo build --no-default-features` failed with E0004 — `ResourceKind::Serial` was missing from one exhaustive match (`src/application/read_resource.rs`). v7.0 fixes the bug. Operators who only build `--all-features` see no functional difference; operators tracking the `port_forward = false` profile gain a clean build.

> The slice-by-slice sub-sections below this point document the incremental landings (alpha.4 → slice 10) that produced the v7.0.0 final state. They are kept for traceability; the canonical migration contract is [v7.0.0 — final](#v700--final) above.

### v7.0.0-alpha.4 — SFTP transport live

The SFTP transport body landed in v7.0.0-alpha.4. `transport=Sftp` now drives a recursive mirror through `RusshRsyncSftpFs` for the supported subset (recursive walk, dry-run, `--delete`, exclude/include patterns, attribute preservation gated on a [`SftpFeatures`](../src/adapters/rsync/sftp/probe.rs) snapshot). The use case caches the probe result in a per-session `DashMap<SessionId, SftpFeatures>` so repeated `ssh_rsync` calls against the same SSH session pay the probe round-trips only once.

**Capability gates** — see [API.md → SFTP capability gates](./API.md#sftp-capability-gates) for the full table. The gates fire before the transport's `start_session` call, so the lane never observes a half-failed run when the server lacks setstat / symlink support.

**Running the e2e VM test** — alpha.4 ships an opt-in end-to-end test (`tests/v7_rsync_e2e_vm.rs`) that drives the live transport against a real Linux host. Default-off; not part of the canonical CI gate. Bring your own VM:

```bash
SSH_MCP_E2E_HOST=vm.services \
SSH_MCP_E2E_USER=root \
SSH_MCP_E2E_KEY_PATH=~/.ssh/id_rsa \
cargo test --features e2e-vm v7_rsync_e2e_vm -- --ignored --nocapture
```

Defaults to `root@vm.services` with `~/.ssh/id_rsa`. The test pre-creates a synthetic source tree on the remote via `ssh_exec` (10 files, 2 nested dirs, 1 symlink, varied modes), drives `SftpRsyncTransport::start_session`, drains the `RsyncProgressEvent` lane to `SyncCompleted`, then verifies every file landed byte-identical (`sha256sum`), modes match (`stat -c '%a'`), and the symlink target resolved correctly (`readlink`).

### SFTP server compatibility notes

Surfaced by the alpha.4 e2e VM run against a stock Debian 13 OpenSSH `internal-sftp` server:

1. **Destination root must exist before the recursive walk.** v7.0.0-alpha.4 fix: the SFTP transport now best-effort `mkdir`s the destination root before walking it. "path exists" failures are swallowed (the happy path on a re-run). Without this fix the comparator emitted `Mkdir` actions for every nested child but never for the root itself, so every subsequent `write_chunk` failed with `No such file`.
2. **OpenSSH `SSH_FXP_SYMLINK` argument-order quirk.** v7.0.0-alpha.4 fix: `RusshRsyncSftpFs::symlink` calls russh-sftp's `symlink(target_first, link_second)` to match OpenSSH's wire-level swap. The SFTP draft documents `(linkpath, targetpath)`; OpenSSH inverts the order, breaking every interoperability client until the workaround is applied. Documented in `OpenSSH PROTOCOL.1`. Without the swap the symlink op surfaced as `SFTP_FEATURE_MISSING` despite the server fully supporting symlinks.
3. **Locked-down `internal-sftp` configurations refuse setstat / symlink.** The capability probe rules these out before the recursive walk burns RTT. Set `preserve.perms=false` / `preserve.symlinks=false` (or use `transport=Wire`) when the probe surfaces `SFTP_FEATURE_MISSING`.

### Deferred to the next v7.0.x slice

Stays out of v7.0.0-alpha.4; lands in the next slice:

- **Wire transport body** — full rsync protocol-v31 handshake + file-list + block-checksum exchange + `fast_rsync` delta tokens. Surface stub today returns `RSYNC_PROTOCOL_ERROR` with detail "Wire transport is being implemented; pass transport=Sftp for the SFTP fallback or wait for the next slice".
- **Local-FS adapter for `RsyncSftpFsPort`** — the alpha.4 SFTP transport walks both `src` and `dst` through the same port, so end-to-end push from a local source tree onto a remote host needs a local-FS implementation. Today the e2e VM test pre-creates the source on the remote side; a future slice adds the local-FS bridge.
- **OpenSSH loopback integration tests** + cross-transport property tests.

### v7.0.0-alpha.5 — Wire transport (handshake + mplex + flist landed)

The Wire transport's first three layers landed in v7.0.0-alpha.5:

- **Handshake** (`src/adapters/rsync/wire/handshake.rs`) — rsync v31 version + checksum-seed exchange. Verified against rsync 3.2.7 on a real Linux VM via the new opt-in `tests/v7_rsync_wire_e2e_vm.rs`. `WireSession::checksum_seed` carries the **server-supplied** seed (the original brief's bidirectional exchange was incorrect for the SSH-tunnelled flow — the client never writes a seed of its own, the server is authoritative).
- **Multiplex framing** (`src/adapters/rsync/wire/mplex.rs`) — `MplexReader` / `MplexWriter` over arbitrary `AsyncRead+Write` halves; the 18 `MSG_*` tag values; the `MPLEX_BASE = 7` bias is hidden from callers; oversized-frame protection.
- **File-list codec** (`src/adapters/rsync/wire/flist.rs`) — encode + decode for the v31 wire format. Covers rsync's varint scheme (1- to 9-byte little-endian-tail format) and the XMIT_* flag matrix (`SAME_NAME`, `SAME_TIME`, `SAME_MODE`, `SAME_UID`, `SAME_GID`, `LONG_NAME`, `EXTENDED_FLAGS`, `HLINKED`). Symlink target capture and hardlink-index capture round out the matrix.

The composition wiring `WireRsyncTransport::with_registry(SshHandleRegistry)` is the production constructor; `WireRsyncTransport::new()` stays as the unwired stub fallback (returns the `RSYNC_PROTOCOL_ERROR` "being implemented" detail line if the registry is missing).

When the post-handshake mplex pump deadlocks waiting on bytes the next slice's sender state machine still owes the server, the transport surfaces a clean `DomainError::Timeout` (lane code `TIMEOUT`) after a 5-second deadline rather than pinning the consumer's `recv_event` lane forever.

#### Wire-compat compatibility notes (rsync 3.2.7 on Ubuntu 24.04)

Five quirks of the rsync-over-SSH wire format that surfaced during the alpha.5 e2e validation; documented here so the next slice does not re-discover them:

1. The textual `@RSYNCD: 31\n` greeting from the original brief is the **daemon-mode** handshake (`rsync://` URL scheme over a dedicated TCP server). The SSH-tunnelled flow uses 4-byte LE u32 binary fields. We follow the SSH wire reality.
2. The brief's bidirectional checksum-seed exchange is incorrect for SSH — the server emits the seed unilaterally; the client never writes a seed of its own.
3. v31 file-list entries encode `mode` / `mtime` / `uid` / `gid` as rsync varints, not raw little-endian. The flist codec follows `flist.c::send_file` (rsync 3.2.7).
4. The mplex layer biases every tag byte by `MPLEX_BASE = 7` on the wire. The `MplexTag::from_wire` / `MplexTag::to_wire` helpers hide the bias.
5. The rsync server-sender path **waits** for the client to send an empty filter-list terminator (single `MSG_DATA` mplex frame with `0u32` LE payload, per `exclude.c::send_filter_list`) before it starts emitting `MSG_FLIST` chunks. The current slice sends the terminator but the full receiver-side state machine that consumes the file-list and replies with the per-file block-checksum requests lands in the next slice.

### v7.0.0-alpha.6 — Wire transport push-direction state machine (block-checksum + tokens + sender)

The Wire transport's push-direction state machine landed in v7.0.0-alpha.6:

- **Block-checksum codec** (`src/adapters/rsync/wire/checksum.rs`) — `SumStruct` (count / blen / s2len / remainder) + `ChunkSig` (rolling u32 + strong bytes) parser. Streams off the inner `MSG_DATA` channel via `DataStreamReader`. Whole-file path (`count == 0`) returns an empty `Vec<ChunkSig>` for new-file uploads.
- **Token-stream codec** (`src/adapters/rsync/wire/tokens.rs`) — whole-file `[i32 chunk_len][bytes]` repeated chunks at `CHUNK_SIZE = 32 KiB` followed by the `0` EOF token, then a 16-byte MD5 trailer. Inline RFC-1321 MD5 (no new dependency); validated against the RFC test vectors.
- **Sender state machine** (`src/adapters/rsync/wire/sender.rs`) — drives the per-file generator request loop. Reads `idx + SumStruct + ChunkSig*` per file; emits `idx + zero-SumStruct + token stream + MD5` per response. Exits on `idx == -1`, then echoes `-1` back for phase-done.
- **Streaming `MSG_DATA` codec** (`src/adapters/rsync/wire/mplex.rs`) — `DataStreamReader` / `DataStreamWriter` chunk inner-protocol bytes through `MSG_DATA` mplex frames, demultiplexing advisory / fatal frames in band. Replaces the alpha.5 `MSG_FLIST` / `MSG_FLIST_EOF` plumbing — those wire bytes do **not** exist in `rsync.h`'s `enum msgcode`. The flist arrives as raw bytes inside `MSG_DATA` frames, terminated by the in-stream `0x00` flag byte.
- **Push-direction wire flow** (`src/adapters/rsync/wire/mod.rs`) — `rsync --server -e.LsfxCIu -r . <dst>` argv (no `--sender`); skips the filter-list send (server's `recv_filter_list` is a no-op without `--delete` / `--prune-empty-dirs`); ships flist + 0x00 terminator only (no trailing io_error u32 in the non-`CF_VARINT_FLIST_FLAGS` path).
- **Local-FS walker** (`src/adapters/rsync/wire/mod.rs::walk_local_source`) — recursive depth-first walker over the local source tree. Replaces the alpha.5 e2e test's "remote source" hack: the e2e VM test now creates the source tree on the **client** side (this host) and pushes it to the remote `rsync --server` process.

**Honest scope retrenchment.** The handshake, mplex framing, flist write, sender state machine, checksum / token codecs, MD5 trailer all build cleanly and pass the canonical lint + test gates. Against `rsync 3.2.7` on a real Linux VM, the **handshake completes** (negotiates protocol 31, parses the multi-byte `compat_flags = 0x17e` varint, captures the seed) and the **flist + 0x00 terminator are delivered**. The receiver-side state machine on the rsync server does not advance from there in this slice — the most likely cause is a subtle flist-entry encoding mismatch (mode/uid/gid varint layout, root-entry `XMIT_TOP_DIR` flag, or `CF_SAFE_FLIST` requirements) that surfaces as a server-side timeout. The e2e test (`tests/v7_rsync_wire_e2e_vm.rs`) accepts `TIMEOUT` as a documented end-of-slice contract; it pins the framing-clean part of the pipeline so a future slice can iterate from there with byte-level confidence.

#### Wire-compat compatibility notes (alpha.6 additions)

Five new quirks layered on top of the alpha.5 list:

6. The `compat_flags` varint **only flows server → client** (per `compat.c::setup_protocol`'s `if (am_server) write_varint(...)` branch). The client side reads it, but does **not** write a symmetric one of its own. An earlier debug attempt that emitted client-side compat_flags caused the server to reject the next byte as a malformed flist entry.
7. The varint may be **multi-byte** when the server's compat flags exceed `0x80`. Setting `CF_VARINT_FLIST_FLAGS = (1<<7)` plus `CF_ID0_NAMES = (1<<8)` lands at `0x17e`, which serialises as `0x81 0x7f` (two bytes) per rsync's `read_varint` byte-extra table. The handshake in this slice handles arbitrary-length rsync varints up to u32.
8. The client's capability-flag string lives in the `-e.<letters>` argv element. The canonical 3.2.7 client emits `-e.iLsfxCIvu`; we emit `-e.LsfxCIu` for the alpha.6 surface. Each letter unlocks a `CF_*` bit on the server side: `f` → `CF_SAFE_FLIST`, `x` → `CF_AVOID_XATTR_OPTIM`, `C` → `CF_CHKSUM_SEED_FIX`, `I` → `CF_INPLACE_PARTIAL_DIR`, `u` → `CF_ID0_NAMES`. Omit `i` to skip incremental-recurse; omit `v` to skip the string-negotiation handshake.
9. The mplex tag space is **not contiguous** — `MSG_DATA = 0`, `MSG_REDO = 9`, `MSG_STATS = 10`, `MSG_IO_ERROR = 22`, `MSG_NOOP = 42`, `MSG_SUCCESS = 100`, `MSG_DELETED = 101`, `MSG_NO_SEND = 102` (per `enum msgcode` in `rsync.h`). Wire bytes are `tag + MPLEX_BASE = 7`. Earlier slices treated tag values as a contiguous block starting at 7; the new layout follows the canonical `MSG_*` constants verbatim.
10. The legacy flist terminator (`0x00` flag byte) does **not** require a trailing io_error `u32` — the receiver's `recv_file_list` breaks on the `0x00` byte and proceeds without reading further. Only the `XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST` end-marker (low byte `0x04` + high byte `0x10`) carries an io_error count. v31's `read_varint` flist path (`xfer_flags_as_varint = compat_flags & CF_VARINT_FLIST_FLAGS`) emits a different terminator we do not use yet.

### v7.0.0-alpha.7 — openrsync port slice 1 (handshake + mplex re-port)

The handcrafted alpha.5 / alpha.6 wire client was **retired** in favour of a canonical port of OpenBSD's `openrsync` (BSD/ISC). Reference tree: `~/dev/openrsync-ref/` (read-only mirror of `cvs.openbsd.org/cgi-bin/cvsweb/src/usr.bin/openrsync/`). Slice 1 covers:

- **`session.c` → `src/adapters/rsync/wire/session.rs`** — `WireSession` mirrors openrsync's `struct sess` field-for-field with widened Rust integer types. The `handshake` driver mirrors `client.c::rsync_client` (lines 51..76) plus the rsync 3.x `compat_flags` varint extension openrsync 27 doesn't carry.
- **`io.c` → `src/adapters/rsync/wire/io.rs`** — `MplexReader` / `MplexWriter` own their `AsyncRead` / `AsyncWrite` halves exclusively. Per-direction state (`mplex_reads`, `mplex_read_remain`, `total_read`, `total_write`, `mplex_writes`) lives on the `&mut WireSession` threaded through every call. Lock-free port; no `Mutex<T>` anywhere on the path.
- The transport's slice-1 driver (`drive_slice1_session`) opens the `rsync --server` exec channel, runs the handshake, drains a few mplex frames as a round-trip proof, then surfaces a documented "next-slice" `RSYNC_PROTOCOL_ERROR` rather than pinning the consumer's `recv_event` lane. End-to-end-tested against rsync 3.2.7 on a real Linux VM (`tests/v7_rsync_wire_e2e_vm.rs`, `--features e2e-vm --include-ignored`).

#### What was deleted

- `src/adapters/rsync/wire/handshake.rs` (alpha.5) — re-ported as `session.rs`.
- `src/adapters/rsync/wire/mplex.rs` (alpha.5) — re-ported as `io.rs`.
- `src/adapters/rsync/wire/flist.rs` (alpha.5) — re-ported in slice 2, see below.
- `src/adapters/rsync/wire/checksum.rs` / `tokens.rs` / `sender.rs` (alpha.6) — re-ported in slice 3 (TBD).

#### Lock-free invariants (preserved)

Every file under `src/adapters/rsync/wire/` carries zero `Mutex` fields on the hot path. The single exception (`LaneState::{rx, join}` in `mod.rs` wrapping a per-session `mpsc::Receiver` and `JoinHandle`) is documented and identical to the slice the SFTP transport uses.

### v7.0.0-alpha.8 — openrsync port slice 2 (flist exchange)

Slice 2 of the openrsync port lands the file-list encode + decode plus a minimal local-source walker, wired into the wire transport's session driver. Coverage:

- **`flist.c` → `src/adapters/rsync/wire/flist.rs`** — `Flist` value-object mirrors openrsync's `struct flist` + `struct flstat` (collapsed; the C nesting offers no Rust benefit). `recv_flist` ports `flist.c::flist_recv` (lines 597..795). `send_flist` ports `flist.c::flist_send` (lines 264..428) — per openrsync's "for ease, make all of our filenames be 'long'" comment, every entry is emitted with `FLIST_NAME_LONG` set, producing a degenerate but valid flag combination the rsync 3.2.x server parses identically. `gen_flist_local` walks a local directory tree (BFS, sorted) and produces `Vec<Flist>` — match-pattern filtering ships in slice 3.
- **`FLIST_*` flag matrix** — eight 8-bit flags (`FLIST_TOP_LEVEL`, `FLIST_MODE_SAME`, `FLIST_RDEV_SAME`, `FLIST_UID_SAME`, `FLIST_GID_SAME`, `FLIST_NAME_SAME`, `FLIST_NAME_LONG`, `FLIST_TIME_SAME`) carried verbatim from openrsync's `flist.c` lines 55..62. The upstream `XMIT_*` names from rsync 3.x's `extern.h` are documented next to each constant for cross-reference.
- **Slice-2 wire flow** (`drive_slice2_session` in `mod.rs`) — handshake → emit `RsyncProgressEvent::SessionStarted { transport: Wire }` → walk local source → send empty filter-list terminator (single `MSG_DATA` frame with `0u32` LE payload) → send flist via `send_flist` → drain a few post-flist mplex frames → return the slice-3 boundary error. The boundary error message is the new `WIRE_NEXT_SLICE_DETAIL` constant pointing at signature + tokens + sender state machines.
- **Security checks (ported)** — `recv_flist` rejects absolute paths (`/etc/passwd`), backtracking paths (`..`, `../etc`, `a/../b`), zero-length pathnames, non-UTF-8 filenames. Mirrors the openrsync `flist_recv_name` "security violation" branches verbatim.
- **Test surface** — 16 unit tests in `flist.rs` cover every FLIST_* flag combination on round-trip (regular files, directories, symlinks, empty list, large varint sizes, top-level flag, uid/gid preservation, security rejection paths) plus the local-walk against `tempfile::tempdir()`. End-to-end against `rsync 3.2.7 --server` on the Linux VM observes:
  - Handshake → `negotiated=31`.
  - 5-entry flist sent (e.g. `["./", "./a.txt", "./b.txt", "./nested/", "./nested/c.txt"]`).
  - Server emits 2 mplex `Data` frames in response (1-byte + 3-byte payloads — first signal of generator activity).
  - Server then closes the channel as it waits for the slice-3 inner-protocol response — exactly the documented "honest deferral" boundary.

#### openrsync port deviations (documented)

- **`FLIST_*` vs `XMIT_*`**: we follow openrsync's protocol-27 8-bit flag naming. The upstream `XMIT_EXTENDED_FLAGS` two-byte header (used at protocols >= 28) is **not** part of the port. The rsync server tolerates the 8-bit shape when the client never sets `XMIT_EXTENDED_FLAGS` first.
- **Always-LONG name encoding**: openrsync always sets `FLIST_NAME_LONG` on send. We do too. This avoids the 1-byte length corner case and the zero-byte sentinel ambiguity. Server-side parsing is identical for both encodings.
- **No identifier sub-lists yet**: openrsync's `idents_send` / `idents_recv` (numeric uid/gid → name lookup) ships in a later slice. The wire transport currently passes `numeric_ids` semantics — we send the numeric uid/gid and never the trailing name table.
- **No `qsort`-driven `flist_topdirs` re-pass**: openrsync's `flist_recv` runs `qsort` then assigns `FLSTAT_TOP_DIR` based on top-level scan; we leave that to the caller because the assignment depends on whether `--recursive` is in effect — context this slice's caller pins via `gen_flist_local`'s explicit top-level `.` entry.

#### Lock-free invariants (preserved)

The slice-2 module continues the slice-1 contract: `recv_flist` and `send_flist` thread `&mut WireSession` through every I/O call, own their reader/writer halves exclusively, and never share state across tasks. `gen_flist_local` walks the filesystem from a single task; intermediate state lives on the function's stack. No new `Mutex<T>` anywhere.

### v7.0.0-alpha.X — openrsync port slice 3 (block decode + token stream + sender state machine)

Slice 3 lands the inner-protocol push direction: block-set signature decode, MD4 + Adler32 hash kernels, the literal/EOF token stream emitter, and the sender state machine that drives one round of the `rsync_sender` finite-state machine to completion. Coverage:

- **`blocks.c` → `src/adapters/rsync/wire/blocks.rs`** — `BlockSet` + `BlockSig` value-objects mirror openrsync's `struct blkset` + `struct blk` (`extern.h` lines 192..243). `read_blockset` ports `blocks.c::blk_recv` (lines 349..442). `write_blockset_header` ports the payload tail of `blocks.c::blk_recv_ack` (lines 326..343), minus the leading file index — the sender state machine emits the index separately so the same helper covers both the live-file path and the `idx == -1` phase-end marker.
- **`hash.c` + `md4.c` → `src/adapters/rsync/wire/hash.rs`** — `hash_fast` ports `hash.c::hash_fast` (Tridgell rolling 32-bit checksum), preserving the C `signed char` sign-extension pattern. `hash_slow_into` ports `hash.c::hash_slow` (MD4 of `(buf || seed_le)`). `FileHasher` ports the `hash_file_*` triplet (MD4 of `(seed_le || file_bytes)` — note the seed is **prepended** here, not appended). MD4 is delegated to the `md4` crate (RustCrypto) — pinned at `md4 = "0.11"` in `Cargo.toml`. No re-port of `md4.c`.
- **`sender.c::send_up_fsm` → `src/adapters/rsync/wire/tokens.rs`** — `emit_whole_file_tokens` covers the `BLKSTAT_DATA` + `BLKSTAT_TOK` + `BLKSTAT_HASH` arms for the **whole-file** path. Chunks the source bytes at `MAX_CHUNK = 32 KiB` literals, finalises with a `0` end-of-file token + 16-byte MD4 trailer. The block-match path (Adler32 hashtable + MD4 hits) returns a clean slice-4 boundary error — no silent corruption.
- **`sender.c::rsync_sender` → `src/adapters/rsync/wire/sender.rs`** — `drive_sender` is the orchestrator. Per-iteration:
  1. Read int32 file index from receiver (`-1` ends the phase).
  2. Read per-file blockset signature stream (`read_blockset`).
  3. Echo file index + blockset header back.
  4. Slurp local source bytes, emit token stream + 16-byte MD4 digest trailer.
  5. Push `RsyncProgressEvent::FileCompleted` onto the lane.
  6. Loop.

  After the phase ends: pump the receiver's post-phase ack stream until the second `-1`, write the doubled `-1` "sender done" sentinel (per `sender.c` lines 221..236, the second sentinel is conditional on `rver > 27`), and best-effort read the goodbye ack.
- **Wire-transport pipeline (`mod.rs`)** — `drive_slice3_session` opens the rsync `--server` exec channel, runs the handshake, splits the russh channel into reader + writer halves, prepends any leftover bytes from the handshake's accumulator in front of the channel reader (otherwise piggy-backed bytes on the seed segment would be silently dropped — first-contact bug surfaced during the e2e), emits the empty filter terminator + flist + post-flist `int32(0)` IO-error sentinel, then drives the sender state machine to completion.
- **Test surface** — 11 unit tests in `blocks.rs` (round-trip count=0 / count=2 blocksets, prologue invariant guards, header round-trip), 9 unit tests in `hash.rs` (Tridgell hash-fast known-vectors with sign-extension regression, MD4 known-vectors via the RustCrypto crate, FileHasher round-trip), 4 unit tests in `tokens.rs` (whole-file path emits literal+EOF+MD4, MAX_CHUNK chunking, empty-file path, block-match slice-4 error guard), 4 unit tests in `sender.rs` (round-trip a single small file via `tokio::io::duplex`, invalid negative index error, block-match slice-4 error). End-to-end (`tests/v7_rsync_wire_e2e_vm.rs`, gated `e2e-vm`) drives the live wire transport against `rsync 3.2.7 --server` on the project Linux VM:
  - Handshake → `negotiated=27` (we now pin `lver=27` per openrsync's `extern.h::RSYNC_PROTOCOL`).
  - 5-entry flist sent; server's flist parser **rejects** the protocol-27 8-bit flag layout because rsync 3.2.7's receiver enforces protocol-30+ flist parsing (the `XMIT_EXTENDED_FLAGS` 16-bit form). Slice 3 surfaces a clean `RSYNC_PROTOCOL_ERROR` with the server's own diagnostic ("File-list index N not in -1 - -1 (`read_ndx_and_attrs`)" / "rsync error: protocol incompatibility (code 2)").
  - Test gracefully accepts the protocol-incompatibility outcome — closing the byte-identical-on-disk verification step until slice 4 lands the protocol-30+ flist upgrade.

#### openrsync port deviations (documented)

- **`RSYNC_PROTOCOL = 27`**: matches openrsync's pinned constant. The session's `lver` is 27, the negotiated protocol drops to `min(lver, rver) = 27`. Modern rsync 3.2.7 servers tolerate the handshake but enforce protocol-30+ flist parsing on the receiver side — so the slice-3 transport completes the handshake + flist exchange but the server rejects the flist bytes. Slice 4 lifts `lver` to 31 and ships the `XMIT_EXTENDED_FLAGS` 16-bit flag layout to close this gap.
- **`mplex_reads = true` even at protocol 27**: mirrors openrsync's `client.c` line 76 (`sess.mplex_reads = 1` is unconditional after handshake). The server's inner-protocol output is mplex-framed regardless of negotiated version; the framer transparently demuxes log / error / advisory frames and presents only `MSG_DATA` payload bytes to the sender state machine. The **client write side** stays raw (`mplex_writes = false`) per `server.c` line 92 — only the server enables write-side framing.
- **`compat_flags` varint conditional on negotiated >= 30**: rsync 3.2.7's server only emits the `compat_flags` byte when negotiating protocol 30+. With our `lver = 27` the server skips it, and the handshake reads only 8 bytes (rver=4 + seed=4) instead of 9. The bit is checked dynamically (`sess.negotiated >= 30`).
- **Whole-file token path only (slice 3 simplification)**: the sender errors clean if the receiver sends a `count > 0` blockset (slice-4 territory). Whole-file is sufficient for first-time sync to an empty destination, which is what the e2e test exercises.
- **Read entire file into memory (slice 3 simplification)**: `read_local_file` slurps the source via `tokio::fs::read`. Slice 4+ chunks via `mmap` / streaming; for small files (< 100 MiB) the simplification is acceptable and avoids dragging in `memmap2` / unsafe.
- **Handshake leftover-bytes preservation**: server frequently piggy-backs the first inner-protocol bytes on the same TCP segment as the seed. `run_handshake_via_msg` returns `(WireSession, Vec<u8>)` and `drive_post_handshake` chains the leftover Cursor in front of the channel reader via `AsyncReadExt::chain`. Without this fix, the first post-handshake read would surface "early eof" on every transfer — a first-contact bug we fixed during e2e iteration.

#### Lock-free invariants (preserved)

The slice-3 modules continue the slice-1/2 contract: `BlockSet`, `BlockSig`, `FileHasher`, the sender-state-machine `SenderCtx` group, and `ZeroStats` all live on per-task stacks or are threaded as `&mut`. The `MplexReader` / `MplexWriter` halves stay owned exclusively by the per-session task. No new `Mutex<T>` anywhere — the only `tokio::sync::Mutex` in the wire transport tree remains the documented `LaneState::{rx, join}` pattern shared with `SftpRsyncTransport`.

#### Self-audit

```text
$ grep -rn "Mutex" src/adapters/rsync/wire/
src/adapters/rsync/wire/mod.rs:80:use tokio::sync::Mutex as AsyncMutex;
src/adapters/rsync/wire/mod.rs:120:    rx: AsyncMutex<Receiver<RsyncProgressEvent>>,
src/adapters/rsync/wire/mod.rs:122:    join: AsyncMutex<Option<JoinHandle<()>>>,
# (the only matches — the documented `LaneState::{rx, join}` exception)
```

### v7.0.0-alpha.X — openrsync port slice 4 (protocol 31 + 16-bit XMIT_EXTENDED_FLAGS + varint30/varlong30 + ndx codec + iflags)

Slice 4 lifts the wire client from protocol 27 to **protocol 31** so the flist + per-file request stream actually parses against stock rsync 3.2.x servers. Coverage:

- **`RSYNC_PROTOCOL` lifted from 27 to 31** (`src/adapters/rsync/wire/session.rs`). `WireSession.negotiated` now becomes `min(31, server_rver) = 31` against any rsync 3.x server. Two new public constants — `XMIT_EXTENDED_FLAGS_MIN_PROTOCOL = 28` (16-bit flag boundary) and `VARINT_FLIST_MIN_PROTOCOL = 30` (varint30/varlong30 boundary) — let downstream encoders branch cleanly.
- **`flist.c::send_file_entry` / `recv_file_entry` 16-bit flag form** (`src/adapters/rsync/wire/flist.rs`). `send_flist` / `recv_flist` now take a `negotiated: i32` argument and switch wire shape accordingly:
  - `negotiated < 28`: legacy openrsync 8-bit `FLIST_*` flag byte (preserved verbatim for the `proto27_round_trip_keeps_legacy_8bit_flag_shape` test).
  - `negotiated >= 28`: 16-bit `write_shortint` flag short with `XMIT_EXTENDED_FLAGS` (= `1 << 2 = 0x04`) pinned in the low byte. Mirrors upstream rsync 3.2.7's `write_shortint(f, xflags)` branch when `(xflags & 0xFF00) || !xflags`.
- **`io.c::read_varint` / `write_varint` / `read_varlong` / `write_varlong` codecs** (`src/adapters/rsync/wire/flist.rs`). Bit-shuffle ports of the upstream `io.c` codecs, including the 64-entry `INT_BYTE_EXTRA` lookup table verbatim from `io.c` line 119. At `negotiated >= 30` the long-name length / file size / mtime / uid / gid / symlink-len fields switch to varint30 / varlong30 encoding (file size uses `min_bytes = 3`, mtime uses `min_bytes = 4`). `recv_long` and `send_long` (the legacy i32 + i64-sentinel pair) stay available for the proto-27 fallback path.
- **`io.c::read_ndx` / `write_ndx` byte-reduction codec** (NEW MODULE — `src/adapters/rsync/wire/ndx.rs`). At protocol >= 30 file-list indices travel as 1..=6 bytes via a stateful diff-from-prev encoding: `NDX_DONE = -1` collapses to a single `0x00` byte, positive indices encode the diff from `prev_positive`, negative indices use the `0xFF` prefix + diff from `prev_negative`. `NdxState` lives on the per-task stack — separate cursors for read direction vs write direction per upstream `static int32 prev_*` initialisation. The slice-3 sender state machine that used `read_int(idx)` / `write_int(idx)` was emitting wire bytes the server interpreted as positive-4-byte sentinels, which is what the `unexpected tag -7 [Receiver]` and `file index 10485762 out of range` diagnostics surfaced during e2e iteration.
- **`rsync.c::read_ndx_and_attrs` iflags + per-file attrs** (`src/adapters/rsync/wire/sender.rs`). After the file index, the receiver emits a 2-byte `iflags` short (`read_shortint`) followed by an optional `fnamecmp_type` byte and `xname` vstring (gated on `ITEM_BASIS_TYPE_FOLLOWS = 1 << 11` and `ITEM_XNAME_FOLLOWS = 1 << 12` respectively). Slice 4 reads + drains them — slice 5+ will act on the bits to drive fuzzy basis matching. The sender echoes the same iflags back per `sender.c::write_ndx_and_attrs`.
- **Stale filter terminator removed** (`src/adapters/rsync/wire/mod.rs`). The slice-3 driver was unconditionally emitting a 4-byte zero filter terminator before the flist. Per upstream `exclude.c::send_filter_list` lines 1644..1660, the client passes `f_out = -1` to `send_rules` and skips the trailing `write_int(f_out, 0)` whenever `am_sender && !receiver_wants_list` (the latter requires `--delete` or `--prune-empty-dirs`). We send neither — so we now omit the filter list entirely.
- **`io_start_multiplex_out` enabled at protocol >= 30** (`src/adapters/rsync/wire/mod.rs`). Per upstream `main.c::client_run` lines 1297..1300, the client-sender mplex-frames its output too at proto >= 30. `sess.mplex_writes` is now lifted to `true` immediately after the handshake when `negotiated >= 30`. Slice 3's `mplex_writes = false` produced wire bytes the server interpreted as a frame header with tag `-7`.
- **Stale post-flist int32 io_error sentinel removed** at proto >= 30. Per upstream `flist.c::recv_file_list` line 2728, the receiver only reads a trailing `read_int(f)` for the io_error flag when `protocol_version < 30`. At proto >= 30 the io_error encoding moves into the flist's end-of-list sentinel itself (`write_shortint(XMIT_EXTENDED_FLAGS|XMIT_IO_ERROR_ENDLIST, ...)` for the error case, plain `write_byte(0)` for the clean-end case).
- **`blocks.c::blk_recv` strong_len = 0 + count = 0 (null_sum) tolerance**. Per upstream `generator.c::generate_and_send_sums` lines 1948..1958, the server emits `write_sum_head(f_out, NULL)` (= `null_sum`, all-zero fields) when `whole_file == 1` (first-time sync to an empty destination). The slice-3 prologue check rejected `strong_len == 0` unconditionally; slice 4 now accepts the all-zero null_sum case while still enforcing `strong_len <= 16` for the non-whole-file path.

#### openrsync port deviations (documented)

The upstream rsync 3.2.7 wire shape diverges from openrsync at multiple points beyond the slice-3 deltas. Slice 4 covers the proto-30+ deltas required for the first file transfer — the deltas listed below are the **slice-5+ surface** that slice 4 explicitly defers:

- **Multi-file ack interleaving (slice-5 boundary)**: after each whole-file transfer completes (literal+EOF+MD4), the receiver acknowledges via `MSG_SUCCESS = 100` mplex frames. The slice-4 `MplexReader` transparently demuxes these, but a third file's transfer request sometimes arrives with the iflags byte stream out-of-frame relative to what `read_ndx_and_attrs` expects. Investigation suggests the receiver may emit additional `MSG_NO_SEND` / `MSG_DELETED` / `MSG_REDO` frames between transfers that have side-effects on the sender's iflags-state machine. **Effect**: e2e push of 3 files (`a.txt`, `b.txt`, `nested/c.txt`) emits `FileCompleted` for `a.txt` + `b.txt` then surfaces `RSYNC_PROTOCOL_ERROR: blk_recv: strong_len 0 not in 1..=16` on the third file. The receiver writes nothing to disk because the connection terminates before the receiver commits the staging area. Two-file pushes get the same outcome.
- **`MSG_FLIST` / `MSG_FLIST_EOF` in inc_recurse mode**: not exercised here because we negotiate without `'i'` in the `-e.LsfxC` compat string. Slice 5+ scope.
- **`MSG_DELETED` / `--delete`**: not exercised here. Slice 6 scope per the original ADR.
- **Receive direction (downloader/receiver pull)**: the slice-4 transport is push-only. Pull direction needs the sender state machine inverted plus the receiver's `recv_files` + `do_recv_token` ports. Slice 5 scope.

#### What e2e proves vs what's missing

The `tests/v7_rsync_wire_e2e_vm.rs` test (gated `e2e-vm`) drives the live wire transport against `rsync 3.2.7 --server` on the project Linux VM. Slice-4 outcomes:

- **Negotiate protocol 31**: ✓ (`handshake: complete rver=31 negotiated=31`).
- **Send 5-entry flist (1 dir + 2 reg files + 1 sub-dir + 1 reg file in sub-dir)**: ✓ (server's flist parser accepts the 16-bit `XMIT_EXTENDED_FLAGS` + varint30/varlong30 byte stream byte-for-byte; previous "unexpected tag -7" / "file index out of range" diagnostics no longer surface).
- **Receive per-file generator request via `read_ndx` + iflags + null_sum**: ✓ (the prologue parses cleanly, `block_count = 0, block_len = 0, strong_len = 0, remainder = 0`).
- **Emit literal + EOF + MD4 token stream for a.txt + b.txt**: ✓ (sender state machine drives both files to `FileCompleted`).
- **Persist files on remote**: ✗ (the receiver buffers everything in a staging area and only commits on protocol completion; the slice-5 multi-file ack interleaving issue terminates the connection before the receiver's `recv_files` returns success).

The test's terminal frame remains a tolerable `SessionFailed { code: RSYNC_PROTOCOL_ERROR, ... }` pending slice 5; the test asserts `acceptable = ["RSYNC_PROTOCOL_ERROR", "TIMEOUT", "TRANSPORT_ERROR"]` so it still passes.

#### Lock-free invariants (preserved)

- New `NdxState` struct lives on per-task stacks. Reader and writer ends own separate cursors per upstream `static int32 prev_*` initialisation — no shared cell, no `Mutex`.
- `NdxState`, the sender's `SenderCtx` group (now extended with `ndx_in: &mut NdxState` and `ndx_out: &mut NdxState`), and the per-file iflags cursor all stay value-typed.
- The `varint` / `varlong` codecs are pure functions returning `(buffer, count)` pairs; no internal state; no allocation beyond the small staging arrays (`[u8; 5]` for varint, `[u8; 9]` for varlong).
- Self-audit:

```text
$ grep -rn "Mutex" src/adapters/rsync/wire/
src/adapters/rsync/wire/mod.rs:80:use tokio::sync::Mutex as AsyncMutex;
src/adapters/rsync/wire/mod.rs:120:    rx: AsyncMutex<Receiver<RsyncProgressEvent>>,
src/adapters/rsync/wire/mod.rs:122:    join: AsyncMutex<Option<JoinHandle<()>>>,
# (still the only `Mutex` matches — the documented `LaneState::{rx, join}` exception)
```

### v7.0.0-alpha.X — openrsync port slice 5 (per-file digest = MD5 + multi-phase send_files loop)

Slice 5 lifts the wire client from "byte-perfect handshake plus untrusted file body" to **byte-identical end-to-end transfer**. Three files (`a.txt`, `b.txt`, `nested/c.txt`) push to a real `rsync 3.2.7 --server` on the project Linux VM and land sha256-equivalent on disk. Coverage:

- **Per-file transfer digest switched from MD4-with-seed to plain MD5 at protocol >= 30** (`src/adapters/rsync/wire/hash.rs`). Upstream rsync 3.2.7's `checksum.c::parse_csum_name` (lines 109..143) implies `CSUM_MD5` whenever `protocol_version >= 30 && proper_seed_order`. The `CSUM_MD5` arm of `sum_init` (lines 597..599) calls `md5_begin(&ctx_md)` with **no seed prologue** — a stark divergence from openrsync's protocol-27-pinned `MD4(seed_le || file_bytes)` shape that we ported in slice 3. The receiver verifies the digest via `memcmp(file_sum1, sender_file_sum, xfer_sum_len)` in `receiver.c::receive_data` line 411; mismatch returns `recv_ok = 0` and the staging area is discarded. **Effect**: previous slices passed every wire framing assertion but the receiver was silently discarding every file.
- **`FileHasher` refactored into an enum** with two variants — `Md4Seeded` (legacy proto < 30, `MD4(seed_le || bytes)`) and `Md5Plain` (proto >= 30, plain MD5 of bytes). `FileHasher::for_protocol(seed, negotiated)` picks the right variant. The `md-5` crate (RustCrypto family, `md-5 = "0.10"`) is the only new dependency.
- **`emit_whole_file_tokens` takes a `negotiated: i32` parameter** (`src/adapters/rsync/wire/tokens.rs`). The token-stream encoder threads the negotiated protocol to `FileHasher::for_protocol` so the per-file 16-byte trailer matches the receiver's expectation byte-for-byte.
- **Multi-phase `send_files` loop** (`src/adapters/rsync/wire/sender.rs`). Direct port of upstream `sender.c::send_files` lines 225..258. The new `drive_send_files_loop` bumps a `phase` counter on every received `NDX_DONE` and **echoes `NDX_DONE` back to the receiver** after each non-final boundary. Exits when `phase > max_phase` (`max_phase = 2` at protocol >= 29). The previous slice-4 implementation read a single phase-end `NDX_DONE` and immediately wrote a doubled `NDX_DONE` back — which deadlocked the upstream generator's `msgdone_cnt` wait loop in `generator.c::generate_files` lines 2384..2402 (the generator needs the receiver to forward at least 2 phase echoes via the `error_pipe` before it advances past the redo phase).
- **Final post-loop `NDX_DONE` write** (`src/adapters/rsync/wire/sender.rs::drive_sender`). After the phase loop exits, the sender writes one more `NDX_DONE` per upstream `sender.c::send_files` line 464 (`write_ndx(f_out, NDX_DONE);` immediately before returning). This is the **third** `NDX_DONE` from the sender; without it the receiver's `recv_files` phase loop never breaks out and the generator never reaches the post-phase wait that emits the final goodbye sentinel.
- **`read_goodbye` now implements the full proto-31 handshake** (`src/adapters/rsync/wire/sender.rs::read_goodbye`). Direct port of `main.c::read_final_goodbye` (lines 875..906): read `NDX_DONE`, write `NDX_DONE`, read another `NDX_DONE` at protocol >= 31; single `NDX_DONE` read at protocol < 31. The previous slice-4 implementation read just one `NDX_DONE` and returned, missing the proto-31 round-trip step.
- **`pump_post_phase_acks` and `write_sender_done` deleted**. Their behaviour is subsumed by the unified phase loop + the post-loop `NDX_DONE` write. The previous control-flow assumed phase 1 was the entire send window and treated phase-2 redo requests as ignorable post-phase acks — a misreading of the upstream loop topology.

#### openrsync port deviations (closed in slice 5)

The slice-4 deviation list flagged "multi-file ack interleaving" as the slice-5 boundary. Slice 5 closes that boundary by porting the multi-phase loop verbatim from upstream rsync 3.2.7 (NOT openrsync 27, which never grew a redo phase). Open boundaries that stay deferred:

- **Block-match path** (`count > 0` in the receiver's signature stream). Slice 5 still emits one giant literal token per file. Adler32 hashtable + sliding-window matching is the natural slice-6 entry.
- **Receive direction (pull)**: not exercised; `do_recv_token` + `recv_files` ports remain slice-7 scope.
- **`--delete`, attrs apply (`-p -t -o -g -l`), hardlinks, sparse, `--partial`**: slice-8+ scope per the original ADR.

#### What e2e proves vs what's missing (slice 5)

The `tests/v7_rsync_wire_e2e_vm.rs` test (gated `e2e-vm`) drives the live wire transport against `rsync 3.2.7 --server` on the project Linux VM. Slice-5 outcomes:

- **Negotiate protocol 31**: ✓ (unchanged from slice 4).
- **Send 5-entry flist**: ✓ (unchanged from slice 4).
- **Receive 3 per-file generator requests + 1 itemize-only directory frame**: ✓ (unchanged from slice 4).
- **Emit literal + EOF + plain MD5 trailer for all 3 files**: ✓ (regression-tested via `whole_file_path_proto_31_emits_md5_no_seed_trailer`).
- **Phase 0/1/2 boundaries handled correctly with two echoes + final post-loop write**: ✓ (`phase=1 echo`, `phase=2 echo`, `phase=3 exit`, then post-loop `NDX_DONE` per sender.c:464).
- **Final goodbye exchange (proto >= 31 round-trip)**: ✓ (`first NDX = -1`, write reply, `second NDX = -1`).
- **`SyncCompleted` event surfaces with `files_done = 3, bytes_transferred = 59`**: ✓.
- **Files land byte-identical on remote disk** — verified via `cat` over SSH against the expected payload strings: ✓.

Three e2e iterations against the live VM confirm the pipeline is stable end-to-end.

#### Lock-free invariants (preserved)

- `FileHasher` enum lives by-value inside the per-file token-emit fn — no shared cell, no `Mutex`.
- `phase` counter is a stack-local `u8` in `drive_send_files_loop` — never `Arc<AtomicU8>` because there is no cross-task observation requirement.
- The unified phase loop keeps the `&mut SenderCtx<'_, R, W>` borrow chain intact; reader and writer halves stay exclusively owned by the same task.
- Self-audit:

```text
$ grep -rn "Mutex" src/adapters/rsync/wire/
src/adapters/rsync/wire/mod.rs:80:use tokio::sync::Mutex as AsyncMutex;
src/adapters/rsync/wire/mod.rs:120:    rx: AsyncMutex<Receiver<RsyncProgressEvent>>,
src/adapters/rsync/wire/mod.rs:122:    join: AsyncMutex<Option<JoinHandle<()>>>,
# (still the only `Mutex` matches — the documented `LaneState::{rx, join}` exception)
```

## v6.0 → v6.1

For **MCP host operators, contributors, and downstream automations** moving from v6.0 to v6.1. **Wire-additive** — every v6.0 host keeps working byte-for-byte; resume is purely opt-in via two new request flags. No tool name strings, resource URI schemes, error categories, env vars, or default-behaviour deltas. Reference: [ADR 0010 — SFTP transfer resume semantics](./adr/0010-sftp-resume.md).

### What's new

- **Two opt-in request flags** on `ssh_upload` and `ssh_download` — `resume: bool?` (default `false`) and `verify: bool?` (default `false`). When `resume=true` the server pre-flights destination size and resumes from the first non-overlapping byte; pairing with `verify=true` sha256-compares the resume prefix on both sides before continuing.
- **One new response line** — `RESUMED_FROM: <u64>` is emitted **only** when the resume offset is greater than zero. v6.0 callers who never set `resume=true` see byte-identical wires.
- **One new structured-content field** — `"resumed_from": <u64>` on the upload / download structured payload. `#[serde(default)] = 0` so v5/v6.0 JSON consumers parse unchanged.
- **One new domain field** — `TransferEntity.resumed_from: u64`. `#[serde(default)] = 0` so v5/v6.0 transfer snapshot JSON deserialises unchanged.
- **Two new wire codes** — `RESUME_OVERSHOOT` and `RESUME_MISMATCH` (both `STATE`, neither retryable). Total error-taxonomy size grows from 38 to 40. See [ADR 0007](./adr/0007-error-taxonomy.md) and [LLM_GUIDE.md → Error handbook](./LLM_GUIDE.md#error-handbook).

### What does NOT change

- Tool catalogue: still 36 tools (35 without `port_forward`). No name string changes.
- Resource URI schemes: byte-identical — `transfer://<id>/progress` lane unchanged.
- Cursor / SubId model: byte-identical.
- Default behaviour: every existing knob (`release_when_no_subs`, lag policy, debouncer windows, broadcast caps, lifecycle bindings) keeps its v6.0 value.
- Environment variables: **none added**. The resume primitive is purely runtime-flag-driven; see [CONFIGURATION.md → v6.1 / ADR 0010](./CONFIGURATION.md#v61--adr-0010--resume--verify-no-new-env-vars).
- MSRV: still Rust 1.95.

### Recommended upgrade path

1. **Bump dependency** — `ssh-mcp = "6.1"` in your host (or rebuild from source). No code change required to keep v6.0 behaviour.
2. **Optional opt-in** — when retrying a failed multi-GiB transfer, pass `resume: true` to `ssh_upload` / `ssh_download`. Inspect the `RESUMED_FROM:` line / `resumed_from` structured field for the offset.
3. **Flaky-link hardening** — pair `resume: true` with `verify: true` on long-haul (cellular, transatlantic, sat-link) transfers to surface mid-transfer prefix corruption via `RESUME_MISMATCH`. Costs one `ssh_exec` round-trip plus O(offset) bytes hashed remotely; default-off keeps the fast path fast.
4. **Branch on the new error codes** — extend any retry policy to treat `RESUME_OVERSHOOT` and `RESUME_MISMATCH` as terminal (caller-fixable); see [LLM_GUIDE.md → Resuming a failed transfer](./LLM_GUIDE.md#v61--adr-0010--resuming-a-failed-transfer) for the two happy paths.

### Wire-format gotchas

- **`RESUMED_FROM:` line position** — emitted between `BYTES:` and `HINT:` on the rendered block. Hosts that parse the upload / download response with positional regex must accommodate the extra optional line. The line is suppressed when offset is `0`, so v6.0-only callers never observe it.
- **Skip plan synchronous completion** — when `resume=true` and remote size already matches local size, the transfer reaches `Completed` synchronously inside the tool call. `transfer://<id>/progress` subscribers connecting after the call get a single replay event with `status=Completed`, `bytes_transferred = total_bytes`, `resumed_from = total_bytes`. No mid-flight events emit. See [RESOURCES.md → v6.1 / ADR 0010](./RESOURCES.md#v61--adr-0010--resumed-transfers-ramp-from-resumed_from).
- **`TransferEntity.resumed_from`** deserialises with `#[serde(default)] = 0` for v5 / v6.0 snapshots — older snapshot files in operational logs still parse with the v6.1 binary.

### What does NOT need to change

- Any host that never passes `resume` / `verify` sees v6.0 byte-for-byte wires.
- Existing snapshot tests in `tests/v4_smoke.rs` / `tests/v5_smoke.rs` keep passing — the new `RESUMED_FROM:` line is gated behind `resume = true` so legacy wire shapes are byte-identical by construction.
- NDJSON daemon callers: `resume` / `verify` land via `#[serde(default)]`; v6.0 daemon clients work unchanged. New parser-level tests live in `src/embed/parser.rs::tests::parses_*_with_resume_and_verify_flags`.
