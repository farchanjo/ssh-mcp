//! Argument types for the v4 inbound MCP layer.
//!
//! Each [`serde::Deserialize`] + [`schemars::JsonSchema`] struct is the
//! inbound DTO the `#[tool_router]` macro deserialises from the MCP
//! JSON-RPC payload. H17 fills v3 parity (every optional tuning knob,
//! env-var fallbacks, validation tables, etc.).
//!
//! ## Module layout
//!
//! Args are split per tool domain to keep the file sizes manageable:
//!
//! - [`connection`]: `ssh_connect`, `ssh_disconnect`, `ssh_list_sessions`,
//!   `ssh_disconnect_agent` and the [`connection::ReusePolicy`] enum.
//! - [`execute`]: `ssh_execute`, `ssh_get_command_output`,
//!   `ssh_list_commands`, `ssh_cancel_command` and the
//!   [`execute::CommandStatus`] enum.
//! - [`shell`]: `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_send_key`,
//!   `ssh_shell_read`, `ssh_shell_wait_for`, `ssh_shell_close`.
//! - [`sftp`]: `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress`.
//! - [`forward`]: `ssh_forward` (feature-gated `port_forward`).
//!
//! `tool_router.rs` references each type through its full submodule
//! path (`super::args::connection::SshConnectArgs`) — no `pub use`
//! shortcuts so the crate-wide `clippy::pub_use` ban stays clean.

pub mod connection;
pub mod execute;
#[cfg(feature = "port_forward")]
pub mod forward;
pub mod serial;
pub mod sftp;
pub mod shell;
pub mod subscription;
