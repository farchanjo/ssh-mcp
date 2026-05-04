# Contributing to ssh-mcp

Thanks for your interest in ssh-mcp. This guide is for contributors sending patches, issues, or proposals to the [farchanjo/ssh-mcp](https://github.com/farchanjo/ssh-mcp) repository. We welcome bug reports, performance fixes, doc improvements, new tests, and architectural proposals via ADR. By participating you agree to act in good faith, keep discussion technical, and assume good intent from reviewers and other contributors.

## Before you start

- Read [CLAUDE.md](CLAUDE.md) for the project map, build commands, and the lock-free / use-case-generic invariants the codebase enforces.
- Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the hexagonal layer contract (domain / ports / application / adapters / infra / composition) and the dependency direction rule.
- Read [docs/adr/0002-adopt-hexagonal-architecture.md](docs/adr/0002-adopt-hexagonal-architecture.md) for the architectural invariants and the rationale you must respect when crossing layers.
- Read [docs/LOCKS.md](docs/LOCKS.md) before touching anything on the hot path (`RunningCommand`, `RunningShell`, `RunningTransfer`, `SessionRef`, `ForwardHandle`, subscription registries).
- All persisted artifacts are en-US: code, comments, identifiers, log lines, commit messages, ADRs, PR descriptions, and branch names. Chat replies follow the user's language; the repo does not.

## Pre-commit gates (must pass)

The four canonical gates that every PR must pass locally before review. CI runs the same set; failing any of them blocks merge.

```bash
cargo build --release --all-features
cargo fmt --all -- --check
cargo clippy --release --all-features --all-targets --workspace -- -D warnings
cargo test --lib --quiet
```

The `-D warnings` flag is non-negotiable: see the strict lint baseline declared in [`Cargo.toml`](Cargo.toml) and summarised in [CLAUDE.md](CLAUDE.md). Never silence a lint with `#[allow(...)]` to make CI green; fix the code instead. Every legitimate `#[allow(...)]` carries a `reason = "..."` (enforced by `clippy::allow_attributes_without_reason`).

For larger refactors, also run `cargo test --tests --quiet` (integration + v4 smoke) and, when the change touches the application layer, `cargo test --features test-fixtures`. See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the full developer loop including coverage and stress-test scripts.

## Code style

- Methods stay under 30 lines (`too-many-lines-threshold = 30` in `clippy.toml`); cognitive complexity stays under 25. Refactor before suppressing.
- `Arc::clone(&x)` instead of `x.clone()` on an `Arc` (enforced by `clippy::clone_on_ref_ptr`).
- Match exhaustively on closed enums — no `_ =>` catch-alls (`clippy::wildcard_enum_match_arm`).
- No `unwrap` / `expect` / `panic!` / `todo!` / `unimplemented!` / `dbg!` / `print*!` outside of tests (Layer A forbids).
- Use cases live in `src/application/` and are generic over their port traits. Adapters live in `src/adapters/` and implement those traits. Domain types in `src/domain/` are pure data — no I/O, no async.

## Commit message format

Angular Conventional Commits: `<type>(<scope>): <subject>`.

- **Types** (lowercase): `feat`, `fix`, `docs`, `test`, `refactor`, `chore`, `perf`, `build`, `ci`.
- **Scope**: a short module or area (`sftp`, `shell`, `tool_router`, `composition`, `subscription`, `adr`, `release`).
- **Subject**: imperative mood ("add transfer progress watcher", not "added transfer progress watcher"); no trailing period; under 72 characters.
- **Body**: wrap at 72 columns; explain *why*, not *what* (the diff already shows the *what*).
- **Footer**: include a `Co-Authored-By:` trailer when the change was AI-assisted, and a `Refs: #NNN` / `Closes: #NNN` when an issue exists.

Break work into small, contextual commits — never `git add -A` a multi-feature blob. One logical change per commit, tests in the same commit as the feature.

## PR checklist

- [ ] Tests ship in the same commit as the feature or fix (lib, integration, or both).
- [ ] `cargo fmt --all -- --check` and `cargo clippy --release --all-features --all-targets --workspace -- -D warnings` are green locally.
- [ ] No `Mutex` / `RwLock` introduced on the hot path — atomics, `ArcSwap`, `DashMap`, `OnceCell`, `tokio::sync::broadcast`, or `tokio::sync::Notify` only. See [docs/LOCKS.md](docs/LOCKS.md).
- [ ] Use cases stay generic over their ports (no `Box<dyn Trait>` in hot paths, no leak of rmcp / russh / SFTP types into `application/`).
- [ ] New env vars get a row in [docs/CONFIGURATION.md](docs/CONFIGURATION.md) and a floor / cap in `src/adapters/config/internal/mod.rs`.
- [ ] New error codes get a row in [docs/llm-ux/ERROR_HANDBOOK.md](docs/llm-ux/ERROR_HANDBOOK.md) and a structured variant in `src/domain/error.rs`.
- [ ] New ADRs follow MADR 4.0 (status, context, decision drivers, considered options, decision outcome, consequences) and live under `docs/adr/NNNN-<kebab-title>.md`. ADR numbers are never reused.
- [ ] Public MCP tool wire shape (markdown body) stays byte-compatible with v3 / v4 unless the change is gated behind a new tool or opt-in flag. Snapshot tests in `tests/v4_smoke.rs` must still pass.
- [ ] Hot-path changes touching atomics or `ArcSwap` ship with a loom invariant in `tests/lockfree_invariants.rs` (gated `#[cfg(loom)]`).

## Architecture invariants (cheat sheet)

| Invariant | Canonical doc |
|---|---|
| Lock-free hot path (no `Mutex` on `Running*` state) | [docs/LOCKS.md](docs/LOCKS.md) |
| Hexagonal layer dependency direction (domain knows nothing; adapters depend inward) | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Use cases generic over ports — static dispatch, no `Box<dyn>` in hot paths | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Strict clippy baseline (`pedantic` + `nursery` + `cargo` at deny; Layer A forbids) | [`Cargo.toml`](Cargo.toml) `[lints.clippy]`, [CLAUDE.md](CLAUDE.md) |
| ADR for any decision spanning more than one layer or changing a port surface | [docs/adr/](docs/adr/) |
| Subscribe-first push delivery for `shell://`, `command://`, `transfer://`, `forward://` | [docs/RESOURCES.md](docs/RESOURCES.md) |
| Wire compatibility with v3 / v4 hosts on the legacy 21-tool catalogue | [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) |

## Test layers

| Command | Scope | Notes |
|---|---|---|
| `cargo test --lib --quiet` | 1156 unit tests across `domain`, `application`, `adapters`, `infra` | Default fast loop. |
| `cargo test --tests --quiet` | Integration tests including the v4 smoke (`tests/v4_smoke.rs`) | Snapshot-checks the markdown wire shape. |
| `cargo test --features test-fixtures` | Use cases against deterministic in-memory adapters (`FakeClock`, `DeterministicIdGenerator`) | Add this when authoring application-layer tests. |
| `cargo test --features port_forward` | Toggles the `ssh_forward` tool and `forward://` resource | Keep parity for both feature combinations. |
| `cargo +nightly test --cfg loom` (gated) | Loom invariants in `tests/lockfree_invariants.rs` | Currently blocked by upstream tokio/loom incompatibility — see the test file header. |

Python integration suites under `scripts/test_*.py` and stress scripts under `scripts/stress_*.py` are optional locally but run on release branches. Ad-hoc developer workflow lives in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Dependency policy

Adding a dependency is a design decision. Prefer the standard library, then `tokio` / `russh` / `rmcp` / `axum` (already pinned), then a vetted crate from the existing tree. A new direct dep needs (a) a clear use case that cannot be satisfied by what's already pulled in, (b) an active maintainer and recent release, (c) an MIT- or Apache-2.0-compatible license, and (d) a sentence in the PR description explaining the choice. Avoid pulling in `async-trait` — the codebase uses native AFIT through `trait-variant` for async ports.

Vendor (copy into-tree under a clear license header) only when the upstream is unmaintained, the surface is small, and the alternative is a transitive bloat. Supply chain: the `multiple_crate_versions` clippy lint is allowed because of transitive duplication from `russh` / `axum`, but new direct duplicates require justification. We do not yet run `cargo deny` or `cargo audit` in CI; running them locally before a security-sensitive PR is encouraged, and proposing a CI lane for either is welcome.

## When opening a PR

1. Branch name format: `<type>/<short-kebab-summary>` — for example `feat/v5-foundation`, `fix/sftp-progress-watcher`, `docs/contributing-guide`.
2. Each commit follows the Angular format above; squash trivial fixups before pushing.
3. Link the issue or ADR the PR resolves (`Closes: #123` in the description).
4. Open the PR against `master` and request review from a maintainer.
5. CI (build + fmt + clippy + lib tests) must be green before review starts; flaky failures get re-run, deterministic failures get fixed.
6. Address every reviewer comment with a follow-up commit (or a clear "won't fix" with rationale) before merge. Do not force-push over review history once a review has started.

## Reporting bugs

Good bug reports save round-trips. Include:

- A minimal reproduction (host config, MCP client, exact tool call, env vars).
- The block-markdown response you observed (paste the `KEY: value` lines and any `--- stdout [nonce] ---` block verbatim).
- The crate version (`Cargo.toml`) and binary (`ssh-mcp` HTTP, `ssh-mcp-stdio`, or the v5 NDJSON daemon).
- Logs at `RUST_LOG=ssh_mcp=debug,russh=info` for the affected window.
- For deadlocks or stalls: a `cargo build` SHA and any loom invariant that fires locally.

Security-sensitive issues (auth bypass, key leakage, RCE-class concerns): email the author directly instead of opening a public issue. See [docs/llm-ux/ERROR_HANDBOOK.md](docs/llm-ux/ERROR_HANDBOOK.md) for the full taxonomy of error codes the server emits.

## Where to ask questions

- Issues: <https://github.com/farchanjo/ssh-mcp/issues> for bugs, missing tools, or doc gaps.
- Discussions: <https://github.com/farchanjo/ssh-mcp/discussions> for design questions, "how do I", and proposals before they become ADRs.
- Author: Fabricio Archanjo · <fabricio@archanjo.com> for sensitive or coordinated-disclosure topics.
