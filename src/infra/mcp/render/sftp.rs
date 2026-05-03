//! SFTP markdown renderers.
//!
//! Mirrors v3 `src/mcp/message/builder.rs` —
//! `TransferStartedBuilder`, `TransferProgressBuilder` — but takes the v4
//! use case Outcomes as input.

use crate::application::download_file::DownloadOutcome;
use crate::application::get_transfer_progress::GetTransferProgressResult;
use crate::application::upload_file::UploadOutcome;
use crate::domain::ids::AgentId;
use crate::domain::transfer::{TransferDirection, TransferStatus};
use crate::infra::mcp::helpers::output::{format_bytes_human, sanitize_value};

/// Render an [`UploadOutcome`] as the v3 `SSH_UPLOAD: STARTED` block.
#[must_use]
pub fn upload_render(outcome: UploadOutcome) -> String {
    let UploadOutcome {
        transfer_id,
        session_id,
        agent_id,
        local_path,
        remote_path,
        total_bytes,
        started_at: _,
    } = outcome;
    render_started(
        "SSH_UPLOAD",
        transfer_id.as_str(),
        session_id.as_str(),
        agent_id.as_ref().map(AgentId::as_str),
        &local_path,
        &remote_path,
        total_bytes,
        TransferDirection::Upload,
    )
}

/// Render a [`DownloadOutcome`] as the v3 `SSH_DOWNLOAD: STARTED`
/// block.
#[must_use]
pub fn download_render(outcome: DownloadOutcome) -> String {
    let DownloadOutcome {
        transfer_id,
        session_id,
        agent_id,
        local_path,
        remote_path,
        total_bytes,
        started_at: _,
    } = outcome;
    render_started(
        "SSH_DOWNLOAD",
        transfer_id.as_str(),
        session_id.as_str(),
        agent_id.as_ref().map(AgentId::as_str),
        &local_path,
        &remote_path,
        total_bytes,
        TransferDirection::Download,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "shared transfer-started renderer needs every field of the started block; pulling them into a struct hurts call-site clarity for two callers"
)]
fn render_started(
    tool: &str,
    transfer_id: &str,
    session_id: &str,
    agent_id: Option<&str>,
    local_path: &str,
    remote_path: &str,
    total_bytes: u64,
    direction: TransferDirection,
) -> String {
    let (from, to) = match direction {
        TransferDirection::Upload => (local_path, remote_path),
        TransferDirection::Download => (remote_path, local_path),
    };
    let mut out = String::with_capacity(384);
    out.push_str(tool);
    out.push_str(": STARTED\nTRANSFER_ID: ");
    out.push_str(transfer_id);
    out.push_str("\nSESSION_ID: ");
    out.push_str(session_id);
    if let Some(agent) = agent_id {
        out.push_str("\nAGENT_ID: ");
        out.push_str(&sanitize_value(agent));
    }
    out.push_str("\nFROM: ");
    out.push_str(&sanitize_value(from));
    out.push_str("\nTO: ");
    out.push_str(&sanitize_value(to));
    out.push_str("\nSIZE: ");
    out.push_str(&format_bytes_human(total_bytes));
    out.push_str(" (");
    out.push_str(&total_bytes.to_string());
    out.push_str(" bytes)\nBYTES: ");
    out.push_str(&total_bytes.to_string());
    append_subscribe_hint(
        &mut out,
        &format!("subscribe to transfer://{transfer_id}/progress for realtime progress"),
    );
    append_next_line(&mut out, &next_hint_for_transfer(transfer_id));
    out
}

