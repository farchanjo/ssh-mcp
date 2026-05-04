//! Async-command markdown renderers.
//!
//! Mirrors v3 `src/mcp/message/builder.rs` —
//! `ExecuteStartedBuilder`, `GetCommandOutputBuilder`,
//! `ListCommandsBuilder`, `CancelCommandCancelledBuilder`,
//! `render_cancel_command_noop` — but takes the v4 use case Outcomes
//! as input.

use serde_json::{Value, json};

use crate::adapters::lifecycle::leak_watcher::LeakRiskAlert;
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
use crate::infra::mcp::render::connection::{append_sub_leak_risk_warnings, warnings_value};

/// Default byte cap applied to output blocks when the request did not
/// supply one. Mirrors the v3 `output_default_bytes` config knob (16
/// KiB).
const DEFAULT_OUTPUT_BYTES: usize = 16 * 1024;

/// Render an [`ExecuteOutcome`] as `SSH_EXEC: STARTED`.
#[must_use]
pub fn execute_render(outcome: ExecuteOutcome) -> String {
    let ExecuteOutcome {
        command_id,
        session_id,
        agent_id,
        started_at: _,
    } = outcome;
    let mut out = String::with_capacity(288);
    out.push_str("SSH_EXEC: STARTED\nCOMMAND_ID: ");
    out.push_str(command_id.as_str());
    out.push_str("\nSESSION_ID: ");
    out.push_str(session_id.as_str());
    if let Some(agent) = agent_id {
        out.push_str("\nAGENT_ID: ");
        out.push_str(&sanitize_value(agent.as_str()));
    }
    // v5 Phase 3 — subscribe is RECOMMENDED for commands: long-poll
    // via ssh_exec_output still works, but push removes the
    // poll loop entirely.
    append_subscribe_hint(
        &mut out,
        &format!(
            "RECOMMENDED: sub_open uri=command://{cmd}/output. Falls back gracefully if you skip (use ssh_exec_output wait=true).",
            cmd = command_id.as_str(),
        ),
    );
    append_next_line(&mut out, &next_hint_for_execute(command_id.as_str()));
    out
}

