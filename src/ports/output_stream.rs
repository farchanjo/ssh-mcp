//! Output-stream port.
//!
//! Surfaces the rolling output buffer maintained by command/shell aggregates
//! to MCP `resources/read` paths. The snapshot value type carries the
//! current cumulative cursor plus the latest sequence number so callers can
//! detect gaps and reconcile head-truncation.

use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ids::{CommandId, ShellId};

/// Snapshot of a single output stream at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
    /// Cumulative bytes consumed from the head since the producer started.
    pub byte_cursor: u64,
    /// Latest sequence number observed for this stream (`0` when no events
    /// have been allocated yet).
    pub last_seq: u64,
    /// Current rolling buffer contents (cheap to clone — `Bytes` is
    /// reference counted).
    pub stdout: Bytes,
    /// Current rolling stderr buffer contents.
    pub stderr: Bytes,
}

/// Output-stream port. Implementations are async because the production
/// adapter may need to acquire an `arc-swap` snapshot under a debounce
/// guard.
#[trait_variant::make(OutputStreamPort: Send)]
pub trait LocalOutputStreamPort: Sync {
    /// Snapshot the current output for an async command.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::CommandNotFound` if the id is unknown, or
    /// `DomainError::Storage` on backend failure.
    async fn snapshot_command(&self, id: &CommandId) -> Result<OutputSnapshot, DomainError>;

    /// Snapshot the current output for an interactive shell.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::ShellNotFound` if the id is unknown, or
    /// `DomainError::Storage` on backend failure.
    async fn snapshot_shell(&self, id: &ShellId) -> Result<OutputSnapshot, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::{OutputSnapshot, OutputStreamPort};
    use bytes::Bytes;

    fn _assert_port<T: OutputStreamPort>() {}

    #[test]
    fn snapshot_struct_round_trip() {
        let snap = OutputSnapshot {
            byte_cursor: 42,
            last_seq: 7,
            stdout: Bytes::from_static(b"hello"),
            stderr: Bytes::new(),
        };
        assert_eq!(snap.byte_cursor, 42);
        assert_eq!(snap.last_seq, 7);
        assert_eq!(snap.stdout.as_ref(), b"hello");
        assert!(snap.stderr.is_empty());
    }
}
