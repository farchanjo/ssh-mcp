//! v4 inbound MCP layer — wires the rmcp `ServerHandler` to the use case
//! container produced by [`crate::composition::UseCases`].
//!
//! ## Module shape
//!
//! - [`server`] — the [`server::McpSshServer`] struct (the rmcp
//!   `ServerHandler` host).
//! - [`tool_router`] — the `#[tool_router]` impl populating the 36 tool
//!   fns (35 without `port_forward`) + the matching
//!   `#[tool_handler] impl ServerHandler` block.
//! - [`resource_handlers`] — adapters between rmcp `resources/*` payloads
//!   and the corresponding `crate::application::*_resource` use cases.
//! - [`peer_handle`] — re-exports the
//!   [`crate::adapters::notifier::rmcp_peer::RmcpPeerHandle`] +
//!   [`crate::adapters::notifier::rmcp_peer::PeerTable`] used by the
//!   subscribe/read paths.
//! - [`args`] — H16 stub argument structs (one per tool); H17 fills v3
//!   parity.
//!
//! ## H16 scope
//!
//! - Compiles end-to-end with both the production binary
//!   ([`crate::composition::prod`]) and the rmcp transport.
//! - Each tool fn returns `TODO H17: render <tool>` so the runtime is
//!   reachable; H17 fills the v3 markdown body.
//! - Each resource handler returns minimal placeholder contents; H17
//!   wires the v3 `_meta` cursor envelope.

pub mod args;
pub mod error_detail;
pub mod helpers;
pub mod idempotency;
pub mod leak_warn_bridge;
pub mod peer_handle;
pub mod progress;
pub mod prompts;
pub mod render;
pub mod resource_handlers;
pub mod resource_templates;
pub mod results;
pub mod server;
pub mod suggestions;
pub mod tool_router;