/// Successor tools after an SFTP `STARTED` response — long-poll the
/// transfer to terminal state.
fn next_hint_for_transfer(transfer_id: &str) -> String {
    format!("ssh_get_transfer_progress(transfer_id={transfer_id}, wait=true)")
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

/// Render a [`GetTransferProgressResult`] as the v3
/// `SSH_GET_TRANSFER_PROGRESS` block.
#[must_use]
pub fn transfer_progress_render(result: &GetTransferProgressResult) -> String {
    let dir_upper = match result.direction {
        TransferDirection::Upload => "UPLOAD",
        TransferDirection::Download => "DOWNLOAD",
    };
    let percent = compute_percent(result.bytes_transferred, result.total_bytes);
    let status_label = match result.status {
        TransferStatus::Running => "RUNNING",
        TransferStatus::Completed => "COMPLETED",
        TransferStatus::Cancelled => "CANCELLED",
        TransferStatus::Failed => {
            return render_progress_failed(
                result.transfer_id.as_str(),
                dir_upper,
                percent,
                result.bytes_transferred,
                result.total_bytes,
                result.error.as_deref().unwrap_or("unknown error"),
            );
        }
    };
    render_progress_block(
        status_label,
        result.transfer_id.as_str(),
        dir_upper,
        percent,
        result.bytes_transferred,
        result.total_bytes,
        matches!(result.status, TransferStatus::Running),
    )
}

fn render_progress_block(
    status: &str,
    transfer_id: &str,
    dir: &str,
    percent: u8,
    bytes_transferred: u64,
    total_bytes: u64,
    in_flight: bool,
) -> String {
    let mut out = String::with_capacity(224);
    out.push_str("SSH_GET_TRANSFER_PROGRESS: ");
    out.push_str(status);
    out.push_str("\nTRANSFER_ID: ");
    out.push_str(transfer_id);
    out.push_str("\nDIRECTION: ");
    out.push_str(dir);
    out.push_str("\nPROGRESS: ");
    out.push_str(&percent.to_string());
    out.push_str("% (");
    out.push_str(&bytes_transferred.to_string());
    out.push('/');
    out.push_str(&total_bytes.to_string());
    out.push_str(" bytes)");
    if in_flight {
        append_next_line(&mut out, &next_hint_for_in_flight_transfer(transfer_id));
    }
    out
}

/// Successor advisory while a transfer is still in flight — subscribe
/// for push progress events or long-poll the same tool to terminal.
fn next_hint_for_in_flight_transfer(transfer_id: &str) -> String {
    format!(
        "resources/subscribe transfer://{transfer_id}/progress | \
         ssh_get_transfer_progress(transfer_id={transfer_id}, wait=true)"
    )
}

fn render_progress_failed(
    transfer_id: &str,
    dir: &str,
    percent: u8,
    bytes_transferred: u64,
    total_bytes: u64,
    reason: &str,
) -> String {
    let mut out = String::with_capacity(192);
    out.push_str("SSH_GET_TRANSFER_PROGRESS: FAILED\nTRANSFER_ID: ");
    out.push_str(transfer_id);
    out.push_str("\nDIRECTION: ");
    out.push_str(dir);
    out.push_str("\nPROGRESS: ");
    out.push_str(&percent.to_string());
    out.push_str("% (");
    out.push_str(&bytes_transferred.to_string());
    out.push('/');
    out.push_str(&total_bytes.to_string());
    out.push_str(" bytes)\nREASON: ");
    out.push_str(&sanitize_value(reason));
    out
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "percentage calculation where precision loss is acceptable and result is always 0..=100"
)]
fn compute_percent(transferred: u64, total: u64) -> u8 {
    if total > 0 {
        (transferred as f64 / total as f64 * 100.0) as u8
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{transfer_progress_render, upload_render};
    use crate::application::get_transfer_progress::GetTransferProgressResult;
    use crate::application::upload_file::UploadOutcome;
    use crate::domain::ids::{SessionId, TransferId};
    use crate::domain::transfer::{TransferDirection, TransferStatus};

    #[test]
    fn upload_emits_started_block() {
        let m = upload_render(UploadOutcome {
            transfer_id: TransferId::new("xfer-1".to_string()),
            session_id: SessionId::new("sess-1".to_string()),
            agent_id: None,
            local_path: "/tmp/f.txt".to_string(),
            remote_path: "/home/user/f.txt".to_string(),
            total_bytes: 1_048_576,
            started_at: "2026-04-18T10:30:00+00:00".to_string(),
        });
        assert!(m.contains("SSH_UPLOAD: STARTED"));
        assert!(m.contains("FROM: /tmp/f.txt"));
        assert!(m.contains("TO: /home/user/f.txt"));
        assert!(m.contains("SIZE: 1.0MB (1048576 bytes)"));
    }

    #[test]
    fn progress_completed_at_100() {
        let m = transfer_progress_render(&GetTransferProgressResult {
            transfer_id: TransferId::new("xfer-1".to_string()),
            direction: TransferDirection::Download,
            status: TransferStatus::Completed,
            bytes_transferred: 1_048_576,
            total_bytes: 1_048_576,
            error: None,
            last_seq: 0,
        });
        assert!(m.starts_with("SSH_GET_TRANSFER_PROGRESS: COMPLETED\n"));
        assert!(m.contains("PROGRESS: 100%"));
    }
}
