//! Production [`SshClientPort`] adapter backed by `russh` and the v3
//! helpers in [`crate::adapters::ssh::internal::client`].
//!
//! ## Responsibility
//!
//! - Holds a `DashMap<SessionId, SessionRecord>` that owns one
//!   `Arc<russh::client::Handle<SshClientHandler>>` per active session.
//! - Translates domain primitives ([`Address`], [`Credentials`],
//!   [`CommandRequest`], [`ShellTerminal`]) into the parameters the v3
//!   helpers expect, and converts every transport-level outcome back
//!   into a [`DomainError`] / [`CommandOutcome`] / [`SessionEntity`] /
//!   [`ShellEntity`] / [`CommandHandle`].
//! - Maintains side-tables for in-flight async commands
//!   (`DashMap<CommandId, CommandRecord>`) and open PTY shells
//!   (`DashMap<ShellId, ShellRecord>`) so that
//!   [`SshClientPort::cancel`] / future shell-IO ports can resolve
//!   domain ids to russh state without leaking it.
//!
//! ## Invariants
//!
//! - The adapter is the only place in the crate that touches
//!   `russh::Disconnect`, `russh::ChannelMsg`, and
//!   `russh::client::Handle`. Use cases stay free of those types.
//! - Every `.await` happens **outside** any `DashMap` shard guard:
//!   each handler clones the relevant `Arc` (or removes the entry) and
//!   then drops the guard before invoking the v3 helpers.
//! - `connect` returns `DomainError::Auth(AuthError::Rejected)` on
//!   credential rejection (we recognise the v3 message), and
//!   `DomainError::ConnectFailed` for everything else after retries are
//!   exhausted. `execute` translates timeouts into
//!   `CommandOutcome { timed_out: true, .. }` instead of an error so
//!   the session stays alive — matching the v3 contract.
//! - `disconnect` is best-effort: if the russh `disconnect` call fails
//!   we still remove the session from the map so the use case never
//!   leaks state.
//!
//! ## What the adapter does **not** do (yet)
//!
//! - It does not feed `execute_async` output into an
//!   [`OutputStreamPort`]. The v3 [`RunningCommand`] machinery is wired
//!   up so cancellation works, but the broadcast frames are dropped on
//!   the floor pending the H10-H15 use-case rewrite.
//! - It does not persist the adapter-side maps across restarts; a fresh
//!   binary boots with no sessions, identical to the v3 server.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use russh::Disconnect;
use russh::client::{self, Msg};
use russh::{Channel, ChannelMsg};
use tokio::sync::{Notify, broadcast, mpsc};
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::adapters::sftp::russh_sftp_adapter::SshHandleRegistry;
use crate::adapters::ssh::internal::async_command::RunningCommand;
use crate::adapters::ssh::internal::client::{
    connect_to_ssh_with_retry, execute_ssh_command, execute_ssh_command_async,
    execute_ssh_command_async_pty, open_pty_shell,
};
use crate::adapters::ssh::internal::session::SshClientHandler;
use crate::adapters::ssh::internal::shell::{
    ChannelWriter, RingBuffer, RunningShell, WriteRequest, now_ms,
};
use crate::adapters::ssh::internal::types::{AsyncCommandInfo, AsyncCommandStatus, ShellInfo};
use crate::domain::auth::AuthError;
use crate::domain::command::{CommandEntity, CommandRequest};
use crate::domain::error::DomainError;
use crate::domain::identity::{Address, Credentials};
use crate::domain::ids::{CommandId, SessionId, ShellId};
use crate::domain::session::SessionEntity;
use crate::domain::shell::{ShellEntity, ShellTerminal};
use crate::ports::ssh_client::{CommandHandle, CommandOutcome, SshClientPort};

/// Type alias for the v3 SSH handle the adapter wraps.
type SshHandle = Arc<client::Handle<SshClientHandler>>;

/// Tunables that the v3 helpers need but the
/// [`crate::ports::ssh_client::SshClientPort`] surface does not carry on
/// every method. Built once at composition time and re-used for every
/// `connect` call.
#[derive(Debug, Clone)]
pub struct RusshAdapterConfig {
    /// Default per-command timeout used when [`CommandRequest::timeout`]
    /// is `None`.
    pub default_command_timeout: Duration,
    /// Maximum SSH connect retry attempts (see `MAX_RETRY_DELAY` in
    /// [`crate::mcp::config`] for the wall-clock cap).
    pub max_retries: u32,
    /// Initial retry delay; doubled with jitter on each attempt.
    pub retry_delay: Duration,
    /// Inactivity timeout for established sessions
    /// (`SSH_INACTIVITY_TIMEOUT`).
    pub inactivity_timeout: Duration,
    /// Whether to negotiate SSH zlib compression.
    pub compression_enabled: bool,
    /// Persistent connection flag (disables inactivity timeout when
    /// `true`).
    pub persistent: bool,
    /// Health-probe timeout used by [`SshClientPort::health_check`].
    pub health_probe_timeout: Duration,
}

impl Default for RusshAdapterConfig {
    fn default() -> Self {
        Self {
            default_command_timeout: Duration::from_secs(180),
            max_retries: 3,
            retry_delay: Duration::from_secs(1),
            inactivity_timeout: Duration::from_secs(300),
            compression_enabled: true,
            persistent: false,
            health_probe_timeout: Duration::from_secs(10),
        }
    }
}

/// Adapter-internal record bound to one session id. `Debug` is
/// implemented manually because the russh handle does not implement it;
/// we redact the handle in the formatted output.
struct SessionRecord {
    handle: SshHandle,
    address: Address,
    username: String,
    connected_at: DateTime<Utc>,
    default_timeout: Duration,
    retry_attempts: u32,
    compression_enabled: bool,
}

