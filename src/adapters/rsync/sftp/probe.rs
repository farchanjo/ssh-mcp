//! SFTP capability probe.
//!
//! Locked-down servers (corporate proxies, Windows OpenSSH, jail-mode
//! `internal-sftp` configurations) routinely refuse one of `setstat`,
//! `symlink`, or POSIX rename. The Wire transport surfaces these as
//! protocol errors, but the SFTP transport needs to know upfront so it
//! can either route the affected operation through the Wire path or
//! return a stable [`DomainError::SftpFeatureMissing`] before the
//! recursive walk burns any RTT.
//!
//! The probe runs three tiny round-trips against a freshly-created
//! ephemeral directory under the user's home; it is idempotent and
//! cheap (3 RTTs total). Cache the result per session in the calling
//! use case — the cost of running it once amortises over every
//! subsequent file in the sync.
//!
//! v7.0.0-alpha.4 first slice: every probe step is a best-effort
//! check; failures collapse to "feature unsupported" rather than
//! propagating as `DomainError`. Probing must never be the reason a
//! sync fails — it can only ever route the sync.

use crate::domain::ids::SessionId;
use crate::ports::rsync_sftp_fs::{RemoteMetadata, RsyncSftpFsPort};

/// Snapshot of SFTP server capabilities the rsync transport cares
/// about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SftpFeatures {
    /// Whether the SFTP server accepts `SSH_FXP_SETSTAT` for non-root
    /// accounts. Most modern OpenSSH builds accept it; some
    /// `internal-sftp` configurations refuse it.
    pub setstat_supported: bool,
    /// Whether the SFTP server accepts `SSH_FXP_SYMLINK`. Windows
    /// OpenSSH 7.7 and certain jails refuse it.
    pub symlink_supported: bool,
    /// Whether the server advertises `posix-rename@openssh.com`.
    /// Currently a stub — russh-sftp does not yet expose the extension
    /// list, so this defaults to `true` (rename works without the
    /// extension on POSIX servers).
    pub posix_rename_supported: bool,
}

impl SftpFeatures {
    /// Default — every feature claimed supported. Used by the probe
    /// when constructing the response, then individual flags are flipped
    /// to `false` as failed RTTs surface.
    #[must_use]
    pub const fn all_supported() -> Self {
        Self {
            setstat_supported: true,
            symlink_supported: true,
            posix_rename_supported: true,
        }
    }

    /// Pessimistic default — every feature claimed unsupported. Used
    /// when the probe cannot even create the scratch directory (most
    /// often a sign of a `chroot`ed `internal-sftp` server with no
    /// home directory).
    #[must_use]
    pub const fn nothing_supported() -> Self {
        Self {
            setstat_supported: false,
            symlink_supported: false,
            posix_rename_supported: false,
        }
    }
}

impl Default for SftpFeatures {
    fn default() -> Self {
        Self::all_supported()
    }
}

/// Outcome of attempting to run the capability probe.
///
/// The probe's own scratch-directory `mkdir` can fail for reasons that
/// have nothing to do with the server's SFTP capabilities — a
/// permission-denied home directory, a `chroot`ed `internal-sftp` jail,
/// or (before the scratch path was made unique per call) a race between
/// two concurrent probes on the same session. That is an
/// **infrastructure failure to run the probe**, not a genuine "this
/// server refuses `setstat`/`symlink`" result, and it must never be
/// cached as one: caching it would permanently wedge every later
/// `ssh_rsync` call on the session behind a stale `nothing_supported`
/// verdict. [`ProbeOutcome`] lets the caller tell the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The scratch directory was created and both capability checks ran
    /// to completion; the wrapped [`SftpFeatures`] is a genuine result
    /// safe to cache for the rest of the session.
    Ran(SftpFeatures),
    /// The probe's scratch-directory `mkdir` failed before any
    /// capability check could run. Callers must fall back to the
    /// pessimistic [`SftpFeatures::nothing_supported`] for *this* call
    /// only, and must NOT cache it — the next call gets a fresh
    /// (unique-path) retry.
    InfraFailure,
}

impl ProbeOutcome {
    /// Features to use for the call that produced this outcome,
    /// regardless of whether the probe genuinely ran. Check
    /// [`Self::should_cache`] before persisting this into a
    /// session-keyed cache.
    #[must_use]
    pub const fn features(self) -> SftpFeatures {
        match self {
            Self::Ran(features) => features,
            Self::InfraFailure => SftpFeatures::nothing_supported(),
        }
    }

    /// `true` when this outcome reflects a genuine capability probe
    /// that ran to completion, i.e. it is safe to cache.
    #[must_use]
    pub const fn should_cache(self) -> bool {
        matches!(self, Self::Ran(_))
    }
}

/// Run the SFTP capability probe against `fs` on `session`.
///
/// Allocates a scratch directory under
/// `<scratch_root>/.ssh-mcp-probe-<session-id>-<nonce>` (default
/// `scratch_root = "/tmp"`) — the trailing `UUIDv7` nonce guarantees two
/// probes on the same session (concurrent calls, or a retry after a
/// prior probe was cancelled before its cleanup ran) never target the
/// same path — exercises each capability, then tears the directory
/// down. The probe never propagates errors — every probe step's
/// failure flips its corresponding flag to `false` and the next step
/// still runs.
pub async fn probe<F>(fs: &F, session: &SessionId) -> ProbeOutcome
where
    F: RsyncSftpFsPort,
{
    probe_with_scratch_root(fs, session, "/tmp").await
}

