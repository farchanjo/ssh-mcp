---
id: 01a2017d-197d-7863-b8ca-b738b16023dc
number: 002
slug: rmcp-sdk-upgrade
status: implemented
created_at: 2026-11-22T04:21:45.469669854Z
---
# Feature Specification: Rmcp Sdk Upgrade

Feature: 002-rmcp-sdk-upgrade
Created: 2026-11-22

Every fact below is measured on the date it names and names the file and line
that carries it, unless the sentence says the fact comes from upstream
documentation and was not measured here. The plan phase re-measures every line
on the tree it receives, because the working tree moves under both features.

## User Stories

- As the maintainer of the MCP transport, I want the process on an `rmcp`
  release that carries no published advisory, so that RUSTSEC-2026-0462, an
  unbounded response body in the Streamable HTTP client, is not reachable from
  the network.
- As an operator of an existing MCP host, I want the wire to stay exactly as it
  is today, so that the SDK move is a binary swap and not a host migration.
- As a reviewer, I want the server to keep the `initialize` handshake, so that
  the per-session state the handler builds stays valid and no request reaches a
  handler that has lost its session.
- As a maintainer, I want no deprecated SDK API and no silenced lint, so that
  the move lands complete instead of leaving a compatibility debt the next
  change has to pay.

## Out of Scope

- Support for MCP protocol revision 2026-07-28: `subscriptions/listen`, the
  stateless lifecycle, and one fresh handler per request.
- MCP tasks, MCP skills, elicitation and Claude Code channels.
- Every other dependency bump (`md-5`, `sha2`, `base64`, `tower-http`), the
  defect list, and the conformity work of items 3 to 6 in the update plan of
  `session.md:70-79`.

## Functional Requirements

### FR-001 The manifest and the lock resolve `rmcp` 3.x at or above 3.11.0

`Cargo.toml:74-82` asks for `rmcp = { version = "1.6", default-features = false,
features = ["server", "client", "macros", "schemars", "transport-io",
"transport-async-rw", "transport-streamable-http-server"] }`, and the current
`Cargo.lock` resolves `rmcp 1.8.0` (`Cargo.lock:1986-1987`) and `rmcp-macros
1.8.0` (`Cargo.lock:2016-2017`). The requirement becomes the `rmcp` 3.x line at
or above 3.11.0, the latest release. The feature set stays the same; where the
3.x line renamed a feature, the 3.x name is used and the rename is recorded in
`docs/MIGRATION.md`.

The move needs no toolchain change. `cargo info rmcp@3.11.0` gives
`rust-version: 1.88` (measured 2026-11-22), the project pins toolchain 1.99.1
(`rust-toolchain.toml:2`) and `rust-version = "1.99"` (`Cargo.toml:16`).

**Acceptance.** `Cargo.toml` declares the `rmcp` 3.x line at or above 3.11.0;
the resolved `Cargo.lock` carries `rmcp` 3.11.0 or a later 3.x; every feature
the manifest enables today is still enabled under its 3.x name; no other direct
dependency requirement changes; the toolchain pin and `rust-version` are
unchanged.

### FR-002 `cargo audit` reports no vulnerability on the resolved set

`cargo audit` measured on 2026-11-22, after feature 001, reports one
vulnerability: RUSTSEC-2026-0462 on `rmcp 1.8.0`, "Unbounded response body
buffering in Streamable HTTP clients", severity 6.5, fixed in `>= 3.9.0`. It is
the open half of entry TM-021 of `doc/arch/threat-model/threat-model.md`
(`:907-912`). The unmaintained `chrono 0.4.45` warning under RUSTSEC-2026-0543
stays, because no fixed upgrade exists.

**Acceptance.** `cargo audit` on the moved set prints no vulnerability, and the
only remaining finding is the `chrono` unmaintained warning under
RUSTSEC-2026-0543, which is reported as still open.

### FR-003 The crate compiles with the three error classes adapted

A compile probe of the bump on 2026-11-21, in a throwaway copy with
`rmcp = "3.11"` and `cargo check --all-features --all-targets`, stopped with 14
error lines in 8 files, of three kinds:

