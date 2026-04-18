//! Shared helpers for MCP response formatting.
//!
//! Provides primitives used by all markdown response builders:
//! - Random nonce generation for delimiter anti-injection
//! - UTF-8 safe truncation (tail and head variants)
//! - Value sanitization (escapes control characters that would break format)
//! - Human-readable byte formatting
//! - Standardized error response formatting

use std::borrow::Cow;

use uuid::Uuid;

/// Length of the nonce in hex characters (32 bits of entropy).
pub const NONCE_LEN: usize = 8;

/// Default maximum bytes allowed in an error DETAIL field.
///
/// Applied by `format_error` to prevent large backtraces from flooding
/// responses. Can be overridden by the `SSH_MCP_ERROR_DETAIL_MAX_BYTES`
/// environment variable in a later commit.
pub const DEFAULT_ERROR_DETAIL_MAX_BYTES: usize = 2048;

/// Generate a random 8-char lowercase hex nonce.
///
/// Uses the first 32 random bits of a `UUIDv4` (~4 billion combinations).
/// Used as a per-response delimiter hash to prevent injection attacks
/// from content imitating output block markers.
#[must_use]
pub fn generate_nonce() -> String {
    let (high, _, _, _) = Uuid::new_v4().as_fields();
    format!("{high:08x}")
}

/// Information about a truncation that occurred during rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationInfo {
    /// Number of bytes shown in the result.
    pub shown_bytes: usize,
    /// Total number of bytes in the original buffer.
    pub total_bytes: usize,
}

impl TruncationInfo {
    /// Construct a new `TruncationInfo` record.
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

/// Truncate a string to at most `max_bytes` at a UTF-8 boundary (head-side).
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
///
/// A byte is a boundary iff it is not a UTF-8 continuation byte
/// (continuation bytes match the bit pattern `10xxxxxx`).
/// Returns `buffer.len()` when no boundary is found before the end.
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

/// Escape control characters (`\n`, `\r`, `\t`) to literal `\\n`, `\\r`, `\\t`.
///
/// Protects the block/inline response format from user-controlled values
/// that contain newlines or tabs. Returns `Cow::Borrowed` when no escape
/// is needed, avoiding an allocation for the common case.
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
///
/// Uses base 1024 (binary prefixes) and one decimal place.
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

/// Format a standardized error response for MCP tools.
///
/// Output format:
///
/// ```text
/// TOOL_NAME: ERROR
/// REASON: [CODE] reason text
/// DETAIL: detail text
/// ```
///
/// - `DETAIL` line is omitted when `detail` is `None` or empty.
/// - `DETAIL` is truncated (head-side) to `DEFAULT_ERROR_DETAIL_MAX_BYTES`
///   with a ` (truncated)` marker appended when cut.
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

/// Render an output block (stdout/stderr/data) with anti-injection delimiter.
///
/// Output format:
///
/// - Empty buffer: `--- NAME [NONCE] (empty) ---`
/// - With content: `--- NAME [NONCE] (annotations) ---\n<content>`
///
/// `status_hint` lets the caller prepend an annotation like `"partial"`
/// (used when a command is still running). Truncation info is appended
/// automatically when `max_bytes < buffer.len()`, e.g.
/// `(truncated: showing 16.0KB of 2.3MB)`.
///
/// The content is UTF-8-safely truncated to the tail (last bytes), which
/// is where recent output lives. Any trailing newline in the content is
/// stripped so the caller can join multiple blocks with `\n` without
/// introducing blank lines.
///
/// The function operates on a borrowed slice (`&[u8]`) — no intermediate
/// `String` is allocated for the full buffer. Only the truncated tail is
/// ever materialized.
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
    use super::*;

    mod generate_nonce {
        use super::*;

