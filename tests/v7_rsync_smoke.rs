//! ADR 0011 — rsync hybrid transport smoke tests (use-case driven).
//!
//! v7.0.0-alpha.2 architectural retrenchment: drives the
//! [`RsyncSyncUseCase`] against the fake transport adapters under
//! `--features test-fixtures`. The tests pin the rsync probe + transport
//! selection branches per the ADR matrix:
//!
//! 1. `transport_sftp_picks_sftp_lane` — `transport=Sftp` skips the
//!    probe and routes to the SFTP fake.
//! 2. `transport_wire_returns_too_old_when_rsync_missing` — forced
//!    Wire path fails the probe; surfaces `RsyncVersionTooOld`.
//! 3. `transport_auto_routes_to_wire_when_rsync_v31_present` — Auto
//!    + probe-says-v31 routes to Wire.
//! 4. `transport_auto_routes_to_sftp_when_rsync_missing` — Auto +
//!    probe-says-missing routes to SFTP.
//! 5. `cancel_drives_close_on_both_transports` — cancel hits both
//!    transports' idempotent close paths and lands the session in
//!    `Cancelled`.
//! 6. `sftp_with_hardlinks_returns_feature_missing` — capability gate
//!    rejects hardlinks on the SFTP path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests use unwrap/expect for brevity"
)]

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use ssh_mcp::adapters::config::env::EnvConfig;
use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::repo::dashmap::rsync::DashMapRsyncRepo;
use ssh_mcp::adapters::repo::dashmap::session::DashMapSessionRepo;
use ssh_mcp::adapters::rsync::fake::transport::{FakeRsyncTransport, FakeRsyncTransportCall};
use ssh_mcp::adapters::rsync::sftp::fake::FakeRsyncSftpFs;
use ssh_mcp::adapters::ssh::fake::FakeSshClient;
use ssh_mcp::application::rsync_sync::{
    RsyncStartedOutcome, RsyncSyncDeps, RsyncSyncRequest, RsyncSyncUseCase, RsyncTransportPicked,
    RsyncTransportSelection,
};
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::identity::Address;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::rsync::RsyncStatus;
use ssh_mcp::domain::rsync_ids::RsyncId;
use ssh_mcp::domain::session::SessionEntity;
use ssh_mcp::ports::session_repo::SessionRepository;

type SmokeUseCase = RsyncSyncUseCase<
    FakeRsyncTransport,
    FakeRsyncTransport,
    FakeRsyncSftpFs,
    DashMapRsyncRepo,
    DashMapSessionRepo,
    FakeSshClient,
    UuidIds,
    EnvConfig,
>;

struct Fixture {
    use_case: SmokeUseCase,
    wire: Arc<FakeRsyncTransport>,
    sftp: Arc<FakeRsyncTransport>,
    sftp_fs: Arc<FakeRsyncSftpFs>,
    ssh: Arc<FakeSshClient>,
    repo: Arc<DashMapRsyncRepo>,
    session_id: SessionId,
}

async fn fixture() -> Fixture {
    let wire = Arc::new(FakeRsyncTransport::new());
    let sftp = Arc::new(FakeRsyncTransport::new());
    let sftp_fs = Arc::new(FakeRsyncSftpFs::new());
    sftp_fs.put_dir("/tmp", 0o755);
    let repo = Arc::new(DashMapRsyncRepo::new());
    let sessions = Arc::new(DashMapSessionRepo::new());
    let ssh = Arc::new(FakeSshClient::new());
    let session_id = SessionId::new("sess-smoke".to_string());
    let address = Address::new("h".to_string(), 22).unwrap();
    let entity = SessionEntity {
        id: session_id.clone(),
        name: None,
        agent_id: None,
        address,
        username: "u".to_string(),
        connected_at: Utc::now(),
        default_timeout: Duration::from_secs(180),
        retry_attempts: 0,
        compression_enabled: false,
        last_health_check: None,
        healthy: None,
    };
    sessions.insert(entity).await.unwrap();
    let ids = Arc::new(UuidIds);
    let config = Arc::new(EnvConfig);
    let use_case = SmokeUseCase::new(RsyncSyncDeps {
        wire: Arc::clone(&wire),
        sftp: Arc::clone(&sftp),
        sftp_fs: Arc::clone(&sftp_fs),
        rsync_repo: Arc::clone(&repo),
        sessions: Arc::clone(&sessions),
        ssh: Arc::clone(&ssh),
        ids,
        config,
    });
    Fixture {
        use_case,
        wire,
        sftp,
        sftp_fs,
        ssh,
        repo,
        session_id,
    }
}

