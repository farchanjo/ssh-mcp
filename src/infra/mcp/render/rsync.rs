//! Rsync markdown + structured-content renderers (ADR 0011).
//!
//! Two surfaces:
//!
//! - [`started_render`] / [`started_structured`] — emitted from
//!   `ssh_rsync` once the transport adapter has handshaked the session.
//! - [`stats_render`] / [`stats_structured`] — emitted from
//!   `ssh_rsync_stats` for both in-flight and terminal sessions.
//! - [`cancel_render`] / [`cancel_structured`] — emitted from
//!   `ssh_rsync_cancel` after the cancel op landed.
//!
//! The block-markdown shape mirrors the v6 SFTP renderer family: first
//! line `TOOL: STATUS`, then `KEY: value` lines, ending with
//! `HINT:` / `NEXT:` advisories per ADR 0005. The structured-content
//! JSON twin lives alongside per ADR 0007 § "Error taxonomy" and
//! v4.7's `tool / status / ...` convention.

use serde_json::{Value, json};

use crate::domain::rsync::RsyncStats;
use crate::infra::mcp::helpers::output::{format_bytes_human, sanitize_value};

/// Transport tier label used on the `TRANSPORT:` line + the structured
/// `transport` field. Matches the wire shape of the future
/// `RsyncTransportKind` projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsyncTransportLabel {
    /// Wire-compat client (rsync v31).
    Wire,
    /// Universal SFTP fallback.
    Sftp,
}

impl RsyncTransportLabel {
    /// Lower-snake-case label used on the wire (`wire` / `sftp`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wire => "wire",
            Self::Sftp => "sftp",
        }
    }
}

/// Inputs for the `SSH_RSYNC: STARTED` render.
#[derive(Debug, Clone)]
pub struct StartedRender<'a> {
    /// Minted rsync session id.
    pub rsync_id: &'a str,
    /// Owning SSH session id.
    pub session_id: &'a str,
    /// Source path (local or remote).
    pub src: &'a str,
    /// Destination path (local or remote).
    pub dst: &'a str,
    /// Transport tier the planner picked.
    pub transport: RsyncTransportLabel,
    /// File count the planner expects to handle (`0` when unknown).
    pub files_planned: u64,
    /// Byte count the planner expects to handle (`0` when unknown).
    pub bytes_planned: u64,
    /// Whether the request asked for `--dry-run`.
    pub dry_run: bool,
    /// Whether the request asked for `--delete`.
    pub delete: bool,
}

/// Render the `SSH_RSYNC: STARTED` markdown block. Mirrors
/// [`crate::infra::mcp::render::sftp::upload_render`]'s shape so a
/// downstream client can branch on the leading `TOOL: STATUS` line
/// alone.
#[must_use]
pub fn started_render(input: &StartedRender<'_>) -> String {
    let mut out = String::with_capacity(384);
    append_started_header(&mut out, input);
    append_started_advisories(&mut out, input.rsync_id);
    out
}

fn append_started_header(out: &mut String, input: &StartedRender<'_>) {
    out.push_str("SSH_RSYNC: STARTED\nRSYNC_ID: ");
    out.push_str(input.rsync_id);
    out.push_str("\nSESSION_ID: ");
    out.push_str(input.session_id);
    out.push_str("\nFROM: ");
    out.push_str(&sanitize_value(input.src));
    out.push_str("\nTO: ");
    out.push_str(&sanitize_value(input.dst));
    out.push_str("\nTRANSPORT: ");
    out.push_str(input.transport.as_str());
    out.push_str("\nFILES_PLANNED: ");
    out.push_str(&input.files_planned.to_string());
    out.push_str("\nBYTES_PLANNED: ");
    out.push_str(&input.bytes_planned.to_string());
    out.push_str(" (");
    out.push_str(&format_bytes_human(input.bytes_planned));
    out.push(')');
    if input.dry_run {
        out.push_str("\nDRY_RUN: true");
    }
    if input.delete {
        out.push_str("\nDELETE: true");
    }
}

