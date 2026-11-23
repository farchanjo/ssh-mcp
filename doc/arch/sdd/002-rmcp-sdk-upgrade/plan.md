# Implementation Plan: Rmcp Sdk Upgrade

## Overview

Move the MCP SDK (`rmcp` and `rmcp-macros`) from the 1.x line to 3.11.0 while
keeping the server on the `initialize` handshake. The server advertises
`2025-11-25` as its ceiling and serves every known revision up to it; a newer
or an unknown revision is answered `2025-11-25`. The change closes
RUSTSEC-2026-0462 and leaves the published wire identical except for those two
`initialize` answers. Variant A (`2025-06-18`) was kept recoverable and
measured against variant B (`2025-11-25`); B was chosen on the evidence in
`research.md` §6b.

The work is one dependency move plus its compile adaptations and the protocol
tests. It touches 15 source files, one manifest line and two new test files.
It adds no tool, no argument, no error code and no environment variable, and
no file under `scripts/` changes.

## Technical Approach

### 1. Manifest and resolution (FR-001, Q-4)

`Cargo.toml:74` changes `version = "1.6"` to `version = "3.11"`. The seven
features keep their names; `cargo info rmcp@3.11.0 --verbose` shows every one
still exists (`research.md` §2), so Q-4 needs no rename and no amendment. The
resolution is `rmcp 3.11.0` and `rmcp-macros 3.11.0`; `darling` moves
0.23.0 → 0.24.1 and `base64 0.23.1`, `tinyvec 1.13.3`,
`unicode-general-category 1.1.0` and `unicode-normalization 0.1.26` are added,
all transitive (`research.md` §3). No other direct requirement and no
toolchain pin changes.

### 2. Model and constructor adaptations (FR-003)

The renames are the 2.0/3.0 model renames from the migration guide §17 and the
`model` index pages. Mechanical, one for one:

| Before | After |
| --- | --- |
| `rmcp::model::Content` | `rmcp::model::ContentBlock` |
| `rmcp::model::Meta` | `rmcp::model::MetaObject` |
| `rmcp::model::PromptMessageContent` | `rmcp::model::ContentBlock` |
| `rmcp::model::PromptMessageRole` | `rmcp::model::Role` |
| `rmcp::model::RawResource` | `rmcp::model::Resource` |
| `rmcp::model::RawResourceTemplate` | `rmcp::model::ResourceTemplate` |
| `AnnotateAble::no_annotation()` | dropped; the field defaults to `None` |
| `ResourceUpdatedNotificationParam { uri }` | `ResourceUpdatedNotificationParam::new(uri)` |

`ContentBlock::Text` is a tuple variant, so the prompt test patterns become
`let ContentBlock::Text(content) = ... else` and read `content.text`. The
default `None` annotations keep the resources and templates on the same wire.

### 3. Response enums (FR-003)

`ServerHandler::read_resource` returns `ReadResourceResponse` and `get_prompt`
returns `GetPromptResponse`; both are `#[non_exhaustive]` enums whose `Complete`
variant carries the old result. Convert at the two return points:

```rust
// read_resource, rsync short-circuit
return outcome
    .map(|content| ReadResourceResult::new(vec![content]))
    .map_err(|e| resource_handlers::map_rsync_read_error(&e))
    .map(Into::into);
// read_resource, generic path
resource_handlers::read_resource_impl(/* ... */)
    .await
    .map(Into::into)

// get_prompt
prompts::get_prompt(&request.name, &args).map(Into::into)
```

`#[tool_handler]` keeps every tool method on `CallToolResult`; the macro wraps
it in `CallToolResponse::Complete`. No tool method changes.

### 4. Protocol version and negotiation (FR-004, FR-006, Q-2)

Today's binary echoes the requested revision, including `2026-07-28`, which it
does not implement (measured on both transports, `research.md` §6). The ceiling
chosen after measuring both variants is `2025-11-25`: it keeps every current
answer and changes only the `2026-07-28` and unknown-revision answers (`research.md` §6b). Add the ceiling
constant and the documented `known_up_to` list, and route the `initialize`
override through the documented `negotiate_initialize`:

