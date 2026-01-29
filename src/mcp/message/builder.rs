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

/// Builder for interactive shell open messages.
///
/// # Example
///
/// ```ignore
/// let message = ShellOpenMessageBuilder::new("shell-123", "sess-456", "xterm", 80, 24)
///     .with_agent_id(Some("my-agent"))
///     .build();
/// ```
pub struct ShellOpenMessageBuilder {
    shell_id: String,
    session_id: String,
    agent_id: Option<String>,
    term: String,
    cols: u32,
    rows: u32,
}

impl ShellOpenMessageBuilder {
    /// Create a new shell open message builder with required fields.
    pub fn new(
        shell_id: impl Into<String>,
        session_id: impl Into<String>,
        term: impl Into<String>,
        cols: u32,
        rows: u32,
    ) -> Self {
        Self {
            shell_id: shell_id.into(),
            session_id: session_id.into(),
            agent_id: None,
            term: term.into(),
            cols,
            rows,
        }
    }

    /// Set the agent ID for the message.
    pub fn with_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.agent_id = agent_id.map(Into::into);
        self
    }

    /// Build the message string.
    pub fn build(&self) -> String {
        let mut lines = vec!["INTERACTIVE SHELL OPENED. REMEMBER THESE IDENTIFIERS:".to_string()];

        if let Some(ref aid) = self.agent_id {
            lines.push(format!("• agent_id: '{}'", aid));
        }
        lines.push(format!("• shell_id: '{}'", self.shell_id));
        lines.push(format!("• session_id: '{}'", self.session_id));
        lines.push(format!(
            "• term: {} ({}x{})",
            self.term, self.cols, self.rows
        ));

        lines.push(String::new()); // empty line
        lines.push(format!(
            "Use ssh_shell_write with shell_id '{}' to send input.",
            self.shell_id
        ));
        lines.push(format!(
            "Use ssh_shell_read with shell_id '{}' to read output.",
            self.shell_id
        ));
        lines.push(format!(
            "Use ssh_shell_close with shell_id '{}' to close the shell.",
            self.shell_id
        ));

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

        #[test]
        fn test_with_agent_id_none_explicit() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_agent_id(None::<String>)
                .build();

            assert!(!message.contains("agent_id"));
            assert!(!message.contains("ssh_disconnect_agent"));
        }

        #[test]
        fn test_with_name_none_explicit() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_name(None::<String>)
                .build();

            assert!(!message.contains("name:"));
        }

        #[test]
        fn test_all_options_combined() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_agent_id(Some("my-agent"))
                .with_name(Some("production-db"))
                .with_retry_attempts(3)
                .with_persistent(true)
                .reused(false)
                .build();

            assert!(message.contains("SSH CONNECTION ESTABLISHED"));
            assert!(message.contains("agent_id: 'my-agent'"));
            assert!(message.contains("session_id: 'sess-123'"));
            assert!(message.contains("name: 'production-db'"));
            assert!(message.contains("host: user@host:22"));
            assert!(message.contains("retry_attempts: 3"));
            assert!(message.contains("persistent: true"));
        }

        #[test]
        fn test_reused_with_all_options() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_agent_id(Some("my-agent"))
                .with_name(Some("staging"))
                .with_retry_attempts(1)
                .with_persistent(true)
                .reused(true)
                .build();

            assert!(message.contains("SESSION REUSED"));
            assert!(!message.contains("SSH CONNECTION ESTABLISHED"));
            assert!(message.contains("agent_id"));
            assert!(message.contains("name"));
        }

        #[test]
        fn test_from_string_types() {
            let session_id = String::from("sess-456");
            let username = String::from("admin");
            let host = String::from("server:2222");

            let message = ConnectMessageBuilder::new(session_id, username, host).build();

            assert!(message.contains("session_id: 'sess-456'"));
            assert!(message.contains("host: admin@server:2222"));
        }

        #[test]
        fn test_builder_order_independence() {
            // Order of method calls shouldn't affect output content
            let msg1 = ConnectMessageBuilder::new("s1", "u", "h:22")
                .with_agent_id(Some("a"))
                .with_persistent(true)
                .build();

            let msg2 = ConnectMessageBuilder::new("s1", "u", "h:22")
                .with_persistent(true)
                .with_agent_id(Some("a"))
                .build();

            // Both should have the same content
            assert!(msg1.contains("agent_id: 'a'"));
            assert!(msg2.contains("agent_id: 'a'"));
            assert!(msg1.contains("persistent: true"));
            assert!(msg2.contains("persistent: true"));
        }

        #[test]
        fn test_message_format_structure() {
            let message = ConnectMessageBuilder::new("sess-123", "user", "host:22")
                .with_agent_id(Some("my-agent"))
                .build();

            // Verify message has proper structure with newlines
            let lines: Vec<&str> = message.lines().collect();
            assert!(lines.len() >= 4); // Header, identifiers, empty line, instructions
            assert!(lines[0].contains("ESTABLISHED"));
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

        #[test]
        fn test_with_agent_id_none_explicit() {
            let message = ExecuteMessageBuilder::new("cmd-123", "sess-456", "ls")
                .with_agent_id(None::<String>)
                .build();

            assert!(!message.contains("agent_id"));
        }

        #[test]
        fn test_from_string_types() {
            let cmd_id = String::from("cmd-789");
            let session_id = String::from("sess-012");
            let command = String::from("echo hello");

            let message = ExecuteMessageBuilder::new(cmd_id, session_id, command).build();

            assert!(message.contains("command_id: 'cmd-789'"));
            assert!(message.contains("session_id: 'sess-012'"));
            assert!(message.contains("command: 'echo hello'"));
        }

        #[test]
        fn test_command_with_special_characters() {
            let message =
                ExecuteMessageBuilder::new("cmd-1", "sess-1", "echo 'hello world' && ls -la")
                    .build();

            assert!(message.contains("echo 'hello world' && ls -la"));
        }

        #[test]
        fn test_command_at_truncation_boundary() {
            // Exactly 50 characters
            let cmd = "a".repeat(50);
            let message = ExecuteMessageBuilder::new("cmd-1", "sess-1", &cmd).build();

            assert!(message.contains(&cmd));
            assert!(!message.contains("..."));
        }

        #[test]
        fn test_command_just_over_truncation() {
            // 51 characters
            let cmd = "a".repeat(51);
            let message = ExecuteMessageBuilder::new("cmd-1", "sess-1", &cmd).build();

            assert!(message.contains("..."));
        }

        #[test]
        fn test_message_includes_usage_instructions() {
            let message = ExecuteMessageBuilder::new("cmd-123", "sess-456", "ls").build();

            // Check that helpful instructions are included
            assert!(message.contains("ssh_get_command_output"));
            assert!(message.contains("ssh_cancel_command"));
            assert!(message.contains("command_id 'cmd-123'"));
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

        #[test]
        fn test_from_string_type() {
            let agent_id = String::from("my-agent-123");
            let message = AgentDisconnectMessageBuilder::new(agent_id)
                .with_sessions_disconnected(1)
                .build();

            assert!(message.contains("agent_id: 'my-agent-123'"));
        }

        #[test]
        fn test_default_values() {
            let message = AgentDisconnectMessageBuilder::new("agent-1").build();

            // Default should be 0 for both
            assert!(message.contains("sessions_disconnected: 0"));
            assert!(message.contains("commands_cancelled: 0"));
            assert!(message.contains("No sessions found"));
        }

        #[test]
        fn test_sessions_but_no_commands() {
            let message = AgentDisconnectMessageBuilder::new("agent-1")
                .with_sessions_disconnected(2)
                .with_commands_cancelled(0)
                .build();

            assert!(message.contains("sessions_disconnected: 2"));
            assert!(message.contains("commands_cancelled: 0"));
            assert!(message.contains("have been terminated"));
        }

        #[test]
        fn test_large_numbers() {
            let message = AgentDisconnectMessageBuilder::new("agent-1")
                .with_sessions_disconnected(100)
                .with_commands_cancelled(500)
                .build();

            assert!(message.contains("sessions_disconnected: 100"));
            assert!(message.contains("commands_cancelled: 500"));
        }

        #[test]
        fn test_builder_order_independence() {
            let msg1 = AgentDisconnectMessageBuilder::new("a")
                .with_sessions_disconnected(1)
                .with_commands_cancelled(2)
                .build();

            let msg2 = AgentDisconnectMessageBuilder::new("a")
                .with_commands_cancelled(2)
                .with_sessions_disconnected(1)
                .build();

            assert!(msg1.contains("sessions_disconnected: 1"));
            assert!(msg2.contains("sessions_disconnected: 1"));
            assert!(msg1.contains("commands_cancelled: 2"));
            assert!(msg2.contains("commands_cancelled: 2"));
        }
    }

    mod shell_open_message_builder {
        use super::*;

        #[test]
        fn test_basic_message() {
            let message =
                ShellOpenMessageBuilder::new("shell-123", "sess-456", "xterm", 80, 24).build();

            assert!(message.contains("INTERACTIVE SHELL OPENED"));
            assert!(message.contains("shell_id: 'shell-123'"));
            assert!(message.contains("session_id: 'sess-456'"));
            assert!(message.contains("term: xterm (80x24)"));
            assert!(message.contains("ssh_shell_write"));
            assert!(message.contains("ssh_shell_read"));
            assert!(message.contains("ssh_shell_close"));
        }

        #[test]
        fn test_with_agent_id() {
            let message = ShellOpenMessageBuilder::new("shell-123", "sess-456", "xterm", 80, 24)
                .with_agent_id(Some("my-agent"))
                .build();

            assert!(message.contains("agent_id: 'my-agent'"));
        }

        #[test]
        fn test_without_agent_id() {
            let message = ShellOpenMessageBuilder::new("shell-123", "sess-456", "xterm", 80, 24)
                .with_agent_id(None::<String>)
                .build();

            assert!(!message.contains("agent_id"));
        }

        #[test]
        fn test_vt100_terminal() {
            let message =
                ShellOpenMessageBuilder::new("shell-1", "sess-1", "vt100", 80, 24).build();

            assert!(message.contains("term: vt100 (80x24)"));
        }

        #[test]
        fn test_custom_dimensions() {
            let message =
                ShellOpenMessageBuilder::new("shell-1", "sess-1", "xterm", 132, 43).build();

            assert!(message.contains("term: xterm (132x43)"));
        }

        #[test]
        fn test_from_string_types() {
            let shell_id = String::from("shell-789");
            let session_id = String::from("sess-012");
            let term = String::from("ansi");

            let message = ShellOpenMessageBuilder::new(shell_id, session_id, term, 80, 24).build();

            assert!(message.contains("shell_id: 'shell-789'"));
            assert!(message.contains("session_id: 'sess-012'"));
            assert!(message.contains("term: ansi"));
        }

        #[test]
        fn test_message_contains_proper_instructions() {
            let message =
                ShellOpenMessageBuilder::new("shell-abc", "sess-def", "xterm", 80, 24).build();

            assert!(message.contains("ssh_shell_write"));
            assert!(message.contains("ssh_shell_read"));
            assert!(message.contains("ssh_shell_close"));
            assert!(message.contains("shell_id 'shell-abc'"));
        }

        #[test]
        fn test_message_format_structure() {
            let message = ShellOpenMessageBuilder::new("shell-1", "sess-1", "xterm", 80, 24)
                .with_agent_id(Some("agent-1"))
                .build();

            let lines: Vec<&str> = message.lines().collect();
            assert!(lines.len() >= 6); // Header, identifiers, empty line, instructions
            assert!(lines[0].contains("INTERACTIVE SHELL OPENED"));
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

        #[test]
        fn test_empty_command() {
            assert_eq!(truncate_command("", 50), "");
        }

        #[test]
        fn test_very_small_max_len() {
            let cmd = "hello world";
            let result = truncate_command(cmd, 5);
            assert_eq!(result, "he...");
            assert_eq!(result.len(), 5);
        }

        #[test]
        fn test_max_len_three() {
            // Edge case: max_len equals length of "..."
            let cmd = "hello";
            let result = truncate_command(cmd, 3);
            assert_eq!(result, "...");
        }

        #[test]
        fn test_max_len_less_than_three() {
            // Edge case: max_len < 3 (less than "..." length)
            let cmd = "hello";
            let result = truncate_command(cmd, 2);
            // saturating_sub prevents underflow
            assert_eq!(result, "...");
        }

        #[test]
        fn test_max_len_zero() {
            let cmd = "hello";
            let result = truncate_command(cmd, 0);
            // With saturating_sub(3) on 0, we get 0, so empty string + "..."
            assert_eq!(result, "...");
        }

        #[test]
        fn test_unicode_command() {
            // Unicode characters may be multi-byte
            let cmd = "echo \u{1F600}"; // emoji
            let result = truncate_command(cmd, 50);
            assert_eq!(result, cmd);
        }

        #[test]
        fn test_one_over_limit() {
            let cmd = "a".repeat(51);
            let result = truncate_command(&cmd, 50);
            assert!(result.ends_with("..."));
            assert_eq!(result.len(), 50);
        }

        #[test]
        fn test_newlines_in_command() {
            let cmd = "echo 'line1\nline2'";
            let result = truncate_command(cmd, 50);
            assert_eq!(result, cmd);
        }

        #[test]
        fn test_tabs_in_command() {
            let cmd = "echo 'col1\tcol2\tcol3'";
            let result = truncate_command(cmd, 50);
            assert_eq!(result, cmd);
        }
    }

    mod edge_cases {
        use super::*;

        #[test]
        fn test_connect_message_special_chars_in_host() {
            let message =
                ConnectMessageBuilder::new("sess-1", "user", "host-name.example.com:2222").build();

            assert!(message.contains("host: user@host-name.example.com:2222"));
        }

        #[test]
        fn test_connect_message_ipv6_host() {
            let message = ConnectMessageBuilder::new("sess-1", "user", "[::1]:22").build();

            assert!(message.contains("host: user@[::1]:22"));
        }

        #[test]
        fn test_execute_message_command_with_quotes() {
            let message =
                ExecuteMessageBuilder::new("cmd-1", "sess-1", "echo \"hello 'world'\"").build();

            assert!(message.contains("echo \"hello 'world'\""));
        }

        #[test]
        fn test_execute_message_empty_command() {
            let message = ExecuteMessageBuilder::new("cmd-1", "sess-1", "").build();

            assert!(message.contains("command: ''"));
        }

        #[test]
        fn test_agent_disconnect_unicode_agent_id() {
            let message = AgentDisconnectMessageBuilder::new("代理-123")
                .with_sessions_disconnected(1)
                .build();

            assert!(message.contains("agent_id: '代理-123'"));
        }

        #[test]
        fn test_connect_message_empty_values() {
            let message = ConnectMessageBuilder::new("", "", "").build();

            assert!(message.contains("session_id: ''"));
            assert!(message.contains("host: @"));
        }

        #[test]
        fn test_execute_message_multiline_command() {
            let multi_cmd = "#!/bin/bash\necho 'hello'\nexit 0";
            let message = ExecuteMessageBuilder::new("cmd-1", "sess-1", multi_cmd).build();

            // Command should be truncated if too long
            assert!(message.contains("command:"));
        }

        #[test]
        fn test_all_builders_produce_non_empty_output() {
            let connect_msg = ConnectMessageBuilder::new("s", "u", "h").build();
            let execute_msg = ExecuteMessageBuilder::new("c", "s", "cmd").build();
            let agent_msg = AgentDisconnectMessageBuilder::new("a").build();

            assert!(!connect_msg.is_empty());
            assert!(!execute_msg.is_empty());
            assert!(!agent_msg.is_empty());
        }

        #[test]
        fn test_connect_message_contains_proper_instructions() {
            let message = ConnectMessageBuilder::new("sess-abc", "user", "host:22").build();

            // Verify it contains helpful instructions
            assert!(message.contains("ssh_execute"));
            assert!(message.contains("sess-abc")); // Session ID mentioned in instructions
        }

        #[test]
        fn test_execute_message_contains_proper_instructions() {
            let message = ExecuteMessageBuilder::new("cmd-xyz", "sess-abc", "ls").build();

            // Verify it contains helpful instructions
            assert!(message.contains("ssh_get_command_output"));
            assert!(message.contains("ssh_cancel_command"));
            assert!(message.contains("cmd-xyz")); // Command ID mentioned in instructions
        }

        #[test]
        fn test_very_long_session_id() {
            let long_id = "s".repeat(500);
            let message = ConnectMessageBuilder::new(&long_id, "user", "host:22").build();

            // Should handle long IDs without truncation
            assert!(message.contains(&long_id));
        }

        #[test]
        fn test_very_long_agent_id() {
            let long_id = "a".repeat(500);
            let message = ConnectMessageBuilder::new("sess-1", "user", "host:22")
                .with_agent_id(Some(&long_id))
                .build();

            assert!(message.contains(&long_id));
        }
    }
}