- `E0432`, unresolved imports of model types renamed in the 2.0 line (`Content`,
  `Meta`, `PromptMessageContent`, `PromptMessageRole`, `AnnotateAble`,
  `RawResource`, `RawResourceTemplate`) in
  `src/infra/mcp/helpers/structured.rs`, `src/infra/mcp/idempotency.rs`,
  `src/infra/mcp/prompts.rs`, `src/infra/mcp/resource_handlers.rs`,
  `src/infra/mcp/resource_templates.rs` and `src/infra/mcp/tool_router.rs`;
- `E0639` on `ResourceUpdatedNotificationParam { uri }` built as a struct
  literal at `src/adapters/notifier/rmcp_adapter.rs:69` and
  `src/adapters/subscription/legacy.rs:606`;
- `E0271` because `read_resource` (`src/infra/mcp/tool_router.rs:4233`) and
  `get_prompt` (`:4292`) must return the response enums of the 3.0 line.

The probe stopped at the library, so the binaries and the integration tests
were not reached and the list is a lower bound.

**Acceptance.** `cargo check --all-features --all-targets` exits 0; the
adaptations stay inside those three classes plus whatever the same check
surfaces beyond the library; no behaviour changes beyond FR-007. A test edit is
allowed only where the SDK renamed or reshaped an item the test names; each
such edit is listed in the plan with its `file:line`; no assertion is weakened,
removed or made conditional.

### FR-004 The server stays on the `initialize` handshake, and a 2026-07-28 client is negotiated down or refused

The release notes of `rmcp 3.0.0` state that revision 2026-07-28 removes
`initialize` and protocol-level HTTP sessions, replaces `resources/subscribe`
and `resources/unsubscribe` with `subscriptions/listen`, and creates a fresh
handler per request; the SDK keeps the earlier APIs "for legacy protocol
sessions" (upstream documentation, not measured here). This server builds all
of its state inside the handler: `build_server`
(`src/composition/prod.rs:670-671`) runs the whole wiring, and the HTTP service
builds one server per session through a factory
(`src/composition/prod.rs:750-756`).

The server therefore does not serve the stateless lifecycle. A client that asks
for 2026-07-28 is either negotiated down to a revision the server serves with
`initialize`, or refused with a protocol error. It never reaches a handler that
has lost its session state. A second reason for the choice is recorded: Claude
Code does not register as a channel a server that negotiates 2026-07-28
(`https://code.claude.com/docs/en/mcp`, section "Push messages with channels",
upstream documentation, not measured here), which matters for the channel
support planned later.

Today's release is wrong about this and the move exposes it. The `rmcp 1.8.0`
binary echoes whatever revision the client asks for, including `2026-07-28`, a
revision it does not implement: measured on stdio and on HTTP on 2026-11-22
(`research.md` §6). That false echo is the reason this requirement exists.

**Acceptance.** A test proves which of the two outcomes the server produces for
a 2026-07-28 client, on stdio and on HTTP. The HTTP test spawns the HTTP binary
on a free port; the stdio test spawns the stdio binary. A discover request that
skips `initialize` is refused with `-32022` and `{requested, supported}` on
both transports.

### FR-005 The HTTP transport keeps protocol-level sessions, so `Mcp-Session-Id` keeps working

The HTTP transport creates a protocol-level session today through
`LocalSessionManager` and `StreamableHttpServerConfig::default()`
(`src/composition/prod.rs:750-756`). The 3.x line keeps that mode as the legacy
session mode; the 3.0.0 release notes record that `stateful_mode` is renamed
`legacy_session_mode` (upstream documentation, not measured here). The session
mode is enabled explicitly, not inherited from a default that may move.

**Acceptance.** An `initialize` over HTTP creates a session, the reply carries
`Mcp-Session-Id`, a second request that presents the header is served (200) and
a request with a made-up id is refused (404). The setting and its 3.x name are
recorded in `docs/MIGRATION.md`. A test in the HTTP test file of FR-011 proves
the three facts.

### FR-006 The protocol ceiling is `2025-11-25`, and the measured answer is fixed per requested revision

`get_info` advertises `ProtocolVersion::V_2025_06_18`
(`src/infra/mcp/tool_router.rs:4184`) and the server echoes the requested
revision today. Two ceilings were measured against the base binary on
2026-11-22 (`research.md` §6):