```rust
const MAX_SUPPORTED_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2025_11_25;

fn supported_protocol_versions() -> &'static [ProtocolVersion] {
    ProtocolVersion::known_up_to(&MAX_SUPPORTED_PROTOCOL_VERSION)
}

fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
    Cow::Borrowed(supported_protocol_versions())
}

async fn initialize(
    &self,
    request: InitializeRequestParams,
    context: RequestContext<RoleServer>,
) -> Result<InitializeResult, McpError> {
    if context.peer.peer_info().is_none() {
        context.peer.set_peer_info(request.clone());
    }
    if let Some(registry) = self.capability_registry.as_ref() {
        record_inline_push_capability(registry, &self.peer_table, &context, &request);
    }
    self.negotiate_initialize(&request)
}
```

`get_info` advertises `.with_protocol_version(MAX_SUPPORTED_PROTOCOL_VERSION)`.
Measured answers: `2024-11-05`, `2025-03-26`, `2025-06-18` and `2025-11-25`
answer themselves; `2026-07-28`, `2024-10-07`, `1999-01-01`, `2027-01-01` and
`garbage` answer `2025-11-25`. An unknown revision is answered with the
`get_info` version; the chosen value is also the latest supported, which is
what the MCP lifecycle recommends: "If the server supports the requested
protocol version, it **MUST** respond with the same version. Otherwise, the
server **MUST** respond with another protocol version it supports. This
**SHOULD** be the *latest* version supported by the server." A discover client
that skips `initialize` is refused with `-32022` and `{requested, supported}`
on both transports. Q-2 is answered yes: 3.11 serves `2025-11-25` with
`initialize`, no amendment.

The embedded daemon client names one shared constant,
`EMBED_PROTOCOL_VERSION` (`src/embed/duplex_transport.rs:32`, line after the
change), at both sites (`src/embed/duplex_transport.rs:142`,
`src/embed/event_mux.rs:242`). Measured
on the base copy: `ProtocolVersion::default()` and `LATEST` are both
`V_2025_11_25` in `rmcp 1.8.0`, and both `embed_client_info()` and
`EmbedClient::default().get_info()` carry `2025-11-25` (`research.md` §14).
Naming it keeps today's value and stops an SDK default from moving it.

### 5. HTTP session configuration and the host allow-list (FR-005, Q-3)

`StreamableHttpServerConfig` is `#[non_exhaustive]`; build it from `Default` and
enable the legacy session mode explicitly. Nothing else is set:

```rust
let mut config = StreamableHttpServerConfig::default();
config.legacy_session_mode = true;
StreamableHttpService::new(
    || Ok(build_server()),
    Arc::new(LocalSessionManager::default()),
    config,
)
```

Measured: every initialize returns `Mcp-Session-Id`; the same id answers 200 and
a made-up id answers 404 (`research.md` §6-§7). `legacy_session_mode` only
applies below `2026-07-28`, which is exactly the negotiated range.

`allowed_hosts` exists in both SDK lines. The field docs say: "Allowed
hostnames or `host:port` authorities for inbound `Host` validation. By default,
Streamable HTTP servers only accept loopback hosts to prevent DNS rebinding
attacks against locally running servers. Public deployments should override
this list with their own hostnames." Measured on `rmcp 1.8.0` and on 3.11.0
with the default configuration: a loopback `Host` is served with 200, any
other `Host` is rejected with 403 and `Forbidden: Host header is not allowed`
(`research.md` §7). `allowed_origins` "Defaults to an empty list, which
disables Origin validation for backward compatibility"; an `Origin` header is
ignored in both builds.

The control therefore stays on. It is not made configurable here: the binary
binds `0.0.0.0` and advertises no hostname, so a request that arrives by name
or address is served only for a loopback `Host`. TM-001 records the current
state and `session.md` records making the list configurable as a later feature.

### 6. Daemon client subscribe path (FR-008, Q-1)

`Peer::subscribe` and `Peer::unsubscribe` are deprecated. `Peer::listen` is the
documented successor for 2026-07-28, but it diverts notifications away from
`ClientHandler`, which the NDJSON daemon depends on. Use the non-deprecated
generic request path instead and keep the mux callback:

```rust
// src/embed/dispatcher.rs
match client
    .peer()
    .send_request(ClientRequest::SubscribeRequest(Request::new(
        SubscribeRequestParams::new(uri.to_string()),
    )))
    .await
    .map_err(|err| err.to_string())
    .and_then(|result| expect_empty_result(&result, "subscribe"))
{
    Ok(()) => Event::Ack { /* ... */ },
    Err(err) => Event::Err { /* ... */ detail: Some(err) },
}

/// `resources/subscribe` and `resources/unsubscribe` answer with the empty
/// result. Any other variant is a protocol surprise; turn it into the same
/// error event the transport arm produces.
fn expect_empty_result(result: &ServerResult, op: &str) -> Result<(), String> {
    if matches!(result, ServerResult::EmptyResult(_)) {
        return Ok(());
    }
    Err(format!("{op}: unexpected response {result:?}"))
}
```

