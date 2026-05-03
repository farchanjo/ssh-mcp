//! Connection identity primitives shared by the session aggregate and
//! adapters.
//!
//! `Address` is the (host, port) tuple that uniquely identifies an SSH
//! endpoint. `Credentials` is the authentication material the use case
//! requests; the actual handshake is delegated to the
//! [`crate::ports::auth_strategy::AuthStrategyPort`] chain. `AgentSocketPath`
//! is a tokio-free newtype over `PathBuf` so the domain stays runtime
//! agnostic.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default SSH port (RFC 4253).
pub const DEFAULT_SSH_PORT: u16 = 22;

/// SSH endpoint address.
///
/// The host is a free-form string accepting IPv4, IPv6 (without brackets),
/// or a DNS name. Validation lives in the adapter that opens the socket;
/// the domain only enforces non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    host: String,
    port: u16,
}

impl Address {
    /// Build an [`Address`] from host and port.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `host` is empty.
    pub fn new(host: String, port: u16) -> Result<Self, AddressError> {
        if host.trim().is_empty() {
            return Err(AddressError::EmptyHost);
        }
        Ok(Self { host, port })
    }

    /// Build an [`Address`] using the default SSH port.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `host` is empty.
    pub fn with_default_port(host: String) -> Result<Self, AddressError> {
        Self::new(host, DEFAULT_SSH_PORT)
    }

    /// Borrow the host portion.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Return the port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Errors raised when constructing an [`Address`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressError {
    /// The host string was empty or whitespace-only.
    EmptyHost,
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHost => f.write_str("address host must not be empty"),
        }
    }
}

impl std::error::Error for AddressError {}

/// Authentication material requested by a use case. Concrete handshakes are
/// performed by [`crate::ports::auth_strategy::AuthStrategyPort`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credentials {
    /// Username + password pair.
    Password {
        /// Login name.
        username: String,
        /// Plaintext password (kept off `Display` representations).
        password: String,
    },
    /// Username + private key, optionally encrypted.
    PrivateKey {
        /// Login name.
        username: String,
        /// PEM-encoded private key material.
        key_pem: String,
        /// Optional passphrase that decrypts the key.
        passphrase: Option<String>,
    },
    /// Username delegated to a running SSH agent at the given socket path.
    Agent {
        /// Login name.
        username: String,
        /// Optional explicit agent socket path. `None` means the adapter
        /// reads `SSH_AUTH_SOCK` itself.
        socket: Option<AgentSocketPath>,
    },
}

impl Credentials {
    /// Borrow the username regardless of the underlying authentication
    /// method.
    #[must_use]
    pub fn username(&self) -> &str {
        match self {
            Self::Password { username, .. }
            | Self::PrivateKey { username, .. }
            | Self::Agent { username, .. } => username,
        }
    }
}

/// Path to a running SSH agent socket (e.g. `SSH_AUTH_SOCK`). The newtype
/// exists so adapters cannot accidentally accept an arbitrary `PathBuf` and
/// the domain stays free of `tokio::net::UnixStream` references.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentSocketPath(PathBuf);

impl AgentSocketPath {
    /// Wrap a raw filesystem path.
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// Borrow the underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        self.0.as_path()
    }

    /// Consume the wrapper and return the owned `PathBuf`.
    #[must_use]
    pub fn into_inner(self) -> PathBuf {
        self.0
    }
}

impl fmt::Display for AgentSocketPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

#[cfg(test)]
mod tests {
    use super::{Address, AddressError, AgentSocketPath, Credentials, DEFAULT_SSH_PORT};
    use std::path::PathBuf;

    #[test]
    fn address_round_trip_preserves_fields() {
        let addr = Address::new("example.com".to_string(), 2222).expect("address");
        assert_eq!(addr.host(), "example.com");
        assert_eq!(addr.port(), 2222);
        assert_eq!(addr.to_string(), "example.com:2222");
    }

    #[test]
    fn address_with_default_port_uses_22() {
        let addr = Address::with_default_port("h".to_string()).expect("address");
        assert_eq!(addr.port(), DEFAULT_SSH_PORT);
    }

    #[test]
    fn address_rejects_empty_host() {
        let err = Address::new(String::new(), 22).expect_err("expected empty host error");
        assert_eq!(err, AddressError::EmptyHost);
    }

    #[test]
    fn address_rejects_whitespace_host() {
        let err = Address::new("   ".to_string(), 22).expect_err("expected empty host error");
        assert_eq!(err, AddressError::EmptyHost);
    }

    #[test]
    fn credentials_username_accessor_covers_all_variants() {
        let pwd = Credentials::Password {
            username: "alice".to_string(),
            password: "x".to_string(),
        };
        let pk = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: "...".to_string(),
            passphrase: None,
        };
        let agent = Credentials::Agent {
            username: "carol".to_string(),
            socket: None,
        };
        assert_eq!(pwd.username(), "alice");
        assert_eq!(pk.username(), "bob");
        assert_eq!(agent.username(), "carol");
    }

    #[test]
    fn agent_socket_path_round_trip() {
        let p = AgentSocketPath::new(PathBuf::from("/tmp/agent.sock"));
        assert_eq!(p.as_path().to_string_lossy(), "/tmp/agent.sock");
        assert_eq!(p.to_string(), "/tmp/agent.sock");
    }
}
