//! Use case: snapshot the progress of a single SFTP transfer.
//!
//! Translates the v3 `ssh_get_transfer_progress_impl` (from
//! `src/mcp/tools/sftp.rs`) into a hexagonal use case driven by a single
//! port. No rmcp / russh / russh-sftp / tokio types leak into the public
//! surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the transfer entity via [`TransferRepository::get`]. A missing
//!    id short-circuits with [`DomainError::TransferNotFound`].
//! 2. When [`GetTransferProgressRequest::wait`] is `false`, build the
//!    snapshot from the freshly fetched entity and return.
//! 3. When [`GetTransferProgressRequest::wait`] is `true`, poll the repo at
//!    [`POLL_INTERVAL`] cadence until either the transfer reaches a terminal
//!    status (`Completed`, `Failed`, `Cancelled`) or the wait deadline
//!    expires. The deadline defaults to [`DEFAULT_WAIT_TIMEOUT`] and is
//!    capped at [`MAX_WAIT_TIMEOUT`] — values above the cap are clamped down
//!    transparently rather than rejected, mirroring v3's `min(300)`
//!    semantics.
//!
//! Whatever the exit branch, the result captures the most recent snapshot
//! the use case observed (running snapshot on timeout, terminal snapshot on
//! completion). Subscribers on `transfer://<id>/progress` receive the same
//! authoritative state through the subscription registry; this use case
//! exists so callers that prefer point-in-time polling do not have to pay
//! for an MCP subscription.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use
//! case clones small handles ([`Arc`]) into local bindings before entering
//! await points. The polling loop yields via [`tokio::time::sleep`] so the
//! executor stays responsive even when the transfer never produces new
//! state.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, sleep};

use crate::domain::error::DomainError;
use crate::domain::ids::TransferId;
use crate::domain::transfer::{TransferDirection, TransferEntity, TransferStatus};
use crate::ports::transfer_repo::TransferRepository;

