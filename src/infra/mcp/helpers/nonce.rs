//! Random nonce generator for output-block delimiters.
//!
//! Mirrors v3 `src/mcp/message/helpers.rs::generate_nonce` exactly so the
//! anti-injection delimiter contract stays unchanged across versions.

use uuid::Uuid;

/// Length of the nonce in hex characters (32 bits of entropy).
pub const NONCE_LEN: usize = 8;

/// Generate a random 8-char lowercase hex nonce.
///
/// Uses the first 32 random bits of a `UUIDv4` (~4 billion combinations).
/// Used as a per-response delimiter hash to prevent injection attacks
/// from content imitating output block markers.
#[must_use]
pub fn generate_nonce() -> String {
    let (high, _, _, _) = Uuid::new_v4().as_fields();
    format!("{high:08x}")
}

#[cfg(test)]
mod tests {
    use super::{NONCE_LEN, generate_nonce};

    #[test]
    fn length_is_eight_lowercase_hex() {
        let nonce = generate_nonce();
        assert_eq!(nonce.len(), NONCE_LEN);
        assert!(
            nonce
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn hundred_calls_have_no_collisions() {
        let mut set = std::collections::HashSet::new();
        for _ in 0..100_usize {
            set.insert(generate_nonce());
        }
        assert_eq!(set.len(), 100, "collisions found in 100 nonces");
    }
}
