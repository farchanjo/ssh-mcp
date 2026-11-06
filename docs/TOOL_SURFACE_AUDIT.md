# Tool surface audit

Analysis of why an MCP host can fail to surface the `ssh-mcp` tool list and why
a model mis-selects or mis-calls the tools that do appear. Harness code is
read-only here; every recommendation lands inside this repository.

The machine-readable export of the current surface lives in
[`TOOL_CATALOG.md`](TOOL_CATALOG.md) and [`tool-catalog.json`](tool-catalog.json).

## Verdict

The server is not the reason tools go missing. A full `initialize` +
`tools/list` handshake over stdio returns all 39 tools, 10 prompts, and the
2,172-character instructions block, on protocol `2025-06-18`.

What the handshake does expose is a tool surface that is hard to consume: 97 KB
of JSON Schema, 50 KB of prose, five tools whose parameter types are an
unresolved `$ref`, six output schemas the host silently drops, and several
near-duplicate tools whose titles do not say which one wins. That is the
selection problem.

## Method

```bash
printf '%s\n' \
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' \
'{"jsonrpc":"2.0","method":"notifications/initialized"}' \
'{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
| ./target/release/ssh-mcp-stdio
```

`scripts/export_tool_catalog.py` automates the same exchange and is the source
of both catalogue files.

## What the wire returns

| Measurement | Value |
| --- | --- |
| Tools listed | 39 |
| Prompts listed | 10 |
| `initialize` instructions | 2,172 chars |
| Tool description prose | 50,009 chars |
| Tool input schemas | 47,187 bytes |
| Total catalogue payload | ~97 KB |
| Parameters with a description | 141 / 141 |

The catalogue is complete. Nothing is paginated, filtered, or feature-gated
away at runtime.

## Why tools can still go missing with a client

### 1. The client unregisters the whole catalogue after repeated reconnects

`@deepseek-ai/dsh-mcp-client` registers tools through `syncTools`, which rolls
back the entire generation when registration throws, and unsets every tool after
its reconnect budget is exhausted:

```
giving up after N consecutive failed reconnect attempts — tools unregistered
```

Any crash, `ENOENT` on the binary path, or startup panic therefore reads as
"the server has no tools". The startup log in `~/.dsh/logs/` already shows this
shape for all three stdio servers (`spawn /usr/local/bin/ssh-mcp-stdio ENOENT`)
during a run where the binaries were not installed. Confirm the path before
blaming the catalogue:

```bash
ls -l /usr/local/bin/ssh-mcp-stdio && /usr/local/bin/ssh-mcp-stdio </dev/null
```

### 2. Five tools emit a parameter type the host cannot resolve

The harness enforces a JSON Schema subset of
`type / oneOf / properties / required / additionalProperties / items / enum /
const` plus annotations. `$ref`, `$defs`, and `anyOf` are outside it. MCP input
schemas bypass the compile step, so they reach the provider verbatim — the model
receives a pointer and no values:

| Tool | Property | Emitted shape |
| --- | --- | --- |
| `ssh_connect` | `reuse` | `anyOf: [$ref ReusePolicy, null]` |
| `ssh_commands` | `status` | `anyOf: [$ref CommandStatus, null]` |
| `sub_open` | `lag_policy` | `anyOf: [$ref LagPolicy, null]` |
| `sub_open` | `lifetime` | `anyOf: [$ref LifetimeKind, null]` |
| `ssh_rsync` | `opts`, `transport` | `$ref RsyncOptsArg`, `$ref RsyncTransportArg` |
| `ssh_shell_press` | `key` | `$ref ShellKey` (36 `oneOf` consts, ~4 KB, all behind the pointer) |

`ssh_shell_press.key` is the worst case: required, and its entire value domain
sits in `$defs`.

### 3. Six output schemas are dropped silently

`supportedOutputSchema()` catches the assertion failure and degrades to `{}`.
These tools therefore advertise no output structure at all:
`serial_active`, `ssh_commands`, `ssh_connect`, `ssh_disconnect_many`,
`ssh_exec_batch`, `ssh_sessions`.

## Why a model picks the wrong tool

### Redundant prose crowds out the decision

All 39 descriptions carry `Status values:` and `Errors:`; 36 carry a `Workflow:`
section; 30 repeat the same push-first `Overlap guidance`. Average description
is 1,282 characters, and the three largest (`sub_open` 2,557, `ssh_rsync` 2,336,
`ssh_run` 1,901) spend most of that budget re-teaching a workflow the
`initialize` instructions already state.

Selection signal is a small fraction of each description. The labels that would
help — "when not to use me" — appear in only two tools.

### Overlapping tools with non-disambiguating titles

| Pair | Titles | Conflict |
| --- | --- | --- |
| `ssh_exec` / `ssh_run` | "Run remote command" / "Run remote command (one-shot)" | `ssh_run` is documented as a penalised fallback, but its name reads primary |
| `ssh_exec_output` / `sub_open` | "Get command output" / "Subscribe to a resource lane" | Polling vs push, undecidable from the titles |
| `ssh_shell_read` / `sub_open` | "Read PTY buffer" / "Subscribe to a resource lane" | Same |
| `sub_stats` / `sub_stats_all` | "Snapshot ... lane stats" / "Aggregate ... stats" | Scope differs, wording does not |

### Closed sets use `oneOf` of `const` instead of `enum`

`ReusePolicy`, `LagPolicy`, `LifetimeKind`, `CommandStatus`, `ShellKey`, and
`RsyncTransportArg` all serialise as `oneOf: [{const: ...}]`. `enum` is the
better-supported spelling for a plain string set, and it renders compactly.

### Text defects

`ssh_run` and `ssh_shell_wait_for` both contain a literal `95 %% of the time` —
the `%` was doubled inside a non-`format!` string literal
(`src/infra/mcp/tool_router.rs:2066`, `:2294`).

### Inconsistency

Only the six `serial_*` schemas set `additionalProperties`; the other 33 omit
it. Three list tools (`ssh_commands`, `ssh_sessions`, `sub_list`) declare
properties with no `required` array, so every parameter reads optional.

## Recommended fixes, in priority order

All of these are repository-side.

1. **Flatten the five `$ref` sites.** Add `#[schemars(inline)]` to the argument
   types, or replace the enums with `enum`-derived strings so schemars emits the
   value domain in place. Largest single win for tool-call accuracy.
2. **Give `ssh_shell_press.key` a plain `enum`** and move the per-key modifier
   policy out of the schema into the description.
3. **Make the six dropped output schemas subset-clean** — inline their `$defs`.
4. **Cut description prose to a template:** one line of purpose, `Use when:`,
   `Prefer X instead when:`, then `Status values:` and `Errors:` only. Target
   under 600 characters. Move the shared workflow into the `initialize`
   instructions, which already exist.
5. **Disambiguate the four overlapping pairs in the `title` field**, for example
   `ssh_run` → "Run remote command (one-shot, new connection)".
6. **Set `additionalProperties: false` uniformly** and add `required` arrays to
   the three list tools where an argument is genuinely mandatory.
7. **Fix the two `%%` literals.**

## Regeneration

```bash
make tools-export         # rewrite both catalogue files
make tools-export-check   # exit 1 when the export drifted
```
