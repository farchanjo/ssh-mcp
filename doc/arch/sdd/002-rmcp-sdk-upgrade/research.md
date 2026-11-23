# Research: Rmcp Sdk Upgrade

Evidence for `plan.md`. The migration was carried through in a throwaway copy so
that every design decision rests on a measurement. Every command below ran on
2026-11-22 in `/tmp/ssh-mcp-rmcp-spike` or `/tmp/ssh-mcp-rmcp-base`; the
repository itself was not built.

## 1. Setup

Two copies of the working tree at `aac7fc4` (feature 001 uncommitted and
present), created with the command the brief fixes:

```sh
rm -rf /tmp/ssh-mcp-rmcp-base && rsync -a --exclude target --exclude .git \
  --exclude .venv --exclude dist --exclude '*.tar.gz' --exclude doc/.specify \
  --exclude .pytest_cache /root/dev/ssh-mcp/ /tmp/ssh-mcp-rmcp-base/
rm -rf /tmp/ssh-mcp-rmcp-spike && rsync -a --exclude target --exclude .git \
  --exclude .venv --exclude dist --exclude '*.tar.gz' --exclude doc/.specify \
  --exclude .pytest_cache /root/dev/ssh-mcp/ /tmp/ssh-mcp-rmcp-spike/
```

The base copy kept `rmcp 1.8.0`. `RUSTC_WRAPPER=sccache` and
`CARGO_BUILD_JOBS=4` were already exported and stayed; `nproc` is 4. No
`-j`, no `--release`, one cargo invocation at a time. Tests ran only through
`cargo nextest run --test-threads num-cpus`. No lint was disabled and no
`#[allow]` was added.

Documentation read (public only, never source):

- `https://docs.rs/rmcp/3.11.0/rmcp/handler/server/trait.ServerHandler.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/service/struct.Peer.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/service/struct.Subscription.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/service/fn.select_protocol_version.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/struct.ProtocolVersion.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/enum.ContentBlock.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/struct.Resource.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/struct.ResourceTemplate.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/struct.SubscriptionFilter.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/enum.ReadResourceResponse.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/enum.GetPromptResponse.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/enum.CallToolResponse.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/model/enum.ClientRequest.html`
- `https://docs.rs/rmcp/3.11.0/rmcp/transport/streamable_http_server/tower/struct.StreamableHttpServerConfig.html`
- `https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v3.0.0`
- `https://github.com/modelcontextprotocol/rust-sdk/releases` (3.8.0 to 3.11.0)
- `https://github.com/modelcontextprotocol/rust-sdk/discussions/969`

`cargo info rmcp@3.11.0` gave `rust-version: 1.88`; the project pins 1.99.1
(`rust-toolchain.toml:2`), so no toolchain change is needed.

## 2. Q-4, the seven features by name

`cargo info rmcp@3.11.0 --verbose` lists the full feature table. Every name the
manifest enables exists unchanged; no rename applies.

| Feature | Exists in 3.11.0 | Note |
| --- | --- | --- |
| `server` | yes | implies `transport-async-rw`, `schemars`, `pastey`, `uuid` |
| `client` | yes | `dep:tokio-stream` |
| `macros` | yes | `dep:rmcp-macros`, `dep:pastey` |
| `schemars` | yes | `dep:schemars` |
| `transport-io` | yes | implies `transport-async-rw`, `tokio/io-std`, `tokio/net` |
| `transport-async-rw` | yes | `tokio/io-util`, `tokio-util/codec` |
| `transport-streamable-http-server` | yes | implies `...-session`, `server-side-http`, `transport-worker` |

## 3. Manifest and resolution

Edit: `Cargo.toml:74` `version = "1.6"` → `version = "3.11"`. Feature list
unchanged.

`cargo update -p rmcp` (exit 0):

```text
    Updating crates.io index
     Locking 9 packages to highest Rust 1.99 compatible versions
      Adding base64 v0.23.1
    Updating darling v0.23.0 -> v0.24.1
    Updating darling_core v0.23.0 -> v0.24.1
    Updating darling_macro v0.23.0 -> v0.24.1
    Updating rmcp v1.8.0 -> v3.11.0
    Updating rmcp-macros v1.8.0 -> v3.11.0
      Adding tinyvec v1.13.3
      Adding unicode-general-category v1.1.0
      Adding unicode-normalization v0.1.26
note: pass `--verbose` to see 39 unchanged dependencies behind latest
```

