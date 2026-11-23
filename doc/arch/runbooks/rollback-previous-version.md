# Runbook — roll back to the previous version

## Purpose

Return an installation to the version that ran before the last deploy, by swapping the
binaries. The documented downgrade path states that a v7.1.x to v7.0.x roll-back is a binary
swap with zero data migration (`docs/MIGRATION.md:937-945`).

## Trigger

A host misbehaves after a version change and the operator decides to go back to the previous
version. The repository documents the pin-and-rebuild shape of the operation
(`README.md:153-158`).

## Preconditions

- The previous version tag exists in the repository (`README.md:153-158`).
- The same build and install prerequisites as the deploy runbook: toolchain at or above 1.99
  (`Cargo.toml:16`), write access to `$(PREFIX)/bin` (`Makefile:24`, `Makefile:31-32`).
- The compatibility record for the roll-back boundary is current. For v7.1.x to v7.0.x the
  record lists what the older binary ignores: the `inline_push` field, the two inline-push
  environment variables, and the `experimental.ssh_inline_push` capability
  (`docs/MIGRATION.md:939-945`).

## Steps

1. Select the previous version in the checkout.

   ```bash
   git -C ssh-mcp checkout vX.Y.Z
   ```

   The pinned-version procedure is documented for hosts that must downgrade
   (`README.md:153-158`).

2. Rebuild and reinstall, so the installed binaries match that tag.

   ```bash
   sudo make release
   ```

   (`Makefile:98-104`, `README.md:115`).

3. Confirm the install.

   ```bash
   make verify
   ```

   (`Makefile:128-135`).

4. Restart the service on the replaced binary. The committed definition is
   `com.farchanjo.ssh-mcp.plist`, which declares `KeepAlive`, so the process is restarted
   after it exits. The repository gives no unload, load, or restart command for it: gap. No
   systemd unit exists in the repository.

5. Leave the environment as it is. `SSH_INLINE_PUSH_MAX_BYTES_PER_NOTIFY` and
   `SSH_INLINE_PUSH_DAEMON_RELAY` are read-only on a v7.1 server and ignored by a v7.0.x
   server, so both may stay set across the boundary (`docs/MIGRATION.md:943`).

6. Expect the client-visible change on opt-in lanes only: after a roll-back from v7.1.x, a
   lane that asked for `inline_push=true` receives `INLINE_PUSH_HONORED: no` and delivery
   falls back to the legacy `notifications/resources/updated` cadence. No client change is
   required (`docs/MIGRATION.md:945`).

## Verification

- `make verify` reports the three binaries (`Makefile:128-135`).
- A host reconnects and receives push events on the legacy cadence; `sub_stats` shows
  `events_sent` advancing (`docs/OPERATIONS.md:871-880`).
- The daemon consumer, if it ran with the relay gate on, simply receives no `inline_push`
  event from the older binary (`docs/MIGRATION.md:944`).

## Rollback

Deploy the newer version again by following
[deploy-new-version.md](./deploy-new-version.md). The v7.1 upgrade is wire-additive for every
existing consumer, so re-issuing the newer binary restores the previous posture without a
client change (`docs/MIGRATION.md:898-900`, `docs/MIGRATION.md:914-920`).
