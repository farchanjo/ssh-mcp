//! Test-only [`SshClientPort`] adapter with scripted, deterministic outcomes.
//!
//! [`FakeSshClient`] records every call (`connect`, `execute`, `disconnect`,
//! `health_check`, `open_shell`, `write_shell`, `close_shell`) into a shared
//! log so tests can assert the orchestration issued exactly the operations
//! expected. Outcomes are scripted per call kind via FIFO queues:
//!
//! - **Connect outcomes**: pushed via [`FakeSshClient::queue_connect_ok`] /
//!   [`FakeSshClient::queue_connect_error`] in FIFO order. Each `connect`
//!   call pops the head; an empty queue yields a default success.
//! - **Health outcomes**: pushed via [`FakeSshClient::queue_health_ok`] /
//!   [`FakeSshClient::queue_health_fail`] in FIFO order. Each `health_check`
//!   call pops the head; an empty queue defaults to success.
//! - **Disconnect outcomes**: pushed via [`FakeSshClient::queue_disconnect_ok`] /
//!   [`FakeSshClient::queue_disconnect_error`] in FIFO order. Each
//!   `disconnect` call pops the head; an empty queue defaults to success.
//! - **`execute_async` outcomes**: pushed via
//!   [`FakeSshClient::queue_execute_async_ok`] /
//!   [`FakeSshClient::queue_execute_async_error`] in FIFO order. Each
//!   `execute_async` call pops the head; an empty queue defaults to a
//!   success that mints a [`CommandHandle`] from the supplied request.
//! - **`open_shell` outcomes**: pushed via
//!   [`FakeSshClient::queue_open_shell_ok`] /
//!   [`FakeSshClient::queue_open_shell_error`] in FIFO order. Each
//!   `open_shell` call pops the head; an empty queue defaults to a success
//!   that mints a [`ShellEntity`] from the supplied terminal description.
//! - **`write_shell` outcomes**: pushed via
//!   [`FakeSshClient::queue_write_shell_ok`] /
//!   [`FakeSshClient::queue_write_shell_error`] in FIFO order. Each
//!   `write_shell` call pops the head; an empty queue defaults to success
//!   reporting `bytes.len()` written.
//! - **`close_shell` outcomes**: pushed via
//!   [`FakeSshClient::queue_close_shell_ok`] /
//!   [`FakeSshClient::queue_close_shell_error`] in FIFO order. Each
//!   `close_shell` call pops the head; an empty queue defaults to success.
//!
//! The remaining port methods (`execute`, `cancel`) are not exercised by the
//! current crop of use cases but the trait surface is satisfied with benign
//! defaults so accidental usage in tests surfaces immediately rather than
//! silently passing.
//!
//! The whole module is gated behind `#[cfg(any(test, feature = "test-fixtures"))]`
//! so it never reaches a release binary.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;

use crate::domain::command::{CommandEntity, CommandRequest};
use crate::domain::error::DomainError;
use crate::domain::identity::{Address, Credentials};
use crate::domain::ids::{CommandId, SessionId, ShellId};
use crate::domain::session::SessionEntity;
use crate::domain::shell::{ShellEntity, ShellTerminal};
use crate::ports::ssh_client::{CommandHandle, CommandOutcome, SshClientPort};

/// Default rolling buffer size handed back by [`FakeSshClient::open_shell`]
/// when a test does not customise it. Mirrors the production default in
/// `russh_adapter::DEFAULT_SHELL_BUFFER_SIZE` so tests stay representative.
const FAKE_SHELL_DEFAULT_BUFFER_BYTES: u64 = 10_u64.saturating_mul(1024).saturating_mul(1024);

/// Default inactivity TTL handed back by [`FakeSshClient::open_shell`].
/// Mirrors the production default of 300 seconds (`SSH_INACTIVITY_TIMEOUT`).
const FAKE_SHELL_DEFAULT_INACTIVITY_TTL: Duration = Duration::from_secs(300);

