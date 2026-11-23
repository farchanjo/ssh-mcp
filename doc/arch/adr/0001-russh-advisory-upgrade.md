---
status: accepted
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Russh Advisory Upgrade

## Context and Problem Statement

The dependency set the current `Cargo.lock` resolves carries published
advisories on the crate that carries the SSH transport. Measured with
`cargo audit` on that lock on 2026-11-22: RUSTSEC-2026-0492, RUSTSEC-2026-0493,
RUSTSEC-2026-0494 and RUSTSEC-2026-0495 on `russh 0.60.3`, each of them 7.5
high, plus RUSTSEC-2023-0071 on `rsa 0.10.0-rc.16` and RUSTSEC-2026-0387 on
`crypto-bigint 0.7.0-rc.28`, the last two arriving transitively through `russh`.
`Cargo.toml:96-97` asks for `russh = "0.60"` and `russh-sftp = "2"`. Entry
TM-021 of `doc/arch/threat-model/threat-model.md` records the exposure and names
its cure: a dependency move to `russh >= 0.65.0` for the SSH transport. Nobody
had made the move.

The move is not source-compatible on one line. With `russh 0.65.4` and
`russh-sftp 4.0.0` resolved, `cargo check --all-features --all-targets` fails
with one error: `src/adapters/ssh/internal/session.rs:124:28: error[E0053]:
method check_server_key has an incompatible type for trait: expected
PublicKeyOrCertificate, found russh::keys::PublicKey` (probe log
`/tmp/ssh-mcp-russh-probe/check.log`). The trait hands the handler a value that
is either a plain public key or a certificate, so the handler has to state what
a certificate means to this server. That statement is the second half of this
record.

Two ADR series now exist in this repository: the pre-speckit series under
`docs/adr/`, numbered `0001` to `0012`, which principle P-08 of the constitution
names (`doc/arch/memory/constitution.md:117-120`), and the speckit series under
`doc/arch/adr/`, which starts at this record. Unifying the two series is left to
the conformity work, and nothing moves or renumbers today.

## Decision Drivers

- The advisories are published and the fixed line is known, and the crate sits
  directly on TB-1 and TB-2 (`doc/arch/threat-model/threat-model.md:64-76`), so
  the exposure is reachable from an unauthenticated caller and from a remote
  SSH host.
- The host-key decision is a security control with tests of its own
  (`src/adapters/ssh/internal/session.rs:154-206`), and a library move must not
  change it in silence.
- The published MCP surface is a frozen contract (P-04 at
  `doc/arch/memory/constitution.md:101-104`, P-05 at `:105-108`), so the change
  stays inside the SSH adapter.
- One concern per change (D-12, `doc/arch/memory/constitution.md:74-77`): one
  dependency move, plus the single source line it forces.

## Considered Options

- Option A: move both crates with one `cargo update -p russh -p russh-sftp` to
  `russh 0.65.4` and `russh-sftp 4.0.0`, adapt `check_server_key`, and reject a
  host certificate.
- Option B: stay on `russh 0.60.x` and lean on a deployment control — a network
  path that reaches only trusted hosts, or a curated host set — until a later
  release.
- Option C: replace the SSH client crate.
- Option D: move `russh`, `russh-sftp` and `rmcp` in one change.

## Decision Outcome

Chosen option: "Option A", because it closes the four advisories the SSH
transport carries with the smallest change the resolver allows: one
`cargo update` for two crates, with no other direct dependency moved
(`/tmp/ssh-mcp-russh-probe/update.log`), and one adapted method, after which the
same check exits 0 (`/tmp/ssh-mcp-russh-probe2/check2.log`). Option B leaves the
four advisories open on a crate every session exercises, which is the finding
TM-021 already records. Option C replaces the adapter layer and its tests for a
problem the fixed release solves. Option D mixes two dependency surfaces, and
RUSTSEC-2026-0462 on `rmcp` belongs to its own feature.

