---
id: 01a20145-f992-79f2-bb26-d86cdc4c4b2b
number: 001
slug: russh-advisory-upgrade
status: implemented
created_at: 2026-11-22T03:21:32.818530498Z
---
# Feature Specification: Russh Advisory Upgrade

Feature: 001-russh-advisory-upgrade
Created: 2026-11-22

Every fact below is measured on 2026-11-22 and names the probe log that carries
it, unless the sentence says the fact comes from the upstream release notes of
`russh` or `russh-sftp` and was not measured here.

## User Stories

- As the maintainer of the SSH transport, I want the process to run on a
  `russh` release without published advisories, so that a protocol or
  memory-safety flaw in the SSH client is not reachable from a remote host on
  every session.
- As an operator, I want the host-key decision of a plain host key to stay
  exactly as it is today, so that a dependency move does not change which hosts
  are trusted.
- As a reviewer, I want the move to arrive as one change that leaves the
  published MCP surface untouched, so that a security update does not carry a
  capability review.
- As a maintainer, I want the channel-open retry classification to be proven
  against the library that ships, so that a change in the rendered error text
  cannot change retry behaviour while the tests stay green.

## Out of Scope

- Validating a host certificate chain. This feature rejects a certificate; it
  does not authenticate one. Certificate validation is a feature of its own.
- Every change to a preferred algorithm list, on the client side or on the
  server side.
- The `rmcp` upgrade that closes RUSTSEC-2026-0462 on `rmcp 1.8.0`, and every
  other dependency bump.
- The defects recorded in `session.md`.

## Functional Requirements

### FR-001 The manifest and the lock resolve russh 0.65.x and russh-sftp 4.x

`Cargo.toml:96-97` asks for `russh = "0.60"` and `russh-sftp = "2"`, and the
current `Cargo.lock` resolves `russh 0.60.3` and `russh-sftp 2.4.0`. One
`cargo update -p russh -p russh-sftp` moves both to `russh 0.65.4` and
`russh-sftp 4.0.0`, and no other direct dependency moves its declared
requirement (probe `/tmp/ssh-mcp-russh-probe/update.log`).

**Acceptance.** `Cargo.toml` declares the `russh` 0.65 line and the
`russh-sftp` 4 line; the resolved `Cargo.lock` carries `russh` 0.65.4 or a
later 0.65.x and `russh-sftp` 4.x; the requirement of every other direct
dependency is unchanged.

### FR-002 The resolved lock carries no advisory on the SSH transport

`cargo audit` on the moved lock reports one vulnerability and one warning: the
vulnerability is RUSTSEC-2026-0462 on `rmcp 1.8.0` and the warning is the
unmaintained `chrono 0.4.45` under RUSTSEC-2026-0543
(`/tmp/ssh-mcp-russh-probe/audit.log`). The six advisories the current lock
carries are gone: RUSTSEC-2026-0492, RUSTSEC-2026-0493, RUSTSEC-2026-0494 and
RUSTSEC-2026-0495 on `russh 0.60.3`, each of them 7.5 high, plus
RUSTSEC-2023-0071 on `rsa 0.10.0-rc.16` and RUSTSEC-2026-0387 on
`crypto-bigint 0.7.0-rc.28`, which arrive transitively through `russh` and are
not declared in the manifest. The move is the cure TM-021 of
`doc/arch/threat-model/threat-model.md` names, for the `russh` half of it.

**Acceptance.** `cargo audit` on the resolved dependency set prints no advisory
for `russh`, `rsa` or `crypto-bigint`, and the only remaining vulnerability it
prints is RUSTSEC-2026-0462 on `rmcp`.

### FR-003 The crate compiles with the handler on the new trait signature

The move breaks exactly one compile: `cargo check --all-features --all-targets`
fails with `src/adapters/ssh/internal/session.rs:124:28: error[E0053]: method
check_server_key has an incompatible type for trait: expected
PublicKeyOrCertificate, found russh::keys::PublicKey`
(`/tmp/ssh-mcp-russh-probe/check.log`). With the override of
`check_server_key` adapted to the new argument type, the same check exits 0
(`/tmp/ssh-mcp-russh-probe2/check2.log`). The override sits at
`src/adapters/ssh/internal/session.rs:122-131`.

**Acceptance.** `cargo check --all-features --all-targets` exits 0, and the
only production source line the move forces is the argument type of
`check_server_key` and the decision inside it.

