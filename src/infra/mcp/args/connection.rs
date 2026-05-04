//! Connection-domain argument types.
//!
//! Mirrors v3 `src/mcp/tools/connection.rs::Ssh*Args` exactly so existing
//! MCP clients see the same JSON schema after the v4 swap.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
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

// Schemars 1.2 default-fn helpers. The `#[schemars(default = "fn")]`
// attribute is parsed by serde-derive-internals (see
// `schemars_derive::attr::schemars_to_serde::SERDE_KEYWORDS`), so the
// fn signature must match the field type — i.e. `() -> Option<T>`.
// Schemars then serializes the returned value into the JSON Schema
// `default` keyword via `_schemars_maybe_to_value!` (see
// `schemars_derive/src/schema_exprs.rs:709`).
//
// `clippy::unnecessary_wraps` is allowed here because the `Option<T>`
// return is not stylistic — it's required by serde's contract for
// `default = "fn"` on an `Option<T>` field.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_timeout_secs() -> Option<u64> {
    Some(30)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_max_retries() -> Option<u32> {
    Some(3)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_retry_delay_ms() -> Option<u64> {
    Some(1000)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_compress() -> Option<bool> {
    Some(true)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_persistent() -> Option<bool> {
    Some(false)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_max_items() -> Option<usize> {
    Some(500)
}

/// Arguments for the `ssh_connect` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
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
    #[schemars(default = "default_timeout_secs")]
    pub timeout_secs: Option<u64>,

    /// Maximum retry attempts for transient connection failures.
    /// Default: 3. Env: `SSH_MAX_RETRIES`.
    #[schemars(default = "default_max_retries")]
    pub max_retries: Option<u32>,

    /// Initial delay between retries in milliseconds (exponential
    /// backoff, capped at 10s). Default: 1000. Env: `SSH_RETRY_DELAY_MS`.
    #[schemars(default = "default_retry_delay_ms")]
    pub retry_delay_ms: Option<u64>,

    /// Enable zlib compression for the SSH transport. Default: true.
    /// Env: `SSH_COMPRESSION`.
    #[schemars(default = "default_compress")]
    pub compress: Option<bool>,

    /// Optional human-readable name for the session (e.g. `production-db`,
    /// `staging-server`). Surfaces in `ssh_list_sessions` to help
    /// disambiguate identical hosts.
    pub name: Option<String>,

    /// Keep the session open indefinitely (disables inactivity timeout).
    /// Default: false. Set true for long-lived backends or daemons.
    #[schemars(default = "default_persistent")]
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
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshDisconnectArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,
}

/// Arguments for the `ssh_list_sessions` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshListSessionsArgs {
    /// `AGENT_ID` returned from `ssh_connect`. Optional filter; when
    /// omitted returns sessions from every agent on this server.
    pub agent_id: Option<String>,

    /// Maximum entries returned. Default: 500. Cap: 10000. Env:
    /// `SSH_MCP_LIST_MAX_ITEMS` / `SSH_MCP_LIST_MAX_ITEMS_CAP`.
    #[schemars(default = "default_max_items")]
    pub max_items: Option<usize>,
}

/// Arguments for the `ssh_disconnect_agent` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshDisconnectAgentArgs {
    /// `AGENT_ID` returned from `ssh_connect`. All sessions owned by
    /// this agent are disconnected; sessions owned by other agents are
    /// not affected.
    pub agent_id: String,
}

/// Arguments for the `ssh_disconnect_many` MCP tool — best-effort
/// bulk disconnect of multiple sessions in a single call.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshDisconnectManyArgs {
    /// 1..=64 `SESSION_ID`s to disconnect. Per-id failures are
    /// reported in the response and do not abort the remaining
    /// disconnects.
    pub session_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{ReusePolicy, SshConnectArgs, SshListSessionsArgs};
    use crate::domain::policy::ReusePolicy as DomainReusePolicy;
    use schemars::schema_for;
    use serde_json::Value;

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

    /// Pull the `default` keyword from a generated property schema.
    ///
    /// Walks the JSON Schema 2020-12 shape produced by `schemars 1.2`:
    /// `{ "properties": { "<field>": { "default": <value>, ... } } }`.
    /// Returns `None` when any step misses so the test asserts on the
    /// presence/absence path explicitly instead of unwrapping mid-walk.
    fn property_default<'a>(schema_json: &'a Value, field: &str) -> Option<&'a Value> {
        let property = schema_json.get("properties")?.get(field)?;
        property.get("default")
    }

    #[test]
    fn ssh_connect_schema_emits_documented_defaults() {
        let schema = schema_for!(SshConnectArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");

        // Defaults documented on the doc-comment must reach the
        // JSON Schema `default` keyword so smaller LLMs that read
        // the schema mechanically (rather than the description) can
        // see them.
        assert_eq!(
            property_default(&schema_json, "timeout_secs"),
            Some(&Value::from(30_u64)),
            "timeout_secs default missing in schema; full schema = {schema_json}"
        );
        assert_eq!(
            property_default(&schema_json, "max_retries"),
            Some(&Value::from(3_u32))
        );
        assert_eq!(
            property_default(&schema_json, "retry_delay_ms"),
            Some(&Value::from(1000_u64))
        );
        assert_eq!(
            property_default(&schema_json, "compress"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            property_default(&schema_json, "persistent"),
            Some(&Value::Bool(false))
        );

        // Fields without a documented default must NOT carry one.
        assert!(
            property_default(&schema_json, "session_id").is_none(),
            "session_id has no documented default but schema emitted one"
        );
        assert!(property_default(&schema_json, "name").is_none());
        assert!(property_default(&schema_json, "agent_id").is_none());
    }

    #[test]
    fn ssh_list_sessions_schema_emits_max_items_default() {
        let schema = schema_for!(SshListSessionsArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "max_items"),
            Some(&Value::from(500_usize))
        );
        assert!(property_default(&schema_json, "agent_id").is_none());
    }

    #[test]
    fn ssh_disconnect_many_args_round_trip() {
        use super::SshDisconnectManyArgs;
        let raw = serde_json::json!({
            "session_ids": ["sess-1", "sess-2", "sess-3"],
        });
        let parsed: SshDisconnectManyArgs = serde_json::from_value(raw).expect("parse");
        assert_eq!(parsed.session_ids.len(), 3);
        assert_eq!(parsed.session_ids[0], "sess-1");
    }
}
