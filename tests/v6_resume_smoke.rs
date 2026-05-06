//! ADR 0010 — SFTP resume smoke integration tests.
//!
//! The 12 scenarios (6 upload + 6 download) drive the
//! [`UploadFileUseCase`] / [`DownloadFileUseCase`] against the test
//! [`FakeSftpClient`], which the v6.1 work extended with three
//! resume-aware queue helpers (`queue_*_resumed`, `queue_*_skipped`).
//! Each scenario maps onto one decision branch from the ADR matrix:
//!
//! 1. Fresh transfer (`resume = false`) -> `resumed_from = 0`,
//!    no `RESUMED_FROM:` line, status = Running.
//! 2. Resume with empty destination -> Truncate-equivalent, no
//!    `RESUMED_FROM:` line.
//! 3. Resume with partial destination -> `RESUMED_FROM: N` line.
//! 4. Resume with equal-size destination -> Skip plan; entity reaches
//!    `Completed` synchronously, `bytes_transferred = total_bytes`.
//! 5. Resume with overshoot -> error containing `[RESUME_OVERSHOOT]`.
//! 6. Resume + verify with synthetic mismatch -> error containing
//!    `[RESUME_MISMATCH]`.
//!
//! Mirror set covers the download direction (scenarios 7..=12).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests use unwrap/expect for brevity"
)]

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::config::memory::MapConfig;
use ssh_mcp::adapters::id_generator::deterministic::SequentialIds;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::adapters::repo::dashmap::session::DashMapSessionRepo;
use ssh_mcp::adapters::repo::dashmap::transfer::DashMapTransferRepo;
use ssh_mcp::adapters::sftp::fake::FakeSftpClient;
use ssh_mcp::application::download_file::{DownloadFileUseCase, DownloadRequest};
use ssh_mcp::application::upload_file::{UploadFileUseCase, UploadRequest};
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::identity::Address;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::session::SessionEntity;
use ssh_mcp::domain::transfer::TransferStatus;
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::session_repo::SessionRepository;
use ssh_mcp::ports::subscriber_registry::{
    ResourceKind, SubscriberRegistryPort, SubscriberSnapshot,
};
use ssh_mcp::ports::transfer_repo::TransferRepository;

/// Stub registry — same shape as the test stubs in
/// [`crate::application::upload_file::tests`]. Resume scenarios do not
/// observe poke ordering, so the trait is satisfied with deterministic
/// no-ops.
#[derive(Debug, Default)]
struct StubRegistry;

impl SubscriberRegistryPort for StubRegistry {
    fn next_seq(&self, _kind: ResourceKind, _resource_id: &str) -> u64 {
        0
    }

    fn current_seq(&self, _kind: ResourceKind, _resource_id: &str) -> u64 {
        0
    }

    fn poke(&self, _kind: ResourceKind, _resource_id: &str) {}

    fn compensate_truncation(&self, _uri: &str, _bytes_dropped: u64) {}

    fn snapshot_subscribers(&self, _uri: &str) -> Vec<SubscriberSnapshot> {
        Vec::new()
    }

    fn peer_byte_cursor(&self, _peer_id: &ssh_mcp::domain::ids::PeerId, _uri: &str) -> u64 {
        0
    }

    fn advance_peer_byte_cursor(
        &self,
        _peer_id: &ssh_mcp::domain::ids::PeerId,
        _uri: &str,
        target: u64,
    ) -> u64 {
        target
    }

    fn gc_closed_peers(&self) -> usize {
        0
    }
}

type Upload = UploadFileUseCase<
    FakeSftpClient,
    DashMapSessionRepo,
    DashMapTransferRepo,
    FakeClock,
    SequentialIds,
    MapConfig,
    StubRegistry,
>;

type Download = DownloadFileUseCase<
    FakeSftpClient,
    DashMapSessionRepo,
    DashMapTransferRepo,
    FakeClock,
    SequentialIds,
    MapConfig,
    StubRegistry,
>;

struct UploadHarness {
    uc: Upload,
    sftp: Arc<FakeSftpClient>,
    sessions: Arc<DashMapSessionRepo>,
    transfers: Arc<DashMapTransferRepo>,
}

struct DownloadHarness {
    uc: Download,
    sftp: Arc<FakeSftpClient>,
    sessions: Arc<DashMapSessionRepo>,
    transfers: Arc<DashMapTransferRepo>,
}

