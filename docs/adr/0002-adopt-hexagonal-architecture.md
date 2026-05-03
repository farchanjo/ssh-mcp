# ADR 0002: Adopt Hexagonal (Ports and Adapters) Architecture for v4

## Status

Accepted (v4.0.0).

## Context

ssh-mcp v3.0.0 (ADR 0001 — [migrate to rmcp](./0001-migrate-to-rmcp.md)) shipped a SOLID-light layout. Tools, storage, the MCP server handler, and the markdown builders all lived under `src/mcp/`, with two structural traits (`AuthStrategy` and the storage trait family) plus per-domain tool modules under `src/mcp/tools/`. That layout served the v3 surface (18 tools, 5 resource schemes, subscribe-first PTY, lock-free runtime) but ran into three structural ceilings during the v3.x maintenance window:

1. **Tight coupling between business logic and rmcp transport.** Every tool function in `src/mcp/tools/<domain>.rs` mixed three concerns: argument deserialization (`schemars`), business logic (russh / dashmap calls), and markdown rendering. Replacing the markdown body shape, swapping the storage backend, or unit-testing a tool branch required dragging the rmcp + russh runtime into the test path.
2. **No explicit abstraction over the lock-free state carriers.** `RunningCommand`, `RunningShell`, `RunningTransfer`, `SessionRef`, and `ForwardHandle` were leak-imported into both the storage layer and every tool that needed to observe them. Adding a new producer (e.g. distributed health probes, future REST gateway) meant either widening the public surface of those structs or duplicating the orchestration code.
3. **Static globals threaded through the codebase.** `SESSION_STORAGE`, `COMMAND_STORAGE`, `SHELL_STORAGE`, `TRANSFER_STORAGE`, and `SUBSCRIPTION_REGISTRY` were `OnceCell` singletons. Tests reset them via per-suite RAII guards, but the ergonomics broke when two tests wanted to compose different fakes.

The v4 roadmap explicitly required:

- Use cases (`ConnectSession`, `OpenShell`, `UploadFile`, `SubscribeResource`, …) testable in isolation with in-memory fakes and zero rmcp / russh / SFTP machinery.
- A wiring root that pins concrete adapters at compile time so wiring errors surface at `cargo build` rather than at runtime.
- A clear separation between the inbound MCP transport (`#[tool_router]` + `ServerHandler`) and the business logic, so a future REST gateway / gRPC façade / Tower middleware integration only writes a new infra layer instead of reworking every tool.
- Zero virtual-call overhead in the hot path — generic ports, never `Box<dyn Trait>`.

Two paths were on the table:

1. **Keep the v3 SOLID-light layout** and incrementally tighten it (introduce more traits, gradually pull state carriers behind interfaces, move markdown rendering into a separate module).
2. **Adopt full hexagonal (Ports and Adapters) architecture** with strict layer rules and a composition root.

## Decision

Adopt **full hexagonal architecture** for v4 with the following invariants:

- Six top-level layers under `src/`: `domain/`, `ports/`, `application/`, `adapters/`, `infra/`, `composition/`.
- Async ports declared via `#[trait_variant::make(Port: Send)]` so the trait surface offers both an AFIT (`async fn` in trait) variant for tests and a `Send`-bounded variant for production use cases.
- Use cases stay generic over their ports (`*UseCase<S: SshClientPort, SR: SessionRepository, …>`). No `Box<dyn Trait>`, no `async-trait` boxing, no `dyn` in the hot path.
- The composition root (`src/composition/prod.rs`) pins one concrete adapter per port via `type ConcreteX = …` aliases, then exposes `build_use_cases()` and `build_server()` for the two binary entry points. Wiring errors surface at the composition root.
- Inbound MCP transport lives entirely under `src/infra/mcp/` (`McpSshServer<UC>` generic over the use-case container, `tool_router.rs`, `resource_handlers.rs`, `args/`, `render/`, `helpers/`, `peer_handle.rs`).
- Public MCP API stays stable: same 18 tools, same 5 resource schemes, same wire format, same env vars. The change is internal.

### Static dispatch via generics + `trait-variant` (Option A)