No other direct dependency moved. The four added crates are transitive.

## 4. Compile errors, in the order met

`cargo check --all-features --all-targets` (first run, exit 101) reported 14
errors in 8 files, the probe list of the brief plus the test modules the probe
never reached:

1. `E0432 rmcp::model::Content` — `src/infra/mcp/helpers/structured.rs:31`.
2. `E0432 rmcp::model::Meta` — `src/infra/mcp/idempotency.rs:59`.
3. `E0432 rmcp::model::{PromptMessageContent, PromptMessageRole}` — `src/infra/mcp/prompts.rs:26`.
4. `E0432 rmcp::model::{AnnotateAble, Meta, RawResource}` — `src/infra/mcp/resource_handlers.rs:19`.
5. `E0432 rmcp::model::{AnnotateAble, RawResourceTemplate}` — `src/infra/mcp/resource_templates.rs:27`.
6. `E0432 Meta` — `src/infra/mcp/idempotency.rs:635` (test module).
7. `E0432 PromptMessageContent` — `src/infra/mcp/prompts.rs:385` (test module).
8. `E0432 Content` — `src/infra/mcp/tool_router.rs:4889`, `:4955`, `:4995` (test module).
9. `E0639` non-exhaustive struct literal — `src/adapters/notifier/rmcp_adapter.rs:69`.
10. `E0639` — `src/adapters/subscription/legacy.rs:606`.
11. `E0271` `read_resource` must return `ReadResourceResponse` — `src/infra/mcp/tool_router.rs:4237`.
12. `E0271` `get_prompt` must return `GetPromptResponse` — `src/infra/mcp/tool_router.rs:4296`.

The same run printed 13 deprecation warnings: `ClientInfo` eight times
(`src/embed/duplex_transport.rs:17`, `:133`, `:142`; `src/embed/event_mux.rs:28`,
`:204`, `:221`, `:241`, `:248`), `ServerInfo` three times
(`src/infra/mcp/tool_router.rs:28`, `:4178`, `:4183`), and
`Peer::subscribe` / `Peer::unsubscribe` once each
(`src/embed/dispatcher.rs:580`, `:611`).

### Fixes

- `Content` → `ContentBlock`; `Meta` → `MetaObject`; `RawResource` →
  `Resource`; `RawResourceTemplate` → `ResourceTemplate`;
  `PromptMessageContent` → `ContentBlock`; `PromptMessageRole` → `Role`.
  Documented by the `model` index pages and the 3.0.0 migration guide §17.
- `ContentBlock::Text` is a tuple variant `Text(TextContent)`, so the prompt
  test patterns became `let ContentBlock::Text(content) = ... else` and the
  assertions read `content.text`.
- `AnnotateAble::no_annotation()` is gone; the field defaults to `None`, so the
  calls were dropped with no wire change.
- `ResourceUpdatedNotificationParam` is `#[non_exhaustive]`; the two literals
  became `ResourceUpdatedNotificationParam::new(uri.to_string())`.
- `read_resource` and `get_prompt` now return `ReadResourceResponse` and
  `GetPromptResponse`; both results convert through `Into::into`
  (`ReadResourceResponse::Complete` / `GetPromptResponse::Complete`). The
  `#[tool]` macro keeps returning `CallToolResult` and wraps it in
  `CallToolResponse::Complete`; no tool method changed.
- `ClientInfo` → `ClientConfig`, `ServerInfo` → `ServerConfig` (deprecated
  aliases per the `model` page; same constructors).

## 4b. Intermediate stages measured

A fresh copy of the base (`/tmp/ssh-mcp-rmcp-stages`) applied T001, T002 and
T003 exactly as `tasks.md` words them, with `cargo check --all-features
--all-targets` after each (one invocation at a time). T001 used the update
command of section 3 verbatim.

| Stage | Exit | Errors by code |
| --- | --- | --- |
| T001 (manifest + update) | 101 | `E0432` 10, `E0639` 2, `E0271` 2 |
| T002 (renames + constructors) | 101 | `E0271` 2 |
| T003 (enums + ceiling + HTTP config) | 0 | none; 2 deprecation warnings |

`src/infra/mcp/tool_router.rs` is not an imports-only file in T002: besides
the import lines, the rename reaches the `get_info` return type and the three
`Content` uses in the test module. T003 therefore starts from two `E0271`
errors and closes the build; the two warnings it leaves are the dispatcher
deprecations T004 removes.