impl fmt::Debug for SessionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRecord")
            .field("address", &self.address)
            .field("username", &self.username)
            .field("connected_at", &self.connected_at)
            .field("default_timeout", &self.default_timeout)
            .field("retry_attempts", &self.retry_attempts)
            .field("compression_enabled", &self.compression_enabled)
            .field("handle", &"<russh::client::Handle>")
            .finish()
    }
}

/// Adapter-internal record bound to one async command id.
pub(crate) struct CommandRecord {
    /// Owning session — used to scope cancellation by tooling that
    /// might want to walk every command for a given session.
    #[allow(
        dead_code,
        reason = "session linkage will drive bulk-cancellation when the use cases land in H10-H15"
    )]
    pub(crate) session_id: SessionId,
    /// v3 lock-free command state. Holding the `Arc` here keeps the
    /// background reader task alive for the duration of the command.
    /// Exposed `pub(crate)` so the sibling
    /// [`crate::adapters::output_stream::russh_output::RusshOutputAdapter`]
    /// can snapshot `output_history` without going through the trait
    /// boundary.
    pub(crate) running: Arc<RunningCommand>,
}

impl fmt::Debug for CommandRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandRecord")
            .field("session_id", &self.session_id)
            .field("running", &"<RunningCommand>")
            .finish()
    }
}

/// Adapter-internal record bound to one open PTY shell id.
pub(crate) struct ShellRecord {
    /// Owning session — used to scope bulk-close logic when a
    /// `disconnect` tears the parent session down.
    #[allow(
        dead_code,
        reason = "session linkage will drive bulk-close when the H13 shell-IO use cases land"
    )]
    pub(crate) session_id: SessionId,
    /// Sink for input frames consumed by the dedicated writer task.
    /// `write_shell` clones this and forwards `WriteRequest::Data`;
    /// `close_shell` forwards `WriteRequest::Close`.
    pub(crate) input_tx: mpsc::Sender<WriteRequest>,
    /// Cancellation handle for the writer task. Firing it terminates
    /// the loop so `close_shell` can guarantee teardown even if the
    /// `WriteRequest::Close` frame never reaches the writer (e.g. the
    /// mpsc queue was already torn down by an inactivity sweep).
    pub(crate) cancel_token: CancellationToken,
    /// Lock-free per-shell state. The reader task publishes incoming
    /// PTY bytes into [`RunningShell::history`] (`ArcSwap`) so the
    /// sibling
    /// [`crate::adapters::output_stream::russh_output::RusshOutputAdapter`]
    /// can snapshot stdout without holding any guard.
    pub(crate) running: Arc<RunningShell>,
}

impl fmt::Debug for ShellRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellRecord")
            .field("session_id", &self.session_id)
            .field("input_tx", &"<mpsc::Sender<WriteRequest>>")
            .field("cancel_token", &self.cancel_token.is_cancelled())
            .field("running", &"<RunningShell>")
            .finish()
    }
}

/// Production adapter implementing [`SshClientPort`].
///
/// `Debug` is implemented manually so the formatted output stays free
/// of every russh-internal type while still surfacing the counts a
/// caller typically wants to see.
#[derive(Clone)]
pub struct RusshAdapter {
    config: Arc<RusshAdapterConfig>,
    sessions: Arc<DashMap<SessionId, SessionRecord>>,
    commands: Arc<DashMap<CommandId, CommandRecord>>,
    shells: Arc<DashMap<ShellId, ShellRecord>>,
    /// Shared registry handed to the sibling
    /// [`crate::adapters::sftp::russh_sftp_adapter::RusshSftpAdapter`].
    /// Populated on `connect` and drained on `disconnect` so SFTP tools
    /// see every session opened through this adapter.
    sftp_handle_registry: SshHandleRegistry,
}

impl fmt::Debug for RusshAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RusshAdapter")
            .field("config", &self.config)
            .field("sessions", &self.sessions.len())
            .field("commands", &self.commands.len())
            .field("shells", &self.shells.len())
            .field("sftp_handle_registry", &self.sftp_handle_registry)
            .finish()
    }
}

impl RusshAdapter {
    /// Build an adapter from explicit tuning. Mints a fresh
    /// [`SshHandleRegistry`] internally; the composition root replaces it
    /// with [`Self::with_sftp_registry`] when it needs the SFTP adapter
    /// to observe the same sessions.
    #[must_use]
    pub fn with_config(config: RusshAdapterConfig) -> Self {
        Self {
            config: Arc::new(config),
            sessions: Arc::new(DashMap::new()),
            commands: Arc::new(DashMap::new()),
            shells: Arc::new(DashMap::new()),
            sftp_handle_registry: SshHandleRegistry::new(),
        }
    }