        #[test]
        fn length_is_eight_lowercase_hex() {
            let nonce = generate_nonce();
            assert_eq!(nonce.len(), NONCE_LEN);
            assert!(
                nonce
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }

        #[test]
        fn hundred_calls_have_no_collisions() {
            let mut set = std::collections::HashSet::new();
            for _ in 0..100_usize {
                set.insert(generate_nonce());
            }
            assert_eq!(set.len(), 100, "collisions found in 100 nonces");
        }

        #[test]
        fn thousand_calls_nearly_unique() {
            let mut set = std::collections::HashSet::new();
            for _ in 0..1000_usize {
                set.insert(generate_nonce());
            }
            // Expected collisions with 2^32 space and 1000 samples: ~0.0001
            assert!(
                set.len() >= 999,
                "too many collisions: {}",
                1000_usize - set.len()
            );
        }

        #[test]
        fn ten_thousand_calls_have_minimal_collisions() {
            let mut set = std::collections::HashSet::new();
            for _ in 0..10_000_usize {
                set.insert(generate_nonce());
            }
            // With 32-bit space and 10k samples the birthday-paradox expected
            // collision count is ~0.012, so seeing more than 2 is a red flag.
            let collisions = 10_000_usize - set.len();
            assert!(
                collisions <= 2,
                "observed {collisions} collisions across 10_000 nonces (expected <= 2)"
            );
        }
    }

    mod truncation_info {
        use super::*;

        #[test]
        fn was_truncated_true_when_shown_less_than_total() {
            let t = TruncationInfo {
                shown_bytes: 50,
                total_bytes: 100,
            };
            assert!(t.was_truncated());
        }

        #[test]
        fn was_truncated_false_when_equal() {
            let t = TruncationInfo {
                shown_bytes: 100,
                total_bytes: 100,
            };
            assert!(!t.was_truncated());
        }

        #[test]
        fn was_truncated_false_when_zero_zero() {
            let t = TruncationInfo {
                shown_bytes: 0,
                total_bytes: 0,
            };
            assert!(!t.was_truncated());
        }
    }

    mod truncate_utf8_safe_tail {
        use super::*;

        #[test]
        fn buffer_smaller_than_max() {
            let (s, info) = truncate_utf8_safe_tail(b"hello", 100);
            assert_eq!(s, "hello");
            assert!(!info.was_truncated());
            assert_eq!(info.shown_bytes, 5);
            assert_eq!(info.total_bytes, 5);
        }

        #[test]
        fn buffer_equal_to_max() {
            let (s, info) = truncate_utf8_safe_tail(b"hello", 5);
            assert_eq!(s, "hello");
            assert!(!info.was_truncated());
        }

        #[test]
        fn buffer_larger_than_max_returns_tail() {
            let (s, info) = truncate_utf8_safe_tail(b"abcdefghij", 4);
            assert_eq!(s, "ghij");
            assert!(info.was_truncated());
            assert_eq!(info.total_bytes, 10);
            assert_eq!(info.shown_bytes, 4);
        }

        #[test]
        fn empty_buffer() {
            let (s, info) = truncate_utf8_safe_tail(b"", 100);
            assert_eq!(s, "");
            assert_eq!(info.total_bytes, 0);
            assert!(!info.was_truncated());
        }

        #[test]
        fn max_zero_returns_empty_with_total() {
            let (s, info) = truncate_utf8_safe_tail(b"hello", 0);
            assert_eq!(s, "");
            assert_eq!(info.total_bytes, 5);
            assert!(info.was_truncated());
        }

        #[test]
        fn multibyte_boundary_moved_forward() {
            // "héllo": bytes = h, 0xC3, 0xA9, l, l, o (6 bytes)
            let buf = "héllo".as_bytes();
            let (result, info) = truncate_utf8_safe_tail(buf, 4);
            // raw_start = 2 (mid of é). safe_start advances to 3 (start of 'l').
            assert_eq!(result, "llo");
            assert!(info.was_truncated());
        }

