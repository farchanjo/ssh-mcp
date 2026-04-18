//! DashMap-based session storage implementation.
//!
//! Provides lock-free concurrent access to SSH sessions using `DashMap`.
//! Includes a secondary index for O(1) agent-to-sessions lookups.

use std::collections::HashSet;
use std::sync::Arc;

use std::sync::LazyLock;

use dashmap::DashMap;
use russh::client;
use tokio::sync::Semaphore;

use crate::mcp::session::SshClientHandler;
use crate::mcp::types::SessionInfo;

use super::traits::{SessionRef, SessionStorage};

/// Max concurrent SSH channels opened per session.
///
/// Strictly serializes channel opens on the same SSH session so rapid
/// `execute + cancel` bursts never race OpenSSH's `MaxSessions` budget
/// (default 10). One permit guarantees the previous channel has fully
/// closed (including the server's `CHANNEL_CLOSE` ack) before the next
/// `channel_open_session` call. Parallel workloads on the same session
/// still proceed — they simply queue through the semaphore.
pub const CHANNEL_CONCURRENCY_PER_SESSION: usize = 1;

/// Stored session data combining metadata with the actual session handle.
pub struct StoredSession {
    pub info: SessionInfo,
    pub handle: Arc<client::Handle<SshClientHandler>>,
    /// Semaphore gating how many channels may be open simultaneously on
    /// this SSH session. Acquired before `channel_open_session()` and
    /// released when the channel fully closes.
    pub channel_permits: Arc<Semaphore>,
}

/// Normalized identity triple used to look up equivalent sessions.
///
/// `host` is stored lowercased; `port` defaults to 22 when the input
/// `host` string does not contain an explicit `:port` suffix.
pub type IdentityTriple = (String, u16, String);

/// `DashMap`-based implementation of `SessionStorage`.
///
/// Secondary indices:
/// - `sessions_by_agent`: `agent_id` -> `HashSet<session_id>`
/// - `sessions_by_identity`: `(host_lc, port, user)` -> `HashSet<session_id>`
///   for smart-reuse detection on `ssh_connect`.
pub struct DashMapSessionStorage {
    sessions: DashMap<String, StoredSession>,
    sessions_by_agent: DashMap<String, HashSet<String>>,
    sessions_by_identity: DashMap<IdentityTriple, HashSet<String>>,
}