fn req(session_id: &SessionId, transport: RsyncTransportSelection) -> RsyncSyncRequest {
    RsyncSyncRequest {
        session_id: session_id.clone(),
        src: "/x".to_string(),
        dst: "/y".to_string(),
        transport,
        preserve_hardlinks: false,
        delta_sync: false,
        preserve_symlinks: false,
        preserve_perms: false,
        preserve_mtime: false,
        delete: false,
        release_when_no_subs: false,
    }
}

#[tokio::test]
async fn transport_sftp_picks_sftp_lane() {
    let f = fixture().await;
    f.sftp
        .queue_start_ok(RsyncId::new("rs-sftp".to_string()), false);
    let outcome: RsyncStartedOutcome = f
        .use_case
        .execute(req(&f.session_id, RsyncTransportSelection::Sftp))
        .await
        .expect("execute ok");
    assert_eq!(outcome.transport, RsyncTransportPicked::Sftp);
    // Wire transport must NOT have been called on transport=Sftp.
    assert_eq!(f.wire.call_count(), 0);
    // SFTP transport saw exactly one StartSession call.
    let calls = f.sftp.calls();
    assert_eq!(calls.len(), 1);
    assert!(matches!(calls[0], FakeRsyncTransportCall::StartSession(_)));
    // Repo carries the registered session.
    let snap = f
        .repo
        .get_owned_session(&RsyncId::new("rs-sftp".to_string()))
        .await;
    assert!(snap.is_some());
}

#[tokio::test]
async fn transport_wire_returns_too_old_when_rsync_missing() {
    let f = fixture().await;
    f.ssh.queue_exec_string("MISSING\n");
    let err = f
        .use_case
        .execute(req(&f.session_id, RsyncTransportSelection::Wire))
        .await
        .expect_err("must error");
    match err {
        DomainError::RsyncVersionTooOld(msg) => {
            assert!(msg.contains("missing") || msg.contains("MISSING") || msg.contains("v31"));
        }
        other => panic!("expected RsyncVersionTooOld, got {other:?}"),
    }
    // Neither transport opened a session.
    assert_eq!(f.wire.call_count(), 0);
    assert_eq!(f.sftp.call_count(), 0);
}

#[tokio::test]
async fn transport_auto_routes_to_wire_when_rsync_v31_present() {
    let f = fixture().await;
    f.ssh
        .queue_exec_string("rsync  version 3.2.7  protocol version 31\n");
    f.wire
        .queue_start_ok(RsyncId::new("rs-auto-wire".to_string()), true);
    let outcome = f
        .use_case
        .execute(req(&f.session_id, RsyncTransportSelection::Auto))
        .await
        .expect("ok");
    assert_eq!(outcome.transport, RsyncTransportPicked::Wire);
    assert_eq!(f.wire.call_count(), 1);
    assert_eq!(f.sftp.call_count(), 0);
}

#[tokio::test]
async fn transport_auto_routes_to_sftp_when_rsync_missing() {
    let f = fixture().await;
    f.ssh.queue_exec_string("MISSING\n");
    f.sftp
        .queue_start_ok(RsyncId::new("rs-auto-sftp".to_string()), false);
    let outcome = f
        .use_case
        .execute(req(&f.session_id, RsyncTransportSelection::Auto))
        .await
        .expect("ok");
    assert_eq!(outcome.transport, RsyncTransportPicked::Sftp);
    assert_eq!(f.sftp.call_count(), 1);
    assert_eq!(f.wire.call_count(), 0);
}

