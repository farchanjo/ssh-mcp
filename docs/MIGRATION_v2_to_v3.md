# Migrating MCP Clients from v2.0 to v3.0

This document is for **client / host implementors** upgrading from `ssh-mcp` 2.0.x to 3.0.0. If you only run the server binary you do not need to change anything except your transport library version.

Cross references:

- [API.md](./API.md) — full v3 tool reference.
- [RESOURCES.md](./RESOURCES.md) — new `resources/*` semantics.
- [ERRORS.md](./ERRORS.md) — exhaustive v3 error catalog.
- [adr/0001-migrate-to-rmcp.md](./adr/0001-migrate-to-rmcp.md) — design rationale.

## Breaking changes

1. **Transport library changed from `poem-mcpserver` 0.3 to `rmcp` 1.6** (the official Anthropic Rust SDK). HTTP transport now follows the **Streamable HTTP MCP** wire format with an SSE channel for server-initiated notifications. Default endpoint is `/` (configurable via `MCP_HTTP_PATH`).
2. **The stdio binary's custom JSON-RPC quirks are gone.** The v2 stdio loop carried a hand-rolled `notifications/cancelled` parser that swallowed responses for cancelled IDs (`camelCase` and `snake_case` both). rmcp handles cancellation natively, so the wire shape is now purely spec-compliant.
3. **Response markdown is now block-only.** v2 mixed inline (`KEY: V | KEY: V`) and block forms depending on field count; v3 always emits one `KEY: value` per line. Parsers that only support the block form keep working; parsers that special-cased the inline form must be updated.
4. **`ReusePolicy` and `CommandStatus` are typed enums** in the JSON schema. v2 accepted `Option<String>` and silently fell back on typos; v3 returns a schema validation error.
5. **Two new tools were added.** `ssh_shell_send_key` and `ssh_shell_wait_for`. The total is 18 (was 16).
6. **Five `resources/*` schemes are now exposed.** `shell://`, `command://`, `transfer://`, `session://`, `forward://`. Subscribing yields `notifications/resources/updated` per debounce window.

## Compatibility matrix

| Feature                          | v2.0                                           | v3.0                                                                  |
| -------------------------------- | ---------------------------------------------- | --------------------------------------------------------------------- |
| Server SDK                       | `poem-mcpserver` 0.3                           | `rmcp` 1.6                                                            |
| HTTP transport                   | Poem streamable HTTP                           | rmcp `StreamableHttpService` (axum-hosted) + SSE notification channel |
| HTTP path                        | `/`                                            | `/` (configurable via `MCP_HTTP_PATH`)                                |
| `Mcp-Session-Id` header          | not used                                       | tracked by rmcp's `LocalSessionManager`                               |
| Tool count                       | 16                                             | **18** (`ssh_shell_send_key`, `ssh_shell_wait_for` added)             |
| `resources/*`                    | not implemented                                | 5 schemes (`shell`, `command`, `transfer`, `session`, `forward`)      |
| Server-initiated notifications   | none                                           | `notifications/resources/updated` (deferred: `list_changed`); cancellation handled natively by rmcp |
| Response format                  | mixed inline / block                           | block-only                                                            |
| `ssh_connect.reuse`              | `Option<String>`                               | `ReusePolicy` enum (`suggest \| auto \| force_new`)                   |
| `ssh_list_commands.status`       | `Option<String>`                               | `CommandStatus` enum (`running \| completed \| cancelled \| failed`)  |
| Stdio cancel-id parser           | custom (`camelCase` + `snake_case`)            | removed — rmcp native                                                 |

## Code changes for clients

### Connect

v2 (loose schema):

```json
{
  "name": "ssh_connect",
  "arguments": {
    "address": "host:22",
    "username": "root",
    "reuse": "auto"
  }
}
```

v3 (typed schema — same wire JSON, schema-validated):

```json
{
  "name": "ssh_connect",
  "arguments": {
    "address": "host:22",
    "username": "root",
    "reuse": "auto"
  }
}
```

If you previously sent `"reuse": "Auto"` or `"reuse": "AUTO"`, rmcp will now reject the call with an `INVALID_PARAMS` JSON-RPC error. Use the `snake_case` literal `"auto"`.

### Execute and poll

Wire format unchanged; the response markdown is now strictly block style:

