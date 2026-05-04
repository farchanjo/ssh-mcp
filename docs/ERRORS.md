# Error Code Reference (v4.8.0)

This is the exhaustive catalog of error codes returned by ssh-mcp tools and `resources/*` methods. Error wire shapes are byte-compatible with v3.0.0 (see [MIGRATION_v3_to_v4.md](./MIGRATION_v3_to_v4.md)). v4.5 promoted 14 tag-prefixed reasons into granular wire codes; v4.6 wired the last three reserved tags (`FORWARD_FAILED`, `LOCAL_NOT_FILE`, `REMOTE_METADATA_ERROR`) to live raise sites; v4.7 adds one new code — `IDEMPOTENCY_KEY_TOO_LONG` — and extends every NOT_FOUND row with a closest-match suggestion in `DETAIL`. See [Granular tag dispatcher](#granular-tag-dispatcher) below. Every code listed here is grounded in the source — `src/application/*.rs` for use case validation, `src/infra/mcp/{tool_router,resource_handlers,helpers/error,idempotency,suggestions}.rs` for the rmcp-facing error mapping, and `src/domain/error.rs` for the central `DomainError` enum.

> **v4.8 — no new error codes.** v4.8 is strictly additive on `tools/list[].outputSchema` advertisement; the structured error envelope (`{ tool, status: "error", code, reason, detail }`) is byte-identical to v4.7.1. Every code, every emission site, every recovery hint listed below carries forward unchanged.

## v4.7 NOT_FOUND closest-match suggestions

When `SESSION_NOT_FOUND`, `SHELL_NOT_FOUND`, `COMMAND_NOT_FOUND`, `TRANSFER_NOT_FOUND`, or `FORWARD_NOT_FOUND` fires and the relevant repo holds at least one live entry, the `DETAIL:` line carries `closest matches: <id1>, <id2>, <id3>` (top-3 Levenshtein neighbors of the supplied id). Smaller LLMs recover from typos without round-tripping `ssh_list_*`. Reference: `src/infra/mcp/suggestions.rs::closest_ids` (top-N picker) + `levenshtein` (byte-level edit distance, lock-free, deterministic tie-break on lexicographic order). The composition root injects an `IdLister` per repo (`src/composition/id_lister.rs`) so the dispatcher can populate the candidate set without taking a lock. When the repo is empty the suggestion clause is omitted — the `DETAIL:` line falls back to its v4.6 shape.

Cross references:

- [API.md](./API.md) — tool reference.
- [LLM_GUIDE.md](./LLM_GUIDE.md) — recovery decisions per error (see section B for the granular-codes summary).
- [RESOURCES.md](./RESOURCES.md) — `resources/*` semantics.

## Format

All tool errors render as a `CallToolResult::error` carrying a markdown text block, with a parallel structured JSON twin (v4.7) on the same response:

```
TOOL_NAME: ERROR
REASON: [CODE] message
DETAIL: <optional context>
```

```json
{ "tool": "ssh_execute", "status": "error", "code": "SESSION_NOT_FOUND",
  "reason": "no session with id sess-x",
  "detail": "closest matches: sess-1, sess-a" }
```

The structured channel is byte-compatible across v4.6 hosts that ignore `structured_content`; the text channel stays identical to v4.6.

`resources/*` errors are returned as proper JSON-RPC errors (`McpError`):

- `INVALID_PARAMS` for malformed URIs / arguments.
- `RESOURCE_NOT_FOUND` for unknown `(scheme, id)` pairs.
- `INTERNAL_ERROR` for registry-level failures (currently never raised — `subscribe` is infallible at runtime).

## Granular tag dispatcher

`src/infra/mcp/tool_router.rs::classify_error` checks a `DomainError` reason for a `TAG: message` prefix and promotes the tag to the wire `CODE`. Three buckets:

- `ARG_TAGS` (matched against `DomainError::InvalidArgument`): `EMPTY_PATTERNS`, `TOO_MANY_PATTERNS`, `PATTERN_TOO_LONG`, `MODIFIER_NOT_ALLOWED`, `INVALID_REPEAT`, `FEATURE_DISABLED`. Untagged messages emit `INVALID_ARGUMENT`.
- `TRANSPORT_TAGS` (matched against `DomainError::Transport`): `WRITE_FAILED`, `CHANNEL_FAILED`, `COMMAND_FAILED`, `FORWARD_FAILED`. Untagged messages emit `TRANSPORT_ERROR`.
- `SFTP_TAGS` (matched against `DomainError::Sftp`): `LOCAL_FILE_ERROR`, `LOCAL_NOT_FILE`, `SFTP_OPEN_FAILED`, `REMOTE_METADATA_ERROR`. Untagged messages emit `SFTP_ERROR`.

**v4.6**: all 14 documented tags reach the wire. The "Reserved" column is now empty — `FORWARD_FAILED`, `LOCAL_NOT_FILE`, and `REMOTE_METADATA_ERROR` are wired to concrete raise sites in v4.6 (see the per-tool tables below).

**v4.7**: one new code, `IDEMPOTENCY_KEY_TOO_LONG`, is raised by every mutating tool wrapper when the caller supplies an oversized `_meta.idempotency_key`. See [v4.7 idempotency error](#v47-idempotency-error) below.

## v4.7 idempotency error

| Code                      | Trigger                                                                                              | Emitted from                                                            | Recommended LLM action                                                                                                       |
| ------------------------- | ---------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `IDEMPOTENCY_KEY_TOO_LONG` | `_meta.idempotency_key` exceeds 256 bytes (`IDEMPOTENCY_KEY_MAX_BYTES`). The use case is NOT executed. | `src/infra/mcp/idempotency.rs::extract_idempotency_key` (returns `KeyOutcome::TooLong`). | Trim the key client-side. The cap is sized for UUID-style values (UUIDv4 is 36 bytes); larger payloads are rejected to bound the cache. |

Empty keys are treated as absent (idempotency OFF for that call), so callers do not have to special-case the missing-vs-empty distinction. See [LLM_GUIDE.md section J](./LLM_GUIDE.md#j-idempotency-v47) for the full request envelope shape and the list of mutating tools that honour the key.

## ssh_connect

| Code                | Trigger                                                                                                  | Emitted from                                                  | Recommended LLM action                                                                                                              |
| ------------------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `CONNECTION_FAILED` | Handshake failed or all retries exhausted (transient + non-retryable both surface here after the budget). | `src/adapters/ssh/russh_adapter.rs::connect`                  | Inspect `DETAIL`; retry with a longer `timeout_secs` if transient, or with corrected credentials if it mentions auth / permission. |
| `AUTH_FAILED`       | Auth chain (password -> key -> agent) exhausted with no successful method.                              | `src/adapters/auth/auth_chain.rs::authenticate`               | Verify credentials; check that the SSH agent is reachable.                                                                          |

## ssh_disconnect

| Code                 | Trigger                                       | Emitted from                                                | Recommended LLM action                                                       |
| -------------------- | --------------------------------------------- | ----------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`  | No session with the given `SESSION_ID`.       | `src/application/disconnect.rs::execute`                    | Run `ssh_list_sessions` to recover the live ID list, or call `ssh_connect`. v4.7: when at least one live session exists, `DETAIL:` carries `closest matches: <id1>, <id2>, <id3>` so smaller LLMs can recover from typos without re-listing — see `src/infra/mcp/suggestions.rs::closest_ids`. |
| `TRANSPORT_ERROR`    | russh transport failed during teardown.       | `src/adapters/ssh/russh_adapter.rs::disconnect`             | Treat as success (the session is gone either way). Surface details if needed. |

## ssh_list_sessions

No error codes. Returns an empty list when the optional `agent_id` filter matches nothing or when no sessions are stored. Dead sessions are health-checked and pruned before the response is built.

## ssh_disconnect_agent

No error codes. Unknown `agent_id` returns `SESSIONS: 0` and `COMMANDS: 0`.

## ssh_execute

| Code                     | Trigger                                                                  | Emitted from                                                       | Recommended LLM action                                                                |
| ------------------------ | ------------------------------------------------------------------------ | ------------------------------------------------------------------ | ------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`      | No session with the given `SESSION_ID`.                                  | `src/application/execute_command.rs::execute`                      | Reconnect via `ssh_connect`. v4.7: `DETAIL: closest matches: ...` populated when the session repo holds at least one live entry. |
| `MAX_COMMANDS_EXCEEDED`  | Per-session running-command cap (100) reached. `DETAIL: limit=100`.      | `src/application/execute_command.rs::execute`                      | Wait for in-flight commands to complete, or `ssh_cancel_command` an obsolete one.     |
| `CHANNEL_FAILED`         | russh failed to open the exec channel. Tagged transport.                | `src/adapters/ssh/russh_adapter.rs::execute_command`              | Inspect; common causes: remote `MaxSessions` exhaustion, kex failure.                 |
| `TRANSPORT_ERROR`        | russh transport error not covered by a tag.                              | `src/adapters/ssh/russh_adapter.rs::execute_command`              | Inspect; consider a fresh session (`ssh_connect reuse=force_new`).                    |

## ssh_get_command_output

| Code                | Trigger                                                                                                                    | Emitted from                                                       | Recommended LLM action                                                                                                |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------- |
| `COMMAND_NOT_FOUND` | No async command with the given `COMMAND_ID`. May indicate the command was cleaned up after `SSH_COMMAND_CLEANUP_TTL`.    | `src/application/get_command_output.rs::execute`                   | Re-issue `ssh_execute`, or rely on the original output captured before the TTL. v4.7: `DETAIL:` carries top-3 Levenshtein neighbors when the command repo is non-empty. |
| `COMMAND_FAILED`    | Status flipped to `Failed` (transport error, exec channel died mid-run). Tagged transport.                                | `src/adapters/ssh/russh_adapter.rs::execute_command`              | Inspect `REASON`; retry the command if the cause looks transient (network), otherwise surface the failure to the user. |

## ssh_list_commands

No error codes. Filters that match nothing return an empty list.

## ssh_cancel_command

| Code                | Trigger                                                                          | Emitted from                                                | Recommended LLM action                              |
| ------------------- | -------------------------------------------------------------------------------- | ----------------------------------------------------------- | --------------------------------------------------- |
| `COMMAND_NOT_FOUND` | No async command with the given `COMMAND_ID`.                                    | `src/application/cancel_command.rs::execute`               | Already cleaned up — treat as success. v4.7: `DETAIL:` carries closest matches from the command repo when the repo is non-empty. |

`NOOP` is not an error: when the command exists but is no longer running, the tool returns `SSH_CANCEL_COMMAND: NOOP` as a successful `CallToolResult`.

## ssh_shell_open

| Code                     | Trigger                                                                | Emitted from                                                       | Recommended LLM action                                                                  |
| ------------------------ | ---------------------------------------------------------------------- | ------------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`      | No session with the given `SESSION_ID`.                                | `src/application/open_shell.rs::execute`                           | Reconnect via `ssh_connect`. v4.7: `DETAIL:` carries closest matches from the session repo when non-empty. |
| `MAX_SHELLS_EXCEEDED`    | Per-session shell cap (10) reached. `DETAIL: limit=10`.                | `src/application/open_shell.rs::execute`                           | Close an idle shell with `ssh_shell_close`.                                             |
| `CHANNEL_FAILED`         | russh failed to open the PTY channel. Tagged transport.               | `src/adapters/ssh/russh_adapter.rs::open_pty_shell`               | Inspect; common causes: remote `MaxSessions` exhaustion, kex failure, transport closed. |
| `TRANSPORT_ERROR`        | russh transport error not covered by a tag.                            | `src/adapters/ssh/russh_adapter.rs::open_pty_shell`               | Inspect; consider a fresh session (`ssh_connect reuse=force_new`).                      |

## ssh_shell_write

| Code              | Trigger                                                              | Emitted from                                                              | Recommended LLM action                                                              |
| ----------------- | -------------------------------------------------------------------- | ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `SHELL_NOT_FOUND` | No active shell with the given `SHELL_ID`.                           | `src/application/write_shell.rs::execute`                                | Reopen via `ssh_shell_open`. v4.7: `DETAIL:` carries closest matches from the shell repo when non-empty. |
| `WRITE_FAILED`    | The dedicated background writer task closed (russh transport gone). | `src/adapters/ssh/russh_adapter.rs::send_shell_data` (tagged transport) | Treat the shell as dead: call `ssh_shell_close` (idempotent) and reopen if needed. |
| `TRANSPORT_ERROR` | russh transport error not covered by a tag.                          | `src/adapters/ssh/russh_adapter.rs::send_shell_data`                     | Inspect; reopen the shell.                                                          |

## ssh_shell_send_key

| Code                     | Trigger                                                                                              | Emitted from                                                              | Recommended LLM action                                                                                       |
| ------------------------ | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `SHELL_NOT_FOUND`        | No active shell with the given `SHELL_ID`.                                                           | `src/application/send_key.rs::execute`                                   | Reopen via `ssh_shell_open`. v4.7: `DETAIL:` carries closest matches from the shell repo when non-empty. |
| `MODIFIER_NOT_ALLOWED`   | Modifier rejected for the requested `key` (tagged invalid argument).                                  | `src/application/send_key.rs::validate_modifiers`                        | See modifier rules below; if you need a non-standard sequence, fall back to `ssh_shell_write` with raw bytes. |
| `INVALID_REPEAT`         | `repeat` outside the range 1..=64 (tagged invalid argument).                                          | `src/application/send_key.rs::validate_repeat`                           | Clamp client-side to [1, 64].                                                                                |
| `WRITE_FAILED`           | The dedicated background writer task closed (tagged transport).                                       | `src/adapters/ssh/russh_adapter.rs::send_shell_data`                     | Treat the shell as dead and reopen.                                                                          |
| `TRANSPORT_ERROR`        | russh transport error not covered by a tag.                                                           | `src/adapters/ssh/russh_adapter.rs::send_shell_data`                     | Inspect; reopen the shell.                                                                                   |

Modifier rules (enforced in `src/domain/keys.rs`):

- Allowed on arrows, navigation keys (`home`, `end`, `page_up`, `page_down`, `insert`, `delete`), and `f1..f12` — any combination of `shift`, `alt`, `ctrl`.
- `tab` accepts `shift` only (produces back-tab `\x1b[Z`).
- All `ctrl_*` variants, `enter`, `escape`, `backspace`, `space` reject every modifier.

## ssh_shell_read

| Code              | Trigger                                    | Emitted from                                  | Recommended LLM action       |
| ----------------- | ------------------------------------------ | --------------------------------------------- | ---------------------------- |
| `SHELL_NOT_FOUND` | No active shell with the given `SHELL_ID`. | `src/application/read_shell.rs::execute`     | Reopen via `ssh_shell_open`. v4.7: `DETAIL:` carries closest matches from the shell repo when non-empty. |

`OPEN`, `CLOSED`, and `TIMEOUT` are statuses, not errors.

## ssh_shell_wait_for

| Code                  | Trigger                                                       | Emitted from                                                                | Recommended LLM action                                                                  |
| --------------------- | ------------------------------------------------------------- | --------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `SHELL_NOT_FOUND`     | No active shell with the given `SHELL_ID`.                    | `src/application/wait_for_pattern.rs::execute`                             | Reopen via `ssh_shell_open`. v4.7: `DETAIL:` carries closest matches from the shell repo when non-empty. |
| `EMPTY_PATTERNS`      | `patterns` vector was empty (tagged invalid argument).         | `src/application/wait_for_pattern.rs::validate_patterns`                   | Pass at least one substring.                                                            |
| `TOO_MANY_PATTERNS`   | `patterns.len() > 16` (tagged invalid argument).               | `src/application/wait_for_pattern.rs::validate_patterns`                   | Group patterns or split the wait into multiple calls.                                   |
| `PATTERN_TOO_LONG`    | A single pattern exceeded 1024 bytes (tagged invalid argument).| `src/application/wait_for_pattern.rs::validate_patterns`                   | Trim the pattern (use a shorter unique prefix).                                         |

`MATCHED`, `TIMEOUT`, and `CLOSED` are statuses, not errors.

## ssh_shell_close

| Code              | Trigger                                    | Emitted from                                   | Recommended LLM action |
| ----------------- | ------------------------------------------ | ---------------------------------------------- | ---------------------- |
| `SHELL_NOT_FOUND` | No active shell with the given `SHELL_ID`. | `src/application/close_shell.rs::execute`     | Treat as success. v4.7: `DETAIL:` carries closest matches from the shell repo when non-empty. |

## ssh_upload

| Code                       | Trigger                                                                                 | Emitted from                                                              | Recommended LLM action                                                              |
| -------------------------- | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`        | No session with the given `SESSION_ID`.                                                 | `src/application/upload_file.rs::execute`                                | Reconnect via `ssh_connect`. v4.7: `DETAIL:` carries closest matches from the session repo when non-empty. |
| `MAX_TRANSFERS_EXCEEDED`   | Per-session transfer cap (10) reached. `DETAIL: limit=10`.                              | `src/application/upload_file.rs::execute`                                | Wait for an in-flight transfer or cancel one via session disconnect.                |
| `LOCAL_FILE_ERROR`         | `fs::metadata` failed on `local_path` (tagged SFTP via `sftp_error_tag`).               | `src/adapters/sftp/russh_sftp_adapter.rs::sftp_error_tag` (operation `stat`) | Inspect `REASON`; verify path, permissions, and that the file is reachable locally. |
| `LOCAL_NOT_FILE`           | `local_path` resolved but is not a regular file (directory, symlink loop, special file). v4.6 live. | `src/application/upload_file.rs::UploadFileUseCase::guard_local_path_is_file` | Pass an actual regular-file path. |
| `SFTP_ERROR`               | Untagged catch-all for `DomainError::Sftp` (any other SFTP failure).                    | `src/adapters/sftp/russh_sftp_adapter.rs`                                | Inspect `REASON`; check remote disk, permissions, SFTP availability.                |

## ssh_download

| Code                       | Trigger                                                                                                | Emitted from                                                              | Recommended LLM action                                                                                            |
| -------------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`        | No session with the given `SESSION_ID`.                                                                | `src/application/download_file.rs::execute`                              | Reconnect via `ssh_connect`. v4.7: `DETAIL:` carries closest matches from the session repo when non-empty. |
| `MAX_TRANSFERS_EXCEEDED`   | Per-session transfer cap (10) reached. `DETAIL: limit=10`.                                             | `src/application/download_file.rs::execute`                              | Wait for an in-flight transfer.                                                                                   |
| `SFTP_OPEN_FAILED`         | Failed to open the SFTP subsystem on the SSH session (tagged SFTP via `sftp_error_tag`).               | `src/adapters/sftp/russh_sftp_adapter.rs::sftp_error_tag` (operation `open`) | Verify the remote host has SFTP enabled (`Subsystem sftp`); fall back to `ssh_execute` + manual `cat`.            |
| `REMOTE_METADATA_ERROR`    | Remote `stat` failed during download — file missing, permission denied, transport blip mid-stat. v4.6 live. | `src/adapters/sftp/russh_sftp_adapter.rs::stat_remote_size` | Inspect `REASON`; verify the remote path and the download user's permissions. |
| `SFTP_ERROR`               | Untagged catch-all for `DomainError::Sftp`.                                                            | `src/adapters/sftp/russh_sftp_adapter.rs`                                | Inspect `REASON`; check remote disk, permissions, SFTP availability.                                              |

## ssh_get_transfer_progress

| Code                  | Trigger                                                                                                | Emitted from                                                          | Recommended LLM action                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `TRANSFER_NOT_FOUND`  | No transfer with the given `TRANSFER_ID`. May indicate cleanup after `SSH_TRANSFER_CLEANUP_TTL` (300s). | `src/application/get_transfer_progress.rs::execute`                  | Re-trigger the transfer if needed; otherwise treat as already terminal. v4.7: `DETAIL:` carries closest matches from the transfer repo when non-empty. |

`RUNNING`, `COMPLETED`, and `FAILED` are statuses, not errors.

## ssh_forward

| Code                | Trigger                                                                                                                                              | Emitted from                                                              | Recommended LLM action                                                                  |
| ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND` | No session with the given `SESSION_ID` (when feature `port_forward` is enabled).                                                                     | `src/application/forward_port.rs::execute`                               | Reconnect via `ssh_connect`. v4.7: `DETAIL:` carries closest matches from the session repo when non-empty. |
| `PORT_IN_USE`       | Local port already bound. `DETAIL: port=<n>`.                                                                                                        | `src/adapters/ssh/russh_adapter.rs::start_port_forward`                  | Pick a different `local_port` or release the current binder.                            |
| `FEATURE_DISABLED`  | Resource subscribe to `forward://` on a build compiled without `--features port_forward` (tagged invalid argument).                                   | `src/application/read_resource.rs::execute` + `subscribe_resource.rs::execute` | Use a build with the feature enabled (default).                                         |
| `FORWARD_FAILED`    | Local listener bind failed for reasons other than `AddrInUse` (e.g. `EACCES` on a privileged port, `EADDRNOTAVAIL` on a host without the requested address, IPv6/IPv4 family mismatch). v4.6 live. | `src/application/forward_port.rs::ForwardPortUseCase::preflight_bind` | Inspect `REASON`; common causes: insufficient privileges (try a port >= 1024), invalid bind address, or the host lacks the requested family. `PORT_IN_USE` is still emitted separately for `AddrInUse`. |

## resources/list

No errors. Returns an empty list when no resources are registered.

## resources/read

| Code                 | Trigger                                                                                                                                  | Recommended action                                                                       |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `INVALID_PARAMS`     | URI parser error (`BadScheme`, `MissingId`, `BadSubPath`, `BadCursor`); on a build without `port_forward`, also raised by `FEATURE_DISABLED` for `forward://`. | Reformat the URI per [RESOURCES.md](./RESOURCES.md) URI grammar; rebuild with `port_forward` if you need `forward://`. |
| `RESOURCE_NOT_FOUND` | The `(scheme, id)` pair does not match any live resource (now also enforced for `forward://` when `port_forward` is enabled).             | Run `resources/list` to recover the live URI catalogue.                                  |

## resources/subscribe

| Code                 | Trigger                                                                                                                  | Recommended action                                              |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------- |
| `INVALID_PARAMS`     | URI parser error; on a build without `port_forward`, also raised by `FEATURE_DISABLED` for `forward://`.                  | Reformat the URI per the URI grammar; rebuild with `port_forward` if needed. |
| `RESOURCE_NOT_FOUND` | `(scheme, id)` is not registered (enforced for every scheme including `forward://` when the feature is enabled).         | Wait for the producer to register, or pick a different URI.     |

## resources/unsubscribe

Idempotent — no error if the URI is not currently subscribed. May still return `INVALID_PARAMS` for malformed URIs.