### FR-004 The client offers no host-certificate algorithm, and rejects a certificate

The client configuration offers no host-certificate algorithm, and it states
that in its own builder instead of inheriting a library default.
`russh::Preferred::host_key_certificates` is the field that governs it: its
documentation calls it "Host-key algorithms for which to also advertise the
OpenSSH certificate variant (`*-cert-v01@openssh.com`)", records that the
certificate names are "advertised ahead of `key`, so a server that has a
certificate presents it in preference to a bare key", and states that the list
is "Empty by default" and that "turning it on is the caller's decision, never a
default" (`https://docs.rs/russh/0.65.4/russh/struct.Preferred.html`). The field
sits on `russh::client::Config::preferred`, whose documentation is "Lists of
preferred algorithms" (`https://docs.rs/russh/0.65.4/russh/client/struct.Config.html`).
The project builds that configuration in `build_client_config`
(`src/adapters/ssh/internal/client.rs:89-118`), which forwards `compression`
into a `Preferred` with `..Default::default()` at `:100-103` and puts it on the
`Config` with `..Default::default()` at `:111-117`. A test pins the offered
algorithm list, so a server that holds both a plain host key and a host
certificate keeps serving the plain key to this client.

When a server presents a certificate anyway, `check_server_key` rejects the
connection. The client validates no certificate chain, so the key inside a
certificate is never checked against `known_hosts`, and no record is written for
it. The rejection emits one `warn` line carrying the host and the port as
structured fields, in the shape of the existing host-key rejections
(`src/adapters/ssh/internal/session.rs:70-86`,
`doc/arch/observability/observability.md:144-147`).

**Acceptance.** A new test proves that a certificate is rejected and that the
rejection writes no `known_hosts` record; a second test proves that the
configuration the client builds offers no host-certificate algorithm, with the
list set empty in `build_client_config` rather than left to the library default.

### FR-005 The plain host-key decision is unchanged

For a plain host key, `verify_host_key`
(`src/adapters/ssh/internal/session.rs:59-88`) keeps today's three outcomes: an
unknown host is learned through TOFU and recorded (`:90-116`), a recorded key
that differs is rejected with the mismatch warning, and the insecure skip flag
accepts every key. The three tests that pin the outcomes stay green:
`skip_flag_accepts_any_host_key` (`:154`), `recorded_key_mismatch_is_rejected`
(`:167`) and `unknown_host_is_learned_via_tofu` (`:190`).

**Acceptance.** The three named tests pass unchanged, and the adapted
`check_server_key` passes a plain host key to `verify_host_key` with no new
branch.

### FR-006 The channel-open retry classification reads the typed error, and a resource shortage is transient

`handle_open_error` renders the failure with `err.to_string()` and hands the
rendering to `is_transient_open_error`, which matches substrings of the lowercase
text (`src/adapters/ssh/internal/client.rs:664-694`). The matcher has two
production callers, both of which already hold a `russh::Error` value:
`open_session_with_retry` (`:526`, the synchronous path shared with the SFTP
transfer path, 25 attempts on the 100 ms to 1 s backoff of `:534-544`) and
`handle_open_error` (`:672`, the asynchronous path). The other five references
are the assertions of the `transient_open_errors` tests (`:1398-1414`).

The classification becomes a function of the typed error:

- `russh::Error::ChannelOpenFailure` with reason `ConnectFailed`, reason
  `AdministrativelyProhibited` or reason `ResourceShortage`: transient.
- reason `UnknownChannelType`, reason `Other { .. }`, and every other
  `russh::Error`: not transient.

Text supplied by the peer never decides a retry. This is a behaviour change on
both sides of that line, and it is deliberate:

- A real `ResourceShortage` refusal is now retried on the existing 25-attempt
  schedule instead of failing at once. That is the behaviour the repository
  already documents and never delivered: `CHANGELOG.md:24`,
  `CHANGELOG.md:71`, `docs/MIGRATION.md:884`, the doc comment at
  `src/adapters/ssh/internal/client.rs:506-510` and the comment at `:686-687`
  all state that a `RESOURCE_SHORTAGE` refusal is transient and is retried,
  while the substring match never classified the typed reason that way.
- A `reason` string inside `ChannelOpenFailure::Other` can no longer trigger a
  retry, where the substring match let text an untrusted peer supplies choose
  one.

