# ADR 0001: Migrate from poem-mcpserver 0.3 to rmcp 1.6

## Status

Accepted (v3.0.0).

## Context

ssh-mcp 2.0.x relied on `poem-mcpserver` 0.3.1 as its MCP transport layer. While that crate was sufficient to expose the 16 tools shipped in v2, two structural gaps blocked the v3 roadmap:

1. **No `resources/*` support.** `poem-mcpserver` does not expose `list_resources`, `read_resource`, `subscribe`, or `unsubscribe` server handlers. The capability handshake hardcodes `subscribe: false`.
2. **No `Peer` notifier.** The crate offers no way to push `notifications/resources/updated` to a connected client. Without it, the v3 subscribe-first PTY flow (open shell -> subscribe -> notification on every chunk -> read delta) cannot be implemented.

The v3 roadmap explicitly requires both:

- Five subscribe-friendly resource schemes (`shell://`, `command://`, `transfer://`, `session://`, `forward://`) that an LLM can poll once and observe via push.
- Per-peer cursor tracking so `resources/read?cursor=auto` returns just the delta since the previous read.
- Sequence numbers, keepalive, and cumulative chunks for backpressure (see [RESOURCES.md](../RESOURCES.md)).

Two paths were available:

1. Fork `poem-mcpserver` and maintain a diff that adds `resources/*` and a `Peer` notifier.
2. Migrate to a SDK that supports MCP resources natively.

## Decision

Migrate to **`rmcp` 1.6**, the official Anthropic Rust SDK for the Model Context Protocol.

`rmcp` provides:

- `ServerHandler` trait methods for `list_resources`, `read_resource`, `subscribe`, and `unsubscribe`.
- `Peer<RoleServer>` with `send_notification` and convenience helpers like `notify_resource_updated`.
- Both Streamable HTTP (`StreamableHttpService` over axum) and stdio transports.
- A `#[tool_router]` + `#[tool]` macro pair that aggregates typed tool implementations into a router.
- Active maintenance by the MCP working group.

## Consequences

### Code-level changes

- **Tool registration** moved from the v2 `#[Tools]` macro to `#[tool_router]` + `#[tool]`. Each tool body becomes:

  ```rust
  #[tool(description = "...")]
  async fn ssh_x(&self, Parameters(args): Parameters<SshXArgs>) -> Result<CallToolResult, McpError>
  ```

- **Args structs** wrap parameters via `Parameters<T>` for typed JSON schema generation (`schemars` derive). Optional fields stay `Option<...>` for forward compatibility.
- **Response shape** unified to a single markdown `String` wrapped in `CallToolResult::success(vec![Content::text(s)])`. The legacy `Text<String>` adapter is gone; `commands.rs` was retired and parked at `commands_legacy.rs.txt` for reference.
- **Five subscribe schemes** implemented in `src/mcp/resources.rs` (URI parser + reader) and `src/mcp/subscription.rs` (registry + debouncer). Producers (shell reader, command reader, transfer task, health probe, forward task) call `SUBSCRIPTION_REGISTRY.poke(kind, id)` after each chunk; the per-resource debouncer task fans out one notification per debounce window.
- **Stdio quirks dropped.** v2 carried a hand-rolled `notifications/cancelled` parser inside `src/bin/ssh_mcp_stdio.rs` (~250 LOC) that intercepted both `camelCase` and `snake_case` and dropped responses for cancelled IDs. rmcp handles cancellation natively; the stdio binary now consists of `serve_stdio(McpSshServer::new())` plus a peer-GC task.
- **Two new tools added** during the migration: `ssh_shell_press` (E10) and `ssh_shell_wait_for` (E11). Total tool count: 18.

### Wire-format changes

Breaking from a client perspective:

- HTTP endpoint behavior — Streamable HTTP MCP transport with a per-session SSE channel for notifications.
- `Mcp-Session-Id` header tracked by rmcp's `LocalSessionManager`.
- Response markdown is now block-only (one `KEY: value` per line); v2 mixed inline and block forms.
- `ReusePolicy` and `CommandStatus` are typed enums; typos now produce a JSON-schema validation error.

See [MIGRATION_v2_to_v3.md](../MIGRATION_v2_to_v3.md) for the full client upgrade guide.

### Operational changes

- Added env vars: `SSH_NOTIFY_DEBOUNCE_MS`, `SSH_NOTIFY_FORCE_FLUSH_MS`, `SSH_NOTIFY_KEEPALIVE_S`, `SSH_MCP_PEER_GC_INTERVAL_S`, `SSH_SHELL_BROADCAST_CAP`, `SSH_COMMAND_BROADCAST_CAP`, `SSH_TRANSFER_BROADCAST_CAP`, `SSH_SESSION_BROADCAST_CAP`, `SSH_FORWARD_BROADCAST_CAP`, `MCP_HTTP_PATH`. See [CONFIGURATION.md](../CONFIGURATION.md).
- A background peer-GC task (`spawn_peer_gc`) drops subscriptions for peers whose rmcp transport has closed. rmcp 1.6 does not surface a peer-disconnect callback, so the GC scans the registry on `SSH_MCP_PEER_GC_INTERVAL_S` (default 30 s).

### Lock-free baseline preserved

The migration also tightened lock-free invariants. Per `Cargo.toml`:

```toml
await_holding_lock              = "deny"
await_holding_refcell_ref       = "deny"
significant_drop_in_scrutinee   = "deny"
significant_drop_tightening     = "deny"
mutex_atomic                    = "deny"
mutex_integer                   = "deny"
```

`RunningShell`, `RunningCommand`, `RunningTransfer`, `SessionRef`, `ForwardHandle`, and `SubscriptionRegistry` are entirely lock-free in production paths (`Arc<ArcSwap<T>>`, atomics, `OnceCell`, broadcast / mpsc channels, `DashMap`). See [LOCKS.md](../LOCKS.md) for the full pattern catalogue.

## Alternatives considered

- **Fork `poem-mcpserver`.** Rejected. Adding `resources/*` and a `Peer` notifier is a non-trivial diff against an upstream that is unlikely to merge it; we would own the maintenance overhead indefinitely while the rest of the MCP ecosystem coalesces around `rmcp`.
- **Roll our own MCP client / transport.** Rejected. The MCP spec is large enough (auth, sessions, capabilities, notifications, cancellation, batching) that a custom implementation would consume tens of KLOC and a real audit budget — a poor trade for an SSH multiplexer.
- **Wait for `poem-mcpserver` to ship resources.** Rejected. No public roadmap exists for the missing features; meanwhile the v3 product requirements (subscribe-first PTY, transfer progress streaming, session health notifications) gate concrete user value.

## References

- rmcp source: <https://github.com/modelcontextprotocol/rust-sdk>
- E1 (rmcp foundation): commit `3f65152` — `feat(v3): E1 rmcp foundation — dep swap + transport rewrite`.
- E3 (canary `ssh_connect`): commit `a956191` — `feat(v3): E3 ssh_connect canary migrated to rmcp`.
- E4 (full tool migration): commit `e17be5d` — `feat(v3): E4 migrate remaining 15 tools to rmcp`.
- E10 (`ssh_shell_press`): commit `f9652c7`.
- E11 (`ssh_shell_wait_for`): commit `83aa193`.
- E12 (subscription registry + debouncer): commit `d87980d`.
- E13 (`resources.rs` parser + handlers): commit `d2798ff`.
- E14 (wire `resources/*` into ServerHandler): commit `f7b7683`.
