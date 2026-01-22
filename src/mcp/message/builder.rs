//! Builder patterns for constructing MCP response messages.
//!
//! These builders follow the fluent API pattern to construct human-readable
//! messages that help LLMs remember important identifiers.

/// Builder for SSH connection success messages.
///
/// # Example
///
/// ```ignore
/// let message = ConnectMessageBuilder::new("session-123", "user", "host:22")
///     .with_agent_id(Some("my-agent"))
///     .with_name(Some("production-db"))
///     .with_retry_attempts(2)
///     .with_persistent(true)
///     .reused(false)
///     .build();
/// ```
pub struct ConnectMessageBuilder {
    session_id: String,
    username: String,
    host: String,
    agent_id: Option<String>,
    name: Option<String>,
    retry_attempts: u32,
    persistent: bool,
    reused: bool,
}

impl ConnectMessageBuilder {
    /// Create a new connect message builder with required fields.
    pub fn new(
        session_id: impl Into<String>,
        username: impl Into<String>,
        host: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            username: username.into(),
            host: host.into(),
            agent_id: None,
            name: None,
            retry_attempts: 0,
            persistent: false,
            reused: false,
        }
    }

    /// Set the agent ID for the message.
    pub fn with_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.agent_id = agent_id.map(Into::into);
        self
    }

    /// Set the session name for the message.
    pub fn with_name(mut self, name: Option<impl Into<String>>) -> Self {
        self.name = name.map(Into::into);
        self
    }

    /// Set the number of retry attempts.
    pub fn with_retry_attempts(mut self, attempts: u32) -> Self {
        self.retry_attempts = attempts;
        self
    }

    /// Set whether the session is persistent.
    pub fn with_persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Set whether this is a reused session.
    pub fn reused(mut self, reused: bool) -> Self {
        self.reused = reused;
        self
    }

    /// Build the message string.
    pub fn build(&self) -> String {
        let header = if self.reused {
            "SESSION REUSED"
        } else {
            "SSH CONNECTION ESTABLISHED"
        };

        let mut lines = vec![format!("{}. REMEMBER THESE IDENTIFIERS:", header)];

        if let Some(ref aid) = self.agent_id {
            lines.push(format!("• agent_id: '{}'", aid));
        }
        lines.push(format!("• session_id: '{}'", self.session_id));
        if let Some(ref n) = self.name {
            lines.push(format!("• name: '{}'", n));
        }
        lines.push(format!("• host: {}@{}", self.username, self.host));
        if self.retry_attempts > 0 {
            lines.push(format!("• retry_attempts: {}", self.retry_attempts));
        }
        if self.persistent {
            lines.push("• persistent: true".to_string());
        }

        lines.push(String::new()); // empty line
        lines.push(format!(
            "Use ssh_execute with session_id '{}' to run commands.",
            self.session_id
        ));
        if let Some(ref aid) = self.agent_id {
            lines.push(format!(
                "Use ssh_disconnect_agent with agent_id '{}' to disconnect all sessions for this agent.",
                aid
            ));
        }

        lines.join("\n")
    }
}

/// Builder for command execution start messages.
///
/// # Example
///
/// ```ignore
/// let message = ExecuteMessageBuilder::new("cmd-123", "session-456", "ls -la")
///     .with_agent_id(Some("my-agent"))
///     .build();
/// ```
pub struct ExecuteMessageBuilder {
    command_id: String,
    session_id: String,
    command: String,
    agent_id: Option<String>,
}

impl ExecuteMessageBuilder {
    /// Create a new execute message builder with required fields.
    pub fn new(
        command_id: impl Into<String>,
        session_id: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            command_id: command_id.into(),
            session_id: session_id.into(),
            command: command.into(),
            agent_id: None,
        }
    }

    /// Set the agent ID for the message.
    pub fn with_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.agent_id = agent_id.map(Into::into);
        self
    }

    /// Build the message string.
    pub fn build(&self) -> String {
        let mut lines = vec![
            "COMMAND STARTED. REMEMBER THESE IDENTIFIERS:".to_string(),
            format!("• command_id: '{}'", self.command_id),
            format!("• session_id: '{}'", self.session_id),
        ];

        if let Some(ref aid) = self.agent_id {
            lines.push(format!("• agent_id: '{}'", aid));
        }

        // Truncate command if too long
        let cmd_display = truncate_command(&self.command, 50);
        lines.push(format!("• command: '{}'", cmd_display));

        lines.push(String::new()); // empty line
        lines.push(format!(
            "Use ssh_get_command_output with command_id '{}' to poll for results.",
            self.command_id
        ));
        lines.push(format!(
            "Use ssh_cancel_command with command_id '{}' to cancel.",
            self.command_id
        ));

        lines.join("\n")
    }
}

/// Builder for agent disconnect messages.
///
/// # Example
///
/// ```ignore
/// let message = AgentDisconnectMessageBuilder::new("my-agent")
///     .with_sessions_disconnected(3)
///     .with_commands_cancelled(5)
///     .build();
/// ```
pub struct AgentDisconnectMessageBuilder {
    agent_id: String,
    sessions_disconnected: usize,
    commands_cancelled: usize,
}