/// Single recorded interaction for assertion purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeSshCall {
    /// `connect(session_id, host:port, username)` was invoked.
    Connect {
        /// Caller-supplied session id (the use case mints this via
        /// [`crate::ports::id_generator::IdGeneratorPort`]).
        session_id: SessionId,
        /// Target endpoint.
        address: Address,
        /// Login username.
        username: String,
    },
    /// `disconnect(session_id)` was invoked.
    Disconnect {
        /// Session that was torn down.
        session_id: SessionId,
    },
    /// `execute(session_id, command)` was invoked.
    Execute {
        /// Session that ran the command.
        session_id: SessionId,
        /// Verbatim command line.
        command: String,
    },
    /// `execute_async(command_id, request)` was invoked. The H12a use case
    /// hands the freshly minted command id straight to the port, so the call
    /// log captures the id together with the routing-relevant request fields
    /// (session, command, optional timeout, pty flag).
    ExecuteAsync {
        /// Use-case-minted command id.
        command_id: CommandId,
        /// Session that owns the spawned channel.
        session_id: SessionId,
        /// Verbatim command line.
        command: String,
        /// Optional override for the per-session default timeout.
        timeout: Option<Duration>,
        /// Whether the channel must allocate a PTY.
        pty: bool,
    },
    /// `health_check(session_id)` was invoked.
    HealthCheck {
        /// Probed session.
        session_id: SessionId,
    },
    /// `open_shell(session_id, terminal, max_buffer_size)` was invoked.
    /// The H13 shell open use case hands the terminal description through
    /// unchanged so the call log captures the negotiated `TERM` value
    /// and the initial PTY geometry alongside the owning session.
    /// `max_buffer_size` records the per-call rolling-buffer override
    /// (None means "use the adapter default" — see v4.7.1 BUG #1 fix).
    OpenShell {
        /// Session that owns the spawned shell.
        session_id: SessionId,
        /// Negotiated `TERM` value.
        term: String,
        /// Initial PTY width in columns.
        cols: u32,
        /// Initial PTY height in rows.
        rows: u32,
        /// Caller-supplied rolling buffer cap (bytes) when present.
        max_buffer_size: Option<u64>,
    },
    /// `write_shell(shell_id, bytes)` was invoked. The H13 shell write
    /// and key-press use cases route every input frame through the same
    /// trait method so the call log captures the target shell together
    /// with the verbatim bytes — tests can therefore assert both the
    /// payload and its byte count without owning a copy of the buffer.
    WriteShell {
        /// Target shell.
        shell_id: ShellId,
        /// Verbatim bytes handed to the port.
        bytes: Bytes,
    },
    /// `close_shell(shell_id)` was invoked. The H13 shell close use case
    /// fires this method exactly once per shell teardown so the call log
    /// captures the target id without any extra payload.
    CloseShell {
        /// Shell that was torn down.
        shell_id: ShellId,
    },
}

