//! DashMap-based session storage implementation.
//!
//! Provides lock-free concurrent access to SSH sessions using `DashMap`.
//! Includes a secondary index for O(1) agent-to-sessions lookups.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use once_cell::sync::Lazy;
use russh::client;

use crate::mcp::session::SshClientHandler;
use crate::mcp::types::SessionInfo;

use super::traits::{SessionRef, SessionStorage};

/// Stored session data combining metadata with the actual session handle.
pub struct StoredSession {
    pub info: SessionInfo,
    pub handle: Arc<client::Handle<SshClientHandler>>,
}

/// DashMap-based implementation of `SessionStorage`.
///
/// Uses two `DashMap` instances:
/// - Primary storage: session_id -> StoredSession
/// - Secondary index: agent_id -> HashSet<session_id> for O(1) agent lookups
pub struct DashMapSessionStorage {
    sessions: DashMap<String, StoredSession>,
    sessions_by_agent: DashMap<String, HashSet<String>>,
}

impl DashMapSessionStorage {
    /// Create a new session storage instance.
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            sessions_by_agent: DashMap::new(),
        }
    }
}

impl Default for DashMapSessionStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStorage for DashMapSessionStorage {
    fn insert(
        &self,
        session_id: String,
        info: SessionInfo,
        handle: Arc<client::Handle<SshClientHandler>>,
    ) {
        self.sessions
            .insert(session_id, StoredSession { info, handle });
    }

    fn get(&self, session_id: &str) -> Option<SessionRef> {
        self.sessions.get(session_id).map(|entry| SessionRef {
            info: entry.info.clone(),
            handle: entry.handle.clone(),
        })
    }

    fn remove(&self, session_id: &str) -> Option<SessionRef> {
        self.sessions
            .remove(session_id)
            .map(|(_, stored)| SessionRef {
                info: stored.info,
                handle: stored.handle,
            })
    }

    fn list(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    fn contains(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    fn session_ids(&self) -> Vec<String> {
        self.sessions.iter().map(|e| e.key().clone()).collect()
    }

    fn update_health(&self, session_id: &str, last_check: String, healthy: bool) {
        if let Some(mut stored) = self.sessions.get_mut(session_id) {
            stored.info.last_health_check = Some(last_check);
            stored.info.healthy = Some(healthy);
        }
    }

    fn register_agent(&self, agent_id: &str, session_id: &str) {
        self.sessions_by_agent
            .entry(agent_id.to_string())
            .or_default()
            .insert(session_id.to_string());
    }

    fn unregister_agent(&self, agent_id: &str, session_id: &str) {
        if let Some(mut sessions) = self.sessions_by_agent.get_mut(agent_id) {
            sessions.remove(session_id);
            if sessions.is_empty() {
                drop(sessions);
                self.sessions_by_agent.remove(agent_id);
            }
        }
    }

    fn get_agent_sessions(&self, agent_id: &str) -> Vec<String> {
        self.sessions_by_agent
            .get(agent_id)
            .map(|sessions| sessions.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn remove_agent_sessions(&self, agent_id: &str) -> Vec<String> {
        self.sessions_by_agent
            .remove(agent_id)
            .map(|(_, sessions)| sessions.into_iter().collect())
            .unwrap_or_default()
    }
}

/// Global session storage instance.
pub static SESSION_STORAGE: Lazy<DashMapSessionStorage> = Lazy::new(DashMapSessionStorage::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_registration() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("test-agent-{}", uuid::Uuid::new_v4());
        let session_id_1 = format!("session-1-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-2-{}", uuid::Uuid::new_v4());

        storage.register_agent(&agent_id, &session_id_1);
        storage.register_agent(&agent_id, &session_id_2);

        let sessions = storage.get_agent_sessions(&agent_id);
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&session_id_1));
        assert!(sessions.contains(&session_id_2));
    }

    #[test]
    fn test_agent_unregistration() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("test-agent-unreg-{}", uuid::Uuid::new_v4());
        let session_id_1 = format!("session-a-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-b-{}", uuid::Uuid::new_v4());

        storage.register_agent(&agent_id, &session_id_1);
        storage.register_agent(&agent_id, &session_id_2);
        storage.unregister_agent(&agent_id, &session_id_1);

        let sessions = storage.get_agent_sessions(&agent_id);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains(&session_id_2));

