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

use std::collections::HashSet;
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

/// Secondary index mapping agent IDs to their session IDs.
///
/// This enables O(1) lookup of all sessions belonging to an agent,
/// which is essential for `ssh_disconnect_agent` and filtered `ssh_list_sessions`.
///
/// # Thread Safety
///
/// Uses `DashMap` with `HashSet` values for lock-free concurrent access.
/// Operations on this index must be kept in sync with `SSH_SESSIONS`.
pub static SESSIONS_BY_AGENT: Lazy<DashMap<String, HashSet<String>>> = Lazy::new(DashMap::new);

/// Register a session under an agent ID in the secondary index.
///
/// Call this when creating a new session with an `agent_id`.
pub fn register_session_agent(agent_id: &str, session_id: &str) {
    SESSIONS_BY_AGENT
        .entry(agent_id.to_string())
        .or_default()
        .insert(session_id.to_string());
}

/// Unregister a session from an agent ID in the secondary index.
///
/// Call this when removing a session that has an `agent_id`.
pub fn unregister_session_agent(agent_id: &str, session_id: &str) {
    if let Some(mut sessions) = SESSIONS_BY_AGENT.get_mut(agent_id) {
        sessions.remove(session_id);
        // Clean up empty entries
        if sessions.is_empty() {
            drop(sessions);
            SESSIONS_BY_AGENT.remove(agent_id);
        }
    }
}

/// Get all session IDs for a specific agent.
///
/// Returns an empty vector if the agent has no sessions.
pub fn get_agent_session_ids(agent_id: &str) -> Vec<String> {
    SESSIONS_BY_AGENT
        .get(agent_id)
        .map(|sessions| sessions.iter().cloned().collect())
        .unwrap_or_default()
}

/// Remove all session IDs for a specific agent and return them.
///
/// This is used by `ssh_disconnect_agent` to get and clear the index atomically.
pub fn remove_agent_sessions(agent_id: &str) -> Vec<String> {
    SESSIONS_BY_AGENT
        .remove(agent_id)
        .map(|(_, sessions)| sessions.into_iter().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_get_agent_sessions() {
        let agent_id = "test-agent-register";
        let session_id_1 = "session-1";
        let session_id_2 = "session-2";

        register_session_agent(agent_id, session_id_1);
        register_session_agent(agent_id, session_id_2);

        let sessions = get_agent_session_ids(agent_id);
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&session_id_1.to_string()));
        assert!(sessions.contains(&session_id_2.to_string()));

        // Cleanup
        SESSIONS_BY_AGENT.remove(agent_id);
    }

    #[test]
    fn test_unregister_session_agent() {
        let agent_id = "test-agent-unregister";
        let session_id_1 = "session-a";
        let session_id_2 = "session-b";

        register_session_agent(agent_id, session_id_1);
        register_session_agent(agent_id, session_id_2);

        unregister_session_agent(agent_id, session_id_1);

        let sessions = get_agent_session_ids(agent_id);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains(&session_id_2.to_string()));

        // Unregister last session should remove the agent entry
        unregister_session_agent(agent_id, session_id_2);
        assert!(!SESSIONS_BY_AGENT.contains_key(agent_id));
    }

    #[test]
    fn test_get_agent_session_ids_empty() {
        let sessions = get_agent_session_ids("nonexistent-agent");
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_remove_agent_sessions() {
        let agent_id = "test-agent-remove";
        let session_id_1 = "session-x";
        let session_id_2 = "session-y";

        register_session_agent(agent_id, session_id_1);
        register_session_agent(agent_id, session_id_2);

        let removed = remove_agent_sessions(agent_id);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&session_id_1.to_string()));
        assert!(removed.contains(&session_id_2.to_string()));

        // Agent entry should be gone
        assert!(!SESSIONS_BY_AGENT.contains_key(agent_id));
    }

    #[test]
    fn test_remove_agent_sessions_empty() {
        let removed = remove_agent_sessions("nonexistent-agent-remove");
        assert!(removed.is_empty());
    }
}
