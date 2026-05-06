//! SFTP helpers for file transfer operations.
//!
//! This module provides functions for opening SFTP sessions and streaming
//! file transfers with progress tracking.
//!
//! # Architecture
//!
//! - `open_sftp_session`: Opens an SFTP subsystem on an SSH channel
//! - `resolve_local_path`: Cross-platform path resolution (relative -> home dir)
//! - `preflight_resume_upload` / `preflight_resume_download`: ADR 0010
//!   length-prefix resume planner — pre-flights the destination size and
//!   returns a [`ResumePlan`] that the streaming layer honours.
//! - `sftp_upload_streaming`: Streams a local file to remote via SFTP
//! - `sftp_download_streaming`: Streams a remote file to local via SFTP

use std::cmp::Ordering as CmpOrdering;
use std::env;
use std::io::{ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use russh::client;
use russh::{ChannelMsg, client::Msg as ClientMsg};
use russh_sftp::client::SftpSession;
use russh_sftp::client::fs::File as SftpFile;
use russh_sftp::protocol::OpenFlags;
use sha2::{Digest, Sha256};
use tokio::fs::{File, OpenOptions, metadata as tokio_metadata};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Notify, OnceCell, broadcast, watch};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::transfer::{CHUNK_SIZE, TransferStatus};
use super::types::ProgressEvent;
use crate::adapters::ssh::internal::session::SshClientHandler;
use crate::adapters::subscription::legacy::{ResourceKind, SUBSCRIPTION_REGISTRY};

/// Bag of lock-free shared state plumbed into the streaming SFTP loops.
///
/// Replaces the previous `Mutex<Option<String>>` for `error` with a
/// write-once `OnceCell` and adds the broadcast/notify primitives that
/// power the future `transfer://<id>/progress` MCP resource.
pub struct TransferShared {
    /// Stable transfer identifier used by the subscription registry to
    /// allocate sequence numbers and wake the debouncer.
    pub transfer_id: String,
    /// Cumulative byte counter incremented after each successful chunk.
    pub bytes_transferred: Arc<AtomicU64>,
    /// Total bytes the transfer is attempting to move (may be 0 for streams
    /// without a known size — e.g. some download metadata cases).
    pub total_bytes: Arc<AtomicU64>,
    /// Live broadcast of `ProgressEvent`s. Send failures are ignored —
    /// no subscribers is the steady state until E13 wires the resource.
    pub progress_tx: broadcast::Sender<ProgressEvent>,
    /// Wake source for intra-server long-poll progress readers.
    pub data_notify: Arc<Notify>,
    /// Token to cancel the transfer.
    pub cancel_token: CancellationToken,
    /// Watch sender for terminal status transitions.
    pub status_tx: watch::Sender<TransferStatus>,
    /// Write-once failure reason. Set only when the transfer ends in
    /// `Failed`.
    pub error: Arc<OnceCell<String>>,
    /// ADR 0010 — resume plan computed by the preflight phase. Defaults
    /// to [`ResumePlan::Truncate`] (v6.0 semantics) when the caller did
    /// not request resume.
    pub resume_plan: ResumePlan,
}

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

/// Outcome of the ADR 0010 length-prefix resume preflight.
///
/// Computed once per transfer, before the streaming task spawns; carried
/// through [`TransferShared`] into the chunk loop so the loop can decide
/// whether to truncate, skip, or seek the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumePlan {
    /// Fresh transfer — open destination with `TRUNCATE` semantics and
    /// stream from byte 0. Identical to v6.0 behaviour. This is also
    /// what `resume = false` always returns.
    Truncate,
    /// Destination already matches the source length — short-circuit
    /// the chunk loop and emit `Completed` synchronously. The transfer
    /// reports `bytes_transferred = total_bytes` (everything was already
    /// in place) and `resumed_from = total_bytes`.
    Skip { total_bytes: u64 },
    /// Destination is shorter than the source — open without `TRUNCATE`,
    /// seek both endpoints to `offset`, and ramp progress from `offset`
    /// to `total_bytes`.
    Resume { offset: u64, total_bytes: u64 },
}

impl ResumePlan {
    /// Byte offset the destination is positioned at when the chunk loop
    /// starts. `0` for [`ResumePlan::Truncate`], `offset` for
    /// [`ResumePlan::Resume`], `total_bytes` for [`ResumePlan::Skip`].
    #[must_use]
    pub const fn start_offset(&self) -> u64 {
        match self {
            Self::Truncate => 0,
            Self::Skip { total_bytes } => *total_bytes,
            Self::Resume { offset, .. } => *offset,
        }
    }

    /// Convenience predicate — `true` only for [`ResumePlan::Skip`].
    #[must_use]
    pub const fn is_skip(&self) -> bool {
        matches!(self, Self::Skip { .. })
    }
}

/// Hash buffer block size for the `verify=true` prefix compare. 64 KiB
/// matches the OS-level read-ahead window typical on modern Linux /
/// macOS and keeps the loop allocation small.
const VERIFY_HASH_BLOCK: usize = 64 * 1024;