fn build_upload_harness() -> UploadHarness {
    let sftp = Arc::new(FakeSftpClient::new());
    let sessions = Arc::new(DashMapSessionRepo::new());
    let transfers = Arc::new(DashMapTransferRepo::new());
    let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
    let ids = Arc::new(SequentialIds::default());
    let config = Arc::new(MapConfig::default_v3());
    let registry = Arc::new(StubRegistry);
    let cascade = CascadeCoordinator::new();
    let lifecycle: Arc<dyn LifecyclePolicyPort> =
        RefcountedLifecycleAdapter::new(cascade, Arc::clone(&clock));
    let uc = UploadFileUseCase::new(
        Arc::clone(&sftp),
        Arc::clone(&sessions),
        Arc::clone(&transfers),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
        Arc::clone(&registry),
    )
    .with_lifecycle(lifecycle);
    UploadHarness {
        uc,
        sftp,
        sessions,
        transfers,
    }
}

fn build_download_harness() -> DownloadHarness {
    let sftp = Arc::new(FakeSftpClient::new());
    let sessions = Arc::new(DashMapSessionRepo::new());
    let transfers = Arc::new(DashMapTransferRepo::new());
    let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
    let ids = Arc::new(SequentialIds::default());
    let config = Arc::new(MapConfig::default_v3());
    let registry = Arc::new(StubRegistry);
    let cascade = CascadeCoordinator::new();
    let lifecycle: Arc<dyn LifecyclePolicyPort> =
        RefcountedLifecycleAdapter::new(cascade, Arc::clone(&clock));
    let uc = DownloadFileUseCase::new(
        Arc::clone(&sftp),
        Arc::clone(&sessions),
        Arc::clone(&transfers),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
        Arc::clone(&registry),
    )
    .with_lifecycle(lifecycle);
    DownloadHarness {
        uc,
        sftp,
        sessions,
        transfers,
    }
}

fn seed_session(repo: &DashMapSessionRepo, id: &str) -> SessionEntity {
    let entity = SessionEntity {
        id: SessionId::new(id.to_string()),
        name: None,
        agent_id: None,
        address: Address::new("h.example.com".to_string(), 22).expect("address"),
        username: "alice".to_string(),
        connected_at: Utc::now(),
        default_timeout: Duration::from_secs(30),
        retry_attempts: 0,
        compression_enabled: true,
        last_health_check: None,
        healthy: None,
    };
    let to_insert = entity.clone();
    let repo_clone = repo.clone();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async move {
            repo_clone.insert(to_insert).await.expect("seed insert");
        });
    });
    entity
}

/// Create a real temp file the use case can stat through its
/// pre-flight `LOCAL_NOT_FILE` guard.
fn seed_local_file(label: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "ssh-mcp-resume-{label}-{nano}.bin",
        nano = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    ));
    std::fs::write(&path, b"adr-0010 resume fixture payload").expect("seed local file");
    path.to_string_lossy().into_owned()
}

fn upload_request(
    session_id: &SessionId,
    label: &str,
    resume: bool,
    verify: bool,
) -> UploadRequest {
    UploadRequest {
        session_id: session_id.clone(),
        local_path: seed_local_file(label),
        remote_path: format!("/srv/{label}.bin"),
        lifecycle_policy: None,
        resume,
        verify,
    }
}

fn download_request(
    session_id: &SessionId,
    label: &str,
    resume: bool,
    verify: bool,
) -> DownloadRequest {
    DownloadRequest {
        session_id: session_id.clone(),
        remote_path: format!("/srv/{label}.bin"),
        local_path: seed_local_file(label),
        lifecycle_policy: None,
        resume,
        verify,
    }
}

// ---- upload scenarios -----------------------------------------------------

/// Scenario 1 — fresh upload: `resume = false`, `resumed_from = 0`, no
/// resumed-from line surfaces because the renderer suppresses zero
/// offsets to keep v6.0 wires byte-identical.
#[tokio::test(flavor = "multi_thread")]
async fn upload_fresh_no_resume_flag_yields_resumed_from_zero() {
    let h = build_upload_harness();
    let sess = seed_session(&h.sessions, "sess-fresh");
    h.sftp.queue_upload_ok(2048);

    let outcome =
        h.uc.execute(upload_request(&sess.id, "fresh", false, false))
            .await
            .expect("fresh upload");

    assert_eq!(outcome.resumed_from, 0);
    assert_eq!(outcome.total_bytes, 2048);
    let stored = h
        .transfers
        .get(&outcome.transfer_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, TransferStatus::Running);
    assert_eq!(stored.resumed_from, 0);
}