        // Unregister last session removes agent entry
        storage.unregister_agent(&agent_id, &session_id_2);
        assert!(storage.get_agent_sessions(&agent_id).is_empty());
    }

    #[test]
    fn test_remove_agent_sessions() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("test-agent-remove-{}", uuid::Uuid::new_v4());
        let session_id_1 = format!("session-x-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-y-{}", uuid::Uuid::new_v4());

        storage.register_agent(&agent_id, &session_id_1);
        storage.register_agent(&agent_id, &session_id_2);

        let removed = storage.remove_agent_sessions(&agent_id);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&session_id_1));
        assert!(removed.contains(&session_id_2));

        // Agent entry should be gone
        assert!(storage.get_agent_sessions(&agent_id).is_empty());
    }

    #[test]
    fn test_get_agent_sessions_empty() {
        let storage = DashMapSessionStorage::new();
        let unique_agent = format!("nonexistent-agent-{}", uuid::Uuid::new_v4());
        let sessions = storage.get_agent_sessions(&unique_agent);
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_contains() {
        let storage = DashMapSessionStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(!storage.contains(&unique_id));
    }

    #[test]
    fn test_list_empty() {
        let storage = DashMapSessionStorage::new();
        assert!(storage.list().is_empty());
    }

    #[test]
    fn test_session_ids_empty() {
        let storage = DashMapSessionStorage::new();
        assert!(storage.session_ids().is_empty());
    }

    #[test]
    fn test_default_implementation() {
        let storage = DashMapSessionStorage::default();
        assert!(storage.list().is_empty());
        assert!(storage.session_ids().is_empty());
    }

    #[test]
    fn test_duplicate_agent_registration() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("test-agent-dup-{}", uuid::Uuid::new_v4());
        let session_id = format!("session-dup-{}", uuid::Uuid::new_v4());

        // Register same session twice under same agent
        storage.register_agent(&agent_id, &session_id);
        storage.register_agent(&agent_id, &session_id);

        // Should still only have one entry (HashSet behavior)
        let sessions = storage.get_agent_sessions(&agent_id);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn test_unregister_nonexistent_agent() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("nonexistent-agent-{}", uuid::Uuid::new_v4());
        let session_id = format!("session-{}", uuid::Uuid::new_v4());

        // Should not panic when unregistering from nonexistent agent
        storage.unregister_agent(&agent_id, &session_id);
        assert!(storage.get_agent_sessions(&agent_id).is_empty());
    }

    #[test]
    fn test_unregister_nonexistent_session_from_agent() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("test-agent-{}", uuid::Uuid::new_v4());
        let session_id_1 = format!("session-1-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-2-{}", uuid::Uuid::new_v4());

        storage.register_agent(&agent_id, &session_id_1);

        // Unregister a session that was never registered
        storage.unregister_agent(&agent_id, &session_id_2);

        // Original session should still be there
        let sessions = storage.get_agent_sessions(&agent_id);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains(&session_id_1));
    }

    #[test]
    fn test_remove_agent_sessions_nonexistent() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        // Should return empty vec, not panic
        let removed = storage.remove_agent_sessions(&agent_id);
        assert!(removed.is_empty());
    }

    #[test]
    fn test_multiple_agents_same_session() {
        let storage = DashMapSessionStorage::new();
        let agent_id_1 = format!("agent-1-{}", uuid::Uuid::new_v4());
        let agent_id_2 = format!("agent-2-{}", uuid::Uuid::new_v4());
        let session_id = format!("shared-session-{}", uuid::Uuid::new_v4());

        // Same session registered under different agents
        storage.register_agent(&agent_id_1, &session_id);
        storage.register_agent(&agent_id_2, &session_id);

        // Each agent should see the session
        assert!(
            storage
                .get_agent_sessions(&agent_id_1)
                .contains(&session_id)
        );
        assert!(
            storage
                .get_agent_sessions(&agent_id_2)
                .contains(&session_id)
        );

        // Removing from one agent shouldn't affect the other
        storage.unregister_agent(&agent_id_1, &session_id);
        assert!(storage.get_agent_sessions(&agent_id_1).is_empty());
        assert!(
            storage
                .get_agent_sessions(&agent_id_2)
                .contains(&session_id)
        );
    }

    #[test]
    fn test_update_health_nonexistent_session() {
        let storage = DashMapSessionStorage::new();
        let session_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        // Should not panic when updating nonexistent session
        storage.update_health(&session_id, "2024-01-15T10:30:00Z".to_string(), true);
    }

    #[test]
    fn test_get_nonexistent_session() {
        let storage = DashMapSessionStorage::new();
        let session_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        assert!(storage.get(&session_id).is_none());
    }

    #[test]
    fn test_remove_nonexistent_session() {
        let storage = DashMapSessionStorage::new();
        let session_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        // Should return None, not panic
        assert!(storage.remove(&session_id).is_none());
    }
}
