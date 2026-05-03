//! Async-command markdown renderers.
//!
//! Mirrors v3 `src/mcp/message/builder.rs` —
//! `ExecuteStartedBuilder`, `GetCommandOutputBuilder`,
//! `ListCommandsBuilder`, `CancelCommandCancelledBuilder`,
//! `render_cancel_command_noop` — but takes the v4 use case Outcomes
//! as input.

use serde_json::{Value, json};

use crate::application::cancel_command::CancelCommandOutcome;
use crate::application::execute_command::ExecuteOutcome;
use crate::application::get_command_output::GetCommandOutputResult;
use crate::application::list_commands::ListCommandsOutcome;
use crate::domain::command::{CommandEntity, CommandStatus};
use crate::domain::ids::AgentId;
use crate::infra::mcp::helpers::nonce::generate_nonce;
use crate::infra::mcp::helpers::output::{
    render_output_block, sanitize_value, truncate_utf8_safe_tail,
};

/// Default byte cap applied to output blocks when the request did not
/// supply one. Mirrors the v3 `output_default_bytes` config knob (16
/// KiB).
const DEFAULT_OUTPUT_BYTES: usize = 16 * 1024;

/// Render an [`ExecuteOutcome`] as `SSH_EXECUTE: STARTED`.
#[must_use]
pub fn execute_render(outcome: ExecuteOutcome) -> String {
    let ExecuteOutcome {
        command_id,
        session_id,
        agent_id,
        started_at: _,
    } = outcome;
    let mut out = String::with_capacity(288);
    out.push_str("SSH_EXECUTE: STARTED\nCOMMAND_ID: ");
    out.push_str(command_id.as_str());
    out.push_str("\nSESSION_ID: ");
    out.push_str(session_id.as_str());
    if let Some(agent) = agent_id {
        out.push_str("\nAGENT_ID: ");
        out.push_str(&sanitize_value(agent.as_str()));
    }
    append_subscribe_hint(
        &mut out,
        &format!(
            "subscribe to command://{cmd}/output for realtime output (preferred over polling)",
            cmd = command_id.as_str(),
        ),
    );
    append_next_line(&mut out, &next_hint_for_execute(command_id.as_str()));
    out
}

/// Successor tools after `ssh_execute: STARTED` — fetch output (with
/// long-poll) or cancel the spawned command.
fn next_hint_for_execute(command_id: &str) -> String {
    format!(
        "ssh_get_command_output(command_id={command_id}, wait=true) | \
         ssh_cancel_command(command_id={command_id})"
    )
}

/// Append a single `NEXT: <hint>` advisory line listing concrete tool
/// calls a smaller LLM can chain without consulting the cookbook.
fn append_next_line(out: &mut String, hint: &str) {
    out.push_str("\nNEXT: ");
    out.push_str(hint);
}

/// Append a `HINT:` line steering 27B-class models toward the
/// subscribe-first resource pattern (push notifications) instead of
/// hot-polling tool calls.
fn append_subscribe_hint(out: &mut String, hint: &str) {
    out.push_str("\nHINT: ");
    out.push_str(hint);
}

/// Render a [`GetCommandOutputResult`] as the v3
/// `SSH_GET_COMMAND_OUTPUT` block.
#[must_use]
pub fn get_command_output_render(result: GetCommandOutputResult) -> String {
    let GetCommandOutputResult {
        command_id,
        status,
        stdout,
        stderr,
        exit_code,
        error: _,
        timed_out,
        last_seq: _,
    } = result;
    let nonce = generate_nonce();
    let (status_label, hint, exit) = classify_state(status, exit_code, timed_out);
    let stdout_block = render_output_block("stdout", &nonce, &stdout, DEFAULT_OUTPUT_BYTES, hint);
    let stderr_block = render_output_block("stderr", &nonce, &stderr, DEFAULT_OUTPUT_BYTES, hint);
    let mut out = build_get_command_output_head(
        status_label,
        command_id.as_str(),
        exit,
        &stdout_block,
        &stderr_block,
    );
    if matches!(status, CommandStatus::Running) {
        append_next_line(
            &mut out,
            &next_hint_for_running_command(command_id.as_str()),
        );
    }
    out
}

