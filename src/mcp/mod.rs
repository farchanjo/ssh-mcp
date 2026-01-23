//! MCP SSH module providing SSH connection and command execution tools.
//!
//! # Architecture
//!
//! This module follows SOLID principles with trait-based abstractions:
//!
//! ## Core Modules
//!
//! - [`types`]: Serializable response types for MCP tools
//! - [`config`]: Configuration resolution with environment variable support
//! - [`error`]: Error classification for retry logic
//! - [`session`]: `SshClientHandler` for russh callbacks
//! - [`client`]: SSH connection and command execution logic
//! - [`async_command`]: Async command tracking and state management
//! - [`forward`]: Port forwarding implementation (feature-gated)
//! - [`commands`]: `McpSSHCommands` MCP tool implementations
//!
//! ## SOLID Architecture Modules
//!
//! - [`storage`]: Storage traits (`SessionStorage`, `CommandStorage`) with DashMap implementations
//! - [`auth`]: Authentication strategies (`PasswordAuth`, `KeyAuth`, `AgentAuth`, `AuthChain`)
//! - [`message`]: Message builders for LLM-friendly responses

pub(crate) mod async_command;
pub mod auth;
pub(crate) mod client;
pub mod commands;
pub(crate) mod config;
pub(crate) mod error;
#[cfg(feature = "port_forward")]
pub(crate) mod forward;
pub mod message;
pub mod schema;
pub mod session;
pub mod storage;
pub mod types;

pub use commands::McpSSHCommands;
