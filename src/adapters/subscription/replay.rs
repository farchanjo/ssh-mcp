//! Replay helper for the per-`SubId` lane (v5 Phase 2).
//!
//! Phase 2 emits a [`LaneMsg::Snapshot`] marker on the lane mpsc when
//! the consumer asks to replay from a cursor — the resource ring
//! buffer is the canonical source of truth, so the actual bytes flow
//! back through the standard `produce` path on the next push. This
//! module wraps the marker construction so the use case never has to
//! know the wire shape of the event.

use crate::domain::error::DomainError;

use super::subscriber_lane::LaneMsg;

/// Build a [`LaneMsg::Snapshot`] for the given cursor.
///
/// Phase 2 emits an empty-delta marker; Phase 3 attaches the
/// reconstructed bytes inline once the resource ring buffer is wired
/// through the lane port.
#[must_use]
pub const fn snapshot_at(cursor: u64) -> LaneMsg {
    LaneMsg::Snapshot {
        cursor,
        delta: Vec::new(),
    }
}

/// Validate that `cursor` lies within the configured replay window
/// (`bytes_available` is the resource ring buffer size).
///
/// # Errors
///
/// Returns [`DomainError::InvalidArgument`] when the cursor is past
/// the available bytes.
pub fn validate_cursor(cursor: u64, bytes_available: u64) -> Result<(), DomainError> {
    if cursor > bytes_available {
        return Err(DomainError::InvalidArgument(format!(
            "cursor {cursor} past replay window {bytes_available}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LaneMsg, snapshot_at, validate_cursor};

    #[test]
    fn snapshot_at_carries_cursor() {
        let msg = snapshot_at(42);
        match msg {
            LaneMsg::Snapshot { cursor, delta } => {
                assert_eq!(cursor, 42);
                assert!(delta.is_empty());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn validate_cursor_accepts_zero_and_in_range() {
        assert!(validate_cursor(0, 100).is_ok());
        assert!(validate_cursor(50, 100).is_ok());
        assert!(validate_cursor(100, 100).is_ok());
    }

    #[test]
    fn validate_cursor_rejects_out_of_range() {
        assert!(validate_cursor(101, 100).is_err());
    }
}
