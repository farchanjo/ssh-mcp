//! Password-based SSH authentication.

use async_trait::async_trait;
use russh::client;

use crate::mcp::session::SshClientHandler;

use super::traits::AuthStrategy;

/// Password authentication strategy.
///
/// Uses username/password credentials to authenticate with the SSH server.
pub struct PasswordAuth {
    password: String,
}

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

    #[test]
    fn test_password_auth_empty_password() {
        let auth = PasswordAuth::new("");
        assert_eq!(auth.password, "");
        assert_eq!(auth.name(), "password");
    }

    #[test]
    fn test_password_auth_unicode_password() {
        let auth = PasswordAuth::new("p@$$w0rd_with_unïcødé");
        assert_eq!(auth.password, "p@$$w0rd_with_unïcødé");
    }

    #[test]
    fn test_password_auth_special_characters() {
        let auth = PasswordAuth::new("!@#$%^&*()_+-=[]{}|;':\",./<>?`~");
        assert_eq!(auth.password, "!@#$%^&*()_+-=[]{}|;':\",./<>?`~");
    }

    #[test]
    fn test_password_auth_whitespace_password() {
        let auth = PasswordAuth::new("password with spaces");
        assert_eq!(auth.password, "password with spaces");
    }

    #[test]
    fn test_password_auth_newline_in_password() {
        let auth = PasswordAuth::new("line1\nline2");
        assert_eq!(auth.password, "line1\nline2");
    }

    #[test]
    fn test_password_auth_very_long_password() {
        let long_password = "a".repeat(10000);
        let auth = PasswordAuth::new(&long_password);
        assert_eq!(auth.password.len(), 10000);
    }

    #[test]
    fn test_password_auth_tab_characters() {
        let auth = PasswordAuth::new("pass\twith\ttabs");
        assert_eq!(auth.password, "pass\twith\ttabs");
    }

    #[test]
    fn test_password_auth_carriage_return() {
        let auth = PasswordAuth::new("pass\r\nwith\r\ncrlf");
        assert_eq!(auth.password, "pass\r\nwith\r\ncrlf");
    }

    #[test]
    fn test_password_auth_null_character() {
        let auth = PasswordAuth::new("pass\0word");
        assert_eq!(auth.password, "pass\0word");
    }

    #[test]
    fn test_password_auth_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PasswordAuth>();
    }
}