We chose **static dispatch** through generic parameters over dynamic dispatch (`Box<dyn Trait>`):

- Zero virtual-call overhead in the hot path (PTY reads, command output writes, SFTP chunks).
- Wiring errors caught by rustc at the composition root.
- `cargo expand` shows monomorphised use cases per concrete adapter set, making profiling straightforward.

The drawback (compile time + binary size) is bounded because the composition root pins exactly one concrete adapter set per binary.

`trait-variant` was preferred over the older `async-trait` macro because:

- AFIT is stable since Rust 1.75 (we pin MSRV `1.85` for the Rust 2024 edition + extra AFIT polish).
- `trait-variant` generates two parallel trait surfaces — `LocalSshClientPort` (pure AFIT, used by tests with no `Send` bound) and `SshClientPort` (`Send`-bounded, used by use cases). Use cases stay generic over the `Send`-bounded variant; tests pick the local variant when convenient.
- No allocation per call (`async-trait` boxes every future).

### `async-trait` removal deferred to H17.6

The legacy `src/mcp/auth/*` chain stayed on `#[async_trait]` because the H8 refit landed under `src/adapters/auth/` while the v3 module remains runtime-active for the H17.6 cleanup window. The `async-trait` direct dependency disappears together with the foundational `mcp::*` modules in v4.1.

### Foundational `src/mcp/` deep decouple deferred to H17.6

The H17.5a hard-delete (commit `95ddc5b`) removed ~14k LOC of orphaned v3 modules: `src/mcp/{tools,storage,server,message,resources,schema,keys,forward}` (with `keys.rs` re-homed under `src/domain/keys.rs`).

The remaining `src/mcp/{async_command,client,config,error,session,sftp,shell,subscription,transfer,types,auth}` modules stay runtime-active in v4.0.0. The russh / SFTP / output / config adapters delegate into them rather than holding the lock-free state carriers (`RunningCommand`, `RunningShell`, `RunningTransfer`) directly. Absorbing those modules into the hexagonal layout (move state carriers under `adapters/<domain>/state/`, replace the global `SUBSCRIPTION_REGISTRY` with the `MemoryRegistry` adapter exclusively, delete the `mcp::*` namespace) is tracked under H17.6 for the v4.1 release.

The deferral was deliberate: H0 → H17 already constituted ~40 commits of refit; H17.6 would have added 10–15 more, blocking H18 verification (1021 lib tests pass) and the H19 documentation update. Shipping v4.0.0 with the foundational modules in place keeps the release cycle bounded while preserving every v3 lock-free invariant.

## Consequences

### Positives

- **Test isolation.** 1021 lib tests + 2 integration tests, the bulk of which run against in-memory fakes (`adapters::ssh::fake`, `adapters::sftp::fake`, `adapters::clock::fake`, `adapters::id_generator::deterministic`, `adapters::config::memory`). No live SSH, no real SFTP, deterministic IDs.
- **Composition isolation.** The two binaries (`ssh-mcp`, `ssh-mcp-stdio`) are thin shells over `composition::prod::run_{http,stdio}`; the wiring lives in one place and is identical between transports.
- **Layer boundary enforcement.** Every layer's allowed / forbidden imports are documented per module and policed by the strict lint baseline. The `domain` layer has zero runtime-crate imports.
- **Static dispatch on the hot path.** `cargo expand` confirms zero virtual-call boundaries between use cases and adapters.
- **Foundational cleanup runway.** H17.5a deleted ~14k LOC of orphaned v3 modules in a single commit. The remaining `mcp::*` modules have a clean H17.6 plan that does not block v4.0.0.

### Negatives

