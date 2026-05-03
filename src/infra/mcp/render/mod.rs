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
pub mod sftp;
pub mod shell;
