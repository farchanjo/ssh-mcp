#![cfg(feature = "e2e-vm")]
//! End-to-end SFTP-rsync round-trip against the user's local Linux VM.
//!
//! Skipped by default — opt in via:
//!
//! ```bash
//! cargo test --features e2e-vm v7_rsync_e2e_vm -- --ignored --nocapture
//! ```
//!
//! Requires SSH access to a Linux host. Defaults to `root@vm.services`
//! with `~/.ssh/id_rsa`; override via:
//!
//! - `SSH_MCP_E2E_HOST` (default `vm.services`)
//! - `SSH_MCP_E2E_PORT` (default `22`)
//! - `SSH_MCP_E2E_USER` (default `root`)
//! - `SSH_MCP_E2E_KEY_PATH` (default `~/.ssh/id_rsa`)
//!
//! What this exercises (ADR 0011 v7.0.0-alpha.4):
//!
//! 1. Pre-creates a synthetic remote source tree via `ssh_exec` (mkdir
//!    + heredoc files + symlink) — the v7.0.0-alpha.4 SFTP transport
//!    walks both `src` and `dst` through the same `RsyncSftpFsPort`,
//!    so the source must be reachable through the SFTP server too. A
//!    local-FS adapter implementing `RsyncSftpFsPort` for the source
//!    side is a follow-up slice (documented in the SFTP server
//!    compatibility notes section of MIGRATION.md).
//! 2. Drives the live `RusshRsyncSftpFs` adapter through
//!    [`SftpRsyncTransport`] to mirror the source tree onto a fresh
//!    destination directory on the same VM.
//! 3. Reads `RsyncProgressEvent` frames off the lane and asserts the
//!    pipeline emits `SessionStarted` then per-file events then
//!    `SyncCompleted` (or `SessionFailed` with a human-readable code).
//! 4. Verifies every file landed byte-identical via `sha256sum` over
//!    SSH and that perms / symlink targets match.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "e2e tests use unwrap/expect/print for brevity; gated behind the opt-in `e2e-vm` feature so they never compile under the canonical CI gate"
)]

use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ssh_mcp::adapters::rsync::sftp::comparator::Direction;
use ssh_mcp::adapters::rsync::sftp::{SftpRsyncOpts, SftpRsyncTransport};
use ssh_mcp::adapters::rsync::types::{PreserveFlags, RsyncProgressEvent, RsyncTransportKind};
use ssh_mcp::adapters::sftp::rsync_fs_impl::RusshRsyncSftpFs;
use ssh_mcp::adapters::sftp::russh_sftp_adapter::SshHandleRegistry;
use ssh_mcp::adapters::ssh::russh_adapter::RusshAdapter;
use ssh_mcp::domain::command::CommandRequest;
use ssh_mcp::domain::identity::{Address, Credentials};
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::ports::rsync_transport::{RsyncDirection, RsyncStartRequest, RsyncTransportPort};
use ssh_mcp::ports::ssh_client::SshClientPort;
use tokio::time::timeout;
use uuid::Uuid;

const DEFAULT_HOST: &str = "vm.services";
const DEFAULT_PORT: u16 = 22;
const DEFAULT_USER: &str = "root";

