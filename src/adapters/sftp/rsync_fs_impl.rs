//! Production [`RsyncSftpFsPort`] adapter backed by `russh-sftp`.
//!
//! Distinct from [`RusshSftpAdapter`] (legacy `ssh_upload` / `ssh_download`):
//! exposes a fine-grained "remote filesystem" surface
//! (`readdir`, `lstat`, `read_link`, `mkdir`, `rmdir`, `remove_file`,
//! `symlink`, `set_metadata`, `read_chunk`, `write_chunk`) that the
//! ADR 0011 SFTP rsync transport drives directly.
//!
//! Each port operation opens its own short-lived SFTP channel through
//! the shared [`SshHandleRegistry`], reuses the existing
//! [`open_sftp_session`] helper, and surfaces underlying SFTP errors as
//! [`DomainError::Sftp`]. The single exception is the SFTP server-side
//! `OP_UNSUPPORTED` (`StatusCode::OpUnsupported`, code 8) — translated to
//! [`DomainError::SftpFeatureMissing`] so the rsync use case can route
//! the request to the Wire transport (or surface an actionable
//! `SFTP_FEATURE_MISSING` error to the LLM) instead of crashing the
//! sync mid-walk.
//!
//! ## Lock-free contract
//!
//! No new `Mutex`. The shared [`SshHandleRegistry`] is `DashMap` only;
//! every port method clones the `Arc<russh::client::Handle>` out of the
//! shard guard before opening the SFTP channel, so the guard never
//! crosses an `await` boundary.

use std::io::SeekFrom;
use std::sync::Arc;

use bytes::Bytes;
use russh::client;
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, FileType, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::adapters::sftp::internal::sftp::open_sftp_session;
use crate::adapters::sftp::russh_sftp_adapter::SshHandleRegistry;
use crate::adapters::ssh::internal::session::SshClientHandler;
use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::ports::rsync_sftp_fs::{RemoteDirEntry, RemoteMetadata, RsyncSftpFsPort};

/// Production [`RsyncSftpFsPort`] adapter.
///
/// Holds a clone of the shared [`SshHandleRegistry`] populated by the
/// SSH adapter. Cheap to clone (`Arc` only).
#[derive(Debug, Clone)]
pub struct RusshRsyncSftpFs {
    handle_registry: SshHandleRegistry,
}

impl RusshRsyncSftpFs {
    /// Build the adapter.
    #[must_use]
    pub const fn new(handle_registry: SshHandleRegistry) -> Self {
        Self { handle_registry }
    }

    /// Resolve the russh handle for `session_id` or surface
    /// [`DomainError::SessionNotFound`].
    fn resolve_handle(
        &self,
        session_id: &SessionId,
    ) -> Result<Arc<client::Handle<SshClientHandler>>, DomainError> {
        self.handle_registry
            .get(session_id)
            .ok_or_else(|| DomainError::SessionNotFound(session_id.clone()))
    }

    /// Open a fresh SFTP session on the live russh handle. Each port
    /// call opens its own session — keeps the lock-free contract simple
    /// and matches the existing legacy upload / download pattern.
    async fn open_session(&self, session_id: &SessionId) -> Result<SftpSession, DomainError> {
        let handle = self.resolve_handle(session_id)?;
        open_sftp_session(&handle)
            .await
            .map_err(|e| DomainError::Sftp(format!("open session: {e}")))
    }
}

impl RsyncSftpFsPort for RusshRsyncSftpFs {
    async fn readdir(
        &self,
        session_id: &SessionId,
        path: &str,
    ) -> Result<Vec<RemoteDirEntry>, DomainError> {
        let sftp = self.open_session(session_id).await?;
        let read_dir = sftp
            .read_dir(path.to_string())
            .await
            .map_err(|e| translate_sftp_error("readdir", &e))?;
        let mut out = Vec::new();
        for entry in read_dir {
            let metadata = entry.metadata();
            out.push(RemoteDirEntry {
                name: entry.file_name(),
                is_dir: metadata.is_dir(),
                is_symlink: metadata.is_symlink(),
                size: metadata.size.unwrap_or_default(),
                mode: extract_mode_bits(&metadata),
                mtime: i64::from(metadata.mtime.unwrap_or_default()),
                uid: metadata.uid.unwrap_or_default(),
                gid: metadata.gid.unwrap_or_default(),
            });
        }
        let _ = sftp.close().await;
        Ok(out)
    }