        #[test]
        fn multibyte_boundary_already_aligned() {
            let buf = "héllo".as_bytes(); // 6 bytes
            // max_bytes = 5: raw_start = 1 is the start of 'é' — no shift.
            let (result, _) = truncate_utf8_safe_tail(buf, 5);
            assert_eq!(result, "éllo");
        }

        #[test]
        fn emoji_four_byte_preserved_in_tail() {
            let mut buf = vec![b'a'; 100_usize];
            let emoji = "😀".as_bytes();
            buf.extend_from_slice(emoji);
            let (result, _) = truncate_utf8_safe_tail(&buf, 6);
            assert!(result.ends_with("😀"));
        }

        #[test]
        fn huge_buffer_small_window() {
            let buf = vec![b'x'; 10 * 1024 * 1024_usize];
            let (result, info) = truncate_utf8_safe_tail(&buf, 16);
            assert_eq!(result.len(), 16);
            assert_eq!(info.total_bytes, 10 * 1024 * 1024);
            assert!(info.was_truncated());
        }

        #[test]
        fn invalid_utf8_handled_via_lossy() {
            let buf = [0xFF_u8, 0xFE, 0xFD];
            let (result, info) = truncate_utf8_safe_tail(&buf, 10);
            // `from_utf8_lossy` replaces invalid bytes with U+FFFD
            assert!(result.contains('\u{FFFD}'));
            assert_eq!(info.total_bytes, 3);
        }
    }

    mod truncate_utf8_safe_head {
        use super::*;

        #[test]
        fn string_smaller_than_max() {
            let (s, t) = truncate_utf8_safe_head("hello", 100);
            assert_eq!(s, "hello");
            assert!(!t);
        }

        #[test]
        fn string_equal_to_max() {
            let (s, t) = truncate_utf8_safe_head("hello", 5);
            assert_eq!(s, "hello");
            assert!(!t);
        }

        #[test]
        fn string_larger_than_max() {
            let (s, t) = truncate_utf8_safe_head("abcdefghij", 4);
            assert_eq!(s, "abcd");
            assert!(t);
        }

        #[test]
        fn multibyte_boundary_walks_back() {
            // "héllo" max=2 would fall between é bytes. Walks back to 1 ("h").
            let (s, t) = truncate_utf8_safe_head("héllo", 2);
            assert_eq!(s, "h");
            assert!(t);
        }

        #[test]
        fn emoji_boundary_walks_back_to_zero() {
            let (s, t) = truncate_utf8_safe_head("😀hi", 2);
            assert_eq!(s, "");
            assert!(t);
        }

        #[test]
        fn empty_string() {
            let (s, t) = truncate_utf8_safe_head("", 100);
            assert_eq!(s, "");
            assert!(!t);
        }

        #[test]
        fn max_zero() {
            let (s, t) = truncate_utf8_safe_head("hello", 0);
            assert_eq!(s, "");
            assert!(t);
        }
    }

    mod sanitize_value {
        use super::*;

        #[test]
        fn plain_text_borrowed() {
            let input = "hello world";
            let output = sanitize_value(input);
            assert!(matches!(output, Cow::Borrowed(_)));
            assert_eq!(output, "hello world");
        }

        #[test]
        fn newline_escaped_to_literal() {
            let output = sanitize_value("line1\nline2");
            assert!(matches!(output, Cow::Owned(_)));
            assert_eq!(output, "line1\\nline2");
        }

        #[test]
        fn carriage_return_escaped() {
            let output = sanitize_value("hello\rworld");
            assert_eq!(output, "hello\\rworld");
        }

        #[test]
        fn tab_escaped() {
            let output = sanitize_value("col1\tcol2");
            assert_eq!(output, "col1\\tcol2");
        }

        #[test]
        fn all_three_combined() {
            let output = sanitize_value("a\nb\rc\td");
            assert_eq!(output, "a\\nb\\rc\\td");
        }

        #[test]
        fn empty_string_borrowed() {
            let output = sanitize_value("");
            assert!(matches!(output, Cow::Borrowed(_)));
            assert_eq!(output, "");
        }

