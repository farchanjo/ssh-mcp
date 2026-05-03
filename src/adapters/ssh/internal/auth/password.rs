//! Password-based SSH authentication.

use russh::client;

use crate::adapters::ssh::internal::session::SshClientHandler;

use super::traits::AuthStrategy;

/// Password authentication strategy.
///
/// Uses username/password credentials to authenticate with the SSH
/// server.
pub(crate) struct PasswordAuth {
    password: String,
}

impl PasswordAuth {
    /// Create a new password authentication strategy.
    pub(crate) fn new(password: impl Into<String>) -> Self {
        Self {
            password: password.into(),
        }
    }
}

impl AuthStrategy for PasswordAuth {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        let result = handle
            .authenticate_password(username, &self.password)
            .await
            .map_err(|e| format!("Password authentication failed: {e}"))?;

        Ok(result.success())
    }

    fn name(&self) -> &'static str {
        "password"
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthStrategy, PasswordAuth};

    #[test]
    fn name_is_password() {
        let auth = PasswordAuth::new("secret");
        assert_eq!(auth.name(), "password");
    }

    #[test]
    fn stores_password_verbatim() {
        let auth = PasswordAuth::new("my-password");
        assert_eq!(auth.password, "my-password");
    }

    #[test]
    fn accepts_owned_string() {
        let auth = PasswordAuth::new(String::from("secret"));
        assert_eq!(auth.password, "secret");
    }

    #[test]
    fn empty_password_is_allowed() {
        let auth = PasswordAuth::new("");
        assert_eq!(auth.password, "");
        assert_eq!(auth.name(), "password");
    }

    #[test]
    fn unicode_password_round_trips() {
        let auth = PasswordAuth::new("p@$$w0rd_with_unïcødé");
        assert_eq!(auth.password, "p@$$w0rd_with_unïcødé");
    }

    #[test]
    fn special_characters_round_trip() {
        let auth = PasswordAuth::new("!@#$%^&*()_+-=[]{}|;':\",./<>?`~");
        assert_eq!(auth.password, "!@#$%^&*()_+-=[]{}|;':\",./<>?`~");
    }

    #[test]
    fn whitespace_password_round_trips() {
        let auth = PasswordAuth::new("password with spaces");
        assert_eq!(auth.password, "password with spaces");
    }

    #[test]
    fn newline_in_password_round_trips() {
        let auth = PasswordAuth::new("line1\nline2");
        assert_eq!(auth.password, "line1\nline2");
    }

    #[test]
    fn very_long_password_round_trips() {
        let long_password = "a".repeat(10_000);
        let auth = PasswordAuth::new(&long_password);
        assert_eq!(auth.password.len(), 10_000);
    }

    #[test]
    fn tab_characters_round_trip() {
        let auth = PasswordAuth::new("pass\twith\ttabs");
        assert_eq!(auth.password, "pass\twith\ttabs");
    }

    #[test]
    fn carriage_return_round_trips() {
        let auth = PasswordAuth::new("pass\r\nwith\r\ncrlf");
        assert_eq!(auth.password, "pass\r\nwith\r\ncrlf");
    }

    #[test]
    fn null_character_round_trips() {
        let auth = PasswordAuth::new("pass\0word");
        assert_eq!(auth.password, "pass\0word");
    }

    #[test]
    fn auth_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PasswordAuth>();
    }

    #[test]
    fn name_is_static_str() {
        let auth = PasswordAuth::new("secret");
        let n: &'static str = auth.name();
        assert_eq!(n, "password");
    }

    #[test]
    fn distinct_passwords_create_distinct_instances() {
        let a = PasswordAuth::new("aaa");
        let b = PasswordAuth::new("bbb");
        assert_eq!(a.password, "aaa");
        assert_eq!(b.password, "bbb");
    }

    #[test]
    fn password_with_emoji_preserved_byte_for_byte() {
        let auth = PasswordAuth::new("p4ss🔑w0rd");
        assert_eq!(auth.password, "p4ss🔑w0rd");
    }
}
