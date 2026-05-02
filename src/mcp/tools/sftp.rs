//! SFTP transfer tools: `ssh_upload`, `ssh_download`,
//! `ssh_get_transfer_progress`.
//!
//! Transfers stream in 32 KiB chunks for bounded memory and report progress
//! via the global [`TRANSFER_STORAGE`]. Up to
//! [`MAX_TRANSFERS_PER_SESSION`] concurrent transfers are allowed per session.

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::fs;
use tokio::time::timeout;

use super::super::message::helpers::format_error;
use super::super::sftp::{classify_transfer_error, open_sftp_session, resolve_local_path};
use super::super::storage::session::SESSION_STORAGE;
use super::super::storage::traits::{SessionStorage, TransferStorage};
use super::super::storage::transfer::TRANSFER_STORAGE;
use super::super::transfer::MAX_TRANSFERS_PER_SESSION;
use super::legacy_helpers::{
    build_transfer_progress_response, err_session_not_found, start_download, start_upload,
    wait_for_transfer_completion,
};

// ---------------------------------------------------------------------------
// `ssh_upload`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_upload` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshUploadArgs {
    /// `SESSION_ID` returned by `ssh_connect`.
    pub session_id: String,

    /// Local file path to upload. Relative paths resolve against the home
    /// directory.
    pub local_path: String,

    /// Remote destination path on the SSH server.
    pub remote_path: String,
}

/// Implementation of `ssh_upload`.
pub async fn ssh_upload_impl(args: SshUploadArgs) -> Result<CallToolResult, McpError> {
    let SshUploadArgs {
        session_id,
        local_path,
        remote_path,
    } = args;

    let current_count = TRANSFER_STORAGE.count_by_session(&session_id);
    if current_count >= MAX_TRANSFERS_PER_SESSION {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_UPLOAD",
            "MAX_TRANSFERS_EXCEEDED",
            "maximum transfers per session reached",
            Some(&format!("limit={MAX_TRANSFERS_PER_SESSION}")),
        ))]));
    }

    let lookup = SESSION_STORAGE
        .get(&session_id)
        .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()));
    let (handle_arc, agent_id) = match lookup {
        Some(pair) => pair,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(
                err_session_not_found("SSH_UPLOAD", &session_id),
            )]));
        }
    };

    let resolved_path = resolve_local_path(&local_path);
    let metadata = match fs::metadata(&resolved_path).await {
        Ok(m) => m,
        Err(e) => {
            let reason = classify_transfer_error(
                &format!("access local file '{}'", resolved_path.display()),
                &e.to_string(),
            );
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_UPLOAD",
                "LOCAL_FILE_ERROR",
                &reason,
                None,
            ))]));
        }
    };

    if !metadata.is_file() {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_UPLOAD",
            "LOCAL_NOT_FILE",
            "path is not a regular file",
            Some(&resolved_path.display().to_string()),
        ))]));
    }

    let body = start_upload(
        session_id,
        agent_id,
        handle_arc,
        &resolved_path,
        remote_path,
        metadata.len(),
    );
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

// ---------------------------------------------------------------------------
// `ssh_download`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_download` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshDownloadArgs {
    /// `SESSION_ID` returned by `ssh_connect`.
    pub session_id: String,

    /// Remote file path to download from the SSH server.
    pub remote_path: String,

    /// Local destination path. Relative paths resolve against the home
    /// directory.
    pub local_path: String,
}

/// Implementation of `ssh_download`.
pub async fn ssh_download_impl(args: SshDownloadArgs) -> Result<CallToolResult, McpError> {
    let SshDownloadArgs {
        session_id,
        remote_path,
        local_path,
    } = args;

    let current_count = TRANSFER_STORAGE.count_by_session(&session_id);
    if current_count >= MAX_TRANSFERS_PER_SESSION {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_DOWNLOAD",
            "MAX_TRANSFERS_EXCEEDED",
            "maximum transfers per session reached",
            Some(&format!("limit={MAX_TRANSFERS_PER_SESSION}")),
        ))]));
    }

    let lookup = SESSION_STORAGE
        .get(&session_id)
        .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()));
    let (handle_arc, agent_id) = match lookup {
        Some(pair) => pair,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(
                err_session_not_found("SSH_DOWNLOAD", &session_id),
            )]));
        }
    };
    let resolved_path = resolve_local_path(&local_path);

    let sftp = match open_sftp_session(&handle_arc).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_DOWNLOAD",
                "SFTP_OPEN_FAILED",
                &e,
                None,
            ))]));
        }
    };
    let remote_metadata = match sftp.metadata(&remote_path).await {
        Ok(m) => m,
        Err(e) => {
            let reason = classify_transfer_error(
                &format!("get remote file metadata for '{remote_path}'"),
                &e.to_string(),
            );
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_DOWNLOAD",
                "REMOTE_METADATA_ERROR",
                &reason,
                None,
            ))]));
        }
    };

    let total_bytes = remote_metadata.size.unwrap_or(0);

    let body = start_download(
        session_id,
        agent_id,
        handle_arc,
        remote_path,
        &resolved_path,
        total_bytes,
    );
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

// ---------------------------------------------------------------------------
// `ssh_get_transfer_progress`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_get_transfer_progress` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshGetTransferProgressArgs {
    /// `TRANSFER_ID` returned by `ssh_upload` or `ssh_download`.
    pub transfer_id: String,

    /// Block until the transfer terminates (or `wait_timeout_secs` elapses).
    /// Default: `false`.
    pub wait: Option<bool>,

    /// Maximum seconds to wait when `wait=true`. Default: `30`. Cap: `300`.
    pub wait_timeout_secs: Option<u64>,
}

/// Implementation of `ssh_get_transfer_progress`.
pub async fn ssh_get_transfer_progress_impl(
    args: SshGetTransferProgressArgs,
) -> Result<CallToolResult, McpError> {
    let SshGetTransferProgressArgs {
        transfer_id,
        wait,
        wait_timeout_secs,
    } = args;
    let wait = wait.unwrap_or(false);
    let wait_timeout = Duration::from_secs(wait_timeout_secs.unwrap_or(30).min(300));

    let lookup = TRANSFER_STORAGE.get_direct(&transfer_id).map(|t| {
        (
            t.status_rx.clone(),
            Arc::clone(&t.bytes_transferred),
            Arc::clone(&t.total_bytes),
            Arc::clone(&t.error),
            t.info.clone(),
        )
    });
    let (status_rx, bytes_transferred, total_bytes_arc, error, transfer_info) = match lookup {
        Some(t) => t,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_GET_TRANSFER_PROGRESS",
                "TRANSFER_NOT_FOUND",
                "no transfer with the given ID",
                Some(&transfer_id),
            ))]));
        }
    };

    if wait {
        let mut rx = status_rx.clone();
        let _ = timeout(wait_timeout, wait_for_transfer_completion(&mut rx)).await;
    }

    let body = build_transfer_progress_response(
        &transfer_id,
        &status_rx,
        &bytes_transferred,
        &total_bytes_arc,
        &error,
        &transfer_info,
    );
    Ok(CallToolResult::success(vec![Content::text(body)]))
}