fn env_host() -> String {
    env::var("SSH_MCP_E2E_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string())
}
fn env_port() -> u16 {
    env::var("SSH_MCP_E2E_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}
fn env_user() -> String {
    env::var("SSH_MCP_E2E_USER").unwrap_or_else(|_| DEFAULT_USER.to_string())
}
fn env_key_path() -> PathBuf {
    if let Ok(p) = env::var("SSH_MCP_E2E_KEY_PATH") {
        return PathBuf::from(p);
    }
    let home = env::var("HOME").unwrap_or_else(|_| "/".to_string());
    PathBuf::from(home).join(".ssh/id_rsa")
}

/// Open a real russh session against the configured target. Surfaces
/// a clear `panic!` on failure so the test fails loudly when SSH access
/// is missing — silent skips would let regressions slip through.
async fn connect_real() -> (Arc<RusshAdapter>, SshHandleRegistry, SessionId) {
    let host = env_host();
    let port = env_port();
    let user = env_user();
    let key_path = env_key_path();

    println!("e2e-vm: connecting to {user}@{host}:{port} with key {key_path:?}");

    if !key_path.exists() {
        panic!(
            "e2e-vm requires SSH access to {user}@{host}:{port}; key file {key_path:?} not found. \
             Configure SSH_MCP_E2E_HOST / USER / KEY_PATH or skip the feature."
        );
    }

    let registry = SshHandleRegistry::new();
    let adapter = Arc::new(RusshAdapter::new().with_sftp_registry(registry.clone()));

    let session_id = SessionId::new(format!("e2e-{}", Uuid::now_v7().simple()));
    let address = Address::new(host.clone(), port).expect("valid address");
    // The russh adapter's `Credentials::PrivateKey { key_pem }` field is
    // overloaded as a file path (see `split_credentials` in the
    // production adapter) — pass the path through verbatim.
    let credentials = Credentials::PrivateKey {
        username: user.clone(),
        key_pem: key_path.to_string_lossy().into_owned(),
        passphrase: None,
    };

    if let Err(err) = adapter
        .connect(
            session_id.clone(),
            address,
            credentials,
            Duration::from_secs(15),
        )
        .await
    {
        panic!(
            "e2e-vm requires SSH access to {user}@{host}:{port}; connect failed: {err}. \
             Configure SSH_MCP_E2E_HOST / USER / KEY_PATH or skip the feature."
        );
    }

    (adapter, registry, session_id)
}

/// Run `cmd` over SSH and return stdout as a UTF-8 string.
async fn exec(adapter: &RusshAdapter, session: &SessionId, cmd: &str) -> String {
    let req = CommandRequest::new(session.clone(), cmd.to_string());
    let outcome = adapter
        .execute(req)
        .await
        .unwrap_or_else(|err| panic!("ssh exec '{cmd}' failed: {err}"));
    String::from_utf8_lossy(&outcome.stdout).into_owned()
}

/// Pre-create a synthetic source tree on the remote host via
/// `ssh_exec`. Mirrors the brief's intent: 5 small files (1-100 bytes),
/// 2 nested dirs each with 2-3 files, 1 symlink, varied modes.
///
/// Returns the absolute remote path of the source tree root.
async fn build_remote_source_tree(adapter: &RusshAdapter, session: &SessionId) -> String {
    let src = format!("/tmp/ssh-mcp-rsync-e2e-src-{}", Uuid::now_v7().simple());
    let script = format!(
        r#"set -e
mkdir -p {src}/nested-a {src}/nested-b
printf 'a' > {src}/a.txt
printf 'hello world' > {src}/b.txt
printf 'this is c with sixty bytes of payload .....' > {src}/c.txt
printf '%.0sd' {{1..80}} > {src}/d.txt
printf '%.0se' {{1..100}} > {src}/e.txt
printf 'one' > {src}/nested-a/f1.txt
printf 'two two' > {src}/nested-a/f2.txt
printf 'three' > {src}/nested-b/g1.txt
printf 'four four four' > {src}/nested-b/g2.txt
printf 'five' > {src}/nested-b/g3.txt
chmod 0600 {src}/a.txt
chmod 0644 {src}/b.txt
chmod 0755 {src}/c.txt
ln -s a.txt {src}/link.txt
echo OK
"#,
        src = src
    );
    let out = exec(adapter, session, &script).await;
    assert!(out.contains("OK"), "remote source-tree setup failed: {out}",);
    src
}

/// Local-side source-tree files mirrored above (rel-path -> sha256
/// of the bytes the script wrote). Used to verify the destination
/// tree matches byte-for-byte.
fn expected_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("a.txt", b"a".to_vec()),
        ("b.txt", b"hello world".to_vec()),
        (
            "c.txt",
            b"this is c with sixty bytes of payload .....".to_vec(),
        ),
        ("d.txt", vec![b'd'; 80]),
        ("e.txt", vec![b'e'; 100]),
        ("nested-a/f1.txt", b"one".to_vec()),
        ("nested-a/f2.txt", b"two two".to_vec()),
        ("nested-b/g1.txt", b"three".to_vec()),
        ("nested-b/g2.txt", b"four four four".to_vec()),
        ("nested-b/g3.txt", b"five".to_vec()),
    ]
}

/// Drain the RsyncProgressEvent lane until SyncCompleted / SessionFailed
/// (or a 30 s deadline). Returns every event seen.
async fn drain_events(
    transport: &SftpRsyncTransport<RusshRsyncSftpFs>,
    rsync_id: &ssh_mcp::domain::rsync_ids::RsyncId,
) -> Vec<RsyncProgressEvent> {
    let mut events = Vec::new();
    for _ in 0..2_000 {
        match timeout(Duration::from_secs(30), transport.recv_event(rsync_id)).await {
            Ok(Ok(Some(event))) => {
                let stop = matches!(
                    event,
                    RsyncProgressEvent::SyncCompleted { .. }
                        | RsyncProgressEvent::SessionFailed { .. }
                );
                events.push(event);
                if stop {
                    break;
                }
            }
            _ => break,
        }
    }
    events
}

/// Lower-case hex of `bytes`. Inlined so the test does not pull a new
/// hex crate into `dev-dependencies`.
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_nibble(b >> 4));
        out.push(hex_nibble(b & 0x0f));
    }
    out
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => char::from(b'0' + n),
        10..=15 => char::from(b'a' + n - 10),
        _ => '?',
    }
}

