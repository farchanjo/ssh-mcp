//! Authentication strategies for SSH connections.
//!
//! This module provides a trait-based authentication system that follows
//! the Strategy pattern, allowing new authentication methods to be added
//! without modifying existing code (Open-Closed Principle).
//!
//! # Available Strategies
//!
//! - [`password::PasswordAuth`]: Password-based authentication
//! - [`key::KeyAuth`]: Private key file authentication
//! - [`agent::AgentAuth`]: SSH agent authentication
//!
//! # Example
//!
//! ```ignore
//! use ssh_mcp::mcp::auth::chain::AuthChain;
//!
//! let chain = AuthChain::new()
//!     .with_password("secret")
//!     .with_key("/path/to/key")
//!     .with_agent();
//!
//! let result = chain.authenticate(&mut handle, "username").await?;
//! ```

pub mod agent;
pub mod chain;
pub mod key;
pub mod password;
pub mod traits;