Both facts are measured, with the rendered text and the class of every relevant
value under both library versions, in
`doc/arch/sdd/001-russh-advisory-upgrade/research.md`, section 4.

`russh::ChannelOpenFailure` carries five variants — `AdministrativelyProhibited`,
`ConnectFailed`, `UnknownChannelType`, `ResourceShortage` and
`Other { code: u32, reason: String }` — and the enum is not `#[non_exhaustive]`
(`https://docs.rs/russh/0.65.4/russh/enum.ChannelOpenFailure.html`).

**Acceptance.** A test builds each of those real `russh::Error` values through
the moved library and asserts its class: the three exhaustion reasons transient;
`UnknownChannelType`, `Other { .. }`, `Disconnect` and `HUP` not transient. It
feeds no hand-written string to the matcher, so it fails if the classification of
any of those values changes.

### FR-007 The project gate is green on the new library

The verified-change gate of P-06 (`doc/arch/memory/constitution.md:109-112`)
passes on the moved library: `cargo fmt --all -- --check`,
`cargo clippy --all-features -- -D warnings`, `cargo nextest run --lib` and
`cargo nextest run --tests --features test-fixtures`. The reference measured on
the unchanged code is 2114 passing lib tests and 2281 passing with the
integration suites. No lint is disabled and no test is ignored or removed
(P-03, `doc/arch/memory/constitution.md:97-100`).

**Acceptance.** Each of the four commands exits 0; the lib run reports at least
2114 passing tests and the test-fixtures run at least 2281; no lint reaches an
allow list, no test carries a new `#[ignore]` and no test file is deleted.

### FR-008 The wire behaviour of the new library passes the Python integration suites

No Rust suite of this repository drives a real SSH or SFTP server. The Python
suites under `scripts/` drive the built binaries against an SSH endpoint, and
they are the regression signal for the wire behaviour of the new library. What
they need to run, from the repository's own files:

- **pytest, with the per-test timeout.** `pytest.ini:6-7` sets `timeout = 60`
  with `timeout_method = thread`, `:10` adds `-ra --strict-markers`, and `:13`
  registers the `requires_sshd` marker the suites use.
- **The package binaries, resolved under the active cargo profile directory.**
  `scripts/helpers/paths.py:36-38` builds `target/<profile>`, and `:58-70`
  names the three binaries: `ssh-mcp` (HTTP), `ssh-mcp-stdio` and
  `ssh-mcp-tail`. The profile is `SSH_MCP_BUILD_PROFILE` and defaults to
  `debug` (`:27`, `:30-33`); `SSH_MCP_BIN`, `SSH_MCP_STDIO_BIN` and
  `SSH_MCP_TAIL_BIN` override one binary each (`:41-55`). `scripts/run_all.sh`
  builds with `cargo build --bins` (`:21-22`), exports the three paths from
  `target/debug` (`:29-40`) and runs the suites with
  `python3 -m pytest scripts/test_*.py -v` (`:42`).
- **An SSH endpoint, which needs no system `sshd`.** `scripts/helpers/fixtures.py:195-221`
  takes the endpoint from `SSH_MCP_TEST_TARGET` when that variable is set, and
  otherwise boots the in-process fixture `scripts/helpers/local_sshd.py`: a
  `paramiko.Transport` on a locally bound `127.0.0.1:<random_port>`
  (`local_sshd.py:1-6`) serving password auth and a per-fixture ed25519
  keypair (`:8-9`), and answering `exec_request`, `pty_request` plus
  `shell_request`, and `subsystem_request("sftp")` (`:14-16`). Without
  `paramiko`, and with `SSH_MCP_TEST_TARGET` unset, the fixture skips
  (`fixtures.py:212`).
- **`paramiko` and `cryptography`** for that fixture (`local_sshd.py:48-51`,
  `:76-77`).

The suites that connect, execute commands, open shells, transfer files and run
rsync over SFTP pass: `scripts/test_http.py` over the `ssh-mcp` HTTP binary
(`http_client` on the `http_server` fixture, `fixtures.py:133-141`, `:160`),
`scripts/test_transfer_progress.py` over `ssh-mcp-stdio` (`stdio_client`,
`fixtures.py:176-180`) and `scripts/test_v7_rsync_http.py` over the HTTP
binary, each marked `requires_sshd` (`scripts/test_http.py:99`,
`scripts/test_transfer_progress.py:48`, `scripts/test_v7_rsync_http.py:48`).
`docs/DEVELOPMENT.md:138` describes this suite as needing a real OpenSSH
server; the fixture chain above is the whole truth, and the in-process paramiko
server serves whenever `SSH_MCP_TEST_TARGET` is unset.