/// Compose the static head of `SSH_GET_COMMAND_OUTPUT` (status + ids +
/// optional exit + stdout/stderr blocks). Pulled out so the entry point
/// stays under the 30-line cognitive threshold.
fn build_get_command_output_head(
    status_label: &str,
    command_id: &str,
    exit: Option<i32>,
    stdout_block: &str,
    stderr_block: &str,
) -> String {
    let mut out = String::with_capacity(192 + stdout_block.len() + stderr_block.len());
    out.push_str("SSH_GET_COMMAND_OUTPUT: ");
    out.push_str(status_label);
    out.push_str("\nCOMMAND_ID: ");
    out.push_str(command_id);
    if let Some(code) = exit {
        out.push_str("\nEXIT: ");
        out.push_str(&code.to_string());
    }
    out.push('\n');
    out.push_str(stdout_block);
    out.push('\n');
    out.push_str(stderr_block);
    out
}

/// Successor advisory for an in-flight command — subscribe for push
/// updates or long-poll the same tool.
fn next_hint_for_running_command(command_id: &str) -> String {
    format!(
        "resources/subscribe command://{command_id}/output | \
         ssh_get_command_output(command_id={command_id}, wait=true)"
    )
}

const fn classify_state(
    status: CommandStatus,
    exit_code: Option<i32>,
    timed_out: bool,
) -> (&'static str, Option<&'static str>, Option<i32>) {
    match status {
        CommandStatus::Running => ("RUNNING", Some("partial"), None),
        CommandStatus::Completed => {
            if timed_out {
                ("TIMEOUT", Some("partial"), None)
            } else {
                ("COMPLETED", None, exit_code)
            }
        }
        CommandStatus::Cancelled => ("CANCELLED", Some("partial"), None),
        CommandStatus::Failed => ("FAILED", Some("partial"), None),
    }
}

/// Render a [`ListCommandsOutcome`] as the v3 `SSH_LIST_COMMANDS` block.
#[must_use]
pub fn list_commands_render(outcome: ListCommandsOutcome) -> String {
    let ListCommandsOutcome { commands, total } = outcome;
    if commands.is_empty() && total == 0 {
        return String::from("SSH_LIST_COMMANDS: OK\nCOUNT: 0");
    }
    let mut out = String::with_capacity(64 + commands.len() * 128);
    out.push_str("SSH_LIST_COMMANDS: OK\nCOUNT: ");
    out.push_str(&commands.len().to_string());
    if total > commands.len() {
        out.push_str(" (showing ");
        out.push_str(&commands.len().to_string());
        out.push_str(" of ");
        out.push_str(&total.to_string());
        out.push(')');
    }
    for c in &commands {
        out.push_str("\n- ");
        append_command_item(&mut out, c);
    }
    out
}

fn append_command_item(out: &mut String, c: &CommandEntity) {
    out.push_str(c.id.as_str());
    out.push_str(" [");
    out.push_str(status_name_upper(c.status));
    out.push_str("] ");
    out.push_str(c.session_id.as_str());
    out.push_str(": ");
    out.push_str(&sanitize_value(&c.command));
    out.push_str(" (");
    out.push_str(extract_time(&c.started_at.to_rfc3339()));
    out.push(')');
}

const fn status_name_upper(s: CommandStatus) -> &'static str {
    match s {
        CommandStatus::Running => "RUNNING",
        CommandStatus::Completed => "COMPLETED",
        CommandStatus::Cancelled => "CANCELLED",
        CommandStatus::Failed => "FAILED",
    }
}

fn extract_time(ts: &str) -> &str {
    if let Some((_, rest)) = ts.split_once('T') {
        let end = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
        &rest[..end]
    } else {
        ts
    }
}

/// Render a [`CancelCommandOutcome`] as the v3 `SSH_CANCEL_COMMAND`
/// block.
#[must_use]
pub fn cancel_command_render(outcome: CancelCommandOutcome) -> String {
    match outcome {
        CancelCommandOutcome::Cancelled {
            command_id,
            stdout,
            stderr,
        } => render_cancel_cancelled(command_id.as_str(), &stdout, &stderr),
        CancelCommandOutcome::NotRunning { command_id, status } => {
            render_cancel_noop(command_id.as_str(), status)
        }
    }
}

