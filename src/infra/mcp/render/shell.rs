//! Interactive shell markdown renderers.
//!
//! Mirrors v3 `src/mcp/message/builder.rs` —
//! `ShellOpenBuilder`, `render_shell_write_ok`,
//! `render_shell_send_key_ok`, `ShellReadBuilder`, `ShellWaitForBuilder`,
//! `render_shell_close_ok` — but takes the v4 use case Outcomes as input.

use serde_json::{Value, json};

use crate::application::close_shell::CloseShellOutcome;
use crate::application::open_shell::OpenShellOutcome;
use crate::application::read_shell::{ReadShellOutcome, ReadShellStatus};
use crate::application::send_key::SendKeyOutcome;
use crate::application::wait_for_pattern::{WaitForPatternOutcome, WaitForPatternStatus};
use crate::application::write_shell::WriteShellOutcome;
use crate::domain::ids::{AgentId, SessionId};
use crate::domain::shell::ShellEntity;
use crate::infra::mcp::helpers::nonce::generate_nonce;
use crate::infra::mcp::helpers::output::{
    render_output_block, sanitize_value, truncate_utf8_safe_tail,
};

/// Default byte cap applied to data blocks when the request did not
/// supply one. Mirrors the v3 `output_default_bytes` config knob (16
/// KiB).
const DEFAULT_OUTPUT_BYTES: usize = 16 * 1024;

/// Render an [`OpenShellOutcome`] as the v3 `SSH_SHELL_OPEN` block.
#[must_use]
pub fn shell_open_render(outcome: OpenShellOutcome) -> String {
    shell_open_render_with_initial(outcome, None)
}

/// v4.7-step7 variant of [`shell_open_render`].
///
/// Injects an `INITIAL_BUFFER:` line right after the standard fields
/// when the inbound `peek_initial_shell_buffer` poll captured non-empty
/// stdout within the budget. `initial_buffer` carries the head-truncated
/// byte slice (already capped by
/// [`crate::infra::mcp::tool_router::SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES`]).
/// Pass `None` to retain the byte-identical legacy shape.
#[must_use]
pub fn shell_open_render_with_initial(
    outcome: OpenShellOutcome,
    initial_buffer: Option<&[u8]>,
) -> String {
    let OpenShellOutcome {
        shell,
        session_id,
        agent_id,
    } = outcome;
    let shell_id = shell.id.as_str().to_string();
    let mut out = String::with_capacity(384);
    append_shell_open_header(&mut out, &shell, &shell_id, &session_id, agent_id.as_ref());
    if let Some(bytes) = initial_buffer.filter(|b| !b.is_empty()) {
        append_initial_buffer_line(&mut out, bytes);
    }
    // v5 Phase 3 — subscribe is RECOMMENDED for shells: polling via
    // ssh_shell_read still works, but push deletes the polling latency.
    append_subscribe_hint(
        &mut out,
        &format!(
            "RECOMMENDED: ssh_subscribe uri=shell://{shell_id}/output. Falls back gracefully if you skip (use ssh_shell_read)."
        ),
    );
    append_next_line(&mut out, &next_hint_for_shell_open(&shell_id));
    out
}

/// Stamp the canonical `SSH_SHELL_OPEN` field block into `out`.
///
/// Pulled out so [`shell_open_render_with_initial`] stays under the
/// 30-line ceiling.
fn append_shell_open_header(
    out: &mut String,
    shell: &ShellEntity,
    shell_id: &str,
    session_id: &SessionId,
    agent_id: Option<&AgentId>,
) {
    out.push_str("SSH_SHELL_OPEN: OK\nSHELL_ID: ");
    out.push_str(shell_id);
    out.push_str("\nSESSION_ID: ");
    out.push_str(session_id.as_str());
    out.push_str("\nTERM: ");
    out.push_str(&sanitize_value(&shell.terminal.term_type));
    out.push(' ');
    out.push_str(&shell.terminal.cols.to_string());
    out.push('x');
    out.push_str(&shell.terminal.rows.to_string());
    if let Some(agent) = agent_id {
        out.push_str("\nAGENT_ID: ");
        out.push_str(&sanitize_value(agent.as_str()));
    }
}

/// Append `INITIAL_BUFFER: <escaped-bytes>` to the response.
///
/// The bytes arrive already head-capped; `sanitize_value` escapes
/// control chars the same way the rest of the markdown shape does so
/// multi-line banners stay on a single line.
fn append_initial_buffer_line(out: &mut String, bytes: &[u8]) {
    let lossy = String::from_utf8_lossy(bytes);
    out.push_str("\nINITIAL_BUFFER: ");
    out.push_str(&sanitize_value(&lossy));
}