/// Successor tools after `ssh_exec: STARTED`.
///
/// v5 Phase 3 ordering: `sub_open` FIRST (push), then the drive ops,
/// with the long-poll fallback listed last.
fn next_hint_for_execute(command_id: &str) -> String {
    format!(
        "sub_open uri=command://{command_id}/output | \
         ssh_exec_cancel(command_id={command_id}) | \
         ssh_exec_output(command_id={command_id}, wait=true) (poll fallback)"
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
/// `SSH_EXEC_OUTPUT` block.
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

/// Compose the static head of `SSH_EXEC_OUTPUT` (status + ids +
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
    out.push_str("SSH_EXEC_OUTPUT: ");
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
        "sub_open uri=command://{command_id}/output (preferred) | \
         ssh_exec_output(command_id={command_id}, wait=true) (poll fallback)"
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

/// Render a [`ListCommandsOutcome`] as the v3 `SSH_COMMANDS` block.
///
/// Equivalent to [`list_commands_render_with_warnings`] with no
/// [`LeakRiskAlert`] entries. Kept for legacy callers.
#[must_use]
pub fn list_commands_render(outcome: ListCommandsOutcome) -> String {
    list_commands_render_with_warnings(outcome, &[])
}

/// Render a [`ListCommandsOutcome`] and append a `WARN: SUB_LEAK_RISK
/// <uri>` line per supplied [`LeakRiskAlert`].
#[must_use]
pub fn list_commands_render_with_warnings(
    outcome: ListCommandsOutcome,
    alerts: &[LeakRiskAlert],
) -> String {
    let ListCommandsOutcome { commands, total } = outcome;
    if commands.is_empty() && total == 0 && alerts.is_empty() {
        return String::from(
            "SSH_COMMANDS: OK\nCOUNT: 0\nNEXT: ssh_exec(session_id=..., command=...) (then sub_open uri=command://<COMMAND_ID>/output)",
        );
    }
    let mut out = String::with_capacity(64 + commands.len() * 128 + alerts.len() * 96);
    out.push_str("SSH_COMMANDS: OK\nCOUNT: ");
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
    append_sub_leak_risk_warnings(&mut out, alerts);
    append_next_for_running(&mut out, &commands);
    out
}

/// Append a `NEXT:` line steering toward `sub_open` (preferred) for
/// the first running command in a list. Honours v5 narrative closure:
/// discovery flows terminate in a push lane, with poll listed as fallback.
fn append_next_for_running(out: &mut String, commands: &[CommandEntity]) {
    if let Some(running) = commands
        .iter()
        .find(|c| matches!(c.status, CommandStatus::Running))
    {
        let id = running.id.as_str();
        append_next_line(
            out,
            &format!(
                "sub_open uri=command://{id}/output (preferred for running entries) | \
                 ssh_exec_output(command_id={id}, wait=true) (poll fallback) | \
                 ssh_exec_cancel(command_id={id})"
            ),
        );
    }
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

/// Render a [`CancelCommandOutcome`] as the v3 `SSH_EXEC_CANCEL`
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
    out.push_str("SSH_EXEC_CANCEL: CANCELLED\nCOMMAND_ID: ");
    out.push_str(command_id);
    out.push('\n');
    out.push_str(&stdout_block);
    out.push('\n');
    out.push_str(&stderr_block);
    out
}

fn render_cancel_noop(command_id: &str, status: CommandStatus) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("SSH_EXEC_CANCEL: NOOP\nCOMMAND_ID: ");
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
        "tool":   "ssh_exec",
        "status": "started",
        "session_id": outcome.session_id.as_str(),
        "command_id": outcome.command_id.as_str(),
        "agent_id":   outcome.agent_id.as_ref().map(AgentId::as_str),
        "next": [
            "ssh_exec_output",
            "ssh_exec_cancel",
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
            format!("ssh_exec_output(command_id={id}, wait=true)"),
        ])
    });
    json!({
        "tool":     "ssh_exec_output",
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
/// [`list_commands_render`]. Empty warnings — kept for legacy callers.
#[must_use]
pub fn list_commands_structured(outcome: &ListCommandsOutcome) -> Value {
    list_commands_structured_with_warnings(outcome, &[])
}

/// Build the list-commands structured payload mirroring
/// [`list_commands_render_with_warnings`]. Adds a `warnings` array.
#[must_use]
pub fn list_commands_structured_with_warnings(
    outcome: &ListCommandsOutcome,
    alerts: &[LeakRiskAlert],
) -> Value {
    let commands: Vec<Value> = outcome.commands.iter().map(command_json).collect();
    json!({
        "tool":   "ssh_commands",
        "status": "ok",
        "commands": commands,
        "count":   outcome.commands.len(),
        "total":   outcome.total,
        "warnings": warnings_value(alerts),
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
                "tool":   "ssh_exec_cancel",
                "status": "ok",
                "command_id": command_id.as_str(),
                "stdout":   stdout_str,
                "stderr":   stderr_str,
                "stdout_truncated": stdout_info.was_truncated(),
                "stderr_truncated": stderr_info.was_truncated(),
            })
        }
        CancelCommandOutcome::NotRunning { command_id, status } => json!({
            "tool":   "ssh_exec_cancel",
            "status": "noop",
            "command_id": command_id.as_str(),
            "reason": command_status_lower(*status),
        }),
    }
}

// ---------------------------------------------------------------------------
// v4.7-step3 — ssh_run + ssh_exec_batch render helpers
// ---------------------------------------------------------------------------

/// Status string used by both Markdown and structured payloads when
/// rendering [`crate::infra::mcp::results::SshRunResult`] /
/// [`crate::infra::mcp::results::SshExecuteBatchEntry`].
#[must_use]
pub const fn run_status_label(result: &GetCommandOutputResult) -> &'static str {
    if result.timed_out && matches!(result.status, CommandStatus::Completed) {
        "TIMEOUT"
    } else {
        match result.status {
            CommandStatus::Running => "RUNNING",
            CommandStatus::Completed => "COMPLETED",
            CommandStatus::Cancelled => "CANCELLED",
            CommandStatus::Failed => "FAILED",
        }
    }
}

/// Lower-cased twin of [`run_status_label`].
#[must_use]
pub const fn run_status_lower(result: &GetCommandOutputResult) -> &'static str {
    if result.timed_out && matches!(result.status, CommandStatus::Completed) {
        "timeout"
    } else {
        command_status_lower(result.status)
    }
}

