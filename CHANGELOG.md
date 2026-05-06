# Changelog

All notable changes to ssh-mcp are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Tests

- **chaos + property + loom coverage for v7.0 rsync hybrid transport**.
  - `tests/property_rsync.rs` — 9 proptest strategies fuzzing the
    public wire codec: flist round-trip at proto 27 and proto 31,
    NDX diff codec, `hash_split` ↔ `combine_s1_s2` inverse, rolling
    `hash_roll` matches recompute, `FileHasher` MD5 (proto 31)
    streaming-API equivalence, `FileHasher` MD4 (proto 27) seed
    sensitivity, mplex `MSG_DATA` framing round-trip, and
    `MplexTag::to_wire` / `from_wire` round-trip on every byte.
  - `tests/chaos_rsync.rs` — 16 chaos scenarios driving the
    `RsyncSyncUseCase` against the deterministic fake adapters under
    `--features test-fixtures`: idempotent concurrent cancel, lane-drop
    terminal `recv_event`, scripted protocol-error propagation,
    bounded concurrent SFTP sessions, capability-gate short-circuit
    (`preserve_hardlinks` + `preserve_perms` + `preserve_symlinks`
    branches), unknown-id cancel / stats / try_stats taxonomy, Auto
    fallback on probe-too-old, wire start failure not registering
    a repo entry, burst cancel after start, concurrent cancel + stats
    no-panic, `SessionFailed` event drains cleanly, `list_active`
    after burst open + close, and direct lane `recv_event` not
    perturbing the repo.
  - `tests/lockfree_invariants_rsync.rs` — 7 loom models (gated
    `#[cfg(loom)]`) covering: rsync session status CAS race
    (Pending/Probing → Running vs cancel latch), no double-cancel,
    lane mpsc + cancel race never underflows, stats counters
    monotonic under concurrent producer + observer, NDX cursor
    monotonic under contention, `WireSession::total_*` saturating-add
    convergence, lane mpsc DropOldest no-underflow. Same upstream
    russh / axum loom incompatibility as the existing
    `tests/lockfree_invariants.rs` — file is structurally ready and
    will run once upstream resolves the `tokio::net` / loom mock
    drift.
- Integration test count: 110 → **134** across **9** binaries (added
  `chaos_rsync` 16 + `property_rsync` 9). Loom invariant total: 20
  → **27** across two files.

## [7.0.0] — 2026-05-06 (canonical release)

ADR 0011 — Rsync hybrid transport. **Wire-additive on the MCP surface**: the v6.x tool catalogue, resource URI schemes, push narrative, and structured-content payloads are byte-identical. Three new MCP tools (`ssh_rsync`, `ssh_rsync_cancel`, `ssh_rsync_stats`), one new push-resource scheme (`rsync://<id>/progress`), and two integrated transports (`WireRsyncTransport` + `SftpRsyncTransport`) ship in this release.

This entry supersedes the prior `[7.0.0] — 2026-05-05` and the ten alpha entries (`alpha.1` through the openrsync slice-10 alpha) that incrementally landed the work. Final state below; per-slice narrative kept in the alpha entries for traceability.

### What ships

- **`SftpRsyncTransport`** (`src/adapters/rsync/sftp/`) — universal SFTP fallback. Drives a recursive sync entirely through SFTP `readdir` + `stat` + `read` + `write` + `setstat` (no remote helper). Supports recursive mirror, dry-run, `--delete`, gitignore-style exclude/include patterns, attribute preservation (perms, mtime, owner, group, symlinks) gated on a per-session `SftpFeatures` capability probe, and `--bwlimit` via a lock-free token bucket. Verified end-to-end against a real Linux VM via `tests/v7_rsync_e2e_vm.rs` (2 tests, gated `e2e-vm`).
- **`WireRsyncTransport`** (`src/adapters/rsync/wire/`) — canonical port of OpenBSD `openrsync` (BSD/ISC) speaking rsync wire protocol v31 against a remote `rsync --server` over the existing russh exec channel. Push and pull both byte-identical against `rsync 3.2.7` on a real Linux VM. Verified end-to-end via `tests/v7_rsync_wire_e2e_vm.rs` (6 tests, gated `e2e-vm`):
  - **Slice 1** — handshake (proto 27→31) + multiplex I/O re-port (`session.rs`, `io.rs`).
  - **Slice 2** — file-list encode/decode + local-source walker (`flist.rs`).
  - **Slice 3** — block-checksum decode + Adler32/MD4 hash kernels + whole-file token stream + sender state machine (`blocks.rs`, `hash.rs`, `tokens.rs`, `sender.rs`).
  - **Slice 4** — protocol 31 lift (16-bit `XMIT_EXTENDED_FLAGS`, varint30/varlong30, ndx codec, iflags) (`flist.rs`, `ndx.rs`, `sender.rs`).
  - **Slice 5** — per-file digest = MD5 at proto >= 30 + multi-phase `send_files` loop + final post-loop `NDX_DONE` + proto-31 goodbye (`hash.rs`, `sender.rs`).
  - **Slice 6** — block-match path: Adler32 hashtable + sliding window + MD4-with-seed strong verify + literal/match tokens (`match_.rs`, `blocks.rs`).
  - **Slice 7** — pull direction + receiver state machine + flist decode (`XMIT_MOD_NSEC` + `IO_ERROR_ENDLIST` + post-sort indices) (`receiver.rs`, `flist.rs`).
  - **Slice 8** — incremental pull (local block-signature emit + match-token consume on the receiver side) (`receiver.rs`).
  - **Slice 9** — `--delete` pass + attrs apply (`-p`, `-t`, `-o`, `-g`, `-l`) plumbed end-to-end (`receiver.rs`, `sender.rs`, `flist.rs`).
  - **Slice 10** — `--partial` (deterministic tempfile + skip-unlink-on-error) + `-S` sparse hole detection (`receiver.rs`).
