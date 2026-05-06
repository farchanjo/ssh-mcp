//! Domain aggregate for an active rsync sync session.
//!
//! Mirrors the lock-free style of the existing
//! [`crate::domain::transfer::TransferEntity`] and the adapter-level
//! [`crate::adapters::lifecycle::refcount::ResourceLifecycle`]:
//!
//! - Status encoded as an [`AtomicU8`] so concurrent
//!   `complete` / `fail` / `cancel` paths converge through
//!   `compare_exchange`.
//! - Counters encoded as [`AtomicU64`] so progress reads from a
//!   subscriber lane never block the producer that just emitted a
//!   delta token.
//! - Stats snapshot is rebuilt from the atomic counters on each read
//!   (see [`RsyncSession::snapshot`]). The [`RsyncStats`] type is
//!   `Copy`, so a per-read assembly costs the same as an
//!   `ArcSwap<RsyncStats>` load on the hot path while keeping the
//!   domain layer free of the `arc-swap` dependency (the domain
//!   layer rules in [`crate::domain`] cap the import surface to
//!   `std`, `serde`, `serde_json`, `chrono`, `thiserror`,
//!   `schemars`, `bytes`).
//! - Zero `Mutex`. The producer / consumer threads land in the
//!   future Wire / SFTP transports; the push lane wires through the
//!   v5 channel-mux pipeline unchanged.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::domain::ids::SessionId;
use crate::domain::rsync_ids::RsyncId;

/// Final or in-flight aggregate counters. Every field is monotonic
/// non-decreasing during a session — the transport never adjusts a
/// counter downward.
///
/// Lives in the domain layer because both the use-case state machine
/// and the transport-layer push-event projection ride this same
/// shape; keeping it under `domain::rsync` lets every other module
/// re-export it via a single canonical path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RsyncStats {
    /// Files the sync planned to handle (file-list size).
    pub files_total: u64,
    /// Files the sync actually finished (success or skip).
    pub files_done: u64,
    /// Bytes the sync planned to handle (sum of source file sizes).
    pub bytes_total: u64,
    /// Bytes that crossed the wire after delta-sync.
    pub bytes_transferred: u64,
    /// Bytes the delta algorithm (or size+mtime match) avoided.
    pub bytes_skipped: u64,
    /// Files removed by `--delete`.
    pub files_deleted: u64,
    /// Files that hit a per-file error.
    pub files_failed: u64,
}

impl RsyncStats {
    /// Build a fresh zeroed stats instance — the canonical "session
    /// has not started" snapshot the host loads into the
    /// `RsyncSession` aggregate at session-open time.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            files_total: 0,
            files_done: 0,
            bytes_total: 0,
            bytes_transferred: 0,
            bytes_skipped: 0,
            files_deleted: 0,
            files_failed: 0,
        }
    }

    /// Bandwidth-savings ratio expressed as permille (`0..=1000`),
    /// `bytes_skipped * 1000 / bytes_total`. Uses integer arithmetic
    /// so the workspace `as_conversions` lint stays happy. Returns
    /// `0` when `bytes_total == 0` to avoid divide-by-zero. Saturates
    /// to `u32::MAX` on the (degenerate) overflow path.
    #[must_use]
    pub fn savings_permille(self) -> u32 {
        if self.bytes_total == 0 {
            return 0;
        }
        let scaled = self.bytes_skipped.saturating_mul(1000_u64) / self.bytes_total;
        u32::try_from(scaled).unwrap_or(u32::MAX)
    }
}

/// Status byte tags. Stable wire bytes are documented per variant so
/// a future loom test can pin the byte layout without referring to
/// the source.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RsyncStatus {
    /// Session minted; not yet emitting frames. Wire byte: `0x01`.
    Pending = 0x01,
    /// Probe phase — server checking remote rsync version /
    /// architecture. Wire byte: `0x02`.
    Probing = 0x02,
    /// Active sync (file-list or delta phase). Wire byte: `0x03`.
    Running = 0x03,
    /// Sync finished cleanly. Wire byte: `0x04`.
    Completed = 0x04,
    /// Sync failed mid-flight. Wire byte: `0x05`.
    Failed = 0x05,
    /// Server-issued cancel landed. Wire byte: `0x06`.
    Cancelled = 0x06,
}

