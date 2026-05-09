//! SFTP argument types.
//!
//! Mirrors v3 `src/mcp/tools/sftp.rs::Ssh*Args` exactly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Schemars 1.2 default-fn helpers — see `connection.rs` for rationale.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_wait() -> Option<bool> {
    Some(false)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_wait_timeout_secs() -> Option<u64> {
    Some(30)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_release_when_no_subs() -> Option<bool> {
    Some(false)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_lifecycle_grace_ms() -> Option<u32> {
    Some(2_000)
}

/// ADR 0010 default — `resume = false` preserves v6.0 behaviour.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_resume() -> Option<bool> {
    Some(false)
}

/// ADR 0010 default — `verify = false` skips the prefix hash compare.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_verify() -> Option<bool> {
    Some(false)
}

/// Arguments for the `ssh_upload` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshUploadArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Local file path to upload. Relative paths resolve against the
    /// home directory.
    pub local_path: String,

    /// Remote destination path on the SSH server.
    pub remote_path: String,

    /// v5 Phase 3 — auto-release when the transfer resource has zero
    /// subscribers. Default: false (legacy v4 behaviour).
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_release_when_no_subs")]
    pub release_when_no_subs: Option<bool>,

    /// v5 Phase 3 — grace window in ms before auto-release fires.
    /// Default: 2000.
    #[schemars(default = "default_lifecycle_grace_ms")]
    pub grace_ms: Option<u32>,

    /// ADR 0010 — opt-in resume from the remote tail. When `true`, the
    /// adapter pre-flights the remote file size and resumes from the
    /// first non-overlapping byte. When `false` (default), every upload
    /// truncates the destination and starts from byte zero (v6.0
    /// semantics). Set together with `verify=true` for a stronger
    /// guarantee that the remote prefix matches the local prefix.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_resume")]
    pub resume: Option<bool>,

    /// ADR 0010 — when `resume=true`, hash the resume prefix on both
    /// sides and abort with `RESUME_MISMATCH` if the hashes diverge.
    /// Default `false` trusts the prefix verbatim. The hash compare
    /// costs one extra `ssh_exec` round-trip plus O(offset) bytes
    /// hashed remotely; only worth enabling if mid-transfer corruption
    /// has been observed in deployment.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_verify")]
    pub verify: Option<bool>,
}

/// Arguments for the `ssh_download` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshDownloadArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Remote file path to download from the SSH server.
    pub remote_path: String,

    /// Local destination path. Relative paths resolve against the home
    /// directory.
    pub local_path: String,

    /// v5 Phase 3 — auto-release when the transfer resource has zero
    /// subscribers. Default: false (legacy v4 behaviour).
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_release_when_no_subs")]
    pub release_when_no_subs: Option<bool>,

    /// v5 Phase 3 — grace window in ms before auto-release fires.
    /// Default: 2000.
    #[schemars(default = "default_lifecycle_grace_ms")]
    pub grace_ms: Option<u32>,

    /// ADR 0010 — opt-in resume from the local tail. See
    /// [`SshUploadArgs::resume`] for the full contract.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_resume")]
    pub resume: Option<bool>,

    /// ADR 0010 — when `resume=true`, hash the resume prefix on both
    /// sides and abort with `RESUME_MISMATCH` on divergence. See
    /// [`SshUploadArgs::verify`] for the cost trade-off.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_verify")]
    pub verify: Option<bool>,
}

/// Arguments for the `ssh_transfer_progress` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshTransferProgressArgs {
    /// `TRANSFER_ID` returned from `ssh_upload` or `ssh_download`.
    pub transfer_id: String,

    /// Block until completion or `wait_timeout_secs` expires. Default:
    /// false.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_wait")]
    pub wait: Option<bool>,

    /// Maximum seconds to block when `wait=true`. Default: 30. Cap: 300.
    #[schemars(default = "default_wait_timeout_secs")]
    pub wait_timeout_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::SshTransferProgressArgs;
    use schemars::schema_for;
    use serde_json::Value;

    /// See `connection::tests::property_default` for the helper rationale.
    fn property_default<'a>(schema_json: &'a Value, field: &str) -> Option<&'a Value> {
        let property = schema_json.get("properties")?.get(field)?;
        property.get("default")
    }

    #[test]
    fn ssh_get_transfer_progress_schema_emits_documented_defaults() {
        let schema = schema_for!(SshTransferProgressArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "wait"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            property_default(&schema_json, "wait_timeout_secs"),
            Some(&Value::from(30_u64))
        );
    }
}
