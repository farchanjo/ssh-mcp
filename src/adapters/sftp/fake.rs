//! Test-only [`SftpClientPort`] adapter with scripted, deterministic outcomes.
//!
//! [`FakeSftpClient`] records every call (`upload`, `download`, `cancel`)
//! into a shared log so tests can assert the orchestration issued exactly
//! the operations expected. Outcomes are scripted per call kind via FIFO
//! queues:
//!
//! - **Upload outcomes**: pushed via [`FakeSftpClient::queue_upload_ok`] /
//!   [`FakeSftpClient::queue_upload_error`] in FIFO order. Each `upload`
//!   call pops the head; an empty queue defaults to a success that mints
//!   a snapshot from the supplied request with `total_bytes = 0`.
//! - **Download outcomes**: pushed via [`FakeSftpClient::queue_download_ok`] /
//!   [`FakeSftpClient::queue_download_error`] in FIFO order. Each `download`
//!   call pops the head; an empty queue defaults to a success that mints a
//!   snapshot with `total_bytes = 0`.
//! - **Cancel outcomes**: pushed via [`FakeSftpClient::queue_cancel_ok`] /
//!   [`FakeSftpClient::queue_cancel_error`] in FIFO order. Each `cancel`
//!   call pops the head; an empty queue defaults to success.
//!
//! The fake mints fresh [`TransferEntity`] snapshots from the supplied
//! request data so callers only need to seed total-byte sizing and error
//! variants. Successful uploads / downloads echo the request's
//! `local_path` and `remote_path` verbatim, mirroring how the production
//! adapter populates the entity once `resolve_local_path` runs.
//!
//! The whole module is gated behind `#[cfg(any(test, feature = "test-fixtures"))]`
//! so it never reaches a release binary.

#![allow(
    dead_code,
    reason = "scripted helpers may be exercised selectively per test scenario"
)]

use std::sync::Arc;
use std::sync::Mutex;

use chrono::Utc;

use crate::domain::error::DomainError;
use crate::domain::ids::TransferId;
use crate::domain::transfer::{TransferDirection, TransferEntity};
use crate::ports::sftp_client::{DownloadRequest, SftpClientPort, UploadRequest};

/// Single recorded interaction for assertion purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeSftpCall {
    /// `upload(transfer_id, request)` was invoked. The fake captures the
    /// id together with the verbatim request so tests can assert routing
    /// without owning a copy of the request fields.
    Upload {
        /// Use-case-minted transfer id.
        transfer_id: TransferId,
        /// Verbatim request DTO handed to the port.
        request: UploadRequest,
    },
    /// `download(transfer_id, request)` was invoked.
    Download {
        /// Use-case-minted transfer id.
        transfer_id: TransferId,
        /// Verbatim request DTO handed to the port.
        request: DownloadRequest,
    },
    /// `cancel(transfer_id)` was invoked.
    Cancel {
        /// Transfer that was cancelled.
        transfer_id: TransferId,
    },
}

/// Scripted outcome for an `upload` call. The fake derives the returned
/// [`TransferEntity`] from the request and the `total_bytes` value handed
/// here so tests do not have to assemble a full entity per scenario.
#[derive(Debug, Clone)]
enum UploadOutcome {
    /// `upload` succeeds and reports the supplied total byte count.
    Ok {
        /// Total bytes the snapshot reports back to the caller.
        total_bytes: u64,
    },
    /// `upload` fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for a `download` call. Mirrors [`UploadOutcome`].
#[derive(Debug, Clone)]
enum DownloadOutcome {
    /// `download` succeeds and reports the supplied total byte count.
    Ok {
        /// Total bytes the snapshot reports back to the caller.
        total_bytes: u64,
    },
    /// `download` fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for a `cancel` call.
#[derive(Debug, Clone)]
enum CancelOutcome {
    /// `cancel` succeeds.
    Ok,
    /// `cancel` fails with the supplied domain error.
    Err(DomainError),
}

/// Test [`SftpClientPort`] adapter. Cloneable; clones share the same
/// scripted state and the same call log via [`Arc`].
#[derive(Debug, Clone, Default)]
pub struct FakeSftpClient {
    inner: Arc<FakeSftpClientInner>,
}

#[derive(Debug, Default)]
struct FakeSftpClientInner {
    /// Scripted `upload` outcomes, popped FIFO.
    upload_queue: Mutex<Vec<UploadOutcome>>,
    /// Scripted `download` outcomes, popped FIFO.
    download_queue: Mutex<Vec<DownloadOutcome>>,
    /// Scripted `cancel` outcomes, popped FIFO.
    cancel_queue: Mutex<Vec<CancelOutcome>>,
    /// Append-only log of every recorded call.
    calls: Mutex<Vec<FakeSftpCall>>,
}

impl FakeSftpClient {
    /// Build an empty fake. Defaults: every `upload` and `download`
    /// succeed with `total_bytes = 0`; every `cancel` succeeds.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a successful `upload` outcome with the supplied total byte
    /// count. Tests use this both to model arbitrary file sizes and to
    /// chain consecutive uploads without worrying about queue exhaustion
    /// (an empty queue defaults to `total_bytes = 0`).
    pub fn queue_upload_ok(&self, total_bytes: u64) {
        Self::push(&self.inner.upload_queue, UploadOutcome::Ok { total_bytes });
    }