fn append_started_advisories(out: &mut String, rsync_id: &str) {
    out.push_str(
        "\nHINT: REQUIRED NEXT STEP: subscribe to the progress lane to drain per-file events without polling.",
    );
    out.push_str("\nNEXT: sub_open uri=rsync://");
    out.push_str(rsync_id);
    out.push_str("/progress");
}

/// Build the `ssh_rsync` started structured payload mirroring
/// [`started_render`].
#[must_use]
pub fn started_structured(input: &StartedRender<'_>) -> Value {
    json!({
        "tool":   "ssh_rsync",
        "status": "started",
        "rsync_id":   input.rsync_id,
        "session_id": input.session_id,
        "from":       input.src,
        "to":         input.dst,
        "transport":  input.transport.as_str(),
        "files_planned": input.files_planned,
        "bytes_planned": input.bytes_planned,
        "dry_run":  input.dry_run,
        "delete":   input.delete,
        "next": [
            format!("sub_open uri=rsync://{}/progress", input.rsync_id),
        ],
    })
}

/// Render the `SSH_RSYNC_STATS: OK` markdown block.
#[must_use]
pub fn stats_render(rsync_id: &str, session_status: &str, stats: &RsyncStats) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("SSH_RSYNC_STATS: OK\nRSYNC_ID: ");
    out.push_str(rsync_id);
    out.push_str("\nSTATUS: ");
    out.push_str(session_status);
    out.push_str("\nFILES_TOTAL: ");
    out.push_str(&stats.files_total.to_string());
    out.push_str("\nFILES_DONE: ");
    out.push_str(&stats.files_done.to_string());
    out.push_str("\nFILES_DELETED: ");
    out.push_str(&stats.files_deleted.to_string());
    out.push_str("\nFILES_FAILED: ");
    out.push_str(&stats.files_failed.to_string());
    out.push_str("\nBYTES_TOTAL: ");
    out.push_str(&stats.bytes_total.to_string());
    out.push_str("\nBYTES_TRANSFERRED: ");
    out.push_str(&stats.bytes_transferred.to_string());
    out.push_str("\nBYTES_SKIPPED: ");
    out.push_str(&stats.bytes_skipped.to_string());
    out
}

/// Build the structured payload for `ssh_rsync_stats`.
#[must_use]
pub fn stats_structured(rsync_id: &str, session_status: &str, stats: &RsyncStats) -> Value {
    json!({
        "tool":   "ssh_rsync_stats",
        "status": "ok",
        "rsync_id": rsync_id,
        "session_status": session_status,
        "files_total":       stats.files_total,
        "files_done":        stats.files_done,
        "files_deleted":     stats.files_deleted,
        "files_failed":      stats.files_failed,
        "bytes_total":       stats.bytes_total,
        "bytes_transferred": stats.bytes_transferred,
        "bytes_skipped":     stats.bytes_skipped,
    })
}

/// Render the `SSH_RSYNC_CANCEL: OK` markdown block.
#[must_use]
pub fn cancel_render(rsync_id: &str) -> String {
    let mut out = String::with_capacity(64);
    out.push_str("SSH_RSYNC_CANCEL: OK\nRSYNC_ID: ");
    out.push_str(rsync_id);
    out
}