/// Successor tools after `ssh_shell_open`.
///
/// v5 Phase 3 ordering: `ssh_subscribe` FIRST (push), then drive the
/// shell, with the polling fallback (`ssh_shell_read`) listed last.
fn next_hint_for_shell_open(shell_id: &str) -> String {
    format!(
        "ssh_subscribe uri=shell://{shell_id}/output | \
         ssh_shell_write | \
         ssh_shell_send_key | \
         ssh_shell_read (poll fallback)"
    )
}

/// Render a [`WriteShellOutcome`] as the v3 `SSH_SHELL_WRITE` block.
#[must_use]
pub fn shell_write_render(outcome: WriteShellOutcome) -> String {
    let WriteShellOutcome {
        shell_id,
        bytes_written,
        last_activity_at: _,
    } = outcome;
    let mut out = String::with_capacity(192);
    out.push_str("SSH_SHELL_WRITE: OK\nSHELL_ID: ");
    out.push_str(shell_id.as_str());
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&bytes_written.to_string());
    append_next_line(&mut out, &next_hint_for_shell_drive(shell_id.as_str()));
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
    let mut out = String::with_capacity(256);
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
    append_next_line(&mut out, &next_hint_for_shell_drive(shell_id.as_str()));
    out
}

/// Successor tools after a shell drive call (`ssh_shell_write` /
/// `ssh_shell_send_key`) — read the buffered response either via
/// subscription or pull-mode read.
fn next_hint_for_shell_drive(shell_id: &str) -> String {
    format!(
        "resources/read shell://{shell_id}/output?cursor=auto | \
         ssh_shell_wait_for | \
         ssh_shell_read"
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
    let mut out = String::with_capacity(192 + data_block.len());
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
    if let Some(hint) = next_hint_for_wait_for(outcome.status, outcome.shell_id.as_str()) {
        append_next_line(&mut out, &hint);
    }
    out
}

/// Successor advisory after `ssh_shell_wait_for`. `Matched` invites a
/// write / send-key / close. `Timeout` invites another wait, a pull
/// read, or close. `Closed` is terminal — no `NEXT`.
fn next_hint_for_wait_for(status: WaitForPatternStatus, shell_id: &str) -> Option<String> {
    match status {
        WaitForPatternStatus::Matched => Some(format!(
            "ssh_shell_write(shell_id={shell_id}, ...) | \
             ssh_shell_send_key(shell_id={shell_id}, ...) | \
             ssh_shell_close(shell_id={shell_id})"
        )),
        WaitForPatternStatus::Timeout => Some(format!(
            "ssh_shell_wait_for(shell_id={shell_id}, ...) | \
             ssh_shell_read(shell_id={shell_id}) | \
             ssh_shell_close(shell_id={shell_id})"
        )),
        WaitForPatternStatus::Closed => None,
    }
}

/// Render a [`CloseShellOutcome`] as the v3 `SSH_SHELL_CLOSE` block.
#[must_use]
pub fn shell_close_render(outcome: &CloseShellOutcome) -> String {
    let mut out = String::with_capacity(64);
    out.push_str("SSH_SHELL_CLOSE: OK\nSHELL_ID: ");
    out.push_str(outcome.shell_id.as_str());
    out
}

// ---------------------------------------------------------------------------
// v4.7 — structured_content payloads (JSON parallel to the Markdown body)
// ---------------------------------------------------------------------------

/// Build the open-shell structured payload mirroring [`shell_open_render`].
#[must_use]
pub fn shell_open_structured(outcome: &OpenShellOutcome) -> Value {
    shell_open_structured_with_initial(outcome, None)
}

/// v4.7-step7 variant of [`shell_open_structured`].
///
/// Emits `initial_buffer` (UTF-8 lossy decoded) on the structured
/// payload when the inbound peek captured stdout. The field is omitted
/// when the peek returned empty (and never overrides the legacy `next`
/// advisory).
#[must_use]
pub fn shell_open_structured_with_initial(
    outcome: &OpenShellOutcome,
    initial_buffer: Option<&[u8]>,
) -> Value {
    let mut json = json!({
        "tool":   "ssh_shell_open",
        "status": "ok",
        "session_id": outcome.session_id.as_str(),
        "shell_id":   outcome.shell.id.as_str(),
        "term":   outcome.shell.terminal.term_type,
        "cols":   outcome.shell.terminal.cols,
        "rows":   outcome.shell.terminal.rows,
        "agent_id": outcome.agent_id.as_ref().map(AgentId::as_str),
        "next": [
            "resources/subscribe shell://<shell_id>/output",
            "ssh_shell_write",
            "ssh_shell_send_key",
        ],
    });
    if let Some(bytes) = initial_buffer.filter(|b| !b.is_empty())
        && let Some(obj) = json.as_object_mut()
    {
        obj.insert(
            "initial_buffer".to_string(),
            Value::String(String::from_utf8_lossy(bytes).into_owned()),
        );
    }
    json
}

/// Build the write-shell structured payload mirroring [`shell_write_render`].
#[must_use]
pub fn shell_write_structured(outcome: &WriteShellOutcome) -> Value {
    json!({
        "tool":   "ssh_shell_write",
        "status": "ok",
        "shell_id":   outcome.shell_id.as_str(),
        "bytes_sent": outcome.bytes_written,
        "next": [
            "resources/read shell://<shell_id>/output?cursor=auto",
            "ssh_shell_wait_for",
            "ssh_shell_read",
        ],
    })
}

/// Build the send-key structured payload mirroring [`shell_send_key_render`].
#[must_use]
pub fn shell_send_key_structured(outcome: &SendKeyOutcome) -> Value {
    let modifiers: Vec<String> = outcome
        .modifier_label
        .as_ref()
        .map(|label| label.split('+').map(str::to_string).collect())
        .unwrap_or_default();
    json!({
        "tool":   "ssh_shell_send_key",
        "status": "ok",
        "shell_id":   outcome.shell_id.as_str(),
        "key":        outcome.key_label,
        "modifiers":  modifiers,
        "repeat":     outcome.repeat,
        "bytes_sent": outcome.bytes_sent,
        "next": [
            "resources/read shell://<shell_id>/output?cursor=auto",
            "ssh_shell_wait_for",
            "ssh_shell_read",
        ],
    })
}

const fn read_status_lower(s: ReadShellStatus) -> &'static str {
    match s {
        ReadShellStatus::Open => "open",
        ReadShellStatus::Closed => "closed",
        ReadShellStatus::Timeout => "timeout",
    }
}

