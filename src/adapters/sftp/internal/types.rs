//! Internal data types used by the SFTP adapter (legacy leftover,
//! relocated in H17.6 P1 from the former v3 types module to
//! `crate::adapters::sftp::internal::types`).
//!
//! These carriers feed the lock-free transfer state in
//! [`super::transfer::RunningTransfer`] and the streaming chunk loop in
//! [`super::sftp`].

/// Live transition event broadcast by `RunningTransfer`. Subscribers consume
/// these via `progress_tx` to drive the `transfer://<id>/progress` MCP
/// resource.
///
/// Each variant carries a `seq` allocated by
/// `crate::adapters::subscription::legacy::SubscriptionRegistry::next_seq` so subscribers
/// recovering from `Lagged` can detect gaps.
#[derive(Debug, Clone, Copy)]
pub enum ProgressEvent {
    /// Transfer made progress — `bytes_transferred` was just updated.
    Tick {
        seq: u64,
        bytes_transferred: u64,
        total_bytes: u64,
    },
    /// Transfer terminated successfully.
    Completed { seq: u64, bytes_transferred: u64 },
    /// Transfer failed (the failure reason is on `RunningTransfer.error`).
    Failed { seq: u64 },
    /// Transfer was cancelled by caller.
    Cancelled { seq: u64 },
}