## 5. Q-1, the daemon's client-side subscribe

`Peer::subscribe` and `Peer::unsubscribe` are deprecated: "resources/subscribe
is legacy-only; use Peer::listen for protocol version 2026-07-28" and
"resources/unsubscribe is legacy-only; cancel the Subscription handle instead".
`Peer::listen(SubscriptionFilter)` returns a `Subscription`, but the docs state
that notifications routed to the returned `Subscription` "are not also delivered
through `ClientHandler` callbacks". The daemon forwards every notification
through `EmbedClient`'s `ClientHandler` (`src/embed/event_mux.rs:252-318`), so
switching to `listen` would move the delivery path and break the mux contract.

The non-deprecated request path that keeps `ClientHandler` delivery is the
generic `Peer::send_request` with the ordinary `ClientRequest::SubscribeRequest`
/ `ClientRequest::UnsubscribeRequest` variants (`ClientRequest` page: no variant
is deprecated; `Peer::send_request` is documented as the general-purpose
send). `ClientRequest` wraps `Request<Method, Params>`, so each call is built
with `Request::new(params)`.

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

fn expect_empty_result(result: &ServerResult, op: &str) -> Result<(), String> {
    if matches!(result, ServerResult::EmptyResult(_)) {
        return Ok(());
    }
    Err(format!("{op}: unexpected response {result:?}"))
}
```

The helper is covered by
`expect_empty_result_accepts_empty_and_reports_other_variants`. The daemon
tests prove it: `tests/v5_daemon_smoke.rs`
`t01_daemon_connect_exec_subscribe_drain` and
`t04_daemon_idempotency_replay` pass, and `scripts/test_v47_subscriptions.py`
passes 20/20. `cargo clippy --all-features -- -D warnings` exits 0 with no
deprecation warning. Q-1 needs no specification amendment.

## 6. Q-2 and FR-004/FR-006, protocol negotiation

### The current defect

The base binary echoes whatever revision the client asks for. Measured on
stdio and HTTP, `2026-07-28` is answered `2026-07-28` with no negotiation and
no error, although 1.8.0 implements no 2026-07-28 lifecycle. That false echo
is the reason FR-004 exists.

Design: a ceiling constant and a supported list from the documented
`ProtocolVersion::known_up_to`, plus the documented `negotiate_initialize` in
the `initialize` override.

```rust
const MAX_SUPPORTED_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2025_11_25;

fn supported_protocol_versions() -> &'static [ProtocolVersion] {
    ProtocolVersion::known_up_to(&MAX_SUPPORTED_PROTOCOL_VERSION)
}

fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
    Cow::Borrowed(supported_protocol_versions())
}