| Requested | Base 1.8.0 | Ceiling `2025-06-18` | Ceiling `2025-11-25` |
| --- | --- | --- | --- |
| `2024-11-05` | `2024-11-05` | `2024-11-05` | `2024-11-05` |
| `2025-03-26` | `2025-03-26` | `2025-03-26` | `2025-03-26` |
| `2025-06-18` | `2025-06-18` | `2025-06-18` | `2025-06-18` |
| `2025-11-25` | `2025-11-25` | `2025-06-18` | `2025-11-25` |
| `2026-07-28` | `2026-07-28` (false) | `2025-06-18` | `2025-11-25` |
| `2024-10-07` (unknown) | `2025-06-18` | `2025-06-18` | `2025-11-25` |
| `1999-01-01` (unknown) | `2025-06-18` | `2025-06-18` | `2025-11-25` |
| `2027-01-01` (unknown) | `2025-06-18` | `2025-06-18` | `2025-11-25` |
| `garbage` (unknown) | `2025-06-18` | `2025-06-18` | `2025-11-25` |

A revision the server does not know is answered with the version
`get_info` carries. The MCP lifecycle specifies: "If the server supports the
requested protocol version, it **MUST** respond with the same version.
Otherwise, the server **MUST** respond with another protocol version it
supports. This **SHOULD** be the *latest* version supported by the server."
The base answer is the `get_info` version; the chosen answer is the latest
supported version, which is the same value because `get_info` carries the
ceiling.

The ceiling is `2025-11-25`: it keeps the answer every current host already
receives and changes only the `2026-07-28` and unknown-revision answers. The decision was taken in this
session on the owner's instruction and is recorded for the owner's review. The
supported set is `ProtocolVersion::known_up_to(V_2025_11_25)`; a request above
the ceiling is negotiated down to it, and a discover request without
`initialize` is refused with `-32022`.

If a later SDK line cannot serve `2025-11-25` with `initialize`, the
specification is amended before any code change.

**Acceptance.** The `initialize` result answers `2024-11-05`, `2025-03-26`,
`2025-06-18` and `2025-11-25` with the same revision, and every other value —
`2026-07-28`, one older unknown revision, one newer unknown revision and a
non-date — with `2025-11-25`, proven by the stdio and HTTP tests of FR-011.
The daemon's embedded client names `2025-11-25` explicitly through one shared
constant (`src/embed/duplex_transport.rs:EMBED_PROTOCOL_VERSION`) instead of
`ProtocolVersion::default()`.

### FR-007 The wire stays byte-identical for existing hosts, and every difference is listed

The published surface is frozen by P-04 and P-05
(`doc/arch/memory/constitution.md:101-108`). Every item below keeps its current
bytes:

- the tool catalogue: `make tools-export-check` (`Makefile:173-174`) reports
  `unchanged` through `scripts/export_tool_catalog.py:260`;
- the `initialize` result: the capability set of `server_capabilities`
  (`src/infra/mcp/tool_router.rs:3988-3998`), the
  `experimental.ssh_inline_push` entry (`:4000-4017`), the instructions
  (`:4072-4113`) and the server identity of `build_implementation`
  (`:3956-3981`);
- the block-markdown response envelope and the structured content of every
  tool, pinned by
  `src/infra/mcp/helpers/output.rs::tests::render_output_block_with_content`
  (`:245`) and
  `src/infra/mcp/render/connection.rs::tests::disconnect_render_emits_block`
  (`:882`);
- `resources/subscribe` and `resources/unsubscribe`, and the
  `notifications/resources/updated` they produce
  (`src/adapters/notifier/rmcp_adapter.rs:69-75`,
  `src/adapters/subscription/legacy.rs:606-609`);
- the custom `notifications/ssh/output`
  (`src/adapters/notifier/rmcp_adapter.rs:102-110`), gated by the capability
  `experimental.ssh_inline_push`;
- the prompts (`src/infra/mcp/tool_router.rs:4284-4299`) and the resource
  templates (`:4223-4231`, `src/infra/mcp/resource_templates.rs`).

Two differences are known in advance and are the point of FR-004 and FR-006,
both on the `protocolVersion` the `initialize` result answers: `2026-07-28`,
which used to be echoed falsely and becomes `2025-11-25`, and every revision
the server does not know, which used to answer the `get_info` version and now
answers the latest supported version, `2025-11-25`. The full table is in
FR-006 and is recorded in `docs/MIGRATION.md`. The unknown-revision answer
follows the MCP lifecycle rule quoted in FR-006.

