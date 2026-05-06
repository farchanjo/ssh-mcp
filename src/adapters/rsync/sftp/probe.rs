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

/// Run the SFTP capability probe against `fs` on `session`.
///
/// Allocates a scratch directory under
/// `<scratch_root>/.ssh-mcp-probe-<session-id>` (default
/// `scratch_root = "/tmp"`), exercises each capability, then tears the
/// directory down. The probe never propagates errors — every probe
/// step's failure flips its corresponding flag to `false` and the next
/// step still runs.
pub async fn probe<F>(fs: &F, session: &SessionId) -> SftpFeatures
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
) -> SftpFeatures
where
    F: RsyncSftpFsPort,
{
    let scratch_dir = format!("{}/.ssh-mcp-probe-{}", scratch_root, session.as_str());
    if fs.mkdir(session, &scratch_dir, 0o755).await.is_err() {
        return SftpFeatures::nothing_supported();
    }

    let setstat_supported = probe_setstat(fs, session, &scratch_dir).await;
    let symlink_supported = probe_symlink(fs, session, &scratch_dir).await;
    // posix-rename probe is currently a stub — russh-sftp does not
    // expose the extension list. Default to `true` on the assumption
    // that every POSIX server's plain `rename` is good enough.
    let posix_rename_supported = true;

    let _ = fs.rmdir(session, &scratch_dir).await;

    SftpFeatures {
        setstat_supported,
        symlink_supported,
        posix_rename_supported,
    }
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
    use super::{SftpFeatures, probe_with_scratch_root};
    use crate::adapters::rsync::sftp::fake::FakeRsyncSftpFs;
    use crate::domain::ids::SessionId;

    fn s() -> SessionId {
        SessionId::new("probe-test".to_string())
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
        // /tmp/.ssh-mcp-probe-<id> succeeds.
        fs.put_dir("/tmp", 0o755);
        let features = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        assert!(features.setstat_supported);
        assert!(features.symlink_supported);
        assert!(features.posix_rename_supported);
    }

    #[tokio::test]
    async fn mkdir_failure_collapses_to_nothing_supported() {
        // FakeRsyncSftpFs returns `Sftp("path exists")` when mkdir
        // targets an existing path. Pre-seeding the scratch dir
        // therefore forces the probe's initial mkdir to fail and the
        // probe returns the pessimistic default.
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/tmp", 0o755);
        fs.put_dir(&format!("/tmp/.ssh-mcp-probe-{}", s().as_str()), 0o755);
        let features = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        assert!(!features.setstat_supported);
        assert!(!features.symlink_supported);
        assert!(!features.posix_rename_supported);
    }

    #[tokio::test]
    async fn probe_when_setstat_unsupported_flips_only_setstat() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/tmp", 0o755);
        fs.fail_setstat();
        let features = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        assert!(!features.setstat_supported);
        assert!(features.symlink_supported);
    }

    #[tokio::test]
    async fn probe_when_symlink_unsupported_flips_only_symlink() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/tmp", 0o755);
        fs.fail_symlink();
        let features = probe_with_scratch_root(&fs, &s(), "/tmp").await;
        assert!(features.setstat_supported);
        assert!(!features.symlink_supported);
    }
}