async fn initialize(/* ... */) -> Result<InitializeResult, McpError> {
    // set_peer_info + record_inline_push_capability unchanged
    self.negotiate_initialize(&request)
}
```

`negotiate_initialize` errors with `UNSUPPORTED_PROTOCOL_VERSION` when no
supported version keeps `initialize`; the non-`initialize` discovery path
returns the same error with `{requested, supported}`.

### Measurement, stdio (`target/debug/ssh-mcp-stdio`)

Command: `/root/dev/ssh-mcp/.venv/bin/python /tmp/probe_proto.py stdio
<binary> <revision>`.

| Asked | Base 1.8.0 | Variant A `2025-06-18` | Variant B `2025-11-25` (chosen) |
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

The base echo of `2026-07-28` is the defect FR-004 exists for; an unknown
revision answers the `get_info` version.

### Measurement, HTTP (`target/debug/ssh-mcp`)

| Asked | Base status / version / session | A status / version / session | B status / version / session |
| --- | --- | --- | --- |
| `2024-11-05` | 200 / `2024-11-05` / session | 200 / `2024-11-05` / session | 200 / `2024-11-05` / session |
| `2025-06-18` | 200 / `2025-06-18` / session | 200 / `2025-06-18` / session | 200 / `2025-06-18` / session |
| `2025-03-26` | 200 / `2025-03-26` / session | 200 / `2025-03-26` / session | 200 / `2025-03-26` / session |
| `2025-11-25` | 200 / `2025-11-25` / session | 200 / `2025-06-18` / session | 200 / `2025-11-25` / session |
| `2026-07-28` | 200 / `2026-07-28` / session | 200 / `2025-06-18` / session | 200 / `2025-11-25` / session |
| `2024-10-07` (unknown) | 200 / `2025-06-18` / session | 200 / `2025-06-18` / session | 200 / `2025-11-25` / session |
| `1999-01-01` (unknown) | 200 / `2025-06-18` / session | 200 / `2025-06-18` / session | 200 / `2025-11-25` / session |
| `2027-01-01` (unknown) | 200 / `2025-06-18` / session | 200 / `2025-06-18` / session | 200 / `2025-11-25` / session |
| `garbage` (unknown) | 200 / `2025-06-18` / session | 200 / `2025-06-18` / session | 200 / `2025-11-25` / session |

In every spike case `tools/list` with the returned `Mcp-Session-Id` answers
200 and a made-up id answers 404, so the session is real.

An unknown revision is answered with the version `get_info` carries. The
`negotiate_initialize` docs say only that it negotiates against
`supported_protocol_versions` and errors when no supported version keeps
`initialize`; they do not state a fallback. The MCP lifecycle does: "If the
server supports the requested protocol version, it **MUST** respond with the
same version. Otherwise, the server **MUST** respond with another protocol
version it supports. This **SHOULD** be the *latest* version supported by the
server." The base answer is the `get_info` version; the chosen answer is the
latest supported, which equals the `get_info` value because `get_info` carries
the ceiling.

### 6b. Variant decision

Variants A and B were both built. B differs only in the ceiling constant and
the `get_info` version. Capture script: `wire_capture.py <binary> <outdir>
<version>`; the nine requests of section 10.

| Client asks | Base vs A | Base vs B |
| --- | --- | --- |
| `2025-11-25` | not run | **all nine captures identical** |
| `2026-07-28` | only `/result/protocolVersion` differs: `2026-07-28` → `2025-06-18` | only `/result/protocolVersion` differs: `2026-07-28` → `2025-11-25` |
| unknown revision | `/result/protocolVersion` `2025-06-18` (unchanged) | `/result/protocolVersion` `2025-06-18` → `2025-11-25` |

B was chosen by the decision rule: with a `2025-11-25` client every capture is
identical to the base, and the gate and the five suites pass on B in both runs
(sections 11 and 12). A changes the answer for every host that asks
`2025-11-25` today; B changes only the false `2026-07-28` echo.

Recorded provenance: decided in this session on the owner's instruction,
recorded for the owner's review.

### A client that skips `initialize`

`server/discover` with `_meta.io.modelcontextprotocol/protocolVersion =
"2026-07-28"`:

- stdio: error `-32022`, `data.requested = "2026-07-28"`,
  `data.supported = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"]`.
- HTTP: status 400, no `Mcp-Session-Id`, the same `-32022` body (the request
  also needs `Mcp-Method: server/discover`; without it the transport answers
  `-32020`).

So the server is never routed through the stateless handler: an `initialize`
client is negotiated down, a discover client is refused with the supported set.

## 7. Q-3, FR-005 and the host allow-list

`StreamableHttpServerConfig` 3.11.0 is `#[non_exhaustive]` with private fields.
`legacy_session_mode: bool` "Only applies to legacy protocol versions
(`< 2026-07-28`)"; requests negotiating 2026-07-28 "are always served
statelessly regardless of this setting".

The `Host` allow-list exists in both SDK lines. The field docs say:

> "Allowed hostnames or `host:port` authorities for inbound `Host` validation.
> By default, Streamable HTTP servers only accept loopback hosts to prevent DNS
> rebinding attacks against locally running servers. Public deployments should
> override this list with their own hostnames."

`allowed_origins` "Defaults to an empty list, which disables Origin validation
for backward compatibility."

Command: each binary on `MCP_HOST=0.0.0.0`, free port, `POST /` with an
`initialize` body, `Content-Type: application/json`, `Accept: application/json,
text/event-stream`, and the `Host` below.

| `Host` | base 1.8.0 | 3.11.0 default | spike B |
| --- | --- | --- | --- |
| `127.0.0.1:<port>` | 200 | 200 | 200 |
| `localhost:<port>` | 200 | 200 | 200 |
| `[::1]:<port>` | 200 | 200 | 200 |
| `example.com` | 403 `Forbidden: Host header is not allowed` | 403, same body | 403, same body |
| `example.com:<port>` | 403, same body | 403, same body | 403, same body |
| `192.168.202.10:<port>` | 403, same body | 403, same body | 403, same body |
| `0.0.0.0:<port>` | 403, same body | 403, same body | 403, same body |
| `evil.example:8443` | 403 | 403 | 403 |
| `Origin: https://evil.example` (loopback `Host`) | 200 | 200 | 200 |