**Acceptance.** The three named suites exit 0 under pytest on the
debug-profile binaries, against `SSH_MCP_TEST_TARGET` or the in-process
fixture.

### FR-009 The published MCP surface does not change

The tool catalogue, every tool argument, every response line of the
block-markdown envelope, the structured twin, the error taxonomy and every
environment variable keep their names and defaults: no tool, argument, response
line, error code or environment variable is added, removed or renamed (P-04 at
`doc/arch/memory/constitution.md:101-104`, P-05 at `:105-108`).

**Acceptance.** The render tests that pin the envelope stay green, and a
comparison of the tool catalogue and of the environment-variable table against
the current release shows no addition, removal or rename.

### FR-010 The change is recorded

`CHANGELOG.md` records the release that carries the move, `docs/MIGRATION.md`
records what a consumer of the previous release has to know, and entry TM-021
of `doc/arch/threat-model/threat-model.md` records that its cure landed and what
stays open in that entry (RUSTSEC-2026-0462 on `rmcp`, the unmaintained warning
on `chrono`).

**Acceptance.** Each of the three named files carries an entry naming `russh`
0.65.x, `russh-sftp` 4.x and the advisories this feature closes.

### FR-011 The adapter calls no session-level write, the one SFTP contract that changed

The upstream release notes of `russh-sftp 4.0.0` state that the client defaults
moved — `max_concurrent_writes` from 8 to 16, a new `max_concurrent_reads` of
16 and a new `max_write_packet_len` — that the request timeout starts when the
request is written, and that file types are decoded as values. Those four facts
come from the release notes and no test here measures them.

The session-level function whose documented contract changed is
`SftpSession::write`, and the adapter calls it nowhere in `src/adapters/sftp/`
or `src/adapters/ssh/`. The session-level calls the adapter does make are
`metadata` (`src/adapters/sftp/internal/sftp.rs:582`, `:603`;
`src/adapters/sftp/russh_sftp_adapter.rs:1126-1127`), `create`
(`src/adapters/sftp/internal/sftp.rs:915`), `open_with_flags`
(`src/adapters/sftp/internal/sftp.rs:941`;
`src/adapters/sftp/rsync_fs_impl.rs:232-233`, `:276-277`), `open`
(`src/adapters/sftp/internal/sftp.rs:1200`), `read_dir`
(`src/adapters/sftp/rsync_fs_impl.rs:88-89`), `symlink_metadata`
(`src/adapters/sftp/rsync_fs_impl.rs:116-117`), `read_link`
(`src/adapters/sftp/rsync_fs_impl.rs:135-136`), `create_dir`
(`src/adapters/sftp/rsync_fs_impl.rs:150`), `remove_dir`
(`src/adapters/sftp/rsync_fs_impl.rs:171`), `remove_file`
(`src/adapters/sftp/rsync_fs_impl.rs:180`), `symlink`
(`src/adapters/sftp/rsync_fs_impl.rs:202`), `set_metadata`
(`src/adapters/sftp/rsync_fs_impl.rs:161`, `:217`) and `close`
(`src/adapters/sftp/rsync_fs_impl.rs:106`, `:129`, `:139`, `:165`, `:174`,
`:183`, `:205`, `:220`, `:257`, `:291`).

The file-level calls on an opened file are a different surface and stay:
`write_all` on that file (`src/adapters/sftp/internal/sftp.rs:1105`;
`src/adapters/sftp/rsync_fs_impl.rs:285`, through
`tokio::io::AsyncWriteExt::write_all`), `read`
(`src/adapters/sftp/internal/sftp.rs:1265`;
`src/adapters/sftp/rsync_fs_impl.rs:245`), `seek`
(`src/adapters/sftp/internal/sftp.rs:961`, `:973`;
`src/adapters/sftp/rsync_fs_impl.rs:237`, `:281`) and `shutdown`
(`src/adapters/sftp/internal/sftp.rs:987`, `:1044`;
`src/adapters/sftp/rsync_fs_impl.rs:256`, `:288`). The new defaults apply as the
library states them, with no override from this project.

