# Runbook — incident, a session will not connect

## Purpose

Restore the ability to open an SSH session from the host and identify which layer refused:
the handshake, the authentication chain, or a remote-side session budget
(`docs/API.md:151-153`, `docs/OPERATIONS.md:851`).

## Trigger

`ssh_connect` answers `SSH_CONNECT: ERROR` with `CONNECTION_FAILED` or `AUTH_FAILED`
(`docs/API.md:151-153`).

## Preconditions

- Access to the server process and its environment, so `RUST_LOG` and the connection tunables
  can be set (`docs/CONFIGURATION.md:53-62`).
- The target address, user, and credential source the host intended to use
  (`docs/API.md:79-91`).

## Steps

1. Capture the failing call and its code. `CONNECTION_FAILED` means the handshake failed or
   all retries were exhausted; `AUTH_FAILED` means the authentication chain rejected every
   method (`docs/API.md:151-153`).

2. List the sessions that already exist, so a reusable or half-open session is not
   overlooked.

   ```text
   ssh_sessions
   ```

   The listing carries the health of each session and probes the live ones
   (`docs/API.md:187-211`, `src/application/list_sessions.rs:175-182`,
   `docs/OPERATIONS.md:882-884`).

3. For `AUTH_FAILED`, walk the chain the server applies: key, then password, then agent
   (`docs/API.md:83`). A wrong or unreadable `key_path` is the common cause and is proven to
   fail this way (`scripts/test_v5_adversarial.py::test_connect_with_invalid_key_path_fails_auth`).

4. For `CONNECTION_FAILED`, check the retry budget and the timeout the call used:
   `SSH_CONNECT_TIMEOUT` default 30 s, `SSH_MAX_RETRIES` default 3, `SSH_RETRY_DELAY_MS`
   default 1000 with backoff capped at 10 s (`docs/CONFIGURATION.md:57-60`). A remote that
   refuses because its own session budget is full is proven reachable as a refusal condition
   (`scripts/test_v5_adversarial.py::test_connect_max_sessions_exceeded_with_low_cap`).

5. Widen the log to see the handshake and the auth chain decisions.

   ```bash
   RUST_LOG=ssh_mcp=info,ssh_mcp::adapters::ssh::internal::client=debug ssh-mcp-stdio
   ```

   The documented targets are `ssh_mcp=debug` for use-case dispatch and
   `ssh_mcp::adapters::ssh::internal::client=debug` for the russh handshake and auth decisions
   (`docs/OPERATIONS.md:842-858`).

6. Confirm the failure is reproducible from the host side with the single-call fallback, which
   pays a full handshake and tears the session down:

   ```text
   ssh_run address=<host> username=<user> command="echo 1"
   ```

   The tool and its connection cost are documented (`docs/API.md:404-446`).

7. Release anything the diagnosis left open. `ssh_disconnect` closes one session and its owned
   resources; `ssh_disconnect_agent` closes every session under an agent scope
   (`docs/API.md:157-269`).

8. No step of this procedure changes a repository artifact. A remote-side cause — the sshd
   session budget, the network path, the remote account state — is outside what this
   repository defines: gap. The repository documents no remote-side remediation.

## Verification

- A fresh `ssh_connect` answers `SSH_CONNECT: OK` with `SESSION_ID`, `HOST`, `RETRY`,
  `PERSISTENT` and `EXPIRES_AT` (`docs/API.md:95-107`).
- The new session appears in `ssh_sessions` and holds no `WARN: SUB_LEAK_RISK` line
  (`docs/OPERATIONS.md:882-884`).
- A follow-up call on the same `SESSION_ID` succeeds; the reuse path and its `REUSED` response
  are documented at `docs/API.md:109-118` and `docs/API.md:91`.

## Rollback

No artifact was changed, so there is nothing to revert. The reversible action the operator
took was opening sessions while diagnosing; close them with `ssh_disconnect` or
`ssh_disconnect_agent` (`docs/API.md:157-269`).
