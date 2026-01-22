//! Storage trait definitions for session and command management.
//!
//! These traits define the interface for storage implementations, enabling
//! dependency injection and testability through mocking.

use std::sync::Arc;

use russh::client;

use crate::mcp::async_command::RunningCommand;
use crate::mcp::session::SshClientHandler;
use crate::mcp::types::{AsyncCommandInfo, SessionInfo};

/// Reference to a stored session for read-only access.
pub struct SessionRef {
    pub info: SessionInfo,
    pub handle: Arc<client::Handle<SshClientHandler>>,
}

/// Trait for session storage operations.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. The default implementation uses `DashMap` for lock-free
/// concurrent access.
#[allow(dead_code)]
pub trait SessionStorage: Send + Sync {
    /// Insert a new session into storage.
    fn insert(
        &self,
        session_id: String,
        info: SessionInfo,
        handle: Arc<client::Handle<SshClientHandler>>,
    );

    /// Get a session by ID, returning a clone of its data.
    fn get(&self, session_id: &str) -> Option<SessionRef>;

    /// Remove a session by ID, returning its data if it existed.
    fn remove(&self, session_id: &str) -> Option<SessionRef>;

    /// List all sessions, returning cloned session info.
    fn list(&self) -> Vec<SessionInfo>;

    /// Check if a session exists.
    fn contains(&self, session_id: &str) -> bool;

    /// Get all session IDs.
    fn session_ids(&self) -> Vec<String>;

    /// Update session health status.
    fn update_health(&self, session_id: &str, last_check: String, healthy: bool);

    /// Register a session under an agent ID.
    fn register_agent(&self, agent_id: &str, session_id: &str);

    /// Unregister a session from an agent ID.
    fn unregister_agent(&self, agent_id: &str, session_id: &str);

    /// Get all session IDs for a specific agent.
    fn get_agent_sessions(&self, agent_id: &str) -> Vec<String>;

    /// Remove all sessions for an agent and return their IDs.
    fn remove_agent_sessions(&self, agent_id: &str) -> Vec<String>;
}

/// Reference to a running command for read-only access.
#[allow(dead_code)]
pub struct CommandRef {
    pub info: AsyncCommandInfo,
    pub running: Arc<RunningCommand>,
}

/// Trait for async command storage operations.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. The default implementation uses `DashMap` for lock-free
/// concurrent access with a secondary index for O(1) session lookups.
#[allow(dead_code)]
pub trait CommandStorage: Send + Sync {
    /// Register a new async command.
    fn register(&self, command_id: String, command: RunningCommand);

    /// Unregister a command by ID, returning it if it existed.
    fn unregister(&self, command_id: &str) -> Option<RunningCommand>;

    /// Get a command by ID.
    fn get(&self, command_id: &str) -> Option<Arc<RunningCommand>>;

    /// Get command info and running state for read access.
    fn get_ref(&self, command_id: &str) -> Option<CommandRef>;

    /// List all command IDs for a session.
    fn list_by_session(&self, session_id: &str) -> Vec<String>;

    /// Count commands for a session.
    fn count_by_session(&self, session_id: &str) -> usize;

    /// List all commands, optionally filtered by session and/or status.
    fn list_all(&self) -> Vec<AsyncCommandInfo>;
}