Where the new SDK renders something else differently on the wire and the
difference cannot be avoided, each difference is listed, justified, and recorded
in `docs/MIGRATION.md`. An unlisted difference fails the feature.

**Acceptance.** `make tools-export-check` reports the catalogue unchanged, or
the feature lists each difference as this requirement states; the named render
tests stay green; the push tests of FR-012 stay green.

### FR-008 No deprecated SDK API remains in use, and no lint is silenced

The probe of 2026-11-21 printed 13 deprecation warnings: `ClientInfo` (8) and
`ServerInfo` (3), deprecated aliases of `ClientConfig` and `ServerConfig`, plus
the client methods `Peer::subscribe` and `Peer::unsubscribe` (1 each, used by
the NDJSON daemon at `src/embed/dispatcher.rs:580` and `:611`). The gate runs
clippy with `-D warnings` (`Makefile:160-161`), so a remaining warning fails
the build.

For the daemon's client-side `subscribe` and `unsubscribe`, the plan phase
finds the non-deprecated way to send those requests on a legacy session. If no
such way exists, the specification is amended before any code changes.

**Acceptance.** The build prints no deprecation warning; no
`#[allow(deprecated)]` is added; no lint is disabled. Either the daemon sends
both requests through the non-deprecated API, or the amendment is recorded.

### FR-009 The project gate is green on the new library

The verified-change gate of P-06 (`doc/arch/memory/constitution.md:109-112`)
passes on the moved library: `cargo fmt --all -- --check`,
`cargo clippy --all-features -- -D warnings`,
`cargo nextest run --test-threads num-cpus --lib` and
`cargo nextest run --test-threads num-cpus --tests --features test-fixtures`.
The reference measured after feature 001 is 2116 passing lib tests and 2283
passing with the integration suites. No test is ignored or removed and no lint
disabled (P-03, `doc/arch/memory/constitution.md:97-100`).

**Acceptance.** Each of the four commands exits 0; the lib run reports at least
2116 passing tests and the test-fixtures run at least 2283; no new
`#[ignore]` and no deleted test file exist. A test edit is allowed only where
the SDK renamed or reshaped an item the test names; each such edit is listed in
the plan with its `file:line`; no assertion is weakened, removed or made
conditional.

### FR-010 The `--no-default-features` build compiles

`--no-default-features` drops `port_forward` and yields the 38-tool surface
(`docs/DEVELOPMENT.md:22`, `:158`, `:188`). The SDK move must not break it.

**Acceptance.** `cargo build --no-default-features` exits 0.

### FR-011 A test proves the protocol answer and the capability set

The protocol version is advertised at
`src/infra/mcp/tool_router.rs:4184`, and no test asserts it today: the only
occurrence of `V_2025_06_18` in the tree is that line (measured 2026-11-22).
The capability set has unit tests
(`initialize_advertises_experimental_ssh_inline_push`,
`server_capabilities_still_advertises_tools_and_resources`,
`server_capabilities_keeps_prompts_capability`), and no test states the
protocol version or the 2026-07-28 outcome.

**Acceptance.** New stdio tests assert, per requested revision, the answer of
FR-006 and the frozen capability set, and that `server/discover` without
`initialize` is refused with `-32022`. A new HTTP test file spawns the HTTP
binary on a free port and asserts the same table; that the reply carries
`Mcp-Session-Id`; that the session is served (200) with a non-empty `tools`
array and a made-up id refused (404); that a non-loopback `Host` gets 403 while
a loopback `Host` is served; and the discover refusal. Every test passes in the
gate of FR-009.

### FR-012 The tests that already exercise push stay green

The push behaviour is pinned by `tests/v5_smoke.rs`
(`t_inline_push_handshake_records_capability` `:479`,
`t_inline_push_full_handshake_to_byte_delivery` `:606`),
`tests/chaos_inline_push.rs`, `tests/property_inline_push.rs` and the NDJSON
daemon `tests/v5_daemon_smoke.rs`
(`t01_daemon_connect_exec_subscribe_drain` `:41`). They run in the gate of
FR-009. Those files and the test modules under `src/` name SDK types —
`tests/v5_smoke.rs` imports `ServerHandler` and model types, and the probe of
2026-11-21 found deprecated `ClientInfo` uses in test code too — so the test
source may have to follow a rename or a reshape.

