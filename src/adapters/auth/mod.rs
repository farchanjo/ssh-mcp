//! [`crate::ports::auth_strategy::AuthStrategyPort`] adapters.
//!
//! Each authentication method lives in its own submodule and implements the
//! port via static dispatch (`trait_variant` AFIT). The
//! [`chain::AuthChainAdapter`] composes an ordered chain that asks each
//! strategy in turn, returning on the first
//! [`crate::domain::auth::AuthOutcome::Authenticated`] response, propagating
//! the first hard error, or surfacing
//! [`crate::domain::auth::AuthError::Exhausted`] when every strategy
//! returned `Rejected`.
//!
//! ## Layering vs. v3
//!
//! The v3 chain (`src/mcp/auth/`) takes `&mut russh::client::Handle` and
//! drives the actual handshake. The v4 port has no handle parameter — the
//! handshake belongs to the [`crate::ports::ssh_client::SshClientPort`]
//! adapter (etapa H6) which owns the `russh::client::Handle`. The v4
//! strategies are therefore a *routing/validation* layer:
//! - the matching variant adapter validates the credential payload;
//! - non-matching adapters return `Rejected` so the chain advances.
//!
//! This split lets use cases compose authentication policies (e.g. add an
//! OAuth strategy later) without coupling them to a specific transport.

pub mod agent;
pub mod chain;
pub mod key;
pub mod password;
