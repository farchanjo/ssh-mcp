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
        let agent_id = "test-agent";
        let session_id_1 = "session-1";
        let session_id_2 = "session-2";

        storage.register_agent(agent_id, session_id_1);
        storage.register_agent(agent_id, session_id_2);

        let sessions = storage.get_agent_sessions(agent_id);
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&session_id_1.to_string()));
        assert!(sessions.contains(&session_id_2.to_string()));
    }

    #[test]
    fn test_agent_unregistration() {
        let storage = DashMapSessionStorage::new();
        let agent_id = "test-agent-unreg";
        let session_id_1 = "session-a";
        let session_id_2 = "session-b";

        storage.register_agent(agent_id, session_id_1);
        storage.register_agent(agent_id, session_id_2);
        storage.unregister_agent(agent_id, session_id_1);

        let sessions = storage.get_agent_sessions(agent_id);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains(&session_id_2.to_string()));

        // Unregister last session removes agent entry
        storage.unregister_agent(agent_id, session_id_2);
        assert!(storage.get_agent_sessions(agent_id).is_empty());
    }

    #[test]
    fn test_remove_agent_sessions() {
        let storage = DashMapSessionStorage::new();
        let agent_id = "test-agent-remove";
        let session_id_1 = "session-x";
        let session_id_2 = "session-y";

        storage.register_agent(agent_id, session_id_1);
        storage.register_agent(agent_id, session_id_2);

        let removed = storage.remove_agent_sessions(agent_id);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&session_id_1.to_string()));
        assert!(removed.contains(&session_id_2.to_string()));

        // Agent entry should be gone
        assert!(storage.get_agent_sessions(agent_id).is_empty());
    }

    #[test]
    fn test_get_agent_sessions_empty() {
        let storage = DashMapSessionStorage::new();
        let sessions = storage.get_agent_sessions("nonexistent-agent");
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_contains() {
        let storage = DashMapSessionStorage::new();
        assert!(!storage.contains("nonexistent"));
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
}