- **Boilerplate.** Adding a tool now spans ~9 files (domain entity, port surface, adapter implementation, use case, args struct, render module, tool_router entry, composition `UseCases<…>` extension, composition `prod.rs` wiring). v3 fit the same change into 1–2 files.
- **Compile time.** Generic monomorphisation across a large `UseCases<S, F, SR, CR, ShR, TR, [FR,] N, AS, OS, SubR, C, Cfg, Idg>` container increases `cargo build --release` from ~45s to ~70s on a warm cache.
- **Two registries during the transition window.** v4 use cases consume `MemoryRegistry`; the foundational producers still poke the v3 `SUBSCRIPTION_REGISTRY` global. Both stay in sync because the rmcp notifier adapter wraps the `Peer<RoleServer>` from the same `PeerTable`. H17.6 collapses this.
- **Mixed async-trait and trait-variant.** The legacy `mcp::auth` chain is the only surviving `#[async_trait]` site. Its replacement `src/adapters/auth/chain.rs` already uses AFIT; the direct dep deletion is gated on H17.6.

### Lock-free baseline preserved

Every v3 lock-free invariant survives:

```toml
await_holding_lock              = "deny"
await_holding_refcell_ref       = "deny"
significant_drop_in_scrutinee   = "deny"
significant_drop_tightening     = "deny"
mutex_atomic                    = "deny"
mutex_integer                   = "deny"
```

The lock-free state carriers (`RunningCommand`, `RunningShell`, `RunningTransfer`, `SessionRef`, `ForwardHandle`) keep zero `Mutex` fields. The new `adapters::repo::dashmap::*` repositories follow the v3 snapshot-then-drop-guard pattern. See [LOCKS.md](../LOCKS.md) for the v4 acquisition order, channel-capacity table, and decision tree.

### Public MCP API unchanged

Same 18 tools. Same 5 resource schemes. Same response markdown shape. Same env vars. Same capability handshake (`V_2025_06_18`, `tools.listChanged=true`, `resources.{subscribe,listChanged}=true`). Same default ports. A v3 host pointed at a v4 server sees no observable difference.

See [MIGRATION_v3_to_v4.md](../MIGRATION_v3_to_v4.md) for the contributor-facing change log.

## Alternatives considered

### A1. Keep the v3 SOLID-light layout