/// Cooperative polling cadence used by the long-poll loop. Matches the v3
/// notify wake-up rhythm without leaking the v3 `Notify` type into the
/// port layer.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Default wait budget when the caller does not supply one. Mirrors the v3
/// default of 30 s.
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard cap on the wait budget. Mirrors the v3 `min(300)` clamp.
const MAX_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct GetTransferProgressRequest {
    /// Transfer to snapshot.
    pub transfer_id: TransferId,
    /// `true` to block until the transfer reaches a terminal state or the
    /// wait deadline expires.
    pub wait: bool,
    /// Optional wait deadline. `None` falls back to
    /// [`DEFAULT_WAIT_TIMEOUT`]; values above [`MAX_WAIT_TIMEOUT`] are
    /// clamped down transparently.
    pub wait_timeout: Option<Duration>,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetTransferProgressResult {
    /// Echoed back so the caller can correlate without recreating the
    /// request.
    pub transfer_id: TransferId,
    /// Direction of the transfer (`upload` / `download`).
    pub direction: TransferDirection,
    /// Lifecycle state observed at snapshot time.
    pub status: TransferStatus,
    /// Bytes transferred so far.
    pub bytes_transferred: u64,
    /// Total bytes the transfer will move (snapshot at start time).
    pub total_bytes: u64,
    /// Optional terminal failure description (set when `status` is `Failed`).
    pub error: Option<String>,
    /// Latest sequence number the use case observed. The current
    /// [`TransferRepository`] surface does not expose a per-transfer event
    /// counter so this field is always `0` in the v4 hexagonal layout —
    /// the field is preserved for forward compatibility with the v3 wire
    /// shape and will start carrying a real value once the
    /// `transfer://<id>/progress` event stream is plumbed through the
    /// subscription registry's `current_seq` accessor.
    pub last_seq: u64,
}

/// Get-transfer-progress use case generic over a single port.
#[derive(Debug)]
pub struct GetTransferProgressUseCase<TR>
where
    TR: TransferRepository + Send + Sync,
{
    transfers: Arc<TR>,
}

impl<TR> GetTransferProgressUseCase<TR>
where
    TR: TransferRepository + Send + Sync,
{
    /// Wire the use case from an already-shared adapter handle.
    #[must_use]
    pub const fn new(transfers: Arc<TR>) -> Self {
        Self { transfers }
    }

    /// Borrow the shared transfer-repository handle.
    ///
    /// The inbound rmcp layer uses this to interleave per-tick lookups
    /// with the use case future when emitting bounded
    /// `notifications/progress` notifications during a `wait=true`
    /// poll. The use case body itself does not need an accessor (it
    /// already owns the same `Arc`); the helper is purely
    /// inbound-facing.
    #[must_use]
    pub const fn transfers(&self) -> &Arc<TR> {
        &self.transfers
    }

    /// Drive the orchestration. See module-level docs for the step-by-step
    /// semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::TransferNotFound`] when the id is unknown to the
    ///   repository at probe time.
    /// - Any [`DomainError`] surfaced by [`TransferRepository::get`] during
    ///   the poll loop.
    pub async fn execute(
        &self,
        req: GetTransferProgressRequest,
    ) -> Result<GetTransferProgressResult, DomainError> {
        let initial = self.fetch_required(&req.transfer_id).await?;
        let final_entity = if req.wait && !is_terminal(initial.status) {
            self.long_poll(&req).await?
        } else {
            initial
        };
        Ok(snapshot_to_result(final_entity))
    }

    /// Fetch the required transfer entity, mapping `Ok(None)` to
    /// [`DomainError::TransferNotFound`].
    async fn fetch_required(
        &self,
        transfer_id: &TransferId,
    ) -> Result<TransferEntity, DomainError> {
        self.transfers
            .get(transfer_id)
            .await?
            .ok_or_else(|| DomainError::TransferNotFound(transfer_id.clone()))
    }

    /// Block until the transfer reaches a terminal state or the wait
    /// deadline expires. Returns whichever entity the loop observed last.
    async fn long_poll(
        &self,
        req: &GetTransferProgressRequest,
    ) -> Result<TransferEntity, DomainError> {
        let deadline = compute_deadline(req.wait_timeout);
        let start = Instant::now();
        loop {
            let entity = self.fetch_required(&req.transfer_id).await?;
            if is_terminal(entity.status) {
                return Ok(entity);
            }
            if start.elapsed() >= deadline {
                return Ok(entity);
            }
            sleep(POLL_INTERVAL).await;
        }
    }
}

/// Resolve the wait deadline from the optional caller-supplied value.
/// Floors at zero (which collapses to a single fetch) and caps at
/// [`MAX_WAIT_TIMEOUT`]. Pulled out so the polling loop stays narrative.
fn compute_deadline(requested: Option<Duration>) -> Duration {
    let raw = requested.unwrap_or(DEFAULT_WAIT_TIMEOUT);
    if raw > MAX_WAIT_TIMEOUT {
        MAX_WAIT_TIMEOUT
    } else {
        raw
    }
}

/// `true` when the supplied status is terminal (no further state changes
/// expected). Pulled out so the wait-eligibility check and the loop exit
/// share the same definition.
const fn is_terminal(status: TransferStatus) -> bool {
    matches!(
        status,
        TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled
    )
}

/// Convert a [`TransferEntity`] snapshot into a [`GetTransferProgressResult`].
/// Pure projection — easier to test in isolation than the full orchestration.
fn snapshot_to_result(entity: TransferEntity) -> GetTransferProgressResult {
    GetTransferProgressResult {
        transfer_id: entity.id,
        direction: entity.direction,
        status: entity.status,
        bytes_transferred: entity.bytes_transferred,
        total_bytes: entity.total_bytes,
        error: entity.error,
        last_seq: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GetTransferProgressRequest, GetTransferProgressUseCase, MAX_WAIT_TIMEOUT, compute_deadline,
        snapshot_to_result,
    };
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, TransferId};
    use crate::domain::transfer::{TransferDirection, TransferEntity, TransferStatus};
    use crate::ports::transfer_repo::TransferRepository;
    use chrono::Utc;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::sleep;

    type UseCaseUnderTest = GetTransferProgressUseCase<DashMapTransferRepo>;

    struct Wired {
        uc: UseCaseUnderTest,
        transfers: Arc<DashMapTransferRepo>,
    }

    fn build_wired() -> Wired {
        let transfers = Arc::new(DashMapTransferRepo::new());
        let uc = GetTransferProgressUseCase::new(Arc::clone(&transfers));
        Wired { uc, transfers }
    }

    fn entity(
        id: &str,
        session: &str,
        direction: TransferDirection,
        status: TransferStatus,
        bytes: u64,
        total: u64,
    ) -> TransferEntity {
        let mut e = TransferEntity::new(
            TransferId::new(id.to_string()),
            SessionId::new(session.to_string()),
            direction,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            total,
        );
        e.status = status;
        e.bytes_transferred = bytes;
        e
    }

    async fn seed(repo: &DashMapTransferRepo, e: TransferEntity) {
        repo.insert(e).await.expect("seed insert");
    }

    fn base_request(id: &str) -> GetTransferProgressRequest {
        GetTransferProgressRequest {
            transfer_id: TransferId::new(id.to_string()),
            wait: false,
            wait_timeout: None,
        }
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_mode_returns_running_state_without_polling() {
        let wired = build_wired();
        seed(
            &wired.transfers,
            entity(
                "t-run",
                "s-1",
                TransferDirection::Upload,
                TransferStatus::Running,
                512,
                2048,
            ),
        )
        .await;
        let result = wired
            .uc
            .execute(base_request("t-run"))
            .await
            .expect("snapshot ok");
        assert_eq!(result.status, TransferStatus::Running);
        assert_eq!(result.bytes_transferred, 512);
        assert_eq!(result.total_bytes, 2048);
        assert_eq!(result.direction, TransferDirection::Upload);
        assert_eq!(result.error, None);
        assert_eq!(result.last_seq, 0);
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_transfer_short_circuits_with_transfer_not_found() {
        let wired = build_wired();
        let id = TransferId::new("ghost".to_string());
        let err = wired
            .uc
            .execute(GetTransferProgressRequest {
                transfer_id: id.clone(),
                wait: false,
                wait_timeout: None,
            })
            .await
            .expect_err("must fail");
        assert_eq!(err, DomainError::TransferNotFound(id));
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_mode_returns_terminal_completed_state() {
        let wired = build_wired();
        seed(
            &wired.transfers,
            entity(
                "t-done",
                "s-1",
                TransferDirection::Download,
                TransferStatus::Completed,
                1024,
                1024,
            ),
        )
        .await;
        let result = wired
            .uc
            .execute(base_request("t-done"))
            .await
            .expect("snapshot ok");
        assert_eq!(result.status, TransferStatus::Completed);
        assert_eq!(result.bytes_transferred, 1024);
        assert_eq!(result.total_bytes, 1024);
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_transfer_surfaces_error_message() {
        let wired = build_wired();
        let mut e = entity(
            "t-fail",
            "s-1",
            TransferDirection::Upload,
            TransferStatus::Failed,
            128,
            1024,
        );
        e.error = Some("disk full".to_string());
        seed(&wired.transfers, e).await;
        let result = wired
            .uc
            .execute(base_request("t-fail"))
            .await
            .expect("snapshot ok");
        assert_eq!(result.status, TransferStatus::Failed);
        assert_eq!(result.error.as_deref(), Some("disk full"));
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_transfer_surfaces_cancelled_status() {
        let wired = build_wired();
        seed(
            &wired.transfers,
            entity(
                "t-cancel",
                "s-1",
                TransferDirection::Download,
                TransferStatus::Cancelled,
                64,
                512,
            ),
        )
        .await;
        let result = wired
            .uc
            .execute(base_request("t-cancel"))
            .await
            .expect("snapshot ok");
        assert_eq!(result.status, TransferStatus::Cancelled);
        assert_eq!(result.bytes_transferred, 64);
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_mode_returns_immediately_when_already_terminal() {
        let wired = build_wired();
        seed(
            &wired.transfers,
            entity(
                "t-pre-done",
                "s-1",
                TransferDirection::Upload,
                TransferStatus::Completed,
                256,
                256,
            ),
        )
        .await;
        let started = std::time::Instant::now();
        let result = wired
            .uc
            .execute(GetTransferProgressRequest {
                transfer_id: TransferId::new("t-pre-done".to_string()),
                wait: true,
                wait_timeout: Some(Duration::from_secs(60)),
            })
            .await
            .expect("wait ok");
        assert_eq!(result.status, TransferStatus::Completed);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "terminal short-circuit must skip the polling loop entirely"
        );
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_mode_resolves_when_status_flips_to_completed() {
        let wired = build_wired();
        seed(
            &wired.transfers,
            entity(
                "t-flip",
                "s-1",
                TransferDirection::Upload,
                TransferStatus::Running,
                256,
                1024,
            ),
        )
        .await;
        // Background flip after 100 ms so the polling loop observes the
        // transition. Kept short so the test runs well under the 1 s
        // CI envelope.
        let twin = wired.transfers.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            let mut e = entity(
                "t-flip",
                "s-1",
                TransferDirection::Upload,
                TransferStatus::Completed,
                1024,
                1024,
            );
            e.id = TransferId::new("t-flip".to_string());
            twin.update(e).await.expect("update");
        });
        let result = wired
            .uc
            .execute(GetTransferProgressRequest {
                transfer_id: TransferId::new("t-flip".to_string()),
                wait: true,
                wait_timeout: Some(Duration::from_secs(2)),
            })
            .await
            .expect("wait ok");
        assert_eq!(result.status, TransferStatus::Completed);
        assert_eq!(result.bytes_transferred, 1024);
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_mode_returns_running_snapshot_when_deadline_expires() {
        let wired = build_wired();
        seed(
            &wired.transfers,
            entity(
                "t-stuck",
                "s-1",
                TransferDirection::Download,
                TransferStatus::Running,
                512,
                10_000,
            ),
        )
        .await;
        let result = wired
            .uc
            .execute(GetTransferProgressRequest {
                transfer_id: TransferId::new("t-stuck".to_string()),
                wait: true,
                wait_timeout: Some(Duration::from_millis(150)),
            })
            .await
            .expect("wait ok");
        assert_eq!(
            result.status,
            TransferStatus::Running,
            "deadline expiry must return the latest running snapshot"
        );
        assert_eq!(result.bytes_transferred, 512);
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_timeout_above_cap_is_clamped() {
        // Pure unit assertion against the helper — easier to verify the
        // cap deterministically than to wait 300 s in a test.
        let cap = compute_deadline(Some(Duration::from_secs(86_400)));
        assert_eq!(cap, MAX_WAIT_TIMEOUT);
        let default_ = compute_deadline(None);
        assert_eq!(default_, Duration::from_secs(30));
        let pass_through = compute_deadline(Some(Duration::from_secs(15)));
        assert_eq!(pass_through, Duration::from_secs(15));
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_to_result_projection_preserves_every_field() {
        let mut e = entity(
            "t-project",
            "s-7",
            TransferDirection::Upload,
            TransferStatus::Failed,
            42,
            100,
        );
        e.error = Some("explicit failure".to_string());
        let projection = snapshot_to_result(e.clone());
        assert_eq!(projection.transfer_id, e.id);
        assert_eq!(projection.direction, e.direction);
        assert_eq!(projection.status, e.status);
        assert_eq!(projection.bytes_transferred, e.bytes_transferred);
        assert_eq!(projection.total_bytes, e.total_bytes);
        assert_eq!(projection.error, e.error);
        assert_eq!(projection.last_seq, 0);
    }
}
