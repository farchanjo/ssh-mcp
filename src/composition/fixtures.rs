//! Test wiring.
//!
//! Test-only fixtures. The composition root proper is exercised end-to-end
//! by the binary entry points; this module collects helpers that downstream
//! tests pre-populate (or assert against) without depending on `prod::build_use_cases`.

#![cfg(test)]

use std::sync::Arc;

use crate::adapters::capability::registry::CapabilityRegistry;

/// Build a fresh, empty [`CapabilityRegistry`] handle for tests.
///
/// ADR 0012 Phase 3 — tests that exercise the inline-push path (or
/// the peer-GC `forget_peer` contract) seed the registry through this
/// helper instead of mining the production composition root.
#[must_use]
pub fn build_capability_registry() -> Arc<CapabilityRegistry> {
    Arc::new(CapabilityRegistry::new())
}

#[cfg(test)]
mod tests {
    use super::build_capability_registry;
    use crate::adapters::capability::registry::CapabilityFlag;
    use crate::domain::ids::PeerId;

    #[test]
    fn composition_root_compiles() {
        // Smoke test: the composition root module is wired and reachable
        // from the test target.
    }

    #[test]
    fn build_capability_registry_returns_empty_handle() {
        let reg = build_capability_registry();
        assert!(reg.is_empty());
    }

    #[test]
    fn fixture_registry_supports_record_and_forget() {
        let reg = build_capability_registry();
        let peer = PeerId::new("fixture-peer".to_string());
        reg.record_capability(peer.clone(), CapabilityFlag::InlinePush, true);
        assert!(reg.peer_has_capability(&peer, CapabilityFlag::InlinePush));
        reg.forget_peer(&peer);
        assert!(reg.is_empty());
    }
}