/// Compute sha256 of `bytes` using the workspace `sha2` dep.
fn local_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex_lower(&digest)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "e2e — requires SSH access to vm.services (or SSH_MCP_E2E_*); run with --features e2e-vm"]
async fn rsync_sftp_round_trip_against_real_vm() {
    // 1. SSH connect.
    let (adapter, registry, session) = connect_real().await;

    // 2. Pre-create the synthetic source tree on the remote host. The
    //    v7.0.0-alpha.4 SFTP transport walks both `src` and `dst`
    //    through the same `RsyncSftpFsPort`; a local-FS adapter is
    //    a follow-up slice. We mirror the same source layout the
    //    brief calls for, just on the remote side.
    let src = build_remote_source_tree(&adapter, &session).await;
    let dst = format!("/tmp/ssh-mcp-rsync-e2e-{}", Uuid::now_v7().simple());
    println!("e2e-vm: src = {src}, dst = {dst}");

    // 3. Wire the live SFTP rsync transport against the russh session.
    let fs = Arc::new(RusshRsyncSftpFs::new(registry));
    let opts = SftpRsyncOpts {
        delete: false,
        dry_run: false,
        bwlimit_bps: None,
        excludes: Vec::new(),
        includes: Vec::new(),
        file_list_limit: 1_000_000,
        direction: Direction::Push,
        preserve: PreserveFlags {
            perms: true,
            mtime: true,
            owner: false,
            group: false,
            links: true,
            hardlinks: false,
            sparse: false,
            devices: false,
        },
        force_transfer: false,
    };
    let transport = SftpRsyncTransport::<RusshRsyncSftpFs>::with_fs(fs, opts, 256);

    let outcome = transport
        .start_session(RsyncStartRequest {
            session_id: session.clone(),
            src: src.clone(),
            dst: dst.clone(),
            direction: RsyncDirection::Push,
            delete: false,
            preserve: PreserveFlags {
                perms: false,
                mtime: false,
                owner: false,
                group: false,
                links: false,
                hardlinks: false,
                sparse: false,
                devices: false,
            },
            dry_run: false,
            exclude: Vec::new(),
            include: Vec::new(),
        })
        .await
        .expect("start_session");

    // 4. Drain the lane.
    let events = drain_events(&transport, &outcome.rsync_id).await;
    println!("e2e-vm: drained {} events", events.len());
    for e in &events {
        println!("e2e-vm: event {e:?}");
    }

    // First event must be SessionStarted (transport=Sftp).
    assert!(
        matches!(
            events.first(),
            Some(RsyncProgressEvent::SessionStarted {
                transport: RsyncTransportKind::Sftp,
                ..
            }),
        ),
        "first event must be SessionStarted, got {:?}",
        events.first()
    );

    // Last event must be SyncCompleted (or surface a clear SessionFailed
    // with a human-readable detail line).
    match events.last() {
        Some(RsyncProgressEvent::SyncCompleted { stats }) => {
            println!("e2e-vm: sync_completed stats={stats:?}");
        }
        Some(RsyncProgressEvent::SessionFailed { code, detail }) => {
            // Cleanup before panicking so we leave the VM tidy.
            let _ = exec(&adapter, &session, &format!("rm -rf {dst}")).await;
            panic!("session failed: code={code} detail={detail}");
        }
        other => panic!("unexpected last event: {other:?}"),
    }

    // 5. Verify every file landed byte-identical via sha256sum.
    for (rel, expected_bytes) in expected_files() {
        let expected_hash = local_sha256(&expected_bytes);
        let remote_path = format!("{dst}/{rel}");
        let remote_out = exec(
            &adapter,
            &session,
            &format!("sha256sum {remote_path} 2>/dev/null | awk '{{print $1}}'"),
        )
        .await;
        let remote_hash = remote_out.trim();
        assert_eq!(
            expected_hash, remote_hash,
            "{rel}: expected sha256 != remote sha256 (expected={expected_hash}, remote={remote_hash})",
        );
    }

    // 6. Verify symlink resolved correctly.
    let link_target = exec(&adapter, &session, &format!("readlink {dst}/link.txt")).await;
    assert_eq!(link_target.trim(), "a.txt", "symlink target mismatch");

    // 7. Verify mode bits match for the entries we explicitly chmod-ed.
    #[cfg(unix)]
    {
        let mode_a = exec(&adapter, &session, &format!("stat -c '%a' {dst}/a.txt")).await;
        assert_eq!(mode_a.trim(), "600", "a.txt mode mismatch");
        let mode_c = exec(&adapter, &session, &format!("stat -c '%a' {dst}/c.txt")).await;
        assert_eq!(mode_c.trim(), "755", "c.txt mode mismatch");
    }

    // 8. Cleanup.
    let _ = exec(&adapter, &session, &format!("rm -rf {dst} {src}")).await;

    // Drop the transport's lane state.
    let _ = transport.close(&outcome.rsync_id).await;

    // Disconnect SSH session.
    let _ = adapter.disconnect(&session).await;

    println!("e2e-vm: PASS — bytes equal, modes match, symlink intact");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "e2e — requires SSH access to vm.services (or SSH_MCP_E2E_*); run with --features e2e-vm"]