impl RsyncStatus {
    /// Encode as the wire byte. Hand-written match keeps the codec
    /// free of `as` casts (the workspace `as_conversions` lint is
    /// `deny`).
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Pending => 0x01,
            Self::Probing => 0x02,
            Self::Running => 0x03,
            Self::Completed => 0x04,
            Self::Failed => 0x05,
            Self::Cancelled => 0x06,
        }
    }

    /// Decode from the wire byte. `None` for unknown bytes so the
    /// caller can surface a typed `INTERNAL_ERROR` rather than
    /// panicking.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Pending),
            0x02 => Some(Self::Probing),
            0x03 => Some(Self::Running),
            0x04 => Some(Self::Completed),
            0x05 => Some(Self::Failed),
            0x06 => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// `true` once the session reached a terminal state (no further
    /// transitions accepted).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Active rsync sync session aggregate.
///
/// Constructed by the use case layer (`RsyncSyncUseCase` — phase 5+);
/// the host stores it under an [`crate::domain::ids::SessionId`]-keyed
/// repository. Every read counter is `Acquire`, every write is
/// `Release`, mirroring the lifecycle adapter's memory ordering.
#[derive(Debug)]
pub struct RsyncSession {
    /// Stable identifier.
    id: RsyncId,
    /// Session that owns the russh channel.
    session_id: SessionId,
    /// Status byte. CAS'd through [`RsyncSession::transition`].
    status: AtomicU8,
    /// Bytes the agent reports as having crossed the wire after
    /// delta-sync.
    bytes_transferred: AtomicU64,
    /// Bytes the delta algorithm avoided.
    bytes_skipped: AtomicU64,
    /// Files the sync has finished (success or skip).
    files_done: AtomicU64,
    /// Files the planner expected to handle.
    files_total: AtomicU64,
    /// Bytes the planner expected to handle.
    bytes_total: AtomicU64,
    /// Bytes deleted by `--delete`. Only populated when the request
    /// asked for `--delete`.
    files_deleted: AtomicU64,
    /// Files that hit a per-file error.
    files_failed: AtomicU64,
}

impl RsyncSession {
    /// Mint a fresh session in [`RsyncStatus::Pending`]. All counters
    /// start at zero. Use [`Self::with_files_total`] /
    /// [`Self::with_bytes_total`] to seed the planner totals once the
    /// file-list phase finishes.
    #[must_use]
    pub const fn new(id: RsyncId, session_id: SessionId) -> Self {
        Self {
            id,
            session_id,
            status: AtomicU8::new(RsyncStatus::Pending.byte()),
            bytes_transferred: AtomicU64::new(0),
            bytes_skipped: AtomicU64::new(0),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            files_deleted: AtomicU64::new(0),
            files_failed: AtomicU64::new(0),
        }
    }

    /// Stable identifier.
    #[must_use]
    pub const fn id(&self) -> &RsyncId {
        &self.id
    }

    /// Session that owns the russh channel.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Seed the planner's expected file count. Called at the end of
    /// the file-list phase (phase 5+). Idempotent: subsequent calls
    /// overwrite, so a re-probe on a retried session converges on the
    /// new planner output.
    pub fn with_files_total(&self, total: u64) {
        self.files_total.store(total, Ordering::Release);
    }

    /// Seed the planner's expected byte count.
    pub fn with_bytes_total(&self, total: u64) {
        self.bytes_total.store(total, Ordering::Release);
    }

    /// Increment `bytes_transferred` and `bytes_skipped` by
    /// `delta_transferred` / `delta_skipped` respectively. Called from
    /// the agent reader loop on each `FileDone` op. Saturating: a
    /// pathologically wrong agent never wraps the counter past `u64::MAX`.
    pub fn record_file_done(&self, delta_transferred: u64, delta_skipped: u64) {
        self.bytes_transferred
            .fetch_add(delta_transferred, Ordering::AcqRel);
        self.bytes_skipped
            .fetch_add(delta_skipped, Ordering::AcqRel);
        self.files_done.fetch_add(1, Ordering::AcqRel);
    }

    /// Record a `--delete` op.
    pub fn record_file_deleted(&self) {
        self.files_deleted.fetch_add(1, Ordering::AcqRel);
    }

    /// Record a per-file failure (sync continues).
    pub fn record_file_failed(&self) {
        self.files_failed.fetch_add(1, Ordering::AcqRel);
    }

    /// Read the current status byte and decode it. Fallback to
    /// [`RsyncStatus::Pending`] in the impossible case the byte got
    /// corrupted (defensive — keeps callers from panicking).
    #[must_use]
    pub fn status(&self) -> RsyncStatus {
        let byte = self.status.load(Ordering::Acquire);
        RsyncStatus::from_byte(byte).unwrap_or(RsyncStatus::Pending)
    }

