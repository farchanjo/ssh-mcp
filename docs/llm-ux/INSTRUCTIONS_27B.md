# Root prompt — 27B-class models

Compact root prompt. Embedded verbatim into `Implementation.instructions` when the host signals a 27B-class model (Gemma 3 27B IT, Mistral Small 3, Qwen 2.5 32B). Phase 3 wires the dispatch; for now the canonical text lives here. Source: [ADR 0005](../adr/0005-llm-ux-priorities.md). Full rules: [`GOLDEN_RULES.md`](./GOLDEN_RULES.md).

```text
SSH MCP v5.0. Subscribe-first. 28 tools.

GOLDEN RULES:
 1. Long-running resource MUST have ≥1 subscriber.
    No subscriber? Pass release_when_no_subs=true.
 2. ssh_unsubscribe(sub_id) when done. Track every sub_id.
 3. lag_drops > 0 in ssh_sub_stats? Use lag_policy=snapshot.
 4. On error: ssh_disconnect_agent(agent_id) wipes your scope.
 5. NEVER hot-poll ssh_shell_read. Use ssh_subscribe + drain.

PUSH-FIRST HAPPY PATHS:
1) Async cmd:
   ssh_connect -> ssh_execute(release_when_no_subs=true)
   -> ssh_subscribe(uri=command://<cid>/output, lifetime=auto-close)
   -> drain events until ev=completed.
2) Interactive shell:
   ssh_connect -> ssh_shell_open(release_when_no_subs=true)
   -> ssh_subscribe(uri=shell://<sid>/output)
   -> ssh_shell_write / ssh_shell_send_key.
3) Upload + progress:
   ssh_upload(release_when_no_subs=true)
   -> ssh_subscribe(uri=transfer://<tid>/progress).

FALLBACK (no subscribe support):
4) ssh_run (one-shot connect+exec+disconnect).
5) ssh_execute -> ssh_get_command_output(wait=true,
                                         wait_timeout_secs=30).

CLEANUP CHECKLIST (run at workflow end):
 [ ] ssh_unsubscribe every sub_id you opened
 [ ] ssh_shell_close / ssh_cancel_command if not auto-close
 [ ] ssh_disconnect_agent(agent_id) on error
 [ ] ssh_disconnect for graceful single-session close

WIRE TIPS:
- Every response: KEY: value lines + JSON in structured_content.
- IDs end in _ID. NEXT: line = next-tool priority order.
- HINT: REQUIRED -> mandatory. HINT: RECOMMENDED -> soft.
- _meta.idempotency_key on retries dedupes mutating tools.
- Errors: REASON: [CODE] desc. DETAIL: cure (read it).
- AUTH/RESOURCE/INTERNAL = never retry. TRANSPORT = backoff.
  POLICY = retry conditional. STATE = retry only with idem key.

LAG POLICIES (per sub_id, default=snapshot):
- snapshot: drop backlog + rebuild from ring buffer.
- block_slow: zero loss, producer blocks (forensic).
- drop_oldest / drop_newest: explicit gap markers.
```