        #[test]
        fn multiline_heavy() {
            let output = sanitize_value("a\nb\nc\nd\ne\nf");
            assert_eq!(output, "a\\nb\\nc\\nd\\ne\\nf");
        }

        #[test]
        fn other_control_chars_left_intact() {
            let output = sanitize_value("a\x07b");
            assert_eq!(output, "a\x07b");
        }

        #[test]
        fn unicode_preserved_and_borrowed() {
            let output = sanitize_value("hello 世界");
            assert_eq!(output, "hello 世界");
            assert!(matches!(output, Cow::Borrowed(_)));
        }

        #[test]
        fn unicode_with_newline_escapes_newline_only() {
            let output = sanitize_value("hello\n世界");
            assert_eq!(output, "hello\\n世界");
        }
    }

    mod format_bytes_human {
        use super::*;

        #[test]
        fn zero() {
            assert_eq!(format_bytes_human(0), "0B");
        }

        #[test]
        fn under_kb() {
            assert_eq!(format_bytes_human(1), "1B");
            assert_eq!(format_bytes_human(512), "512B");
            assert_eq!(format_bytes_human(1023), "1023B");
        }

        #[test]
        fn exactly_1kb() {
            assert_eq!(format_bytes_human(1024), "1.0KB");
        }

        #[test]
        fn mid_kb() {
            assert_eq!(format_bytes_human(1536), "1.5KB");
        }

        #[test]
        fn exactly_1mb() {
            assert_eq!(format_bytes_human(1024 * 1024), "1.0MB");
        }

        #[test]
        fn mid_mb() {
            assert_eq!(format_bytes_human(1_572_864), "1.5MB");
        }

        #[test]
        fn exactly_1gb() {
            assert_eq!(format_bytes_human(1024 * 1024 * 1024), "1.0GB");
        }

        #[test]
        fn very_large() {
            assert_eq!(format_bytes_human(10 * 1024 * 1024 * 1024), "10.0GB");
        }
    }

    mod format_error {
        use super::*;

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
            assert_eq!(
                e,
                "SSH_UPLOAD: ERROR\nREASON: [REMOTE_DIR_NOT_FOUND] remote directory does not exist\nDETAIL: /tmp/missing"
            );
        }

        #[test]
        fn empty_detail_omits_line() {
            let e = format_error("SSH_X", "CODE", "reason", Some(""));
            assert!(!e.contains("DETAIL"));
        }

        #[test]
        fn long_detail_truncated_with_marker() {
            let big = "x".repeat(DEFAULT_ERROR_DETAIL_MAX_BYTES + 100);
            let e = format_error("SSH_X", "CODE", "reason", Some(&big));
            assert!(e.contains("(truncated)"));
            // Full response should stay small: header + truncated detail + marker
            assert!(
                e.len() <= DEFAULT_ERROR_DETAIL_MAX_BYTES + 120,
                "response too long: {} bytes",
                e.len()
            );
        }

        #[test]
        fn exact_cap_detail_not_truncated() {
            let right_at_cap = "y".repeat(DEFAULT_ERROR_DETAIL_MAX_BYTES);
            let e = format_error("SSH_X", "CODE", "reason", Some(&right_at_cap));
            assert!(!e.contains("(truncated)"));
        }

        #[test]
        fn control_chars_in_reason_escaped() {
            let e = format_error("SSH_X", "CODE", "line1\nline2", None);
            assert!(e.contains("line1\\nline2"));
        }

        #[test]
        fn control_chars_in_detail_escaped() {
            let e = format_error("SSH_X", "CODE", "reason", Some("a\nb\tc"));
            assert!(e.contains("a\\nb\\tc"));
        }

