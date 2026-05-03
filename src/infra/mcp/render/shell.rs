//! Interactive shell markdown renderers.
//!
//! Mirrors v3 `src/mcp/message/builder.rs` —
//! `ShellOpenBuilder`, `render_shell_write_ok`,
//! `render_shell_send_key_ok`, `ShellReadBuilder`, `ShellWaitForBuilder`,
//! `render_shell_close_ok` — but takes the v4 use case Outcomes as input.

use crate::application::close_shell::CloseShellOutcome;
use crate::application::open_shell::OpenShellOutcome;
use crate::application::read_shell::{ReadShellOutcome, ReadShellStatus};
use crate::application::send_key::SendKeyOutcome;
use crate::application::wait_for_pattern::{WaitForPatternOutcome, WaitForPatternStatus};
use crate::application::write_shell::WriteShellOutcome;
use crate::infra::mcp::helpers::nonce::generate_nonce;
use crate::infra::mcp::helpers::output::{render_output_block, sanitize_value};

/// Default byte cap applied to data blocks when the request did not
/// supply one. Mirrors the v3 `output_default_bytes` config knob (16
/// KiB).
const DEFAULT_OUTPUT_BYTES: usize = 16 * 1024;

/// Render an [`OpenShellOutcome`] as the v3 `SSH_SHELL_OPEN` block.
#[must_use]
pub fn shell_open_render(outcome: OpenShellOutcome) -> String {
    let OpenShellOutcome {
        shell,
        session_id,
        agent_id,
    } = outcome;
    let mut out = String::with_capacity(192);
    out.push_str("SSH_SHELL_OPEN: OK\nSHELL_ID: ");
    out.push_str(shell.id.as_str());
    out.push_str("\nSESSION_ID: ");
    out.push_str(session_id.as_str());
    out.push_str("\nTERM: ");
    out.push_str(&sanitize_value(&shell.terminal.term_type));
    out.push(' ');
    out.push_str(&shell.terminal.cols.to_string());
    out.push('x');
    out.push_str(&shell.terminal.rows.to_string());
    if let Some(agent) = agent_id {
        out.push_str("\nAGENT: ");
        out.push_str(&sanitize_value(agent.as_str()));
    }
    out
}

/// Render a [`WriteShellOutcome`] as the v3 `SSH_SHELL_WRITE` block.
#[must_use]
pub fn shell_write_render(outcome: WriteShellOutcome) -> String {
    let WriteShellOutcome {
        shell_id,
        bytes_written,
        last_activity_at: _,
    } = outcome;
    let mut out = String::with_capacity(80);
    out.push_str("SSH_SHELL_WRITE: OK\nSHELL_ID: ");
    out.push_str(shell_id.as_str());
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&bytes_written.to_string());
    out
}

/// Render a [`SendKeyOutcome`] as the v3 `SSH_SHELL_SEND_KEY` block.
#[must_use]
pub fn shell_send_key_render(outcome: SendKeyOutcome) -> String {
    let SendKeyOutcome {
        shell_id,
        key_label,
        modifier_label,
        repeat,
        bytes_sent,
        sent_at: _,
    } = outcome;
    let mut out = String::with_capacity(128);
    out.push_str("SSH_SHELL_SEND_KEY: OK\nSHELL_ID: ");
    out.push_str(shell_id.as_str());
    out.push_str("\nKEY: ");
    out.push_str(&key_label);
    if let Some(label) = modifier_label {
        out.push_str("\nMODIFIERS: ");
        out.push_str(&label);
    }
    out.push_str("\nREPEAT: ");
    out.push_str(&repeat.to_string());
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&bytes_sent.to_string());
    out
}

/// Render a [`ReadShellOutcome`] as the v3 `SSH_SHELL_READ` block.
#[must_use]
pub fn shell_read_render(outcome: ReadShellOutcome) -> String {
    let ReadShellOutcome {
        shell_id,
        status,
        data,
        last_seq: _,
        bytes_consumed: _,
    } = outcome;
    let nonce = generate_nonce();
    let status_label = match status {
        ReadShellStatus::Open => "OPEN",
        ReadShellStatus::Closed => "CLOSED",
        ReadShellStatus::Timeout => "TIMEOUT",
    };
    let data_block = render_output_block("data", &nonce, &data, DEFAULT_OUTPUT_BYTES, None);
    let mut out = String::with_capacity(64 + data_block.len());
    out.push_str("SSH_SHELL_READ: ");
    out.push_str(status_label);
    out.push_str("\nSHELL_ID: ");
    out.push_str(shell_id.as_str());
    out.push('\n');
    out.push_str(&data_block);
    out
}