    /// Queue a failed `upload` outcome with the supplied domain error.
    pub fn queue_upload_error(&self, error: DomainError) {
        Self::push(&self.inner.upload_queue, UploadOutcome::Err(error));
    }

    /// Queue a successful `download` outcome with the supplied total byte
    /// count.
    pub fn queue_download_ok(&self, total_bytes: u64) {
        Self::push(
            &self.inner.download_queue,
            DownloadOutcome::Ok { total_bytes },
        );
    }

    /// Queue a failed `download` outcome with the supplied domain error.
    pub fn queue_download_error(&self, error: DomainError) {
        Self::push(&self.inner.download_queue, DownloadOutcome::Err(error));
    }

    /// Queue a successful `cancel` outcome.
    pub fn queue_cancel_ok(&self) {
        Self::push(&self.inner.cancel_queue, CancelOutcome::Ok);
    }

    /// Queue a failed `cancel` outcome with the supplied domain error.
    pub fn queue_cancel_error(&self, error: DomainError) {
        Self::push(&self.inner.cancel_queue, CancelOutcome::Err(error));
    }

    /// Snapshot of every recorded call in invocation order.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeSftpCall> {
        self.inner
            .calls
            .lock()
            .map_or_else(|poison| poison.into_inner().clone(), |guard| guard.clone())
    }

    /// Number of recorded calls.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.inner
            .calls
            .lock()
            .map_or_else(|poison| poison.into_inner().len(), |guard| guard.len())
    }

    fn push<T>(slot: &Mutex<Vec<T>>, value: T) {
        if let Ok(mut guard) = slot.lock() {
            guard.push(value);
        }
    }

    fn record(&self, call: FakeSftpCall) {
        if let Ok(mut log) = self.inner.calls.lock() {
            log.push(call);
        }
    }