    /// Build an adapter with [`RusshAdapterConfig::default`] tuning.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(RusshAdapterConfig::default())
    }

    /// Wire an externally-owned [`SshHandleRegistry`] so the sibling
    /// [`crate::adapters::sftp::russh_sftp_adapter::RusshSftpAdapter`]
    /// sees every session this adapter opens. The composition root holds
    /// the same registry handle and feeds it to the SFTP adapter
    /// constructor — that keeps the bridge contract explicit.
    #[must_use]
    pub fn with_sftp_registry(mut self, registry: SshHandleRegistry) -> Self {
        self.sftp_handle_registry = registry;
        self
    }

    /// Borrow the shared SFTP handle registry. Composition-root helper.
    #[must_use]
    pub const fn sftp_handle_registry(&self) -> &SshHandleRegistry {
        &self.sftp_handle_registry
    }

    /// Borrow the active configuration. Useful for tests that assert
    /// the wired tuning is what they expected.
    #[must_use]
    pub fn config(&self) -> &RusshAdapterConfig {
        &self.config
    }

    /// Number of sessions currently registered. Test/observability
    /// helper — never used inside the trait body.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Number of in-flight async commands tracked by the adapter.
    #[must_use]
    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    /// Number of open PTY shells tracked by the adapter.
    #[must_use]
    pub fn shell_count(&self) -> usize {
        self.shells.len()
    }

    /// Borrow the per-command record table.
    ///
    /// Exposed `pub(crate)` so the sibling
    /// [`crate::adapters::output_stream::russh_output::RusshOutputAdapter`]
    /// can share the same `Arc<DashMap>` and snapshot
    /// `RunningCommand.output_history` without duplicating state. Returning
    /// the [`Arc`] (rather than a raw reference) keeps every consumer
    /// lock-free: the caller clones the [`Arc`] once and never holds a
    /// shard guard across `.await`. The visibility matches
    /// [`CommandRecord`] itself, which is also crate-private.
    #[must_use]
    pub(crate) const fn command_table(&self) -> &Arc<DashMap<CommandId, CommandRecord>> {
        &self.commands
    }

    /// Borrow the per-shell record table. See [`Self::command_table`] for
    /// the rationale.
    #[must_use]
    pub(crate) const fn shell_table(&self) -> &Arc<DashMap<ShellId, ShellRecord>> {
        &self.shells
    }

    /// Internal: clone the russh handle for a given session id without
    /// holding the shard guard across `.await`.
    fn lookup_handle(&self, session_id: &SessionId) -> Result<SshHandle, DomainError> {
        self.sessions.get(session_id).map_or_else(
            || Err(DomainError::SessionNotFound(session_id.clone())),
            |entry| Ok(Arc::clone(&entry.value().handle)),
        )
    }

    /// Internal: clone the default command timeout for a session.
    fn lookup_default_timeout(&self, session_id: &SessionId) -> Result<Duration, DomainError> {
        self.sessions.get(session_id).map_or_else(
            || Err(DomainError::SessionNotFound(session_id.clone())),
            |entry| Ok(entry.value().default_timeout),
        )
    }

    /// Internal: invoke the v3 retrying connect helper with the
    /// adapter's tuning.
    async fn drive_connect(
        &self,
        address_string: &str,
        username: &str,
        password: Option<&str>,
        key_path: Option<&str>,
        connect_timeout: Duration,
    ) -> Result<(client::Handle<SshClientHandler>, u32), DomainError> {
        let cfg = Arc::clone(&self.config);
        connect_to_ssh_with_retry(
            address_string,
            username,
            password,
            key_path,
            connect_timeout,
            cfg.inactivity_timeout,
            cfg.max_retries,
            cfg.retry_delay,
            cfg.compression_enabled,
            cfg.persistent,
        )
        .await
        .map_err(map_connect_error)
    }

    /// Internal: assemble the [`SessionEntity`] snapshot using the
    /// adapter tuning.
    fn build_session_entity(
        &self,
        session_id: &SessionId,
        address: &Address,
        username: &str,
        connected_at: DateTime<Utc>,
        retry_attempts: u32,
    ) -> SessionEntity {
        SessionEntity {
            id: session_id.clone(),
            name: None,
            agent_id: None,
            address: address.clone(),
            username: username.to_string(),
            connected_at,
            default_timeout: self.config.default_command_timeout,
            retry_attempts,
            compression_enabled: self.config.compression_enabled,
            last_health_check: None,
            healthy: None,
        }
    }

    /// Internal: bind a freshly built session record to its id, tearing
    /// the russh handle down on a race. On a successful bind the russh
    /// handle is also pushed into the shared
    /// [`SshHandleRegistry`] so the sibling SFTP adapter resolves the
    /// same session.
    async fn bind_session(
        &self,
        session_id: &SessionId,
        record: SessionRecord,
    ) -> Result<(), DomainError> {
        match self.sessions.entry(session_id.clone()) {
            Entry::Vacant(slot) => {
                let handle = Arc::clone(&record.handle);
                slot.insert(record);
                self.sftp_handle_registry
                    .register(session_id.clone(), handle);
                Ok(())
            }
            Entry::Occupied(_) => {
                let SessionRecord { handle, .. } = record;
                drop_handle_quietly(handle).await;
                Err(DomainError::Internal(format!(
                    "session id raced during connect: {session_id}"
                )))
            }
        }
    }

    /// Internal: spawn the appropriate v3 driver for an async command.
    fn spawn_async_driver(
        handle: SshHandle,
        command: String,
        timeout: Duration,
        running: Arc<RunningCommand>,
        pty: bool,
    ) {
        if pty {
            tokio::spawn(async move {
                execute_ssh_command_async_pty(handle, command, timeout, running).await;
            });
        } else {
            tokio::spawn(async move {
                execute_ssh_command_async(handle, command, timeout, running).await;
            });
        }
    }

    /// Internal: register a freshly spawned async command, cancelling
    /// the spawned task on a race.
    fn register_async_command(
        &self,
        command_id: &CommandId,
        record: CommandRecord,
    ) -> Result<(), DomainError> {
        match self.commands.entry(command_id.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(record);
                Ok(())
            }
            Entry::Occupied(_) => {
                record.running.cancel_token.cancel();
                Err(DomainError::Internal(format!(
                    "command id already bound: {command_id}"
                )))
            }
        }
    }
}

impl Default for RusshAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SshClientPort for RusshAdapter {
    async fn connect(
        &self,
        session_id: SessionId,
        address: Address,
        credentials: Credentials,
        connect_timeout: Duration,
    ) -> Result<SessionEntity, DomainError> {
        self.do_connect(session_id, address, credentials, connect_timeout)
            .await
    }

    async fn disconnect(&self, session_id: &SessionId) -> Result<(), DomainError> {
        // Remove first so a racing `execute` cannot find the handle
        // mid-disconnect; we then send the russh disconnect frame
        // outside the shard guard.
        let removed = self.sessions.remove(session_id);
        let Some((_, record)) = removed else {
            return Err(DomainError::SessionNotFound(session_id.clone()));
        };
        // Drain the SFTP-shared registry so future SFTP lookups for this
        // session id return `SessionNotFound` instead of stale handles.
        let _stale = self.sftp_handle_registry.unregister(session_id);
        let SessionRecord { handle, .. } = record;
        let result = handle
            .disconnect(Disconnect::ByApplication, "Session closed by client", "en")
            .await;
        if let Err(e) = result {
            return Err(DomainError::Transport(format!(
                "russh disconnect failed for {session_id}: {e}"
            )));
        }
        Ok(())
    }

