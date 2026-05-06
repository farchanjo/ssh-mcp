//! Composition root.
//!
//! Wires concrete adapters into use cases. Generic over every port; the
//! production binary in [`prod`] pins concrete types via `type ConcreteX
//! = ...`.
//!
//! The [`UseCases`] container is the single point of cohesion the
//! `infra::mcp` tool layer (etapa H16) consumes — every rmcp tool fn is a
//! one-liner that maps the inbound DTO onto the matching `*UseCase::execute`
//! call. Adapter wiring lives in [`prod`] (production) and [`fixtures`]
//! (test harness).
//!
//! ## `port_forward` feature gating
//!
//! The forward repository port (`crate::ports::forward_repo`) is itself
//! gated behind the `port_forward` Cargo feature. Mirroring that here, the
//! `FR` type parameter only exists when `port_forward` is enabled. A
//! `cfg_attr` over a generic parameter is not supported by the language, so
//! the struct is compiled twice (once per feature configuration); both
//! variants share the same name [`UseCases`] so the rest of the crate can
//! reference it unconditionally. Production wiring picks the matching
//! variant in [`prod`].

pub mod embed;
pub mod fixtures;
pub mod id_lister;
pub mod prod;
pub mod status_sinks;

use std::sync::Arc;

use crate::application::cancel_command::CancelCommandUseCase;
use crate::application::close_shell::CloseShellUseCase;
use crate::application::connect_session::ConnectSessionUseCase;
use crate::application::disconnect_agent::DisconnectAgentUseCase;
use crate::application::disconnect_session::DisconnectSessionUseCase;
use crate::application::download_file::DownloadFileUseCase;
use crate::application::execute_command::ExecuteCommandUseCase;
#[cfg(feature = "port_forward")]
use crate::application::forward_port::ForwardPortUseCase;
use crate::application::get_command_output::GetCommandOutputUseCase;
use crate::application::get_transfer_progress::GetTransferProgressUseCase;
use crate::application::list_commands::ListCommandsUseCase;
use crate::application::list_resources::ListResourcesUseCase;
use crate::application::list_sessions::ListSessionsUseCase;
use crate::application::open_shell::OpenShellUseCase;
use crate::application::peer_gc::PeerGcUseCase;
use crate::application::read_resource::ReadResourceUseCase;
use crate::application::read_shell::ReadShellUseCase;
use crate::application::rsync_sync::RsyncSyncUseCase;
use crate::application::send_key::SendKeyUseCase;
use crate::application::subscribe_resource::SubscribeResourceUseCase;
use crate::application::subscription_admin::{
    DaemonStatsUseCase, ListSubsUseCase, PauseSubUseCase, ReplaySubUseCase, ResumeSubUseCase,
    SetFilterUseCase, SubStatsUseCase, SubscribeUseCase, UnsubscribeUseCase,
};
use crate::application::unsubscribe_resource::UnsubscribeResourceUseCase;
use crate::application::upload_file::UploadFileUseCase;
use crate::application::wait_for_pattern::WaitForPatternUseCase;
use crate::application::write_shell::WriteShellUseCase;
use crate::ports::auth_strategy::AuthStrategyPort;
use crate::ports::clock::ClockPort;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::lifecycle_policy::LifecyclePolicyPort;
use crate::ports::notifier::NotifierPort;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::rsync_repo::RsyncRepository;
use crate::ports::rsync_sftp_fs::RsyncSftpFsPort;
use crate::ports::rsync_transport::RsyncTransportPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::sftp_client::SftpClientPort;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_registry::{SubscriberRegistryAsync, SubscriberRegistryPort};
use crate::ports::transfer_repo::TransferRepository;

