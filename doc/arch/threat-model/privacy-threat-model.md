# ssh-mcp LINDDUN privacy threat model

This document is the privacy threat model of the ssh-mcp server, version 7.1.3
(`Cargo.toml:13-16`). It names the personal and sensitive data the server
processes, where each item lives, how long it lives there, who can see it, and
the LINDDUN threat each combination raises. It is the privacy counterpart of
`doc/arch/threat-model/threat-model.md` and shares its assets, entry points and
trust boundaries.

## Scope

The system processes data that belongs to three parties: the operator who runs
the server, the caller who drives the MCP tools, and the people and systems on
the remote hosts. The analysis covers everything the three binaries touch — the
HTTP transport `ssh-mcp` (`src/main.rs:17-20`), the stdio transport
`ssh-mcp-stdio` (`src/bin/ssh_mcp_stdio.rs:17-20`) and the NDJSON daemon
`ssh-mcp-tail` (`src/bin/ssh_mcp_tail.rs:44-45`) — plus the disk writes those
binaries can reach.

Out of scope: the privacy practices of the remote SSH servers, of the MCP host
that reads the responses, and of the operator's log collector. Each of them is
named where it receives data from this server.

## Data inventory

| Data | Where it lives | How long | Who can see it | Evidence |
| --- | --- | --- | --- | --- |
| Password, PEM key material, key passphrase | Process memory in `Credentials` | The call that carries it | The process, and the local user that can read its memory | `src/domain/identity.rs:91-108` |
| Private key file path and SSH agent socket path | Process memory in the tool argument and then in `Credentials` | The call | The process; the path is not echoed in a response | `src/infra/mcp/args/connection.rs:129-131`, `src/domain/identity.rs:137-158` |
| Target host address and port | Process memory, tool responses, log stream | The session for responses; the log sink's own retention for logs | The MCP client, and any reader of the log stream | `src/infra/mcp/args/connection.rs:118-120`, `src/infra/mcp/render/connection.rs:65`, `src/adapters/ssh/internal/client.rs:260` |
| Username of the SSH login | Process memory, tool responses, log stream | The session for responses; the log sink's own retention for logs | The MCP client, and any reader of the log stream | `src/infra/mcp/args/connection.rs:122-123`, `src/infra/mcp/render/connection.rs:99`, `src/adapters/ssh/internal/client.rs:278` |
| Command line text | Process memory, tool responses, log stream on cancel or timeout | The command, or until the response is rendered; the log stream keeps it per the sink | The MCP client, and any reader of the log stream | `src/infra/mcp/render/execute.rs:230`, `src/adapters/ssh/internal/client.rs:778`, `src/adapters/ssh/internal/client.rs:803` |
| Command stdout and stderr | Per-command in-memory buffer, then the rendered response, then the idempotency cache | Bounded by a 10 MiB cap; the 60-second cleanup TTL is declared but not consumed, so the buffer lives until its record leaves the process | The MCP client | `src/adapters/ssh/internal/client.rs:944`, `src/adapters/config/internal/mod.rs:81`, `:471-484` |
| Shell output | Per-shell in-memory ring buffer, then the rendered response | Until the shell closes, or until the process exits when it was still open at a disconnect; the 600-second inactivity TTL is declared but not consumed | The MCP client | `src/adapters/ssh/internal/shell.rs:107-121`, `src/adapters/config/internal/mod.rs:87`, `src/adapters/ssh/russh_adapter.rs:682-706` |
| Records of a disconnected session | The SSH adapter's own `commands` and `shells` maps, inside the process | Until the process exits, unless every shell was closed and every command cancelled before the disconnect | The process | `src/adapters/ssh/russh_adapter.rs:248-249`, `:682-706`, `:747`, `:788` |
| Serial device output | Static in-memory registry per open port | Until the port closes, bounded by a 1 MiB history | The MCP client | `src/adapters/serial/state.rs:51`, `src/adapters/serial/state.rs:261` |
| Local file bytes leaving the host | Transfer stream memory, then the remote host | The transfer | The remote SSH host the caller named | `src/infra/mcp/args/sftp.rs:66`, `src/adapters/sftp/internal/sftp.rs:1021-1061` |
| Local and remote file names | Process memory, tool responses, log stream on the rsync wire path | The transfer, for responses; the log sink's own retention for logs | The MCP client, and any reader of the log stream | `src/infra/mcp/render/sftp.rs:140-142`, `src/adapters/rsync/wire/receiver.rs:628-634` |
| Host keys of contacted hosts | The `known_hosts` file on disk | Until removed by hand | Anyone who can read that file | `src/adapters/ssh/internal/session.rs:91-116`, `src/adapters/config/internal/mod.rs:459-469` |
| Rendered tool responses | In-memory idempotency cache | 300 seconds by default, at most 1024 entries | The process | `src/infra/mcp/idempotency.rs:74-99` |

