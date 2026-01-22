//! MCP SSH module providing SSH connection and command execution tools.
//!
//! This module is organized into the following submodules:
//!
//! - `types`: Serializable response types for MCP tools
//! - `config`: Configuration resolution with environment variable support
//! - `error`: Error classification for retry logic
//! - `session`: Session storage and management
//! - `client`: SSH connection and authentication logic
//! - `forward`: Port forwarding implementation (feature-gated)
//! - `commands`: MCP tool implementations

pub(crate) mod async_command;
pub(crate) mod client;
pub mod commands;
pub(crate) mod config;
pub(crate) mod error;
#[cfg(feature = "port_forward")]
pub(crate) mod forward;
pub mod session;
pub mod types;

pub use commands::McpSSHCommands;