/// Scripted outcome for a `connect` call.
#[derive(Debug, Clone)]
enum ConnectOutcome {
    /// Connect succeeds with the supplied retry attempts.
    Ok { retry_attempts: u32 },
    /// Connect fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for a `health_check` call.
#[derive(Debug, Clone)]
enum HealthOutcome {
    /// Probe succeeds.
    Ok,
    /// Probe fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for a `disconnect` call.
#[derive(Debug, Clone)]
enum DisconnectOutcome {
    /// Disconnect succeeds.
    Ok,
    /// Disconnect fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for an `execute_async` call.
#[derive(Debug, Clone)]
enum ExecuteAsyncOutcome {
    /// `execute_async` succeeds. The fake mints the [`CommandHandle`] from
    /// the supplied request so tests do not have to plumb a [`CommandEntity`]
    /// builder through the queue.
    Ok,
    /// `execute_async` fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for an `open_shell` call. The fake mints the
/// returned [`ShellEntity`] from the queued shell id together with the
/// session id and terminal description supplied by the caller, so tests
/// only need to seed the queue with the id (or an explicit error).
#[derive(Debug, Clone)]
enum OpenShellOutcome {
    /// `open_shell` succeeds and binds the supplied [`ShellId`].
    Ok { shell_id: ShellId },
    /// `open_shell` fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for a `write_shell` call. The success variant mirrors
/// the contract of [`crate::ports::ssh_client::SshClientPort::write_shell`]
/// — the port returns the number of bytes the writer accepted, which is
/// exactly `bytes.len()` for the production adapter.
#[derive(Debug, Clone)]
enum WriteShellOutcome {
    /// `write_shell` succeeds and reports `bytes.len()` written.
    Ok,
    /// `write_shell` fails with the supplied domain error.
    Err(DomainError),
}

/// Scripted outcome for a `close_shell` call.
#[derive(Debug, Clone)]
enum CloseShellOutcome {
    /// `close_shell` succeeds.
    Ok,
    /// `close_shell` fails with the supplied domain error.
    Err(DomainError),
}

/// Test [`SshClientPort`] adapter. Cloneable; clones share the same
/// scripted state and the same call log via [`Arc`].
#[derive(Debug, Clone, Default)]
pub struct FakeSshClient {
    inner: Arc<FakeSshClientInner>,
}

#[derive(Debug, Default)]
struct FakeSshClientInner {
    /// Scripted `connect` outcomes, popped FIFO.
    connect_queue: Mutex<Vec<ConnectOutcome>>,
    /// Scripted `health_check` outcomes, popped FIFO.
    health_queue: Mutex<Vec<HealthOutcome>>,
    /// Scripted `disconnect` outcomes, popped FIFO.
    disconnect_queue: Mutex<Vec<DisconnectOutcome>>,
    /// Scripted `execute_async` outcomes, popped FIFO. An empty queue
    /// defaults to success so tests that only care about call recording do
    /// not need to seed the queue first.
    execute_async_queue: Mutex<Vec<ExecuteAsyncOutcome>>,
    /// Scripted `open_shell` outcomes, popped FIFO. An empty queue
    /// defaults to a success that mints a deterministic shell id of the
    /// shape `<session>-shell-<n>` (n = `shell_open_counter` + 1).
    open_shell_queue: Mutex<Vec<OpenShellOutcome>>,
    /// Scripted `write_shell` outcomes, popped FIFO.
    write_shell_queue: Mutex<Vec<WriteShellOutcome>>,
    /// Scripted `close_shell` outcomes, popped FIFO.
    close_shell_queue: Mutex<Vec<CloseShellOutcome>>,
    /// Monotonic counter used to mint default shell ids when the
    /// `open_shell_queue` is empty. Starts at zero and increments after
    /// each defaulted call so the issued ids stay unique within a single
    /// test process.
    shell_open_counter: Mutex<u64>,
    /// Append-only log of every recorded call.
    calls: Mutex<Vec<FakeSshCall>>,
}

impl FakeSshClient {
    /// Build an empty fake. Defaults: every `connect` succeeds with zero
    /// retries; every `health_check` succeeds.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a successful `connect` outcome with the supplied retry count.
    pub fn queue_connect_ok(&self, retry_attempts: u32) {
        Self::push(
            &self.inner.connect_queue,
            ConnectOutcome::Ok { retry_attempts },
        );
    }

    /// Queue a failed `connect` outcome with the supplied domain error.
    pub fn queue_connect_error(&self, error: DomainError) {
        Self::push(&self.inner.connect_queue, ConnectOutcome::Err(error));
    }

    /// Queue a successful `health_check` outcome.
    pub fn queue_health_ok(&self) {
        Self::push(&self.inner.health_queue, HealthOutcome::Ok);
    }

    /// Queue a failed `health_check` outcome with the supplied domain error.
    pub fn queue_health_fail(&self, error: DomainError) {
        Self::push(&self.inner.health_queue, HealthOutcome::Err(error));
    }

    /// Queue a successful `disconnect` outcome.
    pub fn queue_disconnect_ok(&self) {
        Self::push(&self.inner.disconnect_queue, DisconnectOutcome::Ok);
    }

    /// Queue a failed `disconnect` outcome with the supplied domain error.
    pub fn queue_disconnect_error(&self, error: DomainError) {
        Self::push(&self.inner.disconnect_queue, DisconnectOutcome::Err(error));
    }

    /// Queue a successful `execute_async` outcome. The fake derives the
    /// returned [`CommandHandle`] from the request handed to the trait
    /// method, so callers script behaviour purely through queue order.
    pub fn queue_execute_async_ok(&self) {
        Self::push(&self.inner.execute_async_queue, ExecuteAsyncOutcome::Ok);
    }

