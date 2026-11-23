# Runbook — incident, a host stops receiving notifications

## Purpose

Find which of the documented causes stops `notifications/resources/updated` from reaching a
consumer, and restore push delivery or move the consumer to a transport that delivers
(`docs/OPERATIONS.md:69-105`).

## Trigger

A host opened a lane with `sub_open` or `resources/subscribe`, the call returned OK (an id for `sub_open`, the empty result for `resources/subscribe`), and no
`notifications/resources/updated` arrives (`docs/OPERATIONS.md:71`, `docs/RESOURCES.md:256-323`).

## Preconditions

- The `sub_id` (or the `uri`) the host tracks, and the host that consumes the events
  (`docs/OPERATIONS.md:84-85`).
- Read access to the server, so `RUST_LOG` can be set and the lane tools can be called
  (`docs/OPERATIONS.md:842-858`).

## Steps

1. Check that the lane exists, and that its URI is the one expected.

   ```text
   sub_list
   sub_stats sub_id=<SUB_ID>
   sub_stats_all
   ```

   The three calls are the documented diagnosis for this symptom
   (`docs/OPERATIONS.md:83-87`). What each one renders is what the code returns:
   `sub_stats` renders `EVENTS_SENT`, `BYTES_SENT`, `LAGGED_DROPS`, `LAGGED_RECOVERIES`,
   `QUEUE_DEPTH`, `QUEUE_HIGH_WATERMARK` and `BLOCK_TOTAL_MS`
   (`src/infra/mcp/render/subscription.rs:415-436`), and `sub_stats_all` renders
   `LANES_TOTAL`, `EVENTS_SENT_TOTAL`, `BYTES_SENT_TOTAL`, `LAGGED_DROPS_TOTAL`,
   `LAGGED_RECOVERIES_TOTAL` and `QUEUE_HIGH_WATERMARK_MAX`
   (`src/infra/mcp/render/subscription.rs:491-507`). The operations document lists different
   names for the same tool (`docs/OPERATIONS.md:860-869`) and carries drift; the render is the
   source of truth.

2. Split server-side silence from transport loss. When `events_sent > 0` and the consumer sees
   nothing, the server emitted and the host or the transport dropped the notification. When
   `events_sent == 0`, the lane is idle and the filter, the lifecycle, or the query is the
   cause (`docs/OPERATIONS.md:89`).

3. When the host drops notifications, move the consumer onto the daemon, whose stdout is
   NDJSON the pipeline owns.

   ```bash
   cat ops.ndjson | ssh-mcp-tail daemon | tee out.ndjson
   ```

   The invocation is documented (`docs/DAEMON.md:78-82`), and the push events are filtered with

   ```bash
   jq 'select(.ev=="push")' < daemon.ndjson
   ```

   (`docs/OPERATIONS.md:890-892`).

4. When peer GC swept the subscription, re-subscribe with the same URI. The registry scans
   every `SSH_MCP_PEER_GC_INTERVAL_S` (default 30 s) and drops peers whose transport closed; a
   reconnecting client gets a new peer identity, and the old `sub_id` is dead
   (`docs/OPERATIONS.md:76`, `docs/OPERATIONS.md:100`).

5. When the grace window expired, the resource is `Closed` and a new subscribe returns
   `RESOURCE_GONE` with `DETAIL: recreate via ssh_shell_open / ssh_exec / ssh_upload`
   (`docs/OPERATIONS.md:77`, `docs/OPERATIONS.md:176-203`). Recreate the resource and open the
   lane in the same turn: the default grace window is 2000 ms (`docs/OPERATIONS.md:209`).

   Known limitation: a grace fire does not tear the resource down. It commits the `Releasing`
   to `Closed` state and the cascade decrement and closes nothing else
   (`src/adapters/lifecycle/grace_timer.rs:148-165`), so the shell or command of the released
   resource is still running on the remote host and still holds its adapter record after the
   window. Close it explicitly before recreating it:
   `ssh_shell_close shell_id=<SHELL_ID>` (`docs/API.md:683-701`) or
   `ssh_exec_cancel command_id=<COMMAND_ID>` (`docs/API.md:378-400`). A reconnect alone does
   not close it: `RusshAdapter::disconnect` leaves the adapter's shell and command maps
   untouched (`src/adapters/ssh/russh_adapter.rs:682-706`, `:248-249`).

