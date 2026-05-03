//! Connection-domain argument types.
//!
//! Mirrors v3 `src/mcp/tools/connection.rs::Ssh*Args` exactly so existing
//! MCP clients see the same JSON schema after the v4 swap.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::domain::policy::ReusePolicy as DomainReusePolicy;

/// Reuse policy applied by `ssh_connect` when an existing session shares
/// the same `(host, port, username)` identity triple.
///
/// In v2.0.1 this was an `Option<String>` accepting
/// `"suggest"|"auto"|"force_new"`, which made typos a silent failure
/// path. v3.0 promoted it to a tagged enum rendered into the JSON schema
/// so MCP clients see the valid values; v4 keeps the surface unchanged
/// and maps it onto the domain [`crate::domain::policy::ReusePolicy`] at
/// the use case boundary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReusePolicy {
    /// Default. Return a `SUGGESTED` response listing the matching
    /// session(s) without connecting. The LLM picks an existing
    /// `SESSION_ID` or retries with `force_new`.
    #[default]
    Suggest,
    /// Reuse the most recent healthy match and return `REUSED`.
    /// Unhealthy matches are still disconnected and counted as
    /// `REPLACED`.
    Auto,
    /// Skip the lookup entirely and create a fresh connection. Existing
    /// matches are left untouched.
    ForceNew,
}

impl ReusePolicy {
    /// Translate the wire enum into the domain [`DomainReusePolicy`].
    #[must_use]
    pub const fn into_domain(self) -> DomainReusePolicy {
        match self {
            Self::Suggest => DomainReusePolicy::Suggest,
            Self::Auto => DomainReusePolicy::Auto,
            Self::ForceNew => DomainReusePolicy::ForceNew,
        }
    }
}

/// Arguments for the `ssh_connect` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshConnectArgs {
    /// Optional `SESSION_ID` returned by a previous `ssh_connect`. When
    /// provided and the session is still alive (health-check `echo 1`),
    /// short-circuits reuse evaluation and returns it as `REUSED`.
    pub session_id: Option<String>,

    /// SSH server address in the form `host:port` (e.g. `192.168.1.1:22`,
    /// `example.com:2222`). Port defaults to 22 when omitted.
    pub address: String,

    /// SSH username for authentication.
    pub username: String,

    /// Password for password-based authentication. Optional when
    /// `key_path` or an SSH agent (env `SSH_AUTH_SOCK`) is available.
    pub password: Option<String>,

    /// Path to a private key file for key-based authentication. Optional.
    /// Authentication chain order: key -> password -> agent.
    pub key_path: Option<String>,

    /// Connection timeout in seconds. Default: 30. Env:
    /// `SSH_CONNECT_TIMEOUT`.
    pub timeout_secs: Option<u64>,

    /// Maximum retry attempts for transient connection failures.
    /// Default: 3. Env: `SSH_MAX_RETRIES`.
    pub max_retries: Option<u32>,

    /// Initial delay between retries in milliseconds (exponential
    /// backoff, capped at 10s). Default: 1000. Env: `SSH_RETRY_DELAY_MS`.
    pub retry_delay_ms: Option<u64>,

    /// Enable zlib compression for the SSH transport. Default: true.
    /// Env: `SSH_COMPRESSION`.
    pub compress: Option<bool>,

    /// Optional human-readable name for the session (e.g. `production-db`,
    /// `staging-server`). Surfaces in `ssh_list_sessions` to help
    /// disambiguate identical hosts.
    pub name: Option<String>,

    /// Keep the session open indefinitely (disables inactivity timeout).
    /// Default: false. Set true for long-lived backends or daemons.
    pub persistent: Option<bool>,

    /// Optional `AGENT_ID` for grouping sessions
    /// (e.g. `claude-code-instance-abc123`). Use `ssh_disconnect_agent`
    /// to bulk-disconnect all sessions for an agent.
    pub agent_id: Option<String>,

    /// Reuse policy applied when an existing session matches the
    /// `(host, port, username)` identity triple. Values: `suggest`
    /// (default), `auto`, `force_new`.
    pub reuse: Option<ReusePolicy>,
}

/// Arguments for the `ssh_disconnect` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshDisconnectArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,
}

/// Arguments for the `ssh_list_sessions` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshListSessionsArgs {
    /// `AGENT_ID` returned from `ssh_connect`. Optional filter; when
    /// omitted returns sessions from every agent on this server.
    pub agent_id: Option<String>,

    /// Maximum entries returned. Default: 500. Cap: 10000. Env:
    /// `SSH_MCP_LIST_MAX_ITEMS` / `SSH_MCP_LIST_MAX_ITEMS_CAP`.
    pub max_items: Option<usize>,
}

/// Arguments for the `ssh_disconnect_agent` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshDisconnectAgentArgs {
    /// `AGENT_ID` returned from `ssh_connect`. All sessions owned by
    /// this agent are disconnected; sessions owned by other agents are
    /// not affected.
    pub agent_id: String,
}

#[cfg(test)]
mod tests {
    use super::ReusePolicy;
    use crate::domain::policy::ReusePolicy as DomainReusePolicy;

    #[test]
    fn reuse_policy_serde_round_trip() {
        let raw = serde_json::json!("auto");
        let policy: ReusePolicy = serde_json::from_value(raw).expect("parse");
        assert_eq!(policy, ReusePolicy::Auto);
    }

    #[test]
    fn reuse_policy_into_domain_maps_each_variant() {
        assert_eq!(
            ReusePolicy::Suggest.into_domain(),
            DomainReusePolicy::Suggest
        );
        assert_eq!(ReusePolicy::Auto.into_domain(), DomainReusePolicy::Auto);
        assert_eq!(
            ReusePolicy::ForceNew.into_domain(),
            DomainReusePolicy::ForceNew
        );
    }

    #[test]
    fn reuse_policy_default_is_suggest() {
        assert_eq!(ReusePolicy::default(), ReusePolicy::Suggest);
    }
}