## What the server persists to disk

The binary keeps no state file, no database and no configuration of its own.
Every repository is an in-memory `DashMap` built by the composition root
(`src/composition/prod.rs:228-231`), and logs are written to stdout for the HTTP
transport and to stderr for stdio (`src/composition/prod.rs:774`,
`src/composition/prod.rs:824`), not to a file the binary manages.

Exactly three writes on disk are reachable from a running server, and each has a
caller who chose it:

1. The host-key record appended by trust-on-first-use, into the path resolved
   from `SSH_MCP_KNOWN_HOSTS` or the home directory
   (`src/adapters/ssh/internal/session.rs:91-116`,
   `src/adapters/config/internal/mod.rs:459-469`).
2. The downloaded file, created or truncated at the caller's `local_path`
   (`src/adapters/sftp/internal/sftp.rs:1206-1209`, reached from
   `src/adapters/sftp/internal/sftp.rs:1170`).
3. The rsync receiver's destination files and its temporary files, under the
   destination root the caller named (`src/adapters/rsync/wire/receiver.rs:530`,
   `src/adapters/rsync/wire/receiver.rs:979`). With `partial` set, a failed
   transfer leaves its temporary file behind on purpose
   (`src/adapters/rsync/wire/receiver.rs:556-563`).

A fourth artifact exists only when the operator creates it: the macOS service
definition redirects the two process streams into
`/usr/local/var/log/ssh-mcp.log` and `/usr/local/var/log/ssh-mcp.error.log`
(`com.farchanjo.ssh-mcp.plist:27-31`). That file is where the log-stream data of
the inventory lands, and its retention is the log subsystem's, not the server's.

## Retention and enforcement

Two retention values are declared in the configuration surface and never
consumed, which changes the real lifetime of two data sets:

- `SSH_COMMAND_CLEANUP_TTL` resolves to 60 seconds
  (`src/adapters/config/internal/mod.rs:80-84`, `:471-484`) and is exposed as
  `ConfigPort::command_cleanup_ttl` (`src/ports/config.rs:35`), but no use case
  reads it: the accessor has no caller outside its own tests.
- `SSH_SHELL_INACTIVITY_TTL` resolves to 600 seconds
  (`src/adapters/config/internal/mod.rs:86-90`) and is exposed as
  `ConfigPort::shell_inactivity_ttl` (`src/ports/config.rs:41`), with the same
  result. The one place a shell-buffer value is touched in a use case discards
  it on purpose: `let _ = self.config.shell_max_buffer_size();`
  (`src/application/read_shell.rs:195-196`).
- `SSH_TRANSFER_CLEANUP_TTL` resolves to 300 seconds
  (`src/adapters/config/internal/mod.rs:99-101`, `:599-610`) and is exposed as
  `ConfigPort::transfer_cleanup_ttl` (`src/ports/config.rs:38`), again with no
  consumer in the application layer.

What those facts mean for the inventory is concrete: a completed command's
output lives until the command entry is removed or the process exits, not for
60 seconds, and a shell's ring buffer lives until the shell closes or the process
exits, not for the 600 seconds the variable suggests.