6. When a lane filter rejects every event, hot-reload it instead of re-subscribing.

   ```text
   sub_filter sub_id=<SUB_ID> pattern=<regex>
   ```

   The tool compiles the regex at call time and mutates the live lane
   (`docs/API.md:1144-1153`, `docs/OPERATIONS.md:102`).

7. When the server answers but the lane stays silent on disk or stdout, raise the subscription
   logs.

   ```bash
   RUST_LOG=ssh_mcp=debug,ssh_mcp::adapters::subscription=trace ssh-mcp-stdio
   ```

   The target shows per-lane mpsc behaviour, debouncer flushes, and snapshot rebuilds
   (`docs/OPERATIONS.md:850`, `docs/OPERATIONS.md:857`).

8. A lane opened by `sub_open` that was paused delivers nothing until `sub_resume`; the flag
   is loaded by both delivery entries (`src/adapters/subscription/lane_bridge.rs:289`, `:303`)
   and by the dispatch preparation and dequeue (`:269`, `:483`), and `sub_resume` sends one
   catch-up `notifications/resources/updated`
   (`src/application/subscription_admin.rs:343-345`). The cure of `docs/OPERATIONS.md:79` and
   `:103` therefore works for it. `resources/subscribe` opens no lane, so a subscription made
   that way has no row in `sub_list` and no pause target; its delivery is the per-peer fan-out
   (`src/adapters/subscription/memory_registry.rs:995-1000`).

9. When `sub_open` names an id no repository holds, the call is refused with the per-kind
   `SHELL_NOT_FOUND` / `COMMAND_NOT_FOUND` / `TRANSFER_NOT_FOUND` / `SESSION_NOT_FOUND` and
   returns no id. A resource that exists but is removed between the check and the lane open
   still yields an id.

10. On the HTTP transport, a newer session can take the producer forwarder from an older one,
   and the older session's lanes then receive only the periodic flush. `build_use_cases`
   (`src/composition/prod.rs:227`) runs once per server instance and installs its own registry
   as the single producer forwarder of the process-wide legacy registry
   (`src/composition/prod.rs:544`); `install_forwarder` replaces the previous forwarder
   (`src/adapters/subscription/legacy.rs:193-195`), and the HTTP service builds each server
   through a factory (`src/composition/prod.rs:750-756`) that the composition comment states
   runs once per session (`src/composition/prod.rs:798-802`). The premise is the code comment,
   not behaviour this repository tests. The symptom is a lane of the older session whose
   `inline_events_sent` stops advancing while the session is still alive. The procedures the
   repository supports today are to reconnect the affected host, or to consume the events on a
   transport with a single composition per process: the stdio binary or the `ssh-mcp-tail`
   daemon (`README.md:120-129`).

## Verification

- `sub_stats` shows `events_sent` advancing for the lane while the consumer receives events;
  for a lane that opted into inline push, `inline_events_sent` is the second signal
  (`src/infra/mcp/render/subscription.rs:415-452`). `bytes_sent` is not a signal: the
  production notify entry passes an empty byte slice into `record_notify`
  (`src/adapters/subscription/lane_bridge.rs:497-510`), which adds its length
  (`:339`, `src/adapters/subscription/subscriber_lane.rs:271-273`), so the counter advances by
  zero on every delivery.
- The comparison tool confirms the server emitted: run the reference MCP client against the
  server and diff what arrives against what the host shows. When the reference client receives
  the notification and the host does not deliver it, the host is the failing side
  (`docs/OPERATIONS.md:911-913`).
- A lane that was recreated appears in `sub_list` with its new `sub_id` and the expected URI
  (`docs/API.md:1166-1175`).

## Rollback

The incident response changes no artifact. The reversible actions are the ones taken while
diagnosing: close lanes opened or recreated during the incident with `sub_close`
(`docs/API.md:1110-1122`), and restore the previous filter with `sub_filter` if a probe pattern
was too narrow (`docs/API.md:1144-1153`).
