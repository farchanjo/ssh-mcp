# Implementation Plan: Russh Advisory Upgrade

## Overview

Feature 001 moves the SSH client library from `russh 0.60.3` and
`russh-sftp 2.4.0` to `russh 0.65.4` and `russh-sftp 4.0.0`, which is the cure
entry TM-021 of `doc/arch/threat-model/threat-model.md` names for the four
advisories the current lock carries on `russh`. The move is one `cargo update`
for two crates plus the one source line the new trait signature forces, and it
closes six advisories while leaving the published MCP surface untouched.

The work is one coordinating change: two manifest lines, one handler method, one
client-configuration line, and the tests and records that prove them. Nothing
outside the file map in this plan is in scope for the implement phase.

## Technical Approach

### The handler (FR-003, FR-004, FR-005)

`client::Handler::check_server_key` in `russh 0.65.4` takes
`&russh::keys::PublicKeyOrCertificate` and returns
`impl Future<Output = Result<bool, Self::Error>> + Send`
(`https://docs.rs/russh/0.65.4/russh/client/trait.Handler.html`). The enum has
two variants, `PublicKey { key: PublicKey, hash_alg: Option<HashAlg> }` and
`Certificate(Certificate)`, it is not `#[non_exhaustive]`, and it exposes
`public_key() -> PublicKey` and `certificate() -> Option<&Certificate>`
(`https://docs.rs/russh/0.65.4/russh/keys/enum.PublicKeyOrCertificate.html`).

The override in `src/adapters/ssh/internal/session.rs:122-133` becomes:

```rust
    async fn check_server_key(
        &mut self,
        server_public_key: &keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if self.skip_verification {
            warn!("host-key verification disabled — MITM exposure");
            return Ok(true);
        }
        match server_public_key {
            keys::PublicKeyOrCertificate::PublicKey { key, .. } => Ok(self.verify_host_key(key)),
            keys::PublicKeyOrCertificate::Certificate(_) => {
                warn!(
                    host = %self.host,
                    port = self.port,
                    "server presented a host certificate — rejecting connection (no certificate validation)"
                );
                Ok(false)
            }
        }
    }
```

Why this passes the lint table (`Cargo.toml:156-209`, `clippy.toml`):

- The match carries exactly the two arms the enum has, so it stays exhaustive
  without a `_ =>` arm: `wildcard_enum_match_arm` is denied and never fires. A
  closed enum with a catch-all is also what P-03 forbids.
- No `unwrap`, `expect`, `panic`, `todo` or print macro appears; those are
  `forbid` level (`Cargo.toml:158-168`). The certificate arm returns `Ok(false)`
  and the plain-key arm returns the value of `verify_host_key`.
- The method body is 14 lines, under the 30-line `too-many-lines-threshold`
  (`clippy.toml:2`), and has no nesting for the cognitive-complexity threshold.
- No suppression is added. `cargo clippy --all-features -- -D warnings` exits 0
  on this body (measured, `research.md`).

The plain-key arm calls the existing `verify_host_key`
(`src/adapters/ssh/internal/session.rs:59-88`) with the borrowed key and adds no
branch, so FR-005 holds by construction: TOFU learn for an unknown host,
rejection for a changed record, accept under `skip_verification`. The three
existing tests (`:154`, `:167`, `:190`) stay unchanged and green.

### The offered algorithms (FR-004)

`russh::client::Config.preferred` is the only field carrying preferred
algorithms (`https://docs.rs/russh/0.65.4/russh/client/struct.Config.html`), and
`Preferred.host_key_certificates` is the field that governs the certificate
variant: its documentation says the names are "advertised ahead of `key`, so a
server that has a certificate presents it in preference to a bare key", that the
list is "Empty by default", and that "turning it on is the caller's decision,
never a default"
(`https://docs.rs/russh/0.65.4/russh/struct.Preferred.html`).

`build_client_config` (`src/adapters/ssh/internal/client.rs:89-118`) stops
leaning on that default:

```rust
    let preferred = Preferred {
        // No host-certificate algorithm is offered: this client validates no
        // certificate chain, so it never invites one.
        host_key_certificates: Cow::Borrowed(&[]),
        compression,
        ..Default::default()
    };
```

with `use std::borrow::Cow;` added to the `std` import block at `:41-47`. The
pinning test joins the `client_config` module at `:1252`:

```rust
        #[test]
        fn test_no_host_certificate_algorithm_is_offered() {
            let config = build_client_config(Duration::from_secs(300), true, false);
            assert!(
                config.preferred.host_key_certificates.is_empty(),
                "the client must offer no host-certificate algorithm"
            );
        }
```

Together the two changes make a server that holds both a plain host key and a
certificate keep serving the plain key, because no certificate algorithm is
advertised to invite the certificate.

### The certificate test (FR-004)

A test in the `session` module builds a host certificate through the public API
and hands it to the handler. `russh::keys` re-exports the whole `ssh-key` crate
(`pub use ssh_key;`, `https://docs.rs/russh/0.65.4/russh/keys/index.html`), so
`russh::keys::ssh_key::certificate::Builder` is reachable without a new
dependency and `PublicKeyOrCertificate` has a `From<Certificate>` impl
(`https://docs.rs/russh/0.65.4/russh/keys/struct.Certificate.html`).

The production test is the throwaway test proven in `research.md`, moved into
the `tests` module of `src/adapters/ssh/internal/session.rs`:

```rust
    #[tokio::test]
    async fn certificate_is_rejected_and_not_recorded(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use russh::client::Handler as _;
        use russh::keys::ssh_key::certificate::{Builder, CertType};
        use russh::keys::PublicKeyOrCertificate;

        let dir = tempdir()?;
        let known_hosts_path = dir.path().join("known_hosts");
        let authority = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let subject = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let mut builder = Builder::new(
            b"probe".to_vec(),
            subject.public_key().clone(),
            0,
            u64::MAX,
        )?;
        builder.cert_type(CertType::Host)?;
        builder.key_id("probe")?;
        builder.all_principals_valid()?;
        let certificate = builder.sign(&authority)?;
        let presented = PublicKeyOrCertificate::Certificate(certificate);

        let mut handler = make_handler("example.invalid", 22, known_hosts_path.clone());
        let accepted = handler.check_server_key(&presented).await?;
        let recorded = std::fs::read_to_string(&known_hosts_path).unwrap_or_default();

        assert!(!accepted, "a host certificate must be rejected");
        assert!(
            recorded.is_empty(),
            "no known_hosts record may be written for a certificate"
        );
        Ok(())
    }
```

The `Builder` setters borrow `&mut self` and `sign` consumes the builder, so the
calls are made in statements rather than in one chain (measured; the chained
form fails E0507).

### The retry classification (FR-006)

`research.md`, section 4, carries the measurement: the rendered text of every
channel-open failure value is byte-identical under `0.60.3` and `0.65.4`, so the
upgrade changes no classification; and the substring matcher classifies a real
`ResourceShortage` refusal as **not** transient, which contradicts the behaviour
the repository documents at `CHANGELOG.md:24`, `CHANGELOG.md:71`,
`docs/MIGRATION.md:884`, `src/adapters/ssh/internal/client.rs:506-510` and
`:686-687`. The decision is to repair the classification in this change.

`is_transient_open_error` (`src/adapters/ssh/internal/client.rs:684-694`)
becomes a classifier over the typed error:

```rust
/// Classify a `channel_open_session` error as transient (slot may reopen soon).
///
/// The decision reads the typed error and never its rendered text, so a
/// `reason` string supplied by the peer cannot decide a retry. OpenSSH
/// surfaces `MaxSessions` exhaustion as `ConnectFailed` /
/// `Administratively prohibited`, and `RESOURCE_SHORTAGE` is the RFC label
/// for a peer out of slots; all three are transient.
const fn is_transient_open_error(err: &russh::Error) -> bool {
    matches!(
        err,
        russh::Error::ChannelOpenFailure(
            russh::ChannelOpenFailure::ConnectFailed
                | russh::ChannelOpenFailure::AdministrativelyProhibited
                | russh::ChannelOpenFailure::ResourceShortage
        )
    )
}
```

