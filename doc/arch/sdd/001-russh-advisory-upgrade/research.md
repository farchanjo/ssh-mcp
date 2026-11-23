# Research: Russh Advisory Upgrade

Evidence for the design decisions of `plan.md`. Every library fact below comes
from public documentation or from a measurement made in a throwaway copy of this
repository; no dependency source was read. Measurements were taken on
2026-11-22.

## Probe setup

Two throwaway copies, both outside the repository:

- `/tmp/ssh-mcp-russh-probe2` — this repository with `russh 0.65.4` and
  `russh-sftp 4.0.0` already resolved.
- `/tmp/ssh-mcp-russh-base060` — a copy left on the library in use, made with:

```sh
mkdir -p /tmp/ssh-mcp-russh-base060 && rsync -a --exclude target --exclude .git \
  --exclude .venv --exclude dist --exclude '*.tar.gz' --exclude doc/.specify \
  /root/dev/ssh-mcp/ /tmp/ssh-mcp-russh-base060/
```

`RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` were exported for every build;
every test run used `cargo nextest run` with a filter and the default test
threads. The repository itself was not built and not modified.

## 1. The handler

Public documentation:

- `check_server_key` — `fn check_server_key(&mut self, server_public_key:
  &PublicKeyOrCertificate) -> impl Future<Output = Result<bool, Self::Error>> +
  Send`, documented "Called to check the server's public key or certificate",
  "This is a very important step to help prevent man-in-the-middle attacks", and
  "The default implementation rejects all keys, and you must override it"
  (`https://docs.rs/russh/0.65.4/russh/client/trait.Handler.html`).
- `russh::keys::PublicKeyOrCertificate` — two variants,
  `PublicKey { key: PublicKey, hash_alg: Option<HashAlg> }` and
  `Certificate(Certificate)`; **not** `#[non_exhaustive]`; methods
  `public_key() -> PublicKey` and `certificate() -> Option<&Certificate>`; a
  `From<Certificate>` impl
  (`https://docs.rs/russh/0.65.4/russh/keys/enum.PublicKeyOrCertificate.html`).
- `russh::keys` re-exports the whole `ssh-key` crate (`pub use ssh_key;`), which
  is why the certificate builder needs no new dependency
  (`https://docs.rs/russh/0.65.4/russh/keys/index.html`).

Because the enum is not `#[non_exhaustive]`, a two-arm match is exhaustive and
`wildcard_enum_match_arm` (`Cargo.toml:177`) never fires. Measured on the
designed body, in `/tmp/ssh-mcp-russh-probe2`:

```sh
cargo clippy --all-features --offline -- -D warnings
```
```
    Checking ssh-mcp v7.1.3 (/tmp/ssh-mcp-russh-probe2)
    Finished `dev` profile [unoptimized] target(s) in 11.24s
CLIPPY_EXIT=0
```

## 2. The offered algorithms

`russh::client::Config::preferred` is the only field of the client configuration
that carries preferred algorithms, documented "Lists of preferred algorithms";
no other field on that struct relates to host keys or certificates
(`https://docs.rs/russh/0.65.4/russh/client/struct.Config.html`).

`russh::Preferred::host_key_certificates` is documented as "Host-key algorithms
for which to also advertise the OpenSSH certificate variant
(`*-cert-v01@openssh.com`)", with the note that those names are "advertised ahead
of `key`, so a server that has a certificate presents it in preference to a bare
key", that the list is **"Empty by default"**, and that "turning it on is the
caller's decision, never a default"
(`https://docs.rs/russh/0.65.4/russh/struct.Preferred.html`).

The default therefore does not advertise a certificate, but the feature does not
rest on it. With `host_key_certificates: Cow::Borrowed(&[])` set in
`build_client_config` and the pinning test added, in
`/tmp/ssh-mcp-russh-probe2`:

```sh
cargo nextest run --lib --offline -E 'test(test_no_host_certificate_algorithm_is_offered) | test(test_compression_enabled_includes_zlib) | test(test_builds_config_with_keepalive)'
```
```
        PASS [   0.003s] (1/3) ssh-mcp adapters::ssh::internal::client::tests::client_config::test_no_host_certificate_algorithm_is_offered
        PASS [   0.004s] (2/3) ssh-mcp adapters::ssh::internal::client::tests::client_config::test_builds_config_with_keepalive
        PASS [   0.004s] (3/3) ssh-mcp adapters::ssh::internal::client::tests::client_config::test_compression_enabled_includes_zlib
     Summary [   0.005s] 3 tests run: 3 passed, 2113 skipped
EXIT=0
```

## 3. Building a host certificate in a test