A disconnect does not free everything either, and the difference is worth
recording exactly. `RusshAdapter::disconnect` removes the session record,
unregisters the session from the shared SFTP handle registry and closes the
forwards bound to it (`src/adapters/ssh/russh_adapter.rs:682-706`); it does not
touch the adapter's own `commands` and `shells` maps (`:248`, `:249`). A command
leaves that map only through `cancel` (`:747`) and a shell only through
`close_shell` (`:788`). The disconnect use case removes the repository rows, and
calls `ssh.cancel` for the commands that are still running
(`src/application/disconnect_session.rs:230-247`, `:252-263`), so after
`ssh_disconnect` the adapter still holds a record — and with it the output
buffer — of every shell of that session and of every command that had already
finished, until the process exits. The repository rows are gone; the bytes are
not.

The consequence for the `docs/OPERATIONS.md:222` statement that a disconnect
cascade-closes every owned shell, command and transfer is that the cascade is a
lifecycle-state and repository-level close: it removes the rows and the lanes,
and does not reclaim the adapter-side buffers of finished commands and shells.
The use case's own comment states the intent for shells — the PTY is reaped with
the transport, so the repository removal is the only side effect it performs
(`src/application/disconnect_session.rs:249-251`).

The ceilings that are enforced, by contrast, are real and traceable: the command
output cap is applied when the buffer is built
(`src/adapters/ssh/internal/client.rs:944`), the shell buffer cap is threaded
into the running shell at open time
(`src/application/open_shell.rs:182-190`,
`src/adapters/ssh/internal/shell.rs:138-147`), the serial history is capped at
1 MiB (`src/adapters/serial/state.rs:51`) and the idempotency cache prunes
oldest-first at its entry ceiling (`src/infra/mcp/idempotency.rs:289`).

## LINDDUN threats

