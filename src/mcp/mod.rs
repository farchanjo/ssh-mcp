//! Foundational SSH/PTY/SFTP runtime carriers (v3 leftovers consumed by v4 adapters).
//!
//! After the H17.5a hard-delete, this module no longer hosts the v3 MCP server,
//! tools, resources, storage, message builders, port forwarder, or runtime.
//! Those concerns now live under:
//!
//! - `infra::mcp::server` / `infra::mcp::tool_router` / `infra::mcp::resource_handlers`
//!   — the rmcp `ServerHandler` and tool/resource wiring.
//! - `application::*` — use cases.
//! - `adapters::repo::dashmap::*` — `DashMap`-backed repositories.
//! - `adapters::repo::dashmap::forward` + `application::forward_port` — port forwarding.
//! - `infra::mcp::render` / `infra::mcp::helpers` — markdown rendering.
//! - `domain::keys` — semantic keystroke encoder.
//!
//! What remains here is foundational state used by the russh / sftp adapters
//! and by composition::prod (peer GC + config resolvers). H17.6 will absorb
//! these into the hexagonal layout and retire the `mcp::` namespace entirely.
//!
//! Surviving modules:
//!
//! - [`async_command`] — `RunningCommand` + `OutputBuffer` consumed by the russh adapter.
//! - [`auth`] — `AuthChain` strategies (still uses `async-trait`).
//! - [`client`] — low-level SSH connect / exec / PTY helpers reused by adapters.
//! - [`config`] — env-var resolvers; `adapters::config::env` delegates here.
//! - [`error`] — retry classification consumed by `mcp::client`.
//! - [`session`] — `SshClientHandler` russh callback handler.
//! - [`sftp`] — streaming SFTP transfer state used by the sftp adapter.
//! - [`shell`] — `RunningShell` + `RingBuffer` consumed by the russh adapter.
//! - [`subscription`] — `SUBSCRIPTION_REGISTRY` + peer GC task spawned by composition.
//! - [`transfer`] — `RunningTransfer` lock-free state.
//! - [`types`] — shared payload structs (`SessionInfo`, `AsyncCommandInfo`, `ShellInfo`, …).

pub(crate) mod async_command;
pub mod auth;
pub(crate) mod client;
pub mod config;
pub(crate) mod error;
pub mod session;
pub(crate) mod sftp;
pub(crate) mod shell;
pub mod subscription;
pub(crate) mod transfer;
pub mod types;
