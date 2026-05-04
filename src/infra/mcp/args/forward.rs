//! Port-forward argument types (feature-gated).
//!
//! Mirrors v3 `src/mcp/tools/forward.rs::SshForwardArgs` exactly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Arguments for the `ssh_forward` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshForwardArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Local TCP port to listen on (e.g. `8080`).
    pub local_port: u16,

    /// Remote host on the server side to forward to (e.g. `localhost`
    /// or `10.0.0.1`).
    pub remote_address: String,

    /// Remote TCP port to forward to (e.g. `3306` for `MySQL`).
    pub remote_port: u16,
}