The base already enforces the loopback-only list, and the 3.11.0 default
answers the same status and body. No configuration line is needed:
`StreamableHttpServerConfig::default()` with `legacy_session_mode = true`.
`allowed_origins` stays at its empty default.

The server binds `0.0.0.0` by default and serves only a loopback `Host`; no
project document says so. A text search of `docs/`, `README.md`, the threat
model and `src/` for `allowed_hosts`, `Host header` and `rebinding` finds
nothing. TM-001 records the state; `session.md` records making the list
configurable as a later feature.

The session measurements are the table in section 6: session id returned,
same id 200, made-up id 404, on every revision. Q-3 (the unadvertised-capability
rejection from the 3.11.0 changelog) does not touch this setting; section 9
covers the custom notification.

## 8. FR-006 and FR-011, tests added

`tests/v7_protocol_negotiation.rs` (new) spawns
`env!("CARGO_BIN_EXE_ssh-mcp-stdio")` and has two `#[test]` functions:
`initialize_answers_each_revision` (table over `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25`, `2026-07-28`, `2024-10-07`, `1999-01-01`,
`2027-01-01`, `garbage`, each asserting the FR-006 answer and the frozen
capability set) and
`discover_without_initialize_is_refused_with_unsupported_version`.

`tests/v7_protocol_http.rs` (new) spawns `env!("CARGO_BIN_EXE_ssh-mcp")` on a
free port with `MCP_HOST=127.0.0.1`, fails with the port named if the bind
deadline passes, sets a read timeout, and has four `#[test]` functions: the
same revision table with `Mcp-Session-Id` present,
`a_returned_session_is_served_and_a_made_up_id_is_refused` (200 with a
non-empty `tools` array / 404),
`a_non_loopback_host_is_refused_and_a_loopback_host_is_served` (403 with the
body / 200), and the discover refusal.

Neither file carries an `#[allow]`; they use `expect` with a message.

In `tool_router` tests, `supported_versions_stop_at_2025_11_25` pins the four
versions of the ceiling, `a_newer_client_is_negotiated_down_to_2025_11_25`
pins the pure negotiation, and `capability_set_is_the_frozen_one` pins the
serialised capability set. `expect_empty_result_accepts_empty_and_reports_other_variants`
covers the dispatcher helper; `embed_client_info_names_the_embedded_revision`
and `default_client_reports_the_embedded_revision` pin the daemon value.

## 9. Push (Q-3)

All pass on the spike with no test edit:

- `tests/v5_smoke.rs`: 15 tests, including
  `t_inline_push_handshake_records_capability`,
  `t_inline_push_full_handshake_to_byte_delivery`,
  `t_inline_push_split_fragments_over_max`,
  `t_inline_push_record_bytes_with_tail_round_trip`.
- `tests/chaos_inline_push.rs`: 8 tests.
- `tests/property_inline_push.rs`: 11 tests.
- `tests/v5_daemon_smoke.rs`: 7 tests, including
  `t01_daemon_connect_exec_subscribe_drain`,
  `t06_daemon_relays_inline_push_through_outbound_mux`,
  `t07_daemon_round_trip_inline_push_byte_delivery`.
- `scripts/test_v47_subscriptions.py`: 20 passed.

`notifications/resources/updated` is proven by the subscription suite and the
smoke tests. For `notifications/ssh/output`, the test that fails if the custom
notification is rejected is
`tests/v5_smoke.rs::t_inline_push_full_handshake_to_byte_delivery` (delivery
bytes), with `tests/chaos_inline_push.rs::chaos_inline_handshake_dual_peers`
and `tests/v5_daemon_smoke.rs::t07_daemon_round_trip_inline_push_byte_delivery`
as the multi-peer and daemon counterparts. All pass, so the 3.11.0
"unadvertised capability methods" change does not reject the notification.

## 10. Wire, byte for byte

Nine identical requests against the base and spike stdio binaries; responses
normalised (sorted keys) under `/tmp/ssh-mcp-rmcp-{base,spike}/wire/`.

