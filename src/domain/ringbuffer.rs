//! Re-exports of the v3 leaf value-object types that already satisfy the
//! "no tokio in their public API" rule.
//!
//! The aggregates that *carry* tokio types (`RunningCommand`, `RunningShell`,
//! `RunningTransfer`) intentionally live under
//! `crate::adapters::{ssh,sftp}::internal::*` (relocated in H17.6 P1) — they
//! are adapter-side runtime state, not domain values.

#![allow(
    clippy::pub_use,
    reason = "intentional re-export of v3 leaf value objects so domain code can refer to them without crossing the adapter boundary"
)]

pub use crate::adapters::ssh::internal::async_command::OutputBuffer;
pub use crate::adapters::ssh::internal::shell::RingBuffer;

#[cfg(test)]
mod tests {
    use super::{OutputBuffer, RingBuffer};
    use bytes::Bytes;

    #[test]
    fn ring_buffer_default_is_empty() {
        let buf = RingBuffer::default();
        assert!(buf.data.is_empty());
    }

    #[test]
    fn ring_buffer_clone_shares_payload() {
        let buf = RingBuffer {
            data: Bytes::from_static(b"hello"),
        };
        let cloned = buf.clone();
        assert_eq!(buf.data.as_ref(), cloned.data.as_ref());
    }

    #[test]
    fn output_buffer_default_is_empty() {
        let buf = OutputBuffer::default();
        assert!(buf.stdout.is_empty());
        assert!(buf.stderr.is_empty());
    }
}
