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

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use russh::client;
use russh_sftp::client::SftpSession;
use russh_sftp::client::fs::File as SftpFile;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::session::SshClientHandler;
use super::transfer::{CHUNK_SIZE, TransferStatus};

/// Classify a raw transfer error into a structured, AI-identifiable error message.
///
/// Pattern-matches the raw error string (case-insensitive) and returns a
/// `[CODE] operation: human-readable detail (raw: original)` formatted message.
///
/// # Error Codes
///
/// | Code | Meaning |
/// |------|---------|
/// | `FILE_NOT_FOUND` | Local or remote file doesn't exist |
/// | `PERMISSION_DENIED` | Insufficient permissions |
/// | `DISK_FULL` | No space left on device |
/// | `CONNECTION_LOST` | SSH connection dropped mid-transfer |
/// | `REMOTE_DIR_NOT_FOUND` | Remote parent directory missing |
/// | `READ_ONLY_FS` | Target filesystem is read-only |
/// | `SFTP_PROTOCOL` | SFTP channel/subsystem failure |
/// | `TIMEOUT` | Operation timed out |
/// | `IO_ERROR` | Generic IO fallback |
#[must_use]
pub fn classify_transfer_error(operation: &str, raw_error: &str) -> String {
    let lower = raw_error.to_lowercase();
    let (code, detail) = match_error_pattern(&lower, operation);
    format!("[{code}] {operation}: {detail} (raw: {raw_error})")
}

/// Match a lowercased error string to a structured error code and detail message.
fn match_error_pattern<'a>(lower: &str, operation: &str) -> (&'a str, &'a str) {
    if lower.contains("read-only file system") || lower.contains("read only file system") {
        ("READ_ONLY_FS", "target filesystem is read-only")
    } else if lower.contains("no space left on device") {
        ("DISK_FULL", "no space left on device")
    } else if lower.contains("permission denied") {
        ("PERMISSION_DENIED", "insufficient permissions")
    } else if lower.contains("broken pipe")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
        || lower.contains("no route to host")
    {
        ("CONNECTION_LOST", "SSH connection lost during transfer")
    } else if lower.contains("timed out") || lower.contains("timeout") {
        ("TIMEOUT", "operation timed out")
    } else if (lower.contains("no such file") && operation.contains("create"))
        || (lower.contains("not a directory") && operation.contains("create"))
    {
        ("REMOTE_DIR_NOT_FOUND", "parent directory does not exist")
    } else if lower.contains("no such file") || lower.contains("not found") {
        ("FILE_NOT_FOUND", "file does not exist")
    } else if lower.contains("channel")
        || lower.contains("subsystem")
        || lower.contains("sftp")
        || lower.contains("session")
    {
        ("SFTP_PROTOCOL", "SFTP protocol/channel error")
    } else {
        ("IO_ERROR", "I/O error")
    }
}

/// Open an SFTP session on the given SSH handle.
///
/// Opens a new session channel, requests the "sftp" subsystem, and
/// creates an `SftpSession` from the channel stream.
pub async fn open_sftp_session(
    handle: &Arc<client::Handle<SshClientHandler>>,
) -> Result<SftpSession, String> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| classify_transfer_error("open SFTP channel", &e.to_string()))?;

    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| classify_transfer_error("request SFTP subsystem", &e.to_string()))?;

    SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| classify_transfer_error("initialize SFTP session", &e.to_string()))
}

/// Resolve a local path, expanding `~` and relative paths against the home directory.
///
/// - Paths starting with `~/` are expanded to the user's home directory.
/// - Absolute paths are returned as-is.
/// - Relative paths are joined with the user's home directory.
/// - Falls back to current directory if home directory is unavailable.
#[must_use]
pub fn resolve_local_path(path: &str) -> PathBuf {
    let expanded = expand_tilde(path);
    let p = Path::new(&expanded);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        home_dir().unwrap_or_else(|| PathBuf::from(".")).join(p)
    }
}

/// Expand a leading `~` or `~/` to the user's home directory.
///
/// - `~` alone resolves to the home directory.
/// - `~/path` resolves to `home_dir/path`.
/// - All other paths are returned unchanged.
#[must_use]
pub fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return home_dir().map_or_else(|| "~".to_string(), |h| h.to_string_lossy().into_owned());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().map_or_else(|| path.to_string(), |h| format!("{}/{rest}", h.display()));
    }
    path.to_string()
}

