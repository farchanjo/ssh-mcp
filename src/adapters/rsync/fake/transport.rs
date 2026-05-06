//! Scripted [`RsyncTransportPort`] adapter for use-case tests.
//!
//! Records every call (`start_session`, `recv_event`, `close`) into a
//! shared log so tests can assert the orchestration issued exactly the
//! operations expected. Outcomes are scripted per call kind via
//! lock-free FIFO queues (`DashMap<u64, T>` indexed by atomic head /
//! tail counters).
//!
//! Defaults when a queue is empty:
//! - `start_session` -> `Ok(RsyncStartOutcome { rsync_id: "rs-fake-N",
//!   wire_transport: false })`
//! - `recv_event` -> `Ok(None)` (terminal)
//! - `close` -> `Ok(())`

#![allow(
    dead_code,
    reason = "scripted helpers may be exercised selectively per test scenario"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::adapters::rsync::types::RsyncProgressEvent;
use crate::domain::error::DomainError;
use crate::domain::rsync_ids::RsyncId;
use crate::ports::rsync_transport::{RsyncStartOutcome, RsyncStartRequest, RsyncTransportPort};

/// Single recorded interaction for assertion purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeRsyncTransportCall {
    /// `start_session(request)` was invoked.
    StartSession(RsyncStartRequest),
    /// `recv_event(rsync_id)` was invoked.
    RecvEvent {
        /// Affected session.
        rsync_id: RsyncId,
    },
    /// `close(rsync_id)` was invoked.
    Close {
        /// Affected session.
        rsync_id: RsyncId,
    },
}

/// Scripted `start_session` outcome.
#[derive(Debug, Clone)]
enum StartOutcome {
    Ok(RsyncStartOutcome),
    Err(DomainError),
}

/// Scripted `recv_event` outcome.
#[derive(Debug, Clone)]
enum RecvOutcome {
    /// Yield an event (the lane is open).
    Event(RsyncProgressEvent),
    /// Yield `None` (the lane closed).
    End,
    /// Yield an error.
    Err(DomainError),
}

/// Scripted `close` outcome.
#[derive(Debug, Clone)]
enum CloseOutcome {
    Ok,
    Err(DomainError),
}

/// Lock-free FIFO scripted-queue. `DashMap` indexed by an atomic head
/// / tail pair; `push` appends at `tail`, `pop` removes at `head`. The
/// producer / consumer never share a write hot path.
#[derive(Debug, Default)]
struct AtomicQueue<T> {
    storage: DashMap<u64, T>,
    head: AtomicU64,
    tail: AtomicU64,
}

impl<T> AtomicQueue<T> {
    fn new() -> Self {
        Self {
            storage: DashMap::new(),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
        }
    }

    fn push(&self, value: T) {
        let slot = self.tail.fetch_add(1, Ordering::AcqRel);
        self.storage.insert(slot, value);
    }

    fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        if head >= tail {
            return None;
        }
        let removed = self.storage.remove(&head).map(|(_, v)| v);
        if removed.is_some() {
            self.head.fetch_add(1, Ordering::AcqRel);
        }
        removed
    }

    fn len(&self) -> u64 {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        tail.saturating_sub(head)
    }
}

/// Scripted [`RsyncTransportPort`] adapter. Cloneable — clones share
/// the underlying queues + call log via [`Arc`].
#[derive(Debug, Clone)]
pub struct FakeRsyncTransport {
    inner: Arc<FakeRsyncTransportInner>,
}

#[derive(Debug)]
struct FakeRsyncTransportInner {
    start_queue: AtomicQueue<StartOutcome>,
    recv_queue: AtomicQueue<RecvOutcome>,
    close_queue: AtomicQueue<CloseOutcome>,
    calls: AtomicQueue<FakeRsyncTransportCall>,
    /// Synthesised id used when the `start_session` queue is empty.
    /// Atomic so cloned fakes mint distinct ids if no script runs.
    auto_id_counter: AtomicU64,
}