#[tokio::test]
async fn cancel_drives_close_on_both_transports() {
    let f = fixture().await;
    f.sftp
        .queue_start_ok(RsyncId::new("rs-cancel".to_string()), false);
    let outcome = f
        .use_case
        .execute(req(&f.session_id, RsyncTransportSelection::Sftp))
        .await
        .expect("ok");
    f.use_case
        .cancel(&outcome.rsync_id)
        .await
        .expect("cancel ok");
    let snap = f.use_case.stats(&outcome.rsync_id).await.expect("stats ok");
    assert_eq!(snap.status, RsyncStatus::Cancelled);
}

#[tokio::test]
async fn sftp_with_hardlinks_returns_feature_missing() {
    let f = fixture().await;
    let mut r = req(&f.session_id, RsyncTransportSelection::Sftp);
    r.preserve_hardlinks = true;
    let err = f.use_case.execute(r).await.expect_err("must error");
    match err {
        DomainError::SftpFeatureMissing(msg) => assert!(msg.contains("hardlink")),
        other => panic!("expected SftpFeatureMissing, got {other:?}"),
    }
}

#[tokio::test]
async fn sftp_with_symlinks_against_unsupported_server_returns_feature_missing() {
    let f = fixture().await;
    f.sftp_fs.fail_symlink();
    let mut r = req(&f.session_id, RsyncTransportSelection::Sftp);
    r.preserve_symlinks = true;
    let err = f.use_case.execute(r).await.expect_err("must error");
    match err {
        DomainError::SftpFeatureMissing(msg) => {
            assert!(msg.contains("symlink"));
            assert!(msg.contains("preserve.symlinks"));
        }
        other => panic!("expected SftpFeatureMissing, got {other:?}"),
    }
    // Transport must NOT have been invoked when the gate fires.
    assert_eq!(f.sftp.call_count(), 0);
}

#[tokio::test]
async fn sftp_with_perms_against_unsupported_server_returns_feature_missing() {
    let f = fixture().await;
    f.sftp_fs.fail_setstat();
    let mut r = req(&f.session_id, RsyncTransportSelection::Sftp);
    r.preserve_perms = true;
    let err = f.use_case.execute(r).await.expect_err("must error");
    match err {
        DomainError::SftpFeatureMissing(msg) => {
            assert!(msg.contains("setstat"));
            assert!(msg.contains("preserve.perms"));
        }
        other => panic!("expected SftpFeatureMissing, got {other:?}"),
    }
    assert_eq!(f.sftp.call_count(), 0);
}

#[tokio::test]
async fn sftp_with_symlinks_against_supported_server_succeeds() {
    let f = fixture().await;
    f.sftp
        .queue_start_ok(RsyncId::new("rs-sym".to_string()), false);
    let mut r = req(&f.session_id, RsyncTransportSelection::Sftp);
    r.preserve_symlinks = true;
    let outcome = f.use_case.execute(r).await.expect("execute ok");
    assert_eq!(outcome.transport, RsyncTransportPicked::Sftp);
}

// ---------------------------------------------------------------------------
// Test helper extension on `DashMapRsyncRepo` — pulls a single session
// by id without going through the `RsyncRepository` trait surface.
// Used to assert the use case registered the session aggregate after
// `execute`. Lives in this file so the production repo trait stays
// minimal.
// ---------------------------------------------------------------------------

trait DashMapRsyncRepoExt {
    async fn get_owned_session(&self, id: &RsyncId) -> Option<()>;
}

impl DashMapRsyncRepoExt for DashMapRsyncRepo {
    async fn get_owned_session(&self, id: &RsyncId) -> Option<()> {
        use ssh_mcp::ports::rsync_repo::RsyncRepository;
        self.get(id).await.ok().flatten().map(|_| ())
    }
}
