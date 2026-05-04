//! Application layer (hexagonal use cases).
//!
//! Use cases orchestrate domain entities through the ports defined in
//! [`crate::ports`]. Inbound DTOs and outbound result enums live alongside
//! each use case so the rmcp tool wrapper (etapa H16) and any other inbound
//! adapter (e.g. a future REST gateway) can drive the use case without
//! reaching into the domain layer.
//!
//! Use cases land incrementally across H10-H15:
//!
//! | Etapa | Use case(s) |
//! |-------|-------------|
//! | H10   | [`connect_session`] — canary establishing the pattern |
//! | H11   | [`disconnect_session`], [`list_sessions`], [`disconnect_agent`] |
//! | H12   | [`execute_command`], [`get_command_output`], [`list_commands`], [`cancel_command`] |
//! | H13   | [`open_shell`], [`write_shell`], [`send_key`], [`read_shell`], [`wait_for_pattern`], [`close_shell`] |
//! | H14   | [`upload_file`], [`download_file`], [`get_transfer_progress`] |
//! | H15   | [`forward_port`] (feature-gated), [`list_resources`], [`read_resource`], [`subscribe_resource`], [`unsubscribe_resource`], [`peer_gc`] |
//!
//! All use cases follow the same shape:
//!
//! - A request DTO carrying validated, port-friendly inputs (no rmcp /
//!   russh / tokio types).
//! - An outcome enum describing every observable result variant the inbound
//!   adapter must render.
//! - A struct with `Arc<Adapter>` fields per port plus a single
//!   `pub async fn execute(&self, req: Request) -> Result<Outcome, DomainError>`
//!   entry point.
//! - Constructed via `new(...)` at the composition root; never instantiated
//!   inline.

pub mod connect_session;
pub mod disconnect_agent;
pub mod disconnect_session;
pub mod list_sessions;

pub mod cancel_command;
pub mod execute_command;
pub mod get_command_output;
pub mod list_commands;

pub mod close_shell;
pub mod open_shell;
pub mod read_shell;
pub mod send_key;
pub mod wait_for_pattern;
pub mod write_shell;

pub mod download_file;
pub mod get_transfer_progress;
pub mod upload_file;

#[cfg(feature = "port_forward")]
pub mod forward_port;
pub mod list_resources;
pub mod peer_gc;
pub mod read_resource;
pub mod subscribe_resource;
pub mod subscription_admin;
pub mod unsubscribe_resource;
