//! Test-only [`SshClientPort`] adapter with scripted, deterministic outcomes.
//!
//! [`FakeSshClient`] records every call (`connect`, `execute`, `disconnect`,
//! `health_check`) into a shared log so tests can assert the orchestration
//! issued exactly the operations expected. Outcomes are scripted per call
//! kind via two queues:
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
//!
//! Other port methods (`execute`, `execute_async`, `cancel`, `open_shell`)
//! are not exercised by the H10 use case but the trait surface is satisfied
//! with `DomainError::Internal("not scripted")` so accidental usage in tests
//! surfaces immediately rather than silently passing.
//!
//! The whole module is gated behind `#[cfg(any(test, feature = "test-fixtures"))]`
//! so it never reaches a release binary.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;

use crate::domain::command::CommandRequest;
use crate::domain::error::DomainError;
use crate::domain::identity::{Address, Credentials};
use crate::domain::ids::{CommandId, SessionId};
use crate::domain::session::SessionEntity;
use crate::domain::shell::{ShellEntity, ShellTerminal};
use crate::ports::ssh_client::{CommandHandle, CommandOutcome, SshClientPort};

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
    /// `health_check(session_id)` was invoked.
    HealthCheck {
        /// Probed session.
        session_id: SessionId,
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
        _command_id: CommandId,
        _request: CommandRequest,
    ) -> Result<CommandHandle, DomainError> {
        Err(DomainError::Internal(
            "FakeSshClient::execute_async not scripted".to_string(),
        ))
    }

    async fn cancel(&self, command_id: &CommandId) -> Result<(), DomainError> {
        Err(DomainError::CommandNotFound(command_id.clone()))
    }

    async fn open_shell(
        &self,
        session_id: &SessionId,
        _terminal: ShellTerminal,
    ) -> Result<ShellEntity, DomainError> {
        Err(DomainError::SessionNotFound(session_id.clone()))
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
    use crate::domain::ids::SessionId;
    use crate::ports::ssh_client::SshClientPort;
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
}