Why this construct passes the lint table without a suppression:

- `matches!` is the shape the project already uses on a library enum
  (`src/adapters/ssh/internal/session.rs:71`), and it has no `_ =>` arm, so
  `wildcard_enum_match_arm` (`Cargo.toml:177`) has nothing to fire on. An
  unknown variant simply does not match, which is the fail-closed default the
  requirement asks for.
- `ChannelOpenFailure` has five variants and is not `#[non_exhaustive]`
  (`https://docs.rs/russh/0.65.4/russh/enum.ChannelOpenFailure.html`), so naming
  three reasons and letting the other two fall through is exhaustive by
  construction rather than by a catch-all.
- `const fn` is required, not cosmetic: the first measured form without it failed
  the gate with `#[deny(clippy::missing_const_for_fn)] implied by
  #[deny(clippy::nursery)]` (`research.md`, section 4). With `const fn` added,
  `cargo clippy --all-features -- -D warnings` exits 0.

Both production call sites hand the classifier the typed value they already hold:

- `open_session_with_retry` (`:526`, the synchronous path, shared with the SFTP
  transfer path through `open_sftp_session`) currently computes
  `let msg = e.to_string();` and passes `&msg`. That binding becomes unused and
  is dropped; the call becomes `is_transient_open_error(&e)`.
- `handle_open_error` (`:672`, the asynchronous path) keeps `let msg =
  err.to_string();` because the `warn!` line above it logs `msg`, and the call
  becomes `is_transient_open_error(err)`.

The `transient_open_errors` tests are replaced. The three literal-string tests at
`:1393-1415` become two tests that build real values:
`exhaustion_reasons_are_transient` constructs
`Error::ChannelOpenFailure(reason)` for `ConnectFailed`,
`AdministrativelyProhibited` and `ResourceShortage` and asserts each is
transient; `other_reasons_and_errors_are_not_transient` constructs
`ChannelOpenFailure::Other { code: 1, reason: "resource_shortage" }` plus
`UnknownChannelType`, `Disconnect` and `HUP` and asserts none is transient.
`backoff_starts_at_100ms_and_caps_at_one_second` (`:1417-1424`) is unchanged.

**Behaviour change, stated plainly.** A real `ResourceShortage` refusal is now
retried on the schedule the synchronous path already has — 25 attempts with the
100 ms to 1 s backoff of `src/adapters/ssh/internal/client.rs:534-544` — where it
failed at once before, and the asynchronous path retries it under
`max_attempts` with `wait_with_cancel`. In the other direction, a `reason`
string inside `ChannelOpenFailure::Other` no longer triggers a retry, so text an
untrusted peer supplies can no longer choose one. Neither change alters what a
caller sees on success; the failing paths differ only in that a resource-shortage
refusal now has up to 25 attempts behind it.

### The dependency move (FR-001, FR-002)

```toml
# Cargo.toml:96-97
russh = "0.65"
russh-sftp = "4"
```

Resolution is one command, run at the repository root:

```sh
cargo update -p russh -p russh-sftp
```

`Cargo.lock` is not tracked (`.gitignore:4`), so the move writes it out of band.
What FR-001 and FR-002 can and cannot prove follows from that:

- **Can:** that the manifest declares the 0.65 and 4 lines; that a fresh resolve
  of those lines yields `russh 0.65.4` and `russh-sftp 4.0.0`; that a resolve of
  the same manifest reproduces the audited set, because the manifest range still
  admits a later 0.65.x.
- **Cannot:** that the audited set is what a consumer resolves. No committed
  lockfile pins the set, which is the same gap the baseline finding
  `pin.cargo.lockfile-missing` records. FR-002's guarantee is therefore a
  statement about the resolved set at the moment of the audit, not about an
  artifact in the tree. Committing the lockfile is item 6 of the update plan in
  `session.md` and stays outside this feature.

