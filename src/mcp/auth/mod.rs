//! Authentication strategies for SSH connections.
//!
//! This module provides a trait-based authentication system that follows
//! the Strategy pattern, allowing new authentication methods to be added
//! without modifying existing code (Open-Closed Principle).
//!
//! # Available Strategies
//!
//! - [`PasswordAuth`]: Password-based authentication
//! - [`KeyAuth`]: Private key file authentication
//! - [`AgentAuth`]: SSH agent authentication
//!
//! # Example
//!
//! ```ignore
//! use ssh_mcp::mcp::auth::{AuthChain, PasswordAuth, KeyAuth, AgentAuth};
//!
//! let chain = AuthChain::new()
//!     .with_password("secret")
//!     .with_key("/path/to/key")
//!     .with_agent();
//!
//! let result = chain.authenticate(&mut handle, "username").await?;
//! ```

mod agent;
mod chain;
mod key;
mod password;
mod traits;

pub use agent::AgentAuth;
pub use chain::AuthChain;
pub use key::KeyAuth;
pub use password::PasswordAuth;
pub use traits::AuthStrategy;
