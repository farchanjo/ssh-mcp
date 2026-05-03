//! Standardized error response builder for v4 MCP tools.
//!
//! Ports the v3 `src/mcp/message/helpers.rs::format_error` shape so every
//! tool surfaces failures through the same `TOOL: ERROR / REASON: [CODE]
//! description / DETAIL: ...` block layout.

use serde_json::{Value, json};

use super::output::{sanitize_value, truncate_utf8_safe_head};

/// Default maximum bytes allowed in an error DETAIL field. Mirrors v3
/// `DEFAULT_ERROR_DETAIL_MAX_BYTES` so the cap stays binary-compatible.
pub const DEFAULT_ERROR_DETAIL_MAX_BYTES: usize = 2048;

/// Format a standardized error response for MCP tools.
///
/// Output layout:
///
/// ```text
/// TOOL_NAME: ERROR
/// REASON: [CODE] reason text
/// DETAIL: detail text
/// ```
///
/// - `DETAIL` is omitted when `detail` is `None` or empty.
/// - `DETAIL` is head-truncated to
///   [`DEFAULT_ERROR_DETAIL_MAX_BYTES`] with a ` (truncated)` marker
///   appended when cut.
/// - Reason and detail are sanitized (control chars escaped).
#[must_use]
pub fn format_error(tool: &str, code: &str, reason: &str, detail: Option<&str>) -> String {
    let safe_reason = sanitize_value(reason);
    let capacity = tool.len() + code.len() + reason.len() + detail.map_or(0, str::len) + 32;
    let mut out = String::with_capacity(capacity);
    out.push_str(tool);
    out.push_str(": ERROR\nREASON: [");
    out.push_str(code);
    out.push_str("] ");
    out.push_str(&safe_reason);

    if let Some(d) = detail
        && !d.is_empty()
    {
        let (truncated, was_truncated) = truncate_utf8_safe_head(d, DEFAULT_ERROR_DETAIL_MAX_BYTES);
        let safe_detail = sanitize_value(truncated);
        out.push_str("\nDETAIL: ");
        out.push_str(&safe_detail);
        if was_truncated {
            out.push_str(" (truncated)");
        }
    }
    out
}

/// Build the v4.7 structured error payload mirroring [`format_error`].
///
/// Shape:
///
/// ```json
/// {
///   "tool":   "ssh_x",
///   "status": "error",
///   "code":   "EMPTY_PATTERNS",
///   "reason": "patterns must contain at least one entry",
///   "detail": "..." | null
/// }
/// ```
///
/// Tool names are emitted as lowercase to match the rmcp `tools/list`
/// catalogue. Detail is truncated to
/// [`DEFAULT_ERROR_DETAIL_MAX_BYTES`] with a trailing ` (truncated)`
/// marker so the structured channel matches the Markdown body byte-for-byte.
#[must_use]
pub fn format_error_structured(
    tool: &str,
    code: &str,
    reason: &str,
    detail: Option<&str>,
) -> Value {
    let detail_value = detail
        .filter(|d| !d.is_empty())
        .map(|d| {
            let (truncated, was_truncated) =
                truncate_utf8_safe_head(d, DEFAULT_ERROR_DETAIL_MAX_BYTES);
            if was_truncated {
                format!("{truncated} (truncated)")
            } else {
                truncated.to_string()
            }
        })
        .map_or(Value::Null, Value::String);
    json!({
        "tool":   tool.to_ascii_lowercase(),
        "status": "error",
        "code":   code,
        "reason": reason,
        "detail": detail_value,
    })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_ERROR_DETAIL_MAX_BYTES, format_error, format_error_structured};

    #[test]
    fn without_detail() {
        let e = format_error("SSH_CONNECT", "AUTH_FAILED", "authentication failed", None);
        assert_eq!(
            e,
            "SSH_CONNECT: ERROR\nREASON: [AUTH_FAILED] authentication failed"
        );
    }

    #[test]
    fn with_detail() {
        let e = format_error(
            "SSH_UPLOAD",
            "REMOTE_DIR_NOT_FOUND",
            "remote directory does not exist",
            Some("/tmp/missing"),
        );
        assert!(e.contains("DETAIL: /tmp/missing"));
    }

    #[test]
    fn long_detail_truncated() {
        let big = "x".repeat(DEFAULT_ERROR_DETAIL_MAX_BYTES + 100);
        let e = format_error("SSH_X", "CODE", "reason", Some(&big));
        assert!(e.contains("(truncated)"));
    }

    #[test]
    fn structured_carries_code_and_reason() {
        let v =
            format_error_structured("SSH_CONNECT", "AUTH_FAILED", "authentication failed", None);
        assert_eq!(v["tool"], "ssh_connect");
        assert_eq!(v["status"], "error");
        assert_eq!(v["code"], "AUTH_FAILED");
        assert_eq!(v["reason"], "authentication failed");
        assert!(v["detail"].is_null());
    }

    #[test]
    fn structured_includes_detail_when_present() {
        let v = format_error_structured(
            "SSH_UPLOAD",
            "REMOTE_DIR_NOT_FOUND",
            "remote directory does not exist",
            Some("/tmp/missing"),
        );
        assert_eq!(v["detail"], "/tmp/missing");
    }

    #[test]
    fn structured_truncates_long_detail() {
        let big = "x".repeat(DEFAULT_ERROR_DETAIL_MAX_BYTES + 100);
        let v = format_error_structured("SSH_X", "CODE", "reason", Some(&big));
        let detail = v["detail"]
            .as_str()
            .expect("detail must be a string when supplied");
        assert!(detail.ends_with("(truncated)"));
    }
}