/// Compute the SHA-256 of the local file `[0..offset]` prefix,
/// streaming in 64 KiB blocks.
///
/// Used by the ADR 0010 `verify=true` path. Never materialises the
/// prefix in memory; the caller pays O(offset) bytes hashed locally.
async fn sha256_local_prefix(local_path: &Path, offset: u64) -> Result<[u8; 32], String> {
    let mut file = File::open(local_path).await.map_err(|e| {
        classify_transfer_error(
            &format!("open local for verify '{}'", local_path.display()),
            &e.to_string(),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; VERIFY_HASH_BLOCK];
    hash_prefix_loop(&mut file, &mut hasher, &mut buf, local_path, offset).await?;
    Ok(hasher.finalize().into())
}

/// Drive the read-and-hash loop for [`sha256_local_prefix`]. Extracted
/// so the parent stays under the project's 30-line clippy threshold.
async fn hash_prefix_loop(
    file: &mut File,
    hasher: &mut Sha256,
    buf: &mut [u8],
    local_path: &Path,
    offset: u64,
) -> Result<(), String> {
    let block_u64 = u64::try_from(buf.len()).unwrap_or(u64::MAX);
    let mut remaining = offset;
    while remaining > 0 {
        let want_u64 = remaining.min(block_u64);
        // bounded by buf.len() (a usize), so the cast cannot truncate.
        let want = usize::try_from(want_u64).unwrap_or(buf.len());
        let n = file.read(&mut buf[..want]).await.map_err(|e| {
            classify_transfer_error(
                &format!("read local for verify '{}'", local_path.display()),
                &e.to_string(),
            )
        })?;
        if n == 0 {
            return Err(format!(
                "[RESUME_MISMATCH] local file '{}' shorter than verify offset {offset}; \
                 destination prefix cannot be validated. Re-run with resume=false to overwrite.",
                local_path.display()
            ));
        }
        hasher.update(&buf[..n]);
        remaining = remaining.saturating_sub(u64::try_from(n).unwrap_or(0));
    }
    Ok(())
}

/// Execute a one-shot remote command on the russh handle and collect
/// stdout/stderr/exit code. Used by the ADR 0010 verify path to drive
/// `sha256sum` / `dd` on the remote host without depending on the SSH
/// adapter's port-level surface.
async fn exec_sync(
    handle: &Arc<client::Handle<SshClientHandler>>,
    command: &str,
) -> Result<(String, String, Option<u32>), String> {
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| classify_transfer_error("open verify channel", &e.to_string()))?;
    channel
        .exec(true, command)
        .await
        .map_err(|e| classify_transfer_error("exec verify command", &e.to_string()))?;
    let mut stdout = Vec::with_capacity(64);
    let mut stderr = Vec::with_capacity(64);
    let mut exit_code: Option<u32> = None;
    collect_exec_output(&mut channel, &mut stdout, &mut stderr, &mut exit_code).await;
    let stdout_s = String::from_utf8_lossy(&stdout).into_owned();
    let stderr_s = String::from_utf8_lossy(&stderr).into_owned();
    Ok((stdout_s, stderr_s, exit_code))
}

/// Drain a russh channel into stdout / stderr buffers and capture the
/// exit code. Mirrors the SSH adapter's `collect_sync_output` but lives
/// here so the SFTP adapter does not pull a cross-module dependency.
async fn collect_exec_output(
    channel: &mut russh::Channel<ClientMsg>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    exit_code: &mut Option<u32>,
) {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, ext }) => {
                if ext == 1 {
                    stderr.extend_from_slice(&data);
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                *exit_code = Some(exit_status);
            }
            Some(ChannelMsg::Eof) => {
                if exit_code.is_some() {
                    break;
                }
            }
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
}

/// Hash 64 hex chars from the head of `stdout` (sha256sum-style output)
/// and return the digest bytes. Returns `None` when the input is too
/// short or contains non-hex characters.
fn parse_sha256_hex(out: &str) -> Option<[u8; 32]> {
    let head: String = out.trim_start().chars().take(64).collect();
    if head.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        let start = i * 2;
        let pair = head.get(start..=start + 1)?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(digest)
}

/// Build the remote-side hash command for an upload-direction verify.
///
/// We hash the entire remote prefix because the local source already
/// covers everything past the offset; if the remote is shorter than the
/// resume offset the preflight would have routed to `Truncate`, so this
/// path always sees `remote_size >= offset` and `sha256sum` over the
/// full file is correct.
fn upload_verify_command(remote_path: &str) -> String {
    let escaped = shell_single_quote(remote_path);
    format!("sha256sum -b -- {escaped} 2>/dev/null")
}

/// Build the remote-side hash command for a download-direction verify.
///
/// We hash only the first `offset` bytes of the remote source because
/// the remote file is normally larger than the local prefix; piping
/// `dd` into `sha256sum` keeps the hash bounded.
fn download_verify_command(remote_path: &str, offset: u64) -> String {
    let escaped = shell_single_quote(remote_path);
    format!("dd if={escaped} bs=1 count={offset} 2>/dev/null | sha256sum")
}

