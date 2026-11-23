---
status: proposed
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Rmcp Sdk Upgrade

## Context and Problem Statement

The dependency set the current `Cargo.lock` resolves carries one published
vulnerability on the crate that carries the MCP transport. Measured with
`cargo audit` on 2026-11-22, after the `russh` move of feature 001:
RUSTSEC-2026-0462 on `rmcp 1.8.0`, "Unbounded response body buffering in
Streamable HTTP clients", severity 6.5, fixed in `>= 3.9.0`. It is the open
half of entry TM-021 of `doc/arch/threat-model/threat-model.md` (`:907-912`).
`Cargo.toml:74-82` asks for `rmcp = "1.6"` with the `server`, `client`,
`macros`, `schemars`, `transport-io`, `transport-async-rw` and
`transport-streamable-http-server` features; the latest release is 3.11.0.

The move is not source-compatible. A compile probe on 2026-11-21, in a
throwaway copy with `rmcp = "3.11"` and `cargo check --all-features
--all-targets`, stopped with 14 error lines in 8 files of three kinds: model
types renamed in the 2.0 line, `ResourceUpdatedNotificationParam` no longer
built from a struct literal, and `read_resource` and `get_prompt` required to
return the 3.0 response enums. The same probe printed 13 deprecation warnings
on `ClientInfo`, `ServerInfo`, `Peer::subscribe` and `Peer::unsubscribe`.

The 3.x line also adds MCP protocol revision 2026-07-28. Its release notes
state that the revision removes `initialize` and protocol-level HTTP sessions,
replaces `resources/subscribe` and `resources/unsubscribe` with
`subscriptions/listen`, and creates a fresh handler per request, while the
earlier APIs stay "for legacy protocol sessions" (upstream documentation, not
measured here). This server builds all of its state inside the handler
(`build_server` at `src/composition/prod.rs:670-671`, one server per HTTP
session through the factory at `:750-756`), so the lifecycle it adopts is a
decision, not a detail.

The current release is also wrong about the protocol version. Measured on
2026-11-22 on stdio and HTTP: `rmcp 1.8.0` echoes whatever revision the client
asks for, including `2026-07-28`, which it does not implement, and answers an
unknown revision with the `get_info` version (`2025-06-18`). The move fixes
both. Both a `2025-06-18` and a `2025-11-25` ceiling were measured; the higher
ceiling was chosen because it keeps every answer a current host already
receives and changes only the `2026-07-28` and unknown-revision answers, which
become `2025-11-25` (`/tmp/ssh-mcp-rmcp-spike-b/wire-*`). The unknown-revision answer follows the
MCP lifecycle: another supported version is required, the latest is
recommended.

## Decision Drivers

- The advisory is published and the fixed line is known, and `rmcp` sits
  directly on TB-1 and TB-2 (`doc/arch/threat-model/threat-model.md:64-76`), so
  the exposure is reachable from an unauthenticated network caller.
- The published MCP surface is a frozen contract (P-04 at
  `doc/arch/memory/constitution.md:101-104`, P-05 at `:105-108`), so the move
  must be invisible on the wire.
- The server's state lives in the per-session handler, so a lifecycle that
  creates a handler per request would lose it.