| ID | Category | Threat | Affected Data | Mitigation |
| --- | --- | --- | --- | --- |
| PT-001 | Linking | Repeated calls join one caller to one identity triple over time: the reuse policy matches an existing session on the `(host, port, username)` triple and returns it, so the server holds a per-caller history of who connected where. | Target host, port and username of every connection | The triple is matched in memory only and is dropped with the session; the server keeps no cross-call identity (`src/infra/mcp/args/connection.rs:172-175`, `src/composition/prod.rs:228-231`). Open risk: nothing limits how long a reuse-eligible session is held, since `persistent=true` disables the inactivity timeout (`src/infra/mcp/args/connection.rs:160-165`). |
| PT-002 | Linking | The `known_hosts` file accumulates the addresses of every host the server has contacted, building a linkable record of relationships on disk that outlives the process. | Host addresses and their public keys | The path is configurable, so the server can be given a file of its own instead of the operator's personal one (`src/adapters/config/internal/mod.rs:459-462`). Open risk: the default is the operator's `~/.ssh/known_hosts` (`src/adapters/config/internal/mod.rs:463-468`), and the record persists until a human removes it. |
| PT-003 | Identifying | The log stream names the person behind a connection: retry, failure and success lines carry the username and the address together. | Username and target host address | The lines are emitted at `warn`, `info` and `error` level into the process stream, and the operator chooses the sink and its retention (`src/adapters/ssh/internal/client.rs:260`, `:278`, `:297-299`, `:304`, `src/composition/prod.rs:774`). Open risk: the default filter is `info` (`src/composition/prod.rs:733-737`), so these lines are on by default. |
| PT-004 | Identifying | An SSH agent identity comment is written to the log at debug level, which can carry the label of a personal key. | The comment of an agent-held identity | The line logs the comment rather than the public key or the private material (`src/adapters/ssh/internal/auth/agent.rs:95`), and it fires only when the operator raises the filter to `debug` (`docs/OPERATIONS.md:851`). |
| PT-005 | Non-repudiation | The full command line is written to the log when a command is cancelled or times out, which attests that a specific caller issued that exact command. | Command line text | The log line carries the text, not the caller's identity, so it is attributable only through the operator's own correlation (`src/adapters/ssh/internal/client.rs:778`, `src/adapters/ssh/internal/client.rs:803`). Open risk: the two lines are at `warn` level and therefore active under the default filter. |
| PT-006 | Non-repudiation | The idempotency cache stores the rendered response together with a digest of the arguments for a mutating call, which evidences that the call happened and with what shape. | Rendered tool responses, and a digest of the arguments | The entry expires after `SSH_IDEMPOTENCY_TTL_SECS` (`src/infra/mcp/idempotency.rs:74`), the ceiling prunes the oldest entries (`src/infra/mcp/idempotency.rs:78`, `:289`), and the arguments themselves are not stored — only a 16-hex-character FNV-1a digest (`src/infra/mcp/idempotency.rs:93-96`, `:578-591`). Open risk: the digest is a 64-bit non-cryptographic hash whose input includes credential fields, so it must be treated as non-secret. |
| PT-007 | Detecting | Response metadata reveals how much a remote produced and when: the cursor, the buffer size and the sequence counters describe a session's activity to anyone reading the responses. | Sizes and timing of remote output | On the stdio transport there is one process and one client, so these numbers reach the caller that owns the transport and no other (`src/composition/prod.rs:823-825`). On HTTP each session has its own instance and its own repositories, and a read of another session's resource does not resolve: the reader looks the shell or the command up in its own repositories and answers `SHELL_NOT_FOUND` or `COMMAND_NOT_FOUND` when it is absent (`src/application/read_resource.rs:501`, `:536`, reached from `src/infra/mcp/resource_handlers.rs:379-390`). Open risk: the numbers still cross on two paths. The serial read consults the process-wide registry before any session context and returns the buffer for the id in the URI (`src/infra/mcp/resource_handlers.rs:379-385`, `src/adapters/serial/state.rs:324-325`), so the cursor and buffer size of another session's port are readable; and the inline-push notification carries `seq` and `cursor_after` together with the bytes to any session that opened a lane for that URI (`src/adapters/notifier/rmcp_adapter.rs:119-131`). Both paths are the subject of TM-022 in the STRIDE model. |
| PT-008 | Data-Disclosure | Credentials cross the MCP transport in the clear as ordinary tool arguments, and the HTTP transport terminates no TLS. | Password, PEM key material, key file path, agent socket path | Nothing echoes or logs the secret: the renderers print host, port and username only (`src/infra/mcp/render/connection.rs:65`, `:99`), and the auth adapters log a strategy name rather than the credential (`src/ports/auth_strategy.rs:14`, `src/adapters/ssh/internal/auth/chain.rs:140`). Open risk: the transport is cleartext, and TM-008 of the STRIDE model records the same finding from the attacker's side. |
| PT-009 | Data-Disclosure | Any local file the service user can read can be sent to a host the caller chooses, because the upload path takes the file path as given. | Local file contents | The path is explicit in the argument and echoed in the response (`src/infra/mcp/args/sftp.rs:66`, `src/infra/mcp/render/sftp.rs:137-142`), and the container runs as a dedicated non-root user (`Dockerfile:28-29`). Open risk: no confinement, no allow-list root, no per-call confirmation. |
| PT-010 | Data-Disclosure | File names reach the log stream and the responses, which discloses the shape of a filesystem and the names of documents it holds. | Local and remote file paths | Markdown fields pass through `sanitize_value`, so a name cannot forge an extra response line (`src/infra/mcp/helpers/output.rs:92-106`). Open risk: the rsync wire receiver logs local paths at `warn` on its error paths (`src/adapters/rsync/wire/receiver.rs:628-634`, `:692-696`, `:828-832`), and the log sink decides whether those names survive. |
| PT-011 | Data-Disclosure | Retained buffers hold more output than the response that served it, and the cleanup values that would shorten that life are not consumed anywhere. | Command stdout and stderr, shell output, serial output | The buffers are bounded in size (10 MiB for a command at `src/adapters/config/internal/mod.rs:97`, 10 MiB for a shell at `:93`, 1 MiB for a serial port at `src/adapters/serial/state.rs:51`). Open risk: size is the only bound. The declared TTLs — 60 seconds for a command and 300 for a transfer — have no consumer (`src/ports/config.rs:35`, `:38`), no reaper runs on shell inactivity (`src/ports/config.rs:41`), and a disconnect leaves the adapter's records and their buffers in the process (`src/adapters/ssh/russh_adapter.rs:682-706`, `:747`, `:788`), so a buffer can outlive the session that produced it. |
| PT-012 | Data-Disclosure | Remote output can carry third-party personal data — a database dump, a log file, a configuration listing — straight into the model's context and into the idempotency cache. | Command and shell output, remote file contents | The rendered slice is capped at 16 KiB by default with a 1 MiB ceiling (`src/adapters/config/internal/mod.rs:105-107`), which bounds how much lands at once. Open risk: no classification, no redaction pass and no policy on what a caller may ask the server to read. |
| PT-013 | Unawareness | The server writes into the trust store of the operator's own SSH client without telling the operator, so a connection decision is made in a file the person curates by hand. | Host keys in the operator's `known_hosts` | The write is logged at `info` with the host and port it recorded (`src/adapters/ssh/internal/session.rs:98-104`), and the path can be overridden (`src/adapters/config/internal/mod.rs:459-462`). Open risk: the default path is the personal file, and the log line is the only notice the operator gets. |
| PT-014 | Unawareness | The two configuration values that decide trust with a remote host are absent from every operator-facing document, so a reader cannot know that trust-on-first-use is the default and that an insecure opt-out exists. | The trust decisions of every session, and the host keys they touch | The module documents both variables in its own environment table (`src/adapters/config/internal/mod.rs:22-23`), and the skip path warns on every check (`src/adapters/ssh/internal/session.rs:126-129`). Open risk: neither `docs/CONFIGURATION.md` nor `README.md` names `SSH_MCP_KNOWN_HOSTS` or `SSH_MCP_INSECURE_SKIP_HOST_KEY_CHECK`. |
| PT-015 | Non-Compliance | Log data that identifies a person is retained with no deletion path, no retention limit and no stated purpose beyond diagnostics, and the shipped service definition persists it in a file. | Username, host address, command line text, file paths in log lines | The operator chooses the sink and the filter (`src/composition/prod.rs:733-737`), and the repository states no retention rule for either. Open risk: the service definition redirects the streams into files with no rotation configured (`com.farchanjo.ssh-mcp.plist:27-31`), so retention is unbounded unless the operator sets it. |
| PT-016 | Non-Compliance | There is no erasure path and no data-subject request path: no tool removes a retained buffer, an idempotency entry or a log record, and the deletions that exist are partial. | Every item in the inventory | `ssh_shell_close` removes a shell's record and buffer from the adapter (`src/adapters/ssh/russh_adapter.rs:788`) and `ssh_exec_cancel` does the same for a command (`:747`); a session disconnect removes the repository rows and the lanes, and cancels only the commands still running (`src/application/disconnect_session.rs:230-247`, `:252-263`); the idempotency cache expires and prunes on its own (`src/infra/mcp/idempotency.rs:282-289`). Open risk: nothing erases on request. A finished command's buffer, a shell that was open at the disconnect and every log record survive the action a person can take, because after `ssh_disconnect` the adapter keeps its records until the process exits (`src/adapters/ssh/russh_adapter.rs:248-249`, `:682-706`), and the repository states no request or retention process for either. |