The public API can build one. `russh::keys::Certificate` offers
`from_openssh(&str)`, `from_bytes(&[u8])` and `read_file(&Path)`, and the
certificate-authority path is the builder
(`https://docs.rs/russh/0.65.4/russh/keys/struct.Certificate.html`). The builder
is created from the subject's public key, configured with `cert_type`,
`key_id` and the principal calls, and signed by the authority:

- `Builder::new(nonce, public_key, valid_after, valid_before) -> Result<Self>`
- `Builder::new_with_validity_times(nonce, public_key, valid_after, valid_before)`
- `cert_type(&mut self, CertType) -> Result<&mut Self>`, default `CertType::User`
- `key_id(&mut self, impl Into<String>) -> Result<&mut Self>`, default empty
- `valid_principal(&mut self, ..)` and `all_principals_valid(&mut self)`
- `sign<S: SigningKey>(self, signing_key: &S) -> Result<Certificate>`, which
  consumes the builder — so the calls go in statements, not one chain

All signatures from
`https://docs.rs/ssh-key/0.7.0-rc.12/ssh_key/certificate/struct.Builder.html`,
reachable in-tree as `russh::keys::ssh_key::certificate::Builder` because
`russh::keys` re-exports `ssh_key`.

The throwaway test, added to the `tests` module of
`/tmp/ssh-mcp-russh-probe2/src/adapters/ssh/internal/session.rs`:

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

        std::fs::write(
            "/tmp/probe-cert-065.txt",
            format!(
                "certificate_accepted={accepted}\nknown_hosts_bytes={}\nknown_hosts_content={:?}\n",
                recorded.len(),
                recorded
            ),
        )?;

        assert!(!accepted, "a host certificate must be rejected");
        assert!(
            recorded.is_empty(),
            "no known_hosts record may be written for a certificate"
        );
        Ok(())
    }
```

```sh
cd /tmp/ssh-mcp-russh-probe2 && cargo nextest run --lib certificate_is_rejected --offline
```
```
        PASS [   0.010s] (1/1) ssh-mcp adapters::ssh::internal::session::tests::certificate_is_rejected_and_not_recorded
     Summary [   0.011s] 1 test run: 1 passed, 2114 skipped
EXIT=0
```

```sh
cat /tmp/probe-cert-065.txt
```
```
certificate_accepted=false
known_hosts_bytes=0
known_hosts_content=""
```

The three existing host-key tests pass beside it on the designed body:

```sh
cd /tmp/ssh-mcp-russh-probe2 && cargo nextest run --lib --offline -E 'test(skip_flag_accepts_any_host_key) | test(recorded_key_mismatch_is_rejected) | test(unknown_host_is_learned_via_tofu) | test(certificate_is_rejected)'
```
```
        PASS [   0.004s] (1/4) ssh-mcp adapters::ssh::internal::session::tests::skip_flag_accepts_any_host_key
        PASS [   0.004s] (2/4) ssh-mcp adapters::ssh::internal::session::tests::recorded_key_mismatch_is_rejected
        PASS [   0.004s] (3/4) ssh-mcp adapters::ssh::internal::session::tests::unknown_host_is_learned_via_tofu
        PASS [   0.011s] (4/4) ssh-mcp adapters::ssh::internal::session::tests::certificate_is_rejected_and_not_recorded
     Summary [   0.012s] 4 tests run: 4 passed, 2111 skipped
EXIT=0
```

The `std::fs::write` to `/tmp/probe-cert-065.txt` exists in the throwaway test
only, so the two facts could be read back from the copy as text. The production
test carried into `plan.md` has no such debugging write: it asserts the rejection
and the empty `known_hosts` file directly, and nothing writes outside the
temporary directory.

**Limit of this proof.** The test proves what the handler does with a
`PublicKeyOrCertificate::Certificate` value. It does not prove the wire case,
where a real server offers a certificate: no Rust suite of this repository drives
a real SSH server, so the end-to-end path stays an accepted risk in the
specification. The narrower claim it does establish is the one FR-004 needs: the
handler rejects the certificate variant and writes no trust record, and the
configuration offers no certificate algorithm to invite one.

## 4. The retry classification

### Today's behaviour, read from `src/adapters/ssh/internal/client.rs:664-694`

`handle_open_error` renders the typed error with `err.to_string()`, logs it, and
calls `is_transient_open_error` over the rendering (`:670-672`). The matcher
lowercases the text and accepts it when it contains one of three substrings
(`:690-693`):

| Substring matched | Intent, from the code and its tests | Caller's action on a match |
|---|---|---|
| `connectfailed` | `ConnectFailed` — the peer's connection to the target failed | retry, up to `MAX_ATTEMPTS` with backoff (`:672-677`), then fail |
| `resource_shortage` | `RESOURCE_SHORTAGE`, the RFC label for a peer out of slots | retry, same path |
| `administratively` | `AdministrativelyProhibited`, an OpenSSH `MaxSessions` refusal | retry, same path |
| anything else | not transient | no retry: the command is marked failed with `Failed to open channel` (`:679-681`) |

### Measured rendered text and classification

One test per copy renders every channel-open failure value and classifies it with
the production matcher copied verbatim from `:684-694`.

```sh
cd /tmp/ssh-mcp-russh-base060 && cargo nextest run --test probe_error_display --offline
cd /tmp/ssh-mcp-russh-probe2 && cargo nextest run --test probe_error_display --offline
```
```
        PASS [   0.003s] (1/1) ssh-mcp::probe_error_display dump_channel_open_failure_display
     Summary [   0.003s] 1 test run: 1 passed, 0 skipped