and the `UnsubscribeRequest` counterpart. The helper is covered by
`expect_empty_result_accepts_empty_and_reports_other_variants`. Proven by
`tests/v5_daemon_smoke.rs::t01_daemon_connect_exec_subscribe_drain`,
`::t04_daemon_idempotency_replay` and `scripts/test_v47_subscriptions.py`
(20/20) (`research.md` §5). Q-1 needs no amendment.

### 7. Tests added (FR-011)

New `tests/v7_protocol_negotiation.rs` spawns
`env!("CARGO_BIN_EXE_ssh-mcp-stdio")` and has two `#[test]` functions:
`initialize_answers_each_revision` (a table over `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25`, `2026-07-28`, `2024-10-07`, `1999-01-01`,
`2027-01-01`, `garbage`, asserting the FR-006 answer and the frozen capability
set from one helper) and
`discover_without_initialize_is_refused_with_unsupported_version` (`-32022`
with `{requested, supported}`). `request(message, id)` reads the id and compares
it with the response; `initialize(asked)` only builds the body.

New `tests/v7_protocol_http.rs` covers HTTP. It spawns
`env!("CARGO_BIN_EXE_ssh-mcp")` on a free port with `MCP_HOST=127.0.0.1` and
`MCP_PORT`, fails with the port named if the 15 s bind deadline passes, sets a
10 s read timeout, and speaks HTTP/1.1 over `std::net::TcpStream` (no new
dependency; `requests` is Python-only and no direct Rust HTTP client is
enabled). Tests:
`initialize_answers_each_revision_and_returns_a_session` (the same table, 200,
`Mcp-Session-Id` present),
`a_returned_session_is_served_and_a_made_up_id_is_refused` (200 with a
non-empty `tools` array / 404),
`a_non_loopback_host_is_refused_and_a_loopback_host_is_served` (403 with
`Forbidden: Host header is not allowed` / 200), and
`discover_without_initialize_is_refused_with_unsupported_version` (400,
`-32022`). The `post` helper takes the `Host` value.

Test lint pattern: neither new file carries an `#[allow]` — they use `expect`
with a message, as `tests/v5_smoke.rs` does. `tests/v4_smoke.rs` wraps tests
in `#[cfg(test)] mod tests`; the new files are plain integration-test files
like v5.

Unit tests: `supported_versions_stop_at_2025_11_25` pins the four-version
ceiling, `a_newer_client_is_negotiated_down_to_2025_11_25` pins the pure rule,
`capability_set_is_the_frozen_one` pins the serialised capability set,
`expect_empty_result_accepts_empty_and_reports_other_variants` covers the
dispatcher helper,
`embed_client_info_names_the_embedded_revision` and
`default_client_reports_the_embedded_revision` pin the daemon's own value.

### 8. Test edits allowed by FR-012

Only SDK renames, no assertion touched (`research.md` §13): `Meta` →
`MetaObject` at `src/infra/mcp/idempotency.rs:635`, `:687`, `:700`, `:706`,
`:714`; `PromptMessageContent` → `ContentBlock` and the tuple pattern at
`src/infra/mcp/prompts.rs:385`, `:446-631`; `Content` → `ContentBlock` at
`src/infra/mcp/tool_router.rs:4889`, `:4902`, `:4955`, `:4974`, `:4995`,
`:5015`. No existing `tests/` file is edited; the two new files are additions,
and no `#[allow]` is added.

## Answers to the Questions

- **Q-1** — `Peer::send_request(ClientRequest::{Subscribe,Unsubscribe}Request(
  Request::new(params)))`, the non-deprecated generic path that keeps
  `ClientHandler` delivery. Proven by the daemon tests. No amendment.
- **Q-2** — Yes. `known_up_to(V_2025_11_25)` serves every revision up to
  `2025-11-25` with `initialize`, and `negotiate_initialize` answers it. No
  amendment.
- **Q-3** — No effect: `notifications/ssh/output` still arrives, proven by
  `t_inline_push_full_handshake_to_byte_delivery` and the daemon round trip.
  No amendment.
- **Q-4** — No feature was renamed; all seven names exist. No amendment.