/// Render the Markdown body for the `ssh_run` tool.
///
/// The block layout mirrors `ssh_exec_output`'s body so
/// existing parsers keep extracting `EXIT:` / `--- stdout ---` lines
/// unchanged; the session/disconnect lines are unique to `ssh_run`.
#[must_use]
pub fn run_render(result: &GetCommandOutputResult, session_id: &str, disconnected: bool) -> String {
    let nonce = generate_nonce();
    let hint = run_block_hint(result);
    let stdout_block =
        render_output_block("stdout", &nonce, &result.stdout, DEFAULT_OUTPUT_BYTES, hint);
    let stderr_block =
        render_output_block("stderr", &nonce, &result.stderr, DEFAULT_OUTPUT_BYTES, hint);
    let mut out = String::with_capacity(192 + stdout_block.len() + stderr_block.len());
    out.push_str("SSH_RUN: ");
    out.push_str(run_status_label(result));
    out.push_str("\nSESSION_ID: ");
    out.push_str(session_id);
    out.push_str("\nCOMMAND_ID: ");
    out.push_str(result.command_id.as_str());
    if let Some(code) = result.exit_code {
        out.push_str("\nEXIT: ");
        out.push_str(&code.to_string());
    }
    out.push_str("\nDISCONNECTED: ");
    out.push_str(if disconnected { "true" } else { "false" });
    out.push('\n');
    out.push_str(&stdout_block);
    out.push('\n');
    out.push_str(&stderr_block);
    if !disconnected {
        append_next_line(&mut out, &next_hint_for_run_kept_alive(session_id));
    }
    out
}

/// NEXT advisory when `ssh_run` kept the session alive — mirrors the
/// `ssh_connect` post-spawn chain so the LLM knows the `session_id` is
/// reusable and lists the most-likely successor calls.
fn next_hint_for_run_kept_alive(session_id: &str) -> String {
    format!(
        "ssh_exec(session_id={session_id}, command=...) | \
         ssh_shell_open(session_id={session_id}) | \
         ssh_upload(session_id={session_id}, ...) | \
         ssh_disconnect(session_id={session_id})"
    )
}

/// Pick the truncation `(partial)` annotation based on the terminal
/// state of the snapshot. `Completed` / non-timeout runs skip the
/// hint so the block reads "clean"; everything else carries
/// `partial` so the caller knows the buffers may be incomplete.
const fn run_block_hint(result: &GetCommandOutputResult) -> Option<&'static str> {
    match result.status {
        CommandStatus::Completed if !result.timed_out => None,
        CommandStatus::Running
        | CommandStatus::Completed
        | CommandStatus::Cancelled
        | CommandStatus::Failed => Some("partial"),
    }
}

/// Build the structured payload for the `ssh_run` tool. Stdout/stderr
/// truncation matches the Markdown side byte-for-byte.
#[must_use]
pub fn run_structured(
    result: &GetCommandOutputResult,
    session_id: &str,
    disconnected: bool,
) -> Value {
    let (stdout, stdout_info) = truncate_utf8_safe_tail(&result.stdout, DEFAULT_OUTPUT_BYTES);
    let (stderr, stderr_info) = truncate_utf8_safe_tail(&result.stderr, DEFAULT_OUTPUT_BYTES);
    json!({
        "tool":     "ssh_run",
        "status":   run_status_lower(result),
        "session_id": session_id,
        "command_id": result.command_id.as_str(),
        "disconnected": disconnected,
        "exit_code": result.exit_code,
        "stdout":   stdout,
        "stderr":   stderr,
        "stdout_truncated": stdout_info.was_truncated(),
        "stderr_truncated": stderr_info.was_truncated(),
        "timed_out": result.timed_out,
        "error":    result.error,
    })
}

/// Lower-cased status surfaced by [`crate::infra::mcp::results::SshExecuteBatchEntry`].
///
/// Same set as [`run_status_lower`] plus the `"skipped"` stop-on-
/// failure label.
#[must_use]
pub const fn batch_entry_status(result: &GetCommandOutputResult) -> &'static str {
    run_status_lower(result)
}