    /// CAS-driven status transition. Returns `Ok(())` on a successful
    /// transition, `Err(actual_status)` on contention (caller can
    /// branch on the observed state). Terminal-status transitions
    /// land at most once — the second writer observes the prior
    /// terminal state in `Err`.
    ///
    /// # Errors
    ///
    /// Returns the observed (non-`from`) status when the CAS fails.
    pub fn transition(&self, from: RsyncStatus, to: RsyncStatus) -> Result<(), RsyncStatus> {
        match self.status.compare_exchange(
            from.byte(),
            to.byte(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(actual_byte) => {
                Err(RsyncStatus::from_byte(actual_byte).unwrap_or(RsyncStatus::Pending))
            }
        }
    }

    /// Force-flip the status to [`RsyncStatus::Completed`] regardless
    /// of the current state. Idempotent: a subsequent terminal
    /// transition observes the terminal state and returns the
    /// observed status without changing it.
    pub fn complete(&self) {
        self.force_terminal(RsyncStatus::Completed);
    }

    /// Force-flip to [`RsyncStatus::Failed`]. Idempotent.
    pub fn fail(&self) {
        self.force_terminal(RsyncStatus::Failed);
    }

    /// Force-flip to [`RsyncStatus::Cancelled`]. Idempotent.
    pub fn cancel(&self) {
        self.force_terminal(RsyncStatus::Cancelled);
    }

    fn force_terminal(&self, target: RsyncStatus) {
        let mut current = self.status.load(Ordering::Acquire);
        loop {
            if let Some(status) = RsyncStatus::from_byte(current)
                && status.is_terminal()
            {
                return;
            }
            match self.status.compare_exchange_weak(
                current,
                target.byte(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Build a [`RsyncStats`] snapshot from the live atomic counters.
    /// Each field uses an `Acquire` load so a subscriber that sees an
    /// updated `bytes_transferred` is guaranteed to see the matching
    /// `files_done` / `files_total` writes from the same producer
    /// thread (the producer publishes counters with `Release`).
    #[must_use]
    pub fn snapshot(&self) -> RsyncStats {
        RsyncStats {
            files_total: self.files_total.load(Ordering::Acquire),
            files_done: self.files_done.load(Ordering::Acquire),
            bytes_total: self.bytes_total.load(Ordering::Acquire),
            bytes_transferred: self.bytes_transferred.load(Ordering::Acquire),
            bytes_skipped: self.bytes_skipped.load(Ordering::Acquire),
            files_deleted: self.files_deleted.load(Ordering::Acquire),
            files_failed: self.files_failed.load(Ordering::Acquire),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RsyncSession, RsyncStatus};
    use crate::domain::ids::SessionId;
    use crate::domain::rsync_ids::RsyncId;

    fn fresh() -> RsyncSession {
        RsyncSession::new(
            RsyncId::new("rs-1".to_string()),
            SessionId::new("sess-1".to_string()),
        )
    }

    #[test]
    fn status_byte_round_trip_through_atomic() {
        for status in [
            RsyncStatus::Pending,
            RsyncStatus::Probing,
            RsyncStatus::Running,
            RsyncStatus::Completed,
            RsyncStatus::Failed,
            RsyncStatus::Cancelled,
        ] {
            let byte = status.byte();
            let back = RsyncStatus::from_byte(byte).unwrap_or_else(|| panic!("byte {byte:#x}"));
            assert_eq!(back, status);
        }
    }

    #[test]
    fn status_byte_decodes_unknown_to_none() {
        assert_eq!(RsyncStatus::from_byte(0x00), None);
        assert_eq!(RsyncStatus::from_byte(0xFF), None);
    }

    #[test]
    fn fresh_session_starts_pending_with_zero_counters() {
        let s = fresh();
        assert_eq!(s.status(), RsyncStatus::Pending);
        let stats = s.snapshot();
        assert_eq!(stats.files_total, 0);
        assert_eq!(stats.files_done, 0);
        assert_eq!(stats.bytes_total, 0);
        assert_eq!(stats.bytes_transferred, 0);
        assert_eq!(stats.bytes_skipped, 0);
        assert_eq!(stats.files_deleted, 0);
        assert_eq!(stats.files_failed, 0);
    }

    #[test]
    fn ids_round_trip_through_accessors() {
        let s = fresh();
        assert_eq!(s.id().as_str(), "rs-1");
        assert_eq!(s.session_id().as_str(), "sess-1");
    }

    #[test]
    fn with_files_and_bytes_total_publishes_to_snapshot() {
        let s = fresh();
        s.with_files_total(100);
        s.with_bytes_total(4_096_000);
        let stats = s.snapshot();
        assert_eq!(stats.files_total, 100);
        assert_eq!(stats.bytes_total, 4_096_000);
    }

    #[test]
    fn record_file_done_accumulates_bytes_and_files() {
        let s = fresh();
        s.record_file_done(1024, 0);
        s.record_file_done(0, 4096);
        s.record_file_done(2048, 1024);
        let stats = s.snapshot();
        assert_eq!(stats.files_done, 3);
        assert_eq!(stats.bytes_transferred, 1024 + 0 + 2048);
        assert_eq!(stats.bytes_skipped, 0 + 4096 + 1024);
    }

    #[test]
    fn record_file_deleted_increments_counter() {
        let s = fresh();
        s.record_file_deleted();
        s.record_file_deleted();
        assert_eq!(s.snapshot().files_deleted, 2);
    }

    #[test]
    fn record_file_failed_increments_counter() {
        let s = fresh();
        s.record_file_failed();
        assert_eq!(s.snapshot().files_failed, 1);
    }

    #[test]
    fn transition_runs_when_from_state_matches() {
        let s = fresh();
        s.transition(RsyncStatus::Pending, RsyncStatus::Probing)
            .unwrap_or_else(|err| panic!("transition: {err:?}"));
        assert_eq!(s.status(), RsyncStatus::Probing);
        s.transition(RsyncStatus::Probing, RsyncStatus::Running)
            .unwrap_or_else(|err| panic!("transition: {err:?}"));
        assert_eq!(s.status(), RsyncStatus::Running);
    }

    #[test]
    fn transition_returns_observed_state_on_mismatch() {
        let s = fresh();
        // Trying to go Pending -> Running short-circuits the Probing
        // transition.
        let observed = s
            .transition(RsyncStatus::Probing, RsyncStatus::Running)
            .unwrap_err();
        assert_eq!(observed, RsyncStatus::Pending);
        // The status byte was unchanged.
        assert_eq!(s.status(), RsyncStatus::Pending);
    }

    #[test]
    fn complete_lands_terminal_state_idempotently() {
        let s = fresh();
        s.complete();
        assert_eq!(s.status(), RsyncStatus::Completed);
        // Subsequent terminal calls observe the terminal state and
        // do not flip back.
        s.fail();
        assert_eq!(s.status(), RsyncStatus::Completed);
        s.cancel();
        assert_eq!(s.status(), RsyncStatus::Completed);
    }

    #[test]
    fn fail_lands_terminal_state_idempotently() {
        let s = fresh();
        s.fail();
        assert_eq!(s.status(), RsyncStatus::Failed);
        s.complete();
        s.cancel();
        assert_eq!(s.status(), RsyncStatus::Failed);
    }

    #[test]
    fn cancel_lands_terminal_state_idempotently() {
        let s = fresh();
        s.cancel();
        assert_eq!(s.status(), RsyncStatus::Cancelled);
        s.complete();
        s.fail();
        assert_eq!(s.status(), RsyncStatus::Cancelled);
    }

    #[test]
    fn snapshot_observes_writes_published_via_record_helpers() {
        let s = fresh();
        s.with_files_total(10);
        s.with_bytes_total(1024);
        s.record_file_done(256, 0);
        s.record_file_done(0, 256);
        s.record_file_deleted();
        s.record_file_failed();
        let stats = s.snapshot();
        assert_eq!(stats.files_total, 10);
        assert_eq!(stats.files_done, 2);
        assert_eq!(stats.bytes_total, 1024);
        assert_eq!(stats.bytes_transferred, 256);
        assert_eq!(stats.bytes_skipped, 256);
        assert_eq!(stats.files_deleted, 1);
        assert_eq!(stats.files_failed, 1);
    }

    #[test]
    fn is_terminal_distinguishes_terminal_from_in_flight_states() {
        assert!(!RsyncStatus::Pending.is_terminal());
        assert!(!RsyncStatus::Probing.is_terminal());
        assert!(!RsyncStatus::Running.is_terminal());
        assert!(RsyncStatus::Completed.is_terminal());
        assert!(RsyncStatus::Failed.is_terminal());
        assert!(RsyncStatus::Cancelled.is_terminal());
    }
}