    /// Queue a failed `execute_async` outcome with the supplied domain error.
    pub fn queue_execute_async_error(&self, error: DomainError) {
        Self::push(
            &self.inner.execute_async_queue,
            ExecuteAsyncOutcome::Err(error),
        );
    }

    /// Queue a successful `open_shell` outcome that binds the supplied
    /// [`ShellId`]. The fake mints the returned [`ShellEntity`] from the
    /// session id and terminal description handed to the trait method,
    /// so callers script behaviour purely through the queue.
    pub fn queue_open_shell_ok(&self, shell_id: ShellId) {
        Self::push(
            &self.inner.open_shell_queue,
            OpenShellOutcome::Ok { shell_id },
        );
    }

    /// Queue a failed `open_shell` outcome with the supplied domain error.
    pub fn queue_open_shell_error(&self, error: DomainError) {
        Self::push(&self.inner.open_shell_queue, OpenShellOutcome::Err(error));
    }

    /// Queue a successful `write_shell` outcome.
    pub fn queue_write_shell_ok(&self) {
        Self::push(&self.inner.write_shell_queue, WriteShellOutcome::Ok);
    }

    /// Queue a failed `write_shell` outcome with the supplied domain error.
    pub fn queue_write_shell_error(&self, error: DomainError) {
        Self::push(&self.inner.write_shell_queue, WriteShellOutcome::Err(error));
    }

    /// Queue a successful `close_shell` outcome.
    pub fn queue_close_shell_ok(&self) {
        Self::push(&self.inner.close_shell_queue, CloseShellOutcome::Ok);
    }

    /// Queue a failed `close_shell` outcome with the supplied domain error.
    pub fn queue_close_shell_error(&self, error: DomainError) {
        Self::push(&self.inner.close_shell_queue, CloseShellOutcome::Err(error));
    }

    /// Snapshot of every recorded call in invocation order.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeSshCall> {
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

    fn record(&self, call: FakeSshCall) {
        if let Ok(mut log) = self.inner.calls.lock() {
            log.push(call);
        }
    }

