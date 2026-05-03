//! Composition root.
//!
//! Wires concrete adapters into use cases. Generic over every port; the
//! production binary in [`prod`] pins concrete types via `type ConcreteX
//! = ...`.
//!
//! Use cases land incrementally in etapas H10-H15. Until then, this module
//! exposes just the binary entry points and the empty [`UseCases`]
//! placeholder.
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

pub mod fixtures;
pub mod prod;

use std::marker::PhantomData;
use std::sync::Arc;

use crate::ports::auth_strategy::AuthStrategyPort;
use crate::ports::clock::ClockPort;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::notifier::NotifierPort;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::sftp_client::SftpClientPort;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_registry::{SubscriberRegistryAsync, SubscriberRegistryPort};
use crate::ports::transfer_repo::TransferRepository;

/// Tuple captured by [`UseCases`] `PhantomData` when `port_forward` is on.
///
/// Extracted as an alias so the type stays under the project's
/// `type-complexity-threshold` clippy gate.
#[cfg(feature = "port_forward")]
type AdapterMarkerWithForward<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg> = (
    Arc<S>,
    Arc<F>,
    Arc<SR>,
    Arc<CR>,
    Arc<ShR>,
    Arc<TR>,
    Arc<FR>,
    Arc<N>,
    Arc<AS>,
    Arc<OS>,
    Arc<SubR>,
    Arc<C>,
    Arc<Cfg>,
    Arc<Idg>,
);

/// Tuple captured by [`UseCases`] `PhantomData` when `port_forward` is off.
#[cfg(not(feature = "port_forward"))]
type AdapterMarkerNoForward<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg> = (
    Arc<S>,
    Arc<F>,
    Arc<SR>,
    Arc<CR>,
    Arc<ShR>,
    Arc<TR>,
    Arc<N>,
    Arc<AS>,
    Arc<OS>,
    Arc<SubR>,
    Arc<C>,
    Arc<Cfg>,
    Arc<Idg>,
);

/// Generic container for every use case the server exposes (with
/// `port_forward` enabled).
///
/// Each type parameter constrains an adapter type. Concrete instantiation
/// happens in [`prod`] (production) and [`fixtures`] (tests).
///
/// Empty in H2 — populated in H10-H15.
#[cfg(feature = "port_forward")]
#[derive(Clone, Debug)]
pub struct UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>
where
    S: SshClientPort + Send + Sync,
    F: SftpClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    N: NotifierPort + Send + Sync,
    AS: AuthStrategyPort + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
{
    // Use case fields land in H10-H15. Phantom keeps generics live for now.
    #[allow(
        clippy::type_complexity,
        reason = "phantom marker carries every adapter generic; H10-H15 replaces it with real use case fields"
    )]
    _marker: PhantomData<
        AdapterMarkerWithForward<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>,
    >,
}

/// Generic container for every use case the server exposes (with
/// `port_forward` disabled — the `FR` parameter is omitted).
#[cfg(not(feature = "port_forward"))]
#[derive(Clone, Debug)]
pub struct UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>
where
    S: SshClientPort + Send + Sync,
    F: SftpClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    N: NotifierPort + Send + Sync,
    AS: AuthStrategyPort + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
{
    #[allow(
        clippy::type_complexity,
        reason = "phantom marker carries every adapter generic; H10-H15 replaces it with real use case fields"
    )]
    _marker:
        PhantomData<AdapterMarkerNoForward<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>,
}
