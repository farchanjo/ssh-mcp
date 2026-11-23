# Runbook — incident, `MAX_TRANSFERS_EXCEEDED`

## Purpose

Clear a per-session transfer-cap refusal and restore file transfer on the affected session,
without misreading the cap as a network or remote-disk failure
(`docs/OPERATIONS.md:554`, `docs/OPERATIONS.md:568`, `docs/OPERATIONS.md:599`).

## Trigger

`ssh_upload`, `ssh_download`, or `ssh_rsync` answers
`REASON: [MAX_TRANSFERS_EXCEEDED]` (`docs/OPERATIONS.md:554`, `docs/OPERATIONS.md:568`,
`docs/OPERATIONS.md:599`).

## Preconditions

- The `SESSION_ID` or `RSYNC_ID` from the refused call, returned by the tool that started the
  work (`docs/API.md:725-726`, `docs/API.md:1011-1019`).
- Read access to the lane or session state, through the tools the server exposes
  (`docs/OPERATIONS.md:882-884`).

## Steps

1. Read the cap that fired. A session holds at most 10 running transfers
   (`src/adapters/sftp/internal/transfer.rs:38`) and at most 4 running rsync sessions
   (`src/ports/config.rs:102`, `src/adapters/config/env.rs:180-182`). Since 7.1.3 the cap
   counts running work only: a terminal transfer keeps its snapshot for
   `ssh_transfer_progress`, and a terminal rsync session keeps its snapshot for
   `ssh_rsync_stats`, but neither holds a slot (`CHANGELOG.md:23`, `session.md:51-59`,
   `docs/OPERATIONS.md:554`).

2. List what is in flight on the session.

   ```text
   ssh_transfer_progress transfer_id=<TRANSFER_ID> wait=false
   ssh_commands session_id=<SESSION_ID>
   ```

   The first returns the point-in-time snapshot of a transfer without waiting
   (`docs/API.md:828-874`); the second lists the commands of the session
   (`docs/API.md:354-374`, `docs/OPERATIONS.md:882-884`). For rsync, `ssh_rsync_stats` returns
   the counters (`docs/API.md:1046-1070`).

3. Free a slot in one of two ways.

   - Cancel the running work: `ssh_exec_cancel command_id=<COMMAND_ID>` cancels one command
     (`docs/API.md:378-400`), and `ssh_rsync_cancel rsync_id=<RSYNC_ID>` cancels one rsync
     session, idempotently (`docs/API.md:1028-1042`).
   - Abort the whole session: `ssh_disconnect` releases every resource the session owns. The
     response reports `TRANSFERS_ABORTED` and `COMMANDS_CANCELLED`, and only resources that
     were still running are counted (`docs/API.md:157-183`, `session.md:69-76`,
     `docs/MIGRATION.md:875`).

4. Re-issue the transfer against the same session. Re-running is safe for a transfer that
   never started; a retry of an interrupted transfer resumes with `resume=true`
   (`docs/API.md:749-750`, `docs/OPERATIONS.md:557-558`).

5. The `DETAIL` line of the refusal recommends raising `SSH_MAX_TRANSFERS_PER_SESSION`
   (`src/infra/mcp/error_detail.rs:126-128`). No such setting resolves at runtime: the value
   is the compile-time constant above, surfaced through the configuration port
   (`src/adapters/config/env.rs:176-178`), and no environment resolver exists for it. Raising
   the cap is a source change followed by the owner's release build
   (`Makefile:98-104`): gap between the recommended action and the implemented configuration
   surface.

## Verification

- The next `ssh_upload` or `ssh_download` on that session answers `STARTED` with a fresh
  `TRANSFER_ID` (`docs/API.md:707-780`).
- The running-count rule itself is proven: twelve sequential uploads on one long-lived session
  each reach `COMPLETED`, because terminal rows do not hold a slot
  (`scripts/test_http.py::test_more_than_ten_sequential_uploads_on_one_session`,
  `session.md:64-67`).
- `ssh_transfer_progress` reports the new transfer as `RUNNING` and then `COMPLETED`, which is
  what the slot released by the cancel is visible in (`docs/API.md:828-874`).

## Rollback

The incident response changes no artifact. Two actions are reversible: a raised cap in source
is reverted by restoring the constant and rebuilding (`Makefile:98-104`), and a session
aborted in step 3 is reopened with a new `ssh_connect` (`docs/API.md:73-153`).
