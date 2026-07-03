//! SFTP markdown renderers.
//!
//! Mirrors v3 `src/mcp/message/builder.rs` —
//! `TransferStartedBuilder`, `TransferProgressBuilder` — but takes the v4
//! use case Outcomes as input.

use serde_json::{Value, json};

use crate::application::download_file::DownloadOutcome;
use crate::application::get_transfer_progress::GetTransferProgressResult;
use crate::application::upload_file::UploadOutcome;
use crate::domain::ids::AgentId;
use crate::domain::transfer::{TransferDirection, TransferStatus};
use crate::infra::mcp::helpers::output::{format_bytes_human, sanitize_value};
use crate::infra::mcp::render::{append_next_line, append_subscribe_hint};

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
        resumed_from,
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
        resumed_from,
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
        resumed_from,
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
        resumed_from,
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
    resumed_from: u64,
    direction: TransferDirection,
) -> String {
    let (from, to) = match direction {
        TransferDirection::Upload => (local_path, remote_path),
        TransferDirection::Download => (remote_path, local_path),
    };
    let mut out = String::with_capacity(384);
    append_started_header(
        &mut out,
        tool,
        transfer_id,
        session_id,
        agent_id,
        from,
        to,
        total_bytes,
    );
    append_resumed_from_line(&mut out, resumed_from);
    append_started_advisories(&mut out, transfer_id);
    out
}

/// Emit the ADR 0010 `RESUMED_FROM:` line when the transfer resumed
/// from a non-zero offset. Skipped on fresh transfers so the v6.0 wire
/// shape is byte-identical for callers who never set `resume=true`.
fn append_resumed_from_line(out: &mut String, resumed_from: u64) {
    if resumed_from == 0 {
        return;
    }
    out.push_str("\nRESUMED_FROM: ");
    out.push_str(&resumed_from.to_string());
}

#[allow(
    clippy::too_many_arguments,
    reason = "private helper extracted to keep render_started under 30 lines; consolidating is worse than the lint"
)]
fn append_started_header(
    out: &mut String,
    tool: &str,
    transfer_id: &str,
    session_id: &str,
    agent_id: Option<&str>,
    from: &str,
    to: &str,
    total_bytes: u64,
) {
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
}

fn append_started_advisories(out: &mut String, transfer_id: &str) {
    // v5 Phase 3 — subscribe is RECOMMENDED for transfers: long-poll
    // via ssh_transfer_progress still works, but push avoids the
    // poll churn for slow transfers.
    append_subscribe_hint(
        out,
        &format!(
            "RECOMMENDED: sub_open uri=transfer://{transfer_id}/progress. Falls back gracefully if you skip (use ssh_transfer_progress wait=true)."
        ),
    );
    append_next_line(out, &next_hint_for_transfer(transfer_id));
}

/// Successor tools after an SFTP `STARTED` response.
///
/// v5 Phase 3 ordering: `sub_open` FIRST (push), then the long-poll
/// fallback.
fn next_hint_for_transfer(transfer_id: &str) -> String {
    format!(
        "sub_open uri=transfer://{transfer_id}/progress | \
         ssh_transfer_progress(transfer_id={transfer_id}, wait=true) (poll fallback)"
    )
}

/// Render a [`GetTransferProgressResult`] as the v3
/// `SSH_TRANSFER_PROGRESS` block.
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
    out.push_str("SSH_TRANSFER_PROGRESS: ");
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
        "sub_open uri=transfer://{transfer_id}/progress (preferred) | \
         ssh_transfer_progress(transfer_id={transfer_id}, wait=true) (poll fallback)"
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
    out.push_str("SSH_TRANSFER_PROGRESS: FAILED\nTRANSFER_ID: ");
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

// ---------------------------------------------------------------------------
// v4.7 — structured_content payloads (JSON parallel to the Markdown body)
// ---------------------------------------------------------------------------