EXIT=0
```

Side by side (`transient` is what the shipped matcher returns):

| Value | `russh 0.60.3` rendered | 0.60.3 transient | `russh 0.65.4` rendered | 0.65.4 transient |
|---|---|---|---|---|
| `ChannelOpenFailure(ConnectFailed)` | `Failed to open channel (ConnectFailed)` | true | `Failed to open channel (ConnectFailed)` | true |
| `ChannelOpenFailure(AdministrativelyProhibited)` | `Failed to open channel (AdministrativelyProhibited)` | true | same | true |
| `ChannelOpenFailure(ResourceShortage)` | `Failed to open channel (ResourceShortage)` | **false** | same | **false** |
| `ChannelOpenFailure(UnknownChannelType)` | `Failed to open channel (UnknownChannelType)` | false | same | false |
| `ChannelOpenFailure(Unknown)` | `Failed to open channel (Unknown)` | false | variant no longer exists | — |
| `ChannelOpenFailure(Other { code, reason })` | variant does not exist | — | `Failed to open channel (Other { code: 7, reason: "probe reason" })` | false |
| `ChannelOpenFailure(Other { reason: "resource_shortage" })` | variant does not exist | — | `Failed to open channel (Other { code: 1, reason: "resource_shortage" })` | **true** |
| `Disconnect` | `Disconnected` | false | same | false |
| `HUP` | `Connection closed by the remote side` | false | same | false |
| hand-written `Channel open failure: RESOURCE_SHORTAGE`, the string the current unit test feeds | — | true | — | true |

Verbatim, from `/tmp/probe-display-060.txt`:

```
Unknown	Failed to open channel (Unknown)	transient=false
AdministrativelyProhibited	Failed to open channel (AdministrativelyProhibited)	transient=true
ConnectFailed	Failed to open channel (ConnectFailed)	transient=true
UnknownChannelType	Failed to open channel (UnknownChannelType)	transient=false
ResourceShortage	Failed to open channel (ResourceShortage)	transient=false
Disconnect	Disconnected	transient=false
HUP	Connection closed by the remote side	transient=false
hand-written RESOURCE_SHORTAGE	Channel open failure: RESOURCE_SHORTAGE	transient=true
```

Verbatim, from `/tmp/probe-display-065.txt`:

```
AdministrativelyProhibited	Failed to open channel (AdministrativelyProhibited)	transient=true
ConnectFailed	Failed to open channel (ConnectFailed)	transient=true
UnknownChannelType	Failed to open channel (UnknownChannelType)	transient=false
ResourceShortage	Failed to open channel (ResourceShortage)	transient=false
Other{code:7}	Failed to open channel (Other { code: 7, reason: "probe reason" })	transient=false
Other{reason:resource_shortage}	Failed to open channel (Other { code: 1, reason: "resource_shortage" })	transient=true
Disconnect	Disconnected	transient=false
HUP	Connection closed by the remote side	transient=false
hand-written RESOURCE_SHORTAGE	Channel open failure: RESOURCE_SHORTAGE	transient=true
```

### What the measurement establishes

1. **The upgrade changes no classification.** For every value both releases can
   produce, the rendered text and the classification are identical. The upgrade
   itself is behaviour-neutral for the retry path.
2. **Today's behaviour is wrong in one place, and not because of the upgrade.**
   `ResourceShortage` is the variant the comment and the test name single out as
   transient, and the matcher classifies it as **not** transient: the matcher
   looks for `resource_shortage` with an underscore, while the library renders
   `ResourceShortage` without one. The unit test
   `max_sessions_and_resource_shortage_are_transient`
   (`src/adapters/ssh/internal/client.rs:1393-1407`) passes only because the
   third assertion feeds the hand-written string
   `Channel open failure: RESOURCE_SHORTAGE`, which no `russh` value renders.
3. **The arm is not dead in 0.65.4, it is remotely controlled.** The only value
   whose rendering contains `resource_shortage` is
   `ChannelOpenFailure::Other { reason }`, where `reason` is the text the peer
   supplied. So the third substring keys retry behaviour off text an untrusted
   server chooses. That is the drift FR-006 exists to close, in its strongest
   form.

Per the constraint that the choice keeps today's classification for every value
unless today's behaviour is itself wrong, this is the point the implement phase
stopped on. The decision taken is to **repair the classification to the intent
the repository documents**: the matcher becomes a function of the typed error,
`ResourceShortage` becomes transient, and no text — rendered or peer-supplied —
decides a retry.

The documented intent that the code did not deliver is in five places:
`CHANGELOG.md:24` ("SFTP channel opens retry transient `MaxSessions` /
`RESOURCE_SHORTAGE` refusals"), `CHANGELOG.md:71` ("Synchronous `ssh_exec` /
health checks now retry the transient `MaxSessions` / `RESOURCE_SHORTAGE`
channel-open refusal"), `docs/MIGRATION.md:884` ("25 attempts, 100 ms → 1 s
backoff"), the doc comment at `src/adapters/ssh/internal/client.rs:506-510`, and
the comment at `:686-687` ("`RESOURCE_SHORTAGE` is the RFC label"). The code
never did it for a value the library produces, and the test that claims it feeds
a string no library value renders.

### The classifier

`russh::ChannelOpenFailure` carries five variants and is not `#[non_exhaustive]`;
the rendered declaration and the variant list
(`https://docs.rs/russh/0.65.4/russh/enum.ChannelOpenFailure.html`) are:

```rust
pub enum ChannelOpenFailure {
    AdministrativelyProhibited,
    ConnectFailed,
    UnknownChannelType,
    ResourceShortage,
    Other {
        code: u32,
        reason: String,
    },
}
```
No attribute line is rendered above the declaration and the word
`non_exhaustive` appears nowhere on the page. The page documents the type as
"Reason for not being able to open a channel", and lists two inherent methods,
`pub fn code(&self) -> u32` ("SSH protocol reason code for this failure") and
`pub fn description(&self) -> &str` ("A human-readable description of this
failure").

The classifier proven in `/tmp/ssh-mcp-russh-probe2`:

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

`matches!` is the shape the project already applies to a library enum at
`src/adapters/ssh/internal/session.rs:71`, and it carries no `_ =>` arm, so
`wildcard_enum_match_arm` has nothing to fire on: an unmatched variant simply
yields `false`.

`const fn` is required by the gate, not stylistic. The first measured form
without it failed:

```sh
cd /tmp/ssh-mcp-russh-probe2 && cargo clippy --all-features --offline -- -D warnings
```
```
    |         ^^^^^^^^^^^^^^^
    = note: `#[deny(clippy::missing_const_for_fn)]` implied by `#[deny(clippy::nursery)]`
help: make the function `const`
    |
694 | const fn is_transient_open_error(err: &russh::Error) -> bool {
    | +++++

error: could not compile `ssh-mcp` (lib) due to 1 previous error
CLIPPY_EXIT=101
```

With `const fn` added, the same command:

```sh
cd /tmp/ssh-mcp-russh-probe2 && cargo clippy --all-features --offline -- -D warnings
```
```
    Checking ssh-mcp v7.1.3 (/tmp/ssh-mcp-russh-probe2)
    Finished `dev` profile [unoptimized] target(s) in 4.54s
CLIPPY_EXIT=0
```

No suppression was added. Both production call sites pass the typed value they
already hold: `open_session_with_retry` at `:526` drops its now-unused
`let msg = e.to_string();` and calls `is_transient_open_error(&e)`;
`handle_open_error` at `:672` keeps `msg` for its `warn!` line and calls
`is_transient_open_error(err)`.

### The test that pins it

The three literal-string tests at `:1393-1415` are replaced by two tests that
build real values, and `backoff_starts_at_100ms_and_caps_at_one_second`
(`:1417-1424`) is unchanged:

```rust
        #[test]
        fn exhaustion_reasons_are_transient() {
            // The three reasons OpenSSH / russh report when the per-connection
            // channel budget is exhausted. Transfers share this retry path
            // through `open_sftp_session`.
            use russh::{ChannelOpenFailure, Error};

            for reason in [
                ChannelOpenFailure::ConnectFailed,
                ChannelOpenFailure::AdministrativelyProhibited,
                ChannelOpenFailure::ResourceShortage,
            ] {
                let err = Error::ChannelOpenFailure(reason);
                assert!(
                    is_transient_open_error(&err),
                    "an exhaustion refusal must be transient: {err}"
                );
            }
        }

        #[test]
        fn other_reasons_and_errors_are_not_transient() {
            use russh::{ChannelOpenFailure, Error};

            let peer_reason = Error::ChannelOpenFailure(ChannelOpenFailure::Other {
                code: 1,
                reason: "resource_shortage".to_string(),
            });
            assert!(
                !is_transient_open_error(&peer_reason),
                "a reason string supplied by the peer must not decide a retry"
            );
            for err in [
                Error::ChannelOpenFailure(ChannelOpenFailure::UnknownChannelType),
                Error::Disconnect,
                Error::HUP,
            ] {
                assert!(
                    !is_transient_open_error(&err),
                    "must not be transient: {err}"
                );
            }
        }
```

```sh
cd /tmp/ssh-mcp-russh-probe2 && cargo nextest run --lib --offline -E 'test(exhaustion_reasons_are_transient) | test(other_reasons_and_errors_are_not_transient) | test(backoff_starts_at_100ms_and_caps_at_one_second)'
```
```
        PASS [   0.003s] (1/3) ssh-mcp adapters::ssh::internal::client::tests::transient_open_errors::backoff_starts_at_100ms_and_caps_at_one_second
        PASS [   0.004s] (2/3) ssh-mcp adapters::ssh::internal::client::tests::transient_open_errors::exhaustion_reasons_are_transient
        PASS [   0.004s] (3/3) ssh-mcp adapters::ssh::internal::client::tests::transient_open_errors::other_reasons_and_errors_are_not_transient
     Summary [   0.005s] 3 tests run: 3 passed, 2113 skipped
EXIT=0
```

### The behaviour change

A real `ResourceShortage` refusal is now retried on the schedule the synchronous
path already has — 25 attempts, 100 ms to 1 s (`:534-544`) — where it failed at
once before; the asynchronous path retries it under `max_attempts` with
`wait_with_cancel`. In the other direction, a `reason` string inside
`ChannelOpenFailure::Other` no longer triggers a retry, so text an untrusted peer
supplies can no longer choose one.

## 5. The dependency move and the untracked lockfile

`Cargo.toml:96-97` declares `russh = "0.60"` and `russh-sftp = "2"`. One
`cargo update -p russh -p russh-sftp` resolves `russh 0.65.4` and
`russh-sftp 4.0.0`, and no other direct dependency moves its declared
requirement (`/tmp/ssh-mcp-russh-probe/update.log`).

`Cargo.lock` is listed in `.gitignore:4`, so it is not in the tree. The
consequence for the specification is written into `plan.md`: FR-001 and FR-002
describe the resolved set, not a committed artifact, and `pin.cargo.lockfile-missing`
stays in the baseline until committing the lockfile is taken up as its own work
(item 6 of the update plan in `session.md`).

## 6. Package availability

Measured with `ls` on the venv and on its `site-packages` distribution
directories; nothing was installed.

- `/root/dev/ssh-mcp/.venv/bin/python --version` → `Python 3.14.8`.
- `/root/dev/ssh-mcp/.venv/bin/python -m pip list` → `No module named pip`. `pip`
  is not installed in that venv, so the version list below comes from the
  `site-packages` directories.
- Present: `pytest 9.2.0`, `pytest_timeout 2.4.0`, `paramiko 5.1.0`,
  `cryptography 50.0.2`, `bcrypt 5.0.0`, `pynacl 1.6.2`, `cffi 2.1.1`,
  `pycparser 3.12`, `requests 2.34.2`, `urllib3 2.8.0`, `certifi 2026.10.23`,
  `idna 3.20`, `charset_normalizer 3.5.2`, `iniconfig 2.3.1`, `pluggy 1.7.0`,
  `packaging 26.3`, `pygments 2.21.0`, `invoke 3.0.3`.
- Every import the three suites need is present: `pytest`,
  `pytest-timeout` (through `pytest.ini:6-7`), `paramiko` and `cryptography`
  (`scripts/helpers/local_sshd.py:48-51`, `:76-77`), and `requests` for the HTTP
  client (`scripts/helpers/mcp_client.py:28`).

## 7. Open decisions carried out of this research

1. The two ADR series, stated in `plan.md` and in the feature's ADR. The
   `ResourceShortage` classification is no longer open: section 4 records the
   decision and the classifier it produces.
