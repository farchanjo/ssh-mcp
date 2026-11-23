# Research: Documentation Drift Corrections

Evidence gathered on 2026-11-22 at `aac7fc4`. Every `file:line` was printed
before it was quoted. The tree was not modified; the lock measurement ran in
`/var/tmp/ssh-mcp/lockcheck3` and the daemon was driven from the already built
`target/debug/ssh-mcp-tail`.

## R-1 The stale-lock instruction (FR-001)

`docs/MIGRATION.md:882` ends: `A stale Cargo.lock must be re-resolved (cargo
update -p russh -p russh-sftp) before it builds against the new lines.` The same
section says at `:888` that the later moves need no manual step.

Measurement:

```
$ mkdir -p /var/tmp/ssh-mcp/lockcheck3
$ rsync -a --exclude target --exclude .git --exclude .venv --exclude dist \
    --exclude '*.tar.gz' --exclude doc --exclude docs --exclude scripts \
    --exclude .pytest_cache /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/lockcheck3/
$ cp /tmp/ssh-mcp-russh-base060/Cargo.lock /var/tmp/ssh-mcp/lockcheck3/Cargo.lock
before: russh 0.60.3, russh-sftp 2.4.0, rmcp 1.8.0, base64 0.22.1,
        sha2 0.10.9 and 0.11.1, tower-http 0.6.11
$ cd /var/tmp/ssh-mcp/lockcheck3 && cargo metadata --format-version 1 >/dev/null
EXIT:0
    Updating crates.io index
     Locking 69 packages to highest Rust 1.99 compatible versions
    Updating rmcp v1.8.0 -> v3.11.0
    Updating russh v0.60.3 -> v0.65.4
    Updating russh-sftp v2.4.0 -> v4.0.0
    Updating tower-http v0.6.11 -> v0.7.1
after: russh 0.65.4, russh-sftp 4.0.0, rmcp 3.11.0, base64 0.23.1,
       sha2 0.11.1, tower-http 0.7.1
$ diff -q /var/tmp/ssh-mcp/lockcheck3/Cargo.lock /root/dev/ssh-mcp/Cargo.lock
DIFF_EXIT:0
```

Verdict: false. `CHANGELOG.md:18` and the feature 001 records are true as
written and stay.

## R-2 The resource error mapping (FR-002)

`map_resource_error` (`src/infra/mcp/resource_handlers.rs:72-79`) builds
`McpError::internal_error(err.to_string(), None)` for the `Internal` category.
No code name and no `data` cross the wire; the message is the domain `Display`.
The category table is `resource_error_category` (`:96-145`).

| Request | `DomainError` | Category | JSON-RPC | Display (`src/domain/error.rs`) |
| --- | --- | --- | --- | --- |
| subscribe | `InvalidArgument` (`src/application/subscribe_resource.rs:146`) | `InvalidParams` (`:98-100`) | -32602 `invalid_params` | `invalid argument: {0}` (`:78-79`) |
| subscribe | `ShellNotFound` (`src/application/subscribe_resource.rs:233`) | `NotFound` (`:103-115`) | -32002 `resource_not_found` | `shell not found: {0}` (`:26-27`) |
| subscribe | `CommandNotFound` (`src/application/subscribe_resource.rs:244`) | `NotFound` | -32002 | `command not found: {0}` (`:22-23`) |
| subscribe | `TransferNotFound` (`src/application/subscribe_resource.rs:255`) | `NotFound` | -32002 | `transfer not found: {0}` (`:30-31`) |
| subscribe | `SessionNotFound` (`src/application/subscribe_resource.rs:266`) | `NotFound` | -32002 | `session not found: {0}` (`:18-19`) |
| subscribe | `ResourceGone` (`src/adapters/lifecycle/refcount.rs:420`, `:484`) | `NotFound` | -32002 | `resource gone: {0}` (`:130-131`) |
| subscribe | `MaxSubsPerUriExceeded` (`src/adapters/subscription/memory_registry.rs:552`; `src/adapters/subscription/subscriber_lane.rs:531`) | `Internal` (`:130-131`) | -32603 `internal_error` | `max subs per URI exceeded for {uri} (limit {limit})` (`:161-167`) |
| subscribe | `MaxSubsTotalExceeded` (`src/adapters/subscription/memory_registry.rs:558`; `src/adapters/subscription/subscriber_lane.rs:470`) | `Internal` | -32603 | `max subs total exceeded (limit {limit})` (`:171-175`) |
| unsubscribe | `InvalidArgument` (`src/application/unsubscribe_resource.rs:100`) | `InvalidParams` | -32602 | `invalid argument: {0}` |
| unsubscribe | `Internal` (`src/adapters/lifecycle/refcount.rs:552-554`) | `Internal` | -32603 | `internal error: {0}` (`:123-124`) |
| read | `InvalidArgument` (`src/application/read_resource.rs:437`, `:472-479`; `src/infra/mcp/resource_handlers.rs:749-751`) | `InvalidParams` | -32602 | `invalid argument: {0}` |
| read | `ShellNotFound` (`src/application/read_resource.rs:501`; `src/adapters/output_stream/russh_output.rs:142`) | `NotFound` | -32002 | `shell not found: {0}` |
| read | `CommandNotFound` (`src/application/read_resource.rs:536`; `src/adapters/output_stream/russh_output.rs:121`) | `NotFound` | -32002 | `command not found: {0}` |
| read | `TransferNotFound` (`src/application/read_resource.rs:594`) | `NotFound` | -32002 | `transfer not found: {0}` |
| read | `SessionNotFound` (`src/application/read_resource.rs:631`) | `NotFound` | -32002 | `session not found: {0}` |
| read | `ForwardNotFound` (`src/application/read_resource.rs:672`) | `NotFound` | -32002 | `forward not found: {0}` (`:34-35`) |
| read | `ResourceGone` (`src/infra/mcp/resource_handlers.rs:531`, rsync read) | `NotFound` | -32002 | `resource gone: {0}` |