    async fn lstat(
        &self,
        session_id: &SessionId,
        path: &str,
    ) -> Result<RemoteMetadata, DomainError> {
        let sftp = self.open_session(session_id).await?;
        let attrs = sftp
            .symlink_metadata(path.to_string())
            .await
            .map_err(|e| translate_sftp_error("lstat", &e))?;
        let meta = RemoteMetadata {
            size: attrs.size.unwrap_or_default(),
            mode: extract_mode_bits(&attrs),
            mtime: i64::from(attrs.mtime.unwrap_or_default()),
            uid: attrs.uid.unwrap_or_default(),
            gid: attrs.gid.unwrap_or_default(),
            is_dir: matches!(attrs.file_type(), FileType::Dir),
            is_symlink: matches!(attrs.file_type(), FileType::Symlink),
        };
        let _ = sftp.close().await;
        Ok(meta)
    }

    async fn read_link(&self, session_id: &SessionId, path: &str) -> Result<String, DomainError> {
        let sftp = self.open_session(session_id).await?;
        let target = sftp
            .read_link(path.to_string())
            .await
            .map_err(|e| translate_sftp_error("read_link", &e))?;
        let _ = sftp.close().await;
        Ok(target)
    }

    async fn mkdir(
        &self,
        session_id: &SessionId,
        path: &str,
        mode: u32,
    ) -> Result<(), DomainError> {
        let sftp = self.open_session(session_id).await?;
        sftp.create_dir(path.to_string())
            .await
            .map_err(|e| translate_sftp_error("mkdir", &e))?;
        // russh-sftp's `create_dir` does not accept mode bits; follow up
        // with a setstat so the directory carries the requested perms.
        if mode != 0 {
            let mut attrs = FileAttributes::empty();
            attrs.permissions = Some(mode & 0o7777);
            // Best-effort — if the server rejects setstat the directory
            // still exists with the umask-default mode. Translate the
            // error so callers can route via Wire when needed.
            sftp.set_metadata(path.to_string(), attrs)
                .await
                .map_err(|e| translate_sftp_error("mkdir setstat", &e))?;
        }
        let _ = sftp.close().await;
        Ok(())
    }

    async fn rmdir(&self, session_id: &SessionId, path: &str) -> Result<(), DomainError> {
        let sftp = self.open_session(session_id).await?;
        sftp.remove_dir(path.to_string())
            .await
            .map_err(|e| translate_sftp_error("rmdir", &e))?;
        let _ = sftp.close().await;
        Ok(())
    }

    async fn remove_file(&self, session_id: &SessionId, path: &str) -> Result<(), DomainError> {
        let sftp = self.open_session(session_id).await?;
        sftp.remove_file(path.to_string())
            .await
            .map_err(|e| translate_sftp_error("remove_file", &e))?;
        let _ = sftp.close().await;
        Ok(())
    }

    async fn symlink(
        &self,
        session_id: &SessionId,
        target: &str,
        link_path: &str,
    ) -> Result<(), DomainError> {
        let sftp = self.open_session(session_id).await?;
        // OpenSSH's sftp-server has a long-standing bug where
        // `SSH_FXP_SYMLINK`'s argument order is swapped relative to the
        // SFTP draft (linkpath ↔ targetpath). russh-sftp follows the
        // draft (`symlink(linkpath, targetpath)`), so to land on the
        // OpenSSH wire we pass `(target_first, link_second)`. This is
        // documented in the OpenSSH PROTOCOL.1 doc — the canonical
        // workaround used by every interoperability client. See also
        // the SFTP server compatibility notes in MIGRATION.md.
        sftp.symlink(target.to_string(), link_path.to_string())
            .await
            .map_err(|e| translate_sftp_error("symlink", &e))?;
        let _ = sftp.close().await;
        Ok(())
    }

    async fn set_metadata(
        &self,
        session_id: &SessionId,
        path: &str,
        meta: RemoteMetadata,
    ) -> Result<(), DomainError> {
        let sftp = self.open_session(session_id).await?;
        let attrs = remote_meta_to_attrs(meta);
        sftp.set_metadata(path.to_string(), attrs)
            .await
            .map_err(|e| translate_sftp_error("set_metadata", &e))?;
        let _ = sftp.close().await;
        Ok(())
    }