FR-006 was amended to name the measured ceiling `2025-11-25`; no other
amendment was needed.

## Wire Difference Table

| Capture | Result | Treatment |
| --- | --- | --- |
| `initialize` | identical | none |
| `tools/list` | identical | none |
| `resources/templates/list` | identical | none |
| `resources/list` | identical | none |
| `prompts/list` | identical | none |
| `prompts/get` | identical | none |
| `ssh_sessions` success | identical | none |
| `ssh_exec_output` domain error | identical | none |
| `ssh_exec` schema error | identical | none |

`scripts/export_tool_catalog.py --check`: `unchanged` for both files.

The nine captures use a client that asks for `2025-06-18`, so they cannot show
the protocol answer. Captured per requested revision, the only difference in
the whole surface is `/result/protocolVersion` on `initialize`:

| Client asks | Base | Chosen | Treatment |
| --- | --- | --- | --- |
| `2024-11-05` | `2024-11-05` | `2024-11-05` | none |
| `2025-03-26` | `2025-03-26` | `2025-03-26` | none |
| `2025-06-18` | `2025-06-18` | `2025-06-18` | none |
| `2025-11-25` | `2025-11-25` | `2025-11-25` | none |
| `2026-07-28` | `2026-07-28` (false echo) | `2025-11-25` | recorded in `docs/MIGRATION.md` |
| unknown (`2024-10-07`, `1999-01-01`, `2027-01-01`, `garbage`) | `2025-06-18` | `2025-11-25` | recorded in `docs/MIGRATION.md` |

Every other JSON path is identical in all cases. The two `protocolVersion`
answers are the authorized differences; both are listed and justified here and
in `docs/MIGRATION.md`.

## FR-015 Statement List

Reference rule: a `file:line` for code that exists before the change is the
repository line today; code the change adds is named by its symbol and, where a
line is given, the line is marked as after the change.

Each statement names an SDK version or explains a workaround by one. "True on
3.11.0" is judged against the docs; historical records stay as written.

| `file:line` | Statement | True on 3.11.0 | Correction |
| --- | --- | --- | --- |
| `Cargo.toml:85-86` | "rmcp 1.6 declares `axum = \"^0.8\"`, so we align on the same major" | no: `cargo tree -i axum` shows `axum v0.8.9` only under `ssh-mcp`, and `cargo tree -p rmcp --depth 1` shows `tower-service v0.3.3`, not axum | "axum 0.8 is this package's own pin (`cargo tree -i axum`); rmcp 3.11 depends on tower-service." |
| `src/infra/mcp/server.rs:22` | "The rmcp 1.5 `#[tool_router]` macro" | macro exists; version stale | "rmcp 3.11" |
| `src/infra/mcp/helpers/structured.rs:11` | "The rmcp 1.6 `CallToolResult` struct is `#[non_exhaustive]`" | yes, version stale | "rmcp 3.11" |
| `src/infra/mcp/tool_router.rs:3963-3964` | "matches the rmcp 1.6 builder surface at `.../rmcp-1.6.0/src/model.rs:1009-1056`" | version stale; the path names the registry source | "matches the rmcp 3.11 `Implementation` builder on docs.rs" |
| `src/ports/subscriber_registry.rs:143` | "rmcp 1.6 does not raise a peer-disconnect callback" | yes, version stale | "rmcp 3.11" |
| `src/adapters/notifier/rmcp_peer.rs:139` | same workaround | yes, version stale | "rmcp 3.11" |
| `src/adapters/subscription/legacy.rs:300` | same workaround | yes, version stale | "rmcp 3.11" |
| `src/adapters/subscription/legacy.rs:620` | same workaround | yes, version stale | "rmcp 3.11" |
| `src/embed/event_mux.rs:235` | "rmcp 1.6 marks ClientInfo + Implementation as `#[non_exhaustive]`" | types are `ClientConfig` now | "rmcp 3.11 marks `ClientConfig` + `Implementation` as `#[non_exhaustive]`" |
| `src/embed/duplex_transport.rs:49`, `:112`, `:118` | comments say `ClientInfo` and "rmcp 1.6" | type is `ClientConfig` | same correction |
| `docs/API.md:3` | "exposed by the v7.0 ssh-mcp server (rmcp 1.6, ...)" | version stale | "rmcp 3.11" |
| `docs/DEVELOPMENT.md:19` | "HTTP server (axum 0.8 + rmcp 1.6)" | version stale | "rmcp 3.11" |
| `docs/ARCHITECTURE.md:133` | "rmcp 1.6 does not expose a peer-disconnect callback" | yes, version stale | "rmcp 3.11" |
| `docs/ARCHITECTURE.md:785` | "Native via rmcp 1.6" | yes, version stale | "rmcp 3.11" |
| `docs/OPERATIONS.md:710` | "routed natively by rmcp 1.6" | yes, version stale | "rmcp 3.11" |
| `docs/OPERATIONS.md:773` | "rmcp 1.6 does not raise a callback" | yes, version stale | "rmcp 3.11" |
| `docs/CONFIGURATION.md:139` | "rmcp 1.6 does not raise a callback on disconnect" | yes, version stale | "rmcp 3.11" |
| `doc/arch/functional/functional.md:122` | "requests rmcp 1.6 at `Cargo.toml:74`, and the lockfile resolves 1.8.0" | no | "requests rmcp 3.11 at `Cargo.toml:74`, and the lockfile resolves 3.11.0" |
| `session.md:59-60` | RUSTSEC-2026-0462 stays open under item 2 | no | record the outcome in item 2 |
| `doc/arch/threat-model/threat-model.md:887`, `:889`, `:910-912` | RUSTSEC-2026-0462 open on `rmcp 1.8.0` | no | record the closure; keep `chrono` open |
| `docs/MIGRATION.md:9`, `:34`, `:234`, `:594`; `docs/adr/0001-migrate-to-rmcp.md:1`, `:68`; `doc/arch/sdd/001-*` | historical v3 records naming 1.6 | yes, as history | not changed |

