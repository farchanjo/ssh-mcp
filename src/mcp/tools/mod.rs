//! MCP tool implementations split by domain (v3.0.0).
//!
//! Replaces the monolithic `commands.rs` (2272 LOC) from v2.0.1. Each
//! sub-module owns a coherent domain:
//!
//! - [`connection`]: `ssh_connect`, `ssh_disconnect`, `ssh_list_sessions`,
//!   `ssh_disconnect_agent`
//! - `execute`: `ssh_execute`, `ssh_get_command_output`, `ssh_list_commands`,
//!   `ssh_cancel_command` (lands in E4)
//! - `shell`: `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_send_key`,
//!   `ssh_shell_read`, `ssh_shell_wait_for`, `ssh_shell_close` (E4 + E10 + E11)
//! - `sftp`: `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress` (E4)
//! - `forward`: `ssh_forward` (E4, feature-gated)
//!
//! All tools follow the rmcp 1.6 pattern:
//!
//! ```ignore
//! #[tool(description = "...")]
//! async fn ssh_x(
//!     &self,
//!     Parameters(args): Parameters<SshXArgs>,
//! ) -> Result<CallToolResult, McpError>
//! ```
//!
//! Args structs derive `Deserialize + JsonSchema`, exposing typed enums where
//! v2.0 used `Option<String>` (e.g., [`ReusePolicy`]).

pub mod connection;

use schemars::JsonSchema;
use serde::Deserialize;

/// Reuse policy applied by `ssh_connect` when an existing session shares the
/// same `(host, port, username)` identity triple.
///
/// In v2.0.1 this was an `Option<String>` accepting `"suggest"|"auto"|"force_new"`,
/// which made typos a silent failure path. v3.0.0 promotes it to a tagged enum
/// rendered into the JSON schema so MCP clients see the valid values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReusePolicy {
    /// Default. Return a `SUGGESTED` response listing the matching session(s)
    /// without connecting. The LLM picks an existing `SESSION_ID` or retries
    /// with `force_new`.
    Suggest,
    /// Reuse the most recent healthy match and return `REUSED`. Unhealthy
    /// matches are still disconnected and counted as `REPLACED`.
    Auto,
    /// Skip the lookup entirely and create a fresh connection. Existing
    /// matches are left untouched.
    ForceNew,
}

impl ReusePolicy {
    /// Map to the legacy string identifier used by internal helpers.
    #[must_use]
    pub const fn as_legacy_str(self) -> &'static str {
        match self {
            Self::Suggest => "suggest",
            Self::Auto => "auto",
            Self::ForceNew => "force_new",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_policy_serde_round_trip() {
        let raw = serde_json::json!("auto");
        let policy: ReusePolicy = serde_json::from_value(raw).unwrap();
        assert_eq!(policy, ReusePolicy::Auto);
    }

    #[test]
    fn reuse_policy_force_new_serde() {
        let raw = serde_json::json!("force_new");
        let policy: ReusePolicy = serde_json::from_value(raw).unwrap();
        assert_eq!(policy, ReusePolicy::ForceNew);
    }

    #[test]
    fn reuse_policy_legacy_str_mapping() {
        assert_eq!(ReusePolicy::Suggest.as_legacy_str(), "suggest");
        assert_eq!(ReusePolicy::Auto.as_legacy_str(), "auto");
        assert_eq!(ReusePolicy::ForceNew.as_legacy_str(), "force_new");
    }
}
