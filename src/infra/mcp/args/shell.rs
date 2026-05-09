//! Interactive PTY shell argument types.
//!
//! Mirrors v3 `src/mcp/tools/shell.rs::Ssh*Args` exactly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::keys::ShellKey;

// Schemars 1.2 default-fn helpers — see `connection.rs` for rationale.
// String-returning helpers cannot be `const` because `String::from` is
// not a const fn; numeric/bool helpers are.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<String>"
)]
fn default_term() -> Option<String> {
    Some("xterm".to_string())
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_cols() -> Option<u32> {
    Some(80)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_rows() -> Option<u32> {
    Some(24)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_inactivity_ttl() -> Option<u64> {
    Some(600)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<String>"
)]
fn default_max_buffer_size() -> Option<String> {
    Some("10m".to_string())
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_modifier() -> Option<bool> {
    Some(false)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_repeat() -> Option<u8> {
    Some(1)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_clear() -> Option<bool> {
    Some(true)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_max_output_bytes() -> Option<usize> {
    Some(16384)
}
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
const fn default_min_bytes() -> Option<usize> {
    Some(1)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_pattern_timeout_secs() -> Option<u64> {
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

/// Arguments for the `ssh_shell_open` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshShellOpenArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Terminal type. Default: `xterm`. Use `vt100` or `ansi` for
    /// SOL/IPMI/serial consoles.
    #[schemars(default = "default_term")]
    pub term: Option<String>,

    /// Terminal width in columns. Default: 80.
    #[schemars(default = "default_cols")]
    pub cols: Option<u32>,

    /// Terminal height in rows. Default: 24.
    #[schemars(default = "default_rows")]
    pub rows: Option<u32>,

    /// Inactivity TTL in seconds. Shell auto-closes if no read or write
    /// happens within this window. Default: 600. Env:
    /// `SSH_SHELL_INACTIVITY_TTL`.
    #[schemars(default = "default_inactivity_ttl")]
    pub inactivity_ttl: Option<u64>,

    /// Maximum output buffer size. Accepts human sizes like `512k`,
    /// `10m`, `1g`, `1t`. Default: `10m`. Env: `SSH_SHELL_MAX_BUFFER_SIZE`.
    /// Oldest bytes dropped first when full.
    #[schemars(default = "default_max_buffer_size")]
    pub max_buffer_size: Option<String>,

    /// v5 Phase 3 — auto-release when the resource has zero
    /// subscribers. Default: false (legacy v4 behaviour).
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_release_when_no_subs")]
    pub release_when_no_subs: Option<bool>,

    /// v5 Phase 3 — grace window in ms before auto-release fires.
    /// Default: 2000.
    #[schemars(default = "default_lifecycle_grace_ms")]
    pub grace_ms: Option<u32>,
}

/// Arguments for the `ssh_shell_write` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshShellWriteArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// Bytes to send to the PTY. Append `\n` to submit a typed command.
    /// Use control sequences directly (e.g. `\x03` for Ctrl+C, `\x1b[A`
    /// for arrow up). Prefer `ssh_shell_press` for named keystrokes.
    pub input: String,
}

/// Arguments for the `ssh_shell_press` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshShellPressArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// Named keystroke to send.
    ///
    /// Per-key modifier policy (modifiers beyond the listed set return
    /// error code `MODIFIER_NOT_ALLOWED`):
    ///
    /// - Arrow keys (`arrow_up`, `arrow_down`, `arrow_left`, `arrow_right`)
    ///   accept any of `shift` / `alt` / `ctrl`.
    /// - Navigation (`home`, `end`, `page_up`, `page_down`, `insert`,
    ///   `delete`) accept any of `shift` / `alt` / `ctrl`.
    /// - Function keys `f1`-`f12` accept any of `shift` / `alt` / `ctrl`.
    /// - `tab` accepts only `shift` (back-tab); rejects `alt` / `ctrl`.
    /// - All other keys (`ctrl_c`, `ctrl_d`, `ctrl_z`, `ctrl_l`,
    ///   `ctrl_r`, `ctrl_w`, `ctrl_u`, `enter`, `escape`, `backspace`,
    ///   `space`) reject every modifier — the keystroke is already a
    ///   complete control byte.
    ///
    /// Examples: `ctrl_c`, `arrow_up`, `f5`, `enter`, `tab`.
    pub key: ShellKey,

    /// Apply Shift modifier. Default: false. Valid on: arrows,
    /// navigation keys, F1-F12, and `tab` (back-tab). See the `key` field
    /// for the full per-key policy.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_modifier")]
    pub shift: Option<bool>,

    /// Apply Alt modifier. Default: false. Valid on: arrows, navigation
    /// keys, and F1-F12. See the `key` field for the full per-key policy.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_modifier")]
    pub alt: Option<bool>,

    /// Apply Ctrl modifier. Default: false. Valid on: arrows, navigation
    /// keys, and F1-F12. Cannot be combined with `ctrl_*` keys (already a
    /// complete control byte). See the `key` field for the full per-key
    /// policy.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_modifier")]
    pub ctrl: Option<bool>,

    /// Repeat the keystroke N times. Default: 1. Range: 1..=64. Values
    /// outside this range return error code `INVALID_REPEAT`.
    #[schemars(default = "default_repeat")]
    pub repeat: Option<u8>,
}

/// Arguments for the `ssh_shell_read` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshShellReadArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// Drain the bytes that were rendered (head-based pagination).
    /// Default: true. With false the buffer is preserved (peek mode) for
    /// inspecting the same window multiple times.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: true.
    #[schemars(default = "default_clear")]
    pub clear: Option<bool>,

    /// Maximum bytes shown in data block. Default: 16384. Cap: 1048576.
    /// Output rendered as the tail (most recent bytes). Env:
    /// `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    #[schemars(default = "default_max_output_bytes")]
    pub max_output_bytes: Option<usize>,

    /// FALLBACK long-poll. Block until `min_bytes` of new output arrive,
    /// the shell closes, or `wait_timeout_secs` expires. Default: false.
    /// Prefer `resources/subscribe shell://<shell_id>/output` (realtime
    /// push) over polling.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: false.
    #[schemars(default = "default_wait")]
    pub wait: Option<bool>,

    /// Maximum seconds to block when `wait=true`. Default: 30. Cap: 300.
    #[schemars(default = "default_wait_timeout_secs")]
    pub wait_timeout_secs: Option<u64>,

    /// Minimum new bytes to wait for before returning (only with
    /// `wait=true`). Default: 1 (any new byte returns). Capped at the
    /// resolved `max_output_bytes`. Floor: 1.
    #[schemars(default = "default_min_bytes")]
    pub min_bytes: Option<usize>,
}

/// Arguments for the `ssh_shell_wait_for` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshShellWaitForArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// 1..=16 substring patterns. First match returns immediately as
    /// `MATCHED_PATTERN`. Each pattern up to 1024 bytes. Prefer
    /// `resources/subscribe shell://<shell_id>/output` (realtime push)
    /// over polling — use this for single-shot prompt gating.
    ///
    /// Errors: empty list returns code `EMPTY_PATTERNS`. List with more
    /// than 16 entries returns `TOO_MANY_PATTERNS`. Any single pattern
    /// over 1024 bytes returns `PATTERN_TOO_LONG`.
    pub patterns: Vec<String>,

    /// Maximum seconds to wait. Default: 30. Cap: 300.
    #[schemars(default = "default_pattern_timeout_secs")]
    pub timeout_secs: Option<u64>,

    /// Maximum bytes shown when matched. Default: 16384. Cap: 1048576.
    /// Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` /
    /// `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    #[schemars(default = "default_max_output_bytes")]
    pub max_output_bytes: Option<usize>,

    /// Drain matched output from the shell history (head) after
    /// returning so subsequent reads start fresh. Default: true.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: true.
    #[schemars(default = "default_clear")]
    pub clear: Option<bool>,
}

/// Arguments for the `ssh_shell_close` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshShellCloseArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,
}

#[cfg(test)]
mod tests {
    use super::{SshShellOpenArgs, SshShellPressArgs, SshShellReadArgs, SshShellWaitForArgs};
    use schemars::schema_for;
    use serde_json::Value;

    /// See `connection::tests::property_default` for the helper rationale.
    fn property_default<'a>(schema_json: &'a Value, field: &str) -> Option<&'a Value> {
        let property = schema_json.get("properties")?.get(field)?;
        property.get("default")
    }

    #[test]
    fn ssh_shell_open_schema_emits_documented_defaults() {
        let schema = schema_for!(SshShellOpenArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "term"),
            Some(&Value::from("xterm"))
        );
        assert_eq!(
            property_default(&schema_json, "cols"),
            Some(&Value::from(80_u32))
        );
        assert_eq!(
            property_default(&schema_json, "rows"),
            Some(&Value::from(24_u32))
        );
        assert_eq!(
            property_default(&schema_json, "inactivity_ttl"),
            Some(&Value::from(600_u64))
        );
        assert_eq!(
            property_default(&schema_json, "max_buffer_size"),
            Some(&Value::from("10m"))
        );
    }

    #[test]
    fn ssh_shell_send_key_schema_emits_modifier_and_repeat_defaults() {
        let schema = schema_for!(SshShellPressArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "shift"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            property_default(&schema_json, "alt"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            property_default(&schema_json, "ctrl"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            property_default(&schema_json, "repeat"),
            Some(&Value::from(1_u8))
        );
    }

    #[test]
    fn ssh_shell_read_schema_emits_documented_defaults() {
        let schema = schema_for!(SshShellReadArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "clear"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            property_default(&schema_json, "max_output_bytes"),
            Some(&Value::from(16384_usize))
        );
        assert_eq!(
            property_default(&schema_json, "wait"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            property_default(&schema_json, "wait_timeout_secs"),
            Some(&Value::from(30_u64))
        );
        assert_eq!(
            property_default(&schema_json, "min_bytes"),
            Some(&Value::from(1_usize))
        );
    }

    #[test]
    fn ssh_shell_wait_for_schema_emits_documented_defaults() {
        let schema = schema_for!(SshShellWaitForArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "timeout_secs"),
            Some(&Value::from(30_u64))
        );
        assert_eq!(
            property_default(&schema_json, "max_output_bytes"),
            Some(&Value::from(16384_usize))
        );
        assert_eq!(
            property_default(&schema_json, "clear"),
            Some(&Value::Bool(true))
        );
    }
}
