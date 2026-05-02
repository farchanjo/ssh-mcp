# Error Code Reference (v3.0.0)

This is the exhaustive catalog of error codes returned by ssh-mcp tools and `resources/*` methods. Every code listed here is grounded in the source — `src/mcp/tools/*.rs` for tools, `src/mcp/resources.rs` for resources.

Cross references:

- [API.md](./API.md) — tool reference.
- [LLM_GUIDE.md](./LLM_GUIDE.md) — recovery decisions per error.
- [RESOURCES.md](./RESOURCES.md) — `resources/*` semantics.

## Format

All tool errors render as a `CallToolResult::error` carrying a markdown text block:

```
TOOL_NAME: ERROR
REASON: [CODE] message
DETAIL: <optional context>
```

`resources/*` errors are returned as proper JSON-RPC errors (`McpError`):

- `INVALID_PARAMS` for malformed URIs / arguments.
- `RESOURCE_NOT_FOUND` for unknown `(scheme, id)` pairs.
- `INTERNAL_ERROR` for registry-level failures (currently never raised — `subscribe` is infallible at runtime).

## ssh_connect

| Code                | Trigger                                                                                                  | Recommended LLM action                                                                                                              |
| ------------------- | -------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `CONNECTION_FAILED` | Handshake failed or all retries exhausted (transient + non-retryable both surface here after the budget). | Inspect `DETAIL`; retry with a longer `timeout_secs` if transient, or with corrected credentials if it mentions auth / permission. |

## ssh_disconnect

| Code                 | Trigger                                       | Recommended LLM action                                                       |
| -------------------- | --------------------------------------------- | ---------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`  | No session with the given `SESSION_ID`.       | Run `ssh_list_sessions` to recover the live ID list, or call `ssh_connect`. |

## ssh_list_sessions

No error codes. Returns an empty list when the optional `agent_id` filter matches nothing or when no sessions are stored. Dead sessions are health-checked and pruned before the response is built.

## ssh_disconnect_agent

No error codes. Unknown `agent_id` returns `SESSIONS: 0` and `COMMANDS: 0`.

## ssh_execute

| Code                     | Trigger                                                                  | Recommended LLM action                                                                |
| ------------------------ | ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`      | No session with the given `SESSION_ID`.                                  | Reconnect via `ssh_connect`.                                                          |
| `MAX_COMMANDS_EXCEEDED`  | Per-session running-command cap (100) reached. `DETAIL: limit=100`.      | Wait for in-flight commands to complete, or `ssh_cancel_command` an obsolete one.     |

## ssh_get_command_output

| Code                | Trigger                                                                                                                    | Recommended LLM action                                                                                                |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `COMMAND_NOT_FOUND` | No async command with the given `COMMAND_ID`. May indicate the command was cleaned up after `SSH_COMMAND_CLEANUP_TTL`.    | Re-issue `ssh_execute`, or rely on the original output captured before the TTL.                                       |
| `COMMAND_FAILED`    | Status flipped to `Failed` (transport error, channel error). The error message lives in `REASON`.                         | Inspect `REASON`; retry the command if the cause looks transient (network), otherwise surface the failure to the user. |

## ssh_list_commands

No error codes. Filters that match nothing return an empty list.

## ssh_cancel_command

| Code                | Trigger                                                                          | Recommended LLM action                              |
| ------------------- | -------------------------------------------------------------------------------- | --------------------------------------------------- |
| `COMMAND_NOT_FOUND` | No async command with the given `COMMAND_ID`.                                    | Already cleaned up — treat as success.              |

`NOOP` is not an error: when the command exists but is no longer running, the tool returns `SSH_CANCEL_COMMAND: NOOP` as a successful `CallToolResult`.

## ssh_shell_open

