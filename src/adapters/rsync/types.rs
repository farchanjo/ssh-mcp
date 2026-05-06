//! Value objects + push-lane projections shared by the ADR 0011 rsync
//! transports.
//!
//! v7.0.0-alpha.2 architectural retrenchment: this module replaces the
//! deleted `ssh-mcp-rsync-proto` crate. The agent-binary path was
//! retracted (no second consumer of these types remains) so the proto
//! crate had no reason to live as a separate workspace member. Only the
//! still-useful types survived:
//!
//! - [`FileKind`] — file-list discriminator used by the future Wire and
//!   SFTP walkers.
//! - [`PreserveFlags`] — attribute-preservation mask carried in the
//!   public `RsyncOpts` surface.
//! - [`RsyncProgressEvent`] / [`RsyncTransportKind`] / [`SkipReason`] /
//!   [`ErrorCode`] — push-lane projection driving
//!   `rsync://<id>/progress`.
//! - [`RsyncStats`] — final aggregate snapshot returned from the sync
//!   session to `ssh_rsync_stats`.
//!
//! The on-the-wire framing types from the deleted proto crate
//! (`RsyncOp`, `RsyncOpPayload`, frame codec, op-code constants) had a
//! single producer (the host) and a single consumer (the agent). With
//! the agent gone they served no purpose and were dropped along with
//! the proto crate. The Wire transport speaks rsync v31 against a
//! remote `rsync --server` process and the SFTP transport speaks
//! plain SFTP — both reach the wire through driver-specific codecs
//! that have no use for a generic op-code enum.

use serde::{Deserialize, Serialize};

use crate::domain::rsync::RsyncStats;

// ---------------------------------------------------------------------------
// File-list value objects
// ---------------------------------------------------------------------------

/// Kind of filesystem entry surfaced by the file-list phase.
///
/// Mirrors the discriminator the Wire and SFTP walkers carry so the
/// future transports can dispatch on file kind without reaching into
/// the mode bits. The serde representation is `tag = "type"` (default)
/// so the on-wire shape stays self-describing — `serde_json` round-trips
/// keep the string form readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Regular file — eligible for delta-sync via `fast_rsync`.
    File,
    /// Directory — created via the Mkdir op and traversed recursively
    /// when [`PreserveFlags`] enables it.
    Directory,
    /// Symbolic link — preserved as-is (link target carried alongside
    /// the entry); never followed.
    Symlink,
    /// Hard link — duplicate inode reference; carried via a Hardlink op
    /// referring to a previous list-entry index.
    Hardlink,
    /// Block / character device file — preserved with `-D` only when
    /// the remote operator is root.
    Device,
    /// FIFO / named pipe — preserved with `-D`.
    Fifo,
    /// Unix domain socket — preserved with `-D`.
    Socket,
}

/// Attribute-preservation flags carried in the public
/// `SshRsyncArgs::opts.preserve` surface (ADR 0011 § Tool surface).
///
/// Field semantics mirror the rsync long flags:
/// - `perms` — `-p`
/// - `mtime` — `-t`
/// - `owner` — `-o` (root only on remote)
/// - `group` — `-g`
/// - `links` — `-l` (symlinks)
/// - `hardlinks` — `-H`
/// - `sparse` — `-S`
/// - `devices` — `-D` (root only)
///
/// `Default` matches `rsync -a` minus `-D` and minus `-H` so the safe
/// path stays root-free; opt-in via the explicit fields.
#[expect(
    clippy::struct_excessive_bools,
    reason = "field-by-field 1:1 mirror of rsync preserve flags; collapsing to a bitmask would lose serde / schemars docstrings and force every host-side caller through the same bit positions. The struct is a value object, not a state machine."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreserveFlags {
    /// Preserve POSIX mode bits (`-p`).
    pub perms: bool,
    /// Preserve modification time (`-t`).
    pub mtime: bool,
    /// Preserve numeric owner (`-o`; root only on remote).
    pub owner: bool,
    /// Preserve numeric group (`-g`).
    pub group: bool,
    /// Preserve symbolic links as-is, never follow (`-l`).
    pub links: bool,
    /// Preserve hard-link graph (`-H`).
    pub hardlinks: bool,
    /// Preserve sparse holes (`-S`).
    pub sparse: bool,
    /// Preserve block / character devices, fifos, sockets (`-D`; root
    /// only on remote).
    pub devices: bool,
}

impl Default for PreserveFlags {
    /// Defaults match `rsync -a` minus `-D` minus `-H` — preserve
    /// perms, mtime, owner, group, and symlinks. Hard links and
    /// devices stay opt-in so a non-root deploy never silently fails
    /// to preserve them; sparse is opt-in too because per-block sparse
    /// preservation requires a transport-specific code path.
    fn default() -> Self {
        Self {
            perms: true,
            mtime: true,
            owner: true,
            group: true,
            links: true,
            hardlinks: false,
            sparse: false,
            devices: false,
        }
    }
}