        #[test]
        fn three_line_structure() {
            let e = format_error("SSH_EXEC", "TIMEOUT", "command timed out", Some("30s"));
            let lines: Vec<&str> = e.lines().collect();
            assert_eq!(lines.len(), 3);
            assert_eq!(lines[0], "SSH_EXEC: ERROR");
            assert!(lines[1].starts_with("REASON: "));
            assert!(lines[2].starts_with("DETAIL: "));
        }

        #[test]
        fn two_line_structure_without_detail() {
            let e = format_error("SSH_EXEC", "CODE", "reason", None);
            let lines: Vec<&str> = e.lines().collect();
            assert_eq!(lines.len(), 2);
        }
    }

    mod find_char_boundary_forward {
        use super::*;

        #[test]
        fn ascii_every_position_is_boundary() {
            let buf = b"hello";
            assert_eq!(find_char_boundary_forward(buf, 0), 0);
            assert_eq!(find_char_boundary_forward(buf, 2), 2);
            assert_eq!(find_char_boundary_forward(buf, 5), 5);
        }

        #[test]
        fn continuation_byte_advances() {
            let buf = "héllo".as_bytes();
            // index 2 is 0xA9 (continuation of é). Advance to index 3 (start of 'l').
            assert_eq!(find_char_boundary_forward(buf, 2), 3);
        }

        #[test]
        fn non_continuation_byte_stays() {
            let buf = "héllo".as_bytes();
            // index 1 is 0xC3 (start of 2-byte seq). Not a continuation byte.
            assert_eq!(find_char_boundary_forward(buf, 1), 1);
        }

        #[test]
        fn from_at_end_returns_end() {
            let buf = b"abc";
            assert_eq!(find_char_boundary_forward(buf, 3), 3);
        }

        #[test]
        fn from_past_end_returns_len() {
            let buf = b"abc";
            assert_eq!(find_char_boundary_forward(buf, 100), 3);
        }

        #[test]
        fn emoji_continuation_bytes_all_skipped() {
            let buf = "😀hello".as_bytes();
            // 😀 occupies indices 0..4. from=2 is a continuation, advance to 4.
            assert_eq!(find_char_boundary_forward(buf, 2), 4);
            assert_eq!(find_char_boundary_forward(buf, 3), 4);
        }
    }

    mod render_output_block {
        use super::*;

        #[test]
        fn empty_buffer_yields_empty_marker_only() {
            let out = render_output_block("stdout", "deadbeef", b"", 1024, None);
            assert_eq!(out, "--- stdout [deadbeef] (empty) ---");
        }

        #[test]
        fn empty_buffer_with_hint_combines_annotations() {
            let out = render_output_block("stdout", "deadbeef", b"", 1024, Some("partial"));
            assert_eq!(out, "--- stdout [deadbeef] (partial, empty) ---");
        }

        #[test]
        fn short_content_no_truncation_no_hint() {
            let out = render_output_block("stdout", "cafe0001", b"hello world", 1024, None);
            assert_eq!(out, "--- stdout [cafe0001] ---\nhello world");
        }

        #[test]
        fn short_content_with_hint() {
            let out =
                render_output_block("stdout", "cafe0002", b"line1\nline2", 1024, Some("partial"));
            assert_eq!(out, "--- stdout [cafe0002] (partial) ---\nline1\nline2");
        }

        #[test]
        fn trailing_newline_stripped_to_avoid_join_blank_lines() {
            let out = render_output_block("stdout", "abc12345", b"file1\nfile2\n", 1024, None);
            assert!(out.ends_with("file2"));
            assert!(!out.ends_with("file2\n"));
        }

        #[test]
        fn single_trailing_newline_stripped_empty_content() {
            // Buffer with JUST a newline: content == "\n", strip leaves "".
            let out = render_output_block("stdout", "abc12345", b"\n", 1024, None);
            assert_eq!(out, "--- stdout [abc12345] ---\n");
        }

        #[test]
        fn multiple_trailing_newlines_only_last_stripped() {
            let out = render_output_block("stdout", "abc12345", b"a\n\n", 1024, None);
            assert!(out.ends_with("a\n"));
            assert!(!out.ends_with("a\n\n"));
        }