## File Map

| File | Requirement |
| --- | --- |
| `Cargo.toml` | FR-001 |
| `Cargo.lock` | FR-001 (untracked, re-resolved) |
| `src/infra/mcp/helpers/structured.rs` | FR-003 |
| `src/infra/mcp/idempotency.rs` | FR-003 |
| `src/infra/mcp/prompts.rs` | FR-003, FR-007 |
| `src/infra/mcp/resource_handlers.rs` | FR-003, FR-007 |
| `src/infra/mcp/resource_templates.rs` | FR-003, FR-007 |
| `src/infra/mcp/tool_router.rs` | FR-003, FR-004, FR-006, FR-007, FR-011 |
| `src/infra/mcp/server.rs` | FR-015 (comment) |
| `src/adapters/notifier/rmcp_adapter.rs` | FR-003, FR-007 |
| `src/adapters/notifier/rmcp_peer.rs` | FR-015 (comment) |
| `src/adapters/subscription/legacy.rs` | FR-003, FR-007 |
| `src/embed/dispatcher.rs` | FR-008 |
| `src/embed/duplex_transport.rs` | FR-006, FR-008 |
| `src/embed/event_mux.rs` | FR-008, FR-015 (comment) |
| `src/ports/subscriber_registry.rs` | FR-015 (comment) |
| `src/composition/prod.rs` | FR-005 |
| `tests/v7_protocol_negotiation.rs` | FR-004, FR-006, FR-011 (new, stdio) |
| `tests/v7_protocol_http.rs` | FR-004, FR-005, FR-006, FR-011 (new, HTTP) |
| `CHANGELOG.md` | FR-015 |
| `docs/MIGRATION.md` | FR-015 |
| `docs/API.md`, `docs/DEVELOPMENT.md`, `docs/ARCHITECTURE.md`, `docs/OPERATIONS.md`, `docs/CONFIGURATION.md` | FR-015 |
| `doc/arch/threat-model/threat-model.md` (TM-001, TM-021) | FR-015 |
| `doc/arch/functional/functional.md` | FR-015 |
| `session.md` | FR-015 |
| `doc/arch/sdd/002-rmcp-sdk-upgrade/{spec,plan,research}.md`, `doc/arch/specs/features/rmcp-sdk-upgrade.feature`, `doc/arch/adr/0002-rmcp-sdk-upgrade.md`, `doc/arch/schemas/rmcp-sdk-upgrade.cue` | feature record |

Anything else is out of scope.

## Verification Plan

Run in order; each must pass before the next.

1. `cargo fmt --all -- --check`
2. `cargo check --all-features --all-targets`
3. `cargo clippy --all-features -- -D warnings`
4. `cargo build --no-default-features --target-dir target/no-default-features`
5. `cargo build --bins`
6. `cargo nextest run --test-threads num-cpus --lib` (reference 2116 plus the
   six unit tests added here = 2122)