A host certificate is rejected, fail closed. The client configuration offers no
host-certificate algorithm, and it states that in its own builder instead of
inheriting a library default: `russh::Preferred::host_key_certificates` is the
governing field, its documentation records that a certificate name is
"advertised ahead of `key`, so a server that has a certificate presents it in
preference to a bare key" and that the list is "Empty by default" with turning
it on "the caller's decision, never a default"
(`https://docs.rs/russh/0.65.4/russh/struct.Preferred.html`), while
`build_client_config` (`src/adapters/ssh/internal/client.rs:89-118`) currently
forwards `preferred` with `..Default::default()`. Setting the list empty in that
builder keeps a server that holds both a plain host key and a certificate
serving the plain key. The client validates no certificate chain, so the key
inside a certificate is never checked against `known_hosts` and no record is
written for it. The rejection is logged at `warn` with the host and the port as
structured fields, following the convention of the existing host-key rejections.
A plain host key keeps today's three outcomes with no new branch: an unknown host
is learned through TOFU, a recorded key that differs is rejected, and the
insecure skip flag accepts.

Rejecting a host certificate is the decision this record takes, and it was taken
in the session that wrote the record, on the instruction to carry the whole plan
forward. It is written down here for the owner's review. The owner has not been
asked to choose between rejecting a certificate and validating a chain:
validating a chain stays a possible later feature and is outside this record.

The change also repairs one classification defect that the move exposed and did
not cause. The channel-open retry matcher read substrings of the rendered error
text, and it never classified a real `ResourceShortage` refusal as transient,
while `CHANGELOG.md:24`, `CHANGELOG.md:71`, `docs/MIGRATION.md:884` and the
comments at `src/adapters/ssh/internal/client.rs:506-510` and `:686-687` all
state that such a refusal is retried. The classification becomes a function of
the typed error: transient for `ConnectFailed`, `AdministrativelyProhibited` and
`ResourceShortage`, and not transient for every other value, so a `reason` string
an untrusted peer supplies can no longer trigger a retry. Repairing it was
decided in the session that wrote this record, on the instruction to carry the
whole plan forward, defect fixes included, and it is recorded here for the owner's
review.

### Consequences

- Good: the four advisories on `russh` and the two on `rsa` and `crypto-bigint`
  leave the resolved set. `cargo audit` on the moved lock reports one
  vulnerability, RUSTSEC-2026-0462 on `rmcp 1.8.0`, and one unmaintained
  warning, `chrono 0.4.45` (`/tmp/ssh-mcp-russh-probe/audit.log`).
- Good: the certificate decision becomes explicit and testable, so the
  repository states what it does with a certificate instead of inheriting a
  default from the library.
- Bad: a host that answers with a certificate in preference to its bare key is
  unusable until a certificate-validation feature exists. An operator whose
  fleet uses host certificates must serve a plain host key for this client.
- Bad: `russh-sftp 4.0.0` changes the documented contract of the session-level
  `write`, alongside its client default moves and the point at which the request
  timeout starts. The adapter calls `write` nowhere, so the change lands on a
  surface this project does not use, and the file-level `write_all` on an opened
  file is a different function that stays.
- Good: a `RESOURCE_SHORTAGE` channel-open refusal is retried as the changelog and
  the migration guide already claim, on the 25-attempt, 100 ms to 1 s schedule the
  synchronous path carries.
- Bad: the repair makes a retry happen where a refusal used to fail at once, so a
  peer that stays out of slots keeps a call alive for the backoff window. In the
  other direction a peer can no longer steer a retry with a `reason` string, which
  is the intended change and is observable only in the failing path's timing.
- Bad: `ssh-key 0.7.0-rc.12` is required by `russh 0.65.4` and stays the only
  pre-release cryptography crate the moved graph carries
  (`/tmp/ssh-mcp-russh-probe/update.log`).
- Bad: the move adopts four behaviour changes the upstream release notes state —
  a host key outside the preferred algorithm list is refused, the inactivity
  timeout also covers a stalled write, decompression is bounded, and
  `Channel::data()` applies backpressure — and no test in this repository
  exercises a real SSH server. The regression signal is the Python suites
  against the local `sshd` fixture (`scripts/helpers/local_sshd.py`), and the
  four are recorded as accepted risks in the feature specification
  (`doc/arch/sdd/001-russh-advisory-upgrade/spec.md`).