## Constitution

Every principle and every applicable doctrine, with how the plan satisfies it:

- **P-01 inward dependencies and static dispatch.** The only production edits
  are inside `src/adapters/ssh/internal/`; no port, use case or composition line
  changes, and no `dyn` is introduced.
- **P-02 lock-free hot paths.** No shared state type is touched and no atomic or
  `ArcSwap` field changes, so no loom invariant is owed.
- **P-03 panic-free production and justified suppressions.** The new handler
  body uses no `unwrap`, `expect`, `panic` or print macro, matches the closed
  enum exhaustively with no `_ =>`, and adds no `#[allow]`.
- **P-04 the MCP wire is frozen.** No renderer, argument struct, envelope line or
  error code is edited; FR-009 proves it with `make tools-export-check`.
- **P-05 additive and opt-in wire.** The feature adds nothing to the wire, so
  there is nothing to opt into.
- **P-06 the verified-change gate.** FR-007 runs the gate as its four commands
  directly — `cargo fmt --all -- --check`, `cargo clippy --all-features --
  -D warnings`, `cargo nextest run --test-threads num-cpus --lib` and
  `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` —
  with compilation at four jobs through the exported `CARGO_BUILD_JOBS=4` and
  `sccache` kept as the compiler wrapper. Nextest takes one test thread per CPU
  on the command line, which is the owner's setting; the `Makefile` target is not
  the gate here.
- **P-07 a session builds in the dev profile.** The implement loop runs the four
  commands above, `cargo build --bins` and the debug-profile binaries driven by
  the Python suites; no release build, no `make release`, `make all`, `make
  install` or `make sign`.
- **P-08 commits and recorded decisions.** The decision lives in
  `doc/arch/adr/0001-russh-advisory-upgrade.md`; the commit follows
  `<type>(<scope>): <subject>` and the owner lands it.
- **P-09 persisted artifacts are en-US.** Every new string, comment, record and
  artifact in this plan is en-US.
- **D-01 spec first.** The plan implements FR-001 to FR-011 of the specified
  feature and nothing else; the specification is the source of truth.
- **D-02 dogfooding.** `speckit validate` and `speckit verify` are part of the
  loop, and the feature closes no rule by weakening one.
- **D-03 hexagonal layering.** Both production files are in the adapters layer
  and import only inward; the layer table in `docs/ARCHITECTURE.md` is unchanged.
- **D-05 object calisthenics.** The changed method has `if` plus an exhaustive
  `match` and both arms return; no `else` is added, so the `branch.else` baseline
  count does not grow.
- **D-06 fully qualified names.** The new imports are `std::borrow::Cow` and the
  `russh::keys::ssh_key::certificate` path in the test module; no `super::` is
  added to production code.
- **D-07 lints and exact pinning.** `cargo clippy --all-features -- -D warnings`
  stays green; the pinning gap is the untracked lockfile recorded above, and this
  feature neither widens nor closes it.
- **D-08 fail loud.** The certificate path returns a rejection rather than
  unwrapping or ignoring, and the plain-key path propagates the existing
  `known_hosts` decision unchanged.
- **D-11 stable contracts.** The error taxonomy, the exit table and the
  `--json` shapes are untouched.
- **D-12 small changes.** One concern: two manifest lines, two source lines and
  their tests. The file map lists five files plus this feature's own records.
- **D-13 offline by default.** No network path is added; `cargo update` and
  `cargo audit` are operator commands, not runtime paths.
- **D-17 reproducible dependency surface.** The manifest keeps its existing line
  ranges and the lockfile stays untracked; the plan records both as the pre-
  existing state rather than changing them here.
- **D-04, D-09, D-10 do not apply.** No model element, no licence text and no
  guard-mediated write is touched.

## Verification Plan