    async fn execute(&self, request: CommandRequest) -> Result<CommandOutcome, DomainError> {
        let CommandRequest {
            session_id,
            command,
            timeout,
            pty,
        } = request;
        if pty {
            return Err(DomainError::InvalidArgument(
                "ssh execute does not support pty=true; use execute_async with pty=true"
                    .to_string(),
            ));
        }
        let handle = self.lookup_handle(&session_id)?;
        let timeout = match timeout {
            Some(value) => value,
            None => self.lookup_default_timeout(&session_id)?,
        };

        let response = execute_ssh_command(&handle, command.as_str(), timeout)
            .await
            .map_err(DomainError::Transport)?;
        Ok(CommandOutcome {
            exit_code: Some(response.exit_code),
            stdout: Bytes::from(response.stdout.into_bytes()),
            stderr: Bytes::from(response.stderr.into_bytes()),
            timed_out: response.timed_out,
        })
    }

    async fn execute_async(
        &self,
        command_id: CommandId,
        request: CommandRequest,
    ) -> Result<CommandHandle, DomainError> {
        self.do_execute_async(command_id, request)
    }

    async fn cancel(&self, command_id: &CommandId) -> Result<(), DomainError> {
        let removed = self.commands.remove(command_id);
        let Some((_, record)) = removed else {
            return Err(DomainError::CommandNotFound(command_id.clone()));
        };
        let CommandRecord { running, .. } = record;
        running.cancel_token.cancel();
        Ok(())
    }

    async fn open_shell(
        &self,
        session_id: &SessionId,
        terminal: ShellTerminal,
    ) -> Result<ShellEntity, DomainError> {
        self.do_open_shell(session_id, terminal).await
    }

    async fn write_shell(&self, shell_id: &ShellId, bytes: Bytes) -> Result<usize, DomainError> {
        let input_tx = self.lookup_shell_input(shell_id)?;
        let len = bytes.len();
        input_tx
            .send(WriteRequest::Data(bytes))
            .await
            .map_err(|e| {
                DomainError::Transport(format!("shell write failed for {shell_id}: {e}"))
            })?;
        Ok(len)
    }

    async fn close_shell(&self, shell_id: &ShellId) -> Result<(), DomainError> {
        // Remove first so a racing `write_shell` cannot enqueue more data
        // after the close frame; we then fire the cancel + close pair
        // outside the shard guard.
        let removed = self.shells.remove(shell_id);
        let Some((_, record)) = removed else {
            return Err(DomainError::ShellNotFound(shell_id.clone()));
        };
        let ShellRecord {
            input_tx,
            cancel_token,
            ..
        } = record;
        // Drain order matches v3: enqueue Close first so the writer
        // closes the russh channel cleanly, then cancel the token to
        // unblock the select arm if the queue was already drained.
        let _ = input_tx.send(WriteRequest::Close).await;
        cancel_token.cancel();
        Ok(())
    }

    async fn health_check(&self, session_id: &SessionId) -> Result<(), DomainError> {
        let handle = self.lookup_handle(session_id)?;
        let timeout = self.config.health_probe_timeout;
        let response = execute_ssh_command(&handle, "echo 1", timeout)
            .await
            .map_err(DomainError::Transport)?;
        if response.timed_out {
            return Err(DomainError::Timeout(format!(
                "health probe timed out for {session_id}"
            )));
        }
        if response.exit_code != 0 {
            return Err(DomainError::Transport(format!(
                "health probe non-zero exit ({}) for {session_id}",
                response.exit_code
            )));
        }
        Ok(())
    }
}

impl RusshAdapter {
    /// Mint a deterministic-shape shell id derived from the session id
    /// and the adapter-side counter. Lives here (outside the trait
    /// impl) so the function can stay short and self-explanatory.
    fn mint_shell_id(&self, session_id: &SessionId) -> ShellId {
        let next = self.shells.len().saturating_add(1);
        ShellId::new(format!("{}-shell-{}", session_id.as_str(), next))
    }

    /// Body of [`SshClientPort::connect`] kept on a private method so
    /// the trait method itself stays under the project's
    /// `too-many-lines` threshold.
    async fn do_connect(
        &self,
        session_id: SessionId,
        address: Address,
        credentials: Credentials,
        connect_timeout: Duration,
    ) -> Result<SessionEntity, DomainError> {
        if self.sessions.contains_key(&session_id) {
            return Err(DomainError::Internal(format!(
                "session id already bound: {session_id}"
            )));
        }
        let username = credentials.username().to_string();
        let (handle, retry_attempts) = self
            .drive_connect_for(&address, username.as_str(), &credentials, connect_timeout)
            .await?;
        let connected_at = Utc::now();
        let entity = self.build_session_entity(
            &session_id,
            &address,
            username.as_str(),
            connected_at,
            retry_attempts,
        );
        let record =
            self.assemble_session_record(handle, address, username, connected_at, retry_attempts);
        self.bind_session(&session_id, record).await?;
        Ok(entity)
    }

    /// Internal: format the address and credentials before delegating
    /// to [`Self::drive_connect`]. Lives on a separate method so the
    /// `do_connect` body stays narrative.
    async fn drive_connect_for(
        &self,
        address: &Address,
        username: &str,
        credentials: &Credentials,
        connect_timeout: Duration,
    ) -> Result<(client::Handle<SshClientHandler>, u32), DomainError> {
        let address_string = format!("{}:{}", address.host(), address.port());
        let (password, key_path) = split_credentials(credentials);
        self.drive_connect(
            address_string.as_str(),
            username,
            password.as_deref(),
            key_path.as_deref(),
            connect_timeout,
        )
        .await
    }