## Reading the table

Four rows carry the highest weight for the people behind the data: PT-001 and
PT-002 join one person's hosts into a lasting record, PT-005 puts command text
into a log the operator may not curate, and PT-008 puts a reusable credential on
a cleartext transport. Three rows are properties of the deployment rather than
of the code: PT-013, PT-014 and PT-015.

Two facts run through the rest of the table and are worth stating once. Lifetimes
are not enforced, only sizes: the cleanup values behind PT-011 have no consumer,
and the disconnect gap that PT-011 and PT-016 record means a buffer can outlive
the session that produced it. And the two transports isolate differently: stdio
serves one client from one process, while HTTP gives each session its own
instance and its own repositories, which is what leaves the serial registry and
the inline-push notification as the two paths by which one session's data can
reach another (PT-007, and TM-022 of the STRIDE model).

The controls that make the model tolerable are worth naming: credentials are
never echoed or logged (PT-008), every retained buffer has a size ceiling that
the code applies (PT-011, PT-012), and the idempotency cache stores a digest
rather than the arguments it fingerprints (PT-006).

## Review

This document is re-read at each constitution review and whenever a cited line
moves (`doc/arch/memory/constitution.md:147-152`). A change to the argument
surface of a tool, to the log stream, to a retention value or to the disk-write
set changes this model, and the change that touches it carries the update.