/// Scenario 2 — resume requested but destination empty: behaviour
/// matches Truncate; `resumed_from = 0`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_resume_with_empty_destination_yields_resumed_from_zero() {
    let h = build_upload_harness();
    let sess = seed_session(&h.sessions, "sess-empty");
    // queue_upload_ok defaults `resumed_from = 0` (Truncate).
    h.sftp.queue_upload_ok(4096);

    let outcome =
        h.uc.execute(upload_request(&sess.id, "empty", true, false))
            .await
            .expect("upload ok");

    assert_eq!(outcome.resumed_from, 0);
    assert_eq!(outcome.total_bytes, 4096);
}

/// Scenario 3 — resume with a partial destination: the adapter
/// preflight returns a Resume plan; the entity carries the offset;
/// `bytes_transferred = resumed_from` so progress events ramp from
/// the resume point rather than zero.
#[tokio::test(flavor = "multi_thread")]
async fn upload_resume_partial_destination_carries_offset() {
    let h = build_upload_harness();
    let sess = seed_session(&h.sessions, "sess-partial");
    h.sftp.queue_upload_resumed(8_388_608, 4_194_304);

    let outcome =
        h.uc.execute(upload_request(&sess.id, "partial", true, false))
            .await
            .expect("partial resume ok");

    assert_eq!(outcome.resumed_from, 4_194_304);
    assert_eq!(outcome.total_bytes, 8_388_608);
    let stored = h
        .transfers
        .get(&outcome.transfer_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.resumed_from, 4_194_304);
    assert_eq!(stored.bytes_transferred, 4_194_304);
}

/// Scenario 4 — resume against an equal-size destination: Skip plan
/// short-circuits to `Completed` synchronously, `bytes_transferred =
/// total_bytes`, `resumed_from = total_bytes`.
#[tokio::test(flavor = "multi_thread")]
async fn upload_resume_equal_size_destination_short_circuits_completed() {
    let h = build_upload_harness();
    let sess = seed_session(&h.sessions, "sess-skip");
    h.sftp.queue_upload_skipped(1_024);

    let outcome =
        h.uc.execute(upload_request(&sess.id, "skip", true, false))
            .await
            .expect("skip ok");

    assert_eq!(outcome.resumed_from, 1_024);
    assert_eq!(outcome.total_bytes, 1_024);
    let stored = h
        .transfers
        .get(&outcome.transfer_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, TransferStatus::Completed);
    assert_eq!(stored.bytes_transferred, 1_024);
}

/// Scenario 5 — resume with destination larger than source surfaces a
/// `[RESUME_OVERSHOOT]`-tagged `DomainError::Sftp`. The renderer maps
/// this onto the wire `REASON:` line so smaller LLMs can branch on the
/// tag string.
#[tokio::test(flavor = "multi_thread")]
async fn upload_resume_overshoot_propagates_tagged_sftp_error() {
    let h = build_upload_harness();
    let sess = seed_session(&h.sessions, "sess-overshoot");
    h.sftp.queue_upload_error(DomainError::Sftp(
        "[RESUME_OVERSHOOT] preflight resume upload: remote size 2048 exceeds local size 1024; \
         refusing to resume. Re-run with resume=false to overwrite, or fix the partial file."
            .to_string(),
    ));

    let err =
        h.uc.execute(upload_request(&sess.id, "overshoot", true, false))
            .await
            .expect_err("overshoot must fail");
    match err {
        DomainError::Sftp(msg) => assert!(
            msg.contains("[RESUME_OVERSHOOT]"),
            "expected RESUME_OVERSHOOT tag, got: {msg}"
        ),
        other => panic!("unexpected error variant: {other:?}"),
    }
}

/// Scenario 6 — resume with `verify = true` and a hash divergence
/// surfaces `[RESUME_MISMATCH]`. The fake adapter does not run real
/// hashes; the queued error mirrors what the russh adapter emits when
/// the local-vs-remote prefix sha256 diverges.
#[tokio::test(flavor = "multi_thread")]
async fn upload_resume_verify_mismatch_propagates_tagged_sftp_error() {
    let h = build_upload_harness();
    let sess = seed_session(&h.sessions, "sess-mismatch");
    h.sftp.queue_upload_error(DomainError::Sftp(
        "[RESUME_MISMATCH] resume prefix sha256 differs (offset=512); local=aa00... remote=bb11... \
         Re-run with resume=false to overwrite, or fix the partial file."
            .to_string(),
    ));

    let err =
        h.uc.execute(upload_request(&sess.id, "mismatch", true, true))
            .await
            .expect_err("mismatch must fail");
    match err {
        DomainError::Sftp(msg) => assert!(
            msg.contains("[RESUME_MISMATCH]"),
            "expected RESUME_MISMATCH tag, got: {msg}"
        ),
        other => panic!("unexpected error variant: {other:?}"),
    }
}