/// Build the read-shell structured payload mirroring [`shell_read_render`].
/// `cleared` echoes the request flag passed in by the tool router so the
/// LLM can branch on whether the buffer was drained.
#[must_use]
pub fn shell_read_structured(outcome: &ReadShellOutcome, cleared: bool) -> Value {
    let (data, info) = truncate_utf8_safe_tail(&outcome.data, DEFAULT_OUTPUT_BYTES);
    json!({
        "tool":     "ssh_shell_read",
        "status":   read_status_lower(outcome.status),
        "shell_id": outcome.shell_id.as_str(),
        "stdout":   data,
        "bytes_returned": info.shown_bytes,
        "buffer_size_at_snapshot": info.total_bytes,
        "cleared":   cleared,
        "truncated": info.was_truncated(),
    })
}

const fn wait_for_status_lower(s: WaitForPatternStatus) -> &'static str {
    match s {
        WaitForPatternStatus::Matched => "matched",
        WaitForPatternStatus::Timeout => "timeout",
        WaitForPatternStatus::Closed => "closed",
    }
}

/// Build the wait-for-pattern structured payload mirroring
/// [`shell_wait_for_render`].
#[must_use]
pub fn shell_wait_for_structured(outcome: &WaitForPatternOutcome) -> Value {
    let (data, info) = truncate_utf8_safe_tail(&outcome.data, DEFAULT_OUTPUT_BYTES);
    let next = match outcome.status {
        WaitForPatternStatus::Matched => Some(json!([
            "ssh_shell_write",
            "ssh_shell_send_key",
            "ssh_shell_close",
        ])),
        WaitForPatternStatus::Timeout => Some(json!([
            "ssh_shell_wait_for",
            "ssh_shell_read",
            "ssh_shell_close",
        ])),
        WaitForPatternStatus::Closed => None,
    };
    json!({
        "tool":     "ssh_shell_wait_for",
        "status":   wait_for_status_lower(outcome.status),
        "shell_id": outcome.shell_id.as_str(),
        "matched_pattern": outcome.matched_pattern,
        "stdout":   data,
        "bytes_returned": info.shown_bytes,
        "next":     next,
    })
}

/// Build the close-shell structured payload mirroring [`shell_close_render`].
#[must_use]
pub fn shell_close_structured(outcome: &CloseShellOutcome) -> Value {
    json!({
        "tool":   "ssh_shell_close",
        "status": "ok",
        "shell_id": outcome.shell_id.as_str(),
    })
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
        assert!(
            m.starts_with("SSH_SHELL_WRITE: OK\nSHELL_ID: shell-1\nBYTES_SENT: 42\n"),
            "body: {m}"
        );
        assert!(
            m.contains("\nNEXT: resources/read shell://shell-1/output?cursor=auto"),
            "body: {m}"
        );
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