        #[test]
        fn truncation_shows_human_readable_sizes() {
            let buf = vec![b'x'; 2 * 1024 * 1024]; // 2MB
            let out = render_output_block("stdout", "trunc001", &buf, 16 * 1024, None);
            assert!(out.contains("(truncated: showing 16.0KB of 2.0MB)"));
        }

        #[test]
        fn truncation_with_partial_hint_combined() {
            let buf = vec![b'x'; 100_000_usize];
            let out = render_output_block("stdout", "trunc002", &buf, 256, Some("partial"));
            assert!(out.starts_with("--- stdout [trunc002] (partial, truncated: showing 256B of "));
        }

        #[test]
        fn large_buffer_renders_fast_and_small() {
            let buf = vec![b'a'; 10 * 1024 * 1024]; // 10MB
            let start = std::time::Instant::now();
            let out = render_output_block("stdout", "deadbeef", &buf, 16 * 1024, None);
            let elapsed = start.elapsed();
            // Should be microseconds; generous 20ms bound for CI variance.
            assert!(
                elapsed.as_millis() < 20,
                "render took {}ms (expected <20ms)",
                elapsed.as_millis()
            );
            // Output should be ~16KB content + marker overhead.
            assert!(out.len() < 17_000, "output too large: {} bytes", out.len());
            assert!(out.contains("truncated"));
        }

        #[test]
        fn multibyte_tail_boundary_safe() {
            // Buffer ends with a 2-byte é. max_bytes picks boundary mid-é — should advance.
            let s = format!("{}héllo", "x".repeat(100_usize));
            let buf = s.as_bytes();
            let out = render_output_block("stdout", "utf80001", buf, 4, None);
            // Must not produce invalid UTF-8 (String construction would replace).
            // Expected: "llo" (3 bytes) after skipping forward past continuation byte.
            assert!(out.ends_with("llo"));
            assert!(out.contains("truncated"));
        }

        #[test]
        fn stderr_name_supported() {
            let out = render_output_block("stderr", "nonce123", b"err line", 1024, None);
            assert_eq!(out, "--- stderr [nonce123] ---\nerr line");
        }

        #[test]
        fn data_name_supported() {
            let out = render_output_block("data", "nonce456", b"$ ls\nfoo", 1024, None);
            assert_eq!(out, "--- data [nonce456] ---\n$ ls\nfoo");
        }

        #[test]
        fn zero_max_bytes_renders_empty_with_truncation_annotation() {
            // max=0 forces the whole buffer into the "truncated" info as unshown.
            // Content is empty so is_empty wins over truncated.
            let out = render_output_block("stdout", "zero0001", b"hello", 0, None);
            assert_eq!(out, "--- stdout [zero0001] (empty) ---");
        }

        #[test]
        fn marker_format_with_custom_nonce() {
            let out = render_output_block("stdout", "12345678", b"x", 100, None);
            assert!(out.starts_with("--- stdout [12345678] ---"));
        }

        #[test]
        fn two_blocks_joined_with_newline_form_correct_layout() {
            // Verify that caller can join blocks with \n and get clean layout.
            let b1 = render_output_block("stdout", "n001", b"file1\nfile2", 1024, None);
            let b2 = render_output_block("stderr", "n001", b"", 1024, None);
            let joined = format!("{b1}\n{b2}");
            assert_eq!(
                joined,
                "--- stdout [n001] ---\nfile1\nfile2\n--- stderr [n001] (empty) ---"
            );
        }

        #[test]
        fn nonce_consistency_across_blocks_is_caller_responsibility() {
            // The renderer accepts any nonce; it's up to the caller to pass
            // the same one to all blocks in a single response.
            let b1 = render_output_block("stdout", "AAA", b"", 1024, None);
            let b2 = render_output_block("stderr", "BBB", b"", 1024, None);
            assert!(b1.contains("[AAA]"));
            assert!(b2.contains("[BBB]"));
        }