Rejected. The three structural ceilings listed in the [Context](#context) section all bind on the v3 layout: tight coupling, leaky state carriers, and static globals. Tightening v3 incrementally would have meant introducing the same layer concepts piecemeal, ending up with a half-hexagonal, half-monolith codebase. Doing the full migration in one window — albeit deferred for the foundational `mcp::*` modules — is cleaner.

### A2. Partial hexagonal — ports for the IO boundary only (rmcp + russh)

Rejected. Wrapping rmcp and russh behind ports without doing the same for repositories and the subscription registry would have left the `SESSION_STORAGE` / `COMMAND_STORAGE` / `SHELL_STORAGE` / `TRANSFER_STORAGE` / `SUBSCRIPTION_REGISTRY` globals in place. Those are the exact surfaces test isolation requires. A partial hexagonal solves a third of the pain points.

### A3. Dynamic dispatch (`Box<dyn Port>`)

Rejected. Two reasons: (a) virtual-call overhead on the PTY / command / transfer hot path; (b) wiring errors surface at runtime instead of at the composition root. Static dispatch via generics has higher compile cost but pays in correctness and profiling clarity. The boilerplate cost is identical (the same trait + impl pair lives either way); the only difference is whether the composition root binds via `Box<dyn ...>` or via `type Concrete<…> = …`.

### A4. Run H17.6 inside v4.0.0

Rejected. H17.6 (foundational decoupling — absorb the surviving `src/mcp/{async_command,client,config,error,session,sftp,shell,subscription,transfer,types,auth}` modules) was estimated at 10–15 commits with significant test surface impact. Folding it into v4.0.0 would have pushed the release window by 2–4 weeks and risked regressing the 1021 lib tests during the move. Shipping v4.0.0 with the foundational modules runtime-active and tracking H17.6 for v4.1 keeps the release cycle bounded; the public API stays identical regardless.

## References

### Hexagonal architecture chain (H0 → H18)

- **H0** — `5a70845` `chore(v4): H0 pin MSRV 1.85 + add trait-variant + axum 0.8`
- **H1** — `e96eca0` `feat(v4): H1 domain core + ports skeleton (trait-variant AFIT, static dispatch)`
- **H2** — `bf4b446` `feat(v4): H2 composition root scaffold + bin delegation`
- **H3a** — `9c48fbc` `feat(v4): H3a clock adapter (system + fake)`
- **H3b** — `a220897` `feat(v4): H3b config adapter (env + memory)`
- **H3c** — `32751ab` `feat(v4): H3c id_generator adapter (uuid + deterministic)`
- **H4** — `da5c88d` `feat(v4): H4 SessionRepository DashMap adapter (canary)`
- **H5a** — `69a9bb7` `feat(v4): H5a CommandRepository DashMap adapter`
- **H5c** — `63e76fb` `feat(v4): H5c TransferRepository DashMap adapter (re-add)`
- **H5d** — `abe8b2b` `feat(v4): H5d ForwardRepository DashMap adapter (feature-gated port_forward)`
- **H6** — `679f307` `feat(v4): H6 SshClientPort russh adapter`
- **H6.5** — `4e87758` `feat(v4): H6.5 OutputStreamPort russh adapter (unblocks H16)`
- **H7** — `3a83f4c` `feat(v4): H7 SftpClientPort russh-sftp adapter`
- **H8** — `a9ea90c` `feat(v4): H8 AuthStrategyPort chain refit (drop async-trait if reachable)`
- **H9** — `c5bd3af` `feat(v4): H9 MemoryRegistry + RmcpNotifier adapters (subscribe layer)`
- **H10** — `86d690e` `feat(v4): H10 ConnectSession use case (canary)`
- **H11a** — `f42cf00` `feat(v4): H11a DisconnectSession use case`
- **H11b** — `d22044c` `feat(v4): H11b ListSessions use case`
- **H11c** — `96b8808` `feat(v4): H11c DisconnectAgent use case`
- **H12a** — `2b05c8b` `feat(v4): H12a ExecuteCommand use case`
- **H12b** — `33b4477` `feat(v4): H12b GetCommandOutput use case`
- **H12c** — `a0571d0` `feat(v4): H12c ListCommands use case (recovered from stash)`
- **H12d** — `7149a54` `feat(v4): H12d CancelCommand use case`
- **H13a** — `07d9574` `feat(v4): H13a OpenShell use case`
- **H13b** — `8b0e366` `feat(v4): H13b WriteShell use case`
- **H13c** — `ceaea1f` `feat(v4): H13c SendKey use case`
- **H13d** — `5a1b0ee` `feat(v4): H13d ReadShell use case`
- **H13e** — `b612b24` `feat(v4): H13e WaitForPattern use case`
- **H13f** — `cd0b257` `feat(v4): H13f CloseShell use case`
- **H14** — `7591324` `feat(v4): H14 sftp-domain use cases (upload, download, get_transfer_progress)`
- **H15** — `b6f9178` `feat(v4): H15 tail use cases (forward + resources/* + peer_gc)`
- **H16** — `1c2c889` `feat(v4): H16 infra MCP layer wiring use cases + 18 tools + resources`
- **H17** — `c535992` `feat(v4): H17 infra MCP args + render migration + SFTP handle sharing`
- **H17.5a** — `95ddc5b` `chore(v4): H17.5a minimal hard-delete (orphaned v3 modules) + move keys.rs to domain` — ~14k LOC cleanup
- **H18** — `ddfbfeb` `test(v4): H18 verify v4 coverage + add smoke integration + fix axum 0.8 root-mount panic`

### External

- `trait-variant` macro: <https://github.com/rust-lang/impl-trait-utils>
- AFIT (async fn in traits) stabilisation: Rust 1.75 release notes.
- Hexagonal architecture (Alistair Cockburn, 2005): <https://alistair.cockburn.us/hexagonal-architecture/>

### Internal

- [ARCHITECTURE.md](../ARCHITECTURE.md) — full layer breakdown and module map.
- [LOCKS.md](../LOCKS.md) — lock-free invariants under the new layout.
- [MIGRATION_v3_to_v4.md](../MIGRATION_v3_to_v4.md) — contributor-facing migration guide.
- [adr/0001-migrate-to-rmcp.md](./0001-migrate-to-rmcp.md) — v3 transport choice (still in force).