    async fn read_chunk(
        &self,
        session_id: &SessionId,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Bytes, DomainError> {
        let sftp = self.open_session(session_id).await?;
        let mut file = sftp
            .open_with_flags(path.to_string(), OpenFlags::READ)
            .await
            .map_err(|e| translate_sftp_error("read_chunk open", &e))?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| DomainError::Sftp(format!("read_chunk seek: {e}")))?;
        }
        let mut buf = vec![0_u8; len];
        let mut filled = 0_usize;
        while filled < len {
            let n = file
                .read(&mut buf[filled..])
                .await
                .map_err(|e| DomainError::Sftp(format!("read_chunk read: {e}")))?;
            if n == 0 {
                break;
            }
            filled = filled.saturating_add(n);
        }
        buf.truncate(filled);
        // Best-effort shutdown — the streaming `File::poll_shutdown`
        // flushes the SFTP close request.
        let _ = file.shutdown().await;
        let _ = sftp.close().await;
        Ok(Bytes::from(buf))
    }

    async fn write_chunk(
        &self,
        session_id: &SessionId,
        path: &str,
        offset: u64,
        data: Bytes,
    ) -> Result<(), DomainError> {
        let sftp = self.open_session(session_id).await?;
        // First chunk (offset 0) creates / truncates; subsequent chunks
        // append at the explicit offset (no truncation).
        let flags = if offset == 0 {
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE
        } else {
            OpenFlags::CREATE | OpenFlags::WRITE
        };
        let mut file = sftp
            .open_with_flags(path.to_string(), flags)
            .await
            .map_err(|e| translate_sftp_error("write_chunk open", &e))?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| DomainError::Sftp(format!("write_chunk seek: {e}")))?;
        }
        file.write_all(&data)
            .await
            .map_err(|e| DomainError::Sftp(format!("write_chunk write: {e}")))?;
        file.shutdown()
            .await
            .map_err(|e| DomainError::Sftp(format!("write_chunk close: {e}")))?;
        let _ = sftp.close().await;
        Ok(())
    }
}

/// Pull the POSIX mode bits (perm + `S_IF`*) out of a russh-sftp
/// [`FileAttributes`].
///
/// Files without an explicit `permissions` field collapse to `0` so the
/// caller can default to the executor's `0o644` / `0o755` policy.
fn extract_mode_bits(attrs: &FileAttributes) -> u32 {
    attrs.permissions.unwrap_or_default()
}

/// Translate a [`RemoteMetadata`] snapshot into a setstat
/// [`FileAttributes`]. Drops the `is_dir` / `is_symlink` flags — those
/// are never set via setstat (the SFTP server determines them at create
/// time).
fn remote_meta_to_attrs(meta: RemoteMetadata) -> FileAttributes {
    let mut attrs = FileAttributes::empty();
    if meta.mode != 0 {
        attrs.permissions = Some(meta.mode & 0o7777);
    }
    let mtime_u32 = u32::try_from(meta.mtime).unwrap_or_default();
    if mtime_u32 != 0 {
        attrs.mtime = Some(mtime_u32);
    }
    if meta.uid != 0 {
        attrs.uid = Some(meta.uid);
    }
    if meta.gid != 0 {
        attrs.gid = Some(meta.gid);
    }
    attrs
}