**Acceptance.** A scan of `src/adapters/sftp/` finds no call to
`SftpSession::write` and no call into another function whose contract changed,
and the transfer suites of FR-008 pass with the library defaults in force.

## Security Requirements

- **Data sensitivity/classification.** The feature reads and writes no data of
  its own: it moves a library version, and the only data path it touches is the
  host-key material the SSH handshake carries and the `known_hosts` record the
  existing TOFU path writes.
- **Authentication/authorization.** The feature changes one authentication
  decision and narrows it: a certificate the client does not validate is
  rejected instead of trusted, while a plain host key keeps the three outcomes
  the current tests pin.
- **Input validation.** The untrusted input is the host-key material a server
  presents on every handshake, and the feature bounds it on both sides: the
  client offers no certificate algorithm to invite one, and a certificate form
  is refused before any trust decision is made.
- **Cryptography in transit/at rest.** The transport cryptography belongs to
  the library; the feature moves it to the release the advisories name as
  fixed, and it adds no cipher, no key format and no stored secret of its own.
- **Logging/audit.** The feature adds one `warn` line for a rejected
  certificate, carrying the host and the port as structured fields and no key
  material, which matches the convention of the existing host-key rejections.
- **Error-handling information exposure.** The rejection keeps the existing
  shape: the log names the host, the port and, on a lookup failure, the error,
  and no path prints key bytes, a fingerprint or the `known_hosts` path.

## Acceptance Scenarios

Given the moved dependency set with `russh` 0.65.x and `russh-sftp` 4.x
When  `cargo audit` runs over it
Then  no advisory names `russh`, `rsa` or `crypto-bigint`, and the only
remaining vulnerability is RUSTSEC-2026-0462 on `rmcp 1.8.0` (FR-002).

Given the client configuration the project builds
When  the host-key algorithms it offers are read
Then  no OpenSSH certificate variant is offered, so a server that holds both a
plain key and a certificate serves the plain key (FR-004).

Given a server that presents a host certificate anyway
When  the client checks the server key
Then  the connection is rejected, one `warn` line names the host and the port,
and no `known_hosts` record is written for the certificate (FR-004).

Given a plain host key and the existing `known_hosts` file
When  the three host-key tests run
Then  `skip_flag_accepts_any_host_key`, `recorded_key_mismatch_is_rejected` and
`unknown_host_is_learned_via_tofu` pass unchanged (FR-005).

Given the channel-open errors the moved library produces
When  the retry classification runs its test
Then  the three exhaustion reasons are transient and every other value,
including a peer-supplied reason string, is not (FR-006).

Given the moved library and the current source tree
When  the four gate commands run
Then  each exits 0, with no lint disabled and no test ignored or removed
(FR-007).

Given a binary built on the moved library and the local SSH server fixture
When  the suites `scripts/test_http.py`, `scripts/test_transfer_progress.py` and
`scripts/test_v7_rsync_http.py` run
Then  each exits 0 (FR-008).

Given an MCP host of the current release
When  it completes `initialize` and calls the tools
Then  the catalogue, the arguments, the response lines, the error codes and the
environment variables are the ones it already knows (FR-009).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  no rule fires that did not fire before the feature, and the error count
stays at or below the baseline of 70.

Given the feature landed
When  a reader opens `CHANGELOG.md`, `docs/MIGRATION.md` or entry TM-021
Then  each names `russh` 0.65.x, `russh-sftp` 4.x and what stays open (FR-010).

## Accepted Risks

- **AR-1 The four behaviour changes the upstream release notes state for
  `russh` 0.65 carry no test here.** The client refuses a host key whose
  algorithm is outside the preferred list, the inactivity timeout also applies
  to a stalled write, decompression is bounded, and `Channel::data()` applies
  backpressure. No Rust suite of this repository drives a real SSH server, so
  none of the four is proven here; they arrive as part of the release the
  feature adopts. Accepted: proving them needs a test harness the repository
  does not have, and the regression signal is the Python suites of FR-008
  against the local fixture.
- **AR-2 The certificate rejection is proven by a unit test only.** No
  end-to-end test presents a host certificate, because the `local_sshd` fixture
  serves a plain host key. Accepted for the same reason.
- **AR-3 The `russh-sftp 4.0.0` default moves are adopted with nothing pinning
  the new numbers.** No test states the concurrency, the packet length or the
  point at which the request timeout starts. Accepted: the numbers are the
  library's own defaults and the project overrides none of them (FR-011).