- **3 new MCP tools** — `ssh_rsync` returns `STARTED` immediately; `ssh_rsync_stats` reads the live aggregate; `ssh_rsync_cancel` is idempotent. Block-markdown wire shape with `KEY: value` lines + 8-hex-char nonce blocks; structured-content twin under `tool: "ssh_rsync"`.
- **1 new push scheme** — `rsync://<RSYNC_ID>/progress` carries `application/json` events: `SessionStarted`, `FileStarted`, `FileProgress`, `FileCompleted`, `FileSkipped { reason: SizeMatch | MtimeMatch | DryRun }`, `FileFailed`, `SyncProgress`, `SyncCompleted`, `SessionFailed`. ADR 0006 byte-threshold flush (default 64 KiB) + `Snapshot` lag policy by default.
- **6 new error codes** — `RSYNC_NOT_FOUND` (RESOURCE), `RSYNC_VERSION_TOO_OLD` (POLICY), `RSYNC_PROTOCOL_ERROR` (TRANSPORT), `RSYNC_FILE_LIST_TOO_LARGE` (POLICY), `RSYNC_PARTIAL_TRANSFER` (TRANSPORT), `SFTP_FEATURE_MISSING` (POLICY). Total error-taxonomy size grows from 40 to 46. Each carries a one-sentence DETAIL line tuned for direct LLM consumption — see [docs/LLM_GUIDE.md → Error handbook](docs/LLM_GUIDE.md#error-handbook).
- **3 new env vars** — `SSH_RSYNC_PROBE_TIMEOUT_MS` (default 2000), `SSH_RSYNC_BLOCK_SIZE` (default auto), `SSH_RSYNC_FILE_LIST_LIMIT` (default 1 000 000). The agent-cache env vars (`SSH_RSYNC_AGENT_CACHE_TTL_DAYS`, `SSH_RSYNC_AGENT_CACHE_DIR`) from the original v7.0 plan were dropped along with the agent path during the v7.0.0-alpha.2 retrenchment.

### Wire compatibility

- **v6.1 hosts unchanged.** No existing tool gains a parameter, no existing response gains a field. The only surface change is the addition of `ssh_rsync*` and the `rsync://` resource scheme.
- **Resource URI schemes for v6.x** — byte-identical: `shell://`, `command://`, `transfer://`, `session://`, `forward://`, `serial://` all unchanged.
- **Cursor / SubId / lifecycle.** `rsync://<id>/progress` is an ordinary push resource — subscribes mint a `SubId` per ADR 0004, attach via `sub_open`, drain via `notifications/resources/updated`. ADR 0003 lifecycle binding applies: `release_when_no_subs = true` on `ssh_rsync` releases the session when the last subscriber detaches.
- **Daemon NDJSON.** `ssh-mcp-tail` parser accepts the new request shape via `serde`'s additive deserialisation; `notifications/resources/updated` events for `rsync://*` URIs surface as ordinary push events keyed on the `rsync_id`.

### Deferred

- **`-c` checksum delta over wire-format extension** — requires bytes outside the openrsync canon. SFTP path raises `SFTP_FEATURE_MISSING`; Wire path raises the same. Successor ADR will scope.
- **`-H` hardlinks** — same reasoning.
- **Encoder symmetry for owner/group when `-o`/`-g` negotiated** — partial. Decode honoured; encode skips bytes the server would otherwise expect on a strict round-trip.
- **`chown` syscall wiring** — root-only path on the receiver.
- **`-D` devices, `-X` xattrs, `-A` ACLs** — out of scope for v7.x.

### Lock-free invariants (closing statement)

Zero new `Mutex<T>` on hot paths landed across the v7.0 cycle. The single documented exception (`LaneState::{rx, join}` wrapping a per-session `mpsc::Receiver` and `JoinHandle` in both `wire/mod.rs` and `sftp/mod.rs`) is per-lane, per-session, never held across an `.await` of another resource. `cargo clippy --release --all-features -- -D warnings` passes the strict baseline; `grep -rn "Mutex" src/adapters/rsync/{wire,sftp}/` reports four matches across the two transports, all in the documented exception.

### Tests

- **1966** library tests passing (release build, deterministic).
- **9** integration smoke tests (`tests/v7_rsync_smoke.rs`): transport selection, capability gates, cancel idempotency, both-transport drive against fakes.
- **16** rsync chaos scenarios (`tests/chaos_rsync.rs`) + **9** rsync property strategies (`tests/property_rsync.rs`) + **7** rsync loom invariants (`tests/lockfree_invariants_rsync.rs`, gated `#[cfg(loom)]`).
- **6** wire e2e tests (`tests/v7_rsync_wire_e2e_vm.rs`, gated `e2e-vm`): push, pull, incremental push, incremental pull, sparse pull, partial naming contract — all against `rsync 3.2.7` on a real Linux VM.
- **2** SFTP e2e tests (`tests/v7_rsync_e2e_vm.rs`, gated `e2e-vm`): round-trip + idempotent second-pass against the same VM.
- **41** carry-over chaos + **32** carry-over property strategies + **20** carry-over loom invariants — preserved without regression.

### Documentation

- ADR 0011 final state — slices 1–10 merged with one-line outcomes; slices 11–12 deferred with rationale; final architecture diagrams (push pipeline + pull pipeline + transport selection + SFTP fallback); wire-shape deviation table; lock-free closing statement; test coverage summary.
- [docs/MIGRATION.md → v6.1 → v7.0](docs/MIGRATION.md#v61--v70) — final canonical migration guide.
- [docs/API.md → Rsync (3, v7.0)](docs/API.md#rsync-3-v70--adr-0011) — three new tool entries with full schemas, examples, and error codes.
- [docs/RESOURCES.md → v7.0 / ADR 0011](docs/RESOURCES.md#v70--adr-0011--rsyncidprogress-event-types) — `rsync://<id>/progress` push scheme + event types.
- [docs/CONFIGURATION.md → v7.0 / ADR 0011](docs/CONFIGURATION.md#v70--adr-0011--rsync-hybrid-transport-3-env-vars) — three new env vars.
- [docs/LLM_GUIDE.md → Recursive sync with rsync](docs/LLM_GUIDE.md#v70--adr-0011--recursive-sync-with-rsync) — happy paths + error handbook entries for all 6 new codes.
- [docs/ARCHITECTURE.md → v7.0 hybrid rsync transport](docs/ARCHITECTURE.md#phase-5--rsync-hybrid-transport-v70-final) — hexagonal layer additions + lock-free invariants.

## [7.0.0-alpha.8] — 2026-05-06

ADR 0011 Wire transport — slice 2 of the openrsync port lands. The
file-list (flist) encode/decode plus a minimal local-source walker
ship in a new `src/adapters/rsync/wire/flist.rs`, wired into the
session driver. Verified end-to-end against `rsync 3.2.7 --server` on
a real Linux VM: the server's generator subsystem accepts our flist
bytes, then waits for the slice-3 block-checksum protocol that
remains on the backlog.

### Added

- `src/adapters/rsync/wire/flist.rs` — port of OpenBSD openrsync's
  `flist.c` (BSD/ISC). Carries `Flist` value-object mirroring
  `struct flist + struct flstat`, the eight `FLIST_*` flag constants
  (verbatim from `flist.c` lines 55..62), `recv_flist` / `send_flist`
  drivers over `MplexReader` / `MplexWriter`, and `gen_flist_local`
  for walking a local directory tree into a sorted `Vec<Flist>`.
  Security checks for absolute / backtracking / zero-length paths
  ported verbatim from openrsync's `flist_recv_name`. 16 unit tests
  cover every FLIST_* flag combination on round-trip.
- Slice-2 driver `drive_slice2_session` in `src/adapters/rsync/wire/mod.rs`
  — handshake → emit `RsyncProgressEvent::SessionStarted { transport: Wire }`
  → walk local source → send empty filter-list terminator (single
  `MSG_DATA` frame with `0u32` LE payload) → send flist → drain a few
  post-flist mplex frames → return the slice-3 boundary error pointing
  at signature + tokens + sender state machines.
- `docs/MIGRATION.md` — `v7.0.0-alpha.8` section documenting the
  slice-2 surface, the openrsync port deviations (FLIST_* vs XMIT_*,
  always-LONG name encoding, no identifier sub-lists, no
  `flist_topdirs` re-pass), and the preserved lock-free invariants.

### Changed

- `src/adapters/rsync/wire/mod.rs::WIRE_NEXT_SLICE_DETAIL` — now reads
  `"openrsync port: handshake + mplex + flist landed (slice 2);
  signature + token + sender state machines are slice 3"`.
- `src/adapters/rsync/wire/mod.rs::run_wire_session` — receives `src`
  by value and threads it through `gen_flist_local`. The slice-2
  pre-flist `SessionStarted` event is emitted before flist send so
  consumers see the canonical envelope.

### Lock-free invariants (preserved)

Zero new `Mutex` fields on hot paths. The single documented exception
(`LaneState::{rx, join}` in `wire/mod.rs` wrapping a per-session
`mpsc::Receiver` and `JoinHandle`) is unchanged from slice 1.

### Wire-compat compatibility notes (against rsync 3.2.7)

1. **`FLIST_*` vs `XMIT_*`**: the openrsync port uses the
   protocol-27 8-bit flag set (`FLIST_TOP_LEVEL`, `FLIST_NAME_LONG`,
   etc.). The upstream rsync `XMIT_EXTENDED_FLAGS` two-byte header
   (used at protocols >= 28) is not part of the port; the rsync
   server parses the 8-bit shape identically when the client never
   advertises `XMIT_EXTENDED_FLAGS`.
2. **Always-LONG name encoding**: every entry sets `FLIST_NAME_LONG`,
   so the name remainder length is always a 4-byte LE i32. Mirrors
   openrsync's "for ease, make all of our filenames be 'long'"
   comment in `flist_send`.
3. **No identifier sub-lists**: numeric uid/gid only this slice;
   `idents_send` / `idents_recv` ship later.

## [7.0.0-alpha.5] — 2026-05-06

ADR 0011 Wire transport — first three layers of the rsync v31
wire-compat client land real: handshake, multiplex framing, file-list
encode/decode. The end-to-end pump exec's `rsync --server --sender`
on the remote, runs the version + checksum-seed exchange, and reaches
the mplex pump. Verified against rsync 3.2.7 on a real Linux VM.

The wire transport now surfaces a 5-second
[`DomainError::Timeout`] when the post-handshake mplex pump is
blocked waiting on bytes the next slice's sender state machine
needs to emit (filter-list send + file-list send for the push path,
or the receiver's request stream for pull). The handshake reaching
the seed exchange against real bytes proves the codec layers work;
the deferred state machines are honestly surfaced as `TIMEOUT` rather
than pinned in a permanent deadlock.

### Added

- `src/adapters/rsync/wire/handshake.rs` — rsync v31 handshake
  (version + server-supplied seed) with full unit-test coverage
  including a `tokio::io::duplex` smoke that mirrors the real wire
  bytes captured from rsync 3.2.7.
- `src/adapters/rsync/wire/mplex.rs` — rsync multiplex I/O codec
  (`MplexReader` / `MplexWriter` over arbitrary `AsyncRead+Write`,
  `MplexTag` enum mapping the 18 `MSG_*` tags through the
  `MPLEX_BASE` bias). Frame-aggregation helpers + frame-size cap +
  fatal-frame propagation.
- `src/adapters/rsync/wire/flist.rs` — file-list encode/decode
  with rsync's varint codec (1- to 9-byte little-endian-tail
  format), the XMIT_* flag matrix (SAME_NAME, SAME_TIME, SAME_MODE,
  SAME_UID, SAME_GID, LONG_NAME, EXTENDED_FLAGS, HLINKED), symlink
  target capture, hardlink-index capture, and the trailing-zero
  list terminator. 16 unit tests (varint roundtrip, XMIT collapse
  paths, EXTENDED_FLAGS high-byte decode, truncated-payload
  diagnostic).
- `WireRsyncTransport::with_registry` — production wiring against
  the shared `SshHandleRegistry`. The legacy `WireRsyncTransport::new`
  stub stays as the unwired fallback so the composition root can
  thread the registry in incrementally.
- `tests/v7_rsync_wire_e2e_vm.rs` — opt-in end-to-end test
  (`cargo test --features e2e-vm --test v7_rsync_wire_e2e_vm --
  --include-ignored --nocapture`) that drives the live wire
  transport against `rsync 3.2.7` on a real Linux host.

### Changed

- `WireSession::checksum_seed` now reflects the wire reality:
  rsync sends the seed unilaterally (server -> client) over SSH;
  the client-sent seed described in the original brief is a
  daemon-mode artefact that does not exist in the SSH-tunnelled
  flow. The handshake API drops the `local_seed` parameter.

### Wire-compat compatibility notes (vs the original ADR 0011 brief)

- The brief's textual `@RSYNCD: 31\n` greeting is the **daemon-mode**
  handshake (`rsync://` URL scheme over a dedicated TCP server). The
  SSH-tunnelled path emits **binary** 4-byte LE u32 fields. We follow
  the SSH-tunnelled wire reality.
- Mode / mtime / uid / gid in v31 file-list entries are **varint**,
  not raw little-endian. The flist codec follows
  `flist.c::send_file` in rsync 3.2.7.
- `MPLEX_BASE = 7` — every mplex tag byte on the wire is biased by
  this offset (`socket.c::write_mplex` in rsync 3.2.x). The
  `MplexTag::from_wire` / `MplexTag::to_wire` helpers hide the bias
  from callers.
- The rsync server-sender path waits for the client to emit an empty
  filter-list terminator before unblocking and emitting the
  `MSG_FLIST` chunks. The current slice sends the terminator but the
  full filter-list semantics + the file-list expectation byte that
  follows it on the push side land in the next slice — the e2e test
  documents this with a 5-second wire-session deadline that surfaces
  the deadlock as a clean `TIMEOUT` instead of pinning the lane.

## [7.0.0-alpha.4] — 2026-05-06

ADR 0011 SFTP transport — capability probe wiring + e2e VM coverage. The
`RsyncSyncUseCase` now consults a per-session-cached `SftpFeatures`
snapshot before driving the SFTP path, surfaces `SFTP_FEATURE_MISSING`
with a one-sentence DETAIL line for `preserve.symlinks` /
`preserve.perms` requests against locked-down servers, and ships an
opt-in end-to-end test (`tests/v7_rsync_e2e_vm.rs`, gated `e2e-vm`)
that drives the live transport against a real Linux host. First
real-world contact surfaced two bugs in the SFTP transport — both
fixed in this slice.

### Added

- `RsyncSyncDeps` constructor bundle on `RsyncSyncUseCase` — port
  cluster the composition root passes in to keep the constructor
  under the `too_many_arguments` Clippy threshold while still
  accepting the new `Arc<RusshRsyncSftpFs>` collaborator that drives
  the SFTP capability probe.
- `RsyncSyncRequest::preserve_symlinks` / `preserve_perms` —
  request-level mirrors of `opts.preserve.links` / `opts.preserve.perms`
  the use case consumes to fire the new probe-driven capability
  gates.
- Per-session probe cache (`DashMap<SessionId, SftpFeatures>`) inside
  `RsyncSyncUseCase` so repeated `ssh_rsync` calls against the same
  SSH session pay the probe round-trips only once.
- `tests/v7_rsync_e2e_vm.rs` — opt-in end-to-end test (`cargo test
  --features e2e-vm v7_rsync_e2e_vm -- --ignored --nocapture`) that
  drives the live SFTP rsync transport against a real Linux host.
  Defaults to `root@vm.services` with `~/.ssh/id_rsa`; honours
  `SSH_MCP_E2E_HOST` / `_USER` / `_KEY_PATH` env-var overrides.
- `e2e-vm` Cargo feature (default-off) and `tempfile` dev-dep.
- 4 new use-case tests pinning the symlink / perms / cache-reuse
  branches; 3 new v7 smoke tests pinning the same branches against
  the integration fake stack.

### Fixed

- **Destination root must exist before the recursive walk** —
  `walk_both_trees` (`src/adapters/rsync/sftp/mod.rs`) now best-effort
  `mkdir`s the destination root before walking it. "Path exists"
  failures are swallowed (the happy path on a re-run). Without this
  fix the comparator emitted `Mkdir` actions for every nested child
  but never for the root itself, so every subsequent `write_chunk`
  failed with `No such file`.
- **OpenSSH `SSH_FXP_SYMLINK` argument-order quirk** —
  `RusshRsyncSftpFs::symlink` (`src/adapters/sftp/rsync_fs_impl.rs`)
  now passes `(target_first, link_second)` to russh-sftp's
  `symlink(linkpath, targetpath)` so the wire-level swap matches
  OpenSSH's long-standing inversion (documented in `OpenSSH
  PROTOCOL.1`). Without the swap symlinks were being created in the
  wrong direction and surfaced as `SFTP_FEATURE_MISSING` despite the
  server fully supporting symlinks.

### Changed

- `RsyncSyncUseCase` gains an extra generic `Sfs: RsyncSftpFsPort`
  for the capability-probe collaborator. The composition root shares
  the same `Arc<RusshRsyncSftpFs>` between the SFTP transport and
  the use-case probe (both ride the same `SshHandleRegistry`).
- `RsyncSyncUseCase::execute` now runs the SFTP capability probe
  inline before invoking the SFTP transport's `start_session` — the
  probe is cached per-session, so repeated calls amortise the three
  round-trips (mkdir + setstat + symlink) across the lifetime of the
  use case.

### Documentation

- `docs/API.md` — flips `transport=Sftp` from "being implemented" to
  live for the supported subset; documents the four capability gates
  (`preserve.symlinks` / `preserve.perms` / `preserve.hardlinks` /
  `verify_checksum`) with the exact detail strings they emit.
- `docs/LLM_GUIDE.md` — adds a "When SFTP transport is enough"
  section, expands the `SFTP_FEATURE_MISSING` error handbook entry
  to cover all four gate cases, and updates the v7 status block
  from "alpha.2 stubs" to "alpha.4 live SFTP".
- `docs/MIGRATION.md` — adds a v7.0.0-alpha.4 sub-section with the
  e2e VM test invocation + env-var matrix and a "SFTP server
  compatibility notes" subsection documenting the OpenSSH symlink
  quirk and the destination-root mkdir requirement.

## [7.0.0-alpha.3] — 2026-05-06

ADR 0011 SFTP transport — first real implementation slice. The
`SftpRsyncTransport` adapter goes from honest "being implemented" stub
to a live recursive-mirror pipeline driven entirely through SFTP
`readdir` + `stat` + `read` + `write` + `setstat` (no remote helper,
no rsync binary required on the host). The Wire transport remains
stubbed and ships in the next slice.

### Added

- `RsyncSftpFsPort` (`src/ports/rsync_sftp_fs.rs`) — narrow remote
  filesystem port distinct from the legacy `SftpClientPort` so the
  rsync mirror does not perturb the production `ssh_upload` /
  `ssh_download` surface. Surfaces `readdir` / `lstat` / `read_link`
  / `mkdir` / `rmdir` / `remove_file` / `symlink` / `set_metadata`
  / `read_chunk` / `write_chunk`.
- `FakeRsyncSftpFs` (`src/adapters/rsync/sftp/fake.rs`) — in-memory
  fake driving the unit and integration tests; exposes
  `fail_symlink` / `fail_setstat` knobs to exercise SFTP server
  rejection paths.
- `SftpWalker` (`src/adapters/rsync/sftp/walker.rs`) — recursive BFS
  walker over `RsyncSftpFsPort::readdir` honouring gitignore-style
  excludes / includes via `globset` and refusing the walk past the
  `SSH_RSYNC_FILE_LIST_LIMIT` cap with `RSYNC_FILE_LIST_TOO_LARGE`.
- `compare_trees` (`src/adapters/rsync/sftp/comparator.rs`) — derives
  ordered `SyncAction` lists (`Mkdir` / `Transfer` / `Setstat` /
  `Symlink` / `Skip` / `Delete`) from a source / destination pair.
  Honours `--delete`, `--checksum` (`force_transfer`), and
  preserve-mask differential setstat.
- `SftpExecutor` (`src/adapters/rsync/sftp/executor.rs`) — applies
  the action list, emits `RsyncProgressEvent`s into an `mpsc` lane
  (32 KiB chunks per ADR 0010, 64 KiB `FileProgress` cadence),
  enforces `dry_run`, cancellation tokens, and an optional
  token-bucket bandwidth limit.
- `TokenBucket` (`src/adapters/rsync/sftp/bwlimit.rs`) — lock-free
  refill-on-demand rate limiter sized in bytes-per-second; drives
  `--bwlimit` for the SFTP path. Cancel-safe.
- `globset` 0.4 dependency for the include / exclude matcher.

### Changed

- `SftpRsyncTransport` is now generic over `F: RsyncSftpFsPort` with
  `NoopFs` as the default. `SftpRsyncTransport::new()` and
  `SftpRsyncTransport::without_fs()` keep returning the honest
  "being implemented" stub for the production composition root that
  has not wired the real adapter yet; downstream callers wire a real
  filesystem via `SftpRsyncTransport::with_fs(fs, opts, capacity)`
  and start receiving live `SessionStarted`, per-file progress, and
  `SyncCompleted` events through `recv_event`.
- `RsyncTransportPort::start_session` for the SFTP transport mints a
  UUIDv7-suffixed `RsyncId`, spawns the pipeline as a background
  task, and registers a per-session lane for `recv_event` /
  `close`.

### Tests

- 1807 lib tests (+32 vs alpha.2): walker (BFS, kind classification,
  exclude / include, file-list cap), comparator (skip / transfer /
  delete / symlink replace / setstat differential / parent-before-child
  ordering), executor (skip / dry-run / transfer / cancel / delete /
  symlink unsupported), bwlimit (zero-rate disabled / refill / take
  semantics), fake fs (readdir / write / read / rmdir / knobs).
- Live transport pipeline test (`live_pipeline_emits_session_started_then_completed`)
  drives an end-to-end recursive sync against `FakeRsyncSftpFs` with
  one source file and asserts both the wire frame ordering and the
  byte-identical destination.

## [7.0.0-alpha.2] — 2026-05-06

ADR 0011 architectural retrenchment — the deployed-agent path was retracted
in favour of two integrated transports (`WireRsyncTransport` for the rsync
v31 wire-compat client and `SftpRsyncTransport` for the universal SFTP
fallback). Both transports live in-process inside the host crate; the
workspace collapsed back to a single package. Both transports return
descriptive `RSYNC_PROTOCOL_ERROR` "being implemented" stubs in this slice;
the surface (tools, error taxonomy, push-lane URI shape, lifecycle
binding) is the permanent shape — the next slice fills the transport
bodies without churning anything else.

### Removed

- `crates/ssh-mcp-rsync-agent/` — the deployed binary sub-crate is gone.
- `crates/ssh-mcp-rsync-proto/` — the shared wire-format crate is gone;
  surviving value-object types (`FileKind`, `PreserveFlags`, `RsyncStats`,
  `RsyncProgressEvent`, `RsyncTransportKind`, `SkipReason`, `ErrorCode`)
  moved to `src/adapters/rsync/types.rs`. `RsyncStats` lives in
  `src/domain/rsync.rs` so domain rules stay clean. Wire-format types
  (`RsyncOp`, `RsyncOpPayload`, frame codec, op-code constants) had a
  single producer / single consumer and were dropped along with the
  agent.
- `target/agents/{x86_64-unknown-linux-musl,aarch64-unknown-linux-musl,aarch64-apple-darwin}`
  — the cross-compiled blobs are gone.
- `RsyncAgentDeployPort`, `EmbeddedAgentDeployAdapter`,
  `AgentRsyncTransport`, `SshShellExecAdapter`, the `RemoteShellExec`
  collaborator port, and the agent embed mechanism — every artefact
  under the deleted `src/adapters/rsync/agent/` sub-tree.
- `transport=Agent` on the inbound `RsyncTransportArg` enum (replaced
  by `transport=Sftp`).
- `agent_path` request override on `SshRsyncArgs` (no agent to
  override).
- Cargo features `embed-linux-x86_64`, `embed-linux-aarch64`,
  `embed-macos-aarch64`, `embed-all`.
- 4 error codes: `AGENT_DEPLOY_FAILED`, `AGENT_ARCH_UNSUPPORTED`,
  `AGENT_TRUST_VIOLATION`, `AGENT_NOEXEC_TARGET`. Net error-taxonomy
  delta: 9 → 6 codes.

### Added

- `WireRsyncTransport` (`src/adapters/rsync/wire/mod.rs`) — stub Tier
  1 transport, drives a remote `rsync --server` over the existing
  russh exec channel. Returns `RSYNC_PROTOCOL_ERROR` with detail
  "Wire transport (rsync v31 wire-compat) is being implemented; pass
  transport=Sftp for the SFTP fallback or wait for the next slice".
- `SftpRsyncTransport` (`src/adapters/rsync/sftp/mod.rs`) — stub Tier
  2 transport, drives a recursive sync entirely through SFTP (no
  remote helper). Returns `RSYNC_PROTOCOL_ERROR` with detail "SFTP
  transport is being implemented; install rsync >= 3.2.0 on the
  remote and pass transport=Wire, or wait for the next slice".
- `transport=Sftp` on the inbound `RsyncTransportArg` enum.
- `SFTP_FEATURE_MISSING` error code (POLICY) — surfaced when the
  request asks the SFTP path for hardlinks (`opts.preserve.hardlinks=true`)
  or delta-sync semantics (`opts.verify_checksum=true`); both are
  Wire-only features today.
- New v7 smoke tests covering each transport-selection branch (forced
  Sftp, forced Wire with rsync missing → `RSYNC_VERSION_TOO_OLD`,
  Auto-routes-to-Wire-when-rsync-v31, Auto-routes-to-Sftp-when-rsync-missing,
  cancel-drives-close-on-both-transports, SFTP-with-hardlinks →
  `SFTP_FEATURE_MISSING`).
- `FakeSshClient::queue_exec_string` — scripted one-shot `execute`
  outcome useful for the rsync probe path and other one-shot
  exec assertions.

### Changed

- `RsyncSyncUseCase` generic surface simplified from
  `<T, D, R, SR, Idg, Cfg, Sh>` to `<W, Sf, R, SR, Ssh, Idg, Cfg>`
  where `W: RsyncTransportPort` is the wire-compat transport,
  `Sf: RsyncTransportPort` is the SFTP fallback, and `Ssh: SshClientPort`
  drives the rsync version probe via `execute`.
- `RsyncTransportPort::send_op` was removed — the future Wire and
  SFTP transports drive their on-the-wire operations through
  transport-specific codecs that do not benefit from a generic op
  enum. The port now exposes `start_session` + `recv_event` + `close`
  only.
- Workspace `Cargo.toml [workspace]` block: members list collapses
  to `["."]`; `[workspace.lints.*]` table stays authoritative for
  the strict 3-layer Clippy baseline.

## [7.0.0] — 2026-05-05 (superseded by 7.0.0 — 2026-05-06)

> **Superseded.** This entry captures the agent-path snapshot before the v7.0.0-alpha.2 architectural retrenchment. The deployed-agent transport described below was retracted in favour of two integrated transports (`WireRsyncTransport` + `SftpRsyncTransport`) that live inside the host crate. See the canonical [`[7.0.0] — 2026-05-06`](#700--2026-05-06-canonical-release) entry above for the shipped state.

ADR 0011 phases 5+6+10 — agent fallback wire-format on `linux-x86_64`, full
feature parity (recursive walk, exclude/include, `--delete`, dry-run, bwlimit,
partial), and the `rsync://<id>/progress` push lane. **Wire-additive** on the
MCP surface: the existing 36 tool catalogue is unchanged; the new
`ssh_rsync` / `ssh_rsync_cancel` / `ssh_rsync_stats` tools currently still
return `RSYNC_NOT_IMPLEMENTED` (the agent crate is fully implemented; the
host-side transport adapter — phase 5b — is deferred to a follow-up).

### Added

- **`crates/ssh-mcp-rsync-agent`** is now a real binary, not a skeleton.
  Implements the full custom protocol from ADR 0011 § "Wire format":
  `Hello` handshake, `ListRequest` walking with `walkdir` + `globset`-based
  gitignore-style exclude/include matcher, `SignatureReq` / `DeltaChunk`
  via `fast_rsync`, `Mkdir` / `Symlink` / `Hardlink` / `Chmod` / `Chown`
  / `Chmtime` / `Unlink` via `std::fs` + `nix`, `Progress` / `FileDone`
  beacons, terminal `Cancel` handling. `--bwlimit=KBPS` token-bucket on
  the writer side (60 ms tick granularity).
- **`ResourceKind::Rsync`** variant on both the legacy
  (`adapters::subscription::legacy`) and hexagonal port
  (`ports::subscriber_registry`) enums. `rsync://<id>/progress` URIs
  parse + format end-to-end through the registry helpers.
- **Workspace versions** synced to `7.0.0` (root `ssh-mcp`,
  `crates/ssh-mcp-rsync-proto`, `crates/ssh-mcp-rsync-agent`).

### Fixed

- **`cargo build --no-default-features` regression** in v6.1.0: the
  `ResourceKind::Serial` variant was missing from the
  `read_resource.rs` non-`port_forward` branch's exhaustive match,
  blocking every no-default-features build. Phase 10 also adds the
  matching `Rsync` arms project-wide so the new variant compiles
  under every feature combination.

### Deferred (out of phase scope; tracked for a follow-up)

- **Phase 5 server-side adapter wiring** (`AgentTransportDeploy` +
  `AgentRsyncTransport`). The agent binary is real but the host-side
  russh+SFTP deploy + drive adapter is not yet plumbed into the
  `RsyncTransportPort` "Agent" impl. The tool router stub therefore
  still answers `RSYNC_NOT_IMPLEMENTED` on every `ssh_rsync*` call.
- **Phase 7** cross-arch matrix (`linux-aarch64`, `macos-aarch64`).
- **Phase 8 / 9** wire-compat client v31 + feature parity.
- **Phase 13** loopback OpenSSH integration tests + cross-transport
  property tests.
- The 5 new env vars from ADR 0011 § "Configuration surface" stay
  documented in [docs/CONFIGURATION.md](docs/CONFIGURATION.md) but
  are not yet read by the host transport adapter (which has not been
  wired in this slice).

## [6.1.0] — 2026-05-05

SFTP transfer resume + verify (ADR 0010). **Wire-additive** — every v6.0 host keeps working byte-for-byte; resume is purely opt-in via two new request flags defaulting to `false`. No tool name strings, resource URI schemes, default behaviours, env vars, or MSRV changes.

### Added

- **`ssh_upload(resume: bool?, verify: bool?)`** and **`ssh_download(resume: bool?, verify: bool?)`** — opt-in resume from the first non-overlapping byte. When `resume=true`, the server pre-flights the destination size and seeks both endpoints to the resume offset; the streaming chunk loop then ramps `bytes_transferred` from `resumed_from` to `total_bytes`. When `verify=true` the resume prefix is sha256-hashed on both sides before continuing — costs one extra `ssh_exec` round-trip plus O(offset) bytes hashed remotely.
- **`RESUMED_FROM:` response line** on the `ssh_upload` / `ssh_download` block-markdown body. Emitted **only** when the resume offset is > 0; suppressed for fresh transfers so v6.0 callers see byte-identical wires.
- **`resumed_from: u64`** field on the upload / download `structured_content` payload and on `TransferEntity` (snapshot JSON). `#[serde(default)] = 0` keeps v5/v6.0 payloads backward-compatible.
- **Two new wire codes** in the `STATE` category (extends [ADR 0007](docs/adr/0007-error-taxonomy.md)):
  - `RESUME_OVERSHOOT` — destination is larger than source; refusing to resume.
  - `RESUME_MISMATCH` — verify-mode hash mismatch (local vs remote diverged).
- **NDJSON daemon** (`ssh-mcp-tail`) now accepts `resume` / `verify` on `upload` / `download` ops via `#[serde(default)]`. v6.0 daemon clients work unchanged.
- **Test surface**: `tests/v6_resume_smoke.rs` (12 scenarios, 6 upload + 6 download covering Truncate / Skip / Resume / Overshoot / Mismatch / fresh transfer); `tests/property_resume.rs` (6 proptest invariants over the pure decision matrix); 4 net-new NDJSON parser tests; 2 net-new dispatcher tests; 5 net-new domain tests on `TransferEntity::with_resumed_from`; 11 unit tests on `decide_*_plan` decision matrix; 10 unit tests on the verify-mode helpers.

### Changed

- Total error-taxonomy size grows from 38 to 40 codes.
- `TransferEntity` deserialisation accepts JSON without a `resumed_from` field (legacy snapshots) — the new field is `#[serde(default)]`.
- Daemon NDJSON `upload` / `download` ops gain `resume?: bool` and `verify?: bool` (omitted by v6.0 callers).

### Migration

See [docs/MIGRATION.md → v6.0 → v6.1](docs/MIGRATION.md#v60--v61) for the full migration guide. Hosts that never opt into the new flags do not need to update code. New flags are documented in [docs/API.md → ssh_upload v6.1](docs/API.md#v61--adr-0010--resume--verify), [docs/LLM_GUIDE.md → Resuming a failed transfer](docs/LLM_GUIDE.md#v61--adr-0010--resuming-a-failed-transfer), and [docs/RESOURCES.md → v6.1](docs/RESOURCES.md#v61--adr-0010--resumed-transfers-ramp-from-resumed_from). No new env vars — see [docs/CONFIGURATION.md → v6.1 / ADR 0010](docs/CONFIGURATION.md#v61--adr-0010--resume--verify-no-new-env-vars).

## [6.0.0] — 2026-05-04

Tool-name namespace cleanup. **First wire-breaking release since v5.0** — only `tools/list` name strings change. Every other surface (resource URI schemes, push narrative, error taxonomy, structured-content payloads, env vars, MSRV, dependency graph) is byte-identical to v5.3.2.

### Why

The v5.x catalogue grew organically: SSH ops, lane administration, and local serial all sat under the `ssh_*` prefix even though `sub_*` is cross-resource (works for `serial://` too) and `serial_*` doesn't run over SSH at all. Smaller LLMs got confused — they'd attempt `serial_open` over an SSH session_id, or treat `ssh_subscribe` as SSH-specific and skip subscribing to a serial port. v6.0 splits the catalogue across three semantic eixos so the tool name itself encodes the transport.

### Renames (15 tools)

**`ssh_*` (21 tools)** — operations that travel over SSH:

| v5.x | v6.0.0 | Reason |
|---|---|---|
| `ssh_connect` / `ssh_disconnect` / `ssh_disconnect_agent` / `ssh_disconnect_many` / `ssh_run` | _(unchanged)_ | session lifecycle |
| `ssh_list_sessions` | `ssh_sessions` | drop `list_` redundancy |
| `ssh_execute` | `ssh_exec` | -3 chars per ref |
| `ssh_execute_batch` | `ssh_exec_batch` | follows `exec_*` group |
| `ssh_get_command_output` | `ssh_exec_output` | drop `get_`, group under `exec_*` |
| `ssh_cancel_command` | `ssh_exec_cancel` | follows `exec_*` group |
| `ssh_list_commands` | `ssh_commands` | drop `list_` |
| `ssh_shell_open` / `_write` / `_read` / `_close` / `_wait_for` | _(unchanged)_ | clear group |
| `ssh_shell_send_key` | `ssh_shell_press` | drop `send_`, mirrors `serial_press` |
| `ssh_upload` / `ssh_download` / `ssh_forward` | _(unchanged)_ | clear |
| `ssh_get_transfer_progress` | `ssh_transfer_progress` | drop `get_` |

**`sub_*` (9 tools)** — subscription / lane management (cross-resource — works for shell / command / transfer / session / forward / serial alike):

| v5.x | v6.0.0 |
|---|---|
| `ssh_subscribe` | `sub_open` |
| `ssh_unsubscribe` | `sub_close` |
| `ssh_sub_pause/resume/filter/replay/list/stats` | `sub_pause/resume/filter/replay/list/stats` |
| `ssh_daemon_stats` | `sub_stats_all` |

Verb-uniform: `open / close / pause / resume / filter / replay / list / stats / stats_all`. The `ssh_` prefix is dropped because subscription / lane administration is not SSH-specific (also serves `serial://` / `forward://` / `session://`).

**`serial_*` (6 tools)** — local UART / TTY / COM (no SSH involved):

| v5.x | v6.0.0 |
|---|---|
| `serial_open` | `serial_open` |
| `serial_close` | `serial_close` |
| `serial_write` | `serial_write` |
| `serial_press` | `serial_press` |
| `serial_scan` | `serial_scan` |
| `serial_active` | `serial_active` |

`serial_scan` (host-visible discovery) vs `serial_active` (per-process held) resolve the prior `list_ports` / `list_open` ambiguity.

### Unchanged (LLM UX preserved)

- **Resource URI schemes**: `shell://`, `command://`, `transfer://`, `session://`, `forward://`, `serial://`.
- **Push narrative**: `notifications/resources/updated` + `resources/read?cursor=auto` flow.
- **HINT line severities** + 38-code error taxonomy.
- **Tool descriptions / `When/Push/Cleanup/Cost/Idempotency/Hygiene` blocks**: cross-references updated to the new names everywhere (root `Implementation.instructions`, `NEXT:` lines, render bodies, prompt templates).
- **Lane fanout pipeline + production port-forward listener + lifecycle cascade close**: all v5.3 semantics carry forward unchanged.

### Migration

Hosts on v3 / v4 / v5 update with the sed snippet (full guide in [docs/MIGRATION.md → v5 → v6](docs/MIGRATION.md#v5--v6)):

```sed
s/\bssh_unsubscribe\b/sub_close/g          # do BEFORE ssh_subscribe — substring trap
s/\bssh_subscribe\b/sub_open/g
s/\bssh_sub_/sub_/g
s/\bssh_daemon_stats\b/sub_stats_all/g
s/\bssh_serial_send_key\b/serial_press/g
s/\bssh_serial_list_ports\b/serial_scan/g
s/\bssh_serial_list_open\b/serial_active/g
s/\bssh_serial_/serial_/g
s/\bssh_execute_batch\b/ssh_exec_batch/g   # do BEFORE ssh_execute
s/\bssh_execute\b/ssh_exec/g
s/\bssh_get_command_output\b/ssh_exec_output/g
s/\bssh_cancel_command\b/ssh_exec_cancel/g
s/\bssh_list_commands\b/ssh_commands/g
s/\bssh_list_sessions\b/ssh_sessions/g
s/\bssh_get_transfer_progress\b/ssh_transfer_progress/g
s/\bssh_shell_send_key\b/ssh_shell_press/g
```

### Quality

- 1657 lib + 41 chaos + 32 property + 2 v5_smoke + 13 integration tests all green.
- Production clippy strict gate (`cargo clippy --release --all-features --bin ssh-mcp-stdio -- -D warnings`) exit 0.
- Schema validation: every one of the 36 MCP tools satisfies `inputSchema.type == "object"`.

### Breaking changes

- **15 tool names renamed** (catalogue above). Hosts must update their `tools/list` parser or apply the sed migration.
- **No-op rename** for all 21 carried-forward `ssh_*` tools — wire shape and structured content stay byte-identical.

## [5.3.2] — 2026-05-04

Hotfix on top of 5.3.1 — lane cascade on session / agent disconnect. Wire-compatible drop-in.

### Fixed

- **`sub_open` lanes cascade-close on disconnect.** Stress test surfaced that `sub_open`-created lanes outlived their parent session even after `force_close` fired on the underlying resource. Per ADR 0004, lane lifetime is intentionally independent of session lifecycle (lifetime variants `auto_close` / `lease` exist for that), but the manual-lifetime case left orphan lanes consuming `lanes_total` and leaking the `SubId` registry. `DisconnectSessionUseCase` and `DisconnectAgentUseCase` now hold an optional `LaneAdmin` handle and call `lane_admin.close(sub_id)` for every lane bound to the resource id of each child being torn down — same pass that fires `lifecycle.force_close`.

After the fix: `lanes_total = 0` immediately after `ssh_disconnect` / `ssh_disconnect_agent`. Manual subscribers can still call `sub_close` explicitly while the session is alive — only orphan lanes (resource gone) are reaped.

### Quality

- 1657 lib + 41 chaos + 32 property + 2 v5_smoke + 13 integration tests all green.
- Production clippy strict gate exit 0.

## [5.3.1] — 2026-05-04

Hotfix on top of 5.3.0 — lifecycle cascade on session / agent disconnect. Wire-compatible drop-in.

### Fixed

- **`DisconnectSessionUseCase` cascade-closes child resource lifecycle entries.** Stress test surfaced that `ssh_disconnect` removed the session entity but left the per-resource lifecycle entries (commands / shells / transfers) in `Owned` state, so the `SUB_LEAK_RISK` watcher kept warning until the resource TTL fired. Use case now optionally holds a `LifecyclePolicyPort` and calls `force_close(kind, id)` on every child before the repo removal — `active_refs` decrements deterministically.
- **`DisconnectAgentUseCase` cascade-closes too.** Sibling fix — bulk-close path (`ssh_disconnect_agent`) had its own `cleanup_session` flow that did not observe the lifecycle adapter either. Same `with_lifecycle` constructor + `force_close` calls in `cancel_commands` / `close_shells` / `abort_transfers`.

After the fix: `SUB_LEAK_RISK` warnings clear immediately on `ssh_disconnect` / `ssh_disconnect_agent` — no TTL wait, no timer, no timeout.

### Quality

- 1657 lib tests + 41 chaos + 32 property + 2 v5_smoke + 13 integration all green.
- Production clippy strict gate exit 0.

## [5.3.0] — 2026-05-04

Lane fanout pipeline + production port-forward listener. Wire-compatible drop-in for any v3 / v4 / v5.0.x / v5.1 / v5.2 host: tools, structured_content schema, env vars, and error taxonomy unchanged for non-subscribe / non-forward workflows.

### What landed

- **`sub_open` push delivery wired end-to-end on stdio/HTTP transport.** Lanes carry the rmcp peer captured at tool-invocation time (`SubscribeRequest.peer: Option<Arc<dyn PeerHandle>>`); the new `LaneFanoutBridge` (`src/adapters/subscription/lane_bridge.rs`) walks the lane snapshot for the broadcast URI, calls `notifier.notify_resource_updated` per peer, and increments per-lane atomics (`events_sent`, `bytes_sent`). Phase 4 NDJSON daemon keeps the channel-mux outbound sink (lane peer = `None`).
- **3 new ports** (`src/ports/notifier.rs`):
  - `LaneNotifierBridge` — boxed-future, dyn-safe trait the `MemoryRegistry::broadcast` consults before the legacy peer fan-out.
  - `DebouncerActivator` — lets `sub_open`-only flows wake the per-resource debouncer the legacy `resources/subscribe` path uses.
  - `ProducerForwarder` — runtime adapters (russh / sftp / serial / shell PTY) feed the legacy `SUBSCRIPTION_REGISTRY` singleton; the forwarder mirrors every `poke` / `record_bytes` into the hexagonal `MemoryRegistry`.
- **Memory-registry hardening** (`src/adapters/subscription/memory_registry.rs`):
  - `record_bytes` lazy-inserts the per-resource counter so producers writing pre-subscribe do not lose accumulation.
  - `ensure_debouncer` reuses the existing counter (instead of overwriting) so spawn does not lose pre-subscribe bytes.
  - `lane_bridge: ArcSwap<Option<Arc<dyn LaneNotifierBridge>>>` field + `install_lane_bridge` setter; `broadcast(uri, bytes_added)` now passes the swapped byte counter to the bridge.
- **Production port-forward listener.** `ssh_forward` no longer drops the listener after preflight: `SshClientPort::open_forward` / `close_forward` (returning a `ForwardHandle { bound_addr }`) drive a real accept loop. Per-connection: `Handle::channel_open_direct_tcpip(remote_addr, remote_port, originator, 0)` + `tokio::io::copy_bidirectional`. Sessions cascade-close their forwards on disconnect — no port leaks, no zombie listeners.
- **Shell PTY producer wired.** `flush_shell_buffer` now calls `SUBSCRIPTION_REGISTRY.poke(Shell, id)` + `record_bytes(Shell, id, chunk_len)` after every flush, so `shell://<id>/output` lanes see real PTY bytes (not just the 1 s force-flush keepalive tick).
- **URI charset hardening.** `parse_uri` rejects ids outside `[A-Za-z0-9_-]+` (whitespace, unicode, control chars, dot-dot) with a new `ParseError::BadIdCharset`. UUIDv7 + legacy snake_case ids unaffected.
- **`serial://` scheme** is now first-class on the `sub_open` URI parser (the legacy URI parser already supported it; the application-side parser was the only gap).
- **MCP schema spec compliance.** `SerialListPortsArgs` / `SerialListOpenArgs` no longer derive `{"type":"null"}` (unit-struct schemars output). Empty named-fields structs derive the spec-mandated `{"type":"object","properties":{},"additionalProperties":false}` so strict MCP hosts (Claude Code) accept the full `tools/list` batch.

### Quality

- **1657 lib tests + 41 chaos + 32 property + 2 v5_smoke + 13 integration** all green.
- Production clippy strict gate (`cargo clippy --release --all-features --bin ssh-mcp-stdio -- -D warnings`) exit 0.
- Schema validation: every one of the 36 MCP tools satisfies `inputSchema.type == "object"`.
- Stress-tested against `vm.services`: 50 MB stdout through subscribe, 1 MB SFTP upload, 6 schemes, 17-sub-per-URI cap fired, 200-char URI accepted, unicode / spaces / control-chars rejected, server stayed responsive across 15 269 push events. No panics, no buffer overflows, no listener leaks.

### Breaking changes

None. Wire-compatible drop-in.

## [5.2.0] — 2026-05-04

ADR 0009 — native serial / UART / TTY / COM transport. Adds a sixth push-resource scheme `serial://<id>/output` that plugs into the existing v4 `SUBSCRIPTION_REGISTRY` debouncer pipeline (debounce + force_flush + keepalive + ADR 0006 Amendment 1 byte-threshold flush). Wire-compatible drop-in for any v3 / v4 / v5.0.x / v5.1 host: tools, structured_content schema, env vars, and error taxonomy unchanged for non-serial workflows.

### What landed

- **New `tokio-serial = "5"` dependency** (default features only; no extra `serde` / `async-std` shims).
- **New `ResourceKind::Serial` variant** on the port enum (`src/ports/subscriber_registry.rs`) and the legacy adapter enum (`src/adapters/subscription/legacy.rs`); `format_uri` / `parse_uri` round-trip `serial://<id>/output` on both. Every exhaustive match across the workspace updated atomically.
- **Domain types** (`src/domain/ids.rs::SerialId`, `src/domain/error.rs::DomainError::SerialNotFound` + `Serial(String)`).
- **Lock-free per-port aggregate** (`src/adapters/serial/state.rs`):
  - `SerialPortState` — `ArcSwap<RingBuffer>` history (subscribers slice without locking the OS reader), `mpsc::channel(64)` write queue (slow remote sink fills the queue first; subsequent writes return `SERIAL_BACKPRESSURE` instead of stalling), `CancellationToken` cooperative shutdown, `AtomicU64` activity / max-buffer atomics, `Notify` data hint.
  - `SerialConfig` — full `stty` parameter coverage: `path`, `baud_rate`, `data_bits` (5..=8), `stop_bits` (1 / 2), `parity` (none / odd / even), `flow_control` (none / software / hardware), `read_timeout`, `max_buffer_size`, `initial_dtr`, `initial_rts`, `label`.
  - `open_port`, `close_port` (idempotent), `write_port` (returns `SerialWriteError::Backpressure` when the mpsc fills), `available_ports`, `read_history_from_cursor`.
  - Reader and writer tasks split via `tokio::io::split` over the `tokio_serial::SerialStream`. Reader appends to history via `ArcSwap::rcu` + calls `SUBSCRIPTION_REGISTRY.next_seq` / `poke` / `record_bytes` (ADR 0006 Amendment 1 byte-threshold) — bit-identical to the `command://*/output` producer wiring.
- **6 new MCP tools** (also documented in [docs/API.md](docs/API.md)):
  - `serial_open` — open a port, return `SERIAL_ID` + `serial://<id>/output` URI.
  - `serial_close` — idempotent cancel.
  - `serial_write` — UTF-8 `text` OR base64 `bytes_base64` (RFC 4648 standard alphabet, inline decoder).
  - `serial_press` — named keystrokes: `enter`/`cr` (`\r`), `lf` (`\n`), `crlf` (`\r\n`), `esc` (`\x1b`), `tab` (`\t`), `backspace` (`\x08`), `ctrl_c` / `ctrl_d` / `ctrl_z`. Optional `repeat` 1..=64.
  - `serial_scan` — snapshot OS-visible serial devices.
  - `serial_active` — snapshot ports currently held by this process.
- **Resource-handler dispatch** — `try_serial_read` short-circuits `resources/read serial://<id>/output` directly against `SERIAL_REGISTRY` (the application read use case bypasses serial; serial state lives outside the application repositories, mirroring the forward-resource pattern).
- **LLM-UX hooks** — `serial_open` `HINT: RECOMMENDED:` line steers the model to subscribe (debounce + 64 KiB byte-threshold flush, same pipeline as shell / command). `serial_write` / `serial_press` `HINT:` line tells the model to wait for `notifications/resources/updated` and drain via `resources/read?cursor=auto`.
- **ADR 0009** — full design narrative, hexagonal layer map, lock-free contract, configuration surface, MCP tool table, cross-platform device-path notes, deferred-to-v5.3 list.
- **`docs/RESOURCES.md`** — sixth push-resource scheme documented.
- **`docs/CONFIGURATION.md`** — no new env vars (serial inherits `SSH_NOTIFY_*` knobs).
- **`docs/API.md`** — 6 new tool entries + Tool catalogue tally bumped to 36 (35 without `port_forward`).
- **README** — version bump, comparison table, "What's new in 5.2.0" section.

### Tool count

| Build | Tools | Schemes |
|---|---|---|
| default (`port_forward = on`) | **36** (was 30) | `shell`, `command`, `transfer`, `forward`, `session`, **`serial`** |
| `--no-default-features` | **35** (was 29) | `shell`, `command`, `transfer`, `session`, **`serial`** |

### Out of scope (deferred to v5.3)

- **Per-call `flush_bytes` / `debounce` overrides on serial subscriptions** — same follow-up that gates the equivalent feature on `sub_open` for shell / command.
- **Hot reconfigure** (change baud / parity without close-reopen).
- **Hardware breaks** (`tcsendbreak` via `ssh_serial_send_break`).
- **Modem control mid-session** (toggle DTR / RTS at runtime — currently only `initial_dtr` / `initial_rts` on open).
- **Lifecycle binding for serial ports** (`release_when_no_subs=true` mirroring ADR 0003 for shells / commands).

### Verified gates

- `cargo build --release` exit 0 (3 binaries: `ssh-mcp`, `ssh-mcp-stdio`, `ssh-mcp-tail`).
- `cargo fmt --all -- --check` exit 0.
- `cargo clippy --release --all-features -- -D warnings` exit 0.
- `cargo test --lib --quiet` — **1655 passed** (was 1649 in 5.1.0; +6 serial tests covering config defaults, parser round-trips, registry sanity).

## [5.1.0] — 2026-05-04

ADR 0006 Amendment 1 — byte-threshold debouncer flush is now wired end-to-end. Adds a fourth wakeup source on the per-resource debouncer (`flush_now`) plus per-resource byte counters that live alongside the existing time-based pipeline. No wire-format breakage; tool catalogue, structured_content schema, and error taxonomy unchanged.

### What changed

- **New env var `SSH_NOTIFY_FLUSH_BYTES`** — default `65_536` (64 KiB). Accepts bare integer bytes (`65536`) or bytesize strings (`8k`, `64k`, `1m`, `1mib`, `2mb`). `0` disables byte-threshold (debouncer reverts to v5.0 time-only behaviour). Clamps to `[1024, 1_048_576]` for non-zero values; unknown suffixes fall back to default.
- **Port `SubscriberRegistryPort::record_bytes(kind, resource_id, bytes_added)`** — new method with default no-op so test fixtures and the legacy adapter compile unchanged.
- **`MemoryRegistry`** (hexagonal v5 lane) — new `flush_now: DashMap<_, Arc<Notify>>`, `bytes_since_flush: DashMap<_, Arc<AtomicUsize>>`, cached `flush_bytes_threshold`, and process-wide `byte_triggered_flushes: AtomicU64`. `record_bytes` increments the per-resource counter (`Relaxed`) and fires `flush_now.notify_one()` exactly once per crossing.
- **`SubscriptionRegistry`** (legacy v4 adapter) — same fields and semantics. Both registries flush on whichever wakes first: debounce window expiry, force-flush tick, keepalive tick, or byte-threshold cross.
- **Producer hooks** — `record_bytes` now called on every chunk write at three production sites:
  - `src/adapters/ssh/internal/client.rs::publish_stdout` and `publish_stderr` for `command://*/output`.
  - `src/adapters/sftp/internal/sftp.rs::upload_inner_loop` and `download_inner_loop` for `transfer://*/progress` (per-chunk SFTP delta).
- **Stats** — `SubscriberStats.byte_triggered_flushes` field (serde `default = 0` for v5.0/v5.0.x compat). v5.1 populates the resource-level total via `MemoryRegistry::byte_triggered_flushes_total()` / `SubscriptionRegistry::byte_triggered_flushes_total()`. Per-lane attribution is reserved for v5.2 (see "Out of scope" below).
- **`sub_open` HINT line** — wire HINT now mentions both knobs explicitly: "Push fires on whichever fires first: ~50ms debounce window (SSH_NOTIFY_DEBOUNCE_MS) OR 64KiB accumulated bytes (SSH_NOTIFY_FLUSH_BYTES). 10-100ms local sleeps cover both budgets."
- **Local-sleep guidance** — same HINT line steers the LLM to wait passively for `notifications/resources/updated` and use Unix `sleep 0.05` / PowerShell `Start-Sleep -Milliseconds 50` for any local idling, never an MCP tool as a sleep primitive.
- **`docs/CONFIGURATION.md`** — env-var reference table updated with the new knob and its semantics.

### Out of scope (deferred to v5.2)

- **Per-call `flush_bytes` / `debounce` overrides on `sub_open`** — ADR 0006 Amendment 1 documents the schema (oneOf integer | string) and first-subscriber-wins semantics, but implementing the per-resource override + `effective_*` stats requires touching the schema, validation, and shared-debouncer reconfiguration. v5.1 wires the env-var-only path; v5.2 layers the per-call overrides on top.
- **Per-lane `byte_triggered_flushes` attribution** — v5.1 exposes the resource-level total; bridging the count into `LaneAtomics` so each `sub_stats` snapshot reflects which lanes saw fan-out increments lands alongside the per-call override work in v5.2.
- **Shell `shell://*/output` byte-threshold** — shell production uses the per-shell `output_tx` broadcast + `data_notify` rather than `SUBSCRIPTION_REGISTRY.next_seq` / `poke`. Wiring the byte-threshold into the shell flush path requires piping `shell_id` into `flush_shell_buffer`; deferred to v5.2.

### Verified gates

- `cargo build --release` exit 0 (3 binaries: `ssh-mcp`, `ssh-mcp-stdio`, `ssh-mcp-tail`).
- `cargo fmt --all -- --check` exit 0.
- `cargo clippy --release --all-features -- -D warnings` exit 0.
- `cargo test --lib --quiet` — **1649 passed** (was 1636 in 5.0.2; +13 new tests covering the bytesize parser, env-var resolver clamps, `0` disable path, `record_bytes` no-op when no debouncer, sub-threshold no-fire, threshold-boundary fire, and stats getter).

## [5.0.2] — 2026-05-04

LLM UX patch — penalize `ssh_run`, steer shell drive ops to await push, and propose ADR 0006 Amendment 1 (byte-threshold debouncer flush). No wire-format breakage; tool catalogue, structured_content schema, env vars, and error taxonomy unchanged.

### Penalize `ssh_run` (promote session reuse)

- `ssh_run` description rewritten as `PENALIZED FALLBACK`. Spells out the per-call handshake cost (200–2000 ms + full session teardown) and the `DO NOT USE WHEN` matrix (any reuse possible, follow-up commands, output > a few KiB). Gives the preferred `ssh_connect(reuse=auto) → ssh_exec → sub_open` path step-by-step.
- `ssh_exec` and `ssh_exec_batch` overlap guidance no longer steers atomic short commands toward `ssh_run`; both redirect to `ssh_connect(reuse=auto)` + `ssh_exec`.
- Root `Implementation.instructions` (both `port_forward` feature gates) demote `ssh_run` from happy-path #1 to penalized fallback #4. Push-first connect+execute+subscribe path becomes the default.
- `docs/API.md` instructions snapshot synced.
- `docs/LLM_GUIDE.md` updates: terse + 70B prompts, decision flowchart (red-classed P5 `ssh_run` node), decision table split into may-revisit vs single-touch rows, FALLBACK PATHS prose all reordered.

### Steer shell drive ops to await push (no `wait_for` chaining)

- After `ssh_shell_write` / `ssh_shell_press`, the response arrives via push on the existing `shell://<id>/output` subscription. New `HINT: RECOMMENDED:` line on every drive response tells the LLM to wait on `notifications/resources/updated` and drain via `resources/read?cursor=auto`, with an explicit `Do NOT call ssh_shell_wait_for unless you need to gate on a specific regex`.
- `next_hint_for_shell_drive` reorders to lead with `sub_open (await push if not subscribed)` and `resources/read?cursor=auto (drain delta on push)`. `ssh_shell_wait_for` appears later qualified `(only for regex prompt sync)`. Polling reads stay last.
- `next_hint_for_wait_for` leads with `sub_open` in BOTH `Matched` and `Timeout` states so the LLM stops chaining `wait_for` recursively. `Closed` remains terminal.
- Tool descriptions: `ssh_shell_write`, `ssh_shell_press` get the `DO NOT chain ssh_shell_wait_for` block. `ssh_shell_wait_for` rewritten to lead with `PREFER sub_open`, lists `DO NOT USE WHEN` cases (post-write chaining, generic 'any output', repeated matches), and gives the preferred subscribe + push + drain path step-by-step.

### Proposed (no code yet)

- ADR 0006 Amendment 1 — byte-threshold debouncer flush. Adds a fourth wakeup source on the per-resource debouncer: flush whichever fires first between debounce window, force-flush tick, keepalive tick, or accumulated bytes since last broadcast. Bumps debouncer defaults to **`200ms` coalesce** + **`64k` byte-threshold**. Switches env-var interface from raw `*_MS` / `*_BYTES` integers to human-readable `Duration` (`200ms`, `1s`) and `ByteSize` (`64k`, `1m`) strings; legacy aliases stay accepted for one minor with a `DEPRECATED:` log nudge. Adds optional per-call `flush_bytes` / `debounce` overrides on `sub_open` (oneOf `integer | string`), with precedence `tool > env > default` and first-subscriber-wins on the shared debouncer. See [ADR 0006 → Amendment 1](docs/adr/0006-backpressure-policies.md#status).

### Verified gates

- `cargo build --release` exit 0 (3 binaries).
- `cargo test --lib --quiet` — **1636 passed** (was 1633 in 5.0.1; +3 new render assertions covering the new HINT line, the subscribe-first NEXT line on send_key, and subscribe-first NEXT in both Matched/Timeout outcomes of `wait_for`).

## [5.0.1] — 2026-05-04

LLM UX patch — closes the v5 narrative arc so every push-capable resource is `sub_open`-first across tool descriptions, NEXT chains, HINT lines, and discovery surfaces. No wire-format breakage; tool catalogue, structured_content schema, env vars, and error taxonomy unchanged.

### Bidirectional-first narrative closure

- `ssh_exec`, `ssh_exec_output`, `ssh_shell_read`, `ssh_shell_wait_for`, `ssh_transfer_progress` descriptions now lead with `sub_open` as the preferred path; polling tools labelled `(poll fallback)` consistently across both server variants (port_forward on/off).
- `next_hint_for_shell_drive` (post-write/send_key): `sub_open` listed FIRST so drive ops honour the subscribe contract opened by `ssh_shell_open`. Cursor poll and `ssh_shell_read` demoted to explicit `(poll fallback)` markers.
- `next_hint_for_running_command` and `next_hint_for_in_flight_transfer`: explicit `(preferred)` and `(poll fallback)` labels — eliminates the self-loop where a polling tool would re-recommend itself.
- `next_hint_for_wait_for` (Timeout branch): subscribe FIRST, poll fallback last.
- `next_hint_for_session` (post-connect): trailing reminder `(each spawn returns an ID that sub_open streams as push)` so the LLM learns the bidirectional contract on call #1; adds `ssh_upload` to the spawn list for completeness.
- `next_hint_for_list_sessions`: `sub_open session://<id>/health` now leads, with `ssh_disconnect_agent` / `ssh_disconnect` as cleanup fallbacks. Discovery flows terminate in a push lane, not in teardown.
- `list_commands_render_with_warnings`: surfaces `sub_open command://<id>/output` for the first RUNNING entry — discovery → push, not discovery → poll.

### Overlap surfacing (zero-cookbook tool selection)

- `ssh_run` description: when-each guidance vs `ssh_connect` + `ssh_exec` (>1 command) and vs `ssh_exec_batch` (>=3 sequential).
- `ssh_exec_batch` description: cites ~70% wire savings over `N x ssh_exec` for `N >= 3`; cross-references `ssh_run` (single) and parallel `ssh_exec` patterns.
- `ssh_disconnect_agent` vs `ssh_disconnect_many`: when-each matrix surfaced in both descriptions so the LLM picks the cheapest bulk-cleanup call without round-trips.
- `ssh_shell_wait_for` description: surfaces overlap with `sub_open` + `filter` for streaming pattern matches; 1-shot pattern gating remains the local sweet spot.

### Verified gates

- `cargo build --release` exit 0 (3 binaries).
- `cargo clippy --release --all-features -- -D warnings` exit 0.
- `cargo test --lib --quiet` — **1633 passed**.
- macOS codesign with Apple Development certificate (TeamIdentifier `MYT54AW7PD`, hardened runtime); `codesign --verify --verbose=2` exit 0 on all three installed binaries.

## [5.0.0] — 2026-05-04

Stable release. All 7 phases merged on `master` plus 3 follow-up hook closures (idempotency args fingerprint, `MAX_SUBS_PER_URI` / `MAX_SUBS_TOTAL` enforcement, UUIDv7 across every id type). Wire-compatible with every v3 / v4 host on the legacy 21-tool catalogue. Host migration guide: [docs/MIGRATION.md → v4 → v5](docs/MIGRATION.md#v4--v5). Design narrative: ADRs at [docs/adr/0001..0008.md](docs/adr/).

### Verified gates at release

- `cargo build --release` exit 0 (3 binaries: `ssh-mcp`, `ssh-mcp-stdio`, `ssh-mcp-tail`).
- `cargo fmt --all -- --check` exit 0.
- `cargo clippy --release --all-features -- -D warnings` exit 0.
- `cargo test --lib --quiet` — **1633 passed**.
- `cargo test --tests --features test-fixtures --quiet` — **88 passed** (41 chaos + 32 property + 8 v5_smoke + 5 v5_daemon_smoke + 2 v4_smoke).
- Python adversarial + chaos suites — **86 passed**.

## [5.0.0-rc1] — 2026-05-04 (superseded by 5.0.0)

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    P1["Phase 1<br/>Lifecycle binding<br/>ADR 0003"]
    P2["Phase 2<br/>Channel mux + sub_id<br/>ADR 0004"]
    P3["Phase 3<br/>LLM UX overhaul<br/>ADR 0005 / 0007"]
    P4["Phase 4<br/>NDJSON daemon<br/>ADR 0008"]

    P1 --> P2 --> P3 --> P4

    style P1 fill:#238636,color:#f0f6fc,stroke:#2ea043
    style P2 fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style P3 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style P4 fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
```

### Highlights

- **Subscribe-first architecture** ([ADR 0003](docs/adr/0003-lifecycle-binding.md)). `ResourceLifecycle` CAS state machine (`Owned → Observed → Releasing → Closed`) + per-session cascade refcount; grace timer arms when last subscriber detaches; new subscribes within the window cancel it; `release_when_no_subs = true` on `ssh_shell_open` / `ssh_exec` / `ssh_upload` / `ssh_download` opts in. Default preserves v4 semantics.
- **Channel mux + sub_id** ([ADR 0004](docs/adr/0004-channel-mux-fairness.md)). Cursor key shifts from `(PeerId, Uri)` to `(SubId, Uri)` (UUIDv7); each subscribe call gets its own bounded `mpsc::channel(N)`, `LagPolicy` (default `Snapshot`), filter pipeline, replay window, and `SubscriberStats` (8 atomics). `ChannelMux` round-robin drainer guarantees fair scheduling. Legacy hosts get a synthesised `sub_id`.
- **LLM UX overhaul** (Phase 3 — in flight; [ADR 0005](docs/adr/0005-llm-ux-priorities.md), [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md)). 9 net-new MCP tools (`sub_open`, `sub_close`, `sub_pause`, `sub_resume`, `sub_filter`, `sub_replay`, `sub_list`, `sub_stats`, `sub_stats_all`); `HINT:` severity escalation (REQUIRED / RECOMMENDED / informational); 10-prompt catalog (5 carry-overs + 5 push-first); `SUB_LEAK_RISK` watcher (default `SSH_SUB_LEAK_RISK_WARN_S = 2 s`).
- **NDJSON daemon binary** (Phase 4 — in flight; [ADR 0008](docs/adr/0008-ndjson-daemon-protocol.md), [docs/DAEMON.md](docs/DAEMON.md)). `ssh-mcp-tail` with three subcommands (`run`, `shell`, `daemon`). Embeds in-process rmcp client + server across `tokio::io::duplex`; stdin NDJSON ops, stdout NDJSON events. Single binary, no IPC, Unix-pipeline composable. For tier-2 / tier-3 hosts (Claude Desktop, Claude Code CLI, IDE integrations) that do not surface `notifications/resources/updated` to the model.
- **38-code error taxonomy** ([ADR 0007](docs/adr/0007-error-taxonomy.md), [docs/LLM_GUIDE.md → Error handbook](docs/LLM_GUIDE.md#error-handbook)). 7 categories (`AUTH`, `TRANSPORT`, `REMOTE`, `RESOURCE`, `POLICY`, `STATE`, `INTERNAL`) with explicit retry semantics; centralised one-sentence DETAIL phrasing.
- **Lock-free invariants extended** ([docs/DEVELOPMENT.md → Lock-free invariants](docs/DEVELOPMENT.md#lock-free-invariants)). New atomics for lifecycle (`AtomicU8` state, `AtomicUsize` sub_count, `AtomicU64` grace_until_ms, `ArcSwap<LifecyclePolicy>`, `Notify` waker) and lane / mux (per-lane mpsc, cursor, pause flag, 8 stats atomics, mux_waker). Loom coverage 8 → 16 (Phase 1 + Phase 2 each add 4). Production clippy gate stays exit-0.
- **MSRV bumped to Rust 1.95** (Rust 2024 edition baseline + AFIT).

### Added

- **Domain**: `src/domain/lifecycle.rs` (`LifecycleState`, `LifecyclePolicy`, `LifecycleSnapshot`, `SessionPolicy`, `DEFAULT_GRACE_MS`); `src/domain/subscription.rs` (`SubId` UUIDv7 wrapper, `LagPolicy`, `LogLevel`, `FilterRule`, `SubscriptionLifetime`, `SubscriberStats`).
- **Ports**: `src/ports/{lifecycle_policy,channel_mux,subscriber_lane}.rs` — each with sync + async slice; `subscriber_lane.rs` also ships a `LaneAdmin` dyn-safe shim.
- **Adapters**: `src/adapters/lifecycle/{mod,refcount,grace_timer,cascade}.rs` (Phase 1); `src/adapters/subscription/{subscriber_lane,channel_mux,filter,replay}.rs` (Phase 2).
- **Domain errors**: `ResourceGone`, `LifecycleStateConflict`, `SessionRefcountUnderflow`, `SubNotFound`, `SubMaxPerUriExceeded`, `SubMaxTotalExceeded`, `LaneBufferFull`, `LagBackpressure`, `RingBufferOverflow`, `MuxBackpressure`, `GraceTimerExpired`.
- **Env vars** (defaults preserve v4 behaviour) — full table in [docs/CONFIGURATION.md](docs/CONFIGURATION.md). Highlights: `SSH_LAG_POLICY_DEFAULT=snapshot`, `SSH_LANE_BUFFER=1024`, `SSH_MUX_BUFFER=8192`, `SSH_BP_BLOCK_TIMEOUT_MS=5000`, `SSH_MAX_SUBS_PER_URI=16`, `SSH_MAX_SUBS_TOTAL=1024`, `SSH_FILTER_REGEX_MAX=1024`, `SSH_REPLAY_WINDOW_BYTES=1m`, `SSH_SUB_LEAK_RISK_WARN_S=2`, `SSH_SUB_LEAK_RISK_KILL_S=0`, `SSH_NDJSON_LINE_MAX=1m`, `SSH_HEARTBEAT_INTERVAL_S=30`, `SSH_DAEMON_STATS_INTERVAL_S=60`, `SSH_GRACE_HARD_TIMEOUT_S=30`, `SSH_NDJSON_PRETTY=false`. Lifecycle grace windows are programmatic per-call (`LifecyclePolicy.grace_ms`) — no env-var override in v5.0.
- **ADRs**: 0003 (lifecycle binding), 0004 (channel mux fairness), 0005 (LLM UX priorities), 0006 (backpressure policies), 0007 (error taxonomy), 0008 (NDJSON daemon protocol).
- **Docs**: `docs/MIGRATION.md` (consolidated v2 → v3, v3 → v4, v4 → v5), `docs/DAEMON.md` (renamed from `INSTRUCTIONS_DAEMON.md`), `docs/OPERATIONS.md` (consolidates `TROUBLESHOOTING.md` + `ERRORS.md` + recovery flows from `FLOWS.md`), `docs/DEVELOPMENT.md` (consolidates `LOCKS.md` + hot-path flows + dev gates), `docs/LLM_GUIDE.md` (single canonical LLM doc — absorbs all of `docs/llm-ux/` + the prior `LLM_GUIDE.md`).
- **Loom invariants**: 8 new interleavings in `tests/lockfree_invariants.rs` (Phase 1 and Phase 2 each add 4). Total: 16.

### Changed

- **Cursor key**: `(PeerId, Uri)` → `(SubId, Uri)` internally; legacy hosts get a synthesised `sub_id` per `(PeerId, Uri)` pair. Cursor advance, gap detection, and replay all key on `SubId` going forward.
- **Default lane LagPolicy**: implicit broadcast `RecvError::Lagged` recovery (v4) → per-lane `Snapshot` (drop backlog + ring-buffer rebuild). Behaviour matches v4 semantics for slow subscribers; tunable via `SSH_LAG_POLICY_DEFAULT`.
- **Session reaper** now consults `SessionLifecycle.active_refs > 0` before honouring inactivity TTL — refcount supersedes TTL.
- **MSRV** 1.85 → 1.95.
- **Subscription registry** adds `(SubId, Uri)` cursor index alongside the existing `(PeerId, Uri)` index (commit `4ccbca3`); both coexist while the lane fan-out runs against the SubId index.

### Migrated

Zero breaking changes on the wire; every delta is a new optional argument or env var with a v4-preserving default. v3 / v4 hosts pointed at v5 servers see no behavioural change unless they opt in. Full host migration guide: [docs/MIGRATION.md → v4 → v5](docs/MIGRATION.md#v4--v5).

### Documentation

- **Reorganised `docs/`** from 28 files (8990 lines) to 10 top-level files + 8 ADRs. Files: `README.md` (directory index, new), `LLM_GUIDE.md` (canonical LLM reference — golden rules, 27B / 70B root prompts, prompts catalogue, anti-patterns, full 38-code error handbook — absorbs all of `docs/llm-ux/`), `OPERATIONS.md` (operator runbook — symptom → cure tree, per-tool error catalogue, recovery flows — merges `TROUBLESHOOTING.md` + `ERRORS.md` + parts of `FLOWS.md`), `DEVELOPMENT.md` (build / clippy gates, lock-free invariants, hot-path sequence diagrams — merges `LOCKS.md` + parts of `FLOWS.md`), `DAEMON.md` (renamed from `INSTRUCTIONS_DAEMON.md`), `MIGRATION.md` (consolidated v2 → v3, v3 → v4, v4 → v5 from three separate files), plus the kept `ARCHITECTURE.md` / `API.md` / `RESOURCES.md` / `CONFIGURATION.md`. The 8 ADRs at `docs/adr/` are unchanged. Every Mermaid diagram preserved; cross-links updated.

### Pre-existing flake noted

`env_config_returns_v3_defaults_with_unset_env` lib test occasionally trips when running `cargo test` in parallel (env-var contention with sibling tests). Single-process runs are stable. Tracked for resolution before rc1 tags.

## [4.8.1] — 2026-05-04

### Highlights

- **Fix: `ssh_transfer_progress` now reports live `bytes_transferred` during running uploads/downloads.** Prior behaviour: the snapshot always returned `bytes_transferred = 0` until the transfer reached terminal state, even though the file on disk was growing. Users had to poll local file size as a workaround. The fix is wire-compatible — the structured payload shape, env vars, and error codes are unchanged.

### Fixed

- **`ssh_transfer_progress` always returned `bytes_transferred = 0` mid-flight** (v4.8.1 BUG, reproducible on any non-instant upload or download). Root cause: the SFTP streaming task incremented the live `Arc<AtomicU64>` per chunk and emitted `ProgressEvent::Tick` on a `tokio::sync::broadcast`, but no listener pumped those ticks into the `TransferEntity` row in the repository — the row was born at `bytes_transferred = 0` (via `fresh_entity`) and only updated on the terminal frame by `spawn_status_watcher`. The use case `get_transfer_progress` reads from `TransferRepository::get(...)`, so it inherited the stale 0 throughout the running window. The same staleness affected `transfer://<id>/progress` resource reads (the broadcast notification fired correctly, but the subsequent `resources/read` returned the stale entity). Fix (`adapters/sftp/russh_sftp_adapter.rs`): a per-transfer progress watcher (`spawn_progress_watcher`) is spawned alongside the existing status watcher; it subscribes to the same broadcast and calls `TransferStatusSink::record_progress(...)` on each Tick, throttled by `PROGRESS_TICK_THROTTLE` (250 ms wall clock) to avoid hot-write per 32 KB chunk. The sink hook, the `RepoTransferStatusSink::record_progress` impl, the `NoopTransferStatusSink::record_progress` impl, and the `TransferEntity::with_progress(bytes)` projection were all already declared since v4.2 — the only missing piece was the producer task that consumed the broadcast. The progress watcher's close-flush guarantees the latest pending value lands before exit, and a terminal frame received before any Tick exits with zero `record_progress` calls so the existing terminal `mark_completed` write is never raced by a stale partial.

### Audit (sibling bug class)

The same shape (live counter incremented by a streaming task, never mirrored to the repo until terminal state) was checked across every subsystem with a streaming surface:

- `ssh_exec_output` — uses live `ArcSwap<OutputBuffer>` already (correct, not affected).
- `ssh_shell_read` — uses live `ArcSwap<RingBuffer>` already (correct, not affected).
- `forward://<id>/events` — N/A (no live byte counter).

Only the SFTP transfer path was affected; this fix is the only required change.

### Tests

- **4 new Rust unit tests** in `progress_watcher_tests` (in `src/adapters/sftp/russh_sftp_adapter.rs`): first-tick is forwarded immediately + close-flush emits last pending value; 50-tick burst is throttled to fewer writes with monotonic non-decreasing values; 250 ms quiet window releases the throttle gate; terminal-before-tick exits with zero `record_progress` calls.
- **2 new Python integration tests** in `scripts/test_transfer_progress.py` (`requires_sshd` mark, mirrors `test_v47_progress.py` / `test_stdio.py` conventions): `test_upload_progress_reports_live_partial_bytes` and `test_download_progress_reports_live_partial_bytes` upload/download a 5 MiB blob, poll `ssh_transfer_progress` at 100 ms cadence for up to 30 s, assert `0 < bytes_transferred < total_bytes` is observed at least once mid-flight (the exact pre-fix bug shape would fail this), assert monotonic non-decreasing `bytes_transferred` across polls, and assert terminal `bytes_transferred == total_bytes`.
- Lib tests: **1168 → 1172** (+4 progress watcher unit tests). Integration tests: 2 / 2 unchanged in `tests/`. Python integration tests: 2 new in `scripts/`.

### Compatibility

- Wire shape: byte-identical to v4.8.0 on the Markdown body and on the `structured_content` JSON payload. The fix is observable only in the *value* of `bytes_transferred` during running snapshots — it now reports real live bytes instead of `0`. No env vars added, no error codes added, no port / sink / repo trait surface changes (the `record_progress` hook was already declared since v4.2).
- v3 / v4.0 / v4.5 / v4.6 / v4.7 / v4.8 hosts work against v4.8.1 servers without any change.

## [4.8.0] — 2026-05-03

### Highlights

- **Full `output_schema` coverage on every MCP tool — 21 / 21.** v4.7 advertised typed `output_schema` on 9 tools (the 6 originals — `ssh_connect`, `ssh_exec`, `ssh_exec_output`, `ssh_shell_open`, `ssh_shell_read`, `ssh_transfer_progress` — plus the v4.7 step-3 additions `ssh_run`, `ssh_exec_batch`, `ssh_disconnect_many`). v4.8 lifts the remaining 12 tools (`ssh_disconnect`, `ssh_sessions`, `ssh_disconnect_agent`, `ssh_commands`, `ssh_exec_cancel`, `ssh_shell_write`, `ssh_shell_press`, `ssh_shell_wait_for`, `ssh_shell_close`, `ssh_upload`, `ssh_download`, `ssh_forward`) to typed schemas mirroring their `structured_content` payload byte-for-byte. Smaller LLMs (Haiku / Llama / Qwen 7B-30B) can now validate every tool response against a published schema without hard-coding any field names.
- **Strictly additive on the wire.** The text channel is byte-identical to v4.7.1; the `structured_content` payload shape is byte-identical to v4.7.1. Only `tools/list[].outputSchema` grows. v3 / v4.0 / v4.5 / v4.6 / v4.7 hosts work against v4.8 servers without any change.

### Added

- **12 new typed result structs** in `src/infra/mcp/results.rs`, each `#[derive(Debug, Clone, Serialize, JsonSchema)] #[non_exhaustive]` mirroring the runtime `structured_content` payload of its tool's success path:
  - `SshDisconnectResult`, `SshSessionsResult` (with `SessionEntry`), `SshDisconnectAgentResult`
  - `SshCommandsResult` (with `CommandEntry`), `SshExecCancelResult`
  - `SshShellWriteResult`, `SshShellPressResult`, `SshShellWaitForResult`, `SshShellCloseResult`
  - `SshUploadResult`, `SshDownloadResult`
  - `SshForwardResult`
- **Typed schema advertisement on 24 new `#[tool]` sites** (12 with `port_forward`, 12 without) — one `output_schema = schema_for_type::<…Result>()` call per `#[tool]` macro invocation in `src/infra/mcp/tool_router.rs`.
- **Full 21 / 21 (or 20 / 20 without `port_forward`) coverage on `tools/list[].outputSchema`** — every tool published by the server now carries an `outputSchema` field. Reference: `src/infra/mcp/results.rs` head doc-comment, "Coverage" section.

### Changed

- **`SshConnectResult` schema fields** — additive growth to cover the existing runtime payload variants on `status = "ok"` / `"reused"` / `"suggested"`:
  - `name: Option<String>` (mirrors the `NAME:` line on suggested matches)
  - `replaced: Option<usize>` (mirrors `REPLACED:` on `status = "ok"` when stale duplicate sessions are evicted on the connect path)
  - `matches: Option<Vec<SessionEntry>>` (mirrors the `MATCHES:` body on `status = "suggested"`)
  - `count: Option<usize>` (mirrors the `COUNT:` line on multi-match suggested responses)
- **`SshShellOpenResult.initial_buffer: Option<String>`** — mirrors the v4.7 `INITIAL_BUFFER:` Markdown line. Was an ad-hoc field in v4.7's free-form structured payload; v4.8 promotes it into the typed schema.
- **`src/infra/mcp/results.rs` doc-comment** — "Coverage" section rewritten to claim 21 / 21 (or 20 / 20 without `port_forward`); "Stability" section preserved.

### Compatibility

- Wire shape: byte-identical to v4.7.1 on the Markdown body and on the `structured_content` JSON payload. No env vars added, no error codes added, no runtime behaviour change. Only `tools/list[].outputSchema` grows.
- v4.7 / v4.6 / v4.5 / v4.0 / v3.0 hosts walking the Markdown body keep working unchanged. Hosts validating against the v4.7 partial `output_schema` continue to validate the same responses (the schemas remained source-compatible — additions only).

### Tests

- No new public-surface tests required (schema advertisement is checked by rmcp at macro-expansion time). The v4.7.1 baseline of 1168 lib tests + 2 integration tests passes unchanged.

## [4.7.1] — 2026-05-03

### Highlights

- **Stability patch driven by exhaustive Python integration + chaos battery (231 scenarios across 11 v4.7 test suites + 3 chaos suites + W1-W15 LLM cross-tool workflows + S1-S20 subscribe contract + CS1-CS12 subscribe chaos).** The user reported MCP resets under workload; the battery surfaced the root cause. Public MCP API stays byte-compatible with v4.7.0 — every contract preserved.

### Fixed

- **`max_buffer_size` tool argument on `ssh_shell_open` is now actually honoured** (v4.7.1 BUG #1, found by `chaos_v47_subscribe.py::cs2_lagged_recovery` and `cs6_buffer_overflow_compensation`). Prior to this patch the override reached the persisted `ShellEntity` but never flowed into the runtime `RunningShell.max_buffer_size: Arc<AtomicU64>` that the reader-task `flush_shell_buffer` helper consults. Result: the buffer cap was silently the adapter default (10 MiB) regardless of caller override, and slow-consumer workloads grew the buffer unbounded toward OOM territory — the most likely cause of the "MCP resetando em workload pesado" symptom. Fix: `SshClientPort::open_shell` gains a `max_buffer_size: Option<u64>` parameter; `OpenShellUseCase` threads it through; `RusshAdapter::do_open_shell` uses the override (or env-driven default) for both the entity AND the runtime atomic backing the reader-task flush threshold. Regression test `flush_shell_buffer_honours_supplied_cap_with_head_truncation` writes 2x the cap and asserts head truncation lands at the cap.
- **`ssh_exec_output` no longer returns `COMMAND_NOT_FOUND` after a successful `ssh_exec_cancel`** (v4.7.1 BUG #2, found by `test_v47_llm_workflows.py::test_w11_cancel_mid_flight`). Prior behaviour: `RusshAdapter::cancel` removes the adapter-internal command record immediately; `RusshOutputAdapter::snapshot_command` then returns `CommandNotFound` even though the repo entity is alive in `Cancelled` status (and `ssh_commands` correctly reports it). `GetCommandOutputUseCase::execute` propagated that error verbatim, breaking the canonical LLM cancel-mid-flight workflow. Fix: when the entity is in a terminal state (`Cancelled` / `Completed` / `Failed`) and `snapshot_command` returns `CommandNotFound`, treat it as a benign "snapshot evicted; entity has the saved status" — return the entity's saved status with empty stdout/stderr. Added `CommandStatus::is_terminal()` helper. Four regression tests cover the three terminal states plus a negative case asserting that `Running` + missing snapshot still surfaces `CommandNotFound` (genuine inconsistencies stay visible).
- **`with_idempotency` purity invariant locked in CI** (v4.7.1 BUG #3 turned out to be a non-bug; flagged by `chaos_v47_subscribe.py::cs12_idempotency_with_subscribe` reporting `post_replay_count_strict_zero=false`). Trace confirmed the wrapper at `tool_router.rs:322-324` already runs the use case only on cache miss; on cache hit it returns the cached body verbatim with no side effects. The 2 post-replay notifications observed in the chaos script come from the subscription debouncer's force-flush ticker (`adapters/subscription/legacy.rs:432-434`, default 1000ms cadence) firing during the chaos script's 1-second observation window. Three regression tests now lock the invariant in case future refactors add a side-call to the replay path: `idempotency_replay_does_not_invoke_callback`, `idempotency_absent_key_runs_callback_every_time`, `idempotency_failed_response_is_not_cached`.

### Test infrastructure

- **231 new Python integration / chaos scenarios** under `scripts/`:
  - `pytest.ini` with global `timeout=60s` (avoids the 1-hour ghost runs the user observed when many test files were aggregated)
  - 11 new v47 pytest files covering every v4.7 surface (structured_content / templates / progress / ssh_run / batches / prompts / idempotency / closest_match / initial_buffer / subscriptions / llm_workflows)
  - 2 new chaos suites: `chaos_v47.py` and `chaos_v47_subscribe.py` (CS1-CS12)
  - Upgraded paramiko fixture (`scripts/helpers/local_sshd.py`): SFTP subsystem now actually starts (was no-op); shell handler now allocates real PTY via `pty.openpty()` so `Ctrl+C` propagates as SIGINT; `send_exit_status` clamps signal exit codes (was crashing paramiko's `struct.pack`)
  - 16 existing test files updated for the v4.7 catalogue (21 tools, AGENT_ID rename, structured_content side-channel, _meta envelope on resources/read)
- **Test results in isolation: 123 PASS, 0 FAIL** on the v47 suites after this patch (was 122 PASS, 1 FAIL on v4.7.0 — the W11 failure is now green).

### Tests

- Lib tests: **1156 → 1168** (+12: 4 from BUG #1 RingBuffer cap regression + 4 from BUG #2 terminal-state + 1 from `is_terminal()` + 3 from BUG #3 idempotency invariant).

### Behaviour notes

- All three fixes are byte-compat preserving — the wire shapes match v4.7.0; only the bug behaviours change.
- BUG #1 changes the trait surface of `SshClientPort::open_shell` (added `max_buffer_size: Option<u64>` parameter). External crates implementing the port must update; in-repo adapters and tests are all updated.
- The pytest fixture-leak issue (zombie `ssh-mcp-stdio` processes when many test files are aggregated in one pytest invocation) is documented but not source-fixed in this patch — the operational mitigation is the new `pytest.ini` timeout plus running each file in isolation. A follow-up (v4.7.2 or v4.8.0) can close the rmcp HTTP child-process cleanup loop.

## [4.7.0] — 2026-05-03

### Highlights

- **MCP Inter-Tool Conversation 100%** — every tool-to-tool optimization the audit identified is now wired. Smaller LLMs (27B-30B class) get a typed JSON channel parallel to Markdown, server-advertised URI templates, mid-flight progress notifications, a one-shot `ssh_run` tool, batch tools, a 5-prompt catalog of canonical workflows, server-side idempotency dedup, NOT_FOUND closest-match suggestions, and an INITIAL_BUFFER snapshot on shell open. Tool count goes 18 → 21 (with `port_forward`) or 17 → 20 (without).
- Public MCP API stays byte-compatible with v4.6.0 on the **text channel** — every existing markdown response is identical to the byte. The `structured_content` channel is purely additive.

### Added

- **`structured_content` JSON parallel to Markdown on every tool** — `CallToolResult` now carries both the existing block-style Markdown text and a typed JSON object. Smaller LLMs index by key without parsing Markdown. Mirrors the KEY: value pairs 1:1 with snake_case field names; surfaces the `NEXT:` hint as a `next: [...]` array; surfaces error responses as `{ tool, status: "error", code, reason, detail }`. Six tools advertise typed `output_schema` (`SshConnectResult`, `SshExecResult`, `SshExecOutputResult`, `SshShellOpenResult`, `SshShellReadResult`, `SshTransferProgressResult`); the remaining 12 emit free-form structured payloads.
- **`resources/templates/list`** — server now advertises 4-5 RFC 6570 URI templates (`shell://{shell_id}/output{?cursor}`, `command://{command_id}/output{?cursor}`, `transfer://{transfer_id}/progress`, `session://{session_id}/health`, and `forward://{forward_id}/events{?cursor}` with `port_forward`). Smaller LLMs construct subscribe URIs without first calling `resources/list`.
- **Progress notifications during long async** — `notifications/progress` fire mid-flight on `ssh_exec_output(wait=true)` (5s cadence), `ssh_transfer_progress(wait=true)` (5s cadence), and `ssh_shell_wait_for` (1s cadence) when the request carries `_meta.progressToken`. Best-effort: notify errors are swallowed so the user-visible response is unchanged.
- **`ssh_run` convenience tool** — one-shot connect + execute + (optional) disconnect. Three round-trips collapsed into one. Args mirror `ssh_connect` + `ssh_exec` with `disconnect_after=true` default. Cost: 1 SSH handshake + 1 channel + (optional) disconnect.
- **`ssh_exec_batch` tool** — chains 1..=16 commands through one session with stop-on-failure semantics (default true). Returns aggregate Markdown plus structured `results[]` array.
- **`ssh_disconnect_many` tool** — best-effort batch disconnect of 1..=64 SESSION_IDs. Per-id failures don't abort the batch.
- **MCP prompts catalog** — `prompts/list` + `prompts/get` with 5 canonical workflows: `run_one_shot_command`, `investigate_session`, `upload_and_verify`, `interactive_shell_drive`, `cleanup_agent`. `prompts` capability advertised on the `initialize` handshake.
- **Idempotency `_meta.idempotency_key`** — DashMap-backed LRU dedup cache (default 300s TTL, 1024 entries; configurable via `SSH_IDEMPOTENCY_TTL_SECS` / `SSH_IDEMPOTENCY_MAX_ENTRIES`). 15 mutating tools wrapped: `ssh_connect`, `ssh_disconnect`, `ssh_disconnect_agent`, `ssh_exec`, `ssh_exec_cancel`, `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_press`, `ssh_shell_close`, `ssh_upload`, `ssh_download`, `ssh_forward`, `ssh_run`, `ssh_exec_batch`, `ssh_disconnect_many`. New error code `IDEMPOTENCY_KEY_TOO_LONG` for keys over 256 bytes. Read-only tools (`ssh_list_*`, `ssh_get_*` with read semantics, `ssh_shell_read`, `ssh_shell_wait_for`) ignore the key.
- **NOT_FOUND closest-match suggestions** — when `SESSION_NOT_FOUND` / `SHELL_NOT_FOUND` / `COMMAND_NOT_FOUND` / `TRANSFER_NOT_FOUND` / `FORWARD_NOT_FOUND` fires and the relevant repo has live entries, the DETAIL line now appends `closest matches: <id1>, <id2>, <id3>` (top 3 byte-level Levenshtein neighbors). Smaller LLMs recover from typos without round-tripping `ssh_list_*`. Pure-Rust implementation, no `strsim` dependency.
- **INITIAL_BUFFER on `ssh_shell_open`** — when the PTY emits stdout within ~100ms, the response carries an `INITIAL_BUFFER:` line (head-truncated to 4 KiB) plus `structured_content.initial_buffer`. Smaller LLMs that follow the `subscribe → read` pattern can sometimes skip the first round-trip. Tunable via `SSH_SHELL_OPEN_INITIAL_PEEK_MS` (default 100), `SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS` (default 5), `SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES` (default 4096).
- **`Implementation.instructions` updated** — both `port_forward` and `not(port_forward)` few-shot blocks bumped to 21 / 20 tool count and call out `ssh_run`, INITIAL_BUFFER, and the idempotency convention.

### Tests

- Lib tests: **1091 → 1156** (+65: 12 from structured_content, 8 from templates + progress, 22 from new tools + prompts, 23 from idempotency / closest-match / initial-buffer). Wire-shape regressions: zero on the text channel.

### Behaviour notes

- Wire byte-compat with v4.6 on the text channel. The structured_content channel is purely additive — clients that ignore it get the same v4.6 Markdown response.
- `ssh_run`, `ssh_exec_batch`, `ssh_disconnect_many` are pure orchestration over existing use cases — no new domain entities.
- Idempotency cache is per-process (not persisted). Server restart clears it. The cache is **lock-free** on the hot path (DashMap); GC runs lazy on insert plus an explicit `evict_expired()` accessor for opt-in periodic sweep.
- Progress notifications obey the MCP spec — when the request omits `_meta.progressToken`, no notifications fire (matches v4.6 behaviour exactly).

## [4.6.0] — 2026-05-03

### Highlights

- LLM UX **100%** — every dimension audited at v4.5 reaches its theoretical maximum for a 27B-30B class model. Self-bootstrap from `Implementation.instructions` + tool list with no external docs. Successor tools advertised in-band via a new `NEXT:` line. Subscribe-first nudges via inline `HINT:` lines on every async-spawn response. JSON Schema `default` keyword visible on every optional field. All 14 documented wire error codes now reach the wire (3 reserved tags promoted to live). Server icon wired. Cost / timing hints inlined in tool descriptions.
- Public MCP API stays byte-compatible with v4.5.0 except for one narrow rename: the wire key for `agent_id` changed from `AGENT:` to `AGENT_ID:` for consistency with every other `_ID` field. Hosts that walk key-value lines generically are unaffected; hosts that grep `^AGENT:` literally must update.

### Added

- **`NEXT:` advisory line** — every response with a clear successor tool now ends with a `NEXT:` line listing pipe-separated concrete tool calls (e.g. `NEXT: ssh_exec_output(command_id=c-X, wait=true) | ssh_exec_cancel(command_id=c-X)`). 12 emission sites:
  - `SSH_CONNECT: OK / REUSED / SUGGESTED`
  - `SSH_SESSIONS: OK` (only when non-empty)
  - `SSH_EXEC: STARTED`
  - `SSH_EXEC_OUTPUT: RUNNING`
  - `SSH_SHELL_OPEN: OK`
  - `SSH_SHELL_WRITE: OK` / `SSH_SHELL_PRESS: OK`
  - `SSH_SHELL_WAIT_FOR: MATCHED / TIMEOUT`
  - `SSH_UPLOAD: STARTED` / `SSH_DOWNLOAD: STARTED`
  - `SSH_TRANSFER_PROGRESS: RUNNING`
  - `SSH_FORWARD: OK`
- **Subscribe-first `HINT:` lines (4 new sites)** — `ssh_shell_open`, `ssh_exec` (async), `ssh_upload` / `ssh_download`, and `ssh_forward` now emit a one-line `HINT:` steering callers to the matching `resources/subscribe` URI ("subscribe to <scheme>://<id>/... for realtime ..."). Anti-leak `HINT:` on `ssh_sessions` (>5 sessions/agent, v4.4) is unchanged.
- **JSON Schema `default` keyword visible on 27 optional fields** — `connection.rs` (7), `execute.rs` (8), `shell.rs` (10), `sftp.rs` (2). Smaller LLMs that read the schema mechanically now see the default value without parsing English from the description. Implemented via `#[schemars(default = "fn_name")]`.
- **Cost / timing hints in tool descriptions** — every `#[tool(description = "...")]` ends with a `Cost: ...` line covering O() complexity, expected latency, blocking-vs-async, and pointer to subscribe paths where applicable. 35 description sites updated (18 with `port_forward` + 17 without).
- **3 reserved error tags now emit live**:
  - `FORWARD_FAILED:` — `application/forward_port.rs::ForwardPortUseCase::preflight_bind` (pre-flight `TcpListener::bind` failures other than `AddrInUse`).
  - `LOCAL_NOT_FILE:` — `application/upload_file.rs::UploadFileUseCase::guard_local_path_is_file` (pre-flight `metadata` check rejects directories / non-regular files).
  - `REMOTE_METADATA_ERROR:` — `adapters/sftp/russh_sftp_adapter.rs::stat_remote_size` (download remote `stat` failures).
  All 14 documented codes in `ERRORS.md` now reach the wire.
- **Server icon wired** — `Implementation.icons` points to `https://raw.githubusercontent.com/farchanjo/ssh-mcp/master/assets/icon.svg` (image/svg+xml, sizes "any"). New `assets/icon.svg` (128x128 terminal-themed mark with `$ ssh` prompt). URL resolves after the v4.6 master push; clients gracefully fall back to title + description before then.

### Changed

- **Wire key rename: `AGENT:` → `AGENT_ID:` (narrow breaking change)** — 7 render sites: `ssh_connect`, `ssh_sessions` (per-row), `ssh_exec` (async), `ssh_shell_open`, `ssh_upload` / `ssh_download`, `ssh_disconnect_agent`. CLAUDE.md required all IDs to end with `_ID` — this closes the last drift. Hosts walking key-value lines generically: no change. Hosts grepping `^AGENT:` literally: update to `^AGENT_ID:`.
- `ssh_connect` description trimmed for token efficiency where redundant; cost line preserves the existing tip bullets.

### Tests

- Lib tests: **1074 → 1091** (+12 from JSON Schema default snapshot tests; +6 from new error-code emission tests; -1 redundant test consolidated). Clippy / build / fmt all clean on touched files.

### Behaviour notes

- The narrow `AGENT:` → `AGENT_ID:` rename is the only wire-shape change in v4.6. Every other addition is purely additive (new lines appended; existing keys preserved).
- `NEXT:` lines are advisory only — they never carry side effects and never affect MCP wire validity. Hosts that ignore them are still fully functional.
- Cost hints in tool descriptions are conventional English text, not machine-readable. They steer models that rank tools by description similarity; they do not constrain behaviour.

## [4.5.0] — 2026-05-03

### Highlights

- LLM UX overhaul. Smaller LLMs (27B-30B class) now have everything they need to drive ssh-mcp without external docs: real bytes + `_meta` envelope on `resources/read`, stable peer identity across subscribe / unsubscribe, 14 granular wire error codes, per-tool `Tool.title` + `ToolAnnotations` hints, server-level `Implementation.title` / `description` / `website_url`, and a few-shot `instructions` field with three canonical workflows. Public MCP API stays byte-compatible with v4.4.0 / v4.3.0 / v4.2.0 / v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same env vars, same defaults; the markdown shape is extended **additively** (forward render adds `FORWARD_ID:` and `SESSION_ID:`) so existing v3 / v4 hosts continue to parse responses without any change.

### Fixed

- **Stable peer identity** — `subscribe` and `unsubscribe` now share the same `PeerId` derived from the `Mcp-Session-Id` header (HTTP transport) or a `Stdio` singleton key. Previously `RmcpPeerHandle::new` minted a fresh UUID per call, so `unsubscribe` was a silent no-op (the registry never matched). The shared `PeerTable` now exposes `get_or_mint`, `lookup_peer`, `drop_by_id`, and `gc_closed_peers`. Wired to the existing peer-GC pump on the stdio path; HTTP path is session-scoped so each session gets its own table cleared on session drop.
- **`resources/read` returns the real body and `_meta`** — the H17 placeholder is gone. Shell URIs now return UTF-8 lossy bytes (`text/plain`); command URIs return v3-style block payloads with shared nonce delimiters; transfer / session / forward URIs return the existing JSON snapshot with `application/json` mime. `_meta` carries `kind`, `cursor`, `buffer_size` (shell + command only), `last_seq`, and `status`. Subscribe-first contract is now byte-compatible with what `LLM_GUIDE.md` describes.
- **Forward render exposes IDs** — `SSH_FORWARD: OK` blocks now emit `FORWARD_ID:` and `SESSION_ID:` lines so callers can construct the matching `forward://<FORWARD_ID>/events` resource URI directly from the tool response.

### Added

- **Granular wire error codes via tag-prefix dispatch** — `classify_error` now recognises 14 documented codes by parsing a `TAG:` prefix from `DomainError::InvalidArgument` / `Transport` / `Sftp` messages. Application and adapter layers prefix every known failure site so smaller LLMs can branch recovery logic on a specific code instead of guessing from the collapsed `INVALID_ARGUMENT` / `TRANSPORT_ERROR` / `SFTP_ERROR` codes. Tags surfaced today: `EMPTY_PATTERNS`, `TOO_MANY_PATTERNS`, `PATTERN_TOO_LONG`, `MODIFIER_NOT_ALLOWED`, `INVALID_REPEAT`, `FEATURE_DISABLED`, `WRITE_FAILED`, `CHANNEL_FAILED`, `COMMAND_FAILED`, `LOCAL_FILE_ERROR`, `SFTP_OPEN_FAILED`. Reserved (dispatcher recognises but no live raise yet): `FORWARD_FAILED`, `LOCAL_NOT_FILE`, `REMOTE_METADATA_ERROR`. Untagged messages still fall through to the legacy flat codes.
- **Rich MCP server identity** — `Implementation` now carries:
  - `title = "SSH Remote Shell"`
  - `description = "Run remote commands, drive PTY shells, transfer files via SFTP, and forward TCP ports over SSH..."`
  - `website_url = "https://github.com/farchanjo/ssh-mcp"`
  - `icons = None` (TODO until the SVG asset lands at a hosted URL).
- **Per-tool `title` + `ToolAnnotations`** — every one of the 18 tools (or 17 without `port_forward`) now declares a human-readable `title` and the `read_only_hint` / `destructive_hint` / `idempotent_hint` annotations so MCP hosts can rank, filter, and warn before destructive use. Read-only tools: `ssh_sessions`, `ssh_exec_output`, `ssh_commands`, `ssh_shell_wait_for`, `ssh_transfer_progress`. Idempotent destructive: disconnect / cancel / shell_close. Open-world hint left unset (defaults to true).
- **Few-shot `instructions` field** — replaces the previous one-liner with three canonical workflows (run command, interactive shell, upload) plus the v4.4 `EXPIRES_AT` / `HINT` / `agent_id` steering cues. Two flavours: `INSTRUCTIONS_WITH_FORWARD` (797 bytes) and `INSTRUCTIONS_WITHOUT_FORWARD` (785 bytes). The non-`port_forward` build advertises 17 tools and 4 streams.
- **Schema documents per-key modifier policy** — `ssh_shell_press` `key` field now spells out the allowed modifier set per key class in its JsonSchema description (arrows / nav / F-keys accept any of `shift`/`alt`/`ctrl`; `tab` accepts only `shift`; control bytes reject every modifier). `ssh_shell_wait_for` `patterns` field documents the wire codes returned on validation failure.
- **Doc surface** — `docs/LLM_GUIDE.md`, `docs/API.md`, `docs/ERRORS.md`, `docs/RESOURCES.md`, `docs/FLOWS.md`, `README.md`, and `CLAUDE.md` are now in sync with v4.5.0 behaviour. New LLM_GUIDE sections cover connection lifecycle (v4.4), granular error codes (v4.5), server identity (v4.5), and a smaller-LLM cookbook with three canonical workflows.

### Tests

- Lib tests: **1060 → 1074** (+5 from `_meta` + PeerTable + render coverage; +9 from granular-error-code classifier coverage).
- 14 Mermaid diagrams (LLM_GUIDE/RESOURCES/FLOWS) re-validated with `mmdc 11.14.0`.
- No public wire-shape regressions: existing v3 / v4 hosts parse v4.5 responses without modification.

### Behaviour notes

- `Tool.title` arrived through `#[tool(title = "...")]` at the macro top level (verified in rmcp-macros 1.6 source). `ToolAnnotations` arrived through `#[tool(annotations(read_only_hint = ..., destructive_hint = ..., idempotent_hint = ...))]`.
- Peer-GC pump on the stdio path now also prunes closed peers from the new `PeerTable`. HTTP path is session-scoped — each rmcp service factory call yields a fresh `McpSshServer` and a fresh `PeerTable`, naturally collected on session drop.

## [4.4.0] — 2026-05-03

### Highlights

- LLM-steering minor release. Public MCP API stays byte-compatible with v4.3.0 / v4.2.0 / v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same env vars, same defaults; the markdown shape is extended **additively** (new `EXPIRES_AT` line, new `HINT` line) so existing v3 / v4 hosts continue to parse the response without any change.

### Added

- **agent_id-aware ranking** — `connect_session` use case now ranks identity matches owned by the requesting `agent_id` ahead of foreign matches under both `reuse=auto` and `reuse=suggest`. Tiebreaker is the existing `connected_at desc` ordering. When the caller does not pass `agent_id`, ranking is a no-op (newest-first stays). Implemented in `src/application/connect_session.rs::rank_by_agent_affinity`.
- **EXPIRES_AT line** — `SSH_CONNECT: OK` and `SSH_CONNECT: REUSED` blocks now emit `EXPIRES_AT: <rfc3339>` (computed as `connected_at + ConfigPort::inactivity_timeout`) so an LLM can ping (any cheap call) before the inactivity sweeper closes the session. When `persistent=true`, the renderer emits `PERSISTENT: true` and skips `EXPIRES_AT`. When the configured inactivity timeout is zero, `EXPIRES_AT` is also omitted (sweeper disabled). Renderer change in `src/infra/mcp/render/connection.rs::append_persistent_or_expiry` + `compute_expires_at`.
- **anti-leak HINT** — `SSH_SESSIONS` now appends `HINT: agent '<id>' owns N sessions; consider ssh_disconnect_agent to bulk-cleanup` when any agent owns more than 5 healthy sessions in the rendered list. Threshold lives in `ANTI_LEAK_HINT_THRESHOLD` (`src/infra/mcp/render/connection.rs`).
- **`ssh_connect` description Tip bullets** — the tool description (both feature flavours of the `#[tool_router]` impl) now includes:
  - `Tip: pass reuse=auto to let the server pick the most recent healthy match in a single round-trip. Use reuse=suggest (default) when you want to inspect matches before reusing. Use reuse=force_new to bypass identity matching entirely.`
  - `Tip: pass agent_id so subsequent sessions are grouped and you can bulk-cleanup with ssh_disconnect_agent. When agent_id is set, reuse=auto/reuse=suggest rank sessions owned by the same agent first.`

### Changed

- `ConnectOutcome::Connected` and `ConnectOutcome::Reused` carry two new fields (`persistent: bool`, `inactivity_timeout: Duration`) so the renderer can compute `EXPIRES_AT` without round-tripping through the SSH adapter or the entity. Internal change — use cases consumed by external code stay generic over the same ports.

### Tests

- Lib tests: **1048 → 1060** (+ 5 connect_session tests covering ranking owned-first / fallback-to-newest / Suggest ordering / `rank_by_agent_affinity` stability + no-op no-agent-id; + 7 render tests covering `EXPIRES_AT` present/absent / persistent skips it / zero-timeout omits it / Reused includes it / anti-leak HINT above threshold / no HINT at threshold / no HINT without agent ids).
- Python integration: stdio **13/13** PASS (no wire-format change in the suite's expected keys).
- HTTP suite: untouched (no test-file changes in `scripts/test_http.py`).

## [4.3.0] — 2026-05-03

### Highlights

- Patch release extending the v4.2 status-pump bridge with **registration sinks**, closing the **TOCTOU race in the per-session transfer cap**, fixing a **shell-id collision** under concurrent `ssh_shell_open` bursts, and shipping a full **chaos suite** (errors, locks, recovery, exhaustion + master runner) on top of the existing stress harness. Public MCP API stays byte-compatible with v4.2.0 / v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same markdown shape, same env vars, same defaults.

### Fixed

- **Shell-id collision under bursts (Bug C)** — concurrent `ssh_shell_open` calls on the same session no longer collide on a deterministic id. The russh adapter now suffixes a UUID v4 onto every minted `ShellId` so 100 concurrent opens produce 100 distinct ids even when the upstream `IdGeneratorPort` returns the same time-derived prefix.
- **Per-session transfer cap TOCTOU (Bug D)** — `ssh_upload` / `ssh_download` no longer breach `SSH_MAX_TRANSFERS_PER_SESSION` under contention. The repository now exposes an atomic `TransferRepository::insert_if_under_cap(entity, cap)` that performs the count probe and the insert under a single shard guard. The use cases route through this method in `persist_or_rollback`; the production transfer registration sink does the same so adapter-side registers cannot slip past the gate either. Optimistic pre-check stays in place to short-circuit obvious overflows before any SFTP RTT.
- **Registration sinks (Bug A)** — `resources/subscribe shell://X/output` no longer fails with `SHELL_NOT_FOUND` immediately after `ssh_shell_open` lands. The russh adapter now bridges its in-memory shell binding into the domain `ShellRepository` via a new internal `ShellRegistrationSink` trait spawned fire-and-forget so the use-case-side `ShellRepository::insert` keeps winning the canonical race; the sink uses `get-then-insert` semantics so a duplicate is a no-op. Mirrors apply for `RunningCommand` (`CommandRegistrationSink`), `TransferShared` (`TransferRegistrationSink`, cap-aware) and (feature-gated) `ForwardHandle` (`ForwardRegistrationSink`). Adapter-driven teardown paths (`close_shell` inactivity sweep, future bulk-cleanup) call `unregister` so the repo never carries stale rows past adapter destruction.
- **Stress scripts go stdio (Bug B)** — `scripts/stress_{concurrent_writes,lagged_sub,locks,subscribe}.py` now default to spawning a single coordinator `ssh-mcp-stdio` child instead of fanning out HTTP clients across ephemeral ports. Set `STRESS_TRANSPORT=http` to flip back to the v4.2 HTTP behaviour. Each script writes a single JSON line summary to stdout (`status: ok | fail | skip`).

### Added

- **Chaos suite** — four new scripts and a master runner under `scripts/`:
  - `chaos_errors.py` — 27 documented-error scenarios (bad ids, validation failures, oversized inputs, unknown sessions, idempotent agent disconnect, invalid URI subscribe, etc.).
  - `chaos_locks.py` — 7 lock-contention scenarios (parallel `shell_open`, parallel `shell_write` FIFO, mixed `send_key + write`, subscribers-then-close, burst execute+cancel, concurrent transfers, open+cancel+close race).
  - `chaos_recovery.py` — 5 recovery scenarios (kill-during-command, sigterm session, buffer truncation, cancel-during-cancel, subscribe-after-close).
  - `chaos_exhaustion.py` — 3 exhaustion scenarios (push 100 MiB upload, max-sessions burst, 1000 subscribers).
  - `chaos_runner.py` — aggregator that runs `test_stdio` + 4 stress + 4 chaos suites and prints a single ASCII summary table.
- `scripts/helpers/chaos.py` — stderr-capturing stdio transport (`_StderrCapturingStdioTransport`), `ChaosSshTarget`, `chaos_session()` context manager, and JSON-line emitters (`write_event`, `write_summary`).

### Changed

- `chaos_locks.py::parallel_shell_open` now accepts `TRANSPORT_ERROR` / `CHANNEL_OPEN_FAILED` / `SSH_ERROR` as valid second-class rejections alongside `MAX_SHELLS_EXCEEDED` (upstream sshd `MaxSessions = 10` is a known environmental cap; the application-side floor of "exactly 10 shells open + 90 documented errors + zero panics" stays strict).
- `scripts/run_all.sh` builds both default-features and `--no-default-features` binaries, then executes pytest + 4 stress + 4 chaos + the aggregate runner.

### Internal

- New traits in `src/adapters/ssh/internal/status_sink.rs`: `CommandRegistrationSink`, `ShellRegistrationSink`, `TransferRegistrationSink`, plus the `port_forward`-gated `ForwardRegistrationSink`. Each carries `register(entity)` + `unregister(id)` over manual AFIT (boxed futures) so the trait stays object-safe.
- New atomic API on `TransferRepository`: `insert_if_under_cap(entity, cap)`. The DashMap implementation acquires the session-bucket shard guard first, gates on the bucket length, and then binds the primary row before releasing — closing the read-then-insert TOCTOU window observed in v4.2.
- New production sinks in `src/composition/status_sinks.rs`: `RepoCommandRegistrationSink` / `RepoShellRegistrationSink` / `RepoTransferRegistrationSink` (cap-aware, holds a `Arc<EnvConfig>`) / `RepoForwardRegistrationSink`. Each uses `get-then-insert` so the canonical use-case insert is never overwritten; `unregister` swallows `Ok(None)` cleanly.
- `RusshAdapter` and `RusshSftpAdapter` gain optional `with_*_registration_sink` setters (default no-op). The composition root wires production sinks alongside the existing status sinks.
- Adapter-side `register` calls are spawned (fire-and-forget) so the use case wins the race; `unregister` calls happen inline at lifecycle exit.
- Helpers `make_stress_client` / `make_stress_client_http` / `stress_transport_mode` added to `scripts/helpers/fixtures.py` so the four stress scripts share one transport-selection seam.

### Tests

- Lib tests: **1031 → 1048** (registration-sink unit tests covering noop variants, duplicate-id handling, unregister silent-when-absent, the feature-gated forward path; new TOCTOU coverage on `DashMapTransferRepo::insert_if_under_cap` including the 50-concurrent-inserts atomic-gate test; new sink-cap-honour test on `RepoTransferRegistrationSink`).
- Python integration suites: stdio **13/13** (unchanged); the 4 stress scripts succeed in stdio mode against a local sshd; the 4 chaos scripts pass with the new strict + environment-aware assertions.

## [4.2.0] — 2026-05-03

### Highlights

- Patch release closing four runtime regressions surfaced by the Python stdio integration suite after the v4.1 deep-decouple. Public MCP API stays byte-compatible with v4.1.0 / v4.0.0 / v3.0.0 — same 18 tools, same 5 resource schemes, same markdown shape, same env vars, same defaults.

### Fixed

- **Command status pump** — `ssh_exec_output { wait=true }` no longer hangs on `RUNNING` after the SSH driver completes. The russh adapter now spawns a dedicated status watcher per async command that bridges the live `RunningCommand.status_rx` watcher channel into the domain `CommandRepository` via a new internal `CommandStatusSink` trait. The composition root pins a `RepoCommandStatusSink` over the shared `DashMapCommandRepo`; tests/fixtures keep the no-op default so no behavioural change reaches the use-case test surface.
- **Cancel snapshot** — `ssh_exec_cancel` no longer surfaces `COMMAND_NOT_FOUND` when the cancellation succeeds. The cancel use case now snapshots stdout/stderr **before** issuing `SshClientPort::cancel`, falling back to an empty snapshot if both pre- and post-cancel reads fail. The SSH adapter still tears its internal command record down inside `cancel`, but the use case no longer races that tear-down.
- **Shell output flush** — `ssh_shell_read` / `ssh_shell_wait_for` long polls no longer time out with empty `data` after a write produced visible output. The shell reader task now flushes incoming PTY frames to `RunningShell.history` (and the broadcast channel) on every chunk (`SHELL_FLUSH_THRESHOLD = 1`) instead of waiting for a 4 KiB batch — interactive shells need per-frame visibility.
- **Shell close pump** — `RusshAdapter::close_shell` now pumps an explicit `Closed` notification into the domain `ShellRepository` via a new internal `ShellStatusSink` trait. Mirrors the command sink path so subscribers / long-poll consumers observe the terminal state without waiting for an entity-removal sweep.
- **Transfer status pump** — SFTP upload/download drivers now surface `Completed` / `Failed` / `Cancelled` and intermediate `record_progress` updates into the domain `TransferRepository` via a new internal `TransferStatusSink` trait. Wired in `composition::prod` over the shared `DashMapTransferRepo`. `ssh_transfer_progress { wait=true }` polls now observe the terminal state cleanly.
- **Python integration parser** — `scripts/helpers/parse_block.py` now publishes wire-key aliases (`exit` → `exit_code`, `sessions` → `sessions_disconnected`, `commands` → `commands_cancelled`) so existing test assertions match the v3 short-form keys without any wire-format change. Test-only fix; the markdown response shape stays byte-identical.

### Internal

- New module `src/adapters/ssh/internal/status_sink.rs` — purpose-built `Send + Sync` trait surface (manual AFIT via boxed futures) so the production russh adapter can hold a single `Arc<dyn CommandStatusSink>` / `Arc<dyn ShellStatusSink>` field without dragging the repository generics into its type list.
- New module `src/composition/status_sinks.rs` — production `RepoCommandStatusSink` / `RepoShellStatusSink` / `RepoTransferStatusSink` implementations backed by `DashMap*Repo` and wired by `composition::prod::build_use_cases`.
- `RusshAdapter` and `RusshSftpAdapter` gain optional `with_*_status_sink` setters (default no-op) so the v4.1 public-surface tests continue to compile and pass without any wiring change.

### Tests

- Lib tests: **1014 → 1031** (+ 10 status-sink unit tests, + 7 production sink integration tests).
- Python integration suites: stdio **8/13 → 13/13**; HTTP **14/14 → 14/14**.

## [4.1.0] — 2026-05-03

### Highlights

- H17.6 deep decouple closes the v4.0.0 deferral window. Every foundational `crate::mcp::*` reference is gone; `src/mcp/` is deleted (~6 500 LOC removed from the runtime tree). The `async-trait` direct dependency is dropped — every adapter uses native AFIT (statically dispatched through enums where dyn-safety was previously required).
- Public MCP API stays byte-compatible with v4.0.0 (and v3.0.0). Same 18 tools, same 5 resource schemes, same markdown shape, same env vars, same defaults.
- Test count grew from **1023 (1021 lib + 2 integration)** in v4.0.0 to **1016 (1014 lib + 2 integration)** in v4.1 — a small reduction reflects the removal of `mcp::*` internal-state regression tests now redundant against the relocated adapter-internal modules; every public-surface test still passes.

### Removed

- `src/mcp/` tree (~6 500 LOC): `client.rs`, `session.rs`, `sftp.rs`, `shell.rs`, `async_command.rs`, `transfer.rs`, `subscription.rs`, `auth/`, `config.rs`, `error.rs`, `types.rs`. The `crate::mcp::*` namespace no longer exists in `src/lib.rs` — only `domain/`, `ports/`, `application/`, `adapters/`, `infra/`, `composition/` remain at the crate root.
- `async-trait` direct dependency dropped from `Cargo.toml`. The v3 strategy chain that pinned it has been rewritten to native AFIT inside the SSH adapter (`src/adapters/ssh/internal/auth/`); dyn dispatch is replaced by an enum exhaustively dispatching to the three concrete strategies. Any `async-trait` copies that remain in the dependency tree are transitive (e.g. via rmcp) and outside our control.

### Changed

- Foundational types relocated to adapter-internal paths so each adapter is self-contained:
  - `src/adapters/ssh/internal/{client,session,async_command,shell,types,error}.rs` — russh wiring helpers (`connect_to_ssh_with_retry`, `execute_ssh_command`, `open_pty_shell`, `SshClientHandler`), `RunningCommand`, `RunningShell` + `RingBuffer`, `SessionInfo` / `AsyncCommandInfo` / `ShellInfo` / `TransferInfo`, retry classification.
  - `src/adapters/ssh/internal/auth/{traits,password,key,agent,chain}.rs` — internal `AuthStrategyPort` + AFIT chain (no async-trait, statically dispatched).
  - `src/adapters/sftp/internal/{sftp,transfer,types}.rs` — streaming SFTP transfer state, `RunningTransfer`, shared payload structs.
  - `src/adapters/config/internal/mod.rs` — env-var resolvers feeding `EnvConfig`.
  - `src/adapters/subscription/legacy.rs` — transitional home for `SubscriptionRegistry` + `SUBSCRIPTION_REGISTRY` global + `spawn_peer_gc`. The hexagonal `MemoryRegistry<N>` adapter is the forward-looking replacement consumed by use cases; the legacy adapter coexists until the SSH/SFTP runtime adapters get wired through the port surface end to end.

### Internal

- H17.6 P1 `bf646f9` — relocate v3 internals to adapter-internal modules under `src/adapters/{ssh,sftp,config}/internal/`.
- H17.6 P2 `00009e3` — decouple `AuthChain` via internal `AuthStrategyPort`; drop `async-trait` direct dep.
- H17.6 P3+P4 `72f1ccd` — final `src/mcp/` delete; relocate `config`, `error`, and `subscription` (legacy) under their owning adapters.

### Migration

See [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) for the v4.1 contributor note (foundational `mcp::*` paths gone — code now lives at `crate::adapters::{ssh,sftp,config,subscription}::internal::*` and `crate::adapters::subscription::legacy`). **No client-side migration is required** — v3 / v4.0 hosts work against v4.1 servers without any change.

### Etapa trail (v4.1 commits)

H17.6 P1 `bf646f9`, P2 `00009e3`, P3+P4 `72f1ccd`.

## [4.0.0] — 2026-05-03

### Highlights

- Internal restructuring to a full **Hexagonal (Ports and Adapters)** architecture. **The public MCP API is unchanged** — every v3 client / host / LLM keeps working without any change to wire format, tool catalogue, resource schemes, env vars, or markdown response shape. v4 is a codebase migration, not a protocol break.
- Test count grew from **832 (820 lib + 12 integration)** in v3 to **1023 (1021 lib + 2 integration)** in v4.
- ~14k LOC of v3 monolith deleted in H17.5a after every consumer migrated to the new layers.

### Breaking (internal architecture only — public MCP surface stable)

- Migrate to a full hexagonal layout: `src/{domain, ports, application, adapters, infra, composition}/`. The v3 `src/mcp/{tools, server, resources, message, storage, schema, keys}` modules are gone; only the foundational `src/mcp/{client, session, sftp, shell, async_command, transfer, subscription, auth, config, error, types}` set survives runtime-active and is slated for absorption in v4.1 (etapa H17.6).
- Use cases moved out of `src/mcp/tools/<domain>.rs` into one struct per business operation under `src/application/`. Each use case takes its ports as generic type parameters (static dispatch via `trait-variant` AFIT, **no `Box<dyn Trait>` in hot paths**) and is unit-tested against in-memory fakes — **zero rmcp / russh / SFTP machinery in the test path**.
- Concrete adapters land under `src/adapters/{ssh, sftp, repo/dashmap, auth, clock, config, id_generator, subscription, notifier, output_stream}/`. The composition root `src/composition/{prod, fixtures}.rs` pins concrete types at compile time so wiring errors surface at `cargo build` rather than runtime.
- `src/infra/mcp/{server, tool_router, resource_handlers, peer_handle, args, render, helpers}` owns the inbound rmcp surface (the 18 `#[tool]` entry points + `resources/*` handlers). Args are split out of v3 `schema.rs` into per-tool `Deserialize + JsonSchema` structs; markdown rendering is split out of v3 `message::builder` into per-domain modules.
- `async-trait` direct dep retained transitionally for the orphaned v3 `src/mcp/auth/` chain (runtime-unreachable in v4, deleted in H17.6 alongside the foundational `mcp::*` modules).

### Added

- 22 use cases under `src/application/`: `connect_session`, `disconnect_session`, `list_sessions`, `disconnect_agent`, `execute_command`, `get_command_output`, `list_commands`, `cancel_command`, `open_shell`, `write_shell`, `send_key`, `read_shell`, `wait_for_pattern`, `close_shell`, `upload_file`, `download_file`, `get_transfer_progress`, `forward_port`, `list_resources`, `read_resource`, `subscribe_resource`, `unsubscribe_resource`, plus the `peer_gc` background sweep.
- 14 ports under `src/ports/`: `ssh_client`, `sftp_client`, `output_stream`, `session_repo`, `command_repo`, `shell_repo`, `transfer_repo`, `forward_repo`, `subscriber_registry`, `notifier`, `id_generator`, `clock`, `config`, `auth_strategy`. All sync slices via plain trait, async slices via `trait-variant` AFIT (replaces `async-trait`'s `Box<dyn Future>` boxing).
- Adapters under `src/adapters/`:
  - `ssh/russh_adapter.rs` (production) + shared `SshHandleRegistry` so the SFTP adapter reuses the russh handle from `RusshClient` instead of opening a second connection.
  - `sftp/russh_sftp_adapter.rs` (production) + `sftp/in_memory.rs` (test fixture).
  - `repo/dashmap/{session, command, shell, transfer, forward}.rs` — lock-free in-memory implementations of every repo port.
  - `auth/{password, key, agent, chain}.rs` — `trait-variant` AFIT rewrite of the v3 strategy chain.
  - `clock/{system, fake}.rs`, `config/env.rs`, `id_generator/{uuid, deterministic}.rs`.
  - `subscription/memory_registry.rs` — `MemoryRegistry<N>` generic over the notifier port (no `Box<dyn>`).
  - `notifier/{rmcp_adapter, rmcp_peer}.rs` — `PeerHandle` abstraction so use cases never see `rmcp::Peer<RoleServer>` directly.
  - `output_stream/{russh_output, in_memory}.rs` — abstract over PTY broadcast streams.
- Infra MCP layer (`src/infra/mcp/`):
  - `server.rs` (`McpSshServer<UC>` generic).
  - `tool_router.rs` (the 18 `#[tool]` entry points; one per use case).
  - `resource_handlers.rs` (`resources/list`, `resources/read`, `resources/subscribe`, `resources/unsubscribe`, `notifications/resources/updated`).
  - `peer_handle.rs` (re-exports the `PeerTable` so binaries can plumb peer-to-handle mapping into the registry).
  - `args/` (per-tool argument structs — replaces v3 `schema.rs`).
  - `render/` (per-domain markdown builders — replaces v3 `message::builder`).
  - `helpers/{error, nonce, output}.rs` (rendering primitives — replaces v3 `message::helpers`).
- Composition root `src/composition/`:
  - `prod.rs` — wires the production adapter set (russh + russh-sftp + DashMap + env config + UUID v4 + tokio mpsc); used by both `ssh-mcp` (HTTP) and `ssh-mcp-stdio`.
  - `fixtures.rs` — wires the deterministic in-memory adapter set (FakeClock + DeterministicIdGen + InMemorySftp + …) for use cases under `cargo test --features test-fixtures`.
- `PeerHandle` abstraction so use cases interact with rmcp peers through a sync handle (`subscribe`, `unsubscribe`, `notify`) instead of holding a live `Peer<RoleServer>` inside a hot `DashMap` value.
- `SshHandleRegistry` shared between the `russh_adapter` and the `russh_sftp_adapter` to fix the v3 dual-connection cost when SFTP and PTY ran on the same session.
- New test fixture feature `test-fixtures` (off by default) — exposes `FakeClock`, `DeterministicIdGen`, `InMemorySftp`, `MemoryRegistry`, etc. so downstream crates can compose the same use cases against deterministic adapters.
- Documentation:
  - `docs/MIGRATION_v3_to_v4.md` — codebase-contributor migration guide (file-path table, per-layer responsibility map, before/after dependency graphs).
  - `docs/adr/0002-adopt-hexagonal-architecture.md` — design rationale, alternatives considered (modular monolith / SOLID-light extension), trade-offs, lock-free invariants under the new layout.
  - `docs/LOCKS.md` — full rewrite for v4. Maps every lock-free invariant to the layer that enforces it (domain types, repo adapters, registry adapter, output-stream adapter).
  - `docs/ARCHITECTURE.md` — full rewrite for v4 hexagonal layout (layer-by-layer module map, dependency graph, sequence diagrams). Mermaid validated via `mmdc`.
- Integration smoke `tests/v4_smoke.rs` exercises the full composition root end to end (replaces the v3 `tests/server_integration.rs` which was bound to the old globals).

### Changed

- All 18 MCP tool descriptions, JSON schemas, and response markdown bodies are **byte-compatible** with v3 (verified by snapshot tests in `tests/v4_smoke.rs`). The 8-hex-char nonce and `--- stdout [<nonce>] ---` delimiter format is unchanged.
- The 5 `resources/*` schemes (`shell://`, `command://`, `transfer://`, `session://`, `forward://`) keep identical URIs, cursor semantics, debounce timings, sequence-number gap detection, and lagged auto-recovery behaviour.
- All 25+ env vars (`SSH_*`, `MCP_*`, `RUST_LOG`) keep identical names, defaults, floors, and caps.
- HTTP transport bind / path defaults (`0.0.0.0:8000` on `/`) and stdio transport behaviour are identical.
- The strict Clippy lint baseline (forbid layer A + deny layer B + lock-free invariants) is identical to v3 — every v3 lint kept; the new layers comply without any new `#[allow]` exceptions.

### Fixed

- HTTP root-mount panic under `axum` 0.8: `composition::prod` now uses `Router::fallback_service` when `MCP_HTTP_PATH = "/"` (axum 0.7 silently accepted nested `/` mounts; axum 0.8 panics). The fallback path keeps the path tunable via `MCP_HTTP_PATH` (e.g. `/mcp`) without touching the binary code.

### Removed

- ~14k LOC of v3 monolith hard-deleted in H17.5a (commit `95ddc5b`):
  - `src/mcp/tools/` (per-domain tool implementations — moved to `src/application/` + `src/infra/mcp/`).
  - `src/mcp/storage/` (DashMap traits + globals — moved to `src/ports/*_repo.rs` + `src/adapters/repo/dashmap/`).
  - `src/mcp/server.rs` (`McpSshServer` + `#[tool_router]` — moved to `src/infra/mcp/{server,tool_router,resource_handlers}.rs`).
  - `src/mcp/message/` (helpers + per-tool builders — moved to `src/infra/mcp/{helpers,render}/`).
  - `src/mcp/resources.rs` (URI parser + reader handlers — moved to `src/application/{list,read,subscribe,unsubscribe}_resource.rs` + `src/infra/mcp/resource_handlers.rs`).
  - `src/mcp/schema.rs` (JSON schema helpers — replaced by per-tool args structs under `src/infra/mcp/args/`).
  - `src/mcp/keys.rs` (semantic keystroke encoder — relocated to `src/domain/keys.rs`; the encoder now lives in the domain layer).
  - `src/mcp/forward.rs` (port-forward state — moved to `src/domain/forward.rs` + `src/adapters/repo/dashmap/forward.rs`).
- `tests/server_integration.rs` (replaced by the leaner `tests/v4_smoke.rs` which exercises the full composition root).

### Deferred to v4.1 (etapa H17.6)

Shipped — see the [4.1.0] entry above for the H17.6 P1+P2+P3+P4 outcome (src/mcp/ deleted, async-trait dropped, AuthChain decoupled). Cross-adapter SFTP refinements (shared transfer scheduler, per-session SFTP semaphore tuning) remain on the v4.2 backlog.

### Migration

See [docs/MIGRATION_v3_to_v4.md](docs/MIGRATION_v3_to_v4.md) for the contributor migration guide. **No client-side migration is required** — v3 hosts work against v4 servers without any change.

### Etapa trail (v4 commits)

H0 `5a70845`, H1 `e96eca0`, H2 `bf4b446`, H3a `9c48fbc`, H3b `a220897`, H3c `32751ab`, H4 `da5c88d`, H5a `69a9bb7`, H5b `60b0b6d`, H5c `63e76fb`, H5d `abe8b2b`, H6 `679f307`, H6.5 `4e87758`, H7 `3a83f4c`, H8 `a9ea90c`, H9 `c5bd3af`, H10 `86d690e`, scaffold `eb197e3`, H11a `f42cf00`, H11b `d22044c`, H11c `96b8808`, H12a `2b05c8b`, H12b `33b4477`, H12c `a0571d0`, H12d `7149a54`, H13 scaffold `587c91d`, H13a `07d9574`, H13b `8b0e366`, H13c `ceaea1f`, H13d `5a1b0ee`, H13e `b612b24`, H13f `cd0b257`, H13 cleanup `1d997dc`, H14 `7591324`, H15 `b6f9178`, H16 `1c2c889`, H17 `c535992`, H17.5a `95ddc5b`, H18 `ddfbfeb`, H19 `b6f117e`.

## [3.0.0] — 2026-05-02

### Breaking
- Migrate MCP transport layer from `poem-mcpserver` 0.3.1 to `rmcp` 1.6 (official Anthropic Rust SDK). HTTP transport now uses `axum` + `rmcp::transport::streamable_http_server::StreamableHttpService` with `Mcp-Session-Id` header tracking. Stdio transport uses `rmcp::transport::io::stdio()`.
- `ssh_connect.reuse` is now a typed enum `ReusePolicy { Suggest, Auto, ForceNew }` instead of `Option<String>`. Wire format unchanged for valid values; typos now produce a JSON-schema validation error.
- `ssh_commands.status` is now a typed enum `CommandStatus { Running, Completed, Cancelled, Failed }` instead of `Option<String>`.
- All MCP responses are now block-style markdown only (drops the v2 inline `KEY: value | KEY: value` form).
- Stdio binary's custom JSON-RPC quirks (cancel-id parser, fallback responses) removed — handled natively by rmcp. ~250 LOC of workarounds dropped.

### Added
- 5 MCP resource subscribe schemes:
  - `shell://<id>/output` (PTY output, cursor support)
  - `command://<id>/output` (async command stdout/stderr, cursor support)
  - `transfer://<id>/progress` (SFTP point-in-time progress)
  - `session://<id>/health` (session health snapshot)
  - `forward://<id>/events` (port-forward event log, feature-gated)
- `ssh_shell_press` MCP tool — semantic keystrokes (ctrl_a..ctrl_z, enter, tab, escape, backspace, space, delete, arrows, nav keys, F1-F12) with shift/alt/ctrl modifier support and 1..=64 repeat. Tab+Shift produces back-tab (`\x1b[Z`).
- `ssh_shell_wait_for` MCP tool — multi-pattern (up to 16) substring gate with timeout.
- `ssh_shell_read` long-poll extension: `wait` / `wait_timeout_secs` / `min_bytes` parameters for fallback over subscribe.
- Subscription registry (`src/mcp/subscription.rs`) with per-resource debouncer (50 ms coalesce, 1 s force-flush, 30 s keepalive), per-(peer,uri) cursor tracking, sequence numbers per event for gap detection, lagged auto-recovery via snapshot, periodic peer GC.
- 9 new env vars:
  - `SSH_COMMAND_BROADCAST_CAP` (default 1024, floor 16, cap 65536)
  - `SSH_SHELL_BROADCAST_CAP` (default 1024, floor 16, cap 65536)
  - `SSH_TRANSFER_BROADCAST_CAP` (default 256, floor 8, cap 4096)
  - `SSH_SESSION_BROADCAST_CAP` (default 256, floor 8, cap 4096)
  - `SSH_FORWARD_BROADCAST_CAP` (default 256, floor 8, cap 4096; feature-gated)
  - `SSH_NOTIFY_DEBOUNCE_MS` (default 50, floor 5, cap 5000)
  - `SSH_NOTIFY_FORCE_FLUSH_MS` (default 1000, floor 100, cap 60000)
  - `SSH_NOTIFY_KEEPALIVE_S` (default 30, floor 5, cap 300)
  - `SSH_MCP_PEER_GC_INTERVAL_S` (default 30, floor 5, cap 300)
- 6 new docs files: `LLM_GUIDE.md`, `RESOURCES.md`, `ERRORS.md`, `LOCKS.md`, `MIGRATION_v2_to_v3.md`, `adr/0001-migrate-to-rmcp.md`.
- 4 new Python stress test scripts.
- ~145 new Rust unit + integration tests; 8 loom invariant tests (gated, blocked by upstream tokio/loom incompatibility in russh+axum — documented).

### Changed
- Lock-free refactor of all hot-path state types:
  - `RunningCommand` — `ArcSwap<OutputBuffer>` + `broadcast::Sender<OutputChunk>` + `OnceCell<i32>` exit_code + `OnceCell<String>` error.
  - `RunningShell` — `ArcSwap<RingBuffer>` + `broadcast::Sender<Bytes>` + `mpsc::Sender<WriteRequest>` (writer task owns ChannelWriter exclusively) + `AtomicU64` last_activity_ms + `Notify` data_notify.
  - `RunningTransfer` — `OnceCell<String>` error + `broadcast::Sender<ProgressEvent>` + `Notify`.
  - `SessionRef` — `broadcast::Sender<HealthEvent>`.
  - `ForwardHandle` — `broadcast::Sender<ForwardEvent>` (feature-gated).
- 0 `Mutex` fields on hot-path state types after this release (verified via `grep`).
- Strict clippy baseline expanded with v3 lock-free invariants: `await_holding_lock`, `await_holding_refcell_ref`, `significant_drop_in_scrutinee`, `significant_drop_tightening`, `mutex_atomic`, `mutex_integer`.
- Tool count: 16 → 18.
- Test count: 502 → 832 (lib + integration).

### Fixed
- `ssh_exec_output.command_id` doc no longer references the non-existent `ssh_execute_async` tool.
- `ssh_disconnect`/`ssh_shell_close` parameter docs now use the canonical "_ID returned from X" phrasing.

### Removed
- `poem`, `poem-mcpserver` deps.
- `src/mcp/commands.rs` (the v2 monolithic 2272-LOC tools file) — split into `src/mcp/tools/{connection,execute,shell,sftp,forward,legacy_helpers}.rs`.
- The custom stdio cancel/fallback shim (~250 LOC).

### Migration
See `docs/MIGRATION_v2_to_v3.md` for client upgrade instructions.

### Commit hashes (v3 etapa trail)
- E1 `3f65152` rmcp foundation
- E3 `a956191` ssh_connect canary
- E4 `e17be5d` 15 remaining tools
- E7 `ce8497d` RunningCommand lock-free
- E8 `4bee2c9` RunningShell lock-free
- E9 `21b8fe1` RunningTransfer + Session/Forward broadcast
- E10 `f9652c7` keys + ssh_shell_press
- E11 `83aa193` long-poll + ssh_shell_wait_for
- E12 `d87980d` subscription registry + backpressure
- E13 `d2798ff` resources.rs URI handlers
- E14 `f7b7683` ServerHandler wiring + peer GC
- E6 `b96d724` format consistency
- E15 `545a680` Rust unit + loom + integration tests
- E16 `6fbc97f` Python integration + stress
- E17 `e3051f6` docs rewrite
- E18 `a197422` docs new (LLM/RESOURCES/ERRORS/LOCKS/MIGRATION/ADR)

## [2.0.1] — 2025-10-19

See `git log` for the v2.x changes; this CHANGELOG was introduced in v3.0.0.

[4.8.1]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.8.1
[4.8.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.8.0
[4.7.1]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.7.1
[4.7.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.7.0
[4.6.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.6.0
[4.5.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.5.0
[4.4.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.4.0
[4.3.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.3.0
[4.2.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.2.0
[4.1.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.1.0
[4.0.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v4.0.0
[3.0.0]: https://github.com/farchanjo/ssh-mcp/releases/tag/v3.0.0
[2.0.1]: https://github.com/farchanjo/ssh-mcp/releases/tag/v2.0.1