/// Quote a path for safe inclusion in a remote shell command using
/// POSIX single-quote rules. Replaces every embedded `'` with `'\''`.
fn shell_single_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Verify that the resume prefix on the remote matches the local prefix
/// by hashing both sides and comparing.
///
/// Used by [`preflight_resume_upload`] / [`preflight_resume_download`]
/// when the caller passes `verify=true`. Pure precondition check —
/// returns `Ok(())` on match, `Err("[RESUME_MISMATCH] ...")` on
/// divergence. Skips entirely when `offset == 0` (no prefix to verify).
async fn verify_resume_prefix(
    handle: &Arc<client::Handle<SshClientHandler>>,
    local_path: &Path,
    remote_path: &str,
    offset: u64,
    direction: VerifyDirection,
) -> Result<(), String> {
    if offset == 0 {
        return Ok(());
    }
    let local_digest = sha256_local_prefix(local_path, offset).await?;
    let command = match direction {
        VerifyDirection::Upload => upload_verify_command(remote_path),
        VerifyDirection::Download => download_verify_command(remote_path, offset),
    };
    let (stdout, stderr, exit_code) = exec_sync(handle, &command).await?;
    let remote_digest = remote_digest_or_err(exit_code, &stdout, &stderr)?;
    compare_resume_digests(offset, &local_digest, &remote_digest)
}

/// Translate an `exec` outcome into the remote sha256 digest, or a
/// `[RESUME_MISMATCH]`-tagged error covering both the non-zero exit and
/// the parse-failure cases.
fn remote_digest_or_err(
    exit_code: Option<u32>,
    stdout: &str,
    stderr: &str,
) -> Result<[u8; 32], String> {
    if exit_code != Some(0) {
        return Err(format!(
            "[RESUME_MISMATCH] remote verify command failed (exit={exit_code:?}); \
             stderr='{}'. Required tools: sha256sum + dd. Re-run with resume=false to overwrite.",
            stderr.trim()
        ));
    }
    parse_sha256_hex(stdout).ok_or_else(|| {
        format!(
            "[RESUME_MISMATCH] could not parse remote sha256 output '{}' (need 64 hex chars). \
             Required tools: sha256sum + dd.",
            stdout.trim()
        )
    })
}

/// Final digest comparison — `Ok(())` on match, tagged error on
/// divergence. Pure helper.
fn compare_resume_digests(
    offset: u64,
    local_digest: &[u8; 32],
    remote_digest: &[u8; 32],
) -> Result<(), String> {
    if local_digest == remote_digest {
        Ok(())
    } else {
        Err(format!(
            "[RESUME_MISMATCH] resume prefix sha256 differs (offset={offset}); \
             local={} remote={}. Re-run with resume=false to overwrite, \
             or fix the partial file.",
            hex_encode(local_digest),
            hex_encode(remote_digest)
        ))
    }
}

/// Direction tag for [`verify_resume_prefix`] so the helper picks the
/// right remote-side hash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyDirection {
    Upload,
    Download,
}

