//! SFTP client port.
//!
//! Abstracts the russh-sftp adapter so use cases can drive uploads,
//! downloads, and progress polling without depending on
//! `russh_sftp::client::SftpSession` directly.

use crate::domain::error::DomainError;
use crate::domain::ids::{SessionId, TransferId};
use crate::domain::transfer::TransferEntity;

/// Description of an upload request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRequest {
    /// Session that owns the SFTP channel.
    pub session_id: SessionId,
    /// Source path on the local filesystem.
    pub local_path: String,
    /// Destination path on the remote host.
    pub remote_path: String,
}

/// Description of a download request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    /// Session that owns the SFTP channel.
    pub session_id: SessionId,
    /// Source path on the remote host.
    pub remote_path: String,
    /// Destination path on the local filesystem.
    pub local_path: String,
}

/// SFTP client port. Implementations are async (network I/O).
#[trait_variant::make(SftpClientPort: Send)]
pub trait LocalSftpClientPort: Send + Sync {
    /// Spawn an upload and return the initial transfer entity (status
    /// `Running`). Progress is published through the
    /// [`crate::ports::output_stream::OutputStreamPort`] adapter.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::MaxTransfersExceeded`, `DomainError::Sftp`, or
    /// `DomainError::InvalidArgument` depending on the failure point.
    async fn upload(
        &self,
        transfer_id: TransferId,
        request: UploadRequest,
    ) -> Result<TransferEntity, DomainError>;

    /// Spawn a download and return the initial transfer entity.
    ///
    /// # Errors
    ///
    /// Returns the same variants as [`Self::upload`].
    async fn download(
        &self,
        transfer_id: TransferId,
        request: DownloadRequest,
    ) -> Result<TransferEntity, DomainError>;

    /// Cancel a running transfer.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::TransferNotFound` if the id does not match a
    /// live transfer, or `DomainError::Sftp` for SFTP-level faults.
    async fn cancel(&self, transfer_id: &TransferId) -> Result<(), DomainError>;
}

#[cfg(test)]
mod tests {
    use super::SftpClientPort;

    fn _assert_port<T: SftpClientPort>() {}
}
