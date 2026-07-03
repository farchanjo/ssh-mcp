//! Domain-level error type returned by use cases.
//!
//! Variants are designed to be exhaustively matched at the adapter layer so
//! HTTP / stdio responders can render a stable error code per case without
//! string parsing.

use thiserror::Error;

use super::auth::AuthError;
use super::ids::{CommandId, ForwardId, SerialId, SessionId, ShellId, TransferId};
use super::lifecycle::LifecycleState;
use super::subscription::SubId;

/// Top-level error variant produced by use cases.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    /// A session referenced by id was not found.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    /// A command referenced by id was not found.
    #[error("command not found: {0}")]
    CommandNotFound(CommandId),

    /// A shell referenced by id was not found.
    #[error("shell not found: {0}")]
    ShellNotFound(ShellId),

    /// A transfer referenced by id was not found.
    #[error("transfer not found: {0}")]
    TransferNotFound(TransferId),

    /// A forwarder referenced by id was not found.
    #[error("forward not found: {0}")]
    ForwardNotFound(ForwardId),

    /// No forwarder is listening on the given local port. Distinct from
    /// [`DomainError::ForwardNotFound`], which keys on the domain
    /// `ForwardId` minted for `forward://` resource URIs — this variant
    /// keys on the raw `local_port` used by [`crate::ports::ssh_client::SshClientPort::close_forward`],
    /// which has no `ForwardId` in scope.
    #[error("no forwarder listening on local port: {0}")]
    ForwardPortNotFound(u16),

    /// An open serial port referenced by id was not found.
    /// v5.2 (ADR 0009).
    #[error("serial not found: {0}")]
    SerialNotFound(SerialId),

    /// Serial transport-level error surfaced by the adapter
    /// (open / read / write / close failures).
    /// v5.2 (ADR 0009).
    #[error("serial error: {0}")]
    Serial(String),

    /// Per-session command capacity was exhausted.
    #[error("max commands per session exceeded (limit {limit})")]
    MaxCommandsExceeded {
        /// Configured per-session command cap.
        limit: usize,
    },

    /// Per-session shell capacity was exhausted.
    #[error("max shells per session exceeded (limit {limit})")]
    MaxShellsExceeded {
        /// Configured per-session shell cap.
        limit: usize,
    },

    /// Per-session transfer capacity was exhausted.
    #[error("max transfers per session exceeded (limit {limit})")]
    MaxTransfersExceeded {
        /// Configured per-session transfer cap.
        limit: usize,
    },

    /// Caller-supplied input failed validation before reaching the adapter.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// SSH connection establishment failed (retry budget exhausted).
    #[error("connect failed: {0}")]
    ConnectFailed(String),

    /// Authentication chain failed.
    #[error(transparent)]
    Auth(#[from] AuthError),

    /// An operation timed out before completing.
    #[error("operation timed out: {0}")]
    Timeout(String),

    /// SFTP-level error surfaced by the adapter.
    #[error("sftp error: {0}")]
    Sftp(String),

    /// A configured local port was already in use.
    #[error("port already in use: {0}")]
    PortInUse(u16),

    /// SSH transport error not covered by a more specific variant.
    #[error("ssh transport error: {0}")]
    Transport(String),

    /// Storage / repository fault (e.g. lock poisoning, IO failure).
    #[error("storage error: {0}")]
    Storage(String),

    /// Internal invariant violated.
    #[error("internal error: {0}")]
    Internal(String),

    /// Subscriber attempted to attach to a resource that has already
    /// transitioned to [`LifecycleState::Closed`]. Used by v5 lifecycle
    /// flows so callers can distinguish a typo (`NOT_FOUND`) from a
    /// terminated resource (`gone`).
    #[error("resource gone: {0}")]
    ResourceGone(String),

    /// A lifecycle transition was attempted from a state that does not
    /// allow it (e.g. resubscribe after Closed). Includes the observed
    /// state and the attempted operation name so adapters can render an
    /// actionable diagnostic.
    #[error("lifecycle state conflict: cannot apply '{attempted}' while in {current:?}")]
    LifecycleStateConflict {
        /// State observed at the moment of the failed transition.
        current: LifecycleState,
        /// Operation that triggered the conflict.
        attempted: &'static str,
    },

    /// Bug-detector: a session refcount decrement was issued while the
    /// counter was already at zero. Surfacing this as a typed error
    /// rather than a panic keeps the strict lint baseline (`forbid
    /// panic`) intact while still flagging the bug at runtime.
    #[error("session refcount underflow on {0}")]
    SessionRefcountUnderflow(SessionId),

    /// Caller referenced a [`SubId`] that does not exist in the
    /// channel mux registry. Wire code: `SUB_NOT_FOUND` (RESOURCE
    /// category — never retry).
    #[error("subscription not found: {0}")]
    SubNotFound(SubId),

    /// Per-URI subscription cap was exhausted. Wire code:
    /// `MAX_SUBS_PER_URI_EXCEEDED` (POLICY category — retry after
    /// unsubscribing a stale lane).
    #[error("max subs per URI exceeded for {uri} (limit {limit})")]
    MaxSubsPerUriExceeded {
        /// URI that hit the cap.
        uri: String,
        /// Configured per-URI cap.
        limit: u16,
    },

    /// Process-wide subscription cap was exhausted. Wire code:
    /// `MAX_SUBS_TOTAL_EXCEEDED`.
    #[error("max subs total exceeded (limit {limit})")]
    MaxSubsTotalExceeded {
        /// Configured global cap.
        limit: u16,
    },

    /// Lane mpsc was full and the policy refused to drop. Wire code:
    /// `LANE_BUFFER_FULL` (POLICY — retry conditional on policy
    /// change).
    #[error("lane buffer full for sub {sub_id} (capacity {capacity})")]
    LaneBufferFull {
        /// Affected lane.
        sub_id: SubId,
        /// Configured mpsc capacity.
        capacity: usize,
    },

    /// Lane lost events under [`crate::domain::subscription::LagPolicy::DropOldest`]
    /// or [`crate::domain::subscription::LagPolicy::DropNewest`]. Wire
    /// code: `LAG_DETECTED` (POLICY — recover on receipt of next
    /// snapshot).
    #[error("lag detected on sub {sub_id}: {dropped} events dropped")]
    LagDetected {
        /// Affected lane.
        sub_id: SubId,
        /// Number of events dropped since the last snapshot.
        dropped: u64,
    },

    /// Mux outbound writer is blocked; lane producer should fall
    /// back to its policy. Wire code: `MUX_BACKPRESSURE`.
    #[error("mux backpressure")]
    MuxBackpressure,

    /// Caller-supplied `lag_policy` value did not match the enum.
    /// Wire code: `INVALID_LAG_POLICY` (STATE category — never retry
    /// without correcting the argument).
    #[error("invalid lag policy: {0}")]
    InvalidLagPolicy(String),

    /// Caller-supplied `lifetime` value was malformed. Wire code:
    /// `INVALID_LIFETIME`.
    #[error("invalid lifetime: {0}")]
    InvalidLifetime(String),

    // ----- ADR 0011 (rsync hybrid transport) -----------------------
    /// Wire-compat path requested but the remote does not have rsync
    /// installed. Wire code: `RSYNC_NOT_FOUND` (RESOURCE — never
    /// retry without changing transport).
    #[error("rsync not found on remote: {0}")]
    RsyncNotFound(String),

    /// Remote rsync protocol version is below v31 (ADR 0011 § "Tier 1").
    /// Wire code: `RSYNC_VERSION_TOO_OLD` (POLICY — retry only after
    /// the operator upgrades rsync or the caller switches to
    /// `transport=agent`).
    #[error("rsync protocol below v31: {0}")]
    RsyncVersionTooOld(String),

    /// Wire-compat negotiation or framing failed mid-protocol.
    /// Wire code: `RSYNC_PROTOCOL_ERROR` (TRANSPORT — retry).
    #[error("rsync protocol error: {0}")]
    RsyncProtocolError(String),

    /// File-list size exceeded the `SSH_RSYNC_FILE_LIST_LIMIT` cap.
    /// Wire code: `RSYNC_FILE_LIST_TOO_LARGE` (POLICY — retry only
    /// after tightening `--exclude` or raising the limit).
    #[error("rsync file list exceeds cap (limit {limit})")]
    RsyncFileListTooLarge {
        /// Configured cap that the planner exceeded.
        limit: u64,
    },

    /// Sync interrupted before completion (`--partial` resumes inherit
    /// from ADR 0010). Wire code: `RSYNC_PARTIAL_TRANSFER` (TRANSPORT
    /// — retry with `--partial`).
    #[error("rsync partial transfer: {0}")]
    RsyncPartialTransfer(String),

    /// Remote SFTP server lacks an operation the `transport=Sftp`
    /// fallback path needs (e.g. `setstat` for hardlinks or symlinks).
    /// Wire code: `SFTP_FEATURE_MISSING` (POLICY — retry only after
    /// dropping the request feature or switching to `transport=Wire`).
    #[error("sftp feature missing: {0}")]
    SftpFeatureMissing(String),

    // ----- ADR 0012 (inline push notifications) -------------------
    /// Caller invoked `NotifierPort::notify_ssh_output` with an
    /// `InlinePayload` whose `bytes.len()` exceeds the server cap
    /// (resolved from `SSH_INLINE_PUSH_MAX_BYTES_PER_NOTIFY`,
    /// default 32 KiB). Wire code: `INLINE_PUSH_OVERSIZE` (POLICY —
    /// non-retryable).
    ///
    /// Defensive guard at the port boundary per ADR 0012 phase 10.
    /// The production splitter in
    /// `LaneFanoutBridge::ship_inline_fragments` always passes
    /// under-cap fragments, so this variant never fires on the
    /// production fan-out path; it surfaces only for direct port
    /// callers (test harnesses, future SDK consumers, NDJSON daemon
    /// outbound paths that bypass the splitter).
    #[error("inline push payload of {payload_bytes} bytes exceeds server cap of {cap_bytes} bytes")]
    InlinePushOversize {
        /// Byte length of the `InlinePayload` that exceeded the cap.
        payload_bytes: usize,
        /// Configured per-notification ceiling (default 32 KiB,
        /// resolved from `SSH_INLINE_PUSH_MAX_BYTES_PER_NOTIFY`).
        cap_bytes: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::{AuthError, DomainError, LifecycleState, SessionId, SubId};

    #[test]
    fn auth_error_converts_via_from() {
        let auth_err = AuthError::Rejected;
        let domain: DomainError = auth_err.into();
        assert_eq!(domain, DomainError::Auth(AuthError::Rejected));
    }

    #[test]
    fn limit_variants_carry_value() {
        let err = DomainError::MaxCommandsExceeded { limit: 100 };
        assert_eq!(
            err.to_string(),
            "max commands per session exceeded (limit 100)"
        );
    }

    #[test]
    fn not_found_messages_include_id() {
        let err = DomainError::SessionNotFound(SessionId::new("sess-1".to_string()));
        assert_eq!(err.to_string(), "session not found: sess-1");
    }

    #[test]
    fn port_in_use_includes_port() {
        assert_eq!(
            DomainError::PortInUse(8080).to_string(),
            "port already in use: 8080"
        );
    }

    #[test]
    fn resource_gone_carries_uri() {
        let err = DomainError::ResourceGone("shell://sh-1/output".to_string());
        assert_eq!(err.to_string(), "resource gone: shell://sh-1/output");
    }

    #[test]
    fn lifecycle_state_conflict_renders_state_and_op() {
        let err = DomainError::LifecycleStateConflict {
            current: LifecycleState::Closed,
            attempted: "subscribe",
        };
        let rendered = err.to_string();
        assert!(rendered.contains("subscribe"), "missing op: {rendered}");
        assert!(rendered.contains("Closed"), "missing state: {rendered}");
    }

    #[test]
    fn session_refcount_underflow_includes_session_id() {
        let id = SessionId::new("sess-7".to_string());
        let err = DomainError::SessionRefcountUnderflow(id);
        assert_eq!(err.to_string(), "session refcount underflow on sess-7");
    }

    #[test]
    fn sub_not_found_carries_sub_id() {
        let err = DomainError::SubNotFound(SubId::new("019028a3".to_string()));
        let rendered = err.to_string();
        assert!(rendered.contains("019028a3"), "missing sub_id: {rendered}");
    }

    #[test]
    fn max_subs_per_uri_carries_uri_and_limit() {
        let err = DomainError::MaxSubsPerUriExceeded {
            uri: "shell://x/output".to_string(),
            limit: 16,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("shell://x/output"));
        assert!(rendered.contains("16"));
    }

    #[test]
    fn max_subs_total_carries_limit() {
        let err = DomainError::MaxSubsTotalExceeded { limit: 1024 };
        assert!(err.to_string().contains("1024"));
    }

    #[test]
    fn lane_buffer_full_carries_capacity() {
        let err = DomainError::LaneBufferFull {
            sub_id: SubId::new("s1".to_string()),
            capacity: 1024,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("s1"));
        assert!(rendered.contains("1024"));
    }

    #[test]
    fn lag_detected_carries_dropped_count() {
        let err = DomainError::LagDetected {
            sub_id: SubId::new("s2".to_string()),
            dropped: 7,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("7"));
        assert!(rendered.contains("s2"));
    }

    #[test]
    fn mux_backpressure_renders_static_message() {
        assert_eq!(DomainError::MuxBackpressure.to_string(), "mux backpressure");
    }

    #[test]
    fn invalid_lag_policy_carries_offending_value() {
        let err = DomainError::InvalidLagPolicy("BlockSlow".to_string());
        assert!(err.to_string().contains("BlockSlow"));
    }

    #[test]
    fn invalid_lifetime_carries_offending_value() {
        let err = DomainError::InvalidLifetime("forever".to_string());
        assert!(err.to_string().contains("forever"));
    }

    // ----- ADR 0011 (rsync hybrid transport) -----------------------

    #[test]
    fn rsync_not_found_carries_diagnostic_string() {
        let err = DomainError::RsyncNotFound("MISSING".to_string());
        let rendered = err.to_string();
        assert!(rendered.contains("rsync not found on remote"));
        assert!(rendered.contains("MISSING"));
    }

    #[test]
    fn rsync_version_too_old_carries_observed_protocol() {
        let err = DomainError::RsyncVersionTooOld("protocol=29".to_string());
        let rendered = err.to_string();
        assert!(rendered.contains("below v31"));
        assert!(rendered.contains("protocol=29"));
    }

    #[test]
    fn rsync_protocol_error_carries_reason() {
        let err = DomainError::RsyncProtocolError("malformed file list".to_string());
        assert!(err.to_string().contains("malformed file list"));
    }

    #[test]
    fn rsync_file_list_too_large_carries_limit() {
        let err = DomainError::RsyncFileListTooLarge { limit: 1_000_000 };
        let rendered = err.to_string();
        assert!(rendered.contains("file list"));
        assert!(rendered.contains("1000000"));
    }

    #[test]
    fn rsync_partial_transfer_carries_reason() {
        let err = DomainError::RsyncPartialTransfer("connection reset".to_string());
        assert!(err.to_string().contains("connection reset"));
    }

    #[test]
    fn sftp_feature_missing_carries_diagnostic() {
        let err = DomainError::SftpFeatureMissing("hardlinks unsupported".to_string());
        let rendered = err.to_string();
        assert!(rendered.contains("sftp feature missing"));
        assert!(rendered.contains("hardlinks"));
    }

    #[test]
    fn rsync_variants_clone_and_eq() {
        let err = DomainError::RsyncNotFound("MISSING".to_string());
        assert_eq!(err.clone(), err);
        let err2 = DomainError::SftpFeatureMissing("hardlinks".to_string());
        assert_eq!(err2.clone(), err2);
        assert_ne!(err, err2);
    }

    #[test]
    fn new_variants_clone_and_eq() {
        let a = DomainError::SubNotFound(SubId::new("x".to_string()));
        let b = a.clone();
        assert_eq!(a, b);
        let c = DomainError::MaxSubsTotalExceeded { limit: 5 };
        let d = c.clone();
        assert_eq!(c, d);
    }
}