impl Default for FakeRsyncTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRsyncTransport {
    /// Build an empty fake.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(FakeRsyncTransportInner {
                start_queue: AtomicQueue::new(),
                recv_queue: AtomicQueue::new(),
                close_queue: AtomicQueue::new(),
                calls: AtomicQueue::new(),
                auto_id_counter: AtomicU64::new(0),
            }),
        }
    }

    /// Queue a successful `start_session` outcome.
    pub fn queue_start_ok(&self, rsync_id: RsyncId, wire_transport: bool) {
        self.inner
            .start_queue
            .push(StartOutcome::Ok(RsyncStartOutcome {
                rsync_id,
                wire_transport,
            }));
    }

    /// Queue a failed `start_session` outcome.
    pub fn queue_start_error(&self, error: DomainError) {
        self.inner.start_queue.push(StartOutcome::Err(error));
    }

    /// Queue a `recv_event` -> Ok(Some(event)) outcome.
    pub fn queue_recv_event(&self, event: RsyncProgressEvent) {
        self.inner.recv_queue.push(RecvOutcome::Event(event));
    }

    /// Queue a `recv_event` -> Ok(None) terminal outcome.
    pub fn queue_recv_end(&self) {
        self.inner.recv_queue.push(RecvOutcome::End);
    }

    /// Queue a failed `recv_event` outcome.
    pub fn queue_recv_error(&self, error: DomainError) {
        self.inner.recv_queue.push(RecvOutcome::Err(error));
    }

    /// Queue a successful `close` outcome.
    pub fn queue_close_ok(&self) {
        self.inner.close_queue.push(CloseOutcome::Ok);
    }

    /// Queue a failed `close` outcome.
    pub fn queue_close_error(&self, error: DomainError) {
        self.inner.close_queue.push(CloseOutcome::Err(error));
    }

    /// Snapshot of every recorded call in invocation order.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeRsyncTransportCall> {
        let mut out = Vec::new();
        while let Some(call) = self.inner.calls.pop() {
            out.push(call);
        }
        out
    }

    /// Number of recorded calls (without draining the log).
    #[must_use]
    pub fn call_count(&self) -> usize {
        usize::try_from(self.inner.calls.len()).unwrap_or(usize::MAX)
    }

    fn record(&self, call: FakeRsyncTransportCall) {
        self.inner.calls.push(call);
    }

    fn synth_rsync_id(&self) -> RsyncId {
        let n = self.inner.auto_id_counter.fetch_add(1, Ordering::AcqRel);
        RsyncId::new(format!("rs-fake-{n}"))
    }
}

impl RsyncTransportPort for FakeRsyncTransport {
    async fn start_session(
        &self,
        request: RsyncStartRequest,
    ) -> Result<RsyncStartOutcome, DomainError> {
        self.record(FakeRsyncTransportCall::StartSession(request));
        match self.inner.start_queue.pop() {
            Some(StartOutcome::Ok(outcome)) => Ok(outcome),
            Some(StartOutcome::Err(err)) => Err(err),
            None => Ok(RsyncStartOutcome {
                rsync_id: self.synth_rsync_id(),
                wire_transport: false,
            }),
        }
    }

    async fn recv_event(
        &self,
        rsync_id: &RsyncId,
    ) -> Result<Option<RsyncProgressEvent>, DomainError> {
        self.record(FakeRsyncTransportCall::RecvEvent {
            rsync_id: rsync_id.clone(),
        });
        match self.inner.recv_queue.pop() {
            Some(RecvOutcome::Event(event)) => Ok(Some(event)),
            Some(RecvOutcome::End) | None => Ok(None),
            Some(RecvOutcome::Err(err)) => Err(err),
        }
    }