/// Generic container holding every use case the v4 server exposes (with
/// `port_forward` enabled).
///
/// Each type parameter constrains an adapter type. Concrete instantiation
/// happens in [`prod`] (production) and [`fixtures`] (tests). The
/// container is free of any rmcp / russh / dashmap reference — it only
/// holds `Arc` handles to use cases parameterised by the port traits.
#[cfg(feature = "port_forward")]
#[derive(Debug)]
pub struct UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg, W, Sf, Sfs, Rs>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    FR: ForwardRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
    W: RsyncTransportPort + Send + Sync + 'static,
    Sf: RsyncTransportPort + Send + Sync + 'static,
    Sfs: RsyncSftpFsPort + Send + Sync + 'static,
    Rs: RsyncRepository + Send + Sync + 'static,
{
    /// Connect / reuse an SSH session.
    pub connect: Arc<ConnectSessionUseCase<S, SR, C, Idg, Cfg>>,
    /// Disconnect a single session.
    pub disconnect: Arc<DisconnectSessionUseCase<S, SR, CR, ShR, TR>>,
    /// List sessions (optional `agent_id` filter).
    pub list_sessions: Arc<ListSessionsUseCase<S, SR, C, Cfg>>,
    /// Bulk disconnect every session bound to an agent id.
    pub disconnect_agent: Arc<DisconnectAgentUseCase<S, SR, CR, ShR, TR>>,

    /// Spawn a one-shot async command.
    pub execute: Arc<ExecuteCommandUseCase<S, SR, CR, C, Idg, Cfg, SubR>>,
    /// Read the latest output of an async command.
    pub get_command_output: Arc<GetCommandOutputUseCase<CR, OS>>,
    /// List async commands across one or all sessions.
    pub list_commands: Arc<ListCommandsUseCase<CR, Cfg>>,
    /// Cancel an in-flight async command.
    pub cancel_command: Arc<CancelCommandUseCase<S, CR, OS>>,

    /// Open a fresh PTY shell.
    pub open_shell: Arc<OpenShellUseCase<S, SR, ShR, C, Idg, Cfg>>,
    /// Send raw bytes to an open shell.
    pub write_shell: Arc<WriteShellUseCase<S, ShR, C>>,
    /// Send a named keystroke to an open shell.
    pub send_key: Arc<SendKeyUseCase<S, ShR, C>>,
    /// Read accumulated output from a shell.
    pub read_shell: Arc<ReadShellUseCase<ShR, OS, C, Cfg>>,
    /// Wait for one of N substring patterns in a shell.
    pub wait_for_pattern: Arc<WaitForPatternUseCase<ShR, OS, C, Cfg>>,
    /// Close a shell.
    pub close_shell: Arc<CloseShellUseCase<S, ShR>>,

    /// Upload a local file via SFTP.
    pub upload_file: Arc<UploadFileUseCase<F, SR, TR, C, Idg, Cfg, SubR>>,
    /// Download a remote file via SFTP.
    pub download_file: Arc<DownloadFileUseCase<F, SR, TR, C, Idg, Cfg, SubR>>,
    /// Snapshot SFTP transfer progress.
    pub get_transfer_progress: Arc<GetTransferProgressUseCase<TR>>,

    /// Set up local-to-remote port forwarding (feature-gated).
    pub forward_port: Arc<ForwardPortUseCase<S, SR, FR, C, Idg, Cfg>>,

    /// Enumerate every active resource for `resources/list`.
    pub list_resources: Arc<ListResourcesUseCase<SR, CR, ShR, TR, FR>>,
    /// Render a single resource snapshot for `resources/read`.
    pub read_resource: Arc<ReadResourceUseCase<ShR, CR, TR, SR, FR, OS, SubR>>,
    /// Subscribe a peer to a resource URI.
    pub subscribe_resource: Arc<SubscribeResourceUseCase<ShR, CR, TR, SR, FR, SubR>>,
    /// Unsubscribe a peer from a resource URI.
    pub unsubscribe_resource: Arc<UnsubscribeResourceUseCase<SubR>>,
    /// Periodic peer GC pass invoked by the runtime task.
    pub peer_gc: Arc<PeerGcUseCase<SubR>>,

    /// `AuthStrategyPort` is wired into the SSH adapter at construction
    /// time and is therefore not consumed by any use case directly. It is
    /// kept on the container so the composition root holds a single
    /// `Arc` handle the binary can hand to ad-hoc tooling (diagnostics,
    /// future use cases, etc.) without re-instantiating the chain.
    pub auth: Arc<AS>,

    /// `NotifierPort` is wired into the subscription registry at
    /// construction time. Recorded here for the same reason as
    /// [`Self::auth`].
    pub notifier: Arc<N>,

    /// v5 lifecycle adapter handle. Held as `Arc<dyn>` so the use case
    /// container does not need a generic parameter for the lifecycle
    /// port — the trait is dyn-safe by design.
    pub lifecycle_policy: Arc<dyn LifecyclePolicyPort>,

    // -- v5 Phase 3 subscription-administration use cases ---------
    /// `sub_open` use case (Phase 3 lane admin).
    pub sub_subscribe: Arc<SubscribeUseCase>,
    /// `sub_close` use case (Phase 3 lane admin).
    pub sub_unsubscribe: Arc<UnsubscribeUseCase>,
    /// `sub_pause` use case.
    pub sub_pause: Arc<PauseSubUseCase>,
    /// `sub_resume` use case.
    pub sub_resume: Arc<ResumeSubUseCase>,
    /// `sub_filter` use case.
    pub sub_filter: Arc<SetFilterUseCase>,
    /// `sub_replay` use case.
    pub sub_replay: Arc<ReplaySubUseCase>,
    /// `sub_list` use case.
    pub sub_list: Arc<ListSubsUseCase>,
    /// `sub_stats` use case.
    pub sub_stats: Arc<SubStatsUseCase>,
    /// `sub_stats_all` use case.
    pub daemon_stats: Arc<DaemonStatsUseCase>,

    /// ADR 0011 — rsync hybrid transport use case.
    ///
    /// Generic over the two transport ports (`W` for the wire-compat
    /// client and `Sf` for the SFTP fallback), the [`RsyncSftpFsPort`]
    /// driving the capability probe, the rsync session repo, the
    /// existing SSH client (used for the rsync version probe),
    /// `SessionRepository` / `IdGeneratorPort` / `ConfigPort`.
    #[expect(
        clippy::type_complexity,
        reason = "the eight-generic surface mirrors the hexagonal port wiring; collapsing into a type alias would hide the bound chain that makes the use case Sync."
    )]
    pub rsync_sync: Arc<RsyncSyncUseCase<W, Sf, Sfs, Rs, SR, S, Idg, Cfg>>,
}