/// Lower-case hex encoding of a 32-byte sha256 digest. Used only for
/// the human-readable error string; not on a hot path.
fn hex_encode(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Pre-flight an upload destination and return the [`ResumePlan`] the
/// streaming task must honour (ADR 0010).
///
/// Behaviour:
/// - `resume == false` -> always returns [`ResumePlan::Truncate`]; v6.0
///   semantics preserved.
/// - `resume == true` and remote file missing -> [`ResumePlan::Truncate`]
///   (resume from a non-existent prefix is identical to a fresh upload).
/// - `resume == true` and remote size > local size -> error tagged
///   `[RESUME_OVERSHOOT]`.
/// - `resume == true` and remote size == local size ->
///   [`ResumePlan::Skip`].
/// - `resume == true` and remote size < local size ->
///   [`ResumePlan::Resume { offset = remote_size }`].
///
/// # Errors
///
/// Returns a tagged string error on overshoot or on unrecoverable
/// metadata failures. Caller maps this through [`DomainError::Sftp`] —
/// the existing classification dispatch handles it.
pub async fn preflight_resume_upload(
    handle: &Arc<client::Handle<SshClientHandler>>,
    local_path: &Path,
    remote_path: &str,
    resume: bool,
    verify: bool,
) -> Result<ResumePlan, String> {
    let local_size = stat_local_for_resume(local_path).await?;
    if !resume {
        return Ok(ResumePlan::Truncate);
    }
    let sftp = open_sftp_session(handle).await?;
    let remote_size = stat_remote_for_resume(&sftp, remote_path).await?;
    drop(sftp);
    let plan = decide_upload_plan(local_size, remote_size)?;
    maybe_verify_prefix(
        handle,
        local_path,
        remote_path,
        plan,
        verify,
        VerifyDirection::Upload,
    )
    .await?;
    Ok(plan)
}

/// Pre-flight a download destination and return the [`ResumePlan`] the
/// streaming task must honour (ADR 0010).
///
/// Behaviour mirrors [`preflight_resume_upload`] with the local and
/// remote roles swapped — the local destination plays the part of "the
/// thing that may already hold a partial prefix".
///
/// # Errors
///
/// Returns a tagged string error on overshoot or on unrecoverable
/// metadata failures.
pub async fn preflight_resume_download(
    handle: &Arc<client::Handle<SshClientHandler>>,
    remote_path: &str,
    local_path: &Path,
    resume: bool,
    verify: bool,
) -> Result<ResumePlan, String> {
    let sftp = open_sftp_session(handle).await?;
    let remote_size = open_remote_size_required(&sftp, remote_path).await?;
    drop(sftp);
    if !resume {
        return Ok(ResumePlan::Truncate);
    }
    let local_size = stat_local_for_resume_optional(local_path).await?;
    let plan = decide_download_plan(local_size, remote_size)?;
    maybe_verify_prefix(
        handle,
        local_path,
        remote_path,
        plan,
        verify,
        VerifyDirection::Download,
    )
    .await?;
    Ok(plan)
}

/// Run the ADR 0010 verify-prefix hash compare when:
/// - the caller passed `verify=true`, AND
/// - the resume plan is [`ResumePlan::Resume`] with a non-zero offset
///   (Truncate has nothing to verify; Skip already implies prefix match
///   by length, but we still re-hash to surface mid-prefix corruption).
///
/// Returns `Ok(())` on match or when the verify pass should be skipped.
async fn maybe_verify_prefix(
    handle: &Arc<client::Handle<SshClientHandler>>,
    local_path: &Path,
    remote_path: &str,
    plan: ResumePlan,
    verify: bool,
    direction: VerifyDirection,
) -> Result<(), String> {
    if !verify {
        return Ok(());
    }
    let offset = match plan {
        ResumePlan::Resume { offset, .. } => offset,
        ResumePlan::Skip { total_bytes } => total_bytes,
        ResumePlan::Truncate => return Ok(()),
    };
    if offset == 0 {
        return Ok(());
    }
    verify_resume_prefix(handle, local_path, remote_path, offset, direction).await
}

/// Stat a local path and return its byte length. Surfaces metadata
/// failures with the existing `[FILE_NOT_FOUND]` / `[PERMISSION_DENIED]`
/// classification.
async fn stat_local_for_resume(local_path: &Path) -> Result<u64, String> {
    tokio_metadata(local_path)
        .await
        .map(|m| m.len())
        .map_err(|err| {
            classify_transfer_error(
                &format!("stat local for resume '{}'", local_path.display()),
                &err.to_string(),
            )
        })
}

/// Stat a local path and return its byte length, or `0` when the file is
/// absent. Used by the download preflight where a missing local file is
/// the legitimate "fresh download" base case.
async fn stat_local_for_resume_optional(local_path: &Path) -> Result<u64, String> {
    match tokio_metadata(local_path).await {
        Ok(meta) => Ok(meta.len()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(0),
        Err(err) => Err(classify_transfer_error(
            &format!("stat local for resume '{}'", local_path.display()),
            &err.to_string(),
        )),
    }
}

/// Stat a remote path through an SFTP session and return its byte
/// length. Returns `0` when the file does not exist (resume base case).
async fn stat_remote_for_resume(sftp: &SftpSession, remote_path: &str) -> Result<u64, String> {
    match sftp.metadata(remote_path).await {
        Ok(meta) => Ok(meta.size.unwrap_or(0)),
        Err(err) => {
            let raw = err.to_string();
            let lower = raw.to_lowercase();
            if lower.contains("no such file") || lower.contains("not found") {
                Ok(0)
            } else {
                Err(classify_transfer_error(
                    &format!("preflight remote metadata '{remote_path}'"),
                    &raw,
                ))
            }
        }
    }
}

/// Stat a remote path through an SFTP session and **require** the file
/// to exist. Used by the download preflight where a missing remote file
/// is fatal (no source to copy from).
async fn open_remote_size_required(sftp: &SftpSession, remote_path: &str) -> Result<u64, String> {
    sftp.metadata(remote_path)
        .await
        .map(|meta| meta.size.unwrap_or(0))
        .map_err(|err| {
            classify_transfer_error(
                &format!("preflight remote metadata '{remote_path}'"),
                &err.to_string(),
            )
        })
}

/// Decision matrix for upload preflight given the two sizes.
///
/// Pure function (no I/O) so the 9-case decision matrix is unit-testable
/// without a live SFTP server. Re-exported through
/// [`super::resume::decide_upload_plan`] for external property tests.
///
/// # Errors
///
/// Returns a `[RESUME_OVERSHOOT]`-tagged error when `remote_size >
/// local_size`. The caller surfaces this through `DomainError::Sftp`.
pub(super) fn decide_upload_plan(local_size: u64, remote_size: u64) -> Result<ResumePlan, String> {
    match remote_size.cmp(&local_size) {
        CmpOrdering::Greater => Err(format!(
            "[RESUME_OVERSHOOT] preflight resume upload: remote size {remote_size} \
             exceeds local size {local_size}; refusing to resume. \
             Re-run with resume=false to overwrite, or fix the partial file."
        )),
        CmpOrdering::Equal => Ok(ResumePlan::Skip {
            total_bytes: local_size,
        }),
        CmpOrdering::Less => Ok(ResumePlan::Resume {
            offset: remote_size,
            total_bytes: local_size,
        }),
    }
}

/// Decision matrix for download preflight; mirror of
/// [`decide_upload_plan`] with local and remote roles swapped.
/// Re-exported through [`super::resume::decide_download_plan`].
///
/// # Errors
///
/// Returns a `[RESUME_OVERSHOOT]`-tagged error when `local_size >
/// remote_size`.
pub(super) fn decide_download_plan(
    local_size: u64,
    remote_size: u64,
) -> Result<ResumePlan, String> {
    match local_size.cmp(&remote_size) {
        CmpOrdering::Greater => Err(format!(
            "[RESUME_OVERSHOOT] preflight resume download: local size {local_size} \
             exceeds remote size {remote_size}; refusing to resume. \
             Re-run with resume=false to overwrite, or fix the partial file."
        )),
        CmpOrdering::Equal => Ok(ResumePlan::Skip {
            total_bytes: remote_size,
        }),
        CmpOrdering::Less => Ok(ResumePlan::Resume {
            offset: local_size,
            total_bytes: remote_size,
        }),
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
/// emitting a `ProgressEvent::Tick` after each chunk and a terminal
/// `Completed` / `Failed` / `Cancelled` event before returning.
///
/// Honours the [`ResumePlan`] carried in `shared`:
/// - [`ResumePlan::Truncate`] — v6.0 behaviour: create + truncate the
///   remote file, stream from byte 0.
/// - [`ResumePlan::Skip`] — short-circuit: emit `Completed` synchronously
///   without opening either file.
/// - [`ResumePlan::Resume`] — open the remote file with `WRITE | CREATE`
///   (no `TRUNCATE`), seek both endpoints to the resume offset, ramp
///   progress from `offset` to `total_bytes`.
pub async fn sftp_upload_streaming(
    handle: Arc<client::Handle<SshClientHandler>>,
    local_path: PathBuf,
    remote_path: String,
    shared: TransferShared,
) {
    if shared.resume_plan.is_skip() {
        handle_transfer_result(Ok(false), "upload", &local_path, &remote_path, &shared);
        return;
    }
    let result = sftp_upload_inner(
        &handle,
        &local_path,
        &remote_path,
        &shared.transfer_id,
        &shared.bytes_transferred,
        &shared.cancel_token,
        &shared.progress_tx,
        &shared.data_notify,
        &shared.total_bytes,
        shared.resume_plan,
    )
    .await;

    handle_transfer_result(result, "upload", &local_path, &remote_path, &shared);
}

/// Handle the result of a transfer operation: log, update status, set
/// error (write-once), and broadcast the terminal `ProgressEvent`.
fn handle_transfer_result(
    result: Result<bool, String>,
    direction: &str,
    local_path: &Path,
    remote_path: &str,
    shared: &TransferShared,
) {
    match result {
        Ok(true) => finalize_cancelled(direction, local_path, remote_path, shared),
        Ok(false) => finalize_completed(direction, local_path, remote_path, shared),
        Err(e) => finalize_failed(direction, local_path, remote_path, shared, e),
    }
}

/// Terminal-state handler for `Cancelled` transfers.
fn finalize_cancelled(
    direction: &str,
    local_path: &Path,
    remote_path: &str,
    shared: &TransferShared,
) {
    info!(
        "SFTP {direction} cancelled: {remote_path} <-> {}",
        local_path.display()
    );
    let _ = shared.status_tx.send(TransferStatus::Cancelled);
    let seq = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Transfer, &shared.transfer_id);
    let _ = shared.progress_tx.send(ProgressEvent::Cancelled { seq });
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Transfer, &shared.transfer_id);
    shared.data_notify.notify_waiters();
}

/// Terminal-state handler for successfully `Completed` transfers.
fn finalize_completed(
    direction: &str,
    local_path: &Path,
    remote_path: &str,
    shared: &TransferShared,
) {
    let bytes = shared.bytes_transferred.load(Ordering::SeqCst);
    info!(
        "SFTP {direction} completed: {remote_path} <-> {} ({bytes} bytes)",
        local_path.display()
    );
    let _ = shared.status_tx.send(TransferStatus::Completed);
    let seq = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Transfer, &shared.transfer_id);
    let _ = shared.progress_tx.send(ProgressEvent::Completed {
        seq,
        bytes_transferred: bytes,
    });
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Transfer, &shared.transfer_id);
    shared.data_notify.notify_waiters();
}

