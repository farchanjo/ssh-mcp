//! Session aggregate value object.
//!
//! Mirrors the legacy v3 `SessionInfo` payload but lives in
//! pure-domain territory: no `tokio`, `russh`, or `dashmap` dependencies.
//! The aggregate is immutable beyond the helpers provided here; mutation
//! happens through use cases that produce a new entity instance.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::identity::Address;
use super::ids::{AgentId, SessionId};

/// Snapshot of a single SSH session as understood by the domain layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntity {
    /// Stable identifier minted at connect time.
    pub id: SessionId,
    /// Optional human-friendly label used in listings and LLM hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional grouping key (e.g. owner agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    /// Resolved (host, port) endpoint.
    pub address: Address,
    /// Login user.
    pub username: String,
    /// Wall-clock timestamp of the successful connect.
    pub connected_at: DateTime<Utc>,
    /// Default per-session timeout.
    pub default_timeout: Duration,
    /// Number of retry attempts consumed during connect.
    pub retry_attempts: u32,
    /// Whether SSH zlib compression was negotiated.
    pub compression_enabled: bool,
    /// Wall-clock timestamp of the most recent health probe, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_health_check: Option<DateTime<Utc>>,
    /// Outcome of the most recent health probe, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
}

impl SessionEntity {
    /// `true` when this session matches the requested identity triple. Used
    /// by `connect_session` to satisfy [`super::policy::ReusePolicy`].
    #[must_use]
    pub fn matches_identity(&self, address: &Address, username: &str) -> bool {
        &self.address == address && self.username == username
    }

    /// Apply a fresh health probe outcome and return the updated entity.
    #[must_use]
    pub const fn with_health(mut self, at: DateTime<Utc>, healthy: bool) -> Self {
        self.last_health_check = Some(at);
        self.healthy = Some(healthy);
        self
    }

    /// Overwrite the timestamp of the most recent health probe.
    ///
    /// Provided for repository adapters that mutate an entity in place
    /// behind a shard lock (e.g. [`crate::ports::session_repo::SessionRepository::update_health`]).
    /// Prefer [`Self::with_health`] in builder-style code.
    pub const fn set_last_health_check(&mut self, at: DateTime<Utc>) {
        self.last_health_check = Some(at);
    }

    /// Overwrite the outcome of the most recent health probe.
    ///
    /// Provided for repository adapters that mutate an entity in place
    /// behind a shard lock (e.g. [`crate::ports::session_repo::SessionRepository::update_health`]).
    /// Prefer [`Self::with_health`] in builder-style code.
    pub const fn set_healthy(&mut self, healthy: bool) {
        self.healthy = Some(healthy);
    }
}

#[cfg(test)]
mod tests {
    use super::{Address, AgentId, SessionEntity, SessionId};
    use chrono::Utc;
    use std::time::Duration;

    fn sample() -> SessionEntity {
        SessionEntity {
            id: SessionId::new("sess-1".to_string()),
            name: Some("primary".to_string()),
            agent_id: Some(AgentId::new("agent-A".to_string())),
            address: Address::new("h".to_string(), 22).expect("address"),
            username: "alice".to_string(),
            connected_at: Utc::now(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: None,
        }
    }

    #[test]
    fn matches_identity_compares_address_and_username() {
        let s = sample();
        assert!(s.matches_identity(&s.address.clone(), "alice"));
        assert!(!s.matches_identity(&s.address.clone(), "bob"));
        let other_addr = Address::new("h".to_string(), 2222).expect("address");
        assert!(!s.matches_identity(&other_addr, "alice"));
    }

    #[test]
    fn with_health_updates_probe_fields() {
        let s = sample();
        let now = Utc::now();
        let updated = s.with_health(now, true);
        assert_eq!(updated.healthy, Some(true));
        assert_eq!(updated.last_health_check, Some(now));
    }

    #[test]
    fn set_last_health_check_overwrites_in_place() {
        let mut s = sample();
        let now = Utc::now();
        s.set_last_health_check(now);
        assert_eq!(s.last_health_check, Some(now));
    }

    #[test]
    fn set_healthy_overwrites_in_place() {
        let mut s = sample();
        s.set_healthy(false);
        assert_eq!(s.healthy, Some(false));
        s.set_healthy(true);
        assert_eq!(s.healthy, Some(true));
    }
}
