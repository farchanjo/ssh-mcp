//! Storage trait definitions for session, command, shell, and transfer management.
//!
//! These traits define the interface for storage implementations, enabling
//! dependency injection and testability through mocking.

use std::sync::Arc;

use dashmap::mapref::one::Ref;
use russh::client;
use tokio::sync::{Semaphore, broadcast};

use crate::mcp::async_command::RunningCommand;
use crate::mcp::session::SshClientHandler;
use crate::mcp::shell::RunningShell;
use crate::mcp::transfer::{RunningTransfer, TransferInfo};
use crate::mcp::types::{
    AsyncCommandInfo, AsyncCommandStatus, HealthEvent, SessionInfo, ShellInfo,
};

/// Reference to a stored session for read-only access.
pub struct SessionRef {
    pub info: SessionInfo,
    pub handle: Arc<client::Handle<SshClientHandler>>,
    /// Semaphore limiting concurrent SSH channels on this session.
    pub channel_permits: Arc<Semaphore>,
    /// Live broadcast of health-check events. Subscribers consume these via
    /// `health_tx.subscribe()` to drive the future
    /// `session://<id>/health` MCP resource (E13).
    pub health_tx: broadcast::Sender<HealthEvent>,
}

/// Trait for session storage operations.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. The default implementation uses `DashMap` for lock-free
/// concurrent access.
#[allow(
    dead_code,
    reason = "trait defines the storage API contract used via dynamic dispatch"
)]
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
#[allow(dead_code, reason = "struct fields are part of the public storage API")]
pub struct CommandRef {
    pub info: AsyncCommandInfo,
    pub running: Arc<RunningCommand>,
}

/// Trait for async command storage operations.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. The default implementation uses `DashMap` for lock-free
/// concurrent access with a secondary index for O(1) session lookups.
#[allow(
    dead_code,
    reason = "trait defines the storage API contract used via dynamic dispatch"
)]
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

    /// Count only running commands for a session.
    fn count_running_by_session(&self, session_id: &str) -> usize;

    /// List all commands.
    fn list_all(&self) -> Vec<AsyncCommandInfo>;

    /// List commands filtered by optional session ID and/or status.
    fn list_filtered(
        &self,
        session_id: Option<&str>,
        status: Option<AsyncCommandStatus>,
    ) -> Vec<AsyncCommandInfo>;
}

/// Trait for shell storage operations.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. The default implementation uses `DashMap` for lock-free
/// concurrent access with a secondary index for O(1) session lookups.
#[allow(
    dead_code,
    reason = "trait defines the storage API contract used via dynamic dispatch"
)]
pub trait ShellStorage: Send + Sync {
    /// Register a new shell.
    fn register(&self, shell_id: String, shell: RunningShell);

    /// Unregister a shell by ID, returning it if it existed.
    fn unregister(&self, shell_id: &str) -> Option<RunningShell>;

    /// Get a direct reference to a shell.
    fn get_direct(&self, shell_id: &str) -> Option<Ref<'_, String, RunningShell>>;

    /// List all shell IDs for a session.
    fn list_by_session(&self, session_id: &str) -> Vec<String>;

    /// Count shells for a session.
    fn count_by_session(&self, session_id: &str) -> usize;

    /// List all shell info entries.
    fn list_all(&self) -> Vec<ShellInfo>;

    /// List shell info filtered by session.
    fn list_filtered(&self, session_id: Option<&str>) -> Vec<ShellInfo>;
}

/// Trait for transfer storage operations.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. The default implementation uses `DashMap` for lock-free
/// concurrent access with a secondary index for O(1) session lookups.
#[allow(
    dead_code,
    reason = "trait defines the storage API contract used via dynamic dispatch"
)]
pub trait TransferStorage: Send + Sync {
    /// Register a new transfer.
    fn register(&self, transfer_id: String, transfer: RunningTransfer);

    /// Unregister a transfer by ID, returning it if it existed.
    fn unregister(&self, transfer_id: &str) -> Option<RunningTransfer>;

    /// Get a direct reference to a transfer.
    fn get_direct(&self, transfer_id: &str) -> Option<Ref<'_, String, RunningTransfer>>;

    /// List all transfer IDs for a session.
    fn list_by_session(&self, session_id: &str) -> Vec<String>;

    /// Count transfers for a session.
    fn count_by_session(&self, session_id: &str) -> usize;

    /// List all transfer info entries.
    fn list_all(&self) -> Vec<TransferInfo>;

    /// List transfer info filtered by session.
    fn list_filtered(&self, session_id: Option<&str>) -> Vec<TransferInfo>;
}