| Code                     | Trigger                                                                | Recommended LLM action                                                                  |
| ------------------------ | ---------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`      | No session with the given `SESSION_ID`.                                | Reconnect via `ssh_connect`.                                                            |
| `MAX_SHELLS_EXCEEDED`    | Per-session shell cap (10) reached. `DETAIL: limit=10`.                | Close an idle shell with `ssh_shell_close`.                                             |
| `CHANNEL_FAILED`         | russh failed to open the PTY channel. `REASON` carries the russh error. | Inspect; common causes: remote `MaxSessions` exhaustion, kex failure, transport closed. |

## ssh_shell_write

| Code              | Trigger                                                              | Recommended LLM action                                                              |
| ----------------- | -------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `SHELL_NOT_FOUND` | No active shell with the given `SHELL_ID`.                           | Reopen via `ssh_shell_open`.                                                        |
| `WRITE_FAILED`    | The dedicated background writer task closed (russh transport gone). | Treat the shell as dead: call `ssh_shell_close` (idempotent) and reopen if needed. |

## ssh_shell_send_key

| Code                     | Trigger                                                                                              | Recommended LLM action                                                                                       |
| ------------------------ | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `SHELL_NOT_FOUND`        | No active shell with the given `SHELL_ID`.                                                           | Reopen via `ssh_shell_open`.                                                                                 |
| `MODIFIER_NOT_ALLOWED`   | Modifier rejected for the requested `key`. `DETAIL: requested=<modifier set>`.                       | See modifier rules below; if you need a non-standard sequence, fall back to `ssh_shell_write` with raw bytes. |
| `INVALID_REPEAT`         | `repeat` outside the range 1..=64. `DETAIL: requested=<n>`.                                          | Clamp client-side to [1, 64].                                                                                |
| `WRITE_FAILED`           | The dedicated background writer task closed.                                                         | Treat the shell as dead and reopen.                                                                          |

Modifier rules (enforced in `src/mcp/keys.rs`):

- Allowed on arrows, navigation keys (`home`, `end`, `page_up`, `page_down`, `insert`, `delete`), and `f1..f12` — any combination of `shift`, `alt`, `ctrl`.
- `tab` accepts `shift` only (produces back-tab `\x1b[Z`).
- All `ctrl_*` variants, `enter`, `escape`, `backspace`, `space` reject every modifier.

## ssh_shell_read

| Code              | Trigger                                    | Recommended LLM action       |
| ----------------- | ------------------------------------------ | ---------------------------- |
| `SHELL_NOT_FOUND` | No active shell with the given `SHELL_ID`. | Reopen via `ssh_shell_open`. |

`OPEN`, `CLOSED`, and `TIMEOUT` are statuses, not errors.

## ssh_shell_wait_for

| Code                  | Trigger                                                       | Recommended LLM action                                                                  |
| --------------------- | ------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `SHELL_NOT_FOUND`     | No active shell with the given `SHELL_ID`.                    | Reopen via `ssh_shell_open`.                                                            |
| `EMPTY_PATTERNS`      | `patterns` vector was empty.                                  | Pass at least one substring.                                                            |
| `TOO_MANY_PATTERNS`   | `patterns.len() > 16`. `DETAIL: count=<n>`.                   | Group patterns or split the wait into multiple calls.                                   |
| `PATTERN_TOO_LONG`    | A single pattern exceeded 1024 bytes. `DETAIL: len=<n>`.      | Trim the pattern (use a shorter unique prefix).                                         |

`MATCHED`, `TIMEOUT`, and `CLOSED` are statuses, not errors.

## ssh_shell_close

| Code              | Trigger                                    | Recommended LLM action |
| ----------------- | ------------------------------------------ | ---------------------- |
| `SHELL_NOT_FOUND` | No active shell with the given `SHELL_ID`. | Treat as success.      |

## ssh_upload

| Code                       | Trigger                                                                 | Recommended LLM action                                                              |
| -------------------------- | ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`        | No session with the given `SESSION_ID`.                                 | Reconnect via `ssh_connect`.                                                        |
| `MAX_TRANSFERS_EXCEEDED`   | Per-session transfer cap (10) reached. `DETAIL: limit=10`.              | Wait for an in-flight transfer or cancel one via session disconnect.                |
| `LOCAL_FILE_ERROR`         | `fs::metadata` failed on `local_path` (resolved against `$HOME`).       | Inspect `REASON`; verify path, permissions, and that the file is reachable locally. |
| `LOCAL_NOT_FILE`           | `local_path` resolved but is not a regular file (directory, symlink loop, special). | Pass an actual file path.                                                  |

## ssh_download

| Code                       | Trigger                                                                                       | Recommended LLM action                                                                                            |
| -------------------------- | --------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND`        | No session with the given `SESSION_ID`.                                                       | Reconnect via `ssh_connect`.                                                                                      |
| `MAX_TRANSFERS_EXCEEDED`   | Per-session transfer cap (10) reached. `DETAIL: limit=10`.                                    | Wait for an in-flight transfer.                                                                                   |
| `SFTP_OPEN_FAILED`         | Failed to open the SFTP subsystem on the SSH session.                                         | Verify the remote host has SFTP enabled (`Subsystem sftp`); fall back to `ssh_execute` + manual `cat`.            |
| `REMOTE_METADATA_ERROR`    | `sftp.metadata(remote_path)` failed (file missing, permission denied, etc.).                  | Inspect `REASON`; verify the path and remote user permissions.                                                    |

## ssh_get_transfer_progress

| Code                  | Trigger                                                                                                | Recommended LLM action                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| `TRANSFER_NOT_FOUND`  | No transfer with the given `TRANSFER_ID`. May indicate cleanup after `SSH_TRANSFER_CLEANUP_TTL` (300s). | Re-trigger the transfer if needed; otherwise treat as already terminal.         |

`RUNNING`, `COMPLETED`, and `FAILED` are statuses, not errors.

## ssh_forward

| Code                | Trigger                                                                                          | Recommended LLM action                                                                  |
| ------------------- | ------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| `SESSION_NOT_FOUND` | No session with the given `SESSION_ID` (when feature `port_forward` is enabled).                  | Reconnect via `ssh_connect`.                                                            |
| `FEATURE_DISABLED`  | Build was compiled without `--features port_forward`. `DETAIL: rebuild with --features port_forward`. | Use a build with the feature enabled (default).                                         |
| `FORWARD_FAILED`    | Listener bind or russh stream-open failed.                                                        | Inspect `REASON`; common causes: local port already in use, remote target unreachable. |

## resources/list

No errors. Returns an empty list when no resources are registered.

## resources/read

| Code                 | Trigger                                                                                | Recommended action                                                |
| -------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `INVALID_PARAMS`     | URI parser error (`BadScheme`, `MissingId`, `BadSubPath`, `BadCursor`).                | Reformat the URI per [RESOURCES.md](./RESOURCES.md) URI grammar.  |
| `RESOURCE_NOT_FOUND` | The `(scheme, id)` pair does not match any live resource (excluding `forward://` which always succeeds). | Run `resources/list` to recover the live URI catalogue.            |

## resources/subscribe

| Code                 | Trigger                                                                                                                  | Recommended action                                              |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------- |
| `INVALID_PARAMS`     | URI parser error.                                                                                                        | Reformat the URI per the URI grammar.                           |
| `RESOURCE_NOT_FOUND` | `(scheme, id)` is not registered. `forward://` URIs always succeed (no enumerable storage layer yet).                    | Wait for the producer to register, or pick a different URI.     |

## resources/unsubscribe

Idempotent — no error if the URI is not currently subscribed. May still return `INVALID_PARAMS` for malformed URIs.