/// Terminal-state handler for `Failed` transfers; sets the write-once error.
fn finalize_failed(
    direction: &str,
    local_path: &Path,
    remote_path: &str,
    shared: &TransferShared,
    err: String,
) {
    error!(
        "SFTP {direction} failed: {remote_path} <-> {}: {err}",
        local_path.display()
    );
    // Write-once: a second `set` returns `Err`, which we deliberately
    // discard — there is no second writer in this code path.
    let _ = shared.error.set(err);
    let _ = shared.status_tx.send(TransferStatus::Failed);
    let seq = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Transfer, &shared.transfer_id);
    let _ = shared.progress_tx.send(ProgressEvent::Failed { seq });
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Transfer, &shared.transfer_id);
    shared.data_notify.notify_waiters();
}

/// Inner upload logic, returns Ok(true) if cancelled, Ok(false) if completed.
#[allow(
    clippy::too_many_arguments,
    reason = "lock-free streaming requires plumbing every shared primitive into the chunk loop"
)]
async fn sftp_upload_inner(
    handle: &Arc<client::Handle<SshClientHandler>>,
    local_path: &Path,
    remote_path: &str,
    transfer_id: &str,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
    progress_tx: &broadcast::Sender<ProgressEvent>,
    data_notify: &Notify,
    total_bytes: &Arc<AtomicU64>,
    resume_plan: ResumePlan,
) -> Result<bool, String> {
    let sftp = open_sftp_session(handle).await?;
    let mut local_file = open_local_file(local_path).await?;
    let mut remote_file = open_remote_file_for_write(&sftp, remote_path, resume_plan).await?;

    if let ResumePlan::Resume { offset, .. } = resume_plan {
        seek_local_file(&mut local_file, offset, local_path).await?;
        seek_remote_file(&mut remote_file, offset, remote_path).await?;
    }

    let cancelled = upload_chunks(
        &mut local_file,
        &mut remote_file,
        local_path,
        remote_path,
        transfer_id,
        bytes_transferred,
        cancel_token,
        progress_tx,
        data_notify,
        total_bytes,
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

/// Open a remote file for writing, honouring the ADR 0010 [`ResumePlan`].
///
/// - [`ResumePlan::Truncate`] reuses [`create_remote_file`] (v6.0 path:
///   `WRITE | CREATE | TRUNCATE`).
/// - [`ResumePlan::Resume`] opens with `WRITE | CREATE` (no truncate)
///   so the existing prefix is preserved. The streaming caller seeks
///   to the resume offset before writing the next chunk.
/// - [`ResumePlan::Skip`] never reaches this helper — the caller
///   short-circuits at the streaming wrapper.
async fn open_remote_file_for_write(
    sftp: &SftpSession,
    remote_path: &str,
    resume_plan: ResumePlan,
) -> Result<SftpFile, String> {
    match resume_plan {
        ResumePlan::Truncate => create_remote_file(sftp, remote_path).await,
        ResumePlan::Resume { .. } => {
            let flags = OpenFlags::WRITE | OpenFlags::CREATE;
            sftp.open_with_flags(remote_path, flags).await.map_err(|e| {
                classify_transfer_error(
                    &format!("open remote file for resume '{remote_path}'"),
                    &e.to_string(),
                )
            })
        }
        ResumePlan::Skip { .. } => Err(format!(
            "[INTERNAL_ERROR] open_remote_file_for_write reached on Skip plan: {remote_path}"
        )),
    }
}

/// Seek a remote SFTP file to `offset`. Surfaces seek failures with the
/// existing transfer-error classifier.
async fn seek_remote_file(
    file: &mut SftpFile,
    offset: u64,
    remote_path: &str,
) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset)).await.map_err(|e| {
        classify_transfer_error(
            &format!("seek remote file '{remote_path}' to offset {offset}"),
            &e.to_string(),
        )
    })?;
    Ok(())
}