**Acceptance.** Every test in those four files passes with its assertions
unchanged. A test edit is allowed only where the SDK renamed or reshaped an
item the test names; each such edit is listed in the plan with its `file:line`;
no assertion is weakened, removed or made conditional.

### FR-013 The Python suites that drive the built binaries pass

The Python suites are the regression signal for the real transports. The
suites that connect, execute commands, open shells, transfer files, run rsync
and exercise subscriptions and idempotency pass: `scripts/test_http.py`,
`scripts/test_transfer_progress.py`, `scripts/test_v7_rsync_http.py`,
`scripts/test_v47_subscriptions.py` and `scripts/test_v47_idempotency.py`.
They run under pytest with the per-test timeout of `pytest.ini:6-7`; one test
of `scripts/test_transfer_progress.py` skips itself on fast machines.

**Acceptance.** Each named suite exits 0 on the debug-profile binaries, against
`SSH_MCP_TEST_TARGET` or the in-process fixture.

### FR-014 `speckit validate` adds no finding to the baseline

The baseline measured before this feature is exit 6 with 70 errors and no
warning: `fqcn.relative-super` 46, `branch.else` 8, `pin.ci.unpinned-action` 5,
`pin.docker.unpinned-image` 2, and one each of `readme.mermaid.missing`,
`readme.index.missing`, `readme.image.missing`, `readme.arch-link.missing`,
`pin.cargo.lockfile-missing`, `llms-txt.file.missing`, `fqcn.glob-import`,
`crew.roster.dangling` and `ci.gitlab.missing`.

**Acceptance.** `speckit validate` reports 70 errors with that rule set and no
warning, and no finding names a file of this feature.

### FR-015 The change is recorded, and version-specific statements that become false are corrected

`CHANGELOG.md` records the release that carries the move, `docs/MIGRATION.md`
records what an operator and a contributor have to know, entry TM-021 of
`doc/arch/threat-model/threat-model.md` records that its `rmcp` half is closed,
and item 2 of the update plan in `session.md:61-69` records the outcome.

The repository's own text names the old SDK version in statements that become
false. The plan phase lists each with `file:line` and says whether the
statement is still true on 3.11; each false one is corrected in the same
change: `Cargo.toml:85-86`, `src/embed/event_mux.rs:235-238`,
`src/ports/subscriber_registry.rs:143`,
`src/adapters/notifier/rmcp_peer.rs:139`,
`src/adapters/subscription/legacy.rs:300`, `:620`,
`src/infra/mcp/helpers/structured.rs:11` and
`src/infra/mcp/tool_router.rs:3963-3966`.

**Acceptance.** Each named file carries the record or the correction, and the
plan lists every version-specific statement with its `file:line` and its
truth on 3.11.

### FR-016 The feature adds no support for 2026-07-28, tasks, skills, elicitation or channels

The boundary is a requirement, not a note: the change adopts the 3.x SDK on the
`initialize` lifecycle and leaves the newer surfaces for later features.

**Acceptance.** No `subscriptions/listen` handler, no stateless-lifecycle
handler, no task, skill, elicitation or channel code is added.

## Security Requirements

- **Data sensitivity/classification.** The feature reads and writes no data of
  its own: it moves the MCP transport library, and the bytes it carries are the
  MCP requests and responses the process already carries.
- **Authentication/authorization.** No new authenticated surface and no new
  credential: the HTTP transport stays unauthenticated as TM-001 records, and
  the `initialize` contract and the session model are unchanged.
- **Input validation.** The untrusted input is the MCP request body and the
  HTTP response body the client buffers; RUSTSEC-2026-0462 is exactly unbounded
  response buffering in the Streamable HTTP client, and the feature adopts the
  release that bounds it (fixed in `>= 3.9.0`).
- **Cryptography in transit/at rest.** No cryptography is added or moved: TLS
  stays terminated outside the process as TM-001 records, and the feature
  handles no key material.
- **Logging/audit.** No metric or log line is added by design; the existing
  targets, levels and destinations stay, and any SDK log difference is recorded
  in `docs/MIGRATION.md`.
- **Error-handling information exposure.** The error codes and messages of JSON
  RPC stay the frozen surface P-04 names, and any rendered difference is listed
  under FR-007 instead of being left implicit.

## Acceptance Scenarios

Given the manifest and the lock after the change
When  the resolver reads them
Then  `rmcp` resolves to 3.11.0 or a later 3.x with the same feature set, and
no other direct dependency requirement moved (FR-001).