// ---- download scenarios ---------------------------------------------------

/// Scenario 7 — fresh download mirror of scenario 1.
#[tokio::test(flavor = "multi_thread")]
async fn download_fresh_no_resume_flag_yields_resumed_from_zero() {
    let h = build_download_harness();
    let sess = seed_session(&h.sessions, "sess-d-fresh");
    h.sftp.queue_download_ok(2048);

    let outcome =
        h.uc.execute(download_request(&sess.id, "d-fresh", false, false))
            .await
            .expect("fresh download");

    assert_eq!(outcome.resumed_from, 0);
    assert_eq!(outcome.total_bytes, 2048);
}

/// Scenario 8 — resume requested but local file empty: Truncate
/// equivalent.
#[tokio::test(flavor = "multi_thread")]
async fn download_resume_with_empty_destination_yields_resumed_from_zero() {
    let h = build_download_harness();
    let sess = seed_session(&h.sessions, "sess-d-empty");
    h.sftp.queue_download_ok(4096);

    let outcome =
        h.uc.execute(download_request(&sess.id, "d-empty", true, false))
            .await
            .expect("download ok");

    assert_eq!(outcome.resumed_from, 0);
    assert_eq!(outcome.total_bytes, 4096);
}

/// Scenario 9 — resume with partial local file: Resume plan with an
/// offset.
#[tokio::test(flavor = "multi_thread")]
async fn download_resume_partial_destination_carries_offset() {
    let h = build_download_harness();
    let sess = seed_session(&h.sessions, "sess-d-partial");
    h.sftp.queue_download_resumed(16_777_216, 8_388_608);

    let outcome =
        h.uc.execute(download_request(&sess.id, "d-partial", true, false))
            .await
            .expect("partial download resume");

    assert_eq!(outcome.resumed_from, 8_388_608);
    assert_eq!(outcome.total_bytes, 16_777_216);
    let stored = h
        .transfers
        .get(&outcome.transfer_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.bytes_transferred, 8_388_608);
}

/// Scenario 10 — equal-size destination: Skip plan short-circuit.
#[tokio::test(flavor = "multi_thread")]
async fn download_resume_equal_size_destination_short_circuits_completed() {
    let h = build_download_harness();
    let sess = seed_session(&h.sessions, "sess-d-skip");
    h.sftp.queue_download_skipped(2_048);

    let outcome =
        h.uc.execute(download_request(&sess.id, "d-skip", true, false))
            .await
            .expect("skip download");

    assert_eq!(outcome.resumed_from, 2_048);
    let stored = h
        .transfers
        .get(&outcome.transfer_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, TransferStatus::Completed);
    assert_eq!(stored.bytes_transferred, 2_048);
}

/// Scenario 11 — overshoot mirror.
#[tokio::test(flavor = "multi_thread")]
async fn download_resume_overshoot_propagates_tagged_sftp_error() {
    let h = build_download_harness();
    let sess = seed_session(&h.sessions, "sess-d-overshoot");
    h.sftp.queue_download_error(DomainError::Sftp(
        "[RESUME_OVERSHOOT] preflight resume download: local size 4096 exceeds remote size 2048; \
         refusing to resume. Re-run with resume=false to overwrite, or fix the partial file."
            .to_string(),
    ));

    let err =
        h.uc.execute(download_request(&sess.id, "d-overshoot", true, false))
            .await
            .expect_err("overshoot must fail");
    match err {
        DomainError::Sftp(msg) => assert!(
            msg.contains("[RESUME_OVERSHOOT]"),
            "expected RESUME_OVERSHOOT tag, got: {msg}"
        ),
        other => panic!("unexpected error variant: {other:?}"),
    }
}

/// Scenario 12 — verify mismatch mirror.
#[tokio::test(flavor = "multi_thread")]
async fn download_resume_verify_mismatch_propagates_tagged_sftp_error() {
    let h = build_download_harness();
    let sess = seed_session(&h.sessions, "sess-d-mismatch");
    h.sftp.queue_download_error(DomainError::Sftp(
        "[RESUME_MISMATCH] resume prefix sha256 differs (offset=1024); local=cc... remote=dd... \
         Re-run with resume=false to overwrite, or fix the partial file."
            .to_string(),
    ));

    let err =
        h.uc.execute(download_request(&sess.id, "d-mismatch", true, true))
            .await
            .expect_err("mismatch must fail");
    match err {
        DomainError::Sftp(msg) => assert!(
            msg.contains("[RESUME_MISMATCH]"),
            "expected RESUME_MISMATCH tag, got: {msg}"
        ),
        other => panic!("unexpected error variant: {other:?}"),
    }
}