/// Seek a local file (`tokio::fs::File`) to `offset`. Mirror of
/// [`seek_remote_file`].
async fn seek_local_file(file: &mut File, offset: u64, local_path: &Path) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset)).await.map_err(|e| {
        classify_transfer_error(
            &format!(
                "seek local file '{}' to offset {offset}",
                local_path.display()
            ),
            &e.to_string(),
        )
    })?;
    Ok(())
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
#[allow(
    clippy::too_many_arguments,
    reason = "lock-free streaming requires plumbing every shared primitive into the chunk loop"
)]
async fn upload_chunks(
    local_file: &mut File,
    remote_file: &mut SftpFile,
    local_path: &Path,
    remote_path: &str,
    transfer_id: &str,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
    progress_tx: &broadcast::Sender<ProgressEvent>,
    data_notify: &Notify,
    total_bytes: &Arc<AtomicU64>,
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
        // ADR 0006 Amendment 1 — feed the byte-threshold counter
        // with the per-chunk delta so a fast transfer can flush the
        // push channel as soon as it produces `SSH_NOTIFY_FLUSH_BYTES`
        // since the last broadcast.
        SUBSCRIPTION_REGISTRY.record_bytes(ResourceKind::Transfer, transfer_id, n);
        emit_tick(
            transfer_id,
            progress_tx,
            data_notify,
            bytes_transferred,
            total_bytes,
        );
    }
}

/// Send a `ProgressEvent::Tick` and wake intra-server long-poll readers.
///
/// Send failures are intentionally swallowed: there may be no subscriber
/// yet (steady state until E13 wires `transfer://<id>/progress`).
fn emit_tick(
    transfer_id: &str,
    progress_tx: &broadcast::Sender<ProgressEvent>,
    data_notify: &Notify,
    bytes_transferred: &AtomicU64,
    total_bytes: &AtomicU64,
) {
    let seq = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Transfer, transfer_id);
    let _ = progress_tx.send(ProgressEvent::Tick {
        seq,
        bytes_transferred: bytes_transferred.load(Ordering::Relaxed),
        total_bytes: total_bytes.load(Ordering::Relaxed),
    });
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Transfer, transfer_id);
    data_notify.notify_waiters();
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
/// emitting a `ProgressEvent::Tick` after each chunk and a terminal
/// `Completed` / `Failed` / `Cancelled` event before returning.
///
/// Honours the [`ResumePlan`] carried in `shared`: see the upload twin
/// for the per-variant semantics. The local destination is opened with
/// `OpenOptions::write+create+truncate(false)` for resume; v6.0
/// `File::create` truncating semantics are preserved for the
/// [`ResumePlan::Truncate`] path.
pub async fn sftp_download_streaming(
    handle: Arc<client::Handle<SshClientHandler>>,
    remote_path: String,
    local_path: PathBuf,
    shared: TransferShared,
) {
    if shared.resume_plan.is_skip() {
        handle_transfer_result(Ok(false), "download", &local_path, &remote_path, &shared);
        return;
    }
    let result = sftp_download_inner(
        &handle,
        &remote_path,
        &local_path,
        &shared.transfer_id,
        &shared.bytes_transferred,
        &shared.cancel_token,
        &shared.progress_tx,
        &shared.data_notify,
        &shared.total_bytes,
        shared.resume_plan,
    )
    .await;

    handle_transfer_result(result, "download", &local_path, &remote_path, &shared);
}

