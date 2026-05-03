# Resources Reference (v4.5.0)

ssh-mcp implements the MCP `resources/*` family on top of five subscribe-friendly URI schemes. This document is the source of truth for URI grammar, cursor semantics, `_meta` fields, subscribe lifecycle, and backpressure features. The wire contract is byte-compatible with v3.0.0 (see [MIGRATION_v3_to_v4.md](./MIGRATION_v3_to_v4.md)); v4.5 puts the v3-promised `_meta` envelope on every read, formalises the per-resource MIME types, and derives a stable `PeerId` from the transport (HTTP `Mcp-Session-Id` header or stdio singleton) so the per-peer cursor survives across requests.

Cross references:

- [API.md](./API.md) — tool reference (each tool documents the resource URI it produces).
- [LLM_GUIDE.md](./LLM_GUIDE.md) — decision table for when to subscribe vs poll.
- [LOCKS.md](./LOCKS.md) — lock-free patterns underpinning the broadcast / cursor layer.

## Capabilities advertised

`McpSshServer::get_info()` (in `src/infra/mcp/tool_router.rs`, see [ARCHITECTURE.md](./ARCHITECTURE.md#srcinframcp--inbound-mcp-transport)) returns:

- `protocol_version = 2025-06-18`.
- `capabilities.tools = { list_changed: true }`.
- `capabilities.resources = { subscribe: true, list_changed: true }`.
- v4.5 `Implementation` identity: `title = "SSH Remote Shell"`, multi-line `description`, `website_url = "https://github.com/farchanjo/ssh-mcp"`. Icons left as TODO. See [API.md - Capability handshake](./API.md#capability-handshake-1).

The `list_changed` advertisement is reserved for tool-driven lifecycle events. The current implementation does not emit `notifications/resources/list_changed`; clients should `resources/list` once and rely on `notifications/resources/updated` plus their own bookkeeping for new shells / commands / transfers. See [ARCHITECTURE.md](./ARCHITECTURE.md) for the deferred plan.

## URI schemes

| Scheme                              | Resource                          | Mime               | Cursor support           |
| ----------------------------------- | --------------------------------- | ------------------ | ------------------------ |
| `shell://<id>/output`               | PTY output buffer                 | `text/plain`       | yes (`?cursor=auto\|<N>\|0`) |
| `command://<id>/output`             | Async command stdout/stderr       | `text/plain`       | yes                      |
| `transfer://<id>/progress`          | SFTP progress (point-in-time)     | `application/json` | no                       |
| `session://<id>/health`             | Session health snapshot           | `application/json` | no                       |
| `forward://<id>/events`             | Port-forward event log            | `application/json` | yes                      |

Reference implementation: `src/application/{list_resources,read_resource,subscribe_resource,unsubscribe_resource}.rs` (use cases), `src/infra/mcp/resource_handlers.rs` (rmcp wiring + URI parser), and `src/adapters/subscription/memory_registry.rs` (registry + per-resource debouncer + per-peer cursor).

### URI grammar

```
<scheme>://<resource_id>/<sub_path>[?cursor=<value>]

<scheme>     ::= shell | command | transfer | session | forward
<resource_id> ::= non-empty UUIDv4 (or any non-empty string for forward)
<sub_path>   ::= output  (shell, command)
              |  progress (transfer)
              |  health   (session)
              |  events   (forward)
<value>      ::= auto | 0 | <u64>
```

Any other `?key=value` pair is silently ignored. Multi-pair queries are supported (`?other=1&cursor=auto&z=2`).

Errors during parsing surface as `INVALID_PARAMS` from `resources/read` and `resources/subscribe`:

- `BadScheme` — scheme not in the list above.
- `MissingId` — empty resource id.
- `BadSubPath` — sub-path does not match the scheme (e.g. `shell://abc/progress`).
- `BadCursor` — `?cursor=` value is neither `auto`, `0`, nor a valid `u64`.

## resources/list response

`resources/list` aggregates open shells, running commands, active transfers, and connected sessions. Forward resources are not enumerable yet (no `ForwardStorage` layer), but subscribe and read still work by URI.

Sample (truncated):

```json
{
  "resources": [
    {
      "uri": "shell://5e2d.../output",
      "name": "Shell 5e2d... (session 7d31...)",
      "description": "PTY output buffer for shell 5e2d... (xterm, 80x24).",
      "mimeType": "text/plain"
    },
    {
      "uri": "command://abc.../output",
      "name": "Command abc... (session 7d31...)",
      "description": "Async command output stream — status running, command: tail -F /var/log/syslog",
      "mimeType": "text/plain"
    },
    {
      "uri": "transfer://9f01.../progress",
      "name": "Transfer 9f01... (upload /tmp/x <-> /opt/x)",
      "description": "SFTP upload progress for transfer 9f01....",
      "mimeType": "application/json"
    },
    {
      "uri": "session://7d31.../health",
      "name": "Session 7d31... (root@example.com)",
      "description": "SSH session health snapshot for root@example.com.",
      "mimeType": "application/json"
    }
  ]
}
```

Pagination is currently disabled (`next_cursor = null`).

## resources/read flow

The cursor argument controls how much history the server replays:

- `?cursor=auto` — server returns only the bytes / events newer than the previous read for **this peer**. The server tracks the per-peer offset in `MemoryRegistry.peer_progress` (`src/adapters/subscription/memory_registry.rs`).
- `?cursor=<N>` — return the slice starting at absolute byte offset `N`. The server clamps `N` to the current buffer length.
- `?cursor=0` (or no `?cursor`) — full snapshot. Useful after a gap is detected via `_meta.last_seq`.

After every read, the server bumps the per-peer cursor to `start + bytes_returned` (saturating). Out-of-range offsets simply return zero bytes.

The total bytes returned per call are capped at `SSH_MCP_OUTPUT_MAX_BYTES_CAP` (default 1 MiB). Anything beyond is left in the buffer for the next read.

## `_meta` fields on `resources/read` (v4.5)

Every `resources/read` response embeds a `_meta` object on `ResourceContents`. Stream resources (`shell` / `command`) carry the cursor pair; snapshots (`transfer` / `session` / `forward`) omit them.

| Key            | Type   | Carried on        | Notes                                                                                                                          |
| -------------- | ------ | ----------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| `kind`         | string | all               | `"shell" | "command" | "transfer" | "session" | "forward"`. Lets the host route the body without re-parsing the URI.        |
| `cursor`       | u64    | shell, command    | Next cursor to pass on the following `?cursor=` read. Server bumps it to `start + bytes_returned` after every read.           |
| `buffer_size`  | u64    | shell, command    | Bytes currently held in the resource history.                                                                                  |
| `last_seq`     | u64    | all               | Last sequence number allocated for the resource. Compare to your previous `last_seq` to detect gaps after a `Lagged` recovery. |
| `status`       | string | all               | Kind-specific snapshot (`open` / `closed` / `running` / `completed` / `failed` / `healthy` / `unhealthy`).                     |

The envelope is constructed in `src/infra/mcp/resource_handlers.rs::build_stream_meta` (shell / command) and `build_snapshot_meta` (transfer / session / forward). The envelope shape is verified by `read_resource_shell_returns_bytes_and_meta` and friends.

### Body MIME types per scheme

| Scheme | MIME | Body shape |
|--------|------|------------|
| `shell://` | `text/plain` | UTF-8 lossy slice of the PTY ring buffer for the requested cursor window. |
| `command://` | `text/plain` | Block-style v3 stdout/stderr (one nonce per response, two `--- name [nonce] ---` blocks separated by a newline). |
| `transfer://` | `application/json` | Snapshot JSON (transfer id, direction, paths, bytes_transferred, total_bytes, status, last_seq). |
| `session://` | `application/json` | Snapshot JSON (session id, healthy, last_health_check, last_seq). |
| `forward://` | `application/json` | Snapshot JSON (forward id, listener, target, accepted/closed counters, last_seq). |

## Stable peer identity (v4.5)

The peer identity used by `?cursor=auto` is derived from the transport, not minted per request:

- HTTP transport: the `Mcp-Session-Id` header (case-insensitive) seeds a `PeerKey::Http(<sid>)` lookup. Every request that lands on the same Streamable HTTP session shares the same `PeerId`.
- Stdio transport: process-wide singleton (`PeerKey::Stdio`).

Implementation: `src/adapters/notifier/rmcp_peer.rs::peer_key_from_context` reads `Mcp-Session-Id` out of the rmcp `RequestContext` extensions, falling back to `Stdio` when the extension is absent. `PeerTable::get_or_mint` keys on `PeerKey` so subscribe + unsubscribe + cursor reads addressed to the same connection always see the same id. Two concurrent peers (two HTTP clients with different `Mcp-Session-Id` values, or one HTTP client + one stdio client) advance independently.

## Subscribe lifecycle

```mermaid
sequenceDiagram
    autonumber
    participant Peer as MCP Peer
    participant Server as ssh-mcp
    participant Reg as SubscriptionRegistry
    participant Deb as Debouncer task

    Peer->>Server: resources/subscribe shell://abc/output
    Server->>Reg: subscribe(kind, id, uri, peer_id, peer)
    alt First subscriber on (kind, id)
        Reg->>Deb: spawn debouncer_task
    end
    Reg-->>Server: ok
    Server-->>Peer: ack

    loop Producer activity
        Note over Server: shell/command/transfer/session/forward producer pokes the registry.
        Server->>Reg: poke(kind, id) (notify_one)
        Reg->>Deb: wakeup
        Deb->>Deb: sleep(debounce_ms)
        Deb->>Server: notify_resource_updated(uri)
        Server->>Peer: notifications/resources/updated
    end

    Note over Deb: Force-flush ticker emits even without pokes (every SSH_NOTIFY_FORCE_FLUSH_MS).
    Note over Deb: Keepalive ticker fires every SSH_NOTIFY_KEEPALIVE_S.

    Peer->>Server: resources/unsubscribe shell://abc/output
    Server->>Reg: unsubscribe(peer_id, uri)
    alt Last subscriber leaves
        Reg->>Deb: abort()
    end
```

Notes:

- The debouncer is a per-`(kind, resource_id)` Tokio task. It is created on first subscribe and aborted on last unsubscribe.
- Re-subscribing from the same `peer_id` to the same URI **replaces** the previous handle (the live `Peer` is refreshed). No duplicates.
- When the rmcp transport closes for a peer, the background peer-GC task scans the subscription registry and drops every subscription owned by that peer (interval: `SSH_MCP_PEER_GC_INTERVAL_S`, default 30 s). v4 entry point: `application::peer_gc::PeerGcUseCase`.

## Notification: `resources/updated`

Fires once per debounce window per subscribed URI.

```json
{
  "method": "notifications/resources/updated",
  "params": {
    "uri": "shell://5e2d.../output"
  }
}
```

When it fires:

- The producer (shell reader, command reader, transfer task, health probe, forward task) called `SubscriberRegistryPort::poke(kind, id)`.
- The debouncer slept `SSH_NOTIFY_DEBOUNCE_MS` (default 50 ms) to coalesce multiple pokes into one notification.
- The debouncer also fires on every `SSH_NOTIFY_FORCE_FLUSH_MS` tick (default 1000 ms) and every `SSH_NOTIFY_KEEPALIVE_S` tick (default 30 s) regardless of producer activity.

The notification carries no payload bytes — call `resources/read?cursor=auto` to fetch the delta.

## Backpressure features (A + B + D)

ssh-mcp implements three independent backpressure compensations.

### A. Sequence numbers

Every `OutputChunk`, `ProgressEvent`, `HealthEvent`, and `ForwardEvent` carries a `seq: u64` allocated from a per-resource `AtomicU64` (`MemoryRegistry::next_seq`). The registry exposes `current_seq(kind, id)` so `resources/read._meta.last_seq` can advertise the latest allocated sequence.

If your peer receives a `notifications/resources/updated` and the `last_seq` in the next `resources/read` jumped by more than 1 since your previous read, you have lagged on the broadcast channel. Recover by reading with `?cursor=0` (full snapshot), then resume `?cursor=auto`.

### B. Keepalive

Per resource, the debouncer task fires a `notifications/resources/updated` every `SSH_NOTIFY_KEEPALIVE_S` (default 30 s) even when the producer is idle. The corresponding `resources/read?cursor=auto` returns an empty body with the same `_meta.cursor` and `_meta.last_seq` as the previous read — a no-progress tick. Use the steady stream of notifications to keep the subscription alive across NAT / proxy timeouts.

### D. Cumulative chunks

The debouncer collapses N producer pokes inside a single debounce window into one outbound notification. The chunks themselves accumulate in the producer's `ArcSwap<RingBuffer>` / `ArcSwap<OutputBuffer>`; the `resources/read` step does the actual coalescing of bytes through the per-peer cursor. Result: subscribers see one notification per ~50 ms regardless of how chatty the producer is.

## Per-peer cursor behaviour

`?cursor=auto` is **per peer**. Two peers subscribed to the same URI advance independently:

```
peer A subscribes shell://abc/output
peer A read ?cursor=auto -> 0..1024  (cursor stored = 1024)

peer B subscribes shell://abc/output
peer B read ?cursor=auto -> 0..2048  (cursor stored = 2048; A's cursor untouched)

peer A read ?cursor=auto -> 1024..2048
peer B read ?cursor=auto -> (no new bytes; _meta unchanged)
```

Implementation: `MemoryRegistry::peer_progress(peer_id, uri) -> Arc<PeerProgress>` (`src/adapters/subscription/memory_registry.rs`) returns the same `Arc` for the same `(peer_id, uri)` tuple and a fresh one for any other combination. This is verified in `tests::peer_progress_returns_independent_arc_for_different_peers` and friends.

## Truncation compensation

When a producer drops bytes from the head of its ring buffer (because `max_buffer_size` was exceeded), `MemoryRegistry::compensate_truncation(uri, bytes_dropped)` is called. Every peer cursor on that URI is decremented by `bytes_dropped` (saturating at 0). Two consequences:

1. A peer that had already consumed past the drop window keeps reading new bytes seamlessly.
2. A peer that was mid-window now reads from offset 0 of the surviving buffer.

After truncation, the next `resources/read?cursor=auto` returns the surviving bytes with `_meta.cursor` reset to the new offset; subscribers detect the gap by comparing the previously seen `_meta.last_seq` against the now-decremented buffer state. Future telemetry (`truncated_since_last_read`, `lagged_since_last_read`) is reserved on `_meta` but not yet emitted in v4.5.

## Tunables

All knobs live under `SSH_NOTIFY_*` and `SSH_*_BROADCAST_CAP`. Defaults are sane for low-latency workflows; see [CONFIGURATION.md](./CONFIGURATION.md) for the full table.

| Env var                          | Default | Range / cap     | Effect                                                                      |
| -------------------------------- | ------- | --------------- | --------------------------------------------------------------------------- |
| `SSH_NOTIFY_DEBOUNCE_MS`         | 50      | clamped         | Delay between first poke and outbound notification.                         |
| `SSH_NOTIFY_FORCE_FLUSH_MS`      | 1000    | clamped         | Maximum gap between notifications when pokes keep arriving.                 |
| `SSH_NOTIFY_KEEPALIVE_S`         | 30      | clamped         | Idle keepalive interval per resource.                                       |
| `SSH_MCP_PEER_GC_INTERVAL_S`     | 30      | min 1           | Period of the peer-GC scan that drops disconnected peers' subscriptions.    |
| `SSH_SHELL_BROADCAST_CAP`        | 1024    | 16..=65536      | Capacity of the shell `output_tx` broadcast channel (Bytes chunks).         |
| `SSH_COMMAND_BROADCAST_CAP`      | 1024    | 16..=65536      | Capacity of the command `output_tx` broadcast channel (`OutputChunk`).      |
| `SSH_TRANSFER_BROADCAST_CAP`     | 256     | 16..=65536      | Capacity of the transfer `progress_tx` broadcast channel (`ProgressEvent`). |
| `SSH_SESSION_BROADCAST_CAP`      | 256     | 16..=65536      | Capacity of the session `health_tx` broadcast channel (`HealthEvent`).      |
| `SSH_FORWARD_BROADCAST_CAP`      | 256     | 16..=65536      | Capacity of the forward `events_tx` broadcast channel (`ForwardEvent`).     |
| `SSH_MCP_OUTPUT_MAX_BYTES_CAP`   | 1 MiB   | clamped         | Hard cap on bytes returned per `resources/read`.                            |

## Worked example — shell subscribe

```text
-> resources/subscribe { "uri": "shell://5e2d/output" }
<- ack

-- nothing happens for 30s --

<- notifications/resources/updated { "uri": "shell://5e2d/output" }   # keepalive tick

-> resources/read { "uri": "shell://5e2d/output?cursor=auto" }
<- contents:
   { "uri": "shell://5e2d/output",
     "mimeType": "text/plain",
     "text": "",
     "_meta": { "kind": "shell", "cursor": 0, "buffer_size": 0,
                "last_seq": 0, "status": "open" } }

-- user sends ssh_shell_write "ls\n" --
-- shell reader emits 32 bytes of output, pokes registry --

<- notifications/resources/updated { "uri": "shell://5e2d/output" }

-> resources/read { "uri": "shell://5e2d/output?cursor=auto" }
<- contents:
   { "uri": "shell://5e2d/output",
     "mimeType": "text/plain",
     "text": "$ ls\nfoo bar baz\n",
     "_meta": { "kind": "shell", "cursor": 32, "buffer_size": 32,
                "last_seq": 1, "status": "open" } }
```