7. `cargo nextest run --test-threads num-cpus --tests --features test-fixtures`
   (reference 2283 plus the six unit and six integration tests = 2295)
8. `cargo audit` (no vulnerability; `chrono` warning allowed)
9. `make tools-export-check` (`unchanged`, both files)
10. From the repository root:
    `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py
    scripts/test_transfer_progress.py scripts/test_v7_rsync_http.py
    scripts/test_v47_subscriptions.py scripts/test_v47_idempotency.py`
    (55 passed, 1 skip). The system `python3` lacks `pytest` and `requests`.
11. `speckit validate` (70 errors, baseline set, no warning, no feature finding)
12. `speckit verify --changed` (no `failed` line)

Reviewer: diff against `/tmp/ssh-mcp-rmcp-spike.patch` (18 files, 649
insertions, 135 deletions) and check that no assertion moved. The patch is the
whole change to `Cargo.toml`, `src/` and `tests/`; the `docs/`, `session.md`
and `doc/arch/` documentation changes below are applied on top of it.

## Documentation Changes

`CHANGELOG.md` gets an Unreleased entry naming RUSTSEC-2026-0462 and the move;
`docs/MIGRATION.md` gets a v7.1.3 → unreleased paragraph carrying the FR-006
table, stating that the only `initialize` differences are the `2026-07-28`
answer and the unknown-revision answer, both now `2025-11-25`, that a newer
client is refused with `-32022` when it skips `initialize`, that the legacy
session mode and its 3.x name keep `Mcp-Session-Id` working, and that the
loopback `Host` rule is unchanged. TM-021 records the
`rmcp` half closed and `chrono` still open. TM-001 records, as a measured fact,
that the HTTP transport validates `Host` against a loopback-only list (quoting
what the documentation says the control protects against, DNS rebinding) and
answers 403 to any other `Host`; no line changes it. `session.md` item 2
records the outcome, and the open items under the HTTP default exposure record
that the server binds `0.0.0.0` and serves only a loopback `Host`, that no
project document says so, and that making the list configurable is a later
feature. The FR-015 table is applied.

## Constitution

- **D-01 spec first** — the change follows the specified, clarified artifacts
  and the measurements in `research.md`.
- **D-02 dogfooding** — `speckit validate` and `verify --changed` close the
  loop; no rule is weakened.
- **D-03 layering** — the edits stay in `adapters`, `embed`, `infra` and
  `composition`; no import crosses inward.
- **D-04 DDD** — the CUE schema keeps value-object roles.
- **D-05 calisthenics** — no new type, field or branch exceeds the existing
  shape.
- **D-06 FQCN** — no `super::` is added to production code, and the
  `fqcn.relative-super` finding count stays at 46 (FR-014).
- **D-07 lints and pinning** — clippy runs with `-D warnings`; no `#[allow]`
  is added, in production or in the new tests.
- **D-08 fail loud** — no `unwrap`/`expect` is added to production; the tests
  use `expect` with a message.
- **D-10 guard-mediated writes** — the implementation uses the normal file
  writes; no guard bypass.
- **D-11 stable contracts** — the wire is identical except the two listed
  `protocolVersion` answers (measurement i).
- **D-12 small changes** — one dependency concern, no unrelated bump.
- **D-13 offline** — the build stays local; no runtime network path is added.
- **D-17 dependency surface** — the manifest requirement moves and the lock
  re-resolves; the lock stays untracked, as before.
- **P-01 inward dependencies** — no adapter calls another adapter.
- **P-02 lock-free hot paths** — no shared state type changes.
- **P-03 panic-free and justified suppressions** — no `#[allow]` is added.
- **P-04 frozen wire** — all nine captures are identical; the two
  `protocolVersion` answers are the only listed differences.
- **P-05 additive wire** — no tool, argument, error code or environment
  variable is added or edited.
- **P-06 verified-change gate** — the full gate is the verification plan.
- **P-07 dev profile** — no release build.
- **P-08 commits and decisions** — ADR 0002 records the decision; the owner
  commits.
- **P-09 en-US** — every artifact is en-US.

## Rollback

Revert the change as one unit: restore `Cargo.toml:74` to `"1.6"`, run
`cargo update -p rmcp -p rmcp-macros`, and restore the 13 source files and the
two new test files from the patch. The lock re-resolves to 1.8.0; no data or
schema migration is needed because the surface never moved.

## Open Decisions

None. Q-1 to Q-4 are answered, and neither amendment condition of the
specification fired.