/// Inner download logic, returns Ok(true) if cancelled, Ok(false) if completed.
#[allow(
    clippy::too_many_arguments,
    reason = "lock-free streaming requires plumbing every shared primitive into the chunk loop"
)]
async fn sftp_download_inner(
    handle: &Arc<client::Handle<SshClientHandler>>,
    remote_path: &str,
    local_path: &Path,
    transfer_id: &str,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
    progress_tx: &broadcast::Sender<ProgressEvent>,
    data_notify: &Notify,
    total_bytes: &Arc<AtomicU64>,
    resume_plan: ResumePlan,
) -> Result<bool, String> {
    let sftp = open_sftp_session(handle).await?;
    let mut remote_file = open_remote_file(&sftp, remote_path).await?;
    let mut local_file = open_local_file_for_write(local_path, resume_plan).await?;

    if let ResumePlan::Resume { offset, .. } = resume_plan {
        seek_remote_file(&mut remote_file, offset, remote_path).await?;
        seek_local_file(&mut local_file, offset, local_path).await?;
    }

    let cancelled = download_chunks(
        &mut remote_file,
        &mut local_file,
        remote_path,
        local_path,
        transfer_id,
        bytes_transferred,
        cancel_token,
        progress_tx,
        data_notify,
        total_bytes,
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

/// Open a local file for writing, honouring the ADR 0010 [`ResumePlan`].
///
/// - [`ResumePlan::Truncate`] reuses [`create_local_file`] (v6.0 path:
///   truncate-on-open).
/// - [`ResumePlan::Resume`] opens with `OpenOptions::write+create+
///   truncate(false)` so the existing prefix is preserved. The streaming
///   caller seeks to the resume offset before writing the next chunk.
/// - [`ResumePlan::Skip`] never reaches this helper.
async fn open_local_file_for_write(
    local_path: &Path,
    resume_plan: ResumePlan,
) -> Result<File, String> {
    match resume_plan {
        ResumePlan::Truncate => create_local_file(local_path).await,
        ResumePlan::Resume { .. } => OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(local_path)
            .await
            .map_err(|e| {
                classify_transfer_error(
                    &format!("open local file for resume '{}'", local_path.display()),
                    &e.to_string(),
                )
            }),
        ResumePlan::Skip { .. } => Err(format!(
            "[INTERNAL_ERROR] open_local_file_for_write reached on Skip plan: {}",
            local_path.display()
        )),
    }
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
#[allow(
    clippy::too_many_arguments,
    reason = "lock-free streaming requires plumbing every shared primitive into the chunk loop"
)]
async fn download_chunks(
    remote_file: &mut SftpFile,
    local_file: &mut File,
    remote_path: &str,
    local_path: &Path,
    transfer_id: &str,
    bytes_transferred: &Arc<AtomicU64>,
    cancel_token: &CancellationToken,
    progress_tx: &broadcast::Sender<ProgressEvent>,
    data_notify: &Notify,
    total_bytes: &Arc<AtomicU64>,
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
        // ADR 0006 Amendment 1 — per-chunk delta into the
        // byte-threshold counter (mirror of the upload path).
        SUBSCRIPTION_REGISTRY.record_bytes(ResourceKind::Transfer, transfer_id, n);
        emit_tick(
            transfer_id,
            progress_tx,
            data_notify,
            bytes_transferred,
            total_bytes,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod resume_plan_decision_matrix {
        use super::*;

        // ---- decide_upload_plan ----------------------------------------

        #[test]
        fn upload_remote_missing_returns_resume_from_zero() {
            // remote_size = 0 (file absent), local_size = 1024
            let plan = decide_upload_plan(1024, 0).expect("plan ok");
            assert_eq!(
                plan,
                ResumePlan::Resume {
                    offset: 0,
                    total_bytes: 1024,
                }
            );
            assert_eq!(plan.start_offset(), 0);
        }

        #[test]
        fn upload_remote_partial_returns_resume_at_remote_size() {
            // remote 512 bytes, local 1024 bytes — resume from 512
            let plan = decide_upload_plan(1024, 512).expect("plan ok");
            assert_eq!(
                plan,
                ResumePlan::Resume {
                    offset: 512,
                    total_bytes: 1024,
                }
            );
            assert_eq!(plan.start_offset(), 512);
        }

        #[test]
        fn upload_remote_equal_returns_skip() {
            let plan = decide_upload_plan(1024, 1024).expect("plan ok");
            assert_eq!(plan, ResumePlan::Skip { total_bytes: 1024 });
            assert!(plan.is_skip());
            assert_eq!(plan.start_offset(), 1024);
        }

        #[test]
        fn upload_remote_larger_returns_resume_overshoot() {
            let err = decide_upload_plan(1024, 2048).expect_err("overshoot");
            assert!(err.starts_with("[RESUME_OVERSHOOT] preflight resume upload"));
            assert!(err.contains("remote size 2048"));
            assert!(err.contains("exceeds local size 1024"));
            assert!(err.contains("resume=false to overwrite"));
        }

        #[test]
        fn upload_zero_byte_local_returns_skip_when_remote_zero() {
            // edge case: empty local file, no remote — both 0, equal -> skip
            let plan = decide_upload_plan(0, 0).expect("plan ok");
            assert_eq!(plan, ResumePlan::Skip { total_bytes: 0 });
        }

        // ---- decide_download_plan --------------------------------------

        #[test]
        fn download_local_missing_returns_resume_from_zero() {
            // local_size = 0 (file absent), remote_size = 1024
            let plan = decide_download_plan(0, 1024).expect("plan ok");
            assert_eq!(
                plan,
                ResumePlan::Resume {
                    offset: 0,
                    total_bytes: 1024,
                }
            );
        }

        #[test]
        fn download_local_partial_returns_resume_at_local_size() {
            let plan = decide_download_plan(384, 1024).expect("plan ok");
            assert_eq!(
                plan,
                ResumePlan::Resume {
                    offset: 384,
                    total_bytes: 1024,
                }
            );
        }

        #[test]
        fn download_local_equal_returns_skip() {
            let plan = decide_download_plan(2048, 2048).expect("plan ok");
            assert_eq!(plan, ResumePlan::Skip { total_bytes: 2048 });
            assert!(plan.is_skip());
        }

        #[test]
        fn download_local_larger_returns_resume_overshoot() {
            let err = decide_download_plan(4096, 2048).expect_err("overshoot");
            assert!(err.starts_with("[RESUME_OVERSHOOT] preflight resume download"));
            assert!(err.contains("local size 4096"));
            assert!(err.contains("exceeds remote size 2048"));
        }

        // ---- ResumePlan helpers ----------------------------------------

        #[test]
        fn truncate_plan_start_offset_is_zero() {
            assert_eq!(ResumePlan::Truncate.start_offset(), 0);
            assert!(!ResumePlan::Truncate.is_skip());
        }

        #[test]
        fn skip_predicate_only_true_for_skip_variant() {
            assert!(ResumePlan::Skip { total_bytes: 1 }.is_skip());
            assert!(!ResumePlan::Truncate.is_skip());
            assert!(
                !ResumePlan::Resume {
                    offset: 1,
                    total_bytes: 2,
                }
                .is_skip()
            );
        }
    }

    mod verify_helpers {
        use super::*;

        #[test]
        fn shell_quote_wraps_in_single_quotes() {
            assert_eq!(shell_single_quote("/tmp/foo"), "'/tmp/foo'");
        }

        #[test]
        fn shell_quote_escapes_embedded_single_quote() {
            // POSIX rule: close quote, escape with backslash, reopen.
            assert_eq!(
                shell_single_quote("/path/to/it's-a-file"),
                "'/path/to/it'\\''s-a-file'"
            );
        }

        #[test]
        fn shell_quote_passes_spaces_unchanged() {
            assert_eq!(shell_single_quote("/tmp/my file"), "'/tmp/my file'");
        }

        #[test]
        fn parse_sha256_round_trip() {
            // sha256 of empty string
            let canonical = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
            let bytes = parse_sha256_hex(canonical).expect("parse ok");
            let hex = hex_encode(&bytes);
            assert_eq!(hex, canonical);
        }

        #[test]
        fn parse_sha256_handles_sha256sum_prefix() {
            // sha256sum prints "<hex>  <filename>\n"
            let line =
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  /tmp/empty\n";
            let bytes = parse_sha256_hex(line).expect("parse ok");
            assert_eq!(
                hex_encode(&bytes),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
        }

        #[test]
        fn parse_sha256_rejects_short_input() {
            assert!(parse_sha256_hex("deadbeef").is_none());
        }

        #[test]
        fn parse_sha256_rejects_non_hex() {
            // 64 chars but with a 'g' poisoning the back half
            let bad = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b8gg";
            assert!(parse_sha256_hex(bad).is_none());
        }

        #[test]
        fn upload_verify_command_uses_sha256sum_full_file() {
            let cmd = upload_verify_command("/srv/data.bin");
            assert!(cmd.contains("sha256sum -b -- '/srv/data.bin'"));
            assert!(cmd.contains("2>/dev/null"));
        }

        #[test]
        fn download_verify_command_uses_dd_pipe_sha256sum() {
            let cmd = download_verify_command("/srv/data.bin", 1024);
            assert!(cmd.contains("dd if='/srv/data.bin' bs=1 count=1024"));
            assert!(cmd.contains("sha256sum"));
        }

        #[test]
        fn hex_encode_matches_lowercase_format() {
            let mut digest = [0_u8; 32];
            for (i, b) in digest.iter_mut().enumerate() {
                *b = u8::try_from(i & 0xFF).unwrap_or(0);
            }
            let hex = hex_encode(&digest);
            assert_eq!(
                hex,
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
            );
        }
    }

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