Implement phase, in order, from the repository root, with `sccache` and four
compile jobs as the environment already exports them (`RUSTC_WRAPPER=sccache`
and `CARGO_BUILD_JOBS=4`; compilation stays at four jobs, and no `-j` is passed):

1. `cargo update -p russh -p russh-sftp` — resolves 0.65.4 and 4.0.0.
2. `cargo check --all-features --all-targets` — must exit 0; this is the step
   that proves FR-003.
3. `cargo fmt --all -- --check`.
4. `cargo clippy --all-features -- -D warnings`.
5. `cargo nextest run --test-threads num-cpus --lib` — at least 2114 passing
   tests, plus the new handler, certificate, configuration and classifier tests.
6. `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` —
   at least 2281.
7. `cargo audit` — no advisory on `russh`, `rsa` or `crypto-bigint`; the only
   vulnerability is RUSTSEC-2026-0462 on `rmcp 1.8.0` (FR-002).
8. `cargo build --bins` — the dev-profile binaries the Python suites drive.
9. `/root/dev/ssh-mcp/.venv/bin/python -m pytest scripts/test_http.py
   scripts/test_transfer_progress.py scripts/test_v7_rsync_http.py` (FR-008).
10. `make tools-export-check` (`Makefile:173-174`, the script at
    `scripts/export_tool_catalog.py --check`) — exits non-zero when the published
    tool catalogue drifted from the binary in `target/debug` (FR-009).
11. `speckit validate` — no finding on a feature file and no rule beyond the
    baseline.

Reviewer, on the same tree:

- Re-run 2, 4, 5, 6, 7, 9, 10 and 11 above.
- Read the diff of `src/adapters/ssh/internal/session.rs` and confirm the match
  has one arm per variant with no wildcard.
- Confirm the manifest diff is exactly the two lines of FR-001.
- Confirm the tool catalogue and the environment-variable table are unchanged:
  FR-009's catalogue proof is `make tools-export-check`, which recomputes
  `docs/TOOL_CATALOG.md` and `docs/tool-catalog.json` from a live `tools/list`
  handshake and compares them with the committed export; the environment-variable
  table is proved by the diff of `Cargo.toml` and by
  `src/adapters/config/internal/mod.rs` carrying no change at all, so the table
  has no source of drift.

Package availability on this machine (measured; install nothing):

- `/root/dev/ssh-mcp/.venv/bin/python` is Python 3.14.8. `pip` is **not**
  installed in that venv, so `python -m pip list` fails with
  `No module named pip`; the versions below come from the
  `site-packages` distribution directories.
- Present: `pytest 9.2.0`, `pytest_timeout 2.4.0`, `paramiko 5.1.0`,
  `cryptography 50.0.2`, `bcrypt 5.0.0`, `pynacl 1.6.2`, `cffi 2.1.1`,
  `pycparser 3.12`, `requests 2.34.2`, `urllib3 2.8.0`, `certifi 2026.10.23`,
  `idna 3.20`, `charset_normalizer 3.5.2`, `iniconfig 2.3.1`, `pluggy 1.7.0`,
  `packaging 26.3`, `pygments 2.21.0`, `invoke 3.0.3`.
- Nothing the suites import is missing. Run pytest from the repository root: the
  suites import `helpers.*`, which `scripts/conftest.py` makes importable.

## Documentation Changes

- **`CHANGELOG.md`** — under the existing `## [Unreleased]` heading (line 8), a
  `### Security` entry naming the four `russh` advisories, RUSTSEC-2023-0071 and
  RUSTSEC-2026-0387 as closed; a `### Changed` entry naming the dependency move
  and the host-certificate rejection; and a `### Fixed` entry for the retry
  classification, recording that a `RESOURCE_SHORTAGE` refusal is now retried as
  `CHANGELOG.md:24` and `:71` already claim and that a peer-supplied reason
  string no longer triggers a retry. The package version stays at 7.1.3; the
  version bump belongs to the owner's release step.
