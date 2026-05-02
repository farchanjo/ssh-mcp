//! MCP SSH module providing SSH connection and command execution tools.
//!
//! # v3.0.0 Architecture
//!
//! Migrated from `poem-mcpserver` 0.3.1 to `rmcp` 1.6 (official Anthropic Rust SDK)
//! to gain native `resources/subscribe` + `notifications/resources/updated` support.
//!
//! ## Core Modules
//!
//! - [`types`]: Internal data carriers (`SessionInfo`, `AsyncCommandInfo`, `ShellInfo`).
//! - [`config`]: Parameter -> Env -> Default resolution with byte-size parser.
//! - [`error`]: Retryable vs non-retryable error classification.
//! - [`session`]: `SshClientHandler` for russh callbacks.
//! - [`client`]: SSH connection, command execution, PTY channels.
//! - [`async_command`]: Async command tracking and state.
//! - [`shell`]: Interactive PTY shell session state.
//! - [`forward`]: Port forwarding (feature-gated).
//! - [`server`]: `McpSshServer` implementing `rmcp::ServerHandler` (the v3 entry point).
//!
//! ## Subscribe-First Layer (v3.0.0)
//!
//! Realtime resource streams via MCP `resources/subscribe`:
//! - `shell://<id>/output`     — PTY output buffer
//! - `command://<id>/output`   — async command stdout/stderr
//! - `transfer://<id>/progress` — SFTP transfer progress events
//! - `session://<id>/health`    — session health change events
//! - `forward://<id>/events`    — port forward accept/close events
//!
//! ## SOLID Architecture
//!
//! - [`storage`]: Lock-free `DashMap`-backed storage with secondary indices.
//! - [`auth`]: `AuthChain` strategy (Password / Key / Agent).
//! - [`message`]: Markdown response builders for LLM-friendly output.

pub(crate) mod async_command;
pub mod auth;
pub(crate) mod client;
pub(crate) mod config;
pub(crate) mod error;
#[cfg(feature = "port_forward")]
pub(crate) mod forward;
pub mod keys;
pub mod message;
pub mod schema;
pub mod server;
pub mod session;
pub(crate) mod sftp;
pub(crate) mod shell;
pub mod storage;
pub mod subscription;
pub mod tools;
pub(crate) mod transfer;
pub mod types;