Given the moved dependency set
When  `cargo audit` reads `Cargo.lock`
Then  no vulnerability is reported and the only finding is the `chrono`
unmaintained warning under RUSTSEC-2026-0543 (FR-002).

Given the moved release resolved
When  `cargo check --all-features --all-targets` runs
Then  the crate compiles with the renamed model types, the notification
constructors and the response enums adapted (FR-003).

Given a client that asks for protocol revision 2026-07-28
When  it completes the handshake on stdio or on HTTP
Then  it is negotiated down to `2025-11-25`, and a discover request without
`initialize` is refused with `-32022` and `{requested, supported}` (FR-004).

Given an HTTP client
When  it initializes and sends a second request with `Mcp-Session-Id`, then one
with a made-up id
Then  the first is served with 200, the second is refused with 404, and both
requests reach the session rules of the same server (FR-005).

Given the `initialize` result for each requested revision
When  its protocol version is read
Then  `2024-11-05`, `2025-03-26`, `2025-06-18` and `2025-11-25` answer
themselves, and `2026-07-28`, `2024-10-07`, `1999-01-01`, `2027-01-01` and
`garbage` answer `2025-11-25` (FR-006).

Given an MCP host of the current release
When  it completes `initialize` and calls the tools
Then  the whole catalogue, the whole `initialize` result (capabilities including
`experimental.ssh_inline_push`, instructions and server identity), every tool's
envelope and structured content, the subscription notifications, inline push,
the prompts and the resource templates are the bytes it already knows, or each
difference is listed, justified and recorded; the only known differences are
the two `protocolVersion` answers of FR-006 (FR-007).

Given the feature specification mandates "no deprecated SDK API remains in use and no lint is silenced"
When  the build compiles the crate with clippy -D warnings
Then  no deprecation warning is printed, no allow attribute is added, and the
daemon subscribe and unsubscribe use the non-deprecated request path or the
specification is amended (FR-008).

Given the moved library and the current source tree
When  the four gate commands run
Then  each exits 0 with at least 2116 lib tests and 2283 tests with the
integration suites, no lint disabled and no test ignored or removed (FR-009).

Given the crate on the moved library
When  `cargo build --no-default-features` runs
Then  the 38-tool surface compiles (FR-010).

Given the feature specification mandates "a test proves the protocol answer and the capability set"
When  the implementation satisfies: a new test asserts the advertised version and a new test asserts the 2026-07-28 outcome
Then  both tests pass in the gate

Given a binary built on the moved library
When  the push suites `tests/v5_smoke.rs`, `tests/chaos_inline_push.rs`,
`tests/property_inline_push.rs` and `tests/v5_daemon_smoke.rs` run
Then  every test in them passes with its assertions unchanged (FR-012).

Given the debug-profile binaries and an SSH endpoint
When  the Python suites run under pytest
Then  each named suite exits 0 (FR-013).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  the error count stays at 70 with the same rule set and no warning fires
(FR-014).

Given the change landed
When  a reader opens `CHANGELOG.md`, `docs/MIGRATION.md`, entry TM-021 or
`session.md`
Then  each records the move, and every version-specific statement the plan
listed is true on 3.11 or corrected (FR-015).

Given the moved SDK
When  the feature is inspected for newer surfaces
Then  no 2026-07-28, task, skill, elicitation or channel support was added
(FR-016).

Given the feature specification mandates "one output block and one disconnect response render exactly as before"
When  the implementation satisfies: the output block render and the disconnect response render are unchanged
Then  the observable contract holds: proved by crate::infra::mcp::helpers::output::tests::render_output_block_with_content
And   the observable contract holds: proved by crate::infra::mcp::render::connection::tests::disconnect_render_emits_block

Given the feature specification mandates "the tools, resources and prompts capabilities are still advertised"
When  the implementation satisfies: the capability set still carries tools, resources and prompts
Then  the observable contract holds: proved by crate::infra::mcp::tool_router::tests::server_capabilities_still_advertises_tools_and_resources
And   the observable contract holds: proved by crate::infra::mcp::tool_router::tests::server_capabilities_keeps_prompts_capability

## Accepted Risks