async fn rsync_sftp_idempotent_second_pass_skips_unchanged() {
    let (adapter, registry, session) = connect_real().await;
    let src = build_remote_source_tree(&adapter, &session).await;
    let dst = format!("/tmp/ssh-mcp-rsync-e2e-idem-{}", Uuid::now_v7().simple());
    println!("e2e-vm-idem: src = {src}, dst = {dst}");

    let fs = Arc::new(RusshRsyncSftpFs::new(registry));
    let opts = SftpRsyncOpts {
        delete: false,
        dry_run: false,
        bwlimit_bps: None,
        excludes: Vec::new(),
        includes: Vec::new(),
        file_list_limit: 1_000_000,
        direction: Direction::Push,
        preserve: PreserveFlags {
            perms: true,
            mtime: true,
            owner: false,
            group: false,
            links: true,
            hardlinks: false,
            sparse: false,
            devices: false,
        },
        force_transfer: false,
    };
    let transport = SftpRsyncTransport::<RusshRsyncSftpFs>::with_fs(Arc::clone(&fs), opts, 256);

    // First pass — full transfer.
    let first = transport
        .start_session(RsyncStartRequest {
            session_id: session.clone(),
            src: src.clone(),
            dst: dst.clone(),
            direction: RsyncDirection::Push,
            delete: false,
            preserve: PreserveFlags {
                perms: false,
                mtime: false,
                owner: false,
                group: false,
                links: false,
                hardlinks: false,
                sparse: false,
                devices: false,
            },
            dry_run: false,
            exclude: Vec::new(),
            include: Vec::new(),
        })
        .await
        .expect("start_session 1");
    let first_events = drain_events(&transport, &first.rsync_id).await;
    let _ = transport.close(&first.rsync_id).await;
    let first_completed = first_events
        .iter()
        .filter(|e| matches!(e, RsyncProgressEvent::FileCompleted { .. }))
        .count();
    println!("e2e-vm-idem: first-pass FileCompleted count = {first_completed}");
    assert!(first_completed >= 5, "first pass should transfer files");

    // Second pass — identical inputs; should mostly skip.
    let second = transport
        .start_session(RsyncStartRequest {
            session_id: session.clone(),
            src: src.clone(),
            dst: dst.clone(),
            direction: RsyncDirection::Push,
            delete: false,
            preserve: PreserveFlags {
                perms: false,
                mtime: false,
                owner: false,
                group: false,
                links: false,
                hardlinks: false,
                sparse: false,
                devices: false,
            },
            dry_run: false,
            exclude: Vec::new(),
            include: Vec::new(),
        })
        .await
        .expect("start_session 2");
    let second_events = drain_events(&transport, &second.rsync_id).await;
    let _ = transport.close(&second.rsync_id).await;
    let second_skipped = second_events
        .iter()
        .filter(|e| matches!(e, RsyncProgressEvent::FileSkipped { .. }))
        .count();
    let second_completed = second_events
        .iter()
        .filter(|e| matches!(e, RsyncProgressEvent::FileCompleted { .. }))
        .count();
    println!("e2e-vm-idem: second-pass skipped={second_skipped} completed={second_completed}",);
    // The size+mtime comparator should classify almost every regular
    // file as a Skip on a no-op pass; allow a handful of completions
    // (e.g. setstat-only files when the comparator emits setstat for
    // mtime parity rounding).
    assert!(
        second_skipped >= 5,
        "second pass should skip the unchanged files (got {second_skipped})",
    );

    // Cleanup + disconnect.
    let _ = exec(&adapter, &session, &format!("rm -rf {dst} {src}")).await;
    let _ = adapter.disconnect(&session).await;
}
