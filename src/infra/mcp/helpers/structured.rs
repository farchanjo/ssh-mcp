//! Dual-channel response builders for v4.7 `structured_content`.
//!
//! Each MCP tool now emits two payloads in parallel:
//!
//! * The legacy block-style Markdown body, kept byte-identical with v4.6
//!   so existing hosts and human operators see the exact same wire shape.
//! * A typed JSON object on `CallToolResult::structured_content`, mirroring
//!   the Markdown KEY: value pairs in `snake_case` so smaller LLMs can
//!   index into the response without parsing prose.
//!
//! The rmcp 1.6 `CallToolResult` struct is `#[non_exhaustive]`, so the
//! helpers below construct via the public `success` / `error`
//! constructors and then populate `structured_content` through the
//! exposed field. This keeps both the legacy `is_error` flag and the
//! v4.7 structured payload coherent.
//!
//! ## Runtime gate — `SSH_MCP_STRUCTURED_CONTENT`
//!
//! Both helpers consult a process-level `OnceLock<bool>` resolved on
//! first call from the `SSH_MCP_STRUCTURED_CONTENT` env var. Default
//! ON (preserves the v4.7 contract). When OFF, both helpers skip
//! setting `result.structured_content`, so the wire payload becomes
//! markdown-only. The markdown body and the `is_error` flag are
//! never touched by the gate. Schema advertisement via `output_schema`
//! on `#[tool]` is unchanged regardless — the env gates the runtime
//! emission, not the published contract.

use std::env;
use std::sync::OnceLock;

use rmcp::model::{CallToolResult, Content};
use serde_json::Value;

use crate::adapters::config::internal::STRUCTURED_CONTENT_ENV_VAR;

/// Process-level cache for the `SSH_MCP_STRUCTURED_CONTENT` decision.
///
/// reason: env reads happen once per process. Subsequent helper calls
/// hit a single atomic load + branch instead of touching `env::var`.
/// The `OnceLock` is intentionally NOT exposed to tests — they call
/// `parse_structured_env` directly to avoid the global cache (the
/// first test to read it would otherwise win for the whole process).
static STRUCTURED_GATE: OnceLock<bool> = OnceLock::new();

/// Pure parser for the `SSH_MCP_STRUCTURED_CONTENT` env var. Mirrors the
/// `SSH_COMPRESSION` parsing logic verbatim.
///
/// Returns `false` only on the explicit off triplet (`false` / `FALSE`
/// / `0`). Every other case — `None`, parse-error, unrecognised value,
/// the explicit on triplet — falls through to `true` to preserve the
/// v4.7 contract by default.
fn parse_structured_env(value: Option<&str>) -> bool {
    let Some(raw) = value else {
        return true;
    };
    if raw.eq_ignore_ascii_case("false") || raw == "0" {
        return false;
    }
    // Any other value (including "true" / "TRUE" / "1" / garbage)
    // resolves to ON. v4.7 contract is the default.
    true
}

/// Resolve the gate once per process via `OnceLock`. Subsequent calls
/// are an atomic load + branch.
fn structured_enabled() -> bool {
    *STRUCTURED_GATE.get_or_init(|| {
        parse_structured_env(env::var(STRUCTURED_CONTENT_ENV_VAR).ok().as_deref())
    })
}

/// Build a successful tool result.
///
/// Carries the v4.6 Markdown body (in `content[0]`) and, when
/// `SSH_MCP_STRUCTURED_CONTENT` is enabled (the default), a v4.7
/// typed JSON object on `structured_content`.
///
/// The text channel is byte-identical with v4.6; the structured channel
/// is purely additive. Smaller LLMs (27B class) can index by key without
/// parsing the Markdown. When the env gate is OFF, the structured
/// payload is suppressed and the result is markdown-only.
#[must_use]
pub fn ok_text_and_structured(text: String, structured: Value) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(text)]);
    if structured_enabled() {
        result.structured_content = Some(structured);
    }
    result
}

/// Build an error tool result.
///
/// Carries the v4.6 standardized error Markdown block (in
/// `content[0]`) and, when `SSH_MCP_STRUCTURED_CONTENT` is enabled
/// (the default), a v4.7 typed JSON object on `structured_content`.
///
/// The structured payload mirrors the Markdown error shape:
///
/// ```json
/// { "tool": "ssh_X", "status": "error", "code": "...", "reason": "...", "detail": "..." | null }
/// ```
///
/// The `is_error` flag is set unconditionally — only the structured
/// payload is gated by the env var.
#[must_use]
pub fn error_text_and_structured(text: String, structured: Value) -> CallToolResult {
    let mut result = CallToolResult::error(vec![Content::text(text)]);
    if structured_enabled() {
        result.structured_content = Some(structured);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{error_text_and_structured, ok_text_and_structured, parse_structured_env};
    use serde_json::json;

    #[test]
    fn ok_payload_carries_text_and_structured() {
        let text = "SSH_DISCONNECT: OK\nSESSION_ID: s-1".to_string();
        let json = json!({ "tool": "ssh_disconnect", "status": "ok", "session_id": "s-1" });
        let result = ok_text_and_structured(text.clone(), json.clone());
        assert_eq!(result.is_error, Some(false));
        // reason: the helper consults OnceLock — when env is unset (the
        // default in tests), the gate resolves ON and the structured
        // payload is attached.
        assert_eq!(result.structured_content, Some(json));
        let body = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert_eq!(body, text);
    }

    #[test]
    fn error_payload_carries_text_and_structured() {
        let text = "SSH_X: ERROR\nREASON: [CODE] reason".to_string();
        let json =
            json!({ "tool": "ssh_x", "status": "error", "code": "CODE", "reason": "reason" });
        let result = error_text_and_structured(text.clone(), json.clone());
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content, Some(json));
    }

    // The five tests below exercise `parse_structured_env` directly so
    // they do NOT race against the global `OnceLock` (which any earlier
    // test could have warmed). This mirrors the design choice
    // documented at the `STRUCTURED_GATE` declaration.

    #[test]
    fn parse_env_off_when_value_is_false() {
        assert!(!parse_structured_env(Some("false")));
        assert!(!parse_structured_env(Some("FALSE")));
        assert!(!parse_structured_env(Some("False")));
    }

    #[test]
    fn parse_env_off_when_value_is_zero() {
        assert!(!parse_structured_env(Some("0")));
    }

    #[test]
    fn parse_env_on_default_when_unset() {
        assert!(parse_structured_env(None));
    }

    #[test]
    fn parse_env_on_when_true_or_one() {
        assert!(parse_structured_env(Some("true")));
        assert!(parse_structured_env(Some("TRUE")));
        assert!(parse_structured_env(Some("True")));
        assert!(parse_structured_env(Some("1")));
    }

    #[test]
    fn parse_env_on_when_unrecognised() {
        // reason: any unknown value falls through to ON to preserve
        // the v4.7 contract on parse-error / typo.
        assert!(parse_structured_env(Some("yes")));
        assert!(parse_structured_env(Some("garbage")));
        assert!(parse_structured_env(Some("")));
        assert!(parse_structured_env(Some("2")));
    }
}