- Claude Code does not register as a channel a server that negotiates
  2026-07-28 (`https://code.claude.com/docs/en/mcp`, section "Push messages with
  channels", upstream documentation, not measured here), which matters for the
  channel support planned later.
- One concern per change (D-12, `doc/arch/memory/constitution.md:74-77`): one
  dependency move, its adaptations, and nothing else.

## Considered Options

- Option A: move `rmcp` to 3.x at or above 3.11.0 with the same feature set,
  keep the `initialize` handshake and the legacy HTTP session mode, adapt the
  three error classes, and remove every deprecated call.
- Option B: stay on `rmcp 1.8.0` and accept RUSTSEC-2026-0462 until a later
  release.
- Option C: move to 3.x and adopt revision 2026-07-28 with the stateless
  lifecycle.
- Option D: replace the MCP SDK.

## Decision Outcome

Chosen option: "Option A", because it closes the advisory with the smallest
change the SDK allows while keeping the lifecycle the server is built around.
Option B leaves an unbounded HTTP response body reachable from the network on a
crate every request exercises. Option C would move the state out of the handler
in the same change, and would make the server invisible to the channel support
planned later; it is a feature of its own. Option D replaces the transport
layer and its tests for a problem a fixed release solves.

The server keeps the `initialize` handshake. A client that asks for
2026-07-28 is negotiated down to a revision the server serves with
`initialize`, or refused with a protocol error; it never reaches a handler that
has lost its session state. The ceiling is `2025-11-25`, chosen on the
measurement above: `2025-03-26`, `2025-06-18` and `2025-11-25` answer
themselves, and `2026-07-28` and every unknown revision answer `2025-11-25`.
A discover client that skips `initialize` is refused with `-32022` and the
supported set. The HTTP transport keeps protocol-level sessions, set explicitly
as the legacy session mode so `Mcp-Session-Id` keeps working.

The loopback-only `Host` allow-list stays on. It exists in both SDK lines: the
base binary already answers 403 `Forbidden: Host header is not allowed` to a
non-loopback `Host`, and the 3.11.0 default answers the same status and body
(measured). No configuration line changes it. The server binds `0.0.0.0` and
serves only a loopback `Host`; TM-001 records the state and the fact that no
project document said so. Decided in this session on the owner's instruction,
recorded for the owner's review.

The wire stays byte-identical for existing hosts: the tool catalogue, the
`initialize` result, the response envelope and structured content, the
subscription methods and their notifications, the custom
`notifications/ssh/output`, the prompts and the resource templates. Where the
new SDK renders something differently and the difference cannot be avoided,
each difference is listed, justified and recorded in `docs/MIGRATION.md`; an
unlisted difference fails the feature.

No deprecated API remains in use and no lint is silenced. For the daemon's
client-side `subscribe` and `unsubscribe`, the plan phase finds the
non-deprecated request path on a legacy session; if none exists, the
specification is amended before code. The same holds if 3.11 cannot serve
`2025-11-25`.

### Consequences

- Good: RUSTSEC-2026-0462 leaves the resolved set. `cargo audit` on the moved
  set reports no vulnerability; the unmaintained `chrono 0.4.45` warning under
  RUSTSEC-2026-0543 stays, because no fixed upgrade exists.
- Good: the lifecycle decision becomes explicit and testable, so a later SDK
  default cannot move the server off `initialize` in silence.
- Good: the deprecated aliases and client methods are replaced in the same
  change, so the `-D warnings` gate stays meaningful.
- Bad: the move touches 13 files at compile time across the MCP surface (model
  types, notification constructors, response enums, daemon client, HTTP
  configuration), so the review is wider than a version edit.
- Bad: a client that asks for `2026-07-28` or an unknown revision no longer
  receives its own value back; it receives `2025-11-25`. That is the intended
  repair, and these two answers are the only wire differences.
- Neutral: the loopback-only `Host` allow-list is unchanged. The binary binds
  `0.0.0.0` and serves only a loopback `Host`, so a request that arrives by
  another name or address is refused; no document said so before TM-001 now
  does, and making the list configurable is left to a later feature.
- Bad: the probe stopped at the library, so the binary and integration
  surfaces are an unmeasured lower bound until the plan phase repeats it.
- Bad: release 3.11.0 "rejects unadvertised capability methods by default"
  (upstream changelog, not measured here), which may bear on the custom
  `notifications/ssh/output`; the plan phase measures it and a break is fixed
  here.
- Bad: the macro crate may render tool schemas differently, which
  `make tools-export-check` will show; an unavoidable difference must be
  listed and recorded.
- Bad: the SDK still offers no peer-disconnect callback, so the polling peer GC
  stays (documentation of 3.11, not measured here).
- Neutral: no toolchain change is needed. `rmcp 3.11.0` declares
  `rust-version: 1.88` (measured 2026-11-22) and the project pins 1.99.1
  (`rust-toolchain.toml:2`, `Cargo.toml:16`).
