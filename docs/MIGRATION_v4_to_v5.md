# v4.x to v5.0 Migration Guide

This document is for **MCP host operators, contributors, and downstream automations** moving from ssh-mcp v4.8.x to v5.0.x. It enumerates wire compatibility, additive surface, default-behaviour deltas, and recipes for the workflows that change shape under v5.

If you only consume the v4 MCP surface and never opt into the new tools, env vars, or `release_when_no_subs` flag, no host-side change is required. v5 is wire-compatible with v4 on every legacy path. The expansions are additive.

## Status

v5.0 is in flight on the `feat/v5-foundation` branch (Phase 0 through Phase 7). This guide is **forthcoming** until v5.0-rc1 ships; sections marked _v5.0 forthcoming_ describe surface that exists in design (the 6 ADRs at [`docs/adr/0003-..0008.md`](./adr/)) but is not yet exercised by every binary in the repo. Phase 1 (lifecycle layer with v4-compatible defaults) is the only fully-wired phase as of this branch snapshot.

The guide will be promoted from _forthcoming_ to _active_ when v5.0-rc1 tags. Until then, treat every wire example as design intent — the test fixtures and integration tests under `tests/` are the binding contract.

## Reading order

If you maintain a host or an LLM prompt today, read the sections in order:

1. [Wire compatibility summary](#wire-compatibility-summary)
2. [Breaking changes](#breaking-changes)
3. [Additive surface](#additive-surface)
4. [Default-behaviour deltas](#default-behaviour-deltas)
5. [Recipes (before / after)](#recipes-before--after)
6. [LLM prompt updates](#llm-prompt-updates)
7. [Deprecation timeline](#deprecation-timeline)
8. [References](#references)

If you only operate the binary, jump to [`docs/INSTRUCTIONS_DAEMON.md`](./INSTRUCTIONS_DAEMON.md) and [`docs/TROUBLESHOOTING.md`](./TROUBLESHOOTING.md).

## Wire compatibility summary

| Surface | v4.8.x | v5.0.x | Compat? |
|---|---|---|---|
| Tool catalogue (legacy 21 tools — 20 without `port_forward`) | 21 | 21 carried over + 9 new (additive) | yes |
| Tool response markdown shape (`KEY: value`, 8-hex nonce, `--- name [nonce] ---`) | block-only | identical | yes |
| Structured `_meta` channel on every tool response | typed JSON | identical (extended for new tools) | yes |
| Resources schemes (`shell://`, `command://`, `transfer://`, `session://`, `forward://`) | 5 | identical (no new schemes in v5.0) | yes |
| `notifications/resources/updated` debounce semantics (`SSH_NOTIFY_*`) | 50 ms / 1 s / 30 s | identical defaults | yes |
| `notifications/cancelled` (rmcp 1.6 native) | yes | yes | yes |
| `prompts/list` + `prompts/get` | 5 prompts | 10 prompts (5 new) | yes — additive |
| `resources/templates/list` | 4 / 5 templates | identical (no new templates in v5.0) | yes |
| Capability handshake (`V_2025_06_18`, `tools.listChanged`, `resources.subscribe`, `resources.listChanged`) | yes | identical | yes |
| Error wire envelope (`SSH_X: ERROR\nREASON: [CODE] description\nDETAIL: ...`) | yes | identical (codes added; format unchanged) | yes |
| Idempotency (`_meta.idempotency_key`) | 15 mutating tools | 15 carried over + 8 of the 9 new tools (the read-only `ssh_sub_list` / `ssh_sub_stats` / `ssh_daemon_stats` are pure reads) | yes |
| Cursor key on resource subscriptions | `(PeerId, Uri)` | `(SubId, Uri)` internally; `(PeerId, Uri)` synthesised for legacy hosts | yes — synthesised |
| HTTP transport bind / path defaults | `0.0.0.0:8000` `/` | identical | yes |
| Stdio transport | identical | identical | yes |

**Net result.** A v4 host pointed at a v5 server gets the same wire bytes on every legacy tool and resource. v5 ships nine new tools and a second binary (`ssh-mcp-tail`) that the v4 host can simply ignore.

## Breaking changes

There is **no breaking change on the wire** between v4.8 and v5.0. There are zero tool removals, zero schema-narrowing edits, and zero behaviour changes for any unmodified host.

The deltas below are introduced as new defaults or new env vars — never as forced behaviour changes:

- New optional argument `release_when_no_subs: bool` on `ssh_shell_open`, `ssh_execute`, `ssh_upload`, `ssh_download` (default: `false` to match v4 semantics — see [Default-behaviour deltas](#default-behaviour-deltas)).
- New optional argument `lifetime: Lifetime` and `lag_policy: LagPolicy` on `ssh_subscribe` (the new tool — see [ADR 0004](./adr/0004-channel-mux-fairness.md)).
- New optional `filter` (regex / level) argument on `ssh_subscribe`.
- New env vars per [ADR 0003](./adr/0003-lifecycle-binding.md), [ADR 0006](./adr/0006-backpressure-policies.md), and [ADR 0008](./adr/0008-ndjson-daemon-protocol.md). The full table is under [`docs/CONFIGURATION.md`](./CONFIGURATION.md) when Phase 6 lands; until then, the ADRs are the canonical source.

If your host parses the wire format byte-for-byte (snapshot tests, audit pipelines), no replacement test fixture is required — every legacy assertion still holds.

## Additive surface

v5.0 adds nine net-new MCP tools and one second binary (`ssh-mcp-tail`). All are additive — older hosts that ignore them continue to work.

### Nine new tools (Phase 3)

The tool catalogue grows from 21 to 30 (or from 20 to 29 without `port_forward`). All nine are subscription-management primitives that key on the new `SubId` (UUIDv7 per `resources/subscribe` or `ssh_subscribe` call) introduced by [ADR 0004](./adr/0004-channel-mux-fairness.md).

| Tool | Purpose | Returns | Idempotency |
|---|---|---|---|
| `ssh_subscribe` | Open a push channel against a `shell://` / `command://` / `transfer://` / `session://` / `forward://` URI. Accepts `lifetime`, `lag_policy`, `filter`. | `sub_id` | yes |
| `ssh_unsubscribe` | Close a push channel by `sub_id`. Triggers grace timer if last subscriber and `release_when_no_subs = true`. | OK / NOT_FOUND | yes |
| `ssh_sub_pause` | Suspend the lane's drain loop. Producer keeps emitting; mpsc fills under the lane's lag policy. | OK | yes |
| `ssh_sub_resume` | Resume the drain loop. | OK | yes |
| `ssh_sub_filter` | Hot-reload the lane's filter regex / level. | OK | yes |
| `ssh_sub_replay` | Re-emit events from a chosen cursor (within the ring buffer window). | event count | no |
| `ssh_sub_list` | Enumerate active sub_ids with summary stats. | array of `{sub_id, uri, queue_depth, lag_policy}` | n/a (read-only) |
| `ssh_sub_stats` | Per-sub_id counter snapshot (events_sent, lag_drops, queue_depth, ...). | typed `SubscriberStats` | n/a |
| `ssh_daemon_stats` | Global stats aggregating across all sub_ids (active sessions, total subs, mux backlog, peer GC pace, ...). | typed `DaemonStats` | n/a |

Every new tool emits the same dual channel as the v4 tools: a markdown body with `KEY: value` lines and an 8-hex-char nonce framing block, plus a parallel `structured_content` JSON object.

### New binary: `ssh-mcp-tail` (Phase 4)

`ssh-mcp-tail` is a single binary with three subcommands (`run`, `shell`, `daemon`). Its primary mode (`daemon`) reads NDJSON commands on stdin and emits NDJSON events on stdout. It embeds the same `composition::prod` adapters used by `ssh-mcp` and `ssh-mcp-stdio`, wired to itself via an in-process `tokio::io::duplex` MCP transport.

The binary exists for hosts that **do not** surface `notifications/resources/updated` to the LLM (Claude Code CLI as of 2026-Q1, and several IDE integrations). Driving it from such a host gives the LLM real push delivery without any host-level subscribe support.

The full reference is at [`docs/INSTRUCTIONS_DAEMON.md`](./INSTRUCTIONS_DAEMON.md). The NDJSON op + event schema is enumerated there; this guide intentionally cross-links rather than duplicating.

### New env vars

The defaults preserve v4 behaviour. The new env vars are listed exhaustively in the canonical `docs/CONFIGURATION.md` table when Phase 6 promotes it; until then, [ADR 0003](./adr/0003-lifecycle-binding.md), [ADR 0006](./adr/0006-backpressure-policies.md), and [ADR 0008](./adr/0008-ndjson-daemon-protocol.md) are authoritative. Highlights:

- `SSH_LIFECYCLE_GRACE_MS` (default 2000) — grace window between last `ssh_unsubscribe` and `Closed` when `release_when_no_subs = true`.
- `SSH_LIFECYCLE_OWN_GRACE_MS` (default unlimited unless `release_when_no_subs = true`) — grace for `Owned` resources that opted into auto-cleanup but never received a subscriber.
- `SSH_SESSION_IDLE_GRACE_MS` (default 5000) — grace at the session level after `active_refs` drops to zero.
- `SSH_LAG_POLICY_DEFAULT` (default `snapshot`) — lane LagPolicy for subscribers that do not specify.
- `SSH_LANE_BUFFER` (default 1024) — per-lane mpsc capacity.
- `SSH_MUX_BUFFER` (default 8192) — global mux mpsc capacity.
- `SSH_BP_BLOCK_TIMEOUT_MS` (default 5000) — `BlockSlow` escape hatch.
- `SSH_SUB_LEAK_RISK_WARN_S` (default 2) — warning threshold for `Owned` resources without subscribers.
- `SSH_SUB_LEAK_RISK_KILL_S` (default 0 = off) — operator-opt-in hard kill threshold.
- `SSH_NDJSON_LINE_MAX` (default 1 MB) — daemon stdin line size limit.
- `SSH_HEARTBEAT_INTERVAL_S` (default 30) — daemon heartbeat cadence.
- `SSH_DAEMON_STATS_INTERVAL_S` (default 60) — daemon stats auto-emit cadence.
- `SSH_GRACE_HARD_TIMEOUT_S` (default 30) — daemon graceful shutdown deadline.

## Default-behaviour deltas

The following defaults change between v4.8 and v5.0. None affect a host that does not opt into the new flag or env var; v4 idioms are preserved.

| Behaviour | v4.8 | v5.0 default | Opt-in flag |
|---|---|---|---|
| Resource auto-cleanup when no subscriber | n/a (manual close required) | unchanged for v4 idioms (`release_when_no_subs = false`) | `release_when_no_subs: true` per call |
| Cursor key on resource subscriptions | `(PeerId, Uri)` | `(SubId, Uri)` internally; legacy hosts get a synthesised `sub_id` per `(PeerId, Uri)` pair | always on (transparent) |
| Lane backpressure policy | one global broadcast channel; `RecvError::Lagged` triggers manual snapshot rebuild | per-lane mpsc with `Snapshot` default | `lag_policy` per `ssh_subscribe` call |
| Peer GC interval | 30 s | 30 s (`SSH_MCP_PEER_GC_INTERVAL_S`) | n/a |
| Session-level reaper | inactivity TTL only | refcount-aware (active_refs supersedes TTL) | always on |
| Inactivity TTL on shell | unchanged (`SSH_SHELL_INACTIVITY_TTL_SECS`) | unchanged | n/a |
| Shutdown sequence | abrupt for stdio; HTTP graceful via axum | NDJSON daemon adds explicit drain (`SSH_GRACE_HARD_TIMEOUT_S`) | `daemon` subcommand only |
| Auto-warning for leak risk | none | `WARN: SUB_LEAK_RISK` line on next `ssh_list_*` call referencing the resource | always on once Phase 3 lands |

The `release_when_no_subs = false` default means v5 hosts that do **not** add the flag inherit v4 leak semantics: a long-running shell persists until manually closed (or until the inactivity TTL fires). This is intentional. v6.0 will flip the default to `true`; v5 ships the flag wired but defaulted off so that hosts upgrade their prompts and idempotency strategy first.

## Recipes (before / after)

The recipes below show the same workflow under v4.8 and under v5.0 push-first. Both are valid in v5.0 — the v4 path remains supported. The v5 path is recommended once your host's prompt and the LLM tooling expose `ssh_subscribe`.

### Open a shell + drain push (Claude Desktop, full-spec host)

**v4.8 — wait + read polling fallback**

```text
ssh_connect(address, username)
  -> SESSION_ID
ssh_shell_open(session_id, cols=80, rows=24)
  -> SHELL_ID
ssh_shell_read(shell_id, wait=true, wait_timeout_secs=30, min_bytes=1)
  # repeat until done; manually close
ssh_shell_close(shell_id)
ssh_disconnect(session_id)
```

**v5.0 — push-first**

```text
ssh_connect(address, username, agent_id="my-claude-agent")
  -> SESSION_ID
ssh_shell_open(session_id, cols=80, rows=24, release_when_no_subs=true)
  -> SHELL_ID  (returns INITIAL_BUFFER if the prompt arrives within 100 ms)
ssh_subscribe(uri="shell://<SHELL_ID>/output", lifetime="auto-close", lag_policy="snapshot")
  -> SUB_ID
# drive the shell; drain push events as they arrive
ssh_shell_write(shell_id, bytes="ls -la\n")
# ... events drain via notifications/resources/updated ...
ssh_unsubscribe(sub_id)            # release_when_no_subs triggers grace timer
# shell auto-closes after SSH_LIFECYCLE_GRACE_MS
ssh_disconnect_agent(agent_id="my-claude-agent")
```

### Run a long command + sub + drain until completed

**v4.8 — wait fallback**

```text
ssh_connect -> SESSION_ID
ssh_execute(session_id, command="run-long-job") -> COMMAND_ID
ssh_get_command_output(command_id, wait=true, wait_timeout_secs=300)
  # blocks until exit or timeout; one tool call burns one round trip
ssh_disconnect(session_id)
```

**v5.0 — push-first with auto-cleanup**

```text
ssh_connect -> SESSION_ID
ssh_execute(session_id, command="run-long-job", release_when_no_subs=true) -> COMMAND_ID
ssh_subscribe(uri="command://<COMMAND_ID>/output",
              lifetime="auto-close",
              lag_policy="snapshot")
  -> SUB_ID
# drain events until { ev: "completed", exit: <int> } arrives
# resource auto-releases (Owned -> Releasing -> Closed) after grace timer
ssh_unsubscribe(sub_id)
ssh_disconnect(session_id)
```

### Upload a file + sub progress

**v4.8 — poll**

```text
ssh_upload(session_id, local="/tmp/file", remote="/srv/file") -> TRANSFER_ID
ssh_get_transfer_progress(transfer_id, wait=true, wait_timeout_secs=300)
  # blocks until completion
```

**v5.0 — push-first**

```text
ssh_upload(session_id, local="/tmp/file", remote="/srv/file",
           release_when_no_subs=true) -> TRANSFER_ID
ssh_subscribe(uri="transfer://<TRANSFER_ID>/progress",
              lifetime="auto-close",
              lag_policy="snapshot")
  -> SUB_ID
# drain { ev: "transfer_progress", bytes: ..., total: ... } events
ssh_unsubscribe(sub_id)
```

### Audit my owned subscriptions (v5.0 only)

```text
ssh_sub_list(filter_by_uri="shell://*")
  -> [{sub_id, uri, queue_depth, lag_policy, lagged_drops}, ...]
# decide which are stale, then:
ssh_unsubscribe(sub_id)
```

A `subscription_hygiene_audit` prompt published via `prompts/list` automates this loop. See [`docs/llm-ux/PROMPTS_CATALOG.md`](./llm-ux/PROMPTS_CATALOG.md) (forthcoming).

### Replay after disconnect (v5.0 only)

```text
# after a network blip, reconnect:
ssh_connect(...) -> SESSION_ID
# the prior shell/command is still alive (refcount > 0 because the
# resource was created with release_when_no_subs=false OR the grace
# window has not elapsed):
ssh_subscribe(uri="shell://<SHELL_ID>/output", lifetime="auto-close")
  -> SUB_ID
# the lane initialises with lag_policy=snapshot; the first event is a
# `{ ev: "snapshot", cursor: N, delta: <bytes> }` with the live ring
# buffer contents from cursor 0 (or `last_seen_cursor` if you provided it).
ssh_sub_replay(sub_id, from_cursor=last_seen)
  # for explicit replay outside the snapshot rebuild
```

If the resource has `Closed` in the meantime (grace timer fired), `ssh_subscribe` returns `RESOURCE_GONE` with a `DETAIL: Resource closed (lifecycle Releasing/Closed); recreate via ssh_shell_open / ssh_execute / ssh_upload.` line. See [`docs/llm-ux/ERROR_HANDBOOK.md`](./llm-ux/ERROR_HANDBOOK.md) (forthcoming) for the full code-by-code retry policy.

### Daemon-mode equivalent (Claude Code CLI, no-subscribe host)

When the host's LLM cannot consume `notifications/resources/updated`, a Claude Code shell can pipe NDJSON through `ssh-mcp-tail`:

```bash
ssh-mcp-tail run --host vm.example.com --user root -- "tail -f /var/log/app.log" \
  | jq 'select(.ev == "push") | .delta'
```

The daemon enforces the same lifecycle and lag policy as the in-process server. See [`docs/INSTRUCTIONS_DAEMON.md`](./INSTRUCTIONS_DAEMON.md) for the full op + event schema and pipeline recipes.

## LLM prompt updates

If you embed `Implementation.instructions` in your host's system prompt or fine-tune a model on the v4 surface, refresh your prompt with the v5 root text. The canonical sources are [`docs/llm-ux/INSTRUCTIONS_27B.md`](./llm-ux/INSTRUCTIONS_27B.md) (compact prompt for 27B-class models) and [`docs/llm-ux/INSTRUCTIONS_70B.md`](./llm-ux/INSTRUCTIONS_70B.md) (extended prompt with tradeoff guidance). Both are forthcoming; until they ship, the prompt body is captured in [ADR 0005](./adr/0005-llm-ux-priorities.md).

The five golden rules from [ADR 0005](./adr/0005-llm-ux-priorities.md) (subscribe-first, always unsubscribe, watch lag_drops, cleanup on error, never hot-poll) are documented in [`docs/llm-ux/GOLDEN_RULES.md`](./llm-ux/GOLDEN_RULES.md) (forthcoming). If your fine-tuning recipe references rules by number, treat that file as the definitive list.

The 10 prompts published via `prompts/list` change shape: 5 v4 carryovers (`run_one_shot_command`, `investigate_session`, `upload_and_verify`, `interactive_shell_drive`, `cleanup_agent`) plus 5 v5 additions (`push_first_long_command`, `push_first_interactive_shell`, `push_first_file_transfer`, `subscription_hygiene_audit`, `chaos_resume_after_disconnect`). The catalog is documented at [`docs/llm-ux/PROMPTS_CATALOG.md`](./llm-ux/PROMPTS_CATALOG.md) (forthcoming).

The 10 documented anti-patterns (hot-poll, leak-on-error, lag-blindness, ...) live in [`docs/llm-ux/ANTIPATTERNS.md`](./llm-ux/ANTIPATTERNS.md) (forthcoming). Use this when the LLM produces a workflow that compiles but leaks.

## Deprecation timeline

| Version | Status | Notes |
|---|---|---|
| **v5.0** | Nothing deprecated. | The legacy `(PeerId, Uri)` cursor key is kept; it is synthesised internally so v4 hosts work unchanged. The v4 `resources/subscribe` flow auto-mints a `sub_id`. The v4 tools, idempotency cache, debouncer defaults, and HTTP/stdio binaries are all preserved with identical semantics. |
| **v5.x** (minor releases) | Legacy `(PeerId, Uri)` cursor key remains supported. | New tools may add optional fields; existing fields keep their semantics. The default lag policy stays `snapshot`. |
| **v6.0** (future, no date) | `release_when_no_subs = true` may become default. | Once empirical data from v5.x confirms the leak rate falls under the auto-cleanup default, v6.0 may flip the flag. The v5 default (`false`) is intentionally conservative so existing hosts inherit v4 behaviour. v6.0 will publish a separate migration guide if the default changes. |

No v4 idiom is forbidden in v5.0. No tool, env var, or wire format is removed. Hosts that never opt into the new surface should not need to update code.

## References

The 6 ADRs at [`docs/adr/`](./adr/) are the canonical source for every design decision behind v5.0. Read in order:

- [ADR 0003 — Lifecycle Binding](./adr/0003-lifecycle-binding.md) — the refcount + grace-timer state machine and the `release_when_no_subs` flag.
- [ADR 0004 — Channel Mux + SubId](./adr/0004-channel-mux-fairness.md) — the `(SubId, Uri)` cursor key and the per-lane mpsc fan-out.
- [ADR 0005 — LLM UX Priorities](./adr/0005-llm-ux-priorities.md) — the layered escalation surface, the prompt catalog growth, the `SUB_LEAK_RISK` warning.
- [ADR 0006 — Backpressure Policies](./adr/0006-backpressure-policies.md) — the four `LagPolicy` variants, the per-frontier failure mode matrix, the `BlockSlow` timeout escape hatch.
- [ADR 0007 — Error Taxonomy](./adr/0007-error-taxonomy.md) — the 7 categories, the new codes (`RESOURCE_GONE`, `SUB_NOT_FOUND`, `LAG_*`, `INVALID_OP`, ...), and the canonical `DETAIL` phrasings.
- [ADR 0008 — NDJSON Daemon Protocol](./adr/0008-ndjson-daemon-protocol.md) — `ssh-mcp-tail` op + event schema, in-process duplex transport, graceful shutdown.

Operational follow-on:

- [`docs/INSTRUCTIONS_DAEMON.md`](./INSTRUCTIONS_DAEMON.md) — the daemon NDJSON reference.
- [`docs/TROUBLESHOOTING.md`](./TROUBLESHOOTING.md) — symptom-driven diagnostic guide.
- [`docs/llm-ux/`](./llm-ux/) — prompt catalog, golden rules, error handbook, anti-patterns.
- [`docs/MIGRATION_v3_to_v4.md`](./MIGRATION_v3_to_v4.md) — historical (v3 to v4 hexagonal restructuring; not relevant for v5).
- [`docs/ARCHITECTURE.md`](./ARCHITECTURE.md) — full hexagonal layout (will be refreshed when Phase 6 lands).
- [`docs/LOCKS.md`](./LOCKS.md) — lock-free invariants enforced by Clippy.
- [`docs/RESOURCES.md`](./RESOURCES.md) — resource scheme contract.
- [`docs/CONFIGURATION.md`](./CONFIGURATION.md) — full env var table (Phase 6 promotes the v5 entries).