impl PreserveFlags {
    /// Build the `rsync -a` baseline (perms + mtime + owner + group +
    /// links). Exists alongside [`Default::default`] purely for
    /// documentation: callers asking for "rsync -a semantics" read the
    /// constructor name without skimming the field defaults.
    #[must_use]
    pub const fn archive() -> Self {
        Self {
            perms: true,
            mtime: true,
            owner: true,
            group: true,
            links: true,
            hardlinks: false,
            sparse: false,
            devices: false,
        }
    }

    /// Build the strictest preservation mask (every field on). Useful
    /// for root-to-root mirrors where the host tree must be byte- and
    /// inode-identical end-to-end.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            perms: true,
            mtime: true,
            owner: true,
            group: true,
            links: true,
            hardlinks: true,
            sparse: true,
            devices: true,
        }
    }

    /// Build the "no preservation" mask — copy bytes only. Mirrors
    /// `rsync -r` without any of the `-a` extras.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            perms: false,
            mtime: false,
            owner: false,
            group: false,
            links: false,
            hardlinks: false,
            sparse: false,
            devices: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Push-lane projection
// ---------------------------------------------------------------------------

/// Transport tier the host selected for this session. Pinned in the
/// first `SessionStarted` event so subscribers can render the path
/// (`Wire` = rsync v31 wire-compat client, `Sftp` = SFTP fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RsyncTransportKind {
    /// Tier 1 — wire-compat client speaking rsync protocol v31 to a
    /// remote `rsync --server` process.
    Wire,
    /// Tier 2 — SFTP fallback driving plain `readdir` + `stat` +
    /// `read` + `write` + `setstat` against the remote SFTP server,
    /// no remote helper required.
    Sftp,
}

/// Reason a per-file step was skipped without crossing the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Source and destination matched on size + mtime (the default
    /// rsync heuristic).
    SizeMatch,
    /// Source and destination matched on size + mtime when caller
    /// requested a `verify_checksum=false` policy.
    MtimeMatch,
    /// `--dry-run` mode: would-be op recorded for diagnostics, no
    /// destructive side-effect.
    DryRun,
}

/// Coarse-grained error code stamped on `FileFailed` /
/// `SessionFailed` events.
///
/// The string is the host-side `DomainError` wire code (e.g.
/// `"RSYNC_PROTOCOL_ERROR"` / `"SFTP_FEATURE_MISSING"`). Carrying it
/// as a `String` keeps this module decoupled from the host
/// `domain::error` taxonomy.
pub type ErrorCode = String;

