//! Production adapter for [`OutputStreamPort`] backed by the lock-free
//! per-command and per-shell records owned by the
//! [`crate::adapters::ssh::russh_adapter::RusshAdapter`].
//!
//! ## Responsibility
//!
//! - Holds shared `Arc<DashMap>` references to the command and shell
//!   tables maintained by the russh adapter. The two adapters are wired
//!   in tandem at composition time so there is exactly one source of
//!   truth for in-flight state.
//! - For each call, looks the requested id up, snapshots the
//!   relevant `arc-swap` payload, and assembles an
//!   [`OutputSnapshot`]. The shard guard is dropped before the
//!   `arc-swap` load so no `.await` ever happens while holding it.
//!
//! ## Invariants
//!
//! - Every method is async-signature only — the body is fully
//!   synchronous because every step is lock-free. Returning a
//!   [`Future`](std::future::Future) keeps the adapter compatible with
//!   the [`OutputStreamPort`] trait surface (which is async to allow
//!   future fakes that *do* await on a debounce barrier).
//! - The reported `last_seq` is `0`. Sequence numbers are minted
//!   centrally by
//!   [`crate::ports::subscription::SubscriberRegistry::next_seq`] and
//!   bound to a `(ResourceKind, id)` pair; the producer record itself
//!   never holds them. Callers that need precise gap detection layer
//!   the registry value on top of the snapshot.
//! - The reported `byte_cursor` is the current snapshot length
//!   (`stdout.len() + stderr.len()` for commands; `data.len()` for
//!   shells). It is *not* the cumulative byte count since the producer
//!   started — neither the command nor the shell record retains that
//!   figure today. The
//!   [`crate::ports::subscription::SubscriberRegistry`] tracks
//!   per-subscriber cursors when full reconciliation is required.
//!
//! ## What the adapter does **not** do
//!
//! - It does not subscribe to the underlying broadcast channels. The
//!   live fan-out path goes through
//!   [`crate::ports::subscription::SubscriberRegistry`]; this adapter
//!   only services the snapshot path used by `resources/read`.
//! - It does not retain its own buffers. The russh adapter owns the
//!   `arc-swap` payloads; this adapter never clones them, only takes
//!   cheap `Arc` snapshots and copies bytes through `Bytes::copy_from_slice`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use dashmap::DashMap;

use crate::adapters::ssh::russh_adapter::{CommandRecord, RusshAdapter, ShellRecord};
use crate::domain::error::DomainError;
use crate::domain::ids::{CommandId, ShellId};
use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};

/// Production adapter implementing [`crate::ports::output_stream::OutputStreamPort`].
#[derive(Clone, Debug)]
pub struct RusshOutputAdapter {
    commands: Arc<DashMap<CommandId, CommandRecord>>,
    shells: Arc<DashMap<ShellId, ShellRecord>>,
}

impl RusshOutputAdapter {
    /// Build an adapter that shares the russh adapter's command and
    /// shell tables.
    ///
    /// Both `Arc`s are cloned cheaply; no state is duplicated. The
    /// pairing is established at composition time so the two adapters
    /// observe the same in-flight set.
    #[must_use]
    pub fn new(ssh: &RusshAdapter) -> Self {
        Self {
            commands: Arc::clone(ssh.command_table()),
            shells: Arc::clone(ssh.shell_table()),
        }
    }

    /// Build an adapter from the raw shared tables.
    ///
    /// Used exclusively by the in-module test suite to wire a
    /// stand-alone instance with a fresh pair of `DashMap`s. Production
    /// must prefer [`Self::new`], which inherits the russh adapter's
    /// own tables. The constructor is gated behind `#[cfg(test)]`
    /// because [`CommandRecord`] and [`ShellRecord`] are crate-private
    /// records — exposing the helper to production callers would either
    /// require those records to leak `pub` or force a runtime helper
    /// nobody currently needs.
    #[cfg(test)]
    #[must_use]
    fn from_tables(
        commands: Arc<DashMap<CommandId, CommandRecord>>,
        shells: Arc<DashMap<ShellId, ShellRecord>>,
    ) -> Self {
        Self { commands, shells }
    }