fn render_cancel_cancelled(command_id: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let nonce = generate_nonce();
    let stdout_block = render_output_block(
        "stdout",
        &nonce,
        stdout,
        DEFAULT_OUTPUT_BYTES,
        Some("partial"),
    );
    let stderr_block = render_output_block(
        "stderr",
        &nonce,
        stderr,
        DEFAULT_OUTPUT_BYTES,
        Some("partial"),
    );
    let mut out = String::with_capacity(96 + stdout_block.len() + stderr_block.len());
    out.push_str("SSH_CANCEL_COMMAND: CANCELLED\nCOMMAND_ID: ");
    out.push_str(command_id);
    out.push('\n');
    out.push_str(&stdout_block);
    out.push('\n');
    out.push_str(&stderr_block);
    out
}

fn render_cancel_noop(command_id: &str, status: CommandStatus) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("SSH_CANCEL_COMMAND: NOOP\nCOMMAND_ID: ");
    out.push_str(command_id);
    out.push_str("\nREASON: ");
    out.push_str(status_name_upper(status));
    out
}

// ---------------------------------------------------------------------------
// v4.7 — structured_content payloads (JSON parallel to the Markdown body)
// ---------------------------------------------------------------------------

/// Build the execute-command structured payload mirroring [`execute_render`].
///
/// Status is always `started`; the synchronous completion path is
/// reserved for a future use case and is not exposed by this tool today.
#[must_use]
pub fn execute_structured(outcome: &ExecuteOutcome) -> Value {
    json!({
        "tool":   "ssh_execute",
        "status": "started",
        "session_id": outcome.session_id.as_str(),
        "command_id": outcome.command_id.as_str(),
        "agent_id":   outcome.agent_id.as_ref().map(AgentId::as_str),
        "next": [
            "ssh_get_command_output",
            "ssh_cancel_command",
        ],
    })
}

/// Lower-cased wire status label per [`CommandStatus`]. Mirrors the
/// `running` / `completed` / `cancelled` / `failed` set the markdown
/// renderer uses (with `timeout` collapsed into `completed`).
const fn command_status_lower(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Running => "running",
        CommandStatus::Completed => "completed",
        CommandStatus::Cancelled => "cancelled",
        CommandStatus::Failed => "failed",
    }
}

/// Build the get-command-output structured payload mirroring [`get_command_output_render`].
///
/// Stdout/stderr are tail-truncated using the same UTF-8-safe helper so
/// the structured payload matches the rendered Markdown block
/// byte-for-byte.
#[must_use]
pub fn get_command_output_structured(result: &GetCommandOutputResult) -> Value {
    let (stdout, stdout_info) = truncate_utf8_safe_tail(&result.stdout, DEFAULT_OUTPUT_BYTES);
    let (stderr, stderr_info) = truncate_utf8_safe_tail(&result.stderr, DEFAULT_OUTPUT_BYTES);
    let status_label = if result.timed_out && matches!(result.status, CommandStatus::Completed) {
        "timeout"
    } else {
        command_status_lower(result.status)
    };
    let next = matches!(result.status, CommandStatus::Running).then(|| {
        let id = result.command_id.as_str();
        json!([
            format!("resources/subscribe command://{id}/output"),
            format!("ssh_get_command_output(command_id={id}, wait=true)"),
        ])
    });
    json!({
        "tool":     "ssh_get_command_output",
        "status":   status_label,
        "command_id": result.command_id.as_str(),
        "exit_code": result.exit_code,
        "stdout":   stdout,
        "stderr":   stderr,
        "stdout_truncated": stdout_info.was_truncated(),
        "stderr_truncated": stderr_info.was_truncated(),
        "timed_out": result.timed_out,
        "error":    result.error,
        "next":     next,
    })
}

