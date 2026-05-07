//! Rsync transport port (ADR 0011).
//!
//! Abstracts the actual sync driver (wire-compat client over rsync v31
//! or SFTP fallback) so the use-case layer can remain ignorant of
//! whether the bytes flow through `rsync --server` or through a plain
//! SFTP `readdir` + `stat` + `read` + `write` + `setstat` chain.
//!
//! Both impls are async and need `Send`-bounded futures (the use cases
//! drive them inside `tokio::spawn`); the port follows the v4
//! `trait_variant::make` pattern so the AFIT version stays the source
//! of truth and the `Send`-bounded alias (`RsyncTransportPort`) is the
//! public surface used by the use cases.
//!
//! v7.0.0-alpha.2 retrenchment note: the `send_op` method that
//! previously surfaced the deleted `RsyncOpPayload` enum was removed —
//! the agent-binary path that consumed it is gone, and the new Wire +
//! SFTP transports drive their on-the-wire operations through
//! transport-specific codecs that do not benefit from a generic op
//! enum.

use crate::adapters::rsync::types::{PreserveFlags, RsyncProgressEvent};
use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::rsync_ids::RsyncId;

/// Direction of the sync.
///
/// Picked at use-case time before [`RsyncTransportPort::start_session`]
/// is called. The Wire transport branches on this to decide whether to
/// drive `rsync --server` (push, our process is the sender) or
/// `rsync --server --sender` (pull, our process is the receiver). The
/// SFTP transport ignores the field — its planner walks both trees and
/// the comparator independently classifies push vs pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RsyncDirection {
    /// Local (`src`) → remote (`dst`).
    #[default]
    Push,
    /// Remote (`src`) → local (`dst`).
    Pull,
}

/// Description of an rsync session start request handed across the port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsyncStartRequest {
    /// Session that owns the russh channel.
    pub session_id: SessionId,
    /// Source path (local or `host:` prefixed). For `direction = Pull`
    /// this is the remote-side path the sender process walks. For
    /// `direction = Push` this is the local tree the host walks.
    pub src: String,
    /// Destination path (local or `host:` prefixed). For
    /// `direction = Pull` this is the local tree where the receiver
    /// drops downloaded files. For `direction = Push` this is the
    /// remote-side path the receiver writes into.
    pub dst: String,
    /// Direction of the sync. Defaults to [`RsyncDirection::Push`]
    /// so existing callers stay wire-compatible.
    pub direction: RsyncDirection,
    /// Slice 9 — `--delete`. When `true`, the receiver removes files in
    /// the destination tree that are absent from the source tree after
    /// the per-file transfer phase completes. Push direction passes the
    /// flag straight to `rsync --server`; pull direction post-walks the
    /// local destination tree against the flist.
    pub delete: bool,
    /// Slice 9 — attribute-preservation mask. The wire transport passes
    /// the matching short flags through to `rsync --server` and applies
    /// the same set of attributes locally on the receiver side. The SFTP
    /// transport keeps its own per-adapter mask (legacy plumbing) and
    /// merges this field into it on a per-call basis.
    pub preserve: PreserveFlags,
    /// Per-call `--dry-run` flag. When `true`, the SFTP transport
    /// short-circuits every destructive op into a `FileSkipped { reason:
    /// DryRun }` event without touching the destination tree. The wire
    /// transport passes the long flag straight to `rsync --server`.
    pub dry_run: bool,
    /// Per-call `--exclude=PATTERN` glob list (gitignore-style). The
    /// SFTP walker skips matching entries; the wire transport forwards
    /// the patterns to `rsync --server` via repeated `--exclude=` flags.
    pub exclude: Vec<String>,
    /// Per-call `--include=PATTERN` glob list. When non-empty, an
    /// include match overrides a matching exclude (rsync semantics).
    pub include: Vec<String>,
}

impl Default for RsyncStartRequest {
    /// Synthetic default used by test fixtures + the `..Default()`
    /// spread syntax. The session id is the empty string — production
    /// callers always overwrite it before passing the request to a
    /// transport adapter; the empty string would short-circuit at the
    /// session-not-found guard in the use case anyway.
    fn default() -> Self {
        Self {
            session_id: SessionId::new(String::new()),
            src: String::new(),
            dst: String::new(),
            direction: RsyncDirection::default(),
            delete: false,
            preserve: PreserveFlags::none(),
            dry_run: false,
            exclude: Vec::new(),
            include: Vec::new(),
        }
    }
}

/// Outcome from [`RsyncTransportPort::start_session`].
///
/// Carries the minted [`RsyncId`] plus the transport tier the planner
/// picked (`Wire` vs `Sftp`) so the use case can stamp the
/// `SessionStarted` push event with the right transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsyncStartOutcome {
    /// Stable identifier for the session.
    pub rsync_id: RsyncId,
    /// `true` when the planner picked the wire-compat tier.
    pub wire_transport: bool,
}

/// Rsync transport port. Implementations are async (network I/O).
#[trait_variant::make(RsyncTransportPort: Send)]
pub trait LocalRsyncTransportPort: Sync {
    /// Open a new sync session and return the minted [`RsyncId`] plus
    /// the transport tier the planner picked.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::RsyncNotFound`,
    /// `DomainError::RsyncVersionTooOld`,
    /// `DomainError::RsyncProtocolError`, or
    /// `DomainError::SftpFeatureMissing` depending on the failure
    /// point.
    async fn start_session(
        &self,
        request: RsyncStartRequest,
    ) -> Result<RsyncStartOutcome, DomainError>;

    /// Pull the next progress event off the session. Returns `Ok(None)`
    /// when the session reached a terminal state and no more events
    /// will arrive.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::RsyncProtocolError` /
    /// `DomainError::RsyncPartialTransfer` for transport-level faults.
    async fn recv_event(
        &self,
        rsync_id: &RsyncId,
    ) -> Result<Option<RsyncProgressEvent>, DomainError>;

    /// Close the session and release all resources. Idempotent: a
    /// double close on the same id is a no-op.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::RsyncProtocolError` if the close handshake
    /// fails; unknown ids are collapsed into `Ok(())` to keep the
    /// close path idempotent.
    async fn close(&self, rsync_id: &RsyncId) -> Result<(), DomainError>;
}

#[cfg(test)]
mod tests {
    use super::RsyncTransportPort;

    fn _assert_port<T: RsyncTransportPort>() {}
}