- **AR-4 `ssh-key 0.7.0-rc.12` stays a pre-release cryptography crate.** It is
  required by `russh 0.65.4` itself and is the only pre-release cryptography
  crate the moved graph carries (`/tmp/ssh-mcp-russh-probe/update.log`).
  Accepted: the manifest cannot remove a dependency the adopted release
  declares, and the alternative is a vendored fork.
- **AR-5 `rmcp 1.8.0` keeps RUSTSEC-2026-0462 and `chrono` keeps its
  unmaintained warning.** Both belong to a later dependency feature and stay
  open by decision, so that this feature remains one reviewable change
  (Out of Scope).
- **AR-6 `speckit verify` resolves a citation only to a `#[test]` function, so
  four proofs cannot be bound.** The four proofs of FR-004 and FR-005 are
  `#[tokio::test]` functions: `skip_flag_accepts_any_host_key`
  (`src/adapters/ssh/internal/session.rs:164`), `recorded_key_mismatch_is_rejected`
  (`:177`), `unknown_host_is_learned_via_tofu` (`:200`) and
  `certificate_is_rejected_and_not_recorded` (`:218`). A citation to any of them
  fails with `citation
  crate::adapters::ssh::internal::session::tests::<name> does not resolve: no
  #[test] function of that name exists in the tree`, so the FR-005 scenario and
  the FR-004 certificate scenario stay `@planned` instead of bound; only the
  FR-004 configuration proof is a `#[test]` function and is bound. The four
  proofs run in the nextest gate (`cargo nextest run --lib`), which is what
  proves them. Accepted: rewriting them as `#[test]` with a hand-built runtime
  would depart from the pattern the module and the project already use.

## Observability

The feature adds no metric, no counter, no NDJSON event and no trace span, and
it changes no target, no log format and no destination. The one new signal is
the `warn` line of FR-004, which reports a rejected host certificate with the
host and the port as structured fields, following the level and message
conventions of `doc/arch/observability/observability.md`: `warn` marks a
degraded but survivable path, and the variable parts ride as fields rather than
inside the message text (`doc/arch/observability/observability.md:144-147`,
`:158-163`). The rejection of a changed plain host key keeps its current line
and its current level.

## Clarifications

### Session 2026-11-22

- **The SFTP call surface is wider than the first draft of FR-011 said.** The
  adapter makes session-level calls to `metadata`, `create`, `open`,
  `open_with_flags`, `read_dir`, `symlink_metadata`, `read_link`, `create_dir`,
  `remove_dir`, `remove_file`, `symlink`, `set_metadata` and `close`, and
  file-level calls to `read`, `write_all`, `seek` and `shutdown`. The one
  session-level function whose documented contract changed in
  `russh-sftp 4.0.0` is `write`, and the adapter calls it nowhere. Resolved:
  FR-011 states that, and the false call list is gone from its scenario.
- **The certificate decision does not rest on a library default.** The
  documentation of `russh::Preferred::host_key_certificates` states the list is
  "Empty by default" and that "turning it on is the caller's decision, never a
  default" (`https://docs.rs/russh/0.65.4/russh/struct.Preferred.html`), while
  `build_client_config` (`src/adapters/ssh/internal/client.rs:89-118`) builds
  its `Preferred` with `..Default::default()`. Resolved: the project sets the
  list empty in its own builder and a test pins the offered algorithm list, so a
  server that holds both a plain host key and a certificate keeps serving the
  plain key to this client.
- **The retry classification is repaired to the intent the repository already
  documents.** FR-006 now states the classification over the typed error, with a
  `ResourceShortage` refusal transient and every peer-supplied reason string
  inert. The measurement behind it is in `research.md`, section 4.
- **Decision provenance for the classification repair.** Repairing it was
  decided in the session that wrote this specification, on the instruction to
  carry the whole plan forward, defect fixes included, and it is recorded here
  for the owner's review. The change is a behaviour change and is stated as one
  in FR-006.
- **The Python suites run under pytest on the debug-profile binaries.** FR-008
  now names what they need, with the citation for each need.
- **Decision provenance.** Rejecting host certificates was decided in the
  session that wrote this specification, on the instruction to take the whole
  plan forward, and it is recorded here for the owner's review. The owner has
  not been asked to choose between rejecting a certificate and validating a
  chain; validating a chain stays a possible later feature, and the Out of Scope
  section excludes it from this feature.
