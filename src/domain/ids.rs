//! Strongly-typed newtype identifiers for the v4 hexagonal core.
//!
//! Every aggregate and external actor is addressed via a dedicated newtype
//! over `String`, so the type system rules out the classic mix-up of
//! `session_id` vs `command_id` at every API boundary.
//!
//! All ids are immutable, cheap to clone (the wrapper has no allocation
//! beyond the inner `String`), serializable for transport at the adapter
//! edges, and `Display`-friendly for logs.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifier for an SSH session aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Wrap a raw identifier string.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier for an async command aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CommandId(String);

impl CommandId {
    /// Wrap a raw identifier string.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier for an interactive shell aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ShellId(String);

impl ShellId {
    /// Wrap a raw identifier string.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ShellId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier for an SFTP transfer aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct TransferId(String);

impl TransferId {
    /// Wrap a raw identifier string.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for TransferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier for a port-forwarder aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ForwardId(String);

impl ForwardId {
    /// Wrap a raw identifier string.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ForwardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of an external agent that owns one or more sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    /// Wrap a raw identifier string.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of a connected MCP peer (subscriber). Stable for the lifetime
/// of the underlying transport.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct PeerId(String);

impl PeerId {
    /// Wrap a raw identifier string.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentId, CommandId, ForwardId, PeerId, SessionId, ShellId, TransferId};

    #[test]
    fn session_id_round_trip_preserves_value() {
        let id = SessionId::new("sess-1".to_string());
        assert_eq!(id.as_str(), "sess-1");
        assert_eq!(id.to_string(), "sess-1");
        assert_eq!(id.clone().into_inner(), "sess-1");
    }

    #[test]
    fn ids_implement_eq_and_hash() {
        let a = CommandId::new("cmd-1".to_string());
        let b = CommandId::new("cmd-1".to_string());
        let c = CommandId::new("cmd-2".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = std::collections::HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }

    #[test]
    fn distinct_id_types_do_not_share_values_at_type_level() {
        // Compile-time guarantee: a SessionId and a CommandId built from the
        // same underlying string are different types and cannot be compared
        // by accident. Asserting display-equality here documents the intent.
        let session = SessionId::new("x".to_string());
        let command = CommandId::new("x".to_string());
        assert_eq!(session.to_string(), command.to_string());
    }

    #[test]
    fn shell_transfer_forward_agent_peer_round_trip() {
        assert_eq!(ShellId::new("s".to_string()).as_str(), "s");
        assert_eq!(TransferId::new("t".to_string()).as_str(), "t");
        assert_eq!(ForwardId::new("f".to_string()).as_str(), "f");
        assert_eq!(AgentId::new("a".to_string()).as_str(), "a");
        assert_eq!(PeerId::new("p".to_string()).as_str(), "p");
    }

    #[test]
    fn serde_uses_transparent_string_wire_format() {
        let id = SessionId::new("abc".to_string());
        let json = serde_json::to_string(&id).expect("serialise");
        assert_eq!(json, "\"abc\"");
        let back: SessionId = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, id);
    }
}