```
SSH_GET_COMMAND_OUTPUT: COMPLETED
COMMAND_ID: 7d31...
EXIT: 0
--- stdout [a3f2b1d7] ---
foo bar
--- stderr [a3f2b1d7] (empty) ---
```

### Interactive shell — old (poll loop)

```
ssh_shell_open
loop {
  ssh_shell_read (clear=true)
  ssh_shell_write
}
ssh_shell_close
```

### Interactive shell — new (subscribe-first)

```
ssh_shell_open                            -> SHELL_ID
resources/subscribe shell://SHELL_ID/output

# parallel:
ssh_shell_write / ssh_shell_send_key

# notifications/resources/updated arrives ->
resources/read shell://SHELL_ID/output?cursor=auto

ssh_shell_close
```

The polling path still works (`ssh_shell_read` is a documented FALLBACK for hosts that cannot consume notifications), but you give up roughly half the tokens you would otherwise spend.

## ReusePolicy enum

v2 used `Option<String>` accepting `"suggest" | "auto" | "force_new"`. v3 promotes this to a tagged enum rendered into the JSON schema:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReusePolicy {
    Suggest,    // default
    Auto,
    ForceNew,
}
```

Wire format for valid values is unchanged. Typos now produce a schema validation error.

## CommandStatus enum

Same treatment for `ssh_list_commands.status`:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}
```

## Recommended upgrade path

1. **Update your MCP host** to a release that supports the Streamable HTTP MCP transport (or compatible stdio). Anthropic Claude Desktop 0.7+, MCP Inspector 0.4+, and any rmcp-based host work out of the box.
2. **Switch tool argument deserialization** to typed enums for `ReusePolicy` and `CommandStatus` if your client validates schemas before sending.
3. **Subscribe to `resources/*`** for realtime UX wins. On long-running shells the token spend drops by roughly 50% versus the v2 polling loop because deltas are pulled with `?cursor=auto` instead of full snapshots.
4. **Drop your custom cancel-id handling** if you previously worked around the stdio quirks. rmcp follows the spec.

## Server-side env var changes

See [CONFIGURATION.md](./CONFIGURATION.md) for the full list. Highlights:

| New in v3                          | Default | Purpose                                                                          |
| ---------------------------------- | ------- | -------------------------------------------------------------------------------- |
| `SSH_NOTIFY_DEBOUNCE_MS`           | 50      | Debounce window for `notifications/resources/updated`.                           |
| `SSH_NOTIFY_FORCE_FLUSH_MS`        | 1000    | Maximum gap between notifications under continuous activity.                     |
| `SSH_NOTIFY_KEEPALIVE_S`           | 30      | Idle keepalive interval per resource.                                            |
| `SSH_MCP_PEER_GC_INTERVAL_S`       | 30      | Period of the peer-GC scan (drops subscriptions for closed transports).          |
| `SSH_SHELL_BROADCAST_CAP`          | 1024    | Capacity of the shell `output_tx` broadcast channel.                             |
| `SSH_COMMAND_BROADCAST_CAP`        | 1024    | Capacity of the command `output_tx` broadcast channel.                           |
| `SSH_TRANSFER_BROADCAST_CAP`       | 256     | Capacity of the transfer `progress_tx` broadcast channel.                        |
| `SSH_SESSION_BROADCAST_CAP`        | 256     | Capacity of the session `health_tx` broadcast channel.                           |
| `SSH_FORWARD_BROADCAST_CAP`        | 256     | Capacity of the forward `events_tx` broadcast channel.                           |
| `MCP_HTTP_PATH`                    | `/`     | HTTP route prefix for the rmcp `StreamableHttpService`.                          |

## Removed env vars

None. All v2 knobs continue to work with the same semantics.

## Wire-format gotchas

- `Mcp-Session-Id`: sent by the rmcp HTTP transport as a session correlation header. Hosts that do not understand it can ignore it; hosts that drop unknown headers will still work because the server falls back to the SSE channel for state.
- `notifications/resources/updated`: payload is `{ "uri": "<uri>" }`. There is no diff in the notification — the host must call `resources/read?cursor=auto` to get the bytes.
- `notifications/resources/list_changed`: capability is advertised but not currently emitted. Treat as forward-compatible.
