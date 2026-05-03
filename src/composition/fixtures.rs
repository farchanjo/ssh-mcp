//! Test wiring.
//!
//! `#[cfg(test)]`-only. Will provide a `build_use_cases_test()` helper
//! returning a [`crate::composition::UseCases`] instantiated with fake
//! adapters once those land in H3. In H2 this is a placeholder that simply
//! demonstrates the composition root is reachable from tests.

#![cfg(test)]

#[cfg(test)]
mod tests {
    #[test]
    fn composition_root_compiles() {
        // Smoke test: the composition root module is wired and reachable
        // from the test target. Real fake adapters land in H3.
    }
}