/// Build the upload structured payload mirroring [`upload_render`].
#[must_use]
pub fn upload_structured(outcome: &UploadOutcome) -> Value {
    json!({
        "tool":   "ssh_upload",
        "status": "started",
        "transfer_id": outcome.transfer_id.as_str(),
        "session_id":  outcome.session_id.as_str(),
        "agent_id":    outcome.agent_id.as_ref().map(AgentId::as_str),
        "from":        outcome.local_path,
        "to":          outcome.remote_path,
        "size_bytes":  outcome.total_bytes,
        "resumed_from": outcome.resumed_from,
        "next": [
            "ssh_transfer_progress(wait=true)",
        ],
    })
}

/// Build the download structured payload mirroring [`download_render`].
#[must_use]
pub fn download_structured(outcome: &DownloadOutcome) -> Value {
    json!({
        "tool":   "ssh_download",
        "status": "started",
        "transfer_id": outcome.transfer_id.as_str(),
        "session_id":  outcome.session_id.as_str(),
        "agent_id":    outcome.agent_id.as_ref().map(AgentId::as_str),
        "from":        outcome.remote_path,
        "to":          outcome.local_path,
        "size_bytes":  outcome.total_bytes,
        "resumed_from": outcome.resumed_from,
        "next": [
            "ssh_transfer_progress(wait=true)",
        ],
    })
}

const fn transfer_status_lower(s: TransferStatus) -> &'static str {
    match s {
        TransferStatus::Running => "running",
        TransferStatus::Completed => "completed",
        TransferStatus::Cancelled => "cancelled",
        TransferStatus::Failed => "failed",
    }
}

const fn transfer_direction_lower(d: TransferDirection) -> &'static str {
    match d {
        TransferDirection::Upload => "upload",
        TransferDirection::Download => "download",
    }
}

/// Build the transfer-progress structured payload mirroring
/// [`transfer_progress_render`].
#[must_use]
pub fn transfer_progress_structured(result: &GetTransferProgressResult) -> Value {
    let percent = compute_percent(result.bytes_transferred, result.total_bytes);
    let next = matches!(result.status, TransferStatus::Running).then(|| {
        let id = result.transfer_id.as_str();
        json!([
            format!("resources/subscribe transfer://{id}/progress"),
            format!("ssh_transfer_progress(transfer_id={id}, wait=true)"),
        ])
    });
    json!({
        "tool":      "ssh_transfer_progress",
        "status":    transfer_status_lower(result.status),
        "transfer_id": result.transfer_id.as_str(),
        "direction": transfer_direction_lower(result.direction),
        "progress_percent":   percent,
        "bytes_transferred":  result.bytes_transferred,
        "total_bytes":        result.total_bytes,
        "error":              result.error,
        "next":               next,
    })
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
            resumed_from: 0,
            started_at: "2026-04-18T10:30:00+00:00".to_string(),
        });
        assert!(m.contains("SSH_UPLOAD: STARTED"));
        assert!(m.contains("FROM: /tmp/f.txt"));
        assert!(m.contains("TO: /home/user/f.txt"));
        assert!(m.contains("SIZE: 1.0MB (1048576 bytes)"));
        // RESUMED_FROM is suppressed for fresh uploads (resumed_from == 0)
        // so the v6.0 wire shape stays byte-identical when callers do
        // not opt into resume.
        assert!(!m.contains("RESUMED_FROM"));
    }

    #[test]
    fn upload_emits_resumed_from_when_offset_non_zero() {
        let m = upload_render(UploadOutcome {
            transfer_id: TransferId::new("xfer-2".to_string()),
            session_id: SessionId::new("sess-2".to_string()),
            agent_id: None,
            local_path: "/tmp/big.bin".to_string(),
            remote_path: "/srv/big.bin".to_string(),
            total_bytes: 10_485_760,
            resumed_from: 4_194_304,
            started_at: "2026-05-05T00:00:00+00:00".to_string(),
        });
        assert!(m.contains("SSH_UPLOAD: STARTED"));
        assert!(m.contains("RESUMED_FROM: 4194304"));
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
        assert!(m.starts_with("SSH_TRANSFER_PROGRESS: COMPLETED\n"));
        assert!(m.contains("PROGRESS: 100%"));
    }
}