/// Push-lane event carried by the `rsync://<id>/progress` resource.
///
/// `serde(tag = "kind")` keeps the wire shape self-describing in
/// `serde_json` (the push pipeline serialises through `serde_json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RsyncProgressEvent {
    /// Sync session opened; pinned at session start.
    SessionStarted {
        /// Transport tier.
        transport: RsyncTransportKind,
        /// Files the planner expected to handle.
        files_planned: u64,
        /// Bytes the planner expected to handle.
        bytes_planned: u64,
    },
    /// Per-file open beacon.
    FileStarted {
        /// Path relative to the sync root.
        rel_path: String,
        /// Total bytes expected for this file.
        bytes_total: u64,
    },
    /// Mid-file progress beacon (debounced upstream).
    FileProgress {
        /// Path relative to the sync root.
        rel_path: String,
        /// Bytes transferred so far.
        bytes_done: u64,
        /// Total bytes expected for this file.
        bytes_total: u64,
    },
    /// Per-file completion summary.
    FileCompleted {
        /// Path relative to the sync root.
        rel_path: String,
        /// Bytes that crossed the wire after delta-sync.
        bytes_transferred: u64,
        /// Bytes the delta algorithm avoided.
        bytes_skipped: u64,
    },
    /// File skipped without crossing the wire.
    FileSkipped {
        /// Path relative to the sync root.
        rel_path: String,
        /// Why the planner skipped it.
        reason: SkipReason,
    },
    /// Per-file failure (sync continues unless `bail=true`).
    FileFailed {
        /// Path relative to the sync root.
        rel_path: String,
        /// Wire error code (e.g. `"RSYNC_PROTOCOL_ERROR"`).
        code: ErrorCode,
        /// One-sentence prose describing the failure.
        detail: String,
    },
    /// Aggregate progress beacon.
    SyncProgress {
        /// Files completed (success or skip) so far.
        files_done: u64,
        /// Files the planner expected to handle.
        files_total: u64,
        /// Bytes transferred so far (delta-sync net).
        bytes_done: u64,
        /// Bytes the planner expected to handle.
        bytes_total: u64,
    },
    /// Sync finished cleanly.
    SyncCompleted {
        /// Final aggregate counters.
        stats: RsyncStats,
    },
    /// Sync failed (terminal). Lane closes after this event.
    SessionFailed {
        /// Wire error code.
        code: ErrorCode,
        /// One-sentence prose.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorCode, FileKind, PreserveFlags, RsyncProgressEvent, RsyncTransportKind, SkipReason,
    };
    // `RsyncStats` lives in `domain::rsync` (the module-level `use`
    // above brings it into scope for the type signatures); the tests
    // import it directly so the rename never silently desynchronises.
    use crate::domain::rsync::RsyncStats;

    #[test]
    fn file_kind_variants_are_distinct() {
        let kinds = [
            FileKind::File,
            FileKind::Directory,
            FileKind::Symlink,
            FileKind::Hardlink,
            FileKind::Device,
            FileKind::Fifo,
            FileKind::Socket,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn preserve_flags_default_matches_archive() {
        assert_eq!(PreserveFlags::default(), PreserveFlags::archive());
    }

    #[test]
    fn preserve_flags_archive_disables_root_only_bits() {
        let archive = PreserveFlags::archive();
        assert!(archive.perms);
        assert!(archive.mtime);
        assert!(archive.owner);
        assert!(archive.group);
        assert!(archive.links);
        assert!(!archive.hardlinks);
        assert!(!archive.sparse);
        assert!(!archive.devices);
    }

    #[test]
    fn preserve_flags_all_sets_every_bit() {
        let all = PreserveFlags::all();
        assert!(all.perms);
        assert!(all.mtime);
        assert!(all.owner);
        assert!(all.group);
        assert!(all.links);
        assert!(all.hardlinks);
        assert!(all.sparse);
        assert!(all.devices);
    }

    #[test]
    fn preserve_flags_none_clears_every_bit() {
        let none = PreserveFlags::none();
        assert!(!none.perms);
        assert!(!none.mtime);
        assert!(!none.owner);
        assert!(!none.group);
        assert!(!none.links);
        assert!(!none.hardlinks);
        assert!(!none.sparse);
        assert!(!none.devices);
    }

    #[test]
    fn rsync_stats_zero_matches_default() {
        assert_eq!(RsyncStats::zero(), RsyncStats::default());
    }

    #[test]
    fn savings_permille_is_zero_for_empty_sync() {
        assert_eq!(RsyncStats::zero().savings_permille(), 0);
    }

    #[test]
    fn savings_permille_full_skip_is_one_thousand() {
        let stats = RsyncStats {
            files_total: 1,
            files_done: 1,
            bytes_total: 1024,
            bytes_transferred: 0,
            bytes_skipped: 1024,
            files_deleted: 0,
            files_failed: 0,
        };
        assert_eq!(stats.savings_permille(), 1000);
    }

    #[test]
    fn savings_permille_half_skip() {
        let stats = RsyncStats {
            files_total: 2,
            files_done: 2,
            bytes_total: 2048,
            bytes_transferred: 1024,
            bytes_skipped: 1024,
            files_deleted: 0,
            files_failed: 0,
        };
        assert_eq!(stats.savings_permille(), 500);
    }

    #[test]
    fn rsync_progress_event_variants_pin_to_distinct_arms() {
        let events = vec![
            RsyncProgressEvent::SessionStarted {
                transport: RsyncTransportKind::Wire,
                files_planned: 100,
                bytes_planned: 4_096_000,
            },
            RsyncProgressEvent::FileStarted {
                rel_path: "src/main.rs".to_string(),
                bytes_total: 8192,
            },
            RsyncProgressEvent::FileProgress {
                rel_path: "src/main.rs".to_string(),
                bytes_done: 4096,
                bytes_total: 8192,
            },
            RsyncProgressEvent::FileCompleted {
                rel_path: "src/main.rs".to_string(),
                bytes_transferred: 1024,
                bytes_skipped: 7168,
            },
            RsyncProgressEvent::FileSkipped {
                rel_path: "Cargo.lock".to_string(),
                reason: SkipReason::SizeMatch,
            },
            RsyncProgressEvent::FileFailed {
                rel_path: "/etc/shadow".to_string(),
                code: ErrorCode::from("RSYNC_PROTOCOL_ERROR"),
                detail: "permission denied".to_string(),
            },
            RsyncProgressEvent::SyncProgress {
                files_done: 50,
                files_total: 100,
                bytes_done: 2_048_000,
                bytes_total: 4_096_000,
            },
            RsyncProgressEvent::SyncCompleted {
                stats: RsyncStats {
                    files_total: 100,
                    files_done: 100,
                    bytes_total: 4_096_000,
                    bytes_transferred: 1_048_576,
                    bytes_skipped: 3_047_424,
                    files_deleted: 0,
                    files_failed: 0,
                },
            },
            RsyncProgressEvent::SessionFailed {
                code: ErrorCode::from("RSYNC_PROTOCOL_ERROR"),
                detail: "remote rsync exited 1".to_string(),
            },
        ];
        // 9 variants — mirrors the ADR 0011 push-lane table.
        assert_eq!(events.len(), 9);
        for (i, a) in events.iter().enumerate() {
            for (j, b) in events.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }
}
