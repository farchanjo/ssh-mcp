//! SFTP filesystem port driving the ADR 0011 SFTP rsync transport.
//!
//! Distinct from [`crate::ports::sftp_client::SftpClientPort`] — that
//! port already serves the production `ssh_upload` / `ssh_download`
//! flows and exposes only `upload` / `download` / `cancel`. The rsync
//! SFTP transport needs a lower-level "remote filesystem" surface
//! (`readdir`, `stat`, `mkdir`, `rmdir`, `remove_file`, `symlink`,
//! `read_link`, `set_metadata`, `read_chunk`, `write_chunk`) to drive
//! a recursive mirror without a remote helper.
//!
//! The port stays narrow: every method maps 1:1 onto an SFTP request
//! the russh-sftp library exposes. Adapters owning the live
//! [`russh_sftp::client::SftpSession`] implement the surface; the
//! [`crate::adapters::rsync::sftp::fake`] fake drives the unit and
//! integration tests deterministically.
//!
//! v7.0.0-alpha.3 — first slice. The russh-sftp adapter wiring lands
//! in the next slice; the production composition root threads
//! [`SftpRsyncTransport::without_fs`] until the production adapter is
//! wired so the public MCP surface keeps returning the honest
//! "being implemented" wire error when the transport runs without an
//! `RsyncSftpFsPort`.

use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;

/// Filesystem entry yielded by [`RsyncSftpFsPort::readdir`].
///
/// Mirrors the subset of SFTP `Attributes` the rsync mirror needs;
/// fields the rsync algorithm never inspects (atime, generation, ACL
/// bits) are dropped on the floor so adapter implementations stay
/// minimal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDirEntry {
    /// File name relative to the directory just read; never includes a
    /// trailing slash.
    pub name: String,
    /// Whether the entry is a directory (drives the BFS recursion).
    pub is_dir: bool,
    /// Whether the entry is a symbolic link (preserved as-is, never
    /// followed).
    pub is_symlink: bool,
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// POSIX mode bits (`S_IFMT` already stripped by the adapter
    /// wherever needed).
    pub mode: u32,
    /// Modification time in unix seconds.
    pub mtime: i64,
    /// Numeric owner uid.
    pub uid: u32,
    /// Numeric owner gid.
    pub gid: u32,
}

/// Snapshot of metadata the executor needs to setstat after writing a
/// file or to skip an unchanged entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteMetadata {
    /// Size in bytes.
    pub size: u64,
    /// POSIX mode bits.
    pub mode: u32,
    /// Modification time in unix seconds.
    pub mtime: i64,
    /// Numeric owner uid.
    pub uid: u32,
    /// Numeric owner gid.
    pub gid: u32,
    /// Whether the path is a directory.
    pub is_dir: bool,
    /// Whether the path is a symbolic link.
    pub is_symlink: bool,
}

/// SFTP filesystem port (ADR 0011 SFTP transport). Implementations are
/// async (network I/O).
#[trait_variant::make(RsyncSftpFsPort: Send)]
pub trait LocalRsyncSftpFsPort: Sync {
    /// List the contents of `path` (non-recursive). Empty / missing
    /// directories surface as [`DomainError::Sftp`] with a descriptive
    /// payload.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] for any underlying SFTP error.
    async fn readdir(
        &self,
        session_id: &SessionId,
        path: &str,
    ) -> Result<Vec<RemoteDirEntry>, DomainError>;

    /// Stat a single path (does NOT follow symlinks — analogous to
    /// `lstat`).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] when the path is missing, not
    /// reachable, or the SFTP server rejects the request.
    async fn lstat(
        &self,
        session_id: &SessionId,
        path: &str,
    ) -> Result<RemoteMetadata, DomainError>;

    /// Read the link target for a symbolic link at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] when the path is not a symlink or
    /// the SFTP server rejects the request.
    async fn read_link(&self, session_id: &SessionId, path: &str) -> Result<String, DomainError>;

    /// Create a new directory at `path` with `mode` permission bits.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] when the directory already exists
    /// (the adapter does NOT collapse `EEXIST` to success — the
    /// caller is the right place to make that decision).
    async fn mkdir(&self, session_id: &SessionId, path: &str, mode: u32)
    -> Result<(), DomainError>;

    /// Remove an empty directory at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] if the directory is non-empty or
    /// missing.
    async fn rmdir(&self, session_id: &SessionId, path: &str) -> Result<(), DomainError>;

    /// Remove a file or symlink at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] if the path does not exist or
    /// names a non-empty directory.
    async fn remove_file(&self, session_id: &SessionId, path: &str) -> Result<(), DomainError>;

    /// Create a symbolic link `link_path` whose target is `target`.
    /// Mirrors POSIX `symlink(target, link_path)` argument order.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] when the SFTP server rejects the
    /// request (most often `OP_UNSUPPORTED` on locked-down servers
    /// — caller maps that to `SftpFeatureMissing`).
    async fn symlink(
        &self,
        session_id: &SessionId,
        target: &str,
        link_path: &str,
    ) -> Result<(), DomainError>;

    /// Apply metadata (`mode`, `mtime`, `uid`, `gid`) to `path`. The
    /// adapter chooses the minimum SFTP setstat round-trips to apply
    /// every supplied field.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] when the SFTP server rejects any
    /// component of the setstat request.
    async fn set_metadata(
        &self,
        session_id: &SessionId,
        path: &str,
        meta: RemoteMetadata,
    ) -> Result<(), DomainError>;

    /// Read up to `len` bytes from `path` starting at `offset`. May
    /// return fewer bytes than requested at EOF.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] on read failure.
    async fn read_chunk(
        &self,
        session_id: &SessionId,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Bytes, DomainError>;

    /// Write `data` to `path` starting at `offset`. The first call
    /// against a path implicitly creates / truncates it (mode 0o644 by
    /// default; the executor follows up with [`Self::set_metadata`]
    /// when preserve flags ask for explicit perms).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] on write failure.
    async fn write_chunk(
        &self,
        session_id: &SessionId,
        path: &str,
        offset: u64,
        data: Bytes,
    ) -> Result<(), DomainError>;
}

#[cfg(test)]
mod tests {
    use super::RsyncSftpFsPort;

    fn _assert_port<T: RsyncSftpFsPort>() {}
}