    /// Number of registered commands. Test/observability helper.
    #[must_use]
    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    /// Number of registered shells. Test/observability helper.
    #[must_use]
    pub fn shell_count(&self) -> usize {
        self.shells.len()
    }
}

impl OutputStreamPort for RusshOutputAdapter {
    async fn snapshot_command(&self, id: &CommandId) -> Result<OutputSnapshot, DomainError> {
        // Look the record up, clone the `Arc<RunningCommand>`, drop
        // the shard guard before the snapshot load. No `.await` runs
        // while the guard is held.
        let history = {
            let entry = self
                .commands
                .get(id)
                .ok_or_else(|| DomainError::CommandNotFound(id.clone()))?;
            Arc::clone(&entry.value().running.output_history)
        };
        let buf = history.load_full();
        let stdout = Bytes::copy_from_slice(&buf.stdout);
        let stderr = Bytes::copy_from_slice(&buf.stderr);
        let byte_cursor =
            u64::try_from(stdout.len().saturating_add(stderr.len())).unwrap_or(u64::MAX);
        Ok(OutputSnapshot {
            byte_cursor,
            last_seq: 0,
            stdout,
            stderr,
        })
    }

    async fn snapshot_shell(&self, id: &ShellId) -> Result<OutputSnapshot, DomainError> {
        let history = {
            let entry = self
                .shells
                .get(id)
                .ok_or_else(|| DomainError::ShellNotFound(id.clone()))?;
            Arc::clone(&entry.value().running.history)
        };
        let buf = history.load_full();
        let stdout = buf.data.clone();
        let byte_cursor = u64::try_from(stdout.len()).unwrap_or(u64::MAX);
        Ok(OutputSnapshot {
            byte_cursor,
            last_seq: 0,
            stdout,
            stderr: Bytes::new(),
        })
    }

