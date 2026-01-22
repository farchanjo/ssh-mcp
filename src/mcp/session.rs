//! SSH session management and global storage.
//!
//! This module provides the session storage infrastructure for managing active SSH connections.
//! It uses a global `Lazy<Mutex<HashMap>>` pattern to store sessions across async tasks,
//! allowing multiple MCP tools to share and access established SSH connections.
//!
//! # Architecture
//!
//! - `SshClientHandler`: A russh client handler that accepts all host keys (similar to
//!   `StrictHostKeyChecking=no` in OpenSSH). In production environments, this should be
//!   extended to verify against known_hosts.
//!
//! - `StoredSession`: Combines session metadata (`SessionInfo`) with the actual russh
//!   handle wrapped in `Arc<>` for safe sharing across async tasks.
//!
//! - `SSH_SESSIONS`: Global static storage for all active sessions, keyed by UUID.
//!
//! # Thread Safety
//!
//! The `client::Handle<SshClientHandler>` is wrapped in `Arc<>` because it's not
//! `Clone`, and we need to share it across multiple async operations (execute, forward, etc.).

use std::sync::Arc;

use dashmap::DashMap;
use once_cell::sync::Lazy;
use russh::{client, keys};

use super::types::SessionInfo;

/// Client handler for russh that accepts all host keys.
///
/// This implementation accepts all server public keys without verification,
/// similar to `StrictHostKeyChecking=no` in OpenSSH configuration.
///
/// # Security Note
///
/// In production environments, you should implement proper host key verification
/// against a known_hosts file to prevent man-in-the-middle attacks.
pub struct SshClientHandler;

impl client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Accept all host keys (similar to StrictHostKeyChecking=no)
        // In production, you'd want to verify against known_hosts
        Ok(true)
    }
}

/// Stored session data combining metadata with the actual session handle.
///
/// The russh `Handle` is not `Clone`, so we wrap it in `Arc<>` to share
/// across multiple async tasks that need to execute commands or manage the session.
pub struct StoredSession {
    /// Session metadata including connection info and timing details
    pub info: SessionInfo,
    /// The actual russh client handle for executing commands
    pub handle: Arc<client::Handle<SshClientHandler>>,
}

/// Global storage for active SSH sessions with metadata.
///
/// Sessions are keyed by a UUID string generated at connection time.
/// Uses `DashMap` for lock-free concurrent access, allowing multiple
/// readers and writers without blocking the entire map.
///
/// # Usage
///
/// ```ignore
/// // Store a new session
/// SSH_SESSIONS.insert(session_id, stored_session);
///
/// // Retrieve a session
/// if let Some(session) = SSH_SESSIONS.get(&session_id) {
///     // Use session.handle or session.info
/// }
///
/// // Remove a session
/// if let Some((_, session)) = SSH_SESSIONS.remove(&session_id) {
///     // Cleanup session
/// }
/// ```
pub static SSH_SESSIONS: Lazy<DashMap<String, StoredSession>> = Lazy::new(DashMap::new);