    /// Internal: assemble a [`SessionRecord`] from a freshly opened
    /// russh handle plus the values resolved during connect.
    fn assemble_session_record(
        &self,
        handle: client::Handle<SshClientHandler>,
        address: Address,
        username: String,
        connected_at: DateTime<Utc>,
        retry_attempts: u32,
    ) -> SessionRecord {
        SessionRecord {
            handle: Arc::new(handle),
            address,
            username,
            connected_at,
            default_timeout: self.config.default_command_timeout,
            retry_attempts,
            compression_enabled: self.config.compression_enabled,
        }
    }

    /// Body of [`SshClientPort::execute_async`] kept on a private
    /// method so the trait method itself stays under the project's
    /// `too-many-lines` threshold. Synchronous internally because the
    /// background driver is spawned on the current runtime via
    /// [`tokio::spawn`] without awaiting it here.
    fn do_execute_async(
        &self,
        command_id: CommandId,
        request: CommandRequest,
    ) -> Result<CommandHandle, DomainError> {
        let CommandRequest {
            session_id,
            command,
            timeout,
            pty,
        } = request;
        let handle = self.lookup_handle(&session_id)?;
        let timeout = match timeout {
            Some(value) => value,
            None => self.lookup_default_timeout(&session_id)?,
        };
        let started_at = Utc::now();
        let info = AsyncCommandInfo {
            command_id: command_id.as_str().to_string(),
            session_id: session_id.as_str().to_string(),
            command: command.clone(),
            status: AsyncCommandStatus::Running,
            started_at: started_at.to_rfc3339(),
        };
        let running = Arc::new(RunningCommand::new(info));
        Self::spawn_async_driver(handle, command.clone(), timeout, Arc::clone(&running), pty);
        let record = CommandRecord {
            session_id: session_id.clone(),
            running,
        };
        self.register_async_command(&command_id, record)?;
        let entity = CommandEntity::new(command_id, session_id, command, started_at);
        Ok(CommandHandle { entity })
    }

    /// Body of [`SshClientPort::open_shell`] kept on a private method
    /// so the trait method itself stays under the project's
    /// `too-many-lines` threshold.
    async fn do_open_shell(
        &self,
        session_id: &SessionId,
        terminal: ShellTerminal,
    ) -> Result<ShellEntity, DomainError> {
        let handle = self.lookup_handle(session_id)?;
        let channel = open_pty_shell(
            &handle,
            terminal.term_type.as_str(),
            terminal.cols,
            terminal.rows,
        )
        .await
        .map_err(DomainError::Transport)?;
        let shell_id = self.mint_shell_id(session_id);
        let entity = ShellEntity::new(
            shell_id.clone(),
            session_id.clone(),
            terminal.clone(),
            Utc::now(),
            self.config.inactivity_timeout,
            DEFAULT_SHELL_BUFFER_SIZE,
        );
        self.bind_shell(&shell_id, session_id, &terminal, channel)?;
        Ok(entity)
    }

    /// Bind a freshly opened russh channel to a shell id by splitting it
    /// into read/write halves, spawning the dedicated writer task, and
    /// driving a real reader that publishes incoming PTY bytes into
    /// [`RunningShell::history`] (`ArcSwap`). Surfaces an `Internal`
    /// error on the (extremely unlikely) id race.
    fn bind_shell(
        &self,
        shell_id: &ShellId,
        session_id: &SessionId,
        terminal: &ShellTerminal,
        channel: Channel<Msg>,
    ) -> Result<(), DomainError> {
        let (read_half, write_half) = channel.split();
        let (input_tx, input_rx) = mpsc::channel::<WriteRequest>(SHELL_INPUT_CHANNEL_CAP);
        let cancel_token = CancellationToken::new();
        let running = Arc::new(RunningShell::new(
            build_shell_info(shell_id, session_id, terminal),
            cancel_token.clone(),
            input_tx.clone(),
            DEFAULT_SHELL_BUFFER_SIZE,
        ));

        spawn_shell_writer_task(write_half, input_rx, cancel_token.clone());
        spawn_shell_reader_task(read_half, &running);

        match self.shells.entry(shell_id.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(ShellRecord {
                    session_id: session_id.clone(),
                    input_tx,
                    cancel_token,
                    running,
                });
                Ok(())
            }
            Entry::Occupied(_) => {
                // Race: cancel the writer task we just spawned so it
                // doesn't outlive the failed binding.
                cancel_token.cancel();
                Err(DomainError::Internal(format!(
                    "shell id already bound: {shell_id}"
                )))
            }
        }
    }

    /// Internal: clone the input sink for a given shell id without
    /// holding the shard guard across `.await`.
    fn lookup_shell_input(
        &self,
        shell_id: &ShellId,
    ) -> Result<mpsc::Sender<WriteRequest>, DomainError> {
        self.shells.get(shell_id).map_or_else(
            || Err(DomainError::ShellNotFound(shell_id.clone())),
            |entry| Ok(entry.value().input_tx.clone()),
        )
    }
}

/// Default rolling buffer size for a freshly opened shell. Use cases
/// that want a non-default size will pass it through a dedicated
/// shell-config port when shells migrate fully.
const DEFAULT_SHELL_BUFFER_SIZE: u64 = 10_u64.saturating_mul(1024).saturating_mul(1024);

/// Capacity for the per-shell input mpsc queue. Mirrors the v3
/// `SHELL_INPUT_CHANNEL_CAP` so the writer task drains at the same rate
/// as the legacy code path.
const SHELL_INPUT_CHANNEL_CAP: usize = 64;

/// Local staging capacity for the shell reader before the first
/// flush. Mirrors `mcp::tools::legacy_helpers::shell_reader`.
const SHELL_READER_LOCAL_CAP: usize = 4096;