/// Variant of [`probe`] that lets the caller override the scratch
/// directory. Mostly useful for tests against the
/// [`crate::adapters::rsync::sftp::fake::FakeRsyncSftpFs`].
pub async fn probe_with_scratch_root<F>(
    fs: &F,
    session: &SessionId,
    scratch_root: &str,
) -> ProbeOutcome
where
    F: RsyncSftpFsPort,
{
    let nonce = uuid::Uuid::now_v7().simple();
    let scratch_dir = format!(
        "{}/.ssh-mcp-probe-{}-{}",
        scratch_root,
        session.as_str(),
        nonce
    );
    if fs.mkdir(session, &scratch_dir, 0o755).await.is_err() {
        return ProbeOutcome::InfraFailure;
    }

    let setstat_supported = probe_setstat(fs, session, &scratch_dir).await;
    let symlink_supported = probe_symlink(fs, session, &scratch_dir).await;
    // posix-rename probe is currently a stub — russh-sftp does not
    // expose the extension list. Default to `true` on the assumption
    // that every POSIX server's plain `rename` is good enough.
    let posix_rename_supported = true;

    let _ = fs.rmdir(session, &scratch_dir).await;

    ProbeOutcome::Ran(SftpFeatures {
        setstat_supported,
        symlink_supported,
        posix_rename_supported,
    })
}

async fn probe_setstat<F>(fs: &F, session: &SessionId, scratch_dir: &str) -> bool
where
    F: RsyncSftpFsPort,
{
    let probe_meta = RemoteMetadata {
        size: 0,
        mode: 0o700,
        mtime: 1_700_000_000,
        uid: 0,
        gid: 0,
        is_dir: true,
        is_symlink: false,
    };
    fs.set_metadata(session, scratch_dir, probe_meta)
        .await
        .is_ok()
}

async fn probe_symlink<F>(fs: &F, session: &SessionId, scratch_dir: &str) -> bool
where
    F: RsyncSftpFsPort,
{
    let link = format!("{scratch_dir}/probe-link");
    let supported = fs.symlink(session, "probe-target", &link).await.is_ok();
    if supported {
        let _ = fs.remove_file(session, &link).await;
    }
    supported
}

#[cfg(test)]
mod tests {
    use super::{ProbeOutcome, SftpFeatures, probe_with_scratch_root};
    use crate::adapters::rsync::sftp::fake::FakeRsyncSftpFs;
    use crate::domain::ids::SessionId;

    fn s() -> SessionId {
        SessionId::new("probe-test".to_string())
    }

    fn ran_features(outcome: ProbeOutcome) -> SftpFeatures {
        match outcome {
            ProbeOutcome::Ran(features) => features,
            ProbeOutcome::InfraFailure => panic!("expected ProbeOutcome::Ran"),
        }
    }

    #[test]
    fn defaults_are_optimistic() {
        let f = SftpFeatures::default();
        assert!(f.setstat_supported);
        assert!(f.symlink_supported);
        assert!(f.posix_rename_supported);
    }

    #[test]
    fn nothing_supported_is_pessimistic() {
        let f = SftpFeatures::nothing_supported();
        assert!(!f.setstat_supported);
        assert!(!f.symlink_supported);
        assert!(!f.posix_rename_supported);
    }

    #[tokio::test]
    async fn fake_fs_probe_reports_full_capabilities() {
        let fs = FakeRsyncSftpFs::new();
        // Pre-create the scratch root parent so mkdir against
        // /tmp/.ssh-mcp-probe-<id>-<nonce> succeeds.
        fs.put_dir("/tmp", 0o755);
        let outcome = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        let features = ran_features(outcome);
        assert!(features.setstat_supported);
        assert!(features.symlink_supported);
        assert!(features.posix_rename_supported);
    }

    #[tokio::test]
    async fn mkdir_infra_failure_yields_infra_failure_outcome() {
        // A genuine infrastructure failure (permission denied, no
        // writable home dir, ...) must surface as `InfraFailure` — a
        // distinct variant from a real "server refuses everything"
        // capability result — so the caller knows not to cache it.
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/tmp", 0o755);
        fs.fail_mkdir();
        let outcome = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        assert_eq!(outcome, ProbeOutcome::InfraFailure);
        // `.features()` still degrades to the pessimistic default for
        // the caller that needs an answer for *this* call.
        let features = outcome.features();
        assert!(!features.setstat_supported);
        assert!(!features.symlink_supported);
        assert!(!features.posix_rename_supported);
        assert!(!outcome.should_cache());
    }

    #[tokio::test]
    async fn concurrent_probes_on_same_session_never_collide() {
        // Regression for BUG #2: two `ssh_rsync` calls racing on a
        // fresh session used to target the exact same fixed scratch
        // path (`/tmp/.ssh-mcp-probe-<session-id>`), so one probe's
        // `mkdir` would fail against the other's still-live directory
        // and wrongly report `nothing_supported`. The per-call UUIDv7
        // nonce gives each probe its own path, so both run to
        // completion independently.
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/tmp", 0o755);
        let session = s();
        let (first, second) = tokio::join!(
            probe_with_scratch_root(&fs, &session, "/tmp"),
            probe_with_scratch_root(&fs, &session, "/tmp"),
        );
        assert!(matches!(first, ProbeOutcome::Ran(_)));
        assert!(matches!(second, ProbeOutcome::Ran(_)));
    }

    #[tokio::test]
    async fn probe_when_setstat_unsupported_flips_only_setstat() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/tmp", 0o755);
        fs.fail_setstat();
        let outcome = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        let features = ran_features(outcome);
        assert!(!features.setstat_supported);
        assert!(features.symlink_supported);
    }

    #[tokio::test]
    async fn probe_when_symlink_unsupported_flips_only_symlink() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/tmp", 0o755);
        fs.fail_symlink();
        let outcome = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        let features = ran_features(outcome);
        assert!(features.setstat_supported);
        assert!(!features.symlink_supported);
    }
}