- **`docs/MIGRATION.md`** — a new operator section titled with the releases it
  spans, added to the section table at the top (`:32`), recording the three facts
  a consumer needs: the manifest now requires `russh` 0.65 and `russh-sftp` 4, so
  a stale lockfile must be re-resolved; a host that answers with a certificate is
  now rejected, so a fleet that serves host certificates must serve the plain key
  instead; and a `RESOURCE_SHORTAGE` channel-open refusal is retried on the
  25-attempt, 100 ms to 1 s schedule that `docs/MIGRATION.md:884` already
  documents for the SFTP and exec paths.
- **`doc/arch/threat-model/threat-model.md`, entry TM-021** — record that the
  `russh` half of the cure landed, that the resolved set now carries no advisory
  on `russh`, `rsa` or `crypto-bigint`, and that RUSTSEC-2026-0462 on `rmcp` and
  the `chrono` unmaintained warning stay open. The facts that are environmental
  keep their date.
- **`session.md`, item 1 of the update plan (`session.md:45-48`)** — record the
  outcome: resolved to 0.65.4 and 4.0.0, the advisories closed, and the remaining
  item-1 work that did not land with it.

## File Map

| File | Requirement it serves |
|---|---|
| `Cargo.toml` line 96-97 | FR-001 |
| `Cargo.lock` (untracked) | FR-001, FR-002 (resolved, not committed) |
| `src/adapters/ssh/internal/session.rs` — `check_server_key`, plus the certificate test and the existing host-key tests | FR-003, FR-004, FR-005 |
| `src/adapters/ssh/internal/client.rs` — `build_client_config` and its `client_config` test module | FR-004 |
| `src/adapters/ssh/internal/client.rs` — `is_transient_open_error`, both of its production callers (`open_session_with_retry` at `:526`, `handle_open_error` at `:672`) and the `transient_open_errors` test module | FR-006 |
| `CHANGELOG.md` | FR-010 |
| `docs/MIGRATION.md` | FR-010 |
| `doc/arch/threat-model/threat-model.md` | FR-010 |
| `session.md` | FR-010 |
| `doc/arch/sdd/001-russh-advisory-upgrade/`, `doc/arch/adr/0001-russh-advisory-upgrade.md`, `doc/arch/specs/features/russh-advisory-upgrade.feature` | the corpus records of FR-001 to FR-011 |

FR-007, FR-008, FR-009 and FR-011 change no file: FR-007 and FR-008 are gate and
suite outcomes, FR-009 is proved by `make tools-export-check` and by the absence
of any change under `src/adapters/config/` or `src/infra/mcp/`, and FR-011 is
proved by the scan recorded in the specification.

Anything outside this map is out of scope for the implement phase, in particular:
`rmcp` and every other dependency bump, the preferred algorithm lists, certificate
chain validation, `Cargo.lock` tracking, the CI action and Docker pins, and every
defect listed in `session.md`.

## Open Decisions

1. **The ADR series.** This feature's record sits in `doc/arch/adr/` while the
   twelve earlier records sit under `docs/adr/`. Unifying the two series is left
   to the conformity work; this plan moves and renumbers nothing.

## Rollback

The change is undone in four steps, all local:

1. Restore `Cargo.toml:96-97` to `russh = "0.60"` and `russh-sftp = "2"`.
2. Run `cargo update -p russh -p russh-sftp` to re-resolve. `Cargo.lock` is
   untracked, so re-resolving is the whole dependency rollback and there is no
   committed lockfile to revert.
3. Revert the two production edits: the `check_server_key` body returns to
   `Ok(self.verify_host_key(server_public_key))` with the `&keys::PublicKey`
   parameter, and `build_client_config` returns to letting `Preferred` take the
   library default. The certificate test is removed with the behaviour it proves.
4. Revert the four record files. The `known_hosts` file is untouched in both
   directions, so no host trust decision has to be migrated either way.

Rollback re-opens the four `russh` advisories and the two transitive ones, and it
restores a client that accepts a certificate's key through the `known_hosts`
lookup the current code performs on whatever key it is handed.