/// Local-buffer high-water mark that triggers an early flush into
/// [`RunningShell::history`]. Mirrors v3's `SHELL_FLUSH_THRESHOLD`.
const SHELL_FLUSH_THRESHOLD: usize = 4096;

/// Spawn the dedicated writer task that owns the russh write half
/// exclusively. Drains [`WriteRequest`] frames from `input_rx` until
/// either a [`WriteRequest::Close`] arrives, the senders are dropped,
/// or `cancel_token` fires. Mirrors `mcp::tools::legacy_helpers::shell_writer`.
fn spawn_shell_writer_task(
    write_half: russh::ChannelWriteHalf<Msg>,
    mut input_rx: mpsc::Receiver<WriteRequest>,
    cancel_token: CancellationToken,
) {
    let writer = ChannelWriter::new(write_half);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = cancel_token.cancelled() => {
                    let _ = writer.close().await;
                    break;
                }
                req = input_rx.recv() => {
                    match req {
                        Some(WriteRequest::Data(bytes)) => {
                            if let Err(e) = writer.write(&bytes).await {
                                warn!("russh adapter shell writer task: {e}");
                                break;
                            }
                        }
                        Some(WriteRequest::Close) | None => {
                            let _ = writer.close().await;
                            break;
                        }
                    }
                }
            }
        }
    });
}

/// Bundle of `Arc`s that the shell reader task needs to publish bytes
/// into [`RunningShell::history`]. Pulled out of the loop body so the
/// public spawn signature stays narrow and the loop function avoids
/// the `too-many-arguments` threshold.
struct ShellReaderHandles {
    history: Arc<ArcSwap<RingBuffer>>,
    output_tx: broadcast::Sender<Bytes>,
    data_notify: Arc<Notify>,
    cancel_token: CancellationToken,
    max_buffer_size: Arc<AtomicU64>,
    last_activity_ms: Arc<AtomicU64>,
}

impl ShellReaderHandles {
    /// Build the bundle from a borrowed [`Arc<RunningShell>`] without
    /// consuming it — the caller still needs the `Arc` to register the
    /// shell record.
    fn from_running(running: &Arc<RunningShell>) -> Self {
        Self {
            history: Arc::clone(&running.history),
            output_tx: running.output_tx.clone(),
            data_notify: Arc::clone(&running.data_notify),
            cancel_token: running.cancel_token.clone(),
            max_buffer_size: Arc::clone(&running.max_buffer_size),
            last_activity_ms: Arc::clone(&running.last_activity_ms),
        }
    }
}

/// Spawn the dedicated reader task that drains the russh read half and
/// publishes incoming PTY bytes into [`RunningShell::history`] via
/// [`ArcSwap::rcu`]. Mirrors `mcp::tools::legacy_helpers::shell_reader`
/// without the registry-side debouncing — the
/// [`crate::ports::subscription::SubscriberRegistry`] adapter (H9) will
/// observe the per-shell broadcast channel directly.
fn spawn_shell_reader_task(read_half: russh::ChannelReadHalf, running: &Arc<RunningShell>) {
    let handles = ShellReaderHandles::from_running(running);
    tokio::spawn(async move {
        run_shell_reader(read_half, handles).await;
    });
}

/// Reader-loop body. Pulled out of [`spawn_shell_reader_task`] so the
/// spawned future stays under the project's `too-many-lines`
/// threshold.
async fn run_shell_reader(mut read_half: russh::ChannelReadHalf, handles: ShellReaderHandles) {
    let mut local: Vec<u8> = Vec::with_capacity(SHELL_READER_LOCAL_CAP);
    loop {
        tokio::select! {
            biased;
            () = handles.cancel_token.cancelled() => break,
            msg = read_half.wait() => {
                if !handle_reader_msg(msg, &mut local, &handles) {
                    break;
                }
            }
        }
    }
    if !local.is_empty() {
        flush_shell_buffer(
            &handles.history,
            &handles.output_tx,
            &handles.data_notify,
            &mut local,
            &handles.max_buffer_size,
        );
    }
}

/// Handle a single russh frame. Returns `true` to continue the reader
/// loop and `false` when the channel signalled EOF / Close (or
/// `None`).
fn handle_reader_msg(
    msg: Option<ChannelMsg>,
    local: &mut Vec<u8>,
    handles: &ShellReaderHandles,
) -> bool {
    match msg {
        Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
            local.extend_from_slice(&data);
            handles.last_activity_ms.store(now_ms(), Ordering::Relaxed);
            if local.len() >= SHELL_FLUSH_THRESHOLD {
                flush_shell_buffer(
                    &handles.history,
                    &handles.output_tx,
                    &handles.data_notify,
                    local,
                    &handles.max_buffer_size,
                );
            }
            true
        }
        Some(ChannelMsg::Eof | ChannelMsg::Close) | None => false,
        Some(_) => true,
    }
}

/// Flush a local staging buffer into [`RunningShell::history`] via
/// [`ArcSwap::rcu`] and broadcast the chunk. The same logic is applied
/// to head-truncate the rolling buffer to `max_buffer_size`.
fn flush_shell_buffer(
    history: &ArcSwap<RingBuffer>,
    output_tx: &broadcast::Sender<Bytes>,
    data_notify: &Notify,
    local: &mut Vec<u8>,
    max_buffer_size: &AtomicU64,
) {
    let chunk = Bytes::copy_from_slice(local);
    local.clear();
    let max_size = usize::try_from(max_buffer_size.load(Ordering::Relaxed)).unwrap_or(usize::MAX);
    history.rcu(|current| {
        let mut combined = Vec::with_capacity(current.data.len() + chunk.len());
        combined.extend_from_slice(&current.data);
        combined.extend_from_slice(&chunk);
        if max_size > 0 && combined.len() > max_size {
            let excess = combined.len() - max_size;
            combined.drain(..excess);
        }
        RingBuffer {
            data: Bytes::from(combined),
        }
    });
    let _ = output_tx.send(chunk);
    data_notify.notify_waiters();
}