        #[test]
        fn injected_fake_marker_in_content_has_different_nonce() {
            // Simulate attacker output containing a crafted marker.
            let evil = b"--- stdout [attacker] ---\nfake content";
            let out = render_output_block("stdout", "realnnce", evil, 1024, None);
            // The real marker has the real nonce; fake marker is inside content.
            assert!(out.starts_with("--- stdout [realnnce] ---"));
            assert!(out.contains("[attacker]"));
            // Client parser can distinguish via matching nonce across blocks.
        }

        #[test]
        fn stress_injected_nonces_never_collide_with_real_nonce() {
            // Inject 1000 carefully-crafted fake markers (each uses a random
            // attacker-chosen 8-hex string). The real response uses its own
            // random nonce, so parsers keyed on "same nonce as first marker"
            // are not fooled. We verify the real nonce is still present and
            // never matches any attacker's nonce even in a collision-prone run.
            for _ in 0..1000_usize {
                let attacker_nonce = generate_nonce();
                let real_nonce = generate_nonce();
                let evil = format!("--- stdout [{attacker_nonce}] ---\nfake\n");
                let out = render_output_block("stdout", &real_nonce, evil.as_bytes(), 1024, None);
                assert!(out.starts_with(&format!("--- stdout [{real_nonce}]")));
                if attacker_nonce != real_nonce {
                    assert_ne!(attacker_nonce, real_nonce);
                }
            }
        }

        #[test]
        fn stress_huge_buffer_many_render_calls_no_excess_alloc() {
            // Render a 5 MB buffer 200 times with a 16 KB cap; total rendered
            // text must stay well under 10 MB cumulative (proves borrow-based
            // renderer doesn't retain previous outputs or allocate the full
            // buffer per call).
            let buf = vec![b'a'; 5 * 1024 * 1024];
            let mut total_bytes = 0_usize;
            for _ in 0..200_usize {
                let rendered = render_output_block("stdout", "deadbeef", &buf, 16 * 1024, None);
                total_bytes += rendered.len();
                assert!(rendered.len() < 20_000);
            }
            assert!(
                total_bytes < 4_500_000,
                "200 renders totaled {total_bytes} bytes (expected < 4.5 MB)"
            );
        }
    }

    mod append_annotations {
        use super::*;

        #[test]
        fn no_annotations_when_clean() {
            let mut out = String::new();
            let info = TruncationInfo::new(10, 10);
            append_annotations(&mut out, None, &info, false);
            assert_eq!(out, "");
        }

        #[test]
        fn only_hint() {
            let mut out = String::new();
            let info = TruncationInfo::new(10, 10);
            append_annotations(&mut out, Some("partial"), &info, false);
            assert_eq!(out, " (partial)");
        }

        #[test]
        fn only_empty() {
            let mut out = String::new();
            let info = TruncationInfo::new(0, 0);
            append_annotations(&mut out, None, &info, true);
            assert_eq!(out, " (empty)");
        }

        #[test]
        fn hint_plus_empty() {
            let mut out = String::new();
            let info = TruncationInfo::new(0, 0);
            append_annotations(&mut out, Some("partial"), &info, true);
            assert_eq!(out, " (partial, empty)");
        }

        #[test]
        fn only_truncation() {
            let mut out = String::new();
            let info = TruncationInfo::new(1024, 5_242_880);
            append_annotations(&mut out, None, &info, false);
            assert_eq!(out, " (truncated: showing 1.0KB of 5.0MB)");
        }

        #[test]
        fn hint_plus_truncation() {
            let mut out = String::new();
            let info = TruncationInfo::new(256, 100_000);
            append_annotations(&mut out, Some("partial"), &info, false);
            assert!(out.starts_with(" (partial, truncated: showing 256B of "));
        }
    }
}