/// Build the structured payload for `ssh_rsync_cancel`.
#[must_use]
pub fn cancel_structured(rsync_id: &str) -> Value {
    json!({
        "tool":   "ssh_rsync_cancel",
        "status": "ok",
        "rsync_id": rsync_id,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        RsyncTransportLabel, StartedRender, cancel_render, cancel_structured, started_render,
        started_structured, stats_render, stats_structured,
    };
    use crate::domain::rsync::RsyncStats;

    fn fixture<'a>() -> StartedRender<'a> {
        StartedRender {
            rsync_id: "rs-1",
            session_id: "sess-1",
            src: "/home/me/build/",
            dst: "remote:/var/www/",
            transport: RsyncTransportLabel::Sftp,
            files_planned: 42,
            bytes_planned: 1_048_576,
            dry_run: false,
            delete: false,
        }
    }

    #[test]
    fn started_render_emits_required_keys() {
        let m = started_render(&fixture());
        assert!(m.starts_with("SSH_RSYNC: STARTED\n"));
        assert!(m.contains("RSYNC_ID: rs-1"));
        assert!(m.contains("SESSION_ID: sess-1"));
        assert!(m.contains("TRANSPORT: sftp"));
        assert!(m.contains("FILES_PLANNED: 42"));
        assert!(m.contains("BYTES_PLANNED: 1048576"));
        assert!(m.contains("HINT: REQUIRED NEXT STEP"));
        assert!(m.contains("NEXT: sub_open uri=rsync://rs-1/progress"));
    }

    #[test]
    fn started_render_emits_dry_run_and_delete_when_set() {
        let mut input = fixture();
        input.dry_run = true;
        input.delete = true;
        let m = started_render(&input);
        assert!(m.contains("DRY_RUN: true"));
        assert!(m.contains("DELETE: true"));
    }

    #[test]
    fn started_render_omits_dry_run_and_delete_when_unset() {
        let m = started_render(&fixture());
        assert!(!m.contains("DRY_RUN:"));
        assert!(!m.contains("DELETE:"));
    }

    #[test]
    fn started_structured_carries_every_field() {
        let v = started_structured(&fixture());
        assert_eq!(v["tool"], "ssh_rsync");
        assert_eq!(v["status"], "started");
        assert_eq!(v["rsync_id"], "rs-1");
        assert_eq!(v["transport"], "sftp");
        assert_eq!(v["files_planned"], 42);
        assert_eq!(v["bytes_planned"], 1_048_576);
        let next = v["next"].as_array().expect("next array");
        assert_eq!(next.len(), 1);
        assert!(
            next[0]
                .as_str()
                .unwrap_or_default()
                .contains("rsync://rs-1")
        );
    }

    #[test]
    fn stats_render_emits_all_counters() {
        let stats = RsyncStats {
            files_total: 100,
            files_done: 75,
            files_deleted: 3,
            files_failed: 1,
            bytes_total: 2048,
            bytes_transferred: 1024,
            bytes_skipped: 256,
        };
        let m = stats_render("rs-1", "running", &stats);
        assert!(m.starts_with("SSH_RSYNC_STATS: OK\n"));
        assert!(m.contains("RSYNC_ID: rs-1"));
        assert!(m.contains("STATUS: running"));
        assert!(m.contains("FILES_TOTAL: 100"));
        assert!(m.contains("FILES_DONE: 75"));
        assert!(m.contains("FILES_DELETED: 3"));
        assert!(m.contains("FILES_FAILED: 1"));
        assert!(m.contains("BYTES_TRANSFERRED: 1024"));
        assert!(m.contains("BYTES_SKIPPED: 256"));
    }

    #[test]
    fn stats_structured_carries_every_field() {
        let stats = RsyncStats {
            files_total: 10,
            files_done: 5,
            files_deleted: 1,
            files_failed: 0,
            bytes_total: 1024,
            bytes_transferred: 512,
            bytes_skipped: 0,
        };
        let v = stats_structured("rs-2", "completed", &stats);
        assert_eq!(v["tool"], "ssh_rsync_stats");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["rsync_id"], "rs-2");
        assert_eq!(v["session_status"], "completed");
        assert_eq!(v["files_total"], 10);
        assert_eq!(v["bytes_transferred"], 512);
    }

    #[test]
    fn cancel_render_pins_status_line() {
        let m = cancel_render("rs-9");
        assert_eq!(m, "SSH_RSYNC_CANCEL: OK\nRSYNC_ID: rs-9");
    }

    #[test]
    fn cancel_structured_carries_id() {
        let v = cancel_structured("rs-9");
        assert_eq!(v["tool"], "ssh_rsync_cancel");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["rsync_id"], "rs-9");
    }

    #[test]
    fn transport_label_round_trip() {
        assert_eq!(RsyncTransportLabel::Wire.as_str(), "wire");
        assert_eq!(RsyncTransportLabel::Sftp.as_str(), "sftp");
    }
}
