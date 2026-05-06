//! Strongly-typed identifier for an [`crate::domain::rsync::RsyncSession`]
//! aggregate.
//!
//! Mirrors the existing `*Id` newtype pattern in
//! [`crate::domain::ids`]; kept in a dedicated module so the rsync
//! domain stays composable without dragging the broader id surface
//! through every import. Phase 5+ adds related newtypes (`AgentSha256`,
//! `RsyncFileIdx`); they will live alongside this one.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifier for an rsync sync session aggregate. Wraps a `String` so
/// the wire format stays human-readable (the host mints a `UUIDv7`
/// stringified, matching the existing v5 `SubId` convention).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct RsyncId(String);

impl RsyncId {
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

impl fmt::Display for RsyncId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::RsyncId;

    #[test]
    fn round_trips_through_display_and_into_inner() {
        let id = RsyncId::new("rs-1".to_string());
        assert_eq!(id.as_str(), "rs-1");
        assert_eq!(id.to_string(), "rs-1");
        assert_eq!(id.clone().into_inner(), "rs-1");
    }

    #[test]
    fn equality_on_inner_string() {
        let a = RsyncId::new("rs-x".to_string());
        let b = RsyncId::new("rs-x".to_string());
        let c = RsyncId::new("rs-y".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn serde_uses_transparent_string_wire_format() {
        let id = RsyncId::new("rs-7".to_string());
        let json = serde_json::to_string(&id).unwrap_or_else(|err| panic!("encode: {err}"));
        assert_eq!(json, "\"rs-7\"");
        let back: RsyncId =
            serde_json::from_str(&json).unwrap_or_else(|err| panic!("decode: {err}"));
        assert_eq!(back, id);
    }
}
