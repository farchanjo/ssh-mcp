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
//! | H11   | disconnect_session, list_sessions, disconnect_agent |
//! | H12   | execute, get_command_output, list_commands, cancel_command |
//! | H13   | shell_open, shell_write, shell_read, shell_close |
//! | H14   | upload, download, get_transfer_progress |
//! | H15   | forward (feature-gated) |
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
