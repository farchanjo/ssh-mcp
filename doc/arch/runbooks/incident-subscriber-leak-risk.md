# Runbook — incident, a `SUB_LEAK_RISK` warning

## Purpose

Clear a `SUB_LEAK_RISK` warning by closing the unobserved resource or attaching a subscriber,
and keep the watcher thresholds tuned (`src/adapters/lifecycle/leak_watcher.rs:1-16`,
`docs/CONFIGURATION.md:155-156`).

## Trigger

A list response carries a `WARN: SUB_LEAK_RISK` line for a resource, or the NDJSON daemon
emits a `warn` event with that code (`src/adapters/lifecycle/leak_watcher.rs:75`,
`docs/MIGRATION.md:695`, `docs/DAEMON.md:375-380`).

## Preconditions

- The resource the warning names, as the URI or the id printed on the warning
  (`src/adapters/lifecycle/leak_watcher.rs:111-131`).
- Read access to the lane and session state (`docs/OPERATIONS.md:882-884`).

## Steps

1. Locate the flagged resource and its owner.

   ```text
   ssh_sessions
   ssh_commands
   sub_list
   ```

   The three listing calls cover shells, commands, and lanes; `ssh_sessions` and
   `ssh_commands` are the documented place the warning line appears
   (`docs/OPERATIONS.md:882-884`, `docs/MIGRATION.md:695`).

2. Decide between the two legitimate states for the resource: it is observed, or it is closed.

   - Attach a subscriber instead of polling: `sub_open uri=<scheme>://<id>/<sub_path>`
     (`docs/API.md:1078-1108`). The same lane tools work across every push scheme
     (`docs/API.md:1076`).
   - Close the resource: `ssh_shell_close shell_id=<SHELL_ID>` (`docs/API.md:683-701`),
     `ssh_exec_cancel command_id=<COMMAND_ID>` (`docs/API.md:378-400`), or
     `serial_close serial_id=<SERIAL_ID>` (`docs/API.md:905-910`).

3. Close the owner when the resource is not wanted at all. A disconnect cascade-closes the
   child lifecycle entries, and the warnings clear immediately — no timer and no timeout is
   involved (`CHANGELOG.md:889-892`, `docs/OPERATIONS.md:215-250`).

4. Tune the watcher when the warning is premature for the workload. The scan period is
   `SSH_SUB_LEAK_RISK_WARN_S`, default 2 s, and the optional hard kill is
   `SSH_SUB_LEAK_RISK_KILL_S`, default 0, which disables it
   (`docs/CONFIGURATION.md:155-156`).

5. On a daemon consumer, isolate the warnings in the event stream.

   ```bash
   jq 'select(.ev=="warn" or .ev=="err")' < daemon.ndjson
   ```

   (`docs/OPERATIONS.md:901-902`).

## Verification

- The next `ssh_sessions` or `ssh_commands` call returns no `WARN: SUB_LEAK_RISK` line for that
  resource (`CHANGELOG.md:892`).
- The watcher behaviour is proven by a committed test: an owned resource with no subscriber
  raises the warning (`tests/v5_smoke.rs::t09_sub_leak_risk_warn`), and the warning propagates
  through the progress channel (`tests/v5_smoke.rs::t10_sub_leak_risk_warn_propagates_via_progress`).
- `sub_stats_all` shows `LANES_TOTAL` dropping when the resource was closed rather than
  observed (`src/infra/mcp/render/subscription.rs:491-507`). The operations document lists
  different names for the same tool (`docs/OPERATIONS.md:860-869`) and carries drift.

## Rollback

The incident response changes no artifact. Two actions are reversible: a raised kill threshold
is restored by setting `SSH_SUB_LEAK_RISK_KILL_S` back to 0 (`docs/CONFIGURATION.md:156`), and
a resource closed in step 2 is reopened with the tool that created it — `ssh_shell_open`,
`ssh_exec`, `serial_open`, or the matching transfer call (`docs/API.md:488-525`,
`docs/API.md:275-306`, `docs/API.md:882-903`).