/// Get the user's home directory from environment variables.
fn home_dir() -> Option<PathBuf> {
    env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
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
pub async fn sftp_upload_streaming(
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

    handle_transfer_result(
        result,
        "upload",
        &local_path,
        &remote_path,
        &bytes_transferred,
        &status_tx,
        &error,
    )
    .await;
}

/// Handles the result of a transfer operation, logging and updating status.
async fn handle_transfer_result(
    result: Result<bool, String>,
    direction: &str,
    local_path: &Path,
    remote_path: &str,
    bytes_transferred: &Arc<AtomicU64>,
    status_tx: &watch::Sender<TransferStatus>,
    error_store: &Arc<Mutex<Option<String>>>,
) {
    match result {
        Ok(true) => {
            info!(
                "SFTP {direction} cancelled: {remote_path} <-> {}",
                local_path.display()
            );
            let _ = status_tx.send(TransferStatus::Cancelled);
        }
        Ok(false) => {
            let bytes = bytes_transferred.load(Ordering::SeqCst);
            info!(
                "SFTP {direction} completed: {remote_path} <-> {} ({bytes} bytes)",
                local_path.display()
            );
            let _ = status_tx.send(TransferStatus::Completed);
        }
        Err(e) => {
            error!(
                "SFTP {direction} failed: {remote_path} <-> {}: {e}",
                local_path.display()
            );
            *error_store.lock().await = Some(e);
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
    let mut local_file = open_local_file(local_path).await?;
    let mut remote_file = create_remote_file(&sftp, remote_path).await?;

    let cancelled = upload_chunks(
        &mut local_file,
        &mut remote_file,
        local_path,
        remote_path,
        bytes_transferred,
        cancel_token,
    )
    .await?;

    if !cancelled {
        flush_remote_file(&mut remote_file, remote_path).await?;
    }

    Ok(cancelled)
}

/// Opens a local file for reading.
async fn open_local_file(local_path: &Path) -> Result<File, String> {
    File::open(local_path).await.map_err(|e| {
        classify_transfer_error(
            &format!("open local file '{}'", local_path.display()),
            &e.to_string(),
        )
    })
}

/// Creates a remote file via SFTP for writing.
async fn create_remote_file(sftp: &SftpSession, remote_path: &str) -> Result<SftpFile, String> {
    sftp.create(remote_path).await.map_err(|e| {
        classify_transfer_error(
            &format!("create remote file '{remote_path}'"),
            &e.to_string(),
        )
    })
}

/// Flushes and shuts down a remote SFTP file.
async fn flush_remote_file(remote_file: &mut SftpFile, remote_path: &str) -> Result<(), String> {
    remote_file.shutdown().await.map_err(|e| {
        classify_transfer_error(
            &format!("flush remote file '{remote_path}'"),
            &e.to_string(),
        )
    })
}

/// Reads chunks from a local file and writes them to a remote SFTP file.
///
/// Returns `Ok(true)` if the transfer was cancelled, `Ok(false)` if completed.
async fn upload_chunks(
    local_file: &mut File,
    remote_file: &mut SftpFile,
    local_path: &Path,
    remote_path: &str,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
) -> Result<bool, String> {
    let mut buf = vec![0_u8; CHUNK_SIZE];

    loop {
        if cancel_token.is_cancelled() {
            let _ = remote_file.shutdown().await;
            return Ok(true);
        }

        let n = local_file.read(&mut buf).await.map_err(|e| {
            classify_transfer_error(
                &format!("read local file '{}'", local_path.display()),
                &e.to_string(),
            )
        })?;

        if n == 0 {
            return Ok(false);
        }

        write_to_sftp_file(remote_file, &buf[..n], remote_path).await?;
        bytes_transferred.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::SeqCst);
    }
}

/// Write a buffer to an SFTP file.
async fn write_to_sftp_file(
    file: &mut SftpFile,
    data: &[u8],
    remote_path: &str,
) -> Result<(), String> {
    file.write_all(data).await.map_err(|e| {
        classify_transfer_error(
            &format!("write to remote file '{remote_path}'"),
            &e.to_string(),
        )
    })
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
pub async fn sftp_download_streaming(
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

    handle_transfer_result(
        result,
        "download",
        &local_path,
        &remote_path,
        &bytes_transferred,
        &status_tx,
        &error,
    )
    .await;
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
    let mut remote_file = open_remote_file(&sftp, remote_path).await?;
    let mut local_file = create_local_file(local_path).await?;

    let cancelled = download_chunks(
        &mut remote_file,
        &mut local_file,
        remote_path,
        local_path,
        bytes_transferred,
        cancel_token,
    )
    .await?;

    if !cancelled {
        flush_local_file(&mut local_file, local_path).await?;
    }

    Ok(cancelled)
}

/// Opens a remote file via SFTP for reading.
async fn open_remote_file(sftp: &SftpSession, remote_path: &str) -> Result<SftpFile, String> {
    sftp.open(remote_path).await.map_err(|e| {
        classify_transfer_error(&format!("open remote file '{remote_path}'"), &e.to_string())
    })
}

/// Creates a local file for writing.
async fn create_local_file(local_path: &Path) -> Result<File, String> {
    File::create(local_path).await.map_err(|e| {
        classify_transfer_error(
            &format!("create local file '{}'", local_path.display()),
            &e.to_string(),
        )
    })
}

/// Flushes a local file after writing.
async fn flush_local_file(local_file: &mut File, local_path: &Path) -> Result<(), String> {
    local_file.flush().await.map_err(|e| {
        classify_transfer_error(
            &format!("flush local file '{}'", local_path.display()),
            &e.to_string(),
        )
    })
}

/// Reads chunks from a remote SFTP file and writes them to a local file.
///
/// Returns `Ok(true)` if the transfer was cancelled, `Ok(false)` if completed.
async fn download_chunks(
    remote_file: &mut SftpFile,
    local_file: &mut File,
    remote_path: &str,
    local_path: &Path,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
) -> Result<bool, String> {
    let mut buf = vec![0_u8; CHUNK_SIZE];

    loop {
        if cancel_token.is_cancelled() {
            let _ = local_file.shutdown().await;
            return Ok(true);
        }

        let n = remote_file.read(&mut buf).await.map_err(|e| {
            classify_transfer_error(&format!("read remote file '{remote_path}'"), &e.to_string())
        })?;

        if n == 0 {
            return Ok(false);
        }

        local_file.write_all(&buf[..n]).await.map_err(|e| {
            classify_transfer_error(
                &format!("write local file '{}'", local_path.display()),
                &e.to_string(),
            )
        })?;

        bytes_transferred.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::SeqCst);
    }
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

        #[test]
        fn test_tilde_path_expanded() {
            let path = resolve_local_path("~/.ssh/id_rsa");
            assert!(!path.to_string_lossy().starts_with('~'));
            assert!(path.to_string_lossy().ends_with(".ssh/id_rsa"));
            assert!(path.is_absolute());
        }

        #[test]
        fn test_tilde_alone_expanded() {
            let path = resolve_local_path("~");
            assert!(!path.to_string_lossy().starts_with('~'));
            assert!(path.is_absolute());
        }
    }

    mod expand_tilde_fn {
        use super::*;

        #[test]
        fn test_tilde_slash_prefix() {
            let result = expand_tilde("~/.ssh/id_rsa");
            assert!(!result.starts_with('~'));
            assert!(result.ends_with(".ssh/id_rsa"));
        }

        #[test]
        fn test_tilde_alone() {
            let result = expand_tilde("~");
            assert!(!result.starts_with('~'));
        }

        #[test]
        fn test_absolute_path_unchanged() {
            let result = expand_tilde("/tmp/file.txt");
            assert_eq!(result, "/tmp/file.txt");
        }

        #[test]
        fn test_relative_path_unchanged() {
            let result = expand_tilde("relative/path");
            assert_eq!(result, "relative/path");
        }

        #[test]
        fn test_tilde_in_middle_unchanged() {
            let result = expand_tilde("/path/~/file");
            assert_eq!(result, "/path/~/file");
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

    mod classify_transfer_error_fn {
        use super::*;

        #[test]
        fn test_file_not_found() {
            let result = classify_transfer_error(
                "open remote file '/tmp/f.txt'",
                "No such file or directory",
            );
            assert!(result.starts_with("[FILE_NOT_FOUND]"));
            assert!(result.contains("open remote file '/tmp/f.txt'"));
            assert!(result.contains("(raw: No such file or directory)"));
        }

        #[test]
        fn test_file_not_found_via_not_found() {
            let result = classify_transfer_error("access", "File not found");
            assert!(result.starts_with("[FILE_NOT_FOUND]"));
        }

        #[test]
        fn test_permission_denied() {
            let result =
                classify_transfer_error("open local file '/root/secret'", "Permission denied");
            assert!(result.starts_with("[PERMISSION_DENIED]"));
            assert!(result.contains("insufficient permissions"));
        }

        #[test]
        fn test_permission_denied_case_insensitive() {
            let result = classify_transfer_error("write", "PERMISSION DENIED");
            assert!(result.starts_with("[PERMISSION_DENIED]"));
        }

        #[test]
        fn test_disk_full() {
            let result = classify_transfer_error(
                "write to remote file '/tmp/big'",
                "No space left on device",
            );
            assert!(result.starts_with("[DISK_FULL]"));
            assert!(result.contains("no space left on device"));
        }

        #[test]
        fn test_connection_lost_broken_pipe() {
            let result = classify_transfer_error("write", "Broken pipe");
            assert!(result.starts_with("[CONNECTION_LOST]"));
        }

        #[test]
        fn test_connection_lost_reset() {
            let result = classify_transfer_error("read", "Connection reset by peer");
            assert!(result.starts_with("[CONNECTION_LOST]"));
        }

        #[test]
        fn test_connection_lost_refused() {
            let result = classify_transfer_error("open", "Connection refused");
            assert!(result.starts_with("[CONNECTION_LOST]"));
        }

        #[test]
        fn test_connection_lost_unreachable() {
            let result = classify_transfer_error("open", "Network is unreachable");
            assert!(result.starts_with("[CONNECTION_LOST]"));
        }

        #[test]
        fn test_connection_lost_no_route() {
            let result = classify_transfer_error("open", "No route to host");
            assert!(result.starts_with("[CONNECTION_LOST]"));
        }

        #[test]
        fn test_remote_dir_not_found_on_create() {
            let result = classify_transfer_error(
                "create remote file '/tmp/nonexistent/dir/file.txt'",
                "No such file or directory",
            );
            assert!(result.starts_with("[REMOTE_DIR_NOT_FOUND]"));
            assert!(result.contains("parent directory does not exist"));
        }

        #[test]
        fn test_remote_dir_not_found_not_a_directory() {
            let result =
                classify_transfer_error("create remote file '/tmp/file/nested'", "Not a directory");
            assert!(result.starts_with("[REMOTE_DIR_NOT_FOUND]"));
        }

        #[test]
        fn test_read_only_fs() {
            let result = classify_transfer_error("write", "Read-only file system");
            assert!(result.starts_with("[READ_ONLY_FS]"));
            assert!(result.contains("target filesystem is read-only"));
        }

        #[test]
        fn test_read_only_fs_without_hyphen() {
            let result = classify_transfer_error("write", "Read only file system");
            assert!(result.starts_with("[READ_ONLY_FS]"));
        }

        #[test]
        fn test_sftp_protocol_channel() {
            let result = classify_transfer_error("open SFTP channel", "Channel open failure");
            assert!(result.starts_with("[SFTP_PROTOCOL]"));
        }

        #[test]
        fn test_sftp_protocol_subsystem() {
            let result =
                classify_transfer_error("request SFTP subsystem", "Subsystem request failed");
            assert!(result.starts_with("[SFTP_PROTOCOL]"));
        }

        #[test]
        fn test_sftp_protocol_session() {
            let result = classify_transfer_error("initialize SFTP session", "Session error");
            assert!(result.starts_with("[SFTP_PROTOCOL]"));
        }

        #[test]
        fn test_timeout() {
            let result = classify_transfer_error("read", "Operation timed out");
            assert!(result.starts_with("[TIMEOUT]"));
        }

        #[test]
        fn test_timeout_keyword() {
            let result = classify_transfer_error("write", "Request timeout");
            assert!(result.starts_with("[TIMEOUT]"));
        }

        #[test]
        fn test_io_error_fallback() {
            let result = classify_transfer_error("write", "Unknown internal error");
            assert!(result.starts_with("[IO_ERROR]"));
            assert!(result.contains("I/O error"));
            assert!(result.contains("(raw: Unknown internal error)"));
        }

        #[test]
        fn test_output_format() {
            let result = classify_transfer_error("write to remote file '/tmp/x'", "Broken pipe");
            assert!(result.starts_with("[CONNECTION_LOST] write to remote file '/tmp/x': "));
            assert!(result.ends_with("(raw: Broken pipe)"));
        }

        #[test]
        fn test_read_only_takes_precedence_over_permission() {
            // "Read-only file system" should not match "permission denied"
            let result = classify_transfer_error("write", "Read-only file system");
            assert!(result.starts_with("[READ_ONLY_FS]"));
        }

        #[test]
        fn test_disk_full_takes_precedence_over_io() {
            let result = classify_transfer_error("write", "No space left on device");
            assert!(result.starts_with("[DISK_FULL]"));
        }
    }
}
