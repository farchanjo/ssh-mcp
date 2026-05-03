# Migrating Contributors from v3.0 to v4.0 (Internal Architecture)

This document is for **codebase contributors** moving from v3.0.x to the v4.0.0 hexagonal layout. The **public MCP API is stable** — every external client / host / LLM continues to work without any change. v4 is an internal restructuring, not a wire-format break.

If you only consume the MCP server (HTTP or stdio), stop here — see [API.md](./API.md) for the unchanged tool catalogue.

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
| Test count and test contracts | grew from 820 → 1021 lib tests; every v3 test still passes |

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
  mcp/            # foundational v3 leftovers, runtime-active until H17.6
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
| `src/mcp/auth/{traits,password,key,agent,chain}.rs` (uses `#[async_trait]`) | port: `src/ports/auth_strategy.rs` (`AuthStrategyPort` via `trait-variant` AFIT) ; adapters: `src/adapters/auth/{password,key,agent,chain}.rs` |

The v3 module is **runtime-unreachable** in v4 (no use case calls into it) but stays in `Cargo.toml` because the foundational `src/mcp/` block still depends on it transitively. H17.6 will delete it together with the `async-trait` direct dep.

### Subscription registry

| v3 path | v4 path |
|---------|---------|
| `src/mcp/subscription.rs` (`SUBSCRIPTION_REGISTRY` global + per-resource debouncer + peer-GC) | port: `src/ports/subscriber_registry.rs` (`SubscriberRegistryPort` sync slice + `SubscriberRegistryAsync` async slice) ; adapter: `src/adapters/subscription/memory_registry.rs` (`MemoryRegistry<N>` generic over the notifier) |

The v3 global `SUBSCRIPTION_REGISTRY` and the `spawn_peer_gc` task are still **runtime-active** in v4.0.0 because the foundational producers (`mcp::async_command`, `mcp::shell`, `mcp::sftp`, `mcp::transfer`, `mcp::session`) poke the v3 global directly. v4 use cases consume the new `MemoryRegistry`. The two coexist during the v4.0.0 transition window; H17.6 collapses them.

### Notifier

| v3 path | v4 path |
|---------|---------|
| `rmcp::Peer<RoleServer>` plumbed by hand into every tool | port: `src/ports/notifier.rs` (`NotifierPort` async + `PeerHandle` sync) ; adapters: `src/adapters/notifier/{rmcp_adapter,rmcp_peer}.rs` ; `PeerTable` re-exposed under `src/infra/mcp/peer_handle.rs` |

### Foundational `src/mcp/` modules (kept, runtime-active)

These v3 modules survived H17.5a because the v4 adapters (`adapters::ssh::russh_adapter`, `adapters::sftp::russh_sftp_adapter`, `adapters::output_stream::russh_output`) still delegate into them. They are slated for absorption in H17.6 (v4.1 cleanup window):

| v3 module | Why it stays |
|-----------|--------------|
| `src/mcp/async_command.rs` | `RunningCommand` lock-free state consumed by the russh adapter. |
| `src/mcp/auth/` | Strategy chain still on `#[async_trait]`. v4 surface is `src/adapters/auth/*`; v3 module is unreachable but pinned by the transitional `async-trait` dep. |
| `src/mcp/client.rs` | Low-level russh helpers (`connect_to_ssh_with_retry`, `execute_ssh_command`, `open_pty_shell`) reused by the adapter. |
| `src/mcp/config.rs` | Env-var resolvers; `adapters::config::env::EnvConfig` delegates here. |
| `src/mcp/error.rs` | Retry-classification helper consumed by `mcp::client`. |
| `src/mcp/session.rs` | `SshClientHandler` russh callback type. |
| `src/mcp/sftp.rs` | Streaming SFTP transfer state used by the SFTP adapter. |
| `src/mcp/shell.rs` | `RunningShell` + `RingBuffer` consumed by the russh adapter. |
| `src/mcp/subscription.rs` | `SUBSCRIPTION_REGISTRY` global + `spawn_peer_gc` task. The `MemoryRegistry` adapter (H9) is the v4 surface; the v3 global is still poked by the foundational producers. |
| `src/mcp/transfer.rs` | `RunningTransfer` lock-free state. |
| `src/mcp/types.rs` | Shared payload structs (`SessionInfo`, `AsyncCommandInfo`, `ShellInfo`, `TransferInfo`). |

H17.5a (commit `95ddc5b`) hard-deleted ~14k LOC of orphaned v3 code: `tools/`, `storage/`, `server.rs`, `message/`, `resources.rs`, `schema.rs`, `keys.rs` (moved to `domain/`), `forward.rs`. The remaining `mcp::*` surface is the runtime-load-bearing minimum.

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

Wiring errors surface at the composition root, not at runtime. The exception is the legacy `mcp::auth` module (`#[async_trait]`) which stays for the H17.6 deferral window.

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

Composition isolation makes these tests fast (no russh, no real SFTP, deterministic IDs). 1021 of the 1023 tests are unit tests at this layer.

### Lints (unchanged baseline)

Every v3 lint stays. The lock-free invariants (`await_holding_lock`, `mutex_atomic`, `mutex_integer`, `significant_drop_in_scrutinee`, `significant_drop_tightening`) now apply to `src/adapters/repo/dashmap/*` and `src/adapters/subscription/memory_registry.rs` instead of `src/mcp/storage/*` and `src/mcp/subscription.rs`. See [LOCKS.md](./LOCKS.md) for the v4 acquisition order and the channel-capacity table.

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

cargo test --lib --quiet                           # 1021 lib tests
cargo test --tests --quiet                         # integration tests
cargo test --all-features                          # combined run

cargo fmt --all -- --check
cargo clippy --all-features --all-targets --workspace -- -D warnings
```

`MSRV` is now `1.85` (Rust 2024 edition + AFIT stable). `axum` was upgraded from `0.7` to `0.8`.

## Future cleanup (H17.6 / v4.1)

The v4.0.0 release deliberately ships with `src/mcp/{async_command,client,config,error,session,sftp,shell,subscription,transfer,types,auth}` runtime-active. Their absorption into the hexagonal layout is tracked under H17.6 / v4.1:

- Move `RunningCommand`, `RunningShell`, `RunningTransfer` into `src/adapters/{ssh,sftp}/state/` and inline the lock-free carriers there.
- Replace `SUBSCRIPTION_REGISTRY` global with the `MemoryRegistry` adapter exclusively (the foundational producers will get an injected handle instead of a static).
- Delete `src/mcp/auth/` and the `async-trait` direct dep.
- Delete the entire `mcp::*` namespace; only `domain/`, `ports/`, `application/`, `adapters/`, `infra/`, `composition/` survive.

See [adr/0002-adopt-hexagonal-architecture.md](./adr/0002-adopt-hexagonal-architecture.md) for the deferral rationale and the H0 → H18 commit chain.
