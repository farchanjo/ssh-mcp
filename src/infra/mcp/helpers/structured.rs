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

use rmcp::model::{CallToolResult, Content};
use serde_json::Value;

/// Build a successful tool result carrying both the v4.6 Markdown body
/// (in `content[0]`) and a v4.7 typed JSON object on
/// `structured_content`.
///
/// The text channel is byte-identical with v4.6; the structured channel
/// is purely additive. Smaller LLMs (27B class) can index by key without
/// parsing the Markdown.
#[must_use]
pub fn ok_text_and_structured(text: String, structured: Value) -> CallToolResult {
    let mut result = CallToolResult::success(vec![Content::text(text)]);
    result.structured_content = Some(structured);
    result
}

/// Build an error tool result carrying both the v4.6 standardized error
/// Markdown block (in `content[0]`) and a v4.7 typed JSON object on
/// `structured_content`.
///
/// The structured payload mirrors the Markdown error shape:
///
/// ```json
/// { "tool": "ssh_X", "status": "error", "code": "...", "reason": "...", "detail": "..." | null }
/// ```
#[must_use]
pub fn error_text_and_structured(text: String, structured: Value) -> CallToolResult {
    let mut result = CallToolResult::error(vec![Content::text(text)]);
    result.structured_content = Some(structured);
    result
}

#[cfg(test)]
mod tests {
    use super::{error_text_and_structured, ok_text_and_structured};
    use serde_json::json;

    #[test]
    fn ok_payload_carries_text_and_structured() {
        let text = "SSH_DISCONNECT: OK\nSESSION_ID: s-1".to_string();
        let json = json!({ "tool": "ssh_disconnect", "status": "ok", "session_id": "s-1" });
        let result = ok_text_and_structured(text.clone(), json.clone());
        assert_eq!(result.is_error, Some(false));
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
}
