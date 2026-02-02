//! SFTP helpers for file transfer operations.
//!
//! This module provides functions for opening SFTP sessions and streaming
//! file transfers with progress tracking.
//!
//! # Architecture
//!
//! - `open_sftp_session`: Opens an SFTP subsystem on an SSH channel
//! - `resolve_local_path`: Cross-platform path resolution (relative -> home dir)
//! - `sftp_upload_streaming`: Streams a local file to remote via SFTP
//! - `sftp_download_streaming`: Streams a remote file to local via SFTP

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use russh::client;
use russh_sftp::client::SftpSession;
use russh_sftp::client::fs::File as SftpFile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::session::SshClientHandler;
use super::transfer::{CHUNK_SIZE, TransferStatus};

/// Open an SFTP session on the given SSH handle.
///
/// Opens a new session channel, requests the "sftp" subsystem, and
/// creates an `SftpSession` from the channel stream.
pub(crate) async fn open_sftp_session(
    handle: &Arc<client::Handle<SshClientHandler>>,
) -> Result<SftpSession, String> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("Failed to open channel for SFTP: {}", e))?;

    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("Failed to request SFTP subsystem: {}", e))?;

    SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("Failed to initialize SFTP session: {}", e))
}

/// Resolve a local path, expanding relative paths against the home directory.
///
/// - Absolute paths are returned as-is.
/// - Relative paths are joined with the user's home directory.
/// - Falls back to current directory if home directory is unavailable.
pub(crate) fn resolve_local_path(path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        home_dir().unwrap_or_else(|| PathBuf::from(".")).join(p)
    }
}

/// Get the user's home directory from environment variables.
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

/// Stream a local file to a remote path via SFTP.
///
/// Reads the local file in 32KB chunks and writes to the remote file,
/// updating the progress counter after each chunk.
///
/// # Arguments
///
/// * `handle` - SSH session handle
/// * `local_path` - Resolved local file path
/// * `remote_path` - Remote destination path
/// * `bytes_transferred` - Atomic counter for tracking progress
/// * `cancel_token` - Token to signal cancellation
/// * `status_tx` - Channel to send status updates
/// * `error` - Shared storage for error messages
pub(crate) async fn sftp_upload_streaming(
    handle: Arc<client::Handle<SshClientHandler>>,
    local_path: PathBuf,
    remote_path: String,
    bytes_transferred: Arc<AtomicU64>,
    cancel_token: CancellationToken,
    status_tx: watch::Sender<TransferStatus>,
    error: Arc<Mutex<Option<String>>>,
) {
    let result = sftp_upload_inner(
        &handle,
        &local_path,
        &remote_path,
        &bytes_transferred,
        &cancel_token,
    )
    .await;

    match result {
        Ok(cancelled) => {
            if cancelled {
                info!(
                    "SFTP upload cancelled: {} -> {}",
                    local_path.display(),
                    remote_path
                );
                let _ = status_tx.send(TransferStatus::Cancelled);
            } else {
                info!(
                    "SFTP upload completed: {} -> {} ({} bytes)",
                    local_path.display(),
                    remote_path,
                    bytes_transferred.load(Ordering::SeqCst)
                );
                let _ = status_tx.send(TransferStatus::Completed);
            }
        }
        Err(e) => {
            error!(
                "SFTP upload failed: {} -> {}: {}",
                local_path.display(),
                remote_path,
                e
            );
            *error.lock().await = Some(e);
            let _ = status_tx.send(TransferStatus::Failed);
        }
    }
}

/// Inner upload logic, returns Ok(true) if cancelled, Ok(false) if completed.
async fn sftp_upload_inner(
    handle: &Arc<client::Handle<SshClientHandler>>,
    local_path: &Path,
    remote_path: &str,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
) -> Result<bool, String> {
    let sftp = open_sftp_session(handle).await?;

    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| format!("Failed to open local file: {}", e))?;

    let mut remote_file = sftp
        .create(remote_path)
        .await
        .map_err(|e| format!("Failed to create remote file: {}", e))?;

    let mut buf = vec![0u8; CHUNK_SIZE];

    loop {
        if cancel_token.is_cancelled() {
            let _ = remote_file.shutdown().await;
            return Ok(true);
        }

        let n = local_file
            .read(&mut buf)
            .await
            .map_err(|e| format!("Failed to read local file: {}", e))?;

        if n == 0 {
            break;
        }

        write_to_sftp_file(&mut remote_file, &buf[..n]).await?;
        bytes_transferred.fetch_add(n as u64, Ordering::SeqCst);
    }

    remote_file
        .shutdown()
        .await
        .map_err(|e| format!("Failed to flush remote file: {}", e))?;

    Ok(false)
}