    fn pop_connect_outcome(&self) -> ConnectOutcome {
        self.inner.connect_queue.lock().map_or_else(
            |_| ConnectOutcome::Ok { retry_attempts: 0 },
            |mut guard| {
                if guard.is_empty() {
                    ConnectOutcome::Ok { retry_attempts: 0 }
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_health_outcome(&self) -> HealthOutcome {
        self.inner.health_queue.lock().map_or_else(
            |_| HealthOutcome::Ok,
            |mut guard| {
                if guard.is_empty() {
                    HealthOutcome::Ok
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_disconnect_outcome(&self) -> DisconnectOutcome {
        self.inner.disconnect_queue.lock().map_or_else(
            |_| DisconnectOutcome::Ok,
            |mut guard| {
                if guard.is_empty() {
                    DisconnectOutcome::Ok
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_execute_async_outcome(&self) -> ExecuteAsyncOutcome {
        self.inner.execute_async_queue.lock().map_or_else(
            |_| ExecuteAsyncOutcome::Ok,
            |mut guard| {
                if guard.is_empty() {
                    ExecuteAsyncOutcome::Ok
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_open_shell_outcome(&self, session_id: &SessionId) -> OpenShellOutcome {
        self.inner.open_shell_queue.lock().map_or_else(
            |_| OpenShellOutcome::Ok {
                shell_id: self.mint_default_shell_id(session_id),
            },
            |mut guard| {
                if guard.is_empty() {
                    OpenShellOutcome::Ok {
                        shell_id: self.mint_default_shell_id(session_id),
                    }
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_write_shell_outcome(&self) -> WriteShellOutcome {
        self.inner.write_shell_queue.lock().map_or_else(
            |_| WriteShellOutcome::Ok,
            |mut guard| {
                if guard.is_empty() {
                    WriteShellOutcome::Ok
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    fn pop_close_shell_outcome(&self) -> CloseShellOutcome {
        self.inner.close_shell_queue.lock().map_or_else(
            |_| CloseShellOutcome::Ok,
            |mut guard| {
                if guard.is_empty() {
                    CloseShellOutcome::Ok
                } else {
                    guard.remove(0)
                }
            },
        )
    }

    /// Mint a default shell id for an `open_shell` call that is not
    /// pre-scripted. Mirrors the production shape `<session>-shell-<n>`
    /// used by [`crate::adapters::ssh::russh_adapter::RusshAdapter`] so
    /// tests reading back the id observe the same convention.
    fn mint_default_shell_id(&self, session_id: &SessionId) -> ShellId {
        let next = self.inner.shell_open_counter.lock().map_or_else(
            |poison| {
                let mut guard = poison.into_inner();
                *guard = guard.saturating_add(1);
                *guard
            },
            |mut guard| {
                *guard = guard.saturating_add(1);
                *guard
            },
        );
        ShellId::new(format!("{}-shell-{next}", session_id.as_str()))
    }
}

impl SshClientPort for FakeSshClient {
    async fn connect(
        &self,
        session_id: SessionId,
        address: Address,
        credentials: Credentials,
        connect_timeout: Duration,
    ) -> Result<SessionEntity, DomainError> {
        let username = credentials.username().to_string();
        self.record(FakeSshCall::Connect {
            session_id: session_id.clone(),
            address: address.clone(),
            username: username.clone(),
        });
        match self.pop_connect_outcome() {
            ConnectOutcome::Ok { retry_attempts } => Ok(SessionEntity {
                id: session_id,
                name: None,
                agent_id: None,
                address,
                username,
                connected_at: Utc::now(),
                default_timeout: connect_timeout,
                retry_attempts,
                compression_enabled: true,
                last_health_check: None,
                healthy: None,
            }),
            ConnectOutcome::Err(err) => Err(err),
        }
    }

    async fn disconnect(&self, session_id: &SessionId) -> Result<(), DomainError> {
        self.record(FakeSshCall::Disconnect {
            session_id: session_id.clone(),
        });
        match self.pop_disconnect_outcome() {
            DisconnectOutcome::Ok => Ok(()),
            DisconnectOutcome::Err(err) => Err(err),
        }
    }

    async fn execute(&self, request: CommandRequest) -> Result<CommandOutcome, DomainError> {
        self.record(FakeSshCall::Execute {
            session_id: request.session_id.clone(),
            command: request.command.clone(),
        });
        // The H10 use case routes liveness through `health_check`; if a future
        // use case scripts `execute` we can extend the queues. Default to a
        // benign success so this branch never trips an unrelated test.
        Ok(CommandOutcome {
            exit_code: Some(0),
            stdout: Bytes::new(),
            stderr: Bytes::new(),
            timed_out: false,
        })
    }

    async fn execute_async(
        &self,
        command_id: CommandId,
        request: CommandRequest,
    ) -> Result<CommandHandle, DomainError> {
        self.record(FakeSshCall::ExecuteAsync {
            command_id: command_id.clone(),
            session_id: request.session_id.clone(),
            command: request.command.clone(),
            timeout: request.timeout,
            pty: request.pty,
        });
        match self.pop_execute_async_outcome() {
            ExecuteAsyncOutcome::Ok => Ok(CommandHandle {
                entity: CommandEntity::new(
                    command_id,
                    request.session_id,
                    request.command,
                    Utc::now(),
                ),
            }),
            ExecuteAsyncOutcome::Err(err) => Err(err),
        }
    }

    async fn cancel(&self, command_id: &CommandId) -> Result<(), DomainError> {
        Err(DomainError::CommandNotFound(command_id.clone()))
    }

    async fn open_shell(
        &self,
        session_id: &SessionId,
        terminal: ShellTerminal,
        max_buffer_size: Option<u64>,
    ) -> Result<ShellEntity, DomainError> {
        self.record(FakeSshCall::OpenShell {
            session_id: session_id.clone(),
            term: terminal.term_type.clone(),
            cols: terminal.cols,
            rows: terminal.rows,
            max_buffer_size,
        });
        match self.pop_open_shell_outcome(session_id) {
            OpenShellOutcome::Ok { shell_id } => Ok(ShellEntity::new(
                shell_id,
                session_id.clone(),
                terminal,
                Utc::now(),
                FAKE_SHELL_DEFAULT_INACTIVITY_TTL,
                max_buffer_size.unwrap_or(FAKE_SHELL_DEFAULT_BUFFER_BYTES),
            )),
            OpenShellOutcome::Err(err) => Err(err),
        }
    }

    async fn write_shell(&self, shell_id: &ShellId, bytes: Bytes) -> Result<usize, DomainError> {
        let len = bytes.len();
        self.record(FakeSshCall::WriteShell {
            shell_id: shell_id.clone(),
            bytes,
        });
        match self.pop_write_shell_outcome() {
            WriteShellOutcome::Ok => Ok(len),
            WriteShellOutcome::Err(err) => Err(err),
        }
    }

    async fn close_shell(&self, shell_id: &ShellId) -> Result<(), DomainError> {
        self.record(FakeSshCall::CloseShell {
            shell_id: shell_id.clone(),
        });
        match self.pop_close_shell_outcome() {
            CloseShellOutcome::Ok => Ok(()),
            CloseShellOutcome::Err(err) => Err(err),
        }
    }

    async fn health_check(&self, session_id: &SessionId) -> Result<(), DomainError> {
        self.record(FakeSshCall::HealthCheck {
            session_id: session_id.clone(),
        });
        match self.pop_health_outcome() {
            HealthOutcome::Ok => Ok(()),
            HealthOutcome::Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FakeSshCall, FakeSshClient};
    use crate::domain::error::DomainError;
    use crate::domain::identity::{Address, Credentials};
    use crate::domain::ids::{SessionId, ShellId};
    use crate::domain::shell::{ShellStatus, ShellTerminal};
    use crate::ports::ssh_client::SshClientPort;
    use bytes::Bytes;
    use std::time::Duration;

    fn sample_creds() -> Credentials {
        Credentials::Password {
            username: "alice".to_string(),
            password: "x".to_string(),
        }
    }

    fn sample_address() -> Address {
        Address::new("h".to_string(), 22).expect("address")
    }

    #[tokio::test]
    async fn default_connect_succeeds_with_zero_retries() {
        let client = FakeSshClient::new();
        let entity = client
            .connect(
                SessionId::new("s-1".to_string()),
                sample_address(),
                sample_creds(),
                Duration::from_secs(30),
            )
            .await
            .expect("connect ok");
        assert_eq!(entity.id.as_str(), "s-1");
        assert_eq!(entity.retry_attempts, 0);
    }

    #[tokio::test]
    async fn queued_connect_error_propagates() {
        let client = FakeSshClient::new();
        client.queue_connect_error(DomainError::ConnectFailed("boom".to_string()));
        let err = client
            .connect(
                SessionId::new("s-1".to_string()),
                sample_address(),
                sample_creds(),
                Duration::from_secs(30),
            )
            .await
            .expect_err("expected error");
        assert_eq!(err, DomainError::ConnectFailed("boom".to_string()));
    }

    #[tokio::test]
    async fn calls_log_records_connect_disconnect_and_health() {
        let client = FakeSshClient::new();
        let sid = SessionId::new("s-1".to_string());
        let _ = client
            .connect(
                sid.clone(),
                sample_address(),
                sample_creds(),
                Duration::from_secs(30),
            )
            .await
            .expect("connect ok");
        client.health_check(&sid).await.expect("health ok");
        client.disconnect(&sid).await.expect("disconnect ok");
        let log = client.calls();
        assert_eq!(log.len(), 3);
        assert!(matches!(log[0], FakeSshCall::Connect { .. }));
        assert!(matches!(log[1], FakeSshCall::HealthCheck { .. }));
        assert!(matches!(log[2], FakeSshCall::Disconnect { .. }));
    }

    fn sample_terminal() -> ShellTerminal {
        ShellTerminal::new("xterm-256color".to_string(), 80, 24)
    }

    #[tokio::test]
    async fn default_open_shell_mints_session_scoped_id_and_records_terminal() {
        let client = FakeSshClient::new();
        let sid = SessionId::new("sess-shell".to_string());
        let entity = client
            .open_shell(&sid, sample_terminal(), None)
            .await
            .expect("open_shell ok");
        assert_eq!(entity.session_id, sid);
        assert_eq!(entity.terminal.term_type, "xterm-256color");
        assert_eq!(entity.terminal.cols, 80);
        assert_eq!(entity.terminal.rows, 24);
        assert_eq!(entity.status, ShellStatus::Open);
        assert!(entity.id.as_str().starts_with("sess-shell-shell-"));
        let log = client.calls();
        assert_eq!(log.len(), 1);
        let recorded = match &log[0] {
            FakeSshCall::OpenShell {
                session_id,
                term,
                cols,
                rows,
                max_buffer_size,
            } => (
                session_id.clone(),
                term.clone(),
                *cols,
                *rows,
                *max_buffer_size,
            ),
            other => panic!("expected OpenShell, got {other:?}"),
        };
        assert_eq!(recorded.0, sid);
        assert_eq!(recorded.1, "xterm-256color");
        assert_eq!(recorded.2, 80);
        assert_eq!(recorded.3, 24);
        assert_eq!(recorded.4, None);
    }

    #[tokio::test]
    async fn queued_open_shell_ok_binds_supplied_shell_id() {
        let client = FakeSshClient::new();
        let sid = SessionId::new("s-1".to_string());
        client.queue_open_shell_ok(ShellId::new("custom-shell".to_string()));
        let entity = client
            .open_shell(&sid, sample_terminal(), None)
            .await
            .expect("open_shell ok");
        assert_eq!(entity.id.as_str(), "custom-shell");
    }

    #[tokio::test]
    async fn queued_open_shell_error_propagates() {
        let client = FakeSshClient::new();
        let sid = SessionId::new("s-1".to_string());
        client.queue_open_shell_error(DomainError::MaxShellsExceeded { limit: 4 });
        let err = client
            .open_shell(&sid, sample_terminal(), None)
            .await
            .expect_err("queued error must propagate");
        match err {
            DomainError::MaxShellsExceeded { limit } => assert_eq!(limit, 4),
            other => panic!("expected MaxShellsExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_write_shell_returns_byte_count_and_records_payload() {
        let client = FakeSshClient::new();
        let shell_id = ShellId::new("sh-1".to_string());
        let payload = Bytes::from_static(b"ls -la\n");
        let written = client
            .write_shell(&shell_id, payload.clone())
            .await
            .expect("write_shell ok");
        assert_eq!(written, payload.len());
        let log = client.calls();
        assert_eq!(log.len(), 1);
        match &log[0] {
            FakeSshCall::WriteShell {
                shell_id: recorded_id,
                bytes,
            } => {
                assert_eq!(recorded_id, &shell_id);
                assert_eq!(bytes.as_ref(), b"ls -la\n");
            }
            other => panic!("expected WriteShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn queued_write_shell_error_propagates() {
        let client = FakeSshClient::new();
        let shell_id = ShellId::new("sh-1".to_string());
        client.queue_write_shell_error(DomainError::ShellNotFound(shell_id.clone()));
        let err = client
            .write_shell(&shell_id, Bytes::from_static(b"x"))
            .await
            .expect_err("queued error must propagate");
        assert_eq!(err, DomainError::ShellNotFound(shell_id));
    }

    #[tokio::test]
    async fn default_close_shell_succeeds_and_records_call() {
        let client = FakeSshClient::new();
        let shell_id = ShellId::new("sh-1".to_string());
        client.close_shell(&shell_id).await.expect("close_shell ok");
        let log = client.calls();
        assert_eq!(log.len(), 1);
        match &log[0] {
            FakeSshCall::CloseShell {
                shell_id: recorded_id,
            } => assert_eq!(recorded_id, &shell_id),
            other => panic!("expected CloseShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn queued_close_shell_error_propagates() {
        let client = FakeSshClient::new();
        let shell_id = ShellId::new("sh-1".to_string());
        client.queue_close_shell_error(DomainError::ShellNotFound(shell_id.clone()));
        let err = client
            .close_shell(&shell_id)
            .await
            .expect_err("queued error must propagate");
        assert_eq!(err, DomainError::ShellNotFound(shell_id));
    }
}