| Capture | Result |
| --- | --- |
| `initialize` | identical |
| `tools/list` | identical |
| `resources/templates/list` | identical |
| `resources/list` | identical |
| `prompts/list` | identical |
| `prompts/get` (`run_one_shot_command`) | identical |
| `tools/call ssh_sessions` (success) | identical |
| `tools/call ssh_exec_output` (domain error) | identical |
| `tools/call ssh_exec` with `session_id: 123` (schema error) | identical |

The nine captures use a `2025-06-18` client and are identical. The two differences the other client revisions expose are the `protocolVersion` answers of section 6; both are listed for `docs/MIGRATION.md`.
`python3 scripts/export_tool_catalog.py --check` in the spike prints
`unchanged: docs/TOOL_CATALOG.md` and `unchanged: docs/tool-catalog.json`,
exit 0.

## 11. Gate

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | 0 |
| `cargo check --all-features --all-targets` | 0 |
| `cargo clippy --all-features -- -D warnings` | 0, no warning |
| `cargo build --no-default-features --target-dir target/no-default-features` | 0 |
| `cargo build --bins` | 0 |
| `cargo nextest run --test-threads num-cpus --lib` | 2122 passed, 0 failed |
| `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 2295 passed, 0 failed |

The reference was 2116 lib and 2283 total; the delta is the twelve new tests
(six unit, six integration: four HTTP and two stdio). The six protocol tests
also pass alone
(`cargo nextest run --test v7_protocol_http --test v7_protocol_negotiation`:
6 passed).

`cargo audit` (exit 0): no vulnerability; one allowed warning,
`chrono 0.4.45` unmaintained under RUSTSEC-2026-0543, "No fixed upgrade is
available!". RUSTSEC-2026-0462 is gone.

## 12. Python suites

Run from the spike directory with
`SSH_MCP_BIN`/`SSH_MCP_STDIO_BIN`/`SSH_MCP_TAIL_BIN` pointing at the spike's
`target/debug` binaries, so each suite drove the spike build.

| Suite | Driven binary | Result |
| --- | --- | --- |
| `scripts/test_http.py` | spike `target/debug/ssh-mcp` | 15 passed |
| `scripts/test_transfer_progress.py` | spike `target/debug/ssh-mcp-stdio` | 1 passed, 1 skipped |
| `scripts/test_v7_rsync_http.py` | spike `target/debug/ssh-mcp` | 10 passed |
| `scripts/test_v47_subscriptions.py` | spike `target/debug/ssh-mcp` | 20 passed |
| `scripts/test_v47_idempotency.py` | spike `target/debug/ssh-mcp` | 9 passed |

Skip reason (`scripts/test_transfer_progress.py:192`): "local paramiko fixture:
transfer completed before any mid-flight poll (all samples zero or final); skip
to avoid false positives on fast machines. samples=[0, 5242880]".

The block above is run 1 (client asks `2025-06-18`). Run 2 changed the client
version in the copy only (`scripts/helpers/mcp_client.py:391`, assertions at
`scripts/test_http.py:34` and `scripts/test_stdio.py:28`) and repeated the five
suites: 15 + 1 + 10 + 20 + 9 passed, the same single skip, 0 failed. Both runs
are green, which is the second half of the decision rule for variant B. The
edits are reverted; the landed tree leaves `scripts/` untouched.

## 13. Edits made in the spike

Reference rule: a `file:line` for code that exists before the change is the
repository line today; code the change adds is named by its symbol and, where a
line is given, the line is marked as after the change.

Manifest: `Cargo.toml:74` version. Source: 15 files plus two new test files.
Patch: `/tmp/ssh-mcp-rmcp-spike.patch` (variant B, 18 files, 649
insertions, 135 deletions; excludes `target`, `Cargo.lock`, logs, captures,
`.pytest_cache`).

| File | Edit | Documentation |
| --- | --- | --- |
| `Cargo.toml:74` | `"1.6"` → `"3.11"` | crate version |
| `src/infra/mcp/helpers/structured.rs:31`, `:83`, `:106` | `Content` → `ContentBlock` | `ContentBlock` page |
| `src/infra/mcp/idempotency.rs:59`, `:518`, `:522`; tests `:635`, `:687`, `:700`, `:706`, `:714` | `Meta` → `MetaObject` | `model` page; guide §17 |
| `src/infra/mcp/prompts.rs:26`, `:214`, `:215`; tests `:385`, `:446-631` | `PromptMessageRole` → `Role`, `PromptMessageContent` → `ContentBlock`, tuple pattern | `ContentBlock` page |
| `src/infra/mcp/resource_handlers.rs:19-20`, `:257`, `:621`, `:709-790` | `RawResource` → `Resource`; `Meta` → `MetaObject`; drop `AnnotateAble` | `Resource` page |
| `src/infra/mcp/resource_templates.rs:27`, `:58-143` | `RawResourceTemplate` → `ResourceTemplate`; drop `AnnotateAble` | `ResourceTemplate` page |
| `src/adapters/notifier/rmcp_adapter.rs:69` | `::new(uri)` | `ResourceUpdatedNotificationParam` non-exhaustive |
| `src/adapters/subscription/legacy.rs:606` | `::new(uri)` | same |
| `src/embed/duplex_transport.rs:17`, `:49`, `:112`, `:118`, `:133`, `:142`, `:176` | `ClientInfo` → `ClientConfig` | deprecation message; `model` page |
| `EMBED_PROTOCOL_VERSION` (`src/embed/duplex_transport.rs:32`, after the change), `src/embed/duplex_transport.rs:142`, `src/embed/event_mux.rs:242` | `ProtocolVersion::default()` → `EMBED_PROTOCOL_VERSION` (`V_2025_11_25`) | section 14 |
| `src/embed/event_mux.rs:28`, `:204`, `:221`, `:241`, `:248` | `ClientInfo` → `ClientConfig` | same |
| `src/infra/mcp/tool_router.rs:25-29`, `:4184-4195`, `:4233-4256`, `:4292-4301` | `ServerInfo` → `ServerConfig`; response enums; ceiling + negotiation | `ServerHandler`, `ProtocolVersion` pages |
| `src/infra/mcp/tool_router.rs` (new tests; lines 5044-5104 after the change) | four new unit tests | FR-006, FR-011 |
| `tests/v7_protocol_negotiation.rs` | new file, four tests | FR-004, FR-006, FR-011 |
| `tests/v7_protocol_http.rs` | new file, three tests | FR-004, FR-005, FR-006 |
| `src/embed/dispatcher.rs:15-18`, `:578-590`, `:608-620` | `send_request` path | `Peer`, `ClientRequest` pages |
| `src/composition/prod.rs:750` (new lines 751-760 after the change) | `legacy_session_mode = true`; default `allowed_hosts` | config page |

### Test edits allowed by FR-012

All are renames or reshapes of an SDK item the test names; no assertion was
weakened, removed or made conditional.

| `file:line` | Item |
| --- | --- |
| `src/infra/mcp/idempotency.rs:635`, `:687`, `:700`, `:706`, `:714` | `Meta` → `MetaObject` |
| `src/infra/mcp/prompts.rs:385`, `:446-631` | `PromptMessageContent` → `ContentBlock`; tuple pattern |
| `src/infra/mcp/tool_router.rs:4889`, `:4902`, `:4955`, `:4974`, `:4995`, `:5015` | `Content` → `ContentBlock` |

No file under `tests/` was edited; the new `tests/v7_protocol_negotiation.rs`
is an addition.

## 14. The daemon's protocol revision

`ProtocolVersion::default()` on the base copy was measured with a throwaway
test: `DEFAULT=ProtocolVersion("2025-11-25") LATEST=ProtocolVersion("2025-11-25")`,
`EMBED=ProtocolVersion("2025-11-25")`,
`MUX=ProtocolVersion("2025-11-25")` — that is `embed_client_info()` and
`EmbedClient::default().get_info()` both carry `2025-11-25`. The `ProtocolVersion`
page for 1.8.0 documents `LATEST = Self::V_2025_11_25`, so the default is the
SDK's latest and every embedded link today negotiates `2025-11-25`.

The literal now lives once, as
`EMBED_PROTOCOL_VERSION = ProtocolVersion::V_2025_11_25` in
`src/embed/duplex_transport.rs` (line 32 after the change), and both sites use
it. Two tests pin the
project's own value: `embed_client_info_names_the_embedded_revision` and
`default_client_reports_the_embedded_revision`. The SDK-default test was
removed because it proves no project requirement.

## 15. `cargo tree` for the axum comment

`cargo tree -i axum` in the spike: `axum v0.8.9` and its only dependent
`ssh-mcp v7.1.3`. `cargo tree -p rmcp --depth 1` lists `tower-service v0.3.3`
and no axum. The corrected comment claims only those two facts.