/// Build the per-command structured entry for `ssh_exec_batch`.
/// Returns the `index`/`command_id`/`stdout`/`stderr` shape consumed
/// by [`crate::infra::mcp::results::SshExecBatchResult`].
#[must_use]
pub fn batch_entry_structured(
    index: usize,
    command_text: &str,
    result: &GetCommandOutputResult,
) -> Value {
    let (stdout, stdout_info) = truncate_utf8_safe_tail(&result.stdout, DEFAULT_OUTPUT_BYTES);
    let (stderr, stderr_info) = truncate_utf8_safe_tail(&result.stderr, DEFAULT_OUTPUT_BYTES);
    json!({
        "index":      index,
        "command":    command_text,
        "status":     batch_entry_status(result),
        "command_id": result.command_id.as_str(),
        "exit_code":  result.exit_code,
        "stdout":     stdout,
        "stderr":     stderr,
        "stdout_truncated": stdout_info.was_truncated(),
        "stderr_truncated": stderr_info.was_truncated(),
        "timed_out":  result.timed_out,
        "error":      result.error,
    })
}

/// Build a `"skipped"` per-command entry surfaced after a stop-on-
/// failure short-circuit. Carries the index + command text so the LLM
/// sees the unexecuted slots without having to diff against the input.
#[must_use]
pub fn batch_skipped_entry(index: usize, command_text: &str) -> Value {
    json!({
        "index":   index,
        "command": command_text,
        "status":  "skipped",
        "stdout":  "",
        "stderr":  "",
        "stdout_truncated": false,
        "stderr_truncated": false,
        "timed_out": false,
    })
}

