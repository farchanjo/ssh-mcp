//! SFTP argument types.
//!
//! Mirrors v3 `src/mcp/tools/sftp.rs::Ssh*Args` exactly.

use schemars::JsonSchema;
use serde::Deserialize;

/// Arguments for the `ssh_upload` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshUploadArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Local file path to upload. Relative paths resolve against the
    /// home directory.
    pub local_path: String,

    /// Remote destination path on the SSH server.
    pub remote_path: String,
}

/// Arguments for the `ssh_download` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshDownloadArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Remote file path to download from the SSH server.
    pub remote_path: String,

    /// Local destination path. Relative paths resolve against the home
    /// directory.
    pub local_path: String,
}

/// Arguments for the `ssh_get_transfer_progress` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshGetTransferProgressArgs {
    /// `TRANSFER_ID` returned from `ssh_upload` or `ssh_download`.
    pub transfer_id: String,

    /// Block until completion or `wait_timeout_secs` expires. Default:
    /// false.
    pub wait: Option<bool>,

    /// Maximum seconds to block when `wait=true`. Default: 30. Cap: 300.
    pub wait_timeout_secs: Option<u64>,
}
