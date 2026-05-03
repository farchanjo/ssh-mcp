# Migrating Contributors from v3.0 to v4.x (Internal Architecture)

This document is for **codebase contributors** moving from v3.0.x to the v4.x hexagonal layout (current: v4.1.0). The **public MCP API is stable** across the v4 line — every external client / host / LLM continues to work without any change. v4 is an internal restructuring, not a wire-format break.

If you only consume the MCP server (HTTP or stdio), stop here — see [API.md](./API.md) for the unchanged tool catalogue.

> **v4.1 update.** The H17.6 deep decouple shipped: the foundational `src/mcp/` tree is gone, the `async-trait` direct dependency is dropped, and every former `crate::mcp::*` reference now lives at `crate::adapters::{ssh,sftp,config,subscription}::internal::*` (or `adapters::subscription::legacy` for the transitional global registry). See the [v4.1 deep decouple addendum](#v41-deep-decouple-addendum) at the bottom of this document.

Cross references:

- [ARCHITECTURE.md](./ARCHITECTURE.md) — full v4 hexagonal layout (layers, module map, sequence diagrams).
- [LOCKS.md](./LOCKS.md) — lock-free invariants under the new layer structure.
- [adr/0002-adopt-hexagonal-architecture.md](./adr/0002-adopt-hexagonal-architecture.md) — design rationale.
- [MIGRATION_v2_to_v3.md](./MIGRATION_v2_to_v3.md) — historical (v2 → v3 client migration; not relevant for v4).

## What stayed identical (zero client work)

| Surface | v3 → v4 status |
|---------|----------------|
| MCP tool count | **18** (same) |
| MCP tool names + signatures | identical |
| MCP tool response markdown shape | identical (block-only, 8-hex nonce delimiters, `KEY: value` per line) |
| 5 `resources/*` schemes (`shell`, `command`, `transfer`, `session`, `forward`) | identical URIs + `_meta` envelope + cursor semantics |
| `notifications/resources/updated` | identical (per debounce window) |
| `notifications/cancelled` | identical (rmcp 1.6 native) |
| Capability handshake (`get_info()`) | identical (`V_2025_06_18`, `tools.listChanged=true`, `resources.{subscribe,listChanged}=true`) |
| 25+ env vars (`SSH_*`, `MCP_*`, `RUST_LOG`) | identical names, defaults, floors, caps |
| HTTP transport bind / path defaults (`0.0.0.0:8000`, `/`) | identical |
| Stdio transport behaviour | identical |
| Strict Clippy lint baseline (forbid / deny + lock-free invariants) | identical (every v3 lint kept) |
| Cargo features (`port_forward` default-on) | identical |
| Test count and test contracts | grew from 820 → 1014 lib tests (1021 in v4.0.0; small dip in v4.1 reflects removal of `mcp::*` internal-state regression tests now redundant against the relocated adapter-internal modules); every v3 public-surface test still passes |

In short: a v3 host pointed at a v4 server sees no observable difference. Even error markdown bodies and the 8-hex nonce randomisation are byte-identical.

## What moved (file-path table)

The v3.0.0 codebase shipped under `src/mcp/` only (the v2 monolith plus the v3 split). v4.0.0 introduces six top-level layers under `src/`:

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

### Tool implementations

| v3 path | v4 path |
|---------|---------|
| `src/mcp/tools/connection.rs` | application: `src/application/{connect_session,disconnect_session,list_sessions,disconnect_agent}.rs` ; render: `src/infra/mcp/{args,render}/connection.rs` ; rmcp wiring: `src/infra/mcp/tool_router.rs` |
| `src/mcp/tools/execute.rs` | application: `src/application/{execute_command,get_command_output,list_commands,cancel_command}.rs` ; render: `src/infra/mcp/{args,render}/execute.rs` |
| `src/mcp/tools/shell.rs` | application: `src/application/{open_shell,write_shell,send_key,read_shell,wait_for_pattern,close_shell}.rs` ; render: `src/infra/mcp/{args,render}/shell.rs` |
| `src/mcp/tools/sftp.rs` | application: `src/application/{upload_file,download_file,get_transfer_progress}.rs` ; render: `src/infra/mcp/{args,render}/sftp.rs` |
| `src/mcp/tools/forward.rs` (feature `port_forward`) | application: `src/application/forward_port.rs` ; render: `src/infra/mcp/{args,render}/forward.rs` |
| `src/mcp/tools/legacy_helpers.rs` | deleted in H17.5a; helpers now under `src/infra/mcp/helpers/{error,nonce,output}.rs` |

### Storage layer

| v3 path | v4 path |
|---------|---------|
| `src/mcp/storage/{traits,session,command,shell,transfer,forward}.rs` + `SESSION_STORAGE` / `COMMAND_STORAGE` / `SHELL_STORAGE` / `TRANSFER_STORAGE` globals | ports: `src/ports/{session_repo,command_repo,shell_repo,transfer_repo,forward_repo}.rs` ; adapters: `src/adapters/repo/dashmap/{session,command,shell,transfer,forward}.rs` (no globals — wired by `composition::prod`) |

### MCP server + resources

| v3 path | v4 path |
|---------|---------|
| `src/mcp/server.rs` (`McpSshServer` + `#[tool_router]`) | `src/infra/mcp/server.rs` (`McpSshServer<UC>` generic) + `src/infra/mcp/tool_router.rs` (the 18 `#[tool]` entry points) + `src/infra/mcp/resource_handlers.rs` (resources/list, read, subscribe, unsubscribe) |
| `src/mcp/resources.rs` (URI parser + reader handlers) | application: `src/application/{list_resources,read_resource,subscribe_resource,unsubscribe_resource}.rs` ; rmcp wiring: `src/infra/mcp/resource_handlers.rs` |
| `src/mcp/message/{helpers,builder}.rs` | helpers: `src/infra/mcp/helpers/{error,nonce,output}.rs` ; builders: `src/infra/mcp/render/{connection,execute,shell,sftp,forward}.rs` |
| `src/mcp/schema.rs` | inlined into the per-tool args structs under `src/infra/mcp/args/*.rs` (uses `#[derive(Deserialize, JsonSchema)]`) |
| `src/mcp/keys.rs` | `src/domain/keys.rs` (moved as part of H17.5a — domain layer owns the semantic keystroke encoder) |

### Authentication

| v3 path | v4 path |
|---------|---------|
| `src/mcp/auth/{traits,password,key,agent,chain}.rs` (uses `#[async_trait]`) | port: `src/ports/auth_strategy.rs` (`AuthStrategyPort` via `trait-variant` AFIT) ; adapters: `src/adapters/auth/{password,key,agent,chain}.rs` ; runtime chain (post v4.1): `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` (native AFIT + enum dispatcher; `async-trait` direct dep dropped) |

The v3 module was deleted in v4.1 H17.6 P2 (commit `00009e3`) along with the `async-trait` direct dep. See the [v4.1 deep decouple addendum](#v41-deep-decouple-addendum).

### Subscription registry

| v3 path | v4 path |
|---------|---------|
| `src/mcp/subscription.rs` (`SUBSCRIPTION_REGISTRY` global + per-resource debouncer + peer-GC) | port: `src/ports/subscriber_registry.rs` (`SubscriberRegistryPort` sync slice + `SubscriberRegistryAsync` async slice) ; hexagonal adapter: `src/adapters/subscription/memory_registry.rs` (`MemoryRegistry<N>` generic over the notifier) ; transitional global (post v4.1): `src/adapters/subscription/legacy.rs` (`SUBSCRIPTION_REGISTRY` + `spawn_peer_gc`) |

In v4.1, the global was relocated from `src/mcp/subscription.rs` to `src/adapters/subscription/legacy.rs` (commit `72f1ccd`). The SSH/SFTP runtime adapters still poke the global; use cases consume `MemoryRegistry<N>` via the port surface. Both stay in sync because the rmcp notifier adapter wraps the `Peer<RoleServer>` from the same `PeerTable`. The legacy adapter goes away once the SSH/SFTP runtime adapters are migrated to the port handle (tracked under v4.x backlog).

### Notifier

| v3 path | v4 path |
|---------|---------|
| `rmcp::Peer<RoleServer>` plumbed by hand into every tool | port: `src/ports/notifier.rs` (`NotifierPort` async + `PeerHandle` sync) ; adapters: `src/adapters/notifier/{rmcp_adapter,rmcp_peer}.rs` ; `PeerTable` re-exposed under `src/infra/mcp/peer_handle.rs` |

### Foundational `src/mcp/` modules — relocated in v4.1

The v4.0.0 release deferred the foundational decouple to H17.6. **v4.1.0** shipped that cleanup: every former `src/mcp/<module>.rs` was relocated under the owning adapter (or, for the global subscription registry, into a transitional `adapters/subscription/legacy.rs`), and the `src/mcp/` directory was deleted entirely. See the [v4.1 deep decouple addendum](#v41-deep-decouple-addendum) below for the path mapping. The historical context: H17.5a (commit `95ddc5b`) first hard-deleted ~14k LOC of orphaned v3 code (`tools/`, `storage/`, `server.rs`, `message/`, `resources.rs`, `schema.rs`, `keys.rs` moved to `domain/`, `forward.rs`); v4.1 H17.6 P1+P3+P4 then relocated the remaining ~6 500 LOC `mcp::*` runtime-load-bearing surface.

## What changed for contributors

### Static dispatch + AFIT

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

### Layer import rules

Enforced by per-module documentation plus the strict lint baseline:

| Layer | Allowed deps | Forbidden deps |
|-------|--------------|----------------|
| `domain` | `std`, `serde`, `serde_json`, `chrono`, `thiserror`, `schemars`, `bytes` | `tokio`, `russh`, `rmcp`, `axum`, `dashmap` |
| `ports` | `domain`, `bytes`, `chrono`, `std`, `trait_variant` | `tokio`, `russh`, `rmcp`, `axum`, `dashmap` |
| `application` | `domain`, `ports`, `tokio` (`select!` / `spawn`) | `russh`, `rmcp`, `axum`, `dashmap` |
| `adapters` | All runtime crates | `rmcp` (only `notifier/rmcp_*`), `axum` |
| `infra` | `rmcp`, `application`, `domain`, `adapters::notifier::rmcp_peer` | `russh`, `russh_sftp`, `dashmap` |
| `composition` | All adapters + `infra::mcp` + `application` | — (it is the leaf) |

When in doubt, run `cargo clippy --all-features --all-targets --workspace -- -D warnings` and read the import error.

### Adding a new tool

In v3 you would touch `src/mcp/tools/<domain>.rs` (args + business logic + render + rmcp wiring all in one file), `src/mcp/server.rs` (route entry), and `src/mcp/storage/<repo>.rs` (state).

In v4 the same change spans:

1. `src/domain/<entity>.rs` — entity + DTO + status enums (if any).
2. `src/ports/<port>.rs` — extend the trait surface (only if a new capability is needed).
3. `src/adapters/<adapter>.rs` — implement the new port method (russh / sftp / notifier / repo / etc.).
4. `src/application/<use_case>.rs` — write the use case (single `pub async fn execute(&self, req: Request) -> Result<Outcome, DomainError>`).
5. `src/infra/mcp/args/<domain>.rs` — `#[derive(Deserialize, JsonSchema)]` argument struct.
6. `src/infra/mcp/render/<domain>.rs` — markdown body builder.
7. `src/infra/mcp/tool_router.rs` — add the `#[tool]` entry, parse args, call the use case, render the outcome, return `CallToolResult`.
8. `src/composition/mod.rs` — extend `UseCases<…>` if a new use case generic appears.
9. `src/composition/prod.rs` — wire the new `Arc<UseCase<…>>` into `build_use_cases`.

The tradeoff is real (more files per change) and intentional: every layer is independently testable with fakes (`adapters::ssh::fake`, `adapters::sftp::fake`, `adapters::clock::fake`, etc.).

### Writing a use case test

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

Composition isolation makes these tests fast (no russh, no real SFTP, deterministic IDs). 1014 of the 1016 tests are unit tests at this layer.

### Lints (unchanged baseline)

Every v3 lint stays. The lock-free invariants (`await_holding_lock`, `mutex_atomic`, `mutex_integer`, `significant_drop_in_scrutinee`, `significant_drop_tightening`) now apply to `src/adapters/repo/dashmap/*`, `src/adapters/subscription/{memory_registry,legacy}.rs`, and the adapter-internal carriers under `src/adapters/{ssh,sftp}/internal/*`. See [LOCKS.md](./LOCKS.md) for the v4.1 acquisition order and the channel-capacity table.

## Recommended workflow for an in-flight v3 patch

If you have a v3 patch sitting on a branch:

1. Identify which `src/mcp/tools/<domain>.rs` function the patch touches.
2. Use the [Tool implementations](#tool-implementations) table above to find the v4 split (`application/<use_case>.rs` + `infra/mcp/render/<domain>.rs` + `infra/mcp/args/<domain>.rs`).
3. Move business logic into the use case; move markdown builders into `render/`; move arg structs into `args/`.
4. If the patch touches global storage, port the writes onto the relevant `*Repository` port and let the composition root pick `DashMap*Repo`.
5. Run `cargo test --all-features` (must pass) and `cargo clippy --all-features --all-targets --workspace -- -D warnings` (must be clean).

## Build / test / run (unchanged)

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

## v4.1 deep decouple addendum

H17.6 P1+P2+P3+P4 (commits `bf646f9`, `00009e3`, `72f1ccd`) shipped in v4.1.0. The foundational `mcp::*` paths are gone:

| Former v4.0 path | v4.1 path |
|------------------|-----------|
| `src/mcp/client.rs`, `mcp::client::*` | `src/adapters/ssh/internal/client.rs`, `adapters::ssh::internal::client::*` |
| `src/mcp/session.rs`, `mcp::session::*` | `src/adapters/ssh/internal/session.rs`, `adapters::ssh::internal::session::*` |
| `src/mcp/async_command.rs`, `mcp::async_command::*` | `src/adapters/ssh/internal/async_command.rs`, `adapters::ssh::internal::async_command::*` |
| `src/mcp/shell.rs`, `mcp::shell::*` | `src/adapters/ssh/internal/shell.rs`, `adapters::ssh::internal::shell::*` |
| `src/mcp/types.rs` (SSH-side payloads), `mcp::types::*` | `src/adapters/ssh/internal/types.rs`, `adapters::ssh::internal::types::*` |
| `src/mcp/error.rs`, `mcp::error::*` | `src/adapters/ssh/internal/error.rs`, `adapters::ssh::internal::error::*` |
| `src/mcp/auth/{traits,password,key,agent,chain}.rs` (`#[async_trait]`) | `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` (native AFIT + enum dispatcher; `async-trait` direct dep dropped) |
| `src/mcp/sftp.rs`, `mcp::sftp::*` | `src/adapters/sftp/internal/sftp.rs`, `adapters::sftp::internal::sftp::*` |
| `src/mcp/transfer.rs`, `mcp::transfer::*` | `src/adapters/sftp/internal/transfer.rs`, `adapters::sftp::internal::transfer::*` |
| `src/mcp/types.rs` (SFTP-side payloads) | `src/adapters/sftp/internal/types.rs` |
| `src/mcp/config.rs`, `mcp::config::*` | `src/adapters/config/internal/mod.rs`, `adapters::config::internal::*` |
| `src/mcp/subscription.rs`, `mcp::subscription::SUBSCRIPTION_REGISTRY` + `mcp::subscription::spawn_peer_gc` | `src/adapters/subscription/legacy.rs`, `adapters::subscription::legacy::SUBSCRIPTION_REGISTRY` + `adapters::subscription::legacy::spawn_peer_gc` (transitional, alongside `adapters::subscription::memory_registry::MemoryRegistry<N>`) |

For in-flight v4.0 patches: `git grep crate::mcp::` will surface every callsite that needs a path rewrite. Adapter-internal modules are private to their owning adapter; use cases never reach in.

The remaining v4.x backlog:

- Migrate the SSH/SFTP runtime adapters off `adapters::subscription::legacy::SUBSCRIPTION_REGISTRY` and onto the `MemoryRegistry<N>` port handle, then delete the legacy adapter.
- Cross-adapter SFTP refinements (shared transfer scheduler, per-session SFTP semaphore tuning).

See [adr/0002-adopt-hexagonal-architecture.md](./adr/0002-adopt-hexagonal-architecture.md) for the original deferral rationale and the H0 → H18 commit chain (the H17.6 closure is appended to that ADR's Consequences section).