/// Write a buffer to an SFTP file.
async fn write_to_sftp_file(file: &mut SftpFile, data: &[u8]) -> Result<(), String> {
    file.write_all(data)
        .await
        .map_err(|e| format!("Failed to write to remote file: {}", e))
}

/// Stream a remote file to a local path via SFTP.
///
/// Reads the remote file in 32KB chunks and writes to the local file,
/// updating the progress counter after each chunk.
///
/// # Arguments
///
/// * `handle` - SSH session handle
/// * `remote_path` - Remote source path
/// * `local_path` - Resolved local destination path
/// * `bytes_transferred` - Atomic counter for tracking progress
/// * `cancel_token` - Token to signal cancellation
/// * `status_tx` - Channel to send status updates
/// * `error` - Shared storage for error messages
pub(crate) async fn sftp_download_streaming(
    handle: Arc<client::Handle<SshClientHandler>>,
    remote_path: String,
    local_path: PathBuf,
    bytes_transferred: Arc<AtomicU64>,
    cancel_token: CancellationToken,
    status_tx: watch::Sender<TransferStatus>,
    error: Arc<Mutex<Option<String>>>,
) {
    let result = sftp_download_inner(
        &handle,
        &remote_path,
        &local_path,
        &bytes_transferred,
        &cancel_token,
    )
    .await;

    match result {
        Ok(cancelled) => {
            if cancelled {
                info!(
                    "SFTP download cancelled: {} -> {}",
                    remote_path,
                    local_path.display()
                );
                let _ = status_tx.send(TransferStatus::Cancelled);
            } else {
                info!(
                    "SFTP download completed: {} -> {} ({} bytes)",
                    remote_path,
                    local_path.display(),
                    bytes_transferred.load(Ordering::SeqCst)
                );
                let _ = status_tx.send(TransferStatus::Completed);
            }
        }
        Err(e) => {
            error!(
                "SFTP download failed: {} -> {}: {}",
                remote_path,
                local_path.display(),
                e
            );
            *error.lock().await = Some(e);
            let _ = status_tx.send(TransferStatus::Failed);
        }
    }
}

/// Inner download logic, returns Ok(true) if cancelled, Ok(false) if completed.
async fn sftp_download_inner(
    handle: &Arc<client::Handle<SshClientHandler>>,
    remote_path: &str,
    local_path: &Path,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
) -> Result<bool, String> {
    let sftp = open_sftp_session(handle).await?;

    let mut remote_file = sftp
        .open(remote_path)
        .await
        .map_err(|e| format!("Failed to open remote file: {}", e))?;

    let mut local_file = tokio::fs::File::create(local_path)
        .await
        .map_err(|e| format!("Failed to create local file: {}", e))?;

    let mut buf = vec![0u8; CHUNK_SIZE];

    loop {
        if cancel_token.is_cancelled() {
            let _ = local_file.shutdown().await;
            return Ok(true);
        }

        let n = remote_file
            .read(&mut buf)
            .await
            .map_err(|e| format!("Failed to read remote file: {}", e))?;

        if n == 0 {
            break;
        }

        local_file
            .write_all(&buf[..n])
            .await
            .map_err(|e| format!("Failed to write local file: {}", e))?;

        bytes_transferred.fetch_add(n as u64, Ordering::SeqCst);
    }

    local_file
        .flush()
        .await
        .map_err(|e| format!("Failed to flush local file: {}", e))?;

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod resolve_local_path {
        use super::*;

        #[test]
        fn test_absolute_path_returned_as_is() {
            let path = resolve_local_path("/tmp/file.txt");
            assert_eq!(path, PathBuf::from("/tmp/file.txt"));
        }

        #[test]
        fn test_relative_path_resolved_to_home() {
            let path = resolve_local_path("file.txt");
            // Should not be just "file.txt" - it should be joined with home or "."
            assert!(path.is_absolute() || path.starts_with("."));
        }

        #[test]
        fn test_relative_path_with_subdirectory() {
            let path = resolve_local_path("subdir/file.txt");
            let path_str = path.to_string_lossy();
            assert!(path_str.ends_with("subdir/file.txt"));
        }

        #[test]
        fn test_absolute_path_with_spaces() {
            let path = resolve_local_path("/tmp/my files/doc.txt");
            assert_eq!(path, PathBuf::from("/tmp/my files/doc.txt"));
        }
    }

    mod home_dir_fn {
        use super::*;

        #[test]
        fn test_home_dir_returns_some_on_most_systems() {
            // On CI/local systems HOME should typically be set
            let result = home_dir();
            // We can't assert Some on all platforms, but verify it doesn't panic
            if let Some(dir) = result {
                assert!(dir.is_absolute());
            }
        }
    }
}