    async fn close(&self, rsync_id: &RsyncId) -> Result<(), DomainError> {
        self.record(FakeRsyncTransportCall::Close {
            rsync_id: rsync_id.clone(),
        });
        match self.inner.close_queue.pop() {
            Some(CloseOutcome::Ok) | None => Ok(()),
            Some(CloseOutcome::Err(err)) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FakeRsyncTransport;
    use crate::adapters::rsync::types::{RsyncProgressEvent, RsyncTransportKind};
    use crate::domain::error::DomainError;
    use crate::domain::ids::SessionId;
    use crate::domain::rsync_ids::RsyncId;
    use crate::ports::rsync_transport::{RsyncDirection, RsyncStartRequest, RsyncTransportPort};

    fn start_request() -> RsyncStartRequest {
        RsyncStartRequest {
            session_id: SessionId::new("s-1".to_string()),
            src: "/tmp/src".to_string(),
            dst: "/tmp/dst".to_string(),
            direction: RsyncDirection::Push,
            ..RsyncStartRequest::default()
        }
    }

    #[tokio::test]
    async fn empty_queue_synthesises_default_outcome() {
        let fake = FakeRsyncTransport::new();
        let outcome = fake.start_session(start_request()).await.expect("start ok");
        assert!(outcome.rsync_id.as_str().starts_with("rs-fake-"));
        assert!(!outcome.wire_transport);
    }

    #[tokio::test]
    async fn queued_start_outcome_pops_in_fifo_order() {
        let fake = FakeRsyncTransport::new();
        fake.queue_start_ok(RsyncId::new("rs-A".to_string()), true);
        fake.queue_start_ok(RsyncId::new("rs-B".to_string()), false);
        let first = fake.start_session(start_request()).await.expect("ok");
        let second = fake.start_session(start_request()).await.expect("ok");
        assert_eq!(first.rsync_id.as_str(), "rs-A");
        assert!(first.wire_transport);
        assert_eq!(second.rsync_id.as_str(), "rs-B");
        assert!(!second.wire_transport);
    }

    #[tokio::test]
    async fn queued_start_error_is_returned() {
        let fake = FakeRsyncTransport::new();
        fake.queue_start_error(DomainError::RsyncProtocolError("boom".to_string()));
        let err = fake.start_session(start_request()).await.expect_err("err");
        assert!(matches!(err, DomainError::RsyncProtocolError(_)));
    }

    #[tokio::test]
    async fn recv_event_yields_queued_event_then_terminal() {
        let fake = FakeRsyncTransport::new();
        let id = RsyncId::new("rs-1".to_string());
        let event = RsyncProgressEvent::SessionStarted {
            transport: RsyncTransportKind::Wire,
            files_planned: 1,
            bytes_planned: 1024,
        };
        fake.queue_recv_event(event.clone());
        fake.queue_recv_end();
        let first = fake.recv_event(&id).await.expect("ok");
        let second = fake.recv_event(&id).await.expect("ok");
        assert_eq!(first, Some(event));
        assert_eq!(second, None);
    }

    #[tokio::test]
    async fn close_default_succeeds() {
        let fake = FakeRsyncTransport::new();
        let id = RsyncId::new("rs-1".to_string());
        fake.close(&id).await.expect("close ok");
    }

    #[tokio::test]
    async fn close_error_is_returned() {
        let fake = FakeRsyncTransport::new();
        let id = RsyncId::new("rs-1".to_string());
        fake.queue_close_error(DomainError::RsyncProtocolError("boom".to_string()));
        let err = fake.close(&id).await.expect_err("err");
        assert!(matches!(err, DomainError::RsyncProtocolError(_)));
    }

    #[tokio::test]
    async fn cloned_fake_shares_call_log() {
        let fake = FakeRsyncTransport::new();
        let twin = fake.clone();
        let _ = fake.start_session(start_request()).await;
        // Call count is observed across clones; popping the log via
        // calls() drains it for both views.
        assert_eq!(twin.call_count(), 1);
    }

    #[test]
    fn fake_is_send_sync_clone() {
        fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
        assert_send_sync_clone::<FakeRsyncTransport>();
    }
}