Cap values: `DEFAULT_MAX_SUBS_PER_URI` 16
(`src/adapters/config/internal/mod.rs:200`), `DEFAULT_MAX_SUBS_TOTAL` 1024
(`:205`), both wired in production (`src/composition/prod.rs:328-329`).

## R-3 The daemon capture (FR-006)

`ssh-mcp-tail --help` names the `daemon` subcommand. `docs/DAEMON.md:84-88`
gives the NDJSON input shape (one `op` object per line, optional `id`). Two
runs, each with stdin closed at the end:

```
$ printf '%s\n' '{"op":"subscribe","uri":"shell://nope/output","id":"drift-1"}' \
    | timeout 20 ./target/debug/ssh-mcp-tail daemon
EXIT:0
{"ev":"err","id":"drift-1","code":"SUBSCRIBE_FAILED","reason":"subscribe shell://nope/output failed","detail":"Mcp error: -32002: shell not found: nope"}
```

```
$ for i in $(seq 1 17); do printf '{"op":"subscribe","uri":"forward://x/events","id":"cap-%02d"}\n' "$i"; done \
    | timeout 30 ./target/debug/ssh-mcp-tail daemon
EXIT:0
... 16 lines {"ev":"ack","id":"cap-01","uri":"forward://x/events"} ... {"ev":"ack","id":"cap-16",...}
{"ev":"err","id":"cap-17","code":"SUBSCRIBE_FAILED","reason":"subscribe forward://x/events failed","detail":"Mcp error: -32603: max subs per URI exceeded for forward://x/events (limit 16)"}
```

So the event code is always `SUBSCRIBE_FAILED` (`src/embed/dispatcher.rs:596-601`)
and `detail` is the rmcp error text: `Mcp error: <json-rpc code>: <domain
display>`, not a code list. The stderr of both runs logs
`response error id=1 error=ErrorData { code: ErrorCode(-32002), message: "shell
not found: nope", data: None }` and the `-32603` counterpart.

## R-4 The filter cap (FR-004)

`DomainError::InvalidArgument` is the compile failure
(`src/adapters/subscription/filter.rs:56-59`); `classify_error` falls through to
`INVALID_ARGUMENT` because the reason carries no tag
(`src/infra/mcp/tool_router.rs:781-786`; `ARG_TAGS` at `:587-594`). No
`FILTER_INVALID` string exists in `src`.

`resolve_filter_regex_max` (`src/adapters/config/internal/mod.rs:906-911`) is
called only by its own test at `:1325`; `LSP findReferences` returns two
references. The cap is resolved and applied by no production caller, so
`docs/CONFIGURATION.md:153` keeps the intended promise and the threat model
records the gap.

## R-5 Excluded statements met

`sub_pause`, `auto_close`/`lease`, `sub_replay`, `resources/unsubscribe`,
`release_when_no_subs`, the three cleanup TTLs, `SSH_INACTIVITY_TIMEOUT` and
`persistent` are listed in `session.md:100-136` as verified defects; their
documents describe the intended behaviour and stay. `docs/LLM_GUIDE.md:1665`
and `:1675` document `INVALID_LIFETIME` and `INVALID_LAG_POLICY`, which no
production path constructs (`src/domain/error.rs:209`, `:214`); they are
recorded as defects in FR-009 rather than corrected here.

## R-6 Baseline

`speckit validate` before the feature: exit 6, 70 errors, no warning; 46
`fqcn.relative-super`, 8 `branch.else`, 5 `pin.ci.unpinned-action`, 2
`pin.docker.unpinned-image`, and one each of nine other rules. `speckit verify
--changed`: 12 passed, 0 failed, 42 unbound after the two new scenarios. The
source tree equals `/var/tmp/ssh-mcp/bumps-spike` (`diff -r` of `src` and
`tests`, `diff` of `Cargo.toml`: no output).
