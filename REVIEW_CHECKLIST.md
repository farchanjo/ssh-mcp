# Review Checklist

This checklist is the gate every pull request must satisfy before it is merged. It encodes the architectural invariants from [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), the lock-free contract from [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#lock-free-invariants), and the wire-stability promise from [CHANGELOG.md](CHANGELOG.md). Reviewers apply each item; authors should self-review against it before requesting review.

## 1. Build, lint, tests

- [ ] `cargo build --release --all-features` succeeds clean.
- [ ] `cargo build --release --no-default-features` succeeds clean (no `port_forward`).
- [ ] `cargo fmt --all -- --check` passes.
- [ ] `cargo clippy --release --all-features --all-targets --workspace -- -D warnings` passes.
- [ ] `cargo test --lib --quiet` passes (1966 tests).
- [ ] `cargo test --tests --features test-fixtures --quiet` passes (134 tests).
- [ ] No `#[allow(...)]` added without a `reason = "..."` justification.
- [ ] No `unwrap()` / `expect()` outside `#[cfg(test)]` modules.
- [ ] No `Mutex` / `RwLock` added on the hot path (`RunningCommand`, `RunningShell`, `RunningTransfer`, `SessionRef`, `ForwardHandle`, `ResourceLifecycle`, `SessionLifecycle`, `MultiplexLane`, `ChannelMux`, rsync transport state). Use `DashMap`, `ArcSwap`, `Atomic*`, `mpsc`, `broadcast`, `Notify` instead.

## 2. Architecture

- [ ] Layer boundary preserved: `domain → ports → application → adapters → infra → composition`. Domain has no async, no Tokio, no I/O. Ports are pure traits. Adapters never call other adapters directly — they cross through the use case.
- [ ] Use cases are generic over their ports — no `Box<dyn Trait>` on the hot path. Cold-path slices may use a dyn-safe variant (e.g. `LaneAdmin`) but it must live alongside the AFIT slice.
- [ ] No new wildcard match arm (`_ =>`) on a closed enum (`wildcard_enum_match_arm = "deny"`).
- [ ] `Arc::clone(&x)` is used everywhere — never `x.clone()` on an `Arc`.
- [ ] Methods stay under 30 lines. SOLID respected. New surface uses Facade / Builder / Fluent API where it crosses module boundaries.

## 3. MCP wire format

- [ ] Tool response keeps the v3 / v4 / v5 markdown envelope: first line `TOOL_NAME: STATUS`; one `KEY: value` per line; IDs end in `_ID`; output blocks use the 8-hex-char nonce with `--- name [nonce] ---` header.
- [ ] `structured_content` JSON twin is byte-equivalent to the markdown payload (key parity verified by snapshot test).
- [ ] Error responses include all three lines: `TOOL_NAME: ERROR`, `REASON: [CODE] description`, `DETAIL: <single sentence>`.
- [ ] Any new error code is added to [docs/adr/0007-error-taxonomy.md](docs/adr/0007-error-taxonomy.md), is mapped to one of the seven categories (`AUTH` / `TRANSPORT` / `REMOTE` / `RESOURCE` / `POLICY` / `STATE` / `INTERNAL`), and has retry semantics declared.
- [ ] Any new tool has a `When: / Push: / Cleanup: / Cost: / Idempotency: / Hygiene:` block at the end of its description and an entry in the `NEXT:` chain of upstream tools.

## 4. Subscription / lifecycle

- [ ] Any new long-lived resource (shell, command, transfer, forward, rsync session, serial port) is wrapped in a `ResourceLifecycle` with CAS state machine (`Owned → Observed → Releasing → Closed`), refcount cascade through `SessionLifecycle.active_refs`, and grace-timer arming on last unsubscribe.
- [ ] New push streams emit `notifications/resources/updated` through `LaneFanoutBridge` (HTTP / stdio) and through the channel-mux outbound sink (NDJSON daemon). Per-lane atomics (`events_sent`, `bytes_sent`) increment on every successful drain.
- [ ] `release_when_no_subs` defaults preserve v4 / v5 / v6 semantics (default `false` — opt-in only).

## 5. Documentation

- [ ] [docs/API.md](docs/API.md) updated for any new tool / argument / response field.
- [ ] [docs/RESOURCES.md](docs/RESOURCES.md) updated for any new push scheme or cursor change.
- [ ] [docs/CONFIGURATION.md](docs/CONFIGURATION.md) updated for any new environment variable.
- [ ] [docs/MIGRATION.md](docs/MIGRATION.md) updated for any wire-visible change.
- [ ] [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md) updated when the error handbook or push-first prompts change.
- [ ] [CHANGELOG.md](CHANGELOG.md) carries a Conventional Commits entry under the unreleased section, scoped correctly (`feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `build`).
- [ ] If the change introduces a new architectural decision, a new ADR is added under [docs/adr/](docs/adr/) following the MADR 4.0 template and the next sequential number (never reused).

## 6. Tests

- [ ] New behaviour has at least one of: a unit test under `src/` (preferred), an integration test under `tests/`, a proptest invariant, a chaos scenario, or a loom invariant.
- [ ] Snapshot tests under `tests/v4_smoke.rs` / `tests/v5_smoke.rs` / `tests/v6_resume_smoke.rs` / `tests/v7_rsync_smoke.rs` stay green — the wire format must remain byte-identical for legacy callers.
- [ ] Python integration suites (`scripts/test_*.py`) updated when the MCP-visible surface changes.

## 7. Security & privacy

- [ ] No secrets, tokens, or private keys committed (verified with `git diff --cached`).
- [ ] No `println!` / `eprintln!` / `dbg!` left in production code (`print_stdout` / `print_stderr` / `dbg_macro` are `forbid`).
- [ ] Any change that touches authentication, authorization, or transport encryption carries an explicit reviewer ack from a maintainer listed in [MAINTAINERS](MAINTAINERS).

## 8. Commits

- [ ] Commits follow the Angular Conventional Commits format: `<type>(<scope>): <subject>`.
- [ ] Subject is ≤50 chars; body explains the **why** when not obvious.
- [ ] No bundled commit covering more than one logical change — break into small contextual commits per [CLAUDE.md](CLAUDE.md).
- [ ] Branch name matches the change: `feat/<scope>`, `fix/<scope>`, `docs/<scope>`, etc.

---

When a reviewer cannot tick a box, the PR is blocked until either the code is changed or the box is explicitly waived in writing on the PR thread.