    fn pop_upload_outcome(&self) -> UploadOutcome {
        self.inner.upload_queue.lock().map_or_else(
            |_| UploadOutcome::Ok { total_bytes: 0 },
            |mut guard| {
                if guard.is_empty() {
                    UploadOutcome::Ok { total_bytes: 0 }
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_download_outcome(&self) -> DownloadOutcome {
        self.inner.download_queue.lock().map_or_else(
            |_| DownloadOutcome::Ok { total_bytes: 0 },
            |mut guard| {
                if guard.is_empty() {
                    DownloadOutcome::Ok { total_bytes: 0 }
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_cancel_outcome(&self) -> CancelOutcome {
        self.inner.cancel_queue.lock().map_or_else(
            |_| CancelOutcome::Ok,
            |mut guard| {
                if guard.is_empty() {
                    CancelOutcome::Ok
                } else {
                    guard.remove(0)
                }
            },
        )
    }
}

impl SftpClientPort for FakeSftpClient {
    async fn upload(
        &self,
        transfer_id: TransferId,
        request: UploadRequest,
    ) -> Result<TransferEntity, DomainError> {
        self.record(FakeSftpCall::Upload {
            transfer_id: transfer_id.clone(),
            request: request.clone(),
        });
        match self.pop_upload_outcome() {
            UploadOutcome::Ok { total_bytes } => Ok(TransferEntity::new(
                transfer_id,
                request.session_id,
                TransferDirection::Upload,
                request.local_path,
                request.remote_path,
                Utc::now(),
                total_bytes,
            )),
            UploadOutcome::Err(error) => Err(error),
        }
    }

    async fn download(
        &self,
        transfer_id: TransferId,
        request: DownloadRequest,
    ) -> Result<TransferEntity, DomainError> {
        self.record(FakeSftpCall::Download {
            transfer_id: transfer_id.clone(),
            request: request.clone(),
        });
        match self.pop_download_outcome() {
            DownloadOutcome::Ok { total_bytes } => Ok(TransferEntity::new(
                transfer_id,
                request.session_id,
                TransferDirection::Download,
                request.local_path,
                request.remote_path,
                Utc::now(),
                total_bytes,
            )),
            DownloadOutcome::Err(error) => Err(error),
        }
    }

    async fn cancel(&self, transfer_id: &TransferId) -> Result<(), DomainError> {
        self.record(FakeSftpCall::Cancel {
            transfer_id: transfer_id.clone(),
        });
        match self.pop_cancel_outcome() {
            CancelOutcome::Ok => Ok(()),
            CancelOutcome::Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FakeSftpCall, FakeSftpClient};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, TransferId};
    use crate::domain::transfer::{TransferDirection, TransferStatus};
    use crate::ports::sftp_client::{DownloadRequest, SftpClientPort, UploadRequest};

    fn upload_request(session: &str) -> UploadRequest {
        UploadRequest {
            session_id: SessionId::new(session.to_string()),
            local_path: "/tmp/source.bin".to_string(),
            remote_path: "/srv/dest.bin".to_string(),
        }
    }

    fn download_request(session: &str) -> DownloadRequest {
        DownloadRequest {
            session_id: SessionId::new(session.to_string()),
            remote_path: "/srv/source.bin".to_string(),
            local_path: "/tmp/dest.bin".to_string(),
        }
    }

    #[tokio::test]
    async fn upload_default_returns_running_snapshot_with_zero_total() {
        let fake = FakeSftpClient::new();
        let entity = fake
            .upload(TransferId::new("t-1".to_string()), upload_request("s-1"))
            .await
            .expect("upload ok");
        assert_eq!(entity.status, TransferStatus::Running);
        assert_eq!(entity.total_bytes, 0);
        assert_eq!(entity.bytes_transferred, 0);
        assert_eq!(entity.direction, TransferDirection::Upload);
        assert_eq!(entity.local_path, "/tmp/source.bin");
        assert_eq!(entity.remote_path, "/srv/dest.bin");
    }

    #[tokio::test]
    async fn queued_upload_total_bytes_round_trips_into_entity() {
        let fake = FakeSftpClient::new();
        fake.queue_upload_ok(4096);
        let entity = fake
            .upload(TransferId::new("t-1".to_string()), upload_request("s-1"))
            .await
            .expect("upload ok");
        assert_eq!(entity.total_bytes, 4096);
    }

    #[tokio::test]
    async fn queued_upload_error_is_returned() {
        let fake = FakeSftpClient::new();
        fake.queue_upload_error(DomainError::Sftp("nope".to_string()));
        let err = fake
            .upload(TransferId::new("t-1".to_string()), upload_request("s-1"))
            .await
            .expect_err("expected error");
        assert_eq!(err, DomainError::Sftp("nope".to_string()));
    }

    #[tokio::test]
    async fn download_default_returns_running_snapshot_with_zero_total() {
        let fake = FakeSftpClient::new();
        let entity = fake
            .download(TransferId::new("t-2".to_string()), download_request("s-2"))
            .await
            .expect("download ok");
        assert_eq!(entity.status, TransferStatus::Running);
        assert_eq!(entity.direction, TransferDirection::Download);
        assert_eq!(entity.total_bytes, 0);
    }

    #[tokio::test]
    async fn queued_download_total_bytes_round_trips_into_entity() {
        let fake = FakeSftpClient::new();
        fake.queue_download_ok(8192);
        let entity = fake
            .download(TransferId::new("t-2".to_string()), download_request("s-2"))
            .await
            .expect("download ok");
        assert_eq!(entity.total_bytes, 8192);
    }

    #[tokio::test]
    async fn queued_download_error_is_returned() {
        let fake = FakeSftpClient::new();
        fake.queue_download_error(DomainError::Sftp("metadata failed".to_string()));
        let err = fake
            .download(TransferId::new("t-2".to_string()), download_request("s-2"))
            .await
            .expect_err("expected error");
        assert_eq!(err, DomainError::Sftp("metadata failed".to_string()));
    }

    #[tokio::test]
    async fn cancel_default_succeeds_and_records_call() {
        let fake = FakeSftpClient::new();
        let id = TransferId::new("t-cancel".to_string());
        fake.cancel(&id).await.expect("cancel ok");
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0], FakeSftpCall::Cancel { transfer_id } if transfer_id == &id));
    }

    #[tokio::test]
    async fn fake_records_calls_in_order() {
        let fake = FakeSftpClient::new();
        let _ = fake
            .upload(TransferId::new("t-1".to_string()), upload_request("s-1"))
            .await;
        let _ = fake
            .download(TransferId::new("t-2".to_string()), download_request("s-2"))
            .await;
        let _ = fake.cancel(&TransferId::new("t-3".to_string())).await;
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert!(matches!(&calls[0], FakeSftpCall::Upload { .. }));
        assert!(matches!(&calls[1], FakeSftpCall::Download { .. }));
        assert!(matches!(&calls[2], FakeSftpCall::Cancel { .. }));
    }

    #[test]
    fn fake_is_send_sync_clone() {
        fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
        assert_send_sync_clone::<FakeSftpClient>();
    }

    #[tokio::test]
    async fn cloned_fake_shares_call_log() {
        let fake = FakeSftpClient::new();
        let twin = fake.clone();
        let _ = fake
            .upload(TransferId::new("t-1".to_string()), upload_request("s-1"))
            .await;
        assert_eq!(twin.call_count(), 1);
    }
}