/// Render the Markdown body for `ssh_exec_batch`. Each executed
/// command emits a per-index header line plus a stdout/stderr block
/// pair; skipped commands surface as `--- skipped #N: <command> ---`.
#[must_use]
pub fn batch_render(
    session_id: &str,
    halted: bool,
    executed: usize,
    total: usize,
    entries: &[BatchEntryView<'_>],
) -> String {
    let label = if halted { "HALTED" } else { "OK" };
    let mut out = String::with_capacity(96 + entries.len() * 192);
    out.push_str("SSH_EXEC_BATCH: ");
    out.push_str(label);
    out.push_str("\nSESSION_ID: ");
    out.push_str(session_id);
    out.push_str("\nEXECUTED: ");
    out.push_str(&executed.to_string());
    out.push_str("\nTOTAL: ");
    out.push_str(&total.to_string());
    for entry in entries {
        out.push('\n');
        match entry {
            BatchEntryView::Executed {
                index,
                command,
                result,
            } => append_batch_executed_block(&mut out, *index, command, result),
            BatchEntryView::Skipped { index, command } => {
                out.push_str("--- skipped #");
                out.push_str(&index.to_string());
                out.push_str(": ");
                out.push_str(&sanitize_value(command));
                out.push_str(" ---");
            }
        }
    }
    append_next_line(&mut out, &next_hint_for_batch(session_id));
    out
}

/// NEXT advisory after `ssh_exec_batch` — steers smaller LLMs back to
/// the push-first single-command path for any further async work.
fn next_hint_for_batch(session_id: &str) -> String {
    format!(
        "ssh_exec(session_id={session_id}, command=...) + sub_open uri=command://<COMMAND_ID>/output (PREFERRED for further async work — push delivery, no poll loop) | \
         ssh_exec_batch(session_id={session_id}, ...) (next sequential batch) | \
         ssh_disconnect(session_id={session_id})"
    )
}

/// Lightweight view consumed by [`batch_render`]. Borrows command text
/// + result so the renderer never owns the underlying buffers.
#[derive(Debug)]
pub enum BatchEntryView<'a> {
    /// One actually executed command — header + stdout/stderr blocks.
    Executed {
        index: usize,
        command: &'a str,
        result: &'a GetCommandOutputResult,
    },
    /// Stop-on-failure short-circuited the loop before this entry.
    Skipped { index: usize, command: &'a str },
}

fn append_batch_executed_block(
    out: &mut String,
    index: usize,
    command: &str,
    result: &GetCommandOutputResult,
) {
    let nonce = generate_nonce();
    let label = run_status_label(result);
    out.push_str("--- command #");
    out.push_str(&index.to_string());
    out.push_str(" [");
    out.push_str(label);
    out.push_str("] ");
    out.push_str(&sanitize_value(command));
    if let Some(code) = result.exit_code {
        out.push_str(" exit=");
        out.push_str(&code.to_string());
    }
    out.push_str(" ---\n");
    out.push_str(&render_output_block(
        "stdout",
        &nonce,
        &result.stdout,
        DEFAULT_OUTPUT_BYTES,
        Some("partial"),
    ));
    out.push('\n');
    out.push_str(&render_output_block(
        "stderr",
        &nonce,
        &result.stderr,
        DEFAULT_OUTPUT_BYTES,
        Some("partial"),
    ));
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
        assert!(m.contains("SSH_EXEC: STARTED"));
        assert!(m.contains("COMMAND_ID: cmd-1"));
        assert!(m.contains("SESSION_ID: sess-1"));
    }

    #[test]
    fn list_commands_empty() {
        let outcome = ListCommandsOutcome {
            commands: vec![],
            total: 0,
        };
        let body = list_commands_render(outcome);
        assert!(body.starts_with("SSH_COMMANDS: OK\nCOUNT: 0"));
        assert!(body.contains("NEXT: ssh_exec("));
        assert!(body.contains("sub_open uri=command://"));
    }

    #[test]
    fn cancel_command_noop_renders_status_label() {
        let m = cancel_command_render(CancelCommandOutcome::NotRunning {
            command_id: CommandId::new("c-1".to_string()),
            status: CommandStatus::Completed,
        });
        assert!(m.contains("SSH_EXEC_CANCEL: NOOP"));
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
        assert!(m.contains("SSH_EXEC_OUTPUT: COMPLETED"));
        assert!(m.contains("EXIT: 0"));
    }

    // ---------- v4.7-step3 ssh_run / ssh_exec_batch render tests ----

    #[test]
    fn run_render_emits_session_command_disconnected_lines() {
        let result = GetCommandOutputResult {
            command_id: CommandId::new("cmd-1".to_string()),
            status: CommandStatus::Completed,
            stdout: Bytes::from_static(b"hi"),
            stderr: Bytes::new(),
            exit_code: Some(0),
            error: None,
            timed_out: false,
            last_seq: 0,
        };
        let body = super::run_render(&result, "sess-1", true);
        assert!(body.starts_with("SSH_RUN: COMPLETED"));
        assert!(body.contains("SESSION_ID: sess-1"));
        assert!(body.contains("COMMAND_ID: cmd-1"));
        assert!(body.contains("EXIT: 0"));
        assert!(body.contains("DISCONNECTED: true"));
    }

    #[test]
    fn run_render_emits_disconnected_false_when_session_kept() {
        let result = GetCommandOutputResult {
            command_id: CommandId::new("cmd-2".to_string()),
            status: CommandStatus::Completed,
            stdout: Bytes::new(),
            stderr: Bytes::new(),
            exit_code: Some(2),
            error: None,
            timed_out: false,
            last_seq: 0,
        };
        let body = super::run_render(&result, "sess-x", false);
        assert!(body.contains("DISCONNECTED: false"));
        assert!(body.contains("EXIT: 2"));
    }

    #[test]
    fn run_structured_mirrors_run_render() {
        let result = GetCommandOutputResult {
            command_id: CommandId::new("cmd-3".to_string()),
            status: CommandStatus::Completed,
            stdout: Bytes::from_static(b"out"),
            stderr: Bytes::from_static(b"err"),
            exit_code: Some(0),
            error: None,
            timed_out: false,
            last_seq: 0,
        };
        let json = super::run_structured(&result, "sess-9", true);
        assert_eq!(json["tool"], "ssh_run");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["session_id"], "sess-9");
        assert_eq!(json["command_id"], "cmd-3");
        assert_eq!(json["disconnected"], true);
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["stdout"], "out");
        assert_eq!(json["stderr"], "err");
        assert_eq!(json["timed_out"], false);
    }

    #[test]
    fn run_render_promotes_timeout_status_when_timed_out() {
        let result = GetCommandOutputResult {
            command_id: CommandId::new("cmd-t".to_string()),
            status: CommandStatus::Completed,
            stdout: Bytes::new(),
            stderr: Bytes::new(),
            exit_code: None,
            error: None,
            timed_out: true,
            last_seq: 0,
        };
        let body = super::run_render(&result, "sess-1", true);
        assert!(body.starts_with("SSH_RUN: TIMEOUT"));
        let structured = super::run_structured(&result, "sess-1", true);
        assert_eq!(structured["status"], "timeout");
    }

    #[test]
    fn batch_render_emits_per_index_blocks() {
        let result = GetCommandOutputResult {
            command_id: CommandId::new("cmd-0".to_string()),
            status: CommandStatus::Completed,
            stdout: Bytes::from_static(b"yes"),
            stderr: Bytes::new(),
            exit_code: Some(0),
            error: None,
            timed_out: false,
            last_seq: 0,
        };
        let entries = vec![
            super::BatchEntryView::Executed {
                index: 0,
                command: "uptime",
                result: &result,
            },
            super::BatchEntryView::Skipped {
                index: 1,
                command: "false",
            },
        ];
        let body = super::batch_render("sess-b", true, 1, 2, &entries);
        assert!(body.starts_with("SSH_EXEC_BATCH: HALTED"));
        assert!(body.contains("SESSION_ID: sess-b"));
        assert!(body.contains("EXECUTED: 1"));
        assert!(body.contains("TOTAL: 2"));
        assert!(body.contains("--- command #0 [COMPLETED] uptime exit=0 ---"));
        assert!(body.contains("--- skipped #1: false ---"));
    }

    #[test]
    fn batch_entry_structured_carries_index_and_status() {
        let result = GetCommandOutputResult {
            command_id: CommandId::new("cmd-7".to_string()),
            status: CommandStatus::Completed,
            stdout: Bytes::from_static(b"o"),
            stderr: Bytes::new(),
            exit_code: Some(1),
            error: None,
            timed_out: false,
            last_seq: 0,
        };
        let json = super::batch_entry_structured(2, "ls", &result);
        assert_eq!(json["index"], 2);
        assert_eq!(json["command"], "ls");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["exit_code"], 1);
        assert_eq!(json["command_id"], "cmd-7");
    }

    #[test]
    fn batch_skipped_entry_marks_status() {
        let json = super::batch_skipped_entry(3, "tail -n 3");
        assert_eq!(json["index"], 3);
        assert_eq!(json["status"], "skipped");
        assert_eq!(json["command"], "tail -n 3");
    }

    // -- v5 Phase 3 — SUB_LEAK_RISK warnings -----------------------------

    use super::{list_commands_render_with_warnings, list_commands_structured_with_warnings};
    use crate::adapters::lifecycle::leak_watcher::{LeakRiskAlert, LeakRiskSeverity};
    use crate::ports::subscriber_registry::ResourceKind;

    fn warn_alert(kind: ResourceKind, id: &str, age_ms: u64) -> LeakRiskAlert {
        LeakRiskAlert {
            kind,
            resource_id: id.to_string(),
            age_ms,
            severity: LeakRiskSeverity::Warn,
        }
    }

    #[test]
    fn list_commands_render_with_warnings_appends_warn_line() {
        let outcome = ListCommandsOutcome {
            commands: vec![],
            total: 0,
        };
        let alerts = vec![warn_alert(ResourceKind::Command, "leaky-cmd", 4_500)];
        let body = list_commands_render_with_warnings(outcome, &alerts);
        assert!(
            body.contains("WARN: SUB_LEAK_RISK command://leaky-cmd/output age_ms=4500"),
            "missing WARN line, body: {body}"
        );
    }

    #[test]
    fn list_commands_render_with_no_alerts_appends_subscribe_steering() {
        let outcome = ListCommandsOutcome {
            commands: vec![],
            total: 0,
        };
        let body = list_commands_render_with_warnings(outcome, &[]);
        assert!(body.starts_with("SSH_COMMANDS: OK\nCOUNT: 0"));
        assert!(body.contains("NEXT: ssh_exec("));
        assert!(body.contains("sub_open uri=command://"));
    }

    #[test]
    fn list_commands_structured_with_warnings_includes_array() {
        let outcome = ListCommandsOutcome {
            commands: vec![],
            total: 0,
        };
        let alerts = vec![warn_alert(ResourceKind::Command, "leaky", 3_000)];
        let json = list_commands_structured_with_warnings(&outcome, &alerts);
        let arr = json["warnings"].as_array().expect("warnings");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["code"], "SUB_LEAK_RISK");
        assert_eq!(arr[0]["resource"], "command://leaky/output");
        assert_eq!(arr[0]["severity"], "warn");
    }
}
