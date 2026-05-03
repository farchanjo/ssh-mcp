//! Output-block rendering primitives for v4 MCP responses.
//!
//! Ports the v3 `src/mcp/message/helpers.rs` truncation, sanitization,
//! human-byte formatting, and `render_output_block` helpers verbatim so
//! the markdown shape MCP clients consume stays bit-compatible.

use std::borrow::Cow;

/// Information about a truncation that occurred during rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationInfo {
    /// Number of bytes shown in the result.
    pub shown_bytes: usize,
    /// Total number of bytes in the original buffer.
    pub total_bytes: usize,
}

impl TruncationInfo {
    /// Construct a new [`TruncationInfo`].
    #[must_use]
    const fn new(shown_bytes: usize, total_bytes: usize) -> Self {
        Self {
            shown_bytes,
            total_bytes,
        }
    }

    /// Returns `true` when the truncation actually dropped content.
    #[must_use]
    pub const fn was_truncated(&self) -> bool {
        self.shown_bytes < self.total_bytes
    }
}

/// Truncate to the tail of at most `max_bytes`, UTF-8 safe.
///
/// Preserves the most recent bytes (useful for command/shell output where
/// the last bytes carry the most relevant state). When truncation lands
/// in the middle of a multi-byte UTF-8 sequence, the start is advanced
/// forward to the next valid char boundary.
///
/// A `max_bytes == 0` call returns an empty string with truncation info
/// reflecting the original buffer size.
#[must_use]
pub fn truncate_utf8_safe_tail(buffer: &[u8], max_bytes: usize) -> (String, TruncationInfo) {
    let total = buffer.len();
    if max_bytes == 0 {
        return (String::new(), TruncationInfo::new(0, total));
    }
    if total <= max_bytes {
        let s = String::from_utf8_lossy(buffer).into_owned();
        return (s, TruncationInfo::new(total, total));
    }
    let safe_start = find_char_boundary_forward(buffer, total - max_bytes);
    let slice = &buffer[safe_start..];
    let s = String::from_utf8_lossy(slice).into_owned();
    (s, TruncationInfo::new(slice.len(), total))
}

/// Truncate a string to at most `max_bytes` at a UTF-8 boundary
/// (head-side).
///
/// Returns a borrowed slice (zero-alloc when possible) plus a boolean
/// indicating whether truncation occurred.
#[must_use]
pub fn truncate_utf8_safe_head(s: &str, max_bytes: usize) -> (&str, bool) {
    if s.len() <= max_bytes {
        return (s, false);
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// Find the next valid UTF-8 char boundary at or after `from`.
fn find_char_boundary_forward(buffer: &[u8], from: usize) -> usize {
    let mut idx = from;
    while idx < buffer.len() {
        if buffer[idx] & 0xC0 != 0x80 {
            return idx;
        }
        idx += 1;
    }
    buffer.len()
}

/// Escape control characters (`\n`, `\r`, `\t`) to literal `\\n`, `\\r`,
/// `\\t`. Returns `Cow::Borrowed` when no escape is needed.
#[must_use]
pub fn sanitize_value(s: &str) -> Cow<'_, str> {
    if !s.bytes().any(|b| matches!(b, b'\n' | b'\r' | b'\t')) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

/// Format bytes in a human-readable way (`B`, `KB`, `MB`, `GB`).
#[allow(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "display-only formatting where precision loss is acceptable for human-readable output"
)]
#[must_use]
pub fn format_bytes_human(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        let v = bytes as f64 / GB as f64;
        format!("{v:.1}GB")
    } else if bytes >= MB {
        let v = bytes as f64 / MB as f64;
        format!("{v:.1}MB")
    } else if bytes >= KB {
        let v = bytes as f64 / KB as f64;
        format!("{v:.1}KB")
    } else {
        format!("{bytes}B")
    }
}

/// Render an output block (stdout/stderr/data) with anti-injection
/// delimiter. Mirrors v3 `render_output_block` verbatim.
#[must_use]
pub fn render_output_block(
    name: &str,
    nonce: &str,
    buffer: &[u8],
    max_bytes: usize,
    status_hint: Option<&str>,
) -> String {
    let (content, info) = truncate_utf8_safe_tail(buffer, max_bytes);
    let is_empty = content.is_empty();
    let capacity = name.len() + nonce.len() + 16 + content.len() + 64;
    let mut out = String::with_capacity(capacity);
    out.push_str("--- ");
    out.push_str(name);
    out.push_str(" [");
    out.push_str(nonce);
    out.push(']');
    append_annotations(&mut out, status_hint, &info, is_empty);
    out.push_str(" ---");
    if !is_empty {
        out.push('\n');
        let trimmed = content.strip_suffix('\n').unwrap_or(&content);
        out.push_str(trimmed);
    }
    out
}

fn append_annotations(out: &mut String, hint: Option<&str>, info: &TruncationInfo, is_empty: bool) {
    let has_truncated = !is_empty && info.was_truncated();
    if hint.is_none() && !is_empty && !has_truncated {
        return;
    }
    out.push_str(" (");
    if let Some(h) = hint {
        out.push_str(h);
    }
    if is_empty {
        if hint.is_some() {
            out.push_str(", ");
        }
        out.push_str("empty");
    } else if has_truncated {
        if hint.is_some() {
            out.push_str(", ");
        }
        append_truncation_text(out, info);
    }
    out.push(')');
}

fn append_truncation_text(out: &mut String, info: &TruncationInfo) {
    let shown = u64::try_from(info.shown_bytes).unwrap_or(u64::MAX);
    let total = u64::try_from(info.total_bytes).unwrap_or(u64::MAX);
    out.push_str("truncated: showing ");
    out.push_str(&format_bytes_human(shown));
    out.push_str(" of ");
    out.push_str(&format_bytes_human(total));
}

#[cfg(test)]
mod tests {
    use super::{
        format_bytes_human, render_output_block, sanitize_value, truncate_utf8_safe_head,
        truncate_utf8_safe_tail,
    };

    #[test]
    fn buffer_smaller_than_max_returns_full_string() {
        let (s, info) = truncate_utf8_safe_tail(b"hello", 100);
        assert_eq!(s, "hello");
        assert!(!info.was_truncated());
    }

    #[test]
    fn buffer_larger_than_max_returns_tail() {
        let (s, info) = truncate_utf8_safe_tail(b"abcdefghij", 4);
        assert_eq!(s, "ghij");
        assert!(info.was_truncated());
    }

    #[test]
    fn head_truncate_keeps_tail() {
        let (s, t) = truncate_utf8_safe_head("abcdefghij", 4);
        assert_eq!(s, "abcd");
        assert!(t);
    }

    #[test]
    fn sanitize_escapes_newlines() {
        let out = sanitize_value("line1\nline2");
        assert_eq!(out, "line1\\nline2");
    }

    #[test]
    fn format_bytes_under_kb() {
        assert_eq!(format_bytes_human(512), "512B");
    }

    #[test]
    fn format_bytes_kb() {
        assert_eq!(format_bytes_human(1024), "1.0KB");
    }

    #[test]
    fn render_output_block_empty_buffer() {
        let out = render_output_block("stdout", "deadbeef", b"", 1024, None);
        assert_eq!(out, "--- stdout [deadbeef] (empty) ---");
    }

    #[test]
    fn render_output_block_with_content() {
        let out = render_output_block("stdout", "deadbeef", b"hi", 1024, None);
        assert_eq!(out, "--- stdout [deadbeef] ---\nhi");
    }
}