/// Translate a russh-sftp client error into a [`DomainError`].
///
/// SFTP `OP_UNSUPPORTED` (status code 8) routes to
/// [`DomainError::SftpFeatureMissing`] so the rsync use case can either
/// fall back to the Wire transport (when running through `Auto`) or
/// surface a stable `SFTP_FEATURE_MISSING` to the LLM. Every other
/// status code, IO error, timeout, etc. collapses to
/// [`DomainError::Sftp`] with a descriptive prefix.
fn translate_sftp_error(op: &str, err: &SftpError) -> DomainError {
    if let SftpError::Status(status) = err
        && matches!(status.status_code, StatusCode::OpUnsupported)
    {
        return DomainError::SftpFeatureMissing(format!(
            "{op}: SFTP server returned OP_UNSUPPORTED ({})",
            status.error_message
        ));
    }
    DomainError::Sftp(format!("{op}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::{RusshRsyncSftpFs, remote_meta_to_attrs, translate_sftp_error};
    use crate::adapters::sftp::russh_sftp_adapter::SshHandleRegistry;
    use crate::domain::error::DomainError;
    use crate::domain::ids::SessionId;
    use crate::ports::rsync_sftp_fs::{RemoteMetadata, RsyncSftpFsPort};
    use bytes::Bytes;
    use russh_sftp::client::error::Error as SftpError;
    use russh_sftp::protocol::{Status, StatusCode};

    fn fake_status(code: StatusCode, msg: &str) -> SftpError {
        SftpError::Status(Status {
            id: 0,
            status_code: code,
            error_message: msg.to_string(),
            language_tag: String::new(),
        })
    }

    #[test]
    fn translate_op_unsupported_routes_to_feature_missing() {
        let err = translate_sftp_error("symlink", &fake_status(StatusCode::OpUnsupported, "nope"));
        match err {
            DomainError::SftpFeatureMissing(msg) => {
                assert!(msg.contains("symlink"));
                assert!(msg.contains("OP_UNSUPPORTED"));
            }
            other => panic!("expected SftpFeatureMissing, got {other:?}"),
        }
    }

    #[test]
    fn translate_other_status_code_routes_to_sftp() {
        let err = translate_sftp_error("readdir", &fake_status(StatusCode::PermissionDenied, "x"));
        match err {
            DomainError::Sftp(msg) => assert!(msg.starts_with("readdir:")),
            other => panic!("expected Sftp, got {other:?}"),
        }
    }

    #[test]
    fn translate_timeout_routes_to_sftp() {
        let err = translate_sftp_error("lstat", &SftpError::Timeout);
        assert!(matches!(err, DomainError::Sftp(_)));
    }

    #[test]
    fn remote_meta_to_attrs_emits_only_set_fields() {
        let meta = RemoteMetadata {
            size: 0,
            mode: 0o755,
            mtime: 1_700_000_000,
            uid: 1000,
            gid: 100,
            is_dir: false,
            is_symlink: false,
        };
        let attrs = remote_meta_to_attrs(meta);
        assert_eq!(attrs.permissions, Some(0o755));
        assert_eq!(attrs.mtime, Some(1_700_000_000));
        assert_eq!(attrs.uid, Some(1000));
        assert_eq!(attrs.gid, Some(100));
    }

    #[test]
    fn remote_meta_to_attrs_skips_zero_fields() {
        let meta = RemoteMetadata {
            size: 0,
            mode: 0,
            mtime: 0,
            uid: 0,
            gid: 0,
            is_dir: false,
            is_symlink: false,
        };
        let attrs = remote_meta_to_attrs(meta);
        assert!(attrs.permissions.is_none());
        assert!(attrs.mtime.is_none());
        assert!(attrs.uid.is_none());
        assert!(attrs.gid.is_none());
    }

    #[test]
    fn remote_meta_to_attrs_strips_mode_to_perm_bits() {
        let meta = RemoteMetadata {
            size: 0,
            mode: 0o100_644,
            mtime: 0,
            uid: 0,
            gid: 0,
            is_dir: false,
            is_symlink: false,
        };
        let attrs = remote_meta_to_attrs(meta);
        // S_IFREG | 0o644 -> 0o644 (low 12 bits including suid/sgid/sticky).
        assert_eq!(attrs.permissions, Some(0o644));
    }

    #[tokio::test]
    async fn readdir_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .readdir(&SessionId::new("missing".to_string()), "/")
            .await
            .expect_err("must err");
        match err {
            DomainError::SessionNotFound(id) => assert_eq!(id.as_str(), "missing"),
            other => panic!("expected SessionNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lstat_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .lstat(&SessionId::new("missing".to_string()), "/")
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn read_chunk_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .read_chunk(&SessionId::new("missing".to_string()), "/x", 0, 32)
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn write_chunk_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .write_chunk(
                &SessionId::new("missing".to_string()),
                "/x",
                0,
                Bytes::from_static(b"x"),
            )
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn symlink_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .symlink(&SessionId::new("missing".to_string()), "/target", "/link")
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn set_metadata_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let meta = RemoteMetadata {
            size: 0,
            mode: 0o644,
            mtime: 0,
            uid: 0,
            gid: 0,
            is_dir: false,
            is_symlink: false,
        };
        let err = fs
            .set_metadata(&SessionId::new("missing".to_string()), "/x", meta)
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn mkdir_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .mkdir(&SessionId::new("missing".to_string()), "/d", 0o755)
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn rmdir_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .rmdir(&SessionId::new("missing".to_string()), "/d")
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn remove_file_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .remove_file(&SessionId::new("missing".to_string()), "/d")
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn read_link_without_session_returns_session_not_found() {
        let fs = RusshRsyncSftpFs::new(SshHandleRegistry::new());
        let err = fs
            .read_link(&SessionId::new("missing".to_string()), "/link")
            .await
            .expect_err("must err");
        assert!(matches!(err, DomainError::SessionNotFound(_)));
    }
}