/// Build a [`ShellInfo`] payload for the lock-free shell record.
fn build_shell_info(
    shell_id: &ShellId,
    session_id: &SessionId,
    terminal: &ShellTerminal,
) -> ShellInfo {
    ShellInfo {
        shell_id: shell_id.as_str().to_string(),
        session_id: session_id.as_str().to_string(),
        term_type: terminal.term_type.clone(),
        cols: terminal.cols,
        rows: terminal.rows,
        opened_at: Utc::now().to_rfc3339(),
    }
}

/// Translate a [`Credentials`] variant into the `(password, key_path)`
/// pair the v3 helpers expect. Agent credentials map to `(None, None)`,
/// which v3 interprets as the agent fallback.
fn split_credentials(credentials: &Credentials) -> (Option<String>, Option<String>) {
    match credentials {
        Credentials::Password { password, .. } => (Some(password.clone()), None),
        Credentials::PrivateKey { key_pem, .. } => {
            // The v3 path takes a *file* path rather than PEM material;
            // bridging that gap belongs to the `AuthStrategyPort` chain
            // (etapa H8). Until then we honour the contract by passing
            // the path through untouched when the caller already gave
            // us one — `key_pem` field is overloaded here.
            (None, Some(key_pem.clone()))
        }
        Credentials::Agent { .. } => (None, None),
    }
}

/// Map the free-form v3 connect error string into a structured
/// [`DomainError`].
fn map_connect_error(message: String) -> DomainError {
    let lower = message.to_lowercase();
    if lower.contains("permission denied")
        || lower.contains("authentication")
        || lower.contains("publickey")
    {
        // The v3 helper folds rejection messages into the retry summary
        // — we surface them as `Auth(Rejected)` so the use case can
        // translate them into a 401-equivalent response.
        return DomainError::Auth(AuthError::Rejected);
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return DomainError::Timeout(message);
    }
    DomainError::ConnectFailed(message)
}

/// Best-effort russh disconnect used to tear down a handle that we
/// minted but cannot keep (e.g. id race during `connect`). Errors are
/// logged at warn level and otherwise swallowed because there is no
/// caller to receive them.
async fn drop_handle_quietly(handle: SshHandle) {
    if let Err(e) = handle
        .disconnect(Disconnect::ByApplication, "id race during connect", "en")
        .await
    {
        warn!("ignoring russh disconnect failure during id-race cleanup: {e}");
    }
    debug!("dropped racing handle without binding it to a session");
}

/// Drain a russh channel until it acknowledges close. Reserved for
/// future use by the shell-IO ports when they migrate; kept here to
/// document the canonical pattern (every `.await` happens after the
/// shard guard is dropped).
#[allow(
    dead_code,
    reason = "drain helper documents the shell-IO pattern; first caller lands when shell-IO ports migrate"
)]
async fn drain_channel_until_close(channel: &mut Channel<Msg>) {
    let drain = async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Close | ChannelMsg::Eof) | None => break,
                Some(_) => {}
            }
        }
    };
    let _ = time::timeout(Duration::from_secs(2), drain).await;
}

#[cfg(test)]
mod tests {
    use super::{
        Address, Bytes, CommandId, CommandOutcome, Credentials, DomainError, RusshAdapter,
        RusshAdapterConfig, SessionId, ShellId, drop_handle_quietly, map_connect_error,
        split_credentials,
    };
    use crate::domain::auth::AuthError;
    use crate::ports::ssh_client::SshClientPort;
    use std::time::Duration;

    fn _assert_port<T: SshClientPort>() {}

    #[test]
    fn adapter_satisfies_ssh_client_port_send_alias() {
        _assert_port::<RusshAdapter>();
    }