- **AR-1 The SDK still offers no peer-disconnect callback, so the polling peer
  GC stays.** The GC is documented as the substitute at
  `src/adapters/subscription/legacy.rs:618-622` and
  `src/adapters/notifier/rmcp_peer.rs:139`, and the absence of the callback is
  documentation of 3.11, not a measurement here. Accepted: removing the poll
  needs a callback the SDK does not expose, and the poll is correct today.
- **AR-2 Release 3.11.0 "rejects unadvertised capability methods by default"
  (upstream changelog, not measured here).** It may bear on the custom
  `notifications/ssh/output`. Accepted with a measurement: the plan phase
  proves the notification still arrives, and a regression here is a defect of
  this feature, not a later one.
- **AR-3 The macro crate may render tool schemas differently.** Accepted with a
  measurement: `make tools-export-check` shows it, and any difference follows
  the FR-007 rule.
- **AR-4 The probe stopped at the library, so the error and warning list is a
  lower bound.** Accepted: the plan phase runs the same check over all targets
  and records the rest before coding.
- **AR-5 `speckit verify` resolves a citation only to a `#[test]` function,
  never to a `#[tokio::test]` one (limit measured on feature 001).** The push
  suites of FR-012 and several capability tests are async, so their scenarios
  stay `@planned` instead of bound. Accepted: the nextest gate proves them.

## Open Questions for the Plan Phase

Each question has a stop condition; neither is an implementation choice.

- **Q-1 Which non-deprecated API sends `resources/subscribe` and
  `resources/unsubscribe` as a client on a legacy session?** FR-008 requires
  one. If none exists in 3.11, amend this specification before coding.
- **Q-2 Can 3.11 serve protocol revision `2025-06-18`?** FR-006 requires it.
  If it cannot, amend this specification before coding.
- **Q-3 Does the unadvertised-capability rejection of AR-2 affect
  `notifications/ssh/output`?** Measure before coding; a break is fixed inside
  this feature.
- **Q-4 Did the 3.x line rename a Cargo feature the manifest enables?** Map
  each name before editing the manifest and record a rename in
  `docs/MIGRATION.md` (FR-001).

## Observability

The feature adds no metric, counter, NDJSON event or trace span, and it changes
no target, no level, no log format and no destination. The SDK's own tracing
events arrive through the existing subscriber and `EnvFilter` in
`src/composition/prod.rs:733-740`, so they keep the configured filter. If 3.x
changes a rendered log line or adds one, the difference is recorded under
FR-007. Conventions stay those of `doc/arch/observability/observability.md`.

## Clarifications

### Session 2026-11-22

- **Decision provenance.** Every decision in this specification was taken in
  the session that wrote it, on the owner's instruction to carry the whole
  update plan forward. They are recorded here for the owner's review, not as
  open questions.
- **Why the `initialize` lifecycle is kept (FR-004, FR-005).** The state lives
  in the per-session handler (`src/composition/prod.rs:670-671`, `:750-756`),
  and Claude Code does not register as a channel a server that negotiates
  2026-07-28. The HTTP session mode is therefore explicit, so a future SDK
  default cannot drop it in silence.
- **Why the wire requirement is a failure condition (FR-007).** P-04 and P-05
  freeze the surface. A difference that cannot be avoided is allowed only when
  it is listed, justified and recorded in `docs/MIGRATION.md`; an unlisted one
  fails the feature.
- **The probe is a lower bound (AR-4).** It stopped at the library, so the
  plan phase repeats it over all targets before any edit.
- **The two amendment conditions (Q-1, Q-2).** Both are checked by the plan,
  and a negative answer amends this specification before code.
- **The protocol ceiling is `2025-11-25`, not `2025-06-18`.** Both were
  measured. The lower ceiling would change the answer for every host that asks
  for `2025-11-25` today; `2025-11-25` changes only the `2026-07-28` and unknown-revision answers.
  Decided in this session on the owner's instruction, recorded for the owner's
  review.
- **An unknown revision answers the latest supported version.** `rmcp 1.8.0`
  answers it with the `get_info` version; `2025-11-25` keeps that rule and
  raises the value with the ceiling. The MCP lifecycle requires another
  supported version and recommends the latest; the chosen answer follows it
  (`research.md` §6b).
- **The loopback-only `Host` allow-list stays on.** It exists in both SDK
  lines: the base binary already answers 403 to a non-loopback `Host`, and the
  3.11.0 default answers the same status and body (measured, `research.md`
  §7). No configuration line changes it; TM-001 records the current state.