    async fn shell_produced_total(&self, id: &ShellId) -> Result<u64, DomainError> {
        // Read the true cumulative producer counter (incremented before
        // head-truncation) rather than the current snapshot length, which
        // pins at the buffer cap. Drop the shard guard before the atomic
        // load; no `.await` runs while it is held.
        let produced = {
            let entry = self
                .shells
                .get(id)
                .ok_or_else(|| DomainError::ShellNotFound(id.clone()))?;
            Arc::clone(&entry.value().running.produced_total)
        };
        Ok(produced.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::{Bytes, RusshOutputAdapter};
    use crate::adapters::ssh::internal::async_command::{OutputBuffer, RunningCommand};
    use crate::adapters::ssh::internal::shell::{RingBuffer, RunningShell, WriteRequest};
    use crate::adapters::ssh::internal::types::{AsyncCommandInfo, AsyncCommandStatus};
    use crate::adapters::ssh::russh_adapter::{CommandRecord, ShellRecord};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{CommandId, SessionId, ShellId};
    use crate::ports::output_stream::OutputStreamPort;
    use dashmap::DashMap;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn _assert_port<T: OutputStreamPort>() {}

    fn sample_command(session_id: &str, command_id: &str) -> Arc<RunningCommand> {
        Arc::new(RunningCommand::new(AsyncCommandInfo {
            command_id: command_id.to_string(),
            session_id: session_id.to_string(),
            command: "echo".to_string(),
            status: AsyncCommandStatus::Running,
            started_at: "2024-01-15T10:30:00Z".to_string(),
        }))
    }

    fn sample_shell(_session_id: &str, _shell_id: &str) -> Arc<RunningShell> {
        Arc::new(RunningShell::new(
            CancellationToken::new(),
            10 * 1024 * 1024,
        ))
    }

    fn build_adapter_with(
        command: Option<(CommandId, SessionId, Arc<RunningCommand>)>,
        shell: Option<(ShellId, SessionId, Arc<RunningShell>)>,
    ) -> RusshOutputAdapter {
        let commands: Arc<DashMap<CommandId, CommandRecord>> = Arc::new(DashMap::new());
        let shells: Arc<DashMap<ShellId, ShellRecord>> = Arc::new(DashMap::new());
        if let Some((cid, sid, running)) = command {
            commands.insert(
                cid,
                CommandRecord {
                    session_id: sid,
                    running,
                },
            );
        }
        if let Some((sh_id, sid, running)) = shell {
            let (input_tx, _rx) = mpsc::channel::<WriteRequest>(8);
            shells.insert(
                sh_id,
                ShellRecord {
                    session_id: sid,
                    input_tx,
                    cancel_token: CancellationToken::new(),
                    running,
                },
            );
        }
        RusshOutputAdapter::from_tables(commands, shells)
    }

    #[test]
    fn adapter_satisfies_output_stream_port_send_alias() {
        _assert_port::<RusshOutputAdapter>();
    }

    #[test]
    fn adapter_is_send_sync_clone_debug() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_clone<T: Clone>() {}
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_send_sync::<RusshOutputAdapter>();
        assert_clone::<RusshOutputAdapter>();
        assert_debug::<RusshOutputAdapter>();
    }

    #[test]
    fn empty_adapter_starts_with_zero_state() {
        let adapter = build_adapter_with(None, None);
        assert_eq!(adapter.command_count(), 0);
        assert_eq!(adapter.shell_count(), 0);
    }

    #[test]
    fn from_tables_shares_arcs_with_caller() {
        let commands: Arc<DashMap<CommandId, CommandRecord>> = Arc::new(DashMap::new());
        let shells: Arc<DashMap<ShellId, ShellRecord>> = Arc::new(DashMap::new());
        let adapter = RusshOutputAdapter::from_tables(Arc::clone(&commands), Arc::clone(&shells));
        // Insert through the external handle, observe through the adapter.
        let cid = CommandId::new("cmd-x".to_string());
        commands.insert(
            cid,
            CommandRecord {
                session_id: SessionId::new("sess-x".to_string()),
                running: sample_command("sess-x", "cmd-x"),
            },
        );
        assert_eq!(adapter.command_count(), 1);
    }

    #[tokio::test]
    async fn snapshot_command_unknown_id_returns_command_not_found() {
        let adapter = build_adapter_with(None, None);
        let id = CommandId::new("absent".to_string());
        match adapter.snapshot_command(&id).await {
            Ok(_) => panic!("missing command id must fail"),
            Err(DomainError::CommandNotFound(reported)) => assert_eq!(reported, id),
            Err(other) => panic!("expected CommandNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_shell_unknown_id_returns_shell_not_found() {
        let adapter = build_adapter_with(None, None);
        let id = ShellId::new("absent".to_string());
        match adapter.snapshot_shell(&id).await {
            Ok(_) => panic!("missing shell id must fail"),
            Err(DomainError::ShellNotFound(reported)) => assert_eq!(reported, id),
            Err(other) => panic!("expected ShellNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_command_returns_empty_payload_for_fresh_record() {
        let cid = CommandId::new("cmd-1".to_string());
        let sid = SessionId::new("sess-1".to_string());
        let running = sample_command("sess-1", "cmd-1");
        let adapter = build_adapter_with(Some((cid.clone(), sid, Arc::clone(&running))), None);
        match adapter.snapshot_command(&cid).await {
            Ok(snap) => {
                assert!(snap.stdout.is_empty());
                assert!(snap.stderr.is_empty());
                assert_eq!(snap.last_seq, 0);
                assert_eq!(snap.byte_cursor, 0);
            }
            Err(e) => panic!("expected snapshot, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_command_reflects_published_buffer() {
        let cid = CommandId::new("cmd-2".to_string());
        let sid = SessionId::new("sess-2".to_string());
        let running = sample_command("sess-2", "cmd-2");

        // Publish a fresh OutputBuffer with both stdout and stderr.
        let mut buf = OutputBuffer::default();
        buf.append_stdout_bounded(b"hello stdout", 0);
        buf.append_stderr_bounded(b"oops stderr", 0);
        running.output_history.store(Arc::new(buf));

        let adapter = build_adapter_with(Some((cid.clone(), sid, running)), None);
        match adapter.snapshot_command(&cid).await {
            Ok(snap) => {
                assert_eq!(snap.stdout.as_ref(), b"hello stdout");
                assert_eq!(snap.stderr.as_ref(), b"oops stderr");
                let want =
                    u64::try_from(b"hello stdout".len() + b"oops stderr".len()).unwrap_or(u64::MAX);
                assert_eq!(snap.byte_cursor, want);
                assert_eq!(snap.last_seq, 0);
            }
            Err(e) => panic!("expected snapshot, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_shell_returns_empty_payload_for_fresh_record() {
        let sh_id = ShellId::new("shell-1".to_string());
        let sid = SessionId::new("sess-1".to_string());
        let running = sample_shell("sess-1", "shell-1");
        let adapter = build_adapter_with(None, Some((sh_id.clone(), sid, running)));
        match adapter.snapshot_shell(&sh_id).await {
            Ok(snap) => {
                assert!(snap.stdout.is_empty());
                assert!(snap.stderr.is_empty());
                assert_eq!(snap.byte_cursor, 0);
                assert_eq!(snap.last_seq, 0);
            }
            Err(e) => panic!("expected snapshot, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_shell_reflects_published_history() {
        let sh_id = ShellId::new("shell-2".to_string());
        let sid = SessionId::new("sess-2".to_string());
        let running = sample_shell("sess-2", "shell-2");
        running.history.store(Arc::new(RingBuffer {
            data: Bytes::from_static(b"login banner"),
        }));

        let adapter = build_adapter_with(None, Some((sh_id.clone(), sid, running)));
        match adapter.snapshot_shell(&sh_id).await {
            Ok(snap) => {
                assert_eq!(snap.stdout.as_ref(), b"login banner");
                assert!(snap.stderr.is_empty());
                let want = u64::try_from(b"login banner".len()).unwrap_or(u64::MAX);
                assert_eq!(snap.byte_cursor, want);
                assert_eq!(snap.last_seq, 0);
            }
            Err(e) => panic!("expected snapshot, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_command_observes_subsequent_publish() {
        let cid = CommandId::new("cmd-3".to_string());
        let sid = SessionId::new("sess-3".to_string());
        let running = sample_command("sess-3", "cmd-3");
        let adapter = build_adapter_with(Some((cid.clone(), sid, Arc::clone(&running))), None);
        // First snapshot is empty.
        match adapter.snapshot_command(&cid).await {
            Ok(snap) => assert_eq!(snap.byte_cursor, 0),
            Err(e) => panic!("expected first snapshot, got {e:?}"),
        }
        // Publish, then re-snapshot.
        let mut buf = OutputBuffer::default();
        buf.append_stdout_bounded(b"abc", 0);
        running.output_history.store(Arc::new(buf));
        match adapter.snapshot_command(&cid).await {
            Ok(snap) => {
                assert_eq!(snap.stdout.as_ref(), b"abc");
                assert_eq!(snap.byte_cursor, 3);
            }
            Err(e) => panic!("expected second snapshot, got {e:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_shell_observes_subsequent_publish() {
        let sh_id = ShellId::new("shell-3".to_string());
        let sid = SessionId::new("sess-3".to_string());
        let running = sample_shell("sess-3", "shell-3");
        let adapter = build_adapter_with(None, Some((sh_id.clone(), sid, Arc::clone(&running))));
        match adapter.snapshot_shell(&sh_id).await {
            Ok(snap) => assert_eq!(snap.byte_cursor, 0),
            Err(e) => panic!("expected first snapshot, got {e:?}"),
        }
        running.history.store(Arc::new(RingBuffer {
            data: Bytes::from_static(b"# "),
        }));
        match adapter.snapshot_shell(&sh_id).await {
            Ok(snap) => {
                assert_eq!(snap.stdout.as_ref(), b"# ");
                assert_eq!(snap.byte_cursor, 2);
            }
            Err(e) => panic!("expected second snapshot, got {e:?}"),
        }
    }
}