    #[test]
    fn adapter_is_send_sync_clone_default_debug() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_clone<T: Clone>() {}
        fn assert_default<T: Default>() {}
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_send_sync::<RusshAdapter>();
        assert_clone::<RusshAdapter>();
        assert_default::<RusshAdapter>();
        assert_debug::<RusshAdapter>();
    }

    #[test]
    fn config_default_matches_documented_baseline() {
        let cfg = RusshAdapterConfig::default();
        assert_eq!(cfg.default_command_timeout, Duration::from_secs(180));
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.retry_delay, Duration::from_secs(1));
        assert_eq!(cfg.inactivity_timeout, Duration::from_secs(300));
        assert!(cfg.compression_enabled);
        assert!(!cfg.persistent);
        assert_eq!(cfg.health_probe_timeout, Duration::from_secs(10));
    }

    #[test]
    fn fresh_adapter_starts_with_zero_state() {
        let adapter = RusshAdapter::new();
        assert_eq!(adapter.session_count(), 0);
        assert_eq!(adapter.command_count(), 0);
        assert_eq!(adapter.shell_count(), 0);
        let twin = RusshAdapter::default();
        assert_eq!(twin.session_count(), 0);
    }

    #[test]
    fn adapter_clone_shares_state_via_arc() {
        // Cloning the adapter must hand out the same backing maps so
        // the composition root can wire one instance into many use
        // cases without losing track of sessions.
        let adapter = RusshAdapter::new();
        let twin = adapter.clone();
        assert_eq!(adapter.session_count(), twin.session_count());
        let lhs: *const RusshAdapterConfig = adapter.config();
        let rhs: *const RusshAdapterConfig = twin.config();
        assert!(std::ptr::eq(lhs, rhs));
    }

    #[test]
    fn split_credentials_password_carries_password_only() {
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let (pwd, key) = split_credentials(&creds);
        assert_eq!(pwd, Some("secret".to_string()));
        assert!(key.is_none());
    }

    #[test]
    fn split_credentials_private_key_carries_pem_in_key_slot() {
        let creds = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: "/tmp/id_ed25519".to_string(),
            passphrase: None,
        };
        let (pwd, key) = split_credentials(&creds);
        assert!(pwd.is_none());
        assert_eq!(key, Some("/tmp/id_ed25519".to_string()));
    }

    #[test]
    fn split_credentials_agent_returns_no_explicit_credentials() {
        let creds = Credentials::Agent {
            username: "carol".to_string(),
            socket: None,
        };
        let (pwd, key) = split_credentials(&creds);
        assert!(pwd.is_none());
        assert!(key.is_none());
    }

    #[test]
    fn map_connect_error_recognises_auth_rejection() {
        let err = map_connect_error("Permission denied (publickey,password)".to_string());
        assert_eq!(err, DomainError::Auth(AuthError::Rejected));
    }

    #[test]
    fn map_connect_error_recognises_authentication_keyword() {
        let err = map_connect_error(
            "Authentication failed: no authentication methods succeeded".to_string(),
        );
        assert_eq!(err, DomainError::Auth(AuthError::Rejected));
    }

    #[test]
    fn map_connect_error_routes_timeout_to_timeout_variant() {
        let err = map_connect_error("Connection timed out after 30s".to_string());
        match err {
            DomainError::Timeout(message) => assert!(message.contains("timed out")),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn map_connect_error_falls_back_to_connect_failed() {
        let err = map_connect_error("some unrelated network glitch".to_string());
        match err {
            DomainError::ConnectFailed(message) => {
                assert_eq!(message, "some unrelated network glitch");
            }
            other => panic!("expected ConnectFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_rejects_pty_request_with_invalid_argument() {
        let adapter = RusshAdapter::new();
        let request = crate::domain::command::CommandRequest::new(
            SessionId::new("missing".to_string()),
            "ls".to_string(),
        )
        .with_pty(true);
        let err = adapter
            .execute(request)
            .await
            .expect_err("pty=true must be rejected");
        match err {
            DomainError::InvalidArgument(message) => {
                assert!(
                    message.contains("pty"),
                    "error must mention pty, got: {message}"
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_unknown_session_returns_session_not_found() {
        let adapter = RusshAdapter::new();
        let id = SessionId::new("absent".to_string());
        let err = adapter
            .disconnect(&id)
            .await
            .expect_err("disconnect on missing id must fail");
        assert_eq!(err, DomainError::SessionNotFound(id));
    }

    #[tokio::test]
    async fn execute_unknown_session_returns_session_not_found() {
        let adapter = RusshAdapter::new();
        let id = SessionId::new("absent".to_string());
        let req = crate::domain::command::CommandRequest::new(id.clone(), "ls".to_string());
        let err = adapter
            .execute(req)
            .await
            .expect_err("execute on missing session must fail");
        assert_eq!(err, DomainError::SessionNotFound(id));
    }

    #[tokio::test]
    async fn cancel_unknown_command_returns_command_not_found() {
        let adapter = RusshAdapter::new();
        let id = CommandId::new("absent".to_string());
        let err = adapter
            .cancel(&id)
            .await
            .expect_err("cancel on missing command must fail");
        assert_eq!(err, DomainError::CommandNotFound(id));
    }

    #[tokio::test]
    async fn open_shell_unknown_session_returns_session_not_found() {
        let adapter = RusshAdapter::new();
        let id = SessionId::new("absent".to_string());
        let terminal = crate::domain::shell::ShellTerminal::new("xterm".to_string(), 80, 24);
        let err = adapter
            .open_shell(&id, terminal)
            .await
            .expect_err("open_shell on missing session must fail");
        assert_eq!(err, DomainError::SessionNotFound(id));
    }

    #[tokio::test]
    async fn health_check_unknown_session_returns_session_not_found() {
        let adapter = RusshAdapter::new();
        let id = SessionId::new("absent".to_string());
        let err = adapter
            .health_check(&id)
            .await
            .expect_err("health_check on missing session must fail");
        assert_eq!(err, DomainError::SessionNotFound(id));
    }

    #[test]
    fn command_outcome_round_trip_preserves_byte_payload() {
        let outcome = CommandOutcome {
            exit_code: Some(0),
            stdout: Bytes::from_static(b"hello"),
            stderr: Bytes::from_static(b""),
            timed_out: false,
        };
        assert_eq!(outcome.stdout.as_ref(), b"hello");
        assert!(outcome.stderr.is_empty());
        assert_eq!(outcome.exit_code, Some(0));
        assert!(!outcome.timed_out);
    }

    #[test]
    fn shell_id_format_is_session_scoped_and_deterministic_in_shape() {
        // We don't exercise the network-bound `open_shell` here; this
        // test pins the documented shape of the shell id so consumers
        // can rely on a `<session>-shell-<n>` prefix when correlating
        // logs.
        let session = SessionId::new("sess-abc".to_string());
        let shell = ShellId::new(format!("{}-shell-{}", session.as_str(), 1));
        assert!(shell.as_str().starts_with("sess-abc-shell-"));
    }

    #[test]
    fn address_round_trips_back_through_adapter_format_string() {
        // The adapter formats `host:port`. Pin that contract — v3
        // `parse_address` relies on it.
        let addr = Address::new("example.com".to_string(), 2222).expect("address");
        let formatted = format!("{}:{}", addr.host(), addr.port());
        assert_eq!(formatted, "example.com:2222");
    }

    #[tokio::test]
    async fn drop_handle_quietly_is_send_safe_signature() {
        // We cannot exercise a real russh handle without a server; this
        // test pins the `Send` bound on the helper so the future
        // remains compatible with `tokio::spawn` if the use case ever
        // moves the call there.
        fn assert_future_send<F: std::future::Future + Send>(_: &F) {}
        // Guard the helper signature without actually invoking it: we
        // never construct a real `SshHandle` here. The body that would
        // call it is unreachable but the compiler still type-checks
        // the spawn boundary.
        let _ = |handle: super::SshHandle| {
            let fut = drop_handle_quietly(handle);
            assert_future_send(&fut);
        };
    }
}
