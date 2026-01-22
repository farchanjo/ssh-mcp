//! Password-based SSH authentication.

use async_trait::async_trait;
use russh::client;

use crate::mcp::session::SshClientHandler;

use super::traits::AuthStrategy;

/// Password authentication strategy.
///
/// Uses username/password credentials to authenticate with the SSH server.
#[allow(dead_code)]
pub struct PasswordAuth {
    password: String,
}

#[allow(dead_code)]
impl PasswordAuth {
    /// Create a new password authentication strategy.
    pub fn new(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
        }
    }
}

#[async_trait]
impl AuthStrategy for PasswordAuth {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        let result = handle
            .authenticate_password(username, &self.password)
            .await
            .map_err(|e| format!("Password authentication failed: {}", e))?;

        Ok(result.success())
    }

    fn name(&self) -> &'static str {
        "password"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_password_auth_name() {
        let auth = PasswordAuth::new("secret");
        assert_eq!(auth.name(), "password");
    }

    #[test]
    fn test_password_auth_creation() {
        let auth = PasswordAuth::new("my-password");
        assert_eq!(auth.password, "my-password");
    }

    #[test]
    fn test_password_auth_from_string() {
        let auth = PasswordAuth::new(String::from("secret"));
        assert_eq!(auth.password, "secret");
    }
}
