//! Russh-internal authentication chain.
//!
//! These strategies own the russh `client::Handle` while the handshake
//! runs and are therefore implementation detail of the
//! [`super::super::russh_adapter::RusshAdapter`]. The
//! [`crate::ports::auth_strategy::AuthStrategyPort`] is a separate
//! credential-routing/validation layer driven by the use cases — see
//! [`crate::adapters::auth`] for the v4 port-side adapters.
//!
//! # Available strategies
//!
//! - [`password::PasswordAuth`]: password-based authentication.
//! - [`key::KeyAuth`]: private key file authentication.
//! - [`agent::AgentAuth`]: SSH agent authentication.
//!
//! # Example
//!
//! ```ignore
//! use ssh_mcp::adapters::ssh::internal::auth::chain::AuthChain;
//!
//! let chain = AuthChain::new()
//!     .with_password("secret")
//!     .with_key("/path/to/key")
//!     .with_agent();
//!
//! let result = chain.authenticate(&mut handle, "username").await?;
//! ```
//!
//! # AFIT migration
//!
//! H17.6 P2 relocated this module from the legacy v3 auth namespace and
//! converted the [`traits::AuthStrategy`] trait to native AFIT (async
//! `fn` in trait). Static dispatch through the
//! [`chain::AuthChain`] enum kept the adapter dyn-free, dropping the
//! direct `async-trait` dependency in the crate.

pub mod agent;
pub mod chain;
pub mod key;
pub mod password;
pub mod traits;