/// Encode a [`CommandEntity`] as a JSON object suitable for the
/// list-commands structured payload.
fn command_json(c: &CommandEntity) -> Value {
    json!({
        "command_id": c.id.as_str(),
        "session_id": c.session_id.as_str(),
        "command":    c.command,
        "status":     command_status_lower(c.status),
        "started_at": c.started_at.to_rfc3339(),
    })
}

/// Build the list-commands structured payload mirroring
/// [`list_commands_render`].
#[must_use]
pub fn list_commands_structured(outcome: &ListCommandsOutcome) -> Value {
    let commands: Vec<Value> = outcome.commands.iter().map(command_json).collect();
    json!({
        "tool":   "ssh_list_commands",
        "status": "ok",
        "commands": commands,
        "count":   outcome.commands.len(),
        "total":   outcome.total,
    })
}

/// Build the cancel-command structured payload mirroring
/// [`cancel_command_render`].
#[must_use]
pub fn cancel_command_structured(outcome: &CancelCommandOutcome) -> Value {
    match outcome {
        CancelCommandOutcome::Cancelled {
            command_id,
            stdout,
            stderr,
        } => {
            let (stdout_str, stdout_info) = truncate_utf8_safe_tail(stdout, DEFAULT_OUTPUT_BYTES);
            let (stderr_str, stderr_info) = truncate_utf8_safe_tail(stderr, DEFAULT_OUTPUT_BYTES);
            json!({
                "tool":   "ssh_cancel_command",
                "status": "ok",
                "command_id": command_id.as_str(),
                "stdout":   stdout_str,
                "stderr":   stderr_str,
                "stdout_truncated": stdout_info.was_truncated(),
                "stderr_truncated": stderr_info.was_truncated(),
            })
        }
        CancelCommandOutcome::NotRunning { command_id, status } => json!({
            "tool":   "ssh_cancel_command",
            "status": "noop",
            "command_id": command_id.as_str(),
            "reason": command_status_lower(*status),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cancel_command_render, execute_render, get_command_output_render, list_commands_render,
    };
    use crate::application::cancel_command::CancelCommandOutcome;
    use crate::application::execute_command::ExecuteOutcome;
    use crate::application::get_command_output::GetCommandOutputResult;
    use crate::application::list_commands::ListCommandsOutcome;
    use crate::domain::command::CommandStatus;
    use crate::domain::ids::{CommandId, SessionId};
    use bytes::Bytes;

    #[test]
    fn execute_render_emits_started() {
        let m = execute_render(ExecuteOutcome {
            command_id: CommandId::new("cmd-1".to_string()),
            session_id: SessionId::new("sess-1".to_string()),
            agent_id: None,
            started_at: "2026-04-18T10:30:00+00:00".to_string(),
        });
        assert!(m.contains("SSH_EXECUTE: STARTED"));
        assert!(m.contains("COMMAND_ID: cmd-1"));
        assert!(m.contains("SESSION_ID: sess-1"));
    }

    #[test]
    fn list_commands_empty() {
        let outcome = ListCommandsOutcome {
            commands: vec![],
            total: 0,
        };
        assert_eq!(
            list_commands_render(outcome),
            "SSH_LIST_COMMANDS: OK\nCOUNT: 0"
        );
    }

    #[test]
    fn cancel_command_noop_renders_status_label() {
        let m = cancel_command_render(CancelCommandOutcome::NotRunning {
            command_id: CommandId::new("c-1".to_string()),
            status: CommandStatus::Completed,
        });
        assert!(m.contains("SSH_CANCEL_COMMAND: NOOP"));
        assert!(m.contains("REASON: COMPLETED"));
    }

    #[test]
    fn get_command_output_completed_includes_exit() {
        let m = get_command_output_render(GetCommandOutputResult {
            command_id: CommandId::new("c-1".to_string()),
            status: CommandStatus::Completed,
            stdout: Bytes::from_static(b"ok"),
            stderr: Bytes::new(),
            exit_code: Some(0),
            error: None,
            timed_out: false,
            last_seq: 0,
        });
        assert!(m.contains("SSH_GET_COMMAND_OUTPUT: COMPLETED"));
        assert!(m.contains("EXIT: 0"));
    }
}