/// Generic container for every use case the v4 server exposes (with
/// `port_forward` disabled — the `FR` parameter is omitted).
#[cfg(not(feature = "port_forward"))]
#[derive(Debug)]
pub struct UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg, W, Sf, Sfs, Rs>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
    W: RsyncTransportPort + Send + Sync + 'static,
    Sf: RsyncTransportPort + Send + Sync + 'static,
    Sfs: RsyncSftpFsPort + Send + Sync + 'static,
    Rs: RsyncRepository + Send + Sync + 'static,
{
    /// Connect / reuse an SSH session.
    pub connect: Arc<ConnectSessionUseCase<S, SR, C, Idg, Cfg>>,
    /// Disconnect a single session.
    pub disconnect: Arc<DisconnectSessionUseCase<S, SR, CR, ShR, TR>>,
    /// List sessions (optional `agent_id` filter).
    pub list_sessions: Arc<ListSessionsUseCase<S, SR, C, Cfg>>,
    /// Bulk disconnect every session bound to an agent id.
    pub disconnect_agent: Arc<DisconnectAgentUseCase<S, SR, CR, ShR, TR>>,

    /// Spawn a one-shot async command.
    pub execute: Arc<ExecuteCommandUseCase<S, SR, CR, C, Idg, Cfg, SubR>>,
    /// Read the latest output of an async command.
    pub get_command_output: Arc<GetCommandOutputUseCase<CR, OS>>,
    /// List async commands across one or all sessions.
    pub list_commands: Arc<ListCommandsUseCase<CR, Cfg>>,
    /// Cancel an in-flight async command.
    pub cancel_command: Arc<CancelCommandUseCase<S, CR, OS>>,

    /// Open a fresh PTY shell.
    pub open_shell: Arc<OpenShellUseCase<S, SR, ShR, C, Idg, Cfg>>,
    /// Send raw bytes to an open shell.
    pub write_shell: Arc<WriteShellUseCase<S, ShR, C>>,
    /// Send a named keystroke to an open shell.
    pub send_key: Arc<SendKeyUseCase<S, ShR, C>>,
    /// Read accumulated output from a shell.
    pub read_shell: Arc<ReadShellUseCase<ShR, OS, C, Cfg>>,
    /// Wait for one of N substring patterns in a shell.
    pub wait_for_pattern: Arc<WaitForPatternUseCase<ShR, OS, C, Cfg>>,
    /// Close a shell.
    pub close_shell: Arc<CloseShellUseCase<S, ShR>>,

    /// Upload a local file via SFTP.
    pub upload_file: Arc<UploadFileUseCase<F, SR, TR, C, Idg, Cfg, SubR>>,
    /// Download a remote file via SFTP.
    pub download_file: Arc<DownloadFileUseCase<F, SR, TR, C, Idg, Cfg, SubR>>,
    /// Snapshot SFTP transfer progress.
    pub get_transfer_progress: Arc<GetTransferProgressUseCase<TR>>,

    /// Enumerate every active resource for `resources/list`.
    pub list_resources: Arc<ListResourcesUseCase<SR, CR, ShR, TR>>,
    /// Render a single resource snapshot for `resources/read`.
    pub read_resource: Arc<ReadResourceUseCase<ShR, CR, TR, SR, OS, SubR>>,
    /// Subscribe a peer to a resource URI.
    pub subscribe_resource: Arc<SubscribeResourceUseCase<ShR, CR, TR, SR, SubR>>,
    /// Unsubscribe a peer from a resource URI.
    pub unsubscribe_resource: Arc<UnsubscribeResourceUseCase<SubR>>,
    /// Periodic peer GC pass invoked by the runtime task.
    pub peer_gc: Arc<PeerGcUseCase<SubR>>,

    /// `AuthStrategyPort` is wired into the SSH adapter at construction
    /// time. Kept here so the composition root retains an `Arc` handle.
    pub auth: Arc<AS>,
    /// `NotifierPort` is wired into the subscription registry at
    /// construction time. Kept here for the same reason as [`Self::auth`].
    pub notifier: Arc<N>,
    /// v5 lifecycle adapter handle. Held as `Arc<dyn>` so the use case
    /// container does not need a generic parameter for the lifecycle
    /// port — the trait is dyn-safe by design.
    pub lifecycle_policy: Arc<dyn LifecyclePolicyPort>,

    // -- v5 Phase 3 subscription-administration use cases ---------
    /// `sub_open` use case (Phase 3 lane admin).
    pub sub_subscribe: Arc<SubscribeUseCase>,
    /// `sub_close` use case (Phase 3 lane admin).
    pub sub_unsubscribe: Arc<UnsubscribeUseCase>,
    /// `sub_pause` use case.
    pub sub_pause: Arc<PauseSubUseCase>,
    /// `sub_resume` use case.
    pub sub_resume: Arc<ResumeSubUseCase>,
    /// `sub_filter` use case.
    pub sub_filter: Arc<SetFilterUseCase>,
    /// `sub_replay` use case.
    pub sub_replay: Arc<ReplaySubUseCase>,
    /// `sub_list` use case.
    pub sub_list: Arc<ListSubsUseCase>,
    /// `sub_stats` use case.
    pub sub_stats: Arc<SubStatsUseCase>,
    /// `sub_stats_all` use case.
    pub daemon_stats: Arc<DaemonStatsUseCase>,

    /// ADR 0011 — rsync hybrid transport use case.
    ///
    /// Generic over the two transport ports (`W` for the wire-compat
    /// client and `Sf` for the SFTP fallback), the [`RsyncSftpFsPort`]
    /// driving the capability probe, the rsync session repo, the
    /// existing SSH client (used for the rsync version probe),
    /// `SessionRepository` / `IdGeneratorPort` / `ConfigPort`.
    #[expect(
        clippy::type_complexity,
        reason = "the eight-generic surface mirrors the hexagonal port wiring; collapsing into a type alias would hide the bound chain that makes the use case Sync."
    )]
    pub rsync_sync: Arc<RsyncSyncUseCase<W, Sf, Sfs, Rs, SR, S, Idg, Cfg>>,
}