/// Render a [`WaitForPatternOutcome`] as the v3
/// `SSH_SHELL_WAIT_FOR` block.
#[must_use]
pub fn shell_wait_for_render(outcome: &WaitForPatternOutcome) -> String {
    let nonce = generate_nonce();
    let (status_label, matched) = match outcome.status {
        WaitForPatternStatus::Matched => ("MATCHED", outcome.matched_pattern.as_deref()),
        WaitForPatternStatus::Timeout => ("TIMEOUT", None),
        WaitForPatternStatus::Closed => ("CLOSED", None),
    };
    let data_block = render_output_block("data", &nonce, &outcome.data, DEFAULT_OUTPUT_BYTES, None);
    let bytes_returned = outcome.data.len().min(DEFAULT_OUTPUT_BYTES);
    let mut out = String::with_capacity(128 + data_block.len());
    out.push_str("SSH_SHELL_WAIT_FOR: ");
    out.push_str(status_label);
    out.push_str("\nSHELL_ID: ");
    out.push_str(outcome.shell_id.as_str());
    if let Some(pattern) = matched {
        out.push_str("\nMATCHED_PATTERN: ");
        out.push_str(&sanitize_value(pattern));
    }
    out.push_str("\nBYTES_RETURNED: ");
    out.push_str(&bytes_returned.to_string());
    out.push('\n');
    out.push_str(&data_block);
    out
}

/// Render a [`CloseShellOutcome`] as the v3 `SSH_SHELL_CLOSE` block.
#[must_use]
pub fn shell_close_render(outcome: &CloseShellOutcome) -> String {
    let mut out = String::with_capacity(64);
    out.push_str("SSH_SHELL_CLOSE: OK\nSHELL_ID: ");
    out.push_str(outcome.shell_id.as_str());
    out
}

#[cfg(test)]
mod tests {
    use super::{
        shell_close_render, shell_read_render, shell_send_key_render, shell_wait_for_render,
        shell_write_render,
    };
    use crate::application::close_shell::CloseShellOutcome;
    use crate::application::read_shell::{ReadShellOutcome, ReadShellStatus};
    use crate::application::send_key::SendKeyOutcome;
    use crate::application::wait_for_pattern::{WaitForPatternOutcome, WaitForPatternStatus};
    use crate::application::write_shell::WriteShellOutcome;
    use crate::domain::ids::ShellId;
    use bytes::Bytes;
    use chrono::Utc;

    #[test]
    fn close_renders_id() {
        let m = shell_close_render(&CloseShellOutcome {
            shell_id: ShellId::new("shell-1".to_string()),
        });
        assert_eq!(m, "SSH_SHELL_CLOSE: OK\nSHELL_ID: shell-1");
    }

    #[test]
    fn write_renders_bytes_sent() {
        let m = shell_write_render(WriteShellOutcome {
            shell_id: ShellId::new("shell-1".to_string()),
            bytes_written: 42,
            last_activity_at: "2026-04-18T10:30:00+00:00".to_string(),
        });
        assert_eq!(m, "SSH_SHELL_WRITE: OK\nSHELL_ID: shell-1\nBYTES_SENT: 42");
    }

    #[test]
    fn send_key_renders_modifiers_when_set() {
        let m = shell_send_key_render(SendKeyOutcome {
            shell_id: ShellId::new("shell-1".to_string()),
            key_label: "arrow_up".to_string(),
            modifier_label: Some("shift+ctrl".to_string()),
            repeat: 3,
            bytes_sent: 18,
            sent_at: Utc::now(),
        });
        assert!(m.contains("KEY: arrow_up"));
        assert!(m.contains("MODIFIERS: shift+ctrl"));
        assert!(m.contains("REPEAT: 3"));
        assert!(m.contains("BYTES_SENT: 18"));
    }

    #[test]
    fn read_renders_open_with_data() {
        let m = shell_read_render(ReadShellOutcome {
            shell_id: ShellId::new("shell-1".to_string()),
            status: ReadShellStatus::Open,
            data: Bytes::from_static(b"$ ls\nfile1"),
            last_seq: 0,
            bytes_consumed: 0,
        });
        assert!(m.starts_with("SSH_SHELL_READ: OPEN\n"));
        assert!(m.contains("SHELL_ID: shell-1"));
        assert!(m.contains("$ ls\nfile1"));
    }

    #[test]
    fn wait_for_matched_includes_pattern() {
        let m = shell_wait_for_render(&WaitForPatternOutcome {
            shell_id: ShellId::new("shell-1".to_string()),
            status: WaitForPatternStatus::Matched,
            matched_pattern: Some("$ ".to_string()),
            data: Bytes::from_static(b"login\nuser@host:~$ "),
            last_seq: 0,
        });
        assert!(m.starts_with("SSH_SHELL_WAIT_FOR: MATCHED\n"));
        assert!(m.contains("MATCHED_PATTERN: $ "));
    }
}
