//! Shared markdown helpers for the v4 inbound MCP layer.
//!
//! Mirrors the v3 `src/mcp/message/helpers.rs` surface: nonce generation,
//! UTF-8 safe truncation, value sanitization, human-readable byte
//! formatting, output-block rendering, and the standardized error format.
//!
//! The H17 migration ports the v3 implementation verbatim — every byte
//! stays bit-compatible so existing MCP clients keep parsing the same
//! payloads while the use case layer takes over the data fan-out.

pub mod error;
pub mod nonce;
pub mod output;
