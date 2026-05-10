# ADR 0011: Rsync hybrid transport (wire-compat client + SFTP fallback)

## Status

**Accepted — v7.0.0 final** (2026-05-06). **Wire-additive on the MCP surface**: no v6.x tool changes shape; v7.0 adds `ssh_rsync` / `ssh_rsync_cancel` / `ssh_rsync_stats` and the `rsync://<id>/progress` push scheme on top.

Both transports are live for the supported feature set:

- **`SftpRsyncTransport`** — universal SFTP fallback (recursive mirror, dry-run, `--delete`, exclude/include, attribute preservation gated on a per-session `SftpFeatures` capability probe). Verified against a real Linux VM via `tests/v7_rsync_e2e_vm.rs`.
- **`WireRsyncTransport`** — canonical port of OpenBSD `openrsync` (BSD/ISC) speaking rsync wire protocol v32 (negotiates down to v31 against rsync 3.2.x servers) against a remote `rsync --server`. Push + pull both byte-identical against `rsync 3.2.7`. Slices 1–10 merged; slices 11–12 (`-c` checksum delta over the wire-format extension and `-H` hardlinks) deferred to v7.1+ — see [Final slice status](#final-slice-status).

The original v7.0 plan (deployed-agent transport with cross-compiled binaries and SFTP self-deploy) was retracted in v7.0.0-alpha.2 in favour of "tudo integrado" — both transports live in-process inside the host crate. See [Architectural retrenchment](#v700-alpha2--architectural-retrenchment-appendix) for the deletions.

Depends on ADR 0003 (Lifecycle Binding), ADR 0004 (Channel Mux), ADR 0005 (LLM UX), ADR 0006 (Backpressure), ADR 0007 (Error Taxonomy), and ADR 0010 (SFTP Resume). Co-exists with — does not replace — `ssh_upload` / `ssh_download`.

## Context

ssh-mcp v6.0 ships full SFTP transfer support but no delta-sync, no recursive directory mirror, no attribute preservation, no exclude/include patterns, no `--delete`, no hardlink/symlink handling, no sparse-file awareness, and no bandwidth shaping. ADR 0010 adds length-prefix resume (start-from-byte-N) but explicitly does not aim at "rsync feature parity" — it targets the single failure mode "transfer interrupted at byte N, resume from byte N" and stops there.

The v6.0 deployment feedback distilled four asks that ADR 0010 cannot satisfy:

1. **Recursive sync with `--delete`** — mirror a local tree onto remote, removing extraneous files. Today: scripted `find` + per-file `ssh_upload` — 10× slower than `rsync` and racy.
2. **Delta sync of large append-only files** (logs, RDB dumps, container layer tarballs). Today: full re-upload every time.
3. **Attribute-preserving copy** (perms, mtime, owner, group, hardlinks, symlinks, sparse). Today: lost.
4. **Exclude/include patterns** (`.git/`, `node_modules/`, `*.tmp`). Today: caller pre-filters file list.

A re-audit of the Rust ecosystem (cargo search 2026-05) found:

| Crate | Role | Status | Useful here |
|---|---|---|---|
| `fast_rsync` 0.2.0 | Pure-Rust librsync (Dropbox); MD4 + MD5 rolling + strong checksums | Production, stable | **Yes — math kernel** |
| `librsync` / `librsync-sys` | C-FFI bindings | Production | No — C dep, redundant with fast_rsync |
| `copia` 0.1.3 | Pure-Rust delta lib | Beta, MSRV 1.75 | Backup option |
| `rsyn` 0.0.1 | Wire-compat rsync client by Martin Pool (rsync co-author) | Pre-alpha, abandoned | Reference only |
| `arrsync` 0.2.1 | Async rsync wire-compat client; **retrieve path only** | Functional, narrow scope | Reference + possible vendor |
| `rjrssync` 0.2.7 | Self-deploying rsync-like tool (embeds cross-arch binaries, SFTP-uploads on first run) | Production | **Yes — pattern reference** |
| `sparsync` / `lms` / `rsy` | rsync-like, custom protocols | Beta/hobby | No (different transport assumptions) |

Key audit finding: the previous design pass dismissed librsync because it requires bilateral execution. The dismissal was correct in isolation but missed two ecosystem signals — `rjrssync` proved the **self-deploying agent pattern** works in production, and `arrsync` proved the **wire-compat client in Rust** is realisable for at least the read path. The two patterns together unlock "rsync completo" without requiring ssh-mcp to choose between deployments that have rsync installed and those that do not.

User decision (recorded for traceability):

- **Wire-compat with rsync.org**: yes — must talk to vanilla `rsync --server` over an existing russh channel when present.
- **Cross-arch fallback agent targets**: `linux-x86_64`, `linux-aarch64`, `macos-aarch64`. Other architectures get an explicit "bring your own agent" path via `RsyncOpts::agent_path`.
- **ADR 0010**: keep as v6.1.0 deliverable; this ADR builds on top, does not replace it (`--partial` semantics inherit from ADR 0010's resume primitive).
- **Math kernel**: `fast_rsync` 0.2.0 (Dropbox), accepting the 2022 release date — the algorithm is stable, the crate has not regressed, and the API is minimal enough that a vendored fork is trivially feasible if upstream activity becomes a concern.

## Decision

Ship a **two-tier transport** behind a single new MCP tool, `ssh_rsync`:

1. **Tier 1 — Wire-compat client.** When the remote host has `rsync >= 3.2.0` (protocol v31), open a russh channel, exec `rsync --server <derived-args>`, and drive protocol v31 from a Rust client we own. Math via `fast_rsync`. Wire-interoperable with stock rsync.
2. **Tier 2 — Self-deploying agent fallback.** When rsync is absent (or below v31 / non-protocol-v31-compatible), detect the remote architecture, SFTP-upload an embedded `ssh-mcp-rsync-agent` binary keyed by sha256, exec it, and drive a custom length-prefixed protocol over the same russh channel pattern. Math via `fast_rsync`. **Not** wire-compatible with rsync.org; covers the same feature set.

Both tiers reuse the v5/v6 lifecycle binding, channel mux, push pipeline, and error taxonomy unchanged.

### Hexagonal layer map

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart TB
    subgraph IN[inbound]
        Tool["tool_router::ssh_rsync"]
    end
    subgraph APP[application]
        UC["RsyncSyncUseCase"]
        Probe["RsyncProbe<br/>(detects rsync)"]
        Sel["TransportSelector"]
    end
    subgraph PORTS[ports]
        TPort["RsyncTransportPort<br/>(start_session, send_op,<br/>recv_event, close)"]
        DPort["RsyncAgentDeployPort<br/>(detect_arch,<br/>upload_if_missing)"]
    end
    subgraph ADAPT[adapters]
        Wire["adapters/rsync/wire<br/>(rsync v31 client)"]
        Agent["adapters/rsync/agent<br/>(custom proto client)"]
        Deploy["adapters/rsync/deploy<br/>(embed + sftp + chmod)"]
        Math["fast_rsync<br/>(sig/delta/patch)"]
    end
    subgraph DOMAIN[domain]
        Sess["RsyncSession aggregate<br/>id, status, stats,<br/>files in-flight"]
        Lane["rsync://<id>/progress<br/>per-file + aggregate events"]
    end

    Tool --> UC
    UC --> Probe --> Sel
    Sel -->|rsync >= v31| TPort
    Sel -->|rsync absent| DPort
    DPort -.-> Deploy
    TPort -.-> Wire
    TPort -.-> Agent
    Wire --> Math
    Agent --> Math
    UC --> Sess
    Sess --> Lane

    classDef in fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    classDef app fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    classDef port fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    classDef adapter fill:#238636,color:#f0f6fc,stroke:#2ea043
    classDef domain fill:#21262d,color:#8b949e,stroke:#30363d
    class Tool in
    class UC,Probe,Sel app
    class TPort,DPort port
    class Wire,Agent,Deploy,Math adapter
    class Sess,Lane domain
```

### Tool surface

```rust
// infra/mcp/tool_router.rs (new)
#[tool(name = "ssh_rsync")]
async fn ssh_rsync(
    &self,
    Parameters(req): Parameters<SshRsyncReq>,
) -> Result<CallToolResult, McpError> { ... }

pub struct SshRsyncReq {
    pub session_id: String,

    /// Local path or remote path. Direction inferred from which side has
    /// the `host:` prefix — exactly one of `src` / `dst` must be remote.
    pub src: String,        // e.g. "/home/me/build/" or "remote:/var/www/"
    pub dst: String,

    /// rsync feature flags. Default `RsyncOpts::default()` mirrors `rsync -a`.
    #[serde(default)]
    pub opts: RsyncOpts,

    /// Transport selection. Default `Auto` (probe; prefer wire-compat).
    #[serde(default)]
    pub transport: RsyncTransport,

    /// Override the on-disk agent path on the remote (skip embedded deploy).
    #[serde(default)]
    pub agent_path: Option<String>,

    /// ADR 0003 lifecycle: rsync session is auto-released when last
    /// subscriber detaches. Default `false` (manual close via `sub_close`
    /// + `ssh_rsync_cancel`).
    #[serde(default)]
    pub release_when_no_subs: bool,
}

#[derive(Default)]
pub struct RsyncOpts {
    pub recursive: bool,         // -r
    pub archive: bool,           // -a (alias for -rlptgoD)
    pub delete: bool,            // --delete
    pub exclude: Vec<String>,    // --exclude=PATTERN
    pub include: Vec<String>,    // --include=PATTERN
    pub dry_run: bool,           // -n
    pub bwlimit_kbps: Option<u64>, // --bwlimit=KBPS
    pub compress: bool,          // -z (only meaningful for wire-compat)
    pub partial: bool,           // --partial (inherits ADR 0010 resume)
    pub verify_checksum: bool,   // -c (force checksum even if size+mtime match)
    pub preserve: PreserveFlags,
}

pub struct PreserveFlags {
    pub perms: bool,      // -p
    pub mtime: bool,      // -t
    pub owner: bool,      // -o (root only on remote)
    pub group: bool,      // -g
    pub links: bool,      // -l (symlinks)
    pub hardlinks: bool,  // -H
    pub sparse: bool,     // -S
    pub devices: bool,    // -D (root only)
}

#[derive(Default)]
pub enum RsyncTransport {
    #[default]
    Auto,        // probe; prefer Wire if rsync >= v31 present, else Agent
    Wire,        // force wire-compat; error if rsync missing
    Agent,       // skip probe; deploy agent
}
```

Companion tool `ssh_rsync_cancel(rsync_id)` — same shape as `ssh_exec_cancel`. Companion read-only `ssh_rsync_stats(rsync_id)` returns the live aggregate (mirrors `ssh_transfer_progress`).

### Transport selection (probe phase)

```text
probe(session) -> ProbeResult:
    out = ssh_exec("which rsync 2>/dev/null && rsync --version 2>/dev/null | head -1 || echo MISSING")
    parse out:
        "MISSING"                           -> RsyncMissing
        "rsync  version <ver>  protocol version <pver>" if pver >= 31
                                            -> RsyncV31 { rsync_version: ver }
        otherwise                           -> RsyncTooOld { protocol: pver }

select(req.transport, probe) -> Result<Transport, DomainError>:
    match (req.transport, probe):
        (Wire,  RsyncV31)         => Ok(Wire)
        (Wire,  RsyncMissing)     => Err(RSYNC_NOT_FOUND)
        (Wire,  RsyncTooOld)      => Err(RSYNC_VERSION_TOO_OLD)
        (Agent, _)                => Ok(Agent)            // skip probe
        (Auto,  RsyncV31)         => Ok(Wire)             // prefer wire
        (Auto,  RsyncMissing)     => deploy_or_err()
        (Auto,  RsyncTooOld)      => deploy_or_err()      // agent reaches feature parity

deploy_or_err() -> Result<Transport, DomainError>:
    arch = ssh_exec("uname -sm")        // "Linux x86_64", "Linux aarch64", "Darwin arm64", ...
    target = arch_to_target(arch)
    match target:
        Linux x86_64                      => Ok(Agent)        // embedded
        Linux aarch64                     => Ok(Agent)
        Darwin aarch64                    => Ok(Agent)
        other if req.agent_path.is_some() => Ok(Agent)        // bring-your-own
        other                             => Err(AGENT_ARCH_UNSUPPORTED)
```

The probe is cached in the v5 `SessionLifecycle` aggregate for the session lifetime — repeated `ssh_rsync` calls against the same session pay one `ssh_exec` per session.

### Tier 1 — Wire-compat client (rsync v31)

**Scope:** rsync protocol version **31 only** (rsync 3.2.0 and newer; the version shipped on every distro since 2020). Older versions are detected and routed to the agent fallback. This is a hard line — implementing v27/v28/v29/v30 backwards-compat is what made `rsyn` plateau at 0.0.1; we will not repeat that mistake.

**Algorithm (sender side, push direction):**

```text
1. open russh channel, exec "rsync --server [-{a,r,l,p,t,g,o,D,H,S,z}]+ . <dst>"
2. handshake:
   - send 4 bytes magic "@RSYNCD" length-prefixed protocol marker
   - exchange supported protocol version (write 31, read remote response)
   - exchange checksum seed (random u32)
3. file-list exchange:
   - walk local tree (respecting --exclude/--include via gitignore-style matcher)
   - for each entry: emit FileEntry { name, size, mtime, mode, uid, gid, link_target?, hardlink_idx? }
   - terminate list with sentinel
4. per-file delta phase:
   - receive block-checksum vector from remote (rolling Adler32 + strong MD5)
   - run fast_rsync::diff to produce token stream (literal | block_match)
   - send token stream
   - receive ACK or NACK with retry attempts
5. cleanup:
   - if --delete, send delete-list to remote
   - exchange final stats (files, bytes, delta savings)
   - close channel cleanly
```

Receiver side (pull direction) is the dual: receive file-list, send block-checksums, receive token stream, apply via `fast_rsync::apply`.

**Implementation strategy:** vendor-evaluate `arrsync`'s receive path as a **reference**, but write our own implementation in `crates/ssh-mcp-rsync-wire/` to stay aligned with our error taxonomy, structured logging (`tracing`), and lock-free invariants. Do not depend on `arrsync` directly — it has a narrow scope (read-only mirror) and the licence (MIT) allows reuse but the API mismatch makes wholesale dependency cost-ineffective.

**Out of scope for v7.0:**

- xattrs (`-X`) — separate sub-protocol, defer to v7.1.
- ACLs (`-A`) — same.
- Daemon-mode / `rsync://` URL scheme — out of scope (we always tunnel through SSH).
- Compression negotiation (`-z`) is accepted in `RsyncOpts` but the wire path forwards it as a raw flag to `rsync --server`; we do not implement zstd / lz4 / xxhash mux ourselves.

### Tier 2 — Self-deploying agent (fallback)

**Sub-crate:** `crates/ssh-mcp-rsync-agent/` (new workspace member). Standalone binary, statically linked (musl on linux, dynamic on macos because Apple does not ship static libSystem). MSRV 1.85 (matches workspace).

**Embed mechanism:** cargo features mirroring the `rjrssync` pattern.

```toml
# Cargo.toml (workspace root)
[features]
default = []
embed-linux-x86_64  = []
embed-linux-aarch64 = []
embed-macos-aarch64 = []
embed-all           = ["embed-linux-x86_64", "embed-linux-aarch64", "embed-macos-aarch64"]

# build.rs (server crate) wires include_bytes! against pre-compiled artefacts
# in target/agents/<triple>/ssh-mcp-rsync-agent
```

CI cross-compiles the agent for each enabled target via `cargo zigbuild` (linux musl) and `cargo build --target aarch64-apple-darwin` (macos, signed). Released artefacts are checked in to a vendored `agents/` directory + sha256 manifest, so a `cargo install ssh-mcp` from crates.io ships the binaries without requiring the consumer to cross-compile.

**Deploy lifecycle:**

```text
1. Detect remote arch via ssh_exec("uname -sm").
2. Compute deployed_path = ~/.cache/ssh-mcp/agent-v<sha256-prefix>-<triple>
3. SFTP stat(deployed_path):
     - present + matching sha256 (read first 4096 bytes, hash vs expected) -> reuse
     - absent -> upload from include_bytes!() blob, chmod 0o755
     - mismatch -> rotate (delete + re-upload)
4. ssh_exec(deployed_path + " --serve") opens the long-running channel
5. Drive custom protocol over the same russh channel
6. On rsync_session close: do NOT delete the agent (keep cached for next call)
7. Cleanup:
   - ssh-mcp host-side periodic task (TTL 30 days) issues
     "find ~/.cache/ssh-mcp/ -mtime +30 -type f -delete" via ssh_exec
     once per session per startup. Idempotent + read-side safe.
```

**Trust model:**

- Embedded sha256 is the source of truth. Agent path includes the sha256 prefix, so two server versions never share an agent file.
- Server verifies the deployed agent's sha256 before exec — defends against partial-upload corruption and a tampered agent on a shared cache directory.
- The agent does NOT verify the server. Trust boundary stops at the SSH session: if the adversary owns the SSH channel, every existing tool (`ssh_exec`, `ssh_upload`) is already compromised.
- A hostile remote with write access to `~/.cache/ssh-mcp/` could replace the binary between sha256 check and exec (TOCTOU). Mitigation: fetch sha256, exec immediately on the same russh channel via `bash -c "[ \"\$(sha256sum ${path} | cut -d' ' -f1)\" = \"${expected}\" ] && exec ${path} --serve"` — single command, no race. Documented as a defence-in-depth measure, not a guarantee.

**Wire format (custom binary protocol):**

```text
Frame: <length: u32 BE> <op_code: u8> <payload: bincode>

Op codes:
  0x01 Hello          { protocol_version: u32, agent_version: String, sha256: [u8; 32] }
  0x02 ListRequest    { root: PathBuf, exclude: Vec<String>, include: Vec<String>, recursive: bool }
  0x03 ListEntry      { rel_path: PathBuf, kind: FileKind, size: u64, mtime: i64, mode: u32, uid: u32, gid: u32, link_target: Option<PathBuf>, hardlink_idx: Option<u32> }
  0x04 ListEnd        { total: u64 }
  0x05 SignatureReq   { rel_path: PathBuf, block_size: u32 }
  0x06 SignatureRsp   { rel_path: PathBuf, sig: Bytes }      // fast_rsync::Signature wire format
  0x07 DeltaReq       { rel_path: PathBuf, sig: Bytes }
  0x08 DeltaChunk     { rel_path: PathBuf, chunk_idx: u32, data: Bytes }
  0x09 DeltaEnd       { rel_path: PathBuf, total_chunks: u32 }
  0x0A Mkdir          { rel_path: PathBuf, mode: u32 }
  0x0B Symlink        { rel_path: PathBuf, target: PathBuf }
  0x0C Hardlink       { rel_path: PathBuf, source_idx: u32 }
  0x0D Chmod          { rel_path: PathBuf, mode: u32 }
  0x0E Chown          { rel_path: PathBuf, uid: u32, gid: u32 }
  0x0F Chmtime        { rel_path: PathBuf, mtime: i64 }
  0x10 Unlink         { rel_path: PathBuf }
  0x11 Progress       { rel_path: PathBuf, bytes_done: u64, bytes_total: u64 }
  0x12 FileDone       { rel_path: PathBuf, bytes_transferred: u64, bytes_skipped: u64 }
  0x13 Error          { code: AgentErrorCode, detail: String, rel_path: Option<PathBuf> }
  0x14 SyncDone       { stats: SyncStats }
  0x15 Cancel         { reason: String }                     // server -> agent

Length-prefixed bincode keeps decoding zero-copy where possible. Op_codes
0x06, 0x08, 0x12 carry the bulk of the bytes; everything else is metadata.
```

The agent is a thin shell over `fast_rsync` + `walkdir` + `nix`/`std::fs::Permissions`. Approximate LoC: 2500 production + 1000 tests.

### Push lane — `rsync://<rsync_id>/progress`

A new push-resource scheme registered in `MemoryRegistry` exactly like `transfer://<id>/progress`. Event types:

```rust
enum RsyncProgressEvent {
    SessionStarted { transport: Wire | Agent, files_planned: u64, bytes_planned: u64 },
    FileStarted    { rel_path: String, bytes_total: u64 },
    FileProgress   { rel_path: String, bytes_done: u64, bytes_total: u64 },
    FileCompleted  { rel_path: String, bytes_transferred: u64, bytes_skipped: u64 },
    FileSkipped    { rel_path: String, reason: SkipReason },     // SizeMatch | MtimeMatch | DryRun
    FileFailed     { rel_path: String, code: ErrorCode, detail: String },
    SyncProgress   { files_done: u64, files_total: u64, bytes_done: u64, bytes_total: u64 },
    SyncCompleted  { stats: RsyncStats },
    SessionFailed  { code: ErrorCode, detail: String },
}
```

Backpressure policy default: `Snapshot` (matches v5 defaults). Lane buffer default `SSH_LANE_BUFFER` (current 256). High-fanout sync of millions of small files -> caller switches to `DropOldest` per ADR 0006.

ADR 0006 Amendment 1 byte-threshold flush applies: per-event JSON is small (< 1 KiB) but `SyncProgress` ticks fire fast on multi-million-file trees; the default 64 KiB threshold flushes the channel ~every 60 events without waiting for the 50 ms debounce.

### Configuration surface

Five new env vars; all default-on with v6-compatible behaviour for callers who never set them.

| Env var | Default | Meaning |
|---|---|---|
| `SSH_RSYNC_PROBE_TIMEOUT_MS` | `2000` | Max time to wait for the `rsync --version` probe. |
| `SSH_RSYNC_AGENT_CACHE_TTL_DAYS` | `30` | Agent file TTL on `~/.cache/ssh-mcp/` (cleanup task). |
| `SSH_RSYNC_AGENT_CACHE_DIR` | `~/.cache/ssh-mcp` | Override deployment directory. Useful for `noexec` `/home`. |
| `SSH_RSYNC_BLOCK_SIZE` | `0` (auto: `sqrt(file_size)` rounded to 1 KiB) | Override fast_rsync block size. |
| `SSH_RSYNC_FILE_LIST_LIMIT` | `1_000_000` | Max file list size; refuse sync above this with `RSYNC_FILE_LIST_TOO_LARGE`. |

### Error taxonomy delta (extends ADR 0007 + ADR 0010)

Nine new codes; total taxonomy 40 → **49**.

| Code | Category | Retry | Detail (one-sentence; ADR 0007 contract) |
|---|---|---|---|
| `RSYNC_NOT_FOUND` | RESOURCE | no | "rsync binary missing on remote; install rsync >= 3.2.0 or set transport=agent." |
| `RSYNC_VERSION_TOO_OLD` | POLICY | conditional | "remote rsync protocol < v31; upgrade to rsync >= 3.2.0 or set transport=agent." |
| `RSYNC_PROTOCOL_ERROR` | TRANSPORT | yes | "wire-compat negotiation failed; check rsync server stderr or fall back to transport=agent." |
| `RSYNC_FILE_LIST_TOO_LARGE` | POLICY | conditional | "file list exceeds SSH_RSYNC_FILE_LIST_LIMIT; tighten --exclude or raise the limit." |
| `RSYNC_PARTIAL_TRANSFER` | TRANSPORT | yes | "transfer interrupted; re-run with --partial to resume." |
| `AGENT_DEPLOY_FAILED` | TRANSPORT | yes | "agent SFTP upload or chmod failed; check writable cache dir or set SSH_RSYNC_AGENT_CACHE_DIR." |
| `AGENT_ARCH_UNSUPPORTED` | POLICY | no | "remote arch not in {linux-x86_64, linux-aarch64, macos-aarch64}; provide agent_path explicitly." |
| `AGENT_TRUST_VIOLATION` | INTERNAL | no | "deployed agent sha256 mismatch; collect logs and report." |
| `AGENT_NOEXEC_TARGET` | POLICY | conditional | "cache dir is on a noexec mount; set SSH_RSYNC_AGENT_CACHE_DIR to an exec-capable path." |

## Consequences

### Wire compatibility

- **v6.x hosts unchanged.** No existing tool gains a parameter, no existing response gains a field. The only surface change is the addition of `ssh_rsync` + `ssh_rsync_cancel` + `ssh_rsync_stats` + the `rsync://` resource scheme.
- **MCP protocol shape stable.** `tools/list` gains 3 entries; `resources/list` gains the dynamic `rsync://<id>/progress` URIs as sessions open. Same envelope, same nonce, same structured-content convention.
- **Cursor / SubId / lifecycle.** `rsync://<id>/progress` is an ordinary push resource — subscribes mint a SubId per ADR 0004, attach via `sub_open`, drain via `notifications/resources/updated`. ADR 0003 lifecycle binding applies: `release_when_no_subs = true` on `ssh_rsync` releases the session when the last subscriber detaches.
- **Daemon NDJSON.** `ssh-mcp-tail` parser gains the new request shape via `serde`'s additive deserialisation; `notifications/resources/updated` events for `rsync://*` URIs surface as ordinary push events keyed on the `rsync_id`.

### Edge cases (documented + tested)

1. **Mid-sync session loss.** Russh channel drops mid-protocol. Wire-compat: `rsync --server` exits non-zero; we surface `RSYNC_PARTIAL_TRANSFER`. Caller re-runs with `--partial` (ADR 0010 resume). Agent: same — agent exits, partial files left intact, re-run resumes.
2. **rsync 3.1 / protocol v30 hosts.** Probe detects, routes to agent fallback. Documented.
3. **`noexec` `/home`.** Agent deploy fails; surface `AGENT_NOEXEC_TARGET`. Caller sets `SSH_RSYNC_AGENT_CACHE_DIR=/var/tmp/ssh-mcp` (or similar exec-capable path). Documented.
4. **SELinux / AppArmor.** Agent exec blocked by MAC policy. Detected as exit-code 126/127; surfaced as `AGENT_DEPLOY_FAILED`. Documented mitigation: deploy agent under `/var/tmp` with appropriate context; or pre-place the agent and pass `agent_path`.
5. **Antivirus quarantine.** Defender / ESET / equivalent quarantines the freshly-uploaded agent binary. Detected as `AGENT_DEPLOY_FAILED` (exec returns ENOENT post-quarantine). Documented; no clean mitigation other than allowlisting.
6. **TOCTOU on agent sha256 verify.** Mitigated by single-shot `bash -c "[hash check] && exec ..."` (see Trust model). Documented as defence-in-depth.
7. **Hardlink graph across sync root.** rsync v31 handles via `-H`; agent emits `Hardlink { source_idx }` referring to a `ListEntry` index. Cross-root hardlinks (link target outside the sync root) are out of scope — documented.
8. **Symlink target outside sync root.** `-l` preserves the symlink as-is; we do NOT follow. Same behaviour as rsync.
9. **Dry-run with `--delete`.** Both transports emit `FileSkipped { reason: DryRun }` for would-delete entries; no destructive op leaves the host.
10. **Bandwidth limit.** Wire-compat passes `--bwlimit=KBPS` to remote `rsync --server`. Agent implements via token-bucket on the writer side of the russh channel (60 ms tick granularity). Tested.
11. **Massive file list (millions of entries).** Memory budget: the file list is streamed, not buffered, on both ends. The `SSH_RSYNC_FILE_LIST_LIMIT` guard prevents pathological cases from OOM-ing the server. Documented.

### Test surface

- **Unit tests (each adapter):** ~80 cases covering frame parsing, op-code dispatch, sha256 verify, arch detection, exclude/include matcher, dry-run.
- **Use-case tests:** drive `RsyncTransportPort` fakes; assert `RsyncSession` lifecycle, push events, error propagation. ~40 cases.
- **Loopback integration:** `tests/v7_rsync_smoke.rs`, new. Spin up an OpenSSH test server (existing `tests/ssh_test_server.rs` infrastructure), invoke `ssh_rsync` against it for both transport modes. Cover: fresh push, fresh pull, delta push (preexisting destination tree, 1 % changed bytes), `--delete` path, `--exclude` semantics, attrs preservation, hardlink graph, symlink, sparse, dry-run, bandwidth limit, cancel mid-flight. ~30 scenarios.
- **Property tests:** `tests/property_rsync.rs`, new. proptest over `(file_count ∈ [0, 1000], byte_size_per_file ∈ [0, 1 MiB], change_ratio ∈ [0, 1])`; assert post-condition `dst tree == src tree`. ~10 strategies.
- **Wire-compat fixture tests:** capture rsync-over-SSH wire bytes from a real `rsync 3.2.7` ↔ `rsync 3.2.7` session; replay through our wire-compat client; assert byte-for-byte parity at the protocol layer. ~5 fixtures.
- **Agent fixture tests:** matrix of arch detection strings (`uname -sm` outputs from 8 distros).

Total new test surface: ~165 unit + integration cases + 10 property strategies + 5 wire fixtures = **~180 net-new test cases**.

### Lock-free invariants (preserved)

- `RsyncSession` aggregate is `AtomicU8 status` + `AtomicU64 bytes_transferred` + `AtomicU64 bytes_skipped` + `AtomicU64 files_done` + `AtomicU64 files_total` + `ArcSwap<RsyncStats>` for the read-side snapshot. Zero `Mutex`.
- Agent deploy state (`deployed_paths: DashMap<(SessionId, AgentSha256), DeployStatus>`) is `DashMap`; deploy is single-shot, idempotent, never holds a lock across an `.await`.
- `RsyncTransportPort` impls expose `tokio::sync::mpsc::channel(N)` for outbound op stream; reuse the `MultiplexLane` pattern from ADR 0004 for inbound progress events.
- New loom invariant tests in `tests/lockfree_invariants.rs`: `rsync_session_status_cas_race`, `agent_deploy_concurrent_idempotent`, `progress_lane_round_robin`. Total 20 → **23** loom tests.

## Implementation phases

| Phase | Scope | Estimate | Deliverable |
|---|---|---|---|
| 1 | Workspace split — promote ssh-mcp to multi-crate workspace; add `crates/ssh-mcp-rsync-agent` (skeleton, "hello world" handshake). | 1 wk | Workspace builds; agent binary exists for 1 target. |
| 2 | Shared protocol types (`crates/ssh-mcp-rsync-proto`): `RsyncOp`, `RsyncProgressEvent`, frame codec, bincode wiring. Used by both transports. | 1 wk | Protocol crate with 100 % unit-test coverage of frame encode/decode. |
| 3 | Domain — `RsyncSession` aggregate, `RsyncTransport` enum, `RsyncStats`, error code additions in `domain/error.rs`. | 1 wk | Compiles; existing tests green. |
| 4 | Ports — `RsyncTransportPort`, `RsyncAgentDeployPort`, fake adapters under `test-fixtures`. | 1 wk | Use case can be driven against fakes end-to-end. |
| 5 | Agent fallback — implement custom protocol in agent binary; deploy adapter (sha256 + SFTP + chmod). Single transport, single arch (linux-x86_64). | 4 wk | `transport=agent` works against linux-x86_64; integration test green. |
| 6 | Agent feature parity — recursive walk, attrs, hardlinks, symlinks, sparse, exclude/include matcher, --delete, dry-run, bwlimit. | 3 wk | Property tests pass for full feature matrix on agent. |
| 7 | Cross-arch agent matrix — linux-aarch64, macos-aarch64. CI cross-compile pipeline (zigbuild + macos signing). Embed manifest. | 2 wk | All three target arches deploy + execute end-to-end on real hardware. |
| 8 | Wire-compat client v31 — handshake, file list, block-checksums, delta tokens via `fast_rsync`. Push direction first; pull direction second. | 6 wk | `transport=wire` works against rsync 3.2.7 reference; wire fixtures green. |
| 9 | Wire-compat feature parity — attrs, hardlinks, sparse, exclude/include, --delete, dry-run, bwlimit. Catch up to agent feature set. | 3 wk | Property tests pass for both transports identically. |
| 10 | Push lane — `rsync://<id>/progress` resource scheme; per-file + aggregate event emission; lane integration with channel-mux + lifecycle binding; ADR 0006 Amendment 1 byte-threshold. | 2 wk | Push events surface to subscribers; loom invariants hold. |
| 11 | Tool router — `ssh_rsync`, `ssh_rsync_cancel`, `ssh_rsync_stats`. NDJSON daemon parser update. Block-markdown response, structured payload, HINT/NEXT lines. | 1 wk | MCP surface complete. |
| 12 | Docs — ADR 0011 (this); `docs/API.md` (new section); `docs/LLM_GUIDE.md` ("Recursive sync with rsync"); `docs/RESOURCES.md` (rsync://); `docs/CONFIGURATION.md` (5 new env vars); `docs/MIGRATION.md` (v6 → v7); `CLAUDE.md` summary. | 1 wk | Docs reflect surface. |
| 13 | Loopback + property + wire-fixture tests + loom invariant additions. | 3 wk | ~180 net-new tests green; coverage >= 85 %. |
| 14 | Beta release — RC1 on a feature flag (`feature = "rsync"` default-off); collect deployment feedback; harden. | 2 wk | RC1 → RC2 with bug fixes from real hosts. |
| 15 | GA — flip default-on, ship v7.0.0. | 1 wk | crates.io release. |

**Total: 32 weeks** of focused single-developer engineering. Parallelism reduces wall-clock: phases 5+8 are independent (different sub-crates); phases 6+9 fork into agent vs wire feature work; phase 10 fans out from 4+8 simultaneously. With one engineer on agent, one on wire, one on tooling/docs, the wall-clock collapses to ~16-18 weeks.

**Sequencing note.** Phases 1–4 are load-bearing for everything else and ship first. Phase 5 (agent on linux-x86_64) is the smallest viable deliverable that unlocks user-facing testing — once it's green, every later phase has a feedback loop. Phase 8 (wire-compat) is the largest single risk; if blockers emerge, fall back to "agent-only v7.0, wire-compat v7.1" — the user-facing surface is identical, only `transport=Wire` returns `RSYNC_VERSION_TOO_OLD` for every host until the wire client lands.

## Alternatives considered

- **Agent-only ("Path A" from audit).** Skip wire-compat entirely. Simpler, ~10 weeks. Rejected because the user explicitly asked for wire-compat; deployments where the operator does not control the remote host (managed services, vendor systems, SaaS shells) cannot accept an agent and need the wire path.
- **Wire-compat-only ("Path B" from audit).** Skip the agent. ~16-24 weeks. Rejected because some hosts (minimal containers, locked-down embedded systems, FreeBSD jails without rsync) genuinely have no rsync, and the cross-arch fallback list confirms the user wants coverage there.
- **Wrap rsync binary as transparent SSH transport ("Path C").** Spawn local `rsync` and pipe through russh. Rejected: loses session reuse (rsync forks its own SSH), push integration shrinks to stderr parsing, and `rsync --info=progress2` output is inconsistent across versions.
- **librsync C bindings instead of `fast_rsync`.** Adds C dep, redundant capability. `fast_rsync` is API-equivalent and stable. Rejected on supply-chain grounds.
- **`copia` instead of `fast_rsync`.** Newer pure-Rust delta library, MSRV 1.75. Rejected for v7.0 because Dropbox has shipped `fast_rsync` at scale; the older codebase has the production track record. Re-evaluate for v7.1+ if `copia` matures and `fast_rsync` activity stays at zero.
- **Vendor `arrsync` directly.** MIT-licensed; covers the receive path. Rejected as a hard dependency because the API is narrow (mirror-only) and the maintenance status is single-author. Treated as a reference implementation only.
- **Daemon-mode `rsync://` URL support.** Out of scope for v7.0; would require a non-SSH transport (TCP), which contradicts the "everything tunnels through an existing SSH session" contract. Punted.
- **xattr / ACL preservation.** Out of scope for v7.0. Both transports support the underlying capability; the wire format and op-code space leave room (op codes 0x16+ unused). Defer to v7.1 with a separate ADR.
- **Single ADR for resume + rsync.** Considered merging ADR 0010 into this ADR as a sub-section. Rejected — ADR 0010 ships in v6.1 (1-2 weeks); ADR 0011 ships in v7.0 (16-32 weeks). Coupling them delays the small win for no architectural benefit.

## References

- ADR 0003 — Lifecycle binding (`release_when_no_subs` semantics inherited).
- ADR 0004 — Channel mux + sub_id (push lane registration pattern).
- ADR 0005 — LLM UX priorities (HINT/NEXT lines on the new tools).
- ADR 0006 — Backpressure policies (default `Snapshot`; byte-threshold flush).
- ADR 0007 — Error taxonomy (extended by 9 codes).
- ADR 0010 — SFTP resume (`--partial` semantics inherit).
- `fast_rsync` 0.2.0 — https://crates.io/crates/fast_rsync (Dropbox; pure-Rust librsync).
- `rjrssync` 0.2.7 — https://github.com/Robert-Hughes/rjrssync (self-deploying agent pattern reference; superseded by the v7.0.0-alpha.2 retrenchment).
- `arrsync` 0.2.1 — https://crates.io/crates/arrsync (rsync wire-compat retrieve-path reference).
- rsync protocol v31 — `tech_report.tex`, `csprotocol.txt` in https://github.com/RsyncProject/rsync.
- openrsync — https://www.openrsync.org/ (BSD-licensed C reference impl).

### Final architecture

Both transports collapse into the same use-case + push-lane shape; only the bytes-on-the-wire layer differs.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart TB
    subgraph IN[Inbound MCP]
        Tool["tool_router::ssh_rsync*"]
    end
    subgraph APP[Application]
        UC["RsyncSyncUseCase<br/>(probe + select + start)"]
    end
    subgraph PORTS[Ports]
        TPort["RsyncTransportPort"]
        SshPort["SshClientPort<br/>(probe via execute)"]
        SftpFs["RsyncSftpFsPort"]
        Repo["RsyncRepository"]
    end
    subgraph ADAPT[Adapters]
        Wire["WireRsyncTransport<br/>(openrsync port)"]
        Sftp["SftpRsyncTransport<br/>(walker + comparator + executor)"]
        Russh["RusshRsyncSftpFs"]
        DashRepo["DashMapRsyncRepo"]
    end
    subgraph LANE[Push lane]
        Mux["ChannelMux<br/>(rsync://&lt;id&gt;/progress)"]
        Out["rmcp Peer / NDJSON writer"]
    end

    Tool --> UC
    UC --> SshPort
    UC --> TPort
    UC --> Repo
    TPort -.-> Wire
    TPort -.-> Sftp
    Sftp --> SftpFs
    SftpFs -.-> Russh
    Repo -.-> DashRepo
    Wire --> Mux
    Sftp --> Mux
    Mux --> Out

    classDef in fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    classDef app fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    classDef port fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    classDef adapter fill:#238636,color:#f0f6fc,stroke:#2ea043
    classDef lane fill:#21262d,color:#8b949e,stroke:#30363d
    class Tool in
    class UC app
    class TPort,SshPort,SftpFs,Repo port
    class Wire,Sftp,Russh,DashRepo adapter
    class Mux,Out lane
```

**Push pipeline (Wire transport).**

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
sequenceDiagram
    participant H as Host (RsyncSyncUseCase)
    participant W as WireRsyncTransport
    participant CH as russh exec channel
    participant S as remote rsync --server

    H->>W: start_session(Push)
    W->>CH: open exec "rsync --server -e.LsfxCIu -r . <dst>"
    W->>CH: handshake — write lver=31
    CH->>W: rver=31 + compat_flags varint + seed
    W->>W: gen_flist_local(src) — BFS + sort
    W->>CH: emit SessionStarted on lane
    Note over W,CH: enable mplex_writes at proto >= 30
    W->>CH: send flist (16-bit XMIT flags + varint30/varlong30)
    loop per file (until idx == NDX_DONE)
        S->>W: ndx + iflags + blockset signature
        W->>CH: ndx + zero-blockset header
        W->>CH: token stream (literal / block_match)
        W->>CH: 16-byte MD5 trailer (proto >= 30)
        W-->>H: FileCompleted lane event
    end
    Note over W,CH: phase loop — echo NDX_DONE per phase, final NDX_DONE per sender.c:464
    W->>CH: read_final_goodbye (NDX_DONE in / NDX_DONE out / NDX_DONE in at proto >= 31)
    W-->>H: SyncCompleted lane event
```

**Pull pipeline (Wire transport).** Same channel, dual roles — the client now plays receiver and the server plays sender.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
sequenceDiagram
    participant H as Host (RsyncSyncUseCase)
    participant W as WireRsyncTransport
    participant CH as russh exec channel
    participant S as remote rsync --server --sender

    H->>W: start_session(Pull)
    W->>CH: open exec "rsync --server --sender -e.LsfxCIu -r <src> ."
    W->>CH: handshake — write lver=31
    CH->>W: rver=31 + compat_flags varint + seed
    Note over W,CH: enable mplex_writes at proto >= 30
    W->>CH: empty filter rule list (int32 0 terminator)
    S->>W: flist (16-bit XMIT + varint30/varlong30 + post-sort indices + IO_ERROR_ENDLIST)
    W-->>H: SessionStarted lane event
    loop per regular file
        W->>CH: ndx + null_sum blockset (whole-file pull)
        S->>W: token stream (literal-only at slice 7)
        W->>W: write to deterministic tempfile
        S->>W: 16-byte MD5 trailer
        W->>W: verify trailer + atomic rename into dst tree
        W-->>H: FileCompleted lane event
    end
    W->>CH: NDX_DONE per phase echo
    Note over W,CH: phase loop + final NDX_DONE
    S->>W: goodbye sentinel
    W-->>H: SyncCompleted lane event
```

**SFTP fallback pipeline.** No remote helper, no rsync binary required; the transport drives a recursive mirror over plain SFTP ops gated on a per-session capability probe.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    Probe["SFTP capability probe<br/>(mkdir + setstat + symlink)<br/>cached per-session"]
    Walk["SftpWalker<br/>(BFS + globset filter)"]
    Cmp["compare_trees<br/>→ SyncAction list"]
    Exec["SftpExecutor<br/>(read + write + setstat<br/>+ token-bucket bwlimit)"]
    Lane["rsync://&lt;id&gt;/progress lane"]

    Probe --> Walk
    Walk --> Cmp
    Cmp --> Exec
    Exec --> Lane

    style Probe fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    style Walk fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Cmp fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Exec fill:#238636,color:#f0f6fc,stroke:#2ea043
    style Lane fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
```

The SFTP probe surfaces `SFTP_FEATURE_MISSING` for `preserve.symlinks=true` (server refused `SSH_FXP_SYMLINK`), `preserve.perms=true` (server refused `SSH_FXP_SETSTAT`), `preserve.hardlinks=true` (Wire-only), or `verify_checksum=true` (Wire-only) — all gated **before** the recursive walk burns RTT, so the lane never observes a half-failed run.

---

## v7.0.0-alpha.2 — Architectural retrenchment (appendix)

After the original v7.0 plan landed phases 5+6+10 (linux-x86_64 agent
binary, full feature parity, push lane), the user's deployment feedback
distilled into a single preference: **tudo integrado** — keep both
transports inside the host crate, no cross-compiled remote binary, no
SFTP-deploy lifecycle, no embed feature matrix. The deployed-agent
path was retracted in favour of two integrated transports:

- **Wire transport (Path B from the original audit)** — drives a
  remote `rsync --server` process over the existing russh exec
  channel. Uses the rsync binary already installed on most unix
  hosts. Wire-interoperable with rsync v31. Implementation deferred
  to the next v7.0.x slice.
- **SFTP fallback transport (Path D from the original audit)** —
  pure SFTP `readdir` + `stat` + `read` + `write` + `setstat`, no
  remote helper. Slower than the Wire path (no rolling-checksum
  delta) but universal. Implementation deferred to the next v7.0.x
  slice.

### What was deleted

- The `crates/ssh-mcp-rsync-agent/` sub-crate (the deployed binary,
  ~5000+ LoC across the protocol main loop, `fast_rsync` math,
  `walkdir` + `globset` matcher, token-bucket bwlimit, frame
  codec, error taxonomy, unit tests).
- The `crates/ssh-mcp-rsync-proto/` sub-crate (shared wire-format
  types + bincode codec). Surviving value-object types
  (`FileKind`, `PreserveFlags`, `RsyncStats`, `RsyncProgressEvent`,
  `RsyncTransportKind`, `SkipReason`, `ErrorCode`) moved to
  `src/adapters/rsync/types.rs` (with `RsyncStats` re-homed in
  `src/domain/rsync.rs` so the domain layer's import-cap rule stays
  intact). Wire-format types (`RsyncOp`, `RsyncOpPayload`, frame
  codec, op-code constants) had a single producer / single consumer
  and were dropped along with the agent.
- `target/agents/{x86_64-unknown-linux-musl,aarch64-unknown-linux-musl,aarch64-apple-darwin}/`
  — the cross-compiled blobs.
- `RsyncAgentDeployPort`, `EmbeddedAgentDeployAdapter`,
  `AgentRsyncTransport`, `SshShellExecAdapter`, the `RemoteShellExec`
  collaborator port — every artefact under the deleted
  `src/adapters/rsync/agent/` sub-tree.
- `transport=Agent` on the inbound `RsyncTransportArg` enum
  (replaced by `transport=Sftp`).
- `agent_path` request override on `SshRsyncArgs` (no agent to
  override).
- Cargo features `embed-linux-x86_64`, `embed-linux-aarch64`,
  `embed-macos-aarch64`, `embed-all`.

### Error taxonomy delta (vs the original v7.0 plan)

The original 9 ADR 0011 codes split into two categories:

| Category | Codes | Status |
|---|---|---|
| Rsync-specific (kept) | `RSYNC_NOT_FOUND`, `RSYNC_VERSION_TOO_OLD`, `RSYNC_PROTOCOL_ERROR`, `RSYNC_FILE_LIST_TOO_LARGE`, `RSYNC_PARTIAL_TRANSFER` | Still useful for Wire + Sftp |
| Agent-specific (dropped) | `AGENT_DEPLOY_FAILED`, `AGENT_ARCH_UNSUPPORTED`, `AGENT_TRUST_VIOLATION`, `AGENT_NOEXEC_TARGET` | Deleted along with the agent path |

One new code added — `SFTP_FEATURE_MISSING` (POLICY, conditional retry)
— surfaced when the request asks the SFTP path for hardlinks
(`opts.preserve.hardlinks=true`) or delta-sync semantics
(`opts.verify_checksum=true`); both are Wire-only features today.

Net taxonomy delta: 9 → 6 codes; total ssh-mcp error-taxonomy
size grows from 40 → 46 (vs the original plan's 49).

### Use-case redesign

`RsyncSyncUseCase` collapsed from a 7-generic-parameter shape
(`<T, D, R, SR, Idg, Cfg, Sh>`) to a 7-parameter shape with one
*dual* transport slot (`<W, Sf, R, SR, Ssh, Idg, Cfg>`):

```rust
pub struct RsyncSyncUseCase<W, Sf, R, SR, Ssh, Idg, Cfg>
where
    W:   RsyncTransportPort,        // wire-compat
    Sf:  RsyncTransportPort,        // sftp fallback
    R:   RsyncRepository,
    SR:  SessionRepository,
    Ssh: SshClientPort,             // probe via .execute()
    Idg: IdGeneratorPort,
    Cfg: ConfigPort,
{ ... }

// Selection algorithm (replaces the original probe + deploy + arch flow):
match (transport_arg, probe_result) {
    (Wire, v >= 31)              => Wire,
    (Wire, _)                    => Err(RsyncVersionTooOld),
    (Sftp, _)                    => Sftp,
    (Auto, v >= 31)              => Wire,
    (Auto, _)                    => Sftp,
}
// Capability gate — Sftp + hardlinks/delta returns SftpFeatureMissing.
```

The probe (`which rsync && rsync --version | head -1`) now runs
through the existing `SshClientPort::execute` path — no separate
`RemoteShellExec` collaborator port. This shrinks the dependency
graph and the use-case constructor.

### v7.0.0-alpha.2 surface contract

This slice freezes the public-API shape: every `ssh_rsync*` call
accepts a request, runs the probe + selection logic, drives the
chosen transport's `start_session`, and surfaces a descriptive
`RSYNC_PROTOCOL_ERROR` with a transport-specific "being
implemented" detail line. The MCP surface, error taxonomy, push-lane
URI shape, and lifecycle binding are the permanent shape; the next
slice fills the transport bodies without churning anything else.

## Final slice status

Both transports closed their respective backlogs in v7.0.0. The Wire transport's openrsync port shipped across ten slices; slices 11–12 (`-c` checksum delta and `-H` hardlinks) require wire-format extensions outside the openrsync canon and are deferred to a successor ADR.

### Per-slice openrsync port rollout

| Slice | Coverage | Status | Files |
|---|---|---|---|
| 1 | Handshake + mplex I/O re-port | merged — handshake reaches `negotiated=31` + valid seed | `wire/session.rs`, `wire/io.rs` |
| 2 | File-list encode/decode + local walker | merged — server's generator accepts our flist bytes | `wire/flist.rs` |
| 3 | Block-checksum decode + Adler32/MD4 hash kernels + whole-file token stream + sender state machine | merged — first byte-identical push | `wire/blocks.rs`, `wire/hash.rs`, `wire/tokens.rs`, `wire/sender.rs` |
| 4 | Protocol 31 lift (`XMIT_EXTENDED_FLAGS` 16-bit, varint30/varlong30, ndx codec, iflags) | merged — flist parses cleanly against rsync 3.2.7 | `wire/flist.rs`, `wire/ndx.rs`, `wire/sender.rs` |
| 5 | Per-file digest = MD5 at proto >= 30 + multi-phase `send_files` loop + final `NDX_DONE` + proto-31 goodbye | merged — 3 files land sha256-equivalent on disk | `wire/hash.rs`, `wire/sender.rs` |
| 6 | Block-match path (Adler32 hashtable + sliding window + MD4-with-seed strong verify + literal/match tokens) | merged — incremental push collapses unchanged blocks | `wire/match_.rs`, `wire/blocks.rs` |
| 7 | Pull direction + receiver state machine + flist decode (`XMIT_MOD_NSEC` + `IO_ERROR_ENDLIST` + post-sort indices) | merged — first byte-identical pull | `wire/receiver.rs`, `wire/flist.rs` |
| 8 | Incremental pull (local block-signature emit + match-token consume on the receiver side) | merged | `wire/receiver.rs` |
| 9 | `--delete` pass + attrs apply (`-p -t -o -g -l`) plumbed end-to-end | merged | `wire/receiver.rs`, `wire/sender.rs`, `wire/flist.rs` |
| 10 | `--partial` (deterministic tempfile + skip-unlink-on-error) + `-S` sparse hole detection | merged | `wire/receiver.rs` |
| 11 | `-c` (checksum delta over a wire-format extension) | **deferred** — requires bytes outside the openrsync canon | — |
| 12 | `-H` (hardlink graph preservation) | **deferred** — same | — |

Deferral rationale: slices 11–12 need wire-format paths that openrsync 27 never grew and that rsync 3.2.x emits behind compat-flag gates we have not yet ported. The work is bounded but does not block the v7.0 GA contract — the SFTP transport's capability gate already surfaces `SFTP_FEATURE_MISSING` for `verify_checksum=true` / `preserve.hardlinks=true`, and the Wire path raises the same error code today. A successor ADR will scope the byte-shape work; until then, callers that need either feature accept the SFTP whole-block path or wait.

### Cumulative layer status

| Layer | Status | File |
|---|---|---|
| v31 handshake (version + compat_flags + seed) | live | `src/adapters/rsync/wire/session.rs` |
| Multiplex I/O framing (`MSG_*` tags + frame codec) | live | `src/adapters/rsync/wire/io.rs` |
| File-list encode / decode (16-bit `XMIT_EXTENDED_FLAGS` + varint30/varlong30 + symlink target + post-sort indices) | live | `src/adapters/rsync/wire/flist.rs` |
| Local-source walker (`gen_flist_local`) | live; `--exclude` / `--include` enforced inline at proto 31 | `src/adapters/rsync/wire/flist.rs` |
| Block-checksum signature exchange + null_sum tolerance | live | `src/adapters/rsync/wire/blocks.rs` |
| Adler32 rolling hash + MD4 (proto < 30) / MD5 (proto >= 30) per-file digest | live | `src/adapters/rsync/wire/hash.rs` |
| Token stream — literal / EOF / block-match arms | live | `src/adapters/rsync/wire/tokens.rs`, `wire/match_.rs` |
| ndx codec (1–6 byte diff-from-prev encoding for file indices) | live | `src/adapters/rsync/wire/ndx.rs` |
| Sender state machine (per-file request loop + multi-phase `send_files` + post-loop sentinel + goodbye) | live | `src/adapters/rsync/wire/sender.rs` |
| Receiver state machine + downloader (token apply + atomic rename + tempfile staging) | live | `src/adapters/rsync/wire/receiver.rs` |
| Attribute apply (`-p` mode, `-t` mtime, `-l` symlink, `-o`/`-g` owner/group decode) | live | `src/adapters/rsync/wire/receiver.rs` |
| `--delete` pass | live | `src/adapters/rsync/wire/receiver.rs`, `wire/sender.rs` |
| `--partial` (deterministic tempfile + skip-unlink-on-error) | live | `src/adapters/rsync/wire/receiver.rs` |
| `-S` sparse hole detection on receive | live | `src/adapters/rsync/wire/receiver.rs` |
| `chown` syscall wiring (root-only) | deferred | — |
| Encoder symmetry for owner/group when `-o`/`-g` negotiated | partial — decode honoured, encode skips bytes the server would otherwise expect | `src/adapters/rsync/wire/flist.rs` |
| `-c` checksum delta over wire-format extension | deferred to v7.1+ | — |
| `-H` hardlinks | deferred to v7.1+ | — |
| `-D` devices, `-X` xattrs, `-A` ACLs | out of scope for v7.x | — |

### End-to-end verification (against `rsync 3.2.7`)

`tests/v7_rsync_wire_e2e_vm.rs` (gated `e2e-vm`) drives the live wire transport against a real Linux VM. Six tests cover the production matrix:

1. **Push pipeline** — 3 files → byte-identical on remote disk.
2. **Incremental sync push** — second pass over a populated tree skips unchanged files; `bytes_skipped` accumulates.
3. **Pull pipeline** — 3 files → byte-identical on local disk.
4. **Incremental pull** — same as push but reversed.
5. **Sparse pull** — 16 400-byte file with holes lands sparse on local disk (`-S` honoured).
6. **Partial naming contract** — interrupted transfer leaves the deterministic tempfile name; retry resumes from the surviving prefix.

`tests/v7_rsync_e2e_vm.rs` (gated `e2e-vm`) runs the SFTP transport against the same VM: round-trip + idempotent second-pass + symlink + perms preservation.

**MCP-host path** — verified independently of the direct-transport tests: `ssh_rsync transport=Wire` from local `/tmp/mcp-fixed-001/` to `vm.services:/tmp/mcp-fixed-001/` against a live `rsync 3.2.7` server produces byte-identical sha256 across the 3-file fixture (`a.txt`, `b.txt`, `nested/c.txt`). The MCP-path proof rides the full call chain `tool_router::ssh_rsync` → `RsyncSyncUseCase::execute` → `WireRsyncTransport::start_session` → openrsync wire driver, surfacing four bugs that the direct-transport tests missed (see [Post-merge fixes](#post-merge-fixes)).

## Post-merge fixes

The v7.0.0 merge cleared the openrsync port (slices 1–10) and the SFTP transport body, but the live MCP-host call path surfaced four follow-up bugs the direct-transport tests in `tests/v7_rsync_wire_e2e_vm.rs` could not detect. The Python integration suite (`scripts/test_v7_rsync_*.py`, [99262b1](https://github.com/farchanjo/ssh-mcp/commit/99262b1)) drove `ssh_rsync` end-to-end through the `tool_router` → `RsyncSyncUseCase` → transport stack and triaged each one. All four landed as wire-additive fixes — no DTO field shapes changed, no error codes added, no env var defaults moved.

| # | Bug | Surface | Commit | Fix shape |
|---|---|---|---|---|
| 1 | Composition root reached the no-registry stub | Every `transport=Wire` (and `Auto` route picking Wire) hit `RSYNC_PROTOCOL_ERROR` "Wire transport (rsync v31 wire-compat) is being implemented" because `composition::prod` instantiated `WireRsyncTransport::new()` instead of `with_registry(sftp.handle_registry().clone())`. | [`bfc68c6`](https://github.com/farchanjo/ssh-mcp/commit/bfc68c6) | One-line composition wiring — the production constructor now reuses the same `SshHandleRegistry` that `ssh_exec` / `ssh_upload` already own. |
| 2 | `RsyncSyncUseCase::execute()` never drained the progress lane | `ssh_rsync_stats` reported `STATUS: pending` indefinitely; lane closed without a terminal status flip. The use case returned `STARTED` immediately but never spawned a task to fold `RsyncProgressEvent` into the `RsyncSession` aggregate. | [`4fb0b35`](https://github.com/farchanjo/ssh-mcp/commit/4fb0b35) | New `spawn_progress_pump(picked, &rsync_id, session)` after `register_session`. `pump_progress_events` calls `transport.recv_event(rsync_id)` in a loop, folds non-terminal events through `apply_counter_event`, and exits on `SyncCompleted` / `SessionFailed` (or marks `Failed` on lane close / transport error). Lock-free: zero new `Mutex`; the spawned task owns its `recv_event` future end-to-end. |
| 3 | `opts.dry_run` / `opts.exclude` / `opts.include` were silently dropped | Each field rode `SshRsyncArgs.opts` correctly, but neither flowed through `RsyncSyncRequest` → `RsyncStartRequest` to the per-call adapter merge. The SFTP walker + executor were already wired correctly; they just received empty / `false` baseline values. | [`4fb0b35`](https://github.com/farchanjo/ssh-mcp/commit/4fb0b35) | Extend both request types with `dry_run` / `exclude` / `include`; the SFTP transport's per-call opts now OR `dry_run` over the baseline and override exclude / include when non-empty (preserves the `with_fs(opts)` test pattern). |
| 4 | Wire transport shipped `host:path` to the remote `rsync --server` cmdline | The MCP DTO accepts `dst="vm.services:/tmp/foo/"`; the wire transport passed the spec verbatim into `build_rsync_server_cmdline`, so the server saw `rsync --server -e.LsfxC -r . vm.services:/tmp/foo/` and aborted with `MSG_IO_ERROR` "0 files transferred". The host-prefix is an rsync-CLI directive that the server never expects in its argv. The Rust e2e tests always passed bare paths so they never caught it. | [`81e3573`](https://github.com/farchanjo/ssh-mcp/commit/81e3573) | New `strip_host_prefix(spec) -> &str` helper splits on the first `:`; called at both push (`request.dst`) and pull (`request.src`) sites before `build_rsync_server_cmdline`. |

A fifth bug (Bug 3 in the Python suite numbering) is an architectural limitation rather than a regression: the SFTP transport walks both `src` and `dst` through a single `RsyncSftpFsPort`, so end-to-end push from a local source onto a remote dst needs a local-FS adapter for `RsyncSftpFsPort`. Today the e2e-vm test pre-creates the source on the remote side; a follow-up slice will land the local-FS bridge. The two related Python tests (`test_rsync_vm_sftp_push` / `test_rsync_vm_sftp_pull`) carry `@pytest.mark.xfail(strict=False)` and auto-pass when the bridge ships.

After all four fixes, the Python integration suite (`scripts/test_v7_rsync_http.py` 10 tests + `scripts/test_v7_rsync_stdio.py` 7 tests + `scripts/test_v7_rsync_vm.py` 4 tests) reports **19 passed + 2 xfailed**.

### Wire-shape deviations from upstream rsync 3.2.7

The openrsync port follows OpenBSD's protocol-27-pinned shape; rsync 3.2.x demands a strict superset for protocol 31 negotiation. Each deviation below is documented inline in the source.

| Area | openrsync canon | Our port (proto 31/32) | Reason |
|---|---|---|---|
| `RSYNC_PROTOCOL` constant | 27 | **32** (was 31; bumped to match rsync 3.4.x administrative signal — wire-format identical to 31; negotiates down to 31 against rsync 3.2.x servers) | rsync 3.4.0 incremented the protocol number as part of CVE-2024-12084..12088 + 12747 fixes; zero new wire branches |
| Flist flag width | 8-bit `FLIST_*` | 16-bit `XMIT_*` (`XMIT_EXTENDED_FLAGS = 0x04` always set in the low byte) | rsync server enforces 16-bit form at proto >= 28 |
| File size / mtime / uid / gid | i32 / i64 sentinel pairs | varint30 / varlong30 (`io.c::read_varint` + 64-entry byte-extra table) | proto >= 30 wire encoding |
| Per-file checksum | MD4 with seed prepended (`MD4(seed_le \|\| file_bytes)`) | MD5 plain (`MD5(file_bytes)`, no seed) at proto >= 30 | `checksum.c::parse_csum_name` `CSUM_MD5` arm |
| File-list indices | i32 `read_int` | 1–6 byte ndx codec (diff-from-prev with `NDX_DONE = -1` collapsing to 0x00) | proto >= 30 reduces wire bytes |
| Multi-phase loop | single phase + sender done sentinel | 2-phase loop + per-phase NDX echo + final `NDX_DONE` per `sender.c` line 464 | proto >= 29 generator redo phase |
| Goodbye exchange | single `NDX_DONE` read | read + write + read at proto >= 31 | `main.c::read_final_goodbye` lines 875..906 |
| Mplex tag space | contiguous block at `MPLEX_BASE = 7` | sparse: `MSG_DATA = 0`, `MSG_REDO = 9`, `MSG_STATS = 10`, `MSG_IO_ERROR = 22`, `MSG_NOOP = 42`, `MSG_SUCCESS = 100`, `MSG_DELETED = 101`, `MSG_NO_SEND = 102` | upstream `enum msgcode` in `rsync.h` |
| Filter list terminator | always emit `int32(0)` before flist | omit when `am_sender && !receiver_wants_list` (no `--delete` / `--prune-empty-dirs`) | `exclude.c::send_filter_list` lines 1644..1660 |
| Post-flist `io_error` sentinel | always read trailing `int32` | only at proto < 30; at proto >= 30 the io_error encodes into the flist's end-of-list `XMIT_IO_ERROR_ENDLIST` short | `flist.c::recv_file_list` line 2728 |
| `compat_flags` varint | not present | conditional on `negotiated >= 30`, server → client only, multi-byte (e.g. `0x81 0x7f` for `0x17e`) | `compat.c::setup_protocol` |
| Mplex output framing on client side | client writes raw | client mplex-frames its output too at proto >= 30 (`io_start_multiplex_out`) | `main.c::client_run` lines 1297..1300 |
| Handshake leftover bytes | discarded | preserved + chained in front of channel reader via `AsyncReadExt::chain` | server frequently piggy-backs first inner-protocol bytes on the seed segment |

### Lock-free invariants (closing statement)

Zero new `Mutex<T>` on hot paths landed across the v7.0 cycle. The single documented exception (`LaneState::{rx, join}` wrapping a per-session `mpsc::Receiver` and `JoinHandle` in both `wire/mod.rs` and `sftp/mod.rs`) is per-lane, per-session, never held across an `.await` of another resource. Every flist value-object, block-set, hash kernel, sender / receiver state machine, ndx cursor, phase counter, and bandwidth token bucket lives by-value on the per-task stack or threads as `&mut` through the call chain. The `mpsc::Sender` half of the lane is `Sync` and crosses tasks freely; the `Receiver` half lives behind the documented mutex slot and never crosses a `.await` boundary.

`grep -rn "Mutex" src/adapters/rsync/{wire,sftp}/` reports four matches across the two transports, all in the documented `LaneState::{rx, join}` exception. `cargo clippy --release --all-features -- -D warnings` passes the strict `mutex_atomic = "deny"` + `await_holding_lock = "deny"` baseline.

### Test coverage summary

| Surface | Counts | Notes |
|---|---|---|
| Library tests | 1986 passing (release build, deterministic) | Includes the rsync transport, value objects, use case, and rendering layers. |
| Integration smoke (`tests/v7_rsync_smoke.rs`) | 9 tests | Transport selection, capability gates, cancel idempotency, both-transport drive against fakes. |
| Rsync chaos suite (`tests/chaos_rsync.rs`) | 16 scenarios | Rsync-specific chaos coverage layered on the existing v5 + v6 chaos baseline. |
| Rsync property suite (`tests/property_rsync.rs`) | 9 strategies | Rsync property coverage on top of the v5 + v6 strategies. |
| Rsync loom invariants (`tests/lockfree_invariants_rsync.rs`, gated `#[cfg(loom)]`) | 7 scenarios | Lane mpsc + atomic counter contention modeled for the rsync transport. |
| Wire e2e (`tests/v7_rsync_wire_e2e_vm.rs`, gated `e2e-vm`) | 6 tests | Push, pull, incremental push, incremental pull, sparse pull, partial naming contract — all against rsync 3.2.7 on a real Linux VM. |
| SFTP e2e (`tests/v7_rsync_e2e_vm.rs`, gated `e2e-vm`) | 2 tests | Round-trip + idempotent second-pass against a real Linux VM. |
| Python integration (`scripts/test_v7_rsync_{http,stdio,vm}.py`) | 21 tests (19 passed + 2 xfailed) | End-to-end MCP-host call path through `tool_router` → `RsyncSyncUseCase` → transport. Drives paramiko fixtures (HTTP / stdio) and the live VM (gated `requires_vm`). Surfaces the four post-merge bugs above; the two xfails cover the deferred local-FS adapter for `RsyncSftpFsPort`. |
| Carry-over chaos (`tests/chaos/`) | 41 scenarios | Pre-v7 coverage; preserved without regression. |
| Carry-over property (`tests/property/`) | 32 strategies | Pre-v7 coverage; preserved without regression. |
| Carry-over loom invariants (`tests/lockfree_invariants.rs`, gated `#[cfg(loom)]`) | 20 scenarios | Pre-v7 coverage; preserved without regression. |

`cargo build --release` and `cargo build --release --no-default-features` both stay warning-free under the strict 3-layer lint baseline.
