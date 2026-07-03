//! Markdown render functions for the v4 inbound MCP layer.
//!
//! Each tool render fn consumes the matching `*Outcome` enum from
//! `crate::application::*` and produces the v3-compatible
//! block-style markdown body the rmcp wrapper sends back as a
//! `CallToolResult::success` / `error`.
//!
//! The format is locked: `TOOL_NAME: STATUS` first line, `KEY: value`
//! lines below, optional `--- name [<nonce>] ---\n<bytes>` output blocks.
//! Every byte stays bit-compatible with the v3 baseline so downstream
//! MCP clients keep parsing the same payloads after the v4 swap.

pub mod connection;
pub mod execute;
#[cfg(feature = "port_forward")]
pub mod forward;
pub mod rsync;
pub mod serial;
pub mod sftp;
pub mod shell;
pub mod subscription;

/// Appends a `\nNEXT: <hint>` line to the wire response body.
pub(crate) fn append_next_line(out: &mut String, hint: &str) {
    out.push_str("\nNEXT: ");
    out.push_str(hint);
}

/// Appends a `\nHINT: <hint>` line to the wire response body.
pub(crate) fn append_subscribe_hint(out: &mut String, hint: &str) {
    out.push_str("\nHINT: ");
    out.push_str(hint);
}