impl AgentDisconnectMessageBuilder {
    /// Create a new agent disconnect message builder.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            sessions_disconnected: 0,
            commands_cancelled: 0,
        }
    }

    /// Set the number of sessions disconnected.
    pub fn with_sessions_disconnected(mut self, count: usize) -> Self {
        self.sessions_disconnected = count;
        self
    }

    /// Set the number of commands cancelled.
    pub fn with_commands_cancelled(mut self, count: usize) -> Self {
        self.commands_cancelled = count;
        self
    }

    /// Build the message string.
    pub fn build(&self) -> String {
        let mut lines = vec![
            "AGENT CLEANUP COMPLETE. SUMMARY:".to_string(),
            format!("• agent_id: '{}'", self.agent_id),
            format!("• sessions_disconnected: {}", self.sessions_disconnected),
            format!("• commands_cancelled: {}", self.commands_cancelled),
            String::new(), // empty line
        ];

        if self.sessions_disconnected == 0 {
            lines.push(format!("No sessions found for agent '{}'.", self.agent_id));
        } else {
            lines.push(format!(
                "All sessions and commands for agent '{}' have been terminated.",
                self.agent_id
            ));
        }

        lines.join("\n")
    }
}

/// Truncate a command string for display purposes.
fn truncate_command(command: &str, max_len: usize) -> String {
    if command.len() > max_len {
        format!("{}...", &command[..max_len.saturating_sub(3)])
    } else {
        command.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod connect_message_builder {
        use super::*;

        #[test]
        fn test_basic_message() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22").build();

            assert!(message.contains("SSH CONNECTION ESTABLISHED"));
            assert!(message.contains("session_id: 'sess-123'"));
            assert!(message.contains("host: user@host:22"));
            assert!(message.contains("ssh_execute"));
        }

        #[test]
        fn test_reused_session() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .reused(true)
                .build();

            assert!(message.contains("SESSION REUSED"));
            assert!(!message.contains("SSH CONNECTION ESTABLISHED"));
        }

        #[test]
        fn test_with_agent_id() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_agent_id(Some("my-agent"))
                .build();

            assert!(message.contains("agent_id: 'my-agent'"));
            assert!(message.contains("ssh_disconnect_agent"));
        }

        #[test]
        fn test_with_name() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_name(Some("production-db"))
                .build();

            assert!(message.contains("name: 'production-db'"));
        }

        #[test]
        fn test_with_retry_attempts() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_retry_attempts(2)
                .build();

            assert!(message.contains("retry_attempts: 2"));
        }

        #[test]
        fn test_zero_retry_attempts_not_shown() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_retry_attempts(0)
                .build();

            assert!(!message.contains("retry_attempts"));
        }

        #[test]
        fn test_with_persistent() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_persistent(true)
                .build();

            assert!(message.contains("persistent: true"));
        }

        #[test]
        fn test_persistent_false_not_shown() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_persistent(false)
                .build();

            assert!(!message.contains("persistent"));
        }
    }

    mod execute_message_builder {
        use super::*;

        #[test]
        fn test_basic_message() {
            let message = ExecuteMessageBuilder::new("cmd-123", "sess-456", "ls -la").build();

            assert!(message.contains("COMMAND STARTED"));
            assert!(message.contains("command_id: 'cmd-123'"));
            assert!(message.contains("session_id: 'sess-456'"));
            assert!(message.contains("command: 'ls -la'"));
            assert!(message.contains("ssh_get_command_output"));
            assert!(message.contains("ssh_cancel_command"));
        }

        #[test]
        fn test_with_agent_id() {
            let message = ExecuteMessageBuilder::new("cmd-123", "sess-456", "ls -la")
                .with_agent_id(Some("my-agent"))
                .build();

            assert!(message.contains("agent_id: 'my-agent'"));
        }

        #[test]
        fn test_long_command_truncated() {
            let long_cmd = "a".repeat(100);
            let message = ExecuteMessageBuilder::new("cmd-123", "sess-456", &long_cmd).build();

            assert!(message.contains("..."));
            assert!(!message.contains(&long_cmd));
        }
    }

    mod agent_disconnect_message_builder {
        use super::*;

        #[test]
        fn test_with_sessions() {
            let message = AgentDisconnectMessageBuilder::new("my-agent")
                .with_sessions_disconnected(3)
                .with_commands_cancelled(5)
                .build();

            assert!(message.contains("AGENT CLEANUP COMPLETE"));
            assert!(message.contains("agent_id: 'my-agent'"));
            assert!(message.contains("sessions_disconnected: 3"));
            assert!(message.contains("commands_cancelled: 5"));
            assert!(message.contains("have been terminated"));
        }

        #[test]
        fn test_no_sessions() {
            let message = AgentDisconnectMessageBuilder::new("my-agent")
                .with_sessions_disconnected(0)
                .with_commands_cancelled(0)
                .build();

            assert!(message.contains("No sessions found"));
        }
    }

    mod truncate_command {
        use super::*;

        #[test]
        fn test_short_command() {
            assert_eq!(truncate_command("ls -la", 50), "ls -la");
        }

        #[test]
        fn test_exact_length() {
            let cmd = "a".repeat(50);
            assert_eq!(truncate_command(&cmd, 50), cmd);
        }

        #[test]
        fn test_long_command() {
            let cmd = "a".repeat(60);
            let result = truncate_command(&cmd, 50);
            assert!(result.ends_with("..."));
            assert!(result.len() <= 50);
        }
    }
}