impl DashMapSessionStorage {
    /// Create a new session storage instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            sessions_by_agent: DashMap::new(),
            sessions_by_identity: DashMap::new(),
        }
    }

    /// Find all session IDs with the given `(host, port, username)` identity.
    ///
    /// `host` is compared case-insensitively. `port` of 0 is treated as 22
    /// for backwards compatibility with callers that omit the port.
    #[must_use]
    pub fn find_by_identity(&self, host: &str, port: u16, username: &str) -> Vec<String> {
        let triple = (
            host.to_lowercase(),
            if port == 0 { 22 } else { port },
            username.to_string(),
        );
        self.sessions_by_identity
            .get(&triple)
            .map(|entry| entry.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Parse a `host[:port]` string into a lowercased host and a port (default 22).
///
/// Supports `"host"`, `"host:22"`, and IPv6 bracketed forms `"[::1]:22"`.
#[must_use]
pub fn parse_host_port(raw: &str) -> (String, u16) {
    if let Some(rest) = raw.strip_prefix('[')
        && let Some((host, after)) = rest.split_once(']')
    {
        let port = after
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(22);
        return (host.to_lowercase(), port);
    }
    raw.rsplit_once(':').map_or_else(
        || (raw.to_lowercase(), 22_u16),
        |(host, port_str)| {
            let port = port_str.parse::<u16>().unwrap_or(22);
            (host.to_lowercase(), port)
        },
    )
}

fn identity_of(info: &SessionInfo) -> IdentityTriple {
    let (host_lc, port) = parse_host_port(&info.host);
    (host_lc, port, info.username.clone())
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
        let triple = identity_of(&info);
        self.sessions_by_identity
            .entry(triple)
            .or_default()
            .insert(session_id.clone());
        let channel_permits = Arc::new(Semaphore::new(CHANNEL_CONCURRENCY_PER_SESSION));
        self.sessions.insert(
            session_id,
            StoredSession {
                info,
                handle,
                channel_permits,
            },
        );
    }

    fn get(&self, session_id: &str) -> Option<SessionRef> {
        self.sessions.get(session_id).map(|entry| SessionRef {
            info: entry.info.clone(),
            handle: Arc::clone(&entry.handle),
            channel_permits: Arc::clone(&entry.channel_permits),
        })
    }

    fn remove(&self, session_id: &str) -> Option<SessionRef> {
        let removed = self.sessions.remove(session_id).map(|(_, stored)| stored);
        if let Some(stored) = removed.as_ref() {
            let triple = identity_of(&stored.info);
            if let Some(mut entry) = self.sessions_by_identity.get_mut(&triple) {
                entry.remove(session_id);
                if entry.is_empty() {
                    drop(entry);
                    self.sessions_by_identity.remove(&triple);
                }
            }
        }
        removed.map(|stored| SessionRef {
            info: stored.info,
            handle: stored.handle,
            channel_permits: stored.channel_permits,
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
pub static SESSION_STORAGE: LazyLock<DashMapSessionStorage> =
    LazyLock::new(DashMapSessionStorage::new);

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

    #[test]
    fn test_multiple_agents_multiple_sessions() {
        let storage = DashMapSessionStorage::new();
        let agent_1 = format!("agent-1-{}", uuid::Uuid::new_v4());
        let agent_2 = format!("agent-2-{}", uuid::Uuid::new_v4());
        let agent_3 = format!("agent-3-{}", uuid::Uuid::new_v4());

        // Register multiple sessions per agent
        for i in 0..5 {
            storage.register_agent(&agent_1, &format!("sess-1-{}-{}", i, uuid::Uuid::new_v4()));
        }
        for i in 0..3 {
            storage.register_agent(&agent_2, &format!("sess-2-{}-{}", i, uuid::Uuid::new_v4()));
        }
        for i in 0..2 {
            storage.register_agent(&agent_3, &format!("sess-3-{}-{}", i, uuid::Uuid::new_v4()));
        }

        assert_eq!(storage.get_agent_sessions(&agent_1).len(), 5);
        assert_eq!(storage.get_agent_sessions(&agent_2).len(), 3);
        assert_eq!(storage.get_agent_sessions(&agent_3).len(), 2);

        // Remove agent_2 sessions
        let removed = storage.remove_agent_sessions(&agent_2);
        assert_eq!(removed.len(), 3);

        // Agent_1 and Agent_3 should be unaffected
        assert_eq!(storage.get_agent_sessions(&agent_1).len(), 5);
        assert_eq!(storage.get_agent_sessions(&agent_3).len(), 2);
        assert!(storage.get_agent_sessions(&agent_2).is_empty());
    }

    #[test]
    fn test_unregister_all_sessions_from_agent() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("test-agent-{}", uuid::Uuid::new_v4());
        let session_ids: Vec<String> = (0..10)
            .map(|i| format!("session-{}-{}", i, uuid::Uuid::new_v4()))
            .collect();

        // Register all sessions
        for sess_id in &session_ids {
            storage.register_agent(&agent_id, sess_id);
        }
        assert_eq!(storage.get_agent_sessions(&agent_id).len(), 10);

        // Unregister one by one
        for sess_id in &session_ids {
            storage.unregister_agent(&agent_id, sess_id);
        }

        // Agent entry should be fully cleaned up
        assert!(storage.get_agent_sessions(&agent_id).is_empty());
    }

    #[test]
    fn test_update_health_multiple_times() {
        let storage = DashMapSessionStorage::new();
        let session_id = format!("health-test-{}", uuid::Uuid::new_v4());

        // Insert a session with minimal info to test health updates
        // Note: We can't easily test with real sessions, so we verify the method doesn't panic
        storage.update_health(&session_id, "2024-01-15T10:00:00Z".to_string(), true);
        storage.update_health(&session_id, "2024-01-15T10:05:00Z".to_string(), false);
        storage.update_health(&session_id, "2024-01-15T10:10:00Z".to_string(), true);

        // No panic means success
    }

    #[test]
    fn test_contains_after_removal() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("agent-{}", uuid::Uuid::new_v4());
        let session_id = format!("session-{}", uuid::Uuid::new_v4());

        storage.register_agent(&agent_id, &session_id);
        assert!(storage.get_agent_sessions(&agent_id).contains(&session_id));

        storage.unregister_agent(&agent_id, &session_id);
        assert!(!storage.get_agent_sessions(&agent_id).contains(&session_id));
    }

    #[test]
    fn test_empty_agent_id() {
        let storage = DashMapSessionStorage::new();
        let empty_agent = "";
        let session_id = format!("session-{}", uuid::Uuid::new_v4());

        // Empty agent ID should still work
        storage.register_agent(empty_agent, &session_id);
        assert!(
            storage
                .get_agent_sessions(empty_agent)
                .contains(&session_id)
        );

        storage.unregister_agent(empty_agent, &session_id);
        assert!(storage.get_agent_sessions(empty_agent).is_empty());
    }

    #[test]
    fn test_empty_session_id() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("agent-{}", uuid::Uuid::new_v4());
        let empty_session = "";

        // Empty session ID should still work
        storage.register_agent(&agent_id, empty_session);
        assert!(
            storage
                .get_agent_sessions(&agent_id)
                .contains(&empty_session.to_string())
        );

        storage.unregister_agent(&agent_id, empty_session);
        assert!(storage.get_agent_sessions(&agent_id).is_empty());
    }

    #[test]
    fn test_unicode_agent_and_session_ids() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("代理-{}", uuid::Uuid::new_v4());
        let session_id = format!("会话-{}", uuid::Uuid::new_v4());

        storage.register_agent(&agent_id, &session_id);
        let sessions = storage.get_agent_sessions(&agent_id);
        assert_eq!(sessions.len(), 1);
        assert!(sessions.contains(&session_id));
    }

    #[test]
    fn test_storage_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapSessionStorage>();
    }

    #[test]
    fn parse_host_port_bare_host_defaults_port_22() {
        let (host, port) = parse_host_port("example.com");
        assert_eq!(host, "example.com");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_host_port_explicit_port() {
        let (host, port) = parse_host_port("example.com:2222");
        assert_eq!(host, "example.com");
        assert_eq!(port, 2222);
    }

    #[test]
    fn parse_host_port_lowercases_host() {
        let (host, port) = parse_host_port("EXAMPLE.COM:22");
        assert_eq!(host, "example.com");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_host_port_ipv6_bracketed() {
        let (host, port) = parse_host_port("[::1]:2222");
        assert_eq!(host, "::1");
        assert_eq!(port, 2222);
    }

    #[test]
    fn parse_host_port_ipv4() {
        let (host, port) = parse_host_port("192.168.1.1:22");
        assert_eq!(host, "192.168.1.1");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_host_port_invalid_port_defaults_22() {
        let (host, port) = parse_host_port("host:not-a-port");
        assert_eq!(host, "host");
        assert_eq!(port, 22);
    }

    #[test]
    fn find_by_identity_empty_when_nothing_registered() {
        let storage = DashMapSessionStorage::new();
        assert!(storage.find_by_identity("host", 22, "user").is_empty());
    }

    #[test]
    fn find_by_identity_is_case_insensitive_on_host() {
        let storage = DashMapSessionStorage::new();
        // Populate the secondary index by using a SessionInfo directly on the map.
        // We can't insert without a real handle, so exercise via internal maps.
        storage
            .sessions_by_identity
            .entry(("vm.services".to_string(), 22, "root".to_string()))
            .or_default()
            .insert("sess-x".to_string());
        let hits_upper = storage.find_by_identity("VM.SERVICES", 22, "root");
        let hits_lower = storage.find_by_identity("vm.services", 22, "root");
        assert_eq!(hits_upper, hits_lower);
        assert!(hits_lower.contains(&"sess-x".to_string()));
    }

    #[test]
    fn stress_identity_index_thousand_entries_lookup_is_fast() {
        let storage = DashMapSessionStorage::new();
        // Pre-populate the index directly (we can't easily create real handles).
        for i in 0..1000_usize {
            storage
                .sessions_by_identity
                .entry((format!("host-{}", i % 50), 22, format!("user-{}", i % 10)))
                .or_default()
                .insert(format!("sess-{i}"));
        }
        let start = std::time::Instant::now();
        // Run 1000 lookups
        for i in 0..1000_usize {
            let hits = storage.find_by_identity(
                &format!("host-{}", i % 50),
                22,
                &format!("user-{}", i % 10),
            );
            assert!(!hits.is_empty());
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "1000 lookups on 1000-entry index took {}ms (expected <100ms)",
            elapsed.as_millis()
        );
    }

    #[test]
    fn find_by_identity_port_zero_treated_as_22() {
        let storage = DashMapSessionStorage::new();
        storage
            .sessions_by_identity
            .entry(("host".to_string(), 22, "u".to_string()))
            .or_default()
            .insert("sess-a".to_string());
        let hits = storage.find_by_identity("host", 0, "u");
        assert!(hits.contains(&"sess-a".to_string()));
    }

    #[test]
    fn test_session_ids_reflects_secondary_index() {
        let storage = DashMapSessionStorage::new();
        let agent_id = format!("agent-{}", uuid::Uuid::new_v4());

        // session_ids() returns session IDs from primary storage
        // Agent index is separate and may have different entries
        assert!(storage.session_ids().is_empty());

        // Registering to agent index doesn't affect session_ids
        // because session_ids reads from primary storage
        storage.register_agent(&agent_id, "sess-1");
        assert!(storage.session_ids().is_empty()); // No actual sessions stored
        assert_eq!(storage.get_agent_sessions(&agent_id).len(), 1); // But agent index has entry
    }
}
